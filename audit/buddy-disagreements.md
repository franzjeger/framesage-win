# Buddy-system disagreements + flags log

Per the user's standing instructions (2026-05-17, "jog handoff"):
when the buddy-system reviewer raises concerns, the primary agent
must STOP the affected workstream and write up both perspectives
here rather than resolve unilaterally.

This file is the audit trail. The user reviews entries on
return and gives the call.

---

## Entry 1 — 2026-05-17, buddy review of jog-handoff workstreams (a)/(b)/(c)

### Buddy verdict summary

| Workstream | Buddy verdict | Primary-agent action |
|---|---|---|
| (a) Rustfmt fix is purely mechanical | PASS | Apply attempted; second-order block uncovered — see §1.1 |
| (b) Outreach drafts safe to send | FAIL-WITH-FIXES on `02-sysinternals.md`; PASS on `01`, `03`, `04` | STOPPED — see §1.2 |
| (c) Group A week 2 scope matches architecture | PASS (with minor naming note) | HELD pending (a) resolution per user's "after rustfmt PR merges + outreach sent, START Group A week 2" ordering — see §1.3 |

### 1.1 Workstream (a) — Rustfmt fix: secondary block uncovered

**Buddy's verdict on (a):** PASS. Buddy sampled 30+ hunks across
9 files (`crates/cli/src/main.rs`, `crates/core/src/policy.rs`,
`crates/core/src/layering.rs`, `crates/engine/src/lib.rs`,
`crates/tray/src/editors.rs`, `crates/tray/src/main.rs`,
`crates/spike-etw/src/main.rs`, `crates/sys/src/inner/process.rs`,
`crates/core/src/undo.rs`) and confirmed zero semantic changes,
zero renames, zero `unsafe`/`pub`/`use`-target changes. Buddy
reverted the fmt apply via `git stash` after verifying.

**Primary-agent action:**

1. Created branch `chore/rustfmt-baseline` from current main (PR #68 merged HEAD, `f55ac7a`).
2. Ran `cargo fmt` (workspace-wide).
3. Confirmed `cargo fmt --check` now exits 0.
4. **Ran `cargo clippy --workspace --all-targets -- -D warnings` to verify CI would actually go green.** This is the CI's second-half check (the failed job is named "rustfmt + clippy"); on the PR #68 run, clippy never executed because rustfmt failed first and short-circuited it.
5. Clippy reported **7 errors** in `crates/spike-etw/src/main.rs`:
   - 5× `clippy::unusual_byte_groupings` (`error: digits of hex, binary or octal literal not in groups of equal size`) on GUID literals at lines 70, 87, 89, 91, 93 — e.g. `const SESSION_GUID: GUID = GUID::from_u128(0x4F8B_1A60_9E2D_4F3F_88C2_5B7E1D6F92A4);` — clippy wants uniform digit-group sizes.
   - 1× `clippy::declare_interior_mutable_const` at line 142 — `const ZERO: AtomicU64 = AtomicU64::new(0);` — clippy wants `static` or `#[allow]`.
   - 1× `clippy::unnecessary_sort_by` at line 492 — `rows.sort_by(|a, b| b.1.cmp(&a.1))` — clippy suggests `sort_by_key(|b| std::cmp::Reverse(b.1))`.
6. All 7 are clippy-roll-forward issues against code that landed in PR #65 (commit `6d43dd3`). They became errors because the toolchain's stable clippy expanded lint coverage between PR #65's merge and now.
7. Fixing them requires **semantic edits**:
   - Re-grouping hex digits changes source text but preserves the GUID value — closest to "mechanical" but still a textual edit beyond formatter scope.
   - `const`→`static` (or `#[allow(...)]` annotation) is a semantic change.
   - `sort_by`→`sort_by_key(|b| Reverse(b.1))` is a semantic change.

**Stop-gate triggered.** The user's instruction (2026-05-17):

> "Standard 'cargo fmt apply, push, merge' — no semantic changes,
> pure formatter output. If anything beyond mechanical reformatting
> needs to change to make CI green, STOP and surface it before
> merging."

The primary agent stopped. The fmt-only commit lives at
`chore/rustfmt-baseline` HEAD (`f703259`, 20 files, +396/-571,
zero `.claude/` contamination after one false-start that was
soft-reset). It has not been pushed.

**What the user needs to decide:**

- **Option 1:** Push the fmt-only commit anyway, open PR, merge. CI will still go red on clippy; the "rustfmt + clippy" job still fails. Net effect on Group A's CI signal: same red baseline, different reason. The rustfmt PR fixed *its* lint but didn't fix CI overall, so the original "Group A produces clean signal" goal isn't met.
- **Option 2:** Expand the PR to include the clippy fixes. The user's stop-gate explicitly disallows this without authorization. Primary agent is NOT taking this option unilaterally.
- **Option 3:** Two PRs back-to-back — push the fmt-only commit now (because buddy verified it), and open a separate clippy-fix PR (semantic edits) where the user can review the seven specific edits.
- **Option 4:** STOP entirely. Defer both fixes. Live with red CI while Group A produces signal; tag Group A PRs explicitly as "CI red on pre-existing main-baseline issues."

Primary-agent recommendation: **Option 3**. The fmt-only commit is fully verified-mechanical by buddy and is one `git push` from a PR. The clippy fixes are tiny (7 edits across 1 file) and individually-reviewable; a separate PR keeps the audit trail clean and lets the user veto specific clippy edits (e.g. if the user prefers `#[allow(clippy::declare_interior_mutable_const)]` over `const`→`static` because the `AtomicU64` zero-init pattern is intentional). But this is a user decision, not a primary-agent decision.

### 1.2 Workstream (b) — Outreach drafts: FAIL-WITH-FIXES on `02-sysinternals.md`

**Buddy's verdict:**

- `01-perfview-issue.md`: PASS. Technical claims (provider/flag/mode names) match the schema doc + architecture exactly. Tone is engineer-to-engineer. Em-dashes at human density (~6 in ~600 words, not LLM-saturation).
- `02-sysinternals.md`: **FAIL-WITH-FIXES.** Two specific issues:
  1. Line 21 contains literal placeholder `Hi {first name},`. If sent without substitution this is a publicly-embarrassing AI-tell. Either substitute the actual recipient's name before send, or change to `Hi Sysinternals team,`.
  2. The "Reach options" framing block (lines 5-10) is internal-process commentary that shouldn't go to the recipient. Only the "Subject"/"Body" portion below `---` should be sent.
- `03-process-hacker-issue.md`: PASS.
- `04-rsysadmin-post.md`: PASS.

**Primary-agent assessment:** Buddy is correct on both points. The `{first name}` placeholder was written assuming the user would substitute it; buddy correctly notes that an LLM-generated placeholder is exactly the kind of detail that makes a "fellow engineer wrote this" message read as AI-generated if it leaks. The "Reach options" preamble was written for the user as send-prep guidance, not as recipient body — buddy is correct that the file structure is ambiguous about that.

**Primary-agent action:**

- Will fix `02-sysinternals.md` mechanically: change `Hi {first name},` to `Hi Sysinternals team,` (the safer fallback — if user later identifies a specific contact, they can personalize at send time) and add a clear `## RECIPIENT-FACING BODY (everything above this section is internal send-prep)` divider so the "Reach options" preamble can't accidentally end up in a sent message.
- **HOLDING all outreach sends.** Per the user's instruction "If buddy agent raises concerns on any of the three: STOP the affected workstream and write up the disagreement in /audit/buddy-disagreements.md," the entire outreach workstream is paused, including the buddy-passing drafts `01`, `03`, `04`. The user authorizes the actual sends after returning.

**What the user needs to decide:**

- Authorize sends for `01` / `03` / `04` (all-PASS) — and where, since each has a different channel:
  - `01-perfview-issue.md` — can be filed via `gh issue create --repo microsoft/perfview` under the user's GitHub identity. Primary agent has technical access to do this if authorized.
  - `03-process-hacker-issue.md` — same pattern, `gh issue create --repo winsiderss/systeminformer`.
  - `04-rsysadmin-post.md` — Reddit. No CLI channel available. User must post manually.
- Review the `02-sysinternals.md` fix when the primary agent applies it; decide whether the recipient salutation should stay generic ("Sysinternals team") or be personalized to a specific contact the user has in mind.

### 1.3 Workstream (c) — Group A week 2 scope: PASS

**Buddy's verdict:** PASS. Buddy verified the proposed week 2 scope matches the architecture's "Phase 3 acceptance criteria → Group A — ETW foundation" (the Group A bullet list updated in PR #68) and the schema doc's "Group A weeks 2-7 implementation gates" section. No scope creep, no quiet-deletion. The four parser-level acceptance criteria (DPC 0x42/0x44/0x45, HardFault 0x20, DiskIo prefix-only, PerfInfo 0x32 no-op) are properly deferred to week 3+ when parsers exist, not silently dropped.

**Minor naming note from buddy:** The proposed plan refers to `crates/framesage-svc/`; the actual crate directory is `crates/service/` (binary name is `framesage-svc.exe`). Non-blocking; primary agent will use the correct path when implementing.

**Primary-agent action:**

Per the user's instruction "After rustfmt PR merges + outreach sent, START Group A week 2 implementation," Group A week 2 is HELD pending (a) and (b) resolution. The buddy passed the scope but the prerequisites haven't cleared.

**What the user needs to decide:**

Does Group A week 2 start get gated on the rustfmt-PR resolution, or can it proceed in parallel on a separate branch? The user's stated rationale for gating was "a red CI baseline poisons every signal Group A produces" — that argument is still active (CI will still be red on clippy even if the fmt-PR merges), so the conservative read is: Group A week 2 waits until CI is green or until the user explicitly accepts a known-red baseline.

### Resolution — 2026-05-17 (user jog-return)

User decisions on all three workstreams, recorded for the audit
trail:

**Workstream (a) — Rustfmt PR path: SPLIT into two PRs.**
- Reason given: bundling rustfmt with the seven clippy edits
  would turn "approve the formatter" into "review seven
  semantic edits inside a 20-file diff," which is the exact
  failure mode that produced the current baseline.
- Execution: PR #69 (fmt-only, mechanical) opened first, merged
  via auto-merge to commit `e097cbf` on main. Clippy follow-up
  shipped separately as `chore/clippy-baseline` — that PR will
  reference this Entry's clippy-fix discussion in its
  description.
- On the AtomicU64 specifically: user's reading was correct —
  the `const ZERO: AtomicU64 = AtomicU64::new(0);` pattern is
  *deliberately* `const` because `[ZERO; 256]` re-evaluates the
  initializer at each slot, producing 256 distinct atomics. A
  `static` won't compile (`AtomicU64: !Copy`). The clippy PR
  uses `#[allow(clippy::declare_interior_mutable_const)]` with
  an expanded comment, NOT the clippy-suggested
  `const`→`static` transform. The original code comment
  ("array repeat with non-Copy types needs the array_repeat
  workaround") was correct but cryptic; the expanded comment
  now explicitly answers "why didn't they just do what clippy
  said?" so future maintainers don't re-derive the analysis.

**Workstream (b) — Outreach authorization, partial:**
- `01-perfview-issue.md` (PASS) — AUTHORIZED, send via `gh
  issue create --repo microsoft/perfview` under the user's
  GitHub identity. Day-5 clock starts at send. Log timestamp
  in `spike/etw-edr-report.md` §10 results-log.
- `03-process-hacker-issue.md` (PASS) — AUTHORIZED, send via
  `gh issue create --repo winsiderss/systeminformer`. Same
  day-5 clock + §10 logging.
- `02-sysinternals.md` (FAIL-WITH-FIXES, now FIXED) — HOLDING.
  User wants to see the recipient-facing body before sending.
  Primary agent will surface the post-fix body in chat for
  user review. Send channel is the user's call (likely email
  to a Sysinternals contact, not public issue).
- `04-rsysadmin-post.md` (PASS) — user is posting this one
  manually via their own Reddit account. Not a primary-agent
  action. Reddit's mod/account-age gating makes
  `gh`-equivalent posting impractical, and an account-mismatched
  cross-post would look suspicious.

**Workstream (c) — Group A week 2 gating: WAIT for full CI green.**
- Rationale carried over from (a): half-green CI poisons Group
  A's regression signal. Clippy PR is small (7 edits, one
  file), so the wait is hours-to-a-day at most.
- Pre-work allowed during the wait:
  1. Draft the Group A week 2 implementation plan as a
     standalone document. Same format and rigor as the Phase 1
     spike report. Run that past buddy before execution.
  2. Run buddy on the clippy PR using the same three-question
     format that worked this round. STOP on any new buddy
     concern; do not shortcut just because confidence is high.

Primary-agent execution order going forward:
  1. Open fmt-only PR #69. ✓ DONE — merged via auto-merge.
  2. Read AtomicU64 use sites, form view, show diff to user
     before opening clippy PR. ✓ DONE — user confirmed in
     chat 2026-05-17.
  3. Apply strengthened `#[allow]` comment per user's
     drafted text, commit on `chore/clippy-baseline`. ✓ DONE.
  4. Open this docs PR (separate from clippy PR per user's
     "keep clippy PR isolated" rule).
  5. Open clippy PR with reference back to this docs PR.
  6. Run buddy on the clippy PR with the same three-question
     format.
  7. After clippy + docs PRs merge and buddy approves:
     - Send outreach 01 + 03 via `gh issue create`. Log §10.
     - Show user the 02 recipient-facing body for sign-off.
     - Draft Group A week 2 implementation plan; run past
       buddy.
  8. After clippy merges + buddy approves the implementation
     plan: START Group A week 2.

**Observation captured for the audit trail (user, 2026-05-17):**

> The AtomicU64 catch is the kind of thing that justifies the
> entire verification rhythm in one moment. Blind
> clippy-fix-application would have produced code that didn't
> compile, you'd have spent twenty minutes debugging it,
> possibly committed a wrong "fix" before realizing the
> compile error meant clippy was wrong. Instead the rhythm
> forced you to read the use sites first, formed the right
> view, and the wrong path got rejected before it cost any
> time. The 7-week Group A estimate has dozens of moments
> like this hiding in it. The discipline you're applying now
> is what keeps those moments cheap.

Logged here because future maintainers reviewing this audit
trail will benefit from the explicit articulation of why the
buddy rhythm exists, not just the mechanical fact that it
does.

---

## Entry 2 — 2026-05-17, buddy review of Group A week 2 plan DRAFT v3

### Buddy verdict summary

| Question | Verdict | Disposition |
|---|---|---|
| (a) Plan matches architecture + schema authority | PASS | No action |
| (b) Scope correctness | PASS | No action |
| (c) Realistic stop gates + feasibility | PASS-WITH-CONCERN (Day 3 overloaded, but stop gates adequate) | Flagged, no fix required |
| (d) Internal consistency **[NEW]** | FAIL | Three concrete inconsistencies + one secondary design question |

**Overall verdict: STOP-ON-(d).**

This was the first use of the four-question buddy format (introduced by user instruction 2026-05-17 after DRAFT v2's §2 directory-name slip went uncaught). The new (d) question — "does the plan agree with itself across sections?" — caught what the prior three would have missed. The format validated its own purpose on first deployment.

### 2.1 Buddy's three (d) findings — all real, all my agreement

**Finding 1: `EtwSession` / `EtwSubsystem` generic-parameter inconsistency between §3.2 and §3.4.**

§3.2 declares (lines 126-141):
```rust
pub struct EtwSession { /* opaque handle */ }
pub enum EtwSubsystem {
    Running(EtwSession),
    Disabled(DegradationMode),
}
pub fn start(opts: SessionOptions) -> Result<EtwSubsystem>;
```

§3.4 declares (lines 327-329):
```rust
pub struct EtwSession<S: EtwSysCalls = RealEtwSysCalls> { /* ... */ }
pub fn start(opts: SessionOptions) -> Result<EtwSubsystem<S>> { /* ... */ }
```

§3.4 implies `EtwSubsystem` is generic (`EtwSubsystem<S>`), but §3.2's enum has no type parameter.

My agreement: this is a real type-system inconsistency. The two sections cannot both be correct as written. The fix is mechanical-but-design-tinged — choose ONE form. Buddy proposes "generic-with-default and propagate to §3.2's enum + start() signature." That's defensible but it leaks the generic parameter into every public API consumer. The alternative (hide the trait indirection entirely behind `#[cfg(test)]` so production has no generic) is less surface-area but couples test infrastructure more tightly to the production code structure.

**Surface to user for design decision:** which form do you want?
- (A) Public generic-with-default: `EtwSession<S: EtwSysCalls = RealEtwSysCalls>`, `EtwSubsystem<S>`. Default makes production callers ignore the param; tests pass `MockEtwSysCalls` explicitly. Buddy's pick.
- (B) `#[cfg(test)]`-only generic: production `EtwSession` is a concrete non-generic type wrapping `RealEtwSysCalls` directly; test code uses a parallel `TestableEtwSession<S>` type that production never sees. Smaller blast radius on the public API; bigger blast radius on the test/production code-share.
- (C) `Box<dyn EtwSysCalls>` field inside `EtwSession`. Smallest API surface (no generic anywhere), but dyn dispatch on the ETW callback hot path. Rejected in DRAFT v3 §3.4 explicitly; if this is what you want, the rejection needs to come out and the hot-path-cost analysis needs revisiting.

**Finding 2: §4 Day 3 stop gates (4 listed) vs §6 Day 3 restatement (only 1 mentioned).**

§4 Day 3 lists FOUR stop gates: formatter conflict, abstraction wrong shape, `cargo asm` codegen-parity fail, user rejects architecture amendment. §6 — explicitly titled "Stop gates within the week (cumulative)" — restates only one Day-3 stop ("if the architecture's intended INFO log line conflicts with the actual formatter"). The other three Day-3 stops silently disappear from §6's checklist.

My agreement: this is a real omission. §6 is supposed to be the single-pane-of-glass restatement. Fix is mechanical: copy the other three Day-3 stop bullets into §6 verbatim. No design decision required.

**Finding 3: §3.5 #5 says supervisor is "created on Day 5" but §4 Day 3 says it scaffolds the "supervisor-side select-loop pattern."**

§3.5 #2 says "the receiver is owned by the consumer-supervisor task in `crates/service/` (created on Day 5)."

§4 Day 3 says "Day 3 ALSO scaffolds the consumer-thread panic-channel mechanism per §3.5: ... and the supervisor-side select-loop pattern."

Both can't be true. Either the supervisor is scaffolded on Day 3 and finished on Day 5, or it's created on Day 5.

My agreement: the plan is ambiguous. Fix is mechanical — pick the timing and propagate. My read: the supervisor's *select-loop pattern* (the structure: a tokio task that awaits the oneshot + drop-rate interval) is scaffolded on Day 3 alongside the channel types, but the supervisor as a *task instance running in the service crate* is created on Day 5 when the service-wiring code lands. Rewording: §4 Day 3 should say "lays the consumer-side primitives and a single-file sketch of the supervisor's select loop in the etw crate; the service-crate task-spawn happens on Day 5." But this is my interpretation, not a determination — needs user confirmation.

### 2.2 Buddy's secondary (cross-cutting) finding — design question requires user decision

> "§3.5 #3 and §4 Day 4 Mode 5 say the supervisor 'emits `DegradationMode::ConsumerPanic` into the existing `SystemEvent` channel' — but `SystemEvent` in `crates/engine/src/lib.rs` line 79 is a Copy enum with only OS-level variants (Suspend, Resume, SessionConsoleConnect/Disconnect, SessionLock/Unlock). It carries no `DegradationMode`-shaped payload. The plan implies a wire that doesn't exist; either a new variant lands (scope creep) or a separate channel is needed."

I verified by grepping `crates/engine/src/lib.rs`: `SystemEvent` is indeed a `Copy enum` with OS-level variants only. The plan implies wiring `DegradationMode::ConsumerPanic` into it, which doesn't fit.

This is a **real design question**, not a mechanical fix. Three resolution paths:

- (A) **Extend `SystemEvent` with an `EtwDegradation(DegradationMode)` variant.** Drops `Copy` (because `DegradationMode::BuildUnsupported { detected_build: Option<u32> }` isn't `Copy` — well, `Option<u32>` IS Copy actually, so the enum *could* stay Copy. Verify.). Adds one variant. Engine consumers learn one new pattern.
- (B) **New separate channel for ETW degradation events.** `tokio::sync::mpsc::UnboundedSender<DegradationEvent>` parallel to the existing `SystemEvent` channel. Cleaner separation (OS events ≠ subsystem degradations), but doubles the channel surface the engine listens on.
- (C) **Punt to Group C.** Week 2's supervisor logs the panic at ERROR level + emits to tracing, but does NOT wire into any engine channel yet. Group C adds the wire when the UI banner needs to consume it. Week 2's Mode 5 test asserts on the tracing-emitted ERROR-level log instead of on a channel send.

Buddy didn't pick one — appropriately, since this is a design question, not a mechanical fix.

**My recommendation:** **(C)** — Group C-deferred wiring. Rationale: week 2's stated scope is "no closed-loop signal is produced yet — the lifecycle is operational." The UI banner that consumes the panic event doesn't exist until Group C. Wiring the event into a channel that has no consumer (until Group C) creates a dangling-edge that invites future-wiring drift. (C) keeps week 2 honest: the panic IS logged, the test DOES assert on something concrete (the tracing event), and the channel wire materializes when it's actually needed.

**(A) is the second-best option** if the user wants the channel landed in week 2 anyway (e.g. to validate the wire works before Group C). The `Copy` question: `DegradationMode::BuildUnsupported { detected_build: Option<u32> }` IS Copy because `Option<Copy>` is Copy. So adding `EtwDegradation(DegradationMode)` doesn't drop the trait. Verified.

**(B) is the least-attractive option** — duplicating channel surface when one channel + one variant suffices feels like premature decoupling.

### 2.3 What I am NOT doing unilaterally

Per the standing rule "Do not resolve disagreements unilaterally — that's what I'm for when I'm back," I am NOT:

- Modifying the plan document to apply any of the four findings without user direction. The plan stays at DRAFT v3 as buddy reviewed it.
- Picking among (A) / (B) / (C) for finding #1 (generic shape) or for the secondary finding (channel shape) on my own.
- Re-running buddy on a v4 that I drafted by guessing your preferences.

What I AM doing:
- Surfacing the four findings here with my agreement and my recommendation where I have one.
- Listing the design choices for each so you can decide quickly.
- Holding execution on Day 1 indefinitely until DRAFT v4 (with your decisions applied) re-passes buddy with the four-question format.

### 2.4 Summary of decisions required from user

| # | Finding | Type | Options | My recommendation |
|---|---|---|---|---|
| d.1 | `EtwSession`/`EtwSubsystem` generic shape inconsistency between §3.2 and §3.4 | Design | (A) public generic-with-default / (B) `#[cfg(test)]`-only generic / (C) `Box<dyn>` (rejected in v3, would need un-rejection) | (A) — cleanest. Buddy's pick too. |
| d.2 | §4 Day 3 has 4 stops; §6 restatement has 1 | Mechanical | Copy the missing 3 bullets into §6 | (the mechanical fix) — no design question |
| d.3 | Supervisor "created on Day 5" vs "scaffolded on Day 3" | Mostly mechanical with one clarification | Reword to: Day 3 scaffolds the select-loop *pattern* in the etw crate; Day 5 spawns the supervisor *task instance* in the service crate | (the clarified split) — confirm? |
| Sec | `SystemEvent` channel doesn't carry `DegradationMode` payload | Design | (A) extend SystemEvent enum / (B) new parallel channel / (C) Group C-deferred wiring | (C) — keeps week 2's stated scope honest |

When you decide on each, DRAFT v4 applies the chosen resolutions, runs through buddy again with the four-question format, and (assuming PROCEED) Day 1 starts.

---

## Entry 3 — 2026-05-17, buddy review of Group A week 2 plan DRAFT v4

### Buddy verdict summary

| Question | Verdict | Notes |
|---|---|---|
| (a) Plan matches architecture + schema authority | PASS | No findings |
| (b) Scope correctness | PASS | No findings |
| (c) Realistic stop gates + feasibility | PASS-WITH-NOTE | Day 3 carries six artifacts; buddy assesses as "~1.5 days of work" but explicitly NOT blocking — risk is acknowledged in plan §9 risk #3 and Day 3 stop gates fire on overload |
| (d) Internal consistency | PASS-WITH-MINOR-NOTES | Two micro-findings; see §3.1 below |

**Overall verdict: PROCEED.**

But: this triggers the user's process flag from the v3→v4 instructions:

> "If v4 surfaces same-category issues, STOP and surface to me. Do not iterate to v5 without surfacing. The right intervention may be pair-writing the plan with buddy from scratch rather than continuing to review finished drafts, but that's a decision I need to make, not a default the auditor reaches for."

The v4 findings are in the same category as v3's — both rounds had their substantive issues land in (d) internal consistency. Per the literal flag, primary agent is STOPPING and surfacing this entry to the user rather than iterating to v5 unilaterally (even though buddy recommended PROCEED with inline fold-in).

### 3.1 Buddy's two (d) micro-findings

Direct quotes from buddy:

**Finding 1 (prose-vs-type-block slip):**
> "Line 596 (§4 Day 3 deliverable prose) writes `Result<EtwSubsystem>` (no generic param) while §3.2 line 173 declares `Result<EtwSubsystem<S>>`. This is the same class of slip v3's d.1 surfaced; v4 fixed the type-block but left a prose shorthand uncorrected. The parameter has a default so it's *technically* readable, but the v4-fix narrative on line 598 explicitly notes the type 'is now consistent with §3.2 per v4 fix d.1' — and then 2 lines earlier writes the bare form. Microscopic."

**Finding 2 (undeclared helper method):**
> "Line 655 (§4 Day 5) calls `session.into_supervisable_parts()` to extract `(consumer_join, exit_rx)` — this method is never declared in §3.2's `EtwSession` public surface (lines 155-195). It's load-bearing for Day 5 wiring; §3.2 should declare it or §3.6 should specify it."

Primary agent's read: both are real. Finding 1 is a copy-edit slip — `Result<EtwSubsystem>` reads fine because the generic has a default, but the prose-vs-type-block disagreement is the exact class of error the (d) check is for. Finding 2 is closer to a real spec gap than a consistency slip — Day 5's wiring code calls a method that doesn't exist in the type's documented surface. The plan needs to either declare `into_supervisable_parts()` in §3.2's `impl` block or specify it in §3.6 alongside `SupervisorLoop`'s consumption pattern.

### 3.2 Buddy's category-of-issue observation (verbatim)

This is the part that bears most on the user's process-flag decision:

> "**Same category as v3 — but the magnitude and severity dropped roughly an order of magnitude.** v3 had four substantive (d) findings: a mismatched type signature between sections, a stop-gate count mismatch, a Day 3/Day 5 supervisor-lifecycle ambiguity, and a missing `DegradationMode` payload path. v4 has two micro-(d): a single inconsistent prose form in Day 3's deliverable description (`Result<EtwSubsystem>` vs `Result<EtwSubsystem<S>>`), and an undeclared helper method `into_supervisable_parts()` in §3.2.
>
> **My read for the user's process decision:** this is NOT structural-rhythm-failure territory. The pattern of v3 findings (substantive type-level disagreements, count mismatches, lifecycle ambiguities) is meaningfully different from v4's residue (one prose shorthand, one missing API declaration in a code sample). The author internalized (d) — they killed every v3 issue and 90% of the new surface they added. The remaining slips are the kind that any human plan-writer leaves: prose drift in a 950-line document, and a helper method that surfaces only when you mentally compile the Day 5 code sample.
>
> **Recommendation:** sign off on v4 with a one-line correction request — declare `into_supervisable_parts()` in §3.2 (or §3.6) and tighten line 596's prose to match §3.2's signature. Don't iterate to v5 over only these two; either fold them into a v4.1 patch-commit on the same branch, or push them as Day 3 in-flight corrections."

### 3.3 User's decision required

Three paths I see; user picks one (or proposes Other):

| Option | Action | When Day 1 starts |
|---|---|---|
| **α (buddy-recommended)** | Apply the two micro-fixes inline as a v4.1 patch-commit on the same branch. Tiny buddy re-confirmation OK if you want it, otherwise flip header to APPROVED. | Same day |
| **β (literal-flag intervention)** | Pair-writing intervention. User co-writes the plan with buddy from scratch rather than continuing finished-draft reviews. Discards v4 in favor of a fresh-write process. | After pair-writing complete |
| **γ (accept as-is)** | Flip v4 to APPROVED with no further edits. The two micro-findings get folded in during Day 3 execution as in-flight corrections (the engineer notices them when mentally compiling the Day 5 code sample and patches §3.2 accordingly). | Same day, with note that v4 ships with two known minor (d) gaps |

**Primary agent's recommendation: option α.** The fixes are mechanical (declare the method, tighten the prose) and surface in well-bounded locations. Pair-writing intervention (β) feels heavier than what the v4 evidence justifies — buddy's "90% reduction" framing is hard to argue with. Option γ leaves a footgun for the Day 3 engineer who may NOT mentally compile the Day 5 code sample and may instead just copy the (broken) snippet.

### 3.4 What primary agent is NOT doing unilaterally

Per the standing rule "Do not resolve disagreements unilaterally — that's what I'm for when I'm back," and the explicit v3→v4 instruction that the same-category trigger is a user-decision moment:

- NOT applying the two micro-fixes to v4. Plan stays at the commit you saw (`9610b9a`).
- NOT picking among α/β/γ on my own.
- NOT flipping the header to APPROVED.
- NOT starting Day 1.
- NOT running buddy on a v4.1 that I drafted by guessing your preference.

What I AM doing:
- Surfacing Entry 3 here with buddy's full verdict, buddy's category observation, and my recommendation.
- Holding execution until you choose.

---

## Entry 4 — 2026-05-17, buddy review of implementation-phase format DRAFT v1 + self-pass lesson

### Context

Lands with the PR #75 patch sequence (the implementation-phase
format doc had been on `plan/implementation-phase-buddy-format` in
DRAFT since 2026-05-17, deferred behind PR #73's strict-sequencing
prerequisite). Numbering: Entry 4 here, Entry 5 already exists on
the rebased branch — non-sequential numbering reflects when entries
were COMMITTED on each branch, not when they were chronologically
written. v4.2 amendment Self-pass Finding S-C already documented
this gap and instructed cross-references to use the phrasing "the
Entry that lands with PR #75 patches."

### Buddy verdict on DRAFT v1 (2026-05-17, planning-phase 4Q)

| Question | Verdict | Notes |
|---|---|---|
| (a) Authority match | PASS | Five questions cover the user's four stated topics (FFI/concurrency/errors/tests); when-to-use gate sensibly scoped |
| (b) Scope correctness | PASS | Five is right size; (5) consistency is genuinely distinct from planning (d); process surface justified |
| (c) Realistic feasibility | PASS | Estimated 30-60 min per typical week-EOD PR; explicit enumeration mitigates hand-waving |
| (d) Internal consistency | **FAIL** | Two cross-reference findings (see below) |

**Overall: STOP-ON-(d).**

### Buddy's two (d) findings

1. **Cross-branch dependency unresolved.** Line 4 of
   `audit/buddy-format-implementation-phase.md` cited Entries 2 & 3
   of `audit/buddy-disagreements.md`, but those entries existed only
   on `plan/group-a-week-2` HEAD. On the format-doc's own branch
   (`plan/implementation-phase-buddy-format`), the file contained only
   Entry 1 at the time of buddy review.

2. **§2.4 mis-attribution.** The self-pass at the bottom of the
   format doc claimed `(§3.2, §3.4, §3.5, §3.6, §12, §6, §2.4) all
   point at sections that exist on plan/group-a-week-2 HEAD`. Five of
   six did; §2.4 did NOT. §2.4 lives in `audit/v0.7-architecture.md`,
   not in `spike/group-a-week-2-plan.md`. The honesty-contract
   reference at line 72 was correctly to the architecture doc; only
   the self-pass bundled it wrong.

### Resolution applied via PR #75 patches (this entry's surfacing)

**User decision (2026-05-17): Option α** — apply the two fixes inline
on the format-doc branch, with the Entry 4 split (per user's
"closed-loop" instruction surfaced via v4.2 Self-pass S-C: capture the
process lesson here in a dedicated entry rather than letting it drift
into the format doc's self-pass section).

**Patch (1):** restructured the "Authoritative inputs" section to
point at the buddy-disagreements log as a whole (with the explanation
of the planning-phase format evolution captured in the cite text)
rather than enumerating specific entries. Removes the cross-branch
dependency; the doc reads accurately in isolation regardless of which
branch's `buddy-disagreements.md` the reader has checked out.

**Patch (2):** fixed the self-pass §2.4 attribution by splitting the
section-existence assertion into "week 2 plan refs (§3.2, §3.4,
§3.5, §3.6, §6, §12) → plan doc on main" and "§2.4 → architecture
doc on main, NOT the plan."

**Process improvements landed alongside the format-doc patches:**

- **META 1 (sweep discipline):** when a (d) check finds an issue
  class at site X, sweep exhaustively across the document for the
  same class. Now in standing instructions. On-record example: Day 3
  Self-pass A initially treated `pub(crate)`-vs-`tests/`-directory as
  build_gate-specific; sweep caught the same pattern in
  degradation_tests and supervisor_tests.

- **META 2 (5Q (1) broadening):** rename from "FFI safety" to
  "Language-mechanics safety" with sub-bullets — `unsafe` SAFETY
  justifications + visibility-vs-consumer-location + integration tests
  in `tests/` can only see `pub` items + trait bounds match
  implementers + Send/Sync where threading is involved. Closes the
  failure mode where Day 3 visibility/Sync gotchas escaped three
  rounds of planning-phase review because no format question named
  them explicitly.

- **§X dependency arrows:** `audit/buddy-format-implementation-phase.md`
  §X "Cross-document consistency dependencies" formalizes the extended
  (d) at the doc level. Implementation-phase (5) inherits the same
  cross-document scope.

### Self-pass lesson (the on-record process improvement)

The v4.2 self-pass with the extended (d) caught its own consistency
violation on first round of use (S-B handoff-cross-branch + S-A
visibility/Sync + S-D sweep-discipline). The extension is doing real
work, not adding ceremony. The lesson:

**Self-passes that make positive claims must include the literal
verification command + its output.** Examples:

- "Section X exists at line Y" → `sed -n 'Yp' path/to/file.md` (output captured inline).
- "File Z exists on this branch" → `ls path/to/Z 2>&1` (output captured inline).
- "Cross-doc reference resolves" → `grep -n "expected text" referenced/path.md` (output captured inline).

A self-pass that just says "checked, looks fine" is not a self-pass.
This rule extends `spike/etw-edr-report.md` §8's literal-output ground
rule to documentation self-checks: machine-state claims (test passes,
build succeeds, file exists at path X with content Y) require verifying
commands and their literal output, regardless of whether the artifact
is code or prose.

### What primary agent is NOT doing unilaterally

- NOT flipping the format doc to APPROVED without buddy 4Q on the
  patched version (the meta-rhythm: the document defining the rhythm
  follows the rhythm).
- NOT extending META 2's broadening beyond what was specifically
  decided alongside v4.2 (visibility / trait-bound / Send-Sync
  sub-bullets — nothing else).
- NOT modifying the planning-phase 4Q format's wording at the same
  time — the extended (d) is inherited via standing instructions and
  via §X in the implementation-phase doc; a separate planning-phase
  format doc doesn't exist (it lives inline in standing instructions),
  so no edit is required.

### Resolution — 2026-05-18 (buddy 4Q on DRAFT v2; v2.1 patch applied)

Buddy ran planning-phase 4Q (with extended (d) cross-doc) on DRAFT v2
of the format doc. Verdict: STOP-ON-(d) with one mechanical finding —
the META 2 Q(1) rename ("FFI safety" → "Language-mechanics safety")
was applied at the section heading + intro but not swept to two
downstream sites in the same document:

1. **Line 124** — the output-format template buddy reviewers
   literally copy into PR reviews: `## (1) FFI safety: PASS|FAIL`.
   Load-bearing because broken propagation = every buddy review
   reverts the rename via copy-paste.
2. **Line 182** — the self-pass meta-section inside this same doc:
   `**(1) FFI safety** — N/A; this is a process doc, no unsafe.`
   Marginal — internal consistency only — but the fix is the same
   two-line edit so doing it together is appropriate.

Watch-signal evaluation per the week-2 plan footer:
- (a) count similar/growing across drafts: DECREASED (v1: 2 → v2: 1).
- (b) severity similar/growing: DECREASED. v1's findings were
  cross-document attribution slips; v2's is a within-document sweep
  miss on a literal rename.
- (c) self-pass empty while buddy still finds issues: NOT empty —
  self-pass made substantive claims that Patch (2) refined. Note: the
  self-pass at line 182 is itself one of the sites that needed the
  rename; degenerate case where the self-pass that should catch the
  sweep miss is also a site of the sweep miss.

**Pattern: CONVERGENCE.** No structural-intervention trigger. Per
buddy's own recommendation ("apply the two-line fix as a v2.1
patch-commit on the same branch, no re-buddy required") and the
2026-05-17 cutoff-the-knot rule, applied as v2.1 patch
(`audit/buddy-format-implementation-phase.md` lines 124 + 182
renamed to "Language-mechanics safety").

**Meta-observation (per buddy + user-noted as audit-trail-load-bearing
in Entry 5 lines 628–639):** the document introducing sweep
discipline failed its own sweep on its first edit. Specifically: the
META 2 broadening commit added §X formalizing sweep discipline, but
the commit itself missed two sites of the same rename it introduced.
This is the rhythm catching its own introducer mid-introduction —
not structural failure, but a clean on-record example of why
self-passes need verification commands + literal output (the
verification command `grep -n "FFI safety" audit/buddy-format-implementation-phase.md`
would have output the two missed lines instantly; the self-pass
without that command read fine and approved itself). Rhythm-
introduction commits ARE the highest-risk sites for the rhythm
they introduce — the author is still building the muscle the rhythm
requires. Documented for future rhythm-introduction commits to plan
extra verification-command sweep on the day of introduction.

Format doc state: DRAFT v2 → v2.1 (this commit). APPROVED flip +
PR #75 merge land as follow-up commits per the user's strict-
sequencing intent.

---

## Entry 5 — 2026-05-17, fresh-eyes re-read of APPROVED week-2 plan surfaced two real gaps + one minor

### Context

Per the session-handoff document merged via PR #76 (resolves once
PR #73 merges to main); §9 line 4 reads: **"Do NOT auto-merge PR
#73 without reviewing the APPROVED plan content first."** The
fresh session re-read `spike/group-a-week-2-plan.md` on
`plan/group-a-week-2` HEAD (`5ca012d`) against a six-bullet
checklist. Two of the four explicit checks (bullets 5 + 6)
surfaced real gaps requiring pre-merge resolution; bullet 3
surfaced a minor inline-fixable gap; bullet 1 passed with notes
(consistent with plan §9 risk #1).

Why the entry exists despite plan already being APPROVED: the
iterative v1→v2→v3→v4→v4.1 process smoothed concerns as they
surfaced; a fresh-eyes pass on the integrated whole catches what
iteration absorbed. The handoff §9 directive is the institutional
recognition of this asymmetry.

### Finding 1 — Day 1 build_gate.rs lacked an injection-seam spec (bullet 5)

Plan §3.1 specified `build_gate.rs`'s public surface but no
injection mechanism. Plan §4 Day 1 unit-test deliverables required
mocking both the build number (predicate-true at synthetic ≥26100)
and a `RtlGetVersion` failure path (predicate-false with
`detected_build() == None`). Plan §3.4's `EtwSysCalls` trait —
which DOES provide `rtl_get_version` — scaffolds on **Day 3**, not
Day 1. Day 1 had a chicken-and-egg the plan didn't resolve.

**User decision (2026-05-17): Option A.** Spec a build_gate-local
`#[cfg(test)]` injection seam: `thread_local!<RefCell<Option<Result<u32, ()>>>>`
override + `pub(crate) set_build_override()` helper. Production
path unchanged in release builds; test parallelism safe via
`thread_local`. §3.1 gains the spec paragraph; §3.4 gains a
one-line note documenting that build_gate uses its own injection
independent of `EtwSysCalls` (two patterns intentional for two
different scopes).

Applied in v4.2 amendment.

### Finding 2 — architecture §2.1 "Survives service restarts" unsatisfied (bullet 6)

Architecture §2.1 line 120 + acceptance criteria checklist line 1585
("Service restart leaves no stale session") together require:
startup `cleanup_stale_session()` + graceful shutdown + crash-leak
caught on next start. Plan §5 test inventory had no test for this;
§4 Day 5 EOD step 4 covered SHUTDOWN (orderly SCM stop), not
RESTART-AFTER-CRASH (the kill-then-cleanup path). §7 acceptance
criteria silent. §8 out-of-scope silent. The behavior is presumably
preserved from the spike code (lifted in Day 2's session.rs) but
the plan never asserted it.

**User decision (2026-05-17): Option B with structural note.**
§4 Day 5 EOD gains step 6: install, start, force-kill (not orderly
SCM stop — use `Stop-Process -Force`), capture `logman query`
immediately to confirm the session leaked, restart via SCM, capture
`logman query` again to confirm `cleanup_stale_session()` ran AND
a fresh session opened. Four literal `logman query` outputs total.
§12.4 mirrors as step 7. §7 acceptance criteria gains the
corresponding checkbox.

**STOP-gate structural addition (per user instruction):** if step 6
shows the leaked session was not cleaned up on restart, this is
NOT a "noted, will fix later" — Day 2's session.rs lift is
incomplete and architecture §2.1's "survives service restarts"
criterion is unsatisfied. Week 2 cannot mark complete until the
lift preserves the spike's `cleanup_stale_session()` behavior.
Plan §4 Day 5 explicitly states this STOP condition.

Applied in v4.2 amendment.

### Finding 3 — §12.4 step 6 (asm-codegen-parity) lacked extraction step (bullet 3)

Plan §4 Day 3 specified the `cargo rustc --emit=asm` capture
command but §12.4 step 6 didn't specify how to extract the relevant
function body from the resulting 100s-of-KB `.s` file. Failure mode:
EOD-report writer's interpretation ranges from "paste 200 KB" to
"paste only the relevant function." Acceptance criterion was prose
("zero production cost") without a visually-checkable concrete
form.

**User decision (2026-05-17): inline patch.** §12.4 step 6
specifies the `cargo rustc --emit=asm` command + a grep extraction
step (either `cargo asm` subcommand OR `awk` extraction by demangled
symbol prefix) + the visually-verifiable acceptance criterion ("no
`call` instructions through vtable, no indirect `jmp` through
register loaded from memory") + the literal-output formatting
requirement (extracted function body only, in ```text``` fences,
with demangled symbol name + source-line annotations preserved).

Applied in v4.2 amendment.

### Process observation (applied per user decision — option α)

The planning-phase 4Q question (d) is extended to include
cross-document consistency. (d) becomes: "does this artifact agree
with itself **AND with the authoritative documents it references**?"

Dependency arrows (define once, reference forever):
- Plan documents (e.g., `group-a-week-2-plan.md`) must agree with:
  `audit/v0.7-architecture.md`, `spike/etw-schemas.md`,
  `spike/etw-edr-report.md`.
- Implementation-phase format must agree with: relevant plan +
  architecture.
- Spike reports must agree with: prior architecture + prior spike
  findings if referenced.
- Architecture amendment PRs must agree with: the architecture
  sections they amend + any plan that references those sections.

Captured in `audit/buddy-format-implementation-phase.md` §X (lands
as part of PR #75 patches). Implementation-phase format inherits
the extended (d) when PR #75 patches land. Finding 1 (plan-internal:
§3.1 vs §4 Day 1) would have been caught by current-(d);
Finding 2 (plan-vs-architecture: §5/§7 silent vs architecture line
1585) is exactly the cross-document case the extension catches.

The buddy run on the v4.2 amendment uses the extended (d) — first
exercise of the extension on itself.

**Meta-poetic confirmation (per user instruction):** the v4.2
self-pass with the extended (d) immediately caught one of its own
consistency violations — Self-pass Finding S-B surfaced that v4.2's
amendments referenced the session-handoff document via a name
(`handoff-2026-05-17.md`) that doesn't resolve on this branch,
because the handoff merged via PR #76 AFTER `plan/group-a-week-2`'s
HEAD. The extension caught the failure on first round of use; rephrasing
applied (S-B1 — inline-quote the directive + acknowledge cross-branch
state). This is the extension doing real work, not adding ceremony.
A consistency check that fails on its own introduction is the most
direct possible evidence that the check tracks something real about
the artifact's correctness.

**Sweep discipline added to self-pass technique (per user decision,
META 1):** when a (d) check finds an issue class at site X, the
same class must be checked exhaustively across the document, not
patched at the single site where it surfaced. Standing instructions
inherit this from Entry 5.

On-record example: Self-pass A initially treated the `pub(crate)`
vs `tests/` directory visibility mismatch as build_gate-specific.
Second self-pass (with sweep-discipline applied retroactively)
surfaced Finding S-D — same root cause, same bug, two more test
files (`degradation_tests.rs` + `supervisor_tests.rs` both use
`#[cfg(test)]` items from the lib via integration-test paths that
can't see them). Without the sweep, Day 3 + Day 4 would have hit
compile errors. With it, the v4.2 amendment closes all three sites
in one round.

Sweep-discipline rule of thumb: when a (d) finding describes a
mechanism (e.g., visibility-rules-vs-test-location, cross-branch
cite resolution, type-signature-prose-drift), the next step is
`grep` / cross-section read to enumerate every site the mechanism
could apply, NOT a single-site patch.

**Implementation-phase 5Q question (1) broadens (per user decision,
META 2):** rename from "FFI safety" to "Language-mechanics safety"
with sub-bullets — `unsafe` blocks need SAFETY justifications;
visibility modifiers match consumer location (`pub` vs `pub(crate)`
vs `#[cfg(test)]`); integration tests in `tests/` can only see
`pub` items (NOT `#[cfg(test)]` or `pub(crate)` items); trait
bounds match what implementers can satisfy; `Send`/`Sync` where
threading is involved. Lands as part of the PR #75 patch sequence
(after PR #73 merges + `plan/implementation-phase-buddy-format`
rebases). The on-record motivation is the same as META 1: visibility
mismatches are mechanically detectable but slipped past three rounds
of planning-phase (d) review because the format didn't name them
explicitly.

**Process-observation summary (Entry 5 captures three concurrent
process improvements):**

1. **Extended (d)** — cross-document consistency added to
   planning-phase 4Q (d) and implementation-phase 5Q (5).
   Dependency arrows captured in
   `audit/buddy-format-implementation-phase.md` §X (lands with PR
   #75 patches).
2. **Sweep discipline** — when (d) finds an issue class at site X,
   sweep exhaustively across the document for the same class.
   Standing instructions inherit from this entry.
3. **Impl-phase (1) broadens** — Language-mechanics safety with
   visibility / trait-bound / `Send`/`Sync` sub-bullets. Lands with
   PR #75 patches.

Three improvements in one entry because they came from one chain of
self-pass findings, and the rhythm-cost of a separate entry per
improvement is higher than the audit-trail value of separating them.

### What primary agent is NOT doing unilaterally

- NOT picking among the resolution options on my own — user picked
  A for Finding 1, B with structural note for Finding 2, inline
  for Finding 3.
- NOT merging PR #73 until buddy reviews the v4.2 amendment with
  the extended (d).
- NOT starting PR #75 patching work (downstream of PR #73 merge
  per strict-sequencing decision).

### Lesson for future self-passes (cross-references the Entry that lands with PR #75 patches; references resolve once PR #75 merges following PR #73 per the strict-sequencing decision in §sequencing below)

This Entry's findings include one that the v4.1 self-pass should
have caught (Finding 1 is plan-internal between §3.1 and §4 Day 1)
and one that no version of (d) up to v4.1 could have caught (Finding 2
is cross-document, which is exactly why the (d) extension is being
added in v4.2). Future self-passes per the new standard documented
in the Entry that lands with PR #75 patches must include explicit
verification commands AND their literal output for every positive
claim — running `grep`/`git show` to verify that referenced sections,
type names, file paths actually exist in their claimed locations.
The (d) extension makes this a structural requirement, not an
aspiration.

### Sequencing

Strict-sequencing decision per user (2026-05-17): PR #73 merges
first (v4.2 amendment + this Entry 5 land via PR #73 since they
live on `plan/group-a-week-2`); then `plan/implementation-phase-buddy-format`
rebases onto new main, picking up Entries 2–5; then PR #75 applies
its format-doc patches + adds Entry 4 + adds §X to the format doc;
then buddy + merge. Outcome on main: Entries 1, 2, 3, 4, 5 in
numerical order, format-doc §X dependency arrows present, all
references resolve.

---

## PR #119 — 2026-05-18 — verdict GO — defects: none

**Source:** Buddy review of `feat/w1.6-sessions-tab-onboarding-page3` (PR #119). Closes F-001 + F-002 from `audit/2026-revision-phase2-findings.md`; Phase 3 roadmap item W1.6, Option B (M effort, scaffold-only, no recording/list/detail).

**Format:** 5Q implementation-phase per `audit/buddy-format-implementation-phase.md`. Buddy held back final verdict pending three targeted clarifications; all three were verified in the working tree before GO was issued.

### Q1 — Forward-compat on the new IPC field

Q: Is `#[serde(default)]` present on `StatusSnapshot.closed_loop_build_supported: bool`?

A: **YES.** `crates/ipc/src/lib.rs:451` carries `#[serde(default)]` directly above the field at `:452`. Forward-compat:
- Old tray + new service → serde drops the unknown field; old tray ignores.
- New tray + old service → serde defaults to `false` → tray renders the unsupported-build empty state. Conservative fallback (the architecture's preferred failure mode — "don't claim closed-loop works if we can't confirm the host supports it").

### Q2 — VERBATIM constant usage on render paths

Q: Are the load-bearing const strings passed directly to `RichText::new(CONST)`, or is there any `format!()` / `concat()` / interpolation that would pass const-level substring tests but inject extra text into the rendered UI?

A: **VERBATIM throughout.** Grep across all 5 load-bearing constants:

| Constant | Render-site (passed VERBATIM to `RichText::new(...)`) |
|---|---|
| `UNSUPPORTED_BUILD_HEADING` | `crates/tray/src/tabs/sessions.rs:126` |
| `UNSUPPORTED_BUILD_BODY` | `crates/tray/src/tabs/sessions.rs:133` |
| `NO_SESSIONS_BODY` | `crates/tray/src/tabs/sessions.rs:181` |
| `NO_SESSIONS_PRIVACY_FOOTER` | `crates/tray/src/tabs/sessions.rs:219` |
| `CLOSED_LOOP_PAGE_EDR_DISCLOSURE` | `crates/tray/src/onboarding.rs:518` |

The sole `format!()` use referencing these constants is `sessions.rs:262` inside the `unsupported_build_does_not_contain_no_sessions_framing` test — concatenating heading + body to check the negative-half substring constraint. Test-only; not on a render path. Drift between const-level test guarantee and rendered output is therefore impossible without explicitly editing the render call sites.

### Q3 — Continue-button gating: tested or .expect()-loud-panic?

Q: Is the page-2 / page-3 Continue-button gating test-covered, or does it rely on the `.expect()`-loud-panic regression-trip pattern?

A: **`.expect()`-loud-panic pattern (acceptable per project style; explicitly confirmed).**

- Page 2 gate: `onboarding.rs:362` — `let continue_enabled = state.choice.is_some();` + `add_enabled(continue_enabled, ...)` + "Pick a level first." muted label when disabled.
- Page 3 gate (W1.6 new): `onboarding.rs:553` — identical shape against `state.closed_loop_choice.is_some()` + "Pick one to continue." muted label.
- Invariant pin at Finish: `:260` + `:263` `.expect("Finish click requires a selection on Page <N>")` — regression in gating surfaces as a loud panic when Finish is clicked without a selection.

No egui-render integration test exercises the gating. Consistent with the pre-W1.6 wizard's test coverage (`aggression_*` tests cover `apply_choice_to_policy` mutation only, not rendering). Full booted-UI integration test deferred to M3.1 / #110 per the W1.6 scaffold-scope contract.

### Verdict + merge

All three clarifications confirm the implementation matches spec. **GO.** Merging PR #119. F-001 + F-002 close on merge.

**No defects fanget.** No CONFLICT-WITH-AUDIT-POSITION entries needed (the Option α resolution was pre-approved before code was written; logged in the PR body for traceability).

---

## PR #120 — 2026-05-18 — verdict GO — review-surface: fresh-eyes diff-description review via separate Claude instance (mobile), CHANGES_REQUESTED on 3 items, addressed in amend before merge

**Source:** Implementation of W1.2 (#81) — remove `bf6.exe` → Javelin AC-detection false-positive marker. Closes finding A-003 (HIGH) per `audit/2026-revision-phase2-findings.md`. Phase 3 roadmap item W1.2, S effort.

### Review-surface honesty (P4.3 — explicitly NOT full buddy-review)

**What was actually checked:**
- **Pre-coding review of the diff-PLAN** (not yet-written code): user surfaced 3 targeted adjustments to the planned approach (1: split tests rather than rename; 2: explanatory comment + cleaner test naming; 3: pre-coding META 1 `git grep -i bf6 -- '*.rs'` sweep to confirm scope containment). All 3 incorporated before coding began.
- **Self-attestation Q1–Q5** (PR body): YES, written by the same LLM-instance that wrote the code. Not independent.
- **Fresh-eyes diff-description review post-coding**: YES — full PR diff pasted back to the review channel; reviewed by a separate Claude instance (mobile interface, no shared context with the implementation instance). Verdict CHANGES_REQUESTED on 3 trivial items:
  1. Remove `"PR #119's successor"` reference from doc-comment line 17 (PR numbers are short-lived; W1.2/A-003 IDs are the durable references).
  2. Unify `"Markers fanget"` Norwegian-in-English-message to English — chose `"Offending entries:"` to match the existing line 274 style.
  3. Add NOTE comment to `engine/lib.rs:2022-2027` explaining the now-permanently-dormant transition-detection log path, with explicit hint that it auto-activates when the dedicated Javelin probe lands.
- **Independent diff-read of compiled binary or behavioral verification**: NO. Const-list-pin tests verify the marker-list state; no end-to-end run on a real host with synthetic bf6.exe process.
- **Verification commands run** (by the implementation instance):
  - `cargo build --workspace` — clean (1 prior `dead_code` warning resolved via `#[allow(dead_code)]` on `AcMarker::Javelin`)
  - `cargo clippy --workspace --all-targets -- -D warnings` — clean
  - `cargo fmt --all --check` — clean
  - `cargo test -p framesage-sys --lib` — 33 passed (including the 3 new ac_detect tests)
  - `cargo test --workspace` — totals unchanged; no regressions
  - `git grep -i bf6 -- '*.rs'` (META 1 pre-coding sweep) — 37 hits classified into 4 categories; only Category D (the one marker) touched.

**Defects fanget:**
- 3 items in fresh-eyes review (all addressed in amend before merge): the PR-#-reference, the Norwegian-in-English message, the missing NOTE on the engine-side dormant log. None semantic; all polish / docs / consistency.
- Buddy also asked explicit confirmation that the `#[allow(dead_code)]` doc-comment on `AcMarker::Javelin` explicitly explained the allow rationale — it did not. Added the suggested sentence: `` `#[allow(dead_code)]` rationale: kept-but-dormant pending dedicated Javelin probe (W1.2 / A-003). ``

**Force-push trade-off captured for future PRs:**
- The amend-then-force-push flow required explicit user authorization to bypass the auto-mode classifier's destructive-git block. The classifier also misclassified `git reset --soft origin/<branch>` (a pure HEAD-pointer move, no remote contact) as "force-push" and blocked that too. Going forward: either user pre-authorizes force-pushes for trivial post-review amends, OR follow-up commits become the default (extra commit in PR history but no classifier friction). For W1.2 the user explicitly chose force-push to preserve clean single-commit PR history.

**Verdict:** GO. PR #120 merged as commit `52f7b97`. Finding A-003 closed.

### Lesson for future PRs

The pre-coding diff-plan review caught structural issues (test-split-vs-rename, comment quality, META 1 sweep discipline) BEFORE code was written — saved a round of post-coding diff churn. The post-coding fresh-eyes diff-description review caught polish issues (PR-#-reference, Norwegian-in-English, missing NOTE) that no test would have surfaced. Both review-surfaces are load-bearing in their own way; neither is "full buddy review" (which would require an independent reviewer reading the actual final diff + running their own verification). Logging the surface honestly so the audit doesn't drift to "self-review with extra steps."

---

## PR #122 — 2026-05-18 — verdict GO — review-surface: pre-coding diff-plan review with 1 mikro-fix surfaced, addressed in initial commit; re-review explicitly waived on pure-docs fix

**Source:** W1.3 (#82) — Arc::as_ptr lifetime doc-comment in `consumer_loop`. Closes finding A-004 (MEDIUM) per `audit/2026-revision-phase2-findings.md`. Phase 3 roadmap item W1.3, S effort, pure-docs change.

### Review-surface honesty

**What was actually checked:**
- **Pre-coding diff-PLAN review** (full diff text drafted + presented to user BEFORE branch creation): user surfaced 1 mikro-fix on the LIFETIME CONTRACT block point 2's wording — replace brittle `"dropped explicitly at line ~1340"` line-number reference with durable `"It is dropped when consumer_loop returns, after the ProcessTrace call below completes"`. Same principle as preferring finding-IDs (A-004) over PR-#-references (PR #119): line numbers drift with future edits, function names + control-flow descriptions don't. Addressed in the INITIAL commit (no amend needed).
- **Pre-coding META 1 sweep** (`git grep -in "Arc::as_ptr\|UserContext\|logfile.Context"`): two production-pattern call-sites found. The one this PR documents (`crates/etw/src/session.rs:1309`); spike-etw site at `crates/spike-etw/src/main.rs:585` explicit no-op-confirmed (scheduled for removal at M3.4 / #113). Two false-positives (`crates/sys/src/inner/version_info.rs:114, 146` — different lifecycle pattern).
- **Spike-citation honesty check** (`grep -n "§12.4\|Step 23\|callback.*never"` in spike report): discovered the spike's Step 23 was about `closed_loop_enabled` toggle, NOT callback timing; Step 24 about Drop teardown; Step 28 about survives-restart. Direct callback-timing instrumentation does NOT exist in the spike. Doc-comment framed honestly as **indirect** evidence (end-to-end teardown ran without crash → no callback fired against dropped Arc, or it would have been observable) rather than claiming direct verification that doesn't exist.
- **Self-attestation Q1–Q5** (in PR body): YES. Same LLM-instance as code author. Re-review explicitly waived by user on a pure-docs fix.
- **Independent diff-read of compiled binary or behavioral verification**: N/A — pure-docs change; no behavioral surface to verify.
- **Verification commands run:**
  - `cargo build --workspace` — clean
  - `cargo clippy --workspace --all-targets -- -D warnings` — clean
  - `cargo fmt --all --check` — clean
  - `cargo test -p framesage-etw --lib` — 18 passed, 3 ignored (totals unchanged from pre-PR)
  - `cargo test --workspace` — no regressions

**Defects fanget (1, addressed pre-coding):**
- LIFETIME CONTRACT block point 2 originally cited `"dropped explicitly at line ~1340"` — brittle line-number reference. User-suggested replacement: `"It is dropped when consumer_loop returns, after the ProcessTrace call below completes."` — durable phrasing tied to function semantics, not source location.

**Convention-locking forward-note (P4.8 kickoff item, NOT changed in this PR):**
This PR establishes the source-code `[UNVERIFIED]` marker format: `[UNVERIFIED — <reason>. <evidence status>. <how to upgrade>.]` per P4.2 promotion from audit-only to source convention. Locking this format as the standard for `git grep -i "\[UNVERIFIED"` triviality is a P4.8-sweep kickoff agenda item. Forward-noted here so the convention-lock decision lands when the periodic sweep starts, not later as a drift-discovery.

**Verdict:** GO. PR #122 merged as commit `45a0b83`. Finding A-004 closed.

### Lesson for future PRs

Pre-coding diff-plan review continues to pay dividends: this is the third PR in a row (#119 W1.6, #120 W1.2, #122 W1.3) where the user-surfaced adjustment landed in the INITIAL commit rather than as an amend / follow-up commit. Net effect: zero force-pushes for this PR, no classifier friction. Worth keeping the "diff-plan first, code second" rhythm explicit for the remaining Week 1 items.

---

## PR #124 — 2026-05-18 — verdict GO — review-surface: pre-coding 6-point analysis + diff-plan review + raw-diff fresh-eyes review (HIGH-severity-shutdown-path discipline); fresh-eyes round was CONFIRMATION, no corrections beyond diff-plan round

**Source:** W1.4 (#83) — drop `verify_session_gone` from `EtwSession::stop()`. Closes finding C-001 (HIGH) + K-003 (linked). Phase 3 roadmap item W1.4, S effort, HIGH severity because it touches shutdown-path code.

### Review-surface honesty

**What was actually checked:**
- **Pre-coding 6-point analysis** explicitly requested by user before diff-plan: call-site context, helper-fn definition, `cleanup_stale_session` coverage comparison, K-003 link verification, META 1 sweep for other call-sites, test/Drop-impl impact. All 6 points addressed in writing before any code was drafted.
- **Pre-coding diff-plan review** (full diff text presented to user before branch creation): user surfaced 1 påkrevd fix (function-name > line-number reference, same durability principle as W1.3) + 1 valgfri (test NOTE — adopted) + 1 PR-body forsterkning (coverage-comparison table included in PR body, not just in chat-history diff-plan). All addressed in INITIAL commit.
- **Raw-diff fresh-eyes review** (gh pr diff 124 pasted to review channel pre-merge per HIGH-severity-shutdown-path-code discipline): user verified the diff matches the plan exactly. **No corrections from this round.** Items verified:
  - No unintended changes / scope creep
  - `query_session_stats` untouched (still used from `EtwSession::query_stats` + `MonitorHandle::poll_drop_stats`)
  - Drop impls untouched
  - `bail!` import implicitly still used (build/clippy green excludes dangling import)
  - NOTE placement relative to test is correct Rust style
- **Self-attestation Q1–Q5** (in PR body): YES. Same LLM-instance as code author.
- **Independent behavioral verification on real host**: NO — this is a deletion of a flake-producing post-STOP query; the absence-of-error is the verification. The deleted code was the source of false-positive bails; removing it cannot introduce new flakes by construction.
- **Verification commands run:**
  - `cargo build --workspace` — clean
  - `cargo clippy --workspace --all-targets -- -D warnings` — clean
  - `cargo fmt --all --check` — clean
  - `cargo test -p framesage-etw --lib` — 18 passed, 3 ignored (totals unchanged from pre-PR; no test referenced `verify_session_gone`)
  - `cargo test --workspace` — no regressions
  - `git grep -n "verify_session_gone" -- '*.rs'` (META 1 pre-coding sweep) — 4 hits classified; 2 removed (etw/session.rs), 2 explicit no-op (spike-etw same-name-different-function).

**Defects fanget (3 in diff-plan round, 0 in raw-diff round):**

Diff-plan round (pre-coding):
1. Påkrevd: `"(line ~704)"` in the explanatory comment → replaced with `"cleanup_stale_session() at the next start_with_syscalls() call"` per the function-name > line-number durability principle (same as W1.3).
2. Valgfri: Add NOTE to the `#[ignore]`'d test explaining what it no longer detects post-W1.4 → adopted (forestaller "did this test fail because of W1.4?" investigation if/when un-ignored).
3. PR-body forsterkning: Include the 5-row coverage-comparison table in the PR body, not just in chat-history diff-plan → adopted (the table IS the safety analysis; future git-history readers should see it in the PR, not have to dig).

Raw-diff round (post-coding, pre-merge):
- **Zero corrections.** Diff matched the plan exactly. Both verify-points raised in the agent's raw-diff report (error-propagation-paths reduction; `query_session_stats` vs `verify_session_gone` separation) were verified as correctly reasoned by the reviewer.

**Catch-rate observation:** PRs #119 (W1.6), #120 (W1.2), #122 (W1.3) all had at least one user-surfaced adjustment between diff-plan and final commit. PR #124 (W1.4) had three adjustments in the diff-plan round but ZERO in the raw-diff round — i.e., the diff-plan round caught everything before code was written. Across the four PRs: ~75% catch rate on the buddy-rhythm rounds, with the layering working as designed (diff-plan catches structural / wording issues; raw-diff round catches scope-creep / unintended-change-creep / accidental-typo-style issues). Healthy signal.

**Verdict:** GO. PR #124 merged as commit `8c7eec3`. Findings C-001 + K-003 closed.

### Lesson for future PRs

The two-round buddy rhythm (diff-plan → raw-diff) has now been demonstrated on a HIGH-severity shutdown-path PR with the result that the raw-diff round was CONFIRMATION rather than correction. This is the desired outcome: by the time code is written and pushed, the diff-plan round has already absorbed the structural feedback. For future HIGH-severity PRs, retaining the raw-diff round even when no defects are expected is still the right discipline — it's cheap insurance, and the result of "zero corrections" is itself useful audit signal (the diff-plan round was thorough).

---

## PR #126 — 2026-05-18 — verdict GO — review-surface: pre-coding 7-point survey caught audit-vs-codebase drift; diff-plan review with 4 user-surfaced fixes; raw-diff fresh-eyes round (per HIGH-severity discipline) + visual strikethrough verification on GitHub-rendered table

**Source:** W1.5 (#84) repurposed from "add the test" to "audit correction" after pre-coding META 1 sweep discovered `mode_5_session_level_full_flow_panic` already existed at `crates/etw/src/session.rs:1718`. Closes E-001 via Resolution block (prior PR #79 resolution), adds E-005 (catch_unwind `&str`-branch coverage gap discovered during the same survey), adds P4.9 (process improvement against future audit-vs-codebase drifts), fixes stale comment at `crates/etw/src/supervisor.rs:254-257`.

### The first audit-tradition self-correction

PR #126 is the audit-tradition's first self-correction. The audit (written 2026-05-18 19:49:56 in PR #117) classified E-001 as open 3h 40m after the test it claimed was missing actually landed in PR #79 (2026-05-18 16:09:37). The drift was caught by Mac-Claude during W1.5 survey (#84), not in a separate audit-sweep. The rhythm caught itself by virtue of the startup procedure requiring code-survey BEFORE diff-plan. **If Mac-Claude had skipped straight to implementation ("roadmap says add test, so add test"), the duplicate test would have landed. Forarbeid > tempo.**

The drift catch motivates P4.9: open-classification is a weaker guarantee than closed-classification. The Phase 2 5/5 spot-check validated CLOSED findings; OPEN findings were assumed honest. Backfill issue M1.13 (#127) tracks the systematic verification work — expected to surface more drifts.

### Review-surface honesty

**What was actually checked:**

- **Pre-coding 7-point survey** (user-requested before diff-plan): arm_panic_in_process_trace infrastructure, existing supervisor-test scope, method-name confirmation (into_supervisable_parts vs _with_monitor), ConsumerExitReason::Panicked shape, catch_unwind RefCell soundness, META 1 sweep for other panic-path tests, trace-event capture decision. The META 1 sweep step is what discovered the audit drift — `git grep -n "arm_panic_in_process_trace\|panic_in_process_trace\|ConsumerExitReason::Panicked\|catch_unwind\|consumer_thread_panic\|consumer_loop.*panic" -- "*.rs"` returned hits at `session.rs:1718` (the existing test). The survey discipline directly prevented duplicate work.
- **Diff-plan review** (full text of all 5 edits + 7 surface decisions presented): user surfaced 4 fixes — (1) påkrevd: retain raw-diff fresh-eyes round despite comment-only change; (2) påkrevd: soften E-005 fix-suggestion from "Cleanest implementation:" to "Suggested implementation: ... or any equivalent approach" allowing `panic_any` alternative; (3) verify: GitHub strikethrough rendering visually before merge with plain-text fallback; (4) follow-up: separate Month 1 issue for P4.9 backfill (not in this PR scope). All 4 addressed in INITIAL commit.
- **Raw-diff fresh-eyes round** (per HIGH-severity-shutdown-path discipline, retained even for comment-only change on supervisor.rs per påkrevd #1): user verified the diff matches the plan exactly. **No corrections from this round.**
- **Visual strikethrough verification** (per påkrevd #3): user opened PR #126's "Files changed" tab on GitHub and confirmed `~~E-001 (...)~~`-syntax renders correctly as gjennomstreket tekst with normal following clause; tabell intakt. No fallback needed.
- **Self-attestation Q1–Q5** (in PR body): YES. Same LLM-instance as code author.
- **Independent behavioral verification**: N/A — audit-correction PR; the "behavior" being verified is the audit's own text correctness, which the user reviewed directly.
- **Verification commands run:**
  - `cargo build --workspace` — clean
  - `cargo clippy --workspace --all-targets -- -D warnings` — clean
  - `cargo fmt --all --check` — clean
  - `cargo test -p framesage-etw --lib mode_5_session_level_full_flow_panic` — 1 passed (confirms the test the Resolution block points to still works after supervisor.rs comment update)
  - `cargo test --workspace` — no regressions
  - `git log --oneline -S "mode_5_session_level_full_flow_panic" -- crates/etw/src/session.rs` — confirmed test landed in commit 6f972b6 (PR #79)
  - `git show -s --format="%ci"` on PR #79 and PR #117 commits — confirmed 3h 40m timeline drift

**Defects fanget (4 in diff-plan round, 0 in raw-diff round):**

Diff-plan round (pre-coding):
1. Påkrevd: retain raw-diff fresh-eyes round for supervisor.rs change despite it being comment-only (principle: catch unexpected; not skip-because-known).
2. Påkrevd: soften E-005 fix-suggestion (don't lock future fixer to mock-API extension).
3. Verify: strikethrough rendering visually (with fallback plan).
4. Follow-up: separate Month 1 issue for P4.9 backfill (created post-merge as #127).

Raw-diff round (post-coding, pre-merge):
- **Zero corrections.** Diff matched the plan exactly. Strikethrough verified visually post-push.

**Verdict:** GO. PR #126 merged as commit `0abfcba`. Issue #84 closed via prior resolution. E-005 added to findings (Month 1, MEDIUM, S effort). P4.9 added to roadmap. M1.13 (#127) created post-merge to track the P4.9 backfill.

### Catch-rate observation update (5 PRs in)

| PR | Diff-plan round adjustments | Raw-diff round adjustments | Note |
|---|---|---|---|
| #119 (W1.6) | yes | yes | Two-round catches |
| #120 (W1.2) | yes (META 1 + diff-plan) | yes | Two-round catches |
| #122 (W1.3) | yes (1 mikro-fix) | 0 | Diff-plan absorbed all |
| #124 (W1.4) | yes (3 items) | 0 | Diff-plan absorbed all |
| #126 (W1.5) | yes (4 items, incl. AUDIT DRIFT catch via META 1 sweep) | 0 | **First audit-tradition self-correction** |

Catch-rate now ~80% on the buddy-rhythm rounds (one or both rounds catches something in 4 of 5 PRs). The pattern: pre-coding survey + diff-plan round absorbs structural / wording / scope-creep concerns; raw-diff round catches the rare unexpected-change. PR #126 was the first to demonstrate that META 1 sweep can also catch audit errors — not just code-side issues.

### Lesson for future PRs

The startup procedure's "code-survey FØR diff-plan" requirement is load-bearing. Without it, PR #126 would have re-implemented an existing test (E-001) instead of correcting the audit. P4.9's Month 1 backfill (#127) will systematically apply that discipline to all remaining open findings — expected to surface 1-3 more drifts based on the 2-of-44 base rate observed so far.

---

## PR #129 — 2026-05-18 — verdict GO — review-surface: 2-round mobile-Claude 4Q planning-phase review with Frank-decision injected between rounds; zero open items at merge

**Source:** Frank's 2026-05-18 scope revision — FrameSage re-scoped from "potentially broadly distributed Windows scheduler-supervisor product" to **personal power-user tool** (audience (a): Frank + other self-installers welcomed on informed-consent basis). New section "## Project scope and audience (2026-05-18 revision)" added to `audit/v0.7-architecture.md` between Executive Summary and §2.1.

### Why this PR is distinct from the W1.x rhythm

The W1.x PRs (W1.2 / W1.3 / W1.4 / W1.5 / W1.6) executed roadmap items as already-decided work. PR #129 is **the first structural architectural decision since the initial Phase 2 audit** — it changes the ship contract for v0.7+, supersedes prior sections by reference, and triggers a 5-PR cascade. The review discipline scaled accordingly: 2 mobile-Claude rounds + Frank-decision injection between rounds.

### Review-surface honesty

**What was actually checked:**

- **Diff-plan round 1** (pre-coding, full draft text presented): mobile-Claude returned CHANGES_REQUESTED with 3 påkrevde + 1 valgfri.
  - Q1 påkrevde #1 (Authority concern): Mac-Claude assumed audience-(a) framing without explicit Frank confirmation. Hard-blocked draft finalization. Mac-Claude STOPped and asked Frank via AskUserQuestion: (a) Frank + other self-installers, OR (b) Frank-only-full-stop. Frank chose (a). Difference materially affects scope of cascade work — if (b), onboarding wizard / install.ps1 robustness / error messages would have been lowered priority too.
  - Q3 påkrevde (Feasibility): Add Decision Point #5 for PresentMon (M3.2) keep-vs-skip — XL-effort, requires Frank's call.
  - Q4 påkrevde (Consistency): Expand AC-row with empirical-feedback-loop framing (mobile-Claude verbatim text adopted).
  - Q4 valgfri: Audit-intensity 30-day re-evaluation note. Frank chose: include.
- **Diff-plan round 2** (revised draft after round 1 fixes): mobile-Claude returned CHANGES_REQUESTED with 1 structural change.
  - DP #5 (PresentMon) moved from "Decision points still open" to "Cascade work tracked separately" as a SEPARATE future scope-revision PR. Rationale: split decisions from data — Frank can't evaluate PresentMon keep-vs-skip until Group A ETW parsers ship and he's used CPU-only attribution for ~2 weeks. "Decision points still open" section removed entirely (was empty after move). §2.2 "How to consume" line updated to point at cascade-item-#6 mechanism. **Framings-PR closes clean with zero open items.**
- **Per W1.4 discipline**: markdown-only change, NO raw-diff round needed. Standard 5Q self-attestation in PR body.
- **Verification commands run:**
  - `cargo build --workspace` — clean (markdown-only but verified for hygiene)
  - `cargo fmt --all --check` — clean
  - No tests changed; existing `closed_loop_page_contains_edr_disclosure_substring` (W1.6) still pinned per DP #3 Option α (keep onboarding copy verbatim through v0.7).

**Defects fanget (4 in round 1, 1 in round 2, 0 in final):**

Round 1:
1. Påkrevd: Frank-decision needed before draft finalization on audience (a) vs (b). Hard-block.
2. Påkrevd: Missing Decision Point #5 (PresentMon).
3. Påkrevd: AC-row needed empirical-feedback-loop framing expansion.
4. Valgfri: Audit-intensity 30-day re-evaluation note.

Round 2:
1. Structural: DP #5 placement wrong — should be cascade item, not open decision point.

Final round (post-round-2-fixes): zero corrections; merge.

**Verdict:** GO. PR #129 merged as commit `f01c823`. Scope revision authoritative on main.

### Cascade work — sequencing

Per mobile-Claude round 2 suggestion: ~one week, NOT one session. 5 cascade PRs + 1 future separate scope-revision PR:

1. **Cascade PR 1** — Findings-file Resolution blocks (8 items: I-001 / I-002 / I-003 / I-005 / J-001 / J-002 / J-003 / J-004)
2. **Cascade PR 2** — Roadmap-file updates (Week 1 won't-fix; Month 2/3 adjusted; K-004/K-005 indefinitely deferred)
3. **Cascade PR 3** — GitHub issues batch-close (#80 / #86 / #87 / #88 + any others)
4. **Cascade PR 4** — CLI verb for closed-loop toggle (DP #2 cascade item, M-effort)
5. **Cascade PR 5** (optional) — README scope callout (Frank's call)
6. **Future separate scope-revision PR** — PresentMon scope decision (gated on Group A completion + 2 weeks Frank-usage)

**Awaiting Frank's directive on cascade-rekkefølge.** Mac-Claude does NOT open cascade PRs autonomously — each step is a separate decision point.

### Catch-rate observation update (7 PRs in)

| PR | Round 1 (pre-coding) | Round 2 (post-coding / structural) | Note |
|---|---|---|---|
| #119 (W1.6) | yes | yes | Two-round catches |
| #120 (W1.2) | yes | yes | Two-round catches |
| #122 (W1.3) | yes (1 mikro-fix) | 0 | Diff-plan absorbed all |
| #124 (W1.4) | yes (3 items) | 0 | Diff-plan absorbed all |
| #126 (W1.5) | yes (4 items, **AUDIT DRIFT catch**) | 0 | First audit-tradition self-correction |
| #129 (scope revision) | yes (4 items + Frank-decision injected) | yes (1 structural — DP #5 → cascade) | **First multi-round review with mid-flight Frank decision.** Both rounds load-bearing. |

Pattern: when the PR is **structural** (architecture decision, scope revision), even after a thorough round 1, round 2 catches genuine structural issues. The DP #5 placement was technically correct in round 1 but the framings-PR-should-close-clean principle wasn't applied until round 2. Lesson: structural PRs warrant 2 rounds even if round 1 looks complete.

### Lesson for future structural PRs

Two rounds of mobile-Claude review is the right rhythm for structural architectural decisions. Round 1 catches content / framing / missing-decision-points. Round 2 catches placement / structural-coherence / closing-the-PR-clean issues. The 1-round rhythm that worked for W1.2 / W1.3 / W1.4 / W1.5 is appropriate for execution PRs (already-decided work); it's NOT sufficient for decision-making PRs (new ship contract, new scope, new architectural direction).

---

## PR #131 — 2026-05-18 — verdict GO — review-surface: mekanisk anvendelse av merget scope-revision (PR #129); no mobile-Claude review per cascade-1 directive

**Source:** Cascade PR 1 of 5 (+1 future) per PR #129 (scope revision) cascade plan. Applies mechanical Resolution blocks to 7 findings in axes I + J per Frank's 2026-05-18 cascade-rekkefølge directive.

### Review-surface honesty

**What was actually checked:**

- **Mechanical application of merged scope-revision framing.** No new architectural decisions; no new content beyond what PR #129 already authoritative-section established. Each of the 7 Resolution blocks cites PR #129 + the scope-revision section directly.
- **Per-finding Resolution wording followed Frank's per-finding directive verbatim:**
  - I-001 / I-002 / I-003 / I-005 / J-004 → won't-fix (5 findings)
  - J-001 / J-003 → REFRAMED, not closed; technical content unchanged, framing-only revision (2 findings)
  - J-002 explicitly NOT touched per Frank's directive ("trenger ikke ny Resolution, bare sjekk at eksisterende status er konsistent") — consistency verified: J-002 cross-refs F-001, which closed via W1.6 / PR #119's onboarding-page-3 scaffold.
- **Index + status-summary updates** applied with strikethrough syntax on the affected v0.7.1 BLOCKER and v1.0 entries (visual rendering of strikethrough in GitHub tables previously validated on PR #126 — fall-back-to-plain-text contingency not needed).
- **No mobile-Claude review** per Frank's explicit cascade-1 directive: "Mobile-Claude review IKKE nødvendig på cascade-1 — mekanisk anvendelse av allerede-godkjent scope-ramme på 7 finding-blokker."
- **Self-attestation Q1–Q5** in PR body (markdown-only; build/clippy/fmt confirmed clean).
- **Verification commands run:**
  - `cargo build --workspace` — clean (markdown-only)
  - `cargo fmt --all --check` — clean
  - `git diff --stat` — 1 file, +24 / -10
  - `grep -n "^## I-00\|^## J-00"` to locate finding starting positions before editing
  - Read each finding's body before drafting its Resolution (no surprises)

**Defects fanget:** zero. Mechanical PR with pre-vetted scope frame.

**Verdict:** GO. PR #131 merged as commit `919dca9`. Cascade-1 of 5 (+1) complete.

### Cascade progress

| # | PR | Status |
|---|---|---|
| **Cascade PR 1** | **#131** | **✓ MERGED** |
| Cascade PR 2 | (next) — Roadmap-fil oppdateringer | Pending |
| Cascade PR 3 | GitHub issues batch-close | Pending |
| Cascade PR 5 | README scope callout | Pending |
| Cascade PR 4 | CLI verb for closed-loop toggle | Deferred — tracked as new Month 1 item in cascade-2 |
| Future scope-revision PR | PresentMon scope decision | Gated on Group A + 2 weeks Frank-usage |

Tempo per Frank's directive: halv-dag-PRer; "mellom hver PR — sjekk at main er stabil og det ikke har vært overraskelser." Next: cascade-2 (roadmap-fil oppdateringer).

### Lesson observation

The "no mobile-Claude review per cascade-1 directive" framing is honest precisely because cascade-1 was purely mechanical. The substantive decisions all happened in PR #129's 2-round review. Cascade work serves only to propagate the decisions into the findings/roadmap/issues files. Future cascade PRs (2, 3, 5) follow the same pattern. **Skipping mobile-Claude review on mechanical-application PRs is the correct discipline** — running review on every PR would dilute the signal of when review actually catches things.

---

## PR #133 — 2026-05-18 — verdict GO — review-surface: mekanisk anvendelse av merget scope-revision (PR #129) + cascade-1 (PR #131); no mobile-Claude review per cascade-2 directive; one numbering-conflict catch self-corrected before push

**Source:** Cascade PR 2 of 4 (+1 future) per PR #129 cascade plan. Applies scope-revision framing to `audit/2026-revision-phase3-roadmap.md` per Frank's 2026-05-18 cascade-rekkefølge directive.

### Review-surface honesty

**What was actually checked:**

- **Mechanical roadmap-fil application of scope-revision.** No new architectural decisions; per-item reclassifications follow per-finding Resolution blocks landed in PR #131 (cascade-1).
- **Per-item changes follow Frank's directive verbatim:**
  - Week 1: W1.1 / W1.7 / W1.8 / W1.9 → won't-fix
  - Month 2: M2.1 / M2.2 → won't-fix; M2.7 (K-005) → deferred indefinitely
  - Month 3: M3.5 → REDEFINED (dogfood-gate per DP #1); M3.6 (K-004) → deferred indefinitely
  - Beyond: I-002 (MSI) → won't-fix
- **New Month 1 items added per Frank's directive that cascade-2 expands the roadmap:**
  - M1.13 — P4.9 backfill (mirrors existing issue #127)
  - M1.14 — CLI verb for closed-loop toggle (scope-revision DP #2 cascade-item, deferred from cascade-PR-4 per Frank)
  - M1.15 — Month 1 buddy review (renumbered from M1.12)
- **Roadmap summary table updated:** item counts, cumulative effort per phase, won't-fix annotations.
- **No mobile-Claude review** per Frank's explicit cascade-2 directive: "Mobile-Claude review IKKE nødvendig. Mekanisk."
- **Self-attestation Q1–Q5** in PR body (markdown-only; build/clippy/fmt clean).

**Defects fanget (1, self-corrected before push):**

**Numbering-conflict catch:** Initial draft of cascade-2 used `M1.13` for the new CLI-verb roadmap item. Self-caught via `grep -n "M1.13\|P4.9 backfill"` post-Edit — discovered that issue #127 already uses `[M1.13]` tag for P4.9 backfill. Corrected before push: M1.13 = P4.9 backfill (mirrors #127), M1.14 = CLI verb (new), M1.15 = buddy review (renumbered from M1.12). The corrected commit includes an inline "(Was M1.12 pre-scope-revision; renumbered ...)" note so future readers see the renumbering trail.

**Lesson:** Renumbering items in a roadmap that has corresponding GitHub-issue trackers requires grep-verifying issue titles before assigning new IDs. The `[M1.X]` convention in issue titles creates an implicit numbering contract between the roadmap file and GitHub issues. Future cascade work that inserts/removes roadmap items should run a grep-verify step against current issue titles BEFORE landing the roadmap edit.

**Verdict:** GO. PR #133 merged as commit `41f3836`. Cascade-2 of 5 complete.

### Cascade progress

| # | PR | Status |
|---|---|---|
| Cascade PR 1 | #131 | ✓ MERGED `919dca9` |
| **Cascade PR 2** | **#133** | **✓ MERGED `41f3836`** |
| Cascade PR 3 | (next) — GitHub issues batch-close | Pending |
| Cascade PR 5 | README scope callout | Pending (after cascade-3) |
| Cascade PR 4 | CLI verb implementation (M1.14) | Deferred until Month 1 bandwidth permits |
| Future scope-revision PR | PresentMon scope decision | Gated on Group A + 2 weeks Frank-usage |

### Pacing decision: STOP after cascade-2 per Frank's tempo directive

Frank's cascade-rekkefølge directive: "Tempo: ikke press alle tre i én økt. Halvdag for cascade-1, halvdag for cascade-2, kort økt for cascade-3, så cascade-5 til slutt."

This session has now completed cascade-1 + cascade-2 + their buddy-logs. Cascade-3 (GitHub issues batch-close) is the third — explicitly excluded from "én økt" per Frank's directive. STOP and surface state. Await Frank's directive on cascade-3 timing.

---

## Cascade-3 (no PR, GitHub-state-only) — 2026-05-18 — verdict GO — review-surface: mekanisk anvendelse av merget scope-revision (PR #129) + cascade-1 (#131) + cascade-2 (#133) on GitHub issue state; no mobile-Claude review per cascade-3 directive

**Source:** Cascade step 3 of 4 (+1 future) per PR #129 cascade plan. Applies scope-revision framing to GitHub issue state per Frank's 2026-05-18 cascade-rekkefølge directive. **No PR — direct GitHub mutations only** (8 closes + 1 retitle + 1 new-issue-create).

### Why no PR

Cascade-3 mutates GitHub issue state, not repo files. No source-of-truth file to commit. The audit trail lives in:
- GitHub's own issue-state log (each closed/retitled/created action timestamped + author-stamped)
- This buddy-log entry (the project-side narrative)
- The findings-fil + roadmap-fil edits already merged in cascade-1 (#131) + cascade-2 (#133), which the closure comments cross-reference

This is intentional separation: GitHub state changes have GitHub's audit trail; project-doctrine changes have repo's audit trail. Cascade-3 lives in GitHub-state-land.

### Review-surface honesty

**What was actually done (10 actions via one-shot bash script):**

Closed as won't-fix (6):
- #80 [W1.1] AC outreach
- #86 [W1.7] AC outreach cross-ref
- #87 [W1.8] cert procurement
- #88 [W1.9] EDR engagement
- #102 [M2.1] signing closeout
- #103 [M2.2] EDR matrix closeout

Closed as deferred-indefinitely (2):
- #108 [M2.7] K-005 engine split — explicit "re-open this issue at that point" language for the un-defer trigger
- #115 [M3.6] K-004 tray split — same

Housekeeping (2):
- #101 retitled from [M1.12] → [M1.15] (per cascade-2 renumbering)
- #135 created as [M1.14] CLI verb (scope-revision DP #2 cascade-item; body cross-refs DP #2 in scope-revision section + M3.5 dogfood-gate dependency)

**Per-issue close-comment template** carried verbatim from Frank's cascade-3 directive ("Closed as [won't-fix | deferred indefinitely] per scope revision merged 2026-05-18 in PR #129 (commit f01c823). ..."), with per-issue rationale appended.

**No mobile-Claude review** per Frank's cascade-3 directive: "Mekanisk anvendelse, ingen mobile-Claude review."

**Verification:**
- `gh issue list --repo franzjeger/framesage-win --label audit-2026-revision --state open` → 26 open
- `gh issue list --repo franzjeger/framesage-win --label audit-2026-revision --state closed` → 13 closed
- Pre-cascade-3 counts (after PR #126 created #127): ~35 open, ~5 closed. Post-cascade-3: 8 issues moved from open → closed; 1 new issue created (#135). Net: open 26, closed 13.
- #101 title in `gh issue list` confirms retitle to [M1.15].
- #135 title confirms creation with [M1.14] prefix + correct labels.

**Defects fanget:** zero.

**Verdict:** GO. Cascade-3 complete.

### Cascade progress

| # | Action | Status |
|---|---|---|
| Cascade PR 1 | Findings-fil Resolution blocks | ✓ MERGED (#131, `919dca9`) |
| Cascade PR 2 | Roadmap-fil scope-revision updates | ✓ MERGED (#133, `41f3836`) |
| **Cascade step 3** | **GitHub issues batch (no PR)** | **✓ COMPLETE** |
| Cascade PR 5 | README scope callout | Pending Frank's directive (next or future session) |
| Cascade PR 4 | CLI verb implementation (M1.14 / #135) | Deferred until Month 1 bandwidth permits |
| Future scope-revision PR | PresentMon scope decision | Gated on Group A + 2 weeks Frank-usage |

### Per Frank's directive: STOP after cascade-3 + report state

Frank's cascade-3 directive: "Etter cascade-3 merget: stopp og rapportér state. Cascade-5 (README) kan vente til neste økt eller gjøres etter en kort pause — Frank's call."

Cascade-3 complete; state reported above + in follow-up chat message. Awaiting Frank's cascade-5 timing directive.

### Pattern observation across 3 cascade steps

| Cascade step | Defects fanget | Self-correction needed |
|---|---|---|
| Cascade-1 (#131) | 0 | No |
| Cascade-2 (#133) | 1 (M1.13 numbering conflict) | Yes (pre-push, grep-verify) |
| Cascade-3 (no PR) | 0 | No |

Pattern: mechanical-application cascades have low defect rates BUT the one defect that did surface (cascade-2's M1.13 conflict) was caught only because the implementer remembered to grep-verify the corresponding GitHub issue state. **Lesson reinforced:** mechanical work isn't defect-free; the discipline that catches the rare defect is what makes "mechanical" honest framing.

---

## PR #137 — 2026-05-18 — verdict GO — review-surface: mekanisk anvendelse av merget scope-revision på README; cascade-5 closes the cascade wave

**Source:** Cascade PR 5 of 5 — final cascade-PR in the PR #129 scope-revision cascade plan. Adds 5-sentence "## Project scope" callout to README between the tagline and the existing "## Who is this for?" section.

### Review-surface honesty

**What was actually done:**
- Single 4-line insertion in `README.md` between tagline (line 3) and the existing `## Who is this for? (read first)` section (line 5).
- Content matches Frank's cascade-5 directive sentence-by-sentence:
  1. FrameSage is a personal power-user tool.
  2. Not a broadly distributed product.
  3. Self-install via `install.ps1` from source is the expected distribution model.
  4. Other power-users welcome on informed-consent basis (audience (a)).
  5. Pointer to `audit/v0.7-architecture.md` §"Project scope and audience (2026-05-18 revision)".
- Placement chosen: FIRST sub-section after tagline, BEFORE "Who is this for?". Rationale: "Who is this for?" addresses USE CASE; new section addresses AUDIENCE; scope is more fundamental ("should I install this at all"), so it goes first.
- **No mobile-Claude review** per Frank's cascade-5 directive ("mekanisk anvendelse av allerede-godkjent scope-ramme").
- Self-attestation Q1–Q5 in PR body.

**Defects fanget:** zero.

**Verdict:** GO. PR #137 merged as commit `fd5b117`. Cascade wave complete.

### Cascade wave summary

| # | Action | Commit | Status |
|---|---|---|---|
| Cascade PR 1 (#131) | Findings-fil Resolution-blokker (7 reclassifications + index/summary updates) | `919dca9` | ✓ MERGED |
| Cascade PR 2 (#133) | Roadmap-fil scope-revision updates (incl. M1.13/M1.14/M1.15 renumbering + summary table) | `41f3836` | ✓ MERGED |
| Cascade step 3 (no PR) | 8 GitHub issues closed + #101 retitled + #135 new (M1.14 CLI verb) | n/a | ✓ COMPLETE |
| **Cascade PR 5 (#137)** | **README scope callout** | **`fd5b117`** | **✓ MERGED** |
| Cascade PR 4 (deferred) | CLI verb implementation (M1.14 / #135) | — | Deferred until Month 1 bandwidth |
| Future scope-revision PR | PresentMon scope decision | — | Gated on Group A completion + 2 weeks Frank-usage |

### Pattern observation across the full cascade wave (4 cascade steps)

| Step | Type | Defects fanget | Self-correction |
|---|---|---|---|
| Cascade-1 (#131) | Findings-fil Resolution-blocks | 0 | N/A |
| Cascade-2 (#133) | Roadmap-fil updates | 1 (M1.13 numbering conflict) | Yes — pre-push grep-verify |
| Cascade-3 (GitHub state) | Issues batch-close + housekeeping | 0 | N/A |
| Cascade-5 (#137) | README callout | 0 | N/A |

**Total defect rate across 4 cascade steps: 1 in ~30 individual actions.** The one defect was caught at the pre-push stage by grep-verify discipline (not by post-merge review). Lesson: mechanical-application cascades have low defect rates, but the rare defect is caught by **the implementer's own pre-push verification**, not by mobile-Claude review. Skipping mobile-Claude review on mechanical PRs is the correct discipline; the verification work that catches defects is grep-verify against source-of-truth state (GitHub issues, existing roadmap structure, current audit-file content).

### Lesson for future cascade waves

When a future scope-revision triggers a cascade wave:
1. **Mobile-Claude review on the framing-PR** (the architectural decision itself).
2. **Skip mobile-Claude review on cascade application PRs** (mechanical work).
3. **Pre-push grep-verify discipline** is the load-bearing defect-catch mechanism for cascade PRs.
4. **One buddy-log entry per cascade step** (whether PR or GitHub-state-only) so the audit trail captures both the mechanical-application AND the grep-verify-discipline outcomes.

This wave validated the pattern. Future cascade waves can follow the same shape.

---

## PR #139 — 2026-05-18 — verdict GO — review-surface: mekanisk P4.9-backfill session 1 (10 findings grep-verified); no mobile-Claude review per established cascade-pattern

**Source:** M1.13 / issue #127 — P4.9 audit-finding verification backfill, session 1 of ~3. Per P4.9: open-classification is a weaker guarantee than closed-classification; backfill applies grep-verify-discipline to every still-open Phase 2 finding to confirm the gap is still in code (or surface drift via Resolution block).

### Session 1 scope + results

Scope (10 findings, all from axes A + B + start of C):
- A-001, A-002, A-005, A-006, A-007 (full axis-A actionable open set)
- B-001, B-002, B-003, B-005 (full axis-B actionable open set; B-004 is N/A credit)
- C-002 (first of axis-C still-open)

Results:
- **VERIFIED OPEN: 10**
- **CLOSED PRIOR: 0** — no drifts found in session 1
- **AMBIGUOUS: 0** — all findings had unambiguous load-bearing claims grep-verify could pin

### Defect-rate observation update

Base-rate prediction (per pre-cascade observation): ~4.5% drift rate (2 drifts in 44 findings).
Session 1 outcome: 0/10. Within expected variance.

Cumulative drift catch across the engagement so far:
- PRE-L-006 (caught during Phase 3 roadmap drafting)
- E-001 (caught during W1.5 META 1 sweep)
- Session 1 of P4.9 backfill: 0 new drifts

Predicted total for remaining 2 sessions: 1-3 drifts based on mobile-Claude's variance estimate. Even if 0 more drifts surface, the verification work hardens the audit's open-classification confidence — that's the P4.9 contract.

### Review-surface honesty

**What was actually done:**
- Per finding: load-bearing claim identified → grep command run against current main → result documented in finding-blokk as "Verified open (M1.13 / P4.9 backfill, 2026-05-18)" annotation including verification command + output summary.
- Annotation format follows P4.9's application of P4.4's SAFETY-VERIFICATION mechanism, retargeted from unsafe-block soundness to finding-existence verification.
- All 10 verification commands used standard `grep -n` against named file paths the findings cite. No fuzzy / multi-step verifications in session 1.
- **No mobile-Claude review** per established cascade-pattern (mechanical work; pre-push grep-verify-discipline is the load-bearing defect catch).
- Self-attestation Q1–Q5 in PR body.

**Defects fanget:** zero.

**Verdict:** GO. PR #139 merged as commit `f4ec21c`. Session 1 complete.

### Session pacing decision

Per Frank's M1.13 directive: 8-10 findings per session, ~3 sessions total. State-rapport after session 1 BEFORE session 2 starts so Frank can decide continue vs pause.

Session 1: 10 findings done in this Mac-Claude turn. **STOP after session 1 + report state.** Await Frank's directive on session 2 timing.

### Pattern observation (4 P4.9 sessions across the engagement)

| Step | Type | Defects fanget |
|---|---|---|
| PRE-L-006 catch | Inline during roadmap drafting | 1 |
| E-001 catch | META 1 sweep during W1.5 | 1 |
| Cascade-1/2/3/5 | Scope-revision application | 1 (M1.13 numbering, pre-push) |
| **M1.13 session 1** | **P4.9 backfill, mechanical** | **0** |

Pattern: drifts cluster around moments of high attention (META 1 sweeps, pre-push grep-verifies) rather than uniformly across mechanical work. Session 1's 0-drift result is consistent — A-axis and B-axis findings tend to be precisely-cited code-structure observations (line numbers, function names) that survive longer than diffuse "test missing" / "behavior unspecified" claims. Sessions 2 + 3 likely contain more of the diffuse-claim variety; drift-rate may be higher there.

---

## PR #141 — 2026-05-18 — verdict GO — review-surface: mekanisk P4.9-backfill session 2 (9 findings); multi-step verification applied to diffuse-claim findings per Frank's session-2 guidance

**Source:** M1.13 / issue #127 — P4.9 audit-finding verification backfill, session 2 of ~3.

### Session 2 scope + results

Scope (9 findings): C-004, C-005, D-001, D-002, D-003, D-004, D-006, E-002, E-003.

Results:
- **VERIFIED OPEN: 9**
- **CLOSED PRIOR: 0** — no drifts
- **AMBIGUOUS: 0**
- **Refinements noted: 2** (text-quality, not drifts):
  - D-001 Impact-text: "three RefCells inside `control_trace`" — actual code has 2 `borrow_mut()` in cited span. Underlying RefCell-borrow-leak concern unchanged.
  - E-003 "per E-001" cross-reference partially-stale. E-001 closed via PR #79's mock-based test; E-003's mock-path concern is mock-covered, but E-003's Fix-text asks for CI-runnable real-ETW path — that's the actual remaining gap. Refinement embedded in annotation; recommendation added for Group A week 3 kickoff to land a CI-runnable real-ETW smoke test with elevated-host auto-skip.

### Multi-step verification per Frank's session-2 guidance

Frank's directive: "For E/G/pre-L-axis findings: bruk multi-step verification, ikke bare enkelt grep." Applied to:
- **E-002**: read source-comment context AT lines 275-300 (acknowledging-comment + `logs_contain` calls) + grep for custom-subscriber patterns. Two-step verification: (a) acknowledging-comment present, (b) custom subscriber absent.
- **E-003**: two-step verification: (a) mock-path test exists (`mode_5_session_level_full_flow_panic` at session.rs:1720), (b) real-ETW tests exist but `#[ignore]`'d. Refinement note documents the partial overlap with E-001's resolution.
- **C-005**: two-step verification: (a) function definition (`compute_attribution_summary`) zero matches, (b) any of 5 threshold-substrings zero matches. Both halves of gap confirmed unbuilt.
- **D-001**: sub-verify of finding's Impact-text claim ("three RefCells") — found 2 in cited span. Inaccuracy noted but doesn't invalidate gap.

### Drift-rate hypothesis test (preliminary)

Pre-session-2 hypothesis: diffuse-claim axes (E "test honesty", G "performance hot path", pre-L "unbuilt deliverables") might drift more than A/B axes (precisely-cited code-structure observations).

Session 2 result on E-axis subset (E-002 + E-003 only): 0 drifts. Sample size too small to confirm or refute. Session 3 covers full E-axis tail (E-004 + E-005) + all G + all pre-L; that's where the hypothesis gets a fairer test.

### Review-surface honesty

**What was actually done:**
- 9 findings verified using grep + `sed -n 'NM,LMp'` for context-reading on diffuse claims.
- All 9 annotations follow the format: `**Verified open (M1.13 / P4.9 backfill session 2, 2026-05-18):** <verification command/s> → <output summary>. Gap holds.` (refinement-notes appended where applicable).
- **No mobile-Claude review** per established cascade-pattern (mechanical work; pre-push grep-verify is the load-bearing defect catch).
- Self-attestation Q1–Q5 in PR body.

**Defects fanget:** zero. Two refinement-notes (not drifts) captured for future audit-text quality work.

**Verdict:** GO. PR #141 merged as commit `fd0215e`. Session 2 complete.

### Cumulative engagement state

| Phase | Drifts |
|---|---|
| PRE-L-006 inline catch (Phase 3 drafting) | 1 |
| E-001 META 1 sweep catch (W1.5) | 1 |
| Cascade-2 numbering-conflict (cascade application) | 1 (pre-push) |
| M1.13 session 1 | 0 |
| **M1.13 session 2** | **0** |
| **Total** | **3 in ~70 individual verifications/actions** |

~4.3% drift catch rate; aligns with the ~4.5% base-rate prediction within statistical variance.

### Session pacing

STOP after session 2 per Frank's "Etter session 2 merget: state-rapport per session 1-format". Await Frank's session 3 timing directive.

---

## PR #143 — 2026-05-18 — verdict GO — review-surface: mekanisk P4.9-backfill session 3a (8 findings); first M1.13 drift catch (F-004) via designed-to-be-verified status finding

**Source:** M1.13 / issue #127 — P4.9 audit-finding verification backfill, session 3a of 3.

### Session 3a scope + results

Scope (8 findings): E-004, E-005, F-004, G-001, G-002, G-003, H-004, I-004.

Results:
- **VERIFIED OPEN: 7**
- **CLOSED VIA VERIFICATION: 1** (F-004 — parking_lot migration confirmed complete)
- **AMBIGUOUS: 0**
- **Refinements noted: 1** (I-004 — install.ps1 → `%ProgramFiles%` source-level confirmed; ACL VM-spot-check still pending)

### F-004 drift detail

F-004 was the only finding in 27 backfill-verified items so far that was DESIGNED-TO-BE-CLOSED-BY-VERIFICATION rather than a genuine audit-vs-codebase drift. The finding text said: "status is unverified this turn; need a quick grep to confirm zero `std::sync::Mutex` remains in tray".

Session 3a executed the verify:
- `grep -rn 'std::sync::Mutex\|std::sync::RwLock' crates/tray/src/` → ZERO matches
- `grep -rn 'parking_lot::Mutex' crates/tray/src/` → 2 matches at `ipc_client.rs:37`, `main.rs:27`

Conclusion: PHASE2-PLAN.md item 3.2 migration is complete; SUMMARY.md "What's already good" credit was accurate. F-004 closed via verification.

**Drift-shape distinction:** PRE-L-006 + E-001 were genuine audit-vs-codebase drifts (audit said "open" when code had already addressed the underlying gap, written hours before the audit). F-004 is **audit-state-progression** — the audit correctly identified "needs verification"; this session performed the verification; result confirmed the underlying work was done.

For drift-rate accounting, F-004 counts but is qualitatively different. True audit-vs-codebase drift count remains 2 (PRE-L-006 + E-001). With cascade-2's M1.13 numbering catch + F-004, total catches across engagement: 4 in ~78 actions (~5.1%, within variance of 4.5% base-rate).

### Hypothesis test status (3 of 3 backfill sessions covering E/G/pre-L axes)

Pre-session-2 hypothesis: E/G/pre-L axes might drift more than A/B (because diffuse claims age worse).

Session-by-session coverage of "diffuse-claim" axes:
- Session 1: A + B + 1 from C. Zero E/G/pre-L. Drift count: 0.
- Session 2: C + D + 2 from E. Drift count: 0 (E-axis fully covered for E-002, E-003 only).
- Session 3a: 2 from E (E-004, E-005) + 3 G + F-004 + H-004 + I-004. Drift count: 1 (F-004, F-axis).

E-axis: 4 findings covered, 0 drifts. G-axis: 3 findings covered, 0 drifts. **Hypothesis NOT confirmed** at this point. Session 3b (pre-L axis) is the final test.

### Review-surface honesty

**What was actually done:**
- 8 findings verified using multi-step verification on E/G/F/H/I (per Frank's session-3a guidance "Vær ekstra grundig i verifikasjons-kommandoene for disse — overflate-grep kan glipp finne drift som mer detaljert sjekk ville fanget").
- F-004 specifically: 2-step grep (residue check + parking_lot positive check). Sufficient because the finding's claim was binary (zero std::sync::Mutex residue or not).
- I-004 multi-step: source-grep for install.ps1 path + ACL comment + ls for verification doc. Refinement captured both source-level closure half + VM-spot-check still-pending half.
- E-004 multi-step: function-existence (compute_attribution_summary) + string-search (Did FrameSage help / degraded) + directory-listing (spike/research). Confirmed dependency on C-005 + negative-session-capture work.
- E-005 multi-step: companion-method check + test-name search + existing test inventory.
- **No mobile-Claude review** per established cascade-pattern.

**Defects fanget:** F-004 closure (counted as drift).

**Verdict:** GO. PR #143 merged as commit `557df2e`. Session 3a complete.

### Cumulative engagement state

| Phase | Drifts (genuine audit-vs-codebase + audit-state-progression) |
|---|---|
| PRE-L-006 inline catch (Phase 3 drafting) | 1 (genuine) |
| E-001 META 1 sweep catch (W1.5) | 1 (genuine) |
| Cascade-2 numbering-conflict (cascade application) | 1 (pre-push catch) |
| M1.13 session 1 | 0 |
| M1.13 session 2 | 0 |
| **M1.13 session 3a** | **1 (audit-state-progression — F-004 verify-status)** |
| **Cumulative** | **4 in ~78 actions; 2 of those are genuine drift** |

### Session pacing

STOP after session 3a per Frank's "Etter session 3a merged: state-rapport, avventer direktiv på 3b". Session 3b will cover K-001, K-003 (consistency check), PRE-L-001 through PRE-L-005 + finalization (status summary update + issue #127 closure if all drifts captured).

---

## PR #145 — 2026-05-18 — verdict GO — review-surface: mekanisk P4.9-backfill session 3b (FINAL); K-003 cross-reference drift catch; M1.13 / issue #127 closed

**Source:** M1.13 / issue #127 — P4.9 audit-finding verification backfill, session 3b (FINAL).

### Session 3b scope + results

Scope (7 findings): K-001, K-003, PRE-L-001 through PRE-L-005.

Results:
- **VERIFIED OPEN: 5** (K-001 + PRE-L-001 + PRE-L-002 + PRE-L-003 + PRE-L-004-with-refinement + PRE-L-005-moot-per-scope-revision)
- **CLOSED PRIOR: 1** (K-003 — cross-reference drift; verify_session_gone removed by PR #124 closed K-003 by side-effect, but no Resolution-block added at the time)
- **Refinements noted: 2** (PRE-L-004 conditional on scope-revision cascade-item #6; PRE-L-005 moot per I-001 won't-fix)

### K-003 drift detail

K-003's finding text: "**Severity:** Linked to C-001 / **Fix:** Tracked in C-001." This is the **cross-reference drift** shape — when C-001 closed via PR #124 (W1.4), the verify_session_gone removal satisfied K-003's REMOVE clause too, but no explicit Resolution-block was added to K-003 at the time. Session 3b's verification (`grep verify_session_gone` → 0 matches in `crates/etw/src/session.rs`) caught the missing Resolution and added it.

**Drift-shape classification (expanding the engagement's taxonomy):**
- **Genuine audit-vs-codebase drift** (PRE-L-006, E-001): audit said "open" when code had already addressed the gap, possibly hours before the audit was written
- **Audit-state-progression drift** (F-004): audit correctly said "needs verification"; later session performed the verification
- **Cross-reference drift** (K-003): one finding's closure should have triggered another's but didn't; the linked finding sat stale until later sweep
- **Pre-push consistency catch** (cascade-2 numbering): caught at PR-push time by author's own grep-verify discipline, before the audit got the chance to drift

All four shapes count for drift-rate accounting but qualitatively differ on where intervention is most valuable.

### M1.13 backfill cumulative state (4 sessions)

| Session | Findings | VERIFIED OPEN | CLOSED | Drift type |
|---|---|---|---|---|
| 1 | 10 | 10 | 0 | — |
| 2 | 9 | 9 | 0 | — |
| 3a | 8 | 7 | 1 (F-004) | Audit-state-progression |
| 3b | 7 | 6 | 1 (K-003) | Cross-reference |
| **Total** | **34** | **32** | **2** | |

**Engagement cumulative drifts: 5 in ~100 actions (~5.0%)**, within variance of 4.5% base-rate prediction:
1. PRE-L-006 (genuine, inline-catch during Phase 3 drafting)
2. E-001 (genuine, META 1 sweep during W1.5)
3. Cascade-2 M1.13 numbering (pre-push catch during cascade application)
4. F-004 (audit-state-progression, M1.13 session 3a)
5. K-003 (cross-reference, M1.13 session 3b)

### Hypothesis test result (E/G/pre-L drifts more than A/B): NOT confirmed

By axis drift count / verified count:
- A: 0/5, B: 0/4, C: 0/3, D: 0/5, E: 0/4
- F: 1/2 (F-004 — designed-to-be-verified shape)
- G: 0/3, H: 0/1, I: 0/1
- K: 1/2 (K-003 — cross-reference shape)
- Pre-L: 0/5

Drifts clustered on **cross-reference + designed-to-be-verified shapes**, NOT on diffuse-claim axes. The pre-session-2 hypothesis was reasonable but the engagement didn't validate it. Future audit cycles should track drift-shape distribution; the lesson here is that linked findings and status-check findings are more likely to drift than diffuse claims (which usually sit in known-unbuilt territory and don't move).

### P4.9 process improvement validated end-to-end

P4.9's premise: open-classification is a weaker guarantee than closed-classification; backfill verification commands belong inline in the audit file.

Validation outcome:
- ~5% catch rate is meaningful — not so high it overwhelms (would suggest the audit is unreliable) and not so low it's noise (would suggest the verification isn't valuable)
- The catches were qualitatively diverse (4 distinct drift shapes)
- The inline annotations now ground every still-open finding in a verifiable command, making FUTURE audit cycles cheaper (they can re-run the verification commands to spot-check current state without re-deriving them)

**P4.9 lesson for future audit cycles:** run the verification at issue-creation time, NOT as a separate post-hoc sweep. M1.13 was the reference implementation; the discipline transfers to every future Phase 2 audit pass.

### Issue #127 closed

`gh issue close 127` executed with full state-rapport comment (cross-referencing this PR + the 4 session PRs + the 4 buddy-log entries).

### Review-surface honesty

**What was actually done:**
- 7 findings verified using multi-step verification on pre-L axis per Frank's session-2/3 guidance.
- K-003 drift caught via cross-reference grep (`grep verify_session_gone` in cited file → 0 matches; C-001 closure side-effect).
- Status summary updated to reflect K-003 closure (23 → 22 actionable open).
- M1.13 finalization block added documenting all 4 sessions' totals + drift-shape taxonomy.
- **No mobile-Claude review** per established cascade-pattern.
- Self-attestation Q1–Q5 in PR body.

**Defects fanget:** K-003 closure (counted as drift; cross-reference shape).

**Verdict:** GO. PR #145 merged as commit `ce4688c`. M1.13 backfill complete; issue #127 closed.

### Final M1.13 engagement summary

M1.13 / P4.9 backfill is now reference instance for the project's audit-discipline toolkit. The 4-session structure (with state-rapport between each) is generalizable to future audit cycles. The buddy-log format ("review-surface honesty" + per-session results table + drift-shape taxonomy) is the durable audit-trail for the P4.9 process improvement.

**Awaiting Frank's direktiv on next reelt arbeidsstykke.** Sannsynlig kandidat per mobile-Claude's pre-M1.13 anbefaling: Group A week 3 (ETW parsers) — substantielt kode-arbeid. Frank's call.

---

## W1.10 (issue #89) — 2026-07-30 — Week 1 buddy review — verdict GO

**Format:** 4Q planning-phase. Reviewer: Claude (agentic session, PRs #153/#154 author — self-review with grep-verified evidence per the P4.9 "verification at review time" discipline; each claim below cites a re-runnable check).

**(a) Do the W1.1–W1.6 closures match the roadmap's spec?**
- W1.1 / W1.7 / W1.8 / W1.9 — WON'T-FIX per scope revision (PR #129); struck through in the roadmap itself. Match.
- W1.2 (A-003) — `grep bf6.exe crates/sys/src/inner/ac_detect.rs` hits only the doc-comment explaining the removal; the Javelin marker list is empty with a regression-guard test. Match.
- W1.3 (A-004) — the consumer-loop Arc::as_ptr lifetime contract comment is present in `crates/etw/src/session.rs` (tagged "W1.3 / closes A-004"). Match.
- W1.4 (C-001) — `grep verify_session_gone crates/` hits only `crates/spike-etw` (the spike, out of scope); `EtwSession::stop()` carries the "deliberately NOT re-querying" rationale block. Option (a) as recommended. Match.
- W1.5 (E-001) — `arm_panic_in_process_trace` infrastructure exists and the Mode-5 supervisor test drives the synthetic panic through the oneshot. Match in substance; the test lives at the supervisor level rather than under the exact name the roadmap sketched.
- W1.6 (F-001/F-002) — Option B shipped: `crates/tray/src/tabs/sessions.rs` exists and onboarding carries the required "EDR validation in progress for v0.7.1" substring. Match.

**(b) Are W1.2/W1.3/W1.4/W1.5 atomic enough for separate PRs?** Historically landed as separate commits/PRs during the Week-1 wave; atomicity honored.

**(c) Is Option B the intended W1.6 choice?** Yes — confirmed by the shipped scaffold + the roadmap's own recommendation.

**(d) External-dep kickoffs documented?** Moot — W1.7–W1.9 are WON'T-FIX per scope revision; no EXTERNAL-DEADLINES.md needed. **(e) New Week-1 items surfaced?** None beyond what the 2026-07-30 #148 audit later caught (logged separately in that issue).

**Verdict: GO.** Week 1 closed.

## M1.15 (issue #101) — 2026-07-30 — Month 1 buddy review — verdict GO with one exception

**Format:** 4Q planning-phase. Reviewer: Claude (same session; author of the #153/#154 closures — findings below grep-verified, not asserted from memory).

**(a) Do M1.1–M1.11 closures match the roadmap's spec?**
- M1.1 (A-001) — one-shot APIs return typed `AlreadyStoppedError`; no `.expect` misuse panics remain on the explicit paths (PR #154). Match.
- M1.2 (B-001) — `start_closed_loop_if_enabled` wrapped in `spawn_blocking`, JoinError → `StartupError` (PR #154). Match.
- M1.3 (B-002) — source-level watchdog-exclusion tests (`watchdog_exclusion_tests` in runtime.rs) pin the select! contents (PR #154). Match.
- M1.4 (B-003) — `docs/syscall-seam-pattern.md` + README Building-section reference (PR #154). Match.
- M1.5 (C-002) — shared `torn_down` flag closes the drop-poll/shutdown race, unit-tested (PR #154). Match.
- M1.6 (C-004+D-006) — six §2.11 matrix cells as engine tests incl. 50× rapid-cycle stress (PR #154). Match, with the caveat that game-mode *planning* is platform-gated, so the cells pin the state machine + ownership guard; live plan/apply remains Windows-runtime-batch territory.
- M1.7 (D-001) — `assert_impl_all!(RealEtwSysCalls: RefUnwindSafe)` + documented mock exemption (PR #154). Match.
- M1.8 (F-003) — multi-line error banner renders first line + collapsible details (PR #154). Code-complete; **visual verification outstanding** (no Windows GUI in the authoring environment) — fold into the next Windows runtime batch.
- M1.9 (F-004) — verified zero residual `std::sync::Mutex` in `crates/tray/src`; closed as completed. Match.
- M1.10 (I-004) — **OPEN.** Requires a clean Windows VM + `icacls` capture; cannot be executed from a Linux agent session. This is the exception.
- M1.11 (J-003) — README "Power-user workflows" section with CLI + OBS examples (PR #153); hotkey honestly documented as planned. Match.
- M1.13 / M1.14 — closed earlier (PR #145 backfill; PR #153 CLI verb).

**(b) External-dependency clocks tracking?** W1.8 cert / W1.1 AC outreach are WON'T-FIX per scope revision. W1.9/M2.2 EDR matrix remains the live external clock for the v0.7.1 default-on flip (M3.5) — unchanged status, still Frank-owned.

**(c) Findings demoted/promoted from real-world behavior?** Two promotions out of the #148 audit wave: G1 (affinity-path denylist bypass) and G2 (missing AC-host denylist names) — both fixed and merged in #153. One stale finding demoted/closed by verification: A-006/G-003 (#106) was already resolved by #152's `OnceLock` bundled list.

**(d) Month 2 sequencing — K-005-first still correct?** No contrary evidence surfaced; the small M2 items (M2.3–M2.6) are already closed via #153, which if anything strengthens the case for tackling K-005 next with a clean slate.

**Verdict: GO with one exception — Month 1 is closed except M1.10 (issue #99), which stays open pending Windows-VM verification.** Also carried forward: M1.8's visual check in the next Windows runtime batch.

## How to use this log going forward

Each new buddy review that surfaces a concern gets a new
numbered entry. Entries are append-only — never edited after the
user reviews and resolves them, so the audit trail stays
trustworthy. When an entry is resolved, append a "**Resolution**"
subsection at the bottom of that entry rather than rewriting the
body.
