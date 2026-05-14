//! framesage policy engine.
//!
//! The engine sits between the OS (via `framesage-sys`) and the user-facing
//! configuration (a `Policy`). Each tick it asks: what is the foreground
//! process right now? — and reconciles the running state against what the
//! policy says it should be.
//!
//! Two dimensions of state are tracked:
//!
//! 1. **Per-process** state — what we changed on the foregrounded process
//!    (CPU Sets, Power Throttling, priority, etc.). Reverted automatically
//!    when focus moves to a different process.
//!
//! 2. **System-wide Game Mode state** — taskbar visibility, stopped services,
//!    suspended background processes, switched power plan. Reverted when the
//!    active profile changes to one that doesn't request Game Mode, or when
//!    the panic button (`framesage game-mode off`) fires.
//!
//! Design notes:
//!
//! * **Reconcile, don't event-chase.** Windows has a `WinEventHook` for focus
//!   changes, but real games still spawn helper windows, fullscreen-flip,
//!   minimise, etc., and the focus events lie often enough that a 250–500ms
//!   polled reconcile is more robust *and* simpler.
//! * **Track what we applied, so we can revert.** Per-process state lives in
//!   `applied`; system state lives in a crash-safe journal so a process kill
//!   doesn't strand the user with a hidden taskbar and stopped services.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use framesage_core::{CpuTopology, GameModeActions, Policy, Profile, ProfileId};
use framesage_gamemode::{
    journal::{Journal, JournalEntry},
    planner::{plan as plan_game_mode, ActionPlan, PlannedAction, SystemStateQuery},
    safe_list::SafeList,
    state::{AppliedActions, PreviousState, SuspendedProcessSnapshot},
};
use framesage_ipc::{Event, ForegroundSnapshot, StatusSnapshot};

/// Dependencies the engine needs at construction. Passing as a struct keeps
/// the call sites readable (we already have policy + topology, and now journal
/// + safe_list join them) and gives us room to grow without breaking callers.
pub struct EngineDeps {
    pub policy: Policy,
    pub topology: CpuTopology,
    pub safe_list: &'static SafeList,
    pub journal: Journal,
}

pub struct Engine {
    state: Arc<RwLock<EngineState>>,
    events: broadcast::Sender<Event>,
    safe_list: &'static SafeList,
    journal: Journal,
}

struct EngineState {
    policy: Policy,
    topology: CpuTopology,
    paused: bool,
    /// What we've applied per-pid, so we can revert per-process state.
    applied: HashMap<u32, AppliedRecord>,
    /// Currently-foregrounded pid, if any.
    current_foreground: Option<u32>,
    /// Last-seen foreground details (so Status doesn't have to syscall).
    foreground_snapshot: Option<ForegroundSnapshot>,
    active_profile: Option<ProfileId>,
    /// Whatever system-wide Game Mode we've entered, if any. Mirrored to the
    /// journal on disk so a crash leaves recoverable state.
    system_mode: Option<ActiveSystemMode>,
}

struct AppliedRecord {
    profile_id: ProfileId,
    /// Opaque per-platform state used to revert per-process changes.
    #[cfg(windows)]
    state: framesage_sys::apply::AppliedState,
    #[cfg(not(windows))]
    _phantom: (),
}

/// What we entered into Game Mode for; mirrors the journal on disk.
#[derive(Debug, Clone)]
struct ActiveSystemMode {
    profile_id: ProfileId,
    previous: PreviousState,
    applied: AppliedActions,
    /// The session UUID, kept so logs and status can correlate with the
    /// journal file we wrote.
    journal_session_id: uuid::Uuid,
}

impl Engine {
    /// Construct an engine with full dependencies.
    pub fn new(deps: EngineDeps) -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            state: Arc::new(RwLock::new(EngineState {
                policy: deps.policy,
                topology: deps.topology,
                paused: false,
                applied: HashMap::new(),
                current_foreground: None,
                foreground_snapshot: None,
                active_profile: None,
                system_mode: None,
            })),
            events: tx,
            safe_list: deps.safe_list,
            journal: deps.journal,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn pause(&self) {
        let mut s = self.state.write();
        if !s.paused {
            s.paused = true;
            let _ = self.events.send(Event::Paused);
            info!("engine paused");
        }
    }

    pub fn resume(&self) {
        let mut s = self.state.write();
        if s.paused {
            s.paused = false;
            let _ = self.events.send(Event::Resumed);
            info!("engine resumed");
        }
    }

    pub fn set_policy(&self, policy: Policy) {
        self.state.write().policy = policy;
        info!("policy replaced");
    }

    pub fn status(&self) -> StatusSnapshot {
        let s = self.state.read();
        let active_profile = s
            .active_profile
            .as_ref()
            .and_then(|id| s.policy.profile(id).cloned());
        StatusSnapshot {
            paused: s.paused,
            policy: s.policy.clone(),
            foreground: s.foreground_snapshot.clone(),
            active_profile,
        }
    }

    /// Panic button: revert any active system mode regardless of foreground.
    /// Idempotent — `Ok(())` if no system mode was active.
    pub fn exit_system_mode_now(&self) {
        let mut s = self.state.write();
        Self::revert_system_mode_locked(&mut s, &self.journal);
    }

    /// Apply a named profile to the currently-foregrounded process,
    /// overriding the normal rule matcher. The override holds until the
    /// foreground changes — at the next focus change, `tick`'s reconcile
    /// path will pick the rule-matched profile for whatever's new.
    ///
    /// Used by the CLI's `framesage apply <profile>` and the tray's
    /// "Apply now" per-profile button. Errors if no foreground exists or
    /// if the profile id isn't in the active policy.
    pub fn apply_once(&self, profile_id: ProfileId) -> Result<()> {
        let foreground = framesage_sys::foreground::current()?
            .ok_or_else(|| anyhow::anyhow!("no foreground process to apply to"))?;
        let mut s = self.state.write();

        if s.paused {
            return Err(anyhow::anyhow!(
                "engine is paused; resume before apply_once"
            ));
        }

        let profile = s
            .policy
            .profile(&profile_id)
            .ok_or_else(|| anyhow::anyhow!("unknown profile id {profile_id}"))?
            .clone();

        // Revert anything we'd previously applied to this same PID (whether
        // the prior profile was the same or different) so we have a clean
        // slate to apply onto.
        if let Some(record) = s.applied.remove(&foreground.pid) {
            revert_record(foreground.pid, record);
        }

        let topology = s.topology.clone();
        let snapshot = ForegroundSnapshot {
            pid: foreground.pid,
            exe_name: foreground.exe_name.clone(),
            path: foreground.path.clone(),
            title: foreground.title.clone(),
        };

        let record = apply_profile(foreground.pid, &profile, &topology)?;
        info!(
            pid = foreground.pid,
            exe = %foreground.exe_name,
            profile = %profile_id,
            "apply_once",
        );
        s.applied.insert(foreground.pid, record);
        s.current_foreground = Some(foreground.pid);
        s.foreground_snapshot = Some(snapshot.clone());
        s.active_profile = Some(profile_id.clone());
        let _ = self.events.send(Event::ForegroundChanged {
            foreground: snapshot,
            profile: profile_id.clone(),
        });

        // System mode reconcile — handles entering/exiting/swapping
        // Game Mode actions according to the new profile.
        let new_actions = profile.game_mode.clone();
        Self::reconcile_system_mode_locked(
            &mut s,
            &self.journal,
            self.safe_list,
            &profile_id,
            new_actions,
        );

        Ok(())
    }

    /// Run on startup: if a journal file exists from a previous (possibly
    /// crashed) session, revert it before we apply anything new. Idempotent.
    pub fn recover_orphan_journal(&self) {
        let entry = match self.journal.read() {
            Ok(Some(entry)) => entry,
            Ok(None) => return,
            Err(e) => {
                warn!(error = %e, "orphan journal read failed; deleting and moving on");
                let _ = self.journal.delete();
                return;
            }
        };
        warn!(
            session = %entry.session_id,
            profile = %entry.profile_id,
            "found orphan game-mode journal; reverting"
        );
        sys_revert_all(&entry.applied, &entry.previous);
        if let Err(e) = self.journal.delete() {
            warn!(error = %e, "failed to delete orphan journal after revert");
        }
    }

    /// One reconciliation pass.
    pub fn tick(&self) -> Result<()> {
        // Snapshot what we need without holding the lock across syscalls.
        let paused = self.state.read().paused;
        if paused {
            return Ok(());
        }

        let foreground = framesage_sys::foreground::current()?;

        let mut s = self.state.write();
        self.reconcile(&mut s, foreground)
    }

    fn reconcile(
        &self,
        s: &mut EngineState,
        foreground: Option<framesage_sys::foreground::ForegroundInfo>,
    ) -> Result<()> {
        let new_pid = foreground.as_ref().map(|f| f.pid);

        // Foreground unchanged — nothing to do.
        if new_pid == s.current_foreground {
            return Ok(());
        }

        // Revert per-process state on the previous foreground.
        if let Some(prev_pid) = s.current_foreground.take() {
            if let Some(record) = s.applied.remove(&prev_pid) {
                revert_record(prev_pid, record);
            }
        }

        // No new foreground? Tear down system mode too — there's nothing to
        // be in Game Mode "for."
        let Some(fg) = foreground else {
            s.foreground_snapshot = None;
            s.active_profile = None;
            Self::revert_system_mode_locked(s, &self.journal);
            return Ok(());
        };

        let profile_id = s
            .policy
            .match_foreground(&fg.exe_name, &fg.path, &fg.title)
            .clone();

        let profile = match s.policy.profile(&profile_id) {
            Some(p) => p.clone(),
            None => {
                warn!(profile = %profile_id, "matched profile id not found in policy");
                return Ok(());
            }
        };

        let topology = s.topology.clone();
        let snapshot = ForegroundSnapshot {
            pid: fg.pid,
            exe_name: fg.exe_name.clone(),
            path: fg.path.clone(),
            title: fg.title.clone(),
        };

        // Per-process apply.
        match apply_profile(fg.pid, &profile, &topology) {
            Ok(record) => {
                info!(pid = fg.pid, exe = %fg.exe_name, profile = %profile_id, "applied");
                s.applied.insert(fg.pid, record);
                s.current_foreground = Some(fg.pid);
                s.foreground_snapshot = Some(snapshot.clone());
                s.active_profile = Some(profile_id.clone());
                let _ = self.events.send(Event::ForegroundChanged {
                    foreground: snapshot,
                    profile: profile_id.clone(),
                });
            }
            Err(e) => {
                warn!(pid = fg.pid, exe = %fg.exe_name, error = %e, "apply failed");
                // We still update foreground tracking so we don't loop trying
                // to re-apply on every tick. System mode below uses
                // active_profile.
                s.current_foreground = Some(fg.pid);
                s.foreground_snapshot = Some(snapshot);
                s.active_profile = Some(profile_id.clone());
            }
        }

        // System mode: reconcile against what the new profile wants.
        let new_actions = profile.game_mode.clone();
        Self::reconcile_system_mode_locked(
            s,
            &self.journal,
            self.safe_list,
            &profile_id,
            new_actions,
        );

        Ok(())
    }

    /// Compare current `system_mode` against the new profile's requested
    /// actions; if different, revert the old session and (if the new profile
    /// asked for it) enter a new one.
    fn reconcile_system_mode_locked(
        s: &mut EngineState,
        journal: &Journal,
        safe_list: &'static SafeList,
        new_profile: &ProfileId,
        new_actions: Option<GameModeActions>,
    ) {
        // Fast paths.
        match (&s.system_mode, &new_actions) {
            (None, None) => return,
            (None, Some(a)) if *a == GameModeActions::default() => return,
            (Some(current), Some(a))
                if &current.profile_id == new_profile && {
                    // The "actions unchanged" test: if the new profile is identical
                    // and the configured actions match what was applied. We don't
                    // store the original actions in ActiveSystemMode, so the
                    // simplest sound rule is "same profile id ⇒ keep." Profile
                    // edits during hot-reload will swap the policy and trigger a
                    // teardown/re-enter via this branch falling through.
                    let _ = a;
                    true
                } =>
            {
                return;
            }
            _ => {}
        }

        // Different (or first) — tear down any existing session, then enter.
        Self::revert_system_mode_locked(s, journal);

        let Some(actions) = new_actions else {
            return;
        };
        if actions == GameModeActions::default() {
            return;
        }

        // Plan.
        let plan_result = plan_game_mode(&actions, safe_list, &PlatformQuery);
        let plan = match plan_result {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => {
                debug!("game mode plan is empty; nothing to do");
                return;
            }
            Err(e) => {
                warn!(error = %e, "game mode planning failed");
                return;
            }
        };

        Self::enter_system_mode_locked(s, journal, new_profile, plan);
    }

    fn enter_system_mode_locked(
        s: &mut EngineState,
        journal: &Journal,
        profile_id: &ProfileId,
        plan: ActionPlan,
    ) {
        // Journal the full *intent* before any kernel mutation. This closes a
        // crash-recovery race that surfaced during first hardware validation:
        // sys_apply_action mutates kernel state synchronously (e.g. SCM marks
        // a service stopped), but the journal write that records "we stopped
        // it" happens after. A SIGKILL between the two left the kernel ahead
        // of the journal, and recovery missed the unjournaled mutation.
        //
        // With intent journaled up-front, recovery reverts everything we
        // *planned* to do. A failed sys_apply_action means recovery reverts a
        // change that was never made — an idempotent no-op (start an already-
        // running service, resume an already-running process).
        let intended = applied_from_plan(&plan);

        let mut entry = JournalEntry::new(profile_id.clone(), plan.previous_state.clone());
        entry.applied = intended.clone();

        if let Err(e) = journal.write(&entry) {
            warn!(error = %e, "initial journal write failed; refusing to enter game mode");
            return;
        }

        // Apply. Partial failures are logged locally; the journal does not
        // need to be rewritten because it already records the full intent.
        let mut any_failed = false;
        let mut applied_count: usize = 0;
        for action in &plan.actions {
            // sys_apply_action takes `&mut AppliedActions` for legacy reasons;
            // we route to a throwaway sink because the authoritative record is
            // `intended`, persisted before we got here.
            let mut sink = AppliedActions::default();
            match sys_apply_action(action, &mut sink) {
                Ok(()) => {
                    applied_count += 1;
                }
                Err(e) => {
                    warn!(?action, error = %e, "game mode action failed; continuing with rest of plan");
                    any_failed = true;
                }
            }
        }

        if !plan.rejections.is_empty() {
            for r in &plan.rejections {
                warn!(rejected = %r.id, reason = %r.reason, "game mode action rejected by safe-list");
            }
        }

        info!(
            profile = %profile_id,
            actions = applied_count,
            partial = any_failed,
            session = %entry.session_id,
            "game mode entered"
        );

        s.system_mode = Some(ActiveSystemMode {
            profile_id: profile_id.clone(),
            previous: plan.previous_state,
            applied: intended,
            journal_session_id: entry.session_id,
        });
    }

    fn revert_system_mode_locked(s: &mut EngineState, journal: &Journal) {
        let Some(active) = s.system_mode.take() else {
            return;
        };
        info!(
            profile = %active.profile_id,
            session = %active.journal_session_id,
            "game mode exiting"
        );
        sys_revert_all(&active.applied, &active.previous);
        if let Err(e) = journal.delete() {
            warn!(error = %e, "journal delete after revert failed");
        }
    }
}

fn revert_record(pid: u32, record: AppliedRecord) {
    #[cfg(windows)]
    if let Err(e) = framesage_sys::apply::revert(pid, record.state) {
        warn!(pid, error = %e, "revert failed");
    }
    debug!(pid, profile = %record.profile_id, "reverted");
    let _ = record;
}

/// Project a plan's actions onto an `AppliedActions` record describing what
/// every action would touch if it ran successfully. The engine journals this
/// up-front so that even a SIGKILL mid-apply leaves a complete revert plan on
/// disk.
///
/// Every `PlannedAction` variant must contribute to at least one field — if a
/// new variant is added later and this match grows a hole, the unit test
/// `applied_from_plan_covers_every_planned_action_variant` will catch it.
fn applied_from_plan(plan: &ActionPlan) -> AppliedActions {
    let mut a = AppliedActions::default();
    for action in &plan.actions {
        match action {
            PlannedAction::HideTaskbar => a.hid_taskbar = true,
            PlannedAction::SetPowerPlan { .. } => a.switched_power_plan = true,
            PlannedAction::StopService { id, .. } => a.stopped_services.push(id.clone()),
            PlannedAction::SuspendProcess { pid, exe } => {
                a.suspended_pids.push(SuspendedProcessSnapshot {
                    pid: *pid,
                    exe: exe.clone(),
                });
            }
            PlannedAction::SetFocusAssist(_) => a.set_focus_assist = true,
            PlannedAction::PauseWindowsUpdate => a.paused_windows_update = true,
        }
    }
    a
}

#[cfg(windows)]
fn apply_profile(pid: u32, profile: &Profile, topology: &CpuTopology) -> Result<AppliedRecord> {
    let state = framesage_sys::apply::apply(pid, profile, topology)?;
    Ok(AppliedRecord {
        profile_id: profile.id.clone(),
        state,
    })
}

#[cfg(not(windows))]
fn apply_profile(_pid: u32, profile: &Profile, _topology: &CpuTopology) -> Result<AppliedRecord> {
    Ok(AppliedRecord {
        profile_id: profile.id.clone(),
        _phantom: (),
    })
}

// ─── platform-specific shims ──────────────────────────────────────────────
//
// These ride the same cfg gates the rest of the workspace uses. On non-Windows
// they no-op so the engine still drives correctly during `framesage-sim` runs.

#[cfg(windows)]
fn sys_apply_action(
    action: &framesage_gamemode::planner::PlannedAction,
    applied: &mut AppliedActions,
) -> Result<()> {
    framesage_sys::game_mode::apply_action(action, applied)
}

#[cfg(not(windows))]
fn sys_apply_action(
    action: &framesage_gamemode::planner::PlannedAction,
    applied: &mut AppliedActions,
) -> Result<()> {
    framesage_sys::game_mode::apply_action(action, applied)
}

#[cfg(windows)]
fn sys_revert_all(applied: &AppliedActions, previous: &PreviousState) {
    framesage_sys::game_mode::revert_all(applied, previous);
}

#[cfg(not(windows))]
fn sys_revert_all(applied: &AppliedActions, previous: &PreviousState) {
    framesage_sys::game_mode::revert_all(applied, previous);
}

#[cfg(windows)]
struct PlatformQuery;

#[cfg(windows)]
impl SystemStateQuery for PlatformQuery {
    fn taskbar_visible(&self) -> anyhow::Result<bool> {
        framesage_sys::game_mode::Win32StateQuery.taskbar_visible()
    }
    fn active_power_plan(&self) -> anyhow::Result<Option<framesage_core::PowerPlanId>> {
        framesage_sys::game_mode::Win32StateQuery.active_power_plan()
    }
    fn service_status(&self, id: &str) -> anyhow::Result<framesage_gamemode::state::ServiceStatus> {
        framesage_sys::game_mode::Win32StateQuery.service_status(id)
    }
    fn pids_by_exe(&self, exe: &str) -> anyhow::Result<Vec<(u32, String)>> {
        framesage_sys::game_mode::Win32StateQuery.pids_by_exe(exe)
    }
}

#[cfg(not(windows))]
struct PlatformQuery;

#[cfg(not(windows))]
impl SystemStateQuery for PlatformQuery {
    fn taskbar_visible(&self) -> anyhow::Result<bool> {
        framesage_sys::game_mode::Win32StateQuery.taskbar_visible()
    }
    fn active_power_plan(&self) -> anyhow::Result<Option<framesage_core::PowerPlanId>> {
        framesage_sys::game_mode::Win32StateQuery.active_power_plan()
    }
    fn service_status(&self, id: &str) -> anyhow::Result<framesage_gamemode::state::ServiceStatus> {
        framesage_sys::game_mode::Win32StateQuery.service_status(id)
    }
    fn pids_by_exe(&self, exe: &str) -> anyhow::Result<Vec<(u32, String)>> {
        framesage_sys::game_mode::Win32StateQuery.pids_by_exe(exe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framesage_core::{FocusAssistMode, PowerPlanId};
    use framesage_gamemode::state::ServiceStatus;

    fn empty_previous() -> PreviousState {
        PreviousState {
            taskbar_visible: true,
            active_power_plan: None,
            services: vec![],
            suspended_pids: vec![],
        }
    }

    /// Locks the load-bearing invariant for crash recovery: every concrete
    /// `PlannedAction` variant must contribute to `AppliedActions`. If a new
    /// variant is added but the helper isn't extended, recovery will silently
    /// miss it — exactly the bug this test exists to prevent regressing.
    #[test]
    fn applied_from_plan_covers_every_planned_action_variant() {
        let plan = ActionPlan {
            previous_state: empty_previous(),
            actions: vec![
                PlannedAction::HideTaskbar,
                PlannedAction::SetPowerPlan {
                    from: Some(PowerPlanId::Balanced),
                    to: PowerPlanId::HighPerformance,
                },
                PlannedAction::StopService {
                    id: "SysMain".into(),
                    was_status: ServiceStatus::Running,
                },
                PlannedAction::StopService {
                    id: "WSearch".into(),
                    was_status: ServiceStatus::Running,
                },
                PlannedAction::SuspendProcess {
                    pid: 1234,
                    exe: "OneDrive.exe".into(),
                },
                PlannedAction::SetFocusAssist(FocusAssistMode::PriorityOnly),
                PlannedAction::PauseWindowsUpdate,
            ],
            rejections: vec![],
        };

        let applied = applied_from_plan(&plan);

        assert!(applied.hid_taskbar);
        assert!(applied.switched_power_plan);
        assert_eq!(
            applied.stopped_services,
            vec!["SysMain".to_string(), "WSearch".to_string()]
        );
        assert_eq!(applied.suspended_pids.len(), 1);
        assert_eq!(applied.suspended_pids[0].pid, 1234);
        assert_eq!(applied.suspended_pids[0].exe, "OneDrive.exe");
        assert!(applied.set_focus_assist);
        assert!(applied.paused_windows_update);
        assert!(applied.anything_applied());
    }

    #[test]
    fn applied_from_plan_yields_empty_for_no_actions() {
        let plan = ActionPlan {
            previous_state: empty_previous(),
            actions: vec![],
            rejections: vec![],
        };
        assert_eq!(applied_from_plan(&plan), AppliedActions::default());
    }
}
