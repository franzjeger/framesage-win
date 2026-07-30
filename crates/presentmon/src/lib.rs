//! framesage-presentmon — v0.7.1 Group B PresentMon adapter scaffold
//! (issue #111, architecture §2.2).
//!
//! Layered so everything decision-shaped is host-independent and
//! tested, per `docs/syscall-seam-pattern.md`:
//!
//! * [`parser`] — header-driven CSV parsing of PresentMon's stdout
//!   (column-order-independent, malformed lines skipped not fatal).
//! * [`aggregate`] — 1 Hz downsampling into `frame_sample`-shaped
//!   stats (count, p50/p99 µs) for the recorder.
//! * [`spawn_policy`] — the PRE-L-004 state machine: ≥30 s between
//!   spawns, reuse a running child when the target name matches, and
//!   a 3-crash restart budget per session.
//! * [`child`] — the thin `cfg(windows)` subprocess driver; the only
//!   module that creates a real process, and every spawn goes through
//!   the policy.
//!
//! Not yet wired: the service-side integration that connects a
//! recording session (framesage-recorder drain worker) to a child —
//! that lands with the Windows runtime batch, because it can only be
//! meaningfully verified against a real PresentMon.exe. License
//! compliance: PresentMon is Intel's, MIT-licensed — bundled text in
//! THIRD_PARTY_LICENSES.md at the repo root; the installer must ship
//! it alongside PresentMon.exe.

pub mod aggregate;
pub mod child;
pub mod parser;
pub mod spawn_policy;

pub use aggregate::{FrameAggregator, FrameStats};
pub use parser::{CsvParser, ParseError, PresentRow};
pub use spawn_policy::{SpawnDecision, SpawnPolicy, MAX_RESTARTS_PER_SESSION, MIN_SPAWN_INTERVAL};

#[cfg(test)]
mod pipeline_tests {
    use super::*;

    /// End-to-end off-Windows: CSV lines → parser → aggregator →
    /// frame stats with the shape the recorder's `frame_sample`
    /// expects.
    #[test]
    fn csv_stream_aggregates_into_one_hz_frame_stats() {
        let mut parser = CsvParser::new();
        let mut agg = FrameAggregator::new();
        let mut out: Vec<FrameStats> = Vec::new();

        parser
            .feed_line("Application,ProcessID,msBetweenPresents")
            .unwrap();
        // Two seconds of 60 fps.
        let mut at_ms = 0u64;
        for _ in 0..120 {
            let row = parser
                .feed_line("game.exe,42,16.667")
                .unwrap()
                .expect("row");
            if let Some(stats) = agg.push(at_ms, row.frame_time_us) {
                out.push(stats);
            }
            at_ms += 16;
        }
        if let Some(stats) = agg.flush() {
            out.push(stats);
        }

        assert_eq!(out.len(), 2, "two 1 Hz buckets for ~1.9 s of frames");
        assert!(out.iter().all(|s| s.frame_time_us_p50 == 16_667));
        assert!(out.iter().all(|s| s.frame_count >= 56));
    }
}
