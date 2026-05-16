# 03 — Privilege & Security Audit (framesage-win)

Threat model: a non-admin local user (or malware running with that user's token)
trying to escalate by abusing the LocalSystem `framesage` service through its
two named pipes or any file it reads.

Read-only review. No code modified.

## Architecture recap

- `framesage-svc.exe` → Windows service, **LocalSystem** (`crates/cli/src/main.rs:248`
  `account_name: None`), `SERVICE_TYPE = OWN_PROCESS`, `AutoStart`.
- Two pipes:
  - **Admin pipe** `\\.\pipe\framesage-admin` — default Win32 DACL
    (`crates/service/src/pipe.rs:149-165`, tokio `ServerOptions::create`).
  - **Status pipe** `\\.\pipe\framesage-status` — explicit SDDL
    `D:(A;;GA;;;BA)(A;;GA;;;SY)(A;;GA;;;AU)` (`crates/service/src/pipe.rs:50`)
    grants Generic All to Authenticated Users.
- Status pipe rejects mutators via `kind == PipeKind::Status && !req.is_read_only()`
  (`crates/service/src/runtime.rs:320`).
- Tray runs unprivileged; elevates via `ShellExecute("runas")`
  (`crates/tray/src/win32.rs:241`) before talking to the admin pipe.

---

## What's done well

- **Two-pipe split, both ACL'd in the kernel.** The SDDL is auditable, short,
  and grants `GA` (not `GW|GR`) so connection semantics are unambiguous
  (`crates/service/src/pipe.rs:50`).
- **Defense-in-depth read-only enforcement.** Even if a future change opens
  a mutator over the status pipe, the server rejects it
  (`crates/service/src/runtime.rs:320`). The `Request::is_read_only` match
  is exhaustive — the compiler enforces classification of every new variant
  (`crates/ipc/src/lib.rs:158-178`), and a unit test locks the contract
  (`crates/ipc/src/lib.rs:389-444`).
- **`FILE_FLAG_FIRST_PIPE_INSTANCE`** on first bind defeats pipe-name
  squatting by a process that races the service at boot
  (`crates/service/src/pipe.rs:103`, `crates/service/src/runtime.rs:247-256`).
- **`PID 0 / PID 4` refused** for suspend/terminate (`crates/sys/src/inner/process_actions.rs:73,89`).
- **Atomic policy save** via temp + rename (`crates/core/src/policy.rs:285-296`).
- **No HTTP/telemetry deps** anywhere in the workspace (`Cargo.toml`,
  full grep for `reqwest|hyper|ureq` — only docstring matches). Zero
  network surface, zero updater. **No phone-home risk.**
- **No `LoadLibrary`, no `SetCurrentDirectory`, no `SearchPath`** anywhere
  in the service code path. All Windows APIs are bound statically via
  the `windows` crate import tables. No DLL-search-order hijack vector.
- **`StorageFlags + parking_lot`** — no `unsafe` cross-process shared
  memory.

---

## Findings

### CRITICAL-1 — TerminateProcess via admin pipe can kill protected SYSTEM PIDs (privilege depends on caller)
- **File:line:** `crates/ipc/src/lib.rs:107`, `crates/service/src/runtime.rs:379`,
  `crates/engine/src/lib.rs:416`, `crates/sys/src/inner/process_actions.rs:72-86`.
- **Scenario:** `Request::TerminateProcess { pid }` is admin-pipe-only, so the
  caller must already be an Administrator. But once admitted, the service
  (LocalSystem) opens the target with `PROCESS_TERMINATE` and calls
  `TerminateProcess(handle, 1)` with **no allow-list, no integrity check,
  no PPL check** beyond `pid != 0 && pid != 4`. A misbehaving / compromised
  *elevated* tray (e.g. malware that obtained admin via another bug) can
  use the service to kill processes the admin token alone couldn't —
  notably any process running as another interactive admin, antivirus
  agents that are not PPL, EDR helper services. The service's LocalSystem
  token has `SeDebugPrivilege` (services do not by default but LocalSystem
  is granted it implicitly via membership in admin groups during SCM
  start) and can `OpenProcess(PROCESS_TERMINATE)` on targets a plain
  admin couldn't. **This is a real privilege amplification.**
- **Severity:** Critical (EoP from admin-but-not-LocalSystem → LocalSystem-scope
  process kill).
- **Fix sketch:** allow-list by exe name OR refuse if target is a Windows
  service / has higher integrity than the caller; or impersonate the named
  pipe client (`ImpersonateNamedPipeClient`) and let the kernel decide.
  Same applies to `SuspendProcess` (`runtime.rs:355`) and
  `SetProcessAffinity` (`runtime.rs:391`) — anyone can pin csrss to one
  CPU and DoS the box.

### CRITICAL-2 — `SetPolicy` accepts arbitrary `Policy`; profiles can stop services / change power plan / suspend any process
- **File:line:** `crates/ipc/src/lib.rs:43`, `crates/service/src/runtime.rs:488-523`,
  `crates/sys/src/inner/game_mode/service.rs`, `crates/sys/src/inner/game_mode/windows_update.rs`,
  `crates/sys/src/inner/game_mode/power_plan.rs`.
- **Scenario:** Admin pipe is admin-only, but `SetPolicy` then `ApplyOnce`
  lets the caller stop any service in the profile's `stop_services`, write
  HKLM\WU pause keys (`windows_update.rs:104` `RegCreateKeyExW(HKEY_LOCAL_MACHINE, …)`),
  and switch power plans. The service trusts the wire profile completely
  — no schema bound on `stop_services` list, no allow-list intersection
  with the bundled `SafeList`. The `SafeList` is only used by the tray UI
  for hints, **not enforced server-side**. Combined with CRITICAL-1 this
  is the same "elevated client can amplify to LocalSystem scope" story:
  the service will gladly stop Windows Defender if asked.
- **Severity:** Critical (EoP, persistence).
- **Fix sketch:** Server-side intersect `profile.stop_services` and
  `profile.suspend_processes` against the bundled `SafeList` before
  acting; reject otherwise.

### HIGH-1 — Policy hot-reload trusts any writer of `C:\ProgramData\framesage\policy.json`
- **File:line:** `crates/service/src/runtime.rs:134-194` (watcher),
  `crates/core/src/paths.rs:18-46` (`config_dir`),
  `crates/core/src/policy.rs:271-297` (save).
- **Scenario:** The service watches the policy file and `engine.set_policy`
  on change. There is **no explicit ACL hardening** on the directory or
  file (full grep for `SetNamedSecurityInfo|SetFileSecurity|icacls` →
  nothing). The dir is created via `std::fs::create_dir_all`
  (`policy.rs:273`), which inherits whatever the parent has. `C:\ProgramData`'s
  default inherited DACL gives the "Users" group **`CREATE_FOLDER`** at
  the root, and once a folder is created by SYSTEM the inherited DACL
  is `(Users: Read & Execute) + (Authenticated Users: Modify on files
  they create) + (SYSTEM/Admin: Full)`. So if the directory does not
  pre-exist and the *first* writer is a non-admin user (e.g. someone
  runs `framesage-svc.exe --console` from their user account before
  install.ps1, or runs the sim binary), that user becomes the
  CREATOR_OWNER and retains **modify** on all subsequent files including
  `policy.json`. From then on, ANY file write to policy.json (matching
  any rule the user picks) causes `engine.set_policy(new_policy)` →
  arbitrary `Profile` → `apply_profile` → `OpenProcess` against
  attacker-named PIDs with SYSTEM rights. Full LocalSystem.
- **Severity:** Critical (TOCTOU-flavoured EoP). Promoted from High because
  the bootstrap order isn't enforced.
- **Fix sketch:** On service startup, call `SetNamedSecurityInfoW` on
  `config_dir()` to force `O:SY G:SY D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;AU)`
  (SYSTEM+Admin FullControl, Authenticated Users Read+Execute only), and
  refuse to load policy.json if its owner isn't SYSTEM/Administrators
  (`GetNamedSecurityInfo` → check owner SID).

### HIGH-2 — `Subscribe` on the status pipe enables status-pipe instance exhaustion DoS
- **File:line:** `crates/service/src/runtime.rs:582-589` (handler),
  `crates/ipc/src/lib.rs:159` (`Request::Subscribe` classified read-only),
  `crates/service/src/pipe.rs:116` (`PIPE_UNLIMITED_INSTANCES`).
- **Scenario:** `Subscribe` holds a pipe instance open indefinitely while
  streaming events. It's `is_read_only == true`, so any Authenticated
  User can issue it on the status pipe. Status pipe uses
  `PIPE_UNLIMITED_INSTANCES` (255 cap) — so an unprivileged user can
  spawn ~255 Subscribe clients, exhaust the kernel pipe-instance table,
  and prevent the legitimate tray's status traffic from connecting.
  Additionally, each subscribe holds a tokio task with a broadcast
  receiver — at 255 receivers the broadcast channel still works but
  CPU + memory grow.
- **Severity:** High (local DoS of the service's IPC plane).
- **Fix sketch:** Cap Subscribe-in-flight per caller PID (use
  `GetNamedPipeClientProcessId`), or bound total subscribers globally
  with a semaphore.

### HIGH-3 — `report_foreground` PID is trusted blindly; engine applies profile by exe_name string
- **File:line:** `crates/ipc/src/lib.rs:64-69`,
  `crates/engine/src/lib.rs:943-952`, with downstream usage in
  `apply_once` / reconcile path opening PIDs via `OpenProcess`.
- **Scenario:** `ReportForeground { pid, exe_name, path, title }` is a
  mutator, so admin-pipe-only — but the engine takes the strings verbatim
  and uses `pid` to drive `apply_profile(pid, &exe_name, …)` in
  `apply_once` (`engine/src/lib.rs:1021`). An elevated-but-compromised
  tray can claim "explorer.exe is foreground with PID = csrss" and
  cause the service to pin csrss's affinity / lower its priority class.
  Privilege amplification is bounded because the caller is already admin,
  but the service has LocalSystem reach that a plain admin doesn't. The
  engine does NOT cross-check that `exe_name` matches what
  `framesage_sys::process::exe_for_pid(pid)` returns.
- **Severity:** High (privilege amplification admin→LocalSystem on
  arbitrary live PIDs). Same family as CRITICAL-1.
- **Fix sketch:** Re-resolve exe name from PID server-side; reject if
  reported `exe_name` doesn't match.

### MEDIUM-1 — Status pipe leaks full process inventory + user names to any local user
- **File:line:** `crates/ipc/src/lib.rs:236-312` (`ProcessSnapshot`),
  `crates/service/src/runtime.rs:337-340` (`ListProcesses` allowed on
  status pipe).
- **Scenario:** Authenticated Users group on the status pipe → anyone
  can call `Request::ListProcesses` and harvest the full PID list,
  exe paths, `DOMAIN\username` ownership (`ProcessSnapshot::user`
  cached server-side), description / company strings, and live
  CPU/memory metrics. This is roughly what Task Manager shows, but it
  bypasses `SeDebugPrivilege` requirements for cross-session info
  because the service runs as SYSTEM and exposes the data over the wire.
  In particular, on a multi-user RDP host, a low-priv user can enumerate
  every other interactive user's processes plus their usernames. The
  full `Policy` is also returned in `Request::Status` — leaks the user's
  rule list (game names, etc.).
- **Severity:** Medium (info disclosure). Mitigated by "this data is
  already visible to anyone with WMI or perfmon access," but the
  pipe-ACL design has made it a one-call drive-by.
- **Fix sketch:** Strip `user` / `exe_path` from snapshots on the status
  pipe; only the admin pipe should expose them. Or move `ListProcesses`
  to the admin pipe entirely.

### MEDIUM-2 — No client identity check on the admin pipe beyond DACL
- **File:line:** `crates/service/src/pipe.rs:149-165` (admin pipe creation),
  `crates/service/src/runtime.rs:296-330` (handler).
- **Scenario:** The handler never calls `GetNamedPipeClientProcessId` or
  `ImpersonateNamedPipeClient`. Authentication is purely the kernel DACL
  on the pipe object. That's *probably* fine (the kernel enforces the
  DACL at `CreateFile` open time), but:
  - The actual DACL of the admin pipe is **whatever tokio's
    `ServerOptions::create` produces with `NULL` SECURITY_ATTRIBUTES**
    — which means the LocalSystem process token's `TokenDefaultDacl`,
    not the literal "Admins + SYSTEM" the comments claim. The
    LocalSystem default DACL grants World (`WD`) `GENERIC_READ |
    GENERIC_EXECUTE`. For a pipe that means non-admin users CAN open
    the handle for read — they just can't `FILE_WRITE_DATA` to send
    a request. Confirm by enumeration. If you ever decide to write
    unsolicited events on the admin pipe, this would leak.
  - The "comment vs reality" mismatch is worth fixing for auditor
    sanity: replace the implicit default with an explicit SDDL
    `D:(A;;GA;;;SY)(A;;GA;;;BA)` mirroring the status pipe pattern.
- **Severity:** Medium (defence-in-depth gap, currently not exploitable
  because the admin pipe only writes in response to a client write).
- **Fix sketch:** Use an explicit admin SDDL via the same `pipe.rs`
  helper. Add `GetNamedPipeClientProcessId` + token-elevation check
  as defense-in-depth and log peer PID for every admin operation.

### MEDIUM-3 — `Request::*` string fields are unbounded; potential service memory pressure
- **File:line:** `crates/ipc/src/lib.rs:64-69` (`ReportForeground`),
  `crates/ipc/src/lib.rs:146` (`DeleteAffinityRule`),
  `crates/ipc/src/lib.rs:43` (`SetPolicy` — full `Policy` blob).
- **Scenario:** newline-delimited JSON over the pipe; `BufReader::lines`
  (`runtime.rs:302`) will buffer the whole line. No `take`/length cap.
  An attacker on the admin pipe (or, for `ReportForeground`/`SetPolicy`
  they need admin anyway) could send a multi-GB single line and OOM
  the service. Lower-impact on the status pipe because only read-only
  requests accept big input — `ListProcesses` and `Status` carry no
  user payload. Still: `BufReader::lines` doesn't enforce a max line
  length and will allocate until exhaustion.
- **Severity:** Medium (DoS via memory exhaustion of LocalSystem process).
- **Fix sketch:** Use `AsyncBufReadExt::take` with a 1 MB cap, or a
  framed codec with `LengthDelimitedCodec::max_frame_length`.

### MEDIUM-4 — `BufReader::lines` strips no embedded nulls; serde would tolerate them but engine passes strings to Win32
- **File:line:** `crates/service/src/runtime.rs:304` (line read),
  `crates/sys/src/inner/process.rs` (used by `exe_for_pid`), and the
  affinity-rule `exe_name` comparison at `engine/src/lib.rs:493`.
- **Scenario:** A JSON string can contain `" "`. `serde_json` will
  pass that through as a `String` with an interior NUL. Nothing in the
  call graph converts these to `OsString`/`PCWSTR` (the affinity-rule
  matcher uses `eq_ignore_ascii_case` on the string only; not passed
  to a Win32 API), so I couldn't find an exploitable sink today.
  Worth a defensive check on `Request::SetAffinityRule.rule.exe_name`
  and `Request::ReportForeground.path` because both are persisted to
  policy.json and could later be consumed by something that does feed
  Win32 wide-string APIs.
- **Severity:** Medium (latent; no current sink found).
- **Fix sketch:** In the IPC handler, reject any string with embedded
  NUL or longer than 32 KB.

### LOW-1 — Service `binPath` is set via `windows_service` which quotes correctly, but the binary lives in `%LOCALAPPDATA%\Programs\FrameSage\` (per-user install dir)
- **File:line:** `install.ps1:44` `$installDir = Join-Path $env:LOCALAPPDATA "Programs\FrameSage"`,
  registered as the service `executable_path` (`crates/cli/src/main.rs:228-250`).
- **Scenario:** Installing a LocalSystem service whose binary lives in a
  **user-writable directory** is the classic "unquoted service path /
  weak binary ACL" anti-pattern dressed in a quoted form. Although
  `windows_service` quotes the path, the directory itself is
  `%LOCALAPPDATA%` of whoever ran `install.ps1` — Authenticated Users
  cannot reach another user's LOCALAPPDATA by default, but the *installing
  user* (who is admin only during the elevated install) retains
  modify on `framesage-svc.exe` afterward. From their normal medium-IL
  session they can replace `framesage-svc.exe` and wait for boot →
  arbitrary code as LocalSystem.
- **Severity:** High in practice (admin→SYSTEM persistence trivially) —
  the classic Windows EoP pattern. Flagging as LOW only because it
  requires the installing-user account; once that account is compromised
  the attacker can already replace the binary at next install. But the
  service shouldn't live there to start with.
- **Fix sketch:** Install to `%ProgramFiles%\FrameSage\` (system-wide,
  Admin-only writable) and never `%LOCALAPPDATA%`.

### LOW-2 — Binaries are unsigned
- **File:line:** `install.ps1` (no `signtool` invocation),
  `Cargo.toml` (no sign step).
- **Scenario:** No Authenticode signature on `framesage-svc.exe`,
  `framesage-tray.exe`, `framesage.exe`. Defender SmartScreen will
  warn on first run; tampering detection is nil; future Windows
  hardening (driver-signing-style requirements creeping into services)
  would block install.
- **Severity:** Low (no immediate EoP, just supply-chain hygiene gap).
- **Fix sketch:** Sign before ship; the README's pre-1.0 note already
  acknowledges this (`Cargo.toml:21-25`).

### LOW-3 — `install.ps1` self-elevates but does not verify it relaunched the original script (TOCTOU)
- **File:line:** `install.ps1:27-33`.
- **Scenario:** The script picks up `$MyInvocation.MyCommand.Path` and
  passes it to a fresh elevated PowerShell. Between the unprivileged
  check and the elevated `Start-Process`, a non-admin can replace the
  script file. Requires write access to wherever the user dropped
  install.ps1 (`%USERPROFILE%\Downloads` etc., where the user owns
  the file anyway), so the marginal escalation is tiny — but the
  pattern is the textbook script-elevation TOCTOU.
- **Severity:** Low.
- **Fix sketch:** Hash-pin the script content, or copy to a
  user-write-locked temp dir before relaunch.

### INFO-1 — Hot-reload of policy.json on close-write is racy with editors
- **File:line:** `crates/service/src/runtime.rs:178-191`.
- **Scenario:** Not security. Worth noting that `Policy::load` on a
  half-written file silently keeps the previous policy (`warn` log
  only); good behavior, no concern.

### INFO-2 — `parking_lot::RwLock` held across long IPC writes
- **File:line:** various `state.write()` calls in `engine/src/lib.rs`.
- **Not exploitable**, but a malicious admin client could stall the
  engine tick loop by holding a slow read. Out of scope for this audit.

---

## Summary

The two-pipe ACL split is a strong design and the `is_read_only`
server-side recheck is the right defense-in-depth. The biggest gaps
are at the **inner boundary**, not the outer one:

1. The admin pipe trusts its callers transitively as if they were
   LocalSystem (CRITICAL-1, CRITICAL-2, HIGH-3). Any admin-but-not-SYSTEM
   caller (legit tray after `runas`, or a future bug, or malware that
   already obtained admin) gains the service's broader reach.
2. The policy file's on-disk ACL is left to inherited defaults
   (HIGH-1). Anyone who plants a `policy.json` owns the service.
3. The service binary lives in a per-user dir (LOW-1) — admin-to-SYSTEM
   persistence is trivial via swap-and-reboot.

No telemetry, no DLL-search-order issues, no string→Win32 sinks
exploitable today, no signed-binary requirement met (acknowledged gap).
