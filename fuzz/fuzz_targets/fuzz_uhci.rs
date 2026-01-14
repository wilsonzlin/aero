#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::memory::MemoryBus;
use aero_usb::uhci::{regs::*, UhciController};

const MEM_SIZE: usize = 256 * 1024;
const MAX_OPS: usize = 1024;

// Minimal UHCI schedule seed layout (all within MEM_SIZE).
const FRAME_LIST_BASE: u32 = 0x1000;
const TD_SETUP_ADDR: u32 = 0x2000;
const TD_STATUS_ADDR: u32 = 0x2010;
const TD_INTR_ADDR: u32 = 0x2020;
const SETUP_BUF_ADDR: u32 = 0x3000;
const INTR_BUF_ADDR: u32 = 0x3100;

// UHCI link pointer bits / token PIDs / status bits (duplicated from `aero-usb` internals).
const LINK_PTR_TERMINATE: u32 = 1 << 0;
const PID_IN: u32 = 0x69;
const PID_SETUP: u32 = 0x2d;
const TD_STATUS_ACTIVE: u32 = 1 << 23;

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

    fn write_u32(&mut self, addr: u32, value: u32) {
        let addr = addr as usize;
        if addr.checked_add(4).is_none() || addr + 4 > self.data.len() {
            return;
        }
        self.data[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_bytes(&mut self, addr: u32, bytes: &[u8]) {
        let addr = addr as usize;
        if bytes.is_empty() {
            return;
        }
        if addr.checked_add(bytes.len()).is_none() || addr + bytes.len() > self.data.len() {
            return;
        }
        self.data[addr..addr + bytes.len()].copy_from_slice(bytes);
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

    // Seed a tiny, valid UHCI schedule so we immediately exercise TD processing even when `data`
    // does not happen to contain a well-formed frame list/QH/TD structure.
    //
    // Frame list: all entries point at a short TD chain:
    //   SETUP (SET_CONFIGURATION 1) -> STATUS IN (ZLP) -> interrupt IN (boot keyboard report) -> T
    //
    // This drives:
    // - endpoint-0 control transfer state machine (`AttachedUsbDevice`)
    // - interrupt IN polling (`UsbHidKeyboard`)
    // - schedule walking (including per-frame budgets/cycle guards)
    for i in 0..1024u32 {
        bus.write_u32(FRAME_LIST_BASE + i * 4, TD_SETUP_ADDR);
    }
    // SETUP TD.
    bus.write_u32(TD_SETUP_ADDR, TD_STATUS_ADDR);
    bus.write_u32(TD_SETUP_ADDR + 4, TD_STATUS_ACTIVE);
    bus.write_u32(
        TD_SETUP_ADDR + 8,
        PID_SETUP | (7u32 << 21), // max_len = 8
    );
    bus.write_u32(TD_SETUP_ADDR + 12, SETUP_BUF_ADDR);
    // STATUS (control IN, ZLP) TD.
    bus.write_u32(TD_STATUS_ADDR, TD_INTR_ADDR);
    bus.write_u32(TD_STATUS_ADDR + 4, TD_STATUS_ACTIVE);
    bus.write_u32(
        TD_STATUS_ADDR + 8,
        PID_IN | (0x7ffu32 << 21), // max_len = 0
    );
    bus.write_u32(TD_STATUS_ADDR + 12, 0);
    // Interrupt IN TD (endpoint 1, 8 bytes).
    bus.write_u32(TD_INTR_ADDR, LINK_PTR_TERMINATE);
    bus.write_u32(TD_INTR_ADDR + 4, TD_STATUS_ACTIVE);
    bus.write_u32(
        TD_INTR_ADDR + 8,
        PID_IN | (1u32 << 15) | (7u32 << 21), // ep=1, max_len=8
    );
    bus.write_u32(TD_INTR_ADDR + 12, INTR_BUF_ADDR);

    // SET_CONFIGURATION(1) setup packet (standard, device, host-to-device).
    bus.write_bytes(
        SETUP_BUF_ADDR,
        &[0x00, 0x09, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00],
    );

    let mut ctl = UhciController::new();

    // Attach a simple USB HID keyboard to exercise control/interrupt paths. Force-enable the port
    // so the schedule walker can resolve address 0 without requiring a full reset/enable sequence.
    let kbd = UsbHidKeyboardHandle::new();
    ctl.hub_mut().attach(0, Box::new(kbd.clone()));
    ctl.hub_mut().force_enable_for_tests(0);

    // Bias towards running schedule processing.
    ctl.io_write(REG_FLBASEADD, 4, FRAME_LIST_BASE);
    ctl.io_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_MAXP) as u32);

    // Press a key before configuration so the first successful SET_CONFIGURATION enqueues an
    // interrupt report, exercising both the DATA and NAK paths.
    kbd.key_event(0x04, true); // 'A' (usage ID)

    // Ensure we always run at least one tick per input so schedule walking is fuzzed even when the
    // input drives `ops=0`.
    ctl.tick_1ms(&mut bus);

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
                // Rearm the interrupt TD so repeated ticks continue to exercise device paths.
                let status = bus.read_u32(TD_INTR_ADDR as u64 + 4);
                bus.write_u32(TD_INTR_ADDR + 4, status | TD_STATUS_ACTIVE);
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
