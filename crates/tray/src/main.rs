//! framesage-tray.exe — system-tray icon + monitor window.
//!
//! v0.2 ships a real persistent tray icon via `tray-icon`. Left-click toggles
//! the monitor window, right-click reveals a menu (*Open* / *Hide* / *Exit*).
//! Closing the window hides it to the tray rather than killing the process —
//! "Exit framesage tray" from the menu is the only way to actually quit.
//!
//! # Layering (item 3.8)
//!
//! Depends on `framesage-core` + `framesage-ipc` + `framesage-sys` (the
//! last for the session-0 foreground workaround in `ipc_client::
//! foreground_reporter_loop`). It does NOT depend on `framesage-engine`
//! — every engine-side action is reached via the IPC protocol so the tray
//! stays a thin UI client. See `ARCHITECTURE.md` at the repo root.
//!
//! The window opens an IPC connection to the service on startup, subscribes
//! to events, and renders live status: active profile, foreground app, recent
//! profile-application events. The tray runs unprivileged: it uses the
//! status pipe (`PIPE_NAME_STATUS`), whose DACL admits Authenticated Users.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
#![cfg_attr(windows, windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use eframe::egui;

use framesage_core::{AppMatch, AppRule, Policy, Profile, ProfileId};
use framesage_ipc::{Request, Response, StatusSnapshot};
#[cfg(windows)]
use tray_icon::TrayIcon;

#[cfg(windows)]
mod icons;
#[cfg(windows)]
mod win32;

mod activity_log;
mod editors;
mod formatters;
mod icon_assets;
mod ipc_client;
mod process_actions;
mod state;
mod theme;
mod tree;
mod widgets;

use editors::render_profile_editor;
use icon_assets::{build_tray, build_window_icon};
use process_actions::{render_process_detail, ProcessAction, PRIORITY_CHOICES};
use ipc_client::{
    background_loop, foreground_reporter_loop, processes_poll_loop, send_request_blocking,
};
use state::{AppState, EventKind, RecentEvent};
use widgets::{
    format_local_hms, render_activity_strip, render_active_profile_summary,
    render_foreground_summary, render_perf_band, render_profile_body, render_readonly_banner,
    render_recent_activity, render_status_bar, render_status_hero,
};

use formatters::{
    affinity_selector_label, cpu_percent_color, decode_affinity_mask, display_profile_id,
    format_bytes, format_tray_tooltip, priority_class_label, truncate_for_echo,
};
use tree::{
    build_tree_view, classify_row, column_hover_text, compare_snapshots, descendants_of,
    row_exe_color, row_marker_color, ProcessSortKey, RowState, TreeRow,
};

/// Signals raised by the tray icon's menu/click handlers, read by the egui
/// `update` loop on the next frame.
///
/// Kept as a flat struct of `Arc<AtomicBool>` / `Arc<Mutex<…>>` rather than a
/// channel because the current tray-icon thread is fire-and-forget — every
/// menu click is idempotent on the egui side, so an extra "pause when already
/// paused" flag bit is harmless and the lock-free atomics keep menu latency
/// at bare-minimum cost.
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
    /// Pause / Resume engine — set by the tray menu, drained in `update()`
    /// which dispatches the matching `Request` over the admin pipe.
    pause_engine: Arc<AtomicBool>,
    resume_engine: Arc<AtomicBool>,
    /// Panic button — force-revert any active Game Mode session.
    game_mode_off: Arc<AtomicBool>,
    /// Reveal `%ProgramData%\framesage\` in Explorer.
    open_config_folder: Arc<AtomicBool>,
    /// Open `policy.json` in the system's default text editor.
    edit_policy: Arc<AtomicBool>,
    /// Jump to a specific tab. The mutex carries the chosen `Tab`; the egui
    /// loop swaps it back to `None` after applying.
    jump_to_tab: Arc<Mutex<Option<Tab>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    /// Item 2.10 / audit H-24 partial: Status is the right landing
    /// page on first launch — hero strip, active-profile summary,
    /// foreground card, ProBalance card, quick actions. The previous
    /// default (`Processes`) dropped users into a data-dense table
    /// they didn't ask for, with no explanation of what FrameSage
    /// does. Status answers "what's happening?" at a glance; the user
    /// can click into Processes when they want to inspect.
    #[default]
    Status,
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
    /// Session-sticky "Remember as rule" toggle that lives at the top of
    /// the "Set CPU affinity" right-click submenu. When `true`, picking
    /// X3D / Non-X3D / All-cores ALSO upserts a persistent `AffinityRule`
    /// for the target exe (in addition to applying to the live PID).
    /// Defaults to `false` so the affinity submenu's behavior matches its
    /// pre-rework one-shot semantics until the user opts in.
    ///
    /// Sticky across menu opens — once you flip it on for "rule everything
    /// I touch today," subsequent picks stay persistent until you flip it
    /// back. This is the explicit trade-off the user picked over the
    /// noisier "every option splits into now / always" model.
    remember_affinity: bool,
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
            remember_affinity: false,
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
    /// When true, Apply also creates/updates a persistent affinity rule
    /// for this exe so the same mask is re-applied on every future launch.
    /// Pre-checked when the picker is opened from a process that already
    /// has a rule (so unchecking + Apply silently keeps the rule unless
    /// the user explicitly clicks "Remove rule").
    save_as_rule: bool,
    /// True iff a persistent rule already exists for this exe at the
    /// moment the picker was opened. Drives the "Remove rule" button —
    /// visible only when there's something to remove. Captured once at
    /// open so the button doesn't appear and disappear as the user toggles
    /// `save_as_rule`.
    rule_existed_at_open: bool,
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
    /// Now also a live handle: we call `set_tooltip` from `update()` to
    /// reflect the current engine state in the tray's hover-tooltip.
    #[cfg(windows)]
    tray: TrayIcon,
    /// Last tooltip we wrote — avoids a `set_tooltip` syscall on every
    /// frame when the state hasn't changed.
    #[cfg(windows)]
    last_tray_tooltip: String,
}

impl FramesageApp {
    fn new(cc: &eframe::CreationContext<'_>, commands: TrayCommands, elevated: bool) -> Self {
        // Install our custom dark theme before the first frame renders so
        // the user never sees the egui-default flash.
        theme::apply(&cc.egui_ctx);

        let state = Arc::new(Mutex::new(AppState::default()));

        // Item 2.9 — hydrate the in-memory recent-events buffer from
        // `%LOCALAPPDATA%\framesage\activity.jsonl` before the
        // background loop spins up so the Activity tab has history
        // even on first paint after a tray restart. Cap at 200 entries
        // so the in-memory buffer (max 1000) has headroom for live
        // events without immediately rolling old persisted entries
        // off the top.
        const HYDRATE_LIMIT: usize = 200;
        match activity_log::ActivityLog::open() {
            Ok(log) => match log.load_last(HYDRATE_LIMIT) {
                Ok(persisted) => {
                    let mut s = state.lock();
                    for pe in persisted {
                        let kind = EventKind::from_persist_tag(&pe.kind);
                        let at = std::time::UNIX_EPOCH
                            + std::time::Duration::from_secs(pe.at_unix_secs);
                        s.recent.push(RecentEvent {
                            at,
                            kind,
                            label: pe.label,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "activity-log load failed; starting with empty Activity tab");
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "activity-log open failed; Activity tab won't persist this run");
            }
        }

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

        // Single-instance signal watcher: a secondary tray process (launched
        // by the user clicking the .exe or Start-menu icon while this tray
        // is already running) calls `SetEvent` on a named Win32 event;
        // this thread blocks on it and flips `commands.show_window` so the
        // egui runtime restores + focuses the window on its next frame.
        // Without this, re-launching framesage-tray.exe was a silent no-op.
        #[cfg(windows)]
        if let Ok(event) = win32::create_show_window_event() {
            let watch_commands = commands.clone();
            let watch_ctx = cc.egui_ctx.clone();
            std::thread::Builder::new()
                .name("framesage-tray-show-window-watcher".into())
                .spawn(move || loop {
                    match event.wait() {
                        Ok(true) => {
                            watch_commands.show_window.store(true, Ordering::Relaxed);
                            watch_ctx.request_repaint();
                        }
                        Ok(false) => {
                            // Abandoned / unexpected — sleep a moment so a
                            // misbehaving wait doesn't busy-loop the CPU.
                            std::thread::sleep(std::time::Duration::from_millis(500));
                        }
                        Err(_) => break,
                    }
                })
                .expect("spawn show-window watcher thread");
        }

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
            #[cfg(windows)]
            last_tray_tooltip: String::new(),
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
                Ok(Response::Ok)
                | Ok(Response::Status(_))
                | Ok(Response::Processes { .. })
                | Ok(Response::UndoResult { .. })
                | Ok(Response::UndoLog { .. }) => {
                    format!("{label}: ok")
                }
                Ok(Response::Error { message }) => format!("{label}: error — {message}"),
                Err(e) => format!("{label}: error — {e}"),
            };
            *last_action.lock() = Some(msg);
        });
    }

    #[cfg(not(windows))]
    fn send_admin_request(&self, _req: Request, _label: &'static str) {
        // No-op on non-Windows so this stub still compiles in cross-checks.
        *self.last_action.lock() = Some("admin requests are Windows-only".to_string());
    }

    /// Look up a persistent affinity rule by exe name from the last service
    /// status snapshot. Returns `None` if no rule matches or the snapshot
    /// hasn't arrived yet. Locks the state mutex briefly to clone the
    /// matched rule — cheap; rules are tiny.
    fn policy_snapshot_lookup_rule(&self, exe_name: &str) -> Option<framesage_core::AffinityRule> {
        let s = self.state.lock();
        s.status
            .as_ref()?
            .policy
            .affinity_rule_for(exe_name)
            .cloned()
    }
}

/// Resolve a `CpuSelector` into a raw u64 affinity bitmap suitable for the
/// picker's working state. Mirrors the engine-side resolution but uses the
/// known CPU count instead of live topology (the tray doesn't carry a
/// CpuTopology); for `Kind(Cache)` / `Kind(Performance)` we fall back to
/// the lower-half / upper-half heuristic that the picker already uses for
/// its highlight column.
///
/// Returns `!0u64` masked to `cpu_count` bits for `All`, an empty mask for
/// resolutions that produced no CPUs (so the picker treats them as "no
/// preset" and stays at whatever the user had).
fn selector_to_mask(selector: &framesage_core::CpuSelector, cpu_count: usize) -> u64 {
    let cpu_count = if cpu_count == 0 { 32 } else { cpu_count };
    let all_mask: u64 = if cpu_count >= 64 {
        u64::MAX
    } else {
        (1u64 << cpu_count) - 1
    };
    match selector {
        framesage_core::CpuSelector::All => all_mask,
        framesage_core::CpuSelector::Mask(m) => *m & all_mask,
        framesage_core::CpuSelector::Kind(framesage_core::CoreKind::Cache) => {
            let hi = cpu_count / 2;
            let mut m = 0u64;
            for i in 0..hi {
                if i < 64 {
                    m |= 1u64 << i;
                }
            }
            m
        }
        framesage_core::CpuSelector::Kind(framesage_core::CoreKind::Performance) => {
            let lo = cpu_count / 2;
            let mut m = 0u64;
            for i in lo..cpu_count {
                if i < 64 {
                    m |= 1u64 << i;
                }
            }
            m
        }
        framesage_core::CpuSelector::Kind(_) => all_mask,
        framesage_core::CpuSelector::Ccd(_)
        | framesage_core::CpuSelector::CcdNot(_)
        | framesage_core::CpuSelector::TopRanked(_) => all_mask,
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
            // Three commands cover every "the window is gone" state:
            //   * Visible(true)   — restores from hide-to-tray
            //   * Minimized(false) — restores from taskbar-minimize (this
            //     was missing; Focus alone won't unminimize a minimized
            //     window on Windows, so a tray click did nothing visible
            //     when the user had hit the "─" button)
            //   * Focus           — brings it forward + activates input
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        if self.commands.hide_window.swap(false, Ordering::Relaxed) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.commands.window_visible.store(false, Ordering::Relaxed);
        }
        // Engine controls dispatched from the tray menu. Each is a one-shot
        // admin-pipe round-trip; we use the existing `send_admin_request`
        // helper so the user-facing echo lands in the status bar.
        if self.commands.pause_engine.swap(false, Ordering::Relaxed) {
            self.send_admin_request(Request::Pause, "pause");
        }
        if self.commands.resume_engine.swap(false, Ordering::Relaxed) {
            self.send_admin_request(Request::Resume, "resume");
        }
        if self.commands.game_mode_off.swap(false, Ordering::Relaxed) {
            self.send_admin_request(Request::GameModeOff, "game-mode off");
        }
        if self
            .commands
            .open_config_folder
            .swap(false, Ordering::Relaxed)
        {
            open_in_shell(&framesage_core::paths::config_dir().to_string_lossy());
        }
        if self.commands.edit_policy.swap(false, Ordering::Relaxed) {
            open_in_shell(&framesage_core::paths::policy_path().to_string_lossy());
        }
        // View → Tab. Pre-set the tab BEFORE the window becomes visible so
        // the first frame painted after show paints the right tab.
        if let Some(target) = self.commands.jump_to_tab.lock().take() {
            self.tab = target;
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
            let s = self.state.lock();
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

        // Refresh the tray-icon tooltip when the engine state changes. The
        // tooltip reads on hover, so a stale string would mislead the user
        // about whether the engine is paused / what profile is active.
        // `set_tooltip` is a Win32 round-trip; we gate it on a string-equality
        // check so we only call it when the formatted text actually differs.
        #[cfg(windows)]
        {
            let new_tooltip = format_tray_tooltip(connected, status_snapshot.as_ref());
            if new_tooltip != self.last_tray_tooltip {
                let _ = self.tray.set_tooltip(Some(&new_tooltip));
                self.last_tray_tooltip = new_tooltip;
            }
        }

        // Hand the latest processes snapshot into the local view buffer. We
        // do this under a separate short lock window so the render path can
        // iterate the rows without holding the mutex (the table walks a
        // virtualized list and we don't want a long borrow blocking the
        // poller thread on its 1 Hz refresh).
        {
            let s = self.state.lock();
            if !s.processes.is_empty() || self.processes.rows.is_empty() {
                self.processes.rows = s.processes.clone();
            }
        }

        // Pull metrics + activity for the always-visible top/bottom strips.
        let (system_metrics, system_history, recent_for_strip) = {
            let s = self.state.lock();
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
        let last_action_text = self.last_action.lock().clone();

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
        let rule_existed_at_open = picker.rule_existed_at_open;

        let mut do_apply = false;
        let mut do_cancel = false;
        let mut do_remove_rule = false;
        let mut new_mask = picker.mask;
        let mut save_as_rule = picker.save_as_rule;

        // CPU count: prefer per_core_cpu_percent length (live), fall back
        // to 32 as a sane default.
        let cpu_count = {
            let s = self.state.lock();
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
                ui.separator();
                ui.add_space(6.0);

                // ── Save as rule ────────────────────────────────────
                // Persists the picked mask as an AffinityRule keyed by
                // the exe name so the same pin is re-applied on every
                // future launch. Pre-checked when the picker was opened
                // against a process whose exe already had a rule
                // (editing existing) — unchecking + Apply doesn't delete
                // the existing rule (use the explicit Remove button for
                // that, so deletion is never accidental).
                let save_label = if save_as_rule {
                    egui::RichText::new(format!("✓ Save as rule for {exe_name}"))
                        .color(theme::ACCENT)
                        .strong()
                } else {
                    egui::RichText::new(format!("Save as rule for {exe_name}"))
                };
                ui.checkbox(&mut save_as_rule, save_label).on_hover_text(
                    "When checked, Apply also writes this mask as a \
                     persistent rule. The same mask is re-applied \
                     automatically every time a process with this exe \
                     name launches, and the engine re-asserts it every \
                     ~2 s to defeat games that override their own \
                     affinity at startup.",
                );

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

                    // Remove rule — only visible when a rule existed at
                    // the time the picker was opened. Decoupled from
                    // `save_as_rule` so the user can clearly see "yes
                    // there's a rule, click here to delete it" as a
                    // distinct action.
                    if rule_existed_at_open {
                        ui.add_space(20.0);
                        if ui
                            .button(egui::RichText::new("Remove rule").color(theme::ERROR))
                            .on_hover_text(
                                "Delete the persistent affinity rule for \
                                 this exe. The live process keeps its \
                                 current mask — clearing the rule only \
                                 stops future launches from being pinned.",
                            )
                            .clicked()
                        {
                            do_remove_rule = true;
                        }
                    }
                });
            });

        // Commit the working mask + checkbox state back to picker state so
        // they persist across re-renders while the modal is open.
        if let Some(p) = self.affinity_picker.as_mut() {
            p.mask = new_mask;
            p.save_as_rule = save_as_rule;
        }

        if do_apply {
            if save_as_rule {
                // Single round-trip when persisting: SetAffinityRule with
                // apply_to_live=true both writes the rule and pins every
                // matching live PID via the engine's walk — which also
                // marks each pinned PID in `affinity_rule_applied` so the
                // 2 s re-assert sweep immediately keeps it sticky. Doing
                // a separate SetProcessAffinity first would leave the PID
                // unmarked (because set_affinity_rule clears the marker
                // set when policy mutates), opening a ~10 s window where
                // a game could overwrite our pin before the background
                // scan re-marked it.
                self.send_admin_request(
                    Request::SetAffinityRule {
                        rule: framesage_core::AffinityRule {
                            exe_name: exe_name.clone(),
                            selector: framesage_core::CpuSelector::Mask(new_mask),
                            note: String::new(),
                        },
                        apply_to_live: true,
                    },
                    "save affinity rule",
                );
            } else {
                self.send_admin_request(
                    Request::SetProcessAffinity {
                        pid,
                        selector: framesage_core::CpuSelector::Mask(new_mask),
                    },
                    "set affinity",
                );
            }
            self.affinity_picker = None;
        } else if do_remove_rule {
            // Explicit Remove rule click — never inferred. Doesn't touch
            // the live process; pin sticks until exit. Matches the UX
            // promise in the button's hover text.
            self.send_admin_request(
                Request::DeleteAffinityRule {
                    exe_name: exe_name.clone(),
                },
                "delete affinity rule",
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
            // Each tab gets a one-line hover-text that names the tab's
            // job, since the labels themselves are deliberately terse.
            let tabs: [(Tab, &str, &str); 5] = [
                (
                    Tab::Processes,
                    "Processes",
                    "Live process viewer — sortable table, tree view, right-click for per-PID actions.",
                ),
                (
                    Tab::Status,
                    "Status",
                    "Engine state, active profile, foreground app, ProBalance state.",
                ),
                (
                    Tab::Activity,
                    "Activity",
                    "Full event log — filter chips + search.",
                ),
                (
                    Tab::Rules,
                    "Rules",
                    "Match rules: when a foreground exe matches, apply the named profile.",
                ),
                (
                    Tab::Profiles,
                    "Profiles",
                    "Per-profile editor — CPU sets, throttling, priority, Game Mode actions.",
                ),
            ];
            for (t, label, hover) in tabs {
                if theme::tab_button(ui, label, self.tab == t)
                    .on_hover_text(hover)
                    .clicked()
                {
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

        // ─── Manual Global Game Mode banner (item 2.11) ─────────────────
        if let Some(global_id) = &s.manual_global_active {
            theme::banner(theme::WARNING).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(theme::WARNING, egui::RichText::new("!").strong().size(13.0));
                    ui.label(
                        egui::RichText::new("Manual Global Game Mode")
                            .strong()
                            .color(theme::TEXT),
                    );
                    ui.colored_label(theme::TEXT_MUTED, "·");
                    ui.label(
                        egui::RichText::new(display_profile_id(&global_id.0))
                            .strong()
                            .color(theme::WARNING),
                    );
                    ui.colored_label(
                        theme::TEXT_MUTED,
                        "is applied system-wide (auto reconcile paused)",
                    );
                    if self.elevated {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Exit Manual Game Mode").clicked() {
                                self.send_admin_request(
                                    Request::DisableManualGlobalGameMode,
                                    "disable manual global game mode",
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
            self.render_quick_actions(ctx, ui, paused, in_game_mode, s);
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
        status: &StatusSnapshot,
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
                                    *self.last_action.lock() =
                                        Some(format!("relaunch failed: {e}"));
                                }
                            }
                        }
                    });
                });
            });
            if let Some(msg) = self.last_action.lock().as_ref() {
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

            // ─── Manual Global Game Mode launcher (item 2.11) ───────────
            // Lists every profile marked `manual_global_eligible` so
            // the user can enter a system-wide quiet-desktop session
            // independent of foreground. When manual global is
            // already active, this section collapses to a single
            // "Exit Manual Game Mode" button so the user has a fast
            // off-switch without scrolling up to the banner.
            let eligible: Vec<&framesage_core::Profile> = status
                .policy
                .profiles
                .values()
                .filter(|p| p.manual_global_eligible)
                .collect();
            if !eligible.is_empty() {
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(theme::section_heading("Manual Global Game Mode"));
                ui.add_space(4.0);
                if let Some(active) = &status.manual_global_active {
                    ui.horizontal(|ui| {
                        ui.colored_label(theme::TEXT_MUTED, "Active:");
                        ui.colored_label(theme::WARNING, display_profile_id(&active.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Exit Manual Game Mode").clicked() {
                                self.send_admin_request(
                                    Request::DisableManualGlobalGameMode,
                                    "disable manual global game mode",
                                );
                            }
                        });
                    });
                } else {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(
                            theme::TEXT_MUTED,
                            "Enter a profile's environment actions system-wide:",
                        );
                    });
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for profile in &eligible {
                            let id = profile.id.clone();
                            let label = format!("Enter {}", display_profile_id(&id.0));
                            if ui.button(label).clicked() {
                                self.send_admin_request(
                                    Request::EnableManualGlobalGameMode { profile: id },
                                    "enable manual global game mode",
                                );
                            }
                        }
                    });
                }
            }

            if let Some(msg) = self.last_action.lock().as_ref() {
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
            let s = self.state.lock();
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
                    self.state.lock().recent.clear();
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

        if let Some(msg) = self.last_action.lock().as_ref() {
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

        // ─── Affinity Rules section ────────────────────────────────────────
        // Lightweight per-exe CPU-affinity rules, managed independently of
        // the heavier Profile + AppRule pair above. Lives in the Rules tab
        // because it's conceptually the same idea — "when this exe runs,
        // do X" — and grouping both rule kinds in one place beats hunting
        // for a separate Affinity Rules tab. View / delete here; creation
        // happens from the Processes-tab right-click "Remember as rule"
        // toggle, where the user can pick the exact mask in context.
        ui.add_space(16.0);
        ui.separator();
        ui.add_space(8.0);
        self.render_affinity_rules_section(ui, &s.policy);
    }

    /// Affinity-rules sub-section of the Rules tab. Read + delete UX —
    /// rule creation happens in context from the Processes tab so the
    /// user picks against the live process they actually care about.
    /// Shows an empty-state CTA when the rule list is empty, otherwise a
    /// compact table of exe → mask → note + a Remove button per row.
    fn render_affinity_rules_section(&mut self, ui: &mut egui::Ui, policy: &Policy) {
        use egui_extras::{Column, TableBuilder};

        ui.heading("Persistent CPU-Affinity Rules");
        ui.label(
            egui::RichText::new(
                "Each rule pins a CPU mask onto every process whose exe name matches. \
                 The engine re-applies them on every spawn and re-asserts every ~2 s \
                 to defeat games that override their own affinity at startup.",
            )
            .color(theme::TEXT_MUTED),
        );
        ui.add_space(6.0);

        if policy.affinity_rules.is_empty() {
            ui.colored_label(
                theme::TEXT_MUTED,
                "No affinity rules yet. To create one, open the Processes tab, \
                 right-click a process, tick 'Remember as rule' in the 'Set CPU \
                 affinity' submenu, then pick a target (X3D CCD, Non-X3D, or Custom…).",
            );
            return;
        }

        // Sort by exe name so the list is stable across renders. Cloning
        // the rule vec is cheap — handfuls of entries in practice.
        let mut rules: Vec<framesage_core::AffinityRule> = policy.affinity_rules.clone();
        rules.sort_by_key(|r| r.exe_name.to_ascii_lowercase());

        let mut remove_exe: Option<String> = None;

        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(220.0).at_least(140.0)) // Exe
            .column(Column::initial(200.0).at_least(120.0)) // Selector
            .column(Column::remainder().at_least(120.0)) // Note
            .column(Column::exact(90.0)) // Remove
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.strong("Exe");
                });
                header.col(|ui| {
                    ui.strong("Pin");
                });
                header.col(|ui| {
                    ui.strong("Note");
                });
                header.col(|ui| {
                    ui.strong("");
                });
            })
            .body(|mut body| {
                for rule in &rules {
                    body.row(20.0, |mut row| {
                        row.col(|ui| {
                            ui.monospace(&rule.exe_name);
                        });
                        row.col(|ui| {
                            ui.label(affinity_selector_label(&rule.selector));
                        });
                        row.col(|ui| {
                            if rule.note.is_empty() {
                                ui.colored_label(theme::TEXT_MUTED, "—");
                            } else {
                                ui.label(&rule.note);
                            }
                        });
                        row.col(|ui| {
                            let remove_enabled = self.elevated;
                            if ui
                                .add_enabled(
                                    remove_enabled,
                                    egui::Button::new(
                                        egui::RichText::new("Remove").color(theme::ERROR),
                                    ),
                                )
                                .on_hover_text(
                                    "Delete this affinity rule. Currently-running \
                                     matching processes keep their pin until they exit.",
                                )
                                .clicked()
                            {
                                remove_exe = Some(rule.exe_name.clone());
                            }
                        });
                    });
                }
            });

        if let Some(exe) = remove_exe {
            self.send_admin_request(
                Request::DeleteAffinityRule { exe_name: exe },
                "delete affinity rule",
            );
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
        if let Some(msg) = self.last_action.lock().as_ref() {
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
                ui.colored_label(theme::TEXT_MUTED, format!("{count} processes"))
                    .on_hover_text(
                        "Total live processes the engine can see, including those whose \
                         exe path it can't open (protected processes still get a row).",
                    );
                ui.separator();
                ui.colored_label(theme::TEXT_MUTED, format!("{total_threads} threads"))
                    .on_hover_text("Sum of OS thread counts across every visible process.");
                ui.separator();
                ui.colored_label(
                    theme::TEXT_MUTED,
                    format!("{} mem", format_bytes(total_mem)),
                )
                .on_hover_text("Sum of working-set bytes across every visible process.");
                ui.separator();
                ui.colored_label(theme::TEXT_MUTED, format!("Total CPU {total_cpu_one_cpu}%"))
                    .on_hover_text(
                        "Sum of per-process CPU% in 'percent of one logical CPU' units. \
                         A 16-thread box maxes at 1600%.",
                    );
                if managed > 0 {
                    ui.separator();
                    ui.colored_label(theme::ACCENT, format!("{managed} managed"))
                        .on_hover_text(
                            "Processes that have an active FrameSage profile applied — \
                             matched a rule, manual override, or one-shot ApplyOnce.",
                        );
                }
                if restrained > 0 {
                    ui.separator();
                    ui.colored_label(theme::WARNING, format!("{restrained} restrained"))
                        .on_hover_text(
                            "Processes that ProBalance has temporarily demoted because \
                             they're hogging CPU under contention.",
                        );
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
        // Lowercased exe names that have a persistent affinity rule. Computed
        // once per render from the latest status snapshot so the per-row
        // pin-marker lookup is an O(1) set hit instead of a linear scan of
        // policy.affinity_rules for every row. Empty set when no snapshot
        // has arrived yet or the policy has no rules.
        let rule_exists_for_exe: std::collections::HashSet<String> = status
            .as_ref()
            .map(|s| {
                s.policy
                    .affinity_rules
                    .iter()
                    .map(|r| r.exe_name.to_ascii_lowercase())
                    .collect()
            })
            .unwrap_or_default();
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
                                            // ── Remember toggle ─────────────────
                                            // Session-sticky checkbox at the top of
                                            // the submenu. When on, every action
                                            // below also writes a persistent rule
                                            // keyed by the targeted exe. Kept
                                            // visually prominent (colored when on)
                                            // so the user notices it's still
                                            // armed on next open.
                                            let label = if self.processes.remember_affinity {
                                                egui::RichText::new("✓ Remember as rule")
                                                    .color(theme::ACCENT)
                                                    .strong()
                                            } else {
                                                egui::RichText::new("Remember as rule")
                                            };
                                            ui.checkbox(
                                                &mut self.processes.remember_affinity,
                                                label,
                                            )
                                            .on_hover_text(
                                                "When checked, the affinity you pick \
                                                 here also saves as a persistent rule \
                                                 keyed by exe name — the same mask is \
                                                 re-applied automatically on every \
                                                 future launch. 'All cores (reset)' \
                                                 also clears any existing rule. Stays \
                                                 on until you uncheck it.",
                                            );
                                            ui.separator();

                                            let remember = self.processes.remember_affinity;
                                            let mut affinity_dispatch =
                                                |sel: framesage_core::CpuSelector,
                                                 close: &mut bool| {
                                                    for (t_pid, t_exe) in &targets {
                                                        action_queue.push(
                                                            ProcessAction::SetAffinity {
                                                                pid: *t_pid,
                                                                selector: sel.clone(),
                                                                save_as_rule_for: if remember
                                                                {
                                                                    Some(t_exe.clone())
                                                                } else {
                                                                    None
                                                                },
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

                                        // ─── Shell + Copy actions ────────────
                                        // Show in Explorer / Copy submenu — the
                                        // standard "where does this thing live
                                        // and how do I tell someone about it"
                                        // affordances every Windows process
                                        // viewer ships. Only meaningful for the
                                        // single-row case; for bulk select they
                                        // lose meaning.
                                        if !bulk {
                                            ui.separator();
                                            let show_enabled = !p.exe_path.is_empty();
                                            if ui
                                                .add_enabled(
                                                    show_enabled,
                                                    egui::Button::new("Show in Explorer"),
                                                )
                                                .on_hover_text(
                                                    "Open the folder containing this exe \
                                                     in Explorer with the file selected.",
                                                )
                                                .on_disabled_hover_text(
                                                    "Engine couldn't resolve the exe path \
                                                     (protected process or already exited).",
                                                )
                                                .clicked()
                                            {
                                                action_queue.push(ProcessAction::ShowInExplorer {
                                                    path: p.exe_path.clone(),
                                                });
                                                ui.close_menu();
                                            }
                                            ui.menu_button("Copy", |ui| {
                                                if ui.button(format!("PID  ({pid})")).clicked() {
                                                    action_queue.push(
                                                        ProcessAction::CopyToClipboard {
                                                            text: pid.to_string(),
                                                        },
                                                    );
                                                    ui.close_menu();
                                                }
                                                if ui.button(format!("Exe name  ({exe})")).clicked()
                                                {
                                                    action_queue.push(
                                                        ProcessAction::CopyToClipboard {
                                                            text: exe.clone(),
                                                        },
                                                    );
                                                    ui.close_menu();
                                                }
                                                if !p.exe_path.is_empty()
                                                    && ui.button("Full path").clicked()
                                                {
                                                    action_queue.push(
                                                        ProcessAction::CopyToClipboard {
                                                            text: p.exe_path.clone(),
                                                        },
                                                    );
                                                    ui.close_menu();
                                                }
                                            });
                                        }

                                        // ─── Tree-aware bulk: Suspend tree ───
                                        // Only meaningful when the parent is
                                        // actually showing children (tr.has_children
                                        // implies "this row is a parent in the
                                        // current snapshot"). Single-PID only —
                                        // for ctrl-multi-select the existing
                                        // bulk Suspend handles the same use case.
                                        if !bulk && tr.has_children {
                                            ui.separator();
                                            if ui
                                                .button("Suspend tree (this + children)")
                                                .on_hover_text(
                                                    "Suspend this process plus every \
                                                     descendant reachable via parent-PID. \
                                                     Same primitive as plain Suspend, just \
                                                     applied to the whole subtree.",
                                                )
                                                .clicked()
                                            {
                                                action_queue.push(ProcessAction::SuspendTree {
                                                    root_pid: pid,
                                                });
                                                ui.close_menu();
                                            }
                                        }

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
                                // Affinity cell is now an interactive badge:
                                // clicking it opens the picker for this PID
                                // (one click vs right-click → submenu →
                                // Custom). A leading 📌 marker indicates a
                                // persistent rule exists for the exe — same
                                // signal Process Lasso uses in its CPU
                                // Affinity column. Right-click brings the
                                // delete option.
                                let has_rule =
                                    rule_exists_for_exe.contains(&p.exe_name.to_ascii_lowercase());
                                let mask_text = format!("{:#x}", p.affinity_mask);
                                let cell_text = if has_rule {
                                    egui::RichText::new(format!("📌 {mask_text}"))
                                        .color(theme::ACCENT)
                                        .monospace()
                                } else {
                                    egui::RichText::new(mask_text).monospace()
                                };
                                let resp =
                                    ui.add(egui::Label::new(cell_text).sense(egui::Sense::click()));
                                let mut decoded = decode_affinity_mask(p.affinity_mask);
                                if has_rule {
                                    decoded.push_str(
                                        "\n\n📌 Persistent affinity rule active \
                                         for this exe. Click to edit, right-click \
                                         to remove.",
                                    );
                                } else {
                                    decoded.push_str(
                                        "\n\nClick to edit affinity. Toggle the \
                                         Save-as-rule checkbox in the picker to \
                                         make it persistent across launches.",
                                    );
                                }
                                let resp = resp.on_hover_text(decoded);
                                if resp.clicked() {
                                    action_queue.push(ProcessAction::RequestAffinityPicker {
                                        pid: p.pid,
                                        exe_name: p.exe_name.clone(),
                                    });
                                }
                                resp.context_menu(|ui| {
                                    if ui.button("Edit affinity / rule…").clicked() {
                                        action_queue.push(ProcessAction::RequestAffinityPicker {
                                            pid: p.pid,
                                            exe_name: p.exe_name.clone(),
                                        });
                                        ui.close_menu();
                                    }
                                    if has_rule
                                        && ui
                                            .button(
                                                egui::RichText::new("Remove persistent rule")
                                                    .color(theme::ERROR),
                                            )
                                            .clicked()
                                    {
                                        action_queue.push(ProcessAction::DeleteAffinityRule {
                                            exe_name: p.exe_name.clone(),
                                        });
                                        ui.close_menu();
                                    }
                                });
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
                                *self.last_action.lock() =
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
                ProcessAction::SetAffinity {
                    pid,
                    selector,
                    save_as_rule_for,
                } => {
                    match save_as_rule_for {
                        None => {
                            // One-shot pin: no rule, just the live PID.
                            self.send_admin_request(
                                Request::SetProcessAffinity { pid, selector },
                                "set affinity",
                            );
                        }
                        Some(exe) if matches!(selector, framesage_core::CpuSelector::All) => {
                            // "All cores" with Remember = "clear the rule
                            // and reset the live pin." The rule deletion
                            // is the persistent intent; the live pin reset
                            // makes the change visible immediately on the
                            // targeted PID.
                            self.send_admin_request(
                                Request::DeleteAffinityRule {
                                    exe_name: exe.clone(),
                                },
                                "delete affinity rule (reset)",
                            );
                            self.send_admin_request(
                                Request::SetProcessAffinity { pid, selector },
                                "reset affinity",
                            );
                        }
                        Some(exe) => {
                            // Persistent intent: one IPC does both the
                            // rule write AND the live-PID pin (via the
                            // engine's apply_to_live walk by exe name).
                            // Critically, the engine marks each pinned
                            // PID in `affinity_rule_applied` during that
                            // walk, so the 2 s re-assert sweep immediately
                            // keeps the pin sticky against game-overrides
                            // — Process Lasso parity for the "pin holds
                            // under load" behavior.
                            self.send_admin_request(
                                Request::SetAffinityRule {
                                    rule: framesage_core::AffinityRule {
                                        exe_name: exe,
                                        selector,
                                        note: String::new(),
                                    },
                                    apply_to_live: true,
                                },
                                "save affinity rule",
                            );
                        }
                    }
                }
                ProcessAction::DeleteAffinityRule { exe_name } => {
                    self.send_admin_request(
                        Request::DeleteAffinityRule { exe_name },
                        "delete affinity rule",
                    );
                }
                ProcessAction::RequestAffinityPicker { pid, exe_name } => {
                    // Pre-populate the picker with the most useful starting
                    // mask. Order: persistent rule > current live mask > all
                    // cores. Editing a rule should show the rule's mask, not
                    // whatever the process happens to be pinned to right now
                    // (which might be the rule's mask or might be drift from
                    // an external change since rule apply).
                    let topology_cpu_count =
                        self.state.lock().system.per_core_cpu_percent.len();
                    let existing_rule_mask = self
                        .policy_snapshot_lookup_rule(&exe_name)
                        .map(|rule| selector_to_mask(&rule.selector, topology_cpu_count));
                    let live_mask = self
                        .processes
                        .rows
                        .iter()
                        .find(|p| p.pid == pid)
                        .map(|p| p.affinity_mask);
                    let initial_mask = existing_rule_mask.or(live_mask).unwrap_or(!0u64);
                    let rule_existed = existing_rule_mask.is_some();
                    self.affinity_picker = Some(AffinityPicker {
                        pid,
                        exe_name,
                        mask: initial_mask,
                        save_as_rule: rule_existed,
                        rule_existed_at_open: rule_existed,
                    });
                }
                ProcessAction::ShowInExplorer { path } => {
                    open_explorer_select(&path);
                    *self.last_action.lock() = Some(format!("show in explorer: {path}"));
                }
                ProcessAction::CopyToClipboard { text } => {
                    ui.ctx().copy_text(text.clone());
                    *self.last_action.lock() =
                        Some(format!("copied: {}", truncate_for_echo(&text, 40)));
                }
                ProcessAction::SuspendTree { root_pid } => {
                    // Expand to root + descendants against the LIVE snapshot.
                    // Doing this here (not at click time) means the user
                    // suspends the actual subtree as of dispatch, not as of
                    // whichever frame they clicked.
                    let descendants = descendants_of(&self.processes.rows, root_pid);
                    for pid in descendants {
                        self.send_admin_request(
                            Request::SuspendProcess { pid },
                            "suspend tree member",
                        );
                    }
                }
            }
        }
    }

    /// Header cell that toggles the sort key on click. Shows ▲ / ▼ on the
    /// active column. Hover-text comes from `column_hover_text(key)` so
    /// every column gets a consistent "what does this mean" tooltip.
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
        let resp = ui
            .add(egui::Label::new(format!("{label}{suffix}")).sense(egui::Sense::click()))
            .on_hover_text(column_hover_text(key));
        if resp.clicked() {
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


// ─── main ────────────────────────────────────────────────────────────────────

fn main() -> eframe::Result<()> {
    // Singleton + elevation handoff: if another tray is running, wait
    // briefly for it to exit (covers the elevation-handoff window). If it
    // doesn't, signal the existing instance to bring its window forward
    // and exit cleanly — the user clicked the .exe / Start-menu icon
    // expecting "show the app," not "fail silently."
    #[cfg(windows)]
    let _singleton = match win32::acquire_singleton() {
        Ok(win32::SingletonAttempt::Primary(guard)) => guard,
        Ok(win32::SingletonAttempt::AlreadyRunning) => {
            // Best-effort signal — we don't care if it succeeded; either
            // the primary woke up and showed its window, or we hit the
            // tiny race window where the primary is starting up and the
            // event isn't created yet, in which case the primary will
            // already show its window naturally.
            let _ = win32::signal_existing_tray_show_window();
            return Ok(());
        }
        Err(e) => {
            eprintln!("framesage-tray: singleton check failed: {e}");
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

/// Open a file, folder, or URL in the OS shell handler. Best-effort: we
/// silently drop spawn errors because there's no useful recovery — the user
/// can always navigate manually. The `cmd /c start "" <target>` form is the
/// reliable cross-input way to do this on Windows (handles paths with
/// spaces and URLs identically). On non-Windows hosts this is a no-op so
/// the rest of the binary still cross-compiles.
/// Open Explorer with `path`'s containing folder open and `path` selected
/// in the list. Uses the documented `explorer.exe /select,<path>` switch —
/// same idiom Task Manager's "Open file location" uses. Best-effort: no
/// useful recovery if Explorer can't launch (the user always has manual
/// navigation).
#[cfg(windows)]
fn open_explorer_select(path: &str) {
    let _ = std::process::Command::new("explorer.exe")
        .arg(format!("/select,{path}"))
        .spawn();
}
#[cfg(not(windows))]
fn open_explorer_select(_path: &str) {}

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
