# LanBridge HarmonyOS log streamer (hdc + hilog).
#
# Usage (from repo root):
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\ohos-log.ps1                 # LanBridge-related lines only
#   ... -File .\scripts\ohos-log.ps1 -Clear                                                    # clear the device buffer first
#   ... -File .\scripts\ohos-log.ps1 -All                                                      # everything (no tag filter)
#   ... -File .\scripts\ohos-log.ps1 -Level E                                                  # only Error and above
#   ... -File .\scripts\ohos-log.ps1 -Save                                                     # also append to data\results\logs\hilog-*.txt
#   ... -File .\scripts\ohos-log.ps1 -Crash                                                    # pull the latest crash files instead of streaming
#   Ctrl+C stops streaming (hdc itself keeps running on the device side; harmless).
[CmdletBinding()]
param(
    [switch]$Clear,
    [switch]$All,
    [switch]$Save,
    [switch]$Crash,
    [ValidateSet('D', 'I', 'W', 'E', 'F')][string]$Level = 'D',
    [string]$Device = ''
)

$ErrorActionPreference = 'Stop'
# hilog carries UTF-8 (Chinese UI strings); keep the console from mangling it.
try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false) } catch { }
$OutputEncoding = New-Object System.Text.UTF8Encoding($false)
. "$PSScriptRoot\env.ps1" -Quiet

$root = Get-LanBridgeRoot
$deveco = Get-DevEcoHome
if (-not $deveco) { throw 'DevEco Studio not found (expected D:\HUAWEI_DEV_ECO).' }
$hdc = (Get-DevEcoTools $deveco).Hdc
if (-not $hdc) { throw "hdc not found under $deveco" }

$targets = @(& $hdc list targets 2>&1 | ForEach-Object { "$_".Trim() } | Where-Object { $_ -and $_ -ne '[Empty]' })
if ($targets.Count -eq 0) { throw 'no device: enable USB debugging, then check "hdc list targets"' }
$dev = $targets[0]
if ($Device) { $dev = $Device }

if ($Crash) {
    $outDir = Join-Path $root 'data\results\logs'
    if (-not (Test-Path -LiteralPath $outDir)) { New-Item -ItemType Directory -Path $outDir -Force | Out-Null }
    $list = & $hdc -t $dev shell "ls -t /data/log/faultlog/faultlogger | head -3" 2>&1
    Write-Host "==> latest fault logs on $dev" -ForegroundColor Cyan
    foreach ($name in ($list -split "`n")) {
        $n = "$name".Trim()
        if (-not $n) { continue }
        $local = Join-Path $outDir $n
        & $hdc -t $dev file recv "/data/log/faultlog/faultlogger/$n" $local 2>&1 | ForEach-Object { Write-Host $_ }
        Write-Host "   -> $local"
    }
    exit 0
}

if ($Clear) {
    & $hdc -t $dev shell hilog -r | Out-Null
    Write-Host '[log] device buffer cleared' -ForegroundColor Yellow
}

$savePath = $null
if ($Save) {
    $outDir = Join-Path $root 'data\results\logs'
    if (-not (Test-Path -LiteralPath $outDir)) { New-Item -ItemType Directory -Path $outDir -Force | Out-Null }
    $savePath = Join-Path $outDir ("hilog-" + (Get-Date -Format 'yyyyMMdd-HHmmss') + ".txt")
    Write-Host "[log] saving to $savePath" -ForegroundColor Yellow
}

# Default filter: our app tag, the Rust/NAPI bridge, and the usual crash markers.
$pattern = 'LanBridge|lanbridge|liblb|lblib|lb_ffi|lb-ffi|JSAPP|CppCrash|APP_INPUT_BLOCK|jscrash'
if ($All) { $pattern = '.' }

Write-Host "[log] device=$dev level=$Level  (Ctrl+C to stop)" -ForegroundColor Cyan

$utf8 = New-Object System.Text.UTF8Encoding($false)
& $hdc -t $dev shell hilog -L $Level 2>&1 | ForEach-Object {
    $line = "$_"
    if ($line -match $pattern) {
        Write-Host $line
        if ($savePath) { [System.IO.File]::AppendAllText($savePath, $line + "`r`n", $utf8) }
    }
}
