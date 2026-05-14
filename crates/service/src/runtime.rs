//! Service runtime: spins up the engine, the named-pipe server, and the tick
//! loop, and shuts them down cleanly when the SCM (or Ctrl+C in console mode)
//! signals stop.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use tracing::{debug, error, info};

use framesage_core::Policy;
use framesage_engine::Engine;
use framesage_ipc::{Event, Request, Response, PIPE_NAME};

/// Synchronous entry point used by the Windows service main fn. Owns its
/// tokio runtime so the SCM thread can block on it.
#[cfg(windows)]
pub fn run_blocking(shutdown: oneshot::Receiver<()>) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .context("build tokio runtime")?
        .block_on(run(shutdown))
}

/// Async entry point. Used in console mode (`--console`) where the caller
/// already has a runtime.
pub async fn run(shutdown: oneshot::Receiver<()>) -> Result<()> {
    let policy = load_policy().unwrap_or_default();
    let topology = detect_topology()?;
    info!(
        cpus = topology.count(),
        rules = policy.rules.len(),
        "framesage engine starting"
    );

    let engine = Arc::new(Engine::new(policy, topology));
    let tick_engine = engine.clone();
    let tick_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(300));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(e) = tick_engine.tick() {
                debug!(error = %e, "tick error");
            }
        }
    });

    let ipc_engine = engine.clone();
    let ipc_handle = tokio::spawn(async move {
        if let Err(e) = serve_ipc(ipc_engine).await {
            error!(error = %e, "ipc server stopped");
        }
    });

    // Wait for shutdown. We don't try to drain in-flight IPC connections —
    // they get cancelled when the task is dropped.
    let _ = shutdown.await;
    info!("shutdown requested");
    tick_handle.abort();
    ipc_handle.abort();
    Ok(())
}

fn load_policy() -> Option<Policy> {
    // TODO(v0.2): load from %ProgramData%\framesage\policy.json.
    None
}

#[cfg(windows)]
fn detect_topology() -> Result<framesage_core::CpuTopology> {
    framesage_sys::topology::detect()
}

#[cfg(not(windows))]
fn detect_topology() -> Result<framesage_core::CpuTopology> {
    // Console mode on a non-Windows dev box: synthesise a 16-thread topology
    // so the engine can be exercised end-to-end without the actual OS.
    use framesage_core::{CoreKind, CpuTopology, LogicalCpu};
    let mut cpus = Vec::new();
    for core in 0..8u32 {
        let ccd = if core < 4 { 0 } else { 1 };
        let kind = if ccd == 0 {
            CoreKind::Cache
        } else {
            CoreKind::Performance
        };
        for smt in 0..2u32 {
            cpus.push(LogicalCpu {
                index: core * 2 + smt,
                physical_core: core,
                ccd,
                kind,
                cppc_rank: Some(100 - core),
                is_smt_sibling: smt == 1,
            });
        }
    }
    Ok(CpuTopology { cpus })
}

#[cfg(windows)]
async fn serve_ipc(engine: Arc<Engine>) -> Result<()> {
    use tokio::net::windows::named_pipe::{PipeMode, ServerOptions};

    // ServerOptions on Windows lets us create a security-attributed pipe.
    // Default ACL allows Administrators + LocalSystem. We deliberately do NOT
    // open it to Everyone — only elevated tools should be able to reconfigure
    // the engine. The tray app needs to be launched with the same user as the
    // service or via a UAC prompt to administer; status read-only access is
    // future work (split read vs admin pipes).
    info!(pipe = %PIPE_NAME, "ipc server listening");

    loop {
        let server = ServerOptions::new()
            .pipe_mode(PipeMode::Byte)
            .first_pipe_instance(false)
            .create(PIPE_NAME)
            .with_context(|| format!("create named pipe {PIPE_NAME}"))?;

        server
            .connect()
            .await
            .context("accept named pipe client")?;

        let engine = engine.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(server, engine).await {
                debug!(error = %e, "client connection ended");
            }
        });
    }
}

#[cfg(not(windows))]
async fn serve_ipc(_engine: Arc<Engine>) -> Result<()> {
    // No named pipes off Windows. Console mode loses the IPC plane; the engine
    // still ticks against the synthetic topology so we can validate state
    // machines without the actual OS.
    info!("ipc server disabled on non-Windows host");
    futures_pending().await
}

#[cfg(not(windows))]
async fn futures_pending() -> ! {
    let () = std::future::pending().await;
    unreachable!()
}

#[cfg(windows)]
async fn handle_client(
    stream: tokio::net::windows::named_pipe::NamedPipeServer,
    engine: Arc<Engine>,
) -> Result<()> {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half).lines();

    while let Some(line) = reader.next_line().await? {
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = Response::Error {
                    message: format!("parse: {e}"),
                };
                write_response(&mut write_half, &resp).await?;
                continue;
            }
        };

        match req {
            Request::Status => {
                let snap = engine.status();
                write_response(&mut write_half, &Response::Status(snap)).await?;
            }
            Request::SetPolicy { policy } => {
                engine.set_policy(policy);
                write_response(&mut write_half, &Response::Ok).await?;
            }
            Request::ApplyOnce { profile: _ } => {
                // TODO(v0.2): one-shot profile apply on the current foreground.
                write_response(&mut write_half, &Response::Ok).await?;
            }
            Request::Pause => {
                engine.pause();
                write_response(&mut write_half, &Response::Ok).await?;
            }
            Request::Resume => {
                engine.resume();
                write_response(&mut write_half, &Response::Ok).await?;
            }
            Request::Subscribe => {
                write_response(&mut write_half, &Response::Ok).await?;
                let mut rx = engine.subscribe();
                while let Ok(event) = rx.recv().await {
                    write_event(&mut write_half, &event).await?;
                }
                break;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
async fn write_response<W>(w: &mut W, resp: &Response) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let mut line = serde_json::to_vec(resp)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(windows)]
async fn write_event<W>(w: &mut W, ev: &Event) -> Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    let mut line = serde_json::to_vec(ev)?;
    line.push(b'\n');
    w.write_all(&line).await?;
    w.flush().await?;
    Ok(())
}

// Silence warnings about unused imports/items on non-Windows.
#[cfg(not(windows))]
#[allow(dead_code)]
fn _silence_warnings() {
    let _ = (
        load_policy,
        std::any::type_name::<Engine>,
        std::any::type_name::<Request>,
        std::any::type_name::<Response>,
        std::any::type_name::<Event>,
        std::any::type_name::<BufReader<tokio::io::DuplexStream>>,
        std::any::type_name::<AsyncBufReadExt>,
        std::any::type_name::<AsyncWriteExt>,
    );
}
