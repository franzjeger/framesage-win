//! Named-pipe RPC between the privileged service and unprivileged clients
//! (tray, CLI).
//!
//! Wire format: newline-delimited JSON. Cheap to write, easy to debug with a
//! pipe-cat tool, no codegen, plenty fast for 10–50 messages per second.

use serde::{Deserialize, Serialize};

use framesage_core::{Policy, Profile, ProfileId};

/// Canonical pipe name. The service creates it as `\\.\pipe\framesage`.
pub const PIPE_NAME: &str = r"\\.\pipe\framesage";

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
