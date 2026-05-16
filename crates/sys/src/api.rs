//! Item 3.1 — abstraction trait for the syscalls the engine relies on.
//!
//! The engine has historically reached into [`framesage_sys`]'s free
//! functions directly — `framesage_sys::ac_detect::detect_anti_cheats()`,
//! `framesage_sys::process::iter_pids()`, etc. That coupling has two
//! consequences worth fixing:
//!
//! 1. **Untestable**: spawning real processes or installing a real
//!    Valorant client just to exercise `maybe_refresh_ac_presence`
//!    isn't a viable test strategy. We've been writing assertion-only
//!    smoke tests against logic that's coupled to syscalls.
//! 2. **No surface for the simulator**: `framesage-sim` wants to
//!    drive the engine through scripted scenarios (Vanguard appears,
//!    then disappears; foreground flips through five apps in two
//!    seconds; etc.) without standing up the real kernel calls.
//!
//! This trait + its production impl (`RealSysApi`) close both gaps.
//! Engine code that needs a syscall goes through `self.sys.foo(...)`;
//! tests pass an `Arc<FakeSysApi>` and drive the timeline by hand.
//!
//! # Scope of this initial PR
//!
//! Only the methods exercised by the AC-detection path land in this
//! trait. The remaining ~60 engine call sites stay on the legacy free
//! functions and migrate to the trait in follow-up PRs. That keeps the
//! diff reviewable and the substrate growable without committing the
//! whole engine to a generic refactor up front.

use crate::foreground::ForegroundInfo;

use anyhow::Result;
use framesage_core::AntiCheatPresence;

/// Trait erasing the syscalls the engine needs to make about the
/// surrounding system. Implementations: `RealSysApi` (production —
/// forwards to the existing `framesage_sys::*` free functions),
/// `MockSysApi` (deterministic test fixture; lives in test-only code).
///
/// `Send + Sync` so an `Arc<dyn SysApi>` can be cheaply cloned across
/// the engine's tick task, IPC task, and reload task. None of the
/// implementations hold per-call mutable state in practice — the
/// underlying syscalls are stateless.
pub trait SysApi: Send + Sync {
    /// Probe the live process list for anti-cheat client / driver
    /// processes (Vanguard, EAC, Javelin, BattlEye, FACEIT, ESEA).
    /// Used by the engine's `maybe_refresh_ac_presence` to choose the
    /// active AC tier.
    ///
    /// Returns `Ok(default)` on hosts where the probe isn't supported
    /// (e.g. non-Windows simulator runs); only returns `Err` if the
    /// underlying process enumeration syscall hard-fails.
    fn detect_anti_cheats(&self) -> Result<AntiCheatPresence>;

    /// Enumerate every PID currently running. Mirrors the legacy
    /// `framesage_sys::process::iter_pids()`. The engine's background
    /// scan and affinity-rule live-walk paths consume this.
    fn iter_pids(&self) -> Result<Vec<u32>>;

    /// Return the full image path for `pid`, or `Ok(None)` if the
    /// process exited mid-call / the PID is protected / etc.
    /// Mirrors `framesage_sys::process::exe_for_pid`.
    fn exe_for_pid(&self, pid: u32) -> Result<Option<String>>;

    /// Return the currently-foregrounded window's metadata in the
    /// engine's session, or `None` if there's no foreground (lock
    /// screen, UAC dialog, session-0 caller). Mirrors
    /// `framesage_sys::foreground::current`. Used as a fallback when
    /// the tray reporter is stale (item 2.6).
    fn current_foreground(&self) -> Result<Option<ForegroundInfo>>;
}

/// Production implementation — every method forwards to the existing
/// `framesage_sys::*` free function. Zero allocations beyond what the
/// underlying function already does; the dyn-dispatch overhead is a
/// single vtable lookup per syscall, far below the syscall's own cost.
pub struct RealSysApi;

impl SysApi for RealSysApi {
    fn detect_anti_cheats(&self) -> Result<AntiCheatPresence> {
        crate::ac_detect::detect_anti_cheats()
    }

    fn iter_pids(&self) -> Result<Vec<u32>> {
        crate::process::iter_pids()
    }

    fn exe_for_pid(&self, pid: u32) -> Result<Option<String>> {
        crate::process::exe_for_pid(pid)
    }

    fn current_foreground(&self) -> Result<Option<ForegroundInfo>> {
        crate::foreground::current()
    }
}
