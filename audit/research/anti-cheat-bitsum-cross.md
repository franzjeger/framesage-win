# Bitsum / Cross-Tool Anti-Cheat History

Research date: 2026-05-16. Scope: Process Lasso (PL), ParkControl, plus comparator tools (RTSS, MSI Afterburner, Process Hacker / System Informer) against the major anti-cheats (Vanguard, EAC, BattlEye, FACEIT, VAC, RICOCHET).

## Bottom line

In ~15 years of Bitsum's history there is **no documented case of a kernel-mode anti-cheat banning a player for using Process Lasso**, and Bitsum's own FAQ states "there has never been a single case of such." However, PL is **not friction-free**: (a) BattlEye routinely **blocks** `ProcessGovernor.exe` from loading alongside protected games (PUBG, R6 Siege); (b) EAC and Vanguard make game processes **inaccessible** to PL, so PL gets ignored rather than punished; (c) a Valve engineer (Fletcher Dunn, 2024) publicly told CS2 players to **stop using PL** because priority adjustments create scheduling **stalls/stutters** — a *correctness* problem, not a ban. The closest comparator with bans is **Process Hacker / System Informer**, where the *kernel driver* (not the act of holding a handle) triggers EAC/BE refusal-to-launch.

## Process Lasso × AC history

| AC | Has flagged PL? | Specific trigger | Severity | Resolution |
|---|---|---|---|---|
| **Riot Vanguard** | No bans found. PL cannot touch Vanguard-protected processes at all (kernel-level protection from boot). | N/A — PL is blocked from making changes, not punished. | None | PL workaround: set rules on launcher (`RiotClientServices.exe`) or use "Enforce by Registry" (IFEO) so priority is applied at process creation before Vanguard arms. |
| **Easy Anti-Cheat (EAC)** | No PL-specific bans documented. EAC prevents direct process access. | Direct affinity/priority writes to the game process fail silently. | None (ignored) | Bitsum docs: set rules on parent launcher (`steam.exe`, `epicgameslauncher.exe`); children inherit. Registry priorities work because they apply pre-EAC. |
| **BattlEye** | **Blocks `ProcessGovernor.exe` from loading** (PUBG, R6 Siege reports). Also blocks Process Hacker, RTSS overlays, SpeedFan, Reshade, MSI Dragon Center. | File-load block, not behavioral. BE's own FAQ explicitly says blocks are **not bans**: "you won't risk getting banned for any of these messages." | Low — game playable, PL inert during session. | None needed for safety. Users who want PL active either accept it gets neutered or close PL before launch. |
| **FACEIT AC** | No documented PL ban thread surfaced. Community guides (Blur Busters) explicitly show players using PL with FACEIT-AC for core pinning. | N/A | None reported | — |
| **ESEA** | No reports surfaced. | — | — | — |
| **VAC (Valve)** | **No bans.** Community/Steam discussion consensus: VAC bans for tampering with game memory/files; PL does neither. Closest official Valve statement (Fletcher Dunn, 2024) is a **performance warning**, not a ban warning. | "Priority inversion" stalls when Steam client process gets demoted. | Stutters / hitches in CS2; no ban. | Dunn's advice: don't run PL on CS2. Bitsum has since added `start_protected_game.exe` handling. |
| **RICOCHET (CoD)** | No surfaced reports. | — | — | — |
| **Punkbuster (legacy)** | No surfaced reports. | — | — | — |

### Telltale changelog evidence

Process Lasso's own revision history shows the team has been quietly hardening against AC-protected launchers:

- **v14.0.2.12 (Apr 2024):** "Add `start_protected_game.exe` to blacklist for rule enforcement." This is the EAC/Vanguard wrapper used by Fortnite, Apex, etc. — PL learned to *not touch it*.
- **v15.0.0.50 (Sept 2024):** "Enforce by Windows Registry" — IFEO-based priority that applies at process-creation, so it works on processes PL cannot otherwise open. Bitsum docs say this method is "benign so is not known to cause any alerts within anti-cheat systems."
- **v17.0.2.18 (Jan 2026):** "Allow rules to be enforced on `start_protected_game.exe`" but "**mandate a 5-second delay of CPU affinity rules**" on it — i.e. let the AC initialize first, then nudge affinity. Followed by **v17.0.2.20 (Jan 2026):** opt-in INI toggle, making it default-off.

That arc — blacklist → registry-only escape hatch → delayed, opt-in re-enable — is the empirical record of how a 15-year-old shop navigates kernel ACs. They **never** intervene during AC init.

## Comparator tools

| Tool | AC interaction | Trigger |
|---|---|---|
| **Process Hacker / System Informer** | EAC, BattlEye **refuse to launch the game** if PH is running. Documented for Fortnite (EAC), R6 Siege (BE), Tarkov (BE). | The **KProcessHacker kernel driver** (elevated inject capabilities). Disabling the driver in PH's Advanced options resolves it. Holding handles to the game process is *not* the trigger — the driver is. |
| **RTSS (RivaTuner Statistics Server)** | BattlEye blocks the overlay; Apex / Battlefield V users reported RTSS triggering EAC/EA AC "killed overlay" messages. | **Overlay injection / D3D hook**, not the stat-reading. RTSS ships a "Stealth Mode" specifically for BE/Vanguard compatibility. |
| **MSI Afterburner** | Same family of complaints as RTSS (shares the RTSS overlay). EAC allegedly auto-bans some users; mostly anecdotal forum claims. BE explicitly blocks **MSI Dragon Center / MSI SDK** for kernel-driver memory-corruption issues — not Afterburner core. | Overlay injection + on some SKUs a kernel driver. |
| **ParkControl** (Bitsum sibling) | **Zero AC reports surfaced.** PC operates at the system power-policy layer (`powercfg` / `Min/Max processor state`), never opens game processes, never injects, no kernel driver. | None. Empirically invisible to ACs. |

**The clear pattern:** kernel ACs punish (a) suspicious **kernel drivers** in third-party tools, and (b) **overlay/in-process injection**. They do **not** punish external user-mode process-control APIs (`SetPriorityClass`, `SetProcessAffinityMask`, `SetProcessWorkingSetSize`) used from outside the protected process. PL stays in the second category; that's why it has never produced a ban.

## Bitsum's own guidance

From the Process Lasso FAQ and docs:

- "Using Process Lasso will not cause bans. There has never been a single case of such… Process Lasso *can't* be used for cheating in any way. It only makes external adjustments to processes with the goal of performance improvement and no direct access to process memory is ever made."
- Worst case if a PL rule is incompatible: "the game will refuse to launch" — not a ban.
- For EAC/Vanguard games: **set rules on the launcher** (`steam.exe`, `epicgameslauncher.exe`); children inherit. **Or** use "Enforce by Registry" (IFEO) for priority, which is applied pre-process-creation and is "benign so is not known to cause any alerts."
- GPU priority does **not** inherit and IFEO doesn't apply, so AC-protected processes simply cannot have GPU priority adjusted by PL — they accept the limitation rather than fight it.
- Process Lasso has **no formally labeled "anti-cheat-aware mode"**; the equivalents are (a) the `start_protected_game.exe` exclusion/delay logic, (b) the Registry enforcement option that sidesteps the AC entirely, and (c) the documentation pattern of "operate on the launcher, not the game."

## Lessons for FrameSage's defaults

Mapping FrameSage's actions to the empirical AC-trigger record:

| FrameSage action | Precedent for AC flag? | Risk verdict |
|---|---|---|
| CPU affinity via OpenProcess + SetProcessAffinityMask on the game process | None directly. But **fails silently** on EAC/Vanguard-protected processes and is the one PL specifically had to back off from on `start_protected_game.exe`. | **Low ban risk, but mid correctness risk.** Default-off for processes whose parent chain includes `start_protected_game.exe`, `BEService.exe`, `EasyAntiCheat.exe`, `vgc.exe`, or are children of `RiotClientServices.exe`. |
| Priority change | None for ban; **documented correctness hazard** (Fletcher Dunn / CS2 priority inversion when *parent* — e.g. Steam — gets demoted). | **Low ban risk, real perf-regression risk.** Never demote the parent launcher. Never demote Steam/Epic/Riot client. Don't apply during the AC handshake window (first ~5s of game process). |
| Working-set trim (`SetProcessWorkingSetSize` / `EmptyWorkingSet`) | No surfaced reports of any AC flagging this. PL doesn't expose it as a per-game default; it's a manual button. | **Very low risk.** Still: skip on AC-protected game PIDs and on `*AntiCheat*` / `vgc*` / `BE*` processes. Document as "if a rule fails, we no-op, never retry-spam." |
| Persistent rule re-asserting (re-applying after AC blocks the write) | This is **exactly** what PL had to learn not to do — the v14/v17 changelog arc. Repeatedly hammering protected processes is the behavior most likely to look anomalous to a behavioral heuristic, even if no AC currently bans for it. | **Highest reputational risk.** Implement: one attempt, log the failure, **do not retry** for the lifetime of that PID. Mirror PL's `start_protected_game.exe` 5-second delay and process-name blacklist. |
| Holding open handles to game processes (for inspection) | Only Process Hacker / System Informer get punished, and only because of their **kernel driver**, not the handle. | **Zero ban risk** for user-mode handles with minimum rights (PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION). Don't ask for PROCESS_VM_READ / PROCESS_VM_WRITE — those *would* look like a cheat-tool. |
| Kernel driver | Process Hacker's KProcessHacker is the canonical "blocked-by-EAC/BE" example. | **Do not ship one.** Stay user-mode. |
| Overlay / DLL injection | RTSS, Afterburner, Reshade — all flagged. | **Do not inject.** |

**Concrete defaults FrameSage should ship:**

1. **AC-aware process blacklist** (no rules applied): `start_protected_game.exe`, `vgc.exe`, `vgk.exe`, `BEService.exe`, `BEServiceLauncher.exe`, `EasyAntiCheat.exe`, `EasyAntiCheat_EOS.exe`, `RiotClientServices.exe` (and its protected children).
2. **Operate-on-launcher pattern** for EAC/Vanguard titles — write to `steam.exe` / `epicgameslauncher.exe` and rely on child inheritance, matching Bitsum's published guidance.
3. **Initialization delay** of ~5 s on any new game PID before first rule write (matches PL v17.0.2.18).
4. **No retries** when a `SetPriorityClass`/`SetProcessAffinityMask` call returns ACCESS_DENIED — log once per PID, move on. Retry-spam is the only PL-style behavior that could plausibly look anomalous to behavioral AC.
5. **Never demote a launcher**, **never demote Steam/Epic/Riot** — the Fletcher Dunn / CS2 priority-inversion lesson.
6. Open game handles with **minimum rights**, never PROCESS_VM_*. No kernel driver. No overlay.
7. Ship a one-click **"AC mode"** that disables all per-game rule writes for the running game and reverts to launcher-only inheritance — the simplest user-facing safety net.

## Sources

- Bitsum, "Process Lasso FAQ" — https://bitsum.com/process-lasso-faq/
- Bitsum, "Process Lasso Documentation" — https://bitsum.com/processlasso-docs/
- Bitsum, "Process Lasso Revision History" — https://bitsum.com/changes/processlasso/
- Bitsum, "Process Lasso 15.0 – Registry Enforced Priorities" — https://bitsum.com/product-update/process-lasso-15-0-registry-enforced-priorities/
- Bitsum Community Forum, "ProcessLasso and anticheat software" (topic 13138) — https://community.bitsum.com/forum/index.php?topic=13138.0
- Bitsum Community Forum, "Anti cheat software…" (topic 8534) — https://community.bitsum.com/forum/index.php?topic=8534.0
- Steam Community, "battleeye blocks Bluetooth, Winrar, ProcessLasso and etc" (PUBG) — https://steamcommunity.com/app/578080/discussions/1/1734342793781064956/
- Steam Community, "Would using Process Lasso result in a VAC ban?" — https://steamcommunity.com/discussions/forum/9/3044984779771722888/
- Fletcher Dunn (Valve), X/Twitter, 2024 — https://x.com/ZPostFacto/status/1816509027683283040
- bo3.gg, "Valve engineer warns Process Lasso software causes crashes in Counter-Strike 2" — https://bo3.gg/news/valve-engineer-warns-process-lasso-software-causes-crashes-in-counter-strike-2
- EscoreNews, "Valve explained what effect Process Lasso can have in CS2" — https://escorenews.com/en/csgo/article/59759-valve-explained-what-effect-process-lasso-can-have-in-cs2-is-this-the-way-to-fix-constant-stutters-in-cs2
- BattlEye FAQ — https://www.battleye.com/support/faq/
- Steam Community, "Process Hacker blocked by BattlEye" (R6 Siege) — https://steamcommunity.com/app/359550/discussions/0/350543368756063192/
- GitHub winsiderss/systeminformer issue #646 "Every game warning" — https://github.com/processhacker/processhacker/issues/646
- Steam Community, "BattlEye blocking RTSS" (R6 Siege) — https://steamcommunity.com/app/359550/discussions/0/350543738457383742
- EA Forums, "Anti-cheat killed overlay MSI Afterburner and Rivatuner statistics server" (BFV) — https://forums.ea.com/discussions/battlefield-v-en/anti-cheat-killed-overlay-msi-afterburner-and-rivatuner-statistics-server/6806331
- EA Answers, "Apex Legends reading RTSS as cheat?" — https://answers.ea.com/t5/Technical-Issues/Apex-Legends-reading-RTSS-as-cheat/td-p/7426040
- Bitsum, "ParkControl" — https://bitsum.com/parkcontrol/ and forum board https://community.bitsum.com/forum/index.php?board=52.0
