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
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, INVALID_HANDLE_VALUE, MAX_PATH,
};

use crate::owned_handle::OwnedHandle;
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

/// Tuple of `(pid, parent_pid, thread_count)` snapshot from a single
/// ToolHelp pass. Cheaper than calling `iter_pids` then opening each PID
/// for thread count + parent separately — ToolHelp populates both
/// (`th32ParentProcessID`, `cntThreads`) for free.
#[derive(Debug, Clone, Copy)]
pub struct PidSnapshot {
    pub pid: u32,
    /// PID of the process that created this one. `0` for orphan / root
    /// processes (System Idle, or processes whose parent has exited). Note
    /// the kernel does not update this when a parent exits — a fresh
    /// `notepad.exe` spawned by `explorer.exe`, after explorer is killed,
    /// still reports the explorer PID until the kernel reaps it. Treated
    /// as an orphan by the tree-builder if no live process matches.
    pub parent_pid: u32,
    pub thread_count: u32,
}

/// Snapshot every running process plus its thread count + parent PID via
/// ToolHelp. The thread count and parent come from the same struct
/// ToolHelp populates for `iter_pids` — no extra OpenProcess required,
/// which matters when the Processes tab re-snapshots ~200 processes
/// every second.
pub fn iter_pid_snapshots() -> Result<Vec<PidSnapshot>> {
    let snap_raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|e| anyhow!("CreateToolhelp32Snapshot failed: {e}"))?;
    if snap_raw == INVALID_HANDLE_VALUE {
        return Err(anyhow!(
            "CreateToolhelp32Snapshot returned INVALID_HANDLE_VALUE"
        ));
    }
    let snap = OwnedHandle::assume_valid(snap_raw);
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut out = Vec::with_capacity(256);
    let first_ok = unsafe { Process32FirstW(snap.as_raw(), &mut entry) }.is_ok();
    if first_ok {
        out.push(PidSnapshot {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            thread_count: entry.cntThreads,
        });
        while unsafe { Process32NextW(snap.as_raw(), &mut entry) }.is_ok() {
            out.push(PidSnapshot {
                pid: entry.th32ProcessID,
                parent_pid: entry.th32ParentProcessID,
                thread_count: entry.cntThreads,
            });
        }
    }
    // snap drops here, closing the snapshot.
    Ok(out)
}

/// Snapshot every running process and return their PIDs.
///
/// Includes PID 0 (System Idle) and PID 4 (System) — callers that want only
/// user-mode processes should filter, since neither can be opened with
/// `PROCESS_QUERY_LIMITED_INFORMATION`.
pub fn iter_pids() -> Result<Vec<u32>> {
    // SAFETY: documented call. Returns INVALID_HANDLE_VALUE on failure.
    let snap_raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|e| anyhow!("CreateToolhelp32Snapshot failed: {e}"))?;
    if snap_raw == INVALID_HANDLE_VALUE {
        return Err(anyhow!(
            "CreateToolhelp32Snapshot returned INVALID_HANDLE_VALUE"
        ));
    }
    let snap = OwnedHandle::assume_valid(snap_raw);

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    let mut pids = Vec::with_capacity(256);

    // SAFETY: snap is a valid snapshot handle from CreateToolhelp32Snapshot.
    // dwSize is initialised. First and Next are documented to return BOOL.
    let first_ok = unsafe { Process32FirstW(snap.as_raw(), &mut entry) }.is_ok();
    if first_ok {
        pids.push(entry.th32ProcessID);
        // SAFETY: entry has been populated by Process32FirstW; reusing for Next
        // is the documented pattern.
        while unsafe { Process32NextW(snap.as_raw(), &mut entry) }.is_ok() {
            pids.push(entry.th32ProcessID);
        }
    }

    // snap drops here, closing the snapshot.
    Ok(pids)
}

/// Full image path of a running process, or `None` if it has exited / is
/// inaccessible (protected processes, anti-cheat-protected processes, PID 0).
pub fn exe_for_pid(pid: u32) -> Result<Option<String>> {
    if pid == 0 {
        // System Idle Process can't be opened.
        return Ok(None);
    }
    // SAFETY: documented call. OwnedHandle (item 3.3) closes the
    // handle on every return path.
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => OwnedHandle::assume_valid(h),
        // Item 4.8 — classify so a genuinely unexpected error
        // (something other than PID-exited / protected-process)
        // surfaces instead of being mis-classified as a routine
        // race.
        Err(e) => {
            return match classify_open_process_error(&e) {
                OpenProcessClass::Exited | OpenProcessClass::AccessDenied => Ok(None),
                OpenProcessClass::Unexpected => Err(anyhow!(
                    "OpenProcess({pid}) for exe lookup failed unexpectedly: {e}"
                )),
            };
        }
    };

    let mut buf = [0u16; MAX_PATH as usize];
    let mut size = buf.len() as u32;
    // SAFETY: handle valid; buf + size valid out params.
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle.as_raw(),
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    // handle drops at end of scope.

    match result {
        Ok(()) => Ok(Some(String::from_utf16_lossy(&buf[..size as usize]))),
        Err(_) => Ok(None),
    }
}

/// Resolve the user that owns a running process, formatted as
/// `"DOMAIN\\username"` (or just `"username"` for a missing domain).
///
/// Chain: `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` →
/// `OpenProcessToken(TOKEN_QUERY)` → `GetTokenInformation(TokenUser)` to
/// extract the SID → `LookupAccountSidW` to map SID → name+domain. Same
/// chain Task Manager / Process Explorer use for their User column;
/// anti-cheat-clean (read-only token query).
///
/// Returns `Ok(None)` for:
///   * PID 0 (System Idle — can't be opened)
///   * Protected / anti-cheat-protected processes (`OpenProcess` denies)
///   * Processes that exited between snapshot and query
///   * Cases where `LookupAccountSidW` can't resolve the SID (well-known
///     SIDs like the special "Console Logon" appliance SIDs sometimes
///     fail; we treat that as "no resolvable user" rather than an error).
pub fn user_for_pid(pid: u32) -> Result<Option<String>> {
    use std::ffi::c_void;
    use windows::Win32::Security::{
        GetTokenInformation, LookupAccountSidW, TokenUser, SID_NAME_USE, TOKEN_QUERY, TOKEN_USER,
    };
    use windows::Win32::System::Threading::OpenProcessToken;

    if pid == 0 {
        return Ok(None);
    }
    // SAFETY: documented call. Returns Err for denied / nonexistent PIDs.
    // OwnedHandle (item 3.3) closes both the process and token handles
    // on every return path.
    let proc_handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => OwnedHandle::assume_valid(h),
        // Item 4.8 — classify Expected vs Unexpected.
        Err(e) => {
            return match classify_open_process_error(&e) {
                OpenProcessClass::Exited | OpenProcessClass::AccessDenied => Ok(None),
                OpenProcessClass::Unexpected => Err(anyhow!(
                    "OpenProcess({pid}) for user lookup failed unexpectedly: {e}"
                )),
            };
        }
    };

    let mut token_raw: HANDLE = HANDLE::default();
    // SAFETY: proc_handle is valid; documented call.
    let token_result =
        unsafe { OpenProcessToken(proc_handle.as_raw(), TOKEN_QUERY, &mut token_raw) };
    // Don't need proc_handle past this point — let it drop early.
    drop(proc_handle);
    if token_result.is_err() {
        return Ok(None);
    }
    let token = OwnedHandle::assume_valid(token_raw);

    // GetTokenInformation: classic two-call dance for buffer sizing.
    let mut needed: u32 = 0;
    // SAFETY: NULL buffer + 0 size deliberately triggers
    // ERROR_INSUFFICIENT_BUFFER and writes the required size into `needed`.
    let _ = unsafe { GetTokenInformation(token.as_raw(), TokenUser, None, 0, &mut needed) };
    if needed == 0 {
        return Ok(None);
    }
    let mut buf = vec![0u8; needed as usize];
    // SAFETY: buf has exactly `needed` bytes.
    let result = unsafe {
        GetTokenInformation(
            token.as_raw(),
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            needed,
            &mut needed,
        )
    };
    // token drops at end of scope.
    if result.is_err() {
        return Ok(None);
    }

    // The first sizeof(TOKEN_USER) bytes of `buf` are a TOKEN_USER struct;
    // its `User.Sid` field points into the same buffer.
    // SAFETY: GetTokenInformation wrote a TOKEN_USER at offset 0.
    let token_user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let sid = token_user.User.Sid;
    if sid.is_invalid() {
        return Ok(None);
    }

    // LookupAccountSidW: another two-call dance — first query needed
    // buffer sizes for name + domain. The W variant takes raw `PWSTR`
    // (not `Option<PWSTR>`), so we pass `PWSTR::null()` to elicit the
    // ERROR_INSUFFICIENT_BUFFER probe.
    let mut name_len: u32 = 0;
    let mut domain_len: u32 = 0;
    let mut sid_use = SID_NAME_USE::default();
    // SAFETY: null name/domain buffers force size return via
    // Err(InsufficientBuffer); name_len + domain_len are valid out-params.
    let _ = unsafe {
        LookupAccountSidW(
            windows::core::PCWSTR::null(),
            sid,
            PWSTR::null(),
            &mut name_len,
            PWSTR::null(),
            &mut domain_len,
            &mut sid_use,
        )
    };
    if name_len == 0 {
        // SID didn't resolve to a known account. Fall back to the SID
        // string form ("S-1-5-18", "S-1-5-21-…") so we at least surface
        // *something* — better than rendering "—" when the row is e.g.
        // a SYSTEM service.
        return Ok(sid_to_string(sid));
    }

    let mut name_buf = vec![0u16; name_len as usize];
    let mut domain_buf = vec![0u16; domain_len as usize];
    // SAFETY: both buffers sized per the previous probe call.
    let result = unsafe {
        LookupAccountSidW(
            windows::core::PCWSTR::null(),
            sid,
            PWSTR(name_buf.as_mut_ptr()),
            &mut name_len,
            PWSTR(domain_buf.as_mut_ptr()),
            &mut domain_len,
            &mut sid_use,
        )
    };
    if result.is_err() {
        return Ok(sid_to_string(sid));
    }

    // LookupAccountSidW writes name_len / domain_len as the actual char
    // counts (NOT including the null terminator) on success.
    let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
    let domain = if domain_len > 0 {
        String::from_utf16_lossy(&domain_buf[..domain_len as usize])
    } else {
        String::new()
    };

    let combined = if domain.is_empty() {
        name
    } else {
        format!("{domain}\\{name}")
    };
    if combined.is_empty() {
        Ok(None)
    } else {
        Ok(Some(combined))
    }
}

/// Format a SID as its canonical string ("S-1-5-18" for LocalSystem etc.)
/// via `ConvertSidToStringSidW`. Used as a fallback display when
/// `LookupAccountSidW` can't resolve the SID to a name (rare; happens
/// for unusual capability SIDs on AppContainer / UWP processes).
fn sid_to_string(sid: windows::Win32::Security::PSID) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;

    let mut out: PWSTR = PWSTR::null();
    // SAFETY: sid is valid (checked by caller); out is a valid out-param.
    let result = unsafe { ConvertSidToStringSidW(sid, &mut out) };
    if result.is_err() || out.is_null() {
        return None;
    }
    // SAFETY: out is a null-terminated wide string allocated by LocalAlloc.
    let len = unsafe {
        let mut n = 0usize;
        while *out.0.add(n) != 0 {
            n += 1;
        }
        n
    };
    // SAFETY: out points to `len` u16 + null.
    let slice = unsafe { std::slice::from_raw_parts(out.0, len) };
    let s = String::from_utf16_lossy(slice);
    // SAFETY: out was returned by the API and must be freed with LocalFree.
    let _ = unsafe { LocalFree(windows::Win32::Foundation::HLOCAL(out.0 as _)) };
    Some(s)
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
        Ok(h) => OwnedHandle::assume_valid(h),
        // Item 4.8 — classify Expected vs Unexpected.
        Err(e) => {
            return match classify_open_process_error(&e) {
                OpenProcessClass::Exited | OpenProcessClass::AccessDenied => Ok(None),
                OpenProcessClass::Unexpected => Err(anyhow!(
                    "OpenProcess({pid}) for cpu times failed unexpectedly: {e}"
                )),
            };
        }
    };

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: handle valid; the four FILETIME out-params are valid pointers.
    let result = unsafe {
        GetProcessTimes(
            handle.as_raw(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };
    // handle drops at end of scope.

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

/// Live working-set size in bytes via `GetProcessMemoryInfo`. `None` if the
/// PID is gone or we can't open it for query. Thin shim over `memory_info`
/// — preserved as the fast-path for callers that only need working-set
/// (e.g. ProBalance's per-tick sample loop, which doesn't need peak or
/// private byte counts).
pub fn working_set_bytes(pid: u32) -> Result<Option<u64>> {
    Ok(memory_info(pid)?.map(|m| m.working_set_bytes))
}

/// Multi-field memory snapshot for one process. All values in bytes.
///
/// Sourced from `GetProcessMemoryInfo` populating
/// `PROCESS_MEMORY_COUNTERS_EX` — the EX variant adds `PrivateUsage`
/// (committed-private-bytes) to what the base struct already carries.
/// Process Lasso's Memory column shows working-set live, and its hover
/// expands to peak + private; we mirror that.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryInfo {
    /// Resident working set right now. Drops when the OS trims the
    /// process under pressure; rises when the process touches pages.
    pub working_set_bytes: u64,
    /// Highest working set the process has ever reached this session.
    /// A growing peak-vs-current gap is the classic memory-leak signal.
    pub peak_working_set_bytes: u64,
    /// Committed private bytes (memory uniquely owned by this process —
    /// not file-mapped, not shared). The closest single number to "how
    /// much RAM this process is responsible for."
    pub private_bytes: u64,
}

/// One-shot multi-field memory snapshot. `None` for PID 0 or for
/// processes the engine can't open (protected, exited).
pub fn memory_info(pid: u32) -> Result<Option<MemoryInfo>> {
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    if pid == 0 {
        return Ok(None);
    }
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => OwnedHandle::assume_valid(h),
        // Item 4.8 — classify Expected vs Unexpected.
        Err(e) => {
            return match classify_open_process_error(&e) {
                OpenProcessClass::Exited | OpenProcessClass::AccessDenied => Ok(None),
                OpenProcessClass::Unexpected => Err(anyhow!(
                    "OpenProcess({pid}) for memory info failed unexpectedly: {e}"
                )),
            };
        }
    };

    // The EX variant is layout-compatible with the base — its first
    // members match `PROCESS_MEMORY_COUNTERS`, with `PrivateUsage`
    // appended at the end. `GetProcessMemoryInfo` accepts a
    // `*mut PROCESS_MEMORY_COUNTERS` and writes up to `cb` bytes.
    let mut ex = PROCESS_MEMORY_COUNTERS_EX::default();
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    // SAFETY: ex is a valid PROCESS_MEMORY_COUNTERS_EX; cast to the base
    // pointer type is layout-safe per Win32 docs; size matches the EX
    // struct so the kernel knows to populate PrivateUsage.
    let r = unsafe {
        GetProcessMemoryInfo(
            handle.as_raw(),
            &mut ex as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            size,
        )
    };
    // handle drops at end of scope.
    match r {
        Ok(()) => Ok(Some(MemoryInfo {
            working_set_bytes: ex.WorkingSetSize as u64,
            peak_working_set_bytes: ex.PeakWorkingSetSize as u64,
            private_bytes: ex.PrivateUsage as u64,
        })),
        Err(_) => Ok(None),
    }
}

/// Cumulative system CPU times (idle / kernel / user) in 100-ns units.
/// Used to compute system-wide CPU% by taking two samples and looking at
/// `1 - delta_idle / delta_total`. `GetSystemTimes` is documented and
/// cheaper than spinning up a PDH counter — exactly what Task Manager uses.
#[derive(Debug, Clone, Copy)]
pub struct SystemCpuTimes {
    pub idle_100ns: u64,
    pub kernel_100ns: u64,
    pub user_100ns: u64,
}

impl SystemCpuTimes {
    /// `kernel` from `GetSystemTimes` includes idle. The "busy" portion is
    /// `kernel + user - idle`. Total wall time across all CPUs is the same
    /// `kernel + user` value (kernel includes idle by Microsoft's
    /// convention). Both delta forms are useful: use this to get busy.
    pub fn busy_100ns(&self) -> u64 {
        self.kernel_100ns
            .saturating_add(self.user_100ns)
            .saturating_sub(self.idle_100ns)
    }
    /// Total CPU-time accounted for (across all logical processors).
    pub fn total_100ns(&self) -> u64 {
        self.kernel_100ns.saturating_add(self.user_100ns)
    }
}

/// Sample `GetSystemTimes`. The caller stores the previous sample and
/// computes `busy / total` from the delta to get a system CPU% in 0-100.
pub fn system_cpu_times() -> Result<SystemCpuTimes> {
    use windows::Win32::System::Threading::GetSystemTimes;
    let mut idle = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: documented call. All three out-params are valid FILETIME ptrs.
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }
        .map_err(|e| anyhow!("GetSystemTimes failed: {e}"))?;
    Ok(SystemCpuTimes {
        idle_100ns: filetime_to_u64(&idle),
        kernel_100ns: filetime_to_u64(&kernel),
        user_100ns: filetime_to_u64(&user),
    })
}

/// CPU times for a single logical processor. Layout-compatible with the
/// fields of `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION` that we care about;
/// the rest of that struct (`DpcTime`, `InterruptTime`, `InterruptCount`)
/// would be useful for a future "interrupt-storm" indicator but is dropped
/// for now to keep the IPC payload tight.
#[derive(Debug, Clone, Copy, Default)]
pub struct PerCpuTimes {
    pub idle_100ns: u64,
    /// `KernelTime` from the NT struct. Microsoft's convention: this INCLUDES
    /// `IdleTime`. The "busy" portion is `kernel + user - idle`.
    pub kernel_100ns: u64,
    pub user_100ns: u64,
}

impl PerCpuTimes {
    pub fn busy_100ns(&self) -> u64 {
        self.kernel_100ns
            .saturating_add(self.user_100ns)
            .saturating_sub(self.idle_100ns)
    }
    pub fn total_100ns(&self) -> u64 {
        self.kernel_100ns.saturating_add(self.user_100ns)
    }
}

/// Layout of `SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION` (one element per
/// logical CPU). Fields and order from public WDK headers (`ntexapi.h`);
/// stable since Windows XP. Padded so the struct is 8-aligned, matching
/// what the kernel writes.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SystemProcessorPerformanceInformation {
    idle_time: i64,
    kernel_time: i64,
    user_time: i64,
    dpc_time: i64,
    interrupt_time: i64,
    interrupt_count: u32,
    _padding: u32,
}

/// `SYSTEM_INFORMATION_CLASS::SystemProcessorPerformanceInformation` — value
/// from public WDK headers. Stable since XP.
const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: u32 = 8;

#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(
        SystemInformationClass: u32,
        SystemInformation: *mut core::ffi::c_void,
        SystemInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> windows::Win32::Foundation::NTSTATUS;
}

/// Sample CPU times for every logical processor.
///
/// Implementation: `NtQuerySystemInformation(SystemProcessorPerformanceInformation)`.
/// Same call Task Manager, Process Explorer, and every Windows process
/// viewer uses for per-core CPU%. Documented well enough in public WDK
/// headers (and stable for two decades) that anti-cheats coexist with it.
///
/// Strategy: we don't know the logical-CPU count a priori, so we do the
/// classic two-call dance — first call with a small buffer to read the
/// required length, then a second call with the real buffer. Both calls
/// are cheap (< 100µs on a typical box). Capped at 256 logical CPUs to
/// keep the worst-case allocation bounded.
pub fn per_cpu_times() -> Result<Vec<PerCpuTimes>> {
    const STRIDE: usize = std::mem::size_of::<SystemProcessorPerformanceInformation>();
    const MAX_CPUS: usize = 256;

    // First call: small buffer just to learn the size. NtQuerySystemInformation
    // returns STATUS_INFO_LENGTH_MISMATCH (0xC0000004) in this case, which we
    // ignore — we only care about ReturnLength.
    let mut needed: u32 = 0;
    // SAFETY: documented call; passing a 1-byte buffer guarantees mismatch,
    // and ReturnLength is a valid out-pointer. We discard the NTSTATUS.
    let _ = unsafe {
        NtQuerySystemInformation(
            SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut needed,
        )
    };
    if needed == 0 {
        return Err(anyhow!(
            "NtQuerySystemInformation reported 0 bytes for per-CPU performance info"
        ));
    }
    let cpus = needed as usize / STRIDE;
    if cpus == 0 || cpus > MAX_CPUS {
        return Err(anyhow!(
            "implausible logical-CPU count from kernel: {cpus} (max {MAX_CPUS})"
        ));
    }

    let mut buf: Vec<SystemProcessorPerformanceInformation> =
        vec![SystemProcessorPerformanceInformation::default(); cpus];
    let mut written: u32 = 0;
    // SAFETY: buf has exactly `cpus * STRIDE` bytes, matching the size the
    // kernel asked for. ReturnLength is valid.
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            (cpus * STRIDE) as u32,
            &mut written,
        )
    };
    if status.0 < 0 {
        return Err(anyhow!(
            "NtQuerySystemInformation(per-CPU) failed: NTSTATUS 0x{:08x}",
            status.0 as u32
        ));
    }
    let returned = (written as usize / STRIDE).min(cpus);

    let mut out = Vec::with_capacity(returned);
    for entry in &buf[..returned] {
        out.push(PerCpuTimes {
            idle_100ns: entry.idle_time as u64,
            kernel_100ns: entry.kernel_time as u64,
            user_100ns: entry.user_time as u64,
        });
    }
    Ok(out)
}

/// Total + available physical memory in bytes via `GlobalMemoryStatusEx`.
/// Returns `(total, available)`. Caller computes used = total - available
/// (or "load%" via the same struct's `dwMemoryLoad` field — but we want
/// the byte counts for the performance band, not just a percentage).
pub fn memory_status() -> Result<(u64, u64)> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut mem = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: dwLength is correctly initialised; mem out-param valid.
    unsafe { GlobalMemoryStatusEx(&mut mem) }
        .map_err(|e| anyhow!("GlobalMemoryStatusEx failed: {e}"))?;
    Ok((mem.ullTotalPhys, mem.ullAvailPhys))
}

/// Live process affinity mask via `GetProcessAffinityMask`. `None` if the
/// PID is gone or we can't open it for query. Returns just the process mask
/// (we discard the system mask — callers that need it can take their own
/// snapshot).
pub fn affinity_mask(pid: u32) -> Result<Option<u64>> {
    use windows::Win32::System::Threading::GetProcessAffinityMask;
    if pid == 0 {
        return Ok(None);
    }
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => OwnedHandle::assume_valid(h),
        // Item 4.8 — classify Expected vs Unexpected.
        Err(e) => {
            return match classify_open_process_error(&e) {
                OpenProcessClass::Exited | OpenProcessClass::AccessDenied => Ok(None),
                OpenProcessClass::Unexpected => Err(anyhow!(
                    "OpenProcess({pid}) for affinity mask failed unexpectedly: {e}"
                )),
            };
        }
    };
    let mut process_mask: usize = 0;
    let mut system_mask: usize = 0;
    // SAFETY: handle valid; both out-params valid.
    let r = unsafe { GetProcessAffinityMask(handle.as_raw(), &mut process_mask, &mut system_mask) };
    // handle drops at end of scope.
    match r {
        Ok(()) => Ok(Some(process_mask as u64)),
        Err(e) => match classify_open_process_error(&e) {
            OpenProcessClass::Exited | OpenProcessClass::AccessDenied => Ok(None),
            OpenProcessClass::Unexpected => Err(anyhow!(
                "GetProcessAffinityMask({pid}) failed unexpectedly: {e}"
            )),
        },
    }
}

/// Item 4.8 — classification of an OpenProcess (and related per-PID
/// query) error. Used to distinguish "this is an expected race against
/// the OS — silently return Ok(None)" from "something genuinely
/// unexpected went wrong — surface it."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpenProcessClass {
    /// `ERROR_INVALID_PARAMETER` — the PID exited between when the
    /// engine snapshotted the process list and when we tried to open
    /// it. Common; expected; not worth logging.
    Exited,
    /// `ERROR_ACCESS_DENIED` — the process is protected (anti-cheat,
    /// AV, kernel-protected service, PPL). Common on a typical
    /// Windows desktop; we will never be able to touch these. Not
    /// worth logging on every retry — the caller (engine) suppresses
    /// further attempts via the per-PID apply-failure backoff (item
    /// 4.9).
    AccessDenied,
    /// Anything else — surface to caller. Most likely a kernel call
    /// failing in a way we haven't seen before, which deserves a log
    /// line even if the immediate effect is the same as `Exited` for
    /// the per-PID call site.
    Unexpected,
}

/// Item 4.8 — map a windows-rs Error to an OpenProcessClass.
pub(crate) fn classify_open_process_error(e: &windows::core::Error) -> OpenProcessClass {
    // HRESULT_FROM_WIN32 prefixes the Win32 error code with 0x80070000.
    // Both ERROR_INVALID_PARAMETER (0x57) and ERROR_ACCESS_DENIED (0x5)
    // come through with that prefix; ERROR_* constants from windows-rs
    // are typed WIN32_ERROR which converts to HRESULT via Into.
    let code = e.code();
    if code == ERROR_INVALID_PARAMETER.to_hresult() {
        OpenProcessClass::Exited
    } else if code == ERROR_ACCESS_DENIED.to_hresult() {
        OpenProcessClass::AccessDenied
    } else {
        OpenProcessClass::Unexpected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Item 4.8 — OpenProcess error classification ──────────────────

    #[test]
    fn classify_invalid_parameter_is_exited() {
        // Construct a windows::core::Error wrapping ERROR_INVALID_PARAMETER.
        // Real OpenProcess produces these when the PID has exited
        // between snapshot and open; we synthesize one for the unit test.
        let e: windows::core::Error = ERROR_INVALID_PARAMETER.into();
        assert_eq!(classify_open_process_error(&e), OpenProcessClass::Exited);
    }

    #[test]
    fn classify_access_denied_is_access_denied() {
        let e: windows::core::Error = ERROR_ACCESS_DENIED.into();
        assert_eq!(
            classify_open_process_error(&e),
            OpenProcessClass::AccessDenied
        );
    }

    /// Anything not in the {INVALID_PARAMETER, ACCESS_DENIED} set
    /// must fall through to Unexpected so the caller surfaces it
    /// rather than silently swallowing a genuinely new failure
    /// mode.
    #[test]
    fn classify_unfamiliar_error_is_unexpected() {
        // ERROR_OUTOFMEMORY (0xE) — has never been observed from
        // OpenProcess in practice but is a plausible Win32 error.
        use windows::Win32::Foundation::ERROR_OUTOFMEMORY;
        let e: windows::core::Error = ERROR_OUTOFMEMORY.into();
        assert_eq!(
            classify_open_process_error(&e),
            OpenProcessClass::Unexpected
        );
    }

    /// `exe_for_pid` against a PID that's definitely gone must
    /// return `Ok(None)` (not Err) — that's the entire reason we
    /// classify INVALID_PARAMETER as Exited. Real OS path, not the
    /// synthesized-error path.
    #[test]
    fn exe_for_pid_classifies_dead_pid_as_ok_none() {
        // High-number PID that's almost certainly not live on the
        // test host. If by accident it IS live, exe_for_pid returns
        // a real path; both outcomes pass the assertion ("not Err").
        let result = exe_for_pid(0x7FFF_FFFE);
        assert!(
            result.is_ok(),
            "definitely-dead PID must not produce Err, got {:?}",
            result
        );
    }

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
