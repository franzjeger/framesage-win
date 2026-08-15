# Release notes

## v0.7.1-dev — 2026-07-30 backlog + Group A/B/C engagement

10 PRs (#153–#162) in one continuous agentic session; workspace
test count rose from ~180 to ~235. All Month-1/Month-2 audit items
buildable off Windows are closed; the Group A/B/C scaffolds for the
closed-loop v0.7.1 release are code-complete pending the Windows
runtime batch.

### For users

- **`framesage closed-loop on|off|status`** — flip closed-loop
  measurement without editing policy.json (M1.14).
- **`framesage sessions list|show <id>`** — inspect recorded
  sessions from the shell, including the honest-attribution verdict.
- **Sessions tab** — list + detail with the §2.4 "Did FrameSage
  help?" attribution panel (asymmetric bands, explicit disabled
  reasons, partial-data opt-in).
- **Session recording** — Game Mode sessions are recorded to
  `%ProgramData%\framesage\sessions\` when closed-loop is enabled
  (strictly opt-in, local-only, 1 GB cap): actions, 1 Hz CPU
  samples, and kernel spike signals.
- **Denylist hardening** — the Vanguard / EAC / BattlEye user-mode
  hosts joined the FACEIT pair on the non-overridable denylist, and
  the affinity-rule paths that previously bypassed the denylist are
  gated (issue #148 audit).
- **README "Power-user workflows"** — OBS scene-scripting examples
  for Manual Global Game Mode.
- Multi-line policy-rejection errors render readably in the tray;
  Subscribe connections are capped per client process.

### Under the hood

- New crates: `framesage-recorder` (§2.3 jsonl schema + retention +
  attribution; five C-005 honesty-contract tests), and
  `framesage-presentmon` (CSV parser, 1 Hz aggregator, PRE-L-004
  spawn policy; PresentMon MIT license bundled in
  THIRD_PARTY_LICENSES.md).
- Group A kernel drain: MSDN-cited win11_24h2_26200 classification
  table + rolling-baseline kernel_signal detector wired through the
  consumer callback and 1 Hz drop-poll.
- One-shot ETW APIs return typed AlreadyStoppedError instead of
  panicking; drop-poll/shutdown race closed with a shared teardown
  flag; RealEtwSysCalls RefUnwindSafe statically asserted.
- Build-gate test override folded to a single seam
  (`test-override` feature); watchdog-exclusion pinned by
  source-level tests; syscall seam pattern documented in
  `docs/syscall-seam-pattern.md`.
- `crates/spike-etw` removed (M3.4) — findings preserved in
  `spike/`.
- MSRV corrected to **1.88** (the declared 1.80 was never
  buildable: locked deps `image 0.25.10`, `time 0.3.47`,
  `pxfm 0.1.29` require it) and enforced by a new `msrv` CI job
  (`cargo check --locked` on the Windows target). `__cpuid` calls
  in `session_recorder.rs` wrapped for the unsafe/safe split across
  toolchains.

### Still gated on Windows hardware / external clocks

Live session recording verification, E-004 negative-session
screenshot, PresentMon/ETW integration runs, %ProgramFiles% ACL
verification (#99), EDR matrix attestation, and the v0.7.1
default-on flip (M3.5 dogfood gate).

---

# Release notes — Phase 3 audit response

This release covers the four-group rollup of audit findings from
`audit/`, shipped as 46 PRs (#16–#61) on the `claude/peaceful-
mayer-4d5448` branch. Every PR landed via small focused commits
with green CI; the workspace test count rose from ~150 to 290+
across the engagement.

## TL;DR for users

If you've been running an earlier framesage build, here's what
changes in this release:

- **First-run wizard.** New installs walk through a 3-way
  consent dialog gating the seeded BF6 / Valorant / Fortnite
  rules. The previous build applied the full Aggressive arsenal
  on the first foreground event with no warning.
- **Settings tab.** Editable ProBalance thresholds, tick
  interval, policy reset, compact-mode toggle.
- **CLI policy verbs.** `framesage policy export <path>`,
  `policy import <path>`, `policy add-rule <exe> <profile>`.
  No more hand-editing `policy.json`.
- **Game Mode editor: full arsenal.** Denylist rationale now
  visible inline. Per-rule AntiCheatProfile selector. Discover-
  services + discover-processes wizards with batch-add. Dry-run
  preview before save.
- **Undo log.** `framesage undo last` reverses the most recent
  priority change, affinity change, suspend, or resume.
- **Topology hot-plug.** Engine re-detects CPU topology on
  sleep/resume + on demand via `framesage refresh-topology`.
  Catches power-plan-driven core parking.
- **Activity log persistence.** Tray now reads
  `%LOCALAPPDATA%\framesage\activity.jsonl` at startup; the
  Status-tab "Session stats (last 24 h)" card aggregates it.
- **WinEvent foreground hook.** Replaced the 250 ms foreground
  poll with `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)` — idle
  wake-up rate drops from ~4/sec to ~0/sec.
- **Sleep / resume + session change** handled via WTS + power
  notifications. Game Mode auto-exits on suspend, resumes on
  wake.
- **Manual Global Game Mode.** `framesage game-mode on
  <profile>` enters a system-wide session decoupled from the
  foreground. Useful for focused-work blocks.

## Group 1 — Must-fix (PRs #16–#24, 9 items)

Safety-critical and consent-related work. Closes the audit's
must-fix items.

| # | Closes | Summary |
|---|---|---|
| 1.1 | H-01, H-02 | Bundled non-overridable denylist for kernel-critical / AV / anti-cheat / GPU / RPC / DNS / audio processes |
| 1.2 | H-03 | Atomic ToolHelp snapshot with retry on race against process spawn / exit |
| 1.3 | H-05 | Bounded retry on stop_service so a stuck-Stopping service can't hang the engine |
| 1.4 | C-07 | `%LOCALAPPDATA%\framesage\sessions.jsonl` — permanent record of every Game Mode session for post-mortem auditing |
| 1.5 | H-25 | Real uninstaller — strips service, binaries, shortcuts; offers complete state cleanup |
| 1.6 | H-26 | Authenticode-friendly install path + Get-FileHash verification recipe |
| 1.7 | H-09 | `Kind(Cache)` selector falls back to `TopRanked(8)` on non-X3D hardware instead of silently clearing affinity |
| 1.8 | H-08 | Atomic per-thread CPU set application — closes the soft-hint gap on games with long-lived worker threadpools |
| 1.9 | H-12, M-12 | AC-aware tier (`Aggressive` / `Hybrid` / `SafeMode` / `Disabled`) on every profile + ESEA auto-standby |

## Group 2 — High-leverage (PRs #25–#33, 11 items)

OS-integration + observability surface that wasn't critical but
was load-bearing for user trust.

| # | Closes | Summary |
|---|---|---|
| 2.1 | H-06 | One-syscall process enumeration via NtQuerySystemInformation (saves N×OpenProcess per second) |
| 2.2 | H-07 | WinEvent foreground hook replaces the 250 ms poll — idle wake-ups drop from ~4/sec to ~0/sec |
| 2.3 | H-04 | `Arc<CpuTopology>` instead of `Vec<LogicalCpu>` — clones become refcount bumps |
| 2.4 | M-02 | Sleep / resume + session change handled via WTS + power notifications |
| 2.5 | H-10 | Per-PID affinity rules — pin Steam so games inherit the X3D CCD at spawn |
| 2.6 | H-13 | Foreground-report staleness fallback so a crashed tray doesn't strand Game Mode |
| 2.7 | H-14 | Multi-monitor taskbar hide (primary + secondaries) |
| 2.8 | H-28 | Engine emits the full Event enum (GameModeEntered / Exited, ProfileApplied / Reverted, AffinityRuleFired, ActionFailed, AntiCheatPresenceChanged) |
| 2.9 | M-03 | `activity.jsonl` persisted; Status-tab Recent activity hydrates from disk at startup |
| 2.10 | H-24 | Status tab as landing page (was: data-dense Processes table) |
| 2.11 | M-21 | Manual Global Game Mode + CLI verb + tray banner |

## Group 3 — Structural (PRs #34–#46, 8 items)

Internal-quality work that doesn't move user-visible surface but
makes future work cheaper.

| # | PR | Summary |
|---|---|---|
| 3.1 | #34, #35 | `SysApi` trait + `Clock` abstraction — engine tests run without real syscalls or wall clock |
| 3.2 | #41 | Tray migrated to `parking_lot::Mutex` — 31 `.lock().unwrap()` callsites collapsed to `.lock()` |
| 3.3 | #45 | `OwnedHandle` RAII wrapper — every manual `unsafe { CloseHandle }` in `framesage-sys::inner` replaced with Drop-driven release |
| 3.4 | #44 | Per-PID CPU sparkline in Processes-tab detail pane |
| 3.5 | #42 | Undo log + `framesage undo last` / `undo list` CLI verbs |
| 3.6 | #36–#40 | Tray module extractions (5 slices) — `main.rs` 6735 → 4242 lines (−37%) |
| 3.7 | #46 | Topology hot-plug — auto-refresh on `SystemEvent::Resume` + manual `framesage refresh-topology` verb |
| 3.8 | #43 | `ARCHITECTURE.md` + automated layering test via `cargo metadata` |

## Group 4 — Polish (PRs #47–#61, 15 items)

QoL, correctness corners, the marquee editor surface.

| # | PR | Closes | Summary |
|---|---|---|---|
| 4.1 | #55 | H-24 | First-run onboarding wizard with 3-way informed consent |
| 4.2 | #53, #57 | H-33, H-34, L-15 | Processes-tab UX (broader filter, Ctrl-F, Esc clear, no width cap, compact mode) |
| 4.3 | #56, #57 | M-23, M-24, M-25 | CLI policy verbs (`export` / `import` / `add-rule`), persistent toggle in profile editor, Settings tab with editable ProBalance thresholds + tick interval + Reset to defaults |
| 4.4 | #51 | M-01, M-02, M-27 | Affinity-mask sanitization (intersect with system_mask before `SetProcessAffinityMask`) + zero-mask refusal at apply time |
| 4.5 | #54 | M-04 | MsMpEng trim-protection verification test (already protected via item 1.1's denylist) |
| 4.6 | #47 | M-18 | ProBalance restrain-side hysteresis — N consecutive samples before demote (default 2 = ~600 ms at the default tick) |
| 4.7 | #50 | M-19 | Revert-state-drift detection — skip revert if user changed priority/affinity via Task Manager |
| 4.8 | #52 | M-08 | OpenProcess error classification — surface Unexpected errors instead of silently mapping them to `Ok(None)` |
| 4.9 | #49 | M-15 | Apply-failure backoff — 30 s per-PID after a failed apply, kills log spam on protected processes |
| 4.10 | #48 | M-16 | Game Mode crash-recovery exe re-check — never resume a reassigned PID |
| 4.11 | #51 | M-17, M-26 | Save-time policy validation — dangling rule refs, out-of-range CCD selectors, `Mask(0)` all refused at SetPolicy time |
| 4.12 | #53 | L-15 | Esc dismisses modals (terminate-confirm, affinity-picker, reset-confirm, preview) |
| 4.13 | #58, #59, #60, #61 | L-21, L-22, M-35 | **GM editor full arsenal**: denylist-aware list editors with inline rationale, per-rule `AntiCheatProfile` selector, discover-processes wizard, discover-services wizard, preview-before-save modal |
| 4.14 | #54 | M-33 | README rewrite — product positioning, full disclosure list, known limitations, complete-removal recipe |
| 4.15 | #53 | M-22, H-31 | `matched_rule_index` on `ForegroundChanged`, Session-stats card on Status tab |

## Test count

- framesage-core: 54 (+ structural validators + AggressionLevel
  tests)
- framesage-engine: 53 (+ ProBalance hysteresis, drift detection,
  apply backoff, topology refresh, item 4.5 trim-protect verifier)
- framesage-gamemode: 37
- framesage-sys: 31 (+ OwnedHandle, classify_open_process_error,
  services enumeration)
- framesage-tray: 37 (+ SessionStats, onboarding aggression
  predicates)
- framesage-ipc: 9 (+ ListServices read-only assertion)
- Plus existing: framesage-sim 28, framesage-cli 3, etc.

**~290+ workspace tests, all green.**

## Non-code milestones

- **M-A (BattlEye outreach)** — pending. Prerequisite for v0.7's
  BE-game seeded rules. Send an email describing framesage's
  user-mode Win32 API surface and asking about compatibility
  with BE-protected games. Draft available on request.

## Architectural notes shipped

- `ARCHITECTURE.md` documents the dep graph + 7 invariants +
  the one intentional inversion (`sys → gamemode` for Win32 impls
  of `SystemStateQuery`).
- `SysApi` trait abstracts the syscall surface — engine tests
  run without real Win32 calls.
- `OwnedHandle` RAII wraps every `HANDLE` in `framesage-sys::
  inner`; compiler enforces `CloseHandle` on every return path.
- Policy structural validation runs at SetPolicy time; ill-formed
  policies are refused at the boundary with concatenated errors.

## What's next

- **v0.7 prerequisites:**
  - BattlEye outreach (M-A)
  - Authenticode signing
  - BE-game seeded rules (BF6, R6 Siege, Apex Legends)
- **v0.7 stretch:** discover-services CPU sampling, per-service
  start_type, hotkey-binding UI for Manual Global Game Mode.
- **v0.8 (the differentiators):** ETW kernel consumer, PresentMon
  integration, DPC latency attribution, auto-profile learning.
  See README.md → Roadmap → v0.3.

---

🤖 Engagement executed by Claude Code on
`claude/peaceful-mayer-4d5448`. 46 PRs, 43 audit items shipped,
~290 tests, ~5 weeks of work compressed into a continuous
agentic session.
