//! Service runtime: spins up the engine, the named-pipe server, and the tick
//! loop, and shuts them down cleanly when the SCM (or Ctrl+C in console mode)
//! signals stop.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use framesage_core::{paths, Policy};
use framesage_engine::{Engine, EngineDeps, SystemEvent};
use framesage_gamemode::{journal::Journal, safe_list::SafeList};
use framesage_ipc::{Event, Request, Response, PIPE_NAME_ADMIN, PIPE_NAME_STATUS};

/// Optional input channels into the runtime. Item 2.4 / audit M-02:
/// the SCM service-control handler dispatches PowerEvent /
/// SessionChange via `system_events`; in `--console` mode the caller
/// passes `None` and the engine simply doesn't react to suspend/
/// session events (consoles run interactively, the user is at the
/// keyboard).
pub struct RuntimeInputs {
    pub shutdown: oneshot::Receiver<()>,
    pub system_events: Option<mpsc::UnboundedReceiver<SystemEvent>>,
}

impl RuntimeInputs {
    /// Convenience for console mode where we only have a shutdown signal.
    pub fn shutdown_only(shutdown: oneshot::Receiver<()>) -> Self {
        Self {
            shutdown,
            system_events: None,
        }
    }
}

/// Synchronous entry point used by the Windows service main fn. Owns its
/// tokio runtime so the SCM thread can block on it.
#[cfg(windows)]
pub fn run_blocking(inputs: RuntimeInputs) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .context("build tokio runtime")?
        .block_on(run(inputs))
}

/// Async entry point. Used in console mode (`--console`) where the caller
/// already has a runtime.
pub async fn run(inputs: RuntimeInputs) -> Result<()> {
    let RuntimeInputs {
        shutdown,
        system_events,
    } = inputs;
    let policy_path = paths::policy_path();

    // Item 1.2 / audit C-04 — harden the config dir's DACL before any
    // policy load. Stops the inherited-CREATOR_OWNER vulnerability where a
    // non-admin who created `%ProgramData%\framesage\` first (e.g. via a
    // `framesage-svc --console` run before installation) keeps modify
    // rights on policy.json forever. SetNamedSecurityInfoW from
    // LocalSystem overwrites whatever DACL was there.
    //
    // Two failure modes both result in continuing with a warning, not
    // refusing to start:
    //  - We're running in console mode under a non-admin user → we don't
    //    have SeTakeOwnership; hardening fails. Dev is responsible for
    //    their own posture; the verify-owner check below will catch it.
    //  - The dir is on a network drive or some FS that doesn't support
    //    Windows ACLs → ditto.
    //
    // The load-side `verify_owner_is_admin_or_system` is the real safety
    // gate: it refuses to trust the file if hardening didn't take.
    #[cfg(windows)]
    {
        let config_dir = paths::config_dir();
        if let Err(e) = crate::acl::harden_config_dir(&config_dir) {
            warn!(
                error = %e,
                "config dir hardening failed — likely running unelevated in console \
                 mode. policy.json owner will be verified before load; install via SCM \
                 for the production trust boundary."
            );
        }
    }

    let policy = load_policy_or_default(&policy_path);
    let topology = detect_topology()?;
    info!(
        cpus = topology.count(),
        rules = policy.rules.len(),
        path = %policy_path.display(),
        "framesage engine starting"
    );

    // Day 5 (v0.7 closed-loop): evaluate the policy + build-gate
    // decision tree and spawn supervisor + drop-poll tasks if enabled.
    // The closed-loop tasks intentionally do NOT participate in the
    // v0.6 watchdog select! below — per architecture §2.1 mode 5
    // amendment (proposal/v0.7-arch-mode5-amendment PR #77), supervisor
    // exit is not a critical service failure. See closed_loop.rs's
    // module docstring for the ownership rationale.
    // M1.2 / B-001 — start_closed_loop_if_enabled runs blocking work
    // (cleanup_stale_session → StartTraceW → std::thread spawn), so
    // park it on the blocking pool instead of a runtime worker thread.
    // Harmless at startup, but it makes a future "restart on policy
    // hot-reload flip" call site trivially correct. tokio::spawn
    // inside the closure still works — spawn_blocking preserves the
    // runtime context.
    // #8 — kernel_signal channel: the closed-loop drop-poll task is
    // the producer; the session recorder is the consumer. Broadcast so
    // future consumers (UI banner) can tap in without re-plumbing.
    let (kernel_signal_tx, kernel_signal_rx) =
        tokio::sync::broadcast::channel::<framesage_etw::KernelSignal>(64);
    let closed_loop_policy = policy.clone();
    let closed_loop_startup = tokio::task::spawn_blocking(move || {
        crate::closed_loop::start_closed_loop_if_enabled(&closed_loop_policy, kernel_signal_tx)
    })
    .await
    .unwrap_or_else(
        |join_err| crate::closed_loop::ClosedLoopStartup::StartupError {
            message: format!("closed-loop startup task panicked: {join_err}"),
        },
    );
    info!(
        startup_result = ?closed_loop_startup,
        "closed-loop startup decision made"
    );

    let engine = Arc::new(Engine::new(EngineDeps::with_real_sys(
        policy,
        topology,
        SafeList::bundled(),
        Journal::at_default_path(),
    )));

    // Recover anything a previous (possibly crashed) session left behind
    // before we start applying new state. This MUST happen before the tick
    // loop launches.
    engine.recover_orphan_journal();

    // #111 — PresentMon frame source. The manager spawns a real
    // PresentMon.exe against the foreground game while a closed-loop
    // session is active and forwards 1 Hz frame_sample buckets into the
    // recorder over this mpsc channel. It reports whether PresentMon.exe
    // is even present on disk, which becomes the recorder's honest
    // presentmon_state capability. Not in the watchdog select! either.
    let (frame_tx, frame_rx) = tokio::sync::mpsc::channel::<framesage_presentmon::FrameStats>(64);
    let (presentmon_available, _presentmon_mgr) =
        crate::presentmon::spawn(engine.clone(), frame_tx);

    // Honest capability stamp for every session_start: ETW is active when
    // the closed-loop drain actually started; PresentMon is active when a
    // real PresentMon.exe is available to attach.
    let caps = crate::session_recorder::SessionCapabilities {
        etw_active: matches!(
            closed_loop_startup,
            crate::closed_loop::ClosedLoopStartup::Running
        ),
        presentmon_active: presentmon_available,
    };

    // #110 drain worker — records Game Mode sessions to the sessions
    // dir when policy.closed_loop_enabled is on. Not in the watchdog
    // select! below: recorder death must never take the rule engine
    // down (same contract as the closed-loop tasks).
    let _session_recorder = crate::session_recorder::spawn(
        engine.clone(),
        paths::sessions_dir(),
        kernel_signal_rx,
        frame_rx,
        caps,
    );

    let tick_engine = engine.clone();
    let mut tick_handle = tokio::spawn(async move {
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
    let mut admin_handle = tokio::spawn(async move {
        if let Err(e) = serve_ipc(admin_engine, PipeKind::Admin).await {
            error!(error = %e, "admin ipc server stopped");
        }
    });

    let status_engine = engine.clone();
    let mut status_handle = tokio::spawn(async move {
        if let Err(e) = serve_ipc(status_engine, PipeKind::Status).await {
            error!(error = %e, "status ipc server stopped");
        }
    });

    let reload_engine = engine.clone();
    let reload_path = policy_path.clone();
    let mut reload_handle = tokio::spawn(async move {
        if let Err(e) = watch_policy(reload_path, reload_engine).await {
            warn!(error = %e, "policy watcher stopped");
        }
    });

    // Item 2.4 / audit M-02 — system-events handler task. Drains the
    // mpsc receiver, forwards each event to engine.handle_system_event.
    // In console mode `system_events` is None and this branch is
    // skipped; the watchdog `select!` below uses a permanently-pending
    // future for that slot so the task structure stays uniform.
    let sys_engine = engine.clone();
    let mut sys_handle = tokio::spawn(async move {
        let Some(mut rx) = system_events else {
            // Console mode: nothing to do. Park forever so the watchdog
            // doesn't think this task died.
            std::future::pending::<()>().await;
            return;
        };
        while let Some(ev) = rx.recv().await {
            sys_engine.handle_system_event(ev);
        }
        // Channel closed (SCM handler dropped sender) — exit cleanly.
        info!("system-events channel closed");
    });

    // Item 1.3 / audit C-06 — task watchdog. Wait for shutdown OR for any
    // critical task to die unexpectedly. The previous `let _ = shutdown.await`
    // pattern hid a real failure mode: if the tick task panicked or returned
    // early, the service's `Running` state survived because main was just
    // awaiting `shutdown`, leaving the user with a running-but-inert
    // service. With this select! pattern, any unexpected exit drops us out
    // of `run` with an Err so the SCM sees the service stop. Combined with
    // FailureActions configured at install time (item 1.3, cli/install_service),
    // SCM will restart the service within 5s.
    //
    // The four critical tasks are: tick (engine main loop), admin IPC,
    // status IPC, reload watcher. Any of them returning while shutdown is
    // not signalled means the service can't do its job — bail out and let
    // SCM restart us clean.
    let unexpected_exit: Option<&'static str> = tokio::select! {
        _ = shutdown => None,
        r = &mut tick_handle => Some(task_died_msg("tick", &r)),
        r = &mut admin_handle => Some(task_died_msg("admin-ipc", &r)),
        r = &mut status_handle => Some(task_died_msg("status-ipc", &r)),
        r = &mut reload_handle => Some(task_died_msg("policy-watcher", &r)),
        r = &mut sys_handle => Some(task_died_msg("system-events", &r)),
    };

    if let Some(name) = unexpected_exit {
        error!(
            task = %name,
            "critical task exited unexpectedly — returning Err so SCM can restart"
        );
    } else {
        info!("shutdown requested");
    }

    tick_handle.abort();
    admin_handle.abort();
    status_handle.abort();
    reload_handle.abort();
    sys_handle.abort();

    if let Some(name) = unexpected_exit {
        return Err(anyhow::anyhow!(
            "service exited because critical task {name} died; SCM should restart us"
        ));
    }
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
    // Defense-in-depth (item 1.2 / audit C-04): verify the file's owner
    // is SYSTEM or BUILTIN\Administrators before trusting its content.
    // If hardening failed earlier in startup (console mode, unsupported
    // FS) or someone managed to plant a file before we hardened, the
    // owner SID is the catch-net. Loading a user-owned policy.json
    // would mean an attacker with modify rights could plant arbitrary
    // AppRule entries that drive `apply_profile` under SYSTEM rights —
    // exactly the EoP primitive the audit identified.
    //
    // Owner check only fires if the file exists; load_or_create_default
    // handles the missing-file case by writing a fresh default (which
    // we just created in a dir we own, so the new file is also owned by
    // SYSTEM via the hardened DACL's inheritance).
    #[cfg(windows)]
    if path.exists() {
        if let Err(e) = crate::acl::verify_owner_is_admin_or_system(path) {
            warn!(
                path = %path.display(),
                error = %e,
                "policy file owner check failed — using built-in defaults; \
                 file contents will NOT be loaded until the file is owned \
                 by SYSTEM or Administrators. Re-install the service \
                 elevated to re-take ownership."
            );
            return Policy::default();
        }
    }

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

// ─── Subscriber caps (item 1.8 H-16 + M2.4 A-005) ─────────────────────────

/// Process-wide ceiling on concurrent Subscribe streams.
const MAX_SUBSCRIBERS_TOTAL: usize = 32;
/// Per-client-PID ceiling. One tray plus a few debug CLIs from the
/// same process never legitimately exceeds this; a client leaking
/// subscriptions hits its own cap without starving other PIDs.
const MAX_SUBSCRIBERS_PER_PID: usize = 8;

/// Sentinel PID used when `GetNamedPipeClientProcessId` fails — those
/// clients share one per-PID budget instead of bypassing the cap.
const UNKNOWN_CLIENT_PID: u32 = u32::MAX;

/// Why a Subscribe was refused. Debug-logged; `user_message` is the
/// IPC error surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubscribeDenied {
    TotalCap,
    PerPidCap,
}

impl SubscribeDenied {
    fn user_message(self) -> String {
        match self {
            SubscribeDenied::TotalCap => format!(
                "Subscribe rejected: maximum of {MAX_SUBSCRIBERS_TOTAL} concurrent \
                 subscribers reached — close another client first"
            ),
            SubscribeDenied::PerPidCap => format!(
                "Subscribe rejected: maximum of {MAX_SUBSCRIBERS_PER_PID} concurrent \
                 subscribers per client process reached — close another \
                 subscription from this process first"
            ),
        }
    }
}

/// M2.4 / A-005 — layered subscriber accounting keyed on client PID.
/// Platform-independent so the cap semantics are unit-testable off
/// Windows; the PID itself comes from `GetNamedPipeClientProcessId`
/// at accept time on the real pipe path.
struct SubscriberCaps {
    by_pid: std::sync::Mutex<std::collections::HashMap<u32, usize>>,
}

impl SubscriberCaps {
    fn new() -> Self {
        Self {
            by_pid: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Reserve a slot for `pid`. On `Err` nothing is reserved.
    fn try_acquire(&self, pid: u32) -> Result<(), SubscribeDenied> {
        let mut map = self.by_pid.lock().expect("subscriber cap lock poisoned");
        let total: usize = map.values().sum();
        if total >= MAX_SUBSCRIBERS_TOTAL {
            return Err(SubscribeDenied::TotalCap);
        }
        let count = map.entry(pid).or_insert(0);
        if *count >= MAX_SUBSCRIBERS_PER_PID {
            return Err(SubscribeDenied::PerPidCap);
        }
        *count += 1;
        Ok(())
    }

    /// Release a slot previously acquired for `pid`. Zeroed entries
    /// are removed so exited clients don't grow the map forever.
    fn release(&self, pid: u32) {
        let mut map = self.by_pid.lock().expect("subscriber cap lock poisoned");
        if let Some(count) = map.get_mut(&pid) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                map.remove(&pid);
            }
        }
    }
}

/// Global instance shared by every IPC connection task.
fn subscriber_caps() -> &'static SubscriberCaps {
    static CAPS: std::sync::OnceLock<SubscriberCaps> = std::sync::OnceLock::new();
    CAPS.get_or_init(SubscriberCaps::new)
}

/// M2.4 / A-005 — resolve the PID on the client end of the pipe.
/// `None` if the query fails (the caller buckets those under
/// `UNKNOWN_CLIENT_PID`).
#[cfg(windows)]
fn pipe_client_pid(stream: &tokio::net::windows::named_pipe::NamedPipeServer) -> Option<u32> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Pipes::GetNamedPipeClientProcessId;

    let mut pid: u32 = 0;
    // SAFETY: the handle comes from a live NamedPipeServer borrowed
    // for the duration of the call; GetNamedPipeClientProcessId only
    // reads it and writes the PID out-param.
    unsafe { GetNamedPipeClientProcessId(HANDLE(stream.as_raw_handle()), &mut pid) }.ok()?;
    Some(pid)
}

#[cfg(windows)]
async fn handle_client(
    stream: tokio::net::windows::named_pipe::NamedPipeServer,
    engine: Arc<Engine>,
    kind: PipeKind,
) -> Result<()> {
    // Capture the client PID while we still hold the unsplit stream;
    // the Subscribe cap below is keyed on it.
    let client_pid = pipe_client_pid(&stream).unwrap_or(UNKNOWN_CLIENT_PID);
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half).lines();

    while let Some(line) = reader.next_line().await? {
        // Item 1.8 / audit H-15. Cap per-line size at 1 MB to defeat
        // "send a multi-GB JSON line and watch the LocalSystem service
        // OOM" attacks. Legitimate `SetPolicy` payloads are <100 KB;
        // legitimate everything-else is <1 KB. 1 MB is generous headroom
        // without leaving an attack window.
        //
        // The kernel pipe buffer (64 KB default) already throttles
        // attackers' throughput, but doesn't bound our Vec growth — a
        // patient attacker could trickle 1 GB through over time.
        // Post-read size check + connection close is the right wall.
        const MAX_LINE_BYTES: usize = 1024 * 1024;
        if line.len() > MAX_LINE_BYTES {
            warn!(
                bytes = line.len(),
                "IPC request exceeds {MAX_LINE_BYTES} byte cap — closing connection"
            );
            let resp = Response::Error {
                message: format!(
                    "request exceeds {MAX_LINE_BYTES}-byte size limit; connection closed"
                ),
            };
            let _ = write_response(&mut write_half, &resp).await;
            break;
        }

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
                // W1.6 / closes F-002 — inject the build-gate predicate
                // into the snapshot. framesage-engine has no etw dep
                // (ARCHITECTURE.md invariant #8) so the engine returns
                // `closed_loop_build_supported: false` by default; the
                // service overrides here with the real probe result
                // before sending. The predicate's result is cached
                // (framesage_etw::build_gate::CACHED_BUILD) so repeated
                // Status requests don't re-probe RtlGetVersion.
                let mut snap = engine.status();
                snap.closed_loop_build_supported =
                    framesage_etw::build_gate::closed_loop_enabled_for_this_build();
                write_response(&mut write_half, &Response::Status(Box::new(snap))).await?;
            }
            Request::ListProcesses => {
                let (snapshots, system) = engine.list_process_snapshots();
                write_response(&mut write_half, &Response::Processes { snapshots, system }).await?;
            }
            Request::ListServices => {
                // Item 4.13 — discover-services view. Enumeration
                // failures bubble up as an empty list (the engine
                // logs the underlying error); the UI handles
                // empty gracefully.
                let services = engine.list_services_for_ipc();
                write_response(&mut write_half, &Response::Services { services }).await?;
            }
            Request::SetProcessPriority { pid, class } => {
                match engine.set_process_priority(pid, class) {
                    Ok(()) => write_response(&mut write_half, &Response::Ok).await?,
                    Err(e) => {
                        write_response(
                            &mut write_half,
                            &Response::Error {
                                message: format!("set_process_priority(pid={pid}) failed: {e:#}"),
                            },
                        )
                        .await?;
                    }
                }
            }
            Request::SuspendProcess { pid } => match engine.suspend_process(pid) {
                Ok(()) => write_response(&mut write_half, &Response::Ok).await?,
                Err(e) => {
                    write_response(
                        &mut write_half,
                        &Response::Error {
                            message: format!("suspend_process(pid={pid}) failed: {e:#}"),
                        },
                    )
                    .await?;
                }
            },
            Request::ResumeProcess { pid } => match engine.resume_process(pid) {
                Ok(()) => write_response(&mut write_half, &Response::Ok).await?,
                Err(e) => {
                    write_response(
                        &mut write_half,
                        &Response::Error {
                            message: format!("resume_process(pid={pid}) failed: {e:#}"),
                        },
                    )
                    .await?;
                }
            },
            Request::TerminateProcess { pid } => match engine.terminate_process(pid) {
                Ok(()) => write_response(&mut write_half, &Response::Ok).await?,
                Err(e) => {
                    write_response(
                        &mut write_half,
                        &Response::Error {
                            message: format!("terminate_process(pid={pid}) failed: {e:#}"),
                        },
                    )
                    .await?;
                }
            },
            Request::SetProcessAffinity { pid, selector } => {
                match engine.set_process_affinity(pid, selector) {
                    Ok(()) => write_response(&mut write_half, &Response::Ok).await?,
                    Err(e) => {
                        write_response(
                            &mut write_half,
                            &Response::Error {
                                message: format!("set_process_affinity(pid={pid}) failed: {e:#}"),
                            },
                        )
                        .await?;
                    }
                }
            }
            Request::TrimWorkingSet { pid } => match engine.trim_working_set(pid) {
                Ok(()) => write_response(&mut write_half, &Response::Ok).await?,
                Err(e) => {
                    write_response(
                        &mut write_half,
                        &Response::Error {
                            message: format!("trim_working_set(pid={pid}) failed: {e:#}"),
                        },
                    )
                    .await?;
                }
            },
            Request::SetAffinityRule {
                rule,
                apply_to_live,
            } => {
                // Engine mutates its in-memory policy + walks live PIDs to pin
                // matches immediately (when apply_to_live), then we persist the
                // policy snapshot to disk so the rule survives a service
                // restart. The disk write is the part most likely to fail in
                // practice — same failure mode as Request::SetPolicy: service
                // running unelevated against a SYSTEM-owned policy.json. We
                // surface it the same way.
                let exe = rule.exe_name.clone();
                match engine.set_affinity_rule(rule, apply_to_live) {
                    Ok(()) => {
                        let snapshot = engine.policy_snapshot();
                        match snapshot.save(&paths::policy_path()) {
                            Ok(()) => {
                                write_response(&mut write_half, &Response::Ok).await?;
                            }
                            Err(e) => {
                                warn!(error = %e, exe = %exe, "policy save after SetAffinityRule failed");
                                write_response(
                                    &mut write_half,
                                    &Response::Error {
                                        message: format!(
                                            "policy.json save failed after creating \
                                             affinity rule for {exe}: {e}. Rule applied \
                                             in memory but will be lost on service \
                                             restart."
                                        ),
                                    },
                                )
                                .await?;
                            }
                        }
                    }
                    Err(e) => {
                        write_response(
                            &mut write_half,
                            &Response::Error {
                                message: format!("set_affinity_rule(exe={exe}) failed: {e:#}"),
                            },
                        )
                        .await?;
                    }
                }
            }
            Request::EnableManualGlobalGameMode { profile } => {
                match engine.enable_manual_global_game_mode(profile) {
                    Ok(()) => write_response(&mut write_half, &Response::Ok).await?,
                    Err(e) => {
                        write_response(
                            &mut write_half,
                            &Response::Error {
                                message: format!("enable_manual_global_game_mode failed: {e:#}"),
                            },
                        )
                        .await?;
                    }
                }
            }
            Request::DisableManualGlobalGameMode => {
                engine.disable_manual_global_game_mode();
                write_response(&mut write_half, &Response::Ok).await?;
            }
            Request::Undo => match engine.undo_last() {
                Ok(undone) => {
                    write_response(&mut write_half, &Response::UndoResult { undone }).await?;
                }
                Err(e) => {
                    write_response(
                        &mut write_half,
                        &Response::Error {
                            message: format!("undo_last failed: {e:#}"),
                        },
                    )
                    .await?;
                }
            },
            Request::UndoLogList { limit } => {
                let entries = engine.undo_log_snapshot(limit as usize);
                write_response(&mut write_half, &Response::UndoLog { entries }).await?;
            }
            Request::ListSessions { limit } => {
                // v0.7.1 Group C (#110) — Sessions-tab list. Directory
                // enumeration + first/last-line parsing is cheap but
                // still filesystem work; park it on the blocking pool
                // so a large sessions dir can't stall the IPC worker.
                let dir = paths::sessions_dir();
                let result =
                    tokio::task::spawn_blocking(move || framesage_recorder::list_sessions(&dir))
                        .await;
                let resp = match result {
                    Ok(Ok(mut sessions)) => {
                        sessions.truncate(limit.clamp(1, 500) as usize);
                        Response::Sessions { sessions }
                    }
                    Ok(Err(e)) => Response::Error {
                        message: format!("list sessions failed: {e:#}"),
                    },
                    Err(join_err) => Response::Error {
                        message: format!("list sessions task failed: {join_err}"),
                    },
                };
                write_response(&mut write_half, &resp).await?;
            }
            Request::ReadSession { session_id } => {
                // v0.7.1 Group C (#110) — session detail. The id is
                // client-supplied; session_file_path validates it
                // against the UUID character set BEFORE any path is
                // built (path-traversal trust boundary).
                let dir = paths::sessions_dir();
                let result = tokio::task::spawn_blocking(move || {
                    let path = framesage_recorder::session_file_path(&dir, &session_id)?;
                    framesage_recorder::read_session(&path)
                })
                .await;
                let resp = match result {
                    Ok(Ok((events, skipped))) => Response::SessionDetail {
                        events,
                        skipped_lines: skipped as u32,
                    },
                    Ok(Err(e)) => Response::Error {
                        message: format!("read session failed: {e:#}"),
                    },
                    Err(join_err) => Response::Error {
                        message: format!("read session task failed: {join_err}"),
                    },
                };
                write_response(&mut write_half, &resp).await?;
            }
            Request::RefreshTopology => {
                // Item 3.7 — manual topology refresh. The engine
                // logs the outcome internally and falls back to the
                // previous snapshot if detection fails, so we
                // always answer Ok.
                engine.refresh_topology();
                write_response(&mut write_half, &Response::Ok).await?;
            }
            Request::DeleteAffinityRule { exe_name } => {
                // Idempotent: delete returns Ok regardless of whether a rule
                // existed. Still persist on every call so the empty state
                // also makes it to disk if the user just cleared a rule.
                engine.delete_affinity_rule(&exe_name);
                let snapshot = engine.policy_snapshot();
                match snapshot.save(&paths::policy_path()) {
                    Ok(()) => write_response(&mut write_half, &Response::Ok).await?,
                    Err(e) => {
                        warn!(error = %e, exe = %exe_name, "policy save after DeleteAffinityRule failed");
                        write_response(
                            &mut write_half,
                            &Response::Error {
                                message: format!(
                                    "policy.json save failed after deleting affinity \
                                     rule for {exe_name}: {e}. Deletion applied in \
                                     memory but will be lost on service restart."
                                ),
                            },
                        )
                        .await?;
                    }
                }
            }
            Request::SetPolicy { policy } => {
                // Server-side safe-list intersection: reject the entire
                // SetPolicy if any profile requests stopping a denylisted
                // service or suspending a denylisted process. Aggression on
                // non-denylisted entries (BITS, WSearch, ClickToRunSvc,
                // OneDrive, etc.) is allowed — those are the product's
                // actual feature surface. Per the product positioning:
                // outside the denylist, aggression is on the table.
                let denied = validate_policy_against_safe_list(&policy);
                if !denied.is_empty() {
                    warn!(
                        count = denied.len(),
                        "SetPolicy rejected: denylisted entries"
                    );
                    write_response(
                        &mut write_half,
                        &Response::Error {
                            message: format!(
                                "SetPolicy rejected: {} denylisted entries — these processes / \
                                 services are on the framesage safety denylist (kernel-critical, \
                                 antivirus, anti-cheat) and cannot be touched regardless of \
                                 profile content:\n  {}",
                                denied.len(),
                                denied.join("\n  "),
                            ),
                        },
                    )
                    .await?;
                    continue;
                }

                // Item 4.11 — structural validation against current
                // topology. Catches dangling rule refs (rule.profile
                // points at a profile id that doesn't exist), out-of-
                // range CCD selectors (`Ccd(7)` on a single-CCD chip
                // would silently resolve to empty + then skip apply
                // entirely), and `Mask(0)` (would refuse at apply time
                // anyway — surface here so the user sees the issue
                // before they save).
                let topology = engine.topology_snapshot();
                let structural = policy.validate_structure(&topology);
                if !structural.is_empty() {
                    warn!(
                        count = structural.len(),
                        "SetPolicy rejected: structural errors"
                    );
                    write_response(
                        &mut write_half,
                        &Response::Error {
                            message: format!(
                                "SetPolicy rejected: {} structural error{}:\n  {}",
                                structural.len(),
                                if structural.len() == 1 { "" } else { "s" },
                                structural.join("\n  "),
                            ),
                        },
                    )
                    .await?;
                    continue;
                }

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
                // Item 1.8 / audit H-16 + M2.4 / A-005. Status pipe has
                // unlimited instances (PIPE_UNLIMITED_INSTANCES = 255
                // max), and any Authenticated User can call Subscribe —
                // which holds a pipe instance open indefinitely while
                // streaming events. Without caps, an unprivileged user
                // could spawn ~255 Subscribe clients, exhaust the
                // kernel pipe-instance table, and prevent the
                // legitimate tray's status traffic from connecting.
                //
                // Two layered caps (see SubscriberCaps):
                // * 32 process-wide — well above legitimate use (one
                //   tray + a handful of debug CLIs), well below the
                //   255 pipe cap so the rest of the IPC plane keeps
                //   working.
                // * 8 per client PID (via GetNamedPipeClientProcessId,
                //   captured at accept time) — one misbehaving client
                //   can no longer exhaust the shared cap for everyone
                //   else.
                if let Err(denied) = subscriber_caps().try_acquire(client_pid) {
                    warn!(
                        client_pid,
                        reason = ?denied,
                        "Subscribe rejected: subscriber cap reached"
                    );
                    write_response(
                        &mut write_half,
                        &Response::Error {
                            message: denied.user_message(),
                        },
                    )
                    .await?;
                    continue;
                }

                // RAII guard ensures the counter decrements even if
                // the connection dies mid-stream / panics / hits an
                // IO error. Without this, every dropped Subscribe
                // would leak a count and we'd hit the cap quickly.
                struct SubGuard {
                    pid: u32,
                }
                impl Drop for SubGuard {
                    fn drop(&mut self) {
                        subscriber_caps().release(self.pid);
                    }
                }
                let _sub_guard = SubGuard { pid: client_pid };

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
//
// Day 5 maintenance: the prior implementation had bit-rotted on the
// non-Windows host build because (a) `load_policy` was renamed to
// `load_policy_or_default` in an earlier refactor but the silence
// list wasn't updated, and (b) `type_name::<AsyncBufReadExt>` (bare
// trait name) became a hard error in newer Rust editions, AND
// `AsyncBufReadExt` / `AsyncWriteExt` are NOT dyn-compatible (their
// methods return `Self`-bound future types), so the `dyn` workaround
// doesn't apply either. Switched to a `PhantomData`-based pattern that
// works with arbitrary traits without dyn-compatibility requirements.
#[cfg(not(windows))]
#[allow(dead_code)]
fn _silence_warnings() {
    use std::marker::PhantomData;
    let _: PhantomData<fn() -> Box<dyn std::any::Any>> = PhantomData;
    let _ = load_policy_or_default;
    let _ = std::any::type_name::<Engine>;
    let _ = std::any::type_name::<Request>;
    let _ = std::any::type_name::<Response>;
    let _ = std::any::type_name::<Event>;
    let _ = std::any::type_name::<BufReader<tokio::io::DuplexStream>>;
    // For not-dyn-compatible traits: take a function pointer to a
    // method that uses the trait as a bound. The function never
    // executes — just keeps the import live.
    fn _uses_async_read<T: AsyncBufReadExt>(_: &T) {}
    fn _uses_async_write<T: AsyncWriteExt>(_: &T) {}
    let _ = _uses_async_read::<BufReader<tokio::io::DuplexStream>>;
    let _ = _uses_async_write::<tokio::io::DuplexStream>;
}

/// Tag a JoinHandle's result with the task name for the watchdog log
/// line. Per item 1.3, *any* unexpected exit (panic, clean return, abort
/// signal we didn't send) means the service can't do its job; the
/// distinction between `Ok(())` and `Err` isn't useful for the user-
/// facing message — both are equally fatal.
fn task_died_msg<T>(
    name: &'static str,
    result: &Result<T, tokio::task::JoinError>,
) -> &'static str {
    match result {
        Ok(_) => name,
        Err(e) if e.is_panic() => name,
        Err(_) => name,
    }
}

/// Walk every profile in `policy` and collect human-readable error
/// strings for any `stop_services` or `suspend_processes` entry that lands
/// on the bundled safe-list denylist. Returns an empty Vec when the
/// policy is acceptable.
///
/// The denylist is the trust-boundary for kernel-critical / AV /
/// anti-cheat entries — these are non-overridable by design. Anything
/// else (BITS / WSearch / ClickToRunSvc / OneDrive / OEM bloat) passes
/// through because that's exactly the aggression surface the product
/// promises.
///
/// Extracted out of the SetPolicy handler so it's unit-testable without
/// spinning up the full IPC stack.
fn validate_policy_against_safe_list(policy: &Policy) -> Vec<String> {
    let safe_list = framesage_gamemode::safe_list::SafeList::bundled();
    let mut denied: Vec<String> = Vec::new();
    for (profile_id, profile) in &policy.profiles {
        let Some(gm) = profile.game_mode.as_ref() else {
            continue;
        };
        for entry in &gm.stop_services {
            if let framesage_gamemode::safe_list::ServiceVerdict::Denied(reason) =
                safe_list.check_service(entry)
            {
                denied.push(format!(
                    "profile '{}': stop_services entry '{}' is on the framesage \
                     denylist for safety — {}",
                    profile_id.0, entry, reason
                ));
            }
        }
        for entry in &gm.suspend_processes {
            if let framesage_gamemode::safe_list::ProcessVerdict::Denied(reason) =
                safe_list.check_process(entry)
            {
                denied.push(format!(
                    "profile '{}': suspend_processes entry '{}' is on the framesage \
                     denylist for safety — {}",
                    profile_id.0, entry, reason
                ));
            }
        }
    }
    denied
}

#[cfg(test)]
mod watchdog_exclusion_tests {
    // M1.3 / B-002 — architecture §2.1 mode 5 amendment: closed-loop
    // task crashes must NOT crash the service, which structurally
    // means the supervisor/drop-poll handles never join the watchdog
    // `tokio::select!` in `run()`. That contract is only enforced by
    // code shape, so pin it with a source-level assertion: extract the
    // watchdog select! block and check its contents.

    const RUNTIME_SRC: &str = include_str!("runtime.rs");

    fn watchdog_select_block() -> &'static str {
        let start = RUNTIME_SRC
            .find("let unexpected_exit: Option<&'static str> = tokio::select! {")
            .expect("watchdog select! block not found — update this test's anchor");
        let end = RUNTIME_SRC[start..]
            .find("};")
            .expect("watchdog select! block unterminated");
        &RUNTIME_SRC[start..start + end]
    }

    #[test]
    fn watchdog_covers_exactly_the_v06_critical_tasks() {
        let block = watchdog_select_block();
        for handle in [
            "shutdown",
            "tick_handle",
            "admin_handle",
            "status_handle",
            "reload_handle",
            "sys_handle",
        ] {
            assert!(
                block.contains(handle),
                "critical task {handle} missing from watchdog select!"
            );
        }
    }

    #[test]
    fn closed_loop_tasks_are_not_in_the_watchdog() {
        let block = watchdog_select_block();
        for forbidden in ["closed_loop", "supervisor", "drop_poll", "monitor"] {
            assert!(
                !block.contains(forbidden),
                "closed-loop task '{forbidden}' found in the watchdog select! — \
                 this violates architecture §2.1 mode 5 (supervisor exit is NOT \
                 a critical service failure)"
            );
        }
    }
}

#[cfg(test)]
mod subscriber_cap_tests {
    use super::*;

    // M2.4 / A-005 acceptance criterion: one PID holding its full
    // per-PID budget cannot block other PIDs from subscribing.
    #[test]
    fn per_pid_cap_does_not_starve_other_pids() {
        let caps = SubscriberCaps::new();
        for _ in 0..MAX_SUBSCRIBERS_PER_PID {
            caps.try_acquire(1111).expect("within per-PID budget");
        }
        assert_eq!(
            caps.try_acquire(1111),
            Err(SubscribeDenied::PerPidCap),
            "9th subscription from the same PID must be refused"
        );
        caps.try_acquire(2222)
            .expect("a different PID must still get a slot");
    }

    #[test]
    fn total_cap_still_enforced_across_pids() {
        let caps = SubscriberCaps::new();
        let full_pids = MAX_SUBSCRIBERS_TOTAL / MAX_SUBSCRIBERS_PER_PID;
        for pid in 0..full_pids as u32 {
            for _ in 0..MAX_SUBSCRIBERS_PER_PID {
                caps.try_acquire(pid).expect("under total cap");
            }
        }
        assert_eq!(
            caps.try_acquire(9999),
            Err(SubscribeDenied::TotalCap),
            "process-wide ceiling holds even for a fresh PID"
        );
    }

    #[test]
    fn release_frees_the_slot_and_prunes_zeroed_entries() {
        let caps = SubscriberCaps::new();
        for _ in 0..MAX_SUBSCRIBERS_PER_PID {
            caps.try_acquire(42).unwrap();
        }
        assert!(caps.try_acquire(42).is_err());
        caps.release(42);
        caps.try_acquire(42).expect("released slot is reusable");
        for _ in 0..MAX_SUBSCRIBERS_PER_PID {
            caps.release(42);
        }
        assert!(
            caps.by_pid.lock().unwrap().is_empty(),
            "fully-released PID entries are pruned"
        );
        // Extra release for an absent PID is a no-op, not a panic/underflow.
        caps.release(42);
    }

    #[test]
    fn unknown_pid_clients_share_one_budget() {
        let caps = SubscriberCaps::new();
        for _ in 0..MAX_SUBSCRIBERS_PER_PID {
            caps.try_acquire(UNKNOWN_CLIENT_PID).unwrap();
        }
        assert_eq!(
            caps.try_acquire(UNKNOWN_CLIENT_PID),
            Err(SubscribeDenied::PerPidCap),
            "PID-query failures cannot bypass the cap"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framesage_core::{game_mode::GameModeActions, profile::Profile, ProfileId};
    use std::collections::HashMap;

    fn policy_with(profile_id: &str, game_mode: GameModeActions) -> Policy {
        let mut profiles = HashMap::new();
        let profile = Profile {
            id: profile_id.into(),
            game_mode: Some(game_mode),
            ..Default::default()
        };
        profiles.insert(ProfileId(profile_id.into()), profile);
        Policy {
            profiles,
            rules: vec![],
            default_profile: ProfileId(profile_id.into()),
            background_profile: None,
            tick_ms: 300,
            probalance: framesage_core::ProBalanceConfig::default(),
            affinity_rules: Vec::new(),
            closed_loop_enabled: false,
        }
    }

    /// Aggressive defaults (BITS / WSearch / OneDrive / etc.) MUST pass.
    /// These are the product's actual feature surface. Refusing them
    /// would defeat the whole point of the product positioning.
    #[test]
    fn validate_accepts_aggressive_but_safe_defaults() {
        let policy = policy_with(
            "aggressive",
            GameModeActions {
                stop_services: vec![
                    "SysMain".into(),
                    "WSearch".into(),
                    "DiagTrack".into(),
                    "BITS".into(),
                    "DoSvc".into(),
                    "WaaSMedicSvc".into(),
                    "UsoSvc".into(),
                    "ClickToRunSvc".into(),
                    "WMPNetworkSvc".into(),
                    "Fax".into(),
                ],
                suspend_processes: vec![
                    "OneDrive.exe".into(),
                    "FileCoAuth.exe".into(),
                    "Dropbox.exe".into(),
                    "GameBar.exe".into(),
                    "WidgetService.exe".into(),
                    "YourPhone.exe".into(),
                    "lghub_updater.exe".into(),
                ],
                ..Default::default()
            },
        );
        let denials = validate_policy_against_safe_list(&policy);
        assert!(
            denials.is_empty(),
            "aggressive defaults must pass; got: {denials:#?}"
        );
    }

    /// Denylisted services (AV, anti-cheat, RPC, DNS, DHCP, audio) MUST
    /// be refused with the rationale string from the JSON.
    #[test]
    fn validate_refuses_denylisted_services() {
        for service in &[
            "WinDefend",     // Defender
            "vgc",           // Riot Vanguard
            "EasyAntiCheat", // EAC
            "BEService",     // BattlEye
            "RpcSs",         // RPC
            "Dhcp",          // DHCP
            "Dnscache",      // DNS
            "AudioSrv",      // Audio
        ] {
            let policy = policy_with(
                "bad",
                GameModeActions {
                    stop_services: vec![service.to_string()],
                    ..Default::default()
                },
            );
            let denials = validate_policy_against_safe_list(&policy);
            assert_eq!(
                denials.len(),
                1,
                "expected refusal for service {service}, got: {denials:#?}"
            );
            assert!(
                denials[0]
                    .to_ascii_lowercase()
                    .contains(&service.to_ascii_lowercase()),
                "denial should name {service}, got: {}",
                denials[0]
            );
            assert!(
                denials[0].contains("denylist"),
                "denial should mention denylist, got: {}",
                denials[0]
            );
        }
    }

    /// Same for processes: critical shell, AV, GPU drivers, kernel /
    /// session processes.
    #[test]
    fn validate_refuses_denylisted_processes() {
        for exe in &[
            "csrss.exe",
            "lsass.exe",
            "wininit.exe",
            "dwm.exe",
            "audiodg.exe",
            "MsMpEng.exe",
            "explorer.exe",
            "nvcontainer.exe",
        ] {
            let policy = policy_with(
                "bad",
                GameModeActions {
                    suspend_processes: vec![exe.to_string()],
                    ..Default::default()
                },
            );
            let denials = validate_policy_against_safe_list(&policy);
            assert_eq!(
                denials.len(),
                1,
                "expected refusal for process {exe}, got: {denials:#?}"
            );
        }
    }

    /// Surface every denial in a single response, not just the first.
    /// The user fixes them all at once instead of one save-reject cycle
    /// per entry. The product contract is "informed refusal."
    #[test]
    fn validate_reports_all_denials_at_once() {
        let policy = policy_with(
            "many-bad",
            GameModeActions {
                stop_services: vec!["WinDefend".into(), "vgc".into(), "Dhcp".into()],
                suspend_processes: vec!["csrss.exe".into(), "lsass.exe".into()],
                ..Default::default()
            },
        );
        let denials = validate_policy_against_safe_list(&policy);
        assert_eq!(
            denials.len(),
            5,
            "must report all 5 denials in one pass; got: {denials:#?}"
        );
    }

    /// Profile without a game_mode (eco/perf-style profiles) is trivially
    /// safe — no services to stop, no processes to suspend.
    #[test]
    fn validate_accepts_profile_without_game_mode() {
        let mut profiles = HashMap::new();
        profiles.insert(
            ProfileId("perf".into()),
            Profile {
                id: "perf".into(),
                game_mode: None,
                ..Default::default()
            },
        );
        let policy = Policy {
            profiles,
            rules: vec![],
            default_profile: ProfileId("perf".into()),
            background_profile: None,
            tick_ms: 300,
            probalance: framesage_core::ProBalanceConfig::default(),
            affinity_rules: Vec::new(),
            closed_loop_enabled: false,
        };
        assert!(validate_policy_against_safe_list(&policy).is_empty());
    }

    /// The shipped default policy passes validation. Locks the invariant
    /// that we never accidentally ship a default that the safety layer
    /// would then reject.
    #[test]
    fn validate_accepts_shipped_default_policy() {
        let denials = validate_policy_against_safe_list(&Policy::default());
        assert!(
            denials.is_empty(),
            "shipped default policy must validate cleanly; got: {denials:#?}"
        );
    }
}
