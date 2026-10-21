# LanBridge env audit (read-only). Writes data\results\env-report-<HOST>.md
# Usage: powershell -NoProfile -File .\scripts\check-env.ps1
# Prefer: . .\scripts\env.ps1; .\scripts\check-env.ps1
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\env.ps1" -Quiet

$root = Get-LanBridgeRoot
$results = @()

function Add-Result {
    param([string]$Name, [string]$Level, [bool]$Ok, [string]$Detail, [string]$Hint = '')
    $script:results += [pscustomobject]@{
        Name   = $Name
        Level  = $Level
        Status = $(if ($Ok) { 'OK' } else { 'MISSING' })
        Detail = $Detail
        Hint   = $Hint
    }
}

$build = [int](Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').CurrentBuild
Add-Result 'Windows' 'P0' ($build -ge 19041) "build $build"
$longPaths = (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem' -Name LongPathsEnabled -ErrorAction SilentlyContinue).LongPathsEnabled
Add-Result 'LongPathsEnabled' 'P1' ($longPaths -eq 1) "=$longPaths" 'setup-dev.ps1 -Only admin'
$drive = Get-PSDrive -Name ($root.Substring(0, 1)) -ErrorAction SilentlyContinue
if ($drive) {
    $freeGB = [math]::Round($drive.Free / 1GB, 1)
    Add-Result 'DiskFree' 'P0' ($freeGB -ge 20) "$freeGB GB"
}

$gitOk = Test-CommandExists 'git'
Add-Result 'git' 'P0' $gitOk $(if ($gitOk) { (& git --version) } else { 'missing' })

$nodeOk = Test-CommandExists 'node'
$npmOk = Test-CommandExists 'npm'
Add-Result 'node' 'P0' $nodeOk $(if ($nodeOk) { (& node -v) } else { 'missing' }) 'nvm / keep C:\nvm4w\nodejs'
Add-Result 'npm' 'P0' $npmOk $(if ($npmOk) { (& npm -v) } else { 'missing' })

$javaOk = $false
$javaDetail = 'missing'
if ($env:JAVA_HOME -and (Test-Path -LiteralPath (Join-Path $env:JAVA_HOME 'bin\java.exe'))) {
    $prev = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    $v = [string](& (Join-Path $env:JAVA_HOME 'bin\java.exe') -version 2>&1 | Select-Object -First 1)
    $ErrorActionPreference = $prev
    $javaOk = $v -match '"(\d+)'
    $javaDetail = "$env:JAVA_HOME | $v"
}
Add-Result 'JDK17' 'P0' $javaOk $javaDetail 'machine path D:\jdk\app'

$rustupOk = Test-CommandExists 'rustup'
$cargoOk = Test-CommandExists 'cargo'
Add-Result 'Rust' 'P0' ($rustupOk -and $cargoOk) $(if ($rustupOk -and $cargoOk) { (& rustc --version) } else { 'missing' }) 'setup-dev.ps1 -Only rust'
if ($rustupOk) {
    $targets = @((& rustup target list --installed) -split "`n" | ForEach-Object { $_.Trim() })
    $need = @('x86_64-pc-windows-msvc', 'aarch64-unknown-linux-ohos')
    $missingTargets = @($need | Where-Object { $targets -notcontains $_ })
    Add-Result 'RustTargets' 'P0' ($missingTargets.Count -eq 0) $(if ($missingTargets.Count -eq 0) { 'win+ohos' } else { 'missing: ' + ($missingTargets -join ', ') }) 'setup-dev.ps1 -Only rust'
}

$msvcPath = Get-MsvcInstallPath
Add-Result 'MSVC' 'P0' ([bool]$msvcPath) $(if ($msvcPath) { $msvcPath } else { 'missing (HaWiAn path stale)' }) 'setup-dev.ps1 -Only msvc -> ide\BuildTools'

$wvOk = $false
$wvDetail = 'missing'
foreach ($reg in @(
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
        'HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    )) {
    $p = Get-ItemProperty $reg -ErrorAction SilentlyContinue
    if ($p -and $p.pv) { $wvOk = $true; $wvDetail = "pv=$($p.pv)"; break }
}
Add-Result 'WebView2' 'P0' $wvOk $wvDetail 'required by Tauri 2'

$deveco = Get-DevEcoHome
Add-Result 'DevEco' 'P0' ([bool]$deveco) $(if ($deveco) { $deveco } else { 'missing' }) 'keep D:\HUAWEI_DEV_ECO'
if ($deveco) {
    $t = Get-DevEcoTools $deveco
    Add-Result 'hdc' 'P0' ([bool]$t.Hdc) $(if ($t.Hdc) { $t.Hdc } else { 'missing' })
    Add-Result 'ohpm' 'P1' ([bool]$t.Ohpm) $(if ($t.Ohpm) { $t.Ohpm } else { 'missing' })
    Add-Result 'hvigor' 'P1' ([bool]$t.Hvigor) $(if ($t.Hvigor) { $t.Hvigor } else { 'missing' })
}

Add-Result 'iperf3' 'P1' (Test-CommandExists 'iperf3') '' 'setup-dev.ps1 -Only tools'
Add-Result 'cmake' 'OPT' (Test-CommandExists 'cmake') '' 'setup-dev.ps1 -Only tools'

$remoteUrl = & git -C $root remote get-url origin 2>$null
Add-Result 'git-remote' 'P0' ([bool]$remoteUrl) "$remoteUrl" 'origin must point at your fork/remote'

# 私有开发资料：公开仓（仅代码 + 产品文档）没有这些目录，因此不参与 P0 判定。
# 私有开发资料：只有存在时才检查（公开仓不含这两块，不必刷出无关条目）。
$planPath = Join-Path $root 'docs\plans\phase1-confirmed.md'
if (Test-Path -LiteralPath $planPath) {
    Add-Result 'phase1-plan' 'OPT' $true 'docs/plans/phase1-confirmed.md'
}
$vendorPath = Join-Path $root 'docs\vendor\INDEX.md'
if (Test-Path -LiteralPath $vendorPath) {
    Add-Result 'vendor-index' 'OPT' $true 'docs/vendor/INDEX.md'
}

$missingP0 = @($results | Where-Object { $_.Level -eq 'P0' -and $_.Status -eq 'MISSING' }).Count
foreach ($r in $results) {
    $color = 'Green'
    if ($r.Status -eq 'MISSING') { $color = $(if ($r.Level -eq 'P0') { 'Red' } else { 'Yellow' }) }
    Write-Host ("[{0}] {1,-3} {2,-16} {3}" -f $r.Status, $r.Level, $r.Name, $r.Detail) -ForegroundColor $color
}
Write-Host ''
if ($missingP0 -eq 0) { Write-Host 'P0 ready' -ForegroundColor Green }
else { Write-Host "P0 missing $missingP0" -ForegroundColor Red }

$resultsDir = Join-Path $root 'data\results'
if (-not (Test-Path -LiteralPath $resultsDir)) { New-Item -ItemType Directory -Path $resultsDir -Force | Out-Null }
$hostName = $env:COMPUTERNAME
$stamp = Get-Date -Format 'yyyy-MM-dd HH:mm:ss'
$mdLines = @("# Env report - $hostName", '', "- time: $stamp", "- P0 missing: $missingP0", '', '| Status | Level | Name | Detail | Hint |', '|---|---|---|---|---|')
foreach ($r in $results) { $mdLines += "| $($r.Status) | $($r.Level) | $($r.Name) | $($r.Detail) | $($r.Hint) |" }
$utf8 = New-Object System.Text.UTF8Encoding($false)
[System.IO.File]::WriteAllText((Join-Path $resultsDir "env-report-$hostName.md"), ($mdLines -join "`n"), $utf8)
[System.IO.File]::WriteAllText((Join-Path $resultsDir "env-report-$hostName.json"), ($results | ConvertTo-Json -Depth 3), $utf8)
Write-Host "wrote data\results\env-report-$hostName.md"

exit $missingP0
