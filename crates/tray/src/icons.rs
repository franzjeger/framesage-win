//! Tray-side executable-icon extraction + cache.
//!
//! Process Lasso / Task Manager / Process Explorer all show a small icon
//! next to each process row. We do the same: ask Windows for the shell's
//! small-icon representation of the exe (`SHGetFileInfoW`), rasterise the
//! returned `HICON` to RGBA bytes (`DrawIconEx` onto a top-down 32-bit
//! `CreateDIBSection`), upload to an egui texture, and cache by path.
//!
//! Extraction is bounded per frame so the first poll-after-launch doesn't
//! freeze the UI thread for 1 s while 200 icons populate. Subsequent
//! frames find cached entries instantly.

#![cfg(windows)]

use std::collections::HashMap;
use std::ffi::c_void;

use anyhow::{anyhow, Result};
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES,
};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, DrawIconEx, DI_NORMAL, HICON};

/// Pixel size we ask Windows for and render at. 16×16 matches
/// `SHGFI_SMALLICON`; matches the row height (~18px) without scaling
/// artefacts.
pub const ICON_PX: u32 = 16;

/// One row of the cache. Holds the loaded texture on success; absent
/// otherwise (negative caching — we don't retry after a failed extraction
/// because the result rarely changes per-path during a session).
enum CacheEntry {
    Loaded(egui::TextureHandle),
    Failed,
}

/// Path-keyed icon cache. `&'static` strings would be nice but the engine
/// hands us owned `String` paths, so we pay the keying allocation. Cache
/// outlives the session.
#[derive(Default)]
pub struct IconCache {
    entries: HashMap<String, CacheEntry>,
}

impl IconCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a texture for `path`, extracting if not yet cached.
    ///
    /// `budget_remaining` bounds how many fresh extractions this call (and
    /// the rest of the frame) may do — callers reserve a frame-wide budget
    /// (e.g. 3) so the UI thread never stalls on a wave of cache misses.
    /// When the budget is exhausted, misses return `None` and the row
    /// renders without an icon until a future frame.
    pub fn get_or_load(
        &mut self,
        ctx: &egui::Context,
        path: &str,
        budget_remaining: &mut u32,
    ) -> Option<egui::TextureHandle> {
        if let Some(entry) = self.entries.get(path) {
            return match entry {
                CacheEntry::Loaded(t) => Some(t.clone()),
                CacheEntry::Failed => None,
            };
        }
        if *budget_remaining == 0 || path.is_empty() {
            return None;
        }
        *budget_remaining -= 1;

        let entry = match extract_icon_rgba(path, ICON_PX) {
            Ok(rgba) => {
                let texture = load_texture(ctx, path, &rgba, ICON_PX);
                CacheEntry::Loaded(texture)
            }
            Err(_) => CacheEntry::Failed,
        };
        let loaded_tex = match &entry {
            CacheEntry::Loaded(t) => Some(t.clone()),
            CacheEntry::Failed => None,
        };
        self.entries.insert(path.to_string(), entry);
        loaded_tex
    }
}

fn load_texture(ctx: &egui::Context, name: &str, rgba: &[u8], size: u32) -> egui::TextureHandle {
    let image = egui::ColorImage::from_rgba_unmultiplied([size as usize, size as usize], rgba);
    ctx.load_texture(
        format!("framesage-icon:{name}"),
        image,
        egui::TextureOptions::LINEAR,
    )
}

/// Extract the small icon for the file at `path` and return it as RGBA bytes
/// in a top-down `size × size` raster.
///
/// Implementation chain: `SHGetFileInfoW(SHGFI_ICON | SHGFI_SMALLICON)` to
/// retrieve a managed `HICON`, then `DrawIconEx` onto a 32-bit
/// `CreateDIBSection`, then a BGRA→RGBA swizzle into a fresh Vec. The icon
/// is destroyed before return so HICON leaks can't accumulate per session.
///
/// `SHGFI_USEFILEATTRIBUTES` is set so the call doesn't touch the disk for
/// files we don't actually need to stat — we already know the file exists
/// (the engine just enumerated it). Saves real time on networked or
/// permission-restricted paths.
fn extract_icon_rgba(path: &str, size: u32) -> Result<Vec<u8>> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHFILEINFOW::default();
    // SAFETY: documented call. `wide` is null-terminated; `info` is a valid
    // out-pointer; we pass its size correctly.
    let result = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES,
        )
    };
    if result == 0 {
        return Err(anyhow!("SHGetFileInfoW returned 0 for {path}"));
    }
    let hicon = info.hIcon;
    if hicon.is_invalid() {
        return Err(anyhow!("SHGetFileInfoW returned null HICON for {path}"));
    }

    // SAFETY: hicon is valid (checked above). `hicon_to_rgba` consumes it
    // logically — we destroy after the rasterise regardless of outcome.
    let rgba = unsafe { hicon_to_rgba(hicon, size) };
    // SAFETY: hicon is the one we just received from SHGetFileInfoW; we own
    // its lifetime per the SHGetFileInfoW contract when SHGFI_ICON is set.
    let _ = unsafe { DestroyIcon(hicon) };
    rgba
}

/// Rasterise an `HICON` to RGBA bytes (top-down). Allocates a temporary
/// memory DC + DIB, draws the icon onto it via `DrawIconEx`, copies the
/// pixel data out with a BGRA→RGBA swizzle, and frees everything. Safe to
/// call concurrently; no shared GDI state is mutated.
unsafe fn hicon_to_rgba(hicon: HICON, size: u32) -> Result<Vec<u8>> {
    // SAFETY: passing HWND::default() requests the screen DC, which is
    // always available; both DC handles are released at the end of the fn.
    let screen_dc = GetDC(HWND::default());
    if screen_dc.is_invalid() {
        return Err(anyhow!("GetDC(screen) failed"));
    }
    // SAFETY: screen_dc is valid (checked above).
    let mem_dc = CreateCompatibleDC(screen_dc);
    if mem_dc.is_invalid() {
        ReleaseDC(HWND::default(), screen_dc);
        return Err(anyhow!("CreateCompatibleDC failed"));
    }

    let bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            // Negative height = top-down (origin at top-left), which is the
            // layout `egui::ColorImage::from_rgba_unmultiplied` expects.
            biHeight: -(size as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut c_void = std::ptr::null_mut();
    // SAFETY: bi populated above; bits is a valid out-pointer that the API
    // will set to a pointer into the DIB's pixel buffer (owned by the
    // section, freed when we `DeleteObject` the section).
    let dib = CreateDIBSection(screen_dc, &bi, DIB_RGB_COLORS, &mut bits, None, 0);
    let dib = match dib {
        Ok(h) if !h.is_invalid() => h,
        _ => {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(HWND::default(), screen_dc);
            return Err(anyhow!("CreateDIBSection failed"));
        }
    };

    // SAFETY: mem_dc and dib are valid; the API returns the previously
    // selected GDI object which we restore before deletion.
    let old_obj = SelectObject(mem_dc, dib);

    // Draw the icon. Returns BOOL — non-zero on success.
    if DrawIconEx(
        mem_dc,
        0,
        0,
        hicon,
        size as i32,
        size as i32,
        0,
        None,
        DI_NORMAL,
    )
    .is_err()
    {
        SelectObject(mem_dc, old_obj);
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND::default(), screen_dc);
        return Err(anyhow!("DrawIconEx failed"));
    }

    // Copy the pixel data with a BGRA→RGBA swizzle. CreateDIBSection with
    // biBitCount=32 + BI_RGB gives us BGRA-in-memory; egui wants RGBA.
    let n = (size as usize) * (size as usize) * 4;
    let mut rgba = vec![0u8; n];
    // SAFETY: `bits` was set by CreateDIBSection to point at an n-byte
    // buffer (top-down 32-bit raster of exactly size×size pixels).
    let src = std::slice::from_raw_parts(bits as *const u8, n);
    for i in (0..n).step_by(4) {
        rgba[i] = src[i + 2];
        rgba[i + 1] = src[i + 1];
        rgba[i + 2] = src[i];
        rgba[i + 3] = src[i + 3];
    }

    // Cleanup. SAFETY: each handle is one we own.
    SelectObject(mem_dc, old_obj);
    let _ = DeleteObject(HGDIOBJ(dib.0));
    let _ = DeleteDC(mem_dc);
    ReleaseDC(HWND::default(), screen_dc);

    Ok(rgba)
}
