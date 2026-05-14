//! framesage-tray.exe — egui-based monitor window.
//!
//! v0.1 ships a plain window (not yet a true system-tray icon — the tray icon
//! and minimize-to-tray come in v0.2; doing it well needs `tray-icon` and a
//! winit event loop bridge that's worth getting right rather than rushing).
//!
//! The window opens an IPC connection to the service on startup, subscribes
//! to events, and renders live status: active profile, foreground app,
//! recent profile-application events.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use eframe::egui;

use framesage_ipc::{Event, ForegroundSnapshot, StatusSnapshot};

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

struct FramesageApp {
    state: Arc<Mutex<AppState>>,
}

impl FramesageApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let state = Arc::new(Mutex::new(AppState::default()));
        let bg_state = state.clone();
        std::thread::spawn(move || {
            background_loop(bg_state);
        });
        Self { state }
    }
}

impl eframe::App for FramesageApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(std::time::Duration::from_millis(500));
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
            // Re-poll status would be nice; instead just patch what we know.
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

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 480.0])
            .with_title("framesage"),
        ..Default::default()
    };
    eframe::run_native(
        "framesage",
        options,
        Box::new(|cc| Ok(Box::new(FramesageApp::new(cc)))),
    )
}
