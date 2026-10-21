# LanBridge one-window dev session: PC shell + phone logs in the SAME terminal.
#
#   .\scripts\start.ps1              # PC shell + phone logs (phone auto-detected)
#   .\scripts\start.ps1 -WinOnly     # PC shell only (fastest path, cleanest Ctrl+C)
#   .\scripts\start.ps1 -PhoneOnly   # phone logs only
#   .\scripts\start.ps1 -Install     # build + install + launch phone app, then PC shell + logs
#   .\scripts\start.ps1 -Phone       # force phone stream even if no device is detected yet
#
# Exit behaviour: Ctrl+C (or closing the window) stops the PC shell, the hilog stream and every
# child cargo/node process. Nothing is left running in the background.
[CmdletBinding()]
param(
    [switch]$WinOnly,
    [switch]$PhoneOnly,
    [switch]$Phone,
    [switch]$Install
)

$ErrorActionPreference = 'Continue'
try { [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false) } catch { }
$OutputEncoding = New-Object System.Text.UTF8Encoding($false)

. "$PSScriptRoot\env.ps1" -Quiet
$root = Get-LanBridgeRoot
$winDir = Join-Path $root 'apps\win-shell'

$deveco = Get-DevEcoHome
$hdc = $null
if ($deveco) { $hdc = (Get-DevEcoTools $deveco).Hdc }

# ---- decide what to start -------------------------------------------------
$wantWin = -not $PhoneOnly
$wantPhone = -not $WinOnly
if ($Phone -or $Install) { $wantPhone = $true }

$dev = ''
if ($wantPhone) {
    if (-not $hdc) {
        Write-Host '[start] DevEco/hdc not found -> phone stream disabled' -ForegroundColor Yellow
        $wantPhone = $false
    } else {
        $targets = @(& $hdc list targets 2>&1 | ForEach-Object { "$_".Trim() } | Where-Object { $_ -and $_ -ne '[Empty]' })
        if ($targets.Count -gt 0) {
            $dev = $targets[0]
        } elseif (-not ($Phone -or $Install)) {
            Write-Host '[start] no phone detected -> PC shell only (plug in, or use -Install)' -ForegroundColor Yellow
            $wantPhone = $false
        }
    }
}

if (-not $wantWin -and -not $wantPhone) { throw 'nothing to start' }

if ($Install) {
    Write-Host '==> build + install + launch on phone' -ForegroundColor Cyan
    & (Join-Path $PSScriptRoot 'ohos-hap.ps1') -Install -Launch
    if ($LASTEXITCODE -ne 0) { throw "ohos-hap.ps1 failed (exit $LASTEXITCODE)" }
}

# ---- single target: run it directly (no relay, best signal handling) ------
if ($wantWin -and -not $wantPhone) {
    Set-MsvcEnv | Out-Null
    Write-Host '[start] PC shell only - Ctrl+C stops everything' -ForegroundColor Green
    Push-Location $winDir
    try { & cmd.exe /c 'npm run tauri dev' }
    finally { Pop-Location }
    exit $LASTEXITCODE
}

if ($wantPhone -and -not $wantWin) {
    $logArgs = @()
    if ($dev) { $logArgs += @('-Device', $dev) }
    & (Join-Path $PSScriptRoot 'ohos-log.ps1') -Clear @logArgs
    exit $LASTEXITCODE
}

# ---- both: relay every stream into this window, prefixed ------------------
$runDir = Join-Path $root ('.tmp\dev\' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Path $runDir -Force | Out-Null
Set-MsvcEnv | Out-Null

# A previous session that was killed abruptly can still hold the vite port; fail fast with advice.
$busy = Get-NetTCPConnection -LocalPort 1420 -State Listen -ErrorAction SilentlyContinue
if ($busy) {
    Write-Host '[start] port 1420 is already in use - a previous dev session is still running.' -ForegroundColor Red
    Write-Host '[start] free it:  Get-NetTCPConnection -LocalPort 1420 | ForEach-Object { Stop-Process -Id $_.OwningProcess -Force }' -ForegroundColor DarkGray
    exit 1
}

Write-Host ('[start] PC shell + phone logs  (device: ' + $(if ($dev) { $dev } else { 'none - waiting' }) + ')') -ForegroundColor Green
Write-Host '[start] Ctrl+C stops both' -ForegroundColor DarkGray

$winOut = Join-Path $runDir 'win.out.log'
$winErr = Join-Path $runDir 'win.err.log'
$ohOut = Join-Path $runDir 'ohos.out.log'
$ohErr = Join-Path $runDir 'ohos.err.log'

# A generated .cmd wrapper keeps quoting deterministic and lets cmd do the redirection.
# (Start-Process is avoided: PS 5.1 fails on machines that define both http_proxy and HTTP_PROXY.)
function New-Runner {
    param([string]$Name, [string]$WorkDir, [string]$CommandLine, [string]$OutFile, [string]$ErrFile)
    $path = Join-Path $runDir ($Name + '.cmd')
    # NOTE: comma binds tighter than '+', so each concatenation needs its own parentheses.
    $lines = @(
        '@echo off',
        ('cd /d "' + $WorkDir + '"'),
        ($CommandLine + ' 1>"' + $OutFile + '" 2>"' + $ErrFile + '"')
    )
    [System.IO.File]::WriteAllLines($path, $lines, (New-Object System.Text.UTF8Encoding($false)))
    return $path
}

function Start-Runner {
    param([string]$RunnerPath)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = 'cmd.exe'
    $psi.Arguments = '/c "' + $RunnerPath + '"'
    $psi.WorkingDirectory = $runDir
    $psi.UseShellExecute = $false
    # Attach to this console (no extra window): closing the terminal sends CTRL_CLOSE to the
    # children as well, so nothing survives a closed window. Ctrl+C cleanup lives in finally.
    $psi.CreateNoWindow = $false
    return [System.Diagnostics.Process]::Start($psi)
}

$children = @()
$script:stopping = $false
$onCancel = [ConsoleCancelEventHandler]{
    param($sender, $e)
    $e.Cancel = $true
    $script:stopping = $true
}
[Console]::add_CancelKeyPress($onCancel)

$winRunner = New-Runner -Name 'win-run' -WorkDir $winDir -CommandLine 'call npm run tauri dev' -OutFile $winOut -ErrFile $winErr

# ANSI escapes are pointless once output goes through a file, and cargo's \r progress bar would
# otherwise pile up into one huge line; both get cleaned in Write-Tail.
$ansi = [char]27 + '\[[0-9;]*[A-Za-z]'

$tails = @(
    @{ Path = $winOut; Tag = 'win '; Color = 'Green'; Pos = 0; Buf = '' },
    @{ Path = $winErr; Tag = 'win!'; Color = 'DarkGray'; Pos = 0; Buf = '' },
    @{ Path = $ohOut; Tag = 'ohos'; Color = 'Cyan'; Pos = 0; Buf = '' },
    @{ Path = $ohErr; Tag = 'ohos'; Color = 'DarkGray'; Pos = 0; Buf = '' }
)

function Read-Tail {
    param($Tail)
    if (-not (Test-Path -LiteralPath $Tail.Path)) { return @() }
    try { $fs = [System.IO.File]::Open($Tail.Path, 'Open', 'Read', 'ReadWrite') } catch { return @() }
    try {
        if ($fs.Length -le $Tail.Pos) { return @() }
        [void]$fs.Seek($Tail.Pos, 'Begin')
        $count = [int]($fs.Length - $Tail.Pos)
        $buf = New-Object byte[] $count
        $read = $fs.Read($buf, 0, $count)
        $Tail.Pos += $read
        if ($read -le 0) { return @() }
        $text = $Tail.Buf + [System.Text.Encoding]::UTF8.GetString($buf, 0, $read)
        $parts = $text -split "`r?`n"
        $Tail.Buf = $parts[$parts.Count - 1]
        if ($parts.Count -le 1) { return @() }
        return $parts[0..($parts.Count - 2)]
    } finally {
        $fs.Dispose()
    }
}

function Write-Tail {
    param($Tail)
    foreach ($line in (Read-Tail -Tail $Tail)) {
        $text = [regex]::Replace($line, $ansi, '')
        $text = ($text -split "`r")[-1]
        if ($text.Trim().Length -eq 0) { continue }
        Write-Host ('[' + $Tail.Tag + '] ') -NoNewline -ForegroundColor $Tail.Color
        Write-Host $text
    }
}

function Stop-Tree {
    param($Id)
    if (-not $Id) { return }
    try { & taskkill.exe /T /F /PID $Id 2>&1 | Out-Null } catch { }
}

try {
    $win = Start-Runner -RunnerPath $winRunner
    $children += $win

    if ($wantPhone -and $hdc) {
        $hdcLine = '"' + $hdc + '" shell hilog -L D'
        if ($dev) { $hdcLine = '"' + $hdc + '" -t ' + $dev + ' shell hilog -L D' }
        $ohRunner = New-Runner -Name 'ohos-run' -WorkDir $root -CommandLine $hdcLine -OutFile $ohOut -ErrFile $ohErr
        $children += (Start-Runner -RunnerPath $ohRunner)
    }

    # The PC shell is the primary stream: when it dies (compile error, window closed) the whole
    # session ends instead of hanging on the hilog tail forever.
    $exitCode = $null
    while (-not $script:stopping) {
        foreach ($t in $tails) { Write-Tail -Tail $t }
        if ($win.HasExited) {
            $exitCode = $win.ExitCode
            break
        }
        Start-Sleep -Milliseconds 150
    }

    Start-Sleep -Milliseconds 300
    foreach ($t in $tails) { Write-Tail -Tail $t }
    if ($exitCode -ne $null -and -not $script:stopping) {
        Write-Host ('[start] PC shell exited (code ' + $exitCode + ')') -ForegroundColor Yellow
    }
} finally {
    [Console]::remove_CancelKeyPress($onCancel)
    Write-Host '[start] stopping...' -ForegroundColor Yellow
    foreach ($c in $children) {
        if ($c -and -not $c.HasExited) { Stop-Tree -Id $c.Id }
    }
    Write-Host '[start] bye' -ForegroundColor Green
}
