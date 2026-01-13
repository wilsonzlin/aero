#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_cpu_core::linear_mem;
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_cpu_core::Exception;

/// Fixed backing size for the `FlatTestBus`.
///
/// This is sized to keep A20-boundary addresses (around 1MiB) in-bounds so we can
/// exercise the multi-segment wrapped slow paths without immediately faulting.
const MEM_SIZE: usize = 2 * 1024 * 1024;
/// Maximum fuzzed byte-slice length for wrapped helpers (kept small to avoid
/// excessive per-iteration runtime).
const MAX_LEN: usize = 4 * 1024;

fn mode_from_u8(v: u8) -> CpuMode {
    match v % 3 {
        0 => CpuMode::Real,
        1 => CpuMode::Protected,
        _ => CpuMode::Long,
    }
}

fn manual_read<const N: usize>(
    state: &CpuState,
    bus: &mut impl CpuBus,
    addr: u64,
) -> Result<[u8; N], Exception> {
    let mut out = [0u8; N];
    for (i, slot) in out.iter_mut().enumerate() {
        let byte_addr = state.apply_a20(addr.wrapping_add(i as u64));
        *slot = bus.read_u8(byte_addr)?;
    }
    Ok(out)
}

fn manual_fetch(
    state: &CpuState,
    bus: &mut impl CpuBus,
    addr: u64,
    max_len: usize,
) -> Result<[u8; 15], Exception> {
    let len = max_len.min(15);
    if len == 0 {
        return Ok([0u8; 15]);
    }

    let mut out = [0u8; 15];
    for i in 0..len {
        let byte_addr = state.apply_a20(addr.wrapping_add(i as u64));
        out[i] = bus.fetch(byte_addr, 1)?[0];
    }
    Ok(out)
}

fn preflight_byte_addrs(
    state: &CpuState,
    bus: &mut impl CpuBus,
    addr: u64,
    len: usize,
) -> (bool, Vec<(u64, u8)>) {
    let mut ok = true;
    let mut snapshot = Vec::new();
    for i in 0..len {
        let byte_addr = state.apply_a20(addr.wrapping_add(i as u64));
        match bus.read_u8(byte_addr) {
            Ok(v) => snapshot.push((byte_addr, v)),
            Err(_) => ok = false,
        }
    }
    (ok, snapshot)
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    let mode = mode_from_u8(u.arbitrary().unwrap_or(0));
    let a20_enabled: bool = u.arbitrary().unwrap_or(true);

    let mut state = CpuState::new(mode);
    state.a20_enabled = a20_enabled;

    // Random address + bounded byte-slice length (FZ-002).
    let offset: u64 = u.arbitrary().unwrap_or(0);
    let len: usize = u.int_in_range(0usize..=MAX_LEN).unwrap_or(0);
    let len_cross = len.max(2).min(MAX_LEN);
    // Sometimes force a larger access to stress segment splitting code paths.
    let len_stress = if (offset & 1) == 0 { MAX_LEN } else { len_cross };

    // Choose a base address biased towards interesting patterns while still
    // allowing truly huge u64 values to reach the helpers.
    let addr_kind: u8 = u.arbitrary().unwrap_or(0);
    let op: u8 = u.arbitrary().unwrap_or(0);
    let fetch_max_len: usize = u.arbitrary().unwrap_or(0usize);

    // Fuzz-derived values for scalar stores.
    let v16: u16 = u.arbitrary().unwrap_or(0);
    let v32: u32 = u.arbitrary().unwrap_or(0);
    let v64: u64 = u.arbitrary().unwrap_or(0);
    let v128: u128 = u.arbitrary().unwrap_or(0);

    // Buffers for byte-array helpers.
    let mut write_buf = vec![0u8; len_stress];
    for b in &mut write_buf {
        *b = u.arbitrary().unwrap_or(0);
    }
    let mut read_buf = vec![0u8; len_stress];

    let offset_in_mem = offset % (MEM_SIZE as u64);
    let base_addr = match addr_kind % 9 {
        // In-range.
        0 => offset_in_mem,
        // A20 bit set (aliases to in-range when A20 is disabled in real mode).
        1 => (1u64 << 20) | offset_in_mem,
        // 32-bit alias (wraps to in-range in non-long modes).
        2 => (1u64 << 32) | offset_in_mem,
        // Both A20 + 32-bit alias bits set.
        3 => (1u64 << 32) | (1u64 << 20) | offset_in_mem,
        // Near 4GiB boundary (exercises 32-bit wrapping logic even if the bus faults).
        4 => 0xFFFF_FFFFu64.wrapping_sub(offset % 32),
        // Near u64 overflow (exercises `wrapping_add` paths).
        5 => u64::MAX.wrapping_sub(offset % 32),
        // Near A20 boundary (real mode A20-disabled wrap).
        6 => 0x000F_FFFFu64.wrapping_sub(offset % 32),
        7 => 0x0010_0000u64.wrapping_add(offset % 32),
        // Totally arbitrary/hugely out-of-range.
        _ => u.arbitrary().unwrap_or(0),
    };

    // Explicit wrap-boundary cases.
    let addr_32_wrap = 0xFFFF_FFFFu64.wrapping_sub(offset % 32);
    let addr_a20_low = 0x000F_FFFFu64.wrapping_sub(offset % 32);
    let addr_a20_high = 0x0010_0000u64.wrapping_add(offset % 32);
    let addr_a20_fetch = 0x000F_FFF0u64.wrapping_add(offset % 16);

    let mut bus = FlatTestBus::new(MEM_SIZE);

    // Initialize a small prefix of RAM from the remaining input bytes.
    // Keep this bounded to avoid per-iteration O(MEM_SIZE) work.
    let init = u.take_rest();
    let init_len = init.len().min(MEM_SIZE);
    let _ = bus.write_bytes(0, &init[..init_len]);

    // Pick an op to validate more strictly (helps catch logic bugs); the rest of
    // the helpers are called below for panic/overflow coverage.
    match op % 6 {
        0 => {
            let expected = manual_read::<2>(&state, &mut bus, base_addr)
                .map(|b| u16::from_le_bytes(b));
            let actual = linear_mem::read_u16_wrapped(&state, &mut bus, base_addr);
            assert_eq!(actual, expected);
        }
        1 => {
            let expected = manual_read::<4>(&state, &mut bus, base_addr)
                .map(|b| u32::from_le_bytes(b));
            let actual = linear_mem::read_u32_wrapped(&state, &mut bus, base_addr);
            assert_eq!(actual, expected);
        }
        2 => {
            let expected = manual_read::<8>(&state, &mut bus, base_addr)
                .map(|b| u64::from_le_bytes(b));
            let actual = linear_mem::read_u64_wrapped(&state, &mut bus, base_addr);
            assert_eq!(actual, expected);
        }
        3 => {
            let expected = manual_read::<16>(&state, &mut bus, base_addr)
                .map(|b| u128::from_le_bytes(b));
            let actual = linear_mem::read_u128_wrapped(&state, &mut bus, base_addr);
            assert_eq!(actual, expected);
        }
        4 => {
            let (preflight_ok, snapshot) = preflight_byte_addrs(&state, &mut bus, base_addr, 8);
            let result = linear_mem::write_u64_wrapped(&state, &mut bus, base_addr, v64);

            if preflight_ok {
                assert!(result.is_ok());
                let bytes = v64.to_le_bytes();
                for (i, byte) in bytes.iter().enumerate() {
                    let byte_addr = state.apply_a20(base_addr.wrapping_add(i as u64));
                    assert_eq!(bus.read_u8(byte_addr), Ok(*byte));
                }
                assert_eq!(
                    linear_mem::read_u64_wrapped(&state, &mut bus, base_addr),
                    Ok(v64)
                );
            } else {
                assert!(result.is_err());
                // Wrapped helpers must not partially commit any bytes on failure.
                for (addr, before) in snapshot {
                    assert_eq!(bus.read_u8(addr), Ok(before));
                }
            }
        }
        _ => {
            let expected = manual_fetch(&state, &mut bus, base_addr, fetch_max_len);
            let actual = linear_mem::fetch_wrapped(&state, &mut bus, base_addr, fetch_max_len);
            assert_eq!(actual, expected);
        }
    }

    // ---- Byte slice helpers -------------------------------------------------
    let _ = linear_mem::read_bytes_wrapped(&state, &mut bus, base_addr, &mut read_buf[..len]);
    let _ = linear_mem::write_bytes_wrapped(&state, &mut bus, base_addr, &write_buf[..len]);

    // ---- Scalar helpers -----------------------------------------------------
    let _ = linear_mem::read_u16_wrapped(&state, &mut bus, base_addr);
    let _ = linear_mem::read_u32_wrapped(&state, &mut bus, base_addr);
    let _ = linear_mem::read_u64_wrapped(&state, &mut bus, base_addr);
    let _ = linear_mem::read_u128_wrapped(&state, &mut bus, base_addr);

    let _ = linear_mem::write_u16_wrapped(&state, &mut bus, base_addr, v16);
    let _ = linear_mem::write_u32_wrapped(&state, &mut bus, base_addr, v32);
    let _ = linear_mem::write_u64_wrapped(&state, &mut bus, base_addr, v64);
    let _ = linear_mem::write_u128_wrapped(&state, &mut bus, base_addr, v128);

    // ---- Fetch helper -------------------------------------------------------
    let _ = linear_mem::fetch_wrapped(&state, &mut bus, base_addr, fetch_max_len);

    // ---- Boundary cases -----------------------------------------------------
    //
    // 32-bit wrap boundary (non-long modes): near 0xFFFF_FFFF.
    let _ = linear_mem::read_bytes_wrapped(
        &state,
        &mut bus,
        addr_32_wrap,
        &mut read_buf[..len_stress],
    );
    let _ = linear_mem::write_bytes_wrapped(&state, &mut bus, addr_32_wrap, &write_buf[..len_stress]);

    // A20 wrap boundary (real/v8086 mode when disabled): around 0x000F_FFFF/0x0010_0000.
    let _ = linear_mem::read_bytes_wrapped(
        &state,
        &mut bus,
        addr_a20_low,
        &mut read_buf[..len_stress],
    );
    let _ = linear_mem::write_bytes_wrapped(&state, &mut bus, addr_a20_low, &write_buf[..len_stress]);
    let _ = linear_mem::fetch_wrapped(&state, &mut bus, addr_a20_low, 15);
    let _ = linear_mem::fetch_wrapped(&state, &mut bus, addr_a20_high, 15);

    // Exact boundary values to maximize wrap/crossing probability.
    let _ = linear_mem::read_bytes_wrapped(
        &state,
        &mut bus,
        0xFFFF_FFFF,
        &mut read_buf[..len_cross],
    );
    let _ = linear_mem::write_bytes_wrapped(&state, &mut bus, 0xFFFF_FFFF, &write_buf[..len_cross]);

    let _ = linear_mem::read_bytes_wrapped(
        &state,
        &mut bus,
        0x000F_FFFF,
        &mut read_buf[..len_cross],
    );
    let _ = linear_mem::write_bytes_wrapped(&state, &mut bus, 0x000F_FFFF, &write_buf[..len_cross]);

    let _ = linear_mem::fetch_wrapped(&state, &mut bus, addr_a20_fetch, 15);
});

