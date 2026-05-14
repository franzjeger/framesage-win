//! Non-Windows stubs so other crates type-check on macOS/Linux developer
//! machines. Nothing here is reachable at runtime — the service / CLI / tray
//! binaries don't build on non-Windows hosts.

use anyhow::{anyhow, Result};
use framesage_core::{CpuTopology, Profile};

pub mod foreground {
    use super::*;
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
}

pub mod process {
    use super::*;
    pub fn iter_pids() -> Result<Vec<u32>> {
        Ok(Vec::new())
    }
    pub fn exe_for_pid(_pid: u32) -> Result<Option<String>> {
        Ok(None)
    }
}
