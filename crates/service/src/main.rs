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
//!    - Shutdown listener (responds to SCM stop / Ctrl+C in console mode).

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use anyhow::Result;
use tracing::info;

#[cfg(windows)]
mod pipe;
mod runtime;

#[cfg(windows)]
mod service_main {
    use super::*;
    use std::ffi::OsString;
    use std::time::Duration;

    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
        ServiceType,
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

        let handler = move |control_event| -> ServiceControlHandlerResult {
            match control_event {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    if let Some(tx) = shutdown_tx.take() {
                        let _ = tx.send(());
                    }
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };

        let status_handle = service_control_handler::register(SERVICE_NAME, handler)?;

        status_handle.set_service_status(ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        })?;

        super::runtime::run_blocking(shutdown_rx)?;

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
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("framesage=info,info"));
    let _ = fmt().with_env_filter(filter).try_init();
}

fn main() -> Result<()> {
    init_tracing();

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
                r = runtime::run(shutdown_rx) => r,
                _ = tokio::signal::ctrl_c() => Ok(()),
            }
        });

    ctrl_c
}
