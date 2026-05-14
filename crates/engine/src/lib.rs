//! framesage policy engine.
//!
//! The engine sits between the OS (via `framesage-sys`) and the user-facing
//! configuration (a `Policy`). Each tick it asks: what is the foreground
//! process right now? — and reconciles the running state against what the
//! policy says it should be.
//!
//! Design notes:
//!
//! * **Reconcile, don't event-chase.** Windows has a `WinEventHook` for focus
//!   changes, but real games still spawn helper windows, fullscreen-flip,
//!   minimise, etc., and the focus events lie often enough that a 250–500ms
//!   polled reconcile is more robust *and* simpler. We'll add a focus hook
//!   later as an optimisation, not as the source of truth.
//! * **Track what we applied, so we can revert.** When a foregrounded process
//!   loses focus (or exits), we restore the per-process state we changed.
//!   Otherwise we leak overrides into processes the user never wanted us
//!   touching.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use parking_lot::RwLock;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use framesage_core::{CpuTopology, Policy, Profile, ProfileId};
use framesage_ipc::{Event, ForegroundSnapshot, StatusSnapshot};

pub struct Engine {
    state: Arc<RwLock<EngineState>>,
    events: broadcast::Sender<Event>,
}

struct EngineState {
    policy: Policy,
    topology: CpuTopology,
    paused: bool,
    /// What we've applied, keyed by pid, so we can revert.
    applied: HashMap<u32, AppliedRecord>,
    /// Currently-foregrounded pid, if any.
    current_foreground: Option<u32>,
    /// Last-seen foreground details (so Status doesn't have to syscall).
    foreground_snapshot: Option<ForegroundSnapshot>,
    active_profile: Option<ProfileId>,
}

struct AppliedRecord {
    profile_id: ProfileId,
    /// Opaque per-platform state used to revert. Held by value here; the
    /// concrete type lives in framesage-sys behind a `cfg(windows)`.
    #[cfg(windows)]
    state: framesage_sys::apply::AppliedState,
    #[cfg(not(windows))]
    _phantom: (),
}

impl Engine {
    pub fn new(policy: Policy, topology: CpuTopology) -> Self {
        let (tx, _) = broadcast::channel(64);
        Self {
            state: Arc::new(RwLock::new(EngineState {
                policy,
                topology,
                paused: false,
                applied: HashMap::new(),
                current_foreground: None,
                foreground_snapshot: None,
                active_profile: None,
            })),
            events: tx,
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

    /// One reconciliation pass. Designed to be called from a tokio interval
    /// with the policy's `tick_ms` cadence.
    pub fn tick(&self) -> Result<()> {
        // Snapshot what we need without holding the lock across syscalls.
        let (paused, tick_ctx) = {
            let s = self.state.read();
            (s.paused, TickContext::from_state(&s))
        };
        if paused {
            return Ok(());
        }

        let foreground = framesage_sys::foreground::current()?;

        let mut s = self.state.write();
        self.reconcile(&mut s, foreground, &tick_ctx)
    }

    fn reconcile(
        &self,
        s: &mut EngineState,
        foreground: Option<framesage_sys::foreground::ForegroundInfo>,
        _ctx: &TickContext,
    ) -> Result<()> {
        let new_pid = foreground.as_ref().map(|f| f.pid);

        // Foreground unchanged? Nothing to do.
        if new_pid == s.current_foreground {
            return Ok(());
        }

        // Revert anything we had applied to the *previous* foreground process.
        if let Some(prev_pid) = s.current_foreground.take() {
            if let Some(record) = s.applied.remove(&prev_pid) {
                #[cfg(windows)]
                if let Err(e) = framesage_sys::apply::revert(prev_pid, record.state) {
                    warn!(pid = prev_pid, error = %e, "revert failed");
                }
                debug!(pid = prev_pid, profile = %record.profile_id, "reverted");
                let _ = record; // silence unused on non-windows
            }
        }

        // Apply the new foreground, if any.
        let Some(fg) = foreground else {
            s.foreground_snapshot = None;
            s.active_profile = None;
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

        match apply_profile(fg.pid, &profile, &topology) {
            Ok(record) => {
                info!(pid = fg.pid, exe = %fg.exe_name, profile = %profile_id, "applied");
                s.applied.insert(fg.pid, record);
                s.current_foreground = Some(fg.pid);
                s.foreground_snapshot = Some(snapshot.clone());
                s.active_profile = Some(profile_id.clone());
                let _ = self.events.send(Event::ForegroundChanged {
                    foreground: snapshot,
                    profile: profile_id,
                });
            }
            Err(e) => {
                warn!(pid = fg.pid, exe = %fg.exe_name, error = %e, "apply failed");
            }
        }

        Ok(())
    }
}

/// Read-only snapshot of bits of state the tick path needs. Kept tiny so we
/// don't hold the engine lock across blocking syscalls.
struct TickContext;

impl TickContext {
    fn from_state(_s: &EngineState) -> Self {
        Self
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
