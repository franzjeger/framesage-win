# FrameSage

A scheduler supervisor for Windows. Watches the foreground app and applies per-process scheduling policy through documented user-mode Win32 APIs — anti-cheat-clean by construction.

## Project scope

FrameSage is a personal power-user tool. It is not a broadly distributed product. The expected distribution model is self-install from source via `install.ps1` — there is no signed binary, no MSI installer, no marketing. Other power-users who install are welcome on the same informed-consent basis as the maintainer: install means accepting the risks the architecture and this README enumerate. Full scope rationale + decision history: see `audit/v0.7-architecture.md` §"Project scope and audience (2026-05-18 revision)".

## Who is this for? (read first)

**FrameSage is for users who want maximum performance during games or focused work sessions.** It will stop background services (Windows Update, Search, telemetry, OEM updaters, cloud sync) and suspend non-essential processes (OneDrive, Dropbox, GameBar, Widgets, RGB tools) during a session. Everything is reversed when the session ends. Every action is journaled and reviewable after the fact.

**If you'd rather a gentle optimizer, this isn't the right tool.** Process Lasso's ProBalance-only mode or Windows' built-in Game Mode are better fits for that.

FrameSage's contract:

- **Aggressive by design.** Default profiles do not tiptoe around BITS, WSearch, or cloud sync. If you'd be unhappy with `taskkill /f /im OneDrive.exe` once an hour during a 2-hour gaming session, this isn't the tool for you.
- **Transparent about every action.** Every service stop, process suspend, power-plan change, and Game Mode entry/exit lands in `%LOCALAPPDATA%\FrameSage\activity.jsonl` with a timestamp. Status tab → Recent activity surfaces the same data live.
- **Fully reversible.** Every action has a documented revert path. A crash-safe journal at `%ProgramData%\framesage\game-mode.journal` recovers stranded sessions on service restart. The panic button (`framesage game-mode off`, or right-click → Exit Game Mode) reverts everything immediately.

You opt in eyes-open via the first-run choice (Aggressive / Balanced / Pinning-only).

## What gets stopped / suspended (full disclosure)

When you launch a game with the default **Aggressive** profile, FrameSage will:

**Stop these services for the duration of the session:**
SysMain, WSearch, DiagTrack, BITS, DoSvc, WaaSMedicSvc, UsoSvc, WpnService, CDPSvc, DPS, WdiServiceHost, WdiSystemHost, WerSvc, PcaSvc, dmwappushservice, ClickToRunSvc, SDRSVC, defragsvc, MapsBroker, AJRouter, WMPNetworkSvc, Fax, RetailDemo, PhoneSvc, RemoteRegistry, icssvc, TrkWks, stisvc.

**Suspend these processes for the duration of the session:**
OneDrive.exe, FileCoAuth.exe, Dropbox.exe, googledrivesync.exe, GoogleDriveFS.exe, pCloud.exe, MEGAsync.exe, OneDriveStandaloneUpdater.exe, GoogleUpdate.exe, MicrosoftEdgeUpdate.exe, lghub_updater.exe, AdobeARM.exe, GameBar.exe, GameBarFTServer.exe, GameBarPresenceWriter.exe, WidgetService.exe, Widgets.exe, YourPhone.exe, PhoneExperienceHost.exe, NVIDIA Web Helper.exe, DellSupportAssistRemedyService.exe, HPSupportSolutionsFrameworkService.exe, HpToastSourceApp.exe, LenovoVantageService.exe.

**Also:**
- Switch the Windows power plan to **High Performance** (or **Ultimate Performance** if available).
- **Hide the taskbar.**
- **Pause Windows Update** for the duration of the session.
- **Pin the game** to the AMD X3D / Cache CCD (or top-ranked cores on non-X3D parts).
- **Bump priority + I/O priority** for the game process.

Everything above is reversed automatically when you exit the game. The journal records each action with timestamps; the Status tab → Recent Sessions card aggregates per-session counts. If FrameSage crashes mid-session, the orphan-journal recovery on next service start reverts everything before doing anything else.

**Cannot be touched (kernel-critical / security):** the bundled denylist is non-overridable and refuses to modify csrss, lsass, wininit, smss, services, dwm, explorer, audiodg, MsMpEng (Defender), NisSrv, SecurityHealthService, vgc (Vanguard), BEService (BattlEye), EasyAntiCheat.exe, FACEIT_AC, ESEAClient, vgk.sys (Vanguard kernel driver), BEDaisy.sys, GPU vendor services (NVIDIA / AMD / Intel), the RPC subsystem (rpcss), DNS resolver (dnscache), and the audio stack. These are protected from suspend, priority changes, affinity changes, and working-set trim regardless of profile content.

> Companion to [process-lasso-linux-rs](https://github.com/franzjeger/process-lasso-linux-rs). The Linux tool can do something Windows architecturally won't permit (truly remove a CPU from the scheduler at runtime). This Windows tool does the next-best thing legally, and tries to do it smarter than the incumbents.

## Status

Runtime-tested on real Windows hardware. Ships as a Windows service (`framesage-svc.exe`) + a polished egui tray UI (`framesage-tray.exe`) + a CLI (`framesage.exe`) + a cross-platform dev harness (`framesage-sim.exe`).

The Processes tab is at feature parity with Process Lasso's main process list: live table with state-coloured row gutter, shell icons, sortable Description / Company / User / PID / CPU / Memory / Threads / Priority / Affinity / Profile / Status columns, parent-child tree view with expand/collapse, click-to-open detail card with resizable splitter, hover tooltips on Affinity (decoded CPU list), CPU% (top-5 cores), and Memory (working-set / peak / private). The perf band shows live aggregate CPU% / RAM% plus a per-logical-CPU bar matrix and a sliding 60-second sparkline.

## What it does

### Foreground reconcile loop
A Windows service (`framesage-svc.exe`) running as LocalSystem watches the foreground window every 300 ms. When the foreground process changes, it applies a profile and reverts on focus change. Per-process knobs:

- **CPU Sets** (soft affinity — scheduler hint, no starvation)
- **Power Throttling** (Eco / Performance / SystemDefault)
- **Priority class**
- **Memory priority**
- **I/O priority** via `NtSetInformationProcess(ProcessIoPriority, …)`
- **Hard affinity mask** (fallback, rarely needed)
- **Working-set trim** on apply

### Game Mode (system-level)
A profile can additionally request **Game Mode**: system-wide actions that go beyond a single process.

- **Hide taskbar** (primary + multi-monitor secondaries via documented `ShowWindow`)
- **Stop services** from a curated safe-list (SysMain, WSearch, DiagTrack, …). Anti-virus and anti-cheat services are explicitly denied.
- **Suspend background processes** from a curated safe-list (OneDrive, Dropbox, RGB tools, …). Shell and kernel processes are explicitly denied.
- **Switch power plan** (Balanced / High Performance / Power Saver / Ultimate Performance / custom GUID)
- **Pause Windows Update** (writes the same `HKLM\SOFTWARE\Microsoft\WindowsUpdate\UX\Settings\PauseUpdates*` keys the Settings app uses)

A **crash-safe journal** at `%ProgramData%\framesage\game-mode.journal` lets the service recover stranded sessions on restart. Panic button: `framesage game-mode off` reverts any active Game Mode session immediately.

### Topology awareness
- **CPPC perf-rank readout** via `CallNtPowerInformation` → `PROCESSOR_POWER_INFORMATION`. Per-CPU `MaxMhz` is folded into `cppc_rank`, which is what `CpuSelector::TopRanked(N)` resolves against.
- **X3D / Cache CCD detection** uses per-CCD L3 cache size as the primary signal (X3D CCD's 96 MB vs non-X3D's 32 MB) with CPPC rank as a fallback for parts that don't expose asymmetric L3.

### ProBalance
Dynamic priority management on CPU contention. When the box is contended, ProBalance demotes background CPU hogs to free headroom for the foreground; restores priorities when contention passes.

### Background process enforcement
The tick loop walks `CreateToolhelp32Snapshot` every 10 s and applies `Policy::background_profile` to every PID that isn't the foreground / our own / on the safe-list denylist.

### Tray UI
Real system-tray icon with right-click menu and minimise-to-tray. The main window has four tabs:

- **Processes** — Process-Lasso-class live process viewer (see [Status](#status))
- **Status** — engine state, active profile, foreground app, recent activity, ProBalance status
- **Rules** — view / add / edit / delete AppRules, persisted via SetPolicy
- **Profiles** — per-profile editor for every knob (CpuSelector, GameModeActions, etc.)

Single-launch elevation: the tray detects whether it's running with the elevated token and disables admin controls when not. Run as user for status-only view; relaunch elevated for control.

### Default policy
Ships rules for Battlefield 6, Valorant, and Fortnite that target the AMD X3D (or Intel P-core) CCD via `CpuSelector::Kind(CoreKind::Cache)` and enable a conservative Game Mode (hide taskbar, stop SysMain/WSearch/DiagTrack, switch to High Performance, suspend OneDrive/Dropbox).

## Anti-cheat policy

Everything FrameSage does is documented user-mode Win32. No kernel driver, no process memory reads/writes, no DLL injection, no Nt-prefix syscall games beyond the small documented set Task Manager / Process Explorer use (`NtSetInformationProcess(ProcessIoPriority)`, `NtQuerySystemInformation(SystemProcessorPerformanceInformation)`). The APIs used here are the same ones Process Lasso, Razer Cortex, and Xbox Game Bar use — Vanguard, EAC, Javelin, and Ricochet all coexist with them. Anything that would put us into BYOVD / driver-hack territory is out of scope, permanently.

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                    framesage-svc.exe                     │
│  (Windows service, LocalSystem, owns the policy engine)  │
│                                                          │
│   ┌─────────────┐    ┌───────────────────────────────┐   │
│   │   engine    │◀──▶│      framesage-sys            │   │
│   │ (tick loop, │    │  (Win32: foreground, apply,   │   │
│   │  reconcile) │    │   topology, process enum,     │   │
│   │  ProBalance)│    │   game-mode, version-info,    │   │
│   └─────────────┘    │   user SIDs, per-CPU times)   │   │
│         ▲            └───────────────────────────────┘   │
│         │  named pipes (admin + status, split ACL)       │
└─────────┼────────────────────────────────────────────────┘
          │
   ┌──────┴──────┐         ┌───────────────────┐
   │ framesage   │         │  framesage-tray   │
   │   .exe      │         │  (egui process    │
   │  (CLI)      │         │   viewer + tray)  │
   └─────────────┘         └───────────────────┘
```

Crate layout:

| Crate | Purpose |
|---|---|
| [`framesage-core`](crates/core) | Domain types: `Profile`, `Policy`, `CpuTopology`, `CpuSelector`, `GameModeActions`. Platform-agnostic. |
| [`framesage-sys`](crates/sys) | Win32 wrappers. `cfg(windows)`-only; stubs elsewhere. Covers foreground, topology, CPU Sets, Power Throttling, I/O priority, process enum, version info, user SIDs, per-CPU performance, memory info, Game Mode actions. |
| [`framesage-ipc`](crates/ipc) | Wire types and pipe names for service↔client RPC (split admin + status pipes). |
| [`framesage-engine`](crates/engine) | Reconciliation loop, apply/revert bookkeeping, ProBalance, background enforcement. |
| [`framesage-gamemode`](crates/gamemode) | Curated safe-list (services + processes), action planner, crash-safe revert journal. Platform-agnostic. |
| [`framesage-service`](crates/service) | `framesage-svc.exe` — SCM-hosted service binary. |
| [`framesage-cli`](crates/cli) | `framesage.exe` — install/uninstall, status, control, topology dump. |
| [`framesage-tray`](crates/tray) | `framesage-tray.exe` — egui process viewer + system tray. |
| [`framesage-sim`](crates/sim) | `framesage-sim.exe` — dev harness; resolves policy + topology against synthetic foreground events on any host. |

## Installing

### Recommended: one-shot installer

```pwsh
# Right-click install.ps1 → "Run with PowerShell" (it self-elevates).
# Or from a normal PowerShell:
powershell -ExecutionPolicy Bypass -File .\install.ps1
```

The installer:
1. Self-elevates via UAC if needed.
2. Kills any running FrameSage tray / console-mode service.
3. Builds release binaries via `cargo build --release`.
4. Copies the binaries to `%LOCALAPPDATA%\Programs\FrameSage\`.
5. Creates Start Menu + Desktop shortcuts.
6. Re-installs the SCM service as LocalSystem (autostart on boot).
7. Launches the tray.

### Manual

```pwsh
# In an elevated PowerShell, in the directory containing the built binaries:
.\framesage.exe install
.\framesage.exe start
.\framesage.exe status
```

To remove:

```pwsh
.\framesage.exe stop
.\framesage.exe uninstall
```

### Complete removal (uninstall + state directories)

`framesage.exe uninstall` removes the SCM service, the binaries, and the Start Menu / Desktop shortcuts. If you also want to delete all state — policy.json, the Game Mode journal, the activity log, and any cached version-info — run these afterwards:

```pwsh
# Service-owned state (requires elevation):
Remove-Item -Recurse -Force "$env:ProgramData\framesage"

# Per-user state (no elevation needed):
Remove-Item -Recurse -Force "$env:LOCALAPPDATA\FrameSage"
Remove-Item -Recurse -Force "$env:LOCALAPPDATA\Programs\FrameSage"
```

If the uninstall ran while Game Mode was active, the uninstaller automatically detects the orphan journal and runs an offline revert before removing the service — your services / processes / power plan / taskbar are restored either way.

### Running in console mode (for development)

The service binary can run as a foreground console process for development, bypassing the SCM:

```pwsh
.\framesage-svc.exe --console
```

Ctrl+C stops it.

## Building

Contributor note: all Win32 syscalls sit behind mock-injectable seam traits (`framesage-sys::SysApi`, `framesage-etw::EtwSysCalls`) so the decision logic tests run on any host. New adapters must follow the same shape — see [docs/syscall-seam-pattern.md](docs/syscall-seam-pattern.md).

**Minimum Rust version: 1.88.** The workspace `rust-version` is pinned to 1.88 and a `msrv` CI job enforces it (`cargo check --locked` on the Windows target). The floor is set by locked deps (`image 0.25.10`, `time 0.3.47`, `pxfm 0.1.29`), not by our own code — don't lower it without re-checking those.

### On Windows

```pwsh
# Once
rustup default stable
rustup target add x86_64-pc-windows-msvc

# Build everything
cargo build --release
```

### From macOS / Linux (cross-compile check)

The platform-agnostic crates run tests on any host:

```sh
cargo test -p framesage-core -p framesage-gamemode -p framesage-ipc -p framesage-sim
```

The dev harness exercises policy + topology end-to-end without needing a Windows machine:

```sh
cargo run -p framesage-sim -- demo
cargo run -p framesage-sim -- match bf6.exe
cargo run -p framesage-sim -- --topology hybrid24 match bf6.exe
cargo run -p framesage-sim -- topology
```

Cross-check the full workspace against the Windows target:

```sh
rustup target add x86_64-pc-windows-gnu
cargo check --workspace --target x86_64-pc-windows-gnu
```

## Power-user workflows

### Manual Global Game Mode from the shell

`framesage game-mode on <profile>` enters Game Mode system-wide regardless of which window has focus, and it stays active until you turn it off. The profile must be marked `manual_global_eligible` in your policy (the default `game-x3d` profile is). Useful for video editing, benchmarking, livestreaming — anything where you want the aggressive profile without focus-gating.

```pwsh
# Enter Manual Global Game Mode with the default aggressive profile:
framesage game-mode on game-x3d

# Exit Manual Global Game Mode only (a focus-driven session, if one is
# running, keeps going):
framesage game-mode off-global

# Panic button — revert EVERYTHING immediately (manual AND focus-driven):
framesage game-mode off
```

Both verbs are idempotent, exit 0 on success, and non-zero with a stderr diagnostic on failure — safe to call from scripts.

### OBS scene scripting

Because the verbs are idempotent, you can bind them to OBS scene transitions (Tools → Scripts, or an Advanced Scene Switcher macro running a command) so Game Mode follows your stream layout:

```pwsh
# Scene "Gaming" activated:
framesage game-mode on game-x3d

# Scene "Just Chatting" / BRB activated:
framesage game-mode off-global
```

The session is journaled with a `manual_global` trigger tag, so the Status tab's Recent Sessions view distinguishes script-driven sessions from focus-driven ones.

### Global hotkey

Press **Ctrl+Alt+G** anywhere to toggle Manual Global Game Mode (same effect as the tray-menu toggle). If another application already owns that combo, the hotkey is disabled and the tray logs it; a custom-binding UI is a later item. The tray menu, Status-tab Quick actions panel, and CLI verbs above all reach the same toggle.

## System requirements (closed-loop measurement)

The static-rule engine (rules, Game Mode, ProBalance) supports Windows 10 22H2 and Windows 11. **Closed-loop measurement** — session recording and the "Did it help?" attribution UI — additionally requires **Windows 11 24H2 (build 26100) or later**. ETW kernel-event schemas are stable on builds we've empirically validated, and v0.7 ships with empirical validation only on Win11 24H2. Older builds may or may not work, and v0.7 won't claim measurement results it can't substantiate.

## Roadmap

### Shipped

- [x] Workspace scaffold, CI (cross-check, native Windows build, clippy, rustfmt)
- [x] Policy file at `%ProgramData%\framesage\policy.json` with hot-reload
- [x] Game Mode (taskbar, services, processes, power plan, Windows Update pause, crash-safe journal, panic button)
- [x] CPPC perf-rank readout + X3D / Cache CCD auto-detection
- [x] I/O priority via `NtSetInformationProcess`
- [x] Background process enforcement (10 s tick, walks ToolHelp)
- [x] Real system tray icon with menu + minimise-to-tray
- [x] Split named-pipe ACL (admin pipe + read-only status pipe)
- [x] ProBalance — dynamic priority management on CPU contention
- [x] Per-logical-CPU performance sampling + perf-band matrix
- [x] Processes-tab UX parity with Process Lasso:
  - Shell icons per row
  - State-coloured row gutter (foreground / managed / ProBalance-restrained)
  - Description + Company + User columns (with version-info / SID caches)
  - Working-set + peak + private memory with hover tooltip
  - Affinity hex column with decoded CPU-list tooltip
  - Parent-child tree view with expand / collapse and sibling-only sort
  - Click-to-open detail card with resizable splitter
- [x] Dev harness (`framesage-sim`) for cross-platform iteration

### v0.3 — the differentiators

- [ ] **ETW kernel consumer.** Read context-switch, DPC/ISR, hard-fault, and disk-queue events in real time. The engine becomes closed-loop instead of static-rule.
- [ ] **PresentMon integration.** Per-frame timing via DXGI ETW → frame-stutter detection → targeted countermeasure.
- [ ] **DPC latency attribution.** Identify the offending driver by name, cross-reference a community database, suggest specific roll-forward/back versions. The single most-requested gaming-tweak feature that nobody productises.
- [ ] **Auto-profile learning.** Over the first N sessions of a game, learn working-set size, thread scaling, NUMA sensitivity, frame-pacing fragility. Generate a per-game profile automatically; user can override.
- [ ] **Per-core history heatmap** in the perf band (60s of per-core load as a stacked time strip).
- [ ] **Storage / Network QoS tagging** for the foreground process.
- [ ] **Telemetry / Indexer / Defender-scan gating** during gameplay sessions.
- [ ] **Column-config menu** in the Processes tab (right-click header → show/hide individual columns).

### Future / maybe

- [ ] **"Pure X3D boot mode"** via `bcdedit /set numproc N` + a dedicated boot entry. The actually-invisible-CCD option for benchmark sessions; trades a reboot for a real topology change. Anti-cheat-safe (boot config, not runtime hook).
- [ ] **MSIX / signed installer.** Distribution polish.

## Known limitations

- **Single processor group only.** Windows splits machines with >64 logical processors into "processor groups" (Threadripper PRO 96-core counts). FrameSage currently flattens group 0; anything beyond is a v0.2 task.
- **Intel hybrid P/E detection is approximate.** We use CPPC perf-rank + L3 cache differential to identify cluster boundaries. A proper `PROCESSOR_RELATIONSHIP::EfficiencyClass` readout (the canonical Intel signal) is a v0.2 task.
- **Re-assert is poll-based, not driver-callback.** The engine re-pushes kernel state for every persistent-pinned PID every ~2 seconds to defeat games that override their own affinity at startup. A driver callback would be lower-jitter but lives outside the anti-cheat-safe envelope this project commits to.
- **Topology hot-plug is reactive, not predictive.** Sleep/resume cycles auto-trigger a topology refresh (so power-plan core parking that landed during sleep is picked up); manual triggers are available via `framesage refresh-topology` for power-plan tweaks that don't fire a resume event.
- **Some Windows builds require unsigned-driver popups for ETW kernel consumers.** v0.3 ETW work depends on this and may require Authenticode signing on the binaries before it ships.

## SmartScreen / Authenticode

The shipped binaries are **not yet Authenticode-signed**. On first launch you'll see a SmartScreen prompt; click "More info" → "Run anyway" to proceed. If you're verifying against a published SHA-256 hash:

```pwsh
Get-FileHash .\framesage.exe -Algorithm SHA256
Get-FileHash .\framesage-svc.exe -Algorithm SHA256
Get-FileHash .\framesage-tray.exe -Algorithm SHA256
```

Compare against the hashes posted in the matching GitHub release notes. Signing is on the roadmap (v0.3 prerequisite for the ETW-consumer work).

## Why not just use Process Lasso / Xbox Game Bar / AMD's 3D V-Cache Optimizer?

- **Process Lasso** fires static rules and never measures what those rules cost or save. It uses hard affinity masks where soft CPU Sets are now the correct primitive.
- **Xbox Game Bar / Game Mode** has exactly one bit of context ("a game is open"). It can't reason about the actual cause of a stutter.
- **AMD 3D V-Cache Optimizer** parks the non-X3D CCD on a single signal from Xbox Game Bar. It doesn't read CPPC ranks per silicon, doesn't expose its policy, can't be authored against, and doesn't try to address non-CCD stutter sources at all.

None of them are doing observability-driven scheduling, which is the v0.3 angle of this project.

## License

Dual-licensed under Apache-2.0 OR MIT, at your option.
