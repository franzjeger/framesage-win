//! Apply a `Profile` to a process — and remember enough to revert it.
//!
//! Every knob is opened, read, written, and the previous value is stored in
//! `AppliedState`. `revert()` walks `AppliedState` and restores. This is
//! deliberately non-transactional: if applying knob 3 fails after knobs 1 and 2
//! succeeded, we keep what we have and return an error. `revert` is best-effort
//! by design — the OS state is the ground truth, not our record of it.

use anyhow::{anyhow, Context, Result};
use std::mem::size_of;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows::Win32::System::SystemInformation::{
    GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
};
use windows::Win32::System::Threading::{
    GetPriorityClass, GetProcessAffinityMask, GetProcessInformation, OpenProcess, OpenThread,
    ProcessMemoryPriority, ProcessPowerThrottling, SetPriorityClass, SetProcessAffinityMask,
    SetProcessDefaultCpuSets, SetProcessInformation, SetThreadSelectedCpuSets,
    ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS,
    IDLE_PRIORITY_CLASS, MEMORY_PRIORITY, MEMORY_PRIORITY_INFORMATION, NORMAL_PRIORITY_CLASS,
    PROCESS_CREATION_FLAGS, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    PROCESS_SET_LIMITED_INFORMATION, THREAD_SET_LIMITED_INFORMATION,
};

use framesage_core::{
    CpuTopology, IoPriority, MemoryPriority, PowerThrottlingMode, PriorityClass, Profile,
};

use super::io_priority;

/// Captures what we changed on the target process, so revert can restore it.
/// `None` for a field means "we didn't touch this knob — leave the previous
/// value alone on revert."
#[derive(Debug, Default)]
pub struct AppliedState {
    prev_priority_class: Option<u32>,
    prev_affinity_mask: Option<usize>,
    prev_power_throttling: Option<PROCESS_POWER_THROTTLING_STATE>,
    prev_memory_priority: Option<MEMORY_PRIORITY_INFORMATION>,
    /// CPU Sets are reverted by passing an empty array — that resets the
    /// process to the system default. We don't currently snapshot the prior
    /// CPU Sets list (would need to round-trip the Vec<u32>); revert clears.
    cpu_sets_set: bool,
    /// Previous `IO_PRIORITY_HINT` for the process before we touched it,
    /// captured via `NtQueryInformationProcess(ProcessIoPriority)`. None means
    /// we left the I/O priority alone.
    prev_io_priority: Option<IoPriority>,
}

pub fn apply(pid: u32, profile: &Profile, topology: &CpuTopology) -> Result<AppliedState> {
    let handle = open_for_write(pid)?;
    let mut state = AppliedState::default();

    // The order matters slightly: do throttling/priority before CPU Sets, so
    // the scheduler picks up the new policy when CPU Sets land. Working-set
    // trim last — once everything else is in place.

    if let Some(class) = profile.priority_class {
        state.prev_priority_class = Some(get_priority_class(handle)?);
        set_priority_class(handle, class).context("set priority class")?;
    }

    if let Some(mode) = profile.power_throttling {
        state.prev_power_throttling = Some(get_power_throttling(handle).unwrap_or(
            PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: 0,
                StateMask: 0,
            },
        ));
        set_power_throttling(handle, mode).context("set power throttling")?;
    }

    if let Some(prio) = profile.memory_priority {
        state.prev_memory_priority = get_memory_priority(handle).ok();
        set_memory_priority(handle, prio).context("set memory priority")?;
    }

    if let Some(prio) = profile.io_priority {
        // Best-effort read of the prior value so revert can restore it. If the
        // query fails (rare — usually permission), fall back to Normal on
        // revert; that's the kernel default for almost every user process.
        state.prev_io_priority = Some(io_priority::get(handle).unwrap_or(IoPriority::Normal));
        io_priority::set(handle, prio).context("set I/O priority")?;
    }

    if let Some(sel) = &profile.cpu_sets {
        let indices = topology.resolve(sel);
        let set_ids = cpuset_ids_for_indices(&indices)?;

        // Three layers, belt + suspenders + safety net:
        //
        //   1. SetProcessDefaultCpuSets — soft hint for threads created
        //      AFTER this call.
        //   2. SetThreadSelectedCpuSets per existing thread — soft hint
        //      for the threads the process already spawned (a game's
        //      threadpool is built at startup, so without this we miss
        //      ~all of it).
        //   3. SetProcessAffinityMask — HARD pin to the same set of
        //      logical CPUs, atomic across existing + future threads.
        //      We save the prior mask under prev_affinity_mask so revert
        //      restores it.
        //
        // The README's "CPU Sets, not affinity, to avoid starvation"
        // stance was right in theory but didn't survive contact with
        // hardware: hardware validation showed games spawning across
        // all cores even with sets applied because the soft hint isn't
        // enforced under load. The X3D CCD has 16 logical CPUs — more
        // than enough headroom for any single game — so the starvation
        // concern is theoretical. We apply the hard mask too.
        //
        // If the user explicitly sets profile.affinity_mask, that wins
        // (overwrites our default mask below).
        set_default_cpu_sets(handle, &set_ids).context("set default CPU sets")?;
        let n = apply_thread_cpu_sets(pid, &set_ids);
        tracing::debug!(pid, threads = n, "applied per-thread CPU sets");

        let hard_mask = mask_from_indices(&indices);
        if hard_mask != 0 {
            state.prev_affinity_mask = Some(get_affinity_mask(handle)?);
            set_affinity_mask(handle, hard_mask).context("set hard affinity from cpu_sets")?;
            tracing::debug!(
                pid,
                mask = format!("{hard_mask:#x}"),
                "applied hard affinity"
            );
        }

        state.cpu_sets_set = true;
    }

    if let Some(sel) = &profile.affinity_mask {
        let indices = topology.resolve(sel);
        let mask = mask_from_indices(&indices);
        state.prev_affinity_mask = Some(get_affinity_mask(handle)?);
        set_affinity_mask(handle, mask).context("set affinity")?;
    }

    if profile.trim_working_set {
        // SAFETY: handle is valid.
        let _ = unsafe { K32EmptyWorkingSet(handle) };
    }

    // SAFETY: we just used handle and won't again.
    let _ = unsafe { CloseHandle(handle) };
    Ok(state)
}

pub fn revert(pid: u32, state: AppliedState) -> Result<()> {
    // Try to reopen the process. If it's gone (PID reused / exited), that's
    // fine — nothing to revert. Use a more permissive open since we might
    // need both set and limited-info flavours depending on which knob we
    // need to restore.
    let handle = match open_for_write(pid) {
        Ok(h) => h,
        Err(_) => return Ok(()),
    };

    if let Some(class_raw) = state.prev_priority_class {
        let class = PROCESS_CREATION_FLAGS(class_raw);
        // SAFETY: handle valid, class is a documented constant.
        if let Err(e) = unsafe { SetPriorityClass(handle, class) } {
            warn_revert(pid, "SetPriorityClass", e);
        }
    }

    if let Some(mask) = state.prev_affinity_mask {
        // SAFETY: handle valid.
        if let Err(e) = unsafe { SetProcessAffinityMask(handle, mask) } {
            warn_revert(pid, "SetProcessAffinityMask", e);
        }
    }

    if let Some(prev) = state.prev_power_throttling {
        let res = unsafe {
            SetProcessInformation(
                handle,
                ProcessPowerThrottling,
                &prev as *const _ as *const _,
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        };
        if let Err(e) = res {
            warn_revert(pid, "SetProcessInformation(PowerThrottling)", e);
        }
    }

    if let Some(prev) = state.prev_memory_priority {
        let res = unsafe {
            SetProcessInformation(
                handle,
                ProcessMemoryPriority,
                &prev as *const _ as *const _,
                size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
            )
        };
        if let Err(e) = res {
            warn_revert(pid, "SetProcessInformation(MemoryPriority)", e);
        }
    }

    if state.cpu_sets_set {
        // Empty array resets the process default to system default. Then
        // walk threads and clear their per-thread overrides too so they
        // pick up the new process default. Without the thread sweep,
        // threads we constrained on apply would stay pinned even after
        // the process default reverts.
        if let Err(e) = unsafe { SetProcessDefaultCpuSets(handle, None) }.ok() {
            warn_revert(pid, "SetProcessDefaultCpuSets(None)", e);
        }
        let cleared = apply_thread_cpu_sets(pid, &[]);
        tracing::debug!(pid, threads = cleared, "cleared per-thread CPU sets");
    }

    if let Some(prio) = state.prev_io_priority {
        if let Err(e) = io_priority::set(handle, prio) {
            warn_revert(pid, "NtSetInformationProcess(ProcessIoPriority)", e);
        }
    }

    // SAFETY: handle valid, last use.
    let _ = unsafe { CloseHandle(handle) };
    Ok(())
}

fn warn_revert(pid: u32, operation: &str, err: impl std::fmt::Display) {
    tracing::warn!(pid, %err, "revert: {operation} failed");
}

// ─── helpers ──────────────────────────────────────────────────────────────

fn open_for_write(pid: u32) -> Result<HANDLE> {
    let rights = PROCESS_QUERY_INFORMATION
        | PROCESS_QUERY_LIMITED_INFORMATION
        | PROCESS_SET_INFORMATION
        | PROCESS_SET_LIMITED_INFORMATION;
    // SAFETY: documented call. Returns Err on failure (e.g. protected
    // process, insufficient privilege).
    unsafe { OpenProcess(rights, false, pid) }
        .map_err(|e| anyhow!("OpenProcess({pid}) for write failed: {e}"))
}

fn get_priority_class(handle: HANDLE) -> Result<u32> {
    // SAFETY: handle is valid.
    let v = unsafe { GetPriorityClass(handle) };
    if v == 0 {
        Err(anyhow!("GetPriorityClass returned 0"))
    } else {
        Ok(v)
    }
}

fn set_priority_class(handle: HANDLE, class: PriorityClass) -> Result<()> {
    let class_const = match class {
        PriorityClass::Idle => IDLE_PRIORITY_CLASS,
        PriorityClass::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        PriorityClass::Normal => NORMAL_PRIORITY_CLASS,
        PriorityClass::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
        PriorityClass::High => HIGH_PRIORITY_CLASS,
    };
    // SAFETY: handle valid.
    unsafe { SetPriorityClass(handle, class_const) }
        .map_err(|e| anyhow!("SetPriorityClass failed: {e}"))
}

fn get_affinity_mask(handle: HANDLE) -> Result<usize> {
    let mut process_mask: usize = 0;
    let mut system_mask: usize = 0;
    // SAFETY: handle valid, out params are valid.
    unsafe { GetProcessAffinityMask(handle, &mut process_mask, &mut system_mask) }
        .map_err(|e| anyhow!("GetProcessAffinityMask failed: {e}"))?;
    Ok(process_mask)
}

fn set_affinity_mask(handle: HANDLE, mask: usize) -> Result<()> {
    // SAFETY: handle valid.
    unsafe { SetProcessAffinityMask(handle, mask) }
        .map_err(|e| anyhow!("SetProcessAffinityMask failed: {e}"))
}

fn mask_from_indices(indices: &[u32]) -> usize {
    let mut m: usize = 0;
    for &i in indices {
        if i < usize::BITS {
            m |= 1usize << i;
        }
    }
    m
}

fn set_power_throttling(handle: HANDLE, mode: PowerThrottlingMode) -> Result<()> {
    let (control, state) = match mode {
        PowerThrottlingMode::Eco => (
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        ),
        PowerThrottlingMode::Performance => (PROCESS_POWER_THROTTLING_EXECUTION_SPEED, 0),
        PowerThrottlingMode::SystemDefault => (0, 0),
    };
    let info = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: control,
        StateMask: state,
    };
    // SAFETY: handle valid, pointer + size correct for ProcessPowerThrottling.
    unsafe {
        SetProcessInformation(
            handle,
            ProcessPowerThrottling,
            &info as *const _ as *const _,
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .map_err(|e| anyhow!("SetProcessInformation(PowerThrottling) failed: {e}"))
}

fn get_power_throttling(handle: HANDLE) -> Result<PROCESS_POWER_THROTTLING_STATE> {
    let mut info = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: 0,
        StateMask: 0,
    };
    // SAFETY: handle valid, out ptr and size correct.
    unsafe {
        GetProcessInformation(
            handle,
            ProcessPowerThrottling,
            &mut info as *mut _ as *mut _,
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    }
    .map_err(|e| anyhow!("GetProcessInformation(PowerThrottling) failed: {e}"))?;
    Ok(info)
}

fn set_memory_priority(handle: HANDLE, prio: MemoryPriority) -> Result<()> {
    let info = MEMORY_PRIORITY_INFORMATION {
        MemoryPriority: MEMORY_PRIORITY(prio.as_u32()),
    };
    // SAFETY: handle valid, ptr + size correct.
    unsafe {
        SetProcessInformation(
            handle,
            ProcessMemoryPriority,
            &info as *const _ as *const _,
            size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
    }
    .map_err(|e| anyhow!("SetProcessInformation(MemoryPriority) failed: {e}"))
}

fn get_memory_priority(handle: HANDLE) -> Result<MEMORY_PRIORITY_INFORMATION> {
    let mut info = MEMORY_PRIORITY_INFORMATION {
        MemoryPriority: MEMORY_PRIORITY(0),
    };
    // SAFETY: handle valid, ptr + size correct.
    unsafe {
        GetProcessInformation(
            handle,
            ProcessMemoryPriority,
            &mut info as *mut _ as *mut _,
            size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
        )
    }
    .map_err(|e| anyhow!("GetProcessInformation(MemoryPriority) failed: {e}"))?;
    Ok(info)
}

fn set_default_cpu_sets(handle: HANDLE, set_ids: &[u32]) -> Result<()> {
    let slice: Option<&[u32]> = if set_ids.is_empty() {
        None
    } else {
        Some(set_ids)
    };
    // SAFETY: handle valid; SetProcessDefaultCpuSets accepts None (clear) or
    // a slice of CPU set IDs. Returns BOOL; convert to Result via `.ok()`.
    unsafe { SetProcessDefaultCpuSets(handle, slice) }
        .ok()
        .map_err(|e| anyhow!("SetProcessDefaultCpuSets failed: {e}"))
}

/// Convert logical-CPU indices into Windows CPU-set IDs.
///
/// Why this isn't just `(idx + 0x100)`: while the kernel happens to assign IDs
/// sequentially today, that's not documented. The robust path is to enumerate
/// via `GetSystemCpuSetInformation` and read each entry's `LogicalProcessorIndex`
/// → `Id` mapping. We cache nothing here yet (it's a hundred-byte syscall on
/// startup-class frequency).
fn cpuset_ids_for_indices(indices: &[u32]) -> Result<Vec<u32>> {
    if indices.is_empty() {
        return Ok(Vec::new());
    }

    let mut buf_size: u32 = 0;
    // First call sizes the buffer.
    // SAFETY: null buffer + 0 size returns ERROR_INSUFFICIENT_BUFFER and sets
    // the required size. We deliberately ignore the result and read the size.
    let _ = unsafe { GetSystemCpuSetInformation(None, 0, &mut buf_size, None, 0) };
    if buf_size == 0 {
        return Err(anyhow!(
            "GetSystemCpuSetInformation returned zero buffer size"
        ));
    }

    let mut buf: Vec<u8> = vec![0; buf_size as usize];
    // SAFETY: buffer is exactly buf_size bytes. Returns BOOL.
    unsafe {
        GetSystemCpuSetInformation(
            Some(buf.as_mut_ptr() as *mut SYSTEM_CPU_SET_INFORMATION),
            buf_size,
            &mut buf_size,
            None,
            0,
        )
    }
    .ok()
    .map_err(|e| anyhow!("GetSystemCpuSetInformation failed: {e}"))?;

    let mut out = Vec::with_capacity(indices.len());
    let mut offset = 0usize;
    while offset + size_of::<SYSTEM_CPU_SET_INFORMATION>() <= buf_size as usize {
        // SAFETY: offset is in bounds.
        let info = unsafe { &*(buf.as_ptr().add(offset) as *const SYSTEM_CPU_SET_INFORMATION) };
        let size = info.Size as usize;
        if size == 0 {
            break;
        }
        // The CpuSet union variant carries Id + LogicalProcessorIndex + …
        // SAFETY: variant union access; we don't reach for other tags.
        let cpu = unsafe { info.Anonymous.CpuSet };
        if indices.contains(&(cpu.LogicalProcessorIndex as u32)) {
            out.push(cpu.Id);
        }
        offset += size;
    }

    Ok(out)
}

/// Walk every thread owned by `pid` and call `SetThreadSelectedCpuSets`
/// with the given CPU-set ids. Passing an empty slice clears the
/// per-thread override and lets each thread fall back to the process
/// default (which the caller should have just reset / set on its own).
///
/// Returns the number of threads we successfully called the API on.
/// Threads we fail to open (mostly: the thread exited between
/// enumeration and OpenThread, or it's protected) are silently skipped —
/// per-thread enforcement is best-effort, the process default handles
/// any thread we miss.
///
/// Why this matters: `SetProcessDefaultCpuSets` only affects threads
/// created AFTER the call. A game with a long-lived worker threadpool
/// (most modern engines) ends up running its existing workers on every
/// core because they were spawned before framesage's apply. Per-thread
/// CPU-set application closes that gap without resorting to hard
/// affinity masks.
fn apply_thread_cpu_sets(pid: u32, set_ids: &[u32]) -> usize {
    // SAFETY: documented call. Returns INVALID_HANDLE_VALUE on failure
    // (we treat it as "no threads enumerated" — count stays 0).
    let snap = match unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) } {
        Ok(h) => h,
        Err(_) => return 0,
    };

    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut count = 0usize;

    // SAFETY: snap is a valid snapshot; dwSize initialised.
    if unsafe { Thread32First(snap, &mut entry) }.is_ok() {
        loop {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: documented call. THREAD_SET_LIMITED_INFORMATION is
                // enough for SetThreadSelectedCpuSets; if the open fails (most
                // commonly: thread exited between snapshot and now) we just
                // skip — not worth log spam for the dozens of races a busy
                // process will produce.
                if let Ok(th) =
                    unsafe { OpenThread(THREAD_SET_LIMITED_INFORMATION, false, entry.th32ThreadID) }
                {
                    // SAFETY: th is valid; set_ids points to a possibly-empty
                    // u32 slice; the API accepts count == 0 to clear.
                    if unsafe { SetThreadSelectedCpuSets(th, set_ids) }.as_bool() {
                        count += 1;
                    }
                    // SAFETY: th was just opened by us.
                    let _ = unsafe { CloseHandle(th) };
                }
            }
            // SAFETY: snap valid; entry reused as documented.
            if unsafe { Thread32Next(snap, &mut entry) }.is_err() {
                break;
            }
        }
    }

    // SAFETY: snap valid, last use.
    let _ = unsafe { CloseHandle(snap) };
    count
}
