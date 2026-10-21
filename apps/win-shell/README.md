# LanBridge Windows shell (Tauri 2 + Vue 3)

Thin UI over `lb-core`. Protocol / TLS / transfer live in the Rust crates under `src/`.

## Dev

```powershell
cd <repo root>
. .\scripts\env.ps1
cd apps\win-shell
npm install
npm run tauri dev
```

Smoke: 本机 / 接收 / 发送；主题 浅色·深色·跟系统；调试折叠。

## Build / install

```powershell
cd <repo root>\apps\win-shell
npm run tauri build            # -> src-tauri\target\release\win-shell.exe
cd ..\..
.\scripts\install-win.ps1     # copies to %LOCALAPPDATA%\Programs\LanBridge + shortcuts
```

## Notes

- Control port defaults to **37532** (`DEFAULT_CONTROL_PORT`); the phone must reach it.
- Logs: `%LOCALAPPDATA%\LanBridge\logs\lanbridge.log.<date>` (`LANBRIDGE_LOG=debug` for detail).
