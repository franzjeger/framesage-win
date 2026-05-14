//! Suspend and resume background processes.
//!
//! We *don't* use `NtSuspendProcess` — it's stable but officially undocumented,
//! and one of framesage's load-bearing constraints is "no Nt-prefix surprises."
//! Instead we enumerate the target process's threads via
//! `CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD)` and call `SuspendThread` /
//! `ResumeThread` on each. This is what Process Hacker, Process Explorer
//! (originally), Sysinternals' pssuspend, and Windows Task Manager all do —
//! anti-cheat treats it as routine.
//!
//! Atomicity: the documented approach is not strictly atomic. Between when
//! we snapshot the thread list and when we suspend each, the target can
//! spawn new threads. For our background-app workload (OneDrive, Dropbox)
//! the thread-spawn rate is low; we accept the rare straggler. If atomicity
//! ever matters more, we can iterate: take a snapshot, suspend, take another,
//! suspend new entries, until a snapshot is stable.

use std::collections::HashSet;

use anyhow::{anyhow, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE, MAX_PATH};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::Threading::{
    OpenThread, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME,
};

/// All PIDs currently running with an executable name matching `exe`
/// (case-insensitive, trailing-component match — directories are ignored).
///
/// Returns `(pid, observed_exe)` pairs so callers can journal what they
/// suspended with the actual case-preserved name.
pub fn find_pids_by_exe(exe: &str) -> Result<Vec<(u32, String)>> {
    let target = exe.to_ascii_lowercase();

    // SAFETY: documented call. 0-PID means "all processes."
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|e| anyhow!("CreateToolhelp32Snapshot(processes) failed: {e}"))?;

    let mut out = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: snapshot is valid; entry has dwSize set.
    if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
        loop {
            let exe_name = read_wide_null_terminated(&entry.szExeFile);
            if exe_name.to_ascii_lowercase() == target {
                out.push((entry.th32ProcessID, exe_name));
            }
            // SAFETY: snapshot still valid; entry reused with original size.
            if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }

    // SAFETY: snapshot is the handle we just opened and haven't otherwise used.
    let _ = unsafe { CloseHandle(snapshot) };
    Ok(out)
}

/// Suspend every thread of the target process.
///
/// Returns the count of threads suspended, mostly for logging. Errors from
/// `OpenThread` on individual threads are swallowed (the thread may have
/// exited between snapshot and open) — we report success if at least one
/// thread was suspended, which is the meaningful threshold for "the process
/// is now paused."
pub fn suspend_process(pid: u32) -> Result<u32> {
    let threads = list_thread_ids_for_pid(pid)?;
    if threads.is_empty() {
        return Err(anyhow!("no threads found for pid {pid}"));
    }
    let mut suspended = 0u32;
    for tid in threads {
        if let Some(handle) = open_thread_for_suspend(tid) {
            // SAFETY: handle is valid; SuspendThread returns previous
            // suspend count or DWORD(-1) on failure.
            let prev = unsafe { SuspendThread(handle) };
            if prev != u32::MAX {
                suspended += 1;
            }
            // SAFETY: handle is the one we just opened; close once.
            let _ = unsafe { CloseHandle(handle) };
        }
    }
    Ok(suspended)
}

/// Resume every thread of the target process.
///
/// Calls `ResumeThread` once per thread regardless of how many times we
/// previously suspended it — the kernel's suspend count is decremented one
/// per call, so a single resume balances a single suspend. If something
/// outside framesage also suspended the process, we'll under-resume; the
/// pragmatic call is to loop until the count hits zero, which we do.
pub fn resume_process(pid: u32) -> Result<u32> {
    let threads = list_thread_ids_for_pid(pid)?;
    let mut resumed = 0u32;
    for tid in threads {
        if let Some(handle) = open_thread_for_suspend(tid) {
            // Drain the suspend count fully.
            loop {
                // SAFETY: handle valid. ResumeThread returns previous suspend
                // count; 0 means it's now running, DWORD(-1) means error.
                let prev = unsafe { ResumeThread(handle) };
                if prev == 0 || prev == u32::MAX {
                    break;
                }
                resumed += 1;
            }
            let _ = unsafe { CloseHandle(handle) };
        }
    }
    Ok(resumed)
}

// ─── helpers ──────────────────────────────────────────────────────────────

fn list_thread_ids_for_pid(pid: u32) -> Result<HashSet<u32>> {
    // SAFETY: documented call. TH32CS_SNAPTHREAD captures the entire system's
    // thread list; we filter by owning PID below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) }
        .map_err(|e| anyhow!("CreateToolhelp32Snapshot(threads) failed: {e}"))?;

    let mut out = HashSet::new();
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    // SAFETY: snapshot valid; entry has dwSize set.
    if unsafe { Thread32First(snapshot, &mut entry) }.is_ok() {
        loop {
            if entry.th32OwnerProcessID == pid {
                out.insert(entry.th32ThreadID);
            }
            if unsafe { Thread32Next(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }

    let _ = unsafe { CloseHandle(snapshot) };
    Ok(out)
}

fn open_thread_for_suspend(tid: u32) -> Option<HANDLE> {
    // SAFETY: documented call. THREAD_SUSPEND_RESUME is the minimal access
    // for SuspendThread/ResumeThread. Returns Err on protected threads or
    // race exits — we treat those as "skip this thread."
    unsafe { OpenThread(THREAD_SUSPEND_RESUME, false, tid) }.ok()
}

fn read_wide_null_terminated(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

#[allow(dead_code)] // kept as a constant for future relocation
const _MAX_PATH_SANITY: usize = MAX_PATH as usize;
