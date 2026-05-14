//! Win32 helpers for the tray: elevation detection, singleton mutex,
//! relaunch-as-admin.
//!
//! The tray runs unprivileged by default so it can live in the user's
//! session without UAC at every boot. But admin operations (pause, resume,
//! game-mode off, set-policy) need the admin pipe, which the kernel ACL
//! refuses to unprivileged callers. The UX is: show a banner with an
//! "Enable controls" button that respawns the tray elevated. One UAC per
//! session beats per-action UAC by a wide margin.
//!
//! The singleton mutex defeats the dual-tray-icon scenario that would
//! otherwise happen during the elevation handoff: the old non-elevated
//! tray exits, the new elevated one starts. Both processes briefly try
//! to hold the tray icon. The mutex makes the second instance wait for
//! the first to release.

#![cfg(windows)]

use anyhow::{anyhow, Context, Result};
use std::os::windows::ffi::OsStrExt;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_NORMAL;

/// Name of the singleton mutex. `Global\` makes it visible across user
/// sessions (so an elevated child in the same session sees the
/// unprivileged parent's mutex). The unique suffix differentiates from
/// other tools that might pick the same short name.
const SINGLETON_MUTEX_NAME: &str = r"Global\framesage-tray-singleton-{f0f6d4f2-2c91-4c83-bf45}";

/// How long the elevated child will wait for the non-elevated parent to
/// release the singleton mutex during handoff. 3 seconds is comfortably
/// more than process-exit latency in practice (single-digit ms).
const SINGLETON_HANDOFF_TIMEOUT: Duration = Duration::from_secs(3);

/// Returns `true` if the current process token has the elevation flag set.
/// This corresponds to "Run as administrator" launches and to the LocalSystem
/// service token. Medium-integrity user sessions return `false`.
pub fn is_elevated() -> Result<bool> {
    let mut token: HANDLE = HANDLE::default();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that doesn't need
    // closing; OpenProcessToken writes to our out-param.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .context("OpenProcessToken")?;

    let mut elevation = TOKEN_ELEVATION::default();
    let mut len: u32 = 0;
    // SAFETY: token is valid (from OpenProcessToken); pointer + size match
    // the TOKEN_ELEVATION layout.
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
    };

    // SAFETY: token is valid; we always close it before returning.
    let _ = unsafe { CloseHandle(token) };

    result.context("GetTokenInformation")?;
    Ok(elevation.TokenIsElevated != 0)
}

/// RAII handle to the singleton mutex. Drop releases it. Returned from
/// [`acquire_singleton`] either immediately (mutex was free) or after a
/// short wait for the previous holder to exit.
pub struct SingletonGuard {
    handle: HANDLE,
}

impl Drop for SingletonGuard {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: mutex handle is valid and owned by us.
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
}

/// Try to acquire the singleton mutex.
///
/// * Success: returns a [`SingletonGuard`]; the caller is the unique
///   tray instance until the guard drops.
/// * Already-held by another process: waits up to
///   [`SINGLETON_HANDOFF_TIMEOUT`] for the holder to exit. If the holder
///   doesn't exit, returns `Err` — caller should display the error and
///   exit.
pub fn acquire_singleton() -> Result<SingletonGuard> {
    let deadline = Instant::now() + SINGLETON_HANDOFF_TIMEOUT;
    loop {
        let name_wide = encode_utf16_z(SINGLETON_MUTEX_NAME);
        // SAFETY: name_wide is null-terminated; CreateMutexW with null
        // security attributes uses the default DACL.
        let handle = unsafe { CreateMutexW(None, true, PCWSTR(name_wide.as_ptr())) }
            .context("CreateMutexW")?;

        // SAFETY: GetLastError reads thread-local error state set by the
        // immediately-preceding API call.
        let last_err = unsafe { GetLastError() };
        if last_err == ERROR_ALREADY_EXISTS {
            // Another tray instance has the mutex. Close our handle —
            // CreateMutexW with an existing name still returns a handle
            // to the same mutex, which we don't want to own.
            // SAFETY: handle is a valid mutex handle.
            let _ = unsafe { CloseHandle(handle) };

            if Instant::now() >= deadline {
                return Err(anyhow!(
                    "another framesage-tray instance is running; refusing to start a duplicate"
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }

        return Ok(SingletonGuard { handle });
    }
}

/// Spawn the same executable elevated via ShellExecute("runas"). Returns
/// `Ok(())` only if the user accepted the UAC prompt and Windows
/// successfully started the new process.
///
/// On success the caller should release any held resources (the singleton
/// mutex, tray icon) and exit so the elevated instance can take over.
pub fn relaunch_as_admin() -> Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;
    let exe_wide = encode_utf16_z(exe.as_os_str());
    let verb_wide = encode_utf16_z("runas");

    // SAFETY: all string pointers are null-terminated UTF-16; show command
    // is a documented constant; hwnd null is valid for ShellExecuteW.
    // Return value: HINSTANCE whose numeric value <= 32 indicates failure.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb_wide.as_ptr()),
            PCWSTR(exe_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_NORMAL,
        )
    };
    let code = result.0 as isize;
    if code <= 32 {
        // 5 = ERROR_ACCESS_DENIED (user clicked No on UAC); 2 = file not
        // found; 31 = no application associated. Surface the raw code.
        return Err(anyhow!(
            "ShellExecute(runas) failed: HINSTANCE={code} (likely UAC declined or path resolution error)"
        ));
    }
    Ok(())
}

fn encode_utf16_z(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    s.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}
