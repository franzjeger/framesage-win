# framesage-win — 2026 Comprehensive Revision (Phase 2 Findings)

**Engagement:** "5-star app that just works" — senior-staff comprehensive revision
**Date:** 2026-05-18
**Audited revision:** main @ HEAD (post PR #79, post v0.7 Group A week 2 merge)
**Codebase size:** ~31,076 LOC across 11 workspace crates
**Auditor:** Sonnet 4.5
**Scope:** Code quality / Architecture / Correctness / Concurrency / Testing / UX-UI / Performance / Security / Distribution / Killer features / Removal. Audits the **built** code AND pre-audits the **unbuilt** v0.7 Group B/C/D plans where the architecture has committed text.

---

## Phase 1 outcome (confirmed by user, recorded here for traceability)

- Existing audit tradition (SUMMARY.md + 89 findings; PHASE2-PLAN.md 43 items in Groups 1–4) is **honest**: 5 randomly-sampled "closed" findings spot-checked PASS. Findings closed by prior groups can be trusted without re-verification.
- Product positioning per `audit/PHASE2-PLAN.md` §"Product positioning" is locked: aggression IS the feature; the only safety bar is (a) don't destabilize OS / AV / anti-cheat / GPU / RPC, (b) don't lie about what will happen, (c) reversible via journal.
- v0.7 architecture (`audit/v0.7-architecture.md`) is approved and partially implemented — ETW Group A weeks 1–2 merged (PR #79); weeks 3–7 (parsers, ring buffer, drain) + Groups B/C/D unbuilt.
- "Closed-loop default-off, EDR-matrix is v0.7.1 gate not v0.7 ship gate" (Option-D from PR #68) is the binding ship contract — no re-litigation.

---

## Severity schema

Findings use the following severity ladder. Severity reflects **time-to-fix urgency**, not impact alone — a HIGH issue is worse than a MEDIUM only if it cannot wait.

| Severity | Meaning | Time-to-fix |
|---|---|---|
| **BLOCKER-v0.7** | Ships broken or breaches the locked safety bar; must close before tagging v0.7 | Days |
| **BLOCKER-v0.7.1** | Ships broken specifically for the closed-loop default-on flip (EDR / signing / parser layer); must close before v0.7.1 | Weeks |
| **HIGH** | Substantial user pain, capability gap vs prior audit's stated goals, or trust regression. Not a ship-blocker but should land in the first 30-day window | 1–4 weeks |
| **MEDIUM** | Quality / robustness / DX issue. Should land before the 1.0 milestone | 4–12 weeks |
| **LOW** | Polish, future-proofing, or removal | Opportunistic |

Each finding carries:
- **Evidence** — file:line + quoted code (verbatim, ≤20 LOC per cite)
- **Impact** — what breaks, who feels it
- **Fix** — concrete next action
- **Effort** — S (≤1 day) / M (2–3 days) / L (~1 week) / XL (multi-week)
- **Gate** — which milestone closes it
- `[UNVERIFIED]` annotation on any Windows-API behavioral claim without an MS-docs / spike / experimental reference. The user explicitly required this marker.
- `CONFLICT-WITH-AUDIT-POSITION` annotation when a finding contradicts an existing audit decision — paired with a specific counter-argument and a proposed disagreement-log update.

Findings IDs use axis-letter + 3-digit numbers, e.g. `D-001` for the first concurrency finding.

---

## Executive summary

framesage-win has reached the point where most of the **shape** is right and the marginal returns shift to **risk-management**. The audit tradition (SUMMARY.md → PHASE2-PLAN.md → grouped execution → buddy review) is the project's biggest asset and should be preserved verbatim. The findings below are not a vote of no-confidence — they're the next layer of the same audit work the project already runs on itself.

The dominant patterns in the new findings:

1. **The ETW crate is the new high-risk surface.** Six BLOCKER/HIGH items below (D-001..D-006, C-001..C-002) sit inside ~2,500 LOC of new code with substantial `unsafe`, raw-pointer / FFI lifetime obligations, and a 1-shot real-hardware validation budget. The crate is well-commented and the trait-seam test architecture is correct, but the validated-on-mocks→production gap is real and the production deployment depends on getting it right the first time.
2. **The v0.6 safety story is largely complete and well-tested.** Groups 1–3 closed everything from C-01 (kernel-write safe-list gate) through M-29 (tray module extraction). Spot-checks confirm those are real. v0.6 ship readiness is high.
3. **v0.7 Group B/C/D have plans but no code.** The architecture text in `audit/v0.7-architecture.md` is detailed — most of the risk in those groups is _execution risk_ (PresentMon subprocess management, sessions JSONL schema correctness, attribution honesty-band thresholds). The pre-audit below surfaces specific gaps in the plans themselves where the unbuilt code would inherit a load-bearing ambiguity.
4. **First-run + onboarding cover the EDR / closed-loop disclosure**, but the unbuilt Sessions-tab empty state has hard requirements (architecture §2.4) that the current tray scaffold does not yet enforce in code. Section J-002 flags this as a structural risk for the v0.7 ship.
5. **Distribution and ops are still pre-1.0** — no Authenticode signing, no MSI, no signed installer verification, no EDR-matrix attestation. These are not v0.7 BLOCKERs because the ship contract explicitly punted them to v0.7.1, but they are BLOCKER-v0.7.1 items, and the v0.7.1 schedule should be cut against this list.
6. **Killer features**: the project's marquee differentiator is the closed-loop honesty contract (architecture §2.4) — the asymmetric +/- bands that surface negative attribution prominently. The unbuilt Group C code path is where that contract becomes real. The findings flag the specific tests that, if absent, would invalidate the differentiator.

---

# AXIS A — Code quality (Rust idioms, unsafe SAFETY, errors, API design)

## A-001 — `EtwSession::stop` and `query_stats` use `.expect()` panics for "called after stop / decomposition" — recoverable misuse panics in a service library

**Severity:** MEDIUM
**Evidence:** `crates/etw/src/session.rs:806-809`, `:828`, `:909-916`, `:925-929`, `:1092-1095`:

```rust
let syscalls = self
    .syscalls
    .take()
    .expect("EtwSession::stop called twice or after decomposition");
```

Four call sites in EtwSession + SessionShutdownHandle use `.expect("…twice…")` to enforce one-shot semantics. The Drop fallbacks correctly handle the missing-syscalls case via `let Some(syscalls) = self.syscalls.take() else { return; };`, but the explicit-API paths panic.

**Impact:** A future caller that holds an `EtwSession` and calls `stop()` twice — or calls `query_stats()` after handing the session to `SupervisorLoop` — turns a logic bug into a service-host panic. The `catch_unwind` guard in the consumer thread doesn't catch panics in the calling thread (the runtime task). Tokio's task panic-handling depends on the runtime config.

**Fix:** Convert each `.expect("…")` to `Result<…, AlreadyStoppedError>` and document that the explicit API is one-shot. The Drop fallback remains the leak-prevention net; the API surface stops being panic-on-misuse.

**Effort:** S
**Gate:** Month 1

## A-002 — `EtwSubsystem` enum hides retryability — caller cannot tell `Disabled(AccessDenied)` from `Disabled(AlreadyExists)` without matching on the inner `DegradationMode`

**Severity:** LOW
**Evidence:** `crates/etw/src/session.rs:617-623`, `:720-726`:

```rust
pub enum EtwSubsystem<S: EtwSysCalls = RealEtwSysCalls> {
    Running(EtwSession<S>),
    Disabled(DegradationMode),
}
```

The "Disabled" branch carries a `DegradationMode` payload but the type itself is opaque to the caller about retry policy. `AccessDenied` (EDR blocked us, must opt out) and `AlreadyExists` (another consumer holds the name, retry might work) are both `Disabled` from the call site.

**Impact:** Currently `closed_loop.rs:172-179` matches `Disabled(mode)` once and emits an info log; no retry. Acceptable for v0.7 but invites a future "retry on AlreadyExists with a different session name" path to land in the wrong layer.

**Fix:** Introduce `EtwSubsystem::DisabledRetryable(mode)` vs `EtwSubsystem::DisabledFatal(mode)` discriminants, OR keep the current shape and document retry policy in the `DegradationMode` enum's per-variant doc-comments.

**Effort:** S
**Gate:** Month 2

## A-003 — `closed_loop.rs::detect_anti_cheats` false-positives on `bf6.exe`

**Severity:** HIGH
**Evidence:** `crates/sys/src/inner/ac_detect.rs:56-58`:

```rust
// BF6 / Javelin process companion (best-effort surface; the actual
// Javelin driver detection is deferred).
(AcMarker::Javelin, "bf6.exe"),
```

`bf6.exe` is a too-generic exe name. Multiple homebrew utilities, modders' tools, and unrelated games-named-with-numbers risk colliding. The case-insensitive match (`exe_name.eq_ignore_ascii_case`) makes it worse on Windows (any user-renamed binary trivially hits).

**Impact:** False detection of Javelin AC will cause the engine to surface the wrong AC-tier banner and (per `engine/lib.rs:3376`'s `ac_safe_mode_target` logic, lines 3376–3406) suppress per-game-process modifications for any rule with `ac_safe_mode_target: Hybrid`. A user running an unrelated `bf6.exe` (mod loader, batch script, named copy) finds their rules silently suppressed.

**Fix:** Either (a) tighten the marker to require a co-located `bf6.exe` AND a known EA Javelin service / driver, or (b) defer the Javelin probe entirely (the comment already says the dedicated probe needs more reverse engineering) and remove the bf6.exe marker. Option (b) is the conservative call.

**Effort:** S
**Gate:** Week 1

## A-004 — `consumer_loop` casts `Arc::as_ptr(&state)` to `*mut c_void` and parks it in `EVENT_TRACE_LOGFILEW::Context` — relies on `state` Arc staying alive through `ProcessTrace`

**Severity:** MEDIUM (currently sound; comment-only fragility)
**Evidence:** `crates/etw/src/session.rs:1309-1322`, `:1345-1358`:

```rust
logfile.Context = Arc::as_ptr(&state) as *mut std::ffi::c_void;

// SAFETY: logfile.LoggerName lives for the OpenTraceW call;
// PROCESS_TRACE_MODE_REAL_TIME → resolve by name.
let handle = unsafe { syscalls.open_trace(&mut logfile) };
```

The callback at line 1345 dereferences `er.UserContext as *const ConsumerState` and reads `state.events_seen`. The contract relies on:
1. ETW kernel-side guarantees callbacks fire only during `ProcessTrace`
2. The `state: Arc<ConsumerState>` local in `consumer_loop` outlives `ProcessTrace`
3. After `ProcessTrace` returns (STOP issued), no more callbacks

All three hold in practice [UNVERIFIED — MSDN documents ProcessTrace as synchronous but doesn't explicitly guarantee no in-flight callbacks at return]. The comment at :1320 ("state Arc keeps ConsumerState alive across every callback invocation") understates the dependency on point 3.

**Impact:** If a future refactor moves the Arc construction out of the local frame (e.g., into an enclosing struct that drops before `ProcessTrace` returns), a use-after-free vector opens. The compile-time guard (`static_assertions::assert_impl_all!(ConsumerState: RefUnwindSafe)` in `supervisor.rs:158`) doesn't cover this.

**Fix:** Either (a) add a doc-comment block in `consumer_loop` explicitly enumerating the three contract points and a "do not move the Arc" reviewer-instruction, OR (b) hold the Arc inside a typestate that statically prevents being dropped before `ProcessTrace` returns. Option (a) is the cheap correct call.

**Effort:** S
**Gate:** Week 1

## A-005 — Service-side `Subscribe` cap (32) uses a process-wide `AtomicUsize` instead of per-PID counting; comment acknowledges the simplification

**Severity:** LOW
**Evidence:** `crates/service/src/runtime.rs:884-924`:

```rust
static ACTIVE_SUBSCRIBES: AtomicUsize = AtomicUsize::new(0);
const MAX_SUBSCRIBES: usize = 32;
// […]
// The counter is process-wide rather than per-PID because per-PID would
// require plumbing `GetNamedPipeClientProcessId` through every
// accept; if 32 connections are actually open from one PID that's
// already pathological.
```

**Impact:** A single misbehaving client (e.g. a CLI script in a loop) trivially exhausts the cap for all clients. The comment acknowledges this as "already pathological"; that's defensible but a real annoyance during dev iteration with multiple CLIs open.

**Fix:** Per-PID cap of 4-8 via `GetNamedPipeClientProcessId` [UNVERIFIED — windows-rs binding present per `framesage_sys` but caller plumbing absent]. Defer to v0.7.1.

**Effort:** S
**Gate:** Month 2

## A-006 — `validate_policy_against_safe_list` allocates a fresh `SafeList::bundled()` per call

**Severity:** LOW
**Evidence:** `crates/service/src/runtime.rs:1022-1023`:

```rust
fn validate_policy_against_safe_list(policy: &Policy) -> Vec<String> {
    let safe_list = framesage_gamemode::safe_list::SafeList::bundled();
```

Called on every `SetPolicy` IPC request. `SafeList::bundled()` is presumed cheap (likely `lazy_static`-style); not verified.

**Impact:** Negligible at IPC rate. Listed for completeness.

**Fix:** Take `&'static SafeList` parameter; pass `Engine::deps.safe_list` from the caller.

**Effort:** S
**Gate:** Month 3

## A-007 — `EtwSession<S>` uses `Option<S>` for the `syscalls` field as a sentinel for "already stopped"; correct, but the .take()-on-Drop pattern repeats verbatim across `EtwSession` and `SessionShutdownHandle`

**Severity:** LOW
**Evidence:** `crates/etw/src/session.rs:647-656`, `:1074-1075`, `:957-994` (Drop), `:1143-1170` (Drop):

Two near-identical Drop impls each take `self.syscalls.take()` as the "already shut down" sentinel. The shape is correct but copy-pasted.

**Impact:** Future divergence risk — a STOP-options change in one Drop misses the other.

**Fix:** Extract `SessionShutdownCore { handle, name, syscalls: Option<S> }` and let both types hold one; share the Drop. (Both types currently have different `session_name` representations — `String` vs `Vec<u16>` — which adds a small refactor cost.)

**Effort:** M
**Gate:** Month 3

# AXIS B — Architecture (boundaries, layering, resource lifecycle, testability)

## B-001 — Closed-loop subsystem startup happens on the tokio runtime worker thread inside `runtime::run`; `EtwSession::start` blocks synchronously through `StartTraceW` + stale-session-cleanup + `std::thread::Builder::spawn`

**Severity:** MEDIUM
**Evidence:** `crates/service/src/runtime.rs:107`:

```rust
let closed_loop_startup = crate::closed_loop::start_closed_loop_if_enabled(&policy);
```

`start_closed_loop_if_enabled` is synchronous; it calls `EtwSession::start(opts)` which executes `cleanup_stale_session` → `StartTraceW` → `std::thread::Builder::spawn` (lines 704–786 of `session.rs`). On a fresh service start with no stale sessions this is fast (< 50ms typical for `StartTraceW` per spike report). On a host with a stuck prior session it can be slower: `cleanup_stale_session` does a synchronous `ControlTraceW(STOP)` against the kernel.

**Impact:** Service-start latency budget hits the tokio worker pool. Acceptable for startup; would be unacceptable for hot-path restarts (when the supervisor decides to relaunch closed-loop, e.g., on policy hot-reload flipping `closed_loop_enabled: false → true`). The current code does not support that path, but the architecture allows it.

**Fix:** Wrap `EtwSession::start` in `tokio::task::spawn_blocking` at the call site. Doesn't help startup latency directly, but parks the sync work where it belongs (blocking pool) and makes future "restart on policy flip" trivially correct.

**Effort:** S
**Gate:** Week 1

## B-002 — Closed-loop tasks deliberately excluded from the v0.6 watchdog `select!` — design correct per architecture §2.1 mode 5 amendment, but the comment trail is the only enforcement

**Severity:** LOW (acknowledged design; flagged for surface-area awareness)
**Evidence:** `crates/service/src/closed_loop.rs:8-23` (module docstring), `crates/service/src/runtime.rs:100-111`, `:196-203`:

```rust
let unexpected_exit: Option<&'static str> = tokio::select! {
    _ = shutdown => None,
    r = &mut tick_handle => Some(task_died_msg("tick", &r)),
    r = &mut admin_handle => Some(task_died_msg("admin-ipc", &r)),
    r = &mut status_handle => Some(task_died_msg("status-ipc", &r)),
    r = &mut reload_handle => Some(task_died_msg("policy-watcher", &r)),
    r = &mut sys_handle => Some(task_died_msg("system-events", &r)),
};
```

The supervisor + drop-poll tasks are spawned via `tokio::spawn` inside `spawn_closed_loop_tasks` (`closed_loop.rs:218-256`) and their handles are dropped. The architecture §2.1 mode 5 amendment (PR #77) says this is correct: supervisor exit is NOT a critical service failure.

**Impact:** Currently correct, but the architecture invariant ("closed-loop task crashes do not crash the service") is enforced only by code-structure and comments, not by any test. A future "add closed-loop to the watchdog for symmetry" patch is a real risk.

**Fix:** Add a unit/integration test that asserts the `tokio::select!` arm set does NOT contain a closed-loop task handle. Lightweight; could be a `#[test]` that compiles a string-grep over `runtime.rs:select!` block.

**Effort:** S
**Gate:** Month 1

## B-003 — `framesage-etw` crate is correctly zero-framesage-deps, but the syscall-seam trait pattern is duplicated rather than shared with `framesage-sys`'s `SysApi` trait

**Severity:** LOW
**Evidence:** `crates/etw/src/session.rs:141-218` (`EtwSysCalls` trait), `crates/sys/src/api.rs` (`SysApi` trait — read in prior context but not in this turn). Two seam traits for the same testability pattern.

**Impact:** Coding the same trait pattern twice is fine; the cost is consistency drift (a future engineer optimizes one but not the other). The crates have different deps (etw is bottom-of-stack, sys is mid), so co-locating the abstractions isn't trivially right either.

**Fix:** Document the seam-trait pattern in `audit/v0.7-architecture.md` or a new `docs/syscall-seam-pattern.md` so the next subsystem (Group B PresentMon adapter, Group C session recorder) uses the same shape.

**Effort:** S
**Gate:** Month 1

## B-004 — Engine's `check_process_modifiable` is correctly applied at apply paths (six call sites at lines 652, 683, 704, 725, 746, 798 + line 3366); SafeList is enforced consistently at IPC and policy boundaries

**Severity:** N/A — credit, not a finding
**Evidence:** `crates/engine/src/lib.rs:3451-3475`:

```rust
fn check_process_modifiable(
    safe_list: &'static SafeList,
    exe_name: &str,
    action: &str,
) -> Result<()> {
    use framesage_gamemode::safe_list::ProcessVerdict;
    match safe_list.check_process(exe_name) {
        ProcessVerdict::Denied(reason) => { /* … */ }
        // Allowed or Unlisted both pass — the denylist is the only authority.
```

Plus `runtime.rs:1022-1052` (validate_policy_against_safe_list) gates SetPolicy server-side. **This is the Theme A fix from SUMMARY.md done correctly** — closes C-01, C-02, C-03, H-17.

## B-005 — `framesage-recorder` and `framesage-presentmon` crates are not yet present in the workspace despite architecture §2.5 listing them as new

**Severity:** LOW (planned, unbuilt)
**Evidence:** `Cargo.toml:3-22`:

```toml
members = [
    "crates/core",
    "crates/sys",
    "crates/ipc",
    "crates/engine",
    "crates/service",
    "crates/cli",
    "crates/tray",
    "crates/sim",
    "crates/gamemode",
    "crates/spike-etw",
    "crates/etw",
]
```

`audit/v0.7-architecture.md:1310-1318` lists `framesage-presentmon` (~600 LOC) and `framesage-recorder` (~600 LOC) as new crates. Neither exists.

**Impact:** Expected — Group B/C unbuilt. Listed so the Phase 3 roadmap captures these as the next two crates to scaffold.

**Fix:** Phase 3 roadmap item; see roadmap §Month 1.

**Effort:** L (each crate)
**Gate:** Group B / Group C kickoff

# AXIS C — Correctness & robustness (ETW lifetime, sleep/resume, AC interference)

## C-001 — `EtwSession::stop` calls `verify_session_gone` after `consumer_join.join()`, but `query_session_stats` re-issues `ControlTraceW(QUERY)` against a possibly-not-yet-fully-deregistered kernel session — flake risk

**Severity:** HIGH
**Evidence:** `crates/etw/src/session.rs:801-822`:

```rust
pub fn stop(mut self) -> Result<()> {
    let syscalls = self.syscalls.take().expect("…");
    stop_session(&syscalls, &self.session_name)?;
    if let Some(handle) = self.consumer_join.take() {
        if let Err(panic_payload) = handle.join() { /* … */ }
    }
    verify_session_gone(&syscalls, &self.session_name)?;
```

`verify_session_gone` calls `query_session_stats` and bails if the QUERY succeeds, with a message asserting "architecture §2.1 'survives service restarts' invariant violated" (line 1237). On a real kernel, `ControlTraceW(STOP)` is documented as synchronous for the session-detach but there is no formal guarantee about when the session-registry-row is fully reclaimed [UNVERIFIED — MSDN's `ControlTraceW` page doesn't pin down the registry-row reclaim timing].

**Impact:** Tests / production calls to `stop()` may flake: STOP returns success, consumer joins, but the QUERY in `verify_session_gone` races the kernel's row-reclaim and finds the session still listed. The bail message would mis-attribute a transient race to an invariant violation.

**Fix:** Either (a) drop the verify step (the STOP RC is sufficient — the architecture invariant is "next StartTraceW succeeds", not "QUERY immediately fails"), OR (b) replace with a retry loop with bounded timeout (~50ms × 5 attempts). Option (a) is the cleaner call; the next start path already has its own `cleanup_stale_session` retry.

**Effort:** S
**Gate:** Week 1

## C-002 — ETW Mode-3 drop-poll task self-terminates only on `query_session_stats` error; if the supervisor calls `shutdown.shutdown()` quickly enough, the monitor's first poll might still succeed against the just-detached session and emit a spurious `KernelDrops` event with the final-tick stats

**Severity:** MEDIUM
**Evidence:** `crates/service/src/closed_loop.rs:231-255`:

```rust
tokio::spawn(async move {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        match monitor.poll_drop_stats(|ev: DegradationEvent| { /* error! */ }) {
            Ok(_stats) => { /* fine */ }
            Err(e) => { warn!(/* … */); break; }
        }
    }
});
```

The supervisor calls `shutdown.shutdown()` synchronously on the panic path (`supervisor.rs:116`). The drop-poll task ticks on a 1Hz interval. If the supervisor's STOP completes between two ticks, the next `poll_drop_stats` call sees the session already gone and errors out cleanly — fine. But there's a window where the QUERY succeeds with a stale-but-non-zero `real_time_buffers_lost`, emitting one final `KernelDrops` event after the session is logically dead.

**Impact:** Log noise; possibly confusing telemetry if a future Group-C UI banner consumes the event stream. Not a correctness violation — the event reflects the final session state.

**Fix:** The supervisor and monitor should share a "session torn down" boolean (e.g., `Arc<AtomicBool>`); supervisor sets on `shutdown()`, monitor checks and exits before polling.

**Effort:** S
**Gate:** Month 1

## C-003 — `framesage-etw::session::EtwSession::start` opens the consumer thread BEFORE the supervisor task is wired; if the consumer panics in its first iteration, the panic-reason oneshot fires but no supervisor is listening yet

**Severity:** MEDIUM
**Evidence:** `crates/etw/src/session.rs:743-787`:

```rust
let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
let consumer_join = std::thread::Builder::new()
    .name("etw-consumer".into())
    .spawn(move || { /* … catch_unwind, send reason, … */ })?;
```

`exit_rx` is returned in the `EtwSession`. Caller (`closed_loop.rs:198-199`) then calls `into_supervisable_parts_with_monitor()` which transfers `exit_rx` to the `SupervisorLoop`. The window between `EtwSession::start` returning `Running(s)` and `supervisor.run().await` actually awaiting the oneshot is small but non-zero. If the consumer thread panics in that window, the `exit_tx.send(reason)` succeeds (oneshot is buffered) and the supervisor will see the reason when it eventually awaits.

**Impact:** Actually OK on closer reading — `oneshot::channel()` buffers one message. The supervisor receives the panic reason whenever it awaits. False alarm; documented here for the audit-trail.

**Fix:** None needed. Documented as PASS-after-analysis.

**Effort:** N/A
**Gate:** N/A

## C-004 — Engine's manual-global Game Mode (`enable_manual_global_game_mode` / `disable_manual_global_game_mode`) interactions with foreground-driven Game Mode are documented in `PHASE2-PLAN.md` §2.11 ("manual wins, focus-driven is suppressed") but the implementation surface needs a dedicated test for the interaction matrix

**Severity:** MEDIUM
**Evidence:** `crates/service/src/runtime.rs:649-666` (IPC verbs), engine-side implementation at `engine/lib.rs` (not re-read this turn). Plan acceptance criterion at `PHASE2-PLAN.md:271-273`: "Activate from tray menu, alt-tab around, confirm taskbar stays hidden + services stay stopped. Deactivate, confirm restore."

The plan's verification is a manual checklist. The interaction matrix (manual active + foreground game launches + manual disabled mid-session + manual+foreground both targeting different profiles) has > 6 cells.

**Impact:** Untested interactions surface as user-visible state divergence — e.g., manual disabled mid-session reverts the wrong subset of actions if the foreground rule also fired.

**Fix:** Add a `tests/` integration test in `crates/engine` driving the 6-cell interaction matrix against the `SysApi` mock. Tests `Engine::enable_manual_global_game_mode` × `Engine::report_foreground` × `Engine::disable_manual_global_game_mode` orderings.

**Effort:** M
**Gate:** Month 1

## C-005 — Pre-audit: architecture §2.4 "Did FrameSage help?" attribution honesty contract specifies five named threshold tests (`compute_attribution_summary(session_with_p99_delta(-9%))` → "improved your 1% lows", etc.). If Group C ships without these tests, the honesty contract is unenforced

**Severity:** BLOCKER-v0.7.1
**Evidence:** `audit/v0.7-architecture.md:1164-1180`:

```
The honesty contract unit test (see Phase 3 Group C acceptance)
asserts these specific thresholds:
- `compute_attribution_summary(session_with_p99_delta(-9%))` →
  rendered string contains `"improved your 1% lows"`
- `compute_attribution_summary(session_with_p99_delta(-6%))` →
  rendered string contains `"Modest improvement"`
- `compute_attribution_summary(session_with_p99_delta(0%))` →
  rendered string contains `"No measurable effect"`
- `compute_attribution_summary(session_with_p99_delta(+4%))` →
  rendered string contains `"Slight regression"` (yellow)
- `compute_attribution_summary(session_with_p99_delta(+6%))` →
  rendered string contains `"**degraded**"` verbatim
```

Group C acceptance criterion at `audit/v0.7-architecture.md:1631-1634` requires these. The Group C deliverable is unbuilt.

**Impact:** The product's marquee differentiator (honest attribution, asymmetric bands) depends on the rendered-string contents. Without tests, a future copy edit drops "**degraded**" verbatim and breaks the contract silently.

**Fix:** Phase 3 Month 2: when Group C scaffolds, the first five tests are these. Treat as gating PR-review item; no Group C PR merges without all five present and green.

**Effort:** S (per test, once the function exists)
**Gate:** v0.7.1

# AXIS D — Concurrency (Send/Sync, atomic ordering, catch_unwind, races)

## D-001 — `consumer_loop` uses `AssertUnwindSafe(|| consumer_loop(…))` wrapping the syscalls value, but the only `RefUnwindSafe` static-assert is on `ConsumerState` — production `RealEtwSysCalls` is ZST (trivially safe) but the test mock contains `RefCell` (not `RefUnwindSafe`)

**Severity:** MEDIUM
**Evidence:** `crates/etw/src/session.rs:760-766`:

```rust
let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    consumer_loop(
        &consumer_name_for_panic,
        consumer_state_for_panic,
        consumer_syscalls_for_panic,
    )
}));
```

`crates/etw/src/supervisor.rs:158-164`:

```rust
const _: () = {
    use static_assertions::assert_impl_all;
    use crate::session::ConsumerState;
    assert_impl_all!(ConsumerState: std::panic::RefUnwindSafe);
};
```

The static_assert covers only `ConsumerState`. The closure additionally captures `consumer_syscalls: S` which for `MockEtwSysCalls` contains `RefCell<VecDeque<…>>` — not `RefUnwindSafe`. The comment at session.rs:752-756 acknowledges this and waves it through ("test mock only invoked sequentially from this thread, no mid-call panic windows").

**Impact:** Specifically: if a test panics WHILE the mock is mid-`borrow_mut().pop_front()`, the `RefCell` borrow leak makes subsequent calls panic with "already borrowed". The mock at session.rs:443-478 takes `borrow_mut()` on three RefCells inside `control_trace`; a panic between those borrows leaves the mock unusable. The `panic_in_process_trace` flag (line 364) IS armed to panic AT the start of `process_trace` — outside any RefCell borrow window — so the documented mock-panic test path is safe. But the audit-honest call is: the bound is silent, not enforced.

**Fix:** Extend the static_assert to: `assert_impl_all!(RealEtwSysCalls: std::panic::RefUnwindSafe);` (production ZST trivially satisfies) and gate `MockEtwSysCalls` panic-injection points to "panic before any borrow_mut" via inline comment + lint.

**Effort:** S
**Gate:** Month 1

## D-002 — `event_record_callback` (`extern "system"`) is `unsafe fn` but lacks an explicit `#[no_mangle]` or `extern "system"` re-check; the ABI-stamp at definition site is correct, but a future refactor that drops to `extern "C"` would silently corrupt the calling convention

**Severity:** LOW (currently correct)
**Evidence:** `crates/etw/src/session.rs:1345-1359`:

```rust
unsafe extern "system" fn event_record_callback(event_record: *mut EVENT_RECORD) {
    if event_record.is_null() { return; }
    let er = unsafe { &*event_record };
    let ctx = er.UserContext as *const ConsumerState;
    if ctx.is_null() { return; }
    let state = unsafe { &*ctx };
    state.events_seen.fetch_add(1, Ordering::Relaxed);
}
```

ABI is `"system"` (correct for ETW callbacks on Windows) [UNVERIFIED — MSDN's ETW callback prototype on Win64 uses `__stdcall` which is `extern "system"` in Rust on Win32 and `extern "C"` on Win64 since x64 has only one calling convention; `extern "system"` is the safe alias]. Not `#[no_mangle]` because it's only used via function pointer.

**Impact:** None today. Drift risk only.

**Fix:** Comment-only — add `// ABI is "system" because ETW callbacks are __stdcall on Win32 and the unified x64 convention on Win64; do not change.` above the fn signature.

**Effort:** S
**Gate:** Month 3

## D-003 — Service-side `Subscribe` cap uses `AtomicUsize::fetch_add` + check-then-fetch_sub on cap-exceeded path — small TOCTOU window where N=32 concurrent calls could briefly push the count above the cap before any of them notices

**Severity:** LOW
**Evidence:** `crates/service/src/runtime.rs:893-895`:

```rust
let prev = ACTIVE_SUBSCRIBES.fetch_add(1, Ordering::Relaxed);
if prev >= MAX_SUBSCRIBES {
    ACTIVE_SUBSCRIBES.fetch_sub(1, Ordering::Relaxed);
```

If 100 callers each fetch_add concurrently, all 100 see prev≥cap, all fetch_sub. The count overshoots and recovers. The cap functions as a soft limit.

**Impact:** Negligible — cap is approximate by design. The next legitimate Subscribe call sees the decremented counter.

**Fix:** Use compare-exchange loop for a hard cap, OR document the soft-cap semantics inline. The latter is the lighter option.

**Effort:** S
**Gate:** Month 3

## D-004 — `consumer_loop`'s `state.events_seen.fetch_add(1, Ordering::Relaxed)` is sound (per-event counter, no read-side ordering dependency) but `query_stats` reads with `Relaxed` and compares to `real_time_buffers_lost` — readers cannot reason about the temporal ordering

**Severity:** LOW
**Evidence:** `crates/etw/src/session.rs:824-836`:

```rust
pub fn query_stats(&self) -> Result<SessionStats> {
    let syscalls = self.syscalls.as_ref().expect("…");
    let q = query_session_stats(syscalls, &self.session_name)?;
    Ok(SessionStats {
        events_lost: q.events_lost,
        real_time_buffers_lost: q.real_time_buffers_lost,
        buffers_written: q.buffers_written,
        events_seen: self.state.events_seen.load(Ordering::Relaxed),
    })
}
```

`events_seen` is monotonic-increasing, written by one thread (consumer), read by another (the monitor task / supervisor). Relaxed is correct for monotone counters. The architecture cost: a reader can see a value that's "slightly behind" the kernel's lost/buffers counters. For a stats display this is acceptable; if the value were ever used to drive an invariant ("if events_seen > 0, then no drops should be possible") it would not be.

**Impact:** None today; load-bearing only on future invariants.

**Fix:** Comment-only — `// Relaxed is correct here because events_seen is a monotone counter; readers tolerate a slight lag.` above the fetch_add.

**Effort:** S
**Gate:** Month 3

## D-005 — `closed_loop.rs::CLOSED_LOOP_BUILD_OVERRIDE` is `thread_local!` and `#[cfg(test)]`-gated; the production path correctly compiles it out, but the `BuildOverrideGuard::set` does NOT reset on panic-from-WITHIN the guard's scope without RAII Drop — Drop is implemented but a panic UNWIND back through `set()` would still leave the override poisoned

**Severity:** N/A (false alarm on re-read)
**Evidence:** `crates/service/src/closed_loop.rs:60-73`:

```rust
pub(crate) struct BuildOverrideGuard;
impl BuildOverrideGuard {
    pub(crate) fn set(v: Option<Result<u32, ()>>) -> Self {
        CLOSED_LOOP_BUILD_OVERRIDE.with(|c| *c.borrow_mut() = v);
        Self
    }
}
impl Drop for BuildOverrideGuard {
    fn drop(&mut self) {
        CLOSED_LOOP_BUILD_OVERRIDE.with(|c| *c.borrow_mut() = None);
    }
}
```

`Drop` fires on panic-unwind. The override clears on either test pass or test panic. Correct as written.

**Impact:** None. Documented as PASS.

**Fix:** None.

**Effort:** N/A
**Gate:** N/A

## D-006 — `framesage-engine` mutex / sync surface untested for the manual-global Game Mode lock interactions

**Severity:** MEDIUM (related to C-004)
**Evidence:** Implementation files (`engine/lib.rs:enable_manual_global_game_mode`, `:disable_manual_global_game_mode`, `:apply_once`) not re-read in this turn; the IPC verbs at `runtime.rs:649-666` and the test set at `engine/lib.rs` (per prior context: 53 tests) cover the new manual-global path's success cases but the lock-ordering with `system_mode` lock is not separately tested.

**Impact:** A future "tick re-enters apply during manual disable" code path could deadlock.

**Fix:** Tied to C-004's interaction matrix — add a "manual-disable mid-tick" stress test that drives `enable → tick → disable → tick` with synthetic events.

**Effort:** M
**Gate:** Month 1

# AXIS E — Testing (coverage, test-asserts-what-it-says, flakiness)

## E-001 — `framesage-etw::supervisor::supervisor_emits_consumer_panic_event_and_calls_shutdown` test uses a SYNTHETIC oneshot to drive the panic path — does not exercise the real consumer-thread → catch_unwind → oneshot flow

**Severity:** HIGH
**Evidence:** `crates/etw/src/supervisor.rs:258-265`:

```rust
drop(exit_rx);
let (synthetic_tx, synthetic_rx) = oneshot::channel();
synthetic_tx
    .send(ConsumerExitReason::Panicked {
        message: "synthetic test panic".to_string(),
    })
    .expect("oneshot send");
```

The test author's own comment at lines 249-257 acknowledges:
> This makes the test test the SupervisorLoop's panic-handling
> path in isolation, not the full consumer-thread → supervisor
> flow. The full-flow test runs in the end-of-week Windows
> batch (real ETW + real catch_unwind).

**Impact:** The real consumer-thread panic-handling path is unverified by automated tests. A regression in `consumer_loop`'s `catch_unwind` wrapper (e.g., a future refactor that wraps the wrong subset of the closure) would not be caught by this test.

**Fix:** Add a test that uses `MockEtwSysCalls::arm_panic_in_process_trace("synthetic")` (the infrastructure already exists at `session.rs:413-418`), starts a real `EtwSession` via mock, and asserts the supervisor receives a `Panicked { message }` with the synthetic message via the REAL oneshot. The plan §4 Day 4 already specifies this path (Mode 5 test); the "end-of-week Windows batch" approach left it as deferred.

**Effort:** M
**Gate:** Week 1 (this is the test that was explicitly deferred and now needs to run)

**Resolution (2026-05-18, W1.5 / #84):** Test already exists at
`crates/etw/src/session.rs:1718` as `mode_5_session_level_full_flow_panic`
(landed in PR #79 / Group A week 2 at 2026-05-18 16:09:37; this finding
was written 3h 40m later at 2026-05-18 19:49:56 in PR #117 without a
grep-verify against current main). Test passes on Win11 24H2; exercises
exactly the path E-001 specified — `MockEtwSysCalls::arm_panic_in_process_trace`
→ real `start_with_syscalls` → `into_supervisable_parts` → real `catch_unwind`
fires in consumer thread → real oneshot delivers Panicked → SupervisorLoop
receives + on_event extracts via `DegradationEvent.detail`.

W1.5 (#84) was repurposed from "add the test" to "audit correction":
(a) this Resolution block, (b) stale-comment fix at `supervisor.rs:254-257`
pointing at the existing test, (c) new E-005 finding for the
downcast::<&str>-coverage gap discovered during W1.5 survey, (d) P4.9
process improvement against future audit-vs-codebase drifts. See
`audit/buddy-disagreements.md` PR #84-closure entry. Issue #84 closed
via prior resolution.

## E-002 — `tracing-test::traced_test` integration tests in `closed_loop.rs` use substring-matching against rendered output, not structured field assertion

**Severity:** LOW (acknowledged in source)
**Evidence:** `crates/service/src/closed_loop.rs:280-296`:

```rust
// Note `tracing-test::logs_contain` does substring-match on the
// rendered output by default. The structured-field assertion
// approach would require a custom tracing subscriber; for week 2
// scope, asserting key substrings + the level via `logs_contain`
// is sufficient.
```

**Impact:** Test breaks on tracing-format upgrades (e.g., if the project ever moves to JSON-formatted logs).

**Fix:** Document the test fragility as a known item or implement a custom subscriber. Defer to v0.7.1 — current tests are functional.

**Effort:** S
**Gate:** Month 3

## E-003 — Engine test count (53 per prior context) is strong for the v0.6 surface; the v0.7 ETW crate has supervisor/build_gate/session unit tests but the **full ProcessTrace mock path** is unverified end-to-end (per E-001)

**Severity:** MEDIUM
**Evidence:** Direct line counts from this turn:
- `crates/etw/src/build_gate.rs:202 LOC` — has inline `mod tests`
- `crates/etw/src/degradation.rs:135 LOC` — likely tests inline
- `crates/etw/src/session.rs:1817 LOC` — has `#[cfg(test)] mod mock` at line 301 + likely inline tests elsewhere
- `crates/etw/src/supervisor.rs:282 LOC` — has 3 tests (mod tests:168-282)

**Impact:** The crate is well-tested at the unit-seam level (mocked syscalls). The integration gap is the end-to-end `EtwSession::start → consumer_loop → panic → supervisor receives → shutdown` path on a real kernel.

**Fix:** Phase 3 Group A weeks 3–7 (parsers + ring buffer + drain) will exercise the real-kernel path. The Step 21–28 manual-validation log captured in `spike/group-a-week-2-report.md` §12.4 covers part of this on the dev host, but the test set should include at least one CI-runnable real-ETW path on Windows runners (per `audit/v0.7-architecture.md` §"EDR interaction" — TESTING IS LOAD-BEARING).

**Effort:** L
**Gate:** Group A week 3 kickoff

## E-004 — No "negative result" session has been intentionally produced + verified to surface the YELLOW "**degraded**" banner per architecture §2.4 — Group C acceptance criterion at `v0.7-architecture.md:1631-1634` requires this manually

**Severity:** BLOCKER-v0.7.1
**Evidence:** `audit/v0.7-architecture.md:1631-1634`:

```
- [ ] Negative-result session intentionally produced (apply a
      bad profile) and verified that the UI shows red attribution
      banner
```

**Impact:** Without this manual verification, the honesty-contract test set (C-005) covers the function-level threshold mapping but not the end-to-end "user can see and act on a regression" experience.

**Fix:** Phase 3 Group C acceptance gate; produce a "bad" profile (high-mask-affinity on Intel hybrid E-cores, for instance) and run a 15-minute Valorant session against it.

**Effort:** M (deliberate-bad-profile build + 1 game session + screenshot)
**Gate:** v0.7.1

## E-005 — Existing `mode_5_session_level_full_flow_panic` test exercises String-payload catch_unwind extraction branch only; the `downcast_ref::<&str>` branch is uncovered

**Severity:** MEDIUM
**Evidence:** `crates/etw/src/session.rs:1720-1767` (existing test) uses
`mock.arm_panic_in_process_trace("synthetic test panic — Day 4 Mode 5")`.
The mock's `process_trace` at `crates/etw/src/session.rs:500-501` then
calls `panic!("{}", msg)` (formatted, produces a `String` panic payload
per Rust's `panic!` macro behavior with format args).

The catch_unwind extraction logic at `crates/etw/src/session.rs:775-783`
has TWO branches:

```rust
let msg = payload
    .downcast_ref::<&str>()        // Branch 1: tested only by &'static str panics
    .copied()
    .map(str::to_owned)
    .or_else(|| payload.downcast_ref::<String>().cloned())  // Branch 2: covered by existing test
    .unwrap_or_else(|| "(panic payload not a string)".to_string());
```

Branch 1 (raw `&'static str` payload from `panic!("static literal")` with
no format args) is uncovered.

**Impact:** A regression in the `downcast_ref::<&str>` branch (e.g., a
future refactor that drops the `.copied().map(str::to_owned)` chain)
would not be caught by any test. Production consumers can panic with
either payload shape; the catch_unwind extraction must handle both.

**Fix:** Add a second test alongside `mode_5_session_level_full_flow_panic`
that exercises the &'static str path. Suggested implementation: add an
`arm_direct_panic_in_process_trace(&self, message: &'static str)`
companion method on MockEtwSysCalls that arms a direct `panic!(msg)`
(no format args, payload stays as &'static str) — or any equivalent
approach that exercises the &str-payload extraction path (e.g.,
`std::panic::panic_any` with a `&'static str` payload). ~30 LOC: one
new arm method (or equivalent) + one new test mirroring
`mode_5_session_level_full_flow_panic`.

**Effort:** S
**Gate:** Month 1
**Discovered:** 2026-05-18 during W1.5 (#84) survey — found via
re-reading the catch_unwind extraction logic at session.rs:775-783
while drafting the E-001 resolution. Cross-references: E-001 Resolution
above; `audit/buddy-disagreements.md` PR #84-closure entry; P4.9
process note.

# AXIS F — UX/UI (first-run, config discoverability, error messages)

## F-001 — Onboarding wizard exists at `crates/tray/src/onboarding.rs` with three-tier aggression choice (Aggressive / Balanced / PinningOnly), but does NOT yet include the architecture §2.4 closed-loop opt-in page (page 3)

**Severity:** BLOCKER-v0.7
**Evidence:** `crates/tray/src/onboarding.rs:7-21`:

```rust
//! Pages:
//!   1. **What FrameSage is** — the verbatim product-positioning
//!      statement that also appears in README.md (item 4.14).
//!      Continue button.
//!   2. **Choose your level** — three radio options (Aggressive /
//!      Balanced / Pinning-only) […]
//!   3. **Manual Game Mode hotkey** — brief intro to the manual
//!      global toggle (item 2.11). […]
//!   4. **Done** — confirmation card […]
```

Compare `audit/v0.7-architecture.md:1354-1425` — the page 3 spec is the EDR-disclosure + closed-loop opt-in page, with required-substring "EDR validation in progress for v0.7.1" and radio buttons for Enable / Leave-disabled.

**Impact:** The architecture commits to "first-run onboarding gains a new page 3 (closed-loop opt-in with EDR-implications disclosure)" as the explicit mechanism that protects EDR-managed users. Shipping v0.7 without it means corporate-laptop / EDR users can flip closed_loop_enabled in Settings without seeing the EDR disclosure — directly contravenes architecture decision #4 + Phase 2 sign-off resolution #4.

**Fix:** Insert a new page (was-3, becomes-3, current-3 becomes-4, current-4 becomes-5) between "Choose your level" and "Manual Game Mode hotkey." Include the required substring per architecture §2.4 Group C acceptance criterion.

**Effort:** M
**Gate:** v0.7 (BLOCKER)

## F-002 — Sessions tab is unbuilt; the architecture §2.4 specifies two distinct empty-state variants (build-unsupported vs. no-sessions-yet) with hard-required substrings; current tray has no Sessions tab

**Severity:** BLOCKER-v0.7
**Evidence:** `audit/v0.7-architecture.md:900-1034` specifies the tab + both empty states. Group C acceptance criterion at `:976-985` and `:1030-1034` requires reviewer-rejection of PRs that ship the wrong empty state.

`crates/tray/src/main.rs` (5215 LOC per prior count) does not contain a `Sessions` tab variant in the `Tab` enum [UNVERIFIED — not re-grep'd this turn; based on file size unchanged from prior context].

**Impact:** Same as F-001 — the architecture commits to it; absence ships a broken closed-loop story.

**Fix:** Group C deliverable — scaffold the tab with both empty-state variants. Use the architecture §2.4 substring requirements as code-comment "DO NOT CHANGE" markers.

**Effort:** L
**Gate:** v0.7 (BLOCKER if closed_loop_enabled is exposed); v0.7.1 if Sessions tab is feature-flagged off in v0.7

**CONFLICT-WITH-AUDIT-POSITION potential:** The current Phase 2 plan does not list F-002 as a Group-1 BLOCKER. The architecture document says (§2.4) the Sessions tab is required as part of v0.7. If the project intends to ship v0.7 with `closed_loop_enabled` toggleable in Settings AND no Sessions tab, that's an architecture-decision violation. Recommend: either (a) defer `closed_loop_enabled` UI exposure to v0.7.1 (architecture decision #4 says default-off; UI surface could be deferred too), OR (b) scaffold the Sessions tab in v0.7 with the empty-state copy as the only working surface (no list/detail views yet) — meets the architecture letter, defers the load-bearing list-and-attribution code to v0.7.1.

## F-003 — Service-side error messages on `SetPolicy` rejection are good (`crates/service/src/runtime.rs:733-742`) — surface the rationale string per safe-list entry. The tray UI rendering of these errors is not audited this turn

**Severity:** N/A — note for tray review
**Evidence:** `crates/service/src/runtime.rs:733-742`:

```rust
&Response::Error {
    message: format!(
        "SetPolicy rejected: {} denylisted entries — these processes / \
         services are on the framesage safety denylist (kernel-critical, \
         antivirus, anti-cheat) and cannot be touched regardless of \
         profile content:\n  {}",
        denied.len(),
        denied.join("\n  "),
    ),
},
```

Multi-line strings via `\n` joining. Tray needs to render multi-line errors readably.

**Fix:** Audit tray's `Response::Error` rendering for multi-line support; if absent, render in a scrollable text area or "expand for details" affordance.

**Effort:** S
**Gate:** Month 1

## F-004 — Tray `parking_lot::Mutex` migration (per PHASE2-PLAN.md item 3.2) status is unverified this turn — the audit credit at SUMMARY.md "What's already good" claims the migration; need a quick grep to confirm zero `std::sync::Mutex` remains in tray

**Severity:** LOW (status check, not a finding)
**Evidence:** Not re-checked this turn. PHASE2-PLAN.md item 3.2 says "Switch `crates/tray/src/main.rs` from `std::sync::Mutex` to `parking_lot::Mutex`."

**Fix:** Trivial verification — Phase 3 roadmap drops this into a "verify-before-removing-from-backlog" item.

**Effort:** S
**Gate:** Month 1

# AXIS G — Performance (allocations, sustained event-rate, memory growth)

## G-001 — Drop-poll task at 1Hz performs synchronous `ControlTraceW(QUERY)` inside a tokio worker tick — blocks tokio runtime worker for the syscall duration

**Severity:** LOW
**Evidence:** `crates/service/src/closed_loop.rs:231-255` — see B-001 / C-002 above.

**Impact:** `ControlTraceW(QUERY)` is typically fast (≤1ms on a healthy session). At 1Hz, this is a minute fraction of a tokio worker's available time. Not a perf issue today; flagged for awareness if the poll rate is ever bumped to 10Hz or higher.

**Fix:** Wrap in `tokio::task::spawn_blocking` if the rate ever increases.

**Effort:** S
**Gate:** Month 3

## G-002 — `consumer_loop`'s `event_record_callback` does `fetch_add(1, Relaxed)` per kernel event — at 100k events/sec the atomic-contention cost is non-zero but bounded; the bigger cost is the unconditional `if ctx.is_null()` check + pointer-cast per call

**Severity:** LOW
**Evidence:** `crates/etw/src/session.rs:1345-1359`:

```rust
unsafe extern "system" fn event_record_callback(event_record: *mut EVENT_RECORD) {
    if event_record.is_null() { return; }
    let er = unsafe { &*event_record };
    let ctx = er.UserContext as *const ConsumerState;
    if ctx.is_null() { return; }
    let state = unsafe { &*ctx };
    state.events_seen.fetch_add(1, Ordering::Relaxed);
}
```

**Impact:** Architecture §2.1's stated drop budget is `< 0.1%` of events at typical load. The callback path is hot; this matters once the parser layer (week 3+) adds actual event dispatch.

**Fix:** Group A weeks 3–7 will replace the no-op callback body with a real ring-buffer push. At that point: profile to confirm the null-checks don't show up in flame graphs; if they do, the `ctx.is_null()` check can be skipped because OpenTraceW with `Context = Arc::as_ptr(&state)` guarantees non-null by construction. Defer until the real dispatch lands.

**Effort:** S (deferred)
**Gate:** Group A week 3+

## G-003 — `validate_policy_against_safe_list` allocates `Vec<String>` per call; not on hot path (only SetPolicy IPC), but the error message also clones the safe-list rationale strings into the formatted output

**Severity:** LOW (matches A-006)
**Evidence:** See A-006.

**Fix:** Combined with A-006.

# AXIS H — Security & privilege

## H-001 — Closed-loop session GUID is hard-coded at `crates/etw/src/session.rs:124`; collision risk with a future Microsoft-provider GUID is theoretically possible but the value was generated randomly per the comment

**Severity:** N/A — credit
**Evidence:** `crates/etw/src/session.rs:122-124`:

```rust
pub(super) const SESSION_GUID: GUID =
    GUID::from_u128(0x7A2E_6C18_4F30_4D9B_A6E1_8B5C_2D71_F0A3);
```

Random 128-bit value; collision odds astronomically low.

**Impact:** None. Documented as PASS.

## H-002 — `framesage-service` startup correctly hardens `%ProgramData%\framesage\` ACL via `crate::acl::harden_config_dir` (`runtime.rs:78-89`) and verifies owner is admin/system before loading policy (`runtime.rs:243-268`) — closes C-04

**Severity:** N/A — credit
**Evidence:** `crates/service/src/runtime.rs:78-89, 243-268` (quoted in prior reads).

## H-003 — Pre-audit: `framesage-recorder` will write to `%ProgramData%\framesage\sessions\<session-id>.jsonl` per architecture §2.3. ACL inheritance from the framesage dir handles read-restrictions, but the session-id is `uuid::v4()`-based and discoverable by enumeration; this is fine because the dir is admin-readable only

**Severity:** N/A — pre-audit credit
**Evidence:** `audit/v0.7-architecture.md:643-654`.

## H-004 — `closed_loop.rs` test-only override `BuildOverrideGuard` requires `pub(crate)` visibility but uses `thread_local!` — sound, but the seam-trait pattern between `framesage-etw::build_gate::detected_build` and `framesage-service::closed_loop::build_gate_detected_build` is dual-layered

**Severity:** LOW
**Evidence:** `crates/service/src/closed_loop.rs:33-99` — the dual-layer seam is documented in the module comment.

**Impact:** Future debugging will be slowed by needing to remember TWO override points. Documented inline; reduces severity.

**Fix:** Once Group A week 3+ stabilizes, fold the override into the etw crate as `#[cfg(test)] pub(crate) fn override_build()` reachable from service-side tests via re-export.

**Effort:** S
**Gate:** Month 3

## H-005 — AC outreach (cross-listed at I-005) — see Axis I.

# AXIS I — Distribution & ops

## I-001 — No Authenticode signing — architecture §2.5 commits to OV cert (~$300/yr Sectigo) or EV cert (~$500/yr DigiCert); not yet procured

**Severity:** BLOCKER-v0.7.1
**Evidence:** `audit/v0.7-architecture.md:1262-1302` specifies signing workflow + verification surface; no `--verify-signature` verb in `crates/cli/src/main.rs` [UNVERIFIED — not re-read this turn].

**Impact:** SmartScreen warning on every install of an admin-requesting tool. Adoption-killer for non-power-users.

**Fix:** Cert procurement (OV first per architecture recommendation) + signing workflow + `--verify-signature` verb + README "Verifying the binary" section. Phase 3 Month 2.

**Effort:** XL (cross-vendor cert + HSM + GitHub Actions wiring + README)
**Gate:** v0.7.1

## I-002 — No MSI installer — PHASE2-PLAN.md punted to a separate engagement; current installer is `install.ps1`

**Severity:** HIGH
**Evidence:** `audit/PHASE2-PLAN.md:25`:

```
Install/uninstall: per your decision, fix the existing PowerShell installer + CLI uninstall now. MSI / WiX / code signing deferred to a separate engagement (not in this plan).
```

**Impact:** No Add/Remove Programs entry; PowerShell self-elevation has script-TOCTOU (L-07 in SUMMARY.md). Both are real adoption costs.

**Fix:** Phase 3 Month 3 — WiX-based MSI. Existing install.ps1 stays for dev iteration.

**Effort:** XL
**Gate:** v1.0

## I-003 — EDR-matrix testing (Defender ATP + CrowdStrike + SentinelOne) is the v0.7.1 default-on-flip gate per architecture §2.1 + `spike/etw-edr-report.md` §6.1

**Severity:** BLOCKER-v0.7.1
**Evidence:** `audit/v0.7-architecture.md:1612-1617`:

```
**NOT a Group A blocker (moved to v0.7.1):** EDR-matrix
validation on Defender ATP / CrowdStrike Falcon / SentinelOne
Singularity. The matrix becomes the gate on the v0.7.1
default-on-flip PR (`closed_loop_enabled: true`), not the
v0.7 ship.
```

**Impact:** Acknowledged. Listed for roadmap visibility.

**Fix:** Phase 3 Month 2 — engage SOC team / security researchers per the spike report's escalation plan; produce `spike/etw-edr-report.md` §6.1 attestation.

**Effort:** XL (2 engineer-days per architecture estimate; could blow to weeks if findings surface)
**Gate:** v0.7.1

## I-004 — Service binary path moved to `%ProgramFiles%\FrameSage\` per item 1.6; ACL hardening via `icacls` is implied — verification path needs documenting

**Severity:** LOW
**Evidence:** `audit/PHASE2-PLAN.md:103-109` (item 1.6 spec) — closure status not re-verified this turn.

**Fix:** Phase 3 Month 1 spot-check: verify install.ps1 lands binaries at `%ProgramFiles%` with `Administrators:F SYSTEM:F Users:RX` ACL.

**Effort:** S
**Gate:** Month 1

## I-005 — Anti-cheat outreach (Riot/Vanguard, EAC, BattlEye, FACEIT, ESEA) — pre-ship research at `audit/research/ANTI-CHEAT-MATRIX.md` (not re-read this turn) — was the user-mandated dual-listed BLOCKER-v0.7.1 item (per Phase 1 confirmation)

**Severity:** BLOCKER-v0.7.1
**Evidence:** AC detection + AC-aware profile enum is built (`engine/lib.rs:3376-3406`, `sys/inner/ac_detect.rs:1-185`). The OUTREACH ("ask Riot / EA / BattlEye / FACEIT / ESEA whether our priority/affinity/CPU-set surface is detectable as cheat-engine-like") is the unbuilt half. Per Phase 1 user instruction: "external dependency, Day-5 cutoff 2026-05-22."

**Impact:** Without explicit AC-vendor sign-off, the v0.7.1 default-on flip risks unilaterally banning users from anti-cheat-protected games. The matrix's invariants are deliberately conservative (no game-process modifications under Vanguard/FACEIT) but the test ground truth is vendor confirmation that the launcher-inheritance and game-process-untouched profile shapes are visible to the AC and accepted as benign.

**Fix:** Phase 3 Week 1 — draft AC-vendor outreach email; cut Day-5 deadline 2026-05-22. Specifically:
1. Email Riot/Vanguard via `vanguard-developers@riotgames.com` describing the AC-aware safe-mode tier
2. Email EA Javelin program (BF6 specific) — channel TBD
3. Email BattlEye via the EAC/BE developer support route
4. Email FACEIT through their anti-cheat-info contact
5. Note: ESEA is in `AntiCheatProfile::Disabled` — engine goes STANDBY when `ESEAClient.exe` is detected; no outreach needed because we explicitly don't modify the game's environment

**Effort:** M (drafting + sending); response window is multi-week
**Gate:** v0.7.1 (must close before default-on flip)

# AXIS J — Killer features & repositioning

## J-001 — Marquee differentiator is the closed-loop honesty contract — `audit/v0.7-architecture.md` §2.4 specifies asymmetric +/- bands that surface NEGATIVE attribution prominently. No competitor (LatencyMon, PresentMon, CapFrameX, Process Lasso) does this end-to-end

**Severity:** N/A — strategic note
**Evidence:** `audit/v0.7-architecture.md:1133-1180`:

```
The bands are deliberately **asymmetric**. Per Phase 2 sign-off:
the cost of claiming help that didn't happen (false positive) is
much higher than the cost of missing credit for real help (false
negative). Translation: be slow to take credit, quick to admit
harm.
```

**Strategic implication:** Marketing/positioning materials should lead with this. "We tell you when we made it worse, in red, with a link to the bad profile."

**Fix:** Tied to v0.7.1 readiness — once Group C ships AND C-005 tests pass AND a real-game negative session is captured (E-004), the marketing claim is real and defensible.

**Effort:** N/A (strategic)
**Gate:** v0.7.1

## J-002 — Closed-loop disclosure / EDR-implication first-run page is uncoded yet specified — every install of v0.7 that lands without this is a credibility-spent rotation

**Severity:** BLOCKER-v0.7 (cross-ref F-001)
**Evidence:** See F-001 above.

## J-003 — Manual Global Game Mode (PHASE2-PLAN.md item 2.11) is a real differentiator vs. Process Lasso for streamers / video editors / benchmarking — not present in competitors as a single-toggle global state

**Severity:** N/A — credit
**Evidence:** Implementation surface via IPC verbs at `crates/service/src/runtime.rs:649-666`.

**Strategic implication:** OBS-scene scripting integration (per item 2.11 spec) is a genuine power-user win.

**Fix:** Document `framesage game-mode start/stop` verbs in README's "Power-user workflows" section.

**Effort:** S
**Gate:** Month 1

## J-004 — Closed-loop attribution + Manual Global Game Mode + AC-aware safe profiles are three differentiators NONE of the competitors offer together; the v0.7.1 launch positioning should lean on this trio

**Severity:** N/A — strategic
**Fix:** Marketing/launch-positioning is out of audit scope. Flagged for the product lead.

# AXIS K — Things to remove / relocate / split

## K-001 — `crates/spike-etw/` is the standalone v0.7 spike binary; comment at `Cargo.toml:13-18` says "not built by default and the v0.6 release scripts never ship it" — REMOVE once Group A week 3+ proves the production crate covers all spike scenarios

**Severity:** LOW
**Evidence:** `Cargo.toml:13-18`:

```toml
# v0.7 Phase 1 spike — standalone binary, not wired into the
# bundled distribution. Lives in the workspace so it picks up
# the workspace `windows` version + `anyhow` etc., but it's
# not built by default and the v0.6 release scripts never
# ship it.
"crates/spike-etw",
```

**Impact:** Workspace bloat; future contributor confusion ("which is the real ETW crate?"). The production crate (`crates/etw`) is the authoritative one.

**Fix:** After Group A week 5+ ships parsers + ring buffer + drain, the spike's experimental value drops to zero. Remove the crate; the spike's content lives in `spike/group-a-week-2-report.md` already.

**Effort:** S
**Gate:** Group A week 5+ post-merge

## K-002 — `spike-etw` SESSION_GUID at production session GUID location says "Generated once for production; replace at v1.0 stable release" — note for v1.0 release checklist

**Severity:** N/A
**Evidence:** `crates/etw/src/session.rs:120-124`.

**Fix:** v1.0 release checklist item — regenerate session GUID once production deployment is broader.

**Effort:** S
**Gate:** v1.0

## K-003 — `framesage-etw::session::EtwSession::stop` retains the `verify_session_gone` post-join step which is the source of C-001's flake risk — REMOVE per the C-001 fix

**Severity:** Linked to C-001
**Fix:** Tracked in C-001.

## K-004 — `crates/tray/src/main.rs` is 5,215 LOC and remains the project's biggest single-file reviewability blocker; PHASE2-PLAN.md item 3.6 partially closed it (extracted ipc_client / state / theme / editors / formatters / icons / icon_assets / onboarding / process_actions / tree / widgets / win32 / activity_log) but the tab renderers + modals + `App` struct + `impl App` body remain inline

**Severity:** MEDIUM (with **HIGH regression-risk** annotation on the split itself)
**Evidence:** `wc -l crates/tray/src/main.rs` = **5,215 LOC**. Per Grep of `^    fn render_*` in main.rs:

```
L998   render_preview_modal
L1154  render_terminate_confirm_modal
L1224  render_affinity_picker_modal
L1683  render_tab_strip
L1748  render_status_tab
L2045  render_settings_tab            ─┐
L2088  render_settings_probalance_card │
L2216  render_settings_tick_card        ├─ ~500 LOC of settings
L2296  render_settings_policy_card     │
L2334  render_settings_reset_confirm  ─┘
L2546  render_activity_tab
L2666  render_rules_tab               ─┐ ~450 LOC rules
L3012  render_affinity_rules_section  ─┘
L3115  render_profiles_tab            (~470 LOC)
L3583  render_processes_tab           (~1,380 LOC — PHASE2-PLAN.md item 3.6 "tray::tabs::processes" still unextracted)
```

**Impact:** Reviewability — a 5,215-LOC file is opaque to code-review at the diff scale that matters (one-screen scroll). New tab additions land in the same file rather than as bounded modules. egui state updates that cross tabs are hard to spot. Test coverage of the rendering paths is essentially zero (eframe-driven UI is hard to unit-test) so refactors carry HIGH regression risk by structural argument alone — moving a `self.foo` access from one method to another can silently change which lock is held during the move.

**Proposed split** (one PR per tab; per-PR diff stays under ~1,500 LOC):

| New module | Source lines in main.rs | LOC estimate | Notes |
|---|---|---|---|
| `tabs/status.rs` | L1748–~2044 | ~300 | render_status_tab |
| `tabs/settings.rs` | L2045–L2545 | ~500 | The five settings render fns + reset-confirm modal (modal is settings-scoped) |
| `tabs/activity.rs` | L2546–L2665 | ~120 | render_activity_tab + activity-row helpers |
| `tabs/rules.rs` | L2666–L3114 | ~450 | render_rules_tab + render_affinity_rules_section |
| `tabs/profiles.rs` | L3115–L3582 | ~470 | render_profiles_tab |
| `tabs/processes.rs` | L3583–L~4960 | ~1,380 | render_processes_tab — the biggest cut; PHASE2-PLAN.md 3.6 already specifies this module |
| `modals.rs` | L998–L1330 | ~330 | render_preview_modal + render_terminate_confirm_modal + render_affinity_picker_modal (NOT the settings-reset modal, which co-locates with settings) |
| `main.rs` (residual) | rest | ~1,500 | App struct + Default impls + `impl FramesageApp { …small helpers… }` + `impl eframe::App` + `fn main()` + selector_to_mask + shell helpers |

**Regression-risk annotation (HIGH):**
- Tab renderers access ~80 fields on `FramesageApp` via `&mut self`. Moving them to free functions in submodules requires explicit `&mut FramesageApp` (or `&mut TabState<'_>`) parameters; the API-surface refactor is mechanical but wide.
- Stable visual diff is the only verification — egui's render path isn't unit-tested. The split PR's reviewer must walk through every tab manually on Windows + capture screenshots before/after.
- Modals share state with the tab that triggered them (e.g., `render_terminate_confirm_modal` reads `self.terminate_confirm` which is populated by processes-tab right-click). Extracting modals means deciding which crate owns the state (recommend: each modal carries its own state struct, hoisted into `App`).

**Fix:** Phase 3 Month 2 — sequenced sub-PRs, one per tab. Settings + Activity first (smaller, lower-risk). Processes last (biggest, highest-risk). Each PR carries before/after screenshots + a "no behavior change" claim that the reviewer must validate manually.

**Effort:** XL (multi-week, 6–7 sub-PRs)
**Gate:** Month 2 (start mid-Month-2 once cert/EDR external clocks are running)

## K-005 — `crates/engine/src/lib.rs` is 4,866 LOC and is the second reviewability blocker; the existing modularization (`probalance.rs:786 LOC`, `clock.rs:59 LOC`, `undo.rs:205 LOC`) proves the pattern but the orchestrator + reconcile + apply + system-mode bodies remain in lib.rs

**Severity:** MEDIUM (with **HIGH regression-risk** annotation)
**Evidence:** `wc -l crates/engine/src/lib.rs` = **4,866 LOC**. Per Grep of `^    fn|pub fn` inside `impl Engine`:

```
L451–1860   Public API methods (set_policy, apply_once, set_process_priority,
            suspend_process, set_affinity_rule, list_process_snapshots,
            status, etc.) — ~30 methods
L1884       tick()
L1960       maybe_refresh_ac_presence
L2054       maybe_run_probalance_locked
L2291       maybe_reassert_persistent_locked
L2426       maybe_scan_background_locked
L2662       reconcile()
L2881–3184  reconcile_system_mode_locked / enter_system_mode_locked /
            revert_system_mode_locked
L3185–3533  apply_profile + check_process_modifiable + sys_apply_action +
            sys_revert_all + revert_record + detect_apply_drift +
            applied_from_plan + classify_apply_failure
L3540–3580  PlatformQuery: SystemStateQuery impl (Windows + non-Windows)
L3580–end   Tests (~1,280 LOC inline — Rust convention, stays put)
```

**Impact:** Same shape as K-004 — review-time opacity. The 53 inline tests are excellent and stay. But the lock-ordering semantics across `maybe_*` methods + `reconcile` + `apply_*` are invisible at a glance; a future contributor cannot see at-a-call-site which lock is held in which method.

**Proposed split** — Phase 1 already flagged "probalance.rs as gold standard, the rest of the engine should look like this." The natural cuts:

| New module | Source lines | LOC estimate | Notes |
|---|---|---|---|
| `engine/api.rs` | L451–L1860 (public methods) | ~1,400 | Keep as `impl Engine` block via `mod api;` + `use api::*` re-export. The 30 public fns are the IPC surface; they stay together. |
| `engine/reconcile.rs` | L1884–L2880 | ~1,000 | `tick()` + `maybe_refresh_ac_presence` + `maybe_run_probalance_locked` + `maybe_reassert_persistent_locked` + `maybe_scan_background_locked` + `reconcile()` |
| `engine/system_mode.rs` | L2881–L3184 | ~300 | The three system-mode methods (`reconcile_system_mode_locked`, `enter_system_mode_locked`, `revert_system_mode_locked`) — pair with `gamemode/journal.rs` |
| `engine/apply.rs` | L3185–L3533 | ~350 | `apply_profile` + `check_process_modifiable` + `revert_record` + `detect_apply_drift` + `applied_from_plan` + `sys_apply_action` + `sys_revert_all` + `classify_apply_failure` + `apply_backoff_active` |
| `engine/system_state.rs` | L3540–L3580 | ~40 | `PlatformQuery` impl of `SystemStateQuery` (Windows + non-Windows variants) |
| `engine/lib.rs` (residual) | rest | ~600 | `EngineDeps` + `EngineState` + `Engine` struct + sub-module declarations + test module |

**Alternative considered: probalance as its own crate.** Phase 1 hinted at this (`probalance::decide` is testable in isolation). The current `probalance.rs` is already a clean module; the cost of pulling it into a `framesage-probalance` crate is workspace bookkeeping for marginal architectural benefit. **Recommendation: keep as a module.** The clean shape is already achieved.

**Regression-risk annotation (HIGH):**
- The four `maybe_*` methods take `&mut EngineState` while `reconcile()` takes `&mut self`. Lock ordering invariants are encoded in the call structure — `tick()` acquires the state lock once and passes `&mut EngineState` down. Splitting into modules must preserve this; pulling `maybe_*` into `engine::reconcile` as free functions taking `&mut EngineState` works but every call-site needs updating.
- `apply_profile` calls `sys.apply(pid, &profile, topology)` via the `SysApi` trait; moving it to `engine/apply.rs` means crossing the visibility boundary with `engine::reconcile.rs` (which calls `apply_profile`). Need `pub(crate) fn`.
- Tests are inline (good); after the split, tests stay in `engine/lib.rs::tests` because they import all submodules via `use crate::*` and exercise public+private surface. Verifies that the split is implementation-detail-only — public API doesn't change.

**Fix:** Phase 3 Month 2 — sequenced sub-PRs, one per module:
1. `engine/system_state.rs` (40 LOC; lowest risk)
2. `engine/apply.rs` (350 LOC; medium risk — central to safe-list enforcement)
3. `engine/system_mode.rs` (300 LOC; medium risk — Game Mode is load-bearing)
4. `engine/reconcile.rs` (1,000 LOC; highest risk — tick loop)
5. `engine/api.rs` (1,400 LOC; medium-high — the IPC surface)

Each sub-PR maintains all 53 tests green + `cargo clippy -- -D warnings` clean + buddy-reviewed for "is the lock ordering preserved" specifically.

**Effort:** XL (multi-week, 5 sub-PRs)
**Gate:** Month 2 (start mid-Month-2; sequence after K-004 starts so the same "split-with-care" rhythm runs in parallel on both crates)

# AXIS L — Pre-audit of unbuilt deliverables

These are quality-control findings against the **plans** for Group A weeks 3–7 / Groups B/C/D. A "PRE-L-NNN" finding is one that would become a real finding IF the code shipped following the plan as currently written.

## PRE-L-001 — Architecture §2.3 `cpu_sample.per_process: Option<Vec<PerProcessCpu>>` schema slot at `audit/v0.7-architecture.md:777-808` is "always-null in v0.7, populated when v0.8 flips `recorder_per_process_enabled: true`." The test at PRE-L-001 must cover BOTH paths in v0.7 — Phase 2 sign-off resolution #5 requires this

**Severity:** PRE-MEDIUM
**Evidence:** `audit/v0.7-architecture.md:801-808`:

```
Group B deliverable must include:
- The recorder code path that builds `per_process` when the
  setting is true (gated, tested, but disabled by default)
- A unit test that exercises both the disabled (null) and
  enabled (populated) paths
```

**Risk:** The Group B PR reviewer must verify both test paths exist. If the reviewer slips, the v0.8 default-flip ships a code path that has never run end-to-end.

**Fix:** Group B PR-review checklist item: confirm both test paths.

## PRE-L-002 — Architecture §2.3 retention policy "Per-session cap: 50 MB; on reaching 80% shift to 0.5Hz sampling, at 95% to 0.1Hz, at 100% stop new samples" is precise but the recorder code path needs three tests (80%, 95%, 100% boundaries)

**Severity:** PRE-MEDIUM
**Evidence:** `audit/v0.7-architecture.md:867-878`.

**Fix:** Group B test plan — add explicit boundary tests.

## PRE-L-003 — Architecture §2.4 attribution-disabled cases (4 reasons listed at `:1183-1191`) each need a test that asserts the user-facing string contents — same shape as the C-005 honesty contract tests but for the failure paths

**Severity:** PRE-MEDIUM
**Evidence:** `audit/v0.7-architecture.md:1183-1191`:

```
- "Session too short for attribution" (< 90s total)
- "Baseline too short" (< 30s before first apply)
- "Frame data unavailable" (PresentMon failed or wasn't enabled
  for this profile)
- "Partial data — drops detected" (when `partial_data: true` —
  user can opt into "show anyway" with explicit caveat)
```

**Fix:** Group C deliverable — four more tests in the honesty-contract test set.

## PRE-L-004 — Architecture §2.2 PresentMon subprocess management risk: "If a user has 30 sessions per day, that's 30 PresentMon spawns/kills per day. […] the cumulative process-creation telemetry could trigger an EDR heuristic. Mitigation: rate-limit + reuse where possible"

**Severity:** PRE-HIGH
**Evidence:** `audit/v0.7-architecture.md:1429-1435`.

**Risk:** The mitigation is loose ("rate-limit + reuse where possible"). Without a hard policy, an over-eager spawn loop will burn EDR-budget on dev machines.

**Fix:** Group B explicit deliverable: rate-limit at most 1 PresentMon spawn / 30s; reuse currently-running PresentMon if `--process_name` matches.

## PRE-L-005 — Architecture §2.5 install.ps1 changes section says "After staging binaries, run `Get-AuthenticodeSignature` against each .exe. If `Status -ne 'Valid'`, abort install with a clear error" but does NOT specify what happens during the v0.7→v0.7.1 transition (cert procured between releases, old binaries unsigned, upgrade path TBD)

**Severity:** PRE-LOW
**Evidence:** `audit/v0.7-architecture.md:1293-1294`.

**Fix:** v0.7.1 release-notes section: "Upgrading from v0.7? Run install.ps1 --upgrade with --skip-signature-check on the OLD binaries to allow uninstall before installing signed v0.7.1 binaries."

## PRE-L-006 — Group A week 3+ schema research deliverable `spike/etw-schemas.md` (per `audit/v0.7-architecture.md:1467-1492`) status is unclear from this turn's reading; the PR #67 reference at `:1684` suggests landed

**Severity:** N/A — status check
**Fix:** Verify `spike/etw-schemas.md` exists and contains per-event-type citations per the ground rule.

---

## Cross-axis themes

### Theme 1 — The v0.7 closed-loop subsystem is well-shaped but has a small validation-gap that the spike work doesn't fully close

Findings: C-001, C-002, E-001, E-003. The seam-trait + mock-injection architecture is correct. The remaining risk is **integration**: real-kernel `ProcessTrace` panic-recovery has been validated manually (Step 21–28) but not via CI. The Phase 3 roadmap should add at least one CI-runnable real-ETW integration test on Windows runners.

### Theme 2 — The architecture's commitment to the closed-loop UX is precise; the code that fulfills that commitment is unbuilt

Findings: F-001, F-002, C-005, E-004. The honesty contract IS the differentiator. Shipping v0.7 without the Sessions tab + onboarding page 3 spends architecture credibility for short-term schedule relief. Section J-002 notes this is a credibility hit.

### Theme 3 — Distribution & ops remain pre-1.0

Findings: I-001 (signing), I-002 (MSI), I-003 (EDR matrix), I-005 (AC outreach). None are v0.7 blockers; all four are v0.7.1 blockers if v0.7.1 is the closed-loop default-on flip. The roadmap should sequence them.

### Theme 4 — The audit tradition itself is now load-bearing on quality

The 89-finding SUMMARY.md + 43-item PHASE2-PLAN.md + 5Q/4Q buddy reviews + post-batch verification commands captured permanently in spike reports — this is the project's primary defense against the "second factual error in a row" pattern called out in `v0.7-architecture.md:1540-1546`. The Phase 4 process-improvements section appends to this strength rather than adding new processes.

---

## Findings index by milestone gate

| Gate | Findings |
|---|---|
| **Week 1 (closure)** | A-003 (bf6.exe AC false-positive), A-004 (Arc::as_ptr doc-comment), C-001 (verify_session_gone flake), ~~E-001 (real consumer-thread panic test)~~ — closed prior to audit; see E-001 Resolution block, I-005 kickoff (AC outreach drafts) |
| **Month 1** | A-001, B-001, B-002, B-003, C-002, C-004, D-001, E-003, **E-005 (catch_unwind &str-branch coverage gap)**, F-003, F-004, I-004, J-003 |
| **Month 2** | A-002, A-005, A-006 (with G-003), H-004, **K-005 (engine/lib.rs split, XL, HIGH-regression-risk — sequential before K-004)** |
| **Month 3** | A-007, D-002, D-003, D-004, G-001, K-001, **K-004 (tray/main.rs split, XL, HIGH-regression-risk — spillover from Month 2; slips to v1.0-prep if Group B/C bandwidth tight)** |
| **v0.7 BLOCKER** | F-001 (onboarding page 3), F-002 (Sessions tab — see CONFLICT note) |
| **v0.7.1 BLOCKER** | C-005 (honesty contract tests), E-004 (negative-session verification), I-001 (signing), I-003 (EDR matrix), I-005 (AC outreach completion) |
| **v1.0** | I-002 (MSI), K-002 (session GUID rotation) |

---

## Status

Phase 2 findings file initialized + populated across all 11 axes (A–K + pre-audit L).
Findings count: **45** (29 actionable open + 1 closed-prior + 6 credits/PASS-after-analysis + 6 pre-audit + 2 strategic + 1 added 2026-05-18). E-001 moved to closed-prior 2026-05-18 per W1.5 (#84) survey — see E-001 Resolution block. E-005 added 2026-05-18 for the catch_unwind `&str`-branch coverage gap surfaced during the same survey.
BLOCKER-v0.7: 2 (F-001, F-002 — with conflict-with-audit-position flag on F-002)
BLOCKER-v0.7.1: 5 (C-005, E-004, I-001, I-003, I-005)
HIGH: 3 (A-003, C-001, I-002) — E-001 reclassified to "Closed prior" per Resolution
MEDIUM: 12 (incl. K-004 + K-005, both HIGH-regression-risk; +E-005 added 2026-05-18 per W1.5 survey)
LOW: 13
N/A (credit/strategic): 10

PRE-L-006 closed inline (spike/etw-schemas.md verified — 713 LOC, structured per ground rule).

Next: Phase 3 roadmap (Week 1 / Month 1 / Month 2 / Month 3 buckets) + Phase 4 process improvements, both produced in `audit/2026-revision-phase3-roadmap.md`.
