//! #6 — global hotkey (default Ctrl+Alt+G) for Manual Global Game Mode.
//!
//! `RegisterHotKey` posts `WM_HOTKEY` to the registering thread's
//! message queue, but eframe/winit owns the main thread's message
//! loop. So we run a dedicated thread with a message-only window that
//! owns the hotkey and pumps messages; on `WM_HOTKEY` it flips an
//! atomic the egui `update()` loop already polls (the same
//! command-flag pattern as the tray menu's other actions).
//!
//! Conflict detection: `RegisterHotKey` fails if another process holds
//! the combo. We surface that as `HotkeyStatus::Conflict` rather than
//! silently doing nothing, so the Settings UI can tell the user to
//! pick another (the binding is fixed at Ctrl+Alt+G for now; the
//! config UI is the v0.7 stretch item).

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Outcome of trying to register the global hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyStatus {
    /// Registered; the background pump is running.
    Registered,
    /// Another process owns Ctrl+Alt+G.
    Conflict,
    /// Registration failed for another reason (logged).
    Failed,
    /// Non-Windows build — no-op (only constructed off Windows).
    #[cfg_attr(windows, allow(dead_code))]
    Unsupported,
}

/// Registers Ctrl+Alt+G on a background message pump. Each press
/// toggles `toggle_flag` (the egui loop drains it and dispatches the
/// enable/disable-manual-global request). The returned guard keeps the
/// pump alive; dropping it unregisters and stops the thread.
#[cfg(windows)]
pub fn register_toggle_hotkey(toggle_flag: Arc<AtomicBool>) -> (HotkeyStatus, Option<HotkeyGuard>) {
    windows_impl::register(toggle_flag)
}

#[cfg(not(windows))]
pub fn register_toggle_hotkey(
    _toggle_flag: Arc<AtomicBool>,
) -> (HotkeyStatus, Option<HotkeyGuard>) {
    (HotkeyStatus::Unsupported, None)
}

/// Keeps the hotkey pump alive. Drop to unregister + join the thread.
/// The field is held only for its `Drop` side effect.
pub struct HotkeyGuard {
    #[cfg(windows)]
    #[allow(dead_code)]
    stop: windows_impl::Stopper,
}

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use std::sync::atomic::Ordering;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, MSG, WM_HOTKEY,
        WM_QUIT,
    };

    /// Ctrl+Alt+G. `0x47` is the virtual-key code for 'G'.
    const HOTKEY_ID: i32 = 0xF5A6;
    const VK_G: u32 = 0x47;

    pub struct Stopper {
        thread_id: u32,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for Stopper {
        fn drop(&mut self) {
            // Post WM_QUIT to the pump thread so GetMessageW returns 0
            // and the loop (which unregisters the hotkey) exits.
            // SAFETY: PostThreadMessageW to a live thread id; the
            // thread only exits after we join below.
            unsafe {
                let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
            }
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    pub fn register(toggle_flag: Arc<AtomicBool>) -> (HotkeyStatus, Option<HotkeyGuard>) {
        let (status_tx, status_rx) = std::sync::mpsc::channel::<(HotkeyStatus, u32)>();
        let handle = std::thread::Builder::new()
            .name("framesage-hotkey".into())
            .spawn(move || pump(toggle_flag, status_tx))
            .expect("spawn hotkey thread");

        // Wait for the pump to report whether RegisterHotKey succeeded.
        match status_rx.recv() {
            Ok((HotkeyStatus::Registered, thread_id)) => (
                HotkeyStatus::Registered,
                Some(HotkeyGuard {
                    stop: Stopper {
                        thread_id,
                        handle: Some(handle),
                    },
                }),
            ),
            Ok((status, _)) => {
                let _ = handle.join();
                (status, None)
            }
            Err(_) => {
                let _ = handle.join();
                (HotkeyStatus::Failed, None)
            }
        }
    }

    fn pump(toggle_flag: Arc<AtomicBool>, status_tx: std::sync::mpsc::Sender<(HotkeyStatus, u32)>) {
        // SAFETY: RegisterHotKey with HWND::default() associates the hotkey
        // with the calling thread; WM_HOTKEY then arrives in this
        // thread's queue. MOD_NOREPEAT avoids autorepeat storms.
        let thread_id = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
        let registered = unsafe {
            RegisterHotKey(
                HWND::default(),
                HOTKEY_ID,
                MOD_CONTROL | MOD_ALT | MOD_NOREPEAT,
                VK_G,
            )
        };
        if registered.is_err() {
            let _ = status_tx.send((HotkeyStatus::Conflict, thread_id));
            return;
        }
        let _ = status_tx.send((HotkeyStatus::Registered, thread_id));

        // Message loop. GetMessageW returns 0 on WM_QUIT (posted by
        // the Stopper), <0 on error; either ends the loop.
        let mut msg = MSG::default();
        loop {
            let got = unsafe { GetMessageW(&mut msg, HWND::default(), 0, 0) };
            if got.0 <= 0 {
                break;
            }
            if msg.message == WM_HOTKEY && msg.wParam.0 as i32 == HOTKEY_ID {
                toggle_flag.store(true, Ordering::Relaxed);
                continue;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        // SAFETY: unregister the hotkey we own before the thread ends.
        unsafe {
            let _ = UnregisterHotKey(HWND::default(), HOTKEY_ID);
        }
    }

    // Silence unused on the LRESULT import path used by the windows
    // crate's macro expansion in some feature combinations.
    #[allow(dead_code)]
    fn _unused(_: LRESULT) {}
}
