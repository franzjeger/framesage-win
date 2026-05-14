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
}
