# Security

## What LanBridge is

A LAN-only peer-to-peer tool. Two devices discover each other over UDP, then talk over mutually
authenticated TLS 1.3 on port **37532**. There is no server, no account and no telemetry.

## Threat model

Assumed attacker: someone on the same LAN who can sniff, spoof or MITM traffic, or connect to the
listening port.

| Property | How |
|---|---|
| Confidentiality | TLS 1.3 on every connection, including the data channels; no plaintext fallback path |
| Peer identity | Certificate fingerprints are pinned at pairing time; a reconnect must present the same certificate |
| Pairing proof | The 6-digit code keys an HMAC over both TLS fingerprints, domain-separated per role (`lb-pair-guest-v0` / `lb-pair-host-v0`). The code itself is **never** sent on the wire |
| Pairing brute force | Every failed attempt (bad handshake, bad code, aborted) costs the peer a linearly growing delay (250 ms → 5 s cap), so a 120 s pairing window only affords tens of guesses out of 10^6 |
| Unpair | A `Bye` frame is only honoured inside a fingerprint-pinned session; unauthenticated peers cannot clear trust |
| File paths | Inbound relative paths are sanitised (no `..`, no absolute, no drive/UNC), every path component is checked for symlinks, and the resolved parent must stay under the destination root; existing files are never silently overwritten |
| Resource limits | Frames ≤ 1 MiB, clipboard text ≤ 1 MiB, clipboard PNG ≤ 16 MiB, one file ≤ 64 GiB, directory entries ≤ 100 000 |
| Logs | Sizes, device ids and protocol outcomes only — never clipboard bodies or file contents |

### Known limits (by design, not bugs)

- **The 6-digit code is short** (20 bits). It is a convenience layer on top of certificate pinning
  and a time-boxed window with the throttle above. If your LAN has an active attacker during
  pairing, compare the device name shown on both screens.
- **No forward secrecy against a stolen device key.** The identity key lives in the user profile
  (unencrypted); re-pair instead of copying the profile around.
- **A paired peer can fill your disk** by offering a large file while you are receiving; the
  receiver pre-sizes the target file. Only pair with devices you control.
- **HarmonyOS forbids reading the clipboard in the background**, so "phone copy → PC" only works
  while the app is in the foreground.
- Traffic is not hidden: an observer on the LAN sees the two devices talking (metadata only — the
  content is TLS-protected).

## Reporting a vulnerability

Please open a **private security advisory** on the repository (Security → Report a vulnerability)
rather than a public issue. Include repro steps and the affected version/commit. This is a side
project, so replies can take a while — but security reports jump the queue.
