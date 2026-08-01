//! Item 3.6 (second slice) — pure egui rendering helpers lifted out
//! of main.rs.
//!
//! Everything in this module:
//!
//! * Takes `ui: &mut egui::Ui` (or a `Painter` for the sparkline)
//!   plus borrowed input data; no `FramesageApp` state.
//! * Renders one self-contained widget — perf readout, sparklines,
//!   status bar, read-only banner, profile-body grid, plus the tiny
//!   kv-row helpers and `format_local_hms` for the activity-log time
//!   column.
//!
//! Why a single module instead of a fan-out: every function here
//! cross-calls at least one other (`render_perf_readout` calls
//! `draw_sparkline`, `render_profile_body` calls `yes_no` /
//! `format_count_summary`, etc.). Keeping them together avoids a
//! visibility-juggling exercise across half-a-dozen tiny files for
//! no readability gain.
//!
//! The Round-3 chrome consolidation (design §3a) deleted the widgets
//! this module carried for the old four-row chrome and the pre-Round-3
//! Status tab — `render_status_hero`, `render_perf_band`,
//! `draw_per_core_matrix`, `render_activity_strip`,
//! `render_recent_activity`, `render_active_profile_summary`,
//! `render_foreground_summary`, `short_path`. They had no callers left
//! once the top bar and the Status tab were rebuilt; git history has
//! them if a later slice wants the per-core matrix back.

use eframe::egui;

use framesage_core::Profile;
use framesage_ipc::SystemMetrics;

use crate::formatters::{cpu_percent_color, format_bytes, format_cpu_selector, format_top_cores};
use crate::theme;

/// Two-line key/value cell for the detail panel. Key in a fixed-width muted
/// font on top, value in the regular body font underneath. Used in
/// `render_process_detail` so the grid wraps gracefully on narrow windows.
pub(crate) fn detail_kv(ui: &mut egui::Ui, key: &str, value: String) {
    ui.vertical(|ui| {
        ui.label(
            egui::RichText::new(key.to_uppercase())
                .small()
                .strong()
                .color(theme::p().text_muted)
                .extra_letter_spacing(1.0),
        );
        ui.label(value);
        ui.add_space(2.0);
    });
}

/// Format a `SystemTime` as `HH:MM:SS` in the current timezone for the
/// Activity Log "Time" column. We deliberately avoid the chrono crate to
/// keep the dep tree small — a `SystemTime` → UNIX seconds → manual h/m/s
/// breakdown is enough for a UI clock readout (no calendar math, no DST
/// edge cases that matter on a one-day-or-less event buffer).
pub(crate) fn format_local_hms(t: std::time::SystemTime) -> String {
    use std::time::UNIX_EPOCH;
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Local offset, seconds east of UTC. Win32 `GetTimeZoneInformation`
    // would give the precise value; for the activity log we just need
    // something that matches the user's wall clock, so use the
    // SystemTime → DateTime difference reported by `chrono`-less means:
    // Windows returns the *current* offset via `_timezone` + DST flag at
    // process startup. As a simpler approximation, ask the OS for the
    // local time of `t` directly via Win32.
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::FILETIME;
        use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
        // SystemTime epoch is 1970-01-01; FILETIME epoch is 1601-01-01.
        // Difference in 100-ns ticks: 116444736000000000.
        let ticks = secs
            .saturating_mul(10_000_000)
            .saturating_add(116_444_736_000_000_000);
        let ft = FILETIME {
            dwLowDateTime: (ticks & 0xFFFF_FFFF) as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };
        let mut utc = windows::Win32::Foundation::SYSTEMTIME::default();
        // SAFETY: ft is a fully-initialised FILETIME, utc is a valid
        // out-parameter for the matching struct.
        if unsafe { FileTimeToSystemTime(&ft, &mut utc) }.is_ok() {
            let mut local = windows::Win32::Foundation::SYSTEMTIME::default();
            // SAFETY: utc is fully initialised, local is a valid out-param.
            if unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local) }.is_ok() {
                return format!(
                    "{:02}:{:02}:{:02}",
                    local.wHour, local.wMinute, local.wSecond
                );
            }
        }
    }
    // Fallback: UTC h/m/s. Better than nothing on non-Windows or if the
    // timezone conversion ever fails.
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// Reusable read-only banner used by the Rules and Profiles tabs when the
/// tray isn't elevated. Matches the Status tab's quick-actions banner so the
/// "you need admin to edit this" signal reads the same everywhere.
pub(crate) fn render_readonly_banner(ui: &mut egui::Ui, body: &str) {
    theme::banner(theme::p().warning).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(
                theme::p().warning,
                egui::RichText::new("⚠").strong().size(14.0),
            );
            ui.label(
                egui::RichText::new("Read-only mode")
                    .strong()
                    .color(theme::p().text),
            );
            ui.colored_label(theme::p().text_muted, format!("— {body}"));
        });
    });
}

/// Compact KV row inside an egui::Grid. Caller is responsible for ending
/// each row with `ui.end_row()` — this helper does it.
pub(crate) fn kv_grid_row(ui: &mut egui::Ui, key: &str, value: String) {
    ui.label(
        egui::RichText::new(key)
            .color(theme::p().text_muted)
            .size(12.0),
    );
    ui.label(value);
    ui.end_row();
}

/// Live perf readout for the right end of the combined top bar (design
/// Round 3 §3a): `CPU 62%  MEM 54%` plus a tiny 60-sample sparkline.
///
/// Drawn into a right-to-left layout, so widgets are added in visual
/// right-to-left order: sparkline first, then MEM, then CPU. The old
/// full-width perf band this replaces also carried a 64-bar per-core
/// matrix; the top bar has no room for it and the design doesn't ask
/// for one, so the per-core breakdown now lives only in the CPU%
/// hover text.
pub(crate) fn render_perf_readout(
    ui: &mut egui::Ui,
    metrics: &SystemMetrics,
    history: &[(u8, u8)],
) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(120.0, 18.0), egui::Sense::hover());
    draw_sparkline(ui.painter(), rect, history);
    ui.add_space(4.0);

    let mem_percent: u8 = if metrics.memory_total_bytes > 0 {
        ((metrics.memory_used_bytes as u128 * 100 / metrics.memory_total_bytes as u128).min(100))
            as u8
    } else {
        0
    };
    let mem_color = if mem_percent > 90 {
        theme::p().error
    } else if mem_percent > 75 {
        theme::p().warning
    } else {
        theme::p().text
    };
    let mem_resp = ui.label(
        egui::RichText::new(format!("{mem_percent}%"))
            .color(mem_color)
            .strong()
            .size(14.0),
    );
    // The absolute GB figures moved into the hover — two numbers plus a
    // "8.6 GB / 47.6 GB" string is more than a chrome bar should spend.
    let _ = mem_resp.on_hover_text(format!(
        "{} / {} used",
        format_bytes(metrics.memory_used_bytes),
        format_bytes(metrics.memory_total_bytes)
    ));
    ui.label(
        egui::RichText::new("MEM")
            .color(theme::p().text_muted)
            .size(10.5),
    );
    ui.add_space(6.0);

    let cpu_resp = ui.label(
        egui::RichText::new(format!("{}%", metrics.cpu_percent))
            .color(cpu_percent_color(metrics.cpu_percent as u16))
            .strong()
            .size(14.0),
    );
    // Hover the aggregate CPU% to see which cores are doing the work —
    // helpful for X3D-class machines where you want to confirm the load
    // actually landed on the favoured CCD.
    let _ = cpu_resp.on_hover_text(format_top_cores(&metrics.per_core_cpu_percent, 5));
    ui.label(
        egui::RichText::new("CPU")
            .color(theme::p().text_muted)
            .size(10.5),
    );
}

/// Render two overlaid lines (CPU + memory) inside `rect` from the
/// `history` ring buffer. Newest sample on the right, oldest on the left.
/// Keeps the visual lightweight — no axes, no grid, just two stroke lines
/// with subtle fills. Same pattern Task Manager / Process Lasso use.
pub(crate) fn draw_sparkline(painter: &egui::Painter, rect: egui::Rect, history: &[(u8, u8)]) {
    use egui::epaint::PathShape;
    use egui::Stroke;

    // Background frame so the line has something to anchor against.
    painter.rect_filled(rect, 3.0, theme::p().surface);

    if history.len() < 2 {
        return;
    }

    let count = history.len().max(2);
    let dx = rect.width() / (count - 1) as f32;
    let mut cpu_points: Vec<egui::Pos2> = Vec::with_capacity(count);
    let mut mem_points: Vec<egui::Pos2> = Vec::with_capacity(count);
    for (i, (cpu, mem)) in history.iter().enumerate() {
        let x = rect.left() + i as f32 * dx;
        let cpu_y = rect.bottom() - (*cpu as f32 / 100.0) * rect.height();
        let mem_y = rect.bottom() - (*mem as f32 / 100.0) * rect.height();
        cpu_points.push(egui::pos2(x, cpu_y));
        mem_points.push(egui::pos2(x, mem_y));
    }

    // CPU line in accent, memory in a muted secondary color. Each gets a
    // subtle fill below the line for visual mass.
    let cpu_stroke = Stroke::new(1.5_f32, theme::p().accent);
    let mem_stroke = Stroke::new(1.0_f32, theme::p().series_secondary);

    // Filled area under the CPU line (the more eye-catching of the two,
    // matching its priority for the user).
    let mut cpu_fill: Vec<egui::Pos2> = cpu_points.clone();
    cpu_fill.push(egui::pos2(rect.right(), rect.bottom()));
    cpu_fill.push(egui::pos2(rect.left(), rect.bottom()));
    painter.add(PathShape::convex_polygon(
        cpu_fill,
        theme::fill_alpha(theme::p().accent, 30),
        Stroke::NONE,
    ));

    painter.add(PathShape::line(mem_points, mem_stroke));
    painter.add(PathShape::line(cpu_points, cpu_stroke));
}

/// Item 3.4 — single-channel sparkline for per-PID CPU% history.
/// Reuses the styling of `draw_sparkline` (background tray + accent
/// line + subtle fill) but with one value per sample instead of two.
/// Used by the Processes-tab detail panel to show 60 s of CPU%
/// history for the selected PID.
pub(crate) fn draw_single_sparkline(painter: &egui::Painter, rect: egui::Rect, history: &[u8]) {
    use egui::epaint::PathShape;
    use egui::Stroke;

    painter.rect_filled(rect, 3.0, theme::p().surface);

    if history.len() < 2 {
        return;
    }

    let count = history.len();
    let dx = rect.width() / (count - 1) as f32;
    let mut points: Vec<egui::Pos2> = Vec::with_capacity(count);
    for (i, &v) in history.iter().enumerate() {
        let x = rect.left() + i as f32 * dx;
        let y = rect.bottom() - (v as f32 / 100.0).clamp(0.0, 1.0) * rect.height();
        points.push(egui::pos2(x, y));
    }

    // Fill under the line for visual mass; same alpha as the
    // CPU fill in the system sparkline.
    let mut fill: Vec<egui::Pos2> = points.clone();
    fill.push(egui::pos2(rect.right(), rect.bottom()));
    fill.push(egui::pos2(rect.left(), rect.bottom()));
    painter.add(PathShape::convex_polygon(
        fill,
        theme::fill_alpha(theme::p().accent, 30),
        Stroke::NONE,
    ));
    painter.add(PathShape::line(
        points,
        Stroke::new(1.5_f32, theme::p().accent),
    ));
}

/// One-line status bar at the very bottom of the window. Shows engine state,
/// process counts, version, and the last-action echo. Sections are separated
/// by thin dividers in `TEXT_DIM` so the eye groups them naturally.
pub(crate) fn render_status_bar(
    ui: &mut egui::Ui,
    connected: bool,
    paused: bool,
    manual_override: Option<&framesage_core::ProfileId>,
    process_count: usize,
    managed_count: usize,
    last_action: Option<&str>,
) {
    ui.horizontal(|ui| {
        // Engine state — anchors the bar on the left.
        let (state_color, state_text) = if !connected {
            (theme::p().error, "Disconnected")
        } else if paused {
            (theme::p().warning, "Paused")
        } else if manual_override.is_some() {
            (theme::p().accent, "Manual")
        } else {
            (theme::p().success, "Running")
        };
        // Painted dot — the bundled font renders U+25CF as a tofu box.
        theme::dot(ui, state_color, 7.0);
        ui.add_space(2.0);
        ui.colored_label(state_color, egui::RichText::new(state_text).strong());

        if let Some(id) = manual_override {
            ui.colored_label(theme::p().text_muted, "·");
            ui.colored_label(theme::p().text_muted, format!("override: {}", id.0));
        }

        ui.colored_label(theme::p().text_muted, "·");
        let managed_str = if managed_count > 0 {
            format!("{process_count} processes ({managed_count} managed)")
        } else {
            format!("{process_count} processes")
        };
        ui.colored_label(theme::p().text_muted, managed_str);

        // Last action echo on the right; trims long messages so a noisy
        // error doesn't break the layout. Version anchors the far right.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.colored_label(
                theme::p().text_muted,
                format!("v{}", env!("CARGO_PKG_VERSION")),
            );
            if let Some(text) = last_action {
                ui.colored_label(theme::p().text_muted, "·");
                let max_chars = 80;
                let trimmed = if text.chars().count() > max_chars {
                    let mut t: String = text.chars().take(max_chars - 1).collect();
                    t.push('…');
                    t
                } else {
                    text.to_string()
                };
                // Color failures red. Match the failure vocabulary the
                // service actually emits (error / failed / rejected /
                // denied) case-insensitively, so a differently-worded
                // rejection isn't silently shown as a neutral message.
                let lower = text.to_ascii_lowercase();
                let is_failure = ["error", "failed", "rejected", "denied", "refused"]
                    .iter()
                    .any(|kw| lower.contains(kw));
                let color = if is_failure {
                    theme::p().error
                } else {
                    theme::p().text_muted
                };
                ui.colored_label(color, trimmed);
            }
        });
    });
}

/// Read-only profile card — knobs + Game Mode summary in a two-column grid.
/// Used by the Profiles tab when the tray is unelevated (the editor lives
/// behind admin gating) and by the Status-tab profile preview.
pub(crate) fn render_profile_body(ui: &mut egui::Ui, p: &Profile) {
    if !p.description.is_empty() {
        ui.colored_label(theme::p().text_muted, &p.description);
        ui.add_space(8.0);
    }

    // Per-process knobs in a tight grid.
    ui.label(theme::section_heading("Per-process"));
    ui.add_space(4.0);
    egui::Grid::new(("profile-perproc-grid", p.id.0.as_str()))
        .num_columns(2)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            kv_grid_row(ui, "CPU sets", format_cpu_selector(p.cpu_sets.as_ref()));
            kv_grid_row(
                ui,
                "Affinity mask",
                format_cpu_selector(p.affinity_mask.as_ref()),
            );
            kv_grid_row(
                ui,
                "Power throttling",
                p.power_throttling
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".into()),
            );
            kv_grid_row(
                ui,
                "Priority class",
                p.priority_class
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".into()),
            );
            kv_grid_row(
                ui,
                "I/O priority",
                p.io_priority
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".into()),
            );
            kv_grid_row(
                ui,
                "Memory priority",
                p.memory_priority
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".into()),
            );
            kv_grid_row(ui, "Trim working set", yes_no(p.trim_working_set).into());
        });

    ui.add_space(10.0);

    // Game Mode section.
    if let Some(gm) = &p.game_mode {
        ui.label(theme::section_heading("Game Mode (system-wide)"));
        ui.add_space(4.0);
        egui::Grid::new(("profile-gamemode-grid", p.id.0.as_str()))
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                kv_grid_row(ui, "Hide taskbar", yes_no(gm.hide_taskbar).into());
                kv_grid_row(
                    ui,
                    "Stop services",
                    if gm.stop_services.is_empty() {
                        "—".into()
                    } else {
                        format_count_summary(&gm.stop_services)
                    },
                );
                kv_grid_row(
                    ui,
                    "Suspend processes",
                    if gm.suspend_processes.is_empty() {
                        "—".into()
                    } else {
                        format_count_summary(&gm.suspend_processes)
                    },
                );
                kv_grid_row(
                    ui,
                    "Power plan",
                    gm.power_plan
                        .as_ref()
                        .map(|p| p.to_string())
                        .unwrap_or_else(|| "—".into()),
                );
                kv_grid_row(
                    ui,
                    "Pause Windows Update",
                    if gm.pause_windows_update {
                        "Yes".into()
                    } else {
                        "No".into()
                    },
                );
            });
    } else {
        ui.colored_label(
            theme::p().text_dim,
            "Game Mode not requested by this profile.",
        );
    }
}

/// Summarise a long list of ids as "N items: first, second, third…" so the
/// profile card stays compact. Full list is visible in the editor anyway.
pub(crate) fn format_count_summary(items: &[String]) -> String {
    let n = items.len();
    if n <= 3 {
        return items.join(", ");
    }
    let preview: Vec<&str> = items.iter().take(3).map(|s| s.as_str()).collect();
    format!("{n} entries — {}, …", preview.join(", "))
}

/// Human label for booleans shown to users. Used in the read-only profile
/// viewer where `true`/`false` reads as developer output, not as a setting.
pub(crate) fn yes_no(b: bool) -> &'static str {
    if b {
        "Yes"
    } else {
        "No"
    }
}

/// Legacy fixed-width KV row, kept around in case a future editor section
/// needs it (mixed widget rows where `egui::Grid` doesn't help). Currently
/// unused since the polish pass; `#[allow(dead_code)]` avoids a warning.
#[allow(dead_code)]
pub(crate) fn kv_row(ui: &mut egui::Ui, key: &str, value: String) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 16.0],
            egui::Label::new(egui::RichText::new(key).monospace().weak()),
        );
        ui.label(value);
    });
}
