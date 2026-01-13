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
pub(crate) fn contiguous_masked_start(state: &CpuState, addr: u64, len: usize) -> Option<u64> {
    if len <= 1 {
        return Some(state.apply_a20(addr));
    }

    // Long mode: no architectural linear masking, so any non-overflowing range
    // is contiguous.
    if state.mode == CpuMode::Long {
        // Avoid using bus bulk helpers when the range wraps the u64 address
        // space; split slow-path per-byte instead.
        let span = len.checked_sub(1)? as u64;
        addr.checked_add(span)?;
        return Some(addr);
    }

    let start = state.apply_a20(addr);
    let span = len.checked_sub(1)? as u64;

    // A20 masking is only applied in real/v8086 mode when disabled.
    if !state.a20_enabled && matches!(state.mode, CpuMode::Real | CpuMode::Vm86) {
        // Conservative contiguity check: require the range to stay within a
        // single "1MiB window" (bit-20 does not change) *and* not overflow the
        // 32-bit linear address space.
        let addr32 = addr & 0xFFFF_FFFF;
        let low20 = addr32 & 0x000F_FFFF;
        if low20.checked_add(span)? > 0x000F_FFFF {
            return None;
        }
        if addr32.checked_add(span)? > 0xFFFF_FFFF {
            return None;
        }
        return Some(start);
    }

    // Non-long modes always truncate linear addresses to 32 bits. Bulk accesses
    // are only safe when the full range stays within one 32-bit window.
    let start32 = start as u32 as u64;
    if start32.checked_add(span)? > 0xFFFF_FFFF {
        return None;
    }
    Some(start)
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

    for (i, slot) in dst.iter_mut().enumerate() {
        let byte_addr = state.apply_a20(addr.wrapping_add(i as u64));
        *slot = bus.read_u8(byte_addr)?;
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

    #[derive(Clone, Copy)]
    struct Segment {
        start: u64,
        offset: usize,
        len: usize,
    }

    // Upper bound on how many segments we will record on the heap. In pathological
    // wrap patterns (e.g. A20 aliasing every 1MiB for very large buffers), the
    // number of segments can become very large and we want to avoid allocating a
    // Vec proportional to the write length.
    const MAX_STORED_SEGMENTS: usize = 4096;

    // Split the access into maximal contiguous runs in masked linear address space,
    // then preflight *all* runs before committing any writes. This keeps multi-byte
    // stores atomic w.r.t page faults even when the architectural address space
    // wraps (32-bit linear wrap, A20 alias wrap).
    //
    // We store segments only up to MAX_STORED_SEGMENTS; if the write fragments
    // further, we fall back to a second pass for the tail without allocating.
    let mut segments: Vec<Segment> = Vec::new();
    let mut segment_count = 0usize;

    let mut seg_start_offset = 0usize;
    let mut seg_start_addr = state.apply_a20(addr);
    let mut prev = seg_start_addr;

    // Preflight pass (no writes).
    for i in 1..src.len() {
        let masked = state.apply_a20(addr.wrapping_add(i as u64));
        if prev.checked_add(1) != Some(masked) {
            let seg_len = i - seg_start_offset;
            bus.preflight_write_bytes(seg_start_addr, seg_len)?;
            segment_count += 1;
            if segment_count <= MAX_STORED_SEGMENTS {
                segments.push(Segment {
                    start: seg_start_addr,
                    offset: seg_start_offset,
                    len: seg_len,
                });
            }
            seg_start_offset = i;
            seg_start_addr = masked;
        }
        prev = masked;
    }

    let seg_len = src.len() - seg_start_offset;
    bus.preflight_write_bytes(seg_start_addr, seg_len)?;
    segment_count += 1;
    if segment_count <= MAX_STORED_SEGMENTS {
        segments.push(Segment {
            start: seg_start_addr,
            offset: seg_start_offset,
            len: seg_len,
        });
    }

    // Commit pass: if the segmentation is small, use the stored descriptors;
    // otherwise, write the stored prefix and redo segmentation for the tail
    // without allocation.
    for seg in &segments {
        bus.write_bytes(seg.start, &src[seg.offset..seg.offset + seg.len])?;
    }

    if segment_count <= MAX_STORED_SEGMENTS {
        return Ok(());
    }

    // Too many segments to store: recompute segmentation for the remaining
    // portion (after the prefix we already wrote), without building a large Vec.
    let offset = segments.iter().map(|s| s.len).sum::<usize>();
    debug_assert!(offset < src.len());

    let mut seg_start_offset = offset;
    let mut seg_start_addr = state.apply_a20(addr.wrapping_add(offset as u64));
    let mut prev = seg_start_addr;

    for i in (offset + 1)..src.len() {
        let masked = state.apply_a20(addr.wrapping_add(i as u64));
        if prev.checked_add(1) != Some(masked) {
            let seg_len = i - seg_start_offset;
            bus.write_bytes(seg_start_addr, &src[seg_start_offset..seg_start_offset + seg_len])?;
            seg_start_offset = i;
            seg_start_addr = masked;
        }
        prev = masked;
    }
    let seg_len = src.len() - seg_start_offset;
    bus.write_bytes(seg_start_addr, &src[seg_start_offset..seg_start_offset + seg_len])?;
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
    for (i, byte) in buf.iter_mut().enumerate().take(len) {
        let byte_addr = state.apply_a20(addr.wrapping_add(i as u64));
        // Use `CpuBus::fetch` (execute access) even on the slow path so paging-aware
        // busses (NX bit, supervisor execute restrictions) observe the correct access type.
        *byte = bus.fetch(byte_addr, 1)?[0];
    }
    Ok(buf)
}
