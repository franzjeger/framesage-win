//! Build-time: synthesise the FrameSage logo as a multi-resolution .ico and
//! embed it into framesage-tray.exe so Explorer, the Start menu, Alt-Tab,
//! and the Windows 11 taskbar all show the brand mark instead of the
//! default Rust-binary icon.
//!
//! The logo math duplicates the runtime renderer in `src/main.rs`
//! (`framesage_logo_rgba`). Two paths exist for the same reason: build.rs
//! runs in its own compilation unit and can't `use` modules from the bin
//! crate. The two functions are byte-identical and only ~60 lines; a unit
//! test in main.rs could pin them together if they ever drift, but for
//! now they're small enough that duplication is the lighter trade-off
//! than introducing a shared crate just for the logo.
//!
//! Skipped on non-Windows targets: embed-resource needs windres on Linux
//! hosts to build a .res for a Windows target, and CI's cross-check job
//! doesn't install mingw-w64. The runtime `with_icon()` and tray-icon
//! paths handle the in-app visuals on every host.

use std::env;
use std::path::PathBuf;

fn main() {
    // Only embed on Windows targets — the .ico mechanism is Windows-PE-only.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    // And only when the host is Windows too — otherwise embed-resource
    // would shell out to windres, which CI's ubuntu cross-check job
    // doesn't have installed. The in-app tray icon + window icon still
    // work via the runtime renderer either way.
    if !cfg!(target_os = "windows") {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR not set by cargo"));
    let ico_path = out_dir.join("framesage.ico");
    let manifest_path = out_dir.join("framesage.manifest");
    let rc_path = out_dir.join("framesage.rc");

    write_ico(&ico_path);
    write_manifest(&manifest_path);
    write_rc(&rc_path, &ico_path, &manifest_path);

    // We ship our own DPI-aware manifest as part of framesage.rc, so we
    // don't ask embed-resource for anything extra. `manifest_optional()` on
    // the result just turns "no rc compiler in PATH" into Ok rather than a
    // hard error — useful on systems where mingw / rc.exe isn't installed.
    embed_resource::compile(&rc_path, embed_resource::NONE)
        .manifest_optional()
        .expect("embed-resource: compile FrameSage .rc");

    println!("cargo:rerun-if-changed=build.rs");
}

fn write_rc(
    rc_path: &std::path::Path,
    ico_path: &std::path::Path,
    manifest_path: &std::path::Path,
) {
    // .rc files use backslash escaping; on Windows the path already has them
    // — we need to double-escape so the resource compiler sees a single \.
    let ico_str = ico_path.display().to_string().replace('\\', "\\\\");
    let manifest_str = manifest_path.display().to_string().replace('\\', "\\\\");
    // "1 24" is CREATEPROCESS_MANIFEST_RESOURCE_ID (1) and RT_MANIFEST (24).
    let body = format!("IDI_ICON1 ICON \"{ico_str}\"\n1 24 \"{manifest_str}\"\n");
    std::fs::write(rc_path, body).expect("write framesage.rc");
}

/// Application manifest declaring per-monitor v2 DPI awareness so Windows
/// hands us WM_DPICHANGED events and lets eframe rescale on its own when the
/// window crosses a monitor boundary. Without this, dragging the window
/// between two monitors with different DPI scaling causes the OS to
/// auto-rescale the window contents — egui then re-rescales them on the
/// next frame, and the result is the "expands randomly, goes crazy"
/// behaviour reported during hardware validation.
fn write_manifest(path: &std::path::Path) {
    let body = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity
      type="win32"
      name="FrameSage.Tray"
      version="1.0.0.0"
      processorArchitecture="*"/>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2,PerMonitor</dpiAwareness>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/PM</dpiAware>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and Windows 11 -->
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
</assembly>
"#;
    std::fs::write(path, body).expect("write framesage.manifest");
}

fn write_ico(out: &std::path::Path) {
    // Render at the four most common Windows shell sizes. Windows picks the
    // best fit at runtime, so shipping 16/32/48/64 covers every UI surface
    // from the tray (16px) through Alt-Tab (32px) through taskbar (48px+).
    let sizes: &[u32] = &[16, 32, 48, 64];
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &size in sizes {
        let (rgba, w, h) = framesage_logo_rgba(size);
        let image = ico::IconImage::from_rgba_data(w, h, rgba);
        dir.add_entry(ico::IconDirEntry::encode(&image).expect("encode ICO entry"));
    }
    let f = std::fs::File::create(out).expect("create framesage.ico");
    dir.write(f).expect("write framesage.ico");
}

// ─── Logo renderer (duplicated from src/main.rs) ────────────────────────────
//
// Keep in sync with `framesage_logo_rgba()` in main.rs. The two are byte-
// identical for a given `size`; the main.rs version always renders at 64.

fn framesage_logo_rgba(size: u32) -> (Vec<u8>, u32, u32) {
    let s = size as f32;
    let scale = s / 64.0;
    let center = (s - 1.0) / 2.0;

    let bg = [0x16u8, 0x1b, 0x22];
    let ring = [0x58u8, 0xa6, 0xff];
    let f_color = [0x9bu8, 0xca, 0xff];

    let disc_outer = 30.5 * scale;
    let disc_inner = 27.5 * scale;

    let f_left = 21.0 * scale;
    let f_right = 45.0 * scale;
    let f_top = 16.0 * scale;
    let f_bot = 48.0 * scale;
    let bar_thick = 7.0 * scale;
    let top_bar_h = 7.0 * scale;
    let mid_bar_y_top = 30.0 * scale;
    let mid_bar_h = 6.0 * scale;
    let mid_bar_right = 40.0 * scale;

    let mut rgba: Vec<u8> = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let fx = x as f32 + 0.5;
            let fy = y as f32 + 0.5;
            let dx = fx - center - 0.5;
            let dy = fy - center - 0.5;
            let r = (dx * dx + dy * dy).sqrt();

            let mut pixel = [0u8, 0, 0, 0];

            if r <= disc_outer {
                let disc_alpha = smoothstep(disc_outer + 0.5, disc_outer - 0.5, r);
                let ring_alpha = smoothstep(disc_inner - 0.5, disc_inner + 0.5, r).min(smoothstep(
                    disc_outer + 0.5,
                    disc_outer - 0.5,
                    r,
                ));

                let a = (disc_alpha * 255.0).clamp(0.0, 255.0) as u8;
                pixel = [bg[0], bg[1], bg[2], a];

                if ring_alpha > 0.0 {
                    let a = (ring_alpha * 255.0).clamp(0.0, 255.0) as u8;
                    pixel = over(pixel, [ring[0], ring[1], ring[2], a]);
                }

                let on_vertical_bar = in_rect(fx, fy, f_left, f_top, f_left + bar_thick, f_bot);
                let on_top_bar = in_rect(fx, fy, f_left, f_top, f_right, f_top + top_bar_h);
                let on_mid_bar = in_rect(
                    fx,
                    fy,
                    f_left,
                    mid_bar_y_top,
                    mid_bar_right,
                    mid_bar_y_top + mid_bar_h,
                );
                if on_vertical_bar || on_top_bar || on_mid_bar {
                    pixel = over(pixel, [f_color[0], f_color[1], f_color[2], 255]);
                }
            }

            rgba.extend_from_slice(&pixel);
        }
    }

    (rgba, size, size)
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn in_rect(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> bool {
    px >= x0 && px < x1 && py >= y0 && py < y1
}

fn over(dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
    let sa = src[3] as f32 / 255.0;
    let da = dst[3] as f32 / 255.0;
    let out_a = sa + da * (1.0 - sa);
    if out_a <= f32::EPSILON {
        return [0, 0, 0, 0];
    }
    let blend = |s: u8, d: u8| -> u8 {
        let v = (s as f32 * sa + d as f32 * da * (1.0 - sa)) / out_a;
        v.clamp(0.0, 255.0) as u8
    };
    [
        blend(src[0], dst[0]),
        blend(src[1], dst[1]),
        blend(src[2], dst[2]),
        (out_a * 255.0).clamp(0.0, 255.0) as u8,
    ]
}
