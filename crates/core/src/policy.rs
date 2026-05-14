//! Policy: the user's configured ruleset and default profile.
//!
//! A `Policy` is what the user authors (or what `framesage-engine` learns over
//! time) and what the service loads on start. The engine walks `rules` looking
//! for the first match against the currently foregrounded app; if none match,
//! `default_profile` is used. Background apps that don't match any rule get
//! `background_profile`.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::profile::{Profile, ProfileId};

#[derive(Debug, Error)]
pub enum PolicyError {
    #[error("io error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("parse error in {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

/// What to match against to pick a profile.
///
/// JSON wire format uses adjacent tagging (`{"type": "...", "value": ...}`),
/// matching `CpuSelector`. See its docs for why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
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

    /// Read a policy from disk. Errors if the file doesn't exist or fails to
    /// parse — for "load if exists, otherwise default" semantics use
    /// `load_or_create_default`.
    pub fn load(path: &Path) -> Result<Self, PolicyError> {
        let bytes = std::fs::read(path).map_err(|e| PolicyError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        serde_json::from_slice(&bytes).map_err(|e| PolicyError::Parse {
            path: path.display().to_string(),
            source: e,
        })
    }

    /// Load from disk if present; otherwise materialise `Policy::default()` to
    /// disk and return it. The parent directory is created as needed.
    ///
    /// This is the bootstrap path the service takes on startup, and it's also
    /// what gives the user a real `policy.json` file to discover and edit.
    pub fn load_or_create_default(path: &Path) -> Result<Self, PolicyError> {
        match Self::load(path) {
            Ok(p) => Ok(p),
            Err(PolicyError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                let default = Self::default();
                default.save(path)?;
                Ok(default)
            }
            Err(e) => Err(e),
        }
    }

    /// Write the policy to disk as pretty-printed JSON.
    ///
    /// Uses a temp-file + rename so the swap is atomic; readers (e.g. a hot
    /// reloader watching for inotify/USN events) never see a half-written
    /// policy.
    pub fn save(&self, path: &Path) -> Result<(), PolicyError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| PolicyError::Io {
                path: parent.display().to_string(),
                source: e,
            })?;
        }
        let body = serde_json::to_vec_pretty(self).map_err(|e| PolicyError::Parse {
            path: path.display().to_string(),
            source: e,
        })?;
        // Temp file alongside the target, then rename. On Windows the rename
        // is atomic when the target is on the same volume — which `tempfile`
        // suffix guarantees.
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp_path = std::path::PathBuf::from(tmp);
        std::fs::write(&tmp_path, &body).map_err(|e| PolicyError::Io {
            path: tmp_path.display().to_string(),
            source: e,
        })?;
        std::fs::rename(&tmp_path, path).map_err(|e| PolicyError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        Ok(())
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
        use crate::game_mode::{GameModeActions, PowerPlanId};
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
        //
        // Game Mode here is aggressive on purpose. Every entry is gated by the
        // engine's curated safe-list (denylist blocks AV / anti-cheat /
        // kernel / shell / audio / GPU drivers), and the journal makes the
        // whole batch crash-safe-revertible. A user who wants less should
        // edit the policy; the default leans into "give the game everything
        // we can give it without breaking the system."
        let game = Profile {
            id: "game-x3d".into(),
            description: "Pin to AMD X3D CCD (or P-cores on Intel hybrid).".to_owned(),
            cpu_sets: Some(CpuSelector::Kind(CoreKind::Cache)),
            power_throttling: Some(PowerThrottlingMode::Performance),
            priority_class: Some(PriorityClass::AboveNormal),
            io_priority: Some(IoPriority::High),
            game_mode: Some(GameModeActions {
                hide_taskbar: true,
                stop_services: vec![
                    // Search / prefetch / telemetry — the canonical three.
                    "SysMain".into(),
                    "WSearch".into(),
                    "DiagTrack".into(),
                    // Update / background-download bandwidth and CPU.
                    "BITS".into(),
                    "DoSvc".into(),
                    "WaaSMedicSvc".into(),
                    // Notifications + device-platform polling.
                    "WpnService".into(),
                    "CDPSvc".into(),
                    // Diagnostics infrastructure (DPS family).
                    "DPS".into(),
                    "WdiServiceHost".into(),
                    "WdiSystemHost".into(),
                    // Vestigial / IoT services that are pure background.
                    "MapsBroker".into(),
                    "AJRouter".into(),
                    "WMPNetworkSvc".into(),
                    "defragsvc".into(),
                    "Fax".into(),
                    "RetailDemo".into(),
                    "PhoneSvc".into(),
                    "RemoteRegistry".into(),
                    "icssvc".into(),
                ],
                suspend_processes: vec![
                    // Cloud-storage syncs — heaviest disk + network bursts.
                    "OneDrive.exe".into(),
                    "FileCoAuth.exe".into(),
                    "Dropbox.exe".into(),
                    "googledrivesync.exe".into(),
                    "GoogleDriveFS.exe".into(),
                    "pCloud.exe".into(),
                    "MEGAsync.exe".into(),
                    // Auto-updaters polling in the background.
                    "OneDriveStandaloneUpdater.exe".into(),
                    "GoogleUpdate.exe".into(),
                    "lghub_updater.exe".into(),
                ],
                power_plan: Some(PowerPlanId::HighPerformance),
                focus_assist: None,
                pause_windows_update: true,
            }),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_policy() -> Policy {
        let mut profiles = HashMap::new();
        profiles.insert(ProfileId("perf".into()), Profile::new("perf"));
        profiles.insert(ProfileId("game-x3d".into()), Profile::new("game-x3d"));
        profiles.insert(ProfileId("dev".into()), Profile::new("dev"));

        let rules = vec![
            AppRule {
                r#match: AppMatch::ExeName("bf6.exe".into()),
                profile: ProfileId("game-x3d".into()),
                note: String::new(),
            },
            AppRule {
                r#match: AppMatch::PathContains("visual studio".into()),
                profile: ProfileId("dev".into()),
                note: String::new(),
            },
            AppRule {
                r#match: AppMatch::WindowTitleContains("DEBUG".into()),
                profile: ProfileId("dev".into()),
                note: String::new(),
            },
        ];

        Policy {
            profiles,
            rules,
            default_profile: ProfileId("perf".into()),
            background_profile: None,
            tick_ms: 250,
        }
    }

    #[test]
    fn exact_exe_match_wins() {
        let p = sample_policy();
        let got = p.match_foreground("bf6.exe", r"C:\Games\bf6\bf6.exe", "Battlefield 6");
        assert_eq!(got.0, "game-x3d");
    }

    #[test]
    fn exe_match_is_case_insensitive() {
        let p = sample_policy();
        let got = p.match_foreground("BF6.EXE", r"C:\Games\bf6\bf6.exe", "");
        assert_eq!(got.0, "game-x3d");
    }

    #[test]
    fn path_contains_match_is_case_insensitive() {
        let p = sample_policy();
        let got = p.match_foreground(
            "devenv.exe",
            r"C:\Program Files\Microsoft Visual Studio\2026\Community\Common7\IDE\devenv.exe",
            "Solution.sln",
        );
        assert_eq!(got.0, "dev");
    }

    #[test]
    fn title_contains_match_works() {
        let p = sample_policy();
        let got = p.match_foreground("anything.exe", r"C:\anything", "MyApp (DEBUG)");
        assert_eq!(got.0, "dev");
    }

    #[test]
    fn unmatched_foreground_falls_back_to_default() {
        let p = sample_policy();
        let got = p.match_foreground("notepad.exe", r"C:\Windows\notepad.exe", "Untitled");
        assert_eq!(got.0, "perf");
    }

    #[test]
    fn first_match_wins_over_later_rules() {
        // ExeName rule for bf6.exe appears before the title-contains rule for
        // DEBUG; bf6 with a DEBUG title should still pick game-x3d, not dev.
        let p = sample_policy();
        let got = p.match_foreground("bf6.exe", r"C:\Games\bf6\bf6.exe", "BF6 DEBUG");
        assert_eq!(got.0, "game-x3d");
    }

    #[test]
    fn policy_round_trips_through_json() {
        let original = Policy::default();
        let json = serde_json::to_string_pretty(&original).expect("serialize");
        let parsed: Policy = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(parsed.default_profile, original.default_profile);
        assert_eq!(parsed.background_profile, original.background_profile);
        assert_eq!(parsed.tick_ms, original.tick_ms);
        assert_eq!(parsed.rules.len(), original.rules.len());
        assert_eq!(parsed.profiles.len(), original.profiles.len());
    }

    #[test]
    fn unknown_profile_id_returns_none() {
        let p = sample_policy();
        assert!(p.profile(&ProfileId("nonexistent".into())).is_none());
        assert!(p.profile(&ProfileId("perf".into())).is_some());
    }

    #[test]
    fn save_then_load_round_trips_default_policy() {
        let dir = std::env::temp_dir().join(format!("framesage-test-{}", std::process::id()));
        let path = dir.join("policy.json");
        let original = Policy::default();

        original.save(&path).expect("save");
        let loaded = Policy::load(&path).expect("load");

        assert_eq!(loaded.default_profile, original.default_profile);
        assert_eq!(loaded.rules.len(), original.rules.len());
        assert_eq!(loaded.profiles.len(), original.profiles.len());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_or_create_default_creates_missing_file() {
        let dir =
            std::env::temp_dir().join(format!("framesage-test-bootstrap-{}", std::process::id()));
        let path = dir.join("policy.json");
        assert!(!path.exists());

        let policy = Policy::load_or_create_default(&path).expect("bootstrap");
        assert!(path.exists(), "bootstrap should create the file");
        assert_eq!(policy.default_profile, Policy::default().default_profile);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_returns_not_found_for_missing_file() {
        let dir =
            std::env::temp_dir().join(format!("framesage-test-missing-{}", std::process::id()));
        let path = dir.join("does-not-exist.json");
        match Policy::load(&path) {
            Err(PolicyError::Io { source, .. }) => {
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }
}
