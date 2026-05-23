//! Item 3.6 (third slice) — FrameSage logo + tray-icon construction
//! lifted out of main.rs.
//!
//! What lives here:
//!
//! * `framesage_logo_rgba()` — synthesises the 64×64 FrameSage brand
//!   mark (dark navy disc, accent-cyan ring, stylised "F") as raw
//!   RGBA bytes. Used as the source for every visual that needs the
//!   logo: system-tray icon, eframe window icon, build-time .ico
//!   embedded in the .exe.
//! * `build_icon()` / `build_window_icon()` — wrap the raw RGBA in
//!   the tray-icon / eframe icon types.
//! * `TrayMenuIds` + `build_tray()` — construct the persistent tray
//!   icon with its full menu (Open/Hide, Pause/Resume, Game Mode
//!   off, View submenu, Open config folder, Edit policy.json, Exit)
//!   plus the two crossbeam-receiver threads that bridge menu /
//!   click events back into the egui runtime via `TrayCommands`.
//!
//! Three small graphics primitives — `smoothstep`, `in_rect`,
//! `over` — are kept here as private helpers since they're only
//! used by `framesage_logo_rgba`.

#![cfg_attr(not(windows), allow(dead_code))]

use eframe::egui;

#[cfg(windows)]
use std::sync::atomic::Ordering;
#[cfg(windows)]
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

#[cfg(windows)]
use crate::{Tab, TrayCommands};

// ─── FrameSage logo (raw RGBA8 synthesis) ────────────────────────────────────

/// Render the FrameSage brand mark at 64×64: dark navy disc, accent-cyan
/// ring, large stylised "F" sitting on the disc. Used as the source for
/// every visual that needs the logo — system-tray icon, eframe window icon,
/// build-time .ico for the .exe's taskbar / Alt-Tab thumbnail.
///
/// Returns (rgba_bytes, width, height). RGBA is row-major, top-down, 8-bit
/// per channel, premultiplied-by-alpha-friendly (egui and tray-icon both
/// expect plain RGBA8888).
pub(crate) fn framesage_logo_rgba() -> (Vec<u8>, u32, u32) {
    const SIZE: u32 = 64;
    let s = SIZE as f32;
    let center = (s - 1.0) / 2.0;

    // Palette pulled from theme so the icon reads as part of the same UI.
    let bg = [0x16u8, 0x1b, 0x22]; // theme::SURFACE
    let ring = [0x58u8, 0xa6, 0xff]; // theme::ACCENT
    let f_color = [0x9bu8, 0xca, 0xff]; // bright cyan/white for legibility

    // Geometric parameters. All in pixel space; tuned by eye.
    let disc_outer = 30.5_f32; // outer edge of the cyan ring
    let disc_inner = 27.5_f32; // inner edge of the ring (= disc fill boundary)

    // "F" glyph layout (bitmap-style; coordinates relative to the canvas):
    let f_left = 21.0;
    let f_right = 45.0;
    let f_top = 16.0;
    let f_bot = 48.0;
    let bar_thick = 7.0;
    let top_bar_h = 7.0;
    let mid_bar_y_top = 30.0;
    let mid_bar_h = 6.0;
    let mid_bar_right = 40.0;

    let mut rgba: Vec<u8> = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            let dx = fx - center - 0.5;
            let dy = fy - center - 0.5;
            let r = (dx * dx + dy * dy).sqrt();

            // Start fully transparent; we'll layer in disc → ring → glyph.
            let mut pixel = [0u8, 0, 0, 0];

            if r <= disc_outer {
                let disc_alpha = smoothstep(disc_outer + 0.5, disc_outer - 0.5, r);
                let ring_alpha = smoothstep(disc_inner - 0.5, disc_inner + 0.5, r).min(smoothstep(
                    disc_outer + 0.5,
                    disc_outer - 0.5,
                    r,
                ));

                // Disc fill.
                let a = (disc_alpha * 255.0).clamp(0.0, 255.0) as u8;
                pixel = [bg[0], bg[1], bg[2], a];

                // Ring overlay (replaces disc fill near the outer band).
                if ring_alpha > 0.0 {
                    let a = (ring_alpha * 255.0).clamp(0.0, 255.0) as u8;
                    pixel = over(pixel, [ring[0], ring[1], ring[2], a]);
                }

                // Glyph: "F" — a vertical bar + two horizontal bars.
                let on_vertical_bar = in_rect(fx, fy, f_left, f_top, f_left + bar_thick, f_bot);
                let on_top_bar = in_rect(fx, fy, f_left, f_top, f_right, f_top + top_bar_h);
                let on_mid_bar = in_rect(
                    fx,
                    fy,
                    f_left,
                    mid_bar_y_top,
                    mid_bar_right,
                    mid_bar_y_top + mid_bar_h,
                );
                if on_vertical_bar || on_top_bar || on_mid_bar {
                    pixel = over(pixel, [f_color[0], f_color[1], f_color[2], 255]);
                }
            }

            rgba.extend_from_slice(&pixel);
        }
    }

    (rgba, SIZE, SIZE)
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn in_rect(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
    px >= x0 && px < x1 && py >= y0 && py < y1
}

/// Source-over alpha compositing on a single pixel. Mutates `dst` toward
/// `src`. Both are straight (non-premultiplied) RGBA8888.
fn over(dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
    let sa = src[3] as f32 / 255.0;
    let da = dst[3] as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= f32::EPSILON {
        return [0, 0, 0, 0];
    }
    let blend = |s: u8, d: u8| -> u8 {
        let v = (s as f32 * sa + d as f32 * da * (1.0 - sa)) / out_a;
        v.clamp(0.0, 255.0) as u8
    };
    [
        blend(src[0], dst[0]),
        blend(src[1], dst[1]),
        blend(src[2], dst[2]),
        (out_a * 255.0).clamp(0.0, 255.0) as u8,
    ]
}

// ─── Icon wrappers ───────────────────────────────────────────────────────────

#[cfg(windows)]
pub(crate) fn build_icon() -> Icon {
    let (rgba, w, h) = framesage_logo_rgba();
    Icon::from_rgba(rgba, w, h).expect("hand-rolled icon is valid RGBA")
}

/// Build the eframe viewport icon shown in the title bar + taskbar. Mirrors
/// the tray-icon rendering so the brand reads consistently. Returns an
/// `egui::IconData` instead of `tray_icon::Icon` because eframe owns that
/// type; the pixel data is otherwise identical to `build_icon()`.
pub(crate) fn build_window_icon() -> egui::IconData {
    let (rgba, w, h) = framesage_logo_rgba();
    egui::IconData {
        rgba,
        width: w,
        height: h,
    }
}

// ─── Tray-icon menu construction ─────────────────────────────────────────────

/// IDs of the dynamically-created menu items so the background event thread
/// can route events back to the right action without comparing strings.
#[cfg(windows)]
struct TrayMenuIds {
    open: MenuId,
    hide: MenuId,
    pause: MenuId,
    resume: MenuId,
    game_mode_off: MenuId,
    view_processes: MenuId,
    view_status: MenuId,
    view_rules: MenuId,
    view_profiles: MenuId,
    open_config: MenuId,
    edit_policy: MenuId,
    exit: MenuId,
}

#[cfg(windows)]
pub(crate) fn build_tray(
    commands: &TrayCommands,
    egui_ctx: egui::Context,
) -> anyhow::Result<TrayIcon> {
    use tray_icon::menu::Submenu;

    let menu = Menu::new();
    let open = MenuItem::new("Open window", true, None);
    let hide = MenuItem::new("Hide window", true, None);

    let pause = MenuItem::new("Pause engine", true, None);
    let resume = MenuItem::new("Resume engine", true, None);
    let game_mode_off = MenuItem::new("Game Mode off (panic)", true, None);

    // View → submenu jumps to a specific tab AND opens the window. Letting
    // the user land directly on the tab they care about is the entire
    // reason for surfacing tabs in the tray.
    let view_processes = MenuItem::new("Processes", true, None);
    let view_status = MenuItem::new("Status", true, None);
    let view_rules = MenuItem::new("Rules", true, None);
    let view_profiles = MenuItem::new("Profiles", true, None);
    let view_submenu = Submenu::new("View", true);
    view_submenu.append(&view_processes)?;
    view_submenu.append(&view_status)?;
    view_submenu.append(&view_rules)?;
    view_submenu.append(&view_profiles)?;

    let open_config = MenuItem::new("Open config folder", true, None);
    let edit_policy = MenuItem::new("Edit policy.json", true, None);
    let exit = MenuItem::new("Exit FrameSage", true, None);

    menu.append(&open)?;
    menu.append(&hide)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&pause)?;
    menu.append(&resume)?;
    menu.append(&game_mode_off)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&view_submenu)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&open_config)?;
    menu.append(&edit_policy)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&exit)?;

    let ids = TrayMenuIds {
        open: open.id().clone(),
        hide: hide.id().clone(),
        pause: pause.id().clone(),
        resume: resume.id().clone(),
        game_mode_off: game_mode_off.id().clone(),
        view_processes: view_processes.id().clone(),
        view_status: view_status.id().clone(),
        view_rules: view_rules.id().clone(),
        view_profiles: view_profiles.id().clone(),
        open_config: open_config.id().clone(),
        edit_policy: edit_policy.id().clone(),
        exit: exit.id().clone(),
    };

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(build_icon())
        .with_tooltip("FrameSage — foreground policy supervisor")
        .build()?;

    // tray-icon delivers MenuEvent and TrayIconEvent via global crossbeam
    // receivers. Bridge to our atomic flags from dedicated threads. The
    // receivers are static references; cloning them is cheap and gives the
    // spawned threads owned handles.
    //
    // After raising a flag we ALWAYS call `egui_ctx.request_repaint()`. When
    // the window is hidden, eframe parks the message loop and `update()`
    // stops running — without an explicit wake, a tray click sets the flag
    // and nothing reads it. `egui::Context` is internally Arc-based, so
    // cloning it for each thread is cheap.
    let cmds_menu = commands.clone();
    let menu_rx = MenuEvent::receiver().clone();
    let wake_menu = egui_ctx.clone();
    std::thread::Builder::new()
        .name("framesage-tray-menu".into())
        .spawn(move || {
            while let Ok(ev) = menu_rx.recv() {
                // Match each known menu id to its TrayCommands flag. View →
                // tab items both flip `show_window` (so the window appears
                // even if hidden) AND set `jump_to_tab`. The egui loop sees
                // both on the next frame and applies them atomically.
                if ev.id == ids.open {
                    cmds_menu.show_window.store(true, Ordering::Relaxed);
                } else if ev.id == ids.hide {
                    cmds_menu.hide_window.store(true, Ordering::Relaxed);
                } else if ev.id == ids.pause {
                    cmds_menu.pause_engine.store(true, Ordering::Relaxed);
                } else if ev.id == ids.resume {
                    cmds_menu.resume_engine.store(true, Ordering::Relaxed);
                } else if ev.id == ids.game_mode_off {
                    cmds_menu.game_mode_off.store(true, Ordering::Relaxed);
                } else if ev.id == ids.view_processes {
                    *cmds_menu.jump_to_tab.lock() = Some(Tab::Processes);
                    cmds_menu.show_window.store(true, Ordering::Relaxed);
                } else if ev.id == ids.view_status {
                    *cmds_menu.jump_to_tab.lock() = Some(Tab::Status);
                    cmds_menu.show_window.store(true, Ordering::Relaxed);
                } else if ev.id == ids.view_rules {
                    *cmds_menu.jump_to_tab.lock() = Some(Tab::Rules);
                    cmds_menu.show_window.store(true, Ordering::Relaxed);
                } else if ev.id == ids.view_profiles {
                    *cmds_menu.jump_to_tab.lock() = Some(Tab::Profiles);
                    cmds_menu.show_window.store(true, Ordering::Relaxed);
                } else if ev.id == ids.open_config {
                    cmds_menu.open_config_folder.store(true, Ordering::Relaxed);
                } else if ev.id == ids.edit_policy {
                    cmds_menu.edit_policy.store(true, Ordering::Relaxed);
                } else if ev.id == ids.exit {
                    // Diagnostic checkpoint #1 (window-close bug investigation):
                    // first observable signal that the Exit path was triggered.
                    // If this line is missing from the diag log, the menu event
                    // never reached this thread.
                    tracing::info!(
                        "diag: tray Exit menu event received — setting \
                         exit_requested flag (checkpoint 1/7)"
                    );
                    cmds_menu.exit_requested.store(true, Ordering::Relaxed);
                } else {
                    continue;
                }
                wake_menu.request_repaint();
            }
        })?;

    // Left-click toggles the window. tray-icon emits TrayIconEvent::Click
    // with `button: MouseButton::Left, button_state: Up` on click release.
    let cmds_click = commands.clone();
    let click_rx = TrayIconEvent::receiver().clone();
    let wake_click = egui_ctx;
    std::thread::Builder::new()
        .name("framesage-tray-click".into())
        .spawn(move || {
            use tray_icon::{MouseButton, MouseButtonState};
            while let Ok(ev) = click_rx.recv() {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = ev
                {
                    // A redundant show signal when the window is already up
                    // just re-focuses it, which matches user expectation.
                    cmds_click.show_window.store(true, Ordering::Relaxed);
                    wake_click.request_repaint();
                }
            }
        })?;

    Ok(tray)
}
