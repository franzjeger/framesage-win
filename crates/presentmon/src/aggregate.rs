//! 1 Hz downsampling of raw presents into `frame_sample`-shaped stats
//! (architecture §2.3 `frame_sample`: count + p50/p99 in µs).
//!
//! Time is injected as milliseconds-since-session-start so the
//! aggregator is clockless and deterministic in tests; the child
//! driver stamps rows with its own monotonic clock.

/// One aggregated sample — maps 1:1 onto the recorder's
/// `SessionEvent::FrameSample` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameStats {
    /// Start of the 1-second bucket, ms since session start.
    pub at_ms: u64,
    pub frame_count: u32,
    pub frame_time_us_p50: u64,
    pub frame_time_us_p99: u64,
    /// Presents flagged `Dropped` by PresentMon within this 1 s bucket
    /// (composed away / never displayed). 0 when the source omits the
    /// column — honest absence, not a claim of zero drops.
    pub frames_dropped: u32,
}

/// Buckets frame times into whole seconds and emits one [`FrameStats`]
/// per completed bucket.
#[derive(Debug, Default)]
pub struct FrameAggregator {
    bucket_start_ms: Option<u64>,
    frame_times_us: Vec<u64>,
    frames_dropped: u32,
}

impl FrameAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push one present. `dropped` is PresentMon's per-present Dropped
    /// flag. Returns a completed bucket's stats when `at_ms` crosses
    /// into a new second.
    pub fn push(&mut self, at_ms: u64, frame_time_us: u64, dropped: bool) -> Option<FrameStats> {
        let bucket = (at_ms / 1000) * 1000;
        let emitted = match self.bucket_start_ms {
            Some(current) if bucket != current => self.emit(),
            None => {
                self.bucket_start_ms = Some(bucket);
                None
            }
            _ => None,
        };
        if self.bucket_start_ms != Some(bucket) {
            self.bucket_start_ms = Some(bucket);
        }
        self.frame_times_us.push(frame_time_us);
        if dropped {
            self.frames_dropped = self.frames_dropped.saturating_add(1);
        }
        emitted
    }

    /// Flush the in-progress bucket (session end / child exit).
    pub fn flush(&mut self) -> Option<FrameStats> {
        self.emit()
    }

    fn emit(&mut self) -> Option<FrameStats> {
        let at_ms = self.bucket_start_ms?;
        if self.frame_times_us.is_empty() {
            return None;
        }
        let mut times = std::mem::take(&mut self.frame_times_us);
        times.sort_unstable();
        let p50 = times[(times.len() - 1) / 2];
        let p99_rank = ((0.99 * times.len() as f64).ceil() as usize).clamp(1, times.len());
        let p99 = times[p99_rank - 1];
        let frames_dropped = std::mem::take(&mut self.frames_dropped);
        self.bucket_start_ms = None;
        Some(FrameStats {
            at_ms,
            frame_count: times.len() as u32,
            frame_time_us_p50: p50,
            frame_time_us_p99: p99,
            frames_dropped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_bucket_per_second_with_percentiles() {
        let mut agg = FrameAggregator::new();
        // 60 frames in second 0: 59 at 16ms + 1 spike at 40ms.
        for i in 0..59 {
            assert_eq!(agg.push(i * 16, 16_000, false), None);
        }
        assert_eq!(agg.push(950, 40_000, false), None);
        // First frame of second 1 closes the bucket.
        let stats = agg.push(1_005, 16_000, false).expect("bucket emitted");
        assert_eq!(stats.at_ms, 0);
        assert_eq!(stats.frame_count, 60);
        assert_eq!(stats.frame_time_us_p50, 16_000);
        assert_eq!(stats.frame_time_us_p99, 40_000, "p99 catches the spike");
    }

    #[test]
    fn dropped_presents_are_counted_per_bucket_then_reset() {
        let mut agg = FrameAggregator::new();
        // Second 0: 3 presents, 2 dropped.
        agg.push(0, 16_000, true);
        agg.push(300, 16_000, false);
        agg.push(600, 16_000, true);
        // Second 1's first present closes bucket 0 and starts bucket 1
        // (which has no drops).
        let bucket0 = agg.push(1_000, 16_000, false).expect("bucket 0 emitted");
        assert_eq!(bucket0.frames_dropped, 2, "both drops land in bucket 0");
        let bucket1 = agg.flush().expect("bucket 1 emitted");
        assert_eq!(
            bucket1.frames_dropped, 0,
            "the drop counter resets between buckets"
        );
    }

    #[test]
    fn flush_emits_the_partial_final_bucket() {
        let mut agg = FrameAggregator::new();
        agg.push(0, 10_000, false);
        agg.push(100, 12_000, false);
        let stats = agg.flush().expect("partial bucket");
        assert_eq!(stats.frame_count, 2);
        assert_eq!(agg.flush(), None, "flush is idempotent");
    }

    #[test]
    fn empty_seconds_emit_nothing() {
        let mut agg = FrameAggregator::new();
        agg.push(0, 16_000, false);
        // Jump straight to second 5 — the intermediate silence emits
        // only the one completed bucket, never empty buckets.
        let stats = agg.push(5_000, 16_000, false).unwrap();
        assert_eq!(stats.frame_count, 1);
        assert_eq!(agg.flush().unwrap().at_ms, 5_000);
    }
}
