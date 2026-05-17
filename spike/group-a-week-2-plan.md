# v0.7 Group A — Week 2 implementation plan

**Status:** DRAFT v2 — buddy-approved (verdict PROCEED) with amendments applied (see §11). Awaiting user sign-off before execution.
**Authoritative inputs:**
- `audit/v0.7-architecture.md` §2.1 (degradation modes, build gate, LocalSystem privilege model) and "Phase 3 acceptance criteria → Group A — ETW foundation"
- `spike/etw-schemas.md` "Group A weeks 2-7 implementation gates" + "Implementation requirements driven by this document"
- `spike/etw-report.md` (Phase 1 spike validated behavior — the reference implementation week 2 lifts from)
- `audit/buddy-disagreements.md` Entry 1 (gating rules + buddy-rhythm requirement)
- `spike/etw-edr-report.md` §6 (closed-loop default-off in v0.7; EDR matrix a v0.7.1 gate)

**Not in scope for v0.7 closed-loop overall:** anything `spike/etw-edr-report.md` §6.1 marks for v0.7.1.

**Not in scope for week 2 specifically:** see §8 below — event-parsing dispatcher, the four schema-doc parser-level acceptance-criteria tests, ring buffer, PresentMon, recorder, UI.

---

## 1. Scope — what week 2 will build

Week 2 lays the production bones of the ETW consumer. By end of week:

- A new `crates/etw/` crate exists in the workspace.
- It contains the session lifecycle code lifted from `crates/spike-etw/` (validated in Phase 1) — `StartTraceW`, `OpenTraceW`, `ProcessTrace`, the drop-rate query loop, clean shutdown, stale-session cleanup.
- The build-gate check (`MIN_BUILD_FOR_CLOSED_LOOP: u32 = 26100`) is in place and `EtwSession::start()` short-circuits to `Disabled` on unsupported builds.
- All **six** degradation modes from architecture §2.1's table have unit-test coverage via synthetic return-value mocks. Mode #1 (`ERROR_ACCESS_DENIED` — the EDR-blocked path) is tested via mock per the EDR-matrix-is-v0.7.1-not-Group-A decision.
- The service crate (`crates/service/`) is wired to spawn the consumer thread on startup if and only if `closed_loop_enabled` in policy AND build gate passes, otherwise it logs the INFO line and does not instantiate the session.

What week 2 does **NOT** build: event parsing (no CSwitch/DPC/ISR/HardFault/DiskIo parsers yet — that's week 3+), SPSC ring buffer for callback→drain (week 3+), PresentMon (Group B), session recorder (Group B), Sessions UI (Group C).

The end-of-week deliverable is a service binary that, when installed on a Win11 24H2+ box with `closed_loop_enabled: true`, opens an ETW session, runs the consumer thread until shutdown, and reports drop-rate stats to logs. **No closed-loop signal is produced yet** — that requires the week 3+ parsers — but the lifecycle is operational and all the degradation paths are covered by tests.

---

## 2. New crate layout: `crates/etw/`

```
crates/etw/                — package name: framesage-etw
├── Cargo.toml
└── src/
    ├── lib.rs           — public API surface (EtwSession, EtwSubsystem,
    │                      closed_loop_enabled_for_this_build)
    ├── session.rs       — EtwSession lifetime: start / stop / consumer
    │                      thread / drop-rate query loop / stale cleanup
    ├── build_gate.rs    — RtlGetVersion wrapper + the MIN_BUILD_FOR_CLOSED_LOOP
    │                      const + closed_loop_enabled_for_this_build()
    ├── degradation.rs   — DegradationMode enum + DegradationEvent (sent
    │                      to the engine for banner UI later in Group C)
    └── tests/           — unit tests for build_gate + degradation modes
        ├── build_gate_tests.rs
        └── degradation_tests.rs
```

**Naming convention note (per buddy review of this plan):** the
workspace pattern is `crates/X/` with `[package].name =
"framesage-X"`. Examples: `crates/service/` → `framesage-service`,
`crates/sys/` → `framesage-sys`, `crates/core/` → `framesage-core`.
The new crate follows that pattern: directory `crates/etw/`,
package name `framesage-etw`. The first DRAFT of this plan
mistakenly wrote `crates/framesage-etw/`; buddy caught it before
execution.

`Cargo.toml` skeleton:

```toml
[package]
name = "framesage-etw"
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true
repository.workspace = true
rust-version.workspace = true
description = "v0.7 closed-loop ETW kernel-event consumer for the framesage-svc service host."

[dependencies]
anyhow = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }

[target.'cfg(windows)'.dependencies]
windows = { workspace = true, features = [
    "Win32_Foundation",
    "Win32_System_Console",
    "Win32_System_Diagnostics_Etw",
    "Win32_System_SystemInformation",
    "Win32_System_Time",
] }
```

Workspace `Cargo.toml` gains `"crates/framesage-etw"` in `members`, and a new `framesage-etw = { path = "crates/framesage-etw" }` entry in `[workspace.dependencies]`.

---

## 3. File-by-file deliverables

### 3.1 `build_gate.rs`

**Public surface:**

```rust
/// Minimum Windows build for v0.7 closed-loop measurement.
/// Per architecture §2.1 "Build gate" (Phase 2 sign-off Decision 1):
/// Windows 11 24H2 = build 26100.
pub const MIN_BUILD_FOR_CLOSED_LOOP: u32 = 26100;

/// Returns true iff the running Windows build supports the v0.7
/// closed-loop subsystem. Uses RtlGetVersion under the hood (NOT
/// GetVersionEx, which manifest-lies on Win11 — see architecture).
pub fn closed_loop_enabled_for_this_build() -> bool { /* ... */ }

/// Returns the detected build number, or None if the underlying
/// RtlGetVersion call failed (extremely unusual; logged at INFO).
pub fn detected_build() -> Option<u32> { /* ... */ }
```

**Implementation notes:**
- `RtlGetVersion` lives in `ntdll.dll`. Bind via `windows::Win32::System::SystemInformation::OSVERSIONINFOEXW` + `RtlGetVersion`.
- The first call caches the result behind a `OnceLock<Option<u32>>` so repeated calls are free. Callers can hit `closed_loop_enabled_for_this_build()` from anywhere without worrying about cost.

**Rationale:** Architecture §2.1's "Build gate" subsection mandates the predicate-short-circuit pattern at `EtwSession::start()`. Putting the check in its own module makes it independently testable and stops the predicate from leaking into the session code.

### 3.2 `session.rs`

**Public surface:**

```rust
pub struct EtwSession { /* opaque handle */ }

#[derive(Debug)]
pub enum EtwSubsystem {
    /// Session running normally.
    Running(EtwSession),
    /// Session not instantiated. The variant carries the reason so
    /// the service can surface it in logs + (later, Group C) the UI.
    Disabled(DegradationMode),
}

impl EtwSession {
    /// Architecture §2.1: the entry point that short-circuits on
    /// build gate, then attempts session start, then returns the
    /// appropriate EtwSubsystem variant.
    pub fn start(opts: SessionOptions) -> Result<EtwSubsystem>;

    /// Triggers an orderly shutdown: ControlTraceW(STOP), joins the
    /// consumer thread, verifies the session no longer exists via
    /// a follow-up QUERY.
    pub fn stop(self) -> Result<()>;

    /// Reads the running session's RealTimeBuffersLost + EventsLost
    /// + buffer stats. Called once per second by the drop-rate query
    /// loop; can also be called externally for diagnostics.
    pub fn query_stats(&self) -> Result<SessionStats>;
}
```

**Where the code comes from:** `crates/spike-etw/src/main.rs` — Phase 1 validated. The lift is mechanical: the spike's `start_session`, `cleanup_stale_session`, `ControlTraceW` query loop, and `ProcessTrace`-spawning consumer thread move to `session.rs`. The spike binary stays in the repo (and on `chore/clippy-baseline` is the version with clippy fixes); week 2's lift is a copy-then-modify, NOT a delete-spike-and-replace.

**Cargo features:**
- `EtwSession` and friends are `#[cfg(windows)]`. On non-Windows the crate compiles to an empty stub so workspace-wide `cargo check --all-targets` keeps working in CI's cross-check job.

### 3.3 `degradation.rs`

**Public surface:**

```rust
/// The six degradation modes from architecture §2.1's table.
/// Stable identifiers consumed by the engine (week 5+, in
/// `EtwSubsystem::Disabled(_)`) and the UI (Group C banner / Status
/// tab indicator).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradationMode {
    /// Mode 1: StartTraceW returned ERROR_ACCESS_DENIED. The
    /// EDR-blocked-us path. Tested via mock in week 2; real-EDR
    /// validation is a v0.7.1 gate (see spike/etw-edr-report.md).
    AccessDenied,
    /// Mode 2: StartTraceW returned ERROR_ALREADY_EXISTS even after
    /// cleanup. Other ETW consumer holds the session name.
    AlreadyExists,
    /// Mode 3: RealTimeBuffersLost > 0 at any QUERY. Marks session
    /// as partial_data.
    KernelDrops,
    /// Mode 4: our consumer-side ring buffer (week 3+) overflows.
    OurDrops,
    /// Mode 5: consumer thread panicked. SCM restarts via existing
    /// FailureActions.
    ConsumerPanic,
    /// Mode 6: build gate — RtlGetVersion returns < MIN_BUILD_FOR_CLOSED_LOOP.
    /// The static-rule fallback per Decision 1.
    BuildUnsupported { detected_build: Option<u32> },
}
```

**Rationale:** the enum is the boundary between framesage-etw's internals and the rest of the system (engine, IPC, UI). Engine code in week 5+ will pattern-match on `EtwSubsystem::Disabled(mode)` to decide which banner to show; defining the enum now means we don't need to refactor every caller when modes get added.

---

## 4. Day-by-day breakdown

Five working days. Each day ends with a testable deliverable and a stop gate.

### Day 1 — crate skeleton + build gate

**Deliverable:** `crates/etw/` exists in the workspace, builds clean. `build_gate.rs` implements the `MIN_BUILD_FOR_CLOSED_LOOP` const + `RtlGetVersion` wrapper + `closed_loop_enabled_for_this_build()` predicate. Unit tests cover: (a) the predicate returns true on builds ≥ 26100 (mocked via a test-only injection point), (b) the predicate returns false on build 22631 (Win11 23H2) and asserts `detected_build() == Some(22631)`, (c) the predicate returns false on a `RtlGetVersion` failure path (mocked) and asserts `detected_build() == None`.

**Stop gate:** if `RtlGetVersion` binding doesn't work as expected (e.g. linking issue, feature gating different in `windows-rs` 0.58), STOP and investigate. Don't fall back to `GetVersionEx` — the architecture explicitly forbids it.

### Day 2 — session lifecycle lift

**Deliverable:** `session.rs` contains the lifted session-lifecycle code from spike-etw. Cargo workspace `cargo check --workspace` is green. The crate compiles in both `#[cfg(windows)]` and stub paths. A minimal `EtwSession::start()` (no event parsing yet) succeeds on a Win11 26200 dev box, opens the session, exits cleanly. `EtwSession::query_stats()` returns sensible numbers (zero drops at idle).

**Stop gate:** if the lift surfaces ANY behavioral delta from the spike (e.g. the spike's `StartTraceW` succeeded but the production lift fails on the same machine), STOP and surface the diff. Don't paper over with "probably just an init-ordering thing."

**Verification command:** run the resulting binary elevated for 60 s, capture `logman query FramesageEtw -ets` output before / during / after, paste literal output into the EOD note per the spike-reports-include-literal-output ground rule.

### Day 3 — degradation enum + EtwSubsystem return type + mock-injection scaffold

**Deliverable:** `degradation.rs` defines `DegradationMode` per §3.3. `EtwSession::start()` returns `Result<EtwSubsystem>` instead of bare `Result<EtwSession>`. The build-gate short-circuit produces `EtwSubsystem::Disabled(DegradationMode::BuildUnsupported { detected_build })` without ever touching ETW APIs. Logged at INFO with the exact line from architecture §2.1.

**Day 3 also scaffolds the mock-injection abstraction** (per buddy review of this plan): a `#[cfg(test)]`-gated trait indirection on the `StartTraceW` / `ControlTraceW` / `RtlGetVersion` call sites. Production code uses the concrete Windows API; tests substitute a fake. Doing the scaffolding here (alongside the `EtwSubsystem` refactor that touches the same call sites) keeps Day 4 focused on writing the six test cases against an existing scaffold, not building scaffold + tests in one day. Buddy's catch: building scaffold + 5 real tests in Day 4 risks spilling into Day 5's service-wiring time.

**Stop gates:**
- If the architecture's intended log line conflicts with the actual `tracing` formatter (rare but possible — line breaks, format-string mismatch), STOP and propose a doc-level fix to the architecture rather than diverging silently.
- If the trait-indirection abstraction is the wrong shape (e.g. introduces lifetime gymnastics, requires dyn-dispatch on the hot path even in production builds, or leaks `cfg(test)` symbols into the public API), STOP and re-think before Day 4 commits more code on top of it.

### Day 4 — degradation-mode unit tests (against Day-3 scaffold)

**Deliverable:** `tests/degradation_tests.rs` contains six tests, one per `DegradationMode` variant. The mock-injection scaffold from Day 3 is the substrate; Day 4 is writing test cases against it, NOT building the scaffold from scratch. Each test injects a synthetic failure at the appropriate layer:
- **Mode 1 (AccessDenied):** `StartTraceW` mock returns `ERROR_ACCESS_DENIED`. Assert `start()` returns `EtwSubsystem::Disabled(AccessDenied)`. NOT against a real EDR — that's a v0.7.1 gate.
- **Mode 2 (AlreadyExists):** mock returns `ERROR_ALREADY_EXISTS` even after `cleanup_stale_session()`. Assert disabled-with-`AlreadyExists`. Verify cleanup was attempted (call count).
- **Mode 3 (KernelDrops):** `query_stats` mock returns `RealTimeBuffersLost = 5`. Assert that a `DegradationEvent::KernelDrops { rate }` is emitted on the next poll cycle.
- **Mode 4 (OurDrops):** the ring buffer doesn't exist yet (week 3+), so this test is a placeholder that asserts the mode exists and serializes correctly. The full path test ships with the ring buffer.
- **Mode 5 (ConsumerPanic):** spawn the consumer thread, have it panic, assert that `EtwSession::stop()` reports the panic via the appropriate channel (logged + event emitted, not silently swallowed).
- **Mode 6 (BuildUnsupported):** build gate returns false (test injection), assert `start()` short-circuits to `Disabled(BuildUnsupported { detected_build: Some(22631) })` and that no ETW APIs were called (call-count assertion against the mock).

**Stop gate:** if any of the six mocks turns out to be impossible to inject without invasive surgery on the session module, STOP and re-think the test approach. The architecture's intent is that all six modes are testable without spinning up a real ETW session or a real EDR; if the production-code structure makes that impossible, the structure needs revisiting BEFORE more code lands on it.

### Day 5 — service wiring + EOD verification

**Deliverable:** `crates/service/` gains a startup hook that, after policy loads, evaluates:

```rust
if policy.closed_loop_enabled && build_gate.closed_loop_enabled_for_this_build() {
    match EtwSession::start(opts) {
        Ok(EtwSubsystem::Running(session)) => spawn_consumer_supervisor(session),
        Ok(EtwSubsystem::Disabled(mode)) => emit_degradation_log(mode),
        Err(e) => emit_startup_error(e),
    }
} else {
    // Either user opted out OR build is unsupported. Either way,
    // no ETW session; engine runs in v0.6 static-rule mode.
    log_static_rule_mode_reason(policy, build_gate);
}
```

The supervisor (its own small task) holds the `EtwSession`, runs the drop-rate query loop on a 1-second tokio interval, and forwards `DegradationEvent`s into the existing system-events channel (`SystemEvent`, see `crates/engine/`). No actual closed-loop signal yet; just the lifecycle.

**EOD verification (per the spike-reports-include-literal-output ground rule):**
1. Install built service on a Win11 26200 dev box.
2. Run with `closed_loop_enabled: false` policy. Capture `Get-Service framesage` literal output. Confirm INFO log shows static-rule path.
3. Update policy to `closed_loop_enabled: true`. Restart service. Capture `logman query FramesageEtw -ets` literal output. Confirm session is running.
4. Stop service via SCM. Capture `logman query FramesageEtw -ets` literal output. Confirm session is gone.
5. Run all unit tests: `cargo test -p framesage-etw -- --nocapture`. Confirm 6/6 degradation-mode tests pass.

Put all literal command outputs in `spike/group-a-week-2-report.md` (created at EOD as the week's deliverable doc).

**Stop gate:** if any of the EOD checks deviates from expected (especially: a stale session left behind after shutdown — that's a regression against architecture §2.1's "Survives service restarts" promise), STOP and DO NOT mark week 2 complete.

---

## 5. Tests written, by file

By end of week, the new crate has:

| Test file | Cases | What they verify |
|---|---|---|
| `tests/build_gate_tests.rs` | 3 cases | Predicate returns true ≥ 26100, false at 22631, false on `RtlGetVersion` failure |
| `tests/degradation_tests.rs` | 6 cases | All six modes round-trip through `EtwSession::start()`/`query_stats()` via synthetic mocks |
| `tests/serialization_tests.rs` (small) | 2 cases | `DegradationMode` derives are consistent (round-trip through serde JSON for IPC use later in Group C — wires up cleanly) |

Total: 11 tests in framesage-etw. Service crate gains 1 integration test asserting the build-gate-fallthrough log message format.

Tests at the **integration** level (real session against real Windows) come in week 3+ once the event parsers exist and there's something to assert against. Week 2's stop-gates rely on the spike binary as the integration-level proof for "the lifecycle works against real Windows" — we already have that data from Phase 1.

---

## 6. Stop gates within the week (cumulative)

Each day's stop gate is restated here so they're visible as a single checklist:

- **Day 1 stop:** if `RtlGetVersion` binding doesn't work as expected. Don't fall back to `GetVersionEx`.
- **Day 2 stop:** if the spike-to-production lift surfaces ANY behavioral delta on the same dev box.
- **Day 3 stop:** if the architecture's intended INFO log line conflicts with the actual formatter.
- **Day 4 stop:** if any of the six degradation-mode mocks turns out to be impossible without invasive code surgery — STOP and re-think structure.
- **Day 5 stop:** if any EOD verification check deviates from expected (especially stale session after shutdown).

Plus the **ground rules** carried from prior phases that remain in force:
- Spike-style reports include verification commands + literal output (PR #68 ground rule).
- No "accept known risk and ship" decisions without the user.
- No ground-rule waivers.
- No scope expansion past what's in the architecture or this plan.
- If a decision point comes up that wasn't anticipated here, STOP and surface; don't guess.

---

## 7. Acceptance criteria — week 2 specific

These are the explicit pass-conditions for "week 2 is complete":

- [ ] `crates/etw/` exists, registered in workspace `Cargo.toml`.
- [ ] `cargo check --workspace` green.
- [ ] `cargo fmt --check` green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` green (continuing the discipline from PR #71).
- [ ] `cargo test -p framesage-etw` green (11 cases listed in §5).
- [ ] `cargo test -p framesage-service` green (existing tests + 1 new integration test).
- [ ] `cargo build -p framesage-svc --release` green.
- [ ] Service binary verified on a Win11 26200 dev box per Day 5 EOD checklist; literal command outputs captured in `spike/group-a-week-2-report.md`.
- [ ] `EtwSession::start()` short-circuits cleanly on build < 26100 (verified via test, not just compiled).
- [ ] No new clippy `#[allow]`s introduced (any added must be justified per the PR #71 pattern).
- [ ] No scope creep against this plan — anything that surfaces mid-week and tempts a "while we're here" diff gets surfaced to the user instead of folded in.

NOT in this week's acceptance criteria (deferred to week 3+):
- The four parser-level criteria from `spike/etw-schemas.md` (DPC 0x42/0x44/0x45, HardFault 0x20, DiskIo prefix-only, PerfInfo 0x32 no-op) — week 3+ when parsers exist.
- The 24h consumer endurance test from architecture's Group A list — week 7.
- EDR matrix validation — v0.7.1 (per `spike/etw-edr-report.md` §6).

---

## 8. Explicitly out of scope for week 2

| Item | Belongs to |
|---|---|
| CSwitch / DPC / ISR / HardFault / DiskIo event parsing | Group A week 3+ |
| SPSC ring buffer for callback → drain | Group A week 3+ |
| 24h endurance test | Group A week 7 |
| EDR matrix validation (Defender ATP / CrowdStrike / S1) | v0.7.1, NOT v0.7 |
| PresentMon subprocess | Group B |
| Session recorder (JSONL on disk) | Group B |
| Sessions UI tab | Group C |
| Attribution UI | Group C |
| Signed binary | Group D |

If any of these tempts a "while we're here" diff during week 2 execution, STOP per the no-scope-creep ground rule and surface.

---

## 9. Risks called out

1. **`RtlGetVersion` linking.** `windows-rs` 0.58 may gate `RtlGetVersion` behind a feature flag we don't yet enable. Mitigation: Day 1's first action is to confirm the binding compiles before any other work; if it doesn't, the day's deliverable shifts to "figure out the right feature flag set."

2. **Spike-to-production lift surfacing a hidden invariant.** The spike binary works as a standalone exe; the production crate has slightly different lifetimes (a tokio runtime in the background, a service-supervisor task, named-pipe IPC fully separate). If the session callback's shared state needs an Arc that the spike just used a static for, that's a real refactor — call it out on Day 2 if it lands rather than papering.

3. **Mock injection design for degradation tests.** The six mode tests need to inject failures at different layers of the session module. If the chosen abstraction (likely a trait-object indirection on `StartTraceW`-like functions for test injection) creeps into the production hot path and slows it down, that's a real cost. Mitigation: only inject in `#[cfg(test)]` builds; production code uses the concrete Windows API directly. Day 4's stop gate explicitly checks for this.

4. **Engine integration timing.** The `SystemEvent` channel exists in `crates/engine/` already; week 2 doesn't introduce engine code. But the consumer-supervisor task in `crates/service/` does need to forward `DegradationEvent` into that channel. If the channel's shape isn't a clean match, the temptation will be to broaden `SystemEvent` "while we're here." DON'T. Surface the mismatch and decide explicitly.

5. **Windows build cache size on CI.** Adding a new crate with ETW feature flags will grow the Cargo build cache. If CI's "native build + test (windows)" job starts hitting the runner's disk quota, that's a separate fix-up PR — surface but don't bundle.

---

## 10. Reproduction instructions

To execute this plan once buddy approves:

```powershell
# From repo root, in elevated PowerShell on Win11 24H2+:
git checkout -b feat/group-a-week-2 origin/main
# ...day-by-day work per §4...
```

Each day's work commits incrementally on `feat/group-a-week-2`. End-of-week PR is opened with the full week's work + `spike/group-a-week-2-report.md` summarizing actual outcomes against the acceptance criteria in §7. That PR runs through buddy too — same three-question format as PR #71.

---

## 11. Buddy review record

**Reviewed by buddy-system agent on 2026-05-17.** Three-question format (same as PR #71):
- (a) Plan matches architecture + schema authority: **PASS** — build-gate value, six degradation modes, lifecycle lift scope all cross-checked against `audit/v0.7-architecture.md` §2.1 + `spike/etw-schemas.md` "implementation gates."
- (b) Scope correctness (no creep, no shrinkage, no substitution): **PASS** — every deliverable cites an authoritative source; the four parser-level criteria are correctly deferred to week 3+; EDR matrix correctly deferred to v0.7.1; ground rules honored.
- (c) Realistic stop gates + risks + daily feasibility: **PASS-WITH-NOTE** on Day 4 feasibility — see amendment below.

**Overall verdict: PROCEED.**

**Buddy's two notes, both applied as amendments to this DRAFT v2:**

1. **Crate naming.** First draft wrote `crates/framesage-etw/`. Workspace pattern is `crates/X/` → `framesage-X` (verified: `crates/service/` → `framesage-service`, `crates/sys/` → `framesage-sys`, `crates/core/` → `framesage-core`). Corrected to `crates/etw/` with `[package].name = "framesage-etw"` throughout this document.

2. **Day 4 de-risking.** The mock-injection trait indirection used by Day 4's six tests is now scaffolded on Day 3 (alongside the `EtwSubsystem` return-type refactor that touches the same call sites). Day 4 becomes "write test cases against an existing scaffold," not "build scaffold + tests in one day." Reduces the risk that Day 4 spills into Day 5's service-wiring time. Day 3 gains a stop gate for the abstraction being the wrong shape.

---

## Status: DRAFT v2 — buddy-approved with amendments applied; awaiting user sign-off

When the user signs off, this document moves from DRAFT v2 to APPROVED via a small follow-up PR that flips the header. Then execution starts on `feat/group-a-week-2`.
