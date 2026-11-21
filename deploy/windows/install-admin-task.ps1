# Starts Lottery Miner as administrator at every login, without a UAC prompt,
# through a scheduled task with "Run with highest privileges". Admin lets
# XMRig apply its MSR tweak (about 10-15% more on RandomX).
#
# Run once: right-click > Run with PowerShell, then approve the admin prompt.
# Undo: Unregister-ScheduledTask -TaskName 'Lottery Miner' (as admin).

param([string]$Exe = (Join-Path $PSScriptRoot '..\..\target\release\lottery-miner.exe'))

$Exe = [IO.Path]::GetFullPath($Exe)
$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) {
    Start-Process powershell.exe -Verb RunAs -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Exe', "`"$Exe`"")
    exit
}

$ErrorActionPreference = 'Stop'
$log = Join-Path $env:TEMP 'lottery-miner-admin-task.log'
Start-Transcript -Path $log -Force | Out-Null
trap { Write-Host "FAILED: $_"; Stop-Transcript | Out-Null; Read-Host 'Press Enter to close'; exit 1 }

if (-not (Test-Path $Exe)) { throw "Not found: $Exe" }

$action = New-ScheduledTaskAction -Execute $Exe -Argument '--hidden'
$trigger = New-ScheduledTaskTrigger -AtLogOn -User "$env:USERDOMAIN\$env:USERNAME"
$principal = New-ScheduledTaskPrincipal -UserId "$env:USERDOMAIN\$env:USERNAME" -LogonType Interactive -RunLevel Highest
# No 3-day time limit (the default would kill the app and its miners).
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew
Register-ScheduledTask -TaskName 'Lottery Miner' -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
$null = Get-ScheduledTask -TaskName 'Lottery Miner'

# The app's own non-admin login entry would start a second copy.
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
if (Get-ItemProperty -Path $run -Name 'LotteryMiner' -ErrorAction SilentlyContinue) {
    Remove-ItemProperty -Path $run -Name 'LotteryMiner'
}

Write-Host "Scheduled task 'Lottery Miner' created: $Exe --hidden, as administrator at login."
Stop-Transcript | Out-Null
Start-Sleep 3
