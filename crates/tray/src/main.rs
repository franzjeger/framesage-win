//! framesage-tray.exe — system-tray icon + monitor window.
//!
//! v0.2 ships a real persistent tray icon via `tray-icon`. Left-click toggles
//! the monitor window, right-click reveals a menu (*Open* / *Hide* / *Exit*).
//! Closing the window hides it to the tray rather than killing the process —
//! "Exit framesage tray" from the menu is the only way to actually quit.
//!
//! The window opens an IPC connection to the service on startup, subscribes
//! to events, and renders live status: active profile, foreground app, recent
//! profile-application events. The tray runs unprivileged: it uses the
//! status pipe (`PIPE_NAME_STATUS`), whose DACL admits Authenticated Users.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;

use framesage_ipc::{Event, ForegroundSnapshot, StatusSnapshot};
#[cfg(windows)]
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

#[derive(Default)]
struct AppState {
    connected: bool,
    last_error: Option<String>,
    status: Option<StatusSnapshot>,
    recent: Vec<RecentEvent>,
}

struct RecentEvent {
    label: String,
}

/// Signals raised by the tray icon's menu/click handlers, read by the egui
/// `update` loop on the next frame.
#[derive(Default, Clone)]
struct TrayCommands {
    show_window: Arc<AtomicBool>,
    hide_window: Arc<AtomicBool>,
    /// `true` once a quit was requested via the tray's *Exit* menu. The egui
    /// close-requested handler reads this to distinguish "user clicked the
    /// window X" (hide to tray) from "user clicked Exit" (actually quit).
    exit_requested: Arc<AtomicBool>,
}

struct FramesageApp {
    state: Arc<Mutex<AppState>>,
    commands: TrayCommands,
    /// Holding the tray icon for its lifetime — drop = icon disappears.
    /// `#[allow(dead_code)]` because we never read it after construction;
    /// the field exists purely to extend the icon's lifetime to match the
    /// app's.
    #[cfg(windows)]
    #[allow(dead_code)]
    tray: TrayIcon,
}

impl FramesageApp {
    fn new(_cc: &eframe::CreationContext<'_>, commands: TrayCommands) -> Self {
        let state = Arc::new(Mutex::new(AppState::default()));
        let bg_state = state.clone();
        std::thread::spawn(move || {
            background_loop(bg_state);
        });

        #[cfg(windows)]
        let tray = build_tray(&commands).expect("build tray icon");

        Self {
            state,
            commands,
            #[cfg(windows)]
            tray,
        }
    }
}

impl eframe::App for FramesageApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(std::time::Duration::from_millis(500));

        // ─── Tray command bridge ────────────────────────────────────────────
        //
        // The tray's menu/click handlers raise atomic flags from another
        // thread; consume them here on the egui thread where ViewportCommand
        // is valid.
        if self.commands.show_window.swap(false, Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if self.commands.hide_window.swap(false, Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        // ─── Close-to-tray ──────────────────────────────────────────────────
        //
        // The window's X button fires `close_requested`. If the user clicked
        // the tray's *Exit*, we let the close go through; otherwise we
        // intercept it, cancel the close, and hide the window to the tray.
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested {
            if self.commands.exit_requested.load(Ordering::Relaxed) {
                // Let the close propagate — the egui runtime will exit.
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }
        if self.commands.exit_requested.load(Ordering::Relaxed) && !close_requested {
            // Exit menu was clicked while the window may not even be open;
            // ensure we send a Close to actually quit.
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // ─── UI ─────────────────────────────────────────────────────────────
        let state = self.state.lock().unwrap();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("framesage");
            ui.separator();

            ui.horizontal(|ui| {
                ui.label("service:");
                if state.connected {
                    ui.colored_label(egui::Color32::from_rgb(80, 200, 120), "connected");
                } else {
                    ui.colored_label(egui::Color32::from_rgb(200, 80, 80), "disconnected");
                }
            });

            if let Some(err) = &state.last_error {
                ui.colored_label(egui::Color32::from_rgb(200, 120, 80), err);
            }

            ui.separator();

            if let Some(s) = &state.status {
                ui.label(format!("paused: {}", s.paused));
                ui.label(format!("rules: {}", s.policy.rules.len()));
                ui.label(format!("default profile: {}", s.policy.default_profile));
                match &s.foreground {
                    Some(fg) => render_foreground(ui, fg),
                    None => {
                        ui.label("foreground: <none>");
                    }
                }
                match &s.active_profile {
                    Some(p) => {
                        ui.label(format!("active profile: {}", p.id));
                        if !p.description.is_empty() {
                            ui.small(&p.description);
                        }
                    }
                    None => {
                        ui.label("active profile: <none>");
                    }
                }
            } else {
                ui.label("waiting for status…");
            }

            ui.separator();
            ui.heading("recent events");
            egui::ScrollArea::vertical().show(ui, |ui| {
                for ev in state.recent.iter().rev().take(20) {
                    ui.label(&ev.label);
                }
            });

            ui.separator();
            ui.small("Closing this window hides to tray. Right-click the tray icon to fully exit.");
        });
    }
}

fn render_foreground(ui: &mut egui::Ui, fg: &ForegroundSnapshot) {
    ui.group(|ui| {
        ui.label(format!("foreground pid: {}", fg.pid));
        ui.label(format!("exe: {}", fg.exe_name));
        if !fg.title.is_empty() {
            ui.label(format!("title: {}", fg.title));
        }
    });
}

// ─── Tray icon ───────────────────────────────────────────────────────────────

/// 32×32 hand-rolled framesage tray icon: a radial blue gradient on a
/// transparent background. Replaceable with a designed .ico later — keeping
/// it programmatic for now avoids shipping a binary asset for v0.2.
#[cfg(windows)]
fn build_icon() -> Icon {
    const SIZE: u32 = 32;
    let mut rgba: Vec<u8> = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let center = (SIZE as f32 - 1.0) / 2.0;
    let max_r = center;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let r = (dx * dx + dy * dy).sqrt() / max_r;
            if r <= 1.0 {
                let t = (1.0 - r).clamp(0.0, 1.0);
                // Soft anti-aliased edge: fade alpha in the outer 6% of radius.
                let alpha = if r > 0.94 {
                    ((1.0 - r) / 0.06 * 255.0).clamp(0.0, 255.0) as u8
                } else {
                    255
                };
                let red = (30.0 + 30.0 * t) as u8;
                let green = (90.0 + 70.0 * t) as u8;
                let blue = (140.0 + 100.0 * t) as u8;
                rgba.extend_from_slice(&[red, green, blue, alpha]);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("hand-rolled icon is valid RGBA")
}

/// IDs of the dynamically-created menu items so the background event thread
/// can route events back to the right action without comparing strings.
#[cfg(windows)]
struct TrayMenuIds {
    open: MenuId,
    hide: MenuId,
    exit: MenuId,
}

#[cfg(windows)]
fn build_tray(commands: &TrayCommands) -> anyhow::Result<TrayIcon> {
    let menu = Menu::new();
    let open = MenuItem::new("Open window", true, None);
    let hide = MenuItem::new("Hide window", true, None);
    let sep = PredefinedMenuItem::separator();
    let exit = MenuItem::new("Exit framesage tray", true, None);

    menu.append(&open)?;
    menu.append(&hide)?;
    menu.append(&sep)?;
    menu.append(&exit)?;

    let ids = TrayMenuIds {
        open: open.id().clone(),
        hide: hide.id().clone(),
        exit: exit.id().clone(),
    };

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_icon(build_icon())
        .with_tooltip("framesage — foreground policy supervisor")
        .build()?;

    // tray-icon delivers MenuEvent and TrayIconEvent via global crossbeam
    // receivers. Bridge to our atomic flags from dedicated threads. The
    // receivers are static references; cloning them is cheap and gives the
    // spawned threads owned handles.
    let cmds_menu = commands.clone();
    let menu_rx = MenuEvent::receiver().clone();
    std::thread::Builder::new()
        .name("framesage-tray-menu".into())
        .spawn(move || {
            while let Ok(ev) = menu_rx.recv() {
                if ev.id == ids.open {
                    cmds_menu.show_window.store(true, Ordering::Relaxed);
                } else if ev.id == ids.hide {
                    cmds_menu.hide_window.store(true, Ordering::Relaxed);
                } else if ev.id == ids.exit {
                    cmds_menu.exit_requested.store(true, Ordering::Relaxed);
                }
            }
        })?;

    // Left-click toggles the window. tray-icon emits TrayIconEvent::Click
    // with `button: MouseButton::Left, button_state: Up` on click release.
    let cmds_click = commands.clone();
    let click_rx = TrayIconEvent::receiver().clone();
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
                }
            }
        })?;

    Ok(tray)
}

// ─── IPC client ──────────────────────────────────────────────────────────────

#[cfg(windows)]
fn background_loop(state: Arc<Mutex<AppState>>) {
    // Simple blocking client using the std synchronous named-pipe support via
    // `std::fs::OpenOptions`. The pipe path is documented to work with
    // CreateFile semantics under the hood.
    loop {
        match try_connect_and_serve(state.clone()) {
            Ok(()) => {}
            Err(e) => {
                let mut s = state.lock().unwrap();
                s.connected = false;
                s.last_error = Some(format!("{e:#}"));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
}

#[cfg(windows)]
fn try_connect_and_serve(state: Arc<Mutex<AppState>>) -> anyhow::Result<()> {
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};

    // The tray only ever sends Status + Subscribe — both read-only — so
    // we open the status pipe. That pipe's ACL grants Authenticated Users
    // access, so the tray works without elevation. (The admin pipe would
    // refuse an unprivileged caller at the OS layer.)
    //
    // FILE_FLAG_OVERLAPPED is not set; we get blocking semantics.
    let pipe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(framesage_ipc::PIPE_NAME_STATUS)?;
    {
        let mut s = state.lock().unwrap();
        s.connected = true;
        s.last_error = None;
    }

    let mut writer = pipe.try_clone()?;
    let mut reader = BufReader::new(pipe);

    // Get an initial status snapshot.
    let mut buf = serde_json::to_vec(&framesage_ipc::Request::Status)?;
    buf.push(b'\n');
    writer.write_all(&buf)?;
    writer.flush()?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if let Ok(framesage_ipc::Response::Status(snap)) =
        serde_json::from_str::<framesage_ipc::Response>(&line)
    {
        state.lock().unwrap().status = Some(*snap);
    }

    // Then subscribe to events.
    let mut buf = serde_json::to_vec(&framesage_ipc::Request::Subscribe)?;
    buf.push(b'\n');
    writer.write_all(&buf)?;
    writer.flush()?;

    line.clear();
    while reader.read_line(&mut line)? > 0 {
        if let Ok(event) = serde_json::from_str::<framesage_ipc::Event>(&line) {
            let label = match &event {
                Event::ForegroundChanged {
                    foreground,
                    profile,
                } => format!(
                    "{} → {} (pid {})",
                    foreground.exe_name, profile, foreground.pid
                ),
                Event::Paused => "paused".into(),
                Event::Resumed => "resumed".into(),
            };
            let mut s = state.lock().unwrap();
            s.recent.push(RecentEvent { label });
            if let (Event::ForegroundChanged { foreground, .. }, Some(snap)) =
                (&event, s.status.as_mut())
            {
                snap.foreground = Some(foreground.clone());
            }
        }
        line.clear();
    }
    Ok(())
}

#[cfg(not(windows))]
fn background_loop(state: Arc<Mutex<AppState>>) {
    let mut s = state.lock().unwrap();
    s.last_error = Some("tray UI only operates against a Windows service".into());
}

// ─── main ────────────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    let commands = TrayCommands::default();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 480.0])
            .with_title("framesage")
            .with_close_button(true),
        ..Default::default()
    };

    let cmds_for_app = commands.clone();
    eframe::run_native(
        "framesage",
        options,
        Box::new(move |cc| Ok(Box::new(FramesageApp::new(cc, cmds_for_app)))),
    )
}
