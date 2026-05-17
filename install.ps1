# FrameSage one-shot installer / updater.
#
# Right-click this file -> "Run with PowerShell" (it self-elevates).
# Or from a normal PowerShell: `powershell -ExecutionPolicy Bypass -File .\install.ps1`
#
# Steps:
#   1. Self-elevates via UAC if not already admin.
#   2. Kills the running tray + any console-mode service.
#   3. Builds release binaries.
#   4. Copies framesage-{tray,svc,sim}.exe + framesage.exe to
#      %LOCALAPPDATA%\Programs\FrameSage\.
#   5. Creates/updates Start Menu + Desktop shortcuts.
#   6. Uninstalls any previous SCM service, re-installs the new svc.exe
#      as LocalSystem (autostart on boot), starts it.
#   7. Deletes the stale C:\ProgramData\framesage\policy.json so the
#      service writes a fresh one with the latest defaults.
#   8. Launches the tray.

$ErrorActionPreference = "Stop"

# ASCII-only output so this script survives PowerShell 5.1 reading the
# source file as Windows-1252 instead of UTF-8.

# --- Self-elevate ------------------------------------------------------------
$currentPrincipal = New-Object Security.Principal.WindowsPrincipal(
    [Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $currentPrincipal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host "[install] not running as admin -- relaunching elevated..." -ForegroundColor Yellow
    $scriptPath = $MyInvocation.MyCommand.Path
    $argList = "-NoProfile -ExecutionPolicy Bypass -File `"$scriptPath`""
    Start-Process -FilePath "powershell.exe" -Verb RunAs -ArgumentList $argList -Wait
    exit
}

Write-Host ""
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host "  FrameSage one-shot installer / updater" -ForegroundColor Cyan
Write-Host "============================================================" -ForegroundColor Cyan
Write-Host ""

$repoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $repoRoot
Write-Host "[install] repo:    $repoRoot"

# Item 1.6 / audit C-10. Install dir is now %ProgramFiles%\FrameSage —
# system-wide, Administrators+SYSTEM only, NOT in any user's profile.
# The previous %LOCALAPPDATA%\Programs\FrameSage location was a classic
# admin-to-SYSTEM persistence primitive: SCM ran framesage-svc.exe as
# LocalSystem from a directory the installing user could write to.
# Compromise the user account → swap the binary → arbitrary code as
# SYSTEM at next boot.
$installDir = Join-Path $env:ProgramFiles "FrameSage"
Write-Host "[install] target:  $installDir"

# Migration: if the legacy per-user install dir exists, we'll move
# binaries to the new system-wide location and then clean it up. This
# preserves existing config (policy.json + sessions.jsonl live in
# %ProgramData%\framesage\, untouched by install location).
$legacyInstallDir = Join-Path $env:LOCALAPPDATA "Programs\FrameSage"
if (Test-Path $legacyInstallDir) {
    Write-Host "[install] legacy install detected at $legacyInstallDir -- will migrate" -ForegroundColor Yellow
}
Write-Host ""

# --- Stop anything running ---------------------------------------------------
Write-Host "[install] stopping any running FrameSage processes / service..." -ForegroundColor Cyan
$svc = Get-Service framesage -ErrorAction SilentlyContinue
if ($null -ne $svc) {
    if ($svc.Status -eq 'Running') {
        Stop-Service framesage -Force
        Start-Sleep -Seconds 2
    }
}
Get-Process framesage-svc, framesage-tray, framesage, framesage-sim `
    -ErrorAction SilentlyContinue |
    ForEach-Object {
        Write-Host "  killing $($_.ProcessName) PID $($_.Id)"
        Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
    }
Start-Sleep -Seconds 1

# --- Build release -----------------------------------------------------------
Write-Host ""
Write-Host "[install] building release (cargo build --release --workspace)..." -ForegroundColor Cyan
& "cargo" build --release --workspace
if ($LASTEXITCODE -ne 0) {
    Write-Host "[install] cargo build failed -- aborting" -ForegroundColor Red
    exit 1
}

# --- Stage binaries ----------------------------------------------------------
Write-Host ""
Write-Host "[install] staging binaries to $installDir..." -ForegroundColor Cyan
New-Item -ItemType Directory -Path $installDir -Force | Out-Null
foreach ($exe in @("framesage-tray.exe", "framesage-svc.exe", "framesage.exe", "framesage-sim.exe")) {
    $src = Join-Path $repoRoot "target\release\$exe"
    $dst = Join-Path $installDir $exe
    Copy-Item -Force $src $dst
    Write-Host "  copied $exe"
}
Copy-Item -Force (Join-Path $repoRoot "README.md") (Join-Path $installDir "README.md")
Copy-Item -Force (Join-Path $repoRoot "LICENSE") (Join-Path $installDir "LICENSE")

# --- Harden install dir ACL --------------------------------------------------
# Item 1.6 / audit C-10. %ProgramFiles% already has a sane default ACL
# (Administrators+SYSTEM full, Users read+execute), but we set it
# explicitly so a misconfigured parent doesn't leak modify rights to
# the user. Inheritance is preserved from %ProgramFiles% — we don't
# need PROTECTED here because Windows defaults already block user
# writes to %ProgramFiles%.
Write-Host ""
Write-Host "[install] hardening install dir ACL..." -ForegroundColor Cyan
$icaclsArgs = @(
    $installDir,
    '/inheritance:r',
    '/grant:r', 'NT AUTHORITY\SYSTEM:(OI)(CI)F',
    '/grant:r', 'BUILTIN\Administrators:(OI)(CI)F',
    '/grant:r', 'BUILTIN\Users:(OI)(CI)RX'
)
$icaclsOut = & icacls @icaclsArgs 2>&1
if ($LASTEXITCODE -ne 0) {
    Write-Host "  warning: icacls failed -- install dir may have weaker permissions" -ForegroundColor Yellow
    Write-Host "  $icaclsOut"
} else {
    Write-Host "  SYSTEM:F  Administrators:F  Users:RX"
}

# --- Legacy install cleanup --------------------------------------------------
# If we migrated from %LOCALAPPDATA%, remove the old dir + binaries
# now that the new install is in place. Shortcuts get rebuilt below
# pointing at the new location.
if (Test-Path $legacyInstallDir) {
    Write-Host ""
    Write-Host "[install] removing legacy install dir at $legacyInstallDir..." -ForegroundColor Cyan
    try {
        Remove-Item -Recurse -Force $legacyInstallDir -ErrorAction Stop
        Write-Host "  done"
    } catch {
        Write-Host "  warning: legacy dir cleanup failed: $_" -ForegroundColor Yellow
        Write-Host "  remove manually: Remove-Item -Recurse -Force '$legacyInstallDir'"
    }
}

# --- Shortcuts ---------------------------------------------------------------
Write-Host ""
Write-Host "[install] creating shortcuts..." -ForegroundColor Cyan
$trayExe = Join-Path $installDir "framesage-tray.exe"
$shell = New-Object -ComObject WScript.Shell
# Per-user Startup folder is critical: the tray is the user-session
# foreground reporter (the LocalSystem service can't see the foreground
# from session 0), so it must auto-launch on logon for the engine to
# actually do anything. Without this, foreground tracking only works
# while the user manually keeps the tray open.
$startupFolder = [Environment]::GetFolderPath('Startup')
$lnkPaths = @(
    (Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\FrameSage.lnk"),
    (Join-Path ([Environment]::GetFolderPath('Desktop')) "FrameSage.lnk"),
    (Join-Path $startupFolder "FrameSage.lnk")
)
foreach ($lnkPath in $lnkPaths) {
    $lnk = $shell.CreateShortcut($lnkPath)
    $lnk.TargetPath = $trayExe
    $lnk.IconLocation = "$trayExe,0"
    $lnk.WorkingDirectory = $installDir
    $lnk.Description = "FrameSage - Windows scheduler supervisor"
    $lnk.Save()
    Write-Host "  $lnkPath"
}

# --- Service: unregister old (SCM-only), install new, start ------------------
#
# We do the unregister via sc.exe directly, NOT via `framesage uninstall`,
# because the full `framesage uninstall` is a clean-slate removal — it
# deletes binaries, shortcuts, and the install dir, which would wipe out
# the binaries we just staged above. install.ps1 owns the binary lifecycle
# (we copied them in this run, we'll copy them again next run); the only
# thing we need from the SCM here is "forget the old service registration
# so we can register the new one cleanly".
Write-Host ""
Write-Host "[install] (re-)registering the framesage service..." -ForegroundColor Cyan
$cli = Join-Path $installDir "framesage.exe"
$svc = Get-Service framesage -ErrorAction SilentlyContinue
if ($null -ne $svc) {
    Write-Host "  unregistering existing SCM service (stop + delete)"
    # Stop first — `sc.exe delete` on a running service marks it
    # "deletion pending" and won't remove the registration until every
    # handle closes, blocking the re-install. We swallow stop failures
    # because the service may already be stopped (race with the kill
    # step at the top of this script).
    & sc.exe stop framesage | Out-Null
    Start-Sleep -Seconds 1
    & sc.exe delete framesage | Out-Null
    # Poll briefly for SCM to actually drop the registration before we
    # try to create one with the same name.
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline) {
        if ($null -eq (Get-Service framesage -ErrorAction SilentlyContinue)) { break }
        Start-Sleep -Milliseconds 250
    }
}
Write-Host "  installing service (LocalSystem, autostart on boot)"
& $cli install
if ($LASTEXITCODE -ne 0) {
    Write-Host "[install] framesage install failed" -ForegroundColor Red
    exit 1
}

# --- Preserve existing policy ------------------------------------------------
# CRITICAL: never delete policy.json on update. It contains the user's rules,
# profile customisations, and manual edits. The service will load whatever is
# there; if the file is missing it bootstraps a fresh default policy on its
# own. We only inform here; we don't touch the file. To intentionally wipe
# the policy, the user runs `Remove-Item C:\ProgramData\framesage\policy.json`
# themselves before re-installing.
$policyPath = "C:\ProgramData\framesage\policy.json"
if (Test-Path $policyPath) {
    Write-Host ""
    Write-Host "[install] keeping existing policy.json (your rules + profile edits)" -ForegroundColor Cyan
    Write-Host "  $policyPath"
    Write-Host "  delete it manually before re-running install.ps1 if you want fresh defaults"
}

Write-Host ""
Write-Host "[install] starting service..." -ForegroundColor Cyan
& $cli start
if ($LASTEXITCODE -ne 0) {
    Write-Host "[install] framesage start failed" -ForegroundColor Red
    exit 1
}

# --- Launch the tray ---------------------------------------------------------
Write-Host ""
Write-Host "[install] launching tray..." -ForegroundColor Cyan
Start-Process -FilePath $trayExe

Write-Host ""
Write-Host "============================================================" -ForegroundColor Green
Write-Host "  Done. Service is elevated; admin actions now actually work." -ForegroundColor Green
Write-Host "============================================================" -ForegroundColor Green
Write-Host ""
Write-Host "  Launch later: Win -> type 'FrameSage' -> Enter"
Write-Host "  Or: $trayExe"
Write-Host ""
