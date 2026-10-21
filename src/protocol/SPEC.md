# LanBridge Protocol v0

> Wire-format authority. Breaking changes bump `PROTOCOL_VERSION`.
> Change this file in the same commit as codec + test vectors.

## Goals (v0)

- Single encrypted session for control, file, and clipboard (message types distinguish traffic).
- Forward compatible: unknown message types are skipped, not fatal.
- Handshake carries: protocol version, `features` bitmap, device id, certificate fingerprint.

## Framing

All **control** messages after TLS use this frame:

| Field        | Size     | Notes                                      |
|--------------|----------|--------------------------------------------|
| `magic`      | 4 bytes  | ASCII `LBfr` (`0x4C 0x42 0x66 0x72`)       |
| `version`    | u16 BE   | `PROTOCOL_VERSION` (v0 = `0`)              |
| `msg_type`   | u16 BE   | see registry                               |
| `payload_len`| u32 BE   | payload bytes; max `1_048_576` (1 MiB)     |
| `payload`    | N bytes  | message-specific                           |

Total header = 12 bytes.

### File data plane (M2)

Bandwidth probe (once per link, on the control stream): `ProbeRequest`, then exactly `byte_count` raw bytes, then `ProbeResult` from the receiver. Both sides run the same planner on those numbers to choose stream count and chunk size.

Data plane: after `OpenChannels` / `ChannelsReady`, the sender opens N extra TLS connections (trusted fingerprints) and sends `ChannelHello`. File bytes are **not** carried on the control stream. For each file, control does `FileOffer` / `FileAccept` / `FileComplete`; each data connection sends one `SliceHeader` then exactly `length` raw bytes. N and the slice split are deterministic from `(resume_offset, file_size, streams)`.

## Features bitmap (`u32`)

| Bit | Name        | Meaning                          |
|-----|-------------|----------------------------------|
| 0   | `FILE`      | File transfer (M2)               |
| 1   | `CLIPBOARD` | Text/image clipboard (M3)        |
| 2   | `RESUME`    | Resume / partial transfer (M2)   |

Intersected = AND of both peers' advertised bits after Hello.

## Message registry

| Type | Name           | Direction     | Payload                                      |
|------|----------------|---------------|----------------------------------------------|
| 0x0001 | `Hello`      | either→peer   | see Hello                                    |
| 0x0002 | `HelloAck`   | response      | same layout as Hello                         |
| 0x0010 | `PairOffer`  | host→guest    | reserved                                     |
| 0x0011 | `PairAccept` | guest→host    | device + fingerprint (**no** pairing code) |
| 0x0012 | `PairConfirm`| host→guest    | host device + fingerprint                    |
| 0x0013 | `PairReject` | either        | `u16` reason                                 |
| 0x0014 | `PairVerify` | guest→host then host→guest | 32-byte HMAC tag (role-domain separated) |
| 0x0020 | `Ping`       | either        | `u64` opaque nonce                           |
| 0x0021 | `Pong`       | response      | echo nonce                                   |
| 0x0030 | `Bye`        | either        | empty                                        |
| 0x0100 | `FileOffer`  | sender→recv   | relative path + size                         |
| 0x0101 | `FileAccept` | recv→sender   | resume offset                                |
| 0x0102 | `FileReject` | recv→sender   | `u16` reason                                 |
| 0x0103 | `FileComplete` | either      | status + bytes transferred                   |
| 0x0110 | `DirOffer`     | sender→recv | relative directory + file count              |
| 0x0111 | `DirComplete`  | sender→recv | status + files completed                     |
| 0x0120 | `ProbeRequest` | sender→recv | `u64` raw byte count (max 16 MiB)            |
| 0x0121 | `ProbeResult`  | recv→sender | bytes + elapsed microseconds                 |
| 0x0140 | `OpenChannels` | sender→recv | stream count + token                         |
| 0x0141 | `ChannelsReady`| recv→sender | empty                                        |
| 0x0142 | `ChannelHello` | data conn   | index + token                                |
| 0x0143 | `SliceHeader`  | data conn   | offset + length, then raw bytes              |
| 0x0200 | `ClipOffer`    | either→peer | kind + seq + byte length                     |
| 0x0201 | `ClipAck`      | response    | seq + status                                 |
| 0x0202 | `ClipReject`   | response    | seq + reason                                 |

Reserved ranges:

- `0x0104..=0x010F`, `0x0112..=0x011F`, `0x0122..=0x013F`, `0x0144..=0x01FF` file (future)
- `0x0203..=0x02FF` clipboard (future)
- `0xF000..=0xFFFF` private / experimental (must still be skippable)

## Hello / HelloAck payload

| Field            | Size / encoding        |
|------------------|------------------------|
| `protocol_version` | u16 BE               |
| `features`         | u32 BE               |
| `device_id_len`    | u8                   |
| `device_id`        | 1..=64 UTF-8 bytes   |
| `fingerprint`      | 32 bytes SHA-256     |
| `alias_len`        | u8                   |
| `alias`            | 0..=32 UTF-8 bytes   |

## PairAccept payload

| Field            | Size / encoding        |
|------------------|------------------------|
| `device_id_len`  | u8                     |
| `device_id`      | 1..=64 UTF-8           |
| `fingerprint`    | 32 bytes               |

Must equal the TLS peer certificate fingerprint observed by the receiver.

## PairConfirm payload

| Field            | Size / encoding        |
|------------------|------------------------|
| `device_id_len`  | u8                     |
| `device_id`      | 1..=64 UTF-8           |
| `fingerprint`    | 32 bytes               |

## PairVerify payload

| Field | Size |
|-------|------|
| `tag` | 32 bytes |

Handshake order (mandatory):

1. Guest → Host: `PairVerify` with **Guest-domain** tag  
2. Host verifies; on failure `PairReject` / close (Host **never** sends the Guest-domain tag)  
3. Host → Guest: `PairVerify` with **Host-domain** tag  
4. Guest verifies Host-domain tag  

```
tag = HMAC-SHA256(key = UTF-8 pairing code,
                  msg = domain || min(fp_a, fp_b) || max(fp_a, fp_b))

domain_guest = ASCII "lb-pair-guest-v0"
domain_host  = ASCII "lb-pair-host-v0"
```

`fp_a` / `fp_b` are SHA-256 fingerprints of the **TLS** certificates for this connection.
The 6-digit pairing code is **never** sent on the wire. Domains differ so a Host that previously leaked a tag cannot be offline-brute-forced from a Guest-domain MAC. A MITM that presents different certs cannot forge a matching tag without the code.

Pairing control steps SHOULD time out (implementation: ~30s/step; accept window ~120s).

A host MUST throttle failed pairing attempts inside one window: the code is only 20 bits, so an
unthrottled window lets a LAN attacker guess at handshake speed. Implementation: every failed
attempt (TLS failure, aborted handshake, wrong code) delays the next accept by a linear backoff
(250 ms × failures, capped at 5 s).

## PairReject reasons

| Code | Meaning                |
|------|------------------------|
| 1    | bad pairing / verify   |
| 2    | expired / no session   |
| 3    | version mismatch       |
| 4    | feature mismatch       |
| 5    | internal error         |
| 6    | fingerprint mismatch   |

## Bye and liveness

`Bye` (`0x0030`) has an **empty** payload and means "I am leaving: unpairing / shutting down".

Rules:

- A `Bye` is only meaningful inside an **established, fingerprint-pinned** session. A frame that
  arrives on a connection that did not pass the pairing / trusted handshake MUST be ignored — it may
  never clear trust (otherwise anyone on the LAN could unpair two devices).
- The receiver MUST immediately drop the sender's trust (delete the pinned peer) and close the
  connection. The two shells then show "peer left (unpaired)" and require a fresh pairing.
- The sender MUST write `Bye` and flush it **before** closing the stream (`close_notify`). Closing
  first leaves the receiver with a bare EOF, which is indistinguishable from a crash.

Liveness (implementation policy, not wire format): while a pair exists, each side sends `Ping` to
the other every **3 s** over a trusted session and expects `Pong`. No authenticated frame from the
peer for **12 s** (four missed heartbeats) means "peer offline"; the acceptance bound is 15 s.
Liveness is derived from authenticated control traffic only — discovery beacons never make a peer
look connected.

## FileOffer payload

| Field | Size / encoding |
|-------|-----------------|
| `path_len` | u16 BE |
| `path` | UTF-8 relative path (`/` separators; no `..`, no absolute) |
| `file_size` | u64 BE total bytes |

## FileAccept payload

| Field | Size |
|-------|------|
| `resume_offset` | u64 BE (bytes already present at receiver; append from here) |

## FileReject reasons

| Code | Meaning |
|------|---------|
| 1 | bad path |
| 2 | unsupported |
| 3 | disk / io |
| 4 | busy |

## DirOffer payload

| Field | Size / encoding |
|-------|-----------------|
| `path_len` | u16 BE |
| `path` | UTF-8 relative directory |
| `file_count` | u32 BE |

## DirComplete payload

| Field | Size |
|-------|------|
| `status` | u16 BE (`0` = ok) |
| `files_ok` | u32 BE |

## ProbeRequest / ProbeResult

| Message | Payload |
|---------|---------|
| `ProbeRequest` | `u64` BE byte count (1..=16 MiB) |
| `ProbeResult` | `u64` BE bytes received, `u64` BE elapsed microseconds |

## OpenChannels / ChannelHello / SliceHeader

| Message | Payload |
|---------|---------|
| `OpenChannels` | `u16` BE stream count (1..=8), `u64` BE token |
| `ChannelsReady` | empty |
| `ChannelHello` | `u16` BE index, `u64` BE token |
| `SliceHeader` | `u64` BE file offset, `u64` BE length, then raw bytes |

## FileComplete payload

| Field | Size |
|-------|------|
| `status` | u16 BE (`0` = ok) |
| `bytes_transferred` | u64 BE |

**Sender completion:** after writing `FileComplete`, the sender **MUST** flush the control TLS stream, then send a TLS `close_notify` (graceful shutdown) before dropping the connection. Abrupt TCP close without `close_notify` makes rustls on the receiver report unexpected EOF and can abort an otherwise complete receive.

**Receiver tolerance (not the happy path):** if `FileComplete` is missing after the data pump finishes, or a data/control stream ends with abrupt TLS EOF, an implementation MAY accept the file when on-disk size already matches `FileOffer.file_size` (bounded wait for `FileComplete`: ~15s) and log a warning. This is fault tolerance only; normal completion is flush + `close_notify` + `FileComplete`.

## Clipboard (M3)

Maps to Win `CF_UNICODETEXT` / PNG-as-file-or-DIB later, and HarmonyOS `MIMETYPE_TEXT_PLAIN` / PNG bridge for `PixelMap` (shells apply OS APIs; wire format stays PNG + UTF-8).

After `ClipOffer`, the sender writes exactly `byte_len` **raw** bytes on the **control** TLS stream (not multipath), then waits for `ClipAck`. Latency path only — no probe/channel setup.

| Message | Payload |
|---------|---------|
| `ClipOffer` | `u8` kind (`1`=text/plain UTF-8, `2`=image/png), `u64` seq, `u64` byte_len (text ≤1 MiB, png ≤16 MiB) |
| `ClipAck` | `u64` seq, `u16` status (`0`=ok) |
| `ClipReject` | `u64` seq, `u16` reason (`1` unsupported, `2` too large, `3` busy) |

Logs must never contain clipboard body.

## Security notes (v0)

- Application payload only after TLS 1.3 with mutual certificate authentication.
- Certificate fingerprints are pinned at pairing time; subsequent sessions must match.
- Pairing short code stays local; identity binding uses `PairVerify` HMAC over TLS fingerprints.
- File paths are sanitized; never silently overwrite (rename on conflict).
- No plaintext product path.

## Test vectors

Canonical hex fixtures live under `vectors/` and are asserted by `lb-protocol` unit tests.
