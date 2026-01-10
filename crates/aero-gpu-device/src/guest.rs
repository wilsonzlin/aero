//! Host-side "synthetic guest" helpers.
//!
//! This module exists so tests (and eventually fuzzers) can drive the exact
//! same ring/ABI path that the real Windows guest will use.

use crate::abi;
use crate::guest_memory::GuestMemory;
use crate::ring::{ByteRing, RingError, RingLocation};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Completion {
    pub seq: u64,
    pub opcode: u16,
    pub status: u32,
}

#[derive(Debug)]
pub struct SyntheticGuest {
    cmd_ring: ByteRing,
    cpl_ring: ByteRing,
    next_seq: u64,
}

impl SyntheticGuest {
    pub fn init_rings(
        mem: &mut dyn GuestMemory,
        cmd_loc: RingLocation,
        cmd_ring_size_bytes: u32,
        cpl_loc: RingLocation,
        cpl_ring_size_bytes: u32,
    ) -> Result<Self, RingError> {
        let cmd_ring = ByteRing::init(mem, cmd_loc, cmd_ring_size_bytes)?;
        let cpl_ring = ByteRing::init(mem, cpl_loc, cpl_ring_size_bytes)?;
        Ok(Self {
            cmd_ring,
            cpl_ring,
            next_seq: 1,
        })
    }

    pub fn open(
        mem: &dyn GuestMemory,
        cmd_loc: RingLocation,
        cpl_loc: RingLocation,
    ) -> Result<Self, RingError> {
        let cmd_ring = ByteRing::open(mem, cmd_loc)?;
        let cpl_ring = ByteRing::open(mem, cpl_loc)?;
        Ok(Self {
            cmd_ring,
            cpl_ring,
            next_seq: 1,
        })
    }

    pub fn submit(
        &mut self,
        mem: &mut dyn GuestMemory,
        opcode: u16,
        payload: &[u8],
    ) -> Result<u64, RingError> {
        let seq = self.next_seq;
        self.next_seq += 1;
        let record = encode_command(opcode, seq, payload);
        self.cmd_ring.push(mem, &record)?;
        Ok(seq)
    }

    pub fn poll_completion(
        &mut self,
        mem: &mut dyn GuestMemory,
    ) -> Result<Option<Completion>, RingError> {
        let Some(record) = self.cpl_ring.pop(mem)? else {
            return Ok(None);
        };
        Ok(Some(decode_completion(&record).ok_or(
            RingError::InvalidRecord("bad completion record"),
        )?))
    }

    pub fn drain_completions(
        &mut self,
        mem: &mut dyn GuestMemory,
    ) -> Result<Vec<Completion>, RingError> {
        let mut out = Vec::new();
        while let Some(cpl) = self.poll_completion(mem)? {
            out.push(cpl);
        }
        Ok(out)
    }
}

pub fn encode_command(opcode: u16, seq: u64, payload: &[u8]) -> Vec<u8> {
    let payload_len = payload.len();
    let padded_payload_len = (payload_len + 7) & !7;
    let size_bytes = abi::GpuCmdHeader::SIZE + padded_payload_len;
    let mut out = vec![0u8; size_bytes];

    out[0..4].copy_from_slice(&abi::GPU_CMD_MAGIC.to_le_bytes());
    out[4..8].copy_from_slice(&(size_bytes as u32).to_le_bytes());
    out[8..10].copy_from_slice(&opcode.to_le_bytes());
    out[10..12].copy_from_slice(&0u16.to_le_bytes()); // flags
    out[12..14].copy_from_slice(&abi::ABI_MAJOR.to_le_bytes());
    out[14..16].copy_from_slice(&abi::ABI_MINOR.to_le_bytes());
    out[16..24].copy_from_slice(&seq.to_le_bytes());
    out[abi::GpuCmdHeader::SIZE..abi::GpuCmdHeader::SIZE + payload_len].copy_from_slice(payload);
    out
}

fn decode_completion(record: &[u8]) -> Option<Completion> {
    if record.len() < abi::GpuCompletion::SIZE {
        return None;
    }
    let magic = u32::from_le_bytes(record[0..4].try_into().ok()?);
    if magic != abi::GPU_CPL_MAGIC {
        return None;
    }
    let size = u32::from_le_bytes(record[4..8].try_into().ok()?) as usize;
    if size != record.len() {
        return None;
    }
    let seq = u64::from_le_bytes(record[8..16].try_into().ok()?);
    let opcode = u16::from_le_bytes(record[16..18].try_into().ok()?);
    let status = u32::from_le_bytes(record[20..24].try_into().ok()?);
    Some(Completion {
        seq,
        opcode,
        status,
    })
}
