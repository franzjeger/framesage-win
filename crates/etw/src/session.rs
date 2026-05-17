//! ETW session lifecycle — `StartTraceW`, `OpenTraceW`, `ProcessTrace`,
//! drop-rate query loop, clean shutdown, stale-session cleanup.
//!
//! Lifted from `crates/spike-etw/src/main.rs` (Phase 1 — validated on
//! Win11 26200) per `spike/group-a-week-2-plan.md` §3.2 + §4 Day 2.
//! Day 2 ships the lifecycle ONLY — no event parsing, no histograms,
//! no ring buffer. The callback bumps a single `events_seen` atomic
//! to prove ETW is actually delivering events; everything else
//! ("which provider", "which opcode", "what PID") is week 3+.
//!
//! Day 3 will refactor this module to introduce the `EtwSysCalls`
//! trait (per §3.4) + the generic `EtwSession<S>` shape + the
//! `EtwSubsystem<S>` return type. Day 2 ships the concrete
//! windows-rs-direct version. The Day 3 refactor is mechanical (wrap
//! each windows-rs call with a trait method) and doesn't change the
//! lifecycle semantics.

use std::sync::Arc;

#[cfg(windows)]
mod windows_impl {
    use std::mem::{size_of, zeroed};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread::JoinHandle;

    use anyhow::{bail, Result};

    use windows::core::GUID;
    use windows::Win32::Foundation::{
        ERROR_ALREADY_EXISTS, ERROR_SUCCESS, ERROR_WMI_INSTANCE_NOT_FOUND,
    };
    use windows::Win32::System::Diagnostics::Etw::{
        ControlTraceW, OpenTraceW, ProcessTrace, StartTraceW, CONTROLTRACE_HANDLE, EVENT_RECORD,
        EVENT_TRACE_CONTROL_QUERY, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW,
        EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, EVENT_TRACE_SYSTEM_LOGGER_MODE,
        PROCESSTRACE_HANDLE, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME,
        WNODE_FLAG_TRACED_GUID,
    };

    use super::{SessionOptions, SessionStats};

    /// Unique session GUID. Distinct from the spike's GUID
    /// (`0x4F8B_1A60_9E2D_4F3F_88C2_5B7E_1D6F_92A4`) so production +
    /// spike binaries can coexist on the same machine during the
    /// transition. Generated once for production; replace at v1.0
    /// stable release.
    pub(super) const SESSION_GUID: GUID =
        GUID::from_u128(0x7A2E_6C18_4F30_4D9B_A6E1_8B5C_2D71_F0A3);

    /// `EVENT_TRACE_PROPERTIES` is variable-length: the struct is
    /// followed in memory by the logger name (wide string) and
    /// optionally a log-file name. Flat `repr(C)` keeps the layout
    /// stable and offsets easy to compute.
    #[repr(C)]
    pub(super) struct EtwSessionPropertiesBuffer {
        base: EVENT_TRACE_PROPERTIES,
        name: [u16; 128],
        logfile: [u16; 128],
    }

    impl EtwSessionPropertiesBuffer {
        /// Build a properties buffer ready to pass to `StartTraceW`.
        pub(super) fn new(opts: &SessionOptions) -> Self {
            // SAFETY: zero-initialisation gives a valid (empty)
            // EVENT_TRACE_PROPERTIES.
            let mut buf: Self = unsafe { zeroed() };

            buf.base.Wnode.BufferSize = size_of::<Self>() as u32;
            buf.base.Wnode.Guid = SESSION_GUID;
            buf.base.Wnode.ClientContext = 1; // QPC timestamps
            buf.base.Wnode.Flags = WNODE_FLAG_TRACED_GUID;

            // Real-time + system-logger mode → private system session
            // (multiple coexist on Win10+; NOT the NT-Kernel-Logger
            // global singleton).
            buf.base.LogFileMode = EVENT_TRACE_REAL_TIME_MODE | EVENT_TRACE_SYSTEM_LOGGER_MODE;

            buf.base.EnableFlags =
                windows::Win32::System::Diagnostics::Etw::EVENT_TRACE_FLAG(opts.enable_flags);

            buf.base.BufferSize = opts.buffer_size_kb;
            buf.base.MinimumBuffers = opts.minimum_buffers;
            buf.base.MaximumBuffers = opts.maximum_buffers;
            buf.base.FlushTimer = 1;

            let name_utf16: Vec<u16> = opts.session_name.encode_utf16().collect();
            let copy_len = name_utf16.len().min(buf.name.len() - 1);
            buf.name[..copy_len].copy_from_slice(&name_utf16[..copy_len]);

            buf.base.LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;
            buf.base.LogFileNameOffset =
                buf.base.LoggerNameOffset + (size_of::<[u16; 128]>() as u32);

            buf
        }

        pub(super) fn as_mut_ptr(&mut self) -> *mut EVENT_TRACE_PROPERTIES {
            // SAFETY: repr(C) makes `base` offset-0; pointer valid for
            // the lifetime of `self`.
            &mut self.base as *mut _
        }
    }

    /// Shared state between the consumer thread (callback writer) and
    /// the EtwSession holder (reader). Only the `events_seen` counter
    /// lives here for Day 2; week 3+ adds the ring buffer + per-PID
    /// attribution state.
    #[derive(Debug)]
    pub(super) struct SessionState {
        pub events_seen: AtomicU64,
    }

    /// Day 2 minimal session handle. Day 3 refactors to
    /// `EtwSession<S: EtwSysCalls = RealEtwSysCalls>` per plan §3.2.
    #[derive(Debug)]
    pub struct EtwSession {
        handle: CONTROLTRACE_HANDLE,
        session_name: String,
        state: Arc<SessionState>,
        /// `None` only between `into_supervisable_parts()` (Day 3+) and
        /// drop. Day 2 always has `Some`.
        consumer_join: Option<JoinHandle<()>>,
    }

    impl EtwSession {
        /// Start the session: cleanup any stale prior instance, call
        /// `StartTraceW`, open + spawn the consumer thread.
        pub fn start(opts: SessionOptions) -> Result<Self> {
            cleanup_stale_session(&opts.session_name)?;

            let mut props = EtwSessionPropertiesBuffer::new(&opts);
            let name_wide: Vec<u16> = opts
                .session_name
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let mut handle: CONTROLTRACE_HANDLE = CONTROLTRACE_HANDLE::default();

            // SAFETY: name_wide is NUL-terminated UTF-16. props is a
            // valid EVENT_TRACE_PROPERTIES with name/logfile offsets
            // pointing into the trailing buffer. `handle` is owned
            // locally; `&mut handle` gives exclusive access for the
            // OUT-parameter write.
            let rc = unsafe {
                StartTraceW(
                    &mut handle,
                    windows::core::PCWSTR(name_wide.as_ptr()),
                    props.as_mut_ptr(),
                )
            };
            if rc != ERROR_SUCCESS {
                if rc == ERROR_ALREADY_EXISTS {
                    bail!(
                        "StartTraceW returned ERROR_ALREADY_EXISTS — \
                         cleanup_stale_session should have removed any \
                         prior '{}' session. Manual fix: `logman stop {} -ets`.",
                        opts.session_name,
                        opts.session_name
                    );
                }
                let code = rc.0;
                bail!(
                    "StartTraceW failed: Win32 error {code} (0x{code:08x}). \
                     Common causes: not elevated (need admin token + \
                     SeSystemProfilePrivilege), or a security product blocking \
                     ETW session creation."
                );
            }

            let state = Arc::new(SessionState {
                events_seen: AtomicU64::new(0),
            });
            let consumer_state = Arc::clone(&state);
            let consumer_name = opts.session_name.clone();
            let consumer_join = std::thread::Builder::new()
                .name("etw-consumer".into())
                .spawn(move || {
                    // Errors from the consumer are logged at WARN.
                    // Day 3+ surfaces them via the supervisor's
                    // ConsumerExitReason channel.
                    if let Err(e) = run_consumer(&consumer_name, consumer_state) {
                        tracing::warn!(error = %e, "ETW consumer thread exited with error");
                    }
                })
                .map_err(|e| anyhow::anyhow!("failed to spawn etw-consumer thread: {e}"))?;

            tracing::info!(session = %opts.session_name, "ETW session started");

            Ok(EtwSession {
                handle,
                session_name: opts.session_name,
                state,
                consumer_join: Some(consumer_join),
            })
        }

        /// Orderly shutdown: `ControlTraceW(STOP)` → consumer thread
        /// observes ERROR_CANCELLED from `ProcessTrace` and returns →
        /// join → verify session is gone via QUERY-must-fail.
        pub fn stop(mut self) -> Result<()> {
            stop_session(&self.session_name)?;

            if let Some(handle) = self.consumer_join.take() {
                // Errors joining the consumer thread mean the OS
                // killed it or it panicked — best-effort log; the
                // session itself is already stopped.
                if let Err(panic_payload) = handle.join() {
                    tracing::warn!(
                        ?panic_payload,
                        "etw-consumer thread panicked during shutdown"
                    );
                }
            }

            verify_session_gone(&self.session_name)?;
            tracing::info!(session = %self.session_name, "ETW session stopped cleanly");
            Ok(())
        }

        /// Query live session stats: events lost, real-time buffers
        /// lost, buffers written, plus the callback-side
        /// `events_seen` counter we maintain ourselves.
        pub fn query_stats(&self) -> Result<SessionStats> {
            let q = query_session_stats(&self.session_name)?;
            Ok(SessionStats {
                events_lost: q.events_lost,
                real_time_buffers_lost: q.real_time_buffers_lost,
                buffers_written: q.buffers_written,
                events_seen: self.state.events_seen.load(Ordering::Relaxed),
            })
        }

        /// Read-only handle accessor for the (rare) caller that needs
        /// the raw CONTROLTRACE_HANDLE. Day 3 will replace with the
        /// `SessionShutdownHandle` pattern (per §3.2) that owns the
        /// teardown surface without exposing the handle directly.
        pub fn raw_handle(&self) -> CONTROLTRACE_HANDLE {
            self.handle
        }
    }

    /// Best-effort teardown for a session left behind by a crashed
    /// previous run. Per architecture §2.1 "Survives service restarts":
    /// startup invokes this; a clean run with no prior state gets
    /// `ERROR_WMI_INSTANCE_NOT_FOUND` and proceeds without complaint.
    fn cleanup_stale_session(session_name: &str) -> Result<()> {
        let mut props = EtwSessionPropertiesBuffer::new(&SessionOptions {
            session_name: session_name.to_string(),
            ..Default::default()
        });
        let name_wide: Vec<u16> = session_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: same shape as stop_session — passing 0-handle is
        // valid when the name is set; ETW looks up by name.
        let rc = unsafe {
            ControlTraceW(
                CONTROLTRACE_HANDLE::default(),
                windows::core::PCWSTR(name_wide.as_ptr()),
                props.as_mut_ptr(),
                EVENT_TRACE_CONTROL_STOP,
            )
        };
        match rc {
            r if r == ERROR_SUCCESS => {
                tracing::info!(
                    session = %session_name,
                    "cleaned up stale ETW session from prior run"
                );
            }
            r if r == ERROR_WMI_INSTANCE_NOT_FOUND => {
                // Expected — no prior session.
            }
            other => {
                tracing::warn!(
                    win32_error = other.0,
                    "stale-session cleanup returned unexpected status; StartTraceW will surface the real error"
                );
            }
        }
        Ok(())
    }

    fn stop_session(session_name: &str) -> Result<()> {
        let mut props = EtwSessionPropertiesBuffer::new(&SessionOptions {
            session_name: session_name.to_string(),
            ..Default::default()
        });
        let name_wide: Vec<u16> = session_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: documented call; 0-handle valid when name is set.
        let rc = unsafe {
            ControlTraceW(
                CONTROLTRACE_HANDLE::default(),
                windows::core::PCWSTR(name_wide.as_ptr()),
                props.as_mut_ptr(),
                EVENT_TRACE_CONTROL_STOP,
            )
        };
        if rc != ERROR_SUCCESS && rc != ERROR_WMI_INSTANCE_NOT_FOUND {
            bail!("ControlTraceW(STOP) failed: {} (0x{:08x})", rc.0, rc.0);
        }
        Ok(())
    }

    fn verify_session_gone(session_name: &str) -> Result<()> {
        match query_session_stats(session_name) {
            Err(_) => Ok(()),
            Ok(_) => bail!(
                "session '{}' is still registered after STOP — \
                 architecture §2.1 'survives service restarts' invariant violated; \
                 a future Start will hit ERROR_ALREADY_EXISTS and cleanup_stale_session \
                 will be the only recovery path",
                session_name
            ),
        }
    }

    /// Internal shape — exposed as `SessionStats` via `query_stats()`
    /// after combining with the callback-side `events_seen` counter.
    struct InternalQueryResult {
        events_lost: u32,
        real_time_buffers_lost: u32,
        buffers_written: u32,
    }

    fn query_session_stats(session_name: &str) -> Result<InternalQueryResult> {
        let mut props = EtwSessionPropertiesBuffer::new(&SessionOptions {
            session_name: session_name.to_string(),
            ..Default::default()
        });
        let name_wide: Vec<u16> = session_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: same shape. After QUERY, props.base fields reflect
        // session-since-start totals.
        let rc = unsafe {
            ControlTraceW(
                CONTROLTRACE_HANDLE::default(),
                windows::core::PCWSTR(name_wide.as_ptr()),
                props.as_mut_ptr(),
                EVENT_TRACE_CONTROL_QUERY,
            )
        };
        if rc != ERROR_SUCCESS {
            bail!("ControlTraceW(QUERY) failed: {} (0x{:08x})", rc.0, rc.0);
        }
        // Reach into the inner EVENT_TRACE_PROPERTIES; the QUERY API
        // writes the fields back into the same buffer.
        let base = unsafe { &*(props.as_mut_ptr() as *const EVENT_TRACE_PROPERTIES) };
        Ok(InternalQueryResult {
            events_lost: base.EventsLost,
            real_time_buffers_lost: base.RealTimeBuffersLost,
            buffers_written: base.BuffersWritten,
        })
    }

    /// Spawned as `etw-consumer` thread. Opens the trace by name +
    /// blocks in ProcessTrace until ControlTraceW(STOP) fires from
    /// the EtwSession owner. Returns ERROR_CANCELLED → treat as clean.
    fn run_consumer(session_name: &str, state: Arc<SessionState>) -> Result<()> {
        let name_wide: Vec<u16> = session_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut logfile: EVENT_TRACE_LOGFILEW = unsafe { zeroed() };
        logfile.LoggerName = windows::core::PWSTR(name_wide.as_ptr() as *mut u16);
        logfile.Anonymous1.ProcessTraceMode =
            PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
        logfile.Anonymous2.EventRecordCallback = Some(event_record_callback);
        // UserContext = raw Arc::as_ptr. The Arc clone `state` is held
        // by THIS function for the duration of ProcessTrace, which
        // keeps the underlying SessionState allocation alive across
        // every callback invocation.
        logfile.Context = Arc::as_ptr(&state) as *mut std::ffi::c_void;

        // SAFETY: logfile.LoggerName is NUL-terminated UTF-16 and
        // lives for the duration of OpenTraceW. PROCESS_TRACE_MODE_REAL_TIME
        // → OpenTraceW resolves the live session by name.
        let handle = unsafe { OpenTraceW(&mut logfile) };
        if handle.Value == u64::MAX {
            bail!("OpenTraceW failed (invalid handle returned — session may have been stopped between StartTraceW and OpenTraceW)");
        }

        let handles = [PROCESSTRACE_HANDLE {
            Value: handle.Value,
        }];
        // SAFETY: handles is a valid array of one handle. ProcessTrace
        // blocks until session is stopped via ControlTraceW(STOP); the
        // `state` Arc keeps SessionState live until this returns.
        let rc = unsafe { ProcessTrace(&handles, None, None) };

        // ERROR_CANCELLED (1223 / 0x4C7) is the clean-shutdown path.
        if rc != ERROR_SUCCESS && rc.0 != 1223 {
            let code = rc.0;
            bail!("ProcessTrace returned unexpected error: {code} (0x{code:08x})");
        }

        // Keep `state` alive until ProcessTrace returns; explicit drop
        // here documents the invariant rather than relying on
        // last-use lifetime extension.
        drop(state);
        Ok(())
    }

    /// ETW callback — runs on the consumer thread inside ProcessTrace.
    /// Day 2 only counts events; week 3+ parses opcodes and pushes
    /// compact KernelEvent structs to the SPSC ring buffer.
    ///
    /// Per architecture §2.1: this callback is not allowed to block.
    /// One atomic increment + return.
    unsafe extern "system" fn event_record_callback(event_record: *mut EVENT_RECORD) {
        if event_record.is_null() {
            return;
        }
        // SAFETY: ETW guarantees the EVENT_RECORD pointer is valid
        // for the duration of this callback. We read fields only;
        // no mutation, no escape of the pointer.
        let er = unsafe { &*event_record };
        let ctx = er.UserContext as *const SessionState;
        if ctx.is_null() {
            return;
        }
        // SAFETY: ctx was set in run_consumer to Arc::as_ptr(&state).
        // The Arc is kept alive by run_consumer for the duration of
        // ProcessTrace (which is what calls this callback), so the
        // SessionState allocation is valid here.
        let state = unsafe { &*ctx };
        state.events_seen.fetch_add(1, Ordering::Relaxed);
    }
}

// ─── Cross-platform public surface ───────────────────────────────────────────

/// Configuration for starting an ETW session.
#[derive(Debug, Clone)]
pub struct SessionOptions {
    pub session_name: String,
    /// OR'd combination of `EVENT_TRACE_FLAG_*` constants. Day 2
    /// defaults to the spike's tested set: CSwitch, DPC, Interrupt,
    /// DiskIo, MemoryHardFaults. Week 3+ parsers narrow or widen as
    /// the schema research dictates.
    pub enable_flags: u32,
    pub buffer_size_kb: u32,
    pub minimum_buffers: u32,
    pub maximum_buffers: u32,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            session_name: "FramesageEtw".into(),
            // Hard-coded values match the spike's
            // EVENT_TRACE_FLAG_* constants since we can't depend on
            // windows-rs on non-Windows for the Default impl. The
            // numeric values are stable kernel ABI:
            // EVENT_TRACE_FLAG_CSWITCH            = 0x00000010
            // EVENT_TRACE_FLAG_DPC                = 0x00000020
            // EVENT_TRACE_FLAG_INTERRUPT          = 0x00000040
            // EVENT_TRACE_FLAG_DISK_IO            = 0x00000100
            // EVENT_TRACE_FLAG_MEMORY_HARD_FAULTS = 0x00002000
            enable_flags: 0x0000_0010 | 0x0000_0020 | 0x0000_0040 | 0x0000_0100 | 0x0000_2000,
            buffer_size_kb: 64,
            minimum_buffers: 20,
            maximum_buffers: 100,
        }
    }
}

/// Live session statistics. `events_lost` + `real_time_buffers_lost`
/// come from `ControlTraceW(QUERY)`; `events_seen` is the
/// callback-side counter the consumer thread maintains.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionStats {
    pub events_lost: u32,
    pub real_time_buffers_lost: u32,
    pub buffers_written: u32,
    pub events_seen: u64,
}

#[cfg(windows)]
pub use windows_impl::EtwSession;

#[cfg(not(windows))]
mod stub {
    use super::{SessionOptions, SessionStats};
    use anyhow::{bail, Result};

    /// Non-Windows stub. Lets `cargo check --workspace` succeed on
    /// Linux (CI cross-check job) while keeping the public API shape
    /// platform-agnostic for downstream callers.
    #[derive(Debug)]
    pub struct EtwSession {
        _private: (),
    }

    impl EtwSession {
        pub fn start(_opts: SessionOptions) -> Result<Self> {
            bail!("framesage-etw session requires Windows (closed-loop ETW consumer)")
        }
        pub fn stop(self) -> Result<()> {
            Ok(())
        }
        pub fn query_stats(&self) -> Result<SessionStats> {
            bail!("framesage-etw session requires Windows")
        }
    }
}

#[cfg(not(windows))]
pub use stub::EtwSession;

// Keep the Arc import live across cfg arms — Day 3's refactor will
// use it in the cross-platform path when SessionShutdownHandle lands.
#[allow(dead_code)]
type _ArcAlias = Arc<()>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_options_default_matches_spike_tested_set() {
        let opts = SessionOptions::default();
        // Spike validated this enable_flags combination on Win11 26200
        // (Phase 1 spike report). Locking the value in so a future
        // edit to Default can't silently change the trace set without
        // a corresponding plan-vs-architecture update.
        let expected_flags = 0x0000_0010 // CSwitch
            | 0x0000_0020 // DPC
            | 0x0000_0040 // Interrupt
            | 0x0000_0100 // DiskIo
            | 0x0000_2000; // MemoryHardFaults
        assert_eq!(opts.enable_flags, expected_flags);
        assert_eq!(opts.session_name, "FramesageEtw");
        assert_eq!(opts.buffer_size_kb, 64);
        assert_eq!(opts.minimum_buffers, 20);
        assert_eq!(opts.maximum_buffers, 100);
    }

    #[test]
    fn session_stats_default_is_zeroed() {
        let s = SessionStats::default();
        assert_eq!(s.events_lost, 0);
        assert_eq!(s.real_time_buffers_lost, 0);
        assert_eq!(s.buffers_written, 0);
        assert_eq!(s.events_seen, 0);
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_start_bails_with_platform_message() {
        // Non-Windows hosts can't run a real session; the stub bails
        // with a clear message. This test runs on Linux (CI cross-check)
        // and macOS (developer hosts during the v0.7 build-out).
        let opts = SessionOptions::default();
        let err = EtwSession::start(opts).expect_err("stub must bail on non-Windows");
        assert!(err.to_string().contains("Windows"), "{err}");
    }

    // Real-Windows tests live behind #[ignore] per the end-of-week
    // batch strategy (2026-05-17). They COMPILE on Mac via the
    // x86_64-pc-windows-gnu cross-target check; they run on the F:
    // drive during the end-of-week batch.
    #[cfg(windows)]
    #[test]
    #[ignore = "deferred to end-of-week Windows runtime batch (real ETW session start/stop)"]
    fn etw_session_starts_and_stops_cleanly() {
        // EOD-batch verification:
        // 1. EtwSession::start(SessionOptions::default()) succeeds.
        // 2. Brief delay (give the consumer thread a moment).
        // 3. query_stats() returns events_lost == 0,
        //    real_time_buffers_lost == 0 at idle.
        // 4. stop() succeeds and verify_session_gone() (internal)
        //    confirms ControlTraceW(QUERY) no longer finds it.
        //
        // Requires: Win11 24H2+ host, elevated, no other ETW consumer
        // holding the "FramesageEtw" session name.
        let opts = SessionOptions::default();
        let sess = EtwSession::start(opts).expect("start should succeed on Win11 24H2+ elevated");
        std::thread::sleep(std::time::Duration::from_millis(200));
        let stats = sess.query_stats().expect("query_stats");
        assert_eq!(stats.real_time_buffers_lost, 0, "no drops at idle");
        sess.stop().expect("stop should succeed cleanly");
    }
}
