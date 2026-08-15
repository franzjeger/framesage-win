# framesage-win — Code Quality Audit

Scope: ~18.6 k lines across 9 crates. Read-only audit.

Severity legend: **HIGH** = ship-blocker / silent correctness risk · **MED** = will hurt as the codebase grows · **LOW** = polish.

---

## Status re-audit (2026-08-15)

This document is a **point-in-time snapshot** taken before several of its findings were fixed. Verified against the live tree on 2026-08-15:

- **HIGH #1 — testable `Engine` — RESOLVED.** `SysApi` trait + `RealSysApi` landed at `crates/sys/src/api.rs:46`; `EngineDeps` now carries an injectable `Arc<dyn SysApi>` (`crates/engine/src/lib.rs:109`) plus a `Clock` (`crates/engine/src/clock.rs` with a `FakeClock` for tests). Engine test count went from 2 to **62**.
- **HIGH #2 — `parking_lot::Mutex` in tray — RESOLVED.** `parking_lot::Mutex` is imported at `crates/tray/src/main.rs:27` and `crates/tray/src/ipc_client.rs:37`; all lock sites use the infallible API (no `PoisonError` handling, no `.unwrap()`).
- **MED — extract tray modules — RESOLVED.** `ipc_client.rs`, `process_actions.rs`, `state.rs`, `widgets.rs`, `formatters.rs`, `tree.rs`, `theme.rs`, `icons.rs`, `win32.rs` are now separate modules.
- **MED — file-sink logging — RESOLVED.** `crates/service/src/main.rs:221` wires `tracing_appender::rolling::daily` + `non_blocking`.
- **MED — `sys → gamemode` dep arrow — by design, keep.** `SystemStateQuery` lives in `gamemode` (planner.rs:44) so `sys` can `impl SystemStateQuery for Win32StateQuery` (sys/src/inner/game_mode/query.rs). The inversion is documented in `crates/sys/src/lib.rs:10-18` and `ARCHITECTURE.md`; it is *not* a defect.
- **LOW/MED — `tracing` in `core` — RESOLVED in this pass.** The dep was unused in `core` source and has been removed from `crates/core/Cargo.toml`. (The body of §1 still lists it as one of `core`'s deps — that line is now stale.)

The ranked list at the bottom (§ Top-5) reflects the **pre-fix** state; use this block for current status.

---

## 1. Layering — mostly clean, some leaks

**What works well**
- `core` has zero Win32 imports (verified by grepping `windows::` / `winapi::` / `Win32::` — no hits in `crates/core`). It only depends on `serde`, `serde_json`, `thiserror`, `tracing`. Textbook.
- `engine` has zero `windows::` / `eframe` / `egui` imports. It talks to `framesage_sys` exclusively through the re-exported module API. Dependency arrows: `tray → ipc/core/sys`, `engine → core/sys/ipc/gamemode`, `sys → core/gamemode`, `core → (nothing internal)`. No cycles.
- `sys` exposes a non-Windows `stub.rs` and `lib.rs` swaps `inner` ↔ `stub` on `cfg(windows)`. The stub matches the real surface (foreground/topology/apply/process/process_actions/version_info/game_mode), so the CI `cargo check` on `x86_64-pc-windows-gnu` *and* the portable test job on Linux both stay honest.

**Issues**
- **MED — `framesage-sys` depends on `framesage-gamemode`.** `crates/sys/Cargo.toml:30`. That makes sys the lowest layer for everything *except* the gamemode types it must know about. It's not a cycle (gamemode doesn't depend on sys), but it inverts the expected arrow: gamemode is the higher-level planner crate. Today sys re-exports `Win32StateQuery: SystemStateQuery` and `apply_action(&PlannedAction, …)`. Cleaner shape: define the `SystemStateQuery` trait + `PlannedAction` enum in `core` (they're pure data + trait), implement them in `sys`. Then sys depends only on core. Low actual harm; mostly aesthetics.
- **MED — `core` depends on `tracing`.** `crates/core/Cargo.toml:15`. `core` should be data-only. `tracing` is a domain-cross-cutting dep but it pulls a runtime knob into the type crate. Grep shows it's used inside `policy.rs` / `topology.rs` for `warn!` lines — those could move up to `engine` (which already has tracing) or be replaced with returned `Result`s the caller logs.
- **LOW — Engine reaches into `framesage_sys::version_info::VersionInfo` and stores it in `EngineState`** (`crates/engine/src/lib.rs:128`). That's a leak of a sys-shaped type into the engine state struct. Not catastrophic — VersionInfo is data, not a handle — but ideally the engine holds a `core`-defined struct that sys *populates*.

---

## 2. Engine testability — mixed: probalance excellent, reconcile untestable

**`probalance::decide`** — *the gold standard.* `crates/engine/src/probalance.rs:118`. Pure function, takes `now: Instant`, samples, sets-as-input, mutable restraint map. 10 unit tests at lines 280–566 cover: under-threshold no-op, top-hog restraint, foreground skip, managed-pid skip, safe-list skip, user-ignore-list (case-insensitive), AboveNormal refusal, dwell window, restore-after-quiet, restore-on-foreground-takeover, restore-on-exit. Excellent coverage of a state machine that would be a nightmare to test against real processes.

**`policy::match_foreground`** — also great. `crates/core/src/policy.rs:181`. 15 tests at lines 485–693 cover exe-name precedence, path glob, case insensitivity, title regex, profile-id fallback, etc.

**`Engine::tick` / `Engine::reconcile`** — *not unit-testable today.* `crates/engine/src/lib.rs:1083, 1703`. They take `&self` (the real Engine) and call `framesage_sys::*` free functions directly (`apply_profile` on line 1820 wraps `framesage_sys::apply::apply`). Two big blockers:
1. No trait abstraction for the syscall surface — `framesage_sys::process::iter_pids` etc. are called as free functions. A `trait SysApi` would let tests inject a mock.
2. `Engine::new(EngineDeps { … })` doesn't accept a clock — `tick` uses `Instant::now()` directly (line 1150), so time-dependent paths (BACKGROUND_SCAN_INTERVAL, PERSISTENT_REASSERT_INTERVAL, ProBalance throttle) can't be deterministically tested.

The single test in `engine/src/lib.rs:2153` — `applied_from_plan_covers_every_planned_action_variant` — is a smart enum-exhaustiveness lock but doesn't exercise the engine. **There are zero integration tests for `Engine`.**

`gamemode/planner.rs` uses a `SystemStateQuery` trait — the right shape — and could be the model for `sys`.

**Severity — HIGH.** The engine is the brain of the product, and the 2 200-line `lib.rs` has effectively no behavioural test coverage. Every refactor is a hand-test against real Windows.

---

## 3. Tray monolith — 6 241 lines, still has 3 obvious extractions

`crates/tray/src/main.rs` line counts:
- 1–600: state, structs, lifetime wiring, `FramesageApp::new`
- 601–4042: `impl eframe::App for FramesageApp` + `impl FramesageApp` — UI panels for every tab
- 4043–5500: process detail pane, format helpers, render helpers, profile editor, CPU-selector editor, logo
- 5500–5780: tray icon build, menu IDs
- 5780–6105: IPC client loops (background_loop, processes_poll_loop, foreground_reporter_loop, send_request_blocking)
- 6105–end: `main`, shell helpers

Already extracted: `formatters.rs` (329 LOC), `tree.rs` (498), `theme.rs` (226), `icons.rs` (241), `win32.rs` (272).

**What still needs to come out**
- **MED — IPC client into `tray::ipc` module (~325 LOC).** `background_loop`, `try_connect_and_serve`, `processes_poll_loop`, `send_processes_and_status_blocking`, `foreground_reporter_loop`, `send_request_blocking` (lines 5780–6104). Pure plumbing, no egui types — almost zero coupling to `FramesageApp`. Could be a self-contained module exposing 3 spawn-thread functions.
- **MED — Tray-menu construction into `tray::menu` (~150 LOC).** `TrayMenuIds`, `build_tray`, `build_icon` (lines 5616–5778, 5608–5615). Self-contained; takes `&TrayCommands` and an `egui::Context`.
- **MED — Process-tab logic into `tray::tabs::processes` (~1200 LOC).** Lines roughly 2900–4200: `render_process_detail`, `ProcessAction`, `detail_kv`, `TerminateConfirm`, `AffinityPicker`, the giant context-menu builder. The Processes tab is its own self-contained surface.
- **LOW — Profile editor into `tray::tabs::profiles` (~250 LOC).** `render_profile_editor`, `game_mode_editor`, `cpu_selector_edit`, `power_plan_edit`, `option_combo`, `format_cpu_selector`, `CpuSelectorKind` (lines 5057–5488).
- **LOW — Status/perf-band rendering** (lines 4417–4768): `render_status_hero`, `render_active_profile_summary`, `render_foreground_summary`, `render_perf_band`, `draw_per_core_matrix`, `draw_sparkline`, `render_status_bar`, `render_activity_strip`. Could live in `tray::tabs::status`.

After those extractions `main.rs` would shrink to ~2 500 LOC (state + `impl eframe::App` + a thin shell). That's manageable. The current 6 241 is not.

---

## 4. Error handling — anyhow-heavy, two suspicious patterns

**Conventions**
- `thiserror` typed errors live in the lower layers: `PolicyError` (`core/policy.rs`), `JournalError`, `PlanError`, `SafeListError` (gamemode). Correct choice — those are libraries.
- `anyhow::Result` everywhere upstream (engine, service, tray, sys/inner). Reasonable.
- `Context` is used pervasively in sys/win32 wrappers (e.g. `apply.rs:67-129`, every `set_*` call has `.context("set priority class")` etc.). Good discipline.

**Issues**
- **HIGH — 30 `.unwrap()` calls in `tray/src/main.rs` against `Mutex::lock()` results.** Every `self.state.lock().unwrap()` (lines 530, 545, 665, 695, 729, 737, 764, 976, 1702, 1710, 1744, 1764, 1803, 1997, 2511, 3876, 3964, 3986, 3990, 5727, 5730, 5733, 5736, 5789, 5814, 5832, 5881, 5910, 5958). `std::sync::Mutex::lock` only fails when the holder panicked. If *any* of the background threads panics holding the state lock, every UI access then panics — the tray becomes a panic cascade rather than recovering. Fix: `parking_lot::Mutex` (already a workspace dep — used in engine) doesn't have `LockResult`, so the unwrap goes away by switching crates.
- **MED — `.expect("spawn …thread")` (tray/src/main.rs:442, 454, 482, also `crates/tray/src/main.rs` build.rs).** Thread-spawn failures are very rare but recoverable; `expect` here kills the tray on OOM or thread-limit. Show a banner instead.
- **MED — `framesage_sys::process::cpu_times(*pid)` swallowed as `continue`** at `engine/lib.rs:1178`, `engine/lib.rs:1186`. Errors are silently dropped (no even debug log) — fine on a per-PID failure, but if every PID fails (a transient kernel issue) the whole ProBalance pass produces zero samples and nothing logs why.

---

## 5. Logging — consistent style, mostly disciplined, a couple of hot loops

**Setup.** `tracing-subscriber` initialised in `service/main.rs:97` with `EnvFilter` default `framesage=info,info`. CLI / sim use `tracing_subscriber::fmt::try_init()` (no env filter — minor inconsistency). The tray has `tracing-subscriber` as a Cargo dep but I see no `init` call — log lines from it never surface.

**Structure.** Mostly structured kv: `info!(pid = fg.pid, exe = %fg.exe_name, profile = %profile_id, "applied")` (engine/lib.rs:1822). `warn!(error = %e, "policy save after SetPolicy failed")` (service/runtime.rs:509). Good.

**Hot-path logging.**
- `engine/lib.rs:1822` — `info!` per foreground change. Reasonable cadence (a few per minute typical).
- `engine/lib.rs:1694-1700` — `debug!` per background-scan tick **only when newly_applied > 0**. Good gating.
- `engine/lib.rs:1165` — `debug!` when `iter_pids` fails. Per-tick worst case, but should be rare.
- `apply.rs:123, 129` — `tracing::debug!` per `apply_profile` call. Fine.

**Issues**
- **LOW — No log rotation / sink config.** The service writes to whatever `tracing-subscriber::fmt` defaults to (stderr). Under SCM that's discarded. For a "background utility" you almost certainly want a file sink with rotation (`tracing-appender`). Today, post-install diagnostics require running `framesage-svc --console`.
- **LOW — Tray emits no logs.** As above. Crashes happen blind.
- **LOW — `tracing` is a dep of `core`** but isn't used much there — could be dropped (see §1).

---

## 6. Threading — sync correctness OK, mixed std/parking_lot

**Engine.** `parking_lot::RwLock<EngineState>` (`engine/lib.rs:60`). `tick` does the right thing: snapshot read for `paused`, drop the read guard, fetch foreground (may syscall), then take a `write()` guard for `reconcile + scan + reassert + probalance` (lib.rs:1083–1112). Critical: **no `.await` calls inside any write-guard hold.** The tick is fully synchronous; the tokio task in `runtime.rs:56-65` calls `engine.tick()` from `tokio::spawn` but tick itself is sync.
- Read-heavy use case (IPC `Status`, `ListProcesses`, several other read-only APIs) justifies `RwLock` over `Mutex`. Correct call.
- However, `list_process_snapshots` (`engine/lib.rs:567`) takes a `write()` guard — it mutates `list_processes_prev_*` for the rolling CPU sample. Held across `iter_pid_snapshots` + per-PID OpenProcess loop — that's ~200 syscalls under a write guard. Status pipe handlers serialise behind it. Probably fine at 1 Hz, but if the tray + a CLI both poll, one blocks the other.

**Tray.** `Arc<Mutex<AppState>>` with `std::sync::Mutex` (lib.rs:425). Used by 4 background threads + the UI thread. The lock windows are short (clone snapshot, drop). No `.await` inside, since `update()` is sync. Correct, just brittle around the unwraps (§4).

**`Arc<AtomicBool>` for menu signals** (tray/main.rs:127–) — appropriate. `Ordering::Relaxed` everywhere; that's fine for fire-and-forget flag bits drained on the next frame. No memory-ordering reasoning required.

**Service tasks.** `tokio::spawn` × 4 (tick, admin pipe, status pipe, watcher) in `runtime.rs:56-91`. On shutdown they're `.abort()`ed — no graceful drain, which is documented and intentional. Acceptable for a tool that's safe to interrupt.

---

## 7. Lifetimes / clone patterns

- `Arc<RwLock<EngineState>>` for engine — write-heavy mutator + many read-only queries. Right choice.
- `Arc<Mutex<AppState>>` for tray — heavily contended on UI thread + 3 background threads, but lock windows are short. Mutex would have been fine; `parking_lot::Mutex` would be better (no poisoning unwraps).
- `&'static SafeList` (`engine/lib.rs:56`) — appropriate: the bundled list is built once at startup and never edited.
- **MED — Clone-as-default crops up on `Policy` and `Profile`.** Every IPC roundtrip clones the full Policy across the JSON layer (necessary), but inside the engine `s.policy.clone()` shows up at `engine/lib.rs:549, 1124` etc. Policy carries a `Vec<AppRule>` + `HashMap` of profiles; cloning per status call is wasteful when only a few callers actually mutate. An `Arc<Policy>` swap-on-write would let `status()` hand out a cheap clone.

---

## 8. Comments — unusually good

Sampled `engine/lib.rs` around `tick` (1083) and around `reconcile` (1703-1854); sampled `tray/main.rs` around `update` (601-790); sampled `probalance.rs` (1-100, 133-186). The prose explains **why**, not what:
- `// Reconcile, don't event-chase.` (engine/lib.rs:21) — design rationale up front
- `// Session 0 isolation: a service running as LocalSystem can't see the interactive desktop` (lib.rs:1090) — the actual Windows quirk that motivated the foreground-reporter path
- `// Some games (POE2, EVE, several Unreal titles) call SetProcessAffinityMask on themselves at startup` (lib.rs:178) — why PERSISTENT_REASSERT_INTERVAL exists at all
- `// hardware validation showed games spawning across all cores even with sets applied` (apply.rs:113) — the empirical reason CPU Sets aren't enough

Not wallpaper. This is one of the better-commented codebases I've reviewed.

---

## 9. CI — well-structured, no clippy gaps

`.github/workflows/ci.yml` has 4 jobs:
1. `cross-check` — `cargo check --workspace --all-targets --target x86_64-pc-windows-gnu` on Ubuntu. Fast pre-validation.
2. `tests-portable` — `cargo test -p core -p gamemode -p ipc -p sim` on Ubuntu host. Plus `cargo run -p sim -- demo` as a smoke test. Smart: lets a Linux dev iterate.
3. `windows-build` — `cargo test --workspace` + release build + staged artifact upload (`framesage-{tray,svc,sim}.exe` + README/LICENSE). Real validation.
4. `lints` — `cargo fmt --check` + `cargo clippy --workspace --target x86_64-pc-windows-gnu --all-targets -- -D warnings`.

`RUSTFLAGS: -D warnings` globally. Clippy is wired at strict level. Rustfmt is enforced.

**Issues**
- **LOW — Clippy not run against MSVC target.** The Linux clippy job uses `x86_64-pc-windows-gnu`. The native windows-build job runs `cargo test` but not clippy. Practical gap: MSVC-only `windows-rs` quirks won't be linted. Minor.
- **LOW — No coverage / no MSRV check.** `rust-version = "1.80"` declared in workspace `Cargo.toml:27` but no `cargo check --locked` with a pinned 1.80 toolchain to catch accidental MSRV bumps.
- **LOW — Artifacts unsigned.** Expected for pre-1.0 (the version comment in Cargo.toml notes 1.0 needs a signed installer).

---

## 10. Dependency hygiene — clean, one Win32 sprawl

Workspace `Cargo.toml` is tight: 14 direct deps, all pinned to major. No version drift between crates (they all use `workspace = true`).

**Notable**
- `eframe = "0.28"` with `default-features = false` + only `default_fonts, glow`. Persistence intentionally disabled (Cargo.toml:73 comment — eframe's window-state persistence broke after pre-DPI-manifest drags). Solid.
- `tokio` feature set is curated: no `process`, `tracing`, etc. Build-time wise that's good.
- `windows = "0.58"` features are listed per-crate (sys lists 13 sub-features, tray lists 8). No global "enable everything" leak.

**Issues**
- **LOW — `windows-sys` appears in 6 different versions** in Cargo.lock (0.45, 0.48, 0.52, 0.59, 0.60.2, 0.61.2). That's transitive (probably `parking_lot`, `notify`, `tokio`, `eframe` all on different schedules), but it bloats compile time noticeably.
- **LOW — `parking_lot` is in the workspace, but tray uses `std::sync::Mutex`.** Pick one.
- **LOW — `tracing` in `core`** — see §1.

---

## 11. Magic numbers — engine well-named, tray scattered

**Good**
- Engine intervals are named constants at the top of `lib.rs`: `BACKGROUND_SCAN_INTERVAL` (10 s, line 173), `PERSISTENT_REASSERT_INTERVAL` (2 s, line 184), `PROBALANCE_SAMPLE_INTERVAL` (1000 ms, line 190). Each has a paragraph-long comment justifying the value.

**Issues**
- **LOW — Tray has inline `Duration::from_millis(N)` calls.**
  - `tray/main.rs:477` — `500` (show-window watcher recovery sleep)
  - `tray/main.rs:612` — `2` s repaint floor
  - `tray/main.rs:5794` — `1500` (IPC reconnect)
  - `tray/main.rs:5945-46` — `1000` / `8000` (processes poll visible/hidden)
  - `tray/main.rs:6045` — `250` (foreground reporter)
  
  Most have neighbour comments explaining; only one (`5794`) has no rationale. Promoting to named constants à la engine would help.
- **LOW — `MAX_RECENT = 1000`** lives inside the function (tray/main.rs:5892). Module-level constant alongside `SYSTEM_HISTORY_LEN`.
- **LOW — `version_info_budget: u32 = 8`** at `engine/lib.rs:599` — fine, just a budget; documented in comment.
- **LOW — Service tick interval `Duration::from_millis(300)`** at `runtime.rs:57`. Promote to named constant — it's load-bearing for ProBalance sample math.

---

## 12. Unsafe — thoroughly justified with SAFETY comments

168 `unsafe { }` blocks across 17 files. 152 `SAFETY:` annotations. Spot-check ratio is excellent.

**Sample (`crates/tray/src/win32.rs`)** — every unsafe block has a SAFETY line above it explaining handle validity, layout, and out-param ownership (lines 63-65, 71-80, 82-83). The `SingletonGuard` `Drop` impl closes the handle on drop — RAII done right. Same pattern in `is_elevated`.

**Sample (`crates/sys/src/inner/process.rs`)** — 30 `unsafe` blocks, 24 SAFETY notes. The mismatch is benign: many blocks are inside a function with a top-of-function SAFETY note that covers a block of similar calls (e.g. ToolHelp iteration).

**`crates/sys/src/inner/apply.rs`** — 38 `unsafe` / 28 SAFETY. Same pattern.

No invariants are obviously violated. Handle lifetimes are tracked. `CloseHandle` is consistently paired. The RAII guards (`SingletonGuard`) are the right pattern; sys/win32 could adopt similar RAII for `OpenProcess` handles instead of paired manual `CloseHandle` (it's already a class of bug source — easy to miss a path), but that's a follow-up refactor, not a defect.

---

## 13. Test coverage — heavy in policy/probalance, sparse everywhere else

Test counts (`#[test]` per crate):
- `core/policy.rs` — 15 (match_foreground, AppMatch::matches, glob, regex)
- `core/profile.rs`, `core/topology.rs`, `core/game_mode.rs`, `core/paths.rs` — handful each
- `engine/probalance.rs` — 10 (full state-machine)
- `engine/lib.rs` — **2** (both about `applied_from_plan`)
- `gamemode/safe_list.rs`, `gamemode/planner.rs`, `gamemode/journal.rs`, `gamemode/state.rs` — several each
- `ipc/lib.rs` — a few (request/response round-trip)
- `sys/inner/process.rs`, `sys/inner/process_actions.rs`, `sys/inner/io_priority.rs`, `sys/inner/topology.rs`, `sys/inner/game_mode/{windows_update,power_plan}.rs` — basic
- `sim/main.rs` — present
- `tray/formatters.rs`, `tray/tree.rs` — present (the extracted modules brought their tests with them)

**Severity — HIGH on the engine gap.** The crate that decides what happens on every foreground change has 2 tests, both covering a single helper function. None of `tick`, `reconcile`, `maybe_run_probalance_locked`, `maybe_scan_background_locked`, `maybe_reassert_persistent_locked`, `reconcile_system_mode_locked`, `revert_system_mode_locked`, `apply_once`, `report_foreground` is tested.

Sim crate exists and `cargo run -p sim -- demo` runs in CI — that's the integration-test substitute, but it's a smoke test, not assertions on behaviour.

---

## 14. API surface — mostly minimal

Engine pub surface (lines via grep at `pub fn` in `engine/lib.rs`): ~30 public methods on `Engine`. They map 1:1 to IPC `Request` variants (`status`, `pause`, `resume`, `set_policy`, `apply_once`, `set_manual_override`, etc.). Reasonable for the service boundary.

**Issues**
- **LOW — Several internal helpers are `pub fn` that look like they should be `pub(crate)` or private.** Candidates: `policy_snapshot()` at line 441 (only the IPC handler uses it), `recover_orphan_journal` at line 1061 (only the service calls it at startup). These are fine as-is for the binary boundary, but if a second consumer ever links engine they'd see surface they shouldn't reach for.
- **LOW — `sys/inner/mod.rs` re-exports everything `pub mod`** which means every helper inside (e.g. `close_handle`, `mask_from_indices`) is reachable via `framesage_sys::apply::*`. Most are visibility-restricted via `fn` not `pub fn`, so this is academic. Verified.

---

## 15. cfg discipline — exemplary

`#[cfg(windows)]` and the non-Windows stub crate work hand in hand:
- `sys/lib.rs:9-19` — `inner` on Windows, `stub` elsewhere. Stub matches inner's surface (verified by walking both — every function in inner has a counterpart in stub).
- `service/runtime.rs` — `detect_topology` has parallel `#[cfg(windows)]` and `#[cfg(not(windows))]` definitions (line 197/202). Same for `serve_ipc` (231/281). The non-windows path runs the engine against a synthesised 16-thread topology so the state-machine layers exercise end-to-end on a dev Mac/Linux.
- `engine/lib.rs` — sprinkle of `#[cfg(windows)]` inside functions for syscall-touching branches; `_ = pid;` style discards on the non-windows side keeps unused-variable warnings clean.

The Linux CI job verifies all this stays buildable. **This is the single biggest reason developer iteration doesn't require a Windows VM.**

---

## What's done well — explicit credit

- **Workspace layering is correct.** `core` is pure data + tests, `sys` is a thin Win32 layer with a stub, `engine` orchestrates. The bin crates (cli, service, tray, sim) compose them.
- **`sim` crate.** Lets the planner + engine state machines be driven from a Mac/Linux. Combined with the portable-test CI job, it's a meaningful pre-Windows iteration loop.
- **RAII handle wrappers in `tray/win32.rs`.** `SingletonGuard` + `EventGuard` close their handles on drop. The pattern should propagate to `sys/inner/*` (where handles are still closed manually).
- **Probalance is well-designed.** Pure function + injected clock + 10 tests = textbook testable state machine. The rest of the engine should look like this.
- **CI matrix.** Cross-check on Linux + native Windows + lints + portable tests. Catches the bulk of what could break before a Windows-only contributor sees it.
- **Comment culture.** Comments explain *why* — design rationale, empirical findings, the actual Windows quirk that motivated each choice. Not wallpaper.

---

## Top-5 to fix next, ranked

> **Status (2026-08-15):** items 1-5 are **resolved** except the `sys → gamemode` clause of item 5, which is **by design** (see the Status re-audit block above). The remaining live follow-ups are the LOW polish items scattered through the body (inline tray durations, `pub(crate)` visibility, `windows-sys` version sprawl).

1. **HIGH — Make `Engine::reconcile` and `Engine::tick` unit-testable.** Introduce a `trait SysApi` for the syscall surface (mirror the `SystemStateQuery` pattern already used by gamemode). Inject an `impl SysApi` plus a `Clock` into `EngineDeps`. Add the missing integration tests. *✅ Resolved — `SysApi` + `FakeClock` in place, 62 engine tests.*
2. **HIGH — Swap `std::sync::Mutex` for `parking_lot::Mutex` in tray.** Kills 30 `.unwrap()` calls and the panic-cascade failure mode. *✅ Resolved — parking_lot throughout, no `PoisonError` handling.*
3. **MED — Extract the 3 obvious modules from `tray/main.rs`** (IPC client, menu builder, Processes-tab logic). Brings the file from 6 241 LOC to ~2 500. *✅ Resolved — modules split out.*
4. **MED — File-sink logging (`tracing-appender`) for service + tray.** Without it, post-install diagnostics require console mode. *✅ Resolved — rolling file appender wired in service.*
5. **LOW/MED — Move `tracing` out of `core`** and `framesage-gamemode` dep out of `sys`. Tightens the dependency arrows. *✅ `core` dep removed; the `sys → gamemode` arrow is by design and stays.*
