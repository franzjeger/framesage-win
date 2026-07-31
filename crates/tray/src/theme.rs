//! Custom egui theme for framesage-tray.
//!
//! Egui's defaults look like a dev-tool demo. This module tunes Visuals,
//! Style spacing, and TextStyle font sizes into something that reads as a
//! polished desktop utility rather than an immediate-mode debug window.
//!
//! **Theme-aware palette (Round 3 renovation).** Colors are no longer
//! bare consts — they live on a [`Palette`] selected by [`Theme`]. The
//! active palette is a process-global set once per [`apply`] call; UI
//! code reads a `Copy` snapshot via [`p`] (`theme::p().accent`, etc.).
//! The dark palette is the original navy/charcoal + cyan; the light
//! palette derives from the same hues (GitHub Primer light) so semantic
//! green/amber/red stay legible on white.

use eframe::egui;
use egui::{Color32, FontFamily, FontId, Rounding, Stroke, TextStyle, Visuals};
use serde::{Deserialize, Serialize};
use std::sync::RwLock;

/// Which theme the UI is painted in. Persisted per-user in tray-prefs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

impl Theme {
    pub fn palette(self) -> Palette {
        match self {
            Theme::Dark => Palette::DARK,
            Theme::Light => Palette::LIGHT,
        }
    }
}

/// The full semantic color set for one theme. `Copy` (17 × `Color32` =
/// 68 bytes) so `theme::p()` snapshots are free at every call site.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub surface_hover: Color32,
    pub surface_active: Color32,
    pub border: Color32,
    pub border_muted: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub text_dim: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub error: Color32,
    /// Secondary data-series color (memory sparkline, second chart line).
    pub series_secondary: Color32,
    /// Foreground for text/glyphs painted on an `accent`-filled surface
    /// (primary buttons, the tab-index badge). NOT `bg` — in light mode
    /// `bg` is near-white and would vanish on the accent fill.
    pub on_accent: Color32,
    /// Chart gridlines / faint separators — between `border_muted` and
    /// `bg`. Consumed by the Sessions chart-restyle slice; defined here
    /// with the rest of the palette so both themes carry it.
    #[allow(dead_code)]
    pub grid: Color32,
    /// egui `extreme_bg_color` (text-edit wells, deepest recess).
    pub extreme_bg: Color32,
    /// egui `code_bg_color`.
    pub code_bg: Color32,
}

impl Palette {
    /// Original dark navy/charcoal + cyan accent.
    pub const DARK: Palette = Palette {
        bg: Color32::from_rgb(0x0e, 0x11, 0x18),
        surface: Color32::from_rgb(0x16, 0x1b, 0x22),
        surface_hover: Color32::from_rgb(0x21, 0x27, 0x33),
        surface_active: Color32::from_rgb(0x2a, 0x30, 0x3d),
        border: Color32::from_rgb(0x30, 0x36, 0x3d),
        border_muted: Color32::from_rgb(0x26, 0x2b, 0x33),
        text: Color32::from_rgb(0xc9, 0xd1, 0xd9),
        text_muted: Color32::from_rgb(0x8b, 0x94, 0x9e),
        text_dim: Color32::from_rgb(0x6e, 0x76, 0x81),
        accent: Color32::from_rgb(0x58, 0xa6, 0xff),
        accent_hover: Color32::from_rgb(0x79, 0xb8, 0xff),
        success: Color32::from_rgb(0x3f, 0xb9, 0x50),
        warning: Color32::from_rgb(0xd2, 0x99, 0x22),
        error: Color32::from_rgb(0xf8, 0x51, 0x49),
        series_secondary: Color32::from_rgb(0x8c, 0x5a, 0xc8),
        on_accent: Color32::from_rgb(0x0e, 0x11, 0x18),
        grid: Color32::from_rgb(0x1c, 0x22, 0x2b),
        extreme_bg: Color32::from_rgb(0x08, 0x0b, 0x10),
        code_bg: Color32::from_rgb(0x0a, 0x0e, 0x14),
    };

    /// Light palette (GitHub Primer light) — same hues, darker semantic
    /// colors so success/warning/error text stays legible on white.
    pub const LIGHT: Palette = Palette {
        bg: Color32::from_rgb(0xf6, 0xf8, 0xfa),
        surface: Color32::from_rgb(0xff, 0xff, 0xff),
        surface_hover: Color32::from_rgb(0xef, 0xf2, 0xf5),
        surface_active: Color32::from_rgb(0xea, 0xee, 0xf2),
        border: Color32::from_rgb(0xd0, 0xd7, 0xde),
        border_muted: Color32::from_rgb(0xea, 0xee, 0xf2),
        text: Color32::from_rgb(0x1f, 0x23, 0x28),
        text_muted: Color32::from_rgb(0x65, 0x6d, 0x76),
        text_dim: Color32::from_rgb(0x83, 0x8d, 0x97),
        accent: Color32::from_rgb(0x09, 0x69, 0xda),
        accent_hover: Color32::from_rgb(0x08, 0x60, 0xca),
        success: Color32::from_rgb(0x1a, 0x7f, 0x37),
        warning: Color32::from_rgb(0x9a, 0x67, 0x00),
        error: Color32::from_rgb(0xcf, 0x22, 0x2e),
        series_secondary: Color32::from_rgb(0x82, 0x50, 0xdf),
        on_accent: Color32::from_rgb(0xff, 0xff, 0xff),
        grid: Color32::from_rgb(0xea, 0xee, 0xf2),
        extreme_bg: Color32::from_rgb(0xea, 0xee, 0xf2),
        code_bg: Color32::from_rgb(0xf6, 0xf8, 0xfa),
    };
}

/// The active palette, swapped by [`apply`]. The UI runs single-threaded
/// but background IPC threads also reference `theme::p()` for status
/// coloring, so guard it with an `RwLock` (reads are uncontended).
static ACTIVE: RwLock<Palette> = RwLock::new(Palette::DARK);

/// A `Copy` snapshot of the active palette. Call at render time:
/// `theme::p().accent`, `theme::p().text_muted`, …
pub fn p() -> Palette {
    *ACTIVE.read().expect("theme palette lock poisoned")
}

// ─── Spacing scale ─────────────────────────────────────────────────────────────
//
// One vertical-rhythm scale so section gaps align to a grid instead of a
// scatter of magic `add_space` numbers. Theme-independent. Prefer these
// over literals.

pub const SP_XS: f32 = 4.0;
pub const SP_SM: f32 = 8.0;
pub const SP_MD: f32 = 12.0;
pub const SP_LG: f32 = 16.0;
pub const SP_XL: f32 = 24.0;

/// A translucent fill of `color` at the given alpha — the one place we
/// derive low-opacity accent/series fills, so callers stop hand-rolling
/// `Color32::from_rgba_premultiplied` inline.
pub fn fill_alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), alpha)
}

// ─── Apply ───────────────────────────────────────────────────────────────────

/// Install the framesage theme on this egui context for `theme`. Sets the
/// process-global active palette, then builds egui `Visuals` from it.
/// Idempotent — safe to call on every theme toggle.
pub fn apply(ctx: &egui::Context, theme: Theme) {
    let pal = theme.palette();
    *ACTIVE.write().expect("theme palette lock poisoned") = pal;

    let mut visuals = match theme {
        Theme::Dark => Visuals::dark(),
        Theme::Light => Visuals::light(),
    };

    visuals.override_text_color = Some(pal.text);
    visuals.panel_fill = pal.bg;
    visuals.window_fill = pal.surface;
    visuals.window_stroke = Stroke::new(1.0_f32, pal.border);
    visuals.window_rounding = Rounding::same(6.0);

    visuals.extreme_bg_color = pal.extreme_bg;
    visuals.faint_bg_color = pal.surface;
    visuals.code_bg_color = pal.code_bg;

    visuals.selection.bg_fill = fill_alpha(pal.accent, 0x55);
    visuals.selection.stroke = Stroke::new(1.0_f32, pal.accent);

    visuals.hyperlink_color = pal.accent;
    visuals.warn_fg_color = pal.warning;
    visuals.error_fg_color = pal.error;

    // Widgets baseline. Egui 0.28 calls the field `rounding`, not `corner_radius`.
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = pal.surface;
    widgets.noninteractive.weak_bg_fill = pal.surface;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, pal.border);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, pal.text);
    widgets.noninteractive.rounding = Rounding::same(4.0);

    widgets.inactive.bg_fill = pal.surface;
    widgets.inactive.weak_bg_fill = pal.surface;
    widgets.inactive.bg_stroke = Stroke::new(1.0_f32, pal.border);
    widgets.inactive.fg_stroke = Stroke::new(1.0_f32, pal.text);
    widgets.inactive.rounding = Rounding::same(4.0);

    widgets.hovered.bg_fill = pal.surface_hover;
    widgets.hovered.weak_bg_fill = pal.surface_hover;
    widgets.hovered.bg_stroke = Stroke::new(1.0_f32, pal.accent);
    widgets.hovered.fg_stroke = Stroke::new(1.0_f32, pal.text);
    widgets.hovered.rounding = Rounding::same(4.0);

    widgets.active.bg_fill = pal.surface_active;
    widgets.active.weak_bg_fill = pal.surface_active;
    widgets.active.bg_stroke = Stroke::new(1.0_f32, pal.accent_hover);
    widgets.active.fg_stroke = Stroke::new(1.5_f32, pal.text);
    widgets.active.rounding = Rounding::same(4.0);

    widgets.open.bg_fill = pal.surface_active;
    widgets.open.weak_bg_fill = pal.surface_active;
    widgets.open.bg_stroke = Stroke::new(1.0_f32, pal.border);
    widgets.open.fg_stroke = Stroke::new(1.0_f32, pal.text);
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
    let pal = p();
    egui::Frame::none()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .rounding(Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(14.0, 10.0))
}

/// Hero strip: full-width, slightly stronger fill, used for the at-a-glance
/// summary at the top of the Status tab. Bigger inner padding than `card()`
/// so the headline reads first.
pub fn hero() -> egui::Frame {
    let pal = p();
    egui::Frame::none()
        .fill(pal.surface_active)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::symmetric(16.0, 12.0))
}

/// Banner frame for stateful warnings / persistent overrides (manual mode,
/// admin-required, paused engine). Fill picks up the accent color at low
/// opacity; stroke is full-opacity for legibility against the panel.
pub fn banner(color: Color32) -> egui::Frame {
    egui::Frame::none()
        .fill(fill_alpha(color, 0x1f))
        .stroke(Stroke::new(1.0_f32, color))
        .rounding(Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
}

/// Pill-shaped status badge — small rounded frame with a colored background
/// at low opacity and a matching foreground stroke.
pub fn status_badge(color: Color32) -> egui::Frame {
    egui::Frame::none()
        .fill(fill_alpha(color, 0x33))
        .stroke(Stroke::new(1.0_f32, color))
        .rounding(Rounding::same(10.0))
        .inner_margin(egui::Margin::symmetric(8.0, 2.0))
}

/// Small uppercase section heading — quiet, used to label groups of fields
/// inside a card without competing visually with the actual values.
pub fn section_heading(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase())
        .small()
        .strong()
        .color(p().text_muted)
        .extra_letter_spacing(1.0)
}

/// Primary call-to-action button — filled accent, on-accent text — so
/// Continue / Finish / Save read as *the* action on a surface instead of
/// being signalled by accent-colored text on a default fill. Returns the
/// click response.
pub fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let pal = p();
    let text = egui::RichText::new(label).color(pal.on_accent).strong();
    ui.add(
        egui::Button::new(text)
            .fill(pal.accent)
            .stroke(Stroke::new(1.0_f32, pal.accent_hover)),
    )
}

/// Destructive button — error-tinted fill + stroke — so Remove / Delete
/// reads as dangerous rather than identical to a neutral secondary button.
pub fn danger_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let pal = p();
    let text = egui::RichText::new(label).color(pal.error).strong();
    ui.add(
        egui::Button::new(text)
            .fill(fill_alpha(pal.error, 0x22))
            .stroke(Stroke::new(1.0_f32, pal.error)),
    )
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
    let pal = p();
    let text = if selected {
        egui::RichText::new(label).strong().color(pal.text)
    } else {
        egui::RichText::new(label).color(pal.text_muted)
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
        painter.rect_filled(rect, Rounding::same(0.0), pal.surface_active);
    } else if response.hovered() {
        painter.rect_filled(rect, Rounding::same(0.0), pal.surface_hover);
    }

    // Centre the label inside the slot.
    let text_pos = rect.center() - galley.size() * 0.5;
    painter.galley(text_pos, galley, pal.text);

    if selected {
        let underline = egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.bottom() - 2.0),
            egui::pos2(rect.right(), rect.bottom()),
        );
        painter.rect_filled(underline, Rounding::same(0.0), pal.accent);
    }

    response
}
