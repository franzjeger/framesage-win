//! Per-process I/O priority via `NtSetInformationProcess(ProcessIoPriority, …)`.
//!
//! The kernel32 surface only exposes `ProcessMemoryPriority` / a few other
//! `PROCESS_INFORMATION_CLASS` values — I/O priority sits behind the NT layer.
//! This is documented well enough that Process Lasso, Sysinternals, and
//! defrag.exe all use it, and (importantly for this project) it does not put
//! us on any anti-cheat watch list: every commercial anti-cheat tolerates it
//! because Windows itself uses it for background defragging and search
//! indexing.
//!
//! We bind the two NT functions we need with a plain `extern "system"` block
//! linked to `ntdll.dll` so we don't take a dependency on the much larger
//! `windows::Wdk` namespace. The integer values for `ProcessIoPriority` (33)
//! and `IO_PRIORITY_HINT` (0..=4) come from the public WDK headers
//! (`ntddk.h`, `ntpoapi.h`) and have been stable since Vista.

use anyhow::{anyhow, Result};
use std::mem::size_of;

use windows::Win32::Foundation::{HANDLE, NTSTATUS};

use framesage_core::IoPriority;

/// The `ProcessIoPriority` member of NT's `PROCESSINFOCLASS`. Value is stable
/// since Vista; matches `IO_PRIORITY_HINT` semantics.
const PROCESS_IO_PRIORITY: u32 = 33;

#[link(name = "ntdll")]
extern "system" {
    fn NtSetInformationProcess(
        ProcessHandle: HANDLE,
        ProcessInformationClass: u32,
        ProcessInformation: *const core::ffi::c_void,
        ProcessInformationLength: u32,
    ) -> NTSTATUS;

    fn NtQueryInformationProcess(
        ProcessHandle: HANDLE,
        ProcessInformationClass: u32,
        ProcessInformation: *mut core::ffi::c_void,
        ProcessInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> NTSTATUS;
}

fn to_hint(prio: IoPriority) -> u32 {
    // Values per public WDK `IO_PRIORITY_HINT`: VeryLow=0, Low=1, Normal=2,
    // High=3, Critical=4. Critical is reserved for the OS itself (paging
    // path); we accept it from the profile but a real kernel will refuse to
    // promote a normal user process that high.
    match prio {
        IoPriority::VeryLow => 0,
        IoPriority::Low => 1,
        IoPriority::Normal => 2,
        IoPriority::High => 3,
        IoPriority::Critical => 4,
    }
}

fn from_hint(raw: u32) -> Option<IoPriority> {
    match raw {
        0 => Some(IoPriority::VeryLow),
        1 => Some(IoPriority::Low),
        2 => Some(IoPriority::Normal),
        3 => Some(IoPriority::High),
        4 => Some(IoPriority::Critical),
        _ => None,
    }
}

/// Read the current I/O priority of `handle`. The process must be opened with
/// `PROCESS_QUERY_INFORMATION` (or `PROCESS_QUERY_LIMITED_INFORMATION`).
pub fn get(handle: HANDLE) -> Result<IoPriority> {
    let mut raw: u32 = 0;
    let mut returned: u32 = 0;
    // SAFETY: out pointer + matching length, handle assumed valid by caller.
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            PROCESS_IO_PRIORITY,
            &mut raw as *mut u32 as *mut core::ffi::c_void,
            size_of::<u32>() as u32,
            &mut returned,
        )
    };
    if status.0 < 0 {
        return Err(anyhow!(
            "NtQueryInformationProcess(ProcessIoPriority) failed: NTSTATUS 0x{:08x}",
            status.0
        ));
    }
    from_hint(raw).ok_or_else(|| anyhow!("kernel returned unknown IO_PRIORITY_HINT {raw}"))
}

/// Set the I/O priority of `handle`. The process must be opened with
/// `PROCESS_SET_INFORMATION`.
pub fn set(handle: HANDLE, prio: IoPriority) -> Result<()> {
    let value: u32 = to_hint(prio);
    // SAFETY: in pointer + matching length, handle assumed valid by caller.
    let status = unsafe {
        NtSetInformationProcess(
            handle,
            PROCESS_IO_PRIORITY,
            &value as *const u32 as *const core::ffi::c_void,
            size_of::<u32>() as u32,
        )
    };
    if status.0 < 0 {
        return Err(anyhow!(
            "NtSetInformationProcess(ProcessIoPriority) failed: NTSTATUS 0x{:08x}",
            status.0
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SET_INFORMATION,
    };

    /// Round-trip the I/O priority on the current process: read the kernel
    /// default, set Low, read back, restore. Exercises the real NT calls so
    /// it catches signature drift in `ntdll` bindings on supported Windows
    /// builds (10/11 desktop SKUs all support ProcessIoPriority).
    #[test]
    fn round_trip_on_current_process() {
        // SAFETY: returns a pseudo-handle that doesn't need closing.
        let pseudo = unsafe { GetCurrentProcess() };
        // GetCurrentProcess returns a pseudo-handle (-1); NtQuery accepts it
        // directly, but we open a real handle to mirror how `apply.rs` will
        // hand us one, which exercises the same code path.
        let pid = std::process::id();
        // SAFETY: documented call.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_INFORMATION | PROCESS_SET_INFORMATION,
                false,
                pid,
            )
        }
        .expect("OpenProcess on self should always succeed");

        let original = get(handle).expect("query our own I/O priority");
        set(handle, IoPriority::Low).expect("set our own I/O priority to Low");
        let after_set = get(handle).expect("re-query after set");
        assert_eq!(after_set, IoPriority::Low, "kernel accepted Low");

        // Restore for hygiene.
        set(handle, original).expect("restore original I/O priority");
        let _ = pseudo; // keep `GetCurrentProcess` referenced in this test.
    }
}
