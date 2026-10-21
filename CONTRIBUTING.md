# Contributing

Thanks for taking a look. This is a personal project, so the bar for "fits the scope" is
deliberately narrow — but bug reports and focused patches are very welcome.

## Before you start

- **Small, single-purpose PRs.** One behaviour per PR; keep formatting-only changes separate.
- **Bug reports**: include OS / HarmonyOS version, what the two ends showed (已连接 / 对端离线 / …),
  and the relevant lines from the logs:
  - PC: `%LOCALAPPDATA%\LanBridge\logs\lanbridge.log.<date>` (`LANBRIDGE_LOG=debug` for detail)
  - Phone: `scripts/ohos-log.ps1` (hilog tag `lanbridge`)
- **Protocol changes** are breaking: update `src/protocol/SPEC.md`, add/extend a vector under
  `src/protocol/vectors/`, bump `PROTOCOL_VERSION`, and say so in the PR.

## Checks that must pass

```powershell
cd src
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

For UI changes, at least build the shell you touched (`npm run build` for the Windows shell,
`hvigor assembleHap` for the phone).

## Scope / non-goals

Not planned: cloud relay, accounts, QUIC, Android, Windows↔Windows, HarmonyOS↔HarmonyOS,
explorer shell extensions, telemetry.

## Data-plane red lines

The transfer path is the hot path: no per-chunk logging, no per-chunk allocation/copy, no default
compression, no full-file hashing by default. If you touch it, include a before/after measurement
(`cargo test -p lb-transfer --test bench_loopback --release`).
