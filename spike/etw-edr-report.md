# v0.7 Phase 2 — EDR Interaction Report

**Status:** **GAP REPORT — not the final EDR validation.**

**Recommendation: STOP for user decision before Group A
implementation starts.** This document surfaces what we can and
cannot test from the current dev environment, per Phase 2
sign-off Decision 2 ("EDR testing matrix stays in scope, gates
Group A implementation start").

The architecture (`audit/v0.7-architecture.md` Section 2.1 →
"EDR interaction — TESTING IS LOAD-BEARING, NOT OPTIONAL") and
the ground rule attached to PR #67 both prohibit handwaving on
this. So rather than fabricate results from a single environment
we don't even fully control, this report documents the testing
gap explicitly and asks for a budget / access decision before
Group A starts.

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

## 6. Request for user decision

Before Group A implementation starts, please choose:

- [ ] **Option A** — pursue community outreach, no budget;
      Group A starts in parallel with no shipping gate until
      we hear back.
- [ ] **Option B** — approve ~$200-300 trial-procurement
      budget + 3 engineer-days for in-house validation; Group
      A starts in parallel, ships gated on completed matrix.
- [ ] **Option C** — ship v0.7 closed-loop default-off; Group
      A starts now with no EDR matrix requirement for v0.7
      ship.
- [ ] **Option D (recommended)** — hybrid: ship default-off
      per Option C, AND pursue community outreach per Option
      A; flip default-on in v0.7.1 when matrix has two of three
      products validated.
- [ ] **Other** — please describe.

Whatever the choice, the v0.6 → v0.7 README delta MUST mention
the EDR-interaction caveat. The architecture's "Closed-loop
and EDR" README section already commits to this; the EDR
report should also be cited from the README so readers can
see exactly what we did and didn't test.

---

## 7. Honesty checklist (per Phase 3 ground rule)

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

**Status:** Phase 2 EDR-testing deliverable produced as a gap
report. Group A implementation is **POLICY BLOCKED** on user
decision in §6 above per Phase 2 sign-off Decision 2.
