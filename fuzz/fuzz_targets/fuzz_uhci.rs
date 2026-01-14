#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::memory::MemoryBus;
use aero_usb::uhci::{regs::*, UhciController};

const MEM_SIZE: usize = 256 * 1024;
const MAX_OPS: usize = 1024;

/// Simple bounded guest-physical memory implementation for UHCI schedule fuzzing.
///
/// Reads outside the provided buffer return zeros; writes are dropped. This matches common
/// "unmapped memory" semantics and ensures the fuzzer cannot trigger host-side OOB accesses via
/// malicious schedule pointers.
#[derive(Clone)]
struct FuzzBus {
    data: Vec<u8>,
}

impl FuzzBus {
    fn new(size: usize, init: &[u8]) -> Self {
        let mut data = vec![0u8; size];
        let n = init.len().min(size);
        data[..n].copy_from_slice(&init[..n]);
        Self { data }
    }
}

impl MemoryBus for FuzzBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        buf.fill(0);
        if buf.is_empty() {
            return;
        }
        if paddr.checked_add(buf.len() as u64).is_none() {
            return;
        }
        let Ok(start) = usize::try_from(paddr) else {
            return;
        };
        if start >= self.data.len() {
            return;
        }
        let avail = self.data.len() - start;
        let n = avail.min(buf.len());
        buf[..n].copy_from_slice(&self.data[start..start + n]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }
        if paddr.checked_add(buf.len() as u64).is_none() {
            return;
        }
        let Ok(start) = usize::try_from(paddr) else {
            return;
        };
        if start >= self.data.len() {
            return;
        }
        let avail = self.data.len() - start;
        let n = avail.min(buf.len());
        self.data[start..start + n].copy_from_slice(&buf[..n]);
    }
}

fn decode_size(bits: u8) -> usize {
    match bits % 3 {
        0 => 1,
        1 => 2,
        _ => 4,
    }
}

fn biased_offset(u: &mut Unstructured<'_>) -> u16 {
    let sel: u8 = u.arbitrary().unwrap_or(0);
    // ~75% of the time, pick from known registers; otherwise allow arbitrary offsets.
    if sel & 0b11 != 0 {
        match sel % 7 {
            0 => REG_USBCMD,
            1 => REG_USBSTS,
            2 => REG_USBINTR,
            3 => REG_FRNUM,
            4 => REG_FLBASEADD,
            5 => REG_PORTSC1,
            _ => REG_PORTSC2,
        }
    } else {
        u.int_in_range(0u16..=0x3f).unwrap_or(0)
    }
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // Fixed-size RAM backing to keep allocations bounded and deterministic.
    let mut bus = FuzzBus::new(MEM_SIZE, data);

    // Ensure address 0 always contains a terminating link pointer (T=1). This prevents the UHCI
    // schedule walker from getting stuck in cycles when it chases malformed pointers into unmapped
    // memory (reads return 0 => addr 0).
    if bus.data.len() >= 4 {
        bus.data[..4].copy_from_slice(&1u32.to_le_bytes());
    }

    let mut ctl = UhciController::new();

    // Attach a simple USB HID keyboard to exercise control/interrupt paths. Force-enable the port
    // so the schedule walker can resolve address 0 without requiring a full reset/enable sequence.
    ctl.hub_mut()
        .attach(0, Box::new(UsbHidKeyboardHandle::new()));
    ctl.hub_mut().force_enable_for_tests(0);

    // Bias towards running schedule processing.
    ctl.io_write(REG_FLBASEADD, 4, 0x1000);
    ctl.io_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_MAXP) as u32);

    let ops: usize = u.int_in_range(0usize..=MAX_OPS).unwrap_or(0);
    for _ in 0..ops {
        let tag: u8 = u.arbitrary().unwrap_or(0);
        match tag % 8 {
            0 | 1 | 2 => {
                let offset = biased_offset(&mut u);
                let size = decode_size(tag >> 3);
                let _ = ctl.io_read(offset, size);
            }
            3 | 4 | 5 => {
                let offset = biased_offset(&mut u);
                let size_bits: u8 = u.arbitrary().unwrap_or(0);
                let size = decode_size(size_bits);
                let value: u32 = u.arbitrary().unwrap_or(0);
                ctl.io_write(offset, size, value);
            }
            6 => {
                ctl.tick_1ms(&mut bus);
            }
            _ => {
                // Snapshot roundtrip to stress TLV decode/encode paths.
                let snap = ctl.save_state();
                let mut fresh = UhciController::new();
                let _ = fresh.load_state(&snap);
                ctl = fresh;
            }
        }
    }
});

