//! Build-gate predicate: does the running Windows build support the
//! v0.7 closed-loop ETW consumer?
//!
//! Per `audit/v0.7-architecture.md` §2.1 "Build gate" + Phase 2
//! sign-off Decision 1: closed-loop measurement requires Windows 11
//! 24H2 (build 26100) or later. Below that, the engine runs in v0.6
//! static-rule mode and the ETW session is never started.
//!
//! The probe calls `RtlGetVersion` (NOT `GetVersionExA`, which
//! manifest-lies on Win11). Result is cached behind a `OnceLock` so
//! repeated calls are free.
//!
//! Test seam (per `spike/group-a-week-2-plan.md` §3.1 v4.2 amendment
//! — Finding 1): a `#[cfg(test)]` thread_local override lets tests
//! substitute synthetic build numbers and simulate `RtlGetVersion`
//! failures without invoking the real Windows API. Production builds
//! compile the override out entirely. Build_gate's seam is separate
//! from `crates/etw/src/session.rs`'s `EtwSysCalls` trait (§3.4
//! v4.2): the trait bundles six session-API calls and lands on Day 3;
//! build_gate's one-call seam lands on Day 1 with this module.

use std::sync::OnceLock;

/// Minimum Windows build for v0.7 closed-loop measurement.
/// Per architecture §2.1 "Build gate" (Phase 2 sign-off Decision 1):
/// Windows 11 24H2 = build 26100.
pub const MIN_BUILD_FOR_CLOSED_LOOP: u32 = 26100;

/// Cached result of the first real-path probe.
/// `Some(Some(build))` = probe returned a build number.
/// `Some(None)`        = probe failed (NTSTATUS != 0).
/// Outer `None`        = probe hasn't been called yet.
static CACHED_BUILD: OnceLock<Option<u32>> = OnceLock::new();

#[cfg(any(test, feature = "test-override"))]
thread_local! {
    /// Per-test override.
    /// `Some(Ok(build))`  → `detected_build()` returns `Some(build)`.
    /// `Some(Err(()))`    → `detected_build()` returns `None`
    ///                      (simulates `RtlGetVersion` failure).
    /// `None`             → fall through to the real cached probe.
    ///
    /// thread_local — not `static Mutex` — so parallel tests don't
    /// step on each other.
    static BUILD_OVERRIDE: std::cell::RefCell<Option<Result<u32, ()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test-only seam. Tests should prefer `BuildOverrideGuard::set(...)`
/// so a panic mid-test doesn't poison subsequent tests on the same
/// thread.
#[cfg(any(test, feature = "test-override"))]
pub fn set_build_override(v: Option<Result<u32, ()>>) {
    BUILD_OVERRIDE.with(|cell| *cell.borrow_mut() = v);
}

/// RAII override: resets the per-thread override on Drop so a panicking
/// test can't leak its override into the next test on the same thread.
#[cfg(any(test, feature = "test-override"))]
pub struct BuildOverrideGuard;

#[cfg(any(test, feature = "test-override"))]
impl BuildOverrideGuard {
    pub fn set(v: Option<Result<u32, ()>>) -> Self {
        set_build_override(v);
        Self
    }
}

#[cfg(any(test, feature = "test-override"))]
impl Drop for BuildOverrideGuard {
    fn drop(&mut self) {
        set_build_override(None);
    }
}

/// Returns true iff the running Windows build supports the v0.7
/// closed-loop subsystem.
pub fn closed_loop_enabled_for_this_build() -> bool {
    detected_build().is_some_and(|b| b >= MIN_BUILD_FOR_CLOSED_LOOP)
}

/// Returns the detected build number, or `None` if the probe failed
/// (extremely unusual; logged at INFO on first probe). Repeated calls
/// are free — the result is cached in `CACHED_BUILD`.
pub fn detected_build() -> Option<u32> {
    #[cfg(any(test, feature = "test-override"))]
    {
        if let Some(override_val) = BUILD_OVERRIDE.with(|cell| *cell.borrow()) {
            return override_val.ok();
        }
    }
    *CACHED_BUILD.get_or_init(probe_build)
}

#[cfg(windows)]
fn probe_build() -> Option<u32> {
    // RtlGetVersion's binding is in Wdk::System::SystemServices in
    // windows-rs 0.58 (links ntdll.dll fn). OSVERSIONINFOW is the
    // struct it expects — the smaller of the two (dwBuildNumber is
    // present in both). Plan §3.1 originally pointed at
    // Win32::System::SystemInformation + OSVERSIONINFOEXW; Day 1
    // verification surfaced the correct path. To be folded into a
    // plan fix-up alongside the Day 5 EOD report.
    use windows::Wdk::System::SystemServices::RtlGetVersion;
    use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    // SAFETY: RtlGetVersion's documented contract requires a
    // writable OSVERSIONINFOW pointer with dwOSVersionInfoSize
    // populated. We initialize size from size_of so it matches the
    // struct layout the API expects; the pointer comes from an
    // exclusive `&mut` borrow of a local, so aliasing is impossible.
    // The function writes the version fields and returns NTSTATUS
    // (>= 0 == success). The binding lives in ntdll.dll which is
    // always loaded; no LoadLibrary indirection.
    let status = unsafe { RtlGetVersion(&mut info) };
    if status.0 >= 0 {
        Some(info.dwBuildNumber)
    } else {
        tracing::info!(
            ntstatus = ?status,
            "RtlGetVersion probe failed; closed-loop will fall back to static-rule mode"
        );
        None
    }
}

#[cfg(not(windows))]
fn probe_build() -> Option<u32> {
    // Non-Windows hosts can't query the build. Cross-check job uses
    // this stub so workspace cargo check stays green on Linux.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicate_true_at_synthetic_build_at_or_above_threshold() {
        let _guard = BuildOverrideGuard::set(Some(Ok(MIN_BUILD_FOR_CLOSED_LOOP)));
        assert!(closed_loop_enabled_for_this_build());
        assert_eq!(detected_build(), Some(MIN_BUILD_FOR_CLOSED_LOOP));
    }

    #[test]
    fn predicate_false_at_synthetic_build_below_threshold() {
        // 22631 = Win11 23H2; below the 26100 = Win11 24H2 threshold.
        let _guard = BuildOverrideGuard::set(Some(Ok(22631)));
        assert!(!closed_loop_enabled_for_this_build());
        assert_eq!(detected_build(), Some(22631));
    }

    #[test]
    fn predicate_false_on_synthetic_rtlgetversion_failure() {
        let _guard = BuildOverrideGuard::set(Some(Err(())));
        assert!(!closed_loop_enabled_for_this_build());
        assert_eq!(detected_build(), None);
    }

    /// Side-channel verification of the real `RtlGetVersion` binding on
    /// Windows (Windows batch Step 12 / `mac-side-uncertainties.md`
    /// Entry 1). The synthetic-override tests above exercise the seam
    /// but never invoke the real probe; this test does, asserts the
    /// probe succeeded (Some) and the value satisfies the gate's
    /// contract (>= MIN_BUILD_FOR_CLOSED_LOOP on supported hosts), and
    /// `eprintln`s the value so a human reviewer can cross-check it
    /// against `[System.Environment]::OSVersion.Version.Build` on the
    /// test host.
    ///
    /// `#[ignore]`'d so it doesn't run in the default fast-feedback
    /// loop; invoked explicitly via `cargo test ... -- --include-ignored`
    /// in the Windows runtime batch.
    #[cfg(windows)]
    #[test]
    #[ignore = "deferred to Windows runtime batch (real RtlGetVersion sanity)"]
    fn real_rtl_get_version_probe_succeeds_on_supported_host() {
        // No override -> falls through to real probe. CACHED_BUILD may
        // already be populated by an earlier test in the same binary
        // run; that's fine — the cache hit returns the same value the
        // probe would have produced.
        let build = detected_build();
        eprintln!(
            "real RtlGetVersion: detected_build() = {build:?} (expect Some(>= {})); MIN_BUILD_FOR_CLOSED_LOOP = {MIN_BUILD_FOR_CLOSED_LOOP}",
            MIN_BUILD_FOR_CLOSED_LOOP
        );
        assert!(
            build.is_some(),
            "real RtlGetVersion returned None; expected Some(build) on Win11"
        );
        let b = build.unwrap();
        assert!(
            b >= MIN_BUILD_FOR_CLOSED_LOOP,
            "real build {b} below MIN_BUILD_FOR_CLOSED_LOOP = {MIN_BUILD_FOR_CLOSED_LOOP}; \
             this test was designed for supported hosts only"
        );
    }
}
