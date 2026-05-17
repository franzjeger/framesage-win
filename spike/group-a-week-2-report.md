# v0.7 Group A — Week 2 EOD report

**Status:** Mac-side code surface complete (Days 1–5). Windows
runtime verification deferred to single end-of-week batch session per
strategy shift 2026-05-17. Sections marked `[Windows batch — pending]`
get filled in during that session.

**Branch:** `feat/group-a-week-2` (HEAD: TBD after Day 5 commit).
**Plan reference:** `spike/group-a-week-2-plan.md` v4.2 APPROVED.
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

**Windows runtime host:** `[Windows batch — pending]`

```text
PS> [System.Environment]::OSVersion.Version
PS> (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion' -Name UBR).UBR
PS> Get-Service framesage
PS> sc.exe query WinDefend
PS> Get-MpComputerStatus
```

Plus a one-line statement of which dev box, which user account,
elevation status (LocalSystem? elevated user?).

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

**Windows runtime (full suite, including `#[ignore]`'d batch tests):** `[Windows batch — pending]`

```text
PS> cargo test -p framesage-etw -- --nocapture --include-ignored
PS> cargo test -p framesage-service
PS> cargo clippy --workspace --all-targets -- -D warnings
PS> cargo fmt --check
```

---

## 12.4 EOD verification checklist (Day 5) — `[Windows batch — pending]`

Repeat the six-step checklist from §4 Day 5 with literal command
outputs. Each step's outputs filled in during the Windows batch
session.

1. `Get-Service framesage` output for `closed_loop_enabled: false`
   (static-rule path) — `[pending]`
2. `logman query FramesageEtw -ets` output for `closed_loop_enabled: true`
   after restart (session running) — `[pending]`
3. `logman query FramesageEtw -ets` output after `Stop-Service framesage`
   (session gone, no leftover) — `[pending]`
4. `cargo test -p framesage-etw -- --nocapture` output (cross-reference
   §12.3) — `[pending]`
5. INFO-log line showing the build-gate path on a Win11 26200 (build
   supported; closed-loop initializes) AND on a synthetically-mocked
   unsupported build (static-rule fallback log line) — `[pending]`
6. **Day 3 codegen-parity asm capture** per plan §12.4 step 6 +
   v4.2 amendment Finding 3 — `[pending]`. Capture commands:
   ```powershell
   cargo rustc -p framesage-etw --release --lib -- --emit=asm -C codegen-units=1
   cargo rustc -p framesage-etw --release --lib --features _asm_baseline -- --emit=asm -C codegen-units=1
   ```
   Extraction: `cargo asm framesage_etw::session::EtwSession::<method-name>`
   or `awk '/^_ZN.*framesage_etw.*<method-name>/,/^$/' target/release/deps/framesage_etw-*.s`.
   Acceptance criterion: no `call` through vtable, no indirect `jmp`
   through register loaded from memory. Both versions byte-identical
   after symbol-name stripping.

   **Note: v4.2 amendment specified an `_asm_baseline` Cargo feature
   for the no-trait baseline. Day 3 did NOT add this feature** — the
   Real impl uses direct windows-rs calls via #[inline] which IS
   effectively the no-trait baseline, and adding a parallel
   `_asm_baseline` feature gate just for the asm diff felt like
   speculative scaffolding for one verification step. Batch decision:
   either capture the asm for `RealEtwSysCalls::start_trace` and
   visually verify "no virtual dispatch" against an inline-direct
   `StartTraceW` call (same diff intent without the Cargo feature),
   or land the `_asm_baseline` feature as a small follow-up commit
   if the batch finds the visual diff insufficient.

7. **Survives-service-restart capture** per §4 Day 5 step 6 + v4.2
   amendment Finding 2 — `[pending]`. Four literal `logman query`
   outputs from the kill-restart sequence:
   (a) post-start (session present, owned by original PID)
   (b) post-force-kill (session leaked, no owner)
   (c) post-restart (session present, owned by new PID — cleanup ran)
   (d) post-clean-stop (session gone)

   Annotate each with `Get-Process framesage` PID before/after.

---

## 12.5 Stop-gate trip log

| Day | Stop gate (§6) | Triggered? | Disposition |
|---|---|---|---|
| 1 | `RtlGetVersion` binding | YES (engaged-and-resolved-in-flight) | Plan §3.1's stated module path was wrong; actual binding lives in `Wdk::System::SystemServices`. Architecture's "don't fall back to GetVersionEx" gate honored — this is the same binding at a different location. Documented uncertainties Entry 1. |
| 2 | Spike-to-prod behavioral delta | NO | Clean lift. |
| 3 | Tracing formatter conflict | NO | Tracing emission uses `?ev` debug format; deferred to Windows batch for visual verification. |
| 3 | Trait-indirection wrong shape | NO | EtwSysCalls trait shape works at compile time; codegen-parity asm capture deferred to batch. |
| 3 | asm codegen-parity fail | DEFERRED | Per strategy shift — capture happens in Windows batch (§12.4 step 6). |
| 3 | User rejects arch §2.1 mode 5 amendment | NOT YET RAISED | Draft PR #77 opened; user reviews during batch. |
| 4 | Mock injection impossible without invasive surgery | NO | Tests landed cleanly via per-method scripted queues; Day 4 added `expect_query_returning` + `arm_panic_in_process_trace` as natural extensions. |
| 5 | EOD verification deviation (especially stale session after shutdown) | DEFERRED | Per strategy shift — all EOD steps happen in Windows batch (§12.4 steps 1–7). The plan §4 Day 5 step 6 STOP gate ("if step 6 surfaces stale-session-after-crash → Day 2 lift is incomplete") engages during the batch, not Mac-side. |

---

## 12.6 Deviations from plan

Day 1 RtlGetVersion module path + struct (`OSVERSIONINFOEXW` →
`OSVERSIONINFOW`); Day 3 trait-signature deltas (5 mechanical
differences) + ConsumerState design (non-generic; consumer thread
captures S by move); Day 4 Mode 3 production wire
(`poll_drop_stats`) added; Day 5 `MonitorHandle` introduced for the
drop-poll sibling task. All resolved inline per user's "fix in code,
don't re-plan" directive (2026-05-17). All deltas accumulate into the
v4.3 plan-vs-reality amendment landing during/after the Windows
batch.

Full per-finding detail in `spike/mac-side-uncertainties.md`
(Entries 1–9).

The Day 3 ConsumerState design change is the largest deviation —
it changes the relationship between `S: EtwSysCalls` and
`ConsumerState`. The plan §3.5 #4 originally said ConsumerState
holds S; Day 3 reality: ConsumerState is non-generic, S is held by
EtwSession directly, the consumer thread closure captures S by
move. The mock-injection architecture is preserved.

---

## 12.7 Recommendation

**`[Mac-side scope: provisional GO pending Windows batch outcome.]`**

The week-2 Mac-side deliverables are complete: code surface for
Days 1–5, 22 tests passing (14 framesage-etw + 8 framesage-service),
clippy + fmt clean on both host + Windows cross-target, workspace
check green. The end-of-week Windows batch (§12.8 below) is the
load-bearing gate; the GO/NO-GO recommendation upgrades to definitive
after that session.

**Conditional on batch outcome:**
- If batch step 7 (survives-restart) succeeds → week 2 marks
  complete; proceed to week 3 (event parsers).
- If batch step 6 (asm codegen-parity) shows dynamic dispatch in the
  trait-dispatched path → EtwSysCalls abstraction needs rework
  before week 3.
- If anything else in batch §12.8 surfaces a real bug → triage
  during batch, land fix-forward commits, re-run batch.

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
