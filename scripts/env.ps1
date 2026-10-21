# LanBridge session env. Usage: . .\scripts\env.ps1
param([switch]$Quiet)

. "$PSScriptRoot\lib.ps1"

$root = Get-LanBridgeRoot
$env:LANBRIDGE_ROOT = $root

# JDK (DevEco / sdk tools); machine convention D:\jdk\app
if (-not $env:JAVA_HOME -or -not (Test-Path -LiteralPath (Join-Path $env:JAVA_HOME 'bin\java.exe'))) {
    $jdkCandidate = 'D:\jdk\app'
    if (Test-Path -LiteralPath (Join-Path $jdkCandidate 'bin\java.exe')) {
        $env:JAVA_HOME = $jdkCandidate
    }
}

# Rust redirected into repo (gitignored)
$env:RUSTUP_HOME = Join-Path $root 'toolchains\rust\rustup'
$env:CARGO_HOME = Join-Path $root 'toolchains\rust\cargo'

$tmp = Join-Path $root '.tmp'
if (-not (Test-Path -LiteralPath $tmp)) { New-Item -ItemType Directory -Path $tmp -Force | Out-Null }
$env:TMP = $tmp
$env:TEMP = $tmp

$addPaths = @()
if ($env:JAVA_HOME) { $addPaths += (Join-Path $env:JAVA_HOME 'bin') }
$addPaths += (Join-Path $env:CARGO_HOME 'bin')

$cmakeBin = Join-Path $root 'toolchains\cmake\bin'
if (Test-Path -LiteralPath (Join-Path $cmakeBin 'cmake.exe')) { $addPaths += $cmakeBin }
$iperfBin = Join-Path $root 'toolchains\iperf3'
if (Test-Path -LiteralPath (Join-Path $iperfBin 'iperf3.exe')) { $addPaths += $iperfBin }

$deveco = Get-DevEcoHome
if ($deveco) {
    $t = Get-DevEcoTools $deveco
    if ($t.Sdk) { $env:DEVECO_SDK_HOME = $t.Sdk }
    if ($t.Hdc) { $addPaths += (Split-Path -Parent $t.Hdc) }
    if ($t.Ohpm) { $addPaths += (Split-Path -Parent $t.Ohpm) }
    if ($t.Hvigor) { $addPaths += (Split-Path -Parent $t.Hvigor) }

    $ohosNative = Join-Path $deveco 'sdk\default\openharmony\native'
    if (Test-Path -LiteralPath (Join-Path $ohosNative 'llvm\bin\clang.exe')) {
        $junction = Join-Path $root 'toolchains\ohos-native'
        if (-not (Test-Path -LiteralPath $junction)) {
            New-Item -ItemType Junction -Path $junction -Target $ohosNative -ErrorAction SilentlyContinue | Out-Null
        }
        $nativePath = if (Test-Path -LiteralPath $junction) { $junction } else { $ohosNative }
        $clang = Join-Path $nativePath 'llvm\bin\clang.exe'
        $sysroot = Join-Path $nativePath 'sysroot'
        $env:CC_aarch64_unknown_linux_ohos = $clang
        $env:CXX_aarch64_unknown_linux_ohos = Join-Path $nativePath 'llvm\bin\clang++.exe'
        $env:AR_aarch64_unknown_linux_ohos = Join-Path $nativePath 'llvm\bin\llvm-ar.exe'
        # rustc defaults to linker name `cc` — point at OHOS clang
        $env:CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER = $clang
        $env:CFLAGS_aarch64_unknown_linux_ohos = "--target=aarch64-linux-ohos --sysroot=$sysroot -D__MUSL__"
        $env:CXXFLAGS_aarch64_unknown_linux_ohos = $env:CFLAGS_aarch64_unknown_linux_ohos
        $env:CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_RUSTFLAGS = "-C link-arg=--target=aarch64-linux-ohos -C link-arg=--sysroot=$sysroot"
        $addPaths += (Join-Path $nativePath 'llvm\bin')
    }
}

foreach ($p in $addPaths) {
    if (-not $p) { continue }
    if (-not (Test-Path -LiteralPath $p)) { continue }
    $current = $env:PATH -split ';'
    if ($current -notcontains $p) { $env:PATH = "$p;$env:PATH" }
}

$machinePath = [Environment]::GetEnvironmentVariable('Path', 'Machine') -split ';'
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User') -split ';'
$merged = New-Object System.Collections.Generic.List[string]
foreach ($p in ($env:PATH -split ';') + $machinePath + $userPath) {
    if ($p -and ($merged -notcontains $p)) { $merged.Add($p) }
}
$env:PATH = ($merged -join ';')

if (-not $Quiet) {
    Write-Host '[env] LanBridge session ready' -ForegroundColor Green
    Write-Host "[env] root       : $root"
    Write-Host "[env] RUSTUP_HOME: $env:RUSTUP_HOME"
    Write-Host "[env] CARGO_HOME : $env:CARGO_HOME"
    Write-Host "[env] JAVA_HOME  : $env:JAVA_HOME"
    Write-Host "[env] DevEco     : $deveco"
}
