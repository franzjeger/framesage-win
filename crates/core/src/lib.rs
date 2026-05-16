//! Domain types for framesage-win.
//!
//! This crate is intentionally platform-agnostic — it defines *what* the system
//! does (profiles, rules, topology), not *how*. The `framesage-sys` crate maps
//! these types onto Win32 APIs; `framesage-engine` decides which profile to
//! apply based on observed state.

pub mod anti_cheat;
pub mod game_mode;
pub mod paths;
pub mod policy;
pub mod profile;
pub mod topology;
pub mod undo;

pub use anti_cheat::AntiCheatPresence;
pub use game_mode::{FocusAssistMode, GameModeActions, PowerPlanId};
pub use policy::{AffinityRule, AppMatch, AppRule, Policy, ProBalanceConfig};
pub use profile::{
    AntiCheatProfile, IoPriority, MemoryPriority, PowerThrottlingMode, PriorityClass, Profile,
    ProfileId,
};
pub use topology::{CoreKind, CpuSelector, CpuTopology, LogicalCpu};
pub use undo::{UndoEntry, UndoSummary, UndoableAction};
