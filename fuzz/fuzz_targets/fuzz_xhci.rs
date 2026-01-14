#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::memory::MemoryBus;
use aero_usb::xhci::{regs, trb::*, XhciController};

const MEM_SIZE: usize = 256 * 1024;
const MAX_OPS: usize = 1024;

// -----------------------------------------------------------------------------
// Minimal xHCI ring seed layout (all within MEM_SIZE).
// -----------------------------------------------------------------------------

const DCBAA_BASE: u64 = 0x1000;
const DEV_CTX_BASE: u64 = 0x2000;
const INPUT_CTX_BASE: u64 = 0x3000;
const CMD_RING_BASE: u64 = 0x4000;
const EVENT_RING_BASE: u64 = 0x5000;
const ERST_BASE: u64 = 0x6000;

/// Bounded guest-physical memory for xHCI ring fuzzing.
///
/// Reads outside the provided buffer return zeros; writes are dropped.
#[derive(Clone)]
struct FuzzBus {
    data: Vec<u8>,
    dma: bool,
}

impl FuzzBus {
    fn new(size: usize, init: &[u8]) -> Self {
        let mut data = vec![0u8; size];
        let n = init.len().min(size);
        data[..n].copy_from_slice(&init[..n]);
        Self { data, dma: true }
    }

    fn write_u32(&mut self, addr: u64, value: u32) {
        let Ok(addr) = usize::try_from(addr) else {
            return;
        };
        if addr.checked_add(4).is_none() || addr + 4 > self.data.len() {
            return;
        }
        self.data[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(&mut self, addr: u64, value: u64) {
        let Ok(addr) = usize::try_from(addr) else {
            return;
        };
        if addr.checked_add(8).is_none() || addr + 8 > self.data.len() {
            return;
        }
        self.data[addr..addr + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn write_trb(&mut self, addr: u64, trb: Trb) {
        self.write_physical(addr, &trb.to_bytes());
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

    fn dma_enabled(&self) -> bool {
        self.dma
    }
}

fn decode_size(bits: u8) -> usize {
    match bits % 4 {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 8,
    }
}

fn biased_offset(u: &mut Unstructured<'_>, port_count: usize) -> u64 {
    let sel: u8 = u.arbitrary().unwrap_or(0);
    // ~75% pick a known register; otherwise pick any offset in the MMIO window.
    if sel & 0b11 != 0 {
        match sel % 14 {
            0 => regs::REG_USBCMD,
            1 => regs::REG_USBSTS,
            2 => regs::REG_CRCR_LO,
            3 => regs::REG_DCBAAP_LO,
            4 => regs::REG_CONFIG,
            5 => regs::REG_MFINDEX,
            6 => regs::REG_INTR0_IMAN,
            7 => regs::REG_INTR0_ERSTSZ,
            8 => regs::REG_INTR0_ERSTBA_LO,
            9 => regs::REG_INTR0_ERDP_LO,
            10 => regs::port::portsc_offset(0),
            11 => regs::port::portsc_offset(port_count.saturating_sub(1)),
            12 => u64::from(regs::DBOFF_VALUE),
            _ => regs::port::portsc_offset((sel as usize) % port_count.max(1)),
        }
    } else {
        u.int_in_range(0u64..=(XhciController::MMIO_SIZE as u64).saturating_sub(1))
            .unwrap_or(0)
    }
}

fn seed_controller_state(bus: &mut FuzzBus, xhci: &mut XhciController) {
    // --- Guest memory structures ---
    //
    // Device context EP0 starts with MPS=8 (matches real device default).
    bus.write_u32(DEV_CTX_BASE + 0x20 + 4, 8u32 << 16);

    // Input control context: Drop=0, Add = Slot + EP0.
    bus.write_u32(INPUT_CTX_BASE, 0);
    bus.write_u32(INPUT_CTX_BASE + 0x04, (1 << 0) | (1 << 1));

    // Input EP0 context requests MPS=64 and Interval=5.
    bus.write_u32(INPUT_CTX_BASE + 0x40, 5u32 << 16);
    bus.write_u32(INPUT_CTX_BASE + 0x40 + 4, 64u32 << 16);
    bus.write_u32(INPUT_CTX_BASE + 0x40 + 8, 0xdead_bee0);
    bus.write_u32(INPUT_CTX_BASE + 0x40 + 12, 0);

    // Command ring:
    //  - TRB0: Enable Slot (cycle=1)
    //  - TRB1: cycle=0 sentinel (ring empty after TRB0)
    {
        let mut trb0 = Trb::new(0, 0, 0);
        trb0.set_trb_type(TrbType::EnableSlotCommand);
        trb0.set_cycle(true);
        bus.write_trb(CMD_RING_BASE, trb0);
    }
    {
        let mut stop = Trb::new(0, 0, 0);
        stop.set_trb_type(TrbType::NoOpCommand);
        stop.set_cycle(false);
        bus.write_trb(CMD_RING_BASE + TRB_LEN as u64, stop);
    }

    // Event Ring Segment Table (ERST) with a single segment pointing at EVENT_RING_BASE.
    bus.write_u64(ERST_BASE, EVENT_RING_BASE);
    bus.write_u32(ERST_BASE + 8, 16); // segment size in TRBs
    bus.write_u32(ERST_BASE + 12, 0);

    // --- Controller MMIO programming ---
    xhci.mmio_write(regs::REG_DCBAAP_LO, 4, DCBAA_BASE);
    xhci.mmio_write(regs::REG_DCBAAP_HI, 4, DCBAA_BASE >> 32);
    xhci.mmio_write(regs::REG_CONFIG, 4, 8); // MaxSlotsEn

    // Command ring base + RCS=1.
    xhci.mmio_write(regs::REG_CRCR_LO, 4, CMD_RING_BASE | 1);
    xhci.mmio_write(regs::REG_CRCR_HI, 4, CMD_RING_BASE >> 32);

    // Program interrupter 0 event ring.
    xhci.mmio_write(regs::REG_INTR0_IMAN, 4, u64::from(regs::IMAN_IE));
    xhci.mmio_write(regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_LO, 4, ERST_BASE);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_HI, 4, ERST_BASE >> 32);
    xhci.mmio_write(regs::REG_INTR0_ERDP_LO, 4, EVENT_RING_BASE);
    xhci.mmio_write(regs::REG_INTR0_ERDP_HI, 4, EVENT_RING_BASE >> 32);

    // Start controller and run one tick to service the synthetic RUN-transition DMA/IRQ.
    xhci.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_RUN));
}

fn rearm_command_ring(bus: &mut FuzzBus, xhci: &mut XhciController) {
    // Reset command ring state back to TRB0 with cycle=1.
    {
        let mut trb0 = Trb::new(0, 0, 0);
        trb0.set_trb_type(TrbType::EnableSlotCommand);
        trb0.set_cycle(true);
        bus.write_trb(CMD_RING_BASE, trb0);
    }
    {
        let mut stop = Trb::new(0, 0, 0);
        stop.set_trb_type(TrbType::NoOpCommand);
        stop.set_cycle(false);
        bus.write_trb(CMD_RING_BASE + TRB_LEN as u64, stop);
    }
    xhci.mmio_write(regs::REG_CRCR_LO, 4, CMD_RING_BASE | 1);
    xhci.mmio_write(regs::REG_CRCR_HI, 4, CMD_RING_BASE >> 32);
    xhci.mmio_write(u64::from(regs::DBOFF_VALUE), 4, 0);
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);
    let mut bus = FuzzBus::new(MEM_SIZE, data);

    let mut xhci = XhciController::new();

    // Attach a USB HID keyboard so port snapshots include nested device trees and we can inject key
    // events during fuzzing.
    let kbd = UsbHidKeyboardHandle::new();
    xhci.attach_device(0, Box::new(kbd.clone()));

    seed_controller_state(&mut bus, &mut xhci);

    // Tick once to execute the deferred DMA-on-RUN probe and drive port timers.
    xhci.tick_1ms(&mut bus);

    // Clear the synthetic RUN-transition IRQ so later command/event interrupts are easier to
    // distinguish.
    xhci.mmio_write(regs::REG_USBSTS, 4, u64::from(regs::USBSTS_EINT));

    // Ring doorbell 0 to process the command ring (Enable Slot).
    xhci.mmio_write(u64::from(regs::DBOFF_VALUE), 4, 0);
    xhci.tick_1ms(&mut bus);

    // Enable Slot clears DCBAA[1] to 0; install the device context pointer after it completes so
    // subsequent commands have a valid output context target.
    bus.write_u64(DCBAA_BASE + 8, DEV_CTX_BASE);

    let ops: usize = u.int_in_range(0usize..=MAX_OPS).unwrap_or(0);
    let port_count = usize::from(xhci.port_count());

    for _ in 0..ops {
        let tag: u8 = u.arbitrary().unwrap_or(0);
        match tag % 10 {
            0 | 1 | 2 => {
                let offset = biased_offset(&mut u, port_count);
                let size = decode_size(tag >> 3);
                let _ = xhci.mmio_read(offset, size);
            }
            3 | 4 | 5 => {
                let offset = biased_offset(&mut u, port_count);
                let size_bits: u8 = u.arbitrary().unwrap_or(0);
                let size = decode_size(size_bits);
                let value: u64 = u.arbitrary().unwrap_or(0);
                xhci.mmio_write(offset, size, value);
            }
            6 => {
                // Run one 1ms tick (drives port timers, command ring, transfer rings, event ring).
                xhci.tick_1ms(&mut bus);
            }
            7 => {
                // Rearm the command ring back to a known, small sequence and ring DB0.
                rearm_command_ring(&mut bus, &mut xhci);
                xhci.tick_1ms(&mut bus);
            }
            8 => {
                // Snapshot roundtrip to stress TLV encode/decode and nested device snapshots.
                let snap = xhci.save_state();
                let mut fresh = XhciController::new();
                // Pre-attach keyboard so restored snapshots apply onto the same Rc handle.
                fresh.attach_device(0, Box::new(kbd.clone()));
                let _ = fresh.load_state(&snap);
                xhci = fresh;
            }
            _ => {
                // Toggle DMA availability and inject keyboard events.
                if let Some(flag) = u.arbitrary::<u8>().ok() {
                    bus.dma = (flag & 1) != 0;
                }
                let usage: u8 = u.arbitrary().unwrap_or(0);
                let pressed: bool = u.arbitrary().unwrap_or(false);
                kbd.key_event(usage, pressed);
            }
        }
    }
});

