//! Win32 implementations of Game Mode actions.

pub mod apply;
pub mod power_plan;
pub mod process;
pub mod query;
pub mod service;
pub mod taskbar;

pub use apply::{apply_action, revert_all};
pub use query::Win32StateQuery;
