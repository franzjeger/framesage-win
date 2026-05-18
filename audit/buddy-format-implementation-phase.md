# Buddy-system format — implementation phase

**Status:** APPROVED 2026-05-18 (DRAFT v1 → v2 errata fixes + META 2 (1) broadening + §X dependency arrows → v2.1 sweep patch → APPROVED). Revision history captured in `audit/buddy-disagreements.md` Entry 4.
**Authoritative inputs:**
- `audit/buddy-disagreements.md` — the buddy-system audit trail. Reader consults the current state of the log for the planning-phase format evolution (3Q → 4Q with the (d) internal-consistency question) and the surfaced same-category-finding watch signal.
- `spike/group-a-week-2-plan.md` — the load-bearing implementation plan whose code reviews this format governs. Section refs throughout this document point at `plan/group-a-week-2` HEAD (now on `main` after PR #73 + PR #78 merge).
- `audit/v0.7-architecture.md` — architecture proposal. §2.4 (Session History UI + honesty contract) is referenced by question (4) below.

The planning-phase four-question format `(a) authority / (b) scope / (c) feasibility / (d) internal-consistency` is for **documents**. Code review during implementation needs a different format tuned for runtime concerns the planning format can't cover: language-mechanics safety, concurrency, error handling, what tests actually assert.

The (d) question carries a cross-document scope (see §X "Cross-document consistency dependencies" below): every artifact must agree with itself AND with the authoritative documents it references. The implementation-phase (5) check inherits the same extended scope at the code level.

This document specifies the implementation-phase format. Applies to:
- The end-of-week Group A PR (week 2, week 3, etc.).
- Any PR that lands more than ~50 LOC of production code in `crates/etw/` or its consumers.
- Any PR that introduces an `unsafe` block or a new dependency.

Does NOT apply to:
- Doc-only PRs (use planning-phase 4Q).
- One-line clippy/fmt fix PRs (no buddy needed; they're already covered by CI).
- Pure test-only PRs that don't touch production code (planning-phase 4Q suffices; the (d) check is enough).

---

## The format: five questions

### (1) Language-mechanics safety

This question was originally "FFI safety" (DRAFT v1, 2026-05-17). Broadened per **META 2** (decided alongside the v4.2 plan amendment: "the trait surface needs a method that wasn't in §3.4's enumeration → STOP, but mechanical visibility/Send-Sync mismatches kept being mis-classified as 'just compile errors'"). Day 3's Entry 3 finding — `RefCell`-backed mock + `ConsumerState`-holds-`S` + `std::thread::spawn`'s `Sync` bound — escaped three rounds of planning-phase review because no format question explicitly named visibility-vs-consumer-location. (5Q (1) covers it now.)

**Sub-bullet (1a) — `unsafe` SAFETY justifications.**

Every `unsafe` block in the diff has an inline `// SAFETY:` comment justifying the contract. The justification names:
- What preconditions the call requires (pointer non-null, buffer length, alignment, lifetime).
- How the caller satisfies each precondition at the call site.
- What invariants must hold after the call (e.g. "session_handle is valid until ControlTraceW(STOP) is called").

**Examples of acceptable justification:**
> `// SAFETY: name_wide is a NUL-terminated UTF-16 buffer (encoded by encode_utf16().chain(once(0)).collect() three lines above); StartTraceW requires PCWSTR pointing at a NUL-terminated wide string. session_handle is a stack-allocated CONTROLTRACE_HANDLE we own; OK to take &mut.`

**Examples of NOT acceptable:**
> `// SAFETY: standard windows-rs pattern` (too vague)
> `// SAFETY: tested manually` (not a contract argument)
> No comment at all.

Buddy should grep `unsafe` in the diff and verify EVERY block has a justification. If even one lacks it: FAIL.

**Sub-bullet (1b) — visibility matches consumer location.**

`pub` items are reachable from any downstream crate. `pub(crate)` is reachable only from within the lib's own crate (NOT integration tests in `tests/`). `#[cfg(test)]` items exist only when the lib itself is being tested (NOT when downstream integration tests are compiled).

**Specific failure modes to flag:**
- `pub(crate)` test helper + integration test in `crates/X/tests/foo.rs` → compile error (integration tests link the crate as an external dep; `pub(crate)` is invisible). **Fix:** either inline the test as `#[cfg(test)] mod tests` in the source module, or change the helper to `pub` (gated behind a `_test_seam` Cargo feature if the API surface concern is real).
- `#[cfg(test)] pub struct MockX` + integration test in `tests/` → same problem (`cfg(test)` isn't set on the lib when the integration test is compiled, so the type is invisible). Same fix.
- `pub fn xxx(...) -> impl SomeTrait` exposed from a sealed-trait pattern → caller can't name the return type → flag.

This rule traces to Day 3 (week 2, 2026-05-17): the DRAFT plan §3.4 chose `RefCell<VecDeque<T>>` for `MockEtwSysCalls` queues with rationale "tests are single-threaded." But the consumer-thread spawn required `S: Sync` (transitively via `Arc<ConsumerState<S>>: Send`), and `RefCell` isn't `Sync`. The plan didn't anticipate the conflict because no format check explicitly named "do the visibility + trait bounds work for the consumers that will actually use the API?"

**Sub-bullet (1c) — `Send`/`Sync` bounds match usage.**

Types crossing thread boundaries need the right markers:
- `std::thread::spawn`'s closure → captured types must be `Send + 'static`.
- `Arc<T>: Send` ⇔ `T: Send + Sync`.
- `tokio::spawn`'s future → `Future + Send`; captured types must be `Send`.
- `parking_lot::Mutex<T>: Sync` ⇔ `T: Send`. (Plus: `parking_lot::Mutex<T>` is NOT `RefUnwindSafe` — `static_assertions::assert_impl_all!(MyState: RefUnwindSafe)` will catch this if `MyState` holds a `Mutex` and is captured by a `catch_unwind` wrapper.)

Buddy reads the captured fields and the trait bounds. A `tokio::spawn(async move { ... })` capturing a non-`Send` future receiver is a flag.

**Sub-bullet (1d) — trait bounds match what implementers can satisfy.**

If `impl<S: EtwSysCalls + Clone + Send + 'static> EtwSession<S>` requires `S: Clone`, every impl must be cloneable. If `MockX` uses `RefCell<VecDeque<T>>`, `#[derive(Clone)]` gives per-clone-state semantics — flag if test design assumed shared state.

If a sealed trait (`pub trait Internal: private::Sealed`) is part of the public API, document that downstream crates can't implement it.

**Sub-bullet (1e) — Windows-rs struct layout.**

- `windows::Win32::*` structs with wire-layout requirements (`EVENT_TRACE_PROPERTIES`, `OSVERSIONINFOEXW`, `WNODE_HEADER`): the field-population code matches the documented Microsoft layout. Cross-reference MSDN.
- `PCWSTR` / `PWSTR` / `PCSTR`: lifetime of the underlying buffer outlives the API call.
- Raw pointers passed to windows-rs: provenance is documented.

### (2) Concurrency correctness

The ETW callback runs on a kernel-event delivery thread. The consumer thread runs `ProcessTrace`. The supervisor task runs in tokio. State shared across these has hazards.

Buddy checks each piece of shared state:
- **Atomics:** the Ordering parameter matches the intent. `Relaxed` is fine for counter increments where no cross-counter ordering is required; `Acquire`/`Release` needed for handoff patterns; `SeqCst` only when ordering is global and load-bearing.
- **Channels:** `oneshot` for one-shot signals, `mpsc::unbounded` for streams, `tokio::sync::watch` for current-value-only state. Wrong channel type for the use case is a flag.
- **Locks:** `parking_lot::Mutex` is preferred over `std::sync::Mutex` in this codebase (workspace dep). Any `Arc<RefCell<T>>` is a bug (not Send); any `RefCell<T>` in production (non-test) code needs a justification — the test-mock pattern in §3.4 of the week-2 plan is allowed because it's `#[cfg(test)]`.
- **Send + Sync bounds:** types crossing thread boundaries (closures captured by `thread::spawn`, types in `tokio::spawn` futures) have the right bounds. Verify by reading the captured fields.
- **`catch_unwind` + `AssertUnwindSafe`:** every use of `AssertUnwindSafe` has a comment explaining why the captured state is `RefUnwindSafe` in practice. A `static_assertions::assert_impl_all!` regression guard is preferred (per week 2 plan §3.5 #4) but not strictly required if the comment is airtight.

Buddy should also check: is any state shared between the consumer thread and the supervisor that is NOT protected (no atomic, no channel, no lock)? If yes: FAIL.

### (3) Error handling — no swallows, no panics on long-running paths

`anyhow::Result` is the workspace's error type. Buddy verifies:
- Every `Result` produced by a `windows-rs` call (returned via `WIN32_ERROR` mapped to `Result`) is either handled at the call site OR explicitly bubbled with `?`. **No `.ok();` or `let _ =` on a fallible call without a justifying comment.**
- Panics in the consumer thread are caught by the `catch_unwind` wrapper (§3.5 of week 2 plan). Panics in the supervisor task or any tokio task are NOT acceptable — tokio task panics print to stderr and detach silently. A `.unwrap()` in async context is a flag; `.expect("...")` is fine only if the expected condition is a compile-time invariant.
- `panic!` calls outside `#[cfg(test)]` are flags. Each one needs a justifying comment ("unreachable, type-system-enforced") or it gets replaced with `.expect()` / an error return.
- Logging: failures that downgrade the subsystem emit at `ERROR`; failures that succeed-with-warning emit at `WARN`; successful operations at `INFO` or below. Buddy checks that severity matches consequence.

### (4) Test-asserts-what-it-says

The test's name describes what it's testing. The test body should actually test that. Buddy checks:
- Test name → assertion alignment. A test named `start_returns_disabled_on_unsupported_build` should assert specifically that `start()` returns `EtwSubsystem::Disabled(BuildUnsupported)` — not just "doesn't panic" or "returns some result."
- Mock setup → assertion alignment. If a test scripts a specific failure (`mock.expect_start_trace(ERROR_ACCESS_DENIED)`), the assertion should be specific to that failure (`assert!(matches!(result, EtwSubsystem::Disabled(AccessDenied)))`), not a generic `is_err()`.
- Test exercises the production code path, not just the mock. A test that asserts only on mock-state (`mock.call_count("start_trace") == 1`) without ever checking the result is a flag — it tests that you wrote the test, not that the production code works.
- `tracing-test`-style log-capture assertions: the production code path actually emits a log matching the captured substring. Verify by reading the production code's `tracing::*!` call. If the code emits `tracing::error!(?event, "x")` and the test asserts `logs_contain("ConsumerPanic")`, the test passes iff `Debug` of `event` contains `"ConsumerPanic"` — which depends on the enum's Debug impl. Fragile; flag.
- **Honesty-contract regression coverage:** for code that touches the closed-loop attribution UI or any other honesty-load-bearing surface, a test must exist that asserts the negative-path text contains the literal expected string (e.g. `assert!(rendered.contains("degraded"))`) per the architecture's §2.4 honesty-contract requirement. Buddy verifies these exist where the diff touches Group C surfaces.

### (5) Internal consistency — function signatures, types, plan-vs-code

The (d) check from planning-phase carries over with code-level specificity. Buddy verifies:
- Function signatures in the diff match the public-API spec in the relevant plan document (week 2 plan §3.2, §3.4, §3.6, etc.). If the plan declares `pub fn into_supervisable_parts(self) -> (..., SessionShutdownHandle)`, the implemented signature must match.
- Type names in the diff match the plan's type names. `EtwSession<S: EtwSysCalls = RealEtwSysCalls>` in the plan → same generic shape in the code.
- Where the diff diverges from the plan, the diff includes a follow-up commit-or-PR that updates the plan to match the code. Code-leads-plan is acceptable iff the plan catches up before merge; silent divergence is a flag.
- Resource lifecycle: every `StartTraceW` has a matching `ControlTraceW(STOP)` on every path (clean shutdown, panic path, error-return path). Buddy reads the code's exit paths and verifies cleanup runs.

---

## Output format for buddy responses

```
## (1) Language-mechanics safety: PASS|FAIL
<list each unsafe block in the diff with its file:line and one-sentence verdict on the SAFETY comment quality>

## (2) Concurrency correctness: PASS|FAIL
<list each piece of shared state with the synchronization mechanism + verdict>

## (3) Error handling: PASS|FAIL
<list any swallowed results, unjustified panics, mismatched-severity logs>

## (4) Test-asserts-what-it-says: PASS|FAIL
<for each new test in the diff: name → assertion alignment verdict>

## (5) Internal consistency (plan-vs-code + lifecycle): PASS|FAIL
<list any signature mismatches; any missing teardown paths>

## Overall verdict
PROCEED | STOP-ON-(N) | STOP-ON-MULTIPLE
<one or two sentences>

## Category-of-issue observation
<For the user's process flag: what category did the findings fall into?
- New category vs prior round: rhythm working as intended.
- Same category as prior round: flag explicitly; user decides whether structural intervention is needed (per the revised watch-signal criteria in the week-2 plan footer: count similar/growing, severity similar/growing, or self-pass empty while buddy still finds issues).>
```

---

## §X Cross-document consistency dependencies

Per **META 1** (extended (d) decided alongside the v4.2 plan amendment, 2026-05-17): every artifact must agree with itself AND with the authoritative documents it references. The implementation-phase (5) question applies the same rule at the code level — function signatures, type names, and behavior contracts in the diff must agree with the plan and architecture documents they reference.

**Dependency arrows (define once, reference forever):**

- **Plan documents** (e.g., `spike/group-a-week-2-plan.md`) must agree with: `audit/v0.7-architecture.md`, `spike/etw-schemas.md`, `spike/etw-edr-report.md`. Buddy on a plan-document PR checks the cited sections in those files actually exist and say what the plan says they say.
- **Implementation-phase format** (this document) must agree with: the relevant plan + architecture. Buddy on a code PR using the 5Q format checks the diff's function signatures + type names against the plan's specification.
- **Spike reports** (e.g., `spike/group-a-week-2-report.md`, `spike/mac-side-uncertainties.md`) must agree with: prior architecture + prior spike findings if referenced. Buddy on a spike-report PR checks cited prior-spike commands + outputs are reproducible.
- **Architecture amendment PRs** (e.g., `proposal/v0.7-arch-mode5-amendment`) must agree with: the architecture sections they amend + any plan that references those sections. A mode-5 amendment that changes the disposition string requires updating every plan section that quotes the old disposition.

**Sweep discipline** (per META 1 + Entry 5 of `audit/buddy-disagreements.md`): when a (d) check finds an issue class at site X, the same class must be checked exhaustively across the document, not patched at the one site where it surfaced. Example: Day 3's `pub(crate)`-vs-`tests/`-directory visibility mismatch was caught in build_gate first; the sweep surfaced the same pattern in degradation_tests and supervisor_tests. Without sweep, those two would have shipped broken.

**Self-pass technique with verification commands** (per Entry 4 of `audit/buddy-disagreements.md`): self-passes that make positive claims ("section X exists at line Y") must include the literal verification command + its output. `grep -n "^## Entry" audit/buddy-disagreements.md` proves the entries exist; `sed -n '120p' audit/v0.7-architecture.md` proves §2.1's "Survives service restarts" claim. The verification ground rule from `spike/etw-edr-report.md` §8 (literal output for machine-state claims) extends to self-passes: a self-pass that just says "checked, looks fine" is not a self-pass.

---

## How this format will be exercised in Group A week 2

- **End of Day 5:** the week-2 PR is opened with the full implementation diff + `spike/group-a-week-2-report.md` (the EOD report per week 2 plan §12). Buddy runs the five-question format on that PR.
- **Day 3 mid-week checkpoint (optional):** if Day 3's deliverable surfaces something complex (the asm-codegen-parity check is the most likely source of complexity), the engineer can run buddy on the Day 3 commit alone with the five-question format. Optional, not required.
- **Mid-week stop-gate trips:** if any of the Day 1-5 stop gates fires (per week 2 plan §6), buddy runs on the surfaced commit before the user decides whether the stop resolves or escalates.

---

## Drift-prevention

This format itself is a document. If it gets edited, the edit goes through planning-phase 4Q review (the meta-rhythm: the document defining the rhythm follows the rhythm). The first edit will be after the week-2 EOD PR — buddy's actual experience exercising the format will surface what's missing.

Self-administered (1)-(5) pass on this draft before commit:

- **(1) Language-mechanics safety** — N/A; this is a process doc, no `unsafe`, no threading, no FFI.
- **(2) Concurrency** — N/A.
- **(3) Error handling** — N/A.
- **(4) Test-asserts-what-it-says** — meta-applies: does the format's stated check actually catch the failure mode it claims to catch? The (1)–(4) checks are themselves grep-able patterns (`unsafe` blocks, `.ok();` swallows, test name vs body alignment) — buddy can verify each in practice.
- **(5) Internal consistency** — section references resolve in their correct authoritative documents:
  - Week 2 plan refs (§3.2, §3.4, §3.5, §3.6, §6, §12) → `spike/group-a-week-2-plan.md` on main.
  - §2.4 (honesty-contract) → `audit/v0.7-architecture.md` on main, NOT the week 2 plan. (The self-pass at the bottom of v4.2's draft bundled §2.4 with the plan-doc refs — Self-pass Finding S-B in v4.2 amendment; that was a doc-pass mistake, not a real cross-doc inconsistency. Lesson captured in `audit/buddy-disagreements.md` Entry 4.)

---

## Status: APPROVED — 2026-05-18

Buddy 4Q with extended (d) ran on DRAFT v2 (commit `d05c431`),
returned STOP-ON-(d) with one mechanical sweep-discipline finding;
v2.1 patch (commit `0997b1e`) applied the fix inline; this APPROVED
flip is the tiny separate commit per the established rhythm. First
real use: the Group A week-2 EOD PR (already opened as PR #79 and
merged via squash to `main` 2026-05-18 at commit `6f972b6`). Future
week 3+ PRs use this format with the broadened (1) Language-mechanics
safety scope.
