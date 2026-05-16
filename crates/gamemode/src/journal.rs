//! Crash-safe revert journal.
//!
//! The engine writes one JSON file before any system-level change:
//! `%ProgramData%\framesage\game-mode.journal`. The file describes what the
//! OS state *was* before the session and what we've changed *so far*. Writes
//! are atomic (temp + rename), so a crash mid-write never leaves a corrupt
//! file. Updates happen after each successful action — `AppliedActions`
//! grows incrementally so revert can act on whatever we managed to apply.
//!
//! Single-session semantics: at most one journal file exists at a time. If a
//! journal is present at service startup, the engine treats it as an orphan
//! from a previous, crashed session, reverts based on it, and deletes it
//! before applying any new state.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::debug;
use uuid::Uuid;

use crate::state::{AppliedActions, PreviousState};
use framesage_core::ProfileId;

const JOURNAL_FILE_NAME: &str = "game-mode.journal";
const JOURNAL_SCHEMA_VERSION: u32 = 1;

/// Append-on-revert history file. Item 1.4 / audit C-07.
///
/// Before this addition, `revert_system_mode_locked` called
/// `journal.delete()` on exit — and with it, **every record of what Game
/// Mode actually did**. A 2-hour `game-x3d` session touching 30+
/// services + 24+ processes vanished completely the moment the user
/// alt-tabbed away from the game.
///
/// Now: instead of deleting, we append a `SessionHistoryEntry`
/// (start+end timestamps, profile, full `AppliedActions`, full
/// `PreviousState`) to `sessions.jsonl`. The active journal file is
/// then deleted (the recovery path still keys on its presence /
/// absence, so that contract stays clean).
///
/// File rotates to `sessions.jsonl.1` when it crosses
/// [`SESSIONS_HISTORY_MAX_BYTES`]. Keeps a single rotation generation
/// to bound disk use without losing recent history.
const SESSIONS_HISTORY_FILE_NAME: &str = "sessions.jsonl";

/// Rotate the sessions history file at 10 MB. A typical
/// `SessionHistoryEntry` is ~3-5 KB serialized, so 10 MB ≈ 2,000–3,000
/// session records — months of typical use. Sized to keep the file
/// scannable by Notepad and trivially-tail-able for support.
const SESSIONS_HISTORY_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// One completed Game Mode session — what the active journal contained
/// plus when it ended and why. Append-only, never mutated after write.
///
/// Lives in the same module as `JournalEntry` because it's the same data
/// model with two extra fields (`ended_at`, `revert_reason`); separating
/// the two would force a translation layer for no real benefit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionHistoryEntry {
    pub schema_version: u32,
    pub session_id: Uuid,
    pub profile_id: ProfileId,
    /// UNIX-style timestamp of session start (mirrors `JournalEntry`).
    pub started_at_unix_secs: u64,
    /// UNIX-style timestamp of session end (revert time).
    pub ended_at_unix_secs: u64,
    /// Why this session ended. Free-form string so we don't have to
    /// version an enum every time the engine learns a new revert
    /// trigger; the producer side is a small fixed set (foreground
    /// change, manual off via tray/CLI, profile swap, service
    /// shutdown, crash recovery).
    pub revert_reason: String,
    pub previous: PreviousState,
    pub applied: AppliedActions,
}

impl SessionHistoryEntry {
    /// Construct from a `JournalEntry` + end-time + reason. Borrows the
    /// entry's identity / start time / pre-state / applied actions
    /// verbatim — those are the source of truth for what happened.
    pub fn from_journal(entry: &JournalEntry, ended_at_unix_secs: u64, reason: &str) -> Self {
        Self {
            schema_version: entry.schema_version,
            session_id: entry.session_id,
            profile_id: entry.profile_id.clone(),
            started_at_unix_secs: entry.created_at_unix_secs,
            ended_at_unix_secs,
            revert_reason: reason.to_owned(),
            previous: entry.previous.clone(),
            applied: entry.applied.clone(),
        }
    }

    /// Duration of the session in seconds. Saturating subtraction
    /// because clock skew on resume-from-sleep can produce a `started`
    /// that's "after" `ended` by a tiny amount; rather than panic, we
    /// report 0.
    pub fn duration_secs(&self) -> u64 {
        self.ended_at_unix_secs
            .saturating_sub(self.started_at_unix_secs)
    }
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("io error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("unsupported journal schema version: found {found}, expected {expected}")]
    UnsupportedSchema { expected: u32, found: u32 },
}

/// A single Game Mode session, written out to the journal file.
///
/// Schema-versioned so older binaries can detect a format change and decline
/// to revert based on an unknown layout (better than misinterpreting).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JournalEntry {
    pub schema_version: u32,
    pub session_id: Uuid,
    pub profile_id: ProfileId,
    /// UNIX-style timestamp of when the session started. Surface in logs and
    /// the CLI; helpful when a session looks abandoned.
    pub created_at_unix_secs: u64,
    pub previous: PreviousState,
    pub applied: AppliedActions,
}

impl JournalEntry {
    pub fn new(profile_id: ProfileId, previous: PreviousState) -> Self {
        let created_at_unix_secs = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            session_id: Uuid::new_v4(),
            profile_id,
            created_at_unix_secs,
            previous,
            applied: AppliedActions::default(),
        }
    }
}

/// Owns the journal file path; reads, writes, and deletes are routed through
/// here so the rest of the code never sees raw paths or partial writes.
#[derive(Debug, Clone)]
pub struct Journal {
    path: PathBuf,
}

impl Journal {
    /// Construct from the platform's standard config directory.
    pub fn at_default_path() -> Self {
        Self::at(framesage_core::paths::config_dir().join(JOURNAL_FILE_NAME))
    }

    /// Construct from an explicit path — used in tests.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Is there a journal file on disk right now?
    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Read the current journal entry, if any. Returns `Ok(None)` for the
    /// common "no journal" case so callers don't have to discriminate
    /// NotFound vs other IO errors.
    pub fn read(&self) -> Result<Option<JournalEntry>, JournalError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(JournalError::Io {
                    path: self.path.display().to_string(),
                    source: e,
                });
            }
        };
        let entry: JournalEntry = serde_json::from_slice(&bytes)?;
        if entry.schema_version != JOURNAL_SCHEMA_VERSION {
            return Err(JournalError::UnsupportedSchema {
                expected: JOURNAL_SCHEMA_VERSION,
                found: entry.schema_version,
            });
        }
        Ok(Some(entry))
    }

    /// Atomically write an entry to disk: serialize, write to `<path>.tmp`,
    /// fsync (best-effort), then rename. On Windows the rename is atomic when
    /// source and destination share a volume.
    pub fn write(&self, entry: &JournalEntry) -> Result<(), JournalError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| JournalError::Io {
                path: parent.display().to_string(),
                source: e,
            })?;
        }

        let body = serde_json::to_vec_pretty(entry)?;

        let mut tmp = self.path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp_path = PathBuf::from(tmp);

        // Scope the file handle so it closes before the rename.
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp_path).map_err(|e| JournalError::Io {
                path: tmp_path.display().to_string(),
                source: e,
            })?;
            f.write_all(&body).map_err(|e| JournalError::Io {
                path: tmp_path.display().to_string(),
                source: e,
            })?;
            // Best-effort fsync; not all filesystems honour it (notably some
            // network mounts), but it costs nothing to ask.
            let _ = f.sync_all();
        }

        std::fs::rename(&tmp_path, &self.path).map_err(|e| {
            // Clean up the temp file if rename failed — otherwise we leak it.
            let _ = std::fs::remove_file(&tmp_path);
            JournalError::Io {
                path: self.path.display().to_string(),
                source: e,
            }
        })?;
        debug!(path = %self.path.display(), "journal written");
        Ok(())
    }

    /// Remove the journal. Idempotent: `Ok(())` if it didn't exist.
    pub fn delete(&self) -> Result<(), JournalError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(JournalError::Io {
                path: self.path.display().to_string(),
                source: e,
            }),
        }
    }

    /// Path of the sessions history file. Lives next to the journal in
    /// the same config dir.
    pub fn history_path(&self) -> PathBuf {
        match self.path.parent() {
            Some(parent) => parent.join(SESSIONS_HISTORY_FILE_NAME),
            None => PathBuf::from(SESSIONS_HISTORY_FILE_NAME),
        }
    }

    /// Append a completed session to the history file (`sessions.jsonl`).
    ///
    /// Format: newline-delimited JSON, one record per line — trivially
    /// tail-able, greppable, scannable in Notepad. Rotates to
    /// `sessions.jsonl.1` when the file exceeds
    /// [`SESSIONS_HISTORY_MAX_BYTES`]; only one rotation generation kept.
    /// The rotation is a single rename + new-file, so a crash mid-rotate
    /// at worst loses one record (the in-flight append) — never
    /// corrupts previously-recorded history.
    ///
    /// Errors are returned so the caller can log loudly. The engine's
    /// revert path treats history-append failure as non-fatal: the
    /// active journal still gets deleted, the user's system still
    /// reverts, only the audit trail is lost. Logging captures that.
    pub fn append_to_history(&self, entry: &SessionHistoryEntry) -> Result<(), JournalError> {
        let history = self.history_path();
        if let Some(parent) = history.parent() {
            std::fs::create_dir_all(parent).map_err(|e| JournalError::Io {
                path: parent.display().to_string(),
                source: e,
            })?;
        }

        // Rotate before the append (not after) so we never exceed the cap
        // by more than one record. Check is cheap: metadata().len() is one
        // FindFirstFile under the hood.
        if let Ok(meta) = std::fs::metadata(&history) {
            if meta.len() >= SESSIONS_HISTORY_MAX_BYTES {
                let rotated = history.with_extension("jsonl.1");
                // remove any pre-existing .1 — we keep exactly one generation
                let _ = std::fs::remove_file(&rotated);
                if let Err(e) = std::fs::rename(&history, &rotated) {
                    // Couldn't rotate; log via Err but still try to append —
                    // worst case the file gets a bit larger than the cap.
                    debug!(
                        old = %history.display(),
                        new = %rotated.display(),
                        error = %e,
                        "sessions history rotation failed; appending to oversized file"
                    );
                }
            }
        }

        // JSON-encode the entry as a single line. serde_json::to_string
        // produces no internal newlines for a struct, so the trailing
        // \n we add is the only newline; ndjson invariant holds.
        let mut line = serde_json::to_string(entry)?;
        line.push('\n');

        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&history)
            .map_err(|e| JournalError::Io {
                path: history.display().to_string(),
                source: e,
            })?;
        f.write_all(line.as_bytes()).map_err(|e| JournalError::Io {
            path: history.display().to_string(),
            source: e,
        })?;
        // Best-effort fsync; same reasoning as `write` above — not all
        // filesystems honour it but it costs nothing to ask.
        let _ = f.sync_all();
        debug!(
            path = %history.display(),
            session = %entry.session_id,
            "session history appended"
        );
        Ok(())
    }

    /// Read every line in the sessions history file, returning each one
    /// as a parsed `SessionHistoryEntry`. Skips lines that fail to parse
    /// (forward-compat: an older binary writing a future schema would
    /// produce records this binary can't read, but we'd rather show the
    /// readable history than refuse to render anything).
    ///
    /// Only reads the active `sessions.jsonl`; the rotated `.1` is not
    /// surfaced through this method. Callers that need full history can
    /// concatenate the two themselves. Most UI consumers want recent
    /// history, which the active file already covers.
    pub fn read_history(&self) -> Result<Vec<SessionHistoryEntry>, JournalError> {
        let history = self.history_path();
        let bytes = match std::fs::read(&history) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(JournalError::Io {
                    path: history.display().to_string(),
                    source: e,
                })
            }
        };
        let text = String::from_utf8_lossy(&bytes);
        let mut out = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<SessionHistoryEntry>(line) {
                Ok(entry) => out.push(entry),
                Err(e) => {
                    debug!(error = %e, "skipping unreadable sessions.jsonl line");
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ServiceStateSnapshot, ServiceStatus, SuspendedProcessSnapshot};
    use framesage_core::PowerPlanId;

    fn tmp_journal_path() -> PathBuf {
        let unique = format!(
            "framesage-journal-test-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        );
        std::env::temp_dir().join(unique)
    }

    fn sample_entry() -> JournalEntry {
        JournalEntry {
            schema_version: JOURNAL_SCHEMA_VERSION,
            session_id: Uuid::new_v4(),
            profile_id: ProfileId("game-x3d".into()),
            created_at_unix_secs: 1_700_000_000,
            previous: PreviousState {
                taskbar_visible: true,
                active_power_plan: Some(PowerPlanId::Balanced),
                services: vec![ServiceStateSnapshot {
                    id: "SysMain".into(),
                    status: ServiceStatus::Running,
                }],
                suspended_pids: vec![],
            },
            applied: AppliedActions {
                hid_taskbar: true,
                stopped_services: vec!["SysMain".into()],
                suspended_pids: vec![SuspendedProcessSnapshot {
                    pid: 1234,
                    exe: "OneDrive.exe".into(),
                }],
                switched_power_plan: true,
                set_focus_assist: false,
                paused_windows_update: false,
            },
        }
    }

    #[test]
    fn read_returns_none_when_no_file() {
        let path = tmp_journal_path();
        let journal = Journal::at(&path);
        assert!(!journal.exists());
        assert!(journal.read().unwrap().is_none());
    }

    #[test]
    fn write_then_read_round_trip_preserves_entry() {
        let path = tmp_journal_path();
        let journal = Journal::at(&path);
        let entry = sample_entry();

        journal.write(&entry).unwrap();
        assert!(journal.exists());

        let loaded = journal.read().unwrap().expect("journal present");
        assert_eq!(loaded, entry);

        journal.delete().unwrap();
        assert!(!journal.exists());
    }

    #[test]
    fn delete_is_idempotent() {
        let path = tmp_journal_path();
        let journal = Journal::at(&path);
        journal.delete().unwrap(); // first call: file didn't exist
        journal.delete().unwrap(); // second call: still doesn't exist
    }

    #[test]
    fn write_replaces_existing_entry_atomically() {
        let path = tmp_journal_path();
        let journal = Journal::at(&path);

        let first = sample_entry();
        journal.write(&first).unwrap();

        let mut second = first.clone();
        second.applied.stopped_services.push("WSearch".into());
        journal.write(&second).unwrap();

        let loaded = journal.read().unwrap().expect("journal present");
        assert_eq!(loaded.applied.stopped_services, vec!["SysMain", "WSearch"]);

        journal.delete().unwrap();
    }

    #[test]
    fn schema_mismatch_returns_error() {
        let path = tmp_journal_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        // Hand-craft a journal with the wrong schema_version.
        let bad = serde_json::json!({
            "schema_version": 999,
            "session_id": Uuid::new_v4().to_string(),
            "profile_id": "any",
            "created_at_unix_secs": 0,
            "previous": {
                "taskbar_visible": true,
                "active_power_plan": null,
                "services": [],
                "suspended_pids": []
            },
            "applied": {
                "hid_taskbar": false,
                "stopped_services": [],
                "suspended_pids": [],
                "switched_power_plan": false,
                "set_focus_assist": false,
                "paused_windows_update": false
            }
        });
        std::fs::write(&path, bad.to_string()).unwrap();

        let journal = Journal::at(&path);
        match journal.read() {
            Err(JournalError::UnsupportedSchema { expected, found }) => {
                assert_eq!(expected, JOURNAL_SCHEMA_VERSION);
                assert_eq!(found, 999);
            }
            other => panic!("expected schema error, got {other:?}"),
        }

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn tmp_file_is_cleaned_up_on_successful_write() {
        let path = tmp_journal_path();
        let journal = Journal::at(&path);
        journal.write(&sample_entry()).unwrap();

        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp_path = PathBuf::from(tmp);
        assert!(
            !tmp_path.exists(),
            "leftover .tmp file after successful write"
        );

        journal.delete().unwrap();
    }

    // ─── Item 1.4 — session history append-on-revert ───────────────

    fn unique_temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "framesage-history-test-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_history_entry(end_offset: u64) -> SessionHistoryEntry {
        let je = sample_entry();
        SessionHistoryEntry::from_journal(
            &je,
            je.created_at_unix_secs + end_offset,
            "foreground_changed",
        )
    }

    /// Bedrock: a freshly-created journal has no history file; reading
    /// it yields an empty Vec (not an error). Lets the engine call
    /// `read_history` unconditionally without special-casing the
    /// "first session ever" case.
    #[test]
    fn read_history_returns_empty_when_no_file() {
        let dir = unique_temp_dir();
        let journal = Journal::at(dir.join(JOURNAL_FILE_NAME));
        let history = journal.read_history().unwrap();
        assert!(history.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// One append, one read — preserves every field including the
    /// computed duration and the revert reason.
    #[test]
    fn append_one_session_round_trips() {
        let dir = unique_temp_dir();
        let journal = Journal::at(dir.join(JOURNAL_FILE_NAME));
        let entry = sample_history_entry(7200); // 2-hour session

        journal.append_to_history(&entry).unwrap();
        let history = journal.read_history().unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0], entry);
        assert_eq!(history[0].duration_secs(), 7200);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Three appends → three entries in append order. Closes the load-
    /// bearing case the audit identified: a user playing three
    /// sessions in a row keeps records of all three, not just the
    /// last.
    #[test]
    fn append_preserves_chronological_order() {
        let dir = unique_temp_dir();
        let journal = Journal::at(dir.join(JOURNAL_FILE_NAME));

        let mut a = sample_history_entry(60);
        a.profile_id = ProfileId("valorant".into());
        let mut b = sample_history_entry(120);
        b.profile_id = ProfileId("bf6".into());
        let mut c = sample_history_entry(180);
        c.profile_id = ProfileId("fortnite".into());

        journal.append_to_history(&a).unwrap();
        journal.append_to_history(&b).unwrap();
        journal.append_to_history(&c).unwrap();

        let history = journal.read_history().unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].profile_id.0, "valorant");
        assert_eq!(history[1].profile_id.0, "bf6");
        assert_eq!(history[2].profile_id.0, "fortnite");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Round-trip via from_journal: the history record carries the
    /// session_id forward so support can correlate a journal-based
    /// crash with the historical entry. Locks the contract that
    /// from_journal copies the session_id verbatim.
    #[test]
    fn from_journal_preserves_session_id() {
        let je = sample_entry();
        let h = SessionHistoryEntry::from_journal(&je, 12345, "manual_off");
        assert_eq!(h.session_id, je.session_id);
        assert_eq!(h.started_at_unix_secs, je.created_at_unix_secs);
        assert_eq!(h.ended_at_unix_secs, 12345);
        assert_eq!(h.revert_reason, "manual_off");
        assert_eq!(h.applied, je.applied);
        assert_eq!(h.previous, je.previous);
    }

    /// Rotation: when sessions.jsonl crosses the size cap, it gets
    /// renamed to sessions.jsonl.1 and a fresh sessions.jsonl is
    /// started. read_history returns only the active file. Bounds disk
    /// use without losing recent records.
    #[test]
    fn rotation_when_history_exceeds_cap() {
        use std::io::Write;
        let dir = unique_temp_dir();
        let journal = Journal::at(dir.join(JOURNAL_FILE_NAME));
        let history_path = journal.history_path();

        // Manually pre-seed sessions.jsonl past the cap so the next
        // append triggers rotation. Use junk bytes — content doesn't
        // matter for the rotation decision (only file size does).
        let pad = vec![b'x'; SESSIONS_HISTORY_MAX_BYTES as usize + 1024];
        let mut f = std::fs::File::create(&history_path).unwrap();
        f.write_all(&pad).unwrap();
        drop(f);

        // Now append. Should rotate the oversized file to .1 and start
        // a new one containing only this entry.
        let entry = sample_history_entry(60);
        journal.append_to_history(&entry).unwrap();

        let rotated = history_path.with_extension("jsonl.1");
        assert!(rotated.exists(), "rotation must produce sessions.jsonl.1");

        let history = journal.read_history().unwrap();
        assert_eq!(
            history.len(),
            1,
            "active file should only contain the post-rotation append"
        );
        assert_eq!(history[0], entry);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Lines that fail to parse (e.g. from a future schema we don't
    /// understand) are skipped rather than aborting the whole read.
    /// Forward-compat — a newer engine writing a future record format
    /// shouldn't make this binary's UI render an empty history when
    /// there ARE readable records.
    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        use std::io::Write;
        let dir = unique_temp_dir();
        let journal = Journal::at(dir.join(JOURNAL_FILE_NAME));
        let history_path = journal.history_path();

        let entry = sample_history_entry(60);
        let good_line = format!("{}\n", serde_json::to_string(&entry).unwrap());

        let mut f = std::fs::File::create(&history_path).unwrap();
        f.write_all(good_line.as_bytes()).unwrap();
        f.write_all(b"this is not JSON, just garbage\n").unwrap();
        f.write_all(b"{ \"schema_version\": 999, \"unknown\": true }\n")
            .unwrap();
        f.write_all(good_line.as_bytes()).unwrap();
        drop(f);

        let history = journal.read_history().unwrap();
        assert_eq!(history.len(), 2, "two good lines, two bad lines skipped");

        std::fs::remove_dir_all(&dir).ok();
    }
}
