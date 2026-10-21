# LanBridge Windows shell dev runner (Tauri 2 + Vue 3).
#
# Usage (from repo root):
#   powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\dev-win.ps1            # npm run tauri dev (full app)
#   ... -File .\scripts\dev-win.ps1 -UiOnly                                              # Vue only (vite, no Rust)
#   ... -File .\scripts\dev-win.ps1 -Build                                               # release build (no dev server)
#
# Log surfaces:
#   terminal  -> Rust panics / tracing, Vite HMR, tauri build output
#   WebView2  -> frontend console: press F12 (or right-click > Inspect) inside the app window
[CmdletBinding()]
param(
    [switch]$UiOnly,
    [switch]$Build
)

# npm prints warnings on stderr; EAP=Stop would turn those into terminating errors.
$ErrorActionPreference = 'Continue'
. "$PSScriptRoot\env.ps1" -Quiet
$root = Get-LanBridgeRoot

if (-not (Test-CommandExists 'node')) { throw 'node not found. Install Node 20+ (nvm C:\nvm4w\nodejs is the machine convention).' }

if (Set-MsvcEnv) {
    Write-Host '[win] MSVC env ready (link.exe + Windows SDK libs)' -ForegroundColor DarkGray
} else {
    Write-Host '[win] WARN: MSVC not prepared; cargo link may fail. Run scripts\check-env.ps1.' -ForegroundColor Yellow
}

$shell = Join-Path $root 'apps\win-shell'
if (-not (Test-Path -LiteralPath (Join-Path $shell 'node_modules'))) {
    Write-Host '[win] node_modules missing -> npm install' -ForegroundColor Cyan
    Push-Location $shell
    try { npm install; if ($LASTEXITCODE -ne 0) { throw "npm install failed (exit $LASTEXITCODE)" } }
    finally { Pop-Location }
}

Push-Location $shell
try {
    if ($Build) {
        Write-Host '[win] npm run tauri build' -ForegroundColor Cyan
        npm run tauri build
    } elseif ($UiOnly) {
        Write-Host '[win] vite dev server only -> http://localhost:1420' -ForegroundColor Cyan
        npm run dev
    } else {
        Write-Host '[win] npm run tauri dev  (first run compiles Rust: a few minutes)' -ForegroundColor Cyan
        Write-Host '[win] frontend console: F12 inside the app window' -ForegroundColor DarkGray
        npm run tauri dev
    }
} finally {
    Pop-Location
}
