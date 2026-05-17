//! framesage-etw — v0.7 closed-loop ETW kernel-event consumer.
//!
//! Day 1 scaffold (per `spike/group-a-week-2-plan.md` §4 Day 1):
//! only `build_gate` is populated. `session`, `degradation`, and
//! `supervisor` modules land on Days 2–5; this file's public
//! re-exports mirror the planned surface so Day-2+ adds bodies, not
//! signatures.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

pub mod build_gate;

pub use build_gate::{
    closed_loop_enabled_for_this_build, detected_build, MIN_BUILD_FOR_CLOSED_LOOP,
};
