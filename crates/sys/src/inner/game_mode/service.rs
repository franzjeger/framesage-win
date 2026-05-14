//! Windows Service Control Manager wrappers — stop, start, and query status.
//!
//! Everything is `OpenSCManagerW` → `OpenServiceW` → operate → `CloseServiceHandle`.
//! Each function opens its own pair of handles for clarity and safety; the
//! handles are local and there's no benefit to caching the SCM handle across
//! operations.
//!
//! Stops poll for `SERVICE_STOPPED` rather than returning immediately, because
//! `ControlService(STOP)` only signals — the service may still be running for
//! seconds afterward. We bound the wait so a misbehaving service can't hang
//! the engine.

use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_NOT_ACTIVE};
use windows::Win32::System::Services::{
    CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx,
    StartServiceW, SC_HANDLE, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_CONTINUE_PENDING,
    SERVICE_CONTROL_STOP, SERVICE_PAUSED, SERVICE_PAUSE_PENDING, SERVICE_QUERY_STATUS,
    SERVICE_RUNNING, SERVICE_START, SERVICE_START_PENDING, SERVICE_STATUS_PROCESS, SERVICE_STOP,
    SERVICE_STOPPED, SERVICE_STOP_PENDING,
};

use framesage_gamemode::state::ServiceStatus;

/// Default time we'll wait for a service to reach a target state.
const SERVICE_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const SERVICE_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Read a service's current status without changing anything.
///
/// Returns `Ok(ServiceStatus::Stopped)` if the service is unknown — callers
/// generally only ask about services they expect to exist; non-existence is
/// surfaced via the wider `framesage-sys::game_mode::query` flow.
pub fn query_service_status(id: &str) -> Result<ServiceStatus> {
    with_service_handle(id, SERVICE_QUERY_STATUS, |handle| {
        let raw = read_status(handle)?;
        Ok(map_status(raw.dwCurrentState.0))
    })
}

/// Send STOP and wait until the service reports STOPPED (or we time out).
///
/// Idempotent: if the service is already stopped, returns `Ok(false)` (we
/// changed nothing). On successful stop, returns `Ok(true)`.
pub fn stop_service(id: &str) -> Result<bool> {
    with_service_handle(
        id,
        SERVICE_QUERY_STATUS | SERVICE_STOP,
        |handle| -> Result<bool> {
            let current = read_status(handle)?;
            if current.dwCurrentState == SERVICE_STOPPED {
                return Ok(false);
            }

            // SAFETY: handle is open with SERVICE_STOP rights; we pass a
            // zero-initialised SERVICE_STATUS out-param.
            let mut status = windows::Win32::System::Services::SERVICE_STATUS::default();
            let stop_result = unsafe { ControlService(handle, SERVICE_CONTROL_STOP, &mut status) };
            if let Err(e) = stop_result {
                // If SCM reports "not active" we raced someone; treat as
                // already-stopped, not an error.
                if matches_win32_error(&e, ERROR_SERVICE_NOT_ACTIVE.0) {
                    return Ok(false);
                }
                return Err(anyhow!("ControlService({id}, STOP) failed: {e}"));
            }

            wait_for_state(handle, SERVICE_STOPPED.0)?;
            Ok(true)
        },
    )
}

/// Start a previously-stopped service. Idempotent: if already running,
/// returns `Ok(false)`.
pub fn start_service(id: &str) -> Result<bool> {
    with_service_handle(
        id,
        SERVICE_QUERY_STATUS | SERVICE_START,
        |handle| -> Result<bool> {
            let current = read_status(handle)?;
            if current.dwCurrentState == SERVICE_RUNNING {
                return Ok(false);
            }

            // SAFETY: handle is open with SERVICE_START; passing no arguments
            // (None, 0) is the documented way to start with the service's
            // default arguments.
            let result = unsafe { StartServiceW(handle, None) };
            if let Err(e) = result {
                if matches_win32_error(&e, ERROR_SERVICE_ALREADY_RUNNING.0) {
                    return Ok(false);
                }
                return Err(anyhow!("StartServiceW({id}) failed: {e}"));
            }

            wait_for_state(handle, SERVICE_RUNNING.0)?;
            Ok(true)
        },
    )
}

// ─── helpers ──────────────────────────────────────────────────────────────

fn with_service_handle<R>(
    id: &str,
    rights: u32,
    body: impl FnOnce(SC_HANDLE) -> Result<R>,
) -> Result<R> {
    let scm = open_scm()?;
    let result = (|| {
        let svc = open_service(scm, id, rights)?;
        let r = body(svc);
        // SAFETY: svc is the handle we just opened and haven't otherwise used.
        let _ = unsafe { CloseServiceHandle(svc) };
        r
    })();
    // SAFETY: scm is the handle we just opened.
    let _ = unsafe { CloseServiceHandle(scm) };
    result
}

fn open_scm() -> Result<SC_HANDLE> {
    // SAFETY: documented call; passing None/None requests the local machine
    // and the active database. SC_MANAGER_CONNECT is the minimum required.
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }
        .map_err(|e| anyhow!("OpenSCManagerW failed: {e}"))?;
    Ok(scm)
}

fn open_service(scm: SC_HANDLE, id: &str, rights: u32) -> Result<SC_HANDLE> {
    let name: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: scm is a valid SCM handle; name is null-terminated and lives
    // for the call's duration.
    let svc = unsafe { OpenServiceW(scm, PCWSTR(name.as_ptr()), rights) }
        .map_err(|e| anyhow!("OpenServiceW({id}) failed: {e}"))?;
    Ok(svc)
}

fn read_status(handle: SC_HANDLE) -> Result<SERVICE_STATUS_PROCESS> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut bytes_needed: u32 = 0;
    let buf = unsafe {
        std::slice::from_raw_parts_mut(
            &mut status as *mut _ as *mut u8,
            std::mem::size_of::<SERVICE_STATUS_PROCESS>(),
        )
    };
    // SAFETY: SC_STATUS_PROCESS_INFO is the documented SC_STATUS_TYPE for the
    // SERVICE_STATUS_PROCESS layout. `buf` is exactly that struct's size.
    unsafe { QueryServiceStatusEx(handle, SC_STATUS_PROCESS_INFO, Some(buf), &mut bytes_needed) }
        .map_err(|e| anyhow!("QueryServiceStatusEx failed: {e}"))?;
    Ok(status)
}

fn map_status(state: u32) -> ServiceStatus {
    match state {
        s if s == SERVICE_STOPPED.0 => ServiceStatus::Stopped,
        s if s == SERVICE_START_PENDING.0 => ServiceStatus::StartPending,
        s if s == SERVICE_STOP_PENDING.0 => ServiceStatus::StopPending,
        s if s == SERVICE_RUNNING.0 => ServiceStatus::Running,
        s if s == SERVICE_CONTINUE_PENDING.0 => ServiceStatus::ContinuePending,
        s if s == SERVICE_PAUSE_PENDING.0 => ServiceStatus::PausePending,
        s if s == SERVICE_PAUSED.0 => ServiceStatus::Paused,
        other => ServiceStatus::Other(other),
    }
}

fn wait_for_state(handle: SC_HANDLE, want_state: u32) -> Result<()> {
    let deadline = Instant::now() + SERVICE_WAIT_TIMEOUT;
    loop {
        let current = read_status(handle)?;
        if current.dwCurrentState.0 == want_state {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "service wait timed out: current={} want={want_state}",
                current.dwCurrentState.0
            ));
        }
        std::thread::sleep(SERVICE_POLL_INTERVAL);
    }
}

fn matches_win32_error(err: &windows::core::Error, want: u32) -> bool {
    let code = err.code().0 as u32;
    code == want || (code & 0xFFFF) == (want & 0xFFFF)
}
