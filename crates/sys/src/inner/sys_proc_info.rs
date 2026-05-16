//! `NtQuerySystemInformation(SystemProcessInformation)` — one syscall,
//! all processes. Item 2.1 / audit C-1 + C-2.
//!
//! Before this module, `Engine::list_process_snapshots` opened a per-PID
//! handle 5 times (priority, affinity, memory, CPU times, exe path) and
//! `Engine::maybe_run_probalance_locked` opened it 3 times (cpu_times,
//! exe_for_pid, priority class). On a typical 250-PID box that was
//! ~750–1250 OpenProcess/CloseHandle pairs per second. The audit ranked
//! this as the largest single footprint contributor.
//!
//! `NtQuerySystemInformation(SystemProcessInformation)` returns
//! per-process CPU times, image name, thread count, working set,
//! private bytes, and handle count for every visible PID in a single
//! kernel call. No per-PID handle needed. The result is a linked-list
//! of variable-length structs (each entry's `NextEntryOffset` points
//! to the next), so we walk it once and copy out the fields we want
//! into a clean owned `Vec<SysProcInfo>`.
//!
//! The remaining per-PID OpenProcess hits in the engine cover fields
//! NTQSI doesn't expose:
//! * **Full image path** (NTQSI gives bare filename only). Still uses
//!   `QueryFullProcessImageNameW`.
//! * **Affinity mask** — not in SYSTEM_PROCESS_INFORMATION.
//! * **User SID** — token-bound, not a process-info field.
//!
//! Those stay on the legacy per-PID API with their existing budgets
//! (icons / user lookups cap at 8/tick). The hot path — every PID
//! every tick — is now a single syscall.

use std::mem::size_of;
use std::time::Duration;

use anyhow::{anyhow, Result};

use windows::core::PWSTR;
use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
use windows::Win32::Foundation::{STATUS_INFO_LENGTH_MISMATCH, UNICODE_STRING};
use windows::Win32::System::WindowsProgramming::SYSTEM_PROCESS_INFORMATION;

/// One process from the NTQSI snapshot. Only the fields the engine
/// actually consumes — the kernel returns more (paged-pool usage,
/// hard-fault counts, etc.) but we ignore the rest to keep the
/// allocation small and the struct readable.
#[derive(Debug, Clone)]
pub struct SysProcInfo {
    pub pid: u32,
    /// Parent PID (`InheritedFromUniqueProcessId` in the kernel struct).
    /// Stored as a u64 because the kernel slot is a HANDLE on x64.
    pub parent_pid: u32,
    /// Image filename only (no path). UNICODE_STRING from NTQSI is
    /// already just the filename — `notepad.exe`, not the full path.
    pub exe_name: String,
    pub thread_count: u32,
    pub handle_count: u32,
    /// Sum of kernel + user CPU time, in 100-ns units (same units as
    /// `GetProcessTimes`). Lets the engine subtract from a prior
    /// sample to compute CPU% over the inter-sample window.
    pub total_cpu_100ns: u64,
    /// Current resident set, bytes.
    pub working_set_bytes: u64,
    /// Highest working set this process has reached.
    pub peak_working_set_bytes: u64,
    /// Committed private bytes (closest single number to "how much
    /// RAM this process is responsible for"). `PrivatePageCount` from
    /// the kernel struct, in pages × page_size. We expose the byte
    /// value to mirror the existing `MemoryInfo::private_bytes`.
    pub private_bytes: u64,
    /// Kernel KPRIORITY (0–31) — base scheduling priority. NOT the
    /// Win32 priority class constant. Callers that need the Win32
    /// class can map via `kpriority_to_win32_class`.
    pub base_priority: i32,
}

/// Run NTQSI once, returning every visible process.
///
/// Implementation: buffer-resize loop. Start at 1 MB; on
/// `STATUS_INFO_LENGTH_MISMATCH` double the buffer and retry. The
/// kernel writes the actual required size into the `return_length`
/// out-param, but doubling avoids tight retry loops when many PIDs
/// spawn between calls.
///
/// Failures bubble up as `anyhow::Error` so the engine can fall back
/// to the per-PID path; this is a soft-required dependency, not a
/// hard one.
pub fn enumerate_processes() -> Result<Vec<SysProcInfo>> {
    // Buffer size grows on STATUS_INFO_LENGTH_MISMATCH. 1 MB covers
    // ~600–800 processes (typical entry is 256-512 bytes + thread
    // array we ignore); a busy CI / dev box with 1000+ PIDs hits 2 MB
    // on retry. Capped at 64 MB to bound the worst case — a system
    // with 64k+ processes is broken; bailing with an error is correct.
    const INITIAL_SIZE: usize = 1024 * 1024;
    const MAX_SIZE: usize = 64 * 1024 * 1024;

    // `vec![0u8; INITIAL_SIZE]` is what clippy wants over the
    // `with_capacity` + `resize` pair — same result, cleaner generated
    // code (one calloc instead of malloc + memset).
    let mut buf: Vec<u8> = vec![0u8; INITIAL_SIZE];

    let mut return_length: u32 = 0;
    let mut attempts = 0;
    loop {
        attempts += 1;
        if buf.len() > MAX_SIZE {
            return Err(anyhow!(
                "NtQuerySystemInformation buffer exceeded {MAX_SIZE} bytes — abnormal process count"
            ));
        }
        // SAFETY: buf is a valid mutable byte slice owned for the
        // duration of the call; return_length is a valid out-param.
        // NtQuerySystemInformation never reads past
        // systeminformationlength, so length-mismatched calls are
        // safe (and the documented retry pattern).
        let status = unsafe {
            NtQuerySystemInformation(
                SystemProcessInformation,
                buf.as_mut_ptr() as *mut _,
                buf.len() as u32,
                &mut return_length,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH {
            // Grow and retry. Double + a slack pad for races: if more
            // processes spawned between probe and retry, we still fit.
            let new_size = (buf.len() * 2).max(return_length as usize + 64 * 1024);
            buf.resize(new_size, 0);
            continue;
        }
        if status.is_err() {
            return Err(anyhow!(
                "NtQuerySystemInformation failed: NTSTATUS={:#x} (attempts={attempts})",
                status.0,
            ));
        }
        break;
    }

    // Walk the linked list. Each entry's `NextEntryOffset` field is
    // bytes from the start of THAT entry to the next; 0 marks the
    // last entry. We bound-check every offset to defend against a
    // hypothetical malformed buffer.
    let mut out: Vec<SysProcInfo> = Vec::with_capacity(256);
    let mut cursor: usize = 0;
    let entry_size = size_of::<SYSTEM_PROCESS_INFORMATION>();
    loop {
        if cursor + entry_size > buf.len() {
            return Err(anyhow!(
                "NTQSI buffer truncated at cursor={cursor} (size={})",
                buf.len()
            ));
        }
        // SAFETY: cursor + entry_size <= buf.len(); the kernel
        // guarantees aligned, valid SYSTEM_PROCESS_INFORMATION at
        // every offset reachable by the NextEntryOffset chain. We
        // never write through this pointer — read-only copy.
        let entry: &SYSTEM_PROCESS_INFORMATION =
            unsafe { &*(buf.as_ptr().add(cursor) as *const SYSTEM_PROCESS_INFORMATION) };

        // Extract fields. The documented kernel layout puts UserTime
        // and KernelTime inside the windows-rs `Reserved1: [u8; 48]`
        // block (it's redacted in the public binding but the layout
        // is stable):
        //   Reserved1[0..8]   = WorkingSetPrivateSize (i64)
        //   Reserved1[8..12]  = HardFaultCount (u32)
        //   Reserved1[12..16] = NumberOfThreadsHighWatermark (u32)
        //   Reserved1[16..24] = CycleTime (u64)
        //   Reserved1[24..32] = CreateTime (i64)
        //   Reserved1[32..40] = UserTime (i64, 100-ns)
        //   Reserved1[40..48] = KernelTime (i64, 100-ns)
        let user_time_100ns = read_i64(&entry.Reserved1, 32) as u64;
        let kernel_time_100ns = read_i64(&entry.Reserved1, 40) as u64;
        let total_cpu_100ns = user_time_100ns.saturating_add(kernel_time_100ns);

        // Parent PID lives at `Reserved2` in the windows-rs binding
        // — actually it's `InheritedFromUniqueProcessId` per the
        // documented layout. The crate exposes it as
        // `*mut core::ffi::c_void`; we just want its low 32 bits as
        // a PID.
        let parent_pid = entry.Reserved2 as usize as u32;

        let pid = entry.UniqueProcessId.0 as usize as u32;
        // PID 0 = System Idle Process; PID 4 = System. We surface them
        // for completeness — the engine's own per-PID filtering already
        // skips PID 0/4 where needed.

        let exe_name = unicode_string_to_owned(&entry.ImageName);

        let info = SysProcInfo {
            pid,
            parent_pid,
            exe_name,
            thread_count: entry.NumberOfThreads,
            handle_count: entry.HandleCount,
            total_cpu_100ns,
            working_set_bytes: entry.WorkingSetSize as u64,
            peak_working_set_bytes: entry.PeakWorkingSetSize as u64,
            // PrivatePageCount is in pages on Vista+; multiply by
            // page size. We use 4 KB as the canonical page size
            // because Windows always reports private bytes against
            // 4 KB regardless of the actual hardware page (large
            // pages don't appear in this counter).
            private_bytes: (entry.PrivatePageCount as u64).saturating_mul(4096),
            base_priority: entry.BasePriority,
        };
        out.push(info);

        let next = entry.NextEntryOffset as usize;
        if next == 0 {
            break;
        }
        // The chain MUST move forward. A 0 offset means "last" (handled
        // above); any other value must be strictly positive.
        if next < entry_size {
            return Err(anyhow!(
                "NTQSI NextEntryOffset {next} < entry_size {entry_size} — kernel bug or malformed buffer"
            ));
        }
        cursor = cursor.saturating_add(next);
        if cursor >= buf.len() {
            // Done — the kernel sometimes points the last entry just
            // past the buffer end via NextEntryOffset; safer to break
            // than to walk off.
            break;
        }
    }

    Ok(out)
}

/// Map a kernel KPRIORITY value (0–31, what NTQSI gives) to the
/// Win32 priority class constant the rest of framesage uses
/// (`GetPriorityClass` return values). Item 2.1 — without this the
/// engine has to make an extra OpenProcess per PID just to get the
/// Win32 class.
///
/// Mapping is the standard one Microsoft documents for
/// process-base-priority quanta. Threads can deviate; for the
/// snapshot we want the process base, which IS the KPRIORITY in
/// `SYSTEM_PROCESS_INFORMATION`.
///
/// Returns 0 for unmapped values (caller treats as "unknown" — same
/// fallback as the existing `priority_class_raw: 0` semantic).
pub fn kpriority_to_win32_class(kpriority: i32) -> u32 {
    match kpriority {
        // IDLE (KPRIORITY 4) → 0x40
        4 => 0x0000_0040,
        // BELOW_NORMAL (KPRIORITY 6) → 0x4000
        6 => 0x0000_4000,
        // NORMAL (KPRIORITY 8) → 0x20
        8 => 0x0000_0020,
        // ABOVE_NORMAL (KPRIORITY 10) → 0x8000
        10 => 0x0000_8000,
        // HIGH (KPRIORITY 13) → 0x80
        13 => 0x0000_0080,
        // REALTIME (KPRIORITY 24) → 0x100
        24 => 0x0000_0100,
        _ => 0,
    }
}

/// `Duration` from a u64 100-ns count. Used by the engine when it
/// needs the elapsed wall-clock equivalent of an NTQSI CPU-time delta.
pub fn duration_from_100ns(units: u64) -> Duration {
    let nanos = units.saturating_mul(100);
    Duration::from_nanos(nanos)
}

/// Read an i64 at `offset` inside a `[u8; N]` array. Helper for the
/// Reserved1 byte-offset extraction in SYSTEM_PROCESS_INFORMATION.
/// The kernel guarantees 8-byte alignment of these fields within the
/// struct, but we use `read_unaligned` for safety — the cost
/// difference is negligible on x86_64 and the safety win is real if
/// the upstream layout ever shifts by a byte.
fn read_i64(buf: &[u8], offset: usize) -> i64 {
    debug_assert!(offset + 8 <= buf.len(), "Reserved1 i64 read out of bounds");
    let mut tmp = [0u8; 8];
    tmp.copy_from_slice(&buf[offset..offset + 8]);
    i64::from_le_bytes(tmp)
}

/// Convert a kernel `UNICODE_STRING` to an owned Rust `String`. The
/// kernel populates `Buffer` with a pointer into the NTQSI buffer
/// (not separately allocated), so we copy lazily on demand. NTQSI's
/// ImageName is the bare exe filename — `notepad.exe`, not the path.
///
/// Returns an empty string if Buffer is null or Length is 0. NTQSI
/// always populates these for normal processes; only the System Idle
/// Process (PID 0) typically has an empty ImageName.
fn unicode_string_to_owned(s: &UNICODE_STRING) -> String {
    if s.Buffer.is_null() || s.Length == 0 {
        return String::new();
    }
    let chars = (s.Length / 2) as usize; // Length is bytes; UTF-16 is 2 bytes/unit
                                         // SAFETY: Buffer is non-null, valid for `chars` u16s per the
                                         // kernel's UNICODE_STRING contract. We only read; never write.
    let slice = unsafe { std::slice::from_raw_parts(s.Buffer.0 as *const u16, chars) };
    String::from_utf16_lossy(slice)
}

// Silence unused-import warnings on the PWSTR import — we use it
// indirectly via UNICODE_STRING's `Buffer` field which is a PWSTR.
#[allow(dead_code)]
fn _unused() {
    let _: PWSTR = PWSTR::null();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: the call returns a populated list on a real
    /// Windows host. We don't assert specific PIDs (varies by box)
    /// but we expect to see at least our own test process and the
    /// System / System Idle entries.
    #[test]
    fn enumerate_returns_processes_including_self() {
        let processes = enumerate_processes().expect("NTQSI should succeed");
        assert!(
            processes.len() >= 4,
            "expected at least a few processes, got {}",
            processes.len()
        );
        let my_pid = std::process::id();
        let me = processes
            .iter()
            .find(|p| p.pid == my_pid)
            .expect("our own PID must appear in the enumeration");
        assert!(
            !me.exe_name.is_empty(),
            "our own exe name must be populated"
        );
        assert!(me.thread_count >= 1, "we have at least 1 thread");
    }

    #[test]
    fn kpriority_mapping_covers_standard_classes() {
        // Locks the standard kernel→Win32 mapping. If a Windows
        // update ever changes these (it won't, the contract is
        // ~30 years old), this test fails loudly.
        assert_eq!(kpriority_to_win32_class(4), 0x0000_0040); // Idle
        assert_eq!(kpriority_to_win32_class(8), 0x0000_0020); // Normal
        assert_eq!(kpriority_to_win32_class(13), 0x0000_0080); // High
        assert_eq!(kpriority_to_win32_class(24), 0x0000_0100); // Realtime
                                                               // Unmapped kernel priorities (e.g. a thread that's been
                                                               // boosted) come back as 0 — the existing "unknown" semantic.
        assert_eq!(kpriority_to_win32_class(7), 0);
        assert_eq!(kpriority_to_win32_class(31), 0);
    }

    #[test]
    fn duration_from_100ns_handles_typical_values() {
        // 10,000,000 × 100ns = 1 second
        assert_eq!(duration_from_100ns(10_000_000), Duration::from_secs(1));
        // u64::MAX should saturate, not overflow
        let _ = duration_from_100ns(u64::MAX);
    }

    #[test]
    fn read_i64_extracts_little_endian_value() {
        // 0x0807060504030201 in LE bytes: 01 02 03 04 05 06 07 08
        let buf: [u8; 48] = {
            let mut b = [0u8; 48];
            b[32..40].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
            b
        };
        assert_eq!(read_i64(&buf, 32), 0x0807_0605_0403_0201);
    }
}
