//! #111 Group B — service-side PresentMon manager (architecture §2.2).
//!
//! Bridges a live Game Mode session to a real `PresentMon.exe` child so
//! `frame_sample` events actually flow into the recorder, completing the
//! closed-loop "Did FrameSage help?" attribution chain. Before this, every
//! session recorded `presentmon_state: "disabled"` and the attribution panel
//! could only ever say "Frame data unavailable".
//!
//! Split per `docs/syscall-seam-pattern.md`:
//!
//! * [`desired_target`] — the host-independent decision: given the policy
//!   gate and the engine's foreground/game-mode state, which PID (if any)
//!   should PresentMon be attached to right now. Fully unit-tested off
//!   Windows.
//! * [`spawn`] — the `cfg(windows)` driver task. Polls the engine, consults
//!   [`desired_target`] + the PRE-L-004 [`SpawnPolicy`], drives the real
//!   child on a blocking thread, and forwards each 1 Hz [`FrameStats`] bucket
//!   into the recorder's frame channel.
//!
//! Like the recorder and the closed-loop tasks, this manager is NOT part of
//! the v0.6 watchdog `select!`: a PresentMon failure must never take the rule
//! engine down. On any error we log, tear the child down, and fall back to
//! honest frame-data-unavailable recording.

use framesage_ipc::ForegroundSnapshot;
use framesage_presentmon::FrameStats;

/// The process PresentMon should currently be attached to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredTarget {
    pub pid: u32,
    pub exe_name: String,
}

/// Host-independent attach decision. PresentMon is attached only when
/// **all** of these hold, mirroring exactly when the recorder is writing
/// a session and can use frame data:
///
/// * the policy has closed-loop recording enabled (strictly opt-in — the
///   §2.4 "Once enabled, just play" contract),
/// * a Game Mode session is active (so there's a session to attribute), and
/// * there's a real foreground process to measure (`pid != 0`).
///
/// Any other state returns `None`, which the driver treats as "detach any
/// running child" — we never keep a PresentMon.exe alive outside a session,
/// keeping the process-creation footprint low (the PRE-L-004 concern).
pub fn desired_target(
    closed_loop_enabled: bool,
    game_mode_active: bool,
    foreground: Option<&ForegroundSnapshot>,
) -> Option<DesiredTarget> {
    if !closed_loop_enabled || !game_mode_active {
        return None;
    }
    let fg = foreground?;
    if fg.pid == 0 || fg.exe_name.is_empty() {
        return None;
    }
    Some(DesiredTarget {
        pid: fg.pid,
        exe_name: fg.exe_name.clone(),
    })
}

/// Spawn the PresentMon manager task. Returns whether a PresentMon.exe is
/// even available on disk — the service stamps that into the recorder's
/// [`crate::session_recorder::SessionCapabilities::presentmon_active`] so
/// sessions tell the truth about whether frame capture was possible.
///
/// `frame_tx` is the recorder's frame-sample intake (mirrors the
/// kernel_signal broadcast pattern, but mpsc: there's exactly one consumer).
#[cfg(windows)]
pub fn spawn(
    engine: std::sync::Arc<framesage_engine::Engine>,
    frame_tx: tokio::sync::mpsc::Sender<FrameStats>,
) -> (bool, tokio::task::JoinHandle<()>) {
    let exe = framesage_core::paths::presentmon_exe_path();
    let available = exe.as_ref().is_some_and(|p| p.exists());
    let handle = tokio::spawn(windows_impl::run(engine, frame_tx, exe));
    (available, handle)
}

/// Off-Windows: no child process, no frame source. Reports unavailable and
/// parks a task that does nothing, so `runtime.rs` stays uniform across
/// hosts. Console mode on a dev box simply records honest
/// `presentmon_state: "disabled"`.
#[cfg(not(windows))]
pub fn spawn(
    _engine: std::sync::Arc<framesage_engine::Engine>,
    _frame_tx: tokio::sync::mpsc::Sender<FrameStats>,
) -> (bool, tokio::task::JoinHandle<()>) {
    let handle = tokio::spawn(async {});
    (false, handle)
}

#[cfg(windows)]
mod windows_impl {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use framesage_engine::Engine;
    use framesage_presentmon::{FrameStats, SpawnDecision, SpawnPolicy};
    use tokio::sync::mpsc;
    use tracing::{info, warn};

    use super::desired_target;

    /// How often we reconcile PresentMon against the engine's foreground /
    /// game-mode state. 2 s is well under the 30 s PRE-L-004 spawn floor,
    /// so the reconcile cadence never itself drives spawn churn; the
    /// SpawnPolicy is the real rate limiter.
    const RECONCILE_INTERVAL: Duration = Duration::from_secs(2);

    /// A currently-attached child: the killable process handle, the
    /// blocking drain thread's join handle, and the exe name it's measuring
    /// (so a foreground change to a *different* game triggers a re-attach,
    /// but the same game is left alone).
    struct Attached {
        target_exe: String,
        child: framesage_presentmon::child::PresentMonChild,
        join: tokio::task::JoinHandle<()>,
    }

    pub async fn run(
        engine: Arc<Engine>,
        frame_tx: mpsc::Sender<FrameStats>,
        exe: Option<PathBuf>,
    ) {
        let Some(exe) = exe else {
            info!("PresentMon.exe path could not be resolved; frame capture disabled");
            return;
        };
        if !exe.exists() {
            info!(
                path = %exe.display(),
                "PresentMon.exe not found next to the service binary; frame capture disabled"
            );
            return;
        }

        let mut policy = SpawnPolicy::new();
        let mut attached: Option<Attached> = None;
        let mut interval = tokio::time::interval(RECONCILE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            // Reap a child whose drain thread finished on its own — the
            // game exited (`--terminate_on_proc_exit` closed stdout, ending
            // the drain). Wait() reports whether it exited cleanly so the
            // restart budget stays accurate.
            if attached.as_ref().is_some_and(|a| a.join.is_finished()) {
                let a = attached.take().unwrap();
                let _ = a.join.await;
                let clean = a.child.wait().unwrap_or(false);
                policy.note_exited(!clean);
                info!(exe = %a.target_exe, clean, "PresentMon child exited on its own");
            }

            let status = engine.status();
            let game_mode_active = status.active_profile.is_some();
            let desired = desired_target(
                engine.closed_loop_enabled(),
                game_mode_active,
                status.foreground.as_ref(),
            );

            match (desired, attached.as_ref()) {
                // Nothing wanted, nothing running — idle.
                (None, None) => {}
                // Session ended (or game closed): detach the child.
                (None, Some(_)) => {
                    detach(attached.take().unwrap(), &mut policy).await;
                }
                // Want a target and something's already attached.
                (Some(want), Some(a)) => {
                    if !want.exe_name.eq_ignore_ascii_case(&a.target_exe) {
                        // Foreground switched to a different game — swap.
                        detach(attached.take().unwrap(), &mut policy).await;
                        try_attach(&exe, &want, &mut policy, &mut attached, &frame_tx);
                    }
                    // Same game already measured — leave it (Reuse).
                }
                // Want a target, nothing attached — try to bring one up.
                (Some(want), None) => {
                    try_attach(&exe, &want, &mut policy, &mut attached, &frame_tx);
                }
            }
        }
    }

    /// Consult the spawn policy and, if allowed, launch a PresentMon child
    /// draining into `frame_tx`. Non-`Spawn` decisions (rate-limited, budget
    /// exhausted) are honest no-ops: we simply record no frames this window.
    fn try_attach(
        exe: &std::path::Path,
        want: &super::DesiredTarget,
        policy: &mut SpawnPolicy,
        attached: &mut Option<Attached>,
        frame_tx: &mpsc::Sender<FrameStats>,
    ) {
        match policy.decide(&want.exe_name, Instant::now()) {
            SpawnDecision::Spawn => {}
            SpawnDecision::Reuse => return,
            SpawnDecision::RateLimited { wait } => {
                warn!(
                    exe = %want.exe_name,
                    wait_secs = wait.as_secs(),
                    "PresentMon spawn rate-limited (PRE-L-004); no frames this window"
                );
                return;
            }
            SpawnDecision::RestartBudgetExhausted => {
                warn!(
                    exe = %want.exe_name,
                    "PresentMon restart budget exhausted this session; \
                     recording frame-data-unavailable"
                );
                return;
            }
        }

        let mut child =
            match framesage_presentmon::child::PresentMonChild::spawn(exe, want.pid) {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, pid = want.pid, "PresentMon spawn failed");
                    // A failed spawn counts as a crash against the budget so
                    // a persistently-failing PresentMon eventually gives up.
                    policy.note_spawned(&want.exe_name, Instant::now());
                    policy.note_exited(true);
                    return;
                }
            };
        let Some(pipe) = child.take_frame_pipe() else {
            warn!(pid = want.pid, "PresentMon child has no stdout pipe; skipping");
            child.stop();
            return;
        };

        // The drain loop is blocking (BufRead over the child stdout pipe),
        // so it owns a dedicated blocking thread. It forwards each 1 Hz
        // bucket via `blocking_send`; the loop ends when the child closes
        // stdout — either the game exits or `detach` kills the process.
        let frame_tx = frame_tx.clone();
        let join = tokio::task::spawn_blocking(move || {
            if let Err(e) = pipe.drain(|stats| {
                // A closed recorder channel is not fatal here — keep draining
                // (and reaping) the child; frames are simply dropped.
                let _ = frame_tx.blocking_send(stats);
            }) {
                warn!(error = %e, "PresentMon drain ended with error");
            }
        });

        policy.note_spawned(&want.exe_name, Instant::now());
        info!(
            exe = %want.exe_name,
            pid = want.pid,
            "PresentMon attached; frame_sample capture active"
        );
        *attached = Some(Attached {
            target_exe: want.exe_name.clone(),
            child,
            join,
        });
    }

    /// Terminate the child now and wait for its drain thread to finish.
    /// Killing the process closes stdout, which unblocks the blocking drain;
    /// we then join it. An orderly detach (session end / foreground game
    /// switch) is never a crash, so it resets the restart budget.
    async fn detach(a: Attached, policy: &mut SpawnPolicy) {
        let Attached {
            target_exe,
            child,
            join,
        } = a;
        // Kill on a blocking thread — kill()+wait() can block briefly.
        let _ = tokio::task::spawn_blocking(move || child.stop()).await;
        let _ = join.await;
        policy.note_exited(false);
        info!(exe = %target_exe, "PresentMon detached");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fg(pid: u32, exe: &str) -> ForegroundSnapshot {
        ForegroundSnapshot {
            pid,
            exe_name: exe.into(),
            path: String::new(),
            title: String::new(),
        }
    }

    #[test]
    fn no_target_when_closed_loop_disabled() {
        let f = fg(42, "game.exe");
        assert_eq!(desired_target(false, true, Some(&f)), None);
    }

    #[test]
    fn no_target_without_an_active_game_mode_session() {
        let f = fg(42, "game.exe");
        assert_eq!(desired_target(true, false, Some(&f)), None);
    }

    #[test]
    fn no_target_without_a_foreground_process() {
        assert_eq!(desired_target(true, true, None), None);
    }

    #[test]
    fn no_target_for_a_zero_pid_or_nameless_foreground() {
        assert_eq!(desired_target(true, true, Some(&fg(0, "game.exe"))), None);
        assert_eq!(desired_target(true, true, Some(&fg(42, ""))), None);
    }

    #[test]
    fn attaches_to_the_foreground_game_when_everything_lines_up() {
        let f = fg(1234, "Cyberpunk2077.exe");
        assert_eq!(
            desired_target(true, true, Some(&f)),
            Some(DesiredTarget {
                pid: 1234,
                exe_name: "Cyberpunk2077.exe".into(),
            })
        );
    }
}
