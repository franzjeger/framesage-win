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

## Entry 6 — 2026-05-18, buddy review M3.7: Month 3 + v0.7.1 tag (5Q implementation-phase format)

### Context

Issue #116 (roadmap item M3.7) calls for a 5Q implementation-phase
buddy review against the v0.7.1 tag candidate. The five questions
are issued by the planning stage of the issue and reproduced
verbatim here for traceability:

1. Language mechanics — Group B/C code passes `cargo clippy -- -D
   warnings` + the audit's `[UNVERIFIED]` sweep (P4.8)?
2. Concurrency — drop-poll + supervisor + recorder + PresentMon
   subprocess interactions deadlock-free?
3. Errors — every Win32 path has a documented error mapping?
4. Test honesty — the five Group C threshold tests assert verbatim
   substrings (not paraphrased)? The negative-session screenshot is
   real?
5. Consistency — architecture §2.4 honesty-contract substrings +
   onboarding-page-3 required substrings + Sessions tab empty-state
   substrings all match their spec word-for-word?

**Buddy ran this review against main at `6f972b6` (PR #79 squash —
Group A week 2 ETW implementation).** Verification commands and
literal output are included per Phase 3 ground rule (architecture
§ "New ground rule: spike reports include verification commands +
literal output").

---

## (1) Language-mechanics safety: FAIL

**Group A (crates/etw + crates/service): PASS on language mechanics.**

Every `unsafe` block in the reviewed code has an adequate `//
SAFETY:` comment naming preconditions and satisfaction:

- `build_gate.rs:120` — `RtlGetVersion(&mut info)`: SAFETY names the
  writable-local precondition, size-of population, aliasing
  impossibility. Adequate.
- `session.rs` (multiple sites) — `StartTraceW`, `ControlTraceW`,
  `OpenTraceW`, `ProcessTrace`, `CloseTrace`: each site delegates to
  "caller contract per trait method docs" and the trait's own `#
  Safety` doc explains all FFI preconditions. Adequate.
- `EtwSessionPropertiesBuffer::new` (`zeroed()`): "zero-init gives a
  valid empty EVENT_TRACE_PROPERTIES." Adequate.
- `event_record_callback`: two dereferences — pointer non-null check
  precedes each. Adequate.
- `MockEtwSysCalls::control_trace` QUERY write-back: "caller passed a
  valid EVENT_TRACE_PROPERTIES per trait contract." Adequate.

Visibility matches consumer location: all tests are inline
`#[cfg(test)] mod tests`, not in `crates/etw/tests/`. This is
correct — the plan §3.4's `pub(crate)` helpers (BuildOverrideGuard,
MockEtwSysCalls) are only visible from inline tests, and there are no
integration tests in a `tests/` subdirectory that would fail to see
them.

Send+Sync bounds: `EtwSession<S: Clone + Send + 'static>` captures
`S` by move into the consumer-thread closure. `Arc<ConsumerState>` is
`Send + Sync` because `ConsumerState` contains only `AtomicU64`.
`static_assertions::assert_impl_all!(ConsumerState: RefUnwindSafe)`
in supervisor.rs catches future regressions at compile time.

`[UNVERIFIED]` sweep: no `[UNVERIFIED]` markers remain in any
Group A code or documentation. All eight `mac-side-uncertainties.md`
entries are marked "Resolved" with literal verification-command
output. Verification:

```
$ grep -rn "UNVERIFIED" crates/etw crates/service spike/mac-side-uncertainties.md
(no output)
```

**FAIL reason: Group B and Group C code does not exist.**

The question asks about "Group B/C code." Verification:

```
$ ls crates/
cli  core  engine  etw  gamemode  ipc  service  sim  spike-etw  sys  tray
```

There is no `crates/recorder`, `crates/presentmon`, or equivalent.

```
$ grep -rn "compute_attribution\|attribution_summary\|PresentMon\|presentmon" crates/ \
    --include="*.rs" | grep -v "target" | grep -v "etw-edr-report"
crates/service/src/closed_loop.rs:203:    // Group C UI banner consumer.
crates/etw/src/session.rs:621:        /// service can surface it in logs and (Group C) the UI banner.
crates/etw/src/degradation.rs:16:/// `EtwSubsystem::Disabled(_)`) and the UI (Group C banner / Status
```

PresentMon spawn, session recorder, and compute_attribution_summary
are referenced only in comments describing future Group C wiring.
None is implemented. The language-mechanics check cannot be performed
on code that does not exist — and its absence is itself the finding.

---

## (2) Concurrency correctness: PARTIAL — see below

**Drop-poll interval task and supervisor: PASS.**

- `spawn_closed_loop_tasks` in `crates/service/src/closed_loop.rs`
  spawns two independent tokio tasks. The supervisor task owns
  `SessionShutdownHandle` (holds `syscalls: Option<S>`); the
  drop-poll task owns `MonitorHandle` (holds a clone of `syscalls`
  + a clone of `Arc<ConsumerState>`). These are non-overlapping
  ownership regions.
- The drop-poll interval calls `MonitorHandle::poll_drop_stats` which
  calls `query_session_stats` (→ `ControlTraceW(QUERY)`). The
  supervisor task calls `shutdown.shutdown()` (→
  `ControlTraceW(STOP)`) only on the consumer-panic path. Both paths
  hold their own `syscalls` clone and issue independent Win32 calls.
  The Win32 session name is the unique ETW session key; concurrent
  QUERY + STOP on the same session name is explicitly permitted by
  the ETW API (QUERY is read-only; STOP is idempotent on
  already-stopped sessions via ERROR_WMI_INSTANCE_NOT_FOUND
  tolerance).
- No shared mutable state between the two tasks. No lock required.
- `ConsumerState::events_seen` is `AtomicU64` with `Ordering::Relaxed`
  — correct for an independent monotone counter with no cross-counter
  ordering requirements.
- `exit_tx` (oneshot Sender) lives exclusively in the consumer-thread
  closure and is consumed once on exit. No contention possible.

**Recorder + PresentMon subprocess: N/A — not implemented.**

Architecture §2.2 describes PresentMon as a child process managed by
the session recorder. The recorder does not exist (see Q1). The
deadlock analysis for recorder ↔ PresentMon process lifecycle
interactions, I/O drain, and crash-restart logic cannot be performed.

**STOP finding on concurrency:** the v0.7.1 tag requires Group B
(PresentMon + recorder) per Phase 3 acceptance criteria
(`audit/v0.7-architecture.md` §1582). Without the recorder and
PresentMon wiring, concurrency Q2 is unresolvable for the Group B
scope.

---

## (3) Error handling: PASS (Group A scope)

Win32 paths and their documented error mappings in the reviewed code:

| Call site | Win32 error | Disposition |
|---|---|---|
| `start_trace` / `StartTraceW` | `ERROR_ACCESS_DENIED` (5) | `Disabled(AccessDenied)` — explicit match, no bail |
| `start_trace` / `StartTraceW` | `ERROR_ALREADY_EXISTS` | `Disabled(AlreadyExists)` — explicit match |
| `start_trace` / `StartTraceW` | any other non-SUCCESS | `bail!` with hex code + human hint |
| `control_trace` / `ControlTraceW` (STOP path) | `ERROR_WMI_INSTANCE_NOT_FOUND` | treated as success (already gone) |
| `control_trace` / `ControlTraceW` (STOP path) | any other non-SUCCESS | `bail!` |
| `control_trace` / `ControlTraceW` (QUERY path) | any non-SUCCESS | `bail!` |
| `open_trace` / `OpenTraceW` | `handle.Value == u64::MAX` | `bail!` |
| `process_trace` / `ProcessTrace` | `ERROR_CANCELLED` (1223) | clean-shutdown path (explicit check) |
| `process_trace` / `ProcessTrace` | any other non-SUCCESS | `bail!` |
| `close_trace` / `CloseTrace` | non-SUCCESS | `tracing::warn!` (non-fatal — already shutting down) |
| `RtlGetVersion` | `status.0 < 0` (NTSTATUS) | `Disabled(BuildUnsupported { detected_build: None })` |

No `.ok()` swallows on fallible Win32 calls. No `.unwrap()` in the
consumer thread outside the `catch_unwind` boundary. The Drop impl
for `EtwSession` and `SessionShutdownHandle` both emit `WARN` on
cleanup failure with a `logman stop` remediation hint — appropriate
severity for a best-effort path.

`tracing_test` subscriber assertions in `crates/service/src/closed_loop.rs`
correctly assert against structured fields
(`reason="policy_opt_out"`, `reason="build_unsupported"`) rather than
free-text substrings. Correct per user's Day 5 guidance.

---

## (4) Test-asserts-what-it-says: FAIL

**Group A tests: PASS.**

Each test name maps cleanly to its assertion:

- `predicate_true_at_synthetic_build_at_or_above_threshold` →
  asserts `closed_loop_enabled_for_this_build() == true`. ✓
- `predicate_false_at_synthetic_build_below_threshold` →
  asserts `!closed_loop_enabled_for_this_build()`. ✓
- `predicate_false_on_synthetic_rtlgetversion_failure` →
  asserts `detected_build() == None`. ✓
- `mode_1_access_denied_returns_disabled` → asserts
  `matches!(result, EtwSubsystem::Disabled(DegradationMode::AccessDenied))`. ✓
- `mode_6_build_unsupported_short_circuits_before_any_etw_call` →
  asserts variant shape + `detected_build == Some(22631)`. ✓
- `supervisor_emits_consumer_panic_event_and_calls_shutdown` →
  asserts `events[0].mode == ConsumerPanic` and
  `events[0].detail.contains("synthetic test panic")`. ✓
- `mode_5_session_level_full_flow_panic` → full-flow test asserting
  `Panicked` reason + `ConsumerPanic` event + literal panic payload
  in detail. ✓

**FAIL: five Group C threshold tests do not exist.**

Architecture §2.4 (line 1164 in `audit/v0.7-architecture.md`) and
Phase 3 Group C acceptance criteria (line 1631) require:

```
compute_attribution_summary(session_with_p99_delta(-9%))
  → rendered string contains "improved your 1% lows"
compute_attribution_summary(session_with_p99_delta(-6%))
  → rendered string contains "Modest improvement"
compute_attribution_summary(session_with_p99_delta(0%))
  → rendered string contains "No measurable effect"
compute_attribution_summary(session_with_p99_delta(+4%))
  → rendered string contains "Slight regression"
compute_attribution_summary(session_with_p99_delta(+6%))
  → rendered string contains "**degraded**" verbatim
```

Verification:

```
$ grep -rn "compute_attribution\|improved your 1%\|Modest improvement\
|No measurable effect\|Slight regression\|degraded" \
    crates/ --include="*.rs" | grep -v "target"
(no output)
```

`compute_attribution_summary` does not exist. The five threshold
tests do not exist. The verbatim-substring requirement for
`"**degraded**"` (the critical anti-euphemism guard) is unmet.

**FAIL: negative-session screenshot does not exist.**

The Group C acceptance criterion (architecture §1629) requires:
"Negative-result session intentionally produced (apply a bad profile)
and verified that the UI shows red attribution banner." No screenshot
file exists in the repository at any path matching that criterion:

```
$ find . -name "*.png" -o -name "*.jpg" | grep -i "session\|negative\|bad" | grep -v target
(no output)
```

---

## (5) Internal consistency: FAIL

**Architecture §2.4 honesty-contract substrings:** unverifiable
because `compute_attribution_summary` does not exist. The required
string table (§2.4 lines 1143–1147) specifies five output strings;
none is asserted anywhere in the codebase.

**Onboarding page 3 required substrings:**

Architecture §2.1 mode 6 (line 343) and §1354 "First-run
onboarding — new closed-loop opt-in page" describe a new page 3 (the
closed-loop opt-in page inserted between current pages 2 and 3).
Implementation note (§1416): "The 'EDR validation in progress for
v0.7.1' line is a required substring per Group C acceptance criterion.
Reviewer rejects a PR that ships page 3 without it."

Current `crates/tray/src/onboarding.rs` has four pages (0–3), not
five. Page 3 in the current implementation is the "Ready to go"
confirmation page, not the closed-loop opt-in page. Verification:

```
$ grep -n "page 3\|page_three\|render_page_three\|Measure whether\
|EDR validation\|closed.loop.*opt\|ClosedLoopChoice" \
    crates/tray/src/onboarding.rs
346:fn render_page_three(ui: &mut egui::Ui, next: &mut Option<NextAction>) {
348:    ui.label(theme::section_heading("Manual Game Mode"));
```

`render_page_three` renders the existing "Manual Game Mode" page, not
the new closed-loop opt-in page. The `ClosedLoopChoice` enum and
"Measure whether rules helped" heading are absent. The required
substring "EDR validation in progress for v0.7.1" is absent.

**Sessions tab empty-state substrings:**

Architecture §2.4 specifies two empty-state variants:

1. **Unsupported-build state (build < 26100):** must contain
   `"requires Windows 11 24H2 or later"` and must NOT contain
   `"After your first 90-second gaming session"`.
2. **No-sessions-yet state (build ≥ 26100, `closed_loop_enabled:
   true`):** must contain `"After your first 90-second gaming
   session"`.

The Sessions tab does not exist in `crates/tray/src/main.rs`.
Verification:

```
$ grep -n "Sessions\|session.*tab\|After your first\
|requires Windows 11 24H2" crates/tray/src/main.rs
1944:                    stats.game_mode_sessions,
1945:                    "Game Mode sessions",
```

No Sessions tab component. No empty-state strings. No "Enable…"
button. The architecture §2.4 spec is entirely unimplemented.

---

## Overall verdict

**STOP-ON-MULTIPLE.**

Three independent blocking gaps; none is a documentation/naming slip:

1. **Group B and C implementations missing.** The v0.7.1 tag
   presupposes Group A (done), Group B (PresentMon + recorder), and
   Group C (Sessions UI + attribution). Neither Group B nor Group C
   code exists. The language-mechanics check, the recorder/PresentMon
   concurrency analysis, the five threshold tests, and the Sessions
   tab substring verification all block on this.

2. **Onboarding page 3 (closed-loop opt-in) not implemented.**
   Current `crates/tray/src/onboarding.rs` has 4 pages identical to
   the v0.6 wizard. The new page 3 ("Measure whether rules helped")
   with the required `ClosedLoopChoice` enum and the load-bearing
   substring "EDR validation in progress for v0.7.1" is absent. A
   v0.7.1 ship without this page launches with `closed_loop_enabled`
   defaulting to `false` (architecture §1664 resolution) but with no
   onboarding path to opt in, which breaks the user-facing contract.

3. **Five Group C threshold tests absent.** The verbatim-substring
   requirement for `compute_attribution_summary` (critical
   anti-euphemism guard for the `"**degraded**"` case) is the
   architecture's stated mitigation for the "attribution UI failing
   the honesty test" risk (§1444). Without these five tests the
   v0.7.1 merge gate is structurally open to a developer under
   deadline smoothing a negative delta and passing CI.

**Q3 (error handling) is PASS** for the Group A scope reviewed. No
swallows, no severity mismatches, all Win32 codes documented.

**Q2 (concurrency) is PASS** for the drop-poll + supervisor
subsystem reviewed. The recorder + PresentMon interactions cannot be
verified because the code does not exist.

**Q1 (Group A language mechanics + UNVERIFIED sweep) is PASS** for
the existing implementation. All SAFETY comments are adequate,
visibility matches consumer location, Send+Sync bounds are correct,
no `[UNVERIFIED]` markers remain in code or docs.

---

## Category-of-issue observation

Findings 1–3 are all in the same category: **implementation scope
gap** — the v0.7.1 tag review arrived before the Phase 3 Group B and
C deliverables were built. This is distinct from prior entries (which
were documentation-consistency and API-shape issues within an
existing deliverable). The watch-signal criteria (count, severity,
same-category-as-prior-round) do not apply here because this is the
first 5Q code review; it surfaces a scheduling/scope gap, not a
quality-regression pattern.

**What the user needs to decide:**

| # | Question | Options |
|---|---|---|
| 1 | Group B (PresentMon + recorder) and Group C (Sessions UI + attribution) scope: are these required for the v0.7.1 tag, or does v0.7.1 ship as Group-A-only with the default-on-flip? | (A) v0.7.1 = Group A only; B/C deferred to v0.8. Remove B/C from v0.7.1 acceptance criteria. (B) v0.7.1 requires B+C. Start Group B. |
| 2 | Onboarding page 3: required for any v0.7.1 candidate regardless of B/C decision? | (A) Yes — any build tagged v0.7.1 must have the opt-in page. (B) Defer to when B/C lands (user can't opt in to something that doesn't record yet anyway). |
| 3 | Five threshold tests: Group C acceptance criterion per architecture. Required before any Group C code ships. | Not a decision — these block the Group C merge gate by definition. Whoever writes `compute_attribution_summary` also writes the five tests. |

Primary-agent holds execution on all v0.7.1 tag work pending user
decisions on (1) and (2).

---

## How to use this log going forward

Each new buddy review that surfaces a concern gets a new
numbered entry. Entries are append-only — never edited after the
user reviews and resolves them, so the audit trail stays
trustworthy. When an entry is resolved, append a "**Resolution**"
subsection at the bottom of that entry rather than rewriting the
body.
