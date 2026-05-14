//! Detect CPU topology via `GetLogicalProcessorInformationEx`, then enrich
//! each logical CPU with a coarse perf rank from `CallNtPowerInformation` and
//! the L3 cache size servicing its CCD.
//!
//! Notes on what this code does and doesn't do today:
//!
//! * **Processor groups.** Windows splits >64-logical-processor systems into
//!   "processor groups." Threadripper PRO 96-core counts. We currently flatten
//!   group 0 only — anything beyond is a v0.2 concern.
//! * **CPPC ranks.** We read `PROCESSOR_POWER_INFORMATION` via
//!   `CallNtPowerInformation(ProcessorInformation, …)` and use `MaxMhz` as the
//!   per-CPU rank. On AMD Ryzen `MaxMhz` is the *ACPI base spec* — uniform
//!   across both CCDs on a 9950X3D, so it's useless for X3D detection (we
//!   verified this on real hardware). On Intel hybrid the same field does
//!   discriminate P vs E cores. We populate it anyway so `TopRanked(N)` has
//!   data to work with where it can.
//! * **L3 cache size per CCD.** The reliable X3D signal. The X3D CCD's L3 is
//!   ~3x the non-X3D CCD's (96 MB vs 32 MB). We enumerate L3 caches via
//!   `GetLogicalProcessorInformationEx(RelationCache, …)`, stamp each logical
//!   CPU with the size of its CCD's L3, and `retag_ccds_from_signals` uses
//!   the size differential to mark the larger-L3 CCD as `CoreKind::Cache`.
//! * **Intel hybrid (P/E split)** is *not* detected here yet — the proper
//!   signal is `PROCESSOR_RELATIONSHIP::EfficiencyClass`, a v0.2 task.

use anyhow::{anyhow, Result};
use tracing::{debug, warn};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, RelationCache, RelationProcessorCore, CACHE_RELATIONSHIP,
    LOGICAL_PROCESSOR_RELATIONSHIP, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};

use framesage_core::{CoreKind, CpuTopology, LogicalCpu};

use super::cppc;

/// Enumerate logical processors and their physical-core groupings, then enrich
/// each `LogicalCpu` with its CPPC rank (from `PROCESSOR_POWER_INFORMATION`)
/// and its CCD's L3 cache size (from `RelationCache` enumeration). Finally
/// `retag_ccds_from_signals` uses those signals to identify the X3D CCD.
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
                l3_cache_bytes: None,
                is_smt_sibling: sibling_idx > 0,
            });
            next_index += 1;
        }
    }

    // Enrich with per-CPU MaxMhz. The CPPC readout is a single syscall that
    // returns one record per logical processor in Windows-index order; we
    // tolerate a failure here because the topology is still usable (selectors
    // that don't reference TopRanked / Kind(Cache) keep working) and we don't
    // want a stale Windows build to brick the service.
    match cppc::read(cpus.len()) {
        Ok(power_info) => {
            for cpu in &mut cpus {
                if let Some(info) = power_info.get(cpu.index as usize) {
                    if info.MaxMhz > 0 {
                        cpu.cppc_rank = Some(info.MaxMhz);
                    }
                }
            }
            debug!(
                cpus = cpus.len(),
                "cppc ranks populated from PROCESSOR_POWER_INFORMATION"
            );
        }
        Err(e) => {
            warn!(error = %e, "CPPC readout failed; TopRanked will degrade");
        }
    }

    // Enrich with per-CPU L3 cache size. This is the load-bearing signal for
    // X3D detection on AMD; rank alone is insufficient (see module docs).
    match enumerate_l3_caches() {
        Ok(l3_caches) => {
            for (mask, size) in &l3_caches {
                for cpu in &mut cpus {
                    if mask & (1u64 << cpu.index) != 0 {
                        cpu.l3_cache_bytes = Some(*size);
                    }
                }
            }
            debug!(
                caches = l3_caches.len(),
                "L3 cache sizes populated from RelationCache enumeration"
            );
        }
        Err(e) => {
            warn!(error = %e, "L3 cache enumeration failed; X3D detection will fall back to CPPC ranks");
        }
    }

    let mut topo = CpuTopology { cpus };
    topo.retag_ccds_from_signals();
    Ok(topo)
}

struct PhysicalCore {
    /// Logical processor indices (within group 0) that share this core.
    logical_processors: Vec<u32>,
}

/// Walk `GetLogicalProcessorInformationEx(RelationCache, …)` and return one
/// `(group0_mask, size_bytes)` for each L3 cache the OS reports.
///
/// We only consider group 0 for parity with the rest of this module; multi-
/// group machines lose their other groups' caches here (same caveat as core
/// enumeration).
fn enumerate_l3_caches() -> Result<Vec<(u64, u32)>> {
    let mut buf_size: u32 = 0;
    // SAFETY: null buffer + 0 size; documented to return ERROR_INSUFFICIENT_BUFFER
    // with the required size in `buf_size`.
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationCache, None, &mut buf_size) };
    if buf_size == 0 {
        return Err(anyhow!(
            "GetLogicalProcessorInformationEx(RelationCache) returned zero size"
        ));
    }

    let mut buf: Vec<u8> = vec![0; buf_size as usize];
    // SAFETY: buf has buf_size bytes; the API writes a sequence of variable-
    // length records.
    unsafe {
        GetLogicalProcessorInformationEx(
            RelationCache,
            Some(buf.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX),
            &mut buf_size,
        )
    }
    .map_err(|e| anyhow!("GetLogicalProcessorInformationEx(RelationCache) failed: {e}"))?;

    let mut caches = Vec::new();
    let mut offset = 0usize;
    while offset < buf_size as usize {
        // SAFETY: `offset` is in bounds; each record's `Size` field tells us
        // how many bytes it occupies.
        let info = unsafe {
            &*(buf.as_ptr().add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX)
        };
        let size = info.Size as usize;
        if size == 0 || offset + size > buf_size as usize {
            break;
        }
        if info.Relationship == LOGICAL_PROCESSOR_RELATIONSHIP(RelationCache.0) {
            // SAFETY: we just checked `Relationship == RelationCache`.
            let cache: CACHE_RELATIONSHIP = unsafe { info.Anonymous.Cache };
            if cache.Level == 3 {
                // SAFETY: GroupMask is valid for at least one entry (the
                // single-group variant). Multi-group L3 doesn't exist on
                // current consumer silicon — if it ever does we lose the
                // other groups here, same scope cut as enumerate_cores.
                let mask = unsafe { cache.Anonymous.GroupMask };
                if mask.Group == 0 {
                    caches.push((mask.Mask as u64, cache.CacheSize));
                }
            }
        }
        offset += size;
    }

    Ok(caches)
}

fn enumerate_cores() -> Result<Vec<PhysicalCore>> {
    let mut buf_size: u32 = 0;
    // First call sizes the buffer.
    // SAFETY: passing a null ptr with a 0 size; documented to return
    // ERROR_INSUFFICIENT_BUFFER with the required size in `buf_size`.
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut buf_size) };
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
            let groups =
                unsafe { std::slice::from_raw_parts(proc_info.GroupMask.as_ptr(), group_count) };
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
