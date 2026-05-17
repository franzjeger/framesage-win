# v0.7 Group A — Week 2 EOD report

**Status:** Windows runtime batch COMPLETE 2026-05-17 (UTC).
All §12.8 agenda steps (1–28) executed; architectural promises
validated empirically. Recommendation: **GO**. Detail in §12.7.

**Branch:** `feat/group-a-week-2` (HEAD: `98128a5` at Windows batch start,
plus this commit's report fill-in).
**Plan reference:** `spike/group-a-week-2-plan.md` v4.2 APPROVED; v4.3
amendment in flight on `plan/group-a-week-2-v4.3-amendment`.
**Architecture amendment:** PR #77 (draft) — `proposal/v0.7-arch-mode5-amendment`.

---

## 12.1 Environment attestation

**Mac-side (development host — where Days 1–5 were written):**

```text
$ uname -a
Darwin <hostname> 25.5.0 Darwin Kernel Version 25.5.0 [...] arm64

$ rustc --version
[Captured at batch — pinned by rust-toolchain.toml]

$ cargo --version
[Captured at batch]

$ rustup target list --installed
aarch64-apple-darwin
x86_64-pc-windows-gnu
```

**Windows runtime host (env-1; literal capture 2026-05-17):**

```text
PS> [System.Environment]::OSVersion.Version
  Major  Minor  Build  Revision
  -----  -----  -----  --------
  10     0      26200  0

PS> (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -Name UBR).UBR
8457

PS> Get-Service framesage   # at batch start
Status   Name      DisplayName
------   ----      -----------
Running  framesage framesage scheduler supervisor
   (v0.6 dev install from earlier today, 07:27 binary timestamp;
    BINARY_PATH_NAME=C:\Program Files\FrameSage\framesage-svc.exe,
    SERVICE_START_NAME=LocalSystem, START_TYPE=AUTO_START)
   (uninstalled during agenda Step 20.5; reinstalled via install.ps1 at Step 21)

PS> sc.exe query WinDefend
[SC] EnumQueryServicesStatus:OpenService FAILED 1060:
The specified service does not exist as an installed service.
   (NOTE: Defender platform not installed on this host — earlier env-1
    state per spike/etw-edr-report.md §2 still holds. Material caveat
    for EDR matrix work in v0.7.1 but NOT a Group A week 2 concern.)

PS> Get-Process MsMpEng -ErrorAction SilentlyContinue
   (no MsMpEng process; consistent with WinDefend absence)
```

**Identity:** `FRANZJEGER\Frank Andreas Lia` (local user account).

**Elevation context:** Agent PowerShell tool runs UNELEVATED (subprocess of
Claude Code CLI at `%APPDATA%\Claude\claude-code\2.1.138\claude.exe`,
itself a per-user app). UAC config: `EnableLUA=1`,
`ConsentPromptBehaviorAdmin=0` (admin actions silently auto-elevated for the
user's token without consent prompt). Admin operations during the batch
issued via `Start-Process -Verb RunAs cmd.exe /c <bat>` which spawn a
silently-elevated subprocess. Verified by Step 0 elevation diagnostic:
`Start-Process -Verb RunAs powershell.exe ... -File <test.ps1>` produced
elevated process with `IsInRole(Administrator) = True` in 0.227s with no
UAC dialog (case (c) per the batch's elevation diagnostic).

Operating model finding for future Windows-batch sessions: ETW state
queries and any privilege-filtered diagnostic must be run elevated.
Unelevated `logman query -ets` filters by access; the agent saw an
empty list earlier in the batch while a leaked session was in fact
present (visible only to the elevated query). v4.3 amendment Section
4 documents the standing rule.

---

## 12.2 Per-day deliverable status

| Day | Planned deliverable (§4) | Actual outcome | Stop gate triggered? | Notes |
|---|---|---|---|---|
| 1 | crates/etw/ skeleton + build_gate.rs + 3 inline unit tests + RtlGetVersion binding compile | matched; stop-gate engaged-and-resolved-in-flight | Day 1 binding-path stop gate (resolved in-flight, not surfaced) | `RtlGetVersion` lives in `Wdk::System::SystemServices`, NOT plan §3.1's stated `Win32::System::SystemInformation` path. Documented `mac-side-uncertainties.md` Entry 1. |
| 2 | session.rs lifecycle lift from spike-etw + Cargo workspace check green | matched | none | Lift is a clean copy-then-rename. New production SESSION_GUID distinct from spike's so they coexist during transition. |
| 3 | degradation.rs + EtwSysCalls trait + EtwSubsystem return + generic EtwSession<S> + SupervisorLoop scaffold + arch amendment PR | matched; one substantive design finding resolved inline | none (design-level finding resolved inline per Day 3 STOP-gate guidance category (2)) | Five uncertainties-doc entries logged (Entries 2–6). Compile-time static_assertions guard caught a real bug on first deployment (parking_lot::Mutex isn't RefUnwindSafe). Deeper conflict surfaced: RefCell-mock + ConsumerState-holds-S + std::thread::spawn's Sync bound. Resolved via Option B: ConsumerState non-generic; consumer thread captures S by move. |
| 4 | Six degradation-mode tests against Day 3 scaffold + 1 service-crate integration test | matched (3 modes via session inline tests + Mode 5 split into supervisor + session levels) | none | Mode 3 wire `EtwSession::poll_drop_stats` added as production code — surfaced explicitly in Day 4 commit message per user guidance. No new uncertainties. Lower-stakes day as expected. |
| 5 | Service-crate wiring + SupervisorLoop instance + EOD verification | matched (Mac-side scope) | none | MonitorHandle introduced per uncertainties Entry 7 (plan §4 Day 5 pseudo-code was incomplete for the drop-poll sibling task). Closed-loop tasks intentionally excluded from v0.6 watchdog per arch §2.1 mode 5 amendment. _silence_warnings host-rot fixed inline (Entry 8). Policy::closed_loop_enabled added (Entry 9). |

---

## 12.3 Test results inventory

**Mac-side (latest run):**

```text
$ cargo test -p framesage-etw
running 14 tests
test build_gate::tests::predicate_false_at_synthetic_build_below_threshold ... ok
test build_gate::tests::predicate_false_on_synthetic_rtlgetversion_failure ... ok
test build_gate::tests::predicate_true_at_synthetic_build_at_or_above_threshold ... ok
test degradation::tests::bare_constructor_produces_empty_detail ... ok
test degradation::tests::build_unsupported_carries_build_number ... ok
test degradation::tests::degradation_mode_variants_are_distinct ... ok
test session::tests::mode_3_poll_drop_stats_emits_kernel_drops_when_buffers_lost ... ok
test session::tests::mode_3_poll_drop_stats_silent_when_zero_drops ... ok
test session::tests::mode_4_our_drops_variant_exists_and_is_distinct ... ok
test session::tests::non_windows_start_bails_with_platform_message ... ok
test session::tests::session_options_default_matches_spike_tested_set ... ok
test session::tests::session_stats_default_is_zeroed ... ok
test supervisor::tests::consumer_exit_reason_clean_shutdown_distinguishable_from_panicked ... ok
test supervisor::tests::consumer_exit_reason_debug_includes_message ... ok

test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured

$ cargo test -p framesage-service
running 8 tests
test runtime::tests::validate_accepts_aggressive_but_safe_defaults ... ok
test runtime::tests::validate_accepts_profile_without_game_mode ... ok
test runtime::tests::validate_accepts_shipped_default_policy ... ok
test runtime::tests::validate_refuses_denylisted_processes ... ok
test runtime::tests::validate_refuses_denylisted_services ... ok
test runtime::tests::validate_reports_all_denials_at_once ... ok
test closed_loop::tests::build_gate_fallthrough_emits_structured_build_unsupported_event ... ok
test closed_loop::tests::opt_out_path_emits_structured_policy_opt_out_event ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured
```

**Lint + format checks (Mac-side):**

```text
$ cargo clippy --workspace --target x86_64-pc-windows-gnu --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ cargo clippy -p framesage-etw --all-targets -- -D warnings
    Finished `dev` profile

$ cargo fmt --all -- --check
(empty output — clean)
```

**Windows runtime (full suite — captured 2026-05-17 elevated):**

The agenda's Step 9 (`cargo test -p framesage-etw -- --nocapture --include-ignored`)
went through **four iterations** before passing. Each iteration surfaced a
new real-Windows finding that didn't show on Mac-side cross-target. All
four findings now permanently fixed on `feat/group-a-week-2`:

| Round | Finding | Symptom | Fix | Commit |
|---|---|---|---|---|
| 1 (original) | Shared `FramesageEtw` name + parallel tests race | 1/20 fail: `starts_and_stops_cleanly` got `Disabled(AlreadyExists)` | F1 — PID-suffixed unique session names per test | `9998ec9` |
| 2 (post-F1) | Different test failed same way despite unique names | 1/20 fail: `drop_path_fires_event` got `Disabled(AlreadyExists)` | G1 — `#[serial_test::serial]` on real-ETW tests | `a5b955f` |
| 3 (post-G1) | First-test-of-binary failed — pre-existing kernel leak from earlier round | 2/2 fail in isolation B test | Pre-batch cleanup of leaked `FramesageEtwTest_drop_path_<old PID>` session via elevated `logman stop`; then D1 — `impl Drop for EtwSession` to prevent future leaks | `23e6457` + `35b7cb0` (Cargo.lock catchup) |
| 4 (post-D1 verification) | Reverified parallelism diagnosis as REAL, not leak artifact | All 20 pass cleanly with full config | (state already converged) | — |

Final Step 9 run with all fixes:

```text
PS> cargo test -p framesage-etw -- --nocapture --include-ignored
running 20 tests
test build_gate::tests::predicate_false_at_synthetic_build_below_threshold ... ok
test build_gate::tests::predicate_false_on_synthetic_rtlgetversion_failure ... ok
test build_gate::tests::predicate_true_at_synthetic_build_at_or_above_threshold ... ok
test degradation::tests::bare_constructor_produces_empty_detail ... ok
test degradation::tests::build_unsupported_carries_build_number ... ok
test degradation::tests::degradation_mode_variants_are_distinct ... ok
test session::tests::mode_1_access_denied_returns_disabled ... ok
test session::tests::mode_2_already_exists_returns_disabled_after_cleanup_retry ... ok
test session::tests::mode_3_poll_drop_stats_emits_kernel_drops_when_buffers_lost ... ok
test session::tests::mode_3_poll_drop_stats_silent_when_zero_drops ... ok
test session::tests::mode_4_our_drops_variant_exists_and_is_distinct ... ok
test session::tests::mode_5_session_level_full_flow_panic ... ok
test session::tests::mode_6_build_unsupported_short_circuits_before_any_etw_call ... ok
test session::tests::real_etw_session_drop_path_fires_event ... ok
test session::tests::real_etw_session_starts_and_stops_cleanly ... ok
test session::tests::session_options_default_matches_spike_tested_set ... ok
test session::tests::session_stats_default_is_zeroed ... ok
test supervisor::tests::consumer_exit_reason_clean_shutdown_distinguishable_from_panicked ... ok
test supervisor::tests::consumer_exit_reason_debug_includes_message ... ok
test supervisor::tests::supervisor_emits_consumer_panic_event_and_calls_shutdown ... ok

test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.89s
```

Step 10 — `cargo test -p framesage-service` (unelevated):

```text
running 11 tests
test acl::tests::well_known_sids_match_themselves ... ok
test runtime::tests::validate_accepts_profile_without_game_mode ... ok
test runtime::tests::validate_reports_all_denials_at_once ... ok
test runtime::tests::validate_accepts_aggressive_but_safe_defaults ... ok
test closed_loop::tests::opt_out_path_emits_structured_policy_opt_out_event ... ok
test runtime::tests::validate_refuses_denylisted_processes ... ok
test runtime::tests::validate_accepts_shipped_default_policy ... ok
test runtime::tests::validate_refuses_denylisted_services ... ok
test acl::tests::hardened_sddl_parses ... ok
test acl::tests::verify_refuses_dir_owned_by_test_user ... ok
test closed_loop::tests::build_gate_fallthrough_emits_structured_build_unsupported_event ... ok

test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
```

(Mac-side reported 8 tests; Windows has +3 ACL tests gated by `#[cfg(windows)]`.)

Step 11 — `cargo test --workspace -- --include-ignored` (full workspace,
elevated): also went through iterations:

| Round | Finding | Fix | Commit |
|---|---|---|---|
| 1 | `framesage-core::layering::workspace_layering_invariants_hold` failed — new `framesage-etw` crate not registered in `crates/core/src/layering.rs` allowlist + `ARCHITECTURE.md` lacked the row | Update layering registry + add invariant 8 to ARCHITECTURE.md | part of `39644f6` |
| 2 (post-layering) | `framesage_svc::closed_loop::build_gate_fallthrough_emits_structured_build_unsupported_event` failed: elevated context took the Running branch into `tokio::spawn` outside a runtime; plain `#[test]` couldn't handle | A + D1: build-gate override seam in `closed_loop.rs` + `#[tokio::test]` rewrite + `impl Drop for SessionShutdownHandle` | `39644f6` |
| 3 (post-A+D1) | All framesage-etw real-ETW tests failed — pre-existing kernel leak of canonical `FramesageEtw` session from Round 2's mid-test crash | Pre-batch cleanup of leaked `FramesageEtw` via elevated `logman stop`; D1 prevents future leaks | (cleanup admin op) |
| Final | 250/250 workspace tests pass with 0 failures | (state converged) | — |

Final Step 11 result by crate:

| Binary | Passed | Failed | Ignored |
|---|---|---|---|
| `framesage` main | 0 | 0 | 0 |
| `framesage-core` | 54 | 0 | 0 |
| `framesage-engine` | 53 | 0 | 0 |
| `framesage-etw` | 20 | 0 | 0 |
| `framesage-gamemode` | 37 | 0 | 0 |
| `framesage-ipc` | 3 | 0 | 0 |
| `framesage-service` | 11 | 0 | 0 |
| `framesage-sim` | 4 | 0 | 0 |
| `framesage-sys` | 31 | 0 | 0 |
| `framesage-tray` | 37 | 0 | 0 |
| spike-etw + 6 doc-tests | 0 | 0 | 0 |
| **Total** | **250** | **0** | **0** |

Step 12 — Entry 1 RtlGetVersion real probe (permanent regression test):

```text
PS> cargo test -p framesage-etw real_rtl_get_version_probe_succeeds_on_supported_host -- --nocapture --include-ignored
running 1 test
test build_gate::tests::real_rtl_get_version_probe_succeeds_on_supported_host ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 20 filtered out; finished in 0.00s

# eprintln captured:
real RtlGetVersion: detected_build() = Some(26200) (expect Some(>= 26100)); MIN_BUILD_FOR_CLOSED_LOOP = 26100

# Host cross-check:
[System.Environment]::OSVersion.Version.Build = 26200    (match)
```

(Test added in `98128a5` as a permanent #[ignore]'d regression guard.)

Lint + format on real Windows (final):

```text
PS> cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.51s
    (no warnings, exit 0)

PS> cargo fmt --check
    (no output, exit 0)
```

Release build verification (Step 8 — note plan-vs-reality correction):

```text
PS> cargo build -p framesage-service --release
   Compiling framesage-core v0.5.0
   Compiling windows v0.58.0
   Compiling framesage-gamemode v0.5.0
   Compiling framesage-ipc v0.5.0
   Compiling framesage-sys v0.5.0
   Compiling framesage-etw v0.5.0
   Compiling framesage-engine v0.5.0
   Compiling framesage-service v0.5.0
    Finished `release` profile [optimized] target(s) in 27.37s

target/release/framesage-svc.exe   2,278,400 bytes
```

**Note (plan-vs-reality correction):** Agenda Step 8 said
`cargo build -p framesage-svc --release` — that's the BINARY name (`[[bin]]`
declaration in `crates/service/Cargo.toml`), not the PACKAGE name
(`[package].name = "framesage-service"`). cargo rejected with
`error: package ID specification 'framesage-svc' did not match any packages`.
Corrected to `-p framesage-service`. Documentation slip in the agenda,
no behavioral consequence. Logged in v4.3 amendment Section 1.

---

## 12.4 EOD verification checklist (Day 5) — COMPLETE

All seven sub-steps executed during Windows runtime batch §12.8 steps 21–28
(2026-05-17). Literal outputs below; full agenda traces in §12.8 and
git commit messages.

### Step 21 — Install v0.7 via install.ps1

```text
PS> Start-Process -Verb RunAs -FilePath powershell.exe `
    -ArgumentList "-NoProfile","-ExecutionPolicy","Bypass","-File", `
        "F:\Projects\framesage-win\peaceful-mayer-4d5448\install.ps1" -Wait

install.ps1 exit: 0; elapsed: 44s

[install] target:  C:\Program Files\FrameSage
[install] building release (cargo build --release --workspace)...
    Finished `release` profile [optimized] target(s) in 42.63s
[install] staging binaries to C:\Program Files\FrameSage...
  copied framesage-tray.exe
  copied framesage-svc.exe
  copied framesage.exe
  copied framesage-sim.exe
[install] hardening install dir ACL...
  SYSTEM:F  Administrators:F  Users:RX
[install] (re-)registering the framesage service...
  installing service (LocalSystem, autostart on boot)
installed: framesage
  failure-actions: restart x3 with 5s delay, reset after 24h
[install] keeping existing policy.json (your rules + profile edits)
  C:\ProgramData\framesage\policy.json
[install] starting service...
started
[install] launching tray...
```

Six post-install verifications all PASS (full detail in §12.8 step 21
and prior chat transcript). Key facts:
- Service Running, LocalSystem, AUTO_START, PID 18516
- `BINARY_PATH_NAME = "C:\Program Files\FrameSage\framesage-svc.exe"`
- `C:\ProgramData\framesage\policy.json` preserved as v0.6 content (no
  `closed_loop_enabled` field on disk at this point)
- `Get-Process framesage*` shows tray PID 18452 + svc PID 18516
- `logman query FramesageEtw -ets` returns "Data Collector Set was not
  found" — confirms Entry 9 upgrade scenario PASS (serde defaults the
  missing field to false, no ETW session created).

### Step 22 — `Get-Service framesage` with `closed_loop_enabled: false` (static-rule path)

Right after install (before any policy edit), service log captured:

```text
2026-05-17T19:55:03.857489Z  INFO framesage_svc::closed_loop:
  closed-loop disabled by policy.closed_loop_enabled = false;
  engine runs in v0.6 static-rule mode
  reason="policy_opt_out"
```

Structured `reason="policy_opt_out"` field present per Day 5 spec.
**Static-rule path verified.** Service writes to daily-rotated log at
`C:\ProgramData\framesage\logs\framesage-svc.log.2026-05-17` (per v0.6
item 2.7 / audit H-29 work).

Side observation captured at this step: ~3 min after install, the
service rewrote `policy.json` (+35 bytes, added `"closed_loop_enabled":
false` explicitly). Cause: serde round-trip — any IPC-triggered policy
save (almost certainly tray interaction during diagnostic period)
re-serializes the in-memory Policy struct, which materializes the new
field at its default value. **Behavior is desirable** (policy.json
self-documents after first save) but should be documented in v0.7 README
so the timing isn't surprising. v4.3 amendment Section 4 captures this
operating-model finding.

### Step 23 — Edit policy.json to `closed_loop_enabled: true`, restart, verify session running

Quoted destructive admin op (executed via `Start-Process -Verb RunAs`):

```powershell
$path = 'C:\ProgramData\framesage\policy.json'
$j = Get-Content $path -Raw | ConvertFrom-Json
$j.closed_loop_enabled = $true
$j | ConvertTo-Json -Depth 50 | Set-Content $path -Encoding UTF8
Restart-Service framesage
```

```text
policy.json updated
Service status after Restart-Service: Running

PS> sc.exe queryex framesage
SERVICE_NAME: framesage
        STATE              : 4  RUNNING
        PID                : 14344

PS> logman query FramesageEtw -ets
Name:                 FramesageEtw
Status:               Running
Buffer Size:          64
Buffers Lost:         0
Buffers Written:      0   (immediately post-start; counter increments quickly)
File Mode:            Real-time
Provider:
Name:                 {D4BBEE17-B545-4888-858B-744169015B25}     KeywordsAny: 0x5
Name:                 {3D5C43E3-0F1C-4202-B817-174C0070DC79}     KeywordsAny: 0x1
Name:                 {82958CA9-B6CD-47F8-A3A8-03AE85A4BC24}     KeywordsAny: 0x2
Name:                 {599A2A76-4D91-4910-9AC7-7D33F2E97A6C}     KeywordsAny: 0x200
The command completed successfully.

PS> logman query -ets | findstr /i Frame
FramesageEtw   Trace   Running        (canonical name only — no PID-suffixed test names leaked into production)
```

Service log:

```text
2026-05-17T20:10:45.028073Z  INFO framesage_etw::session::windows_impl: ETW session started session=FramesageEtw
2026-05-17T20:10:45.028090Z  INFO framesage_svc::closed_loop: closed-loop ETW session started + supervisor/drop-poll tasks spawned reason="running"
2026-05-17T20:10:45.028093Z  INFO framesage_svc::runtime: closed-loop startup decision made startup_result=Running
```

**This is the first end-to-end execution of the v0.7 closed-loop
production wire on real Windows under LocalSystem.** Build gate +
EtwSysCalls trait dispatch + EVENT_TRACE_SYSTEM_LOGGER_MODE session
creation + ProcessTrace + tokio supervisor + tokio drop-poll all
integrate. The four providers listed are the system-trace internal
provider GUIDs (distinct from per-event class GUIDs the consumer matches
against); expected behavior. Canonical session naming preserved — F1
test-isolation fix did NOT leak into production.

### Step 24 — Stop-Service, verify session gone

```text
PS> Stop-Service framesage
Service status after Stop-Service: Stopped

PS> sc.exe queryex framesage
STATE              : 1  STOPPED
PID                : 0

PS> logman query FramesageEtw -ets
Error: Data Collector Set was not found.
   (exit -2144337918 — expected)

PS> logman query -ets | findstr /i Frame
   (no Frame* sessions)
```

Service log at teardown:

```text
2026-05-17T20:12:50.474507Z  INFO framesage_svc::runtime: shutdown requested
2026-05-17T20:12:50.474514Z  INFO framesage_svc::runtime: system-events channel closed
2026-05-17T20:12:50.507606Z  INFO framesage_etw::session::windows_impl:
                                SessionShutdownHandle::drop: session stopped (fallback path)
                                session=FramesageEtw
```

**Architectural finding (Reading 1 ratified by user):** Teardown
executes via `SessionShutdownHandle::Drop` (D1 from Step 11), NOT via
the supervisor's explicit `SessionShutdownHandle::shutdown()` call. The
supervisor task is cancelled by tokio runtime shutdown (per
closed-loop-tasks-not-in-watchdog choice from PR #77 Mode 5 amendment)
before it can run its clean-exit path. **D1 Drop is LOAD-BEARING in
production, not defensive belt-and-suspenders.** This is intentional
architectural design — the panic-isolation pattern from Mode 5
amendment requires closed-loop tasks outside the watchdog, and
cancellation-on-shutdown is the corollary. Drop owns teardown of
closed-loop resources. v4.3 amendment Section 2 documents in full.

### Step 25 — cross-reference test results from §12.3

All test results from §12.3 reflected here; no additional run needed at
this step. The 20-tests-in-`framesage-etw` final result + the 250-tests-
workspace-wide result both reference the same fixed-state code.

### Step 26 — INFO log on supported + synthetic unsupported builds

Supported build (Win11 26200, real `RtlGetVersion` probe) — captured in
Step 22/23 logs above:

```text
INFO framesage_svc::closed_loop:
  closed-loop ETW session started + supervisor/drop-poll tasks spawned
  reason="running"
```

Synthetic unsupported build (Win11 23H2 = 22631 via
`BuildOverrideGuard::set(Some(Ok(22631)))` test seam in framesage-service):

```text
PS> cargo test -p framesage-service closed_loop::tests::build_gate_fallthrough_emits_structured_build_unsupported_event -- --nocapture
running 1 test
2026-05-17T20:19:54.544364Z  INFO build_gate_fallthrough_emits_structured_build_unsupported_event:
   framesage_svc::closed_loop:
   closed-loop disabled: Windows build below MIN_BUILD_FOR_CLOSED_LOOP;
   engine runs in v0.6 static-rule mode
   reason="build_unsupported" detected_build=Some(22631) minimum_build=26100
test closed_loop::tests::build_gate_fallthrough_emits_structured_build_unsupported_event ... ok
```

**Both branches of the build-gate decision tree exercise structured
logging per Day 5 spec.** Named `reason` field on both, plus
`detected_build` + `minimum_build` on the BuildUnsupported variant.

### Step 27 — codegen-parity asm capture

```text
PS> cargo rustc -p framesage-service --release --bin framesage-svc -- --emit=asm -C codegen-units=1
   Finished `release` profile [optimized] target(s) in 28.07s
   target/release/deps/framesage_svc.s   18,600,569 bytes
```

Direct IAT calls to all six windows-rs ETW APIs (verified by grep on
the monomorphized service binary):

| API | Direct `callq *__imp_XXX(%rip)` calls |
|---|---|
| `StartTraceW` | 1 |
| `ControlTraceW` | 4 (matches 4 call sites: cleanup, retry, stop, query) |
| `OpenTraceW` | 1 |
| `ProcessTrace` | 1 |
| `CloseTrace` | 1 |
| `RtlGetVersion` | 2 (probe cached/uncached paths) |

**Zero indirection:**
- No `callq *%register` instructions attributable to framesage-etw
  trait dispatch (the register-indirect calls present in the 18MB
  binary are tokio/futures vtable polls elsewhere — not our trait)
- **No `RealEtwSysCalls` or `EtwSysCalls` symbols in the
  monomorphized output** — both inlined away entirely
- Pattern verified: `EtwSession::<RealEtwSysCalls>::start(opts)` →
  monomorphized → `RealEtwSysCalls::start_trace` inlined → direct
  `callq *__imp_StartTraceW(%rip)`

**The `EtwSysCalls` trait abstraction has zero runtime cost in release
builds.** Plan §3.4 v4.2 amendment's codegen-parity acceptance
criterion fully met. The `_asm_baseline` Cargo feature (deferred per
Day 5 report) is **NOT needed** — the visual diff on the real
monomorphized binary is sufficient and arguably stronger evidence than
a synthetic baseline. v4.3 amendment Section 1 cites this as the
final disposition of the deferred `_asm_baseline` decision.

### Step 28 — survives-restart sequence (the load-bearing test)

Four-transition sequence: Start → Stop-Process -Force → Start → Stop-Service.

| Transition | Service state | PID | ETW session | Buffers |
|---|---|---|---|---|
| **(a)** Start | Running | 12200 | Running, 4 providers | 38 |
| **(b)** Force-kill | Stopped, exit 1067 (ERROR_PROCESS_ABORTED) | gone | **STILL Running**, 4 providers | 46 |
| **(c)** Start again | Running | 4860 (new) | Running, 4 providers | 74 |
| **(d)** Stop-Service | Stopped | — | not found | — |

Critical log lines at transition (c):

```text
2026-05-17T20:26:28.985922Z  INFO framesage_svc::runtime:
                                framesage engine starting cpus=32 rules=5
2026-05-17T20:26:29.015860Z  INFO framesage_etw::session::windows_impl:
                                cleaned up stale ETW session session=FramesageEtw    ← LOAD-BEARING
2026-05-17T20:26:29.016152Z  INFO framesage_etw::session::windows_impl:
                                ETW session started session=FramesageEtw             ← StartTraceW succeeded after cleanup
2026-05-17T20:26:29.016171Z  INFO framesage_svc::closed_loop:
                                closed-loop ETW session started + supervisor/drop-poll tasks spawned
                                reason="running"
```

**Architecture §2.1's "Survives service restarts" promise validated
empirically on real Windows for the first time in this engagement.**
The compose-of-Finding-1 (per-process slot constraint) and Finding-2
(kernel-owned session lifetime exceeds process lifetime) production
hazard — invisible to Mac-side cross-target verification — is correctly
handled by Day 2's session-lifecycle lift. v4.3 amendment Sections 2
+ 5 document the full chain.

---

## 12.5 Stop-gate trip log

| Day | Stop gate (§6) | Triggered? | Disposition |
|---|---|---|---|
| 1 | `RtlGetVersion` binding | YES (engaged-and-resolved-in-flight) | Plan §3.1's stated module path was wrong; actual binding lives in `Wdk::System::SystemServices`. Architecture's "don't fall back to GetVersionEx" gate honored — this is the same binding at a different location. Documented uncertainties Entry 1. |
| 2 | Spike-to-prod behavioral delta | NO | Clean lift. |
| 3 | Tracing formatter conflict | NO | Tracing emission uses `?ev` debug format; verified visually during batch. |
| 3 | Trait-indirection wrong shape | NO | EtwSysCalls trait shape works at compile time; codegen-parity verified empirically at batch Step 27 (zero indirection, all 6 ETW APIs called via direct `callq *__imp_XXX(%rip)`). |
| 3 | asm codegen-parity fail | NO (verified PASS) | Visual diff on monomorphized framesage-svc.s confirms zero trait dispatch overhead. `_asm_baseline` Cargo feature determined unnecessary; v4.3 Section 1 closes this item. |
| 3 | User rejects arch §2.1 mode 5 amendment | NO | PR #77 reviewed during batch Step 24; user ratified Reading 1 (D1 Drop is load-bearing in production, consistent with mode 5 amendment's panic-isolation design). |
| 4 | Mock injection impossible without invasive surgery | NO | Tests landed cleanly via per-method scripted queues. |
| 5 | EOD verification deviation (especially stale session after shutdown) | NO — all EOD steps verified | Stale-session-after-shutdown specifically tested at Step 24 + Step 28 (d). Both passed via D1 Drop fallback path (Reading 1). Architecture §2.1's promise validated. |

### Batch-additional stop-gates triggered + dispositions

These are STOPs the agent surfaced DURING the Windows batch session (not anticipated in plan §6) and the user's disposition for each:

| Batch step | STOP cause | Disposition | Resolution |
|---|---|---|---|
| Env attest #1 | PowerShell tool unelevated | User restarted PowerShell window expecting elevation to propagate; agent verified the relaunch had no effect because Claude Code itself is a per-user app + UAC config is case (c) silent-elevate. Switched to `Start-Process -Verb RunAs` for admin ops. | Resolved before agenda step 1. v4.3 Section 4 documents the operating model. |
| Env attest #2 | Existing v0.6 framesage service running on host (not from this engagement) | User authorized full Step 20.5 uninstall path; ProgramData preserved (Entry 9 upgrade scenario set up "for free"). | Resolved at Step 20.5; v0.6 uninstall validated as positive finding. |
| Step 8 | `cargo build -p framesage-svc --release` rejected: package name typo in agenda | User authorized fix-forward with `-p framesage-service` + one-line note in v4.3 amendment. | Resolved at Step 8; v4.3 Section 1. |
| Step 9 (×4 rounds) | Iterative real-Windows findings: (1) name collision, (2) parallel StartTraceW serialization, (3) leaked sessions from prior runs, (4) verification of #2 as real not artifact | All four user-authorized fixes landed inline (F1, G1, kernel cleanup, D1). Each round produced a permanent fix to feat/group-a-week-2. | Resolved with commits `9998ec9` → `a5b955f` → `23e6457` → `35b7cb0`. |
| Step 11 (×3 rounds) | Workspace-wide test failures: (1) layering registry not updated for `framesage-etw`, (2) closed_loop test mis-scoped for elevated context (tokio::spawn outside runtime), (3) inherited kernel leak | All fixed in one commit `39644f6` (layering update + build-gate seam + Drop on SessionShutdownHandle). | Resolved at Step 11. |
| Step 16 | Entry 5 hypothesis test ("Mac thinks `ERROR_ACCESS_DENIED` not exported") | Grep proved otherwise: `windows-0.58.0/.../Foundation/mod.rs:1087 pub const ERROR_ACCESS_DENIED: WIN32_ERROR = WIN32_ERROR(5u32);` exists. Refactored inline to use canonical import; removed private helper. | Resolved at Step 16 with commit `98128a5`. |
| Step 20.5 | v0.6 uninstall left 2 self-locked binaries behind (CLI executing itself + tray running) | User authorized: tray-kill + manual binary delete + install-dir remove. Encountered Claude-tool-layer safety guard blocking `Remove-Item` on `C:\Program Files\*`; user ran the 3 Remove-Item commands manually. | Resolved with user-manual cleanup. v4.3 Section 4 documents Claude-tool-layer guard as operating-model finding. |
| Step 23 (post-finding) | Game Mode journal "Show Game Mode journal" tray menu pops Windows ShellExecute "cannot find" dialog when journal absent (v0.6 UX bug) | Logged as v0.6 UX backlog item (Section 7 of v4.3 amendment); not blocking. | Deferred to follow-up PR. |
| Step 23 (post-finding) | Silent policy.json mutation (serde-round-trip materializes `closed_loop_enabled: false`) | User accepted as designed-but-undocumented; document in v0.7 README per Section 6 of v4.3. | Logged; behavior is benign + arguably desirable (policy.json self-documents). |
| Pre-Step-23 | Workspace `Cargo.toml` still says `version = "0.5.0"` despite "v0.7" engagement vocabulary | User deferred to ship-prep checklist (v4.3 Section 6); not blocking week 2 completion. | Logged. |
| Step 24 (architecture) | Teardown went through D1 Drop, NOT supervisor's clean shutdown() | User ratified Reading 1: D1 Drop is intentionally load-bearing per PR #77 mode 5 design's "closed-loop tasks outside watchdog → cancellation on runtime shutdown → Drop owns teardown" corollary. | Resolved; v4.3 Section 2 promotes this from "defensive belt-and-suspenders" to "load-bearing design pattern" with corresponding rule for Group B/C resource types. |

---

## 12.6 Deviations from plan

### Mac-side deviations (Days 1–5)

| # | Plan section | Deviation | Resolution |
|---|---|---|---|
| 1 | §3.1 RtlGetVersion binding | Plan said `Win32::System::SystemInformation` + `OSVERSIONINFOEXW`. Reality: windows-rs 0.58 has `RtlGetVersion` in `Wdk::System::SystemServices` + uses `OSVERSIONINFOW` (the smaller struct; dwBuildNumber is in both). | Day 1 inline correction. Added `Wdk_System_SystemServices` feature to `framesage-etw/Cargo.toml`. Architecture's "don't fall back to `GetVersionEx`" stop-gate honored — same binding at a different path. |
| 2 | §3.4 EtwSysCalls trait signatures | 5 mechanical signature deltas vs windows-rs 0.58 (`&mut CONTROLTRACE_HANDLE` → `*mut CONTROLTRACE_HANDLE`; `control_code: u32` → `EVENT_TRACE_CONTROL` typed wrapper; `*mut FILETIME` → `Option<*const FILETIME>`; `OSVERSIONINFOEXW` → `OSVERSIONINFOW`; trait methods became `unsafe fn`). | Day 3 inline correction. Trait methods are `unsafe fn` (deviation from plan's safe-fn signature) — rationale: every method takes raw pointers, making the trait method `unsafe` keeps the SAFETY chain visible from caller to real impl. |
| 3 | §3.5 #4 ConsumerState design | Plan said ConsumerState holds `S: EtwSysCalls`. Combined with §3.4's `RefCell<VecDeque<...>>` mock-queue choice, this required `S: Sync` for `Arc<ConsumerState<S>>: Send`, but `RefCell` is not `Sync`. Plan didn't anticipate the conflict. | Day 3 inline Option B: ConsumerState becomes non-generic (just `events_seen: AtomicU64`). Consumer thread closure captures `syscalls: S` by move (`Send + 'static` suffices, no Sync needed). EtwSession holds `syscalls: S` directly. Mock-injection architecture preserved. |
| 4 | §4 Day 4 — Mode 3 wire | Plan listed Mode 3 as a degradation-mode test only. Day 4 surfaced that the test needs an actual production wire to emit on poll. | Day 4 added `EtwSession::poll_drop_stats(on_event: impl Fn(DegradationEvent))` as production code. Flagged in commit message per user guidance "surface production additions, don't fold them in silently." |
| 5 | §4 Day 5 — drop-poll sibling task wiring | Plan pseudo-code spawned only the supervisor task and used `into_supervisable_parts` which consumes the EtwSession; the prose said "drop-poll sibling task calls `query_stats()`" but the sibling task had no way to access the session after decomposition. | Day 5 added `MonitorHandle<S>` type + `EtwSession::into_supervisable_parts_with_monitor()` returning a 4-tuple. MonitorHandle owns a clone of syscalls + session_name + `Arc<ConsumerState>`; read-only access to stats from sibling task. Mac-side uncertainties Entry 7. |
| 6 | §7 acceptance criteria | Plan didn't list `Policy::closed_loop_enabled` as a new policy field (mentioned in §3.5 + Mac-side Entry 9 but not formally added to the acceptance bulletins). | Day 5 added `closed_loop_enabled: bool` to `framesage_core::Policy` (defaults to false via `#[serde(default)]`). Three `Policy { ... }` literal sites updated (crates/core, crates/ipc, crates/service). |

### Windows-batch deviations (Steps 5–28)

| # | Agenda step | Deviation | Resolution |
|---|---|---|---|
| 7 | Step 8 | `cargo build -p framesage-svc --release` rejected by cargo: `framesage-svc` is the BINARY name (`[[bin]]` declaration), not the PACKAGE name (`[package].name = "framesage-service"`). | Agenda text correction: use `-p framesage-service`. Documentation slip, no behavioral consequence. |
| 8 | Step 9 — `#[ignore]`'d real-ETW tests | Plan didn't anticipate (a) shared session-name collisions across parallel tests, (b) per-process kernel-side StartTraceW serialization, (c) kernel-owned session lifetime exceeds process lifetime, (d) elevated-context branching reaching `tokio::spawn` outside a runtime. | Four inline fixes (F1 PID-suffixed names; G1 `#[serial_test::serial]`; D1 `impl Drop for EtwSession`; D1' `impl Drop for SessionShutdownHandle`) — none of which were in the plan. Each fix permanent on `feat/group-a-week-2`. |
| 9 | Step 11 — workspace tests | `framesage-core::layering::workspace_layering_invariants_hold` failed: new `framesage-etw` crate not registered in allowlist; `ARCHITECTURE.md` lacked the row. Mac-side per-crate tests didn't run workspace-wide invariants. | Updated `crates/core/src/layering.rs` ALLOWED_EDGES + added invariant 8 to ARCHITECTURE.md. |
| 10 | Step 16 — Entry 5 hypothesis | Mac-side guessed `ERROR_ACCESS_DENIED` wasn't exported in windows-rs 0.58. Real Windows grep proved it IS exported at the standard path. | Refactored inline to use canonical `windows::Win32::Foundation::ERROR_ACCESS_DENIED`; removed Mac-side's private helper. |
| 11 | Step 23 — closed_loop test | Test was a plain `#[test]` that assumed AccessDenied branch on Windows (the Mac-side blind spot — Mac couldn't elevate). Elevated context took the Running branch into `tokio::spawn` outside a runtime, panicking. | Added `CLOSED_LOOP_BUILD_OVERRIDE` test seam to `closed_loop.rs` (mirrors framesage-etw's pattern); rewrote test as `#[tokio::test(flavor = "multi_thread")]` + `BuildOverrideGuard::set(Some(Ok(22631)))`. |

### Deviations accepted as designed (not fixed)

| Behavior | Reason |
|---|---|
| Silent policy.json mutation on first IPC-triggered save post-upgrade | Serde-round-trip materialization works correctly; new fields appear with default values; benign + arguably desirable (self-documenting). v0.7 README needs a one-line explanation. |
| D1 Drop is load-bearing in production (not defensive belt-and-suspenders) | Intentional corollary of PR #77 Mode 5 amendment's panic-isolation choice (closed-loop tasks outside watchdog → cancellation on runtime shutdown → Drop owns teardown). Reading 1 ratified by user at Step 24. v4.3 Section 2 promotes this to a pattern rule. |
| Game Mode "Show Game Mode journal" tray menu pops Windows ShellExecute dialog when journal absent | Pre-existing v0.6 UX bug; not in scope of v0.7 closed-loop work. Logged for v0.6 follow-up PR. |
| Workspace `version = "0.5.0"` unchanged across v0.6 (Phase 2) + v0.7 (Phase 3) | Engagement vocabulary "v0.7" ≠ semver. Bump deferred to ship-prep checklist (v4.3 Section 6). |

All deviations accumulate into the **v4.3 plan-vs-reality amendment** on
branch `plan/group-a-week-2-v4.3-amendment` (draft landing at agenda
step 31, separate PR coordinated with PR #77 and feat/group-a-week-2 PR
at step 32). Full per-finding detail in `spike/mac-side-uncertainties.md`
(Entries 1–9, all now resolved).

---

## 12.7 Recommendation

**GO — week 2 complete; proceed to week 3 (event parsers).**

Definitive after Windows runtime batch 2026-05-17. The conditional gates
from the prior version of this section all resolved positively:

| Gate | Outcome | Evidence |
|---|---|---|
| Step 28 (survives-restart) succeeds | ✅ PASS | §12.4 step 28 — Architecture §2.1's "Survives service restarts" promise validated empirically. `cleanup_stale_session` reclaimed force-killed leak; subsequent `StartTraceW` succeeded; PID-2 ≠ PID-1. |
| Step 27 (asm codegen-parity) shows zero dynamic dispatch | ✅ PASS | §12.4 step 27 — All 6 ETW APIs called via direct `callq *__imp_XXX(%rip)`. No `RealEtwSysCalls` / `EtwSysCalls` symbols in monomorphized binary (inlined away). |
| Real-Windows tests pass (Step 9 + Step 11) | ✅ PASS | §12.3 — 250/250 workspace tests with 0 failures after 4 inline real-Windows fixes (F1/G1/D1 + D1' + layering + closed_loop seam) |
| EDR matrix validation | ⚠ DEFERRED TO v0.7.1 | Per `spike/etw-edr-report.md` §6 (Phase 2 sign-off Decision 2). Not in scope for Group A week 2 completion. |

### Week 2 deliverables summary

**Code surface — final state on `feat/group-a-week-2`:**

| Crate | Tests passing | Notes |
|---|---|---|
| `framesage-etw` (new) | 20 (incl. 3 #[ignore]'d real-ETW) | Build gate, EtwSysCalls trait, EtwSession + Drop, SessionShutdownHandle + Drop, MonitorHandle, SupervisorLoop, all 6 degradation modes |
| `framesage-service` (existing + new closed_loop module) | 11 | closed_loop wire, build-gate seam, structured logging |
| `framesage-core` (layering invariants) | 54 | `framesage-etw` registered in ALLOWED_EDGES |
| Workspace total | 250 | 0 failed, 0 ignored under `--include-ignored` |

**Architectural validation:**
- ETW kernel session creation under LocalSystem token: verified
- `cleanup_stale_session` + `StartTraceW` retry path under abrupt-termination scenario: verified
- D1 Drop pattern (load-bearing in production per Reading 1): verified at Steps 24 + 28
- Trait-dispatch zero-overhead claim: verified at Step 27
- v0.6 → v0.7 policy upgrade non-breaking: verified at Step 21 + Entry 9
- Structured logging at both decision-tree branches: verified at Step 26

**8 commits on `feat/group-a-week-2` since batch start:**

```
98128a5  fix(etw): Entry 1 + Entry 5 resolved per Windows runtime batch Steps 12 + 16
39644f6  fix(etw,service,core,arch): Step 11 findings — SessionShutdownHandle Drop, closed_loop build-gate seam, layering registry
35b7cb0  chore: lock serial_test 3.4.0 + scc 2.4.0 + sdd 3.0.10 in Cargo.lock
23e6457  fix(etw): impl Drop for EtwSession with leak-prevention fallback (Step 9 finding #2)
a5b955f  fix(etw): serialize real-ETW tests with #[serial_test::serial] (Step 9 finding)
9998ec9  fix(etw): real-ETW test isolation via unique session names (Step 9 finding)
[+ this report fill-in commit]
[+ subsequent v4.3 amendment cross-reference, if any]
```

(Plus prior Mac-side commits f4b6e83 through 56995ea covering Days 1-5.)

### Week 3 entry criteria — all met

- All 9 Mac-side uncertainties resolved or verified (per
  `spike/mac-side-uncertainties.md`)
- Architecture §2.1 promises validated empirically on real Windows
- Test infrastructure stable: 250/250 with 0 flake under default
  parallelism (after D1 + serial_test + unique names + Drop pattern)
- Codegen parity confirmed; no trait abstraction overhead to refactor
  before week 3 builds parsers on top
- v0.6 → v0.7 upgrade path verified non-breaking on a real
  v0.6-installed host

### Pre-week-3 prerequisites (NOT week 2 blockers)

These items unblock week 3 cleanly but were intentionally scoped out
of week 2:
- v4.3 plan-vs-reality amendment lands (separate PR via
  `plan/group-a-week-2-v4.3-amendment` — agenda step 31)
- PR #77 (arch §2.1 mode 5 amendment) merges
- `feat/group-a-week-2` PR opens + reviews + merges
- See agenda step 32 for the coordinated three-PR sequence

### Ship-prep items deferred to v0.7 release-prep (NOT week 2 blockers)

Captured in v4.3 amendment Section 6:
- Workspace `Cargo.toml` version bump (currently `0.5.0`)
- v0.7 README updates (closed-loop section, policy.json field-
  appears-on-first-save explainer, opt-in toggle docs)
- `install.ps1` version-name strings audit
- Re-run install.ps1 with new-version binaries before shipping
- Authenticode signing decision (closes the unsigned-binary line in
  `spike/etw-edr-report.md` §6.1)
- MSI/Inno installer question
- Pre-ship: EDR matrix validation per `spike/etw-edr-report.md` §6.1
  criteria — all four mandatory before v0.7.1 default-on flip

---

## 12.8 Windows runtime batch agenda

**Single ordered list, executable in one Windows session on the F:
drive.** This consolidates every Mac-side deferred item: `#[ignore]`'d
tests, uncertainties entries, Day 5 EOD steps, asm capture.

### Pre-batch setup

1. `git checkout feat/group-a-week-2 && git pull` — get the latest.
2. Confirm Win11 24H2+ (build 26100 or later) elevated PowerShell.
3. Verify no other ETW consumer holds the `FramesageEtw` session
   name: `logman query FramesageEtw -ets` should return "Data
   Collector Set was not found."
4. If a stale session exists from a prior run: `logman stop FramesageEtw -ets`.

### Build + lint verification (real Windows toolchain)

5. `cargo check --workspace` — workspace clean on native Windows.
6. `cargo clippy --workspace --all-targets -- -D warnings` — clean.
7. `cargo fmt --check` — clean.
8. `cargo build -p framesage-svc --release` — release binary builds.

### Test execution (native Windows)

9. `cargo test -p framesage-etw -- --nocapture --include-ignored` —
   runs all 14 + the `#[ignore]`'d batch tests:
   - `real_etw_session_starts_and_stops_cleanly` (Day 2)
   - `real_etw_session_drop_path_fires_event` (Day 4)
   - `mode_5_session_level_full_flow_panic` (already runs on
     Windows; verifies on real Windows runtime too)
10. `cargo test -p framesage-service` — runs 8 host tests + the
    Windows-only build path. Verify all green.
11. `cargo test --workspace -- --include-ignored` — full sweep
    catches any cross-crate issues.

### Per-uncertainties-entry verification

12. **Entry 1 (RtlGetVersion):** real `detected_build()` returns
    Some(N) where N matches `(Get-ItemProperty
    'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -Name UBR).UBR`
    (or `[System.Environment]::OSVersion.Version.Build`).
13. **Entry 2 (trait-signature deltas):** if test #9 passes, signatures
    are correct.
14. **Entry 3 (ConsumerState design):** the supervisor Mode 5 test
    in #9 exercises the catch_unwind + AssertUnwindSafe boundary on
    real Windows.
15. **Entry 4 (MockEtwSysCalls Clone semantics):** Mode 3 + Mode 5
    test #9 outcomes confirm per-clone-state is fine for current
    tests.
16. **Entry 5 (ERROR_ACCESS_DENIED helper):** `grep -rn ERROR_ACCESS_DENIED
    ~/.cargo/registry/src/index.crates.io-*/windows-0.58.0/` — if
    windows-rs has it under a different path, refactor inline to use
    the named constant.
17. **Entry 6 (Mode 5 supervisor isolation):** the supervisor-level
    test exists; the session-level full-flow test (Day 4) exercises
    the same path through real wiring. Both run in #9.
18. **Entry 7 (MonitorHandle):** Day 5 service wiring — verify the
    drop-poll task starts when closed_loop_enabled = true + build
    gate passes. Look for "closed-loop ETW session started" +
    (after force-stop) "ETW drop-poll task terminating; session
    likely closed" in the log.
19. **Entry 8 (_silence_warnings host-rot):** verified on Mac (test
    #10 ran); no Windows-side verification needed.
20. **Entry 9 (Policy::closed_loop_enabled):** load a v0.6
    policy.json (without the field); verify Policy.closed_loop_enabled
    defaults to false; flip to true via policy.json edit; restart
    service; verify the build-gate path is taken.

### Day 5 EOD verification (per plan §4 Day 5)

21. Install built service: `cargo install --path crates/cli` then
    `framesage install` (or equivalent v0.6 install command).
22. Step 1: `Get-Service framesage` with policy.closed_loop_enabled
    = false → confirm static-rule path log.
23. Step 2: edit policy.json to set closed_loop_enabled = true;
    restart; `logman query FramesageEtw -ets` confirms session
    running.
24. Step 3: `Stop-Service framesage`; `logman query FramesageEtw -ets`
    confirms session gone.
25. Step 4: cross-reference test results from #9.
26. Step 5: INFO log on supported build (Win11 24H2+) AND
    INFO log on synthetic unsupported build (via build_gate test
    override during the test runs).
27. **Step 6 — codegen-parity asm capture:**
    ```powershell
    cargo rustc -p framesage-etw --release --lib -- --emit=asm -C codegen-units=1
    # Locate .s file in target\release\deps\
    # Extract: cargo asm framesage_etw::session::windows_impl::RealEtwSysCalls::start_trace
    # OR: awk '/^_ZN.*framesage_etw.*RealEtwSysCalls.*start_trace/,/^$/' target\release\deps\framesage_etw-*.s
    ```
    Visually verify: no `call` through `[register]`, no `mov rax,
    [rsi+0xN]` followed by `call rax`, no indirect `jmp` through
    register loaded from memory. Direct `call StartTraceW` (or
    similar symbol) is expected.
28. **Step 7 — survives-restart sequence (four logman captures):**
    ```powershell
    Start-Service framesage; logman query FramesageEtw -ets  # (a) post-start
    $pid = (Get-Process framesage).Id; Stop-Process -Force -Id $pid
    logman query FramesageEtw -ets                            # (b) post-force-kill
    Start-Service framesage; logman query FramesageEtw -ets   # (c) post-restart
    Stop-Service framesage; logman query FramesageEtw -ets    # (d) post-clean-stop
    ```
    Annotate each with `Get-Process framesage` PID before/after.

### Post-batch deliverables

29. Update this report's `[Windows batch — pending]` sections with
    literal command outputs from steps 5–28.
30. Update `spike/mac-side-uncertainties.md` Entries 1–9 with
    "**Resolved (Windows batch):** ..." subsections; if any entry
    surfaces a real bug, escalate to a numbered Entry in
    `audit/buddy-disagreements.md`.
31. Draft + commit the **v4.3 plan-vs-reality amendment** on a new
    branch `plan/group-a-week-2-v4.3-amendment` consolidating:
    - Day 1 RtlGetVersion module path correction (§3.1)
    - Day 3 trait-signature deltas (§3.4) + ConsumerState design
      change (§3.5 #4)
    - Day 4 poll_drop_stats production wire (§4 Day 4)
    - Day 5 MonitorHandle + into_supervisable_parts_with_monitor
      (§4 Day 5 + new §3.7 MonitorHandle)
    - Day 5 closed_loop_enabled policy field (§7 acceptance)
    - any asm-capture deviations from the `_asm_baseline` feature
      gate plan
32. Open PR for the v4.3 amendment. Coordinate with PR #77 (arch
    §2.1 mode 5 amendment) and the feat/group-a-week-2 PR — three
    PRs land in coordinated order: arch #77 → v4.3 plan amendment →
    feat/group-a-week-2.

### If batch surfaces a STOP-level issue

Triage during batch session per standing instructions. Document in
`audit/buddy-disagreements.md` as Entry-N if cross-product or
architectural; document inline in this report if it's a single-step
fix-forward.
