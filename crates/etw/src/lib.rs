//! framesage-etw — v0.7 closed-loop ETW kernel-event consumer.
//!
//! Day 3 scaffold (per `spike/group-a-week-2-plan.md` §4 Day 1–3):
//! `build_gate` (Day 1) + `session` lifecycle (Day 2) + the
//! `EtwSysCalls` trait surface + `EtwSubsystem` + `SessionShutdownHandle`
//! (Day 3 §3.2 + §3.4) + `degradation::DegradationMode` /
//! `DegradationEvent` (§3.3) + `supervisor::SupervisorLoop` (§3.6).
//!
//! Day 4 lands the per-mode unit tests against this scaffold. Day 5
//! instantiates `SupervisorLoop` inside `crates/service/`.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

pub mod build_gate;
pub mod classify;
pub mod degradation;
pub mod session;
pub mod signal;
pub mod supervisor;

pub use build_gate::{
    closed_loop_enabled_for_this_build, detected_build, MIN_BUILD_FOR_CLOSED_LOOP,
};
pub use classify::{classify, KernelEventKind, ProviderGuid, KERNEL_EVENT_KINDS};
pub use degradation::{DegradationEvent, DegradationMode};
pub use session::{
    EtwSession, EtwSubsystem, EtwSysCalls, MonitorHandle, RealEtwSysCalls, SessionOptions,
    SessionShutdownHandle, SessionStats,
};
pub use signal::{KernelSignal, KernelSignalDetector};
pub use supervisor::{ConsumerExitReason, SupervisorLoop};
