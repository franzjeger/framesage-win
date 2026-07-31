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
    use std::process::{Child, ChildStdout, Command, Stdio};
    use std::time::Instant;

    use anyhow::{Context, Result};

    use crate::{CsvParser, FrameAggregator, FrameStats};

    /// A running PresentMon child. Deliberately holds only the process
    /// handle — the stdout pipe is moved out into a separate
    /// [`FramePipe`] via [`take_frame_pipe`](Self::take_frame_pipe) so
    /// the drain can run on a blocking thread while the caller keeps
    /// this handle to [`stop`](Self::stop) the process out-of-band.
    ///
    /// Killing the process closes its stdout, which unblocks a
    /// `FramePipe::drain` parked on a blocking read — that's how a
    /// session-end / foreground-game-switch detach terminates the child
    /// promptly instead of leaking a `PresentMon.exe` until the game
    /// itself exits (the PRE-L-004 footprint concern).
    pub struct PresentMonChild {
        child: Child,
        started: Instant,
        pipe_taken: bool,
    }

    /// The stdout half of a running child, drained on a blocking thread.
    /// Separated from the process handle so the two can live on
    /// different threads: the drain blocks on reads here while the
    /// owner of the [`PresentMonChild`] can kill the process to stop it.
    pub struct FramePipe {
        stdout: ChildStdout,
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
                pipe_taken: false,
            })
        }

        /// Move the stdout pipe out for draining on a blocking thread,
        /// leaving `self` holding the killable process handle. Returns
        /// `None` if the pipe was already taken or the child has no
        /// stdout.
        pub fn take_frame_pipe(&mut self) -> Option<FramePipe> {
            if self.pipe_taken {
                return None;
            }
            let stdout = self.child.stdout.take()?;
            self.pipe_taken = true;
            Some(FramePipe {
                stdout,
                started: self.started,
            })
        }

        /// Reap the child and report whether it exited cleanly. Call
        /// after the drain thread has finished (EOF) to learn whether
        /// the child crashed (budget accounting) vs. exited normally.
        pub fn wait(mut self) -> Result<bool> {
            let status = self.child.wait().context("wait for PresentMon child")?;
            Ok(status.success())
        }

        /// Stop the process now (kill + reap). Closing stdout via the
        /// kill unblocks any `FramePipe::drain` parked on a read, so the
        /// caller can then join the drain thread. A no-op-safe kill if
        /// the child already exited on its own.
        pub fn stop(mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl FramePipe {
        /// Blocking drain loop: reads stdout to EOF, pushing each 1 Hz
        /// bucket into `sink`. Ends when the child closes stdout —
        /// either by exiting on its own (`--terminate_on_proc_exit`) or
        /// because the owner killed it via [`PresentMonChild::stop`].
        /// Run on a dedicated blocking thread.
        pub fn drain(self, mut sink: impl FnMut(FrameStats)) -> Result<()> {
            let mut parser = CsvParser::new();
            let mut agg = FrameAggregator::new();
            for line in BufReader::new(self.stdout).lines() {
                let line = line?;
                if let Some(row) = parser.feed_line(&line)? {
                    let at_ms = self.started.elapsed().as_millis() as u64;
                    if let Some(stats) = agg.push(at_ms, row.frame_time_us, row.dropped) {
                        sink(stats);
                    }
                }
            }
            if let Some(stats) = agg.flush() {
                sink(stats);
            }
            Ok(())
        }
    }
}
