//! Get the currently-foregrounded process.
//!
//! `GetForegroundWindow` -> `GetWindowThreadProcessId` -> `OpenProcess` ->
//! `QueryFullProcessImageNameW`. This is the documented chain; it's what every
//! gaming optimiser, RGB suite, and overlay uses, so anti-cheat has no problem
//! with it.

use anyhow::{anyhow, Context, Result};
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HWND, MAX_PATH};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
};

#[derive(Debug, Clone)]
pub struct ForegroundInfo {
    pub pid: u32,
    pub exe_name: String,
    pub path: String,
    pub title: String,
}

/// Return the foreground process, if there is one.
///
/// Returns `Ok(None)` when no window has focus (lock screen, transition,
/// elevated UAC consent screen — desktop is technically focused but the HWND
/// is null). Returns an error only for unexpected API failures.
pub fn current() -> Result<Option<ForegroundInfo>> {
    // SAFETY: GetForegroundWindow is always safe. Returns NULL when no window
    // is in the foreground, which we handle.
    let hwnd: HWND = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return Ok(None);
    }

    let mut pid: u32 = 0;
    // SAFETY: hwnd is non-null (checked above). `pid` is a valid out-pointer.
    let _tid = unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == 0 {
        return Ok(None);
    }

    let title = read_window_title(hwnd).unwrap_or_default();
    let path = read_process_image_path(pid).context("read image path")?;
    let exe_name = exe_name_from_path(&path);

    Ok(Some(ForegroundInfo {
        pid,
        exe_name,
        path,
        title,
    }))
}

fn read_window_title(hwnd: HWND) -> Option<String> {
    // SAFETY: hwnd is a valid HWND.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return None;
    }
    let mut buf: Vec<u16> = vec![0; (len as usize) + 1];
    // SAFETY: buf has space for len + 1 wide chars.
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if n == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..n as usize]))
}

fn read_process_image_path(pid: u32) -> Result<String> {
    // SAFETY: OpenProcess with LIMITED_INFORMATION is always sound; it returns
    // an error handle if access is denied (e.g. protected processes).
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .map_err(|e| anyhow!("OpenProcess({pid}) failed: {e}"))?;

    let mut buf = [0u16; MAX_PATH as usize];
    let mut size = buf.len() as u32;
    // SAFETY: handle is valid (we just opened it). buf and size are valid.
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };

    // Always close the handle; ignore the close result.
    // SAFETY: handle is the value we just opened and have not invalidated.
    let _ = unsafe { CloseHandle(handle) };

    result.map_err(|e| anyhow!("QueryFullProcessImageNameW({pid}) failed: {e}"))?;
    Ok(String::from_utf16_lossy(&buf[..size as usize]))
}

fn exe_name_from_path(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_owned()
}
