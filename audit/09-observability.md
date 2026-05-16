# Observability Audit — peaceful-mayer-4d5448

**Question:** Can a user see what FrameSage did in the last hour / day / week, and trust it was helpful?
**Answer:** **No.** The product has a live "what's happening right now" view but essentially **zero retrospective audit trail**. Every observability surface evaporates on tray restart, has no on-disk persistence beyond a single Game Mode revert journal, and silently buries half of the actions FrameSage actually performs.

Severity is High across the board because trust = observability in this product category, and a user enabling `game-x3d` for a 2-hour session today has no way to confirm tomorrow that anything sensible happened.

---

## 1. The IPC `Event` enum is dramatically under-populated

`crates/ipc/src/lib.rs:338-364` — the entire surface area the tray ever learns about:

- `ForegroundChanged { foreground, profile }`
- `Paused`
- `Resumed`
- `ProBalanceRestrained { pid, exe_name, from_class, to_class }`
- `ProBalanceRestored { pid, exe_name, restored_class }`

That's **five variants**. Cross-referenced with `self.events.send(...)` call sites in `crates/engine/src/lib.rs:269, 286, 925, 1040, 1138, 1309, 1342, 1805, 1827` — the engine emits exactly those five.

**Things the engine does but never tells the tray about** (severity High):
- Profile applied / reverted to a non-foreground PID via background scan (`apply_profile` in reconcile / `revert_record` lines 904, 1008, 1732, 1790). User sees "Diablo IV → game-x3d" but never sees the per-PID priority / affinity / I/O writes that actually went through.
- Game Mode entered / exited (`reconcile_system_mode_locked` lines 932, 1048; `revert_system_mode_locked` line 1983). The single biggest, scariest action the app takes — hides taskbar, stops services, suspends processes, switches power plan — fires zero events. The user only sees the antecedent `ForegroundChanged`.
- Per-service stop / restore (`AppliedActions.stopped_services` — `crates/gamemode/src/state.rs:86`). 30 services may have been stopped; the tray never hears about any of them.
- Per-process suspend / resume during Game Mode (`AppliedActions.suspended_pids` — line 87).
- Power plan switch (`switched_power_plan` — line 88).
- Focus assist / Windows Update pause (lines 89-90).
- Affinity rule fired against a fresh PID (`set_affinity_rule` line 458, persistent re-assert loop lines 1453-1506).
- Affinity rule rule created / deleted (`delete_affinity_rule` line 532).
- Manual override set / cleared.
- One-shot `SetProcessPriority` / `SuspendProcess` / `TerminateProcess` / `TrimWorkingSet` — every right-click action the user themselves takes from the Processes tab. The user *just clicked Terminate on PID 1234* and gets no event echoed back; only the bottom-of-window `last_action` banner confirms it (severity Medium since the user initiated it).
- Apply / revert **failures** — see §7.
- Orphan-journal crash recovery — see §9.
- Service / engine start / stop.
- Policy reload (after `SetPolicy`).

The five-variant enum captures roughly **20%** of what FrameSage actually does to the system.

## 2. History is purely in-memory and tiny

`crates/tray/src/main.rs:5892` — `const MAX_RECENT: usize = 1000;` and `s.recent` is a `Vec<RecentEvent>` on `AppState` (line 55). The comment claims "1000 entries is ~5 minutes of constant flicker."

- **No persistence to disk.** Search for `save_recent`, `load_recent`, `action_history`, `history.json` returns zero hits in tray + engine + service.
- **Tray restart = total amnesia.** Close tray, reopen → empty Activity tab. The service keeps running, but events are only delivered live over the `Subscribe` stream (`try_connect_and_serve` line 5842) — there is no replay or backfill of pre-subscribe history. Severity: **High.**
- **Service restart = total amnesia** (no service-side ring either).
- **5-minute horizon at 250 ms flicker.** On a typical multi-monitor desktop with focus bouncing between Chrome, VS Code, Slack the buffer rolls over in single-digit minutes. The "last hour / day / week" question in the prompt is unanswerable.

## 3. Per-process history doesn't exist

`render_process_detail` (`crates/tray/src/main.rs:4143-4341`) shows:
- live CPU / memory / threads / priority / affinity (from the 1 Hz `ListProcesses` poll)
- current managed profile + matched rule note
- current ProBalance restraint status

What it does **not** show:
- "What has FrameSage done to this PID today?" — no history.
- "Has the rule for `Diablo IV.exe` ever fired?" — impossible to answer.
- "When was this process demoted by ProBalance, for how long, and what triggered it?" — not in the panel.

The Activity tab has a substring filter (line 1822) so a user could search `Diablo IV.exe`, but only against the ≤1000 in-memory entries (§2). Severity: **High** — this is the exact "trust me, FrameSage helped" question a power user asks.

## 4. "Why is priority AboveNormal?" is unanswerable from history

For a *currently managed* PID, the detail panel shows `Profile: game-x3d (Rule)` plus `Rule note` (lines 4216-4233). That answers the *current* state question reasonably well — **already done well.**

But "why was it AboveNormal an hour ago, and who changed it back" is **invisible**:
- The `ForegroundChanged` event names the profile id but not the priority delta.
- ProBalance events name from/to priority classes (good) but only for ProBalance demotions, not for profile-driven priority changes.
- No event records the actual kernel writes (`SetPriorityClass`, `SetProcessAffinityMask`, `SetIoPriority`).

## 5. No quantified-impact metrics

Process Lasso's headline value prop is "we did X demotions in the last hour for you." FrameSage emits the events but never aggregates them. Search for `restrain_count`, `demotion_count`, `session_stats`, `impact_summary`: zero hits. The Status tab's ProBalance card (`render_probalance_card` line 1581) shows `Currently restraining: N processes` — point-in-time, not cumulative. Severity: **High** — without a number, "FrameSage helped" is a vibe.

## 6. Game Mode session log is gone the moment it ends

This is the most damaging gap.

`crates/gamemode/src/journal.rs` writes a `JournalEntry` with full `AppliedActions { hid_taskbar, stopped_services, suspended_pids, switched_power_plan, set_focus_assist, paused_windows_update }` to `%ProgramData%\framesage\game-mode.journal` while a session is live (`Journal::write` line 130). On exit, `revert_system_mode_locked` calls `self.journal.delete()` — confirmed by reading `crates/engine/src/lib.rs:1983-1992` flow.

So:
- **During** a Game Mode session: the journal exists on disk, but the tray has no UI surface that reads it. No "currently stopped services" list, no "suspended processes" list, no "power plan switched from X to Y" indicator. The Activity tab and Status tab tell the user nothing about the heavy actions.
- **After** a Game Mode session: the journal is deleted. The 30 services that were stopped, the 24 processes that were suspended, the power plan change, the taskbar hide — **all of it is destroyed**. Tomorrow the user has no record that any of it ever happened.

Severity: **Critical**. A 2-hour game-x3d session that touches 50+ system objects produces zero post-session audit. This is the headline trust failure.

The fix is obvious and small: instead of `journal.delete()` on revert, append the entry to a rolling `sessions.jsonl` log and surface it as a "Game Mode session history" view.

## 7. Errors are buried in tracing logs

`crates/engine/src/lib.rs:1136` — `warn!(pid, error = %e, "probalance: failed to release restraint on disable")`. There are 32 `warn!`/`error!` call sites and **none of them produce a user-visible event**. Failed service stop because the service is protected? Buried. Failed process suspend because PID exited? Buried. Failed power plan switch because GUID not present? Buried.

`AppliedActions` even tracks success per-action (line 84-91), so the data exists — it's just never surfaced past the planner's stdout. Severity: **High** — a silent failure is indistinguishable from a successful action to the user.

## 8. Tracing log is not discoverable

`crates/service/src/main.rs:97-102` — `init_tracing()` uses `tracing_subscriber::fmt()` with `EnvFilter("framesage=info,info")`. **No file appender configured.** Logs go to the service's stdout, which is `\Device\Null` once the SCM runs the service detached. They are not written to a file, not to Windows Event Log, not anywhere a user can ever look. There is no "Open log folder" menu item; search for `Open log` and `EventLog` returns zero matches across the workspace.

In console mode (`--console`) you get logs on a terminal, but that's a dev convenience, not user-visible. Severity: **High** — when something goes wrong there is literally no log to send to support.

## 9. Crash recovery is silent

`recover_orphan_journal` (`crates/engine/src/lib.rs:1060-1080`) reads the orphan, calls `sys_revert_all`, deletes the journal, and `warn!`s the result. No IPC event is emitted (you can grep — there's no `events.send` inside the function). The user, opening the tray after a crash, sees no "FrameSage detected an unclean shutdown and reverted N actions" banner. Severity: **High** — this is exactly the moment trust is most fragile.

## 10. Persistent rule audit is impossible

The user creates an `AffinityRule` for `Diablo IV.exe → X3D CCD`. Three days later they want to know:
- Did this rule fire? On which PIDs? When? How many times?

`set_affinity_rule` (`crates/engine/src/lib.rs:458`) and the persistent re-assert loop (lines 1453-1506) emit no IPC events, write no per-rule counter, and don't even `tracing::info!` per-application. There is no rule-firing telemetry anywhere. Severity: **Medium-High** — undermines the case for using persistent rules vs one-shot pins.

## 11. The `last_action` banner is fleeting and reactive only

`crates/tray/src/main.rs:377` — `last_action: Arc<Mutex<Option<String>>>`. Written by `send_admin_request` (line 530) on completion of user-initiated admin requests, displayed at the bottom of the Status tab (line 4811), Rules tab (line 1997), Profiles area (line 2511), and the Processes context-menu paths (lines 3876, 3986, 3990).

Useful, but limited:
- Only set by user-initiated tray actions, not engine-initiated work.
- Single `Option<String>` — overwritten on every action, no history.
- Disappears on tray restart.
- The error-detection heuristic on line 4821 (`text.contains("error")`) is a string match; a structured error type would be more robust.

Verdict: useful for "did my click go through?" — useless for "what did the engine do?". Severity: **Low** in isolation, **Medium** as a substitute for proper observability.

## 12. Notifications / toasts: none

No `Notification`, `toast`, `MessageBox`, or balloon API call exists in `crates/tray/`. The only `Notification` hits are in `crates/core/src/policy.rs` (which is unrelated `NotificationFlags` config). No earned-attention surface — the user must open the tray window to learn anything. Severity: **Medium** — debatable design choice (Process Lasso's toasts are famously noisy), but at minimum the Game Mode entered/exited and crash-recovery cases warrant a one-time toast.

---

## What's already done well

- **Activity tab structure** (`render_activity_tab` line 1758) — kind chips, substring search, newest-first table, clear-log button. The UI scaffolding is right; the data feeding it is the problem.
- **ProBalance event fidelity** — `from_class` / `to_class` raw values captured at decision time (`crates/ipc/src/lib.rs:351-354`) means the action log can render the demotion factually. Best-in-class for the events that do exist.
- **Process detail panel surfaces rule-note and matched-profile** (lines 4216-4233) — answers "why is this PID managed?" for *current* state cleanly.
- **`last_action` banner** as a confirm-receipt for user-initiated actions — small but valuable.
- **`AppliedActions` already tracks what was applied** — the data model for a real audit log is half-built. Plumbing exists; the events to fire it just aren't wired.
- **The Journal model is crash-safe** (atomic write, schema versioned) — extending it to append-on-revert instead of delete-on-revert is a small change.

---

## What a user CAN see

- The current foreground app's name + the profile FrameSage chose for it.
- Live priority class, affinity mask, CPU%, memory of every process (1 Hz refresh).
- Which processes are currently ProBalance-restrained (point-in-time count).
- Live ProBalance demote/restore events as they happen, up to ~5 minutes back, lost on tray restart.
- The most recent admin action they themselves triggered (one-line banner).
- Whether the engine is connected / paused / in manual override (status hero).

## What a user CANNOT see

- Any history older than the in-memory ring buffer (max ~5 min of churn; zero after tray restart).
- Anything that happened during a now-ended Game Mode session — services stopped, processes suspended, power plan switched, taskbar hidden, all destroyed on revert.
- Per-process action history ("what did FrameSage do to Diablo IV today").
- Whether a persistent affinity rule has ever fired.
- Aggregated impact numbers ("ProBalance demoted 47 processes in the last hour").
- Whether any apply / revert / service-stop failed silently.
- Whether crash recovery ran and what it reverted.
- The location of the service log (there isn't one on disk).
- Profile-driven priority/affinity changes (only ProBalance events carry priority deltas).
- Game Mode entered / exited as a discrete event.

## Top three fixes by ROI

1. **Persist `JournalEntry` to a rolling `sessions.jsonl` on Game Mode revert** instead of `journal.delete()`. Add a "Game Mode history" view in the tray. Single-digit hours of work; eliminates the worst trust gap. (`crates/engine/src/lib.rs:1983`, `crates/gamemode/src/journal.rs:172`).
2. **Add IPC `Event` variants for: `GameModeEntered/Exited` (with `AppliedActions` summary), `ProfileApplied/Reverted`, `AffinityRuleFired`, `ActionFailed { what, error }`.** Stream them, and persist the ring buffer (or all of it) to `%ProgramData%\framesage\activity.jsonl` so tray restart doesn't lose history. (`crates/ipc/src/lib.rs:338`).
3. **Add a `tracing_appender::rolling::daily` file sink to `init_tracing` and an "Open log folder" tray menu item.** Five-line change; gives support a debuggable artifact. (`crates/service/src/main.rs:97`).
