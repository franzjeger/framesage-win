//! Sessions tab — v0.7 closed-loop session-history surface.
//!
//! W1.6 / closes F-002 (Phase 3 roadmap item #85).
//!
//! **#110 slice 3:** on top of the W1.6 empty-state scaffold, this
//! now renders the list view (rows from `Request::ListSessions`) and
//! the detail pane with the §2.4 honest-attribution panel (events
//! from `Request::ReadSession`, verdict via
//! `framesage_recorder::compute_attribution_summary`). Fetching is
//! the caller's job — `render` returns a [`SessionsAction`] and
//! main.rs spawns the IPC thread; this module stays IO-free and
//! testable. Still deferred: the frame-time timeline chart and
//! per-core heatmap (§2.4 detail view, Group D polish).
//!
//! The load-bearing v0.7 contract is the **substring contents** of
//! the two empty-state messages (Group C acceptance criterion at
//! `audit/v0.7-architecture.md:976-985` + `:1030-1034`):
//!
//! - Unsupported-build (build < 26100): MUST contain "requires
//!   Windows 11 24H2 or later" AND MUST NOT contain "After your
//!   first 90-second gaming session".
//! - No-sessions-yet (build ≥ 26100): MUST contain "After your
//!   first 90-second gaming session".
//!
//! Both substrings are pinned by inline tests below. A future
//! copy-edit that drops either substring breaks the test. This is
//! by design — the honesty-of-empty-state framing IS the v0.7
//! commitment to users running on Win10/Win11-23H2, who would
//! otherwise see "No sessions yet" framing that lies about WHY
//! the tab is empty.
//!
//! **Build-gate source:** `StatusSnapshot.closed_loop_build_supported`,
//! populated by the service from `framesage_etw::closed_loop_enabled_for_this_build()`
//! per the W1.6 design (architecture-invariant-#8-respecting
//! Option α — see PR description for the alternatives considered).
//! The tray crate does NOT depend on `framesage-etw` directly.

use eframe::egui;
use framesage_ipc::framesage_recorder::{
    compute_attribution_summary, Attribution, DeltaBand, SessionEvent, SessionListEntry,
};
use framesage_ipc::StatusSnapshot;

use crate::state::SessionDetailState;
use crate::theme;

/// What the caller (main.rs) should do after this frame. The render
/// fn never does IPC itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionsAction {
    /// Fetch / refresh the session list (`Request::ListSessions`).
    RefreshList,
    /// Fetch one session's events (`Request::ReadSession`).
    OpenDetail(String),
    /// #5 — open the closed-loop opt-in dialog (the onboarding
    /// page's EDR-implications disclosure) before flipping the
    /// policy toggle.
    OpenClosedLoopOptIn,
}

// ─── Load-bearing string constants — DO NOT EDIT WITHOUT UPDATING TESTS ──
//
// These two strings carry the Group C acceptance criterion's required
// substrings verbatim. The inline tests below assert that the constants
// contain those substrings. A change here that drops a substring will
// fail tests; a change that adds substrings is OK provided no required
// substring is removed.

/// First line of the unsupported-build empty state. MUST contain
/// the substring `"requires Windows 11 24H2 or later"` verbatim
/// (Group C acceptance criterion).
pub const UNSUPPORTED_BUILD_HEADING: &str =
    "Closed-loop measurement requires Windows 11 24H2 or later.";

/// Body of the unsupported-build empty state. Explains static-rule
/// mode is still active (so the user understands the rest of
/// FrameSage is still working).
pub const UNSUPPORTED_BUILD_BODY: &str =
    "FrameSage is running in static-rule mode — rules still fire, \
     but FrameSage can't measure whether they helped on this Windows build. \
     To enable session recording and the \"Did it help?\" attribution UI, \
     upgrade to Windows 11 24H2 (build 26100) or later.";

/// Headline of the no-sessions-yet empty state. MUST contain the
/// substring `"After your first 90-second gaming session"` verbatim
/// (Group C acceptance criterion).
pub const NO_SESSIONS_BODY: &str =
    "FrameSage measures whether its rules actually helped your games. \
     After your first 90-second gaming session, this tab will show \
     frame-time and CPU history per session, which profile FrameSage \
     applied and when, and the honest answer to \"Did 1% lows improve?\" — \
     including when they didn't.";

/// Inline "Why this requirement?" help copy. MUST stay verbatim-
/// aligned with README's "System requirements (closed-loop
/// measurement)" section — the Group C acceptance criterion at
/// architecture §2.4 — pinned by the include_str! test below.
pub const WHY_REQUIREMENT_BODY: &str =
    "ETW kernel-event schemas are stable on builds we've empirically validated, \
     and v0.7 ships with empirical validation only on Win11 24H2. Older builds \
     may or may not work, and v0.7 won't claim measurement results it can't \
     substantiate.";

/// Privacy footer shared by both no-sessions-yet sub-states.
pub const NO_SESSIONS_PRIVACY_FOOTER: &str =
    "Once enabled, just play. Sessions record automatically when a rule fires. \
     Recorded data is local-only — nothing leaves your machine.";

// ─── Render entry point ──────────────────────────────────────────────────

/// Render one frame of the Sessions tab. Caller passes the latest
/// `StatusSnapshot` from the engine (always present once IPC has
/// connected at least once).
///
/// Decision tree (per architecture §2.4 table at :922-926):
///
/// | `closed_loop_build_supported` | `policy.closed_loop_enabled` | Render |
/// |---|---|---|
/// | false | (irrelevant — Settings hides the toggle) | unsupported-build |
/// | true | false | no-sessions-yet w/ Enable affordance |
/// | true | true | no-sessions-yet w/ "ON · waiting for first session" |
///
/// In v0.7 there is no "sessions exist" branch — the recorder isn't
/// wired yet (M3.1 deliverable). The third row of the table renders
/// the same empty-state framing, just with the toggle state flipped.
pub fn render(
    ui: &mut egui::Ui,
    status: Option<&StatusSnapshot>,
    sessions: Option<&[SessionListEntry]>,
    detail: Option<&mut SessionDetailState>,
    fetch_pending: bool,
) -> Option<SessionsAction> {
    let Some(snap) = status else {
        // No status yet (IPC hasn't connected). Render a neutral
        // placeholder; do NOT default to either empty state because
        // the build-gate field would be `false` by Default but that
        // doesn't reflect actual unsupported-ness.
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("Connecting to FrameSage service…").color(theme::TEXT_MUTED),
            );
        });
        return None;
    };

    if !snap.closed_loop_build_supported {
        render_unsupported_build(ui);
        return None;
    }

    match sessions {
        // Not fetched yet — kick off the first fetch, show a
        // lightweight placeholder meanwhile.
        None => {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("Loading sessions…").color(theme::TEXT_MUTED));
            });
            if fetch_pending {
                None
            } else {
                Some(SessionsAction::RefreshList)
            }
        }
        // Fetched, nothing recorded — the load-bearing §2.4 empty
        // states, unchanged from the W1.6 scaffold.
        Some([]) => {
            let mut action = None;
            render_no_sessions_yet(ui, snap.policy.closed_loop_enabled, &mut action);
            action
        }
        Some(list) => render_list_and_detail(ui, list, detail, fetch_pending),
    }
}

/// §2.4 list view (top) + detail pane (bottom).
fn render_list_and_detail(
    ui: &mut egui::Ui,
    list: &[SessionListEntry],
    detail: Option<&mut SessionDetailState>,
    fetch_pending: bool,
) -> Option<SessionsAction> {
    let mut action = None;

    let total_bytes: u64 = list.iter().map(|e| e.file_bytes).sum();
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "📁 {} session{} stored · {:.0} MB · cap 1 GB",
                list.len(),
                if list.len() == 1 { "" } else { "s" },
                total_bytes as f64 / (1024.0 * 1024.0)
            ))
            .color(theme::TEXT_MUTED),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add_enabled(!fetch_pending, egui::Button::new("Refresh"))
                .clicked()
            {
                action = Some(SessionsAction::RefreshList);
            }
        });
    });
    ui.separator();

    let selected_id = detail.as_ref().map(|d| d.session_id.clone());
    egui::ScrollArea::vertical()
        .id_source("sessions-list")
        .max_height(ui.available_height() * 0.45)
        .show(ui, |ui| {
            for entry in list {
                let is_selected = selected_id.as_deref() == Some(entry.session_id.as_str());
                let dur = match entry.duration_secs {
                    Some(secs) => format!("{}m{:02}s", secs / 60, secs % 60),
                    None => "in progress / crashed".to_string(),
                };
                let mut label = format!("🎮 {} · {} · {}", entry.game_exe, entry.profile_id, dur);
                if entry.partial_data {
                    label.push_str("  ⚠ partial data");
                }
                let resp = ui.selectable_label(is_selected, label);
                if resp.clicked() && !is_selected {
                    action = Some(SessionsAction::OpenDetail(entry.session_id.clone()));
                }
            }
        });

    ui.separator();
    match detail {
        Some(d) => render_detail(ui, d),
        None => {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(if fetch_pending {
                    "Loading session…"
                } else {
                    "Select a session to see the \"Did it help?\" attribution."
                })
                .color(theme::TEXT_MUTED),
            );
        }
    }
    action
}

/// Detail pane: event summary + the §2.4 honest-attribution panel.
/// Charts (frame-time timeline, per-core heatmap) are Group D polish
/// and intentionally absent from this slice.
fn render_detail(ui: &mut egui::Ui, detail: &mut SessionDetailState) {
    ui.label(
        egui::RichText::new(format!("Session {}", detail.session_id))
            .strong()
            .size(14.0),
    );
    let frame_samples = detail
        .events
        .iter()
        .filter(|e| matches!(e, SessionEvent::FrameSample { .. }))
        .count();
    let actions = detail
        .events
        .iter()
        .filter(|e| matches!(e, SessionEvent::FramesageAction { .. }))
        .count();
    ui.label(
        egui::RichText::new(format!(
            "{} events · {} frame samples · {} actions{}",
            detail.events.len(),
            frame_samples,
            actions,
            if detail.skipped_lines > 0 {
                format!(" · {} malformed lines skipped", detail.skipped_lines)
            } else {
                String::new()
            }
        ))
        .size(12.0)
        .color(theme::TEXT_MUTED),
    );
    ui.add_space(8.0);

    // #3 — frame-time timeline (p50 line, p99 shaded) with the
    // Game Mode enter marker. Drawn from the session's frame_sample
    // events; absent-frame-data sessions show a hint instead.
    render_frame_time_chart(ui, &detail.events);
    // #1 — per-core CPU heatmap from the session's cpu_sample events.
    render_cpu_heatmap(ui, &detail.events);
    ui.add_space(8.0);

    ui.label(
        egui::RichText::new("\"Did FrameSage help?\" attribution")
            .strong()
            .size(13.0),
    );
    ui.add_space(4.0);
    match compute_attribution_summary(&detail.events) {
        Attribution::Computed(summary) => render_attribution_summary(ui, &summary),
        Attribution::Disabled {
            reason,
            computed_anyway,
        } => {
            ui.label(
                egui::RichText::new(format!("Attribution disabled: {}", reason.message()))
                    .color(theme::WARNING),
            );
            if let Some(summary) = computed_anyway {
                ui.checkbox(
                    &mut detail.show_partial_anyway,
                    "Show anyway (partial data — treat with caution)",
                );
                if detail.show_partial_anyway {
                    render_attribution_summary(ui, &summary);
                }
            }
        }
    }
}

fn render_attribution_summary(
    ui: &mut egui::Ui,
    summary: &framesage_ipc::framesage_recorder::AttributionSummary,
) {
    // §2.4 band colors: green only above the conservative +8% claim
    // threshold; the degraded banner is loud by design.
    let color = match summary.band {
        DeltaBand::Improved => theme::SUCCESS,
        DeltaBand::ModestImprovement | DeltaBand::NoEffect => theme::TEXT_MUTED,
        DeltaBand::SlightRegression | DeltaBand::Degraded => theme::WARNING,
    };
    // The stored headline carries the **degraded** emphasis marker
    // asserted by the honesty-contract tests; render it as bold text
    // without the literal asterisks.
    let display = summary.headline.replace("**", "");
    let mut text = egui::RichText::new(display).color(color).size(13.0);
    if matches!(summary.band, DeltaBand::Degraded | DeltaBand::Improved) {
        text = text.strong();
    }
    ui.label(text);
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(format!(
            "1% lows: {:+.1}%   ·   avg frame time: {:+.1}%   ·   variance: {:+.1}%",
            summary.p99_delta_pct, summary.avg_frame_time_delta_pct, summary.variance_delta_pct
        ))
        .size(12.0)
        .color(theme::TEXT_MUTED),
    );
    ui.label(
        egui::RichText::new(format!(
            "baseline {}s–{}s · with rules {}s–{}s",
            summary.baseline_window_ms.0 / 1000,
            summary.baseline_window_ms.1 / 1000,
            summary.with_rules_window_ms.0 / 1000,
            summary.with_rules_window_ms.1 / 1000
        ))
        .size(11.0)
        .color(theme::TEXT_MUTED),
    );
}

fn render_unsupported_build(ui: &mut egui::Ui) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.set_max_width(560.0);

        ui.label(egui::RichText::new("🔒").size(48.0));
        ui.add_space(12.0);

        ui.label(
            egui::RichText::new(UNSUPPORTED_BUILD_HEADING)
                .strong()
                .size(16.0),
        );
        ui.add_space(8.0);

        ui.label(
            egui::RichText::new(UNSUPPORTED_BUILD_BODY)
                .size(13.0)
                .color(theme::TEXT_MUTED),
        );
        ui.add_space(16.0);

        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .button("Open Windows Update")
                        .on_hover_text("Launches ms-settings:windowsupdate — no automatic install.")
                        .clicked()
                    {
                        // Documented URI handler for the Windows
                        // Update Settings pane. No background
                        // check, no auto-install — the user lands
                        // on the pane and decides.
                        crate::open_in_shell("ms-settings:windowsupdate");
                    }
                });
            });
        });
        ui.add_space(10.0);
        // #4 — inline help panel per §2.4: reproduces the README
        // "System requirements" rationale verbatim.
        egui::CollapsingHeader::new("Why this requirement?")
            .id_source("why-requirement")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(WHY_REQUIREMENT_BODY)
                        .size(12.0)
                        .color(theme::TEXT_MUTED),
                );
            });
    });
}

fn render_no_sessions_yet(
    ui: &mut egui::Ui,
    closed_loop_enabled: bool,
    action: &mut Option<SessionsAction>,
) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.set_max_width(560.0);

        ui.label(egui::RichText::new("📊").size(48.0));
        ui.add_space(12.0);

        ui.label(
            egui::RichText::new("No sessions recorded yet.")
                .strong()
                .size(16.0),
        );
        ui.add_space(8.0);

        ui.label(
            egui::RichText::new(NO_SESSIONS_BODY)
                .size(13.0)
                .color(theme::TEXT_MUTED),
        );
        ui.add_space(16.0);

        // Status row — flips between OFF/[Enable…] and ON·waiting.
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Closed-loop measurement is currently:").size(13.0),
                    );
                    ui.add_space(6.0);
                    if closed_loop_enabled {
                        ui.label(
                            egui::RichText::new("ON · waiting for first session")
                                .strong()
                                .color(theme::SUCCESS),
                        );
                    } else {
                        ui.label(egui::RichText::new("OFF").strong().color(theme::TEXT_MUTED));
                        ui.add_space(8.0);
                        if ui
                            .button("Enable…")
                            .on_hover_text(
                                "Opens the closed-loop opt-in with the EDR-implications \
                                 disclosure before flipping the toggle.",
                            )
                            .clicked()
                        {
                            *action = Some(SessionsAction::OpenClosedLoopOptIn);
                        }
                    }
                });
            });
        });

        ui.add_space(12.0);
        ui.label(
            egui::RichText::new(NO_SESSIONS_PRIVACY_FOOTER)
                .size(12.0)
                .color(theme::TEXT_MUTED),
        );
    });
}

/// #3 — frame-time timeline: p99 as a shaded band, p50 as a line,
/// with a vertical marker at the first apply/enter action. µs → ms on
/// the axis. No frame samples → an honest "no frame data" hint (the
/// session was recorded without PresentMon).
fn render_frame_time_chart(ui: &mut egui::Ui, events: &[SessionEvent]) {
    let samples: Vec<(u64, f32, f32)> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::FrameSample {
                at_ms,
                frame_time_us_p50,
                frame_time_us_p99,
                ..
            } => Some((
                *at_ms,
                *frame_time_us_p50 as f32 / 1000.0,
                *frame_time_us_p99 as f32 / 1000.0,
            )),
            _ => None,
        })
        .collect();

    ui.label(egui::RichText::new("Frame time (ms)").size(12.0).strong());
    if samples.len() < 2 {
        ui.label(
            egui::RichText::new("no frame data recorded for this session")
                .size(11.0)
                .color(theme::TEXT_MUTED),
        );
        return;
    }

    let apply_ms = events.iter().find_map(|e| match e {
        SessionEvent::FramesageAction { at_ms, action, .. }
            if action == "apply_profile" || action == "game_mode_entered" =>
        {
            Some(*at_ms)
        }
        _ => None,
    });

    let (min_t, max_t) = (samples[0].0, samples[samples.len() - 1].0);
    let span_t = (max_t - min_t).max(1) as f32;
    let max_ms = samples.iter().map(|s| s.2).fold(1.0_f32, f32::max).max(1.0);

    let (rect, _resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 90.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let x = |t: u64| rect.left() + (t - min_t) as f32 / span_t * rect.width();
    let y = |ms: f32| rect.bottom() - (ms / max_ms) * rect.height();

    painter.rect_filled(rect, 2.0, theme::SURFACE);
    // p99 shaded band (baseline 0 → p99).
    for w in samples.windows(2) {
        let (a, b) = (w[0], w[1]);
        let poly = vec![
            egui::pos2(x(a.0), y(a.2)),
            egui::pos2(x(b.0), y(b.2)),
            egui::pos2(x(b.0), rect.bottom()),
            egui::pos2(x(a.0), rect.bottom()),
        ];
        painter.add(egui::Shape::convex_polygon(
            poly,
            theme::ACCENT.gamma_multiply(0.18),
            egui::Stroke::NONE,
        ));
    }
    // p50 line.
    for w in samples.windows(2) {
        painter.line_segment(
            [
                egui::pos2(x(w[0].0), y(w[0].1)),
                egui::pos2(x(w[1].0), y(w[1].1)),
            ],
            egui::Stroke::new(1.5_f32, theme::ACCENT),
        );
    }
    // Game Mode enter marker.
    if let Some(t) = apply_ms {
        if t >= min_t && t <= max_t {
            painter.line_segment(
                [
                    egui::pos2(x(t), rect.top()),
                    egui::pos2(x(t), rect.bottom()),
                ],
                egui::Stroke::new(1.0_f32, theme::WARNING),
            );
        }
    }
    ui.label(
        egui::RichText::new(format!(
            "p50 line · p99 shaded · peak {max_ms:.1} ms · orange = Game Mode entered"
        ))
        .size(10.0)
        .color(theme::TEXT_MUTED),
    );
}

/// #1 — per-core CPU heatmap: one row per logical CPU, time on x,
/// cell brightness = utilisation. Drawn from cpu_sample events.
fn render_cpu_heatmap(ui: &mut egui::Ui, events: &[SessionEvent]) {
    let samples: Vec<&Vec<u8>> = events
        .iter()
        .filter_map(|e| match e {
            SessionEvent::CpuSample { per_core_pct, .. } if !per_core_pct.is_empty() => {
                Some(per_core_pct)
            }
            _ => None,
        })
        .collect();
    if samples.is_empty() {
        return;
    }
    let cores = samples.iter().map(|s| s.len()).max().unwrap_or(0);
    if cores == 0 {
        return;
    }
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(format!("Per-core CPU ({cores} cores)"))
            .size(12.0)
            .strong(),
    );
    let row_h = 6.0_f32;
    let (rect, _resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h * cores as f32),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::SURFACE);
    let cell_w = rect.width() / samples.len() as f32;
    for (col, sample) in samples.iter().enumerate() {
        for core in 0..cores {
            let pct = sample.get(core).copied().unwrap_or(0) as f32 / 100.0;
            // Blue→green→yellow→red-ish via accent gamma; cheap and
            // theme-consistent (a full perceptual colormap is the
            // Group D polish item).
            let color = theme::ACCENT.gamma_multiply(0.15 + pct * 0.85);
            let cell = egui::Rect::from_min_size(
                egui::pos2(
                    rect.left() + col as f32 * cell_w,
                    rect.top() + core as f32 * row_h,
                ),
                egui::vec2(cell_w.ceil(), row_h),
            );
            painter.rect_filled(cell, 0.0, color);
        }
    }
}

// ─── Acceptance-criterion substring tests ────────────────────────────────
//
// Group C acceptance criterion at `audit/v0.7-architecture.md:976-985`
// requires the unsupported-build empty state to contain a specific
// substring AND to NOT contain another. The full criterion describes an
// integration test that mocks RtlGetVersion + boots the UI; the W1.6
// scaffold deliverable here pins the substrings at the source-string
// level — the heavier integration test lands with the full Sessions tab
// in M3.1 (#110). The substring-presence guarantee transfers from
// `const &str` to the rendered UI because the render functions pass the
// constants directly to `egui::RichText::new(...)`; a future refactor
// that paraphrases the rendered copy MUST also update the constants,
// at which point these tests catch the drift.

#[cfg(test)]
mod tests {
    use super::*;

    /// W1.6 acceptance criterion #1 — unsupported-build empty state.
    #[test]
    fn unsupported_build_heading_contains_required_substring() {
        assert!(
            UNSUPPORTED_BUILD_HEADING.contains("requires Windows 11 24H2 or later"),
            "UNSUPPORTED_BUILD_HEADING must contain the Group C \
             acceptance criterion substring verbatim — got: {UNSUPPORTED_BUILD_HEADING:?}",
        );
    }

    /// W1.6 acceptance criterion #1 negative half — unsupported-build
    /// empty state MUST NOT contain the no-sessions-yet framing. This
    /// is the load-bearing honesty constraint: the user on Win10 22H2
    /// can't fix the tab being empty by playing a game.
    #[test]
    fn unsupported_build_does_not_contain_no_sessions_framing() {
        // Concatenate both unsupported-build strings so the test sees
        // the full body the user reads.
        let combined = format!("{UNSUPPORTED_BUILD_HEADING} {UNSUPPORTED_BUILD_BODY}");
        assert!(
            !combined.contains("After your first 90-second gaming session"),
            "unsupported-build empty state must NOT contain the no-sessions-yet \
             framing — concatenated source: {combined:?}",
        );
    }

    /// W1.6 acceptance criterion #2 — no-sessions-yet empty state.
    #[test]
    fn no_sessions_body_contains_required_substring() {
        assert!(
            NO_SESSIONS_BODY.contains("After your first 90-second gaming session"),
            "NO_SESSIONS_BODY must contain the Group C acceptance \
             criterion substring verbatim — got: {NO_SESSIONS_BODY:?}",
        );
    }

    /// #4 / Group C acceptance criterion — the inline help panel's
    /// copy must match the README "System requirements" rationale
    /// verbatim. include_str! makes the alignment a compile-coupled
    /// source-level check: edit either side without the other and
    /// this fails.
    #[test]
    fn why_requirement_panel_matches_readme_verbatim() {
        let readme = include_str!("../../../../README.md");
        // The README wraps at different points; compare on
        // whitespace-normalized text so only wording drift fails.
        let normalize = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalize(readme).contains(&normalize(WHY_REQUIREMENT_BODY)),
            "README 'System requirements' rationale must contain the panel copy verbatim"
        );
    }

    /// Sanity check: the privacy footer says "local-only — nothing
    /// leaves your machine" per the architecture §2.4 spec at
    /// :1014-1016. Not a Group C BLOCKER but pinning the privacy
    /// commitment at source-level catches drift.
    #[test]
    fn no_sessions_privacy_footer_contains_local_only_commitment() {
        assert!(
            NO_SESSIONS_PRIVACY_FOOTER.contains("nothing leaves your machine"),
            "NO_SESSIONS_PRIVACY_FOOTER must surface the local-only \
             privacy commitment — got: {NO_SESSIONS_PRIVACY_FOOTER:?}",
        );
    }
}
