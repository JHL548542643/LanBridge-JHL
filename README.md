# LanBridge-JHL

> **Independent project.** The name is a play on "LAN bridge" and is qualified with `-JHL` on purpose:
> there are unrelated products called *LanBridge* (a commercial remote-desktop service, an old Linksys
> device, a design award entry). Nothing here is affiliated with, endorsed by, or derived from them.
> Bundle ids keep the historical `com.lanbridge.*` prefix so existing installs keep their pairing data.

Fast file transfer and a shared clipboard between **Windows** and **HarmonyOS** phones —
straight over the LAN. No cloud, no relay server, no account.

[中文说明见下方](#中文)

> Status: phase-1 feature-complete for personal use; not on any app store.
> Protocol v0 — see [`src/protocol/SPEC.md`](src/protocol/SPEC.md).

## What it does

| | |
|---|---|
| **Transfer** | Files and folders, resume, multiple parallel TLS streams, bandwidth probing before a transfer |
| **Clipboard** | Text and images, both directions (phone → PC works while the app is in the foreground) |
| **Pairing** | 6-digit code, pinned certificate fingerprints, mutual TLS on every connection |
| **Status sync** | `Bye` on unpair propagates in < 3 s; a 3 s heartbeat flips the peer to "offline" in ≤ 15 s |
| **No cloud** | Discovery over UDP broadcast + optional manual IP; nothing leaves your LAN |

## Architecture

| Layer | Tech |
|---|---|
| Shared core | Rust (`src/crates`: protocol, crypto, session, transfer, clipboard, core, ffi) |
| Windows UI | Tauri 2 + Vue 3 (`apps/win-shell`) |
| HarmonyOS UI | ArkTS + ArkUI + NAPI (`apps/ohos-shell`), data plane stays in Rust |
| Transport | TCP + TLS 1.3, multiple streams on the control port (default **37532**) |

## Build

Prerequisites: Rust (stable, 1.85+), Node.js 20+, and — for the phone — DevEco Studio + a
HarmonyOS device with USB debugging.

```powershell
# core tests
cd src
cargo test --workspace

# Windows app
cd ..\apps\win-shell
npm install
npm run tauri dev          # or: npm run tauri build
cd ..\..
.\scripts\install-win.ps1 # copies the release build to %LOCALAPPDATA%\Programs\LanBridge

# HarmonyOS app (needs DevEco; see apps/ohos-shell/README.md for signing)
.\scripts\ohos-hap.ps1 -Install -Launch
```

The PowerShell helpers under `scripts/` are convenience wrappers; the plain `cargo` / `npm` /
`hvigor` commands work without them.

## Privacy & security

LAN-only, mTLS with pinned fingerprints, the 6-digit code never goes over the wire (it only keys an
HMAC over the TLS fingerprints). Logs record sizes and device ids — never clipboard contents or file
data. See [SECURITY.md](SECURITY.md) for the threat model and how to report a problem.

## Licence

Dual-licensed under **MIT OR Apache-2.0** — pick whichever you prefer.

## 中文

> 本项目与任何同名商业产品无关；仓库名带 `-JHL` 以作区分。

局域网内 **Windows ↔ 鸿蒙手机** 的高速文件传输 + 跨机剪贴板（文本/图片），不依赖任何外部服务与账号。

- **传输**：多路 TLS 并发、目录、断点续传、传输前带宽探测
- **剪贴板**：双向文本与图片（受系统限制，手机→PC 仅在 App 前台时可用）
- **配对**：6 位码 + 证书指纹固定 + 全链路 mTLS；取消配对会立即通知对端
- **状态**：3 秒心跳，断网/被杀 15 秒内显示「对端离线」
- **构建**：Windows 用 Tauri 2 + Vue 3；鸿蒙用 ArkTS + NAPI，数据面仍是 Rust

协议见 [`src/protocol/SPEC.md`](src/protocol/SPEC.md)；安全模型见 [SECURITY.md](SECURITY.md)。
许可证：**MIT 或 Apache-2.0 双许可**。
