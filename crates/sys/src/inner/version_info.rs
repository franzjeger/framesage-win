//! Read FileDescription / CompanyName / ProductName from a PE's version
//! resource (the "Details" tab in Explorer's file-properties dialog).
//!
//! Uses the documented `version.dll` trio: `GetFileVersionInfoSizeW` →
//! `GetFileVersionInfoW` → `VerQueryValueW`. Same calls Task Manager,
//! Process Explorer, and every other process viewer use; anti-cheat-clean.
//!
//! Strings live in a versioned `\StringFileInfo\<lang><codepage>\<Field>`
//! sub-block; the language+codepage pair is itself stored in
//! `\VarFileInfo\Translation`. We read that block first, then fall back to
//! the common en-US / neutral candidates if a file's translation block is
//! missing or the field isn't present in the listed language.

use std::ffi::c_void;

use anyhow::Result;
use windows::core::PCWSTR;
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};

/// Subset of version-resource fields we surface. All optional: a binary may
/// ship no version resource at all (very old / hand-assembled executables),
/// or may include the resource but omit specific fields.
#[derive(Debug, Clone, Default)]
pub struct VersionInfo {
    /// "FileDescription" — the human-readable one-line label that Task
    /// Manager shows in its Description column ("Microsoft OneDrive",
    /// "Battlefield 6", "Steam Client Service Helper").
    pub description: Option<String>,
    /// "CompanyName" — the publisher string. Useful for distinguishing
    /// e.g. "Microsoft" from "Adobe" at a glance.
    pub company: Option<String>,
    /// "ProductName" — the marketing name for the larger product the binary
    /// belongs to. Often more recognisable than the exe filename.
    pub product_name: Option<String>,
}

impl VersionInfo {
    pub fn is_empty(&self) -> bool {
        self.description.is_none() && self.company.is_none() && self.product_name.is_none()
    }
}

/// Read the version resource at `path`. Returns `Err` only on programmer
/// error (path can't be encoded as UTF-16); a missing or unreadable
/// resource yields `Ok(VersionInfo::default())` so callers can cache the
/// result and skip retrying paths that have no resource.
pub fn read_version_info(path: &str) -> Result<VersionInfo> {
    if path.is_empty() {
        return Ok(VersionInfo::default());
    }
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    // First call: how big is the resource?
    // SAFETY: documented call. Passing `None` for the out handle is allowed.
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None) };
    if size == 0 {
        return Ok(VersionInfo::default());
    }

    // Second call: read it.
    let mut buf = vec![0u8; size as usize];
    // SAFETY: buf has exactly `size` bytes; documented API.
    let result = unsafe {
        GetFileVersionInfoW(
            PCWSTR(wide.as_ptr()),
            0,
            size,
            buf.as_mut_ptr() as *mut c_void,
        )
    };
    if result.is_err() {
        return Ok(VersionInfo::default());
    }

    // Translation block: tells us which lang+codepage StringFileInfo entries
    // are actually present in this binary. Most binaries ship exactly one.
    let mut candidates = read_translation_candidates(&buf);
    // Append known fallbacks so even resource-but-no-Translation files yield
    // something. Order: try file-declared langs first, then well-known pairs.
    for fb in WELL_KNOWN_LANG_CODEPAGE_PAIRS {
        if !candidates.contains(fb) {
            candidates.push(*fb);
        }
    }

    Ok(VersionInfo {
        description: lookup_string_field(&buf, &candidates, "FileDescription"),
        company: lookup_string_field(&buf, &candidates, "CompanyName"),
        product_name: lookup_string_field(&buf, &candidates, "ProductName"),
    })
}

/// (lang_id, codepage) pairs that ship with most US-English builds. Order
/// matters — first hit wins. Unicode pairs are listed first because newer
/// toolchains default to them.
const WELL_KNOWN_LANG_CODEPAGE_PAIRS: &[(u16, u16)] = &[
    (0x0409, 0x04B0), // en-US, Unicode
    (0x0409, 0x04E4), // en-US, 1252
    (0x0000, 0x04B0), // neutral, Unicode
    (0x0000, 0x04E4), // neutral, 1252
];

fn read_translation_candidates(buf: &[u8]) -> Vec<(u16, u16)> {
    let subblock: Vec<u16> = "\\VarFileInfo\\Translation\0".encode_utf16().collect();
    let mut value_ptr: *mut c_void = std::ptr::null_mut();
    let mut value_len: u32 = 0;
    // SAFETY: buf came from GetFileVersionInfoW and is a valid resource blob.
    // value_ptr + value_len are out-params; the returned pointer points back
    // into `buf`, no allocation occurs.
    let ok = unsafe {
        VerQueryValueW(
            buf.as_ptr() as *const c_void,
            PCWSTR(subblock.as_ptr()),
            &mut value_ptr,
            &mut value_len,
        )
    };
    if !ok.as_bool() || value_ptr.is_null() || value_len < 4 {
        return Vec::new();
    }
    // Translation block is an array of LANGANDCODEPAGE structs:
    //   struct { WORD wLanguage; WORD wCodePage; }
    // value_len is in bytes; pairs is value_len/4.
    let pair_count = (value_len as usize) / 4;
    // SAFETY: pointer is into our `buf` and pair_count is bounded by the
    // size the kernel reported.
    let words = unsafe { std::slice::from_raw_parts(value_ptr as *const u16, pair_count * 2) };
    let mut out = Vec::with_capacity(pair_count);
    for chunk in words.chunks_exact(2) {
        out.push((chunk[0], chunk[1]));
    }
    out
}

fn lookup_string_field(buf: &[u8], candidates: &[(u16, u16)], field: &str) -> Option<String> {
    for &(lang, cp) in candidates {
        let subblock = format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\{field}");
        let wide: Vec<u16> = subblock.encode_utf16().chain(std::iter::once(0)).collect();
        let mut value_ptr: *mut c_void = std::ptr::null_mut();
        let mut value_len: u32 = 0;
        // SAFETY: same as in read_translation_candidates.
        let ok = unsafe {
            VerQueryValueW(
                buf.as_ptr() as *const c_void,
                PCWSTR(wide.as_ptr()),
                &mut value_ptr,
                &mut value_len,
            )
        };
        if !ok.as_bool() || value_ptr.is_null() || value_len == 0 {
            continue;
        }
        // value_len is in WCHARs (NOT bytes) including the terminating null.
        // Strip the null and any whitespace, reject if the field came back
        // empty (some binaries ship "<None>" or trailing whitespace).
        let len_chars = (value_len as usize).saturating_sub(1);
        // SAFETY: pointer is into `buf` and len_chars is bounded by value_len.
        let slice = unsafe { std::slice::from_raw_parts(value_ptr as *const u16, len_chars) };
        let s = String::from_utf16_lossy(slice).trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}
