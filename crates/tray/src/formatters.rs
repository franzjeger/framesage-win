//! Pure formatting + decoding helpers used across the tray UI.
//!
//! Lives in its own module because nothing here touches FramesageApp state
//! or egui rendering — everything is `(plain inputs) → String / Color32`.
//! That makes the helpers trivial to unit-test (which we do, in-tree) and
//! shrinks `main.rs` so the UI rendering paths read more cleanly.
//!
//! Each function's contract is in its rustdoc; the inline tests at the
//! bottom of this file are the executable specification.

use eframe::egui;

use framesage_ipc::StatusSnapshot;

use crate::theme;

// ─── Memory / byte-size formatters ───────────────────────────────────────────

/// Human-readable byte count with one-step unit selection (GB ≥ 1 GiB, MB ≥
/// 1 MiB, KB ≥ 1 KiB, else bytes). Used in the Memory column and the
/// total-memory stats badge.
pub fn format_bytes(b: u64) -> String {
    if b >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", b as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if b >= 1024 * 1024 {
        format!("{} MB", b / (1024 * 1024))
    } else if b >= 1024 {
        format!("{} KB", b / 1024)
    } else {
        format!("{b} B")
    }
}

// ─── CPU / affinity formatters ───────────────────────────────────────────────

/// Color intensity for a CPU% cell — green at idle, yellow on busy, red on
/// hot. Same gradient the perf-band aggregate and the per-core matrix use,
/// so the table and the band speak the same color language.
pub fn cpu_percent_color(cpu: u16) -> egui::Color32 {
    match cpu {
        0..=10 => theme::TEXT_MUTED,
        11..=50 => theme::TEXT,
        51..=80 => theme::WARNING,
        _ => theme::ERROR,
    }
}

/// Decode a process affinity bitmask into a human-readable CPU-range list:
/// `0x000000ff → "CPUs: 0–7"`, `0x0000800f → "CPUs: 0–3, 15"`. Renders
/// `"(none)"` for an empty mask. Used as the affinity column's hover
/// tooltip so the hex is scannable and the decode is on demand.
pub fn decode_affinity_mask(mask: u64) -> String {
    if mask == 0 {
        return "(none)".to_string();
    }
    let mut groups: Vec<String> = Vec::new();
    let mut run_start: Option<u32> = None;
    let mut last_set: Option<u32> = None;
    for i in 0..64u32 {
        let bit_set = (mask >> i) & 1 == 1;
        if bit_set {
            if run_start.is_none() {
                run_start = Some(i);
            }
            last_set = Some(i);
        } else if let Some(start) = run_start {
            let end = last_set.unwrap_or(start);
            push_run(&mut groups, start, end);
            run_start = None;
        }
    }
    // Final run if the highest bits are set.
    if let Some(start) = run_start {
        let end = last_set.unwrap_or(start);
        push_run(&mut groups, start, end);
    }
    format!("CPUs: {}", groups.join(", "))
}

fn push_run(out: &mut Vec<String>, start: u32, end: u32) {
    if start == end {
        out.push(start.to_string());
    } else {
        // En-dash, not hyphen — Process Lasso uses the same and it reads
        // better as a range.
        out.push(format!("{start}–{end}"));
    }
}

/// Top-N cores by load, formatted as a multi-line tooltip body:
/// `"Core 4: 87%\nCore 8: 73%\n..."`. Used as the perf-band aggregate
/// CPU% tooltip so a busy aggregate has obvious provenance — which
/// cores are actually hot.
pub fn format_top_cores(percents: &[u8], n: usize) -> String {
    if percents.is_empty() {
        return "(per-core data not available yet)".to_string();
    }
    let mut pairs: Vec<(usize, u8)> = percents.iter().copied().enumerate().collect();
    pairs.sort_by_key(|(_, p)| std::cmp::Reverse(*p));
    pairs
        .into_iter()
        .take(n)
        .map(|(i, p)| format!("Core {i}: {p}%"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Map a raw Win32 priority class constant to the short label Task Manager
/// uses ("Normal", "High", "Realtime", …). Returns "—" for unknown values
/// so the table doesn't show a stale or garbled string.
pub fn priority_class_label(raw: u32) -> &'static str {
    match raw {
        0x0000_0040 => "Idle",
        0x0000_4000 => "BelowNormal",
        0x0000_0020 => "Normal",
        0x0000_8000 => "AboveNormal",
        0x0000_0080 => "High",
        0x0000_0100 => "Realtime",
        _ => "—",
    }
}

// ─── Profile + tooltip text ──────────────────────────────────────────────────

/// Format the tooltip shown when hovering the FrameSage system-tray icon.
/// Two-line layout: "FrameSage — <state>" + "Active: <profile> · Foreground:
/// <exe>". Reads the StatusSnapshot to decide what to show; degrades
/// gracefully when fields are absent ("Active: — / Foreground: —").
pub fn format_tray_tooltip(connected: bool, status: Option<&StatusSnapshot>) -> String {
    let state = match (connected, status.map(|s| s.paused)) {
        (false, _) => "disconnected",
        (true, Some(true)) => "paused",
        (true, _) => "running",
    };
    let active_profile = status
        .and_then(|s| s.active_profile.as_ref())
        .map(|p| p.id.0.as_str())
        .unwrap_or("—");
    let foreground = status
        .and_then(|s| s.foreground.as_ref())
        .map(|f| f.exe_name.as_str())
        .unwrap_or("—");
    format!("FrameSage — {state}\nActive: {active_profile}  ·  Foreground: {foreground}")
}

/// Convert a profile id slug like `"game-x3d"` to a display label `"Game X3D"`.
/// Policy.json stores ids in the form the user authored (kebab- or
/// snake_case so rules round-trip stably); the UI shows the prettier
/// version. Splits on `-` and `_`, upper-cases the first letter of each
/// token, and special-cases hardware acronyms (X3D / CPU / etc.) so they
/// stay shouty.
pub fn display_profile_id(raw: &str) -> String {
    raw.split(['-', '_'])
        .filter(|s| !s.is_empty())
        .map(|token| {
            // Acronyms / vendor jargon that should stay shouty.
            let upper = token.to_ascii_uppercase();
            if matches!(
                upper.as_str(),
                "X3D" | "CPU" | "GPU" | "RAM" | "IO" | "CCD" | "AMD" | "NV" | "DLSS"
            ) {
                return upper;
            }
            let mut chars = token.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ─── Misc text helpers ───────────────────────────────────────────────────────

/// Trim `s` to fit in a `max`-character status-bar echo, appending `…` when
/// the original ran longer. Operates on chars (not bytes) so non-ASCII
/// names don't slice mid-codepoint.
pub fn truncate_for_echo(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framesage_ipc::{ForegroundSnapshot, StatusSnapshot};

    #[test]
    fn profile_id_display_handles_common_cases() {
        assert_eq!(display_profile_id("perf"), "Perf");
        assert_eq!(display_profile_id("eco"), "Eco");
        assert_eq!(display_profile_id("game-x3d"), "Game X3D");
        assert_eq!(display_profile_id("low_power"), "Low Power");
        assert_eq!(display_profile_id("cpu-bound"), "CPU Bound");
        assert_eq!(display_profile_id(""), "");
    }

    #[test]
    fn decode_affinity_mask_collapses_contiguous_runs() {
        assert_eq!(decode_affinity_mask(0x0000_00ff), "CPUs: 0–7");
        assert_eq!(decode_affinity_mask(0x0000_800f), "CPUs: 0–3, 15");
        assert_eq!(decode_affinity_mask(0x0000_0001), "CPUs: 0");
        assert_eq!(decode_affinity_mask(0xffff_ffff), "CPUs: 0–31");
        // Singletons separated by gaps don't collapse.
        assert_eq!(decode_affinity_mask(0b1010_1010), "CPUs: 1, 3, 5, 7");
    }

    #[test]
    fn decode_affinity_mask_empty_renders_none() {
        assert_eq!(decode_affinity_mask(0), "(none)");
    }

    #[test]
    fn decode_affinity_mask_includes_high_bits() {
        // Bit 63 alone — last-run handling at the loop boundary.
        assert_eq!(decode_affinity_mask(1u64 << 63), "CPUs: 63");
        // Top byte set as a contiguous block.
        assert_eq!(decode_affinity_mask(0xff00_0000_0000_0000), "CPUs: 56–63");
    }

    #[test]
    fn format_top_cores_sorts_descending_and_caps() {
        let pct = vec![10, 80, 30, 95, 5, 50, 70, 20];
        let s = format_top_cores(&pct, 3);
        assert_eq!(s, "Core 3: 95%\nCore 1: 80%\nCore 6: 70%");
    }

    #[test]
    fn format_top_cores_handles_empty() {
        assert_eq!(
            format_top_cores(&[], 5),
            "(per-core data not available yet)"
        );
    }

    #[test]
    fn truncate_for_echo_short_string_unchanged() {
        assert_eq!(truncate_for_echo("hello", 40), "hello");
    }

    #[test]
    fn truncate_for_echo_long_string_gets_ellipsis() {
        let s = "a".repeat(100);
        let got = truncate_for_echo(&s, 10);
        assert_eq!(got.chars().count(), 10);
        assert!(got.ends_with('…'));
    }

    #[test]
    fn truncate_for_echo_respects_chars_not_bytes() {
        // Norwegian letters are multi-byte in UTF-8 but single-char.
        // Slicing by byte index would panic on a non-boundary; chars()
        // is the right primitive.
        let s = "æøåæøåæøå";
        let got = truncate_for_echo(s, 5);
        assert_eq!(got.chars().count(), 5);
    }

    fn fake_status(paused: bool, profile: Option<&str>, fg: Option<&str>) -> StatusSnapshot {
        let active_profile = profile.map(|id| {
            let mut p = framesage_core::Profile::new(id);
            p.description = format!("test profile {id}");
            p
        });
        let foreground = fg.map(|exe| ForegroundSnapshot {
            pid: 42,
            exe_name: exe.to_string(),
            path: String::new(),
            title: String::new(),
        });
        StatusSnapshot {
            paused,
            policy: framesage_core::Policy::default(),
            foreground,
            active_profile,
            manual_override: None,
        }
    }

    #[test]
    fn format_tray_tooltip_disconnected_state() {
        let s = format_tray_tooltip(false, None);
        assert!(s.contains("disconnected"));
        assert!(s.contains("Active: —"));
        assert!(s.contains("Foreground: —"));
    }

    #[test]
    fn format_tray_tooltip_paused_with_profile() {
        let snap = fake_status(true, Some("game-x3d"), Some("bf6.exe"));
        let s = format_tray_tooltip(true, Some(&snap));
        assert!(s.contains("paused"));
        assert!(s.contains("game-x3d"));
        assert!(s.contains("bf6.exe"));
    }

    #[test]
    fn format_tray_tooltip_running_no_foreground() {
        let snap = fake_status(false, None, None);
        let s = format_tray_tooltip(true, Some(&snap));
        assert!(s.contains("running"));
        assert!(s.contains("Active: —"));
        assert!(s.contains("Foreground: —"));
    }
}
