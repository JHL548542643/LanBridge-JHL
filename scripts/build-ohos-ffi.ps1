# Build lb-ffi for HarmonyOS (aarch64).
# Requires ohos rust target + OHOS NDK linker configured in env / .cargo.
param(
    [switch]$Release
)

# cargo writes progress to stderr; with EAP=Stop PowerShell 5.1 turns that into a terminating
# NativeCommandError and the script dies mid-build (exit 1) even though cargo succeeded.
# Keep it non-terminating and check $LASTEXITCODE explicitly instead.
$ErrorActionPreference = "Continue"
$Root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot "env.ps1")

$profile = if ($Release) { "release" } else { "release" }
$target = "aarch64-unknown-linux-ohos"

# Prefer repo-root /target so DevEco gets a stable LB_FFI_LIB path.
$env:CARGO_TARGET_DIR = Join-Path $Root "target"

$linker = $env:CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER
if (-not $linker -or -not (Test-Path -LiteralPath $linker)) {
    Write-Host "[ohos-ffi] ERROR: OHOS linker unset. Run . .\scripts\env.ps1 first (needs DevEco sdk/.../native)."
    exit 1
}
Write-Host "[ohos-ffi] linker: $linker"
Write-Host "[ohos-ffi] CARGO_TARGET_DIR: $env:CARGO_TARGET_DIR"

Push-Location (Join-Path $Root "src")
try {
    # --features ohos-log: forward tracing events to hilog (see lb-ffi/src/hilog.rs).
    Write-Host "[ohos-ffi] cargo build -p lb-ffi --target $target --$profile --features ohos-log"
    & cargo build -p lb-ffi --target $target --$profile --features ohos-log 2>&1 |
        ForEach-Object { Write-Host $_ }
    $code = $LASTEXITCODE
} finally {
    Pop-Location
}

if ($code -ne 0) {
    Write-Host "[ohos-ffi] ERROR: cargo build failed (exit $code)" -ForegroundColor Red
    exit $code
}

$candidates = @(
    (Join-Path $env:CARGO_TARGET_DIR "$target\$profile\liblb_ffi.a"),
    (Join-Path $env:CARGO_TARGET_DIR "$target\$profile\liblb_ffi.so")
)
$found = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $found) {
    Write-Host "[ohos-ffi] WARN: artifact not found. Check linker/SDK. Candidates:"
    $candidates | ForEach-Object { Write-Host "  $_" }
    exit 1
}
Write-Host "[ohos-ffi] OK: $found"
Write-Host "[ohos-ffi] Pass to CMake: -DLB_FFI_LIB=$($found.Replace('\','/'))"
