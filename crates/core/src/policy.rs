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
use crate::topology::{CpuSelector, CpuTopology};

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

/// Standalone per-exe CPU-affinity rule. Lightweight alternative to creating
/// a full Profile + AppRule pair when the only thing the user wants is "pin
/// this exe to the X3D CCD every time it launches."
///
/// The engine applies a rule when:
///   * the user creates it (immediately, to live PIDs with the matching exe)
///   * a new process spawns whose exe matches (caught by the background scan)
///   * the persistent re-assert tick fires (so the kernel state stays sticky
///     against games that touch their own affinity at startup)
///
/// Matching is case-insensitive against the bare exe filename (no path), the
/// Process Lasso model. A power user who needs path discrimination can still
/// fall back to the full Profile/AppRule mechanism.
///
/// Precedence: an `AffinityRule` overrides any `Profile.cpu_sets` /
/// `affinity_mask` chosen by `AppRule` matching for the same exe. The user
/// stated affinity intent explicitly, so it wins.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AffinityRule {
    /// Case-insensitive exact match against the executable filename (no path).
    /// E.g. `"diablo iv.exe"`, `"valorant-win64-shipping.exe"`.
    pub exe_name: String,
    /// What to pin the matching process to. `CpuSelector::All` is treated as
    /// "no pin" — callers normally delete the rule instead, but storing All
    /// is harmless and lets the UI treat "reset" as just another pick.
    pub selector: CpuSelector,
    /// Free-form human note shown in the rules manager. Defaults to empty.
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

    /// Dynamic-priority contention management. When the system is under heavy
    /// CPU load AND a non-foreground process is the top consumer, temporarily
    /// lower its priority class one step so the foreground app gets the CPU.
    /// Restore after a quiet dwell window. Disabled by default — opt in via
    /// policy.
    #[serde(default)]
    pub probalance: ProBalanceConfig,

    /// Persistent per-exe CPU-affinity rules. Lighter weight than a full
    /// Profile + AppRule pair; the right call for the common "pin Diablo IV
    /// to the X3D CCD" case. See [`AffinityRule`] for matching + precedence.
    #[serde(default)]
    pub affinity_rules: Vec<AffinityRule>,
}

/// Tunables for dynamic priority management. Modeled after Process Lasso's
/// ProBalance feature — clean-room reimplementation from public docs and
/// observed behavior. Defaults to disabled until the user opts in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProBalanceConfig {
    /// Master switch. `false` (default) means the engine does nothing in this
    /// area — no sampling, no decisions, zero overhead.
    pub enabled: bool,

    /// System-wide CPU utilisation, expressed as a percentage of total
    /// across all logical processors, above which we consider the machine
    /// "under contention" and become eligible to restrain background hogs.
    /// 75 is a sensible default — below this the system has slack and there's
    /// nothing to fix.
    pub system_cpu_threshold_percent: u8,

    /// A single non-foreground process must be consuming at least this much
    /// of one logical CPU (i.e. "100" means one fully-busy thread) to be
    /// considered a hog worth restraining. Prevents twitchy restraint of
    /// processes that briefly spike.
    pub hog_cpu_threshold_percent: u16,

    /// Minimum dwell, milliseconds, that a process stays restrained before
    /// we'll even consider restoring it. Avoids ping-ponging the priority
    /// class on borderline-busy processes.
    pub min_restrain_ms: u64,

    /// Item 4.6 — restrain-side hysteresis. A candidate must read as a hog
    /// for this many *consecutive* samples before we demote it. 1 is the
    /// pre-4.6 behavior (instant demote on the first sample over
    /// threshold); 2 (the new default) requires two ticks in a row, which
    /// at the default 300 ms tick is ~600 ms of sustained pressure. This
    /// kills the false-positive demote of processes that briefly spike to
    /// 100% during a single sample window (Chrome on a tab switch, an
    /// editor running save-on-blur, etc.) without meaningfully delaying
    /// restraint of genuine hogs.
    ///
    /// Pairs with the existing `min_restrain_ms` dwell on the restore
    /// side — together they form full hysteresis: slow to demote, slow to
    /// restore. Audit M-18.
    #[serde(default = "default_min_restrain_samples")]
    pub min_restrain_samples: u32,

    /// Process names (case-insensitive, no path) that ProBalance never
    /// touches. Beyond the system-critical denylist enforced internally,
    /// this is the user's escape hatch.
    #[serde(default)]
    pub ignore_processes: Vec<String>,
}

fn default_min_restrain_samples() -> u32 {
    2
}

impl Default for ProBalanceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            system_cpu_threshold_percent: 75,
            hog_cpu_threshold_percent: 50,
            min_restrain_ms: 1500,
            min_restrain_samples: default_min_restrain_samples(),
            ignore_processes: Vec::new(),
        }
    }
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

    /// Item 4.15 — variant of `match_foreground` that also returns
    /// which rule (by index into `self.rules`) produced the match.
    /// Returns `(None, &default_profile)` when no rule matched.
    /// The index lets the activity feed link each `ForegroundChanged`
    /// event back to the specific user-authored rule that fired —
    /// the answer to "why did THAT profile apply?" without
    /// re-running the matcher by hand.
    pub fn match_foreground_indexed(
        &self,
        exe_name: &str,
        path: &str,
        title: &str,
    ) -> (Option<usize>, &ProfileId) {
        for (idx, rule) in self.rules.iter().enumerate() {
            if rule.r#match.matches(exe_name, path, title) {
                return (Some(idx), &rule.profile);
            }
        }
        (None, &self.default_profile)
    }

    pub fn profile(&self, id: &ProfileId) -> Option<&Profile> {
        self.profiles.get(id)
    }

    /// Item 4.11 — structural validation of a Policy against the current
    /// CPU topology. Returns a list of human-readable error messages
    /// (empty = valid). Service-side `SetPolicy` calls this before
    /// applying so a tray edit referencing a deleted profile or a
    /// non-existent CCD is rejected at the boundary rather than
    /// crashing the next reconcile.
    ///
    /// Checks:
    ///   * `default_profile` references a profile that exists in
    ///     `profiles`.
    ///   * `background_profile`, if set, references an existing
    ///     profile.
    ///   * Every `rule.profile` references an existing profile.
    ///   * Every `Profile.cpu_sets` / `Profile.affinity_mask` is
    ///     well-formed against the current topology (`Ccd(N)` /
    ///     `CcdNot(N)` requires N < number of distinct CCDs; `Mask(0)`
    ///     is rejected per item 4.4 — would leave the process with
    ///     no CPU).
    ///
    /// Does NOT check safe-list intersection (that's
    /// `validate_policy_against_safe_list` in the service layer) or
    /// stop-list / suspend-list content (those are user-aggressive
    /// surfaces).
    pub fn validate_structure(&self, topology: &CpuTopology) -> Vec<String> {
        let mut errors = Vec::new();

        if !self.profiles.contains_key(&self.default_profile) {
            errors.push(format!(
                "default_profile '{}' does not reference an existing profile",
                self.default_profile
            ));
        }
        if let Some(bg) = &self.background_profile {
            if !self.profiles.contains_key(bg) {
                errors.push(format!(
                    "background_profile '{bg}' does not reference an existing profile"
                ));
            }
        }

        for (idx, rule) in self.rules.iter().enumerate() {
            if !self.profiles.contains_key(&rule.profile) {
                errors.push(format!(
                    "rules[{idx}].profile '{}' does not reference an existing profile",
                    rule.profile
                ));
            }
        }

        let ccd_count = topology.ccds().count();
        let validate_selector =
            |sel: &CpuSelector, field_path: &str, errors: &mut Vec<String>| match sel {
                CpuSelector::Mask(0) => errors.push(format!(
                    "{field_path}: Mask(0) would leave the process with no CPU; refusing",
                )),
                CpuSelector::Ccd(n) if ccd_count > 0 && (*n as usize) >= ccd_count => {
                    errors.push(format!(
                        "{field_path}: Ccd({n}) but this topology only has {ccd_count} CCDs"
                    ));
                }
                CpuSelector::CcdNot(n) if ccd_count > 0 && (*n as usize) >= ccd_count => {
                    errors.push(format!(
                        "{field_path}: CcdNot({n}) but this topology only has {ccd_count} CCDs"
                    ));
                }
                _ => {}
            };

        for (id, profile) in &self.profiles {
            if let Some(sel) = &profile.cpu_sets {
                validate_selector(sel, &format!("profiles[{id}].cpu_sets"), &mut errors);
            }
            if let Some(sel) = &profile.affinity_mask {
                validate_selector(sel, &format!("profiles[{id}].affinity_mask"), &mut errors);
            }
        }

        errors
    }

    /// Find an affinity rule for the given exe name (case-insensitive).
    pub fn affinity_rule_for(&self, exe_name: &str) -> Option<&AffinityRule> {
        self.affinity_rules
            .iter()
            .find(|r| r.exe_name.eq_ignore_ascii_case(exe_name))
    }

    /// Insert or replace an affinity rule for the rule's exe name. Returns
    /// `true` if an existing rule was replaced, `false` if a new one was
    /// appended. Case-insensitive match for the existing-rule lookup.
    pub fn upsert_affinity_rule(&mut self, rule: AffinityRule) -> bool {
        if let Some(slot) = self
            .affinity_rules
            .iter_mut()
            .find(|r| r.exe_name.eq_ignore_ascii_case(&rule.exe_name))
        {
            *slot = rule;
            true
        } else {
            self.affinity_rules.push(rule);
            false
        }
    }

    /// Remove the affinity rule for the given exe name (case-insensitive).
    /// Returns `true` if a rule was removed, `false` if no match existed.
    pub fn remove_affinity_rule(&mut self, exe_name: &str) -> bool {
        let before = self.affinity_rules.len();
        self.affinity_rules
            .retain(|r| !r.exe_name.eq_ignore_ascii_case(exe_name));
        self.affinity_rules.len() != before
    }

    /// Read a policy from disk. Errors if the file doesn't exist or fails to
    /// parse — for "load if exists, otherwise default" semantics use
    /// `load_or_create_default`.
    ///
    /// Tolerates a leading UTF-8 BOM (`EF BB BF`). PowerShell 5.1's
    /// `Set-Content -Encoding UTF8` always emits one, so any admin editing
    /// `policy.json` from the shipped Windows shell will produce a BOMed
    /// file. `serde_json` rejects BOMed input by spec, so we strip it here.
    pub fn load(path: &Path) -> Result<Self, PolicyError> {
        let bytes = std::fs::read(path).map_err(|e| PolicyError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
        serde_json::from_slice(body).map_err(|e| PolicyError::Parse {
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
            // The pin sticks for the lifetime of the game process. Alt-tabbing
            // to a browser or task manager must NOT relinquish the X3D CCD.
            persistent: true,
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
                    "UsoSvc".into(),
                    // Notifications + device-platform polling.
                    "WpnService".into(),
                    "CDPSvc".into(),
                    // Diagnostics infrastructure (DPS family) + crash reports.
                    "DPS".into(),
                    "WdiServiceHost".into(),
                    "WdiSystemHost".into(),
                    "WerSvc".into(),
                    "PcaSvc".into(),
                    "dmwappushservice".into(),
                    // Microsoft-app updaters that wake on their own schedule.
                    "ClickToRunSvc".into(),
                    // Background backup + maintenance.
                    "SDRSVC".into(),
                    "defragsvc".into(),
                    // Vestigial / IoT services that are pure background.
                    "MapsBroker".into(),
                    "AJRouter".into(),
                    "WMPNetworkSvc".into(),
                    "Fax".into(),
                    "RetailDemo".into(),
                    "PhoneSvc".into(),
                    "RemoteRegistry".into(),
                    "icssvc".into(),
                    "TrkWks".into(),
                    "stisvc".into(),
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
                    "MicrosoftEdgeUpdate.exe".into(),
                    "lghub_updater.exe".into(),
                    "AdobeARM.exe".into(),
                    // Game Bar + Xbox overlay. Many gamers disable Game Bar
                    // entirely; suspending here drops its CPU/frame-pacing
                    // overhead for the session without flipping the registry
                    // toggle permanently.
                    "GameBar.exe".into(),
                    "GameBarFTServer.exe".into(),
                    "GameBarPresenceWriter.exe".into(),
                    // Windows Widgets — pure background poller for news /
                    // weather / stocks.
                    "WidgetService.exe".into(),
                    "Widgets.exe".into(),
                    // Phone Link bridge — periodic Bluetooth/Wi-Fi Direct
                    // chatter. Pair with CDPSvc stop.
                    "YourPhone.exe".into(),
                    "PhoneExperienceHost.exe".into(),
                    // GeForce Experience background helper (driver itself is
                    // safe-listed and untouched).
                    "NVIDIA Web Helper.exe".into(),
                    // OEM preinstalled telemetry / SupportAssist suites.
                    "DellSupportAssistRemedyService.exe".into(),
                    "HPSupportSolutionsFrameworkService.exe".into(),
                    "HpToastSourceApp.exe".into(),
                    "LenovoVantageService.exe".into(),
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

        // Item 1.9 / AC matrix — clone game-x3d into two AC-tier variants
        // for the seeded BF6 + Valorant rules. Defaults D-9 (Valorant →
        // SafeMode) and D-10 (BF6 → Hybrid).
        //
        // We materialise these as distinct profiles (not per-rule
        // overrides) so the user can see + edit them in the Profiles
        // tab. The shared content (services / processes / power plan)
        // is inherited via clone — keep one source of truth for the
        // sledgehammer's contents.
        let mut game_hybrid = game.clone();
        game_hybrid.id = "game-x3d-hybrid".into();
        game_hybrid.description =
            "BF6 / EA Javelin: environment optimizations only — never touches the game process. \
             Javelin actively blocks affinity changes on dual-CCD Ryzen during multiplayer. \
             See audit/research/anti-cheat-eac.md."
                .to_owned();
        game_hybrid.ac_safe_mode_target = crate::AntiCheatProfile::Hybrid;

        let mut game_safe = game.clone();
        game_safe.id = "game-x3d-safe".into();
        game_safe.description =
            "Vanguard (Valorant) / FACEIT / ESEA: AC-Safe Mode — environment optimizations only, \
             game process NEVER touched. Vanguard reserves hardware bans; Process Lasso has \
             produced VAN: Competitive Restrictions. Mirrors Hone's 1M+-user model. See \
             audit/research/anti-cheat-vanguard.md."
                .to_owned();
        game_safe.ac_safe_mode_target = crate::AntiCheatProfile::SafeMode;

        profiles.insert(perf.id.clone(), perf);
        profiles.insert(game.id.clone(), game.clone());
        profiles.insert(game_hybrid.id.clone(), game_hybrid.clone());
        profiles.insert(game_safe.id.clone(), game_safe.clone());
        profiles.insert(eco.id.clone(), eco);

        // Seed a handful of well-known game executables. Per defaults
        // D-9/D-10 + AC matrix research:
        //   * Valorant → game-x3d-safe (Vanguard hardware-ban risk)
        //   * BF6 → game-x3d-hybrid (Javelin affinity-blocking risk)
        //   * Fortnite → game-x3d (EAC, friendly case, full aggression)
        // Users can re-point any rule to a different AC tier per profile
        // editor; these are just the recommended defaults.
        let rules = vec![
            AppRule {
                r#match: AppMatch::ExeName("bf6.exe".into()),
                profile: game_hybrid.id.clone(),
                note: "Battlefield 6 (EA Javelin Hybrid mode)".into(),
            },
            AppRule {
                r#match: AppMatch::ExeName("VALORANT-Win64-Shipping.exe".into()),
                profile: game_safe.id.clone(),
                note: "Valorant (Vanguard Safe Mode)".into(),
            },
            AppRule {
                r#match: AppMatch::ExeName("FortniteClient-Win64-Shipping.exe".into()),
                profile: game.id.clone(),
                note: "Fortnite (EAC Aggressive)".into(),
            },
        ];

        Self {
            profiles,
            rules,
            default_profile: "perf".into(),
            background_profile: Some("eco".into()),
            tick_ms: Self::default_tick_ms(),
            probalance: ProBalanceConfig::default(),
            affinity_rules: Vec::new(),
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
            probalance: ProBalanceConfig::default(),
            affinity_rules: Vec::new(),
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

    #[test]
    fn affinity_rule_upsert_inserts_new_and_replaces_existing() {
        use crate::topology::{CoreKind, CpuSelector};

        let mut p = sample_policy();
        assert!(p.affinity_rules.is_empty());

        let inserted = p.upsert_affinity_rule(AffinityRule {
            exe_name: "diablo iv.exe".into(),
            selector: CpuSelector::Kind(CoreKind::Cache),
            note: String::new(),
        });
        assert!(!inserted, "first call should append, not replace");
        assert_eq!(p.affinity_rules.len(), 1);

        // Different case for the same exe should replace, not append.
        let replaced = p.upsert_affinity_rule(AffinityRule {
            exe_name: "DIABLO IV.EXE".into(),
            selector: CpuSelector::All,
            note: "updated".into(),
        });
        assert!(replaced);
        assert_eq!(p.affinity_rules.len(), 1);
        assert_eq!(p.affinity_rules[0].selector, CpuSelector::All);
        assert_eq!(p.affinity_rules[0].note, "updated");
    }

    #[test]
    fn affinity_rule_lookup_is_case_insensitive() {
        use crate::topology::{CoreKind, CpuSelector};

        let mut p = sample_policy();
        p.upsert_affinity_rule(AffinityRule {
            exe_name: "Diablo IV.exe".into(),
            selector: CpuSelector::Kind(CoreKind::Cache),
            note: String::new(),
        });

        assert!(p.affinity_rule_for("diablo iv.exe").is_some());
        assert!(p.affinity_rule_for("DIABLO IV.EXE").is_some());
        assert!(p.affinity_rule_for("notepad.exe").is_none());
    }

    #[test]
    fn affinity_rule_remove_returns_true_only_when_match_existed() {
        use crate::topology::{CoreKind, CpuSelector};

        let mut p = sample_policy();
        p.upsert_affinity_rule(AffinityRule {
            exe_name: "diablo iv.exe".into(),
            selector: CpuSelector::Kind(CoreKind::Cache),
            note: String::new(),
        });

        assert!(p.remove_affinity_rule("DIABLO IV.EXE"));
        assert!(p.affinity_rules.is_empty());
        assert!(!p.remove_affinity_rule("diablo iv.exe"));
    }

    // ─── Item 4.11 — structural validation ──────────────────────────────

    fn two_ccd_topology() -> CpuTopology {
        use crate::topology::{CoreKind, LogicalCpu};
        let mut cpus = Vec::new();
        for ccd in 0u8..2 {
            for core in 0..4u32 {
                cpus.push(LogicalCpu {
                    index: (ccd as u32) * 4 + core,
                    physical_core: (ccd as u32) * 4 + core,
                    ccd,
                    kind: CoreKind::Performance,
                    cppc_rank: None,
                    l3_cache_bytes: None,
                    is_smt_sibling: false,
                });
            }
        }
        CpuTopology { cpus }
    }

    #[test]
    fn validate_structure_accepts_default_policy() {
        let p = Policy::default();
        let topo = two_ccd_topology();
        assert!(
            p.validate_structure(&topo).is_empty(),
            "Policy::default must validate cleanly"
        );
    }

    #[test]
    fn validate_structure_rejects_dangling_rule_profile_ref() {
        let mut p = sample_policy();
        p.rules.push(AppRule {
            r#match: AppMatch::ExeName("ghost.exe".into()),
            profile: ProfileId("does-not-exist".into()),
            note: String::new(),
        });
        let errs = p.validate_structure(&two_ccd_topology());
        assert!(
            errs.iter().any(|e| e.contains("does-not-exist")),
            "expected error mentioning the dangling profile id, got: {errs:?}"
        );
    }

    #[test]
    fn validate_structure_rejects_dangling_default_profile() {
        let mut p = sample_policy();
        p.default_profile = ProfileId("missing".into());
        let errs = p.validate_structure(&two_ccd_topology());
        assert!(
            errs.iter()
                .any(|e| e.contains("default_profile") && e.contains("missing")),
            "expected error mentioning default_profile, got: {errs:?}"
        );
    }

    #[test]
    fn validate_structure_rejects_out_of_range_ccd_selector() {
        let mut p = sample_policy();
        // Insert a profile with Ccd(7) — only 2 CCDs in the topology.
        let mut bad = Profile::new("bad-ccd");
        bad.cpu_sets = Some(CpuSelector::Ccd(7));
        p.profiles.insert(bad.id.clone(), bad);
        let errs = p.validate_structure(&two_ccd_topology());
        assert!(
            errs.iter()
                .any(|e| e.contains("Ccd(7)") && e.contains("2 CCDs")),
            "expected out-of-range Ccd error, got: {errs:?}"
        );
    }

    #[test]
    fn validate_structure_rejects_mask_zero() {
        let mut p = sample_policy();
        let mut bad = Profile::new("bad-mask");
        bad.affinity_mask = Some(CpuSelector::Mask(0));
        p.profiles.insert(bad.id.clone(), bad);
        let errs = p.validate_structure(&two_ccd_topology());
        assert!(
            errs.iter()
                .any(|e| e.contains("Mask(0)") && e.contains("refusing")),
            "expected Mask(0) refusal, got: {errs:?}"
        );
    }

    #[test]
    fn validate_structure_accumulates_multiple_errors() {
        // A pathological policy with several distinct problems; we
        // expect every error surfaced, not just the first.
        let mut p = sample_policy();
        p.default_profile = ProfileId("missing-1".into());
        p.background_profile = Some(ProfileId("missing-2".into()));
        p.rules.push(AppRule {
            r#match: AppMatch::ExeName("g.exe".into()),
            profile: ProfileId("missing-3".into()),
            note: String::new(),
        });
        let mut bad = Profile::new("bad");
        bad.affinity_mask = Some(CpuSelector::Mask(0));
        p.profiles.insert(bad.id.clone(), bad);

        let errs = p.validate_structure(&two_ccd_topology());
        // 4 distinct errors expected.
        assert!(
            errs.len() >= 4,
            "expected at least 4 errors, got {}: {errs:?}",
            errs.len()
        );
    }

    #[test]
    fn load_strips_utf8_bom() {
        // PowerShell 5.1's `Set-Content -Encoding UTF8` prepends EF BB BF.
        // Without BOM tolerance, a hand-edited policy.json silently fails
        // to parse and the service falls back to defaults — exactly the
        // failure mode users hit in practice.
        let dir = std::env::temp_dir().join(format!("framesage-test-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("policy.json");
        let body = serde_json::to_vec_pretty(&Policy::default()).expect("serialize");
        let mut bomed = vec![0xEF, 0xBB, 0xBF];
        bomed.extend_from_slice(&body);
        std::fs::write(&path, &bomed).expect("write");

        let loaded = Policy::load(&path).expect("BOMed file must load");
        assert_eq!(loaded.rules.len(), Policy::default().rules.len());

        std::fs::remove_dir_all(&dir).ok();
    }
}
