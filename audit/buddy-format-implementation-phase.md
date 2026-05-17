# Buddy-system format — implementation phase

**Status:** DRAFT — pending buddy review per user instruction (2026-05-17).
**Authoritative inputs:** `audit/buddy-disagreements.md` Entry 1 (planning-phase 3Q), Entry 2 (planning-phase 4Q + (d)), Entry 3 (v4 review).

The planning-phase four-question format `(a) authority / (b) scope / (c) feasibility / (d) internal-consistency` is for **documents**. Code review during implementation needs a different format tuned for runtime concerns the planning format can't cover: concurrency, error handling, FFI safety, what tests actually assert.

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

### (1) FFI safety

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

Also check:
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
## (1) FFI safety: PASS|FAIL
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

## How this format will be exercised in Group A week 2

- **End of Day 5:** the week-2 PR is opened with the full implementation diff + `spike/group-a-week-2-report.md` (the EOD report per week 2 plan §12). Buddy runs the five-question format on that PR.
- **Day 3 mid-week checkpoint (optional):** if Day 3's deliverable surfaces something complex (the asm-codegen-parity check is the most likely source of complexity), the engineer can run buddy on the Day 3 commit alone with the five-question format. Optional, not required.
- **Mid-week stop-gate trips:** if any of the Day 1-5 stop gates fires (per week 2 plan §6), buddy runs on the surfaced commit before the user decides whether the stop resolves or escalates.

---

## Drift-prevention

This format itself is a document. If it gets edited, the edit goes through planning-phase 4Q review (the meta-rhythm: the document defining the rhythm follows the rhythm). The first edit will be after the week-2 EOD PR — buddy's actual experience exercising the format will surface what's missing.

Self-administered (1)-(5) pass on this draft before commit:

- **(1) FFI safety** — N/A; this is a process doc, no `unsafe`.
- **(2) Concurrency** — N/A.
- **(3) Error handling** — N/A.
- **(4) Test-asserts-what-it-says** — meta-applies: does the format's stated check actually catch the failure mode it claims to catch? The (1)–(4) checks are themselves grep-able patterns (`unsafe` blocks, `.ok();` swallows, test name vs body alignment) — buddy can verify each in practice.
- **(5) Internal consistency** — section references all resolve; cross-references to week 2 plan section numbers (§3.2, §3.4, §3.5, §3.6, §12, §6, §2.4) all point at sections that exist on `plan/group-a-week-2` HEAD.

---

## Status: DRAFT — pending buddy review with the planning-phase four-question format

When buddy approves, this document moves to APPROVED. The first real use is the Group A week-2 EOD PR.
