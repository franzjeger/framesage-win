//! Process enumeration.
//!
//! Needed for `background_profile` enforcement: walk all running PIDs, check
//! the image path against rules, apply background policy to the ones that
//! don't match a foreground rule.
//!
//! Implementation uses the ToolHelp snapshot API (`CreateToolhelp32Snapshot` +
//! `Process32FirstW` / `Process32NextW`). ToolHelp returns a static snapshot,
//! which is exactly the semantics the engine wants — iterating it can't race
//! against process creation/exit, and the cost is a single ~ms syscall on a
//! typical box (~250 processes). `NtQuerySystemInformation(SystemProcessInformation)`
//! is cheaper for very large process counts but requires hand-walking a
//! variable-length structure; ToolHelp keeps the unsafe surface small.

use anyhow::{anyhow, Result};

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, INVALID_HANDLE_VALUE, MAX_PATH};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

/// CPU times (kernel + user) for a single process, both in 100-nanosecond
/// units (the unit `GetProcessTimes` reports). Subtracting two samples taken
/// at known wall-clock instants yields per-process CPU consumption over that
/// interval; dividing by the elapsed wall time gives utilisation as a
/// fraction of one logical CPU.
#[derive(Debug, Clone, Copy)]
pub struct ProcessCpuTimes {
    /// `lpKernelTime` from `GetProcessTimes`, in 100-ns units.
    pub kernel_100ns: u64,
    /// `lpUserTime` from `GetProcessTimes`, in 100-ns units.
    pub user_100ns: u64,
}

impl ProcessCpuTimes {
    /// Sum of kernel + user CPU time consumed.
    pub fn total_100ns(&self) -> u64 {
        self.kernel_100ns.saturating_add(self.user_100ns)
    }
}

/// Snapshot every running process and return their PIDs.
///
/// Includes PID 0 (System Idle) and PID 4 (System) — callers that want only
/// user-mode processes should filter, since neither can be opened with
/// `PROCESS_QUERY_LIMITED_INFORMATION`.
pub fn iter_pids() -> Result<Vec<u32>> {
    // SAFETY: documented call. Returns INVALID_HANDLE_VALUE on failure.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|e| anyhow!("CreateToolhelp32Snapshot failed: {e}"))?;
    if snap == INVALID_HANDLE_VALUE {
        return Err(anyhow!(
            "CreateToolhelp32Snapshot returned INVALID_HANDLE_VALUE"
        ));
    }

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    let mut pids = Vec::with_capacity(256);

    // SAFETY: snap is a valid snapshot handle from CreateToolhelp32Snapshot.
    // dwSize is initialised. First and Next are documented to return BOOL.
    let first_ok = unsafe { Process32FirstW(snap, &mut entry) }.is_ok();
    if first_ok {
        pids.push(entry.th32ProcessID);
        // SAFETY: entry has been populated by Process32FirstW; reusing for Next
        // is the documented pattern.
        while unsafe { Process32NextW(snap, &mut entry) }.is_ok() {
            pids.push(entry.th32ProcessID);
        }
    }

    close_handle(snap);
    Ok(pids)
}

/// Full image path of a running process, or `None` if it has exited / is
/// inaccessible (protected processes, anti-cheat-protected processes, PID 0).
pub fn exe_for_pid(pid: u32) -> Result<Option<String>> {
    if pid == 0 {
        // System Idle Process can't be opened.
        return Ok(None);
    }
    // SAFETY: documented call.
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => h,
        // ACCESS_DENIED / INVALID_PARAMETER (PID exited): silently skip.
        Err(_) => return Ok(None),
    };

    let mut buf = [0u16; MAX_PATH as usize];
    let mut size = buf.len() as u32;
    // SAFETY: handle valid; buf + size valid out params.
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    close_handle(handle);

    match result {
        Ok(()) => Ok(Some(String::from_utf16_lossy(&buf[..size as usize]))),
        Err(_) => Ok(None),
    }
}

/// Sample CPU times (kernel + user) for a single PID. Used by ProBalance to
/// compute per-process CPU utilisation between two ticks: subtract this
/// tick's `total_100ns` from the next tick's, divide by elapsed wall time
/// in 100 ns units, get a fraction of one logical CPU.
///
/// Returns `Ok(None)` if the PID is gone or inaccessible (protected
/// processes, PID 0 / 4, ACCESS_DENIED). The caller can treat this as
/// "no signal" and skip the PID.
pub fn cpu_times(pid: u32) -> Result<Option<ProcessCpuTimes>> {
    if pid == 0 {
        return Ok(None);
    }
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => h,
        Err(_) => return Ok(None),
    };

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: handle valid; the four FILETIME out-params are valid pointers.
    let result =
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    close_handle(handle);

    match result {
        Ok(()) => Ok(Some(ProcessCpuTimes {
            kernel_100ns: filetime_to_u64(&kernel),
            user_100ns: filetime_to_u64(&user),
        })),
        Err(_) => Ok(None),
    }
}

fn filetime_to_u64(ft: &FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

fn close_handle(h: HANDLE) {
    // SAFETY: h is a handle we own; CloseHandle is idempotent on the same
    // handle (subsequent closes return an error we don't act on).
    let _ = unsafe { CloseHandle(h) };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// We're a running process — at minimum, our own PID must show up.
    #[test]
    fn snapshot_includes_self() {
        let pids = iter_pids().expect("snapshot a process list");
        let mine = std::process::id();
        assert!(
            pids.contains(&mine),
            "expected own pid {mine} in the snapshot of {} pids",
            pids.len()
        );
    }

    #[test]
    fn exe_for_self_matches_current_exe() {
        let mine = std::process::id();
        let resolved = exe_for_pid(mine).expect("resolve our own image path");
        let resolved = resolved.expect("self should always resolve");
        let cur = std::env::current_exe()
            .expect("current_exe")
            .to_string_lossy()
            .to_string();
        // Path equality is case-insensitive on Windows but the strings
        // sometimes differ by canonicalisation; compare the final component.
        let resolved_basename = resolved.rsplit('\\').next().unwrap_or("").to_lowercase();
        let cur_basename = cur.rsplit('\\').next().unwrap_or("").to_lowercase();
        assert_eq!(resolved_basename, cur_basename);
    }

    #[test]
    fn exe_for_pid_zero_is_none() {
        // System Idle can't be opened; we should return Ok(None) not panic.
        assert_eq!(exe_for_pid(0).unwrap(), None);
    }

    #[test]
    fn exe_for_obviously_nonexistent_pid_is_none() {
        // PIDs above 0xFFFF_0000 are well past any real Windows process.
        assert_eq!(exe_for_pid(0xFFFE_0000).unwrap(), None);
    }
}
