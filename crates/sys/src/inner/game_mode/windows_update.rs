//! Pause and resume Windows Update via the documented `WindowsUpdate\UX\Settings`
//! registry keys.
//!
//! This is the same mechanism the Settings app uses when the user clicks
//! "Pause updates for 1 week" — Windows Update Agent reads
//! `PauseUpdatesStartTime` / `PauseUpdatesExpiryTime` (REG_SZ, ISO-8601 UTC)
//! and the Quality/Feature counterparts on Win11 to decide whether to scan
//! for updates. Setting these keys is documented behavior, runs entirely in
//! user-mode, and is anti-cheat-clean by construction.
//!
//! We pause for one hour by default. On revert we delete every key we set,
//! which the agent treats as "no pause active." If anyone (the user, the
//! Settings app, group policy) had set a longer pause before Game Mode
//! engaged, we'd overwrite it — that's a known limitation; capturing the
//! prior values for restore is a v0.3 polish.
//!
//! Requires HKLM write access. framesage-svc runs as LocalSystem, which
//! has it. A non-elevated console run will silently fail (warn-logged).

use anyhow::{anyhow, Context, Result};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE,
    KEY_SET_VALUE, KEY_WOW64_64KEY, REG_CREATE_KEY_DISPOSITION, REG_OPTION_NON_VOLATILE, REG_SZ,
};

/// Windows Update Settings hive. The same path the Settings app writes to.
const WU_SETTINGS_KEY: PCWSTR = w!("SOFTWARE\\Microsoft\\WindowsUpdate\\UX\\Settings");

/// Every pause-related value we manage. Setting all four is what the Settings
/// app does on Win11 for "Pause updates"; Win10 ignores the feature/quality
/// pair gracefully.
const PAUSE_VALUES: &[(&str, ValueKind)] = &[
    ("PauseUpdatesStartTime", ValueKind::StartTime),
    ("PauseUpdatesExpiryTime", ValueKind::ExpiryTime),
    ("PauseFeatureUpdatesStartTime", ValueKind::StartTime),
    ("PauseFeatureUpdatesEndTime", ValueKind::ExpiryTime),
    ("PauseQualityUpdatesStartTime", ValueKind::StartTime),
    ("PauseQualityUpdatesEndTime", ValueKind::ExpiryTime),
];

#[derive(Clone, Copy)]
enum ValueKind {
    StartTime,
    ExpiryTime,
}

/// Default pause window. One hour is comfortably longer than any single game
/// session and short enough that a crashed framesage doesn't leave updates
/// stranded for days.
pub const DEFAULT_PAUSE: Duration = Duration::from_secs(60 * 60);

/// Pause Windows Update for `duration`, starting now.
///
/// Returns Ok(()) if the pause keys were written. Errors only on unexpected
/// registry failures (a non-elevated caller will get a permission error and
/// the engine logs+continues — that's the documented contract for game-mode
/// actions).
pub fn pause(duration: Duration) -> Result<()> {
    let now = SystemTime::now();
    let end = now + duration;
    let start_iso = iso8601_utc(now)?;
    let end_iso = iso8601_utc(end)?;

    with_settings_key(|key| {
        for (name, kind) in PAUSE_VALUES {
            let value = match kind {
                ValueKind::StartTime => start_iso.as_str(),
                ValueKind::ExpiryTime => end_iso.as_str(),
            };
            set_string_value(key, name, value)?;
        }
        Ok(())
    })
}

/// Resume Windows Update by deleting every pause key we set. Missing values
/// are treated as success (Windows Update interprets missing keys as "no
/// pause active").
pub fn resume() -> Result<()> {
    with_settings_key(|key| {
        for (name, _) in PAUSE_VALUES {
            // Best-effort delete; ERROR_FILE_NOT_FOUND is fine.
            let wide = wide(name);
            // SAFETY: key is valid; wide is a valid UTF-16 string.
            let _ = unsafe { RegDeleteValueW(key, PCWSTR(wide.as_ptr())) };
        }
        Ok(())
    })
}

fn with_settings_key<F>(f: F) -> Result<()>
where
    F: FnOnce(HKEY) -> Result<()>,
{
    let mut key = HKEY::default();
    let mut disposition = REG_CREATE_KEY_DISPOSITION(0);
    // SAFETY: all out-pointers are valid; the key path is a static wide
    // string. RegCreateKeyEx opens or creates; the key already exists on
    // every modern Windows install.
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            WU_SETTINGS_KEY,
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_WOW64_64KEY,
            None,
            &mut key,
            Some(&mut disposition),
        )
    };
    if status.is_err() {
        return Err(anyhow!(
            "open HKLM\\{} failed: {:?}",
            wide_lossy(WU_SETTINGS_KEY),
            status
        ));
    }

    let result = f(key);

    // SAFETY: key is the handle we just opened.
    let _ = unsafe { RegCloseKey(key) };
    result
}

fn set_string_value(key: HKEY, name: &str, value: &str) -> Result<()> {
    let wide_name = wide(name);
    let wide_value = wide(value);
    let bytes: &[u8] = wide_slice_as_bytes(&wide_value);
    // SAFETY: key valid; both wide buffers are valid UTF-16 with NUL terminator.
    let status = unsafe { RegSetValueExW(key, PCWSTR(wide_name.as_ptr()), 0, REG_SZ, Some(bytes)) };
    if status.is_err() {
        return Err(anyhow!("RegSetValueExW({name}) failed: {:?}", status));
    }
    Ok(())
}

fn iso8601_utc(t: SystemTime) -> Result<String> {
    // The WU agent accepts the same format the Settings app writes — an
    // ISO-8601 UTC timestamp with second precision and a trailing "Z".
    // chrono would be the obvious dependency, but writing this by hand keeps
    // framesage-sys's dep surface small and is six lines of arithmetic.
    let secs = t
        .duration_since(UNIX_EPOCH)
        .context("system time before epoch?!")?
        .as_secs();
    Ok(format_iso8601(secs))
}

fn format_iso8601(epoch_secs: u64) -> String {
    // Civil-date conversion from Unix seconds. The algorithm is the
    // Howard Hinnant chrono recipe (public domain) restricted to 1970+.
    let days = (epoch_secs / 86_400) as i64;
    let secs_of_day = epoch_secs % 86_400;
    let (hour, minute, second) = (
        (secs_of_day / 3600) as u32,
        ((secs_of_day % 3600) / 60) as u32,
        (secs_of_day % 60) as u32,
    );

    // Shift epoch from 1970-01-01 to 0000-03-01 to make the leap-year math
    // trivial (the year's last month is February).
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    if month <= 2 {
        year += 1;
    }

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_slice_as_bytes(v: &[u16]) -> &[u8] {
    // SAFETY: a [u16] is always 2-byte aligned and 2*N bytes long.
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn wide_lossy(p: PCWSTR) -> String {
    // SAFETY: PCWSTR comes from a `w!` static; nul-terminated.
    unsafe {
        let mut len = 0usize;
        while *p.0.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p.0, len))
    }
}

#[cfg(test)]
mod tests {
    use super::format_iso8601;

    #[test]
    fn iso8601_known_dates() {
        // 1970-01-01T00:00:00Z is epoch 0.
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00Z");
        // 2000-01-01T00:00:00Z is 946684800.
        assert_eq!(format_iso8601(946_684_800), "2000-01-01T00:00:00Z");
        // 2024-02-29T12:34:56Z is 1709210096.
        assert_eq!(format_iso8601(1_709_210_096), "2024-02-29T12:34:56Z");
        // 2038-01-19T03:14:07Z (just before 32-bit signed overflow).
        assert_eq!(format_iso8601(2_147_483_647), "2038-01-19T03:14:07Z");
    }
}
