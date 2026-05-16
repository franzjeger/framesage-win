# Anti-Cheat Compatibility Matrix — Synthesis

Built from five parallel research investigations (Vanguard, EAC+Javelin, BattlEye, FACEIT+ESEA, Bitsum cross-tool history). Source files at `audit/research/anti-cheat-<vendor>.md`. This document is the launch-go/no-go reference for the seeded game rules.

---

## TL;DR (one paragraph, honest)

**Zero documented anti-cheat bans from Process-Lasso-class tools across 15+ years of operation.** Bans-that-DO-happen cluster around (a) third-party kernel drivers (Process Hacker's KProcessHacker) and (b) overlay/DLL injection (RTSS, Afterburner, Reshade). FrameSage doesn't do either. The single universal technical invariant: **never request `PROCESS_VM_READ` or `PROCESS_VM_WRITE` on a protected game process** — that's the access mask BattlEye / EAC / FACEIT / Vanguard scanners explicitly key on. `PROCESS_SET_INFORMATION` (what we use for affinity/priority) is outside every documented scanner pattern. **However**, three real risks remain that gate the seeded-rule decision: (1) Vanguard has produced Competitive Restrictions on Process Lasso users mis-detecting our SYSTEM-service activity (account-level, recoverable, but real), (2) ESEA explicitly names Process Lasso as a conflict (Error #107, uninstall recommended), (3) BattlEye routinely *file-blocks* unsigned helpers (game won't launch, not a ban). Net: ship Fortnite / BF6-non-game-actions / EAC titles at full aggression; ship a default-ON **"Anti-Cheat Aware Safe Mode"** for Valorant + CS2 third-party leagues that touches the environment but not the game process; ship a Tarkov/BE preset; document the ESEA conflict in release notes; sign binaries before ship.

---

## Master matrix: action × anti-cheat

Severity scale: ✅ Confirmed safe / ✓ Probably safe / ⚠️ Risky / ❌ Confirmed unsafe / ⛔ Never do (regardless of mode)

| # | Action | Vanguard (Valorant) | EAC (Fortnite/Apex/Rust) | EAC+Javelin (BF6) | BattlEye (PUBG/R6/Tarkov) | FACEIT AC (CS2) | ESEA (CS2) |
|---|---|---|---|---|---|---|---|
| 1 | Stop telemetry/search/update services (SysMain/WSearch/DiagTrack/DoSvc/etc.) | ✓ | ✓ | ✓ | ✓ | ⚠️ avoid wuauserv/UsoSvc/WaaSMedicSvc | ⚠️ |
| 2 | Stop **BITS** | ✓ | ✓ | ✓ | ✓ | ⚠️ launcher may need transfer | ⚠️ |
| 3 | Stop **ClickToRunSvc** | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| 4 | Stop **SDRSVC / defragsvc / WMPNetwork / Fax / Retail / Phone / icssvc / TrkWks / stisvc / etc.** | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| 5 | Suspend **OneDrive / FileCoAuth / Dropbox / Google Drive / pCloud / MEGA** | ⚠️ NtSuspendProcess hooked, criteria undocumented | ✓ | ✓ | ✓ | ✓ | ✓ |
| 6 | Suspend **GameBar*** | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| 7 | Suspend **WidgetService / YourPhone / PhoneExperience** | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| 8 | Suspend **NVIDIA Web Helper** | ⚠️ NVIDIA driver components observed by Vanguard | ✓ | ✓ | ✓ | ✓ | ✓ |
| 9 | Suspend **OEM updaters** (Dell/HP/Lenovo SupportAssist family) | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| 10 | Suspend **G Hub / Adobe ARM / Edge Updater / Google Updater / OneDrive Updater** | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| 11 | **Set CPU affinity on the GAME PROCESS** (PROCESS_SET_INFORMATION) | ⚠️ access stripped, logged | ✅ (silent no-op worst case) | ❌ Javelin blocks core parking on dual-CCD Ryzen; press names PL | ✓ (open with min rights, open-apply-close) | ❌ FACEIT closes handles to cs2.exe | ❌ Error #107 documented |
| 12 | **Set CPU Sets** on the game | ⚠️ same as affinity | ✅ | ❌ same | ✓ | ❌ | ❌ |
| 13 | **Set priority class** on the game | ⚠️ same | ✅ | ❌ same | ✓ | ❌ | ❌ |
| 14 | **Set I/O priority** on the game | ⚠️ same | ✅ | ❌ same | ✓ | ❌ | ❌ |
| 15 | **Set Power Throttling** on the game | ⚠️ same | ✅ | ❌ same | ✓ | ❌ | ❌ |
| 16 | **K32EmptyWorkingSet on adjacent (non-game) processes** | ✅ | ✅ | ✅ | ✅ | ✓ | ✓ |
| 17 | **Power plan switch** to HighPerformance / Ultimate | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 18 | **Hide taskbar** (ShowWindow on Shell_TrayWnd) | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| 19 | **Pause Windows Update** (registry pause keys) | ✓ | ✅ | ✅ | ✅ | ⛔ FACEIT refuses launch with broken WU | ⚠️ |
| 20 | **Stop wuauserv / UsoSvc** (not just pause) | ✓ | ✓ | ✓ | ✓ | ⛔ same reason | ❌ |
| 21 | SYSTEM service holding **handles to the game** (min-rights, open-apply-close) | ⚠️ Vanguard ObRegisterCallbacks audits opens | ✓ if `PROCESS_VM_*` excluded | ✓ same | ✓ if `PROCESS_VM_*` excluded — verified by secret.club RE | ❌ FACEIT actively closes handles | ❌ |
| 22 | SYSTEM service holding **`PROCESS_VM_READ` or `PROCESS_VM_WRITE`** on game | ⛔ | ⛔ | ⛔ | ⛔ BE handle scanner explicitly keys on this | ⛔ | ⛔ |
| 23 | SYSTEM service **running continuously** while game runs | ✓ | ✅ | ✓ | ✓ if signed | ✓ if signed | ✓ if signed |
| 24 | **Touch any AC binary** (vgc/vgk/BEService/BEDaisy/EasyAntiCheat*/FACEIT_AC/start_protected_game/RiotClientServices) | ⛔ | ⛔ | ⛔ | ⛔ "instant ban" reported | ⛔ | ⛔ |
| 25 | **Demote the launcher process** (steam.exe / epicgameslauncher / RiotClientServices) | ⛔ correctness | ⛔ correctness (Valve/Dunn) | ⛔ correctness | ⛔ correctness | ⛔ correctness | ⛔ correctness |
| 26 | **Load a kernel driver** | ⛔ | ⛔ | ⛔ | ⛔ explicit BE FAQ | ⛔ | ⛔ |
| 27 | **Inject overlay or DLL** | ⛔ | ⛔ | ⛔ | ⛔ RTSS/Afterburner precedent | ⛔ | ⛔ |

### Reading the matrix

- **All ✅ across a row** (rows 17, 18) = ship in every mode, no gating.
- **All ✓ or ✅** (rows 3, 4, 6, 7, 9, 10, 16) = ship by default in all modes; no AC concerns.
- **Single ⚠️ in a row** (rows 1, 2, 19, 20) = ship by default except in FACEIT/ESEA mode (auto-detected by AC presence).
- **Mixed ⚠️/❌ on game-process actions** (rows 11–15) = game-process modifications are the central gating decision; default-OFF in Safe Mode, opt-in via the first-run dialog.
- **⛔ on a row** (rows 22, 24, 25, 26, 27) = hard-coded invariants. Never expose to user. Tests must enforce.

---

## "Anti-Cheat Aware Safe Mode" specification

A new GameMode tier between the user's three first-run choices (Aggressive / Balanced / Pinning only). Auto-applied per-game based on detected AC, can be manually overridden per-rule.

### Safe Mode for Vanguard-protected games (Valorant)
**Default for any rule that matches `VALORANT-Win64-Shipping.exe` when `vgc.exe`/`vgk.sys` detected.**

What stays ON:
- Stop services from default list (telemetry/search/update/OEM — rows 1–4)
- Suspend cloud-sync / GameBar / OEM updaters / Office helpers (rows 5–10) **EXCEPT** NVIDIA Web Helper (row 8 — ⚠️ for Vanguard)
- Working-set trim on adjacent processes (row 16)
- Power plan switch (row 17)
- Hide taskbar (row 18)
- Pause Windows Update via registry (row 19)
- Operate on `RiotClientServices.exe` (launcher) for priority/IO — children inherit, Bitsum-validated pattern

What turns OFF:
- All direct modifications to `VALORANT-Win64-Shipping.exe` process (rows 11–15)
- Suspending NVIDIA Web Helper specifically
- Holding handles to the Valorant process for any reason

### Safe Mode for FACEIT-protected matches (CS2)
**Default when `FACEIT_AC.sys` or `FACEITService.exe` detected and target is `cs2.exe`/`csgo.exe`.**

What stays ON:
- Suspend cloud-sync / GameBar / OEM updaters (rows 5–10)
- Working-set trim on adjacent processes (row 16)
- Power plan switch (row 17)
- Hide taskbar (row 18)
- Operate on `steam.exe` for priority/affinity — children inherit (Bitsum's documented FACEIT workaround)

What turns OFF:
- All modifications to `cs2.exe` / `csgo.exe` directly
- **Stopping** `wuauserv` / `UsoSvc` / `WaaSMedicSvc` / `BITS` — FACEIT will refuse to launch
- **Pausing** Windows Update via registry — FACEIT will refuse to launch
- Holding handles to `cs2.exe` for any reason

### Safe Mode for ESEA-protected matches (CS2 with ESEA Client)
**Default when `ESEAClient.exe` / `eseaclient_x64.exe` detected.**

What stays ON: same as FACEIT Safe Mode.

What turns OFF: same as FACEIT Safe Mode + show a one-time banner: *"ESEA Client is known to conflict with process-optimizer tools (Error #107). If you see Error #107, click here to disable FrameSage for this session."* One-click "disable for ESEA" toggle in tray.

### Safe Mode for BattlEye-protected games (Tarkov / PUBG / R6 / DayZ / ARMA / Squad / Destiny 2)
**Default when `BEService.exe` / `BEDaisy.sys` / `*_BE.exe` detected.**

What stays ON: everything except game-process modifications (rows 1–10, 16–20).

What turns OFF:
- Game-process affinity / priority etc. for **Tarkov specifically** (reputational hazard — users will blame us for unrelated bans)
- Game-process actions stay ON for PUBG / R6 / DayZ / etc. (BattlEye's documented scanner mask is outside our access pattern)

Additionally: BE Safe Mode prefers launcher-inheritance — set rule on `eft-launcher.exe` / `RSI Launcher.exe` / etc. when present.

### Aggressive Mode for EAC-protected games (Fortnite / Apex / Elden Ring / Rust)
**Default when EAC detected on a non-BF6, non-CS2 title.**

Full aggression — every row except the ⛔ invariants. EAC is the friendly case; ship as designed.

### BF6 specifically (EAC + Javelin)
**Hybrid.** All non-game-process actions ON (rows 1–10, 16–20). Game-process actions (rows 11–15) **default-OFF** with an explicit per-rule warning naming Javelin: *"EA Javelin actively blocks affinity changes on dual-CCD Ryzen during multiplayer. Press has named Process Lasso as risk-bearing for BF6. No confirmed bans yet, but consider leaving this off."*

---

## Final seeded-rule recommendations (per default game)

| Game | AC | Seeded rule | Game Mode behavior on first launch |
|---|---|---|---|
| **Valorant** (`VALORANT-Win64-Shipping.exe`) | Vanguard | **Auto-apply Vanguard Safe Mode**. No direct game-process modification. Show first-launch dialog: "Valorant uses Riot Vanguard. We've automatically applied our Anti-Cheat Aware Safe profile. [Learn more] [Override anyway, eyes-open]." | Safe Mode |
| **BF6** (`bf6.exe`, `bf6_launcher.exe`) | EAC + Javelin | **BF6 Hybrid**. Aggressive for environment (services / processes / power / taskbar). Game-process actions default-OFF with Javelin warning. | Hybrid |
| **Fortnite** (`FortniteClient-Win64-Shipping.exe`) | EAC | **Full aggression** as designed. No special gating. | Aggressive |
| (future addition) Apex / Elden Ring / Rust | EAC | Full aggression. | Aggressive |
| (future addition) CS2 / CSGO | FACEIT or ESEA detected | Auto-apply FACEIT/ESEA Safe Mode. No game-process modification. No WU pause. | Safe Mode |
| (future addition) Tarkov / R6S / PUBG / DayZ | BattlEye | BE Safe Mode (Aggressive for environment; Tarkov also disables game-process actions). Signed binary + pre-launch outreach to BattlEye support required. | Mostly aggressive |

### What this means for the v0.6 default rule set

The seeded BF6 / Valorant / Fortnite rules in `crates/core/src/policy.rs:454-470` need to grow a per-rule `ac_safe_mode_target: AntiCheatProfile` field that the engine resolves at apply time against detected AC drivers. The first-run dialog (item 4.1 in PHASE2-PLAN) presents the user's three choices (Aggressive / Balanced / Pinning) for the rules where it matters — Vanguard and FACEIT/ESEA always use Safe Mode regardless of user choice, because the user can't opt out of "don't break my account."

---

## Architectural invariants (encode as tests)

These are non-negotiable. Each becomes a unit test in Phase 3 Group 1.

1. **Every `OpenProcess` against a protected-game PID requests at most `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION | PROCESS_SET_LIMITED_INFORMATION | SYNCHRONIZE`.** Never `PROCESS_VM_READ`, `PROCESS_VM_WRITE`, `PROCESS_VM_OPERATION`, `PROCESS_CREATE_THREAD`, `PROCESS_DUP_HANDLE`, or `PROCESS_ALL_ACCESS`. Test: synthetic call site enumeration; CI grep-fail on forbidden access masks against game PIDs.

2. **AC-binary hard deny-list (compile-time const):** `vgc.exe`, `vgk.exe`, `vgtray.exe`, `BEService.exe`, `BEServiceLauncher.exe`, `BEDaisy.sys`, `BEClient*.dll`, `EasyAntiCheat.exe`, `EasyAntiCheat_EOS.exe`, `start_protected_game.exe`, `RiotClientServices.exe`, `FACEIT_AC.exe`, `FACEIT_Start_Protected_Game.exe`, `FACEITService.exe`, `ESEAClient.exe`, `eseaclient_x64.exe`, `WinDefend`, `MsMpEng.exe` (already covered), `*_BE.exe` glob. Test: every IPC mutator + apply path rejects these with rationale "AC component — never touched."

3. **Launcher demotion ban (compile-time const):** never lower priority of `steam.exe`, `epicgameslauncher.exe`, `RiotClientServices.exe`, `EALaunchHelper.exe`, `EpicWebHelper.exe`. The Fletcher Dunn / CS2 priority-inversion lesson. Test: priority-down request against these returns Response::Error.

4. **Open-apply-close pattern for game handles.** Never cache a handle to a protected game. Open at apply time, close after the syscall. Test: code review + grep for any `HashMap<u32, OwnedHandle>` keyed on PIDs flagged as game.

5. **AC detection via driver enumeration.** Engine probes loaded kernel drivers (via `EnumDeviceDrivers` or service enumeration) for `vgk.sys`, `EasyAntiCheat.sys`, `BEDaisy.sys`, `FACEIT_AC.sys`, `esea_*.sys` at startup and on policy reload. Detected → bias defaults toward Safe Mode for affected games.

6. **5-second init delay on new game PIDs before first rule write.** Mirrors Process Lasso v17.0.2.18. Let the AC arm before we poke the process. Test: timing-injected test using item 3.1's `Clock` trait.

7. **No retry on ACCESS_DENIED.** Log once per (PID, action), then never retry for that PID's lifetime. Test: synthetic mock returning ACCESS_DENIED, assert one log line per PID.

8. **No kernel driver. Ever.** Test: `cargo tree` grep + CI check that `windows-drivers` / WDK crates are not in the dependency graph.

9. **No DLL injection. No overlay. Ever.** Test: codebase grep for `CreateRemoteThread`, `LoadLibraryA/W in another process`, `SetWindowsHookEx`, `WH_GETMESSAGE`. All must be absent.

10. **NtSuspendProcess gating.** Even though our deny-list already covers AC binaries, add an explicit second-layer check at the syscall site: if target's image path contains `\Riot\` or `\Vanguard\` or `\BattlEye\` or `\FACEIT\` or `\ESEA\`, refuse with rationale. Defense-in-depth.

---

## Pre-ship checklist (BLOCKS Group 4 release)

- [ ] All 10 invariants above encoded as tests in `crates/engine/src/sys_api.rs` mock infrastructure (depends on Group 3 item 3.1).
- [ ] AC detection probe implemented in `crates/sys/src/inner/ac_detect.rs` (new file). Enumerates loaded kernel drivers + services, returns `AntiCheatPresence` enum.
- [ ] `AntiCheatProfile` enum added to `crates/core/src/policy.rs`: `Aggressive` / `Hybrid` / `SafeMode` / `Disabled`.
- [ ] `Profile.ac_safe_mode_target` field added; serde-default to `AntiCheatProfile::Aggressive` so existing policies migrate.
- [ ] Seeded BF6 rule ships with `ac_safe_mode_target: AntiCheatProfile::Hybrid` and game-process actions on the BF6 image default to disabled with the Javelin warning.
- [ ] Seeded Valorant rule ships with `ac_safe_mode_target: AntiCheatProfile::SafeMode` (cannot be downgraded via first-run; aggressive opt-in requires explicit per-rule confirmation in profile editor).
- [ ] Seeded Fortnite rule ships with `ac_safe_mode_target: AntiCheatProfile::Aggressive` (default).
- [ ] First-run dialog (item 4.1) shows the AC-Safe-Mode disclosure for Valorant specifically, with the three-option choice constrained to "use Safe Mode" (default) or "override and use [chosen aggression] anyway" (warning + log).
- [ ] BattlEye support outreach: send `support@battleye.com` a pre-launch heads-up with signed binary SHA256 and a tool description. Mirror Bitsum/RTSS path. **Out of scope of code work** but blocks BE-protected game support.
- [ ] All shipped binaries Authenticode-signed (deferred to MSI/code-signing engagement per Phase 2 plan, but **at minimum** sign before exposing FrameSage to BattlEye-protected titles).
- [ ] README + first-run dialog include this AC matrix in plain English: "Here's how we treat each anti-cheat. [Link to AC matrix]."
- [ ] One-click "Disable for this game session" toggle in tray menu (immediate revert + skip-apply on next foreground).

---

## What this changes in PHASE2-PLAN.md

Three items need updating:

1. **Item 4.1 (first-run onboarding)** — extend the three-way choice with AC-aware logic. For Valorant, the choice is "use AC-Safe Mode (recommended)" vs "override eyes-open." The Aggressive/Balanced/Pinning choice applies to non-AC-gated games.

2. **Item 4.13 (Game Mode editor full arsenal)** — add an `AntiCheatProfile` editor row per profile. Discover-services and Discover-processes views grey out AC binaries with rationale (same UX as the existing denylist).

3. **NEW item 4.16 — AC detection + AC-aware mode infrastructure.** Should be promoted to Group 1 actually — it's a safety-critical landing. Proposal: move to **Group 1 item 1.9**, alongside the safe-list enforcement. Same architectural shape (deny-list driven, server-side enforced) and shares the test infrastructure.

4. **Defaults: D-2/D-3/D-4 stand. D-5 augmented with the AC matrix.** When the user picks Aggressive in the first-run dialog, Valorant and CS2 third-party leagues still get Safe Mode applied (the user opted into aggressive *defaults*, not into account suicide).

---

## Open questions (would benefit from empirical testing on throwaway accounts)

These are the items the research couldn't definitively answer. Each could be tested via a throwaway Riot account / fresh FACEIT account / non-anchor Steam account — if budget allows. Not blocking ship since defaults already accommodate the worst case.

1. **Does suspending a non-game process via SYSTEM `NtSuspendProcess` actually trip Vanguard's documented hook?** The reverse-engineer who decompiled it doesn't know. Empirical test: suspend OneDrive mid-Valorant-match on a throwaway, watch for VAN: errors.
2. **Does Vanguard log SYSTEM `OpenProcess(PROCESS_SET_INFORMATION)` against Valorant in a way that contributes to its "suspicious activity" score?** No public answer.
3. **Does BattlEye's file-block list catch unsigned services that don't load DLLs into the game?** Process Lasso ran into this; we might not (different process model) but worth testing pre-launch.
4. **Does FACEIT specifically check WU service state at match end, or only at match start?** A user who pauses WU and finishes the match without restoring it — do they get kicked next match?

---

## Sources (consolidated)

See per-vendor files for full lists. Top sources:
- Bitsum FAQ + 15-year forum / changelog — https://bitsum.com/process-lasso-faq/
- secret.club BattlEye RE — https://secret.club/2019/02/10/battleye-anticheat.html
- archie-osu Vanguard RE (April 2025) — https://archie-osu.github.io/2025/04/11/vanguard-research.html
- godeye.club Van1338 disclosure ($6K Riot bounty) — https://godeye.club/van1338-design-flaw-in-riot-vanguard
- Riot Developer Relations Vanguard FAQ — https://www.riotgames.com/en/DevRel/vanguard-faq
- BattlEye FAQ — https://www.battleye.com/support/faq/
- arxiv 2408.00500 "Critical Examination of Kernel-Level Anti-Cheat" — https://arxiv.org/html/2408.00500v1
- ESEA Error #107 KB — https://support.esea.net/hc/en-us/articles/1260801694010-Error-107
- Club386 BF6 / Javelin / Process Lasso — https://www.club386.com/battlefield-6-anti-cheat-isnt-playing-nice-with-core-parking-on-amd-ryzen-cpus/
- Fletcher Dunn / Valve / CS2 PL warning — https://x.com/ZPostFacto/status/1816509027683283040
- Hone (1M+ Valorant users, conservative model we should mirror) — https://hone.gg/game/valorant/
