$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$src = Join-Path $root 'apps\win-shell\src-tauri\target\release\win-shell.exe'
if (-not (Test-Path $src)) { throw "release exe not found: $src" }
$dir = Join-Path $env:LOCALAPPDATA 'Programs\LanBridge'
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Get-Process -Name LanBridge,win-shell -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 400
Copy-Item $src (Join-Path $dir 'LanBridge.exe') -Force
Copy-Item (Join-Path $root 'assets\branding\icon.ico') (Join-Path $dir 'LanBridge.ico') -Force

$ws = New-Object -ComObject WScript.Shell
$startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
$lnk = $ws.CreateShortcut((Join-Path $startMenu 'LanBridge.lnk'))
$lnk.TargetPath = Join-Path $dir 'LanBridge.exe'
$lnk.WorkingDirectory = $dir
$lnk.IconLocation = (Join-Path $dir 'LanBridge.ico')
$lnk.Description = 'LanBridge - 局域网传输与剪贴板'
$lnk.Save()

$desktop = [Environment]::GetFolderPath('Desktop')
$lnk2 = $ws.CreateShortcut((Join-Path $desktop 'LanBridge.lnk'))
$lnk2.TargetPath = Join-Path $dir 'LanBridge.exe'
$lnk2.WorkingDirectory = $dir
$lnk2.IconLocation = (Join-Path $dir 'LanBridge.ico')
$lnk2.Save()

$uninstKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\LanBridge'
New-Item -Path $uninstKey -Force | Out-Null
Set-ItemProperty $uninstKey -Name DisplayName -Value 'LanBridge'
Set-ItemProperty $uninstKey -Name DisplayVersion -Value '0.1.0'
Set-ItemProperty $uninstKey -Name Publisher -Value 'LanBridge'
Set-ItemProperty $uninstKey -Name DisplayIcon -Value (Join-Path $dir 'LanBridge.ico')
Set-ItemProperty $uninstKey -Name InstallLocation -Value $dir
Set-ItemProperty $uninstKey -Name UninstallString -Value ('powershell -NoProfile -Command "Remove-Item -Recurse -Force ''' + $dir + '''; Remove-Item -Recurse -Force ''' + $uninstKey + '''"')
Set-ItemProperty $uninstKey -Name NoModify -Value 1 -Type DWord
Set-ItemProperty $uninstKey -Name NoRepair -Value 1 -Type DWord

"installed to $dir"
Get-ChildItem $dir | Select-Object Name,Length | Format-Table -AutoSize | Out-String
(Get-Item (Join-Path $dir 'LanBridge.exe')).VersionInfo | Select-Object FileDescription,ProductName,FileVersion | Format-List | Out-String
