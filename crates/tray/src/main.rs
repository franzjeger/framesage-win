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

use framesage_core::{
    AppMatch, AppRule, CoreKind, CpuSelector, FocusAssistMode, GameModeActions, IoPriority,
    MemoryPriority, Policy, PowerPlanId, PowerThrottlingMode, PriorityClass, Profile, ProfileId,
};
use framesage_ipc::{Event, ForegroundSnapshot, Request, Response, StatusSnapshot};
#[cfg(windows)]
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

#[cfg(windows)]
mod win32;

mod theme;

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
    Profiles,
}

/// State for the Rules-tab inline editor. Holds only the open form; the
/// draft policy lives on `FramesageApp` so Rules and Profiles can edit
/// the same buffer.
#[derive(Default)]
struct RulesEditor {
    /// Currently editing the add/edit form for a specific rule (or a new
    /// one). `None` means no form is open.
    form: Option<RuleForm>,
}

/// State for the Profiles-tab inline editor. Like `RulesEditor`, the draft
/// itself lives on `FramesageApp` so a Save in either tab persists changes
/// made in both.
#[derive(Default)]
struct ProfilesEditor {
    /// Profile id currently in edit mode (only one at a time). `None`
    /// means all profiles are in view-only mode.
    editing_id: Option<String>,
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
    /// Shared policy draft. `None` = no in-flight edits, UI mirrors the
    /// service's current policy. Lazily populated on first edit in either
    /// the Rules tab or the Profiles tab. Save sends one SetPolicy.
    policy_draft: Option<Policy>,
    /// Rules-tab editor state.
    rules: RulesEditor,
    /// Profiles-tab editor state.
    profiles: ProfilesEditor,
    /// Holding the tray icon for its lifetime — drop = icon disappears.
    /// `#[allow(dead_code)]` because we never read it after construction;
    /// the field exists purely to extend the icon's lifetime to match the
    /// app's.
    #[cfg(windows)]
    #[allow(dead_code)]
    tray: TrayIcon,
}

impl FramesageApp {
    fn new(cc: &eframe::CreationContext<'_>, commands: TrayCommands, elevated: bool) -> Self {
        // Install our custom dark theme before the first frame renders so
        // the user never sees the egui-default flash.
        theme::apply(&cc.egui_ctx);

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
            policy_draft: None,
            rules: RulesEditor::default(),
            profiles: ProfilesEditor::default(),
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

        // Header with brand mark, tabs, connection badge. The OS title bar
        // already says "framesage" so the inline label is styled small and
        // colored — it's a brand mark, not a duplicate heading.
        egui::TopBottomPanel::top("framesage-header")
            .frame(
                egui::Frame::none()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                    .stroke(egui::Stroke::new(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("FRAMESAGE")
                            .color(theme::ACCENT)
                            .size(14.0)
                            .strong(),
                    );
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.selectable_value(&mut self.tab, Tab::Status, "Status");
                    ui.selectable_value(&mut self.tab, Tab::Rules, "Rules");
                    ui.selectable_value(&mut self.tab, Tab::Profiles, "Profiles");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (color, text) = if connected {
                            (theme::SUCCESS, "connected")
                        } else {
                            (theme::ERROR, "disconnected")
                        };
                        theme::status_badge(color).show(ui, |ui| {
                            ui.colored_label(color, text);
                        });
                    });
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(err) = &last_error {
                ui.colored_label(theme::ERROR, err);
            }

            match self.tab {
                Tab::Status => self.render_status_tab(ctx, ui, &status_snapshot, &recent_events),
                Tab::Rules => self.render_rules_tab(ui, &status_snapshot),
                Tab::Profiles => self.render_profiles_tab(ui, &status_snapshot),
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
            theme::card().show(ui, |ui| {
                kv_row(
                    ui,
                    "Engine",
                    if s.paused {
                        "paused".into()
                    } else {
                        "running".into()
                    },
                );
                kv_row(ui, "Rules", s.policy.rules.len().to_string());
                kv_row(ui, "Default profile", s.policy.default_profile.to_string());
                match &s.active_profile {
                    Some(p) => {
                        kv_row(ui, "Active profile", p.id.to_string());
                        if !p.description.is_empty() {
                            ui.add_space(2.0);
                            ui.colored_label(theme::TEXT_MUTED, &p.description);
                        }
                    }
                    None => {
                        kv_row(ui, "Active profile", "—".into());
                    }
                }
            });
            ui.add_space(6.0);
            match &s.foreground {
                Some(fg) => render_foreground(ui, fg),
                None => {
                    theme::card().show(ui, |ui| {
                        ui.colored_label(theme::TEXT_MUTED, "No foreground process detected.");
                    });
                }
            }
        } else {
            ui.colored_label(theme::TEXT_MUTED, "Waiting for the service to respond…");
        }

        ui.separator();

        // ─── Controls (admin-only) ──────────────────────────────────────
        //
        // Non-elevated tray: show a "Read-only" banner with a button
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
                ui.colored_label(theme::SUCCESS, "Admin controls enabled");
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
                ui.colored_label(theme::WARNING, "Read-only — admin pipe needs UAC");
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
                theme::WARNING,
                "Read-only — switch to Status tab and click Enable controls to edit rules.",
            );
            ui.separator();
        }

        // Resolve the policy to display: the editor's draft if present,
        // otherwise the service's current policy. The view-vs-edit
        // distinction matters because the service can push a status
        // update mid-edit; we don't want that to clobber unsaved work.
        let displayed_policy = self.policy_draft.as_ref().unwrap_or(&s.policy).clone();
        let dirty = self.policy_draft.is_some();
        let profile_ids: Vec<ProfileId> = {
            let mut ids: Vec<_> = displayed_policy.profiles.keys().cloned().collect();
            ids.sort_by(|a, b| a.0.cmp(&b.0));
            ids
        };

        // ─── Toolbar ────────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            let add_enabled = self.elevated && self.rules.form.is_none();
            if ui
                .add_enabled(add_enabled, egui::Button::new("Add rule"))
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
                if self.policy_draft.is_none() {
                    self.policy_draft = Some(s.policy.clone());
                }
            }

            let save_enabled = self.elevated && dirty && self.rules.form.is_none();
            if ui
                .add_enabled(save_enabled, egui::Button::new("Save changes"))
                .clicked()
            {
                if let Some(draft) = self.policy_draft.take() {
                    self.send_admin_request(Request::SetPolicy { policy: draft }, "save policy");
                }
            }

            let discard_enabled = dirty && self.rules.form.is_none();
            if ui
                .add_enabled(discard_enabled, egui::Button::new("Discard"))
                .clicked()
            {
                self.policy_draft = None;
                self.rules.form = None;
            }

            if dirty {
                theme::status_badge(theme::WARNING).show(ui, |ui| {
                    ui.colored_label(theme::WARNING, "unsaved");
                });
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
                let draft = self.policy_draft.get_or_insert_with(|| s.policy.clone());
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
                            "{:6}  {}  ->  {}{}",
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
                                .add_enabled(actions_enabled, egui::Button::new("Delete"))
                                .on_hover_text("Delete rule")
                                .clicked()
                            {
                                delete_index = Some(i);
                            }
                            if ui
                                .add_enabled(actions_enabled, egui::Button::new("Edit"))
                                .on_hover_text("Edit rule")
                                .clicked()
                            {
                                edit_index = Some(i);
                            }
                        });
                    });
                }

                if let Some(i) = delete_index {
                    let draft = self.policy_draft.get_or_insert_with(|| s.policy.clone());
                    if i < draft.rules.len() {
                        draft.rules.remove(i);
                    }
                }
                if let Some(i) = edit_index {
                    let draft = self.policy_draft.get_or_insert_with(|| s.policy.clone());
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

    /// Render the Profiles tab. Profiles are shown as collapsible cards;
    /// each can be flipped into edit mode via the Edit button. Editing
    /// targets the shared `policy_draft`, so a Save here commits both
    /// rule edits and profile edits in one round-trip.
    fn render_profiles_tab(&mut self, ui: &mut egui::Ui, status: &Option<StatusSnapshot>) {
        let Some(s) = status else {
            ui.label("waiting for status…");
            return;
        };

        let displayed_policy = self.policy_draft.as_ref().unwrap_or(&s.policy).clone();
        let dirty = self.policy_draft.is_some();

        // ─── Toolbar ────────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            let active_id = s.active_profile.as_ref().map(|p| p.id.0.as_str());
            ui.label("Profiles:");
            ui.label(format!("default = {}", displayed_policy.default_profile));
            if let Some(bg) = &displayed_policy.background_profile {
                ui.label(format!("background = {bg}"));
            }
            if let Some(active) = active_id {
                ui.label(format!("active = {active}"));
            }
        });
        ui.horizontal(|ui| {
            let save_enabled = self.elevated
                && dirty
                && self.profiles.editing_id.is_none()
                && self.rules.form.is_none();
            if ui
                .add_enabled(save_enabled, egui::Button::new("Save changes"))
                .clicked()
            {
                if let Some(draft) = self.policy_draft.take() {
                    self.send_admin_request(Request::SetPolicy { policy: draft }, "save policy");
                }
            }
            let discard_enabled = dirty && self.profiles.editing_id.is_none();
            if ui
                .add_enabled(discard_enabled, egui::Button::new("Discard"))
                .clicked()
            {
                self.policy_draft = None;
                self.profiles.editing_id = None;
            }
            if dirty {
                theme::status_badge(theme::WARNING).show(ui, |ui| {
                    ui.colored_label(theme::WARNING, "unsaved");
                });
            }
        });

        if !self.elevated {
            ui.colored_label(
                theme::WARNING,
                "Read-only — open the Status tab and click Enable controls.",
            );
        }
        if let Some(msg) = self.last_action.lock().unwrap().as_ref() {
            ui.small(msg);
        }
        ui.separator();

        let mut profile_ids: Vec<ProfileId> = displayed_policy.profiles.keys().cloned().collect();
        profile_ids.sort_by(|a, b| a.0.cmp(&b.0));

        // Collect the deferred mutations so we can apply them outside the
        // borrowing scope of the iteration over `profile_ids`.
        enum Op {
            EnterEdit(String),
            ExitEdit,
            // Boxed because Profile (with its Policy-shape internals) is
            // much larger than the other variants. Clippy enforces this so
            // we don't pay the cost on every Op enum value.
            UpdateProfile(String, Box<Profile>),
        }
        let mut ops: Vec<Op> = Vec::new();

        egui::ScrollArea::vertical().show(ui, |ui| {
            for id in &profile_ids {
                let Some(p) = displayed_policy.profiles.get(id) else {
                    continue;
                };
                let is_active = s.active_profile.as_ref().is_some_and(|ap| ap.id == *id);
                let is_editing = self.profiles.editing_id.as_deref() == Some(id.0.as_str());
                let header_text = if is_editing {
                    format!("{}  (editing)", id.0)
                } else if is_active {
                    format!("{}  (active)", id.0)
                } else {
                    id.0.clone()
                };
                let header_color = if is_active {
                    theme::ACCENT
                } else {
                    theme::TEXT
                };
                let header = egui::RichText::new(header_text).color(header_color);
                egui::CollapsingHeader::new(header)
                    .default_open(is_active || is_editing)
                    .id_source(("profile-card", id.0.as_str()))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let edit_enabled = self.elevated
                                && self.profiles.editing_id.is_none()
                                && self.rules.form.is_none();
                            if !is_editing
                                && ui
                                    .add_enabled(edit_enabled, egui::Button::new("Edit"))
                                    .clicked()
                            {
                                ops.push(Op::EnterEdit(id.0.clone()));
                            }
                            if is_editing && ui.button("Done").clicked() {
                                ops.push(Op::ExitEdit);
                            }
                        });
                        if is_editing {
                            let mut edited = p.clone();
                            render_profile_editor(ui, &mut edited);
                            if edited != *p {
                                ops.push(Op::UpdateProfile(id.0.clone(), Box::new(edited)));
                            }
                        } else {
                            render_profile_body(ui, p);
                        }
                    });
            }
        });

        for op in ops {
            match op {
                Op::EnterEdit(id) => {
                    self.profiles.editing_id = Some(id);
                    if self.policy_draft.is_none() {
                        self.policy_draft = Some(s.policy.clone());
                    }
                }
                Op::ExitEdit => {
                    self.profiles.editing_id = None;
                }
                Op::UpdateProfile(id, new_profile) => {
                    let draft = self.policy_draft.get_or_insert_with(|| s.policy.clone());
                    draft.profiles.insert(ProfileId(id), *new_profile);
                }
            }
        }
    }
}

fn render_foreground(ui: &mut egui::Ui, fg: &ForegroundSnapshot) {
    theme::card().show(ui, |ui| {
        kv_row(ui, "Foreground PID", fg.pid.to_string());
        kv_row(ui, "Executable", fg.exe_name.clone());
        if !fg.title.is_empty() {
            kv_row(ui, "Window title", fg.title.clone());
        }
    });
}

fn render_profile_body(ui: &mut egui::Ui, p: &Profile) {
    if !p.description.is_empty() {
        ui.label(&p.description);
        ui.add_space(4.0);
    }
    ui.group(|ui| {
        ui.heading("Per-process");
        kv_row(ui, "CPU sets", format_cpu_selector(p.cpu_sets.as_ref()));
        kv_row(
            ui,
            "Affinity mask",
            format_cpu_selector(p.affinity_mask.as_ref()),
        );
        kv_row(
            ui,
            "Power throttling",
            p.power_throttling
                .map(|v| format!("{v:?}"))
                .unwrap_or_else(|| "—".to_owned()),
        );
        kv_row(
            ui,
            "Priority class",
            p.priority_class
                .map(|v| format!("{v:?}"))
                .unwrap_or_else(|| "—".to_owned()),
        );
        kv_row(
            ui,
            "I/O priority",
            p.io_priority
                .map(|v| format!("{v:?}"))
                .unwrap_or_else(|| "—".to_owned()),
        );
        kv_row(
            ui,
            "Memory priority",
            p.memory_priority
                .map(|v| format!("{v:?}"))
                .unwrap_or_else(|| "—".to_owned()),
        );
        kv_row(ui, "Trim working set", p.trim_working_set.to_string());
    });

    if let Some(gm) = &p.game_mode {
        ui.add_space(4.0);
        ui.group(|ui| {
            ui.heading("Game Mode (system-wide)");
            kv_row(ui, "Hide taskbar", gm.hide_taskbar.to_string());
            kv_row(
                ui,
                "Stop services",
                if gm.stop_services.is_empty() {
                    "—".to_owned()
                } else {
                    gm.stop_services.join(", ")
                },
            );
            kv_row(
                ui,
                "Suspend processes",
                if gm.suspend_processes.is_empty() {
                    "—".to_owned()
                } else {
                    gm.suspend_processes.join(", ")
                },
            );
            kv_row(
                ui,
                "Power plan",
                gm.power_plan
                    .as_ref()
                    .map(|p| format!("{p:?}"))
                    .unwrap_or_else(|| "—".to_owned()),
            );
            kv_row(
                ui,
                "Focus assist",
                gm.focus_assist
                    .map(|m| format!("{m:?} (stub)"))
                    .unwrap_or_else(|| "—".to_owned()),
            );
            kv_row(
                ui,
                "Pause Windows Update",
                format!(
                    "{}{}",
                    gm.pause_windows_update,
                    if gm.pause_windows_update {
                        " (stub)"
                    } else {
                        ""
                    }
                ),
            );
        });
    } else {
        ui.add_space(4.0);
        ui.colored_label(
            theme::TEXT_MUTED,
            "Game Mode: not requested by this profile.",
        );
    }
}

fn kv_row(ui: &mut egui::Ui, key: &str, value: String) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 16.0],
            egui::Label::new(egui::RichText::new(key).monospace().weak()),
        );
        ui.label(value);
    });
}

/// Per-profile editor for the simple fields. CpuSelector (cpu_sets,
/// affinity_mask) and game_mode are shown read-only; their editors land
/// in a follow-up commit.
fn render_profile_editor(ui: &mut egui::Ui, p: &mut Profile) {
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
            |v| format!("{v:?}"),
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
            |v| format!("{v:?}"),
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
            |v| format!("{v:?}"),
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
            |v| format!("{v:?}"),
        );
        ui.horizontal(|ui| {
            ui.add_sized(
                [150.0, 16.0],
                egui::Label::new(egui::RichText::new("Trim working set").weak()),
            );
            ui.checkbox(&mut p.trim_working_set, "");
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

/// Editor for the `GameModeActions` block. Service stop/process suspend
/// lists are edited as multi-line text — one entry per line — so the user
/// doesn't have to mentally parse comma-separated strings while typing.
/// Both lists are gated by the engine's curated safe-list at apply time;
/// unknown ids are logged and skipped, so the user can't break things
/// with a typo here.
fn game_mode_editor(ui: &mut egui::Ui, gm: &mut GameModeActions) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 16.0],
            egui::Label::new(egui::RichText::new("Hide taskbar").weak()),
        );
        ui.checkbox(&mut gm.hide_taskbar, "");
    });

    option_combo(
        ui,
        "Focus assist",
        &mut gm.focus_assist,
        &[
            FocusAssistMode::Off,
            FocusAssistMode::PriorityOnly,
            FocusAssistMode::AlarmsOnly,
        ],
        |v| format!("{v:?} (stub)"),
    );

    string_list_edit(
        ui,
        "Stop services",
        &mut gm.stop_services,
        "One service short-name per line (e.g. SysMain, WSearch, DiagTrack).\nSafe-list gate at apply time — unknown ids are logged and skipped.",
    );

    string_list_edit(
        ui,
        "Suspend processes",
        &mut gm.suspend_processes,
        "One exe name per line (e.g. OneDrive.exe, Dropbox.exe).\nSafe-list gate at apply time — shell/kernel/AV/anti-cheat are denied.",
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

/// `Vec<String>` editor: each entry on its own line in a multi-line text
/// area. Empty lines are filtered out on save so the user can leave a
/// trailing blank while typing without polluting the policy.
fn string_list_edit(ui: &mut egui::Ui, label: &str, items: &mut Vec<String>, hint: &str) {
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

/// Option<PowerPlanId> editor. PowerPlanId::Custom carries an arbitrary
/// GUID string; the UI offers it via an "<custom>" entry that swaps in
/// a text field for the GUID. Most users want one of the four named
/// plans, so they're presented first.
fn power_plan_edit(ui: &mut egui::Ui, label: &str, plan: &mut Option<PowerPlanId>) {
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
fn option_combo<T>(
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
            None => "<unset>".to_owned(),
            Some(v) => fmt(v),
        };
        egui::ComboBox::from_id_source(("option-combo", label))
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                ui.selectable_value(current, None, "<unset>");
                for v in variants {
                    ui.selectable_value(current, Some(*v), fmt(v));
                }
            });
    });
}

fn format_cpu_selector(sel: Option<&CpuSelector>) -> String {
    match sel {
        None => "<unset>".to_owned(),
        Some(CpuSelector::All) => "All cores".to_owned(),
        Some(CpuSelector::Kind(k)) => format!("Kind: {k:?}"),
        Some(CpuSelector::Ccd(c)) => format!("CCD {c}"),
        Some(CpuSelector::CcdNot(c)) => format!("Everything except CCD {c}"),
        Some(CpuSelector::TopRanked(n)) => format!("Top {n} by CPPC rank"),
        Some(CpuSelector::Mask(m)) => format!("Mask 0x{m:016x}"),
    }
}

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
            Self::Unset => "<unset>",
            Self::All => "All cores",
            Self::Kind => "Kind (Performance/Efficiency/Cache)",
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
fn cpu_selector_edit(ui: &mut egui::Ui, label: &str, sel: &mut Option<CpuSelector>) {
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
                    .selected_text(format!("{k:?}"))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(k, CoreKind::Performance, "Performance");
                        ui.selectable_value(k, CoreKind::Efficiency, "Efficiency");
                        ui.selectable_value(k, CoreKind::Cache, "Cache");
                    });
            }
            Some(CpuSelector::Ccd(c)) | Some(CpuSelector::CcdNot(c)) => {
                ui.add(egui::DragValue::new(c).range(0..=15).speed(0.1));
            }
            Some(CpuSelector::TopRanked(n)) => {
                ui.add(egui::DragValue::new(n).range(1..=128).speed(0.25));
            }
            Some(CpuSelector::Mask(m)) => {
                // u128 isn't a DragValue primitive on egui 0.28. Render as a
                // hex text field with parse-on-change; on bad input we keep
                // the old value rather than zero out destructively.
                let mut buf = format!("{m:#x}");
                if ui.text_edit_singleline(&mut buf).changed() {
                    let trimmed = buf.trim().trim_start_matches("0x");
                    if let Ok(parsed) = u128::from_str_radix(trimmed, 16) {
                        *m = parsed;
                    }
                }
            }
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
                    "{} -> {} (pid {})",
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
