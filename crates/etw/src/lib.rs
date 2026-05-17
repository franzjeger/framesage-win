//! framesage-etw — v0.7 closed-loop ETW kernel-event consumer.
//!
//! Day 2 scaffold (per `spike/group-a-week-2-plan.md` §4 Day 1–2):
//! `build_gate` (Day 1) + `session` lifecycle (Day 2). `degradation`
//! + `supervisor` land Days 3–5.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

pub mod build_gate;
pub mod session;

pub use build_gate::{
    closed_loop_enabled_for_this_build, detected_build, MIN_BUILD_FOR_CLOSED_LOOP,
};
pub use session::{EtwSession, SessionOptions, SessionStats};
