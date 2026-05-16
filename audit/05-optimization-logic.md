# Audit 05 — Optimization Logic

**Question:** Does the engine ACTUALLY help, or is it cargo-cult heuristics with no measurement?

**Verdict (one-line):** Open-loop ruleset with a clean state machine and good safety architecture, but with a small number of real correctness bugs around alt-tab thrashing, non-X3D fallback, and at least two service-stop landmines. Nothing in here measures frametimes, DPC latency, context switches, or CPU contention end-to-end — so every claim of "benefit" remains faith-based.

---

## 1. Does the engine MEASURE anything?

**Finding:** No closed-loop measurement of outcomes. The engine reads `GetSystemTimes` + per-PID `GetProcessTimes` to compute CPU% (`crates/engine/src/lib.rs:567-820`, `:1121-1364`), and that single signal feeds both the Processes-tab UI and ProBalance's restrain trigger. That is the *only* thing the engine "measures." The README v0.3 roadmap explicitly lists ETW + PresentMon as TODO (`README.md:208-211`).

**Concrete gap:** there is no DPC/ISR sampling, no hard-fault counting, no disk-queue depth, no frame-time, no scheduler-runqueue depth, no per-thread context-switch rate. Every `apply_profile` call (`lib.rs:2037`) writes settings and walks away — no after-the-fact comparison of the metric the change was supposed to improve. The codebase has no "did this rule help?" feedback loop and no telemetry mechanism that would let a future one exist.

**Severity:** **Medium for marketing claims, Low for correctness.** The static rules (priority class, IO priority, power throttling) are uncontroversial Win32 mechanisms; they do *something*. Whether that something is net-positive on a given workload is unmeasured. Calling this "ProBalance" or "optimization" sets the user expectation that an outcome is being optimized for. It isn't.

---

## 2. Thrashing prevention on alt-tab

**Files:** `crates/engine/src/lib.rs:1703-1854` (`reconcile`).

**Observed logic:** Each tick (300 ms default, `crates/core/src/policy.rs:177`) reads the current foreground. If `new_pid == s.current_foreground` we early-return at `lib.rs:1711`. Otherwise we revert the previous PID's state (unless its profile is `persistent`) and apply the new PID's profile.

**No debounce, no hysteresis.** Tray polls foreground at 250 ms (`crates/tray/src/main.rs:6045`), service ticks at 300 ms, so worst-case latency is ~550 ms but worst-case *thrash rate* is roughly one revert+apply pair per tick that observes a focus change.

**Concrete scenario.** User alt-tabs Browser → Game → Browser → Game → Browser → Game within 1 second. Game is `game-x3d` (persistent), so it stays pinned across the trip away. Browser hits the rule fallback (`default_profile = perf`, `policy.rs:475`), which is non-persistent — so every "browser focused" tick: revert perf-record + reapply perf-record. Each apply is `OpenProcess` + 3-4 `SetProcessInformation` calls + `CloseHandle`. Three round-trips = ~12-20 syscalls per second against `chrome.exe`. Modest, but real, and zero benefit (perf profile just sets Performance throttling + Normal class + Normal IO — the values most processes already have).

**Severity:** **Low-Medium.** Not destabilising, but it's unnecessary churn that contradicts the README claim that polling reconcile is "more robust *and* simpler" (`lib.rs:21-24`). Process Lasso de-dupes its restraint events on the same edge. A 2-3 sample hysteresis (foreground PID stable for N ticks before applying) would eliminate this.

---

## 3. ProBalance correctness

**File:** `crates/engine/src/probalance.rs`, `crates/engine/src/lib.rs:1121-1364`.

What's done well:
- **Foreground skip:** `probalance.rs:194` — never restrains foreground.
- **Managed-PID skip:** `probalance.rs:195` — won't fight rule-managed profiles.
- **Safe-list skip:** `probalance.rs:198` (reuses `safe_list.denied_process_names()` via `lib.rs:1261-1265`). Covers csrss/dwm/audiodg/MsMpEng/anti-cheat. **This is solid.**
- **Refuses to demote AboveNormal/High/Realtime:** `probalance.rs:245-251`. Good — won't elbow an explicit media-app priority.
- **Dwell window:** `probalance.rs:131, 156` — once restrained, won't restore for `min_restrain_ms` (default 1500 ms, `policy.rs:169`). Prevents short-flap restore.
- **One-step demotion only:** Normal→BelowNormal, BelowNormal→Idle. Reasonable.
- **Restore on PID-exit / foreground-promotion / load-drop / managed-now:** `probalance.rs:143-178`. Correct.

**Gaps:**

- **No multi-sample hysteresis on the restrain side.** `probalance.rs:191-202`: a process becomes a "hog" the moment ONE sample crosses `hog_cpu_threshold_percent` (50% of one CPU by default). A 1-second sampling cadence (`lib.rs:190`) plus a single-sample trigger means any one-second burst over threshold will trigger a restraint. There is no equivalent of the dwell-on-restore on the restrain side — no "must be over threshold for N consecutive samples." This is asymmetric and biases toward over-restraining. **Severity: Low-Medium.**

- **No anti-flap after restore.** A PID that was just restored is immediately re-eligible for restraint on the next sample if it's still busy and the system is contended. The dwell only protects the "stays restrained" phase, not the "after we let it go" phase. **Severity: Low.**

- **`system_cpu_percent` is derived from sampled-PID sum, not `GetSystemTimes`** (`lib.rs:1251-1256`): "% of one CPU summed across all sampled PIDs, divided by CPU count." This undercounts kernel time we couldn't open (protected processes), so the threshold trigger is biased low. Note: `list_process_snapshots` does this correctly via `GetSystemTimes` (`lib.rs:764-777`); ProBalance should share that path. **Severity: Low.**

- **Disabled by default** (`policy.rs:166`). So in shipping form, ProBalance does nothing unless the user opts in. That's honest, but the README treats it like a headline feature.

---

## 4. Default rules sanity (game-x3d)

**File:** `crates/core/src/policy.rs:343-436`.

What the seeded rules apply (bf6.exe, VALORANT-Win64-Shipping.exe, FortniteClient-Win64-Shipping.exe → `game-x3d`):
- `cpu_sets = Kind(Cache)` — pins to X3D / cache-CCD
- `priority_class = AboveNormal`
- `io_priority = High`
- `power_throttling = Performance`
- `persistent = true` (good — survives alt-tab)
- Full Game Mode: hide taskbar, switch to HighPerformance plan, stop 27 services, suspend 28 processes, pause Windows Update.

**Severity issues:**

- **`Kind(Cache)` on non-X3D hardware:** `crates/core/src/topology.rs:189-192` resolves to whatever CPUs are tagged `CoreKind::Cache`. On a single-CCD Ryzen, an Intel non-hybrid box, or any chip where the cache-CCD detector at `topology.rs:140-176` didn't promote one CCD over the other, the resolve returns empty. Then `crates/sys/src/inner/apply.rs:95-136`: `cpuset_ids_for_indices(&[])` returns `Ok(vec![])`, then `set_default_cpu_sets(handle, &[])` calls `SetProcessDefaultCpuSets(handle, None)` which **clears any existing default CPU sets** (`apply.rs:522-533`). Then `hard_mask = 0` and the hard-affinity branch at `apply.rs:126` is skipped. Net effect: a non-X3D user running Valorant gets their CPU sets *cleared* by us, while they still pay the priority + IO + power-throttling + full Game Mode tax. **Severity: Medium.** Should detect "no cache CCD on this machine" and fall through to `TopRanked(N)` or `All` to avoid the silent clear-and-no-pin.

- **Hard affinity + CPU Sets:** `apply.rs:106-117` admits the README's "CPU Sets, not affinity" stance "didn't survive contact with hardware." Now we ALWAYS apply a hard `SetProcessAffinityMask` alongside the soft hint when `profile.cpu_sets` is set. That removes the README's stated benefit (no starvation under contention). Pragmatically defensible — the X3D CCD has 16 threads, plenty of headroom — but the README is now lying about the mechanism. **Severity: Low for correctness, High for honesty.**

- **`priority_class = AboveNormal` on persistent games:** uncontroversial for foreground games. Note this *interacts* with ProBalance: ProBalance refuses to touch AboveNormal+ (`probalance.rs:245`), so when game-x3d is on a game, ProBalance won't ever clamp the game even if it were misclassified as a "hog." Good.

- **Aggressive Game Mode applied to non-X3D users:** the rule fires by exe name. A user on a 10-core non-X3D Intel desktop launching Valorant gets 27 services stopped, 28 processes suspended, taskbar hidden, power plan flipped, Windows Update paused — for *no* CPU-pinning benefit (see Kind(Cache) issue above). **Severity: Medium.** Defaults should be conservative; right now the default behaviour assumes X3D hardware.

---

## 5. CpuSelector::Kind(Cache) on non-X3D systems

Covered in §4. Concretely: empty indices → `set_default_cpu_sets(handle, None)` clears existing CPU sets (`crates/sys/src/inner/apply.rs:522-533`), and `set_affinity_mask_for_pid` would refuse `mask == 0` (`apply.rs:370-374`) — but the call path at `apply.rs:126` already short-circuits when `hard_mask == 0`, so no error surfaces. Silent no-op pin with side effects (clears prior CPU sets, applies all non-CPU knobs). **Severity: Medium.**

---

## 6. Game Mode action safety

**Allowed list inspected from `crates/gamemode/src/safe_lists/services.json` and `processes.json`.**

What's done well — the denylist is genuinely good (`services.json:251-326`, `processes.json:401-506`): WinDefend, MpsSvc, vgc, EasyAntiCheat, BEService, AudioSrv, RpcSs, gpsvc, csrss, dwm, audiodg, nvcontainer, MsMpEng all permanently denied. dependency-aware (AudioSrv + AudioEndpointBuilder both listed).

**Real landmines in the default game-x3d profile (`policy.rs:355-391`):**

- **BITS** stop (`policy.rs:362`). BITS isn't only Windows Update — it's also the transport for **Windows Backup, OneDrive sync, Defender definition deltas, MS Store update downloads, Intune/MDM**. Stopping it during a session is documented "safe" in `services.json:71-75`, and Windows will restart it on demand. BUT: stopping BITS aborts any in-flight transfer mid-session. A user with a 50 GB game-update download in progress when they launch Valorant just lost their resume token. **Severity: Low-Medium.** Mitigation: skip the stop if BITS has active jobs. Currently no such check.

- **WSearch** stop (`policy.rs:359`). The rationale in `services.json:11-15` says "no game depends on it." But **Outlook full-text search**, **Explorer search**, and **Start menu search results** all depend on it. A user who alt-tabs to Outlook to find a message during gameplay sees broken search until they exit the profile. Tolerable. **Severity: Low.**

- **SDRSVC + defragsvc** stop (`policy.rs:378-379`). Defrag triggers go through `defragsvc`. If a user has Windows Backup running, stopping `SDRSVC` mid-backup leaves the backup in an indeterminate state. Same as BITS: no "is this service currently doing important work?" check before stopping. **Severity: Low.**

- **`PauseWindowsUpdate` is stubbed** (`crates/gamemode/src/planner.rs:215`, `:71-76`). The plan emits the action; the actual apply may not exist yet. Worth verifying separately. **Severity: Informational.**

- **`FocusAssist` is correctly rejected** with a NotImplemented (`planner.rs:200-212`). Good — honest rejection beats silent no-op.

- **`suspend_processes`** list (`policy.rs:392-430`) — checks out. Cloud-storage syncs, auto-updaters, telemetry, GameBar (suspending GameBar is fine; the *driver-level* overlay path lives in `nvcontainer.exe` which is denied). No anti-cheat or audio process in the suspend list. The denylist at `processes.json:401-506` is what enforces this — the default game-x3d profile only names processes that survive the safe-list partition.

- **Power plan switch:** `HighPerformance` (`policy.rs:431`). On laptops this drains battery; on desktops it disables core parking/lowers C-states. Real measurable input-latency win is documented; tradeoff is acceptable. The planner correctly captures the prior plan for revert (`planner.rs:117-142`). **Severity: None.**

---

## 7. Apply-revert correctness with user-modified state

**File:** `crates/engine/src/lib.rs:1723-1736`, `crates/sys/src/inner/apply.rs:207-300`.

**Observed:** On apply, we snapshot prev priority class, prev affinity mask, prev power throttling, prev memory priority, prev IO priority into `AppliedState`. On revert, we write those values back literally.

**Concrete scenario.** User launches Notepad. Reconcile applies `perf` profile (Normal class). User opens Task Manager, manually sets Notepad to `High`. User alt-tabs away from Notepad. We revert to the captured Normal — overwriting the user's High. The user's manual change is silently lost.

**Severity: Low-Medium.** Process Lasso has the same issue and the user community accepts it as cost of doing business. But we don't currently check "did the current state match what we last applied?" before reverting. A "check that current matches expected applied state, else assume user changed it and skip revert" guard at `revert_record` (`lib.rs:1999-2006`) would close this. PID-reuse defense already exists (`lib.rs:1407-1432`); state-drift defense does not.

---

## 8. CPU sets vs hard affinity — 2 s re-assert window

**File:** `crates/engine/src/lib.rs:184` (`PERSISTENT_REASSERT_INTERVAL = 2 s`), `:1374-1509`.

The 2 s sweep is the *only* defense against a game calling `SetProcessAffinityMask` on itself or CPU-Set advisory drift. Process Lasso does this on every priority/affinity *event* via a kernel callback — instant defeat of the override. Our 2 s polling means a game that re-pins itself at startup runs unpinned for up to 2 s into the loading screen. For most games that's irrelevant (loading screen, not gameplay). For games that re-pin periodically during play (POE2 mentioned at `lib.rs:179-184`), there's a 0-2 s window of being off the X3D CCD every period.

**Severity: Low.** Polling-vs-driver-callback is a real architectural gap, but the window is small relative to gameplay duration. Mention it honestly in the README; don't sell it as parity.

**PID-reuse defense in the sweep** (`lib.rs:1407-1432`): compares live exe name against the captured `expected_exe`, skips and marks stale on mismatch. **Done well.**

---

## 9. Foreground-reconcile latency

Tray polls foreground at 250 ms (`crates/tray/src/main.rs:6045`). Service ticks at `tick_ms` (default 300, `policy.rs:177`). Worst-case latency from focus-change wall-clock to apply: ~550 ms.

For games (foreground change is alt-tab in/out), 550 ms is fine — by the time the game wins focus, the user is still pressing keys; the X3D pin landing 0.5 s in is invisible. For `perf` on Notepad, irrelevant.

**Severity: None for the use case.**

---

## 10. Background-process enforcement cost/benefit

**File:** `crates/engine/src/lib.rs:1525-1701` (`maybe_scan_background_locked`), bounded to every 10 s (`lib.rs:173`).

**Real cost.** On a 600-PID machine: one `iter_pids` (cheap, single ToolHelp snapshot), then for every new PID a sequence of `OpenProcess` + `exe_for_pid` + safe-list check + `apply_profile`. The implementation skips PIDs already in `applied` (`lib.rs:1643`), so steady-state cost is "scan exe names for net-new PIDs since last scan." On a busy machine churning processes (build farms, browsers spawning helper processes), the per-scan cost adds up. The 10 s bound keeps the average down.

**Real benefit.** Apply `eco` profile to a non-foreground background app = Power Throttling Eco + IO Low + Memory Priority Low (`policy.rs:439-447`). Win32 enforces these — Eco mode actively reduces clock targets on hybrid silicon. On a non-hybrid Ryzen, Power Throttling Eco has minimal effect. IO Low is meaningful when the background app is doing disk work concurrent with a foreground game.

**Severity: Low.** Not actively harmful, possibly modestly beneficial on hybrid silicon. The benefit on non-hybrid AMD desktops (the X3D crowd this is sold to) is small. Worth measuring with ETW before claiming a benefit.

---

## What's done well

- **Safe-list architecture** (denylist of csrss, dwm, audiodg, anti-cheats, AV, GPU drivers, RPC, network stack) shared across ProBalance + Game Mode + background scan. Single source of truth at `crates/gamemode/src/safe_lists/*.json`, fed to ProBalance via `denied_process_names()` (`crates/gamemode/src/safe_list.rs:202-204`, used at `crates/engine/src/lib.rs:1261-1265`). **Genuinely solid.**
- **PID-reuse defense via exe-name compare** in the persistent re-assert sweep (`lib.rs:1414-1432`) and affinity-rule sweep (`lib.rs:1468-1482`). Caught a class of bug that bit Process Lasso historically.
- **Journal-first crash safety for Game Mode** (`lib.rs:1920-1939`): intent written before any kernel mutation, recovery reverts what was *planned*. Right architecture.
- **`persistent` flag** (`profile.rs:188-197`) — correct primitive for "game stays pinned across alt-tab." Cleanly applied at `lib.rs:1723-1736`.
- **ProBalance dwell + foreground/managed/safe-list skip** — the four guards that matter are all in place; the gaps are around hysteresis depth, not safety.
- **Power-plan revert info captured up-front** with fail-closed behaviour (`planner.rs:117-142`): if we can't read the current plan, we don't switch — instead of stranding the user on HighPerformance forever.

---

## Bottom line

The engine is a competently-built static-rule applier with thoughtful safety and revert architecture. It does NOT measure anything — every claim of "optimization benefit" is faith-based and contingent on the user happening to be on the hardware the defaults were tuned for (AMD X3D). The biggest correctness gaps:

1. **`Kind(Cache)` silently clears CPU sets on non-X3D hardware** while still applying the rest of game-x3d (priority bump, IO bump, full Game Mode service-stop). Whole-system optimization tax for non-X3D users, no CPU-pin benefit. Fix: detect "no cache CCD on this machine" and fall back to `TopRanked` or skip the cpu_sets branch.
2. **BITS stop has no in-flight-transfer check** — can abandon a Windows Update / OneDrive transfer mid-byte.
3. **No revert-state-drift detection** — silently overwrites user-made priority/affinity changes from Task Manager.
4. **No hysteresis on ProBalance restrain trigger** (only on restore) — single-sample over-threshold restrains immediately.
5. **No closed-loop verification anywhere** — README is honest that this is v0.3 territory, but the absence is the elephant in the room.

The "earns its CPU cost" answer: **probably yes on AMD X3D hardware running listed games**, **probably no on non-X3D Intel boxes** where the marquee pin does nothing and the Game Mode tax still ships.
