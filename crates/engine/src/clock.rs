//! Item 3.1 — abstraction trait for clock reads.
//!
//! The engine reads `Instant::now()` and `SystemTime::now()` in roughly
//! a dozen places — every cadence gate, every "compare elapsed against
//! threshold" check, every journal-entry timestamp. Those reads are
//! the single biggest barrier to deterministic engine tests: a test
//! that wants to verify "the AC-detection probe fires every 5 seconds,
//! not every tick" has no way to advance time without sleeping for
//! real, and a sleep-based test is both slow and flaky.
//!
//! This trait lets the engine call `self.clock.now()` instead of
//! `Instant::now()`, and tests pass a `FakeClock` that advances on
//! command. Production code uses `SystemClock`, which is a zero-cost
//! wrapper around the std functions.
//!
//! # Why both `Instant` and `SystemTime`?
//!
//! - `Instant::now()` is monotonic; ideal for "has X duration elapsed
//!   since Y" comparisons. The engine uses this for cadence (AC
//!   probe, background scan, persistent reassert).
//! - `SystemTime::now()` is wall-clock; ideal for human-meaningful
//!   timestamps (sessions.jsonl entries, activity events). The engine
//!   uses this for journal records and game-mode duration_secs.
//!
//! Distinct methods so tests can move them independently — useful for
//! e.g. "what happens if the wall clock jumps backwards but the
//! monotonic clock keeps advancing" (NTP correction during a game
//! mode session).

use std::time::{Instant, SystemTime};

/// Erased clock. `Send + Sync` so an `Arc<dyn Clock>` can be cheaply
/// cloned across the engine's tick / IPC / reload tasks.
pub trait Clock: Send + Sync {
    /// Current monotonic instant. Used for elapsed-since-last
    /// comparisons in cadence-gated paths.
    fn now(&self) -> Instant;

    /// Current wall-clock time, for timestamps the user (or an audit
    /// log) will read.
    fn unix_now(&self) -> SystemTime;
}

/// Production implementation — straight `Instant::now()` /
/// `SystemTime::now()`. Zero-sized struct; the `Arc<dyn Clock>` indirection
/// in the engine adds one vtable lookup per call which is negligible
/// (the std reads themselves are 50–200 ns).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn unix_now(&self) -> SystemTime {
        SystemTime::now()
    }
}
