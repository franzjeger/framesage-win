//! #2 — per-user tray UI preferences (`%LOCALAPPDATA%\framesage\
//! tray-prefs.json`).
//!
//! Deliberately NOT part of policy.json: these are per-user UI
//! choices, they don't belong in the engine's policy, and they must
//! not be shared with the LocalSystem service. Load at tray startup,
//! save on every change. Unknown/missing fields fall back to defaults
//! so a schema bump is backwards-compatible.

use serde::{Deserialize, Serialize};

/// Which optional Processes-tab columns are visible. The required
/// columns (marker, icon, Process, PID, CPU, Memory, Profile, Status)
/// are always shown and not represented here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessesColumns {
    pub description: bool,
    pub company: bool,
    pub user: bool,
    pub threads: bool,
    pub priority: bool,
    pub affinity: bool,
}

impl Default for ProcessesColumns {
    fn default() -> Self {
        // All optional columns on by default — the current behavior,
        // so an upgrade changes nothing until the user hides one.
        Self {
            description: true,
            company: true,
            user: true,
            threads: true,
            priority: true,
            affinity: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrayPrefs {
    pub schema_version: u32,
    #[serde(default)]
    pub processes_columns: ProcessesColumns,
}

impl Default for TrayPrefs {
    fn default() -> Self {
        Self {
            schema_version: 1,
            processes_columns: ProcessesColumns::default(),
        }
    }
}

impl TrayPrefs {
    fn path() -> std::path::PathBuf {
        framesage_core::paths::user_data_dir().join("tray-prefs.json")
    }

    /// Load from disk, falling back to defaults on any error (missing
    /// file, parse failure) — UI prefs must never block startup.
    pub fn load() -> Self {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(body) => serde_json::from_str(&body).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Best-effort save; errors are logged, not surfaced (a failed
    /// pref write shouldn't interrupt the user).
    pub fn save(&self) {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(body) => {
                if let Err(e) = std::fs::write(&path, body) {
                    tracing::warn!(error = %e, "failed to save tray prefs");
                }
            }
            Err(e) => tracing::warn!(error = %e, "failed to serialize tray prefs"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_show_every_optional_column() {
        let c = ProcessesColumns::default();
        assert!(c.description && c.company && c.user && c.threads && c.priority && c.affinity);
    }

    #[test]
    fn prefs_round_trip_through_json_and_tolerate_missing_fields() {
        let prefs = TrayPrefs {
            schema_version: 1,
            processes_columns: ProcessesColumns {
                company: false,
                affinity: false,
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&prefs).unwrap();
        let back: TrayPrefs = serde_json::from_str(&json).unwrap();
        assert_eq!(prefs, back);

        // Missing processes_columns → defaults (backwards-compatible).
        let minimal: TrayPrefs = serde_json::from_str(r#"{"schema_version":1}"#).unwrap();
        assert_eq!(minimal.processes_columns, ProcessesColumns::default());
    }
}
