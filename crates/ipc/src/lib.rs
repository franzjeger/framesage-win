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
            Request::Status | Request::Subscribe => true,
            Request::SetPolicy { .. }
            | Request::ApplyOnce { .. }
            | Request::SetManualOverride { .. }
            | Request::ClearManualOverride
            | Request::ReportForeground { .. }
            | Request::ReportNoForeground
            | Request::Pause
            | Request::Resume
            | Request::GameModeOff => false,
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
    Error {
        message: String,
    },
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
