//! Item 2.9 / audit M-03 — persistent activity-event log.
//!
//! The tray buffers ~1000 events in memory (`AppState.recent`) to power the
//! Activity Log tab and the Status-tab "Recent activity" panel. Before this
//! module the buffer was process-local — every restart of the tray, every
//! service-restart reconnect, every reboot started the user with an empty
//! Activity tab. For an observability-first product positioning that's the
//! wrong default.
//!
//! This module owns the disk side:
//!
//! 1. `ActivityLog::open()` creates `%LOCALAPPDATA%\framesage\activity.jsonl`
//!    if needed and opens a buffered append writer.
//! 2. `ActivityLog::load_last(n)` reads the trailing `n` JSON lines into a
//!    `Vec<PersistedActivityEvent>` so startup can hydrate `AppState.recent`.
//! 3. `ActivityLog::append(...)` writes one line per event and flushes — events
//!    are rare enough (handfuls per second at worst) that a fsync on every
//!    write is comfortably under any latency budget.
//! 4. `ActivityLog::rotate_if_oversized()` truncates back to the last
//!    `MAX_PERSISTED_EVENTS` entries when the file grows past
//!    `MAX_PERSISTED_BYTES`, so the file stays bounded across years of use
//!    without ever pruning entries the in-memory buffer might still want.
//!
//! Format: one event per line, JSON, schema version 1. Field-by-field stable:
//!
//! ```jsonl
//! {"schema_version":1,"at_unix_secs":1736023412,"kind":"foreground","label":"firefox.exe -> perf (pid 4392)"}
//! ```
//!
//! Append-safety: each line is `< 1 KB` in practice; one `write_all` against
//! a `File` opened with `OpenOptions::append(true)` maps to a single
//! `FILE_APPEND_DATA` write on Windows, which the kernel serialises across
//! all writers. No locking needed even if a second tray instance somehow ran
//! (it can't — single-instance enforcement lives in main.rs).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use framesage_core::paths;

/// On-disk schema version. Bump if the field set changes; readers refuse
/// future versions to avoid silently dropping fields they don't understand.
const SCHEMA_VERSION: u32 = 1;

/// Rotation thresholds. We keep the file bounded both by entry count
/// (during startup truncation) and by byte size (the trigger that fires
/// the truncation). At ~200 bytes/line on average, 5 MiB is roughly
/// 25 000 entries — well above the 1000-entry in-memory ceiling, so we
/// never lose history the UI might still want.
const MAX_PERSISTED_BYTES: u64 = 5 * 1024 * 1024;
const MAX_PERSISTED_EVENTS: usize = 10_000;

/// One persisted event. Decoupled from the runtime `RecentEvent` so the
/// on-disk format can evolve without breaking the UI types (and vice
/// versa). `at_unix_secs` is unix-epoch seconds — small, monotonic-ish,
/// easy to grep / sort by.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedActivityEvent {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub at_unix_secs: u64,
    /// Snake-case discriminant string (`"foreground"`, `"engine"`,
    /// `"probalance_restrained"`, `"probalance_restored"`, `"other"`).
    /// Stored as a string rather than an enum so a reader from a newer
    /// version that introduced more kinds doesn't choke a reader from
    /// an older version — unknown values map back to `Other` at load
    /// time.
    pub kind: String,
    pub label: String,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

impl PersistedActivityEvent {
    /// Build a persisted event from an in-memory `(SystemTime, kind_str,
    /// label)` triple. Times before the Unix epoch (shouldn't happen on
    /// Windows but cheap to guard) get a 0 timestamp.
    pub fn new(at: SystemTime, kind: &str, label: String) -> Self {
        let at_unix_secs = at
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            schema_version: SCHEMA_VERSION,
            at_unix_secs,
            kind: kind.to_owned(),
            label,
        }
    }
}

/// Disk-backed activity event log. Holds a buffered writer for append
/// fast-path; the writer is flushed after every event so a crash doesn't
/// drop pending entries. The writer is `Option`al so rotation can drop
/// it (releasing the OS file handle), rename the file, and reopen — all
/// without unsafe sentinel writers.
pub struct ActivityLog {
    path: PathBuf,
    writer: Option<BufWriter<File>>,
}

impl ActivityLog {
    /// Open (creating if needed) the activity log at
    /// [`paths::activity_log_path`]. Creates the parent directory if it
    /// doesn't exist. Append mode means existing entries are preserved
    /// across runs.
    pub fn open() -> Result<Self> {
        let path = paths::activity_log_path();
        Self::open_at(path)
    }

    /// Open at an arbitrary path. Pulled out so tests can supply a
    /// tempdir without relying on the user's real `%LOCALAPPDATA%`.
    pub fn open_at(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create activity-log parent dir {parent:?}"))?;
        }
        let writer = Self::open_append_writer(&path)?;
        Ok(Self {
            path,
            writer: Some(writer),
        })
    }

    /// Open the path in append+create mode and wrap it in a BufWriter.
    /// Pulled out so `open` and `rotate_if_oversized` share the same
    /// flag set.
    fn open_append_writer(path: &std::path::Path) -> Result<BufWriter<File>> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open activity log {path:?}"))?;
        Ok(BufWriter::new(file))
    }

    /// Append one event. Each call serialises to one JSON line + newline
    /// and flushes the buffered writer. Errors are surfaced; the caller
    /// logs and carries on (losing one persisted event is regrettable
    /// but shouldn't crash the UI thread).
    pub fn append(&mut self, event: &PersistedActivityEvent) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("activity-log writer is not open (mid-rotation?)"))?;
        let line = serde_json::to_string(event)
            .with_context(|| "serialise PersistedActivityEvent to JSON")?;
        writer
            .write_all(line.as_bytes())
            .with_context(|| format!("write activity event to {:?}", self.path))?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }

    /// Read the trailing `n` events from disk. Used at startup to
    /// hydrate the in-memory buffer. Cheaply re-opens the file as a
    /// reader (the writer half stays open for appends); both share the
    /// kernel-level file lock granted by `OpenOptions::append`.
    ///
    /// Truncates silently on parse failure for any single line — a
    /// corrupted entry (partial write across a crash, manual edit
    /// gone wrong) shouldn't blank the entire history. The
    /// `schema_version` field guards forward-compat: rows from a
    /// newer schema are dropped rather than misinterpreted.
    pub fn load_last(&self, n: usize) -> Result<Vec<PersistedActivityEvent>> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            // Activity log doesn't exist yet (fresh install) — return
            // empty. Not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("open {:?} for read", self.path)),
        };
        let reader = BufReader::new(file);
        let mut events: Vec<PersistedActivityEvent> = Vec::new();
        for line in reader.lines() {
            let Ok(line) = line else {
                continue;
            };
            if line.is_empty() {
                continue;
            }
            let parsed: Result<PersistedActivityEvent, _> = serde_json::from_str(&line);
            match parsed {
                Ok(ev) if ev.schema_version == SCHEMA_VERSION => events.push(ev),
                // Drop rows from future schema versions (forward compat)
                // and rows that fail to parse (corruption / partial
                // write). The intentional silence keeps a single bad
                // line from killing the whole history.
                _ => continue,
            }
        }
        // Keep only the tail. Avoids a `clone+drain(0..)` two-pass
        // pattern — `split_off` returns the tail in O(N) without an
        // intermediate vec.
        if events.len() > n {
            let start = events.len() - n;
            events = events.split_off(start);
        }
        Ok(events)
    }

    /// If the underlying file has grown past `MAX_PERSISTED_BYTES`,
    /// rewrite it with only the last `MAX_PERSISTED_EVENTS` entries.
    /// Idempotent — call it on startup and any time you want a
    /// bounded-size guarantee.
    ///
    /// Writes to a sibling `.tmp` file, then atomically renames over
    /// the active path. Any reader mid-load (impossible in practice —
    /// only the tray reads this — but cheap to be correct) either sees
    /// the old or new file, never a partial. After rename we reopen
    /// the writer half so subsequent appends go to the new file.
    pub fn rotate_if_oversized(&mut self) -> Result<()> {
        let metadata = match std::fs::metadata(&self.path) {
            Ok(m) => m,
            Err(_) => return Ok(()),
        };
        if metadata.len() <= MAX_PERSISTED_BYTES {
            return Ok(());
        }
        let events = self.load_last(MAX_PERSISTED_EVENTS)?;
        let tmp = self.path.with_extension("jsonl.tmp");

        // 1. Write the tail into a sibling tmp file.
        {
            let f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .with_context(|| format!("create activity-log rotation tmp {tmp:?}"))?;
            let mut w = BufWriter::new(f);
            for ev in &events {
                let line = serde_json::to_string(ev)?;
                w.write_all(line.as_bytes())?;
                w.write_all(b"\n")?;
            }
            w.flush()?;
        }

        // 2. Drop our writer half so Windows lets us rename over the
        //    target. `take()` leaves `self.writer = None` so an append
        //    racing with rotation gets a clear error rather than
        //    writing to a stale file handle.
        let _ = self.writer.take();

        // 3. Atomic rename. On Windows ReplaceFile semantics apply when
        //    the target exists and isn't open elsewhere.
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("rotate activity log {tmp:?} -> {:?}", self.path))?;

        // 4. Re-open the writer in append mode against the new file.
        self.writer = Some(Self::open_append_writer(&self.path)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Round-trip: write three events, reload, get them back in order
    /// with their fields intact. Locks the on-disk schema is mutually
    /// consistent with the loader.
    #[test]
    fn round_trip_three_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("activity.jsonl");
        let mut log = ActivityLog::open_at(path.clone()).unwrap();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_736_000_000);
        log.append(&PersistedActivityEvent::new(
            now,
            "foreground",
            "firefox.exe -> perf".into(),
        ))
        .unwrap();
        log.append(&PersistedActivityEvent::new(
            now + Duration::from_secs(1),
            "engine",
            "engine paused".into(),
        ))
        .unwrap();
        log.append(&PersistedActivityEvent::new(
            now + Duration::from_secs(2),
            "probalance_restrained",
            "restrained chrome.exe".into(),
        ))
        .unwrap();
        // Re-open from disk to prove the buffered writer flushed.
        let reopened = ActivityLog::open_at(path).unwrap();
        let loaded = reopened.load_last(100).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].kind, "foreground");
        assert_eq!(loaded[1].label, "engine paused");
        assert_eq!(loaded[2].at_unix_secs, 1_736_000_002);
    }

    /// `load_last` must return ONLY the tail when the file exceeds the
    /// requested limit. This is the load-bearing constraint that
    /// prevents startup from spending unbounded time hydrating
    /// `AppState.recent` from a year-old log.
    #[test]
    fn load_last_returns_only_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("activity.jsonl");
        let mut log = ActivityLog::open_at(path.clone()).unwrap();
        for i in 0..50 {
            log.append(&PersistedActivityEvent::new(
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + i),
                "engine",
                format!("event {i}"),
            ))
            .unwrap();
        }
        let loaded = ActivityLog::open_at(path).unwrap().load_last(10).unwrap();
        assert_eq!(loaded.len(), 10);
        assert_eq!(loaded[0].label, "event 40");
        assert_eq!(loaded[9].label, "event 49");
    }

    /// A line from a future schema version must be dropped silently —
    /// the rest of the file remains usable. This prevents a downgrade
    /// (rare but possible: user reverts framesage) from blanking the
    /// activity tab.
    #[test]
    fn future_schema_version_lines_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("activity.jsonl");
        // Hand-write a file with a v1 row, a v999 row, and another v1.
        std::fs::write(
            &path,
            br#"{"schema_version":1,"at_unix_secs":100,"kind":"engine","label":"ok"}
{"schema_version":999,"at_unix_secs":200,"kind":"engine","label":"future"}
{"schema_version":1,"at_unix_secs":300,"kind":"engine","label":"also ok"}
"#,
        )
        .unwrap();
        let log = ActivityLog::open_at(path).unwrap();
        let loaded = log.load_last(100).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].label, "ok");
        assert_eq!(loaded[1].label, "also ok");
    }

    /// A corrupted line (mid-line crash, manual edit gone wrong) must
    /// be dropped without killing the rest of the file.
    #[test]
    fn corrupt_lines_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("activity.jsonl");
        std::fs::write(
            &path,
            br#"{"schema_version":1,"at_unix_secs":100,"kind":"engine","label":"before"}
not valid json at all
{"schema_version":1,"at_unix_secs":300,"kind":"engine","label":"after"}
"#,
        )
        .unwrap();
        let log = ActivityLog::open_at(path).unwrap();
        let loaded = log.load_last(100).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].label, "before");
        assert_eq!(loaded[1].label, "after");
    }
}
