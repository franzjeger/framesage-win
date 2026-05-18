//! Sessions tab — v0.7 closed-loop session-history surface.
//!
//! W1.6 / closes F-002 (Phase 3 roadmap item #85).
//!
//! **Scaffold scope (v0.7 ship):** This module renders the two
//! empty-state variants per `audit/v0.7-architecture.md` §2.4. It
//! does NOT yet contain the list view, detail view, attribution
//! panel, or any IPC wiring for `Request::ListSessions` /
//! `Request::ReadSession` — those land in Phase 3 Month 3 M3.1
//! (Group C deliverable, #110).
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
use framesage_ipc::StatusSnapshot;

use crate::theme;

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
pub fn render(ui: &mut egui::Ui, status: Option<&StatusSnapshot>) {
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
        return;
    };

    if !snap.closed_loop_build_supported {
        render_unsupported_build(ui);
    } else {
        render_no_sessions_yet(ui, snap.policy.closed_loop_enabled);
    }
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
                    ui.add_space(8.0);
                    let _ = ui
                        .add_enabled(false, egui::Button::new("Why this requirement?"))
                        .on_hover_text(
                            "Inline help panel lands in v0.7 Group C deliverable (#110).",
                        );
                });
            });
        });
    });
}

fn render_no_sessions_yet(ui: &mut egui::Ui, closed_loop_enabled: bool) {
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
                        let _ = ui
                            .add_enabled(false, egui::Button::new("Enable…"))
                            .on_hover_text(
                                "Re-opens first-run onboarding's closed-loop opt-in page \
                                     so the EDR-implications disclosure is shown before \
                                     flipping the toggle. Full wiring lands in v0.7 Group C \
                                     deliverable (#110).",
                            );
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
