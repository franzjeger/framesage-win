# Phase 2 — Improvement Plan (REVISED post-product-positioning correction)

Built against `audit/SUMMARY.md`. Four execution groups, ordered by dependency: Must-fix → High-leverage → Structural → Polish. Cadence (your "Best Practice" pick): per-group stops + one sub-stop inside any group with tightly-related but independent items (~5 stops total).

---

## Product positioning — applied throughout this plan

FrameSage's contract is **"squeeze every drop of performance."** Aggressive Game Mode behavior — stopping non-critical services, suspending non-essential processes, switching power plan, hiding taskbar — is the **feature**, not a footgun to defuse.

The safety bar is **only** three things:

  - **(a) Never destabilize the OS itself.** csrss, lsass, wininit, dwm, kernel-critical, antivirus, anti-cheat, RPC, DHCP, DNS, audio stack, GPU drivers. This is the bundled denylist at `crates/gamemode/src/safe_lists/{processes,services}.json:denylist[]`. It is non-overridable by design. Items 1.1, 1.2, 1.3 enforce *this* line — BSOD prevention, not aggression limits.
  - **(b) Never lie to the user about what will happen.** The user is choosing aggression with informed consent. Pre-session: they see the full list. Post-session: they see the journal. Item 1.4 + item 4.1's onboarding land here.
  - **(c) Always reversible.** Anything Game Mode stops must restart cleanly on exit. Anything suspended must resume. The journal-based revert is sacred — preserve the audit trail religiously.

**Outside of (a)–(c), aggression is on the table.** BITS, WSearch, ClickToRunSvc, OneDrive, Office, telemetry — all fair game for default Game Mode. The journal records when it happened and when it was restored; the user opted in eyes-open.

This reframes several of the auditor's "softening" recommendations as features-not-bugs (re-classified inline below — search for `[reframed]`).

Each item carries: scope · files · risk · verification · effort · finding-IDs it closes.

Effort scale: **S** = ≤1 day · **M** = 2–3 days · **L** = ~1 week · **XL** = multi-week.

Install/uninstall: per your decision, fix the existing PowerShell installer + CLI uninstall now. MSI / WiX / code signing deferred to a separate engagement (not in this plan).

Default rules: a batch proposal lives at the bottom — review and approve before Group 1 starts.

---

## Group 1 — Must-fix (safety-critical, ship-blocking)

These can destabilize the user's system, create security holes, or leave the system stranded. **Nothing else ships before this group is complete.**

### 1.1 — Safe-list enforcement at every kernel-write entry point
**Scope.** Add `SafeList::check_process(exe_name)` **denylist** gate at every per-PID kernel-write entry point. Server-side intersect `Profile.stop_services` / `suspend_processes` against the bundled SafeList **denylist** in `SetPolicy` handler before storing.

**Confirmed denylist scope (does NOT creep into "annoying to stop but technically fine" territory):**
- Kernel/session/shell-critical: csrss, lsass, services, smss, wininit, winlogon, explorer, dwm, sihost, ApplicationFrameHost, RuntimeBroker, LockApp, StartMenuExperienceHost, ShellExperienceHost, TextInputHost, SearchHost, ctfmon, fontdrvhost
- Audio: audiodg
- AV / Security: MsMpEng, NisSrv, SecurityHealthService, WinDefend, MpsSvc
- Anti-cheat: vgc (Vanguard), EasyAntiCheat, BEService
- GPU drivers: nvcontainer, NVDisplay.Container, atiesrxx, RadeonSoftware
- Network/RPC: RpcSs, RpcEptMapper, DcomLaunch, Dhcp, Dnscache, AudioSrv, AudioEndpointBuilder, gpsvc, SamSs, ProfSvc

**Explicitly NOT in the denylist (and the gate does NOT block these — aggression is the feature):**
- BITS, DoSvc, WaaSMedicSvc, UsoSvc — Windows Update / transport
- WSearch — Windows Search
- ClickToRunSvc — Office background
- SysMain, DiagTrack, WpnService, CDPSvc, DPS, WdiServiceHost — telemetry / prefetch
- OneDrive.exe, FileCoAuth.exe, Dropbox.exe, googledrivesync.exe — cloud sync
- GameBar.exe, WidgetService.exe, YourPhone.exe — Windows extras
- OEM updaters (Dell/HP/Lenovo SupportAssist family)

The gate **only** blocks the line (a) "this can destabilize the OS or break anti-cheat" — not the line "this is annoying but reversible."

**Files.** `crates/engine/src/lib.rs:302-425` (IPC actions), `crates/sys/src/inner/apply.rs:57-154` (apply()), `crates/sys/src/inner/process_actions.rs:73-98` (suspend/terminate), `crates/sys/src/inner/apply.rs:328-394` (priority/affinity/trim), `crates/service/src/runtime.rs:488-523` (SetPolicy handler).
**Risk.** Could reject legitimate edge cases (e.g. user wants to set priority on a process whose exe shadows a denylist name). Mitigation: deny is final, but log + surface a Response::Error carrying the JSON rationale string so the user sees *why* it was blocked.
**Verification.** New tests in `engine/lib.rs` driving every IPC variant against PID for `csrss.exe`, `lsass.exe`, `wininit.exe`, `MsMpEng.exe`, `vgc.exe`, `dwm.exe` — all must be rejected with rationale. Companion test: `SetPriority` on `WSearch.exe` / `BITS`-hosted svchost / `OneDrive.exe` is **allowed** (these are the aggression surface). `SetPolicy` with `stop_services: ["WinDefend"]` is rejected; `stop_services: ["WSearch"]` is allowed.
**Effort.** M.
**Closes.** C-01, C-02, C-03, H-17 (ReportForeground exe-name cross-check is a natural addition). M-04 (Defender trim) — closed because MsMpEng is in the denylist.

### 1.2 — Policy file + dir ACL hardening on service startup
**Scope.** On service startup, call `SetNamedSecurityInfoW` on `%ProgramData%\framesage\` to force `O:SY G:SY D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;AU)` (SYSTEM+Admin FullControl, Authenticated Users Read+Execute only). On policy load, verify owner is SYSTEM/Administrators via `GetNamedSecurityInfo`; refuse load + keep last in-memory policy if not.
**Files.** `crates/service/src/runtime.rs:33-49` (startup), `crates/core/src/policy.rs:252-264` (load_or_create_default).
**Risk.** Could break dev workflow where someone runs `framesage-svc --console` from a non-admin shell first (the case that creates the vulnerability). Mitigation: console mode prints a clear warning + creates dir under user's own profile instead.
**Verification.** Unit test the ACL string generator. Manual test: create dir as non-admin first, start service, confirm ACL is re-hardened (or service refuses load with clear log).
**Effort.** S.
**Closes.** C-04.

### 1.3 — SCM FailureActions + tick task watchdog
**Scope.** Two parts:
  1. `cli/main.rs install` calls `ChangeServiceConfig2W(SERVICE_CONFIG_FAILURE_ACTIONS)` with `restart/restart/restart` and a 60s reset period.
  2. Wrap each `tokio::spawn` task body (`tick`, `admin`, `status`, `reload`) in a supervisor loop that catches panics via `std::panic::catch_unwind` (compatible with `panic="abort"` only if we change it to `unwind`). Decision: switch `Cargo.toml:95` to `panic = "unwind"` for the service binary specifically (`profile.release.package.framesage-service` if cargo supports it; else workspace-level with consequences noted).
**Files.** `crates/cli/src/main.rs:239-258`, `crates/service/src/runtime.rs:56-91`, `Cargo.toml:95`.
**Risk.** Switching `panic="abort"` → `"unwind"` adds a small binary-size cost and changes panic semantics (unwinding through Win32 callbacks is undefined). Mitigation: only `framesage-service` flips; other bins stay abort. Alternative: keep abort, rely on SCM auto-restart only — simpler but loses in-process recovery.
**Verification.** Inject a deliberate panic in a debug-only IPC variant; confirm SCM restarts service within 60s and tick resumes. Test that `Cargo.toml` change builds + ships all crates.
**Effort.** S.
**Closes.** C-05, C-06.

### 1.4 — Game Mode journal: append-on-revert to sessions.jsonl
**Scope.** Replace `journal.delete()` in `revert_system_mode_locked` with append to `%ProgramData%\framesage\sessions.jsonl` carrying the full `AppliedActions` + start/end timestamps + revert success per action. Delete the active `game-mode.journal` only after the append succeeds. Add an IPC event `GameModeExited { summary }` so the tray learns about it live.
**Files.** `crates/engine/src/lib.rs:1983-1992`, `crates/gamemode/src/journal.rs:172` (new `append_to_history` method), `crates/ipc/src/lib.rs:338-364` (new Event variant).
**Risk.** sessions.jsonl can grow; cap at 10 MB with rotate-to-`.1` on overflow. JSON line ordering matters for crash recovery; use the same atomic write pattern.
**Verification.** Run a complete game-x3d session in console mode, confirm `sessions.jsonl` gets one entry with the right action counts. Then trigger crash mid-session → recover → confirm two entries (the recovered one + the next clean exit).
**Effort.** S.
**Closes.** C-07.

### 1.5 — Uninstall actually uninstalls
**Scope.** Rewrite `framesage uninstall` (`cli/main.rs:271-284`) to:
  1. `Open` service with `STOP | DELETE`, `Stop` it, wait for `STOPPED` (30s timeout), force-kill `framesage-svc.exe` if still alive.
  2. Open + recover any orphan `game-mode.journal` to revert any pending system mutations.
  3. Delete the three shortcuts (`Start Menu\Programs\FrameSage.lnk`, `Desktop\FrameSage.lnk`, **`Startup\FrameSage.lnk`** — this is the worst residue).
  4. Delete `%LOCALAPPDATA%\Programs\FrameSage\` (recursive).
  5. Prompt (interactive) about deleting `%ProgramData%\framesage\` (policy + sessions.jsonl) — default `No`, preserve unless explicitly removed.
  6. Document the residue policy in README until #1.6 lands.
**Files.** `crates/cli/src/main.rs:271-284`, `README.md:130-145`, possibly add `install.ps1 --uninstall` flag for parity.
**Risk.** Force-killing a service mid-Game-Mode is the same scenario the crash journal already handles — recovery on next start is fine. Shortcut deletion uses well-known paths; safe.
**Verification.** Install + uninstall on a clean VM; verify no FrameSage processes, no shortcuts, no service registration, no binary residue. Verify tray does NOT respawn on next login.
**Effort.** S.
**Closes.** C-08, C-09, H-32.

### 1.6 — Service binary location: %ProgramFiles% + ACL
**Scope.** Switch `install.ps1` install dir from `%LOCALAPPDATA%\Programs\FrameSage\` to `%ProgramFiles%\FrameSage\`. Apply ACL `Administrators:F SYSTEM:F Users:RX` via `icacls` after copy. Update CLI service install path computation. Maintain a migration path: if old install dir exists, copy out → install to new dir → register service against new path → delete old dir.
**Files.** `install.ps1:44, 77-85, 97-111, 124`, `crates/cli/src/main.rs:228-250`, `crates/core/src/paths.rs` if any binary-path resolution.
**Risk.** Existing users with the per-user install need migration. Migration script runs once at upgrade. Multi-user machines now get a single SYSTEM-wide install — the right outcome.
**Verification.** Fresh install lands in ProgramFiles with the right ACL (verify via `icacls`). Service starts. Tray launches. Upgrade from old per-user install correctly migrates + cleans up old dir.
**Effort.** S.
**Closes.** C-10.

### 1.7 — Defaults: keep aggression, fix silent-clear, gate behind informed consent
**Scope.** Two halves:
  1. **Correctness fix only:** when `Kind(Cache)` resolves to an empty CPU set on non-X3D hardware, fall back to `TopRanked(8)` instead of calling `SetProcessDefaultCpuSets(handle, None)` (which clears existing CPU sets). The aggressive Game Mode tax stays; users now also get a sensible best-effort pin instead of a silent no-op.
  2. **Aggression stays at full strength.** No services removed from `game-x3d.stop_services` (BITS, WSearch, ClickToRunSvc, SDRSVC, defragsvc, etc. — all kept). No processes removed from `game-x3d.suspend_processes` (OneDrive/FileCoAuth/Dropbox/cloud-sync/Office helpers — all kept). The Game Mode profile applied to a recognised game IS the aggressive sledgehammer by design.
  3. **Consent gate** — `game_mode` field on the seeded BF6/Valorant/Fortnite rules ships as `None` at install time, populated by the first-run onboarding (item 4.1) to one of three user-chosen profiles: `Aggressive` (full sledgehammer), `Balanced` (no service/process touching), `Pinning only`. The user-visible Game Mode definitions remain fully aggressive — the question is *whether they're armed*, not *how strong they are*.

**Files.** `crates/sys/src/inner/apply.rs:522-533` (silent-clear fix), `crates/core/src/policy.rs:343-470` (defaults — confirm only the consent-gating change; aggression preserved), small first-run flag in `tray/main.rs` (the actual consent UI lands in item 4.1).
**Risk.** Existing policy.json files are NOT touched (user's choices win). Only fresh installs see the consent flow.
**Verification.** Synthetic non-X3D topology: launch Valorant simulation, confirm CPU sets are now `TopRanked(8)` not empty. Existing X3D paths unchanged (cache CCD resolves non-empty → no change in behavior).
**Effort.** S.
**Closes.** H-09 (silent clear), H-18 (consent gate prevents surprise sledgehammer at first launch). Feeds H-24 (first-run onboarding) and the new manual-global toggle (item 2.11).

### 1.8 — IPC line-length cap + DoS resistance
**Scope.** Replace `BufReader::lines` with `AsyncBufReadExt::take(MAX_LINE)` (suggest 1 MB cap). Cap status-pipe Subscribe count at 16 per caller-PID via `GetNamedPipeClientProcessId`. Both server-side.
**Files.** `crates/service/src/runtime.rs:302` (lines), `crates/service/src/runtime.rs:582-589` (subscribe).
**Risk.** 1 MB cap is generous; legitimate `SetPolicy` payloads are <100 KB.
**Verification.** Synthetic test: connect to admin pipe, send 10 MB single line → expect connection close + log. Spawn 32 status-pipe `Subscribe` requests from a single PID → expect first 16 succeed, rest fail with clear error.
**Effort.** S.
**Closes.** H-15, H-16.

### 1.9 — AC detection + AC-aware Safe Mode infrastructure (NEW — promoted from pre-ship research)
**Scope.** Anti-cheat-aware behavior is a safety landing alongside the safe-list enforcement of 1.1 — it gets the same Group-1 priority. Full spec at `audit/research/ANTI-CHEAT-MATRIX.md`. Four parts:

  1. **AC detection probe** — new `crates/sys/src/inner/ac_detect.rs`. Enumerates loaded kernel drivers (via `EnumDeviceDrivers`) + running services (`EnumServicesStatusExW`) + running processes (`iter_pids`+exe lookup) for known AC signatures: `vgk.sys`/`vgc.exe` (Vanguard), `EasyAntiCheat.sys`/`EasyAntiCheat.exe`/`EasyAntiCheat_EOS.exe` (EAC), `BEDaisy.sys`/`BEService.exe` (BattlEye), `FACEIT_AC.sys`/`FACEIT_AC.exe`/`FACEITService.exe` (FACEIT), `ESEAClient.exe`/`eseaclient_x64.exe` (ESEA), EA Javelin's driver chain (BF6). Probes at startup + on policy reload + on every background scan (cheap; piggybacks on existing enumeration). Result: `AntiCheatPresence { vanguard, eac, battleye, faceit, esea, javelin }`.

  2. **`AntiCheatProfile` enum** on `Profile` struct: `Aggressive` (full sledgehammer, no AC concern) / `Hybrid` (environment yes, game-process no — for BF6+Javelin) / `SafeMode` (environment yes, game-process never, launcher-inheritance for affinity — for Vanguard + FACEIT) / `Disabled` (engine standby; for ESEA). `serde(default = "AntiCheatProfile::Aggressive")` so existing policies migrate.

  3. **Engine respects the profile at apply time.** When applying a rule whose `ac_safe_mode_target` is `SafeMode` or `Hybrid`, skip game-process modifications (priority/affinity/CPU sets/IO prio/power throttling) and either route via launcher PID (Bitsum-documented pattern) or no-op-with-log. When `Disabled` AND the corresponding AC is detected running, the engine enters STANDBY for that session — no rule writes, no scans, no actions until the AC binary exits. This is the **ESEA auto-pause** decision: when `ESEAClient.exe` is running, FrameSage goes dark for ESEA users, sidestepping Error #107 entirely.

  4. **Architectural invariants encoded as compile-time/runtime checks** (10 invariants from the AC matrix):
     - Every `OpenProcess` against a protected game requests at most `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SET_INFORMATION | PROCESS_SET_LIMITED_INFORMATION | SYNCHRONIZE`. CI grep + unit test enforces.
     - AC-binary hard deny-list (const array): `vgc.exe`, `vgk.exe`, `vgtray.exe`, `BEService*`, `BEDaisy*`, `BEClient*`, `EasyAntiCheat*`, `start_protected_game.exe`, `RiotClientServices.exe`, `FACEIT_AC*`, `FACEITService*`, `ESEAClient*`, `*_BE.exe` glob. Every IPC mutator + apply path rejects these with rationale.
     - Launcher demotion ban: never lower priority of `steam.exe`, `epicgameslauncher.exe`, `RiotClientServices.exe`, `EALaunchHelper.exe`, `EpicWebHelper.exe`. The Fletcher Dunn / CS2 priority-inversion lesson.
     - Open-apply-close pattern for game handles; never cache.
     - 5-second init delay on new game PIDs before first rule write (mirrors PL v17.0.2.18). Requires Group 3 item 3.1's `Clock` trait, but the deny-list checks + handle-rights checks can land in Group 1 against a real clock.
     - No retry on ACCESS_DENIED — log once per (PID, action), never retry for that PID's lifetime.
     - No kernel driver. Ever. CI: `cargo tree` greps for WDK crates.
     - No DLL injection / overlay. Codebase grep: no `CreateRemoteThread` / `SetWindowsHookEx` / etc.
     - Defense-in-depth `NtSuspendProcess` gating: if target's image path contains `\Riot\` / `\Vanguard\` / `\BattlEye\` / `\FACEIT\` / `\ESEA\`, refuse with rationale (second layer beyond the name deny-list).
     - WU pause/stop gate: if FACEIT detected, refuse to pause WU or stop `wuauserv` / `UsoSvc` / `WaaSMedicSvc` (FACEIT refuses to launch with broken WU).

**Files.** New `crates/sys/src/inner/ac_detect.rs` + stub in `crates/sys/src/stub.rs`. New `crates/core/src/policy.rs` enum + field. `crates/engine/src/lib.rs` — apply path respects AC profile, standby mode for `Disabled`, deny-list integration for the additional AC binaries. `crates/ipc/src/lib.rs` — new `AntiCheatPresence` in `StatusSnapshot`. Tests across `engine/src/lib.rs` for each of the 10 invariants.
**Risk.** AC detection adds enumeration cost at startup + per-tick check. Mitigated by caching presence with a 10s revalidation interval. ESEA standby is the most behaviorally-novel piece — if it's buggy, FrameSage doesn't apply rules but also doesn't break anything (the user just sees the tray banner "Standby for ESEA"). Vanguard's detection-via-driver-enumeration requires `PROCESS_QUERY_LIMITED_INFORMATION` on the device-driver list — read-only, no concerns.
**Verification.** Each of the 10 invariants gets a dedicated unit test. AC detection test: synthetic mock of `EnumDeviceDrivers` returns `vgk.sys`; presence struct reports Vanguard. ESEA standby test: mock returns `ESEAClient.exe` in PID list; engine.tick() short-circuits with no apply calls. Vanguard-safe-mode test: rule matching `VALORANT-Win64-Shipping.exe` with `ac_safe_mode_target: SafeMode` produces zero game-process syscalls but full environment actions.
**Effort.** L. This is the second-largest Group-1 item after 1.1 itself.
**Closes.** Pre-ship AC matrix items 1–10. Blocks Group 4 ship until merged.

**Group 1 stop-for-review point.** After 1.1–1.9 all merged + verified, present:
- Build/clippy/fmt/test green.
- New tests inventory (which dangerous APIs now have guard tests).
- Self-footprint measurement at idle (200 PIDs): wakeups/sec, CPU%, private bytes, handle count. Numbers recorded as the Group-1 baseline so later groups can demonstrate improvement.

---

## Group 2 — High-leverage (largest user-visible impact per LoC)

Once safety is locked in, these are the wins users will feel: app footprint drop, observability appears, no more silent hangs on service restart, first-run lands somewhere sensible.

### 2.1 — Single-syscall process snapshot via NtQuerySystemInformation
**Scope.** Add `crates/sys/src/inner/sys_proc_info.rs` wrapping `NtQuerySystemInformation(SystemProcessInformation)` → one call returns per-PID CPU times, image name, priority class, thread count, working set, handle count in a single allocation. Engine's `list_process_snapshots` + ProBalance sample switch to this path. Falls back to current per-PID OpenProcess loop only for fields NTQSI doesn't carry (affinity mask, full image path — those stay on PID-handle-cached lookups, gated by 8-budget for cold lookups).
**Files.** New `sys_proc_info.rs`, `crates/engine/src/lib.rs:567-820` (list_process_snapshots), `crates/engine/src/lib.rs:1162-1248` (probalance sample).
**Risk.** NTQSI is undocumented-but-stable. Buffer-resize loop is standard (start at 1 MB, double on STATUS_INFO_LENGTH_MISMATCH). NULL handles in returned structs need care.
**Verification.** Compare snapshot output to current path on a busy box; assert identical CPU% / memory / thread count for 100+ PIDs. **Measure**: syscalls/sec at idle before/after (target: drop from ~750–1250 to ~10).
**Effort.** M.
**Closes.** H-01, H-02 (the largest single footprint contributors).

### 2.2 — Foreground reporter: WinEvent hook + dedupe + visibility gate
**Scope.** Replace the 250 ms poll with `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)`. On callback: capture pid/exe/title, dedupe against last-sent, send via IPC. Add a window-visibility gate so the loop sleeps longer when tray window is hidden (currently it ignores visibility). Keep the polling path as a fallback for environments where WinEvent doesn't fire (rare; UAC dialogs).
**Files.** `crates/tray/src/main.rs:6043-6079`, new `crates/tray/src/win_event.rs`.
**Risk.** WinEvent hooks run on the calling thread's message loop — needs a dedicated message-pump thread. Hook installation can fail under restricted tokens (mitigated by polling fallback).
**Verification.** Idle measurement: admin-pipe accepts/sec before/after (target: <0.1/sec at idle). Alt-tab between 5 apps in 1s, confirm 5 events delivered with correct sequence.
**Effort.** M.
**Closes.** H-03, L-02.

### 2.3 — `Arc<Topology>` + `Arc<Policy>` + cached safe-list sets
**Scope.** Wrap `EngineState.topology` in `Arc<CpuTopology>` (topology is immutable after startup). `status()` returns `Arc<Policy>` instead of cloning. Build `safe_list_exes` HashSet once at startup, invalidate only on `set_policy`. Same for `user_ignore_exes`.
**Files.** `crates/engine/src/lib.rs:67-68, 442, 548, 1261-1270, 1404, 1462, 1556, 1767`, `crates/ipc/src/lib.rs` (Response::Status structure).
**Risk.** API change to `status()` callers (tray reads `Policy` from `StatusSnapshot`). Migration is type-driven; compiler will guide.
**Verification.** Allocator-pressure benchmark: 10-minute idle run, measure private-bytes growth before/after (target: <1 MB/hour).
**Effort.** S.
**Closes.** H-04, H-06.

### 2.4 — Sleep/resume + WTS session-change awareness
**Scope.** Two hooks:
  1. Service registers for `WTSRegisterSessionNotification` + `SERVICE_ACCEPT_SESSIONCHANGE`; on session-change, log + arbitrate which session's foreground reporter wins (default: active console session).
  2. Service registers for `RegisterPowerSettingNotification` + handles `WM_POWERBROADCAST`/`PBT_APMSUSPEND`+`PBT_APMRESUMESUSPEND`. On suspend: snapshot state; on resume: re-query power plan + verify journal-recorded state still matches reality (taskbar visible? services running?) before assuming our applied state is intact. Stale assumptions → log + emit `ActionFailed` event (see 2.7).
**Files.** `crates/service/src/main.rs:73-74` (SCM control mask), new `crates/service/src/power.rs` and `crates/service/src/wts.rs`, engine wiring.
**Risk.** SCM control handler runs on a separate thread; needs careful sync with engine state. Multi-session arbitration is a design call — propose "active console session wins" as default.
**Verification.** Manual: suspend laptop with Game Mode active, resume, confirm engine reconciles. FUS test: log in as user B while user A's session has Game Mode active.
**Effort.** M.
**Closes.** H-10, H-11.

### 2.5 — Tray IPC timeout + reconnect with backoff
**Scope.** Wrap `OpenOptions::open(PIPE_NAME_ADMIN)` in a timeout (suggest 2s) via `WaitNamedPipeW` first, then open with timeout. On failure: exponential backoff (250ms → 500ms → 1s → 2s → cap at 5s). Both reporter loop AND any send from UI thread get the same treatment so UI never hangs.
**Files.** `crates/tray/src/main.rs:6087-6104` (send_request_blocking), `crates/tray/src/main.rs:6043-6079` (reporter loop).
**Risk.** A timeout that's too short fights legitimate slow connects under load. 2s is a comfortable floor.
**Verification.** Restart service while tray is open; confirm UI stays responsive throughout; reporter reconnects within 5s of service-back-up; no log spam during outage (one warn at start, one info at recovery).
**Effort.** S.
**Closes.** H-14, L-02, L-09.

### 2.6 — Stale-reporter detection + fallback
**Scope.** Track `last_report_at: Option<Instant>` alongside `reported_foreground`. In `tick()`, if `reported_foreground.is_some()` AND `last_report_at` is older than 10s, treat as no-report and fall back to session-local polling. Re-evaluate on next report received.
**Files.** `crates/engine/src/lib.rs:1093-1104, 943-952`.
**Risk.** False-positive fallback on a temporarily-unresponsive tray. 10s window is generous (tray reports every 250ms; even 1s drift is anomalous).
**Verification.** Kill tray with `taskkill /F /IM framesage-tray.exe` while Game Mode is active; confirm engine reverts state within ~10s (no longer permanently stuck).
**Effort.** S.
**Closes.** H-13.

### 2.7 — File-sink logging + "Open log folder"
**Scope.** Replace `tracing_subscriber::fmt` with `tracing_appender::rolling::daily` writing to `%ProgramData%\framesage\logs\framesage-svc.log.YYYY-MM-DD` (rotate daily, keep 7). Same for tray → `%LOCALAPPDATA%\FrameSage\logs\framesage-tray.log...`. Add "Open log folder" item to tray File menu.
**Files.** `crates/service/src/main.rs:97-102`, `crates/tray/src/main.rs` tracing init (currently absent), tray File menu.
**Risk.** Log volume — daily rotation + 7-day retention bounds it. Disk-write contention is negligible at info level.
**Verification.** 1-hour test: verify log file exists, rotates correctly on day change (simulate via system clock), Open Log Folder opens Explorer to the right path.
**Effort.** S.
**Closes.** H-29.

### 2.8 — Expanded IPC Event enum
**Scope.** Add Event variants:
  - `GameModeEntered { profile_id, summary: AppliedActionsSummary }`
  - `GameModeExited { profile_id, summary: AppliedActionsSummary, duration_secs }`
  - `ProfileApplied { pid, exe_name, profile_id }`
  - `ProfileReverted { pid, exe_name, profile_id }`
  - `AffinityRuleFired { pid, exe_name, rule_exe_name }`
  - `ActionFailed { what: String, error: String, pid: Option<u32> }`
  Wire emission at every existing engine site that mutates kernel state or fails to.
**Files.** `crates/ipc/src/lib.rs:338-364`, `crates/engine/src/lib.rs` (every `apply_profile` / `revert_record` / `apply_affinity_rule` / Game Mode site).
**Risk.** Activity tab volume goes up. Mitigated by filter chips (already present).
**Verification.** Run a full game-x3d session; verify exactly one GameModeEntered + one GameModeExited + N ProfileApplied/Reverted + summary event counts match journal.
**Effort.** S.
**Closes.** H-28, H-30 (combined with 2.7 file-sink), M-22 (rule index attribution).

### 2.9 — Activity log persistence to activity.jsonl
**Scope.** Tray appends every `RecentEvent` to `%LOCALAPPDATA%\FrameSage\activity.jsonl` (rotate at 10 MB → `.1`). On tray start, load tail of last 1000 entries into the ring buffer. Adds backfill across restarts.
**Files.** `crates/tray/src/main.rs:5842-5896` (event ingestion), new `tray/src/activity_log.rs`.
**Risk.** Disk-write per event — buffer to BufWriter, flush every 5s.
**Verification.** Restart tray after 50 events; confirm Activity tab pre-populates from disk.
**Effort.** S.
**Closes.** H-31, H-28 (partial — history survives restart).

### 2.10 — Default landing tab = Status
**Scope.** Change `Tab::default()` from `Tab::Processes` to `Tab::Status`.
**Files.** `crates/tray/src/main.rs:158-165`.
**Risk.** None.
**Verification.** Fresh install + first launch lands on Status with hero / profile summary / quick actions visible.
**Effort.** Trivial (5 minutes).
**Closes.** H-24 (partial — first-run onboarding lands in Group 4).

### 2.11 — Manual Global Game Mode (NEW — user-supplied requirement)
**Scope.** A global "give me max perf for the next N minutes regardless of focus" switch. Useful for video editing, benchmarking, livestreaming, render farms, anything where the user wants the aggressive profile active without it being focus-gated. Five surfaces, one underlying mechanism:

  1. **Per-profile flag** `manual_global_eligible: bool` on `Profile` struct. Defaults `true` for profiles that touch system-wide state (game-x3d / any profile with non-None `game_mode`), `false` for narrow per-app tunings (eco, perf). User can override per profile.

  2. **Tray menu toggle** under the existing right-click menu: "Activate Game Mode ▸" → submenu listing `manual_global_eligible` profiles → click activates. While active: "Deactivate Game Mode" replaces the submenu. Status hero shows a bold banner "Manual Game Mode active: <profile_id>" with a Deactivate button.

  3. **CLI verbs** `framesage game-mode start [--profile <id>]` (defaults to first `manual_global_eligible` profile; errors if none) and `framesage game-mode stop`. Both round-trip via the admin pipe. Idempotent. Enables OBS scene scripts: scene "Gaming" → `game-mode start --profile game-x3d` on enter, `game-mode stop` on leave.

  4. **Global hotkey** registered from the tray process via `RegisterHotKey` — default `Ctrl+Alt+G`. Configurable in Settings (item 4.3). On bind: detect conflict via `RegisterHotKey` return; if denied, prompt user to pick another. The hotkey toggles the same activate/deactivate as the menu item.

  5. **Same journal + revert path** as foreground-triggered activation. The engine's `reconcile_system_mode_locked` learns a new `ManualOverride` source alongside the existing foreground-driven path; the journal entry tags `trigger: "manual_global"` so the session-history view can distinguish. **No new dangerous code path** — the same `plan_game_mode` → `apply_plan` → journal-write machinery runs.

**Files.** `crates/core/src/profile.rs` (new `manual_global_eligible` field, `#[serde(default = "default_true_for_aggressive_profiles")]`). `crates/ipc/src/lib.rs` (new `Request::StartManualGameMode { profile_id }` / `StopManualGameMode`, new `Event::ManualGameModeStarted/Stopped`). `crates/service/src/runtime.rs` (route the new IPC variants). `crates/engine/src/lib.rs` (new `manual_game_mode_active: Option<ProfileId>` state, engine reconcile loop checks this *first*, falls through to foreground-driven if `None`). `crates/cli/src/main.rs` (new `game-mode start|stop` verbs). `crates/tray/src/main.rs` (menu + status banner + hotkey wiring; new `tray/src/hotkey.rs` for RegisterHotKey).
**Risk.** Interaction with focus-driven Game Mode: what if the user has manual active AND launches a game with its own game-x3d rule? Decision: manual wins, focus-driven is suppressed while manual is active. Foreground-changed events still drive per-PID profile (pin/priority) but the system-wide Game Mode is locked to the manual choice. Hotkey conflict detection is hard-mode — fallback "pick another" prompt is essential.
**Verification.** Activate from tray menu, alt-tab around, confirm taskbar stays hidden + services stay stopped. Deactivate, confirm restore. CLI: `start --profile game-x3d`, sleep 5s, `stop` — journal shows correct trigger tag. Hotkey: bind, switch focus to Notepad, hit Ctrl+Alt+G, confirm activation; hit again, confirm deactivation. Conflict: temporarily bind same hotkey in another app, restart tray, expect bind-fail + user prompt.
**Effort.** M.
**Closes.** New requirement. No existing finding IDs.

**Group 2 sub-stops.** Two natural break points inside this group, because the items below are independent and you'll want to see footprint numbers before approving Structural work:
  - Sub-stop 2a: after 2.1 + 2.3 (the footprint wins). Show wakeups/sec + private bytes + handles BEFORE / AFTER. Aim: 10× reduction in steady-state syscalls.
  - Sub-stop 2b: after 2.7 + 2.8 + 2.9 (observability). Demo Activity tab persistence + Open Log Folder + Game Mode session events.

**Group 2 stop-for-review point.** After all of Group 2: presentable demo of the app running for 10 minutes at idle with measured footprint, plus a full Game Mode session with the new audit trail visible end-to-end.

---

## Group 3 — Structural (enables future work)

These don't ship user-visible features, but every subsequent change becomes faster and safer. Sequence carefully — testability change unblocks the rest.

### 3.1 — Trait abstraction for SysApi + injected Clock → engine becomes unit-testable
**Scope.** Define `trait SysApi` in `crates/engine/src/sys_api.rs` covering every `framesage_sys::*` free function the engine calls. Production impl wraps the real calls. Test impl is a mock with scriptable returns. Add `trait Clock { fn now(&self) -> Instant; }` and inject via `EngineDeps`. Refactor every `Instant::now()` in engine to use the injected clock. Mirror the existing `SystemStateQuery` pattern in `gamemode/planner.rs`.
**Files.** New `crates/engine/src/sys_api.rs` + `crates/engine/src/clock.rs`, refactor `crates/engine/src/lib.rs` (every direct `framesage_sys::*` call and every `Instant::now()` call). Engine becomes generic over `<S: SysApi, C: Clock>` or boxed-dyn.
**Risk.** Touches ~50 sites across `lib.rs`. Big diff; mechanical. **Tests are the verification.** Once trait is in place, add integration tests for `tick` / `reconcile` / `maybe_run_probalance_locked` / `maybe_scan_background_locked` / `maybe_reassert_persistent_locked` covering: foreground change, ProBalance restrain+restore, persistent re-assert on PID reuse, background scan picks up new PID, etc.
**Verification.** Engine `lib.rs` ends with **≥15 unit tests** covering the previously-untested paths. cargo-test green; clippy clean.
**Effort.** L. This is the single biggest debt-reduction in the codebase.
**Closes.** H-20.

### 3.2 — `parking_lot::Mutex` in tray
**Scope.** Switch `crates/tray/src/main.rs` from `std::sync::Mutex` to `parking_lot::Mutex` (already a workspace dep). `parking_lot::Mutex::lock` doesn't return `LockResult`, so 30 `.unwrap()` calls disappear by typing change.
**Files.** `crates/tray/src/main.rs` (30 sites, plus the type declarations).
**Risk.** None — `parking_lot::Mutex` is a drop-in. Possible subtle behavior change: no poisoning on panic. That's the *point*.
**Verification.** Tests + build green. Manually inject a panic in a background thread holding the lock; confirm UI keeps working.
**Effort.** S.
**Closes.** H-21.

### 3.3 — `OwnedHandle` newtype in sys/inner
**Scope.** Define `crates/sys/src/inner/handle.rs` with `pub struct OwnedHandle(HANDLE);` + `Drop`+`CloseHandle`. Migrate every `OpenProcess`/`CreateToolhelp32Snapshot`/`OpenThread`/`OpenProcessToken` site to return `OwnedHandle`. Manual `close_handle` calls disappear. RAII discipline that already exists in `tray/win32.rs:SingletonGuard` propagates to sys.
**Files.** New `crates/sys/src/inner/handle.rs`, refactor `crates/sys/src/inner/process.rs`, `apply.rs`, `process_actions.rs`, `topology.rs`, `cppc.rs`, `foreground.rs`, `version_info.rs`, all `game_mode/*.rs`.
**Risk.** Diff is mechanical but wide. Misuse class (double-close, leaked-on-error) becomes a compile error. **Net win.**
**Verification.** Grep confirms zero manual `CloseHandle` calls remain. Tests + build green.
**Effort.** M.
**Closes.** L-31, L-32. Hardens against future regressions.

### 3.4 — Per-process CPU history ring + sparkline in detail panel
**Scope.** Add `HashMap<u32, VecDeque<(Instant, u16)>>` on AppState for per-selected-PID CPU% history (bounded at 60 samples = 1 minute @ 1Hz). Push current value on every poll. Render sparkline in detail panel using egui's `Plot` or a hand-drawn line. Only populate for currently-selected PID (don't carry history for 600 PIDs).
**Files.** `crates/tray/src/main.rs:4143-4341` (detail panel render), AppState additions.
**Risk.** Marginal memory (60 samples × selected PIDs). Selection change clears prior PID's buffer.
**Verification.** Select a process, watch sparkline populate over 60s; switch selection, confirm new history starts from zero.
**Effort.** S.
**Closes.** H-22.

### 3.5 — Undo log + "what Game Mode is currently holding" view + per-action undo
**Scope.** Two surfaces:
  1. Status tab card: "Game Mode is currently holding: 30 services / 24 processes" with expand → list with per-row Resume / Restart button.
  2. Per-process priority change records prior class in `AppState.undo_stack: HashMap<u32, PriorClass>`. Right-click → "Undo priority change" appears when an entry exists.
**Files.** `crates/tray/src/main.rs` Status tab rendering, new `tray/src/undo.rs`. Reads from engine's `system_mode.applied` via IPC (need a new read-only IPC `GetSystemModeState` or extend StatusSnapshot).
**Risk.** Per-action undo for service-restore conflicts with auto-restore on Game Mode exit. Resolution: if Game Mode is active, "Undo this service stop" is grayed with explanation "exits with Game Mode."
**Verification.** Enter Game Mode → confirm card shows the right counts → expand → confirm action lists match journal → click one Resume → confirm service is restarted but Game Mode remains active for the rest.
**Effort.** M.
**Closes.** H-23.

### 3.6 — Extract `tray::ipc` + `tray::menu` + `tray::tabs::processes`
**Scope.** Three module extractions from the 6,241-line `tray/main.rs`:
  - `tray::ipc` (~325 LOC): `background_loop`, `try_connect_and_serve`, `processes_poll_loop`, `send_processes_and_status_blocking`, `foreground_reporter_loop`, `send_request_blocking`.
  - `tray::menu` (~150 LOC): `TrayMenuIds`, `build_tray`, `build_icon`.
  - `tray::tabs::processes` (~1,200 LOC): `render_process_detail`, `ProcessAction`, `detail_kv`, `TerminateConfirm`, `AffinityPicker`, the giant context-menu builder.
After: `main.rs` shrinks to ~2,500 LOC (state + `impl App` + thin shell).
**Files.** New `crates/tray/src/ipc.rs`, `crates/tray/src/menu.rs`, `crates/tray/src/tabs/processes.rs`, refactor `tray/src/main.rs`.
**Risk.** Visibility changes (pub(crate) instead of inline). Each module gets its own tests.
**Verification.** Build + clippy + fmt + test all green. Line-count check: `wc -l tray/src/main.rs` ≤ 3,000.
**Effort.** M.
**Closes.** M-29.

### 3.7 — Topology hot-plug refresh + multi-group (GROUP_AFFINITY) + Intel hybrid
**Scope.** Three related changes:
  1. Topology refresh on `WM_DEVICECHANGE`/`DBT_DEVICEARRIVAL` for CPU enable/disable. Subscribe via the SCM control handler (Service Notify pattern).
  2. Add `GROUP_AFFINITY` path in `apply.rs` and `topology.rs` for >64-CPU systems. Falls back to current u64 path on single-group machines.
  3. Read `PROCESSOR_RELATIONSHIP::EfficiencyClass` and stamp `CoreKind::Efficiency` accordingly. Update `retag_ccds_from_signals` to handle Intel hybrid topology.
**Files.** `crates/sys/src/inner/topology.rs:51-235`, `crates/sys/src/inner/apply.rs:419-442` (mask_from_indices), `crates/service/src/main.rs` (device-notify wiring).
**Risk.** GROUP_AFFINITY tested only on single-group hardware in dev; requires CI test using `framesage-sim` synthetic topology with >64 logical CPUs.
**Verification.** Synthetic topology test for 96-core single-group (Threadripper PRO config), 128-thread dual-group (EPYC), and 16-thread P+E hybrid (Alder Lake config). Verify `CpuSelector::Kind(Cache)` resolves correctly on each.
**Effort.** L.
**Closes.** H-07, H-08, H-12.

### 3.8 — Layering: move `tracing` out of core, `framesage-gamemode` out of sys deps
**Scope.** Remove `tracing` from `crates/core/Cargo.toml` — replace `warn!` lines in `policy.rs`/`topology.rs` with returned `Result<_, Warning>` or `tracing` re-exported through an upper layer. Move `SystemStateQuery` trait + `PlannedAction` enum definitions from `gamemode` to `core` (pure data + trait), implement them in `sys` — flips the arrow so `sys` depends only on `core`.
**Files.** `crates/core/Cargo.toml`, `crates/core/src/policy.rs`, `crates/core/src/topology.rs`, `crates/sys/Cargo.toml:30`, `crates/gamemode/src/planner.rs` (trait moves), `crates/core/src/game_mode.rs` (new home for `PlannedAction`).
**Risk.** Wide diff, mostly mechanical. Workspace builds verifies.
**Verification.** Build + clippy clean. `cargo tree -p framesage-core` shows no tracing. `cargo tree -p framesage-sys` shows no gamemode.
**Effort.** M.
**Closes.** M-28.

**Group 3 stop-for-review point.** After all of Group 3: tray monolith is broken up, engine has 15+ new tests, RAII handle discipline is enforced by the type system, layering arrows are clean. No user-visible changes; pure code-health gain. Time to verify build/clippy/fmt/test still green and that the new tests catch the bug class they're meant to.

---

## Group 4 — Polish (QoL, correctness corners, configurability)

These individually are small but collectively close the gap with Process Lasso on the "feels finished" axis.

### 4.1 — First-run onboarding (3-way informed consent — REVISED per user)
**Scope.** First-launch detection (`%LOCALAPPDATA%\FrameSage\first-run-complete` marker file). Modal walk-through that gates the seeded BF6/Valorant/Fortnite rules behind an explicit user choice. No defaults are pre-selected.

**Page 1 — What FrameSage is.** Verbatim positioning statement from item 4.14: *"FrameSage is for users who want maximum performance during games or focused work sessions. It will stop background services and suspend non-essential processes during a session. Everything is reversed when the session ends. If you'd rather a gentle optimizer, this isn't the right tool."* Continue button.

**Page 2 — Choose your level (no default pre-selected).** Three radio options, each with full disclosure of what it does.

*Note (added per AC matrix):* The three choices apply to BF6, Fortnite, and any future user-added games. **Valorant is special** — Vanguard's track record requires AC-Safe Mode regardless of which of the three you pick. We honor your aggression preference for the *environment* (services, processes, power, taskbar) but never touch the Valorant process itself. This is the same approach Hone (1M+ Valorant users, zero AC issues) takes, and the AC matrix research justified it. If you want to override for Valorant specifically, you can do so per-rule in the profile editor with a separate eyes-open warning naming the VAN: Competitive Restriction risk.

  - **Aggressive** *(recommended for dedicated gaming PCs)* — when you launch one of the recognized games, FrameSage will:
    - **Stop these services** (expandable: live list pulled from the seeded `game-x3d.stop_services`, with display names + rationale from safe-list JSON): SysMain, WSearch, DiagTrack, BITS, DoSvc, WaaSMedicSvc, UsoSvc, WpnService, CDPSvc, DPS, WdiServiceHost, WdiSystemHost, WerSvc, PcaSvc, dmwappushservice, ClickToRunSvc, SDRSVC, defragsvc, MapsBroker, AJRouter, WMPNetworkSvc, Fax, RetailDemo, PhoneSvc, RemoteRegistry, icssvc, TrkWks, stisvc.
    - **Suspend these processes** (expandable: live list from seeded `suspend_processes`): OneDrive.exe, FileCoAuth.exe, Dropbox.exe, googledrivesync.exe, GoogleDriveFS.exe, pCloud.exe, MEGAsync.exe, OneDriveStandaloneUpdater.exe, GoogleUpdate.exe, MicrosoftEdgeUpdate.exe, lghub_updater.exe, AdobeARM.exe, GameBar.exe, GameBarFTServer.exe, GameBarPresenceWriter.exe, WidgetService.exe, Widgets.exe, YourPhone.exe, PhoneExperienceHost.exe, NVIDIA Web Helper.exe, DellSupportAssistRemedyService.exe, HPSupportSolutionsFrameworkService.exe, HpToastSourceApp.exe, LenovoVantageService.exe.
    - Switch to **High Performance / Ultimate Performance** power plan.
    - **Hide the taskbar.**
    - **Pause Windows Update** for the session.
    - **Pin the game to the X3D / cache CCD** (or top-ranked cores on non-X3D).
    - Bump priority + I/O priority.
    - *Everything above is reversed automatically when you exit the game.* The journal records each action with timestamps; review under Status → Recent Sessions.

  - **Balanced** — CPU pinning + priority bumps + power plan switch only. No services stopped. No processes suspended. Cloud sync stays running. OK for shared / work-also-laptop machines.

  - **Pinning only** — affinity to X3D CCD + nothing else. Conservative; the safest of the three.

  Continue button disabled until a choice is selected.

**Page 3 — Manual Game Mode hotkey.** Brief intro to the manual global toggle (item 2.11). Offers to bind `Ctrl+Alt+G` (default) or pick a custom binding. "Skip" option for users who only want focus-driven activation.

**Page 4 — Done.** Tray opens on Status tab. Marker file written.

The chosen profile from Page 2 is what populates `game_mode` on the seeded BF6/Valorant/Fortnite rules. Choose `Balanced` → those rules get a Balanced `GameModeActions` (`hide_taskbar: false`, empty `stop_services`/`suspend_processes`, power plan still flipped). Choose `Pinning only` → those rules get `game_mode: None`. Choose `Aggressive` → the full sledgehammer as documented.

**Files.** New `crates/tray/src/onboarding.rs`, wired into `FramesageApp::new`. The chosen profile is written by IPC `SetPolicy` so the policy.json reflects the user's consent.
**Risk.** None — pure UI work. Skip path: if user closes the window without choosing, marker is NOT written and onboarding re-fires on next launch. Default `Pinning only` only applies if the user explicitly picks Skip-And-Use-Safest after a "are you sure" prompt.
**Verification.** Delete marker, relaunch, walk through each path; verify policy.json reflects the chosen Game Mode aggression level. Verify the expandable lists actually pull from the safe-list JSON rationale strings.
**Effort.** M (was S — larger because of the disclosure UI).
**Closes.** H-24 (full coverage). Replaces the original D-5 mechanism with the user's stronger consent model.

### 4.2 — UX gaps: filter searches all columns, Ctrl-F, drop content-width cap, font option
**Scope.** Four small changes:
  - Filter searches `exe_name` + Description + Company + User (case-insensitive substring across all four).
  - Ctrl-F focuses the filter textbox. Esc clears it.
  - Drop `MAX_CONTENT_WIDTH = 980.0` clamp on the Processes tab specifically (other tabs keep it).
  - Settings → Compact mode toggle: 11.5pt body + 16px rows on the table only (gets ~25% more rows).
**Files.** `crates/tray/src/main.rs:2769, 878, 2966-3011`, `theme.rs:104-118`.
**Risk.** Filter searching more columns may surprise users expecting exe-only. Mitigate with placeholder text "Filter by name, description, company, user".
**Verification.** Type "microsoft" with the new filter; expect every Microsoft-published process to match by Company.
**Effort.** S.
**Closes.** H-33, H-34, L-15, L-19.

### 4.3 — Configurability gaps
**Scope.** Five surface additions:
  - "Reset to defaults" button in Settings (with confirmation).
  - "Export policy" / "Import policy" buttons (file dialogs).
  - CLI verbs: `policy export <path>`, `policy import <path>`, `policy add-rule <exe> <profile>`.
  - `persistent` field exposed in profile editor.
  - ProBalance thresholds + tick_ms + background_profile + ignore_processes editable in Settings.
**Files.** `crates/tray/src/main.rs` (Settings tab — may need to add one), `crates/cli/src/main.rs` (new verbs).
**Risk.** Wide UI surface; design needs to stay simple. Settings can be a new tab or expand the Status tab's Settings card.
**Verification.** Manual walk-through.
**Effort.** M.
**Closes.** M-23, M-24, M-25.

### 4.4 — Mask sanitization + apply path guards
**Scope.** Intersect computed affinity mask with `system_mask` from `GetProcessAffinityMask` before calling `SetProcessAffinityMask`. Add `mask != 0` guard at `apply.rs:142`. Reject hand-edited profile masks that are zero or have no intersection with system_mask at SetPolicy time (return Response::Error with explanation).
**Files.** `crates/sys/src/inner/apply.rs:139-144, 419-442`, `crates/service/src/runtime.rs:488-523` (SetPolicy validation).
**Risk.** None.
**Verification.** Test: construct a profile with `Mask(0)`, send via SetPolicy, expect Error. Construct `Mask(0xFFFFFFFFFFFFFFFF)` on a 16-thread box, confirm engine intersects to `0xFFFF`.
**Effort.** S.
**Closes.** M-01, M-02, M-27.

### 4.5 — Defender trim protection [reframed: M-20 BITS check REJECTED]
**Scope.** Single change retained: `MsMpEng.exe` added to the trim-working-set protection alongside existing suspend protection (already covered via item 1.1's denylist enforcement — this is a verification step, not new code).

**Reframed-as-feature.** The original audit proposed an in-flight-transfer check before stopping BITS. Per the product-positioning correction: **rejected**. Aggressive Game Mode stops BITS unconditionally. The journal records when it happened; revert restarts it; transfers resume. That's the deal the user signed up for. A 50 GB download paused for a 2-hour game is paused; it's not lost. Adding a quiet "actually no, let's not" check would betray the consent the user gave.

The journal entry + the post-session sessions.jsonl line + the GameModeExited event (items 1.4 + 2.8) give the user complete visibility into what happened. That's the contract: full aggression + full audit trail + full reversibility, not "aggressive sometimes."

**Files.** `crates/sys/src/inner/apply.rs:342-356` (trim path) — denylist consultation already in 1.1. No new BITS-specific logic.
**Risk.** None.
**Verification.** Trigger trim on MsMpEng PID, expect refusal with denylist rationale surfaced. Trigger Game Mode with active BITS job: BITS stops, journal records, session ends, BITS restarts, job resumes (BITS is designed for this).
**Effort.** Trivial (verification only).
**Closes.** M-04. M-20 is **reframed as expected behavior** — explicitly documented in README (item 4.14) and surfaced in the first-run onboarding (item 4.1) so the user knows.

### 4.6 — ProBalance restrain-side hysteresis
**Scope.** Require N consecutive samples (default 2) over `hog_cpu_threshold_percent` before restraining. Configurable via `ProBalanceConfig.min_restrain_samples`.
**Files.** `crates/engine/src/probalance.rs:191-202`, `crates/core/src/policy.rs:142-160`.
**Risk.** Slight delay (1 extra sample = +1 sec) before restraint. Test ensures the existing dwell-on-restore is preserved.
**Verification.** Unit test that adds a synthetic 1-sample hog, confirms no restraint; 2-sample, confirms restraint. Existing probalance tests stay green.
**Effort.** S.
**Closes.** M-18.

### 4.7 — Revert-state-drift detection
**Scope.** Before reverting a profile on focus change, check that current process priority/affinity matches what `AppliedRecord` says we applied. If drifted (user changed via Task Manager), skip revert and log + emit `ActionFailed` with reason.
**Files.** `crates/engine/src/lib.rs:1723-1736, 1999-2006`.
**Risk.** Reading process state is a few extra syscalls per revert (handful per minute typical).
**Verification.** Manual: launch notepad, FrameSage applies perf, manually set notepad to High via Task Manager, alt-tab away, confirm notepad stays at High.
**Effort.** S.
**Closes.** M-19.

### 4.8 — OpenProcess error distinction
**Scope.** Replace blanket `Ok(None)` with explicit handling: `ERROR_INVALID_PARAMETER` (PID exited) → `Ok(None)` silently; `ERROR_ACCESS_DENIED` (protected) → `Ok(None)` once per PID then suppressed; anything else → `Err`.
**Files.** `crates/sys/src/inner/process.rs:147-151, 199-202, 356-359, 422-425, 652-655`.
**Risk.** None.
**Verification.** Unit test against a synthetic mock that returns each error code; confirm classification.
**Effort.** S.
**Closes.** M-08.

### 4.9 — Apply-failure backoff
**Scope.** Track `(pid, last_failure_at)` for apply failures. Skip re-apply for the same PID within 30s. Drop entry on PID exit or successful apply.
**Files.** `crates/engine/src/lib.rs:1832-1841, 1783-1791`.
**Risk.** A transient failure followed by ability to apply will wait up to 30s before retry. Acceptable.
**Verification.** Unit test: 3 rapid failures → 1 warn log line, not 3.
**Effort.** S.
**Closes.** M-15.

### 4.10 — Game Mode crash-recovery exe-name re-check
**Scope.** Recovery path mirrors runtime: query live exe for each suspended PID, skip Resume if mismatch.
**Files.** `crates/engine/src/lib.rs:1061-1080`.
**Risk.** None.
**Verification.** Synthetic: crash recovery with a journal pointing at a PID that's been reassigned; confirm we skip the wrong-exe Resume + log.
**Effort.** S.
**Closes.** M-16.

### 4.11 — Save-time policy validation
**Scope.** On `SetPolicy` accept, validate: every `AppRule.profile` references an existing ProfileId; every `Profile.cpu_sets`/`affinity_mask` is well-formed against current topology (Ccd(N) where N < CCD count). On failure, reject with Response::Error naming the offending field.
**Files.** `crates/service/src/runtime.rs:488-523`, `crates/core/src/policy.rs` (validation method).
**Risk.** Hand-edited policies that worked under v0.5 may fail validation; mitigate by returning warnings (not errors) for hand-edits, errors only for IPC mutations.
**Verification.** Test: SetPolicy with dangling ProfileId, expect Error. Test: hand-edit policy.json with dangling ref, hot-reload, expect warning log + previous policy retained.
**Effort.** S.
**Closes.** M-17, M-26.

### 4.12 — Esc closes modals
**Scope.** Bind Esc in `render_terminate_confirm_modal` and `render_affinity_picker_modal` to cancel.
**Files.** `crates/tray/src/main.rs:898-952, 959-`.
**Risk.** None.
**Effort.** Trivial.
**Closes.** L-15.

### 4.13 — Game Mode editor: full arsenal with discover-and-add wizards (STRENGTHENED per user)
**Scope.** Turn the Game Mode editor from a passive text-area into the **power-user surface that justifies the product**. Four parts:

  1. **Add-anything semantics with explanation, not silent drop** [reframed: H-19, M-35].
     - User can freely add ANY Windows service id (`stop_services`) or process exe (`suspend_processes`) — Spooler, Themes, SysMain, BluetoothUserService, ContosoCorpAgent.exe, custom in-house tools, anything.
     - If the entry is on the bundled denylist (kernel/AV/anti-cheat — the narrow set from 1.1), the editor shows it inline with a red "Blocked: <rationale>" hint and refuses to save with that entry. The rationale string comes from `gamemode/src/safe_lists/*.json:denylist[].rationale`.
     - If the entry is *not* on the denylist, it's accepted. No "this might be a bad idea" gatekeeping. The user knows their machine.
     - Side effect: closes H-19 and M-35 as features-not-bugs — the underlying behavior (denylist non-overridable) stays exactly as designed; the only fix is that the UI now shows the rejection reason instead of silently dropping the entry at apply time.

  **Plus:** per `audit/research/ANTI-CHEAT-MATRIX.md`, the discover-services and discover-processes views show AC binaries (vgc, BEService, EasyAntiCheat, FACEIT_AC, ESEAClient, vgk.sys, BEDaisy.sys, etc.) with the same greyed-out "Blocked: AC component — never touched" treatment. These are sourced from the AC detection module's hard deny-list (item 1.9), not the bundled safe-list JSON, so even when the JSON ships an "allow" the AC binaries stay protected. AC binaries also get a special pill-icon ("AC") so users see they're a different category than the bundled kernel/AV denylist.

  Each profile in the editor grows an **`AntiCheatProfile` selector** (Aggressive / Hybrid / SafeMode / Disabled) — the same enum item 1.9 added to `Profile`. Hover-text explains the four options with concrete trade-offs. Seeded rules show their current value with an info icon explaining "this default was set based on the AC matrix; we recommend leaving it." Manual override allowed but produces an in-modal warning when stepping a Valorant/CS2 rule down from SafeMode to Aggressive.

  2. **"Discover services" view** — new section in the profile editor.
     - Lists every running service with: service id, display name, status, **CPU time delta over last 60 s**, **memory working set**, **start type** (Manual/Auto/AutoDelayed/Disabled), **process exe** (for hosted services, the svchost group).
     - Sortable by CPU delta (descending) — the top of the list is "what's actually costing me CPU right now."
     - Each row: checkbox + "Add to stop list" button. Selected rows can be batch-added.
     - Hover-tooltip on each row shows: per-service rationale if it exists in the safe-list JSON allowlist; "Unknown service — research before stopping" otherwise.
     - Denylist services are visible but greyed out with "Blocked: <rationale>" inline.

  3. **"Discover background processes" wizard** — paired surface.
     - Lists processes that are NOT the foreground / NOT in `applied` / NOT in the bundled denylist.
     - Sorted by CPU% over last 60s (descending). Top N (default 30) shown, with "show all" expander.
     - Per-row: exe + description + publisher (from VersionInfo cache) + CPU% + memory + Suspend Yes/No checkbox.
     - "Add selected to suspend list" button writes to the currently-edited profile's `suspend_processes`.
     - Same denylist treatment: critical processes visible but greyed.

  4. **Preview button before save** (existing L-22).
     - Click "Preview" → modal showing exactly what *this* profile would do if activated against the current foreground app: list of services that would be stopped (with current status), list of processes that would be suspended (with current PIDs + working sets), power-plan switch (from X to Y), taskbar action, Windows Update action. "This will free approximately N MB of working set and X% of CPU based on current samples."
     - Apply (saves the profile + closes preview) / Cancel (discards changes).

**Files.** `crates/tray/src/main.rs:5196-5208` (existing Game Mode editor), new `crates/tray/src/tabs/profiles/discover.rs` (services + processes discovery views), `crates/sys/src/inner/services.rs` (new — service enumeration with CPU sampling via QueryServiceStatusEx + open/close service handles), reuse `framesage_ipc::ProcessSnapshot` for process discovery.
**Risk.** Service enumeration via `EnumServicesStatusExW` is a few thousand entries on a typical box — sample sparingly (10 s cadence when the Discover view is open; not at all when closed). CPU-time per service requires QueryServiceStatusEx + sampling at intervals; same pattern as our existing per-PID sampling.
**Verification.** Open Discover services view, confirm WSearch / BITS / OneDrive appear with non-zero CPU delta. Confirm csrss / lsass / vgc appear greyed with rationale. Add Spooler to stop list (un-greyed because not on denylist; no in-product justification needed). Activate the profile → service stops → revert restores it.
**Effort.** L (this is the marquee surface).
**Closes.** L-21 (rationale display), L-22 (dry-run preview), H-19 (reframed: kept behavior, fixed UX), M-35 (reframed: same).

### 4.14 — README + product positioning (REVISED per user)
**Scope.** Three additions / changes:

  1. **Product position statement (new top of README, above "What it does"):**

     > **FrameSage is for users who want maximum performance during games or focused work sessions.** It will stop background services (Windows Update, Search, telemetry, OEM updaters, cloud sync) and suspend non-essential processes (OneDrive, Dropbox, GameBar, Widgets, RGB tools) during a session. Everything is reversed when the session ends. Every action is journaled and reviewable after the fact.
     >
     > **If you'd rather a gentle optimizer, this isn't the right tool.** Process Lasso's ProBalance-only mode or Windows' built-in Game Mode are better fits for that.
     >
     > FrameSage's contract: aggressive by design, transparent about every action, fully reversible. You opt in eyes-open via the first-run choice.

     Same text drives the first-run dialog (item 4.1 Page 1) so the framing is consistent.

  2. **"What gets stopped/suspended" disclosure section** (new under "Game Mode"). Mirrors the first-run lists: full service list with rationale, full process list with rationale, power plan change, taskbar action, Windows Update pause. The whole arsenal, fully named. No surprises.

  3. **Existing items kept:** install residue, complete-removal recipe (now also covered by item 1.5's real uninstaller), SmartScreen warning + SHA256 verification path, "Known limitations" section (single processor group / no Intel hybrid pre-3.7 / polling re-assert vs driver callback).

**Files.** `README.md` (top + new disclosure section + existing items).
**Risk.** None.
**Effort.** S.
**Closes.** M-33. Sets the contract that all other docs / dialogs / tooltips reference.

### 4.15 — Activity events carry matched-rule index + ProBalance cumulative stats
**Scope.** Add `matched_rule_index: Option<usize>` to ForegroundChanged event. Add a "Session stats" card to Status tab: ProBalance demotions today, services stopped today, profiles applied today (pulled from activity.jsonl).
**Files.** `crates/ipc/src/lib.rs:338-364`, `crates/tray/src/main.rs` Status tab.
**Risk.** None.
**Effort.** S.
**Closes.** M-22, H-31.

**Group 4 stop-for-review point.** Final QoL pass complete; v0.6 ready to ship.

---

## Default rules: final decisions (post user-reframe)

### D-1 — Remove BITS from default game-x3d
**Status: REJECTED by user.** Keep BITS in aggressive default. Stopping cloud-sync / WU transport during a 1–4 hour gaming session is the product. The journal entry will show the user when it was stopped and when it was restarted. Transfers resume on restart — that's what BITS is designed for.

### D-2 — Remove WSearch from default game-x3d
**Status: REJECTED by user.** Keep WSearch in aggressive default. Indexing during gaming is exactly the kind of background CPU/IO the product exists to eliminate.

### D-3 — Remove ClickToRunSvc from default
**Status: REJECTED by user.** Keep in aggressive default. Office background sync can wait 2 hours.

### D-4 — Kind(Cache) fallback to TopRanked(8) when empty
**Status: ACCEPTED.** Correctness fix, not an aggression dial. Silent no-op while still paying the Game Mode tax is the wrong outcome regardless of philosophy. Lands in item 1.7.

### D-5 — Aggression stays at full strength; informed-consent gate at first-run
**Status: REPLACED.** Original proposal "ship Game Mode disabled by default" rejected. New mechanism: aggressive Game Mode is unchanged in definition, but the seeded BF6/Valorant/Fortnite rules ship with `game_mode: None` at install time. The first-run onboarding (item 4.1) presents the full disclosure (every service, every process, every action, with the JSON rationale strings) and the user picks one of: **Aggressive** (full sledgehammer) / **Balanced** (pin + priority + power plan only) / **Pinning only** (affinity only). Their choice populates the seeded rules' `game_mode` field via `SetPolicy`. No defaults pre-selected. The user opts in eyes-open or not at all.

### D-6 — Keep BF6 / Valorant / Fortnite rules
**Status: ACCEPTED.** Pre-seeded game rules give users immediate value on the games they're most likely to install for.

### D-7 (new) — `manual_global_eligible` defaults
**Status: NEW.** Profiles touching system-wide state (any profile with non-None `game_mode`) default to `manual_global_eligible: true`. Narrow per-app profiles (eco, perf) default `false`. Users can override per profile in the editor.

### D-8 (new) — Re-classification: things the auditor flagged are features
**Status: NEW.** Several Phase-1 findings are reframed as expected behavior, not bugs:

  - **M-20 (BITS in-flight check):** rejected. Aggressive mode stops BITS unconditionally; journal records; revert restarts. The contract.
  - **H-19 (user-added safe-list dropped at apply time):** behavior kept, UX fixed. The bundled denylist for kernel/AV/anti-cheat is non-overridable BY DESIGN. The fix is to surface the rejection reason inline in the editor (item 4.13), not to allow override.
  - **M-35 (suspend_processes unknowns dropped):** same — behavior kept (critical-process protection), UX fixed (rationale shown).

### D-9 (new, from AC matrix) — Seeded Valorant rule ships SafeMode-locked
**Status: NEW.** Valorant rule ships with `ac_safe_mode_target: AntiCheatProfile::SafeMode`. The first-run dialog's three-way choice (Aggressive/Balanced/Pinning) applies to all other games BUT NOT TO VALORANT. The user can step Valorant down from SafeMode to a more aggressive tier per-rule in the profile editor with a separate eyes-open warning (item 4.13), but the default ships safe. This is non-negotiable because (a) Vanguard does hardware bans, (b) the AC matrix research found documented VAN: Competitive Restrictions on Process Lasso users, (c) "the user opted into aggressive defaults, not into account suicide." Mirrors Hone's 1M+ user model.

### D-10 (new, from AC matrix) — Seeded BF6 rule ships Hybrid by default
**Status: NEW.** BF6 rule ships with `ac_safe_mode_target: AntiCheatProfile::Hybrid`. Environment actions (services / processes / power / taskbar) run at the user's first-run-chosen aggression level. Game-process actions (affinity / CPU sets / priority / IO prio / power throttling on `bf6.exe`) default OFF, with the profile editor showing an inline warning naming EA Javelin's core-parking blocker and the press coverage that named Process Lasso as risk-bearing.

### D-11 (new, from AC matrix) — ESEA auto-pause is built in
**Status: NEW.** When the AC detection probe (item 1.9) sees `ESEAClient.exe` running, the engine enters STANDBY mode regardless of policy contents — no apply calls, no scans, no actions until ESEAClient exits. Tray shows banner: "ESEA Client detected. FrameSage is on standby to avoid Error #107. Resumes when ESEA exits." User can override via tray menu (with confirmation) but the default is auto-pause. Sidesteps the documented Process-Lasso-class conflict without requiring user action.

### M-A (new, from AC matrix) — Mandatory BattlEye support outreach before any BE-game seeded rule ships
**Status: NEW non-code milestone.** Before adding seeded rules for Tarkov / PUBG / R6 / DayZ / ARMA / Squad / Destiny 2 (none ship in v0.6), send `support@battleye.com` a pre-launch heads-up with: signed FrameSage binary SHA256s, description of what the tool does, hard-deny-list of BE binaries we never touch, list of access masks we use (PROCESS_SET_INFORMATION-only), commitment to never load a kernel driver / inject DLLs / open VM_READ/VM_WRITE handles. Mirror the path Bitsum / RTSS / MSI used. This is process work, not code work — track as a v0.7 prerequisite. v0.6 ships only the existing three seeded games (Valorant + BF6 + Fortnite).

---

## Execution sequencing (revised — Group 1 now has 9 items including AC infra)

| Order | Group | Items | Stops | Estimated total effort |
|---|---|---|---|---|
| 1 | Must-fix | 1.1–1.9 (added 1.9 AC detection + Safe Mode infra per AC matrix) | 1 stop after the group | ~M–L total (1.9 alone is L) |
| 2 | High-leverage | 2.1–2.11 | 2 sub-stops (2a after 2.1+2.3, 2b after 2.7+2.8+2.9) + 1 stop after the group | ~L total |
| 3 | Structural | 3.1–3.8 | 1 stop after the group | ~L total (3.1 + 3.7 are the long poles) |
| 4 | Polish | 4.1–4.15 | 1 stop after the group | ~M–L total |

Total stops: **5** (1 + 3 + 1 + 1) — unchanged.

**Pre-ship checklist (BLOCKS Group 4 release):** see `audit/research/ANTI-CHEAT-MATRIX.md` final section. Most checklist items land in Group 1 (item 1.9 covers detection + invariants + ESEA auto-pause) or are non-code milestones (BattlEye outreach M-A; Authenticode signing deferred but required before BE-game seeded rules ship).

---

## What's deferred (out of scope for this plan)

These are real items but better handled as separate engagements:

- **MSI / Inno installer + code signing.** Multi-week, requires EV cert procurement. Track as v1.0 prerequisite.
- **ETW / PresentMon integration (closed-loop measurement).** README v0.3 roadmap item. Architectural; deserves its own scoping pass.
- **PPL / EDR enumeration.** Listing protected processes that we *can't* touch even with safe-list pass is a separate API surface.
- **Screen reader (accessibility-kit) integration.** egui-upstream limitation; deserves an upstream contribution rather than a workaround.
- **Per-process command line / open handles in detail panel.** Process Explorer parity; nice-to-have, not a competitive gap.
- **Driver callback for instant pin re-assert** (vs our 2s polling). Out of scope per anti-cheat policy.

---

**Awaiting your approval to begin Group 1.** Please call out:
- Any item to drop or defer.
- The default-rules batch (D-1 to D-6) — approve as-is, or per-item changes.
- Whether the 5-stop cadence is right or you want more granularity.
