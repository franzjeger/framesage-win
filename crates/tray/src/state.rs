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

/// Item 4.15 — aggregated counts of significant events over the
/// trailing 24 hours. Backs the "Session stats" card on the Status
/// tab so the user has a single-glance view of what FrameSage has
/// done lately ("36 profiles applied, 4 ProBalance demotions, 1
/// Game Mode session"). 24-hour sliding window rather than
/// calendar-day to keep the math timezone-free and to stay useful
/// across midnight rollover.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionStats {
    pub(crate) profiles_applied: u32,
    pub(crate) probalance_demotions: u32,
    pub(crate) probalance_restores: u32,
    pub(crate) game_mode_sessions: u32,
}

impl SessionStats {
    /// Walk a slice of `RecentEvent`s and tally counts of events
    /// observed within the last 24 hours of `now`. The activity log
    /// is hydrated into `AppState.recent` at startup so this works
    /// off-engine without disk I/O on every Status-tab render.
    pub(crate) fn from_recent(recent: &[RecentEvent], now: SystemTime) -> Self {
        const WINDOW_SECS: u64 = 24 * 60 * 60;
        let cutoff = now
            .checked_sub(std::time::Duration::from_secs(WINDOW_SECS))
            .unwrap_or(now);
        let mut out = SessionStats::default();
        for ev in recent {
            if ev.at < cutoff {
                continue;
            }
            match ev.kind {
                EventKind::Foreground => out.profiles_applied += 1,
                EventKind::ProBalanceRestrained => out.probalance_demotions += 1,
                EventKind::ProBalanceRestored => out.probalance_restores += 1,
                EventKind::Engine | EventKind::Other => {
                    // Game-mode-entered events surface under
                    // EventKind::Engine with a label starting "Game
                    // Mode entered:". Label-substring detection is
                    // fragile vs structured event types but matches
                    // the current ipc_client.rs label format; a future
                    // dedicated EventKind::GameMode would tighten this.
                    if ev.label.starts_with("Game Mode entered") {
                        out.game_mode_sessions += 1;
                    }
                }
            }
        }
        out
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn ev(at: SystemTime, kind: EventKind, label: &str) -> RecentEvent {
        RecentEvent {
            at,
            kind,
            label: label.to_owned(),
        }
    }

    /// Mixed event types within the 24-hour window: each category
    /// contributes to its respective counter; events older than 24 h
    /// are excluded.
    #[test]
    fn session_stats_counts_events_within_window() {
        let now = SystemTime::now();
        let ago = |secs: u64| now - Duration::from_secs(secs);
        let recent = vec![
            ev(ago(60), EventKind::Foreground, "notepad -> perf"),
            ev(ago(120), EventKind::Foreground, "vscode -> perf"),
            ev(ago(300), EventKind::Foreground, "bf6 -> game-x3d"),
            ev(ago(180), EventKind::ProBalanceRestrained, "chrome demoted"),
            ev(ago(150), EventKind::ProBalanceRestrained, "slack demoted"),
            ev(ago(140), EventKind::ProBalanceRestored, "chrome restored"),
            ev(
                ago(400),
                EventKind::Engine,
                "Game Mode entered: game-x3d (24 svcs, 16 procs)",
            ),
            // Outside the window — must NOT count.
            ev(
                ago(48 * 60 * 60),
                EventKind::Foreground,
                "ancient -> default",
            ),
            ev(
                ago(25 * 60 * 60),
                EventKind::ProBalanceRestrained,
                "ancient demote",
            ),
        ];
        let stats = SessionStats::from_recent(&recent, now);
        assert_eq!(stats.profiles_applied, 3);
        assert_eq!(stats.probalance_demotions, 2);
        assert_eq!(stats.probalance_restores, 1);
        assert_eq!(stats.game_mode_sessions, 1);
    }

    /// Empty buffer produces zeroes — no panic, no underflow.
    #[test]
    fn session_stats_empty_buffer_yields_zeros() {
        let stats = SessionStats::from_recent(&[], SystemTime::now());
        assert_eq!(stats, SessionStats::default());
    }

    /// Engine events whose label doesn't start with "Game Mode
    /// entered" must NOT increment the game-mode counter — the
    /// label-prefix detection is deliberately narrow.
    #[test]
    fn session_stats_engine_events_without_game_mode_prefix_dont_count() {
        let now = SystemTime::now();
        let recent = vec![
            ev(now - Duration::from_secs(60), EventKind::Engine, "engine paused"),
            ev(now - Duration::from_secs(50), EventKind::Engine, "engine resumed"),
            ev(
                now - Duration::from_secs(40),
                EventKind::Engine,
                "Game Mode exited: game-x3d after 1800s",
            ),
        ];
        let stats = SessionStats::from_recent(&recent, now);
        assert_eq!(
            stats.game_mode_sessions, 0,
            "only 'Game Mode entered' labels should count as sessions"
        );
    }
}
