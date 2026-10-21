# LanBridge HarmonyOS shell: build FFI + HAP, install, launch (CLI only, no DevEco UI needed).
#
# Usage (from repo root):
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\ohos-hap.ps1            # Rust FFI + signed HAP
#   ... -File .\scripts\ohos-hap.ps1 -Install                                              # + install to device
#   ... -File .\scripts\ohos-hap.ps1 -Install -Launch                                     # + start the app
#   ... -File .\scripts\ohos-hap.ps1 -Status                                              # device + artifact status only
#   ... -File .\scripts\ohos-hap.ps1 -NoFfi -NoBuild -Install                              # only install the existing HAP
[CmdletBinding()]
param(
    [switch]$Status,
    [switch]$NoFfi,
    [switch]$NoBuild,
    [switch]$Install,
    [switch]$Launch,
    [string]$Device = ''
)

# hvigor / hdc write progress to stderr; with EAP=Stop PowerShell 5.1 turns that into a
# terminating NativeCommandError, so keep it non-terminating and check $LASTEXITCODE explicitly.
$ErrorActionPreference = 'Continue'
. "$PSScriptRoot\env.ps1" -Quiet

$root = Get-LanBridgeRoot
$appDir = Join-Path $root 'apps\ohos-shell'
$bundle = 'com.lanbridge.ohosshell'
$ability = 'EntryAbility'
$hap = Join-Path $appDir 'entry\build\default\outputs\default\entry-default-signed.hap'
$ffi = Join-Path $root 'target\aarch64-unknown-linux-ohos\release\liblb_ffi.a'

$deveco = Get-DevEcoHome
if (-not $deveco) { throw 'DevEco Studio not found (expected D:\HUAWEI_DEV_ECO). Run scripts\check-env.ps1.' }
$tools = Get-DevEcoTools $deveco
$hdc = $tools.Hdc
$hvigor = $tools.Hvigor
if (-not $hdc) { throw "hdc not found under $deveco" }

# hvigor needs its bundled node + ohpm on PATH.
foreach ($p in @((Join-Path $deveco 'tools\node'), (Join-Path $deveco 'tools\ohpm\bin'), (Join-Path $deveco 'tools\hvigor\bin'))) {
    if ((Test-Path -LiteralPath $p) -and -not (($env:PATH -split ';') -contains $p)) { $env:PATH = "$p;$env:PATH" }
}
if (-not $env:DEVECO_SDK_HOME) { $env:DEVECO_SDK_HOME = Join-Path $deveco 'sdk' }

function Get-Targets {
    $out = & $hdc list targets 2>&1
    @($out | ForEach-Object { "$_".Trim() } | Where-Object { $_ -and $_ -ne '[Empty]' })
}

function Show-Status {
    Write-Host "[ohos] hdc     : $hdc"
    $ver = (& $hdc -v 2>&1 | Select-Object -First 1)
    Write-Host "[ohos] version : $ver"
    # '@(...)' matters: a single target returned from a function unwraps to a string,
    # and $targets[0] would then be its first character.
    $targets = @(Get-Targets)
    if ($targets.Count -eq 0) {
        Write-Host '[ohos] device  : NONE (plug the phone, enable USB debugging, accept the prompt)' -ForegroundColor Yellow
    } else {
        foreach ($t in $targets) {
            $model = (& $hdc -t $t shell param get const.product.model 2>&1 | Select-Object -First 1)
            $api = (& $hdc -t $t shell param get const.ohos.apiversion 2>&1 | Select-Object -First 1)
            Write-Host "[ohos] device  : $t  model=$model  api=$api" -ForegroundColor Green
        }
    }
    if (Test-Path -LiteralPath $ffi) {
        $g = Get-Item -LiteralPath $ffi
        Write-Host ("[ohos] ffi     : {0:N1} MB  {1}" -f ($g.Length / 1MB), $g.LastWriteTime)
    } else {
        Write-Host '[ohos] ffi     : not built yet (run without -NoFfi)' -ForegroundColor Yellow
    }
    if (Test-Path -LiteralPath $hap) {
        $f = Get-Item -LiteralPath $hap
        Write-Host ("[ohos] hap     : {0:N1} MB  {1}" -f ($f.Length / 1MB), $f.LastWriteTime)
    } else {
        Write-Host '[ohos] hap     : not built yet' -ForegroundColor Yellow
    }
}

if ($Status) { Show-Status; exit 0 }

Show-Status

if (-not $NoFfi) {
    Write-Host '==> [1/2] Rust FFI (aarch64-unknown-linux-ohos, release)' -ForegroundColor Cyan
    & (Join-Path $PSScriptRoot 'build-ohos-ffi.ps1') -Release
    if ($LASTEXITCODE -ne 0) { throw "build-ohos-ffi.ps1 failed (exit $LASTEXITCODE)" }
}

if (-not $NoBuild) {
    if (-not (Test-Path -LiteralPath $hvigor)) { throw "hvigorw not found: $hvigor" }
    Write-Host '==> [2/2] hvigorw assembleHap (signed, debug)' -ForegroundColor Cyan
    Push-Location $appDir
    try {
        & $hvigor assembleHap --no-daemon 2>&1 | ForEach-Object { Write-Host $_ }
        $code = $LASTEXITCODE
    } finally {
        Pop-Location
    }
    if ($code -ne 0) { throw "hvigor assembleHap failed (exit $code)" }
    if (-not (Test-Path -LiteralPath $hap)) { throw "signed HAP not found: $hap" }
    Write-Host "[ohos] OK: $hap" -ForegroundColor Green
}

if ($Install -or $Launch) {
    if (-not (Test-Path -LiteralPath $hap)) { throw "no HAP to install: $hap" }
    # '@(...)' matters: a single target returned from a function unwraps to a string,
    # and $targets[0] would then be its first character.
    $targets = @(Get-Targets)
    if ($targets.Count -eq 0) { throw 'no device: enable USB debugging, then check "hdc list targets"' }
    $dev = $targets[0]
    if ($Device) { $dev = $Device }
    elseif ($targets.Count -gt 1) { Write-Host "[ohos] multiple devices, using $dev (pass -Device to override)" -ForegroundColor Yellow }

    Write-Host "==> hdc install -r ($dev)" -ForegroundColor Cyan
    $installOut = & $hdc -t $dev install -r $hap 2>&1
    $installOut | ForEach-Object { Write-Host $_ }
    # hdc exits 0 even when the bundle manager refuses (e.g. missing ACL), so inspect the text.
    $installText = ($installOut | Out-String)
    if ($LASTEXITCODE -ne 0 -or $installText -match 'failed to install' -or $installText -match 'error:') {
        throw "install failed: $($installText.Trim())"
    }

    if ($Launch) {
        Write-Host "==> aa start $bundle / $ability" -ForegroundColor Cyan
        & $hdc -t $dev shell aa start -a $ability -b $bundle 2>&1 | ForEach-Object { Write-Host $_ }
    }
}

Write-Host '[ohos] done.' -ForegroundColor Green
Write-Host '[ohos] logs: powershell -NoProfile -File .\scripts\ohos-log.ps1'
