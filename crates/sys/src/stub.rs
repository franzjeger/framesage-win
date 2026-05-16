//! Non-Windows stubs so other crates type-check on macOS/Linux developer
//! machines. Nothing here is reachable at runtime — the service / CLI / tray
//! binaries don't build on non-Windows hosts.

use anyhow::{anyhow, Result};
use framesage_core::{AntiCheatPresence, CpuTopology, PriorityClass, Profile};

pub mod ac_detect {
    use super::*;
    /// Non-Windows stub: returns "nothing detected" so cross-platform
    /// engine paths (used by `framesage-sim`) get a consistent
    /// no-AC-active view. The real probe in `inner::ac_detect`
    /// requires `framesage_sys::process::iter_pids` which only
    /// works on Windows.
    pub fn detect_anti_cheats() -> Result<AntiCheatPresence> {
        Ok(AntiCheatPresence::default())
    }
}

pub mod sys_proc_info {
    use super::*;
    use std::time::Duration;

    /// Stub mirror of `inner::sys_proc_info::SysProcInfo` so callers
    /// type-check on non-Windows.
    #[derive(Debug, Clone, Default)]
    pub struct SysProcInfo {
        pub pid: u32,
        pub parent_pid: u32,
        pub exe_name: String,
        pub thread_count: u32,
        pub handle_count: u32,
        pub total_cpu_100ns: u64,
        pub working_set_bytes: u64,
        pub peak_working_set_bytes: u64,
        pub private_bytes: u64,
        pub base_priority: i32,
    }

    /// Non-Windows: no kernel to query. Returns empty Vec so the
    /// engine's fallback path (per-PID ToolHelp + OpenProcess) is
    /// the only active code path in `framesage-sim`.
    pub fn enumerate_processes() -> Result<Vec<SysProcInfo>> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }

    pub fn kpriority_to_win32_class(_kpriority: i32) -> u32 {
        0
    }

    pub fn duration_from_100ns(units: u64) -> Duration {
        Duration::from_nanos(units.saturating_mul(100))
    }
}

pub mod foreground {
    use super::*;
    #[derive(Debug, Clone)]
    pub struct ForegroundInfo {
        pub pid: u32,
        pub exe_name: String,
        pub path: String,
        pub title: String,
    }
    pub fn current() -> Result<Option<ForegroundInfo>> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }
}

pub mod topology {
    use super::*;
    pub fn detect() -> Result<CpuTopology> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }
}

pub mod apply {
    use super::*;
    #[derive(Debug, Default)]
    pub struct AppliedState;
    pub fn apply(_pid: u32, _profile: &Profile, _topology: &CpuTopology) -> Result<AppliedState> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }
    pub fn revert(_pid: u32, _state: AppliedState) -> Result<()> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }
    pub fn reassert(_pid: u32, _profile: &Profile, _topology: &CpuTopology) -> Result<()> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }
    pub fn get_priority_class_for_pid(_pid: u32) -> Result<Option<u32>> {
        Ok(None)
    }
    pub fn set_priority_class_for_pid(_pid: u32, _class: PriorityClass) -> Result<()> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }
    pub fn restore_priority_class_for_pid(_pid: u32, _raw_class: u32) -> Result<()> {
        Ok(())
    }
    pub fn set_affinity_mask_for_pid(_pid: u32, _mask: u64) -> Result<()> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }
    pub fn trim_working_set_for_pid(_pid: u32) -> Result<()> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }
}

pub mod process {
    use super::*;

    #[derive(Debug, Clone, Copy)]
    pub struct ProcessCpuTimes {
        pub kernel_100ns: u64,
        pub user_100ns: u64,
    }
    impl ProcessCpuTimes {
        pub fn total_100ns(&self) -> u64 {
            self.kernel_100ns.saturating_add(self.user_100ns)
        }
    }

    #[derive(Debug, Clone, Copy)]
    pub struct PidSnapshot {
        pub pid: u32,
        pub parent_pid: u32,
        pub thread_count: u32,
    }

    pub fn iter_pids() -> Result<Vec<u32>> {
        Ok(Vec::new())
    }
    pub fn iter_pid_snapshots() -> Result<Vec<PidSnapshot>> {
        Ok(Vec::new())
    }
    pub fn exe_for_pid(_pid: u32) -> Result<Option<String>> {
        Ok(None)
    }
    pub fn user_for_pid(_pid: u32) -> Result<Option<String>> {
        Ok(None)
    }
    pub fn cpu_times(_pid: u32) -> Result<Option<ProcessCpuTimes>> {
        Ok(None)
    }
    pub fn working_set_bytes(_pid: u32) -> Result<Option<u64>> {
        Ok(None)
    }
    #[derive(Debug, Clone, Copy, Default)]
    pub struct MemoryInfo {
        pub working_set_bytes: u64,
        pub peak_working_set_bytes: u64,
        pub private_bytes: u64,
    }
    pub fn memory_info(_pid: u32) -> Result<Option<MemoryInfo>> {
        Ok(None)
    }
    pub fn affinity_mask(_pid: u32) -> Result<Option<u64>> {
        Ok(None)
    }
    #[derive(Debug, Clone, Copy, Default)]
    pub struct SystemCpuTimes {
        pub idle_100ns: u64,
        pub kernel_100ns: u64,
        pub user_100ns: u64,
    }
    impl SystemCpuTimes {
        pub fn busy_100ns(&self) -> u64 {
            0
        }
        pub fn total_100ns(&self) -> u64 {
            0
        }
    }
    pub fn system_cpu_times() -> Result<SystemCpuTimes> {
        Ok(SystemCpuTimes::default())
    }
    #[derive(Debug, Clone, Copy, Default)]
    pub struct PerCpuTimes {
        pub idle_100ns: u64,
        pub kernel_100ns: u64,
        pub user_100ns: u64,
    }
    impl PerCpuTimes {
        pub fn busy_100ns(&self) -> u64 {
            0
        }
        pub fn total_100ns(&self) -> u64 {
            0
        }
    }
    pub fn per_cpu_times() -> Result<Vec<PerCpuTimes>> {
        Ok(Vec::new())
    }
    pub fn memory_status() -> Result<(u64, u64)> {
        Ok((0, 0))
    }
}

pub mod process_actions {
    use super::*;
    pub fn suspend(_pid: u32) -> Result<()> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }
    pub fn resume(_pid: u32) -> Result<()> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }
    pub fn terminate(_pid: u32) -> Result<()> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }
}

pub mod version_info {
    use super::Result;

    #[derive(Debug, Clone, Default)]
    pub struct VersionInfo {
        pub description: Option<String>,
        pub company: Option<String>,
        pub product_name: Option<String>,
    }
    impl VersionInfo {
        pub fn is_empty(&self) -> bool {
            self.description.is_none() && self.company.is_none() && self.product_name.is_none()
        }
    }
    pub fn read_version_info(_path: &str) -> Result<VersionInfo> {
        Ok(VersionInfo::default())
    }
}

pub mod game_mode {
    //! Non-Windows stubs. Game Mode actions are no-ops on a developer host;
    //! the planner and journal still exercise end-to-end via `framesage-sim`,
    //! but actual state changes only happen on real Windows.

    use anyhow::{anyhow, Result};
    use framesage_core::PowerPlanId;
    use framesage_gamemode::{
        planner::{PlannedAction, SystemStateQuery},
        state::{AppliedActions, PreviousState, ServiceStatus},
    };

    #[derive(Debug, Default, Clone, Copy)]
    pub struct Win32StateQuery;

    impl SystemStateQuery for Win32StateQuery {
        fn taskbar_visible(&self) -> Result<bool> {
            Err(anyhow!("framesage-sys: not supported on this host"))
        }
        fn active_power_plan(&self) -> Result<Option<PowerPlanId>> {
            Err(anyhow!("framesage-sys: not supported on this host"))
        }
        fn service_status(&self, _id: &str) -> Result<ServiceStatus> {
            Err(anyhow!("framesage-sys: not supported on this host"))
        }
        fn pids_by_exe(&self, _exe: &str) -> Result<Vec<(u32, String)>> {
            Err(anyhow!("framesage-sys: not supported on this host"))
        }
    }

    pub fn apply_action(_action: &PlannedAction, _applied: &mut AppliedActions) -> Result<()> {
        Err(anyhow!("framesage-sys: not supported on this host"))
    }

    pub fn revert_all(_applied: &AppliedActions, _previous: &PreviousState) {
        // No-op on non-Windows.
    }
}
