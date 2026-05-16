# 02 – OS Correctness Audit (Win32 / NT API surface)

Scope: every Win32 / NT call reachable from `crates/sys/src/inner/**` plus
`crates/tray/src/win32.rs`. Findings are ranked by blast radius. File paths
are absolute. Files referenced in the brief but absent in this repo
(`memory.rs`, `system_cpu.rs`, `user_info.rs`) are folded into `process.rs`
in the current tree — that's where the relevant code lives.

---

## CRITICAL findings (potential BSOD / kernel-state corruption / mass-kill)

### C1. The "apply-to-any-PID" IPC surface bypasses the safe-list entirely
**Files:**
- `crates/engine/src/lib.rs:302–410, 416–425` (`set_process_priority`, `suspend_process`, `resume_process`, `trim_working_set`, `set_process_affinity`, `terminate_process`)
- `crates/sys/src/inner/process_actions.rs:36–98` (`suspend` / `resume` / `terminate`)
- `crates/sys/src/inner/apply.rs:328–333, 369–379, 385–394, 342–356` (`set_priority_class_for_pid`, `set_affinity_mask_for_pid`, `restore_priority_class_for_pid`, `trim_working_set_for_pid`)

**What's wrong:** The IPC actions reachable from the tray's Processes-tab
right-click menu (set priority, set affinity, suspend, resume, terminate,
trim working set) accept an arbitrary `pid` and feed it straight to
`OpenProcess` + the kernel write. The denylist in
`crates/gamemode/src/safe_lists/processes.json` (csrss, wininit, lsass,
services, smss, dwm, audiodg, MsMpEng, anti-cheat, GPU drivers, …) is only
consulted by:
- The Game Mode planner (`crates/gamemode/src/planner.rs`)
- ProBalance demotions (`crates/engine/src/probalance.rs:198`, via the
  `safe_list_exes` set assembled in `lib.rs:1261–1265`)
- The background-scan path in `lib.rs:1647–1657`

`process_actions::terminate` only refuses PID 0 and PID 4
(`process_actions.rs:73–77`). `open_for_suspend` does the same
(`process_actions.rs:89–93`). Everything else — including `csrss.exe`
(PID ~600 on a typical box), `wininit.exe`, `services.exe`, `lsass.exe`,
`smss.exe` — is gated only by NT's access check. `csrss` and `wininit`
are **Critical Processes**: terminating them blue-screens the box with
`CRITICAL_PROCESS_DIED` (0xEF). An admin token (which the tray relaunch
flow makes the common case — `tray/src/win32.rs:241`) plus
`SeDebugPrivilege` (granted by default to elevated admins) is enough to
get `PROCESS_TERMINATE` on most of them.

**Concrete risk:** Single misclick from an elevated user, or any rogue
client that can open the named pipe, BSODs the box. `csrss` termination is
literally the textbook BSOD demo.

**Severity: CRITICAL.** Add a denylist gate in
`process_actions::terminate`, `suspend`, and in
`apply::set_priority_class_for_pid` / `set_affinity_mask_for_pid` /
`trim_working_set_for_pid` before the `OpenProcess`. The denylist is a
static set already loaded at startup; the check is cheap.

### C2. Profile `apply()` on system PIDs is unguarded
**File:** `crates/sys/src/inner/apply.rs:57–154` (`apply`)

The rule engine maps `exe_name` → profile. There's no defense against a
user (or attacker controlling `policy.json`) writing a rule that targets
`csrss.exe`. `apply()` opens with
`PROCESS_QUERY_INFORMATION | PROCESS_SET_INFORMATION | …` (line 291–298)
and then calls `SetPriorityClass(IDLE)`, `SetProcessAffinityMask(1)`,
`SetProcessInformation(MemoryPriority=VeryLow)`, etc. on the live
critical process. Setting csrss to IDLE priority class or pinning it to
one logical CPU does not BSOD outright but reliably hangs the session
(input freezes, the desktop becomes unresponsive). Setting affinity = 1
on `lsass.exe` will deadlock the logon path.

**Concrete risk:** A `policy.json` rule for `csrss.exe`, `lsass.exe`,
`dwm.exe` (or even just a typo'd glob that catches one) corrupts the
running session until reboot. Less catastrophic than a BSOD, but still
recovery-by-reset for end users.

**Severity: CRITICAL.** `apply()` must consult the safe-list denylist
on the exe name before touching the handle.

### C3. Topology flattens to processor group 0 — affinity on >64-CPU machines silently misbehaves
**File:** `crates/sys/src/inner/topology.rs:51, 174–177, 233–243`
**File:** `crates/sys/src/inner/apply.rs:434–442` (`mask_from_indices`),
`apply.rs:428–432` (`set_affinity_mask`)

`enumerate_cores` explicitly skips `g.Group != 0` (line 235). The mask
returned to `SetProcessAffinityMask` is a `usize`, which on x64 is 64
bits — but `SetProcessAffinityMask` is itself group-scoped and only
addresses the calling process's *primary* group. Threadripper PRO 96-core
and dual-socket Xeon/EPYC machines (which are exactly the workstations
this tool targets) have >64 logical CPUs split across multiple groups.
On those, the user's "pin to CCD 3" selector lands in `mask_from_indices`,
gets a bit pattern that intersects group 0's logical set, and the kernel
silently applies a *different* (or empty) affinity than what the user
expected.

Per Microsoft's docs: a process that already runs in a multi-group
configuration ignores `SetProcessAffinityMask` if the mask would cross
groups; in single-group mode the call is fine but pins to group 0.

**Concrete risk:** On Threadripper PRO (the silicon framesage is
specifically marketed for via X3D detection), the user pins a game to
"Kind(Cache)" expecting CCD 1 (logical 16–31, group 0) and instead gets
group-0 CPUs that aren't part of the X3D CCD at all — performance
*regression* vs. doing nothing. Module docs at `topology.rs:8–10`
acknowledge this is "v0.2." Document; don't ship the affinity hammer
on multi-group hardware until `GROUP_AFFINITY` lands.

**Severity: HIGH** (drops to CRITICAL if a customer with a 96-core
chip enables it — silent miscalculation on a $5K CPU).

### C4. Intel hybrid (P/E) tagging is wrong
**File:** `crates/sys/src/inner/topology.rs:54–66`

Every logical CPU is tagged `CoreKind::Performance` at construction
(line 62). The "CCDs of 8" heuristic (line 51) is AMD-specific. On Intel
12th-gen+ (Alder Lake, Raptor Lake, Meteor Lake) the correct signal is
`PROCESSOR_RELATIONSHIP::EfficiencyClass` (0 = E-core, ≥1 = P-core),
which the code does not read. Module docs at `topology.rs:22–23`
acknowledge this.

`retag_ccds_from_signals` in `framesage_core` then groups by 8 cores
and stamps the larger-L3 group as `Cache` — fine on a Ryzen X3D, complete
nonsense on a 13900K where E-cores share an L3 with P-cores in the same
"CCD-of-8."

**Concrete risk:** "Pin to Performance cores" on a 13900K either misses
P-cores or includes E-cores. Same as C3, this turns into a perf
regression vs. doing nothing. **Severity: HIGH.**

### C5. `set_affinity_mask` ignores `system_mask` returned by GetProcessAffinityMask
**File:** `crates/sys/src/inner/apply.rs:419–432, 434–442`

`get_affinity_mask` discards the second out-param. `mask_from_indices`
just OR's bits `1<<i`. There's no check that the resulting mask is a
*subset* of the system mask. Setting a bit for a logical CPU the target
process can't legally run on (e.g. WSRM-restricted, or a CPU offlined
by powercfg) causes `SetProcessAffinityMask` to return `ERROR_INVALID_PARAMETER`
— the apply() context translates that to a generic "set affinity failed,"
the engine logs and continues, and the profile is half-applied with no
revert. Worst case the user's hand-edited affinity mask in `policy.json`
contains a stale bit; we now log an error every reassert tick.

**Severity: MEDIUM** (no crash, just confusing failure mode). Mask the
computed value with the system mask before calling.

---

## HIGH findings (per-process corruption, handle leaks, error-silencing)

### H1. `K32EmptyWorkingSet` exposed for arbitrary PIDs (incl. via IPC)
**File:** `crates/sys/src/inner/apply.rs:146–149, 342–356`

`K32EmptyWorkingSet` is documented to require `PROCESS_QUERY_INFORMATION |
PROCESS_SET_QUOTA`. The code opens with both (via `open_for_write`,
line 290–298). The call is safe on user processes — at worst, the
process page-faults heavily on next dispatch. But:
- On `MsMpEng.exe` (Windows Defender), trimming the working set is
  documented to cause heavy disk I/O storms as it page-faults its
  signature database back in. Defender is on the denylist for suspend
  but NOT consulted here.
- On a protected/PPL process the open fails (good).

The `apply.rs:148` site (inside `apply()`) inherits whatever the profile
rule says. The `trim_working_set_for_pid` site (line 342) is the
IPC-reachable variant.

**Severity: MEDIUM.** Apply the same denylist gate as C1.

### H2. `process.rs:200–211` — `OpenProcessToken` leak on early return
**File:** `crates/sys/src/inner/process.rs:204–210`

```rust
let mut token: HANDLE = HANDLE::default();
let token_result = unsafe { OpenProcessToken(proc_handle, TOKEN_QUERY, &mut token) };
close_handle(proc_handle);
if token_result.is_err() {
    return Ok(None);
}
```

On `OpenProcessToken` *failure* `token` is `HANDLE::default()` = NULL
and there's nothing to close — fine. But on **success** every subsequent
early-return path goes through an explicit `close_handle(token)` — and
those are there (line 218, 232). So this is correct. Confirming "no leak."

(Listing this so it's clear I checked. Not a finding.)

### H3. `apply.rs:266` — return-value pattern abuse on `SetProcessDefaultCpuSets(None)`
**File:** `crates/sys/src/inner/apply.rs:266–268`

```rust
if let Err(e) = unsafe { SetProcessDefaultCpuSets(handle, None) }.ok() {
    warn_revert(pid, "SetProcessDefaultCpuSets(None)", e);
}
```

`SetProcessDefaultCpuSets` returns `BOOL`. `.ok()` on a `BOOL` is from
the `windows` crate's `BOOL::ok()`, which converts to `Result<(), Error>`.
`if let Err(e) = …` is correct, but the pattern is brittle — readers
expect `?` or `.map_err`. Not a soundness issue. **Severity: LOW** (style).

### H4. `set_affinity_mask` accepts mask=0 indirectly
**File:** `crates/sys/src/inner/apply.rs:428–432, 434–442`

`mask_from_indices` returns 0 when `indices` is empty. The public
`set_affinity_mask_for_pid` rejects 0 (line 370–374) — good. But
`apply()` at line 126–134 and 139–144 checks `hard_mask != 0` / no
check on `mask` from the second branch. The second branch (line 139–144,
`profile.affinity_mask`) does NOT check for zero before calling
`set_affinity_mask`. The kernel rejects mask=0 with
`ERROR_INVALID_PARAMETER`, the apply path returns the error, and the
profile is left half-applied.

**Severity: MEDIUM.** Add a `mask != 0` guard at line 142.

### H5. `process.rs:583–587` — `cpus > MAX_CPUS = 256` rejects valid hardware
**File:** `crates/sys/src/inner/process.rs:567, 589–593`

Cap of 256 is fine today, but the per-CPU sampling code falsely rejects a
512-core dual-socket Sapphire Rapids EPYC. This will appear when
hyperscalers run the tool on dev workstations. **Severity: LOW** (future
proofing).

### H6. `process.rs:317–342` — `sid_to_string` LocalFree on null is benign but value pointer is read pre-check
**File:** `crates/sys/src/inner/process.rs:322–340`

```rust
let mut out: PWSTR = PWSTR::null();
let result = unsafe { ConvertSidToStringSidW(sid, &mut out) };
if result.is_err() || out.is_null() {
    return None;
}
```

This is correct — null-check before dereference. The subsequent `len`
loop dereferences `*out.0.add(n)` which is UB if `out` is not
null-terminated. `ConvertSidToStringSidW` is documented to always
null-terminate. Confirmed safe. (Not a finding.)

### H7. `version_info.rs:148–164` — `value_len` measured in WCHARs, not bytes
**File:** `crates/sys/src/inner/version_info.rs:155–161`

Comment says "value_len is in WCHARs (NOT bytes)" — this is correct per
`VerQueryValueW` docs. The slice is built from `value_ptr as *const u16`
with `len_chars` u16s. Code is right; flagging because this is one of
those API quirks that bites the next maintainer who "fixes" the comment.

### H8. `process.rs:240–243` — `TOKEN_USER` cast assumes the layout is at offset 0
**File:** `crates/sys/src/inner/process.rs:240–241`

```rust
let token_user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
let sid = token_user.User.Sid;
```

Documented and correct. `GetTokenInformation(TokenUser)` writes a
`TOKEN_USER` at offset 0 followed by the variable-length SID data the
struct's `Sid` field points into. The buffer must outlive `sid` use —
it does (the surrounding `buf` is held through both `LookupAccountSidW`
calls). Safe. (Not a finding.)

### H9. `cppc.rs:30–52` — `CallNtPowerInformation` buffer matches request size
**File:** `crates/sys/src/inner/cppc.rs:26–55`

Size is computed `expected_count * sizeof(PROCESSOR_POWER_INFORMATION)`
and passed to the API. Safe. Note `expected_count` is whatever
`topology.rs` enumerated — group 0 only — so on a multi-group machine
the array is undersized vs. what the kernel knows about. The kernel
truncates and returns success; no overflow. (Not a finding, downstream
of C3.)

---

## MEDIUM findings (subtle correctness / future-fragility)

### M1. `game_mode/process.rs:101–120` — atomicity hole on `resume_process`
**File:** `crates/sys/src/inner/game_mode/process.rs:73–120`

Documented (lines 12–17): suspend isn't atomic against new thread
creation. Resume loops `ResumeThread` to drain the suspend count
(line 107–115). Per-thread loop is fine, but:
1. A thread created AFTER suspend that we never suspended will still be
   running. If we resume the others, the process resumes overall —
   intended. But if framesage's suspend was followed by an outside party
   also suspending (e.g. user double-clicked Pause in Task Manager), our
   resume only decrements our own count once per `ResumeThread` per
   thread, leaving the target half-suspended.
2. `SuspendThread` on x64 has the well-known "suspended in syscall"
   hazard for `DllMain`-equivalent paths. NOT a problem for the
   targeted workload (OneDrive, Dropbox); a problem for general use.

Module already documents both. **Severity: LOW–MEDIUM.** Prefer
`NtSuspendProcess` (already used by the IPC path in
`process_actions.rs`!) — note the explicit comment at
`game_mode/process.rs:3–9` rejecting it for "no Nt-prefix surprises,"
while `process_actions.rs:23–30` uses it anyway. Internally
inconsistent.

### M2. `power_plan.rs:54–63` — `PowerSetActiveScheme` does not persist
**File:** `crates/sys/src/inner/game_mode/power_plan.rs:49–63`

`PowerSetActiveScheme` is documented as session-active. The setting
*does* persist into the per-user power policy across reboots (Windows
writes it to the registry), but it does NOT survive certain
sleep/hibernation transitions cleanly: when modern standby (S0ix) wakes,
the kernel can revert to the "balanced on AC, balanced on DC" default
under some OEM power-profile XMLs. The revert path
(`game_mode/apply.rs:125–134`) does restore correctly when game-mode
exits. **Severity: LOW.** Document. The PreviousState capture at
`query.rs:23–27` is the right approach.

### M3. `windows_update.rs:61–77, 81–92` — does not capture/restore prior pause window
**File:** `crates/sys/src/inner/game_mode/windows_update.rs:14–15`

Self-documented at module level. If the user had a 7-day pause active,
our 1-hour pause overwrites it; revert deletes our keys, leaving the
user with no pause. **Severity: MEDIUM.** Documented as v0.3.

### M4. `power_plan.rs:38–43` — `*guid_ptr` deref before LocalFree
**File:** `crates/sys/src/inner/game_mode/power_plan.rs:39–42`

Correct order: deref → copy by value → `LocalFree`. No use-after-free.
(Not a finding.)

### M5. `service.rs:172–187` — `wait_for_state` polls without timeout-on-handle-death
**File:** `crates/sys/src/inner/game_mode/service.rs:172–187`

If the service binary crashes mid-stop, SCM may report `STOP_PENDING`
forever. The 30 s timeout (line 29) catches this, but a misbehaving
service that bounces between RUNNING and STOP_PENDING will exhaust the
timeout. **Severity: LOW.** Acceptable.

### M6. `foreground.rs:64–66` — `GetWindowTextW` buffer = `len + 1`
**File:** `crates/sys/src/inner/foreground.rs:64–66`

`GetWindowTextLengthW` returns *without* the terminator; `GetWindowTextW`
needs `len+1` for the terminator and returns chars *without*. Code uses
`len+1` for the buffer and slices `..n`. Correct. Note that
`GetWindowTextLengthW` can over-report for windows owned by other
processes (it returns the buffer needed if the window had its WM_GETTEXT
handled synchronously, which it may not be). This means buffer is sometimes
larger than needed — fine, never smaller. Safe. (Not a finding.)

### M7. `tray/win32.rs:127–149` — `acquire_singleton` retries `CreateMutexW` instead of waiting on the existing handle
**File:** `crates/tray/src/win32.rs:125–153`

Each retry closes its handle (line 142) and re-creates. Functionally
correct; idiomatic would be `OpenMutexW` + `WaitForSingleObject` on the
existing handle. Cost: ~3 extra syscalls per 200 ms during the 3 s
handoff window. **Severity: LOW** (style/efficiency).

### M8. `windows_update.rs:103–115` — `KEY_WOW64_64KEY` redundant in this binary
**File:** `crates/sys/src/inner/game_mode/windows_update.rs:110`

`framesage-sys` is built as 64-bit (Cargo.toml `[target.'cfg(windows)']`).
A 64-bit process accessing HKLM\SOFTWARE\Microsoft\WindowsUpdate is
already in the 64-bit view. `KEY_WOW64_64KEY` is a no-op here, but if
the binary is ever cross-compiled to x86 (WoW64), the flag becomes
load-bearing. **Severity: NONE** (defensive; correct).

---

## LOW findings (style / hygiene)

### L1. `process.rs:667–671` — `close_handle` swallows the result
**File:** `crates/sys/src/inner/process.rs:667–671`

```rust
fn close_handle(h: HANDLE) {
    let _ = unsafe { CloseHandle(h) };
}
```

`CloseHandle` failures on a non-pseudo handle indicate a double-close or
invalid handle — a bug in our code. Swallowing the result is fine for
production but loses signal. Consider a `debug_assert!` in debug builds.

### L2. Pervasive `let _ = unsafe { CloseHandle(h) };` instead of RAII
**Files:** every file in `crates/sys/src/inner/**`

Every Win32 HANDLE acquisition is paired with a manual close. There's
no `HandleGuard` wrapper. With many functions having 2+ early-return
paths (`apply.rs`, `process.rs::user_for_pid`), one missed close in a
future refactor is a guaranteed handle leak. The codebase is currently
careful and I did not find a missed close path on this read, but the
*next* refactor is one branch away from leaking.

`tray/win32.rs:90–103` (`SingletonGuard`) shows the team knows the
pattern. **Severity: LOW.** Consider an `OwnedHandle` newtype in
`crates/sys/src/inner/mod.rs` with `Drop` calling `CloseHandle`.

### L3. `process_actions.rs:73` — terminate refuses PID 0/4 but not PID for `System` / `Registry` / `Memory Compression`
**File:** `crates/sys/src/inner/process_actions.rs:73`

`System` is PID 4. `Registry` and `Memory Compression` are minimal
processes (PIDs vary, not 0/4). Their `OpenProcess(PROCESS_TERMINATE)`
will fail with ACCESS_DENIED — kernel will not let them be terminated
from user mode — so the deny-by-omission is harmless today. Subsumed
by C1.

### L4. `apply.rs:30` — `THREAD_SET_LIMITED_INFORMATION` may be insufficient on older Windows
**File:** `crates/sys/src/inner/apply.rs:633–634`

`SetThreadSelectedCpuSets` is documented to need `THREAD_SET_LIMITED_INFORMATION`
on Win10 1809+. On older builds (1607–1803) it needed `THREAD_SET_INFORMATION`.
Framesage targets modern Windows so this is fine. (Not a finding;
flagging because the comment at line 633 is correct.)

### L5. `topology.rs:165–177` — multi-group L3 lost
**File:** `crates/sys/src/inner/topology.rs:174–177`

`if mask.Group == 0 { … }` — same scope cut as C3. Threadripper PRO
"3D V-Cache on CCD in group 1" silently has no L3 size recorded;
X3D detection then misfires. Subsumed by C3.

---

## Hot-reload concerns (policy.json watcher)

### N1. `service/runtime.rs:130–150` — `notify` watcher fires mid-write
**File:** `crates/service/src/runtime.rs:130–150` (per Grep results)

Standard notify pitfall: editors write `policy.json` as
truncate→write→close, and the watcher can fire on the empty intermediate
state. The current loader at `crates/core/src/policy.rs` (not read in
this audit) needs to handle JSON parse errors gracefully and NOT clear
the in-memory policy on a failed load. Not directly an OS-correctness
issue but adjacent to it — if a failed parse drops the policy, the
engine reverts to defaults, which then revert running profiles, which
calls `SetPriorityClass`/etc. with the *original* class. Net: a save
to `policy.json` causes a one-tick churn of kernel writes on every
managed PID. **Severity: LOW–MEDIUM.** Out of pure-Win32 scope but
flagged because it amplifies the C2 blast radius.

---

## What's done well

- **Safe-list architecture** (`crates/gamemode/src/safe_list.rs`) is
  excellent: vendored JSON, schema-versioned, deny overrides allow,
  case-insensitive, denylist explicit and tested
  (`safe_list.rs:327–344`). The right shape for trust-boundary code.
  The bug is that not all syscall sites consult it (C1, C2, H1).
- **PID 0/4 refusal** in `process_actions.rs:73, 89` for the most
  common system PIDs.
- **The two-call sizing dance** is correctly implemented everywhere
  it's used: `LookupAccountSidW` (`process.rs:255–286`),
  `GetTokenInformation` (`process.rs:216–230`),
  `GetSystemCpuSetInformation` (`apply.rs:547–570`),
  `GetLogicalProcessorInformationEx` (`topology.rs:188–209`),
  `NtQuerySystemInformation` (`process.rs:572–612`).
- **`NtSetInformationProcess(ProcessIoPriority)`** in `io_priority.rs`:
  correct InfoClass (33), correct buffer size (`sizeof(u32)`),
  correct alignment (u32 is 4-aligned, kernel requirement is met),
  status checked via `status.0 < 0`. Round-trip test
  (`io_priority.rs:128–154`) exercises real NT calls.
- **`SingletonGuard`** in `tray/win32.rs:90–103` is the right RAII
  shape; would love to see it spread to `crates/sys/src/inner`.
- **`PROCESS_QUERY_LIMITED_INFORMATION`** consistently used for
  read-only operations (`process.rs:147, 199, 356, 422, 652`,
  `foreground.rs:76`). This is the modern Win10+ idiom — works on
  anti-cheat-protected processes for read where the older
  `PROCESS_QUERY_INFORMATION` was denied.
- **`PowerGetActiveScheme` LocalFree** correctly ordered in
  `power_plan.rs:38–42` (deref + copy before free).
- **WoW64-safe registry access** (`KEY_WOW64_64KEY` in
  `windows_update.rs:110`) — defensive, costs nothing.
- **Service-stop polling** with bounded timeout
  (`service.rs:172–187`) — won't hang the engine on a stuck service.
- **`anti-cheat-clean` API choices everywhere**: ToolHelp instead of
  reading kernel memory; documented `K32EmptyWorkingSet` instead of
  poking process VAD; `NtSuspendProcess` (kernel-routine, not driver
  injection) for atomic suspend.

---

## Recommended remediation order

1. **C1 + C2 + H1** in one PR: add `safe_list::check_process` gate
   at every IPC entry-point and inside `apply::apply` keyed on
   `exe_name`. This closes the BSOD risk without changing any API.
2. **C5 / H4**: mask sanitization (intersect with system mask, reject
   zero).
3. **C3 / C4 / L5**: ship a `GROUP_AFFINITY` path and read
   `EfficiencyClass`. Block "CPU pinning" UI controls on multi-group
   hardware until that lands.
4. **L2**: introduce `OwnedHandle` newtype.

Total Win32 surface audited: ~1,800 LoC across 13 files. Estimated
remediation: ~400 LoC + ~100 LoC tests.
