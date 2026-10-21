# Bidirectional LAN 85% bench (PC↔phone). UTF-8 BOM for Windows PowerShell 5.1.
# Dir A (automated): PC guest_lan_bench → phone host (pair-wait).
# Dir B (semi-auto): paired shells; phone sends Download blob; pull last-send-bench.md.
param(
    [string]$Device = "",
    [int]$FileMiB = 64,
    [int]$ProbeMiB = 8,
    [switch]$SkipPhoneToPc,
    [int]$PhoneToPcWaitSec = 0
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot "env.ps1")

function Get-HdcTarget {
    param([string]$Prefer)
    if ($Prefer) { return $Prefer }
    $lines = hdc list targets 2>$null | ForEach-Object { "$_".Trim() } |
        Where-Object { $_ -and $_ -notmatch 'COM\d' -and $_ -notmatch '^\[|UART' }
    $usb = $lines | Where-Object { $_ -match '^[0-9A-Za-z]+$' } | Select-Object -First 1
    if (-not $usb) { throw "no hdc USB device" }
    return $usb
}

function Invoke-Hdc {
    param([string]$Target, [Parameter(ValueFromRemainingArguments = $true)][string[]]$Args)
    & hdc -t $Target @Args
}

function Get-LayoutTexts([string]$JsonPath) {
    $outTxt = Join-Path $Root ".tmp\bench_layout_rows.txt"
    $py = @"
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
lines=['|'.join(str(x).replace('|','/') for x in row) for row in out]
Path(r'$($outTxt.Replace('\','/'))').write_text('\n'.join(lines), encoding='utf-8')
"@
    python -c $py
    return @(Get-Content -LiteralPath $outTxt -Encoding UTF8)
}

function Parse-BoundsCenter([string]$bounds) {
    if ($bounds -notmatch '\[(\d+),(\d+)\]\[(\d+),(\d+)\]') { return $null }
    $x1 = [int]$Matches[1]; $y1 = [int]$Matches[2]; $x2 = [int]$Matches[3]; $y2 = [int]$Matches[4]
    return @{ X = [int](($x1 + $x2) / 2); Y = [int](($y1 + $y2) / 2) }
}

function Dump-Layout([string]$Target) {
    $layoutPhone = Join-Path $Root ".tmp\bench_layout.json"
    $layoutRemote = "/data/local/tmp/lb_bench_layout.json"
    Invoke-Hdc $Target shell "uitest dumpLayout -p $layoutRemote" | Out-Null
    Invoke-Hdc $Target file recv $layoutRemote $layoutPhone | Out-Null
    return ,(Get-LayoutTexts $layoutPhone)
}

function Find-Button($rows, [string]$label) {
    foreach ($line in $rows) {
        $p = $line.Split('|')
        if ($p.Length -ge 4 -and $p[0] -eq 'Button' -and $p[1] -eq $label) {
            return (Parse-BoundsCenter $p[3])
        }
    }
    return $null
}

function New-BenchBlob([int]$MiB) {
    $blobDir = Join-Path $Root ".tmp\lan-bench"
    New-Item -ItemType Directory -Force -Path $blobDir | Out-Null
    $blob = Join-Path $blobDir ("blob-{0}m.bin" -f $MiB)
    $bytes = [int64]$MiB * 1MB
    if ((Test-Path $blob) -and ((Get-Item $blob).Length -eq $bytes)) {
        return $blob
    }
    Write-Host "[bench] writing $blob ($MiB MiB)…"
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
    return $blob
}

$Target = Get-HdcTarget -Prefer $Device
$stamp = Get-Date -Format "yyyyMMdd-HHmm"
$resultsDir = Join-Path $Root "data\results"
New-Item -ItemType Directory -Force -Path $resultsDir | Out-Null
$reportA = Join-Path $resultsDir "m4-lan-pc2phone-$stamp.md"
$reportB = Join-Path $resultsDir "m4-lan-phone2pc-$stamp.md"
$reportSummary = Join-Path $resultsDir "m4-lan-bidir-$stamp.md"

Write-Host "[bench] device=$Target stamp=$stamp"

# --- Direction A: PC → phone ---
Invoke-Hdc $Target shell "aa force-stop com.lanbridge.ohosshell" | Out-Null
Start-Sleep -Seconds 1
Invoke-Hdc $Target shell "aa start -a EntryAbility -b com.lanbridge.ohosshell" | Out-Null
Start-Sleep -Seconds 3

$rows = Dump-Layout $Target
$clear = Find-Button $rows '清除配对'
if ($clear) {
    Write-Host "[bench] clear pair at $($clear.X),$($clear.Y)"
    Invoke-Hdc $Target shell "uitest uiInput click $($clear.X) $($clear.Y)" | Out-Null
    Start-Sleep -Seconds 2
    $rows = Dump-Layout $Target
}

# Ensure online (重新上线 if present)
$online = Find-Button $rows '重新上线'
if ($online) {
    Write-Host "[bench] go online at $($online.X),$($online.Y)"
    Invoke-Hdc $Target shell "uitest uiInput click $($online.X) $($online.Y)" | Out-Null
    Start-Sleep -Seconds 2
    $rows = Dump-Layout $Target
}

$code = $null
$phoneIp = $null
$port = 37532
foreach ($line in $rows) {
    $p = $line.Split('|')
    if ($p.Length -lt 2) { continue }
    if ($p[0] -eq 'Text' -and $p[1] -match '^\d{6}$') { $code = $p[1] }
    if ($p[1] -match '本机 IP：(\d+\.\d+\.\d+\.\d+)') { $phoneIp = $Matches[1] }
}
if (-not $phoneIp) {
    $ifconfig = Invoke-Hdc $Target shell "ifconfig wlan0" | Out-String
    if ($ifconfig -match 'inet addr:(\d+\.\d+\.\d+\.\d+)') { $phoneIp = $Matches[1] }
}
if (-not $code) { throw "pairing code not found on phone UI (unlock + open LanBridge unpaired)" }
if (-not $phoneIp) { throw "phone IP not found" }
Write-Host "[bench] A: phone=$phoneIp port=$port code=$code"

# Receive dir must be ready or serve_once never arms (destReady=false).
$recvBtn = Find-Button $rows '接收位置'
if ($recvBtn) {
    Write-Host "[bench] tap 接收位置 to ensure DOWNLOAD dest"
    Invoke-Hdc $Target shell "uitest uiInput click $($recvBtn.X) $($recvBtn.Y)" | Out-Null
    Start-Sleep -Seconds 2
}

# Avoid Win shell racing the phone control port / TLS after pair.
Get-Process -Name "win-shell" -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Seconds 1

$blob = New-BenchBlob -MiB $FileMiB
Push-Location (Join-Path $Root "src")
try {
    cargo build -p lb-core --example guest_lan_bench --release
} finally { Pop-Location }
$exe = Join-Path $Root "target\release\examples\guest_lan_bench.exe"
$probeBytes = [int64]$ProbeMiB * 1MB

Write-Host "[bench] A: guest_lan_bench → phone (wait 2s for pair-wait arm)"
Start-Sleep -Seconds 2
& $exe $phoneIp $port $code $blob $probeBytes $reportA
if ($LASTEXITCODE -ne 0) { throw "guest_lan_bench failed rc=$LASTEXITCODE" }
Write-Host "[bench] A ok → $reportA"

if ($SkipPhoneToPc) {
    Write-Host "[bench] skip phone→PC"
    exit 0
}

# --- Direction B: phone → PC (semi-auto) ---
Write-Host ""
Write-Host "[bench] B: ensure Win shell + phone are paired and listening."
Write-Host "[bench] Pushing blob to phone Download as lb-bench-${FileMiB}m.bin"
# HarmonyOS Docs/Download (not Android /sdcard/Download).
$remoteBlob = "/storage/media/100/local/files/Docs/Download/lb-bench-${FileMiB}m.bin"
Invoke-Hdc $Target file send $blob $remoteBlob | Out-Null
$benchMdRemote = "/data/app/el2/100/base/com.lanbridge.ohosshell/haps/entry/files/lb-session/last-send-bench.md"

Write-Host ""
Write-Host ">>> 请在手机 LanBridge：添加文件 → 选「下载」里的 lb-bench-${FileMiB}m.bin → 发送（须已与 PC 配对）"
$pullLocal = Join-Path $Root ".tmp\last-send-bench.md"
Remove-Item $pullLocal -ErrorAction SilentlyContinue
$found = $null
if ($PhoneToPcWaitSec -gt 0) {
    Write-Host "[bench] polling last-send-bench.md for ${PhoneToPcWaitSec}s…"
    $deadline = (Get-Date).AddSeconds($PhoneToPcWaitSec)
    while ((Get-Date) -lt $deadline) {
        $ls = Invoke-Hdc $Target shell "ls '$benchMdRemote'" 2>$null | Out-String
        if ($ls -match 'last-send-bench\.md') { $found = $benchMdRemote; break }
        Start-Sleep -Seconds 5
    }
} else {
    Write-Host ">>> 发送完成后回到这里按 Enter"
    $null = Read-Host
    $ls = Invoke-Hdc $Target shell "ls '$benchMdRemote'" 2>$null | Out-String
    if ($ls -match 'last-send-bench\.md') { $found = $benchMdRemote }
}
if (-not $found) {
    Write-Host "[bench] WARN: last-send-bench.md not found — OH 需含 last_send 指标的新包，且发送已完成"
} else {
    Write-Host "[bench] pull $found"
    Invoke-Hdc $Target file recv $found $pullLocal | Out-Null
    Copy-Item $pullLocal $reportB -Force
    Write-Host "[bench] B ok → $reportB"
}

# Summary
$aText = if (Test-Path $reportA) { Get-Content $reportA -Raw -Encoding UTF8 } else { "(missing)" }
$bText = if (Test-Path $reportB) { Get-Content $reportB -Raw -Encoding UTF8 } else { "(missing — phone send metrics not pulled)" }
$summary = @"
# M4 LAN bidirectional bench

- stamp: $stamp
- file_mib: $FileMiB
- probe_mib_pc2phone: $ProbeMiB
- note: phone→PC probe size is shell DEFAULT_PROBE_BYTES (1 MiB) unless rebuilt otherwise

## A · PC → phone

$aText

## B · phone → PC

$bText
"@
[System.IO.File]::WriteAllText($reportSummary, $summary, [System.Text.UTF8Encoding]::new($false))
Write-Host "[bench] summary → $reportSummary"
