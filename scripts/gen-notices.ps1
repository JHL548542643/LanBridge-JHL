# Generate THIRD-PARTY-NOTICES.md for binary releases.
#
# A Rust binary statically links ~140 crates; MIT/BSD/Apache all require reproducing their
# copyright notices when you ship the binary. Source-only checkouts do not need this file, but
# release artefacts (.exe / .hap) should carry it.
#
# Usage (repo root, PowerShell):
#   .\scripts\gen-notices.ps1                 # -> THIRD-PARTY-NOTICES.md
#   .\scripts\gen-notices.ps1 -Out notices.md
#
# Works offline: licence texts come from the local cargo registry cache; the package list and
# licence expressions come from cargo metadata / package-lock.json.
[CmdletBinding()]
param(
    [string]$Out = 'THIRD-PARTY-NOTICES.md',
    [string]$CargoHome = $(if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' })
)

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$registry = Join-Path $CargoHome 'registry\src'

function Get-Packages([string]$manifest) {
    if (-not (Test-Path -LiteralPath $manifest)) { return @() }
    $json = & cargo metadata --format-version 1 --locked --manifest-path $manifest 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $json) { return @() }
    $meta = $json | ConvertFrom-Json
    $meta.packages | Where-Object { $_.name -ne 'win-shell' } | ForEach-Object {
        [pscustomobject]@{
            Name    = $_.name
            Version = $_.version
            License = $(if ($_.license) { $_.license } else { 'UNKNOWN' })
        }
    }
}

Write-Host '[notices] collecting crates from lockfiles'
$packages = @()
$packages += Get-Packages (Join-Path $root 'src\Cargo.toml')
$packages += Get-Packages (Join-Path $root 'apps\win-shell\src-tauri\Cargo.toml')

# frontend runtime deps (Vue, Tauri JS APIs) are bundled into the exe as well
$pkgJson = Join-Path $root 'apps\win-shell\package.json'
if (Test-Path -LiteralPath $pkgJson) {
    $pkg = Get-Content -LiteralPath $pkgJson -Raw | ConvertFrom-Json
    foreach ($d in $pkg.dependencies.PSObject.Properties) {
        $ver = $d.Value -replace '^[\^~]', ''
        $packages += [pscustomobject]@{ Name = $d.Name; Version = $ver; License = 'see package-lock.json' }
    }
}

$packages = $packages | Sort-Object Name, Version -Unique
Write-Host ("[notices] " + $packages.Count + " packages")

# ---- licence texts, deduplicated by content ---------------------------------
$texts = [ordered]@{}
foreach ($p in $packages) {
    if (-not (Test-Path -LiteralPath $registry)) { break }
    $dir = Get-ChildItem -LiteralPath $registry -Directory -ErrorAction SilentlyContinue |
        ForEach-Object { Join-Path $_.FullName ($p.Name + '-' + $p.Version) } |
        Where-Object { Test-Path -LiteralPath $_ } |
        Select-Object -First 1
    if (-not $dir) { continue }
    foreach ($f in (Get-ChildItem -LiteralPath $dir -File -ErrorAction SilentlyContinue |
            Where-Object { $_.Name -match '^(LICENSE|LICENCE|UNLICENSE|COPYING)' })) {
        $body = Get-Content -LiteralPath $f.FullName -Raw -ErrorAction SilentlyContinue
        if (-not $body -or $body.Length -lt 20) { continue }
        $key = ($body -replace '\s+', ' ').Trim()
        if (-not $texts.Contains($key)) {
            $texts[$key] = [pscustomobject]@{ Title = ($p.Name + ' — ' + $f.Name); Body = $body.Trim() }
        }
    }
}

# ---- emit -------------------------------------------------------------------
$sb = New-Object System.Text.StringBuilder
[void]$sb.AppendLine('# Third-party notices')
[void]$sb.AppendLine()
[void]$sb.AppendLine('LanBridge-JHL ships third-party software. Source checkouts resolve these through')
[void]$sb.AppendLine('`Cargo.lock` and `package-lock.json`; **binary** artefacts (.exe / .hap) must carry the')
[void]$sb.AppendLine('licence texts below to satisfy their MIT / BSD / Apache-2.0 terms.')
[void]$sb.AppendLine()
[void]$sb.AppendLine('Regenerate with `scripts/gen-notices.ps1`.')
[void]$sb.AppendLine()
[void]$sb.AppendLine('## Components')
[void]$sb.AppendLine()
[void]$sb.AppendLine('| Package | Version | Licence |')
[void]$sb.AppendLine('|---|---|---|')
foreach ($p in $packages) {
    [void]$sb.AppendLine('| ' + $p.Name + ' | ' + $p.Version + ' | ' + $p.License + ' |')
}
[void]$sb.AppendLine()
[void]$sb.AppendLine('## Licence texts')
foreach ($k in $texts.Keys) {
    $t = $texts[$k]
    [void]$sb.AppendLine()
    [void]$sb.AppendLine('### ' + $t.Title)
    [void]$sb.AppendLine()
    [void]$sb.AppendLine('```')
    [void]$sb.AppendLine($t.Body)
    [void]$sb.AppendLine('```')
}

$dest = Join-Path $root $Out
[System.IO.File]::WriteAllText($dest, $sb.ToString(), (New-Object System.Text.UTF8Encoding($false)))
Write-Host ("[notices] wrote " + $dest + " (" + [math]::Round((Get-Item $dest).Length / 1KB) + " KB, " + $texts.Count + " licence texts)")
