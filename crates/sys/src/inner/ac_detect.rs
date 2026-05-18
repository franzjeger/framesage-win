//! Anti-cheat presence detection. Item 1.9 / AC matrix.
//!
//! Determines which kernel anti-cheats are currently active on this
//! machine by enumerating running processes and known driver/service
//! names. Cheap enough to run on every persistent-reassert tick (every
//! 2 s) — the heavy lift is the already-existing `iter_pids` snapshot
//! that the engine takes for other purposes.
//!
//! Detection strategy per AC:
//!   * **Vanguard**: `vgc.exe` or `vgtray.exe` process. Once Valorant
//!     has launched even once since boot, `vgk.sys` stays loaded — but
//!     the user-mode `vgc.exe` is what we actually probe.
//!   * **EAC**: `EasyAntiCheat.exe`, `EasyAntiCheat_EOS.exe`, or
//!     `easyanticheat.exe` process. Per-session — flips as games come
//!     and go.
//!   * **Javelin** (BF6): **detection deferred pending dedicated probe**
//!     (W1.2 / finding A-003). Earlier versions used `bf6.exe` as a
//!     heuristic marker, but the exe name is too generic — unrelated
//!     tools and games named bf6.exe trivially false-positive,
//!     silently flipping the Javelin presence bit and suppressing
//!     per-game-process modifications via the
//!     `AntiCheatProfile::Hybrid` apply-path at engine/lib.rs:3376-3406.
//!     `AntiCheatPresence.javelin` remains permanently `false` in v0.7
//!     until a dedicated probe (Javelin service / driver enumeration)
//!     wires it. Hybrid mode for BF6 still fires correctly because the
//!     `ac_safe_mode_target: Hybrid` tier is set statically on the
//!     seeded BF6 profile, not driven by AC-presence detection.
//!   * **BattlEye**: `BEService.exe` process.
//!   * **FACEIT**: `FACEITService.exe`, `FACEIT_AC.exe`, or
//!     `FACEIT_Start_Protected_Game.exe`.
//!   * **ESEA**: `ESEAClient.exe` or `eseaclient_x64.exe`.
//!
//! Future hardening (deferred): enumerate loaded kernel drivers via
//! `EnumDeviceDrivers` to catch the driver-loaded-no-userland case
//! (e.g. game crashed but driver lingers). For v0.6 the user-mode
//! probe is sufficient because every AC ships a companion service /
//! tray and our use case (tell the engine which mode to apply at
//! game-launch time) is well-served.

use anyhow::Result;

use framesage_core::AntiCheatPresence;

use crate::inner::process::{exe_for_pid, iter_pids};

/// Compile-time list of (AC marker, exe name) pairs. Case-insensitive
/// match against the bare exe filename from `exe_for_pid`. Multiple
/// process names per AC because the actual binary varies across
/// versions and OS environments.
///
/// Kept as a `&[(&str, &str)]` instead of grouped per-AC so the probe
/// loop is one pass over the live PID list, O(N × M) where N = PID
/// count (~300) and M = marker count (~10). Single-digit microseconds
/// in practice.
///
/// **`AcMarker::Javelin` is intentionally absent from this list.**
/// W1.2 / finding A-003 removed the only Javelin marker (`bf6.exe`)
/// because the exe name is too generic — any unrelated tool, mod, or
/// renamed binary named `bf6.exe` trivially false-positives via the
/// case-insensitive match below. The Javelin enum variant + the
/// `AntiCheatPresence.javelin` field stay so a future dedicated probe
/// (Javelin service / driver enumeration) can wire them without an
/// enum-variant addition; until then `presence.javelin` remains
/// permanently `false`. Inline tests below pin the empty-list state
/// as a regression guard.
const AC_PROCESS_MARKERS: &[(AcMarker, &str)] = &[
    // Vanguard
    (AcMarker::Vanguard, "vgc.exe"),
    (AcMarker::Vanguard, "vgtray.exe"),
    // EAC
    (AcMarker::Eac, "EasyAntiCheat.exe"),
    (AcMarker::Eac, "EasyAntiCheat_EOS.exe"),
    (AcMarker::Eac, "easyanticheat.exe"),
    // Javelin — intentionally no markers (W1.2). See doc-comment above.
    // BattlEye
    (AcMarker::Battleye, "BEService.exe"),
    (AcMarker::Battleye, "BEServiceLauncher.exe"),
    // FACEIT
    (AcMarker::Faceit, "FACEITService.exe"),
    (AcMarker::Faceit, "FACEIT_AC.exe"),
    (AcMarker::Faceit, "FACEIT_Start_Protected_Game.exe"),
    // ESEA
    (AcMarker::Esea, "ESEAClient.exe"),
    (AcMarker::Esea, "eseaclient_x64.exe"),
];

#[derive(Clone, Copy)]
enum AcMarker {
    Vanguard,
    Eac,
    /// W1.2 / A-003: no markers in AC_PROCESS_MARKERS reference this
    /// variant (the only candidate `bf6.exe` was too generic). The
    /// variant + the `AntiCheatPresence.javelin` field stay so a
    /// future dedicated probe (Javelin service / driver enumeration)
    /// can wire them without an enum-variant addition. Until then
    /// `presence.javelin` remains permanently `false`. The match-arm
    /// at `detect_anti_cheats` line ~112 covers this variant for
    /// exhaustiveness; the arm is reached only if a future
    /// AC_PROCESS_MARKERS entry uses `AcMarker::Javelin`.
    ///
    /// `#[allow(dead_code)]` rationale: kept-but-dormant pending
    /// dedicated Javelin probe (W1.2 / A-003).
    #[allow(dead_code)]
    Javelin,
    Battleye,
    Faceit,
    Esea,
}

/// Run the detection probe. Walks the live PID list once and returns
/// the union of detected ACs.
///
/// Errors only when the underlying `iter_pids` call fails — i.e. the
/// snapshot couldn't be taken. Individual `exe_for_pid` failures (PID
/// exited mid-walk, protected process) are silently treated as "no
/// match" because that's the correct semantic — we can't know what we
/// can't see.
///
/// Designed to be cheap: re-uses the same `iter_pids` infrastructure
/// the engine already runs for ProBalance and background scan. Caller
/// is expected to cache the result and re-probe every 2 s or so,
/// piggybacked on the persistent-reassert tick — not per-event.
pub fn detect_anti_cheats() -> Result<AntiCheatPresence> {
    let mut presence = AntiCheatPresence::default();

    let pids = iter_pids()?;
    for pid in pids {
        let exe_path = match exe_for_pid(pid) {
            Ok(Some(path)) => path,
            // PID exited mid-walk, or we can't open it (protected,
            // ACL-denied). Either way: not a useful signal, skip.
            Ok(None) | Err(_) => continue,
        };
        let exe_name = exe_path.rsplit(['\\', '/']).next().unwrap_or(&exe_path);

        for (marker, marker_exe) in AC_PROCESS_MARKERS {
            if exe_name.eq_ignore_ascii_case(marker_exe) {
                match marker {
                    AcMarker::Vanguard => presence.vanguard = true,
                    AcMarker::Eac => presence.eac = true,
                    AcMarker::Javelin => presence.javelin = true,
                    AcMarker::Battleye => presence.battleye = true,
                    AcMarker::Faceit => presence.faceit = true,
                    AcMarker::Esea => presence.esea = true,
                }
                // No need to keep checking other markers for this
                // exe — exe names don't overlap across ACs.
                break;
            }
        }

        // Early exit: if every AC is set, no point continuing the
        // scan. Pathological case (almost never happens — most boxes
        // run zero AC games at a time) but cheap to check.
        if presence.vanguard
            && presence.eac
            && presence.javelin
            && presence.battleye
            && presence.faceit
            && presence.esea
        {
            break;
        }
    }

    Ok(presence)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: the probe runs without error on the test host (any
    /// machine). The actual presence values depend on what's running
    /// at test time — we don't assert specific values because that
    /// would be flaky. Just verify the call shape.
    #[test]
    fn detect_does_not_error_on_clean_host() {
        let presence = detect_anti_cheats().expect("detect should not fail");
        // We don't know what's running on this test machine — could
        // be nothing (CI), could be Vanguard (dev box). Either is
        // valid; we just want to confirm the probe returns a value.
        let _ = presence.any_present();
    }

    /// AC_PROCESS_MARKERS covers every variant of `AcMarker` EXCEPT
    /// Javelin. Catches the bug-class where someone adds a new
    /// variant (e.g., a new AC vendor) but forgets to wire the
    /// marker. Javelin is split out into a separate test below
    /// because its empty-marker state is INTENTIONAL (W1.2 / A-003)
    /// and an "except Javelin" framing here would silently pass if
    /// someone re-introduced a too-generic Javelin marker. The
    /// explicit-empty test below trips loudly in that case.
    #[test]
    fn markers_cover_every_ac_variant_except_javelin() {
        let mut saw_vanguard = false;
        let mut saw_eac = false;
        let mut saw_battleye = false;
        let mut saw_faceit = false;
        let mut saw_esea = false;
        for (marker, _) in AC_PROCESS_MARKERS {
            match marker {
                AcMarker::Vanguard => saw_vanguard = true,
                AcMarker::Eac => saw_eac = true,
                AcMarker::Javelin => {
                    // Intentional no-op — see
                    // javelin_marker_list_is_intentionally_empty_pending_dedicated_probe
                    // below. A Javelin marker existing here is NOT
                    // a coverage win; it's a regression of W1.2.
                }
                AcMarker::Battleye => saw_battleye = true,
                AcMarker::Faceit => saw_faceit = true,
                AcMarker::Esea => saw_esea = true,
            }
        }
        assert!(saw_vanguard, "no Vanguard marker");
        assert!(saw_eac, "no EAC marker");
        assert!(saw_battleye, "no BattlEye marker");
        assert!(saw_faceit, "no FACEIT marker");
        assert!(saw_esea, "no ESEA marker");
    }

    /// W1.2 / finding A-003: the `AcMarker::Javelin` marker list is
    /// intentionally EMPTY in v0.7 because the only candidate exe
    /// name (`bf6.exe`) is too generic — unrelated tools, mods, or
    /// renamed binaries trivially false-positive via the
    /// case-insensitive match in `detect_anti_cheats`. Re-introducing
    /// any Javelin marker without a dedicated-probe replacement
    /// (Javelin service / driver enumeration) fails this test.
    ///
    /// `AntiCheatPresence.javelin` remains permanently `false` until
    /// the dedicated probe lands. Hybrid mode for BF6 is unaffected
    /// because `ac_safe_mode_target: Hybrid` is set statically on the
    /// seeded BF6 profile, not driven by AC-presence detection.
    #[test]
    fn javelin_marker_list_is_intentionally_empty_pending_dedicated_probe() {
        let javelin_count = AC_PROCESS_MARKERS
            .iter()
            .filter(|(marker, _)| matches!(marker, AcMarker::Javelin))
            .count();
        assert_eq!(
            javelin_count,
            0,
            "AcMarker::Javelin must have ZERO markers in v0.7 (W1.2 / \
             A-003). A re-introduced marker is a regression — see \
             module docstring + ac_detect.rs's AC_PROCESS_MARKERS \
             doc-comment for context. Offending entries: {:?}",
            AC_PROCESS_MARKERS
                .iter()
                .filter(|(marker, _)| matches!(marker, AcMarker::Javelin))
                .map(|(_, exe)| *exe)
                .collect::<Vec<_>>(),
        );
    }

    // Pin at the AC_PROCESS_MARKERS const level rather than via
    // detect_anti_cheats() output because detect_anti_cheats() calls
    // real iter_pids() + exe_for_pid() with no mock-injection seam.
    // Iterator-injection refactor is M-effort and out of scope for
    // W1.2; const-level pin is sufficient because there is no
    // transformation between the marker list and what detect_anti_cheats
    // consumes — a re-introduction of bf6.exe to the marker list
    // trips this test immediately. See finding A-003 + roadmap W1.2.
    #[test]
    fn bf6_exe_not_in_ac_marker_list() {
        // Collect the matched exe strings (not the marker enum, which
        // would require Debug on AcMarker). Two motivations matter
        // identically: prove the list is empty, and surface the
        // offending entries if not.
        let bf6_entries: Vec<&str> = AC_PROCESS_MARKERS
            .iter()
            .filter(|(_, exe)| exe.eq_ignore_ascii_case("bf6.exe"))
            .map(|(_, exe)| *exe)
            .collect();
        assert!(
            bf6_entries.is_empty(),
            "bf6.exe must not appear in AC_PROCESS_MARKERS — the exe \
             name is too generic for AC detection (W1.2 / A-003). \
             Offending entries: {bf6_entries:?}",
        );
    }
}
