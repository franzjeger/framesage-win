//! Cross-session attribution rollup — the "does this profile help *in
//! general*?" answer, per `audit/v0.7-architecture.md` §2.4's intent
//! that a single session is noisy but a trend is trustworthy.
//!
//! A single [`crate::compute_attribution_summary`] verdict is one
//! noisy sample: frame-time varies run to run. This module groups a
//! user's sessions by `(game_exe, profile_id)` and reports the **median
//! 1% lows delta** across the sessions that were actually attributable.
//!
//! Honesty rules, deliberately strict:
//!
//! * Only [`Attribution::Computed`] verdicts feed the aggregate. A
//!   `Disabled` verdict — including a `PartialData` one carrying a
//!   `computed_anyway` summary — is **excluded**: the per-session code
//!   already decided that number isn't trustworthy, and averaging
//!   untrustworthy numbers doesn't launder them into a trustworthy one.
//! * The rollup always reports `attributable_sessions` out of
//!   `total_sessions`, so the UI can say "6 of 9 sessions had usable
//!   data" rather than implying all nine agreed.
//! * The band is classified from the **median**, not the mean — one
//!   pathological session can't drag the verdict across a boundary.

use crate::attribution::{Attribution, DeltaBand};

/// One `(game, profile)` rollup across a user's sessions.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AggregateAttribution {
    pub game_exe: String,
    pub profile_id: String,
    /// Sessions for this pair that produced a trustworthy `Computed`
    /// verdict — the ones the median is taken over.
    pub attributable_sessions: usize,
    /// All sessions seen for this pair (attributable or not). Always
    /// `>= attributable_sessions`; the difference is sessions that were
    /// too short, had no frame data, partial data, etc.
    pub total_sessions: usize,
    /// Median of the per-session 1% lows deltas (percent; negative =
    /// improvement). Present only when `attributable_sessions > 0`.
    pub median_p99_delta_pct: Option<f64>,
    /// Band classified from the median. `None` when there's nothing
    /// attributable to classify.
    pub band: Option<DeltaBand>,
    /// One-line rollup headline for the UI. Never fabricates a verdict:
    /// with no attributable sessions it states exactly that.
    pub headline: String,
}

/// Input row: one session's game/profile plus its per-session
/// attribution result (from [`crate::compute_attribution_summary`]).
/// The caller supplies these by reading each session file; keeping the
/// aggregate pure over this shape makes it fully testable off-host.
pub struct SessionAttribution {
    pub game_exe: String,
    pub profile_id: String,
    pub attribution: Attribution,
}

/// Group sessions by `(game_exe, profile_id)` and roll each group up
/// into an [`AggregateAttribution`]. Groups are returned sorted by
/// `total_sessions` descending, then game/profile name, so the most-
/// played pairs surface first and the order is deterministic.
pub fn compute_aggregates(sessions: &[SessionAttribution]) -> Vec<AggregateAttribution> {
    use std::collections::BTreeMap;

    // BTreeMap keeps a stable, name-sorted grouping without needing a
    // clock or hasher (the workflow/runtime forbids Random anyway).
    let mut groups: BTreeMap<(String, String), (usize, Vec<f64>)> = BTreeMap::new();
    for s in sessions {
        let entry = groups
            .entry((s.game_exe.clone(), s.profile_id.clone()))
            .or_insert((0, Vec::new()));
        entry.0 += 1; // total
        if let Attribution::Computed(summary) = &s.attribution {
            entry.1.push(summary.p99_delta_pct);
        }
    }

    let mut out: Vec<AggregateAttribution> = groups
        .into_iter()
        .map(|((game_exe, profile_id), (total, mut deltas))| {
            let attributable = deltas.len();
            let (median, band, headline) = if attributable == 0 {
                (
                    None,
                    None,
                    format!(
                        "No attributable sessions yet for {game_exe} · {profile_id} \
                         ({total} recorded, none with usable frame data)"
                    ),
                )
            } else {
                let m = median(&mut deltas);
                let b = DeltaBand::classify(m);
                (
                    Some(m),
                    Some(b),
                    aggregate_headline(b, m, attributable, total),
                )
            };
            AggregateAttribution {
                game_exe,
                profile_id,
                attributable_sessions: attributable,
                total_sessions: total,
                median_p99_delta_pct: median,
                band,
                headline,
            }
        })
        .collect();

    out.sort_by(|a, b| {
        b.total_sessions
            .cmp(&a.total_sessions)
            .then_with(|| a.game_exe.cmp(&b.game_exe))
            .then_with(|| a.profile_id.cmp(&b.profile_id))
    });
    out
}

/// Median of a delta set. Sorts in place; averages the two middles on
/// an even count. Caller guarantees non-empty.
fn median(deltas: &mut [f64]) -> f64 {
    deltas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = deltas.len();
    if n % 2 == 1 {
        deltas[n / 2]
    } else {
        (deltas[n / 2 - 1] + deltas[n / 2]) / 2.0
    }
}

/// Rollup headline. Mirrors the per-session band vocabulary (including
/// the `**degraded**` emphasis marker the UI renders) but framed across
/// the sample so the user reads it as a trend, and always states the
/// attributable/total denominator.
fn aggregate_headline(band: DeltaBand, median: f64, attributable: usize, total: usize) -> String {
    let n = median.abs().round() as i64;
    let scope = format!("across {attributable} of {total} sessions");
    match band {
        DeltaBand::Improved => {
            format!("Consistently improves your 1% lows by ~{n}% ({scope})")
        }
        DeltaBand::ModestImprovement => {
            format!("Modest improvement in 1% lows, ~{n}% ({scope})")
        }
        DeltaBand::NoEffect => format!("No measurable effect on 1% lows ({scope})"),
        DeltaBand::SlightRegression => {
            format!("Slight regression in 1% lows, ~{n}% ({scope})")
        }
        DeltaBand::Degraded => format!(
            "This profile **degraded** your 1% lows by ~{n}% ({scope}). Review this profile."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::{AttributionSummary, DisabledReason};

    fn computed(p99: f64) -> Attribution {
        Attribution::Computed(Box::new(AttributionSummary {
            avg_frame_time_delta_pct: p99,
            p99_delta_pct: p99,
            variance_delta_pct: 0.0,
            band: DeltaBand::classify(p99),
            headline: String::new(),
            baseline_window_ms: (0, 60_000),
            with_rules_window_ms: (60_000, 180_000),
        }))
    }

    fn disabled_partial(p99: f64) -> Attribution {
        // A partial-data verdict that DOES carry a computed_anyway —
        // the aggregate must still exclude it.
        Attribution::Disabled {
            reason: DisabledReason::PartialData,
            computed_anyway: Some(Box::new(AttributionSummary {
                avg_frame_time_delta_pct: p99,
                p99_delta_pct: p99,
                variance_delta_pct: 0.0,
                band: DeltaBand::classify(p99),
                headline: String::new(),
                baseline_window_ms: (0, 60_000),
                with_rules_window_ms: (60_000, 180_000),
            })),
        }
    }

    fn row(game: &str, profile: &str, attribution: Attribution) -> SessionAttribution {
        SessionAttribution {
            game_exe: game.into(),
            profile_id: profile.into(),
            attribution,
        }
    }

    #[test]
    fn median_of_odd_and_even_counts() {
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
    }

    #[test]
    fn groups_by_game_and_profile_and_takes_the_median() {
        let rows = vec![
            row("g.exe", "x3d", computed(-10.0)),
            row("g.exe", "x3d", computed(-6.0)),
            row("g.exe", "x3d", computed(-2.0)),
            row("other.exe", "x3d", computed(1.0)),
        ];
        let agg = compute_aggregates(&rows);
        assert_eq!(agg.len(), 2);
        // Most-played pair (g.exe/x3d, 3 sessions) sorts first.
        let g = &agg[0];
        assert_eq!(
            (g.game_exe.as_str(), g.profile_id.as_str()),
            ("g.exe", "x3d")
        );
        assert_eq!(g.attributable_sessions, 3);
        assert_eq!(g.total_sessions, 3);
        assert_eq!(g.median_p99_delta_pct, Some(-6.0));
        assert_eq!(g.band, Some(DeltaBand::ModestImprovement));
    }

    #[test]
    fn disabled_and_partial_verdicts_are_excluded_but_still_counted() {
        let rows = vec![
            row("g.exe", "x3d", computed(-9.0)),
            row(
                "g.exe",
                "x3d",
                Attribution::Disabled {
                    reason: DisabledReason::SessionTooShort,
                    computed_anyway: None,
                },
            ),
            // Partial-with-computed_anyway must NOT feed the median.
            row("g.exe", "x3d", disabled_partial(50.0)),
        ];
        let agg = compute_aggregates(&rows);
        assert_eq!(agg.len(), 1);
        let g = &agg[0];
        assert_eq!(g.attributable_sessions, 1, "only the Computed one counts");
        assert_eq!(g.total_sessions, 3, "all three are counted as recorded");
        assert_eq!(
            g.median_p99_delta_pct,
            Some(-9.0),
            "the partial 50% outlier must be excluded from the median"
        );
        assert_eq!(g.band, Some(DeltaBand::Improved));
    }

    #[test]
    fn a_group_with_no_attributable_sessions_says_so_without_inventing_a_verdict() {
        let rows = vec![row(
            "g.exe",
            "x3d",
            Attribution::Disabled {
                reason: DisabledReason::FrameDataUnavailable,
                computed_anyway: None,
            },
        )];
        let agg = compute_aggregates(&rows);
        assert_eq!(agg.len(), 1);
        let g = &agg[0];
        assert_eq!(g.attributable_sessions, 0);
        assert_eq!(g.total_sessions, 1);
        assert_eq!(g.median_p99_delta_pct, None);
        assert_eq!(g.band, None);
        assert!(g.headline.contains("No attributable sessions yet"));
    }

    #[test]
    fn degraded_rollup_keeps_the_emphasis_marker_and_denominator() {
        let rows = vec![
            row("g.exe", "x3d", computed(9.0)),
            row("g.exe", "x3d", computed(7.0)),
        ];
        let agg = compute_aggregates(&rows);
        let g = &agg[0];
        assert_eq!(g.band, Some(DeltaBand::Degraded));
        assert!(g.headline.contains("**degraded**"));
        assert!(g.headline.contains("across 2 of 2 sessions"));
    }
}
