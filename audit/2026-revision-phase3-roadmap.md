# framesage-win — Phase 3 Roadmap + Phase 4 Process Improvements

**Companion file to:** `audit/2026-revision-phase2-findings.md`
**Date:** 2026-05-18
**Scope:** Translates the Phase 2 findings into a sequenced execution plan (Phase 3) and surfaces the meta/process improvements the audit revealed (Phase 4).

---

## Sequencing principles

1. **Ship-blocking before quality-of-life.** v0.7 BLOCKERs land first. v0.7.1 BLOCKERs land second.
2. **External dependencies first within a bucket.** AC outreach + cert procurement have multi-week response windows — start the clock immediately.
3. **Preserve audit tradition.** Each milestone closes with a buddy-review checkpoint per the 4Q/5Q format at `audit/buddy-format-implementation-phase.md`. Don't replace the existing rhythm; bolt the new findings into it.
4. **Don't reopen merged decisions.** The product positioning, closed-loop-default-off, EDR-as-v0.7.1-gate, and asymmetric-attribution-bands decisions are locked. Findings that contradict those go through `audit/buddy-disagreements.md` first.
5. **Cite-or-defer.** Findings tagged `[UNVERIFIED]` either get a verification step in their fix slot OR a `[UNVERIFIED]` tag on the rendered behavior; never silently promoted to "verified."

---

# Phase 3 — Roadmap

## Week 1 (target completion: 2026-05-25)

Focus: external-clock dependencies + small-risk-large-payoff fixes + the deferred real-mock test.

### W1.1 — Draft + send AC outreach (I-005)
**Owner:** Engineering lead (you)
**Deliverable:** Five drafted emails (Vanguard, EA Javelin, BattlEye, FACEIT, internal note on ESEA), sent to vendor support channels with a one-page architecture summary attached.
**Architecture summary contents:** The AC-aware safe-mode tier scheme (`AntiCheatProfile::Aggressive | Hybrid | SafeMode | Disabled`), the launcher-inheritance pattern for affinity, the explicit "no game-process modifications" invariant under SafeMode, the ten architectural invariants from `audit/research/ANTI-CHEAT-MATRIX.md`, the "we go STANDBY when ESEAClient.exe is detected" decision for ESEA.
**Success criterion:** Five send-receipts captured. No response is expected in the week; track in `audit/research/ANTI-CHEAT-OUTREACH-LOG.md` (new file).
**Effort:** M
**Closes:** I-005 (the outreach launch — not the response gate)

### W1.2 — Fix bf6.exe AC false-positive (A-003)
**Owner:** Engineer
**Change:** Remove the `(AcMarker::Javelin, "bf6.exe")` entry at `crates/sys/src/inner/ac_detect.rs:58`. Add a regression test asserting `bf6.exe` does NOT flip the Javelin presence bit. Update doc-comment to reflect "Javelin detection deferred pending dedicated probe."
**Success criterion:** Build green, new test passes, existing tests pass.
**Effort:** S
**Closes:** A-003

### W1.3 — Add Arc::as_ptr lifetime doc-comment (A-004)
**Owner:** Engineer
**Change:** Add a multi-line doc-comment block above `crates/etw/src/session.rs:1295 fn consumer_loop` enumerating the three contract points (ETW callback timing, state Arc local-frame lifetime, post-ProcessTrace no-callback guarantee). Cross-reference the static_assertion in supervisor.rs.
**Success criterion:** Comment lands. No code change.
**Effort:** S
**Closes:** A-004

### W1.4 — Drop verify_session_gone OR add bounded retry (C-001)
**Owner:** Engineer
**Decision required:** (a) drop the verify step entirely — `ControlTraceW(STOP)` RC is the source of truth; OR (b) retry-loop with 50ms × 5 attempts.
**Recommendation:** (a). The next-start path's `cleanup_stale_session` already handles the "session lingered" case. Verification is double-spending the same guarantee.
**Change:** Remove lines 819 (and the helper `verify_session_gone` at :1233-1242). Update `stop()` to return success after the syscall returns success.
**Success criterion:** Existing tests pass; supervisor / drop-poll integration paths still terminate cleanly on stop.
**Effort:** S
**Closes:** C-001

### W1.5 — Real-mock consumer-thread panic test (E-001)
**Owner:** Engineer
**Change:** Add `crates/etw/src/session.rs::tests::consumer_thread_panic_propagates_via_oneshot` using `MockEtwSysCalls::arm_panic_in_process_trace("synthetic")` infrastructure that already exists. The test calls `EtwSession::start_with_syscalls(mock, opts)`, transitions through `into_supervisable_parts`, and asserts the oneshot delivers `ConsumerExitReason::Panicked { message }` with the synthetic message verbatim.
**Success criterion:** New test passes. Removes the "deferred to end-of-week Windows batch" comment.
**Effort:** M
**Closes:** E-001, partially closes the integration-gap aspect of E-003

### W1.6 — Decide + ship Sessions-tab + onboarding-page-3 (F-001, F-002)
**Owner:** Engineer
**Change:** Decide ship contract for v0.7:
- **Option A:** Defer `closed_loop_enabled` UI exposure to v0.7.1. The field stays in policy.json (default-off). Settings UI does NOT show a toggle. Sessions tab does NOT appear. Onboarding page 3 not yet needed.
  - **Effort:** S (decision-only)
  - **Pro:** Zero new surface; v0.7 ships minimal-risk.
  - **Con:** Spends architecture commitment (§2.4 + onboarding page 3 are documented v0.7 requirements). Users on Win11 24H2 see an inconsistency between architecture doc and shipped product.
- **Option B (REVISED EFFORT — was L, now M):** Scaffold both surfaces in v0.7 with empty-state-only behavior. Sessions tab shows the unsupported-build OR no-sessions-yet empty state per architecture §2.4 verbatim. Onboarding page 3 shows the EDR-disclosure copy with the required substring "EDR validation in progress for v0.7.1". Both code paths are scaffold-only: no IPC additions, no list, no detail, no recording — the `closed_loop_enabled` toggle persists into the existing policy field but no code path consumes it yet.
  - **Effort breakdown (~250–350 LOC total, 2–3 days):**
    - New `crates/tray/src/tabs/sessions.rs` (~120 LOC): two empty-state branches gated on `closed_loop_enabled_for_this_build()`; no list/detail.
    - Tab enum addition + render_tab_strip branch (~10 LOC in main.rs).
    - Onboarding page 3 insertion in `crates/tray/src/onboarding.rs` (~120 LOC): page index renumber, render method, radio buttons binding to `Policy.closed_loop_enabled`, required-substring constant.
    - Two Group C acceptance criterion tests (~80 LOC): the unsupported-build empty state contains "requires Windows 11 24H2 or later" AND does NOT contain "After your first 90-second gaming session"; onboarding page 3 contains "EDR validation in progress for v0.7.1" verbatim.
  - **Pro:** Architecture commitment met substantively. Empty-state copy is load-bearing per Group C acceptance criterion — tests in place from day 1 lock the strings against drift. Onboarding page 3 disclosure protects EDR-managed users at first run.
  - **Con:** Adds a tab visible to users that doesn't do anything yet (until Group C M3.1 fills it). Mitigated by the empty-state copy explicitly framing it as "After your first 90-second gaming session, this tab will show…"
- **Recommendation: Option B.** The original L estimate was for the full tab with list + detail + attribution rendering. At M effort and 2–3 days, Option B meets the architecture commitment with minimal code and locks the load-bearing strings against future drift. The "no recording" pre-condition is enforced because `closed_loop_enabled` defaults false and no IPC consumer exists yet — flipping the toggle is a no-op in v0.7.
**Effort:** M (Option B recommended) / S (Option A fallback)
**Closes:** F-001 + F-002 if Option B selected.

### W1.7 — AC vendor outreach kickoff (I-005)
**Already in W1.1 — listed for cross-reference. External dependency clock starts on Day 1.**

### W1.8 — Authenticode signing cert procurement kickoff (I-001)
**Owner:** You / engineering lead
**Change:** Kick off Sectigo OV cert procurement on Day 1 (not Month 2). Multi-week response curve:
- Day 1: Submit OV order with Sectigo (or chosen CA). Identity verification typically takes 3–10 business days.
- Day 1: Set up Azure subscription for Key Vault HSM (if not already in place). Pre-authorize the storage.
- Day 1: Open internal ticket for AzureSignTool integration.
**Why Day 1, not Month 2:** Original M2.1 placement assumed cert procurement starts when engineering work starts. But cert procurement is wall-clock-bound (3+ weeks for OV-to-SmartScreen-reputation, longer for HSM provisioning). Starting Day 1 of Week 1 means the cert is in hand when M2.1's engineering work needs it. Late start = guaranteed v0.7.1 slip.
**Success criterion:** Cert order submitted with order ID captured in `audit/research/EXTERNAL-DEADLINES.md` (new file per P4.6). Azure Key Vault HSM provisioned.
**Effort:** S (kickoff only; deliverable lands in M2.1)
**Closes:** Half of I-001 (kickoff); M2.1 closes the rest

### W1.9 — EDR matrix engagement kickoff (I-003)
**Owner:** You / engineering lead
**Change:** Engage SOC team / security researchers for the three-product matrix (Defender ATP, CrowdStrike Falcon, SentinelOne Singularity) on Day 1.
- Day 1: Reach out to internal SOC (if available) for Defender ATP test environment. Fallback per `spike/etw-edr-report.md` §6: external security researchers via SANS / NCC Group / Trail of Bits engagement.
- Day 1: Request trial/eval licenses for CrowdStrike Falcon and SentinelOne Singularity on clean Win11 24H2 VMs (~2 weeks lead time for license issuance).
- Day 1: Set up VM templates (Win11 24H2 clean install + framesage-svc.exe + the test suite).
**Why Day 1, not Month 2:** Same logic as I-001 — wall-clock-bound. EDR engagement lead time is 2–3 weeks for licenses + ~1 week per product to actually run the test matrix. Starting Month 2 = guaranteed v0.7.1 slip.
**Success criterion:** All three license/access pathways requested, lead times captured in `audit/research/EXTERNAL-DEADLINES.md`. Test-VM template ready.
**Effort:** M (kickoff only; deliverable lands in M2.2)
**Closes:** Half of I-003 (kickoff); M2.2 closes the rest

### W1.10 — Week 1 buddy review
**Owner:** You + buddy reviewer
**Format:** 4Q planning-phase format per `audit/buddy-format-implementation-phase.md`
**Questions:** (a) Do the W1.1–W1.6 closures match this roadmap's spec? (b) Are W1.2 / W1.3 / W1.4 / W1.5 atomic enough to land in separate PRs? (c) Is the Option-A-vs-B choice in W1.6 the one the project intends (recommendation: Option B per revised effort analysis)? (d) Are the W1.7–W1.9 external-dep kickoffs documented in `audit/research/EXTERNAL-DEADLINES.md` with realistic lead times? (e) Are there NEW Week-1 items the buddy surfaces that this roadmap misses?
**Success criterion:** Verdict captured in `audit/buddy-disagreements.md` if any.

## Month 1 (target completion: 2026-06-18)

Focus: code-quality robustness, architectural cleanups, integration tests.

### M1.1 — `EtwSession::stop` and `query_stats` recoverable-misuse (A-001)
Convert panic-on-misuse to Result return. Effort: S.

### M1.2 — `start_closed_loop_if_enabled` wrapped in `spawn_blocking` (B-001)
Effort: S.

### M1.3 — Watchdog-exclusion test (B-002)
Add a test asserting closed-loop tasks are NOT in `runtime::run`'s `tokio::select!`. Could be a string-grep test. Effort: S.

### M1.4 — Document seam-trait pattern (B-003)
New file: `docs/syscall-seam-pattern.md`. Effort: S.

### M1.5 — Drop-poll task vs supervisor shutdown race (C-002)
Shared `Arc<AtomicBool>` "torn down" flag. Effort: S.

### M1.6 — Manual-global Game Mode interaction matrix tests (C-004 + D-006)
Six-cell matrix as integration tests against the SysApi mock. Effort: M.

### M1.7 — Extend RefUnwindSafe static_assert (D-001)
Add `assert_impl_all!(RealEtwSysCalls: RefUnwindSafe);`. Comment on the MockEtwSysCalls case. Effort: S.

### M1.8 — Tray multi-line error rendering (F-003)
Audit `Response::Error` rendering in tray; ensure multi-line strings display readably. Effort: S.

### M1.9 — Verify parking_lot migration (F-004)
Grep `crates/tray/src` for residual `std::sync::Mutex`. Close or escalate. Effort: S.

### M1.10 — Verify `%ProgramFiles%` install path + ACL (I-004)
Manual install on clean VM; capture `icacls` output to `audit/research/install-verification-2026-06.md`. Effort: S.

### M1.11 — Document Manual Global Game Mode CLI verbs (J-003)
README "Power-user workflows" section. Effort: S.

### M1.12 — Month 1 buddy review (4Q format)

## Month 2 (target completion: 2026-07-18)

Focus: v0.7.1 unblockers (signing-cert + EDR-matrix kickoff) + remaining Quality items.

### M2.1 — Authenticode signing — workflow + verification surface (I-001 closeout)
**Pre-condition:** Cert procured + Azure Key Vault HSM provisioned during Week 1 (W1.8).
**Sub-tasks:**
- GitHub Actions workflow: `cargo build --release` → `AzureSignTool` → `signtool verify`
- New CLI verb: `framesage --verify-signature` calling `WinVerifyTrust` on every shipped binary
- `install.ps1` change: `Get-AuthenticodeSignature` check on staged binaries; abort install on `Status -ne 'Valid'` per architecture §2.5
- README "Verifying the binary" section
**Effort:** L (cert is already in hand thanks to W1.8 kickoff)
**Closes:** I-001 (full closeout)

### M2.2 — EDR-matrix — execute test runs across three products (I-003 closeout)
**Pre-condition:** EDR licenses / test access acquired during Week 1 (W1.9).
**Sub-tasks:**
- Run framesage closed-loop end-to-end on Defender ATP test environment; capture any alerts triggered. Document in `spike/etw-edr-report.md` §6.1.
- Run on CrowdStrike Falcon test environment; same.
- Run on SentinelOne Singularity test environment; same.
- Produce v0.7.1 default-on-flip attestation per architecture §"NOT a Group A blocker".
**Effort:** L (licenses in hand from W1.9; the test runs themselves are ~3 engineer-days for the matrix)
**Closes:** I-003 (full closeout)

### M2.3 — `EtwSubsystem::DisabledRetryable` discriminant (A-002)
Effort: S.

### M2.4 — Per-PID Subscribe cap (A-005)
Plumb `GetNamedPipeClientProcessId`. Effort: S.

### M2.5 — SafeList::bundled() static reference (A-006 + G-003)
Effort: S.

### M2.6 — Etw-side build-gate override fold-in (H-004)
Move thread-local override into `framesage-etw` crate; service-side seam becomes a re-export. Effort: S.

### M2.7 — Engine/lib.rs reviewability split (K-005) — SEQUENTIAL, Month 2 priority
**Owner:** Engineer (dedicated for the duration)
**Rationale for sequencing K-005 BEFORE K-004:** 53 inline engine tests provide objective per-sub-PR verification; engineer learns the "split rhythm" on the higher-confidence target. Tray-split (K-004) has no test safety net (eframe render path has no unit coverage — visual screenshot diff is the only verification) and benefits from the rhythm + reviewer pattern established here first.
**Sub-PRs (sequenced, low-to-high regression risk):**
1. `engine/system_state.rs` (~40 LOC) — PlatformQuery impl
2. `engine/apply.rs` (~350 LOC) — apply_profile + helpers + safe-list gate
3. `engine/system_mode.rs` (~300 LOC) — three system-mode methods
4. `engine/reconcile.rs` (~1,000 LOC) — tick + four maybe_* + reconcile
5. `engine/api.rs` (~1,400 LOC) — 30 public methods
**Per-PR reviewer requirement:** all 53 inline tests stay green; `cargo clippy -- -D warnings` clean; explicit "lock ordering preserved" check.
**Effort:** XL (~2–3 engineer-weeks for the 5 sub-PRs sequentially)
**Closes:** K-005
**Regression-risk annotation:** HIGH — lock-ordering across maybe_* + reconcile + apply paths is the load-bearing invariant; any reshuffle that breaks it surfaces as production deadlock or apply-stuck states.

### M2.8 — Month 2 buddy review

**K-004 (tray/main.rs split) moved to Month 3 spillover (M3.6).** Honest Month-2 engineer-week accounting: M2.1 (~1wk) + M2.2 (~1wk) + M2.3–M2.6 (~1wk combined) + K-005 (~2–3wk) already totals ~5–6 engineer-weeks. Adding K-004 (~2–3wk) puts Month 2 at 7–9 weeks of work in a 4-week calendar; one engineer cannot context-switch between two ~3,000-LOC refactors simultaneously without lock-ordering or visual-state mistakes. Sliding K-004 is defensible because K-004 is MEDIUM severity (reviewability improvement, not BLOCKER for v0.7.1 ship).

## Month 3 (target completion: 2026-08-18)

Focus: v0.7.1 ship readiness — honesty contract tests, negative-session verification, polish, then v0.7.1 tag.

### M3.1 — Group C deliverable kickoff (Sessions UI + honesty contract)
**Sub-tasks:**
- Scaffold `framesage-recorder` crate (~600 LOC per architecture estimate)
- Scaffold Sessions tab in tray (`crates/tray/src/tabs/sessions.rs` — modular per item 3.6)
- Implement `compute_attribution_summary` with asymmetric bands per architecture §2.4
- Five honesty-contract tests (C-005)
- Disabled-attribution case tests (PRE-L-003)
- Negative-session manual verification + screenshot (E-004)
- Empty-state UI per architecture §2.4 (F-002 if not closed in W1)
**Effort:** XL (multi-engineer-week)
**Closes:** C-005, E-004, F-002, PRE-L-003

### M3.2 — Group B deliverable kickoff (PresentMon + recorder integration)
**Sub-tasks:**
- Scaffold `framesage-presentmon` crate (~600 LOC)
- Subprocess management with rate-limit (≥30s between spawns) + reuse-if-same-process-name policy (PRE-L-004)
- Per-process schema slot test path (PRE-L-001)
- Retention boundary tests (80% / 95% / 100%) (PRE-L-002)
**Effort:** XL
**Closes:** PRE-L-001, PRE-L-002, PRE-L-004

### M3.3 — Architecture documentation drift (A-007 + D-002 + D-004 + G-001)
Consolidate code-comment doc-debt items. Effort: S each.

### M3.4 — Remove `crates/spike-etw` (K-001)
After Group A week 5+ stabilizes. Effort: S.

**Note:** PRE-L-006 (verify `spike/etw-schemas.md` exists) was resolved inline during Phase 3 roadmap drafting — file exists at 713 LOC with proper provider GUIDs, authority citations, and Win11 26200 empirical observations per the ground rule. No tracking needed; closed.

### M3.6 — Tray/main.rs reviewability split (K-004) — SPILLOVER from Month 2
**Owner:** Engineer
**Pre-condition:** K-005 (M2.7) closed. The split rhythm + reviewer pattern is established.
**Sub-PRs (sequenced, low-to-high regression risk):**
1. `tabs/settings.rs` (~500 LOC extraction) — 5 render_settings_* methods + reset-confirm modal
2. `tabs/activity.rs` (~120 LOC)
3. `tabs/rules.rs` (~450 LOC) — render_rules_tab + render_affinity_rules_section
4. `tabs/profiles.rs` (~470 LOC)
5. `tabs/status.rs` (~300 LOC)
6. `modals.rs` (~330 LOC) — render_preview_modal + render_terminate_confirm_modal + render_affinity_picker_modal
7. `tabs/processes.rs` (~1,380 LOC — HIGHEST regression risk) — LAST, after rhythm established
**Per-PR reviewer requirement:** before/after screenshots on Windows + "no behavior change" claim, validated manually walking through every tab.
**Effort:** XL (~2–3 engineer-weeks for the 7 sub-PRs sequentially)
**Closes:** K-004; PHASE2-PLAN.md item 3.6 closeout
**Regression-risk annotation:** HIGH — eframe render path has no unit-test coverage; visual diff is the only verification.
**Slip-to-v1.0 trigger:** If Group B (M3.2) + Group C (M3.1) consume most of Month 3 engineering bandwidth, K-004 slides to "Beyond Month 3 / v1.0 prep" rather than competing with v0.7.1 ship-readiness. Decision point: Month 3 mid-checkpoint.

### M3.7 — Month 3 + v0.7.1 tag buddy review (5Q implementation-phase format)

### M3.5 — v0.7.1 default-on flip — only if I-003 (EDR) and I-005 (AC) both close
**Sub-task:** Once both EDR matrix attestation is in place AND every AC vendor has responded (positively or with addressable concerns), flip `closed_loop_enabled: false → true` default in `policy.rs:653, 694`. Remove the "EDR validation in progress for v0.7.1" required substring from onboarding page 3 per architecture §"First-run onboarding."
**Gate:** External dependencies — schedule slips if either I-003 or I-005 slips.
**Closes:** v0.7.1 ship contract

## Beyond Month 3 (v1.0 trajectory)

- I-002 (MSI / WiX installer) — XL
- K-002 (rotate session GUID for v1.0)
- Persistent v1.0 prep: signed installer end-to-end, multi-week of dogfood, polish polish polish.

---

# Phase 4 — Process improvements

This section captures the **meta-observations** about how the project does engineering, not what it builds. They apply to future audit cycles, not just this one.

## P4.1 — The audit tradition is working; preserve it verbatim

**Observation:** The project's SUMMARY.md → PHASE2-PLAN.md → grouped execution → buddy review rhythm is the single biggest defense against the "drift / two factual errors in a row" pattern called out at `audit/v0.7-architecture.md:1540-1546`. Five spot-checks across closed findings ALL passed — the tradition is honest.

**Implication:** Resist the temptation to add new layers. New findings flow into the existing PHASE2-PLAN.md format. New roadmaps reference the existing 4Q/5Q buddy-review format at `audit/buddy-format-implementation-phase.md`. The 2026 revision (this file + the findings file) lives alongside the original audit, not replacing it.

**Action:** None. Continue as-is.

## P4.2 — `[UNVERIFIED]` markers should propagate

**Observation:** The user explicitly required `[UNVERIFIED]` markers on Windows-API behavioral claims without MS-docs/spike/experimental backing. The findings file uses these in 5 places. They are a useful audit-trail device.

**Implication:** Promote `[UNVERIFIED]` from audit-only to source-code convention. Comments in the codebase that claim Windows-API behavior without a doc citation should carry the marker. Example: the `ControlTraceW(STOP)` synchronization assumption at `etw/src/session.rs:819` should carry `// [UNVERIFIED — MSDN doesn't pin down registry-row reclaim timing]` if C-001's fix doesn't drop the verify step entirely.

**Action:** Phase 3 Month 1 — sweep the etw + sys crates for unverified API claims; add markers. Effort: M.

## P4.3 — Buddy review verdicts should be locatable

**Observation:** PR #79's 4Q buddy review surfaced four citation defects (§12.4 step 7→Step 28, step 6→Step 27, "4 batch-fix commits"→6, "8 commits since batch start"→7) and a STOP verdict that was overridden. The defects + the override decision are not centrally captured.

**Implication:** A future audit can't tell which buddy reviews surfaced defects vs. which surfaced clean PRs. The signal-to-noise of the buddy reviews degrades.

**Action:** Standardize buddy-review-result capture in `audit/buddy-disagreements.md` (file already exists per repo listing). Format: `## PR #<N> — <date> — verdict <STOP/GO> — defects: <list>`. Phase 3 Week 1.

**Effort:** S.

## P4.4 — Spike-to-production handoff requires explicit verification commands

**Observation:** The `audit/v0.7-architecture.md:1540-1572` ground rule (spike reports include verification commands + literal output) is necessary but landed late. Phase 1's "Defender is running" claim was wrong; Phase 2's DPC opcode 0x2E was wrong.

**Implication:** The rule is correct but its enforcement is at PR-review only. A more durable mechanism: any spike-derived `unsafe` block or Win32-API call in production code should carry a `// SAFETY-VERIFICATION:` comment block pointing at the spike report section + step number that validated it.

**Example pattern:**
```rust
// SAFETY-VERIFICATION: validated on Win11 24H2 build 26200 per
//   spike/group-a-week-2-report.md §12.4 Step 23.
// xperf cross-check histogram matched within ±0.3%.
unsafe { syscalls.process_trace(&handles, None, None) }
```

**Action:** Phase 3 Month 1 — backfill on the etw crate's existing `unsafe` blocks; require it on future Win32-API additions. Effort: M.

## P4.5 — Decision-log centralization

**Observation:** Architecture decisions live in `audit/v0.7-architecture.md` (Phase 2 sign-off resolutions §1654-1666). PHASE2-PLAN.md decisions live in its own preamble. Per-finding decisions live in SUMMARY.md severity classifications. Cross-finding decisions live in buddy reviews. No central index.

**Implication:** A new contributor reading "why does closed_loop_enabled default-false?" must find the answer across three files. A future audit can't tell what's settled and what's open.

**Action:** Phase 3 Month 2 — create `audit/DECISIONS.md` indexing every decision with date, owner, file:section reference, and "revisit-if" criteria. Effort: M.

## P4.6 — The dual-listed AC outreach pattern is the right shape for external-dependency findings

**Observation:** Finding I-005 (AC outreach) is dual-listed under H (Security) and I (Distribution) per user instruction. The dual-listing makes it visible in two contexts without duplicating the entry. The Day-5 cutoff (2026-05-22) gives the work a real deadline.

**Implication:** Extend the pattern. EDR-matrix (I-003) is similarly external-dependency-bound; signing-cert procurement (I-001) is similarly external-dependency-bound. Both should carry deadlines + dual-listings.

**Action:** Phase 3 Week 1 — add explicit "external dependency deadline" tags to I-001 (cert procurement deadline: 2026-06-15) and I-003 (EDR engagement deadline: 2026-06-22). Track in `audit/research/EXTERNAL-DEADLINES.md` (new file).

**Effort:** S.

## P4.7 — Phase 3 + Phase 4 are now load-bearing on roadmap-execution discipline

**Observation:** This roadmap has 28+ deliverables across 3 months. The project's prior cadence (one PR/day, buddy-reviewed) suggests the deliverable count is plausible — but only if the project resists scope creep.

**Implication:** Each PR should map to exactly one roadmap item OR explicitly say "out of scope for this PR; tracked as <ID>". The mapping should appear in the PR description.

**Action:** Phase 3 Week 1 — add a `.github/PULL_REQUEST_TEMPLATE.md` that includes a "Roadmap item closed:" field. Effort: S.

## P4.8 — The `[UNVERIFIED]`-tagged claims need a periodic sweep

**Observation:** Tagged claims accumulate. Without periodic cleanup, the marker becomes wallpaper.

**Action:** Once per release tag (v0.7, v0.7.1, v1.0) — sweep the codebase for `[UNVERIFIED]` markers, attempt verification, downgrade to `// VERIFIED via <citation>` where possible. Phase 3 Month 3 + v0.7.1 release-readiness checklist.

**Effort:** S per sweep.

## P4.9 — Audit-finding verification asymmetry

**Observation:** Phase 2 spot-check 5/5 PASS validated that CLOSED findings were honest. It did NOT validate that OPEN findings were genuinely open. PRE-L-006 (spike/etw-schemas.md already existed) and E-001 (mode_5_session_level_full_flow_panic already landed in PR #79, 3h 40m before E-001 was written) were both audit-vs-codebase drifts where the audit classified resolved items as unresolved.

**Implication:** open-classification is a weaker guarantee than closed-classification. Future audit cycles must grep-verify each finding against current main before classifying as unresolved. The verification commands belong in the audit file itself (per P4.4's SAFETY-VERIFICATION pattern, applied to finding-existence rather than unsafe-block-soundness).

**Action:** future audit-cycle openers run an explicit verification step ("does the code already do what this finding asks for?") before classifying. Phase 3 Month 1 — backfill verification commands on remaining open Phase 2 findings; flag any other drifts.

**Effort:** M (one-time backfill).

---

## Roadmap summary table

| Phase | Window | Effort | Deliverables |
|---|---|---|---|
| Week 1 | Now → 2026-05-25 | L cumulative | 9 items (W1.1–W1.9) + buddy review. **3 external-clock kickoffs (AC outreach + cert procurement + EDR engagement) fire Day 1.** |
| Month 1 | 2026-05-25 → 2026-06-18 | L cumulative | 11 items (M1.1–M1.11) + buddy review |
| Month 2 | 2026-06-18 → 2026-07-18 | XL cumulative | 8 items (M2.1–M2.8). **K-005 (engine split, sequential) takes 2–3 weeks; K-004 slipped to M3.6 because Month 2 with both splits is 7–9 engineer-weeks in 4 calendar weeks.** Cert + EDR deliverables close (kickoffs were Week 1). |
| Month 3 | 2026-07-18 → 2026-08-18 | XL cumulative | Group B + Group C + v0.7.1 flip + K-004 spillover (or slip-to-v1.0-prep) + buddy review |
| Beyond | 2026-08+ | XL | MSI installer, v1.0 prep |

## Status

Phase 3 roadmap revised post-buddy-feedback (2026-05-18 round 3):
- **Added** K-004 + K-005 (tray/main.rs + engine/lib.rs splits) with HIGH-regression-risk annotation.
- **Re-sequenced** K-005 → K-004 as **sequential** (was parallel). K-005 lives in Month 2 (M2.7); K-004 slipped to Month 3 spillover (M3.6) because honest engineer-week accounting puts Month-2-with-both-splits at 7–9 weeks of work in a 4-week calendar. K-005 first because 53 inline engine tests provide objective per-sub-PR verification; tray-split benefits from established rhythm.
- **Slip trigger documented** on M3.6: if Group B (M3.2) + Group C (M3.1) consume most of Month-3 bandwidth, K-004 slides to "Beyond Month 3 / v1.0 prep" rather than competing with v0.7.1 ship-readiness.
- **Moved** cert-procurement (I-001) and EDR-engagement (I-003) start clocks to Day 1 of Week 1 (was Month 2). Deliverables still land in Month 2 but external-dep clocks fire immediately to avoid guaranteed v0.7.1 slip.
- **Closed** PRE-L-006 inline (spike/etw-schemas.md verified — exists, 713 LOC, structured per ground rule).
- **Revised** W1.6 to Option B (confirmed by user) — M effort, ~250–350 LOC scaffold-only deliverable (Sessions tab empty-state + onboarding page 3 EDR-disclosure + Group-C-acceptance substring tests).

The Phase 2 + Phase 3 + Phase 4 deliverables for the 2026 revision are now captured in:
- `audit/2026-revision-phase2-findings.md` — **44 findings** across 11 axes (added K-004 + K-005)
- `audit/2026-revision-phase3-roadmap.md` — this file (added W1.7/W1.8/W1.9 external-clock kickoffs + M2.7/M2.8 reviewability splits)

Ready for conversion to GitHub issues. Estimated total: **~37 issues** (Week 1: 10, Month 1: 12, Month 2: 8, Month 3: 7).

Title format: `[<id>] <short description>` (e.g., `[W1.2] Fix bf6.exe AC false-positive`).
Body includes: roadmap-item ref, finding-id, effort, severity, summary, acceptance criterion, "Roadmap item closed:" footer per P4.7.
