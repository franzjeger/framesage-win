# ETW Kernel Event Schemas — Phase 3 Group A Week 1 Deliverable

**Status:** Schema research week deliverable per the new Phase 3
ground rule (`audit/v0.7-architecture.md` → "Phase 3 ground
rules"). Required reading before Group A implementation begins.

**Date:** 2026-05-17. Documents consulted on this date — see
"Citation provenance" section for refetch instructions.

**Empirical observations:** captured on Windows 11 Pro for
Workstations build 26200 via the spike binary's `--histogram`
flag (PR #65). Multiple Windows builds were NOT empirically
validated in this round — see "Build coverage gap" near the
bottom for the path to filling that gap.

---

## Why this document exists

The v0.7 ETW kernel consumer parses event headers from real-time
ETW events. Each parse decision has a source-of-authority
question behind it:

- *"Does opcode 36 mean CSwitch?"* — MSDN says yes (cited below).
- *"Does PerfInfo opcode 50 (which the spike observed) mean
  something useful?"* — MSDN does NOT document it; it's
  empirically observed but NOT in v0.7 parse scope.

The new Phase 3 ground rule from the user: "**No 'I think this
opcode means DPC' without a citation. If schema research reveals
build-dependent opcode meanings, surface before continuing.**"

This document does that for every event the v0.7 consumer
parses. Each entry's "Authority" line is the source we'll cite
when a maintenance engineer six months from now asks "where does
this offset come from?"

---

## Build coverage — closed by Phase 2 sign-off Decision 1

**v0.7 supports Windows 11 24H2 (build 26100) and later only.**
Earlier Windows builds run the v0.6 static-rule engine
unchanged; the closed-loop subsystem (ETW consumer, PresentMon
spawn, session recorder) is gated on a build check at service
startup. See architecture document Section 2.1 "Build-gate
degradation mode" for the full degradation behavior.

**What this means for schema research:**

- The original multi-build empirical-validation gate is
  CLOSED — we don't promise to work on Win10 22H2 or Win11
  23H2, so we don't need empirical proof that our schema
  parses correctly there.
- This document's empirical column covers Win11 26200, which
  is in the supported range (26100+). For Win11 25H2 and
  later, the assumption is that opcode values stay stable per
  the MSDN authority (which doesn't carry per-build opcode
  tables). If a future Win11 release breaks our parser, the
  build-gate's "passthrough mode" catches it cleanly via the
  five degradation modes in architecture Section 2.1.
- For Win11 builds **below** 26100, the closed-loop subsystem
  never instantiates and this document's parse rules are
  irrelevant.

**What v0.8 should revisit:**

- User demand for older-build support. If a real fraction of
  the user base runs Win11 23H2 or earlier, v0.8 could add
  build-aware schema selection. That's a real engineering
  cost (~2-3 weeks for the per-build opcode tables + empirical
  validation matrix) that v0.7 deliberately doesn't pay.
- Empirical validation on a Hyper-V Win11 25H2 / 26H1 image
  once Microsoft releases later builds. The build-gate cap is
  "26100+ inclusive" — every new build past 26100 should be
  re-confirmed via the spike's `--histogram` mode before being
  declared supported.

---

## Provider GUID summary

| Provider | GUID | Authority |
|---|---|---|
| Thread (kernel) | `{3D6FA8D1-FE05-11D0-9DDA-00C04FD7BA7C}` | MSDN [Thread_V2 class](https://learn.microsoft.com/en-us/windows/win32/etw/thread-v2) (consulted 2026-05-17) |
| PerfInfo | `{CE1DBFB4-137E-4DA6-87B0-3F59AA102CBC}` | MSDN [PerfInfo class](https://learn.microsoft.com/en-us/windows/win32/etw/perfinfo) (consulted 2026-05-17) |
| PageFault | `{3D6FA8D3-FE05-11D0-9DDA-00C04FD7BA7C}` | MSDN [PageFault_V2 class](https://learn.microsoft.com/en-us/windows/win32/etw/pagefault-v2) (consulted 2026-05-17) |
| DiskIo | `{3D6FA8D4-FE05-11D0-9DDA-00C04FD7BA7C}` | MSDN [DiskIo_TypeGroup1 class](https://learn.microsoft.com/en-us/windows/win32/etw/diskio-typegroup1) (consulted 2026-05-17) — see "build-dependent field layout" note below |

All four GUIDs were empirically confirmed against the spike's
real-time event stream on Windows 11 26200: events on each GUID
appeared in expected proportions (Thread dominant, PerfInfo
second, DiskIo and PageFault much lower under idle load).

---

## CSwitch — context switch

### Authority

**MSDN page:** [CSwitch class](https://learn.microsoft.com/en-us/windows/win32/etw/cswitch).
Consulted 2026-05-17. MOF declaration on that page:

```
[EventType{36}, EventTypeName{"CSwitch"}]
class CSwitch : Thread_V2
```

`EventType{36}` → opcode value `36` decimal = `0x24` hex.

### Provider

Thread (`{3D6FA8D1-FE05-11D0-9DDA-00C04FD7BA7C}`).

### Opcode

**0x24 (36)** — same value across all documented Windows
versions per the MSDN page (no version-specific opcode note).

### Empirical observation (Win11 26200)

| Test run | Duration | Events at opcode 0x24 | % of Thread provider |
|---|---|---|---|
| 60s primary | 60 s | 211,098 | 100.00% |

Spike consumer observed exclusively opcode 0x24 under the
Thread provider with `EVENT_TRACE_FLAG_CSWITCH` enabled. Matches
MSDN authority cleanly.

### Layout (v0.7 parse scope)

For v0.7's closed-loop signals we need:
- `NewThreadId: uint32` (offset 0, WmiDataId 1)
- `OldThreadId: uint32` (offset 4, WmiDataId 2)
- `NewThreadPriority: sint8` (offset 8, WmiDataId 3)
- `OldThreadPriority: sint8` (offset 9, WmiDataId 4)

The remaining fields (PreviousCState, OldThreadWaitReason, etc.)
are present but not consumed by v0.7. Future work in v0.8 may
read OldThreadWaitReason (the "why did this thread block" signal,
useful for detecting I/O-bound stalls).

**Authority for the layout:** the same MSDN CSwitch page lists
WmiDataId values 1-12 with types. The "Required" section says
"Minimum supported client: Windows Vista" — layout has been
stable since Vista.

---

## DPC — deferred procedure call (regular)

### Authority

**MSDN page:** [DPC class](https://learn.microsoft.com/en-us/windows/win32/etw/dpc).
Consulted 2026-05-17. MOF declaration:

```
[EventType{66, 68, 69}, EventTypeName{"ThreadDPC", "DPC", "TimerDPC"}]
class DPC : PerfInfo
```

`EventType{66, 68, 69}` are mapped name-by-position to
`{"ThreadDPC", "DPC", "TimerDPC"}`:
- **66 (0x42) → ThreadDPC**
- **68 (0x44) → DPC (regular)**
- **69 (0x45) → TimerDPC**

The MSDN [PerfInfo class](https://learn.microsoft.com/en-us/windows/win32/etw/perfinfo)
page (consulted same date) repeats this mapping in its event-
types table — independent confirmation from a second MSDN page.

### Provider

PerfInfo (`{CE1DBFB4-137E-4DA6-87B0-3F59AA102CBC}`).

### Opcodes (all three documented, none version-specific per MSDN)

| Opcode | Type name | What |
|---|---|---|
| **0x42 (66)** | ThreadDPC | Threaded DPC fires; runs at PASSIVE_LEVEL rather than DISPATCH_LEVEL |
| **0x44 (68)** | DPC | Regular DPC fires at DISPATCH_LEVEL |
| **0x45 (69)** | TimerDPC | DPC scheduled by a timer expiry |

### Empirical observation (Win11 26200)

| Opcode | Type | Events in 60s | % of PerfInfo |
|---|---|---|---|
| 0x42 (66) | ThreadDPC | 0 | 0.00% |
| 0x44 (68) | DPC | 18,468 | 56.09% |
| 0x45 (69) | TimerDPC | 323 | 0.98% |

ThreadDPC at 0 events is expected under light dev-box load —
threaded DPCs are rare. Will surface on machines with specific
drivers that use the threaded-DPC mechanism (some Realtek audio,
some NVIDIA generations).

### Layout (v0.7 parse scope)

All three opcodes share the DPC class layout:
- `InitialTime: object` (WmiTime extension, treat as `uint64`
  QPC ticks)
- `Routine: uint32` (DPC routine address — pointer-sized; on
  x64 this is actually `uint64`)

**Critical layout note:** the MSDN page documents `Routine` as
`uint32`, but on x64 systems the actual on-wire size is 64 bits
because addresses are 64 bits. The `EVENT_RECORD.EventHeader.Flags`
field contains `EVENT_HEADER_FLAG_32_BIT_HEADER` /
`EVENT_HEADER_FLAG_64_BIT_HEADER` which tells the parser which
size to use. v0.7 reads this flag and selects the parser
accordingly.

**Authority for this gotcha:** documented in the
[Event Tracing Pointer-Size Note](https://learn.microsoft.com/en-us/windows/win32/etw/event-tracing-mof-qualifiers#pointer)
MOF qualifier page. The `Pointer` qualifier tells consumers to
choose 32/64 size based on the trace metadata.

### What v0.7 records

For each DPC event: increment per-second DPC count, decode
Routine address against the running kernel-image base list (the
existing engine has no image-load consumer, but the spike's
schema-research conclusion is that we do NOT need image
attribution for v0.7 — `DPC driver attribution` is the deferred
v0.8 feature per the v0.7 scope cut). v0.7 only needs the DPC
*rate* per second, not which driver caused it.

---

## ISR — interrupt service routine

### Authority

**MSDN page:** [ISR class](https://learn.microsoft.com/en-us/windows/win32/etw/isr).
Consulted 2026-05-17. MOF declaration:

```
[EventType{67}, EventTypeName{"ISR"}]
class ISR : PerfInfo
```

Re-confirmed on the [PerfInfo class](https://learn.microsoft.com/en-us/windows/win32/etw/perfinfo)
page (event type table entry for value 67).

### Provider

PerfInfo (`{CE1DBFB4-137E-4DA6-87B0-3F59AA102CBC}`).

### Opcode

**0x43 (67)** — same value across documented Windows versions.

### Empirical observation (Win11 26200)

| Test run | Duration | Events at opcode 0x43 | % of PerfInfo |
|---|---|---|---|
| 60s primary | 60 s | 1,788 | 5.43% |

### Layout (v0.7 parse scope)

- `InitialTime: object` (QPC ticks, like DPC)
- `Routine: uint32`/`uint64` (same pointer-size gotcha as DPC)
- `ReturnValue: uint8` (was interrupt claimed?)
- `Vector: uint8` (IDT vector)
- `Reserved: uint16`

v0.7 reads `Routine` for the rate metric. Vector is interesting
but not used in v0.7. ReturnValue could be used to discard
unclaimed interrupts but is not in v0.7 scope.

---

## HardFault — hard page fault (PageFault_HardFault)

### Authority

**MSDN page:** [PageFault_HardFault class](https://learn.microsoft.com/en-us/windows/win32/etw/pagefault-hardfault).
Consulted 2026-05-17. MOF declaration:

```
[EventType{32}, EventTypeName{"HardFault"}]
class PageFault_HardFault : PageFault_V2
```

`EventType{32}` → opcode value `32` decimal = `0x20` hex.

### IMPORTANT — coexists with the older `EVENT_TRACE_TYPE_MM_HPF`

The Windows SDK header
`<windows-10-sdk-10.0.26100.0>/shared/evntrace.h` (line 383)
defines:

```c
#define EVENT_TRACE_TYPE_MM_HPF                 0x0E      // Hard page fault
```

This is a DIFFERENT event with the same conceptual meaning —
both fire when a hard page fault occurs, but they use different
opcodes and different MOF layouts on the same provider GUID:

| Opcode | MOF class | Layout |
|---|---|---|
| 0x0E (14) | `PageFault_TypeGroup1` | Smaller: faulting address only |
| **0x20 (32)** | **`PageFault_HardFault`** | Larger: InitialTime + ReadOffset + VirtualAddress + FileObject + TThreadId + ByteCount |

**v0.7 uses opcode 0x20 (the richer HardFault).** Authority for
preferring this: the
[PageFault_V2 class MSDN page](https://learn.microsoft.com/en-us/windows/win32/etw/pagefault-v2)
documents both in its event-type table, listing 0x20 (32) as
"Hard page fault event. The [**PageFault_HardFault**] MOF class
defines the event data." The richer layout is the modern one;
the 0x0E variant is documented as the older `MM_HPF` and is what
fires under `EVENT_TRACE_FLAG_MEMORY_PAGE_FAULTS` (the broader
flag), while `EVENT_TRACE_FLAG_MEMORY_HARD_FAULTS` (the flag we
enable) fires the 0x20 variant.

**This discrepancy is the kind of finding the ground rule is
designed to catch.** Without the schema research, the spike's
`hard_fault_opcode == 0x20` looked like "I guess this is right"
when it's actually authoritatively correct based on the flag
we enabled.

### Provider

PageFault (`{3D6FA8D3-FE05-11D0-9DDA-00C04FD7BA7C}`).

### Opcode

**0x20 (32)** — same value across documented Windows versions.

### Empirical observation (Win11 26200)

| Test run | Duration | Events at opcode 0x20 | % of PageFault |
|---|---|---|---|
| 60s primary | 60 s | 3 (idle) | 100% |
| 30s collision-test | 30 s | 91 (with activity) | 100% |
| 30s 4× buffer test | 30 s | 2 | 100% |

Hard faults are rare on a hot working set; counts vary by 0-100×
depending on the workload. Spike-induced activity (logman query
mid-run reads parts of the registry/file cache) caused the 91
count vs 3 count.

### Layout (v0.7 parse scope)

- `InitialTime: object` (QPC ticks)
- `ReadOffset: uint64` — file offset that was read
- `VirtualAddress: uint32`/`uint64` (pointer)
- `FileObject: uint32`/`uint64` (pointer)
- `TThreadId: uint32`
- `ByteCount: uint32`

v0.7 records the rate (events/sec) and optionally the
`TThreadId` for per-PID attribution. ByteCount lets us compute
total faulted-in bytes per second — useful as a stutter signal.

---

## DiskIo (Read, Write)

### Authority

**MSDN page:** [DiskIo_TypeGroup1 class](https://learn.microsoft.com/en-us/windows/win32/etw/diskio-typegroup1).
Consulted 2026-05-17. MOF declaration:

```
[EventType{10,11}, EventTypeName{"Read","Write"}]
class DiskIo_TypeGroup1 : DiskIo
```

**Authority cross-reference:** the Windows SDK header
`<windows-10-sdk-10.0.26100.0>/shared/evntrace.h` defines:

```c
#define EVENT_TRACE_TYPE_IO_READ               0x0A
#define EVENT_TRACE_TYPE_IO_WRITE              0x0B
#define EVENT_TRACE_TYPE_IO_READ_INIT          0x0C
#define EVENT_TRACE_TYPE_IO_WRITE_INIT         0x0D
#define EVENT_TRACE_TYPE_IO_FLUSH              0x0E
```

Two independent authorities (SDK header constant + MSDN MOF
declaration) match. Confidence is high.

### Provider

DiskIo (`{3D6FA8D4-FE05-11D0-9DDA-00C04FD7BA7C}`).

### Opcodes

| Opcode | SDK constant | MOF EventTypeName |
|---|---|---|
| **0x0A (10)** | `EVENT_TRACE_TYPE_IO_READ` | Read |
| **0x0B (11)** | `EVENT_TRACE_TYPE_IO_WRITE` | Write |
| 0x0C (12) | `EVENT_TRACE_TYPE_IO_READ_INIT` | (init, not in v0.7) |
| 0x0D (13) | `EVENT_TRACE_TYPE_IO_WRITE_INIT` | (init, not in v0.7) |
| 0x0E (14) | `EVENT_TRACE_TYPE_IO_FLUSH` | (Flush, not in v0.7) |

### Empirical observation (Win11 26200)

| Opcode | Events in 60s | % of DiskIo |
|---|---|---|
| 0x0A (10) Read | 2 | 0.41% |
| 0x0B (11) Write | 446 | 90.84% |
| 0x0E (14) Flush | 43 | 8.76% |

The 91% write-heavy distribution is the Win11 26200 dev-box
under light activity (Steam in tray, browser tabs, Windows
Update transcript writes). Reads were nearly absent because the
working set fit in RAM during the test.

### BUILD-DEPENDENT LAYOUT — this is the gotcha

The MSDN [DiskIo_TypeGroup1 page](https://learn.microsoft.com/en-us/windows/win32/etw/diskio-typegroup1)
documents three distinct field layouts in its Remarks section,
keyed by Windows release:

**Windows Server 2003 (V0 layout — 7 fields):**
```
uint32 DiskNumber
uint32 IrpFlags
uint32 TransferSize
uint32 ResponseTime         ← "Reserved" in modern layout, holds ResponseTime here
uint64 ByteOffset
uint32 FileObject
uint64 HighResResponseTime  ← NOT supported on this version
```

**Windows Server 2003 SP1 / Vista (V1 layout — 8 fields):**
```
uint32 DiskNumber
uint32 IrpFlags
uint32 TransferSize
uint32 ResponseTime
uint64 ByteOffset
uint32 FileObject
uint32 Irp                  ← NEW
uint64 HighResResponseTime  ← NEW
```

**Windows 7+ (V2 layout — 9 fields, the current page header):**
```
uint32 DiskNumber
uint32 IrpFlags
uint32 TransferSize
uint32 Reserved             ← was "QueueDepth" on Win7/Server 2008 R2
uint64 ByteOffset           ← changed to sint64 in modern page
uint32 FileObject
uint32 Irp
uint64 HighResResponseTime
uint32 IssuingThreadId      ← NEW (NOT supported on Win7 / Server 2008 R2 and earlier)
```

**Architectural implication — does this trigger the ground-rule
re-plan?**

The user's ground rule:
> "If schema research reveals build-dependent opcode meanings,
>  surface before continuing — changes consumer architecture
>  from fixed-offset parsing to build-aware schema selection."

Analysis:
- **Opcode meanings are stable.** Opcode 10 means Read across
  all five documented Windows versions. Same for Write at 11.
  Opcode meanings are NOT build-dependent.
- **Event LAYOUTS are build-dependent.** Field count + names
  change across Win Server 2003 → Vista → Win 7 → Win 7+.

The proposed v0.7 consumer architecture already handles this
correctly:
1. It parses by `EVENT_HEADER.EventDescriptor.Version` field —
   the Version is bumped by Microsoft when the layout changes
   (V0 → V1 → V2 in the MSDN page).
2. v0.7 only needs `TransferSize` (offset 8 in all 3 layouts)
   and the opcode itself. **We don't read the version-dependent
   fields.** TransferSize is at the same offset across all
   layouts because the first three fields (DiskNumber, IrpFlags,
   TransferSize) are unchanged.

**Verdict: ground rule does NOT trigger a re-plan.** Opcodes are
stable; we deliberately parse only the prefix that's stable
across all documented layouts.

**Engineering follow-up for Group A weeks 2-7:** the parser must
include a unit test that loads a synthetic Win Server 2003 V0
layout AND a Win 11 V2 layout, asserting both produce identical
DiskNumber+IrpFlags+TransferSize outputs. That's the
build-independence guarantee in test form.

### Layout (v0.7 parse scope — prefix only)

- `DiskNumber: uint32` (offset 0)
- `IrpFlags: uint32` (offset 4)
- `TransferSize: uint32` (offset 8)

We deliberately do NOT parse the variable-layout fields beyond
offset 12. The HighResResponseTime field would be useful for
disk-latency metrics in v0.8 but requires per-build offset
arithmetic that we're punting.

---

## Out-of-scope observation: PerfInfo opcode 0x32 (50)

During the 60s primary spike run, **12,347 events were observed
on the PerfInfo provider at opcode 50 (0x32)** — 37.50% of all
PerfInfo events, second only to regular DPC at opcode 68.

**Authority search outcome: MSDN-undocumented.**

- [PerfInfo class MSDN page](https://learn.microsoft.com/en-us/windows/win32/etw/perfinfo)
  consulted 2026-05-17. Its event-types table lists opcodes 46,
  51, 52, 66, 67, 68, 69. **50 is not listed.**
- [Thread_V2 MSDN page](https://learn.microsoft.com/en-us/windows/win32/etw/thread-v2)
  lists opcode 50 as "ReadyThread" — but that's on the Thread
  provider, NOT PerfInfo. The spike's bucketing puts these
  events under PerfInfo provider GUID, confirmed by the
  histogram (PerfInfo total adds up correctly).

### Disposition (per Phase 2 sign-off Decision 3)

**v0.7 status: observed on Win11 26200, MSDN-undocumented,
parsed as no-op.**

The v0.7 PerfInfo parser explicitly matches opcodes
`{0x42, 0x43, 0x44, 0x45}` and routes everything else — including
0x32 — to the no-op branch. The branch increments the
"discarded by classifier" counter (for buffer-sizing
diagnostics) and returns. Contributes nothing to closed-loop
signals.

This disposition is the kind the schema-research ground rule
was designed for: **honest, cited, deferred with a forwarding
address.** A v0.8 implementer adding closer-loop signals who
needs to know "what's that 0x32 on PerfInfo" reads this
section, follows the v0.8 follow-up below.

### v0.8 follow-up forwarding address

Investigation path when (and only when) opcode 0x32 becomes
load-bearing for v0.8 attribution work:

1. Read `microsoft/perfview` repository, file
   `src/TraceEvent/Parsers/KernelTraceEventParser.cs`. PerfView
   is maintained by Microsoft and tracks new kernel opcodes as
   Microsoft adds them. Pin the commit hash in the v0.8 schema
   doc update.
2. If PerfView doesn't name 0x32 either: run `xperf -on
   PROC_THREAD+LOADER+PROFILE -minbuffers 200 -maxbuffers 400 -f
   trace.etl` for 60 s under load, then open the .etl in
   `wpa.exe` and look at the PerfInfo events table. Microsoft's
   WPA has decoded event names for many events that aren't on
   MSDN. If WPA has a name, cite the WPA decode + Windows
   release.
3. If neither MSDN nor PerfView nor WPA know: the opcode is
   either undocumented kernel telemetry that Microsoft hasn't
   exposed, OR Microsoft-internal-only. v0.8 cannot use it
   without reverse engineering, which violates the project's
   anti-cheat-clean / documented-API-only posture.

**Acknowledged limitation:** I did NOT directly read PerfView's
source during this research week. MSDN authority was sufficient
for the v0.7 parse scope; PerfView lookup is the next step
when v0.8 needs it.

---

## What changes if a future Windows release breaks something

The v0.7 architecture (`audit/v0.7-architecture.md` section 2.1)
specifies a build-detection path:

1. At session startup, `RtlGetVersion` returns the build number.
2. The consumer picks an `OpcodeTable` whose range covers that
   build.
3. If no table covers the build → degraded "passthrough" mode
   with a tray banner.

This document is the **authority** behind each row of those
tables. When a future Windows release breaks the consumer,
a maintenance engineer:

1. Re-runs the spike's `--histogram` on the new build.
2. Diffs the empirical output against this document's "Empirical
   observation" sections.
3. If opcodes shifted: this document's "Authority" lines tell
   them which MSDN page to re-consult. If MSDN was wrong, the
   PerfView source becomes the next authority.
4. Updates this document with new build-number entries before
   updating the consumer code.

**The schema doc is the single source of truth for "where does
this number come from."**

---

## Citation provenance

All MSDN pages cited above were consulted 2026-05-17. The
specific page versions can be refetched via WebFetch with the
URLs given. Each MSDN page on the Microsoft Learn portal has a
`gitcommit` field in its front-matter that pins the document
version; if a page is updated and an opcode value changes,
maintainers should diff against the pinned `git_commit_id`
captured in this document.

| Page | gitcommit pinned in 2026-05-17 fetch |
|---|---|
| CSwitch | `4d5f26c6be39ae31f663357fe190f3868a37f9be` |
| DPC | `4d5f26c6be39ae31f663357fe190f3868a37f9be` |
| ISR | `4d5f26c6be39ae31f663357fe190f3868a37f9be` |
| PerfInfo | `4d5f26c6be39ae31f663357fe190f3868a37f9be` |
| PageFault_V2 | `4d5f26c6be39ae31f663357fe190f3868a37f9be` |
| PageFault_HardFault | `4d5f26c6be39ae31f663357fe190f3868a37f9be` |
| Thread_V2 | `39839c995535665df29798a9a6e6791ec59c8dfd` |
| DiskIo_TypeGroup1 | `4d5f26c6be39ae31f663357fe190f3868a37f9be` |

Windows SDK header: 10.0.26100.0 (the only SDK installed on the
test machine; the `evntrace.h` constants we cite have been
stable since at least Windows 7 per MSDN per-version notes on
each constant).

Spike binary: built from `crates/spike-etw/` at commit
`6d43dd3` (PR #65 merged). The `--histogram` flag added in this
research week is at the proposal/v0.7-architecture branch HEAD
(not yet merged when this document was written).

---

## Group A weeks 2-7 implementation gates

Per the v0.7 architecture's Phase 3 acceptance criteria + the
Phase 2 sign-off decisions, implementation cannot begin until:

- [x] **This document exists and cites authority per event.**
      Done in this week.
- [x] **Multi-build empirical validation NOT REQUIRED.**
      Closed by Phase 2 sign-off Decision 1: v0.7 supports
      Windows 11 24H2 (build 26100) and later only. Earlier
      builds run the v0.6 static-rule engine. See architecture
      Section 2.1 "Build-gate degradation mode".
- [ ] **EDR testing matrix** (Defender ATP + CrowdStrike Falcon
      + SentinelOne Singularity per Phase 2 sign-off Decision
      2). Output is `/spike/etw-edr-report.md`. Required before
      implementation can start.

### Implementation requirements driven by this document

These are not gates (they don't block start) but they ARE
acceptance criteria for Group A weeks 2-7 — the consumer
implementation MUST satisfy these to land:

- [ ] **DPC opcodes use MSDN-authoritative values** per Phase 2
      sign-off Decision 4. Specifically: the consumer matches
      `0x42` (ThreadDPC), `0x44` (DPC), `0x45` (TimerDPC). The
      spike's original `0x2E` constant was empirically wrong;
      production code must NOT carry the bug forward. A unit
      test asserts the parser recognizes all three documented
      DPC opcodes and rejects `0x2E` as DPC.
- [ ] **HardFault opcode uses `0x20` (PageFault_HardFault),
      not `0x0E` (MM_HPF).** The two coexist on the PageFault
      provider; we picked `0x20` because it's the richer
      layout AND because `EVENT_TRACE_FLAG_MEMORY_HARD_FAULTS`
      (our enabled flag) fires the `0x20` variant. Unit test
      pins the choice with a comment citing this document's
      "HardFault" section.
- [ ] **DiskIo parser reads only stable-prefix fields**
      (DiskNumber, IrpFlags, TransferSize). Version-dependent
      fields beyond offset 12 are explicitly skipped. Unit
      test loads a synthetic Win Server 2003 V0 layout AND a
      Win 11 V2 layout and asserts identical stable-prefix
      output from both.
- [ ] **PerfInfo opcode 0x32 (50) parsed as no-op** per
      Decision 3. Discarded events increment a counter for
      buffer-sizing diagnostics, contribute nothing to
      closed-loop signals. Code comment cites this document's
      "Out-of-scope observation" section.

Implementation work in Group A weeks 2-7 is BLOCKED on the
EDR-testing gate above. Continuing without it risks shipping
v0.7 with a real anti-cheat-compatibility unknown.

---

## Findings summary

1. **Every event v0.7 parses has an authoritative citation.** No
   "I think this opcode means..." remains.
2. **Opcode meanings are stable across Windows versions** (per
   MSDN authority) for CSwitch, DPC, ISR, HardFault, DiskIo
   Read/Write. **Ground rule's "stop if opcodes differ" does NOT
   trigger** — pending empirical Win10/Win11-23H2 validation.
3. **DiskIo field layouts ARE build-dependent.** Architecture
   already handles via `EVENT_HEADER.EventDescriptor.Version` +
   stable-prefix parsing. Unit test required as a regression
   gate.
4. **The spike's original DPC opcode 0x2E was empirically
   wrong** (no PerfInfo events fire at 0x2E in our test).
   Authoritative DPC opcodes are 0x42/0x44/0x45 (ThreadDPC/DPC/
   TimerDPC). Spike production code (when implemented in Group
   A) uses the authoritative values.
5. **Spike's hard-fault opcode 0x20 was empirically RIGHT** — but
   for a different reason than originally guessed. There are
   two hard-fault events on the PageFault provider (0x0E
   PageFault_TypeGroup1 + 0x20 PageFault_HardFault); the
   richer-layout 0x20 fires under `EVENT_TRACE_FLAG_MEMORY_HARD_FAULTS`
   which is the flag we want.
6. **PerfInfo opcode 0x32 (50) is observed but undocumented.**
   v0.7 discards. v0.8 should investigate via PerfView source if
   the signal is useful.

This document is the deliverable. Group A implementation gates
open after multi-build empirical validation.
