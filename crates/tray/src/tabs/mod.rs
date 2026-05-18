//! Tab renderer modules.
//!
//! W1.6 pioneers this module structure with `sessions.rs`. The K-004
//! (PHASE2-PLAN.md item 3.6 / Phase 3 M3.6) split moves the rest of
//! the tab renderers (`render_status_tab`, `render_processes_tab`,
//! `render_activity_tab`, etc., currently inline in `main.rs`) into
//! sibling modules here. Until that lands, this module is
//! intentionally minimal.

pub mod sessions;
