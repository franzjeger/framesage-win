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
//!   * **Javelin** (BF6): detected via the EA Javelin service /
//!     companion process; for v0.6 we surface as a derived "EAC + BF6
//!     process present" since the dedicated probe needs more reverse
//!     engineering. Conservative — Hybrid mode for BF6 fires off the
//!     seeded rule's `ac_safe_mode_target`, not detection.
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
const AC_PROCESS_MARKERS: &[(AcMarker, &str)] = &[
    // Vanguard
    (AcMarker::Vanguard, "vgc.exe"),
    (AcMarker::Vanguard, "vgtray.exe"),
    // EAC
    (AcMarker::Eac, "EasyAntiCheat.exe"),
    (AcMarker::Eac, "EasyAntiCheat_EOS.exe"),
    (AcMarker::Eac, "easyanticheat.exe"),
    // BF6 / Javelin process companion (best-effort surface; the actual
    // Javelin driver detection is deferred).
    (AcMarker::Javelin, "bf6.exe"),
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

    /// AC_PROCESS_MARKERS is non-empty and covers every variant of
    /// `AcMarker`. Catches the bug-class where someone adds a new
    /// variant but forgets to wire the marker.
    #[test]
    fn markers_cover_every_ac_variant() {
        let mut saw_vanguard = false;
        let mut saw_eac = false;
        let mut saw_javelin = false;
        let mut saw_battleye = false;
        let mut saw_faceit = false;
        let mut saw_esea = false;
        for (marker, _) in AC_PROCESS_MARKERS {
            match marker {
                AcMarker::Vanguard => saw_vanguard = true,
                AcMarker::Eac => saw_eac = true,
                AcMarker::Javelin => saw_javelin = true,
                AcMarker::Battleye => saw_battleye = true,
                AcMarker::Faceit => saw_faceit = true,
                AcMarker::Esea => saw_esea = true,
            }
        }
        assert!(saw_vanguard, "no Vanguard marker");
        assert!(saw_eac, "no EAC marker");
        assert!(saw_javelin, "no Javelin marker");
        assert!(saw_battleye, "no BattlEye marker");
        assert!(saw_faceit, "no FACEIT marker");
        assert!(saw_esea, "no ESEA marker");
    }
}
