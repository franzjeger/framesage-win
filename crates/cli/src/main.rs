//! framesage.exe — CLI for installing the service and talking to it.
//!
//! Subcommands map roughly 1:1 to `framesage_ipc::Request`. The exception is
//! `install` / `uninstall` / `start` / `stop`, which talk to the SCM directly.

#![cfg_attr(not(windows), allow(unused_imports, dead_code))]

use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use tracing::info;

use framesage_core::ProfileId;
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
    /// Game Mode controls — status, panic-off, safe-list inspection.
    #[command(subcommand)]
    GameMode(GameModeCmd),
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
        Cmd::GameMode(sub) => match sub {
            GameModeCmd::Status => tokio_block(async { print_game_mode_status().await }),
            GameModeCmd::Off => tokio_block(async { send_simple(Request::GameModeOff).await }),
            GameModeCmd::SafeList => {
                print_safe_list();
                Ok(())
            }
        },
    }
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
    use windows_service::service::{
        ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceType,
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

    info!(service = SERVICE_NAME, "installed");
    println!("installed: {SERVICE_NAME}");
    Ok(())
}

#[cfg(not(windows))]
fn install_service(_bin: Option<&str>) -> Result<()> {
    Err(anyhow!("install is Windows-only"))
}

#[cfg(windows)]
fn uninstall_service() -> Result<()> {
    use windows_service::service::ServiceAccess;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("open SCM")?;
    let service = manager
        .open_service(SERVICE_NAME, ServiceAccess::DELETE)
        .context("open service")?;
    service.delete().context("delete service")?;

    println!("uninstalled: {SERVICE_NAME}");
    Ok(())
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
async fn open_pipe() -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    use tokio::net::windows::named_pipe::ClientOptions;
    ClientOptions::new()
        .open(framesage_ipc::PIPE_NAME)
        .with_context(|| format!("open pipe {}", framesage_ipc::PIPE_NAME))
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

    let stream = open_pipe().await?;
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
            Response::Error { message } => return Err(anyhow!(message)),
        }
    }
    Ok(())
}

#[cfg(windows)]
async fn print_status() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let stream = open_pipe().await?;
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
