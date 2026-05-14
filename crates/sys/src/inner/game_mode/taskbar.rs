//! Show/hide the taskbar.
//!
//! Two window classes need handling: `Shell_TrayWnd` (primary taskbar) and
//! `Shell_SecondaryTrayWnd` (per-extra-monitor taskbar in multi-monitor
//! setups). We hide/show both atomically.
//!
//! `ShowWindow` is a documented, decades-stable API. Hiding the taskbar this
//! way is what fullscreen-cover utilities have done since Windows XP — no
//! anti-cheat objects to it.

use anyhow::Result;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GetClassNameW, IsWindowVisible, ShowWindow, SW_HIDE, SW_SHOW,
};

/// Hide the primary taskbar and every secondary taskbar.
///
/// Returns `true` if anything was changed (i.e. at least one taskbar window
/// was visible and we hid it). Idempotent: calling twice in a row hides on
/// the first call and is a no-op on the second.
pub fn hide_taskbar() -> Result<bool> {
    apply_visibility(false)
}

/// Restore the primary taskbar and every secondary taskbar.
///
/// Idempotent: calling on already-visible taskbars is a no-op.
pub fn show_taskbar() -> Result<bool> {
    apply_visibility(true)
}

/// Is the primary taskbar currently visible? Multi-monitor secondaries are
/// assumed to track the primary; we don't currently report on them
/// independently because the user-facing experience is whole-screen.
pub fn taskbar_visible() -> Result<bool> {
    let hwnd = find_primary_taskbar();
    if hwnd.0.is_null() {
        // No taskbar found at all — treat as "not visible" rather than
        // erroring; matches the conservative planner policy.
        return Ok(false);
    }
    // SAFETY: hwnd is the result of FindWindowExW; documented safe for read.
    Ok(unsafe { IsWindowVisible(hwnd) }.as_bool())
}

fn apply_visibility(visible: bool) -> Result<bool> {
    let mut changed = false;

    // Primary taskbar.
    let primary = find_primary_taskbar();
    if !primary.0.is_null() {
        changed |= set_window_visibility(primary, visible);
    }

    // Secondary (multi-monitor) taskbars.
    let mut secondaries: Vec<HWND> = Vec::new();
    let secondaries_ptr: *mut Vec<HWND> = &mut secondaries;
    // SAFETY: EnumWindows invokes our callback synchronously while the closure
    // and Vec live; the LPARAM round-trip is the documented contract.
    let _ = unsafe {
        EnumWindows(
            Some(enum_collect_secondaries),
            LPARAM(secondaries_ptr as isize),
        )
    };
    for hwnd in secondaries {
        changed |= set_window_visibility(hwnd, visible);
    }

    Ok(changed)
}

fn find_primary_taskbar() -> HWND {
    let class: Vec<u16> = "Shell_TrayWnd\0".encode_utf16().collect();
    // SAFETY: FindWindowExW with all-null parents/child + a wide-string class
    // is documented to return either the matching HWND or null.
    unsafe {
        FindWindowExW(
            HWND::default(),
            HWND::default(),
            PCWSTR(class.as_ptr()),
            PCWSTR::null(),
        )
    }
    .unwrap_or(HWND::default())
}

fn set_window_visibility(hwnd: HWND, visible: bool) -> bool {
    // SAFETY: hwnd is non-null by precondition. ShowWindow can't fail in any
    // way relevant to us — its return value indicates the *previous* state.
    let cmd = if visible { SW_SHOW } else { SW_HIDE };
    let was_visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    let _ = unsafe { ShowWindow(hwnd, cmd) };
    was_visible != visible
}

extern "system" fn enum_collect_secondaries(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the &mut Vec<HWND> we passed in. EnumWindows is
    // synchronous, so the vec is alive for the duration of every callback.
    let buf: &mut Vec<HWND> = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };

    let mut class_buf = [0u16; 64];
    // SAFETY: class_buf is fixed-size; GetClassNameW writes at most `len`
    // chars and returns the count written. Zero on failure, which we ignore.
    let n = unsafe { GetClassNameW(hwnd, &mut class_buf) };
    if n > 0 {
        let class = String::from_utf16_lossy(&class_buf[..n as usize]);
        if class == "Shell_SecondaryTrayWnd" {
            buf.push(hwnd);
        }
    }
    BOOL(1) // continue enumerating
}
