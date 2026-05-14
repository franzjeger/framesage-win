//! Custom egui theme for framesage-tray.
//!
//! Egui's defaults look like a dev-tool demo. This module tunes Visuals,
//! Style spacing, and TextStyle font sizes into something that reads as a
//! polished desktop utility rather than an immediate-mode debug window.
//!
//! Palette is a dark navy/charcoal with a cyan-blue accent — chosen to feel
//! performance/tooling-coded without being aggressively gamer-RGB. All
//! semantic colors (success/warning/error/muted) are exported so the rest
//! of the UI can stop hand-rolling Color32::from_rgb everywhere.

use eframe::egui;
use egui::{Color32, FontFamily, FontId, Rounding, Stroke, TextStyle, Visuals};

// ─── Palette ─────────────────────────────────────────────────────────────────

pub const BG: Color32 = Color32::from_rgb(0x0e, 0x11, 0x18);
pub const SURFACE: Color32 = Color32::from_rgb(0x16, 0x1b, 0x22);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x21, 0x27, 0x33);
pub const SURFACE_ACTIVE: Color32 = Color32::from_rgb(0x2a, 0x30, 0x3d);
pub const BORDER: Color32 = Color32::from_rgb(0x30, 0x36, 0x3d);

pub const TEXT: Color32 = Color32::from_rgb(0xc9, 0xd1, 0xd9);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x8b, 0x94, 0x9e);
#[allow(dead_code)] // available for future "(stub)" / footnote rendering
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x6e, 0x76, 0x81);

pub const ACCENT: Color32 = Color32::from_rgb(0x58, 0xa6, 0xff);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0x79, 0xb8, 0xff);

pub const SUCCESS: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
pub const WARNING: Color32 = Color32::from_rgb(0xd2, 0x99, 0x22);
pub const ERROR: Color32 = Color32::from_rgb(0xf8, 0x51, 0x49);

// ─── Apply ───────────────────────────────────────────────────────────────────

/// Install the framesage theme on this egui context. Idempotent — calling
/// it more than once just re-applies the same values.
pub fn apply(ctx: &egui::Context) {
    let mut visuals = Visuals::dark();

    visuals.override_text_color = Some(TEXT);
    visuals.panel_fill = BG;
    visuals.window_fill = SURFACE;
    visuals.window_stroke = Stroke::new(1.0, BORDER);
    visuals.window_rounding = Rounding::same(6.0);

    visuals.extreme_bg_color = Color32::from_rgb(0x08, 0x0b, 0x10);
    visuals.faint_bg_color = SURFACE;
    visuals.code_bg_color = Color32::from_rgb(0x0a, 0x0e, 0x14);

    visuals.selection.bg_fill = Color32::from_rgba_premultiplied(0x58, 0xa6, 0xff, 0x55);
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);

    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = WARNING;
    visuals.error_fg_color = ERROR;

    // Widgets baseline. Egui 0.28 calls the field `rounding`, not `corner_radius`.
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = SURFACE;
    widgets.noninteractive.weak_bg_fill = SURFACE;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.noninteractive.rounding = Rounding::same(4.0);

    widgets.inactive.bg_fill = SURFACE;
    widgets.inactive.weak_bg_fill = SURFACE;
    widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.inactive.rounding = Rounding::same(4.0);

    widgets.hovered.bg_fill = SURFACE_HOVER;
    widgets.hovered.weak_bg_fill = SURFACE_HOVER;
    widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.hovered.rounding = Rounding::same(4.0);

    widgets.active.bg_fill = SURFACE_ACTIVE;
    widgets.active.weak_bg_fill = SURFACE_ACTIVE;
    widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_HOVER);
    widgets.active.fg_stroke = Stroke::new(1.5, TEXT);
    widgets.active.rounding = Rounding::same(4.0);

    widgets.open.bg_fill = SURFACE_ACTIVE;
    widgets.open.weak_bg_fill = SURFACE_ACTIVE;
    widgets.open.bg_stroke = Stroke::new(1.0, BORDER);
    widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.open.rounding = Rounding::same(4.0);

    ctx.set_visuals(visuals);

    // Style: spacing, fonts.
    let mut style = (*ctx.style()).clone();
    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(8.0, 6.0);
    s.button_padding = egui::vec2(10.0, 4.0);
    s.window_margin = egui::Margin::same(10.0);
    s.indent = 18.0;
    s.icon_width = 14.0;
    s.icon_spacing = 6.0;
    s.combo_height = 220.0;

    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(18.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(13.5, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(13.5, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(11.5, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.5, FontFamily::Monospace),
    );

    ctx.set_style(style);
}

/// Standard card frame: surface background, subtle border, rounded corners,
/// generous inner padding. Use for the discrete "blocks" of UI inside a tab.
pub fn card() -> egui::Frame {
    egui::Frame::none()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, BORDER))
        .rounding(Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(14.0, 10.0))
}

/// Hero strip: full-width, slightly stronger fill, used for the at-a-glance
/// summary at the top of the Status tab. Bigger inner padding than `card()`
/// so the headline reads first.
pub fn hero() -> egui::Frame {
    egui::Frame::none()
        .fill(SURFACE_ACTIVE)
        .stroke(Stroke::new(1.0, BORDER))
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(16.0, 12.0))
}

/// Banner frame for stateful warnings / persistent overrides (manual mode,
/// admin-required, paused engine). Fill picks up the accent color at low
/// opacity; stroke is full-opacity for legibility against the dark panel.
pub fn banner(color: Color32) -> egui::Frame {
    let bg = Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 0x1f);
    egui::Frame::none()
        .fill(bg)
        .stroke(Stroke::new(1.0, color))
        .rounding(Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
}

/// Pill-shaped status badge — small rounded frame with a colored background
/// at low opacity and a matching foreground stroke.
pub fn status_badge(color: Color32) -> egui::Frame {
    let bg = Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 0x33);
    egui::Frame::none()
        .fill(bg)
        .stroke(Stroke::new(1.0, color))
        .rounding(Rounding::same(10.0))
        .inner_margin(egui::Margin::symmetric(8.0, 2.0))
}

/// Small uppercase section heading — quiet, used to label groups of fields
/// inside a card without competing visually with the actual values.
pub fn section_heading(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase())
        .small()
        .strong()
        .color(TEXT_MUTED)
        .extra_letter_spacing(1.0)
}

/// Process Lasso–style tab button. Renders a chunky labelled rectangle with a
/// 2-pixel accent underline when selected. Returns the click response so the
/// caller can drive its own selection state — keeps this widget composable
/// without baking in a particular `Tab` enum.
///
/// Visual states:
/// * **selected**   — surface-active fill, accent underline, full-strength text
/// * **hovered**    — surface-hover fill, muted underline
/// * **idle**       — transparent, muted text
pub fn tab_button(ui: &mut egui::Ui, label: &str, selected: bool) -> egui::Response {
    let text = if selected {
        egui::RichText::new(label).strong().color(TEXT)
    } else {
        egui::RichText::new(label).color(TEXT_MUTED)
    };
    let galley = egui::WidgetText::RichText(text).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Button,
    );
    let padding = egui::vec2(12.0, 8.0);
    let desired = galley.size() + padding * 2.0;
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());

    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, Rounding::same(0.0), SURFACE_ACTIVE);
    } else if response.hovered() {
        painter.rect_filled(rect, Rounding::same(0.0), SURFACE_HOVER);
    }

    // Centre the label inside the slot.
    let text_pos = rect.center() - galley.size() * 0.5;
    painter.galley(text_pos, galley, TEXT);

    if selected {
        let underline = egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.bottom() - 2.0),
            egui::pos2(rect.right(), rect.bottom()),
        );
        painter.rect_filled(underline, Rounding::same(0.0), ACCENT);
    }

    response
}
