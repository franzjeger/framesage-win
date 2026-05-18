# Mac-side uncertainties — agenda for end-of-week Windows runtime batch

Per the user's standing instruction (2026-05-17 strategy shift):

> If during Day 2-4 you hit something that genuinely cannot be
> verified on Mac without speculation, or where the Mac-side
> cross-check passes but you have a credible hypothesis that real
> Windows would behave differently — commit the code AND open a
> small note here naming the hypothesis and what test would verify
> it during the end-of-week batch. This doc becomes the agenda for
> the Windows session.

Append-only. Each entry: what was done, the hypothesis, the
Windows-side verification command that resolves it. Resolved entries
get a "**Resolved:**" line appended; entries that turn into real bugs
become Entry-N in `audit/buddy-disagreements.md`.

---

## Entry 1 — Day 1 (2026-05-17): `RtlGetVersion` module path

**What was done:** Plan §3.1 said `RtlGetVersion` lives in
`windows::Win32::System::SystemInformation`. Day 1 verification found
it actually lives in `windows::Wdk::System::SystemServices` in
windows-rs 0.58. Cargo.toml gained `Wdk_System_SystemServices`
feature; binding works in cross-check.

**Hypothesis:** the documented `ntdll.dll` "system" fn binding (via
`windows_targets::link!`) resolves at runtime on Win11 24H2+ and
returns `dwBuildNumber == 26200` (or the actual build of the test
host).

**Windows-side verification:** `cargo test -p framesage-etw -- --nocapture build_gate` — three inline tests + once unblocked, a brief sanity
print of `detected_build()` from a one-shot binary. The build_gate
tests use the override seam, so they don't actually exercise the
real RtlGetVersion. A side-channel sanity check:

```powershell
cd crates\etw
cargo test -p framesage-etw -- --nocapture
# Verify three build_gate tests pass; if they do, the binding compiled
# and the override seam works. Real RtlGetVersion verification: 
# add a throwaway test like:
#   #[test]
#   fn debug_real_rtl() { eprintln!("real = {:?}", detected_build()); }
# Run with --nocapture, confirm eprintln'd build matches
# [System.Environment]::OSVersion.Version's build field.
```

**Resolved (Windows batch, 2026-05-17, agenda Step 12):** real
`RtlGetVersion` binding works on Win11 26200. Permanent #[ignore]'d
regression test `real_rtl_get_version_probe_succeeds_on_supported_host`
added to `crates/etw/src/build_gate.rs` (commit `98128a5`). Test run
captured:

```text
real RtlGetVersion: detected_build() = Some(26200) (expect Some(>= 26100)); MIN_BUILD_FOR_CLOSED_LOOP = 26100
```

Host cross-check `[System.Environment]::OSVersion.Version.Build = 26200`
matches the probe result exactly. Day 1's module-path correction to
`Wdk::System::SystemServices` + `OSVERSIONINFOW` struct verified
empirically. No further action.

---

## Entry 2 — Day 3 (2026-05-17): trait-signature deltas vs windows-rs 0.58

**What was done:** Plan §3.4 specified the `EtwSysCalls` trait
methods with the following signatures. windows-rs 0.58 turned out to
have several mechanical differences:

| Method | Plan §3.4 spec | windows-rs 0.58 actual |
|---|---|---|
| `start_trace` | `session_handle: &mut CONTROLTRACE_HANDLE` | `*mut CONTROLTRACE_HANDLE` (we wrap with `.as_mut().expect(...)` in the Real impl) |
| `control_trace` | `control_code: u32` | `EVENT_TRACE_CONTROL` (a typed wrapper around u32) |
| `process_trace` | `start_time: *mut FILETIME, end_time: *mut FILETIME` | `Option<*const FILETIME>` for both |
| `rtl_get_version` | `info: *mut OSVERSIONINFOEXW` | `*mut OSVERSIONINFOW` (smaller; `dwBuildNumber` is in both) |
| (all methods) | safe `fn` | `unsafe fn` in our trait (deviation from plan — see "Design decision" below) |

**Day-3 design decision (deviation from plan):** trait methods are
`unsafe fn`. Plan §3.4 left them safe. Rationale: every method takes
raw pointers and forwards to `unsafe` windows-rs calls. A safe trait
method that internally calls `unsafe` hides the FFI contract from the
caller; making the trait method `unsafe` keeps the SAFETY chain
visible (caller's `unsafe { syscalls.start_trace(...) }` matches the
real impl's `unsafe { StartTraceW(...) }`).

Mock impls have trivial `unsafe { }` bodies (they ignore the pointers
and return scripted values from `RefCell<VecDeque<_>>`).

**Hypothesis:** the trait surface compiles + tests pass on real
Windows because we matched windows-rs 0.58's actual signatures. No
behavioral delta vs Day 2's concrete implementation (since the trait
is a thin wrapper).

**Windows-side verification:** `cargo test -p framesage-etw -- --nocapture --include-ignored` — runs all mode tests + the
`real_etw_session_starts_and_stops_cleanly` ignored test. If any test
fails on signature mismatch, surface as a real bug.

**Resolved (Windows batch, 2026-05-17, agenda Step 9 + Step 13):** all
20 framesage-etw tests pass under `--include-ignored` on real Windows
(final state after four rounds of inline fixes — see report §12.3). The
trait signatures compile + link + execute correctly against windows-rs
0.58 on the actual Win11 26200 runtime. Five signature deltas
(start_trace `*mut`, control_trace typed `EVENT_TRACE_CONTROL`,
process_trace `Option<*const FILETIME>`, rtl_get_version `OSVERSIONINFOW`,
all methods `unsafe fn`) all validated empirically. No further action.

---

## Entry 3 — Day 3 (2026-05-17): ConsumerState design conflict (caught + resolved by static_assertions guard)

**What was done:** Plan §3.5 #4 specified ConsumerState holds
`(a) AtomicU64 counters, (b) CONTROLTRACE_HANDLE, (c) a clone of S:
EtwSysCalls`. Per the plan the catch_unwind soundness analysis
required `ConsumerState: RefUnwindSafe`, with a `static_assertions::assert_impl_all!`
guard in supervisor.rs to lock this in.

**Day-3 implementation:** the initial draft put `parking_lot::Mutex<Option<PROCESSTRACE_HANDLE>>`
on ConsumerState for the CloseTrace path. The `static_assertions`
guard fired immediately at compile time — `parking_lot::Mutex<T>`
contains an `UnsafeCell<T>` which is NOT `RefUnwindSafe`. The guard
caught a real bug on its first deployment.

**Resolution (inline):** the CloseTrace handle doesn't need to live
on ConsumerState — the consumer thread is the only caller, and the
handle can be a local variable inside `consumer_loop`. Field removed.

**Deeper Day-3 finding (also resolved inline):** plan §3.4 chose
`RefCell<VecDeque<T>>` for MockEtwSysCalls queues ("tests are
single-threaded"). Plan §3.5 #4 said ConsumerState holds `S: EtwSysCalls`.
These together require `S: Sync` (for `Arc<ConsumerState<S>>: Send`).
But `RefCell` is NOT `Sync`. The plan didn't anticipate the conflict.

**Resolution (Option B inline):** restructure so ConsumerState
doesn't hold S. ConsumerState becomes non-generic (just `events_seen:
AtomicU64`). The consumer thread closure captures `syscalls: S`
directly by move (Send + 'static suffices; no Sync needed). EtwSession
holds its own `syscalls: S` field for `into_supervisable_parts` to
move into SessionShutdownHandle. Preserves the plan's mock-injection
architecture while honoring the bound reality.

The static_assertions guard now asserts `ConsumerState: RefUnwindSafe`
(no generic param) — AtomicU64 satisfies trivially.

**Hypothesis:** Mode 5 (consumer panic) supervisor test passes on
real Windows because (a) MockEtwSysCalls works for the supervisor's
synthetic-panic injection path, (b) AssertUnwindSafe still soundly
wraps the consumer-thread closure (the closure captures only
RefUnwindSafe types — Arc<ConsumerState>, String, and S which we
treat as caller-asserted safe).

**Windows-side verification:** `cargo test -p framesage-etw supervisor -- --nocapture`
— the supervisor::tests::supervisor_emits_consumer_panic_event_and_calls_shutdown
test runs Mode 5 end-to-end. If the test passes, the design fix is
sound. If it fails, surface for review.

**Resolved (Windows batch, 2026-05-17, agenda Step 9 + Step 14):**
both supervisor::tests::supervisor_emits_consumer_panic_event_and_calls_shutdown
AND session::tests::mode_5_session_level_full_flow_panic pass on real
Windows. catch_unwind + AssertUnwindSafe + oneshot path works correctly
under the actual Windows thread runtime. ConsumerState design (non-
generic, S held by EtwSession directly, consumer-thread closure
captures S by move) is sound. The static_assertions::assert_impl_all!(
ConsumerState: RefUnwindSafe) compile-time guard caught a real bug
during Mac-side Day 3 (parking_lot::Mutex was added to ConsumerState
prematurely, isn't RefUnwindSafe, guard fired immediately); guard
removal of the offending field resulted in the final clean design. No
further action.

---

## Entry 4 — Day 3 (2026-05-17): MockEtwSysCalls Clone semantics

**What was done:** Day 3's start_with_syscalls bound includes `S: Clone`
(to clone for the consumer thread). MockEtwSysCalls has
`#[derive(Clone)]` which gives per-clone queue state (each clone has
its own `RefCell<VecDeque<...>>` copy).

**Hypothesis:** for Day 3 + Day 4 tests, per-clone semantics are
fine:
- Mode 1/2/6 short-circuit before any clone happens (the queues are
  consumed by build-gate + start-trace before the consumer thread
  spawns).
- Mode 5 (supervisor test) clones once for the consumer thread; tests
  only script the build-gate `expect_rtl_get_version`, which is
  consumed BEFORE the clone fires. The consumer thread's clone has
  empty queues, returning ERROR_SUCCESS defaults — which is what we
  want for Mode 5 (consumer starts, then panics via synthetic
  injection, not via mock-scripted failure).

**Windows-side verification:** the supervisor Mode 5 test passes
on Windows host. If a future test needs shared-state mock clones
(e.g., scripting a process_trace return value for the consumer
thread), switch MockEtwSysCalls's RefCell → `Arc<Mutex<VecDeque<...>>>`.
Not blocking; document the gotcha for future test authors.

**Resolved (Windows batch, 2026-05-17, agenda Step 15):** Mode 3 +
Mode 5 tests all pass on real Windows. Per-clone-state semantics work
for the current test surface. Documented the gotcha here for future
test authors (Day 4's `arm_panic_in_process_trace` extension already
exercises the pattern). No further action.

---

## Entry 5 — Day 3 (2026-05-17): ERROR_ACCESS_DENIED not exported in windows-rs 0.58

**What was done:** windows-rs 0.58 doesn't appear to export
`ERROR_ACCESS_DENIED` as a constant in
`windows::Win32::Foundation::*`. Spike used named constants like
`ERROR_ALREADY_EXISTS` + `ERROR_WMI_INSTANCE_NOT_FOUND` without
issue; `ERROR_ACCESS_DENIED` appears to be missing. Day 3 added a
small private helper `fn ERROR_ACCESS_DENIED() -> WIN32_ERROR { WIN32_ERROR(5) }`
that constructs the constant manually from the documented Win32 error
code (5).

**Hypothesis:** the value `5` is correct (verified via MSDN). The
helper works the same as a named constant would. If windows-rs has
this constant under a different module path I missed, refactor to use
it during the end-of-week batch.

**Windows-side verification:** the Mode 1 (AccessDenied) mock test
asserts `EtwSubsystem::Disabled(DegradationMode::AccessDenied)` when
`expect_start_trace(WIN32_ERROR(5))` scripts a Mode 1 failure. If the
match works, the value is correct. Cleanup-as-needed: `grep -rn ERROR_ACCESS_DENIED ~/.cargo/registry/src/index.crates.io-*/windows-0.58.0/`
to find the right import path.

**Resolved (Windows batch, 2026-05-17, agenda Step 16): hypothesis was
WRONG. windows-rs 0.58 DOES export ERROR_ACCESS_DENIED at the canonical
path.** Grep found:

```text
C:\Users\Frank Andreas Lia\.cargo\registry\src\index.crates.io-1949cf8c6b5b557f\windows-0.58.0\src\Windows\Win32\Foundation\mod.rs:1087:
  pub const ERROR_ACCESS_DENIED: WIN32_ERROR = WIN32_ERROR(5u32);
```

Same module as the other constants we already import. Refactored inline
(commit `98128a5`): added `ERROR_ACCESS_DENIED` to the existing
`use windows::Win32::Foundation::{...}` import block; replaced
`if rc == ERROR_ACCESS_DENIED()` (private helper call) with
`if rc == ERROR_ACCESS_DENIED` (canonical constant); removed the
`fn ERROR_ACCESS_DENIED() -> WIN32_ERROR { ... }` helper at the bottom
of `mod windows_impl`. Mode 1 test still passes after refactor (`cargo
test -p framesage-etw -- --include-ignored` 21/21). No further action.

---

## Entry 6 — Day 3 (2026-05-17): Mode 5 supervisor test exercises panic path in isolation, not full consumer flow

**What was done:** The supervisor's inline Mode 5 test
(`supervisor_emits_consumer_panic_event_and_calls_shutdown`) drops
the real `exit_rx` from `into_supervisable_parts()` and constructs a
synthetic oneshot channel with a pre-baked `Panicked` message. This
tests the supervisor's panic-handling logic without requiring the
consumer thread to actually panic mid-ProcessTrace.

**Hypothesis:** the test demonstrates the supervisor reacts correctly
to a Panicked exit reason; the consumer-thread → supervisor full-flow
panic propagation IS covered in supervisor.rs's compile-time
ConsumerState: RefUnwindSafe guard + the AssertUnwindSafe wrapper in
session.rs's consumer-thread spawn closure. The Mode 5 architectural
contract is verified piece-wise rather than end-to-end on Mac.

**Windows-side verification:** during the end-of-week batch, add a
separate `#[ignore]`'d test that uses `MockEtwSysCalls` scripting
`process_trace` to return a sentinel that triggers `panic!()` inside
the consumer body. Verify the oneshot fires with `Panicked` AND the
supervisor emits the DegradationEvent. This is the "full-flow"
Mode 5 test that Mac-side compilation can't actually run (the
panic happens inside `std::thread::spawn`'s closure, which on Mac
would still work but doesn't actually involve ETW infrastructure).

If the end-of-week full-flow test reveals the supervisor doesn't
react correctly to a real panic, that's an Entry-N in
audit/buddy-disagreements.md.

**Resolved (Day 4):** the Mode 5 full-flow session-level test
landed via `MockEtwSysCalls::arm_panic_in_process_trace` — see
`crates/etw/src/session.rs::tests::mode_5_session_level_full_flow_panic`.
Test exercises the full path: start_with_syscalls → consumer thread
spawn → mock's process_trace panics → catch_unwind fires → real
oneshot sends Panicked → SupervisorLoop receives → on_event called
with ConsumerPanic. Bounded by tokio::time::timeout(5s). The
Windows-batch run still verifies it works against a real Windows
host runtime as well.

**Resolved (Windows batch, 2026-05-17, agenda Step 17):** both tests
(supervisor-isolation + session-level full-flow) pass on real Windows
host runtime. The mock-driven full-flow test exercises the same path
through the real Windows thread runtime that production would use.
Architectural Mode 5 wire validated end-to-end. No further action.

---

## Entry 7 — Day 5 (2026-05-17): MonitorHandle introduced for drop-poll sibling task

**What was done:** plan §4 Day 5 pseudo-code only spawned the
supervisor task and used `into_supervisable_parts` which consumes
the EtwSession. The prose says "the drop-rate query loop runs
concurrently in a sibling task that calls EtwSession::query_stats()
on a 1-second tokio interval" — but the sibling task has no way to
call query_stats once the session is decomposed.

**Resolution (inline — no STOP):** added `MonitorHandle<S>` type +
`EtwSession::into_supervisable_parts_with_monitor` returning the
4-tuple `(JoinHandle, oneshot::Receiver, SessionShutdownHandle,
MonitorHandle)`. The MonitorHandle owns a clone of syscalls +
session_name + the Arc<ConsumerState>; provides
`poll_drop_stats(on_event)` for periodic stat queries from a sibling
tokio task. Read-only — does NOT call ControlTraceW(STOP). The
supervisor remains the only stop path.

The `into_supervisable_parts` 3-tuple variant stays for the
supervisor.rs synthetic-panic test (which doesn't need monitoring).
Service-side wiring uses `..._with_monitor`.

**Hypothesis:** the drop-poll task self-terminates cleanly when the
session ends (its query_session_stats call fails because the session
is gone; the task logs at WARN and breaks). The supervisor task and
the drop-poll task run independently in the tokio runtime; neither
participates in the v0.6 watchdog select! per architecture §2.1
mode 5 amendment (PR #77).

**Windows-side verification:** during the end-of-week batch, start
the service with `closed_loop_enabled: true` on a Win11 24H2+
elevated host. Verify:
1. Both supervisor + drop-poll tasks appear in tracing logs (look
   for "closed-loop ETW session started" + "ETW consumer-supervisor
   task completed" or "ETW drop-poll task terminating").
2. Force-stop the session via `logman stop FramesageEtw -ets`;
   verify both tasks self-terminate (drop-poll logs "session likely
   closed"; supervisor logs the consumer's exit reason).
3. Service itself stays up — watchdog doesn't fire because closed-
   loop tasks are intentionally excluded from the select!.

**Resolved (Windows batch, 2026-05-17, agenda Steps 18 + 23 + 24 +
28):** the MonitorHandle pattern works end-to-end on real Windows.
Verified at:
- Step 23 (closed-loop enable + restart): supervisor + drop-poll
  tasks spawned per the `closed-loop ETW session started + supervisor/
  drop-poll tasks spawned reason="running"` INFO log line.
- Step 24 (clean Stop-Service): closed-loop tasks cancelled cleanly
  by tokio runtime shutdown; SessionShutdownHandle::Drop tears down
  the session (Reading 1 ratified by user — D1 Drop is load-bearing
  here by design, not defensive).
- Step 28 (survives-restart): four-transition cycle (Start → force-
  kill → Start → Stop-Service) completes correctly. Service stays up
  through clean shutdown; closed-loop tasks don't bring down the
  service host. Watchdog correctly excludes them per architecture
  §2.1 mode 5 amendment.

No further action.

---

## Entry 8 — Day 5 (2026-05-17): runtime.rs _silence_warnings host-rot fix-forward

**What was done:** while wiring closed_loop into runtime.rs, found
that `cargo test -p framesage-service` failed on non-Windows hosts
because the `_silence_warnings` function (a non-Windows-only helper
that keeps imports referenced) had bit-rotted:
- `load_policy` was renamed to `load_policy_or_default` in an
  earlier refactor but the silence list wasn't updated.
- `type_name::<AsyncBufReadExt>` (bare trait name) became a hard
  error in newer Rust editions; `dyn` doesn't help here because
  AsyncBufReadExt's methods return `Self`-bound future types and
  it's not dyn-compatible.

**Resolution (inline maintenance):** updated to the new function
name, and switched to a `fn<T: Trait>(&T)` generic-bound pattern
that exercises the trait without requiring dyn-compatibility.

**Hypothesis:** pure host-side maintenance with no effect on
Windows-target behavior. The new `_silence_warnings` compiles clean
on host AND has no Windows-side counterpart (it's
`#[cfg(not(windows))]`).

**Windows-side verification:** none required — the function never
runs on Windows.

**Resolved (Windows batch, 2026-05-17, agenda Step 19):** no Windows-
side action needed; verified by Mac-side `cargo test -p framesage-
service` (8 tests) passing during Day 5. Windows-side `cargo test -p
framesage-service` (11 tests including 3 cfg(windows)-gated ACL tests)
also passing per agenda Step 10. No further action.

---

## Entry 9 — Day 5 (2026-05-17): Policy::closed_loop_enabled added

**What was done:** added `closed_loop_enabled: bool` field to
`framesage_core::Policy`. Defaults to false per `etw-edr-report.md`
§6 (v0.7 ships closed-loop default-off; v0.7.1 flip gated on §6.1).
Three `Policy { ... }` literal sites updated to include the field
(crates/core, crates/ipc, crates/service).

**Hypothesis:** policy.json files written by v0.6 will load
correctly with the new field defaulting to false (serde's
`#[serde(default)]` attribute is on the field). New installs get
the default-off behavior; users opt in by editing policy.json.

**Windows-side verification:** during the end-of-week batch, test
upgrade scenario:
1. Start with a v0.6 policy.json (no closed_loop_enabled field).
2. Load via load_policy_or_default; verify no error.
3. Verify Policy.closed_loop_enabled = false (default).
4. Verify start_closed_loop_if_enabled returns OptedOut + logs the
   structured reason="policy_opt_out" event.
5. Manually flip the field to true; restart service; verify the
   build-gate path is taken next.

**Resolved (Windows batch, 2026-05-17, agenda Steps 20 + 21 + 22 + 23):
upgrade scenario PASSED end-to-end.** Used the host's pre-existing v0.6
`C:\ProgramData\framesage\policy.json` (preserved during Step 20.5
uninstall) as the actual upgrade-test input — no synthetic setup needed.

Per-sub-step:
1. **Pre-existing v0.6 policy.json present** (4424 bytes, no
   `closed_loop_enabled` field). Verified at Step 21 V4: `findstr
   closed_loop policy.json` → exit 1, no match.
2. **load_policy_or_default loaded without error.** Service running per
   Step 21 V1; no startup errors in service log.
3. **Verified closed_loop_enabled = false (default).** Indirect
   verification via Step 21 V6: `logman query FramesageEtw -ets` returned
   "not found" — service did NOT create an ETW session, consistent with
   `closed_loop_enabled = false` taking the OptedOut branch.
4. **Verified structured reason="policy_opt_out" event.** Step 22 service
   log capture:
   ```text
   2026-05-17T19:55:03.857489Z  INFO framesage_svc::closed_loop:
     closed-loop disabled by policy.closed_loop_enabled = false;
     engine runs in v0.6 static-rule mode
     reason="policy_opt_out"
   ```
5. **Manually flipped to true + restarted; build-gate path taken.** Step
   23: edited policy.json, Restart-Service, verified ETW session running
   (`FramesageEtw` Running in kernel with 4 providers), service log
   showed `reason="running"`.

**Side observation captured during sub-step 1 → 2 transition (~3 min
after install):** the service rewrote policy.json with
`"closed_loop_enabled": false` added explicitly (+35 bytes). Cause:
serde-round-trip on first IPC-triggered policy save (almost certainly
tray interaction during the diagnostic period). Net effect: policy.json
self-documents after first save with all current-version fields visible.
**Benign + arguably desirable** — but worth documenting in v0.7 README
so the timing of "field appears" isn't surprising. v4.3 amendment
Section 4 captures the operating-model finding; Section 6 captures the
README TODO.

No further action.
