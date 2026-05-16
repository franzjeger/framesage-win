//! Win32 wrappers for framesage-win.
//!
//! Everything here is gated on `cfg(windows)`. On non-Windows hosts this crate
//! compiles to an empty shell so the workspace stays buildable for things like
//! tooling and CI dry-runs.
//!
//! # Layering (item 3.8)
//!
//! Depends on `framesage-core` (for the type vocabulary) and
//! `framesage-gamemode` (for the `SystemStateQuery` trait + `ServiceStatus`
//! / `AppliedActions` / `PreviousState` data shapes). The `gamemode` dep is
//! the one inversion in the workspace graph: this crate provides Win32
//! impls of traits that `gamemode` *defines*. The inversion is intentional
//! — the trait must live in a layer both sides can see, and `gamemode` is
//! the right home for the data shapes. The bridge is contained to one
//! submodule (`inner::game_mode`) so the rest of `framesage-sys`
//! (`process`, `apply`, `ac_detect`, `foreground`, etc.) stays
//! gamemode-free. See `ARCHITECTURE.md` at the repo root.
//!
//! This crate must NOT depend on `framesage-ipc`, `framesage-engine`,
//! `framesage-service`, `framesage-cli`, `framesage-tray`, or
//! `framesage-sim`.

#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(windows)]
mod inner;

#[cfg(windows)]
pub use inner::*;

#[cfg(not(windows))]
mod stub;

#[cfg(not(windows))]
pub use stub::*;

// Item 3.1 — trait abstraction over the syscall surface. Lives at
// the crate root so the production `RealSysApi` can forward to either
// `inner` or `stub` modules transparently.
mod api;
pub use api::{RealSysApi, SysApi};
