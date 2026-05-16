# Audit 10 — Install / Update / Uninstall Story

Scope: `install.ps1`, `crates/cli/src/main.rs` (install/uninstall/start/stop verbs),
`.github/workflows/ci.yml`, `.claude/deploy-persistent.ps1`, `.claude/FINISH-DEPLOY.bat`,
`crates/core/src/paths.rs`, `README.md`.

Verdict up front: **the uninstall story is broken**. `framesage uninstall` deletes the
SCM registration and nothing else. Every other side effect the installer creates —
binaries, three shortcuts (including the Startup-folder one that respawns the tray
on every login), `%ProgramData%\framesage\`, `policy.json`, the `game-mode.journal`
that can hold the system in a degraded state — is left behind.

---

## 1. What `install.ps1` actually changes

Persistent side effects, file:line in `install.ps1`:

- **Binaries copied** to `%LOCALAPPDATA%\Programs\FrameSage\` (`install.ps1:44, 77-85`):
  `framesage-tray.exe`, `framesage-svc.exe`, `framesage.exe`, `framesage-sim.exe`,
  plus `README.md` and `LICENSE`. Per-user install dir.
- **Three shortcuts** (`install.ps1:97-111`):
  - `%APPDATA%\Microsoft\Windows\Start Menu\Programs\FrameSage.lnk`
  - `Desktop\FrameSage.lnk`
  - `Startup\FrameSage.lnk` ← **this is autostart**. Tray spawns at every logon.
- **SCM service** `framesage` (`install.ps1:124`, executes `framesage.exe install`
  which lives in `crates/cli/src/main.rs:221-263`):
  - DisplayName: `framesage scheduler supervisor` (`main.rs:16`)
  - StartType: `AutoStart` (`main.rs:243`) — boots with the OS
  - Account: `LocalSystem` (`main.rs:248`)
  - Executable path: `%LOCALAPPDATA%\Programs\FrameSage\framesage-svc.exe`
- **`%ProgramData%\framesage\`** — not explicitly created by `install.ps1`; the
  service creates it lazily on first run (`crates/core/src/paths.rs:18-34`,
  `crates/service/src/runtime.rs:34, 504`). Contains `policy.json` and possibly
  `game-mode.journal` (`crates/gamemode/src/journal.rs:26, 86`).

No registry edits, no scheduled tasks, no firewall rules, no Add/Remove-Programs
entry, no Uninstall registry key. (Confirmed by grep — only firewall reference is
inside the safe-list JSON, not in any installer code.)

---

## 2. What `framesage uninstall` actually does — and doesn't

`crates/cli/src/main.rs:271-284`:

```rust
let service = manager.open_service(SERVICE_NAME, ServiceAccess::DELETE)?;
service.delete()?;
```

That is the entire uninstall. **Severity: critical.**

What survives:

| Left behind                                                | Source                                      | Severity |
|------------------------------------------------------------|---------------------------------------------|----------|
| `%LOCALAPPDATA%\Programs\FrameSage\*.exe` (4 binaries)     | `install.ps1:77-83`                         | High     |
| Start Menu shortcut                                        | `install.ps1:99`                            | Medium   |
| Desktop shortcut                                           | `install.ps1:100`                           | Medium   |
| **Startup-folder shortcut → tray respawns at every login** | `install.ps1:101`                           | **Critical** |
| `%ProgramData%\framesage\policy.json`                      | `crates/service/src/runtime.rs:432, 470, 504` | Medium |
| `%ProgramData%\framesage\game-mode.journal`                | `crates/gamemode/src/journal.rs:86`         | **High** |
| `%ProgramData%\framesage\` itself                          | `crates/core/src/paths.rs:33`               | Medium   |
| README.md / LICENSE copies in install dir                  | `install.ps1:84-85`                         | Low      |

The `game-mode.journal` leftover is the worst: if uninstall happens while a Game
Mode session is active, the journal documents stopped services / suspended
processes that the (now-deleted) service will never revert. The user is left with
a half-modified system and no tool to fix it. `cli/src/main.rs:200-207`
acknowledges the journal is authoritative; nothing reverts it on uninstall.

The Startup-folder shortcut is the next worst: a user who uninstalls the service
will see a tray icon respawn at logon, pointing at `framesage-tray.exe` which is
still present. The tray will then fail to connect to its named pipe and either
error or sit broken in the systray.

**Severity summary for §2: critical.** No uninstaller script exists. The CLI verb
is misleadingly named — it should be `unregister-service`.

---

## 3. Service install correctness

`crates/cli/src/main.rs:239-258`:

- `binPath` is passed via `windows-service` crate's `ServiceInfo.executable_path`
  (PathBuf). The crate quotes correctly. **OK.**
- `display_name` set. `set_description` called (`main.rs:256-258`). **OK.**
- `start_type: AutoStart`, `error_control: Normal`. **OK.**
- **Gap: no failure actions configured.** No `SERVICE_FAILURE_ACTIONS` /
  `RecoverServiceOnCrash`. If the service crashes, SCM won't restart it. For an
  always-on engine that's the whole point, this is a meaningful miss.
  **Severity: medium.**
- **Gap: no dependencies.** `dependencies: vec![]` (`main.rs:247`). The engine
  touches `Power Throttling`, CPU sets, possibly other power-related subsystems.
  At minimum a dependency on `RpcSs` would be conventional; absence is unlikely to
  break anything but is worth noting. **Severity: low.**
- **Gap: no `SERVICE_SID_TYPE_UNRESTRICTED`** or any hardening (running as
  `LocalSystem` with full token). **Severity: low** (defensible for the workload).

---

## 4. Service uninstall correctness

`crates/cli/src/main.rs:271-284` — opens with `ServiceAccess::DELETE` only and
calls `delete()`. **Does not stop the service first, does not wait for stop.**

- If the service is running, `DeleteService` flags it for deletion and the actual
  removal happens when the last handle closes, which can leave a `Marked for
  deletion` zombie that blocks re-install until reboot. `install.ps1:50-56`
  works around this by issuing `Stop-Service` first, but a bare
  `framesage.exe uninstall` invocation (which the README at line 134 recommends)
  doesn't. **Severity: high.**
- No force-kill if the service hangs in STOP_PENDING. **Severity: medium.**
- No retry/backoff. **Severity: low.**

---

## 5. Self-elevation flow

`install.ps1:25-33` is clean: detects non-admin, relaunches via `Start-Process
-Verb RunAs -Wait`, exits. One UAC prompt. Failure path: if the user declines
UAC, `Start-Process` throws and `$ErrorActionPreference = "Stop"` aborts with a
red error in the now-orphaned non-elevated window. Acceptable, not great.
**Severity: low.**

---

## 6. Update / upgrade story

There is **no update mechanism**. The user is expected to re-run `install.ps1`
(`README.md:104-119`).

- Re-running `install.ps1` correctly: stops service (`:50-56`), kills tray
  processes (`:57-62`), overwrites binaries (`:81`), uninstalls old service
  (`:120`), reinstalls (`:124`). **OK.**
- `policy.json` preservation: explicitly preserved (`install.ps1:130-143`). Good.
- **Gap: no policy schema migration.** If v0.5 adds a required field, the v0.4
  policy will fail to load. The service falls back to defaults
  (`crates/service/src/runtime.rs:497-517`) which silently discards user rules.
  **Severity: medium.**
- **Gap: orphaned old binaries.** If a future build adds a new exe (say,
  `framesage-updater.exe`), the install dir will contain old exes from the
  previous version that the new `install.ps1` doesn't know to delete. Not a
  problem today (no name changes yet); structural risk. **Severity: low.**
- **Gap: no version check.** Installing v0.4 over v0.5 silently downgrades.
  **Severity: low.**

---

## 7. Code signing

CI workflow `.github/workflows/ci.yml:64-81` produces unsigned artifacts. No
`signtool.exe`, no `AzureSignTool`, no EV cert, no signed-release pipeline.

- **First-run SmartScreen warning** is guaranteed. Users will see the blue
  "Windows protected your PC" dialog.
- No reproducible-build provenance, no SLSA attestation.
- **Severity: high** for a utility that asks for admin and installs a
  LocalSystem service. This is the single biggest "looks like malware" signal.

---

## 8. Installer format

PowerShell script. **Wrong choice for a public utility:**

- SmartScreen flags unsigned `.ps1` files harder than `.exe`.
- Corporate execution policies (`Restricted`, `AllSigned`) block `.ps1` by
  default. README explicitly tells users to `-ExecutionPolicy Bypass`
  (`README.md:109`), which is exactly what malware tutorials say.
- No Add/Remove Programs entry. No DisplayIcon, DisplayName, UninstallString.
- No silent-install support for IT admins (`/S`, `/qn`).
- Requires `cargo` in PATH (`install.ps1:68`) — **this is a source-tree
  installer, not a binary installer.** A user who downloaded a release zip
  cannot run it.

**Recommendation:** ship an Inno Setup `.iss` or WiX `.wxs` MSI. No such file
exists in the repo (confirmed via glob). **Severity: high.**

---

## 9. Install location — per-user dir for a LocalSystem service

`install.ps1:44` → `%LOCALAPPDATA%\Programs\FrameSage`. The service is registered
to load `framesage-svc.exe` from there (`main.rs:333-343`).

Concrete problems:

- **Service runs as `LocalSystem` but binary lives under one user's profile.**
  If user A installs and user B logs in, the service still runs A's binary. If A
  is deleted or their profile reset, the service fails to start at next boot
  (`STATUS_OBJECT_NAME_NOT_FOUND`). **Severity: high.**
- **ACL footgun:** `%LOCALAPPDATA%` is writable by the owning user.
  Theoretically the user can replace `framesage-svc.exe` with arbitrary code
  that SCM will then execute as LocalSystem on next start. This is a classic
  **privilege escalation primitive** if the user account is later compromised
  or shared. `install.ps1` does not tighten ACLs on the install dir.
  **Severity: high (security).**
- **Multi-user machines:** only the installing user gets the Start Menu /
  Desktop / Startup shortcuts (they go to `$env:APPDATA` and `[Environment]
  ::GetFolderPath('Startup')`, both per-user). Other users see a running
  service but no tray.

Right answer: install binaries to `%ProgramFiles%\FrameSage\`, ACL'd to
`Administrators:F SYSTEM:F Users:RX`. That requires MSI / Inno + UAC, and is
exactly why PowerShell is the wrong installer.

---

## 10. Tray autostart

Documented: yes, in `install.ps1:92-96` comments. Mechanism: **Startup folder
shortcut** (`install.ps1:101`). Not a Run-key, not a scheduled task.

- **Uninstall does not remove it** (see §2). **Severity: critical.**
- No "launch at logon" toggle in the tray UI that I can see — users who want to
  disable it must manually delete the .lnk.

---

## 11. Policy file location

`%ProgramData%\framesage\policy.json` (`crates/core/src/paths.rs:18-50`).

- Created lazily by the service on first run (`runtime.rs:504`), not by the
  installer.
- Migrated on upgrade: **no** (see §6). Preserved verbatim — fine if schema is
  stable, lossy otherwise.
- Preserved on uninstall: **yes, by accident** (uninstall doesn't touch it).
  This is actually the desired behaviour but it's not a deliberate choice — the
  uninstaller simply doesn't know the file exists.

---

## 12. Firewall / network

No `New-NetFirewallRule` anywhere (grep confirmed). IPC is named pipes
(`crates/cli/src/main.rs:349-354`), no TCP listener. **Nothing to clean up.
OK.**

---

## 13. Tray icon resource

Not bundled as a separate file. The shortcut points at `$trayExe,0`
(`install.ps1:106`), meaning the icon is embedded in `framesage-tray.exe` itself
as resource index 0 (presumably via a `build.rs` / `.rc` file — not audited
here). No external `.ico` to install or remove. **OK.**

---

## 14. Documentation gap (first-time user)

`README.md:102-145` covers install + remove but doesn't tell the user:

- That a service runs as LocalSystem at boot, autostart.
- That a tray respawns at every logon via a Startup shortcut.
- That `%ProgramData%\framesage\` is created and persists across uninstalls.
- That binaries live under their user profile (single-user install).
- That uninstall **only** removes the SCM service and **everything else must be
  cleaned by hand**. Not a single sentence about this.
- That binaries are unsigned and SmartScreen will warn.
- How to fully purge — there is no documented "complete removal" recipe.

**Severity: high** — for a utility that touches scheduler policy as
LocalSystem, the trust deficit from this omission is real.

---

## Top fixes, ranked

1. **Write a real uninstaller.** `framesage uninstall` should: stop service →
   wait → delete service → revert any active `game-mode.journal` →
   delete the three shortcuts → delete `%LOCALAPPDATA%\Programs\FrameSage\` →
   prompt about `%ProgramData%\framesage\`. (`crates/cli/src/main.rs:271-284`.)
2. **Stop-before-delete in the CLI verb**, not just in `install.ps1`. Wait for
   `STOPPED` with a timeout, force-kill the process as fallback.
3. **Ship a signed MSI** (WiX) or signed Inno Setup. Adds Add/Remove Programs
   entry, proper UninstallString, ACL'd `%ProgramFiles%` install, code-signed
   exes to dodge SmartScreen.
4. **Configure SCM failure actions** in `install_service`
   (`crates/cli/src/main.rs:239`) — restart on crash, 60s reset period.
5. **Document the uninstall residue** in README until #1 lands.
