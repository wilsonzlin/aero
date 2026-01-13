#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_cpu_core::linear_mem;
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_cpu_core::Exception;

/// Small, fixed memory backing for the `FlatTestBus`.
///
/// This doesn't need to be large to exercise 4GiB/A20 aliasing because the
/// wrapped helpers mask/transform the provided linear address before issuing
/// per-byte bus accesses.
const MEM_SIZE: usize = 256 * 1024;

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

    // Bias addresses towards interesting architectural wrap/alias patterns while
    // still allowing truly huge u64 values to reach the helpers.
    let offset: u64 = u.arbitrary().unwrap_or(0);
    let offset_in_mem = offset % (MEM_SIZE as u64);
    let addr_kind: u8 = u.arbitrary().unwrap_or(0);
    let base_addr = match addr_kind % 7 {
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
        // Totally arbitrary/hugely out-of-range.
        _ => u.arbitrary().unwrap_or(0),
    };

    let op: u8 = u.arbitrary().unwrap_or(0);
    let op_kind = op % 6;

    // Op-specific extra inputs (consumed before we hand the remainder to RAM init).
    let write_value: u64 = if op_kind == 4 { u.arbitrary().unwrap_or(0) } else { 0 };
    let fetch_max_len: usize = if op_kind == 5 {
        u.arbitrary().unwrap_or(0usize)
    } else {
        0
    };

    let mut bus = FlatTestBus::new(MEM_SIZE);

    // Initialize a small prefix of RAM from the remaining input bytes.
    // Keep this bounded to avoid per-iteration O(MEM_SIZE) work.
    let init = u.take_rest();
    let init_len = init.len().min(MEM_SIZE);
    let _ = bus.write_bytes(0, &init[..init_len]);

    match op_kind {
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
            let result =
                linear_mem::write_u64_wrapped(&state, &mut bus, base_addr, write_value);

            if preflight_ok {
                assert!(result.is_ok());
                let bytes = write_value.to_le_bytes();
                for (i, byte) in bytes.iter().enumerate() {
                    let byte_addr = state.apply_a20(base_addr.wrapping_add(i as u64));
                    assert_eq!(bus.read_u8(byte_addr), Ok(*byte));
                }
                assert_eq!(
                    linear_mem::read_u64_wrapped(&state, &mut bus, base_addr),
                    Ok(write_value)
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
});
