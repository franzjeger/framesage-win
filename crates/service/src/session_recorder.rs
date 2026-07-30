//! #110 drain worker — records engine activity into on-disk session
//! files (`framesage-recorder` jsonl, architecture §2.3).
//!
//! Session lifecycle in this slice:
//!
//! * `Event::GameModeEntered` opens a session (when the policy has
//!   `closed_loop_enabled = true` — recording is strictly opt-in per
//!   the §2.4 "Once enabled, just play" contract).
//! * Engine events during the session become `framesage_action`
//!   lines (profile apply/revert, ProBalance restrain/restore).
//! * `Event::GameModeExited` writes `session_end` and closes the
//!   file, then re-enforces the 1 GB total cap.
//!
//! **Honesty about missing data:** until #111 (PresentMon) and the
//! Group A ETW drain land, sessions carry `presentmon_state:
//! "disabled"` / `etw_state: "unavailable"`, contain no
//! `frame_sample` events, and are marked `partial_data: true` per
//! §2.3. The attribution panel therefore shows "Frame data
//! unavailable" rather than fabricating a verdict — exactly the
//! §2.4 disabled-attribution contract.
//!
//! Like the closed-loop tasks, the drain worker is NOT part of the
//! v0.6 watchdog `select!` — a recorder failure must never take the
//! rule engine down. On any per-session error we log and drop the
//! session; the engine is unaffected.

use std::path::PathBuf;
use std::time::Instant;

use framesage_ipc::Event;
use framesage_recorder::{
    schema::SystemInfo, SessionEvent, SessionSummary, SessionWriter, SCHEMA_VERSION,
    TOTAL_CAP_BYTES,
};
use tracing::{info, warn};

/// One in-flight recording.
struct ActiveRecording {
    writer: SessionWriter,
    session_id: String,
    started: Instant,
    actions_applied: u32,
}

/// Event-driven session recorder. Platform-independent and IO-light:
/// all state is the optional in-flight recording; the caller owns the
/// event source and the policy gate.
pub struct SessionRecorder {
    dir: PathBuf,
    current: Option<ActiveRecording>,
    /// Most recent foreground exe/pid + matched rule, captured from
    /// `ForegroundChanged` so `session_start` can name the game that
    /// triggered the Game Mode session.
    last_foreground: Option<(String, u32, Option<u32>)>,
}

impl SessionRecorder {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            current: None,
            last_foreground: None,
        }
    }

    /// Test-only probe; production callers only feed events.
    #[cfg(test)]
    pub fn is_recording(&self) -> bool {
        self.current.is_some()
    }

    /// Feed one engine event. `closed_loop_enabled` is the policy gate
    /// sampled by the caller at event time; it only gates session
    /// *start* — an in-flight session always runs to its end so the
    /// file stays well-formed.
    pub fn handle_event(&mut self, event: &Event, closed_loop_enabled: bool) {
        match event {
            Event::ForegroundChanged {
                foreground,
                matched_rule_index,
                ..
            } => {
                self.last_foreground = Some((
                    foreground.exe_name.clone(),
                    foreground.pid,
                    matched_rule_index.map(|i| i as u32),
                ));
            }
            Event::GameModeEntered {
                profile_id,
                services_to_stop,
                processes_to_suspend,
                power_plan_changing,
                taskbar_hiding,
                pausing_windows_update,
            } => {
                if self.current.is_some() {
                    // Engine contract is one system-mode session at a
                    // time; a second enter without an exit means we
                    // missed the exit event. Close what we have as
                    // partial rather than interleaving two sessions.
                    warn!("GameModeEntered with a recording in flight; closing previous session");
                    self.finish_current("superseded");
                }
                if !closed_loop_enabled {
                    return;
                }
                let session_id = uuid::Uuid::new_v4().to_string();
                let (game_exe, game_pid, matched_rule_index) = self
                    .last_foreground
                    .clone()
                    .unwrap_or_else(|| ("<unknown>".into(), 0, None));
                let start = SessionEvent::SessionStart {
                    schema_version: SCHEMA_VERSION,
                    at_ms: 0,
                    session_id: session_id.clone(),
                    started_at_unix_secs: unix_now_secs(),
                    game_exe,
                    game_pid,
                    profile_id: profile_id.0.clone(),
                    matched_rule_index,
                    system: host_system_info(),
                    // Honest capability statement for this slice: no
                    // ETW drain, no PresentMon child yet (#111 /
                    // Group A follow-ups flip these).
                    etw_state: "unavailable".into(),
                    presentmon_state: "disabled".into(),
                    opcode_table: "unknown".into(),
                };
                match SessionWriter::create(&self.dir, &session_id, &start) {
                    Ok(mut writer) => {
                        let details = serde_json::json!({
                            "services_to_stop": services_to_stop,
                            "processes_to_suspend": processes_to_suspend,
                            "power_plan_target": power_plan_changing,
                            "taskbar_hiding": taskbar_hiding,
                            "windows_update_pausing": pausing_windows_update,
                        });
                        let _ = writer.append(&SessionEvent::FramesageAction {
                            schema_version: SCHEMA_VERSION,
                            at_ms: 0,
                            action: "game_mode_entered".into(),
                            profile_id: profile_id.0.clone(),
                            details: Some(details),
                        });
                        info!(session = %session_id, profile = %profile_id, "session recording started");
                        self.current = Some(ActiveRecording {
                            writer,
                            session_id,
                            started: Instant::now(),
                            actions_applied: 1,
                        });
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to open session recording; session not recorded");
                    }
                }
            }
            Event::GameModeExited { reason, .. } => {
                let reason = reason.clone();
                self.finish_current(&reason);
            }
            Event::ProfileApplied {
                pid,
                exe_name,
                profile_id,
            } => {
                self.record_action(
                    "apply_profile",
                    &profile_id.0,
                    serde_json::json!({"pid": pid, "exe_name": exe_name}),
                );
            }
            Event::ProfileReverted {
                pid,
                exe_name,
                profile_id,
            } => {
                self.record_action(
                    "revert_profile",
                    &profile_id.0,
                    serde_json::json!({"pid": pid, "exe_name": exe_name}),
                );
            }
            Event::ProBalanceRestrained { pid, exe_name, .. } => {
                self.record_action(
                    "probalance_restrained",
                    "",
                    serde_json::json!({"pid": pid, "exe_name": exe_name}),
                );
            }
            Event::ProBalanceRestored { pid, exe_name, .. } => {
                self.record_action(
                    "probalance_restored",
                    "",
                    serde_json::json!({"pid": pid, "exe_name": exe_name}),
                );
            }
            // Everything else is not session-relevant (yet).
            _ => {}
        }
    }

    fn record_action(&mut self, action: &str, profile_id: &str, details: serde_json::Value) {
        let Some(rec) = self.current.as_mut() else {
            return;
        };
        let at_ms = rec.started.elapsed().as_millis() as u64;
        let event = SessionEvent::FramesageAction {
            schema_version: SCHEMA_VERSION,
            at_ms,
            action: action.to_string(),
            profile_id: profile_id.to_string(),
            details: Some(details),
        };
        if let Err(e) = rec.writer.append(&event) {
            warn!(error = %e, session = %rec.session_id, "session append failed; dropping recording");
            self.current = None;
        } else {
            rec.actions_applied += 1;
        }
    }

    fn finish_current(&mut self, reason: &str) {
        let Some(rec) = self.current.take() else {
            return;
        };
        let at_ms = rec.started.elapsed().as_millis() as u64;
        let end = SessionEvent::SessionEnd {
            schema_version: SCHEMA_VERSION,
            at_ms,
            reason: reason.to_string(),
            // §2.3: partial_data is true when PresentMon was
            // unavailable for any window — which in this slice is the
            // whole session.
            partial_data: true,
            etw_drops_total: 0,
            presentmon_restarts: 0,
            summary: SessionSummary {
                duration_secs: at_ms / 1000,
                frame_time_p50_us_baseline: None,
                frame_time_p50_us_with_rules: None,
                frame_time_p99_us_baseline: None,
                frame_time_p99_us_with_rules: None,
                actions_applied: rec.actions_applied,
                kernel_signals: 0,
            },
        };
        let session_id = rec.session_id.clone();
        if let Err(e) = rec.writer.finish(&end) {
            warn!(error = %e, session = %session_id, "session finish failed");
        } else {
            info!(session = %session_id, reason, "session recording finished");
        }
        // §2.3 total cap — rotate oldest sessions after each close so
        // the directory stays bounded without a periodic task.
        if let Err(e) = framesage_recorder::enforce_total_cap(&self.dir, TOTAL_CAP_BYTES) {
            warn!(error = %e, "session total-cap enforcement failed");
        }
    }
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn host_system_info() -> SystemInfo {
    SystemInfo {
        // detected_build is cached; None (probe failed / non-Windows)
        // records as 0.
        os_build: framesage_etw::build_gate::detected_build().unwrap_or(0),
        // CPU brand / CCD topology plumbing lands with the Group B
        // integration; empty/zero is honest absence, not a claim.
        cpu_brand: String::new(),
        logical_cpus: std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(0),
        topology_ccds: 0,
        memory_total_bytes: 0,
    }
}

/// Spawn the drain worker: subscribes to the engine's event stream
/// and feeds the recorder. NOT part of the v0.6 watchdog — recorder
/// death must never take the rule engine down (the task logs and
/// exits; sessions simply stop recording until service restart).
pub fn spawn(
    engine: std::sync::Arc<framesage_engine::Engine>,
    dir: PathBuf,
) -> tokio::task::JoinHandle<()> {
    let mut rx = engine.subscribe();
    tokio::spawn(async move {
        let mut recorder = SessionRecorder::new(dir);
        loop {
            match rx.recv().await {
                Ok(event) => {
                    // Sample the policy gate at event time — a toggle
                    // flip applies from the next session start.
                    let enabled = engine.status().policy.closed_loop_enabled;
                    recorder.handle_event(&event, enabled);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                    warn!(missed, "session recorder lagged behind engine events");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    info!("engine event stream closed; session recorder exiting");
                    return;
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use framesage_core::ProfileId;
    use framesage_ipc::ForegroundSnapshot;
    use framesage_recorder::{
        compute_attribution_summary, list_sessions, read_session, Attribution, DisabledReason,
    };

    fn entered(profile: &str) -> Event {
        Event::GameModeEntered {
            profile_id: ProfileId(profile.into()),
            services_to_stop: 9,
            processes_to_suspend: 3,
            power_plan_changing: true,
            taskbar_hiding: true,
            pausing_windows_update: true,
        }
    }

    fn exited(reason: &str) -> Event {
        Event::GameModeExited {
            profile_id: ProfileId("game-x3d".into()),
            services_restored: 9,
            processes_resumed: 3,
            power_plan_restored: true,
            taskbar_restored: true,
            wu_pause_restored: true,
            duration_secs: 1,
            reason: reason.into(),
        }
    }

    fn foreground(exe: &str, pid: u32) -> Event {
        Event::ForegroundChanged {
            foreground: ForegroundSnapshot {
                pid,
                exe_name: exe.into(),
                path: String::new(),
                title: String::new(),
            },
            profile: ProfileId("game-x3d".into()),
            matched_rule_index: Some(4),
        }
    }

    #[test]
    fn full_session_records_and_reads_back_honestly() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());

        rec.handle_event(&foreground("Attila.exe", 1234), true);
        rec.handle_event(&entered("game-x3d"), true);
        assert!(rec.is_recording());
        rec.handle_event(
            &Event::ProfileApplied {
                pid: 1234,
                exe_name: "Attila.exe".into(),
                profile_id: ProfileId("game-x3d".into()),
            },
            true,
        );
        rec.handle_event(&exited("foreground_lost"), true);
        assert!(!rec.is_recording());

        let list = list_sessions(dir.path()).unwrap();
        assert_eq!(list.len(), 1);
        let entry = &list[0];
        assert_eq!(entry.game_exe, "Attila.exe");
        assert_eq!(entry.profile_id, "game-x3d");
        assert!(
            entry.partial_data,
            "no-PresentMon sessions must be marked partial (§2.3)"
        );

        let path = dir.path().join(format!("{}.jsonl", entry.session_id));
        let (events, skipped) = read_session(&path).unwrap();
        assert_eq!(skipped, 0);
        assert!(matches!(
            events.first(),
            Some(SessionEvent::SessionStart { .. })
        ));
        assert!(matches!(
            events.last(),
            Some(SessionEvent::SessionEnd { .. })
        ));

        // The honesty contract holds end-to-end: with no frame data,
        // attribution is disabled with an explicit reason — never a
        // fabricated verdict. (The window checks precede the frame-
        // data check, so a short synthetic session reports too-short;
        // both are disabled states, which is the load-bearing part.)
        match compute_attribution_summary(&events) {
            Attribution::Disabled { reason, .. } => {
                assert!(
                    matches!(
                        reason,
                        DisabledReason::FrameDataUnavailable
                            | DisabledReason::SessionTooShort
                            | DisabledReason::BaselineTooShort
                    ),
                    "unexpected disabled reason: {reason:?}"
                );
            }
            other => panic!("attribution must be disabled without frame data; got {other:?}"),
        }
    }

    #[test]
    fn recording_is_strictly_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());
        rec.handle_event(&entered("game-x3d"), false);
        assert!(!rec.is_recording());
        rec.handle_event(&exited("foreground_lost"), false);
        assert!(list_sessions(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn exit_without_enter_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());
        rec.handle_event(&exited("foreground_lost"), true);
        assert!(list_sessions(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn double_enter_closes_previous_session_as_superseded() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());
        rec.handle_event(&foreground("a.exe", 1), true);
        rec.handle_event(&entered("game-x3d"), true);
        rec.handle_event(&entered("game-x3d"), true);
        rec.handle_event(&exited("foreground_lost"), true);

        let list = list_sessions(dir.path()).unwrap();
        assert_eq!(list.len(), 2, "both sessions closed as complete files");
        for entry in &list {
            let path = dir.path().join(format!("{}.jsonl", entry.session_id));
            let (events, _) = read_session(&path).unwrap();
            assert!(
                matches!(events.last(), Some(SessionEvent::SessionEnd { .. })),
                "every file ends with session_end"
            );
        }
    }

    #[test]
    fn actions_outside_a_session_are_not_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());
        rec.handle_event(
            &Event::ProfileApplied {
                pid: 1,
                exe_name: "a.exe".into(),
                profile_id: ProfileId("perf".into()),
            },
            true,
        );
        assert!(list_sessions(dir.path()).unwrap().is_empty());
    }
}
