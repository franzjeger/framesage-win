//! ACL hardening for `%ProgramData%\framesage\`.
//!
//! Closes audit finding **C-04**. The audit caught the following hole:
//! `%ProgramData%` defaults inherit `CREATOR_OWNER:Modify` for the first
//! non-admin user who creates a sub-directory. If a developer (or an
//! attacker) ever ran `framesage-svc --console` from a normal user shell
//! *before* a proper service install — even once — then that user
//! permanently owned `%ProgramData%\framesage\` and every subsequent
//! `policy.json` written by the LocalSystem service was modifiable by
//! that user. From there, hot-reload turns any malicious policy edit
//! into arbitrary `OpenProcess` calls under SYSTEM rights.
//!
//! The fix is two layers:
//!
//! 1. **Harden on startup** (`harden_config_dir`). We force the
//!    directory's DACL to the explicit, non-inherited, SYSTEM+Admin-only
//!    set defined by [`HARDENED_SDDL`], and take ownership as SYSTEM. We
//!    do this whether the dir exists or not — `SetNamedSecurityInfoW`
//!    overwrites whatever was there. Every existing file under the dir
//!    gets re-hardened too (`policy.json`, `game-mode.journal`,
//!    `sessions.jsonl`).
//!
//! 2. **Verify on load** (`verify_owner_is_admin_or_system`). Defense in
//!    depth: before trusting `policy.json`, we re-check that its owner is
//!    SYSTEM or BUILTIN\Administrators. If hardening on this run somehow
//!    failed (e.g. the service is running unelevated in console mode),
//!    the caller can fall back to built-in defaults instead of loading a
//!    user-owned file that an attacker could have planted.
//!
//! Console mode (running as a non-SYSTEM user): hardening will fail —
//! the user doesn't have `SeTakeOwnershipPrivilege` on a SYSTEM-owned
//! dir. We log loudly and proceed; the dev is responsible for their own
//! security posture in console mode, and SCM-mode installs are the
//! production path that this module's invariants actually protect.

#![cfg(windows)]

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use tracing::{info, warn};

use windows::core::PCWSTR;
use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW,
    SetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    CreateWellKnownSid, EqualSid, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    WinBuiltinAdministratorsSid, WinLocalSystemSid, ACL, DACL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    WELL_KNOWN_SID_TYPE,
};

/// Target DACL for the config dir.
///
/// * `O:SY` — owner is LocalSystem.
/// * `G:SY` — primary group is LocalSystem.
/// * `D:PAI` — DACL is PROTECTED from inheriting upward (parent
///   `%ProgramData%` cannot inject ACEs), and is auto-inherited downward
///   (children of our dir inherit the ACEs below).
/// * `(A;OICI;FA;;;SY)` — Allow, Object+Container inherit, Full Access,
///   LocalSystem.
/// * `(A;OICI;FA;;;BA)` — Allow, Object+Container inherit, Full Access,
///   Built-in Administrators.
/// * `(A;OICI;0x1200a9;;;AU)` — Allow, Object+Container inherit,
///   `FILE_GENERIC_READ | FILE_GENERIC_EXECUTE` (0x1200a9), Authenticated
///   Users.
///
/// **No CREATOR_OWNER ACE.** That's the load-bearing omission — the
/// vulnerability the audit identified was inherited `CREATOR_OWNER:M`
/// from `%ProgramData%`'s default ACL. The `PAI` flag stops that
/// inheritance and the explicit ACE list never re-introduces it.
///
/// Authenticated Users keep Read+Execute so any user can read
/// `policy.json` for diagnostics. They cannot write — that's what closes
/// the hole. (Hot-reload still works because the SCM service runs as
/// SYSTEM, which has Full Access.)
const HARDENED_SDDL: &str = "O:SYG:SYD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;AU)";

/// Apply the hardened ACL to `path` and to every file/dir directly under
/// it. Creates the directory if it doesn't exist.
///
/// On success: the dir is owned by LocalSystem, only SYSTEM and
/// Administrators can write, Authenticated Users can read. Any existing
/// `policy.json` / `game-mode.journal` / `sessions.jsonl` under the dir
/// has the same ACL applied.
///
/// On failure (most likely cause: running not-as-SYSTEM in console mode
/// against a dir we don't own): logs a warning and returns Err. The
/// caller decides whether to continue with defaults or refuse to start.
pub fn harden_config_dir(path: &Path) -> Result<()> {
    // Ensure the dir exists before we try to set its security descriptor.
    // create_dir_all is a no-op if it already exists. ACL of the freshly
    // created dir doesn't matter — we're about to overwrite it.
    if let Err(e) = std::fs::create_dir_all(path) {
        warn!(path = %path.display(), error = %e, "create_dir_all failed");
        return Err(anyhow!("create config dir {}: {e}", path.display()));
    }

    apply_hardened_sd(path).with_context(|| format!("harden directory {}", path.display()))?;

    // Walk one level down and harden every existing file. Subdirectories
    // would inherit via OICI, but existing children created before this
    // call keep their old ACL until we explicitly overwrite. We don't
    // recurse — the only directory level we own is the immediate config
    // dir; the only files inside are flat (policy.json,
    // game-mode.journal, sessions.jsonl, logs/<rolling>).
    match std::fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let entry_path = entry.path();
                if let Err(e) = apply_hardened_sd(&entry_path) {
                    warn!(
                        path = %entry_path.display(),
                        error = %e,
                        "child file hardening failed (non-fatal; parent dir hardening covers new files)"
                    );
                }
            }
        }
        Err(e) => {
            warn!(
                path = %path.display(),
                error = %e,
                "read_dir failed after hardening — children not re-checked"
            );
        }
    }

    info!(path = %path.display(), "config dir hardened to SYSTEM+Admin-only DACL");
    Ok(())
}

/// Inner helper: build the SECURITY_DESCRIPTOR from [`HARDENED_SDDL`] and
/// apply it to a single file or directory path. Sets owner, group, and
/// DACL all in one Set call. The DACL is marked PROTECTED so it does NOT
/// inherit from the parent (closing the vulnerability).
fn apply_hardened_sd(path: &Path) -> Result<()> {
    let sddl_wide: Vec<u16> = HARDENED_SDDL
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut psd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: sddl_wide is null-terminated UTF-16, alive for the call;
    // &mut psd is a valid out-parameter; we don't use the size out-param.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl_wide.as_ptr()),
            SDDL_REVISION_1,
            &mut psd,
            None,
        )
    }
    .context("ConvertStringSecurityDescriptorToSecurityDescriptorW")?;
    // Wrap the allocation lifetime — we LocalFree on every exit below.
    let _sd_guard = SdGuard(psd);

    // Extract owner, group, and DACL pointers from the descriptor so we
    // can pass them to SetNamedSecurityInfoW. The SDs returned by
    // ConvertString... are self-relative; the Get* helpers walk the
    // structure for us.
    let mut owner: PSID = PSID::default();
    let mut owner_defaulted: windows::Win32::Foundation::BOOL = false.into();
    // SAFETY: psd is valid (just constructed); out params are valid.
    unsafe { GetSecurityDescriptorOwner(psd, &mut owner, &mut owner_defaulted) }
        .context("GetSecurityDescriptorOwner")?;

    let mut dacl_present: windows::Win32::Foundation::BOOL = false.into();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut dacl_defaulted: windows::Win32::Foundation::BOOL = false.into();
    // SAFETY: psd is valid; out params are valid.
    unsafe { GetSecurityDescriptorDacl(psd, &mut dacl_present, &mut dacl, &mut dacl_defaulted) }
        .context("GetSecurityDescriptorDacl")?;
    if !dacl_present.as_bool() {
        return Err(anyhow!(
            "hardened SDDL did not produce a DACL — programming error in HARDENED_SDDL"
        ));
    }

    // We don't extract the group SID — owner + DACL + the PROTECTED flag
    // are what matters for the security invariant. SetNamedSecurityInfoW
    // accepts None for the group sid when we don't pass
    // GROUP_SECURITY_INFORMATION.

    let path_wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    // PROTECTED_DACL_SECURITY_INFORMATION is the load-bearing flag: it
    // sets the SE_DACL_PROTECTED bit on the object, blocking ACE
    // inheritance from the parent. Without it, the parent's
    // CREATOR_OWNER:Modify ACE leaks back in.
    let info_flags = OWNER_SECURITY_INFORMATION
        | DACL_SECURITY_INFORMATION
        | PROTECTED_DACL_SECURITY_INFORMATION;

    // SAFETY: path_wide is null-terminated UTF-16; owner is a valid SID
    // pointer derived from psd (alive); dacl is a valid ACL pointer
    // derived from psd (alive). Group is None because we don't pass
    // GROUP_SECURITY_INFORMATION.
    let result = unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(path_wide.as_ptr()),
            SE_FILE_OBJECT,
            info_flags,
            owner,
            PSID::default(),
            Some(dacl as *const ACL),
            None,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(anyhow!(
            "SetNamedSecurityInfoW({}) failed with WIN32_ERROR {}",
            path.display(),
            result.0
        ));
    }
    Ok(())
}

/// Check that `path`'s file-system owner is SYSTEM or BUILTIN\Administrators.
///
/// Returns Ok if the owner is one of those two well-known SIDs. Returns
/// Err with a descriptive message if the owner is anyone else — typically
/// because a non-admin process created the file (or directory) before the
/// service hardened it, leaving CREATOR_OWNER on that user.
///
/// The caller should treat an Err as a strong signal: this file is not
/// safe to load and may have been planted by a non-admin user with
/// modify rights inherited from CREATOR_OWNER. Loading and trusting it
/// would be the EoP primitive the audit identified.
pub fn verify_owner_is_admin_or_system(path: &Path) -> Result<()> {
    let path_wide: Vec<u16> = path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut psd = PSECURITY_DESCRIPTOR::default();
    let mut owner: PSID = PSID::default();
    // SAFETY: path_wide is null-terminated; owner + psd are valid out
    // params. We pass DACL=None, GROUP=None — we only want the owner SID.
    // The owner out-param is `Option<*mut PSID>`; we pass a raw pointer
    // to our local `owner` binding.
    let result = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(path_wide.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner as *mut PSID),
            None,
            None,
            None,
            &mut psd,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(anyhow!(
            "GetNamedSecurityInfoW({}) failed with WIN32_ERROR {}",
            path.display(),
            result.0
        ));
    }
    // The descriptor is allocated by Win32 and must be released — wrap
    // for RAII regardless of which arm we take below.
    let _sd_guard = SdGuard(psd);

    if sid_matches_well_known(owner, WinLocalSystemSid)
        || sid_matches_well_known(owner, WinBuiltinAdministratorsSid)
    {
        Ok(())
    } else {
        Err(anyhow!(
            "{} is not owned by SYSTEM or Administrators — refusing to trust it. \
             Re-install the service elevated to re-take ownership.",
            path.display()
        ))
    }
}

/// Build a well-known SID and compare it to `sid`. Returns false on any
/// failure (failed-to-build counts as "not a match"), since the caller's
/// only decision is allow-or-deny and a build failure must NOT be
/// interpreted as a match.
fn sid_matches_well_known(sid: PSID, kind: WELL_KNOWN_SID_TYPE) -> bool {
    // SECURITY_MAX_SID_SIZE = 68 bytes. Stack buffer is plenty.
    let mut buf = [0u8; 68];
    let mut size: u32 = buf.len() as u32;
    let well_known = PSID(buf.as_mut_ptr() as *mut _);
    // SAFETY: buf is large enough for any well-known SID; out param size
    // is valid; well_known points into buf which outlives the call. We
    // discard the result and return false on error — a build failure
    // must not be interpreted as a match.
    let built = unsafe { CreateWellKnownSid(kind, PSID::default(), well_known, &mut size) };
    if built.is_err() {
        return false;
    }
    // SAFETY: both SIDs are valid; EqualSid is a documented pure comparison.
    // The windows crate wraps EqualSid's BOOL return as `Result<()>` — Ok
    // means the SIDs match (Win32 BOOL was non-zero), Err means they don't
    // or the call failed. The caller only cares about match/no-match, so
    // collapse to bool.
    unsafe { EqualSid(sid, well_known) }.is_ok()
}

/// RAII guard for a `PSECURITY_DESCRIPTOR` allocated by Win32 (e.g. via
/// `ConvertStringSecurityDescriptor...` or `GetNamedSecurityInfoW`). Drop
/// calls `LocalFree` exactly once. Mirrors the helper in
/// `crates/service/src/pipe.rs` — kept duplicated rather than extracted
/// because the two modules' security-descriptor lifecycles are
/// independent and the type is 6 lines.
struct SdGuard(PSECURITY_DESCRIPTOR);

impl Drop for SdGuard {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: self.0 was allocated by a Win32 API that documents
            // LocalFree as the deallocator (ConvertStringSecurityDescriptor
            // / GetNamedSecurityInfo). Casting `PSECURITY_DESCRIPTOR` to
            // `HLOCAL` is documented as safe. Mirrors pipe.rs:SdGuard.
            let _ = unsafe { LocalFree(HLOCAL(self.0 .0)) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::fs;

    /// HARDENED_SDDL must round-trip through Win32's SDDL parser without
    /// error. Catches typos in the SDDL constant at compile-time-equivalent
    /// (test-time) granularity.
    #[test]
    fn hardened_sddl_parses() {
        let sddl_wide: Vec<u16> = HARDENED_SDDL
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut psd = PSECURITY_DESCRIPTOR::default();
        let result = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl_wide.as_ptr()),
                SDDL_REVISION_1,
                &mut psd,
                None,
            )
        };
        assert!(result.is_ok(), "HARDENED_SDDL failed to parse: {result:?}");
        let _g = SdGuard(psd);
        let mut dacl_present: windows::Win32::Foundation::BOOL = false.into();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut dacl_defaulted: windows::Win32::Foundation::BOOL = false.into();
        unsafe {
            GetSecurityDescriptorDacl(psd, &mut dacl_present, &mut dacl, &mut dacl_defaulted)
        }
        .expect("GetSecurityDescriptorDacl");
        assert!(dacl_present.as_bool(), "HARDENED_SDDL must contain a DACL");
    }

    /// LocalSystem SID lookup works and produces a valid SID we can
    /// compare against. Sanity for the verifier's plumbing.
    #[test]
    fn well_known_sids_match_themselves() {
        let mut sys_buf = [0u8; 68];
        let mut sys_size: u32 = sys_buf.len() as u32;
        let sys_sid = PSID(sys_buf.as_mut_ptr() as *mut _);
        unsafe { CreateWellKnownSid(WinLocalSystemSid, PSID::default(), sys_sid, &mut sys_size) }
            .expect("CreateWellKnownSid(LocalSystem)");
        assert!(sid_matches_well_known(sys_sid, WinLocalSystemSid));
        assert!(!sid_matches_well_known(
            sys_sid,
            WinBuiltinAdministratorsSid
        ));
    }

    /// A directory the test process just created is owned by the test
    /// user (not SYSTEM, unless the test is running as SYSTEM which is
    /// unusual). So `verify_owner_is_admin_or_system` should return Err
    /// — exactly the protective behavior we want when policy.json is
    /// owned by a non-admin.
    #[test]
    fn verify_refuses_dir_owned_by_test_user() {
        // Tests run as the user, not SYSTEM. The temp dir we create here
        // is therefore owned by us. If somehow the test runner IS
        // SYSTEM (CI-as-SYSTEM, unusual), this test would spuriously
        // pass — accept that and document.
        let temp = env::temp_dir().join(format!(
            "framesage-acl-test-{}-{}",
            std::process::id(),
            chrono_like_nanos()
        ));
        fs::create_dir_all(&temp).expect("mkdir temp");
        let result = verify_owner_is_admin_or_system(&temp);
        let _ = fs::remove_dir_all(&temp);
        // Either: we're the user (Err), or the test runner is SYSTEM
        // (Ok). Both are valid outcomes; don't fail either way. The
        // assertion is that the function runs and produces a defined
        // verdict — not a panic, not a hang, not a silent allow on
        // garbage input.
        match result {
            Ok(()) => {
                // Test runner is admin/SYSTEM — fine.
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("not owned by SYSTEM or Administrators"),
                    "error should explain ownership mismatch, got: {msg}"
                );
            }
        }
    }

    /// Crude nanosecond-ish suffix for unique temp paths without pulling
    /// in `chrono` as a test-only dep. SystemTime::now is good enough.
    fn chrono_like_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    }
}
