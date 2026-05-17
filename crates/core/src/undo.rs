//! Item 3.5 — undo-log public types.
//!
//! Lives in `framesage-core` rather than `framesage-engine` so the IPC
//! layer (`framesage-ipc`) can carry them without forming a cycle:
//! `ipc` → `core` (already), `engine` → `ipc`, and `engine` → `core`.
//! Putting these types in `engine` would force `ipc` → `engine`, which
//! breaks the layering.
//!
//! `framesage-engine` re-exports these and adds the in-memory ring
//! buffer (`UndoLog`) that holds them — that's the implementation
//! detail and stays engine-local.

use serde::{Deserialize, Serialize};

use crate::PriorityClass;

/// One row in the undo log. The `id` is monotonic across the engine's
/// lifetime so the CLI's `framesage undo list` can show stable
/// identifiers; the `at_unix_secs` is for human-readable
/// "5 minutes ago" formatting in the tray UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoEntry {
    pub id: u64,
    pub at_unix_secs: u64,
    pub action: UndoableAction,
}

/// What action the user took and the prior state needed to reverse it.
///
/// Each variant carries:
///
/// * Identifying data (`pid`, `exe_name`) so the tray can render the
///   entry without re-querying.
/// * The "before" state needed to undo. For `SetPriority`/`SetAffinity`
///   that's the previous value; for `SuspendProcess`/`ResumeProcess`
///   the reverse action is the inverse and no extra state is needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UndoableAction {
    /// User changed `pid`'s priority class. Undo restores
    /// `previous_raw_class` via `set_priority_class_for_pid`.
    SetPriority {
        pid: u32,
        exe_name: String,
        previous_raw_class: u32,
        applied_class: PriorityClass,
    },
    /// User changed `pid`'s affinity mask. Undo restores
    /// `previous_mask` via `set_affinity_mask_for_pid`. If the
    /// previous mask was unreadable at capture time (rare; PID
    /// protected mid-call), the entry carries `None` and undo
    /// rejects with a clear error rather than zeroing the affinity.
    SetAffinity {
        pid: u32,
        exe_name: String,
        previous_mask: Option<u64>,
        applied_mask: u64,
    },
    /// User suspended `pid`. Undo resumes.
    SuspendProcess { pid: u32, exe_name: String },
    /// User resumed `pid`. Undo re-suspends.
    ResumeProcess { pid: u32, exe_name: String },
}

impl UndoableAction {
    /// Short one-line human description. Used in the CLI's
    /// `framesage undo list` output and in the tray's recent-undo
    /// section. The label is the SAME regardless of whether the
    /// entry is current-state or already-undone — the caller adds
    /// "[undone]" prefix from context.
    pub fn describe(&self) -> String {
        match self {
            UndoableAction::SetPriority {
                pid,
                exe_name,
                applied_class,
                ..
            } => {
                format!("set priority of {exe_name} (pid {pid}) to {applied_class}")
            }
            UndoableAction::SetAffinity {
                pid,
                exe_name,
                applied_mask,
                ..
            } => {
                format!("set affinity of {exe_name} (pid {pid}) to 0x{applied_mask:016x}")
            }
            UndoableAction::SuspendProcess { pid, exe_name } => {
                format!("suspended {exe_name} (pid {pid})")
            }
            UndoableAction::ResumeProcess { pid, exe_name } => {
                format!("resumed {exe_name} (pid {pid})")
            }
        }
    }
}

/// What `Engine::undo_last` returns to the caller. The `summary` is a
/// short human-readable description of WHAT was undone, suitable for
/// the CLI's stdout and the tray's last-action echo. The `failure`
/// field is `Some` when the entry was popped but the reverse syscall
/// itself failed — typically because the target PID exited between
/// the original action and the undo. The entry is still removed from
/// the log (idempotent semantics: a second undo invocation pops the
/// next entry, not the one that failed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UndoSummary {
    pub entry: UndoEntry,
    pub summary: String,
    pub failure: Option<String>,
}
