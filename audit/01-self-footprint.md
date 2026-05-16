# Self-Footprint Audit

## TL;DR
Footprint is **moderate but salvageable** — the service's idle floor is dominated by ProBalance's 1 Hz "OpenProcess every live PID three times" sweep, and the tray's 4 Hz foreground reporter that opens a fresh admin-pipe instance on every tick. There are no timer-resolution abuses, no handle leaks I can find, and the caching/eviction discipline is mostly sound; but the engine doesn't yet know how to ride event sources (ETW process notifications, foreground WinEvent hook), so it pays a brute-force-polling tax that scales with system process count.

## Critical

### C1. ProBalance sweep OpenProcesses every live PID 3× per second
`crates/engine/src/lib.rs:1162-1248` — `maybe_run_probalance_locked` runs every `PROBALANCE_SAMPLE_INTERVAL` (1 s) when enabled. For every live PID returned by `iter_pids()` it calls **three** separate handle-creating functions:
- `framesage_sys::process::cpu_times(*pid)` — OpenProcess + CloseHandle (`crates/sys/src/inner/process.rs:356`)
- `framesage_sys::process::exe_for_pid(*pid)` — OpenProcess + CloseHandle (`crates/sys/src/inner/process.rs:147`)
- `framesage_sys::apply::get_priority_class_for_pid(*pid)` — OpenProcess + CloseHandle (`crates/sys/src/inner/apply.rs:308`)

Impact: on a typical desktop with ~250 PIDs that is ~750 OpenProcess/CloseHandle pairs per second, **purely to drive ProBalance** — and ProBalance is on by default. Each pair is a syscall round-trip + an ACL check; you also allocate a `String` for every exe name via `to_ascii_lowercase()` (line 1185) and discard it immediately for PIDs you decided not to consider. **Severity: Critical** for a 24/7 utility — this is the largest single CPU/wakeup contributor at idle. Mitigation: one consolidated NT call (`NtQuerySystemInformation(SystemProcessInformation)`) returns per-PID CPU times, image name, and priority class in a single hop; ToolHelp's `iter_pid_snapshots` already proves the pattern. If you keep the per-PID path, at minimum batch `exe_for_pid` results into a cache keyed by `(pid, create_time)` so re-sampling existing PIDs doesn't re-OpenProcess them.

### C2. `list_process_snapshots` does ~5 OpenProcess per PID — polled at 1 Hz by tray
`crates/engine/src/lib.rs:567-820`. For every PID in the toolhelp snapshot the engine opens and closes process handles for:
- `exe_for_pid` (616)
- `get_priority_class_for_pid` (626)
- `affinity_mask` (631)
- `memory_info` (640)
- `cpu_times` (648)
- `user_for_pid` (716) — gated by an 8-per-tick budget, OK

That is **5 OpenProcess/PID** every time the tray polls. The tray polls Status + ListProcesses on the same status-pipe connection every 1 s while the window is visible (`crates/tray/src/main.rs:5945`). On a 250-PID box that is ~1250 OpenProcess/sec **while the Processes tab is on screen**. The hidden-window throttle to 8 s (line 5946) is excellent; the visible-window cost is not. **Severity: Critical** during visible use, **High** at idle. Mitigation: same as C1 — one `NtQuerySystemInformation` call returns most of those fields in a single allocation. Failing that, hold one process handle per PID across the loop instead of re-opening for each field (`memory_info`, `cpu_times`, `affinity_mask`, `get_priority_class` all only need `PROCESS_QUERY_LIMITED_INFORMATION`).

## High

### H1. Foreground reporter hammers the admin pipe at 4 Hz with single-shot connections
`crates/tray/src/main.rs:6043-6079` — `foreground_reporter_loop` sleeps 250 ms, calls `framesage_sys::foreground::current()`, and forwards the result via `send_request_blocking(PIPE_NAME_ADMIN, …)` which calls `OpenOptions::new().read(true).write(true).open(PIPE_NAME_ADMIN)` (`crates/tray/src/main.rs:6091`) — a **fresh pipe instance per call**. On the service side every accept spawns a new tokio task (`crates/service/src/runtime.rs:265`) which the comment at runtime.rs:240 explicitly calls out as a known hot path. Impact: 4 admin-pipe accepts/sec, 4 tokio task spawns/sec, 4 JSON encode/decode pairs/sec, plus the work in the engine's `report_foreground` which takes the engine `RwLock` for write (lib.rs:944). Also: this loop runs **regardless of whether the tray window is visible** — there is no `window_visible` gate, unlike `processes_poll_loop`. **Severity: High** — wakeups/sec floor + cross-process IPC + writer-lock churn that contends with the 300 ms tick. Mitigation: cache the previous (pid, title) tuple and only send when it changes (you already short-circuit `None → None`; do the same for `Some(same) → Some(same)`). Drop cadence to 500–1000 ms when window is hidden. Better: switch to a `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` in the tray and only report on the event.

### H2. Per-tick `topology.clone()` and `policy.clone()` in lock-held hot paths
- `crates/engine/src/lib.rs:1404` — `let topology = s.topology.clone();` inside `maybe_reassert_persistent_locked`, fires every 2 s.
- `crates/engine/src/lib.rs:1556` — same clone in `maybe_scan_background_locked`, every 10 s.
- `crates/engine/src/lib.rs:1462` — second clone in the same function for the affinity-rule path.
- `crates/engine/src/lib.rs:1767` — `let topology = s.topology.clone();` inside `reconcile` on every foreground change.
- `crates/engine/src/lib.rs:548` — `status()` clones the entire `Policy` on every IPC `Request::Status`. Tray polls Status every 1 s, so this is **~1 Policy clone/sec** while the window is visible.
- `crates/engine/src/lib.rs:442` — `policy_snapshot()` clones again on every SetAffinityRule / SetPolicy (rare; fine).

Each `CpuTopology` is a `Vec<LogicalCpu>` of `cpus` (~16–24 entries), 50–100 bytes each. Each `Policy` carries rules + profiles + the affinity-rules vec. None are huge but they hit the allocator on the hot path. **Severity: High** in aggregate — allocator pressure on a 24/7 service shows up as commit-charge growth in monitoring. Mitigation: wrap `topology` in `Arc<CpuTopology>` (it's immutable after startup); have `status()` return `Arc<Policy>` or a leaner `StatusSummary` that copies only what tray needs.

### H3. `apply_thread_cpu_sets` snapshots *every thread in the system* per call
`crates/sys/src/inner/apply.rs:610-654` — `CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0)` enumerates **all threads in the system** (Microsoft's documented behaviour for `dwProcessID = 0`), then filters by PID in user space. Called from `apply::apply` (line 122), `reassert` (line 187), and `revert` (line 269). The persistent re-assert sweep runs `reassert` every 2 s for every PID running a persistent profile **with `cpu_sets` configured** (which is the canonical X3D path). On a busy gaming box that is ~3000 thread entries marshalled and filtered every 2 s **per persistent PID**.

**Severity: High** when persistent + cpu_sets is the active path (i.e., the most-marketed feature). Mitigation: use `NtQueryInformationProcess(ProcessBasicInformation)` to get the thread list scoped to a single PID, or cache the thread-IDs across re-asserts and only refresh on thread-count drift detected via the cheap `cntThreads` from `iter_pid_snapshots`.

### H4. Safe-list and ignore-list HashSets rebuilt every ProBalance sample
`crates/engine/src/lib.rs:1261-1270` — every 1 s, ProBalance constructs `safe_list_exes` (lowercased clone of the entire safe-list denylist) and `user_ignore_exes` (lowercased clone of the config). Both are immutable across the engine's lifetime modulo a policy reload. **Severity: High** for steady-state allocation rate. Mitigation: build once, store in `EngineState` (or `&'static` for the safe-list), invalidate only on `set_policy`.

## Medium / Low / Polish

### M1. Service tick rate is 300 ms even when nothing is happening
`crates/service/src/runtime.rs:57` — `interval(Duration::from_millis(300))`. Most ticks early-return on `new_pid == s.current_foreground` (engine.rs:1711), so the cost per tick is one `RwLock::read` + one `framesage_sys::foreground::current()` *only when no foreground reporter has been seen yet*. With the tray running, the service's own foreground poll is bypassed (engine.rs:1098), so the steady-state tick is essentially a lock-read and a clone. Still — 3.3 wakeups/sec at idle, forever. **Severity: Medium**. Mitigation: make the tick driven by the tray's `ReportForeground` (which is event-coalesced) for the foreground reconcile, and run the background-scan / re-assert / probalance loops on their own coarser tokio intervals (10 s / 2 s / 1 s) so the 300 ms cadence disappears entirely. The engine already has the interval constants; it just needs to demote the master tick from "every 300 ms touch everything" to "wake on event, lazy intervals for the rest."

### M2. Foreground reporter retries on transient pipe failure without backoff
`crates/tray/src/main.rs:6072-6076` — when `send_request_blocking` fails (service down, ERROR_PIPE_BUSY), the loop silently retries 250 ms later. During service restart that is ~4 failed CreateFile calls per second for as long as the outage lasts. **Severity: Low** but it does affect logs / ETW and can mask real problems. Mitigation: exponential backoff up to 5 s on consecutive failures.

### M3. `policy_snapshot_lookup_rule` clones a rule for every right-click matched lookup
`crates/tray/src/main.rs:544-551` — minor; called on user interaction only. **Polish**.

### M4. `version_info_cache` is documented as never-evicting
`crates/engine/src/lib.rs:128`. Comment claims ~200 entries × ~150 bytes (~30 KB). Reality on a busy box that launches new binaries (Cargo target dirs, installer .tmp paths, scratch scripts) over weeks can climb higher — and the key is the full UTF-16-decoded path string, not a hash. **Severity: Polish**. Mitigation: cap at 1024 entries with LRU eviction (process paths don't repeat indefinitely).

### M5. `iter_pids` returns `Vec<u32>` allocated fresh on every caller
`crates/sys/src/inner/process.rs:106`. Called 3× per tick path (probalance + bg scan + set_affinity_rule). Pre-sized to 256 (good) but the Vec is dropped after the loop. **Severity: Polish**. Mitigation: thread-local reusable buffer, or shift to the consolidated NT call which gives you everything in one allocation.

### M6. `applied_count`/`new_marks` Vec allocated per `set_affinity_rule` apply-to-live
`crates/engine/src/lib.rs:484-485` — rare-path (user clicks "Apply now"), not a footprint issue. **Polish**.

### M7. Tray's `MAX_RECENT = 1000` ring is drained via `drain(0..n)` not `pop_front`
`crates/tray/src/main.rs:5893-5896` — `Vec::drain(0..n)` shifts the tail every overflow. Use `VecDeque`. **Polish**.

### M8. `set_tooltip` on the tray icon is correctly gated by string-equality
`crates/tray/src/main.rs:716-720` — good. Note positive.

### M9. egui repaint floor of 2 s is fine; processes poller is window-visible gated; menu/click threads only wake on event
`crates/tray/src/main.rs:612` + `5971` + `5982-5986`. **Acknowledged good** — the tray's hidden-window cost is minimal *except for the foreground reporter*.

### M10. `processes_poll_loop` reuses one status-pipe connection for both `ListProcesses` and `Status`
`crates/tray/src/main.rs:6011-6037`. Good — one ACL check, two requests. Note positive.

### M11. The status pipe accept loop pre-creates the next instance before the current one connects
`crates/service/src/runtime.rs:240-258` — closes the race window. Comment correctly identifies the symptom this fixes (the foreground reporter's 250 ms cadence used to hit the gap). Note positive.

## What's already good
RAII handle discipline is consistently correct — every `OpenProcess` / `OpenThread` / `CreateToolhelp32Snapshot` is paired with a `CloseHandle` on both success and error paths (`crates/sys/src/inner/process.rs:147`, `:199`, `:356`, `:422`, `:652`; `crates/sys/src/inner/apply.rs:152`, `:203`, `:280`, `:313`, `:331`, `:348`, `:377`, `:392`, `:642`, `:653`). Cache eviction is thoughtful where it matters — `user_cache` is pruned to live PIDs every `list_process_snapshots` call (engine.rs:753-755), `affinity_rule_applied` and `probalance_prev_samples` are pruned per-sweep, `recent` is capped at 1000. There are **no `timeBeginPeriod` calls anywhere** in the codebase — no system-wide timer-resolution abuse. The egui idle floor is properly throttled (2 s repaint, hidden-window guards on the high-frequency poller), the file watcher is event-driven via `notify` (ReadDirectoryChangesW underneath), and tick interval constants are extracted at the top of `engine/src/lib.rs` so they're easy to tune (BACKGROUND_SCAN_INTERVAL=10s, PERSISTENT_REASSERT_INTERVAL=2s, PROBALANCE_SAMPLE_INTERVAL=1s). The service's "prefer tray's foreground report over session-0 poll" plumbing (engine.rs:1096-1104) is the right architectural call for cross-session correctness *and* footprint.
