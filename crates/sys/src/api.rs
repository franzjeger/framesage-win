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
//! Item 3.1b expands the surface to cover the full engine call set:
//! apply / revert / process actions / system metrics / version info /
//! per-PID enumeration. That's ~25 methods. The signatures stay
//! one-to-one with the underlying free functions so each migration is
//! mechanical.

use crate::apply::AppliedState;
use crate::foreground::ForegroundInfo;
use crate::process::{MemoryInfo, PerCpuTimes, PidSnapshot, ProcessCpuTimes, SystemCpuTimes};
use crate::services::ServiceInfo;
use crate::sys_proc_info::SysProcInfo;
use crate::version_info::VersionInfo;

use anyhow::Result;
use framesage_core::{AntiCheatPresence, CpuTopology, PriorityClass, Profile};

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
    // ─── AC detection ──────────────────────────────────────────────

    /// Probe the live process list for anti-cheat client / driver
    /// processes (Vanguard, EAC, Javelin, BattlEye, FACEIT, ESEA).
    /// Used by the engine's `maybe_refresh_ac_presence` to choose the
    /// active AC tier.
    fn detect_anti_cheats(&self) -> Result<AntiCheatPresence>;

    // ─── Topology ──────────────────────────────────────────────────

    /// Re-enumerate the machine's CPU topology via
    /// `GetLogicalProcessorInformationEx` + CPPC + L3 cache
    /// enrichment. Called once at engine startup and again on
    /// `SystemEvent::Resume` (item 3.7) so the engine picks up power-
    /// plan-driven core parking or VM hot-plug events that happened
    /// while the system was suspended.
    fn detect_topology(&self) -> Result<CpuTopology>;

    // ─── Process enumeration ───────────────────────────────────────

    /// Enumerate every PID currently running.
    fn iter_pids(&self) -> Result<Vec<u32>>;

    /// Cheaper enumeration: PID + parent_pid + thread_count via the
    /// ToolHelp snapshot path. Used by the engine's process-tab data
    /// source as a fallback when the NTQSI single-syscall path fails.
    fn iter_pid_snapshots(&self) -> Result<Vec<PidSnapshot>>;

    /// One-syscall enumeration via NtQuerySystemInformation. Faster
    /// than the per-PID OpenProcess walk; the engine's
    /// `list_process_snapshots` prefers this when it succeeds.
    fn enumerate_processes(&self) -> Result<Vec<SysProcInfo>>;

    /// Return the full image path for `pid`, or `Ok(None)` if the
    /// process exited mid-call / the PID is protected / etc.
    fn exe_for_pid(&self, pid: u32) -> Result<Option<String>>;

    /// Resolve the process's owning user account name. `Ok(None)`
    /// when the kernel reports a token but `LookupAccountSidW`
    /// returns nothing (rare; some capability SIDs).
    fn user_for_pid(&self, pid: u32) -> Result<Option<String>>;

    /// Per-PID CPU times (kernel + user). The engine diffs these
    /// across samples to compute the CPU% column in the Processes tab.
    fn cpu_times(&self, pid: u32) -> Result<Option<ProcessCpuTimes>>;

    /// Per-PID memory (working set, peak working set, private bytes).
    fn memory_info(&self, pid: u32) -> Result<Option<MemoryInfo>>;

    /// Process affinity mask. The engine reads it for the Affinity
    /// column tooltip and for verifying applied affinity persisted.
    fn affinity_mask(&self, pid: u32) -> Result<Option<u64>>;

    // ─── System-wide metrics ───────────────────────────────────────

    /// System CPU times via `GetSystemTimes`. Diffed across samples
    /// for the perf-band aggregate.
    fn system_cpu_times(&self) -> Result<SystemCpuTimes>;

    /// Per-logical-CPU times via NT query. Empty Vec on hardware /
    /// platforms where the query isn't available.
    fn per_cpu_times(&self) -> Result<Vec<PerCpuTimes>>;

    /// `(total, available)` memory in bytes via
    /// `GlobalMemoryStatusEx`.
    fn memory_status(&self) -> Result<(u64, u64)>;

    // ─── Foreground ────────────────────────────────────────────────

    /// Current foreground window in the engine's session, or `None`
    /// for lock-screen / UAC / session-0 callers.
    fn current_foreground(&self) -> Result<Option<ForegroundInfo>>;

    // ─── Profile apply / revert ────────────────────────────────────

    /// Apply every knob in `profile` against `pid`. Returns an opaque
    /// `AppliedState` token the caller stores so `revert` can undo
    /// the changes later.
    fn apply(
        &self,
        pid: u32,
        profile: &Profile,
        topology: &CpuTopology,
    ) -> Result<AppliedState>;

    /// Revert per-PID changes captured in `state`.
    fn revert(&self, pid: u32, state: AppliedState) -> Result<()>;

    /// Re-apply a profile without capturing prior state. Used by the
    /// persistent-pin re-assert sweep — the engine wants to push
    /// kernel state back into the desired shape without recording
    /// "what it was before" again.
    fn reassert(&self, pid: u32, profile: &Profile, topology: &CpuTopology) -> Result<()>;

    // ─── One-shot per-PID setters ──────────────────────────────────

    /// Get the raw Win32 priority class constant for `pid`, or
    /// `Ok(None)` if it can't be read.
    fn get_priority_class_for_pid(&self, pid: u32) -> Result<Option<u32>>;

    /// Set priority class on `pid`. Bypasses the profile system —
    /// used by the Processes tab's right-click "Set priority" submenu
    /// and by ProBalance's restrain action.
    fn set_priority_class_for_pid(&self, pid: u32, class: PriorityClass) -> Result<()>;

    /// Restore a raw Win32 priority class constant. Used by
    /// ProBalance's restore action to set the original (pre-restrain)
    /// class back. Idempotent on already-restored PIDs.
    fn restore_priority_class_for_pid(&self, pid: u32, raw_class: u32) -> Result<()>;

    /// Set process affinity mask. One-shot — does NOT capture prior
    /// state. Used by the Processes-tab right-click and by the
    /// background affinity-rule walk.
    fn set_affinity_mask_for_pid(&self, pid: u32, mask: u64) -> Result<()>;

    /// `K32EmptyWorkingSet`. Pushes the PID's resident pages back to
    /// the system free pool.
    fn trim_working_set_for_pid(&self, pid: u32) -> Result<()>;

    // ─── Process actions ───────────────────────────────────────────

    /// Freeze every thread of `pid` via `NtSuspendProcess`. Stacks
    /// across repeated calls (each increments the suspend counter);
    /// `resume_process` resets to zero.
    fn suspend_process(&self, pid: u32) -> Result<()>;

    /// Release a previous suspend via `NtResumeProcess`. Safe on a
    /// process that isn't currently suspended.
    fn resume_process(&self, pid: u32) -> Result<()>;

    /// `TerminateProcess(pid, 1)`. The caller is responsible for any
    /// confirmation UX — the trait fires immediately.
    fn terminate_process(&self, pid: u32) -> Result<()>;

    // ─── Version info ──────────────────────────────────────────────

    /// Read the binary's version resource (Description, Company,
    /// ProductName). All fields are `Option<String>`; an empty
    /// `VersionInfo` means the binary has no resource at all.
    fn read_version_info(&self, exe_path: &str) -> Result<VersionInfo>;

    // ─── Services ──────────────────────────────────────────────────

    /// Item 4.13 — enumerate every Win32 service the SCM knows
    /// about (active + inactive) for the tray's discover-services
    /// view. Sorted by display_name (case-insensitive) for stable
    /// UI ordering.
    fn enumerate_services(&self) -> Result<Vec<ServiceInfo>>;
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

    fn detect_topology(&self) -> Result<CpuTopology> {
        crate::topology::detect()
    }

    fn iter_pids(&self) -> Result<Vec<u32>> {
        crate::process::iter_pids()
    }

    fn iter_pid_snapshots(&self) -> Result<Vec<PidSnapshot>> {
        crate::process::iter_pid_snapshots()
    }

    fn enumerate_processes(&self) -> Result<Vec<SysProcInfo>> {
        crate::sys_proc_info::enumerate_processes()
    }

    fn exe_for_pid(&self, pid: u32) -> Result<Option<String>> {
        crate::process::exe_for_pid(pid)
    }

    fn user_for_pid(&self, pid: u32) -> Result<Option<String>> {
        crate::process::user_for_pid(pid)
    }

    fn cpu_times(&self, pid: u32) -> Result<Option<ProcessCpuTimes>> {
        crate::process::cpu_times(pid)
    }

    fn memory_info(&self, pid: u32) -> Result<Option<MemoryInfo>> {
        crate::process::memory_info(pid)
    }

    fn affinity_mask(&self, pid: u32) -> Result<Option<u64>> {
        crate::process::affinity_mask(pid)
    }

    fn system_cpu_times(&self) -> Result<SystemCpuTimes> {
        crate::process::system_cpu_times()
    }

    fn per_cpu_times(&self) -> Result<Vec<PerCpuTimes>> {
        crate::process::per_cpu_times()
    }

    fn memory_status(&self) -> Result<(u64, u64)> {
        crate::process::memory_status()
    }

    fn current_foreground(&self) -> Result<Option<ForegroundInfo>> {
        crate::foreground::current()
    }

    fn apply(
        &self,
        pid: u32,
        profile: &Profile,
        topology: &CpuTopology,
    ) -> Result<AppliedState> {
        crate::apply::apply(pid, profile, topology)
    }

    fn revert(&self, pid: u32, state: AppliedState) -> Result<()> {
        crate::apply::revert(pid, state)
    }

    fn reassert(&self, pid: u32, profile: &Profile, topology: &CpuTopology) -> Result<()> {
        crate::apply::reassert(pid, profile, topology)
    }

    fn get_priority_class_for_pid(&self, pid: u32) -> Result<Option<u32>> {
        crate::apply::get_priority_class_for_pid(pid)
    }

    fn set_priority_class_for_pid(&self, pid: u32, class: PriorityClass) -> Result<()> {
        crate::apply::set_priority_class_for_pid(pid, class)
    }

    fn restore_priority_class_for_pid(&self, pid: u32, raw_class: u32) -> Result<()> {
        crate::apply::restore_priority_class_for_pid(pid, raw_class)
    }

    fn set_affinity_mask_for_pid(&self, pid: u32, mask: u64) -> Result<()> {
        crate::apply::set_affinity_mask_for_pid(pid, mask)
    }

    fn trim_working_set_for_pid(&self, pid: u32) -> Result<()> {
        crate::apply::trim_working_set_for_pid(pid)
    }

    fn suspend_process(&self, pid: u32) -> Result<()> {
        crate::process_actions::suspend(pid)
    }

    fn resume_process(&self, pid: u32) -> Result<()> {
        crate::process_actions::resume(pid)
    }

    fn terminate_process(&self, pid: u32) -> Result<()> {
        crate::process_actions::terminate(pid)
    }

    fn read_version_info(&self, exe_path: &str) -> Result<VersionInfo> {
        crate::version_info::read_version_info(exe_path)
    }

    fn enumerate_services(&self) -> Result<Vec<ServiceInfo>> {
        crate::services::enumerate_services()
    }
}
