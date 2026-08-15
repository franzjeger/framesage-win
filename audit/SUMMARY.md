# framesage-win — Audit Summary

Phase 1 of a multi-phase improvement engagement. Read-only audit of ~18,650 LOC across 9 crates. Ten dimension-specific reports live alongside this file (`01-self-footprint.md` … `10-install-uninstall.md`) with file:line citations for every finding.

---

## State of the codebase (one paragraph, honest)

This is an **unusually well-architected mid-stage codebase that is one production-readiness pass away from being competitive with Process Lasso**, but currently ships several footguns that range from "first-time user gets the full sledgehammer" to "elevated misclick → BSOD." The bones are excellent: workspace layering is clean, the `sim` crate keeps Linux dev iteration honest, `probalance::decide` is a textbook testable state machine with 10 covering tests, the comment culture explains *why* (not what) at a level rarely seen, the safe-list architecture is the right shape, the Game Mode journal is properly crash-safe with intent-first writes, and there's zero telemetry / zero network surface / zero `LoadLibrary` (so no DLL-search-order vector). The gaps cluster in three places: (1) the **safe-list is great but not consulted at the IPC surface, the policy-`apply` boundary, or by `SetPolicy`** — so the security model leaks at every inner trust boundary that wasn't the original two-pipe-ACL split (which IS done well); (2) **no closed-loop measurement and no retrospective audit trail** — every "optimization" claim is faith-based, the 5-variant IPC event enum captures ~20% of what the engine actually does, the Game Mode journal is *deleted* on revert (destroying the post-session audit), and there is no on-disk log anywhere; (3) **the install/uninstall story is broken** — `framesage uninstall` only deletes the SCM service, leaving four binaries, three shortcuts (including the autostart one that respawns a now-broken tray), `policy.json`, and the `game-mode.journal` that can hold the system in a degraded state. The codebase earns its CPU cost on AMD X3D hardware running listed games, and *probably doesn't* on non-X3D Intel boxes where the marquee `Kind(Cache)` selector silently clears CPU sets while the aggressive Game Mode tax still ships.

---

## Cross-cutting themes (root causes spanning multiple dimensions)

These are the patterns where one underlying fix closes several audit findings at once.

### Theme A — Safe-list is excellent but not enforced at inner trust boundaries
`crates/gamemode/src/safe_lists/*.json` denylist covers csrss/lsass/dwm/audiodg/MsMpEng/Vanguard/EAC/BattlEye — the canonical "you'd brick the box" set, with rationale per entry, schema-versioned, unit-tested. **And then it's only consulted by Game Mode planner + ProBalance + background scan.** Every other apply path bypasses it:
- Per-PID IPC: `set_process_priority`, `suspend_process`, `terminate_process`, `set_process_affinity`, `trim_working_set` (engine/lib.rs:302–425). [02-C1, 03-Crit-1]
- Profile `apply()` (sys/inner/apply.rs:57–154) accepts any exe_name from a policy rule. [02-C2]
- `SetPolicy` (service/runtime.rs:488–523) accepts arbitrary `stop_services` / `suspend_processes` lists from any admin caller. [03-Crit-2]
- `ReportForeground` (engine/lib.rs:943–952) trusts the reported `pid`+`exe_name` pair without server-side cross-check. [03-High-3]

**Single fix** — call `SafeList::check_process(exe_name)` at every kernel-write entry-point and intersect every wire-`Profile`'s lists against the bundled denylist before applying. Closes 5 critical-or-high findings and removes the entire BSOD / admin-amplification class.

### Theme B — No measurement, no audit trail, no on-disk logs
The product is open-loop and amnesiac:
- Engine reads one signal (`GetSystemTimes` + per-PID `GetProcessTimes`) and never measures whether any rule helped. [05-§1]
- IPC `Event` enum has 5 variants for the ~25 distinct things the engine actually does. [09-§1]
- Game Mode journal records 30+ system mutations then **`journal.delete()`** on revert (engine/lib.rs:1983). [09-§6]
- `tracing-subscriber` writes to stderr only; under SCM that's `\Device\Null`. No file sink, no Event Log, no "Open log folder" affordance. [09-§8]
- 32 `warn!`/`error!` sites in engine, zero surfaced as user-visible events. [09-§7]
- Activity tab is a 1000-entry in-memory ring, wiped on tray restart. [09-§2]

The product cannot answer the question *"what did FrameSage do for me in the last hour?"* — which is the trust-building question in this category.

### Theme C — Polling-everywhere when events would do
The footprint floor is dominated by polling that scales with system PID count:
- ProBalance: 3× `OpenProcess` per live PID at 1 Hz = ~750 syscalls/sec idle. [01-C1]
- `list_process_snapshots`: 5× `OpenProcess` per PID at 1 Hz while window visible = ~1250 syscalls/sec. [01-C2]
- Foreground reporter: 4 Hz admin-pipe opens regardless of window visibility, no event-driven gating. [01-H1]
- Master service tick: 300 ms wakeup even when nothing changes. [01-M1]
- Safe-list HashSets rebuilt every ProBalance sample. [01-H4]

`NtQuerySystemInformation(SystemProcessInformation)` returns per-PID CPU/image/priority in one call. `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` makes the foreground reporter event-driven. Both are documented user-mode APIs. The engine has been carrying a brute-force polling tax for one round of optimization.

### Theme D — No environmental-event awareness
Zero references to `WM_POWERBROADCAST`, `WTSRegisterSessionNotification`, `SERVICE_ACCEPT_SESSIONCHANGE`, or topology hot-plug. The engine assumes the world is stable. Consequences: power-plan reverts surprise the user after sleep, RDP/FUS double-trays race to drive the same engine, hot-plug CPUs are invisible, the tray hangs indefinitely on `OpenOptions::open` during service restart with no timeout. [04-HIGH-1/2/3/4/5/7]

### Theme E — Engine is not unit-testable
2,200-line `engine/lib.rs` has **2 tests** (both on a helper enum-exhaustiveness lock). `tick`, `reconcile`, all `maybe_*` methods, `apply_once`, `report_foreground` are untested because:
- `framesage_sys::*` is called as free functions, not through a trait.
- `Instant::now()` is called directly, not through an injectable clock.
- `gamemode/planner.rs` already shows the right shape — `SystemStateQuery` trait — and `probalance::decide` is the gold-standard pure-function model.

Every engine change is a hand-test against real Windows. [08-§2, 08-§13]

### Theme F — Defaults pre-armed for a narrow hardware profile
The shipped policy targets BF6/Valorant/Fortnite at `game-x3d`, which:
- Pins to `Kind(Cache)` — **silently clears CPU sets on non-X3D hardware** while still applying the rest of the aggressive profile. [05-§4, 05-§5]
- Stops 30 services including `WSearch` (Outlook search broken), `BITS`/`DoSvc`/`WaaSMedicSvc` together (Defender sig updates blocked), `ClickToRunSvc` (Office stalls). [05-§6, 07-#1]
- Suspends 24 processes including `OneDrive.exe`+`FileCoAuth.exe` (cloud saves don't replicate during the session). [07-#1]
- No first-run opt-in. A user who installs "just to look" and then launches Valorant gets the full sledgehammer.

### Theme G — Install / uninstall story is a stub
`framesage uninstall` deletes the SCM registration and **nothing else**:
- 4 binaries left in `%LOCALAPPDATA%\Programs\FrameSage\`.
- Startup-folder shortcut left → tray respawns at every logon, fails to connect, sits broken.
- `policy.json` left (probably right behavior, but unintentional).
- **`game-mode.journal` left** — represents in-flight system modifications that nothing can revert once the service is gone. [10-§2]
- Per-user install dir for LocalSystem service = classic admin→SYSTEM persistence primitive. [10-§9, 03-Low-1]
- Unsigned binaries → guaranteed SmartScreen warning for a tool that requests admin. [10-§7]

---

## Findings ranked by severity

File:line in each row points to the deeper write-up in the dimension file. Severity uses the user's rubric.

### Critical (anything that can destabilize the user's system, security holes, footprint regressions)

| # | Finding | File:line | Dimension |
|---|---|---|---|
| C-01 | Per-PID IPC actions (`SetProcessPriority`/`Suspend`/`Terminate`/`SetProcessAffinity`/`TrimWorkingSet`) bypass safe-list — elevated misclick on csrss/wininit/lsass BSODs the box with `CRITICAL_PROCESS_DIED` | `engine/lib.rs:302-425`, `sys/inner/process_actions.rs:73` | 02-C1, 03-Crit-1 |
| C-02 | `Profile::apply()` accepts any rule exe_name with no denylist consult — a policy.json rule for csrss/lsass/dwm freezes the session | `sys/inner/apply.rs:57-154` | 02-C2 |
| C-03 | `SetPolicy` admin pipe accepts arbitrary `stop_services` / `suspend_processes` with no server-side intersection against bundled SafeList — admin-but-not-SYSTEM caller stops Windows Defender | `service/runtime.rs:488-523`, `ipc/lib.rs:43` | 03-Crit-2 |
| C-04 | Policy hot-reload trusts any writer of `policy.json` — no ACL hardening; if `%ProgramData%\framesage\` is created by a non-admin (console-mode dev run before install) that user keeps Modify and owns the service via crafted policy → arbitrary OpenProcess as SYSTEM | `service/runtime.rs:134-194`, `core/paths.rs:18-46` | 03-High-1 (promoted) |
| C-05 | No SCM `FailureActions` configured; with `panic = "abort"` any unhandled panic is a permanent silent outage until reboot | `cli/main.rs:239-250`, `Cargo.toml:95` | 04-CRIT-1 |
| C-06 | Service tick task panic kills the engine silently; SCM still reports Running. No watchdog, no auto-restart. Same for admin/status/reload tasks | `service/runtime.rs:56-91` | 04-CRIT-2 |
| C-07 | Game Mode journal is **deleted on revert** — a 2-hour session that touched 50+ system objects produces zero post-session audit. Worst trust failure | `engine/lib.rs:1983-1992`, `gamemode/journal.rs:172` | 09-§6 |
| C-08 | Uninstall leaves the autostart Startup-folder shortcut → tray respawns at every logon pointing at orphan/deleted binaries → broken-tray persistence the user cannot trace to FrameSage | `install.ps1:101`, `cli/main.rs:271-284` | 10-§2 |
| C-09 | Uninstall leaves `game-mode.journal` behind → if uninstall happens while a Game Mode session is active, stopped services / suspended processes are stranded with no tool to revert them | `cli/main.rs:271-284`, `gamemode/journal.rs:86` | 10-§2 |
| C-10 | Service binary lives in `%LOCALAPPDATA%` (per-user) but runs as LocalSystem at boot → classic admin→SYSTEM persistence primitive. If the installing user is later compromised, binary swap → arbitrary code as LocalSystem next boot | `install.ps1:44`, `cli/main.rs:228-250` | 10-§9, 03-Low-1 |

### High (functional regressions that survive indefinitely, large footprint contributors, severe trust gaps)

| # | Finding | File:line | Dimension |
|---|---|---|---|
| H-01 | ProBalance does 3× OpenProcess per live PID at 1 Hz (~750 syscalls/sec idle on 250-PID box) | `engine/lib.rs:1162-1248` | 01-C1 |
| H-02 | `list_process_snapshots` does 5× OpenProcess per PID at 1 Hz (~1250 syscalls/sec window-visible) | `engine/lib.rs:567-820` | 01-C2 |
| H-03 | Foreground reporter opens fresh admin-pipe instance 4× / sec regardless of window visibility | `tray/main.rs:6043-6079`, `tray/main.rs:6091` | 01-H1, 04-HIGH-4 |
| H-04 | Per-tick `topology.clone()` (every 2 s + every 10 s + every reconcile) and `policy.clone()` (every IPC Status, ~1 Hz) | `engine/lib.rs:548, 1404, 1462, 1556, 1767` | 01-H2 |
| H-05 | `apply_thread_cpu_sets` snapshots every thread in the system per call; fires every 2 s per persistent + cpu_sets PID | `sys/inner/apply.rs:610-654` | 01-H3 |
| H-06 | Safe-list + ignore-list HashSets rebuilt every ProBalance sample (1 Hz) | `engine/lib.rs:1261-1270` | 01-H4 |
| H-07 | Topology flattens to processor group 0; affinity silently misapplies on >64-CPU Threadripper PRO / dual-socket | `sys/inner/topology.rs:235`, `sys/inner/apply.rs:434-442` | 02-C3 |
| H-08 | Intel hybrid P/E not detected — every logical CPU tagged `CoreKind::Performance`; "Pin to Performance cores" wrong on 12th-gen+ | `sys/inner/topology.rs:54-66` | 02-C4 |
| H-09 | `Kind(Cache)` on non-X3D silently clears existing CPU sets via `SetProcessDefaultCpuSets(None)` while still applying the rest of game-x3d's aggressive Game Mode | `sys/inner/apply.rs:522-533`, `policy.rs:343-436` | 05-§4/§5 |
| H-10 | No sleep/resume handling anywhere (zero `WM_POWERBROADCAST` / `PBT_*` refs); power-plan reverts can surprise after laptop battery transitions; Game Mode state diverges from reality after resume | (grep across tree) | 04-HIGH-1 |
| H-11 | No WTS session change / RDP handling; multi-user / FUS races two trays into the same admin pipe with no session arbitration | `service/main.rs:73-74` | 04-HIGH-2 |
| H-12 | CPU topology captured once at startup, only cloned thereafter; no hot-plug refresh | `service/runtime.rs:36`, `engine/lib.rs:67-68` | 04-HIGH-3 |
| H-13 | Service never falls back to session-0 polling if tray dies (`foreground_reporter_seen` flipped permanently true); engine stuck on last game profile until service restart | `engine/lib.rs:1093-1104` | 04-HIGH-5 |
| H-14 | Tray IPC `OpenOptions::open` has no timeout; hangs unbounded during service restart, can hang UI thread | `tray/main.rs:6087-6104` | 04-HIGH-4/7 |
| H-15 | `BufReader::lines` on IPC has no max-line cap → multi-GB JSON line OOMs LocalSystem service | `service/runtime.rs:302` | 03-Med-3 (DoS) |
| H-16 | Status pipe `Subscribe` is uncapped → 255-instance kernel pipe-instance DoS by any Authenticated User | `service/pipe.rs:116`, `ipc/lib.rs:159` | 03-High-2 |
| H-17 | `ReportForeground.pid` not cross-checked against reported `exe_name` → elevated tray can drive arbitrary PID's profile application via LocalSystem reach | `engine/lib.rs:943-952, 1021` | 03-High-3 |
| H-18 | Aggressive default `game-x3d` pre-armed for BF6/Valorant/Fortnite; no first-run opt-in; first launch of any of those = 30 services stopped + 24 processes suspended + power plan flip + WSearch / BITS / OneDrive disrupted | `policy.rs:454-470, 343-436` | 07-#1, 05-§6 |
| H-19 | Curated safe-list is baked into the gamemode crate; user-added entries are silently dropped at apply time (no UI feedback) | `gamemode/safe_lists/*.json`, `tray/main.rs:5200` | 07-#8 |
| H-20 | Engine is essentially untestable: 2,200-line `lib.rs` has 2 tests (both on a helper). No trait abstraction for syscall surface, no injectable clock | `engine/lib.rs` | 08-§2/§13 |
| H-21 | 30 `std::sync::Mutex::lock().unwrap()` calls in tray — any background-thread panic holding the lock cascades into UI panic. `parking_lot::Mutex` already a workspace dep | `tray/main.rs` (30 sites) | 08-§4 |
| H-22 | No per-PID CPU history / sparkline in process detail panel (Process Explorer parity gap) | `tray/main.rs:4143-4341` | 06-#4 |
| H-23 | No undo for individual actions; Game Mode panic is the only revert path; per-process priority changes have no undo memory | (absent) | 06-#6 |
| H-24 | No first-run / onboarding; default landing tab = Processes (should be Status); empty Rules tab dumps user into a blank state with no CTA | `tray/main.rs:158-165, 2137` | 06-#13, 07-#11 |
| H-25 | Table has no keyboard navigation; egui's accessibility-kit is off; screen readers see one canvas | `tray/main.rs:3144-3147` | 06-#12 |
| H-26 | No code signing → guaranteed SmartScreen warning for a tool that asks for admin + installs LocalSystem service | `.github/workflows/ci.yml:64-81` | 10-§7, 03-Low-2 |
| H-27 | Installer is unsigned PowerShell that requires `cargo` in PATH — source-tree installer, not a binary installer; no Add/Remove Programs entry; no MSI/Inno | `install.ps1`, `README.md:104-119` | 10-§8 |
| H-28 | IPC Event enum captures ~20% of engine actions; ProfileApplied/Reverted, GameModeEntered/Exited, AffinityRuleFired, ActionFailed, every per-service/process action all unobserved | `ipc/lib.rs:338-364` | 09-§1 |
| H-29 | No file-sink logging anywhere; tracing writes to stderr → `\Device\Null` under SCM; no on-disk log to send to support | `service/main.rs:97-102` | 09-§8, 08-§5 |
| H-30 | 32 `warn!`/`error!` sites in engine, zero surfaced as user-visible events; silent apply/revert failures look like success | `engine/lib.rs` (32 sites) | 09-§7 |
| H-31 | No quantified-impact metrics anywhere — Activity card shows "currently restraining N" but no cumulative "X demotions today" | `tray/main.rs:1581` | 09-§5 |
| H-32 | Service uninstall doesn't stop the service first → `Marked for deletion` zombie blocks re-install until reboot when README's recommended `framesage.exe uninstall` is used standalone | `cli/main.rs:271-284`, `README.md:130-134` | 10-§4 |
| H-33 | Default column widths sum to ~1255px but central panel capped at `MAX_CONTENT_WIDTH = 980.0` → guaranteed horizontal overflow on first run in the most-data-dense tab. Cap also wastes ultrawide real-estate | `tray/main.rs:878, 2969-2982` | 06-#1 |
| H-34 | Filter searches `exe_name` only despite Description / Company / User columns being rendered → typing "microsoft" finds nothing useful | `tray/main.rs:2852-2854` | 06-#2/#3 |

### Medium

| # | Finding | File:line | Dimension |
|---|---|---|---|
| M-01 | `set_affinity_mask` ignores system_mask returned by `GetProcessAffinityMask`; mask not intersected with what process can legally run on → half-applied profile + log spam on stale hand-edited bits | `sys/inner/apply.rs:419-432, 434-442` | 02-C5/H4 |
| M-02 | `set_affinity_mask` (apply path) has no `mask != 0` guard at line 142 (the public IPC variant does) | `sys/inner/apply.rs:139-144` | 02-H4 |
| M-03 | Per-CPU sampling caps at 256 CPUs → rejects 512-core dual-socket EPYC | `sys/inner/process.rs:567, 583-587` | 02-H5 |
| M-04 | `K32EmptyWorkingSet` exposed for arbitrary PIDs incl. Defender → trimming MsMpEng forces disk-I/O storm as it page-faults sig DB | `sys/inner/apply.rs:146-149, 342-356` | 02-H1 |
| M-05 | Game-mode manual thread-loop suspend is non-atomic; inconsistent with `NtSuspendProcess` used elsewhere in process_actions | `sys/inner/game_mode/process.rs:73-120` vs `sys/inner/process_actions.rs:23-30` | 02-M1 |
| M-06 | Windows Update pause doesn't capture/restore prior user pause window | `sys/inner/game_mode/windows_update.rs:14-15` | 02-M3 |
| M-07 | Hot-reload `notify` watcher can fire mid-write of policy.json (editor truncate→write→close pattern); fallback behavior keeps last policy on parse error (good) | `service/runtime.rs:134-150` | 02-N1, 04-MED-7 |
| M-08 | OpenProcess errors collapsed to `Ok(None)` indiscriminately (no distinction between INVALID_PARAMETER / ACCESS_DENIED) — masks legitimate bugs | `sys/inner/process.rs:147-151, 199-202, 356-359, 422-425, 652-655` | 04-HIGH-6 |
| M-09 | Admin pipe uses tokio default NULL SECURITY_ATTRIBUTES → actual DACL is LocalSystem's `TokenDefaultDacl` (grants World GENERIC_READ), not the "Admins+SYSTEM only" comments claim. Currently unexploitable; comment/reality drift | `service/pipe.rs:149-165` | 03-Med-2 |
| M-10 | Status pipe leaks process inventory + `DOMAIN\username` to any Authenticated User → multi-user RDP host info disclosure | `ipc/lib.rs:236-312`, `service/runtime.rs:337-340` | 03-Med-1 |
| M-11 | IPC string fields not NUL-checked; latent — no current Win32 sink, but policy.json round-trip persistence means a future consumer could inherit a poisoned `exe_name` | `service/runtime.rs:304`, `engine/lib.rs:493` | 03-Med-4 |
| M-12 | `version_info_cache` unbounded; "never evict" comment wrong in two scenarios: in-place upgrade leaves stale company/desc strings; `%TEMP%\<uuid>\foo.exe` one-off paths accumulate forever | `engine/lib.rs:128, 692-705` | 01-M4, 04-MED-2 |
| M-13 | `SetPolicy` policy in-memory mutation outlives a failed save → engine applies new policy while next restart silently reverts it; user gets banner but situation is brittle | `service/runtime.rs:503-523, 435-449, 469-486` | 04-MED-3 |
| M-14 | Atomic rename across volumes fails (`%ProgramData%` symlinked to different volume for redirected corporate folders) → user loses in-memory edits | `core/policy.rs:285-296`, `gamemode/journal.rs:160-167` | 04-MED-4 |
| M-15 | Apply-failure path has no exponential backoff → log spam every focus return when same PID fails to apply | `engine/lib.rs:1832-1841, 1783-1791` | 04-MED-5 |
| M-16 | Game Mode crash-recovery never re-checks suspended PIDs' exe names; runtime path has the defense, recovery path doesn't | `engine/lib.rs:1061-1080` vs `lib.rs:1413-1432` | 04-MED-6 |
| M-17 | Hot-reload accepts policies with dangling ProfileId references; engine logs and silently falls through | `service/runtime.rs:181-191`, `engine/lib.rs:1759-1764` | 04-MED-7, 07-#9 |
| M-18 | No multi-sample hysteresis on ProBalance restrain side (only on restore side) — single-sample over threshold triggers immediately, biases toward over-restraining | `engine/probalance.rs:191-202` | 05-§3 |
| M-19 | No revert-state-drift detection — silently overwrites user-made priority/affinity changes from Task Manager when foreground moves away from a non-persistent profile | `engine/lib.rs:1723-1736, 1999-2006` | 05-§7 |
| M-20 | BITS stop in default game-x3d has no in-flight-transfer check → abandons OneDrive / Windows Update / Defender-delta transfers mid-byte | `policy.rs:362`, `gamemode/safe_lists/services.json:71-75` | 05-§6 |
| M-21 | Hard affinity is now applied alongside CPU Sets (apply.rs:106-117 admits "didn't survive hardware validation") — README's "CPU Sets is the correct primitive" claim is no longer accurate | `sys/inner/apply.rs:106-117`, `README.md:226` | 05-§4 |
| M-22 | Activity events don't carry the matched-rule index → "bf6.exe → game-x3d" never says "...because rule #3 matched" | `tray/main.rs:5849-5879` | 06-#5 |
| M-23 | `Profile.persistent` (load-bearing flag on game-x3d) is not displayed or editable in tray UI — power-user knob hidden behind hand-edit | `tray/main.rs:4917-4965, 5057-5128` | 07-#3 |
| M-24 | ProBalance thresholds, `tick_ms`, `background_profile`, `ignore_processes` are read-only labels; only Enable/Disable toggle exists in UI | `tray/main.rs:1581-1661`, `core/policy.rs:142-160` | 07-#4 |
| M-25 | No Reset-to-defaults, no Export/Import; CLI has no policy mutation verbs (`policy export|import|add-rule`) → GPO / scripted rollout requires direct file munging | `cli/main.rs:32-66` | 07-#5/#6/#7 |
| M-26 | No save-time lint on rules referencing unknown profile ids, or `Ccd(7)` on a single-CCD chip | `tray/main.rs:2097-2109, 5466-5468` | 07-#9 |
| M-27 | Profile editor `affinity_mask` text field accepts `0x0` and oversize → save silently writes a no-cores-pinned profile (picker correctly blocks) | `tray/main.rs:5472-5484` | 07-#2 |
| M-28 | `framesage-sys` depends on `framesage-gamemode` (inverted arrow); `framesage-core` depends on `tracing` (data crate with runtime dep) | `crates/sys/Cargo.toml:30`, `crates/core/Cargo.toml:15` | 08-§1 |
| M-29 | `tray/main.rs` at 6,241 LOC still hosts 3 obvious extractable modules (IPC client ~325 LOC, tray-menu builder ~150 LOC, Processes-tab UI ~1,200 LOC) | `tray/main.rs` | 08-§3 |
| M-30 | `list_process_snapshots` holds write guard across ~200 OpenProcess calls — status pipe handlers serialise behind it | `engine/lib.rs:567` | 08-§6 |
| M-31 | No SCM failure-actions in install verb; no service dependencies declared (`RpcSs` would be conventional); no SID hardening | `cli/main.rs:239-258` | 10-§3 |
| M-32 | No policy schema migration on upgrade; new required field → v0.4 policy fails to load → service falls back to defaults (silently discards user rules) | `service/runtime.rs:497-517` | 10-§6 |
| M-33 | README documents no uninstall residue, no "complete removal" recipe, no admin/LocalSystem disclosure, no SmartScreen warning | `README.md:102-145` | 10-§14 |
| M-34 | Re-launching elevated tray loses in-progress policy_draft → user starts a rule unelevated, clicks elevate banner, draft gone | `tray/main.rs:policy_draft` | 06-#14 |
| M-35 | `Profile.suspend_processes` / `stop_services` in user-authored profiles produce silent drops on unknowns — entry persisted to policy.json but never executed | `tray/main.rs:5200, 5207` | 07-#8 |

### Low / Polish (style, future-proofing, minor wins)

> **Status re-audit (2026-08-15):** the following Low items are now **resolved** in the audit-08 follow-up pass (see `08-code-quality.md` Status re-audit block):
> - **L-26** — MSRV check: `rust-version` corrected to **1.88** (locked deps `image 0.25.10`, `time 0.3.47`/`time-core 0.1.8`, `pxfm 0.1.29` require it) + a `msrv` CI job pinned to `dtolnay/rust-toolchain@1.88` running `cargo check --locked`.
> - **L-28** — inline tray durations promoted to named constants (`RECONNECT_BACKOFF`, `POLL_INTERVAL_VISIBLE/HIDDEN`, `MAX_RECENT`, `IDLE_REPAINT_INTERVAL`, `SHOW_WINDOW_WATCHER_BACKOFF`).
> - **L-29** — `MAX_RECENT` promoted to a module-level constant.
> - **L-30** — service tick interval promoted to `TICK_INTERVAL`.
> - **L-27** — `pub` → `pub(crate)`: **false positive** (both methods have cross-crate callers in `framesage-service`).
>
> Remaining live Low items: **L-23** (`windows-sys` version sprawl — transitive, cosmetic).

| # | Finding | File:line | Dimension |
|---|---|---|---|
| L-01 | Service master tick is 300 ms even when nothing's happening; could be coarser intervals on individual loops | `service/runtime.rs:57` | 01-M1 |
| L-02 | Foreground reporter retries on transient pipe failure without backoff (4× / sec during service restart) | `tray/main.rs:6072-6076` | 01-M2 |
| L-03 | `MAX_RECENT = 1000` ring uses `Vec::drain(0..n)` instead of `VecDeque` | `tray/main.rs:5893-5896` | 01-M7 |
| L-04 | `iter_pids` returns a fresh `Vec<u32>` per caller (3× per tick path) | `sys/inner/process.rs:106` | 01-M5 |
| L-05 | Power-plan switch doesn't capture/persist prior plan across hibernation transitions (S0ix modern standby) | `sys/inner/game_mode/power_plan.rs:49-63` | 02-M2 |
| L-06 | `acquire_singleton` re-creates mutex per retry instead of `OpenMutexW + WaitForSingleObject` | `tray/win32.rs:125-153` | 02-M7 |
| L-07 | `install.ps1` self-elevation has classic script-TOCTOU (file replacement between non-elevated check and elevated relaunch) | `install.ps1:27-33` | 03-Low-3 |
| L-08 | `tick_handle.abort()` doesn't await; in-flight blocking syscalls return to a dropped frame (sound, but no explicit drain) | `service/runtime.rs:97-100` | 04-LOW-2 |
| L-09 | Foreground-reporter thread has no cancel signal; tray exit kills as daemon (cosmetic but real briefly-running pid after window close) | `tray/main.rs:6044-6079` | 04-HIGH-7 |
| L-10 | Asymmetric detail-panel actions vs right-click menu (panel lacks Show in Explorer / Copy / Suspend tree / Trim WS) | `tray/main.rs:4252-4339` | 06-#16 |
| L-11 | "Apply profile now" in bulk falls back to ApplyProfileForeground (single-shot to foreground) — code comment acknowledges; menu lies about what it does | `tray/main.rs:3193-3207` | 06-#17 |
| L-12 | No "Activity" item in tray View submenu | `tray/main.rs:5650-5654` | 06-#8 |
| L-13 | Striped table rows barely visible on dark theme | `tray/main.rs:2966` | 06-#1 |
| L-14 | No light mode / no system-theme follow toggle | `tray/theme.rs:40` | 06-#10 |
| L-15 | Esc doesn't close modals; `render_terminate_confirm_modal` / `render_affinity_picker_modal` lack key handlers | `tray/main.rs:898-952, 959-` | 06-#12 |
| L-16 | Two parallel rule systems (AppRule + AffinityRule) visually disjoint in Rules tab — look like two unrelated features | `tray/main.rs:2205-2321` | 06-#7 |
| L-17 | AppRule list rendered as plain `ui.label()` instead of TableBuilder; hard to scan with >10 rules | `tray/main.rs:2142-2178` | 06-#7 |
| L-18 | `>` ASCII prefix on foreground rows instead of an actual triangle (default font glyph coverage gap) | `tray/main.rs:3097-3101` | 06-#2 |
| L-19 | No Ctrl-F to focus filter | `tray/main.rs:2769` | 06-#3 |
| L-20 | Filter / search not regex / fuzzy | `tray/main.rs:2769` | 06-#3 |
| L-21 | Game Mode editor doesn't surface rich rationale strings from safe-list JSON (only visible via CLI `game-mode safe-list`) | `tray/main.rs:5196-5208` | 07-#10 |
| L-22 | No Game Mode dry-run / preview | (absent) | 07-#12 |
| L-23 | `windows-sys` appears in 6 different versions in lockfile (transitive); compile-time bloat | `Cargo.lock` | 08-§10 |
| L-24 | `parking_lot` is workspace dep but tray uses `std::sync::Mutex` — pick one | `tray/main.rs` | 08-§10 |
| L-25 | Clippy not run against MSVC target (Linux clippy uses `x86_64-pc-windows-gnu`; native windows-build runs `cargo test` only) | `.github/workflows/ci.yml` | 08-§9 |
| L-26 | No MSRV check (`rust-version = "1.80"` declared, no pinned-toolchain `cargo check --locked`) | `Cargo.toml:27`, `.github/workflows/ci.yml` | 08-§9 |
| L-27 | `policy_snapshot` / `recover_orphan_journal` are `pub fn` but only used from the service binary; could be `pub(crate)` | `engine/lib.rs:441, 1061` | 08-§14 |
| L-28 | Magic-number discipline in engine is exemplary; tray has scattered inline `Duration::from_millis(N)` calls | `tray/main.rs:477, 612, 5794, 5945-46, 6045` | 08-§11, 01-M5/M6 |
| L-29 | `MAX_RECENT = 1000` is a local `const`, should be module-level alongside `SYSTEM_HISTORY_LEN` | `tray/main.rs:5892` | 08-§11 |
| L-30 | Service tick interval `Duration::from_millis(300)` should be a named constant (load-bearing for ProBalance sample math) | `service/runtime.rs:57` | 08-§11 |
| L-31 | `OwnedHandle` newtype with `Drop`+`CloseHandle` not yet in `crates/sys/src/inner` (RAII pattern from `tray/win32.rs:90-103` should propagate) | `crates/sys/src/inner/**` | 02-L2, 08-§7 |
| L-32 | `close_handle` swallows `CloseHandle` failures (double-close / invalid-handle indicators) instead of `debug_assert!` | `sys/inner/process.rs:667-671` | 02-L1 |
| L-33 | Notifications: none; no earned toast for "Game Mode failed to restart service X" | (absent) | 06-#9 |
| L-34 | `policy_snapshot_lookup_rule` clones a rule per user-interaction lookup | `tray/main.rs:544-551` | 01-M3 |
| L-35 | `applied_count`/`new_marks` Vec allocated per `set_affinity_rule` apply-to-live (rare path) | `engine/lib.rs:484-485` | 01-M6 |

---

## What's already good (genuine credit, not "fine")

These are the things the codebase does **better than typical** for the category, and that any future refactor must preserve:

- **Workspace layering and the `sim` crate.** `core` is pure data with zero Win32 imports, `engine` has zero `windows::` / `eframe` / `egui` imports, the dependency arrows are clean and one-directional. `sim` plus the portable-test CI job means Mac/Linux iteration is real, not aspirational. [08-§1, §15]
- **`probalance::decide`.** Textbook testable state machine — pure function, injected clock, 10 unit tests covering under-threshold no-op, top-hog restraint, foreground/managed/safe-list/user-ignore skip, AboveNormal refusal, dwell window, restore-after-quiet, restore-on-foreground-takeover, restore-on-exit. **The rest of the engine should look like this.** [08-§2]
- **The two-pipe ACL split.** Admin pipe + status pipe with separate SDDLs, `Request::is_read_only` as an exhaustive match enforced server-side as defense-in-depth, unit test locks the contract, `FILE_FLAG_FIRST_PIPE_INSTANCE` defeats pipe-name squatting, pre-armed next-instance closes the accept race. Auditable, short, intentional. [03 (intro), 04 (intro)]
- **Crash-safe Game Mode journal with intent-first writes.** Journal written BEFORE any kernel mutation, schema-versioned, fail-safe on parse error (`engine/lib.rs:1065-1069` deletes unparseable orphan instead of crash-looping). The rare implementation that gets this ordering right. The on-revert `journal.delete()` is the gap (C-07) — the underlying pattern is excellent. [04, 09]
- **Atomic policy writes + UTF-8 BOM tolerance.** `core/policy.rs:285-296` temp+rename, `policy.rs:240` strips BOM (catches PowerShell 5.1's `Set-Content -Encoding UTF8` output). Real footgun closed. [04, 07]
- **PID-reuse defense via exe-name compare.** Captured at apply time in `AppliedRecord.exe_name`, re-checked in the 2s persistent reassert and the affinity-rule sweep. Caught a class of bug that bit Process Lasso historically. [04, 05]
- **Singleton mutex with handoff grace + cross-instance show-window event.** 3s wait for an exiting prior tray, secondary signals primary to focus rather than failing. RAII `SingletonGuard` auto-releases on crash. The pattern to propagate. [04, 06]
- **No telemetry, no network surface, no `LoadLibrary` / `SearchPath` / `SetCurrentDirectory`.** Zero DLL-search-order hijack vector. Zero phone-home risk. Auditable for what it isn't, not just what it is. [03]
- **Safe-list architecture** itself — JSON, schema-versioned, rationale per entry, deny>allow, case-folded, tested explicitly for csrss/lsass/MsMpEng. Covers AV / anti-cheat / RPC / kernel shell consistently. The **shape** is right; the gap is enforcement at every kernel-write entry-point (Theme A). [02, 05, 07]
- **Comment culture.** Comments explain *why*, not what. Session 0 isolation, POE2 self-reconfiguring affinity, hardware-validated CPU Set bypass, "Reconcile, don't event-chase" — design rationale captured in-source. Rare in this category. [08-§8]
- **Universal RAII handle discipline.** Every `OpenProcess` / `OpenThread` / `CreateToolhelp32Snapshot` paired with `CloseHandle` on both success and error paths. Currently manual; the candidate `OwnedHandle` newtype (L-31) would lock this in for the future. **No timeBeginPeriod calls anywhere** — no system-wide timer-resolution abuse. [01, 02]
- **Live performance band.** Per-core matrix + 60-sec sparkline + hover-tooltips for top-5 hottest cores. Meaningfully better than Process Lasso (no sparkline) and Task Manager (no per-core bar matrix in this footprint). Not eye-candy — answers "did the load land on the right CCD?" in one glance. [06-#15]
- **Multi-select with Ctrl/Shift + bulk context menu** evaluated at end-of-frame so modifier state is captured at click time. Pluralised menu labels ("Suspend 4 processes"). Matches Task Manager / Process Explorer convention exactly. [06-#17]
- **X3D-aware affinity submenu + session-sticky "Remember as rule" toggle + 📌 indicator on rule-pinned rows.** Cleanest such bridge between one-shot and persistent pin I've reviewed. Process Lasso forces a separate dialog. [06 (well-done), 07]
- **Manual override re-evaluation** closes the "stranded Game Mode after manual-off" bug end-to-end. [04]
- **`#[serde(default)]` everywhere on additive policy fields** — v0.1 policy.json loads under v0.5. The schema-migration gap (M-32) is real but the data-shape compatibility floor is right. [04, 07]
- **CI matrix.** Cross-check on Linux + native Windows build + tests + clippy `-D warnings` + rustfmt `--check`. The portable-test job exercises core/gamemode/ipc/sim on Linux. Catches the bulk of what would break before a Windows-only contributor sees it. [08-§9]

---

## Pointers to the deep-dive files

| # | Dimension | File |
|---|---|---|
| 01 | Self-footprint (CPU, RAM, wakeups, handles) | `audit/01-self-footprint.md` |
| 02 | OS interaction correctness (Win32/NT API surface) | `audit/02-os-correctness.md` |
| 03 | Privilege & security (IPC, ACLs, EoP vectors) | `audit/03-privilege-security.md` |
| 04 | Reliability (sleep/RDP/crash/upgrade/recovery) | `audit/04-reliability.md` |
| 05 | Optimization-logic correctness (does it actually help?) | `audit/05-optimization-logic.md` |
| 06 | UX/UI (process list density, undo, accessibility, first-run) | `audit/06-ux-ui.md` |
| 07 | Configurability (defaults, footguns, profile system, import/export) | `audit/07-configurability.md` |
| 08 | Code quality (layering, testability, error handling, threading) | `audit/08-code-quality.md` |
| 09 | Observability for the user (audit log, on-disk logs, impact metrics) | `audit/09-observability.md` |
| 10 | Install / update / uninstall (uninstall residue, signing, MSI) | `audit/10-install-uninstall.md` |

Phase 2 (grouped improvement plan: Must-fix / High-leverage / Structural / Polish, with scope+files+risk+verification per item) will be produced next, for your approval before any code changes.
