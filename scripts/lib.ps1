# LanBridge shared helpers (dot-sourced by scripts\*.ps1)

function Get-LanBridgeRoot {
    return (Split-Path -Parent $PSScriptRoot)
}

function Test-CommandExists([string]$Name) {
    return [bool](Get-Command $Name -ErrorAction SilentlyContinue)
}

function Get-DevEcoHome {
    if ($env:DEVECO_HOME -and (Test-Path -LiteralPath $env:DEVECO_HOME)) { return $env:DEVECO_HOME }
    $candidates = @(
        'D:\HUAWEI_DEV_ECO\app\DevEco Studio',
        'C:\Program Files\Huawei\DevEco Studio'
    )
    foreach ($c in $candidates) {
        if (Test-Path -LiteralPath $c) { return $c }
    }
    $keys = @(
        'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*'
    )
    $entry = Get-ItemProperty $keys -ErrorAction SilentlyContinue |
        Where-Object { $_.DisplayName -eq 'DevEco Studio' } |
        Select-Object -First 1
    if ($entry -and $entry.InstallLocation -and (Test-Path -LiteralPath $entry.InstallLocation)) {
        return $entry.InstallLocation
    }
    return $null
}

function Get-DevEcoTools([string]$DevEcoHome) {
    $tools = @{ Sdk = $null; Hdc = $null; Ohpm = $null; Hvigor = $null }
    if (-not $DevEcoHome) { return $tools }
    $sdk = Join-Path $DevEcoHome 'sdk'
    if (Test-Path -LiteralPath $sdk) { $tools.Sdk = $sdk }
    $hdc = Join-Path $DevEcoHome 'sdk\default\openharmony\toolchains\hdc.exe'
    if (Test-Path -LiteralPath $hdc) { $tools.Hdc = $hdc }
    $ohpm = Join-Path $DevEcoHome 'tools\ohpm\bin\ohpm.bat'
    if (Test-Path -LiteralPath $ohpm) { $tools.Ohpm = $ohpm }
    $hvigor = Join-Path $DevEcoHome 'tools\hvigor\bin\hvigorw.bat'
    if (Test-Path -LiteralPath $hvigor) { $tools.Hvigor = $hvigor }
    return $tools
}

# Prepare MSVC (link.exe + Windows SDK libs) for Rust MSVC toolchain / Tauri builds.
# Rust does not read vcvars; it needs link.exe on PATH plus LIB/INCLUDE set.
function Set-MsvcEnv {
    $vs = Get-MsvcInstallPath
    if (-not $vs) { return $false }
    $msvcRoot = Join-Path $vs 'VC\Tools\MSVC'
    if (-not (Test-Path -LiteralPath $msvcRoot)) { return $false }
    $ver = Get-ChildItem -LiteralPath $msvcRoot -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending | Select-Object -First 1
    if (-not $ver) { return $false }

    $hostBin = Join-Path $ver.FullName 'bin\Hostx64\x64'
    if (-not (Test-Path -LiteralPath (Join-Path $hostBin 'link.exe'))) { return $false }
    if (($env:PATH -split ';') -notcontains $hostBin) { $env:PATH = "$hostBin;$env:PATH" }

    $libs = @((Join-Path $ver.FullName 'lib\x64'))
    $incs = @((Join-Path $ver.FullName 'include'))

    $kits = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10'
    if (Test-Path -LiteralPath (Join-Path $kits 'Lib')) {
        $sdkVer = Get-ChildItem -LiteralPath (Join-Path $kits 'Lib') -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending | Select-Object -First 1
        if ($sdkVer) {
            foreach ($sub in @('um\x64', 'ucrt\x64')) {
                $p = Join-Path $sdkVer.FullName $sub
                if (Test-Path -LiteralPath $p) { $libs += $p }
            }
            foreach ($sub in @('ucrt', 'um', 'shared', 'winrt')) {
                $p = Join-Path (Join-Path $kits 'Include') (Join-Path $sdkVer.Name $sub)
                if (Test-Path -LiteralPath $p) { $incs += $p }
            }
        }
    }

    $env:LIB = (($env:LIB -split ';') + $libs | Where-Object { $_ } | Select-Object -Unique) -join ';'
    $env:INCLUDE = (($env:INCLUDE -split ';') + $incs | Where-Object { $_ } | Select-Object -Unique) -join ';'
    return $true
}

function Get-MsvcInstallPath {
    $root = Get-LanBridgeRoot
    $local = Join-Path $root 'ide\BuildTools'
    if (Test-Path -LiteralPath (Join-Path $local 'VC')) { return $local }

    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { return $null }
    $inst = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
    if ($inst) {
        $path = ($inst | Select-Object -First 1)
        if ($path -and (Test-Path -LiteralPath $path)) { return $path }
    }
    return $null
}

function Get-ProxyUrl {
    foreach ($var in @('HTTPS_PROXY', 'HTTP_PROXY', 'https_proxy', 'http_proxy')) {
        $v = [Environment]::GetEnvironmentVariable($var)
        if ($v) { return $v }
    }
    return $null
}
