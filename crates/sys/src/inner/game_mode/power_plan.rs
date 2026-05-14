//! Read and set the active Windows power plan.
//!
//! `PowerGetActiveScheme` returns a GUID via an LocalAlloc'd pointer that the
//! caller must `LocalFree`. We do that immediately after copying the GUID out,
//! so callers never see the raw pointer.
//!
//! `PowerSetActiveScheme` takes a `*const GUID`. We accept either a well-known
//! `PowerPlanId` variant or a `Custom(guid_string)` and parse to a GUID at
//! the boundary.

use std::ptr;

use anyhow::{anyhow, Result};
use windows::core::GUID;
use windows::Win32::Foundation::LocalFree;
use windows::Win32::System::Power::{PowerGetActiveScheme, PowerSetActiveScheme};

use framesage_core::PowerPlanId;

/// Return the GUID of the active power plan, mapped to a `PowerPlanId`. A
/// well-known GUID is mapped to its named variant; anything else becomes
/// `PowerPlanId::Custom(<guid>)`.
pub fn get_active_plan() -> Result<PowerPlanId> {
    let mut guid_ptr: *mut GUID = ptr::null_mut();
    // SAFETY: PowerGetActiveScheme writes a pointer to a freshly-allocated
    // GUID into our out-param. None for the registry-key arg uses the default
    // user-power-key. Errors are surfaced via WIN32_ERROR.
    let status = unsafe { PowerGetActiveScheme(None, &mut guid_ptr) };
    if status.is_err() {
        return Err(anyhow!("PowerGetActiveScheme returned {}", status.0));
    }
    if guid_ptr.is_null() {
        return Err(anyhow!(
            "PowerGetActiveScheme returned success but null guid"
        ));
    }

    // SAFETY: API contract says guid_ptr is a valid GUID pointer on success.
    let guid = unsafe { *guid_ptr };
    // SAFETY: documented free of the LocalAlloc'd buffer. Ignore failure —
    // it would only mean a leak, not unsoundness.
    let _ = unsafe { LocalFree(windows::Win32::Foundation::HLOCAL(guid_ptr as _)) };

    Ok(guid_to_plan_id(&guid))
}

/// Set the active power plan. Returns `Ok(())` on success, even if the plan
/// was already active (Windows returns no error in that case).
pub fn set_active_plan(plan: &PowerPlanId) -> Result<()> {
    let guid = parse_guid_str(plan.guid())?;
    // SAFETY: documented call. We pass a pointer to our local GUID which
    // lives for the call's duration. None for the registry-key arg uses the
    // default user-power-key.
    let status = unsafe { PowerSetActiveScheme(None, Some(&guid)) };
    if status.is_err() {
        return Err(anyhow!(
            "PowerSetActiveScheme({}) returned {}",
            plan.guid(),
            status.0
        ));
    }
    Ok(())
}

fn guid_to_plan_id(g: &GUID) -> PowerPlanId {
    let canonical = format_guid_lower(g);
    match canonical.as_str() {
        "381b4222-f694-41f0-9685-ff5bb260df2e" => PowerPlanId::Balanced,
        "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c" => PowerPlanId::HighPerformance,
        "a1841308-3541-4fab-bc81-f71556f20b4a" => PowerPlanId::PowerSaver,
        "e9a42b02-d5df-448d-aa00-03f14749eb61" => PowerPlanId::UltimatePerformance,
        _ => PowerPlanId::Custom(canonical),
    }
}

fn format_guid_lower(g: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7],
    )
}

fn parse_guid_str(s: &str) -> Result<GUID> {
    // Accept lowercase, uppercase, and braced forms — Windows is permissive.
    let trimmed = s.trim_matches(|c| c == '{' || c == '}');
    let cleaned: String = trimmed.chars().filter(|c| *c != '-').collect();
    if cleaned.len() != 32 {
        return Err(anyhow!("invalid GUID length: {s:?}"));
    }
    let bytes = hex_decode_32(&cleaned).ok_or_else(|| anyhow!("invalid GUID hex: {s:?}"))?;
    Ok(GUID {
        data1: u32::from_be_bytes(bytes[0..4].try_into().unwrap()),
        data2: u16::from_be_bytes(bytes[4..6].try_into().unwrap()),
        data3: u16::from_be_bytes(bytes[6..8].try_into().unwrap()),
        data4: bytes[8..16].try_into().unwrap(),
    })
}

fn hex_decode_32(s: &str) -> Option<[u8; 16]> {
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    let b = s.as_bytes();
    for i in 0..16 {
        let hi = hex_nibble(b[i * 2])?;
        let lo = hex_nibble(b[i * 2 + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_guids_round_trip() {
        for plan in [
            PowerPlanId::Balanced,
            PowerPlanId::HighPerformance,
            PowerPlanId::PowerSaver,
            PowerPlanId::UltimatePerformance,
        ] {
            let guid = parse_guid_str(plan.guid()).expect("parse");
            let back = guid_to_plan_id(&guid);
            assert_eq!(plan, back, "round-trip failed for {plan:?}");
        }
    }

    #[test]
    fn custom_guid_round_trip_lowercases() {
        let guid = parse_guid_str("AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE").expect("parse");
        match guid_to_plan_id(&guid) {
            PowerPlanId::Custom(s) => {
                assert_eq!(s, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn braced_guid_parses() {
        assert!(parse_guid_str("{381b4222-f694-41f0-9685-ff5bb260df2e}").is_ok());
    }

    #[test]
    fn invalid_guid_returns_error() {
        assert!(parse_guid_str("not-a-guid").is_err());
        assert!(parse_guid_str("").is_err());
        assert!(parse_guid_str("38").is_err());
    }
}
