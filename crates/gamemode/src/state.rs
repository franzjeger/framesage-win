//! Snapshots of "what was the OS doing before we touched it" and "what did we
//! actually change." Both are journaled to disk *before* any state-modifying
//! call, so a crash mid-session leaves enough breadcrumbs for the next
//! service start to revert cleanly.

use serde::{Deserialize, Serialize};

use framesage_core::PowerPlanId;

/// State captured before any action runs. The planner constructs this from
/// the result of `framesage-sys` query calls (or from synthetic input during
/// `framesage-sim` runs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousState {
    /// Was the primary taskbar visible? Multi-monitor secondary taskbars are
    /// rolled into the same boolean — we assume they tracked the primary.
    pub taskbar_visible: bool,

    /// Active power plan when the session started. May be `None` if the
    /// query failed (e.g. transient PPM error) — in that case revert won't
    /// try to restore.
    pub active_power_plan: Option<PowerPlanId>,

    /// Per-service status snapshot, only for the services in the action plan.
    /// Each entry says "this service was Running / Stopped / etc. before we
    /// touched it." If `Running`, we know to start it again on revert.
    pub services: Vec<ServiceStateSnapshot>,

    /// PIDs that matched a process-suspend rule at apply time. On revert we
    /// pass these through to `framesage-sys::game_mode::process::resume_pid`.
    /// `Vec` instead of `HashMap` so the journal is ordered (= deterministic
    /// for diffing logs).
    pub suspended_pids: Vec<SuspendedProcessSnapshot>,
}

/// Status of one Windows service at the moment we looked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStateSnapshot {
    /// Service short name, e.g. `SysMain`. Case-sensitive in the snapshot —
    /// we record what SCM gave us back, even though lookups elsewhere are
    /// case-insensitive.
    pub id: String,
    pub status: ServiceStatus,
}

/// Subset of Windows' `SERVICE_STATUS` that we care about for revert.
/// Maps 1:1 to documented `SERVICE_STATUS::dwCurrentState` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Stopped,
    StartPending,
    StopPending,
    Running,
    ContinuePending,
    PausePending,
    Paused,
    /// Anything else SCM reports — we keep the raw value so logs can show it.
    Other(u32),
}

impl ServiceStatus {
    /// Did the service appear to be doing useful work? Used by the planner
    /// to decide whether to bother stopping it (no point if already stopped)
    /// and by revert to decide whether to start it (only if it was Running).
    pub fn was_running(self) -> bool {
        matches!(
            self,
            ServiceStatus::Running | ServiceStatus::StartPending | ServiceStatus::ContinuePending
        )
    }
}

/// What we suspended — PID + exe so logs are readable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspendedProcessSnapshot {
    pub pid: u32,
    pub exe: String,
}

/// What we actually changed. Updated incrementally as the apply loop runs, so
/// even partial-success leaves us with an accurate revert plan.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedActions {
    pub hid_taskbar: bool,
    pub stopped_services: Vec<String>,
    pub suspended_pids: Vec<SuspendedProcessSnapshot>,
    pub switched_power_plan: bool,
    pub set_focus_assist: bool,
    pub paused_windows_update: bool,
}

impl AppliedActions {
    /// Did we change anything? Used to skip writing an empty journal entry
    /// for a profile whose actions all got rejected or matched current state.
    pub fn anything_applied(&self) -> bool {
        self.hid_taskbar
            || !self.stopped_services.is_empty()
            || !self.suspended_pids.is_empty()
            || self.switched_power_plan
            || self.set_focus_assist
            || self.paused_windows_update
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn was_running_matches_active_states_only() {
        assert!(ServiceStatus::Running.was_running());
        assert!(ServiceStatus::StartPending.was_running());
        assert!(ServiceStatus::ContinuePending.was_running());
        assert!(!ServiceStatus::Stopped.was_running());
        assert!(!ServiceStatus::Paused.was_running());
        assert!(!ServiceStatus::StopPending.was_running());
        assert!(!ServiceStatus::Other(42).was_running());
    }

    #[test]
    fn applied_actions_default_is_empty() {
        assert!(!AppliedActions::default().anything_applied());
    }

    #[test]
    fn applied_actions_round_trips_through_json() {
        let original = AppliedActions {
            hid_taskbar: true,
            stopped_services: vec!["SysMain".into(), "WSearch".into()],
            suspended_pids: vec![
                SuspendedProcessSnapshot {
                    pid: 1234,
                    exe: "OneDrive.exe".into(),
                },
                SuspendedProcessSnapshot {
                    pid: 5678,
                    exe: "Dropbox.exe".into(),
                },
            ],
            switched_power_plan: true,
            set_focus_assist: false,
            paused_windows_update: false,
        };
        let json = serde_json::to_string_pretty(&original).expect("serialize");
        let parsed: AppliedActions = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, original);
    }

    #[test]
    fn previous_state_round_trips_through_json() {
        let original = PreviousState {
            taskbar_visible: true,
            active_power_plan: Some(PowerPlanId::Balanced),
            services: vec![ServiceStateSnapshot {
                id: "SysMain".into(),
                status: ServiceStatus::Running,
            }],
            suspended_pids: vec![],
        };
        let json = serde_json::to_string(&original).expect("serialize");
        let parsed: PreviousState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, original);
    }
}
