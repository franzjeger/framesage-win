//! Item 3.6 (fourth slice) — profile / Game-Mode / sub-field editors
//! lifted out of main.rs.
//!
//! What lives here:
//!
//! * `render_profile_editor` — the top-level Profile edit form
//!   (description + per-process knobs + CPU targeting + Game Mode
//!   wrapper).
//! * `game_mode_editor` — the GameModeActions sub-editor (hide
//!   taskbar, services-to-stop list, processes-to-suspend list,
//!   power plan, WU pause).
//! * Sub-field widgets: `string_list_edit`, `power_plan_edit`,
//!   `option_combo`, `cpu_selector_edit`.
//! * `CpuSelectorKind` — discriminant for `Option<CpuSelector>`
//!   that drives the kind-dropdown in `cpu_selector_edit`.
//!
//! All `pub(crate)` because they're called from `FramesageApp`'s
//! tab rendering paths in main.rs. None of these depend on
//! `FramesageApp` state — they take `&mut`s of the policy fields
//! and let the caller persist on save.

use eframe::egui;

use framesage_core::{
    AntiCheatProfile, CoreKind, CpuSelector, GameModeActions, IoPriority, MemoryPriority,
    PowerPlanId, PowerThrottlingMode, PriorityClass, Profile,
};

use crate::theme;

// ─── Profile editor (top-level) ──────────────────────────────────────────────

/// Per-profile editor for the simple fields. CpuSelector (cpu_sets,
/// affinity_mask) and game_mode are shown read-only; their editors land
/// in a follow-up commit.
pub(crate) fn render_profile_editor(ui: &mut egui::Ui, p: &mut Profile) {
    ui.group(|ui| {
        ui.heading("Description");
        ui.add(
            egui::TextEdit::multiline(&mut p.description)
                .hint_text("Human description of what this profile does.")
                .desired_rows(2)
                .desired_width(f32::INFINITY),
        );
    });

    ui.add_space(4.0);
    ui.group(|ui| {
        ui.heading("Anti-cheat behavior");
        ac_profile_selector(ui, &mut p.ac_safe_mode_target);
    });

    ui.add_space(4.0);
    ui.group(|ui| {
        ui.heading("Per-process (editable)");
        option_combo(
            ui,
            "Power throttling",
            &mut p.power_throttling,
            &[
                PowerThrottlingMode::Eco,
                PowerThrottlingMode::Performance,
                PowerThrottlingMode::SystemDefault,
            ],
            |v| v.to_string(),
        );
        option_combo(
            ui,
            "Priority class",
            &mut p.priority_class,
            &[
                PriorityClass::Idle,
                PriorityClass::BelowNormal,
                PriorityClass::Normal,
                PriorityClass::AboveNormal,
                PriorityClass::High,
            ],
            |v| v.to_string(),
        );
        option_combo(
            ui,
            "I/O priority",
            &mut p.io_priority,
            &[
                IoPriority::VeryLow,
                IoPriority::Low,
                IoPriority::Normal,
                IoPriority::High,
                IoPriority::Critical,
            ],
            |v| v.to_string(),
        );
        option_combo(
            ui,
            "Memory priority",
            &mut p.memory_priority,
            &[
                MemoryPriority::VeryLow,
                MemoryPriority::Low,
                MemoryPriority::Medium,
                MemoryPriority::BelowNormal,
                MemoryPriority::Normal,
            ],
            |v| v.to_string(),
        );
        ui.horizontal(|ui| {
            ui.add_sized(
                [150.0, 16.0],
                egui::Label::new(egui::RichText::new("Trim working set").weak()),
            );
            ui.checkbox(&mut p.trim_working_set, "");
        });
        // Item 4.3 — persistent flag exposed in the editor. When set,
        // the per-PID knobs stick for the lifetime of the matching
        // process (no revert on focus change) AND the engine
        // re-asserts them every ~2 s to defeat self-modification
        // (games that call SetProcessAffinityMask on themselves at
        // startup, etc.). Default for game-x3d profiles; users
        // editing custom profiles need the explicit toggle.
        ui.horizontal(|ui| {
            ui.add_sized(
                [150.0, 16.0],
                egui::Label::new(egui::RichText::new("Persistent").weak()),
            )
            .on_hover_text(
                "Per-PID knobs stick for the lifetime of the matching process. \
                 The engine also re-asserts the apply every ~2s to defeat games \
                 that overwrite their own affinity at startup. Recommended for \
                 game profiles; leave off for tools you alt-tab between often.",
            );
            ui.checkbox(&mut p.persistent, "");
        });
    });

    ui.add_space(4.0);
    ui.group(|ui| {
        ui.heading("CPU targeting (editable)");
        cpu_selector_edit(ui, "CPU sets", &mut p.cpu_sets);
        cpu_selector_edit(ui, "Affinity mask", &mut p.affinity_mask);
    });

    ui.add_space(4.0);
    ui.group(|ui| {
        ui.heading("Game Mode (editable)");
        let mut enabled = p.game_mode.is_some();
        let was_enabled = enabled;
        ui.checkbox(
            &mut enabled,
            "Enable system-wide Game Mode for this profile",
        );
        if enabled != was_enabled {
            p.game_mode = if enabled {
                Some(GameModeActions::default())
            } else {
                None
            };
        }
        if let Some(gm) = &mut p.game_mode {
            game_mode_editor(ui, gm);
        }
    });
}

// ─── Game Mode actions editor ────────────────────────────────────────────────

/// Editor for the `GameModeActions` block. Service stop/process suspend
/// lists are edited as multi-line text — one entry per line — so the user
/// doesn't have to mentally parse comma-separated strings while typing.
/// Both lists are gated by the engine's curated safe-list at apply time;
/// unknown ids are logged and skipped, so the user can't break things
/// with a typo here.
pub(crate) fn game_mode_editor(ui: &mut egui::Ui, gm: &mut GameModeActions) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 16.0],
            egui::Label::new(egui::RichText::new("Hide taskbar").weak()),
        );
        ui.checkbox(&mut gm.hide_taskbar, "");
    });

    // Focus Assist has no documented user-mode API on current Windows builds
    // (Microsoft's own Settings app uses an undocumented COM interface). The
    // planner rejects this action with `NotImplemented` at apply time, so
    // we surface that here as a disabled control with a one-line explanation
    // — exposing a checkbox that silently does nothing was the prior bug.
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 16.0],
            egui::Label::new(egui::RichText::new("Focus assist").weak()),
        );
        ui.add_enabled(
            false,
            egui::Label::new(
                egui::RichText::new("disabled — no documented Windows API")
                    .color(theme::TEXT_MUTED),
            ),
        );
    });
    // Clear any value a previous build had stored so it doesn't haunt the
    // policy file as a value that will never be honoured.
    gm.focus_assist = None;

    // Item 4.13 — replace the bare multiline-text editors with
    // denylist-aware variants that show per-line "Blocked:
    // <rationale>" hints inline. The denylist remains
    // non-overridable; the change is the UX (user sees WHY an
    // entry is refused at edit time instead of silently dropping
    // it at apply time).
    safe_list_aware_list_edit(
        ui,
        "Stop services",
        &mut gm.stop_services,
        ListKind::Service,
        "One service short-name per line (e.g. SysMain, WSearch, DiagTrack).\n\
         Allowed: any non-denylisted service.\n\
         Blocked: kernel-critical / AV / anti-cheat services (highlighted inline).",
    );

    safe_list_aware_list_edit(
        ui,
        "Suspend processes",
        &mut gm.suspend_processes,
        ListKind::Process,
        "One exe name per line (e.g. OneDrive.exe, Dropbox.exe).\n\
         Allowed: any non-denylisted process.\n\
         Blocked: shell / kernel / AV / anti-cheat (highlighted inline).",
    );

    power_plan_edit(ui, "Power plan", &mut gm.power_plan);

    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 16.0],
            egui::Label::new(egui::RichText::new("Pause Windows Update").weak()),
        );
        ui.checkbox(&mut gm.pause_windows_update, "(stub in v0.1)");
    });
}

// ─── Sub-field widgets ───────────────────────────────────────────────────────

/// Item 4.13 — discriminates between "service id" and "process exe"
/// for the [`safe_list_aware_list_edit`] widget; controls which
/// SafeList check (`check_service` vs `check_process`) runs against
/// each entry and what label is shown when an entry is denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListKind {
    Service,
    Process,
}

/// Item 4.13 — denylist-aware `Vec<String>` editor. Wraps
/// [`string_list_edit`] with a per-entry status table below the
/// multiline textbox: each non-empty entry is checked against the
/// bundled SafeList and labelled "Blocked: <rationale>" inline when
/// it's denylisted, "OK" when allowed, "unknown" when not on either
/// list (the engine accepts these — user knows their machine).
///
/// The denylist is non-overridable. This widget makes the rejection
/// reason visible at edit time so the user understands why an
/// attempted entry would be refused at apply time, instead of
/// silently dropping it (the pre-4.13 behavior).
pub(crate) fn safe_list_aware_list_edit(
    ui: &mut egui::Ui,
    label: &str,
    items: &mut Vec<String>,
    kind: ListKind,
    hint: &str,
) {
    use framesage_gamemode::safe_list::{ProcessVerdict, SafeList, ServiceVerdict};
    let safe_list = SafeList::bundled();

    string_list_edit(ui, label, items, hint);

    if items.is_empty() {
        return;
    }
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.add_sized([150.0, 16.0], egui::Label::new(""));
        ui.vertical(|ui| {
            for entry in items.iter() {
                // Skip empty / whitespace-only lines silently — the
                // textbox can leave trailing blanks while typing.
                if entry.trim().is_empty() {
                    continue;
                }
                let (color, status) = match kind {
                    ListKind::Service => match safe_list.check_service(entry) {
                        ServiceVerdict::Denied(reason) => {
                            (theme::ERROR, format!("Blocked: {reason}"))
                        }
                        ServiceVerdict::Allowed(_) => {
                            (theme::SUCCESS, "OK (in curated allowlist)".to_owned())
                        }
                        ServiceVerdict::Unlisted => (
                            theme::TEXT_MUTED,
                            "unknown — accepted, but verify before stopping".to_owned(),
                        ),
                    },
                    ListKind::Process => match safe_list.check_process(entry) {
                        ProcessVerdict::Denied(reason) => {
                            (theme::ERROR, format!("Blocked: {reason}"))
                        }
                        ProcessVerdict::Allowed(_) => {
                            (theme::SUCCESS, "OK (in curated allowlist)".to_owned())
                        }
                        ProcessVerdict::Unlisted => (
                            theme::TEXT_MUTED,
                            "unknown — accepted, but verify before suspending".to_owned(),
                        ),
                    },
                };
                ui.horizontal(|ui| {
                    ui.colored_label(theme::TEXT, egui::RichText::new(entry).monospace());
                    ui.colored_label(color, status);
                });
            }
        });
    });
}

/// `Vec<String>` editor: each entry on its own line in a multi-line text
/// area. Empty lines are filtered out on save so the user can leave a
/// trailing blank while typing without polluting the policy.
pub(crate) fn string_list_edit(
    ui: &mut egui::Ui,
    label: &str,
    items: &mut Vec<String>,
    hint: &str,
) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 16.0],
            egui::Label::new(egui::RichText::new(label).monospace().weak()),
        );
        let mut buf = items.join("\n");
        let resp = ui.add(
            egui::TextEdit::multiline(&mut buf)
                .desired_rows(3)
                .desired_width(280.0)
                .hint_text(hint),
        );
        if resp.changed() {
            *items = buf
                .lines()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect();
        }
    });
}

/// Item 4.13 — `AntiCheatProfile` per-rule selector. Four radios
/// (Aggressive / Hybrid / SafeMode / Disabled) with hover-text
/// explaining the trade-offs. Defaults to whatever the profile
/// currently carries (Aggressive on fresh profiles; the seeded
/// game / game-x3d / game-x3d-hybrid / game-x3d-safe profiles
/// ship with explicit values per defaults D-9 + D-10).
pub(crate) fn ac_profile_selector(ui: &mut egui::Ui, target: &mut AntiCheatProfile) {
    let options = [
        (
            AntiCheatProfile::Aggressive,
            "Aggressive",
            "Full sledgehammer. Every knob in the Profile applies, including \
             direct modifications to the game process (affinity, priority, CPU \
             sets, I/O priority, power throttling). Recommended for games with \
             no AC concerns (single-player, EAC-with-no-Javelin titles).",
        ),
        (
            AntiCheatProfile::Hybrid,
            "Hybrid",
            "Environment actions at full strength (services / processes / \
             power / taskbar), but the game process itself is left alone. \
             Recommended for BF6 + EA Javelin: Javelin actively blocks core \
             parking / affinity changes on dual-CCD Ryzen during multiplayer \
             and the press has named Process Lasso as risk-bearing.",
        ),
        (
            AntiCheatProfile::SafeMode,
            "AC-Safe Mode",
            "Environment actions still fire; game-process modifications NEVER \
             fire even if the user-authored profile asks for them. The \
             defensive choice for Vanguard-protected titles (Valorant). \
             Mirrors the Hone approach (1M+ Valorant users, zero AC issues).",
        ),
        (
            AntiCheatProfile::Disabled,
            "Disabled",
            "Engine enters STANDBY for this profile — no apply, no scans, \
             no ProBalance. Use when even the environment actions are too \
             risky (ESEA conflicts, unknown-AC titles you're paranoid about). \
             You can still manually flip back to a stricter tier later.",
        ),
    ];

    for (variant, label, explainer) in options {
        let selected = *target == variant;
        ui.horizontal(|ui| {
            let resp = ui.radio(selected, label);
            if resp.clicked() {
                *target = variant;
            }
            ui.colored_label(
                theme::TEXT_MUTED,
                egui::RichText::new(explainer).size(11.5),
            )
            .on_hover_text(format!("Variant: {variant:?}"));
        });
    }
}

/// Option<PowerPlanId> editor. PowerPlanId::Custom carries an arbitrary
/// GUID string; the UI offers it via an "<custom>" entry that swaps in
/// a text field for the GUID. Most users want one of the four named
/// plans, so they're presented first.
pub(crate) fn power_plan_edit(ui: &mut egui::Ui, label: &str, plan: &mut Option<PowerPlanId>) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 16.0],
            egui::Label::new(egui::RichText::new(label).monospace().weak()),
        );

        // Compute current selection text + discriminant tag for the combo.
        let selected_text = match plan {
            None => "<unset>".to_owned(),
            Some(PowerPlanId::Balanced) => "Balanced".to_owned(),
            Some(PowerPlanId::HighPerformance) => "High Performance".to_owned(),
            Some(PowerPlanId::PowerSaver) => "Power Saver".to_owned(),
            Some(PowerPlanId::UltimatePerformance) => "Ultimate Performance".to_owned(),
            Some(PowerPlanId::Custom(_)) => "Custom GUID".to_owned(),
        };

        // Selection via a 6-way combo; on change we replace the whole option
        // with the default value for the new variant.
        let mut new_choice = match plan {
            None => 0u8,
            Some(PowerPlanId::Balanced) => 1,
            Some(PowerPlanId::HighPerformance) => 2,
            Some(PowerPlanId::PowerSaver) => 3,
            Some(PowerPlanId::UltimatePerformance) => 4,
            Some(PowerPlanId::Custom(_)) => 5,
        };
        let prev_choice = new_choice;
        egui::ComboBox::from_id_source(("power-plan", label))
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut new_choice, 0, "<unset>");
                ui.selectable_value(&mut new_choice, 1, "Balanced");
                ui.selectable_value(&mut new_choice, 2, "High Performance");
                ui.selectable_value(&mut new_choice, 3, "Power Saver");
                ui.selectable_value(&mut new_choice, 4, "Ultimate Performance");
                ui.selectable_value(&mut new_choice, 5, "Custom GUID");
            });
        if new_choice != prev_choice {
            *plan = match new_choice {
                1 => Some(PowerPlanId::Balanced),
                2 => Some(PowerPlanId::HighPerformance),
                3 => Some(PowerPlanId::PowerSaver),
                4 => Some(PowerPlanId::UltimatePerformance),
                5 => Some(PowerPlanId::Custom(String::new())),
                _ => None,
            };
        }

        // Custom variant: render a GUID text field.
        if let Some(PowerPlanId::Custom(guid)) = plan {
            ui.add(
                egui::TextEdit::singleline(guid)
                    .hint_text("GUID like 381b4222-f694-…")
                    .desired_width(260.0),
            );
        }
    });
}

/// Helper widget for `Option<Enum>` fields. Renders a labeled ComboBox
/// where the first entry is "<unset>" (maps to `None`) followed by the
/// concrete variants. Generic on T so each enum gets its own widget at
/// compile time with no boxing.
pub(crate) fn option_combo<T>(
    ui: &mut egui::Ui,
    label: &str,
    current: &mut Option<T>,
    variants: &[T],
    fmt: impl Fn(&T) -> String,
) where
    T: Copy + PartialEq,
{
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 16.0],
            egui::Label::new(egui::RichText::new(label).monospace().weak()),
        );
        let selected_text = match current {
            None => "—".to_owned(),
            Some(v) => fmt(v),
        };
        egui::ComboBox::from_id_source(("option-combo", label))
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                ui.selectable_value(current, None, "— (unset)");
                for v in variants {
                    ui.selectable_value(current, Some(*v), fmt(v));
                }
            });
    });
}

// ─── CpuSelector edit ────────────────────────────────────────────────────────

/// Discriminant for a `Option<CpuSelector>` field, used to drive the
/// kind-dropdown in `cpu_selector_edit`. Decoupled from `CpuSelector`
/// itself so changing the kind doesn't require carrying the old
/// variant's data around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CpuSelectorKind {
    Unset,
    All,
    Kind,
    Ccd,
    CcdNot,
    TopRanked,
    Mask,
}

impl CpuSelectorKind {
    fn from_option(sel: Option<&CpuSelector>) -> Self {
        match sel {
            None => Self::Unset,
            Some(CpuSelector::All) => Self::All,
            Some(CpuSelector::Kind(_)) => Self::Kind,
            Some(CpuSelector::Ccd(_)) => Self::Ccd,
            Some(CpuSelector::CcdNot(_)) => Self::CcdNot,
            Some(CpuSelector::TopRanked(_)) => Self::TopRanked,
            Some(CpuSelector::Mask(_)) => Self::Mask,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Unset => "— (unset)",
            Self::All => "All cores",
            Self::Kind => "By core kind",
            Self::Ccd => "CCD by index",
            Self::CcdNot => "Everything except CCD",
            Self::TopRanked => "Top N by CPPC rank",
            Self::Mask => "Explicit bitmask",
        }
    }

    /// Materialise a default `Option<CpuSelector>` for this discriminant.
    /// When the user switches the dropdown to a new variant we lose the
    /// previous variant's data — using a stable default for each kind is
    /// less surprising than trying to coerce values across variants.
    fn default_value(self) -> Option<CpuSelector> {
        match self {
            Self::Unset => None,
            Self::All => Some(CpuSelector::All),
            Self::Kind => Some(CpuSelector::Kind(CoreKind::Cache)),
            Self::Ccd => Some(CpuSelector::Ccd(0)),
            Self::CcdNot => Some(CpuSelector::CcdNot(1)),
            Self::TopRanked => Some(CpuSelector::TopRanked(8)),
            Self::Mask => Some(CpuSelector::Mask(0xffff)),
        }
    }
}

/// Two-cell edit widget for `Option<CpuSelector>`. Left cell is a label.
/// Right cell is a kind-dropdown followed by a variant-specific value
/// widget (CoreKind combo for Kind, DragValue for the numeric variants,
/// hex text input for Mask).
pub(crate) fn cpu_selector_edit(ui: &mut egui::Ui, label: &str, sel: &mut Option<CpuSelector>) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 16.0],
            egui::Label::new(egui::RichText::new(label).monospace().weak()),
        );

        let mut kind = CpuSelectorKind::from_option(sel.as_ref());
        let prev_kind = kind;
        egui::ComboBox::from_id_source(("cpu-selector-kind", label))
            .selected_text(kind.label())
            .show_ui(ui, |ui| {
                for k in [
                    CpuSelectorKind::Unset,
                    CpuSelectorKind::All,
                    CpuSelectorKind::Kind,
                    CpuSelectorKind::Ccd,
                    CpuSelectorKind::CcdNot,
                    CpuSelectorKind::TopRanked,
                    CpuSelectorKind::Mask,
                ] {
                    ui.selectable_value(&mut kind, k, k.label());
                }
            });
        if kind != prev_kind {
            *sel = kind.default_value();
        }

        // Variant-specific value widget. Mutates the contained data in
        // place so the user sees their typing reflect immediately.
        match sel {
            None | Some(CpuSelector::All) => {}
            Some(CpuSelector::Kind(k)) => {
                egui::ComboBox::from_id_source(("cpu-selector-corekind", label))
                    .selected_text(k.to_string())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            k,
                            CoreKind::Performance,
                            CoreKind::Performance.to_string(),
                        );
                        ui.selectable_value(
                            k,
                            CoreKind::Efficiency,
                            CoreKind::Efficiency.to_string(),
                        );
                        ui.selectable_value(k, CoreKind::Cache, CoreKind::Cache.to_string());
                    });
            }
            Some(CpuSelector::Ccd(c)) | Some(CpuSelector::CcdNot(c)) => {
                ui.add(egui::DragValue::new(c).range(0..=15).speed(0.1));
            }
            Some(CpuSelector::TopRanked(n)) => {
                ui.add(egui::DragValue::new(n).range(1..=128).speed(0.25));
            }
            Some(CpuSelector::Mask(m)) => {
                // Hex text field with parse-on-change; on bad input we keep
                // the old value rather than zero out destructively. Width
                // is u64 (Windows KAFFINITY), so up to 64 logical CPUs in
                // one processor group.
                let mut buf = format!("{m:#x}");
                if ui.text_edit_singleline(&mut buf).changed() {
                    let trimmed = buf.trim().trim_start_matches("0x");
                    if let Ok(parsed) = u64::from_str_radix(trimmed, 16) {
                        *m = parsed;
                    }
                }
            }
        }
    });
}
