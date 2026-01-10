//! Packet parsing and crafting helpers used by the emulator network stack.
//!
//! The types in this module are designed to be "zero-copy-ish": parsers borrow the
//! original byte slice and provide field accessors without allocating.

pub mod arp;
pub mod checksum;
pub mod dhcp;
pub mod dns;
pub mod ethernet;
pub mod icmp;
pub mod ipv4;
pub mod tcp;
pub mod udp;

use core::fmt;

/// Errors returned by packet parsers/builders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketError {
    /// The input buffer ended before the header/payload could be read.
    Truncated {
        /// Minimum number of bytes required to proceed.
        needed: usize,
        /// Actual available bytes.
        actual: usize,
    },
    /// A field was structurally invalid (e.g. IPv4 version != 4).
    Malformed(&'static str),
    /// An unsupported format was encountered (e.g. non Ethernet/IPv4 ARP).
    Unsupported(&'static str),
    /// Provided output buffer was too small to serialize into.
    BufferTooSmall { needed: usize, actual: usize },
}

impl fmt::Display for PacketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PacketError::Truncated { needed, actual } => {
                write!(f, "packet truncated (needed {needed}, got {actual})")
            }
            PacketError::Malformed(msg) => write!(f, "malformed packet: {msg}"),
            PacketError::Unsupported(msg) => write!(f, "unsupported packet: {msg}"),
            PacketError::BufferTooSmall { needed, actual } => {
                write!(f, "buffer too small (needed {needed}, got {actual})")
            }
        }
    }
}

impl std::error::Error for PacketError {}

/// An Ethernet MAC address.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    pub const BROADCAST: MacAddr = MacAddr([0xff; 6]);

    pub fn octets(self) -> [u8; 6] {
        self.0
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0;
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            b[0], b[1], b[2], b[3], b[4], b[5]
        )
    }
}

pub(crate) fn ensure_len(data: &[u8], needed: usize) -> Result<(), PacketError> {
    if data.len() < needed {
        return Err(PacketError::Truncated {
            needed,
            actual: data.len(),
        });
    }
    Ok(())
}

pub(crate) fn ensure_out_buf_len(buf: &[u8], needed: usize) -> Result<(), PacketError> {
    if buf.len() < needed {
        return Err(PacketError::BufferTooSmall {
            needed,
            actual: buf.len(),
        });
    }
    Ok(())
}
