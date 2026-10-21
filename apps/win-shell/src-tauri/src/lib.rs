//! LanBridge Windows shell — Tauri commands over lb-core.

use lb_core::{
    establish_receiver, establish_sender, generate_pairing_code, pair_as_guest, pair_as_host,
    push_clipboard, recv_clipboard, recv_file, send_file, ByteProgress, ClipboardItem,
    DeviceIdentity, DeviceSession, Features, InboundEvent, InboundRead, MsgType, RecvOptions,
    SendOptions, SessionStatus, PROTOCOL_VERSION, DEFAULT_CONTROL_PORT,
};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, State, WindowEvent,
};
use tokio::net::TcpListener;

struct AppState {
    session: tokio::sync::Mutex<Option<DeviceSession>>,
    /// Lock-free progress mirrors (updated on session_open).
    progress: StdMutex<Arc<ByteProgress>>,
    started_ms: StdMutex<Arc<AtomicU64>>,
    /// 0 idle, 1 inbound, 2 outbound.
    kind_live: StdMutex<Arc<std::sync::atomic::AtomicU8>>,
    name_live: StdMutex<Arc<StdMutex<String>>>,
    last_status: StdMutex<Option<SessionStatus>>,
    /// Hash of the last clipboard image we pushed, so a re-read of the same image is not resent.
    last_image_hash: StdMutex<Option<u64>>,
}

fn bind_live_handles(state: &AppState, svc: &DeviceSession) {
    if let Ok(mut p) = state.progress.lock() {
        *p = svc.progress_handle();
    }
    if let Ok(mut s) = state.started_ms.lock() {
        *s = svc.started_ms_handle();
    }
    if let Ok(mut k) = state.kind_live.lock() {
        *k = svc.kind_live_handle();
    }
    if let Ok(mut n) = state.name_live.lock() {
        *n = svc.name_live_handle();
    }
}

fn overlay_live_progress(state: &AppState, status: &mut SessionStatus) {
    let progress = state
        .progress
        .lock()
        .ok()
        .map(|g| g.clone())
        .unwrap_or_default();
    let started = state
        .started_ms
        .lock()
        .ok()
        .map(|g| g.load(Ordering::Relaxed))
        .unwrap_or(0);
    let kind = state
        .kind_live
        .lock()
        .ok()
        .map(|g| g.load(Ordering::Relaxed))
        .unwrap_or(0);
    let name = state
        .name_live
        .lock()
        .ok()
        .and_then(|g| g.lock().ok().map(|n| n.clone()))
        .unwrap_or_default();
    DeviceSession::apply_live_transfer(status, progress.as_ref(), started, kind, &name);
}

fn session_data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("LanBridge").join("device")
}

/// End other `win-shell.exe` instances so fixed port 37532 is free after
/// tray-hide / `tauri dev` rebuild races. Never kills unrelated processes.
/// Single instance via a named mutex: works no matter what the exe is called (dev build is
/// win-shell.exe, the installed copy is LanBridge.exe). Killing by image name used to leave two
/// processes fighting for the listen port.
#[cfg(windows)]
fn claim_single_instance() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateMutexW(
            attrs: *const core::ffi::c_void,
            owner: i32,
            name: *const u16,
        ) -> *mut core::ffi::c_void;
        fn GetLastError() -> u32;
    }
    const ERROR_ALREADY_EXISTS: u32 = 183;
    static HANDLE: std::sync::atomic::AtomicPtr<core::ffi::c_void> =
        std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

    let name: Vec<u16> = "Local\\LanBridge.SingleInstance\0".encode_utf16().collect();
    // SAFETY: standard Win32 mutex creation; the handle is kept alive for the process lifetime.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if !handle.is_null() {
        HANDLE.store(handle, std::sync::atomic::Ordering::Relaxed);
    }
    !already
}

/// Keep the old name as a no-op so the two call sites stay readable.
#[cfg(windows)]
fn terminate_other_win_shell() {
    if !claim_single_instance() {
        eprintln!("[app] another LanBridge is already running — exiting this one");
        std::process::exit(0);
    }
}

#[cfg(not(windows))]
fn terminate_other_win_shell() {}

/// If `port` is still held by a leftover `win-shell.exe`, kill that PID only.
#[cfg(windows)]
fn reclaim_port_if_stale_win_shell(port: u16) -> bool {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let self_pid = std::process::id();
    let Ok(out) = std::process::Command::new("netstat")
        .args(["-ano", "-p", "tcp"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let needle = format!(":{port}");
    let mut killed = false;
    for line in text.lines() {
        let upper = line.to_ascii_uppercase();
        if !upper.contains("LISTENING") || !line.contains(&needle) {
            continue;
        }
        // Match "*.*.*.*:37532" / "[::]:37532" as local address column.
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 5 {
            continue;
        }
        let local = cols[1];
        if !(local.ends_with(&needle)
            || local.ends_with(&format!("]:{port}"))
            || local == format!("0.0.0.0:{port}")
            || local == format!("[::]:{port}"))
        {
            // Also accept any "*:port" style
            if !local.ends_with(&format!(":{port}")) {
                continue;
            }
        }
        let Ok(pid) = cols[cols.len() - 1].parse::<u32>() else {
            continue;
        };
        if pid == 0 || pid == self_pid {
            continue;
        }
        // Only reclaim our own leftover shells.
        let name_ok = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()
            .and_then(|o| {
                let s = String::from_utf8_lossy(&o.stdout).to_ascii_lowercase();
                Some(s.contains("win-shell.exe"))
            })
            .unwrap_or(false);
        if !name_ok {
            continue;
        }
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        killed = true;
    }
    killed
}

#[cfg(not(windows))]
fn reclaim_port_if_stale_win_shell(_port: u16) -> bool {
    false
}

fn with_session_mut<F, T>(state: &AppState, f: F) -> Result<T, String>
where
    F: FnOnce(&mut DeviceSession) -> Result<T, String>,
{
    let mut slot = state.session.blocking_lock();
    let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
    f(svc)
}

fn cache_status(state: &AppState, status: &SessionStatus) {
    if let Ok(mut g) = state.last_status.lock() {
        *g = Some(status.clone());
    }
}

#[derive(Serialize)]
struct CoreInfo {
    protocol_version: u16,
    features: String,
}

#[derive(Serialize)]
struct IdentityView {
    device_id: String,
    alias: String,
    fingerprint_hex: String,
}

#[derive(Serialize)]
#[allow(dead_code)]
struct HostReady {
    port: u16,
    pairing_code: String,
    device_id: String,
    lan_ip: String,
    listen_hint: String,
    paired: bool,
}

#[derive(Serialize)]
#[allow(dead_code)]
struct GuestStatus {
    paired: bool,
    host: String,
    port: u16,
    peer_id: String,
}

#[derive(Serialize)]
struct ServeEventView {
    kind: String,
    bytes: u64,
    path: Option<String>,
    preview: Option<String>,
    message: Option<String>,
    /// Full inbound text — written to OS clipboard on the UI/main thread (see App.vue).
    /// Async Tokio workers often fail to persist Win32 clipboard ownership.
    text: Option<String>,
    /// Inbound PNG (base64) for main-thread OS write.
    png_base64: Option<String>,
}

/// Native open-file dialog (user-facing pick; never prefill sandbox/.tmp paths in UI).
#[tauri::command]
fn pick_open_file() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("选择要发送的文件")
        .pick_file()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Multi-select files for the send queue.
#[tauri::command]
fn pick_open_files() -> Vec<String> {
    rfd::FileDialog::new()
        .set_title("选择要发送的文件（可多选）")
        .pick_files()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

/// Default receive directory: Downloads/LanBridge (created if missing).
#[tauri::command]
fn default_recv_dir() -> Result<String, String> {
    let home = std::env::var("USERPROFILE").map_err(|_| "no USERPROFILE".to_string())?;
    let dir = PathBuf::from(home).join("Downloads").join("LanBridge");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.to_string_lossy().into_owned())
}

/// Open a file or folder in Windows Explorer.
#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err("路径不存在".into());
    }
    let mut cmd = std::process::Command::new("explorer");
    if p.is_file() {
        // /select,<path> highlights the file in its parent folder.
        cmd.arg(format!("/select,{}", p.display()));
    } else {
        cmd.arg(p.as_os_str());
    }
    cmd.spawn().map_err(|e| e.to_string())?;
    Ok(())
}

/// Native folder dialog for receive destination.
#[tauri::command]
fn pick_save_folder() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("选择接收保存目录")
        .pick_folder()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Native open dialog filtered to PNG.
#[tauri::command]
fn pick_open_png() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("选择 PNG 图片")
        .add_filter("PNG", &["png"])
        .pick_file()
        .map(|p| p.to_string_lossy().into_owned())
}

#[derive(Serialize)]
struct TransferView {
    bytes_transferred: u64,
    path: String,
}

#[derive(Serialize)]
struct ClipboardView {
    kind: String,
    preview: String,
    byte_len: u64,
    /// True when text was written to the OS clipboard.
    os_clipboard_written: bool,
}

/// Best-effort primary LAN IPv4 (UDP connect trick; no packets required).
#[allow(dead_code)]
fn primary_lan_ipv4() -> Option<String> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("1.1.1.1:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => {
            Some(v4.to_string())
        }
        _ => None,
    }
}

fn os_clipboard_get_text() -> Result<String, String> {
    let _g = clipboard_guard();
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.get_text().map_err(|e| e.to_string())
}

fn os_clipboard_set_text(text: &str) -> Result<(), String> {
    let _g = clipboard_guard();
    let mut last = String::new();
    for attempt in 0..6 {
        match arboard::Clipboard::new() {
            Ok(mut cb) => match cb.set_text(text.to_string()) {
                Ok(()) => {
                    // Drop before read — some managers only flush on CloseClipboard.
                    drop(cb);
                    std::thread::sleep(std::time::Duration::from_millis(20 + attempt * 15));
                    match os_clipboard_get_text() {
                        Ok(got) if got == text => return Ok(()),
                        Ok(_) => last = "clipboard verify mismatch".into(),
                        Err(e) => last = e,
                    }
                }
                Err(e) => last = e.to_string(),
            },
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(std::time::Duration::from_millis(40 + attempt * 40));
    }
    Err(if last.is_empty() {
        "clipboard set failed".into()
    } else {
        last
    })
}

fn png_encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "image size overflow".to_string())?;
    if rgba.len() != expected {
        return Err("rgba length mismatch".into());
    }
    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut out);
    use image::ImageEncoder;
    encoder
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

fn png_decode_rgba(png: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let img = image::load_from_memory(png).map_err(|e| e.to_string())?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    Ok((w, h, rgba.into_raw()))
}

fn os_clipboard_set_png(png: &[u8]) -> Result<(u32, u32), String> {
    let (w, h, rgba) = png_decode_rgba(png)?;
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_image(arboard::ImageData {
        width: w as usize,
        height: h as usize,
        bytes: std::borrow::Cow::Owned(rgba),
    })
    .map_err(|e| e.to_string())?;
    Ok((w, h))
}


/// All clipboard access goes through this lock: two threads opening/closing the Win32 clipboard
/// at the same time invalidate each other's handles (the 0xc0000005 we saw at startup).
static CLIPBOARD_LOCK: StdMutex<()> = StdMutex::new(());

fn clipboard_guard() -> std::sync::MutexGuard<'static, ()> {
    CLIPBOARD_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Read the clipboard image as PNG, covering the formats screenshot tools actually use.
///
/// Many tools (PixPin, browsers, Office) put a **file** (CF_HDROP) or a registered "PNG" blob
/// on the clipboard instead of a bitmap, so a CF_DIB-only reader silently sees nothing.
fn os_clipboard_get_png() -> Result<Vec<u8>, String> {
    let _g = clipboard_guard();
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if let Ok(img) = cb.get_image() {
            let w = img.width as u32;
            let h = img.height as u32;
            if let Ok(png) = png_encode_rgba(w, h, img.bytes.as_ref()) {
                return Ok(png);
            }
        }
    }
    #[cfg(windows)]
    {
        if let Ok(png) = windows_clipboard_image_png() {
            return Ok(png);
        }
    }
    eprintln!("[clip] cannot read image, formats: {:?}", clipboard_format_names());
    Err("clipboard has no image".into())
}

#[cfg(windows)]
fn windows_clipboard_image_png() -> Result<Vec<u8>, String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(hwnd: *mut core::ffi::c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn GetClipboardData(format: u32) -> *mut core::ffi::c_void;
        fn RegisterClipboardFormatW(name: *const u16) -> u32;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
    }
    #[link(name = "shell32")]
    extern "system" {
        fn DragQueryFileW(hdrop: *mut core::ffi::c_void, ifile: u32, path: *mut u16, cch: u32) -> u32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GlobalLock(h: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
        fn GlobalUnlock(h: *mut core::ffi::c_void) -> i32;
        fn GlobalSize(h: *mut core::ffi::c_void) -> usize;
    }

    const CF_HDROP: u32 = 15;

    // SAFETY: standard clipboard protocol; the clipboard is opened once and always closed.
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Err("open clipboard failed".into());
        }
        let result = (|| -> Result<Vec<u8>, String> {
            // Registered "PNG" blobs are not trustable (some tools store a path there) and
            // the raw-HGLOBAL read was crashing; CF_DIB below covers every tool we saw.
            // (a2) raw CF_DIB / CF_DIBV5 (PixPin, Office, OLE DataObject clipboards).
            for fmt in [8u32, 17u32] {
                if IsClipboardFormatAvailable(fmt) != 0 {
                    let handle = GetClipboardData(fmt);
                    if !handle.is_null() {
                        let ptr = GlobalLock(handle);
                        if !ptr.is_null() {
                            // A bogus/huge GlobalSize used to turn into an out-of-bounds read.
                            const MAX_DIB: usize = 256 * 1024 * 1024;
                            let len = GlobalSize(handle).min(MAX_DIB);
                            let out = if len >= 40 {
                                dib_to_png(ptr as *const u8, len)
                            } else {
                                None
                            };
                            GlobalUnlock(handle);
                            if let Some(png) = out {
                                return Ok(png);
                            }
                        }
                    }
                }
            }
            // (b) file list (CF_HDROP): use the first PNG file it points at.
            if IsClipboardFormatAvailable(CF_HDROP) != 0 {
                let handle = GetClipboardData(CF_HDROP);
                if !handle.is_null() {
                    let count = DragQueryFileW(handle, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
                    for index in 0..count {
                        let mut buf = vec![0u16; 1024];
                        let n = DragQueryFileW(handle, index, buf.as_mut_ptr(), buf.len() as u32);
                        if n == 0 {
                            continue;
                        }
                        let path =
                            std::path::PathBuf::from(OsString::from_wide(&buf[..n as usize]));
                        if let Ok(bytes) = std::fs::read(&path) {
                            if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                                return Ok(bytes);
                            }
                        }
                    }
                }
            }
            Err("no image formats on clipboard".into())
        })();
        CloseClipboard();
        result
    }
}

fn load_png_file(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    if data.len() < 8 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("not a PNG file".into());
    }
    // Validate decode (also bounds memory via image crate).
    let _ = png_decode_rgba(&data)?;
    Ok(data)
}

fn clip_preview(text: &str) -> String {
    const MAX: usize = 40;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        let s: String = text.chars().take(MAX).collect();
        format!("{s}…")
    }
}

fn ensure_crypto() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn short_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{n:x}")
}

/// Sync command — prefer this over writing clipboard inside async serve workers.
#[tauri::command]
fn copy_text_to_os_clipboard(text: String) -> Result<(), String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("empty".into());
    }
    os_clipboard_set_text(t)
}

/// Sync PNG write for inbound clipboard (main-thread friendly vs async serve).
#[tauri::command]
fn copy_png_to_os_clipboard(png_base64: String) -> Result<(), String> {
    let png = decode_b64(&png_base64)?;
    if png.is_empty() {
        return Err("empty png".into());
    }
    let _ = os_clipboard_set_png(&png)?;
    Ok(())
}

#[tauri::command]
fn read_os_clipboard_text() -> Result<String, String> {
    match os_clipboard_get_text() {
        Ok(s) => Ok(s.trim().to_string()),
        Err(e) => {
            // An image-only clipboard (e.g. a screenshot) is not an error: arboard reports
            // "no text available", and the caller uses that empty string to switch to the
            // image branch. Only surface real failures.
            if os_clipboard_has_image() {
                Ok(String::new())
            } else {
                Err(e)
            }
        }
    }
}

/// Win32 clipboard change counter — skip reads when unchanged (vendor: GetClipboardSequenceNumber).
#[tauri::command]
fn read_os_clipboard_seq() -> u32 {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn GetClipboardSequenceNumber() -> u32;
        }
        // SAFETY: Win32 user32 export; no handle required.
        unsafe { GetClipboardSequenceNumber() }
    }
    #[cfg(not(windows))]
    {
        0
    }
}


/// Decode a CF_DIB / CF_DIBV5 payload (the shape PixPin and Office put on the clipboard)
/// into PNG. arboard returns Err for OLE/DataObject clipboards, so this is the fallback.
#[cfg(windows)]
unsafe fn dib_to_png(ptr: *const u8, len: usize) -> Option<Vec<u8>> {
    if ptr.is_null() || len < 40 {
        return None;
    }
    let bytes = std::slice::from_raw_parts(ptr, len);
    let read_u32 = |off: usize| -> Option<u32> {
        bytes.get(off..off + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let read_i32 = |off: usize| -> Option<i32> {
        bytes.get(off..off + 4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let read_u16 = |off: usize| -> Option<u16> {
        bytes.get(off..off + 2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    };

    let header_size = read_u32(0)? as usize;
    let width = read_i32(4)?;
    let raw_height = read_i32(8)?;
    let bpp = read_u16(14)? as usize;
    let compression = read_u32(16)?;
    let clr_used = read_u32(32)? as usize;

    // BITMAPINFOHEADER family only; dimensions are capped so a corrupt header cannot make us
    // allocate gigabytes (or index past the buffer).
    const MAX_DIM: i32 = 20_000;
    let header_ok = matches!(header_size, 40 | 52 | 56 | 108 | 124);
    // 0 = BI_RGB, 3 = BI_BITFIELDS (PixPin and many 32bpp producers use this).
    let compression_ok = matches!(compression, 0 | 3);
    if !header_ok
        || !compression_ok
        || width <= 0
        || width > MAX_DIM
        || raw_height == 0
        || raw_height.abs() > MAX_DIM
        || (bpp != 24 && bpp != 32)
    {
        return None;
    }
    let top_down = raw_height < 0;
    let height = raw_height.unsigned_abs() as usize;
    let width_usize = width as usize;

    let palette = clr_used.saturating_mul(4);
    let pixel_off = header_size.saturating_add(palette);
    let stride = (width_usize.saturating_mul(bpp) / 8 + 3) & !3usize;
    let total = stride.checked_mul(height)?;
    if pixel_off.checked_add(total)? > bytes.len() {
        return None;
    }

    let mut rgba = vec![0u8; width_usize * height * 4];
    for y in 0..height {
        let src_row = if top_down { y } else { height - 1 - y };
        let src = &bytes[pixel_off + src_row * stride..];
        let dst = &mut rgba[y * width_usize * 4..(y + 1) * width_usize * 4];
        for x in 0..width_usize {
            let s = x * (bpp / 8);
            let d = x * 4;
            dst[d] = src[s + 2];
            dst[d + 1] = src[s + 1];
            dst[d + 2] = src[s];
            dst[d + 3] = if bpp == 32 { src[s + 3] } else { 255 };
        }
    }
    png_encode_rgba(width as u32, height as u32, &rgba).ok()
}

/// Names of the formats currently on the clipboard (diagnostics only).
#[cfg(windows)]
fn clipboard_format_names() -> Vec<String> {
    #[link(name = "user32")]
    extern "system" {
        fn OpenClipboard(hwnd: *mut core::ffi::c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn EnumClipboardFormats(format: u32) -> u32;
        fn GetClipboardFormatNameW(format: u32, name: *mut u16, max: i32) -> i32;
    }

    let mut out = Vec::new();
    // SAFETY: balanced Open/Close; buffer sized for the documented 255-char limit.
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return out;
        }
        let mut fmt = 0u32;
        loop {
            fmt = EnumClipboardFormats(fmt);
            if fmt == 0 {
                break;
            }
            let mut buf = [0u16; 256];
            let n = GetClipboardFormatNameW(fmt, buf.as_mut_ptr(), buf.len() as i32);
            if n > 0 {
                out.push(String::from_utf16_lossy(&buf[..n as usize]));
            } else {
                out.push(format!("fmt#{fmt}"));
            }
        }
        CloseClipboard();
    }
    out
}

#[cfg(not(windows))]
fn clipboard_format_names() -> Vec<String> {
    Vec::new()
}

/// True when the OS clipboard holds an image (screenshot / "copy image").
/// Cheap: only format presence, no pixel copy. Windows always synthesises CF_DIB.
#[tauri::command]
fn os_clipboard_has_image() -> bool {
    let _g = clipboard_guard();
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn IsClipboardFormatAvailable(format: u32) -> i32;
        }
        const CF_DIB: u32 = 8;
        const CF_DIBV5: u32 = 17;
        // SAFETY: read-only user32 query; no clipboard handle is opened.
        let bitmap = unsafe {
            IsClipboardFormatAvailable(CF_DIB) != 0 || IsClipboardFormatAvailable(CF_DIBV5) != 0
        };
        // Screenshot tools may copy a file or a PNG blob instead of a bitmap.
        let found = bitmap || windows_clipboard_image_png().is_ok();
        if !found {
            eprintln!("[clip] no image, formats: {:?}", clipboard_format_names());
        }
        found
    }
    #[cfg(not(windows))]
    {
        false
    }
}


// ---------------------------------------------------------------------------
// Clipboard history (local only): survives restarts, capped, wipeable.
// ---------------------------------------------------------------------------

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ClipHistoryEntry {
    id: String,
    /// "text" | "image"
    kind: String,
    preview: String,
    bytes: u64,
    /// epoch millis
    ts: u64,
    /// image file name under <dir>/img, None for text
    #[serde(default)]
    file: Option<String>,
    /// full text (already capped at MAX_TEXT_STORE), None for image
    #[serde(default)]
    text: Option<String>,
}

const CLIP_HISTORY_MAX: usize = 100;
const CLIP_TEXT_STORE_MAX: usize = 64 * 1024;
const CLIP_IMAGE_STORE_MAX: u64 = 64 * 1024 * 1024;

fn clip_history_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir());
    base.join("LanBridge").join("clipboard")
}

static CLIP_HISTORY_LOCK: StdMutex<()> = StdMutex::new(());

fn history_guard() -> std::sync::MutexGuard<'static, ()> {
    CLIP_HISTORY_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn history_load(dir: &std::path::Path) -> Vec<ClipHistoryEntry> {
    let path = dir.join("history.json");
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<Vec<ClipHistoryEntry>>(&bytes).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn history_save(dir: &std::path::Path, entries: &[ClipHistoryEntry]) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(entries) {
        let _ = std::fs::write(dir.join("history.json"), bytes);
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn history_push(mut entry: ClipHistoryEntry) {
    let _g = history_guard();
    let dir = clip_history_dir();
    let mut entries = history_load(&dir);
    entry.id = format!("{}-{}", now_millis(), entries.len());
    entries.insert(0, entry);
    // Keep the newest N and drop the image files of the evicted ones.
    let keep: Vec<ClipHistoryEntry> = entries.iter().take(CLIP_HISTORY_MAX).cloned().collect();
    let keep_files: std::collections::HashSet<String> =
        keep.iter().filter_map(|e| e.file.clone()).collect();
    for old in entries.iter().skip(CLIP_HISTORY_MAX) {
        if let Some(name) = &old.file {
            if !keep_files.contains(name) {
                let _ = std::fs::remove_file(dir.join("img").join(name));
            }
        }
    }
    history_save(&dir, &keep);
}

fn history_add_text(text: &str) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    let stored: String = trimmed.chars().take(CLIP_TEXT_STORE_MAX).collect();
    let preview: String = trimmed.chars().take(60).collect();
    history_push(ClipHistoryEntry {
        id: String::new(),
        kind: "text".into(),
        preview,
        bytes: trimmed.len() as u64,
        ts: now_millis(),
        file: None,
        text: Some(stored),
    });
}

fn history_add_image(png: &[u8]) {
    if png.is_empty() || png.len() as u64 > CLIP_IMAGE_STORE_MAX {
        return;
    }
    // Content-addressed file name: the same image copied twice reuses one file.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in png {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    let name = format!("{hash:016x}.png");
    let dir = clip_history_dir();
    let _ = std::fs::create_dir_all(dir.join("img"));
    let _ = std::fs::write(dir.join("img").join(&name), png);
    history_push(ClipHistoryEntry {
        id: String::new(),
        kind: "image".into(),
        preview: format!("图片 {} KB", (png.len() as u64 + 1023) / 1024),
        bytes: png.len() as u64,
        ts: now_millis(),
        file: Some(name),
        text: None,
    });
}

#[tauri::command]
fn clip_history_list(limit: Option<u32>) -> Vec<ClipHistoryEntry> {
    let _g = history_guard();
    let entries = history_load(&clip_history_dir());
    let n = limit.unwrap_or(30) as usize;
    entries.into_iter().take(n).collect()
}

#[tauri::command]
fn clip_history_clear() -> Result<(), String> {
    let _g = history_guard();
    let dir = clip_history_dir();
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Put a history entry back on the system clipboard.
#[tauri::command]
fn clip_history_apply(id: String) -> Result<(), String> {
    let entry = {
        let _g = history_guard();
        let dir = clip_history_dir();
        history_load(&dir)
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| "entry not found".to_string())?
    };
    match entry.kind.as_str() {
        "text" => {
            let text = entry.text.unwrap_or_default();
            os_clipboard_set_text(&text)
        }
        _ => {
            let name = entry.file.ok_or_else(|| "missing file".to_string())?;
            let path = clip_history_dir().join("img").join(name);
            let png = std::fs::read(path).map_err(|e| e.to_string())?;
            os_clipboard_set_png(&png).map(|_| ())
        }
    }
}

#[tauri::command]
fn core_info() -> CoreInfo {
    let f = Features::M3_CLIPBOARD.0;
    CoreInfo {
        protocol_version: PROTOCOL_VERSION,
        features: format!("0x{f:08x} (FILE|CLIPBOARD|RESUME)"),
    }
}

#[tauri::command]
fn make_identity(alias: String) -> Result<IdentityView, String> {
    let id = DeviceIdentity::generate(format!("win-{}", short_id()), alias)
        .map_err(|e| e.to_string())?;
    Ok(IdentityView {
        device_id: id.device_id.clone(),
        alias: id.alias.clone(),
        fingerprint_hex: id.fingerprint_hex(),
    })
}

#[tauri::command]
fn make_pairing_code() -> String {
    generate_pairing_code().as_str().to_string()
}

#[tauri::command]
fn session_open(state: State<'_, AppState>, alias: String) -> Result<SessionStatus, String> {
    ensure_crypto();
    let dir = session_data_dir();
    let a = if alias.trim().is_empty() {
        "PC"
    } else {
        alias.trim()
    };
    let svc = DeviceSession::open(dir, "lanbridge-win", a).map_err(|e| e.to_string())?;
    bind_live_handles(&state, &svc);
    let st = svc.status();
    cache_status(&state, &st);
    let mut slot = state.session.blocking_lock();
    *slot = Some(svc);
    Ok(st)
}

#[tauri::command]
fn session_status(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    match state.session.try_lock() {
        Ok(mut slot) => {
            let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
            let _ = svc.poll_pair_wait();
            svc.poll_heartbeat();
            svc.apply_discovery_endpoints();
            let mut st = svc.status();
            overlay_live_progress(&state, &mut st);
            cache_status(&state, &st);
            Ok(st)
        }
        Err(_) => {
            let mut st = state
                .last_status
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .ok_or_else(|| "会话忙".to_string())?;
            overlay_live_progress(&state, &mut st);
            Ok(st)
        }
    }
}

#[tauri::command]
async fn session_go_online(
    state: State<'_, AppState>,
    listen_port: Option<u16>,
) -> Result<SessionStatus, String> {
    ensure_crypto();
    let port = listen_port.unwrap_or(0);
    let effective = if port == 0 {
        DEFAULT_CONTROL_PORT
    } else {
        port
    };

    {
        let mut slot = state.session.lock().await;
        let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
        match svc.go_online(port).await {
            Ok(_) => return Ok(svc.status()),
            Err(e) => {
                let msg = e.to_string();
                if !msg.contains("port_in_use") {
                    return Err(msg);
                }
            }
        }
    }

    // Stale tray / hot-reload instance still listening — reclaim only win-shell.
    terminate_other_win_shell();
    let _ = reclaim_port_if_stale_win_shell(effective);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut slot = state.session.lock().await;
    let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
    match svc.go_online(port).await {
        Ok(_) => Ok(svc.status()),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("port_in_use") {
                Err(format!(
                    "端口 {effective} 仍被其他程序占用（非本应用残留）。请在高级改监听端口，或结束占用进程后重试"
                ))
            } else {
                Err(msg)
            }
        }
    }
}

#[tauri::command]
fn session_go_offline(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    with_session_mut(&state, |svc| {
        svc.go_offline();
        Ok(svc.status())
    })
}

/// Unpair: Bye the peer first so the phone drops「已连接」within one heartbeat, then wipe trust.
#[tauri::command]
async fn session_unpair(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    ensure_crypto();
    let mut slot = state.session.lock().await;
    let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
    svc.unpair().await.map_err(|e| e.to_string())?;
    let st = svc.status();
    cache_status(&state, &st);
    Ok(st)
}

#[tauri::command]
async fn session_begin_pair(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    ensure_crypto();
    // Must be async so arm_pair_wait's spawn sees a Tokio runtime (sync
    // blocking_lock path panics / aborts the same way OH did).
    let mut slot = state.session.lock().await;
    let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
    svc.arm_pair_wait().map_err(|e| e.to_string())?;
    let st = svc.status();
    cache_status(&state, &st);
    Ok(st)
}

#[tauri::command]
fn session_pairing_code(state: State<'_, AppState>) -> Result<String, String> {
    // Explicit remint only; arm_pair_wait uses the current code without reminting.
    with_session_mut(&state, |svc| Ok(svc.mint_pairing_code()))
}

#[tauri::command]
async fn session_pair(
    state: State<'_, AppState>,
    host: String,
    port: u16,
    code: String,
) -> Result<SessionStatus, String> {
    ensure_crypto();
    let mut slot = state.session.lock().await;
    let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
    svc.pair(&host, port, &code)
        .await
        .map_err(|e| e.to_string())?;
    Ok(svc.status())
}

#[tauri::command]
async fn session_pair_nearby(
    state: State<'_, AppState>,
    device_id: String,
    code: String,
) -> Result<SessionStatus, String> {
    ensure_crypto();
    let mut slot = state.session.lock().await;
    let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
    svc.pair_nearby(&device_id, &code)
        .await
        .map_err(|e| e.to_string())?;
    Ok(svc.status())
}

#[tauri::command]
fn session_set_peer(
    state: State<'_, AppState>,
    host: String,
    port: u16,
) -> Result<SessionStatus, String> {
    with_session_mut(&state, |svc| {
        svc.set_peer(&host, port).map_err(|e| e.to_string())?;
        Ok(svc.status())
    })
}

#[tauri::command]
async fn session_probe_peer(state: State<'_, AppState>) -> Result<SessionStatus, String> {
    ensure_crypto();
    let mut slot = state.session.lock().await;
    let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
    svc.probe_peer().await.map_err(|e| e.to_string())?;
    let st = svc.status();
    cache_status(&state, &st);
    Ok(st)
}

#[tauri::command]
async fn session_send_file(
    state: State<'_, AppState>,
    file_path: String,
    remote_name: String,
) -> Result<u64, String> {
    ensure_crypto();
    let path = PathBuf::from(file_path.trim());
    if !path.is_file() {
        return Err("文件不存在".into());
    }
    let work = {
        let mut slot = state.session.lock().await;
        let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
        svc.prepare_send_file(&path, &remote_name)
            .map_err(|e| e.to_string())?
    };
    // Release session mutex while bytes flow so status polls see live %.
    let result = DeviceSession::execute_send_file(work).await;
    {
        let mut slot = state.session.lock().await;
        if let Some(svc) = slot.as_mut() {
            svc.finish_transfer(result.is_ok());
            if let Ok(ref r) = result {
                svc.record_last_send(*r);
                let st = svc.status();
                cache_status(&state, &st);
            }
        }
    }
    result.map(|r| r.bytes).map_err(|e| e.to_string())
}

#[tauri::command]
async fn session_send_clipboard_text(
    state: State<'_, AppState>,
    text: String,
) -> Result<ClipboardView, String> {
    ensure_crypto();
    let payload = {
        let t = text.trim();
        if t.is_empty() {
            os_clipboard_get_text()?.trim().to_string()
        } else {
            t.to_string()
        }
    };
    if payload.is_empty() {
        return Err("没有可发送的文本（输入框与系统剪贴板皆空）".into());
    }
    let mut slot = state.session.lock().await;
    let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
    svc.send_clipboard(ClipboardItem::Text(payload.clone()))
        .await
        .map_err(|e| e.to_string())?;
    history_add_text(&payload);
    Ok(ClipboardView {
        kind: "text".into(),
        byte_len: payload.len() as u64,
        preview: clip_preview(&payload),
        os_clipboard_written: false,
    })
}

#[tauri::command]
async fn session_send_clipboard_image(
    state: State<'_, AppState>,
    file_path: String,
) -> Result<ClipboardView, String> {
    ensure_crypto();
    let path = PathBuf::from(file_path.trim());
    let png = if path.as_os_str().is_empty() {
        os_clipboard_get_png()?
    } else {
        load_png_file(&path)?
    };
    let len = png.len() as u64;
    // Same image read twice (delayed rendering bumps the clipboard sequence): skip the resend.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in png.iter().take(256 * 1024) {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    {
        let mut last = state.last_image_hash.lock().map_err(|e| e.to_string())?;
        if *last == Some(hash) {
            eprintln!("[clip-img] duplicate skipped");
            return Ok(ClipboardView {
                kind: "png".into(),
                byte_len: len,
                preview: "[PNG]".into(),
                os_clipboard_written: false,
            });
        }
        *last = Some(hash);
    }
    let mut slot = state.session.lock().await;
    let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
    svc.send_clipboard(ClipboardItem::ImagePng(png.clone().into()))
        .await
        .map_err(|e| e.to_string())?;
    eprintln!("[clip-img] sent {len} bytes");
    history_add_image(&png);
    Ok(ClipboardView {
        kind: "png".into(),
        byte_len: len,
        preview: "[PNG]".into(),
        os_clipboard_written: false,
    })
}

#[tauri::command]
async fn session_serve_once(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    dest_root: String,
) -> Result<ServeEventView, String> {
    ensure_crypto();
    let dest = PathBuf::from(dest_root.trim());
    if dest.as_os_str().is_empty() {
        return Err("请先选择保存目录".into());
    }
    // Accept without holding the session mutex so outbound clip/file can run concurrently.
    let parts = {
        let mut slot = state.session.lock().await;
        let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
        svc.take_serve_parts().map_err(|e| e.to_string())?
    };
    let mut session = DeviceSession::accept_once(&parts)
        .await
        .map_err(|e| e.to_string())?;
    // One read loop for every entry point: Bye / EOF become events, not「入站异常」.
    let read = {
        let mut slot = state.session.lock().await;
        let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
        svc.read_inbound(&mut session)
            .await
            .map_err(|e| e.to_string())?
    };
    let frame = match read {
        InboundRead::Frame(f) => f,
        InboundRead::Done(ev) => return Ok(serve_event_view(ev)),
    };
    let ev = match MsgType::from_u16(frame.msg_type) {
        Some(MsgType::ProbeRequest) => {
            let work = {
                let mut slot = state.session.lock().await;
                let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
                svc.prepare_inbound_file().map_err(|e| e.to_string())?
            };
            let result = DeviceSession::execute_inbound_file(work, session, frame, dest).await;
            {
                let mut slot = state.session.lock().await;
                if let Some(svc) = slot.as_mut() {
                    svc.finish_transfer(matches!(&result, Ok(InboundEvent::File { .. })));
                    let st = svc.status();
                    cache_status(&state, &st);
                }
            }
            result.map_err(|e| e.to_string())?
        }
        _ => {
            let mut slot = state.session.lock().await;
            let svc = slot.as_mut().ok_or_else(|| "会话未打开".to_string())?;
            svc.dispatch_inbound(session, frame, dest)
                .await
                .map_err(|e| e.to_string())?
        }
    };
    Ok(match ev {
        InboundEvent::File { bytes, path } => ServeEventView {
            kind: "file".into(),
            bytes,
            path: Some(path),
            preview: None,
            message: None,
            text: None,
            png_base64: None,
        },
        InboundEvent::ClipboardText { bytes, text } => {
            history_add_text(&text);
            let write_msg = write_clipboard_on_main(&app, text.clone());
            ServeEventView {
                kind: "clipboard_text".into(),
                bytes: bytes as u64,
                path: None,
                preview: Some(clip_preview(&text)),
                message: Some(write_msg),
                text: Some(text),
                png_base64: None,
            }
        }
        InboundEvent::ClipboardPng { bytes, png_base64 } => {
            if let Ok(png) = decode_b64(&png_base64) {
                history_add_image(&png);
            }
            let write_msg = write_png_clipboard_on_main(&app, png_base64.clone());
            ServeEventView {
                kind: "clipboard_png".into(),
                bytes: bytes as u64,
                path: None,
                preview: Some("[PNG]".into()),
                message: Some(write_msg),
                text: None,
                png_base64: Some(png_base64),
            }
        }
        InboundEvent::PeerLeft { reason } => ServeEventView {
            kind: "peer_left".into(),
            bytes: 0,
            path: None,
            preview: None,
            message: Some(reason),
            text: None,
            png_base64: None,
        },
        InboundEvent::PeerClosed => ServeEventView {
            kind: "peer_closed".into(),
            bytes: 0,
            path: None,
            preview: None,
            message: None,
            text: None,
            png_base64: None,
        },
        InboundEvent::Heartbeat { nonce } => ServeEventView {
            kind: "heartbeat".into(),
            bytes: nonce,
            path: None,
            preview: None,
            message: None,
            text: None,
            png_base64: None,
        },
        InboundEvent::Error { message } => ServeEventView {
            kind: "error".into(),
            bytes: 0,
            path: None,
            preview: None,
            message: Some(message),
            text: None,
            png_base64: None,
        },
    })
}

/// Minimal view for events that carry no payload (liveness / trust teardown).
fn serve_event_view(ev: InboundEvent) -> ServeEventView {
    let (kind, bytes, message) = match ev {
        InboundEvent::Heartbeat { nonce } => ("heartbeat", nonce, None),
        InboundEvent::PeerLeft { reason } => ("peer_left", 0, Some(reason)),
        InboundEvent::PeerClosed => ("peer_closed", 0, None),
        InboundEvent::Error { message } => ("error", 0, Some(message)),
        other => ("error", 0, Some(format!("{other:?}"))),
    };
    ServeEventView {
        kind: kind.into(),
        bytes,
        path: None,
        preview: None,
        message,
        text: None,
        png_base64: None,
    }
}

fn write_clipboard_on_main(app: &tauri::AppHandle, text: String) -> String {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    if app
        .run_on_main_thread(move || {
            let r = os_clipboard_set_text(&text);
            let _ = tx.send(r);
        })
        .is_err()
    {
        return "接收成功但写系统剪贴板失败: main-thread dispatch".into();
    }
    match rx.recv() {
        Ok(Ok(())) => "已写入系统剪贴板".into(),
        Ok(Err(e)) => format!("接收成功但写系统剪贴板失败: {e}"),
        Err(_) => "接收成功但写系统剪贴板失败: channel closed".into(),
    }
}

fn write_png_clipboard_on_main(app: &tauri::AppHandle, png_base64: String) -> String {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    if app
        .run_on_main_thread(move || {
            let r = decode_b64(&png_base64).and_then(|png| os_clipboard_set_png(&png).map(|_| ()));
            let _ = tx.send(r);
        })
        .is_err()
    {
        return "接收成功但写系统剪贴板失败: main-thread dispatch".into();
    }
    match rx.recv() {
        Ok(Ok(())) => "已写入系统剪贴板".into(),
        Ok(Err(e)) => format!("接收成功但写系统剪贴板失败: {e}"),
        Err(_) => "接收成功但写系统剪贴板失败: channel closed".into(),
    }
}

fn decode_b64(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut i = 0;
    while i + 3 < bytes.len() {
        let (a, b, c, d) = (bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]);
        i += 4;
        let av = val(a).ok_or_else(|| "bad base64".to_string())?;
        let bv = val(b).ok_or_else(|| "bad base64".to_string())?;
        out.push((av << 2) | (bv >> 4));
        if c != b'=' {
            let cv = val(c).ok_or_else(|| "bad base64".to_string())?;
            out.push((bv << 4) | (cv >> 2));
            if d != b'=' {
                let dv = val(d).ok_or_else(|| "bad base64".to_string())?;
                out.push((cv << 6) | dv);
            }
        }
    }
    Ok(out)
}

#[allow(dead_code)]
fn lan_ip_hint() -> String {
    primary_lan_ipv4().unwrap_or_else(|| "（查看 ipconfig）".into())
}

/// Same-process loopback transfer smoke (no second window needed).
#[tauri::command]
async fn self_loopback_transfer() -> Result<TransferView, String> {
    ensure_crypto();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../.tmp/win-loopback");
    let src_dir = root.join("src");
    let dst_dir = root.join("dst");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&src_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dst_dir).map_err(|e| e.to_string())?;
    let src_file = src_dir.join("smoke.bin");
    let payload: Vec<u8> = (0..8_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src_file, &payload).map_err(|e| e.to_string())?;

    let host_id = Arc::new(
        DeviceIdentity::generate(format!("win-h-{}", short_id()), "H").map_err(|e| e.to_string())?,
    );
    let guest_id = Arc::new(
        DeviceIdentity::generate(format!("win-g-{}", short_id()), "G").map_err(|e| e.to_string())?,
    );
    let code = generate_pairing_code();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let code_h = code.clone();
    let dest_root = dst_dir.clone();
    let host = tokio::spawn(async move {
        let (peer, session, _) = pair_as_host(host_id.clone(), &listener, &code_h)
            .await
            .map_err(|e| e.to_string())?;
        let mut link = establish_receiver(session, &listener, host_id, &peer)
            .await
            .map_err(|e| e.to_string())?;
        recv_file(
            &mut link,
            &RecvOptions {
                dest_root,
                overwrite: false,
                progress: None,
                live_name: None,
            },
        )
        .await
        .map_err(|e| e.to_string())
    });

    let (peer, session) = pair_as_guest(guest_id.clone(), addr, &code)
        .await
        .map_err(|e| e.to_string())?;
    let mut link = establish_sender(session, addr, guest_id, &peer, 256 * 1024)
        .await
        .map_err(|e| e.to_string())?;
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
    .map_err(|e| e.to_string())?;
    let (recv_summary, path) = host
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e)?;
    if sent.bytes_transferred != recv_summary.bytes_transferred {
        return Err("byte count mismatch".into());
    }
    Ok(TransferView {
        bytes_transferred: recv_summary.bytes_transferred,
        path: path.to_string_lossy().into_owned(),
    })
}

/// Same-process PNG clipboard smoke (tiny generated image).
#[tauri::command]
async fn self_loopback_clipboard_image() -> Result<ClipboardView, String> {
    ensure_crypto();
    let rgba = [255u8, 0, 0, 255, 0, 255, 0, 255];
    let png = png_encode_rgba(2, 1, &rgba)?;
    let host_id = Arc::new(
        DeviceIdentity::generate(format!("win-ih-{}", short_id()), "H").map_err(|e| e.to_string())?,
    );
    let guest_id = Arc::new(
        DeviceIdentity::generate(format!("win-ig-{}", short_id()), "G").map_err(|e| e.to_string())?,
    );
    let code = generate_pairing_code();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let code_h = code.clone();
    let expect = png.clone();
    let host = tokio::spawn(async move {
        let (_peer, mut session, _) = pair_as_host(host_id, &listener, &code_h)
            .await
            .map_err(|e| e.to_string())?;
        let (_seq, item) = recv_clipboard(&mut session)
            .await
            .map_err(|e| e.to_string())?;
        match item {
            ClipboardItem::ImagePng(b) if b.as_ref() == expect.as_slice() => Ok(b),
            _ => Err("clipboard image mismatch".into()),
        }
    });
    let (_peer, mut session) = pair_as_guest(guest_id, addr, &code)
        .await
        .map_err(|e| e.to_string())?;
    push_clipboard(&mut session, &ClipboardItem::ImagePng(png.clone().into()))
        .await
        .map_err(|e| e.to_string())?;
    let got = host
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e: String| e)?;
    let (w, h) = os_clipboard_set_png(&got)?;
    Ok(ClipboardView {
        kind: "png".into(),
        byte_len: got.len() as u64,
        preview: format!("[PNG {w}×{h}]"),
        os_clipboard_written: true,
    })
}

/// Same-process clipboard text smoke.
#[tauri::command]
async fn self_loopback_clipboard_text(text: String) -> Result<ClipboardView, String> {
    ensure_crypto();
    let payload = {
        let t = text.trim();
        if t.is_empty() {
            "lanbridge-clip-smoke".to_string()
        } else {
            t.to_string()
        }
    };
    let host_id = Arc::new(
        DeviceIdentity::generate(format!("win-ch-{}", short_id()), "H").map_err(|e| e.to_string())?,
    );
    let guest_id = Arc::new(
        DeviceIdentity::generate(format!("win-cg-{}", short_id()), "G").map_err(|e| e.to_string())?,
    );
    let code = generate_pairing_code();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let code_h = code.clone();
    let expect = payload.clone();
    let host = tokio::spawn(async move {
        let (_peer, mut session, _) = pair_as_host(host_id, &listener, &code_h)
            .await
            .map_err(|e| e.to_string())?;
        let (_seq, item) = recv_clipboard(&mut session)
            .await
            .map_err(|e| e.to_string())?;
        match item {
            ClipboardItem::Text(t) if t == expect => Ok(t),
            _ => Err("clipboard mismatch".into()),
        }
    });
    let (_peer, mut session) = pair_as_guest(guest_id, addr, &code)
        .await
        .map_err(|e| e.to_string())?;
    push_clipboard(&mut session, &ClipboardItem::Text(payload.clone()))
        .await
        .map_err(|e| e.to_string())?;
    let got = host
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e: String| e)?;
    os_clipboard_set_text(&got)?;
    Ok(ClipboardView {
        kind: "text".into(),
        byte_len: got.len() as u64,
        preview: clip_preview(&got),
        os_clipboard_written: true,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
/// One new screenshot found on disk (PixPin / Snipping Tool save one even without copying).
#[derive(Clone, serde::Serialize)]
struct ScreenshotEvent {
    path: String,
    bytes: u64,
}

fn screenshot_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(std::path::PathBuf::from(local).join("PixPin").join("Data"));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        dirs.push(
            std::path::PathBuf::from(profile)
                .join("Pictures")
                .join("Screenshots"),
        );
    }
    if let Some(extra) = std::env::var_os("LB_SCREENSHOT_DIRS") {
        for p in std::env::split_paths(&extra) {
            dirs.push(p);
        }
    }
    dirs.into_iter().filter(|d| d.is_dir()).collect()
}

fn is_image_file(path: &std::path::Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    matches!(ext.as_deref(), Some("png") | Some("jpg") | Some("jpeg"))
}

/// Watch the folders screenshot tools save into.
///
/// Clipboard-based capture is not enough: PixPin's "save and copy" may only write a file,
/// and its editor can hold the clipboard while the user annotates. A new file in these
/// folders is the most reliable trigger there is.
fn spawn_screenshot_watcher(app: tauri::AppHandle) {
    let started = std::time::SystemTime::now();
    tauri::async_runtime::spawn(async move {
        let mut seen: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        loop {
            for dir in screenshot_dirs() {
                let entries = match std::fs::read_dir(&dir) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !is_image_file(&path) || seen.contains(&path) {
                        continue;
                    }
                    let meta = match entry.metadata() {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    // Mark every pre-existing file as seen: the app must not re-send history.
                    if !seen.insert(path.clone()) {
                        continue;
                    }
                    let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                    if modified < started || meta.len() == 0 {
                        continue;
                    }
                    // Give the writer a moment to finish before the shell reads the bytes.
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    let bytes = std::fs::metadata(&path)
                        .map(|m| m.len())
                        .unwrap_or(meta.len());
                    // stderr lands in the start.ps1 terminal, which is the only log surface
                    // the shell has today.
                    eprintln!("[screenshot] {}", path.display());
                    let _ = app.emit(
                        "screenshot-file",
                        ScreenshotEvent {
                            path: path.to_string_lossy().to_string(),
                            bytes,
                        },
                    );
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        }
    });
}

/// `%LOCALAPPDATA%\LanBridge\logs\lanbridge.log.<date>` — control-plane diagnostics only.
///
/// Red line: never log clipboard bodies or file contents. The core only logs
/// sizes, device ids and protocol outcomes; this sink just persists them.
fn log_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("LanBridge").join("logs")
}

/// Keeps the non-blocking writer thread alive for the whole process.
static LOG_GUARD: StdMutex<Option<tracing_appender::non_blocking::WorkerGuard>> = StdMutex::new(None);

fn init_logging() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let (writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(&dir, "lanbridge.log"));
    if let Ok(mut g) = LOG_GUARD.lock() {
        *g = Some(guard);
    }
    // `LANBRIDGE_LOG=debug` for verbose runs; default keeps the control plane readable.
    let filter = tracing_subscriber::EnvFilter::try_from_env("LANBRIDGE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer),
        )
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init();
}

pub fn run() {
    init_logging();
    ensure_crypto();
    // Before bind: only one win-shell may own the fixed control port.
    terminate_other_win_shell();
    tauri::Builder::default()
        .manage(AppState {
            session: tokio::sync::Mutex::new(None),
            progress: StdMutex::new(Arc::new(ByteProgress::default())),
            started_ms: StdMutex::new(Arc::new(AtomicU64::new(0))),
            kind_live: StdMutex::new(Arc::new(std::sync::atomic::AtomicU8::new(0))),
            name_live: StdMutex::new(Arc::new(StdMutex::new(String::new()))),
            last_status: StdMutex::new(None),
            last_image_hash: StdMutex::new(None),
        })
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            spawn_screenshot_watcher(app.handle().clone());
            let show = MenuItem::with_id(app, "show", "打开窗口", true, None::<&str>)?;
            let pause_clip =
                MenuItem::with_id(app, "pause_clip", "暂停/恢复自动剪贴板", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &pause_clip, &quit])?;

            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .tooltip("LanBridge")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "quit" => app.exit(0),
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "pause_clip" => {
                        let _ = app.emit("tray-toggle-clip-auto", ());
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            // Close → tray (do not exit). Vendor: system-tray.mdx; M4 Wave A.
            if let Some(win) = app.get_webview_window("main") {
                let handle = app.handle().clone();
                win.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        if let Some(w) = handle.get_webview_window("main") {
                            let _ = w.hide();
                        }
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            core_info,
            pick_open_file,
            pick_open_files,
            default_recv_dir,
            pick_save_folder,
            open_path,
            pick_open_png,
            make_identity,
            make_pairing_code,
            session_open,
            session_status,
            session_go_online,
            session_go_offline,
            session_unpair,
            session_begin_pair,
            session_pairing_code,
            session_pair,
            session_pair_nearby,
            session_set_peer,
            session_probe_peer,
            session_send_file,
            session_send_clipboard_text,
            session_send_clipboard_image,
            session_serve_once,
            self_loopback_transfer,
            self_loopback_clipboard_text,
            self_loopback_clipboard_image,
            copy_text_to_os_clipboard,
            copy_png_to_os_clipboard,
            clip_history_list,
            clip_history_clear,
            clip_history_apply,
            read_os_clipboard_text,
            read_os_clipboard_seq,
            os_clipboard_has_image
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(all(test, windows))]
mod clipboard_probe_tests {
    use super::*;

    /// The tests all drive the single system clipboard, so they must not run concurrently.
    static TEST_CLIP: StdMutex<()> = StdMutex::new(());
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        TEST_CLIP.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Guards the screenshot path: "copy image" must be detectable without copying pixels.
    #[test]
    fn detects_clipboard_image_but_not_text() {
        let _serial = serial();
        let mut cb = arboard::Clipboard::new().expect("clipboard open");
        cb.set_image(arboard::ImageData {
            width: 2,
            height: 2,
            bytes: std::borrow::Cow::Owned(vec![255u8; 2 * 2 * 4]),
        })
        .expect("set image");
        assert!(os_clipboard_has_image(), "image in clipboard must be detected");

        cb.set_text("lanbridge-probe".to_string()).expect("set text");
        assert!(
            !os_clipboard_has_image(),
            "text-only clipboard must not be reported as image"
        );
    }



    /// Reproduces the startup crash: reading back an image we wrote ourselves.
    #[test]
    fn reads_back_clipboard_image_without_crashing() {
        let _serial = serial();
        let mut cb = arboard::Clipboard::new().expect("clipboard open");
        cb.set_image(arboard::ImageData {
            width: 8,
            height: 8,
            bytes: std::borrow::Cow::Owned(vec![200u8; 8 * 8 * 4]),
        })
        .expect("set image");
        drop(cb);
        std::thread::sleep(std::time::Duration::from_millis(150));
        eprintln!("[probe] image set, reading back");
        let png = os_clipboard_get_png();
        eprintln!("[probe] read result: {:?}", png.as_ref().map(|v| v.len()));
        assert!(png.is_ok(), "clipboard image must be readable");
        eprintln!("[probe] has_image = {}", os_clipboard_has_image());
    }
    /// Builds a tiny 32bpp bottom-up DIB and checks it decodes to a PNG of the same size.
    #[test]
    fn decodes_dib_bitmap() {
        let _serial = serial();
        let width = 2u32;
        let height = 2u32;
        let header: u32 = 40;
        let stride = ((width as usize * 4 + 3) & !3) as usize;
        let mut buf = vec![0u8; header as usize + stride * height as usize];
        buf[0..4].copy_from_slice(&header.to_le_bytes());
        buf[4..8].copy_from_slice(&(width as i32).to_le_bytes());
        buf[8..12].copy_from_slice(&(height as i32).to_le_bytes()); // bottom-up
        buf[12..14].copy_from_slice(&1u16.to_le_bytes());
        buf[14..16].copy_from_slice(&32u16.to_le_bytes());
        for px in buf[header as usize..].iter_mut() {
            *px = 0x40;
        }
        let png = unsafe { dib_to_png(buf.as_ptr(), buf.len()) }.expect("dib decodes");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let decoded = png_decode_rgba(&png).expect("png decodes");
        assert_eq!(decoded.0, width);
        assert_eq!(decoded.1, height);
    }
    /// PixPin-style tools copy a *file* (CF_HDROP) instead of a bitmap; that must still work.
    #[test]
    fn reads_png_from_file_clipboard() {
        let _serial = serial();
        let dir = std::env::temp_dir().join("lb-clip-file-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("shot.png");
        let png = png_encode_rgba(2, 2, &[255u8; 16]).expect("encode png");
        std::fs::write(&path, &png).expect("write png");

        let status = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command"])
            .arg(format!("Set-Clipboard -Path '{}'", path.display()))
            .status()
            .expect("spawn powershell");
        assert!(status.success(), "Set-Clipboard failed");

        let got = os_clipboard_get_png().expect("clipboard file image must be readable");
        assert!(got.starts_with(b"\x89PNG\r\n\x1a\n"), "expected PNG bytes");
        assert!(os_clipboard_has_image(), "file-clipboard must report an image");
    }
}
