//! framesage-recorder — v0.7.1 Group C session recorder scaffold.
//!
//! Issue #110 / architecture §2.3 + §2.4. Three pieces:
//!
//! * [`schema`] — the on-disk session event schema (jsonl, v1),
//!   including the reserved `cpu_sample.per_process` v0.8 slot.
//! * [`store`] — append-only session writer with the §2.3 retention
//!   policy (80/95/100% downsample thresholds, 1 GB total-cap
//!   rotation), plus the reader and list-view derivation.
//! * [`attribution`] — `compute_attribution_summary`: the honest
//!   "Did FrameSage help?" computation with the deliberately
//!   asymmetric delta bands and explicit disabled-attribution states.
//!
//! What this scaffold does NOT yet contain (remaining #110 scope):
//! the service-side drain worker that feeds the writer from live
//! engine/ETW/PresentMon events, the `ListSessions` / `ReadSession`
//! IPC surface, and the full Sessions-tab list/detail UI. Those land
//! on top of these types; nothing here depends on Windows, so the
//! whole crate is testable on any host.

pub mod attribution;
pub mod schema;
pub mod store;

pub use attribution::{
    compute_attribution_summary, Attribution, AttributionSummary, DeltaBand, DisabledReason,
};
pub use schema::{SessionEvent, SessionSummary, SystemInfo, SCHEMA_VERSION};
pub use store::{
    enforce_total_cap, list_sessions, read_session, sample_rate_for_bytes, SampleRate,
    SessionListEntry, SessionWriter, PER_SESSION_CAP_BYTES, TOTAL_CAP_BYTES,
};
