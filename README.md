# FrameSage

A better-than-Process-Lasso scheduler supervisor for Windows. Watches the foreground app and applies per-process scheduling policy through documented user-mode Win32 APIs — anti-cheat-clean by construction.

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

### Running in console mode (for development)

The service binary can run as a foreground console process for development, bypassing the SCM:

```pwsh
.\framesage-svc.exe --console
```

Ctrl+C stops it.

## Building

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

## Why not just use Process Lasso / Xbox Game Bar / AMD's 3D V-Cache Optimizer?

- **Process Lasso** fires static rules and never measures what those rules cost or save. It uses hard affinity masks where soft CPU Sets are now the correct primitive.
- **Xbox Game Bar / Game Mode** has exactly one bit of context ("a game is open"). It can't reason about the actual cause of a stutter.
- **AMD 3D V-Cache Optimizer** parks the non-X3D CCD on a single signal from Xbox Game Bar. It doesn't read CPPC ranks per silicon, doesn't expose its policy, can't be authored against, and doesn't try to address non-CCD stutter sources at all.

None of them are doing observability-driven scheduling, which is the v0.3 angle of this project.

## License

Dual-licensed under Apache-2.0 OR MIT, at your option.
