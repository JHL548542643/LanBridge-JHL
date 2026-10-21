# Enable ohos.permission.READ_PASTEBOARD on local DevEco debug signing (personal sideload).
# Run elevated (Administrator): right-click PowerShell -> Run as administrator, then:
#   cd <repo root>
#   powershell -ExecutionPolicy Bypass -File .\scripts\ohos-enable-pasteboard-acl.ps1
#
# Why: READ_PASTEBOARD is a restricted ACL. Auto-generated debug Profile must list it in
# allowed-acls or hdc install fails with 9568289. Official alternative is PasteButton only
# (wrong UX for LanBridge). Personal self-use path = patch local UnsignedReleasedProfileTemplate
# then regenerate DevEco automatic signature.

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot "lib.ps1")
$deveco = Get-DevEcoHome
if (-not $deveco) { throw "DevEco Studio not found. Run scripts\check-env.ps1 first." }
$tpl = Join-Path $deveco "sdk\default\openharmony\toolchains\lib\UnsgnedReleasedProfileTemplate.json"
$bakDir = Join-Path $root ".tmp"
New-Item -ItemType Directory -Force -Path $bakDir | Out-Null
$bak = Join-Path $bakDir "UnsgnedReleasedProfileTemplate.json.bak"

if (-not (Test-Path $tpl)) {
  throw "Template not found: $tpl"
}

Copy-Item $tpl $bak -Force
Write-Host "[ok] backup -> $bak"

$raw = Get-Content $tpl -Raw -Encoding UTF8
if ($raw -notmatch "allowed-acls") {
  throw "Template missing allowed-acls field"
}
$patched = $raw -replace '"allowed-acls"\s*:\s*\[[^\]]*\]', '"allowed-acls": [ "ohos.permission.READ_PASTEBOARD" ]'
if ($patched -notmatch "READ_PASTEBOARD") {
  throw "Patch failed"
}
Set-Content -Path $tpl -Value $patched -Encoding UTF8
Write-Host "[ok] template allowed-acls includes READ_PASTEBOARD"

Write-Host ""
Write-Host "Next (required - regenerates debug Profile with ACL):"
Write-Host "  1. Open DevEco Studio -> File -> Project Structure -> Signing Configs"
Write-Host "  2. Check 'Automatically generate signature' (or uncheck/recheck to force regen)"
Write-Host "  3. Apply/OK, then tell the agent: rebuild"
Write-Host "  4. Phone will prompt for clipboard permission once"
Write-Host ""
Write-Host "Restore template later from: $bak"
