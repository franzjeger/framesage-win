//! Process enumeration. Needed for `background_profile` enforcement: walk all
//! running PIDs, check the image path against rules, apply background policy
//! to the ones that don't match a foreground rule.
//!
//! v0.1 stub: returns nothing. Real implementation uses
//! `CreateToolhelp32Snapshot` + `Process32FirstW` / `Process32NextW`, or the
//! NT `NtQuerySystemInformation(SystemProcessInformation, ...)` for a single
//! snapshot. Both are documented; ToolHelp is the simplest.

use anyhow::Result;

pub fn iter_pids() -> Result<Vec<u32>> {
    // TODO(v0.2): implement via CreateToolhelp32Snapshot.
    Ok(Vec::new())
}

pub fn exe_for_pid(_pid: u32) -> Result<Option<String>> {
    // TODO(v0.2): QueryFullProcessImageNameW like in foreground.rs.
    Ok(None)
}
