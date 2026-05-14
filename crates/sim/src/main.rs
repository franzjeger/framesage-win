//! framesage-sim — drive the policy and topology resolver against synthetic
//! foreground events on any host.
//!
//! Real Windows hardware is the only place to run the full service end-to-end,
//! but every decision the engine makes is `Policy::match_foreground` followed
//! by `CpuTopology::resolve` — both of which are platform-agnostic. This
//! binary exercises those on macOS or Linux so we can iterate on rules,
//! profiles, and CCD/CPU selectors without rebooting into Windows.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use framesage_core::{paths, AppMatch, CoreKind, CpuTopology, LogicalCpu, Policy};

#[derive(Parser, Debug)]
#[command(
    name = "framesage-sim",
    version,
    about = "framesage policy/topology dev harness"
)]
struct Cli {
    /// Path to a policy.json. Defaults to the platform's standard location.
    #[arg(long, global = true)]
    policy: Option<PathBuf>,

    /// Synthetic CPU topology to resolve against.
    #[arg(long, global = true, value_enum, default_value_t = TopologyChoice::Dual7950)]
    topology: TopologyChoice,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy)]
enum TopologyChoice {
    /// 8-thread synthetic (4 cores, 2 CCDs of 2). Smallest useful shape.
    Dual4,
    /// 16-thread synthetic shaped like a 7800X3D. 1 CCD, all Cache.
    Single7800,
    /// 32-thread synthetic shaped like a 7950X3D / 9950X3D. 2 CCDs, X3D on
    /// CCD 0, Performance on CCD 1.
    Dual7950,
    /// 24-thread Intel hybrid shape: 8 P-cores (SMT) + 8 E-cores.
    Hybrid24,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Print the active policy as resolved (paths, rule count, default).
    Policy,

    /// Show the synthetic topology that would be used to resolve selectors.
    Topology,

    /// Look up the profile that would be applied to a given foreground app
    /// and print which logical CPUs the selector resolves to.
    Match {
        /// Executable file name, e.g. `bf6.exe`.
        exe: String,

        /// Full path of the exe — used by `path_contains` rules.
        #[arg(long, default_value = "")]
        path: String,

        /// Window title — used by `window_title_contains` rules.
        #[arg(long, default_value = "")]
        title: String,
    },

    /// Run a fixed demo: walk every rule in the policy plus a couple of
    /// unmatched apps and print the decision for each. Useful for sanity-
    /// checking a freshly-edited policy.
    Demo,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let cli = Cli::parse();

    let policy = load_policy(cli.policy.as_deref())?;
    let topology = build_topology(cli.topology);

    match cli.cmd {
        Cmd::Policy => print_policy_summary(&policy),
        Cmd::Topology => print_topology(&topology),
        Cmd::Match { exe, path, title } => {
            print_match(&policy, &topology, &exe, &path, &title);
        }
        Cmd::Demo => run_demo(&policy, &topology),
    }
    Ok(())
}

fn load_policy(arg: Option<&std::path::Path>) -> Result<Policy> {
    let path = arg
        .map(|p| p.to_path_buf())
        .unwrap_or_else(paths::policy_path);
    if path.exists() {
        Policy::load(&path).with_context(|| format!("loading {}", path.display()))
    } else {
        // Don't write a default on a developer machine — just return the
        // in-memory default so the sim is read-only by default.
        Ok(Policy::default())
    }
}

fn print_policy_summary(policy: &Policy) {
    println!("default profile: {}", policy.default_profile);
    if let Some(bg) = &policy.background_profile {
        println!("background profile: {bg}");
    }
    println!("tick_ms: {}", policy.tick_ms);
    println!("profiles ({}):", policy.profiles.len());
    let mut names: Vec<&framesage_core::ProfileId> = policy.profiles.keys().collect();
    names.sort_by(|a, b| a.0.cmp(&b.0));
    for n in names {
        let p = &policy.profiles[n];
        println!(
            "  - {} — {}",
            n,
            if p.description.is_empty() {
                "(no description)"
            } else {
                &p.description
            }
        );
    }
    println!("rules ({}):", policy.rules.len());
    for r in &policy.rules {
        let m = match &r.r#match {
            AppMatch::ExeName(n) => format!("exe={n}"),
            AppMatch::PathContains(s) => format!("path~={s}"),
            AppMatch::WindowTitleContains(s) => format!("title~={s}"),
        };
        println!("  - {} -> {}  ({})", m, r.profile, r.note);
    }
}

fn print_topology(topology: &CpuTopology) {
    println!("topology: {} logical CPUs", topology.count());
    for cpu in &topology.cpus {
        println!(
            "  cpu{:2}  core={:2}  ccd={}  kind={:?}  rank={:?}  smt={}",
            cpu.index, cpu.physical_core, cpu.ccd, cpu.kind, cpu.cppc_rank, cpu.is_smt_sibling
        );
    }
}

fn print_match(policy: &Policy, topology: &CpuTopology, exe: &str, path: &str, title: &str) {
    let profile_id = policy.match_foreground(exe, path, title);
    println!("foreground: {exe}");
    if !path.is_empty() {
        println!("  path:  {path}");
    }
    if !title.is_empty() {
        println!("  title: {title}");
    }
    println!("  → matched profile: {profile_id}");

    let Some(profile) = policy.profile(profile_id) else {
        println!("  ! profile id not found in policy.profiles");
        return;
    };
    if !profile.description.is_empty() {
        println!("    {}", profile.description);
    }

    if let Some(sel) = &profile.cpu_sets {
        let resolved = topology.resolve(sel);
        println!("    cpu_sets {:?} → {:?}", sel, resolved);
    }
    if let Some(sel) = &profile.affinity_mask {
        let resolved = topology.resolve(sel);
        println!("    affinity {:?} → {:?}", sel, resolved);
    }
    if let Some(pt) = profile.power_throttling {
        println!("    power_throttling: {pt:?}");
    }
    if let Some(pc) = profile.priority_class {
        println!("    priority_class: {pc:?}");
    }
    if let Some(io) = profile.io_priority {
        println!("    io_priority: {io:?}");
    }
    if let Some(mp) = profile.memory_priority {
        println!("    memory_priority: {mp:?}");
    }

    if let Some(actions) = &profile.game_mode {
        print_game_mode_dry_run(actions);
    }
}

/// Show what Game Mode *would* do given a synthetic OS state. Resolves
/// against the curated safe-list (which is identical to what the engine sees
/// at runtime) but uses an in-memory `SystemStateQuery` fake — so we can
/// inspect plans without touching the real OS.
fn print_game_mode_dry_run(actions: &framesage_core::GameModeActions) {
    use framesage_gamemode::{
        planner::{plan, PlannedAction, SystemStateQuery},
        safe_list::SafeList,
        state::ServiceStatus,
    };

    println!("    game-mode actions requested:");
    if actions.hide_taskbar {
        println!("      - hide_taskbar");
    }
    if let Some(plan_id) = &actions.power_plan {
        println!("      - power_plan: {:?} ({})", plan_id, plan_id.guid());
    }
    if let Some(fa) = actions.focus_assist {
        println!("      - focus_assist: {fa:?} (stubbed in v0.1)");
    }
    if actions.pause_windows_update {
        println!("      - pause_windows_update (stubbed in v0.1)");
    }
    if !actions.stop_services.is_empty() {
        println!(
            "      - stop_services: {}",
            actions.stop_services.join(", ")
        );
    }
    if !actions.suspend_processes.is_empty() {
        println!(
            "      - suspend_processes: {}",
            actions.suspend_processes.join(", ")
        );
    }

    // Synthetic state: everything is in "ready for game mode" position so
    // every requested action gets planned.
    struct SyntheticQuery;
    impl SystemStateQuery for SyntheticQuery {
        fn taskbar_visible(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
        fn active_power_plan(&self) -> anyhow::Result<Option<framesage_core::PowerPlanId>> {
            Ok(Some(framesage_core::PowerPlanId::Balanced))
        }
        fn service_status(&self, _id: &str) -> anyhow::Result<ServiceStatus> {
            Ok(ServiceStatus::Running)
        }
        fn pids_by_exe(&self, exe: &str) -> anyhow::Result<Vec<(u32, String)>> {
            // Pretend each safe-listed exe has one running PID; derive it
            // from the exe name so distinct exes get distinct synthetic PIDs
            // (the planner dedupes by PID, so collisions would hide cases).
            let pid = 10_000 + (exe.bytes().map(|b| b as u32).sum::<u32>() % 9000);
            Ok(vec![(pid, exe.to_string())])
        }
    }

    let result = plan(actions, SafeList::bundled(), &SyntheticQuery);
    match result {
        Ok(plan_out) => {
            if plan_out.is_empty() && plan_out.rejections.is_empty() {
                println!("    game-mode: no actionable items after safe-list filter");
            } else {
                println!("    game-mode plan ({} actions):", plan_out.actions.len());
                for a in &plan_out.actions {
                    let line = match a {
                        PlannedAction::HideTaskbar => "hide taskbar".to_string(),
                        PlannedAction::SetPowerPlan { from, to } => format!(
                            "power plan {} → {}",
                            from.as_ref()
                                .map(|p| format!("{p:?}"))
                                .unwrap_or_else(|| "<unknown>".into()),
                            format_args!("{to:?}")
                        ),
                        PlannedAction::StopService { id, was_status } => {
                            format!("stop service {id} (was {was_status:?})")
                        }
                        PlannedAction::SuspendProcess { pid, exe } => {
                            format!("suspend process {exe} pid={pid}")
                        }
                        PlannedAction::SetFocusAssist(mode) => {
                            format!("set focus assist {mode:?} [stub]")
                        }
                        PlannedAction::PauseWindowsUpdate => {
                            "pause Windows Update [stub]".to_string()
                        }
                    };
                    println!("      - {line}");
                }
                for r in &plan_out.rejections {
                    println!(
                        "      ! rejected {}: {} ({})",
                        r.id,
                        r.reason,
                        format!("{:?}", r.kind).to_lowercase()
                    );
                }
            }
        }
        Err(e) => {
            println!("    game-mode plan failed: {e}");
        }
    }
}

fn run_demo(policy: &Policy, topology: &CpuTopology) {
    let mut probes: Vec<(String, String, String)> = policy
        .rules
        .iter()
        .map(|r| match &r.r#match {
            AppMatch::ExeName(n) => (n.clone(), String::new(), String::new()),
            AppMatch::PathContains(s) => (
                "example.exe".into(),
                format!(r"C:\Program Files\{s}\app.exe"),
                String::new(),
            ),
            AppMatch::WindowTitleContains(s) => {
                ("example.exe".into(), String::new(), format!("App {s}"))
            }
        })
        .collect();
    probes.push(("notepad.exe".into(), String::new(), String::new()));
    probes.push(("explorer.exe".into(), String::new(), String::new()));

    for (exe, path, title) in probes {
        println!("───────────────");
        print_match(policy, topology, &exe, &path, &title);
    }
}

// ─── synthetic topologies ─────────────────────────────────────────────────

fn build_topology(choice: TopologyChoice) -> CpuTopology {
    match choice {
        TopologyChoice::Dual4 => mk_dual_ccd(2, 2, true),
        TopologyChoice::Single7800 => mk_single_ccd(8, CoreKind::Cache),
        TopologyChoice::Dual7950 => mk_dual_ccd(8, 8, true),
        TopologyChoice::Hybrid24 => mk_intel_hybrid(8, 8),
    }
}

fn mk_single_ccd(cores: u32, kind: CoreKind) -> CpuTopology {
    let mut cpus = Vec::new();
    for c in 0..cores {
        for smt in 0..2 {
            cpus.push(LogicalCpu {
                index: c * 2 + smt,
                physical_core: c,
                ccd: 0,
                kind,
                cppc_rank: Some(120 - c),
                is_smt_sibling: smt == 1,
            });
        }
    }
    CpuTopology { cpus }
}

fn mk_dual_ccd(cache_cores: u32, perf_cores: u32, smt: bool) -> CpuTopology {
    let mut cpus = Vec::new();
    let mut idx = 0u32;
    let smt_count = if smt { 2 } else { 1 };
    for c in 0..cache_cores {
        for s in 0..smt_count {
            cpus.push(LogicalCpu {
                index: idx,
                physical_core: c,
                ccd: 0,
                kind: CoreKind::Cache,
                cppc_rank: Some(95 - c),
                is_smt_sibling: s == 1,
            });
            idx += 1;
        }
    }
    for c in 0..perf_cores {
        for s in 0..smt_count {
            cpus.push(LogicalCpu {
                index: idx,
                physical_core: cache_cores + c,
                ccd: 1,
                kind: CoreKind::Performance,
                cppc_rank: Some(130 - c),
                is_smt_sibling: s == 1,
            });
            idx += 1;
        }
    }
    CpuTopology { cpus }
}

fn mk_intel_hybrid(p_cores: u32, e_cores: u32) -> CpuTopology {
    let mut cpus = Vec::new();
    let mut idx = 0u32;
    // P-cores with SMT.
    for c in 0..p_cores {
        for s in 0..2 {
            cpus.push(LogicalCpu {
                index: idx,
                physical_core: c,
                ccd: 0,
                kind: CoreKind::Performance,
                cppc_rank: Some(140 - c),
                is_smt_sibling: s == 1,
            });
            idx += 1;
        }
    }
    // E-cores without SMT.
    for c in 0..e_cores {
        cpus.push(LogicalCpu {
            index: idx,
            physical_core: p_cores + c,
            ccd: 0,
            kind: CoreKind::Efficiency,
            cppc_rank: Some(70 - c),
            is_smt_sibling: false,
        });
        idx += 1;
    }
    CpuTopology { cpus }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framesage_core::CpuSelector;

    #[test]
    fn dual7950_default_game_profile_lands_on_cache_ccd() {
        let policy = Policy::default();
        let topology = build_topology(TopologyChoice::Dual7950);

        let pid = policy.match_foreground("bf6.exe", "", "");
        let profile = policy.profile(pid).expect("game profile present");
        let sel = profile.cpu_sets.as_ref().expect("cpu_sets set");

        let resolved = topology.resolve(sel);
        assert!(!resolved.is_empty());
        for idx in resolved {
            let cpu = topology
                .cpus
                .iter()
                .find(|c| c.index == idx)
                .expect("cpu exists");
            assert_eq!(cpu.kind, CoreKind::Cache, "game must land on Cache CCD");
        }
    }

    #[test]
    fn unmatched_exe_falls_back_to_default_perf_profile() {
        let policy = Policy::default();
        let pid = policy.match_foreground("notepad.exe", "", "");
        assert_eq!(pid.0, "perf");
    }

    #[test]
    fn hybrid_topology_has_p_and_e_cores() {
        let topology = build_topology(TopologyChoice::Hybrid24);
        assert_eq!(topology.count(), 24);
        let p_count = topology.cpus_of_kind(CoreKind::Performance).count();
        let e_count = topology.cpus_of_kind(CoreKind::Efficiency).count();
        assert_eq!(p_count, 16, "8 P-cores × 2 SMT");
        assert_eq!(e_count, 8, "8 E-cores × 1 SMT");
    }

    #[test]
    fn topready_selector_picks_p_cores_first_on_hybrid() {
        // Game profile in the default policy uses Kind(Cache). On hybrid with
        // no Cache cores, that resolves to empty — sanity-check, plus verify
        // TopRanked still does the right thing.
        let topology = build_topology(TopologyChoice::Hybrid24);
        let resolved = topology.resolve(&CpuSelector::TopRanked(8));
        assert_eq!(resolved.len(), 8);
        for idx in resolved {
            let cpu = topology.cpus.iter().find(|c| c.index == idx).unwrap();
            assert_eq!(cpu.kind, CoreKind::Performance);
            assert!(!cpu.is_smt_sibling);
        }
    }
}
