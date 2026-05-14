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
use windows::Win32::System::ProcessStatus::K32EmptyWorkingSet;
use windows::Win32::System::SystemInformation::{
    GetSystemCpuSetInformation, SYSTEM_CPU_SET_INFORMATION,
};
use windows::Win32::System::Threading::{
    GetPriorityClass, GetProcessAffinityMask, GetProcessInformation, OpenProcess,
    SetPriorityClass, SetProcessAffinityMask, SetProcessDefaultCpuSets, SetProcessInformation,
    ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, HIGH_PRIORITY_CLASS,
    IDLE_PRIORITY_CLASS, MEMORY_PRIORITY, MEMORY_PRIORITY_INFORMATION, NORMAL_PRIORITY_CLASS,
    PROCESS_CREATION_FLAGS, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
    PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
    PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    PROCESS_SET_LIMITED_INFORMATION, ProcessMemoryPriority, ProcessPowerThrottling,
};

use framesage_core::{
    CpuTopology, IoPriority, MemoryPriority, PowerThrottlingMode, PriorityClass, Profile,
};

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
    /// I/O priority isn't yet wired up (needs NtSetInformationProcess).
    /// Placeholder for v0.2.
    #[allow(dead_code)]
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
        state.prev_power_throttling = Some(get_power_throttling(handle).unwrap_or_else(|_| {
            PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: 0,
                StateMask: 0,
            }
        }));
        set_power_throttling(handle, mode).context("set power throttling")?;
    }

    if let Some(prio) = profile.memory_priority {
        state.prev_memory_priority = get_memory_priority(handle).ok();
        set_memory_priority(handle, prio).context("set memory priority")?;
    }

    if let Some(sel) = &profile.cpu_sets {
        let indices = topology.resolve(sel);
        let set_ids = cpuset_ids_for_indices(&indices)?;
        set_default_cpu_sets(handle, &set_ids).context("set default CPU sets")?;
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
        let _ = unsafe { SetPriorityClass(handle, class) };
    }

    if let Some(mask) = state.prev_affinity_mask {
        // SAFETY: handle valid.
        let _ = unsafe { SetProcessAffinityMask(handle, mask) };
    }

    if let Some(prev) = state.prev_power_throttling {
        let _ = unsafe {
            SetProcessInformation(
                handle,
                ProcessPowerThrottling,
                &prev as *const _ as *const _,
                size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
            )
        };
    }

    if let Some(prev) = state.prev_memory_priority {
        let _ = unsafe {
            SetProcessInformation(
                handle,
                ProcessMemoryPriority,
                &prev as *const _ as *const _,
                size_of::<MEMORY_PRIORITY_INFORMATION>() as u32,
            )
        };
    }

    if state.cpu_sets_set {
        // Empty array resets to system default.
        let _ = unsafe { SetProcessDefaultCpuSets(handle, None) };
    }

    // SAFETY: handle valid, last use.
    let _ = unsafe { CloseHandle(handle) };
    Ok(())
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
    let _ = unsafe {
        GetSystemCpuSetInformation(None, 0, &mut buf_size, None, 0)
    };
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
        let info = unsafe {
            &*(buf.as_ptr().add(offset) as *const SYSTEM_CPU_SET_INFORMATION)
        };
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
