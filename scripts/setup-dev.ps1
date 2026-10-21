# LanBridge setup (idempotent). Placement:
#   toolchains\  rust/cmake/iperf3 (gitignored)
#   ide\BuildTools  MSVC (gitignored)
#   keep existing: D:\HUAWEI_DEV_ECO, D:\jdk\app, C:\nvm4w\nodejs
#
# Usage:
#   .\scripts\setup-dev.ps1 -Yes
#   .\scripts\setup-dev.ps1 -Yes -Only rust
#   .\scripts\setup-dev.ps1 -Yes -Only msvc
[CmdletBinding()]
param(
    [switch]$Yes,
    [ValidateSet('all', 'rust', 'msvc', 'tools', 'admin')]
    [string[]]$Only = @('all')
)

$ErrorActionPreference = 'Stop'
. "$PSScriptRoot\env.ps1" -Quiet

$root = Get-LanBridgeRoot
$phases = @($Only)
if ($phases -contains 'all') { $phases = @('rust', 'msvc', 'tools', 'admin') }

function Confirm-Step([string]$What) {
    if ($Yes) { return $true }
    $answer = Read-Host "$What ? [y/N]"
    return ($answer -match '^[Yy]')
}

function Invoke-RustPhase {
    Write-Host '--- Rust (toolchains\rust) ---' -ForegroundColor Cyan
    New-Item -ItemType Directory -Force -Path (Join-Path $root 'toolchains\rust') | Out-Null
    $env:RUSTUP_HOME = Join-Path $root 'toolchains\rust\rustup'
    $env:CARGO_HOME = Join-Path $root 'toolchains\rust\cargo'
    if (-not (Test-CommandExists 'rustup')) {
        if (-not (Confirm-Step 'Install rustup into toolchains\rust')) { return }
        $rustupInit = Join-Path $root '.tmp\rustup-init.exe'
        New-Item -ItemType Directory -Force -Path (Split-Path $rustupInit) | Out-Null
        $url = 'https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe'
        $proxy = Get-ProxyUrl
        Write-Host "download $url"
        if ($proxy) { Invoke-WebRequest -Uri $url -OutFile $rustupInit -Proxy $proxy -UseBasicParsing }
        else { Invoke-WebRequest -Uri $url -OutFile $rustupInit -UseBasicParsing }
        & $rustupInit -y --no-modify-path --profile minimal --default-toolchain stable
        $env:PATH = "$(Join-Path $env:CARGO_HOME 'bin');$env:PATH"
    } else {
        Write-Host 'rustup present'
    }
    & rustup component add rustfmt clippy
    & rustup target add x86_64-pc-windows-msvc aarch64-unknown-linux-ohos
}

function Invoke-MsvcPhase {
    Write-Host '--- MSVC Build Tools (ide\BuildTools) ---' -ForegroundColor Cyan
    $existing = Get-MsvcInstallPath
    if ($existing) {
        Write-Host "MSVC OK: $existing"
        return
    }
    if (-not (Confirm-Step 'Install VS 2022 Build Tools into ide\BuildTools (large, UAC)')) { return }
    if (-not (Test-CommandExists 'winget')) { throw 'winget missing' }
    $installPath = Join-Path $root 'ide\BuildTools'
    New-Item -ItemType Directory -Force -Path (Split-Path $installPath) | Out-Null
    # Registry may still point at deleted HaWiAn path; force reinstall into this repo.
    $override = "--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended --installPath `"$installPath`""
    & winget install --id Microsoft.VisualStudio.2022.BuildTools --force --accept-source-agreements --accept-package-agreements --override $override
    if (-not (Get-MsvcInstallPath)) {
        Write-Warning "MSVC still missing. Open Visual Studio Installer and repair, or reinstall to: $installPath"
    }
}

function Invoke-ToolsPhase {
    Write-Host '--- optional tools (cmake / iperf3 via winget) ---' -ForegroundColor Cyan
    if (-not (Test-CommandExists 'winget')) { Write-Warning 'winget missing'; return }
    $packages = @(
        @{ Id = 'Kitware.CMake'; Cmd = 'cmake' },
        @{ Id = 'ar51an.iperf3'; Cmd = 'iperf3' }
    )
    foreach ($pkg in $packages) {
        if (Test-CommandExists $pkg.Cmd) { Write-Host "$($pkg.Cmd) OK"; continue }
        if (Confirm-Step "winget install $($pkg.Cmd)") {
            & winget install --id $pkg.Id --accept-source-agreements --accept-package-agreements --silent
        }
    }
}

function Invoke-AdminPhase {
    Write-Host '--- admin hints (manual if needed) ---' -ForegroundColor Cyan
    Write-Host 'LongPaths / Defender exclude: run elevated:'
    Write-Host '  New-ItemProperty -Path HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem -Name LongPathsEnabled -Value 1 -PropertyType DWORD -Force'
    Write-Host "  Add-MpPreference -ExclusionPath '$root'"
}

foreach ($phase in $phases) {
    switch ($phase) {
        'rust' { Invoke-RustPhase }
        'msvc' { Invoke-MsvcPhase }
        'tools' { Invoke-ToolsPhase }
        'admin' { Invoke-AdminPhase }
    }
}

Write-Host 'setup-dev done. Re-run: . .\scripts\env.ps1; .\scripts\check-env.ps1' -ForegroundColor Green
