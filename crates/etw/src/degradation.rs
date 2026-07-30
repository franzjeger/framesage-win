//! ETW subsystem degradation modes and the event type that carries
//! them to consumers (logs, future UI banner, supervisor sink).
//!
//! Per `spike/group-a-week-2-plan.md` §3.3 (Day 3 deliverable) +
//! architecture §2.1's six-mode table.
//!
//! The enum is the boundary between framesage-etw's internals and
//! the rest of the system. Engine code in week 5+ pattern-matches on
//! `EtwSubsystem::Disabled(mode)` to pick the banner / log line;
//! defining all six variants now means future modes don't trigger a
//! ripple of caller updates.

/// The six degradation modes from architecture §2.1.
///
/// Stable identifiers consumed by the engine (week 5+, in
/// `EtwSubsystem::Disabled(_)`) and the UI (Group C banner / Status
/// tab indicator). Variants are intentionally not `non_exhaustive` —
/// adding a seventh mode IS a coordinated API change that needs every
/// match-arm caller to update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradationMode {
    /// Mode 1: `StartTraceW` returned `ERROR_ACCESS_DENIED`. The EDR-
    /// blocked-us path. Tested via mock in week 2; real-EDR validation
    /// is a v0.7.1 gate (see `spike/etw-edr-report.md` §6.1).
    AccessDenied,
    /// Mode 2: `StartTraceW` returned `ERROR_ALREADY_EXISTS` even
    /// after `cleanup_stale_session()`. Another ETW consumer holds the
    /// session name. Recovery: restart FrameSage manually OR the other
    /// consumer exits.
    AlreadyExists,
    /// Mode 3: `RealTimeBuffersLost > 0` at any `QUERY`. Marks the
    /// session as `partial_data`; future Sessions UI shows the
    /// degraded marker. Recovery is automatic on next clean window.
    KernelDrops,
    /// Mode 4: our consumer-side ring buffer (week 3+) overflows
    /// because the drain thread couldn't keep up. Week 2 declares the
    /// variant for shape-completeness; the actual emission path lands
    /// when the ring buffer does.
    OurDrops,
    /// Mode 5: consumer thread panicked.
    ///
    /// Disposition (per v3 user decision + architecture §2.1 mode 5
    /// amendment, per `audit/v0.7-architecture.md` §2.1 mode 5 row):
    /// **ETW subsystem transitions to Disabled; service host stays up;
    /// engine continues in v0.6 static-rule mode.** SCM restart not
    /// required because the rule-engine half is still serving.
    ConsumerPanic,
    /// Mode 6: build gate — `RtlGetVersion` returns a build number
    /// below `MIN_BUILD_FOR_CLOSED_LOOP` (currently 26100 = Win11 24H2).
    /// Static-rule fallback per architecture §2.1 "Build gate" /
    /// Phase 2 sign-off Decision 1.
    ///
    /// `detected_build` carries the actual build number for the log
    /// and UI surfacing ("Closed-loop disabled on Win11 23H2 / build
    /// 22631; upgrade to 24H2 for closed-loop attribution"). `None`
    /// means the `RtlGetVersion` probe itself failed — extremely rare,
    /// distinct from "build is too low."
    BuildUnsupported { detected_build: Option<u32> },
}

impl DegradationMode {
    /// M2.3 / A-002 — start-time retry policy for this mode.
    ///
    /// `true` means a later `EtwSession::start()` attempt may succeed
    /// without any change to the host (the blocking condition is
    /// transient or externally owned):
    ///
    /// * `AlreadyExists` — another consumer holds the session name;
    ///   retry succeeds once it exits.
    /// * `KernelDrops` / `OurDrops` — runtime data-loss modes that
    ///   recover automatically on the next clean window; a session
    ///   restart is always worth attempting.
    ///
    /// `false` means retrying without operator/host change is futile:
    ///
    /// * `AccessDenied` — EDR / policy block; retrying re-triggers the
    ///   same denial (and may look like probing to the EDR).
    /// * `BuildUnsupported` — the Windows build won't change under us.
    /// * `ConsumerPanic` — per architecture §2.1 mode 5, the subsystem
    ///   stays down for the service lifetime; a restart of the service
    ///   (not a blind in-process retry) is the recovery path.
    pub fn is_start_retryable(&self) -> bool {
        match self {
            DegradationMode::AlreadyExists
            | DegradationMode::KernelDrops
            | DegradationMode::OurDrops => true,
            DegradationMode::AccessDenied
            | DegradationMode::ConsumerPanic
            | DegradationMode::BuildUnsupported { .. } => false,
        }
    }
}

/// Carries a `DegradationMode` to the consumer (production: a
/// `tracing::error!` sink per v3 secondary decision Option C; tests:
/// a captured-event-vec closure).
///
/// `detail` is a free-form string for log readability — e.g., the
/// underlying `WIN32_ERROR` value as hex, or the panic-message
/// extracted from a `ConsumerPanic`. Not parsed by consumers; intended
/// for humans reading logs or the Group C UI banner.
#[derive(Debug, Clone)]
pub struct DegradationEvent {
    pub mode: DegradationMode,
    pub detail: String,
}

impl DegradationEvent {
    /// Construct a degradation event with no detail string. Useful for
    /// modes where the variant itself is the entire signal (e.g., the
    /// build-gate path).
    pub fn bare(mode: DegradationMode) -> Self {
        Self {
            mode,
            detail: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn degradation_mode_variants_are_distinct() {
        // Lock the six variants in. If a seventh ever lands without
        // being a coordinated API change, this test failing tells the
        // engineer to check every `match DegradationMode` arm in the
        // tree before adding the variant.
        let variants = [
            DegradationMode::AccessDenied,
            DegradationMode::AlreadyExists,
            DegradationMode::KernelDrops,
            DegradationMode::OurDrops,
            DegradationMode::ConsumerPanic,
            DegradationMode::BuildUnsupported {
                detected_build: Some(22631),
            },
        ];
        for (i, a) in variants.iter().enumerate() {
            for (j, b) in variants.iter().enumerate() {
                if i == j {
                    assert_eq!(a, b);
                } else {
                    assert_ne!(a, b);
                }
            }
        }
    }

    #[test]
    fn build_unsupported_carries_build_number() {
        let with = DegradationMode::BuildUnsupported {
            detected_build: Some(22631),
        };
        let without = DegradationMode::BuildUnsupported {
            detected_build: None,
        };
        assert_ne!(with, without);
    }

    // M2.3 / A-002 — the retry classification is load-bearing for the
    // service's future retry loop; lock each variant's disposition.
    #[test]
    fn start_retry_policy_per_variant() {
        assert!(DegradationMode::AlreadyExists.is_start_retryable());
        assert!(DegradationMode::KernelDrops.is_start_retryable());
        assert!(DegradationMode::OurDrops.is_start_retryable());
        assert!(!DegradationMode::AccessDenied.is_start_retryable());
        assert!(!DegradationMode::ConsumerPanic.is_start_retryable());
        assert!(!DegradationMode::BuildUnsupported {
            detected_build: Some(22631)
        }
        .is_start_retryable());
        assert!(!DegradationMode::BuildUnsupported {
            detected_build: None
        }
        .is_start_retryable());
    }

    #[test]
    fn bare_constructor_produces_empty_detail() {
        let ev = DegradationEvent::bare(DegradationMode::AccessDenied);
        assert_eq!(ev.mode, DegradationMode::AccessDenied);
        assert!(ev.detail.is_empty());
    }
}
