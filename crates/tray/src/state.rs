//! Shared state types lifted out of main.rs for the 3.6 tray module
//! extractions. Keeping them in their own file lets `ipc_client`
//! mutate the buffer without a circular dep on the main module.
//!
//! Everything here is `pub(crate)` (or stricter) because the tray is a
//! single-binary crate — no need for outward `pub`.
//!
//! What lives here:
//!
//! * `AppState` — the shared mutex-guarded state the IPC client + UI
//!   threads both touch.
//! * `RecentEvent` — one row in the activity-events ring buffer.
//! * `EventKind` — coarse category for filter chips + persist-tag
//!   round-tripping with `activity_log::PersistedActivityEvent`.
//! * `SYSTEM_HISTORY_LEN` — sample count for the perf-band sparkline.

use std::collections::{HashMap, VecDeque};
use std::time::SystemTime;

use eframe::egui;
use framesage_ipc::{ProcessSnapshot, StatusSnapshot, SystemMetrics};

use crate::theme;

/// Shared, mutex-guarded application state. Owned by `FramesageApp` and
/// passed via `Arc<Mutex<AppState>>` to the background threads
/// (`ipc_client::background_loop`, `ipc_client::processes_poll_loop`).
#[derive(Default)]
pub(crate) struct AppState {
    pub(crate) connected: bool,
    pub(crate) last_error: Option<String>,
    pub(crate) status: Option<StatusSnapshot>,
    pub(crate) recent: Vec<RecentEvent>,
    /// Latest snapshot of all processes from the service. Refreshed by
    /// `processes_poll_loop` at ~1 Hz. Empty until the first poll completes.
    pub(crate) processes: Vec<ProcessSnapshot>,
    /// Live system-wide metrics paired with the latest `processes` snapshot
    /// (CPU% / mem used / mem total). Refreshed each poll.
    pub(crate) system: SystemMetrics,
    /// Sliding ring buffer of the last `SYSTEM_HISTORY_LEN` (CPU%, mem%)
    /// samples — backs the sparkline in the permanent performance band at
    /// the top of every tab. Newest at the back.
    pub(crate) system_history: VecDeque<(u8, u8)>,
    /// Item 3.4 — per-PID CPU% history. Each VecDeque caps at
    /// `SYSTEM_HISTORY_LEN` samples (newest at the back), refreshed
    /// each `processes_poll_loop` tick. Backs the sparkline shown in
    /// the Processes-tab detail panel. PIDs that disappear between
    /// snapshots are evicted to keep the map bounded.
    pub(crate) per_pid_cpu_history: HashMap<u32, VecDeque<u8>>,
}

/// Number of samples kept in `AppState.system_history` and each
/// per-PID `per_pid_cpu_history` entry. 60 samples × 1 Hz poll = 60
/// seconds of history, which matches Task Manager / PL's default
/// graph window. Cheap (120 bytes for the system pair-history; ~120
/// bytes per managed PID for the per-PID variant).
pub(crate) const SYSTEM_HISTORY_LEN: usize = 60;

pub(crate) struct RecentEvent {
    /// Wall-clock time the event was received. Rendered as `HH:MM:SS` in
    /// the Activity Log; the strip + Status-tab recent activity ignore it.
    pub(crate) at: SystemTime,
    /// Coarse category for filter chips + color-coding. `Other` is the
    /// catch-all so a new IPC event variant doesn't get silently lost.
    pub(crate) kind: EventKind,
    pub(crate) label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum EventKind {
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
    pub(crate) fn display(self) -> &'static str {
        match self {
            EventKind::Foreground => "Foreground",
            EventKind::Engine => "Engine",
            EventKind::ProBalanceRestrained => "ProBalance demote",
            EventKind::ProBalanceRestored => "ProBalance restore",
            EventKind::Other => "Other",
        }
    }

    /// Snake-case discriminant string used as the on-disk
    /// `PersistedActivityEvent::kind`. Distinct from `display()` (which
    /// is the human-friendly UI label) so the wire format stays stable
    /// even if we rename the UI label.
    pub(crate) fn persist_tag(self) -> &'static str {
        match self {
            EventKind::Foreground => "foreground",
            EventKind::Engine => "engine",
            EventKind::ProBalanceRestrained => "probalance_restrained",
            EventKind::ProBalanceRestored => "probalance_restored",
            EventKind::Other => "other",
        }
    }

    /// Inverse of `persist_tag` — unknown strings (e.g. from a future
    /// schema variant the running binary doesn't know about) map to
    /// `Other` rather than dropping the event.
    pub(crate) fn from_persist_tag(tag: &str) -> Self {
        match tag {
            "foreground" => EventKind::Foreground,
            "engine" => EventKind::Engine,
            "probalance_restrained" => EventKind::ProBalanceRestrained,
            "probalance_restored" => EventKind::ProBalanceRestored,
            _ => EventKind::Other,
        }
    }

    pub(crate) fn color(self) -> egui::Color32 {
        match self {
            EventKind::Foreground => theme::ACCENT,
            EventKind::Engine => theme::TEXT_MUTED,
            EventKind::ProBalanceRestrained => theme::WARNING,
            EventKind::ProBalanceRestored => theme::SUCCESS,
            EventKind::Other => theme::TEXT,
        }
    }
}
