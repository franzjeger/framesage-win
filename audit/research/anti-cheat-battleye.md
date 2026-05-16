# FrameSage Anti-Cheat Pre-Ship Research — BattlEye

**Scope:** PUBG, Rainbow Six Siege, ARMA 3, DayZ, Escape from Tarkov (EFT), Destiny 2, Squad
**Verdict (TL;DR):** BattlEye is loud about *file blocks* but very narrow about *bans*. Per the official FAQ, it "only ever bans for the use of actual cheats/hacks or components of such hacks which are designed to intentionally bypass BE's protection." Reverse-engineering write-ups confirm its handle scanner targets `PROCESS_VM_READ | PROCESS_VM_WRITE` specifically — **not** `PROCESS_SET_INFORMATION`. The biggest real-world risks for FrameSage are (a) the `BEService.exe` file-block list flagging FrameSage's helper exe and preventing game launch, (b) Tarkov's notoriously over-aggressive enforcement reputation, and (c) any action that suspends/halts `BEService` itself.

---

## 1. Core BattlEye behavior FrameSage must respect

### 1.1 Handle scanner targets memory access, not info-set
Two independent reverse-engineering writeups from secret.club confirm the handle enumeration check key line:

> `if ( handle_info->handles[handle_index].GrantedAccess & PROCESS_VM_READ|PROCESS_VM_WRITE )`

It captures image name, path, size, and granted access for processes holding game-process handles with VM read/write rights ([secret.club analysis][sc1], [developer tracking][sc2]). **`PROCESS_SET_INFORMATION` is not in the scan mask.** This is the single most load-bearing finding for FrameSage: a SYSTEM service opening the game with `PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION` to apply affinity/priority is not the access pattern BE's reported scanner looks for.

### 1.2 Ban policy is narrow, kicks are wide
[BattlEye's FAQ][bef]: "we only ever ban for the use of actual cheats/hacks… No one is banned for using non-hack programs… or other passive non-cheating activity." They reserve the right to **kick** for things like macro tools.

### 1.3 File blocks ≠ bans, but they break game launch
PUBG community threads document BE blocking `ProcessGovernor.exe` (Process Lasso), WinRAR, Bluetooth drivers, RTSS, MSI Afterburner DLLs ([PUBG Steam thread][pubg-block], [RTSS PUBG forum][pubg-rtss]). Consistently: "you won't risk getting banned for these messages showing blocked files." But if BE blocks FrameSage's helper exe at game launch, the game won't start. That is a UX-fatal outcome even if no account penalty follows.

### 1.4 Interfering with `BEService` itself is fatal
Community-aggregated guidance is unambiguous: "Suspending or terminating the BattlEye Service while a game process is running can result in an instant ban." **FrameSage MUST exclude `BEService.exe`, `BEDaisy.sys`, `BEClient*.dll`, and the game's `*_BE.exe` launcher** from every suspend/stop/throttle pathway. This must be a hard, name- and signature-checked deny-list, not a soft preference.

---

## 2. Pair-by-pair classification

### 2.1 Stop services mid-session (telemetry, search, update, OEM updaters)
**Classification: Probably safe.** BE does not surveil service control manager state on unrelated services. No reports of bans/kicks tied to stopping `DiagTrack`, `WSearch`, `wuauserv`, Dell/Lenovo updaters. Service stops are administratively logged in the event log, not transmitted to BE's scanner. **Risk:** stopping `wuauserv` or `BITS` during the launcher's pre-flight may cause the launcher's *own* update check to fail (not BE-related). Hard deny anything named `BEService*` or `BattlEye*`.

### 2.2 Suspend processes via NtSuspendProcess (cloud sync, GameBar, OEM, NVIDIA, G Hub)
**Classification: Probably safe** for the listed targets. BE's scanner pattern targets handles *to the game*, not the act of suspending unrelated processes. No reports surfaced of bans for suspending OneDrive/Dropbox/GameBar/LGHUB. **Risks worth being loud about:**
- Suspending `explorer.exe` is *probably safe* but unusual enough that it may correlate with cheater telemetry; recommend leaving it running.
- Suspending NVIDIA `NVDisplay.Container.exe` mid-game has caused overlay/driver issues unrelated to BE — keep an allow-list.
- **HARD DENY:** any `BattlEye*`, `BEService*`, the game's own `*_BE.exe` launcher (BE typically launches the game as a child of `*_BE.exe` and parent-tree termination/suspension is the classic ban path).

### 2.3 Affinity / CPU Sets / priority / I/O priority / power throttling on the game process
**Classification: Probably safe.** Most-cited piece of evidence: a community answer on the EFT forum to "if I change the priority of the EFT process, do I get a ban?" — "**No. You won't get banned for doing this**" ([EFT forum][eft-prio]). Bitsum's FAQ: "Using Process Lasso will not cause bans. There has never been a single case of such" ([Bitsum][bitsum]). Process Lasso is widely used with PUBG/Tarkov/R6 and BE-protected ARMA via setting inheritance from the launcher ([Bohemia forums][bohemia-aff]).

**Implementation caveat with teeth:** how you obtain the handle matters. Request only `PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_LIMITED_INFORMATION` (the minimum set needed for `SetProcessAffinityMask`, `SetPriorityClass`, `SetProcessInformation` for power throttling/CPU sets). **Do not** open with `PROCESS_ALL_ACCESS`, `PROCESS_VM_READ`, or `PROCESS_VM_WRITE` — those are exactly what the secret.club-documented BE handle scanner looks for. Affinity-via-launcher-inheritance (set on the Steam/EFT launcher before it spawns the game) is the safest fallback and is the technique Bitsum officially recommends.

### 2.4 K32EmptyWorkingSet on game-adjacent processes
**Classification: Probably safe** for adjacent processes. Empty-working-set requires `PROCESS_SET_QUOTA | PROCESS_QUERY_INFORMATION` — neither is in BE's scan mask. **Never call this on the game process itself** — even though it likely won't trigger BE, the immediate page-fault storm can cause stutter/hitch that users will (incorrectly) blame on the game.

### 2.5 Power plan switch
**Classification: Confirmed safe.** `PowerSetActiveScheme` and friends are global-system operations; BE does not monitor `powercfg` state changes. No bans, kicks, or false-positive reports surfaced. Razer Cortex (which also flips power plans) has caused BE "Query Timeout" issues in Destiny 2 ([Destiny 2 Steam][d2-cortex]), but that's the Razer overlay/services, not the power plan switch itself.

### 2.6 Hide taskbar
**Classification: Confirmed safe.** Pure shell window-state manipulation. BE does not observe taskbar visibility. Many games already auto-hide the shell in fullscreen-exclusive. **Do not** kill `explorer.exe` to achieve this — kill the window, not the process.

### 2.7 Pause Windows Update
**Classification: Confirmed safe.** Stopping `wuauserv`/`UsoSvc` or setting the pause-until registry value is invisible to BE. Caveat: do this *before* the game launcher does its own update check, or the launcher may fail (game-side, not BE-side).

### 2.8 SYSTEM service holding handles to the game process
**Classification: Probably safe — IF the handle's `GrantedAccess` excludes `PROCESS_VM_READ` and `PROCESS_VM_WRITE`.** Per secret.club's BE analysis, the misc-report path captures "any process with an open process handle (VM_WRITE|VM_READ) to the game." A handle with only `PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_LIMITED_INFORMATION | SYNCHRONIZE` is not what that filter catches. **Strong recommendation: do not cache the handle. Open, apply the setting, close.** This minimizes the time window where any future scan iteration could surface FrameSage's process. Also: the FrameSage service binary MUST be Authenticode-signed — BE's heuristics weight unsigned binaries holding game handles more harshly than signed ones.

### 2.9 SYSTEM service running continuously while game runs
**Classification: Probably safe.** BE does not ban for "an unknown signed service is running" — every Windows box has dozens. The risk is BE's file-block list flagging the FrameSage exe (preventing game launch, not banning), same class of issue that hit Process Lasso, RTSS, and MSI Afterburner. **Mitigations:**
- Ship with Authenticode + EV cert if budget allows.
- Use a benign service name and binary name (avoid words like "inject," "hook," "tweak," "patch," "memory").
- Pre-emptively contact BattlEye support (`support@battleye.com`) before public launch to whitelist FrameSage's signed binary. This is the standard remediation path Bitsum, RTSS, etc. have used.
- Do not load any kernel driver. BE explicitly states it blocks "software that is using kernel drivers which contain known security issues" ([BE FAQ][bef]). User-mode SYSTEM service only.

---

## 3. Cross-cutting answers

**Q: BattlEye flagged Process Hacker/Explorer — does that apply to a SYSTEM service with only `PROCESS_SET_INFORMATION`?**
**A: No, based on documented BE source.** The Process Hacker incidents had two drivers: (1) PH's optional kernel driver (`kprocesshacker.sys`) which BE blocked due to it granting arbitrary memory access ([Steam discussion][ph-steam], [PH forum][ph-forum]), and (2) PH opening processes with full access by default. A SYSTEM service that never loads a driver and opens game handles with `PROCESS_SET_INFORMATION` only is not the same risk profile.

**Q: Has BE banned for Process Lasso, RivaTuner, MSI Afterburner?**
**A: No documented bans.** Both Bitsum and community consensus say no bans. BE has *blocked file loads* of all three at various points ([PUBG RTSS thread][pubg-rtss], [Survarium][surv-rts]). Blocks cause game-launch failures, not account penalties. RTSS "Stealth Mode" is the canonical workaround.

**Q: Tarkov specifically — anything about background-service stopping or process-suspending tools?**
**A: Loud reputation, narrow actual ban triggers.** EFT/BSG runs aggressive ban waves (11k bans Dec'23–Jan'24, 28k Jun–Oct'24 per official Tarkov X/Twitter), but documented triggers are: Cheat Engine open (even unattached), DLL injectors, memory editors, HWID-spoofed accounts. Process Lasso, priority changes, and service stops have *no* documented EFT ban cases. Tarkov's pattern is "false positives skew toward kicks/file-blocks, not bans, but appeals are notoriously hard to win" — so the practical risk for FrameSage is reputational (a user blames FrameSage for any unrelated EFT ban) more than mechanical. Recommendation: ship with an explicit Tarkov-mode that's the most conservative profile (no suspend on anything game-adjacent, affinity-only via launcher inheritance, no working-set trimming).

**Q: Does BE scan for "unexpected processes have my game handle"?**
**A: Yes, but with a specific access-mask filter (`VM_READ|VM_WRITE`).** The report includes image path, size, and granted access. A SYSTEM service holding only `PROCESS_SET_INFORMATION`-class rights is outside that filter per the public RE writeups. This is the linchpin of FrameSage's safety story and should be the design invariant: **never request VM_READ or VM_WRITE on a BE-protected game.**

---

## 4. Risk-ranked recommendations

1. **HARDEN:** Name-and-path deny-list for `BEService*`, `BEDaisy*`, `BEClient*`, `*_BE.exe`, `BattlEye*` — never suspend/stop/throttle/empty-working-set/kill any of these. Enforce in the SYSTEM service, not just UI.
2. **HARDEN:** Process handles to BE-protected games open with `PROCESS_SET_INFORMATION | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_LIMITED_INFORMATION | SYNCHRONIZE` *only*. Add unit/integration tests that assert this access mask. Audit every `OpenProcess` call site.
3. **HARDEN:** Open-apply-close pattern for game handles; do not cache. Reduces handle-scan exposure window.
4. **SHIP-BLOCK:** Authenticode sign every FrameSage binary, ideally EV. Unsigned SYSTEM services holding game handles are the worst possible posture.
5. **PROCESS:** Pre-launch outreach to `support@battleye.com` with signed binary hashes, requesting whitelist. Mirror the path Bitsum/RTSS/MSI used.
6. **PROFILE:** Conservative "BE mode" auto-applied for Tarkov, R6, PUBG, ARMA 3, DayZ, Destiny 2, Squad: affinity via launcher-inheritance, no game-process handles at all if user toggles "max paranoia," no working-set trim, no suspend on `*_BE.exe` parent tree or any child thereof.
7. **NEVER:** Load a kernel driver. Period. BE's stated policy targets exactly that.

---

## Sources

- [BattlEye FAQ — official ban/kick policy][bef]
- [secret.club — BattlEye anti-cheat analysis (handle scanner mask)][sc1]
- [secret.club — BattlEye reverse engineer tracking][sc2]
- [Bitsum — Process Lasso FAQ (no bans, EAC workarounds)][bitsum]
- [PUBG Steam — BE blocks ProcessLasso, WinRAR, Bluetooth (file blocks, not bans)][pubg-block]
- [PUBG forum — BE now blocks RivaTuner/MSI Afterburner][pubg-rtss]
- [Survarium — RivaTuner + MSI Afterburner BE issue][surv-rts]
- [EFT forum — "if I change EFT priority do I get banned?" — No][eft-prio]
- [Bohemia forums — CPU affinity manager BE-compatible for ARMA 3][bohemia-aff]
- [Steam — Process Hacker blocked by BattlEye in R6 Siege][ph-steam]
- [Destiny 2 Steam — Razer Cortex vs BattlEye query timeout][d2-cortex]

[bef]: https://www.battleye.com/support/faq/
[sc1]: https://secret.club/2019/02/10/battleye-anticheat.html
[sc2]: https://secret.club/2020/03/31/battleye-developer-tracking.html
[bitsum]: https://bitsum.com/process-lasso-faq/
[pubg-block]: https://steamcommunity.com/app/578080/discussions/1/1734342793781064956/
[pubg-rtss]: https://forums.pubg.com/topic/45317-battleeye-now-blocks-rivatuner-msi-afterburner-stats-monitor/
[surv-rts]: https://forum.survarium.com/en/viewtopic.php?t=15478
[eft-prio]: https://forum.escapefromtarkov.com/topic/113350-if-i-change-the-priority-of-the-eft-process-do-i-get-a-ban/
[bohemia-aff]: https://forums.bohemia.net/forums/topic/208896-cpu-affinity-manager-be-compatible-sotware/
[ph-steam]: https://steamcommunity.com/app/359550/discussions/0/350543368756063192/
[d2-cortex]: https://steamcommunity.com/app/1085660/discussions/0/3266807987606426634/
