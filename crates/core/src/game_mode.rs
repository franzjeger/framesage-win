//! System-level Game Mode actions — what a profile *asks for*, not what it
//! resolves to.
//!
//! The types here live in `framesage-core` so a `Profile` (and therefore a
//! `Policy`) can carry them on the wire and round-trip through JSON without
//! pulling in the heavier `framesage-gamemode` crate (which owns the curated
//! safe-list, the planner, and the journal). Anything that's just a *value* —
//! "what to do" — belongs here. Anything that's *behaviour* — "how to do it",
//! "what's safe to do" — belongs in `framesage-gamemode`.

use serde::{Deserialize, Serialize};

/// What a profile wants done at the system level when it becomes active.
///
/// Every field is opt-in. An empty `GameModeActions::default()` is a no-op:
/// the engine notices "the profile has Some(game_mode)" but plans nothing.
/// This shape is deliberate — it means an unattended user can opt in to one
/// piece (e.g. just hide the taskbar) without inheriting an aggressive default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameModeActions {
    /// Hide the primary taskbar and any secondary multi-monitor taskbars for
    /// the duration of the profile. Reverts on profile-exit. Survives crashes
    /// via the engine's revert journal.
    pub hide_taskbar: bool,

    /// Desired Focus Assist / "Do Not Disturb" mode while the profile is
    /// active. `None` means "don't touch the current setting."
    pub focus_assist: Option<FocusAssistMode>,

    /// Stop these Windows services for the duration of the profile, then
    /// start them again on exit. Only entries that appear in the engine's
    /// curated safe-list are honoured — unknown ids are logged and skipped.
    /// Service short names, e.g. `"SysMain"`, `"WSearch"`.
    pub stop_services: Vec<String>,

    /// Suspend (not kill) these background processes — same safe-list gate as
    /// services. Process executable names, case-insensitive,
    /// e.g. `"OneDrive.exe"`, `"Dropbox.exe"`.
    pub suspend_processes: Vec<String>,

    /// Switch to this power plan; revert on exit. `None` leaves the current
    /// plan alone.
    pub power_plan: Option<PowerPlanId>,

    /// Pause Windows Update for the duration of the profile. Stubbed in v0.1
    /// of game-mode — recorded in the plan, surfaced in logs, but the actual
    /// `UsoClient` / WUA-COM wiring lands in a follow-up.
    pub pause_windows_update: bool,
}

/// Focus Assist (Windows 10/11 "Do Not Disturb") mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusAssistMode {
    /// All notifications visible.
    Off,
    /// Only priority-list contacts + apps shown.
    PriorityOnly,
    /// Only alarms break through.
    AlarmsOnly,
}

/// Windows power plan identifier.
///
/// Well-known plans are referenced by name so a profile authored on one
/// machine works on another even if the GUIDs differ in some edge cases.
/// `Custom(<guid>)` is the escape hatch for vendor plans (ASUS, ROG,
/// "Ryzen High Performance", etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum PowerPlanId {
    /// Well-known: Balanced (`381b4222-f694-41f0-9685-ff5bb260df2e`).
    Balanced,
    /// Well-known: High Performance (`8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c`).
    HighPerformance,
    /// Well-known: Power Saver (`a1841308-3541-4fab-bc81-f71556f20b4a`).
    PowerSaver,
    /// Well-known: Ultimate Performance
    /// (`e9a42b02-d5df-448d-aa00-03f14749eb61`). Hidden by default on most
    /// SKUs; created on demand via `powercfg /duplicatescheme` on first apply.
    UltimatePerformance,
    /// Custom GUID — anything `powercfg /list` will accept.
    Custom(String),
}

impl PowerPlanId {
    /// Canonical Windows GUID for this plan. For `Custom`, returns the value
    /// stored in the variant verbatim.
    pub fn guid(&self) -> &str {
        match self {
            PowerPlanId::Balanced => "381b4222-f694-41f0-9685-ff5bb260df2e",
            PowerPlanId::HighPerformance => "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c",
            PowerPlanId::PowerSaver => "a1841308-3541-4fab-bc81-f71556f20b4a",
            PowerPlanId::UltimatePerformance => "e9a42b02-d5df-448d-aa00-03f14749eb61",
            PowerPlanId::Custom(g) => g.as_str(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_actions_is_a_noop() {
        let a = GameModeActions::default();
        assert!(!a.hide_taskbar);
        assert!(a.focus_assist.is_none());
        assert!(a.stop_services.is_empty());
        assert!(a.suspend_processes.is_empty());
        assert!(a.power_plan.is_none());
        assert!(!a.pause_windows_update);
    }

    #[test]
    fn well_known_plan_guids_are_canonical() {
        // Microsoft's documented GUIDs — these must match what `powercfg /list`
        // shows on a clean install. If a Windows version ever changes one of
        // these (none has, in twenty years), this test catches it.
        assert_eq!(
            PowerPlanId::Balanced.guid(),
            "381b4222-f694-41f0-9685-ff5bb260df2e"
        );
        assert_eq!(
            PowerPlanId::HighPerformance.guid(),
            "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c"
        );
        assert_eq!(
            PowerPlanId::PowerSaver.guid(),
            "a1841308-3541-4fab-bc81-f71556f20b4a"
        );
        assert_eq!(
            PowerPlanId::UltimatePerformance.guid(),
            "e9a42b02-d5df-448d-aa00-03f14749eb61"
        );
    }

    #[test]
    fn custom_plan_round_trip_preserves_guid() {
        let custom = PowerPlanId::Custom("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into());
        assert_eq!(custom.guid(), "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");

        let json = serde_json::to_string(&custom).expect("serialize");
        let parsed: PowerPlanId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, custom);
    }

    #[test]
    fn actions_round_trip_through_json_preserves_every_field() {
        let original = GameModeActions {
            hide_taskbar: true,
            focus_assist: Some(FocusAssistMode::PriorityOnly),
            stop_services: vec!["SysMain".into(), "WSearch".into()],
            suspend_processes: vec!["OneDrive.exe".into()],
            power_plan: Some(PowerPlanId::UltimatePerformance),
            pause_windows_update: true,
        };
        let json = serde_json::to_string_pretty(&original).expect("serialize");
        let parsed: GameModeActions = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(parsed, original);
    }
}
