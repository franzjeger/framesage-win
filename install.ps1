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
$installDir = Join-Path $env:LOCALAPPDATA "Programs\FrameSage"
Write-Host "[install] target:  $installDir"
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

# --- Service: uninstall old, install new, start ------------------------------
Write-Host ""
Write-Host "[install] (re-)registering the framesage service..." -ForegroundColor Cyan
$cli = Join-Path $installDir "framesage.exe"
$svc = Get-Service framesage -ErrorAction SilentlyContinue
if ($null -ne $svc) {
    Write-Host "  uninstalling existing service"
    & $cli uninstall
    Start-Sleep -Seconds 1
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
