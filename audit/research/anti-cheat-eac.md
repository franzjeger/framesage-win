# Anti-Cheat Compatibility Research: Easy Anti-Cheat (EAC) + EA Javelin (Battlefield 6)

Scope: Battlefield 6 (Javelin + EAC-style layered AC), Fortnite (EAC), Apex Legends (EAC),
Elden Ring (EAC), Rust (EAC).
Date: 2026-05-16.

## Executive summary

EAC's published profile is meaningfully friendlier than Vanguard's. The primary EAC behavior
against external tooling is to **strip dangerous handle rights at `ObRegisterCallbacks`** (in
the pre-op) rather than terminate or ban the offender. The kernel driver registers handle
callbacks, process/image-load notify routines, and user-mode `ntdll` hooks
(`NtOpenProcess`, `NtReadVirtualMemory`, `NtWriteVirtualMemory`, `NtAllocateVirtualMemory`)
— so what EAC really cares about is **memory access**, not external metadata/control bits.
A `PROCESS_SET_INFORMATION`-only handle is the exact "external adjustment, no memory access"
pattern Bitsum / Process Lasso has been shipping with zero confirmed EAC bans for years
(see [Bitsum FAQ](https://bitsum.com/process-lasso-faq/) and
[Bitsum forum: ProcessLasso and anticheat software](https://community.bitsum.com/forum/index.php?topic=13138.0)).

The single notable outlier is **EA Javelin in Battlefield 6**. Javelin is EA's own kernel
driver layered on top of EAC infrastructure, and it actively *blocks core parking / affinity
manipulation* on multi-CCD Ryzen during multiplayer
([Club386](https://www.club386.com/battlefield-6-anti-cheat-isnt-playing-nice-with-core-parking-on-amd-ryzen-cpus/),
[PC Gamer](https://www.pcgamer.com/games/fps/battlefield-6-and-valorants-invasive-anti-cheats-are-locked-in-a-turf-war/)).
Outlets warn this *could* trigger a false-positive ban, but no confirmed bans have surfaced
on EA Forums or Steam as of this writing
([EA Forums thread](https://forums.ea.com/discussions/battlefield-6-general-discussion-en/will-process-lasso-really-get-me-banned/12870441)).
Treat BF6 affinity manipulation as Risky and gate it behind a default-off seeded rule.

No EAC ban has ever been confirmed for stopping Windows services, suspending unrelated
third-party processes, trimming working sets, switching power plans, hiding the taskbar, or
pausing Windows Update. EAC's own troubleshooting guidance *routinely tells users to stop
overlays, OneDrive, Game Bar, and other background apps* — i.e. it expects users to be doing
exactly what we're doing
([Rust EAC troubleshooting](https://support.facepunchstudios.com/hc/en-us/articles/360019318037-EAC-Authentication-Timeout)).

---

## Action × EAC verdicts

| # | Action | Fortnite | Apex | Elden Ring | Rust | BF6 (Javelin+) |
|---|---|---|---|---|---|---|
| 1 | Stop Windows services (SysMain/WSearch/DiagTrack/BITS/DoSvc/etc.) | Probably safe | Probably safe | Probably safe | Probably safe | Probably safe |
| 2 | Suspend 3rd-party processes via NtSuspendProcess from SYSTEM (OneDrive, GameBar, vendor bloat, updaters) | Probably safe | Probably safe | Probably safe | Probably safe | Probably safe |
| 3 | Affinity / CPU Sets / priority class / I/O prio / power throttling on the **game** via `PROCESS_SET_INFORMATION` | Confirmed safe | Confirmed safe | Confirmed safe | Confirmed safe | **Risky** |
| 4 | Working-set trim (`K32EmptyWorkingSet`) on non-game processes | Confirmed safe | Confirmed safe | Confirmed safe | Confirmed safe | Confirmed safe |
| 5 | Power plan switch to High Performance | Confirmed safe | Confirmed safe | Confirmed safe | Confirmed safe | Confirmed safe |
| 6 | Hide taskbar (shell window state) | Confirmed safe | Confirmed safe | Confirmed safe | Confirmed safe | Confirmed safe |
| 7 | Pause Windows Update | Confirmed safe | Confirmed safe | Confirmed safe | Confirmed safe | Confirmed safe |
| 8 | SYSTEM service holding handles to game process | Probably safe (PROCESS_SET_INFORMATION only) | Probably safe | Probably safe | Probably safe | Probably safe |
| 9 | SYSTEM service running continuously while game runs | Confirmed safe | Confirmed safe | Confirmed safe | Confirmed safe | Confirmed safe |

Notes per row:

**Row 1 — services.** EAC's troubleshooting literally instructs users to stop conflicting
background apps and check `output_log.txt` for "forbidden" processes
([Facepunch / Rust](https://support.facepunchstudios.com/hc/en-us/articles/360019318037-EAC-Authentication-Timeout)).
There is no public report of an EAC ban tied to a stopped Windows service. Telemetry
service stack is irrelevant to anti-cheat — EAC doesn't telemeter through DiagTrack.

**Row 2 — process suspension.** Suspending OneDrive / Dropbox / GameBar / vendor support
agents from a SYSTEM service does not touch the protected game process and does not read
its memory. EAC's `ObRegisterCallbacks` is keyed on handles **to the game**, not on the
suspender. No reports of bans for suspending unrelated processes.

**Row 3 — process control bits on the game (the load-bearing one).**

- For all EAC titles *other than BF6*: EAC strips `PROCESS_VM_READ`/`PROCESS_VM_WRITE` in
  the handle pre-op but **leaves `PROCESS_SET_INFORMATION` alone**. This is exactly why
  Process Lasso's "Enforce By Registry" and launcher-inheritance workarounds work
  ([Bitsum FAQ](https://bitsum.com/process-lasso-faq/)). The Armored Core VI / Le Mans
  Ultimate threads confirm EAC *blocks the modification silently* rather than banning
  ([Steam: ACVI](https://steamcommunity.com/app/1888160/discussions/0/3820795131608163674/)).
  Worst case for FrameSage is a no-op; not a ban.
- For BF6: Javelin actively blocks core parking / affinity changes on dual-CCD Ryzen
  ([Club386](https://www.club386.com/battlefield-6-anti-cheat-isnt-playing-nice-with-core-parking-on-amd-ryzen-cpus/)).
  No confirmed bans, but multiple outlets explicitly call out Process Lasso as risk-bearing
  in BF6 specifically. **Verdict: Risky.** Recommend BF6-specific gating.

**Row 4 — `K32EmptyWorkingSet`.** Trimming a *non-game* process's working set is a normal
documented Win32 call. EAC does not callback on this. Zero reports.

**Rows 5–7.** Power plan / taskbar / WU pause are pure user-session/policy operations that
never touch any game handle. Zero reports across all five titles.

**Row 8 — holding handles.** EAC's pre-op callback can strip undesired access bits at
handle-open time. Opening with only `PROCESS_SET_INFORMATION` (and **never** requesting
`PROCESS_VM_READ`, `PROCESS_VM_WRITE`, `PROCESS_VM_OPERATION`,
`PROCESS_CREATE_THREAD`, or `PROCESS_DUP_HANDLE`) is the Bitsum-validated pattern. The
handle should still open (with reduced rights), or open with the requested rights. Either
way: no ban surface. Keep handle lifetime short; don't `DuplicateHandle`; don't
re-OpenProcess on tight intervals.

**Row 9 — long-running SYSTEM service.** Background SYSTEM services co-existing with EAC
titles are ubiquitous (telemetry, MSI Center, iCUE, NVIDIA Container, vendor RGB). No EAC
documentation calls this out and no community reports exist.

---

## Cross-cutting checks

**Trusted-tool list.** EAC does **not** publish a counterpart to Bitsum's compatibility
list. Epic's [Anti-Cheat Integration Checklist](https://dev.epicgames.com/docs/game-services/anti-cheat/anti-cheat-integration-check-list)
and [Using the Anti-Cheat Interfaces](https://dev.epicgames.com/docs/game-services/anti-cheat/using-anti-cheat)
do not enumerate prohibited user tooling — they're integration guides for game developers.
There is therefore no whitelist to apply for. Compatibility is implicit and behavioral.

**"Service was stopped" bans.** Zero. EAC's documented support pattern actively encourages
stopping conflicting services / overlays. Searches across EA forums, Facepunch, and Epic
support yielded no ban precedent tied to a service stop.

**Handle scrutiny on the game process.** Yes, EAC scrutinizes via `ObRegisterCallbacks`,
but the documented behavior is to **strip dangerous access bits** in the pre-op, not to
ban. Cheat-development write-ups confirm this strip-not-kill model
([TATEWARE deep-dive](https://tateware.com/blog/easy-anti-cheat-how-it-works),
[Back Engineering blog](https://blog.back.engineering/10/08/2021/)).

**Bitsum forum — EAC bans tied to Process Lasso.** None reported. Bitsum's official line
is unambiguous: *"using Process Lasso will not cause bans. There has never been a single
case of such"* ([FAQ](https://bitsum.com/process-lasso-faq/)). The Bitsum community thread
[ProcessLasso and anticheat software](https://community.bitsum.com/forum/index.php?topic=13138.0)
returned 403 to automated fetch but indexed metadata is consistent.

**Javelin specifics (BF6).** Javelin is a kernel driver on top of layered EAC-style
detection. Distinctive behaviors:

- Memory-protection race with Valorant's Vanguard — two AC drivers cannot co-exist on the
  same boot ([PC Gamer](https://www.pcgamer.com/games/fps/battlefield-6-and-valorants-invasive-anti-cheats-are-locked-in-a-turf-war/)).
- Active suppression of core-parking / affinity changes during multiplayer.
- Requires Secure Boot + TPM 2.0 + HVCI / VBS
  ([Windows Central](https://www.windowscentral.com/gaming/battlefield-6-says-its-kernel-level-anticheat-ea-javelin-has-been-a-huge-success)).
- EA's own [Season 1 anti-cheat update](https://www.ea.com/en/games/battlefield/battlefield-6/news/battlefield-6-anticheat-update-season-1)
  mentions only "cheating hardware" as a third-party category — no enumeration of process
  tools, but no whitelist either.
- Public outlets explicitly name Process Lasso as risk-bearing for BF6
  ([Club386](https://www.club386.com/battlefield-6-anti-cheat-isnt-playing-nice-with-core-parking-on-amd-ryzen-cpus/)).

---

## Loud findings

- **BF6 affinity manipulation is the one Risky item in the matrix.** No confirmed bans,
  but the press is loud enough that a seeded rule should ship default-OFF for `bf6.exe`
  and any `bf6*` launcher binaries.
- **MSI Afterburner / RivaTuner have a history of EAC false positives on Apex Legends**
  ([EA Forums report](https://forums.ea.com/discussions/apex-legends-technical-issues-en/i-just-got-banned-and-maybe-msi-afterburner-is-a-reason/12413698),
  [AFK Gaming](https://afkgaming.com/esports/news/apex-legends-players-are-getting-banned-for-no-reason)).
  These are *overlay* false positives, not process-control false positives — FrameSage
  doesn't inject an overlay, so we're not in that blast radius. Still worth a note in
  release docs.

## Default verdict — seeded rules for BF6 / Fortnite

- **Fortnite (`FortniteClient-Win64-Shipping.exe`):** seed default-ON for affinity,
  priority, working-set trim of background apps, service quiet-down. EAC is the friendly
  case; no ban surface from the actions we ship.
- **Battlefield 6 (`bf6.exe` and BF6 launcher chain):** seed default-ON for *non-game*
  actions (service quiet-down, suspend background apps, working-set trim of others, power
  plan, taskbar hide, WU pause). Seed default-**OFF** for any rule that modifies the BF6
  process itself (affinity, CPU sets, priority class, I/O priority, power throttling) and
  surface a one-line in-app warning when the user toggles it on, naming Javelin
  explicitly.

## Net recommendation

EAC's behavior matches the published profile: it's a strip-rights model, not a kill-on-
sight model, and FrameSage's `PROCESS_SET_INFORMATION`-only handle stays well clear of the
detection surface. Ship as-is for Fortnite / Apex / Elden Ring / Rust. Gate BF6
game-process modification behind an explicit default-OFF seeded rule.
