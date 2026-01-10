//! E1000 transmit offload helpers (checksum insertion + TSO).
//!
//! The E1000 Windows (and Linux) drivers may emit *advanced* transmit
//! descriptors when hardware offloads are enabled:
//! - A context descriptor describing checksum offsets, header lengths, MSS, …
//! - One or more data descriptors containing the frame bytes.
//!
//! Real silicon performs checksum insertion and TCP segmentation offload (TSO)
//! in hardware. In the emulator we translate those requests into fully-formed
//! Ethernet frames before handing them to the host/network stack.

use core::fmt;
use core::net::Ipv4Addr;

use nt_packetlib::io::net::packet::checksum::{ipv4_header_checksum, transport_checksum_ipv4};

/// Per-packet checksum offload flags (from the Advanced TX data descriptor's `popts` field).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxChecksumFlags {
    /// Insert/compute the IPv4 header checksum (`IXSM`).
    pub ipv4: bool,
    /// Insert/compute the TCP/UDP checksum (`TXSM`).
    pub l4: bool,
}

impl TxChecksumFlags {
    pub const IXSM: u8 = 0x01;
    pub const TXSM: u8 = 0x02;

    pub fn from_popts(popts: u8) -> Self {
        Self {
            ipv4: (popts & Self::IXSM) != 0,
            l4: (popts & Self::TXSM) != 0,
        }
    }
}

/// Offload context latched from an Advanced TX context descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxOffloadContext {
    /// Start of IPv4 header (bytes from start of frame).
    pub ipcss: usize,
    /// Location of IPv4 checksum field (bytes from start of frame).
    pub ipcso: usize,
    /// End of IPv4 checksum region (inclusive, bytes from start of frame).
    pub ipcse: usize,
    /// Start of TCP/UDP header (bytes from start of frame).
    pub tucss: usize,
    /// Location of TCP/UDP checksum field (bytes from start of frame).
    pub tucso: usize,
    /// End of TCP/UDP checksum region (inclusive, bytes from start of frame).
    pub tucse: usize,
    /// Maximum Segment Size for TSO (payload bytes per segment).
    pub mss: usize,
    /// Total header length for TSO (bytes from start of frame to start of payload).
    pub hdr_len: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffloadError {
    FrameTooShort,
    UnsupportedIpVersion(u8),
    UnsupportedL4Protocol(u8),
    InvalidContext(&'static str),
}

impl fmt::Display for OffloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooShort => write!(f, "frame too short"),
            Self::UnsupportedIpVersion(v) => write!(f, "unsupported IP version {v}"),
            Self::UnsupportedL4Protocol(p) => write!(f, "unsupported L4 protocol {p}"),
            Self::InvalidContext(msg) => write!(f, "invalid offload context: {msg}"),
        }
    }
}

impl std::error::Error for OffloadError {}

pub fn apply_checksum_offload(
    frame: &mut [u8],
    ctx: TxOffloadContext,
    flags: TxChecksumFlags,
) -> Result<(), OffloadError> {
    if !flags.ipv4 && !flags.l4 {
        return Ok(());
    }

    if ctx.ipcss + 20 > frame.len() {
        return Err(OffloadError::FrameTooShort);
    }

    let version = frame[ctx.ipcss] >> 4;
    if version != 4 {
        return Err(OffloadError::UnsupportedIpVersion(version));
    }

    let ihl = (frame[ctx.ipcss] & 0x0F) as usize * 4;
    if ihl < 20 || ctx.ipcss + ihl > frame.len() {
        return Err(OffloadError::FrameTooShort);
    }

    let total_len = u16::from_be_bytes([frame[ctx.ipcss + 2], frame[ctx.ipcss + 3]]) as usize;
    let ip_available = frame.len().saturating_sub(ctx.ipcss);
    let ip_len = if total_len >= ihl && total_len <= ip_available {
        total_len
    } else {
        ip_available
    };

    if flags.ipv4 {
        if ctx.ipcso + 2 > frame.len() {
            return Err(OffloadError::FrameTooShort);
        }
        frame[ctx.ipcso..ctx.ipcso + 2].fill(0);

        // ipcse is inclusive. If the context is invalid, fall back to the IHL.
        let header_end = if ctx.ipcse >= ctx.ipcss && ctx.ipcse + 1 <= frame.len() {
            ctx.ipcse + 1
        } else {
            ctx.ipcss + ihl
        };

        let checksum = ipv4_header_checksum(&frame[ctx.ipcss..header_end]);
        frame[ctx.ipcso..ctx.ipcso + 2].copy_from_slice(&checksum.to_be_bytes());
    }

    if flags.l4 {
        if ctx.tucss >= frame.len() || ctx.tucso + 2 > frame.len() {
            return Err(OffloadError::FrameTooShort);
        }

        let proto = frame[ctx.ipcss + 9];
        if proto != 6 && proto != 17 {
            return Err(OffloadError::UnsupportedL4Protocol(proto));
        }

        let l4_offset = ctx.tucss;
        let l4_len = ip_len
            .checked_sub(l4_offset - ctx.ipcss)
            .ok_or(OffloadError::FrameTooShort)?;
        if l4_offset + l4_len > frame.len() {
            return Err(OffloadError::FrameTooShort);
        }

        frame[ctx.tucso..ctx.tucso + 2].fill(0);

        let src_ip = Ipv4Addr::new(
            frame[ctx.ipcss + 12],
            frame[ctx.ipcss + 13],
            frame[ctx.ipcss + 14],
            frame[ctx.ipcss + 15],
        );
        let dst_ip = Ipv4Addr::new(
            frame[ctx.ipcss + 16],
            frame[ctx.ipcss + 17],
            frame[ctx.ipcss + 18],
            frame[ctx.ipcss + 19],
        );

        let segment = &frame[l4_offset..l4_offset + l4_len];
        let mut checksum = transport_checksum_ipv4(src_ip, dst_ip, proto, segment);
        // UDP checksum of 0 is transmitted as all ones.
        if proto == 17 && checksum == 0 {
            checksum = 0xFFFF;
        }
        frame[ctx.tucso..ctx.tucso + 2].copy_from_slice(&checksum.to_be_bytes());
    }

    Ok(())
}

pub fn tso_segment(
    frame: &[u8],
    ctx: TxOffloadContext,
    flags: TxChecksumFlags,
) -> Result<Vec<Vec<u8>>, OffloadError> {
    if ctx.mss == 0 {
        return Err(OffloadError::InvalidContext("mss is 0"));
    }
    if ctx.ipcss + 20 > frame.len() || ctx.tucss + 20 > frame.len() {
        return Err(OffloadError::FrameTooShort);
    }

    let version = frame[ctx.ipcss] >> 4;
    if version != 4 {
        return Err(OffloadError::UnsupportedIpVersion(version));
    }

    let ihl = (frame[ctx.ipcss] & 0x0F) as usize * 4;
    if ihl < 20 || ctx.ipcss + ihl > frame.len() {
        return Err(OffloadError::FrameTooShort);
    }

    let proto = frame[ctx.ipcss + 9];
    if proto != 6 {
        return Err(OffloadError::UnsupportedL4Protocol(proto));
    }

    let tcp_header_len = ((frame[ctx.tucss + 12] >> 4) as usize) * 4;
    if tcp_header_len < 20 || ctx.tucss + tcp_header_len > frame.len() {
        return Err(OffloadError::FrameTooShort);
    }

    let computed_hdr_len = ctx.tucss + tcp_header_len;
    let hdr_len = if ctx.hdr_len != 0 && ctx.hdr_len <= frame.len() && ctx.hdr_len == computed_hdr_len {
        ctx.hdr_len
    } else {
        computed_hdr_len
    };
    if hdr_len > frame.len() {
        return Err(OffloadError::FrameTooShort);
    }

    if frame.len() <= hdr_len {
        // No payload; treat this as a normal frame and just compute checksums.
        let mut out = frame.to_vec();
        apply_checksum_offload(&mut out, ctx, flags)?;
        return Ok(vec![out]);
    }

    let base_seq = u32::from_be_bytes(frame[ctx.tucss + 4..ctx.tucss + 8].try_into().unwrap());
    let base_ip_id = u16::from_be_bytes(frame[ctx.ipcss + 4..ctx.ipcss + 6].try_into().unwrap());

    let payload = &frame[hdr_len..];
    let segments = (payload.len() + ctx.mss - 1) / ctx.mss;

    let mut out = Vec::with_capacity(segments);
    for idx in 0..segments {
        let start = idx * ctx.mss;
        let end = (start + ctx.mss).min(payload.len());
        let chunk = &payload[start..end];

        let mut seg = Vec::with_capacity(hdr_len + chunk.len());
        seg.extend_from_slice(&frame[..hdr_len]);
        seg.extend_from_slice(chunk);

        // Update IPv4 total length.
        let ip_total_len = (seg.len() - ctx.ipcss) as u16;
        seg[ctx.ipcss + 2..ctx.ipcss + 4].copy_from_slice(&ip_total_len.to_be_bytes());

        // Increment IPv4 ID per segment.
        let ip_id = base_ip_id.wrapping_add(idx as u16);
        seg[ctx.ipcss + 4..ctx.ipcss + 6].copy_from_slice(&ip_id.to_be_bytes());

        // Update TCP sequence number.
        let seq = base_seq.wrapping_add((idx * ctx.mss) as u32);
        seg[ctx.tucss + 4..ctx.tucss + 8].copy_from_slice(&seq.to_be_bytes());

        // Clear FIN/PSH on all but the last segment.
        if idx + 1 != segments {
            let flags_off = ctx.tucss + 13;
            seg[flags_off] &= !(0x01 | 0x08);
        }

        // Compute requested checksums for this segment.
        apply_checksum_offload(&mut seg, ctx, flags)?;

        out.push(seg);
    }

    Ok(out)
}

