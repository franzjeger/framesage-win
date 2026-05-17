//! Item 4.13 — enumerate Windows services for the tray's
//! discover-services view.
//!
//! Backs `SysApi::enumerate_services()`. Uses
//! `EnumServicesStatusExW(SERVICE_STATE_ALL)` to grab every
//! service the SCM knows about plus its current
//! `SERVICE_STATUS_PROCESS` (current state + owning PID for
//! running services).
//!
//! What we DON'T fetch in this first cut:
//!
//! * **Start type** (Manual / Auto / AutoDelayed / Disabled) —
//!   requires a per-service `QueryServiceConfigW` round trip
//!   after the enum. Skipping saves N extra syscalls; can be
//!   added if the discover-services UI grows a column for it.
//! * **Per-service CPU / memory** — would require resolving the
//!   owning PID (for shared svchost groups, that's a partial
//!   picture anyway) and sampling. Substantial; planned for a
//!   later iteration where the discover view sorts by CPU delta.
//!
//! The output is sorted by display_name alphabetically so the
//! UI doesn't have to sort again for its default view.

use anyhow::{anyhow, Result};
use windows::Win32::System::Services::{
    CloseServiceHandle, EnumServicesStatusExW, OpenSCManagerW, ENUM_SERVICE_STATUS_PROCESSW,
    SC_ENUM_PROCESS_INFO, SC_HANDLE, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_RUNNING,
    SERVICE_START_PENDING, SERVICE_STATE_ALL, SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_WIN32,
};

/// One service the SCM knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
    /// Service short id (`SysMain`, `WSearch`, …). Passed as-is to
    /// `stop_service` / `start_service`.
    pub name: String,
    /// Human-readable display name (`SysMain` → "SysMain", but
    /// e.g. `WSearch` → "Windows Search").
    pub display_name: String,
    /// Current run state. Coarse — distinguishes the three the
    /// tray's discover view actually cares about.
    pub status: ServiceStatusKind,
    /// PID of the process hosting this service. `None` for stopped
    /// services. Shared svchost groups will have the same PID
    /// across many services.
    pub owning_pid: Option<u32>,
}

/// Coarse run-state classification. The Win32 SERVICE_STATUS enum
/// has 8 values; the tray UI only cares about three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceStatusKind {
    /// Currently running.
    Running,
    /// Currently stopped.
    Stopped,
    /// Transitioning (start_pending, stop_pending, etc.). Treat as
    /// "in flight" in the UI.
    Pending,
}

impl ServiceStatusKind {
    fn from_raw(raw: u32) -> Self {
        if raw == SERVICE_RUNNING.0 {
            Self::Running
        } else if raw == SERVICE_STOPPED.0 {
            Self::Stopped
        } else if raw == SERVICE_START_PENDING.0 || raw == SERVICE_STOP_PENDING.0 {
            Self::Pending
        } else {
            // Paused / pause-pending / continue-pending — UI treats
            // them as "in flight". The tray doesn't ever pause /
            // resume services itself, so the distinction doesn't
            // change behavior.
            Self::Pending
        }
    }
}

/// Enumerate every Win32 service the SCM knows about. Returns
/// entries sorted by display_name (case-insensitive) so the UI
/// gets a stable order without a second sort.
///
/// The enumeration covers `SERVICE_WIN32 | SERVICE_STATE_ALL` —
/// active + inactive Win32 services (excludes drivers, which the
/// tray's profile editor wouldn't act on anyway).
pub fn enumerate_services() -> Result<Vec<ServiceInfo>> {
    // SAFETY: documented call. Returns Err on access denied; the
    // SCM service is always live.
    let scm_raw = unsafe {
        OpenSCManagerW(
            windows::core::PCWSTR::null(),
            windows::core::PCWSTR::null(),
            SC_MANAGER_ENUMERATE_SERVICE,
        )
    }
    .map_err(|e| anyhow!("OpenSCManager failed: {e}"))?;
    let scm = OwnedScHandle(scm_raw);

    // First call: sizing pass. Pass an empty buffer; SCM tells us
    // how big the real one needs to be via the `bytes_needed`
    // out-param + ERROR_MORE_DATA. We ignore the error and read
    // the size.
    let mut bytes_needed: u32 = 0;
    let mut services_returned: u32 = 0;
    let mut resume_handle: u32 = 0;
    // SAFETY: passing a null/zero buffer is the documented "size
    // me" call. The bool result is ignored intentionally.
    let _ = unsafe {
        EnumServicesStatusExW(
            scm.0,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_STATE_ALL,
            None,
            &mut bytes_needed,
            &mut services_returned,
            Some(&mut resume_handle),
            windows::core::PCWSTR::null(),
        )
    };
    if bytes_needed == 0 {
        return Ok(Vec::new());
    }

    // Second call: real enumeration into a buffer of the size
    // SCM asked for. Pad by 4 KiB so services that started between
    // the two calls don't trigger a third pass.
    let buf_size = (bytes_needed as usize) + 4096;
    let mut buffer: Vec<u8> = vec![0; buf_size];
    let mut bytes_needed_2: u32 = 0;
    services_returned = 0;
    resume_handle = 0;
    // SAFETY: buffer is sized + zero-initialised; pointers are
    // valid; SCM fills `services_returned` entries of
    // ENUM_SERVICE_STATUS_PROCESSW (a fixed-size struct followed
    // by trailing string data pointed at via lpServiceName /
    // lpDisplayName, all within the same buffer).
    let ok = unsafe {
        EnumServicesStatusExW(
            scm.0,
            SC_ENUM_PROCESS_INFO,
            SERVICE_WIN32,
            SERVICE_STATE_ALL,
            Some(&mut buffer),
            &mut bytes_needed_2,
            &mut services_returned,
            Some(&mut resume_handle),
            windows::core::PCWSTR::null(),
        )
    };
    if let Err(e) = ok {
        return Err(anyhow!("EnumServicesStatusExW(real pass) failed: {e}"));
    }

    let mut out: Vec<ServiceInfo> = Vec::with_capacity(services_returned as usize);
    // SAFETY: buffer holds `services_returned` consecutive
    // ENUM_SERVICE_STATUS_PROCESSW structs; lpServiceName /
    // lpDisplayName point into the same buffer (SCM-owned
    // null-terminated UTF-16).
    let entries = unsafe {
        std::slice::from_raw_parts(
            buffer.as_ptr() as *const ENUM_SERVICE_STATUS_PROCESSW,
            services_returned as usize,
        )
    };
    for entry in entries {
        let name = read_pwstr_until_nul(entry.lpServiceName.0);
        let display_name = read_pwstr_until_nul(entry.lpDisplayName.0);
        let status = ServiceStatusKind::from_raw(entry.ServiceStatusProcess.dwCurrentState.0);
        let owning_pid = match (status, entry.ServiceStatusProcess.dwProcessId) {
            (ServiceStatusKind::Running, pid) if pid != 0 => Some(pid),
            _ => None,
        };
        out.push(ServiceInfo {
            name,
            display_name,
            status,
            owning_pid,
        });
    }

    // Stable sort by display_name (case-insensitive).
    out.sort_by(|a, b| {
        a.display_name
            .to_ascii_lowercase()
            .cmp(&b.display_name.to_ascii_lowercase())
    });
    Ok(out)
}

/// RAII wrapper for `SC_HANDLE`. Mirrors `OwnedHandle` (item 3.3)
/// but the SCM handles live in their own namespace separate from
/// kernel `HANDLE`, so they need their own RAII type.
struct OwnedScHandle(SC_HANDLE);

impl Drop for OwnedScHandle {
    fn drop(&mut self) {
        // SAFETY: handle was given to us by the SCM. CloseServiceHandle
        // returns BOOL — nothing useful to do with a failure.
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

/// Read a null-terminated UTF-16 string starting at `ptr`. Returns
/// an empty string for a null pointer. Caller asserts the pointer
/// is valid (points into a buffer that lives at least as long as
/// this call).
fn read_pwstr_until_nul(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: caller asserts validity. We stop at the first u16
    // equal to 0, which is the documented terminator. Bound at
    // 4096 chars defensively in case SCM ever produces an
    // unterminated string (which the docs guarantee against, but
    // defense in depth costs nothing).
    let mut len = 0usize;
    unsafe {
        while len < 4096 {
            if *ptr.add(len) == 0 {
                break;
            }
            len += 1;
        }
        let slice = std::slice::from_raw_parts(ptr, len);
        String::from_utf16_lossy(slice)
    }
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The SCM is always running on a Windows host. Enumeration
    /// MUST return a non-empty list with a few well-known services
    /// present.
    #[test]
    fn enumerate_returns_known_services() {
        let services = enumerate_services().expect("enumerate services");
        assert!(
            !services.is_empty(),
            "no services returned — SCM must have at least one"
        );

        // Spot-check: every Windows install has DcomLaunch.
        let names: std::collections::HashSet<String> = services
            .iter()
            .map(|s| s.name.to_ascii_lowercase())
            .collect();
        assert!(
            names.contains("dcomlaunch"),
            "DcomLaunch should always be present"
        );
    }

    /// Output must be sorted by display_name (case-insensitive)
    /// so the UI doesn't need to re-sort for its default view.
    #[test]
    fn enumerate_output_is_sorted_by_display_name() {
        let services = enumerate_services().expect("enumerate services");
        for window in services.windows(2) {
            let a = window[0].display_name.to_ascii_lowercase();
            let b = window[1].display_name.to_ascii_lowercase();
            assert!(a <= b, "enumeration not sorted: {a:?} should be <= {b:?}");
        }
    }

    /// Every Running service must carry an owning PID; every
    /// Stopped service must carry None. (Pending sits in between
    /// and we don't pin it.)
    #[test]
    fn enumerate_running_services_have_pids_stopped_dont() {
        let services = enumerate_services().expect("enumerate services");
        for svc in &services {
            match svc.status {
                ServiceStatusKind::Running => {
                    assert!(
                        svc.owning_pid.is_some(),
                        "Running service {} should have a PID",
                        svc.name
                    );
                }
                ServiceStatusKind::Stopped => {
                    assert!(
                        svc.owning_pid.is_none(),
                        "Stopped service {} should NOT have a PID",
                        svc.name
                    );
                }
                ServiceStatusKind::Pending => {
                    // Either is permissible during a transition.
                }
            }
        }
    }
}
