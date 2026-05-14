//! framesage policy engine.
//!
//! The engine sits between the OS (via `framesage-sys`) and the user-facing
//! configuration (a `Policy`). Each tick it asks: what is the foreground
//! process right now? — and reconciles the running state against what the
//! policy says it should be.
//!
//! Two dimensions of state are tracked:
//!
//! 1. **Per-process** state — what we changed on the foregrounded process
//!    (CPU Sets, Power Throttling, priority, etc.). Reverted automatically
//!    when focus moves to a different process.
//!
//! 2. **System-wide Game Mode state** — taskbar visibility, stopped services,
//!    suspended background processes, switched power plan. Reverted when the
//!    active profile changes to one that doesn't request Game Mode, or when
//!    the panic button (`framesage game-mode off`) fires.
//!
//! Design notes:
//!
//! * **Reconcile, don't event-chase.** Windows has a `WinEventHook` for focus
//!   changes, but real games still spawn helper windows, fullscreen-flip,
//!   minimise, etc., and the focus events lie often enough that a 250–500ms
//!   polled reconcile is more robust *and* simpler.
//! * **Track what we applied, so we can revert.** Per-process state lives in
//!   `applied`; system state lives in a crash-safe journal so a process kill
//!   doesn't strand the user with a hidden taskbar and stopped services.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

pub mod probalance;

use framesage_core::{CpuTopology, GameModeActions, Policy, Profile, ProfileId};
use framesage_gamemode::{
    journal::{Journal, JournalEntry},
    planner::{plan as plan_game_mode, ActionPlan, PlannedAction, SystemStateQuery},
    safe_list::SafeList,
    state::{AppliedActions, PreviousState, SuspendedProcessSnapshot},
};
use framesage_ipc::{Event, ForegroundSnapshot, ProcessSnapshot, StatusSnapshot, SystemMetrics};

/// Dependencies the engine needs at construction. Passing as a struct keeps
/// the call sites readable (we already have policy + topology, and now journal
/// + safe_list join them) and gives us room to grow without breaking callers.
pub struct EngineDeps {
    pub policy: Policy,
    pub topology: CpuTopology,
    pub safe_list: &'static SafeList,
    pub journal: Journal,
}

pub struct Engine {
    state: Arc<RwLock<EngineState>>,
    events: broadcast::Sender<Event>,
    safe_list: &'static SafeList,
    journal: Journal,
}

struct EngineState {
    policy: Policy,
    topology: CpuTopology,
    paused: bool,
    /// What we've applied per-pid, so we can revert per-process state.
    applied: HashMap<u32, AppliedRecord>,
    /// Currently-foregrounded pid, if any.
    current_foreground: Option<u32>,
    /// Last-seen foreground details (so Status doesn't have to syscall).
    foreground_snapshot: Option<ForegroundSnapshot>,
    active_profile: Option<ProfileId>,
    /// Whatever system-wide Game Mode we've entered, if any. Mirrored to the
    /// journal on disk so a crash leaves recoverable state.
    system_mode: Option<ActiveSystemMode>,
    /// Last time we walked the process list to enforce `background_profile`.
    /// `None` until the first scan; subsequent scans honour
    /// `BACKGROUND_SCAN_INTERVAL`.
    last_background_scan: Option<Instant>,
    /// Last time we re-pushed kernel state for every persistent-profile PID.
    /// `None` until the first sweep; subsequent sweeps honour
    /// `PERSISTENT_REASSERT_INTERVAL`.
    last_persistent_reassert: Option<Instant>,
    /// Per-PID CPU-time + exe-name snapshot from the previous ProBalance
    /// sample. Used to compute deltas (CPU% over the inter-sample window).
    /// Keyed by PID; entries for PIDs that have exited are reaped each
    /// sample. Empty when ProBalance is disabled.
    probalance_prev_samples: HashMap<u32, ProBalancePrevSample>,
    /// Timestamp of the last ProBalance sample, paired with
    /// `probalance_prev_samples`. The wall-clock delta between this and
    /// `Instant::now()` divides the CPU-time delta into a utilisation %.
    probalance_last_sample_at: Option<Instant>,
    /// Active ProBalance restraints — PID → original priority class + exe.
    /// Populated when `probalance::decide` returns `Decision::Restrain`,
    /// drained on `Decision::Restore`.
    probalance_restrained: HashMap<u32, probalance::RestrainedRecord>,
    /// Per-PID CPU-time snapshot from the previous call to
    /// `list_process_snapshots`. Independent of `probalance_prev_samples`
    /// because the Processes tab needs CPU% whether or not the user has
    /// ProBalance enabled. Updated on every IPC `ListProcesses` request.
    list_processes_prev_samples: HashMap<u32, u64>,
    /// Wall-clock instant matching `list_processes_prev_samples`. Used to
    /// turn CPU-time deltas into a % of one logical CPU.
    list_processes_last_sample_at: Option<Instant>,
    /// Previous system-wide CPU-time sample (`GetSystemTimes`). Subtracted
    /// from the current sample inside `list_process_snapshots` to derive
    /// the live system CPU% surfaced in the performance band.
    list_processes_prev_system_cpu: Option<framesage_sys::process::SystemCpuTimes>,
    /// Previous per-logical-CPU sample, parallel to
    /// `list_processes_prev_system_cpu` but populated from
    /// `framesage_sys::process::per_cpu_times`. `None` until the first
    /// successful sample; we only emit `per_core_cpu_percent` once we have
    /// two samples to diff. Empty Vec on platforms / hardware where the NT
    /// per-CPU query failed.
    list_processes_prev_per_cpu: Option<Vec<framesage_sys::process::PerCpuTimes>>,
    /// Per-exe-path version-info cache. A `Some(VersionInfo)` entry means
    /// "we tried to read the binary's version resource and here's what we
    /// got" — any of the inner `Option<String>` fields can be `None` if
    /// the resource omitted that specific field; an empty `VersionInfo`
    /// (all fields `None`) means the binary has no resource at all. A
    /// missing key means we haven't tried yet. We never evict — paths are
    /// stable for the lifetime of an installed binary, and the cache stays
    /// small (~200 entries × ~150 bytes).
    version_info_cache: HashMap<String, framesage_sys::version_info::VersionInfo>,
    /// Manual mode: when set, every foreground reconcile applies this
    /// profile instead of consulting Rules. Stays set across focus
    /// changes until explicitly cleared via `clear_manual_override` /
    /// `Request::ClearManualOverride`.
    manual_override: Option<ProfileId>,
    /// Foreground reported by a user-session helper (typically the tray).
    /// `None` means "the user session is idle / on lock screen / no
    /// foreground"; the tick should treat it the same as
    /// `framesage_sys::foreground::current()` returning None.
    ///
    /// Set by [`Self::report_foreground`] from the IPC handler. The
    /// service running as LocalSystem in session 0 can't call
    /// `GetForegroundWindow` itself — that returns null cross-session —
    /// so the engine prefers the report when one is fresh, falling back
    /// to the session-local poll only if no report has ever arrived
    /// (covers the console-mode dev path).
    reported_foreground: Option<framesage_sys::foreground::ForegroundInfo>,
    /// True once at least one IPC ReportForeground arrived. Lets the
    /// tick path distinguish "user-session helper is connected and the
    /// desktop is idle" from "no helper has ever reported, fall back to
    /// session-local polling".
    foreground_reporter_seen: bool,
}

/// How often the engine walks all PIDs to apply `Policy::background_profile`.
/// Tuned for "low enough that newly-spawned background apps get throttled
/// promptly, high enough not to thrash the OpenProcess/CloseHandle pair on
/// every tick." Each scan touches only PIDs not already in `applied`, so the
/// steady-state cost is one ToolHelp snapshot + a Vec membership check.
const BACKGROUND_SCAN_INTERVAL: Duration = Duration::from_secs(10);

/// How often the engine re-pushes kernel state (affinity, CPU sets, priority,
/// I/O priority) for every PID running under a `persistent` profile. Some
/// games (POE2, EVE, several Unreal titles) call `SetProcessAffinityMask` on
/// themselves at startup or on resolution changes, "fixing" what they think
/// is a misconfiguration. CPU Sets are also advisory — the scheduler can
/// override them under contention. Re-pushing every 2 s defeats both modes
/// of override and is cheap: each sweep is one `SetProcess*` call per knob
/// per managed PID, and persistent-profile PIDs are typically 1–3 (games,
/// not background apps).
const PERSISTENT_REASSERT_INTERVAL: Duration = Duration::from_secs(2);

/// How often the engine samples per-PID CPU usage for ProBalance. 1 s gives
/// reasonable accuracy (a process that's busy for 200 ms of every second
/// shows up at ~20%) without thrashing OpenProcess. Skipped entirely when
/// `policy.probalance.enabled == false`.
const PROBALANCE_SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);

/// Cached fields from the previous ProBalance sample.
#[derive(Debug, Clone, Copy)]
struct ProBalancePrevSample {
    /// `kernel + user` 100-ns ticks from `GetProcessTimes` at last sample.
    total_cpu_100ns: u64,
}

struct AppliedRecord {
    profile_id: ProfileId,
    /// Image filename (without path) captured at apply time. Used by the
    /// periodic re-assert sweep to defend against PID reuse: if the PID's
    /// current exe doesn't match `exe_name`, the original process is gone
    /// and Windows reassigned the PID — we drop the record and never push
    /// our settings onto an unrelated process. Compared case-insensitively.
    exe_name: String,
    /// Opaque per-platform state used to revert per-process changes.
    #[cfg(windows)]
    state: framesage_sys::apply::AppliedState,
    #[cfg(not(windows))]
    _phantom: (),
}

/// What we entered into Game Mode for; mirrors the journal on disk.
#[derive(Debug, Clone)]
struct ActiveSystemMode {
    profile_id: ProfileId,
    previous: PreviousState,
    applied: AppliedActions,
    /// The session UUID, kept so logs and status can correlate with the
    /// journal file we wrote.
    journal_session_id: uuid::Uuid,
}

impl Engine {
    /// Construct an engine with full dependencies.
    pub fn new(deps: EngineDeps) -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            state: Arc::new(RwLock::new(EngineState {
                policy: deps.policy,
                topology: deps.topology,
                paused: false,
                applied: HashMap::new(),
                current_foreground: None,
                foreground_snapshot: None,
                active_profile: None,
                system_mode: None,
                last_background_scan: None,
                last_persistent_reassert: None,
                probalance_prev_samples: HashMap::new(),
                probalance_last_sample_at: None,
                probalance_restrained: HashMap::new(),
                list_processes_prev_samples: HashMap::new(),
                list_processes_last_sample_at: None,
                list_processes_prev_system_cpu: None,
                list_processes_prev_per_cpu: None,
                version_info_cache: HashMap::new(),
                manual_override: None,
                reported_foreground: None,
                foreground_reporter_seen: false,
            })),
            events: tx,
            safe_list: deps.safe_list,
            journal: deps.journal,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub fn pause(&self) {
        let mut s = self.state.write();
        if !s.paused {
            s.paused = true;
            let _ = self.events.send(Event::Paused);
            info!("engine paused");
        }
    }

    pub fn resume(&self) {
        let mut s = self.state.write();
        if s.paused {
            s.paused = false;
            let _ = self.events.send(Event::Resumed);
            info!("engine resumed");
        }
    }

    pub fn set_policy(&self, policy: Policy) {
        self.state.write().policy = policy;
        info!("policy replaced");
    }

    /// One-off priority change against any live PID. Bypasses the profile
    /// system — used by the Processes tab's right-click "Set priority"
    /// submenu. If the PID is currently managed by us via a rule, the
    /// next reconcile (or re-assert tick for persistent profiles) will
    /// overwrite this with the rule's class; that's the right semantic —
    /// the rule still wins.
    pub fn set_process_priority(
        &self,
        pid: u32,
        class: framesage_core::PriorityClass,
    ) -> Result<()> {
        #[cfg(windows)]
        {
            framesage_sys::apply::set_priority_class_for_pid(pid, class)?;
        }
        #[cfg(not(windows))]
        {
            let _ = (pid, class);
        }
        Ok(())
    }

    pub fn status(&self) -> StatusSnapshot {
        let s = self.state.read();
        let active_profile = s
            .active_profile
            .as_ref()
            .and_then(|id| s.policy.profile(id).cloned());
        StatusSnapshot {
            paused: s.paused,
            policy: s.policy.clone(),
            foreground: s.foreground_snapshot.clone(),
            active_profile,
            manual_override: s.manual_override.clone(),
        }
    }

    /// Collect a snapshot row for every visible process plus paired
    /// system-wide metrics (CPU%, memory). Backs the tray's Processes tab +
    /// the permanent performance band. Self-contained: opens its own
    /// handles, manages its own per-PID CPU-time history so the % is
    /// computed even when ProBalance is disabled.
    ///
    /// Costs ~1 ToolHelp snapshot + 4 `OpenProcess`/`CloseHandle` pairs per
    /// PID (priority, affinity, mem, cpu_times) + 2 cheap system syscalls
    /// (`GetSystemTimes`, `GlobalMemoryStatusEx`). On a 200-process machine
    /// that's ~800 syscalls per call — fine at 1 Hz from the tray, not fine
    /// at 100 Hz, which is why the tray polls.
    pub fn list_process_snapshots(&self) -> (Vec<ProcessSnapshot>, SystemMetrics) {
        #[cfg(not(windows))]
        return (Vec::new(), SystemMetrics::default());
        #[cfg(windows)]
        {
            let now = Instant::now();
            let mut s = self.state.write();
            let elapsed = match s.list_processes_last_sample_at {
                Some(prev) => now.duration_since(prev),
                None => Duration::ZERO,
            };
            let elapsed_100ns = elapsed
                .as_secs()
                .saturating_mul(10_000_000)
                .saturating_add(elapsed.subsec_nanos() as u64 / 100);
            s.list_processes_last_sample_at = Some(now);

            let pid_snapshots = match framesage_sys::process::iter_pid_snapshots() {
                Ok(v) => v,
                Err(e) => {
                    warn!(error = %e, "list_process_snapshots: iter_pid_snapshots failed");
                    return (Vec::new(), SystemMetrics::default());
                }
            };

            let mut new_prev: HashMap<u32, u64> = HashMap::with_capacity(pid_snapshots.len());
            let mut out: Vec<ProcessSnapshot> = Vec::with_capacity(pid_snapshots.len());
            // Bound how many fresh version-info reads we do this tick.
            // GetFileVersionInfoW is fast (~0.5ms) but I/O-touching; capping
            // at 8 per tick keeps the worst-case cost predictable when a
            // fresh process list paints for the first time. The cache is
            // populated lazily over a few seconds rather than all-at-once.
            let mut version_info_budget: u32 = 8;

            for ps in &pid_snapshots {
                let pid = ps.pid;
                if pid == 0 {
                    continue;
                }

                // exe path + bare filename. We surface BOTH in the snapshot:
                // the tray uses the filename for the table label and matching,
                // and the full path to extract the exe's icon via
                // `SHGetFileInfoW`.
                let exe_path = match framesage_sys::process::exe_for_pid(pid) {
                    Ok(Some(path)) => path,
                    Ok(None) | Err(_) => continue,
                };
                let exe_name = exe_path
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&exe_path)
                    .to_owned();

                let priority_class_raw = framesage_sys::apply::get_priority_class_for_pid(pid)
                    .ok()
                    .flatten()
                    .unwrap_or(0);

                let affinity_mask = framesage_sys::process::affinity_mask(pid)
                    .ok()
                    .flatten()
                    .unwrap_or(0);

                let memory_bytes = framesage_sys::process::working_set_bytes(pid)
                    .ok()
                    .flatten()
                    .unwrap_or(0);

                let total_cpu = framesage_sys::process::cpu_times(pid)
                    .ok()
                    .flatten()
                    .map(|t| t.total_100ns())
                    .unwrap_or(0);
                let cpu_percent: u16 = if elapsed_100ns > 0 {
                    let prev = s
                        .list_processes_prev_samples
                        .get(&pid)
                        .copied()
                        .unwrap_or(0);
                    if prev == 0 {
                        0 // first time we see this PID — no delta yet
                    } else {
                        let delta = total_cpu.saturating_sub(prev);
                        ((delta as u128).saturating_mul(100) / elapsed_100ns as u128)
                            .min(u16::MAX as u128) as u16
                    }
                } else {
                    0
                };
                new_prev.insert(pid, total_cpu);

                // Rule match: ask the policy matcher. We don't have window
                // title here (no foreground info for arbitrary PIDs), so
                // title-based rules won't fire on the Processes view — that's
                // fine, they're inherently foreground-scoped.
                let rule_note = s
                    .policy
                    .rules
                    .iter()
                    .find(|r| r.r#match.matches(&exe_name, &exe_name, ""))
                    .map(|r| r.note.clone());

                let managed_profile = s.applied.get(&pid).map(|r| r.profile_id.0.clone());
                let restrained_by_probalance = s.probalance_restrained.contains_key(&pid);

                // Version-info description + company, cached by full exe
                // path. On a miss we spend one of this tick's reads on a
                // synchronous version-resource read; once the budget is
                // exhausted, new paths render with `None` fields and fill
                // in over subsequent ticks. We cache the whole VersionInfo
                // (not just the description) so Company comes for free —
                // both fields are decoded from the same resource buffer.
                let info = match s.version_info_cache.get(&exe_path) {
                    Some(v) => v.clone(),
                    None => {
                        if version_info_budget > 0 {
                            version_info_budget -= 1;
                            let v = framesage_sys::version_info::read_version_info(&exe_path)
                                .unwrap_or_default();
                            s.version_info_cache.insert(exe_path.clone(), v.clone());
                            v
                        } else {
                            framesage_sys::version_info::VersionInfo::default()
                        }
                    }
                };

                out.push(ProcessSnapshot {
                    pid,
                    exe_name,
                    exe_path,
                    description: info.description,
                    company: info.company,
                    priority_class_raw,
                    affinity_mask,
                    cpu_percent,
                    memory_bytes,
                    threads: ps.thread_count,
                    matched_rule_note: rule_note,
                    managed_profile,
                    restrained_by_probalance,
                });
            }

            s.list_processes_prev_samples = new_prev;

            // ─── System-wide metrics ─────────────────────────────────────
            //
            // System CPU% = 100 - (delta_idle / delta_total) over the same
            // wall-clock interval as the per-process sample. We compute it
            // from `GetSystemTimes` rather than summing per-process CPU%
            // because per-process omits whatever fraction of kernel time
            // we couldn't open (protected processes) and undercounts.
            let sys_cpu_now = framesage_sys::process::system_cpu_times().ok();
            let system_cpu_percent: u8 = match (&sys_cpu_now, &s.list_processes_prev_system_cpu) {
                (Some(now_t), Some(prev_t)) => {
                    let total_delta = now_t.total_100ns().saturating_sub(prev_t.total_100ns());
                    let idle_delta = now_t.idle_100ns.saturating_sub(prev_t.idle_100ns);
                    if total_delta == 0 {
                        0
                    } else {
                        let busy = total_delta.saturating_sub(idle_delta);
                        ((busy as u128 * 100 / total_delta as u128).min(100)) as u8
                    }
                }
                _ => 0,
            };
            s.list_processes_prev_system_cpu = sys_cpu_now;

            // Per-logical-CPU sample. Same idiom as the aggregate above: take
            // a fresh sample, diff against the previous one (if any) to
            // produce a per-core utilisation 0-100, then store the fresh
            // sample for next time. Length mismatches between samples (rare
            // — would require a hot-plug) fall through to an empty Vec so
            // the tray draws no per-core matrix until two compatible samples
            // accumulate.
            let per_cpu_now = framesage_sys::process::per_cpu_times().ok();
            let per_core_cpu_percent: Vec<u8> = match (&per_cpu_now, &s.list_processes_prev_per_cpu)
            {
                (Some(now_v), Some(prev_v)) if now_v.len() == prev_v.len() => now_v
                    .iter()
                    .zip(prev_v.iter())
                    .map(|(n, p)| {
                        let total = n.total_100ns().saturating_sub(p.total_100ns());
                        let idle = n.idle_100ns.saturating_sub(p.idle_100ns);
                        if total == 0 {
                            0u8
                        } else {
                            let busy = total.saturating_sub(idle);
                            ((busy as u128 * 100 / total as u128).min(100)) as u8
                        }
                    })
                    .collect(),
                _ => Vec::new(),
            };
            s.list_processes_prev_per_cpu = per_cpu_now;

            let (mem_total, mem_avail) = framesage_sys::process::memory_status().unwrap_or((0, 0));
            let mem_used = mem_total.saturating_sub(mem_avail);

            let metrics = SystemMetrics {
                cpu_percent: system_cpu_percent,
                per_core_cpu_percent,
                memory_used_bytes: mem_used,
                memory_total_bytes: mem_total,
            };

            (out, metrics)
        }
    }

    /// Enter manual mode: every foreground reconcile from now on applies
    /// `profile_id` regardless of Rules / default_profile, until
    /// `clear_manual_override` is called. Errors if the profile id isn't
    /// present in the active policy. Idempotent on a no-change SetManual
    /// for the same profile.
    pub fn set_manual_override(&self, profile_id: ProfileId) -> Result<()> {
        let mut s = self.state.write();
        if s.policy.profile(&profile_id).is_none() {
            return Err(anyhow::anyhow!("unknown profile id {profile_id}"));
        }
        let changed = s.manual_override.as_ref() != Some(&profile_id);
        s.manual_override = Some(profile_id.clone());
        if changed {
            info!(profile = %profile_id, "manual override set");
            self.force_recompute_active_profile_locked(&mut s);
        }
        Ok(())
    }

    /// Leave manual mode. Idempotent — no-op if manual mode was already off.
    /// On change, immediately re-evaluates the current foreground's profile
    /// (rule-match or default) and runs the full reconcile so the system
    /// reverts Game Mode + restores the previous-profile knobs without
    /// waiting for a focus change. This closes a bug where exiting manual
    /// mode while focused on the same window left the taskbar hidden.
    pub fn clear_manual_override(&self) {
        let mut s = self.state.write();
        if s.manual_override.take().is_some() {
            info!("manual override cleared");
            self.force_recompute_active_profile_locked(&mut s);
        }
    }

    /// Re-evaluate the current foreground's profile and re-apply it
    /// end-to-end (per-process + Game Mode), bypassing the
    /// "new_pid == current_foreground" early-return in `reconcile`.
    ///
    /// Called from anywhere that changes the *answer* to "what profile
    /// should the current foreground get" without changing the foreground
    /// PID itself — most prominently the manual-override set/clear paths.
    /// Without this, those paths only took effect at the next focus
    /// change, which stranded Game Mode state (taskbar hidden, services
    /// stopped, …) until the user happened to alt-tab.
    fn force_recompute_active_profile_locked(&self, s: &mut EngineState) {
        let Some(prev_pid) = s.current_foreground else {
            return;
        };
        let Some(snapshot) = s.foreground_snapshot.clone() else {
            return;
        };

        // Resolve the new profile via the same precedence the tick path
        // uses: manual override wins, else first-match rule, else default.
        let profile_id = match &s.manual_override {
            Some(ov) => ov.clone(),
            None => s
                .policy
                .match_foreground(&snapshot.exe_name, &snapshot.path, &snapshot.title)
                .clone(),
        };
        let profile = match s.policy.profile(&profile_id) {
            Some(p) => p.clone(),
            None => {
                warn!(profile = %profile_id, "force_recompute: profile id not in policy");
                return;
            }
        };

        // Same-profile-already-applied: don't churn the kernel state. Mirrors
        // reconcile + apply_once. Especially important here because
        // force_recompute fires on manual-override toggles — if the user is
        // already running game-x3d and toggles "set as manual mode" off
        // (which leaves them on game-x3d via the rule matcher), we'd
        // momentarily tear down the X3D pin for no reason.
        let already_correct = s
            .applied
            .get(&prev_pid)
            .map(|r| r.profile_id == profile_id)
            .unwrap_or(false);
        if !already_correct {
            // Revert old per-PID state so the new apply captures a clean prev.
            if let Some(record) = s.applied.remove(&prev_pid) {
                revert_record(prev_pid, record);
            }

            let topology = s.topology.clone();
            match apply_profile(prev_pid, &snapshot.exe_name, &profile, &topology) {
                Ok(record) => {
                    info!(pid = prev_pid, profile = %profile_id, "force_recompute applied");
                    s.applied.insert(prev_pid, record);
                }
                Err(e) => {
                    warn!(pid = prev_pid, error = %e, "force_recompute apply failed");
                }
            }
        } else {
            info!(
                pid = prev_pid,
                profile = %profile_id,
                "force_recompute: already correct, preserving state"
            );
        }
        s.active_profile = Some(profile_id.clone());
        let _ = self.events.send(Event::ForegroundChanged {
            foreground: snapshot,
            profile: profile_id.clone(),
        });

        // Reconcile system-wide Game Mode against the new profile.
        let new_actions = profile.game_mode.clone();
        Self::reconcile_system_mode_locked(
            s,
            &self.journal,
            self.safe_list,
            &profile_id,
            new_actions,
        );
    }

    /// Accept a foreground report from a user-session helper (the tray).
    /// See the `reported_foreground` field doc for the why.
    pub fn report_foreground(&self, pid: u32, exe_name: String, path: String, title: String) {
        let mut s = self.state.write();
        s.reported_foreground = Some(framesage_sys::foreground::ForegroundInfo {
            pid,
            exe_name,
            path,
            title,
        });
        s.foreground_reporter_seen = true;
    }

    /// Accept a "no foreground" report (lock screen, UAC, transition).
    pub fn report_no_foreground(&self) {
        let mut s = self.state.write();
        s.reported_foreground = None;
        s.foreground_reporter_seen = true;
    }

    /// Panic button: revert any active system mode regardless of foreground.
    /// Idempotent — `Ok(())` if no system mode was active.
    pub fn exit_system_mode_now(&self) {
        let mut s = self.state.write();
        Self::revert_system_mode_locked(&mut s, &self.journal);
    }

    /// Apply a named profile to the currently-foregrounded process,
    /// overriding the normal rule matcher. The override holds until the
    /// foreground changes — at the next focus change, `tick`'s reconcile
    /// path will pick the rule-matched profile for whatever's new.
    ///
    /// Used by the CLI's `framesage apply <profile>` and the tray's
    /// "Apply now" per-profile button. Errors if no foreground exists or
    /// if the profile id isn't in the active policy.
    pub fn apply_once(&self, profile_id: ProfileId) -> Result<()> {
        let foreground = framesage_sys::foreground::current()?
            .ok_or_else(|| anyhow::anyhow!("no foreground process to apply to"))?;
        let mut s = self.state.write();

        if s.paused {
            return Err(anyhow::anyhow!(
                "engine is paused; resume before apply_once"
            ));
        }

        let profile = s
            .policy
            .profile(&profile_id)
            .ok_or_else(|| anyhow::anyhow!("unknown profile id {profile_id}"))?
            .clone();

        // If we already applied the SAME profile to this PID and the new
        // profile is persistent, the state is in place — don't churn it by
        // revert+reapply. Mirrors the reconcile fast path. Without this,
        // hitting "Apply now" on a persistent game-x3d profile briefly tears
        // down the affinity before re-establishing it — visible UX glitch.
        let already_correct = s
            .applied
            .get(&foreground.pid)
            .map(|r| r.profile_id == profile_id)
            .unwrap_or(false);
        if !already_correct {
            // Revert anything we'd previously applied to this same PID
            // (whether the prior profile was the same or different) so we
            // have a clean slate to apply onto.
            if let Some(record) = s.applied.remove(&foreground.pid) {
                revert_record(foreground.pid, record);
            }
        }

        let topology = s.topology.clone();
        let snapshot = ForegroundSnapshot {
            pid: foreground.pid,
            exe_name: foreground.exe_name.clone(),
            path: foreground.path.clone(),
            title: foreground.title.clone(),
        };

        if !already_correct {
            let record = apply_profile(foreground.pid, &foreground.exe_name, &profile, &topology)?;
            info!(
                pid = foreground.pid,
                exe = %foreground.exe_name,
                profile = %profile_id,
                "apply_once",
            );
            s.applied.insert(foreground.pid, record);
        } else {
            info!(
                pid = foreground.pid,
                exe = %foreground.exe_name,
                profile = %profile_id,
                "apply_once: already correct, preserving state",
            );
        }
        s.current_foreground = Some(foreground.pid);
        s.foreground_snapshot = Some(snapshot.clone());
        s.active_profile = Some(profile_id.clone());
        let _ = self.events.send(Event::ForegroundChanged {
            foreground: snapshot,
            profile: profile_id.clone(),
        });

        // System mode reconcile — handles entering/exiting/swapping
        // Game Mode actions according to the new profile.
        let new_actions = profile.game_mode.clone();
        Self::reconcile_system_mode_locked(
            &mut s,
            &self.journal,
            self.safe_list,
            &profile_id,
            new_actions,
        );

        Ok(())
    }

    /// Run on startup: if a journal file exists from a previous (possibly
    /// crashed) session, revert it before we apply anything new. Idempotent.
    pub fn recover_orphan_journal(&self) {
        let entry = match self.journal.read() {
            Ok(Some(entry)) => entry,
            Ok(None) => return,
            Err(e) => {
                warn!(error = %e, "orphan journal read failed; deleting and moving on");
                let _ = self.journal.delete();
                return;
            }
        };
        warn!(
            session = %entry.session_id,
            profile = %entry.profile_id,
            "found orphan game-mode journal; reverting"
        );
        sys_revert_all(&entry.applied, &entry.previous);
        if let Err(e) = self.journal.delete() {
            warn!(error = %e, "failed to delete orphan journal after revert");
        }
    }

    /// One reconciliation pass.
    pub fn tick(&self) -> Result<()> {
        // Snapshot what we need without holding the lock across syscalls.
        let paused = self.state.read().paused;
        if paused {
            return Ok(());
        }

        // Session 0 isolation: a service running as LocalSystem can't see
        // the interactive desktop, so `GetForegroundWindow` returns null
        // from session 0. We prefer a foreground report from the
        // user-session helper (the tray, via Request::ReportForeground).
        // If no report has ever arrived (console-mode dev path), we fall
        // back to the in-process poll.
        let foreground = {
            let s = self.state.read();
            if s.foreground_reporter_seen {
                s.reported_foreground.clone()
            } else {
                drop(s);
                framesage_sys::foreground::current()?
            }
        };

        let mut s = self.state.write();
        self.reconcile(&mut s, foreground)?;
        Self::maybe_scan_background_locked(&mut s, self.safe_list);
        Self::maybe_reassert_persistent_locked(&mut s);
        self.maybe_run_probalance_locked(&mut s);
        Ok(())
    }

    /// One ProBalance pass: sample per-PID CPU, compute deltas vs. last
    /// sample, hand to `probalance::decide`, execute returned `Restrain` /
    /// `Restore` decisions as kernel calls, emit IPC events.
    ///
    /// Bounded by `PROBALANCE_SAMPLE_INTERVAL` (1 s) — the engine ticks much
    /// faster than that (300 ms by default), so most ticks fall through.
    /// Zero-cost when `cfg.enabled == false`.
    fn maybe_run_probalance_locked(&self, s: &mut EngineState) {
        // Fast-path bail when ProBalance is disabled.
        let cfg = s.policy.probalance.clone();
        if !cfg.enabled {
            // If the user just toggled it OFF while restraints were active,
            // we still need to release them. One-time drain.
            if !s.probalance_restrained.is_empty() {
                let drained: Vec<(u32, probalance::RestrainedRecord)> =
                    s.probalance_restrained.drain().collect();
                for (pid, rec) in drained {
                    #[cfg(windows)]
                    if let Err(e) = framesage_sys::apply::restore_priority_class_for_pid(
                        pid,
                        rec.original_raw_class,
                    ) {
                        warn!(pid, error = %e, "probalance: failed to release restraint on disable");
                    }
                    let _ = self.events.send(Event::ProBalanceRestored {
                        pid,
                        exe_name: rec.exe_name,
                        restored_class: rec.original_raw_class,
                    });
                }
                s.probalance_prev_samples.clear();
                s.probalance_last_sample_at = None;
            }
            return;
        }

        let now = Instant::now();
        let elapsed = match s.probalance_last_sample_at {
            Some(t) if now.duration_since(t) < PROBALANCE_SAMPLE_INTERVAL => return,
            Some(t) => now.duration_since(t),
            None => Duration::from_millis(0), // first sample — seed only
        };

        // Snapshot per-PID CPU times. Capture the foreground PID first so
        // we never even consider restraining it.
        let foreground_pid = s.current_foreground;
        let managed_pids: HashSet<u32> = s.applied.keys().copied().collect();

        let live_pids: Vec<u32> = match framesage_sys::process::iter_pids() {
            Ok(v) => v,
            Err(e) => {
                debug!(error = %e, "probalance: iter_pids failed; skipping sample");
                return;
            }
        };

        // Sample CPU times for every live PID we can open. PIDs that fail
        // (protected, exited) are silently skipped — they aren't candidates.
        let mut current_samples: HashMap<u32, (u64, String)> = HashMap::new();
        for pid in &live_pids {
            #[cfg(windows)]
            {
                let times = match framesage_sys::process::cpu_times(*pid) {
                    Ok(Some(t)) => t,
                    Ok(None) | Err(_) => continue,
                };
                let exe_name = match framesage_sys::process::exe_for_pid(*pid) {
                    Ok(Some(p)) => p
                        .rsplit(['\\', '/'])
                        .next()
                        .unwrap_or(&p)
                        .to_ascii_lowercase(),
                    Ok(None) | Err(_) => continue,
                };
                current_samples.insert(*pid, (times.total_100ns(), exe_name));
            }
            #[cfg(not(windows))]
            {
                let _ = pid;
            }
        }

        s.probalance_last_sample_at = Some(now);

        // First-sample seed: store, no decision can be made yet (no delta).
        if elapsed.is_zero() {
            s.probalance_prev_samples = current_samples
                .iter()
                .map(|(pid, (total, _))| {
                    (
                        *pid,
                        ProBalancePrevSample {
                            total_cpu_100ns: *total,
                        },
                    )
                })
                .collect();
            return;
        }

        // Compute CPU% per PID over `elapsed`. Format is "% of one logical
        // CPU" — 100 means one fully busy thread, 200 means two, etc.
        // Both kernel and user CPU time use 100-ns units, so we divide by
        // `elapsed_100ns / 100` (i.e. elapsed_micros * 10 → percent).
        let elapsed_100ns =
            (elapsed.as_secs() * 10_000_000) + (elapsed.subsec_nanos() as u64 / 100);
        if elapsed_100ns == 0 {
            return;
        }
        let mut decision_samples: Vec<probalance::ProcessSample> =
            Vec::with_capacity(current_samples.len());
        let mut system_busy_100ns: u64 = 0;
        for (pid, (total, exe)) in &current_samples {
            let prev_total = match s.probalance_prev_samples.get(pid) {
                Some(p) => p.total_cpu_100ns,
                None => continue, // first time we saw this PID — wait for next sample
            };
            let delta = total.saturating_sub(prev_total);
            system_busy_100ns = system_busy_100ns.saturating_add(delta);
            let cpu_percent_of_one = ((delta as u128).saturating_mul(100) / elapsed_100ns as u128)
                .min(u16::MAX as u128) as u16;
            // Query the current priority class for the demotion-target gate.
            #[cfg(windows)]
            let current_raw_class = match framesage_sys::apply::get_priority_class_for_pid(*pid) {
                Ok(Some(c)) => c,
                _ => continue,
            };
            #[cfg(not(windows))]
            let current_raw_class = 0x20u32;
            decision_samples.push(probalance::ProcessSample {
                pid: *pid,
                exe_name: exe.clone(),
                cpu_percent_of_one_cpu: cpu_percent_of_one,
                current_raw_class,
            });
        }

        // System CPU% = total CPU-time consumed across all sampled PIDs,
        // normalised to one fully-busy logical CPU, divided by CPU count.
        let cpu_count = s.topology.cpus.len().max(1) as u128;
        let system_cpu_percent: u8 = (((system_busy_100ns as u128).saturating_mul(100)
            / (elapsed_100ns as u128 * cpu_count))
            .min(100)) as u8;

        // Build the safe-list-name set. The game-mode safe-list's process
        // denylist already covers the system-critical names ProBalance must
        // never touch (dwm, audiodg, csrss, anti-cheat, AV, GPU drivers …).
        let safe_list_exes: HashSet<String> = self
            .safe_list
            .denied_process_names()
            .map(|n| n.to_ascii_lowercase())
            .collect();
        let user_ignore_exes: HashSet<String> = cfg
            .ignore_processes
            .iter()
            .map(|n| n.to_ascii_lowercase())
            .collect();

        let decisions = probalance::decide(
            &cfg,
            now,
            system_cpu_percent,
            foreground_pid,
            &decision_samples,
            &managed_pids,
            &safe_list_exes,
            &user_ignore_exes,
            &mut s.probalance_restrained,
        );

        for d in decisions {
            match d {
                probalance::Decision::Restrain {
                    pid,
                    exe_name,
                    original_raw_class,
                    demote_to,
                    demote_to_raw_class,
                } => {
                    #[cfg(windows)]
                    let result = framesage_sys::apply::set_priority_class_for_pid(pid, demote_to);
                    #[cfg(not(windows))]
                    let result: Result<()> = {
                        let _ = demote_to;
                        Ok(())
                    };
                    match result {
                        Ok(()) => {
                            info!(
                                pid,
                                exe = %exe_name,
                                from = format!("{:#x}", original_raw_class),
                                to = format!("{:#x}", demote_to_raw_class),
                                "probalance: restrained"
                            );
                            let _ = self.events.send(Event::ProBalanceRestrained {
                                pid,
                                exe_name,
                                from_class: original_raw_class,
                                to_class: demote_to_raw_class,
                            });
                        }
                        Err(e) => {
                            warn!(pid, error = %e, "probalance: restrain syscall failed; rolling back state");
                            // Don't leak a bookkeeping entry the kernel
                            // doesn't actually reflect.
                            s.probalance_restrained.remove(&pid);
                        }
                    }
                }
                probalance::Decision::Restore {
                    pid,
                    exe_name,
                    restored_raw_class,
                } => {
                    #[cfg(windows)]
                    if let Err(e) = framesage_sys::apply::restore_priority_class_for_pid(
                        pid,
                        restored_raw_class,
                    ) {
                        debug!(pid, error = %e, "probalance: restore syscall failed (process likely exited)");
                    }
                    info!(
                        pid,
                        exe = %exe_name,
                        restored = format!("{:#x}", restored_raw_class),
                        "probalance: restored"
                    );
                    let _ = self.events.send(Event::ProBalanceRestored {
                        pid,
                        exe_name,
                        restored_class: restored_raw_class,
                    });
                }
            }
        }

        // Roll forward the prev-sample buffer, dropping dead PIDs so the
        // map size stays proportional to live process count.
        s.probalance_prev_samples = current_samples
            .into_iter()
            .map(|(pid, (total, _exe))| {
                (
                    pid,
                    ProBalancePrevSample {
                        total_cpu_100ns: total,
                    },
                )
            })
            .collect();
    }

    /// Re-push kernel state (affinity, CPU sets, priority, I/O priority) onto
    /// every PID currently running under a `persistent` profile. Defeats
    /// runtime overrides — games that call `SetProcessAffinityMask` on
    /// themselves, plus CPU-Set advisory drift under scheduler contention.
    ///
    /// Bounded by `PERSISTENT_REASSERT_INTERVAL` (2 s). Each sweep just calls
    /// the per-knob setters; no prev-state capture, no revert plan rewrite —
    /// the original `AppliedRecord` continues to describe what to undo.
    fn maybe_reassert_persistent_locked(s: &mut EngineState) {
        let now = Instant::now();
        if let Some(last) = s.last_persistent_reassert {
            if now.duration_since(last) < PERSISTENT_REASSERT_INTERVAL {
                return;
            }
        }
        s.last_persistent_reassert = Some(now);

        // Snapshot first; we can't iterate `s.applied` while also re-borrowing
        // `s.policy` / `s.topology` immutably (the borrow checker isn't smart
        // enough to see that `applied` and the other fields are disjoint).
        // Tuple is (pid, expected_exe_name, profile).
        let pids_to_reassert: Vec<(u32, String, Profile)> = s
            .applied
            .iter()
            .filter_map(|(pid, record)| {
                let profile = s.policy.profile(&record.profile_id)?;
                if profile.persistent {
                    Some((*pid, record.exe_name.clone(), profile.clone()))
                } else {
                    None
                }
            })
            .collect();

        if pids_to_reassert.is_empty() {
            return;
        }

        let topology = s.topology.clone();
        let mut stale_pids: Vec<u32> = Vec::new();
        for (pid, expected_exe, profile) in pids_to_reassert {
            // PID reuse defense: Windows can reassign a PID seconds after the
            // original process exits. Without this check, our 2 s re-assert
            // sweep would happily push game-x3d onto whatever new process
            // happens to hold the PID now. Query the live exe and skip on
            // mismatch — the background-scan path will drop the record on
            // its next sweep (or we drop it here once we know it's stale).
            #[cfg(windows)]
            {
                let live_exe = match framesage_sys::process::exe_for_pid(pid) {
                    Ok(Some(path)) => path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned(),
                    Ok(None) | Err(_) => {
                        // Process gone (or unreadable — same outcome: we
                        // can't re-assert anyway). Mark for cleanup.
                        stale_pids.push(pid);
                        continue;
                    }
                };
                if !live_exe.eq_ignore_ascii_case(&expected_exe) {
                    debug!(
                        pid,
                        expected = %expected_exe,
                        live = %live_exe,
                        "re-assert: PID was reassigned to a different exe; dropping record"
                    );
                    stale_pids.push(pid);
                    continue;
                }
                if let Err(e) = framesage_sys::apply::reassert(pid, &profile, &topology) {
                    debug!(pid, error = %e, "persistent re-assert failed");
                }
            }
            #[cfg(not(windows))]
            {
                let _ = (pid, expected_exe, profile, &topology);
            }
        }

        for pid in stale_pids {
            s.applied.remove(&pid);
            if s.current_foreground == Some(pid) {
                s.current_foreground = None;
            }
        }
    }

    /// Walk every running PID and apply `Policy::background_profile` to any
    /// that we aren't already managing. Also drops `applied` entries for PIDs
    /// that no longer exist (process exited; no revert needed).
    ///
    /// Skipped: the system idle / system PIDs, our own PID, the current
    /// foreground PID, anything we already applied to, and anything whose
    /// exe appears on the safe-list's process denylist (system shell, audio,
    /// GPU drivers, anti-cheat). All other failures (OpenProcess denied on
    /// a protected process, exe path unreadable) silently skip — those are
    /// expected on every Windows box and aren't worth log spam.
    ///
    /// Bounded by `BACKGROUND_SCAN_INTERVAL` so we don't thrash on every
    /// `tick_ms`; the first call from a fresh service start does the heavy
    /// lift, subsequent calls only touch newly-spawned PIDs.
    fn maybe_scan_background_locked(s: &mut EngineState, safe_list: &'static SafeList) {
        // Bail early if the policy doesn't want background enforcement at all.
        let Some(bg_profile_id) = s.policy.background_profile.clone() else {
            return;
        };
        let Some(bg_profile) = s.policy.profile(&bg_profile_id).cloned() else {
            warn!(
                profile = %bg_profile_id,
                "background_profile points at an unknown profile id; skipping background scan"
            );
            return;
        };

        let now = Instant::now();
        if let Some(last) = s.last_background_scan {
            if now.duration_since(last) < BACKGROUND_SCAN_INTERVAL {
                return;
            }
        }
        s.last_background_scan = Some(now);

        let live_pids: Vec<u32> = match framesage_sys::process::iter_pids() {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "process enumeration failed; skipping background scan");
                return;
            }
        };
        let live_set: HashSet<u32> = live_pids.iter().copied().collect();
        let self_pid = std::process::id();
        let foreground_pid = s.current_foreground;
        let topology = s.topology.clone();

        // ─── Drop records for PIDs that exited since last scan ──────────────
        let stale: Vec<u32> = s
            .applied
            .keys()
            .copied()
            .filter(|p| !live_set.contains(p))
            .collect();
        for pid in stale {
            // Process is gone; no revert syscall would succeed. Just drop.
            s.applied.remove(&pid);
        }

        // ─── Apply background profile to new PIDs ───────────────────────────
        let mut newly_applied = 0usize;
        for pid in live_pids {
            if pid == 0 || pid == 4 {
                continue; // System Idle / System
            }
            if pid == self_pid {
                continue;
            }
            if Some(pid) == foreground_pid {
                continue;
            }
            if s.applied.contains_key(&pid) {
                continue;
            }

            // Filter against the safe-list denylist — same denylist that
            // protects suspend_processes, repurposed here so we don't, e.g.,
            // throttle dwm or audiodg into stuttering territory.
            let exe_name = match framesage_sys::process::exe_for_pid(pid) {
                Ok(Some(path)) => path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned(),
                Ok(None) => continue, // exited mid-snapshot, or unreadable
                Err(_) => continue,
            };
            if matches!(
                safe_list.check_process(&exe_name),
                framesage_gamemode::safe_list::ProcessVerdict::Denied(_)
            ) {
                continue;
            }

            match apply_profile(pid, &exe_name, &bg_profile, &topology) {
                Ok(record) => {
                    s.applied.insert(pid, record);
                    newly_applied += 1;
                }
                // ACCESS_DENIED / INVALID_PARAMETER on protected processes
                // is expected and not worth surfacing.
                Err(_) => continue,
            }
        }

        if newly_applied > 0 {
            debug!(
                profile = %bg_profile_id,
                applied = newly_applied,
                managed = s.applied.len(),
                "background scan applied profile to new PIDs"
            );
        }
    }

    fn reconcile(
        &self,
        s: &mut EngineState,
        foreground: Option<framesage_sys::foreground::ForegroundInfo>,
    ) -> Result<()> {
        let new_pid = foreground.as_ref().map(|f| f.pid);

        // Foreground unchanged — nothing to do.
        if new_pid == s.current_foreground {
            return Ok(());
        }

        // Revert per-process state on the previous foreground, UNLESS the
        // profile that was applied is marked `persistent`. Persistent pins
        // (game-x3d is the canonical example) outlive focus loss: a game
        // must stay on the X3D CCD while the user briefly alt-tabs to a
        // browser, a chat client, or Task Manager. Without this guard,
        // every focus flicker (loading screens, splash transitions, the
        // user popping Task Manager) ripped the X3D pin off and put the
        // game back on all cores.
        if let Some(prev_pid) = s.current_foreground.take() {
            if let Some(record) = s.applied.get(&prev_pid) {
                let keep = s
                    .policy
                    .profile(&record.profile_id)
                    .map(|p| p.persistent)
                    .unwrap_or(false);
                if !keep {
                    if let Some(record) = s.applied.remove(&prev_pid) {
                        revert_record(prev_pid, record);
                    }
                }
            }
        }

        // No new foreground? Tear down system mode too — there's nothing to
        // be in Game Mode "for."
        let Some(fg) = foreground else {
            s.foreground_snapshot = None;
            s.active_profile = None;
            Self::revert_system_mode_locked(s, &self.journal);
            return Ok(());
        };

        // Manual override wins over the rule matcher. Set by
        // `set_manual_override` / `Request::SetManualOverride` and cleared
        // by the complementary calls. When active, every foreground app
        // gets this profile regardless of what Rules would have matched.
        let profile_id = match &s.manual_override {
            Some(ov) => ov.clone(),
            None => s
                .policy
                .match_foreground(&fg.exe_name, &fg.path, &fg.title)
                .clone(),
        };

        let profile = match s.policy.profile(&profile_id) {
            Some(p) => p.clone(),
            None => {
                warn!(profile = %profile_id, "matched profile id not found in policy");
                return Ok(());
            }
        };

        let topology = s.topology.clone();
        let snapshot = ForegroundSnapshot {
            pid: fg.pid,
            exe_name: fg.exe_name.clone(),
            path: fg.path.clone(),
            title: fg.title.clone(),
        };

        // If the new foreground was already managed by us with the SAME
        // profile, skip the revert+reapply churn — the state is already
        // correct. (This is the common path for a persistent profile: the
        // user alt-tabs away and back; we kept the state in place, and
        // now we just need to update tracking.)
        //
        // If it was managed with a DIFFERENT profile, revert first so the
        // next apply captures clean prev-state.
        let already_correct = s
            .applied
            .get(&fg.pid)
            .map(|r| r.profile_id == profile_id)
            .unwrap_or(false);
        if !already_correct {
            if let Some(prev_record) = s.applied.remove(&fg.pid) {
                revert_record(fg.pid, prev_record);
            }
        }

        // Per-process apply (idempotent: if already_correct, just update tracking).
        if already_correct {
            info!(
                pid = fg.pid,
                exe = %fg.exe_name,
                profile = %profile_id,
                "re-foregrounded persistent pin — state preserved"
            );
            s.current_foreground = Some(fg.pid);
            s.foreground_snapshot = Some(snapshot.clone());
            s.active_profile = Some(profile_id.clone());
            let _ = self.events.send(Event::ForegroundChanged {
                foreground: snapshot.clone(),
                profile: profile_id.clone(),
            });
            // Fall through to system-mode reconcile below.
            let new_actions = profile.game_mode.clone();
            Self::reconcile_system_mode_locked(
                s,
                &self.journal,
                self.safe_list,
                &profile_id,
                new_actions,
            );
            return Ok(());
        }
        match apply_profile(fg.pid, &fg.exe_name, &profile, &topology) {
            Ok(record) => {
                info!(pid = fg.pid, exe = %fg.exe_name, profile = %profile_id, "applied");
                s.applied.insert(fg.pid, record);
                s.current_foreground = Some(fg.pid);
                s.foreground_snapshot = Some(snapshot.clone());
                s.active_profile = Some(profile_id.clone());
                let _ = self.events.send(Event::ForegroundChanged {
                    foreground: snapshot,
                    profile: profile_id.clone(),
                });
            }
            Err(e) => {
                warn!(pid = fg.pid, exe = %fg.exe_name, error = %e, "apply failed");
                // We still update foreground tracking so we don't loop trying
                // to re-apply on every tick. System mode below uses
                // active_profile.
                s.current_foreground = Some(fg.pid);
                s.foreground_snapshot = Some(snapshot);
                s.active_profile = Some(profile_id.clone());
            }
        }

        // System mode: reconcile against what the new profile wants.
        let new_actions = profile.game_mode.clone();
        Self::reconcile_system_mode_locked(
            s,
            &self.journal,
            self.safe_list,
            &profile_id,
            new_actions,
        );

        Ok(())
    }

    /// Compare current `system_mode` against the new profile's requested
    /// actions; if different, revert the old session and (if the new profile
    /// asked for it) enter a new one.
    fn reconcile_system_mode_locked(
        s: &mut EngineState,
        journal: &Journal,
        safe_list: &'static SafeList,
        new_profile: &ProfileId,
        new_actions: Option<GameModeActions>,
    ) {
        // Fast paths.
        match (&s.system_mode, &new_actions) {
            (None, None) => return,
            (None, Some(a)) if *a == GameModeActions::default() => return,
            (Some(current), Some(a))
                if &current.profile_id == new_profile && {
                    // The "actions unchanged" test: if the new profile is identical
                    // and the configured actions match what was applied. We don't
                    // store the original actions in ActiveSystemMode, so the
                    // simplest sound rule is "same profile id ⇒ keep." Profile
                    // edits during hot-reload will swap the policy and trigger a
                    // teardown/re-enter via this branch falling through.
                    let _ = a;
                    true
                } =>
            {
                return;
            }
            _ => {}
        }

        // Different (or first) — tear down any existing session, then enter.
        Self::revert_system_mode_locked(s, journal);

        let Some(actions) = new_actions else {
            return;
        };
        if actions == GameModeActions::default() {
            return;
        }

        // Plan.
        let plan_result = plan_game_mode(&actions, safe_list, &PlatformQuery);
        let plan = match plan_result {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => {
                debug!("game mode plan is empty; nothing to do");
                return;
            }
            Err(e) => {
                warn!(error = %e, "game mode planning failed");
                return;
            }
        };

        Self::enter_system_mode_locked(s, journal, new_profile, plan);
    }

    fn enter_system_mode_locked(
        s: &mut EngineState,
        journal: &Journal,
        profile_id: &ProfileId,
        plan: ActionPlan,
    ) {
        // Journal the full *intent* before any kernel mutation. This closes a
        // crash-recovery race that surfaced during first hardware validation:
        // sys_apply_action mutates kernel state synchronously (e.g. SCM marks
        // a service stopped), but the journal write that records "we stopped
        // it" happens after. A SIGKILL between the two left the kernel ahead
        // of the journal, and recovery missed the unjournaled mutation.
        //
        // With intent journaled up-front, recovery reverts everything we
        // *planned* to do. A failed sys_apply_action means recovery reverts a
        // change that was never made — an idempotent no-op (start an already-
        // running service, resume an already-running process).
        let intended = applied_from_plan(&plan);

        let mut entry = JournalEntry::new(profile_id.clone(), plan.previous_state.clone());
        entry.applied = intended.clone();

        if let Err(e) = journal.write(&entry) {
            warn!(error = %e, "initial journal write failed; refusing to enter game mode");
            return;
        }

        // Apply. Partial failures are logged locally; the journal does not
        // need to be rewritten because it already records the full intent.
        let mut any_failed = false;
        let mut applied_count: usize = 0;
        for action in &plan.actions {
            // sys_apply_action takes `&mut AppliedActions` for legacy reasons;
            // we route to a throwaway sink because the authoritative record is
            // `intended`, persisted before we got here.
            let mut sink = AppliedActions::default();
            match sys_apply_action(action, &mut sink) {
                Ok(()) => {
                    applied_count += 1;
                }
                Err(e) => {
                    warn!(?action, error = %e, "game mode action failed; continuing with rest of plan");
                    any_failed = true;
                }
            }
        }

        if !plan.rejections.is_empty() {
            for r in &plan.rejections {
                warn!(rejected = %r.id, reason = %r.reason, "game mode action rejected by safe-list");
            }
        }

        info!(
            profile = %profile_id,
            actions = applied_count,
            partial = any_failed,
            session = %entry.session_id,
            "game mode entered"
        );

        s.system_mode = Some(ActiveSystemMode {
            profile_id: profile_id.clone(),
            previous: plan.previous_state,
            applied: intended,
            journal_session_id: entry.session_id,
        });
    }

    fn revert_system_mode_locked(s: &mut EngineState, journal: &Journal) {
        let Some(active) = s.system_mode.take() else {
            return;
        };
        info!(
            profile = %active.profile_id,
            session = %active.journal_session_id,
            "game mode exiting"
        );
        sys_revert_all(&active.applied, &active.previous);
        if let Err(e) = journal.delete() {
            warn!(error = %e, "journal delete after revert failed");
        }
    }
}

fn revert_record(pid: u32, record: AppliedRecord) {
    #[cfg(windows)]
    if let Err(e) = framesage_sys::apply::revert(pid, record.state) {
        warn!(pid, error = %e, "revert failed");
    }
    debug!(pid, profile = %record.profile_id, "reverted");
    let _ = record;
}

/// Project a plan's actions onto an `AppliedActions` record describing what
/// every action would touch if it ran successfully. The engine journals this
/// up-front so that even a SIGKILL mid-apply leaves a complete revert plan on
/// disk.
///
/// Every `PlannedAction` variant must contribute to at least one field — if a
/// new variant is added later and this match grows a hole, the unit test
/// `applied_from_plan_covers_every_planned_action_variant` will catch it.
fn applied_from_plan(plan: &ActionPlan) -> AppliedActions {
    let mut a = AppliedActions::default();
    for action in &plan.actions {
        match action {
            PlannedAction::HideTaskbar => a.hid_taskbar = true,
            PlannedAction::SetPowerPlan { .. } => a.switched_power_plan = true,
            PlannedAction::StopService { id, .. } => a.stopped_services.push(id.clone()),
            PlannedAction::SuspendProcess { pid, exe } => {
                a.suspended_pids.push(SuspendedProcessSnapshot {
                    pid: *pid,
                    exe: exe.clone(),
                });
            }
            PlannedAction::SetFocusAssist(_) => a.set_focus_assist = true,
            PlannedAction::PauseWindowsUpdate => a.paused_windows_update = true,
        }
    }
    a
}

#[cfg(windows)]
fn apply_profile(
    pid: u32,
    exe_name: &str,
    profile: &Profile,
    topology: &CpuTopology,
) -> Result<AppliedRecord> {
    let state = framesage_sys::apply::apply(pid, profile, topology)?;
    Ok(AppliedRecord {
        profile_id: profile.id.clone(),
        exe_name: exe_name.to_owned(),
        state,
    })
}

#[cfg(not(windows))]
fn apply_profile(
    _pid: u32,
    exe_name: &str,
    profile: &Profile,
    _topology: &CpuTopology,
) -> Result<AppliedRecord> {
    Ok(AppliedRecord {
        profile_id: profile.id.clone(),
        exe_name: exe_name.to_owned(),
        _phantom: (),
    })
}

// ─── platform-specific shims ──────────────────────────────────────────────
//
// These ride the same cfg gates the rest of the workspace uses. On non-Windows
// they no-op so the engine still drives correctly during `framesage-sim` runs.

#[cfg(windows)]
fn sys_apply_action(
    action: &framesage_gamemode::planner::PlannedAction,
    applied: &mut AppliedActions,
) -> Result<()> {
    framesage_sys::game_mode::apply_action(action, applied)
}

#[cfg(not(windows))]
fn sys_apply_action(
    action: &framesage_gamemode::planner::PlannedAction,
    applied: &mut AppliedActions,
) -> Result<()> {
    framesage_sys::game_mode::apply_action(action, applied)
}

#[cfg(windows)]
fn sys_revert_all(applied: &AppliedActions, previous: &PreviousState) {
    framesage_sys::game_mode::revert_all(applied, previous);
}

#[cfg(not(windows))]
fn sys_revert_all(applied: &AppliedActions, previous: &PreviousState) {
    framesage_sys::game_mode::revert_all(applied, previous);
}

#[cfg(windows)]
struct PlatformQuery;

#[cfg(windows)]
impl SystemStateQuery for PlatformQuery {
    fn taskbar_visible(&self) -> anyhow::Result<bool> {
        framesage_sys::game_mode::Win32StateQuery.taskbar_visible()
    }
    fn active_power_plan(&self) -> anyhow::Result<Option<framesage_core::PowerPlanId>> {
        framesage_sys::game_mode::Win32StateQuery.active_power_plan()
    }
    fn service_status(&self, id: &str) -> anyhow::Result<framesage_gamemode::state::ServiceStatus> {
        framesage_sys::game_mode::Win32StateQuery.service_status(id)
    }
    fn pids_by_exe(&self, exe: &str) -> anyhow::Result<Vec<(u32, String)>> {
        framesage_sys::game_mode::Win32StateQuery.pids_by_exe(exe)
    }
}

#[cfg(not(windows))]
struct PlatformQuery;

#[cfg(not(windows))]
impl SystemStateQuery for PlatformQuery {
    fn taskbar_visible(&self) -> anyhow::Result<bool> {
        framesage_sys::game_mode::Win32StateQuery.taskbar_visible()
    }
    fn active_power_plan(&self) -> anyhow::Result<Option<framesage_core::PowerPlanId>> {
        framesage_sys::game_mode::Win32StateQuery.active_power_plan()
    }
    fn service_status(&self, id: &str) -> anyhow::Result<framesage_gamemode::state::ServiceStatus> {
        framesage_sys::game_mode::Win32StateQuery.service_status(id)
    }
    fn pids_by_exe(&self, exe: &str) -> anyhow::Result<Vec<(u32, String)>> {
        framesage_sys::game_mode::Win32StateQuery.pids_by_exe(exe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use framesage_core::{FocusAssistMode, PowerPlanId};
    use framesage_gamemode::state::ServiceStatus;

    fn empty_previous() -> PreviousState {
        PreviousState {
            taskbar_visible: true,
            active_power_plan: None,
            services: vec![],
            suspended_pids: vec![],
        }
    }

    /// Locks the load-bearing invariant for crash recovery: every concrete
    /// `PlannedAction` variant must contribute to `AppliedActions`. If a new
    /// variant is added but the helper isn't extended, recovery will silently
    /// miss it — exactly the bug this test exists to prevent regressing.
    #[test]
    fn applied_from_plan_covers_every_planned_action_variant() {
        let plan = ActionPlan {
            previous_state: empty_previous(),
            actions: vec![
                PlannedAction::HideTaskbar,
                PlannedAction::SetPowerPlan {
                    from: Some(PowerPlanId::Balanced),
                    to: PowerPlanId::HighPerformance,
                },
                PlannedAction::StopService {
                    id: "SysMain".into(),
                    was_status: ServiceStatus::Running,
                },
                PlannedAction::StopService {
                    id: "WSearch".into(),
                    was_status: ServiceStatus::Running,
                },
                PlannedAction::SuspendProcess {
                    pid: 1234,
                    exe: "OneDrive.exe".into(),
                },
                PlannedAction::SetFocusAssist(FocusAssistMode::PriorityOnly),
                PlannedAction::PauseWindowsUpdate,
            ],
            rejections: vec![],
        };

        let applied = applied_from_plan(&plan);

        assert!(applied.hid_taskbar);
        assert!(applied.switched_power_plan);
        assert_eq!(
            applied.stopped_services,
            vec!["SysMain".to_string(), "WSearch".to_string()]
        );
        assert_eq!(applied.suspended_pids.len(), 1);
        assert_eq!(applied.suspended_pids[0].pid, 1234);
        assert_eq!(applied.suspended_pids[0].exe, "OneDrive.exe");
        assert!(applied.set_focus_assist);
        assert!(applied.paused_windows_update);
        assert!(applied.anything_applied());
    }

    #[test]
    fn applied_from_plan_yields_empty_for_no_actions() {
        let plan = ActionPlan {
            previous_state: empty_previous(),
            actions: vec![],
            rejections: vec![],
        };
        assert_eq!(applied_from_plan(&plan), AppliedActions::default());
    }
}
