//! "Did FrameSage help?" attribution — full spec from
//! `audit/v0.7-architecture.md` §2.4.
//!
//! This is the load-bearing honest-attribution computation. The
//! architecture commits to telling the truth, including negative
//! results:
//!
//! * Baseline window = first 60 s, mechanically truncated at the first
//!   `apply_profile` / `game_mode_entered` action — the user can't
//!   cherry-pick windows.
//! * Deliberately **asymmetric** delta bands: slow to take credit
//!   (positive claim threshold 8%), quick to admit harm (degraded
//!   banner at +5%).
//! * Attribution is *disabled* — with an explicit reason — rather than
//!   silently computed on sessions that can't support the claim.
//!
//! Band boundaries are policy decisions, not implementation details;
//! the honesty-contract tests below pin them with the exact substrings
//! the architecture specifies.

use crate::schema::SessionEvent;

/// Baseline must be at least this long or attribution is disabled.
pub const MIN_BASELINE_SECS: u64 = 30;
/// Baseline is capped at this length even if no rule fires earlier.
pub const MAX_BASELINE_SECS: u64 = 60;
/// With-rules window must be at least this long.
pub const MIN_WITH_RULES_SECS: u64 = 60;

/// Why attribution could not be computed for a session (§2.4
/// "Disabled-attribution states"). The panel must say explicitly WHY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisabledReason {
    /// With-rules window < 60 s (session < ~90 s total).
    SessionTooShort,
    /// < 30 s of samples before the first apply.
    BaselineTooShort,
    /// PresentMon failed or wasn't enabled — no frame samples in one
    /// or both windows.
    FrameDataUnavailable,
    /// `session_end.partial_data == true`. The user can opt into
    /// "show anyway" — the computed summary (when computable) rides
    /// along in [`Attribution::Disabled`].
    PartialData,
    /// No `apply_profile` / `game_mode_entered` action in the session:
    /// there is nothing to attribute.
    NoRuleFired,
}

impl DisabledReason {
    /// Panel copy per §2.4 — these strings are part of the honesty
    /// contract surface.
    pub fn message(&self) -> &'static str {
        match self {
            DisabledReason::SessionTooShort => "Session too short for attribution",
            DisabledReason::BaselineTooShort => "Baseline too short",
            DisabledReason::FrameDataUnavailable => "Frame data unavailable",
            DisabledReason::PartialData => "Partial data — drops detected",
            DisabledReason::NoRuleFired => "No rule fired during this session",
        }
    }
}

/// The asymmetric delta band for the headline 1% lows number (§2.4
/// "Delta reporting rules — ASYMMETRIC").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeltaBand {
    /// delta < -8% — "improved by N%" (green)
    Improved,
    /// -8% ..= -3% — "Modest improvement" (gray)
    ModestImprovement,
    /// -3% ..= +3% — "No measurable effect" (gray)
    NoEffect,
    /// +3% ..= +5% — "Slight regression" (yellow)
    SlightRegression,
    /// > +5% — "degraded … Review this profile." (yellow + link)
    Degraded,
}

impl DeltaBand {
    /// Classify a 1%-lows delta (percent; negative = improvement).
    ///
    /// Boundary semantics follow the §2.4 table: `< -8` is Improved
    /// (so exactly -8 is Modest), `> +5` is Degraded (so exactly +5
    /// is SlightRegression), and the gray band is inclusive at ±3.
    pub fn classify(p99_delta_pct: f64) -> Self {
        if p99_delta_pct < -8.0 {
            DeltaBand::Improved
        } else if p99_delta_pct < -3.0 {
            DeltaBand::ModestImprovement
        } else if p99_delta_pct <= 3.0 {
            DeltaBand::NoEffect
        } else if p99_delta_pct <= 5.0 {
            DeltaBand::SlightRegression
        } else {
            DeltaBand::Degraded
        }
    }
}

/// Computed attribution numbers for a session.
#[derive(Debug, Clone, PartialEq)]
pub struct AttributionSummary {
    /// Mean of `frame_time_us_p50`, baseline → with-rules, percent.
    pub avg_frame_time_delta_pct: f64,
    /// 99th percentile of the `frame_time_us_p99` values, baseline →
    /// with-rules, percent. The headline number.
    pub p99_delta_pct: f64,
    /// Variance of `frame_time_us_p50`, baseline → with-rules, percent.
    pub variance_delta_pct: f64,
    pub band: DeltaBand,
    /// The rendered one-line headline per the §2.4 band table. The
    /// honesty-contract tests assert exact substrings of this string.
    pub headline: String,
    /// Baseline window in ms since session start (start, end).
    pub baseline_window_ms: (u64, u64),
    /// With-rules window in ms since session start (start, end).
    pub with_rules_window_ms: (u64, u64),
}

/// Result of [`compute_attribution_summary`].
#[derive(Debug, Clone, PartialEq)]
pub enum Attribution {
    /// Attribution disabled; the panel shows `reason.message()`.
    /// For [`DisabledReason::PartialData`] the summary is still
    /// computed when possible so the UI can offer "show anyway" with
    /// an explicit caveat.
    Disabled {
        reason: DisabledReason,
        computed_anyway: Option<Box<AttributionSummary>>,
    },
    Computed(Box<AttributionSummary>),
}

/// §2.4 headline copy per band. `n` is the absolute delta rounded to
/// the nearest whole percent.
///
/// The Degraded copy embeds the literal `**degraded**` marker — the
/// Group C acceptance criterion asserts that exact substring, and the
/// UI renders the emphasis.
fn headline_for(band: DeltaBand, p99_delta_pct: f64) -> String {
    let n = p99_delta_pct.abs().round() as i64;
    match band {
        DeltaBand::Improved => format!("Game Mode improved your 1% lows by {n}%"),
        DeltaBand::ModestImprovement => format!("Modest improvement in 1% lows: {n}%"),
        DeltaBand::NoEffect => "No measurable effect on 1% lows".to_string(),
        DeltaBand::SlightRegression => format!("Slight regression in 1% lows: {n}%"),
        DeltaBand::Degraded => {
            format!("Game Mode **degraded** your 1% lows by {n}%. Review this profile.")
        }
    }
}

/// Actions that start the with-rules window (§2.4 "Baseline window").
fn is_windowing_action(action: &str) -> bool {
    action == "apply_profile" || action == "game_mode_entered"
}

fn mean(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    Some(values.iter().sum::<f64>() / values.len() as f64)
}

fn variance(values: &[f64]) -> Option<f64> {
    let m = mean(values)?;
    if values.len() < 2 {
        return None;
    }
    Some(values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / values.len() as f64)
}

/// 99th percentile (nearest-rank) of a value set.
fn p99(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = ((0.99 * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    Some(sorted[rank - 1])
}

fn delta_pct(baseline: f64, with_rules: f64) -> f64 {
    if baseline == 0.0 {
        return 0.0;
    }
    (with_rules - baseline) / baseline * 100.0
}

/// Compute the §2.4 attribution for a full session event stream.
///
/// The stream is expected in file order (`session_start` first,
/// `session_end` last) but only ordering of `at_ms` values is relied
/// upon.
pub fn compute_attribution_summary(events: &[SessionEvent]) -> Attribution {
    let session_end_ms = events
        .iter()
        .rev()
        .find_map(|e| match e {
            SessionEvent::SessionEnd { at_ms, .. } => Some(*at_ms),
            _ => None,
        })
        .unwrap_or_else(|| events.iter().map(SessionEvent::at_ms).max().unwrap_or(0));

    let partial_data = events.iter().any(|e| {
        matches!(
            e,
            SessionEvent::SessionEnd {
                partial_data: true,
                ..
            }
        )
    });

    let first_apply_ms = events.iter().find_map(|e| match e {
        SessionEvent::FramesageAction { at_ms, action, .. } if is_windowing_action(action) => {
            Some(*at_ms)
        }
        _ => None,
    });

    let Some(first_apply_ms) = first_apply_ms else {
        return Attribution::Disabled {
            reason: DisabledReason::NoRuleFired,
            computed_anyway: None,
        };
    };

    // Baseline: 0 → min(first apply, 60 s). Mechanically defined —
    // no window picking.
    let baseline_end_ms = first_apply_ms.min(MAX_BASELINE_SECS * 1000);
    if baseline_end_ms < MIN_BASELINE_SECS * 1000 {
        return Attribution::Disabled {
            reason: DisabledReason::BaselineTooShort,
            computed_anyway: None,
        };
    }

    // With-rules: first apply → session end; at least 60 s.
    if session_end_ms.saturating_sub(first_apply_ms) < MIN_WITH_RULES_SECS * 1000 {
        return Attribution::Disabled {
            reason: DisabledReason::SessionTooShort,
            computed_anyway: None,
        };
    }

    let mut base_p50: Vec<f64> = Vec::new();
    let mut base_p99: Vec<f64> = Vec::new();
    let mut rules_p50: Vec<f64> = Vec::new();
    let mut rules_p99: Vec<f64> = Vec::new();
    for e in events {
        if let SessionEvent::FrameSample {
            at_ms,
            frame_time_us_p50,
            frame_time_us_p99,
            ..
        } = e
        {
            if *at_ms < baseline_end_ms {
                base_p50.push(*frame_time_us_p50 as f64);
                base_p99.push(*frame_time_us_p99 as f64);
            } else if *at_ms >= first_apply_ms {
                rules_p50.push(*frame_time_us_p50 as f64);
                rules_p99.push(*frame_time_us_p99 as f64);
            }
        }
    }

    let (Some(base_avg), Some(rules_avg), Some(base_lows), Some(rules_lows)) = (
        mean(&base_p50),
        mean(&rules_p50),
        p99(&base_p99),
        p99(&rules_p99),
    ) else {
        return Attribution::Disabled {
            reason: DisabledReason::FrameDataUnavailable,
            computed_anyway: None,
        };
    };

    let p99_delta = delta_pct(base_lows, rules_lows);
    let band = DeltaBand::classify(p99_delta);
    let summary = Box::new(AttributionSummary {
        avg_frame_time_delta_pct: delta_pct(base_avg, rules_avg),
        p99_delta_pct: p99_delta,
        variance_delta_pct: match (variance(&base_p50), variance(&rules_p50)) {
            (Some(b), Some(w)) if b > 0.0 => delta_pct(b, w),
            _ => 0.0,
        },
        band,
        headline: headline_for(band, p99_delta),
        baseline_window_ms: (0, baseline_end_ms),
        with_rules_window_ms: (first_apply_ms, session_end_ms),
    });

    if partial_data {
        // §2.4: partial data disables confidence claims; the computed
        // numbers ride along for the explicit "show anyway" opt-in.
        return Attribution::Disabled {
            reason: DisabledReason::PartialData,
            computed_anyway: Some(summary),
        };
    }

    Attribution::Computed(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{SessionEvent, SessionSummary, SCHEMA_VERSION};

    /// Build a synthetic session whose with-rules 1% lows differ from
    /// baseline by `p99_delta_pct` percent. Baseline: 60 s of samples
    /// at p50=16000us / p99=22000us; apply at 60 s; with-rules: 120 s
    /// of samples with p99 scaled by the delta.
    fn session_with_p99_delta(p99_delta_pct: f64) -> Vec<SessionEvent> {
        session_with_p99_delta_opts(p99_delta_pct, false)
    }

    fn session_with_p99_delta_opts(p99_delta_pct: f64, partial: bool) -> Vec<SessionEvent> {
        let mut events = Vec::new();
        let base_p50 = 16_000u64;
        let base_p99 = 22_000u64;
        let rules_p99 = (base_p99 as f64 * (1.0 + p99_delta_pct / 100.0)).round() as u64;
        for sec in 0..60 {
            events.push(SessionEvent::FrameSample {
                schema_version: SCHEMA_VERSION,
                at_ms: sec * 1000,
                frame_count: 60,
                frame_time_us_p50: base_p50,
                frame_time_us_p99: base_p99,
                frames_dropped: 0,
            });
        }
        events.push(SessionEvent::FramesageAction {
            schema_version: SCHEMA_VERSION,
            at_ms: 60_000,
            action: "apply_profile".into(),
            profile_id: "game-x3d".into(),
            details: None,
        });
        for sec in 60..180 {
            events.push(SessionEvent::FrameSample {
                schema_version: SCHEMA_VERSION,
                at_ms: sec * 1000,
                frame_count: 60,
                frame_time_us_p50: base_p50,
                frame_time_us_p99: rules_p99,
                frames_dropped: 0,
            });
        }
        events.push(SessionEvent::SessionEnd {
            schema_version: SCHEMA_VERSION,
            at_ms: 180_000,
            reason: "foreground_lost".into(),
            partial_data: partial,
            etw_drops_total: u64::from(partial),
            presentmon_restarts: 0,
            summary: SessionSummary {
                duration_secs: 180,
                frame_time_p50_us_baseline: Some(base_p50),
                frame_time_p50_us_with_rules: Some(base_p50),
                frame_time_p99_us_baseline: Some(base_p99),
                frame_time_p99_us_with_rules: Some(rules_p99),
                actions_applied: 1,
                kernel_signals: 0,
                frames_dropped: 0,
            },
        });
        events
    }

    fn headline_of(attr: Attribution) -> String {
        match attr {
            Attribution::Computed(s) => s.headline,
            other => panic!("expected Computed attribution; got {other:?}"),
        }
    }

    // ─── C-005 — the five honesty-contract tests, verbatim substrings
    //     per architecture §2.4. Band boundaries are policy decisions;
    //     these tests prove they survive refactors. ─────────────────

    #[test]
    fn honesty_contract_minus_9_pct_is_improved() {
        let h = headline_of(compute_attribution_summary(&session_with_p99_delta(-9.0)));
        assert!(
            h.contains("improved your 1% lows"),
            "-9% must render the improved headline; got {h:?}"
        );
    }

    #[test]
    fn honesty_contract_minus_6_pct_is_modest_improvement() {
        let h = headline_of(compute_attribution_summary(&session_with_p99_delta(-6.0)));
        assert!(
            h.contains("Modest improvement"),
            "-6% must render the modest-improvement headline; got {h:?}"
        );
    }

    #[test]
    fn honesty_contract_zero_pct_is_no_measurable_effect() {
        let h = headline_of(compute_attribution_summary(&session_with_p99_delta(0.0)));
        assert!(
            h.contains("No measurable effect"),
            "0% must render the no-effect headline; got {h:?}"
        );
    }

    #[test]
    fn honesty_contract_plus_4_pct_is_slight_regression() {
        let h = headline_of(compute_attribution_summary(&session_with_p99_delta(4.0)));
        assert!(
            h.contains("Slight regression"),
            "+4% must render the slight-regression headline; got {h:?}"
        );
    }

    #[test]
    fn honesty_contract_plus_6_pct_is_degraded_verbatim() {
        let h = headline_of(compute_attribution_summary(&session_with_p99_delta(6.0)));
        assert!(
            h.contains("**degraded**"),
            "+6% must render the degraded banner with the verbatim marker; got {h:?}"
        );
        assert!(h.contains("Review this profile."));
    }

    // ─── Asymmetry boundaries — the load-bearing shape ──────────────

    #[test]
    fn bands_are_asymmetric_slow_to_credit_quick_to_admit_harm() {
        assert_eq!(DeltaBand::classify(-8.5), DeltaBand::Improved);
        assert_eq!(DeltaBand::classify(-8.0), DeltaBand::ModestImprovement);
        assert_eq!(DeltaBand::classify(-3.5), DeltaBand::ModestImprovement);
        assert_eq!(DeltaBand::classify(-3.0), DeltaBand::NoEffect);
        assert_eq!(DeltaBand::classify(3.0), DeltaBand::NoEffect);
        assert_eq!(DeltaBand::classify(3.5), DeltaBand::SlightRegression);
        assert_eq!(DeltaBand::classify(5.0), DeltaBand::SlightRegression);
        assert_eq!(DeltaBand::classify(5.5), DeltaBand::Degraded);
        // The asymmetry itself: +5.5% is already a banner; -5.5% is
        // still only "modest".
        assert_eq!(DeltaBand::classify(-5.5), DeltaBand::ModestImprovement);
    }

    // ─── PRE-L-003 — disabled-attribution cases ─────────────────────

    #[test]
    fn disabled_when_no_rule_fired() {
        let mut events = session_with_p99_delta(0.0);
        events.retain(|e| !matches!(e, SessionEvent::FramesageAction { .. }));
        let attr = compute_attribution_summary(&events);
        assert!(matches!(
            attr,
            Attribution::Disabled {
                reason: DisabledReason::NoRuleFired,
                ..
            }
        ));
    }

    #[test]
    fn disabled_when_baseline_too_short() {
        // Apply fires at 10 s — baseline truncated to 10 s < 30 s.
        let mut events = session_with_p99_delta(0.0);
        for e in events.iter_mut() {
            if let SessionEvent::FramesageAction { at_ms, .. } = e {
                *at_ms = 10_000;
            }
        }
        let attr = compute_attribution_summary(&events);
        assert!(
            matches!(
                attr,
                Attribution::Disabled {
                    reason: DisabledReason::BaselineTooShort,
                    ..
                }
            ),
            "got {attr:?}"
        );
        assert_eq!(
            DisabledReason::BaselineTooShort.message(),
            "Baseline too short"
        );
    }

    #[test]
    fn disabled_when_with_rules_window_too_short() {
        // End the session 30 s after apply — with-rules < 60 s.
        let mut events = session_with_p99_delta(0.0);
        events.retain(|e| e.at_ms() <= 90_000 || matches!(e, SessionEvent::SessionEnd { .. }));
        for e in events.iter_mut() {
            if let SessionEvent::SessionEnd { at_ms, .. } = e {
                *at_ms = 90_000;
            }
        }
        let attr = compute_attribution_summary(&events);
        assert!(
            matches!(
                attr,
                Attribution::Disabled {
                    reason: DisabledReason::SessionTooShort,
                    ..
                }
            ),
            "got {attr:?}"
        );
        assert_eq!(
            DisabledReason::SessionTooShort.message(),
            "Session too short for attribution"
        );
    }

    #[test]
    fn disabled_when_frame_data_unavailable() {
        let mut events = session_with_p99_delta(0.0);
        events.retain(|e| !matches!(e, SessionEvent::FrameSample { .. }));
        let attr = compute_attribution_summary(&events);
        assert!(
            matches!(
                attr,
                Attribution::Disabled {
                    reason: DisabledReason::FrameDataUnavailable,
                    ..
                }
            ),
            "got {attr:?}"
        );
    }

    #[test]
    fn partial_data_disables_but_carries_computed_summary_for_opt_in() {
        let attr = compute_attribution_summary(&session_with_p99_delta_opts(-9.0, true));
        match attr {
            Attribution::Disabled {
                reason: DisabledReason::PartialData,
                computed_anyway: Some(summary),
            } => {
                assert!(summary.headline.contains("improved your 1% lows"));
            }
            other => panic!("expected PartialData with computed_anyway; got {other:?}"),
        }
    }

    // Cherry-picking defense: baseline mechanically ends at the first
    // windowing action even when later actions exist.
    #[test]
    fn baseline_ends_at_first_windowing_action() {
        let mut events = session_with_p99_delta(-9.0);
        events.push(SessionEvent::FramesageAction {
            schema_version: SCHEMA_VERSION,
            at_ms: 120_000,
            action: "game_mode_entered".into(),
            profile_id: "game-x3d".into(),
            details: None,
        });
        match compute_attribution_summary(&events) {
            Attribution::Computed(s) => {
                assert_eq!(s.baseline_window_ms, (0, 60_000));
                assert_eq!(s.with_rules_window_ms.0, 60_000);
            }
            other => panic!("expected Computed; got {other:?}"),
        }
    }
}
