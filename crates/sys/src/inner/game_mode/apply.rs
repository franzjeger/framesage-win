//! Execute and revert a `PlannedAction`.
//!
//! The engine calls these — one for apply, one for revert — and is otherwise
//! agnostic about how each action lands. Each branch is intentionally small
//! and uses the per-feature module under `crates/sys/src/inner/game_mode/`.

use anyhow::Result;
use tracing::{debug, warn};

use framesage_gamemode::planner::PlannedAction;
use framesage_gamemode::state::{
    AppliedActions, PreviousState, ServiceStatus, SuspendedProcessSnapshot,
};

use super::{power_plan, process, service, taskbar};

/// Execute one planned action and reflect the result into `applied`.
///
/// Returns `Ok(())` only on successful application or documented no-ops (e.g.
/// service already stopped). Returns `Err` on any state-modifying failure so
/// the engine can surface it; the caller is responsible for deciding whether
/// to continue with the rest of the plan.
pub fn apply_action(action: &PlannedAction, applied: &mut AppliedActions) -> Result<()> {
    match action {
        PlannedAction::HideTaskbar => {
            let changed = taskbar::hide_taskbar()?;
            applied.hid_taskbar = applied.hid_taskbar || changed;
            debug!(changed, "apply: hide_taskbar");
            Ok(())
        }
        PlannedAction::SetPowerPlan { to, .. } => {
            power_plan::set_active_plan(to)?;
            applied.switched_power_plan = true;
            debug!(plan = %to.guid(), "apply: power plan switched");
            Ok(())
        }
        PlannedAction::StopService { id, .. } => {
            let changed = service::stop_service(id)?;
            if changed {
                applied.stopped_services.push(id.clone());
            }
            debug!(service = %id, changed, "apply: stop_service");
            Ok(())
        }
        PlannedAction::SuspendProcess { pid, exe } => {
            let count = process::suspend_process(*pid)?;
            applied.suspended_pids.push(SuspendedProcessSnapshot {
                pid: *pid,
                exe: exe.clone(),
            });
            debug!(pid = pid, exe = %exe, suspended_threads = count, "apply: suspend_process");
            Ok(())
        }
        PlannedAction::SetFocusAssist(mode) => {
            // v0.1 stub. Recorded in `applied` so revert knows to no-op too.
            warn!(?mode, "Focus Assist control not yet implemented (v0.3)");
            applied.set_focus_assist = true;
            Ok(())
        }
        PlannedAction::PauseWindowsUpdate => {
            warn!("Windows Update pause not yet implemented (v0.3)");
            applied.paused_windows_update = true;
            Ok(())
        }
    }
}

/// Revert everything in `applied`, consulting `previous` to know what state
/// to restore. Best-effort: failures are logged and skipped, never propagated,
/// because revert MUST drain — the alternative is stranding the user.
pub fn revert_all(applied: &AppliedActions, previous: &PreviousState) {
    // Reverse the apply order so dependents are restored last.

    if applied.paused_windows_update {
        debug!("revert: pause_windows_update (stub)");
    }
    if applied.set_focus_assist {
        debug!("revert: focus_assist (stub)");
    }

    for snap in &applied.suspended_pids {
        match process::resume_process(snap.pid) {
            Ok(count) => {
                debug!(pid = snap.pid, exe = %snap.exe, resumed_threads = count, "revert: resume_process")
            }
            Err(e) => {
                warn!(pid = snap.pid, exe = %snap.exe, error = %e, "revert: resume_process failed")
            }
        }
    }

    for id in &applied.stopped_services {
        // Only start services that were running before we stopped them.
        let was_running = previous
            .services
            .iter()
            .find(|s| s.id.eq_ignore_ascii_case(id))
            .map(|s| s.status.was_running())
            .unwrap_or(true); // if we have no record, default to "yes, start it"
        if !was_running {
            debug!(service = %id, "revert: service was not running before; leaving stopped");
            continue;
        }
        match service::start_service(id) {
            Ok(changed) => debug!(service = %id, changed, "revert: start_service"),
            Err(e) => warn!(service = %id, error = %e, "revert: start_service failed"),
        }
    }

    if applied.switched_power_plan {
        if let Some(prev) = &previous.active_power_plan {
            match power_plan::set_active_plan(prev) {
                Ok(()) => debug!(plan = %prev.guid(), "revert: power plan restored"),
                Err(e) => warn!(error = %e, "revert: power plan restore failed"),
            }
        } else {
            warn!("revert: switched power plan but have no previous plan recorded");
        }
    }

    if applied.hid_taskbar {
        match taskbar::show_taskbar() {
            Ok(changed) => debug!(changed, "revert: show_taskbar"),
            Err(e) => warn!(error = %e, "revert: show_taskbar failed"),
        }
    }

    let _ = ServiceStatus::Stopped; // silence unused-import on prerelease builds
}
