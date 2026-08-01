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
#[allow(dead_code)] // part of the spacing scale; currently unreferenced
pub const SP_XL: f32 = 24.0;

// ─── Metrics (design Round 3, EGUI_SPEC §1) ──────────────────────────────────
//
// Every size the renovation specifies, in logical points. Call sites use
// these instead of literals so the same primitive is the same size on
// every tab — the spec's §5.3 "per-site drift" failure mode.

pub struct Metrics;

#[allow(dead_code)] // full scale carried; not every constant has a call site yet
impl Metrics {
    // Type scale
    pub const TEXT_BODY: f32 = 13.0;
    pub const TEXT_SMALL: f32 = 12.5;
    pub const TEXT_TINY: f32 = 12.0;
    pub const TEXT_SECTION: f32 = 10.5;
    pub const TEXT_CARD_VALUE: f32 = 15.0;
    pub const TEXT_HERO: f32 = 16.0;
    pub const TEXT_MONO: f32 = 12.0;
    pub const TEXT_PILL: f32 = 11.0;

    // Radii
    pub const R_CARD: f32 = 8.0;
    pub const R_BUTTON: f32 = 6.0;
    pub const R_PILL: f32 = 10.0;

    // Card geometry
    pub const CARD_PAD_X: f32 = 12.0;
    pub const CARD_PAD_Y: f32 = 10.0;
    pub const HERO_PAD_X: f32 = 14.0;
    pub const HERO_PAD_Y: f32 = 12.0;
    pub const CARD_GAP: f32 = 10.0;
    pub const PAGE_PAD: f32 = 12.0;

    // Chrome
    pub const TAB_PAD_X: f32 = 10.0;
    pub const TAB_PAD_Y: f32 = 8.0;
    pub const TAB_UNDERLINE: f32 = 2.0;
    pub const SPARKLINE_W: f32 = 90.0;
    pub const SPARKLINE_H: f32 = 18.0;

    // Table
    pub const ROW_H: f32 = 26.0;
    pub const ROW_H_2LINE: f32 = 34.0;
    pub const ROW_H_RULES: f32 = 30.0;
    pub const HEADER_H: f32 = 24.0;
    pub const GUTTER_W: f32 = 4.0;
    pub const CPU_BAR_W: f32 = 34.0;
    pub const CPU_BAR_H: f32 = 5.0;

    // Controls
    pub const BTN_PAD: egui::Vec2 = egui::vec2(12.0, 6.0);
    pub const CHIP_PAD: egui::Vec2 = egui::vec2(10.0, 3.0);
    pub const PILL_PAD: egui::Vec2 = egui::vec2(8.0, 2.0);
    pub const SEARCH_W: f32 = 300.0;
    pub const DOT: f32 = 6.0;
    pub const HERO_DOT: f32 = 9.0;
    pub const SLIDER_W: f32 = 140.0;
    pub const DRAG_VALUE: egui::Vec2 = egui::vec2(52.0, 22.0);

    // Sessions
    pub const SESSION_LIST_W: f32 = 280.0;
    pub const CHART_H: f32 = 140.0;

    /// Tint ratio for pill / chip fills (EGUI_SPEC §2.3 writes this as
    /// `color.gamma_multiply(0.13)`). We feed it to [`mix`] instead so the
    /// light palette gets a *lighter* tint rather than a dark blob —
    /// gamma_multiply always darkens, which is only right on dark.
    pub const TINT_PILL: f32 = 0.13;
    /// Same idea for the hero wash.
    pub const TINT_HERO: f32 = 0.07;
}

/// A translucent fill of `color` at the given alpha — the one place we
/// derive low-opacity accent/series fills, so callers stop hand-rolling
/// alpha math inline.
///
/// Uses `from_rgba_unmultiplied`, i.e. straight alpha: `alpha` is the
/// opacity you'd expect, and egui premultiplies. The previous
/// `from_rgba_premultiplied` was fed full-strength RGB with a low alpha,
/// which is the encoding for an almost-opaque *additive* color — so
/// every "10% tint" in the app (status badges, banners, the selection
/// fill, the sparkline area) painted as a near-solid block of color.
/// That, not the palette, is why tinted surfaces read as shouting.
pub fn fill_alpha(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

/// Opaque blend of `color` over `base` at `t` (0.0–1.0), mixed in sRGB
/// space — "10% of the accent, 90% of the panel" in the sense a designer
/// means it.
///
/// Why not just a translucent fill: egui's renderer blends in *linear*
/// space, where a bright accent at 10% alpha over a near-black panel
/// comes back out at roughly 30% after gamma encoding. The Round-3
/// paused hero was specified as a 10% amber wash and painted as an olive
/// block. Mixing ourselves and handing egui an opaque color sidesteps
/// the blend entirely, and stays correct in the light theme because we
/// mix over whatever the actual background is.
pub fn mix(color: Color32, base: Color32, t: f32) -> Color32 {
    let f = |a: u8, b: u8| (a as f32 * t + b as f32 * (1.0 - t)).clamp(0.0, 255.0) as u8;
    Color32::from_rgb(
        f(color.r(), base.r()),
        f(color.g(), base.g()),
        f(color.b(), base.b()),
    )
}

/// Filled status dot, painted rather than typeset. The bundled font has
/// no U+25CF glyph, so `RichText::new("●")` renders as a tofu box — the
/// dots on the hero, the activity rows, and the status bar were all
/// showing as little squares. Returns the response so callers can
/// attach hover text.
pub fn dot(ui: &mut egui::Ui, color: Color32, diameter: f32) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::hover());
    ui.painter()
        .circle_filled(rect.center(), diameter / 2.0, color);
    response
}

/// Hero status dot with the glow the mockup gives live states
/// (EGUI_SPEC §2.8): the solid dot plus a wider disc at 30 % alpha.
/// Idle heroes call [`dot`] instead — no glow when nothing is running.
pub fn dot_glow(ui: &mut egui::Ui, color: Color32, diameter: f32) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(diameter, diameter), egui::Sense::hover());
    let painter = ui.painter();
    painter.circle_filled(rect.center(), diameter * 0.9, fill_alpha(color, 0x4d));
    painter.circle_filled(rect.center(), diameter / 2.0, color);
    response
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
        .rounding(Rounding::same(Metrics::R_CARD))
        .inner_margin(egui::Margin::symmetric(
            Metrics::CARD_PAD_X,
            Metrics::CARD_PAD_Y,
        ))
}

/// `card()` plus the width claim (EGUI_SPEC §2.1). A bare `Frame`
/// shrink-wraps its content, which is the root of the "zero-width
/// text / scattered cards" bug class: a card in a sized column paints
/// only as wide as its longest line, and any right-aligned sibling
/// inside it gets nothing to align against. Prefer this over `card()`
/// wherever the card sits in a column or a grid.
pub fn card_full<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card()
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Hero strip: full-width, slightly stronger fill, used for the at-a-glance
/// summary at the top of the Status tab. Bigger inner padding than `card()`
/// so the headline reads first.
pub fn hero() -> egui::Frame {
    let pal = p();
    egui::Frame::none()
        .fill(pal.surface)
        .stroke(Stroke::new(1.0_f32, pal.border))
        .rounding(Rounding::same(Metrics::R_CARD))
        .inner_margin(egui::Margin::symmetric(
            Metrics::HERO_PAD_X,
            Metrics::HERO_PAD_Y,
        ))
}

/// Hero frame tinted by a semantic color — the Status tab's headline
/// state card (design Round 3 §3a). Same geometry as [`hero`] but the
/// fill/stroke pick up `color`: a low-opacity wash plus a ~45%-alpha
/// border, which reads as "this state is notable" without the
/// full-strength stroke of [`banner`] (that one is for transient
/// warnings and would shout next to a 19 px headline).
pub fn hero_tinted(color: Color32) -> egui::Frame {
    let pal = p();
    egui::Frame::none()
        .fill(mix(color, pal.bg, Metrics::TINT_HERO))
        .stroke(Stroke::new(1.0_f32, mix(color, pal.bg, 0.45)))
        .rounding(Rounding::same(Metrics::R_CARD))
        .inner_margin(egui::Margin::symmetric(
            Metrics::HERO_PAD_X,
            Metrics::HERO_PAD_Y,
        ))
}

/// Banner frame for stateful warnings / persistent overrides (manual mode,
/// admin-required, paused engine). Fill picks up the accent color at low
/// opacity; stroke is full-opacity for legibility against the panel.
pub fn banner(color: Color32) -> egui::Frame {
    egui::Frame::none()
        .fill(mix(color, p().bg, 0.13))
        .stroke(Stroke::new(1.0_f32, color))
        .rounding(Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
}

/// Pill-shaped status badge — small rounded frame with a colored background
/// at low opacity and a matching foreground stroke.
pub fn status_badge(color: Color32) -> egui::Frame {
    egui::Frame::none()
        .fill(mix(color, p().surface, Metrics::TINT_PILL))
        .stroke(Stroke::new(1.0_f32, color))
        .rounding(Rounding::same(Metrics::R_PILL))
        .inner_margin(egui::Margin::symmetric(
            Metrics::PILL_PAD.x,
            Metrics::PILL_PAD.y,
        ))
}

/// Status pill, complete (EGUI_SPEC §2.3). Text is the state color at
/// full alpha on the 13 %-tinted fill — never muted text inside a
/// colored pill (§5.4).
pub fn pill(ui: &mut egui::Ui, color: Color32, text: &str) -> egui::Response {
    status_badge(color)
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .size(Metrics::TEXT_PILL)
                    .color(color),
            );
        })
        .response
}

/// Small uppercase section heading — quiet, used to label groups of fields
/// inside a card without competing visually with the actual values.
pub fn section_heading(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase())
        .size(Metrics::TEXT_SECTION)
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
            .fill(mix(pal.error, pal.surface, 0.14))
            .stroke(Stroke::new(1.0_f32, pal.error)),
    )
}

/// Filter chip — a pill-shaped toggle (design Round 3 §3b/§3c). Active
/// reads as accent border + tinted fill + full-strength label; inactive
/// is a quiet outline. Returns the click response so the caller owns the
/// selection semantics (exclusive on Processes, multi on Activity).
pub fn chip(ui: &mut egui::Ui, label: &str, active: bool) -> egui::Response {
    let pal = p();
    let text = if active {
        egui::RichText::new(label)
            .size(Metrics::TEXT_TINY)
            .color(pal.accent)
            .strong()
    } else {
        egui::RichText::new(label)
            .size(Metrics::TEXT_TINY)
            .color(pal.text_muted)
    };
    let (fill, stroke) = if active {
        (
            mix(pal.accent, pal.bg, Metrics::TINT_PILL),
            Stroke::new(1.0_f32, pal.accent),
        )
    } else {
        (pal.surface, Stroke::new(1.0_f32, pal.border))
    };
    let prev = ui.spacing().button_padding;
    ui.spacing_mut().button_padding = Metrics::CHIP_PAD;
    let resp = ui.add(
        egui::Button::new(text)
            .fill(fill)
            .stroke(stroke)
            .rounding(Rounding::same(Metrics::R_PILL)),
    );
    ui.spacing_mut().button_padding = prev;
    resp
}

/// Slider row (EGUI_SPEC §2.5): fixed-width accent-filled track, then a
/// DragValue box, then the label — all on one line. The stock
/// `Slider::text()` form is what produced the detached-handle rows in
/// Settings: egui's default `slider_width` is a fixed 100 pt that
/// doesn't shrink, so in a narrow column the track collapses and leaves
/// the handle floating beside the number.
pub fn labeled_slider<N: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut N,
    range: std::ops::RangeInclusive<N>,
    label: &str,
) -> egui::Response {
    ui.horizontal(|ui| {
        let pal = p();
        ui.spacing_mut().slider_width = Metrics::SLIDER_W;
        ui.visuals_mut().selection.bg_fill = pal.accent;
        let mut resp = ui.add(egui::Slider::new(value, range).show_value(false));
        resp |= ui.add_sized(Metrics::DRAG_VALUE, egui::DragValue::new(value));
        ui.label(
            egui::RichText::new(label)
                .size(Metrics::TEXT_SMALL)
                .color(pal.text_muted),
        );
        resp
    })
    .inner
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
    let padding = egui::vec2(Metrics::TAB_PAD_X, Metrics::TAB_PAD_Y);
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
            egui::pos2(rect.left(), rect.bottom() - Metrics::TAB_UNDERLINE),
            egui::pos2(rect.right(), rect.bottom()),
        );
        painter.rect_filled(underline, Rounding::same(0.0), pal.accent);
    }

    response
}
