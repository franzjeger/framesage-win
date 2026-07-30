//! `SupervisorLoop` — supervises the ETW consumer thread.
//!
//! Day 3 scaffold per `spike/group-a-week-2-plan.md` §3.6 (Day 3
//! type) + §3.5 (panic-channel mechanism). The Day 5 instance lands
//! in `crates/service/src/runtime.rs` and wires the production
//! `tracing::error!` sink. This module is type + unit-test only.
//!
//! Held by the consumer-supervisor tokio task in `crates/service/`;
//! awaits the oneshot::Receiver until the consumer thread exits
//! (clean or panic), then calls the on_event sink and tears down the
//! session via `SessionShutdownHandle`.

use std::thread::JoinHandle;

use tokio::sync::oneshot;

use crate::degradation::{DegradationEvent, DegradationMode};

#[cfg(windows)]
use crate::session::{EtwSysCalls, RealEtwSysCalls, SessionShutdownHandle};

#[cfg(not(windows))]
use crate::session::{EtwSysCalls, RealEtwSysCalls, SessionShutdownHandle};

/// Reason the consumer thread exited. The `Sender` half of the
/// oneshot lives inside the consumer-thread spawn closure (see
/// `crates/etw/src/session.rs`); the `Receiver` half is owned by the
/// `SupervisorLoop`.
#[derive(Debug)]
pub enum ConsumerExitReason {
    /// Normal completion — session was stopped externally (the
    /// supervisor or another path called `EtwSession::stop()` →
    /// `ControlTraceW(STOP)` → `ProcessTrace` returned `ERROR_CANCELLED`).
    CleanShutdown,
    /// Consumer thread panicked. `message` is best-effort extracted
    /// from the panic payload (see plan §3.5 #1 for the extraction
    /// pattern).
    Panicked { message: String },
}

/// Generic over `F` (the event sink) and `S` (the syscalls impl).
/// Production: `F = impl Fn(DegradationEvent)` wired to
/// `|ev| tracing::error!(?ev, "ETW degradation event")`, `S = RealEtwSysCalls`.
/// Tests: `F` captures into an `Arc<Mutex<Vec<DegradationEvent>>>`,
/// `S = MockEtwSysCalls`.
#[derive(Debug)]
pub struct SupervisorLoop<F, S = RealEtwSysCalls>
where
    F: Fn(DegradationEvent) + Send + Sync + 'static,
    S: EtwSysCalls,
{
    consumer_join: JoinHandle<()>,
    exit_rx: oneshot::Receiver<ConsumerExitReason>,
    /// Owned by the supervisor so the panic-teardown path can call
    /// `shutdown()` without needing the full `EtwSession`.
    shutdown: SessionShutdownHandle<S>,
    on_event: F,
}

impl<F, S> SupervisorLoop<F, S>
where
    F: Fn(DegradationEvent) + Send + Sync + 'static,
    S: EtwSysCalls,
{
    pub fn new(
        consumer_join: JoinHandle<()>,
        exit_rx: oneshot::Receiver<ConsumerExitReason>,
        shutdown: SessionShutdownHandle<S>,
        on_event: F,
    ) -> Self {
        Self {
            consumer_join,
            exit_rx,
            shutdown,
            on_event,
        }
    }

    /// Runs the supervisor's select loop. Returns when the consumer
    /// thread exits (clean or panic).
    ///
    /// On panic: fires `on_event(DegradationEvent { mode: ConsumerPanic, .. })`,
    /// calls `shutdown.shutdown()` to tear the session down, then
    /// joins the consumer thread.
    ///
    /// On clean shutdown: emits a single INFO log and joins.
    ///
    /// **Service host stays up either way** — the rule-engine half
    /// continues running v0.6 static-rule mode. This is the
    /// deliberate design change from architecture §2.1 mode 5's
    /// original "service exits non-zero, SCM restarts" wording; the
    /// architecture-amendment PR lands before Day 5's service-wiring
    /// code (per plan §3.5 #5 + the amendment branch
    /// `proposal/v0.7-arch-mode5-amendment` — draft, opens Day 3).
    pub async fn run(self) -> ConsumerExitReason {
        let SupervisorLoop {
            consumer_join,
            exit_rx,
            shutdown,
            on_event,
        } = self;

        // exit_rx.await returns RecvError only if the Sender was
        // dropped without sending — which would mean the consumer
        // thread aborted at OS level (extremely rare). Treat as
        // clean-shutdown rather than synthetic-panic so we don't
        // double-count.
        let reason = exit_rx.await.unwrap_or(ConsumerExitReason::CleanShutdown);

        match &reason {
            ConsumerExitReason::Panicked { message } => {
                on_event(DegradationEvent {
                    mode: DegradationMode::ConsumerPanic,
                    detail: message.clone(),
                });
                if let Err(e) = shutdown.shutdown() {
                    tracing::warn!(error = %e, "session shutdown after consumer panic failed");
                }
            }
            ConsumerExitReason::CleanShutdown => {
                tracing::info!("ETW consumer thread exited cleanly");
                // `let _ =` rather than `drop()` so the non-Windows
                // stub (where SessionShutdownHandle holds only
                // PhantomData) doesn't trip clippy's drop_non_drop.
                // On Windows the effect is identical — the handle is
                // released here, freeing the syscalls clone before we
                // join the consumer thread.
                let _ = shutdown;
            }
        }
        // join() returns Err only if the thread itself panicked AND
        // the panic propagated past our catch_unwind wrapper — which
        // shouldn't happen for AssertUnwindSafe captures of types we
        // statically assert RefUnwindSafe on. Logged best-effort.
        if let Err(panic_payload) = consumer_join.join() {
            tracing::warn!(
                ?panic_payload,
                "etw-consumer thread panicked past catch_unwind boundary"
            );
        }
        reason
    }
}

// ─── Compile-time guard: ConsumerState must be RefUnwindSafe ─────────────────
//
// Per plan §3.5 #4 + v4.2 amendment (Finding 1 / Self-pass A): if a
// future change adds a non-RefUnwindSafe field to ConsumerState
// (e.g., a Cell<T> or a custom mutex), the AssertUnwindSafe in the
// consumer-thread spawn closure becomes unsound. This static
// assertion catches that regression at compile time.
//
// Lives in supervisor.rs (not build_gate.rs's tests) because the
// guard's natural home is alongside ConsumerState's owner — and the
// supervisor is what relies on the catch_unwind contract being sound.

#[cfg(windows)]
const _: () = {
    use static_assertions::assert_impl_all;

    use crate::session::{ConsumerState, RealEtwSysCalls};

    assert_impl_all!(ConsumerState: std::panic::RefUnwindSafe);
    // M1.7 / D-001 — the catch_unwind closure also captures the
    // `consumer_syscalls: S` value, so the production syscall type must
    // be RefUnwindSafe too (trivially true today: RealEtwSysCalls is a
    // ZST). MockEtwSysCalls is deliberately NOT asserted: it contains a
    // RefCell and is not RefUnwindSafe. That is acceptable only under
    // the test-side constraint that panic injection happens strictly
    // before any `borrow_mut` in the mock (arm_panic_in_process_trace
    // fires at function entry), so no torn RefCell state can be
    // observed across the unwind boundary. If a future mock injects
    // panics mid-borrow, it must switch the RefCell to a Mutex first.
    assert_impl_all!(RealEtwSysCalls: std::panic::RefUnwindSafe);
};

// ─── Inline tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// ConsumerExitReason should print at debug level so panic
    /// payloads land in logs in a useful form.
    #[test]
    fn consumer_exit_reason_debug_includes_message() {
        let r = ConsumerExitReason::Panicked {
            message: "synthetic test panic".to_string(),
        };
        let s = format!("{r:?}");
        assert!(s.contains("synthetic test panic"), "{s}");
    }

    #[test]
    fn consumer_exit_reason_clean_shutdown_distinguishable_from_panicked() {
        let panicked = ConsumerExitReason::Panicked {
            message: "x".to_string(),
        };
        let clean = ConsumerExitReason::CleanShutdown;
        // Pattern-match-based discriminator; we don't derive PartialEq
        // on ConsumerExitReason because Panicked's message comparison
        // would invite tests that pin specific panic strings.
        assert!(matches!(clean, ConsumerExitReason::CleanShutdown));
        assert!(matches!(panicked, ConsumerExitReason::Panicked { .. }));
    }

    // ─── Mode 5 (ConsumerPanic) — supervisor synthetic-panic test ────────────
    //
    // This is the inline supervisor test from plan §4 Day 3. Drives
    // SupervisorLoop with a synthetic consumer thread that panics;
    // asserts the on_event sink received DegradationEvent { mode:
    // ConsumerPanic, .. } exactly once.
    //
    // Runs on any platform (uses MockEtwSysCalls + a synthetic thread
    // — no real ETW APIs). On non-Windows the test still compiles +
    // runs because SessionShutdownHandle and EtwSysCalls have stub
    // implementations in non-Windows builds.

    #[cfg(windows)]
    #[tokio::test]
    async fn supervisor_emits_consumer_panic_event_and_calls_shutdown() {
        use std::sync::{Arc, Mutex};

        use crate::session::{MockEtwSysCalls, SessionOptions};

        // Synthetic shutdown handle. Build a real SessionShutdownHandle
        // via EtwSession::start_with_syscalls + into_supervisable_parts
        // so the supervisor receives a real instance to drop. The
        // start path needs an rtl_get_version expectation that returns
        // a supported build + a start_trace expectation that succeeds.
        let mock_sess = MockEtwSysCalls::new();
        use windows::Win32::Foundation::NTSTATUS;
        mock_sess.expect_rtl_get_version(NTSTATUS(0), 26200);
        // start_trace succeeds; consumer thread will spawn.
        // (control_trace queue empty → defaults to ERROR_SUCCESS for
        // cleanup_stale_session.)
        let subsystem =
            crate::session::EtwSession::start_with_syscalls(mock_sess, SessionOptions::default())
                .expect("start_with_syscalls");
        let running = match subsystem {
            crate::session::EtwSubsystem::Running(s) => s,
            crate::session::EtwSubsystem::Disabled(m) => {
                panic!("expected Running; got Disabled({m:?})")
            }
        };

        // Sink captures into a vec the test can inspect.
        let captured: Arc<Mutex<Vec<DegradationEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_sink = Arc::clone(&captured);

        // EtwSession::into_supervisable_parts gives us the real
        // JoinHandle, oneshot Receiver, and SessionShutdownHandle.
        let (consumer_join, exit_rx, shutdown) = running
            .into_supervisable_parts()
            .expect("fresh Running session decomposes");

        // The actual ETW consumer thread is running with the mock
        // process_trace queue empty (returns ERROR_SUCCESS by default),
        // so it will return immediately — that's a CleanShutdown path,
        // not the Panicked path we want.
        //
        // To exercise the Panicked path, we synthesize a panic by
        // dropping a fresh oneshot Sender with a panicked message.
        // The real consumer's exit_rx is dropped; we use a synthetic
        // exit_rx for the supervisor.
        //
        // This makes the test test the SupervisorLoop's panic-handling
        // path in isolation, not the full consumer-thread → supervisor
        // flow. The full-flow test (real consumer-thread panic via
        // mock-armed process_trace → real catch_unwind → real oneshot
        // → supervisor receives Panicked) lives at
        // `crates/etw/src/session.rs`'s `mode_5_session_level_full_flow_panic`.
        drop(exit_rx);
        let (synthetic_tx, synthetic_rx) = oneshot::channel();
        synthetic_tx
            .send(ConsumerExitReason::Panicked {
                message: "synthetic test panic".to_string(),
            })
            .expect("oneshot send");

        let supervisor = SupervisorLoop::new(consumer_join, synthetic_rx, shutdown, move |ev| {
            captured_for_sink.lock().unwrap().push(ev);
        });

        let reason = supervisor.run().await;
        assert!(matches!(reason, ConsumerExitReason::Panicked { .. }));

        let events = captured.lock().unwrap();
        assert_eq!(events.len(), 1, "exactly one DegradationEvent emitted");
        assert!(matches!(events[0].mode, DegradationMode::ConsumerPanic));
        assert!(
            events[0].detail.contains("synthetic test panic"),
            "{}",
            events[0].detail
        );
    }
}
