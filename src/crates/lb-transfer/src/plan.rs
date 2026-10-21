//! Bandwidth planner and deterministic byte-range split.

/// Streams and chunk size chosen from a measured receive rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferPlan {
    pub streams: u16,
    pub chunk_size: usize,
    pub probe_bytes: u64,
    pub probe_elapsed_us: u64,
    pub probe_bps: u64,
}

pub fn plan_from_probe(bytes: u64, elapsed_us: u64) -> TransferPlan {
    let elapsed_us = elapsed_us.max(1);
    let probe_bps = bytes.saturating_mul(1_000_000) / elapsed_us;
    // 1 Mbit/s = 125_000 bytes/s.
    let mbps = probe_bps / 125_000;
    // One TLS stream saturates a core above ~2 Gbit/s (typical loopback).
    // Extra streams help mid-range LAN links, not an already-saturated core.
    let streams: u16 = if mbps >= 2_000 {
        1
    } else if mbps >= 800 {
        4
    } else if mbps >= 200 {
        2
    } else {
        1
    };
    let chunk_size = if mbps >= 400 { 1024 * 1024 } else { 256 * 1024 };
    TransferPlan {
        streams,
        chunk_size,
        probe_bytes: bytes,
        probe_elapsed_us: elapsed_us,
        probe_bps,
    }
}

/// Split `[start, start+len)` into `n` contiguous slices (some may be empty).
pub fn split_range(start: u64, len: u64, n: u16) -> Vec<(u64, u64)> {
    let n = n.max(1) as u64;
    let base = len / n;
    let rem = len % n;
    let mut out = Vec::with_capacity(n as usize);
    let mut off = start;
    for i in 0..n {
        let extra = if i < rem { 1 } else { 0 };
        let slice_len = base + extra;
        out.push((off, slice_len));
        off += slice_len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_covers_exactly() {
        let parts = split_range(10, 100, 3);
        assert_eq!(parts, vec![(10, 34), (44, 33), (77, 33)]);
        let sum: u64 = parts.iter().map(|p| p.1).sum();
        assert_eq!(sum, 100);
    }

    #[test]
    fn slow_link_is_one_stream() {
        // 10 MiB in 2s ≈ 40 Mbit/s.
        let plan = plan_from_probe(10 * 1024 * 1024, 2_000_000);
        assert_eq!(plan.streams, 1);
        assert_eq!(plan.chunk_size, 256 * 1024);
    }

    #[test]
    fn saturated_core_stays_one_stream() {
        // ~6 Gbit/s → one stream, large chunks.
        let plan = plan_from_probe(780_000_000, 1_000_000);
        assert_eq!(plan.streams, 1);
        assert_eq!(plan.chunk_size, 1024 * 1024);
    }
}
