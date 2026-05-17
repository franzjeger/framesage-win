//! Item 3.5 — per-action undo log.
//!
//! Closes the audit's "no way to reverse a misclick" gap. The engine
//! records every user-initiated action that mutates kernel state into
//! an in-memory ring buffer; `Engine::undo_last` pops the most recent
//! entry and reverses it.
//!
//! Public types (`UndoEntry`, `UndoableAction`, `UndoSummary`) live
//! in `framesage-core` so the IPC layer can carry them without
//! forming a `ipc → engine` cycle. This module defines the
//! `UndoLog` ring buffer that holds them — the engine-local
//! implementation detail.
//!
//! # Scope of this initial PR
//!
//! Records the four most common Processes-tab right-click actions:
//!
//! * `SetPriority` — restores the previous Win32 priority class
//!   constant captured before the change.
//! * `SetAffinity` — restores the previous affinity mask captured
//!   before the change.
//! * `SuspendProcess` — reverses with a resume call.
//! * `ResumeProcess` — reverses with a suspend call.
//!
//! Not yet covered (follow-up scope):
//!
//! * `ApplyOnce` — needs the previous `AppliedRecord` shape captured
//!   pre-apply, which is more involved (per-PID prev state for every
//!   knob the profile touched).
//! * Manual override / Manual Global Game Mode toggles — those have
//!   their own panic-button paths; layering undo on top is lower
//!   priority than the per-PID actions.
//! * Affinity-rule mutations — need to capture the full previous
//!   `AffinityRule` shape for a clean restore.
//!
//! Each can ship as its own follow-up; the `UndoableAction` enum is
//! designed to grow by adding variants without breaking callers.
//!
//! # Why an in-memory ring buffer
//!
//! The undo log is a UX nicety, not a crash-recovery primitive. If
//! the service restarts, the user's session is already disturbed
//! enough that "you can undo what happened 30 seconds ago" matters
//! less than "the engine is healthy again." If a future PR needs
//! persistence (cross-session undo), the entries are already
//! serde-derived; appending each entry to a JSONL file at the same
//! `%LOCALAPPDATA%\framesage\` path the tray's activity log uses is
//! a straight-line follow-up.

use std::collections::VecDeque;
use std::time::SystemTime;

pub use framesage_core::{UndoEntry, UndoSummary, UndoableAction};

/// Maximum entries retained in the ring buffer. 50 is comfortably
/// above any realistic "I want to undo my last few mistakes" workflow
/// while keeping the memory footprint trivial (~5 KB on average — most
/// entries are small structs with one PID and one prior value).
pub(crate) const UNDO_LOG_CAP: usize = 50;

/// In-memory ring buffer of recorded actions. Lives inside
/// `EngineState` and is mutated under the engine's `RwLock`.
#[derive(Debug, Default)]
pub(crate) struct UndoLog {
    entries: VecDeque<UndoEntry>,
    next_id: u64,
}

impl UndoLog {
    /// Append a new action. Auto-fills `id` (monotonic from
    /// `next_id`) and `at_unix_secs` from the system clock. Older
    /// entries are evicted when length exceeds `UNDO_LOG_CAP`.
    pub(crate) fn record(&mut self, action: UndoableAction) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let at_unix_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.entries.push_back(UndoEntry {
            id,
            at_unix_secs,
            action,
        });
        while self.entries.len() > UNDO_LOG_CAP {
            self.entries.pop_front();
        }
    }

    /// Pop the most recent entry, leaving everything older intact.
    /// Returns `None` when the log is empty.
    pub(crate) fn pop_last(&mut self) -> Option<UndoEntry> {
        self.entries.pop_back()
    }

    /// Read-only view ordered newest-first, capped at `limit` entries.
    /// Used by `framesage undo list` and the tray's undo panel.
    pub(crate) fn snapshot_newest_first(&self, limit: usize) -> Vec<UndoEntry> {
        self.entries.iter().rev().take(limit).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framesage_core::PriorityClass;

    fn dummy_suspend(pid: u32) -> UndoableAction {
        UndoableAction::SuspendProcess {
            pid,
            exe_name: format!("p{pid}.exe"),
        }
    }

    /// Ring buffer caps at UNDO_LOG_CAP — pushing 100 entries leaves
    /// the most recent 50.
    #[test]
    fn ring_buffer_caps_at_undo_log_cap() {
        let mut log = UndoLog::default();
        for i in 0..(UNDO_LOG_CAP as u32 * 2) {
            log.record(dummy_suspend(i));
        }
        assert_eq!(log.entries.len(), UNDO_LOG_CAP);
        assert_eq!(log.entries.front().unwrap().id as usize, UNDO_LOG_CAP);
        assert_eq!(
            log.entries.back().unwrap().id as usize,
            UNDO_LOG_CAP * 2 - 1
        );
    }

    /// `pop_last` returns the most recent entry and leaves older
    /// entries intact.
    #[test]
    fn pop_last_returns_most_recent() {
        let mut log = UndoLog::default();
        log.record(dummy_suspend(1));
        log.record(dummy_suspend(2));
        log.record(dummy_suspend(3));
        let popped = log.pop_last().unwrap();
        match popped.action {
            UndoableAction::SuspendProcess { pid, .. } => assert_eq!(pid, 3),
            other => panic!("unexpected variant: {other:?}"),
        }
        assert_eq!(log.entries.len(), 2);
    }

    /// `snapshot_newest_first` orders entries newest-first and
    /// respects the limit.
    #[test]
    fn snapshot_orders_newest_first_with_limit() {
        let mut log = UndoLog::default();
        for i in 1..=5 {
            log.record(dummy_suspend(i));
        }
        let snap = log.snapshot_newest_first(3);
        assert_eq!(snap.len(), 3);
        match &snap[0].action {
            UndoableAction::SuspendProcess { pid, .. } => assert_eq!(*pid, 5),
            other => panic!("unexpected variant: {other:?}"),
        }
        match &snap[2].action {
            UndoableAction::SuspendProcess { pid, .. } => assert_eq!(*pid, 3),
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    /// `describe` produces a one-line summary for each variant.
    #[test]
    fn describe_mentions_pid_and_exe() {
        let actions = [
            UndoableAction::SetPriority {
                pid: 100,
                exe_name: "explorer.exe".into(),
                previous_raw_class: 0x20,
                applied_class: PriorityClass::High,
            },
            UndoableAction::SetAffinity {
                pid: 200,
                exe_name: "chrome.exe".into(),
                previous_mask: Some(0xff),
                applied_mask: 0xf0,
            },
            UndoableAction::SuspendProcess {
                pid: 300,
                exe_name: "onedrive.exe".into(),
            },
            UndoableAction::ResumeProcess {
                pid: 400,
                exe_name: "dropbox.exe".into(),
            },
        ];
        for a in &actions {
            let s = a.describe();
            match a {
                UndoableAction::SetPriority { pid, exe_name, .. }
                | UndoableAction::SetAffinity { pid, exe_name, .. }
                | UndoableAction::SuspendProcess { pid, exe_name }
                | UndoableAction::ResumeProcess { pid, exe_name } => {
                    assert!(s.contains(&pid.to_string()), "pid missing: {s}");
                    assert!(s.contains(exe_name), "exe missing: {s}");
                }
            }
        }
    }
}
