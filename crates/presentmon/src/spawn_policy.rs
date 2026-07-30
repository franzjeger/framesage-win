//! PresentMon subprocess spawn policy — PRE-L-004 / architecture §2.2.
//!
//! The EDR-heuristic concern: 30 gaming sessions a day = 30
//! PresentMon spawn/kill pairs a day, and cumulative process-creation
//! telemetry can trip EDR heuristics. Mitigation, verbatim from the
//! finding's fix: **rate-limit to at most 1 spawn / 30 s, and reuse a
//! currently-running PresentMon when the target process name
//! matches.**
//!
//! Pure state machine — time is injected, so every branch is testable
//! on any host. The Windows child driver consults `decide` before
//! every spawn; there is deliberately no other path to a spawn.

use std::time::{Duration, Instant};

/// Minimum spacing between PresentMon process creations (PRE-L-004).
pub const MIN_SPAWN_INTERVAL: Duration = Duration::from_secs(30);

/// Give up restarting a crashing child after this many restarts within
/// one session — a PresentMon that dies this often is fighting
/// something (EDR, driver); keep the telemetry footprint low and fall
/// back to frame-data-unavailable honesty instead.
pub const MAX_RESTARTS_PER_SESSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnDecision {
    /// A child targeting the same process name is already running —
    /// keep using it (the reuse half of PRE-L-004).
    Reuse,
    /// Spawn a new child now.
    Spawn,
    /// Inside the 30 s window — retry after `wait`.
    RateLimited { wait: Duration },
    /// Restart budget exhausted for this session; do not spawn again.
    RestartBudgetExhausted,
}

/// Tracks spawn history across a service lifetime. One instance per
/// child driver.
#[derive(Debug, Default)]
pub struct SpawnPolicy {
    last_spawn: Option<Instant>,
    running_target: Option<String>,
    restarts_this_session: u32,
}

impl SpawnPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Should we spawn a PresentMon child for `target_exe` at `now`?
    pub fn decide(&self, target_exe: &str, now: Instant) -> SpawnDecision {
        if self
            .running_target
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case(target_exe))
        {
            return SpawnDecision::Reuse;
        }
        if self.restarts_this_session >= MAX_RESTARTS_PER_SESSION {
            return SpawnDecision::RestartBudgetExhausted;
        }
        if let Some(last) = self.last_spawn {
            let since = now.duration_since(last);
            if since < MIN_SPAWN_INTERVAL {
                return SpawnDecision::RateLimited {
                    wait: MIN_SPAWN_INTERVAL - since,
                };
            }
        }
        SpawnDecision::Spawn
    }

    /// Record a spawn that actually happened.
    pub fn note_spawned(&mut self, target_exe: &str, now: Instant) {
        self.last_spawn = Some(now);
        self.running_target = Some(target_exe.to_string());
    }

    /// Record the child exiting. `crashed` distinguishes an unexpected
    /// death (counts against the restart budget) from an orderly stop
    /// at session end (which also resets the budget for the next
    /// session).
    pub fn note_exited(&mut self, crashed: bool) {
        self.running_target = None;
        if crashed {
            self.restarts_this_session += 1;
        } else {
            self.restarts_this_session = 0;
        }
    }

    pub fn restarts_this_session(&self) -> u32 {
        self.restarts_this_session
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_spawn_is_allowed_then_rate_limited() {
        let mut p = SpawnPolicy::new();
        let t0 = Instant::now();
        assert_eq!(p.decide("game.exe", t0), SpawnDecision::Spawn);
        p.note_spawned("game.exe", t0);
        p.note_exited(true);

        // 10 s later: still inside the 30 s window.
        let t1 = t0 + Duration::from_secs(10);
        match p.decide("game.exe", t1) {
            SpawnDecision::RateLimited { wait } => assert_eq!(wait, Duration::from_secs(20)),
            other => panic!("expected RateLimited; got {other:?}"),
        }
        // 30 s later: allowed again.
        assert_eq!(
            p.decide("game.exe", t0 + MIN_SPAWN_INTERVAL),
            SpawnDecision::Spawn
        );
    }

    #[test]
    fn running_child_with_same_target_is_reused_case_insensitively() {
        let mut p = SpawnPolicy::new();
        let t0 = Instant::now();
        p.note_spawned("Game.exe", t0);
        // Reuse wins even inside the rate-limit window — no new
        // process creation happens at all (PRE-L-004's preferred
        // outcome).
        assert_eq!(p.decide("game.exe", t0), SpawnDecision::Reuse);
        // A different target is NOT reused; it's a real spawn request
        // subject to the rate limit.
        assert!(matches!(
            p.decide("other.exe", t0),
            SpawnDecision::RateLimited { .. }
        ));
    }

    #[test]
    fn crash_restart_budget_exhausts_then_resets_on_clean_exit() {
        let mut p = SpawnPolicy::new();
        let mut now = Instant::now();
        for _ in 0..MAX_RESTARTS_PER_SESSION {
            assert_eq!(p.decide("game.exe", now), SpawnDecision::Spawn);
            p.note_spawned("game.exe", now);
            p.note_exited(true);
            now += MIN_SPAWN_INTERVAL;
        }
        assert_eq!(
            p.decide("game.exe", now),
            SpawnDecision::RestartBudgetExhausted,
            "3 crashes in one session stops the retry loop"
        );
        // An orderly session-end exit resets the budget.
        p.note_exited(false);
        assert_eq!(p.decide("game.exe", now), SpawnDecision::Spawn);
    }
}
