//! Helpers for architecturally correct linear-memory accesses.
//!
//! Tier-0 and the assist layer often form a masked linear address using
//! [`crate::state::CpuState::apply_a20`], then perform a multi-byte bus access
//! (`read_u16`, `read_u32`, ...). The default `CpuBus` scalar helpers advance the
//! address with plain `+ 1`, which breaks architectural wrapping semantics when
//! a multi-byte access crosses:
//! - the 4GiB boundary in non-long modes (32-bit linear address space), or
//! - the A20 gate alias boundary in real/v8086 mode with A20 disabled.
//!
//! The helpers in this module fix that by re-applying [`CpuState::apply_a20`] to
//! each byte address (`addr + i`) so every byte is accessed through the correct
//! masked linear address.

use crate::exception::Exception;
use crate::mem::CpuBus;
use crate::state::{CpuMode, CpuState};

#[inline]
fn wrapped_segment_len(state: &CpuState, raw_addr: u64, remaining: usize) -> usize {
    debug_assert!(remaining > 0);

    // Long mode: no architectural masking. The only discontinuity comes from
    // overflowing the u64 address space.
    if state.mode == CpuMode::Long {
        let rem_u64 = u64::try_from(remaining).unwrap_or(u64::MAX);
        if raw_addr.checked_add(rem_u64.saturating_sub(1)).is_some() {
            return remaining;
        }

        // Safe: overflow implies `raw_addr != 0`, so `u64::MAX - raw_addr + 1` fits in u64.
        let until_wrap = (u64::MAX - raw_addr) + 1;
        let until_wrap_usize = usize::try_from(until_wrap).unwrap_or(usize::MAX);
        return remaining.min(until_wrap_usize);
    }

    // Non-long modes: linear addresses are 32-bit.
    let addr32 = raw_addr & 0xFFFF_FFFF;
    let mut max = 0x1_0000_0000u64 - addr32; // bytes until 32-bit wrap (inclusive)

    // With A20 disabled (real/v8086): bit 20 is forced low. Contiguity in masked
    // address space requires staying within a single 1MiB window (no bit-20
    // boundary crossing).
    if !state.a20_enabled && matches!(state.mode, CpuMode::Real | CpuMode::Vm86) {
        let low20 = addr32 & 0x000F_FFFF;
        let until_a20 = 0x1_00000u64 - low20;
        max = max.min(until_a20);
    }

    let max_usize = usize::try_from(max).unwrap_or(usize::MAX);
    remaining.min(max_usize)
}

#[inline]
pub(crate) fn contiguous_masked_start(state: &CpuState, addr: u64, len: usize) -> Option<u64> {
    if len == 0 {
        return Some(state.apply_a20(addr));
    }

    // A wrapped range is safe to use bus bulk helpers for iff it is contiguous in
    // masked linear address space. `wrapped_segment_len` gives the maximal
    // contiguous prefix; use it as a single source of truth for contiguity.
    if wrapped_segment_len(state, addr, len) == len {
        Some(state.apply_a20(addr))
    } else {
        None
    }
}

pub fn read_bytes_wrapped<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
    dst: &mut [u8],
) -> Result<(), Exception> {
    if dst.is_empty() {
        return Ok(());
    }

    if let Some(start) = contiguous_masked_start(state, addr, dst.len()) {
        return bus.read_bytes(start, dst);
    }

    // Slow path: split into contiguous runs in masked linear address space. This
    // avoids per-byte `read_u8` overhead for large reads that cross an
    // architectural wrap boundary (32-bit linear wrap, A20 alias wrap).
    let mut offset = 0usize;
    while offset < dst.len() {
        let raw = addr.wrapping_add(offset as u64);
        let start = state.apply_a20(raw);
        let len = wrapped_segment_len(state, raw, dst.len() - offset);
        bus.read_bytes(start, &mut dst[offset..offset + len])?;
        offset += len;
    }
    Ok(())
}

pub fn write_bytes_wrapped<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
    src: &[u8],
) -> Result<(), Exception> {
    if src.is_empty() {
        return Ok(());
    }

    if let Some(start) = contiguous_masked_start(state, addr, src.len()) {
        return bus.write_bytes(start, src);
    }
    // Split the access into contiguous runs in masked linear address space, then
    // preflight *all* runs before committing any writes. This keeps multi-byte
    // stores atomic w.r.t page faults even when the architectural address space
    // wraps (32-bit linear wrap, A20 alias wrap).
    //
    // Important: the number of discontinuities is tiny under x86's linear masking
    // rules (32-bit wrap + optional A20 wrap), so compute segment boundaries with
    // arithmetic rather than scanning byte-by-byte. We also avoid allocating a
    // temporary `Vec` by running two passes: preflight, then commit.

    // Pass 1: preflight all segments.
    let mut offset = 0usize;
    while offset < src.len() {
        let raw = addr.wrapping_add(offset as u64);
        let start = state.apply_a20(raw);
        let len = wrapped_segment_len(state, raw, src.len() - offset);
        bus.preflight_write_bytes(start, len)?;
        offset += len;
    }

    // Pass 2: commit writes.
    let mut offset = 0usize;
    while offset < src.len() {
        let raw = addr.wrapping_add(offset as u64);
        let start = state.apply_a20(raw);
        let len = wrapped_segment_len(state, raw, src.len() - offset);
        bus.write_bytes(start, &src[offset..offset + len])?;
        offset += len;
    }
    Ok(())
}

pub fn read_u16_wrapped<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
) -> Result<u16, Exception> {
    if let Some(start) = contiguous_masked_start(state, addr, 2) {
        return bus.read_u16(start);
    }
    let mut buf = [0u8; 2];
    read_bytes_wrapped(state, bus, addr, &mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

pub fn read_u32_wrapped<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
) -> Result<u32, Exception> {
    if let Some(start) = contiguous_masked_start(state, addr, 4) {
        return bus.read_u32(start);
    }
    let mut buf = [0u8; 4];
    read_bytes_wrapped(state, bus, addr, &mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

pub fn read_u64_wrapped<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
) -> Result<u64, Exception> {
    if let Some(start) = contiguous_masked_start(state, addr, 8) {
        return bus.read_u64(start);
    }
    let mut buf = [0u8; 8];
    read_bytes_wrapped(state, bus, addr, &mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

pub fn read_u128_wrapped<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
) -> Result<u128, Exception> {
    if let Some(start) = contiguous_masked_start(state, addr, 16) {
        return bus.read_u128(start);
    }
    let mut buf = [0u8; 16];
    read_bytes_wrapped(state, bus, addr, &mut buf)?;
    Ok(u128::from_le_bytes(buf))
}

pub fn write_u16_wrapped<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
    value: u16,
) -> Result<(), Exception> {
    if let Some(start) = contiguous_masked_start(state, addr, 2) {
        return bus.write_u16(start, value);
    }
    write_bytes_wrapped(state, bus, addr, &value.to_le_bytes())
}

pub fn write_u32_wrapped<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
    value: u32,
) -> Result<(), Exception> {
    if let Some(start) = contiguous_masked_start(state, addr, 4) {
        return bus.write_u32(start, value);
    }
    write_bytes_wrapped(state, bus, addr, &value.to_le_bytes())
}

pub fn write_u64_wrapped<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
    value: u64,
) -> Result<(), Exception> {
    if let Some(start) = contiguous_masked_start(state, addr, 8) {
        return bus.write_u64(start, value);
    }
    write_bytes_wrapped(state, bus, addr, &value.to_le_bytes())
}

pub fn write_u128_wrapped<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
    value: u128,
) -> Result<(), Exception> {
    if let Some(start) = contiguous_masked_start(state, addr, 16) {
        return bus.write_u128(start, value);
    }
    write_bytes_wrapped(state, bus, addr, &value.to_le_bytes())
}

pub fn fetch_wrapped<B: CpuBus>(
    state: &CpuState,
    bus: &mut B,
    addr: u64,
    max_len: usize,
) -> Result<[u8; 15], Exception> {
    let len = max_len.min(15);
    if len == 0 {
        return Ok([0u8; 15]);
    }

    if let Some(start) = contiguous_masked_start(state, addr, len) {
        return bus.fetch(start, len);
    }

    let mut buf = [0u8; 15];
    // Slow path: split into contiguous runs in masked linear address space. This
    // keeps instruction fetch efficient in the rare case where the architectural
    // address space wraps (32-bit wrap in non-long modes, A20 alias wrap).
    let mut offset = 0usize;
    while offset < len {
        let raw = addr.wrapping_add(offset as u64);
        let start = state.apply_a20(raw);
        let seg_len = wrapped_segment_len(state, raw, len - offset);
        // Use `CpuBus::fetch` (execute access) even on the slow path so paging-aware
        // busses (NX bit, supervisor execute restrictions) observe the correct access type.
        let chunk = bus.fetch(start, seg_len)?;
        buf[offset..offset + seg_len].copy_from_slice(&chunk[..seg_len]);
        offset += seg_len;
    }
    Ok(buf)
}
