//! Domain types for framesage-win.
//!
//! This crate is intentionally platform-agnostic — it defines *what* the system
//! does (profiles, rules, topology), not *how*. The `framesage-sys` crate maps
//! these types onto Win32 APIs; `framesage-engine` decides which profile to
//! apply based on observed state.

pub mod paths;
pub mod policy;
pub mod profile;
pub mod topology;

pub use policy::{AppMatch, AppRule, Policy};
pub use profile::{
    IoPriority, MemoryPriority, PowerThrottlingMode, PriorityClass, Profile, ProfileId,
};
pub use topology::{CoreKind, CpuSelector, CpuTopology, LogicalCpu};
