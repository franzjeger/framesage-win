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

use framesage_core::{AppMatch, AppRule, Policy, ProfileId};
use framesage_ipc::{Event, ForegroundSnapshot, Request, Response, StatusSnapshot};
#[cfg(windows)]
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

#[cfg(windows)]
mod win32;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Status,
    Rules,
}

/// State for the Rules-tab inline editor. The model is "edit a local clone
/// of the policy, then commit via SetPolicy on Save." This lets the user
/// batch multiple add/delete operations into a single round-trip and gives
/// them a clear Discard escape hatch if they change their mind.
#[derive(Default)]
struct RulesEditor {
    /// Local clone of the policy with any in-flight edits applied. `None`
    /// means "view mode" — the UI mirrors the service's current policy
    /// from the status snapshot. Lazily populated on first edit.
    draft: Option<Policy>,
    /// Currently editing the add/edit form for a specific rule (or a new
    /// one). `None` means no form is open.
    form: Option<RuleForm>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchKind {
    ExeName,
    PathContains,
    WindowTitleContains,
}

impl From<&AppMatch> for MatchKind {
    fn from(m: &AppMatch) -> Self {
        match m {
            AppMatch::ExeName(_) => MatchKind::ExeName,
            AppMatch::PathContains(_) => MatchKind::PathContains,
            AppMatch::WindowTitleContains(_) => MatchKind::WindowTitleContains,
        }
    }
}

/// Working buffer for an add-or-edit form. Mirrors `AppRule` with the
/// match variant split into (kind, value) so radio buttons + a text
/// field can drive it before being recombined on save.
struct RuleForm {
    /// `Some(i)` = editing rule at index `i` in the draft; `None` = adding.
    editing_index: Option<usize>,
    match_kind: MatchKind,
    match_value: String,
    profile_id: String,
    note: String,
}

struct FramesageApp {
    state: Arc<Mutex<AppState>>,
    commands: TrayCommands,
    /// `true` if this process has the elevated token (UAC-elevated launch or
    /// LocalSystem). Determines whether admin controls are enabled in the UI.
    elevated: bool,
    /// One-line status echo from the last admin button click (e.g. "paused"
    /// or "error: …"). Cleared after a few seconds by the egui repaint loop.
    last_action: Arc<Mutex<Option<String>>>,
    /// Active top-level tab.
    tab: Tab,
    /// Rules-tab editor state.
    rules: RulesEditor,
    /// Holding the tray icon for its lifetime — drop = icon disappears.
    /// `#[allow(dead_code)]` because we never read it after construction;
    /// the field exists purely to extend the icon's lifetime to match the
    /// app's.
    #[cfg(windows)]
    #[allow(dead_code)]
    tray: TrayIcon,
}

impl FramesageApp {
    fn new(_cc: &eframe::CreationContext<'_>, commands: TrayCommands, elevated: bool) -> Self {
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
            elevated,
            last_action: Arc::new(Mutex::new(None)),
            tab: Tab::default(),
            rules: RulesEditor::default(),
            #[cfg(windows)]
            tray,
        }
    }

    /// Dispatch a one-shot admin request on a background thread; on
    /// completion (success or failure) update `last_action` so the UI
    /// shows feedback. Returns immediately so the click handler doesn't
    /// block the egui frame.
    #[cfg(windows)]
    fn send_admin_request(&self, req: Request, label: &'static str) {
        let last_action = self.last_action.clone();
        std::thread::spawn(move || {
            let result = send_request_blocking(framesage_ipc::PIPE_NAME_ADMIN, &req);
            let msg = match result {
                Ok(Response::Ok) | Ok(Response::Status(_)) => format!("{label}: ok"),
                Ok(Response::Error { message }) => format!("{label}: error — {message}"),
                Err(e) => format!("{label}: error — {e}"),
            };
            *last_action.lock().unwrap() = Some(msg);
        });
    }

    #[cfg(not(windows))]
    fn send_admin_request(&self, _req: Request, _label: &'static str) {
        // No-op on non-Windows so this stub still compiles in cross-checks.
        *self.last_action.lock().unwrap() = Some("admin requests are Windows-only".to_string());
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
        // Hold the state lock only long enough to copy the snapshot; the
        // edit form needs &mut self.rules which conflicts with a long-held
        // immutable borrow of state.
        let (connected, last_error, status_snapshot, recent_events) = {
            let s = self.state.lock().unwrap();
            (
                s.connected,
                s.last_error.clone(),
                s.status.clone(),
                s.recent
                    .iter()
                    .rev()
                    .take(20)
                    .map(|e| e.label.clone())
                    .collect::<Vec<_>>(),
            )
        };

        // Header with title + tabs.
        egui::TopBottomPanel::top("framesage-header").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("framesage");
                ui.separator();
                ui.selectable_value(&mut self.tab, Tab::Status, "Status");
                ui.selectable_value(&mut self.tab, Tab::Rules, "Rules");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if connected {
                        ui.colored_label(egui::Color32::from_rgb(80, 200, 120), "● connected");
                    } else {
                        ui.colored_label(egui::Color32::from_rgb(200, 80, 80), "● disconnected");
                    }
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(err) = &last_error {
                ui.colored_label(egui::Color32::from_rgb(200, 120, 80), err);
            }

            match self.tab {
                Tab::Status => self.render_status_tab(ctx, ui, &status_snapshot, &recent_events),
                Tab::Rules => self.render_rules_tab(ui, &status_snapshot),
            }
        });
    }
}

impl FramesageApp {
    fn render_status_tab(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        status: &Option<StatusSnapshot>,
        recent: &[String],
    ) {
        if let Some(s) = status {
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

        // ─── Controls (admin-only) ──────────────────────────────────────
        //
        // Non-elevated tray: show a "🔒 Read-only" banner with a button
        // to relaunch elevated. Elevated tray: enable Pause/Resume/
        // Game-Mode-Off buttons that hit the admin pipe directly.
        #[cfg(windows)]
        {
            let paused = status.as_ref().map(|s| s.paused).unwrap_or(false);
            let active_profile = status.as_ref().and_then(|s| s.active_profile.as_ref());
            let in_game_mode = active_profile
                .map(|p| p.game_mode.is_some())
                .unwrap_or(false);

            if self.elevated {
                ui.colored_label(
                    egui::Color32::from_rgb(80, 200, 120),
                    "🔓 admin controls enabled",
                );
                ui.horizontal(|ui| {
                    if paused {
                        if ui.button("Resume engine").clicked() {
                            self.send_admin_request(Request::Resume, "resume");
                        }
                    } else if ui.button("Pause engine").clicked() {
                        self.send_admin_request(Request::Pause, "pause");
                    }
                    let gm_button = egui::Button::new("Game Mode off");
                    if ui.add_enabled(in_game_mode, gm_button).clicked() {
                        self.send_admin_request(Request::GameModeOff, "game-mode off");
                    }
                });
                if let Some(msg) = self.last_action.lock().unwrap().as_ref() {
                    ui.small(msg);
                }
            } else {
                ui.colored_label(
                    egui::Color32::from_rgb(200, 150, 80),
                    "🔒 read-only — admin pipe needs UAC",
                );
                if ui
                    .button("Enable controls (UAC)…")
                    .on_hover_text(
                        "Relaunch framesage-tray elevated so Pause/Resume/Game-Mode-Off work.",
                    )
                    .clicked()
                {
                    match win32::relaunch_as_admin() {
                        Ok(()) => {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            self.commands.exit_requested.store(true, Ordering::Relaxed);
                        }
                        Err(e) => {
                            *self.last_action.lock().unwrap() =
                                Some(format!("relaunch failed: {e}"));
                        }
                    }
                }
                if let Some(msg) = self.last_action.lock().unwrap().as_ref() {
                    ui.small(msg);
                }
            }
        }

        ui.separator();
        ui.heading("recent events");
        egui::ScrollArea::vertical().show(ui, |ui| {
            for label in recent {
                ui.label(label);
            }
        });

        ui.separator();
        ui.small("Closing this window hides to tray. Right-click the tray icon to fully exit.");
    }

    /// Render the Rules tab — view and edit `Policy::rules` via batched
    /// add/delete operations that commit on Save.
    fn render_rules_tab(&mut self, ui: &mut egui::Ui, status: &Option<StatusSnapshot>) {
        let Some(s) = status else {
            ui.label("waiting for status…");
            return;
        };

        if !self.elevated {
            ui.colored_label(
                egui::Color32::from_rgb(200, 150, 80),
                "🔒 read-only — switch to Status tab and click Enable controls to edit rules.",
            );
            ui.separator();
        }

        // Resolve the policy to display: the editor's draft if present,
        // otherwise the service's current policy. The view-vs-edit
        // distinction matters because the service can push a status
        // update mid-edit; we don't want that to clobber unsaved work.
        let displayed_policy = self.rules.draft.as_ref().unwrap_or(&s.policy).clone();
        let dirty = self.rules.draft.is_some();
        let profile_ids: Vec<ProfileId> = {
            let mut ids: Vec<_> = displayed_policy.profiles.keys().cloned().collect();
            ids.sort_by(|a, b| a.0.cmp(&b.0));
            ids
        };

        // ─── Toolbar ────────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            let add_enabled = self.elevated && self.rules.form.is_none();
            if ui
                .add_enabled(add_enabled, egui::Button::new("➕ Add rule"))
                .clicked()
            {
                let default_profile = profile_ids
                    .first()
                    .map(|id| id.0.clone())
                    .unwrap_or_else(|| s.policy.default_profile.0.clone());
                self.rules.form = Some(RuleForm {
                    editing_index: None,
                    match_kind: MatchKind::ExeName,
                    match_value: String::new(),
                    profile_id: default_profile,
                    note: String::new(),
                });
                if self.rules.draft.is_none() {
                    self.rules.draft = Some(s.policy.clone());
                }
            }

            let save_enabled = self.elevated && dirty && self.rules.form.is_none();
            if ui
                .add_enabled(save_enabled, egui::Button::new("💾 Save changes"))
                .clicked()
            {
                if let Some(draft) = self.rules.draft.take() {
                    self.send_admin_request(Request::SetPolicy { policy: draft }, "save policy");
                }
            }

            let discard_enabled = dirty && self.rules.form.is_none();
            if ui
                .add_enabled(discard_enabled, egui::Button::new("↩ Discard"))
                .clicked()
            {
                self.rules.draft = None;
                self.rules.form = None;
            }

            if dirty {
                ui.colored_label(egui::Color32::from_rgb(220, 170, 60), "● unsaved");
            }
        });

        if let Some(msg) = self.last_action.lock().unwrap().as_ref() {
            ui.small(msg);
        }

        ui.separator();

        // ─── Inline form (add or edit) ──────────────────────────────────────
        if let Some(form) = &mut self.rules.form {
            let mut commit = false;
            let mut cancel = false;
            ui.group(|ui| {
                ui.heading(if form.editing_index.is_some() {
                    "Edit rule"
                } else {
                    "Add rule"
                });
                ui.horizontal(|ui| {
                    ui.label("Match:");
                    ui.radio_value(&mut form.match_kind, MatchKind::ExeName, "exe name");
                    ui.radio_value(
                        &mut form.match_kind,
                        MatchKind::PathContains,
                        "path contains",
                    );
                    ui.radio_value(
                        &mut form.match_kind,
                        MatchKind::WindowTitleContains,
                        "title contains",
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Value:");
                    let hint = match form.match_kind {
                        MatchKind::ExeName => "e.g. bf6.exe",
                        MatchKind::PathContains => "e.g. Battlefield 6",
                        MatchKind::WindowTitleContains => "e.g. DEBUG",
                    };
                    ui.add(
                        egui::TextEdit::singleline(&mut form.match_value)
                            .hint_text(hint)
                            .desired_width(280.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Profile:");
                    egui::ComboBox::from_id_source("rule-profile-combo")
                        .selected_text(&form.profile_id)
                        .show_ui(ui, |ui| {
                            for id in &profile_ids {
                                ui.selectable_value(&mut form.profile_id, id.0.clone(), &id.0);
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label("Note:");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.note)
                            .hint_text("optional human note")
                            .desired_width(280.0),
                    );
                });
                ui.horizontal(|ui| {
                    let can_save =
                        !form.match_value.trim().is_empty() && !form.profile_id.trim().is_empty();
                    if ui
                        .add_enabled(can_save, egui::Button::new("Save"))
                        .clicked()
                    {
                        commit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });

            if commit {
                // form is a &mut so we have to drop the borrow before
                // mutating self.rules below.
                let new_rule = AppRule {
                    r#match: match form.match_kind {
                        MatchKind::ExeName => AppMatch::ExeName(form.match_value.trim().to_owned()),
                        MatchKind::PathContains => {
                            AppMatch::PathContains(form.match_value.trim().to_owned())
                        }
                        MatchKind::WindowTitleContains => {
                            AppMatch::WindowTitleContains(form.match_value.trim().to_owned())
                        }
                    },
                    profile: ProfileId(form.profile_id.trim().to_owned()),
                    note: form.note.trim().to_owned(),
                };
                let editing_index = form.editing_index;
                self.rules.form = None;
                let draft = self.rules.draft.get_or_insert_with(|| s.policy.clone());
                match editing_index {
                    Some(i) if i < draft.rules.len() => draft.rules[i] = new_rule,
                    _ => draft.rules.push(new_rule),
                }
            } else if cancel {
                self.rules.form = None;
            }

            ui.separator();
        }

        // ─── Rule list ──────────────────────────────────────────────────────
        let rules = displayed_policy.rules.clone();
        if rules.is_empty() {
            ui.label("(no rules — add one to map a foreground app to a profile)");
        } else {
            egui::ScrollArea::vertical().show(ui, |ui| {
                let mut delete_index: Option<usize> = None;
                let mut edit_index: Option<usize> = None;
                for (i, rule) in rules.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let (kind, value) = match &rule.r#match {
                            AppMatch::ExeName(s) => ("exe", s.as_str()),
                            AppMatch::PathContains(s) => ("path~", s.as_str()),
                            AppMatch::WindowTitleContains(s) => ("title~", s.as_str()),
                        };
                        ui.label(format!(
                            "{:6}  {} → {}{}",
                            kind,
                            value,
                            rule.profile,
                            if rule.note.is_empty() {
                                String::new()
                            } else {
                                format!("  ({})", rule.note)
                            }
                        ));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let actions_enabled = self.elevated && self.rules.form.is_none();
                            if ui
                                .add_enabled(actions_enabled, egui::Button::new("✖"))
                                .on_hover_text("Delete rule")
                                .clicked()
                            {
                                delete_index = Some(i);
                            }
                            if ui
                                .add_enabled(actions_enabled, egui::Button::new("✎"))
                                .on_hover_text("Edit rule")
                                .clicked()
                            {
                                edit_index = Some(i);
                            }
                        });
                    });
                }

                if let Some(i) = delete_index {
                    let draft = self.rules.draft.get_or_insert_with(|| s.policy.clone());
                    if i < draft.rules.len() {
                        draft.rules.remove(i);
                    }
                }
                if let Some(i) = edit_index {
                    let draft = self.rules.draft.get_or_insert_with(|| s.policy.clone());
                    if let Some(rule) = draft.rules.get(i).cloned() {
                        self.rules.form = Some(RuleForm {
                            editing_index: Some(i),
                            match_kind: MatchKind::from(&rule.r#match),
                            match_value: match rule.r#match {
                                AppMatch::ExeName(s)
                                | AppMatch::PathContains(s)
                                | AppMatch::WindowTitleContains(s) => s,
                            },
                            profile_id: rule.profile.0,
                            note: rule.note,
                        });
                    }
                }
            });
        }
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

/// One-shot blocking IPC: open the named pipe, send a single request,
/// read a single response, close. Used by admin button handlers; we
/// deliberately don't reuse a persistent connection because admin
/// operations are rare and the simpler per-call lifecycle is easier
/// to reason about than a long-lived sender.
#[cfg(windows)]
fn send_request_blocking(pipe_name: &str, req: &Request) -> anyhow::Result<Response> {
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};

    let pipe = OpenOptions::new().read(true).write(true).open(pipe_name)?;
    let mut writer = pipe.try_clone()?;
    let mut reader = BufReader::new(pipe);

    let mut buf = serde_json::to_vec(req)?;
    buf.push(b'\n');
    writer.write_all(&buf)?;
    writer.flush()?;

    let mut line = String::new();
    reader.read_line(&mut line)?;
    let resp: Response = serde_json::from_str(line.trim_end())?;
    Ok(resp)
}

// ─── main ────────────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    // Singleton + elevation handoff: if another tray is running, wait briefly
    // for it to exit (in case this is the elevated child taking over from a
    // non-elevated parent), then fail cleanly if it doesn't.
    #[cfg(windows)]
    let _singleton = match win32::acquire_singleton() {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("framesage-tray: {e}");
            return Ok(());
        }
    };

    #[cfg(windows)]
    let elevated = win32::is_elevated().unwrap_or(false);
    #[cfg(not(windows))]
    let elevated = false;

    let commands = TrayCommands::default();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([520.0, 480.0])
            .with_title(if elevated {
                "framesage (admin)"
            } else {
                "framesage"
            })
            .with_close_button(true),
        ..Default::default()
    };

    let cmds_for_app = commands.clone();
    eframe::run_native(
        "framesage",
        options,
        Box::new(move |cc| Ok(Box::new(FramesageApp::new(cc, cmds_for_app, elevated)))),
    )
}
