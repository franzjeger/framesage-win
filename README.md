# framesage-win

A better-than-Process-Lasso scheduler supervisor for Windows. Watches the foreground app and applies per-process scheduling policy through documented user-mode Win32 APIs — anti-cheat-clean by construction.

> Companion to [process-lasso-linux-rs](https://github.com/franzjeger/process-lasso-linux-rs). The Linux tool can do something Windows architecturally won't permit (truly remove a CPU from the scheduler at runtime). This Windows tool does the next-best thing legally, and tries to do it smarter than the incumbents.

## Status: v0.1 scaffold

The repo currently contains a working architecture and a first vertical slice. It compiles on Windows; on other hosts only `framesage-core`, `framesage-ipc`, and stubs build. Not yet runtime-tested end-to-end on Windows hardware — see [Roadmap](#roadmap).

## What it does (today)

- A Windows service (`framesage-svc.exe`) running as LocalSystem watches the foreground window every 300 ms.
- When the foreground process changes, it applies a profile that overrides:
  - **CPU Sets** (soft affinity — scheduler hint, no starvation)
  - **Power Throttling** (Eco / Performance / SystemDefault)
  - **Priority class**
  - **Memory priority**
  - **Hard affinity mask** (fallback, rarely needed)
  - **Working-set trim** on apply
- When focus moves away, it reverts the changes it made to that PID.
- Default policy ships with rules for Battlefield 6, Valorant, and Fortnite that target the AMD X3D (or Intel P-core) CCD via `CpuSelector::Kind(CoreKind::Cache)`.

## What it doesn't do yet

See [Roadmap](#roadmap). Short list: ETW consumption, PresentMon integration, DPC latency attribution, CPPC perf-rank reads, auto-profile learning, true system tray icon.

## Anti-cheat policy

Everything framesage does is documented user-mode Win32. No kernel driver, no process memory reads/writes, no DLL injection, no Nt-prefix syscall games. The APIs used here are the same ones Process Lasso, Razer Cortex, and Xbox Game Bar use — Vanguard, EAC, Javelin, and Ricochet all coexist with them. Anything that would put us into BYOVD / driver-hack territory is out of scope, permanently.

## Architecture

```
┌──────────────────────────────────────────────────────────┐
│                    framesage-svc.exe                     │
│  (Windows service, LocalSystem, owns the policy engine)  │
│                                                          │
│   ┌─────────────┐    ┌───────────────────────────────┐   │
│   │   engine    │◀──▶│      framesage-sys            │   │
│   │ (tick loop, │    │  (Win32: foreground, apply,   │   │
│   │  reconcile) │    │   topology, process enum)     │   │
│   └─────────────┘    └───────────────────────────────┘   │
│         ▲                                                │
│         │  named pipe (\\.\pipe\framesage)               │
└─────────┼────────────────────────────────────────────────┘
          │
   ┌──────┴──────┐         ┌───────────────────┐
   │ framesage   │         │  framesage-tray   │
   │   .exe      │         │  (egui monitor)   │
   │  (CLI)      │         └───────────────────┘
   └─────────────┘
```

Crate layout:

| Crate | Purpose |
|---|---|
| [`framesage-core`](crates/core) | Domain types: `Profile`, `Policy`, `CpuTopology`, `CpuSelector`. Platform-agnostic. |
| [`framesage-sys`](crates/sys) | Win32 wrappers. `cfg(windows)`-only; stubs elsewhere. |
| [`framesage-ipc`](crates/ipc) | Wire types and pipe name for service↔client RPC. |
| [`framesage-engine`](crates/engine) | Reconciliation loop, apply/revert bookkeeping. |
| [`framesage-service`](crates/service) | `framesage-svc.exe` — SCM-hosted service binary. |
| [`framesage-cli`](crates/cli) | `framesage.exe` — install/uninstall, status, control. |
| [`framesage-tray`](crates/tray) | `framesage-tray.exe` — egui monitoring window. |

## Building

### On Windows

```pwsh
# Once
rustup default stable
rustup target add x86_64-pc-windows-msvc

# Build everything
cargo build --release
```

The default build target is `x86_64-pc-windows-msvc` (set in `.cargo/config.toml`).

### From macOS / Linux (cross-compile check)

You can `cargo check` the platform-agnostic crates directly:

```sh
cargo check -p framesage-core --target $(rustc -vV | sed -n 's/host: //p')
cargo check -p framesage-ipc  --target $(rustc -vV | sed -n 's/host: //p')
```

For a full Windows cross-compile, the recommended path is [`cargo-xwin`](https://github.com/rust-cross/cargo-xwin) which downloads the Windows SDK on demand:

```sh
cargo install cargo-xwin
cargo xwin build --release --target x86_64-pc-windows-msvc
```

Alternatively the GNU target works with `brew install mingw-w64` (macOS) — comment out the default target in `.cargo/config.toml` and pass `--target x86_64-pc-windows-gnu` explicitly.

## Installing the service (on a Windows machine)

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

## Running in console mode (for development)

The service binary can run as a foreground console process for development, bypassing the SCM:

```pwsh
.\framesage-svc.exe --console
```

Ctrl+C stops it.

## Roadmap

### v0.2

- [ ] **CPPC perf-rank readout** via `CallNtPowerInformation` → `PROCESSOR_POWER_INFORMATION`. Lets `CpuSelector::TopRanked(N)` actually pin to the fastest silicon on this specific chip.
- [ ] **X3D/Cache CCD detection.** Use the CPPC rank distribution: on dual-CCD X3D parts the X3D CCD has lower max frequency.
- [ ] **Background process enforcement.** Walk `CreateToolhelp32Snapshot` once per N seconds, apply `background_profile` to processes that don't match any rule.
- [ ] **Policy file at `%ProgramData%\framesage\policy.json`**, hot-reloaded by the service.
- [ ] **True tray icon** via the `tray-icon` crate, minimise-to-tray, autostart-tray.
- [ ] **I/O priority** via `NtSetInformationProcess(ProcessIoPriority, …)`.
- [ ] **Pipe ACL** — split read-only status access (for an unprivileged tray) from admin-only control access.

### v0.3 — the differentiators

- [ ] **ETW kernel consumer.** Read context-switch, DPC/ISR, hard-fault, and disk-queue events in real time. The engine becomes closed-loop instead of static-rule.
- [ ] **PresentMon integration.** Per-frame timing via DXGI ETW → frame-stutter detection → targeted countermeasure.
- [ ] **DPC latency attribution.** Identify the offending driver by name, cross-reference a community database, suggest specific roll-forward/back versions. The single most-requested gaming-tweak feature that nobody productises.
- [ ] **Auto-profile learning.** Over the first N sessions of a game, learn working-set size, thread scaling, NUMA sensitivity, frame-pacing fragility. Generate a per-game profile automatically; user can override.
- [ ] **Storage / Network QoS tagging** for the foreground process.
- [ ] **Telemetry / Indexer / Defender-scan gating** during gameplay sessions.

### Future / maybe

- [ ] **"Pure X3D boot mode"** via `bcdedit /set numproc N` + a dedicated boot entry. The actually-invisible-CCD option for benchmark sessions; trades a reboot for a real topology change. Anti-cheat-safe (boot config, not runtime hook).

## Why not just use Process Lasso / Xbox Game Bar / AMD's 3D V-Cache Optimizer?

- **Process Lasso** fires static rules and never measures what those rules cost or save. It uses hard affinity masks where soft CPU Sets are now the correct primitive.
- **Xbox Game Bar / Game Mode** has exactly one bit of context ("a game is open"). It can't reason about the actual cause of a stutter.
- **AMD 3D V-Cache Optimizer** parks the non-X3D CCD on a single signal from Xbox Game Bar. It doesn't read CPPC ranks per silicon, doesn't expose its policy, can't be authored against, and doesn't try to address non-CCD stutter sources at all.

None of them are doing observability-driven scheduling, which is the v0.3 angle of this project.

## License

Dual-licensed under Apache-2.0 OR MIT, at your option.
