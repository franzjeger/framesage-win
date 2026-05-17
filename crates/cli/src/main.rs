//! framesage.exe — CLI for installing the service and talking to it.
//!
//! Subcommands map roughly 1:1 to `framesage_ipc::Request`. The exception is
//! `install` / `uninstall` / `start` / `stop`, which talk to the SCM directly.
//!
//! # Layering (item 3.8)
//!
//! Depends on `framesage-core` + `framesage-gamemode` + `framesage-ipc` +
//! `framesage-sys` (for topology detection in `framesage topology`). It
//! does NOT depend on `framesage-engine` — every engine-side action is
//! reached via the IPC protocol so the CLI stays a thin client. See
//! `ARCHITECTURE.md` at the repo root.

#![cfg_attr(not(windows), allow(unused_imports, dead_code))]

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use tracing::info;

use framesage_core::{CoreKind, ProfileId};
use framesage_ipc::{Request, Response, StatusSnapshot};

const SERVICE_NAME: &str = "framesage";
const SERVICE_DISPLAY: &str = "framesage scheduler supervisor";
const SERVICE_DESCRIPTION: &str =
    "Observes foreground apps and applies per-process scheduling policies (CPU Sets, Power Throttling, priorities).";

#[derive(Parser, Debug)]
#[command(
    name = "framesage",
    version,
    about = "framesage CLI",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Register the service with the Windows Service Control Manager.
    /// Must be run elevated.
    Install {
        /// Path to framesage-svc.exe. Defaults to the binary next to the CLI.
        #[arg(long)]
        bin: Option<String>,
    },
    /// Remove the service from the SCM.
    Uninstall,
    /// Start the service.
    Start,
    /// Stop the service.
    Stop,
    /// Print service status (queried over the named pipe).
    Status,
    /// Pause the engine — it stops applying anything, but stays alive.
    Pause,
    /// Resume after a pause.
    Resume,
    /// One-shot: apply a named profile to the current foreground process.
    Apply {
        /// Profile id, e.g. `game-x3d`.
        profile: String,
    },
    /// Print the CPU topology as detected by `framesage-sys` on this machine
    /// — number of logical CPUs, per-CCD layout, kind (Performance / Cache /
    /// Efficiency), CPPC ranks, SMT siblings. Useful for verifying that X3D
    /// detection picked the right CCD on a new chip.
    Topology,
    /// Item 3.7 — ask the running service to re-detect CPU topology
    /// and swap its cached snapshot. The service already refreshes
    /// automatically on sleep/resume; this verb covers cases where
    /// the user toggled core-parking or processor-state limits via
    /// Windows' advanced power settings (which don't fire a resume
    /// event) and wants the engine to pick up the change without a
    /// reboot.
    RefreshTopology,
    /// Game Mode controls — status, panic-off, safe-list inspection.
    #[command(subcommand)]
    GameMode(GameModeCmd),
    /// Item 3.5 — undo the most recent user-initiated action
    /// (priority change, affinity change, suspend, resume).
    /// Pops one entry from the engine's in-memory undo log and
    /// applies its reverse. With no subcommand, undoes the last
    /// action; `undo list` shows the recent log.
    #[command(subcommand)]
    Undo(UndoCmd),
    /// Item 4.3 — policy management verbs. Export the live policy
    /// to a JSON file, import from a file (committed via SetPolicy
    /// so the engine sees it immediately + the service persists),
    /// or add a quick exe-name rule from the shell.
    #[command(subcommand)]
    Policy(PolicyCmd),
}

#[derive(Subcommand, Debug)]
enum PolicyCmd {
    /// Save the running service's current policy to a JSON file.
    /// Uses the same shape as `policy.json` on disk so the file is
    /// readable by `policy import` round-trip. Exports the live
    /// in-memory state, which may include unsaved-to-disk edits.
    Export {
        /// Destination file path. Will be overwritten if it exists.
        path: String,
    },
    /// Read a policy JSON file and commit it via SetPolicy. The
    /// usual server-side validation runs (safe-list + structural);
    /// rejection prints the error and the live policy is unchanged.
    Import {
        /// Source file path.
        path: String,
    },
    /// Append a single exe-name → profile rule to the running
    /// policy and commit via SetPolicy. Idempotent on existing
    /// rules: if a rule with the same exe-name already exists, it
    /// is replaced. Convenience for the shell-driven workflow
    /// ("frameSage policy add-rule notepad.exe perf").
    AddRule {
        /// Exe filename (case-insensitive match in the engine —
        /// e.g. "bf6.exe").
        exe: String,
        /// Profile id to bind. Must already exist in the policy;
        /// the engine rejects unknown profile ids in the SetPolicy
        /// structural validator.
        profile: String,
    },
}

#[derive(Subcommand, Debug)]
enum UndoCmd {
    /// Pop the most recent entry from the engine's undo log and
    /// apply its reverse. Idempotent: each invocation removes one
    /// entry. If the reverse fails (typically because the target
    /// PID exited between the original action and the undo), the
    /// failure is printed but the entry is still removed.
    #[command(name = "last")]
    Last,
    /// List the recent undo-log entries, newest first. Default
    /// limit is 20 entries; the engine keeps up to 50 in memory.
    List {
        /// Maximum entries to show (default 20).
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },
}

#[derive(Subcommand, Debug)]
enum GameModeCmd {
    /// Show whether Game Mode is currently active, and what the curated
    /// safe-list contains.
    Status,
    /// Panic button — force-revert any active Game Mode session immediately.
    /// Same effect as the service noticing focus left the game, but
    /// triggerable by hand if the engine got stuck or the user wants out.
    Off,
    /// Print the curated safe-list (services + processes) with rationale.
    SafeList,
    /// Item 2.11 — enter Manual Global Game Mode for `profile`. The
    /// profile's `game_mode` actions are applied system-wide
    /// regardless of foreground, and stay until `framesage game-mode
    /// off-global` (or `off`) is invoked. Profile must be marked
    /// `manual_global_eligible` in the policy.
    On {
        /// Profile id (must have `manual_global_eligible = true`).
        profile: String,
    },
    /// Item 2.11 — exit Manual Global Game Mode. Idempotent. Distinct
    /// from `off`: `off` is the panic button that clears anything
    /// active (manual global OR auto session); `off-global` only
    /// touches manual global so an auto session triggered by the
    /// foreground keeps running.
    OffGlobal,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Install { bin } => install_service(bin.as_deref()),
        Cmd::Uninstall => uninstall_service(),
        Cmd::Start => start_service(),
        Cmd::Stop => stop_service(),
        Cmd::Status => tokio_block(async { print_status().await }),
        Cmd::Pause => tokio_block(async { send_simple(Request::Pause).await }),
        Cmd::Resume => tokio_block(async { send_simple(Request::Resume).await }),
        Cmd::Apply { profile } => tokio_block(async {
            send_simple(Request::ApplyOnce {
                profile: ProfileId(profile),
            })
            .await
        }),
        Cmd::Topology => print_topology(),
        Cmd::RefreshTopology => {
            tokio_block(async { send_simple(Request::RefreshTopology).await })
        }
        Cmd::GameMode(sub) => match sub {
            GameModeCmd::Status => tokio_block(async { print_game_mode_status().await }),
            GameModeCmd::Off => tokio_block(async { send_simple(Request::GameModeOff).await }),
            GameModeCmd::SafeList => {
                print_safe_list();
                Ok(())
            }
            GameModeCmd::On { profile } => tokio_block(async {
                send_simple(Request::EnableManualGlobalGameMode {
                    profile: ProfileId(profile),
                })
                .await
            }),
            GameModeCmd::OffGlobal => {
                tokio_block(async { send_simple(Request::DisableManualGlobalGameMode).await })
            }
        },
        Cmd::Undo(sub) => match sub {
            UndoCmd::Last => tokio_block(async { send_simple(Request::Undo).await }),
            UndoCmd::List { limit } => {
                tokio_block(async { send_simple(Request::UndoLogList { limit }).await })
            }
        },
        Cmd::Policy(sub) => match sub {
            PolicyCmd::Export { path } => {
                tokio_block(async { policy_export(&path).await })
            }
            PolicyCmd::Import { path } => {
                tokio_block(async { policy_import(&path).await })
            }
            PolicyCmd::AddRule { exe, profile } => {
                tokio_block(async { policy_add_rule(&exe, &profile).await })
            }
        },
    }
}

/// Item 4.3 — export the live policy via Status, write as
/// pretty-printed JSON to `path`. Overwrites unconditionally.
#[cfg(windows)]
async fn policy_export(path: &str) -> Result<()> {
    let policy = fetch_live_policy().await?;
    let body =
        serde_json::to_string_pretty(&policy).context("serialize policy for export")?;
    std::fs::write(path, body).with_context(|| format!("write policy export to {path}"))?;
    println!("exported policy to {path}");
    Ok(())
}

#[cfg(not(windows))]
async fn policy_export(_path: &str) -> Result<()> {
    Err(anyhow!("policy verbs are Windows-only"))
}

/// Item 4.3 — read policy JSON from disk, commit via SetPolicy.
/// The server-side safe-list + structural validators run; any
/// rejection comes back as Response::Error with the offending
/// details surfaced verbatim.
#[cfg(windows)]
async fn policy_import(path: &str) -> Result<()> {
    let body =
        std::fs::read_to_string(path).with_context(|| format!("read policy file {path}"))?;
    let policy: framesage_core::Policy = serde_json::from_str(&body)
        .with_context(|| format!("parse policy file {path} as JSON"))?;
    send_simple(Request::SetPolicy { policy }).await?;
    println!("imported policy from {path}");
    Ok(())
}

#[cfg(not(windows))]
async fn policy_import(_path: &str) -> Result<()> {
    Err(anyhow!("policy verbs are Windows-only"))
}

/// Item 4.3 — pull the live policy, upsert an exe-name rule
/// pointing at `profile`, commit via SetPolicy. Replaces an
/// existing rule for the same exe (case-insensitive) so repeated
/// invocations are idempotent. The engine refuses if the profile
/// id doesn't exist (Response::Error via the structural
/// validator added in item 4.11).
#[cfg(windows)]
async fn policy_add_rule(exe: &str, profile: &str) -> Result<()> {
    use framesage_core::{AppMatch, AppRule};
    let mut policy = fetch_live_policy().await?;
    let new_rule = AppRule {
        r#match: AppMatch::ExeName(exe.to_string()),
        profile: ProfileId(profile.to_string()),
        note: format!("added via `framesage policy add-rule {exe} {profile}`"),
    };
    // Idempotent: if a rule already exists for this exe (case-
    // insensitive), replace it. Otherwise append.
    let mut replaced = false;
    for slot in policy.rules.iter_mut() {
        if let AppMatch::ExeName(existing) = &slot.r#match {
            if existing.eq_ignore_ascii_case(exe) {
                *slot = new_rule.clone();
                replaced = true;
                break;
            }
        }
    }
    if !replaced {
        policy.rules.push(new_rule);
    }
    send_simple(Request::SetPolicy { policy }).await?;
    if replaced {
        println!("updated existing rule for {exe} -> {profile}");
    } else {
        println!("added rule {exe} -> {profile}");
    }
    Ok(())
}

#[cfg(not(windows))]
async fn policy_add_rule(_exe: &str, _profile: &str) -> Result<()> {
    Err(anyhow!("policy verbs are Windows-only"))
}

/// Item 4.3 helper — pull the live policy off the running service
/// via a Status request. Reused by `policy export` and
/// `policy add-rule`.
#[cfg(windows)]
async fn fetch_live_policy() -> Result<framesage_core::Policy> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let stream = open_pipe(Request::Status.target_pipe()).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half).lines();
    let mut line = serde_json::to_vec(&Request::Status)?;
    line.push(b'\n');
    write_half.write_all(&line).await?;
    write_half.flush().await?;
    let Some(resp_line) = reader.next_line().await? else {
        return Err(anyhow!("service closed pipe without responding"));
    };
    let resp: Response = serde_json::from_str(&resp_line)?;
    match resp {
        Response::Status(s) => Ok(s.policy.clone()),
        Response::Error { message } => Err(anyhow!(message)),
        other => Err(anyhow!("expected Status response, got {other:?}")),
    }
}

#[cfg(windows)]
fn print_topology() -> Result<()> {
    let topo = framesage_sys::topology::detect().context("topology::detect failed")?;
    println!("logical cpus: {}", topo.cpus.len());

    let mut ccds: Vec<u8> = topo.ccds().collect();
    ccds.sort_unstable();
    for ccd in &ccds {
        let cpus: Vec<_> = topo.cpus_on_ccd(*ccd).collect();
        let cache = cpus.iter().filter(|c| c.kind == CoreKind::Cache).count();
        let perf = cpus
            .iter()
            .filter(|c| c.kind == CoreKind::Performance)
            .count();
        let eff = cpus
            .iter()
            .filter(|c| c.kind == CoreKind::Efficiency)
            .count();
        let top_rank = cpus.iter().filter_map(|c| c.cppc_rank).max();
        let l3 = cpus.iter().filter_map(|c| c.l3_cache_bytes).max();
        println!(
            "ccd {}: {} cpus  (perf={} cache={} eff={})  top_rank={}  l3={}",
            ccd,
            cpus.len(),
            perf,
            cache,
            eff,
            top_rank
                .map(|r| r.to_string())
                .unwrap_or_else(|| "—".into()),
            l3.map(|b| format!("{} MB", b / (1024 * 1024)))
                .unwrap_or_else(|| "—".into()),
        );
    }

    println!();
    for cpu in &topo.cpus {
        println!(
            "  cpu{:2}  core={:2}  ccd={}  kind={:?}  rank={:?}  l3={}  smt={}",
            cpu.index,
            cpu.physical_core,
            cpu.ccd,
            cpu.kind,
            cpu.cppc_rank,
            cpu.l3_cache_bytes
                .map(|b| format!("{}MB", b / (1024 * 1024)))
                .unwrap_or_else(|| "—".into()),
            cpu.is_smt_sibling
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn print_topology() -> Result<()> {
    Err(anyhow!(
        "topology detection is Windows-only; use `framesage-sim topology` for the cross-platform dev view"
    ))
}

fn print_safe_list() {
    let list = framesage_gamemode::safe_list::SafeList::bundled();
    println!("Services (allow-listed for stop):");
    let mut services: Vec<_> = list.services().collect();
    services.sort_by(|a, b| a.id.cmp(&b.id));
    for s in services {
        let recommended = if s.default_stop { " *" } else { "  " };
        println!("  {}{}  ({})", recommended, s.id, s.display_name);
        println!("      {}", s.rationale);
    }
    println!();
    println!("Processes (allow-listed for suspend):");
    let mut processes: Vec<_> = list.processes().collect();
    processes.sort_by(|a, b| a.exe.cmp(&b.exe));
    for p in processes {
        let recommended = if p.default_suspend { " *" } else { "  " };
        println!("  {}{}  ({})", recommended, p.exe, p.display_name);
        println!("      {}", p.rationale);
    }
    println!();
    println!("* = recommended default. Add to a profile's `stop_services` / `suspend_processes` to apply.");
}

async fn print_game_mode_status() -> Result<()> {
    // Game Mode active-state isn't yet a first-class field in StatusSnapshot;
    // for v0.1 we surface the active profile and let the user infer. The
    // engine logs and the journal file are authoritative; we print the
    // journal path for visibility.
    print_status().await?;
    let journal_path = framesage_core::paths::config_dir().join("game-mode.journal");
    if journal_path.exists() {
        println!("\ngame-mode journal: PRESENT ({})", journal_path.display());
        println!("  Game Mode appears active. Run `framesage game-mode off` to revert.");
    } else {
        println!("\ngame-mode journal: absent ({})", journal_path.display());
        println!("  No Game Mode session active.");
    }
    Ok(())
}

fn tokio_block<F: std::future::Future<Output = Result<()>>>(fut: F) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(fut)
}

// ─── SCM ──────────────────────────────────────────────────────────────────

#[cfg(windows)]
fn install_service(bin_override: Option<&str>) -> Result<()> {
    use std::ffi::OsString;
    use std::time::Duration;
    use windows_service::service::{
        ServiceAccess, ServiceAction, ServiceActionType, ServiceErrorControl,
        ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType,
        ServiceType,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let bin_path = match bin_override {
        Some(p) => std::path::PathBuf::from(p),
        None => default_service_binary()?,
    };

    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .context("open SCM")?;

    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(SERVICE_DISPLAY),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: bin_path,
        launch_arguments: vec![],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };

    let service = manager
        .create_service(&info, ServiceAccess::CHANGE_CONFIG)
        .context("CreateService")?;

    service
        .set_description(SERVICE_DESCRIPTION)
        .context("set description")?;

    // Item 1.3 / audit C-05 — configure SCM FailureActions so any service
    // crash auto-restarts within 5 seconds instead of leaving the user
    // with a permanent silent outage until reboot.
    //
    // The triple is the canonical "restart aggressively but not forever"
    // pattern Windows admins recognize: restart, restart, restart-then-
    // give-up, with a 1-day reset period. If the service crashes more
    // than 3 times in 24 hours, SCM stops trying — at that point the
    // user has a real problem that auto-restart won't paper over (likely
    // a corrupted policy.json or hardware change), and the silence is
    // the right signal.
    //
    // `set_failure_actions_on_non_crash_failures(true)` is critical
    // because `panic = "abort"` (Cargo.toml:95) makes panics produce a
    // clean process exit with code 1, not a crash dump. Without this
    // flag SCM only triggers FailureActions on actual hardware exceptions
    // (access violations, etc.) and would happily leave a panic-exited
    // service stopped.
    let failure_actions = ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86_400)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![
            ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(5),
            },
            ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(5),
            },
            ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: Duration::from_secs(5),
            },
        ]),
    };
    service
        .update_failure_actions(failure_actions)
        .context("update_failure_actions")?;
    service
        .set_failure_actions_on_non_crash_failures(true)
        .context("set_failure_actions_on_non_crash_failures")?;

    info!(service = SERVICE_NAME, "installed with SCM FailureActions");
    println!("installed: {SERVICE_NAME}");
    println!("  failure-actions: restart x3 with 5s delay, reset after 24h");
    Ok(())
}

#[cfg(not(windows))]
fn install_service(_bin: Option<&str>) -> Result<()> {
    Err(anyhow!("install is Windows-only"))
}

#[cfg(windows)]
fn uninstall_service() -> Result<()> {
    use std::thread::sleep;
    use std::time::{Duration, Instant};
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    // Item 1.5 / audit C-08 + C-09 + H-32. Before this, `uninstall`
    // deleted the SCM service registration and called it done. The audit
    // caught the residue: 4 orphan binaries in the install dir, three
    // shortcuts (Start Menu, Desktop, Startup), and — most damaging — a
    // potentially-active `game-mode.journal` that nobody will ever
    // revert now that the service is gone. The user is left with a
    // half-modified system, a tray that respawns at every logon
    // pointing at a missing exe, and no tool to fix any of it.
    //
    // This implementation does the full clean-up. Each step is
    // best-effort independent — if one fails, the others still run, so
    // a partial uninstall is still mostly-clean.

    let mut report = UninstallReport::default();

    // Step 1 — Recover any orphan Game Mode journal BEFORE killing the
    // service. We do this by asking the service itself (if still
    // running) to fire its panic-button revert path. If the service is
    // already stopped, we leave the journal to the next service start —
    // but the service is about to be deleted, so we fall back to a
    // best-effort manual revert via the gamemode crate's recovery API.
    //
    // The journal contains the only record of what services we stopped
    // and what processes we suspended. Failing to recover it means
    // those system mutations are permanent until reboot.
    recover_journal_before_uninstall(&mut report);

    // Step 2 — Open the service. If it's not registered, we still want
    // to do shortcut + binary cleanup, so a "service not found" is not
    // fatal.
    let manager_res = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT);
    let service_present = match &manager_res {
        Ok(manager) => manager
            .open_service(
                SERVICE_NAME,
                ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
            )
            .is_ok(),
        Err(_) => false,
    };

    if let Ok(manager) = &manager_res {
        if service_present {
            // Step 3 — Stop the service before delete. If we don't, SCM
            // marks the service "deletion pending" and won't actually
            // remove the registration until every handle closes — which
            // on a running service won't happen until reboot, blocking
            // re-install in the meantime (H-32).
            if let Ok(service) = manager.open_service(
                SERVICE_NAME,
                ServiceAccess::QUERY_STATUS | ServiceAccess::STOP,
            ) {
                let current = service
                    .query_status()
                    .map(|s| s.current_state)
                    .unwrap_or(ServiceState::Stopped);

                if current != ServiceState::Stopped {
                    println!("stopping {SERVICE_NAME}...");
                    let _ = service.stop();
                    // Poll for STOPPED up to 30 s. SCM stop is async;
                    // the call returns once the request is accepted,
                    // not once the service has actually stopped.
                    let deadline = Instant::now() + Duration::from_secs(30);
                    while Instant::now() < deadline {
                        match service.query_status() {
                            Ok(s) if s.current_state == ServiceState::Stopped => break,
                            _ => sleep(Duration::from_millis(500)),
                        }
                    }
                    let final_state = service
                        .query_status()
                        .map(|s| s.current_state)
                        .unwrap_or(ServiceState::Stopped);
                    if final_state != ServiceState::Stopped {
                        // The service hung in STOP_PENDING or refused
                        // to stop. Fall back to killing the process by
                        // name so DeleteService doesn't leave us with a
                        // "marked for deletion" zombie until reboot.
                        eprintln!(
                            "  warning: service did not stop cleanly within 30s — force-killing"
                        );
                        force_kill_service_process();
                        sleep(Duration::from_millis(500));
                    }
                }
                report.service_stopped = true;
            }

            // Step 4 — Delete the SCM registration.
            if let Ok(service) = manager.open_service(SERVICE_NAME, ServiceAccess::DELETE) {
                match service.delete() {
                    Ok(()) => {
                        println!("  unregistered SCM service: {SERVICE_NAME}");
                        report.service_unregistered = true;
                    }
                    Err(e) => {
                        eprintln!("  service delete failed: {e}");
                    }
                }
            }
        } else {
            println!("note: service '{SERVICE_NAME}' is not registered (already uninstalled?)");
        }
    } else {
        eprintln!("warning: could not open SCM — service cleanup skipped");
    }

    // Step 5 — Remove shortcuts. The Startup-folder one is the worst
    // residue because the tray respawns at every logon, fails to find
    // the (deleted) service, and either errors or sits broken. The
    // user has no way to trace that broken tray icon back to
    // framesage. This step is the single highest-impact cleanup.
    remove_user_shortcuts(&mut report);

    // Step 6 — Remove install-dir binaries. The 4 .exes are dead weight
    // and take up ~40 MB. Best-effort: a binary file lock (tray still
    // running somewhere, AV scan in progress) means we skip that file
    // and log it. The user can re-run after rebooting.
    remove_install_dir(&mut report);

    // Step 7 — Print final report. Tell the user EXACTLY what's left
    // (notably `%ProgramData%\framesage\` which we preserve by default
    // — it contains policy.json + sessions.jsonl, which the user may
    // want for a future reinstall).
    print_uninstall_summary(&report);

    Ok(())
}

#[derive(Default)]
struct UninstallReport {
    service_stopped: bool,
    service_unregistered: bool,
    journal_reverted: bool,
    journal_path_for_user: Option<std::path::PathBuf>,
    shortcuts_removed: Vec<std::path::PathBuf>,
    shortcuts_failed: Vec<(std::path::PathBuf, String)>,
    binaries_removed: Vec<std::path::PathBuf>,
    binaries_failed: Vec<(std::path::PathBuf, String)>,
    install_dir_removed: bool,
    install_dir_path: Option<std::path::PathBuf>,
}

#[cfg(windows)]
fn recover_journal_before_uninstall(report: &mut UninstallReport) {
    let journal_path = framesage_core::paths::config_dir().join("game-mode.journal");
    if !journal_path.exists() {
        return;
    }

    report.journal_path_for_user = Some(journal_path.clone());
    println!(
        "found active Game Mode journal at {}",
        journal_path.display()
    );
    println!("  attempting recovery via running service...");

    // First try: ask the live service to revert via the panic-button
    // IPC. This is the cleanest path because the engine already knows
    // how to read the journal + call the right syscalls + emit events.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("  could not start runtime for IPC revert: {e}");
            return;
        }
    };
    let ipc_ok = rt.block_on(async {
        match send_simple(Request::GameModeOff).await {
            Ok(()) => true,
            Err(e) => {
                eprintln!("  IPC revert failed: {e} (service may already be stopped)");
                false
            }
        }
    });

    if ipc_ok {
        println!("  service-driven revert succeeded");
        report.journal_reverted = true;
        // Service should have deleted the journal as part of its revert
        // path. Confirm and stop here.
        if !journal_path.exists() {
            return;
        }
    }

    // Second try: read the journal ourselves and reconstruct the
    // revert. Same machinery the engine's recovery path uses on next
    // start — but the service is about to be deleted, so we run it
    // here.
    eprintln!("  service-driven revert not available; attempting offline recovery");
    let journal = framesage_gamemode::journal::Journal::at(&journal_path);
    match journal.read() {
        Ok(Some(entry)) => {
            #[cfg(windows)]
            {
                framesage_sys::game_mode::revert_all(&entry.applied, &entry.previous);
            }
            println!(
                "  offline revert ran for session {} ({} services, {} processes)",
                entry.session_id,
                entry.applied.stopped_services.len(),
                entry.applied.suspended_pids.len()
            );

            // Append to history before deleting — same contract as the
            // engine's revert path (item 1.4). The user later launching
            // the reinstalled service will still see this session in
            // sessions.jsonl.
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let history = framesage_gamemode::journal::SessionHistoryEntry::from_journal(
                &entry,
                now,
                "uninstall",
            );
            if let Err(e) = journal.append_to_history(&history) {
                eprintln!("  history append failed: {e}");
            }
            if let Err(e) = journal.delete() {
                eprintln!("  journal delete failed: {e}");
            }
            report.journal_reverted = true;
        }
        Ok(None) => {
            // Race: journal disappeared between exists() check and read.
        }
        Err(e) => {
            eprintln!("  journal read failed: {e}");
        }
    }
}

#[cfg(windows)]
fn force_kill_service_process() {
    // Use taskkill to force-terminate framesage-svc.exe by image name.
    // We invoke it as a child process so the shell-out is auditable
    // and we don't drag TerminateProcess into the CLI binary.
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/IM", "framesage-svc.exe"])
        .output();
}

#[cfg(windows)]
fn remove_user_shortcuts(report: &mut UninstallReport) {
    // install.ps1 creates three .lnk files. Cleanup mirrors the install
    // — we use the same well-known folder lookups so a Group Policy
    // redirected Startup folder still gets hit.
    let appdata = std::env::var_os("APPDATA").map(std::path::PathBuf::from);
    let userprofile = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from);

    let mut targets: Vec<std::path::PathBuf> = Vec::new();
    if let Some(appdata) = appdata.as_ref() {
        // Start Menu → Programs → FrameSage.lnk
        targets.push(
            appdata
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("FrameSage.lnk"),
        );
        // Startup folder — the load-bearing cleanup target.
        targets.push(
            appdata
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join("Startup")
                .join("FrameSage.lnk"),
        );
    }
    if let Some(profile) = userprofile.as_ref() {
        targets.push(profile.join("Desktop").join("FrameSage.lnk"));
    }

    for target in targets {
        if !target.exists() {
            continue;
        }
        match std::fs::remove_file(&target) {
            Ok(()) => {
                println!("  removed shortcut: {}", target.display());
                report.shortcuts_removed.push(target);
            }
            Err(e) => {
                eprintln!("  shortcut removal failed: {} ({e})", target.display());
                report.shortcuts_failed.push((target, e.to_string()));
            }
        }
    }
}

#[cfg(windows)]
fn remove_install_dir(report: &mut UninstallReport) {
    // Item 1.6 / audit C-10. Clean BOTH the current install location
    // (%ProgramFiles%\FrameSage) and the legacy per-user location
    // (%LOCALAPPDATA%\Programs\FrameSage) so users who installed
    // before the path move don't end up with permanent orphans.
    //
    // Reports against the primary (%ProgramFiles%) install — that's
    // the canonical location post-1.6. Legacy cleanup is silent on
    // success, noisy on failure.
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        candidates.push(std::path::PathBuf::from(pf).join("FrameSage"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            std::path::PathBuf::from(local)
                .join("Programs")
                .join("FrameSage"),
        );
    }

    if let Some(primary) = candidates.first() {
        report.install_dir_path = Some(primary.clone());
    }

    for install_dir in candidates {
        if !install_dir.exists() {
            continue;
        }
        let is_primary = report
            .install_dir_path
            .as_deref()
            .is_some_and(|p| p == install_dir);

        // Walk the dir and remove each known artifact individually so
        // we can report which specific file is locked (vs a single
        // recursive remove that bails on first error with no context).
        let mut any_removed = false;
        for name in &[
            "framesage-tray.exe",
            "framesage-svc.exe",
            "framesage.exe",
            "framesage-sim.exe",
            "README.md",
            "LICENSE",
        ] {
            let path = install_dir.join(name);
            if !path.exists() {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    println!("  removed: {}", path.display());
                    if is_primary {
                        report.binaries_removed.push(path);
                    } else {
                        any_removed = true;
                    }
                }
                Err(e) => {
                    eprintln!("  removal failed: {} ({e})", path.display());
                    if is_primary {
                        report.binaries_failed.push((path, e.to_string()));
                    }
                }
            }
        }

        // Try removing the now-empty dir.
        match std::fs::remove_dir(&install_dir) {
            Ok(()) => {
                println!("  removed install dir: {}", install_dir.display());
                if is_primary {
                    report.install_dir_removed = true;
                } else if any_removed {
                    println!(
                        "  (legacy install dir from pre-1.6 era cleaned up: {})",
                        install_dir.display()
                    );
                }
            }
            Err(_) => {
                println!(
                    "  install dir not removed (contains other files): {}",
                    install_dir.display()
                );
            }
        }
    }
}

#[cfg(windows)]
fn print_uninstall_summary(report: &UninstallReport) {
    println!();
    println!("uninstall summary:");
    println!(
        "  SCM service: {}",
        if report.service_unregistered {
            "removed"
        } else {
            "skipped (not present or removal failed)"
        }
    );
    if report.journal_reverted {
        println!("  Game Mode journal: reverted");
    } else if report.journal_path_for_user.is_some() {
        println!(
            "  Game Mode journal: NOT reverted — system state may have residual modifications. \
             Reboot recommended."
        );
    }
    println!(
        "  shortcuts removed: {} ({} failed)",
        report.shortcuts_removed.len(),
        report.shortcuts_failed.len()
    );
    println!(
        "  binaries removed: {} ({} failed)",
        report.binaries_removed.len(),
        report.binaries_failed.len()
    );
    if !report.binaries_failed.is_empty() {
        println!(
            "  retry uninstall after rebooting if any binary is still locked by AV / running process"
        );
    }

    // %ProgramData% preservation is deliberate. policy.json is the
    // user's authored config; sessions.jsonl is their audit history.
    // We don't auto-delete either — the user can reinstall and recover
    // both. We DO tell them where it is so a manual purge is possible.
    let config_dir = framesage_core::paths::config_dir();
    if config_dir.exists() {
        println!();
        println!(
            "preserved: {} (policy.json + sessions.jsonl)",
            config_dir.display()
        );
        println!(
            "  delete this dir manually if you want a clean uninstall, or leave it for a future reinstall."
        );
    }
}

#[cfg(not(windows))]
fn uninstall_service() -> Result<()> {
    Err(anyhow!("uninstall is Windows-only"))
}

#[cfg(windows)]
fn start_service() -> Result<()> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("open SCM")?;
    let service = manager
        .open_service(SERVICE_NAME, ServiceAccess::START)
        .context("open service")?;
    service.start::<&str>(&[]).context("start service")?;
    println!("started");
    Ok(())
}

#[cfg(not(windows))]
fn start_service() -> Result<()> {
    Err(anyhow!("start is Windows-only"))
}

#[cfg(windows)]
fn stop_service() -> Result<()> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("open SCM")?;
    let service = manager
        .open_service(SERVICE_NAME, ServiceAccess::STOP)
        .context("open service")?;
    service.stop().context("stop service")?;
    println!("stopped");
    Ok(())
}

#[cfg(not(windows))]
fn stop_service() -> Result<()> {
    Err(anyhow!("stop is Windows-only"))
}

#[cfg(windows)]
fn default_service_binary() -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    let dir = exe.parent().context("CLI exe has no parent dir?")?;
    let candidate = dir.join("framesage-svc.exe");
    if candidate.exists() {
        Ok(candidate)
    } else {
        Err(anyhow!(
            "framesage-svc.exe not found next to framesage.exe at {}",
            candidate.display()
        ))
    }
}

// ─── IPC ──────────────────────────────────────────────────────────────────

#[cfg(windows)]
async fn open_pipe(name: &str) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    use tokio::net::windows::named_pipe::ClientOptions;
    ClientOptions::new()
        .open(name)
        .with_context(|| format!("open pipe {name}"))
}

#[cfg(not(windows))]
async fn print_status() -> Result<()> {
    Err(anyhow!("status is Windows-only"))
}

#[cfg(not(windows))]
async fn send_simple(_req: Request) -> Result<()> {
    Err(anyhow!("ipc is Windows-only"))
}

#[cfg(windows)]
async fn send_simple(req: Request) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    // Pick the right pipe based on whether this request mutates state.
    // The admin pipe rejects non-admin callers at the OS layer; the status
    // pipe rejects mutators in the IPC handler. Routing here keeps both
    // outcomes friendly: a `pause` on the status pipe would error with
    // "requires admin pipe" — by going straight to the admin pipe instead,
    // an elevated CLI just works.
    let stream = open_pipe(req.target_pipe()).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half).lines();

    let mut line = serde_json::to_vec(&req)?;
    line.push(b'\n');
    write_half.write_all(&line).await?;
    write_half.flush().await?;

    if let Some(resp_line) = reader.next_line().await? {
        let resp: Response = serde_json::from_str(&resp_line)?;
        match resp {
            Response::Ok => println!("ok"),
            Response::Status(_) => println!("ok"),
            Response::Processes { .. } => println!("ok"),
            Response::Services { services } => {
                println!("ok ({} services)", services.len())
            }
            Response::UndoResult { undone } => match undone {
                Some(summary) => {
                    println!("{}", summary.summary);
                    if let Some(failure) = summary.failure {
                        println!("(reverse failed: {failure})");
                    }
                }
                None => println!("nothing to undo"),
            },
            Response::UndoLog { entries } => print_undo_log(&entries),
            Response::Error { message } => return Err(anyhow!(message)),
        }
    }
    Ok(())
}

/// Format an undo-log snapshot for `framesage undo list`. Newest entry
/// first; each row is `id  YYYY-MM-DD HH:MM:SS  description`. Item 3.5.
fn print_undo_log(entries: &[framesage_core::UndoEntry]) {
    if entries.is_empty() {
        println!("undo log is empty");
        return;
    }
    println!(
        "{:<6}  {:<19}  description",
        "id", "timestamp (UTC)"
    );
    for e in entries {
        let ts = format_unix_local_or_utc(e.at_unix_secs);
        println!("{:<6}  {:<19}  {}", e.id, ts, e.action.describe());
    }
}

/// `unix_secs` → `YYYY-MM-DD HH:MM:SS UTC`. The CLI emits UTC so the
/// output is stable across machines / users / log captures and
/// doesn't drag in a chrono dependency or the `windows` crate.
/// (The tray's Activity Log already does local-time formatting via
/// its existing windows-rs dep; the CLI's row table is the place
/// where stable UTC is the better default.)
fn format_unix_local_or_utc(secs: u64) -> String {
    let days = secs / 86_400;
    let h = (secs / 3_600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    // Days since epoch → Y-M-D via a simple Gregorian walk. Cheap and
    // covers the next ~thousand years without external deps.
    let (y, mo, d) = epoch_days_to_ymd(days as i64);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}")
}

/// Convert days-since-unix-epoch to (year, month, day). Algorithm
/// from Howard Hinnant's date library (public domain) — handles
/// the Gregorian calendar correctly and runs in constant time.
fn epoch_days_to_ymd(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468; // shift epoch from 1970-01-01 to 0000-03-01
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146097)
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = y + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

#[cfg(windows)]
async fn print_status() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    // Status is read-only — always use the status pipe so an unprivileged
    // CLI invocation works without UAC.
    let stream = open_pipe(Request::Status.target_pipe()).await?;
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half).lines();

    let mut line = serde_json::to_vec(&Request::Status)?;
    line.push(b'\n');
    write_half.write_all(&line).await?;
    write_half.flush().await?;

    let Some(resp_line) = reader.next_line().await? else {
        return Err(anyhow!("service closed pipe without responding"));
    };
    let resp: Response = serde_json::from_str(&resp_line)?;
    match resp {
        Response::Status(s) => print_status_snapshot(&s),
        Response::Ok => println!("ok"),
        Response::Processes { .. } => println!("ok"),
        Response::Services { .. } => println!("ok"),
        Response::UndoResult { .. } | Response::UndoLog { .. } => println!("ok"),
        Response::Error { message } => return Err(anyhow!(message)),
    }
    Ok(())
}

fn print_status_snapshot(s: &StatusSnapshot) {
    println!("paused: {}", s.paused);
    println!("rules:  {}", s.policy.rules.len());
    println!("default profile: {}", s.policy.default_profile);
    match &s.foreground {
        Some(fg) => println!(
            "foreground: pid={} exe={} title={:?}",
            fg.pid, fg.exe_name, fg.title
        ),
        None => println!("foreground: <none>"),
    }
    match &s.active_profile {
        Some(p) => println!("active profile: {} ({})", p.id, p.description),
        None => println!("active profile: <none>"),
    }
}
