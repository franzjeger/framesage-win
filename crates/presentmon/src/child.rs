//! Windows child-process driver — the ONLY module here that touches a
//! real process. Everything above it (parser, aggregator, spawn
//! policy) is host-independent and tested; this layer stays thin per
//! `docs/syscall-seam-pattern.md` ("only the thin real layer needs the
//! Windows runtime batch").
//!
//! Contract:
//! * Every spawn goes through [`crate::SpawnPolicy::decide`] — there
//!   is no other spawn path (PRE-L-004).
//! * The child is launched with `--terminate_on_proc_exit` so it
//!   follows the game's lifetime, and `--output_stdout` so no temp
//!   files are created.
//! * Stdout lines flow through [`crate::CsvParser`] →
//!   [`crate::FrameAggregator`]; each completed 1 Hz bucket is handed
//!   to the caller's sink.

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(windows)]
mod windows_impl {
    use std::io::{BufRead, BufReader};
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::time::Instant;

    use anyhow::{Context, Result};

    use crate::{CsvParser, FrameAggregator, FrameStats};

    /// A running PresentMon child + the streaming pipeline.
    pub struct PresentMonChild {
        child: Child,
        started: Instant,
    }

    impl PresentMonChild {
        /// Spawn `PresentMon.exe` for one target PID. The caller must
        /// have consulted the spawn policy first.
        pub fn spawn(presentmon_exe: &Path, target_pid: u32) -> Result<Self> {
            let child = Command::new(presentmon_exe)
                .args([
                    "--process_id",
                    &target_pid.to_string(),
                    "--output_stdout",
                    "--terminate_on_proc_exit",
                    "--stop_existing_session",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .spawn()
                .with_context(|| format!("spawn PresentMon at {}", presentmon_exe.display()))?;
            Ok(Self {
                child,
                started: Instant::now(),
            })
        }

        /// Blocking drain loop: reads stdout to EOF, pushing each 1 Hz
        /// bucket into `sink`. Returns whether the child exited
        /// cleanly. Run on a dedicated blocking thread.
        pub fn drain(mut self, mut sink: impl FnMut(FrameStats)) -> Result<bool> {
            let stdout = self
                .child
                .stdout
                .take()
                .context("PresentMon child has no stdout pipe")?;
            let mut parser = CsvParser::new();
            let mut agg = FrameAggregator::new();
            for line in BufReader::new(stdout).lines() {
                let line = line?;
                if let Some(row) = parser.feed_line(&line)? {
                    let at_ms = self.started.elapsed().as_millis() as u64;
                    if let Some(stats) = agg.push(at_ms, row.frame_time_us) {
                        sink(stats);
                    }
                }
            }
            if let Some(stats) = agg.flush() {
                sink(stats);
            }
            let status = self.child.wait().context("wait for PresentMon child")?;
            Ok(status.success())
        }

        /// Best-effort stop at session end (orderly, not a crash).
        pub fn stop(mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
