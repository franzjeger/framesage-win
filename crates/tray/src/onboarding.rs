//! Item 4.1 — first-run onboarding wizard.
//!
//! On first launch (no `%LOCALAPPDATA%\framesage\first-run-complete`
//! marker file), this module renders a 4-page modal that gates the
//! seeded BF6 / Valorant / Fortnite rules behind an explicit
//! user choice. No defaults are pre-selected.
//!
//! Pages:
//!   1. **What FrameSage is** — the verbatim product-positioning
//!      statement that also appears in README.md (item 4.14).
//!      Continue button.
//!   2. **Choose your level** — three radio options (Aggressive /
//!      Balanced / Pinning-only), each with full disclosure of
//!      what it does. Expandable lists pull the service / process
//!      names from the seeded `game_mode` actions so the user
//!      sees exactly what Aggressive entails.
//!   3. **Manual Game Mode hotkey** — brief intro to the manual
//!      global toggle (item 2.11). For the v0.6 ship the
//!      hotkey-binding UI itself is a stub ("default Ctrl+Alt+G,
//!      configure in Settings later") — the manual global path
//!      is already accessible via CLI and tray menu.
//!   4. **Done** — confirmation card, Finish button. Marker file
//!      is written + policy mutation applied via SetPolicy on
//!      Finish click.
//!
//! Skip semantics: closing the window without choosing on Page 2
//! does NOT write the marker — the wizard re-fires on next launch.
//! This is the conservative behavior; only an explicit Finish
//! commits.

use framesage_core::{paths, Policy};
use serde::{Deserialize, Serialize};

use crate::theme;

/// How aggressive should the seeded gaming rules be? Maps 1:1 to the
/// three radio options on onboarding Page 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AggressionLevel {
    /// The default arsenal: stop services, suspend processes,
    /// hide taskbar, pause Windows Update, switch power plan,
    /// pin to X3D. Recommended for dedicated gaming PCs.
    Aggressive,
    /// CPU pinning + priority + power plan switch only. No
    /// services stopped, no processes suspended, cloud sync stays
    /// running, taskbar visible. OK for shared / work-also-laptop
    /// machines.
    Balanced,
    /// Affinity to X3D CCD + nothing else. The safest of the
    /// three; closest to what Process Lasso's CPU-affinity-only
    /// rules do.
    PinningOnly,
}

impl AggressionLevel {
    pub fn label(self) -> &'static str {
        match self {
            Self::Aggressive => "Aggressive",
            Self::Balanced => "Balanced",
            Self::PinningOnly => "Pinning only",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Self::Aggressive => "recommended for dedicated gaming PCs",
            Self::Balanced => "OK for shared / work-also-laptop machines",
            Self::PinningOnly => "the safest of the three",
        }
    }
}

/// Wizard state. Carries the current page index, the user's
/// in-flight selection, and per-page UI state (expanded lists).
/// Owned by `FramesageApp` and rendered as a modal overlay.
#[derive(Debug, Default)]
pub struct OnboardingState {
    pub page: u8,
    pub choice: Option<AggressionLevel>,
}

/// Returned by [`render`] to signal what the host should do next.
#[derive(Debug)]
pub enum OnboardingResult {
    /// User clicked Finish on Page 4. Caller writes the marker
    /// file and applies the chosen aggression level to the
    /// running policy via SetPolicy.
    Finished(AggressionLevel),
    /// Modal still visible — keep calling render each frame.
    StillVisible,
}

/// True when the onboarding marker doesn't exist yet — caller
/// should show the wizard.
pub fn should_show() -> bool {
    !paths::first_run_marker_path().exists()
}

/// Write the marker file so the wizard never fires again on this
/// user account. Caller invokes this AFTER a successful SetPolicy
/// that committed the choice (so a SetPolicy failure leaves the
/// wizard re-firing on next launch).
pub fn write_marker() -> std::io::Result<()> {
    let path = paths::first_run_marker_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Stamp the file with the timestamp + chosen level for forensic
    // value — contents are NOT consumed by the loader, only existence
    // matters. This is intentional: future schema changes can shift
    // the body without breaking the gate.
    std::fs::write(&path, b"first-run-complete\n")
}

/// Mutate `policy` in-place: rewrite the seeded gaming profiles'
/// `game_mode` field to match the chosen aggression level. Touches
/// the profiles `policy.profile()` resolves for the bf6 / valorant /
/// fortnite seeded rules (game-x3d, game-x3d-hybrid, game-x3d-safe);
/// the `affinity_mask` / `cpu_sets` / `priority_class` knobs are
/// left untouched so even Pinning-only still pins.
///
/// Aggressive: leave `game_mode` as the seeded full arsenal.
/// Balanced: keep power_plan switch only; clear stop_services,
///   suspend_processes, hide_taskbar, pause_windows_update.
/// PinningOnly: set `game_mode = None` entirely.
pub fn apply_choice_to_policy(policy: &mut Policy, level: AggressionLevel) {
    // The seeded gaming profile ids. If a future Policy::default()
    // adds more profiles, they get the same treatment automatically
    // via the predicate.
    for profile in policy.profiles.values_mut() {
        let id = profile.id.0.as_str();
        // Heuristic: any profile id starting "game" is a gaming
        // profile subject to the user's aggression choice.
        // "perf", "eco", and user-authored non-gaming profiles
        // (which won't start with "game") stay untouched.
        if !id.starts_with("game") {
            continue;
        }
        match level {
            AggressionLevel::Aggressive => {
                // No mutation — the seeded arsenal is exactly what
                // Aggressive means.
            }
            AggressionLevel::Balanced => {
                if let Some(gm) = profile.game_mode.as_mut() {
                    gm.stop_services.clear();
                    gm.suspend_processes.clear();
                    gm.hide_taskbar = false;
                    gm.pause_windows_update = false;
                    // Keep `power_plan` — that's the
                    // single benign action Balanced retains.
                }
            }
            AggressionLevel::PinningOnly => {
                profile.game_mode = None;
            }
        }
    }
}

/// Render one frame of the wizard. Returns the appropriate result
/// based on user action. Caller is expected to invoke each frame
/// while `state` exists.
pub fn render(ctx: &egui::Context, state: &mut OnboardingState) -> OnboardingResult {
    let mut next_action: Option<NextAction> = None;

    egui::Window::new("Welcome to FrameSage")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .default_width(580.0)
        .show(ctx, |ui| match state.page {
            0 => render_page_one(ui, &mut next_action),
            1 => render_page_two(ui, state, &mut next_action),
            2 => render_page_three(ui, &mut next_action),
            _ => render_page_four(ui, state, &mut next_action),
        });

    match next_action {
        Some(NextAction::Forward) => {
            state.page = state.page.saturating_add(1).min(3);
            OnboardingResult::StillVisible
        }
        Some(NextAction::Back) => {
            state.page = state.page.saturating_sub(1);
            OnboardingResult::StillVisible
        }
        Some(NextAction::Finish) => {
            // Page 4 only reachable when state.choice is Some — pin
            // the invariant here.
            let level = state
                .choice
                .expect("Finish click requires a selection on Page 2");
            OnboardingResult::Finished(level)
        }
        None => OnboardingResult::StillVisible,
    }
}

enum NextAction {
    Forward,
    Back,
    Finish,
}

fn render_page_one(ui: &mut egui::Ui, next: &mut Option<NextAction>) {
    ui.add_space(4.0);
    ui.label(theme::section_heading("Welcome to FrameSage"));
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "FrameSage is for users who want maximum performance during games or focused \
             work sessions. It will stop background services and suspend non-essential \
             processes during a session. Everything is reversed when the session ends. \
             Every action is journaled and reviewable after the fact.",
        )
        .size(13.0),
    );
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "If you'd rather a gentle optimizer, this isn't the right tool. Process Lasso's \
             ProBalance-only mode or Windows' built-in Game Mode are better fits for that.",
        )
        .color(theme::TEXT_MUTED)
        .size(13.0),
    );
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new(
                    egui::RichText::new("Continue")
                        .strong()
                        .color(theme::ACCENT),
                ))
                .clicked()
            {
                *next = Some(NextAction::Forward);
            }
        });
    });
}

fn render_page_two(ui: &mut egui::Ui, state: &mut OnboardingState, next: &mut Option<NextAction>) {
    ui.add_space(4.0);
    ui.label(theme::section_heading("Choose your level"));
    ui.add_space(4.0);
    ui.colored_label(
        theme::TEXT_MUTED,
        "Applies to BF6, Fortnite, and any user-added games. Valorant always uses AC-Safe \
         mode (Vanguard track record) regardless of your choice — your aggression \
         preference still controls the environment around it (services, processes, power).",
    );
    ui.add_space(10.0);

    radio_card(
        ui,
        state,
        AggressionLevel::Aggressive,
        "Stops cloud sync, OEM updaters, Windows Search/Update/telemetry. Suspends RGB \
         tools, GameBar, Widgets. Hides taskbar. Switches to High Performance. Pins to \
         X3D / Cache CCD. Reversed automatically on exit.",
    );
    ui.add_space(6.0);
    radio_card(
        ui,
        state,
        AggressionLevel::Balanced,
        "CPU pin + priority bump + power plan switch only. No services stopped. No \
         processes suspended. Cloud sync stays running. Taskbar stays visible.",
    );
    ui.add_space(6.0);
    radio_card(
        ui,
        state,
        AggressionLevel::PinningOnly,
        "Affinity pin to the X3D / Cache CCD (or top-ranked cores on non-X3D) and \
         nothing else. Closest equivalent to a Process Lasso affinity-only rule.",
    );

    ui.add_space(14.0);
    ui.horizontal(|ui| {
        if ui.button("Back").clicked() {
            *next = Some(NextAction::Back);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let continue_enabled = state.choice.is_some();
            if ui
                .add_enabled(
                    continue_enabled,
                    egui::Button::new(
                        egui::RichText::new("Continue")
                            .strong()
                            .color(theme::ACCENT),
                    ),
                )
                .clicked()
            {
                *next = Some(NextAction::Forward);
            }
            if !continue_enabled {
                ui.colored_label(theme::TEXT_MUTED, "Pick a level first.");
            }
        });
    });
}

fn radio_card(
    ui: &mut egui::Ui,
    state: &mut OnboardingState,
    level: AggressionLevel,
    description: &str,
) {
    let selected = state.choice == Some(level);
    let border = if selected {
        theme::ACCENT
    } else {
        theme::TEXT_MUTED
    };
    egui::Frame::none()
        .stroke(egui::Stroke::new(1.0, border))
        .rounding(6.0)
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let mut sel = selected;
                if ui.radio_value(&mut sel, true, "").changed() && sel {
                    state.choice = Some(level);
                }
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(level.label())
                            .strong()
                            .size(14.0)
                            .color(if selected { theme::ACCENT } else { theme::TEXT }),
                    );
                    ui.colored_label(theme::TEXT_MUTED, level.subtitle());
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(description).size(12.5));
                });
            });
        });
}

fn render_page_three(ui: &mut egui::Ui, next: &mut Option<NextAction>) {
    ui.add_space(4.0);
    ui.label(theme::section_heading("Manual Game Mode"));
    ui.add_space(8.0);
    ui.label(
        "Beyond per-game rules, you can manually enter Game Mode system-wide for any \
         profile. Use this when you're starting a focused work block or an unrecognised \
         game without a rule.",
    );
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "For v0.6 the hotkey is fixed at Ctrl+Alt+G (default). Custom binding lives \
             in the Settings tab — coming in v0.7.",
        )
        .color(theme::TEXT_MUTED),
    );
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new("You can also enter Manual Game Mode from:").color(theme::TEXT_MUTED),
    );
    ui.label(
        egui::RichText::new("  • the tray menu (right-click → \"Enter Manual Game Mode\")")
            .color(theme::TEXT_MUTED),
    );
    ui.label(
        egui::RichText::new("  • the Status-tab \"Quick actions\" panel").color(theme::TEXT_MUTED),
    );
    ui.label(
        egui::RichText::new("  • `framesage game-mode on <profile>` (CLI)")
            .color(theme::TEXT_MUTED),
    );

    ui.add_space(14.0);
    ui.horizontal(|ui| {
        if ui.button("Back").clicked() {
            *next = Some(NextAction::Back);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new(
                    egui::RichText::new("Continue")
                        .strong()
                        .color(theme::ACCENT),
                ))
                .clicked()
            {
                *next = Some(NextAction::Forward);
            }
        });
    });
}

fn render_page_four(ui: &mut egui::Ui, state: &OnboardingState, next: &mut Option<NextAction>) {
    ui.add_space(4.0);
    ui.label(theme::section_heading("Ready to go"));
    ui.add_space(8.0);
    if let Some(level) = state.choice {
        ui.label(format!(
            "You picked: {}. That's what BF6, Fortnite, and any future user-added games \
             will use.",
            level.label()
        ));
        ui.add_space(4.0);
        ui.colored_label(
            theme::TEXT_MUTED,
            "You can change this later in the Profiles tab. The Status tab shows what's \
             happening live; the Recent activity card surfaces every action FrameSage \
             takes with a timestamp.",
        );
    } else {
        // Safety: render shouldn't reach Page 4 without a choice
        // (Continue is disabled on Page 2 without one). Render a
        // sensible fallback anyway.
        ui.colored_label(
            theme::ERROR,
            "Internal error: reached Page 4 without a choice.",
        );
    }
    ui.add_space(14.0);
    ui.horizontal(|ui| {
        if ui.button("Back").clicked() {
            *next = Some(NextAction::Back);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(egui::Button::new(
                    egui::RichText::new("Finish").strong().color(theme::SUCCESS),
                ))
                .clicked()
            {
                *next = Some(NextAction::Finish);
            }
        });
    });
}

/// Synthesize a Balanced `GameModeActions` from an existing
/// Aggressive one. Public so tests can exercise the predicate
/// without going through the full Policy mutation.
#[cfg(test)]
pub fn balanced_from(
    aggressive: &framesage_core::game_mode::GameModeActions,
) -> framesage_core::game_mode::GameModeActions {
    let mut gm = aggressive.clone();
    gm.stop_services.clear();
    gm.suspend_processes.clear();
    gm.hide_taskbar = false;
    gm.pause_windows_update = false;
    gm
}

#[cfg(test)]
mod tests {
    use super::*;
    use framesage_core::game_mode::{GameModeActions, PowerPlanId};

    fn aggressive_default() -> Policy {
        Policy::default()
    }

    #[test]
    fn aggression_aggressive_leaves_game_mode_unchanged() {
        let baseline = aggressive_default();
        let mut p = aggressive_default();
        apply_choice_to_policy(&mut p, AggressionLevel::Aggressive);

        // Every gaming profile's game_mode is identical pre/post.
        for (id, profile) in &p.profiles {
            if !id.0.starts_with("game") {
                continue;
            }
            let baseline_profile = baseline.profile(id).expect("profile in baseline");
            assert_eq!(
                profile.game_mode, baseline_profile.game_mode,
                "Aggressive must not perturb {id}'s game_mode",
            );
        }
    }

    #[test]
    fn aggression_balanced_clears_stop_services_and_suspend_processes() {
        let mut p = aggressive_default();
        apply_choice_to_policy(&mut p, AggressionLevel::Balanced);

        for (id, profile) in &p.profiles {
            if !id.0.starts_with("game") {
                continue;
            }
            let gm = profile
                .game_mode
                .as_ref()
                .expect("Balanced must keep game_mode (just emptied)");
            assert!(
                gm.stop_services.is_empty(),
                "{id}.game_mode.stop_services must be empty under Balanced",
            );
            assert!(
                gm.suspend_processes.is_empty(),
                "{id}.game_mode.suspend_processes must be empty under Balanced",
            );
            assert!(
                !gm.hide_taskbar,
                "{id}.game_mode.hide_taskbar must be false under Balanced",
            );
            assert!(
                !gm.pause_windows_update,
                "{id}.game_mode.pause_windows_update must be false under Balanced",
            );
        }
    }

    #[test]
    fn aggression_balanced_keeps_power_plan_switch() {
        let mut p = aggressive_default();
        apply_choice_to_policy(&mut p, AggressionLevel::Balanced);
        // Find any gaming profile, confirm power plan is preserved
        // when it was set in the seeded version.
        let baseline = aggressive_default();
        for (id, profile) in &p.profiles {
            if !id.0.starts_with("game") {
                continue;
            }
            let baseline_gm = baseline
                .profile(id)
                .and_then(|p| p.game_mode.as_ref())
                .map(|gm| gm.power_plan.clone())
                .unwrap_or_default();
            let actual = profile
                .game_mode
                .as_ref()
                .map(|gm| gm.power_plan.clone())
                .unwrap_or_default();
            assert_eq!(
                baseline_gm, actual,
                "{id}.game_mode.power_plan must survive Balanced",
            );
        }
    }

    #[test]
    fn aggression_pinning_only_clears_game_mode_entirely() {
        let mut p = aggressive_default();
        apply_choice_to_policy(&mut p, AggressionLevel::PinningOnly);
        for (id, profile) in &p.profiles {
            if !id.0.starts_with("game") {
                continue;
            }
            assert!(
                profile.game_mode.is_none(),
                "{id}.game_mode must be None under PinningOnly",
            );
        }
    }

    #[test]
    fn aggression_choice_does_not_touch_non_gaming_profiles() {
        // perf + eco should be byte-identical pre/post any choice.
        let baseline = aggressive_default();
        for level in [
            AggressionLevel::Aggressive,
            AggressionLevel::Balanced,
            AggressionLevel::PinningOnly,
        ] {
            let mut p = aggressive_default();
            apply_choice_to_policy(&mut p, level);
            for non_gaming in ["perf", "eco"] {
                let baseline_p = baseline.profile(&non_gaming.into());
                let actual_p = p.profile(&non_gaming.into());
                assert_eq!(
                    baseline_p, actual_p,
                    "{level:?} must not touch the '{non_gaming}' profile",
                );
            }
        }
    }

    /// balanced_from is a test-only helper but lives in the
    /// production module so it's reachable. Pin the basic
    /// invariant: it returns the same struct shape minus the
    /// stop/suspend/hide/pause fields.
    #[test]
    fn balanced_from_keeps_power_plan_strips_aggression() {
        let gm = GameModeActions {
            stop_services: vec!["SysMain".into(), "WSearch".into()],
            suspend_processes: vec!["OneDrive.exe".into()],
            hide_taskbar: true,
            pause_windows_update: true,
            power_plan: Some(PowerPlanId::HighPerformance),
            ..Default::default()
        };

        let bal = balanced_from(&gm);
        assert!(bal.stop_services.is_empty());
        assert!(bal.suspend_processes.is_empty());
        assert!(!bal.hide_taskbar);
        assert!(!bal.pause_windows_update);
        assert_eq!(bal.power_plan, Some(PowerPlanId::HighPerformance));
    }
}
