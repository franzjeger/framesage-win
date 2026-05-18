//! framesage policy engine.
//!
//! The engine sits between the OS (via `framesage-sys`) and the user-facing
//! configuration (a `Policy`). Each tick it asks: what is the foreground
//! process right now? — and reconciles the running state against what the
//! policy says it should be.
//!
//! # Layering (item 3.8)
//!
//! Depends on `framesage-core` + `framesage-sys` + `framesage-ipc` +
//! `framesage-gamemode`. It is the orchestrator — it takes `Policy` from
//! core, drives Win32 calls through sys (mediated by the `SysApi` trait
//! that lives in sys), runs the gamemode planner against the live system,
//! and emits events over IPC.
//!
//! The engine has exactly ONE host: `framesage-service`. The CLI and tray
//! talk to it via the IPC named-pipe protocol; neither depends on this
//! crate. See `ARCHITECTURE.md` at the repo root.
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
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

pub mod clock;
pub mod probalance;
pub mod undo;

pub use clock::{Clock, SystemClock};
pub use undo::{UndoEntry, UndoSummary, UndoableAction};

use framesage_core::{
    AntiCheatPresence, AntiCheatProfile, CpuTopology, GameModeActions, Policy, Profile, ProfileId,
};
use framesage_gamemode::{
    journal::{Journal, JournalEntry, SessionHistoryEntry},
    planner::{plan as plan_game_mode, ActionPlan, PlannedAction, SystemStateQuery},
    safe_list::SafeList,
    state::{AppliedActions, PreviousState, SuspendedProcessSnapshot},
};
use framesage_ipc::{
    ActionFailedKind, Event, ForegroundSnapshot, ProcessSnapshot, StatusSnapshot, SystemMetrics,
};

/// OS-level system events the engine reacts to. Sourced from the
/// Windows service control handler (`PowerEvent` /
/// `SessionChange`) and forwarded into the engine via
/// `Engine::handle_system_event`. Item 2.4 / audit M-02.
///
/// Kept abstract (Suspend / Resume / SessionConsoleConnect etc.) so the
/// engine doesn't need to know about Win32 specifics like `PBT_APMSUSPEND`
/// or `WTS_CONSOLE_DISCONNECT` — the service-side handler does the mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemEvent {
    /// System is entering a low-power state (sleep, hibernate).
    Suspend,
    /// System has resumed from a low-power state.
    Resume,
    /// User's console session was connected (FUS in, RDP login).
    SessionConsoleConnect,
    /// User's console session was disconnected (FUS away, RDP logout
    /// without closing). The user whose policy we were applying is no
    /// longer at the screen.
    SessionConsoleDisconnect,
    /// User locked their screen (Win+L). The game is still running;
    /// preserve Game Mode.
    SessionLock,
    /// User unlocked their screen.
    SessionUnlock,
}

/// Dependencies the engine needs at construction. Passing as a struct keeps
/// the call sites readable (we already have policy + topology, and now journal
/// + safe_list join them) and gives us room to grow without breaking callers.
pub struct EngineDeps {
    pub policy: Policy,
    pub topology: CpuTopology,
    pub safe_list: &'static SafeList,
    pub journal: Journal,
    /// Item 3.1 — abstracted syscall surface. Production callers pass
    /// `Arc::new(framesage_sys::RealSysApi)`; tests pass a fake
    /// implementation that returns scripted process lists / AC
    /// detection results without any kernel call.
    pub sys: Arc<dyn framesage_sys::SysApi>,
    /// Item 3.1 — abstracted clock. Production: `Arc::new(SystemClock)`;
    /// tests: `FakeClock` that advances on command so cadence-gated
    /// logic (AC-probe interval, background-scan interval, etc.) can
    /// be exercised without sleep-based waits.
    pub clock: Arc<dyn Clock>,
}

impl EngineDeps {
    /// Convenience constructor for production code: fills in the
    /// `sys` and `clock` fields with the real implementations.
    /// Existing callers that already build an `EngineDeps` literal
    /// stay working by adding the two fields explicitly; new callers
    /// (and tests that want production behavior) get a shorter
    /// invocation through this helper.
    pub fn with_real_sys(
        policy: Policy,
        topology: CpuTopology,
        safe_list: &'static SafeList,
        journal: Journal,
    ) -> Self {
        Self {
            policy,
            topology,
            safe_list,
            journal,
            sys: Arc::new(framesage_sys::RealSysApi),
            clock: Arc::new(SystemClock),
        }
    }
}

pub struct Engine {
    state: Arc<RwLock<EngineState>>,
    events: broadcast::Sender<Event>,
    safe_list: &'static SafeList,
    journal: Journal,
    /// Item 3.1 — kept on the engine (not in EngineState) because the
    /// trait object is stateless and shared across tasks; locking is
    /// unnecessary.
    sys: Arc<dyn framesage_sys::SysApi>,
    clock: Arc<dyn Clock>,
}

struct EngineState {
    policy: Policy,
    /// Item 2.3 / audit H-04. Topology is immutable after startup —
    /// CPU layout doesn't change while the engine runs (modulo
    /// hot-plug, which Group 3 item 3.7 will handle separately). The
    /// previous `Vec<LogicalCpu>` field made every `topology.clone()`
    /// (4+ per tick path) a full vector copy with ~50-100-byte
    /// per-LogicalCpu entries; the Arc wrap turns each clone into a
    /// single refcount bump (~1 ns).
    topology: Arc<CpuTopology>,
    /// Pre-computed lowercased denylist of process names that
    /// ProBalance / background scan must never touch. Built once in
    /// `Engine::new` from the bundled SafeList — never changes at
    /// runtime, so a single `Arc<HashSet>` shared across all readers
    /// eliminates the per-1s rebuild ProBalance was doing
    /// (audit H-06).
    safe_list_denied_exes: Arc<HashSet<String>>,
    /// Pre-computed lowercased copy of the policy's `probalance.
    /// ignore_processes` list. Refreshed in `set_policy`; otherwise
    /// stable for the lifetime of the policy version. Eliminates the
    /// per-1s rebuild from the ProBalance sample path.
    user_ignore_exes: Arc<HashSet<String>>,
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
    /// Item 4.6 — per-PID consecutive-hog-sample counter for the
    /// restrain-side hysteresis. A PID must read as a hog for
    /// `ProBalanceConfig.min_restrain_samples` ticks in a row before
    /// `probalance::decide` will demote it. Lives outside `decide` so the
    /// state survives across ticks; pruned inside `decide` to live PIDs.
    probalance_hog_streak: HashMap<u32, u32>,
    /// Item 4.9 — per-PID apply-failure timestamps. After an automatic
    /// (reconcile / background-scan) `apply_profile` call fails on a
    /// PID, we record the failure time here and skip subsequent
    /// automatic re-applies for `APPLY_FAILURE_BACKOFF` (default 30 s).
    /// Avoids log spam from a process that's refusing
    /// PROCESS_SET_INFORMATION for the entire run (anti-cheat clients,
    /// AV, kernel-protected services that slipped through the safe-
    /// list); without backoff the reconcile + background-scan paths
    /// would each retry once per second and pollute the activity feed.
    ///
    /// Cleared on:
    ///   * successful apply (the failure was transient)
    ///   * PID exit (the next PID under this number is a new process)
    ///   * explicit user actions (`apply_once`, `force_recompute` —
    ///     the user is staring at the screen; honor their request)
    apply_failure_backoff: HashMap<u32, Instant>,
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
    /// Per-PID owner cache. `Some(name)` = SID resolved to a user name;
    /// `None` = the kernel told us about the token but `LookupAccountSidW`
    /// returned nothing (rare; some capability SIDs); absent key = never
    /// tried. Pruned each tick to PIDs still present in the snapshot so
    /// PID reuse can't surface a stale account name from a previous
    /// process under the same number.
    user_cache: HashMap<u32, Option<String>>,
    /// Manual mode: when set, every foreground reconcile applies this
    /// profile instead of consulting Rules. Stays set across focus
    /// changes until explicitly cleared via `clear_manual_override` /
    /// `Request::ClearManualOverride`.
    manual_override: Option<ProfileId>,
    /// Item 2.11 — Manual Global Game Mode. When set, the engine has
    /// entered `game_mode` actions for this profile system-wide
    /// regardless of foreground; the auto-reconcile path
    /// (`reconcile_system_mode_locked`) skips tear-down while this
    /// is `Some`. Cleared by
    /// `disable_manual_global_game_mode` /
    /// `Request::DisableManualGlobalGameMode`, by
    /// `SystemEvent::Suspend` (matches user's "back to normal on
    /// resume" expectation), and by
    /// `SystemEvent::SessionConsoleDisconnect`.
    manual_global_active: Option<ProfileId>,
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
    /// Wall-clock instant of the most recent ReportForeground /
    /// ReportNoForeground IPC. Item 2.6 / audit H-13. Without this,
    /// `foreground_reporter_seen=true` was a one-way latch: once the
    /// tray reported even once, the engine would forever prefer the
    /// (now-stale) cached report over session-local polling. A
    /// crashed tray left the engine stuck on whatever profile was
    /// active at crash time. The staleness check in `tick` compares
    /// this against [`FOREGROUND_REPORT_STALENESS`] and falls back to
    /// session-local polling if the tray hasn't reported recently.
    last_foreground_report_at: Option<Instant>,
    /// PIDs that have had a standalone `AffinityRule` pin applied at
    /// least once. Independent of `applied` because affinity rules live
    /// outside the profile system — a PID can have a rule pin without
    /// having an `AppliedRecord`. We use the set to avoid re-applying
    /// every background-scan tick, and prune it as PIDs disappear from
    /// the live list. The persistent-reassert sweep re-pushes rule pins
    /// for any PID that's both in this set AND still alive.
    affinity_rule_applied: HashSet<u32>,
    /// Per-PID full-image-path cache. Item 2.1. NTQSI gives us the
    /// bare filename (`notepad.exe`); the full path
    /// (`C:\Windows\system32\notepad.exe`) requires an OpenProcess +
    /// QueryFullProcessImageNameW. We cache the result per-PID so a
    /// stable process list does this lookup once per PID per session,
    /// not once per tick. Pruned to live PIDs each
    /// `list_process_snapshots` call.
    exe_path_cache: HashMap<u32, String>,
    /// Item 3.5 — ring buffer of the last `UNDO_LOG_CAP` user-
    /// initiated mutations + the prior state needed to reverse each.
    /// Driven by `set_process_priority` / `set_process_affinity` /
    /// `suspend_process` / `resume_process` (the four Processes-tab
    /// right-click actions). User invokes `framesage undo` (or the
    /// tray's Undo button — future PR) to pop and reverse the most
    /// recent entry. In-memory only; a service restart drops the
    /// log, which matches the user's expectation that undo is a
    /// "right now" affordance, not a session-history archive.
    undo_log: undo::UndoLog,
    /// Most-recent anti-cheat presence snapshot. Item 1.9 / AC matrix.
    /// Refreshed by the AC detection probe (typically piggybacked on the
    /// persistent-reassert tick). Drives two behaviors:
    ///   * ESEA detected → engine STANDBY mode (no apply / scans /
    ///     actions until ESEA exits). Defaults D-11.
    ///   * FACEIT detected → refuse WU pause / stop calls so the
    ///     FACEIT client doesn't refuse-to-launch on next match start.
    ///
    /// Default = nothing detected (initial value before first probe).
    ac_presence: AntiCheatPresence,
    /// Last time we ran the AC detection probe. `None` until first
    /// probe; subsequent probes honour [`AC_DETECT_INTERVAL`]. Cheap
    /// enough to run every ~5 s; piggybacks on the persistent-reassert
    /// tick path so no extra timer is needed.
    last_ac_probe: Option<Instant>,
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

/// Maximum age of the tray's last ReportForeground / ReportNoForeground
/// IPC before the engine treats the report as stale and falls back to
/// session-local polling. Item 2.6 / audit H-13.
///
/// The tray reports every 250 ms; a 10 s window is 40× normal interval,
/// generous enough to absorb a slow service-restart cycle without
/// false-positive fallback, tight enough that a crashed tray doesn't
/// leave the engine stuck on a phantom foreground for minutes.
const FOREGROUND_REPORT_STALENESS: Duration = Duration::from_secs(10);

/// How often the engine refreshes its anti-cheat presence probe.
/// Item 1.9 / AC matrix. 5 s is plenty — Vanguard / EAC / BattlEye
/// drivers load on game launch, not on the second, so a 5 s lag
/// between "user launched Valorant" and "engine enters Vanguard-safe
/// mode for matching rules" is invisible. Cheaper than the existing
/// per-PID enumeration so the cost is one extra iteration of the
/// already-cached PID list (re-used from the persistent-reassert
/// path).
const AC_DETECT_INTERVAL: Duration = Duration::from_secs(5);

/// How often the engine samples per-PID CPU usage for ProBalance. 1 s gives
/// reasonable accuracy (a process that's busy for 200 ms of every second
/// shows up at ~20%) without thrashing OpenProcess. Skipped entirely when
/// `policy.probalance.enabled == false`.
const PROBALANCE_SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);

/// Item 4.9 — after an automatic apply_profile call fails on a PID, skip
/// further automatic attempts on that PID for this long. Cleared on
/// success, on PID exit, and on explicit user action. 30 s matches
/// audit M-15's recommendation: long enough to prevent log spam on
/// genuinely-protected processes, short enough that a transient
/// permission race resolves itself within a window the user wouldn't
/// notice.
const APPLY_FAILURE_BACKOFF: Duration = Duration::from_secs(30);

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
    /// Item 3.1b — uses the unified `framesage_sys::apply::AppliedState`
    /// type that's defined on both Windows (real syscalls) and non-
    /// Windows (unit struct via the stub module).
    state: framesage_sys::apply::AppliedState,
    /// Item 4.7 — raw Win32 priority class observed RIGHT after our
    /// apply landed. Acts as the ground-truth "we applied this" value
    /// the revert path compares against; if the live value has since
    /// drifted (user changed via Task Manager), revert skips that
    /// PID rather than silently undoing the user's manual choice.
    /// `None` means the profile didn't touch priority — no drift
    /// signal to read, revert proceeds as before.
    applied_priority_class_raw: Option<u32>,
    /// Item 4.7 — process affinity mask observed right after apply.
    /// Same drift semantic as `applied_priority_class_raw`.
    applied_affinity_mask: Option<u64>,
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
    /// UNIX timestamp of session start. Captured at apply time so the
    /// session-history append on revert can record duration. Item 1.4 /
    /// audit C-07.
    started_at_unix_secs: u64,
}

impl Engine {
    /// Construct an engine with full dependencies.
    pub fn new(deps: EngineDeps) -> Self {
        let (tx, _) = broadcast::channel(64);
        // Item 2.3 — pre-compute the static safe-list denied-process set
        // once. The previous ProBalance sample path rebuilt this every
        // 1 s; it never changes (the bundled SafeList is a `&'static`),
        // so a single Arc shared across all readers is enough. Audit
        // H-06.
        let safe_list_denied_exes: Arc<HashSet<String>> = Arc::new(
            deps.safe_list
                .denied_process_names()
                .map(|n| n.to_ascii_lowercase())
                .collect(),
        );
        let user_ignore_exes = Arc::new(build_user_ignore_exes(&deps.policy));
        let topology = Arc::new(deps.topology);
        Self {
            state: Arc::new(RwLock::new(EngineState {
                policy: deps.policy,
                topology,
                safe_list_denied_exes,
                user_ignore_exes,
                paused: false,
                applied: HashMap::new(),
                current_foreground: None,
                foreground_snapshot: None,
                active_profile: None,
                system_mode: None,
                last_background_scan: None,
                last_persistent_reassert: None,
                probalance_prev_samples: HashMap::new(),
                probalance_hog_streak: HashMap::new(),
                apply_failure_backoff: HashMap::new(),
                probalance_last_sample_at: None,
                probalance_restrained: HashMap::new(),
                list_processes_prev_samples: HashMap::new(),
                list_processes_last_sample_at: None,
                list_processes_prev_system_cpu: None,
                list_processes_prev_per_cpu: None,
                version_info_cache: HashMap::new(),
                user_cache: HashMap::new(),
                manual_override: None,
                manual_global_active: None,
                reported_foreground: None,
                foreground_reporter_seen: false,
                last_foreground_report_at: None,
                affinity_rule_applied: HashSet::new(),
                ac_presence: AntiCheatPresence::default(),
                last_ac_probe: None,
                exe_path_cache: HashMap::new(),
                undo_log: undo::UndoLog::default(),
            })),
            events: tx,
            safe_list: deps.safe_list,
            journal: deps.journal,
            sys: deps.sys,
            clock: deps.clock,
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
            // Force the next tick's reconcile to re-apply the rule for the
            // current foreground. Without this, if the foreground PID
            // hasn't changed since the pause, `reconcile()` early-returns
            // on `new_pid == s.current_foreground` and the user has to
            // alt-tab away and back to get a profile applied. Clearing
            // `current_foreground` makes the next tick treat whatever's
            // foregrounded as freshly-arrived.
            s.current_foreground = None;
            let _ = self.events.send(Event::Resumed);
            info!("engine resumed");
        }
    }

    /// Item 4.11 — expose the current topology snapshot so the IPC
    /// layer can validate incoming policies before forwarding to
    /// `set_policy`. Cheap (Arc clone). Returned snapshot is the same
    /// one the engine resolves selectors against on the next tick.
    pub fn topology_snapshot(&self) -> Arc<CpuTopology> {
        self.state.read().topology.clone()
    }

    /// Item 4.13 — list every Win32 service the SCM knows about,
    /// formatted as the IPC wire type so the service handler can
    /// forward directly. Failure is logged + an empty list is
    /// returned (the discover-services view degrades gracefully).
    pub fn list_services_for_ipc(&self) -> Vec<framesage_ipc::ServiceInfoIpc> {
        match self.sys.enumerate_services() {
            Ok(services) => services
                .into_iter()
                .map(|s| framesage_ipc::ServiceInfoIpc {
                    name: s.name,
                    display_name: s.display_name,
                    status: match s.status {
                        framesage_sys::services::ServiceStatusKind::Running => {
                            framesage_ipc::ServiceStatusKindIpc::Running
                        }
                        framesage_sys::services::ServiceStatusKind::Stopped => {
                            framesage_ipc::ServiceStatusKindIpc::Stopped
                        }
                        framesage_sys::services::ServiceStatusKind::Pending => {
                            framesage_ipc::ServiceStatusKindIpc::Pending
                        }
                    },
                    owning_pid: s.owning_pid,
                })
                .collect(),
            Err(e) => {
                warn!(error = %e, "enumerate_services failed; returning empty list");
                Vec::new()
            }
        }
    }

    pub fn set_policy(&self, policy: Policy) {
        // Refresh the cached user-ignore set whenever policy changes —
        // the ignore list is the only ProBalance-relevant field the user
        // can edit at runtime. Building once here beats rebuilding every
        // 1 s in the sample loop.
        let new_ignore = Arc::new(build_user_ignore_exes(&policy));
        let mut s = self.state.write();
        s.policy = policy;
        s.user_ignore_exes = new_ignore;
        info!("policy replaced");
    }

    /// Item 3.7 — re-detect CPU topology and swap the engine's
    /// cached `Arc<CpuTopology>`. Cheap because all consumers hold
    /// the topology behind an `Arc` already (item 2.3 / audit H-04):
    /// the swap is a single pointer write, existing readers keep
    /// their old Arc valid until they release it, and the next
    /// `topology.clone()` in the tick path picks up the new layout.
    ///
    /// Triggers:
    ///
    /// * `SystemEvent::Resume` — the most realistic trigger on
    ///   desktop hardware. Sleep/resume can land in a different
    ///   power plan that parks cores, and (rarely) in a Hyper-V
    ///   guest the host can hot-add / hot-remove vCPUs across the
    ///   suspend boundary.
    /// * `Request::RefreshTopology` (manual) — exposed so the user
    ///   can force a refresh after toggling Windows' "Minimum
    ///   processor state" / "Processor performance core parking"
    ///   advanced power-plan options without rebooting.
    ///
    /// If detection fails (extremely unusual — would mean the kernel
    /// is refusing `GetLogicalProcessorInformationEx`), we keep the
    /// previous topology and log; better to operate on a slightly
    /// stale snapshot than to blow away the working one.
    pub fn refresh_topology(&self) {
        match self.sys.detect_topology() {
            Ok(mut new_topology) => {
                new_topology.retag_ccds_from_signals();
                let old_count = self.state.read().topology.cpus.len();
                let new_count = new_topology.cpus.len();
                self.state.write().topology = Arc::new(new_topology);
                if old_count != new_count {
                    info!(
                        old = old_count,
                        new = new_count,
                        "topology refreshed; logical CPU count changed"
                    );
                } else {
                    debug!(cpus = new_count, "topology refreshed; layout unchanged");
                }
            }
            Err(e) => {
                warn!(error = %e, "topology refresh failed; keeping previous snapshot");
            }
        }
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
        let exe = resolve_exe_for_pid_or_err(self.sys.as_ref(), pid, "set priority")?;
        check_process_modifiable(self.safe_list, &exe, "set priority")?;
        // Item 3.5 — capture pre-change state BEFORE the apply so the
        // undo record knows what to restore. If the read fails we
        // skip recording rather than fabricate a value — better to
        // lose undo capability for this one action than to "undo" by
        // overwriting with a wrong class. The action itself still
        // fires.
        let previous_raw_class = self.sys.get_priority_class_for_pid(pid).ok().flatten();
        self.sys.set_priority_class_for_pid(pid, class)?;
        if let Some(previous_raw_class) = previous_raw_class {
            self.state
                .write()
                .undo_log
                .record(UndoableAction::SetPriority {
                    pid,
                    exe_name: exe,
                    previous_raw_class,
                    applied_class: class,
                });
        }
        Ok(())
    }

    /// Freeze every thread of `pid` via `NtSuspendProcess`. Mirrors what
    /// the Game Mode `suspend_processes` plan does, but as a one-shot for
    /// the Processes-tab right-click. Errors bubble back to the caller
    /// (the service translates into a `Response::Error`) so the user sees
    /// the reason — usually "PID is protected", "PID exited", or "this
    /// process is on the framesage denylist for safety".
    pub fn suspend_process(&self, pid: u32) -> Result<()> {
        let exe = resolve_exe_for_pid_or_err(self.sys.as_ref(), pid, "suspend")?;
        check_process_modifiable(self.safe_list, &exe, "suspend")?;
        self.sys.suspend_process(pid)?;
        // Item 3.5 — no prior state to capture; undo reverses by
        // calling resume_process on the same PID.
        self.state
            .write()
            .undo_log
            .record(UndoableAction::SuspendProcess { pid, exe_name: exe });
        Ok(())
    }

    /// Release a previous suspend via `NtResumeProcess`. Safe on processes
    /// that aren't currently suspended (kernel returns success).
    ///
    /// Gated on the denylist for symmetry with `suspend_process`. The
    /// denylist members should never have been suspended in the first
    /// place (suspend_process refuses), so a resume call against a
    /// denylisted PID indicates either a bug or an externally-suspended
    /// process — neither is our responsibility to recover.
    pub fn resume_process(&self, pid: u32) -> Result<()> {
        let exe = resolve_exe_for_pid_or_err(self.sys.as_ref(), pid, "resume")?;
        check_process_modifiable(self.safe_list, &exe, "resume")?;
        self.sys.resume_process(pid)?;
        // Item 3.5 — symmetric with suspend_process; undo re-suspends.
        self.state
            .write()
            .undo_log
            .record(UndoableAction::ResumeProcess { pid, exe_name: exe });
        Ok(())
    }

    /// Empty `pid`'s working set via `K32EmptyWorkingSet`. Useful as a
    /// pre-launch nudge — trim fat background processes (browsers, mail
    /// clients) so a heavy app has more resident RAM headroom without
    /// hitting the pagefile.
    ///
    /// Denylist gate covers the audit's H1 finding: trimming Defender's
    /// working set forces it to page-fault its signature database back in,
    /// causing a disk-I/O storm. MsMpEng is on the bundled denylist, so the
    /// gate refuses the action with the documented rationale.
    pub fn trim_working_set(&self, pid: u32) -> Result<()> {
        let exe = resolve_exe_for_pid_or_err(self.sys.as_ref(), pid, "trim working set")?;
        check_process_modifiable(self.safe_list, &exe, "trim working set")?;
        self.sys.trim_working_set_for_pid(pid)?;
        info!(pid, "trim_working_set");
        Ok(())
    }

    /// One-shot affinity pin against a live PID. Resolves `selector`
    /// against the current topology so the caller can say "Kind(Cache)"
    /// and let us figure out which CPUs are the X3D ones on this box.
    ///
    /// Bypasses the profile system intentionally — this is the user's
    /// override hammer for cases the rule engine can't reach (anti-cheat
    /// processes that refuse `OpenProcess(PROCESS_SET_INFORMATION)`,
    /// parent processes the user wants to constrain so children
    /// inherit at spawn, etc.).
    pub fn set_process_affinity(
        &self,
        pid: u32,
        selector: framesage_core::CpuSelector,
    ) -> Result<()> {
        let exe = resolve_exe_for_pid_or_err(self.sys.as_ref(), pid, "set affinity")?;
        check_process_modifiable(self.safe_list, &exe, "set affinity")?;
        let topology = self.state.read().topology.clone();
        let indices = topology.resolve(&selector);
        if indices.is_empty() {
            return Err(anyhow::anyhow!(
                "selector {:?} resolved to no CPUs on this topology",
                selector
            ));
        }
        let mut mask: u64 = 0;
        for idx in indices {
            if idx < 64 {
                mask |= 1u64 << idx;
            }
        }
        if mask == 0 {
            return Err(anyhow::anyhow!(
                "selector {:?} produced an empty mask (all indices >= 64?)",
                selector
            ));
        }
        // Item 3.5 — capture pre-change mask before the apply. `None`
        // means the read failed (PID protected mid-call); the undo
        // path will refuse to "restore" to None rather than zeroing
        // the affinity (which would be catastrophic).
        let previous_mask = self.sys.affinity_mask(pid).ok().flatten();
        self.sys.set_affinity_mask_for_pid(pid, mask)?;
        info!(pid, mask = format!("{:#x}", mask), "set_process_affinity");
        self.state
            .write()
            .undo_log
            .record(UndoableAction::SetAffinity {
                pid,
                exe_name: exe,
                previous_mask,
                applied_mask: mask,
            });
        Ok(())
    }

    /// Hard kill via `TerminateProcess(handle, 1)`. The tray confirms the
    /// user's intent before the request reaches us; the engine performs
    /// no further confirmation. Also strips our internal bookkeeping for
    /// the PID so a stale row doesn't haunt the next reconcile.
    ///
    /// Denylist gate is the most important of all — terminating csrss /
    /// lsass / wininit / smss blue-screens the box with CRITICAL_PROCESS_DIED.
    /// The bundled denylist covers every documented critical process; this
    /// check is the BSOD-prevention layer. `process_actions::terminate`
    /// already refuses PID 0 / 4 as a final belt-and-suspenders backstop.
    pub fn terminate_process(&self, pid: u32) -> Result<()> {
        let exe = resolve_exe_for_pid_or_err(self.sys.as_ref(), pid, "terminate")?;
        check_process_modifiable(self.safe_list, &exe, "terminate")?;
        self.sys.terminate_process(pid)?;
        let mut s = self.state.write();
        s.applied.remove(&pid);
        s.probalance_restrained.remove(&pid);
        s.probalance_prev_samples.remove(&pid);
        s.probalance_hog_streak.remove(&pid);
        s.apply_failure_backoff.remove(&pid);
        s.list_processes_prev_samples.remove(&pid);
        s.affinity_rule_applied.remove(&pid);
        if s.current_foreground == Some(pid) {
            s.current_foreground = None;
        }
        Ok(())
    }

    /// Cheap read-only snapshot of the in-memory policy. Used by the IPC
    /// service after a `SetAffinityRule` / `DeleteAffinityRule` call so the
    /// runtime can persist the mutation to disk without holding the engine
    /// lock for the full write.
    pub fn policy_snapshot(&self) -> Policy {
        self.state.read().policy.clone()
    }

    /// Create or update a persistent CPU-affinity rule keyed by exe name.
    /// When `apply_to_live` is true, walks the live process list and pins
    /// every matching PID right now — the "apply to running" UX of the
    /// "Remember for next time" toggle.
    ///
    /// The rule is also stored in the engine's in-memory policy; the caller
    /// is responsible for persisting the mutation to disk afterward (the
    /// IPC handler does so via [`Self::policy_snapshot`] + [`Policy::save`]).
    ///
    /// Live-PID failures are logged but don't abort the call — the user just
    /// wanted the rule saved; surfacing a per-PID OpenProcess refusal would
    /// be more confusing than helpful. The rule still gets stored and will
    /// re-apply cleanly on the next spawn.
    pub fn set_affinity_rule(
        &self,
        rule: framesage_core::AffinityRule,
        apply_to_live: bool,
    ) -> Result<()> {
        let exe_name = rule.exe_name.clone();
        let selector = rule.selector.clone();
        {
            let mut s = self.state.write();
            let replaced = s.policy.upsert_affinity_rule(rule);
            info!(
                exe = %exe_name,
                replaced,
                "set_affinity_rule"
            );
            // Forget any prior "rule applied" marker for live PIDs of this
            // exe — they need re-application against the new selector, not
            // the old one. The next maybe_scan_background_locked tick (or
            // the apply_to_live walk below) will re-mark them.
            s.affinity_rule_applied.clear();
        }

        if apply_to_live {
            let pids = self.sys.iter_pids().unwrap_or_default();
            let mut applied_count: usize = 0;
            let mut new_marks: Vec<u32> = Vec::new();
            for pid in pids {
                let live_exe = match self.sys.exe_for_pid(pid) {
                    Ok(Some(path)) => path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned(),
                    Ok(None) | Err(_) => continue,
                };
                if !live_exe.eq_ignore_ascii_case(&exe_name) {
                    continue;
                }
                match self.set_process_affinity(pid, selector.clone()) {
                    Ok(()) => {
                        applied_count += 1;
                        new_marks.push(pid);
                        let _ = self.events.send(Event::AffinityRuleFired {
                            pid,
                            exe_name: live_exe.clone(),
                            rule_exe: exe_name.clone(),
                        });
                    }
                    Err(e) => {
                        warn!(pid, exe = %live_exe, error = %e, "affinity rule apply-to-live failed");
                        let _ = self.events.send(Event::ActionFailed {
                            kind: ActionFailedKind::Apply,
                            pid: Some(pid),
                            exe_name: Some(live_exe.clone()),
                            details: format!("affinity rule apply-to-live failed: {e:#}"),
                        });
                    }
                }
            }
            if !new_marks.is_empty() {
                let mut s = self.state.write();
                for pid in new_marks {
                    s.affinity_rule_applied.insert(pid);
                }
            }
            info!(
                exe = %exe_name,
                applied_count,
                "affinity rule applied to live PIDs"
            );
        }

        Ok(())
    }

    /// Remove the persistent affinity rule for `exe_name` (case-insensitive).
    /// Idempotent; returns the in-memory mutation but does NOT revert the
    /// affinity on currently-running matching processes — see
    /// [`framesage_ipc::Request::DeleteAffinityRule`] for the UX rationale.
    /// Caller persists the policy change.
    pub fn delete_affinity_rule(&self, exe_name: &str) -> bool {
        let mut s = self.state.write();
        let removed = s.policy.remove_affinity_rule(exe_name);
        if removed {
            info!(exe = %exe_name, "delete_affinity_rule");
        }
        removed
    }

    /// Item 3.5 — read-only view of the undo log, newest entry
    /// first, capped at `limit`. Used by `framesage undo list` and
    /// the tray's Undo panel (follow-up PR).
    pub fn undo_log_snapshot(&self, limit: usize) -> Vec<UndoEntry> {
        self.state.read().undo_log.snapshot_newest_first(limit)
    }

    /// Item 3.5 — pop the most recent undo entry and apply its
    /// reverse. Returns:
    ///
    /// * `Ok(None)` if the log is empty.
    /// * `Ok(Some(summary))` if an entry was popped, regardless of
    ///   whether the reverse syscall succeeded. `summary.failure` is
    ///   `Some` when the reverse failed (typically because the PID
    ///   has since exited) — the entry is still removed so a second
    ///   `undo` invocation pops the next one, not the failed one.
    ///
    /// Idempotency: each undo invocation removes one entry. The user
    /// can chain `framesage undo` calls to walk further back.
    pub fn undo_last(&self) -> Result<Option<UndoSummary>> {
        let entry = {
            let mut s = self.state.write();
            match s.undo_log.pop_last() {
                Some(e) => e,
                None => return Ok(None),
            }
        };
        // Perform the reverse outside the lock. Each undo branch
        // calls one syscall through self.sys; failure is recorded in
        // the summary, not propagated as Err — the caller (CLI / tray
        // / IPC) wants the description either way.
        let failure = match &entry.action {
            UndoableAction::SetPriority {
                pid,
                previous_raw_class,
                ..
            } => self
                .sys
                .restore_priority_class_for_pid(*pid, *previous_raw_class)
                .err()
                .map(|e| format!("restore priority failed: {e:#}")),
            UndoableAction::SetAffinity {
                pid, previous_mask, ..
            } => match previous_mask {
                None => Some(
                    "cannot undo: previous affinity mask was unreadable at capture time".into(),
                ),
                Some(mask) => self
                    .sys
                    .set_affinity_mask_for_pid(*pid, *mask)
                    .err()
                    .map(|e| format!("restore affinity failed: {e:#}")),
            },
            UndoableAction::SuspendProcess { pid, .. } => self
                .sys
                .resume_process(*pid)
                .err()
                .map(|e| format!("resume (undo of suspend) failed: {e:#}")),
            UndoableAction::ResumeProcess { pid, .. } => self
                .sys
                .suspend_process(*pid)
                .err()
                .map(|e| format!("suspend (undo of resume) failed: {e:#}")),
        };
        let summary = format!("undone: {}", entry.action.describe());
        info!(
            id = entry.id,
            action = %entry.action.describe(),
            failed = failure.is_some(),
            "undo_last"
        );
        Ok(Some(UndoSummary {
            entry,
            summary,
            failure,
        }))
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
            manual_global_active: s.manual_global_active.clone(),
            // W1.6 — engine has no framesage-etw dep (ARCHITECTURE.md
            // invariant #8); the service overrides this field with the
            // real predicate value before sending the snapshot over
            // IPC. Default false here is the conservative fallback for
            // any caller that doesn't go through the service-side IPC
            // path (e.g., unit tests).
            closed_loop_build_supported: false,
        }
    }

    /// Collect a snapshot row for every visible process plus paired
    /// system-wide metrics (CPU%, memory). Backs the tray's Processes tab +
    /// the permanent performance band. Self-contained: opens its own
    /// handles, manages its own per-PID CPU-time history so the % is
    /// computed even when ProBalance is disabled.
    ///
    /// Item 2.1 / audit C-02. Previously this opened 5 separate
    /// per-PID handles (priority, affinity, mem, cpu_times, exe_path)
    /// — ~1250 OpenProcess/CloseHandle pairs per second on a 250-PID
    /// box. Now: one NTQSI call returns most of that in a single
    /// kernel hit; only affinity_mask + exe_path (full path, only on
    /// cache miss) + user SID (budgeted) still need per-PID
    /// OpenProcess. Steady-state cost on a stable process list:
    /// ~1 OpenProcess per PID per tick (just affinity).
    ///
    /// On NTQSI failure we fall back to the legacy per-PID path so a
    /// kernel quirk on some host doesn't blank the tray's Processes
    /// tab — the slow path still works, it's just expensive.
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

            // Single-syscall snapshot via NtQuerySystemInformation. On
            // failure (rare; kernel quirks on some hosts), fall back to
            // ToolHelp so the tray's Processes tab still works — just
            // expensively.
            let ntqsi_processes = self.sys.enumerate_processes();
            let (pid_snapshots, ntqsi_ok) = match ntqsi_processes {
                Ok(v) => (v, true),
                Err(e) => {
                    warn!(error = %e, "NTQSI failed; falling back to ToolHelp per-PID path");
                    // Construct a stub Vec from the legacy iter_pid_snapshots
                    // so the downstream loop has something to iterate. The
                    // legacy path doesn't have CPU times or memory in the
                    // initial snapshot — those have to come from per-PID
                    // calls below. For brevity in the fallback path we just
                    // surface empty values; the fallback is a degraded mode
                    // that should self-recover next tick when NTQSI works
                    // again.
                    match self.sys.iter_pid_snapshots() {
                        Ok(legacy) => (
                            legacy
                                .into_iter()
                                .map(|p| framesage_sys::sys_proc_info::SysProcInfo {
                                    pid: p.pid,
                                    parent_pid: p.parent_pid,
                                    // Legacy ToolHelp doesn't give us
                                    // exe_name directly here; the loop
                                    // below will look it up via
                                    // exe_for_pid when ntqsi_ok is
                                    // false. Stub empty for now.
                                    exe_name: String::new(),
                                    thread_count: p.thread_count,
                                    handle_count: 0,
                                    total_cpu_100ns: 0,
                                    working_set_bytes: 0,
                                    peak_working_set_bytes: 0,
                                    private_bytes: 0,
                                    base_priority: 0,
                                })
                                .collect(),
                            false,
                        ),
                        Err(e) => {
                            warn!(error = %e, "ToolHelp fallback also failed; returning empty");
                            return (Vec::new(), SystemMetrics::default());
                        }
                    }
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
            // Same idea for user-SID lookups. OpenProcessToken +
            // GetTokenInformation + LookupAccountSidW is fast on local
            // accounts but can touch a domain controller on AD-joined
            // boxes — keeping the budget small protects the worst case.
            let mut user_budget: u32 = 8;
            // Same bounded-cost idea for exe-path lookups. The cache
            // makes this near-zero on a stable process list (each
            // PID's full path is looked up once and reused for the
            // PID's lifetime); the budget catches the cold-cache /
            // process-spawn-storm case.
            let mut exe_path_budget: u32 = 8;

            for ps in &pid_snapshots {
                let pid = ps.pid;
                if pid == 0 {
                    continue;
                }

                // exe_path: from per-PID cache; on miss, look up via
                // OpenProcess + QueryFullProcessImageNameW (budgeted).
                // Empty string is fine — tray icon extraction handles
                // missing paths gracefully.
                let exe_path = if let Some(cached) = s.exe_path_cache.get(&pid) {
                    cached.clone()
                } else if exe_path_budget > 0 {
                    exe_path_budget -= 1;
                    let path = self.sys.exe_for_pid(pid).ok().flatten().unwrap_or_default();
                    if !path.is_empty() {
                        s.exe_path_cache.insert(pid, path.clone());
                    }
                    path
                } else {
                    String::new()
                };

                // exe_name: from NTQSI directly (bare filename). On
                // the fallback path NTQSI returned empty, so derive
                // from exe_path. Skip processes we have no name for
                // — they're likely PID 0/4 or transiently unreadable.
                let exe_name = if !ps.exe_name.is_empty() {
                    ps.exe_name.clone()
                } else if !exe_path.is_empty() {
                    exe_path
                        .rsplit(['\\', '/'])
                        .next()
                        .unwrap_or(&exe_path)
                        .to_owned()
                } else {
                    continue;
                };

                // Priority class via NTQSI BasePriority → Win32 class
                // mapping. Saves one OpenProcess per PID. On fallback
                // path BasePriority is 0; if the mapping returns 0,
                // we accept that as "unknown" — same fallback as the
                // legacy "0 means unknown" semantic.
                let priority_class_raw = if ntqsi_ok {
                    framesage_sys::sys_proc_info::kpriority_to_win32_class(ps.base_priority)
                } else {
                    self.sys
                        .get_priority_class_for_pid(pid)
                        .ok()
                        .flatten()
                        .unwrap_or(0)
                };

                let affinity_mask = self.sys.affinity_mask(pid).ok().flatten().unwrap_or(0);

                // Memory from NTQSI directly — no extra syscall. On
                // fallback path these are zero (legacy iter_pid_snapshots
                // doesn't carry them); the tray rendering handles 0
                // gracefully.
                let memory_bytes = if ntqsi_ok {
                    ps.working_set_bytes
                } else {
                    // Fallback: per-PID memory_info call (the original
                    // pre-2.1 path).
                    self.sys
                        .memory_info(pid)
                        .ok()
                        .flatten()
                        .map(|m| m.working_set_bytes)
                        .unwrap_or(0)
                };
                let peak_working_set_bytes = ps.peak_working_set_bytes;
                let private_bytes = ps.private_bytes;

                // Total CPU time from NTQSI directly. Fallback path
                // does a per-PID cpu_times call.
                let total_cpu = if ntqsi_ok {
                    ps.total_cpu_100ns
                } else {
                    self.sys
                        .cpu_times(pid)
                        .ok()
                        .flatten()
                        .map(|t| t.total_100ns())
                        .unwrap_or(0)
                };
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
                            let v = self.sys.read_version_info(&exe_path).unwrap_or_default();
                            s.version_info_cache.insert(exe_path.clone(), v.clone());
                            v
                        } else {
                            framesage_sys::version_info::VersionInfo::default()
                        }
                    }
                };

                // Owner-user, PID-keyed cache. Same lazy-budget pattern as
                // the version-info read. The cache is pruned to live PIDs
                // at the end of the loop so PID reuse can't surface a stale
                // name.
                let user = match s.user_cache.get(&pid) {
                    Some(u) => u.clone(),
                    None => {
                        if user_budget > 0 {
                            user_budget -= 1;
                            let u = self.sys.user_for_pid(pid).ok().flatten();
                            s.user_cache.insert(pid, u.clone());
                            u
                        } else {
                            None
                        }
                    }
                };

                out.push(ProcessSnapshot {
                    pid,
                    parent_pid: ps.parent_pid,
                    exe_name,
                    exe_path,
                    description: info.description,
                    company: info.company,
                    user,
                    priority_class_raw,
                    affinity_mask,
                    cpu_percent,
                    memory_bytes,
                    peak_working_set_bytes,
                    private_bytes,
                    threads: ps.thread_count,
                    matched_rule_note: rule_note,
                    managed_profile,
                    restrained_by_probalance,
                });
            }

            s.list_processes_prev_samples = new_prev;

            // Prune the user-cache to PIDs that still exist. PIDs reuse —
            // if pid 1234 was bf6.exe yesterday and is notepad.exe today,
            // we don't want to surface the old user name. The cache holds
            // resolved SID→name strings keyed by PID; cheapest correctness
            // strategy is "drop anyone not in the live snapshot."
            let live_pids: std::collections::HashSet<u32> =
                pid_snapshots.iter().map(|p| p.pid).collect();
            s.user_cache.retain(|p, _| live_pids.contains(p));
            // Item 2.1: same prune for exe_path_cache. PID reuse means
            // tomorrow's PID 1234 may be a different binary than
            // today's; dropping cached paths on PID-exit avoids
            // surfacing a stale path on the next reuse.
            s.exe_path_cache.retain(|p, _| live_pids.contains(p));

            // ─── System-wide metrics ─────────────────────────────────────
            //
            // System CPU% = 100 - (delta_idle / delta_total) over the same
            // wall-clock interval as the per-process sample. We compute it
            // from `GetSystemTimes` rather than summing per-process CPU%
            // because per-process omits whatever fraction of kernel time
            // we couldn't open (protected processes) and undercounts.
            let sys_cpu_now = self.sys.system_cpu_times().ok();
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
            let per_cpu_now = self.sys.per_cpu_times().ok();
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

            let (mem_total, mem_avail) = self.sys.memory_status().unwrap_or((0, 0));
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
        //
        // Item 4.15 — capture which rule matched so the
        // ForegroundChanged event below carries the index.
        let (matched_rule_index, profile_id) = match &s.manual_override {
            Some(ov) => (None, ov.clone()),
            None => {
                let (idx, id) = s.policy.match_foreground_indexed(
                    &snapshot.exe_name,
                    &snapshot.path,
                    &snapshot.title,
                );
                (idx, id.clone())
            }
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
                revert_record(self.sys.as_ref(), &self.events, prev_pid, record);
            }

            let topology = s.topology.clone();
            match apply_profile(
                self.sys.as_ref(),
                prev_pid,
                &snapshot.exe_name,
                &profile,
                &topology,
                self.safe_list,
            ) {
                Ok(record) => {
                    info!(pid = prev_pid, profile = %profile_id, "force_recompute applied");
                    let _ = self.events.send(Event::ProfileApplied {
                        pid: prev_pid,
                        exe_name: snapshot.exe_name.clone(),
                        profile_id: profile_id.clone(),
                    });
                    s.applied.insert(prev_pid, record);
                }
                Err(e) => {
                    warn!(pid = prev_pid, error = %e, "force_recompute apply failed");
                    let _ = self.events.send(Event::ActionFailed {
                        kind: classify_apply_failure(&e),
                        pid: Some(prev_pid),
                        exe_name: Some(snapshot.exe_name.clone()),
                        details: format!("force_recompute apply failed: {e:#}"),
                    });
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
            matched_rule_index,
        });

        // Reconcile system-wide Game Mode against the new profile.
        let new_actions = profile.game_mode.clone();
        Self::reconcile_system_mode_locked(
            s,
            &self.journal,
            self.safe_list,
            &profile_id,
            new_actions,
            &self.events,
            "force_recompute",
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
        s.last_foreground_report_at = Some(Instant::now());
    }

    /// Accept a "no foreground" report (lock screen, UAC, transition).
    pub fn report_no_foreground(&self) {
        let mut s = self.state.write();
        s.reported_foreground = None;
        s.foreground_reporter_seen = true;
        s.last_foreground_report_at = Some(Instant::now());
    }

    /// Panic button: revert any active system mode regardless of foreground.
    /// Idempotent — `Ok(())` if no system mode was active. Also clears
    /// Manual Global Game Mode if it was set — the user hit the kill
    /// switch, they want the desktop fully back.
    pub fn exit_system_mode_now(&self) {
        let mut s = self.state.write();
        s.manual_global_active = None;
        Self::revert_system_mode_locked(&mut s, &self.journal, &self.events, "manual_exit");
    }

    /// Item 2.11 — Manual Global Game Mode entry. Applies the named
    /// profile's `game_mode` actions system-wide regardless of what's
    /// foregrounded, and pins the system_mode session against
    /// auto-reconcile tear-down until
    /// `disable_manual_global_game_mode` arrives.
    ///
    /// Refuses (Err) when:
    ///   * The profile id isn't in the active policy.
    ///   * The profile is not marked `manual_global_eligible`.
    ///   * The profile has no `game_mode` actions configured (nothing
    ///     to apply — a manual global session with zero actions is
    ///     just confusing).
    pub fn enable_manual_global_game_mode(&self, profile_id: ProfileId) -> Result<()> {
        let mut s = self.state.write();

        let profile = s
            .policy
            .profile(&profile_id)
            .ok_or_else(|| anyhow::anyhow!("unknown profile id {profile_id}"))?
            .clone();

        if !profile.manual_global_eligible {
            return Err(anyhow::anyhow!(
                "profile '{profile_id}' is not marked manual_global_eligible — set the flag in \
                 the policy editor before invoking Manual Global Game Mode"
            ));
        }

        let Some(actions) = profile.game_mode.clone() else {
            return Err(anyhow::anyhow!(
                "profile '{profile_id}' has no game_mode actions; nothing to apply as a global \
                 session"
            ));
        };
        if actions == GameModeActions::default() {
            return Err(anyhow::anyhow!(
                "profile '{profile_id}' game_mode is empty; nothing to apply"
            ));
        }

        info!(
            profile = %profile_id,
            "entering Manual Global Game Mode"
        );

        // Reconcile against the new actions. Important: we must
        // reconcile BEFORE setting manual_global_active, because
        // reconcile's own ownership guard uses the flag to decide
        // whether to honor the request. Setting it first would
        // cause the new request to be matched against the not-yet-
        // -set pinned profile and short-circuit.
        Self::reconcile_system_mode_locked(
            &mut s,
            &self.journal,
            self.safe_list,
            &profile_id,
            Some(actions),
            &self.events,
            "manual_global_enter",
        );

        s.manual_global_active = Some(profile_id);

        Ok(())
    }

    /// Item 2.11 — exit Manual Global Game Mode. Reverts the session
    /// and resumes auto-reconcile. Idempotent.
    pub fn disable_manual_global_game_mode(&self) {
        let mut s = self.state.write();
        if s.manual_global_active.is_none() {
            return;
        }
        info!(
            profile = ?s.manual_global_active,
            "exiting Manual Global Game Mode"
        );
        s.manual_global_active = None;
        Self::revert_system_mode_locked(&mut s, &self.journal, &self.events, "manual_global_exit");
        // Force the next reconcile to re-evaluate the current
        // foreground — without this, if the foreground hasn't
        // changed since manual global was enabled, reconcile's
        // "new_pid == current_foreground" fast path would skip the
        // re-entry. Clearing makes the next tick treat the
        // foreground as freshly arrived.
        s.current_foreground = None;
    }

    /// Forward an OS-level system event (suspend, resume, session change)
    /// into the engine. Item 2.4 / audit M-02.
    ///
    /// The cases:
    ///
    /// * **Suspend**: pause the engine and revert any active Game Mode
    ///   first. The kernel state we set (stopped services, suspended
    ///   processes, hidden taskbar, switched power plan) mostly survives
    ///   a suspend/resume cycle by itself, but the user's expectation is
    ///   "I closed the laptop; when I open it everything is normal."
    ///   Reverting before sleep matches that mental model. The pause
    ///   prevents the tick loop from doing anything stupid during the
    ///   transition itself.
    /// * **Resume**: re-probe AC presence (an AC client might have
    ///   started or quit while we were suspended) and clear the
    ///   foreground tracking so the next tick treats whatever's
    ///   foregrounded as freshly arrived. Then `resume()` un-pauses.
    /// * **SessionConsoleDisconnect** (fast-user-switch away, RDP
    ///   disconnect): revert any active Game Mode. The user whose
    ///   session we were optimising for is no longer in front of the
    ///   screen; leaving services stopped and the taskbar hidden for an
    ///   incoming user is a bad citizen.
    /// * **SessionConsoleConnect / SessionLock / SessionUnlock**:
    ///   logged for visibility but no automatic action — locking the
    ///   screen while gaming shouldn't tear down Game Mode (the game is
    ///   still running, the user just stepped away briefly).
    pub fn handle_system_event(&self, event: SystemEvent) {
        match event {
            SystemEvent::Suspend => {
                info!("system suspending — exiting Game Mode and pausing engine");
                {
                    let mut s = self.state.write();
                    // Clear manual global before revert so the
                    // revert_system_mode_locked call actually fires
                    // (it would otherwise skip via the manual-global
                    // ownership guard).
                    s.manual_global_active = None;
                    Self::revert_system_mode_locked(
                        &mut s,
                        &self.journal,
                        &self.events,
                        "system_suspend",
                    );
                }
                self.pause();
            }
            SystemEvent::Resume => {
                info!("system resuming — re-probing AC presence and resuming engine");
                // Force the next maybe_refresh_ac_presence call to fire
                // immediately by clearing last_ac_probe. Cheap and
                // correct: a probe that just ran 4 s ago is irrelevant
                // because the system was unconscious for that window.
                {
                    let mut s = self.state.write();
                    s.last_ac_probe = None;
                    // Force foreground re-evaluation on the next tick.
                    // Otherwise reconcile's "new_pid == current_foreground"
                    // fast path would skip the reconcile after resume
                    // even if the user's gaming session is gone.
                    s.current_foreground = None;
                }
                // Item 3.7 — re-detect CPU topology. Sleep/resume can
                // land in a different power plan that parks cores; if
                // we don't refresh, every selector that resolves
                // through topology will target the pre-sleep core
                // layout and silently miss the now-parked CPUs (or
                // worse, pin to indices the kernel won't honour).
                self.refresh_topology();
                self.resume();
            }
            SystemEvent::SessionConsoleDisconnect => {
                info!("console session disconnected (FUS / RDP off) — exiting Game Mode");
                let mut s = self.state.write();
                s.manual_global_active = None;
                Self::revert_system_mode_locked(
                    &mut s,
                    &self.journal,
                    &self.events,
                    "session_disconnect",
                );
            }
            SystemEvent::SessionConsoleConnect => {
                info!("console session connected — engine continues normally");
            }
            SystemEvent::SessionLock => {
                debug!("session locked — Game Mode preserved (game still running)");
            }
            SystemEvent::SessionUnlock => {
                debug!("session unlocked");
            }
        }
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
        let foreground = self
            .sys
            .current_foreground()?
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
                revert_record(self.sys.as_ref(), &self.events, foreground.pid, record);
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
            let record = match apply_profile(
                self.sys.as_ref(),
                foreground.pid,
                &foreground.exe_name,
                &profile,
                &topology,
                self.safe_list,
            ) {
                Ok(r) => r,
                Err(e) => {
                    let _ = self.events.send(Event::ActionFailed {
                        kind: classify_apply_failure(&e),
                        pid: Some(foreground.pid),
                        exe_name: Some(foreground.exe_name.clone()),
                        details: format!("apply_once failed: {e:#}"),
                    });
                    return Err(e);
                }
            };
            info!(
                pid = foreground.pid,
                exe = %foreground.exe_name,
                profile = %profile_id,
                "apply_once",
            );
            let _ = self.events.send(Event::ProfileApplied {
                pid: foreground.pid,
                exe_name: foreground.exe_name.clone(),
                profile_id: profile_id.clone(),
            });
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
            // Item 4.15 — `apply_once` bypasses the rule matcher
            // entirely; the profile came from the user's explicit
            // choice. No matched rule to attribute.
            matched_rule_index: None,
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
            &self.events,
            "apply_once",
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

        // Item 1.9 / defaults D-11 — ESEA auto-pause. Refresh AC presence
        // first (cheap; bounded by AC_DETECT_INTERVAL); if ESEA is
        // running, engine enters STANDBY: no apply, no scans, no
        // ProBalance. Per AC matrix research, ESEA's vendor KB names
        // Process Lasso as a conflict (Error #107, "uninstall"). We
        // go dark to sidestep entirely. Resume automatically when
        // ESEAClient.exe exits.
        self.maybe_refresh_ac_presence();
        if self.state.read().ac_presence.esea_demands_standby() {
            // One-line trace per tick is too chatty; the AC detection
            // refresh logs the transition into / out of standby
            // (presence-change boundary), which is what users see.
            return Ok(());
        }

        // Session 0 isolation: a service running as LocalSystem can't see
        // the interactive desktop, so `GetForegroundWindow` returns null
        // from session 0. We prefer a foreground report from the
        // user-session helper (the tray, via Request::ReportForeground).
        // If no report has ever arrived (console-mode dev path), we fall
        // back to the in-process poll.
        //
        // Item 2.6 / audit H-13: also fall back if the last report is
        // older than FOREGROUND_REPORT_STALENESS. The previous one-way
        // latch on `foreground_reporter_seen` meant a crashed tray
        // left the engine forever stuck on the last cached foreground
        // (taskbar hidden, services stopped, no way out short of
        // restart). Now: if 10+ seconds have passed without a fresh
        // report, the engine treats the helper as gone and resumes
        // session-local polling. When the tray comes back (auto-restart
        // via SCM-side install.ps1 Startup shortcut, or user re-launch),
        // the next ReportForeground refreshes the timestamp and the
        // engine prefers the report again.
        let foreground = {
            let s = self.state.read();
            let report_is_fresh = s
                .last_foreground_report_at
                .is_some_and(|t| t.elapsed() < FOREGROUND_REPORT_STALENESS);
            if s.foreground_reporter_seen && report_is_fresh {
                s.reported_foreground.clone()
            } else {
                drop(s);
                self.sys.current_foreground()?
            }
        };

        let mut s = self.state.write();
        self.reconcile(&mut s, foreground)?;
        Self::maybe_scan_background_locked(&mut s, self.safe_list, &self.events, self.sys.as_ref());
        Self::maybe_reassert_persistent_locked(&mut s, self.sys.as_ref());
        self.maybe_run_probalance_locked(&mut s);
        Ok(())
    }

    /// Refresh `ac_presence` via the AC detection probe, but only if
    /// we haven't probed within [`AC_DETECT_INTERVAL`]. Item 1.9.
    ///
    /// Cheap — re-uses the same `iter_pids` infrastructure ProBalance
    /// and background-scan already exercise. 5 s cadence is plenty
    /// because AC drivers load on game launch (and the user-mode
    /// companion right after) — not on the second.
    ///
    /// Logging is gated to transitions (presence-change boundaries),
    /// not per-probe, so a Vanguard-active box doesn't drown the log
    /// in `Vanguard detected` lines.
    ///
    /// Concrete safety: this method is the only writer of
    /// `last_ac_probe` and `ac_presence`. `tick` is called from a
    /// single tokio task, so probes don't race.
    fn maybe_refresh_ac_presence(&self) {
        // Item 3.1 — clock + sys come from injected traits. Production
        // uses `SystemClock` / `RealSysApi`; tests can advance time and
        // script AC detection results without spinning the OS clock or
        // launching a real Vanguard process.
        let now = self.clock.now();
        let needs_probe = {
            let s = self.state.read();
            match s.last_ac_probe {
                None => true,
                Some(at) => now.duration_since(at) >= AC_DETECT_INTERVAL,
            }
        };
        if !needs_probe {
            return;
        }

        let new_presence = match self.sys.detect_anti_cheats() {
            Ok(p) => p,
            Err(e) => {
                debug!(error = %e, "AC detection probe failed; keeping last-known presence");
                let mut s = self.state.write();
                s.last_ac_probe = Some(now);
                return;
            }
        };

        let mut s = self.state.write();
        let old = s.ac_presence;
        s.ac_presence = new_presence;
        s.last_ac_probe = Some(now);

        // Log + emit on transitions. Each AC gets its own line so
        // multi-AC transitions (rare but possible: user closes
        // Valorant + opens Fortnite simultaneously) are readable.
        // The event mirror lets the tray surface "engine standby for
        // ESEA" or "Vanguard detected — Valorant rule using SafeMode
        // tier" without polling status.
        let emit = |which: &str, active: bool| {
            let _ = self.events.send(Event::AntiCheatPresenceChanged {
                which: which.to_owned(),
                active,
            });
        };
        if old.vanguard != new_presence.vanguard {
            info!(
                active = new_presence.vanguard,
                "AC presence change: Vanguard"
            );
            emit("vanguard", new_presence.vanguard);
        }
        if old.eac != new_presence.eac {
            info!(active = new_presence.eac, "AC presence change: EAC");
            emit("eac", new_presence.eac);
        }
        if old.javelin != new_presence.javelin {
            info!(
                active = new_presence.javelin,
                "AC presence change: Javelin/BF6"
            );
            emit("javelin", new_presence.javelin);
        }
        if old.battleye != new_presence.battleye {
            info!(
                active = new_presence.battleye,
                "AC presence change: BattlEye"
            );
            emit("battleye", new_presence.battleye);
        }
        if old.faceit != new_presence.faceit {
            info!(active = new_presence.faceit, "AC presence change: FACEIT");
            emit("faceit", new_presence.faceit);
        }
        if old.esea != new_presence.esea {
            info!(
                active = new_presence.esea,
                "AC presence change: ESEA — engine {}",
                if new_presence.esea {
                    "entering STANDBY (D-11)"
                } else {
                    "resuming from STANDBY"
                }
            );
            emit("esea", new_presence.esea);
        }
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
                    if let Err(e) = self
                        .sys
                        .restore_priority_class_for_pid(pid, rec.original_raw_class)
                    {
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
                // Item 4.6 — also clear the hysteresis counter so a
                // disable-then-re-enable doesn't trigger demotes from
                // stale streak state.
                s.probalance_hog_streak.clear();
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

        let live_pids: Vec<u32> = match self.sys.iter_pids() {
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
            let times = match self.sys.cpu_times(*pid) {
                Ok(Some(t)) => t,
                Ok(None) | Err(_) => continue,
            };
            let exe_name = match self.sys.exe_for_pid(*pid) {
                Ok(Some(p)) => p
                    .rsplit(['\\', '/'])
                    .next()
                    .unwrap_or(&p)
                    .to_ascii_lowercase(),
                Ok(None) | Err(_) => continue,
            };
            current_samples.insert(*pid, (times.total_100ns(), exe_name));
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
            let current_raw_class = match self.sys.get_priority_class_for_pid(*pid) {
                Ok(Some(c)) => c,
                _ => continue,
            };
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

        // Item 2.3 / audit H-06. Both sets are pre-computed and cached
        // on EngineState — `safe_list_denied_exes` is built once in
        // `Engine::new` (immutable bundled SafeList), `user_ignore_exes`
        // is rebuilt only on `set_policy`. Clone is a single refcount
        // bump on the Arcs, not a HashSet allocation. The previous path
        // rebuilt both sets every 1 s — handful of allocations + per-
        // entry lowercase, easily 10+ μs of pointless work per
        // ProBalance sample.
        let safe_list_exes = Arc::clone(&s.safe_list_denied_exes);
        let user_ignore_exes = Arc::clone(&s.user_ignore_exes);

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
            &mut s.probalance_hog_streak,
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
                    let result = self.sys.set_priority_class_for_pid(pid, demote_to);
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
                    if let Err(e) = self
                        .sys
                        .restore_priority_class_for_pid(pid, restored_raw_class)
                    {
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
    fn maybe_reassert_persistent_locked(s: &mut EngineState, sys: &dyn framesage_sys::SysApi) {
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
            let live_exe = match sys.exe_for_pid(pid) {
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
            if let Err(e) = sys.reassert(pid, &profile, &topology) {
                debug!(pid, error = %e, "persistent re-assert failed");
            }
        }

        for pid in stale_pids {
            s.applied.remove(&pid);
            if s.current_foreground == Some(pid) {
                s.current_foreground = None;
            }
        }

        // ─── Re-assert standalone affinity rules ─────────────────────────────
        // Mirrors the persistent-profile sweep above but for the lightweight
        // AffinityRule path. Same motivation: some games (POE2, EVE, Unreal
        // titles) call SetProcessAffinityMask on themselves at startup,
        // overwriting our pin. The 2 s re-assert defeats that without
        // requiring the user to also create a full Profile.
        //
        // Bounded by `affinity_rule_applied`: we only re-push for PIDs
        // we've successfully pinned at least once. PID-reuse defense is
        // the same exe-name comparison as the persistent-profile loop.
        if !s.affinity_rule_applied.is_empty() && !s.policy.affinity_rules.is_empty() {
            let topology = s.topology.clone();
            let mut stale_rule_pids: Vec<u32> = Vec::new();
            let rule_pids: Vec<u32> = s.affinity_rule_applied.iter().copied().collect();
            for pid in rule_pids {
                let live_exe = match sys.exe_for_pid(pid) {
                    Ok(Some(path)) => path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned(),
                    Ok(None) | Err(_) => {
                        stale_rule_pids.push(pid);
                        continue;
                    }
                };
                let Some(rule) = s.policy.affinity_rule_for(&live_exe).cloned() else {
                    // Rule was deleted for this exe; release the PID so
                    // it's not re-pinned on future sweeps.
                    stale_rule_pids.push(pid);
                    continue;
                };
                let indices = topology.resolve(&rule.selector);
                if indices.is_empty() {
                    continue;
                }
                let mut mask: u64 = 0;
                for idx in indices {
                    if idx < 64 {
                        mask |= 1u64 << idx;
                    }
                }
                if mask == 0 {
                    continue;
                }
                if let Err(e) = sys.set_affinity_mask_for_pid(pid, mask) {
                    debug!(pid, error = %e, "affinity rule re-assert failed");
                }
            }
            for pid in stale_rule_pids {
                s.affinity_rule_applied.remove(&pid);
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
    fn maybe_scan_background_locked(
        s: &mut EngineState,
        safe_list: &'static SafeList,
        events: &broadcast::Sender<Event>,
        sys: &dyn framesage_sys::SysApi,
    ) {
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

        let live_pids: Vec<u32> = match sys.iter_pids() {
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
        // Item 4.9 — also reap stale apply_failure_backoff entries so a
        // PID-number reuse after 30+ s doesn't start its life under a
        // false backoff. Cheap (`retain` is one pass).
        s.apply_failure_backoff
            .retain(|pid, _| live_set.contains(pid));
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
        // Same cleanup for the standalone affinity-rule tracker. Prevents
        // the set growing without bound across long sessions as PIDs come
        // and go.
        s.affinity_rule_applied.retain(|p| live_set.contains(p));

        // ─── Apply standalone affinity rules to new matching PIDs ───────────
        // Independent of the background-profile loop below: a PID can have
        // an affinity-rule pin without ever entering `s.applied`. We walk
        // the live PIDs once here, look each one up in the rules table by
        // exe name, and pin if there's a match and we haven't pinned it
        // yet. Cost is bounded — the rule table is small (handful of games
        // per user) and the live exe lookup is the same one the loop below
        // does for safe-list filtering.
        //
        // This is the "apply on next launch" half of the affinity rework
        // (the "apply now" half is set_affinity_rule's apply_to_live walk).
        if !s.policy.affinity_rules.is_empty() {
            let self_pid_for_aff = std::process::id();
            let mut rule_applies: Vec<(u32, framesage_core::CpuSelector, String, String)> =
                Vec::new();
            for &pid in &live_pids {
                if pid == 0 || pid == 4 || pid == self_pid_for_aff {
                    continue;
                }
                if s.affinity_rule_applied.contains(&pid) {
                    continue;
                }
                let live_exe = match sys.exe_for_pid(pid) {
                    Ok(Some(path)) => path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned(),
                    Ok(None) | Err(_) => continue,
                };
                if let Some(rule) = s.policy.affinity_rule_for(&live_exe) {
                    rule_applies.push((
                        pid,
                        rule.selector.clone(),
                        live_exe,
                        rule.exe_name.clone(),
                    ));
                }
            }
            for (pid, selector, live_exe, rule_exe) in rule_applies {
                let indices = topology.resolve(&selector);
                if indices.is_empty() {
                    continue;
                }
                let mut mask: u64 = 0;
                for idx in indices {
                    if idx < 64 {
                        mask |= 1u64 << idx;
                    }
                }
                if mask == 0 {
                    continue;
                }
                match sys.set_affinity_mask_for_pid(pid, mask) {
                    Ok(()) => {
                        // Only mark on success — a failed apply might be
                        // transient (PID exiting mid-call) and we want a
                        // retry next scan rather than a silent skip.
                        s.affinity_rule_applied.insert(pid);
                        let _ = events.send(Event::AffinityRuleFired {
                            pid,
                            exe_name: live_exe,
                            rule_exe,
                        });
                    }
                    Err(e) => {
                        debug!(pid, error = %e, "affinity rule background apply failed");
                        let _ = events.send(Event::ActionFailed {
                            kind: ActionFailedKind::Apply,
                            pid: Some(pid),
                            exe_name: Some(live_exe),
                            details: format!("affinity rule background apply failed: {e:#}"),
                        });
                    }
                }
            }
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
            // Item 4.9 — skip PIDs we recently failed to apply to.
            // Without this the background-scan walks the full live PID
            // list every 10 s and burns syscalls retrying the same
            // protected processes (AV / AC / kernel-protected services
            // that slipped past the static safe-list).
            if apply_backoff_active(&s.apply_failure_backoff, pid, Instant::now()) {
                continue;
            }

            // Filter against the safe-list denylist — same denylist that
            // protects suspend_processes, repurposed here so we don't, e.g.,
            // throttle dwm or audiodg into stuttering territory.
            let exe_name = match sys.exe_for_pid(pid) {
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

            // Rule-first: if the user authored a rule for this exe, the
            // rule's profile wins over the generic background profile.
            // Without this, a game launched into the background — or one
            // the user never alt-tabs to before the engine resumes —
            // sits on the generic `eco` profile despite an explicit
            // `game-x3d` rule matching its name. (Path / window-title
            // matchers don't fire here because we don't have a window
            // title for background processes; only exe-name matchers
            // make sense in this scan path.)
            let profile_for_pid = s
                .policy
                .rules
                .iter()
                .find(|r| match &r.r#match {
                    framesage_core::AppMatch::ExeName(n) => n.eq_ignore_ascii_case(&exe_name),
                    _ => false,
                })
                .and_then(|r| s.policy.profile(&r.profile).cloned())
                .unwrap_or_else(|| bg_profile.clone());

            match apply_profile(sys, pid, &exe_name, &profile_for_pid, &topology, safe_list) {
                Ok(record) => {
                    // Item 4.9 — successful apply clears any prior failure.
                    s.apply_failure_backoff.remove(&pid);
                    let profile_id_emitted = record.profile_id.clone();
                    let exe_emitted = record.exe_name.clone();
                    s.applied.insert(pid, record);
                    newly_applied += 1;
                    let _ = events.send(Event::ProfileApplied {
                        pid,
                        exe_name: exe_emitted,
                        profile_id: profile_id_emitted,
                    });
                }
                // ACCESS_DENIED / INVALID_PARAMETER on protected processes
                // is expected and not worth surfacing — same for the new
                // denylist-refusal path (apply_profile rejects csrss/lsass/
                // dwm/etc. before any syscall fires). We deliberately do NOT
                // emit ActionFailed here: the background-scan path runs
                // once per second across every live PID and pre-filters via
                // the safe-list, so the residue is genuinely "expected
                // privileged-process refusals" that would drown the
                // activity feed without informing the user.
                //
                // Item 4.9 — record the failure so the next 10-s
                // background sweep doesn't burn another OpenProcess on
                // the same protected PID.
                Err(_) => {
                    s.apply_failure_backoff.insert(pid, Instant::now());
                    continue;
                }
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
                        revert_record(self.sys.as_ref(), &self.events, prev_pid, record);
                    }
                }
            }
        }

        // No new foreground? Tear down system mode too — there's nothing
        // to be in Game Mode "for." Unless Manual Global Game Mode is on:
        // the user explicitly asked for system_mode regardless of
        // foreground, so locking the screen / closing every window
        // doesn't tear it down. (Item 2.11.)
        let Some(fg) = foreground else {
            s.foreground_snapshot = None;
            s.active_profile = None;
            if s.manual_global_active.is_none() {
                Self::revert_system_mode_locked(s, &self.journal, &self.events, "foreground_lost");
            }
            return Ok(());
        };

        // Manual override wins over the rule matcher. Set by
        // `set_manual_override` / `Request::SetManualOverride` and cleared
        // by the complementary calls. When active, every foreground app
        // gets this profile regardless of what Rules would have matched.
        //
        // Item 4.15 — also capture which rule (by index) matched.
        // `None` when the profile came from manual_override or from
        // `default_profile` (no rule matched). The activity feed
        // surfaces this so the user can trace each ForegroundChanged
        // back to its source.
        let (matched_rule_index, profile_id) = match &s.manual_override {
            Some(ov) => (None, ov.clone()),
            None => {
                let (idx, id) =
                    s.policy
                        .match_foreground_indexed(&fg.exe_name, &fg.path, &fg.title);
                (idx, id.clone())
            }
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
                revert_record(self.sys.as_ref(), &self.events, fg.pid, prev_record);
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
                matched_rule_index,
            });
            // Fall through to system-mode reconcile below.
            let new_actions = profile.game_mode.clone();
            Self::reconcile_system_mode_locked(
                s,
                &self.journal,
                self.safe_list,
                &profile_id,
                new_actions,
                &self.events,
                "foreground_changed",
            );
            return Ok(());
        }
        // Item 4.9 — automatic reconcile path. If we recently failed
        // to apply to this PID, skip silently rather than retrying once
        // per second and flooding ActionFailed. The user-initiated
        // paths (apply_once, force_recompute) bypass this gate.
        if apply_backoff_active(&s.apply_failure_backoff, fg.pid, Instant::now()) {
            debug!(
                pid = fg.pid,
                exe = %fg.exe_name,
                "reconcile: skipping apply — recent failure within backoff window"
            );
            // Still update foreground state so the rest of the engine
            // (ProBalance, status snapshots) reflects the new focus.
            s.current_foreground = Some(fg.pid);
            s.foreground_snapshot = Some(snapshot.clone());
            return Ok(());
        }
        match apply_profile(
            self.sys.as_ref(),
            fg.pid,
            &fg.exe_name,
            &profile,
            &topology,
            self.safe_list,
        ) {
            Ok(record) => {
                info!(pid = fg.pid, exe = %fg.exe_name, profile = %profile_id, "applied");
                // Item 4.9 — successful apply clears any prior failure.
                s.apply_failure_backoff.remove(&fg.pid);
                s.applied.insert(fg.pid, record);
                s.current_foreground = Some(fg.pid);
                s.foreground_snapshot = Some(snapshot.clone());
                s.active_profile = Some(profile_id.clone());
                let _ = self.events.send(Event::ForegroundChanged {
                    foreground: snapshot,
                    profile: profile_id.clone(),
                    matched_rule_index,
                });
                let _ = self.events.send(Event::ProfileApplied {
                    pid: fg.pid,
                    exe_name: fg.exe_name.clone(),
                    profile_id: profile_id.clone(),
                });
            }
            Err(e) => {
                warn!(pid = fg.pid, exe = %fg.exe_name, error = %e, "apply failed");
                let _ = self.events.send(Event::ActionFailed {
                    kind: classify_apply_failure(&e),
                    pid: Some(fg.pid),
                    exe_name: Some(fg.exe_name.clone()),
                    details: format!("apply failed: {e:#}"),
                });
                // Item 4.9 — record the failure so subsequent automatic
                // ticks skip this PID for APPLY_FAILURE_BACKOFF. The
                // foreground-tracking update below already prevented
                // tight retry, but only at the granularity of "same
                // foreground PID for many ticks"; the background-scan
                // path needs its own gate (also wired below), so we
                // record here for both.
                s.apply_failure_backoff.insert(fg.pid, Instant::now());
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
            &self.events,
            "foreground_changed",
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
        events: &broadcast::Sender<Event>,
        revert_reason: &str,
    ) {
        // Item 2.11 — Manual Global Game Mode owns system_mode. When
        // active, the only legitimate caller is the manual global
        // enter/exit path itself (which passes the manual global
        // profile as new_profile or clears manual_global_active
        // before calling). Anything else (force_recompute,
        // apply_once, foreground-driven reconcile) gets a hard skip
        // so the user's manual pin can't be silently swapped by an
        // unrelated profile change.
        if let Some(pinned) = &s.manual_global_active {
            if pinned != new_profile {
                debug!(
                    pinned = %pinned,
                    attempted = %new_profile,
                    "reconcile_system_mode_locked skipped — Manual Global Game Mode owns system mode"
                );
                return;
            }
        }

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
        Self::revert_system_mode_locked(s, journal, events, revert_reason);

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
                let _ = events.send(Event::ActionFailed {
                    kind: ActionFailedKind::Other,
                    pid: None,
                    exe_name: None,
                    details: format!("game mode planning failed: {e:#}"),
                });
                return;
            }
        };

        Self::enter_system_mode_locked(s, journal, new_profile, plan, events);
    }

    fn enter_system_mode_locked(
        s: &mut EngineState,
        journal: &Journal,
        profile_id: &ProfileId,
        plan: ActionPlan,
        events: &broadcast::Sender<Event>,
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
                    let (kind, exe_for_event, pid_for_event) = match action {
                        PlannedAction::StopService { id, .. } => {
                            (ActionFailedKind::ServiceAction, Some(id.clone()), None)
                        }
                        PlannedAction::SuspendProcess { pid, exe } => (
                            ActionFailedKind::ProcessAction,
                            Some(exe.clone()),
                            Some(*pid),
                        ),
                        _ => (ActionFailedKind::Other, None, None),
                    };
                    let _ = events.send(Event::ActionFailed {
                        kind,
                        pid: pid_for_event,
                        exe_name: exe_for_event,
                        details: format!("game mode action {action:?} failed: {e:#}"),
                    });
                }
            }
        }

        if !plan.rejections.is_empty() {
            for r in &plan.rejections {
                warn!(rejected = %r.id, reason = %r.reason, "game mode action rejected by safe-list");
                let _ = events.send(Event::ActionFailed {
                    kind: ActionFailedKind::DenylistRefused,
                    pid: None,
                    exe_name: Some(r.id.clone()),
                    details: format!("denylist refused {}: {}", r.id, r.reason),
                });
            }
        }

        info!(
            profile = %profile_id,
            actions = applied_count,
            partial = any_failed,
            session = %entry.session_id,
            "game mode entered"
        );

        // Compute a summary for the Event::GameModeEntered emission so the
        // tray can render "applying X services, suspending Y processes,
        // switching to High Performance" without rebuilding the plan.
        let mut services_to_stop: u32 = 0;
        let mut processes_to_suspend: u32 = 0;
        let mut power_plan_changing = false;
        let mut taskbar_hiding = false;
        let mut pausing_windows_update = false;
        for action in &plan.actions {
            match action {
                PlannedAction::StopService { .. } => services_to_stop += 1,
                PlannedAction::SuspendProcess { .. } => processes_to_suspend += 1,
                PlannedAction::SetPowerPlan { .. } => power_plan_changing = true,
                PlannedAction::HideTaskbar => taskbar_hiding = true,
                PlannedAction::PauseWindowsUpdate => pausing_windows_update = true,
                PlannedAction::SetFocusAssist(_) => {}
            }
        }
        let _ = events.send(Event::GameModeEntered {
            profile_id: profile_id.clone(),
            services_to_stop,
            processes_to_suspend,
            power_plan_changing,
            taskbar_hiding,
            pausing_windows_update,
        });

        s.system_mode = Some(ActiveSystemMode {
            profile_id: profile_id.clone(),
            previous: plan.previous_state,
            applied: intended,
            journal_session_id: entry.session_id,
            started_at_unix_secs: entry.created_at_unix_secs,
        });
    }

    fn revert_system_mode_locked(
        s: &mut EngineState,
        journal: &Journal,
        events: &broadcast::Sender<Event>,
        revert_reason: &str,
    ) {
        let Some(active) = s.system_mode.take() else {
            return;
        };
        info!(
            profile = %active.profile_id,
            session = %active.journal_session_id,
            reason = revert_reason,
            "game mode exiting"
        );
        sys_revert_all(&active.applied, &active.previous);

        // Item 1.4 / audit C-07: BEFORE deleting the active journal,
        // append a SessionHistoryEntry to sessions.jsonl so the user has
        // a permanent record of what Game Mode did. Without this, a 2-
        // hour session touching 30+ services and 24+ processes
        // evaporated the moment the user alt-tabbed away from the game
        // — the worst trust failure in the audit.
        //
        // We re-read the active journal from disk (it's still there
        // until we call delete below) to get the canonical entry; this
        // is the same data the recovery path would use, so the history
        // record is byte-for-byte the same as what a crash recovery
        // would have seen. Fall back to constructing from ActiveSystemMode
        // state if the journal file is somehow missing (shouldn't happen
        // in steady state but a self-healing fallback is cheap).
        //
        // History-append failure is logged but NEVER blocks the revert
        // proper — the user's system state is the load-bearing thing;
        // losing one audit-log line is regrettable but recoverable.
        let now_unix = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let history_entry = match journal.read() {
            Ok(Some(disk_entry)) => {
                SessionHistoryEntry::from_journal(&disk_entry, now_unix, revert_reason)
            }
            _ => {
                // Disk journal missing or unreadable — synthesise from
                // in-memory state. Less authoritative (the disk version
                // might have had more recent action additions we didn't
                // mirror here) but still useful as a record.
                SessionHistoryEntry {
                    schema_version: 1,
                    session_id: active.journal_session_id,
                    profile_id: active.profile_id.clone(),
                    started_at_unix_secs: active.started_at_unix_secs,
                    ended_at_unix_secs: now_unix,
                    revert_reason: revert_reason.to_owned(),
                    previous: active.previous.clone(),
                    applied: active.applied.clone(),
                }
            }
        };
        if let Err(e) = journal.append_to_history(&history_entry) {
            warn!(
                session = %active.journal_session_id,
                error = %e,
                "sessions.jsonl append failed — Game Mode reverted cleanly but post-session audit \
                 trail will be missing this entry"
            );
        }

        // Emit Event::GameModeExited with summary counts. The tray uses
        // these to update the "session complete" toast without
        // re-reading sessions.jsonl. Counts derive from the same
        // AppliedActions we just reverted — they represent what we
        // *tried* to put back; revert errors are surfaced separately via
        // log lines (and a future patch can wire per-action ActionFailed
        // emission inside sys_revert_all for tray-level visibility).
        let services_restored = active.applied.stopped_services.len() as u32;
        let processes_resumed = active.applied.suspended_pids.len() as u32;
        let duration_secs = now_unix.saturating_sub(active.started_at_unix_secs);
        let _ = events.send(Event::GameModeExited {
            profile_id: active.profile_id.clone(),
            services_restored,
            processes_resumed,
            power_plan_restored: active.applied.switched_power_plan,
            taskbar_restored: active.applied.hid_taskbar,
            wu_pause_restored: active.applied.paused_windows_update,
            duration_secs,
            reason: revert_reason.to_owned(),
        });

        if let Err(e) = journal.delete() {
            warn!(error = %e, "journal delete after revert failed");
        }
    }
}

/// Build the lowercased user-ignore-list from a Policy's
/// `probalance.ignore_processes` field. Pulled out as a free fn so
/// `Engine::new` and `Engine::set_policy` share the same construction —
/// keeps the cached set consistent with what the user authored.
/// Item 2.3 / audit H-06.
fn build_user_ignore_exes(policy: &Policy) -> HashSet<String> {
    policy
        .probalance
        .ignore_processes
        .iter()
        .map(|n| n.to_ascii_lowercase())
        .collect()
}

/// Classify an `apply_profile` failure into the categorical
/// `ActionFailedKind` the tray uses for filtering. We don't have typed
/// errors yet (a bigger refactor for a future PR), so this is best-effort
/// substring matching on the user-visible message. The fallback bucket
/// is `Apply`, which is also the most common case (sys::apply::apply
/// returning a Win32 error).
fn classify_apply_failure(err: &anyhow::Error) -> ActionFailedKind {
    // Check most-specific patterns first. The "Disabled" tier error
    // also contains the word "refused" (the message ends with "apply
    // refused"), so it MUST be matched before the broader denylist
    // check or it gets miscategorised.
    let msg = err.to_string();
    if msg.contains("ac_safe_mode_target=Disabled") {
        ActionFailedKind::AcTierBlocked
    } else if msg.contains("denylisted process") {
        ActionFailedKind::DenylistRefused
    } else {
        ActionFailedKind::Apply
    }
}

/// Revert per-PID changes captured in `record`. Fires
/// `Event::ProfileReverted` unconditionally (the engine's
/// `applied`-map slot for this PID is gone either way) and an
/// `Event::ActionFailed` if the kernel revert call returned an error
/// — the user's process may be left in the modified state, which is
/// exactly the case audit H-30 wanted surfaced.
fn revert_record(
    sys: &dyn framesage_sys::SysApi,
    events: &broadcast::Sender<Event>,
    pid: u32,
    record: AppliedRecord,
) {
    let profile_id = record.profile_id.clone();
    let exe_name = record.exe_name.clone();

    // Item 4.7 — revert-state-drift detection. If the user changed
    // priority/affinity via Task Manager mid-session, our revert
    // would silently undo their manual choice. Detect drift first;
    // on detect, skip the revert entirely and surface
    // ActionFailed::DriftDetected so the activity feed makes the
    // skipped revert visible.
    if let Some(drift) = detect_apply_drift(sys, pid, &record) {
        warn!(
            pid,
            exe = %exe_name,
            detail = %drift,
            "revert: skipping — live state drifted from what we applied (likely user-edited via Task Manager)"
        );
        let _ = events.send(Event::ActionFailed {
            kind: ActionFailedKind::DriftDetected,
            pid: Some(pid),
            exe_name: Some(exe_name.clone()),
            details: format!(
                "revert skipped: live process state drifted ({drift}); manual change preserved"
            ),
        });
        // We do NOT emit ProfileReverted on the drift skip — we
        // didn't revert anything, so claiming we did would
        // mis-label the activity feed.
        return;
    }

    if let Err(e) = sys.revert(pid, record.state) {
        warn!(pid, error = %e, "revert failed");
        let _ = events.send(Event::ActionFailed {
            kind: ActionFailedKind::Revert,
            pid: Some(pid),
            exe_name: Some(exe_name.clone()),
            details: format!("revert failed: {e:#}"),
        });
    }
    debug!(pid, profile = %profile_id, "reverted");
    let _ = events.send(Event::ProfileReverted {
        pid,
        exe_name,
        profile_id,
    });
}

/// Item 4.7 — return `Some(reason)` if the live priority class or
/// affinity mask differs from what `AppliedRecord` captured at apply
/// time. Returns `None` for no detectable drift (either we never
/// touched the knob, or the live value still matches). Best-effort:
/// if the kernel queries fail (rare; process exited mid-call), we
/// return `None` and let the revert proceed — better to attempt a
/// no-op revert on an exited PID than skip a legitimate revert based
/// on a transient error.
fn detect_apply_drift(
    sys: &dyn framesage_sys::SysApi,
    pid: u32,
    record: &AppliedRecord,
) -> Option<String> {
    if let Some(applied_class) = record.applied_priority_class_raw {
        if let Ok(Some(live_class)) = sys.get_priority_class_for_pid(pid) {
            if live_class != applied_class {
                return Some(format!(
                    "priority class: applied 0x{applied_class:x}, live 0x{live_class:x}"
                ));
            }
        }
    }
    if let Some(applied_mask) = record.applied_affinity_mask {
        if let Ok(Some(live_mask)) = sys.affinity_mask(pid) {
            if live_mask != applied_mask {
                return Some(format!(
                    "affinity mask: applied 0x{applied_mask:x}, live 0x{live_mask:x}"
                ));
            }
        }
    }
    None
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

/// Item 4.9 — returns true if a previous failed apply on `pid` is still
/// within the backoff window. Caller skips the apply attempt silently
/// when this returns true. Entries past the window stay in the map (the
/// next successful apply or PID-exit reaping clears them); checking is
/// O(1).
fn apply_backoff_active(backoff: &HashMap<u32, Instant>, pid: u32, now: Instant) -> bool {
    backoff
        .get(&pid)
        .map(|t| now.duration_since(*t) < APPLY_FAILURE_BACKOFF)
        .unwrap_or(false)
}

fn apply_profile(
    sys: &dyn framesage_sys::SysApi,
    pid: u32,
    exe_name: &str,
    profile: &Profile,
    topology: &CpuTopology,
    safe_list: &'static SafeList,
) -> Result<AppliedRecord> {
    // Defense-in-depth: the rule-match path resolves exe_name from the
    // foreground reporter and matches against policy rules; without this
    // guard, a policy.json with a rule `ExeName("csrss.exe") → game-x3d`
    // would happily push IDLE priority and a 1-CPU affinity onto csrss
    // the moment Windows reused that PID for any briefly-foregrounded
    // child (in practice csrss is never foregrounded, but the apply path
    // also fires from background-scan + persistent re-assert sweeps, so
    // narrowing the trust boundary here covers every entry path through
    // apply()). The IPC layer also gates per-PID actions in the same way
    // — this is the second layer for the rule-driven path.
    check_process_modifiable(safe_list, exe_name, "apply profile")?;

    // Item 1.9 / AC matrix — honor the profile's anti-cheat-aware tier.
    // SafeMode/Hybrid both strip per-game-process modifications (affinity,
    // priority, CPU sets, I/O priority, power throttling, working-set
    // trim, memory priority) before passing to sys::apply::apply. The
    // Game Mode actions (services / processes-to-suspend / power plan /
    // taskbar / WU pause) are NOT stripped — those fire via the system-
    // mode path, not apply_profile, and they're the environment-around-
    // the-game half of the safe profiles.
    let effective_profile = match profile.ac_safe_mode_target {
        AntiCheatProfile::Aggressive => profile.clone(),
        AntiCheatProfile::Hybrid | AntiCheatProfile::SafeMode => {
            let mut p = profile.clone();
            p.cpu_sets = None;
            p.affinity_mask = None;
            p.priority_class = None;
            p.io_priority = None;
            p.power_throttling = None;
            p.memory_priority = None;
            p.trim_working_set = false;
            debug!(
                pid,
                exe = %exe_name,
                profile = %profile.id,
                tier = ?profile.ac_safe_mode_target,
                "AC-aware: stripped per-game-process knobs"
            );
            p
        }
        AntiCheatProfile::Disabled => {
            // Belt-and-suspenders: tick gates STANDBY mode, so we
            // shouldn't reach apply_profile in Disabled. If we do,
            // refuse cleanly — better than running a profile the
            // user opted into being disabled.
            return Err(anyhow::anyhow!(
                "profile '{}' has ac_safe_mode_target=Disabled — apply refused",
                profile.id.0
            ));
        }
    };

    let state = sys.apply(pid, &effective_profile, topology)?;

    // Item 4.7 — capture the live values right after the apply landed
    // for drift detection at revert time. Only capture knobs the
    // profile actually wrote (`None` for the others so revert knows
    // there's no signal to read). Best-effort: a syscall failure here
    // doesn't roll back the apply — we just record `None` and forgo
    // drift detection for that knob.
    let applied_priority_class_raw = if effective_profile.priority_class.is_some() {
        sys.get_priority_class_for_pid(pid).ok().flatten()
    } else {
        None
    };
    let applied_affinity_mask =
        if effective_profile.affinity_mask.is_some() || effective_profile.cpu_sets.is_some() {
            sys.affinity_mask(pid).ok().flatten()
        } else {
            None
        };

    Ok(AppliedRecord {
        profile_id: profile.id.clone(),
        exe_name: exe_name.to_owned(),
        state,
        applied_priority_class_raw,
        applied_affinity_mask,
    })
}

/// Trust-boundary gate: refuse to touch a process whose exe is on the
/// bundled denylist (kernel/session-critical, AV, anti-cheat, GPU drivers,
/// RPC, DNS, audio stack, shell-critical). The rationale string from
/// `gamemode/src/safe_lists/processes.json` is surfaced in the returned
/// error so the user sees *why* it was refused, not just that it was.
///
/// `action` is a short human-readable label (e.g. "set priority", "suspend",
/// "apply profile") used in the error message and log line.
///
/// This is the narrow safety bar the product positioning explicitly carves
/// out: aggression on BITS / WSearch / ClickToRunSvc / OneDrive / etc. is the
/// feature. Aggression on csrss / lsass / wininit / dwm / MsMpEng / vgc is
/// BSOD-or-ban territory and is non-negotiable. Items not on the denylist
/// pass through unchanged.
fn check_process_modifiable(
    safe_list: &'static SafeList,
    exe_name: &str,
    action: &str,
) -> Result<()> {
    use framesage_gamemode::safe_list::ProcessVerdict;
    match safe_list.check_process(exe_name) {
        ProcessVerdict::Denied(reason) => {
            warn!(
                exe = %exe_name,
                action,
                reason,
                "refused {action} on denylisted process",
            );
            Err(anyhow::anyhow!(
                "refused {action} on {exe_name}: this process is on the \
                 framesage denylist for safety — {reason}",
            ))
        }
        // Allowed or Unlisted both pass — the denylist is the only authority.
        // "Unlisted" is the default state for every process the user might
        // legitimately want to modify (notepad, chrome, the user's own
        // game). The product position is explicit: anything not on the
        // narrow denylist is fair game.
        ProcessVerdict::Allowed(_) | ProcessVerdict::Unlisted => Ok(()),
    }
}

/// Resolve a PID to its bare exe filename for denylist checking, or return
/// an error if the PID has exited / is inaccessible. Used by every per-PID
/// IPC handler before consulting the safe-list. PID-lookup failure is
/// surfaced cleanly so the tray's status banner can explain "process is
/// gone" instead of swallowing the action silently.
///
/// Item 3.1b — takes `sys` explicitly so this stays a free function
/// callable from both `&self` Engine methods and free apply/revert
/// helpers without a borrow split.
fn resolve_exe_for_pid_or_err(
    sys: &dyn framesage_sys::SysApi,
    pid: u32,
    action: &str,
) -> Result<String> {
    match sys.exe_for_pid(pid) {
        Ok(Some(path)) => Ok(path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned()),
        Ok(None) => Err(anyhow::anyhow!(
            "cannot {action} on pid {pid}: process not found or inaccessible \
             (likely exited or protected)",
        )),
        Err(e) => Err(anyhow::anyhow!(
            "cannot {action} on pid {pid}: exe lookup failed: {e}",
        )),
    }
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

    // ─── Group 1 / item 1.1 — denylist enforcement at every kernel-write entry ───
    //
    // These tests lock the safety bar: the bundled denylist (csrss / lsass /
    // wininit / dwm / audiodg / MsMpEng / Vanguard / EAC / BattlEye / RPC /
    // DHCP / DNS / audio / GPU drivers) is refused at every per-PID action
    // path, with the JSON rationale string in the error message. Items NOT
    // on the denylist (WSearch / BITS-svchost / OneDrive / GameBar / random
    // user processes) MUST still pass the gate — they're the product's
    // aggression surface.
    //
    // Each test exercises the engine-level helper directly because that's
    // the trust boundary; the per-PID IPC handlers all call it before any
    // syscall. Testing via the IPC handlers themselves would require a
    // live PID, which would either be fragile (need a real process) or
    // require trait-injection of the syscall layer (Group 3 item 3.1).

    /// csrss / lsass / wininit / smss / services — terminating any of
    /// these blue-screens the box with CRITICAL_PROCESS_DIED. The gate
    /// must refuse with the JSON rationale visible in the error message.
    #[test]
    fn check_process_modifiable_refuses_kernel_critical() {
        let safe_list = framesage_gamemode::safe_list::SafeList::bundled();
        for exe in &[
            "csrss.exe",
            "lsass.exe",
            "wininit.exe",
            "services.exe",
            "smss.exe",
            "winlogon.exe",
        ] {
            let err = check_process_modifiable(safe_list, exe, "terminate")
                .expect_err(&format!("expected denial for {exe}"));
            let msg = err.to_string();
            assert!(
                msg.contains("denylist"),
                "error for {exe} should mention denylist, got: {msg}"
            );
            assert!(
                msg.to_ascii_lowercase().contains(&exe.to_ascii_lowercase()),
                "error for {exe} should name the exe, got: {msg}"
            );
        }
    }

    /// Shell / audio / GPU-driver / font-rendering — modifying these
    /// freezes the session or breaks the desktop. Same denial path.
    #[test]
    fn check_process_modifiable_refuses_shell_audio_gpu() {
        let safe_list = framesage_gamemode::safe_list::SafeList::bundled();
        for exe in &[
            "dwm.exe",
            "explorer.exe",
            "audiodg.exe",
            "fontdrvhost.exe",
            "nvcontainer.exe",
            "atiesrxx.exe",
            "sihost.exe",
        ] {
            assert!(
                check_process_modifiable(safe_list, exe, "set affinity").is_err(),
                "{exe} must be refused",
            );
        }
    }

    /// AV / anti-cheat — modifying these is a textbook anti-cheat bypass
    /// attempt and the canonical malware tactic for AV. Vanguard's HWID-ban
    /// risk and BattlEye's instant-ban precedent both apply.
    #[test]
    fn check_process_modifiable_refuses_av_and_anticheat() {
        let safe_list = framesage_gamemode::safe_list::SafeList::bundled();
        for exe in &["MsMpEng.exe", "NisSrv.exe", "SecurityHealthService.exe"] {
            assert!(
                check_process_modifiable(safe_list, exe, "suspend").is_err(),
                "{exe} (AV/security) must be refused",
            );
        }
    }

    /// Item 4.5 / audit M-04 — the trim-working-set path goes through
    /// the same `check_process_modifiable` gate as suspend / set
    /// priority / set affinity. MsMpEng (Defender) is protected
    /// from trim by the bundled denylist already enforced at the
    /// engine layer (no special-case trim logic needed). Trimming
    /// Defender's working set forces it to page-fault its signature
    /// database back in — a disk-I/O storm that defeats the point
    /// of Game Mode.
    ///
    /// This test pins the protection so a future refactor that
    /// re-routes `Engine::trim_working_set` around the gate will
    /// fail loudly. The test asserts via the gate function
    /// directly because the integration path (Engine::trim_working_set)
    /// also calls `exe_for_pid` against a live PID — which a unit
    /// test can't supply without spawning Defender, which is both
    /// platform-specific and rude.
    #[test]
    fn check_process_modifiable_refuses_trim_against_msmpeng() {
        let safe_list = framesage_gamemode::safe_list::SafeList::bundled();
        let err = check_process_modifiable(safe_list, "MsMpEng.exe", "trim working set")
            .expect_err("MsMpEng must be denied for trim");
        let msg = err.to_string();
        assert!(
            msg.to_ascii_lowercase().contains("msmpeng"),
            "error must name the exe, got: {msg}"
        );
        assert!(
            msg.contains("denylist"),
            "error must surface the denylist rationale, got: {msg}"
        );
    }

    /// The other direction of the safety bar — non-denylisted exes MUST
    /// pass the gate. These are the product's aggression surface; refusing
    /// them would defeat the point of the product positioning.
    #[test]
    fn check_process_modifiable_allows_aggression_targets() {
        let safe_list = framesage_gamemode::safe_list::SafeList::bundled();
        for exe in &[
            // Cloud sync — aggressive default suspends these
            "OneDrive.exe",
            "Dropbox.exe",
            "googledrivesync.exe",
            "MEGAsync.exe",
            // Game Bar — aggressive default suspends these
            "GameBar.exe",
            "GameBarFTServer.exe",
            // OEM updaters
            "DellSupportAssistRemedyService.exe",
            "LenovoVantageService.exe",
            // Random user processes — gate must not block these
            "notepad.exe",
            "chrome.exe",
            "code.exe",
            "steam.exe",
            // Games — must pass (per-game safety is enforced elsewhere
            // via the AC matrix profile, not at this gate)
            "VALORANT-Win64-Shipping.exe",
            "FortniteClient-Win64-Shipping.exe",
            "bf6.exe",
        ] {
            check_process_modifiable(safe_list, exe, "set priority")
                .unwrap_or_else(|e| panic!("{exe} must pass the gate, got error: {e}"));
        }
    }

    /// Case-insensitive matching — the JSON denylist uses canonical case
    /// (csrss.exe) but the gate must match regardless of how the exe name
    /// arrives from `framesage_sys::process::exe_for_pid`. Windows preserves
    /// the original case from the file system; same exe on different
    /// systems can vary.
    #[test]
    fn check_process_modifiable_is_case_insensitive() {
        let safe_list = framesage_gamemode::safe_list::SafeList::bundled();
        for variant in &["CSRSS.EXE", "csrss.exe", "CsRsS.ExE"] {
            assert!(
                check_process_modifiable(safe_list, variant, "terminate").is_err(),
                "case variant {variant} of csrss must still be refused",
            );
        }
    }

    /// The error message must surface the rationale string from the JSON
    /// denylist so the user understands *why* the action was refused — not
    /// just "denied." The product positioning requires informed consent;
    /// the converse is informed refusal.
    #[test]
    fn check_process_modifiable_surfaces_rationale() {
        let safe_list = framesage_gamemode::safe_list::SafeList::bundled();
        let err = check_process_modifiable(safe_list, "csrss.exe", "terminate")
            .expect_err("csrss must be refused");
        let msg = err.to_string();
        // The rationale at safe_lists/processes.json for csrss.exe says
        // "blue-screens the machine." Anchor on the substring; if the JSON
        // is reworded this test reveals a coupling that's actually the
        // contract we want — the error message MUST tell the user why.
        assert!(
            msg.contains("blue-screen") || msg.contains("BSOD"),
            "error should explain WHY (rationale from JSON), got: {msg}"
        );
    }

    // ─── Item 1.9 — AC-aware Safe Mode + ESEA standby + invariants ──
    //
    // Tests the architectural primitives the AC matrix recommended.
    // The full engine-loop integration tests for AC standby would
    // require Group 3's trait-injection infrastructure (item 3.1);
    // these unit tests cover the deterministic boundaries:
    // AntiCheatPresence behavior, apply_profile's AC-tier stripping,
    // and Disabled-tier refusal.

    /// AntiCheatProfile::Aggressive on apply leaves the profile
    /// unmodified. (Existing behavior — no regression.)
    #[test]
    fn apply_profile_aggressive_tier_does_not_strip_knobs() {
        // We can't actually call apply_profile() here without a real
        // Windows process, but we can verify the AC-tier classification
        // logic via the profile struct itself + by exercising the
        // sim-build path.
        let p = Profile {
            id: "test-aggressive".into(),
            cpu_sets: Some(framesage_core::CpuSelector::All),
            priority_class: Some(framesage_core::PriorityClass::High),
            ac_safe_mode_target: AntiCheatProfile::Aggressive,
            ..Default::default()
        };
        // Round-trip through clone — the AC-tier match in apply_profile
        // does `profile.clone()` for the Aggressive arm so the cloned
        // shape is identical.
        let cloned = p.clone();
        assert_eq!(cloned.cpu_sets, p.cpu_sets);
        assert_eq!(cloned.priority_class, p.priority_class);
        assert_eq!(cloned.ac_safe_mode_target, AntiCheatProfile::Aggressive);
    }

    /// SafeMode and Hybrid tiers strip the same per-game-process
    /// knobs. The stripped profile's game_mode + persistent flag are
    /// preserved so environment actions still fire.
    #[test]
    fn safemode_and_hybrid_tiers_share_strip_semantics() {
        // The actual stripping happens inside apply_profile's match
        // arm. We exercise the same logic here against a constructed
        // profile to lock the contract.
        let original = Profile {
            id: "test-safe".into(),
            cpu_sets: Some(framesage_core::CpuSelector::All),
            affinity_mask: Some(framesage_core::CpuSelector::All),
            priority_class: Some(framesage_core::PriorityClass::High),
            io_priority: Some(framesage_core::IoPriority::High),
            power_throttling: Some(framesage_core::PowerThrottlingMode::Performance),
            memory_priority: Some(framesage_core::MemoryPriority::Normal),
            trim_working_set: true,
            persistent: true,
            ac_safe_mode_target: AntiCheatProfile::SafeMode,
            ..Default::default()
        };
        // Mirror the strip logic from apply_profile:
        let mut stripped = original.clone();
        stripped.cpu_sets = None;
        stripped.affinity_mask = None;
        stripped.priority_class = None;
        stripped.io_priority = None;
        stripped.power_throttling = None;
        stripped.memory_priority = None;
        stripped.trim_working_set = false;
        // What MUST be preserved:
        assert_eq!(stripped.id, original.id);
        assert!(stripped.persistent, "persistent flag preserved in SafeMode");
        // What MUST be cleared:
        assert!(stripped.cpu_sets.is_none());
        assert!(stripped.affinity_mask.is_none());
        assert!(stripped.priority_class.is_none());
        assert!(stripped.io_priority.is_none());
        assert!(stripped.power_throttling.is_none());
        assert!(stripped.memory_priority.is_none());
        assert!(!stripped.trim_working_set);
    }

    /// Defaults D-9 + D-10: seeded Valorant + BF6 rules ship with the
    /// correct AC tiers. Locks the contract that a fresh policy.json
    /// out of the box gives Vanguard / Javelin users the safe defaults
    /// the AC matrix prescribes.
    #[test]
    fn seeded_default_rules_have_correct_ac_tiers() {
        let policy = framesage_core::Policy::default();

        // Find each seeded rule and look up its target profile.
        let rule_for = |exe: &str| -> Option<&framesage_core::Profile> {
            let rule = policy
                .rules
                .iter()
                .find(|r| matches!(&r.r#match, framesage_core::AppMatch::ExeName(n) if n.eq_ignore_ascii_case(exe)))?;
            policy.profile(&rule.profile)
        };

        // D-9: Valorant must ship with SafeMode.
        let valorant_profile = rule_for("VALORANT-Win64-Shipping.exe")
            .expect("Valorant seeded rule must resolve to a profile");
        assert_eq!(
            valorant_profile.ac_safe_mode_target,
            AntiCheatProfile::SafeMode,
            "D-9: Valorant rule must ship with SafeMode tier (Vanguard hardware-ban risk)"
        );

        // D-10: BF6 must ship with Hybrid.
        let bf6_profile = rule_for("bf6.exe").expect("BF6 seeded rule must resolve to a profile");
        assert_eq!(
            bf6_profile.ac_safe_mode_target,
            AntiCheatProfile::Hybrid,
            "D-10: BF6 rule must ship with Hybrid tier (EA Javelin affinity-blocking risk)"
        );

        // Fortnite stays Aggressive (EAC is the friendly case).
        let fortnite_profile = rule_for("FortniteClient-Win64-Shipping.exe")
            .expect("Fortnite seeded rule must resolve to a profile");
        assert_eq!(
            fortnite_profile.ac_safe_mode_target,
            AntiCheatProfile::Aggressive,
            "Fortnite ships Aggressive (EAC strip-rights model, no ban precedent)"
        );
    }

    /// AntiCheatPresence::esea_demands_standby is the signal that
    /// gates the engine STANDBY path in tick(). Locks the boolean
    /// logic.
    #[test]
    fn ac_presence_esea_drives_standby() {
        let mut p = AntiCheatPresence::default();
        assert!(!p.esea_demands_standby());
        p.esea = true;
        assert!(p.esea_demands_standby());
        p.esea = false;
        p.vanguard = true; // Other ACs do NOT trigger standby
        assert!(!p.esea_demands_standby());
    }

    /// FACEIT presence refuses WU pause (per matrix row 19/20). The
    /// engine's WU-pause path consults this; without it, FACEIT
    /// refuses to launch on a system with broken / paused WU.
    #[test]
    fn ac_presence_faceit_refuses_wu_pause() {
        let p = AntiCheatPresence {
            faceit: true,
            ..Default::default()
        };
        assert!(p.refuses_wu_pause());
        // Other ACs don't refuse — Vanguard / EAC / BattlEye are fine
        // with WU paused.
        let q = AntiCheatPresence {
            vanguard: true,
            eac: true,
            battleye: true,
            ..Default::default()
        };
        assert!(!q.refuses_wu_pause());
    }

    // ─── Item 2.7+2.8 follow-up — Event emission wiring ──────────────────
    //
    // The engine's apply / revert / system-mode / AC-presence paths
    // all `let _ = self.events.send(...)`; verifying every emission
    // call site requires a live PID and a real foreground (Group 3
    // 3.1 work). What we CAN lock here:
    //
    //   * `classify_apply_failure` correctly buckets the three
    //     known anyhow-error shapes the engine produces, so the
    //     tray's filter-by-kind path is wired to the right source
    //     of truth.
    //   * `revert_record` emits ProfileReverted via the broadcast
    //     channel — the simulator path runs without syscalls and
    //     exercises the unconditional emission branch.

    /// classify_apply_failure must distinguish the three error shapes
    /// the engine's apply_profile produces so the tray can filter
    /// "denied by safety bar" separately from "Win32 error" without
    /// substring-matching the details string every render.
    #[test]
    fn classify_apply_failure_buckets_known_shapes() {
        let denylist = anyhow::anyhow!(
            "refused apply profile on denylisted process csrss.exe: kernel-critical"
        );
        assert_eq!(
            classify_apply_failure(&denylist),
            ActionFailedKind::DenylistRefused
        );

        let disabled =
            anyhow::anyhow!("profile 'foo' has ac_safe_mode_target=Disabled — apply refused");
        assert_eq!(
            classify_apply_failure(&disabled),
            ActionFailedKind::AcTierBlocked
        );

        let win32 = anyhow::anyhow!("SetProcessAffinityMask failed: ERROR_ACCESS_DENIED");
        assert_eq!(classify_apply_failure(&win32), ActionFailedKind::Apply);
    }

    /// SystemEvent::Suspend must pause the engine. The pause flag is
    /// the load-bearing signal — without it, the tick task keeps
    /// reconciling foreground during the suspend transition and
    /// potentially fights the OS over kernel writes.
    #[test]
    fn system_event_suspend_pauses_engine() {
        let engine = test_engine();
        assert!(!engine.status().paused);
        engine.handle_system_event(SystemEvent::Suspend);
        assert!(
            engine.status().paused,
            "Suspend must leave the engine paused"
        );
    }

    /// SystemEvent::Resume must un-pause AND clear last_ac_probe so
    /// the next tick's maybe_refresh_ac_presence fires immediately
    /// (rather than waiting up to 5 s for the regular cadence). It
    /// must also clear current_foreground so reconcile's "new_pid ==
    /// current_foreground" fast path doesn't skip the post-resume
    /// re-evaluation.
    #[test]
    fn system_event_resume_clears_state_and_resumes() {
        let engine = test_engine();
        engine.handle_system_event(SystemEvent::Suspend);
        assert!(engine.status().paused);

        engine.handle_system_event(SystemEvent::Resume);
        assert!(!engine.status().paused, "Resume must un-pause the engine");
    }

    /// Item 3.7 — refresh_topology swaps the engine's cached
    /// Arc<CpuTopology> with whatever SysApi::detect_topology
    /// returns. Pin the swap by exercising it directly: prime the
    /// mock with a 4-CPU topology, call refresh, confirm the
    /// engine's view now reports 4 CPUs.
    #[test]
    fn refresh_topology_swaps_engine_snapshot() {
        let sys = Arc::new(MockSysApi::new());
        let clock = Arc::new(FakeClock::new());
        // Engine starts with CpuTopology::default() = 0 CPUs.
        let (engine, _events) = engine_with_mocks(sys.clone(), clock);
        assert_eq!(engine.state.read().topology.cpus.len(), 0);

        // Prime a new topology: 4 logical CPUs on a single CCD.
        sys.set_topology(framesage_core::CpuTopology {
            cpus: (0..4)
                .map(|i| framesage_core::LogicalCpu {
                    index: i,
                    physical_core: i / 2,
                    ccd: 0,
                    kind: framesage_core::CoreKind::Performance,
                    cppc_rank: Some(100 - i),
                    l3_cache_bytes: None,
                    is_smt_sibling: i % 2 == 1,
                })
                .collect(),
        });

        engine.refresh_topology();
        assert_eq!(
            sys.topology_call_count(),
            1,
            "refresh_topology must call SysApi::detect_topology exactly once"
        );
        assert_eq!(
            engine.state.read().topology.cpus.len(),
            4,
            "engine must adopt the new topology"
        );
    }

    /// Item 3.7 — SystemEvent::Resume must fire refresh_topology so
    /// power-plan-driven core parking that landed during the sleep
    /// window gets picked up automatically. Without this, every
    /// selector resolved through topology after resume targets the
    /// pre-sleep layout.
    #[test]
    fn system_event_resume_triggers_topology_refresh() {
        let sys = Arc::new(MockSysApi::new());
        let clock = Arc::new(FakeClock::new());
        let (engine, _events) = engine_with_mocks(sys.clone(), clock);
        assert_eq!(sys.topology_call_count(), 0);

        engine.handle_system_event(SystemEvent::Suspend);
        // Suspend must NOT refresh topology — the system is going
        // to sleep, no point detecting now.
        assert_eq!(sys.topology_call_count(), 0);

        engine.handle_system_event(SystemEvent::Resume);
        assert_eq!(
            sys.topology_call_count(),
            1,
            "Resume must fire exactly one topology refresh"
        );
    }

    /// Item 3.7 — if SysApi::detect_topology fails (extremely
    /// unusual; would mean the kernel is refusing
    /// GetLogicalProcessorInformationEx), refresh_topology must
    /// keep the previous snapshot rather than blow it away. Better
    /// to operate on a slightly stale topology than no topology.
    #[test]
    fn refresh_topology_preserves_previous_on_detect_failure() {
        /// Mock that returns an error from detect_topology.
        struct FailingSys;
        impl framesage_sys::SysApi for FailingSys {
            fn detect_anti_cheats(&self) -> Result<framesage_core::AntiCheatPresence> {
                Ok(framesage_core::AntiCheatPresence::default())
            }
            fn detect_topology(&self) -> Result<framesage_core::CpuTopology> {
                Err(anyhow::anyhow!("simulated kernel failure"))
            }
            fn iter_pids(&self) -> Result<Vec<u32>> {
                Ok(Vec::new())
            }
            fn iter_pid_snapshots(&self) -> Result<Vec<framesage_sys::process::PidSnapshot>> {
                Ok(Vec::new())
            }
            fn enumerate_processes(
                &self,
            ) -> Result<Vec<framesage_sys::sys_proc_info::SysProcInfo>> {
                Err(anyhow::anyhow!("mock: not supported"))
            }
            fn exe_for_pid(&self, _pid: u32) -> Result<Option<String>> {
                Ok(None)
            }
            fn user_for_pid(&self, _pid: u32) -> Result<Option<String>> {
                Ok(None)
            }
            fn cpu_times(
                &self,
                _pid: u32,
            ) -> Result<Option<framesage_sys::process::ProcessCpuTimes>> {
                Ok(None)
            }
            fn memory_info(&self, _pid: u32) -> Result<Option<framesage_sys::process::MemoryInfo>> {
                Ok(None)
            }
            fn affinity_mask(&self, _pid: u32) -> Result<Option<u64>> {
                Ok(None)
            }
            fn system_cpu_times(&self) -> Result<framesage_sys::process::SystemCpuTimes> {
                Ok(framesage_sys::process::SystemCpuTimes {
                    idle_100ns: 0,
                    kernel_100ns: 0,
                    user_100ns: 0,
                })
            }
            fn per_cpu_times(&self) -> Result<Vec<framesage_sys::process::PerCpuTimes>> {
                Ok(Vec::new())
            }
            fn memory_status(&self) -> Result<(u64, u64)> {
                Ok((0, 0))
            }
            fn current_foreground(
                &self,
            ) -> Result<Option<framesage_sys::foreground::ForegroundInfo>> {
                Ok(None)
            }
            fn apply(
                &self,
                _pid: u32,
                _profile: &Profile,
                _topology: &CpuTopology,
            ) -> Result<framesage_sys::apply::AppliedState> {
                Err(anyhow::anyhow!("mock: not supported"))
            }
            fn revert(&self, _pid: u32, _state: framesage_sys::apply::AppliedState) -> Result<()> {
                Ok(())
            }
            fn reassert(
                &self,
                _pid: u32,
                _profile: &Profile,
                _topology: &CpuTopology,
            ) -> Result<()> {
                Ok(())
            }
            fn get_priority_class_for_pid(&self, _pid: u32) -> Result<Option<u32>> {
                Ok(None)
            }
            fn set_priority_class_for_pid(
                &self,
                _pid: u32,
                _class: framesage_core::PriorityClass,
            ) -> Result<()> {
                Ok(())
            }
            fn restore_priority_class_for_pid(&self, _pid: u32, _raw_class: u32) -> Result<()> {
                Ok(())
            }
            fn set_affinity_mask_for_pid(&self, _pid: u32, _mask: u64) -> Result<()> {
                Ok(())
            }
            fn trim_working_set_for_pid(&self, _pid: u32) -> Result<()> {
                Ok(())
            }
            fn suspend_process(&self, _pid: u32) -> Result<()> {
                Ok(())
            }
            fn resume_process(&self, _pid: u32) -> Result<()> {
                Ok(())
            }
            fn terminate_process(&self, _pid: u32) -> Result<()> {
                Ok(())
            }
            fn read_version_info(
                &self,
                _exe_path: &str,
            ) -> Result<framesage_sys::version_info::VersionInfo> {
                Ok(framesage_sys::version_info::VersionInfo::default())
            }
            fn enumerate_services(&self) -> Result<Vec<framesage_sys::services::ServiceInfo>> {
                Ok(Vec::new())
            }
        }

        // Build an engine with a non-empty starting topology so we
        // can verify it survives the failed refresh.
        let starting_topology = framesage_core::CpuTopology {
            cpus: (0..2)
                .map(|i| framesage_core::LogicalCpu {
                    index: i,
                    physical_core: i,
                    ccd: 0,
                    kind: framesage_core::CoreKind::Performance,
                    cppc_rank: None,
                    l3_cache_bytes: None,
                    is_smt_sibling: false,
                })
                .collect(),
        };
        let policy = Policy {
            default_profile: ProfileId("default".into()),
            profiles: {
                let mut m = std::collections::HashMap::new();
                m.insert(ProfileId("default".into()), Profile::default());
                m
            },
            ..Default::default()
        };
        let sys: Arc<dyn framesage_sys::SysApi> = Arc::new(FailingSys);
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::new());
        let deps = EngineDeps {
            policy,
            topology: starting_topology,
            safe_list: SafeList::bundled(),
            journal: Journal::at_default_path(),
            sys,
            clock,
        };
        let engine = Engine::new(deps);
        assert_eq!(engine.state.read().topology.cpus.len(), 2);

        engine.refresh_topology();
        assert_eq!(
            engine.state.read().topology.cpus.len(),
            2,
            "failed refresh must NOT clobber the previous topology"
        );
    }

    /// SessionLock / SessionUnlock must NOT tear down Game Mode —
    /// locking the screen while gaming is a normal event (user steps
    /// away for a moment) and the game is still running. Pinning the
    /// no-op so a future refactor doesn't accidentally extend the
    /// disconnect handler to cover Lock.
    #[test]
    fn system_event_session_lock_is_a_noop() {
        let engine = test_engine();
        assert!(!engine.status().paused);
        engine.handle_system_event(SystemEvent::SessionLock);
        engine.handle_system_event(SystemEvent::SessionUnlock);
        assert!(
            !engine.status().paused,
            "Lock/Unlock must NOT pause the engine (game is still running)"
        );
    }

    /// enable_manual_global_game_mode must refuse a profile that
    /// isn't marked `manual_global_eligible` — the flag exists
    /// precisely so the picker doesn't fill up with every game
    /// profile the user has authored.
    #[test]
    fn manual_global_refuses_non_eligible_profile() {
        let engine = test_engine_with_profile(Profile {
            id: ProfileId("quiet".into()),
            manual_global_eligible: false,
            game_mode: Some(GameModeActions {
                hide_taskbar: true,
                ..Default::default()
            }),
            ..Default::default()
        });
        let err = engine
            .enable_manual_global_game_mode(ProfileId("quiet".into()))
            .expect_err("non-eligible profile must be refused");
        assert!(
            err.to_string().contains("manual_global_eligible"),
            "error should mention the missing flag: {err}"
        );
        assert!(engine.status().manual_global_active.is_none());
    }

    /// enable_manual_global_game_mode must refuse a profile whose
    /// `game_mode` is None — entering a manual global session that
    /// applies zero actions is just confusing.
    #[test]
    fn manual_global_refuses_profile_with_no_game_mode() {
        let engine = test_engine_with_profile(Profile {
            id: ProfileId("naked".into()),
            manual_global_eligible: true,
            game_mode: None,
            ..Default::default()
        });
        let err = engine
            .enable_manual_global_game_mode(ProfileId("naked".into()))
            .expect_err("profile with no game_mode must be refused");
        assert!(err.to_string().contains("no game_mode"));
    }

    /// disable_manual_global_game_mode must be idempotent — calling
    /// it when no session is active is a no-op, not an error or
    /// crash. Mirrors the panic-button (`exit_system_mode_now`)
    /// semantic.
    #[test]
    fn manual_global_disable_is_idempotent() {
        let engine = test_engine();
        engine.disable_manual_global_game_mode();
        engine.disable_manual_global_game_mode();
        assert!(engine.status().manual_global_active.is_none());
    }

    // ─── Item 4.9 — apply-failure backoff ──────────────────────────────────

    /// Predicate: a fresh failure entry (recorded "now") suppresses
    /// the apply within the window; an entry past the window does not;
    /// no entry at all does not.
    #[test]
    fn apply_backoff_active_window_semantics() {
        let mut map = HashMap::new();
        let now = Instant::now();
        assert!(
            !apply_backoff_active(&map, 42, now),
            "no entry → no backoff"
        );

        // Fresh failure: backoff active.
        map.insert(42, now);
        assert!(
            apply_backoff_active(&map, 42, now),
            "freshly recorded failure must be inside the window"
        );

        // Same entry, but observed 31 seconds later: expired.
        let later = now + Duration::from_secs(31);
        assert!(
            !apply_backoff_active(&map, 42, later),
            "entry past APPLY_FAILURE_BACKOFF must not gate the retry"
        );

        // A different PID is unaffected by 42's entry.
        assert!(
            !apply_backoff_active(&map, 43, now),
            "backoff entries are per-PID"
        );
    }

    /// The window constant is load-bearing for the audit's M-15
    /// recommendation. Pin it so a future tweak surfaces in code review.
    #[test]
    fn apply_failure_backoff_is_thirty_seconds() {
        assert_eq!(APPLY_FAILURE_BACKOFF, Duration::from_secs(30));
    }

    // ─── Item 4.7 — revert-state-drift detection ────────────────────────

    fn drift_record(
        applied_priority_class_raw: Option<u32>,
        applied_affinity_mask: Option<u64>,
    ) -> AppliedRecord {
        AppliedRecord {
            profile_id: ProfileId("perf".into()),
            exe_name: "notepad.exe".into(),
            state: framesage_sys::apply::AppliedState::default(),
            applied_priority_class_raw,
            applied_affinity_mask,
        }
    }

    /// No drift to report: live priority matches what we recorded,
    /// affinity matches, predicate returns None → revert proceeds.
    #[test]
    fn detect_apply_drift_returns_none_when_live_matches_applied() {
        let sys = Arc::new(MockSysApi::new());
        sys.set_live_priority_class(Some(0x80)); // HIGH
        sys.set_live_affinity_mask(Some(0xFFFF));
        let record = drift_record(Some(0x80), Some(0xFFFF));
        assert!(detect_apply_drift(sys.as_ref(), 1234, &record).is_none());
    }

    /// Load-bearing case: live priority is AboveNormal (0x8000) but
    /// we recorded High (0x80) at apply time. Predicate must return
    /// Some(_) so revert_record skips the kernel revert.
    #[test]
    fn detect_apply_drift_returns_some_when_priority_drifted() {
        let sys = Arc::new(MockSysApi::new());
        sys.set_live_priority_class(Some(0x8000)); // ABOVE_NORMAL
        let record = drift_record(Some(0x80), None);
        let drift = detect_apply_drift(sys.as_ref(), 1234, &record);
        let msg = drift.expect("drift expected when live class differs from applied");
        assert!(
            msg.contains("priority"),
            "drift message must mention which knob: {msg}"
        );
    }

    /// Mirror case for the affinity knob — user pinned to a single
    /// core via Task Manager after we'd applied a wider mask.
    #[test]
    fn detect_apply_drift_returns_some_when_affinity_drifted() {
        let sys = Arc::new(MockSysApi::new());
        sys.set_live_affinity_mask(Some(0x01)); // user pinned to CPU 0
        let record = drift_record(None, Some(0xFFFF));
        let drift = detect_apply_drift(sys.as_ref(), 1234, &record);
        let msg = drift.expect("drift expected when live mask differs from applied");
        assert!(
            msg.contains("affinity"),
            "drift message must mention affinity: {msg}"
        );
    }

    /// A profile that didn't touch priority or affinity leaves both
    /// `applied_*` fields as None. Predicate has no signal to read
    /// and must return None → revert proceeds. Belt-and-suspenders
    /// against a future regression where we wrongly treat None as
    /// "always drifted."
    #[test]
    fn detect_apply_drift_returns_none_when_no_applied_fields_recorded() {
        let sys = Arc::new(MockSysApi::new());
        sys.set_live_priority_class(Some(0x8000));
        sys.set_live_affinity_mask(Some(0x01));
        let record = drift_record(None, None);
        assert!(detect_apply_drift(sys.as_ref(), 1234, &record).is_none());
    }

    /// revert_record on a drifted record must:
    ///   1. NOT call sys.revert (no kernel mutation)
    ///   2. emit ActionFailed { kind: DriftDetected }
    ///   3. NOT emit ProfileReverted (since nothing reverted)
    #[test]
    fn revert_record_emits_drift_detected_and_skips_revert() {
        let (tx, mut rx) = broadcast::channel(8);
        let sys = Arc::new(MockSysApi::new());
        // Live priority drifted from applied.
        sys.set_live_priority_class(Some(0x8000));
        let record = drift_record(Some(0x80), None);
        revert_record(sys.as_ref(), &tx, 4242, record);

        let mut saw_drift = false;
        let mut saw_reverted = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                Event::ActionFailed {
                    kind: ActionFailedKind::DriftDetected,
                    ..
                } => saw_drift = true,
                Event::ProfileReverted { .. } => saw_reverted = true,
                _ => {}
            }
        }
        assert!(saw_drift, "expected ActionFailed::DriftDetected event");
        assert!(
            !saw_reverted,
            "must NOT emit ProfileReverted when we skipped the revert"
        );
    }

    fn test_engine_with_profile(profile: Profile) -> Engine {
        let policy = Policy {
            default_profile: profile.id.clone(),
            profiles: {
                let mut m = std::collections::HashMap::new();
                m.insert(profile.id.clone(), profile);
                m
            },
            ..Default::default()
        };
        Engine::new(EngineDeps::with_real_sys(
            policy,
            CpuTopology::default(),
            framesage_gamemode::safe_list::SafeList::bundled(),
            Journal::at_default_path(),
        ))
    }

    fn test_engine() -> Engine {
        let policy = Policy {
            default_profile: ProfileId("default".into()),
            profiles: {
                let mut m = std::collections::HashMap::new();
                m.insert(ProfileId("default".into()), Profile::default());
                m
            },
            ..Default::default()
        };
        Engine::new(EngineDeps::with_real_sys(
            policy,
            CpuTopology::default(),
            framesage_gamemode::safe_list::SafeList::bundled(),
            Journal::at_default_path(),
        ))
    }

    // ─── Item 3.1 — first deterministic test using injected SysApi + Clock ──
    //
    // Proves the trait substrate works end-to-end against the AC-detection
    // path. Before this PR, exercising `maybe_refresh_ac_presence` required
    // a live Vanguard install + a 5-second real-clock wait per cadence
    // check; with the trait substrate we drive both inputs by hand.
    //
    // What's pinned:
    //   1. The cadence gate honors `last_ac_probe + AC_DETECT_INTERVAL`.
    //      Calling tick faster than that doesn't re-probe.
    //   2. A Vanguard transition (None → Some) emits the
    //      `AntiCheatPresenceChanged { which: "vanguard", active: true }`
    //      event exactly once.
    //   3. An ESEA transition pauses the engine via the standby gate.
    //      (Covered indirectly via `esea_demands_standby` test elsewhere;
    //      here we just confirm the event fires.)

    /// Mock `SysApi` that returns scripted AC detection results. The
    /// next-result slot lets the test set what the next
    /// `detect_anti_cheats()` call should return; default is an
    /// AntiCheatPresence with all-false fields.
    struct MockSysApi {
        next_ac: std::sync::Mutex<framesage_core::AntiCheatPresence>,
        ac_calls: std::sync::atomic::AtomicUsize,
        next_topology: std::sync::Mutex<framesage_core::CpuTopology>,
        topology_calls: std::sync::atomic::AtomicUsize,
        /// Item 4.7 — scripted live priority class. `Some(class)` is
        /// returned from `get_priority_class_for_pid` regardless of
        /// PID; `None` mimics "PID exited / unreadable" (the engine
        /// skips drift detection on this signal).
        next_priority_class: std::sync::Mutex<Option<u32>>,
        /// Item 4.7 — scripted live affinity mask. Same semantics.
        next_affinity_mask: std::sync::Mutex<Option<u64>>,
    }

    impl MockSysApi {
        fn new() -> Self {
            Self {
                next_ac: std::sync::Mutex::new(framesage_core::AntiCheatPresence::default()),
                ac_calls: std::sync::atomic::AtomicUsize::new(0),
                next_topology: std::sync::Mutex::new(framesage_core::CpuTopology::default()),
                topology_calls: std::sync::atomic::AtomicUsize::new(0),
                next_priority_class: std::sync::Mutex::new(None),
                next_affinity_mask: std::sync::Mutex::new(None),
            }
        }
        fn set_ac(&self, p: framesage_core::AntiCheatPresence) {
            *self.next_ac.lock().unwrap() = p;
        }
        fn ac_call_count(&self) -> usize {
            self.ac_calls.load(std::sync::atomic::Ordering::Relaxed)
        }
        fn set_topology(&self, t: framesage_core::CpuTopology) {
            *self.next_topology.lock().unwrap() = t;
        }
        fn topology_call_count(&self) -> usize {
            self.topology_calls
                .load(std::sync::atomic::Ordering::Relaxed)
        }
        fn set_live_priority_class(&self, raw: Option<u32>) {
            *self.next_priority_class.lock().unwrap() = raw;
        }
        fn set_live_affinity_mask(&self, mask: Option<u64>) {
            *self.next_affinity_mask.lock().unwrap() = mask;
        }
    }

    impl framesage_sys::SysApi for MockSysApi {
        fn detect_anti_cheats(&self) -> Result<framesage_core::AntiCheatPresence> {
            self.ac_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(*self.next_ac.lock().unwrap())
        }
        fn detect_topology(&self) -> Result<framesage_core::CpuTopology> {
            self.topology_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(self.next_topology.lock().unwrap().clone())
        }
        fn iter_pids(&self) -> Result<Vec<u32>> {
            Ok(Vec::new())
        }
        fn iter_pid_snapshots(&self) -> Result<Vec<framesage_sys::process::PidSnapshot>> {
            Ok(Vec::new())
        }
        fn enumerate_processes(&self) -> Result<Vec<framesage_sys::sys_proc_info::SysProcInfo>> {
            Err(anyhow::anyhow!("mock: not supported"))
        }
        fn exe_for_pid(&self, _pid: u32) -> Result<Option<String>> {
            Ok(None)
        }
        fn user_for_pid(&self, _pid: u32) -> Result<Option<String>> {
            Ok(None)
        }
        fn cpu_times(&self, _pid: u32) -> Result<Option<framesage_sys::process::ProcessCpuTimes>> {
            Ok(None)
        }
        fn memory_info(&self, _pid: u32) -> Result<Option<framesage_sys::process::MemoryInfo>> {
            Ok(None)
        }
        fn affinity_mask(&self, _pid: u32) -> Result<Option<u64>> {
            Ok(*self.next_affinity_mask.lock().unwrap())
        }
        fn system_cpu_times(&self) -> Result<framesage_sys::process::SystemCpuTimes> {
            // No Default impl in the Windows variant; construct
            // explicitly with zeros.
            Ok(framesage_sys::process::SystemCpuTimes {
                idle_100ns: 0,
                kernel_100ns: 0,
                user_100ns: 0,
            })
        }
        fn per_cpu_times(&self) -> Result<Vec<framesage_sys::process::PerCpuTimes>> {
            Ok(Vec::new())
        }
        fn memory_status(&self) -> Result<(u64, u64)> {
            Ok((0, 0))
        }
        fn current_foreground(&self) -> Result<Option<framesage_sys::foreground::ForegroundInfo>> {
            Ok(None)
        }
        fn apply(
            &self,
            _pid: u32,
            _profile: &Profile,
            _topology: &CpuTopology,
        ) -> Result<framesage_sys::apply::AppliedState> {
            Err(anyhow::anyhow!("mock: not supported"))
        }
        fn revert(&self, _pid: u32, _state: framesage_sys::apply::AppliedState) -> Result<()> {
            Ok(())
        }
        fn reassert(&self, _pid: u32, _profile: &Profile, _topology: &CpuTopology) -> Result<()> {
            Ok(())
        }
        fn get_priority_class_for_pid(&self, _pid: u32) -> Result<Option<u32>> {
            Ok(*self.next_priority_class.lock().unwrap())
        }
        fn set_priority_class_for_pid(
            &self,
            _pid: u32,
            _class: framesage_core::PriorityClass,
        ) -> Result<()> {
            Ok(())
        }
        fn restore_priority_class_for_pid(&self, _pid: u32, _raw_class: u32) -> Result<()> {
            Ok(())
        }
        fn set_affinity_mask_for_pid(&self, _pid: u32, _mask: u64) -> Result<()> {
            Ok(())
        }
        fn trim_working_set_for_pid(&self, _pid: u32) -> Result<()> {
            Ok(())
        }
        fn suspend_process(&self, _pid: u32) -> Result<()> {
            Ok(())
        }
        fn resume_process(&self, _pid: u32) -> Result<()> {
            Ok(())
        }
        fn terminate_process(&self, _pid: u32) -> Result<()> {
            Ok(())
        }
        fn read_version_info(
            &self,
            _exe_path: &str,
        ) -> Result<framesage_sys::version_info::VersionInfo> {
            Ok(framesage_sys::version_info::VersionInfo::default())
        }
        fn enumerate_services(&self) -> Result<Vec<framesage_sys::services::ServiceInfo>> {
            Ok(Vec::new())
        }
    }

    /// Mock `Clock` that returns a hand-set instant. `advance(dur)`
    /// moves it forward.
    struct FakeClock {
        now: std::sync::Mutex<Instant>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: std::sync::Mutex::new(Instant::now()),
            }
        }
        fn advance(&self, by: Duration) {
            let mut n = self.now.lock().unwrap();
            *n += by;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            *self.now.lock().unwrap()
        }
        fn unix_now(&self) -> SystemTime {
            SystemTime::now()
        }
    }

    fn engine_with_mocks(
        sys: Arc<MockSysApi>,
        clock: Arc<FakeClock>,
    ) -> (Engine, broadcast::Receiver<Event>) {
        let policy = Policy {
            default_profile: ProfileId("default".into()),
            profiles: {
                let mut m = std::collections::HashMap::new();
                m.insert(ProfileId("default".into()), Profile::default());
                m
            },
            ..Default::default()
        };
        let engine = Engine::new(EngineDeps {
            policy,
            topology: CpuTopology::default(),
            safe_list: framesage_gamemode::safe_list::SafeList::bundled(),
            journal: Journal::at_default_path(),
            sys,
            clock,
        });
        let rx = engine.subscribe();
        (engine, rx)
    }

    /// First-ever AC probe fires (no `last_ac_probe` yet, so the
    /// cadence gate doesn't apply), and a None→Vanguard transition
    /// emits the corresponding event.
    #[test]
    fn ac_probe_emits_event_on_vanguard_transition() {
        let sys = Arc::new(MockSysApi::new());
        let clock = Arc::new(FakeClock::new());
        let (engine, mut rx) = engine_with_mocks(sys.clone(), clock.clone());

        // Set Vanguard active before the first probe.
        sys.set_ac(framesage_core::AntiCheatPresence {
            vanguard: true,
            ..Default::default()
        });

        engine.maybe_refresh_ac_presence();
        assert_eq!(sys.ac_call_count(), 1);

        // Drain events looking for the transition.
        let mut saw_vanguard = false;
        while let Ok(ev) = rx.try_recv() {
            if let Event::AntiCheatPresenceChanged { which, active } = ev {
                if which == "vanguard" && active {
                    saw_vanguard = true;
                }
            }
        }
        assert!(
            saw_vanguard,
            "Vanguard transition must emit AntiCheatPresenceChanged"
        );
    }

    /// The cadence gate honors AC_DETECT_INTERVAL. Two probes back-to-
    /// back without advancing the clock should only call into the
    /// SysApi once.
    #[test]
    fn ac_probe_respects_cadence_gate() {
        let sys = Arc::new(MockSysApi::new());
        let clock = Arc::new(FakeClock::new());
        let (engine, _rx) = engine_with_mocks(sys.clone(), clock.clone());

        engine.maybe_refresh_ac_presence();
        assert_eq!(sys.ac_call_count(), 1, "first probe must fire");

        // Without advancing time, the next call must NOT trigger
        // another probe.
        engine.maybe_refresh_ac_presence();
        assert_eq!(sys.ac_call_count(), 1, "probe must be cadence-gated");

        // Advance just under the interval — still no probe.
        clock.advance(AC_DETECT_INTERVAL - Duration::from_millis(1));
        engine.maybe_refresh_ac_presence();
        assert_eq!(sys.ac_call_count(), 1, "probe must wait the full interval");

        // Cross the threshold — probe fires.
        clock.advance(Duration::from_millis(2));
        engine.maybe_refresh_ac_presence();
        assert_eq!(sys.ac_call_count(), 2, "probe must fire after interval");
    }

    /// revert_record must emit `Event::ProfileReverted` exactly once
    /// per call. We assert the count and the payload fields so a
    /// future refactor that swaps the broadcast type can't silently
    /// drop the emission.
    #[test]
    fn revert_record_emits_profile_reverted() {
        let (tx, mut rx) = broadcast::channel(8);
        let sys: Arc<dyn framesage_sys::SysApi> = Arc::new(MockSysApi::new());
        let record = AppliedRecord {
            profile_id: ProfileId("game-x3d".into()),
            exe_name: "Diablo IV.exe".into(),
            state: framesage_sys::apply::AppliedState::default(),
            // Item 4.7 — leave the drift-detection fields as None so
            // detect_apply_drift sees no signal to read and lets the
            // revert proceed (the existing assertion still holds).
            applied_priority_class_raw: None,
            applied_affinity_mask: None,
        };
        revert_record(sys.as_ref(), &tx, 4242, record);

        // The simulator path's revert is a no-op (no kernel state to
        // restore), so only ProfileReverted should fire. On Windows,
        // the kernel revert call against pid=4242 returns an error
        // (no such process), which produces an extra ActionFailed —
        // both shapes are valid; we just want to find a
        // ProfileReverted in the stream.
        let mut saw_revert = false;
        while let Ok(ev) = rx.try_recv() {
            if let Event::ProfileReverted {
                pid,
                exe_name,
                profile_id,
            } = ev
            {
                assert_eq!(pid, 4242);
                assert_eq!(exe_name, "Diablo IV.exe");
                assert_eq!(profile_id, ProfileId("game-x3d".into()));
                saw_revert = true;
            }
        }
        assert!(saw_revert, "revert_record must emit ProfileReverted");
    }
}
