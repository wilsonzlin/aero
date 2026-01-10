//! Byte ring buffer used for command/completion transport.

use core::fmt;

use crate::abi::{
    GpuRecordHeader, GpuRingHeader, ABI_MAJOR, ABI_MINOR, GPU_PAD_MAGIC, GPU_RING_MAGIC,
};
use crate::guest_memory::{GuestMemory, GuestMemoryError, GuestMemoryExt};

#[derive(Debug)]
pub enum RingError {
    GuestMemory(GuestMemoryError),
    InvalidRingHeader(&'static str),
    InvalidRecord(&'static str),
    RingFull,
}

impl fmt::Display for RingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GuestMemory(e) => write!(f, "{e}"),
            Self::InvalidRingHeader(msg) => write!(f, "invalid ring header: {msg}"),
            Self::InvalidRecord(msg) => write!(f, "invalid ring record: {msg}"),
            Self::RingFull => write!(f, "ring full"),
        }
    }
}

impl std::error::Error for RingError {}

impl From<GuestMemoryError> for RingError {
    fn from(value: GuestMemoryError) -> Self {
        Self::GuestMemory(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RingLocation {
    pub base_paddr: u64,
}

impl RingLocation {
    pub const fn data_base_paddr(self) -> u64 {
        self.base_paddr + GpuRingHeader::SIZE as u64
    }
}

#[derive(Clone, Debug)]
pub struct ByteRing {
    loc: RingLocation,
    ring_size_bytes: u32,
}

impl ByteRing {
    pub fn init(
        mem: &mut dyn GuestMemory,
        loc: RingLocation,
        ring_size_bytes: u32,
    ) -> Result<Self, RingError> {
        if ring_size_bytes < 256 {
            return Err(RingError::InvalidRingHeader(
                "ring_size_bytes must be >= 256",
            ));
        }
        if ring_size_bytes % 8 != 0 {
            return Err(RingError::InvalidRingHeader(
                "ring_size_bytes must be 8-byte aligned",
            ));
        }

        let header = GpuRingHeader {
            magic: GPU_RING_MAGIC,
            abi_major: ABI_MAJOR,
            abi_minor: ABI_MINOR,
            ring_size_bytes,
            head: 0,
            tail: 0,
            _reserved: [0; 11],
        };
        write_ring_header(mem, loc.base_paddr, &header)?;
        // Data region contents are unspecified; zero for determinism in tests.
        mem.write_zeros(loc.data_base_paddr(), ring_size_bytes as usize)?;

        Ok(Self {
            loc,
            ring_size_bytes,
        })
    }

    pub fn open(mem: &dyn GuestMemory, loc: RingLocation) -> Result<Self, RingError> {
        let header = read_ring_header(mem, loc.base_paddr)?;
        if header.magic != GPU_RING_MAGIC {
            return Err(RingError::InvalidRingHeader("bad magic"));
        }
        if header.abi_major != ABI_MAJOR {
            return Err(RingError::InvalidRingHeader("ABI major mismatch"));
        }
        if header.ring_size_bytes < 256 || header.ring_size_bytes % 8 != 0 {
            return Err(RingError::InvalidRingHeader("invalid ring_size_bytes"));
        }
        Ok(Self {
            loc,
            ring_size_bytes: header.ring_size_bytes,
        })
    }

    pub fn ring_size_bytes(&self) -> u32 {
        self.ring_size_bytes
    }

    pub fn pop(&mut self, mem: &mut dyn GuestMemory) -> Result<Option<Vec<u8>>, RingError> {
        loop {
            let header = read_ring_header(mem, self.loc.base_paddr)?;
            let mut head = header.head;
            let tail = header.tail;
            if head == tail {
                return Ok(None);
            }

            if head >= self.ring_size_bytes || tail >= self.ring_size_bytes {
                return Err(RingError::InvalidRingHeader("head/tail out of range"));
            }

            let record_hdr_addr = self.loc.data_base_paddr() + head as u64;
            let record_hdr_bytes = mem.read_exact::<{ GpuRecordHeader::SIZE }>(record_hdr_addr)?;
            let record_magic = u32::from_le_bytes(record_hdr_bytes[0..4].try_into().unwrap());
            let record_size = u32::from_le_bytes(record_hdr_bytes[4..8].try_into().unwrap());

            if record_size < GpuRecordHeader::SIZE as u32 {
                return Err(RingError::InvalidRecord("record_size < header size"));
            }
            if record_size % 8 != 0 {
                return Err(RingError::InvalidRecord("record_size not 8-byte aligned"));
            }
            if record_size > self.ring_size_bytes {
                return Err(RingError::InvalidRecord("record_size exceeds ring size"));
            }

            let contig = self.ring_size_bytes - head;
            if record_size > contig {
                return Err(RingError::InvalidRecord(
                    "record spans end of ring (writer must insert pad record)",
                ));
            }

            if record_magic == GPU_PAD_MAGIC {
                // Wrap marker: consume it and restart at 0.
                head = 0;
                write_ring_head(mem, self.loc.base_paddr, head)?;
                continue;
            }

            let mut out = vec![0u8; record_size as usize];
            mem.read(record_hdr_addr, &mut out)?;

            head += record_size;
            if head == self.ring_size_bytes {
                head = 0;
            }
            write_ring_head(mem, self.loc.base_paddr, head)?;
            return Ok(Some(out));
        }
    }

    pub fn push(&mut self, mem: &mut dyn GuestMemory, record: &[u8]) -> Result<(), RingError> {
        if record.len() < GpuRecordHeader::SIZE {
            return Err(RingError::InvalidRecord("record too small"));
        }
        if record.len() % 8 != 0 {
            return Err(RingError::InvalidRecord("record size not 8-byte aligned"));
        }
        if record.len() > self.ring_size_bytes as usize {
            return Err(RingError::InvalidRecord("record larger than ring"));
        }

        let header = read_ring_header(mem, self.loc.base_paddr)?;
        let head = header.head;
        let mut tail = header.tail;
        if head >= self.ring_size_bytes || tail >= self.ring_size_bytes {
            return Err(RingError::InvalidRingHeader("head/tail out of range"));
        }

        let used = if tail >= head {
            tail - head
        } else {
            self.ring_size_bytes - (head - tail)
        };
        // Reserve 8 bytes so head==tail means "empty".
        let free = self.ring_size_bytes - used - 8;
        let record_len = record.len() as u32;
        if record_len > free {
            return Err(RingError::RingFull);
        }

        let contig = self.ring_size_bytes - tail;
        if record_len > contig {
            // Need to wrap: write an explicit pad record to consume the remainder.
            let pad_len = contig;
            debug_assert!(pad_len >= GpuRecordHeader::SIZE as u32);
            debug_assert!(pad_len % 8 == 0);
            let pad_addr = self.loc.data_base_paddr() + tail as u64;
            let mut pad = vec![0u8; pad_len as usize];
            pad[0..4].copy_from_slice(&GPU_PAD_MAGIC.to_le_bytes());
            pad[4..8].copy_from_slice(&pad_len.to_le_bytes());
            mem.write(pad_addr, &pad)?;

            tail = 0;
        }

        let dst_addr = self.loc.data_base_paddr() + tail as u64;
        mem.write(dst_addr, record)?;
        tail += record_len;
        if tail == self.ring_size_bytes {
            tail = 0;
        }
        write_ring_tail(mem, self.loc.base_paddr, tail)?;
        Ok(())
    }
}

fn read_ring_header(mem: &dyn GuestMemory, base: u64) -> Result<GpuRingHeader, RingError> {
    let bytes = mem.read_exact::<{ GpuRingHeader::SIZE }>(base)?;
    Ok(decode_ring_header(&bytes))
}

fn write_ring_header(
    mem: &mut dyn GuestMemory,
    base: u64,
    header: &GpuRingHeader,
) -> Result<(), RingError> {
    let bytes = encode_ring_header(header);
    mem.write(base, &bytes)?;
    Ok(())
}

fn write_ring_head(mem: &mut dyn GuestMemory, base: u64, head: u32) -> Result<(), RingError> {
    // head offset is 12 bytes into header.
    mem.write_u32_le(base + 12, head)?;
    Ok(())
}

fn write_ring_tail(mem: &mut dyn GuestMemory, base: u64, tail: u32) -> Result<(), RingError> {
    // tail offset is 16 bytes into header.
    mem.write_u32_le(base + 16, tail)?;
    Ok(())
}

fn decode_ring_header(bytes: &[u8; GpuRingHeader::SIZE]) -> GpuRingHeader {
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    let abi_major = u16::from_le_bytes(bytes[4..6].try_into().unwrap());
    let abi_minor = u16::from_le_bytes(bytes[6..8].try_into().unwrap());
    let ring_size_bytes = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    let head = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    let tail = u32::from_le_bytes(bytes[16..20].try_into().unwrap());

    // Reserved is always little-endian u32s.
    let mut reserved = [0u32; 11];
    let mut off = 20;
    for slot in &mut reserved {
        *slot = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        off += 4;
    }

    GpuRingHeader {
        magic,
        abi_major,
        abi_minor,
        ring_size_bytes,
        head,
        tail,
        _reserved: reserved,
    }
}

fn encode_ring_header(header: &GpuRingHeader) -> [u8; GpuRingHeader::SIZE] {
    let mut out = [0u8; GpuRingHeader::SIZE];
    out[0..4].copy_from_slice(&header.magic.to_le_bytes());
    out[4..6].copy_from_slice(&header.abi_major.to_le_bytes());
    out[6..8].copy_from_slice(&header.abi_minor.to_le_bytes());
    out[8..12].copy_from_slice(&header.ring_size_bytes.to_le_bytes());
    out[12..16].copy_from_slice(&header.head.to_le_bytes());
    out[16..20].copy_from_slice(&header.tail.to_le_bytes());

    let mut off = 20;
    for slot in header._reserved {
        out[off..off + 4].copy_from_slice(&slot.to_le_bytes());
        off += 4;
    }
    out
}
