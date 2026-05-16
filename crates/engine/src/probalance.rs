//! ProBalance — dynamic priority-class management on CPU contention.
//!
//! Clean-room reimplementation of the high-level behavior Bitsum's Process
//! Lasso documents under the same name. The intent: when the system is under
//! sustained CPU load AND a non-foreground process is the largest consumer,
//! temporarily lower that process's priority class one step so the foreground
//! app's threads stop being elbowed off the scheduler. Restore after a quiet
//! dwell. No rules required — this is the catch-all for the "Chrome spiked
//! up to 100% and my game stutters" scenario.
//!
//! Design choices for our version:
//!
//! * **Foreground-aware.** We never restrain the foreground PID. The whole
//!   point is to give it CPU.
//! * **Never touch rule-managed PIDs.** If the user wrote a rule for a
//!   process, their explicit profile wins. ProBalance only acts on the
//!   un-managed background.
//! * **Never touch the safe-list.** dwm.exe, audiodg.exe, csrss.exe, anti-
//!   cheat, AV, GPU drivers — the same denylist the game-mode planner uses.
//! * **One-step demotion only.** Normal → BelowNormal, BelowNormal → Idle.
//!   We never demote AboveNormal or High processes (probably a media app or
//!   game already with explicit priority).
//! * **Dwell window.** A restrained process stays restrained for at least
//!   `min_restrain_ms` after being demoted, regardless of how the load
//!   evolves. Avoids ping-pong on borderline-busy processes.
//! * **Always restorable.** We stash the raw Win32 priority class at demote
//!   time and write it back at restore time, so nothing is forgotten if a
//!   user toggled the process to AboveNormal via Task Manager mid-restraint.
//!
//! The state machine is small enough to live in pure Rust with no syscalls
//! itself — the engine passes in sampled CPU times each tick and we decide
//! who to restrain / restore. The actual kernel calls happen in the engine
//! via `framesage_sys::apply::{set_priority_class_for_pid, restore_…}`.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use framesage_core::{PriorityClass, ProBalanceConfig};

/// Per-PID restraint bookkeeping. The engine uses this to know what to
/// un-do once the dwell expires.
#[derive(Debug, Clone)]
pub struct RestrainedRecord {
    /// Raw Win32 priority class constant captured *before* we demoted the
    /// process. Restore writes this back literally — no interpretation.
    pub original_raw_class: u32,
    /// The class we demoted to. Mostly useful for diagnostics + the Event.
    pub demoted_to_raw_class: u32,
    /// Cached image filename (lowercased) for log/event labels.
    pub exe_name: String,
    /// When we restrained — `min_restrain_ms` is measured from this point.
    pub restrained_at: Instant,
}

/// One row of the input the engine hands to [`decide`]: per-PID CPU%
/// (as a fraction of one logical CPU, multiplied by 100) plus the exe name
/// we observed. Caller computes the % from two `cpu_times` samples + the
/// wall-clock delta.
#[derive(Debug, Clone)]
pub struct ProcessSample {
    pub pid: u32,
    pub exe_name: String,
    /// "CPU %" in the same units Task Manager shows — 100 means one fully
    /// busy logical CPU's worth of work; a process running 8 threads flat
    /// out on an 8-thread part would read 800.
    pub cpu_percent_of_one_cpu: u16,
    /// Current Win32 priority class constant. Used to decide whether we
    /// CAN demote (we won't touch Above-Normal or High).
    pub current_raw_class: u32,
}

/// Outcome of one decide pass. Engine consumes these by:
///   * calling `set_priority_class_for_pid` for each `Restrain`,
///   * calling `restore_priority_class_for_pid` for each `Restore`,
///   * emitting matching `Event::ProBalance{Restrained,Restored}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Restrain {
        pid: u32,
        exe_name: String,
        original_raw_class: u32,
        demote_to: PriorityClass,
        demote_to_raw_class: u32,
    },
    Restore {
        pid: u32,
        exe_name: String,
        restored_raw_class: u32,
    },
}

/// One CPU's worth of utilisation. `u16` is plenty — a process can run
/// many CPUs' worth but values past a few thousand never matter for the
/// thresholding logic, and `u16::MAX` (~65 535) corresponds to ~655 fully
/// busy threads.
pub type CpuPercent = u16;

/// Run the decision pass.
///
/// * `cfg` — current `ProBalanceConfig` from the policy. Caller already
///   checked `cfg.enabled`.
/// * `now` — for dwell-timer arithmetic; passed in so tests can mock it.
/// * `system_cpu_percent` — total system CPU utilisation (0-100). Above
///   `cfg.system_cpu_threshold_percent` we consider the box "under
///   contention" and become eligible to restrain. Below, we only restore.
/// * `foreground_pid` — PID of the current foreground process. Never
///   restrained, never even considered as a hog.
/// * `samples` — per-process CPU samples from this tick.
/// * `managed_pids` — set of PIDs the engine already manages via a rule
///   (i.e. has an entry in `s.applied`). ProBalance leaves these alone —
///   user-authored rules win.
/// * `safe_list_exes` — process names the safe-list refuses to touch
///   (dwm, audiodg, csrss, GPU drivers, anti-cheat, …). Lowercased.
/// * `restrained` — current restraint state, updated in-place.
/// * `hog_streak` — per-PID consecutive-hog-sample counter (item 4.6).
///   Incremented when a sample reads as a hog, reset to 0 when it
///   doesn't. The candidate must reach `cfg.min_restrain_samples`
///   before we'll demote.
///
/// Returns the list of state transitions the engine should turn into syscalls.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    cfg: &ProBalanceConfig,
    now: Instant,
    system_cpu_percent: u8,
    foreground_pid: Option<u32>,
    samples: &[ProcessSample],
    managed_pids: &HashSet<u32>,
    safe_list_exes: &HashSet<String>,
    user_ignore_exes: &HashSet<String>,
    restrained: &mut HashMap<u32, RestrainedRecord>,
    hog_streak: &mut HashMap<u32, u32>,
) -> Vec<Decision> {
    let mut decisions = Vec::new();
    let live_pids: HashSet<u32> = samples.iter().map(|s| s.pid).collect();
    let min_restrain = Duration::from_millis(cfg.min_restrain_ms);
    // Item 4.6 — 0 and 1 are equivalent (a single sample suffices). We
    // floor at 1 so a config bug (zero) can never short-circuit the
    // existing "must be a hog this very tick" baseline.
    let min_samples = cfg.min_restrain_samples.max(1);

    // ─── Step 1: restore eligibility ───────────────────────────────────────
    //
    // Anything currently restrained whose:
    //   * dwell timer has expired AND
    //   * the system is no longer under contention OR
    //   * the process is no longer a hog OR
    //   * the process is now the foreground OR
    //   * the process has exited (no sample)
    // …gets restored. We pre-compute the set of "still a problem" PIDs to
    // avoid an O(n*m) scan inside the iteration.
    let still_hogs: HashSet<u32> = samples
        .iter()
        .filter(|s| {
            Some(s.pid) != foreground_pid
                && s.cpu_percent_of_one_cpu >= cfg.hog_cpu_threshold_percent
        })
        .map(|s| s.pid)
        .collect();
    let system_under_contention = system_cpu_percent >= cfg.system_cpu_threshold_percent;

    let restore_candidates: Vec<u32> = restrained
        .iter()
        .filter_map(|(pid, rec)| {
            let dwell_satisfied = now.duration_since(rec.restrained_at) >= min_restrain;
            if !dwell_satisfied {
                return None;
            }
            let still_problem =
                system_under_contention && still_hogs.contains(pid) && live_pids.contains(pid);
            if still_problem {
                None
            } else {
                Some(*pid)
            }
        })
        .collect();

    for pid in restore_candidates {
        if let Some(rec) = restrained.remove(&pid) {
            decisions.push(Decision::Restore {
                pid,
                exe_name: rec.exe_name,
                restored_raw_class: rec.original_raw_class,
            });
        }
    }

    // ─── Step 2: streak bookkeeping ────────────────────────────────────────
    //
    // Item 4.6 — maintain a per-PID "consecutive samples reading as a hog"
    // counter. Increment on any sample that crosses the hog threshold (so
    // PIDs that are already restrained also accumulate — harmless, and
    // means the counter is "warm" if the user clears the restraint
    // manually). Reset to 0 for any sample below threshold. Reap entries
    // for PIDs that disappeared from the sample list.
    //
    // Done BEFORE the contention gate so we still reset streaks for PIDs
    // that dropped below threshold during a quiet window — otherwise a
    // PID that was at 79% for 10 ticks would instantly re-restrain the
    // moment system pressure returned, without re-earning the right.
    for sample in samples {
        if sample.cpu_percent_of_one_cpu >= cfg.hog_cpu_threshold_percent {
            *hog_streak.entry(sample.pid).or_insert(0) += 1;
        } else {
            hog_streak.remove(&sample.pid);
        }
    }
    hog_streak.retain(|pid, _| live_pids.contains(pid));

    // ─── Step 3: restrain eligibility ──────────────────────────────────────
    //
    // Only when the system is genuinely under load — below the threshold we
    // have no reason to elbow anyone aside.
    if !system_under_contention {
        return decisions;
    }

    // Pick candidates: non-foreground, non-managed, non-safe-list,
    // not user-ignored, not already restrained, currently a hog,
    // demotable from current class, AND has been a hog for at least
    // `min_samples` consecutive samples (item 4.6 hysteresis).
    let mut candidates: Vec<&ProcessSample> = samples
        .iter()
        .filter(|s| {
            Some(s.pid) != foreground_pid
                && !managed_pids.contains(&s.pid)
                && !restrained.contains_key(&s.pid)
                && s.cpu_percent_of_one_cpu >= cfg.hog_cpu_threshold_percent
                && hog_streak.get(&s.pid).copied().unwrap_or(0) >= min_samples
                && !safe_list_exes.contains(&s.exe_name.to_ascii_lowercase())
                && !user_ignore_exes.contains(&s.exe_name.to_ascii_lowercase())
                && demotion_target(s.current_raw_class).is_some()
        })
        .collect();

    // Highest CPU% first — single largest hog goes first.
    candidates.sort_by_key(|s| std::cmp::Reverse(s.cpu_percent_of_one_cpu));

    for sample in candidates {
        let Some((demote_to, demote_to_raw)) = demotion_target(sample.current_raw_class) else {
            continue;
        };
        restrained.insert(
            sample.pid,
            RestrainedRecord {
                original_raw_class: sample.current_raw_class,
                demoted_to_raw_class: demote_to_raw,
                exe_name: sample.exe_name.clone(),
                restrained_at: now,
            },
        );
        decisions.push(Decision::Restrain {
            pid: sample.pid,
            exe_name: sample.exe_name.clone(),
            original_raw_class: sample.current_raw_class,
            demote_to,
            demote_to_raw_class: demote_to_raw,
        });
    }

    decisions
}

/// What we demote a given raw Win32 class to, if anything. Returns `None`
/// for classes we refuse to touch: `IDLE_PRIORITY_CLASS` (already at the
/// floor), `ABOVE_NORMAL_PRIORITY_CLASS` / `HIGH_PRIORITY_CLASS` /
/// `REALTIME_PRIORITY_CLASS` (something is explicitly running them hot —
/// not our place to interfere), or `PROCESS_MODE_BACKGROUND_*` flags.
///
/// Constants:
///   IDLE_PRIORITY_CLASS         0x00000040
///   BELOW_NORMAL_PRIORITY_CLASS 0x00004000
///   NORMAL_PRIORITY_CLASS       0x00000020
///   ABOVE_NORMAL_PRIORITY_CLASS 0x00008000
///   HIGH_PRIORITY_CLASS         0x00000080
///   REALTIME_PRIORITY_CLASS     0x00000100
fn demotion_target(raw_class: u32) -> Option<(PriorityClass, u32)> {
    match raw_class {
        0x0000_0020 => Some((PriorityClass::BelowNormal, 0x0000_4000)), // Normal -> BelowNormal
        0x0000_4000 => Some((PriorityClass::Idle, 0x0000_0040)),        // BelowNormal -> Idle
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ProBalanceConfig {
        ProBalanceConfig {
            enabled: true,
            system_cpu_threshold_percent: 70,
            hog_cpu_threshold_percent: 80,
            min_restrain_ms: 1000,
            // Tests that pre-date item 4.6 expect instant restraint; keep
            // their semantics by defaulting to 1 sample here. New tests
            // that exercise hysteresis override to 2+.
            min_restrain_samples: 1,
            ignore_processes: vec![],
        }
    }

    fn now() -> Instant {
        Instant::now()
    }

    fn sample(pid: u32, exe: &str, cpu: u16, raw_class: u32) -> ProcessSample {
        ProcessSample {
            pid,
            exe_name: exe.into(),
            cpu_percent_of_one_cpu: cpu,
            current_raw_class: raw_class,
        }
    }

    #[test]
    fn below_threshold_does_nothing() {
        let mut r = HashMap::new();
        let d = decide(
            &cfg(),
            now(),
            50, // system idle
            Some(1),
            &[sample(2, "chrome.exe", 200, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        assert!(d.is_empty());
        assert!(r.is_empty());
    }

    #[test]
    fn restrains_top_hog_under_contention() {
        let mut r = HashMap::new();
        let d = decide(
            &cfg(),
            now(),
            90,
            Some(1),
            &[
                sample(2, "chrome.exe", 250, 0x20),
                sample(3, "notepad.exe", 5, 0x20),
            ],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        assert_eq!(d.len(), 1);
        match &d[0] {
            Decision::Restrain {
                pid,
                demote_to_raw_class,
                ..
            } => {
                assert_eq!(*pid, 2);
                assert_eq!(*demote_to_raw_class, 0x4000); // BelowNormal
            }
            other => panic!("expected Restrain, got {other:?}"),
        }
        assert!(r.contains_key(&2));
    }

    #[test]
    fn skips_foreground_pid() {
        let mut r = HashMap::new();
        let d = decide(
            &cfg(),
            now(),
            90,
            Some(2),
            &[sample(2, "chrome.exe", 300, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        assert!(d.is_empty(), "must not restrain the foreground process");
    }

    #[test]
    fn skips_managed_pid() {
        let mut r = HashMap::new();
        let managed: HashSet<u32> = [2u32].into_iter().collect();
        let d = decide(
            &cfg(),
            now(),
            90,
            Some(1),
            &[sample(2, "chrome.exe", 300, 0x20)],
            &managed,
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        assert!(d.is_empty(), "rule-managed PIDs must be left to their rule");
    }

    #[test]
    fn skips_safe_list() {
        let mut r = HashMap::new();
        let safe: HashSet<String> = ["audiodg.exe".into()].into_iter().collect();
        let d = decide(
            &cfg(),
            now(),
            90,
            Some(1),
            &[sample(2, "audiodg.exe", 300, 0x20)],
            &HashSet::new(),
            &safe,
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        assert!(d.is_empty(), "safe-listed exes must never be touched");
    }

    #[test]
    fn skips_user_ignore_list() {
        let mut r = HashMap::new();
        let ignore: HashSet<String> = ["obs64.exe".into()].into_iter().collect();
        let d = decide(
            &cfg(),
            now(),
            90,
            Some(1),
            &[sample(2, "OBS64.exe", 300, 0x20)], // case mismatch deliberate
            &HashSet::new(),
            &HashSet::new(),
            &ignore,
            &mut r,
            &mut HashMap::new(),
        );
        assert!(
            d.is_empty(),
            "user ignore list must be matched case-insensitively"
        );
    }

    #[test]
    fn refuses_to_demote_above_normal() {
        let mut r = HashMap::new();
        let d = decide(
            &cfg(),
            now(),
            90,
            Some(1),
            &[sample(2, "encoder.exe", 300, 0x8000)], // AboveNormal
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        assert!(
            d.is_empty(),
            "AboveNormal+ processes are explicit and not our place to touch"
        );
    }

    #[test]
    fn dwell_window_holds_restraint() {
        let mut r = HashMap::new();
        let t0 = Instant::now();
        // First pass: restrain.
        decide(
            &cfg(),
            t0,
            90,
            Some(1),
            &[sample(2, "chrome.exe", 300, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        assert!(r.contains_key(&2));

        // Second pass, system back to idle, ONLY 500 ms later (under dwell):
        // must NOT restore yet.
        let d = decide(
            &cfg(),
            t0 + Duration::from_millis(500),
            10,
            Some(1),
            &[sample(2, "chrome.exe", 5, 0x4000)], // now BelowNormal as we demoted
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        assert!(
            d.is_empty(),
            "dwell window must protect against immediate ping-pong"
        );
        assert!(r.contains_key(&2));
    }

    #[test]
    fn restores_after_dwell_and_quiet() {
        let mut r = HashMap::new();
        let t0 = Instant::now();
        decide(
            &cfg(),
            t0,
            90,
            Some(1),
            &[sample(2, "chrome.exe", 300, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        // Past dwell, system quiet:
        let d = decide(
            &cfg(),
            t0 + Duration::from_millis(2000),
            20,
            Some(1),
            &[sample(2, "chrome.exe", 5, 0x4000)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        assert_eq!(d.len(), 1);
        match &d[0] {
            Decision::Restore {
                pid,
                restored_raw_class,
                ..
            } => {
                assert_eq!(*pid, 2);
                assert_eq!(*restored_raw_class, 0x20); // back to Normal
            }
            other => panic!("expected Restore, got {other:?}"),
        }
        assert!(r.is_empty(), "restored record should be removed");
    }

    #[test]
    fn restores_when_process_becomes_foreground() {
        let mut r = HashMap::new();
        let t0 = Instant::now();
        decide(
            &cfg(),
            t0,
            90,
            Some(1),
            &[sample(2, "chrome.exe", 300, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        let d = decide(
            &cfg(),
            t0 + Duration::from_millis(2000),
            90,      // system still busy
            Some(2), // but Chrome is now foreground — must restore
            &[sample(2, "chrome.exe", 300, 0x4000)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        assert_eq!(d.len(), 1);
        assert!(matches!(d[0], Decision::Restore { pid: 2, .. }));
    }

    #[test]
    fn restores_when_process_exits() {
        let mut r = HashMap::new();
        let t0 = Instant::now();
        decide(
            &cfg(),
            t0,
            90,
            Some(1),
            &[sample(2, "chrome.exe", 300, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        // After dwell, the process is no longer in the sample list (exited).
        // The restored decision should still be emitted so the restraint
        // bookkeeping doesn't leak.
        let d = decide(
            &cfg(),
            t0 + Duration::from_millis(2000),
            90,
            Some(1),
            &[],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut HashMap::new(),
        );
        assert_eq!(d.len(), 1);
        assert!(matches!(d[0], Decision::Restore { pid: 2, .. }));
        assert!(r.is_empty());
    }

    // ─── Item 4.6 — restrain-side hysteresis ────────────────────────────
    //
    // The 2-sample default closes M-18 (single-tick CPU spikes triggered
    // false-positive restraints). Each test below pins one specific
    // behavior: a 1-sample hog must NOT be demoted under the new
    // default; 2 consecutive samples must demote; a hog that drops below
    // threshold mid-streak must lose its accumulated count.

    fn cfg_hysteresis_2() -> ProBalanceConfig {
        ProBalanceConfig {
            min_restrain_samples: 2,
            ..cfg()
        }
    }

    #[test]
    fn hysteresis_one_sample_hog_does_not_restrain() {
        let mut r = HashMap::new();
        let mut hs = HashMap::new();
        let d = decide(
            &cfg_hysteresis_2(),
            now(),
            90,
            Some(1),
            &[sample(2, "chrome.exe", 300, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut hs,
        );
        assert!(
            d.is_empty(),
            "single-sample hog must NOT trip the 2-sample hysteresis"
        );
        assert!(r.is_empty(), "no restraint record on first sample");
        assert_eq!(
            hs.get(&2).copied(),
            Some(1),
            "first hog sample seeds the streak counter at 1"
        );
    }

    #[test]
    fn hysteresis_two_consecutive_samples_restrain() {
        let mut r = HashMap::new();
        let mut hs = HashMap::new();
        let t0 = Instant::now();
        // Sample 1: hog but below the 2-sample bar — no restraint.
        let d1 = decide(
            &cfg_hysteresis_2(),
            t0,
            90,
            Some(1),
            &[sample(2, "chrome.exe", 300, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut hs,
        );
        assert!(d1.is_empty());

        // Sample 2: same hog, streak reaches 2 — must restrain.
        let d2 = decide(
            &cfg_hysteresis_2(),
            t0 + Duration::from_millis(300),
            90,
            Some(1),
            &[sample(2, "chrome.exe", 300, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut hs,
        );
        assert_eq!(d2.len(), 1);
        assert!(matches!(d2[0], Decision::Restrain { pid: 2, .. }));
        assert!(r.contains_key(&2));
    }

    #[test]
    fn hysteresis_streak_resets_on_below_threshold_sample() {
        let mut r = HashMap::new();
        let mut hs = HashMap::new();
        let t0 = Instant::now();
        let cfg = cfg_hysteresis_2();

        // Two hog samples — but interrupted by one quiet sample. The
        // streak must reset, so the second hog sample is only count=1
        // again and must NOT restrain.
        decide(
            &cfg,
            t0,
            90,
            Some(1),
            &[sample(2, "chrome.exe", 300, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut hs,
        );
        assert_eq!(hs.get(&2).copied(), Some(1));

        // Quiet sample (below threshold): streak resets.
        decide(
            &cfg,
            t0 + Duration::from_millis(300),
            90,
            Some(1),
            &[sample(2, "chrome.exe", 10, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut hs,
        );
        assert!(
            !hs.contains_key(&2),
            "below-threshold sample must remove the streak entry"
        );

        // Hog returns — streak is back to 1, not 2. Must not restrain.
        let d = decide(
            &cfg,
            t0 + Duration::from_millis(600),
            90,
            Some(1),
            &[sample(2, "chrome.exe", 300, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut hs,
        );
        assert!(
            d.is_empty(),
            "post-reset, a single hog sample must not be enough"
        );
        assert!(r.is_empty());
    }

    /// Hysteresis must NOT delay a one-sample hog when
    /// `min_restrain_samples = 1` — that's the pre-4.6 baseline and
    /// existing tests should still pass. This is also a sanity check
    /// on the `.max(1)` floor we apply inside `decide`.
    #[test]
    fn hysteresis_one_sample_default_preserves_legacy_behavior() {
        let mut r = HashMap::new();
        let mut hs = HashMap::new();
        let d = decide(
            &cfg(), // cfg() returns min_restrain_samples = 1
            now(),
            90,
            Some(1),
            &[sample(2, "chrome.exe", 300, 0x20)],
            &HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
            &mut r,
            &mut hs,
        );
        assert_eq!(d.len(), 1, "legacy 1-sample config must restrain instantly");
        assert!(matches!(d[0], Decision::Restrain { pid: 2, .. }));
    }
}
