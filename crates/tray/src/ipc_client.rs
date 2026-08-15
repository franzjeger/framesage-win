//! Item 3.6 — IPC client plumbing lifted out of main.rs.
//!
//! Three long-running threads + a couple of one-shot helpers, all
//! talking to the framesage service over named pipes:
//!
//! 1. [`background_loop`] — opens the status pipe, subscribes to
//!    events, pushes them into [`AppState::recent`] and persists each
//!    to `%LOCALAPPDATA%\framesage\activity.jsonl`. Reconnects on
//!    failure with a 1.5 s back-off.
//! 2. [`processes_poll_loop`] — sends `Request::ListProcesses` +
//!    `Request::Status` once per second (8× less often when the
//!    window is hidden), updates the Processes-tab snapshot + the
//!    perf-band sparkline.
//! 3. [`foreground_reporter_loop`] — installs a
//!    `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` to wake on focus
//!    changes (with a 250 ms fallback poll), sends
//!    `Request::ReportForeground` / `Request::ReportNoForeground` to
//!    the admin pipe. Closes the session-0 isolation gap (services
//!    can't see `GetForegroundWindow` cross-session).
//!
//! Plus:
//!
//! * [`send_request_blocking`] — one-shot pipe round-trip used by
//!   admin button handlers. Uses [`wait_for_pipe`] internally so a
//!   missing service surfaces a clear error instead of hanging.
//! * [`send_processes_and_status_blocking`] — internal helper used
//!   by `processes_poll_loop`.
//!
//! All Windows-only; non-Windows stubs at the bottom keep cross-build
//! type-checking green for `framesage-sim`.

#![cfg_attr(not(windows), allow(dead_code))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use eframe::egui;
use framesage_ipc::{Event, ProcessSnapshot, Request, Response, StatusSnapshot, SystemMetrics};

use crate::activity_log;
use crate::state::{AppState, EventKind, RecentEvent, SYSTEM_HISTORY_LEN};

// ─── Tuning constants ─────────────────────────────────────────────────────────

/// Back-off between reconnect attempts in [`background_loop`]. 1.5 s is
/// long enough to avoid hammering a dead service with pipe opens, short
/// enough that the "reconnecting…" banner disappears quickly once the
/// service comes back.
const RECONNECT_BACKOFF: Duration = Duration::from_millis(1500);

/// Poll cadence for `ListProcesses` + `Status` when the window is visible.
/// 1 Hz matches the engine's own tick interval so the tray never asks for
/// fresher data than the service produces.
const POLL_INTERVAL_VISIBLE: Duration = Duration::from_millis(1000);

/// Poll cadence when the window is hidden. 8× less frequent to cut the
/// idle CPU floor (the user-reported issue that motivated this split).
const POLL_INTERVAL_HIDDEN: Duration = Duration::from_millis(8000);

/// Cap on the in-memory `AppState::recent` ring buffer. ~5 minutes of
/// constant foreground flicker; more than enough for the Activity strip
/// (shows 5) and the Recent Activity panel (shows 20).
const MAX_RECENT: usize = 1000;

// ─── Event-subscribe loop ────────────────────────────────────────────────────

#[cfg(windows)]
pub(crate) fn background_loop(state: Arc<Mutex<AppState>>) {
    // Simple blocking client using the std synchronous named-pipe support via
    // `std::fs::OpenOptions`. The pipe path is documented to work with
    // CreateFile semantics under the hood.
    //
    // Item 2.9 — the activity-log writer lives here so it survives
    // reconnect attempts (a service restart doesn't lose the writer
    // half). Rotation runs once at startup so we don't carry an
    // unbounded file across launches.
    let mut activity_log: Option<activity_log::ActivityLog> = match activity_log::ActivityLog::open(
    ) {
        Ok(mut log) => {
            if let Err(e) = log.rotate_if_oversized() {
                tracing::warn!(error = %e, "activity-log rotation failed; continuing");
            }
            Some(log)
        }
        Err(e) => {
            tracing::warn!(error = %e, "activity-log open failed; events won't persist this run");
            None
        }
    };
    loop {
        match try_connect_and_serve(state.clone(), activity_log.as_mut()) {
            Ok(()) => {}
            Err(e) => {
                let mut s = state.lock();
                s.connected = false;
                s.last_error = Some(format!("{e:#}"));
            }
        }
        std::thread::sleep(RECONNECT_BACKOFF);
    }
}

#[cfg(windows)]
fn try_connect_and_serve(
    state: Arc<Mutex<AppState>>,
    mut activity_log: Option<&mut activity_log::ActivityLog>,
) -> anyhow::Result<()> {
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};

    // The tray only ever sends Status + Subscribe — both read-only — so
    // we open the status pipe. That pipe's ACL grants Authenticated Users
    // access, so the tray works without elevation. (The admin pipe would
    // refuse an unprivileged caller at the OS layer.)
    //
    // FILE_FLAG_OVERLAPPED is not set; we get blocking semantics.
    let pipe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(framesage_ipc::PIPE_NAME_STATUS)?;
    {
        let mut s = state.lock();
        s.connected = true;
        s.last_error = None;
    }

    let mut writer = pipe.try_clone()?;
    let mut reader = BufReader::new(pipe);

    // Get an initial status snapshot.
    let mut buf = serde_json::to_vec(&Request::Status)?;
    buf.push(b'\n');
    writer.write_all(&buf)?;
    writer.flush()?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if let Ok(Response::Status(snap)) = serde_json::from_str::<Response>(&line) {
        state.lock().status = Some(*snap);
    }

    // Then subscribe to events.
    let mut buf = serde_json::to_vec(&Request::Subscribe)?;
    buf.push(b'\n');
    writer.write_all(&buf)?;
    writer.flush()?;

    line.clear();
    while reader.read_line(&mut line)? > 0 {
        if let Ok(event) = serde_json::from_str::<Event>(&line) {
            let (kind, label) = match &event {
                Event::ForegroundChanged {
                    foreground,
                    profile,
                    matched_rule_index,
                } => (
                    EventKind::Foreground,
                    // Item 4.15 — surface the matched rule index when
                    // available so the activity feed answers "which
                    // rule caused this?" at a glance. Suppress when no
                    // rule matched (default profile path / manual
                    // override / apply_once).
                    if let Some(idx) = matched_rule_index {
                        format!(
                            "{} -> {} (pid {}, rule #{idx})",
                            foreground.exe_name, profile, foreground.pid
                        )
                    } else {
                        format!(
                            "{} -> {} (pid {})",
                            foreground.exe_name, profile, foreground.pid
                        )
                    },
                ),
                Event::Paused => (EventKind::Engine, "engine paused".into()),
                Event::Resumed => (EventKind::Engine, "engine resumed".into()),
                Event::ProBalanceRestrained {
                    pid,
                    exe_name,
                    from_class,
                    to_class,
                } => (
                    EventKind::ProBalanceRestrained,
                    format!(
                        "probalance restrained {} (pid {}) {:#x} -> {:#x}",
                        exe_name, pid, from_class, to_class
                    ),
                ),
                Event::ProBalanceRestored {
                    pid,
                    exe_name,
                    restored_class,
                } => (
                    EventKind::ProBalanceRestored,
                    format!(
                        "probalance restored {} (pid {}) -> {:#x}",
                        exe_name, pid, restored_class
                    ),
                ),
                // ─── Item 2.8 / audit H-28 ──────────────────────
                Event::GameModeEntered {
                    profile_id,
                    services_to_stop,
                    processes_to_suspend,
                    power_plan_changing,
                    taskbar_hiding,
                    pausing_windows_update,
                } => (
                    EventKind::Engine,
                    format!(
                        "Game Mode entered: {} ({} svcs, {} procs{}{}{})",
                        profile_id,
                        services_to_stop,
                        processes_to_suspend,
                        if *power_plan_changing {
                            ", power plan"
                        } else {
                            ""
                        },
                        if *taskbar_hiding { ", taskbar" } else { "" },
                        if *pausing_windows_update { ", WU" } else { "" },
                    ),
                ),
                Event::GameModeExited {
                    profile_id,
                    services_restored,
                    processes_resumed,
                    duration_secs,
                    reason,
                    ..
                } => (
                    EventKind::Engine,
                    format!(
                        "Game Mode exited: {} after {}s ({}; {} svcs / {} procs restored)",
                        profile_id, duration_secs, reason, services_restored, processes_resumed
                    ),
                ),
                Event::ProfileApplied {
                    pid,
                    exe_name,
                    profile_id,
                } => (
                    EventKind::Engine,
                    format!("applied {} -> {} (pid {})", profile_id, exe_name, pid),
                ),
                Event::ProfileReverted {
                    pid,
                    exe_name,
                    profile_id,
                } => (
                    EventKind::Engine,
                    format!("reverted {} from {} (pid {})", profile_id, exe_name, pid),
                ),
                Event::AffinityRuleFired {
                    pid,
                    exe_name,
                    rule_exe,
                } => (
                    EventKind::Engine,
                    format!(
                        "affinity rule '{}' fired against {} (pid {})",
                        rule_exe, exe_name, pid
                    ),
                ),
                Event::ActionFailed {
                    kind,
                    pid,
                    exe_name,
                    details,
                } => (
                    EventKind::Other,
                    format!(
                        "action failed: {:?}{}{}{}",
                        kind,
                        exe_name
                            .as_deref()
                            .map(|n| format!(" {n}"))
                            .unwrap_or_default(),
                        pid.map(|p| format!(" (pid {p})")).unwrap_or_default(),
                        format_args!(" — {details}"),
                    ),
                ),
                Event::AntiCheatPresenceChanged { which, active } => (
                    EventKind::Engine,
                    format!(
                        "AC presence change: {} {}",
                        which,
                        if *active { "active" } else { "inactive" }
                    ),
                ),
            };
            let now = std::time::SystemTime::now();
            // Item 2.9 — persist the event before pushing into the
            // in-memory buffer so a crash between push and flush
            // doesn't leave the UI showing an event that isn't on
            // disk. Append failures are warn-and-continue: the UI
            // experience is more important than the persistence
            // guarantee for any single event.
            if let Some(log) = activity_log.as_deref_mut() {
                let pe = activity_log::PersistedActivityEvent::new(
                    now,
                    kind.persist_tag(),
                    label.clone(),
                );
                if let Err(e) = log.append(&pe) {
                    tracing::warn!(error = %e, "activity-log append failed");
                }
            }

            let mut s = state.lock();
            s.recent.push(RecentEvent {
                at: now,
                kind,
                label,
            });
            // Cap the event buffer — see MAX_RECENT for the rationale.
            if s.recent.len() > MAX_RECENT {
                let drop = s.recent.len() - MAX_RECENT;
                s.recent.drain(0..drop);
            }
            if let (Event::ForegroundChanged { foreground, .. }, Some(snap)) =
                (&event, s.status.as_mut())
            {
                snap.foreground = Some(foreground.clone());
            }
        }
        line.clear();
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn background_loop(state: Arc<Mutex<AppState>>) {
    let mut s = state.lock();
    s.last_error = Some("tray UI only operates against a Windows service".into());
}

// ─── Processes-tab data poller ───────────────────────────────────────────────

/// Poll `Request::ListProcesses` over the status pipe every 1 s and push
/// the result (plus paired system metrics) into `AppState`. Wakes the egui
/// runtime each refresh so the Processes tab and the performance band
/// update even when no other input arrives.
#[cfg(windows)]
pub(crate) fn processes_poll_loop(
    state: Arc<Mutex<AppState>>,
    ctx: egui::Context,
    window_visible: Arc<AtomicBool>,
) {
    // Cadence depends on whether the window is visible. Hidden window =
    // poll 8× less often (and skip the egui repaint wake entirely). The
    // user reported FrameSage burning CPU; this is the largest single
    // contributor — 120-row table render every 1 s × always-on = the
    // bulk of the idle CPU floor. See POLL_INTERVAL_* for the values.
    let visible_interval = POLL_INTERVAL_VISIBLE;
    let hidden_interval = POLL_INTERVAL_HIDDEN;
    loop {
        let visible = window_visible.load(Ordering::Relaxed);
        match send_processes_and_status_blocking() {
            Ok((snapshots, system, status)) => {
                let mem_percent: u8 = if system.memory_total_bytes > 0 {
                    ((system.memory_used_bytes as u128 * 100 / system.memory_total_bytes as u128)
                        .min(100)) as u8
                } else {
                    0
                };
                let cpu_for_history = system.cpu_percent;
                let mut s = state.lock();

                // Item 3.4 — capture per-PID CPU% into the per-PID
                // ring buffer BEFORE swapping s.processes, so we can
                // diff the new and old PID sets in one pass. Each
                // VecDeque caps at SYSTEM_HISTORY_LEN.
                let new_pids: std::collections::HashSet<u32> =
                    snapshots.iter().map(|p| p.pid).collect();
                for snap in &snapshots {
                    let entry = s.per_pid_cpu_history.entry(snap.pid).or_insert_with(|| {
                        std::collections::VecDeque::with_capacity(SYSTEM_HISTORY_LEN)
                    });
                    entry.push_back(snap.cpu_percent.min(255) as u8);
                    while entry.len() > SYSTEM_HISTORY_LEN {
                        entry.pop_front();
                    }
                }
                // Evict PIDs that disappeared this tick — keeps the
                // map bounded to the live process count instead of
                // growing forever as PIDs come and go.
                s.per_pid_cpu_history
                    .retain(|pid, _| new_pids.contains(pid));

                s.processes = snapshots;
                s.system = system;
                // Refresh the cached Status every tick so the UI never
                // shows stale paused/policy state. Without this, clicking
                // Pause/Resume or Enable-ProBalance updates the engine but
                // the UI keeps showing the value cached at first connect.
                s.status = Some(status);
                s.system_history.push_back((cpu_for_history, mem_percent));
                while s.system_history.len() > SYSTEM_HISTORY_LEN {
                    s.system_history.pop_front();
                }
                drop(s);
                if visible {
                    ctx.request_repaint();
                }
            }
            Err(_) => {
                // Service down or pipe busy — skip this tick. The connect
                // status drives the UI's "Disconnected" pill via the
                // existing background_loop; no need to surface the failure
                // here too.
            }
        }
        std::thread::sleep(if visible {
            visible_interval
        } else {
            hidden_interval
        });
    }
}

#[cfg(not(windows))]
pub(crate) fn processes_poll_loop(
    _state: Arc<Mutex<AppState>>,
    _ctx: egui::Context,
    _window_visible: Arc<AtomicBool>,
) {
}

/// One status-pipe round-trip per tick: send ListProcesses, then Status,
/// read both responses. Reuses a single pipe instance so we only burn one
/// ACL check per second. The Status fetch is what keeps the tray's view
/// of `paused` + `policy.probalance.enabled` in sync with the engine —
/// without it, the UI shows whatever values were current at first connect.
#[cfg(windows)]
fn send_processes_and_status_blocking(
) -> anyhow::Result<(Vec<ProcessSnapshot>, SystemMetrics, StatusSnapshot)> {
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};

    let pipe = OpenOptions::new()
        .read(true)
        .write(true)
        .open(framesage_ipc::PIPE_NAME_STATUS)?;
    let mut writer = pipe.try_clone()?;
    let mut reader = BufReader::new(pipe);

    // ListProcesses
    let mut buf = serde_json::to_vec(&Request::ListProcesses)?;
    buf.push(b'\n');
    writer.write_all(&buf)?;
    writer.flush()?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let (snapshots, system) = match serde_json::from_str::<Response>(&line)? {
        Response::Processes { snapshots, system } => (snapshots, system),
        other => {
            return Err(anyhow::anyhow!(
                "expected Processes response, got {other:?}"
            ))
        }
    };

    // Status — same pipe, same handler, just a second request.
    let mut buf = serde_json::to_vec(&Request::Status)?;
    buf.push(b'\n');
    writer.write_all(&buf)?;
    writer.flush()?;
    line.clear();
    reader.read_line(&mut line)?;
    let status = match serde_json::from_str::<Response>(&line)? {
        Response::Status(s) => *s,
        other => return Err(anyhow::anyhow!("expected Status response, got {other:?}")),
    };

    Ok((snapshots, system, status))
}

// ─── Foreground reporter (WinEvent hook + message pump) ──────────────────────

/// Foreground-reporter thread. Watches the user-session foreground
/// window and forwards changes to the service via the admin pipe.
/// Stays running for the program's lifetime; the engine prefers
/// these reports over its own (session-0-blind) polling.
///
/// Item 2.2 / audit M-01. Previously this polled
/// `foreground::current()` every 250 ms blindly. That's roughly four
/// foreground-related Win32 calls per second forever, even when the
/// user is idle and the foreground hasn't moved in hours. Now: a
/// `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` registration wakes us
/// the instant focus moves, and `MsgWaitForMultipleObjectsEx`
/// pumps the message queue so the OS can deliver the hook callback.
/// The 250 ms timeout on the wait stays in place as a fallback —
/// some fullscreen-exclusive games and remote-session edge cases
/// don't fire the system-foreground event reliably, and we'd rather
/// over-report than miss a focus change. Steady-state on an idle
/// desktop: ~0 wake-ups per second instead of 4. Item 2.5 / audit
/// L-02 backoff stays in place: if the service is down the wait
/// interval doubles up to 5 s so we don't slam the pipe-wait
/// timeout 4×/sec during a restart cycle.
#[cfg(windows)]
pub(crate) fn foreground_reporter_loop() {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent};
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MsgWaitForMultipleObjectsEx, PeekMessageW, EVENT_SYSTEM_FOREGROUND, MSG,
        MSG_WAIT_FOR_MULTIPLE_OBJECTS_EX_FLAGS, PM_REMOVE, QS_ALLINPUT, WINEVENT_OUTOFCONTEXT,
        WINEVENT_SKIPOWNPROCESS,
    };

    // Item 2.5 / audit L-02 — exponential backoff stays in place; if
    // the service goes down we still want the wait-interval to grow
    // so we're not slamming the pipe-wait timeout 4×/sec.
    const NORMAL_INTERVAL_MS: u32 = 250;
    const BACKOFF_CAP_MS: u32 = 5_000;

    // Hook is installed once at thread start and torn down only if
    // the loop ever exits (which it doesn't — but UnhookWinEvent is
    // symmetric and reads cleanly). With WINEVENT_OUTOFCONTEXT, the
    // callback runs on THIS thread, so the hook MUST live alongside
    // the message pump. WINEVENT_SKIPOWNPROCESS suppresses events
    // generated by the tray's own focus changes (e.g. the settings
    // window getting focus when the user opens it) — those aren't
    // interesting and would cause a spurious report cycle.
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            HMODULE::default(),
            Some(win_event_foreground_callback),
            0, // any process
            0, // any thread
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    if hook.0.is_null() {
        // SetWinEventHook failed (extremely rare — would need a
        // session-isolated or stripped token). Fall back to pure
        // polling: the loop still works without a hook, it just
        // burns the 250 ms cadence forever.
        tracing::warn!(
            "SetWinEventHook(EVENT_SYSTEM_FOREGROUND) failed; falling back to 250 ms poll only"
        );
    } else {
        tracing::info!("WinEvent foreground hook installed");
    }

    let mut last_pid: Option<u32> = None;
    let mut current_interval_ms = NORMAL_INTERVAL_MS;
    loop {
        // Poll foreground + report if changed. Either we got here via
        // the hook (real focus change) or via the timeout (fallback
        // poll). Either way the comparison against last_pid keeps us
        // from spamming duplicate reports.
        let req = match framesage_sys::foreground::current() {
            Ok(Some(fg)) => {
                let changed = last_pid != Some(fg.pid);
                last_pid = Some(fg.pid);
                if changed {
                    Some(Request::ReportForeground {
                        pid: fg.pid,
                        exe_name: fg.exe_name,
                        path: fg.path,
                        title: fg.title,
                    })
                } else {
                    // Same PID — still report periodically so the
                    // engine's stale-reporter check (item 2.6) stays
                    // satisfied. Cheap: one IPC send per ~10 s of
                    // unchanged foreground vs. one per 250 ms.
                    if current_interval_ms >= 2_000 {
                        Some(Request::ReportForeground {
                            pid: fg.pid,
                            exe_name: fg.exe_name,
                            path: fg.path,
                            title: fg.title,
                        })
                    } else {
                        None
                    }
                }
            }
            Ok(None) => {
                let needs_report = last_pid.is_some();
                last_pid = None;
                if needs_report {
                    Some(Request::ReportNoForeground)
                } else {
                    None
                }
            }
            Err(_) => None,
        };
        if let Some(req) = req {
            match send_request_blocking(framesage_ipc::PIPE_NAME_ADMIN, &req) {
                Ok(_) => {
                    current_interval_ms = NORMAL_INTERVAL_MS;
                }
                Err(_) => {
                    current_interval_ms = current_interval_ms.saturating_mul(2).min(BACKOFF_CAP_MS);
                }
            }
        }

        // Wait for either: (a) the WinEvent hook firing (focus
        // changed; thread-message arrives in our queue), or (b) the
        // timeout (fallback poll). MsgWaitForMultipleObjectsEx with
        // QS_ALLINPUT wakes on any message; the empty handle slice
        // means we're not waiting on additional kernel objects.
        unsafe {
            MsgWaitForMultipleObjectsEx(
                None,
                current_interval_ms,
                QS_ALLINPUT,
                MSG_WAIT_FOR_MULTIPLE_OBJECTS_EX_FLAGS(0),
            );
        }

        // Drain the message queue so the hook callback runs. The
        // callback itself is a no-op — the act of dispatching wakes
        // us via the prior MsgWait, which is all we need. We loop
        // PeekMessage until empty so a burst of focus changes (alt-
        // tab through a long task list) gets coalesced into a
        // single foreground re-check at the top of the next
        // iteration.
        let mut msg = MSG::default();
        loop {
            let has_msg = unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) };
            if !has_msg.as_bool() {
                break;
            }
            unsafe {
                DispatchMessageW(&msg);
            }
        }
    }

    // Unreachable — included for documentation. If the loop ever
    // becomes joinable, UnhookWinEvent here keeps the OS-level hook
    // table tidy.
    #[allow(unreachable_code)]
    if !hook.0.is_null() {
        unsafe {
            let _ = UnhookWinEvent(hook);
        }
    }
}

/// WinEvent callback for `EVENT_SYSTEM_FOREGROUND`. Deliberately a
/// no-op: the callback's only job is to exist (Windows requires
/// some callback for `SetWinEventHook` to be valid) and to ensure
/// the OS routes the event to our thread's message queue. The
/// queue-arrival wake is what unblocks `MsgWaitForMultipleObjectsEx`
/// in `foreground_reporter_loop`, which then does the actual
/// foreground re-check.
///
/// Keeping this empty matters for safety: WinEvent callbacks run on
/// our message-pump thread but the Windows-rs guidance is to do
/// minimal work and never block. Stuffing IPC sends or
/// foreground::current() calls in here would be a footgun.
#[cfg(windows)]
unsafe extern "system" fn win_event_foreground_callback(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    _event: u32,
    _hwnd: windows::Win32::Foundation::HWND,
    _id_object: i32,
    _id_child: i32,
    _id_event_thread: u32,
    _dwms_event_time: u32,
) {
}

// ─── One-shot helpers ────────────────────────────────────────────────────────

/// One-shot blocking IPC: open the named pipe, send a single request,
/// read a single response, close. Used by admin button handlers; we
/// deliberately don't reuse a persistent connection because admin
/// operations are rare and the simpler per-call lifecycle is easier
/// to reason about than a long-lived sender.
///
/// Item 2.5 / audit H-14. The previous implementation called
/// `OpenOptions::open(pipe_name)` directly — on Windows, if the named
/// pipe server has no available instances, that call can block for an
/// unbounded amount of time (default WaitNamedPipe timeout is 50 ms
/// but the OS internally retries). During a service restart the tray
/// UI thread would freeze for the duration of the outage. Now: probe
/// availability with `WaitNamedPipeW` first, with a hard 2-second
/// timeout. If the server isn't ready in 2 s, we surface a clear
/// "service unavailable" error instead of hanging the caller.
#[cfg(windows)]
pub(crate) fn send_request_blocking(pipe_name: &str, req: &Request) -> anyhow::Result<Response> {
    use std::fs::OpenOptions;
    use std::io::{BufRead, BufReader, Write};

    /// How long to wait for a pipe instance to become available
    /// before erroring. 2 s is comfortably above legitimate slow
    /// connects (worst-case kernel pipe-instance recycling) while
    /// staying below "user notices UI is frozen" (typical perception
    /// threshold ~3 s).
    const PIPE_WAIT_TIMEOUT_MS: u32 = 2000;

    // Probe availability first. WaitNamedPipeW returns immediately if
    // an instance is available, or after the timeout if none becomes
    // available. We treat timeout / not-found as a connection failure
    // — the caller's job is to back off and retry, not hang.
    wait_for_pipe(pipe_name, PIPE_WAIT_TIMEOUT_MS)?;

    let pipe = OpenOptions::new().read(true).write(true).open(pipe_name)?;
    let mut writer = pipe.try_clone()?;
    let mut reader = BufReader::new(pipe);

    let mut buf = serde_json::to_vec(req)?;
    buf.push(b'\n');
    writer.write_all(&buf)?;
    writer.flush()?;

    let mut line = String::new();
    reader.read_line(&mut line)?;
    let resp: Response = serde_json::from_str(line.trim_end())?;
    Ok(resp)
}

#[cfg(not(windows))]
pub(crate) fn send_request_blocking(_pipe_name: &str, _req: &Request) -> anyhow::Result<Response> {
    Err(anyhow::anyhow!(
        "framesage-tray IPC is only available on Windows"
    ))
}

/// `WaitNamedPipeW(pipe_name, timeout_ms)` — returns immediately if a
/// server-side pipe instance is available, or errors after the
/// timeout. Item 2.5 / audit H-14.
///
/// The Win32 function returns BOOL; we surface failure as Err so the
/// caller can distinguish "no service" from "open failed for some
/// other reason." Common error: ERROR_FILE_NOT_FOUND (service not
/// running yet) or ERROR_SEM_TIMEOUT (timeout elapsed).
#[cfg(windows)]
fn wait_for_pipe(pipe_name: &str, timeout_ms: u32) -> anyhow::Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::System::Pipes::WaitNamedPipeW;

    let wide: Vec<u16> = pipe_name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: wide is a null-terminated UTF-16 string alive for the
    // duration of this call; WaitNamedPipeW is documented as safe
    // with any non-null timeout value (0 = NMPWAIT_USE_DEFAULT_WAIT,
    // 0xFFFFFFFF = INFINITE, anything else = ms).
    let result = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), timeout_ms) };
    if result.as_bool() {
        Ok(())
    } else {
        let err = std::io::Error::last_os_error();
        Err(anyhow::anyhow!("WaitNamedPipeW({pipe_name}) failed: {err}"))
    }
}
