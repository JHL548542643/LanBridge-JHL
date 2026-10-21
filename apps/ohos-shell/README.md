# LanBridge HarmonyOS shell (ArkTS + NAPI)

UI plus a NAPI bridge over `lb-ffi`; the transport stays in Rust.

## Open in DevEco

1. Copy `build-profile.json5.example` → `build-profile.json5`（后者已 gitignore，含本机签名口令，勿提交）
2. DevEco Studio → Open → `<repo root>/apps/ohos-shell`
3. File → Project Structure → Signing Configs → Automatically generate signature
4. Sync SDK / ohpm as prompted
5. Build the Rust FFI for the phone ABI, then point CMake at it (see below)
6. Run on device

## Build `lb_ffi` for OHOS

```powershell
cd <repo root>
. .\scripts\env.ps1
.\scripts\build-ohos-ffi.ps1 -Release
```

Requires DevEco OHOS native (`env.ps1` sets `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER`).
The script writes `target/aarch64-unknown-linux-ohos/release/liblb_ffi.a` and prints the path.

In DevEco **entry → build-profile / CMake arguments**:

```text
-DLB_FFI_LIB=<repo root>/target/aarch64-unknown-linux-ohos/release/liblb_ffi.a
```

## Then

```powershell
.\scripts\ohos-hap.ps1 -Install -Launch   # FFI -> HAP -> install -> start
.\scripts\ohos-log.ps1 -Clear            # hilog filtered to this app
```

## Notes

- Control port **37532**; both devices must sit on the same LAN / hotspot.
- `READ_PASTEBOARD` is a restricted ACL: sideloading needs the local profile patch
  (`scripts/ohos-enable-pasteboard-acl.ps1`, admin) or `hdc install` fails with 9568289.
- HarmonyOS forbids background clipboard reads, so "phone copy → PC" only works while the app is
  in the foreground.
