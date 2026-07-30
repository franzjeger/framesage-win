//! Where framesage stores persistent state on disk.
//!
//! On Windows the natural home is `%ProgramData%\framesage\` (machine-wide,
//! readable by all users, writable by Administrators/LocalSystem). Falling
//! back to the user's `%LOCALAPPDATA%` if `PROGRAMDATA` isn't set keeps dev
//! and console-mode runs sensible.
//!
//! On non-Windows hosts we use `$XDG_CONFIG_HOME/framesage/` (or
//! `~/.config/framesage/`), which is what `framesage-sim` will use when the
//! engine is exercised on a developer's macOS or Linux box.

use std::path::PathBuf;

const APP_DIR_NAME: &str = "framesage";
const POLICY_FILE_NAME: &str = "policy.json";
const ACTIVITY_LOG_FILE_NAME: &str = "activity.jsonl";

/// Directory holding all framesage state.
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(p) = std::env::var_os("PROGRAMDATA") {
            return PathBuf::from(p).join(APP_DIR_NAME);
        }
        if let Some(p) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(p).join(APP_DIR_NAME);
        }
        // Last resort — same directory as the running exe.
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                return parent.to_path_buf();
            }
        }
        PathBuf::from(format!(r"C:\ProgramData\{APP_DIR_NAME}"))
    }

    #[cfg(not(windows))]
    {
        if let Some(p) = std::env::var_os("XDG_CONFIG_HOME") {
            return PathBuf::from(p).join(APP_DIR_NAME);
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(".config").join(APP_DIR_NAME);
        }
        PathBuf::from(format!("./.{APP_DIR_NAME}"))
    }
}

/// The canonical `policy.json` location.
pub fn policy_path() -> PathBuf {
    config_dir().join(POLICY_FILE_NAME)
}

/// v0.7.1 Group C (#110, architecture §2.3) — on-disk session
/// recordings: `<config_dir>/sessions/<session-id>.jsonl`. Inherits
/// the hardened config-dir DACL (Administrators + LocalSystem write;
/// users read-only) because it's on-disk personal data.
pub fn sessions_dir() -> PathBuf {
    config_dir().join("sessions")
}

/// Item 2.9 / audit M-03. Per-user data directory — distinct from
/// [`config_dir`] because the service hardens the latter's DACL to
/// LocalSystem + Administrators (so an unprivileged tray running in
/// the user's session can't write there). Tray-owned data
/// (activity.jsonl, future per-user UI state) lives here under
/// `%LOCALAPPDATA%\framesage\`.
///
/// On non-Windows we follow XDG: `$XDG_DATA_HOME/framesage/` or
/// `~/.local/share/framesage/`.
pub fn user_data_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(p) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(p).join(APP_DIR_NAME);
        }
        // Fallback to config_dir if LOCALAPPDATA isn't set — better
        // to share with the engine than crash.
        config_dir()
    }
    #[cfg(not(windows))]
    {
        if let Some(p) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(p).join(APP_DIR_NAME);
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join(APP_DIR_NAME);
        }
        PathBuf::from(format!("./.{APP_DIR_NAME}-data"))
    }
}

/// Item 2.9 / audit M-03. The tray's activity event log
/// (`activity.jsonl`). Append-only line-delimited JSON, one event per
/// line. Lives in [`user_data_dir`] so an unprivileged tray can write
/// to it without elevation.
pub fn activity_log_path() -> PathBuf {
    user_data_dir().join(ACTIVITY_LOG_FILE_NAME)
}

/// Item 4.1 — marker file written when the user completes the
/// first-run onboarding wizard. Presence of the file gates the
/// modal: if it exists, skip onboarding. If it doesn't exist (fresh
/// install or user reset their per-user state), show the wizard.
///
/// File contents are unused — the existence-check is all that
/// matters. Lives in [`user_data_dir`] so it's per-user and an
/// unprivileged tray can write it without elevation.
pub fn first_run_marker_path() -> PathBuf {
    user_data_dir().join("first-run-complete")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_path_lives_under_config_dir() {
        let dir = config_dir();
        let file = policy_path();
        assert!(
            file.starts_with(&dir),
            "policy {file:?} is not under {dir:?}"
        );
        assert_eq!(file.file_name().unwrap(), "policy.json");
    }

    #[test]
    fn config_dir_ends_with_framesage() {
        let dir = config_dir();
        assert_eq!(dir.file_name().unwrap(), "framesage");
    }

    #[test]
    fn activity_log_lives_under_user_data_dir() {
        let dir = user_data_dir();
        let file = activity_log_path();
        assert!(
            file.starts_with(&dir),
            "activity log {file:?} is not under {dir:?}"
        );
        assert_eq!(file.file_name().unwrap(), "activity.jsonl");
    }
}
