//! Anti-cheat presence detection — what's running on this machine right
//! now.
//!
//! Lives in `core` (not `sys`) because both the engine (Windows) and the
//! sim crate (cross-platform) need to reason about it. The actual
//! detection probe — enumerating loaded drivers + running services +
//! process exes — lives in `framesage_sys::ac_detect`; this module
//! defines the data shape they share.
//!
//! Per `audit/research/ANTI-CHEAT-MATRIX.md`, five kernel anti-cheats
//! drive different per-rule behavior:
//!
//! * **Vanguard** (Valorant) → SafeMode default. Hardware bans
//!   reserved; documented VAN: Competitive Restrictions on Process
//!   Lasso users.
//! * **EAC** (Fortnite, Apex, Elden Ring, Rust) → Aggressive ok. Strip-
//!   rights model, no ban precedent for our access pattern.
//! * **EA Javelin** (Battlefield 6, layered on EAC infrastructure) →
//!   Hybrid. Javelin actively blocks core parking / affinity on dual-
//!   CCD Ryzen; press named Process Lasso as risk-bearing.
//! * **BattlEye** (PUBG, R6, Tarkov, DayZ, ARMA, Squad, Destiny 2) →
//!   Aggressive ok IF signed binary + min-rights handle pattern. File-
//!   block list precedent for unsigned helpers.
//! * **FACEIT AC** / **ESEA** (CS2 third-party leagues) → SafeMode +
//!   STANDBY respectively. ESEA explicitly names Process Lasso in
//!   support KB.

use serde::{Deserialize, Serialize};

/// Snapshot of which anti-cheat drivers / services / processes are
/// currently visible on this machine. Each field is independently
/// detected by the probe in `framesage_sys::ac_detect`. Multiple ACs
/// can be active simultaneously (e.g. Vanguard + EAC if Valorant +
/// Fortnite both have their drivers loaded).
///
/// Default = nothing detected — what the engine sees on a fresh boot
/// before any AC-protected game has been launched in this session.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AntiCheatPresence {
    /// `vgk.sys` kernel driver loaded OR `vgc.exe` running. Riot
    /// Vanguard. Always-on kernel driver once Valorant has been
    /// launched even once since boot.
    pub vanguard: bool,
    /// `EasyAntiCheat.sys` loaded OR `EasyAntiCheat.exe` /
    /// `EasyAntiCheat_EOS.exe` running. Epic/Tencent EAC. Per-session
    /// (the driver loads on game launch, unloads on exit) — so this
    /// flag flips as games come and go.
    pub eac: bool,
    /// EA Javelin (BF6's kernel driver). Detected via the Javelin
    /// service or its driver. Co-exists with EAC infrastructure but
    /// behaves differently — Javelin actively blocks affinity edits.
    pub javelin: bool,
    /// `BEDaisy.sys` loaded OR `BEService.exe` running.
    pub battleye: bool,
    /// `FACEIT_AC.sys` loaded OR `FACEITService.exe` / `FACEIT_AC.exe`
    /// running. Kernel-mode AC layered on top of VAC for CS2 third-
    /// party matchmaking.
    pub faceit: bool,
    /// `ESEAClient.exe` / `eseaclient_x64.exe` running. ESEA support
    /// explicitly names Process Lasso as a conflict (Error #107,
    /// "uninstall"). FrameSage auto-pauses while this is detected.
    pub esea: bool,
}

impl AntiCheatPresence {
    /// Any AC active on this box? Used by the engine to decide
    /// whether to consult the per-AC profile-tier table at all.
    pub fn any_present(&self) -> bool {
        self.vanguard || self.eac || self.javelin || self.battleye || self.faceit || self.esea
    }

    /// True if ESEA is running, in which case the engine enters
    /// STANDBY (no rules apply, no scans, no actions). Per defaults
    /// D-11 + the AC matrix research, ESEA + FrameSage produces
    /// Error #107; we go dark to sidestep.
    pub fn esea_demands_standby(&self) -> bool {
        self.esea
    }

    /// True if any AC is detected for which Windows Update health
    /// matters at game launch — specifically FACEIT, which refuses
    /// to launch when WU is broken or paused for too long. When
    /// FACEIT is present, the engine should refuse to stop wuauserv /
    /// UsoSvc / WaaSMedicSvc and refuse to pause WU via the
    /// registry. Per AC matrix table rows 19 + 20.
    pub fn refuses_wu_pause(&self) -> bool {
        self.faceit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_presence_is_all_false_and_any_present_is_false() {
        let p = AntiCheatPresence::default();
        assert!(!p.any_present());
        assert!(!p.esea_demands_standby());
        assert!(!p.refuses_wu_pause());
    }

    #[test]
    fn esea_alone_demands_standby() {
        let p = AntiCheatPresence {
            esea: true,
            ..Default::default()
        };
        assert!(p.any_present());
        assert!(p.esea_demands_standby());
    }

    #[test]
    fn faceit_alone_refuses_wu_pause() {
        let p = AntiCheatPresence {
            faceit: true,
            ..Default::default()
        };
        assert!(p.any_present());
        assert!(p.refuses_wu_pause());
        // FACEIT presence does NOT imply ESEA standby — these are
        // distinct ACs with distinct behaviors.
        assert!(!p.esea_demands_standby());
    }

    #[test]
    fn vanguard_alone_does_not_trigger_standby_or_wu_refusal() {
        let p = AntiCheatPresence {
            vanguard: true,
            ..Default::default()
        };
        assert!(p.any_present());
        assert!(!p.esea_demands_standby());
        assert!(!p.refuses_wu_pause());
        // Vanguard's protection is per-profile via AntiCheatProfile::
        // SafeMode (D-9), not a global standby / WU refusal.
    }

    #[test]
    fn presence_round_trips_through_json() {
        let p = AntiCheatPresence {
            vanguard: true,
            eac: false,
            javelin: true,
            battleye: false,
            faceit: true,
            esea: false,
        };
        let json = serde_json::to_string(&p).expect("serialize");
        let back: AntiCheatPresence = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(p, back);
    }
}
