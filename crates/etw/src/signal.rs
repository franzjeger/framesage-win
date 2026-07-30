//! §2.3 `kernel_signal` detection — rolling-baseline spike detector.
//!
//! The schema's rule: each signal is "emitted only when current rate
//! exceeds 3× the rolling 5-minute baseline (a 'signal', not a
//! sample)". This module is the pure implementation: feed it one
//! per-kind cumulative-count snapshot per second (from the consumer's
//! atomic counters) and it emits at most one `KernelSignal` per kind
//! per cooldown window.
//!
//! Scaffold-documented judgment calls (§2.3 leaves them open):
//!
//! * **Warm-up:** no emission until a kind has ≥ [`MIN_BASELINE_SECS`]
//!   seconds of history — a 3× rule against a 2-second baseline is
//!   noise.
//! * **Rate floor:** rates below [`MIN_RATE_PER_SEC`] never signal,
//!   so a near-idle baseline (0–2 events/sec) can't make trivial
//!   activity look like a storm.
//! * **Cooldown:** once a kind fires, it stays quiet for
//!   [`COOLDOWN_SECS`] — "a signal, not a sample" means a sustained
//!   storm is one line (plus follow-ups every cooldown), not 1 Hz
//!   spam into the session file.
//!
//! Time is an injected second index; no clocks, fully deterministic
//! tests.

use crate::classify::{KernelEventKind, KERNEL_EVENT_KINDS};

/// Rolling-baseline window per §2.3: 5 minutes.
pub const BASELINE_WINDOW_SECS: usize = 300;
/// Emission threshold multiplier per §2.3.
pub const SPIKE_MULTIPLIER: f64 = 3.0;
/// Warm-up: minimum seconds of history before a kind may signal.
pub const MIN_BASELINE_SECS: usize = 30;
/// Rates below this never signal regardless of baseline ratio.
pub const MIN_RATE_PER_SEC: u64 = 50;
/// Per-kind quiet period after an emission.
pub const COOLDOWN_SECS: u64 = 10;

/// One emitted signal — maps 1:1 onto the recorder's
/// `SessionEvent::KernelSignal` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelSignal {
    pub kind: KernelEventKind,
    /// §2.3 `signal` discriminant string.
    pub signal: &'static str,
    pub rate_per_sec: u64,
    pub baseline_5min_per_sec: u64,
    pub above_baseline_pct: u64,
}

#[derive(Debug)]
struct KindState {
    /// Per-second rates, newest at the back, capped at the window.
    history: std::collections::VecDeque<u64>,
    last_cumulative: u64,
    /// Second index of the last emission, for the cooldown.
    last_emitted_at: Option<u64>,
}

impl Default for KindState {
    fn default() -> Self {
        Self {
            history: std::collections::VecDeque::with_capacity(BASELINE_WINDOW_SECS),
            last_cumulative: 0,
            last_emitted_at: None,
        }
    }
}

/// The detector. One instance per ETW session; feed once per second.
#[derive(Debug, Default)]
pub struct KernelSignalDetector {
    kinds: [KindState; KERNEL_EVENT_KINDS],
}

impl KernelSignalDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one snapshot of the per-kind **cumulative** counters,
    /// taken at second `second_index` since session start. Returns
    /// the signals that fire this second.
    pub fn tick(
        &mut self,
        second_index: u64,
        cumulative_counts: &[u64; KERNEL_EVENT_KINDS],
    ) -> Vec<KernelSignal> {
        let mut out = Vec::new();
        for kind in KernelEventKind::all() {
            let idx = kind as usize;
            let state = &mut self.kinds[idx];
            let cumulative = cumulative_counts[idx];
            let rate = cumulative.saturating_sub(state.last_cumulative);
            state.last_cumulative = cumulative;

            // Baseline from history BEFORE pushing the current second
            // — the spike itself must not inflate its own baseline.
            let baseline = if state.history.len() >= MIN_BASELINE_SECS {
                let sum: u64 = state.history.iter().sum();
                Some(sum / state.history.len() as u64)
            } else {
                None
            };

            state.history.push_back(rate);
            if state.history.len() > BASELINE_WINDOW_SECS {
                state.history.pop_front();
            }

            let Some(baseline) = baseline else { continue };
            if rate < MIN_RATE_PER_SEC {
                continue;
            }
            if (rate as f64) <= (baseline as f64) * SPIKE_MULTIPLIER {
                continue;
            }
            if let Some(last) = state.last_emitted_at {
                if second_index.saturating_sub(last) < COOLDOWN_SECS {
                    continue;
                }
            }
            state.last_emitted_at = Some(second_index);
            let above_pct = if baseline == 0 {
                // Idle baseline exploded straight past the floor —
                // report the rate itself as the percentage anchor.
                rate.saturating_mul(100)
            } else {
                ((rate as f64 - baseline as f64) / baseline as f64 * 100.0).round() as u64
            };
            out.push(KernelSignal {
                kind,
                signal: kind.signal_name(),
                rate_per_sec: rate,
                baseline_5min_per_sec: baseline,
                above_baseline_pct: above_pct,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_steady(
        det: &mut KernelSignalDetector,
        kind: KernelEventKind,
        seconds: u64,
        rate: u64,
        start_second: u64,
        start_cumulative: u64,
    ) -> u64 {
        let mut cumulative = start_cumulative;
        for s in 0..seconds {
            cumulative += rate;
            let mut counts = [0u64; KERNEL_EVENT_KINDS];
            counts[kind as usize] = cumulative;
            let signals = det.tick(start_second + s, &counts);
            assert!(
                signals.is_empty(),
                "steady load must not signal (second {})",
                start_second + s
            );
        }
        cumulative
    }

    #[test]
    fn spike_over_3x_baseline_emits_the_section_2_3_payload() {
        let mut det = KernelSignalDetector::new();
        let kind = KernelEventKind::Dpc;
        // 60 s of 3200 DPCs/sec baseline (the §2.3 example numbers).
        let cumulative = feed_steady(&mut det, kind, 60, 3200, 0, 0);
        // Then one second at 14823/sec.
        let mut counts = [0u64; KERNEL_EVENT_KINDS];
        counts[kind as usize] = cumulative + 14_823;
        let signals = det.tick(60, &counts);
        assert_eq!(signals.len(), 1);
        let s = &signals[0];
        assert_eq!(s.signal, "dpc_spike");
        assert_eq!(s.rate_per_sec, 14_823);
        assert_eq!(s.baseline_5min_per_sec, 3200);
        assert_eq!(s.above_baseline_pct, 363, "matches the §2.3 example");
    }

    #[test]
    fn warm_up_and_rate_floor_suppress_noise() {
        let mut det = KernelSignalDetector::new();
        let kind = KernelEventKind::HardFault;
        // Second 5, huge ratio but only 10 s of history → warm-up.
        let mut cumulative = 0;
        for s in 0..10 {
            cumulative += 2;
            let mut counts = [0u64; KERNEL_EVENT_KINDS];
            counts[kind as usize] = cumulative;
            assert!(det.tick(s, &counts).is_empty());
        }
        // After warm-up (30 s at ~2/sec), a jump to 40/sec is 20× the
        // baseline but below the 50/sec floor → still quiet.
        cumulative = feed_steady(&mut det, kind, 30, 2, 10, cumulative);
        let mut counts = [0u64; KERNEL_EVENT_KINDS];
        counts[kind as usize] = cumulative + 40;
        assert!(
            det.tick(40, &counts).is_empty(),
            "sub-floor rates never signal"
        );
    }

    #[test]
    fn cooldown_collapses_a_sustained_storm_into_sparse_signals() {
        let mut det = KernelSignalDetector::new();
        let kind = KernelEventKind::ContextSwitch;
        let mut cumulative = feed_steady(&mut det, kind, 60, 1000, 0, 0);
        let mut emissions = 0;
        for s in 0..COOLDOWN_SECS {
            cumulative += 10_000;
            let mut counts = [0u64; KERNEL_EVENT_KINDS];
            counts[kind as usize] = cumulative;
            emissions += det.tick(60 + s, &counts).len();
        }
        assert_eq!(
            emissions, 1,
            "10 storm-seconds inside one cooldown window = one signal"
        );
    }

    #[test]
    fn kinds_are_tracked_independently() {
        let mut det = KernelSignalDetector::new();
        // Warm both kinds up in the same ticks.
        let mut counts = [0u64; KERNEL_EVENT_KINDS];
        for s in 0..60 {
            counts[KernelEventKind::Dpc as usize] += 1000;
            counts[KernelEventKind::DiskIo as usize] += 200;
            assert!(det.tick(s, &counts).is_empty());
        }
        // DiskIo spikes; DPC stays steady.
        counts[KernelEventKind::Dpc as usize] += 1000;
        counts[KernelEventKind::DiskIo as usize] += 5_000;
        let signals = det.tick(60, &counts);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].signal, "diskio_spike");
    }
}
