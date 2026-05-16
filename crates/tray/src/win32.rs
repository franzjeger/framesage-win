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
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, GetCurrentProcess, OpenEventW, OpenProcessToken, SetEvent,
    WaitForSingleObject, EVENT_MODIFY_STATE, INFINITE, SYNCHRONIZATION_ACCESS_RIGHTS,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_NORMAL;

/// Name of the singleton mutex. `Global\` makes it visible across user
/// sessions (so an elevated child in the same session sees the
/// unprivileged parent's mutex). The unique suffix differentiates from
/// other tools that might pick the same short name.
const SINGLETON_MUTEX_NAME: &str = r"Global\framesage-tray-singleton-{f0f6d4f2-2c91-4c83-bf45}";

/// Cross-instance "show the window" signal. When the user double-clicks the
/// .exe (or any Start-menu / Explorer launch) while a tray is already
/// running, the second instance opens this event, calls `SetEvent`, and
/// exits. The first instance has a thread blocked on `WaitForSingleObject`
/// that wakes, flips `commands.show_window`, and the egui runtime restores
/// + focuses the window on its next frame.
///
/// Auto-reset so a single `SetEvent` from the second instance produces a
/// single wake on the first — repeated launches each get one wake.
const SHOW_WINDOW_EVENT_NAME: &str =
    r"Global\framesage-tray-show-window-{2dc9c8d3-7b51-4a87-9c2e-3a99e7f1c5b0}";

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

/// Outcome of [`acquire_singleton`]. Callers act differently:
/// * `Primary` — we own the tray, proceed with full startup. Hold the
///   guard for the process lifetime; dropping releases the mutex.
/// * `AlreadyRunning` — another tray is up and the elevation-handoff
///   window has elapsed. The caller should signal the existing instance
///   to show its window (via [`signal_existing_tray_show_window`]) and
///   exit. This is the "user double-clicked the exe / Start-menu icon
///   while a tray was already running" path — the right UX is "bring
///   the existing window forward," not "fail with a popup."
pub enum SingletonAttempt {
    Primary(SingletonGuard),
    AlreadyRunning,
}

/// Try to acquire the singleton mutex.
///
/// Waits up to [`SINGLETON_HANDOFF_TIMEOUT`] for any previous holder to
/// exit (covers the elevation-handoff window). After that returns
/// `AlreadyRunning` so the caller can signal-and-exit instead of
/// presenting an error.
pub fn acquire_singleton() -> Result<SingletonAttempt> {
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
                return Ok(SingletonAttempt::AlreadyRunning);
            }
            std::thread::sleep(Duration::from_millis(200));
            continue;
        }

        return Ok(SingletonAttempt::Primary(SingletonGuard { handle }));
    }
}

/// RAII wrapper around the auto-reset "show window" event the primary
/// tray instance owns. Drop closes the handle.
pub struct ShowWindowEvent {
    handle: HANDLE,
}

// SAFETY: a Win32 kernel HANDLE is process-wide and refers to a kernel
// object; the documented thread-safety contract for `WaitForSingleObject`,
// `SetEvent`, and `CloseHandle` makes them callable from any thread that
// owns the handle. Moving this struct between threads is therefore sound.
unsafe impl Send for ShowWindowEvent {}
unsafe impl Sync for ShowWindowEvent {}

impl Drop for ShowWindowEvent {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: handle owned by us.
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }
}

impl ShowWindowEvent {
    /// Block the calling thread until another instance signals us to show
    /// the window, or until the handle is closed. Returns `Ok(true)` on a
    /// real signal, `Ok(false)` on abandoned / error so the caller can
    /// loop without crashing if Windows ever returns an unexpected state.
    pub fn wait(&self) -> Result<bool> {
        // SAFETY: handle is owned + valid.
        let r = unsafe { WaitForSingleObject(self.handle, INFINITE) };
        Ok(r == WAIT_OBJECT_0)
    }
}

/// Create (or open) the named auto-reset event the primary tray waits on
/// for "user re-launched the .exe, please show your window" pings. Called
/// once during primary startup; the handle is held for the lifetime of the
/// process.
pub fn create_show_window_event() -> Result<ShowWindowEvent> {
    let name_wide = encode_utf16_z(SHOW_WINDOW_EVENT_NAME);
    // Manual reset = false → auto-reset (one wake per SetEvent call).
    // Initial state = false → starts non-signaled so the wait blocks
    // immediately on first call.
    // SAFETY: name_wide is null-terminated; null security attributes use
    // the default DACL.
    let handle = unsafe { CreateEventW(None, false, false, PCWSTR(name_wide.as_ptr())) }
        .context("CreateEventW")?;
    Ok(ShowWindowEvent { handle })
}

/// Open the named event the primary tray created and signal it once,
/// telling that primary to bring its window forward. Used by the
/// secondary instance after a "tray already running" detection — the
/// caller signals, then exits, leaving the primary to handle the show.
///
/// Returns `Ok(true)` if the event existed and we signaled it; `Ok(false)`
/// if the event didn't exist (which means the primary is starting up at
/// the same time and hasn't created the event yet — rare race, but we
/// don't want to crash the secondary over it).
pub fn signal_existing_tray_show_window() -> Result<bool> {
    let name_wide = encode_utf16_z(SHOW_WINDOW_EVENT_NAME);
    // SAFETY: name_wide is null-terminated.
    let handle = match unsafe {
        OpenEventW(
            SYNCHRONIZATION_ACCESS_RIGHTS(EVENT_MODIFY_STATE.0),
            false,
            PCWSTR(name_wide.as_ptr()),
        )
    } {
        Ok(h) => h,
        Err(_) => return Ok(false),
    };
    // SAFETY: handle came from OpenEventW so it's valid.
    let r = unsafe { SetEvent(handle) };
    // SAFETY: handle is ours to close.
    let _ = unsafe { CloseHandle(handle) };
    r.context("SetEvent")?;
    Ok(true)
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
