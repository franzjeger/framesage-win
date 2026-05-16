# Anti-Cheat Compatibility — FACEIT AC & ESEA Client

Pre-ship research for FrameSage on Windows. Both FACEIT AC and ESEA Client layer
kernel-mode drivers on top of VAC and scrutinize machine state more aggressively
than VAC alone. Scope: CS2 / CS:GO matches.

Classification scale: **Confirmed safe** / **Probably safe** / **Risky** /
**Confirmed unsafe**.

---

## Headline findings

- **ESEA explicitly names Process Lasso as a conflict.** ESEA's own support KB
  lists Process Lasso among programs that cause Error #107 and instructs users
  to **uninstall** it. That is the only public, vendor-named call-out of a
  Bitsum-class tool by either AC.
  ([ESEA support][esea107], [Bitsum FAQ][bitsum-faq])
- **FACEIT AC instruments the kernel heavily.** Independent technical write-ups
  document `LoadImage`, `CreateProcess`, `CreateThread` callbacks; instrumentation
  callbacks to catch syscall returns; "locate and close open handles to the
  protected game"; and a vulnerable-driver blocklist.
  ([arxiv paper][arxiv], [StealthCore writeup][stealth])
- **FACEIT will not start if Windows Update is broken.** It refuses to launch
  on systems "missing important Windows security updates" — disabling/pausing
  Windows Update around match time can leave the user unable to queue.
  ([FACEIT WU article (cached via 3rd-party)][partition], [Recoverit summary][recoverit])
- **FACEIT AC service is self-watched.** If the `FACEITService` is stopped or
  disabled, the AC itself errors. We don't control this service, but it
  signals FACEIT _does_ check service state. ([FACEIT KB summary][servicekb])
- **No vendor-named blocklisting of Process Lasso on FACEIT.** Bitsum's FAQ
  states no bans have ever been issued. But FACEIT's published incompatibility
  list (MSI Dragon Center old, Cirrus Logic, Lenovo Accelerator, ESET old,
  AOMEI Backupper old, USB Network Gate, AMD/NVIDIA Pixel Clock Patcher) is
  **driver-centric**, not user-mode-utility-centric.
  ([Forbidden Driver KB summary][forbidden])

---

## Per-action classification

### 1. Stop services mid-match (telemetry/search/update/OEM)

| AC | Verdict | Reason |
|---|---|---|
| FACEIT | **Risky** | FACEIT AC actively probes Windows Update health and refuses launch when WU is broken ([partition][partition], [recoverit][recoverit]). DiagTrack/WSearch/WU not individually proven required, but stopping `wuauserv` / `UsoSvc` / `WaaSMedicSvc` while AC is running risks the next AC re-check failing. AC also self-watches its own service ([servicekb][servicekb]) — strong signal it monitors service state generally. |
| ESEA | **Risky** | ESEA's kernel driver loads at boot ([techraptor][techraptor]); fewer public reports but historically stricter. No vendor docs covering arbitrary service stops. |

**Action item:** never stop `wuauserv`, `UsoSvc`, `WaaSMedicSvc`, `WaaSMedicSvc`,
`BITS`, `DPS`, `Sense` (Defender ATP), `FACEITService`, `FACEIT_AC`, `ESEAClient*`,
or the kernel-mode AC drivers while CS2/CSGO is running.

### 2. Suspend processes mid-match via `NtSuspendProcess` from SYSTEM

| AC | Verdict | Reason |
|---|---|---|
| FACEIT | **Risky → Confirmed unsafe if target is cs2.exe** | FACEIT registers `OB_OPERATION_HANDLE_CREATE` filters and "locate[s] and close[s] open handles to the protected game" ([arxiv][arxiv]). SYSTEM suspending an unrelated user-mode process is **probably tolerated** (no public reports of it triggering). SYSTEM suspending cs2.exe is **never safe** — it requires opening cs2.exe with `PROCESS_SUSPEND_RESUME`, which FACEIT will strip. |
| ESEA | **Risky** | Kernel driver; same handle-stripping logic assumed. Public ban policy mentions "Illegal Customization" tied to AC. ([ESEA ban rules][banrules]) |

**Action item:** never enumerate `PROCESS_SUSPEND_RESUME` against cs2.exe /
csgo.exe / FACEIT_AC.exe / ESEAClient.exe. Skip these PIDs in the suspend tree.

### 3. Affinity / priority / I/O priority on cs2.exe / csgo.exe

| AC | Verdict | Reason |
|---|---|---|
| FACEIT | **Risky** | Bitsum confirms EAC strips `PROCESS_SET_INFORMATION` on protected processes ([bitsum-faq][bitsum-faq]); FACEIT does the same per arxiv ([arxiv][arxiv]). At best the call fails with Access Denied. At worst the AC logs the attempt. Community workaround is to set affinity on `steam.exe` and let CS2 inherit, never on cs2.exe directly. ([Bitsum FAQ][bitsum-faq]) |
| ESEA | **Confirmed unsafe** | ESEA explicitly tells users to uninstall Process Lasso ([esea107][esea107]). The reason isn't published but the most charitable read is "any user-mode tool poking the game process trips us." |

**Action item:** never touch cs2.exe / csgo.exe affinity/priority/IO-priority
directly. If we want CS to inherit good affinity, gate it on Steam.exe at launch
and back off the moment AC drivers are detected loaded.

### 4. `K32EmptyWorkingSet` on adjacent processes

| AC | Verdict | Reason |
|---|---|---|
| FACEIT | **Probably safe** | Requires `PROCESS_SET_QUOTA | PROCESS_QUERY_INFORMATION` on the target, not on cs2.exe. No public reports. |
| ESEA | **Probably safe** | Same. No reports either way. |

Caveat: calling it on cs2.exe / FACEIT_AC.exe / ESEAClient.exe is unsafe for the
same reason as (3). Stay on adjacent processes only.

### 5. Power plan switch

| AC | Verdict | Reason |
|---|---|---|
| FACEIT | **Confirmed safe** | No documented detection. `SetActiveScheme` is a documented Win32 API used by OEM dock software, Lenovo Vantage, etc. No reports in any forum. |
| ESEA | **Confirmed safe** | Same. |

### 6. Hide taskbar

| AC | Verdict | Reason |
|---|---|---|
| FACEIT | **Confirmed safe** | Cosmetic shell op. Not a process operation. Not a syscall AC instruments. |
| ESEA | **Confirmed safe** | Same. |

### 7. Pause Windows Update

| AC | Verdict | Reason |
|---|---|---|
| FACEIT | **Confirmed unsafe (eventually)** | FACEIT AC refuses launch on systems missing security updates ([partition][partition], [recoverit][recoverit]). Pausing WU for a single match: probably no immediate impact. Pausing it for weeks: AC will start refusing the user. |
| ESEA | **Probably safe** | No published WU dependency, but ESEA's stricter posture means we'd rather not test it. |

**Action item:** if we ever pause WU, restore the pause window aggressively and
never extend it. Surface a warning if the user has FACEIT AC installed.

### 8. SYSTEM service holding handles to cs2.exe / csgo.exe

| AC | Verdict | Reason |
|---|---|---|
| FACEIT | **Confirmed unsafe** | The arxiv paper is explicit: FACEIT has "mechanisms to locate and close open handles to the protected game" ([arxiv][arxiv]). Even if our service holds them with read-only access for monitoring, FACEIT will strip the handle and log the event. A pattern of repeated handle opens may be flagged. |
| ESEA | **Confirmed unsafe** | Same threat model; ring-0 driver on by boot. |

**Action item:** do not `OpenProcess(cs2.exe)` from the SYSTEM service at all.
Use process enumeration (toolhelp / NtQuerySystemInformation) which never opens
a handle to the target. PID + name + cmdline is fine.

### 9. SYSTEM service running continuously

| AC | Verdict | Reason |
|---|---|---|
| FACEIT | **Probably safe** | FACEIT enumerates running drivers/services and reports them. A signed, non-rootkit SYSTEM service is not by itself flagged — Steam, GeForce Experience, antivirus all run as SYSTEM during matches. |
| ESEA | **Probably safe** | Same. But ESEA's reaction to Process Lasso (likely user-mode) suggests they have a list. Our service name and binary will be enumerated. |

**Action items:** sign the service binary; choose a non-obfuscated display name
("FrameSage"); publish what the service does so AC vendors can categorize it if
asked; do not load any unsigned kernel driver under any circumstance (FACEIT
maintains a blocked-driver list and unloads vulnerable ones ([arxiv][arxiv],
[forbidden][forbidden])).

---

## Cross-cutting answers

- **Is Process Lasso / Bitsum on FACEIT's blocklist?** No public evidence.
  FACEIT's published blocklist is drivers, not user-mode tools
  ([forbidden][forbidden]). Bitsum says no bans, ever ([bitsum-faq][bitsum-faq]).
- **Is Process Lasso on ESEA's blocklist?** **Yes, effectively.** ESEA support
  tells users to uninstall it to fix Error #107 ([esea107][esea107]). Not a ban,
  but a documented "this tool breaks our client."
- **CPU affinity during a match?** All kernel ACs (EAC, FACEIT, ESEA, BE)
  strip `PROCESS_SET_INFORMATION` from the protected game. Bitsum's documented
  workaround is to target `steam.exe` and let inheritance carry settings
  ([bitsum-faq][bitsum-faq]).
- **Services required RUNNING at match start?** `wuauserv` health is checked
  by FACEIT ([partition][partition], [recoverit][recoverit]). `FACEITService`
  and `FACEIT_AC` (kernel) must be running. ESEA requires its own client + driver.
  No public evidence that WSearch/DiagTrack are required.
- **SYSTEM service touching the protected game?** FACEIT actively closes open
  handles to cs2.exe and logs the event ([arxiv][arxiv]). Treat as unsafe.

---

## Recommendations (loud)

1. **Hard-coded skip list in framesage-svc:** `cs2.exe`, `csgo.exe`,
   `FACEIT_AC.exe`, `FACEIT_Start_Protected_Game.exe`, `FACEITService.exe`,
   `ESEAClient.exe`, `eseaclient_x64.exe`, and any process whose PE imports a
   driver named `FACEIT_AC.sys` / `esea*.sys`.
2. **Never open a handle to cs2.exe** with anything stronger than
   `PROCESS_QUERY_LIMITED_INFORMATION`, and ideally not at all.
3. **Detect AC presence and disable mutators on those targets.** Probe for
   FACEIT_AC.sys / ESEA driver loaded; if present, our affinity/priority/suspend
   actions on cs2.exe become **no-op + log**.
4. **Don't pause Windows Update** when FACEIT_AC.sys is detected; surface a
   warning instead.
5. **Don't stop wuauserv / UsoSvc / FACEITService / DPS / Sense** at any time
   while a CS match-capable process tree exists.
6. **ESEA users:** document that running FrameSage may produce Error #107 on
   ESEA Client, mirroring Process Lasso's known conflict, and provide a clean
   "disable for ESEA" toggle.

---

## Sources

- [esea107]: https://support.esea.net/hc/en-us/articles/1260801694010-Error-107-The-ESEA-Client-must-be-launched-before-any-supported-games-
- [banrules]: https://eseaplay.weebly.com/ban-rules.html
- [techraptor]: https://techraptor.net/gaming/news/esea-anti-cheating-client-always-unless-you-uninstall
- [bitsum-faq]: https://bitsum.com/process-lasso-faq/
- [arxiv]: https://arxiv.org/html/2408.00500v1
- [stealth]: https://stealth-core.com/blog/game-cheats/how-faceit-anti-cheat-works/
- [partition]: https://www.partitionwizard.com/news/faceit-ac-system-missing-windows-security-updates.html
- [recoverit]: https://recoverit.wondershare.com/windows-computer-tips/your-system-is-missing-important-windows-security-updates.html
- [servicekb]: https://support.faceit.com/hc/en-us/articles/360014177880-The-service-cannot-be-started
- [forbidden]: https://support.faceit.com/hc/en-us/articles/360014237259--Forbidden-driver-error-message-and-blocked-drivers
- FACEIT support hub: https://support.faceit.com/hc/en-us/categories/360002712659-Anti-Cheat
- Guided Hacking ESEA bypass (context only): https://guidedhacking.com/threads/anticheat-esea-bypass.16114/
