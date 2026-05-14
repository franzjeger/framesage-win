//! Detect CPU topology via `GetLogicalProcessorInformationEx`.
//!
//! Notes on what this code does and doesn't do today:
//!
//! * **Processor groups.** Windows splits >64-logical-processor systems into
//!   "processor groups." Threadripper PRO 96-core counts. We currently flatten
//!   group 0 only — anything beyond is a v0.2 concern.
//! * **X3D / Cache CCD detection.** Windows doesn't tell us which CCD has 3D
//!   V-Cache. The reliable signal is CPPC perf-rank distribution: on a dual-CCD
//!   X3D part, the X3D CCD has lower top CPPC rank (lower max frequency). We
//!   keep that detection out of `detect()` and tag every CCD as `Performance`
//!   here; a higher layer can mark one CCD as `Cache` after looking at the
//!   ranks. This file's job is just to enumerate.
//! * **CPPC ranks.** Read via `Get-WmiObject Win32_Processor.NumberOfCores` /
//!   the perf-state-info MSR isn't exposed cleanly to user-mode. The proper
//!   path is `CallNtPowerInformation(ProcessorInformation, …)` which yields
//!   `PROCESSOR_POWER_INFORMATION` per logical CPU — that gives us
//!   `CurrentMhz` and `MaxMhz`. We use MaxMhz as a coarse rank proxy until we
//!   wire up the proper CPPC readout via Windows PPM perf-control MSRs (which
//!   needs the WMI provider `MSAcpi_ThermalZoneTemperature`-class trick or a
//!   small driver — out of scope for v0.1).

use anyhow::{anyhow, Result};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, LOGICAL_PROCESSOR_RELATIONSHIP, RelationProcessorCore,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};

use framesage_core::{CoreKind, CpuTopology, LogicalCpu};

/// Enumerate logical processors and their physical-core groupings.
///
/// The OS reports each physical core as one record with a bitmask of its
/// logical processors. We expand those into `LogicalCpu` entries and assign
/// CCD indices based on the order cores are reported (an imperfect heuristic —
/// see module docs).
pub fn detect() -> Result<CpuTopology> {
    let cores = enumerate_cores()?;

    // Group cores into "CCDs of 8" as a first cut. This matches Ryzen 7000/9000
    // CCDs and is wrong on Intel hybrid (where we *should* be reading the
    // SMT-mask + core-efficiency-class) — that branch lands in v0.2.
    const CORES_PER_CCD_HEURISTIC: usize = 8;

    let mut cpus = Vec::new();
    let mut next_index: u32 = 0;
    for (core_idx, core) in cores.iter().enumerate() {
        let ccd = (core_idx / CORES_PER_CCD_HEURISTIC) as u8;
        for (sibling_idx, _) in core.logical_processors.iter().enumerate() {
            cpus.push(LogicalCpu {
                index: next_index,
                physical_core: core_idx as u32,
                ccd,
                kind: CoreKind::Performance,
                cppc_rank: None,
                is_smt_sibling: sibling_idx > 0,
            });
            next_index += 1;
        }
    }

    Ok(CpuTopology { cpus })
}

struct PhysicalCore {
    /// Logical processor indices (within group 0) that share this core.
    logical_processors: Vec<u32>,
}

fn enumerate_cores() -> Result<Vec<PhysicalCore>> {
    let mut buf_size: u32 = 0;
    // First call sizes the buffer.
    // SAFETY: passing a null ptr with a 0 size; documented to return
    // ERROR_INSUFFICIENT_BUFFER with the required size in `buf_size`.
    let _ = unsafe {
        GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut buf_size)
    };
    if buf_size == 0 {
        return Err(anyhow!(
            "GetLogicalProcessorInformationEx returned zero size"
        ));
    }

    let mut buf: Vec<u8> = vec![0; buf_size as usize];
    // SAFETY: buf has buf_size bytes; the API will write a sequence of
    // variable-length records into it.
    unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buf.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX),
            &mut buf_size,
        )
    }
    .map_err(|e| anyhow!("GetLogicalProcessorInformationEx failed: {e}"))?;

    let mut cores = Vec::new();
    let mut offset = 0usize;
    while offset < buf_size as usize {
        // SAFETY: `offset` is within bounds of buf; each record's `Size` field
        // tells us how many bytes it occupies.
        let info = unsafe {
            &*(buf.as_ptr().add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)
        };
        let size = info.Size as usize;
        if size == 0 || offset + size > buf_size as usize {
            break;
        }
        if info.Relationship == LOGICAL_PROCESSOR_RELATIONSHIP(RelationProcessorCore.0) {
            // The Processor union variant gives us a GroupCount + array of
            // GROUP_AFFINITY. We only handle group 0 here.
            // SAFETY: we just checked `Relationship == RelationProcessorCore`.
            let proc_info = unsafe { info.Anonymous.Processor };
            let group_count = proc_info.GroupCount as usize;
            let mut logical = Vec::new();
            // SAFETY: GroupMask is a flexible array; we trust the
            // GroupCount-bounded slice given to us.
            let groups = unsafe {
                std::slice::from_raw_parts(proc_info.GroupMask.as_ptr(), group_count)
            };
            for g in groups {
                if g.Group != 0 {
                    continue; // multi-group not yet supported
                }
                let mask = g.Mask;
                for bit in 0..64 {
                    if mask & (1usize << bit) != 0 {
                        logical.push(bit as u32);
                    }
                }
            }
            if !logical.is_empty() {
                cores.push(PhysicalCore {
                    logical_processors: logical,
                });
            }
        }
        offset += size;
    }

    Ok(cores)
}
