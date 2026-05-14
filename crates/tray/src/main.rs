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
    AppMatch, AppRule, CoreKind, CpuSelector, GameModeActions, IoPriority, MemoryPriority, Policy,
    PowerPlanId, PowerThrottlingMode, PriorityClass, Profile, ProfileId,
};
use framesage_ipc::{Event, ForegroundSnapshot, Request, Response, StatusSnapshot};
#[cfg(windows)]
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

#[cfg(windows)]
mod icons;
#[cfg(windows)]
mod win32;

mod theme;

#[derive(Default)]
struct AppState {
    connected: bool,
    last_error: Option<String>,
    status: Option<StatusSnapshot>,
    recent: Vec<RecentEvent>,
    /// Latest snapshot of all processes from the service. Refreshed by
    /// `processes_poll_loop` at ~1 Hz. Empty until the first poll completes.
    processes: Vec<framesage_ipc::ProcessSnapshot>,
    /// Live system-wide metrics paired with the latest `processes` snapshot
    /// (CPU% / mem used / mem total). Refreshed each poll.
    system: framesage_ipc::SystemMetrics,
    /// Sliding ring buffer of the last `SYSTEM_HISTORY_LEN` (CPU%, mem%)
    /// samples — backs the sparkline in the permanent performance band at
    /// the top of every tab. Newest at the back.
    system_history: std::collections::VecDeque<(u8, u8)>,
}

/// Number of samples kept in `AppState.system_history`. 60 samples × 1 Hz
/// poll = 60 seconds of history, which matches Task Manager / PL's default
/// graph window. Cheap (120 bytes).
const SYSTEM_HISTORY_LEN: usize = 60;

struct RecentEvent {
    /// Wall-clock time the event was received. Rendered as `HH:MM:SS` in
    /// the Activity Log; the strip + Status-tab recent activity ignore it.
    at: std::time::SystemTime,
    /// Coarse category for filter chips + color-coding. `Other` is the
    /// catch-all so a new IPC event variant doesn't get silently lost.
    kind: EventKind,
    label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum EventKind {
    Foreground,
    Engine, // Paused / Resumed
    ProBalanceRestrained,
    ProBalanceRestored,
    /// Forward-compat catch-all. The IPC `Event` enum is exhaustively
    /// matched today; this variant is kept so a new event added on the
    /// service side without an immediate tray-side update still slots in
    /// somewhere instead of being silently dropped.
    #[allow(dead_code)]
    Other,
}

impl EventKind {
    fn display(self) -> &'static str {
        match self {
            EventKind::Foreground => "Foreground",
            EventKind::Engine => "Engine",
            EventKind::ProBalanceRestrained => "ProBalance demote",
            EventKind::ProBalanceRestored => "ProBalance restore",
            EventKind::Other => "Other",
        }
    }
    fn color(self) -> egui::Color32 {
        match self {
            EventKind::Foreground => theme::ACCENT,
            EventKind::Engine => theme::TEXT_MUTED,
            EventKind::ProBalanceRestrained => theme::WARNING,
            EventKind::ProBalanceRestored => theme::SUCCESS,
            EventKind::Other => theme::TEXT,
        }
    }
}

/// Signals raised by the tray icon's menu/click handlers, read by the egui
/// `update` loop on the next frame.
#[derive(Default, Clone)]
struct TrayCommands {
    show_window: Arc<AtomicBool>,
    hide_window: Arc<AtomicBool>,
    /// `true` while the main window is visible (set in `update()` every
    /// frame, cleared when a hide-to-tray completes). Background poller
    /// threads gate their `ctx.request_repaint()` calls on this so they
    /// don't burn CPU drawing into an invisible window. Visible-state is
    /// derived from "is update() running" rather than asking egui directly
    /// because eframe 0.28 doesn't expose viewport visibility to background
    /// threads.
    window_visible: Arc<AtomicBool>,
    /// `true` once a quit was requested via the tray's *Exit* menu. The egui
    /// close-requested handler reads this to distinguish "user clicked the
    /// window X" (hide to tray) from "user clicked Exit" (actually quit).
    exit_requested: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    Status,
    #[default]
    Processes,
    Activity,
    Rules,
    Profiles,
}

/// Live state for the Processes tab. Polled from the engine in the
/// background thread (along with the existing status poll); rendered by the
/// UI thread without holding the network for any longer than a clone.
struct ProcessesView {
    /// Most recent snapshot from `Request::ListProcesses`. Replaced wholesale
    /// each refresh — no diffing.
    rows: Vec<framesage_ipc::ProcessSnapshot>,
    /// Substring filter on exe name (case-insensitive, stripped on render).
    filter: String,
    /// Column the user picked to sort by. Defaults to CPU % descending —
    /// the convention every other process viewer uses, so users find the
    /// noisy process at the top without having to click anything.
    sort_by: Option<ProcessSortKey>,
    /// Descending if true, else ascending. Toggled by clicking the same
    /// column header twice.
    sort_desc: bool,
    /// PID of the row the user has clicked to inspect. When `Some(pid)` the
    /// table reserves a strip at the bottom for the detail panel; click the
    /// same row again (or the panel's × button) to clear.
    selected_pid: Option<u32>,
    /// Render mode: when true, rows nest under their parent PID with
    /// indentation + a ▶/▼ toggle, like Process Explorer's "All Processes"
    /// tab. Otherwise the table is a flat sortable list. Filter forces flat
    /// mode regardless — searching across the whole tree is more useful
    /// than searching within visible subtrees.
    tree_mode: bool,
    /// PIDs whose children are currently hidden in tree mode. Default is
    /// "all expanded" so a fresh session shows the full process forest;
    /// the user opts *out* of detail by collapsing branches they don't
    /// care about. Storing collapsed (not expanded) makes new processes
    /// inherit the "expanded" default automatically — no enumeration of
    /// every fresh PID required.
    collapsed: std::collections::HashSet<u32>,
    /// Height in pixels the detail panel occupies when something is
    /// selected. `None` means "use the default 210". Set when the user
    /// drags the splitter bar; persists for the rest of the session so
    /// the layout doesn't snap back every time a selection changes.
    detail_height: Option<f32>,
    /// Multi-selection set. Ctrl-click toggles a PID's membership;
    /// Shift-click extends the range from `last_clicked_pid` to the
    /// clicked PID (in current visual sort order); plain click clears
    /// the multi-set and falls through to single `selected_pid` handling.
    ///
    /// When the right-click context menu fires on a PID that's in
    /// `multi_selected`, all the listed actions (Set affinity, Apply
    /// profile, Set priority, Suspend, Resume, Terminate) dispatch
    /// against every PID in the set at once. Right-clicking outside
    /// the selection clears it and acts only on the right-clicked PID
    /// — same behavior Task Manager and Process Explorer use.
    multi_selected: std::collections::HashSet<u32>,
    /// PID anchor for Shift-click range selection. Updated on every
    /// plain or Ctrl click.
    last_clicked_pid: Option<u32>,
}

impl Default for ProcessesView {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            filter: String::new(),
            sort_by: Some(ProcessSortKey::Cpu),
            sort_desc: true,
            selected_pid: None,
            tree_mode: true,
            collapsed: std::collections::HashSet::new(),
            detail_height: None,
            multi_selected: std::collections::HashSet::new(),
            last_clicked_pid: None,
        }
    }
}

/// Activity Log tab UI state. The event buffer itself lives in
/// `AppState.recent` (so the IPC subscribe thread can push without
/// reaching into the UI tree); this struct just holds filter chip
/// toggles + the search-substring textbox.
struct ActivityLogView {
    /// Filter chip per kind — when `false` the kind is hidden from the
    /// table. Defaults: everything visible.
    show_foreground: bool,
    show_engine: bool,
    show_probalance_restrain: bool,
    show_probalance_restore: bool,
    show_other: bool,
    /// Substring search across the rendered label. Case-insensitive.
    filter: String,
}

impl Default for ActivityLogView {
    fn default() -> Self {
        Self {
            show_foreground: true,
            show_engine: true,
            show_probalance_restrain: true,
            show_probalance_restore: true,
            show_other: true,
            filter: String::new(),
        }
    }
}

/// State for the "are you sure?" modal that gates Terminate. Persisted on
/// `FramesageApp` so a re-render keeps the dialog open until the user
/// resolves it; cleared on Cancel or after the Confirm fires the IPC.
struct TerminateConfirm {
    pid: u32,
    exe_name: String,
}

/// State for the custom-mask affinity picker modal. Holds the working
/// mask as the user toggles individual CPUs; Apply turns it into a
/// `CpuSelector::Mask` IPC. The X3D / non-X3D quick presets in the
/// context menu don't go through the picker — they dispatch directly
/// with a `Kind(...)` selector so the engine resolves against the
/// live topology.
struct AffinityPicker {
    pid: u32,
    exe_name: String,
    /// Bit `i` set = CPU `i` allowed. Initialised to the process's
    /// current mask so the user can tweak rather than start fresh.
    mask: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessSortKey {
    ExeName,
    Pid,
    Cpu,
    Memory,
    Threads,
    Priority,
    Profile,
    Description,
    Company,
    User,
}

/// Visual classification for a Processes-tab row. Drives the colored leading
/// marker column, the exe-name color, and (indirectly) several glyph
/// prefixes. Order matters in `classify_row` — the cases are checked in
/// priority sequence so the most "interesting" state wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowState {
    /// The currently-focused application. Wins over every other state — the
    /// user wants the foreground row to be obvious at a glance.
    Foreground,
    /// ProBalance has demoted this process for contention relief.
    Restrained,
    /// A profile is currently applied (a rule matched, or manual mode).
    Managed,
    /// Plain background process — no special framesage involvement.
    Default,
}

fn classify_row(p: &framesage_ipc::ProcessSnapshot, foreground_pid: Option<u32>) -> RowState {
    if foreground_pid == Some(p.pid) {
        RowState::Foreground
    } else if p.restrained_by_probalance {
        RowState::Restrained
    } else if p.managed_profile.is_some() {
        RowState::Managed
    } else {
        RowState::Default
    }
}

/// Color for the leading vertical marker bar. `None` means "draw nothing"
/// (default rows stay clean so the colored rows pop visually).
fn row_marker_color(state: RowState) -> Option<egui::Color32> {
    match state {
        RowState::Foreground => Some(theme::ACCENT),
        RowState::Restrained => Some(theme::WARNING),
        RowState::Managed => Some(theme::SUCCESS),
        RowState::Default => None,
    }
}

/// Color for the exe-name column. Foreground gets the accent; ProBalance-
/// restrained gets warning. Managed keeps default text — the leading marker
/// and the Profile column already say "managed" loud enough; over-coloring
/// the name on every managed row makes the list feel screamy.
fn row_exe_color(state: RowState) -> egui::Color32 {
    match state {
        RowState::Foreground => theme::ACCENT,
        RowState::Restrained => theme::WARNING,
        RowState::Managed | RowState::Default => theme::TEXT,
    }
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
    /// Add-profile inline form. `Some(string)` = form is open with the id
    /// the user is typing. `None` = no form. Mutually exclusive with
    /// `editing_id` so the user never thinks they're editing two things
    /// at once.
    new_form: Option<String>,
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
    /// Processes-tab live view + UI state (filter, sort).
    processes: ProcessesView,
    /// Activity Log tab state — visible event-kind filter set + substring
    /// search. Independent of the `recent` event buffer itself (which lives
    /// in `AppState` so the network thread can push into it without
    /// touching the UI tree).
    activity: ActivityLogView,
    /// `Some` while the Terminate confirmation modal is open. Setting this
    /// from a context-menu click opens the modal; the modal's Confirm /
    /// Cancel buttons clear it and (for Confirm) fire the IPC.
    terminate_confirm: Option<TerminateConfirm>,
    /// `Some` while the custom-mask affinity picker is open. Opened from
    /// context-menu → "Set CPU affinity → Custom…". Apply / Cancel
    /// clear it.
    affinity_picker: Option<AffinityPicker>,
    /// Per-exe icon cache, populated lazily as rows render. Lives outside
    /// `ProcessesView` because the egui textures it holds want to be reused
    /// across tab switches (cheaper than re-extracting on tab return).
    #[cfg(windows)]
    icons: icons::IconCache,
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

        // Session-0 isolation work-around: the service runs as LocalSystem
        // in session 0 and `GetForegroundWindow` returns null cross-session.
        // The tray runs in the user's session, so it can see the foreground;
        // it reports every 250ms to the service via the admin pipe. The
        // engine prefers the report over its own (broken-from-session-0)
        // poll. Without this loop, no profile ever applies when the service
        // is properly installed.
        #[cfg(windows)]
        std::thread::Builder::new()
            .name("framesage-tray-foreground-reporter".into())
            .spawn(foreground_reporter_loop)
            .expect("spawn foreground reporter thread");

        // Processes-tab data source: one-shot `Request::ListProcesses` once
        // per second. Separate from the long-lived Subscribe connection in
        // `background_loop` so the event stream stays open and the snapshot
        // poll can use the status pipe's short-lived semantics.
        let proc_state = state.clone();
        let proc_ctx = cc.egui_ctx.clone();
        let proc_visible = commands.window_visible.clone();
        std::thread::Builder::new()
            .name("framesage-tray-processes-poller".into())
            .spawn(move || processes_poll_loop(proc_state, proc_ctx, proc_visible))
            .expect("spawn processes poller thread");

        // The tray runs in a separate thread; pass an egui::Context clone so
        // the menu/click handlers can wake the runtime. Without this, hiding
        // the window parks the message loop and tray clicks fall on the floor
        // — flags get set, but `update()` never runs to read them.
        #[cfg(windows)]
        let tray = build_tray(&commands, cc.egui_ctx.clone()).expect("build tray icon");

        Self {
            state,
            commands,
            elevated,
            last_action: Arc::new(Mutex::new(None)),
            tab: Tab::default(),
            policy_draft: None,
            rules: RulesEditor::default(),
            profiles: ProfilesEditor::default(),
            processes: ProcessesView::default(),
            activity: ActivityLogView::default(),
            terminate_confirm: None,
            affinity_picker: None,
            #[cfg(windows)]
            icons: icons::IconCache::new(),
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
                Ok(Response::Ok) | Ok(Response::Status(_)) | Ok(Response::Processes { .. }) => {
                    format!("{label}: ok")
                }
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
        // Idle keep-alive: 2 s instead of 500 ms. Background threads
        // (processes poller @ 1 Hz, IPC event subscribe on demand) drive
        // the actually-useful repaints via `ctx.request_repaint()`; this
        // value is the floor that catches anything those threads miss
        // (animation easing, hover-state transitions). 500 ms forced a
        // full table re-render twice a second even when nothing changed,
        // which on the Processes tab with 120 rows + per-row context_menu
        // adds up. 2 s leaves the UI feeling instant while cutting the
        // idle CPU floor roughly in half.
        ctx.request_repaint_after(std::time::Duration::from_secs(2));

        // Mark window visible for background threads. Cleared when we
        // process a hide-to-tray request below. Background poll threads
        // gate their `ctx.request_repaint()` calls on this — no point
        // burning CPU drawing a window the user can't see.
        self.commands.window_visible.store(true, Ordering::Relaxed);

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
            self.commands.window_visible.store(false, Ordering::Relaxed);
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
                self.commands.window_visible.store(false, Ordering::Relaxed);
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

        // Hand the latest processes snapshot into the local view buffer. We
        // do this under a separate short lock window so the render path can
        // iterate the rows without holding the mutex (the table walks a
        // virtualized list and we don't want a long borrow blocking the
        // poller thread on its 1 Hz refresh).
        {
            let s = self.state.lock().unwrap();
            if !s.processes.is_empty() || self.processes.rows.is_empty() {
                self.processes.rows = s.processes.clone();
            }
        }

        // Pull metrics + activity for the always-visible top/bottom strips.
        let (system_metrics, system_history, recent_for_strip) = {
            let s = self.state.lock().unwrap();
            (
                s.system.clone(),
                s.system_history.iter().copied().collect::<Vec<_>>(),
                s.recent
                    .iter()
                    .rev()
                    .take(5)
                    .map(|e| e.label.clone())
                    .collect::<Vec<_>>(),
            )
        };

        // Snapshot derived bits used by the shell panels. Computed before any
        // panel runs so the borrow checker doesn't have to navigate `&mut self`
        // through the closures below.
        let paused = status_snapshot.as_ref().map(|s| s.paused).unwrap_or(false);
        let manual_override = status_snapshot
            .as_ref()
            .and_then(|s| s.manual_override.clone());
        let process_count = self.processes.rows.len();
        let managed_count = self
            .processes
            .rows
            .iter()
            .filter(|p| p.managed_profile.is_some())
            .count();
        let last_action_text = self.last_action.lock().unwrap().clone();

        // ─── Menu bar ──────────────────────────────────────────────────────
        // File / Engine / View / Tools / Help on the left, FrameSage brand
        // mark + connection badge on the right. Matches the Process Lasso /
        // Process Hacker convention for a desktop utility.
        egui::TopBottomPanel::top("framesage-menubar")
            .frame(
                egui::Frame::none()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                    .stroke(egui::Stroke::new(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                self.render_menubar(ui, connected, paused);
            });

        // ─── Toolbar ───────────────────────────────────────────────────────
        // Iconic quick actions for the most common one-clicks: pause/resume
        // the engine, panic-revert Game Mode, open the policy file, jump
        // into the config folder. Stays light — anything that needs args
        // belongs in a menu, not here.
        egui::TopBottomPanel::top("framesage-toolbar")
            .frame(
                egui::Frame::none()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                    .stroke(egui::Stroke::new(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                self.render_toolbar(ui, paused, manual_override.is_some());
            });

        // ─── Tab strip ─────────────────────────────────────────────────────
        // Chunky bordered tabs with a 2px accent underline on the active one.
        // Below the toolbar so the visual hierarchy is menu → tools → tabs.
        egui::TopBottomPanel::top("framesage-tab-strip")
            .frame(
                egui::Frame::none()
                    .fill(theme::BG)
                    .inner_margin(egui::Margin {
                        left: 8.0,
                        right: 8.0,
                        top: 0.0,
                        bottom: 0.0,
                    })
                    .stroke(egui::Stroke::new(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                self.render_tab_strip(ui);
            });

        // ─── Performance band ──────────────────────────────────────────────
        // CPU% + Mem% + sliding 60s sparkline. Visible on every tab so the
        // "what is the box doing right now" answer is always one glance away.
        egui::TopBottomPanel::top("framesage-perf-band")
            .frame(
                egui::Frame::none()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(12.0, 6.0)),
            )
            .show(ctx, |ui| {
                render_perf_band(ui, &system_metrics, &system_history);
            });

        // ─── Status bar ────────────────────────────────────────────────────
        // Single thin line at the very bottom: engine state, process count,
        // app version, last-action echo. Bottom panels stack from the bottom
        // up by show-order — this one is shown FIRST so it lands on the
        // window's bottom edge with the activity strip above it.
        egui::TopBottomPanel::bottom("framesage-status-bar")
            .frame(
                egui::Frame::none()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(10.0, 3.0))
                    .stroke(egui::Stroke::new(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                render_status_bar(
                    ui,
                    connected,
                    paused,
                    manual_override.as_ref(),
                    process_count,
                    managed_count,
                    last_action_text.as_deref(),
                );
            });

        // ─── Activity strip ────────────────────────────────────────────────
        // Last 5 engine actions, horizontal scroller. Shown AFTER the status
        // bar so it lands above it.
        egui::TopBottomPanel::bottom("framesage-activity-strip")
            .frame(
                egui::Frame::none()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(12.0, 5.0)),
            )
            .show(ctx, |ui| {
                render_activity_strip(ui, &recent_for_strip);
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(err) = &last_error {
                ui.colored_label(theme::ERROR, err);
            }

            // Cap content width so the UI doesn't stretch into a single
            // 3440-pixel line of widgets on ultrawides. The earlier attempt
            // wrapped the body in a horizontal layout for centering, but a
            // horizontal sizes vertically to its content — that clipped any
            // tab whose list scrolled past the initial height (Rules,
            // Profiles). `set_max_width` keeps the vertical layout intact;
            // wide windows just leave empty space on the right.
            const MAX_CONTENT_WIDTH: f32 = 980.0;
            ui.set_max_width(MAX_CONTENT_WIDTH);
            self.render_active_tab(ctx, ui, &status_snapshot, &recent_events);
        });

        // Modal: Terminate confirmation. Renders on top of every panel; the
        // surrounding UI keeps drawing so the user can still see the row
        // they're about to kill. `Window` is the standard egui pattern for
        // a transient modal-ish overlay.
        self.render_terminate_confirm_modal(ctx);
        // Modal: custom-mask affinity picker. Same overlay treatment.
        self.render_affinity_picker_modal(ctx);
    }
}

impl FramesageApp {
    /// "Are you sure?" modal that gates `TerminateProcess`. Renders a small
    /// fixed-size window centered on the screen. Cancel closes without
    /// firing the IPC; Confirm fires `Request::TerminateProcess` and the
    /// engine kills the process with exit code 1.
    fn render_terminate_confirm_modal(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.terminate_confirm else {
            return;
        };
        let pid = pending.pid;
        let exe_name = pending.exe_name.clone();

        // Two output flags so we can drop the borrow before mutating self.
        let mut do_confirm = false;
        let mut do_cancel = false;

        let title = "Terminate process?";
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.add_space(4.0);
                ui.colored_label(
                    theme::ERROR,
                    egui::RichText::new("This is a hard kill.").strong(),
                );
                ui.add_space(2.0);
                ui.label(format!(
                    "FrameSage will call TerminateProcess on {exe_name} (pid {pid}). \
                     The process has no chance to save state. If it owns unsaved \
                     work or is a system process you'll see immediate side effects."
                ));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                    ui.add_space(4.0);
                    if ui
                        .add(egui::Button::new(
                            egui::RichText::new("Terminate")
                                .color(theme::ERROR)
                                .strong(),
                        ))
                        .clicked()
                    {
                        do_confirm = true;
                    }
                });
            });

        if do_confirm {
            self.send_admin_request(Request::TerminateProcess { pid }, "terminate process");
            self.terminate_confirm = None;
        } else if do_cancel {
            self.terminate_confirm = None;
        }
    }

    /// Custom-mask affinity picker. Grid of toggles (one per logical CPU)
    /// with the X3D / Cache cores highlighted so the user can see which
    /// half is the gaming side at a glance. Apply fires
    /// `Request::SetProcessAffinity { selector: Mask(...) }` with the
    /// composed bitmap; Cancel closes without firing.
    fn render_affinity_picker_modal(&mut self, ctx: &egui::Context) {
        let Some(picker) = self.affinity_picker.as_mut() else {
            return;
        };
        let pid = picker.pid;
        let exe_name = picker.exe_name.clone();

        let mut do_apply = false;
        let mut do_cancel = false;
        let mut new_mask = picker.mask;

        // CPU count: prefer per_core_cpu_percent length (live), fall back
        // to 32 as a sane default.
        let cpu_count = {
            let s = self.state.lock().unwrap();
            let n = s.system.per_core_cpu_percent.len();
            if n == 0 {
                32
            } else {
                n
            }
        };
        // Heuristic: assume the lower half of CPUs is the X3D / Cache CCD
        // on AMD dual-CCD parts. Correct for 9950X3D on this hardware
        // (we verified via L3 cache enumeration in `framesage topology`).
        // The Kind(Cache) preset uses the actual topology — this
        // highlight in the picker is just a visual hint.
        let x3d_lo = 0usize;
        let x3d_hi = cpu_count / 2;

        egui::Window::new("Set CPU affinity")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(540.0)
            .show(ctx, |ui| {
                ui.label(format!("{exe_name} (pid {pid})"));
                ui.add_space(4.0);
                ui.colored_label(
                    theme::TEXT_MUTED,
                    "Toggle individual CPUs. Highlighted column = likely X3D CCD.",
                );
                ui.add_space(8.0);

                // Quick presets row.
                ui.horizontal(|ui| {
                    if ui.button("X3D CCD only").clicked() {
                        let mut m = 0u64;
                        for i in x3d_lo..x3d_hi {
                            if i < 64 {
                                m |= 1u64 << i;
                            }
                        }
                        new_mask = m;
                    }
                    if ui.button("Non-X3D CCD only").clicked() {
                        let mut m = 0u64;
                        for i in x3d_hi..cpu_count {
                            if i < 64 {
                                m |= 1u64 << i;
                            }
                        }
                        new_mask = m;
                    }
                    if ui.button("All cores").clicked() {
                        let mut m = 0u64;
                        for i in 0..cpu_count {
                            if i < 64 {
                                m |= 1u64 << i;
                            }
                        }
                        new_mask = m;
                    }
                    if ui.button("Invert").clicked() {
                        let mut all = 0u64;
                        for i in 0..cpu_count {
                            if i < 64 {
                                all |= 1u64 << i;
                            }
                        }
                        new_mask ^= all;
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                // 16 toggles per row — fits 32 CPUs in two rows, 64 in
                // four, etc. Each toggle is a tiny labeled checkbox.
                const PER_ROW: usize = 16;
                let mut idx = 0usize;
                while idx < cpu_count {
                    ui.horizontal(|ui| {
                        for col in 0..PER_ROW {
                            let i = idx + col;
                            if i >= cpu_count {
                                break;
                            }
                            let bit = 1u64 << i.min(63);
                            let mut on = new_mask & bit != 0;
                            let in_x3d = i >= x3d_lo && i < x3d_hi;
                            let label = if in_x3d {
                                egui::RichText::new(format!("{i}")).color(theme::ACCENT)
                            } else {
                                egui::RichText::new(format!("{i}"))
                            };
                            if ui.checkbox(&mut on, label).changed() {
                                if on {
                                    new_mask |= bit;
                                } else {
                                    new_mask &= !bit;
                                }
                            }
                        }
                    });
                    idx += PER_ROW;
                }

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let count = new_mask.count_ones();
                    ui.label(format!("Mask: 0x{new_mask:016X}  ({count} cores)"));
                });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        do_cancel = true;
                    }
                    ui.add_space(4.0);
                    let apply_enabled = new_mask != 0;
                    let apply_btn = ui.add_enabled(
                        apply_enabled,
                        egui::Button::new(
                            egui::RichText::new("Apply").color(theme::ACCENT).strong(),
                        ),
                    );
                    if apply_btn.clicked() {
                        do_apply = true;
                    }
                    if !apply_enabled {
                        ui.colored_label(theme::ERROR, "Pick at least one CPU.");
                    }
                });
            });

        // Commit the working mask back to picker state so it persists across
        // re-renders while the modal is open.
        if let Some(p) = self.affinity_picker.as_mut() {
            p.mask = new_mask;
        }

        if do_apply {
            self.send_admin_request(
                Request::SetProcessAffinity {
                    pid,
                    selector: framesage_core::CpuSelector::Mask(new_mask as u128),
                },
                "set affinity",
            );
            self.affinity_picker = None;
        } else if do_cancel {
            self.affinity_picker = None;
        }
    }

    /// Render the menu bar. Items dispatch to the same `send_admin_request`
    /// helper the toolbar uses, or to a small set of shell-out helpers
    /// (`open_in_shell`) for file/folder/URL launches. View → tab items
    /// duplicate the tab strip below — that's deliberate; menu users and
    /// click-tab users both expect the option.
    fn render_menubar(&mut self, ui: &mut egui::Ui, connected: bool, paused: bool) {
        egui::menu::bar(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open policy file…").clicked() {
                    open_in_shell(&framesage_core::paths::policy_path().to_string_lossy());
                    ui.close_menu();
                }
                if ui.button("Open config folder").clicked() {
                    open_in_shell(&framesage_core::paths::config_dir().to_string_lossy());
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Exit FrameSage").clicked() {
                    self.commands.exit_requested.store(true, Ordering::Relaxed);
                    ui.close_menu();
                }
            });

            ui.menu_button("Engine", |ui| {
                let pause_label = if paused { "Resume" } else { "Pause" };
                if ui.button(pause_label).clicked() {
                    let req = if paused {
                        Request::Resume
                    } else {
                        Request::Pause
                    };
                    self.send_admin_request(req, if paused { "resume" } else { "pause" });
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Game Mode off (panic)").clicked() {
                    self.send_admin_request(Request::GameModeOff, "game-mode off");
                    ui.close_menu();
                }
                if ui.button("Show Game Mode journal").clicked() {
                    open_in_shell(
                        &framesage_core::paths::config_dir()
                            .join("game-mode.journal")
                            .to_string_lossy(),
                    );
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Clear manual override").clicked() {
                    self.send_admin_request(Request::ClearManualOverride, "clear manual override");
                    ui.close_menu();
                }
            });

            ui.menu_button("View", |ui| {
                let tabs = [
                    (Tab::Processes, "Processes"),
                    (Tab::Status, "Status"),
                    (Tab::Activity, "Activity"),
                    (Tab::Rules, "Rules"),
                    (Tab::Profiles, "Profiles"),
                ];
                for (t, label) in tabs {
                    let marker = if self.tab == t { "* " } else { "  " };
                    if ui.button(format!("{marker}{label}")).clicked() {
                        self.tab = t;
                        ui.close_menu();
                    }
                }
            });

            ui.menu_button("Tools", |ui| {
                if ui.button("Open policy file…").clicked() {
                    open_in_shell(&framesage_core::paths::policy_path().to_string_lossy());
                    ui.close_menu();
                }
                if ui.button("Open config folder").clicked() {
                    open_in_shell(&framesage_core::paths::config_dir().to_string_lossy());
                    ui.close_menu();
                }
                if ui.button("Run topology in terminal").clicked() {
                    // Run `framesage topology` from the same dir as the tray
                    // exe, in a new terminal window so the user can read the
                    // output. Best-effort: ignore failure.
                    spawn_framesage_subcommand("topology");
                    ui.close_menu();
                }
            });

            ui.menu_button("Help", |ui| {
                if ui.button("GitHub repository").clicked() {
                    open_in_shell("https://github.com/franzjeger/framesage-win");
                    ui.close_menu();
                }
                if ui.button("Report an issue").clicked() {
                    open_in_shell("https://github.com/franzjeger/framesage-win/issues");
                    ui.close_menu();
                }
                ui.separator();
                ui.label(format!("FrameSage v{}", env!("CARGO_PKG_VERSION")));
            });

            // Brand mark + connection badge on the right side of the bar.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (color, text) = if connected {
                    (theme::SUCCESS, "Connected")
                } else {
                    (theme::ERROR, "Disconnected")
                };
                theme::status_badge(color).show(ui, |ui| {
                    ui.colored_label(color, text);
                });
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("FrameSage")
                        .color(theme::ACCENT)
                        .strong(),
                );
            });
        });
    }

    /// Quick-action toolbar. Visible regardless of which tab is active.
    /// Buttons echo the most common menu choices for users who don't want to
    /// pop a menu just to pause the engine.
    fn render_toolbar(&mut self, ui: &mut egui::Ui, paused: bool, manual_active: bool) {
        ui.horizontal(|ui| {
            // Pause / Resume — text shifts based on engine state so the
            // button always reads as the next action. Plain ASCII labels —
            // egui's default font doesn't have triangle / pause-bar glyphs
            // (they render as empty boxes), and the verb alone is clear.
            let pause_label = if paused { "Resume" } else { "Pause" };
            let pause_color = if paused { theme::WARNING } else { theme::TEXT };
            if ui
                .add(egui::Button::new(
                    egui::RichText::new(pause_label).color(pause_color),
                ))
                .on_hover_text(if paused {
                    "Resume the engine — apply profiles on foreground change"
                } else {
                    "Pause the engine — stop applying anything until resumed"
                })
                .clicked()
            {
                let req = if paused {
                    Request::Resume
                } else {
                    Request::Pause
                };
                self.send_admin_request(req, if paused { "resume" } else { "pause" });
            }

            // Game Mode panic button. Always-on; idempotent if no session is
            // active.
            if ui
                .button("🎮 Game Mode off")
                .on_hover_text("Force-revert any active Game Mode session")
                .clicked()
            {
                self.send_admin_request(Request::GameModeOff, "game-mode off");
            }

            // Clear manual override is conditional — only worth surfacing
            // when manual mode is actually engaged.
            if manual_active
                && ui
                    .button("X Clear manual")
                    .on_hover_text("Leave manual mode; foreground apply returns to Rules")
                    .clicked()
            {
                self.send_admin_request(Request::ClearManualOverride, "clear manual override");
            }

            ui.separator();

            if ui
                .button("📂 Open config folder")
                .on_hover_text("Reveal the FrameSage config directory in Explorer")
                .clicked()
            {
                open_in_shell(&framesage_core::paths::config_dir().to_string_lossy());
            }

            if ui
                .button("📝 Edit policy")
                .on_hover_text("Open policy.json in the system editor")
                .clicked()
            {
                open_in_shell(&framesage_core::paths::policy_path().to_string_lossy());
            }
        });
    }

    /// Tab strip below the toolbar. Uses the chunky `theme::tab_button` so
    /// the active tab reads with a strong visual anchor (filled background
    /// + accent underline) instead of egui's faint selectable label.
    fn render_tab_strip(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let tabs = [
                (Tab::Processes, "Processes"),
                (Tab::Status, "Status"),
                (Tab::Activity, "Activity"),
                (Tab::Rules, "Rules"),
                (Tab::Profiles, "Profiles"),
            ];
            for (t, label) in tabs {
                if theme::tab_button(ui, label, self.tab == t).clicked() {
                    self.tab = t;
                }
            }
        });
    }

    fn render_active_tab(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        status: &Option<StatusSnapshot>,
        recent: &[String],
    ) {
        match self.tab {
            Tab::Status => self.render_status_tab(ctx, ui, status, recent),
            Tab::Processes => self.render_processes_tab(ui, status),
            Tab::Activity => self.render_activity_tab(ui),
            Tab::Rules => self.render_rules_tab(ui, status),
            Tab::Profiles => self.render_profiles_tab(ui, status),
        }
    }

    fn render_status_tab(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        status: &Option<StatusSnapshot>,
        recent: &[String],
    ) {
        let Some(s) = status else {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.colored_label(theme::TEXT_MUTED, "Waiting for the service to respond…");
            });
            return;
        };

        // ─── Hero: engine state at a glance ─────────────────────────────
        render_status_hero(ui, s);
        ui.add_space(10.0);

        // ─── Manual-mode banner (only when active) ──────────────────────
        if let Some(manual_id) = &s.manual_override {
            theme::banner(theme::WARNING).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::WARNING, egui::RichText::new("!").strong().size(13.0));
                    ui.label(
                        egui::RichText::new("Manual mode")
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.colored_label(theme::TEXT_MUTED, "·");
                    ui.label(
                        egui::RichText::new(display_profile_id(&manual_id.0))
                            .strong()
                            .color(theme::WARNING),
                    );
                    ui.colored_label(theme::TEXT_MUTED, "is pinned to every foreground app");
                    if self.elevated {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Exit manual mode").clicked() {
                                self.send_admin_request(
                                    Request::ClearManualOverride,
                                    "clear manual override",
                                );
                            }
                        });
                    }
                });
            });
            ui.add_space(10.0);
        }

        // ─── Side-by-side: Active profile · Foreground ──────────────────
        ui.columns(2, |cols| {
            theme::card().show(&mut cols[0], |ui| {
                ui.label(theme::section_heading("Active profile"));
                ui.add_space(6.0);
                render_active_profile_summary(ui, s);
            });
            theme::card().show(&mut cols[1], |ui| {
                ui.label(theme::section_heading("Foreground"));
                ui.add_space(6.0);
                match &s.foreground {
                    Some(fg) => render_foreground_summary(ui, fg),
                    None => {
                        ui.colored_label(theme::TEXT_MUTED, "No foreground process detected.");
                    }
                }
            });
        });

        ui.add_space(10.0);

        // ─── ProBalance card ────────────────────────────────────────────
        // Live state for the dynamic-priority manager. We aggregate the
        // restraint count from the latest `Processes` snapshot (already
        // refreshed at 1 Hz by the same poller that backs the Processes
        // tab) so this card is always in step with the table view.
        let restrained_now = self
            .processes
            .rows
            .iter()
            .filter(|p| p.restrained_by_probalance)
            .count();
        self.render_probalance_card(ui, s, restrained_now);

        ui.add_space(10.0);

        // ─── Quick actions ──────────────────────────────────────────────
        #[cfg(windows)]
        {
            let paused = s.paused;
            let in_game_mode = s
                .active_profile
                .as_ref()
                .map(|p| p.game_mode.is_some())
                .unwrap_or(false);
            self.render_quick_actions(ctx, ui, paused, in_game_mode);
            ui.add_space(10.0);
        }

        // ─── Recent activity ────────────────────────────────────────────
        ui.label(theme::section_heading("Recent activity"));
        ui.add_space(4.0);
        render_recent_activity(ui, recent);
    }

    /// ProBalance card — Status-tab summary of the dynamic-priority
    /// manager. Shows whether it's on, the configured thresholds, and the
    /// number of processes currently held in restraint. When the engine is
    /// elevated and we have the admin token, an "Enable" / "Disable" button
    /// toggles `policy.probalance.enabled` and sends `SetPolicy` so the
    /// change is persisted to `policy.json` immediately.
    fn render_probalance_card(
        &mut self,
        ui: &mut egui::Ui,
        s: &StatusSnapshot,
        restrained_now: usize,
    ) {
        theme::card().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::section_heading("ProBalance"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let cfg = &s.policy.probalance;
                    let (color, text) = if cfg.enabled {
                        (theme::SUCCESS, "Enabled")
                    } else {
                        (theme::TEXT_MUTED, "Disabled")
                    };
                    theme::status_badge(color).show(ui, |ui| {
                        ui.colored_label(color, text);
                    });
                });
            });
            ui.add_space(6.0);

            let cfg = s.policy.probalance.clone();
            ui.horizontal(|ui| {
                ui.colored_label(theme::TEXT_MUTED, "Currently restraining:");
                let color = if restrained_now > 0 {
                    theme::WARNING
                } else {
                    theme::TEXT_MUTED
                };
                ui.colored_label(color, format!("{restrained_now} processes"));
            });
            ui.horizontal(|ui| {
                ui.colored_label(theme::TEXT_MUTED, "Trigger:");
                ui.colored_label(
                    theme::TEXT,
                    format!(
                        "system CPU >= {}% AND non-foreground hog >= {}% of one core",
                        cfg.system_cpu_threshold_percent, cfg.hog_cpu_threshold_percent
                    ),
                );
            });
            ui.horizontal(|ui| {
                ui.colored_label(theme::TEXT_MUTED, "Dwell:");
                ui.colored_label(
                    theme::TEXT,
                    format!(
                        "{} ms before any restraint can be released",
                        cfg.min_restrain_ms
                    ),
                );
            });

            ui.add_space(8.0);

            // Toggle. Unelevated tray can show the state but can't send
            // SetPolicy through the admin pipe, so the button is greyed
            // when we're not running with the admin token.
            #[cfg(windows)]
            if self.elevated {
                let label = if cfg.enabled {
                    "Disable ProBalance"
                } else {
                    "Enable ProBalance"
                };
                if ui.button(label).clicked() {
                    let mut new_policy = s.policy.clone();
                    new_policy.probalance.enabled = !cfg.enabled;
                    self.send_admin_request(
                        Request::SetPolicy { policy: new_policy },
                        "toggle probalance",
                    );
                }
            } else {
                ui.colored_label(
                    theme::TEXT_MUTED,
                    "Relaunch FrameSage as administrator to toggle ProBalance.",
                );
            }
        });
    }

    /// Quick-actions strip: elevation prompt when not elevated, or
    /// Pause/Resume + Game-Mode-off when we are. Wrapped in a card so it
    /// reads as a distinct section rather than loose buttons.
    #[cfg(windows)]
    fn render_quick_actions(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        paused: bool,
        in_game_mode: bool,
    ) {
        if !self.elevated {
            theme::banner(theme::WARNING).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::WARNING, egui::RichText::new("!").strong().size(14.0));
                    ui.label(
                        egui::RichText::new("Read-only mode")
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.colored_label(
                        theme::TEXT_MUTED,
                        "— Pause, Resume, and Game Mode controls need admin.",
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("Enable controls (UAC)…")
                            .on_hover_text(
                                "Relaunch FrameSage elevated so admin actions go through.",
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
                    });
                });
            });
            if let Some(msg) = self.last_action.lock().unwrap().as_ref() {
                ui.add_space(2.0);
                ui.small(msg);
            }
            return;
        }

        theme::card().show(ui, |ui| {
            ui.label(theme::section_heading("Quick actions"));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if paused {
                    "Resume engine"
                } else {
                    "Pause engine"
                };
                if ui.button(label).clicked() {
                    if paused {
                        self.send_admin_request(Request::Resume, "resume");
                    } else {
                        self.send_admin_request(Request::Pause, "pause");
                    }
                }
                if ui
                    .add_enabled(in_game_mode, egui::Button::new("Exit Game Mode"))
                    .on_hover_text(
                        "Force any active Game Mode session to revert immediately — restores \
                         the taskbar, restarts paused services, resumes suspended processes.",
                    )
                    .clicked()
                {
                    self.send_admin_request(Request::GameModeOff, "game-mode off");
                }
            });
            if let Some(msg) = self.last_action.lock().unwrap().as_ref() {
                ui.add_space(4.0);
                ui.small(msg);
            }
        });
    }

    /// Activity Log tab — full history of every engine event the IPC
    /// subscribe stream has delivered. Filter chips per event kind plus a
    /// substring search make it easy to ask "what did ProBalance do for
    /// the last 5 minutes" or "did the rule for steam.exe ever fire?".
    ///
    /// Buffer is capped at 1000 entries; oldest evicted first (the strip
    /// + Status-tab recent activity already read from the same buffer).
    fn render_activity_tab(&mut self, ui: &mut egui::Ui) {
        use egui_extras::{Column, TableBuilder};

        // Snapshot the event buffer + clear flag under a short lock so the
        // render closure doesn't hold the mutex across the table walk.
        let events: Vec<RecentEvent> = {
            let s = self.state.lock().unwrap();
            s.recent
                .iter()
                .map(|e| RecentEvent {
                    at: e.at,
                    kind: e.kind,
                    label: e.label.clone(),
                })
                .collect()
        };

        // Filter UI — kind chips + substring search.
        ui.horizontal(|ui| {
            ui.label("Show:");
            ui.checkbox(&mut self.activity.show_foreground, "Foreground");
            ui.checkbox(&mut self.activity.show_engine, "Engine");
            ui.checkbox(
                &mut self.activity.show_probalance_restrain,
                "ProBalance demote",
            );
            ui.checkbox(
                &mut self.activity.show_probalance_restore,
                "ProBalance restore",
            );
            ui.checkbox(&mut self.activity.show_other, "Other");
            ui.add_space(8.0);
            ui.label("Find:");
            ui.add(
                egui::TextEdit::singleline(&mut self.activity.filter)
                    .hint_text("substring (case-insensitive)")
                    .desired_width(220.0),
            );
            if ui.button("Clear").clicked() {
                self.activity.filter.clear();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let total = events.len();
                ui.colored_label(theme::TEXT_MUTED, format!("{total} events"));
                if ui.button("Clear log").clicked() {
                    self.state.lock().unwrap().recent.clear();
                }
            });
        });
        ui.add_space(4.0);

        // Apply filters (in newest-first order so the most recent event
        // sits at the top — opposite of the underlying buffer's append
        // order). Filter chips compose with substring match.
        let want_kind = |k: EventKind| -> bool {
            match k {
                EventKind::Foreground => self.activity.show_foreground,
                EventKind::Engine => self.activity.show_engine,
                EventKind::ProBalanceRestrained => self.activity.show_probalance_restrain,
                EventKind::ProBalanceRestored => self.activity.show_probalance_restore,
                EventKind::Other => self.activity.show_other,
            }
        };
        let needle = self.activity.filter.to_ascii_lowercase();
        let filtered: Vec<&RecentEvent> = events
            .iter()
            .rev()
            .filter(|e| want_kind(e.kind))
            .filter(|e| needle.is_empty() || e.label.to_ascii_lowercase().contains(&needle))
            .collect();

        if filtered.is_empty() {
            ui.colored_label(
                theme::TEXT_MUTED,
                if events.is_empty() {
                    "No events yet. Activity will appear here as the engine reconciles."
                } else {
                    "No events match the current filter."
                },
            );
            return;
        }

        // Table: Time | Kind | Message. Wide message column on the right.
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(95.0).at_least(80.0))
            .column(Column::initial(180.0).at_least(120.0))
            .column(Column::remainder().at_least(160.0))
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.label("Time");
                });
                header.col(|ui| {
                    ui.label("Kind");
                });
                header.col(|ui| {
                    ui.label("Event");
                });
            })
            .body(|body| {
                body.rows(18.0, filtered.len(), |mut row| {
                    let e = filtered[row.index()];
                    row.col(|ui| {
                        ui.monospace(format_local_hms(e.at));
                    });
                    row.col(|ui| {
                        ui.colored_label(e.kind.color(), e.kind.display());
                    });
                    row.col(|ui| {
                        ui.label(&e.label);
                    });
                });
            });
    }

    /// Render the Rules tab — view and edit `Policy::rules` via batched
    /// add/delete operations that commit on Save.
    fn render_rules_tab(&mut self, ui: &mut egui::Ui, status: &Option<StatusSnapshot>) {
        let Some(s) = status else {
            ui.label("waiting for status…");
            return;
        };

        if !self.elevated {
            render_readonly_banner(
                ui,
                "Rule edits need admin — open the Status tab and click Enable controls.",
            );
            ui.add_space(8.0);
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

            // Shortcut: open the Add-rule form pre-filled from whatever's
            // foregrounded right now. Saves the typing-the-exe-name step
            // that turned the bare "Add rule" flow into the most-asked-about
            // pain point during hardware validation.
            let fg_exe = s.foreground.as_ref().map(|fg| fg.exe_name.clone());
            let from_fg_enabled = self.elevated && self.rules.form.is_none() && fg_exe.is_some();
            let from_fg_btn = egui::Button::new("Add rule for foreground");
            let resp = ui.add_enabled(from_fg_enabled, from_fg_btn).on_hover_text(
                "Pre-fill an Add-rule form with the current foreground app's exe name. \
                     You can change the matched profile or the match kind before saving.",
            );
            if resp.clicked() {
                if let Some(exe) = fg_exe.clone() {
                    let default_profile = profile_ids
                        .first()
                        .map(|id| id.0.clone())
                        .unwrap_or_else(|| s.policy.default_profile.0.clone());
                    let note = s
                        .foreground
                        .as_ref()
                        .map(|fg| fg.title.clone())
                        .filter(|t| !t.is_empty())
                        .unwrap_or_default();
                    self.rules.form = Some(RuleForm {
                        editing_index: None,
                        match_kind: MatchKind::ExeName,
                        match_value: exe,
                        profile_id: default_profile,
                        note,
                    });
                    if self.policy_draft.is_none() {
                        self.policy_draft = Some(s.policy.clone());
                    }
                }
            }

            // Save changes is enabled whenever there's something to
            // save. Previously gated on "form not open", which made the
            // button greyed-out while the user was filling in a rule —
            // confusing and looked broken. The rule-form's own "Save"
            // button now triggers persistence directly (further down),
            // so most users won't even need this button; it's kept for
            // the case where the user makes multiple edits via Edit
            // buttons and wants to batch.
            let save_enabled = self.elevated && dirty;
            if ui
                .add_enabled(save_enabled, egui::Button::new("Save changes"))
                .clicked()
            {
                if let Some(draft) = self.policy_draft.take() {
                    self.send_admin_request(Request::SetPolicy { policy: draft }, "save policy");
                }
            }

            let discard_enabled = dirty;
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
                            .desired_width(220.0),
                    );
                    // Per-match-kind shortcut: pull the right field off the
                    // current foreground. Disabled when there's no foreground.
                    let fg = s.foreground.as_ref();
                    let (label, value): (&str, Option<String>) = match form.match_kind {
                        MatchKind::ExeName => {
                            ("Use foreground exe", fg.map(|f| f.exe_name.clone()))
                        }
                        MatchKind::PathContains => {
                            ("Use foreground path", fg.map(|f| f.path.clone()))
                        }
                        MatchKind::WindowTitleContains => (
                            "Use foreground title",
                            fg.map(|f| f.title.clone()).filter(|t| !t.is_empty()),
                        ),
                    };
                    let enabled = value.is_some();
                    if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                        if let Some(v) = value {
                            form.match_value = v;
                        }
                    }
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
                // Persist immediately. The hardware-validation footgun
                // was the two-Save flow: form's Save just appended to
                // the draft, then user had to find the toolbar's Save
                // changes to actually push to the service. Single-click
                // intent is what the user actually wants.
                if self.elevated {
                    if let Some(draft) = self.policy_draft.take() {
                        self.send_admin_request(Request::SetPolicy { policy: draft }, "save rule");
                    }
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

        // Section caption — sets context once, doesn't repeat the hero's
        // policy summary (that's in Status tab).
        ui.colored_label(
            theme::TEXT_MUTED,
            "Profiles auto-apply per foreground app via Rules. \
             \u{201c}Apply to foreground\u{201d} overrides the rule for the current app only. \
             \u{201c}Set as manual mode\u{201d} pins a profile to every foreground app until you exit.",
        );
        ui.add_space(8.0);

        // Manual-mode banner. Matches the Status tab's design so the same
        // state reads consistently no matter which tab you land on.
        let mut clear_manual_clicked = false;
        if let Some(manual_id) = &s.manual_override {
            theme::banner(theme::WARNING).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::WARNING, egui::RichText::new("!").strong().size(13.0));
                    ui.label(
                        egui::RichText::new("Manual mode")
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.colored_label(theme::TEXT_MUTED, "·");
                    ui.label(
                        egui::RichText::new(display_profile_id(&manual_id.0))
                            .strong()
                            .color(theme::WARNING),
                    );
                    ui.colored_label(theme::TEXT_MUTED, "is pinned to every foreground app");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(self.elevated, egui::Button::new("Exit manual mode"))
                            .clicked()
                        {
                            clear_manual_clicked = true;
                        }
                    });
                });
            });
            ui.add_space(8.0);
        }
        if clear_manual_clicked {
            self.send_admin_request(Request::ClearManualOverride, "clear manual override");
        }
        ui.horizontal(|ui| {
            let add_enabled = self.elevated
                && self.profiles.editing_id.is_none()
                && self.profiles.new_form.is_none()
                && self.rules.form.is_none();
            if ui
                .add_enabled(add_enabled, egui::Button::new("Add profile"))
                .on_hover_text(
                    "Create a new profile. After saving, expand the new profile \
                     and click Edit to fill in the per-process knobs and Game Mode.",
                )
                .clicked()
            {
                self.profiles.new_form = Some(String::new());
                if self.policy_draft.is_none() {
                    self.policy_draft = Some(s.policy.clone());
                }
            }

            // Save changes is enabled whenever there's anything to save.
            // It used to be gated on "no profile being edited and no form
            // open", which was misery: users edited a profile, hit Save,
            // saw nothing happen (button greyed out), and concluded the
            // app was broken. Profile edits already stream into
            // policy_draft every frame (via Op::UpdateProfile), so it's
            // safe to persist mid-edit — the user can keep editing
            // afterwards and Save again.
            let save_enabled = self.elevated && dirty;
            if ui
                .add_enabled(save_enabled, egui::Button::new("Save changes"))
                .clicked()
            {
                if let Some(draft) = self.policy_draft.take() {
                    self.send_admin_request(Request::SetPolicy { policy: draft }, "save policy");
                }
            }
            let discard_enabled = dirty;
            if ui
                .add_enabled(discard_enabled, egui::Button::new("Discard"))
                .clicked()
            {
                self.policy_draft = None;
                self.profiles.editing_id = None;
                self.profiles.new_form = None;
            }
            if dirty {
                theme::status_badge(theme::WARNING).show(ui, |ui| {
                    ui.colored_label(theme::WARNING, "unsaved");
                });
            }
        });

        // Inline new-profile form. Renders below the toolbar when active.
        if let Some(new_id) = &mut self.profiles.new_form {
            let mut commit = false;
            let mut cancel = false;
            ui.add_space(4.0);
            ui.group(|ui| {
                ui.label(theme::section_heading("New profile"));
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Id:");
                    ui.add(
                        egui::TextEdit::singleline(new_id)
                            .hint_text("e.g. game-poe2")
                            .desired_width(220.0),
                    );
                });
                ui.colored_label(
                    theme::TEXT_MUTED,
                    "Lowercase, dashes only — matches what you'd reference from Rules \
                     (e.g. \u{201c}game-poe2\u{201d} becomes \u{201c}Game POE2\u{201d} in the UI).",
                );
                ui.horizontal(|ui| {
                    let trimmed = new_id.trim();
                    let id_taken = displayed_policy
                        .profiles
                        .keys()
                        .any(|p| p.0.eq_ignore_ascii_case(trimmed));
                    let can_save = !trimmed.is_empty() && !id_taken;
                    if ui.add_enabled(can_save, egui::Button::new("Add")).clicked() {
                        commit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                    if id_taken {
                        ui.colored_label(theme::WARNING, "An profile with this id already exists.");
                    }
                });
            });
            if commit {
                let id = new_id.trim().to_owned();
                let draft = self.policy_draft.get_or_insert_with(|| s.policy.clone());
                draft.profiles.insert(
                    ProfileId(id.clone()),
                    Profile {
                        id: ProfileId(id.clone()),
                        description: String::new(),
                        ..Default::default()
                    },
                );
                self.profiles.new_form = None;
                // Drop straight into edit mode on the new profile so the
                // user can fill in the knobs immediately.
                self.profiles.editing_id = Some(id);
                // Persist the bare new profile to disk immediately. Future
                // field edits will continue to stream into the draft and
                // can be saved via the toolbar Save changes button.
                if self.elevated {
                    if let Some(draft_to_save) = self.policy_draft.clone() {
                        self.send_admin_request(
                            Request::SetPolicy {
                                policy: draft_to_save,
                            },
                            "save profile",
                        );
                    }
                }
            } else if cancel {
                self.profiles.new_form = None;
            }
        }

        if !self.elevated {
            ui.add_space(4.0);
            render_readonly_banner(
                ui,
                "Profile edits need admin — open the Status tab and click Enable controls.",
            );
        }
        if let Some(msg) = self.last_action.lock().unwrap().as_ref() {
            ui.small(msg);
        }
        ui.add_space(8.0);

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
            ApplyNow(String),
            SetManual(String),
            ClearManual,
            DeleteProfile(String),
        }
        let mut ops: Vec<Op> = Vec::new();

        egui::ScrollArea::vertical().show(ui, |ui| {
            for id in &profile_ids {
                let Some(p) = displayed_policy.profiles.get(id) else {
                    continue;
                };
                let is_active = s.active_profile.as_ref().is_some_and(|ap| ap.id == *id);
                let is_editing = self.profiles.editing_id.as_deref() == Some(id.0.as_str());
                let pretty = display_profile_id(&id.0);
                let header_text = if is_editing {
                    format!("{pretty}  (editing)")
                } else if is_active {
                    format!("{pretty}  (active)")
                } else {
                    pretty
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
                            // Apply-now: send ApplyOnce(id) over the admin
                            // pipe. Disabled in edit mode (the user should
                            // Save first) and on the already-active profile
                            // (no-op vs. the normal rule-match path).
                            let apply_enabled = self.elevated
                                && !is_editing
                                && !is_active
                                && self.rules.form.is_none();
                            let apply_btn = egui::Button::new(
                                egui::RichText::new("Apply to foreground").strong(),
                            )
                            .fill(theme::ACCENT)
                            .stroke(egui::Stroke::new(1.0, theme::ACCENT_HOVER));
                            if ui
                                .add_enabled(apply_enabled, apply_btn)
                                .on_hover_text(
                                    "Apply this profile to the current foreground app right now. \
                                     The override holds until you focus a different app, at which \
                                     point the Rules tab decides what profile to apply next.",
                                )
                                .clicked()
                            {
                                ops.push(Op::ApplyNow(id.0.clone()));
                            }

                            // Manual-mode toggle for this profile. If this profile
                            // is already the manual override, the button becomes an
                            // exit affordance; otherwise it sets the override.
                            let is_manual = s.manual_override.as_ref().is_some_and(|m| m == id);
                            let manual_label = if is_manual {
                                "Exit manual mode"
                            } else {
                                "Set as manual mode"
                            };
                            let manual_enabled =
                                self.elevated && !is_editing && self.rules.form.is_none();
                            if ui
                                .add_enabled(manual_enabled, egui::Button::new(manual_label))
                                .on_hover_text(
                                    "Manual mode pins this profile across every focus change. \
                                     The Rules tab and default profile are bypassed until you \
                                     exit manual mode.",
                                )
                                .clicked()
                            {
                                if is_manual {
                                    ops.push(Op::ClearManual);
                                } else {
                                    ops.push(Op::SetManual(id.0.clone()));
                                }
                            }

                            // Delete: right-aligned, separated visually since
                            // it's destructive. Disabled on the active /
                            // manual / default / background profile to stop
                            // the user from deleting something the engine is
                            // currently referencing.
                            let is_default = displayed_policy.default_profile == *id;
                            let is_background = displayed_policy
                                .background_profile
                                .as_ref()
                                .is_some_and(|p| p == id);
                            let referenced_by_rule =
                                displayed_policy.rules.iter().any(|r| r.profile == *id);
                            let delete_enabled = self.elevated
                                && !is_editing
                                && !is_active
                                && !is_manual
                                && !is_default
                                && !is_background
                                && !referenced_by_rule
                                && self.rules.form.is_none();
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let hover = if is_default {
                                        "Cannot delete the default profile."
                                    } else if is_background {
                                        "Cannot delete the background profile."
                                    } else if referenced_by_rule {
                                        "Cannot delete: still referenced by one or more rules. \
                                         Remove or edit those rules first."
                                    } else if is_active || is_manual {
                                        "Cannot delete the currently-applied profile."
                                    } else {
                                        "Delete this profile from the policy."
                                    };
                                    if ui
                                        .add_enabled(delete_enabled, egui::Button::new("Delete"))
                                        .on_hover_text(hover)
                                        .clicked()
                                    {
                                        ops.push(Op::DeleteProfile(id.0.clone()));
                                    }
                                },
                            );
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
                Op::ApplyNow(id) => {
                    self.send_admin_request(
                        Request::ApplyOnce {
                            profile: ProfileId(id.clone()),
                        },
                        "apply now",
                    );
                }
                Op::SetManual(id) => {
                    self.send_admin_request(
                        Request::SetManualOverride {
                            profile: ProfileId(id.clone()),
                        },
                        "set manual override",
                    );
                }
                Op::ClearManual => {
                    self.send_admin_request(Request::ClearManualOverride, "clear manual override");
                }
                Op::DeleteProfile(id) => {
                    let draft = self.policy_draft.get_or_insert_with(|| s.policy.clone());
                    draft.profiles.remove(&ProfileId(id));
                    // Stay below the dirty-state checkbox: don't auto-Save;
                    // user confirms by clicking Save changes.
                }
            }
        }
    }

    /// Processes tab — live table of every process, with filter + sort +
    /// per-row context menu. The main day-to-day view; mirrors the mental
    /// model of every other process supervisor (Task Manager, Process Lasso,
    /// Process Explorer).
    ///
    /// Data source is `AppState.processes`, refreshed at ~1 Hz by
    /// `processes_poll_loop`. The render path holds the lock only long
    /// enough to take a `Vec` snapshot — long-running sort + table walk
    /// happens against the local copy.
    fn render_processes_tab(&mut self, ui: &mut egui::Ui, status: &Option<StatusSnapshot>) {
        use egui_extras::{Column, TableBuilder};

        // ─── Toolbar: filter, summary stats, row count ─────────────────────
        // Aggregate totals across the snapshot — gives the "what's the box
        // doing right now" answer right above the table. CPU is summed in
        // "% of one CPU" units so the total is naturally bounded by
        // (CPU count * 100).
        let total_cpu_one_cpu: u32 = self
            .processes
            .rows
            .iter()
            .map(|p| p.cpu_percent as u32)
            .sum();
        let total_mem: u64 = self.processes.rows.iter().map(|p| p.memory_bytes).sum();
        let total_threads: u32 = self.processes.rows.iter().map(|p| p.threads).sum();
        let managed = self
            .processes
            .rows
            .iter()
            .filter(|p| p.managed_profile.is_some())
            .count();
        let restrained = self
            .processes
            .rows
            .iter()
            .filter(|p| p.restrained_by_probalance)
            .count();

        ui.horizontal(|ui| {
            ui.label("Filter:");
            ui.add(
                egui::TextEdit::singleline(&mut self.processes.filter)
                    .hint_text("type to filter by exe name")
                    .desired_width(200.0),
            );
            if ui.button("Clear").clicked() {
                self.processes.filter.clear();
            }
            ui.separator();
            // Tree-mode toggle. Disabled when a filter is set — the filter
            // forces flat mode so search can find hits buried inside
            // collapsed subtrees. The disabled checkbox communicates that
            // without being a no-op (hover shows the explanation).
            let tree_enabled = self.processes.filter.is_empty();
            let resp = ui
                .add_enabled(
                    tree_enabled,
                    egui::Checkbox::new(&mut self.processes.tree_mode, "Tree"),
                )
                .on_disabled_hover_text("Tree mode is disabled while a filter is active");
            if tree_enabled && resp.clicked() && self.processes.tree_mode {
                // Re-enable tree → start fully expanded so the user sees
                // the whole forest rather than wondering why nothing
                // appeared.
                self.processes.collapsed.clear();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let count = self.processes.rows.len();
                ui.colored_label(theme::TEXT_MUTED, format!("{count} processes"));
                ui.separator();
                ui.colored_label(theme::TEXT_MUTED, format!("{total_threads} threads"));
                ui.separator();
                ui.colored_label(
                    theme::TEXT_MUTED,
                    format!("{} mem", format_bytes(total_mem)),
                );
                ui.separator();
                ui.colored_label(theme::TEXT_MUTED, format!("Total CPU {total_cpu_one_cpu}%"));
                if managed > 0 {
                    ui.separator();
                    ui.colored_label(theme::ACCENT, format!("{managed} managed"));
                }
                if restrained > 0 {
                    ui.separator();
                    ui.colored_label(theme::WARNING, format!("{restrained} restrained"));
                }
            });
        });
        ui.add_space(4.0);

        // ─── Apply filter + sort to a local view ───────────────────────────
        //
        // Tree mode and a non-empty filter are mutually exclusive: searching
        // across the whole tree is more useful than searching within visible
        // subtrees, so the filter forces a flat list. Sort applies either
        // way — in tree mode it sorts within siblings, in flat mode it
        // sorts globally.
        let filter_lc = self.processes.filter.to_ascii_lowercase();
        let tree_active = self.processes.tree_mode && self.processes.filter.is_empty();
        let sort_by = self.processes.sort_by;
        let sort_desc = self.processes.sort_desc;

        let mut rows: Vec<framesage_ipc::ProcessSnapshot> = self
            .processes
            .rows
            .iter()
            .filter(|p| {
                filter_lc.is_empty() || p.exe_name.to_ascii_lowercase().contains(&filter_lc)
            })
            .cloned()
            .collect();
        if !tree_active {
            // Flat sort. Tree mode does its own per-sibling sort inside
            // build_tree_view.
            rows.sort_by(|a, b| compare_snapshots(a, b, sort_by, sort_desc));
        }

        // `visible` is what the body iterates: in tree mode the depth-first
        // flattened tree, in flat mode a trivial all-depth-0 list. Keeping
        // the body loop uniform avoids branching on `tree_active` per row.
        let visible: Vec<TreeRow> = if tree_active {
            build_tree_view(&rows, &self.processes.collapsed, |a, b| {
                compare_snapshots(a, b, sort_by, sort_desc)
            })
        } else {
            rows.iter()
                .enumerate()
                .map(|(i, r)| TreeRow {
                    pid: r.pid,
                    row_index: i,
                    depth: 0,
                    has_children: false,
                })
                .collect()
        };

        // ─── Pull the profile id list so the context menu can offer them ──
        let profile_ids: Vec<String> = status
            .as_ref()
            .map(|s| s.policy.profiles.keys().map(|p| p.0.clone()).collect())
            .unwrap_or_default();
        let mut profile_ids = profile_ids;
        profile_ids.sort();

        // ─── Foreground PID (drives the Foreground row state) ─────────────
        let foreground_pid = status
            .as_ref()
            .and_then(|s| s.foreground.as_ref())
            .map(|f| f.pid);
        let selected_pid = self.processes.selected_pid;

        // ─── Layout split: reserve space for the detail panel ──────────────
        // When a row is selected we carve a strip from the bottom of the
        // central area for the detail card; the table fills the rest. A
        // draggable splitter bar between the two lets the user adjust the
        // ratio — Process Lasso / Process Explorer both ship the same
        // affordance. The chosen height persists for the rest of the
        // session via `ProcessesView.detail_height`.
        const DETAIL_H_DEFAULT: f32 = 210.0;
        const DETAIL_H_MIN: f32 = 100.0;
        const DETAIL_H_MAX: f32 = 600.0;
        const SPLITTER_H: f32 = 6.0;
        let detail_open = self.processes.selected_pid.is_some();
        let avail_h = ui.available_height();
        let detail_h = if detail_open {
            self.processes
                .detail_height
                .unwrap_or(DETAIL_H_DEFAULT)
                .clamp(DETAIL_H_MIN, DETAIL_H_MAX)
        } else {
            0.0
        };
        let splitter_h = if detail_open { SPLITTER_H } else { 0.0 };
        // Always leave the table at least 120px tall so even a tiny window
        // stays usable. The detail panel will scroll internally if cramped.
        let table_h = (avail_h - detail_h - splitter_h - 4.0).max(120.0);
        let mut splitter_drag_delta: f32 = 0.0;

        // ─── Table ─────────────────────────────────────────────────────────
        //
        // egui_extras::TableBuilder handles virtualised rows so a 500-row
        // process list stays cheap even with the per-row context menu.
        // The leading 6px "marker" column paints a colored vertical bar per
        // row state — like the "modified" gutter in code editors. Empty in
        // default rows so colored rows pop without the table looking busy.
        // The 22px icon column sits between the marker and the exe name and
        // renders each process's shell icon (extracted lazily on the UI
        // thread, bounded by `icon_budget`).
        let mut action_queue: Vec<ProcessAction> = Vec::new();
        let mut clicked_pid: Option<u32> = None;
        let mut close_detail = false;
        let mut toggled_pid: Option<u32> = None;
        // Per-frame icon extraction budget. Caps the worst-case cost when a
        // wave of new processes hits the table for the first time — without
        // this a fresh poll could trigger 200 SHGetFileInfoW calls in one
        // frame and stall the runtime.
        #[cfg_attr(not(windows), allow(unused_mut, unused_variables))]
        let mut icon_budget: u32 = 4;
        #[cfg(windows)]
        let ctx_for_icons = ui.ctx().clone();
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), table_h),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                TableBuilder::new(ui)
                    .striped(true)
                    .resizable(true)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(6.0)) // State marker (no header)
                    .column(Column::exact(22.0)) // Icon (no header)
                    .column(Column::initial(220.0).at_least(120.0)) // Exe
                    .column(Column::initial(200.0).at_least(120.0)) // Description
                    .column(Column::initial(140.0).at_least(80.0)) // Company
                    .column(Column::initial(140.0).at_least(80.0)) // User
                    .column(Column::initial(60.0).at_least(50.0)) // PID
                    .column(Column::initial(60.0).at_least(45.0)) // CPU%
                    .column(Column::initial(85.0).at_least(60.0)) // Memory
                    .column(Column::initial(55.0).at_least(45.0)) // Threads
                    .column(Column::initial(85.0).at_least(60.0)) // Priority
                    .column(Column::initial(110.0).at_least(70.0)) // Affinity
                    .column(Column::initial(100.0).at_least(70.0)) // Profile
                    .column(Column::remainder().at_least(70.0)) // Status
                    .header(20.0, |mut header| {
                        header.col(|_ui| {}); // marker column — no header glyph
                        header.col(|_ui| {}); // icon column — no header glyph
                        header
                            .col(|ui| self.sortable_header(ui, "Process", ProcessSortKey::ExeName));
                        header.col(|ui| {
                            self.sortable_header(ui, "Description", ProcessSortKey::Description)
                        });
                        header
                            .col(|ui| self.sortable_header(ui, "Company", ProcessSortKey::Company));
                        header.col(|ui| self.sortable_header(ui, "User", ProcessSortKey::User));
                        header.col(|ui| self.sortable_header(ui, "PID", ProcessSortKey::Pid));
                        header.col(|ui| self.sortable_header(ui, "CPU %", ProcessSortKey::Cpu));
                        header.col(|ui| self.sortable_header(ui, "Memory", ProcessSortKey::Memory));
                        header
                            .col(|ui| self.sortable_header(ui, "Threads", ProcessSortKey::Threads));
                        header.col(|ui| {
                            self.sortable_header(ui, "Priority", ProcessSortKey::Priority)
                        });
                        header.col(|ui| {
                            ui.label("Affinity");
                        });
                        header
                            .col(|ui| self.sortable_header(ui, "Profile", ProcessSortKey::Profile));
                        header.col(|ui| {
                            ui.label("Status");
                        });
                    })
                    .body(|body| {
                        body.rows(18.0, visible.len(), |mut row| {
                            let tr = visible[row.index()];
                            let p = &rows[tr.row_index];
                            let pid = p.pid;
                            let exe = p.exe_name.clone();
                            let state = classify_row(p, foreground_pid);

                            // Marker column: paint a 3px vertical band on the left of
                            // the cell when the row has a non-default state. The
                            // painter is clipped to the cell, so this never leaks
                            // outside the column even with the table's striping.
                            row.col(|ui| {
                                if let Some(color) = row_marker_color(state) {
                                    let rect = ui.max_rect();
                                    let bar = egui::Rect::from_min_size(
                                        rect.min,
                                        egui::vec2(3.0, rect.height()),
                                    );
                                    ui.painter().rect_filled(bar, egui::Rounding::ZERO, color);
                                }
                            });

                            // Icon column: render the shell icon for this exe.
                            // Cache-miss extractions are bounded by `icon_budget`
                            // (per-frame) so a fresh poll never stalls the UI.
                            // On non-Windows hosts the column stays blank — same
                            // result as a miss.
                            row.col(|ui| {
                                #[cfg(windows)]
                                {
                                    if !p.exe_path.is_empty() {
                                        if let Some(tex) = self.icons.get_or_load(
                                            &ctx_for_icons,
                                            &p.exe_path,
                                            &mut icon_budget,
                                        ) {
                                            ui.add(
                                                egui::Image::new(&tex)
                                                    .fit_to_exact_size(egui::vec2(16.0, 16.0)),
                                            );
                                        }
                                    }
                                }
                                #[cfg(not(windows))]
                                {
                                    let _ = ui;
                                }
                            });

                            row.col(|ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    // Tree indent + ▶/▼ toggle. Tree mode
                                    // only — in flat mode tr.depth is 0
                                    // and tr.has_children is false, so no
                                    // indent and no glyph.
                                    if tr.depth > 0 {
                                        ui.add_space(tr.depth as f32 * 14.0);
                                    }
                                    if tr.has_children {
                                        let collapsed_now = self.processes.collapsed.contains(&pid);
                                        // ASCII glyphs — the unicode triangles
                                        // ▶/▼ render as empty boxes in egui's
                                        // default font (no glyph coverage),
                                        // which the user reported as "stupid
                                        // squares" everywhere. ASCII renders
                                        // identically on every font.
                                        let glyph = if collapsed_now { "+" } else { "-" };
                                        let tri = ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(glyph)
                                                    .color(theme::TEXT_MUTED)
                                                    .monospace(),
                                            )
                                            .sense(egui::Sense::click()),
                                        );
                                        if tri.clicked() {
                                            toggled_pid = Some(pid);
                                        }
                                    } else if tr.depth > 0 {
                                        // Leaf child: reserve the toggle's
                                        // width so labels of siblings still
                                        // align under the parent's name.
                                        ui.add_space(10.0);
                                    }

                                    // Foreground rows get a small right-pointing triangle
                                    // prefix so the eye picks them out instantly even
                                    // when the user has scrolled away from the colored
                                    // marker. ASCII `> ` instead of unicode triangle —
                                    // egui's default font doesn't include the
                                    // Geometric Shapes block, and unicode triangles
                                    // render as empty boxes.
                                    let label_text = if state == RowState::Foreground {
                                        format!("> {}", p.exe_name)
                                    } else {
                                        p.exe_name.clone()
                                    };
                                    // Wrap the label in an explicit
                                    // `Label::sense(click)` so a single click
                                    // toggles row selection. The subsequent
                                    // .context_menu() call attaches the
                                    // right-click menu to the same response —
                                    // same widget serves both interactions.
                                    let label = egui::Label::new(
                                        egui::RichText::new(label_text).color(row_exe_color(state)),
                                    )
                                    .sense(egui::Sense::click());
                                    let mut resp = ui.add(label);
                                    // Highlight the currently-selected row by stroking
                                    // a thin accent border around the cell. Multi-
                                    // selected rows get a translucent fill so the
                                    // user can see the bulk-action target set at
                                    // a glance, distinct from the single "detail"
                                    // selection's stroke.
                                    let in_multi = self.processes.multi_selected.contains(&pid);
                                    if in_multi {
                                        let rect = ui.max_rect();
                                        ui.painter().rect_filled(
                                            rect,
                                            egui::Rounding::ZERO,
                                            egui::Color32::from_rgba_unmultiplied(50, 130, 246, 40),
                                        );
                                    }
                                    if selected_pid == Some(pid) {
                                        let rect = ui.max_rect();
                                        ui.painter().rect_stroke(
                                            rect,
                                            egui::Rounding::ZERO,
                                            egui::Stroke::new(1.0, theme::ACCENT),
                                        );
                                    }
                                    if resp.clicked() {
                                        clicked_pid = Some(pid);
                                        resp.mark_changed();
                                    }
                                    // Right-click anywhere on the name opens the per-PID
                                    // context menu — same affordance Process Explorer uses.
                                    // When the right-clicked row is part of the multi-
                                    // selection, the menu acts on EVERY selected PID;
                                    // otherwise it acts only on the right-clicked one
                                    // (Task Manager / Process Explorer convention).
                                    let multi = &self.processes.multi_selected;
                                    let in_multi_now = multi.contains(&pid);
                                    let targets: Vec<(u32, String)> =
                                        if in_multi_now && multi.len() > 1 {
                                            rows.iter()
                                                .filter(|r| multi.contains(&r.pid))
                                                .map(|r| (r.pid, r.exe_name.clone()))
                                                .collect()
                                        } else {
                                            vec![(pid, exe.clone())]
                                        };
                                    let bulk = targets.len() > 1;
                                    resp.context_menu(|ui| {
                                        if bulk {
                                            ui.label(format!(
                                                "{} processes selected",
                                                targets.len()
                                            ));
                                        } else {
                                            ui.label(format!("{} (pid {})", &exe, pid));
                                        }
                                        ui.separator();
                                        ui.menu_button("Set priority", |ui| {
                                            for (label, class) in PRIORITY_CHOICES.iter() {
                                                if ui.button(*label).clicked() {
                                                    for (t_pid, _) in &targets {
                                                        action_queue.push(
                                                            ProcessAction::SetPriority {
                                                                pid: *t_pid,
                                                                class: *class,
                                                            },
                                                        );
                                                    }
                                                    ui.close_menu();
                                                }
                                            }
                                        });
                                        ui.menu_button("Apply profile now", |ui| {
                                            for pid_name in &profile_ids {
                                                if ui.button(pid_name).clicked() {
                                                    // ApplyProfileForeground actually
                                                    // applies to the FOREGROUND process
                                                    // (single-shot). For bulk we'd want
                                                    // per-PID apply — falling back to
                                                    // foreground apply for the bulk
                                                    // case until that IPC lands.
                                                    action_queue.push(
                                                        ProcessAction::ApplyProfileForeground {
                                                            profile: pid_name.clone(),
                                                        },
                                                    );
                                                    ui.close_menu();
                                                }
                                            }
                                        });
                                        ui.menu_button("Create rule for this exe", |ui| {
                                            for pid_name in &profile_ids {
                                                if ui.button(pid_name).clicked() {
                                                    // For bulk, dedupe by exe so we
                                                    // don't push N identical Create
                                                    // actions for the same exe name
                                                    // (every steamwebhelper.exe row
                                                    // shares the same name).
                                                    let mut seen = std::collections::HashSet::new();
                                                    for (_, e) in &targets {
                                                        let lk = e.to_ascii_lowercase();
                                                        if seen.insert(lk) {
                                                            action_queue.push(
                                                                ProcessAction::CreateRule {
                                                                    exe_name: e.clone(),
                                                                    profile: pid_name.clone(),
                                                                },
                                                            );
                                                        }
                                                    }
                                                    ui.close_menu();
                                                }
                                            }
                                        });
                                        ui.menu_button("Set CPU affinity", |ui| {
                                            let mut affinity_dispatch =
                                                |sel: framesage_core::CpuSelector,
                                                 close: &mut bool| {
                                                    for (t_pid, _) in &targets {
                                                        action_queue.push(
                                                            ProcessAction::SetAffinity {
                                                                pid: *t_pid,
                                                                selector: sel.clone(),
                                                            },
                                                        );
                                                    }
                                                    *close = true;
                                                };
                                            let mut want_close = false;
                                            if ui.button("X3D CCD (Cache cores)").clicked() {
                                                affinity_dispatch(
                                                    framesage_core::CpuSelector::Kind(
                                                        framesage_core::CoreKind::Cache,
                                                    ),
                                                    &mut want_close,
                                                );
                                            }
                                            if ui
                                                .button("Non-X3D CCD (Performance cores)")
                                                .clicked()
                                            {
                                                affinity_dispatch(
                                                    framesage_core::CpuSelector::Kind(
                                                        framesage_core::CoreKind::Performance,
                                                    ),
                                                    &mut want_close,
                                                );
                                            }
                                            if ui.button("All cores (reset)").clicked() {
                                                affinity_dispatch(
                                                    framesage_core::CpuSelector::All,
                                                    &mut want_close,
                                                );
                                            }
                                            if ui.button("Custom…").clicked() {
                                                // The picker is single-PID by design
                                                // (one mask per process). For bulk
                                                // custom-mask use, the user picks once
                                                // then can use Ctrl-click + the X3D /
                                                // non-X3D presets next time.
                                                action_queue.push(
                                                    ProcessAction::RequestAffinityPicker {
                                                        pid,
                                                        exe_name: exe.clone(),
                                                    },
                                                );
                                                want_close = true;
                                            }
                                            if want_close {
                                                ui.close_menu();
                                            }
                                        });
                                        ui.separator();
                                        let trim_label = if bulk {
                                            format!(
                                                "Trim working set on {} processes",
                                                targets.len()
                                            )
                                        } else {
                                            "Trim working set".to_string()
                                        };
                                        if ui
                                            .button(trim_label)
                                            .on_hover_text(
                                                "Release the process's resident pages back \
                                                 to the kernel — frees RAM for a heavy launch. \
                                                 The process's working set re-grows on next \
                                                 page-touch, so use as a pre-launch nudge.",
                                            )
                                            .clicked()
                                        {
                                            for (t_pid, _) in &targets {
                                                action_queue.push(ProcessAction::TrimWorkingSet {
                                                    pid: *t_pid,
                                                });
                                            }
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        let suspend_label = if bulk {
                                            format!("Suspend {} processes", targets.len())
                                        } else {
                                            "Suspend process".to_string()
                                        };
                                        if ui.button(suspend_label).clicked() {
                                            for (t_pid, _) in &targets {
                                                action_queue
                                                    .push(ProcessAction::Suspend { pid: *t_pid });
                                            }
                                            ui.close_menu();
                                        }
                                        let resume_label = if bulk {
                                            format!("Resume {} processes", targets.len())
                                        } else {
                                            "Resume process".to_string()
                                        };
                                        if ui.button(resume_label).clicked() {
                                            for (t_pid, _) in &targets {
                                                action_queue
                                                    .push(ProcessAction::Resume { pid: *t_pid });
                                            }
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        let terminate_label = if bulk {
                                            format!("Terminate {} processes…", targets.len())
                                        } else {
                                            "Terminate process…".to_string()
                                        };
                                        if ui
                                            .add(egui::Button::new(
                                                egui::RichText::new(terminate_label)
                                                    .color(theme::ERROR),
                                            ))
                                            .clicked()
                                        {
                                            // Terminate is gated by the confirm modal.
                                            // For bulk, we push one RequestTerminate
                                            // per PID — the modal opens for the first,
                                            // and the next pops up after Cancel/Apply
                                            // until they're all resolved. (Could be
                                            // improved to a single multi-target modal
                                            // in a follow-up.)
                                            for (t_pid, e_name) in &targets {
                                                action_queue.push(
                                                    ProcessAction::RequestTerminate {
                                                        pid: *t_pid,
                                                        exe_name: e_name.clone(),
                                                    },
                                                );
                                            }
                                            ui.close_menu();
                                        }
                                    });
                                }); // ui.horizontal (tree indent + name)
                            });
                            // Description: human-readable label from the
                            // exe's version resource ("Microsoft OneDrive",
                            // "Steam Client Service Helper"). Truncates with
                            // an ellipsis when the cell can't fit the full
                            // string; full text shown on hover.
                            row.col(|ui| match &p.description {
                                Some(desc) => {
                                    let resp = ui.add(egui::Label::new(desc).truncate());
                                    if desc.len() > 24 {
                                        let _ = resp.on_hover_text(desc);
                                    }
                                }
                                None => {
                                    ui.weak("—");
                                }
                            });
                            // Company: publisher string from the same version
                            // resource the description came from.
                            row.col(|ui| match &p.company {
                                Some(co) => {
                                    let resp = ui.add(egui::Label::new(co).truncate());
                                    if co.len() > 18 {
                                        let _ = resp.on_hover_text(co);
                                    }
                                }
                                None => {
                                    ui.weak("—");
                                }
                            });
                            // User: owning account, "DOMAIN\\username" or
                            // just "username" for local accounts. Color
                            // SYSTEM / NT-SERVICE rows muted so the eye
                            // skips OS background and lands on user code.
                            row.col(|ui| match &p.user {
                                Some(u) => {
                                    let is_system = u.starts_with("NT AUTHORITY\\")
                                        || u == "SYSTEM"
                                        || u.starts_with("NT SERVICE\\")
                                        || u.starts_with("Window Manager\\")
                                        || u.starts_with("Font Driver Host\\");
                                    let label = egui::Label::new(egui::RichText::new(u).color(
                                        if is_system {
                                            theme::TEXT_MUTED
                                        } else {
                                            theme::TEXT
                                        },
                                    ))
                                    .truncate();
                                    let resp = ui.add(label);
                                    if u.len() > 18 {
                                        let _ = resp.on_hover_text(u);
                                    }
                                }
                                None => {
                                    ui.weak("—");
                                }
                            });
                            row.col(|ui| {
                                ui.monospace(p.pid.to_string());
                            });
                            row.col(|ui| {
                                // Color the CPU% column based on intensity: green for
                                // idle, yellow for moderate, red for hot. Anchors
                                // attention on the actually-busy processes at a
                                // glance — same affordance Task Manager / PL use.
                                let color = cpu_percent_color(p.cpu_percent);
                                ui.colored_label(color, format!("{}", p.cpu_percent));
                            });
                            row.col(|ui| {
                                let resp = ui.monospace(format_bytes(p.memory_bytes));
                                // Hover the working-set figure to see the
                                // wider memory story: how high the working
                                // set has ever climbed (peak) and how much
                                // is uniquely this process's (private).
                                // A growing peak-vs-current gap is the
                                // classic memory-leak signal.
                                let tip = format!(
                                    "Working set: {}\nPeak working set: {}\nPrivate bytes: {}",
                                    format_bytes(p.memory_bytes),
                                    format_bytes(p.peak_working_set_bytes),
                                    format_bytes(p.private_bytes),
                                );
                                let _ = resp.on_hover_text(tip);
                            });
                            row.col(|ui| {
                                ui.monospace(p.threads.to_string());
                            });
                            row.col(|ui| {
                                ui.label(priority_class_label(p.priority_class_raw));
                            });
                            row.col(|ui| {
                                // Show the affinity mask in hex (compact) but
                                // expand to a human-friendly CPU range list on
                                // hover ("CPUs: 0–7, 14"). 0x0 collapses to
                                // "(none)" which is what an inaccessible
                                // process yields.
                                let resp = ui.monospace(format!("{:#x}", p.affinity_mask));
                                let decoded = decode_affinity_mask(p.affinity_mask);
                                let _ = resp.on_hover_text(decoded);
                            });
                            row.col(|ui| match &p.managed_profile {
                                Some(id) => {
                                    // ★ prefix marks rows whose profile came from a
                                    // user-authored Rule (a match in policy.rules).
                                    // Without the prefix it'd be impossible at a
                                    // glance to tell a Rule-pinned profile from a
                                    // one-shot ApplyOnce or manual-override profile.
                                    // matched_rule_note is set for every rule match
                                    // (empty string if the user didn't write a note),
                                    // so its presence is the signal.
                                    let pinned = p.matched_rule_note.is_some();
                                    let label = if pinned {
                                        format!("★ {id}")
                                    } else {
                                        id.clone()
                                    };
                                    ui.colored_label(theme::ACCENT, label);
                                }
                                None => {
                                    ui.weak("—");
                                }
                            });
                            row.col(|ui| {
                                if p.restrained_by_probalance {
                                    // ● prefix calls out ProBalance involvement;
                                    // visually pairs with the WARNING-tinted marker
                                    // bar on the same row.
                                    ui.colored_label(theme::WARNING, "● ProBalance");
                                } else if let Some(note) = &p.matched_rule_note {
                                    if note.is_empty() {
                                        ui.weak("rule");
                                    } else {
                                        ui.weak(note);
                                    }
                                } else {
                                    ui.weak("—");
                                }
                            });
                        });
                    });
            }, // close allocate_ui_with_layout for the table region
        );

        // ─── Splitter + Detail panel ──────────────────────────────────────
        // Splitter bar between the table and the detail panel. Drag the bar
        // to resize. Width is the full available width; height is
        // SPLITTER_H. Hovered/dragged states tint the bar with the accent
        // colour and switch the cursor to vertical-resize so the affordance
        // reads at a glance.
        if let Some(pid) = selected_pid {
            let (splitter_rect, splitter_resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), SPLITTER_H),
                egui::Sense::drag(),
            );
            let active = splitter_resp.hovered() || splitter_resp.dragged();
            let bar_color = if active { theme::ACCENT } else { theme::BORDER };
            // Paint a thin centred 1px line so the bar reads as a divider
            // when idle and a target when hovered. The full-height rect
            // catches drag events comfortably even if the user grabs near
            // the edge.
            let line_y = splitter_rect.center().y;
            ui.painter().rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(splitter_rect.left(), line_y - 0.5),
                    egui::pos2(splitter_rect.right(), line_y + 0.5),
                ),
                egui::Rounding::ZERO,
                bar_color,
            );
            if active {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            }
            if splitter_resp.dragged() {
                // Dragging down shrinks the detail panel (table grows);
                // dragging up enlarges it. drag_delta().y is positive when
                // moving the cursor down — invert so the detail-height
                // delta matches the user's mental model.
                splitter_drag_delta = -splitter_resp.drag_delta().y;
            }

            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), detail_h),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    render_process_detail(
                        ui,
                        pid,
                        &rows,
                        &profile_ids,
                        &mut action_queue,
                        &mut close_detail,
                    );
                },
            );
        }

        // ─── Apply selection toggle / close flag ───────────────────────────
        //
        // Multi-select rules (mirrors Task Manager / Process Explorer):
        //   * Plain click — clear multi_selected, set selected_pid (toggle
        //     off if it was already the single selection).
        //   * Ctrl click  — toggle membership in multi_selected; selected_pid
        //     becomes the clicked PID; remember as range anchor.
        //   * Shift click — extend range from last_clicked_pid to the
        //     clicked PID, using the current visual sort order of `rows`.
        //
        // We read the modifier state at the END of the frame so a click on
        // row N captures whatever Ctrl/Shift was held during the click —
        // matches what every other table widget does.
        if let Some(pid) = clicked_pid {
            let (ctrl, shift) = ui
                .ctx()
                .input(|i| (i.modifiers.command || i.modifiers.ctrl, i.modifiers.shift));
            if shift {
                // Range select from the last anchor PID, in current visual
                // order (`rows` is already filter+sort-applied above).
                if let Some(anchor) = self.processes.last_clicked_pid {
                    let pids: Vec<u32> = rows.iter().map(|r| r.pid).collect();
                    let a = pids.iter().position(|p| *p == anchor);
                    let b = pids.iter().position(|p| *p == pid);
                    if let (Some(ai), Some(bi)) = (a, b) {
                        let (lo, hi) = if ai <= bi { (ai, bi) } else { (bi, ai) };
                        self.processes.multi_selected.clear();
                        for p in &pids[lo..=hi] {
                            self.processes.multi_selected.insert(*p);
                        }
                        self.processes.selected_pid = Some(pid);
                    }
                } else {
                    // No anchor yet — treat as plain click.
                    self.processes.multi_selected.clear();
                    self.processes.selected_pid = Some(pid);
                    self.processes.last_clicked_pid = Some(pid);
                }
            } else if ctrl {
                if !self.processes.multi_selected.remove(&pid) {
                    self.processes.multi_selected.insert(pid);
                }
                self.processes.selected_pid = Some(pid);
                self.processes.last_clicked_pid = Some(pid);
            } else {
                // Plain click: clear multi, toggle single.
                self.processes.multi_selected.clear();
                if self.processes.selected_pid == Some(pid) {
                    self.processes.selected_pid = None;
                } else {
                    self.processes.selected_pid = Some(pid);
                }
                self.processes.last_clicked_pid = Some(pid);
            }
        }
        if close_detail {
            self.processes.selected_pid = None;
        }
        // Toggle tree expand/collapse: a row's ▶/▼ click flips its PID's
        // membership in the `collapsed` set. Default empty = all expanded;
        // explicit membership = children hidden for this branch.
        if let Some(pid) = toggled_pid {
            if !self.processes.collapsed.remove(&pid) {
                self.processes.collapsed.insert(pid);
            }
        }
        // Apply any splitter drag accumulated this frame. Clamping to
        // [MIN, MAX] keeps the detail strip from collapsing to invisible
        // or eating the entire window. Stored as `Some(h)` so the next
        // frame uses the user's chosen size instead of the default.
        if splitter_drag_delta != 0.0 {
            let new_h = (detail_h + splitter_drag_delta).clamp(DETAIL_H_MIN, DETAIL_H_MAX);
            self.processes.detail_height = Some(new_h);
        }

        // ─── Dispatch context-menu actions outside the render closure ─────
        for action in action_queue {
            match action {
                ProcessAction::SetPriority { pid, class } => {
                    self.send_admin_request(
                        Request::SetProcessPriority { pid, class },
                        "set priority",
                    );
                }
                ProcessAction::ApplyProfileForeground { profile } => {
                    self.send_admin_request(
                        Request::ApplyOnce {
                            profile: ProfileId(profile),
                        },
                        "apply profile",
                    );
                }
                ProcessAction::CreateRule { exe_name, profile } => {
                    if let Some(s) = status {
                        // Take a snapshot of the policy we'd send before any
                        // mutable borrows on self happen, then issue the IPC
                        // call. This avoids the borrow-checker complaining
                        // about overlapping borrows of self.policy_draft and
                        // self.send_admin_request.
                        let new_policy = {
                            let draft = self.policy_draft.get_or_insert_with(|| s.policy.clone());
                            let already = draft.rules.iter().any(|r| match &r.r#match {
                                AppMatch::ExeName(n) => n.eq_ignore_ascii_case(&exe_name),
                                _ => false,
                            });
                            if already {
                                None
                            } else {
                                draft.rules.push(AppRule {
                                    r#match: AppMatch::ExeName(exe_name.clone()),
                                    profile: ProfileId(profile),
                                    note: String::new(),
                                });
                                Some(draft.clone())
                            }
                        };
                        match new_policy {
                            Some(p) => {
                                self.send_admin_request(
                                    Request::SetPolicy { policy: p },
                                    "create rule",
                                );
                            }
                            None => {
                                *self.last_action.lock().unwrap() =
                                    Some(format!("Rule for {exe_name} already exists"));
                            }
                        }
                    }
                }
                ProcessAction::Suspend { pid } => {
                    self.send_admin_request(Request::SuspendProcess { pid }, "suspend process");
                }
                ProcessAction::Resume { pid } => {
                    self.send_admin_request(Request::ResumeProcess { pid }, "resume process");
                }
                ProcessAction::TrimWorkingSet { pid } => {
                    self.send_admin_request(Request::TrimWorkingSet { pid }, "trim working set");
                }
                ProcessAction::RequestTerminate { pid, exe_name } => {
                    // Don't fire the IPC yet — open the confirm modal first.
                    // The modal's "Confirm" click is what actually terminates.
                    self.terminate_confirm = Some(TerminateConfirm { pid, exe_name });
                }
                ProcessAction::SetAffinity { pid, selector } => {
                    self.send_admin_request(
                        Request::SetProcessAffinity { pid, selector },
                        "set affinity",
                    );
                }
                ProcessAction::RequestAffinityPicker { pid, exe_name } => {
                    // Pre-populate the picker with the process's CURRENT mask
                    // so the user can tweak rather than start from scratch.
                    // Default to all cores if we can't read the live mask.
                    let initial_mask = self
                        .processes
                        .rows
                        .iter()
                        .find(|p| p.pid == pid)
                        .map(|p| p.affinity_mask)
                        .unwrap_or(!0u64);
                    self.affinity_picker = Some(AffinityPicker {
                        pid,
                        exe_name,
                        mask: initial_mask,
                    });
                }
            }
        }
    }

    /// Header cell that toggles the sort key on click. Shows ▲ / ▼ on the
    /// active column.
    fn sortable_header(&mut self, ui: &mut egui::Ui, label: &str, key: ProcessSortKey) {
        let active = self.processes.sort_by == Some(key);
        // ASCII arrows for the same reason as the tree toggles: ▲/▼ are
        // outside the default font's coverage and render as empty squares.
        let suffix = if !active {
            ""
        } else if self.processes.sort_desc {
            " v"
        } else {
            " ^"
        };
        if ui
            .add(egui::Label::new(format!("{label}{suffix}")).sense(egui::Sense::click()))
            .clicked()
        {
            if active {
                self.processes.sort_desc = !self.processes.sort_desc;
            } else {
                self.processes.sort_by = Some(key);
                self.processes.sort_desc = !matches!(
                    key,
                    ProcessSortKey::ExeName | ProcessSortKey::Pid | ProcessSortKey::Profile
                );
            }
        }
    }
}

/// Pending context-menu click captured during the render pass; dispatched
/// after the render closure releases its borrow on `self`.
enum ProcessAction {
    SetPriority {
        pid: u32,
        class: PriorityClass,
    },
    ApplyProfileForeground {
        profile: String,
    },
    CreateRule {
        exe_name: String,
        profile: String,
    },
    Suspend {
        pid: u32,
    },
    Resume {
        pid: u32,
    },
    TrimWorkingSet {
        pid: u32,
    },
    /// Opens the Terminate confirmation modal — DOES NOT directly send the
    /// IPC. The modal's "Confirm" button is what fires the actual request.
    /// Captured separately from `Suspend` / `Resume` because we never want
    /// a misclick to nuke a process.
    RequestTerminate {
        pid: u32,
        exe_name: String,
    },
    /// One-shot affinity pin using a topology-aware selector (Kind(Cache)
    /// for X3D, Kind(Performance) for non-X3D, All for reset, etc.).
    /// Engine resolves against live `CpuTopology`.
    SetAffinity {
        pid: u32,
        selector: framesage_core::CpuSelector,
    },
    /// Opens the custom-mask affinity picker modal for `pid`. The modal's
    /// Apply button is what fires the actual `SetAffinity` IPC with the
    /// user-built mask.
    RequestAffinityPicker {
        pid: u32,
        exe_name: String,
    },
}

/// (display label, enum value) pairs used by the per-row priority submenu.
/// Order matches Task Manager's "Set priority" — high to low — which is
/// what users expect.
const PRIORITY_CHOICES: &[(&str, PriorityClass)] = &[
    ("High", PriorityClass::High),
    ("Above Normal", PriorityClass::AboveNormal),
    ("Normal", PriorityClass::Normal),
    ("Below Normal", PriorityClass::BelowNormal),
    ("Idle (lowest)", PriorityClass::Idle),
];

/// Pick a CPU%-column foreground based on intensity. Same band thresholds
/// Process Lasso / Task Manager use:
///   * 0–10  → muted (idle, default text)
///   * 10–50 → normal text
///   * 50–80 → warning (yellow)
///   * 80+   → error (red)
fn cpu_percent_color(cpu: u16) -> egui::Color32 {
    match cpu {
        0..=10 => theme::TEXT_MUTED,
        11..=50 => theme::TEXT,
        51..=80 => theme::WARNING,
        _ => theme::ERROR,
    }
}

/// Compare two `ProcessSnapshot`s by the chosen sort key + direction.
///
/// Single source of truth for both the flat-mode sort and the per-sibling
/// sort inside `build_tree_view`. `None` for `sort_by` means "preserve
/// input order" (= `Equal` for every pair) so callers can opt out without
/// branching at the call site.
fn compare_snapshots(
    a: &framesage_ipc::ProcessSnapshot,
    b: &framesage_ipc::ProcessSnapshot,
    sort_by: Option<ProcessSortKey>,
    desc: bool,
) -> std::cmp::Ordering {
    let Some(key) = sort_by else {
        return std::cmp::Ordering::Equal;
    };
    let ord = match key {
        ProcessSortKey::ExeName => a
            .exe_name
            .to_ascii_lowercase()
            .cmp(&b.exe_name.to_ascii_lowercase()),
        ProcessSortKey::Pid => a.pid.cmp(&b.pid),
        ProcessSortKey::Cpu => a.cpu_percent.cmp(&b.cpu_percent),
        ProcessSortKey::Memory => a.memory_bytes.cmp(&b.memory_bytes),
        ProcessSortKey::Threads => a.threads.cmp(&b.threads),
        ProcessSortKey::Priority => a.priority_class_raw.cmp(&b.priority_class_raw),
        ProcessSortKey::Profile => a.managed_profile.cmp(&b.managed_profile),
        ProcessSortKey::Description => {
            // Case-insensitive; None collates after Some so labelled rows
            // cluster together regardless of direction.
            let av = a.description.as_deref().unwrap_or("\u{ffff}");
            let bv = b.description.as_deref().unwrap_or("\u{ffff}");
            av.to_ascii_lowercase().cmp(&bv.to_ascii_lowercase())
        }
        ProcessSortKey::Company => {
            let av = a.company.as_deref().unwrap_or("\u{ffff}");
            let bv = b.company.as_deref().unwrap_or("\u{ffff}");
            av.to_ascii_lowercase().cmp(&bv.to_ascii_lowercase())
        }
        ProcessSortKey::User => {
            let av = a.user.as_deref().unwrap_or("\u{ffff}");
            let bv = b.user.as_deref().unwrap_or("\u{ffff}");
            av.to_ascii_lowercase().cmp(&bv.to_ascii_lowercase())
        }
    };
    if desc {
        ord.reverse()
    } else {
        ord
    }
}

/// One visible row in tree mode. The flat table iterates these instead of
/// the raw `Vec<ProcessSnapshot>`; the depth controls indentation and the
/// `has_children` flag controls whether the ▶/▼ toggle renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TreeRow {
    pid: u32,
    /// Index into the unsorted `rows: &[ProcessSnapshot]` slice the builder
    /// was given. Lets the renderer fetch the underlying snapshot without
    /// a second hash lookup.
    row_index: usize,
    depth: u8,
    has_children: bool,
}

/// Build a depth-first flattened view of the process tree, respecting the
/// user's collapsed set.
///
/// Edges come from `ProcessSnapshot::parent_pid`. A process is a root when
/// its parent is `0` OR points at a PID not present in `rows` (the parent
/// has exited but the kernel hasn't reaped the orphan). Cycles — which
/// shouldn't be possible but a stale PID-reuse race could theoretically
/// produce — are broken via a DFS stack: if we'd revisit a PID already
/// on the path, we skip rather than loop.
///
/// Children of each parent (and the root list) are sorted by `cmp`, so
/// the chosen ProcessSortKey applies within siblings — matches the
/// Process Explorer / Process Lasso convention.
fn build_tree_view(
    rows: &[framesage_ipc::ProcessSnapshot],
    collapsed: &std::collections::HashSet<u32>,
    cmp: impl Fn(&framesage_ipc::ProcessSnapshot, &framesage_ipc::ProcessSnapshot) -> std::cmp::Ordering,
) -> Vec<TreeRow> {
    use std::collections::HashMap;
    let mut by_pid: HashMap<u32, usize> = HashMap::with_capacity(rows.len());
    for (i, r) in rows.iter().enumerate() {
        by_pid.insert(r.pid, i);
    }
    let mut children: HashMap<u32, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (i, r) in rows.iter().enumerate() {
        let parent_alive = r.parent_pid != 0 && by_pid.contains_key(&r.parent_pid);
        if parent_alive {
            children.entry(r.parent_pid).or_default().push(i);
        } else {
            roots.push(i);
        }
    }
    // Sort within siblings and at the root.
    for v in children.values_mut() {
        v.sort_by(|&a, &b| cmp(&rows[a], &rows[b]));
    }
    roots.sort_by(|&a, &b| cmp(&rows[a], &rows[b]));

    let mut out = Vec::with_capacity(rows.len());
    let mut stack: Vec<u32> = Vec::new();
    for r in roots {
        visit_tree(r, 0, rows, &children, collapsed, &mut out, &mut stack);
    }
    out
}

fn visit_tree(
    i: usize,
    depth: u8,
    rows: &[framesage_ipc::ProcessSnapshot],
    children: &std::collections::HashMap<u32, Vec<usize>>,
    collapsed: &std::collections::HashSet<u32>,
    out: &mut Vec<TreeRow>,
    stack: &mut Vec<u32>,
) {
    let pid = rows[i].pid;
    if stack.contains(&pid) {
        // Cycle: shouldn't happen in practice (the kernel's parent linkage
        // doesn't make loops), but a malformed snapshot could trip us up.
        // Drop silently rather than loop forever.
        return;
    }
    let has_children = children.get(&pid).map(|v| !v.is_empty()).unwrap_or(false);
    out.push(TreeRow {
        pid,
        row_index: i,
        depth,
        has_children,
    });
    if !has_children || collapsed.contains(&pid) {
        return;
    }
    stack.push(pid);
    if let Some(kids) = children.get(&pid) {
        for &c in kids {
            visit_tree(c, depth + 1, rows, children, collapsed, out, stack);
        }
    }
    stack.pop();
}

/// Decode a process affinity bitmask into a human-readable CPU-range list:
/// `0x000000ff → "CPUs: 0–7"`, `0x0000800f → "CPUs: 0–3, 15"`. Renders
/// `"(none)"` for an empty mask. Used as the affinity column's hover
/// tooltip so the hex is scannable and the decode is on demand.
fn decode_affinity_mask(mask: u64) -> String {
    if mask == 0 {
        return "(none)".to_string();
    }
    let mut groups: Vec<String> = Vec::new();
    let mut run_start: Option<u32> = None;
    let mut last_set: Option<u32> = None;
    for i in 0..64u32 {
        let bit_set = (mask >> i) & 1 == 1;
        if bit_set {
            if run_start.is_none() {
                run_start = Some(i);
            }
            last_set = Some(i);
        } else if let Some(start) = run_start {
            let end = last_set.unwrap_or(start);
            push_run(&mut groups, start, end);
            run_start = None;
        }
    }
    // Final run if the highest bits are set.
    if let Some(start) = run_start {
        let end = last_set.unwrap_or(start);
        push_run(&mut groups, start, end);
    }
    format!("CPUs: {}", groups.join(", "))
}

fn push_run(out: &mut Vec<String>, start: u32, end: u32) {
    if start == end {
        out.push(start.to_string());
    } else {
        // En-dash, not hyphen — Process Lasso uses the same and it reads
        // better as a range.
        out.push(format!("{start}–{end}"));
    }
}

/// Top-N cores by load, formatted as a multi-line tooltip body:
/// `"Core 4: 87%\nCore 8: 73%\n..."`. Used as the perf-band aggregate
/// CPU% tooltip so a busy aggregate has obvious provenance — which
/// cores are actually hot.
fn format_top_cores(percents: &[u8], n: usize) -> String {
    if percents.is_empty() {
        return "(per-core data not available yet)".to_string();
    }
    let mut pairs: Vec<(usize, u8)> = percents.iter().copied().enumerate().collect();
    pairs.sort_by_key(|(_, p)| std::cmp::Reverse(*p));
    pairs
        .into_iter()
        .take(n)
        .map(|(i, p)| format!("Core {i}: {p}%"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn priority_class_label(raw: u32) -> &'static str {
    match raw {
        0x0000_0040 => "Idle",
        0x0000_4000 => "BelowNormal",
        0x0000_0020 => "Normal",
        0x0000_8000 => "AboveNormal",
        0x0000_0080 => "High",
        0x0000_0100 => "Realtime",
        _ => "—",
    }
}

/// Selected-process detail panel rendered below the Processes table.
///
/// Layout: title bar with exe + pid + close button, then a two-column field
/// grid (key on the left in mono-muted, value on the right in plain text),
/// then a row of action buttons that mirror the right-click context menu.
/// The detail card is the discoverability surface for users who never
/// right-click — Process Lasso ships the same set of actions both ways for
/// exactly this reason.
fn render_process_detail(
    ui: &mut egui::Ui,
    pid: u32,
    rows: &[framesage_ipc::ProcessSnapshot],
    profile_ids: &[String],
    action_queue: &mut Vec<ProcessAction>,
    close_flag: &mut bool,
) {
    let Some(p) = rows.iter().find(|p| p.pid == pid) else {
        // PID disappeared between snapshots — auto-close the panel rather
        // than render a misleading "unknown process" card.
        *close_flag = true;
        return;
    };

    theme::card().show(ui, |ui| {
        // Title row: exe name + PID badge + close.
        ui.horizontal(|ui| {
            ui.heading(&p.exe_name);
            theme::status_badge(theme::TEXT_MUTED).show(ui, |ui| {
                ui.colored_label(theme::TEXT_MUTED, format!("pid {}", p.pid));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("✕")
                    .on_hover_text("Close detail panel")
                    .clicked()
                {
                    *close_flag = true;
                }
            });
        });
        // Subtitle: the version-resource description, when the engine has
        // it. Sits directly under the heading so the relationship between
        // exe name and friendly name reads at a glance.
        if let Some(desc) = &p.description {
            ui.colored_label(theme::TEXT_MUTED, desc);
        }
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, true])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // Left column: metrics.
                    ui.vertical(|ui| {
                        detail_kv(ui, "CPU", format!("{} %", p.cpu_percent));
                        // Working set + the supporting peak / private values.
                        // The "growth gap" between current working set and
                        // peak working set is the classic memory-leak signal.
                        let memory_summary = if p.peak_working_set_bytes > 0 || p.private_bytes > 0
                        {
                            format!(
                                "{}  (peak {} · private {})",
                                format_bytes(p.memory_bytes),
                                format_bytes(p.peak_working_set_bytes),
                                format_bytes(p.private_bytes),
                            )
                        } else {
                            format_bytes(p.memory_bytes)
                        };
                        detail_kv(ui, "Memory", memory_summary);
                        detail_kv(ui, "Threads", p.threads.to_string());
                        detail_kv(
                            ui,
                            "Priority",
                            priority_class_label(p.priority_class_raw).to_string(),
                        );
                        detail_kv(ui, "Affinity", format!("{:#018x}", p.affinity_mask));
                    });
                    ui.separator();
                    // Right column: framesage state.
                    ui.vertical(|ui| {
                        let profile_text = match &p.managed_profile {
                            Some(id) => {
                                if p.matched_rule_note.is_some() {
                                    format!("★ {id}  (Rule)")
                                } else {
                                    id.clone()
                                }
                            }
                            None => "—".to_string(),
                        };
                        detail_kv(ui, "Profile", profile_text);
                        detail_kv(ui, "User", p.user.as_deref().unwrap_or("—").to_string());
                        let rule_note = p
                            .matched_rule_note
                            .as_deref()
                            .filter(|n| !n.is_empty())
                            .unwrap_or("—");
                        detail_kv(ui, "Rule note", rule_note.to_string());
                        let probal = if p.restrained_by_probalance {
                            "● restrained"
                        } else {
                            "—"
                        };
                        detail_kv(ui, "ProBalance", probal.to_string());
                    });
                });
            });

        ui.add_space(6.0);
        ui.separator();
        ui.add_space(6.0);

        // Action row — same submenus as the table's right-click context menu.
        let exe = p.exe_name.clone();
        let pid = p.pid;
        ui.horizontal(|ui| {
            ui.menu_button("Set priority", |ui| {
                for (label, class) in PRIORITY_CHOICES.iter() {
                    if ui.button(*label).clicked() {
                        action_queue.push(ProcessAction::SetPriority { pid, class: *class });
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Apply profile now", |ui| {
                for pid_name in profile_ids {
                    if ui.button(pid_name).clicked() {
                        action_queue.push(ProcessAction::ApplyProfileForeground {
                            profile: pid_name.clone(),
                        });
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Create rule for this exe", |ui| {
                for pid_name in profile_ids {
                    if ui.button(pid_name).clicked() {
                        action_queue.push(ProcessAction::CreateRule {
                            exe_name: exe.clone(),
                            profile: pid_name.clone(),
                        });
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Set affinity", |ui| {
                if ui.button("X3D CCD").clicked() {
                    action_queue.push(ProcessAction::SetAffinity {
                        pid,
                        selector: framesage_core::CpuSelector::Kind(
                            framesage_core::CoreKind::Cache,
                        ),
                    });
                }
                if ui.button("Non-X3D CCD").clicked() {
                    action_queue.push(ProcessAction::SetAffinity {
                        pid,
                        selector: framesage_core::CpuSelector::Kind(
                            framesage_core::CoreKind::Performance,
                        ),
                    });
                }
                if ui.button("All cores").clicked() {
                    action_queue.push(ProcessAction::SetAffinity {
                        pid,
                        selector: framesage_core::CpuSelector::All,
                    });
                }
                if ui.button("Custom…").clicked() {
                    action_queue.push(ProcessAction::RequestAffinityPicker {
                        pid,
                        exe_name: exe.clone(),
                    });
                }
            });
            ui.separator();
            if ui.button("Suspend").clicked() {
                action_queue.push(ProcessAction::Suspend { pid });
            }
            if ui.button("Resume").clicked() {
                action_queue.push(ProcessAction::Resume { pid });
            }
            if ui
                .add(egui::Button::new(
                    egui::RichText::new("Terminate…").color(theme::ERROR),
                ))
                .clicked()
            {
                action_queue.push(ProcessAction::RequestTerminate {
                    pid,
                    exe_name: exe.clone(),
                });
            }
        });
    });
}

/// Two-line key/value cell for the detail panel. Key in a fixed-width muted
/// font on top, value in the regular body font underneath. Used in
/// `render_process_detail` so the grid wraps gracefully on narrow windows.
fn detail_kv(ui: &mut egui::Ui, key: &str, value: String) {
    ui.vertical(|ui| {
        ui.label(
            egui::RichText::new(key.to_uppercase())
                .small()
                .strong()
                .color(theme::TEXT_MUTED)
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
fn format_local_hms(t: std::time::SystemTime) -> String {
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

fn format_bytes(b: u64) -> String {
    if b >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", b as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024 * 1024 {
        format!("{} MB", b / (1024 * 1024))
    } else if b >= 1024 {
        format!("{} KB", b / 1024)
    } else {
        format!("{b} B")
    }
}

/// Hero strip at the top of the Status tab. One row, three signals: engine
/// state (with colored dot), policy summary (rules + default profile),
/// FrameSage-wide "what's happening right now" sentence on the right.
fn render_status_hero(ui: &mut egui::Ui, s: &StatusSnapshot) {
    theme::hero().show(ui, |ui| {
        ui.horizontal(|ui| {
            let (dot_color, headline) = if s.paused {
                (theme::WARNING, "Paused")
            } else {
                (theme::SUCCESS, "Running")
            };
            // Engine state with a coloured dot.
            ui.label(egui::RichText::new("\u{25cf}").color(dot_color).size(14.0));
            ui.label(
                egui::RichText::new(headline)
                    .size(18.0)
                    .strong()
                    .color(theme::TEXT),
            );
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new(format!("{} rules", s.policy.rules.len()))
                    .color(theme::TEXT_MUTED),
            );
            ui.label(egui::RichText::new("·").color(theme::TEXT_MUTED));
            ui.label(
                egui::RichText::new(format!(
                    "default: {}",
                    display_profile_id(&s.policy.default_profile.0)
                ))
                .color(theme::TEXT_MUTED),
            );
            if let Some(bg) = &s.policy.background_profile {
                ui.label(egui::RichText::new("·").color(theme::TEXT_MUTED));
                ui.label(
                    egui::RichText::new(format!("background: {}", display_profile_id(&bg.0)))
                        .color(theme::TEXT_MUTED),
                );
            }
        });
    });
}

/// Single-card summary of the active profile: name, description, and the
/// three knobs the user cares about most at a glance.
fn render_active_profile_summary(ui: &mut egui::Ui, s: &StatusSnapshot) {
    let Some(p) = &s.active_profile else {
        ui.colored_label(theme::TEXT_MUTED, "No profile applied yet.");
        return;
    };
    ui.label(
        egui::RichText::new(display_profile_id(&p.id.0))
            .size(17.0)
            .strong()
            .color(theme::ACCENT),
    );
    if !p.description.is_empty() {
        ui.add_space(2.0);
        ui.colored_label(theme::TEXT_MUTED, &p.description);
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
fn render_foreground_summary(ui: &mut egui::Ui, fg: &ForegroundSnapshot) {
    ui.label(
        egui::RichText::new(&fg.exe_name)
            .size(17.0)
            .strong()
            .color(theme::TEXT),
    );
    ui.add_space(2.0);
    if !fg.title.is_empty() {
        ui.colored_label(theme::TEXT_MUTED, &fg.title);
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
fn render_readonly_banner(ui: &mut egui::Ui, body: &str) {
    theme::banner(theme::WARNING).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.colored_label(theme::WARNING, egui::RichText::new("⚠").strong().size(14.0));
            ui.label(
                egui::RichText::new("Read-only mode")
                    .strong()
                    .color(theme::TEXT),
            );
            ui.colored_label(theme::TEXT_MUTED, format!("— {body}"));
        });
    });
}

/// Truncate long paths for display — keep the drive letter and the final
/// two components, ellipsise the middle. Avoids the path field exploding
/// the card width on deep installs.
fn short_path(path: &str) -> String {
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
fn kv_grid_row(ui: &mut egui::Ui, key: &str, value: String) {
    ui.label(egui::RichText::new(key).color(theme::TEXT_MUTED).size(12.0));
    ui.label(value);
    ui.end_row();
}

/// Permanent performance band rendered above every tab. Two numeric
/// readouts (CPU%, Memory) plus a 60-sample sparkline. Designed to compress
/// to ~28 px of vertical space — enough to read at a glance, not enough to
/// dominate the tab content below it.
fn render_perf_band(
    ui: &mut egui::Ui,
    metrics: &framesage_ipc::SystemMetrics,
    history: &[(u8, u8)],
) {
    ui.horizontal(|ui| {
        // Left cluster: the live numeric readouts. Color-coded by intensity
        // so the band visually flags contention without the user having to
        // read the number.
        ui.label(
            egui::RichText::new("CPU")
                .color(theme::TEXT_MUTED)
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
                .color(theme::TEXT_MUTED)
                .size(11.0),
        );
        let mem_color = if mem_percent > 90 {
            theme::ERROR
        } else if mem_percent > 75 {
            theme::WARNING
        } else {
            theme::TEXT
        };
        ui.label(
            egui::RichText::new(format!("{}%", mem_percent))
                .color(mem_color)
                .strong()
                .size(15.0),
        );
        ui.colored_label(
            theme::TEXT_MUTED,
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
            let desired = egui::vec2(280.0, 22.0);
            let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());
            draw_sparkline(ui.painter(), rect, history);

            // Per-core matrix between sparkline and MEM. Right-to-left
            // layout means this allocation appears to the *left* of the
            // sparkline. Skipped on first sample (engine hasn't accumulated
            // two yet) or if the kernel refused per-CPU info.
            if !metrics.per_core_cpu_percent.is_empty() {
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
fn draw_per_core_matrix(ui: &mut egui::Ui, percents: &[u8]) {
    let cores = percents.len().min(PER_CORE_MAX_BARS);
    let total_w = (PER_CORE_BAR_W + PER_CORE_BAR_GAP) * cores as f32 - PER_CORE_BAR_GAP;
    let desired = egui::vec2(total_w.max(1.0), 22.0);
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter();

    // Background tray for the bars — visual anchor so bars at 0% still
    // appear inside a "well" rather than floating against the band.
    painter.rect_filled(rect, 3.0, theme::SURFACE);

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
fn draw_sparkline(painter: &egui::Painter, rect: egui::Rect, history: &[(u8, u8)]) {
    use egui::epaint::PathShape;
    use egui::{Color32, Stroke};

    // Background frame so the line has something to anchor against.
    painter.rect_filled(rect, 3.0, theme::SURFACE);

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
    let cpu_stroke = Stroke::new(1.5, theme::ACCENT);
    let mem_stroke = Stroke::new(1.0, Color32::from_rgb(140, 90, 200));

    // Filled area under the CPU line (the more eye-catching of the two,
    // matching its priority for the user).
    let mut cpu_fill: Vec<egui::Pos2> = cpu_points.clone();
    cpu_fill.push(egui::pos2(rect.right(), rect.bottom()));
    cpu_fill.push(egui::pos2(rect.left(), rect.bottom()));
    painter.add(PathShape::convex_polygon(
        cpu_fill,
        Color32::from_rgba_unmultiplied(50, 130, 246, 30),
        Stroke::NONE,
    ));

    painter.add(PathShape::line(mem_points, mem_stroke));
    painter.add(PathShape::line(cpu_points, cpu_stroke));
}

/// Permanent activity strip — last ~5 engine actions in one horizontal
/// scroller at the bottom. Mirrors the Status tab's Recent Activity, but
/// compact and always visible regardless of which tab is open.
/// One-line status bar at the very bottom of the window. Shows engine state,
/// process counts, version, and the last-action echo. Sections are separated
/// by thin dividers in `TEXT_DIM` so the eye groups them naturally.
fn render_status_bar(
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
            (theme::ERROR, "Disconnected")
        } else if paused {
            (theme::WARNING, "Paused")
        } else if manual_override.is_some() {
            (theme::ACCENT, "Manual")
        } else {
            (theme::SUCCESS, "Running")
        };
        ui.colored_label(
            state_color,
            egui::RichText::new(format!("● {state_text}")).strong(),
        );

        if let Some(id) = manual_override {
            ui.colored_label(theme::TEXT_MUTED, "·");
            ui.colored_label(theme::TEXT_MUTED, format!("override: {}", id.0));
        }

        ui.colored_label(theme::TEXT_MUTED, "·");
        let managed_str = if managed_count > 0 {
            format!("{process_count} processes ({managed_count} managed)")
        } else {
            format!("{process_count} processes")
        };
        ui.colored_label(theme::TEXT_MUTED, managed_str);

        // Last action echo on the right; trims long messages so a noisy
        // error doesn't break the layout. Version anchors the far right.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.colored_label(theme::TEXT_MUTED, format!("v{}", env!("CARGO_PKG_VERSION")));
            if let Some(text) = last_action {
                ui.colored_label(theme::TEXT_MUTED, "·");
                let max_chars = 80;
                let trimmed = if text.chars().count() > max_chars {
                    let mut t: String = text.chars().take(max_chars - 1).collect();
                    t.push('…');
                    t
                } else {
                    text.to_string()
                };
                let color = if text.contains("error") {
                    theme::ERROR
                } else {
                    theme::TEXT_MUTED
                };
                ui.colored_label(color, trimmed);
            }
        });
    });
}

fn render_activity_strip(ui: &mut egui::Ui, recent: &[String]) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("ACTIVITY")
                .color(theme::TEXT_MUTED)
                .size(10.0)
                .strong(),
        );
        ui.add_space(8.0);
        if recent.is_empty() {
            ui.colored_label(theme::TEXT_MUTED, "no events yet");
            return;
        }
        egui::ScrollArea::horizontal()
            .max_width(f32::INFINITY)
            .show(ui, |ui| {
                for (i, line) in recent.iter().enumerate() {
                    if i > 0 {
                        ui.colored_label(theme::TEXT_MUTED, "·");
                    }
                    let color = if line.contains("probalance") {
                        theme::WARNING
                    } else if line.contains("game-x3d") {
                        theme::ACCENT
                    } else {
                        theme::TEXT
                    };
                    ui.colored_label(color, line);
                }
            });
    });
}

/// Recent activity feed. Treats consecutive identical lines as one (with a
/// "× N" suffix) so the user sees signal not noise. Most recent first.
fn render_recent_activity(ui: &mut egui::Ui, recent: &[String]) {
    if recent.is_empty() {
        ui.colored_label(theme::TEXT_MUTED, "No activity yet.");
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
                        ui.label(egui::RichText::new("›").color(theme::TEXT_DIM).monospace());
                        ui.label(*label);
                        if *n > 1 {
                            ui.colored_label(
                                theme::TEXT_MUTED,
                                egui::RichText::new(format!("× {n}")).small(),
                            );
                        }
                    });
                }
            });
    });
}

fn render_profile_body(ui: &mut egui::Ui, p: &Profile) {
    if !p.description.is_empty() {
        ui.colored_label(theme::TEXT_MUTED, &p.description);
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
        ui.colored_label(theme::TEXT_DIM, "Game Mode not requested by this profile.");
    }
}

/// Summarise a long list of ids as "N items: first, second, third…" so the
/// profile card stays compact. Full list is visible in the editor anyway.
fn format_count_summary(items: &[String]) -> String {
    let n = items.len();
    if n <= 3 {
        return items.join(", ");
    }
    let preview: Vec<&str> = items.iter().take(3).map(|s| s.as_str()).collect();
    format!("{n} entries — {}, …", preview.join(", "))
}

/// Human label for booleans shown to users. Used in the read-only profile
/// viewer where `true`/`false` reads as developer output, not as a setting.
fn yes_no(b: bool) -> &'static str {
    if b {
        "Yes"
    } else {
        "No"
    }
}

/// Title-case a profile id for display. The underlying id stays as the user
/// authored it (e.g. `"game-x3d"` so rules and policy.json round-trip
/// stably), but the UI shows `"Game X3D"`. Splits on `-` and `_`, upper-cases
/// the first letter of each token, and special-cases hardware acronyms like
/// `x3d` so the display reads as branded text instead of slug. Pure function.
fn display_profile_id(raw: &str) -> String {
    raw.split(['-', '_'])
        .filter(|s| !s.is_empty())
        .map(|token| {
            // Acronyms / vendor jargon that should stay shouty.
            let upper = token.to_ascii_uppercase();
            if matches!(
                upper.as_str(),
                "X3D" | "CPU" | "GPU" | "RAM" | "IO" | "CCD" | "AMD" | "NV" | "DLSS"
            ) {
                return upper;
            }
            let mut chars = token.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        build_tree_view, classify_row, decode_affinity_mask, display_profile_id, format_top_cores,
        row_marker_color, RowState,
    };
    use std::collections::HashSet;

    #[test]
    fn profile_id_display_handles_common_cases() {
        assert_eq!(display_profile_id("perf"), "Perf");
        assert_eq!(display_profile_id("eco"), "Eco");
        assert_eq!(display_profile_id("game-x3d"), "Game X3D");
        assert_eq!(display_profile_id("low_power"), "Low Power");
        assert_eq!(display_profile_id("cpu-bound"), "CPU Bound");
        assert_eq!(display_profile_id(""), "");
    }

    fn make_proc(pid: u32) -> framesage_ipc::ProcessSnapshot {
        framesage_ipc::ProcessSnapshot {
            pid,
            parent_pid: 0,
            exe_name: "test.exe".into(),
            exe_path: String::new(),
            description: None,
            company: None,
            user: None,
            priority_class_raw: 0x20, // NORMAL_PRIORITY_CLASS
            affinity_mask: 0xFFFF,
            cpu_percent: 0,
            memory_bytes: 0,
            peak_working_set_bytes: 0,
            private_bytes: 0,
            threads: 1,
            matched_rule_note: None,
            managed_profile: None,
            restrained_by_probalance: false,
        }
    }

    #[test]
    fn classify_row_foreground_beats_everything() {
        // Even if a process is also restrained or managed, the foreground
        // designation wins — the user expects the focused row to read as
        // "this is what you're using right now."
        let mut p = make_proc(42);
        p.restrained_by_probalance = true;
        p.managed_profile = Some("game-x3d".into());
        assert_eq!(classify_row(&p, Some(42)), RowState::Foreground);
    }

    #[test]
    fn classify_row_restrained_beats_managed() {
        let mut p = make_proc(7);
        p.restrained_by_probalance = true;
        p.managed_profile = Some("perf".into());
        assert_eq!(classify_row(&p, Some(1)), RowState::Restrained);
    }

    #[test]
    fn classify_row_managed_when_only_profile() {
        let mut p = make_proc(7);
        p.managed_profile = Some("perf".into());
        assert_eq!(classify_row(&p, Some(1)), RowState::Managed);
    }

    #[test]
    fn classify_row_default_otherwise() {
        let p = make_proc(7);
        assert_eq!(classify_row(&p, Some(1)), RowState::Default);
        assert_eq!(classify_row(&p, None), RowState::Default);
    }

    #[test]
    fn row_marker_default_paints_nothing() {
        // Default rows leave the marker column empty so the colored ones
        // pop. Locking this in — accidental "always-on" would defeat the
        // gutter's purpose.
        assert!(row_marker_color(RowState::Default).is_none());
        assert!(row_marker_color(RowState::Foreground).is_some());
        assert!(row_marker_color(RowState::Restrained).is_some());
        assert!(row_marker_color(RowState::Managed).is_some());
    }

    #[test]
    fn decode_affinity_mask_collapses_contiguous_runs() {
        assert_eq!(decode_affinity_mask(0x0000_00ff), "CPUs: 0–7");
        assert_eq!(decode_affinity_mask(0x0000_800f), "CPUs: 0–3, 15");
        assert_eq!(decode_affinity_mask(0x0000_0001), "CPUs: 0");
        assert_eq!(decode_affinity_mask(0xffff_ffff), "CPUs: 0–31");
        // Singletons separated by gaps don't collapse.
        assert_eq!(decode_affinity_mask(0b1010_1010), "CPUs: 1, 3, 5, 7");
    }

    #[test]
    fn decode_affinity_mask_empty_renders_none() {
        assert_eq!(decode_affinity_mask(0), "(none)");
    }

    #[test]
    fn decode_affinity_mask_includes_high_bits() {
        // Bit 63 alone — last-run handling at the loop boundary.
        assert_eq!(decode_affinity_mask(1u64 << 63), "CPUs: 63");
        // Top byte set as a contiguous block.
        assert_eq!(decode_affinity_mask(0xff00_0000_0000_0000), "CPUs: 56–63");
    }

    #[test]
    fn format_top_cores_sorts_descending_and_caps() {
        let pct = vec![10, 80, 30, 95, 5, 50, 70, 20];
        let s = format_top_cores(&pct, 3);
        assert_eq!(s, "Core 3: 95%\nCore 1: 80%\nCore 6: 70%");
    }

    #[test]
    fn format_top_cores_handles_empty() {
        assert_eq!(
            format_top_cores(&[], 5),
            "(per-core data not available yet)"
        );
    }

    fn proc_with_parent(pid: u32, parent_pid: u32) -> framesage_ipc::ProcessSnapshot {
        let mut p = make_proc(pid);
        p.parent_pid = parent_pid;
        p
    }

    #[test]
    fn build_tree_view_groups_children_under_parents() {
        // Tree:
        //   1 (root)
        //     └ 2
        //         └ 4
        //     └ 3
        //   5 (root, parent=99 missing → orphan = root)
        let rows = vec![
            proc_with_parent(1, 0),
            proc_with_parent(2, 1),
            proc_with_parent(3, 1),
            proc_with_parent(4, 2),
            proc_with_parent(5, 99),
        ];
        let collapsed = HashSet::new();
        let tree = build_tree_view(&rows, &collapsed, |a, b| a.pid.cmp(&b.pid));
        // With ascending-PID sort within siblings, expected DFS order is:
        // 1, 2, 4, 3, 5
        let pids: Vec<u32> = tree.iter().map(|t| t.pid).collect();
        assert_eq!(pids, vec![1, 2, 4, 3, 5]);
        let depths: Vec<u8> = tree.iter().map(|t| t.depth).collect();
        assert_eq!(depths, vec![0, 1, 2, 1, 0]);
    }

    #[test]
    fn build_tree_view_respects_collapsed() {
        // Collapsing pid 1 should hide 2, 3, 4 — they all sit under 1.
        let rows = vec![
            proc_with_parent(1, 0),
            proc_with_parent(2, 1),
            proc_with_parent(3, 1),
            proc_with_parent(4, 2),
            proc_with_parent(5, 0),
        ];
        let mut collapsed = HashSet::new();
        collapsed.insert(1);
        let tree = build_tree_view(&rows, &collapsed, |a, b| a.pid.cmp(&b.pid));
        let pids: Vec<u32> = tree.iter().map(|t| t.pid).collect();
        assert_eq!(pids, vec![1, 5]);
        // Pid 1 still has children even though they're hidden; the toggle
        // glyph needs to render.
        assert!(tree[0].has_children);
        assert!(!tree[1].has_children);
    }

    #[test]
    fn build_tree_view_treats_missing_parents_as_orphans() {
        // PID 7's parent (42) isn't in the snapshot → 7 should be a root.
        let rows = vec![proc_with_parent(1, 0), proc_with_parent(7, 42)];
        let tree = build_tree_view(&rows, &HashSet::new(), |a, b| a.pid.cmp(&b.pid));
        assert_eq!(
            tree.iter()
                .filter(|t| t.depth == 0)
                .map(|t| t.pid)
                .collect::<Vec<_>>(),
            vec![1, 7]
        );
    }

    #[test]
    fn build_tree_view_breaks_cycles() {
        // Pathological cycle (shouldn't happen in real kernel state but
        // defending against it): 1 → 2 → 1.
        let rows = vec![proc_with_parent(1, 2), proc_with_parent(2, 1)];
        let tree = build_tree_view(&rows, &HashSet::new(), |a, b| a.pid.cmp(&b.pid));
        // Both have a "parent" present so neither is a top-level root by
        // our normal rule — `roots` ends up empty. The output should be
        // empty rather than infinite-looping.
        assert!(tree.is_empty());
    }

    #[test]
    fn build_tree_view_sort_applies_to_siblings_only() {
        // Sort by reverse pid; expect siblings to flip but tree structure
        // intact.
        let rows = vec![
            proc_with_parent(1, 0),
            proc_with_parent(2, 1),
            proc_with_parent(3, 1),
        ];
        let tree = build_tree_view(&rows, &HashSet::new(), |a, b| b.pid.cmp(&a.pid));
        let pids: Vec<u32> = tree.iter().map(|t| t.pid).collect();
        // Root is unchanged (only one root); siblings of 1 emit in
        // reverse-PID order → 3, then 2.
        assert_eq!(pids, vec![1, 3, 2]);
    }
}

/// Legacy fixed-width KV row, kept around in case a future editor section
/// needs it (mixed widget rows where `egui::Grid` doesn't help). Currently
/// unused since the polish pass; `#[allow(dead_code)]` avoids a warning.
#[allow(dead_code)]
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
            |v| v.to_string(),
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
            |v| v.to_string(),
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
            |v| v.to_string(),
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
            |v| v.to_string(),
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

    // Focus Assist has no documented user-mode API on current Windows builds
    // (Microsoft's own Settings app uses an undocumented COM interface). The
    // planner rejects this action with `NotImplemented` at apply time, so
    // we surface that here as a disabled control with a one-line explanation
    // — exposing a checkbox that silently does nothing was the prior bug.
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 16.0],
            egui::Label::new(egui::RichText::new("Focus assist").weak()),
        );
        ui.add_enabled(
            false,
            egui::Label::new(
                egui::RichText::new("disabled — no documented Windows API")
                    .color(theme::TEXT_MUTED),
            ),
        );
    });
    // Clear any value a previous build had stored so it doesn't haunt the
    // policy file as a value that will never be honoured.
    gm.focus_assist = None;

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
            None => "—".to_owned(),
            Some(v) => fmt(v),
        };
        egui::ComboBox::from_id_source(("option-combo", label))
            .selected_text(selected_text)
            .show_ui(ui, |ui| {
                ui.selectable_value(current, None, "— (unset)");
                for v in variants {
                    ui.selectable_value(current, Some(*v), fmt(v));
                }
            });
    });
}

fn format_cpu_selector(sel: Option<&CpuSelector>) -> String {
    match sel {
        None => "—".to_owned(),
        Some(CpuSelector::All) => "All cores".to_owned(),
        Some(CpuSelector::Kind(k)) => k.to_string(),
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
            Self::Unset => "— (unset)",
            Self::All => "All cores",
            Self::Kind => "By core kind",
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
                    .selected_text(k.to_string())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            k,
                            CoreKind::Performance,
                            CoreKind::Performance.to_string(),
                        );
                        ui.selectable_value(
                            k,
                            CoreKind::Efficiency,
                            CoreKind::Efficiency.to_string(),
                        );
                        ui.selectable_value(k, CoreKind::Cache, CoreKind::Cache.to_string());
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

// ─── FrameSage logo ─────────────────────────────────────────────────────────

/// Render the FrameSage brand mark at 64×64: dark navy disc, accent-cyan
/// ring, large stylised "F" sitting on the disc. Used as the source for
/// every visual that needs the logo — system-tray icon, eframe window icon,
/// build-time .ico for the .exe's taskbar / Alt-Tab thumbnail.
///
/// Returns (rgba_bytes, width, height). RGBA is row-major, top-down, 8-bit
/// per channel, premultiplied-by-alpha-friendly (egui and tray-icon both
/// expect plain RGBA8888).
fn framesage_logo_rgba() -> (Vec<u8>, u32, u32) {
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

#[cfg(windows)]
fn build_icon() -> Icon {
    let (rgba, w, h) = framesage_logo_rgba();
    Icon::from_rgba(rgba, w, h).expect("hand-rolled icon is valid RGBA")
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
fn build_tray(commands: &TrayCommands, egui_ctx: egui::Context) -> anyhow::Result<TrayIcon> {
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
                if ev.id == ids.open {
                    cmds_menu.show_window.store(true, Ordering::Relaxed);
                } else if ev.id == ids.hide {
                    cmds_menu.hide_window.store(true, Ordering::Relaxed);
                } else if ev.id == ids.exit {
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
            let (kind, label) = match &event {
                Event::ForegroundChanged {
                    foreground,
                    profile,
                } => (
                    EventKind::Foreground,
                    format!(
                        "{} -> {} (pid {})",
                        foreground.exe_name, profile, foreground.pid
                    ),
                ),
                Event::Paused => (EventKind::Engine, "engine paused".into()),
                Event::Resumed => (EventKind::Engine, "engine resumed".into()),
                Event::ProBalanceRestrained {
                    pid,
                    exe_name,
                    from_class,
                    to_class,
                } => (
                    EventKind::ProBalanceRestrained,
                    format!(
                        "probalance restrained {} (pid {}) {:#x} -> {:#x}",
                        exe_name, pid, from_class, to_class
                    ),
                ),
                Event::ProBalanceRestored {
                    pid,
                    exe_name,
                    restored_class,
                } => (
                    EventKind::ProBalanceRestored,
                    format!(
                        "probalance restored {} (pid {}) -> {:#x}",
                        exe_name, pid, restored_class
                    ),
                ),
            };
            let mut s = state.lock().unwrap();
            s.recent.push(RecentEvent {
                at: std::time::SystemTime::now(),
                kind,
                label,
            });
            // Cap the event buffer. Without this it grows forever (one
            // entry per foreground change, every 250 ms in the worst case).
            // 1000 entries is ~5 minutes of constant flicker — plenty for
            // any UI consumer (the Activity strip shows 5, the Recent
            // Activity panel shows 20).
            const MAX_RECENT: usize = 1000;
            if s.recent.len() > MAX_RECENT {
                let drop = s.recent.len() - MAX_RECENT;
                s.recent.drain(0..drop);
            }
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

/// User-session foreground reporter — the workaround for session-0
/// isolation that lets a LocalSystem-installed service know what's
/// foregrounded in the user's desktop.
///
/// Background: `GetForegroundWindow` returns null when called from
/// session 0 (where Windows services run). The engine therefore can't
/// see the foreground when it polls itself. This loop runs in the
/// user's session (i.e., wherever the tray runs), polls
/// `framesage_sys::foreground::current()` every 250ms, and forwards
/// the result over the admin pipe as `Request::ReportForeground` or
/// `Request::ReportNoForeground`. The engine then uses the reported
/// value in its tick loop.
///
/// Deliberately tolerant of transient pipe failures: if the service
/// isn't running yet (we start before it does on logon), we silently
/// retry on the next tick. No backoff is needed — every 250ms is fine
/// Poll `Request::ListProcesses` over the status pipe every 1 s and push
/// the result (plus paired system metrics) into `AppState`. Wakes the egui
/// runtime each refresh so the Processes tab and the performance band
/// update even when no other input arrives.
#[cfg(windows)]
fn processes_poll_loop(
    state: Arc<Mutex<AppState>>,
    ctx: egui::Context,
    window_visible: Arc<AtomicBool>,
) {
    // Cadence depends on whether the window is visible. Hidden window =
    // poll 8× less often (and skip the egui repaint wake entirely). The
    // user reported FrameSage burning CPU; this is the largest single
    // contributor — 120-row table render every 1 s × always-on = the
    // bulk of the idle CPU floor.
    let visible_interval = std::time::Duration::from_millis(1000);
    let hidden_interval = std::time::Duration::from_millis(8000);
    loop {
        let visible = window_visible.load(Ordering::Relaxed);
        match send_processes_and_status_blocking() {
            Ok((snapshots, system, status)) => {
                let mem_percent: u8 = if system.memory_total_bytes > 0 {
                    ((system.memory_used_bytes as u128 * 100 / system.memory_total_bytes as u128)
                        .min(100)) as u8
                } else {
                    0
                };
                let cpu_for_history = system.cpu_percent;
                let mut s = state.lock().unwrap();
                s.processes = snapshots;
                s.system = system;
                // Refresh the cached Status every tick so the UI never
                // shows stale paused/policy state. Without this, clicking
                // Pause/Resume or Enable-ProBalance updates the engine but
                // the UI keeps showing the value cached at first connect.
                s.status = Some(status);
                s.system_history.push_back((cpu_for_history, mem_percent));
                while s.system_history.len() > SYSTEM_HISTORY_LEN {
                    s.system_history.pop_front();
                }
                drop(s);
                if visible {
                    ctx.request_repaint();
                }
            }
            Err(_) => {
                // Service down or pipe busy — skip this tick. The connect
                // status drives the UI's "Disconnected" pill via the
                // existing background_loop; no need to surface the failure
                // here too.
            }
        }
        std::thread::sleep(if visible {
            visible_interval
        } else {
            hidden_interval
        });
    }
}

/// One status-pipe round-trip per tick: send ListProcesses, then Status,
/// read both responses. Reuses a single pipe instance so we only burn one
/// ACL check per second. The Status fetch is what keeps the tray's view
/// of `paused` + `policy.probalance.enabled` in sync with the engine —
/// without it, the UI shows whatever values were current at first connect.
#[cfg(windows)]
fn send_processes_and_status_blocking() -> anyhow::Result<(
    Vec<framesage_ipc::ProcessSnapshot>,
    framesage_ipc::SystemMetrics,
    framesage_ipc::StatusSnapshot,
)> {
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};

    let pipe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(framesage_ipc::PIPE_NAME_STATUS)?;
    let mut writer = pipe.try_clone()?;
    let mut reader = BufReader::new(pipe);

    // ListProcesses
    let mut buf = serde_json::to_vec(&framesage_ipc::Request::ListProcesses)?;
    buf.push(b'\n');
    writer.write_all(&buf)?;
    writer.flush()?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let (snapshots, system) = match serde_json::from_str::<framesage_ipc::Response>(&line)? {
        framesage_ipc::Response::Processes { snapshots, system } => (snapshots, system),
        other => {
            return Err(anyhow::anyhow!(
                "expected Processes response, got {other:?}"
            ))
        }
    };

    // Status — same pipe, same handler, just a second request.
    let mut buf = serde_json::to_vec(&framesage_ipc::Request::Status)?;
    buf.push(b'\n');
    writer.write_all(&buf)?;
    writer.flush()?;
    line.clear();
    reader.read_line(&mut line)?;
    let status = match serde_json::from_str::<framesage_ipc::Response>(&line)? {
        framesage_ipc::Response::Status(s) => *s,
        other => return Err(anyhow::anyhow!("expected Status response, got {other:?}")),
    };

    Ok((snapshots, system, status))
}

/// even if the service is down for minutes.
#[cfg(windows)]
fn foreground_reporter_loop() {
    let interval = std::time::Duration::from_millis(250);
    let mut last_pid: Option<u32> = None;
    loop {
        let req = match framesage_sys::foreground::current() {
            Ok(Some(fg)) => {
                last_pid = Some(fg.pid);
                Some(Request::ReportForeground {
                    pid: fg.pid,
                    exe_name: fg.exe_name,
                    path: fg.path,
                    title: fg.title,
                })
            }
            Ok(None) => {
                let needs_report = last_pid.is_some();
                last_pid = None;
                if needs_report {
                    Some(Request::ReportNoForeground)
                } else {
                    // Still no foreground; don't spam the service with
                    // duplicate "no foreground" reports. The engine
                    // already saw the last None.
                    None
                }
            }
            Err(_) => None,
        };
        if let Some(req) = req {
            // Best-effort: drop the result. Service might be starting,
            // not yet running, restarting, etc. The next tick will retry.
            let _ = send_request_blocking(framesage_ipc::PIPE_NAME_ADMIN, &req);
        }
        std::thread::sleep(interval);
    }
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
            .with_inner_size([640.0, 560.0])
            .with_min_inner_size([520.0, 420.0])
            // Cap the window aggressively. egui's `persistence` feature
            // restores whatever last_size the prior session wrote — including
            // the multi-thousand-pixel sizes that a pre-DPI-manifest drag
            // between monitors used to produce. The manifest closes the root
            // cause, but a stale persisted value would still come back to
            // bite us, so we cap.
            .with_max_inner_size([1600.0, 1400.0])
            .with_title(if elevated {
                "FrameSage (admin)"
            } else {
                "FrameSage"
            })
            .with_icon(build_window_icon())
            .with_close_button(true),
        ..Default::default()
    };

    let cmds_for_app = commands.clone();
    eframe::run_native(
        "FrameSage",
        options,
        Box::new(move |cc| Ok(Box::new(FramesageApp::new(cc, cmds_for_app, elevated)))),
    )
}

/// Build the eframe viewport icon shown in the title bar + taskbar. Mirrors
/// the tray-icon rendering so the brand reads consistently. Returns an
/// `egui::IconData` instead of `tray_icon::Icon` because eframe owns that
/// type; the pixel data is otherwise identical to `build_icon()`.
fn build_window_icon() -> egui::IconData {
    let (rgba, w, h) = framesage_logo_rgba();
    egui::IconData {
        rgba,
        width: w,
        height: h,
    }
}

/// Open a file, folder, or URL in the OS shell handler. Best-effort: we
/// silently drop spawn errors because there's no useful recovery — the user
/// can always navigate manually. The `cmd /c start "" <target>` form is the
/// reliable cross-input way to do this on Windows (handles paths with
/// spaces and URLs identically). On non-Windows hosts this is a no-op so
/// the rest of the binary still cross-compiles.
fn open_in_shell(target: &str) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "", target])
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = target; // keep the param used on non-Windows
    }
}

/// Run `framesage.exe <subcommand>` in a new console window so the user can
/// read its output. Used by `Tools → Run topology` — we don't have a tab
/// for topology yet, and the CLI's pretty-printed table is the canonical
/// view. Best-effort; silently no-ops if framesage.exe isn't next to the
/// tray binary.
fn spawn_framesage_subcommand(subcommand: &str) {
    #[cfg(windows)]
    {
        let Some(framesage) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("framesage.exe")))
        else {
            return;
        };
        if !framesage.exists() {
            return;
        }
        let _ = std::process::Command::new("cmd")
            .args(["/c", "start", "cmd", "/k"])
            .arg(framesage)
            .arg(subcommand)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = subcommand;
    }
}
