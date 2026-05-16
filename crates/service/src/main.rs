//! framesage-svc.exe — the Windows service host.
//!
//! Runs as LocalSystem, owns the policy engine, and serves clients (tray, CLI)
//! over a named pipe. The whole process is structured as:
//!
//! 1. Service control dispatcher hands us a service main fn (Windows side).
//! 2. We spin up a tokio runtime and a `framesage_engine::Engine`.
//! 3. Three tasks run concurrently:
//!    - Tick loop (calls `engine.tick()` on the policy's cadence).
//!    - IPC server (accepts named-pipe clients and dispatches `Request`s).
//!
//! # Layering (item 3.8)
//!
//! The ONLY crate that depends on `framesage-engine`. Service is the
//! engine's host process; everything else (CLI, tray) interacts via the
//! `framesage-ipc` named-pipe protocol. See `ARCHITECTURE.md` at the repo
//! root.
//!    - Shutdown listener (responds to SCM stop / Ctrl+C in console mode).

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use anyhow::Result;
use tracing::info;

#[cfg(windows)]
mod acl;
#[cfg(windows)]
mod pipe;
mod runtime;

#[cfg(windows)]
mod service_main {
    use super::*;
    use std::ffi::OsString;
    use std::time::Duration;

    use framesage_engine::SystemEvent;
    use windows_service::service::{
        PowerEventParam, ServiceControl, ServiceControlAccept, ServiceExitCode,
        SessionChangeReason, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_dispatcher;

    pub const SERVICE_NAME: &str = "framesage";
    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    windows_service::define_windows_service!(ffi_service_main, service_main);

    pub fn run() -> Result<()> {
        // Hands off to the SCM. Returns when SCM tells us we're done.
        service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
        Ok(())
    }

    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = service_loop() {
            tracing::error!("service exited with error: {e:#}");
        }
    }

    fn service_loop() -> Result<()> {
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let mut shutdown_tx = Some(shutdown_tx);

        // Item 2.4 / audit M-02 — system-events channel. The SCM
        // handler runs synchronously and must return within a few ms or
        // Windows kills the service; sending into an unbounded mpsc is
        // a non-blocking ~lock-free push, well within budget. The
        // runtime drains it in a dedicated tokio task.
        let (sys_events_tx, sys_events_rx) = tokio::sync::mpsc::unbounded_channel::<SystemEvent>();

        let handler = move |control_event| -> ServiceControlHandlerResult {
            match control_event {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    if let Some(tx) = shutdown_tx.take() {
                        let _ = tx.send(());
                    }
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                ServiceControl::PowerEvent(param) => {
                    if let Some(ev) = power_event_to_system_event(param) {
                        let _ = sys_events_tx.send(ev);
                    }
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::SessionChange(param) => {
                    if let Some(ev) = session_change_to_system_event(param.reason) {
                        let _ = sys_events_tx.send(ev);
                    }
                    ServiceControlHandlerResult::NoError
                }
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };

        let status_handle = service_control_handler::register(SERVICE_NAME, handler)?;

        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP
                | ServiceControlAccept::SHUTDOWN
                | ServiceControlAccept::POWER_EVENT
                | ServiceControlAccept::SESSION_CHANGE,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        super::runtime::run_blocking(super::runtime::RuntimeInputs {
            shutdown: shutdown_rx,
            system_events: Some(sys_events_rx),
        })?;

        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        Ok(())
    }

    /// Map a SCM PowerEvent payload to the engine's abstract
    /// `SystemEvent`. Returns `None` for payload variants we don't
    /// care about (battery-low warnings, OEM events, power-status
    /// changes that aren't transitions). Item 2.4 / audit M-02.
    fn power_event_to_system_event(param: PowerEventParam) -> Option<SystemEvent> {
        match param {
            // System is about to enter low-power state.
            PowerEventParam::Suspend => Some(SystemEvent::Suspend),
            // System resumed. PBT_APMRESUMEAUTOMATIC fires on every resume
            // (timer or user-initiated); PBT_APMRESUMESUSPEND fires only
            // when user input triggered the resume. We treat them
            // identically — the engine wants to re-probe AC presence
            // and force foreground re-check either way.
            PowerEventParam::ResumeAutomatic | PowerEventParam::ResumeSuspend => {
                Some(SystemEvent::Resume)
            }
            // PBT_APMRESUMECRITICAL: the system resumed without
            // notifying anyone of the suspend (battery died, hard
            // reset). State is unreliable; treat as resume.
            PowerEventParam::ResumeCritical => Some(SystemEvent::Resume),
            // Everything else is informational (battery low, status
            // change, query-suspend handshakes) — no engine action.
            _ => None,
        }
    }

    /// Map a SCM SessionChange payload to the engine's abstract
    /// `SystemEvent`. Item 2.4 / audit M-02.
    fn session_change_to_system_event(reason: SessionChangeReason) -> Option<SystemEvent> {
        match reason {
            // Console session connected (FUS to this user, RDP login).
            // RemoteConnect is the RDP version of the same idea.
            SessionChangeReason::ConsoleConnect | SessionChangeReason::RemoteConnect => {
                Some(SystemEvent::SessionConsoleConnect)
            }
            // Console session disconnected (FUS away, RDP logoff
            // without closing). The user we were optimising for is no
            // longer in front of the screen.
            SessionChangeReason::ConsoleDisconnect | SessionChangeReason::RemoteDisconnect => {
                Some(SystemEvent::SessionConsoleDisconnect)
            }
            SessionChangeReason::SessionLock => Some(SystemEvent::SessionLock),
            SessionChangeReason::SessionUnlock => Some(SystemEvent::SessionUnlock),
            // Logon/logoff/create/terminate fire for system-init
            // sequences and FUS, but the engine's per-user state lives
            // in the console-session lifecycle, not the underlying
            // session-create/destroy events.
            _ => None,
        }
    }
}

/// Item 2.7 / audit H-29. Initialize tracing with a rolling file
/// appender writing to `%ProgramData%\framesage\logs\`. Daily rotation;
/// no retention cap built into `tracing-appender` (it just rotates,
/// doesn't prune) but the directory only accumulates rolled files —
/// each is small enough that years of operation stays well under any
/// reasonable disk budget.
///
/// Stderr/stdout output is preserved (still wired to the fmt subscriber
/// when running in `--console` mode) so a dev running console-mode
/// still gets immediate log output; the file sink is additive.
///
/// File handle: held by a static `WorkerGuard` returned to `main` so
/// the appender's background-flush thread stays alive for the
/// program's lifetime.
fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("framesage=info,info"));

    // Try to set up the file appender. On failure (dir not writable —
    // typical when running as a non-admin user before service install,
    // or in `--console` mode under a restricted token) fall back to
    // stderr-only so the binary still works.
    let logs_dir = framesage_core::paths::config_dir().join("logs");
    if let Err(e) = std::fs::create_dir_all(&logs_dir) {
        eprintln!(
            "framesage-svc: log directory create failed at {}: {e}. \
             Falling back to stderr-only logging.",
            logs_dir.display()
        );
        let _ = fmt().with_env_filter(filter).try_init();
        return None;
    }

    let file_appender = tracing_appender::rolling::daily(&logs_dir, "framesage-svc.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let file_layer = fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false) // no ANSI codes in files
        .with_target(true);
    let stderr_layer = fmt::layer().with_writer(std::io::stderr);

    let init_result = tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .try_init();

    if init_result.is_err() {
        // try_init only fails if a subscriber's already installed —
        // shouldn't happen in our process model, but if it does, the
        // existing subscriber is already serving logs; just drop the
        // guard via early return.
        return None;
    }
    Some(guard)
}

fn main() -> Result<()> {
    // Hold the log-flusher guard for the program's lifetime. If the
    // appender failed to initialize (no write access to ProgramData),
    // _log_guard is None and tracing falls back to stderr-only.
    let _log_guard = init_tracing();

    // If we're launched outside the SCM (e.g. for development), fall back to
    // running the engine inline. Detect "is this an SCM start?" by checking
    // the standard env var Windows sets. On non-Windows we just run inline.
    let console_mode = std::env::args().any(|a| a == "--console")
        || std::env::var_os("framesage_CONSOLE").is_some();

    #[cfg(windows)]
    if !console_mode {
        return service_main::run();
    }

    info!("starting framesage in console mode");
    let (_shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

    let ctrl_c = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            tokio::select! {
                r = runtime::run(runtime::RuntimeInputs::shutdown_only(shutdown_rx)) => r,
                _ = tokio::signal::ctrl_c() => Ok(()),
            }
        });

    ctrl_c
}
