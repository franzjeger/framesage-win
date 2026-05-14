//! System-level Game Mode for framesage.
//!
//! This crate owns three things:
//!
//! 1. **The curated safe-list** — what services may be stopped and what
//!    processes may be suspended. Authoritative deny-list overrides allow-list.
//! 2. **The journal** — atomic on-disk record of "what state did the OS have
//!    before we touched it, and what did we change." Survives crashes; the
//!    service revives it on startup to revert a stranded session.
//! 3. **The planner** — turns a `GameModeActions` request plus current OS
//!    state into a typed, ordered `ActionPlan` of reversible operations.
//!
//! The actual Win32 calls live in `framesage-sys::game_mode::*`. This crate
//! is platform-agnostic and runs on macOS / Linux during development; the
//! `framesage-sim` harness drives the planner here against synthetic state.

pub mod journal;
pub mod planner;
pub mod safe_list;
pub mod state;

pub use journal::{Journal, JournalEntry, JournalError};
pub use planner::{plan, ActionPlan, PlanError, PlannedAction};
pub use safe_list::{
    ProcessVerdict, Rejection, RejectionKind, SafeList, SafeListError, SafeProcessEntry,
    SafeServiceEntry, ServiceVerdict,
};
pub use state::{AppliedActions, PreviousState, ServiceStateSnapshot, ServiceStatus};
