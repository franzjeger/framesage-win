# v0.7 Group A — Week 2 implementation plan

**Status:** DRAFT v3 — user pushback on DRAFT v2 with five required fixes applied (see §11). Awaiting buddy re-review with the new four-question format (adds (d) internal consistency) and user sign-off before execution.
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

Workspace `Cargo.toml` gains `"crates/etw"` in `members`, and a new `framesage-etw = { path = "crates/etw" }` entry in `[workspace.dependencies]`.

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

### 3.4 Mock injection abstraction

**Why this section exists:** DRAFT v2 referenced "a `#[cfg(test)]`-gated trait indirection on the `StartTraceW` / `ControlTraceW` / `RtlGetVersion` call sites" without specifying the surface. User flagged this as a load-bearing gap in v3 instructions. This section is the concrete specification.

**Trait surface — `EtwSysCalls`:**

```rust
/// Indirection layer over the Windows ETW system calls that
/// `EtwSession` invokes. Production builds use `RealEtwSysCalls`
/// (a zero-sized type that compiles to direct windows-rs calls);
/// `#[cfg(test)]` builds substitute `MockEtwSysCalls` which scripts
/// return values per call site.
///
/// Generic over the impl type — production code is
/// `EtwSession<RealEtwSysCalls>` monomorphized at the call site,
/// NO dyn dispatch on the hot path. See "Generic vs dyn" below.
pub trait EtwSysCalls {
    /// `StartTraceW`. Returns `WIN32_ERROR` so tests can script
    /// `ERROR_ACCESS_DENIED` (Mode 1) and `ERROR_ALREADY_EXISTS`
    /// (Mode 2).
    fn start_trace(
        &self,
        session_handle: &mut CONTROLTRACE_HANDLE,
        session_name: PCWSTR,
        properties: *mut EVENT_TRACE_PROPERTIES,
    ) -> WIN32_ERROR;

    /// `ControlTraceW`. The control_code argument distinguishes
    /// QUERY (Mode 3 — RealTimeBuffersLost > 0), STOP (clean
    /// shutdown), and FLUSH paths. Tests can match on control_code
    /// to script per-usage-pattern responses.
    fn control_trace(
        &self,
        handle: CONTROLTRACE_HANDLE,
        session_name: PCWSTR,
        properties: *mut EVENT_TRACE_PROPERTIES,
        control_code: u32,
    ) -> WIN32_ERROR;

    /// `OpenTraceW`. Returns `PROCESSTRACE_HANDLE`; tests can
    /// return an invalid handle to simulate Open failure.
    fn open_trace(&self, logfile: *mut EVENT_TRACE_LOGFILEW) -> PROCESSTRACE_HANDLE;

    /// `ProcessTrace`. Blocks. In production, returns when the
    /// session is stopped via `ControlTraceW(STOP)`. Tests can
    /// return immediately with an injected status to exercise the
    /// consumer-thread teardown path.
    fn process_trace(
        &self,
        handles: &[PROCESSTRACE_HANDLE],
        start_time: *mut FILETIME,
        end_time: *mut FILETIME,
    ) -> WIN32_ERROR;

    /// `CloseTrace`. Tests assert this is called on every successful
    /// teardown path to verify we don't leak ETW handles.
    fn close_trace(&self, handle: PROCESSTRACE_HANDLE) -> WIN32_ERROR;

    /// `RtlGetVersion`. Tests can script different builds (22631
    /// for Win11 23H2 → Mode 6 BuildUnsupported, 26200 for the
    /// happy path) and the failure path (returns NTSTATUS error).
    fn rtl_get_version(&self, info: *mut OSVERSIONINFOEXW) -> NTSTATUS;
}
```

**Production impl — `RealEtwSysCalls`:**

```rust
/// Zero-sized type. All methods are `#[inline]` direct calls into
/// `windows::Win32::System::Diagnostics::Etw::*` (and
/// `windows::Win32::System::SystemInformation::RtlGetVersion`). The
/// compiler monomorphizes `EtwSession<RealEtwSysCalls>` at the call
/// site; the trait indirection produces identical codegen to a
/// direct call in release builds.
pub struct RealEtwSysCalls;

impl EtwSysCalls for RealEtwSysCalls {
    #[inline]
    fn start_trace(
        &self,
        session_handle: &mut CONTROLTRACE_HANDLE,
        session_name: PCWSTR,
        properties: *mut EVENT_TRACE_PROPERTIES,
    ) -> WIN32_ERROR {
        unsafe { windows::Win32::System::Diagnostics::Etw::StartTraceW(session_handle, session_name, properties) }
    }
    // ... other methods identical pattern ...
}
```

**Day 3 verification of codegen-parity (per v3 user instruction):**

Day 3's deliverable includes confirming via `cargo asm` (or equivalent — `cargo rustc --release -- --emit=asm` and grep) that `EtwSession::start::<RealEtwSysCalls>` produces identical assembly to a hypothetical direct-call version that doesn't use the trait. If it doesn't, the abstraction is the wrong shape and Day 3's stop gate fires.

**Test impl — `MockEtwSysCalls` with per-method scripted queue:**

```rust
/// Test-only. Each method has its own scripted-return queue;
/// tests push the expected return values into the queue before
/// invoking the code-under-test. RefCell because tests are
/// single-threaded; the queue mutation isn't a concurrency hazard
/// in tests, and using Mutex would impose ordering constraints
/// that don't exist in production.
#[cfg(test)]
pub struct MockEtwSysCalls {
    start_trace_returns: RefCell<VecDeque<WIN32_ERROR>>,
    control_trace_returns: RefCell<VecDeque<WIN32_ERROR>>,
    open_trace_returns: RefCell<VecDeque<PROCESSTRACE_HANDLE>>,
    process_trace_returns: RefCell<VecDeque<WIN32_ERROR>>,
    close_trace_returns: RefCell<VecDeque<WIN32_ERROR>>,
    rtl_get_version_returns: RefCell<VecDeque<(NTSTATUS, OSVERSIONINFOEXW)>>,
    // Call counts per method, for assertions like "cleanup was attempted".
    call_counts: RefCell<HashMap<&'static str, usize>>,
}

impl MockEtwSysCalls {
    pub fn new() -> Self { /* empty queues */ }
    pub fn expect_start_trace(&self, ret: WIN32_ERROR) { /* push to queue */ }
    pub fn expect_control_trace(&self, ret: WIN32_ERROR) { /* push */ }
    // ... etc per method ...
    pub fn call_count(&self, method: &str) -> usize { /* lookup */ }
}
```

**Decision: per-method scripted queue, NOT state machine.** Per v3 user instruction ("Buddy must approve the choice before Day 3 begins"). Rationale: tests script per-call-site failures (Mode 1 is `start_trace` returning `ERROR_ACCESS_DENIED`; Mode 2 is two `start_trace` calls — the cleanup-and-retry — with the second returning `ERROR_ALREADY_EXISTS`). A state machine would couple all the methods and force tests to specify global transitions for behaviors they don't care about; per-method queues let each test touch only the methods relevant to its mode.

**Generic vs dyn — explicit position:**

Production code uses `EtwSession<S: EtwSysCalls>` monomorphized at the call site. **No `dyn EtwSysCalls` anywhere in the production hot path.** The trait exists solely to substitute a different impl in `#[cfg(test)]` builds. Using `dyn` would impose virtual-dispatch cost on every kernel-event callback (millions per second under load) for zero production benefit — the type is statically known at compile time.

The `EtwSession` struct's type parameter defaults to `RealEtwSysCalls` so production callers write `EtwSession::start(opts)` without naming the type:

```rust
pub struct EtwSession<S: EtwSysCalls = RealEtwSysCalls> { /* ... */ }
impl<S: EtwSysCalls + Default> EtwSession<S> {
    pub fn start(opts: SessionOptions) -> Result<EtwSubsystem<S>> { /* ... */ }
}
```

Tests opt in to the mock with `EtwSession::<MockEtwSysCalls>::start_with_mock(mock, opts)` or equivalent.

### 3.5 Consumer-thread panic-channel mechanism

**Why this section exists:** DRAFT v2's Mode 5 (ConsumerPanic) test said "report the panic via the appropriate channel (logged + event emitted, not silently swallowed)" without specifying what that channel is. User flagged this as a load-bearing gap in v3 instructions. This section is the concrete specification.

**The mechanism:**

1. The consumer thread's entry point wraps its body in `std::panic::catch_unwind`:

   ```rust
   let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
       consumer_loop(state)
   }));
   match result {
       Ok(()) => {
           // Normal completion (session stopped externally).
           exit_tx.send(ConsumerExitReason::CleanShutdown).ok();
       }
       Err(panic_payload) => {
           // Extract a string from the payload if possible.
           let msg = panic_payload.downcast_ref::<&str>().copied()
               .or_else(|| panic_payload.downcast_ref::<String>().map(|s| s.as_str()))
               .unwrap_or("(panic payload not a string)").to_owned();
           exit_tx.send(ConsumerExitReason::Panicked { message: msg }).ok();
       }
   }
   ```

2. The channel is a **tokio `oneshot::channel<ConsumerExitReason>`** — the consumer thread exits exactly once, so oneshot fits the cardinality. The receiver is owned by the consumer-supervisor task in `crates/service/` (created on Day 5).

3. The supervisor's `tokio::select!` loop:
   - Polls the drop-rate query loop (1-second interval).
   - Awaits the oneshot for consumer exit.
   - When the oneshot fires with `Panicked { message }`:
     - Logs at ERROR level with the panic message + a stack trace if available.
     - Emits `DegradationMode::ConsumerPanic` into the existing `SystemEvent` channel (engine listens; Group C UI surfaces the banner).
     - Tears down the session cleanly (calls `EtwSession::stop()` on the cached handle).
     - Transitions the `EtwSubsystem` to `Disabled(ConsumerPanic)` in supervisor-local state.
     - **Does NOT exit the service host.** The engine continues running v0.6 static-rule mode.

4. **UnwindSafe analysis:**
   - The consumer thread's captured state is `Arc<ConsumerState>` where `ConsumerState` holds (a) atomic counters (`AtomicU64` — `UnwindSafe`), (b) the ETW session handle (`CONTROLTRACE_HANDLE` is a wrapper around a `usize`, `UnwindSafe`), (c) a clone of the syscall impl `S: EtwSysCalls` (`RealEtwSysCalls` is ZST, `UnwindSafe`).
   - **`AssertUnwindSafe` is used at the catch_unwind site** because the closure captures `state: Arc<ConsumerState>` — `Arc<T>` is `UnwindSafe` only if `T: RefUnwindSafe`. `ConsumerState` contains only `AtomicU64` and handles (both `RefUnwindSafe`), so the assert is sound. Documented inline in `session.rs` with the audit ground rule explaining the soundness.
   - If a future change adds a field to `ConsumerState` that is NOT `RefUnwindSafe` (e.g. a `Cell<T>` or a custom mutex), the assert becomes unsound. A `static_assertions::assert_impl_all!(ConsumerState: RefUnwindSafe)` test in `tests/build_gate_tests.rs` (since it's a compile-time assert, location is arbitrary) catches this regression at the next build.

5. **Conflict with architecture §2.1 mode 5 — surfaced for follow-up architecture amendment:**

   Architecture §2.1's mode 5 row currently reads:
   > "Consumer thread panics → Service exits non-zero. SCM restarts via the FailureActions config we already ship."

   The mechanism specified above does NOT exit the service — only the ETW subsystem transitions to `Disabled`, and the engine continues running v0.6 static-rule mode. This is a **deliberate design change** in v3 (per user instruction): a one-time panic in the consumer shouldn't crash the whole service when the rule-engine half can still serve. The behavior is closer to the architecture's other degradation modes (1-4) which already leave the service running.

   **Required follow-up:** before Day 5's service-wiring code lands on main, a separate architecture amendment PR updates §2.1 mode 5 to:
   > "Consumer thread panics → EtwSubsystem transitions to Disabled(ConsumerPanic). Service host stays up; engine continues in v0.6 static-rule mode. Tray surfaces banner via existing degradation-event channel. SCM restart not required."

   This amendment lands as its own small PR. Day 5's code references the amended §2.1.

6. **Coexistence with v0.6 Group 1's FailureActions + tick-task watchdog:**
   - FailureActions still applies to *service-host* crashes (the SCM-restart mechanism for cases where the service binary actually exits non-zero — out-of-memory, unrecoverable panics in the engine's tick task, etc.).
   - The consumer-thread panic path is a separate code path inside the still-running service. FailureActions does NOT fire because the service host doesn't exit.
   - The tick-task watchdog (v0.6 Group 1 item) is independent of the ETW consumer; the engine's tick task runs whether or not the ETW subsystem is alive. If the tick task itself panics, the existing v0.6 behavior applies (service-host exit + SCM restart).

**Day 4 Mode 5 test asserts this concrete mechanism:**

- Inject a panic into the consumer thread (via `MockEtwSysCalls::process_trace` returning a sentinel that causes the consumer loop to call `panic!("test injection")`, or via a direct `panic!` injection point in the consumer body gated on `#[cfg(test)]`).
- Assert that the oneshot fires with `ConsumerExitReason::Panicked { message }` where `message.contains("test injection")`.
- Assert that the supervisor emits `DegradationMode::ConsumerPanic` into the test-mode `SystemEvent` channel.
- Assert that the service-host process is still alive after the panic (in unit-test scope: the test runner doesn't exit; in integration scope: a follow-up week-3+ test on real Windows).
- Assert that `CloseTrace` was called on teardown (via mock call-count).

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

**Day 3 also scaffolds the mock-injection abstraction per §3.4:** the `EtwSysCalls` trait, the `RealEtwSysCalls` zero-sized production impl, the `#[cfg(test)] MockEtwSysCalls` with per-method scripted queues, and the generic `EtwSession<S: EtwSysCalls = RealEtwSysCalls>` type parameter. Day 3 verifies via `cargo asm` (or equivalent) that `EtwSession::start::<RealEtwSysCalls>` produces identical assembly to a hypothetical direct-call version in release builds — that's the codegen-parity check §3.4 specifies.

**Day 3 ALSO scaffolds the consumer-thread panic-channel mechanism per §3.5:** the `tokio::sync::oneshot` channel for `ConsumerExitReason`, the `std::panic::catch_unwind` wrapper around the consumer body (with `AssertUnwindSafe` and the `static_assertions` regression-guard), and the supervisor-side select-loop pattern. The full supervisor task lands on Day 5; Day 3 just lays the consumer-side primitives so Day 4's Mode 5 test has something to assert against.

**Day 3 also opens (NOT lands) the architecture-amendment follow-up PR for §2.1 mode 5** per §3.5 #5 ("Conflict with architecture §2.1 mode 5 — surfaced for follow-up architecture amendment"). The amendment changes the mode 5 disposition from "service exits non-zero, SCM restarts" to "ETW subsystem disabled, service stays up." That amendment PR can land before, during, or after Day 4 — but it MUST land before Day 5's service-wiring code merges to main.

**Stop gates:**
- If the architecture's intended log line conflicts with the actual `tracing` formatter (rare but possible — line breaks, format-string mismatch), STOP and propose a doc-level fix to the architecture rather than diverging silently.
- If the trait-indirection abstraction is the wrong shape (e.g. introduces lifetime gymnastics, requires dyn-dispatch on the hot path even in production builds, or leaks `cfg(test)` symbols into the public API), STOP and re-think before Day 4 commits more code on top of it.
- **If `cargo asm` shows the trait indirection does NOT produce identical release codegen** to a direct-call version, STOP and surface — the `EtwSysCalls` abstraction is either wrong or the compiler isn't monomorphizing as expected (LTO config issue, missing `#[inline]`, etc.).
- **If the user rejects the architecture §2.1 mode 5 amendment** (i.e. they want the original "service exits + SCM restart" semantics), STOP — the §3.5 design needs reworking to match the architecture's stated behavior before Day 4's Mode 5 test is written.

### Day 4 — degradation-mode unit tests (against Day-3 scaffold)

**Deliverable:** `tests/degradation_tests.rs` contains six tests, one per `DegradationMode` variant. The mock-injection scaffold from Day 3 is the substrate; Day 4 is writing test cases against it, NOT building the scaffold from scratch. Each test injects a synthetic failure at the appropriate layer:
- **Mode 1 (AccessDenied):** `StartTraceW` mock returns `ERROR_ACCESS_DENIED`. Assert `start()` returns `EtwSubsystem::Disabled(AccessDenied)`. NOT against a real EDR — that's a v0.7.1 gate.
- **Mode 2 (AlreadyExists):** mock returns `ERROR_ALREADY_EXISTS` even after `cleanup_stale_session()`. Assert disabled-with-`AlreadyExists`. Verify cleanup was attempted (call count).
- **Mode 3 (KernelDrops):** `query_stats` mock returns `RealTimeBuffersLost = 5`. Assert that a `DegradationEvent::KernelDrops { rate }` is emitted on the next poll cycle.
- **Mode 4 (OurDrops):** the ring buffer doesn't exist yet (week 3+), so this test is a placeholder that asserts the mode exists and serializes correctly. The full path test ships with the ring buffer.
- **Mode 5 (ConsumerPanic):** exercise the mechanism specified in §3.5. Inject a panic into the consumer thread (via the `#[cfg(test)]` injection point gated behind a `MockEtwSysCalls`-returned sentinel, OR via a direct `panic!` injection in the consumer body's test-only branch). Assert (a) the `tokio::sync::oneshot::channel<ConsumerExitReason>` fires with `Panicked { message }` where `message.contains("test injection")`, (b) the supervisor emits `DegradationMode::ConsumerPanic` into the test-mode `SystemEvent` channel, (c) the service-host process is still alive after the panic (unit-test scope: the test runner doesn't exit; in integration scope on real Windows, that's a week-3+ follow-up test), (d) `CloseTrace` was called on teardown (via `MockEtwSysCalls::call_count("close_trace") == 1`).
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
| `tests/build_gate_tests.rs` | 3 cases | Predicate returns true ≥ 26100, false at 22631, false on `RtlGetVersion` failure. Plus the compile-time `static_assertions::assert_impl_all!(ConsumerState: RefUnwindSafe)` guard from §3.5 #4. |
| `tests/degradation_tests.rs` | 6 cases | All six modes round-trip through `EtwSession::start()`/`query_stats()` via the `MockEtwSysCalls` substrate from §3.4 |

Total: 9 tests in framesage-etw (3 build-gate + 6 degradation). Service crate gains 1 integration test asserting the build-gate-fallthrough log message format.

**Cut from DRAFT v2 (per v3 fix #5):** `tests/serialization_tests.rs` (2 cases for `DegradationMode` serde round-trip). Rationale: the crate has no `serde` dependency at this point (the §2 Cargo.toml block doesn't list it), and IPC consumption of `DegradationMode` is a Group C concern — Group C will add the IPC-shape tests when the IPC surface actually consumes the type. Premature testing here would force a `serde` dependency that has no production use yet.

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
- [ ] `cargo test -p framesage-etw` green (9 cases listed in §5: 3 build-gate + 6 degradation).
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

Each day's work commits incrementally on `feat/group-a-week-2`. End-of-week PR is opened with the full week's work + `spike/group-a-week-2-report.md` (template per §12) summarizing actual outcomes against the acceptance criteria in §7. That PR runs through buddy too — using the **four-question format** introduced in this v3 (the three questions from PR #71 plus (d) internal consistency).

---

## 11. Buddy review record

**Reviewed by buddy-system agent on 2026-05-17.** Three-question format (same as PR #71):
- (a) Plan matches architecture + schema authority: **PASS** — build-gate value, six degradation modes, lifecycle lift scope all cross-checked against `audit/v0.7-architecture.md` §2.1 + `spike/etw-schemas.md` "implementation gates."
- (b) Scope correctness (no creep, no shrinkage, no substitution): **PASS** — every deliverable cites an authoritative source; the four parser-level criteria are correctly deferred to week 3+; EDR matrix correctly deferred to v0.7.1; ground rules honored.
- (c) Realistic stop gates + risks + daily feasibility: **PASS-WITH-NOTE** on Day 4 feasibility — see amendment below.

**Overall verdict: PROCEED.**

**Buddy's two notes from the v1→v2 round, both applied:**

1. **Crate naming.** First draft wrote `crates/framesage-etw/`. Workspace pattern is `crates/X/` → `framesage-X` (verified: `crates/service/` → `framesage-service`, `crates/sys/` → `framesage-sys`, `crates/core/` → `framesage-core`). Corrected to `crates/etw/` with `[package].name = "framesage-etw"` throughout this document.

2. **Day 4 de-risking.** The mock-injection trait indirection used by Day 4's six tests is now scaffolded on Day 3 (alongside the `EtwSubsystem` return-type refactor that touches the same call sites). Day 4 becomes "write test cases against an existing scaffold," not "build scaffold + tests in one day." Reduces the risk that Day 4 spills into Day 5's service-wiring time. Day 3 gains a stop gate for the abstraction being the wrong shape.

### User pushback on DRAFT v2 (2026-05-17)

User reviewed DRAFT v2 and rejected it pending five fixes. All applied here in DRAFT v3:

1. **§3.4 (NEW) — Mock injection abstraction.** v2 referenced the trait indirection without specifying its surface. v3 adds the full `EtwSysCalls` trait with six methods (one per intercepted call site: `start_trace`, `control_trace`, `open_trace`, `process_trace`, `close_trace`, `rtl_get_version`), the `RealEtwSysCalls` zero-sized production impl with `#[inline]` direct windows-rs calls, the `MockEtwSysCalls` test impl using per-method `RefCell<VecDeque<...>>` scripted queues (not state machine — explicit decision recorded in §3.4), and the generic `EtwSession<S: EtwSysCalls = RealEtwSysCalls>` monomorphization (NO dyn dispatch on the hot path). Day 3 verifies codegen-parity via `cargo asm`. Buddy must approve the per-method-queue choice in the v3 review.

2. **§3.5 (NEW) — Consumer-thread panic-channel mechanism.** v2's Mode 5 test said "report via the appropriate channel" without specifying. v3 names the mechanism: `std::panic::catch_unwind` with `AssertUnwindSafe` wrapper, `tokio::sync::oneshot::channel<ConsumerExitReason>` for the panic signal, supervisor-side `tokio::select!` that emits `DegradationMode::ConsumerPanic` and tears down the session cleanly. **Service host stays up; engine continues in v0.6 static-rule mode.** This is a deliberate design change from architecture §2.1 mode 5's "service exits non-zero, SCM restarts" — surfaced in §3.5 #5 as a required architecture amendment that must land BEFORE Day 5's service-wiring code. A `static_assertions::assert_impl_all!(ConsumerState: RefUnwindSafe)` regression guard prevents future changes from silently making the `AssertUnwindSafe` unsound.

3. **§2 directory-name inconsistency fixed.** v2's amendment caught the directory at the top of §2 but missed the workspace-`Cargo.toml`-additions paragraph at the bottom, which still referenced `crates/framesage-etw`. v3 fixes that paragraph to `crates/etw`. This is the exact class of error the new buddy question (d) — internal consistency — is designed to catch.

4. **§12 (NEW) — EOD week-2 report template.** v2 said "summarizing actual outcomes against the acceptance criteria in §7" without specifying the report's structure. v3 adds §12 with required sections mirroring Phase 1's spike report verbatim where applicable.

5. **Serialization tests cut.** v2 listed a `tests/serialization_tests.rs` with 2 cases for `DegradationMode` serde round-trip. The crate had no `serde` dependency. v3 cuts the test and the row from §5; Group C adds IPC-shape tests when the IPC surface actually consumes the type. §7 acceptance criterion updated from "11 cases" to "9 cases." Premature testing eliminated.

### Buddy four-question format introduced in v3

User instruction (2026-05-17): the buddy review format gains a fourth question:

> **(d) Internal consistency** — does the plan agree with itself across sections? Are referenced section numbers, file paths, type names, and acceptance-criteria items consistent throughout?

This catches the class of error that produced the §2 directory slip in v2 (buddy passed (a) "plan matches architecture" but didn't check the plan against itself). The four-question format applies retroactively to this planning-phase review (v3 will be re-reviewed with all four questions) and going forward to the implementation-phase reviews (week 2 EOD PR, week 3, etc.).

---

## 12. End-of-week report template — `spike/group-a-week-2-report.md`

Created at EOD Day 5 as the week's deliverable doc. Mirrors `spike/etw-report.md` (Phase 1)'s structure verbatim where applicable. **Required sections, in order:**

### 12.1 Environment attestation

Per the PR #68 ground rule ("spike reports include verification commands and their literal output, not just conclusions"). Capture and paste literal output of:

```text
PS> [System.Environment]::OSVersion.Version
PS> (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -Name UBR).UBR
PS> Get-Service framesage
PS> sc.exe query WinDefend          # or equivalent third-party AV product
PS> Get-MpComputerStatus            # if Defender platform is installed
```

Plus a one-line statement of which dev box, which user account, elevation status (LocalSystem? elevated user?).

### 12.2 Per-day deliverable status

A table with one row per day (Day 1 through Day 5):

| Day | Planned deliverable (§4) | Actual outcome | Stop gate triggered? | Notes |
|---|---|---|---|---|

"Actual outcome" cells either say "matched" or list the deviation. If a deviation, link to the audit-trail entry in `audit/buddy-disagreements.md` or surface in §12.6 below.

### 12.3 Test results inventory

```text
PS> cargo test -p framesage-etw -- --nocapture
   (paste full literal output)

PS> cargo test -p framesage-service
   (paste full literal output)

PS> cargo clippy --workspace --all-targets -- -D warnings
   (paste full literal output)

PS> cargo fmt --check
   (paste — should be empty / exit 0)
```

State explicitly whether the 9 framesage-etw cases + 1 service integration test passed. If any failed, surface in §12.6.

### 12.4 EOD verification checklist (Day 5)

Repeat the five-step checklist from §4 Day 5 with literal command outputs:

1. `Get-Service framesage` output for `closed_loop_enabled: false` (static-rule path)
2. `logman query FramesageEtw -ets` output for `closed_loop_enabled: true` after restart (session running)
3. `logman query FramesageEtw -ets` output after `Stop-Service framesage` (session gone, no leftover)
4. `cargo test -p framesage-etw -- --nocapture` output (already in §12.3 — cross-reference)
5. INFO-log line showing the build-gate path on a Win11 26200 (build is supported; closed-loop initializes) AND on a synthetically-mocked unsupported build (static-rule fallback log line)

### 12.5 Stop-gate trip log

A table of which stop gates fired during the week:

| Day | Stop gate (§6) | Triggered? | If yes: disposition |
|---|---|---|---|

Expected outcome: none triggered. If any triggered, the disposition column says "surfaced to user as PR #N comment," "blocked Day N+1 until user decision," etc.

### 12.6 Deviations from plan

If §12.2 had any "actual outcome != planned" rows, this section is the prose explanation. Each deviation gets: (a) what was planned, (b) what actually happened, (c) why, (d) what was surfaced to the user and when, (e) what the resolution was. **Default expectation: none.** If this section ends up populated, the week-2 EOD PR is NOT a rubber-stamp merge; it's a discussion PR.

### 12.7 Recommendation

Section parallels Phase 1 spike report's "Recommendation" section. State:
- GO / NO-GO on proceeding to week 3.
- If GO: which week-3 ground rules carry forward (e.g. the buddy four-question format).
- If NO-GO: what specifically blocks week 3 (with a reference to the audit/buddy-disagreements.md entry).

### 12.8 Appendix A — How to reproduce

```powershell
# From repo root, elevated PowerShell on Win11 24H2+:
git checkout feat/group-a-week-2  # or whatever the merged commit is
cargo build -p framesage-svc --release
# (install instructions — same as v0.6)
# (validation commands — same as §12.4)
```

---

## Status: DRAFT v3 — five user-fix amendments applied; awaiting buddy re-review (4-question format) and user sign-off

When buddy approves and the user signs off, this document moves from DRAFT v3 to APPROVED via a small follow-up PR that flips the header. Then execution starts on `feat/group-a-week-2`.

The architecture-amendment PR for §2.1 mode 5 (per §3.5 #5) lands as a SEPARATE small PR before Day 5's service-wiring code.
