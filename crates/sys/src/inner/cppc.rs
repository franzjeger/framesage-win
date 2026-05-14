//! CPPC perf-rank readout via `CallNtPowerInformation(ProcessorInformation, …)`.
//!
//! Windows surfaces a per-logical-CPU `PROCESSOR_POWER_INFORMATION` array that
//! includes `MaxMhz` — the maximum frequency the OS believes the CPU can hit.
//! This is *not* a real CPPC perf-class read (those live in MSRs that aren't
//! exposed cleanly to user-mode), but it's a high-quality coarse proxy:
//!
//! * On hybrid silicon, P-cores have higher `MaxMhz` than E-cores.
//! * On dual-CCD AMD parts, the non-X3D CCD has higher `MaxMhz` than the X3D
//!   CCD (the cache stack costs about 500 MHz of headroom).
//!
//! That's the signal `CpuSelector::TopRanked(N)` needs to pin to the actual
//! fastest cores on this specific chip, and the signal `CpuTopology::
//! retag_ccds_from_ranks` uses to detect which CCD carries the 3D V-Cache.

use anyhow::{anyhow, Context, Result};
use windows::Win32::System::Power::{
    CallNtPowerInformation, ProcessorInformation, PROCESSOR_POWER_INFORMATION,
};

/// Read `PROCESSOR_POWER_INFORMATION` for every logical CPU the OS knows about.
///
/// `expected_count` is the number of logical processors we just enumerated via
/// `GetLogicalProcessorInformationEx`. We size the output buffer to match; the
/// kernel writes exactly one record per logical CPU it sees.
pub fn read(expected_count: usize) -> Result<Vec<PROCESSOR_POWER_INFORMATION>> {
    if expected_count == 0 {
        return Ok(Vec::new());
    }
    let mut buf: Vec<PROCESSOR_POWER_INFORMATION> =
        vec![PROCESSOR_POWER_INFORMATION::default(); expected_count];
    let byte_len = expected_count
        .checked_mul(std::mem::size_of::<PROCESSOR_POWER_INFORMATION>())
        .ok_or_else(|| anyhow!("CPU count {expected_count} overflows buffer size"))?;
    let byte_len_u32: u32 = byte_len
        .try_into()
        .with_context(|| format!("CPU buffer size {byte_len} too large for u32"))?;

    // SAFETY: passing a correctly-sized output buffer, no input buffer needed
    // for ProcessorInformation. NTSTATUS is checked via `.ok()`.
    let status = unsafe {
        CallNtPowerInformation(
            ProcessorInformation,
            None,
            0,
            Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
            byte_len_u32,
        )
    };
    status
        .ok()
        .map_err(|e| anyhow!("CallNtPowerInformation(ProcessorInformation) failed: {e}"))?;

    Ok(buf)
}
