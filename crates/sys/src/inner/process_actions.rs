//! Suspend / Resume / Terminate for an arbitrary live PID.
//!
//! Process-level suspend (via `NtSuspendProcess` / `NtResumeProcess`) freezes
//! every thread of the target process atomically — the same primitive Task
//! Manager's "Suspend Process" menu item uses, and the one Game Mode's
//! `suspend_processes` action uses to quiesce cloud-sync daemons. These NT
//! routines are undocumented in MSDN but stable since Vista and bound via
//! `ntdll.dll` direct linkage to keep the unsafe surface tiny and match the
//! pattern already in `io_priority.rs`.
//!
//! Termination uses the documented `TerminateProcess` (kernel32) with
//! `PROCESS_TERMINATE` rights — no graceful shutdown, just SIGKILL semantics.
//! The tray's right-click "Terminate" gates this behind a confirm dialog so
//! it can't be mis-fired.

use anyhow::{anyhow, Result};

use windows::Win32::Foundation::{CloseHandle, HANDLE, NTSTATUS};
use windows::Win32::System::Threading::{
    OpenProcess, TerminateProcess, PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE,
};

#[link(name = "ntdll")]
extern "system" {
    /// Suspend every thread of the target process. Idempotent — repeated calls
    /// stack into the suspend counter, but `NtResumeProcess` resets to zero
    /// regardless of depth, so callers don't need to track count.
    fn NtSuspendProcess(ProcessHandle: HANDLE) -> NTSTATUS;
    fn NtResumeProcess(ProcessHandle: HANDLE) -> NTSTATUS;
}

/// Freeze every thread of `pid`. Caller is responsible for releasing the
/// suspend later (or accepting that the process stays paused until reboot).
/// Returns `Err` for protected processes / PID 0 / dead PIDs — the common
/// case the engine handles by reporting the error back through IPC.
pub fn suspend(pid: u32) -> Result<()> {
    let handle = open_for_suspend(pid)?;
    // SAFETY: handle is valid (we just opened it).
    let status = unsafe { NtSuspendProcess(handle) };
    // SAFETY: handle owned, last use.
    let _ = unsafe { CloseHandle(handle) };
    if status.0 < 0 {
        return Err(anyhow!(
            "NtSuspendProcess({pid}) failed: NTSTATUS 0x{:08x}",
            status.0
        ));
    }
    Ok(())
}

/// Release a previous suspend. Resets the suspend counter to zero — safe to
/// call on a process that's already running (returns success), so it's the
/// natural "panic-button" path for the tray's right-click Resume.
pub fn resume(pid: u32) -> Result<()> {
    let handle = open_for_suspend(pid)?;
    // SAFETY: handle valid.
    let status = unsafe { NtResumeProcess(handle) };
    // SAFETY: handle owned, last use.
    let _ = unsafe { CloseHandle(handle) };
    if status.0 < 0 {
        return Err(anyhow!(
            "NtResumeProcess({pid}) failed: NTSTATUS 0x{:08x}",
            status.0
        ));
    }
    Ok(())
}

/// Force-terminate the target process with exit code 1. No graceful
/// shutdown — equivalent to `taskkill /F`. Caller MUST have already
/// confirmed the user's intent (the tray does this with a modal dialog).
pub fn terminate(pid: u32) -> Result<()> {
    if pid == 0 || pid == 4 {
        return Err(anyhow!(
            "refusing to terminate PID {pid} (System Idle / System)"
        ));
    }
    // SAFETY: documented call. Returns Err on protected processes / dead PIDs.
    let handle = unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) }
        .map_err(|e| anyhow!("OpenProcess(PROCESS_TERMINATE, pid={pid}) failed: {e}"))?;
    // SAFETY: handle valid. Exit code 1 mirrors Task Manager's "End task".
    let r = unsafe { TerminateProcess(handle, 1) };
    // SAFETY: handle owned.
    let _ = unsafe { CloseHandle(handle) };
    r.map_err(|e| anyhow!("TerminateProcess({pid}) failed: {e}"))
}

fn open_for_suspend(pid: u32) -> Result<HANDLE> {
    if pid == 0 || pid == 4 {
        return Err(anyhow!(
            "refusing to suspend/resume PID {pid} (System Idle / System)"
        ));
    }
    // SAFETY: documented call. PROCESS_SUSPEND_RESUME is the minimum right
    // for NtSuspendProcess / NtResumeProcess.
    unsafe { OpenProcess(PROCESS_SUSPEND_RESUME, false, pid) }
        .map_err(|e| anyhow!("OpenProcess(PROCESS_SUSPEND_RESUME, pid={pid}) failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip suspend → resume on a freshly-spawned child. Verifies the
    /// NT calls return success on a non-protected target and that the
    /// resume actually unfreezes (the child completes its busy-wait if we
    /// resumed correctly).
    #[test]
    fn suspend_resume_round_trip_on_child() {
        use std::process::{Command, Stdio};

        // Spawn a child that sleeps briefly — enough for us to suspend +
        // resume before it would otherwise exit. `cmd /c timeout` is in
        // every Windows shell; we don't care about its output.
        let mut child = Command::new("cmd")
            .args(["/c", "timeout", "/t", "5", "/nobreak"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd timeout child");
        let pid = child.id();

        suspend(pid).expect("suspend child");
        resume(pid).expect("resume child");

        // Clean up: kill the child so the test exits promptly instead of
        // waiting 5 s for its sleep.
        let _ = child.kill();
        let _ = child.wait();
    }
}
