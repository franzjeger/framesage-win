# Architecture

framesage-win is split into nine crates inside a single Cargo workspace.
The layering is one-way and acyclic; item 3.8 of the Phase 3 audit
asked for this graph to be documented so future contributors don't
drift.

This file is the authoritative reference. Each crate's `lib.rs` /
`main.rs` top docstring states its allowed dependencies; PR review
checks the union against the diagram below.

## Dependency graph

```
                      ┌──────────────────┐
                      │       core       │   pure types · no framesage deps
                      └─┬──┬──┬──┬─────┬─┘
                        │  │  │  │     │
              ┌─────────┘  │  │  │     │
              │         ┌──┘  │  │     │
              ▼         ▼     ▼  ▼     ▼
          ┌──────┐  ┌──────┐ ┌──────┐ ┌─────┐
          │ ipc  │  │ game │ │ sim  │ │ sys │   (sim drives gamemode
          └──┬─┬─┘  │ mode │ └──────┘ │     │    + core directly;
             │ │    └─┬──┬─┘          │     │    sys also imports
             │ │      │  │            │     │    gamemode — see note)
             │ │      │  └────────────┤     │
             │ │      │               └─┬───┘
             │ │      │                 │
             │ │      └──────────┬──────┤
             │ └──────────┐      │      │
             └────────────┴──────┴──────┘
                          │
                          ▼
                   ┌────────────┐
                   │   engine   │   orchestrator
                   └─┬──────┬───┘
                     │      │
                     │      └──────┐  (only service depends on engine —
                     ▼             │   cli / tray talk to it via IPC)
              ┌──────────┐         │
              │ service  │         │
              └──────────┘         │
                                   │
              ┌──────┐ ┌──────┐    │
              │ cli  │ │ tray │   ─┘   (both depend on core, ipc, sys —
              └──────┘ └──────┘         not engine)
```

## Crate purposes

| Crate                | Role                                  | Depends on                           |
|----------------------|---------------------------------------|--------------------------------------|
| `framesage-core`     | Pure types (Profile, Policy, etc.)    | — (std + serde only)                 |
| `framesage-ipc`      | Named-pipe protocol types             | core                                 |
| `framesage-gamemode` | Planner + journal + safe-list         | core                                 |
| `framesage-sys`      | Win32 wrappers (cfg(windows))         | core, gamemode                       |
| `framesage-sim`      | Simulator for the gamemode planner    | core, gamemode                       |
| `framesage-engine`   | Policy engine (the orchestrator)      | core, ipc, gamemode, sys             |
| `framesage-etw`      | v0.7 closed-loop ETW kernel-event consumer (cfg(windows)) | — (windows + tokio + parking_lot, no framesage deps) |
| `framesage-recorder` | v0.7.1 session recorder: jsonl schema, retention, honest attribution (§2.3/§2.4) | — (serde + uuid, no framesage deps) |
| `framesage-presentmon` | v0.7.1 PresentMon adapter: CSV parse, 1 Hz aggregation, PRE-L-004 spawn policy (§2.2) | — (no framesage deps) |
| `framesage-service`  | Windows service host (LocalSystem)    | core, engine, etw, gamemode, ipc, sys |
| `framesage-cli`      | `framesage.exe` (status, install, …)  | core, gamemode, ipc, sys             |
| `framesage-tray`     | Tray UI (egui + tray-icon)            | core, gamemode, ipc, sys             |

## Layering invariants

The PR-review checklist enforces these rules. None of them are
verified at build time today — the workspace dep graph is small
enough that a human reviewer catches violations reliably. If the
graph grows, a `cargo metadata` walk in CI would automate it.

1. **`framesage-core` has zero framesage deps.** It's the bottom of
   the stack; any crate may depend on it. Its only external deps are
   serde + std (+ chrono-free local time on Windows via the `windows`
   crate, gated to cfg(windows)).

2. **`framesage-ipc` depends on `framesage-core` only.** The IPC
   layer is a pure protocol crate — `Request`, `Response`, `Event`,
   the pipe names. It must not pull in `framesage-engine` or
   `framesage-sys`; if it did, the tray and CLI would transitively
   drag in the engine (and its Win32 surface) just to send a
   `Request::Status`. Types that the engine wants to ship over IPC
   (e.g. `UndoEntry`) live in `framesage-core` for exactly this
   reason.

3. **`framesage-gamemode` depends on `framesage-core` only.** The
   planner + journal + safe-list are platform-agnostic; the Win32
   implementations of the `SystemStateQuery` trait live in
   `framesage-sys::inner::game_mode`. The dependency direction is
   `sys → gamemode`, NOT the reverse.

4. **`framesage-sys` depends on `framesage-core` and `framesage-gamemode`.**
   The `gamemode` dep is the one inversion in the graph: `sys`
   provides Win32 impls of the `SystemStateQuery` trait that
   `gamemode` defines. This is intentional — the trait must be
   *defined* somewhere both sides can see, and `gamemode` is the
   right layer for the data shapes (`PreviousState`,
   `AppliedActions`, `ServiceStatus`, etc.). The gamemode bridge
   lives in a dedicated submodule (`framesage-sys::inner::game_mode`)
   so the rest of `sys` (the SysApi trait, process enumeration, AC
   detection, apply/revert primitives) stays gamemode-free.

5. **`framesage-engine` depends on core + ipc + gamemode + sys.**
   It's the orchestrator: takes `Policy` (core), drives Win32 calls
   (sys), runs the gamemode planner (gamemode), emits events over
   IPC (ipc). It does NOT depend on the consumer crates
   (service / cli / tray).

6. **`framesage-cli` and `framesage-tray` do NOT depend on
   `framesage-engine`.** They talk to the engine via IPC.
   `framesage-sim` doesn't depend on engine either; it drives the
   gamemode planner directly with synthetic state.

   `framesage-tray` DOES depend on `framesage-gamemode` (item 4.13):
   the profile editor consumes the bundled SafeList to surface
   denylist rationale strings inline as the user types. The
   coupling is read-only of static JSON data — no behavior is
   triggered through the dep — so it doesn't move the trust
   boundary.

7. **Only `framesage-service` depends on `framesage-engine`.** The
   service is the engine's host process. Everything else interacts
   via the named-pipe protocol.

8. **`framesage-etw` is a v0.7-era bottom-of-stack crate with zero
   framesage deps.** It wraps the Windows ETW API surface
   (`StartTraceW`, `OpenTraceW`, `ProcessTrace`, etc.) and exposes
   the closed-loop session lifecycle + supervisor + degradation
   types. Only `framesage-service` depends on it — the service
   host spawns the supervisor + drop-poll tasks per Day 5 wiring.
   The crate is gated `cfg(windows)` and compiles to a stub on
   non-Windows targets so the workspace stays Mac/Linux-buildable
   for cross-target verification.

## Why this shape

- **The IPC barrier is the trust boundary.** The service runs as
  LocalSystem; the tray runs as the user. Forcing all interaction
  through IPC means the tray can't accidentally invoke an engine
  method that bypasses the safe-list (item 1.1) or the AC tier
  filter (item 1.9).

- **The simulator runs on Linux.** `framesage-sim` exercises the
  gamemode planner against synthetic state, which is how the
  planner's tests stay deterministic without needing real Win32
  services to be running. The `sys → gamemode` direction (not
  the reverse) keeps `gamemode` compilable on non-Windows.

- **The CLI is a thin client.** `framesage.exe` exists to do four
  things: install/uninstall the service, send a Request, format the
  Response, exit. Pulling in the engine would balloon its binary
  size for no functional gain.

- **The tray is also a thin client.** Same reasoning. The tray UI
  needs Profile and Policy types (core), the wire format (ipc), and
  one Win32 helper module (sys::foreground) to do the session-0
  workaround. It does not orchestrate anything; the engine does.

## When this changes

Adding a new crate? Pick the layer it belongs in:

- Pure data with serde derives → `framesage-core` (or a new sibling).
- New Win32 wrapper → `framesage-sys`.
- New IPC verb → `framesage-ipc` (request/response) + engine handler.
- New planner mode → `framesage-gamemode`.
- New consumer (different UI, different daemon) → mirror the
  shape of `framesage-cli` or `framesage-tray`.

Adding a new dep between existing crates? Document it here in the
same PR. The graph above is the contract.
