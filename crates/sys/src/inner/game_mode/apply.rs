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

use super::{power_plan, process, service, taskbar, windows_update};

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
            // Planner now rejects Focus Assist requests with `NotImplemented`,
            // so this arm should be unreachable for plans produced by the
            // current planner. We keep the arm (and the variant) for forward
            // compatibility — when a clean user-mode API ships, the planner
            // will start emitting the action again and only this branch needs
            // to learn the new call.
            warn!(
                ?mode,
                "received SetFocusAssist action; planner should have rejected it as NotImplemented"
            );
            applied.set_focus_assist = true;
            Ok(())
        }
        PlannedAction::PauseWindowsUpdate => {
            windows_update::pause(windows_update::DEFAULT_PAUSE)?;
            applied.paused_windows_update = true;
            debug!(
                pause_secs = windows_update::DEFAULT_PAUSE.as_secs(),
                "apply: pause Windows Update"
            );
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
        match windows_update::resume() {
            Ok(()) => debug!("revert: Windows Update resumed"),
            Err(e) => warn!(error = %e, "revert: Windows Update resume failed"),
        }
    }
    if applied.set_focus_assist {
        debug!("revert: focus_assist (stub)");
    }

    for snap in &applied.suspended_pids {
        // Item 4.10 / audit M-16. Before resuming, verify the live exe
        // at this PID still matches what we suspended. The journal
        // outlives the suspended process: after a crash + reboot, the
        // PID number we recorded may now belong to an entirely
        // unrelated process (Windows reuses PIDs). Resuming the wrong
        // process via NtResumeProcess is a silent kernel mutation —
        // every `ResumeThread` increments the suspend counter towards
        // zero, so we'd be flipping random background processes into a
        // "now twice-not-suspended" state if they happened to be
        // self-suspended.
        match resume_check_for_pid(snap.pid, &snap.exe) {
            ResumeCheck::Proceed => {
                match process::resume_process(snap.pid) {
                    Ok(count) => {
                        debug!(pid = snap.pid, exe = %snap.exe, resumed_threads = count, "revert: resume_process")
                    }
                    Err(e) => {
                        warn!(pid = snap.pid, exe = %snap.exe, error = %e, "revert: resume_process failed")
                    }
                }
            }
            ResumeCheck::SkipExited => {
                debug!(
                    pid = snap.pid,
                    exe = %snap.exe,
                    "revert: suspended PID is gone; nothing to resume"
                );
            }
            ResumeCheck::SkipMismatch { live_exe } => {
                warn!(
                    pid = snap.pid,
                    journaled_exe = %snap.exe,
                    live_exe = %live_exe,
                    "revert: PID reassigned to different exe since suspend; skipping resume to avoid touching the wrong process"
                );
            }
            ResumeCheck::SkipUnverified { reason } => {
                warn!(
                    pid = snap.pid,
                    exe = %snap.exe,
                    reason = %reason,
                    "revert: could not verify exe for suspended PID; skipping resume rather than risk wrong-process resume"
                );
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

/// Item 4.10 — decision returned by [`resume_check_for_pid`]. Separates
/// the "is it safe to resume this PID?" predicate from the actual resume
/// syscall so the predicate can be unit-tested without standing up real
/// suspended processes.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResumeCheck {
    /// Live exe matches the journaled exe (case-insensitive). Safe to
    /// resume.
    Proceed,
    /// `exe_for_pid` returned `Ok(None)` — the PID has exited (and
    /// possibly been reused, but not reused yet). Nothing to do.
    SkipExited,
    /// Live exe differs from journaled exe — PID has been reassigned to
    /// a different process. Resuming would touch the wrong process.
    SkipMismatch { live_exe: String },
    /// `exe_for_pid` itself errored. Best-effort skip: we'd rather strand
    /// a suspended PID (rare; user can reboot) than blindly resume an
    /// unverified PID.
    SkipUnverified { reason: String },
}

/// Item 4.10 — return whether `pid` is still the same process we
/// journaled as `journaled_exe`, by querying its live image path.
///
/// Case-insensitive bare-filename comparison: we suspended by full path
/// but journaled the basename (matches the engine's storage shape, which
/// keeps exe names without paths to ride out user reinstalls / portable
/// app moves).
pub(crate) fn resume_check_for_pid(pid: u32, journaled_exe: &str) -> ResumeCheck {
    match super::super::process::exe_for_pid(pid) {
        Ok(Some(live_path)) => {
            let live_exe = live_path
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or(&live_path);
            if live_exe.eq_ignore_ascii_case(journaled_exe) {
                ResumeCheck::Proceed
            } else {
                ResumeCheck::SkipMismatch {
                    live_exe: live_exe.to_owned(),
                }
            }
        }
        Ok(None) => ResumeCheck::SkipExited,
        Err(e) => ResumeCheck::SkipUnverified {
            reason: format!("{e:#}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The current test process's own PID + its actual exe name should
    /// always match — the canonical "safe to proceed" case. Establishes
    /// that the predicate at least connects to `exe_for_pid` on this
    /// host.
    #[test]
    fn resume_check_proceeds_when_live_exe_matches_journaled() {
        let pid = std::process::id();
        let live_path = super::super::super::process::exe_for_pid(pid)
            .expect("exe_for_pid on self")
            .expect("self has an exe");
        let live_exe = live_path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&live_path)
            .to_owned();
        assert_eq!(
            resume_check_for_pid(pid, &live_exe),
            ResumeCheck::Proceed
        );
    }

    /// Item 4.10 load-bearing case: journal says "OneDrive.exe" but the
    /// live PID is the test runner. Predicate must refuse the resume.
    /// Without this check, a journal pointing at a stale PID that got
    /// reassigned to the test runner would have flipped its suspend
    /// counter under us.
    #[test]
    fn resume_check_skips_when_live_exe_does_not_match() {
        let pid = std::process::id();
        let result = resume_check_for_pid(pid, "OneDrive.exe");
        match result {
            ResumeCheck::SkipMismatch { live_exe } => {
                assert!(
                    !live_exe.eq_ignore_ascii_case("OneDrive.exe"),
                    "live exe should be the test runner, not OneDrive.exe"
                );
            }
            other => panic!(
                "expected SkipMismatch for test-runner PID with fake journaled exe, got {other:?}"
            ),
        }
    }

    /// A PID that's almost certainly not alive (very large 32-bit
    /// value). `exe_for_pid` returns `Ok(None)` for non-existent PIDs
    /// (some kernel paths return errors for "definitely-dead" — we
    /// accept either as a skip without resume).
    #[test]
    fn resume_check_skips_when_pid_is_gone() {
        // A PID well above what Windows hands out in practice. If by
        // some accident this PID is live on the test host, the
        // assertion still passes for any non-Proceed variant.
        let likely_dead_pid = 0x7FFF_FFFE;
        let result = resume_check_for_pid(likely_dead_pid, "OneDrive.exe");
        assert!(
            matches!(
                result,
                ResumeCheck::SkipExited | ResumeCheck::SkipUnverified { .. }
            ),
            "expected SkipExited or SkipUnverified for a definitely-dead PID, got {result:?}"
        );
    }

    /// Case-insensitivity is load-bearing: the journal stores
    /// `OneDrive.exe` (canonical case from the planner's seed) but the
    /// live exe may report as `onedrive.exe` on some filesystems /
    /// PATH-resolution paths. Predicate must treat the two as a match.
    #[test]
    fn resume_check_is_case_insensitive() {
        let pid = std::process::id();
        let live_path = super::super::super::process::exe_for_pid(pid)
            .expect("exe_for_pid on self")
            .expect("self has an exe");
        let live_exe = live_path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(&live_path);
        // Build an upper-case variant of the live exe — predicate must
        // still return Proceed.
        let mangled = live_exe.to_ascii_uppercase();
        assert_eq!(
            resume_check_for_pid(pid, &mangled),
            ResumeCheck::Proceed,
            "case-mangled journaled exe should still match live exe"
        );
    }
}
