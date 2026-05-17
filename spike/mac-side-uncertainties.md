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
