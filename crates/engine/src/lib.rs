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

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use framesage_core::{CpuTopology, GameModeActions, Policy, Profile, ProfileId};
use framesage_gamemode::{
    journal::{Journal, JournalEntry},
    planner::{plan as plan_game_mode, ActionPlan, SystemStateQuery},
    safe_list::SafeList,
    state::{AppliedActions, PreviousState},
};
use framesage_ipc::{Event, ForegroundSnapshot, StatusSnapshot};

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
}

struct AppliedRecord {
    profile_id: ProfileId,
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
        }
    }

    /// Panic button: revert any active system mode regardless of foreground.
    /// Idempotent — `Ok(())` if no system mode was active.
    pub fn exit_system_mode_now(&self) {
        let mut s = self.state.write();
        Self::revert_system_mode_locked(&mut s, &self.journal);
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

        let foreground = framesage_sys::foreground::current()?;

        let mut s = self.state.write();
        self.reconcile(&mut s, foreground)
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

        // Revert per-process state on the previous foreground.
        if let Some(prev_pid) = s.current_foreground.take() {
            if let Some(record) = s.applied.remove(&prev_pid) {
                revert_record(prev_pid, record);
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

        let profile_id = s
            .policy
            .match_foreground(&fg.exe_name, &fg.path, &fg.title)
            .clone();

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

        // Per-process apply.
        match apply_profile(fg.pid, &profile, &topology) {
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
        let mut entry = JournalEntry::new(profile_id.clone(), plan.previous_state.clone());

        // Write the journal *before* any state change. If this fails, we
        // refuse to apply — no journal means no safe revert path.
        if let Err(e) = journal.write(&entry) {
            warn!(error = %e, "initial journal write failed; refusing to enter game mode");
            return;
        }

        let mut applied = AppliedActions::default();
        let mut any_failed = false;

        for action in &plan.actions {
            match sys_apply_action(action, &mut applied) {
                Ok(()) => {}
                Err(e) => {
                    warn!(?action, error = %e, "game mode action failed; continuing with rest of plan");
                    any_failed = true;
                }
            }
            // Update the journal incrementally so a crash leaves the most
            // accurate possible revert plan.
            entry.applied = applied.clone();
            if let Err(je) = journal.write(&entry) {
                // We've already started applying actions; we can't safely
                // continue without journaling, but rolling back what we did
                // is essentially "revert now." Do exactly that.
                warn!(error = %je, "journal update failed mid-apply; reverting");
                sys_revert_all(&applied, &plan.previous_state);
                let _ = journal.delete();
                return;
            }
        }

        if !plan.rejections.is_empty() {
            for r in &plan.rejections {
                warn!(rejected = %r.id, reason = %r.reason, "game mode action rejected by safe-list");
            }
        }

        info!(
            profile = %profile_id,
            actions = applied.applied_count(),
            partial = any_failed,
            session = %entry.session_id,
            "game mode entered"
        );

        s.system_mode = Some(ActiveSystemMode {
            profile_id: profile_id.clone(),
            previous: plan.previous_state,
            applied,
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

trait AppliedActionsExt {
    fn applied_count(&self) -> usize;
}

impl AppliedActionsExt for AppliedActions {
    fn applied_count(&self) -> usize {
        let mut n = 0;
        if self.hid_taskbar {
            n += 1;
        }
        n += self.stopped_services.len();
        n += self.suspended_pids.len();
        if self.switched_power_plan {
            n += 1;
        }
        if self.set_focus_assist {
            n += 1;
        }
        if self.paused_windows_update {
            n += 1;
        }
        n
    }
}

#[cfg(windows)]
fn apply_profile(pid: u32, profile: &Profile, topology: &CpuTopology) -> Result<AppliedRecord> {
    let state = framesage_sys::apply::apply(pid, profile, topology)?;
    Ok(AppliedRecord {
        profile_id: profile.id.clone(),
        state,
    })
}

#[cfg(not(windows))]
fn apply_profile(_pid: u32, profile: &Profile, _topology: &CpuTopology) -> Result<AppliedRecord> {
    Ok(AppliedRecord {
        profile_id: profile.id.clone(),
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
