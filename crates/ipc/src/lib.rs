//! Named-pipe RPC between the privileged service and unprivileged clients
//! (tray, CLI).
//!
//! Wire format: newline-delimited JSON. Cheap to write, easy to debug with a
//! pipe-cat tool, no codegen, plenty fast for 10–50 messages per second.
//!
//! # Two pipes, one ACL split
//!
//! The service binds two named pipes:
//!
//! * **Status pipe** ([`PIPE_NAME_STATUS`]) — open to Authenticated Users.
//!   Accepts only read-only requests ([`Request::is_read_only`]). The tray
//!   UI and any unprivileged status caller connects here.
//! * **Admin pipe** ([`PIPE_NAME_ADMIN`]) — default Windows ACL
//!   (Administrators + LocalSystem). Accepts every request, including
//!   mutators like [`Request::SetPolicy`].
//!
//! A client picks the pipe by asking `Request::is_read_only`: read-only ⇒
//! status pipe, otherwise admin pipe. The server enforces the same rule on
//! the receive side as defense-in-depth, so a misrouted mutator on the status
//! pipe is rejected with [`Response::Error`] rather than executed.

use serde::{Deserialize, Serialize};

use framesage_core::{Policy, Profile, ProfileId};

/// Status pipe — readable + writable by Authenticated Users for round-tripping
/// status queries. The service rejects any non-read-only request received on
/// this pipe regardless of caller identity.
pub const PIPE_NAME_STATUS: &str = r"\\.\pipe\framesage-status";

/// Admin pipe — Administrators + LocalSystem only. Accepts every request,
/// including policy mutations and the Game Mode panic button.
pub const PIPE_NAME_ADMIN: &str = r"\\.\pipe\framesage-admin";

/// Requests sent from a client to the service.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Return high-level service status.
    Status,
    /// Replace the active policy. The service persists it and re-evaluates.
    SetPolicy { policy: Policy },
    /// Apply a named profile to the currently foregrounded process, ignoring
    /// the normal rule matcher until focus changes.
    ApplyOnce { profile: ProfileId },
    /// Enter manual mode: apply this profile to every foreground app and
    /// keep applying it across focus changes until explicitly cleared.
    /// Takes precedence over the Rules tab + default_profile until
    /// `ClearManualOverride` arrives.
    SetManualOverride { profile: ProfileId },
    /// Leave manual mode. Foreground apply returns to consulting Rules +
    /// default_profile. Idempotent — no-op if manual mode was already off.
    ClearManualOverride,
    /// Report the user-session foreground app to the service.
    ///
    /// The service runs in Windows session 0 (LocalSystem) and can't see
    /// the interactive desktop — `GetForegroundWindow()` returns null
    /// from session 0. The tray runs in the user's session, polls
    /// foreground itself, and sends this report on every frame. Engine
    /// uses the most recent report instead of polling on its own. Sent
    /// every few hundred ms; routed on the admin pipe because it
    /// ultimately drives apply/revert.
    ReportForeground {
        pid: u32,
        exe_name: String,
        path: String,
        title: String,
    },
    /// Tell the service the user-session currently has no foreground
    /// (lock screen, UAC dialog, transition). Engine treats this as
    /// "no foreground" so any active profile reverts.
    ReportNoForeground,
    /// Pause the engine (still alive, but stops applying anything).
    Pause,
    /// Resume after a pause.
    Resume,
    /// Panic button — force any active Game Mode session to revert immediately.
    /// Idempotent: no-op if no session is active.
    GameModeOff,
    /// Subscribe to live status events. The server keeps the connection open
    /// and streams `Event` records.
    Subscribe,
    /// Return a snapshot of every visible process plus the engine's view of
    /// how each maps onto our state (rule match, profile applied, ProBalance
    /// restraint). Read-only — backs the tray's Processes tab.
    ListProcesses,
    /// Set the priority class of an arbitrary live PID. The engine opens
    /// the process and writes the class directly — bypasses the profile
    /// system, so this is a one-off action not a persistent rule. Backs
    /// the Processes tab's right-click "Set priority" submenu.
    SetProcessPriority {
        pid: u32,
        class: framesage_core::PriorityClass,
    },
}

impl Request {
    /// True if this request only reads engine state and never mutates kernel
    /// or service state. Read-only requests are valid on the status pipe;
    /// mutators must go through the admin pipe.
    ///
    /// Keep this match exhaustive — the compiler will catch any new variant
    /// that forgets to classify itself, and the unit tests below cover every
    /// current variant by name.
    pub fn is_read_only(&self) -> bool {
        match self {
            Request::Status | Request::Subscribe | Request::ListProcesses => true,
            Request::SetPolicy { .. }
            | Request::ApplyOnce { .. }
            | Request::SetManualOverride { .. }
            | Request::ClearManualOverride
            | Request::ReportForeground { .. }
            | Request::ReportNoForeground
            | Request::Pause
            | Request::Resume
            | Request::GameModeOff
            | Request::SetProcessPriority { .. } => false,
        }
    }

    /// Which pipe should a client open to send this request? Convenience
    /// wrapper around [`Request::is_read_only`].
    pub fn target_pipe(&self) -> &'static str {
        if self.is_read_only() {
            PIPE_NAME_STATUS
        } else {
            PIPE_NAME_ADMIN
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Ok,
    /// Boxed because `StatusSnapshot` carries the full `Policy` and dwarfs the
    /// other variants — clippy enforces this so we don't blow the response
    /// enum's stack footprint on every reply.
    Status(Box<StatusSnapshot>),
    Processes {
        snapshots: Vec<ProcessSnapshot>,
        /// Live system metrics paired with the snapshot — backs the
        /// performance band at the top of the tray UI. All three are
        /// "right now" point-in-time values, so the tray keeps its own
        /// 60-sample history for the sparkline; the service only emits
        /// the current reading.
        #[serde(default)]
        system: SystemMetrics,
    },
    Error {
        message: String,
    },
}

/// System-wide point-in-time metrics, attached to each `Processes`
/// response. The performance band at the top of the tray UI consumes
/// these to render the sliding sparkline + current values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SystemMetrics {
    /// Total CPU utilisation across all logical processors, 0-100. Derived
    /// from `GetSystemTimes` deltas between two engine ticks.
    pub cpu_percent: u8,
    /// Per-logical-CPU utilisation, 0-100 each. Index `i` is logical CPU `i`
    /// in Windows' numbering (group 0). Empty until the engine has two
    /// samples to diff, or on machines where the kernel refused the
    /// `NtQuerySystemInformation(SystemProcessorPerformanceInformation)`
    /// call. Tray renders this as a row of per-core bars in the perf band.
    #[serde(default)]
    pub per_core_cpu_percent: Vec<u8>,
    /// Physical RAM in use, bytes (total - available).
    pub memory_used_bytes: u64,
    /// Physical RAM installed, bytes.
    pub memory_total_bytes: u64,
}

/// One row of the Processes tab's live view. Sent over IPC, so all fields
/// are owned + serialisable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSnapshot {
    pub pid: u32,
    /// PID of the parent process at snapshot time (`PROCESSENTRY32W::
    /// th32ParentProcessID`). `0` for true roots; a non-zero value
    /// pointing at a PID that's not in the snapshot is an orphan and is
    /// rendered at the top of the tree. `serde(default)` so older clients
    /// still parse responses written by newer servers.
    #[serde(default)]
    pub parent_pid: u32,
    /// Image filename only (no path), original case as the kernel reports.
    pub exe_name: String,
    /// Full image path. Carried alongside `exe_name` so the tray can extract
    /// the exe's icon via `SHGetFileInfoW` without round-tripping. Empty
    /// string when the engine couldn't resolve it (protected process, exited
    /// between enumerate and query). `serde(default)` so older clients still
    /// parse responses written by newer servers and vice-versa.
    #[serde(default)]
    pub exe_path: String,
    /// Human-readable "FileDescription" string from the exe's version
    /// resource — what Task Manager shows in its Description column
    /// ("Microsoft OneDrive", "Steam Client Service Helper"). `None` when
    /// the resource is missing or the file is unreadable; cached
    /// engine-side keyed by `exe_path`, so the cost is paid once per exe.
    /// `serde(default)` so the field is optional on the wire.
    #[serde(default)]
    pub description: Option<String>,
    /// "CompanyName" string from the exe's version resource ("Microsoft
    /// Corporation", "Valve", "Electronic Arts"). Useful for telling
    /// publisher at a glance when an unfamiliar binary appears. Cached
    /// alongside `description` from the same resource read.
    #[serde(default)]
    pub company: Option<String>,
    /// User that owns the process, formatted as `"DOMAIN\\username"`
    /// (or just `"username"` when there's no domain). `None` for
    /// processes the engine couldn't open or whose SID failed to
    /// resolve. Cached engine-side keyed by PID with eviction on PID
    /// disappearance so the cache stays bounded.
    #[serde(default)]
    pub user: Option<String>,
    /// Live `GetPriorityClass` value (raw Win32 constant).
    pub priority_class_raw: u32,
    /// Live `GetProcessAffinityMask` value. `u64` so we can grow past 32 CPUs.
    pub affinity_mask: u64,
    /// "% of one logical CPU" over the engine's last sample window. 0 if
    /// the engine hasn't sampled the process yet (e.g. just spawned).
    pub cpu_percent: u16,
    /// Working set, bytes.
    pub memory_bytes: u64,
    /// Thread count.
    pub threads: u32,
    /// Note text from the rule that matched this exe, if any. `None` means
    /// no rule matched and the engine is treating it as background.
    pub matched_rule_note: Option<String>,
    /// Profile id the engine has currently applied to this PID, if any.
    /// `None` means the engine isn't tracking the PID (no rule match AND
    /// not seen yet by the background scan).
    pub managed_profile: Option<String>,
    /// `true` if ProBalance currently has this PID restrained (priority
    /// class lowered for contention relief). Mutually exclusive with
    /// `managed_profile` in normal operation — ProBalance skips managed
    /// PIDs by design.
    pub restrained_by_probalance: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub paused: bool,
    pub policy: Policy,
    pub foreground: Option<ForegroundSnapshot>,
    pub active_profile: Option<Profile>,
    /// When `Some`, the engine is in manual mode and applying the named
    /// profile to every foreground app regardless of Rules. The tray
    /// surfaces this with a banner + "Disable manual mode" button.
    #[serde(default)]
    pub manual_override: Option<ProfileId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForegroundSnapshot {
    pub pid: u32,
    pub exe_name: String,
    pub path: String,
    pub title: String,
}

/// Streamed when a client uses `Request::Subscribe`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Foreground app changed; engine has applied (or will apply) `profile`.
    ForegroundChanged {
        foreground: ForegroundSnapshot,
        profile: ProfileId,
    },
    Paused,
    Resumed,

    /// ProBalance demoted a background CPU hog. `from_class` and `to_class`
    /// are raw Win32 priority class constants captured at decision time so
    /// the action log can render the demotion factually.
    ProBalanceRestrained {
        pid: u32,
        exe_name: String,
        from_class: u32,
        to_class: u32,
    },

    /// ProBalance restored a previously-restrained process to its original
    /// priority class (the value captured in the matching `Restrained` event).
    ProBalanceRestored {
        pid: u32,
        exe_name: String,
        restored_class: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn sample_policy() -> Policy {
        Policy {
            profiles: HashMap::new(),
            rules: vec![],
            default_profile: ProfileId("perf".into()),
            background_profile: None,
            tick_ms: 300,
            probalance: framesage_core::ProBalanceConfig::default(),
        }
    }

    /// Pipe-routing contract: read-only requests target the status pipe,
    /// mutators target the admin pipe. If a new variant lands without a
    /// classification in `is_read_only`, this match-based test won't catch
    /// it — but `is_read_only`'s own match IS exhaustive, so the compiler
    /// catches it there. This test locks the public routing table.
    #[test]
    fn is_read_only_classifies_every_request_variant() {
        assert!(Request::Status.is_read_only());
        assert!(Request::Subscribe.is_read_only());
        assert!(Request::ListProcesses.is_read_only());
        assert!(!Request::SetProcessPriority {
            pid: 1,
            class: framesage_core::PriorityClass::Normal,
        }
        .is_read_only());
        assert!(!Request::SetPolicy {
            policy: sample_policy()
        }
        .is_read_only());
        assert!(!Request::ApplyOnce {
            profile: ProfileId("game-x3d".into())
        }
        .is_read_only());
        assert!(!Request::SetManualOverride {
            profile: ProfileId("game-x3d".into())
        }
        .is_read_only());
        assert!(!Request::ClearManualOverride.is_read_only());
        assert!(!Request::ReportForeground {
            pid: 1,
            exe_name: String::new(),
            path: String::new(),
            title: String::new(),
        }
        .is_read_only());
        assert!(!Request::ReportNoForeground.is_read_only());
        assert!(!Request::Pause.is_read_only());
        assert!(!Request::Resume.is_read_only());
        assert!(!Request::GameModeOff.is_read_only());
    }

    #[test]
    fn target_pipe_matches_is_read_only() {
        assert_eq!(Request::Status.target_pipe(), PIPE_NAME_STATUS);
        assert_eq!(Request::Subscribe.target_pipe(), PIPE_NAME_STATUS);
        assert_eq!(Request::Pause.target_pipe(), PIPE_NAME_ADMIN);
        assert_eq!(Request::Resume.target_pipe(), PIPE_NAME_ADMIN);
        assert_eq!(Request::GameModeOff.target_pipe(), PIPE_NAME_ADMIN);
        assert_eq!(
            Request::ApplyOnce {
                profile: ProfileId("game-x3d".into())
            }
            .target_pipe(),
            PIPE_NAME_ADMIN
        );
        assert_eq!(
            Request::SetPolicy {
                policy: sample_policy()
            }
            .target_pipe(),
            PIPE_NAME_ADMIN
        );
    }

    #[test]
    fn pipe_names_use_correct_prefix() {
        // Catch typos. The kernel will reject anything not starting with
        // `\\.\pipe\` so a regression here would manifest at runtime as
        // ERROR_INVALID_NAME, which is hard to debug. Lock it here.
        assert!(PIPE_NAME_STATUS.starts_with(r"\\.\pipe\"));
        assert!(PIPE_NAME_ADMIN.starts_with(r"\\.\pipe\"));
        assert_ne!(PIPE_NAME_STATUS, PIPE_NAME_ADMIN);
    }
}
