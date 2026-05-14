//! Win32 implementation of `framesage_gamemode::planner::SystemStateQuery`.
//!
//! This is the boundary between the platform-agnostic planner and real OS
//! calls. Every method returns `anyhow::Result` so a transient API failure
//! becomes a per-action skip rather than an engine-level error.

use anyhow::Result;

use framesage_core::PowerPlanId;
use framesage_gamemode::planner::SystemStateQuery;
use framesage_gamemode::state::ServiceStatus;

use super::{power_plan, process, service, taskbar};

/// Concrete `SystemStateQuery` for Windows. Zero-sized — all the state lives
/// in the OS. Construct freely; planning is read-only.
#[derive(Debug, Default, Clone, Copy)]
pub struct Win32StateQuery;

impl SystemStateQuery for Win32StateQuery {
    fn taskbar_visible(&self) -> Result<bool> {
        taskbar::taskbar_visible()
    }

    fn active_power_plan(&self) -> Result<Option<PowerPlanId>> {
        power_plan::get_active_plan().map(Some)
    }

    fn service_status(&self, id: &str) -> Result<ServiceStatus> {
        service::query_service_status(id)
    }

    fn pids_by_exe(&self, exe: &str) -> Result<Vec<(u32, String)>> {
        process::find_pids_by_exe(exe)
    }
}
