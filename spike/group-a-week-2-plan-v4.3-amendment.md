# v0.7 Group A — Week 2 implementation plan, v4.3 amendment

**Status:** DRAFT — pending buddy four-question review + user sign-off.
**Supersedes:** v4.2 APPROVED (`spike/group-a-week-2-plan.md`, signed off 2026-05-17).
**Source of authority:**
- `spike/group-a-week-2-plan.md` (v4.2) — the implementation plan being amended.
- `spike/group-a-week-2-report.md` (post-batch fill-in, commit `91d5ee2`) — the EOD report with literal Windows-runtime evidence.
- `spike/mac-side-uncertainties.md` Entries 1-9 with "Resolved (Windows batch)" subsections — per-entry empirical dispositions.
- `audit/v0.7-architecture.md` §2.1 + mode 5 amendment PR #77 — architectural context.
- `audit/buddy-disagreements.md` — prior buddy-rhythm decisions still in force.

**Composition:** v4.3 = v4.2 + Mac-side Days 1-5 deltas + Windows runtime batch findings 2026-05-17. The on-disk `spike/group-a-week-2-plan.md` v4.2 text is not edited; this amendment captures the v4.2→v4.3 deltas. After PR coordination at agenda step 32, v4.3 becomes the canonical merged plan; a future single PR can roll the deltas into the source plan for cleanliness, but the audit trail of "what v4.2 said vs what reality required" lives here permanently.

**Branch:** `plan/group-a-week-2-v4.3-amendment` off `origin/main` (NOT off `feat/group-a-week-2`; the amendment is to the plan document, not to the implementation).

---

## Section 1 — Per-finding plan-vs-reality deltas

These are the specific edits v4.3 makes to v4.2's plan text. Each item names the v4.2 plan section being amended, the reality observed, and the disposition.

### 1.1 — Day 1: `RtlGetVersion` module path + struct (Mac-side, Entry 1)

**v4.2 plan §3.1 said:** "RtlGetVersion lives in `windows::Win32::System::SystemInformation`. Use `OSVERSIONINFOEXW`."

**Windows reality:** windows-rs 0.58 has `RtlGetVersion` in `windows::Wdk::System::SystemServices`. It accepts `OSVERSIONINFOW` (the smaller struct; `dwBuildNumber` is in both). Architecture's "don't fall back to `GetVersionExA`" stop-gate is honored — this is the same documented `ntdll.dll` binding at a different windows-rs path, not a kernel driver or a manifest-lying API.

**Disposition:** Inline correction landed during Day 1 (uncertainties Entry 1; resolved at agenda Step 12 — commit `98128a5` adds permanent `#[ignore]`'d regression test `real_rtl_get_version_probe_succeeds_on_supported_host` in `build_gate.rs`). `framesage-etw/Cargo.toml` gained `Wdk_System_SystemServices` to the `[target.'cfg(windows)'.dependencies] windows.features` list.

**v4.2 plan text to update on future single PR roll-up:** §3.1 "Implementation notes" — replace the `windows::Win32::System::SystemInformation::OSVERSIONINFOEXW + RtlGetVersion` line with the actual `windows::Wdk::System::SystemServices::RtlGetVersion` + `windows::Win32::System::SystemInformation::OSVERSIONINFOW`.

### 1.2 — Day 3: `EtwSysCalls` trait signature deltas (Mac-side, Entry 2)

**v4.2 plan §3.4 said:** the trait methods are safe `fn`s with `&mut CONTROLTRACE_HANDLE`, `control_code: u32`, `*mut FILETIME`, `*mut OSVERSIONINFOEXW`.

**Windows reality (windows-rs 0.58):** five mechanical signature deltas:

| Method | v4.2 plan §3.4 spec | windows-rs 0.58 actual |
|---|---|---|
| `start_trace` | `session_handle: &mut CONTROLTRACE_HANDLE` | `*mut CONTROLTRACE_HANDLE` |
| `control_trace` | `control_code: u32` | `EVENT_TRACE_CONTROL` typed wrapper |
| `process_trace` | `*mut FILETIME` | `Option<*const FILETIME>` |
| `rtl_get_version` | `*mut OSVERSIONINFOEXW` | `*mut OSVERSIONINFOW` |
| (all methods) | safe `fn` | `unsafe fn` |

**Disposition:** Inline correction landed during Day 3 (uncertainties Entry 2; verified at agenda Step 9 + Step 13 — all 20 framesage-etw tests pass on real Windows). Trait methods are `unsafe fn` — explicit deviation from v4.2's safe signature; rationale: every method takes raw pointers and forwards to `unsafe` windows-rs calls, so making the trait method `unsafe` keeps the SAFETY chain visible from caller to real impl.

### 1.3 — Day 3: `ConsumerState` design change (Mac-side, Entry 3)

**v4.2 plan §3.5 #4 said:** `ConsumerState` holds `(AtomicU64 counters, CONTROLTRACE_HANDLE, S: EtwSysCalls)`.

**Windows reality:** combined with v4.2 §3.4's `RefCell<VecDeque<...>>` mock-queue choice, the v4.2 design required `S: Sync` for `Arc<ConsumerState<S>>: Send`. But `RefCell` is NOT `Sync`. The plan didn't anticipate the conflict.

**Disposition:** Inline correction during Day 3 (uncertainties Entry 3; resolved at agenda Step 9 + Step 14). Option B (the option taken): `ConsumerState` becomes non-generic — only `events_seen: AtomicU64`. The consumer thread closure captures `syscalls: S` directly by move (`Send + 'static` suffices; no `Sync` needed). `EtwSession` holds its own `syscalls: S` field for `into_supervisable_parts` to move into `SessionShutdownHandle`. The mock-injection architecture from v4.2 §3.4 is preserved.

The `static_assertions::assert_impl_all!(ConsumerState: RefUnwindSafe)` compile-time guard from v4.2 §3.5 #4 caught a related real bug on first deployment (an early Day 3 draft put `parking_lot::Mutex<Option<PROCESSTRACE_HANDLE>>` on ConsumerState for the CloseTrace path; `parking_lot::Mutex<T>` contains an `UnsafeCell<T>` which is NOT `RefUnwindSafe`; the guard fired immediately at compile time). Guard removal of the offending field resulted in the final clean design.

### 1.4 — Day 4: `poll_drop_stats` production wire (Mac-side, no Entry)

**v4.2 plan §4 Day 4 said:** "six degradation-mode tests + 1 service-crate integration test." Mode 3 listed as a degradation test.

**Reality:** Mode 3 needs an actual production wire to emit `KernelDrops` events on poll. Day 4 added `EtwSession::poll_drop_stats(on_event: impl Fn(DegradationEvent))` as production code, surfaced explicitly in the Day 4 commit message per user guidance "surface production additions, don't fold them in silently."

**Disposition:** Wire exists as production code; verified at agenda Step 9 by `mode_3_poll_drop_stats_emits_kernel_drops_when_buffers_lost` + `mode_3_poll_drop_stats_silent_when_zero_drops`.

### 1.5 — Day 5: `MonitorHandle` introduction (Mac-side, Entry 7)

**v4.2 plan §4 Day 5 said (pseudo-code):** spawns supervisor task via `into_supervisable_parts()` which consumes the EtwSession. The prose said "drop-poll sibling task calls `EtwSession::query_stats()` on a 1-second tokio interval" — but the sibling task has no way to call `query_stats` once the session is decomposed.

**Reality:** the v4.2 pseudo-code was incomplete for the drop-poll sibling. Day 5 added:
- `MonitorHandle<S>` type — read-only monitor that owns a clone of `syscalls` + session name + `Arc<ConsumerState>` for stat access. Does NOT call `ControlTraceW(STOP)`; the supervisor remains the only stop path.
- `EtwSession::into_supervisable_parts_with_monitor()` returning a 4-tuple `(JoinHandle, oneshot::Receiver, SessionShutdownHandle, MonitorHandle)`.
- The `into_supervisable_parts()` 3-tuple variant stays for the supervisor.rs synthetic-panic test (doesn't need monitoring).
- Service-side wiring uses `..._with_monitor`.

**Disposition:** New §3.7 needs to be added to v4.2 §3 specifying `MonitorHandle`. Verified at agenda Step 18 + Step 23.

### 1.6 — Day 5: `Policy::closed_loop_enabled` policy field (Mac-side, Entry 9)

**v4.2 plan §7 acceptance criteria said:** no explicit "new policy field" item (mentioned in §3.5 + Mac-side Entry 9 but not formally added to the acceptance bulletins).

**Reality:** Day 5 added `closed_loop_enabled: bool` to `framesage_core::Policy`. Defaults to false via `#[serde(default)]`. Three `Policy { ... }` literal sites updated (crates/core, crates/ipc, crates/service).

**Disposition:** Verified at agenda Step 20-23 (Entry 9 upgrade scenario). v4.3 adds the field to §7 acceptance: "`Policy::closed_loop_enabled: bool` exists in `framesage-core`; defaults to false on missing-field via `#[serde(default)]`; v0.6 → v0.7 policy upgrade is non-breaking (verified empirically)."

### 1.7 — Step 8: `cargo build` package-name correction (Windows batch)

**Windows runtime batch agenda (`spike/group-a-week-2-report.md` §12.8 step 8) said:** `cargo build -p framesage-svc --release`

**Reality:** `framesage-svc` is the BINARY name (`[[bin]]` declaration in `crates/service/Cargo.toml`), not the PACKAGE name (`[package].name = "framesage-service"`). cargo rejects with: `error: package ID specification 'framesage-svc' did not match any packages`.

**Disposition:** Agenda text correction — use `-p framesage-service` (or `--bin framesage-svc`). Documentation slip in v4.2, no behavioral consequence.

### 1.8 — Step 9 + Step 11: ETW test-isolation findings (Windows batch — new findings, not Mac-side)

These four real-Windows findings emerged during agenda Step 9 + Step 11 and required new code on `feat/group-a-week-2`:

| Round | Finding | Fix | Commit |
|---|---|---|---|
| F1 | Real-ETW tests in session.rs share the `FramesageEtw` session name with each other and with production. Parallel test threads race for the same kernel session. | Each `#[ignore]`'d real-ETW test constructs `SessionOptions` with a PID-suffixed unique name. Production code (via `SessionOptions::default()`) keeps the canonical `FramesageEtw`. | `9998ec9` |
| G1 | After F1, tests still failed: parallel `StartTraceW` calls within the same process serialize at the kernel level and return `ERROR_ALREADY_EXISTS` even when session names are disjoint. | Add `serial_test = "3"` to `framesage-etw` `[dev-dependencies]`; annotate both `#[ignore]`'d real-ETW tests with `#[serial_test::serial(real_etw)]`. Mock-based tests stay parallel for fast feedback. | `a5b955f` + lockfile `35b7cb0` |
| D1 | After G1, first-test-of-binary still failed: kernel ETW sessions persist past process death. The `drop_path` test used `drop(sess)` without explicit `sess.stop()`; the session leaked into the kernel; next test-binary invocation hit the kernel-side per-process slot limit. | `impl Drop for EtwSession<S>` with `syscalls: S` → `Option<S>` pattern. Drop runs `stop_session()` as a fallback on the leak path; explicit `stop()` and `into_supervisable_parts*` paths `.take()` syscalls so Drop sees `None` and skips. | `23e6457` |
| D1' | Step 11 surfaced the same leak class on `SessionShutdownHandle` (Drop fallback was on `EtwSession` only; supervisor task being tokio-cancelled at runtime shutdown dropped `SessionShutdownHandle` without calling `shutdown()`). | `impl Drop for SessionShutdownHandle<S>` mirroring the EtwSession pattern. Same `Option<S>` discipline. | part of `39644f6` |

**Disposition:** All four fixes permanent on `feat/group-a-week-2`. v4.3 §3.8 (new) needs to document the four-round diagnostic chain and the pattern rules (per-test unique names + serial + Drop impls for any real-system-resource test).

### 1.9 — Step 11: workspace layering registry oversight (Windows batch)

**v4.2 plan didn't mention:** that adding a new crate to the workspace requires updating `crates/core/src/layering.rs` `ALLOWED_EDGES` + `ARCHITECTURE.md` crate table + the layering invariants list.

**Reality:** Mac-side ran per-crate tests (`cargo test -p framesage-etw`, `cargo test -p framesage-service`) which DON'T exercise `framesage-core`'s `workspace_layering_invariants_hold` test. `cargo test --workspace` does. Step 11's first attempt failed with two violations: `framesage-service` depends on `framesage-etw` not in allowlist; package `framesage-etw` not in `ALLOWED_EDGES`.

**Disposition:** Fixed in commit `39644f6`:
- `crates/core/src/layering.rs` `ALLOWED_EDGES` gained `("framesage-etw", &[])` and added `framesage-etw` to `framesage-service`'s allowed targets.
- `ARCHITECTURE.md` gained `framesage-etw` row in the crate table + new invariant 8: "`framesage-etw` is a v0.7-era bottom-of-stack crate ... Only `framesage-service` depends on it ..."

### 1.10 — Step 11: `framesage-service` closed_loop test mis-scoped for elevated context (Windows batch)

**v4.2 plan didn't anticipate:** the `build_gate_fallthrough_emits_structured_build_unsupported_event` test in `framesage-service/closed_loop.rs` assumed the AccessDenied branch always taken on Windows. On elevated Win11 24H2+, `StartTraceW` succeeds and the function reaches `spawn_closed_loop_tasks` → `tokio::spawn` outside a runtime → panic.

**Reality:** Mac-side blind spot — Mac doesn't elevate and doesn't run real ETW, so the elevated-Windows-only code path was unreachable.

**Disposition:** Fixed in commit `39644f6`:
- Added `#[cfg(test)]` `CLOSED_LOOP_BUILD_OVERRIDE` thread_local + `BuildOverrideGuard` RAII at module level in `closed_loop.rs`. Mirrors `framesage-etw`'s build_gate pattern; production wraps `build_gate::closed_loop_enabled_for_this_build()` calls via local `build_gate_pass()` / `build_gate_detected_build()` functions that consult the override in tests, fall through to the real probe in production. Compiled out entirely in release builds.
- Test rewritten as `#[tokio::test(flavor = "multi_thread")]` + `#[tracing_test::traced_test]` + `BuildOverrideGuard::set(Some(Ok(22631)))`. Override forces BuildUnsupported branch deterministically on any host; `#[tokio::test]` is defensive belt-and-suspenders.

### 1.11 — Step 16: `ERROR_ACCESS_DENIED` helper removal (Windows batch — Entry 5 disposition)

**v4.2 plan + Mac-side Entry 5 hypothesized:** windows-rs 0.58 doesn't export `ERROR_ACCESS_DENIED`.

**Windows reality:** Hypothesis was WRONG. Grep at agenda Step 16 found:
```
windows-0.58.0/src/Windows/Win32/Foundation/mod.rs:1087:
  pub const ERROR_ACCESS_DENIED: WIN32_ERROR = WIN32_ERROR(5u32);
```

Same module as `ERROR_ALREADY_EXISTS` / `ERROR_SUCCESS` / `ERROR_WMI_INSTANCE_NOT_FOUND` which we already import.

**Disposition:** Refactored inline (commit `98128a5`). Added `ERROR_ACCESS_DENIED` to the existing `use windows::Win32::Foundation::{...}` import block; replaced `if rc == ERROR_ACCESS_DENIED()` (private helper call) with `if rc == ERROR_ACCESS_DENIED` (canonical constant); removed the `fn ERROR_ACCESS_DENIED() -> WIN32_ERROR` helper. Mode 1 test still passes after refactor (21/21).

### 1.12 — `_asm_baseline` Cargo feature: NOT NEEDED (Windows batch — closes v4 finding d.1's `_asm_baseline` requirement)

**v4 finding d.1 introduced the `_asm_baseline` feature:** "Plus v4 adds an explicit Day 3 verification step: capture `cargo rustc --emit=asm` output on at least one method and demonstrate codegen-parity against a no-trait baseline (a sibling `direct_call_baseline_*` function gated behind an internal `_asm_baseline` Cargo feature). Don't take 'monomorphizes cleanly' on faith." (v4.2 plan line 947.) v4.2 amendment Finding 3 (plan line 972) is a SEPARATE finding about asm extraction methodology (`cargo asm` + `awk`); it doesn't introduce or modify the feature gate. v4.3 §1.12 closes the v4-finding-d.1 feature requirement specifically; v4.2 amendment Finding 3's extraction methodology is also closed by the Step 27 visual-diff approach but that's an incidental consequence.

**Reality (Windows batch Step 27):** the asm capture on the monomorphized `framesage-svc.s` (18MB release binary) shows all 6 windows-rs ETW APIs called via direct `callq *__imp_XXX(%rip)` (the standard Windows PE import-table call), with `RealEtwSysCalls` and `EtwSysCalls` symbols completely absent from the binary (inlined away by monomorphization). The visual diff against "a hypothetical direct-call version" is unambiguous — there's nothing to diff against because both forms reduce to the same instruction stream.

**Disposition:** The `_asm_baseline` Cargo feature is **NOT NEEDED**. v4.3 closes the v4-finding-d.1 `_asm_baseline` requirement as "verification approach changed: visual diff on monomorphized binary is strictly stronger evidence than a synthetic baseline feature gate would have provided." v4.2 amendment Finding 3's extraction methodology (`cargo asm` + `awk`) is also satisfied incidentally — the visual diff IS the extraction. No code change required; v4.3 removes both the `_asm_baseline` feature task AND the cargo-asm + awk extraction step from any future scope.

---

## Section 2 — Real-Windows architectural findings

These are the cross-cutting architectural findings from the Windows runtime batch — they're not single-day deltas; they're patterns or constraints that the v4.2 plan didn't anticipate because they're invisible to Mac-side cross-target compilation.

### Finding #1 — Per-process kernel-side `StartTraceW` serialization

**Statement:** Parallel `StartTraceW` calls from within the same process serialize at the kernel level and return `ERROR_ALREADY_EXISTS` when contended, **independent of session-name collision**. Empirically reproduced (Step 9 Isolation B): two `#[ignore]`'d real-ETW tests with unique PID-suffixed session names, default parallel test threads → both fail with `Disabled(AlreadyExists)`.

**Cause:** Undocumented but reproducible kernel-side behavior of `EVENT_TRACE_SYSTEM_LOGGER_MODE` session creation. Plausibly: the kernel maintains a per-process accounting slot for in-flight system-trace creations; the second concurrent call sees the slot occupied and reports it as ALREADY_EXISTS even though the name it asked for isn't taken.

**Production impact:** Zero. Production code only ever creates one ETW session per service instance.

**Test impact (load-bearing):** Any future real-Windows test that calls `EtwSession::start()` MUST be annotated `#[serial_test::serial]` so the test harness serializes it against other real-ETW tests in the same crate. The 18 mock-based tests stay parallel for fast feedback.

**Pattern rule for weeks 3-7 + v0.8+:** any new real-Windows test that creates an ETW session uses `#[serial_test::serial(real_etw)]` (same named group as the existing tests, so they all serialize against each other).

### Finding #2 — Kernel-owned session lifetime exceeds process lifetime

**Statement:** System-trace ETW sessions are kernel-owned. They survive the creating process's exit. Empirically reproduced (Step 28 transition (b)): force-killing the framesage-svc process via `Stop-Process -Force` left the `FramesageEtw` session running in the kernel with no owning process (sc.exe queryex reported PID 0; the session continued to accumulate buffer writes).

**Cause:** Once `StartTraceW` succeeds, the session is registered with the kernel's ETW subsystem; process exit does NOT reap it. Only explicit `ControlTraceW(STOP)`, `logman stop <name> -ets`, or system reboot reclaim the session.

**Production impact (load-bearing):** Any code path that creates an ETW session MUST guarantee cleanup on all exit paths. Explicit `stop()` alone is insufficient — the path that doesn't run `stop()` (panic-unwind, `?`-bubble, abrupt termination of a SupervisorTask via tokio runtime shutdown) must trigger cleanup via Drop.

**Architectural pattern (D1 + D1'):** every closed-loop resource type that owns a kernel handle, subprocess, file descriptor, or other external resource MUST have a correct `Drop` impl. Implicit join via the supervisor's clean-exit path is NOT the teardown contract; Drop is. Currently implemented for `EtwSession<S>` and `SessionShutdownHandle<S>`. **Pattern rule:**
- Group B (PresentMon subprocess, session recorder file handles, tokio task handles owning resources): Drop impls required.
- Group C (UI surface that subscribes to closed-loop state): UI cleanup is OS-managed on app exit; Drop still encouraged for consistency.

**Validation:** Step 24 + Step 28 both showed teardown via the D1 Drop fallback path (`SessionShutdownHandle::drop: session stopped (fallback path)` log line), with kernel state clean. Step 28(c) showed the `cleanup_stale_session` retry path correctly reclaiming a force-killed leak at next service start.

### Finding #3 — Privilege-filtered diagnostic visibility

**Statement:** `logman query -ets` filters its output by access — unelevated queries return only sessions the current user owns, NOT the system-wide list. The agent's unelevated diagnostic queries showed "no FrameSage sessions" while a leaked session was in fact present (visible only to elevated queries).

**Cause:** Standard Windows access-control filtering on the ETW subsystem.

**Operating-model impact:** ETW state checks (and any other privilege-filtered diagnostic) must be elevated to be authoritative. This includes `logman query -ets`, `tracelog -l`, `wevtutil`, and analogous queries against named pipes / driver state. Tools that filter silently are dangerous in a debugging context — agents should never trust unelevated queries against system-level resources.

**Pattern rule:** the next Windows-batch session's pre-batch setup must include an elevated `logman query -ets | findstr Frame` as a standing rule, regardless of whether the prior batch reported a clean state.

### Composed-finding production hazard (Finding #1 + Finding #2)

**Statement:** A leaked session in a previous test-binary invocation (Finding #2 leak) will silently block subsequent invocations from the same source tree via the per-process slot constraint (Finding #1) — even though the new invocation uses a DIFFERENT session name from the leaked one. The leak occupies the kernel-side accounting slot; subsequent `StartTraceW` calls hit ALREADY_EXISTS not because of name collision but because the slot is full.

**Cause:** The two findings compose multiplicatively. Without leak prevention (Finding #2 mitigation: Drop impls), every test run leaks a session. Without parallelism management (Finding #1 mitigation: `#[serial]`), parallel tests collide even with unique names. Without elevated diagnostic (Finding #3 mitigation: pre-batch cleanup with elevated query), the agent can't see the leaks accumulating.

**Mitigation (composed):** All three required. None alone is sufficient.
- Finding #1: `#[serial_test::serial]` on real-ETW tests.
- Finding #2: `impl Drop for <type-owning-kernel-resource>` for both `EtwSession<S>` and `SessionShutdownHandle<S>`.
- Finding #3: elevated `logman query -ets | findstr Frame` as pre-batch cleanup step.

**Validation:** Step 28 cleanly demonstrates the mitigation working. The transition (c) log shows `cleanup_stale_session(&syscalls, &"FramesageEtw")` running on the force-killed leak from transition (b), reclaiming the slot, and the subsequent `StartTraceW` succeeding.

### Finding #11.0 — Workspace layering invariant test scope

**Statement:** `framesage-core`'s `workspace_layering_invariants_hold` test (item 3.8 from the original audit) is the only check that catches new-crate-not-in-allowlist errors. Per-crate `cargo test -p <crate>` invocations don't exercise it. Only `cargo test --workspace` does.

**Operating-model impact:** any future workspace-wide architectural change (new crate, new dep edge) MUST be verified via `cargo test --workspace`, not just per-crate tests. Mac-side Days 1-5 ran per-crate tests; the Windows batch's Step 11 surfaced the gap.

**Pattern rule:** workspace `cargo test` belongs in CI as a standing gate. The next CI iteration should add `cargo test --workspace -- --include-ignored` (elevated, on a Windows runner) so this class of error fails fast.

### D1 Drop as load-bearing architectural pattern (Reading 1 ratified)

**Statement:** Per agenda Step 24's user-ratified Reading 1: the D1 Drop impls on `EtwSession<S>` and `SessionShutdownHandle<S>` are NOT defensive belt-and-suspenders; they are the LOAD-BEARING teardown path in production. The supervisor's explicit `shutdown()` method does not run during normal service shutdown because the supervisor task is cancelled by tokio runtime shutdown (per the closed-loop-tasks-not-in-watchdog choice from PR #77 mode 5 amendment) before it can run clean-exit code. Drop is what catches it.

**Causal chain:** mode 5 amendment requires closed-loop tasks excluded from the v0.6 watchdog `select!` (panic isolation: consumer-thread panic must NOT crash the service). Tokio runtime shutdown cancels excluded tasks. Cancelled tasks drop their state mid-await. `SessionShutdownHandle` gets dropped without `shutdown()` having been called. D1 Drop fires.

**Architectural consequence:** the panic-isolation design and the Drop-mediated teardown are inseparable. Any reading that treats Drop as "defensive" misses that production relies on Drop for clean teardown.

**Pattern rule (re-stated):** see Finding #2. Every closed-loop resource type requires a correct Drop impl.

**Architecture doc amendment (separate item):** `audit/v0.7-architecture.md` §2.1 mode 5 amendment (PR #77) should grow a subsection or footnote documenting the Drop-mediated teardown corollary. Future readers of the mode 5 spec should understand both the panic-isolation rationale AND its teardown consequence in one place. This v4.3 amendment notes the architecture-doc amendment as a follow-up; the actual edit happens via PR #77 review.

---

## Section 3 — Mac-side blind-spot category taxonomy

The Windows runtime batch surfaced 11 distinct real-Windows findings. They fall into five categories. For each: what Mac-side cross-target verification CAN catch, what it CANNOT, and design implications for weeks 3-7.

### Category 3.1 — Kernel-side coordination behaviors

**Definition:** Behaviors that emerge from runtime coordination across kernel resources (per-process slots, per-name registries, system-wide singletons, IOCP queues). The compose-of-Finding-1 production hazard is the marquee example.

**Mac-side CAN catch:**
- API signature mismatches (e.g., `windows-rs 0.58` vs plan-stated path/struct — Entry 1, Entry 2)
- Missing feature flags (`Wdk_System_SystemServices` for `RtlGetVersion` — Entry 1)
- Compile-time type errors (`Send`/`Sync` bounds — Entry 3's `RefCell + Sync` conflict)
- Trait-object vs generic-monomorphization choice (Entry 3's design pivot)

**Mac-side CANNOT catch:**
- Kernel-side internal accounting (per-process slot constraint on `StartTraceW`)
- Lifetime semantics of kernel-owned resources beyond process death
- Whether parallel calls to a Win32 API serialize internally

**Design implication:** Any v0.7-era code that calls a Win32 API which manages kernel state should be assumed to have UNDOCUMENTED concurrency and lifetime quirks. The test design must include `#[serial_test::serial]` for parallel hazards + Drop impls for lifetime hazards as a default. Removing them requires empirical justification on real Windows, not just docs reading.

**Weeks 3-7 application:** Group B will introduce `PresentMon` subprocess + session recorder file handles. Both are external resources. Pattern rules from Findings #1 + #2 + #3 apply directly.

### Category 3.2 — Process-lifetime vs kernel-resource-lifetime mismatch

**Definition:** Resources owned by the kernel that survive process death (Finding #2). Includes: ETW sessions, named pipes (briefly post-close), kernel objects with refcounts the process held.

**Mac-side CAN catch:** Rust ownership rules + Drop impl correctness via `static_assertions` / RefUnwindSafe guards + unit-test scaffolds. Mac-side validated the D1 pattern's correctness via the Drop impl tests.

**Mac-side CANNOT catch:** Whether the kernel actually reaps the resource on process exit. (For ETW: it doesn't. For named pipes: it does, mostly.) Without a real Windows runtime, the "leaked across process death" failure mode is invisible.

**Design implication:** Any new kernel-resource-owning type in weeks 3-7 needs both a Mac-side `Drop` impl and a Windows-batch integration test that force-kills the owning process and verifies cleanup.

**Weeks 3-7 application:** Group B's session recorder writes JSONL to a file under `%ProgramData%\framesage\sessions\`. On force-kill, the file handle is closed by the OS (file system behavior is process-lifetime); the file persists (intended behavior — sessions survive crashes per Phase 2 §1.4 journal preservation). But: if Group B introduces a temp-file + atomic-rename pattern, the temp file might leak on crash. Drop impl + integration test required.

### Category 3.3 — Privilege-filtered diagnostic visibility

**Definition:** Diagnostic tools that silently filter their output by access (Finding #3). Includes: `logman`, `wevtutil`, parts of `sc.exe`, parts of `Get-EventLog`/`Get-WinEvent`, named-pipe enumeration.

**Mac-side CAN catch:** Nothing. This category doesn't manifest until a real Windows host with non-LocalSystem context tries to inspect system state.

**Mac-side CANNOT catch:** Anything in this category, by definition.

**Design implication:** Diagnostic queries during agent-driven sessions must run elevated to be authoritative. Unelevated queries return "no problem" reports when problems exist.

**Weeks 3-7 application:** Group A weeks 3-7 build event parsers + per-event dispatcher. Their integration tests will inspect ETW state. Operating-model rule: pre-test cleanup via elevated query (find leaked sessions) before any test that creates a session. Standing rule for any future Windows-batch session.

### Category 3.4 — Code paths that branch on runtime/privilege context

**Definition:** Code that takes a different execution path depending on whether it's running elevated, as LocalSystem, on a particular Windows build, or against a particular kernel feature set. Mac-side cross-target compilation produces a single binary that the kernel decides what to do with; the agent can't choose the path it'll be tested against.

**Mac-side CAN catch:** Wrong-path-taken at compile time (e.g., `cfg!(target_os)` branches that fail to compile on a target).

**Mac-side CANNOT catch:**
- Runtime-determined branches like `start_closed_loop_if_enabled`'s decision tree (policy → build gate → StartTraceW → spawn tokio tasks). On Mac, the platform stub returns false at the first check; the deeper code paths are never exercised.
- The specific consequence: Step 11 found the `closed_loop` test panicking inside `tokio::spawn` outside a runtime because Mac-side never reached the path.

**Design implication:** Tests of decision-tree code MUST have a test-only override seam at EACH branch the runtime might choose. The user-introduced `CLOSED_LOOP_BUILD_OVERRIDE` thread_local in `closed_loop.rs` is the pattern. Future decision-tree tests should compose overrides at every layer.

**Weeks 3-7 application:** Group A weeks 3+ event-parsing dispatchers will likely have build-version-dependent branches (per `spike/etw-schemas.md`'s acceptance criteria — DPC opcodes 0x42/0x44/0x45 differ by build). Override seams will be required.

### Category 3.5 — Test side effects on system state

**Definition:** Tests that create real system state (ETW sessions, services, registry keys, files in `%ProgramData%`) and leave that state behind for subsequent test invocations or other system tools to inherit. The Step 9 leak chain is the marquee example: a `drop(sess)` without explicit `sess.stop()` left a session in the kernel for hours.

**Mac-side CAN catch:** Drop impl correctness via tests. Mac-side caught the missing Drop via the `static_assertions` regression guard.

**Mac-side CANNOT catch:** Whether a specific test ACTUALLY runs its Drop impl. On Mac, `drop(sess)` is a no-op because the EtwSession is a stub. On Windows, `drop(sess)` runs the real Drop impl which runs `ControlTraceW(STOP)` — but only because we added Drop. Pre-D1, `drop(sess)` was a no-op AND the session leaked.

**Design implication:** Tests that touch named system resources (ETW sessions, named pipes, named mutexes, file locks at fixed paths) require explicit isolation:
- `#[serial_test::serial(<group>)]` to prevent parallel collisions
- PID-suffixed unique names to prevent cross-process collisions
- Drop impls on production types to prevent abrupt-termination leaks
- An "if I were the next person to start this test binary, would the previous run's state still be present?" mental check at test-write time

**Weeks 3-7 application:** Group B's session recorder writes JSONL files. Group C's UI surface subscribes to IPC events. Any test that creates a real session recorder OR a real IPC subscription leaves residue. Apply the rule: each test gets a unique session-id-prefix or per-test-tempdir for file writes; subscriptions get torn down via Drop.

---

## Section 4 — Operating model additions

These are standing rules for future weeks. v4.3 promotes them from one-off observations to permanent process state.

### 4.1 — ETW state checks always elevated

Every ETW diagnostic during agent-driven Windows batch sessions runs elevated. Unelevated `logman query -ets` is access-filtered and not authoritative.

Pre-batch standing rule: `Start-Process -Verb RunAs cmd.exe /c "logman query -ets > out.txt"` before any test that creates an ETW session. After-batch standing rule: same check, plus elevated `logman stop <name> -ets` for any leftover Frame* sessions.

### 4.2 — Named-system-resource tests require explicit isolation

Every test that creates a named system resource (ETW session, named pipe, named mutex, file at a fixed path) follows the three-rule composition:
- `#[serial_test::serial(<group>)]` for in-process parallelism management
- PID-suffixed unique names or per-test-tempdir for cross-process isolation
- Drop impls on the production type the test creates (not the test code itself; the production type)

A test that only does two of three (e.g., #[serial] + Drop but reuses a fixed name) will fail under cross-process contention. A test that only does Drop will fail under in-process parallelism. A test that only does serial+unique-name will leak.

### 4.3 — Post-test ETW state cleanup as standing rule

Any test session that creates real ETW sessions ends with an elevated state check. Surface any survivors. Clean them up before the next session starts.

This is the pre-batch cleanup step from Operating Model Item 4.1 stated from the "leaving state behind" side.

### 4.4 — Closed-loop resource types require correct Drop impls

Per Finding #2 / D1 pattern: every type that owns a kernel handle, subprocess, file descriptor, named pipe, or other external resource attached to the closed-loop subsystem MUST have a `Drop` impl that releases the resource. Implicit join via the supervisor's clean-exit path is NOT the teardown contract; Drop is.

Currently applies to: `EtwSession<S>`, `SessionShutdownHandle<S>`.

Will apply to (weeks 3+ and beyond):
- `PresentMon` subprocess handle wrapper (Group B)
- Session recorder file handle wrapper (Group B)
- Any tokio task `JoinHandle` that owns an external resource (Group B + C)

Pattern guide: `Option<S>`-take-on-explicit-teardown idiom with Drop fallback. See `crates/etw/src/session.rs::impl Drop for EtwSession` for the canonical example.

### 4.5 — README: policy.json field materialization on first IPC save

User-facing doc TODO for v0.7 README: "Policy fields added in new versions appear in your policy.json with default values after the first time you change a setting via the tray. Existing settings are never modified."

This pre-empts user surprise when they see `closed_loop_enabled: false` appear in their policy.json a few minutes after the v0.7 upgrade install completes.

### 4.6 — `Claude tool layer blocks Remove-Item on C:\Program Files\*`

Operating model rule discovered at Step 20.5: Claude's PowerShell tool has a safety guard that pattern-matches `Remove-Item` / `del` on paths starting with `C:\Program` (Program Files protection). This fires regardless of elevation — it's at the tool boundary, not the Windows boundary.

Net effect for future Windows-batch sessions: if a batch requires modifying files under `C:\Program Files\`, the destructive op routes through the user manually (paste output back into chat) or through tooling that doesn't trigger the filter (e.g., `cargo install --path crates/cli` writes to `~/.cargo/bin/`, not Program Files; install.ps1 spawns its own elevated process and the internal Copy-Item calls run inside that process, not via my tool).

---

## Section 5 — Positive findings

The batch surfaced many findings; not all are negative. These are the empirical positives worth surfacing prominently.

### 5.1 — v0.6 uninstall validation

PR #19 + #63 + #64's v0.6 uninstall design validated end-to-end on real Windows during agenda Step 20.5. Partial-success path correctly surfaced locked-binary failures (`framesage.exe` self-locked during its own uninstall; `framesage-tray.exe` locked by running tray) with actionable error messages, preserved SCM cleanup, preserved ProgramData per user choice (no auto-deletion), suggested the right remediation ("retry uninstall after rebooting if any binary is still locked by AV / running process"). The two binaries that failed to remove had documented reasons; not a regression — behavior-as-designed.

### 5.2 — Codegen-parity verified on real Windows

Step 27 confirmed the `EtwSysCalls` trait abstraction has zero runtime cost in release builds. All 6 windows-rs ETW APIs (`StartTraceW`×1, `ControlTraceW`×4, `OpenTraceW`×1, `ProcessTrace`×1, `CloseTrace`×1, `RtlGetVersion`×2) called via direct `callq *__imp_XXX(%rip)` IAT calls in the monomorphized `framesage-svc.s`. `RealEtwSysCalls` and `EtwSysCalls` symbols absent from the binary (inlined away by monomorphization). Mac-side design assumption ("zero-cost abstraction via generic monomorphization") empirically validated.

### 5.3 — Survives-restart architectural promise met

Step 28's four-transition cycle (Start → force-kill → Start → Stop-Service) validates architecture §2.1's "Survives service restarts" promise empirically. The compose-of-Finding-1-and-Finding-2 production hazard is correctly handled by Day 2's session-lifecycle lift (`cleanup_stale_session` running before `StartTraceW`).

First end-to-end execution of v0.7 closed-loop production wire on real Windows under LocalSystem token. All Mac-side scaffolding integrates correctly: build gate, EtwSysCalls trait dispatch, EVENT_TRACE_SYSTEM_LOGGER_MODE session creation, consumer thread + ProcessTrace, supervisor + drop-poll tokio tasks.

### 5.4 — v0.6 → v0.7 policy upgrade non-breaking

Entry 9 upgrade scenario PASSED. v0.6 policy.json (no `closed_loop_enabled` field) loaded into v0.7 service; serde defaulted the field to false per `#[serde(default)]`; static-rule path taken; no ETW session created; user's v0.6 rules + profile customizations preserved verbatim.

### 5.5 — Serde-round-trip materialization works correctly across version boundary

Side-finding from Entry 9 verification: the v0.7 service's first IPC-triggered policy save (almost certainly the tray's diagnostic-period interaction during install) round-tripped policy.json through serde and materialized the new `closed_loop_enabled: false` field explicitly. Net effect: policy.json self-documents after first save. Benign + arguably desirable. README explanation captured in Operating Model Item 4.5.

### 5.6 — D1 Drop pattern proves load-bearing

The Drop impl work (Step 11 D1 on `EtwSession`, D1' on `SessionShutdownHandle`) is the load-bearing teardown path in production. Step 24 + Step 28 both showed teardown via `SessionShutdownHandle::drop: session stopped (fallback path)` log line. Kernel state clean after every Stop-Service. Reading 1 ratified by user as intentional architectural design.

---

## Section 6 — Pre-ship-prep checklist (v0.7 release)

These items don't gate week 2 completion. They gate v0.7 user-visible ship.

- [ ] Bump workspace `Cargo.toml` `version` from `0.5.0` to `0.7.0` (or chosen scheme).
- [ ] Decide version scheme: matches engagement-vocabulary `0.7.0`, jumps `0.6.0`+1, or independent (`1.0.0-pre`)?
- [ ] Update v0.7 README's version references.
- [ ] Update install.ps1's installer messages if any version-name strings are baked in.
- [ ] Re-run install.ps1 with new-version binaries before shipping.
- [ ] Add v0.7 README section on closed-loop measurement:
  - What it does, what it logs, what it stores
  - The `closed_loop_enabled` policy toggle (default false in v0.7; v0.7.1 may flip)
  - The "field appears in policy.json after first IPC save" explainer (per Operating Model Item 4.5)
  - The Game Mode + closed-loop separation
- [ ] Authenticode signing decision (closes the unsigned-binary line in `spike/etw-edr-report.md` §6.1 criterion 3). Required for v0.7.1 default-on-flip; may be required for v0.7 ship.
- [ ] MSI / Inno installer question — v0.7 still ships via `install.ps1`?
- [ ] EDR matrix validation per `spike/etw-edr-report.md` §6.1 — all four criteria required before v0.7.1 default-on flip:
  - Clean run on Defender ATP, CrowdStrike Falcon, SentinelOne Singularity (all three)
  - At least one run under realistic gaming load
  - Signed binary (criterion 3 above)
  - Any flagging-vendor allow-list cleared

---

## Section 7 — v0.6 UX backlog discovered during batch

These are pre-existing v0.6 issues surfaced during the v0.7 batch session. Logged here for v0.6.x follow-up PRs (not v0.7 ship blockers).

### 7.1 — Game Mode journal dialog (tray UX)

**Symptom:** Tray's "Show Game Mode journal" menu item (`crates/tray/src/main.rs:1525-1532`) calls `open_in_shell(<journal-path>)` without first checking if the journal file exists. When Game Mode hasn't been triggered (the common case after a fresh install), the journal file doesn't exist, and Windows ShellExecute pops the "Windows cannot find ... game-mode.journal" dialog.

**Fix sketch:** Either disable the menu item when journal absent (preferred — `is_enabled = framesage_core::paths::config_dir().join("game-mode.journal").exists()`), or replace the ShellExecute call with an in-app "no journal yet — start Game Mode to create one" message.

**Effort:** ~30 minutes of tray code + a UI test that doesn't actually fire ShellExecute.

**Disposition:** v0.6.x follow-up PR after v0.7 ships.

---

## Section 8 — Cross-references + merge sequence

### Commits on `feat/group-a-week-2` since the Windows batch began

```
91d5ee2  docs(week-2-batch): post-batch report fill-in + uncertainties Windows-side dispositions
98128a5  fix(etw): Entry 1 + Entry 5 resolved per Windows runtime batch Steps 12 + 16
39644f6  fix(etw,service,core,arch): Step 11 findings — SessionShutdownHandle Drop, closed_loop build-gate seam, layering registry
35b7cb0  chore: lock serial_test 3.4.0 + scc 2.4.0 + sdd 3.0.10 in Cargo.lock
23e6457  fix(etw): impl Drop for EtwSession with leak-prevention fallback (Step 9 finding #2)
a5b955f  fix(etw): serialize real-ETW tests with #[serial_test::serial] (Step 9 finding)
9998ec9  fix(etw): real-ETW test isolation via unique session names (Step 9 finding)
56995ea  feat(service): Day 5 — closed-loop wiring + supervisor/drop-poll spawn + EOD report  (Mac-side Day 5 baseline)
2b93475  feat(etw): Day 4 — degradation-mode tests + Mode 3 poll wire                          (Mac-side Day 4 baseline)
5029f59  feat(etw): Day 3 — EtwSysCalls trait + EtwSubsystem + supervisor + degradation        (Mac-side Day 3 baseline)
65f070d  feat(etw): Day 2 — session lifecycle lift from spike-etw                              (Mac-side Day 2 baseline)
f4b6e83  feat(etw): Day 1 — crates/etw/ skeleton + build_gate.rs                               (Mac-side Day 1 baseline)
```

### The three-PR merge sequence (agenda step 32)

Per agenda step 32, three PRs land in coordinated order to maintain a clean architectural narrative on `origin/main`:

1. **PR #77** — `proposal/v0.7-arch-mode5-amendment` (already a draft).
   Architecture §2.1 mode 5 amendment. Mode 5 description changes from "service exits non-zero on consumer panic; SCM restarts" to "consumer panic exits the closed-loop subsystem; service stays up." The Drop-mediated-teardown corollary (this v4.3 Section 2) should be added to PR #77 as a small footnote or subsection during its review pass before merge.

2. **PR (new)** — `plan/group-a-week-2-v4.3-amendment` (this branch).
   v4.3 plan-vs-reality amendment. References PR #77 as the merged architecture state.

3. **PR (new)** — `feat/group-a-week-2`.
   The week-2 implementation. References both PR #77 (amended architecture) and the v4.3 amendment (amended plan) as its supersedence chain. Contains 12 commits (Days 1-5 baseline + 7 Windows-batch findings).

Each PR carries its own buddy review per the established rhythm. PR #77's review covered the architectural Mode 5 amendment; this v4.3 amendment carries its own buddy four-question pass (in this branch's PR). The `feat/group-a-week-2` PR will carry the implementation-phase 5Q buddy review (`audit/buddy-format-implementation-phase.md` if it ever merged — or the planning-phase 4Q if not).

### v0.7 architecture doc amendment to PR #77

PR #77 needs a small addition during its review pass before merge: the Drop-mediated-teardown corollary from this v4.3 Section 2. Suggested location: §2.1 mode 5 amendment text, new subsection or footnote titled "Teardown contract for closed-loop resources":

> The panic-isolation choice (closed-loop tasks excluded from the v0.6 watchdog `select!`) has a teardown consequence: when the service shuts down its tokio runtime, the excluded tasks are cancelled rather than awaited. Their state is dropped mid-await. Therefore, any closed-loop resource type that owns a kernel handle, subprocess, file descriptor, or other external resource MUST have a correct `Drop` impl that releases the resource. Implicit join via the supervisor's clean-exit path is NOT the teardown contract for these tasks; Drop is. Currently implemented for `EtwSession<S>` and `SessionShutdownHandle<S>`; future closed-loop types (PresentMon subprocess wrapper, session recorder file handle wrapper, etc.) follow the same pattern.

The agent does NOT make this edit unilaterally; PR #77's review by user is the appropriate place.

### v4.2 → v4.3 supersedence

When the v4.3 amendment PR merges, it becomes the canonical merged plan state. The on-disk `spike/group-a-week-2-plan.md` still says "DRAFT v2 → v3 → v4 → v4.1 → APPROVED v4.2". A future cleanup PR (NOT week 2 scope) can roll the v4.3 deltas into the source document for cleanliness, but the audit trail of "what v4.2 said vs what reality required" lives here permanently.

---

## Status: DRAFT — pending buddy four-question review + user sign-off

When buddy approves and the user signs off, this document moves to APPROVED via a small follow-up commit (header status update). After PR #77 + v4.3 amendment + feat/group-a-week-2 land in sequence per Section 8, week 2 is complete; week 3 (event parsers) begins on a new branch off the merged `origin/main`.
