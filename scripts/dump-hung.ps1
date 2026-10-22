# Capture a full dump of a hung LanBridge/win-shell process.
#
# Usage (when the window shows「未响应」):
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\dump-hung.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\dump-hung.ps1 -ProcessId 1234
#
# The dump lands in .tmp\hang-<timestamp>.dmp; it contains the stacks of every thread, which is
# what a debugger needs to say *where* the UI thread is stuck. Pair it with the log line whose
# "step=" field names the last known step.
[CmdletBinding()]
param(
    # NOT $Pid: that is a read-only automatic variable in PowerShell.
    [int]$ProcessId,
    [string]$Out
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot

if (-not $ProcessId) {
    $candidates = @(Get-Process -Name LanBridge, win-shell -ErrorAction SilentlyContinue)
    if ($candidates.Count -eq 0) { throw 'LanBridge is not running' }
    $ProcessId = ($candidates | Sort-Object StartTime -Descending | Select-Object -First 1).Id
}

$proc = Get-Process -Id $ProcessId -ErrorAction SilentlyContinue
if (-not $proc) { throw "no process with pid $ProcessId" }

if (-not $Out) {
    $dir = Join-Path $root '.tmp'
    $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
    $Out = Join-Path $dir "hang-$stamp.dmp"
}
$dirName = Split-Path -Parent $Out
New-Item -ItemType Directory -Path $dirName -Force | Out-Null
$Out = (Resolve-Path -LiteralPath $dirName).Path + '\' + (Split-Path -Leaf $Out)
if (Test-Path -LiteralPath $Out) { Remove-Item -LiteralPath $Out -Force }

Write-Host "[dump] pid=$ProcessId name=$($proc.ProcessName) responding=$($proc.Responding)"
Write-Host "[dump] target: $Out"

# comsvcs!MiniDump: same mechanism Task Manager uses for "Create dump file".
$rundll = Join-Path $env:SystemRoot 'System32\rundll32.exe'
& $rundll (Join-Path $env:SystemRoot 'System32\comsvcs.dll'), MiniDump $ProcessId $Out full

# rundll32 writes the file asynchronously and does not set an exit code we can trust.
$waited = 0
while (-not (Test-Path -LiteralPath $Out) -and $waited -lt 60) {
    Start-Sleep -Milliseconds 500
    $waited += 0.5
}

if (Test-Path -LiteralPath $Out) {
    $mb = [math]::Round((Get-Item -LiteralPath $Out).Length / 1MB, 1)
    Write-Host "[dump] ok: $Out ($mb MB)" -ForegroundColor Green
} else {
    Write-Host '[dump] failed: no file written (needs the same user session; try an elevated shell)' -ForegroundColor Red
}
