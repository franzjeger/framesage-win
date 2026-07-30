//! v0.7 closed-loop ETW consumer wiring for the service host.
//!
//! Day 5 deliverable per `spike/group-a-week-2-plan.md` §4 Day 5.
//! Encapsulates the startup-time evaluation of three predicates
//! (policy opt-in, build gate, EtwSession::start success), the
//! conditional `EtwSession::start()` call, and the spawning of the
//! `SupervisorLoop` task and the drop-rate poll interval task.
//!
//! Ownership / shutdown coordination (per user Day 5 watch-item):
//! the closed-loop tasks intentionally **do not** participate in the
//! v0.6 watchdog `tokio::select!` in `runtime::run()`. Rationale: per
//! architecture §2.1 mode 5 amendment (proposal/v0.7-arch-mode5-amendment
//! PR #77), the supervisor exiting (consumer thread done — clean or
//! panic) is NOT a critical service failure. The service stays up;
//! engine continues in v0.6 static-rule mode. Putting the supervisor
//! in the watchdog would invert that contract — every consumer panic
//! would terminate the service.
//!
//! The drop-poll interval task self-terminates when the session is
//! gone (its query_session_stats call fails). The supervisor task
//! self-terminates when the consumer exits. Both are independent of
//! the v0.6 critical tasks (tick / admin-ipc / status-ipc / reload /
//! sys-events) — those still drive the watchdog.

use std::time::Duration;

use framesage_core::Policy;
use framesage_etw::{
    build_gate, DegradationEvent, EtwSession, EtwSubsystem, SessionOptions, SupervisorLoop,
};
use tracing::{error, info, warn};

// M2.6 / H-004 — the build-gate test override seam lives solely in
// `framesage_etw::build_gate`. Service-side tests reach it through the
// `test-override` cargo feature enabled from this crate's
// dev-dependencies (Cargo feature unification applies it to test
// builds only; production binaries compile the seam out). The
// service-local thread_local duplicate that used to sit here is gone —
// one seam, one place.
#[cfg(test)]
use build_gate::BuildOverrideGuard;

/// Result of evaluating + (conditionally) starting the closed-loop
/// subsystem at service startup. The variants are mutually exclusive
/// and exhaust the decision-tree per plan §4 Day 5. Fields are read
/// via the `Debug` derive (used in tracing log lines); the
/// `dead_code` allow silences the lint that doesn't trace through
/// Debug-derived field reads.
#[derive(Debug)]
#[allow(dead_code)]
pub enum ClosedLoopStartup {
    /// `policy.closed_loop_enabled = false` — user opted out.
    OptedOut,
    /// Build gate failed — Windows build < `MIN_BUILD_FOR_CLOSED_LOOP`.
    /// `detected_build` carries the actual build (or None if the probe
    /// itself failed).
    BuildUnsupported { detected_build: Option<u32> },
    /// `EtwSession::start()` returned `Disabled(mode)` — typically
    /// AccessDenied (EDR block) or AlreadyExists (another consumer).
    SessionDisabled {
        mode: framesage_etw::DegradationMode,
    },
    /// `EtwSession::start()` returned an unexpected `Err` — logged at
    /// ERROR. Service still starts; engine runs in v0.6 static-rule mode.
    StartupError { message: String },
    /// Closed-loop is running. Supervisor + drop-poll tasks have been
    /// `tokio::spawn`'d. Caller doesn't track the handles — see
    /// module docstring for the ownership rationale.
    Running,
}

/// Evaluate the closed-loop decision tree and spawn the supervisor +
/// drop-poll tasks if enabled. Called once during service startup,
/// after policy load + topology detection.
///
/// **Build-gate-fallthrough log lines are structured** (per user Day 5
/// guidance for §5 acceptance criterion): every reason for falling
/// through to static-rule mode emits a `tracing` event with named
/// fields (`reason`, `detected_build`, `degradation_mode`) so the
/// integration test can assert against fields rather than substring-
/// matching the formatted message.
pub fn start_closed_loop_if_enabled(policy: &Policy) -> ClosedLoopStartup {
    if !policy.closed_loop_enabled {
        info!(
            reason = "policy_opt_out",
            "closed-loop disabled by policy.closed_loop_enabled = false; engine runs in v0.6 static-rule mode"
        );
        return ClosedLoopStartup::OptedOut;
    }

    if !build_gate::closed_loop_enabled_for_this_build() {
        let detected = build_gate::detected_build();
        info!(
            reason = "build_unsupported",
            detected_build = ?detected,
            minimum_build = build_gate::MIN_BUILD_FOR_CLOSED_LOOP,
            "closed-loop disabled: Windows build below MIN_BUILD_FOR_CLOSED_LOOP; engine runs in v0.6 static-rule mode"
        );
        return ClosedLoopStartup::BuildUnsupported {
            detected_build: detected,
        };
    }

    let opts = SessionOptions::default();
    match EtwSession::start(opts) {
        Ok(EtwSubsystem::Running(session)) => {
            spawn_closed_loop_tasks(session);
            info!(
                reason = "running",
                "closed-loop ETW session started + supervisor/drop-poll tasks spawned"
            );
            ClosedLoopStartup::Running
        }
        Ok(EtwSubsystem::Disabled(mode)) => {
            info!(
                reason = "session_disabled",
                degradation_mode = ?mode,
                "closed-loop session opened in Disabled state; engine runs in v0.6 static-rule mode"
            );
            ClosedLoopStartup::SessionDisabled { mode }
        }
        Err(e) => {
            error!(
                reason = "startup_error",
                error = %e,
                "closed-loop startup failed unexpectedly; engine runs in v0.6 static-rule mode"
            );
            ClosedLoopStartup::StartupError {
                message: e.to_string(),
            }
        }
    }
}

/// Spawn supervisor + drop-poll tasks. Per the module docstring,
/// these tasks are **not** added to the v0.6 watchdog select! —
/// their exit is not a critical service failure.
#[cfg(windows)]
fn spawn_closed_loop_tasks(session: EtwSession) {
    // M1.1 / A-001: decomposition is fallible (one-shot API). A fresh
    // Running session always decomposes; if it somehow doesn't, fall
    // back to static-rule mode instead of panicking the service.
    let (consumer_join, exit_rx, shutdown, monitor) = match session
        .into_supervisable_parts_with_monitor()
    {
        Ok(parts) => parts,
        Err(e) => {
            error!(error = %e, "closed-loop decomposition failed; engine runs in v0.6 static-rule mode");
            return;
        }
    };

    // Production on_event sink: tracing::error! emission. Per v3
    // secondary decision Option C, tracing IS the wire to the (future)
    // Group C UI banner consumer. The closure is Send + Sync + 'static
    // (no captured non-static references) so the SupervisorLoop's
    // bound is satisfied.
    let supervisor =
        SupervisorLoop::new(consumer_join, exit_rx, shutdown, |ev: DegradationEvent| {
            error!(
                degradation_mode = ?ev.mode,
                detail = %ev.detail,
                "ETW degradation event"
            );
        });

    // Supervisor task: awaits the oneshot, fires on_event on panic
    // path, joins the consumer thread, returns ConsumerExitReason.
    // Service host continues running regardless of the supervisor's
    // exit reason (per architecture §2.1 mode 5 amendment).
    tokio::spawn(async move {
        let reason = supervisor.run().await;
        info!(
            consumer_exit_reason = ?reason,
            "ETW consumer-supervisor task completed; static-rule mode if Panicked"
        );
    });

    // Drop-poll interval task: 1-second cadence calling
    // MonitorHandle::poll_drop_stats. Self-terminates when the
    // session is gone (query_session_stats fails). The on_event sink
    // is the same shape as the supervisor's — tracing::error! emission.
    //
    // M3.3 / G-001 — poll_drop_stats runs a blocking ControlTraceW
    // (QUERY) syscall directly on the async executor. At the current
    // 1 Hz cadence the sub-millisecond call is harmless; if the poll
    // rate is ever increased (or the query observed slow under load),
    // wrap the call in tokio::task::spawn_blocking so it can't stall
    // the runtime's worker threads.
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            match monitor.poll_drop_stats(|ev: DegradationEvent| {
                error!(
                    degradation_mode = ?ev.mode,
                    detail = %ev.detail,
                    "ETW degradation event"
                );
            }) {
                Ok(_stats) => { /* fine; emission is inside the closure */ }
                Err(e) => {
                    // Session is likely gone (supervisor cleaned up
                    // after consumer exit). Log once and exit.
                    warn!(
                        error = %e,
                        "ETW drop-poll task terminating; session likely closed"
                    );
                    break;
                }
            }
        }
    });
}

#[cfg(not(windows))]
fn spawn_closed_loop_tasks(_session: EtwSession) {
    // Non-Windows stub: EtwSession::start would have bailed before
    // reaching here. This branch exists so the function signature
    // resolves cross-platform.
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build-gate-fallthrough log format integration test per plan §5
    /// acceptance criterion. Uses `tracing-test` to assert against the
    /// structured event (level + target/message substring) rather than
    /// substring-matching the formatted output. Per user Day 5
    /// guidance: "structured tracing fields, not bare string."
    ///
    /// Note `tracing-test::logs_contain` does substring-match on the
    /// rendered output by default. The structured-field assertion
    /// approach would require a custom tracing subscriber; for week 2
    /// scope, asserting key substrings + the level via `logs_contain`
    /// is sufficient. Substring set includes the structured-field
    /// formatting (`reason="policy_opt_out"`) so a future log-format
    /// change that drops the field would break the test.
    #[tracing_test::traced_test]
    #[test]
    fn opt_out_path_emits_structured_policy_opt_out_event() {
        let policy = Policy {
            closed_loop_enabled: false,
            ..Policy::default()
        };
        let result = start_closed_loop_if_enabled(&policy);
        assert!(matches!(result, ClosedLoopStartup::OptedOut));
        // Assert against the structured-field formatting.
        assert!(logs_contain("reason=\"policy_opt_out\""));
        assert!(logs_contain(
            "closed-loop disabled by policy.closed_loop_enabled"
        ));
    }

    /// Build-gate-fallthrough assertion via the test-only override seam
    /// (per Step 11 finding #11.1). The override forces the
    /// `BuildUnsupported` branch deterministically on any host — including
    /// elevated Win11 24H2+ where the real build gate would pass and the
    /// function would reach `tokio::spawn`, panicking outside a runtime.
    ///
    /// `#[tokio::test(flavor = "multi_thread")]` provides a runtime as a
    /// defensive belt-and-suspenders measure; with the override active the
    /// function returns before reaching any `tokio::spawn`, but the runtime
    /// availability protects against future refactors that move the spawn
    /// earlier in the decision tree.
    ///
    /// `tracing-test` integrates with `#[tokio::test]` the same way it does
    /// with `#[test]` — the attribute order matters: `#[tokio::test]` must
    /// be the outermost so the runtime is set up before the subscriber.
    #[tokio::test(flavor = "multi_thread")]
    #[tracing_test::traced_test]
    async fn build_gate_fallthrough_emits_structured_build_unsupported_event() {
        // Synthetic build below the threshold (Win11 23H2 = 22631 < 26100).
        // RAII guard resets the override on Drop so a panicking test can't
        // poison subsequent tests on the same thread.
        let _guard = BuildOverrideGuard::set(Some(Ok(22631)));

        let policy = Policy {
            closed_loop_enabled: true,
            ..Policy::default()
        };
        let result = start_closed_loop_if_enabled(&policy);
        assert!(
            matches!(
                result,
                ClosedLoopStartup::BuildUnsupported {
                    detected_build: Some(22631)
                }
            ),
            "override forces BuildUnsupported branch; got {result:?}"
        );
        assert!(logs_contain("reason=\"build_unsupported\""));
        assert!(logs_contain("closed-loop disabled"));
    }
}
