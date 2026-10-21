//! C ABI for platform shells. Keep surface small; no UI types.
//!
//! Session model: pair once (`lb_host_await_pair` / `lb_guest_pair`), then
//! trusted reconnect for file/clipboard without a new pairing code.
//!
//! # Safety
//! Callers must pass valid UTF-8 C strings and free buffers returned via
//! [`lb_string_free`].

#![allow(clippy::missing_safety_doc)]

use lb_core::{
    establish_receiver, establish_sender, generate_pairing_code, pair_as_guest, pair_as_host,
    push_clipboard, recv_clipboard, recv_file, send_file, shutdown_link, ClipboardItem,
    DeviceIdentity, DeviceSession, GuestService, HostService, InboundEvent, InboundRead, MsgType,
    RecvOptions, SendOptions,
};
use lb_protocol::{Features, PROTOCOL_VERSION};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::PathBuf;
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

#[cfg(feature = "ohos-log")]
mod hilog;

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Runtime::new().expect("tokio runtime")
    })
}

fn host_slot() -> &'static Mutex<Option<HostService>> {
    static SLOT: OnceLock<Mutex<Option<HostService>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn guest_slot() -> &'static Mutex<Option<GuestService>> {
    static SLOT: OnceLock<Mutex<Option<GuestService>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

fn session_cell() -> &'static tokio::sync::Mutex<Option<DeviceSession>> {
    static CELL: OnceLock<tokio::sync::Mutex<Option<DeviceSession>>> = OnceLock::new();
    CELL.get_or_init(|| tokio::sync::Mutex::new(None))
}

struct LiveShare {
    progress: std::sync::Mutex<Arc<lb_core::ByteProgress>>,
    started_ms: std::sync::Mutex<Arc<std::sync::atomic::AtomicU64>>,
    kind_live: std::sync::Mutex<Arc<std::sync::atomic::AtomicU8>>,
    name_live: std::sync::Mutex<Arc<std::sync::Mutex<String>>>,
    last_json: std::sync::Mutex<String>,
}

fn live_share() -> &'static LiveShare {
    static LIVE: OnceLock<LiveShare> = OnceLock::new();
    LIVE.get_or_init(|| LiveShare {
        progress: std::sync::Mutex::new(Arc::new(lb_core::ByteProgress::default())),
        started_ms: std::sync::Mutex::new(Arc::new(std::sync::atomic::AtomicU64::new(0))),
        kind_live: std::sync::Mutex::new(Arc::new(std::sync::atomic::AtomicU8::new(0))),
        name_live: std::sync::Mutex::new(Arc::new(std::sync::Mutex::new(String::new()))),
        last_json: std::sync::Mutex::new("{}".into()),
    })
}

fn bind_live(svc: &DeviceSession) {
    let live = live_share();
    if let Ok(mut p) = live.progress.lock() {
        *p = svc.progress_handle();
    }
    if let Ok(mut s) = live.started_ms.lock() {
        *s = svc.started_ms_handle();
    }
    if let Ok(mut k) = live.kind_live.lock() {
        *k = svc.kind_live_handle();
    }
    if let Ok(mut n) = live.name_live.lock() {
        *n = svc.name_live_handle();
    }
}

fn status_json_live(mut status: lb_core::SessionStatus) -> String {
    let live = live_share();
    let progress = live
        .progress
        .lock()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();
    let started = live
        .started_ms
        .lock()
        .ok()
        .map(|g| g.load(std::sync::atomic::Ordering::Relaxed))
        .unwrap_or(0);
    let kind = live
        .kind_live
        .lock()
        .ok()
        .map(|g| g.load(std::sync::atomic::Ordering::Relaxed))
        .unwrap_or(0);
    let name = live
        .name_live
        .lock()
        .ok()
        .and_then(|g| g.lock().ok().map(|n| n.clone()))
        .unwrap_or_default();
    DeviceSession::apply_live_transfer(&mut status, progress.as_ref(), started, kind, &name);
    serde_json::to_string(&status).unwrap_or_else(|_| "{}".into())
}

fn status_json(svc: &DeviceSession) -> String {
    status_json_live(svc.status())
}

/// Write session into the process-wide cell (open path).
fn put_session(svc: DeviceSession) {
    bind_live(&svc);
    let json = status_json(&svc);
    if let Ok(mut g) = live_share().last_json.lock() {
        *g = json;
    }
    let mut slot = runtime().block_on(session_cell().lock());
    *slot = Some(svc);
}

fn short_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| format!("{:x}", d.as_millis()))
        .unwrap_or_else(|_| "0".into())
}

fn clear_last_error() {
    if let Ok(mut g) = last_error_slot().lock() {
        g.clear();
    }
}

fn set_last_error(msg: impl Into<String>) {
    if let Ok(mut g) = last_error_slot().lock() {
        *g = msg.into();
    }
}

fn last_error_slot() -> &'static Mutex<String> {
    static SLOT: OnceLock<Mutex<String>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(String::new()))
}

fn fail(code: i32, msg: impl Into<String>) -> i32 {
    set_last_error(msg);
    code
}

fn to_cstring(s: String) -> *mut c_char {
    CString::new(s)
        .map(|c| c.into_raw())
        .unwrap_or(ptr::null_mut())
}

fn cstr<'a>(p: *const c_char) -> Result<&'a str, i32> {
    if p.is_null() {
        return Err(2);
    }
    unsafe { CStr::from_ptr(p).to_str().map_err(|_| 2) }
}

/// Last failure detail from FFI (UTF-8). Null if none. Free with [`lb_string_free`].
#[no_mangle]
pub extern "C" fn lb_last_error() -> *mut c_char {
    match last_error_slot().lock() {
        Ok(g) if !g.is_empty() => to_cstring(g.clone()),
        _ => ptr::null_mut(),
    }
}

/// Caller must free with [`lb_string_free`].
#[no_mangle]
pub unsafe extern "C" fn lb_string_free(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    drop(CString::from_raw(s));
}

fn to_bytes(v: Vec<u8>) -> (*mut u8, usize) {
    let mut boxed = v.into_boxed_slice();
    let ptr = boxed.as_mut_ptr();
    let len = boxed.len();
    std::mem::forget(boxed);
    (ptr, len)
}

/// Free buffer from clipboard APIs / loopback PNG.
#[no_mangle]
pub unsafe extern "C" fn lb_bytes_free(p: *mut u8, len: usize) {
    if p.is_null() {
        return;
    }
    drop(Vec::from_raw_parts(p, len, len));
}

#[no_mangle]
pub extern "C" fn lb_protocol_version() -> u16 {
    PROTOCOL_VERSION
}

#[no_mangle]
pub extern "C" fn lb_features_bits() -> u32 {
    Features::M3_CLIPBOARD.0
}

/// Returns heap C string "0x00000007 (FILE|CLIPBOARD|RESUME)"; free with lb_string_free.
#[no_mangle]
pub extern "C" fn lb_features_label() -> *mut c_char {
    let f = Features::M3_CLIPBOARD.0;
    to_cstring(format!("0x{f:08x} (FILE|CLIPBOARD|RESUME)"))
}

/// Returns 6-digit pairing code; free with lb_string_free.
#[no_mangle]
pub extern "C" fn lb_make_pairing_code() -> *mut c_char {
    to_cstring(generate_pairing_code().as_str().to_string())
}

/// Generate identity. `out_*` must be freed with lb_string_free. Returns 0 on success.
#[no_mangle]
pub unsafe extern "C" fn lb_make_identity(
    alias: *const c_char,
    out_device_id: *mut *mut c_char,
    out_alias: *mut *mut c_char,
    out_fp_hex: *mut *mut c_char,
) -> i32 {
    if out_device_id.is_null() || out_alias.is_null() || out_fp_hex.is_null() {
        return 1;
    }
    let alias = if alias.is_null() {
        "Phone".to_string()
    } else {
        match CStr::from_ptr(alias).to_str() {
            Ok(s) => s.to_string(),
            Err(_) => return 2,
        }
    };
    let id = match DeviceIdentity::generate(format!("ohos-{}", short_id()), alias) {
        Ok(v) => v,
        Err(_) => return 3,
    };
    *out_fp_hex = to_cstring(id.fingerprint_hex());
    *out_device_id = to_cstring(id.device_id);
    *out_alias = to_cstring(id.alias);
    0
}

/// Same-process loopback send/recv. Writes bytes to `out_bytes`. Returns 0 on success.
#[no_mangle]
pub unsafe extern "C" fn lb_self_loopback_transfer(out_bytes: *mut u64) -> i32 {
    if out_bytes.is_null() {
        return 1;
    }
    let result = runtime().block_on(async {
        let root = std::env::temp_dir().join(format!("lb-ffi-loop-{}", short_id()));
        let src_dir = root.join("src");
        let dst_dir = root.join("dst");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&src_dir).map_err(|_| 10)?;
        std::fs::create_dir_all(&dst_dir).map_err(|_| 10)?;
        let src_file = src_dir.join("smoke.bin");
        let payload: Vec<u8> = (0..8_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&src_file, &payload).map_err(|_| 10)?;

        let host_id = Arc::new(
            DeviceIdentity::generate(format!("ffi-h-{}", short_id()), "H").map_err(|_| 11)?,
        );
        let guest_id = Arc::new(
            DeviceIdentity::generate(format!("ffi-g-{}", short_id()), "G").map_err(|_| 11)?,
        );
        let code = generate_pairing_code();
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|_| 12)?;
        let addr = listener.local_addr().map_err(|_| 12)?;
        let code_h = code.clone();
        let dest = dst_dir.clone();
        let host = tokio::spawn(async move {
            let (peer, session, _) = pair_as_host(host_id.clone(), &listener, &code_h)
                .await
                .map_err(|_| 13i32)?;
            let mut link = establish_receiver(session, &listener, host_id, &peer)
                .await
                .map_err(|_| 14i32)?;
            recv_file(
                &mut link,
                &RecvOptions {
                    dest_root: dest,
                    overwrite: false,
                    progress: None,
                    live_name: None,
                },
            )
            .await
            .map_err(|_| 15i32)
        });
        let (peer, session) = pair_as_guest(guest_id.clone(), addr, &code)
            .await
            .map_err(|_| 16)?;
        let mut link = establish_sender(session, addr, guest_id, &peer, 256 * 1024)
            .await
            .map_err(|_| 17)?;
        let sent = send_file(
            &mut link,
            &src_file,
            &SendOptions {
                remote_path: "smoke.bin".into(),
                progress: None,
                live_name: None,
            },
        )
        .await
        .map_err(|_| 18)?;
        let _ = shutdown_link(&mut link).await;
        let (recv, _) = host.await.map_err(|_| 19)??;
        if sent.bytes_transferred != recv.bytes_transferred {
            return Err(20);
        }
        Ok(recv.bytes_transferred)
    });
    match result {
        Ok(n) => {
            *out_bytes = n;
            0
        }
        Err(code) => code,
    }
}

// ── Host service (pair once, then trusted recv) ─────────────────────────────

/// Start host listen. Persist identity/trust under `data_dir`.
/// `out_paired`: 1 if trust already present. Free `out_code` with lb_string_free.
#[no_mangle]
pub unsafe extern "C" fn lb_host_start(
    data_dir: *const c_char,
    alias: *const c_char,
    out_port: *mut u16,
    out_code: *mut *mut c_char,
    out_paired: *mut u8,
) -> i32 {
    if data_dir.is_null() || out_port.is_null() || out_code.is_null() || out_paired.is_null() {
        return 1;
    }
    clear_last_error();
    let dir = match cstr(data_dir) {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => return fail(2, "data_dir required"),
    };
    let alias = match cstr(alias) {
        Ok(s) if !s.is_empty() => s.to_string(),
        _ => "Phone-host".to_string(),
    };
    let result = runtime().block_on(async { HostService::start(dir, &alias).await });
    match result {
        Ok((svc, port)) => {
            let code = svc.pairing_code();
            let paired = if svc.is_paired() { 1u8 } else { 0u8 };
            let mut slot = match host_slot().lock() {
                Ok(g) => g,
                Err(_) => return fail(21, "host lock"),
            };
            *slot = Some(svc);
            *out_port = port;
            *out_code = to_cstring(code);
            *out_paired = paired;
            0
        }
        Err(e) => fail(12, format!("host start: {e}")),
    }
}

/// Wait for first-time (or re-)pair. Host must already be started. Keeps listen alive.
#[no_mangle]
pub unsafe extern "C" fn lb_host_await_pair(out_peer_id: *mut *mut c_char) -> i32 {
    clear_last_error();
    let mut svc = {
        let mut slot = match host_slot().lock() {
            Ok(g) => g,
            Err(_) => return fail(21, "host lock"),
        };
        match slot.take() {
            Some(s) => s,
            None => return fail(22, "host not started"),
        }
    };
    let result = runtime().block_on(async { svc.await_pair().await });
    match result {
        Ok(peer) => {
            if let Ok(mut slot) = host_slot().lock() {
                *slot = Some(svc);
            }
            if !out_peer_id.is_null() {
                *out_peer_id = to_cstring(peer.device_id);
            }
            0
        }
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.contains("timeout") { 16 } else { 13 };
            if let Ok(mut slot) = host_slot().lock() {
                *slot = Some(svc);
            }
            fail(code, format!("await_pair: {msg}"))
        }
    }
}

/// Trusted accept + recv one file. Requires prior pair. Host stays listening.
#[no_mangle]
pub unsafe extern "C" fn lb_host_recv_file(
    dest_root: *const c_char,
    out_bytes: *mut u64,
    out_path: *mut *mut c_char,
) -> i32 {
    if dest_root.is_null() || out_bytes.is_null() || out_path.is_null() {
        return 1;
    }
    clear_last_error();
    let dest = match cstr(dest_root) {
        Ok(s) => PathBuf::from(s),
        Err(c) => return c,
    };
    let mut svc = {
        let mut slot = match host_slot().lock() {
            Ok(g) => g,
            Err(_) => return fail(21, "host lock"),
        };
        match slot.take() {
            Some(s) => s,
            None => return fail(22, "host not started"),
        }
    };
    if !svc.is_paired() {
        if let Ok(mut slot) = host_slot().lock() {
            *slot = Some(svc);
        }
        return fail(26, "not paired — call await_pair first");
    }
    let result = runtime().block_on(async { svc.recv_file(dest).await });
    match result {
        Ok((n, path)) => {
            if let Ok(mut slot) = host_slot().lock() {
                *slot = Some(svc);
            }
            *out_bytes = n;
            *out_path = to_cstring(path.to_string_lossy().into_owned());
            0
        }
        Err(e) => {
            let msg = e.to_string();
            let code = if msg.contains("timed out") || msg.contains("timeout") {
                16
            } else {
                15
            };
            if let Ok(mut slot) = host_slot().lock() {
                *slot = Some(svc);
            }
            fail(code, format!("recv_file: {msg}"))
        }
    }
}

/// Trusted accept + recv clipboard text. Free `*out_text` with lb_string_free.
#[no_mangle]
pub unsafe extern "C" fn lb_host_recv_clipboard_text(out_text: *mut *mut c_char) -> i32 {
    if out_text.is_null() {
        return 1;
    }
    clear_last_error();
    let mut svc = {
        let mut slot = match host_slot().lock() {
            Ok(g) => g,
            Err(_) => return fail(21, "host lock"),
        };
        match slot.take() {
            Some(s) => s,
            None => return fail(22, "host not started"),
        }
    };
    if !svc.is_paired() {
        if let Ok(mut slot) = host_slot().lock() {
            *slot = Some(svc);
        }
        return fail(26, "not paired");
    }
    let result = runtime().block_on(async { svc.recv_clipboard().await });
    match result {
        Ok(ClipboardItem::Text(t)) => {
            if let Ok(mut slot) = host_slot().lock() {
                *slot = Some(svc);
            }
            *out_text = to_cstring(t);
            0
        }
        Ok(ClipboardItem::ImagePng(_)) => {
            if let Ok(mut slot) = host_slot().lock() {
                *slot = Some(svc);
            }
            fail(34, "got image; use lb_host_recv_clipboard")
        }
        Err(e) => {
            if let Ok(mut slot) = host_slot().lock() {
                *slot = Some(svc);
            }
            fail(30, format!("recv_clipboard: {e}"))
        }
    }
}

/// Trusted accept + recv text or PNG.
/// `out_kind`: 1=text UTF-8, 2=PNG. Free `*out_bytes` with [`lb_bytes_free`].
#[no_mangle]
pub unsafe extern "C" fn lb_host_recv_clipboard(
    out_kind: *mut u8,
    out_bytes: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    if out_kind.is_null() || out_bytes.is_null() || out_len.is_null() {
        return 1;
    }
    clear_last_error();
    let mut svc = {
        let mut slot = match host_slot().lock() {
            Ok(g) => g,
            Err(_) => return fail(21, "host lock"),
        };
        match slot.take() {
            Some(s) => s,
            None => return fail(22, "host not started"),
        }
    };
    if !svc.is_paired() {
        if let Ok(mut slot) = host_slot().lock() {
            *slot = Some(svc);
        }
        return fail(26, "not paired");
    }
    let result = runtime().block_on(async { svc.recv_clipboard().await });
    match result {
        Ok(item) => {
            if let Ok(mut slot) = host_slot().lock() {
                *slot = Some(svc);
            }
            let (kind, bytes) = match item {
                ClipboardItem::Text(t) => (1u8, t.into_bytes()),
                ClipboardItem::ImagePng(b) => (2u8, b.to_vec()),
            };
            let (ptr, len) = to_bytes(bytes);
            *out_kind = kind;
            *out_bytes = ptr;
            *out_len = len;
            0
        }
        Err(e) => {
            if let Ok(mut slot) = host_slot().lock() {
                *slot = Some(svc);
            }
            fail(30, format!("recv_clipboard: {e}"))
        }
    }
}

/// Drop host listen socket.
#[no_mangle]
pub extern "C" fn lb_host_stop() -> i32 {
    match host_slot().lock() {
        Ok(mut g) => {
            *g = None;
            0
        }
        Err(_) => fail(21, "host lock"),
    }
}

// ── Guest service (pair once, then trusted send) ────────────────────────────

/// Open guest with persisted trust/endpoint under `data_dir`.
/// `out_paired` 1 if ready to send. Free host/peer strings with lb_string_free.
#[no_mangle]
pub unsafe extern "C" fn lb_guest_open(
    data_dir: *const c_char,
    alias: *const c_char,
    out_paired: *mut u8,
    out_host: *mut *mut c_char,
    out_port: *mut u16,
    out_peer_id: *mut *mut c_char,
) -> i32 {
    if data_dir.is_null() || out_paired.is_null() {
        return 1;
    }
    clear_last_error();
    let dir = match cstr(data_dir) {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => return fail(2, "data_dir required"),
    };
    let alias = match cstr(alias) {
        Ok(s) if !s.is_empty() => s.to_string(),
        _ => "Phone".to_string(),
    };
    let svc = match GuestService::open(dir, &alias) {
        Ok(s) => s,
        Err(e) => return fail(11, format!("guest open: {e}")),
    };
    let paired = if svc.is_paired() { 1u8 } else { 0u8 };
    if !out_host.is_null() {
        *out_host = match svc.remembered_host() {
            Some(h) => to_cstring(h),
            None => ptr::null_mut(),
        };
    }
    if !out_port.is_null() {
        *out_port = svc.remembered_port().unwrap_or(0);
    }
    if !out_peer_id.is_null() {
        *out_peer_id = match svc.peer_device_id() {
            Some(id) => to_cstring(id),
            None => ptr::null_mut(),
        };
    }
    *out_paired = paired;
    match guest_slot().lock() {
        Ok(mut g) => *g = Some(svc),
        Err(_) => return fail(21, "guest lock"),
    }
    0
}

/// First-time pair with host. Persists trust + endpoint.
#[no_mangle]
pub unsafe extern "C" fn lb_guest_pair(host: *const c_char, port: u16, code: *const c_char) -> i32 {
    clear_last_error();
    let host = match cstr(host) {
        Ok(s) => s.to_string(),
        Err(c) => return c,
    };
    let code = match cstr(code) {
        Ok(s) => s.to_string(),
        Err(c) => return c,
    };
    let mut svc = {
        let mut slot = match guest_slot().lock() {
            Ok(g) => g,
            Err(_) => return fail(21, "guest lock"),
        };
        match slot.take() {
            Some(s) => s,
            None => return fail(27, "guest not opened — call lb_guest_open"),
        }
    };
    let result = runtime().block_on(async { svc.pair(&host, port, &code).await });
    match result {
        Ok(_) => {
            if let Ok(mut slot) = guest_slot().lock() {
                *slot = Some(svc);
            }
            0
        }
        Err(e) => {
            let msg = e.to_string();
            let c = if msg.contains("timeout") { 17 } else { 16 };
            if let Ok(mut slot) = guest_slot().lock() {
                *slot = Some(svc);
            }
            fail(c, format!("guest pair: {msg}"))
        }
    }
}

/// After host app restart: update remembered IP/port (same trusted peer).
#[no_mangle]
pub unsafe extern "C" fn lb_guest_update_endpoint(host: *const c_char, port: u16) -> i32 {
    clear_last_error();
    let host = match cstr(host) {
        Ok(s) => s.to_string(),
        Err(c) => return c,
    };
    let mut slot = match guest_slot().lock() {
        Ok(g) => g,
        Err(_) => return fail(21, "guest lock"),
    };
    let svc = match slot.as_mut() {
        Some(s) => s,
        None => return fail(27, "guest not opened"),
    };
    match svc.update_endpoint(&host, port) {
        Ok(()) => 0,
        Err(e) => fail(25, format!("update_endpoint: {e}")),
    }
}

/// Trusted send file (no pairing code). Requires prior pair.
#[no_mangle]
pub unsafe extern "C" fn lb_guest_send_file(
    file_path: *const c_char,
    remote_name: *const c_char,
    out_bytes: *mut u64,
) -> i32 {
    if out_bytes.is_null() {
        return 1;
    }
    clear_last_error();
    let file_path = match cstr(file_path) {
        Ok(s) => PathBuf::from(s),
        Err(c) => return c,
    };
    let remote = match cstr(remote_name) {
        Ok(s) => s.to_string(),
        Err(_) => String::new(),
    };
    if !file_path.is_file() {
        return fail(24, "file not found");
    }
    let svc = {
        let mut slot = match guest_slot().lock() {
            Ok(g) => g,
            Err(_) => return fail(21, "guest lock"),
        };
        match slot.take() {
            Some(s) => s,
            None => return fail(27, "guest not opened"),
        }
    };
    if !svc.is_paired() {
        if let Ok(mut slot) = guest_slot().lock() {
            *slot = Some(svc);
        }
        return fail(26, "not paired — call lb_guest_pair");
    }
    let result = runtime().block_on(async { svc.send_file(&file_path, &remote).await });
    match result {
        Ok(n) => {
            if let Ok(mut slot) = guest_slot().lock() {
                *slot = Some(svc);
            }
            *out_bytes = n;
            0
        }
        Err(e) => {
            if let Ok(mut slot) = guest_slot().lock() {
                *slot = Some(svc);
            }
            fail(18, format!("send_file: {e}"))
        }
    }
}

/// Trusted push clipboard text.
#[no_mangle]
pub unsafe extern "C" fn lb_guest_send_clipboard_text(text: *const c_char) -> i32 {
    clear_last_error();
    let text = match cstr(text) {
        Ok(s) if !s.is_empty() => s.to_string(),
        _ => return fail(2, "text required"),
    };
    let svc = {
        let mut slot = match guest_slot().lock() {
            Ok(g) => g,
            Err(_) => return fail(21, "guest lock"),
        };
        match slot.take() {
            Some(s) => s,
            None => return fail(27, "guest not opened"),
        }
    };
    if !svc.is_paired() {
        if let Ok(mut slot) = guest_slot().lock() {
            *slot = Some(svc);
        }
        return fail(26, "not paired");
    }
    let result = runtime().block_on(async { svc.send_clipboard(ClipboardItem::Text(text)).await });
    match result {
        Ok(()) => {
            if let Ok(mut slot) = guest_slot().lock() {
                *slot = Some(svc);
            }
            0
        }
        Err(e) => {
            if let Ok(mut slot) = guest_slot().lock() {
                *slot = Some(svc);
            }
            fail(32, format!("send_clipboard: {e}"))
        }
    }
}

/// Trusted push PNG clipboard.
#[no_mangle]
pub unsafe extern "C" fn lb_guest_send_clipboard_png(data: *const u8, len: usize) -> i32 {
    if data.is_null() || len == 0 {
        return fail(2, "png required");
    }
    if len > 16 * 1024 * 1024 {
        return fail(35, "png too large");
    }
    clear_last_error();
    let png = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
    if png.len() < 8 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
        return fail(36, "not a PNG");
    }
    let svc = {
        let mut slot = match guest_slot().lock() {
            Ok(g) => g,
            Err(_) => return fail(21, "guest lock"),
        };
        match slot.take() {
            Some(s) => s,
            None => return fail(27, "guest not opened"),
        }
    };
    if !svc.is_paired() {
        if let Ok(mut slot) = guest_slot().lock() {
            *slot = Some(svc);
        }
        return fail(26, "not paired");
    }
    let result = runtime().block_on(async {
        svc.send_clipboard(ClipboardItem::ImagePng(png.into()))
            .await
    });
    match result {
        Ok(()) => {
            if let Ok(mut slot) = guest_slot().lock() {
                *slot = Some(svc);
            }
            0
        }
        Err(e) => {
            if let Ok(mut slot) = guest_slot().lock() {
                *slot = Some(svc);
            }
            fail(32, format!("send_clipboard: {e}"))
        }
    }
}

// ── Unified DeviceSession (session-ux-v1) ────────────────────────────────────

/// Open session. Free `*out_status_json` with lb_string_free.
#[no_mangle]
pub unsafe extern "C" fn lb_session_open(
    data_dir: *const c_char,
    device_id: *const c_char,
    alias: *const c_char,
    out_status_json: *mut *mut c_char,
) -> i32 {
    if data_dir.is_null() || out_status_json.is_null() {
        return 1;
    }
    clear_last_error();
    // Control-plane logs from here on land in hilog (tag "lanbridge").
    #[cfg(feature = "ohos-log")]
    hilog::init_once();
    let dir = match cstr(data_dir) {
        Ok(s) if !s.is_empty() => PathBuf::from(s),
        _ => return fail(2, "data_dir required"),
    };
    let device_id = match cstr(device_id) {
        Ok(s) if !s.is_empty() => s.to_string(),
        _ => "lanbridge-device".to_string(),
    };
    let alias = match cstr(alias) {
        Ok(s) if !s.is_empty() => s.to_string(),
        _ => "Device".to_string(),
    };
    match DeviceSession::open(dir, &device_id, &alias) {
        Ok(svc) => {
            *out_status_json = to_cstring(status_json(&svc));
            put_session(svc);
            0
        }
        Err(e) => fail(41, format!("session_open: {e}")),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lb_session_status(out_status_json: *mut *mut c_char) -> i32 {
    if out_status_json.is_null() {
        return 1;
    }
    clear_last_error();
    match session_cell().try_lock() {
        Ok(mut g) => {
            let Some(svc) = g.as_mut() else {
                return fail(40, "not_open");
            };
            let _ = svc.poll_pair_wait();
            svc.poll_heartbeat();
            svc.apply_discovery_endpoints();
            let json = status_json(svc);
            if let Ok(mut last) = live_share().last_json.lock() {
                *last = json.clone();
            }
            *out_status_json = to_cstring(json);
            0
        }
        Err(_) => {
            let status: lb_core::SessionStatus = {
                let raw = live_share()
                    .last_json
                    .lock()
                    .ok()
                    .map(|g| g.clone())
                    .unwrap_or_else(|| "{}".into());
                serde_json::from_str(&raw).unwrap_or_else(|_| {
                    // Minimal fallback if cache empty/corrupt.
                    serde_json::from_str(
                        r#"{"state":"online","paired":true,"listen_port":0,"listening":true,"pairing_code":null,"peer_device_id":null,"peer_alias":null,"peer_host":null,"peer_port":null,"device_id":"","alias":"","local_ipv4":null,"connection":"connected","disconnect_reason":null,"nearby":[],"banner":"","transfer_done":0,"transfer_total":0,"transfer_bps":0,"transfer_name":""}"#,
                    )
                    .unwrap()
                })
            };
            let json = status_json_live(status);
            *out_status_json = to_cstring(json);
            0
        }
    }
}

/// `port == 0` → default 37532 / saved. Writes actual port to `out_port` if non-null.
#[no_mangle]
pub unsafe extern "C" fn lb_session_go_online(port: u16, out_port: *mut u16) -> i32 {
    clear_last_error();
    let result = runtime().block_on(async {
        let mut g = session_cell().lock().await;
        let Some(svc) = g.as_mut() else {
            return Err((40i32, "not_open".to_string()));
        };
        match svc.go_online(port).await {
            Ok(p) => Ok(p),
            Err(e) => {
                let msg = e.to_string();
                let c = if msg.contains("port_in_use") { 42 } else { 43 };
                Err((c, msg))
            }
        }
    });
    match result {
        Ok(p) => {
            if !out_port.is_null() {
                *out_port = p;
            }
            0
        }
        Err((c, msg)) => {
            if c == 42 {
                fail(42, "port_in_use")
            } else {
                fail(c, format!("go_online: {msg}"))
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn lb_session_go_offline() -> i32 {
    clear_last_error();
    let mut g = runtime().block_on(session_cell().lock());
    if let Some(svc) = g.as_mut() {
        svc.go_offline();
    }
    0
}

/// Unpair: tell the peer (Bye) then drop local trust. Returns fresh status JSON when non-null.
#[no_mangle]
pub unsafe extern "C" fn lb_session_unpair(out_status_json: *mut *mut c_char) -> i32 {
    clear_last_error();
    let result = runtime().block_on(async {
        let mut g = session_cell().lock().await;
        let Some(svc) = g.as_mut() else {
            return Err((40i32, "not_open".to_string()));
        };
        svc.unpair().await.map_err(|e| (49i32, e.to_string()))
    });
    match result {
        Ok(()) => {
            if !out_status_json.is_null() {
                let json = runtime().block_on(async {
                    let g = session_cell().lock().await;
                    g.as_ref().map(status_json)
                });
                *out_status_json = to_cstring(json.unwrap_or_else(|| "{}".into()));
            }
            0
        }
        Err((c, msg)) => fail(c, format!("unpair: {msg}")),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lb_session_begin_pair(out_peer_id: *mut *mut c_char) -> i32 {
    clear_last_error();
    // Must run arm_pair_wait inside block_on so tokio::spawn has a runtime
    // (NAPI async work threads are not on the Tokio runtime).
    let result = runtime().block_on(async {
        let mut g = session_cell().lock().await;
        let Some(svc) = g.as_mut() else {
            return Err((40i32, "not_open".to_string()));
        };
        match svc.arm_pair_wait() {
            Ok(code) => Ok(code),
            Err(e) => {
                let msg = e.to_string();
                let code = if msg.contains("not_online") {
                    44
                } else if msg.contains("no_tokio_runtime") {
                    48
                } else {
                    13
                };
                Err((code, format!("begin_pair: {msg}")))
            }
        }
    });
    match result {
        Ok(_code) => {
            if !out_peer_id.is_null() {
                *out_peer_id = to_cstring(String::new());
            }
            0
        }
        Err((c, msg)) => fail(c, msg),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lb_session_pair(
    host: *const c_char,
    port: u16,
    code: *const c_char,
) -> i32 {
    clear_last_error();
    let host = match cstr(host) {
        Ok(s) => s.to_string(),
        Err(c) => return c,
    };
    let code = match cstr(code) {
        Ok(s) => s.to_string(),
        Err(c) => return c,
    };
    let result = runtime().block_on(async {
        let mut g = session_cell().lock().await;
        let Some(svc) = g.as_mut() else {
            return Err((40i32, "not_open".to_string()));
        };
        match svc.pair(&host, port, &code).await {
            Ok(_) => Ok(()),
            Err(e) => Err((16, e.to_string())),
        }
    });
    match result {
        Ok(()) => 0,
        Err((c0, msg)) => {
            let c = if msg.contains("pair_timeout") || msg.contains("timeout") {
                17
            } else if c0 == 40 {
                40
            } else {
                16
            };
            fail(c, format!("session_pair: {msg}"))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lb_session_pair_nearby(
    device_id: *const c_char,
    code: *const c_char,
) -> i32 {
    clear_last_error();
    let device_id = match cstr(device_id) {
        Ok(s) => s.to_string(),
        Err(c) => return c,
    };
    let code = match cstr(code) {
        Ok(s) => s.to_string(),
        Err(c) => return c,
    };
    let result = runtime().block_on(async {
        let mut g = session_cell().lock().await;
        let Some(svc) = g.as_mut() else {
            return Err((40i32, "not_open".to_string()));
        };
        match svc.pair_nearby(&device_id, &code).await {
            Ok(_) => Ok(()),
            Err(e) => Err((16, e.to_string())),
        }
    });
    match result {
        Ok(()) => 0,
        Err((c0, msg)) => {
            let c = if msg.contains("peer_not_found") {
                48
            } else if msg.contains("pair_timeout") || msg.contains("timeout") {
                17
            } else if c0 == 40 {
                40
            } else {
                16
            };
            fail(c, format!("session_pair_nearby: {msg}"))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lb_session_set_peer(host: *const c_char, port: u16) -> i32 {
    clear_last_error();
    let host = match cstr(host) {
        Ok(s) => s.to_string(),
        Err(c) => return c,
    };
    let mut g = runtime().block_on(session_cell().lock());
    let Some(svc) = g.as_mut() else {
        return fail(40, "not_open");
    };
    match svc.set_peer(&host, port) {
        Ok(()) => 0,
        Err(e) => fail(25, format!("set_peer: {e}")),
    }
}

#[no_mangle]
pub extern "C" fn lb_session_probe_peer() -> i32 {
    clear_last_error();
    let prepared = runtime().block_on(async {
        let mut g = session_cell().lock().await;
        let Some(svc) = g.as_mut() else {
            return Err((40i32, "not_open".to_string()));
        };
        svc.prepare_probe().map_err(|e| (46i32, e.to_string()))
    });
    let work = match prepared {
        Ok(w) => w,
        Err((c0, msg)) => {
            let c = if msg.contains("busy") {
                45
            } else if msg.contains("peer_unreachable") {
                46
            } else if msg.contains("not_paired") {
                26
            } else if msg.contains("not_online") {
                41
            } else if c0 == 40 {
                40
            } else {
                46
            };
            return fail(c, format!("probe_peer: {msg}"));
        }
    };
    let result = runtime().block_on(DeviceSession::execute_probe(work));
    runtime().block_on(async {
        let mut g = session_cell().lock().await;
        if let Some(svc) = g.as_mut() {
            svc.finish_probe(result.is_ok());
        }
    });
    match result {
        Ok(()) => 0,
        Err(e) => {
            let msg = e.to_string();
            let c = if msg.contains("busy") {
                45
            } else if msg.contains("peer_unreachable") {
                46
            } else if msg.contains("not_paired") {
                26
            } else if msg.contains("not_online") {
                41
            } else {
                46
            };
            fail(c, format!("probe_peer: {msg}"))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lb_session_send_file(
    file_path: *const c_char,
    remote_name: *const c_char,
    out_bytes: *mut u64,
) -> i32 {
    if out_bytes.is_null() {
        return 1;
    }
    clear_last_error();
    let file_path = match cstr(file_path) {
        Ok(s) => PathBuf::from(s),
        Err(c) => return c,
    };
    let remote = match cstr(remote_name) {
        Ok(s) => s.to_string(),
        Err(_) => String::new(),
    };
    if !file_path.is_file() {
        return fail(24, "file not found");
    }
    let prepared = runtime().block_on(async {
        let mut g = session_cell().lock().await;
        let Some(svc) = g.as_mut() else {
            return Err((40i32, "not_open".to_string()));
        };
        svc.prepare_send_file(&file_path, &remote)
            .map_err(|e| (18i32, e.to_string()))
    });
    let work = match prepared {
        Ok(w) => w,
        Err((c0, msg)) => {
            let c = if msg.contains("busy") {
                45
            } else if msg.contains("peer_unreachable") {
                46
            } else if msg.contains("not_paired") {
                26
            } else if c0 == 40 {
                40
            } else {
                18
            };
            return fail(c, format!("send_file: {msg}"));
        }
    };
    // Mutex released while bytes flow — status polls see live %.
    let result = runtime().block_on(DeviceSession::execute_send_file(work));
    runtime().block_on(async {
        let mut g = session_cell().lock().await;
        if let Some(svc) = g.as_mut() {
            svc.finish_transfer(result.is_ok());
            if let Ok(ref r) = result {
                svc.record_last_send(*r);
            }
            let json = status_json(svc);
            if let Ok(mut last) = live_share().last_json.lock() {
                *last = json;
            }
        }
    });
    match result {
        Ok(r) => {
            *out_bytes = r.bytes;
            0
        }
        Err(e) => {
            let msg = e.to_string();
            let c = if msg.contains("busy") {
                45
            } else if msg.contains("peer_unreachable") {
                46
            } else if msg.contains("not_paired") {
                26
            } else {
                18
            };
            fail(c, format!("send_file: {msg}"))
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lb_session_send_clipboard_text(text: *const c_char) -> i32 {
    clear_last_error();
    let text = match cstr(text) {
        Ok(s) if !s.is_empty() => s.to_string(),
        _ => return fail(2, "text required"),
    };
    let result = runtime().block_on(async {
        let mut g = session_cell().lock().await;
        let Some(svc) = g.as_mut() else {
            return Err("not_open".to_string());
        };
        svc.send_clipboard(ClipboardItem::Text(text))
            .await
            .map_err(|e| e.to_string())
    });
    match result {
        Ok(()) => 0,
        Err(msg) => {
            if msg == "not_open" {
                fail(40, "not_open")
            } else {
                fail(32, format!("send_clipboard: {msg}"))
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lb_session_send_clipboard_png(data: *const u8, len: usize) -> i32 {
    if data.is_null() || len == 0 {
        return fail(2, "png required");
    }
    if len > 16 * 1024 * 1024 {
        return fail(35, "png too large");
    }
    clear_last_error();
    let png = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
    if png.len() < 8 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
        return fail(36, "not a PNG");
    }
    let result = runtime().block_on(async {
        let mut g = session_cell().lock().await;
        let Some(svc) = g.as_mut() else {
            return Err("not_open".to_string());
        };
        svc.send_clipboard(ClipboardItem::ImagePng(png.into()))
            .await
            .map_err(|e| e.to_string())
    });
    match result {
        Ok(()) => 0,
        Err(msg) => {
            if msg == "not_open" {
                fail(40, "not_open")
            } else {
                fail(32, format!("send_clipboard: {msg}"))
            }
        }
    }
}

/// Accept one inbound. Free `*out_event_json` with lb_string_free.
/// Accept runs **without** holding the session mutex so outbound send/status can proceed.
#[no_mangle]
pub unsafe extern "C" fn lb_session_serve_once(
    dest_root: *const c_char,
    out_event_json: *mut *mut c_char,
) -> i32 {
    if dest_root.is_null() || out_event_json.is_null() {
        return 1;
    }
    clear_last_error();
    let dest = match cstr(dest_root) {
        Ok(s) => PathBuf::from(s),
        Err(c) => return c,
    };
    let parts = {
        let result = runtime().block_on(async {
            let mut g = session_cell().lock().await;
            let Some(svc) = g.as_mut() else {
                return Err((40i32, "not_open".to_string()));
            };
            svc.take_serve_parts().map_err(|e| (47i32, e.to_string()))
        });
        match result {
            Ok(p) => p,
            Err((c0, msg)) => {
                return map_serve_err(c0, msg);
            }
        }
    };
    let accepted = runtime().block_on(DeviceSession::accept_once(&parts));
    let mut session = match accepted {
        Ok(s) => s,
        Err(e) => return map_serve_err(47, e.to_string()),
    };
    // One read loop for all entry points: Bye / EOF are events, not「入站异常」.
    let read = {
        let result = runtime().block_on(async {
            let mut g = session_cell().lock().await;
            let Some(svc) = g.as_mut() else {
                return Err((40i32, "not_open".to_string()));
            };
            svc.read_inbound(&mut session)
                .await
                .map_err(|e| (47i32, e.to_string()))
        });
        match result {
            Ok(r) => r,
            Err((c0, msg)) => return map_serve_err(c0, msg),
        }
    };
    let frame = match read {
        InboundRead::Frame(f) => f,
        InboundRead::Done(ev) => {
            let json = serde_json::to_string(&ev)
                .unwrap_or_else(|_| r#"{"kind":"error","message":"serialize"}"#.into());
            *out_event_json = to_cstring(json);
            return 0;
        }
    };
    let result = if matches!(
        MsgType::from_u16(frame.msg_type),
        Some(MsgType::ProbeRequest)
    ) {
        let work = {
            let prepared = runtime().block_on(async {
                let mut g = session_cell().lock().await;
                let Some(svc) = g.as_mut() else {
                    return Err((40i32, "not_open".to_string()));
                };
                svc.prepare_inbound_file()
                    .map_err(|e| (47i32, e.to_string()))
            });
            match prepared {
                Ok(w) => w,
                Err((c0, msg)) => return map_serve_err(c0, msg),
            }
        };
        let result = runtime().block_on(DeviceSession::execute_inbound_file(
            work, session, frame, dest,
        ));
        runtime().block_on(async {
            let mut g = session_cell().lock().await;
            if let Some(svc) = g.as_mut() {
                svc.finish_transfer(matches!(&result, Ok(InboundEvent::File { .. })));
                let json = status_json(svc);
                if let Ok(mut last) = live_share().last_json.lock() {
                    *last = json;
                }
            }
        });
        result.map_err(|e| (47, e.to_string()))
    } else {
        runtime().block_on(async {
            let mut g = session_cell().lock().await;
            let Some(svc) = g.as_mut() else {
                return Err((40i32, "not_open".to_string()));
            };
            svc.dispatch_inbound(session, frame, dest)
                .await
                .map_err(|e| (47, e.to_string()))
        })
    };
    match result {
        Ok(ev) => {
            let json = serde_json::to_string(&ev)
                .unwrap_or_else(|_| r#"{"kind":"error","message":"serialize"}"#.into());
            *out_event_json = to_cstring(json);
            0
        }
        Err((c0, msg)) => map_serve_err(c0, msg),
    }
}

fn map_serve_err(c0: i32, msg: String) -> i32 {
    let c = if msg.contains("busy") {
        45
    } else if msg.contains("not_online") {
        44
    } else if msg.contains("not_paired") {
        26
    } else if msg.contains("timeout") {
        16
    } else if c0 == 40 {
        40
    } else {
        47
    };
    fail(c, format!("serve_once: {msg}"))
}

#[no_mangle]
pub unsafe extern "C" fn lb_session_pairing_code(out_code: *mut *mut c_char) -> i32 {
    if out_code.is_null() {
        return 1;
    }
    clear_last_error();
    let mut g = runtime().block_on(session_cell().lock());
    let Some(svc) = g.as_mut() else {
        return fail(40, "not_open");
    };
    *out_code = to_cstring(svc.mint_pairing_code());
    0
}

// ── Loopback clipboard (unchanged semantics) ────────────────────────────────

/// Same-process text clipboard loopback. `out_text` freed with lb_string_free.
#[no_mangle]
pub unsafe extern "C" fn lb_self_loopback_clipboard_text(
    text: *const c_char,
    out_text: *mut *mut c_char,
) -> i32 {
    if out_text.is_null() {
        return 1;
    }
    let text = match cstr(text) {
        Ok(s) if !s.is_empty() => s.to_string(),
        _ => return 2,
    };
    let result = runtime().block_on(async move {
        let host_id = Arc::new(
            DeviceIdentity::generate(format!("ffi-ch-{}", short_id()), "H").map_err(|_| 11)?,
        );
        let guest_id = Arc::new(
            DeviceIdentity::generate(format!("ffi-cg-{}", short_id()), "G").map_err(|_| 11)?,
        );
        let code = generate_pairing_code();
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|_| 12)?;
        let addr = listener.local_addr().map_err(|_| 12)?;
        let code_h = code.clone();
        let expect = text.clone();
        let host = tokio::spawn(async move {
            let (_peer, mut session, _) = pair_as_host(host_id, &listener, &code_h)
                .await
                .map_err(|_| 13i32)?;
            let (_seq, item) = recv_clipboard(&mut session).await.map_err(|_| 30i32)?;
            match item {
                ClipboardItem::Text(t) if t == expect => Ok::<String, i32>(t),
                _ => Err(31),
            }
        });
        let (_peer, mut session) = pair_as_guest(guest_id, addr, &code).await.map_err(|_| 16)?;
        push_clipboard(&mut session, &ClipboardItem::Text(text))
            .await
            .map_err(|_| 32)?;
        let got = host.await.map_err(|_| 19)??;
        Ok::<String, i32>(got)
    });
    match result {
        Ok(t) => {
            *out_text = to_cstring(t);
            0
        }
        Err(c) => c,
    }
}

/// Loopback PNG smoke. Free `*out_bytes` with [`lb_bytes_free`].
#[no_mangle]
pub unsafe extern "C" fn lb_self_loopback_clipboard_png(
    data: *const u8,
    len: usize,
    out_bytes: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    if data.is_null() || len == 0 || out_bytes.is_null() || out_len.is_null() {
        return 1;
    }
    if len > 16 * 1024 * 1024 {
        return 35;
    }
    let png = std::slice::from_raw_parts(data, len).to_vec();
    if png.len() < 8 || &png[..8] != b"\x89PNG\r\n\x1a\n" {
        return 36;
    }
    let expect = png.clone();
    let result = runtime().block_on(async move {
        let host_id = Arc::new(
            DeviceIdentity::generate(format!("ohos-ih-{}", short_id()), "H").map_err(|_| 11)?,
        );
        let guest_id = Arc::new(
            DeviceIdentity::generate(format!("ohos-ig-{}", short_id()), "G").map_err(|_| 11)?,
        );
        let code = generate_pairing_code();
        let listener = TcpListener::bind("127.0.0.1:0").await.map_err(|_| 12)?;
        let addr = listener.local_addr().map_err(|_| 12)?;
        let code_h = code.clone();
        let host = {
            let host_id = host_id.clone();
            tokio::spawn(async move {
                let (_peer, mut session, _) = pair_as_host(host_id, &listener, &code_h)
                    .await
                    .map_err(|_| 13i32)?;
                let (_seq, item) = recv_clipboard(&mut session).await.map_err(|_| 30i32)?;
                match item {
                    ClipboardItem::ImagePng(b) if b.as_ref() == expect.as_slice() => Ok(b.to_vec()),
                    _ => Err(31),
                }
            })
        };
        let (_peer, mut session) = pair_as_guest(guest_id, addr, &code).await.map_err(|_| 16)?;
        push_clipboard(&mut session, &ClipboardItem::ImagePng(png.into()))
            .await
            .map_err(|_| 32)?;
        host.await.map_err(|_| 21)?.map_err(|e| e)
    });
    match result {
        Ok(bytes) => {
            let (ptr, n) = to_bytes(bytes);
            *out_bytes = ptr;
            *out_len = n;
            0
        }
        Err(c) => c,
    }
}

// Compatibility aliases used by older NAPI glue during transition.
#[no_mangle]
pub unsafe extern "C" fn lb_host_begin(
    alias: *const c_char,
    out_port: *mut u16,
    out_code: *mut *mut c_char,
) -> i32 {
    // Ephemeral data dir under temp — prefer lb_host_start with real filesDir.
    let dir = std::env::temp_dir().join(format!("lb-host-{}", short_id()));
    let dir_c = match CString::new(dir.to_string_lossy().as_bytes()) {
        Ok(c) => c,
        Err(_) => return 2,
    };
    let mut paired = 0u8;
    lb_host_start(dir_c.as_ptr(), alias, out_port, out_code, &mut paired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{CStr, CString};

    #[test]
    fn protocol_and_features() {
        assert_eq!(lb_protocol_version(), 0);
        assert_eq!(lb_features_bits(), Features::M3_CLIPBOARD.0);
        unsafe {
            let p = lb_features_label();
            assert!(!p.is_null());
            let s = CStr::from_ptr(p).to_str().unwrap();
            assert!(s.contains("FILE"));
            lb_string_free(p);
        }
    }

    #[test]
    fn pairing_code_len() {
        unsafe {
            let p = lb_make_pairing_code();
            let s = CStr::from_ptr(p).to_str().unwrap();
            assert_eq!(s.len(), 6);
            lb_string_free(p);
        }
    }

    #[test]
    fn self_loopback_smoke() {
        let mut bytes = 0u64;
        let rc = unsafe { lb_self_loopback_transfer(&mut bytes) };
        assert_eq!(rc, 0, "loopback rc");
        assert_eq!(bytes, 8_000);
    }

    #[test]
    fn self_loopback_clipboard_text_smoke() {
        unsafe {
            let input = CString::new("hello-clip-ffi").unwrap();
            let mut out: *mut c_char = ptr::null_mut();
            let rc = lb_self_loopback_clipboard_text(input.as_ptr(), &mut out);
            assert_eq!(rc, 0, "clip loopback rc");
            assert!(!out.is_null());
            let s = CStr::from_ptr(out).to_str().unwrap();
            assert_eq!(s, "hello-clip-ffi");
            lb_string_free(out);
        }
    }

    #[test]
    fn self_loopback_clipboard_png_smoke() {
        let png: Vec<u8> = vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x05, 0xFE,
            0x02, 0xFE, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        unsafe {
            let mut out: *mut u8 = ptr::null_mut();
            let mut len = 0usize;
            let rc = lb_self_loopback_clipboard_png(png.as_ptr(), png.len(), &mut out, &mut len);
            assert_eq!(rc, 0, "png loopback rc");
            assert_eq!(len, png.len());
            assert!(!out.is_null());
            let got = std::slice::from_raw_parts(out, len);
            assert_eq!(got, png.as_slice());
            lb_bytes_free(out, len);
        }
    }

    #[test]
    fn pair_once_then_transfer() {
        let root = std::env::temp_dir().join(format!("lb-ffi-sess-{}", short_id()));
        let host_dir = root.join("host");
        let guest_dir = root.join("guest");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&host_dir).unwrap();
        std::fs::create_dir_all(&guest_dir).unwrap();
        let src = root.join("a.bin");
        std::fs::write(&src, b"session-model").unwrap();
        let dest = root.join("dst");
        std::fs::create_dir_all(&dest).unwrap();

        unsafe {
            let _ = lb_host_stop();
            let host_d = CString::new(host_dir.to_string_lossy().as_bytes()).unwrap();
            let guest_d = CString::new(guest_dir.to_string_lossy().as_bytes()).unwrap();
            let alias = CString::new("H").unwrap();
            let mut port = 0u16;
            let mut code_ptr: *mut c_char = ptr::null_mut();
            let mut paired = 0u8;
            assert_eq!(
                lb_host_start(
                    host_d.as_ptr(),
                    alias.as_ptr(),
                    &mut port,
                    &mut code_ptr,
                    &mut paired
                ),
                0
            );
            assert_eq!(paired, 0);
            assert!(!code_ptr.is_null());
            let code = CStr::from_ptr(code_ptr).to_string_lossy().into_owned();
            lb_string_free(code_ptr);

            let mut g_paired = 0u8;
            let mut g_host: *mut c_char = ptr::null_mut();
            let mut g_port = 0u16;
            let mut g_peer: *mut c_char = ptr::null_mut();
            assert_eq!(
                lb_guest_open(
                    guest_d.as_ptr(),
                    alias.as_ptr(),
                    &mut g_paired,
                    &mut g_host,
                    &mut g_port,
                    &mut g_peer
                ),
                0
            );
            assert_eq!(g_paired, 0);

            let host_ip = CString::new("127.0.0.1").unwrap();
            let code_c = CString::new(code).unwrap();
            let pair_host =
                std::thread::spawn(move || unsafe { lb_host_await_pair(ptr::null_mut()) });
            std::thread::sleep(std::time::Duration::from_millis(50));
            assert_eq!(lb_guest_pair(host_ip.as_ptr(), port, code_c.as_ptr()), 0);
            assert_eq!(pair_host.join().unwrap(), 0);

            // Second transfer: no code.
            let path_c = CString::new(src.to_string_lossy().as_bytes()).unwrap();
            let remote = CString::new("a.bin").unwrap();
            let dest_c = CString::new(dest.to_string_lossy().as_bytes()).unwrap();
            let recv = std::thread::spawn(move || unsafe {
                let mut n = 0u64;
                let mut out_path: *mut c_char = ptr::null_mut();
                let rc = lb_host_recv_file(dest_c.as_ptr(), &mut n, &mut out_path);
                if !out_path.is_null() {
                    lb_string_free(out_path);
                }
                (rc, n)
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
            let mut sent = 0u64;
            assert_eq!(
                lb_guest_send_file(path_c.as_ptr(), remote.as_ptr(), &mut sent),
                0
            );
            let (rc, n) = recv.join().unwrap();
            if rc != 0 {
                let err = unsafe { lb_last_error() };
                let msg = if err.is_null() {
                    String::new()
                } else {
                    let s = unsafe { CStr::from_ptr(err) }
                        .to_string_lossy()
                        .into_owned();
                    unsafe { lb_string_free(err) };
                    s
                };
                panic!("recv_file rc={rc} err={msg}");
            }
            assert_eq!(n, sent);
            assert_eq!(sent, 13);
            assert_eq!(lb_host_stop(), 0);
        }
    }
}
