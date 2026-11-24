#Requires -RunAsAdministrator
# Copies the release build to Program Files, where only administrators can replace it, and
# registers a task that starts it elevated at logon without a UAC prompt. Run it again after
# every rebuild, from the account that plays the games, and not while a watched game is running.
$ErrorActionPreference = 'Stop'

$source = Join-Path $PSScriptRoot 'target\release\hdrify.exe'
if (-not (Test-Path $source)) { throw 'No release build found. Run "cargo build --release" first.' }
$folder = Join-Path $env:ProgramFiles 'HDRify'
$exe = Join-Path $folder 'hdrify.exe'

# A running copy locks the exe.
Stop-ScheduledTask -TaskName HDRify -ErrorAction SilentlyContinue
Get-Process hdrify -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1

New-Item -ItemType Directory -Force $folder | Out-Null
Copy-Item $source $exe -Force

$user = [Security.Principal.WindowsIdentity]::GetCurrent().Name
# Without a zero time limit Task Scheduler ends the task after 3 days. Without priority 4 a
# task runs below normal, with low disk and memory priority, and HDR would come on late.
$settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -Priority 4 -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName HDRify -Force `
    -Action (New-ScheduledTaskAction -Execute $exe) `
    -Trigger (New-ScheduledTaskTrigger -AtLogOn -User $user) `
    -Principal (New-ScheduledTaskPrincipal -UserId $user -RunLevel Highest) `
    -Settings $settings | Out-Null

Start-ScheduledTask -TaskName HDRify
"HDRify installed to $exe and started. It starts at logon for $user."
