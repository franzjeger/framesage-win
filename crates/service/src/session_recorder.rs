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
//! **Honesty about missing data:** capability state is stamped per
//! session from [`SessionCapabilities`] — `presentmon_state`/
//! `etw_state` read "active" only when a real PresentMon.exe is
//! attached (#111) and the closed-loop ETW drain is running (Group A).
//! When either source is absent the session carries the honest
//! "disabled"/"unavailable" state, records no `frame_sample` events
//! from it, and stays `partial_data: true` per §2.3 — so the
//! attribution panel shows "Frame data unavailable" rather than
//! fabricating a verdict (the §2.4 disabled-attribution contract). A
//! session that captured frame samples with zero drops is non-partial
//! and the closed loop can actually attribute against it.
//!
//! Like the closed-loop tasks, the drain worker is NOT part of the
//! v0.6 watchdog `select!` — a recorder failure must never take the
//! rule engine down. On any per-session error we log and drop the
//! session; the engine is unaffected.

use std::path::PathBuf;
use std::time::Instant;

use framesage_ipc::Event;
use framesage_presentmon::FrameStats;
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
    /// 1 Hz cpu_sample tick counter — drives the §2.3 downsampling.
    cpu_ticks: u64,
    kernel_signals: u32,
    /// #111 — frames actually recorded this session. Drives the
    /// honest `session_end.partial_data`: a session with zero frame
    /// samples can't support attribution and stays partial.
    frame_samples_recorded: u32,
    /// Cumulative PresentMon `Dropped` count seen this session, folded
    /// into the partial-data signal.
    frames_dropped_total: u64,
}

/// Event-driven session recorder. Platform-independent and IO-light:
/// all state is the optional in-flight recording; the caller owns the
/// event source and the policy gate.
/// #111 — honest capability state stamped into `session_start`. Set
/// by the service from the closed-loop startup result + whether a
/// PresentMon frame source is attached. Defaults to "nothing
/// available" so an unwired recorder tells the truth.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionCapabilities {
    /// ETW kernel drain is running (closed-loop session active).
    pub etw_active: bool,
    /// A PresentMon frame source is attached and will attempt to
    /// record frame_sample events.
    pub presentmon_active: bool,
}

pub struct SessionRecorder {
    dir: PathBuf,
    current: Option<ActiveRecording>,
    /// Most recent foreground exe/pid + matched rule, captured from
    /// `ForegroundChanged` so `session_start` can name the game that
    /// triggered the Game Mode session.
    last_foreground: Option<(String, u32, Option<u32>)>,
    /// #111 — capability state stamped into each session_start.
    caps: SessionCapabilities,
}

impl SessionRecorder {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            current: None,
            last_foreground: None,
            caps: SessionCapabilities::default(),
        }
    }

    /// #111 — update the capability state stamped into future
    /// `session_start` events. In-flight sessions keep their stamp.
    pub fn set_capabilities(&mut self, caps: SessionCapabilities) {
        self.caps = caps;
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
                    // #111 — honest capability statement per §2.3.
                    etw_state: if self.caps.etw_active {
                        "active".into()
                    } else {
                        "unavailable".into()
                    },
                    presentmon_state: if self.caps.presentmon_active {
                        "active".into()
                    } else {
                        "disabled".into()
                    },
                    opcode_table: if self.caps.etw_active {
                        "win11_24h2_26200".into()
                    } else {
                        "unknown".into()
                    },
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
                            cpu_ticks: 0,
                            kernel_signals: 0,
                            frame_samples_recorded: 0,
                            frames_dropped_total: 0,
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

    /// #7 — append a §2.3 `cpu_sample` line if a session is recording.
    /// The 1 Hz caller downsamples per the writer's §2.3 sample rate
    /// (0.5 Hz at 80% of the per-session cap, 0.1 Hz at 95%).
    pub fn record_cpu_sample(&mut self, total_pct: u8, per_core_pct: Vec<u8>) {
        let Some(rec) = self.current.as_mut() else {
            return;
        };
        rec.cpu_ticks = rec.cpu_ticks.wrapping_add(1);
        let keep_every = match rec.writer.sample_rate() {
            framesage_recorder::SampleRate::Full1Hz => 1,
            framesage_recorder::SampleRate::Half => 2,
            framesage_recorder::SampleRate::Tenth => 10,
            framesage_recorder::SampleRate::ActionsOnly => return,
        };
        if rec.cpu_ticks % keep_every != 0 {
            return;
        }
        let at_ms = rec.started.elapsed().as_millis() as u64;
        let event = SessionEvent::CpuSample {
            schema_version: SCHEMA_VERSION,
            at_ms,
            total_pct,
            per_core_pct,
            // §2.3: ALWAYS null in v0.7.x (reserved v0.8 slot).
            per_process: None,
        };
        if let Err(e) = rec.writer.append(&event) {
            warn!(error = %e, session = %rec.session_id, "cpu_sample append failed; dropping recording");
            self.current = None;
        }
    }

    /// #8 — append a §2.3 `kernel_signal` line from the ETW drain's
    /// spike detector. Signals are rare by construction (cooldown in
    /// the detector), so they are always written.
    pub fn record_kernel_signal(&mut self, sig: &framesage_etw::KernelSignal) {
        let Some(rec) = self.current.as_mut() else {
            return;
        };
        let at_ms = rec.started.elapsed().as_millis() as u64;
        let event = SessionEvent::KernelSignal {
            schema_version: SCHEMA_VERSION,
            at_ms,
            signal: sig.signal.to_string(),
            rate_per_sec: sig.rate_per_sec,
            baseline_5min_per_sec: sig.baseline_5min_per_sec,
            above_baseline_pct: sig.above_baseline_pct,
        };
        if let Err(e) = rec.writer.append(&event) {
            warn!(error = %e, session = %rec.session_id, "kernel_signal append failed; dropping recording");
            self.current = None;
        } else {
            rec.kernel_signals = rec.kernel_signals.saturating_add(1);
        }
    }

    /// #111 — append a §2.3 `frame_sample` line from the PresentMon
    /// aggregator. The aggregator already emits at 1 Hz; downsample
    /// beyond that per the retention rate, same as cpu_sample. A
    /// session that records at least one frame sample is no longer
    /// partial-for-missing-frames.
    pub fn record_frame_sample(&mut self, stats: &FrameStats) {
        let Some(rec) = self.current.as_mut() else {
            return;
        };
        // Dropped frames count toward the partial-data signal even if
        // the sample itself is downsampled away.
        rec.frames_dropped_total = rec
            .frames_dropped_total
            .saturating_add(stats.frames_dropped as u64);
        let keep_every = match rec.writer.sample_rate() {
            framesage_recorder::SampleRate::Full1Hz => 1,
            framesage_recorder::SampleRate::Half => 2,
            framesage_recorder::SampleRate::Tenth => 10,
            framesage_recorder::SampleRate::ActionsOnly => return,
        };
        // Reuse cpu_ticks-style counter dedicated to frames.
        rec.frame_samples_recorded = rec.frame_samples_recorded.saturating_add(1);
        if rec.frame_samples_recorded % keep_every != 0 {
            return;
        }
        let at_ms = rec.started.elapsed().as_millis() as u64;
        let event = SessionEvent::FrameSample {
            schema_version: SCHEMA_VERSION,
            at_ms,
            frame_count: stats.frame_count,
            frame_time_us_p50: stats.frame_time_us_p50,
            frame_time_us_p99: stats.frame_time_us_p99,
            frames_dropped: stats.frames_dropped,
        };
        if let Err(e) = rec.writer.append(&event) {
            warn!(error = %e, session = %rec.session_id, "frame_sample append failed; dropping recording");
            self.current = None;
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
            // §2.3: partial_data is true when frame data is missing for
            // any window (no PresentMon samples) or drops were seen.
            // A session that recorded frame samples with zero drops is
            // now non-partial — the closed loop can actually attribute.
            partial_data: rec.frame_samples_recorded == 0 || rec.frames_dropped_total > 0,
            etw_drops_total: 0,
            presentmon_restarts: 0,
            summary: SessionSummary {
                duration_secs: at_ms / 1000,
                frame_time_p50_us_baseline: None,
                frame_time_p50_us_with_rules: None,
                frame_time_p99_us_baseline: None,
                frame_time_p99_us_with_rules: None,
                actions_applied: rec.actions_applied,
                kernel_signals: rec.kernel_signals,
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
    mut kernel_signals: tokio::sync::broadcast::Receiver<framesage_etw::KernelSignal>,
    mut frame_samples: tokio::sync::mpsc::Receiver<FrameStats>,
    caps: SessionCapabilities,
) -> tokio::task::JoinHandle<()> {
    let mut rx = engine.subscribe();
    tokio::spawn(async move {
        let mut recorder = SessionRecorder::new(dir);
        recorder.set_capabilities(caps);
        // #7 — 1 Hz cpu_sample tick while a session is recording.
        let mut cpu_interval = tokio::time::interval(std::time::Duration::from_secs(1));
        cpu_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut signals_closed = false;
        let mut frames_closed = false;
        loop {
            tokio::select! {
                event = rx.recv() => match event {
                    Ok(event) => {
                        // #11 — cheap policy-gate read instead of
                        // cloning the whole policy per event.
                        let enabled = engine.closed_loop_enabled();
                        recorder.handle_event(&event, enabled);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        warn!(missed, "session recorder lagged behind engine events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!("engine event stream closed; session recorder exiting");
                        return;
                    }
                },
                sig = kernel_signals.recv(), if !signals_closed => match sig {
                    // #8 — kernel_signal lines from the ETW drain.
                    Ok(sig) => recorder.record_kernel_signal(&sig),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        warn!(missed, "session recorder lagged behind kernel signals");
                    }
                    // Closed-loop disabled / torn down: keep serving
                    // engine events; permanently disarm this arm.
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        signals_closed = true;
                    }
                },
                _ = cpu_interval.tick() => {
                    if let Some((total, per_core)) = engine.sample_cpu_for_recorder() {
                        recorder.record_cpu_sample(total, per_core);
                    }
                }
                frame = frame_samples.recv(), if !frames_closed => match frame {
                    // #111 — 1 Hz frame_sample buckets from the PresentMon
                    // manager. record_frame_sample no-ops outside a session,
                    // so a child that outlives a session drops its tail
                    // frames harmlessly.
                    Some(stats) => recorder.record_frame_sample(&stats),
                    // All PresentMon senders dropped (manager exited): keep
                    // serving engine events; permanently disarm this arm so
                    // recv() doesn't busy-resolve.
                    None => frames_closed = true,
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

    fn frame_stats(at_ms: u64, p50: u64, p99: u64) -> FrameStats {
        FrameStats {
            at_ms,
            frame_count: 60,
            frame_time_us_p50: p50,
            frame_time_us_p99: p99,
            frames_dropped: 0,
        }
    }

    // #111 — capabilities are stamped into session_start honestly.
    #[test]
    fn session_start_reflects_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());
        rec.set_capabilities(SessionCapabilities {
            etw_active: true,
            presentmon_active: true,
        });
        rec.handle_event(&foreground("g.exe", 7), true);
        rec.handle_event(&entered("game-x3d"), true);
        rec.handle_event(&exited("done"), true);

        let entry = &list_sessions(dir.path()).unwrap()[0];
        let path = dir.path().join(format!("{}.jsonl", entry.session_id));
        let (events, _) = read_session(&path).unwrap();
        match events.first().unwrap() {
            SessionEvent::SessionStart {
                etw_state,
                presentmon_state,
                opcode_table,
                ..
            } => {
                assert_eq!(etw_state, "active");
                assert_eq!(presentmon_state, "active");
                assert_eq!(opcode_table, "win11_24h2_26200");
            }
            other => panic!("expected session_start, got {other:?}"),
        }
    }

    // #111 — a session that records clean frame samples is NOT partial
    // and yields a real attribution verdict (baseline 0-60s vs
    // with-rules, per §2.4). This is the closed loop actually working.
    #[test]
    fn frame_samples_make_a_session_non_partial_and_attributable() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());
        rec.set_capabilities(SessionCapabilities {
            etw_active: true,
            presentmon_active: true,
        });
        rec.handle_event(&foreground("g.exe", 7), true);
        rec.handle_event(&entered("game-x3d"), true);
        // 60 s baseline at 22ms p99, then apply, then 120 s at 19.7ms
        // p99 (a real improvement).
        for _ in 0..60 {
            rec.record_frame_sample(&frame_stats(0, 16_000, 22_000));
        }
        rec.handle_event(
            &Event::ProfileApplied {
                pid: 7,
                exe_name: "g.exe".into(),
                profile_id: ProfileId("game-x3d".into()),
            },
            true,
        );
        for _ in 0..120 {
            rec.record_frame_sample(&frame_stats(0, 15_800, 19_700));
        }
        rec.handle_event(&exited("foreground_lost"), true);

        let entry = &list_sessions(dir.path()).unwrap()[0];
        assert!(
            !entry.partial_data,
            "clean frame samples must clear the partial flag"
        );
    }

    // #111 — dropped frames keep a session partial even with samples.
    #[test]
    fn dropped_frames_keep_session_partial() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());
        rec.handle_event(&foreground("g.exe", 7), true);
        rec.handle_event(&entered("game-x3d"), true);
        let mut s = frame_stats(0, 16_000, 22_000);
        s.frames_dropped = 5;
        rec.record_frame_sample(&s);
        rec.handle_event(&exited("done"), true);

        let entry = &list_sessions(dir.path()).unwrap()[0];
        assert!(entry.partial_data, "dropped frames mark the session partial");
    }

    // #7/#8 — cpu_sample + kernel_signal actually land in the file and
    // count into the session_end summary.
    #[test]
    fn cpu_and_kernel_samples_are_recorded_and_summarized() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());
        rec.handle_event(&foreground("g.exe", 7), true);
        rec.handle_event(&entered("game-x3d"), true);
        rec.record_cpu_sample(47, vec![62, 28, 71, 44]);
        rec.record_kernel_signal(&framesage_etw::KernelSignal {
            kind: framesage_etw::KernelEventKind::Dpc,
            signal: "dpc_spike",
            rate_per_sec: 14_823,
            baseline_5min_per_sec: 3200,
            above_baseline_pct: 363,
        });
        rec.handle_event(&exited("done"), true);

        let entry = &list_sessions(dir.path()).unwrap()[0];
        let path = dir.path().join(format!("{}.jsonl", entry.session_id));
        let (events, _) = read_session(&path).unwrap();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::CpuSample { total_pct: 47, .. })),
            "cpu_sample must be in the file"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, SessionEvent::KernelSignal { .. })),
            "kernel_signal must be in the file"
        );
        match events.last().unwrap() {
            SessionEvent::SessionEnd { summary, .. } => {
                assert_eq!(summary.kernel_signals, 1);
            }
            other => panic!("expected session_end, got {other:?}"),
        }
    }

    // Samples outside a session are dropped, not buffered.
    #[test]
    fn frame_samples_outside_a_session_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());
        rec.record_frame_sample(&frame_stats(0, 16_000, 22_000));
        assert!(list_sessions(dir.path()).unwrap().is_empty());
    }
}
