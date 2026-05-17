//! v0.7 Phase 1 spike — prove the ETW kernel-consumer path works on
//! real Windows 11 hardware before any production code touches the
//! main workspace.
//!
//! What this binary does:
//!
//! 1. Starts a **private** real-time system-logger trace session
//!    (NOT the legacy "NT Kernel Logger" global singleton, which
//!    would conflict with WPR / xperf / any other consumer).
//! 2. Enables the kernel-flag set we need for v0.7:
//!      - `EVENT_TRACE_FLAG_CSWITCH`        — context switches
//!      - `EVENT_TRACE_FLAG_DPC`            — deferred procedure calls
//!      - `EVENT_TRACE_FLAG_INTERRUPT`      — ISR routines
//!      - `EVENT_TRACE_FLAG_DISK_IO`        — disk read/write
//!      - `EVENT_TRACE_FLAG_MEMORY_HARD_FAULTS` — page faults to disk
//! 3. Spawns a dedicated consumer thread that calls `ProcessTrace`
//!    against the live session via `OpenTraceW` in real-time mode.
//! 4. Counts events per provider GUID (Thread / PerfInfo / DiskIo /
//!    PageFault), parser failures, and tracks dropped events via
//!    periodic `ControlTraceW(EVENT_TRACE_CONTROL_QUERY)` reads of
//!    `RealTimeBuffersLost` + `EventsLost`.
//! 5. Runs for the configured duration (default 60 seconds) or
//!    until Ctrl-C.
//! 6. Stops the session cleanly so `logman query -ets` shows no
//!    leftover session after exit.
//!
//! Anti-cheat status: ETW consumption is the same API surface that
//! PerfView, xperf, Process Hacker, LatencyMon, and GPU-Z all use.
//! No game-protection product treats it as a flag. But: this spike
//! exists partly so we can verify EDR (Defender ATP / CrowdStrike /
//! SentinelOne) behavior before we wire the consumer into the
//! shipped service.

use std::collections::HashMap;
use std::io::Write;
use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::Parser;

use windows::core::GUID;
use windows::Win32::Foundation::{
    BOOL, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, ERROR_WMI_INSTANCE_NOT_FOUND, WIN32_ERROR,
};
use windows::Win32::System::Console::{SetConsoleCtrlHandler, CTRL_C_EVENT};
use windows::Win32::System::Diagnostics::Etw::{
    ControlTraceW, OpenTraceW, ProcessTrace, StartTraceW, CONTROLTRACE_HANDLE,
    EVENT_CONTROL_CODE_DISABLE_PROVIDER, EVENT_RECORD, EVENT_TRACE_CONTROL_QUERY,
    EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_FLAG_CSWITCH, EVENT_TRACE_FLAG_DISK_IO,
    EVENT_TRACE_FLAG_DPC, EVENT_TRACE_FLAG_INTERRUPT, EVENT_TRACE_FLAG_MEMORY_HARD_FAULTS,
    EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE,
    EVENT_TRACE_SYSTEM_LOGGER_MODE, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME,
    PROCESSTRACE_HANDLE, WNODE_FLAG_TRACED_GUID,
};

// ─── Constants ────────────────────────────────────────────────────────────────

/// Unique session name. Deliberately NOT "NT Kernel Logger" (the
/// legacy global singleton) — using a private name keeps us from
/// conflicting with WPR / xperf and lets us run alongside them.
const SESSION_NAME: &str = "FramesageEtwSpike";

/// Unique session GUID. Generated once; identifies our session in
/// `logman` output. Use a NEW guid every release to avoid stale-state
/// confusion if a previous version left a session behind.
const SESSION_GUID: GUID = GUID::from_u128(0x4F8B_1A60_9E2D_4F3F_88C2_5B7E1D6F92A4);

/// Default duration if no `--duration` flag passed.
const DEFAULT_DURATION_SECS: u64 = 60;

/// How often the main thread polls counters + queries the session
/// for dropped-event stats. Low enough to catch a transient burst,
/// high enough that the polling itself is invisible in counts.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

// ─── Kernel event provider GUIDs ──────────────────────────────────────────────
//
// These come from MSDN's "NT Kernel Logger Constants" doc. The
// individual kernel "providers" inside a system logger session are
// distinguished by the ProviderId in each event's EventHeader.

/// Thread events — opcodes include CSwitch (36).
const PROVIDER_THREAD: GUID = GUID::from_u128(0x3D6F_A8D1_FE05_11D0_9DDA_00C04FD7BA7C);
/// PerfInfo — DPC (opcode 46), ISR (66/67), SystemCallEnter, etc.
const PROVIDER_PERFINFO: GUID = GUID::from_u128(0xCE1D_BFB4_137E_4DA6_87B0_3F59AA102CBC);
/// DiskIo — Read/Write/Flush (10/11/14).
const PROVIDER_DISKIO: GUID = GUID::from_u128(0x3D6F_A8D4_FE05_11D0_9DDA_00C04FD7BA7C);
/// PageFault — HardFault (opcode 32), TransitionFault, etc.
const PROVIDER_PAGEFAULT: GUID = GUID::from_u128(0x3D6F_A8D3_FE05_11D0_9DDA_00C04FD7BA7C);

// ─── Shared counters ──────────────────────────────────────────────────────────

/// All counters incremented from the ETW callback thread, read from
/// the main thread for periodic snapshots. `Relaxed` because we don't
/// need cross-counter ordering — each is independent and the totals
/// only need to be eventually-consistent at snapshot time.
#[derive(Default)]
struct Counters {
    // Per-provider event counts.
    thread_events: AtomicU64,
    perfinfo_events: AtomicU64,
    diskio_events: AtomicU64,
    pagefault_events: AtomicU64,
    other_events: AtomicU64,

    // PerfInfo sub-categorisation by opcode.
    dpc_events: AtomicU64,
    isr_events: AtomicU64,

    // PageFault sub-category.
    hard_fault_events: AtomicU64,

    // Parser failures — events that arrived but our callback couldn't
    // make sense of. For the spike we only "parse" the header
    // (ProviderId + Opcode), so failures here mean an event with a
    // malformed header, which would indicate a serious problem.
    parse_failures: AtomicU64,

    // Total events seen at the callback level. Includes everything
    // above plus events that our callback gets called on but doesn't
    // bucket (counted in `other_events`). Used as the denominator
    // when computing the breakdown.
    total_events: AtomicU64,
}

impl Counters {
    fn snapshot(&self) -> CountersSnapshot {
        CountersSnapshot {
            thread: self.thread_events.load(Ordering::Relaxed),
            perfinfo: self.perfinfo_events.load(Ordering::Relaxed),
            diskio: self.diskio_events.load(Ordering::Relaxed),
            pagefault: self.pagefault_events.load(Ordering::Relaxed),
            other: self.other_events.load(Ordering::Relaxed),
            dpc: self.dpc_events.load(Ordering::Relaxed),
            isr: self.isr_events.load(Ordering::Relaxed),
            hard_fault: self.hard_fault_events.load(Ordering::Relaxed),
            parse_failures: self.parse_failures.load(Ordering::Relaxed),
            total: self.total_events.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct CountersSnapshot {
    thread: u64,
    perfinfo: u64,
    diskio: u64,
    pagefault: u64,
    other: u64,
    dpc: u64,
    isr: u64,
    hard_fault: u64,
    parse_failures: u64,
    total: u64,
}

// ─── ETW session properties buffer ────────────────────────────────────────────
//
// `EVENT_TRACE_PROPERTIES` is variable-length: the struct is followed
// in memory by the logger name (wide string) and optionally a
// log-file name. We allocate a flat repr(C) struct so the layout is
// stable and the offsets are easy to compute.

#[repr(C)]
struct EtwSessionPropertiesBuffer {
    base: EVENT_TRACE_PROPERTIES,
    /// Logger name buffer. `LoggerNameOffset` points to this field's
    /// offset within the struct.
    name: [u16; 128],
    /// Log-file name buffer. Unused for real-time sessions (no file
    /// is written) but the buffer must still exist so ETW has space
    /// to write a default name into.
    logfile: [u16; 128],
}

impl EtwSessionPropertiesBuffer {
    /// Build a properties buffer ready to pass to `StartTraceW`.
    ///
    /// `EnableFlags` is the OR'd set of `EVENT_TRACE_FLAG_*` values
    /// — what kernel events we want recorded.
    fn new(session_name: &str, enable_flags: u32) -> Self {
        // SAFETY: zero-initialisation gives us a valid (empty)
        // EVENT_TRACE_PROPERTIES. We then fill the fields we need.
        let mut buf: Self = unsafe { zeroed() };

        // Wnode setup — the SystemTraceProvider's session has a
        // unique GUID. Setting BufferSize to the total allocation
        // is what tells StartTraceW where the name buffers start.
        buf.base.Wnode.BufferSize = size_of::<Self>() as u32;
        buf.base.Wnode.Guid = SESSION_GUID;
        buf.base.Wnode.ClientContext = 1; // QPC timestamps (highest resolution)
        buf.base.Wnode.Flags = WNODE_FLAG_TRACED_GUID;

        // Real-time + system-logger mode. The combination is what
        // makes this a private (non-NT-Kernel-Logger) system
        // session — multiple of these can coexist on Win10+.
        buf.base.LogFileMode = EVENT_TRACE_REAL_TIME_MODE | EVENT_TRACE_SYSTEM_LOGGER_MODE;

        // Kernel flags — the actual "what events do you want".
        buf.base.EnableFlags = windows::Win32::System::Diagnostics::Etw::EVENT_TRACE_FLAG(
            enable_flags,
        );

        // Buffer tuning. Defaults (64 KiB buffers, 20-64 of them)
        // are usually fine; we can crank these via flags later.
        // For the spike, default sizing.
        buf.base.BufferSize = 64; // KB per buffer
        buf.base.MinimumBuffers = 20;
        buf.base.MaximumBuffers = 100;
        buf.base.FlushTimer = 1; // seconds — flush at most every 1s

        // Encode the session name into the trailing buffer.
        let name_utf16: Vec<u16> = session_name.encode_utf16().collect();
        let copy_len = name_utf16.len().min(buf.name.len() - 1);
        buf.name[..copy_len].copy_from_slice(&name_utf16[..copy_len]);
        // Null terminator (already 0 from zeroed()).

        // Offsets within the buffer. ETW uses these to locate the
        // name strings without us passing them separately.
        buf.base.LoggerNameOffset =
            (size_of::<EVENT_TRACE_PROPERTIES>()) as u32;
        buf.base.LogFileNameOffset =
            buf.base.LoggerNameOffset + (size_of::<[u16; 128]>() as u32);

        buf
    }

    fn as_mut_ptr(&mut self) -> *mut EVENT_TRACE_PROPERTIES {
        // SAFETY: repr(C) layout means `base` is at offset 0; the
        // pointer is valid for the lifetime of `self`.
        &mut self.base as *mut _
    }
}

// ─── Ctrl-C handler ───────────────────────────────────────────────────────────
//
// SetConsoleCtrlHandler runs the handler on a thread the OS picks.
// We just flip an atomic; the main loop polls it and triggers
// session shutdown.

static CTRL_C_RECEIVED: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn ctrl_c_handler(ctrl_type: u32) -> BOOL {
    if ctrl_type == CTRL_C_EVENT {
        CTRL_C_RECEIVED.store(true, Ordering::SeqCst);
        // Return TRUE (handled) so the OS doesn't terminate us
        // before the main loop runs cleanup.
        return BOOL(1);
    }
    BOOL(0)
}

// ─── ETW event callback (called on the consumer's thread) ─────────────────────
//
// PROCESS_TRACE_MODE_EVENT_RECORD makes `ProcessTrace` deliver each
// event as an `EVENT_RECORD`. The `UserContext` field of the LOGFILE
// is our `Arc<Counters>` (raw pointer cast); we don't own the
// memory, we just bump counters and return ASAP.

unsafe extern "system" fn event_record_callback(event_record: *mut EVENT_RECORD) {
    if event_record.is_null() {
        return;
    }
    let er = &*event_record;
    let ctx = er.UserContext as *const Counters;
    if ctx.is_null() {
        return;
    }
    let counters = &*ctx;

    counters.total_events.fetch_add(1, Ordering::Relaxed);

    let provider = er.EventHeader.ProviderId;
    let opcode = er.EventHeader.EventDescriptor.Opcode;

    if provider == PROVIDER_THREAD {
        counters.thread_events.fetch_add(1, Ordering::Relaxed);
    } else if provider == PROVIDER_PERFINFO {
        counters.perfinfo_events.fetch_add(1, Ordering::Relaxed);
        // DPC == 0x2E (46), ISR == 0x42-0x43 (66/67). Some PerfInfo
        // opcodes overlap across Win10 / Win11 builds; this is the
        // documented set.
        match opcode {
            0x2E => {
                counters.dpc_events.fetch_add(1, Ordering::Relaxed);
            }
            0x42 | 0x43 => {
                counters.isr_events.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    } else if provider == PROVIDER_DISKIO {
        counters.diskio_events.fetch_add(1, Ordering::Relaxed);
    } else if provider == PROVIDER_PAGEFAULT {
        counters.pagefault_events.fetch_add(1, Ordering::Relaxed);
        if opcode == 0x20 {
            counters.hard_fault_events.fetch_add(1, Ordering::Relaxed);
        }
    } else {
        counters.other_events.fetch_add(1, Ordering::Relaxed);
    }
}

// ─── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "spike-etw", about = "v0.7 ETW spike")]
struct Cli {
    /// Run duration in seconds (default 60).
    #[arg(long, default_value_t = DEFAULT_DURATION_SECS)]
    duration: u64,

    /// Override buffer count multiplier. 1.0 = defaults. Use 2.0 or
    /// 4.0 to test drop-rate behavior under higher buffer counts
    /// (spike report compares these explicitly).
    #[arg(long, default_value_t = 1.0)]
    buffer_mult: f64,

    /// Print per-second progress lines (verbose).
    #[arg(long)]
    verbose: bool,
}

// ─── Main ─────────────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let cli = Cli::parse();

    println!("====================================================");
    println!("  framesage v0.7 ETW spike");
    println!("====================================================");
    println!("session name:   {}", SESSION_NAME);
    println!(
        "session GUID:   {{{:08x}-{:04x}-{:04x}-...}}",
        SESSION_GUID.data1, SESSION_GUID.data2, SESSION_GUID.data3
    );
    println!("duration:       {} seconds", cli.duration);
    println!("buffer mult:    {}x default", cli.buffer_mult);
    println!();

    // Install Ctrl-C handler before any of the work — if the user
    // hits Ctrl-C mid-startup we still need clean session teardown.
    // SAFETY: ctrl_c_handler is a static fn pointer with the right
    // calling convention.
    unsafe {
        SetConsoleCtrlHandler(Some(ctrl_c_handler), true).ok();
    }

    // If a previous run left the session registered (crash, kill -9,
    // etc.), tear it down before we try to start a fresh one. This
    // is idempotent — ERROR_WMI_INSTANCE_NOT_FOUND just means
    // "session wasn't there", which is what we wanted anyway.
    cleanup_stale_session()?;

    let counters = Arc::new(Counters::default());

    // Start the session.
    let session_handle = start_session(cli.buffer_mult)?;
    println!("[etw] session started (handle 0x{:x})", session_handle.Value);

    // Open the trace for consumption + spawn the consumer thread.
    // The consumer's main job is to call `ProcessTrace` and let it
    // block; events arrive via the static callback above. We give
    // the callback a raw pointer to the shared counters via
    // `UserContext`.
    let counters_for_thread = counters.clone();
    let consumer = thread::Builder::new()
        .name("etw-consumer".into())
        .spawn(move || -> Result<()> { run_consumer(counters_for_thread) })
        .context("spawn consumer thread")?;

    // Main loop: every POLL_INTERVAL, snapshot counters + query the
    // session for buffer-loss stats. Exit on duration elapsed or
    // Ctrl-C.
    let deadline = Instant::now() + Duration::from_secs(cli.duration);
    let mut last_snapshot = counters.snapshot();
    let mut last_query = query_session_stats().ok();
    let mut tick = 0u64;
    while Instant::now() < deadline && !CTRL_C_RECEIVED.load(Ordering::SeqCst) {
        thread::sleep(POLL_INTERVAL);
        tick += 1;
        let now = counters.snapshot();
        let q = query_session_stats().ok();
        if cli.verbose {
            print_tick(tick, &last_snapshot, &now, last_query.as_ref(), q.as_ref());
        }
        last_snapshot = now;
        last_query = q;
    }

    if CTRL_C_RECEIVED.load(Ordering::SeqCst) {
        println!();
        println!("[etw] Ctrl-C received; stopping session...");
    } else {
        println!();
        println!("[etw] duration elapsed; stopping session...");
    }

    // Stop. Once `ControlTraceW(STOP)` returns, the consumer's
    // `ProcessTrace` call returns shortly after — typically <100 ms.
    stop_session()?;

    // Join the consumer. If it errored we surface it but don't
    // exit non-zero unless the error is unexpected (ProcessTrace
    // returns ERROR_CANCELLED on clean stop, which we treat as
    // success).
    match consumer.join() {
        Ok(Ok(())) => println!("[etw] consumer thread exited cleanly"),
        Ok(Err(e)) => println!("[etw] consumer thread error: {e:#}"),
        Err(_) => println!("[etw] consumer thread panicked"),
    }

    // Final stats — counters + dropped-event totals.
    print_final_summary(&counters);
    println!();
    verify_session_gone()?;

    Ok(())
}

fn start_session(buffer_mult: f64) -> Result<CONTROLTRACE_HANDLE> {
    let mut props = EtwSessionPropertiesBuffer::new(
        SESSION_NAME,
        EVENT_TRACE_FLAG_CSWITCH.0
            | EVENT_TRACE_FLAG_DPC.0
            | EVENT_TRACE_FLAG_INTERRUPT.0
            | EVENT_TRACE_FLAG_DISK_IO.0
            | EVENT_TRACE_FLAG_MEMORY_HARD_FAULTS.0,
    );
    // Apply the buffer multiplier — the spike compares 1×/2×/4×.
    props.base.MinimumBuffers =
        ((props.base.MinimumBuffers as f64) * buffer_mult).round() as u32;
    props.base.MaximumBuffers =
        ((props.base.MaximumBuffers as f64) * buffer_mult).round() as u32;

    let name_wide: Vec<u16> = SESSION_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut handle: CONTROLTRACE_HANDLE = CONTROLTRACE_HANDLE::default();

    // SAFETY: name_wide is null-terminated; props is a valid
    // EVENT_TRACE_PROPERTIES with name/logfile offsets pointing
    // into the trailing buffer.
    let rc = unsafe {
        StartTraceW(
            &mut handle,
            windows::core::PCWSTR(name_wide.as_ptr()),
            props.as_mut_ptr(),
        )
    };

    if rc != ERROR_SUCCESS {
        let code = rc.0;
        if rc == ERROR_ALREADY_EXISTS {
            bail!(
                "StartTraceW returned ERROR_ALREADY_EXISTS — a session named \
                 '{}' is registered but cleanup_stale_session() should have \
                 removed it. Try: `logman stop {} -ets` manually.",
                SESSION_NAME,
                SESSION_NAME
            );
        }
        bail!(
            "StartTraceW failed: Win32 error {code} (0x{code:08x}). \
             Common causes: not elevated (need admin token + \
             SeSystemProfilePrivilege), or running under a security \
             product that blocks ETW session creation."
        );
    }

    Ok(handle)
}

fn run_consumer(counters: Arc<Counters>) -> Result<()> {
    let name_wide: Vec<u16> = SESSION_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut logfile: EVENT_TRACE_LOGFILEW = unsafe { zeroed() };
    logfile.LoggerName = windows::core::PWSTR(name_wide.as_ptr() as *mut u16);
    logfile.Anonymous1.ProcessTraceMode =
        PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
    logfile.Anonymous2.EventRecordCallback = Some(event_record_callback);
    // Pass the counters pointer as UserContext — the callback reads
    // it back. The Arc is kept alive by `counters` here for the
    // lifetime of `ProcessTrace`; we don't free it until after
    // ProcessTrace returns.
    logfile.Context = Arc::as_ptr(&counters) as *mut std::ffi::c_void;

    // SAFETY: logfile holds a valid LoggerName pointer for the
    // duration of OpenTraceW. PROCESS_TRACE_MODE_REAL_TIME means
    // OpenTraceW will hook up to the live session by name.
    let handle = unsafe { OpenTraceW(&mut logfile) };
    if handle.Value == u64::MAX {
        // INVALID_PROCESSTRACE_HANDLE
        bail!("OpenTraceW failed (invalid handle returned)");
    }

    let handles = [PROCESSTRACE_HANDLE { Value: handle.Value }];
    // SAFETY: handles is a valid array of one handle. ProcessTrace
    // blocks until the session is stopped (or, in non-real-time
    // mode, until the file is fully consumed).
    let rc = unsafe { ProcessTrace(&handles, None, None) };
    // ERROR_CANCELLED is what we get when ControlTrace(STOP) fires
    // from the main thread. Treat as clean exit.
    if rc != ERROR_SUCCESS && rc.0 != /* ERROR_CANCELLED */ 1223 {
        let code = rc.0;
        bail!(
            "ProcessTrace returned unexpected error: {code} (0x{code:08x})"
        );
    }

    Ok(())
}

fn stop_session() -> Result<()> {
    let mut props = EtwSessionPropertiesBuffer::new(SESSION_NAME, 0);
    let name_wide: Vec<u16> = SESSION_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: documented call. Passing 0 for the session handle is
    // valid when the name is set; ETW looks up the session by name.
    let rc = unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            windows::core::PCWSTR(name_wide.as_ptr()),
            props.as_mut_ptr(),
            EVENT_TRACE_CONTROL_STOP,
        )
    };
    if rc != ERROR_SUCCESS && rc != ERROR_WMI_INSTANCE_NOT_FOUND {
        bail!(
            "ControlTraceW(STOP) failed: {} (0x{:08x})",
            rc.0,
            rc.0
        );
    }
    Ok(())
}

fn cleanup_stale_session() -> Result<()> {
    let mut props = EtwSessionPropertiesBuffer::new(SESSION_NAME, 0);
    let name_wide: Vec<u16> = SESSION_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: same shape as stop_session above.
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
            println!("[etw] cleaned up stale session from prior run");
        }
        r if r == ERROR_WMI_INSTANCE_NOT_FOUND => {
            // Expected — no prior session.
        }
        other => {
            // Non-fatal — we'll see how StartTraceW reacts.
            println!(
                "[etw] cleanup got Win32 error {} (0x{:08x}) — continuing",
                other.0, other.0
            );
        }
    }
    Ok(())
}

fn query_session_stats() -> Result<QueryResult> {
    let mut props = EtwSessionPropertiesBuffer::new(SESSION_NAME, 0);
    let name_wide: Vec<u16> = SESSION_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: same shape. After QUERY returns, props.base.EventsLost
    // and RealTimeBuffersLost reflect totals since session start.
    let rc = unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            windows::core::PCWSTR(name_wide.as_ptr()),
            props.as_mut_ptr(),
            EVENT_TRACE_CONTROL_QUERY,
        )
    };
    if rc != ERROR_SUCCESS {
        bail!(
            "ControlTraceW(QUERY) failed: {} (0x{:08x})",
            rc.0,
            rc.0
        );
    }
    Ok(QueryResult {
        events_lost: props.base.EventsLost,
        real_time_buffers_lost: props.base.RealTimeBuffersLost,
        log_buffers_lost: props.base.LogBuffersLost,
        buffers_written: props.base.BuffersWritten,
        number_of_buffers: props.base.NumberOfBuffers,
        free_buffers: props.base.FreeBuffers,
    })
}

#[derive(Debug, Clone, Copy)]
struct QueryResult {
    events_lost: u32,
    real_time_buffers_lost: u32,
    log_buffers_lost: u32,
    buffers_written: u32,
    number_of_buffers: u32,
    free_buffers: u32,
}

fn verify_session_gone() -> Result<()> {
    let q = query_session_stats();
    match q {
        Err(_) => {
            println!("[etw] verify: session correctly removed (QUERY no longer succeeds)");
            Ok(())
        }
        Ok(_) => {
            println!(
                "[etw] verify: WARNING — session is still registered after STOP. \
                 Run `logman query -ets` to inspect; manual cleanup may be needed."
            );
            Ok(())
        }
    }
}

fn print_tick(
    tick: u64,
    last: &CountersSnapshot,
    now: &CountersSnapshot,
    last_query: Option<&QueryResult>,
    now_query: Option<&QueryResult>,
) {
    let delta_total = now.total.saturating_sub(last.total);
    let delta_dpc = now.dpc.saturating_sub(last.dpc);
    let delta_isr = now.isr.saturating_sub(last.isr);
    let delta_diskio = now.diskio.saturating_sub(last.diskio);
    let delta_hf = now.hard_fault.saturating_sub(last.hard_fault);
    let drops_now = now_query.map(|q| q.events_lost).unwrap_or(0);
    let drops_last = last_query.map(|q| q.events_lost).unwrap_or(0);
    let drops_delta = drops_now.saturating_sub(drops_last);
    println!(
        "[t={tick:>3}s] total={delta_total:>6} cswitch={:>5} dpc={delta_dpc:>4} isr={delta_isr:>4} disk={delta_diskio:>4} hardfault={delta_hf:>3}  dropped={drops_delta}",
        now.thread.saturating_sub(last.thread)
    );
    let _ = std::io::stdout().flush();
}

fn print_final_summary(counters: &Counters) {
    let s = counters.snapshot();
    let q = query_session_stats().ok();
    println!();
    println!("====================================================");
    println!("  Final summary");
    println!("====================================================");
    println!(
        "  Total events received:         {:>12}",
        s.total
    );
    println!(
        "    Thread (incl. CSwitch):      {:>12}",
        s.thread
    );
    println!(
        "    PerfInfo (DPC + ISR + ...):  {:>12}",
        s.perfinfo
    );
    println!("      of which DPC:              {:>12}", s.dpc);
    println!("      of which ISR:              {:>12}", s.isr);
    println!(
        "    DiskIo:                      {:>12}",
        s.diskio
    );
    println!(
        "    PageFault:                   {:>12}",
        s.pagefault
    );
    println!("      of which HardFault:        {:>12}", s.hard_fault);
    println!(
        "    Other:                       {:>12}",
        s.other
    );
    println!("  Parse failures:                {:>12}", s.parse_failures);
    if let Some(q) = q {
        println!();
        println!("  Session-level buffer stats:");
        println!("    EventsLost:                  {:>12}", q.events_lost);
        println!("    RealTimeBuffersLost:         {:>12}", q.real_time_buffers_lost);
        println!("    LogBuffersLost:              {:>12}", q.log_buffers_lost);
        println!("    BuffersWritten:              {:>12}", q.buffers_written);
        println!("    NumberOfBuffers:             {:>12}", q.number_of_buffers);
        println!("    FreeBuffers:                 {:>12}", q.free_buffers);
        let drop_pct = if s.total == 0 {
            0.0
        } else {
            (q.events_lost as f64) / ((s.total + q.events_lost as u64) as f64) * 100.0
        };
        println!();
        println!(
            "  Drop rate:                     {:>11.4}%",
            drop_pct
        );
    }
    // Provider mix as percent of total — useful for sanity check
    // (e.g. "thread events should be 60-80% of total").
    if s.total > 0 {
        println!();
        println!("  Provider mix (% of total):");
        let pct = |n: u64| (n as f64) / (s.total as f64) * 100.0;
        let mut rows: Vec<(&str, f64)> = vec![
            ("Thread", pct(s.thread)),
            ("PerfInfo", pct(s.perfinfo)),
            ("DiskIo", pct(s.diskio)),
            ("PageFault", pct(s.pagefault)),
            ("Other", pct(s.other)),
        ];
        rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        for (name, p) in rows {
            println!("    {name:<12} {p:>6.2}%");
        }
    }
}

// silence unused-import warnings on enum variants used only via the
// EVENT_CONTROL_CODE_DISABLE_PROVIDER constant for future TraceSet
// extension (kept here so the import list reads in context).
const _: u32 = EVENT_CONTROL_CODE_DISABLE_PROVIDER.0;
// silence WIN32_ERROR / HashMap which we may want for opcode breakdown.
const _: Option<HashMap<u32, u32>> = None;
const _: WIN32_ERROR = ERROR_SUCCESS;
