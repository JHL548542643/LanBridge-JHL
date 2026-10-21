//! File transfer over paired TLS sessions (probe, multipath, directory).

#![forbid(unsafe_code)]

mod error;
mod path;
mod plan;
mod transfer;

pub use error::{Error, Result};
pub use path::{pick_dest_for_recv, resolve_dest_path, sanitize_relative_path};
pub use plan::{plan_from_probe, split_range, TransferPlan};
pub use transfer::{
    establish_receiver, establish_sender, recv_dir, recv_file, send_dir, send_file, shutdown_link,
    ByteProgress, Link, RecvOptions, SendOptions, TransferSummary, DEFAULT_PROBE_BYTES,
};
