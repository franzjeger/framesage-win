//! Policy: the user's configured ruleset and default profile.
//!
//! A `Policy` is what the user authors (or what `framesage-engine` learns over
//! time) and what the service loads on start. The engine walks `rules` looking
//! for the first match against the currently foregrounded app; if none match,
//! `default_profile` is used. Background apps that don't match any rule get
//! `background_profile`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::profile::{Profile, ProfileId};

/// What to match against to pick a profile.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppMatch {
    /// Case-insensitive exact match against the executable filename (no path).
    /// E.g. `"bf6.exe"`, `"valorant-win64-shipping.exe"`.
    ExeName(String),
    /// Case-insensitive substring match against the full image path.
    PathContains(String),
    /// Case-insensitive substring match against the window title. Useful for
    /// distinguishing modes within one binary (e.g. game vs launcher).
    WindowTitleContains(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppRule {
    pub r#match: AppMatch,
    pub profile: ProfileId,
    /// Human note shown in the UI / logs.
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    /// All profiles known to the engine, keyed by id.
    pub profiles: HashMap<ProfileId, Profile>,

    /// First-match wins.
    #[serde(default)]
    pub rules: Vec<AppRule>,

    /// Applied to the foreground app when no rule matches.
    pub default_profile: ProfileId,

    /// Applied to background processes when no rule matches them. Typically
    /// something like `eco` (Power Throttling Eco, I/O VeryLow). Optional —
    /// `None` means "leave background apps alone."
    #[serde(default)]
    pub background_profile: Option<ProfileId>,

    /// Watcher tick rate, milliseconds. 250–500 is a reasonable range; lower
    /// means snappier response to focus changes, higher means less overhead.
    #[serde(default = "Policy::default_tick_ms")]
    pub tick_ms: u64,
}

impl Policy {
    fn default_tick_ms() -> u64 {
        300
    }

    /// Find the profile that should be applied to a foregrounded app.
    pub fn match_foreground(&self, exe_name: &str, path: &str, title: &str) -> &ProfileId {
        for rule in &self.rules {
            if rule.r#match.matches(exe_name, path, title) {
                return &rule.profile;
            }
        }
        &self.default_profile
    }

    pub fn profile(&self, id: &ProfileId) -> Option<&Profile> {
        self.profiles.get(id)
    }
}

impl AppMatch {
    pub fn matches(&self, exe_name: &str, path: &str, title: &str) -> bool {
        match self {
            AppMatch::ExeName(n) => exe_name.eq_ignore_ascii_case(n),
            AppMatch::PathContains(s) => {
                path.to_ascii_lowercase().contains(&s.to_ascii_lowercase())
            }
            AppMatch::WindowTitleContains(s) => {
                title.to_ascii_lowercase().contains(&s.to_ascii_lowercase())
            }
        }
    }
}

impl Default for Policy {
    fn default() -> Self {
        use crate::profile::{IoPriority, PowerThrottlingMode, PriorityClass};
        use crate::topology::{CoreKind, CpuSelector};

        let mut profiles = HashMap::new();

        // "perf" — the default foreground profile. Tell the OS not to throttle
        // this process; let the scheduler do the rest. Conservative on purpose.
        let perf = Profile {
            id: "perf".into(),
            description: "Default foreground: do not throttle, normal priority.".to_owned(),
            power_throttling: Some(PowerThrottlingMode::Performance),
            priority_class: Some(PriorityClass::Normal),
            io_priority: Some(IoPriority::Normal),
            ..Default::default()
        };

        // "game-x3d" — the marquee profile. Pin CPU Sets to the X3D / Cache CCD.
        // No hard affinity — CPU Sets let the scheduler spill if the CCD is
        // pinned by something else, which is the desired behavior.
        let game = Profile {
            id: "game-x3d".into(),
            description: "Pin to AMD X3D CCD (or P-cores on Intel hybrid).".to_owned(),
            cpu_sets: Some(CpuSelector::Kind(CoreKind::Cache)),
            power_throttling: Some(PowerThrottlingMode::Performance),
            priority_class: Some(PriorityClass::AboveNormal),
            io_priority: Some(IoPriority::High),
            ..Default::default()
        };

        // "eco" — what we put background processes into.
        let eco = Profile {
            id: "eco".into(),
            description: "Background: efficiency cores, low I/O, low memory priority.".to_owned(),
            cpu_sets: Some(CpuSelector::Kind(CoreKind::Efficiency)),
            power_throttling: Some(PowerThrottlingMode::Eco),
            io_priority: Some(IoPriority::Low),
            memory_priority: Some(crate::profile::MemoryPriority::Low),
            ..Default::default()
        };

        profiles.insert(perf.id.clone(), perf);
        profiles.insert(game.id.clone(), game.clone());
        profiles.insert(eco.id.clone(), eco);

        // Seed a handful of well-known game executables. Users can edit freely.
        let rules = vec![
            AppRule {
                r#match: AppMatch::ExeName("bf6.exe".into()),
                profile: game.id.clone(),
                note: "Battlefield 6".into(),
            },
            AppRule {
                r#match: AppMatch::ExeName("VALORANT-Win64-Shipping.exe".into()),
                profile: game.id.clone(),
                note: "Valorant".into(),
            },
            AppRule {
                r#match: AppMatch::ExeName("FortniteClient-Win64-Shipping.exe".into()),
                profile: game.id.clone(),
                note: "Fortnite".into(),
            },
        ];

        Self {
            profiles,
            rules,
            default_profile: "perf".into(),
            background_profile: Some("eco".into()),
            tick_ms: Self::default_tick_ms(),
        }
    }
}
