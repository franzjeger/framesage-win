//! Session event schema v1 — verbatim from `audit/v0.7-architecture.md`
//! §2.3 "Session recording schema".
//!
//! Every event line carries `schema_version` + `kind` + `at_ms`; the
//! `kind` string is the serde tag. v0.7.1 ships schema_version 1.
//! Adding fields is backwards-compatible; removing or renaming bumps
//! to v2.

use serde::{Deserialize, Serialize};

/// Schema version this crate writes. Readers accept any v1 line and
/// skip unknown `kind`s (forward compatibility within v1).
pub const SCHEMA_VERSION: u32 = 1;

/// One line of a session `.jsonl` file. Tagged by `kind` per §2.3.
///
/// `schema_version` and `at_ms` live on every variant rather than a
/// wrapper struct so each serialized line is exactly the flat object
/// the architecture specifies.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
    /// Always the first line of the file. Written once.
    SessionStart {
        schema_version: u32,
        at_ms: u64,
        session_id: String,
        started_at_unix_secs: u64,
        game_exe: String,
        game_pid: u32,
        profile_id: String,
        matched_rule_index: Option<u32>,
        system: SystemInfo,
        /// `active` | `passthrough` | `unavailable`
        etw_state: String,
        /// `active` | `crashed` | `disabled`
        presentmon_state: String,
        /// Table-id string from §2.1; `unknown` on passthrough.
        opcode_table: String,
    },
    /// Every apply / revert / Game Mode entry/exit.
    FramesageAction {
        schema_version: u32,
        at_ms: u64,
        /// `apply_profile` | `revert_profile` | `game_mode_entered` |
        /// `game_mode_exited` | `manual_override_set` |
        /// `manual_override_cleared` | `probalance_restrained` |
        /// `probalance_restored`
        action: String,
        profile_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
    },
    /// Aggregated kernel-event signal from the ETW drain worker —
    /// emitted only when current rate exceeds 3× the rolling 5-minute
    /// baseline.
    KernelSignal {
        schema_version: u32,
        at_ms: u64,
        /// `dpc_spike` | `isr_spike` | `hard_fault_burst` |
        /// `context_switch_storm` | `diskio_spike`
        signal: String,
        rate_per_sec: u64,
        baseline_5min_per_sec: u64,
        above_baseline_pct: u64,
    },
    /// PresentMon-sourced, 1 Hz downsampled.
    FrameSample {
        schema_version: u32,
        at_ms: u64,
        frame_count: u32,
        frame_time_us_p50: u64,
        frame_time_us_p99: u64,
        frames_dropped: u32,
    },
    /// 1 Hz from the engine's existing per-core sampling.
    CpuSample {
        schema_version: u32,
        at_ms: u64,
        total_pct: u8,
        per_core_pct: Vec<u8>,
        /// Schema slot reserved for v0.8 (Phase 2 sign-off resolution
        /// #5). ALWAYS `None` in v0.7.x regardless of conditions; the
        /// `recorder_per_process_enabled` policy hook (default false,
        /// no UI) is what v0.8 flips. Existing readers see `null`,
        /// v0.8+ readers see populated data — schema_version stays 1.
        /// Privacy rationale: §2.3 privacy table (per-PID CPU + exe
        /// names on disk = a coarse picture of what the user runs;
        /// v0.8 makes it an informed opt-in alongside auto-profile
        /// learning).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        per_process: Option<Vec<PerProcessCpu>>,
    },
    /// Emitted when a managed PID's working set changes by > 100 MB
    /// since the last sample, or on every 30-second tick.
    WorkingSetDelta {
        schema_version: u32,
        at_ms: u64,
        pid: u32,
        exe_name: String,
        delta_bytes: i64,
        current_bytes: u64,
    },
    /// Always the last line. Marks the session immutable.
    SessionEnd {
        schema_version: u32,
        at_ms: u64,
        reason: String,
        /// True if `etw_drops_total > 0`, PresentMon was unavailable /
        /// crashed for any window, or ETW ran in passthrough mode.
        partial_data: bool,
        etw_drops_total: u64,
        presentmon_restarts: u32,
        summary: SessionSummary,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemInfo {
    pub os_build: u32,
    pub cpu_brand: String,
    pub logical_cpus: u32,
    pub topology_ccds: u32,
    pub memory_total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PerProcessCpu {
    pub pid: u32,
    pub exe_name: String,
    pub cpu_pct: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionSummary {
    pub duration_secs: u64,
    pub frame_time_p50_us_baseline: Option<u64>,
    pub frame_time_p50_us_with_rules: Option<u64>,
    pub frame_time_p99_us_baseline: Option<u64>,
    pub frame_time_p99_us_with_rules: Option<u64>,
    pub actions_applied: u32,
    pub kernel_signals: u32,
}

impl SessionEvent {
    /// Milliseconds since session start, present on every variant.
    pub fn at_ms(&self) -> u64 {
        match self {
            SessionEvent::SessionStart { at_ms, .. }
            | SessionEvent::FramesageAction { at_ms, .. }
            | SessionEvent::KernelSignal { at_ms, .. }
            | SessionEvent::FrameSample { at_ms, .. }
            | SessionEvent::CpuSample { at_ms, .. }
            | SessionEvent::WorkingSetDelta { at_ms, .. }
            | SessionEvent::SessionEnd { at_ms, .. } => *at_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_start() -> SessionEvent {
        SessionEvent::SessionStart {
            schema_version: SCHEMA_VERSION,
            at_ms: 0,
            session_id: "f47ac10b-58cc-4372-a567-0e02b2c3d479".into(),
            started_at_unix_secs: 1_778_996_829,
            game_exe: "Attila.exe".into(),
            game_pid: 16444,
            profile_id: "game-x3d".into(),
            matched_rule_index: Some(4),
            system: SystemInfo {
                os_build: 26200,
                cpu_brand: "AMD Ryzen 9 9950X3D".into(),
                logical_cpus: 32,
                topology_ccds: 2,
                memory_total_bytes: 137_438_953_472,
            },
            etw_state: "active".into(),
            presentmon_state: "active".into(),
            opcode_table: "win11_24h2_26200".into(),
        }
    }

    #[test]
    fn events_round_trip_through_serde() {
        let events = vec![
            sample_start(),
            SessionEvent::FramesageAction {
                schema_version: SCHEMA_VERSION,
                at_ms: 1234,
                action: "game_mode_entered".into(),
                profile_id: "game-x3d".into(),
                details: Some(serde_json::json!({"services_to_stop": 9})),
            },
            SessionEvent::FrameSample {
                schema_version: SCHEMA_VERSION,
                at_ms: 2000,
                frame_count: 287,
                frame_time_us_p50: 3489,
                frame_time_us_p99: 6240,
                frames_dropped: 0,
            },
            SessionEvent::SessionEnd {
                schema_version: SCHEMA_VERSION,
                at_ms: 600_000,
                reason: "foreground_lost".into(),
                partial_data: false,
                etw_drops_total: 0,
                presentmon_restarts: 0,
                summary: SessionSummary {
                    duration_secs: 600,
                    frame_time_p50_us_baseline: Some(3520),
                    frame_time_p50_us_with_rules: Some(3489),
                    frame_time_p99_us_baseline: Some(6310),
                    frame_time_p99_us_with_rules: Some(6240),
                    actions_applied: 1,
                    kernel_signals: 0,
                },
            },
        ];
        for ev in events {
            let line = serde_json::to_string(&ev).unwrap();
            let back: SessionEvent = serde_json::from_str(&line).unwrap();
            assert_eq!(ev, back);
        }
    }

    #[test]
    fn kind_tag_is_snake_case_per_architecture() {
        let line = serde_json::to_string(&sample_start()).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["kind"], "session_start");
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["at_ms"], 0);
    }

    // §2.3 / PRE-L-001 — the per_process schema slot: null (omitted)
    // in v0.7.x, populated shape must still parse for v0.8 readers.
    #[test]
    fn cpu_sample_per_process_slot_disabled_and_enabled_paths() {
        let disabled = SessionEvent::CpuSample {
            schema_version: SCHEMA_VERSION,
            at_ms: 2000,
            total_pct: 47,
            per_core_pct: vec![62, 28, 71, 44],
            per_process: None,
        };
        let line = serde_json::to_string(&disabled).unwrap();
        assert!(
            !line.contains("per_process"),
            "null slot must be omitted on serialization"
        );

        let enabled_line = r#"{"schema_version":1,"kind":"cpu_sample","at_ms":2000,"total_pct":47,"per_core_pct":[62],"per_process":[{"pid":16444,"exe_name":"Attila.exe","cpu_pct":28}]}"#;
        let back: SessionEvent = serde_json::from_str(enabled_line).unwrap();
        match back {
            SessionEvent::CpuSample { per_process, .. } => {
                let pp = per_process.expect("populated slot parses");
                assert_eq!(pp[0].exe_name, "Attila.exe");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
}
