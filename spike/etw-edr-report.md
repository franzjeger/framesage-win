# v0.7 Phase 2 — EDR Interaction Report

**Status:** **GAP REPORT — not the final EDR validation.**
**Approved as the Phase 2 deliverable on 2026-05-17 with Option D
selected (hybrid).** Group A weeks 2-7 unblock immediately; the
EDR matrix becomes a v0.7.1 default-on-flip gate, not a v0.7
ship gate.

This document surfaces what we can and cannot test from the
current dev environment, records the resolution chosen, and
locks in the validation criteria for the v0.7.1 flip *before*
v0.7.1 schedule pressure starts skewing judgment.

The architecture (`audit/v0.7-architecture.md` Section 2.1 →
"EDR interaction — TESTING IS LOAD-BEARING, NOT OPTIONAL") and
the ground rule attached to PR #67 both prohibit handwaving on
this. So rather than fabricate results from a single environment
we don't even fully control, this report documents the gap
explicitly, records the user-approved path forward, and
specifies what evidence will close the gap in v0.7.1.

---

## 1. What was tested on env-1 (this dev box)

**Hardware:** the same dev box used for the Phase 1 ETW spike
and the schema-research week (`spike/etw-schemas.md`).

**OS:** Windows 11 Pro for Workstations, version 10.0.26200,
UBR 8457. Build 26200 is above the v0.7 minimum (Decision 1
build gate, 26100), so closed-loop subsystem would be enabled
on this machine in a v0.7 release.

**Spike binary tested:** `target\release\spike-etw.exe` —
unsigned, run elevated (LocalSystem-equivalent via PowerShell
`Start-Process -Verb RunAs`).

### What happened

| Question | Result on env-1 |
|---|---|
| Did the unsigned binary launch? | Yes. No SmartScreen prompt. |
| Did `StartTraceW` succeed? | Yes, on every run during schema week (≥ 6 full sessions of 60 s and one 30 s collision test). |
| Did `ProcessTraceW` block-and-deliver events? | Yes. ~19K events/sec idle, ~80K events/sec gaming-load. |
| Did any AV alert fire? | None observed. |
| Was the spike process terminated mid-run? | No. All sessions ran to completion. |
| Were any binary writes / on-disk artifacts flagged? | No quarantine entry, no `Get-MpThreatDetection` history (because the cmdlet provider is also absent — see §2). |

**These results look great. They are also unreliable as a
Defender baseline. §2 explains why.**

---

## 2. Why env-1 is NOT a representative Defender test

This is the gap-surfacing portion of the report. The previous
Phase 1 spike report (`spike/etw-report.md` §Q1) noted in
passing that "Defender itself is running" on env-1. That
claim does not survive a closer look on 2026-05-17.

### Live state on env-1, captured 2026-05-17

```text
=== OS build ===
Caption:      Microsoft Windows 11 Pro for Workstations
Version:      10.0.26200
BuildNumber:  26200
UBR:          8457

=== WinDefend service ===
sc.exe query WinDefend
  → [SC] EnumQueryServicesStatus:OpenService FAILED 1060:
    The specified service does not exist as an installed service.

=== MsMpEng process ===
Get-Process MsMpEng  →  (not running)

=== Defender status ===
Get-MpComputerStatus  →  (returned nothing — cmdlet has no provider
                          because the Defender platform isn't installed)

=== SecurityCenter2 ===
Get-CimInstance -Namespace root\SecurityCenter2 -ClassName AntiVirusProduct
  displayName:   Windows Defender
  productState:  0x62100
  instanceGuid:  {D68DDC3A-831F-4fae-9E44-DA132C1ACF46}
```

### Interpretation

- **`WinDefend` service does not exist.** This is unusual — on
  a stock Win11 install, `WinDefend` is the AM service that
  hosts `MsMpEng.exe`. Its absence means Microsoft Defender
  Antivirus has been removed or disabled at a platform level on
  this machine. Possibilities include: a Microsoft Defender for
  Endpoint downgrade scenario, a Win11 Pro for Workstations
  install with a tampered AV stack, or a previous third-party
  AV install that uninstalled `WinDefend` and was later
  uninstalled itself without restoring it.
- **`MsMpEng.exe` is not running.** Consistent with the service
  being absent.
- **`Get-MpComputerStatus` returns nothing.** The `Defender`
  PowerShell module needs the platform binaries present to
  report status; their absence is consistent with the service
  being uninstalled.
- **`SecurityCenter2` still lists "Windows Defender" with
  productState `0x62100`.** Decoded: byte layout for
  `productState` per [Microsoft's security-center docs] is
  `(provider, scanner_state, definition_state)`; `0x62100`
  decodes to "AV provider present in registry, real-time
  protection OFF, definitions out-of-date". This is a stale
  registry advertisement, not a running engine. (Security
  Center can hold onto vestigial AV registrations after a
  product is removed.)

[Microsoft's security-center docs]: https://learn.microsoft.com/en-us/windows/win32/api/iwscapi/

**Bottom line: there is effectively no real-time AV running on
env-1.** The Phase 1 report's claim that "Defender itself is
running" was wrong on this machine as of 2026-05-17 — possibly
wrong then too, possibly the Defender platform was removed in
the interim.

### What this means for the EDR matrix

> **"Spike binary ran clean on env-1" tells us nothing about
> how Defender ATP, CrowdStrike Falcon, or SentinelOne
> Singularity will treat the production v0.7 service.**

It is a useful negative result for *bare-OS-with-no-AV*
behavior (the spike doesn't crash, doesn't leak handles,
doesn't trigger the OS's own kernel-event integrity checks),
but the architecture explicitly demands behavioral validation
against three EDR products. We have zero of three.

---

## 3. Access gap — what we can't test from here

| Product | Status | Blocker |
|---|---|---|
| **Microsoft Defender for Endpoint** | NOT TESTABLE on env-1 — the Defender platform isn't installed, and standalone Defender (the consumer AV) is not the same as Defender ATP (the EDR product). Defender ATP requires a Microsoft 365 E5 license or a 90-day E5 trial. | No E5 trial currently active. Setting one up requires (a) creating a new tenant or running against an existing one, (b) enrolling at least one device, (c) waiting for behavioral telemetry to settle (≥ 24 h). |
| **CrowdStrike Falcon** | NOT TESTABLE. Trial is 15 days but requires sales-call qualification per [falcon.crowdstrike.com/trial]. The "free trial" link routes to a sales contact form, not a self-service download. | No active CrowdStrike trial. No CrowdStrike business contact on file. |
| **SentinelOne Singularity** | NOT TESTABLE. Same pattern as CrowdStrike — the free-trial CTA on sentinelone.com routes to "Request a Demo", which is sales-call-gated. | No active S1 trial. No S1 sales relationship. |

[falcon.crowdstrike.com/trial]: https://www.crowdstrike.com/products/trials/try-falcon-prevent/

**Compounding factor: no clean Win11 VM available on env-1.**
The architecture's testing model is "spin up a clean Win11 VM
in Hyper-V or VMware Workstation, install one EDR at a time,
snapshot between runs." Env-1 has neither Hyper-V enabled
(disabled at firmware level for unrelated GPU passthrough
reasons) nor VMware Workstation installed. Standing this up
in-house is on the order of half a day for Hyper-V (enabling
virt-in-firmware + restoring GPU passthrough) plus the EDR
trial-procurement bottleneck above.

---

## 4. Options to close the gap

Per Phase 2 sign-off Decision 2, this is a user-decision
moment. Three viable paths:

### Option A — Anthropic alignment + security-community outreach

Per architecture §2.1 "Fallback if a product is genuinely
unobtainable": reach out via Discord / X / LinkedIn to
security researchers or SOC analysts who routinely operate
their own EDR stacks. Many will run a 30-minute test as a
favor.

- **Cost:** 0 USD. ~2-3 days of social-outreach lead time, plus
  the testers' own time (cap promised at 30 min per product).
- **Risk:** quality-of-evidence varies — a researcher will not
  necessarily be willing to run the binary on a tenant they
  care about. We may get screenshots of policy alerts but not
  full hunting-query telemetry.
- **Recommended contacts to pursue, in order:**
  1. The [PerfView issue tracker maintainers] — they routinely
     receive EDR-flag reports against PerfView (which uses the
     same SystemTraceProvider API as our spike) and may have a
     standing test rig.
  2. SANS GIAC instructors who teach EDR-evasion / blue-team
     curricula — they typically operate multi-EDR home labs.
  3. The [Sysinternals + Process Hacker Discord communities]
     — Process Hacker uses identical APIs and gets flagged
     constantly; testers there have direct experience.

[PerfView issue tracker maintainers]: https://github.com/microsoft/perfview/issues
[Sysinternals + Process Hacker Discord communities]: https://discord.gg/sysinternals

### Option B — Pay for a per-product trial setup

Budget the trial procurement explicitly:

| Cost item | Estimate |
|---|---|
| Microsoft 365 E5 trial (90 days, $0 if first trial) | 0 USD |
| CrowdStrike Falcon Go (cheapest paid SKU, 30-day) | ~150 USD/month, 1 month |
| SentinelOne Singularity Core (cheapest paid SKU) | ~50 USD/endpoint/year, billed monthly ≈ 5 USD/month, minimum 5 endpoints |
| Azure / AWS Win11 VM for testing rig | ~25 USD for 2 engineer-days of VM time |
| **Engineer time** | ~3 days at full focus |

Total: ~200-300 USD plus 3 engineer-days. This is the path the
architecture's "Total cost: 2 engineer-days" estimate
implicitly assumed (but that estimate predates the
trial-procurement reality check above).

### Option C — Defer the EDR matrix to a v0.7.1 patch ship

Ship v0.7 with the closed-loop subsystem **disabled by
default** (matches Decision 5 / Phase 2 sign-off: the
`closed_loop_enabled` policy defaults to `false`), document
the EDR-validation gap loudly in the README, and gate
turning it on by default until v0.7.1 once the matrix is
populated.

- **Cost:** zero up-front. Shifts the work to v0.7.1 release
  prep but doesn't block Group A implementation.
- **Risk:** the closed-loop subsystem is the marquee v0.7
  feature. Shipping it disabled-by-default means users who
  install v0.7 and don't read release notes get the v0.6
  experience. We've avoided that pattern explicitly elsewhere
  ("we are not in the business of inert features"). This option
  contradicts the v0.7 narrative.

### Option D — Hybrid: ship Option C while running Option A

Ship v0.7 with closed-loop default-off + the EDR gap
documented (Option C), AND start the community outreach in
parallel (Option A). When two of three products are validated
via Option A, flip the default to ON in a v0.7.1 patch ship.

This is the **recommended path** because it (a) unblocks Group
A implementation today, (b) doesn't burn cash before knowing
whether free-community testing will work, (c) doesn't ship
unverified EDR claims, and (d) gives v0.7 users the choice to
opt in if they know their EDR posture.

---

## 5. What Group A implementation can / cannot rely on

**Can rely on (validated):**
- Private-session `StartTraceW` with unique GUID works on
  Win11 24H2 (build 26200) — Phase 1 spike empirical
- Kernel-event opcodes per `spike/etw-schemas.md` — schema
  authority validated against MSDN MOF + empirical histogram
- `closed_loop_enabled` policy gate works as a hard kill
  switch (no ETW session ever opened when false) — Phase 1
  spike validated the "don't even call StartTraceW" branch

**Cannot rely on (gap-blocked):**
- "Defender ATP doesn't flag us" — UNVALIDATED on the actual
  Defender platform (env-1 has no Defender installed)
- "CrowdStrike Falcon doesn't kill our process" — UNVALIDATED
- "SentinelOne Singularity doesn't quarantine us" —
  UNVALIDATED
- "Behavior is the same signed vs unsigned" — UNVALIDATED
  (Group D produces the signed binary; today's spike is
  unsigned only)

**What this means in code:**
The Group A degradation table (architecture §2.1, Mode #1
"`StartTraceW` returns ERROR_ACCESS_DENIED") is the
EDR-blocked-us path. That path is fully designed and must be
fully tested in Group A regardless of whether the EDR matrix
is populated. **Group A's degradation tests do not depend on
real EDR — they depend on simulating the access-denied
return.** So Group A implementation is not technically
blocked on this report; it is *policy* blocked, per Decision
2, until we have a user call on Option A/B/C/D.

---

## 6. Decision — Option D (hybrid), approved 2026-05-17

**Approved path:** Option D — hybrid.

1. **v0.7 ships with `closed_loop_enabled: false` by default.**
   This already matches the policy default committed in Phase 2
   sign-off resolution #4. First-run onboarding page 3 gains
   one additional line surfacing the EDR-validation gap (see
   §6.4 below).
2. **Community outreach starts immediately** under the
   Option-A scope, with the contact list and evidence
   requirements tightened — see §7.
3. **Group A weeks 2-7 unblock now.** The EDR matrix is no
   longer policy-blocking because closed-loop is default-off in
   v0.7. The matrix becomes a v0.7.1 gate.
4. **The v0.7.1 default-on-flip PR cannot ship until the
   criteria in §6.1 are met.** These are non-negotiable in the
   v0.7.1 cycle — they exist now, in writing, before schedule
   pressure tilts judgment.

### 6.1 Validation criteria for the v0.7.1 default-on flip

ALL of the following must hold before the flip ships. "2 of 3
products validated" is **not** sufficient.

1. **Clean run on each of the three products.** Spike binary
   (or, where applicable, signed v0.7 binary — see criterion
   3) runs on a clean Win11 26100+ VM under each of: Defender
   ATP, CrowdStrike Falcon, SentinelOne Singularity, with
   **no** EDR UI alerts, **no** quarantine, **no** behavioral
   flag at suspicious-behavior thresholds, and **no** admin-
   console / hunting-query telemetry that surfaces FrameSage
   as suspicious. A "clean run" consists of: install →
   60-second `--duration` run → 5-minute `--duration` run →
   session teardown.
2. **At least one of the three runs is performed under
   realistic gaming load** — a real game session running
   concurrently with the ETW session, not idle. This validates
   that the EDR doesn't flag the *combination* of an ETW
   kernel session + concurrent game-adjacent process
   modification (priority bumps, affinity changes). Idle-only
   validation is insufficient.
3. **The signed v0.7 binary is the artifact tested**, not the
   unsigned spike. Group D produces the Authenticode-signed
   binary on a parallel track; if signing is not ready by
   v0.7.1, the flip waits. (This criterion also closes the
   "unsigned vs signed behavior may differ" caveat in §5.)
4. **Vendor-allow-listed counts as validated.** If an EDR
   product flags FrameSage but the flag clears once the signing
   certificate is submitted to the vendor's allow-list /
   reputation system, that product counts as "validated with
   vendor remediation pending" — the flip can proceed iff the
   vendor confirms allow-list grant. If a vendor instead
   requires *architectural changes* in FrameSage to clear the
   flag, that is a v0.8 conversation, not a v0.7.1 one.

### 6.2 Escalation paths based on outreach results

Once community outreach completes (day-5 hard cutoff per §7):

| Outcome | Action |
|---|---|
| **3 of 3 products come back clean** with evidence-level results | Flip ships in v0.7.1 once criteria §6.1 #2 (gaming-load run) and #3 (signed binary) are also met. |
| **2 of 3 clean + 1 inconclusive** | Fall back to Option B (paid in-house validation) for the inconclusive product before the flip. Budget approved up to ~$200-300 + 3 engineer-days for that single product. |
| **1 of 3 clean, or worse** | The flip waits. Re-scope the `closed_loop` architecture for v0.8 with EDR-vendor outreach as a serious engagement, not a side task. v0.7.1 ships without the default-on flip; the closed-loop subsystem remains opt-in indefinitely. |

### 6.3 README + release-notes obligations

Independent of which outcome lands, the v0.6 → v0.7 README
delta MUST mention the EDR-interaction caveat. The
architecture's "Closed-loop and EDR" README section already
commits to this; this report must also be cited from the
README so readers can see exactly what we did and didn't test.
The v0.7 release notes must say "closed-loop measurement
default-off in v0.7 pending EDR validation; see
`spike/etw-edr-report.md`."

### 6.4 First-run onboarding copy delta

Page 3 of the onboarding wizard (architecture §
"First-run onboarding — new closed-loop opt-in page") gains
this line in the disclosure block, above the radio buttons:

> *EDR validation in progress for v0.7.1. Enable if you're on
> a personal machine; we recommend leaving disabled on
> work-managed machines until v0.7.1 confirms compatibility.*

This is wired into the Group C acceptance criterion as a
mandatory string check (reviewer rejects a PR that ships
page 3 without it). Removed in the v0.7.1 default-on flip PR
once §6.1 criteria are met.

---

## 7. Community outreach scope (Option D execution)

### Target contacts, in priority order

1. **PerfView maintainers** (Vance Morrison + the GitHub
   contributor set). PerfView consumes the same
   SystemTraceProvider API we do, has had every EDR
   conversation under the sun, and is the most likely source
   of current data about which products flag ETW consumers.
   Reach: file an issue on
   [microsoft/perfview](https://github.com/microsoft/perfview/issues)
   tagged "discussion" with a pointer to this report; cc
   Vance via the maintainer list if there's no response in 48
   hours.
2. **Sysinternals (Mark Russinovich's team), if any direct
   contact exists.** Process Explorer / Process Monitor face
   exactly our EDR problem at much larger scale. The team
   accepts technical inquiries via the Sysinternals forum and,
   for established correspondents, via direct email. No
   guaranteed channel; surface this as a "if any team contact
   exists" item, not a default expectation.
3. **Process Hacker / System Informer maintainers.** Same
   category as FrameSage (kernel-event consumer, native UI,
   open source), smaller scale, will have current data. Reach:
   issue on
   [winsiderss/systeminformer](https://github.com/winsiderss/systeminformer/issues)
   tagged "question" with the same scope-of-questions doc.
4. **r/sysadmin and EDR-focused Slack/Discord communities.**
   The BloodHound community in particular has EDR-savvy folks
   who routinely run multi-EDR home labs. Reach: a single
   request post in r/sysadmin (read the wiki rules first to
   avoid the auto-remove pattern), plus a question in the
   BloodHound Slack #general (channel rules permitting).

### What each contact receives

Standardized package (drafted as part of the same outreach
push; lives outside the repo because it gets sent to external
parties — but the templates are tracked in
`spike/outreach/`):

- The **unsigned spike binary**, OR build instructions for
  contacts who reasonably refuse to run an unsigned binary.
  (Both options offered; the contact picks.)
- A copy of this report (`spike/etw-edr-report.md`) trimmed
  to §§1, 3, and 6.1 so the contact sees the scope of
  questions, the validation criteria they're contributing
  to, and the env-1 gap that motivated the outreach.
- A copy of `spike/etw-schemas.md` for technical reviewers
  who want to validate the ETW behavior is "just a normal
  ETW consumer" before lending the test box.

### Evidence-level results, not anecdote

Each respondent is asked specifically for:

- **Screenshots** of the EDR console showing FrameSage's
  process during the run, OR
- **Exported logs / alerts** from the EDR's admin console
  (CSV, JSON, whatever the product produces), OR
- **Hunting-query output** if the product has one
  (CrowdStrike CSPL, Defender ATP advanced hunting, S1 deep
  visibility queries) showing what telemetry the product
  collected about FrameSage.

A response of "I ran it, seemed fine" is **not acceptable**
as a validation source for §6.1 criterion 1 — that produces
no archivable evidence and we can't show it to a v0.7.1
reviewer asking "how do you know?"

### Day-5 hard cutoff

Outreach starts on the day this PR merges. If no
evidence-level response has landed for a given product by
**day 5**, that product escalates immediately to Option B
(paid in-house validation) without further waiting. The day-5
cutoff exists so that "we're still waiting on the community"
doesn't become a euphemism for "no one wrote back and we
haven't done anything about it."

Tracking lives in this report's §10 "Results log" — appended
to as responses arrive.

### Tracking

Each respondent's package + response is tracked in `§10
Results log` below. Each row records: respondent identifier
(handle / org / repo), product covered, date sent, date
responded, evidence-level result link, and the criterion
§6.1 #N that the response satisfies.

---

## 8. Process change for future spike reports

Phase 1's "Defender is running" claim was wrong on env-1.
Phase 2's schema research caught the DPC opcode 0x2E error.
Both are caught-by-the-rhythm wins, but **two factual errors
in a row is the point to tighten the rhythm itself.**

### New requirement, effective immediately

Any spike report, environment-attestation document, or
similar "we observed X" claim in this repo must include the
**verification commands and their literal output**, not just
the conclusions drawn from them.

**Forbidden form:**

> Defender is running on env-1.

**Required form:**

> Defender is running on env-1.
> ```
> PS> Get-Service WinDefend
>   Status   Name           DisplayName
>   ------   ----           -----------
>   Running  WinDefend      Microsoft Defender Antivirus Service
> ```

Or for build verification:

> Build 26200 verified.
> ```
> PS> [System.Environment]::OSVersion.Version
>   Major  Minor  Build  Revision
>   -----  -----  -----  --------
>   10     0      26200  0
> ```

### What "literal output" means

- The exact command (with no edits / annotations / paraphrase).
- The exact output (preserving column formatting, IDs, GUIDs,
  PIDs, and timestamps as they appeared).
- Captured at the time the claim was authored, with the date
  of capture in the surrounding text.

### Scope

This requirement applies to **any** claim that depends on a
specific machine's state — running services, installed
products, OS build, ETW session attributes, file paths, etc.
It does NOT apply to API-shape claims that are derivable from
public documentation (e.g., "`StartTraceW` returns a `WIN32_ERROR`")
— those are properly cited from MSDN or SDK headers per the
existing PR #67 ground rule.

### Where this is enforced

- PR review checklist (Group A acceptance criterion, new bullet).
- Architecture doc Phase 3 ground rules section (`audit/v0.7-architecture.md`),
  next to the existing schema-research ground rule.
- This report itself: §2 already includes the captured
  PowerShell output for the env-1 Defender state, which is
  the model future reports follow.

This is a cheap process tightening that catches the class of
error that surfaced in PR #68.

---

## 9. Honesty checklist (per Phase 3 ground rule)

This report passes the "would you sign your name to this if
you were the user" check on the following claims:

| Claim | Source |
|---|---|
| Env-1 has no running Defender platform | Live PowerShell capture in §2, 2026-05-17 |
| The Phase 1 spike report's "Defender is running" was wrong on this machine today | Direct re-check against `sc.exe query WinDefend` |
| Spike binary launched + ran on env-1 | Phase 1 spike report `spike/etw-report.md` §Q1, plus six schema-research-week runs |
| Trials for CrowdStrike + SentinelOne are sales-gated | Vendor websites, verified 2026-05-17 |
| Defender ATP requires E5 / E5 trial | [Microsoft Defender for Endpoint licensing] |
| PerfView uses the same SystemTraceProvider API | Verified during PR #67 schema research |

[Microsoft Defender for Endpoint licensing]: https://learn.microsoft.com/en-us/microsoft-365/security/defender-endpoint/minimum-requirements

Claims this report **does not** make:
- "v0.7 is safe with EDR" — we have not validated this.
- "EDR vendors won't flag us" — no evidence either way.
- "Most users won't see any issue" — speculation; insufficient
  data.

When the EDR matrix is populated (whether via Option A, B, or
D), this file will be updated to a real report with row-per-
product results matching the architecture's §2.1 schema (12
rows: 3 products × 2 signing states × 2 EDR-policy profiles).
Until then, this is the gap document.

---

---

## 10. Results log

Populated as community-outreach responses arrive. Each row is
**evidence-level** per §7 "Evidence-level results" — anecdotal
"seemed fine" responses do not get logged here.

### 10.1 Outreach sent

| Date sent (UTC) | Channel | Issue / link | Day-5 cutoff (UTC) | Day-5 cutoff (local UTC+2) | Status |
|---|---|---|---|---|---|
| 2026-05-17 09:18:50 | GitHub issue, `microsoft/perfview` | [#2422](https://github.com/microsoft/perfview/issues/2422) | 2026-05-22 09:18:50 | 2026-05-22 11:18 | awaiting response |
| 2026-05-17 09:19:13 | GitHub issue, `winsiderss/systeminformer` | [#2916](https://github.com/winsiderss/systeminformer/issues/2916) | 2026-05-22 09:19:13 | 2026-05-22 11:19 | awaiting response |
| _(02 Sysinternals — held pending user sign-off on recipient-facing body)_ |  |  |  |  |  |
| _(04 r/sysadmin — user posting manually via own Reddit account)_ |  |  |  |  |  |

Day-5 hard cutoff applies per outreach per `spike/etw-edr-report.md` §7. If no evidence-level response has landed by the listed cutoff, escalate that product (Defender ATP / CrowdStrike Falcon / SentinelOne Singularity) immediately to Option B (paid in-house validation) without further waiting.

### 10.2 Evidence-level responses received

| Date sent | Respondent | Product | Date responded | Evidence link | Criterion satisfied | Notes |
|---|---|---|---|---|---|---|
| _(none yet)_ |  |  |  |  |  |  |

When this table fills to satisfy criteria §6.1 #1 (clean run
on all three products) + #2 (gaming-load run on at least one)
+ #3 (signed binary) + #4 (vendor allow-list, if any
remediation was required), the v0.7.1 default-on-flip PR
becomes shippable.

---

**Status:** Phase 2 EDR-testing deliverable approved as a gap
report. Option D selected 2026-05-17. Group A weeks 2-7
**UNBLOCKED**. v0.7 ships closed-loop default-off. The v0.7.1
flip is gated on §6.1 criteria + §10 results log.
