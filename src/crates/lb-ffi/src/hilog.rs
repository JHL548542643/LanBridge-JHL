//! HarmonyOS hilog bridge: forward `tracing` events to `OH_LOG_Print`.
//!
//! `ohos-log` feature only (built by scripts/build-ohos-ffi.ps1 for the OHOS target). The point is
//! that the shared core already emits control-plane events; without a subscriber they vanish, and
//! on-device debugging degenerates into watching the UI.
//!
//! Privacy: events carry sizes, device ids and protocol outcomes only — never
//! clipboard bodies or file contents. Do not add bodies to log calls.

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_uint};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata, Subscriber};

/// hilog `LOG_APP`.
const LOG_APP: c_int = 0;
/// hilog tag; matches the default filter in scripts/ohos-log.ps1.
const TAG: &[u8] = b"lanbridge\0";
/// `%{public}s` keeps the message readable (plain `%s` is redacted release builds).
const FMT: &[u8] = b"%{public}s\0";
/// App-level domain (must stay inside 0x0..=0xFFFFF).
const DOMAIN: c_uint = 0x0B1D;

extern "C" {
    fn OH_LOG_Print(
        log_type: c_int,
        level: c_int,
        domain: c_uint,
        tag: *const c_char,
        fmt: *const c_char,
        ...
    ) -> c_int;
}

fn hilog_level(level: &Level) -> c_int {
    match *level {
        Level::ERROR => 6,
        Level::WARN => 5,
        Level::INFO => 4,
        Level::DEBUG => 3,
        Level::TRACE => 2,
    }
}

/// Collects the event message plus any key=value fields into one line.
#[derive(Default)]
struct LineWriter(String);

impl LineWriter {
    fn push_field(&mut self, name: &str) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        self.0.push_str(name);
        self.0.push('=');
    }
}

impl Visit for LineWriter {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0.push_str(value);
        } else {
            self.push_field(field.name());
            self.0.push_str(value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.push_str(&format!("{value:?}"));
        } else {
            self.push_field(field.name());
            self.0.push_str(&format!("{value:?}"));
        }
    }
}

struct HilogSubscriber {
    /// Everything is forwarded; **hilog's own level** is the filter (`hilog -b D` for verbose).
    /// That keeps a runtime switch without an extra FFI knob.
    max_level: Level,
}

impl Subscriber for HilogSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= self.max_level
    }

    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut line = LineWriter::default();
        event.record(&mut line);
        let text = format!("{}: {}", event.metadata().target(), line.0);
        let Ok(msg) = CString::new(text) else {
            return;
        };
        // SAFETY: both format strings are static NUL-terminated byte strings; `msg` is a valid
        // NUL-terminated CString alive for the call. OH_LOG_Print only reads them.
        unsafe {
            OH_LOG_Print(
                LOG_APP,
                hilog_level(event.metadata().level()),
                DOMAIN,
                TAG.as_ptr().cast(),
                FMT.as_ptr().cast(),
                msg.as_ptr(),
            );
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

/// Print one line straight to hilog, bypassing `tracing` (bootstrap / self-test).
pub fn raw_log(level: c_int, msg: &str) {
    let Ok(msg) = CString::new(msg) else {
        return;
    };
    // SAFETY: static NUL-terminated format strings; `msg` outlives the call.
    unsafe {
        OH_LOG_Print(
            LOG_APP,
            level,
            DOMAIN,
            TAG.as_ptr().cast(),
            FMT.as_ptr().cast(),
            msg.as_ptr(),
        );
    }
}

/// Install the hilog subscriber once per process (idempotent).
pub fn init_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let subscriber = HilogSubscriber {
            max_level: Level::TRACE,
        };
        match tracing::subscriber::set_global_default(subscriber) {
            // Doubles as a self-test: one line per process start proves the tag/domain work.
            Ok(()) => tracing::info!(
                "hilog bridge ready (tag=lanbridge domain=0x0B1D; raise verbosity with \"hilog -b D\")"
            ),
            Err(e) => raw_log(6, &format!("hilog bridge: set_global_default failed: {e}")),
        }
    });
}
