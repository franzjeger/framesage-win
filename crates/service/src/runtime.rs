//! Service runtime: spins up the engine, the named-pipe server, and the tick
//! loop, and shuts them down cleanly when the SCM (or Ctrl+C in console mode)
//! signals stop.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use tracing::{debug, error, info, warn};

use framesage_core::{paths, Policy};
use framesage_engine::{Engine, EngineDeps};
use framesage_gamemode::{journal::Journal, safe_list::SafeList};
use framesage_ipc::{Event, Request, Response, PIPE_NAME_ADMIN, PIPE_NAME_STATUS};

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
    let policy_path = paths::policy_path();
    let policy = load_policy_or_default(&policy_path);
    let topology = detect_topology()?;
    info!(
        cpus = topology.count(),
        rules = policy.rules.len(),
        path = %policy_path.display(),
        "framesage engine starting"
    );

    let engine = Arc::new(Engine::new(EngineDeps {
        policy,
        topology,
        safe_list: SafeList::bundled(),
        journal: Journal::at_default_path(),
    }));

    // Recover anything a previous (possibly crashed) session left behind
    // before we start applying new state. This MUST happen before the tick
    // loop launches.
    engine.recover_orphan_journal();
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

    // Two IPC servers: one admin pipe (default Windows ACL — Administrators
    // + LocalSystem only) and one status pipe (permissive ACL via SDDL so
    // the unprivileged tray UI can connect). The handler enforces "status
    // pipe accepts only read-only requests" regardless of caller identity.
    let admin_engine = engine.clone();
    let admin_handle = tokio::spawn(async move {
        if let Err(e) = serve_ipc(admin_engine, PipeKind::Admin).await {
            error!(error = %e, "admin ipc server stopped");
        }
    });

    let status_engine = engine.clone();
    let status_handle = tokio::spawn(async move {
        if let Err(e) = serve_ipc(status_engine, PipeKind::Status).await {
            error!(error = %e, "status ipc server stopped");
        }
    });

    let reload_engine = engine.clone();
    let reload_path = policy_path.clone();
    let reload_handle = tokio::spawn(async move {
        if let Err(e) = watch_policy(reload_path, reload_engine).await {
            warn!(error = %e, "policy watcher stopped");
        }
    });

    // Wait for shutdown. We don't try to drain in-flight IPC connections —
    // they get cancelled when the task is dropped.
    let _ = shutdown.await;
    info!("shutdown requested");
    tick_handle.abort();
    admin_handle.abort();
    status_handle.abort();
    reload_handle.abort();
    Ok(())
}

/// Which named pipe a handler is currently serving on. Drives ACL-layer
/// rejection of non-read-only requests on the status pipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(windows)]
enum PipeKind {
    /// Default Windows ACL — Administrators + LocalSystem. Accepts every
    /// request type.
    Admin,
    /// Permissive ACL (Authenticated Users). Only read-only requests are
    /// honoured; anything else replies with an error rather than executing.
    Status,
}

fn load_policy_or_default(path: &std::path::Path) -> Policy {
    match Policy::load_or_create_default(path) {
        Ok(p) => p,
        Err(e) => {
            warn!(
                error = %e,
                "failed to load or create policy file — using built-in defaults (not persisted)"
            );
            Policy::default()
        }
    }
}

/// File-system watcher that hot-reloads the policy when `policy.json` changes
/// on disk. Debounces bursts: editors (VS Code, Notepad) often emit several
/// events for a single save. We coalesce by collecting events into a 250 ms
/// window and applying at most once per window.
async fn watch_policy(path: PathBuf, engine: Arc<Engine>) -> Result<()> {
    use notify::{RecommendedWatcher, RecursiveMode, Watcher};

    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("policy path has no parent"))?
        .to_path_buf();

    // notify's callback runs on its own thread; bridge into tokio via an
    // unbounded mpsc channel.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let tx_clone = tx.clone();
    let mut watcher: RecommendedWatcher =
        notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
            Ok(_event) => {
                let _ = tx_clone.send(());
            }
            Err(e) => debug!(error = %e, "notify error"),
        })
        .context("create watcher")?;

    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("watch {}", dir.display()))?;

    info!(path = %path.display(), "policy hot-reload watcher active");

    loop {
        // Block for the first event…
        if rx.recv().await.is_none() {
            break;
        }
        // …then drain any further events for 250 ms (coalesce).
        let debounce = tokio::time::sleep(Duration::from_millis(250));
        tokio::pin!(debounce);
        loop {
            tokio::select! {
                _ = &mut debounce => break,
                Some(_) = rx.recv() => continue,
            }
        }
        // Only reload if the target file actually exists. Other files in the
        // dir change (e.g. tmp swap files) — ignore those, we just react to
        // any signal as a hint to re-check.
        if !path.exists() {
            continue;
        }
        match Policy::load(&path) {
            Ok(new_policy) => {
                info!(
                    rules = new_policy.rules.len(),
                    profiles = new_policy.profiles.len(),
                    "policy reloaded"
                );
                engine.set_policy(new_policy);
            }
            Err(e) => warn!(error = %e, "policy reload failed; keeping previous"),
        }
    }
    drop(watcher); // keep alive until the loop exits
    Ok(())
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
                l3_cache_bytes: None,
                is_smt_sibling: smt == 1,
            });
        }
    }
    Ok(CpuTopology { cpus })
}

#[cfg(windows)]
async fn serve_ipc(engine: Arc<Engine>, kind: PipeKind) -> Result<()> {
    let pipe_name = match kind {
        PipeKind::Admin => PIPE_NAME_ADMIN,
        PipeKind::Status => PIPE_NAME_STATUS,
    };
    info!(pipe = %pipe_name, kind = ?kind, "ipc server listening");

    // Pre-create the FIRST instance with FILE_FLAG_FIRST_PIPE_INSTANCE to
    // defeat name squatting. Each loop iteration then creates the NEXT
    // instance BEFORE handing the current one off — this closes a
    // microsecond gap that hits clients connecting in the window between
    // accept and re-create as ERROR_PIPE_BUSY. The tray's foreground
    // reporter connects every 250ms; under that load the gap was hit
    // often enough to make `framesage status` and concurrent IPC calls
    // intermittently fail.
    let mut next_server = match kind {
        PipeKind::Admin => crate::pipe::create_admin_pipe(pipe_name, true)?,
        PipeKind::Status => crate::pipe::create_status_pipe(pipe_name, true)?,
    };
    loop {
        // Move the listener we're about to accept on into `current`,
        // immediately stand up its replacement.
        let current = next_server;
        next_server = match kind {
            PipeKind::Admin => crate::pipe::create_admin_pipe(pipe_name, false)?,
            PipeKind::Status => crate::pipe::create_status_pipe(pipe_name, false)?,
        };

        current
            .connect()
            .await
            .context("accept named pipe client")?;

        let engine = engine.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_client(current, engine, kind).await {
                debug!(error = %e, "client connection ended");
            }
        });
    }
}

#[cfg(not(windows))]
#[derive(Debug, Clone, Copy)]
enum PipeKind {
    Admin,
    Status,
}

#[cfg(not(windows))]
async fn serve_ipc(_engine: Arc<Engine>, _kind: PipeKind) -> Result<()> {
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
    kind: PipeKind,
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

        // Defense in depth: even though the status pipe's OS-layer ACL is
        // permissive, the IPC handler still rejects any mutating request on
        // it. A misrouted client (or a future bug that opens the status
        // pipe for a mutator) gets a clean error rather than executing.
        if kind == PipeKind::Status && !req.is_read_only() {
            let resp = Response::Error {
                message: format!(
                    "{:?} requires the admin pipe ({})",
                    std::mem::discriminant(&req),
                    framesage_ipc::PIPE_NAME_ADMIN
                ),
            };
            write_response(&mut write_half, &resp).await?;
            continue;
        }

        match req {
            Request::Status => {
                let snap = engine.status();
                write_response(&mut write_half, &Response::Status(Box::new(snap))).await?;
            }
            Request::ListProcesses => {
                let snapshots = engine.list_process_snapshots();
                write_response(&mut write_half, &Response::Processes { snapshots }).await?;
            }
            Request::SetPolicy { policy } => {
                // Apply in-memory first so subsequent ticks see the change
                // immediately, then persist to disk so the edit survives
                // service restart. The FS watcher will fire a redundant
                // reload from the disk write — benign because `set_policy`
                // is idempotent on identical content.
                //
                // Crucially, if the disk save fails (most common cause: the
                // service is running unelevated and can't write the
                // SYSTEM-owned `C:\ProgramData\framesage\policy.json`), we
                // surface that as a Response::Error so the tray's
                // last_action banner shows the user what went wrong. The
                // previous behaviour — warn-log and return Ok — meant edits
                // silently evaporated on service restart, which is the worst
                // possible failure mode for a config UI.
                engine.set_policy(policy.clone());
                match policy.save(&paths::policy_path()) {
                    Ok(()) => {
                        write_response(&mut write_half, &Response::Ok).await?;
                    }
                    Err(e) => {
                        warn!(error = %e, "policy save after SetPolicy failed");
                        write_response(
                            &mut write_half,
                            &Response::Error {
                                message: format!(
                                    "policy.json save failed: {e}. Edit applied in memory \
                                     but will be lost on service restart. \
                                     Install the service elevated so it can write \
                                     C:\\ProgramData\\framesage\\policy.json."
                                ),
                            },
                        )
                        .await?;
                    }
                }
            }
            Request::ApplyOnce { profile } => match engine.apply_once(profile) {
                Ok(()) => {
                    write_response(&mut write_half, &Response::Ok).await?;
                }
                Err(e) => {
                    write_response(
                        &mut write_half,
                        &Response::Error {
                            message: format!("apply_once failed: {e:#}"),
                        },
                    )
                    .await?;
                }
            },
            Request::SetManualOverride { profile } => match engine.set_manual_override(profile) {
                Ok(()) => {
                    write_response(&mut write_half, &Response::Ok).await?;
                }
                Err(e) => {
                    write_response(
                        &mut write_half,
                        &Response::Error {
                            message: format!("set_manual_override failed: {e:#}"),
                        },
                    )
                    .await?;
                }
            },
            Request::ClearManualOverride => {
                engine.clear_manual_override();
                write_response(&mut write_half, &Response::Ok).await?;
            }
            Request::ReportForeground {
                pid,
                exe_name,
                path,
                title,
            } => {
                engine.report_foreground(pid, exe_name, path, title);
                write_response(&mut write_half, &Response::Ok).await?;
            }
            Request::ReportNoForeground => {
                engine.report_no_foreground();
                write_response(&mut write_half, &Response::Ok).await?;
            }
            Request::Pause => {
                engine.pause();
                write_response(&mut write_half, &Response::Ok).await?;
            }
            Request::GameModeOff => {
                engine.exit_system_mode_now();
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
