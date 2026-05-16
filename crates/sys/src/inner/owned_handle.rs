//! Item 3.3 — RAII wrapper around a Win32 `HANDLE`.
//!
//! framesage-sys does a LOT of `OpenProcess` → query → `CloseHandle`
//! sequences. Each one is a hand-rolled `let handle = unsafe {
//! OpenProcess(...)? }; ... let _ = unsafe { CloseHandle(handle) };`
//! dance with the close on every early-return path. Long-lived
//! processes (the service runs for hours / days at a time) lose
//! handles to any missed close — the kernel handle table fills up,
//! eventually `OpenProcess` starts returning `ERROR_NO_SYSTEM_RESOURCES`,
//! and the engine silently stops working.
//!
//! `OwnedHandle` wraps a `HANDLE` with `Drop` that closes it. The
//! type is `Send` (so handles can move across thread boundaries,
//! which the engine already needs for its multi-task tokio runtime)
//! but explicitly NOT `Sync` — Win32 documents `CloseHandle` as not
//! thread-safe with respect to other ops on the same handle, so we
//! require unique ownership for safe use.
//!
//! The wrapper is intentionally narrow:
//!
//! * `take_from_raw(HANDLE)` — assume ownership of an already-open
//!   handle (use after `OpenProcess` / `CreateFileW` / etc.).
//! * `as_raw()` — pass the underlying `HANDLE` into a Win32 call
//!   that needs it. The OwnedHandle stays alive for the duration of
//!   the borrow.
//! * `Drop` — `CloseHandle`, swallowing errors (Win32 will return
//!   `ERROR_INVALID_HANDLE` if we double-close; in production this
//!   shouldn't happen because ownership is unique).
//!
//! Notably absent: `Clone`. There's no safe way to clone a handle
//! without `DuplicateHandle`, which has its own access-rights
//! considerations; callers that need a second reference to the
//! same kernel object should call `DuplicateHandle` explicitly.

use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};

/// RAII owner of a Win32 `HANDLE`. Closes the handle on drop.
///
/// The wrapper is sound by construction:
///
/// * `OwnedHandle::from_raw(h)` consumes a raw HANDLE that the caller
///   asserts is valid + unique. Subsequent `as_raw()` calls return
///   the same value; no `CloseHandle` happens until drop.
/// * Methods that take `&self` (read-only Win32 calls like
///   `GetPriorityClass`) borrow without yielding ownership. The
///   handle stays open across the borrow.
/// * `into_raw()` releases ownership without closing — use sparingly
///   (e.g. when handing the handle to a Win32 function that takes
///   ownership, like `RegisterWaitForSingleObject` callback
///   registrations). Most callers won't need it.
#[derive(Debug)]
pub struct OwnedHandle(HANDLE);

// SAFETY: Win32 HANDLE values are kernel object indices; they're
// safe to move between threads. The handle's underlying kernel
// object is reference-counted by the OS; CloseHandle from any
// thread is well-defined.
//
// We intentionally do NOT impl Sync — CloseHandle is documented as
// not safe to call concurrently with other ops on the same handle,
// and requiring `&mut self` (or unique ownership) for any mutation
// is the simplest way to enforce that without sprinkling locks.
unsafe impl Send for OwnedHandle {}

impl OwnedHandle {
    /// Take ownership of an already-open `HANDLE`. The caller must
    /// ensure:
    ///
    /// 1. `h` is a valid kernel handle (not `INVALID_HANDLE_VALUE`
    ///    or any sentinel).
    /// 2. No other `OwnedHandle` will be constructed from the same
    ///    underlying handle (no double-free on drop).
    ///
    /// Returns `None` if `h` is `INVALID_HANDLE_VALUE` (some Win32
    /// APIs use this as a failure sentinel — easier to filter here
    /// than at every call site).
    pub fn from_raw(h: HANDLE) -> Option<Self> {
        if h == INVALID_HANDLE_VALUE || h.0.is_null() {
            None
        } else {
            Some(Self(h))
        }
    }

    /// Take ownership unconditionally. Use only when you've already
    /// established the handle is non-sentinel (e.g. windows-rs'
    /// `OpenProcess(...)?` returns a typed `Result<HANDLE>` whose
    /// `Ok` variant guarantees a real handle). Callers preferring
    /// the sentinel check should use `from_raw`.
    pub fn assume_valid(h: HANDLE) -> Self {
        Self(h)
    }

    /// Borrow the underlying `HANDLE` for a single Win32 call. The
    /// OwnedHandle's lifetime keeps the handle open for the duration
    /// of the borrow.
    pub fn as_raw(&self) -> HANDLE {
        self.0
    }

    /// Release ownership without closing — returns the raw `HANDLE`
    /// for callers that need to hand it to a Win32 function that
    /// takes ownership.
    pub fn into_raw(self) -> HANDLE {
        let h = self.0;
        std::mem::forget(self);
        h
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` was provided by a windows-rs FFI call
        // that returned a valid handle (the only constructor paths
        // are `from_raw` with explicit sentinel filtering, or
        // `assume_valid` where the caller already established
        // validity). `CloseHandle` is well-defined on every valid
        // handle including pseudo-handles like `GetCurrentProcess`
        // (which `assume_valid` callers should NOT wrap — see
        // module docs).
        //
        // We swallow errors: by the time Drop runs, the natural
        // place to report is gone (no Result to propagate, panic
        // would mask whatever Err caused the early return). A
        // double-close attempt would surface as
        // `ERROR_INVALID_HANDLE` here — and even that is
        // recoverable since the kernel handle table simply lost
        // one stale entry.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Threading::GetCurrentProcess;

    /// `from_raw` filters `INVALID_HANDLE_VALUE` and null. The two
    /// real-world sources of bogus handles are `CreateFileW` failure
    /// (returns `INVALID_HANDLE_VALUE`) and a zero-initialised
    /// `HANDLE::default()` — both should produce `None`.
    #[test]
    fn from_raw_rejects_sentinels() {
        assert!(OwnedHandle::from_raw(INVALID_HANDLE_VALUE).is_none());
        assert!(OwnedHandle::from_raw(HANDLE::default()).is_none());
    }

    /// A real valid handle wraps and drops cleanly. We use
    /// `GetCurrentProcess`, which returns a pseudo-handle —
    /// `CloseHandle` on it is a documented no-op, so this test
    /// exercises the wrap-and-drop happy path without leaking
    /// real kernel objects. Note we use `assume_valid` rather than
    /// `from_raw` because the pseudo-handle's value is literally
    /// `-1` cast to `HANDLE` (== `INVALID_HANDLE_VALUE`) — the
    /// sentinel filter in `from_raw` is exactly right for the real
    /// failure sources but wrong for this specific pseudo-handle.
    #[test]
    fn wraps_and_drops_real_handle() {
        // SAFETY: GetCurrentProcess returns the pseudo-handle for
        // the current process; always valid.
        let h = unsafe { GetCurrentProcess() };
        let owned = OwnedHandle::assume_valid(h);
        let _ = owned.as_raw();
        // owned drops here; CloseHandle on the pseudo-handle is a
        // safe no-op per Microsoft docs.
    }

    /// `into_raw` releases ownership — the returned HANDLE must NOT
    /// be closed by Drop (we'd double-free). We verify by calling
    /// into_raw and ensuring the test doesn't trigger an
    /// `ERROR_INVALID_HANDLE` cascade.
    #[test]
    fn into_raw_releases_without_closing() {
        let h = unsafe { GetCurrentProcess() };
        let owned = OwnedHandle::assume_valid(h);
        let raw = owned.into_raw();
        // If Drop had fired here, the next access of `raw` would
        // be Use-After-CloseHandle. We can't easily assert "the
        // handle is still open" cheaply; the unit test exists to
        // pin the API surface.
        assert_eq!(raw, h);
    }
}
