//! Plan a Game Mode session.
//!
//! Inputs: the `GameModeActions` a profile requested, the curated `SafeList`,
//! and a read-only view of current OS state. Output: a typed, ordered
//! `ActionPlan` that the engine can execute and journal.
//!
//! Planning is pure — no syscalls happen here, only queries through the
//! `SystemStateQuery` trait. That makes the planner trivial to test on any
//! host: pass a fake query, assert on the plan.
//!
//! Apply ordering rationale: cheap and reversible operations go first so a
//! crash early in the apply loop leaves the smallest trail to revert. The
//! current order is taskbar → power plan → services → processes → stubs.

use std::collections::HashSet;

use thiserror::Error;
use tracing::warn;

use framesage_core::{FocusAssistMode, GameModeActions, PowerPlanId};

use crate::safe_list::{Rejection, RejectionKind, SafeList};
use crate::state::{PreviousState, ServiceStateSnapshot, ServiceStatus, SuspendedProcessSnapshot};

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("system query failed for {what}: {source}")]
    Query {
        what: &'static str,
        #[source]
        source: anyhow::Error,
    },
}

/// Read-only window onto OS state. The planner takes a trait object so:
/// - on Windows, `framesage-sys` provides a real impl,
/// - in `framesage-sim`, a synthetic impl drives the planner from JSONL or
///   command-line input,
/// - in unit tests, an in-memory fake.
///
/// Every method returns `anyhow::Result` so a query failure doesn't crash the
/// engine — it surfaces as a `PlanError::Query`, the offending action gets
/// skipped, and the rest of the plan proceeds.
pub trait SystemStateQuery {
    fn taskbar_visible(&self) -> anyhow::Result<bool>;
    fn active_power_plan(&self) -> anyhow::Result<Option<PowerPlanId>>;
    fn service_status(&self, id: &str) -> anyhow::Result<ServiceStatus>;
    fn pids_by_exe(&self, exe: &str) -> anyhow::Result<Vec<(u32, String)>>;
}

/// One concrete reversible operation. The engine knows how to apply each
/// variant and how to revert it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedAction {
    HideTaskbar,
    SetPowerPlan {
        from: Option<PowerPlanId>,
        to: PowerPlanId,
    },
    StopService {
        id: String,
        /// Status at plan time. Engine uses this to skip the stop call when
        /// the service was already stopped, and to know whether revert should
        /// start it again.
        was_status: ServiceStatus,
    },
    SuspendProcess {
        pid: u32,
        exe: String,
    },
    /// Stubbed in v0.1 — recorded so the journal carries intent for v0.3
    /// when the actual Focus Assist toggle lands.
    SetFocusAssist(FocusAssistMode),
    /// Stubbed in v0.1.
    PauseWindowsUpdate,
}

/// Fully-resolved plan: the previous state we'll snapshot to the journal,
/// the operations to perform, and anything the safe-list rejected so the
/// caller can surface it in logs / UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionPlan {
    pub previous_state: PreviousState,
    pub actions: Vec<PlannedAction>,
    pub rejections: Vec<Rejection>,
}

impl ActionPlan {
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }
}

pub fn plan(
    actions: &GameModeActions,
    safe_list: &SafeList,
    query: &dyn SystemStateQuery,
) -> Result<ActionPlan, PlanError> {
    let mut planned: Vec<PlannedAction> = Vec::new();
    let mut rejections: Vec<Rejection> = Vec::new();
    let mut snapshotted_services: Vec<ServiceStateSnapshot> = Vec::new();

    // ─── Taskbar ─────────────────────────────────────────────────────────
    let taskbar_visible = query.taskbar_visible().map_err(|e| PlanError::Query {
        what: "taskbar visibility",
        source: e,
    })?;
    if actions.hide_taskbar && taskbar_visible {
        planned.push(PlannedAction::HideTaskbar);
    }

    // ─── Power plan ──────────────────────────────────────────────────────
    //
    // Policy: if we can't read the currently active plan, we DON'T switch.
    // The previous plan is what revert needs; without it, we'd strand the
    // user on whatever plan we set. Better to skip than to half-apply.
    let active_power_plan_result = query.active_power_plan();
    let active_power_plan = active_power_plan_result
        .as_ref()
        .ok()
        .and_then(|v| v.clone());
    if let Some(target) = &actions.power_plan {
        match &active_power_plan_result {
            Ok(Some(cur)) => {
                let already_on = cur.guid().eq_ignore_ascii_case(target.guid());
                if !already_on {
                    planned.push(PlannedAction::SetPowerPlan {
                        from: Some(cur.clone()),
                        to: target.clone(),
                    });
                }
            }
            Ok(None) => {
                warn!(
                    "active power plan query returned None; skipping power-plan switch (no revert info)"
                );
            }
            Err(e) => {
                warn!(error = %e, "active power plan query failed; skipping power-plan switch");
            }
        }
    }

    // ─── Services ────────────────────────────────────────────────────────
    let (allowed_services, mut svc_rejections) =
        safe_list.partition_services(&actions.stop_services);
    rejections.append(&mut svc_rejections);
    for entry in allowed_services {
        match query.service_status(&entry.id) {
            Ok(status) => {
                snapshotted_services.push(ServiceStateSnapshot {
                    id: entry.id.clone(),
                    status,
                });
                if status.was_running() {
                    planned.push(PlannedAction::StopService {
                        id: entry.id.clone(),
                        was_status: status,
                    });
                }
            }
            Err(e) => {
                warn!(service = %entry.id, error = %e, "service status query failed; skipping");
            }
        }
    }

    // ─── Processes ───────────────────────────────────────────────────────
    let (allowed_processes, mut proc_rejections) =
        safe_list.partition_processes(&actions.suspend_processes);
    rejections.append(&mut proc_rejections);

    // Dedupe PIDs across exe entries so two rules targeting the same process
    // don't generate a double-suspend.
    let mut seen_pids: HashSet<u32> = HashSet::new();
    let mut suspended_snapshots: Vec<SuspendedProcessSnapshot> = Vec::new();
    for entry in allowed_processes {
        let pids = match query.pids_by_exe(&entry.exe) {
            Ok(v) => v,
            Err(e) => {
                warn!(process = %entry.exe, error = %e, "process lookup failed; skipping");
                continue;
            }
        };
        for (pid, exe) in pids {
            if !seen_pids.insert(pid) {
                continue;
            }
            planned.push(PlannedAction::SuspendProcess {
                pid,
                exe: exe.clone(),
            });
            suspended_snapshots.push(SuspendedProcessSnapshot { pid, exe });
        }
    }

    // ─── Focus Assist ────────────────────────────────────────────────────
    //
    // Microsoft hasn't shipped a documented user-mode API to set Focus Assist
    // — the Settings app uses an undocumented COM interface that breaks
    // between Windows builds. Rather than fake success or hand-roll a fragile
    // binding, we surface the request as a visible rejection so the user
    // knows their setting isn't doing anything. The field remains in the
    // serde schema for forward compatibility when a clean API ships.
    if let Some(mode) = actions.focus_assist {
        rejections.push(Rejection {
            id: format!("focus_assist:{mode}"),
            kind: RejectionKind::NotImplemented,
            reason: "Focus Assist control has no documented user-mode API on this Windows version"
                .to_owned(),
        });
    }

    if actions.pause_windows_update {
        planned.push(PlannedAction::PauseWindowsUpdate);
    }

    Ok(ActionPlan {
        previous_state: PreviousState {
            taskbar_visible,
            active_power_plan,
            services: snapshotted_services,
            suspended_pids: suspended_snapshots,
        },
        actions: planned,
        rejections,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// In-memory `SystemStateQuery` so the planner can be unit-tested without
    /// any OS calls. Mutex is for interior mutability of the query log; tests
    /// share immutable state otherwise.
    #[derive(Default)]
    struct FakeQuery {
        taskbar_visible: bool,
        power_plan: Option<PowerPlanId>,
        services: HashMap<String, ServiceStatus>,
        processes: HashMap<String, Vec<(u32, String)>>,
        calls: Mutex<Vec<String>>,
    }

    impl FakeQuery {
        fn new() -> Self {
            Self {
                taskbar_visible: true,
                power_plan: Some(PowerPlanId::Balanced),
                ..Default::default()
            }
        }
        fn with_service(mut self, id: &str, status: ServiceStatus) -> Self {
            self.services.insert(id.to_string(), status);
            self
        }
        fn with_process(mut self, exe: &str, pid: u32) -> Self {
            self.processes
                .entry(exe.to_string())
                .or_default()
                .push((pid, exe.to_string()));
            self
        }
    }

    impl SystemStateQuery for FakeQuery {
        fn taskbar_visible(&self) -> anyhow::Result<bool> {
            self.calls.lock().unwrap().push("taskbar".into());
            Ok(self.taskbar_visible)
        }
        fn active_power_plan(&self) -> anyhow::Result<Option<PowerPlanId>> {
            self.calls.lock().unwrap().push("power_plan".into());
            Ok(self.power_plan.clone())
        }
        fn service_status(&self, id: &str) -> anyhow::Result<ServiceStatus> {
            self.calls.lock().unwrap().push(format!("svc:{id}"));
            self.services
                .get(id)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("unknown service {id} in fake"))
        }
        fn pids_by_exe(&self, exe: &str) -> anyhow::Result<Vec<(u32, String)>> {
            self.calls.lock().unwrap().push(format!("proc:{exe}"));
            Ok(self.processes.get(exe).cloned().unwrap_or_default())
        }
    }

    #[test]
    fn empty_actions_produce_empty_plan() {
        let query = FakeQuery::new();
        let plan = plan(&GameModeActions::default(), SafeList::bundled(), &query).unwrap();
        assert!(plan.is_empty());
        assert!(plan.rejections.is_empty());
    }

    #[test]
    fn hide_taskbar_is_planned_only_when_visible() {
        let actions = GameModeActions {
            hide_taskbar: true,
            ..Default::default()
        };
        let visible = FakeQuery::new();
        let plan_visible = plan(&actions, SafeList::bundled(), &visible).unwrap();
        assert_eq!(plan_visible.actions, vec![PlannedAction::HideTaskbar]);

        let hidden = FakeQuery {
            taskbar_visible: false,
            ..FakeQuery::new()
        };
        let plan_hidden = plan(&actions, SafeList::bundled(), &hidden).unwrap();
        assert!(plan_hidden.actions.is_empty(), "no-op when already hidden");
    }

    #[test]
    fn power_plan_skipped_when_already_on_target() {
        let actions = GameModeActions {
            power_plan: Some(PowerPlanId::Balanced),
            ..Default::default()
        };
        let query = FakeQuery::new(); // power_plan = Balanced
        let plan_out = plan(&actions, SafeList::bundled(), &query).unwrap();
        assert!(plan_out.actions.is_empty());
    }

    #[test]
    fn power_plan_switch_is_planned_when_different() {
        let actions = GameModeActions {
            power_plan: Some(PowerPlanId::UltimatePerformance),
            ..Default::default()
        };
        let query = FakeQuery::new(); // power_plan = Balanced
        let plan_out = plan(&actions, SafeList::bundled(), &query).unwrap();
        assert_eq!(
            plan_out.actions,
            vec![PlannedAction::SetPowerPlan {
                from: Some(PowerPlanId::Balanced),
                to: PowerPlanId::UltimatePerformance,
            }]
        );
    }

    #[test]
    fn allowed_service_planned_only_if_running() {
        let actions = GameModeActions {
            stop_services: vec!["SysMain".into(), "WSearch".into()],
            ..Default::default()
        };
        let query = FakeQuery::new()
            .with_service("SysMain", ServiceStatus::Running)
            .with_service("WSearch", ServiceStatus::Stopped);

        let plan_out = plan(&actions, SafeList::bundled(), &query).unwrap();

        let stops: Vec<&PlannedAction> = plan_out
            .actions
            .iter()
            .filter(|a| matches!(a, PlannedAction::StopService { .. }))
            .collect();
        assert_eq!(
            stops.len(),
            1,
            "only the running service gets a stop action"
        );
        match stops[0] {
            PlannedAction::StopService { id, .. } => assert_eq!(id, "SysMain"),
            _ => unreachable!(),
        }

        // But BOTH services should be snapshotted in previous_state so revert
        // knows what state each had.
        assert_eq!(plan_out.previous_state.services.len(), 2);
    }

    #[test]
    fn denied_services_are_rejected_not_planned() {
        let actions = GameModeActions {
            stop_services: vec!["WinDefend".into(), "vgc".into()],
            ..Default::default()
        };
        let query = FakeQuery::new();
        let plan_out = plan(&actions, SafeList::bundled(), &query).unwrap();

        assert!(
            plan_out
                .actions
                .iter()
                .all(|a| !matches!(a, PlannedAction::StopService { .. })),
            "no service stops planned"
        );
        assert_eq!(plan_out.rejections.len(), 2);
    }

    #[test]
    fn unlisted_services_are_rejected_not_planned() {
        let actions = GameModeActions {
            stop_services: vec!["TotallyMadeUpService".into()],
            ..Default::default()
        };
        let query = FakeQuery::new();
        let plan_out = plan(&actions, SafeList::bundled(), &query).unwrap();
        assert!(plan_out.actions.is_empty());
        assert_eq!(plan_out.rejections.len(), 1);
        assert_eq!(
            plan_out.rejections[0].kind,
            crate::safe_list::RejectionKind::Unlisted
        );
    }

    #[test]
    fn process_suspend_planned_for_each_matching_pid_deduped() {
        let actions = GameModeActions {
            suspend_processes: vec!["OneDrive.exe".into(), "ONEDRIVE.EXE".into()],
            ..Default::default()
        };
        let query = FakeQuery::new()
            .with_process("OneDrive.exe", 1111)
            .with_process("OneDrive.exe", 2222);
        let plan_out = plan(&actions, SafeList::bundled(), &query).unwrap();

        let pids: Vec<u32> = plan_out
            .actions
            .iter()
            .filter_map(|a| match a {
                PlannedAction::SuspendProcess { pid, .. } => Some(*pid),
                _ => None,
            })
            .collect();
        assert_eq!(pids.len(), 2);
        assert!(pids.contains(&1111));
        assert!(pids.contains(&2222));
    }

    #[test]
    fn shell_processes_are_rejected_not_planned() {
        let actions = GameModeActions {
            suspend_processes: vec!["explorer.exe".into(), "csrss.exe".into()],
            ..Default::default()
        };
        let query = FakeQuery::new();
        let plan_out = plan(&actions, SafeList::bundled(), &query).unwrap();
        assert!(
            plan_out.actions.is_empty(),
            "shell processes should never be planned"
        );
        assert_eq!(plan_out.rejections.len(), 2);
    }

    #[test]
    fn focus_assist_is_rejected_as_not_implemented() {
        // Until a clean user-mode Focus Assist API exists, the planner refuses
        // to claim Focus Assist will run — it surfaces a visible rejection
        // instead of silently doing nothing at apply time.
        let actions = GameModeActions {
            focus_assist: Some(FocusAssistMode::PriorityOnly),
            ..Default::default()
        };
        let query = FakeQuery::new();
        let plan_out = plan(&actions, SafeList::bundled(), &query).unwrap();
        assert!(
            !plan_out
                .actions
                .iter()
                .any(|a| matches!(a, PlannedAction::SetFocusAssist(_))),
            "Focus Assist must not appear in the actions list"
        );
        let rejection = plan_out
            .rejections
            .iter()
            .find(|r| r.kind == RejectionKind::NotImplemented)
            .expect("Focus Assist should be rejected with NotImplemented");
        assert!(rejection.id.starts_with("focus_assist:"));
    }

    #[test]
    fn pause_windows_update_is_planned() {
        let actions = GameModeActions {
            pause_windows_update: true,
            ..Default::default()
        };
        let query = FakeQuery::new();
        let plan_out = plan(&actions, SafeList::bundled(), &query).unwrap();
        assert!(plan_out
            .actions
            .contains(&PlannedAction::PauseWindowsUpdate));
    }

    #[test]
    fn power_plan_query_failure_doesnt_panic_just_skips() {
        struct FailingPlanQuery(FakeQuery);
        impl SystemStateQuery for FailingPlanQuery {
            fn taskbar_visible(&self) -> anyhow::Result<bool> {
                self.0.taskbar_visible()
            }
            fn active_power_plan(&self) -> anyhow::Result<Option<PowerPlanId>> {
                Err(anyhow::anyhow!("PowerGetActiveScheme broke"))
            }
            fn service_status(&self, id: &str) -> anyhow::Result<ServiceStatus> {
                self.0.service_status(id)
            }
            fn pids_by_exe(&self, exe: &str) -> anyhow::Result<Vec<(u32, String)>> {
                self.0.pids_by_exe(exe)
            }
        }

        let actions = GameModeActions {
            power_plan: Some(PowerPlanId::HighPerformance),
            hide_taskbar: true,
            ..Default::default()
        };
        let query = FailingPlanQuery(FakeQuery::new());
        let plan_out = plan(&actions, SafeList::bundled(), &query).unwrap();
        // Taskbar should still be planned even though power-plan query failed.
        assert!(plan_out.actions.contains(&PlannedAction::HideTaskbar));
        // And the power-plan action is dropped, not crashed.
        assert!(plan_out
            .actions
            .iter()
            .all(|a| !matches!(a, PlannedAction::SetPowerPlan { .. })));
    }
}
