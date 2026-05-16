# Anti-Cheat Research: Riot Vanguard

## Bottom line (one sentence)

**The default Valorant Game Mode profile as proposed is RISKY** — not because any single action is a known-cheating signal, but because (a) Vanguard hooks `NtSuspendProcess` in the kernel with undocumented criteria, (b) Process Lasso — the closest analogue to FrameSage — has multiple public reports of triggering Vanguard mis-detection that resulted in **VAN: Competitive Restrictions** (account-level, not HWID, but still user-facing pain), and (c) Riot's Developer Relations FAQ states explicitly that **there is no allowlist** and Riot will not bless any third-party tool.

## Specific recommendation

**Ship with an "Anti-Cheat Aware" mode that is ON by default for Valorant** (and EAC titles). Concretely:

1. **Detect `vgc.exe` / `vgk.sys` / `VALORANT-Win64-Shipping.exe` and downgrade the rule** to a "Safe" tier that:
   - **Does NOT** open a handle to `VALORANT-Win64-Shipping.exe` at all. Skip affinity, CPU sets, priority, IO priority, and Power Throttling on the game process itself. (Inheritance via launcher is the Bitsum-recommended workaround for EAC; same principle applies to Vanguard.)
   - **Does NOT** suspend anything in the `vgc`/`vgk`/`Riot*`/`VALORANT*` family, and does NOT suspend processes that Vanguard might be communicating with.
   - **Keeps** power plan switch, taskbar hide, working-set trim on unrelated processes, and stopping non-Vanguard-relevant services (with WSearch/SysMain still suspect — see below).
2. **First-run warning** when Valorant is detected for the first time, with a one-click "use Safe profile for Valorant" CTA. Riot's anti-cheat lead's statements about treating any attempt to interfere with Vanguard as suspicious make conservative defaults a legal/PR necessity.
3. **Never** stop `vgc`, never close handles to Vanguard, never touch the `vgk` driver.
4. The SYSTEM service running continuously is fine in principle (Process Lasso does the same), but **do not hold open process handles** to Valorant — open, do the work, close immediately. Better: skip the game process entirely under Safe mode.

There are **no confirmed reports of HARDWARE bans from Process-Lasso-class tools**. Reported consequences top out at "Competitive Restriction" (account-level, recoverable). But Riot reserves HWID bans, and shipping aggressive defaults that mimic Process Lasso's known-bad behavior is asking for the first such case to be ours.

## Findings table

| Action | Verdict | Evidence | Notes |
|---|---|---|---|
| Stop **SysMain** while Valorant runs | Probably safe | No Riot mention; no community reports | Win10/11 prefetch service. Not in Vanguard's known incompatibility list. Stopping it should not impact `vgk.sys`/`vgc.exe`. |
| Stop **WSearch** | Probably safe | No reports linking to Vanguard | Indexer. Independent of Vanguard. |
| Stop **DiagTrack** | Probably safe (lean toward Risky) | [winhelponline](https://www.winhelponline.com/blog/diagtrack-connected-user-experiences-and-telemetry-service/) confirms it's the "Connected User Experiences and Telemetry" service | Vanguard does not document a dependency. **Caveat**: Vanguard's user-mode component may emit ETW telemetry. If DiagTrack is *fully removed*, some debloater tools have caused VAN errors; *stopping* it transiently mid-game is much less risky. Still recommend leaving alone in Safe profile. |
| Stop **BITS** | Probably safe | No Vanguard link found | BITS is used by Windows Update / browser background DLs. Not anti-cheat relevant. |
| Stop **DoSvc, WaaSMedicSvc, UsoSvc, WpnService, CDPSvc, DPS, WdiServiceHost, WdiSystemHost, WerSvc, PcaSvc, dmwappushservice, ClickToRunSvc, SDRSVC, defragsvc, MapsBroker, AJRouter, WMPNetworkSvc, Fax, RetailDemo, PhoneSvc, RemoteRegistry, icssvc, TrkWks, stisvc** | Probably safe | No Vanguard documentation flags any of these | Standard debloat targets. None are Vanguard dependencies. |
| Suspend **OneDrive.exe, FileCoAuth.exe, Dropbox.exe, googledrivesync.exe, GoogleDriveFS.exe, pCloud.exe, MEGAsync.exe** | **Risky** | [archie-osu Vanguard research](https://archie-osu.github.io/2025/04/11/vanguard-research.html) — Vanguard kernel-hooks `NtSuspendProcess`; criteria for blocking are undocumented. | Hook is real, redacted criteria. Suspending non-game processes is *probably* fine but no clearance. Author of the reverse-engineering writeup says "If anyone knows under what condition this actually trips, I'd be interested to know." |
| Suspend **OneDriveStandaloneUpdater.exe, GoogleUpdate.exe, MicrosoftEdgeUpdate.exe, lghub_updater.exe, AdobeARM.exe** | Probably safe | Same `NtSuspendProcess` hook risk applies | Updaters are low-profile; least likely to be in Vanguard's "do not suspend" set. |
| Suspend **GameBar.exe, GameBarFTServer.exe, GameBarPresenceWriter.exe** | Probably safe | [criticalhit.net](https://www.criticalhit.net/gaming/valorants-intrusive-anti-cheat-system-can-now-be-disabledfrom-the-taskbar/) — Vanguard already actively blocks Xbox Game Bar injection | Vanguard treats Game Bar as adversarial overlay. Suspending it is friendlier than what Vanguard does itself. |
| Suspend **WidgetService.exe, Widgets.exe, YourPhone.exe, PhoneExperienceHost.exe** | Probably safe | Not Vanguard-relevant | Modern Windows shell apps; no overlap with anti-cheat. |
| Suspend **NVIDIA Web Helper.exe** | **Risky** | NVIDIA helper is part of the GeForce Experience stack which Vanguard already blocks injection from | NVIDIA driver components are observed by Vanguard. Suspending the *user-mode helper* (not the driver) is probably fine, but if Vanguard polls it, you could trigger a heuristic. |
| Suspend **DellSupportAssistRemedyService.exe, HPSupportSolutionsFrameworkService.exe, HpToastSourceApp.exe, LenovoVantageService.exe** | Probably safe | No Vanguard link | OEM bloat. |
| Suspend `lghub_updater.exe` (Logitech G Hub) | Probably safe | No reports. G Hub itself isn't on the Vanguard naughty list (RTCore64 is) | Updater only — main G Hub left running. |
| **Set CPU affinity on Valorant from SYSTEM service** via `PROCESS_SET_INFORMATION` | **Risky** | [godeye.club Van1338 disclosure](https://godeye.club/van1338-design-flaw-in-riot-vanguard) — Vanguard strips access masks from handles to Valorant via `ObRegisterCallbacks`; "fully privileged handles, once acquired, are immediately patched by Vanguard" | The strip is silent — call should fail with `ERROR_ACCESS_DENIED`, NOT trigger a ban. But the act of *requesting* `PROCESS_SET_INFORMATION` is observed. No documented kick, but no clearance either. **Use launcher inheritance instead** (Bitsum's official EAC workaround in their [FAQ](https://bitsum.com/process-lasso-faq/)). |
| **Set CPU Sets** (`SetProcessDefaultCpuSets`) on Valorant | **Risky** | Same handle/access-rights mechanism as affinity | Identical risk profile. |
| **Set priority class** on Valorant (`SetPriorityClass` to AboveNormal) | **Risky** | Same | Bitsum explicitly recommends "Enforce By Registry" (`PerfOptions\CpuPriorityClass` registry) instead of runtime API calls for anti-cheat games — Vanguard sees the registry-applied value as native, not an external write. |
| **Set IO priority** via `NtSetInformationProcess(ProcessIoPriority)` | **Risky** | Same access-mask stripping likely; undocumented hook coverage | Probably stripped; treat as Risky by default. |
| **Power Throttling mode** (`ProcessPowerThrottling=Performance`) | **Risky** | Same | Same handle-based mechanism. |
| **Working-set trim** (`K32EmptyWorkingSet`) on game-adjacent processes (NOT Valorant) | Confirmed safe | Standard memory API; Vanguard's hooks target the game process, not third parties | Action does not touch Valorant. No detection vector. |
| **Power plan switch** (`PowerSetActiveScheme`) | Confirmed safe | System-wide; no per-process handle. [IQON optimization guide](https://iqondigital.com/learn/games/optimize-valorant-performance) recommends Ultimate Performance for Valorant explicitly. | Riot itself does not flag power plan changes. |
| **Hide taskbar** (`ShowWindow(SW_HIDE)` on `Shell_TrayWnd`) | Confirmed safe | Targets `explorer.exe`'s window, not Valorant | Many guides recommend hiding the taskbar for Valorant fullscreen. Not anti-cheat relevant. |
| **Pause Windows Update** via `HKLM\SOFTWARE\Microsoft\WindowsUpdate\UX\Settings\PauseUpdates*` | Probably safe | Documented Windows registry settings used by Settings UI | Vanguard does check Windows version compliance ("This Build of Vanguard is Out of Compliance" error), but that's about Vanguard's own version, not Windows updates being paused. Stopping `wuauserv` mid-session is *different* and untested — recommend registry-pause approach only. |
| **SYSTEM service holding handles to Valorant** | **Risky** | [godeye.club](https://godeye.club/van1338-design-flaw-in-riot-vanguard) — Vanguard ObRegisterCallbacks audits handle opens and strips rights | Holding a long-lived handle keeps you on Vanguard's audit log. **Mitigation**: open with minimum rights (`PROCESS_QUERY_LIMITED_INFORMATION` only), do work, close immediately. Don't keep handles open. |
| **SYSTEM service running continuously while Valorant runs** | Probably safe | Process Lasso's ProcessGovernor.exe runs as SYSTEM continuously; no bans reported in 15+ years of operation | Existence of a SYSTEM-level optimizer service is fine. *What* it does is what matters. |

## Critical / high-risk findings

### 1. The `NtSuspendProcess` kernel hook is real, and its trip conditions are not public

archie-osu's April 2025 reverse-engineering of Vanguard confirms that `vgk.sys` installs a dispatch-table hook on `NtSuspendProcess` that runs a `RunSomeChecks()` function on both the caller and target before allowing the syscall. The reverse-engineer themselves states: **"If anyone knows under what condition this actually trips, I'd be interested to know."** That is the most important sentence in our entire research surface.

Suspending Valorant's process — which we are NOT proposing — would almost certainly trip the hook. Suspending unrelated processes (OneDrive, Game Bar, etc.) is the open question. Empirical testing on a throwaway Riot account is the only way to definitively answer this.

Source: https://archie-osu.github.io/2025/04/11/vanguard-research.html

### 2. Vanguard strips access rights on ALL handles to Valorant, including from SYSTEM

The Van1338 bug bounty disclosure ($6,000 paid by Riot) documents that Vanguard uses `ObRegisterCallbacks` to:
- Audit every handle open against `VALORANT-Win64-Shipping.exe`
- Strip dangerous access rights (`PROCESS_VM_READ`, `PROCESS_VM_WRITE`, and per multiple sources, `PROCESS_SET_INFORMATION`, `PROCESS_SUSPEND_RESUME`)
- Patch already-opened handles via direct kernel object manipulation

Even from SYSTEM context, our `SetProcessAffinityMask` / `SetPriorityClass` / `SetProcessInformation` calls on Valorant will most likely **fail silently with `ERROR_ACCESS_DENIED`**. No documented evidence that this triggers a ban or kick — but every call is logged from Vanguard's perspective, and Riot's anti-cheat lead Phillip "MirageOfPenguins" Koskinas has stated they treat attempts to interfere with Vanguard's protections as suspicious.

Source: https://godeye.club/van1338-design-flaw-in-riot-vanguard

### 3. Process Lasso has caused Vanguard mis-detections leading to Competitive Restrictions

Multiple Reddit / Discord / forum reports describe the same pattern:
1. User runs Process Lasso with default ProBalance + game profile rules
2. Vanguard service (`vgc`) stops or fails to stay running
3. User receives a VAN: Competitive Restriction (queue-dodge-style penalty) on their account
4. No HWID ban

Bitsum's own troubleshooting guidance recommends: disable Process Lasso from Windows startup, disable ProBalance, remove any rules targeting Vanguard-related processes. This is the closest real-world analogue to FrameSage's planned default behavior, and it has caused user-facing pain.

Sources:
- https://www.oreateai.com/blog/can-you-get-banned-for-using-process-lasso-site-wwwredditcom/c7287a63d4589be93347d9037fb33bfd
- https://bitsum.com/process-lasso-faq/

### 4. Hone (the most popular Valorant-targeted optimizer) takes a deliberately conservative approach

Hone (1M+ users, Epic Games Store + Overwolf listed) explicitly markets that it "optimizes your PC's performance without altering, reading, or interacting with the game files themselves." It performs system-wide tweaks (mitigations, VBS toggles, etc.) and intentionally avoids touching the game process. Its only Vanguard-incompatibility issues are "Vanguard refuses to launch" when users disable VBS — never a ban.

This is the model FrameSage's Safe profile should follow for Valorant: **touch the environment around the game, not the game itself.**

Source: https://hone.gg/game/valorant/

### 5. No allowlist exists, and Riot will not provide one

Riot's official Developer Relations FAQ for Vanguard states: **"There is absolutely no allow list for Vanguard. Developer Relations can make no exceptions, carve out any loopholes, or perform any secret handshakes."** This means FrameSage will not get pre-clearance no matter how the company asks. Burden of safety is entirely on us.

Source: https://www.riotgames.com/en/DevRel/vanguard-faq

## Open questions (empirical testing required)

1. **Does suspending a non-game user-mode process while Valorant runs actually trip Vanguard's `NtSuspendProcess` hook?** Reverse-engineer who decompiled it doesn't know. We need to test: suspend OneDrive.exe mid-match on a throwaway account, see if Vanguard logs anything / issues a VAN: restriction.
2. **Does `SetPriorityClass` from SYSTEM on Valorant succeed or fail-silently?** We expect the latter (access-mask stripped), but unconfirmed. If it succeeds and Vanguard later sees "priority changed externally," that could be a heuristic.
3. **Does stopping SysMain cause Vanguard's user-mode service to behave abnormally?** SysMain is occasionally mentioned anecdotally in Vanguard-incompatibility threads but never with a confirmed mechanism.
4. **What happens if we stop a service Vanguard's user-mode component depends on (e.g., RpcSs, DcomLaunch — NOT on our list, but worth verifying we exclude them)?** Our current list doesn't include these but we should add explicit exclusions.
5. **Does Vanguard observe the registry pause-Windows-Update keys?** No public evidence either way. Probably no, but untested.

## Sources

- archie-osu, "Inside Riot Vanguard's Dispatch Table Hooks" (April 2025): https://archie-osu.github.io/2025/04/11/vanguard-research.html
- godeye.club, "Van1338: Design Flaw in Riot Vanguard ($6,000 bounty)": https://godeye.club/van1338-design-flaw-in-riot-vanguard
- Riot Games Developer Relations, "Vanguard FAQ for Third-Party Applications": https://www.riotgames.com/en/DevRel/vanguard-faq
- Riot Games Support, "VAN: Incompatible Software": https://support-valorant.riotgames.com/hc/en-us/articles/48441713812755-VAN-Incompatible-Software
- Riot Games Support, "Vanguard Restrictions": https://support-valorant.riotgames.com/hc/en-us/articles/22291331362067-Vanguard-Restrictions
- Riot Games, "VAN: Restriction and Closing the Motherboard Pre-Boot Gap": https://www.riotgames.com/en/news/vanguard-security-update-motherboard
- Bitsum, "Process Lasso FAQ": https://bitsum.com/process-lasso-faq/
- Bitsum Community Forum, "Anti cheat software...": https://community.bitsum.com/forum/index.php?topic=8534.0
- Hone, "Boost FPS In Valorant": https://hone.gg/game/valorant/
- guru3d Forums, "RTCore64.sys and Valorant / Vanguard": https://forums.guru3d.com/threads/rtcore64-sys-and-valorant-vanguard.431963/
- ValorantForums, "Valorant's Anti Cheat System, Vanguard, Blocking MSI?": https://valorantforums.com/d/275-valorants-anti-cheat-system-vanguard-blocking-msi
- criticalhit.net, "Valorant's intrusive anti-cheat system can now be disabled… from the taskbar": https://www.criticalhit.net/gaming/valorants-intrusive-anti-cheat-system-can-now-be-disabledfrom-the-taskbar/
- Sesame Disk, "Kernel-Level Anti-Cheat Systems: How They Work and Risks": https://sesamedisk.com/kernel-level-anti-cheat-how-it-works-risks/
- arXiv 2408.00500, "A Critical Examination of Kernel-Level Anti-Cheat Systems": https://arxiv.org/html/2408.00500v1
- secret.club, "Why anti-cheat software utilize kernel drivers": https://secret.club/2020/04/17/kernel-anticheats.html
- IQON Digital, "How to Optimize Valorant for Performance (2026)": https://iqondigital.com/learn/games/optimize-valorant-performance
- oreate AI, "Can You Get Banned for Using Process Lasso (Reddit review)": https://www.oreateai.com/blog/can-you-get-banned-for-using-process-lasso-site-wwwredditcom/c7287a63d4589be93347d9037fb33bfd
- AnswerOverflow VALORANT: https://www.answeroverflow.com/m/1252257711433842850
- winhelponline, "Restore Missing DiagTrack Service": https://www.winhelponline.com/blog/diagtrack-connected-user-experiences-and-telemetry-service/
- Microsoft Learn, "SetProcessWorkingSetSize": https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-setprocessworkingsetsize
