//! Item 3.6 (second slice) — pure egui rendering helpers lifted out
//! of main.rs.
//!
//! Everything in this module:
//!
//! * Takes `ui: &mut egui::Ui` (or a `Painter` for the sparkline)
//!   plus borrowed input data; no `FramesageApp` state.
//! * Renders one self-contained widget — hero strip, profile card,
//!   foreground card, perf band, sparkline, per-core matrix, status
//!   bar, activity strip, recent-activity feed, read-only banner,
//!   profile-body grid, plus the tiny kv-row helpers and
//!   `format_local_hms` for the activity-log time column.
//!
//! Why a single module instead of a fan-out: every function here
//! cross-calls at least one other (`render_active_profile_summary`
//! calls `kv_grid_row`, `render_foreground_summary` calls
//! `short_path`, `render_profile_body` calls `yes_no` /
//! `format_count_summary`, etc.). Keeping them together avoids a
//! visibility-juggling exercise across half-a-dozen tiny files for
//! no readability gain.

use eframe::egui;

use framesage_core::Profile;
use framesage_ipc::{ForegroundSnapshot, StatusSnapshot, SystemMetrics};

use crate::formatters::{
    cpu_percent_color, display_profile_id, format_bytes, format_cpu_selector, format_top_cores,
};
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

/// Hero strip at the top of the Status tab. One row, three signals: engine
/// state (with colored dot), policy summary (rules + default profile),
/// FrameSage-wide "what's happening right now" sentence on the right.
pub(crate) fn render_status_hero(ui: &mut egui::Ui, s: &StatusSnapshot) {
    theme::hero().show(ui, |ui| {
        ui.horizontal(|ui| {
            let (dot_color, headline) = if s.paused {
                (theme::p().warning, "Paused")
            } else {
                (theme::p().success, "Running")
            };
            // Engine state with a coloured dot.
            ui.label(egui::RichText::new("\u{25cf}").color(dot_color).size(14.0));
            ui.label(
                egui::RichText::new(headline)
                    .size(18.0)
                    .strong()
                    .color(theme::p().text),
            );
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new(format!("{} rules", s.policy.rules.len()))
                    .color(theme::p().text_muted),
            );
            ui.label(egui::RichText::new("·").color(theme::p().text_muted));
            ui.label(
                egui::RichText::new(format!(
                    "default: {}",
                    display_profile_id(&s.policy.default_profile.0)
                ))
                .color(theme::p().text_muted),
            );
            if let Some(bg) = &s.policy.background_profile {
                ui.label(egui::RichText::new("·").color(theme::p().text_muted));
                ui.label(
                    egui::RichText::new(format!("background: {}", display_profile_id(&bg.0)))
                        .color(theme::p().text_muted),
                );
            }
        });
    });
}

/// Single-card summary of the active profile: name, description, and the
/// three knobs the user cares about most at a glance.
// Superseded by the Round-3 Status stat cards (design renovation);
// retained for reuse by later renovation slices.
#[allow(dead_code)]
pub(crate) fn render_active_profile_summary(ui: &mut egui::Ui, s: &StatusSnapshot) {
    let Some(p) = &s.active_profile else {
        ui.colored_label(theme::p().text_muted, "No profile applied yet.");
        return;
    };
    ui.label(
        egui::RichText::new(display_profile_id(&p.id.0))
            .size(17.0)
            .strong()
            .color(theme::p().accent),
    );
    if !p.description.is_empty() {
        ui.add_space(2.0);
        ui.colored_label(theme::p().text_muted, &p.description);
    }
    ui.add_space(8.0);
    egui::Grid::new("active-profile-grid")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            kv_grid_row(ui, "CPU sets", format_cpu_selector(p.cpu_sets.as_ref()));
            kv_grid_row(
                ui,
                "Throttling",
                p.power_throttling
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".into()),
            );
            kv_grid_row(
                ui,
                "Priority",
                p.priority_class
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "—".into()),
            );
            kv_grid_row(
                ui,
                "Game Mode",
                if p.game_mode.is_some() {
                    "Enabled".into()
                } else {
                    "—".into()
                },
            );
        });
}

/// Single-card summary of the currently-foregrounded process.
#[allow(dead_code)] // superseded by Round-3 Status hero; retained for reuse
pub(crate) fn render_foreground_summary(ui: &mut egui::Ui, fg: &ForegroundSnapshot) {
    ui.label(
        egui::RichText::new(&fg.exe_name)
            .size(17.0)
            .strong()
            .color(theme::p().text),
    );
    ui.add_space(2.0);
    if !fg.title.is_empty() {
        ui.colored_label(theme::p().text_muted, &fg.title);
        ui.add_space(8.0);
    } else {
        ui.add_space(8.0);
    }
    egui::Grid::new("foreground-grid")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            kv_grid_row(ui, "PID", fg.pid.to_string());
            if !fg.path.is_empty() {
                kv_grid_row(ui, "Path", short_path(&fg.path));
            }
        });
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

/// Truncate long paths for display — keep the drive letter and the final
/// two components, ellipsise the middle. Avoids the path field exploding
/// the card width on deep installs.
#[allow(dead_code)] // used only by render_foreground_summary (retained)
pub(crate) fn short_path(path: &str) -> String {
    if path.len() <= 60 {
        return path.to_owned();
    }
    let parts: Vec<&str> = path.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    if parts.len() < 4 {
        return path.to_owned();
    }
    let first = parts[0];
    let last_two = &parts[parts.len() - 2..];
    format!("{}\\…\\{}\\{}", first, last_two[0], last_two[1])
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

/// Permanent performance band rendered above every tab. Two numeric
/// readouts (CPU%, Memory) plus a 60-sample sparkline. Designed to compress
/// to ~28 px of vertical space — enough to read at a glance, not enough to
/// dominate the tab content below it.
pub(crate) fn render_perf_band(ui: &mut egui::Ui, metrics: &SystemMetrics, history: &[(u8, u8)]) {
    ui.horizontal(|ui| {
        // Left cluster: the live numeric readouts. Color-coded by intensity
        // so the band visually flags contention without the user having to
        // read the number.
        ui.label(
            egui::RichText::new("CPU")
                .color(theme::p().text_muted)
                .size(11.0),
        );
        let cpu_color = cpu_percent_color(metrics.cpu_percent as u16);
        let cpu_resp = ui.label(
            egui::RichText::new(format!("{}%", metrics.cpu_percent))
                .color(cpu_color)
                .strong()
                .size(15.0),
        );
        // Hover the aggregate CPU% to see which cores are doing the work —
        // helpful for X3D-class machines where you want to confirm the
        // load actually landed on the favoured CCD.
        let _ = cpu_resp.on_hover_text(format_top_cores(&metrics.per_core_cpu_percent, 5));
        ui.add_space(16.0);

        let mem_percent: u8 = if metrics.memory_total_bytes > 0 {
            ((metrics.memory_used_bytes as u128 * 100 / metrics.memory_total_bytes as u128)
                .min(100)) as u8
        } else {
            0
        };
        ui.label(
            egui::RichText::new("MEM")
                .color(theme::p().text_muted)
                .size(11.0),
        );
        let mem_color = if mem_percent > 90 {
            theme::p().error
        } else if mem_percent > 75 {
            theme::p().warning
        } else {
            theme::p().text
        };
        ui.label(
            egui::RichText::new(format!("{}%", mem_percent))
                .color(mem_color)
                .strong()
                .size(15.0),
        );
        ui.colored_label(
            theme::p().text_muted,
            format!(
                " {} / {}",
                format_bytes(metrics.memory_used_bytes),
                format_bytes(metrics.memory_total_bytes)
            ),
        );

        // Right cluster: per-core CPU matrix (if available) + the sparkline.
        // Sparkline drawn first from the right edge; per-core matrix slots
        // in between the MEM text and the sparkline.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // S4 — flex the sparkline to the space left after the numeric
            // readouts (min 120, max 280) instead of a fixed 280, and drop
            // the per-core matrix on narrow windows so it can't collide
            // with the MEM readout. The aggregate CPU% hover still carries
            // the top-core breakdown when the matrix is hidden.
            let avail = ui.available_width();
            let spark_w = avail.clamp(120.0, 280.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(spark_w, 22.0), egui::Sense::hover());
            draw_sparkline(ui.painter(), rect, history);

            // Per-core matrix between sparkline and MEM. Right-to-left
            // layout means this allocation appears to the *left* of the
            // sparkline. Skipped on first sample (engine hasn't accumulated
            // two yet), if the kernel refused per-CPU info, or when the band
            // is too narrow to fit both the sparkline and the matrix.
            const MATRIX_MIN_WIDTH: f32 = 520.0;
            if !metrics.per_core_cpu_percent.is_empty() && avail >= MATRIX_MIN_WIDTH {
                ui.add_space(12.0);
                draw_per_core_matrix(ui, &metrics.per_core_cpu_percent);
            }
        });
    });
}

/// Width and styling for one bar in the per-core matrix. Constants so the
/// hit-test math in the hover handler stays in sync with the painter.
const PER_CORE_BAR_W: f32 = 5.0;
const PER_CORE_BAR_GAP: f32 = 1.0;
const PER_CORE_MAX_BARS: usize = 64;

/// Render one vertical bar per logical CPU. Bar height tracks utilisation
/// (0-100 ≡ 0% to full cell height); bar color reuses `cpu_percent_color`
/// so the matrix and the aggregate number speak the same color language.
/// Hover shows "Core N: M%" via an at-pointer tooltip — same affordance
/// Task Manager's grid view uses.
pub(crate) fn draw_per_core_matrix(ui: &mut egui::Ui, percents: &[u8]) {
    let cores = percents.len().min(PER_CORE_MAX_BARS);
    let total_w = (PER_CORE_BAR_W + PER_CORE_BAR_GAP) * cores as f32 - PER_CORE_BAR_GAP;
    let desired = egui::vec2(total_w.max(1.0), 22.0);
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter();

    // Background tray for the bars — visual anchor so bars at 0% still
    // appear inside a "well" rather than floating against the band.
    painter.rect_filled(rect, 3.0, theme::p().surface);

    let pad_y = 2.0;
    let max_h = rect.height() - pad_y * 2.0;
    let stride = PER_CORE_BAR_W + PER_CORE_BAR_GAP;
    for (i, &pct) in percents.iter().take(cores).enumerate() {
        let x = rect.left() + i as f32 * stride;
        let h = (max_h * (pct as f32 / 100.0)).clamp(1.0, max_h);
        let bar_rect = egui::Rect::from_min_size(
            egui::pos2(x, rect.bottom() - pad_y - h),
            egui::vec2(PER_CORE_BAR_W, h),
        );
        painter.rect_filled(
            bar_rect,
            egui::Rounding::ZERO,
            cpu_percent_color(pct as u16),
        );
    }

    // Tooltip: identify the core under the cursor + its percent. We compute
    // the label first (cheap, off the hover position) and attach via
    // `Response::on_hover_text` so egui handles tooltip placement, fade-in,
    // and dismissal automatically.
    let label = response.hover_pos().and_then(|pos| {
        let x_in_rect = (pos.x - rect.left()).max(0.0);
        let idx = (x_in_rect / stride) as usize;
        if idx < cores {
            Some(format!("Core {idx}: {}%", percents[idx]))
        } else {
            None
        }
    });
    if let Some(text) = label {
        let _ = response.on_hover_text(text);
    }
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
        ui.colored_label(
            state_color,
            egui::RichText::new(format!("● {state_text}")).strong(),
        );

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

/// Permanent activity strip — last ~5 engine actions in one horizontal
/// scroller at the bottom. Mirrors the Status tab's Recent Activity, but
/// compact and always visible regardless of which tab is open.
pub(crate) fn render_activity_strip(ui: &mut egui::Ui, recent: &[String]) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("ACTIVITY")
                .color(theme::p().text_muted)
                .size(10.0)
                .strong(),
        );
        ui.add_space(8.0);
        if recent.is_empty() {
            ui.colored_label(theme::p().text_muted, "no events yet");
            return;
        }
        egui::ScrollArea::horizontal()
            .max_width(f32::INFINITY)
            .show(ui, |ui| {
                for (i, line) in recent.iter().enumerate() {
                    if i > 0 {
                        ui.colored_label(theme::p().text_muted, "·");
                    }
                    let color = if line.contains("probalance") {
                        theme::p().warning
                    } else if line.contains("game-x3d") {
                        theme::p().accent
                    } else {
                        theme::p().text
                    };
                    ui.colored_label(color, line);
                }
            });
    });
}

/// Recent activity feed. Treats consecutive identical lines as one (with a
/// "× N" suffix) so the user sees signal not noise. Most recent first.
#[allow(dead_code)] // superseded by the Round-3 compact activity card; retained
pub(crate) fn render_recent_activity(ui: &mut egui::Ui, recent: &[String]) {
    if recent.is_empty() {
        ui.colored_label(theme::p().text_muted, "No activity yet.");
        return;
    }
    theme::card().show(ui, |ui| {
        egui::ScrollArea::vertical()
            .max_height(220.0)
            .show(ui, |ui| {
                // De-dupe consecutive identical labels. The engine fires
                // ForegroundChanged on every focus shift, including spurious
                // self-shifts when popups close — without dedup the feed
                // gets noisy fast.
                let mut last: Option<&String> = None;
                let mut count = 1usize;
                let mut grouped: Vec<(&String, usize)> = Vec::new();
                for label in recent.iter().rev() {
                    if last == Some(label) {
                        count += 1;
                    } else {
                        if let Some(l) = last {
                            grouped.push((l, count));
                        }
                        last = Some(label);
                        count = 1;
                    }
                }
                if let Some(l) = last {
                    grouped.push((l, count));
                }

                for (i, (label, n)) in grouped.iter().enumerate() {
                    if i > 0 {
                        ui.add_space(2.0);
                    }
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("›")
                                .color(theme::p().text_dim)
                                .monospace(),
                        );
                        ui.label(*label);
                        if *n > 1 {
                            ui.colored_label(
                                theme::p().text_muted,
                                egui::RichText::new(format!("× {n}")).small(),
                            );
                        }
                    });
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
