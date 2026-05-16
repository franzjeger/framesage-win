//! Profile: a bundle of policy decisions to apply to a process.
//!
//! Each knob maps to a specific Win32 mechanism in `framesage-sys`; see the
//! `apply` module there. Every knob is optional — `None` means "leave the OS
//! default alone." This is important: profiles must compose by overlay, and an
//! unset field must never overwrite a setting from a higher-priority profile.

use serde::{Deserialize, Serialize};

use crate::game_mode::GameModeActions;
use crate::topology::CpuSelector;

/// Stable identifier for a profile inside a `Policy`. UTF-8 string, opaque to
/// the engine but human-readable for log lines and CLI output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProfileId(pub String);

impl From<&str> for ProfileId {
    fn from(s: &str) -> Self {
        ProfileId(s.to_owned())
    }
}

impl std::fmt::Display for ProfileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Maps to `PROCESS_INFORMATION_CLASS::ProcessIoPriority`. Five levels; the
/// kernel uses these to bias the disk queue. VeryLow is the right setting for
/// indexers, telemetry, scheduled scans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoPriority {
    VeryLow,
    Low,
    Normal,
    High,
    Critical,
}

impl std::fmt::Display for IoPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::VeryLow => "Very low",
            Self::Low => "Low",
            Self::Normal => "Normal",
            Self::High => "High",
            Self::Critical => "Critical",
        };
        f.write_str(s)
    }
}

/// Maps to `PROCESS_INFORMATION_CLASS::ProcessMemoryPriority`. 1..=5. Lower
/// values get trimmed from the working set first under memory pressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPriority {
    VeryLow,
    Low,
    Medium,
    BelowNormal,
    Normal,
}

impl MemoryPriority {
    pub fn as_u32(self) -> u32 {
        match self {
            Self::VeryLow => 1,
            Self::Low => 2,
            Self::Medium => 3,
            Self::BelowNormal => 4,
            Self::Normal => 5,
        }
    }
}

impl std::fmt::Display for MemoryPriority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::VeryLow => "Very low",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::BelowNormal => "Below normal",
            Self::Normal => "Normal",
        };
        f.write_str(s)
    }
}

/// `SetPriorityClass` levels. We don't expose Realtime by default — it can
/// freeze the desktop and there's almost never a real reason for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriorityClass {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
}

impl std::fmt::Display for PriorityClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Idle => "Idle",
            Self::BelowNormal => "Below normal",
            Self::Normal => "Normal",
            Self::AboveNormal => "Above normal",
            Self::High => "High",
        };
        f.write_str(s)
    }
}

/// Maps to `PROCESS_POWER_THROTTLING_STATE`.
///
/// `Eco` tells the scheduler "I'm a background task" — on hybrid silicon this
/// pins to E-cores and reduces frequency targets. `Performance` is the inverse:
/// "do not throttle me, I'm latency-sensitive." `SystemDefault` removes any
/// override we previously set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerThrottlingMode {
    Eco,
    Performance,
    SystemDefault,
}

impl std::fmt::Display for PowerThrottlingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Eco => "Eco (background)",
            Self::Performance => "Performance (no throttle)",
            Self::SystemDefault => "System default",
        };
        f.write_str(s)
    }
}

/// A profile is a set of (optional) policy overrides for a process.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Profile {
    pub id: ProfileId,

    /// Human-readable description for the UI.
    #[serde(default)]
    pub description: String,

    /// `SetProcessDefaultCpuSets` target. Preferred over raw affinity because
    /// it's a *hint* the scheduler can override under load — no starvation if
    /// the favored cores are pinned by something higher priority.
    #[serde(default)]
    pub cpu_sets: Option<CpuSelector>,

    /// Hard affinity fallback. Use only when CPU Sets aren't enough (rare).
    #[serde(default)]
    pub affinity_mask: Option<CpuSelector>,

    #[serde(default)]
    pub power_throttling: Option<PowerThrottlingMode>,

    #[serde(default)]
    pub priority_class: Option<PriorityClass>,

    #[serde(default)]
    pub io_priority: Option<IoPriority>,

    #[serde(default)]
    pub memory_priority: Option<MemoryPriority>,

    /// Empty the working set on apply. Useful for forcing background apps to
    /// release RAM before a heavy foreground app launches.
    #[serde(default)]
    pub trim_working_set: bool,

    /// System-level "Game Mode" actions applied while this profile is the
    /// active foreground profile. Hide-taskbar, stop-services, suspend-
    /// processes, switch-power-plan, etc. The engine plans these against the
    /// curated safe-list in `framesage-gamemode`; unknown ids are rejected
    /// during planning, not at apply time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_mode: Option<GameModeActions>,

    /// When the foreground moves OFF a process holding this profile, leave
    /// the per-process state in place (affinity, priority, I/O priority) and
    /// keep the process tracked. The pin only releases when the process
    /// itself exits. This is the right behavior for game pins — a game
    /// should stay on the X3D CCD even while the user briefly alt-tabs to
    /// a browser, a chat client, or Task Manager.
    ///
    /// Non-persistent profiles (e.g. `perf` for casual foreground tracking)
    /// revert on focus loss, matching the original short-lived semantics.
    #[serde(default)]
    pub persistent: bool,

    /// Item 1.9 / audit `audit/research/ANTI-CHEAT-MATRIX.md`.
    ///
    /// Which anti-cheat-aware behavior tier the engine should apply when
    /// this profile fires. Drives whether direct game-process
    /// modifications (affinity, priority, CPU sets, I/O priority, power
    /// throttling) are skipped — i.e. whether the profile is allowed to
    /// touch the game process itself, or only the environment around it
    /// (services, processes-to-suspend, power plan, taskbar).
    ///
    /// Defaults to `Aggressive` via serde so existing policy.json files
    /// migrate cleanly to the most permissive (and historically-existing)
    /// behavior. The seeded BF6 / Valorant rules ship with explicit
    /// non-Aggressive values per defaults D-9 + D-10.
    #[serde(default)]
    pub ac_safe_mode_target: AntiCheatProfile,

    /// Item 2.11. When true, this profile shows up in the tray's
    /// "Manual Global Game Mode" picker — meaning the user can
    /// explicitly enter this profile's `game_mode` actions
    /// system-wide without needing a matching foreground process.
    /// Useful for "quiet desktop" presets (stop OneDrive / WSearch /
    /// BITS, hide taskbar, switch to High Performance) that aren't
    /// tied to a specific app.
    ///
    /// Defaults to false so existing policies stay opt-in: the
    /// picker won't suddenly fill up with every game profile the
    /// user has authored. The seeded `quiet-desktop` profile (if
    /// any future migration adds one) would set this to true.
    #[serde(default)]
    pub manual_global_eligible: bool,
}

/// Anti-cheat-aware safety tier for a profile. See
/// `audit/research/ANTI-CHEAT-MATRIX.md` for the full rationale and the
/// per-AC verdicts that drove each tier's design.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AntiCheatProfile {
    /// Full sledgehammer — every knob in the Profile applies, including
    /// direct modifications to the game process (affinity, priority,
    /// CPU sets, I/O priority, power throttling) AND the system-wide
    /// Game Mode actions (stop services, suspend processes, power
    /// plan, taskbar hide). Default for fresh policies and for games
    /// with no AC concerns (single-player, EAC-with-no-Javelin titles).
    #[default]
    Aggressive,
    /// Hybrid mode — environment actions (services / processes /
    /// power / taskbar) at full strength, but the game process itself
    /// is left alone. For BF6 + EA Javelin: Javelin actively blocks
    /// core parking / affinity changes on dual-CCD Ryzen during
    /// multiplayer; the press has named Process Lasso as risk-bearing.
    /// We keep the environment optimizations (free up CPU / RAM /
    /// power for the game) but don't poke the game process directly.
    Hybrid,
    /// AC-Safe Mode — environment actions still fire, game process
    /// modifications NEVER fire even if the user-authored profile
    /// requests them. For Vanguard (Valorant): Riot reserves hardware
    /// bans; documented VAN: Competitive Restrictions have hit
    /// Process Lasso users; Riot explicitly will not allowlist any
    /// third-party tool. Mirrors Hone's 1M+-user model — touch the
    /// environment, never the game. Also the right default for FACEIT-
    /// /ESEA-protected CS2 matches.
    SafeMode,
    /// Engine STANDBY mode — no rule writes, no scans, no actions
    /// fire while the corresponding AC is detected running. Used for
    /// ESEA per audit research: ESEA's vendor support KB explicitly
    /// names Process Lasso as a conflict (Error #107, "uninstall to
    /// fix"). FrameSage going dark while ESEAClient.exe runs
    /// sidesteps the conflict entirely. Resumes automatically when
    /// the AC exits.
    Disabled,
}

impl Profile {
    pub fn new(id: impl Into<ProfileId>) -> Self {
        Self {
            id: id.into(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_impls_use_human_friendly_labels() {
        assert_eq!(IoPriority::VeryLow.to_string(), "Very low");
        assert_eq!(IoPriority::Critical.to_string(), "Critical");

        assert_eq!(MemoryPriority::VeryLow.to_string(), "Very low");
        assert_eq!(MemoryPriority::BelowNormal.to_string(), "Below normal");

        assert_eq!(PriorityClass::AboveNormal.to_string(), "Above normal");
        assert_eq!(PriorityClass::BelowNormal.to_string(), "Below normal");
        assert_eq!(PriorityClass::High.to_string(), "High");

        assert_eq!(PowerThrottlingMode::Eco.to_string(), "Eco (background)");
        assert_eq!(
            PowerThrottlingMode::Performance.to_string(),
            "Performance (no throttle)"
        );
        assert_eq!(
            PowerThrottlingMode::SystemDefault.to_string(),
            "System default"
        );
    }
}
