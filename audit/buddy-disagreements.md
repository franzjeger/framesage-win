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

## How to use this log going forward

Each new buddy review that surfaces a concern gets a new
numbered entry. Entries are append-only — never edited after the
user reviews and resolves them, so the audit trail stays
trustworthy. When an entry is resolved, append a "**Resolution**"
subsection at the bottom of that entry rather than rewriting the
body.
