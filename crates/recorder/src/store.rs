//! On-disk session store — jsonl writer/reader + retention policy per
//! `audit/v0.7-architecture.md` §2.3.
//!
//! The directory is injected (production: the service passes
//! `%ProgramData%\framesage\sessions\`; tests pass a tempdir), so this
//! module stays platform-independent. ACLs are the service's concern —
//! the sessions dir inherits from the hardened config dir.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::schema::{SessionEvent, SCHEMA_VERSION};

/// Per-session file cap (§2.3 Retention). Approached only on
/// very-long sessions at the 1 Hz sample rate.
pub const PER_SESSION_CAP_BYTES: u64 = 50 * 1024 * 1024;
/// Total cap across all sessions; startup cleanup rotates oldest
/// first.
pub const TOTAL_CAP_BYTES: u64 = 1024 * 1024 * 1024;

/// Sampling rate the drain worker should use, derived from how full
/// the current session file is (§2.3: downsample, never truncate an
/// in-progress session).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleRate {
    /// < 80% of the per-session cap — normal 1 Hz.
    Full1Hz,
    /// ≥ 80% — 0.5 Hz.
    Half,
    /// ≥ 95% — 0.1 Hz.
    Tenth,
    /// ≥ 100% — stop writing samples; keep rare `framesage_action` +
    /// `kernel_signal` events.
    ActionsOnly,
}

/// §2.3 retention thresholds → sampling rate.
pub fn sample_rate_for_bytes(written_bytes: u64) -> SampleRate {
    let pct = written_bytes as f64 / PER_SESSION_CAP_BYTES as f64;
    if pct >= 1.0 {
        SampleRate::ActionsOnly
    } else if pct >= 0.95 {
        SampleRate::Tenth
    } else if pct >= 0.80 {
        SampleRate::Half
    } else {
        SampleRate::Full1Hz
    }
}

/// True for the event kinds that keep flowing even at 100% of the
/// per-session cap.
fn is_always_written(event: &SessionEvent) -> bool {
    matches!(
        event,
        SessionEvent::FramesageAction { .. }
            | SessionEvent::KernelSignal { .. }
            | SessionEvent::SessionStart { .. }
            | SessionEvent::SessionEnd { .. }
    )
}

/// Append-only writer for one session's `.jsonl` file.
pub struct SessionWriter {
    path: PathBuf,
    file: File,
    written_bytes: u64,
}

impl SessionWriter {
    /// Create `<dir>/<session_id>.jsonl` and write the `session_start`
    /// line. `start` must be a [`SessionEvent::SessionStart`].
    pub fn create(dir: &Path, session_id: &str, start: &SessionEvent) -> Result<Self> {
        anyhow::ensure!(
            matches!(start, SessionEvent::SessionStart { .. }),
            "first event must be session_start"
        );
        std::fs::create_dir_all(dir)
            .with_context(|| format!("create sessions dir {}", dir.display()))?;
        let path = dir.join(format!("{session_id}.jsonl"));
        let file = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("create session file {}", path.display()))?;
        let mut writer = Self {
            path,
            file,
            written_bytes: 0,
        };
        writer.write_line(start)?;
        Ok(writer)
    }

    /// Current §2.3 sampling rate given how full this file is. The
    /// drain worker consults this before producing samples.
    pub fn sample_rate(&self) -> SampleRate {
        sample_rate_for_bytes(self.written_bytes)
    }

    /// Append one event. At 100% of the per-session cap, sample-class
    /// events are dropped (returns Ok(false)); action/signal/end
    /// events are always written (§2.3: no truncation of in-progress
    /// sessions — downsample only, and the always-written kinds are
    /// rare so the total stays bounded).
    pub fn append(&mut self, event: &SessionEvent) -> Result<bool> {
        if self.sample_rate() == SampleRate::ActionsOnly && !is_always_written(event) {
            return Ok(false);
        }
        self.write_line(event)?;
        Ok(true)
    }

    fn write_line(&mut self, event: &SessionEvent) -> Result<()> {
        let mut line = serde_json::to_vec(event).context("serialize session event")?;
        line.push(b'\n');
        self.file
            .write_all(&line)
            .with_context(|| format!("append to {}", self.path.display()))?;
        self.written_bytes += line.len() as u64;
        Ok(())
    }

    /// Write the `session_end` line and flush. Consumes the writer —
    /// the file is immutable afterwards.
    pub fn finish(mut self, end: &SessionEvent) -> Result<()> {
        anyhow::ensure!(
            matches!(end, SessionEvent::SessionEnd { .. }),
            "last event must be session_end"
        );
        self.write_line(end)?;
        self.file.flush().context("flush session file")?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn written_bytes(&self) -> u64 {
        self.written_bytes
    }
}

/// Read a session file back into events. Unknown `kind`s and
/// malformed lines are skipped with a count (forward compatibility
/// within schema v1; a truncated final line from a crash mid-write
/// must not poison the whole session).
pub fn read_session(path: &Path) -> Result<(Vec<SessionEvent>, usize)> {
    let file = File::open(path).with_context(|| format!("open session {}", path.display()))?;
    let mut events = Vec::new();
    let mut skipped = 0usize;
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<SessionEvent>(&line) {
            Ok(ev) => events.push(ev),
            Err(_) => skipped += 1,
        }
    }
    Ok((events, skipped))
}

/// One row for the Sessions-tab list view (§2.4). Derived from the
/// first and last lines only — cheap enough to build for every stored
/// session.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionListEntry {
    pub session_id: String,
    pub game_exe: String,
    pub profile_id: String,
    pub started_at_unix_secs: u64,
    pub duration_secs: Option<u64>,
    pub partial_data: bool,
    pub file_bytes: u64,
}

/// Resolve `<dir>/<session_id>.jsonl`, refusing ids that could
/// escape the sessions directory. Session ids are UUID hex strings
/// with dashes (§2.3); anything else — path separators, dots, drive
/// letters — is rejected at this trust boundary. The IPC handler
/// calls this with a client-supplied id.
pub fn session_file_path(dir: &Path, session_id: &str) -> Result<PathBuf> {
    let valid = !session_id.is_empty()
        && session_id.len() <= 64
        && session_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    anyhow::ensure!(valid, "invalid session id");
    Ok(dir.join(format!("{session_id}.jsonl")))
}

/// Enumerate `<dir>/*.jsonl` into list entries, newest first.
pub fn list_sessions(dir: &Path) -> Result<Vec<SessionListEntry>> {
    let mut entries = Vec::new();
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(entries),
        Err(e) => return Err(e).with_context(|| format!("read sessions dir {}", dir.display())),
    };
    for dirent in read_dir {
        let path = dirent?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let file_bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let (events, _skipped) = match read_session(&path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let Some(SessionEvent::SessionStart {
            session_id,
            game_exe,
            profile_id,
            started_at_unix_secs,
            ..
        }) = events.first()
        else {
            continue;
        };
        let (duration_secs, partial_data) = match events.last() {
            Some(SessionEvent::SessionEnd {
                partial_data,
                summary,
                ..
            }) => (Some(summary.duration_secs), *partial_data),
            // No session_end: crashed / in-progress session. Surface
            // it as partial rather than hiding it.
            _ => (None, true),
        };
        entries.push(SessionListEntry {
            session_id: session_id.clone(),
            game_exe: game_exe.clone(),
            profile_id: profile_id.clone(),
            started_at_unix_secs: *started_at_unix_secs,
            duration_secs,
            partial_data,
            file_bytes,
        });
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.started_at_unix_secs));
    Ok(entries)
}

/// §2.3 total-cap cleanup: while the directory exceeds
/// `total_cap_bytes`, delete the oldest session file. Returns the
/// deleted paths. Run at engine startup.
pub fn enforce_total_cap(dir: &Path, total_cap_bytes: u64) -> Result<Vec<PathBuf>> {
    let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
    let read_dir = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("read sessions dir {}", dir.display())),
    };
    for dirent in read_dir {
        let path = dirent?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let meta = std::fs::metadata(&path)?;
        let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        files.push((path, meta.len(), modified));
    }
    // Oldest first.
    files.sort_by_key(|(_, _, modified)| *modified);
    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    let mut deleted = Vec::new();
    for (path, len, _) in files {
        if total <= total_cap_bytes {
            break;
        }
        std::fs::remove_file(&path)
            .with_context(|| format!("rotate old session {}", path.display()))?;
        tracing::info!(path = %path.display(), bytes = len, "rotated old session (total cap)");
        total = total.saturating_sub(len);
        deleted.push(path);
    }
    Ok(deleted)
}

/// Convenience: a minimal `session_start` for tests and the sim
/// harness.
pub fn synthetic_session_start(session_id: &str, game_exe: &str, profile_id: &str) -> SessionEvent {
    SessionEvent::SessionStart {
        schema_version: SCHEMA_VERSION,
        at_ms: 0,
        session_id: session_id.to_string(),
        started_at_unix_secs: 0,
        game_exe: game_exe.to_string(),
        game_pid: 0,
        profile_id: profile_id.to_string(),
        matched_rule_index: None,
        system: crate::schema::SystemInfo {
            os_build: 0,
            cpu_brand: String::new(),
            logical_cpus: 0,
            topology_ccds: 0,
            memory_total_bytes: 0,
        },
        etw_state: "unavailable".into(),
        presentmon_state: "disabled".into(),
        opcode_table: "unknown".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{SessionSummary, SCHEMA_VERSION};

    fn end_event(at_ms: u64, partial: bool) -> SessionEvent {
        SessionEvent::SessionEnd {
            schema_version: SCHEMA_VERSION,
            at_ms,
            reason: "test".into(),
            partial_data: partial,
            etw_drops_total: 0,
            presentmon_restarts: 0,
            summary: SessionSummary {
                duration_secs: at_ms / 1000,
                frame_time_p50_us_baseline: None,
                frame_time_p50_us_with_rules: None,
                frame_time_p99_us_baseline: None,
                frame_time_p99_us_with_rules: None,
                actions_applied: 0,
                kernel_signals: 0,
            },
        }
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let start = synthetic_session_start("s1", "Attila.exe", "game-x3d");
        let mut w = SessionWriter::create(dir.path(), "s1", &start).unwrap();
        w.append(&SessionEvent::FrameSample {
            schema_version: SCHEMA_VERSION,
            at_ms: 1000,
            frame_count: 60,
            frame_time_us_p50: 16_000,
            frame_time_us_p99: 22_000,
            frames_dropped: 0,
        })
        .unwrap();
        let path = w.path().to_path_buf();
        w.finish(&end_event(2000, false)).unwrap();

        let (events, skipped) = read_session(&path).unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], SessionEvent::SessionStart { .. }));
        assert!(matches!(events[2], SessionEvent::SessionEnd { .. }));
    }

    #[test]
    fn reader_skips_malformed_lines_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let start = synthetic_session_start("s2", "a.exe", "p");
        let w = SessionWriter::create(dir.path(), "s2", &start).unwrap();
        let path = w.path().to_path_buf();
        drop(w);
        // Simulate a crash mid-write: torn final line.
        use std::io::Write as _;
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{{\"schema_version\":1,\"kind\":\"frame_sa").unwrap();

        let (events, skipped) = read_session(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(skipped, 1);
    }

    // §2.3 / PRE-L-002 — retention boundary behavior at 80/95/100%.
    #[test]
    fn retention_thresholds_map_to_sample_rates() {
        let cap = PER_SESSION_CAP_BYTES;
        assert_eq!(sample_rate_for_bytes(0), SampleRate::Full1Hz);
        assert_eq!(sample_rate_for_bytes(cap * 79 / 100), SampleRate::Full1Hz);
        assert_eq!(sample_rate_for_bytes(cap * 80 / 100), SampleRate::Half);
        assert_eq!(sample_rate_for_bytes(cap * 94 / 100), SampleRate::Half);
        assert_eq!(sample_rate_for_bytes(cap * 95 / 100), SampleRate::Tenth);
        assert_eq!(sample_rate_for_bytes(cap), SampleRate::ActionsOnly);
    }

    #[test]
    fn at_full_cap_samples_drop_but_actions_still_write() {
        let dir = tempfile::tempdir().unwrap();
        let start = synthetic_session_start("s3", "a.exe", "p");
        let mut w = SessionWriter::create(dir.path(), "s3", &start).unwrap();
        // Force the cap without writing 50 MB.
        w.written_bytes = PER_SESSION_CAP_BYTES;

        let wrote_sample = w
            .append(&SessionEvent::FrameSample {
                schema_version: SCHEMA_VERSION,
                at_ms: 1000,
                frame_count: 60,
                frame_time_us_p50: 1,
                frame_time_us_p99: 1,
                frames_dropped: 0,
            })
            .unwrap();
        assert!(!wrote_sample, "samples must be dropped at 100% cap");

        let wrote_action = w
            .append(&SessionEvent::FramesageAction {
                schema_version: SCHEMA_VERSION,
                at_ms: 1000,
                action: "apply_profile".into(),
                profile_id: "p".into(),
                details: None,
            })
            .unwrap();
        assert!(wrote_action, "actions keep flowing at 100% cap");
    }

    #[test]
    fn list_sessions_orders_newest_first_and_flags_missing_end() {
        let dir = tempfile::tempdir().unwrap();
        for (id, started, finish) in [("old", 100u64, true), ("new", 200, false)] {
            let mut start = synthetic_session_start(id, "a.exe", "p");
            if let SessionEvent::SessionStart {
                started_at_unix_secs,
                ..
            } = &mut start
            {
                *started_at_unix_secs = started;
            }
            let w = SessionWriter::create(dir.path(), id, &start).unwrap();
            if finish {
                w.finish(&end_event(5000, false)).unwrap();
            }
        }
        let list = list_sessions(dir.path()).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].session_id, "new");
        assert!(
            list[0].partial_data,
            "session without session_end surfaces as partial"
        );
        assert_eq!(list[1].session_id, "old");
        assert!(!list[1].partial_data);
        assert_eq!(list[1].duration_secs, Some(5));
    }

    #[test]
    fn total_cap_rotates_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        for id in ["a", "b", "c"] {
            let start = synthetic_session_start(id, "x.exe", "p");
            let w = SessionWriter::create(dir.path(), id, &start).unwrap();
            w.finish(&end_event(1000, false)).unwrap();
            // Distinct mtimes so rotation order is deterministic.
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let sizes: u64 = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        // Cap below total but above two files → exactly one (the
        // oldest) is rotated out.
        let cap = sizes - 1;
        let deleted = enforce_total_cap(dir.path(), cap).unwrap();
        assert_eq!(deleted.len(), 1);
        assert!(deleted[0].to_string_lossy().contains("a.jsonl"));
        let remaining = list_sessions(dir.path()).unwrap();
        assert_eq!(remaining.len(), 2);
    }
}

#[cfg(test)]
mod session_id_tests {
    use super::*;

    #[test]
    fn session_file_path_rejects_traversal_and_junk() {
        let dir = Path::new("/tmp/sessions");
        assert!(session_file_path(dir, "f47ac10b-58cc-4372-a567-0e02b2c3d479").is_ok());
        for bad in [
            "../etc/passwd",
            "..",
            "a/b",
            "a\\b",
            "",
            "c:evil",
            "x.jsonl",
            &"a".repeat(65),
        ] {
            assert!(session_file_path(dir, bad).is_err(), "must reject {bad:?}");
        }
    }
}
