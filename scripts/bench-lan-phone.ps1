# Orchestrate dual-device LAN bench: phone Host + PC Guest.
# Requires: hdc device online, LanBridge app installed, same WLAN.
param(
    [string]$Device = "",
    [int]$FileMiB = 64,
    [int]$ProbeMiB = 8
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot "env.ps1")

function Get-HdcTarget {
    param([string]$Prefer)
    if ($Prefer) { return $Prefer }
    $lines = hdc list targets 2>$null | ForEach-Object { "$_".Trim() } | Where-Object { $_ -and $_ -notmatch 'COM\d' -and $_ -notmatch '^\[|UART' }
    $usb = $lines | Where-Object { $_ -match '^[0-9A-Za-z]+$' } | Select-Object -First 1
    if (-not $usb) { throw "no hdc USB device; connect phone and enable debugging" }
    return $usb
}

function Invoke-Hdc {
    param([string]$Target, [Parameter(ValueFromRemainingArguments = $true)][string[]]$Args)
    & hdc -t $Target @Args
}

function Get-LayoutTexts {
    param([string]$JsonPath)
    python -c @"
import json
from pathlib import Path
data=json.loads(Path(r'$($JsonPath.Replace('\','/'))').read_text(encoding='utf-8'))
def walk(n, out):
  if not isinstance(n, dict): return
  a=n.get('attributes') or {}
  t=a.get('type') or ''; text=a.get('text') or ''; hint=a.get('hint') or ''; b=a.get('bounds') or ''
  if text or hint: out.append((t, text, hint, b))
  for c in n.get('children') or []: walk(c, out)
out=[]; walk(data,out)
for row in out:
  print('|'.join(str(x) for x in row))
"@
}

function Parse-BoundsCenter([string]$bounds) {
    # [x1,y1][x2,y2]
    if ($bounds -notmatch '\[(\d+),(\d+)\]\[(\d+),(\d+)\]') { return $null }
    $x1 = [int]$Matches[1]; $y1 = [int]$Matches[2]; $x2 = [int]$Matches[3]; $y2 = [int]$Matches[4]
    return @{ X = [int](($x1 + $x2) / 2); Y = [int](($y1 + $y2) / 2) }
}

$Target = Get-HdcTarget -Prefer $Device
Write-Host "[bench] device=$Target"

# Ensure app foreground
Invoke-Hdc $Target shell "aa start -a EntryAbility -b com.lanbridge.ohosshell" | Out-Null
Start-Sleep -Seconds 2

$layoutPhone = Join-Path $Root ".tmp\bench_layout.json"
$layoutRemote = "/data/local/tmp/lb_bench_layout.json"

Invoke-Hdc $Target shell "uitest dumpLayout -p $layoutRemote" | Out-Null
Invoke-Hdc $Target file recv $layoutRemote $layoutPhone | Out-Null
$rows = Get-LayoutTexts $layoutPhone

function Find-Button([string]$label) {
    foreach ($line in $rows) {
        $p = $line.Split('|')
        if ($p.Length -ge 4 -and $p[0] -eq 'Button' -and $p[1] -eq $label) {
            return (Parse-BoundsCenter $p[3])
        }
    }
    return $null
}

$begin = Find-Button 'Host: begin listen'
if (-not $begin) { throw "Host: begin listen button not found; unlock phone and open LanBridge" }
Write-Host "[bench] tap Host begin at $($begin.X),$($begin.Y)"
Invoke-Hdc $Target shell "uitest uiInput click $($begin.X) $($begin.Y)" | Out-Null
Start-Sleep -Seconds 2

Invoke-Hdc $Target shell "uitest dumpLayout -p $layoutRemote" | Out-Null
Invoke-Hdc $Target file recv $layoutRemote $layoutPhone | Out-Null
$rows = Get-LayoutTexts $layoutPhone

$port = $null
$code = $null
$phoneIpHint = $null
foreach ($line in $rows) {
    $p = $line.Split('|')
    if ($p.Length -lt 2) { continue }
    if ($p[1] -match '^Guest: this phone LAN IP \+ port (\d+)$') {
        $port = [int]$Matches[1]
        $phoneIpHint = $p[1]
    }
    if ($p[0] -eq 'Text' -and $p[1] -match '^\d{6}$') {
        $code = $p[1]
    }
    if ($p[0] -eq 'TextInput' -and $p[2] -eq 'port' -and $p[1] -match '^\d+$') {
        $port = [int]$p[1]
    }
    if ($p[0] -eq 'TextInput' -and $p[2] -eq 'pairing code' -and $p[1] -match '^\d{6}$') {
        $code = $p[1]
    }
}
if (-not $port -or -not $code) {
    throw "failed to parse host port/code from UI. hint=$phoneIpHint"
}

# Phone WLAN IP (same subnet as PC)
$ifconfig = Invoke-Hdc $Target shell "ifconfig wlan0" | Out-String
if ($ifconfig -notmatch 'inet addr:(\d+\.\d+\.\d+\.\d+)') {
    throw "cannot parse phone wlan0 IP"
}
$phoneIp = $Matches[1]
Write-Host "[bench] phone=$phoneIp port=$port code=$code"

$wait = Find-Button 'Host: wait pair + recv'
# re-find after dump refresh
foreach ($line in $rows) {
    $p = $line.Split('|')
    if ($p.Length -ge 4 -and $p[0] -eq 'Button' -and $p[1] -eq 'Host: wait pair + recv') {
        $wait = Parse-BoundsCenter $p[3]
    }
}
if (-not $wait) { throw "Host: wait pair + recv not found" }

# Prepare payload
$blobDir = Join-Path $Root ".tmp\lan-bench"
New-Item -ItemType Directory -Force -Path $blobDir | Out-Null
$blob = Join-Path $blobDir ("blob-{0}m.bin" -f $FileMiB)
$bytes = [int64]$FileMiB * 1MB
Write-Host "[bench] writing $blob ($FileMiB MiB)…"
$fs = [System.IO.File]::Create($blob)
try {
    $chunk = New-Object byte[] (1MB)
    for ($i = 0; $i -lt $chunk.Length; $i++) { $chunk[$i] = [byte]($i % 251) }
    $left = $bytes
    while ($left -gt 0) {
        $n = [Math]::Min([int64]$chunk.Length, $left)
        $fs.Write($chunk, 0, [int]$n)
        $left -= $n
    }
} finally { $fs.Dispose() }

Push-Location (Join-Path $Root "src")
try {
    cargo build -p lb-core --example guest_lan_bench --release | Out-Null
} finally { Pop-Location }
$exe = Join-Path $Root "target\release\examples\guest_lan_bench.exe"
$report = Join-Path $Root "data\results\m4-lan-phone-bench.md"
$probeBytes = [int64]$ProbeMiB * 1MB

Write-Host "[bench] tap Host wait at $($wait.X),$($wait.Y)"
Invoke-Hdc $Target shell "uitest uiInput click $($wait.X) $($wait.Y)" | Out-Null
Start-Sleep -Milliseconds 500

Write-Host "[bench] guest_lan_bench -> ${phoneIp}:$port"
& $exe $phoneIp $port $code $blob $probeBytes $report
$codeExit = $LASTEXITCODE
Write-Host "[bench] exit=$codeExit report=$report"
if ($codeExit -ne 0) { exit $codeExit }
