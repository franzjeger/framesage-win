# 04 — Reliability Audit

Scope: surviving sleep/resume, fast user switching / RDP, dynamic CPU
topology, mid-op process exit / PID reuse, service crash, config corruption,
upgrades, journal recovery, tray crash, IPC loss, watchdogs, per-PID bookkeeping
growth. Read-only review — no code modified.

Severity legend: **CRIT** (data loss / unrecoverable state) ·
**HIGH** (functional regression survives indefinitely) ·
**MED** (degraded behaviour, self-healing on next event) ·
**LOW** (minor / cosmetic).

---

## What is already done well

- **Crash-safe Game Mode journal with intent-first write.**
  `engine/src/lib.rs:1931-1939` writes the full intended `AppliedActions`
  *before* mutating any kernel state. A SIGKILL between journal and apply
  causes recovery to undo a no-op (idempotent), not miss real state. This
  is the rare implementation that gets this ordering right.
- **Schema-versioned journal with fail-safe.** `gamemode/src/journal.rs:118`
  rejects mismatched versions, and `engine/src/lib.rs:1065-1069` deletes
  an unparseable orphan instead of crash-looping.
- **Atomic policy writes.** `core/src/policy.rs:285-296` and
  `gamemode/src/journal.rs:140-167` both use `temp + rename`. Journal also
  performs best-effort `fsync` (`journal.rs:157`) and cleans up the temp on
  rename failure (`journal.rs:161-167`).
- **UTF-8 BOM tolerance on policy load** — `core/src/policy.rs:240`. Catches
  PowerShell 5.1's `Set-Content -Encoding UTF8` output.
- **PID-reuse defense via exe-name compare.** Stored at apply time in
  `AppliedRecord.exe_name` (`engine/src/lib.rs:199-207`); re-checked every
  2 s persistent reassert (`lib.rs:1413-1432`) and the affinity-rule sweep
  (`lib.rs:1468-1482`). The exit case drops the record cleanly
  (`lib.rs:1444-1449`).
- **Singleton mutex with handoff grace + cross-instance show-window event.**
  `tray/src/win32.rs:125-153` waits up to 3 s for an exiting prior tray
  before declaring "already running"; the secondary signals the primary
  to focus rather than failing (`win32.rs:214-233`). Mutex handle is RAII
  so a crashed process auto-releases.
- **Defense-in-depth ACL on status pipe.** `runtime.rs:320-330` rejects
  mutating requests on the status pipe even if its DACL is permissive.
- **Pre-armed next-instance pipe.** `runtime.rs:246-257` creates the next
  pipe instance before accepting the current one to close a busy window.
- **Per-PID bookkeeping is pruned every background scan.** `applied`
  (`lib.rs:1559-1568`), `affinity_rule_applied` (`lib.rs:1572`), `user_cache`
  (`lib.rs:753-755`), and ProBalance state (`lib.rs:1351-1363`) all retain
  only live PIDs.
- **Manual override re-evaluation** (`lib.rs:865-939`) closes the
  "stranded Game Mode after manual-off" bug end-to-end.
- **`#[serde(default)]` on every additive policy field** (`policy.rs:56,87,97,
  106,111,119,125` and `profile.rs:149-196`) means a v0.1 `policy.json`
  loads under v0.5.

---

## Findings

### CRIT-1 — No SCM `FailureActions` configured; service crash is permanent
`cli/src/main.rs:239-250` builds `ServiceInfo` with no failure-actions
field. The crate's `ServiceInfo` doesn't carry one, and `install_service`
never calls `ChangeServiceConfig2W(SERVICE_CONFIG_FAILURE_ACTIONS)`.
The README/installer (`install.ps1:113-128`) doesn't run `sc.exe failure`
either. Scenario: any tokio task panic that propagates, an OOM, or a wild
syscall result kills the process; SCM marks it Stopped; FrameSage stays
down until reboot or manual `Start-Service`. Combined with `panic = "abort"`
(`Cargo.toml:95`) this turns any non-caught panic in the tick loop into a
silent permanent outage.
Mitigation needed: `restart/restart/restart` with a small reset period.

### CRIT-2 — Service `tick` task panic kills the engine silently
`runtime.rs:56-65` spawns `tick_engine.tick()` in a `tokio::spawn` that only
logs errors; a *panic* inside the task aborts the task but the SCM `Running`
state survives because the main task is just awaiting `shutdown`. With
`panic = "abort"` (`Cargo.toml:95`), one panicking syscall takes the whole
process down — see CRIT-1 — but without `panic = abort`, a thread-local
panic would leave the service running with no engine ticking. Either way
there is no self-restart of the tick task and no watchdog. Same problem
for `admin_handle`/`status_handle`/`reload_handle` (`runtime.rs:71-91`).

### HIGH-1 — No sleep/resume handling at all
Grep across the tree finds zero references to `WM_POWERBROADCAST`,
`RegisterPowerSettingNotification`, `PBT_APMSUSPEND`, `PBT_APMRESUMESUSPEND`,
or any equivalent. Implications:
- After a multi-hour sleep, the wall-clock-tied
  `Instant::now()`-based intervals (`engine/src/lib.rs:173,184,190`) are
  monotonic and continue to advance, so they fire as expected — that's OK.
  But the very first post-resume tick will compute a multi-hour `elapsed_100ns`
  in `list_process_snapshots` (`lib.rs:574-582`) and ProBalance
  (`lib.rs:1150-1155, 1218-1222`). The CPU-% formula uses
  `delta * 100 / elapsed_100ns` and is **saturated** with `min(u16::MAX)` /
  `min(100)`, so it produces 0 instead of overflow — safe but the *first*
  post-resume sample is meaningless. Acceptable, but document it.
- Bigger issue: Game Mode actions taken pre-suspend (taskbar hidden, services
  stopped, power plan switched) survive suspend, and on resume Windows can
  re-enable taskbar / restart services on its own → the engine's idea of
  "we hid the taskbar" diverges from reality. No reconcile-against-current-
  state happens; the journal still shows we hid it, so on revert we'll try to
  show an already-visible taskbar (idempotent, fine), but if a stopped
  service auto-restarted on resume, we'll happily stop it again at the
  next focus change. No corruption, but user-visible flicker.
- **Power plan reverts** — if user manually changed the plan during sleep
  resume (e.g. battery mode kicks in on a laptop), our journal still says
  "previous = Balanced" and we'll snap them back when the game profile exits.
  Surprise. `state.rs:21-22` notes the field is `Option`, but it's set
  to the value queried at apply time, not re-queried on resume.

### HIGH-2 — No WTS session change / RDP handling
No `WTSRegisterSessionNotification`, `SERVICE_ACCEPT_SESSIONCHANGE`, or
session-tracking anywhere. `main.rs:73-74` only accepts `STOP | SHUTDOWN`.
The architecture (`runtime.rs:1091-1104`) assumes a single user-session
foreground reporter (the tray) feeds `reported_foreground`. On fast user
switching / RDP disconnect:
- The original session's tray keeps reporting until killed; on session
  disconnect (`WTSDisconnectSession`) the tray process typically keeps
  running but `GetForegroundWindow` returns NULL — tray correctly sends
  `ReportNoForeground` (`tray/src/main.rs:6058-6068`), so engine sees idle.
  OK.
- New session's tray starts (per-user Startup folder shortcut,
  `install.ps1:96-101`), connects to the *single* admin pipe, both
  trays now race to report foreground. Engine has no "which session is
  active" arbitration — the last writer wins per tick. On a multi-user
  box, the engine may apply session A's foreground rule while the active
  session is B. No corruption but wrong-profile activation.
- Game Mode actions (hide taskbar, stop services) are global, so a Game
  Mode entered by session A persists across a switch to session B —
  session B sees no taskbar and stopped Search. Bad UX.

### HIGH-3 — CPU topology captured exactly once at startup; no hot-plug
`runtime.rs:36` calls `detect_topology()` once; the result is stored in
`EngineState.topology` (`lib.rs:67-68`) and only **cloned** thereafter
(`lib.rs:382, 907, 1012, 1253, 1404, 1462, 1556, 1767`). Scenario: laptop
with E-cores parked under battery, then plugged in and additional cores
come online; CPPC ranking changes after a BIOS update + warm-reset; VM
gets extra vCPUs hot-added. The X3D-CCD selector keeps pointing at the
old indices. Severity HIGH because the user explicitly asked for that
core set and now silently gets a different one. No mechanism to refresh
short of full service restart.

### HIGH-4 — Tray IPC has no reconnect; foreground reporter is one-shot
`tray/src/main.rs:6044-6079` opens `OpenOptions::open(PIPE_NAME_ADMIN)` per
250 ms tick. Each iteration is a fresh blocking `open()` call against the
admin pipe (`main.rs:6087-6104`). That's actually robust to service
restarts — every report is independent. BUT:
- `OpenOptions::open` on a named pipe with no available instance blocks
  or returns `ERROR_PIPE_BUSY`; there's no `WaitNamedPipeW` retry and no
  timeout. During a service restart the call can hang an unbounded time;
  the result is silently dropped (`main.rs:6075` `let _ = ...`). The
  reporter thread will sit blocked across the entire service-restart
  window — `last_pid` stays None-or-stale, no recovery once service is
  back, until the OS unblocks the syscall.
- Worse: any other tray→service call (`send_request_blocking`,
  `main.rs:6087`) called from the UI thread can hang the UI for the
  duration of a service outage. There's no timeout / no `CancelSynchronousIo`.

### HIGH-5 — Service has no stale-reporter detection
`engine/src/lib.rs:1093-1104`: once `foreground_reporter_seen = true`, the
engine **only** uses `reported_foreground`, never falls back to
`framesage_sys::foreground::current()`. If the tray dies (crashed, killed
by user, OOM), `reported_foreground` retains its last value forever and
the engine keeps applying that profile until service restart. There is no
"stale-after-N-seconds" check, no last-report timestamp tracked, no
heartbeat. Severity HIGH because a crashed tray leaves the system stuck on
e.g. the game profile (taskbar hidden, services stopped) with the user
unable to revert short of running the CLI panic-button or restarting the
service.

### HIGH-6 — Process exit between `iter_pids` and `exe_for_pid` is the only
TOCTOU defense; OpenProcess error is collapsed to `Ok(None)`
`sys/inner/process.rs:147-151, 199-202, 356-359, 422-425, 652-655`: every
`OpenProcess` failure is mapped to `Ok(None)` indiscriminately — no
distinction between `ERROR_INVALID_PARAMETER` (PID exited),
`ERROR_ACCESS_DENIED` (protected), or anything else. The engine treats all
three as "skip and pretend nothing happened" (`lib.rs:1180-1187, 1417-1422,
1595-1598, 1650-1654`). This is *correct* for the steady-state — protected
processes shouldn't log-spam every tick — but it masks legitimate bugs
(e.g. an installer that's stripped our token of `PROCESS_QUERY_LIMITED_
INFORMATION`). Suggest at least one DEBUG-level log path that distinguishes.

### HIGH-7 — Foreground reporter loop sleeps with `std::thread::sleep` and
ignores cancel
`tray/src/main.rs:6044-6079` is a `loop {}` with `thread::sleep(250ms)` and
no cancel signal. When the tray exits via the egui close handler, this
thread isn't joined — it's a daemon and the process exit kills it. Fine for
tray exit. BUT if `OpenOptions::open` is blocked in the kernel for a long
time (HIGH-4), the thread can outlive the egui main loop's clean shutdown
intent. Symptom: tray "process still running" briefly on Task Manager
after window close. Low impact but real.

### MED-1 — IPC over admin pipe re-opens 4× per second across processes
`runtime.rs:246-269` accepts; each accept calls `tokio::spawn`. The
foreground reporter alone is 4 connections/sec. Over a 24-hr session
that's ~350k connection lifecycles. No leak visible (each task is dropped
after `handle_client` returns), but a long-running session under tracing
verbose builds up significant log volume at debug level
(`runtime.rs:267-268`). Cosmetic.

### MED-2 — `version_info_cache` is unbounded and never pruned
`engine/src/lib.rs:128, 692-705`. Keyed by full exe path string. The
in-source comment ("never evict — paths stable, cache stays small ~200
entries") is wrong in two scenarios:
1. **Upgrade-on-disk**: when the user updates an app in place, the
   description/company strings in the binary's resource may have changed;
   the cache returns stale strings until service restart.
2. **One-off paths**: portable apps run from `%TEMP%\<uuid>\foo.exe`,
   debuggers compiling-and-running fresh paths, installers spawning helpers
   from %TEMP%. Each unique path is a permanent cache entry. On a CI box
   running `cargo run --bin foo` thousands of times, the cache grows
   unbounded. ~150 bytes × N is not huge, but it is a slow leak.

### MED-3 — Policy save on a `SetPolicy` IPC call has no retry; the in-memory
mutation can outlive a corrupted file
`runtime.rs:503-523`. The in-memory policy is updated *before* the save
attempt. If the save fails (disk full, file locked by an editor, ACL
mismatch), the engine continues happily applying the new policy in memory,
and the next service restart reverts everything. The user sees a banner
explaining "edit applied but will be lost on restart"
(`runtime.rs:513-519`) — good UX, but the policy at this point is a
ticking time-bomb. Same problem in `SetAffinityRule` (`runtime.rs:435-449`)
and `DeleteAffinityRule` (`runtime.rs:469-486`).

### MED-4 — Atomic rename across volumes can fail; no fallback to copy+rename
`policy.rs:285-296` and `journal.rs:160-167` both rely on `std::fs::rename`.
On Windows, `MoveFileEx` defaults don't allow cross-volume moves; if
`%ProgramData%` is a symlink to a different volume (corporate redirected
folders are real), the rename returns `ERROR_NOT_SAME_DEVICE` and we lose
the new state. Comment at `policy.rs:282-284` acknowledges the same-volume
requirement but doesn't fall back. Low frequency, but when it hits the
user loses all in-memory edits on next restart.

### MED-5 — `applied` HashMap entry insertion on apply *failure* path
`lib.rs:1832-1841`: when `apply_profile` errors, the engine still updates
`current_foreground` / `foreground_snapshot` / `active_profile` but
**doesn't** add an `applied` record. Correct. However combined with
`already_correct` short-circuit (`lib.rs:1783-1791`), if the same PID
fails to apply, next tick treats it as "no prior record, try again" → log
spam at warn level once per focus return. Not severe but the apply-failure
case has no exponential backoff / try-once-then-skip.

### MED-6 — Game Mode journal recovery never checks if the recovered
suspended PIDs are still alive
`lib.rs:1061-1080` calls `sys_revert_all` with the journal's recorded
`suspended_pids`. If our crashed session suspended pid=1234 (OneDrive.exe)
and during the crash the user killed it and pid=1234 is now a fresh
notepad.exe, `revert_all` will call `NtResumeProcess(1234)` on the new
notepad. `NtResumeProcess` on a not-suspended process is a no-op
(documented), so practically benign — but in principle the exe-name
recorded in `SuspendedProcessSnapshot` (`state.rs:75-79`) is *never
checked* against the live PID during revert. Same defense as the
runtime-PID-reuse path would be free to add.

### MED-7 — Hot-reload coalesces 250 ms but doesn't validate before swap
`runtime.rs:181-191`: on FS-watcher fire, `Policy::load` parses then
`engine.set_policy` swaps. The load function rejects invalid JSON, good.
But if the user saves an internally-inconsistent policy (e.g. a rule
referencing a `ProfileId` that doesn't exist in `profiles`), it loads
cleanly and the engine then fails at match time (`lib.rs:1759-1764`,
silently logs "matched profile id not found") and falls through. No
schema validation pass. Low severity — engine recovers — but the user
gets no feedback that their hand-edit is broken.

### LOW-1 — `signal_existing_tray_show_window` race window
`win32.rs:209-233` documents the race: the secondary may arrive before
the primary creates the event. Mitigation comment claims "the primary
will already show its window naturally"; that's true only if the primary
shows on startup, which it does. Acceptable.

### LOW-2 — `tick_handle.abort()` doesn't await; in-flight kernel calls
`runtime.rs:97-100`: aborting a tokio task drops it; if a tick is mid-way
through `apply_profile` (which calls into `framesage-sys` blocking
syscalls), the abort point is the next `await`. Most apply paths are
blocking calls without awaits, so the abort doesn't actually interrupt
them — but the runtime then drops while the kernel call returns to a
dropped frame. Soundness-wise this is fine (Rust drop), but there's no
explicit drain on shutdown. Mostly cosmetic; the SCM `STOP_PENDING` window
is 30 s by default and a tick completes in ms.

### LOW-3 — Tray's `applied` ↔ tray UI sees stale data across restarts
The status pipe `ListProcesses` returns engine snapshots; if the service
restarts mid-session, the tray shows momentarily-empty `managed_profile`
fields because the new service has an empty `applied` map until the next
foreground change. Self-heals in seconds.

---

## Summary

The codebase is unusually conscientious about crash-safety (journal-before-
apply, atomic temp+rename, schema versions, BOM tolerance, PID-reuse exe
checks). The reliability gaps are concentrated in *environmental events
the code doesn't acknowledge*:

1. No SCM `FailureActions` (CRIT-1).
2. No sleep/resume hook (HIGH-1).
3. No WTS session-change hook (HIGH-2).
4. No topology refresh after hot-plug (HIGH-3).
5. No tray-reporter staleness detection (HIGH-5).
6. Tray IPC has no timeout/cancel and can hang the UI (HIGH-4, HIGH-7).

Fixing CRIT-1 + HIGH-5 alone closes the two failure modes most likely to
leave a user stuck on a hidden taskbar with stopped services and no working
tray — i.e. the worst UX outcomes the journal exists to prevent.
