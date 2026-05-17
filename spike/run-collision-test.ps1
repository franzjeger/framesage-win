$spike = 'F:\Projects\framesage-win\peaceful-mayer-4d5448\target\release\spike-etw.exe'
$spikeOut = "$env:TEMP\spike-etw-bg.txt"
Remove-Item $spikeOut -Force -ErrorAction SilentlyContinue

Write-Output '=== launching spike in background (30s) ==='
$p = Start-Process -FilePath $spike -ArgumentList '--duration 30' `
    -PassThru -RedirectStandardOutput $spikeOut -NoNewWindow
Write-Output "spike pid: $($p.Id)"
Start-Sleep -Seconds 5

Write-Output ''
Write-Output '=== logman query -ets (filtered for our session + headers) ==='
$ets = logman query -ets 2>&1
$ets | Where-Object { $_ -match 'Frame|^Data|^Provider|^----' }
Write-Output ''
Write-Output '=== logman query FramesageEtwSpike -ets (full detail) ==='
logman query FramesageEtwSpike -ets 2>&1

Write-Output ''
Write-Output '=== conflict test: start a SECOND session with same name ==='
$conflict = logman start FramesageEtwSpike -p 'Microsoft-Windows-Kernel-Process' -ets 2>&1
$conflict
Write-Output "exit code: $LASTEXITCODE"

Write-Output ''
Write-Output '=== waiting for spike to finish ==='
$p.WaitForExit()
Write-Output 'spike done'

Write-Output ''
Write-Output '=== spike stdout (final summary) ==='
if (Test-Path $spikeOut) { Get-Content $spikeOut -Raw } else { '(no stdout)' }
