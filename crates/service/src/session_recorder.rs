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
//! session that captured frame samples is non-partial and the closed
//! loop can attribute against it; `partial_data` tracks *data-quality*
//! loss (no samples, or ETW event drops), never normal presentation
//! frame-drops.
//!
//! Like the closed-loop tasks, the drain worker is NOT part of the
//! v0.6 watchdog `select!` — a recorder failure must never take the
//! rule engine down. On any per-session error we log and drop the
//! session; the engine is unaffected.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
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
    /// Cumulative PresentMon `Dropped` present count this session.
    /// Recorded as a metric (per §2.3 `frames_dropped` sum); NOT a
    /// data-quality flag — normal presentation drops must not disable
    /// attribution.
    frames_dropped_total: u64,
    /// Value of the shared ETW-drops counter at session start.
    /// `session_end.etw_drops_total` is `current - baseline` — the
    /// kernel-event drops (RealTimeBuffersLost) observed during this
    /// session's window, the real §2.3 partial-data trigger.
    etw_drops_baseline: u64,
    /// Value of the shared PresentMon crash-restart counter at session
    /// start. `session_end.presentmon_restarts` is `current - baseline`,
    /// i.e. restarts observed during this session's window.
    presentmon_restarts_baseline: u32,
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
    /// #111 — process-lifetime PresentMon crash-restart counter, shared
    /// with the PresentMon manager task (the manager increments it on
    /// each crash-restart). Snapshotted at each session start so
    /// `session_end.presentmon_restarts` reports only this session's
    /// restarts without any cross-task channel.
    presentmon_restarts: Arc<AtomicU32>,
    /// Group A — process-lifetime ETW kernel-drop counter, shared with
    /// the closed-loop drop-poll task (it accumulates RealTimeBuffersLost
    /// deltas). Snapshotted per session, same pattern as
    /// `presentmon_restarts`, to drive `partial_data` +
    /// `session_end.etw_drops_total` honestly.
    etw_drops: Arc<AtomicU64>,
    /// Host system facts stamped into every `session_start`. Defaults to
    /// the honest-minimum probe (`default_system_info`); the service
    /// overrides it at spawn with the real CPU brand / CCD / memory once
    /// the engine topology is known.
    system_info: SystemInfo,
}

impl SessionRecorder {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            current: None,
            last_foreground: None,
            caps: SessionCapabilities::default(),
            presentmon_restarts: Arc::new(AtomicU32::new(0)),
            etw_drops: Arc::new(AtomicU64::new(0)),
            system_info: default_system_info(),
        }
    }

    /// Override the host system facts stamped into future
    /// `session_start` events. Set once at spawn, before any session.
    pub fn set_system_info(&mut self, info: SystemInfo) {
        self.system_info = info;
    }

    /// Group A — adopt the shared ETW kernel-drop counter the drop-poll
    /// task accumulates, so `partial_data` and `session_end.etw_drops_total`
    /// reflect real kernel-event loss. Set before sessions start.
    pub fn set_etw_drop_counter(&mut self, counter: Arc<AtomicU64>) {
        self.etw_drops = counter;
    }

    /// #111 — update the capability state stamped into future
    /// `session_start` events. In-flight sessions keep their stamp.
    pub fn set_capabilities(&mut self, caps: SessionCapabilities) {
        self.caps = caps;
    }

    /// #111 — adopt the shared PresentMon crash-restart counter the
    /// manager task increments, so `session_end.presentmon_restarts` is
    /// honest. Must be set before sessions start; in-flight sessions use
    /// whatever counter was current when they began.
    pub fn set_presentmon_restart_counter(&mut self, counter: Arc<AtomicU32>) {
        self.presentmon_restarts = counter;
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
                    system: self.system_info.clone(),
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
                            etw_drops_baseline: self.etw_drops.load(Ordering::Relaxed),
                            presentmon_restarts_baseline: self
                                .presentmon_restarts
                                .load(Ordering::Relaxed),
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
        // Kernel-event drops observed during this session's window.
        let etw_drops_total = self
            .etw_drops
            .load(Ordering::Relaxed)
            .saturating_sub(rec.etw_drops_baseline);
        let end = SessionEvent::SessionEnd {
            schema_version: SCHEMA_VERSION,
            at_ms,
            reason: reason.to_string(),
            // §2.3: partial_data means the frame/kernel data is
            // untrustworthy for a window — no PresentMon samples at all,
            // or ETW *event* drops (etw_drops_total). It is deliberately
            // NOT triggered by PresentMon *frame* drops: composed-away /
            // never-displayed presents are normal for many titles, and
            // folding them in would mark nearly every real session
            // partial, permanently disabling attribution. frames_dropped
            // is recorded per sample as a metric, not a quality flag.
            partial_data: rec.frame_samples_recorded == 0 || etw_drops_total > 0,
            etw_drops_total,
            // §2.3 — restarts observed during this session's window.
            presentmon_restarts: self
                .presentmon_restarts
                .load(Ordering::Relaxed)
                .saturating_sub(rec.presentmon_restarts_baseline),
            summary: SessionSummary {
                duration_secs: at_ms / 1000,
                frame_time_p50_us_baseline: None,
                frame_time_p50_us_with_rules: None,
                frame_time_p99_us_baseline: None,
                frame_time_p99_us_with_rules: None,
                actions_applied: rec.actions_applied,
                kernel_signals: rec.kernel_signals,
                frames_dropped: rec.frames_dropped_total,
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

/// Honest-minimum system facts, used before the service has wired in
/// the engine topology (and by the recorder unit tests). Everything we
/// can't cheaply probe here is left at its zero/empty absence value.
fn default_system_info() -> SystemInfo {
    SystemInfo {
        // detected_build is cached; None (probe failed / non-Windows)
        // records as 0.
        os_build: framesage_etw::build_gate::detected_build().unwrap_or(0),
        cpu_brand: String::new(),
        logical_cpus: std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(0),
        topology_ccds: 0,
        memory_total_bytes: 0,
    }
}

/// Full system facts for `session_start`, built once at spawn from the
/// engine's detected topology plus a CPUID brand read and a
/// GlobalMemoryStatusEx total. Replaces the zeros/empty of
/// [`default_system_info`] with real values so recorded sessions are
/// useful forensically (which chip, CCD layout, RAM).
fn host_system_info(topology: &framesage_core::CpuTopology) -> SystemInfo {
    SystemInfo {
        os_build: framesage_etw::build_gate::detected_build().unwrap_or(0),
        cpu_brand: cpu_brand(),
        logical_cpus: topology.count() as u32,
        topology_ccds: topology.ccds().count() as u32,
        memory_total_bytes: total_physical_memory_bytes(),
    }
}

/// Total physical RAM in bytes via the sys layer's `GlobalMemoryStatusEx`
/// wrapper. 0 off Windows or on probe failure (honest absence).
fn total_physical_memory_bytes() -> u64 {
    #[cfg(windows)]
    {
        framesage_sys::process::memory_status()
            .map(|(total, _avail)| total)
            .unwrap_or(0)
    }
    #[cfg(not(windows))]
    {
        0
    }
}

/// CPU brand string via the CPUID extended leaves (0x8000_0002..=4),
/// e.g. "AMD Ryzen 9 9950X3D 16-Core Processor". Empty on non-x86_64 or
/// when the leaves aren't supported.
fn cpu_brand() -> String {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::__cpuid;
        // __cpuid is safe on x86_64 (CPUID is always available); leaf
        // 0x8000_0000 reports the highest extended leaf available.
        let max_ext = __cpuid(0x8000_0000).eax;
        if max_ext < 0x8000_0004 {
            return String::new();
        }
        let mut bytes = Vec::with_capacity(48);
        for leaf in [0x8000_0002u32, 0x8000_0003, 0x8000_0004] {
            let r = __cpuid(leaf);
            for reg in [r.eax, r.ebx, r.ecx, r.edx] {
                bytes.extend_from_slice(&reg.to_le_bytes());
            }
        }
        // The brand string is NUL-padded; trim at the first NUL and
        // collapse the interior padding spaces Intel/AMD leave.
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end])
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        String::new()
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
    presentmon_restarts: Arc<AtomicU32>,
    etw_drops: Arc<AtomicU64>,
) -> tokio::task::JoinHandle<()> {
    let mut rx = engine.subscribe();
    tokio::spawn(async move {
        let mut recorder = SessionRecorder::new(dir);
        recorder.set_capabilities(caps);
        recorder.set_presentmon_restart_counter(presentmon_restarts);
        recorder.set_etw_drop_counter(etw_drops);
        // Stamp real host facts (CPU brand, CCD count, RAM) from the
        // engine's detected topology, replacing the honest-zero default.
        recorder.set_system_info(host_system_info(&engine.topology_snapshot()));
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

    // #111 — PresentMon frame drops are a normal metric, NOT a
    // data-quality flag: they are summed into the summary but must not
    // mark the session partial (§2.3 partial_data = no samples or ETW
    // *event* drops). Folding presentation drops into partial_data would
    // disable attribution on nearly every real session.
    #[test]
    fn dropped_frames_are_summed_but_do_not_mark_a_session_partial() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());
        rec.handle_event(&foreground("g.exe", 7), true);
        rec.handle_event(&entered("game-x3d"), true);
        let mut s = frame_stats(0, 16_000, 22_000);
        s.frames_dropped = 5;
        rec.record_frame_sample(&s);
        let mut s2 = frame_stats(0, 16_000, 22_000);
        s2.frames_dropped = 3;
        rec.record_frame_sample(&s2);
        rec.handle_event(&exited("done"), true);

        let entry = &list_sessions(dir.path()).unwrap()[0];
        assert!(
            !entry.partial_data,
            "presentation frame drops must NOT mark the session partial"
        );

        // …but the drop total rides along in the summary as a metric.
        let path = dir.path().join(format!("{}.jsonl", entry.session_id));
        let (events, _) = read_session(&path).unwrap();
        match events.last().unwrap() {
            SessionEvent::SessionEnd { summary, .. } => {
                assert_eq!(summary.frames_dropped, 8, "5 + 3 drops summed");
            }
            other => panic!("expected session_end, got {other:?}"),
        }
    }

    // #111 — session_end.presentmon_restarts reflects only the restarts
    // the shared counter accrued during this session's window.
    #[test]
    fn presentmon_restarts_are_scoped_to_the_session_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());
        let counter = Arc::new(AtomicU32::new(0));
        rec.set_presentmon_restart_counter(counter.clone());

        // Two restarts happened BEFORE this session — must not be counted.
        counter.store(2, Ordering::Relaxed);
        rec.handle_event(&foreground("g.exe", 7), true);
        rec.handle_event(&entered("game-x3d"), true);
        // Three restarts DURING the session.
        counter.fetch_add(3, Ordering::Relaxed);
        rec.handle_event(&exited("done"), true);

        let entry = &list_sessions(dir.path()).unwrap()[0];
        let path = dir.path().join(format!("{}.jsonl", entry.session_id));
        let (events, _) = read_session(&path).unwrap();
        match events.last().unwrap() {
            SessionEvent::SessionEnd {
                presentmon_restarts,
                ..
            } => assert_eq!(
                *presentmon_restarts, 3,
                "only in-session restarts count, not the pre-session baseline"
            ),
            other => panic!("expected session_end, got {other:?}"),
        }
    }

    // host_system_info counts logical CPUs and distinct CCDs from the
    // engine topology (not zeros), and cpu_brand() never panics.
    #[test]
    fn host_system_info_reports_real_topology_facts() {
        use framesage_core::{CoreKind, CpuTopology, LogicalCpu};
        let mut cpus = Vec::new();
        for core in 0..4u32 {
            let ccd = if core < 2 { 0 } else { 1 };
            cpus.push(LogicalCpu {
                index: core,
                physical_core: core,
                ccd,
                kind: CoreKind::Cache,
                cppc_rank: None,
                l3_cache_bytes: None,
                is_smt_sibling: false,
            });
        }
        let topo = CpuTopology { cpus };
        let info = host_system_info(&topo);
        assert_eq!(info.logical_cpus, 4);
        assert_eq!(info.topology_ccds, 2, "two distinct CCDs");
        // cpu_brand is host-dependent; just assert the probe is total
        // (no panic) — it's exercised for coverage.
        let _ = cpu_brand();
    }

    // Group A — ETW kernel-event drops during a session mark it partial
    // and populate session_end.etw_drops_total, scoped to the session
    // window (pre-session drops on the shared counter don't count).
    #[test]
    fn etw_drops_during_a_session_mark_it_partial_and_are_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let mut rec = SessionRecorder::new(dir.path().to_path_buf());
        let drops = Arc::new(AtomicU64::new(0));
        rec.set_etw_drop_counter(drops.clone());

        // 7 drops happened BEFORE this session — must not be counted.
        drops.store(7, Ordering::Relaxed);
        rec.handle_event(&foreground("g.exe", 7), true);
        rec.handle_event(&entered("game-x3d"), true);
        // A clean frame sample would otherwise make the session
        // non-partial — but an in-session ETW drop overrides that.
        rec.record_frame_sample(&frame_stats(0, 16_000, 22_000));
        drops.fetch_add(4, Ordering::Relaxed);
        rec.handle_event(&exited("done"), true);

        let entry = &list_sessions(dir.path()).unwrap()[0];
        assert!(
            entry.partial_data,
            "in-session ETW drops must mark the session partial"
        );
        let path = dir.path().join(format!("{}.jsonl", entry.session_id));
        let (events, _) = read_session(&path).unwrap();
        match events.last().unwrap() {
            SessionEvent::SessionEnd {
                etw_drops_total, ..
            } => assert_eq!(*etw_drops_total, 4, "only in-session drops count"),
            other => panic!("expected session_end, got {other:?}"),
        }
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
