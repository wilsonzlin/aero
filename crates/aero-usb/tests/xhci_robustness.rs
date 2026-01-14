//! Robustness tests for xHCI MMIO and TRB ring parsing.
//!
//! These tests focus on *defensive behavior* when guest-provided inputs are malformed:
//! - no panics on odd MMIO access sizes/offsets
//! - bounded work when walking rings containing Link TRB loops

use std::panic::{catch_unwind, AssertUnwindSafe};

use aero_usb::xhci::ring::{RingCursor, RingError, RingPoll};
use aero_usb::xhci::trb::{Trb, TrbType};
use aero_usb::xhci::XhciController;
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel};

mod util;

use util::TestMemory;

/// A tiny `MemoryBus` implementation that never panics on out-of-range accesses.
///
/// This is important for xHCI MMIO fuzzing because the controller may perform DMA reads from guest
/// addresses sourced from guest-controlled registers (e.g. CRCR).
struct SafeMemory {
    bytes: Vec<u8>,
}

impl SafeMemory {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
        }
    }
}

impl MemoryBus for SafeMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let Ok(start) = usize::try_from(paddr) else {
            buf.fill(0xFF);
            return;
        };
        if start >= self.bytes.len() {
            buf.fill(0xFF);
            return;
        }

        let available = self.bytes.len() - start;
        let take = available.min(buf.len());
        buf[..take].copy_from_slice(&self.bytes[start..start + take]);
        buf[take..].fill(0xFF);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let Ok(start) = usize::try_from(paddr) else {
            return;
        };
        if start >= self.bytes.len() {
            return;
        }

        let available = self.bytes.len() - start;
        let take = available.min(buf.len());
        self.bytes[start..start + take].copy_from_slice(&buf[..take]);
    }
}

#[derive(Default)]
struct DummyDevice;

impl UsbDeviceModel for DummyDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }
}

#[derive(Clone)]
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        // LCG with good spectral properties (PCG-style multiplier, but without permutation).
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }

    fn pick_from<T: Copy>(&mut self, values: &[T]) -> T {
        let idx = (self.next_u64() as usize) % values.len();
        values[idx]
    }

    fn gen_offset(&mut self, mmio_size: u64) -> u64 {
        match self.next_u64() % 8 {
            // In-bounds offsets.
            0 => self.next_u64() % mmio_size,
            // Just out-of-bounds.
            1 => mmio_size + (self.next_u64() % (mmio_size.max(1))),
            // Around the end-of-window boundary (including potential crossing).
            2 => mmio_size.saturating_sub(32) + (self.next_u64() % 128),
            // Small offsets.
            3 => self.next_u64() & 0xff,
            // 32-bit-ish offsets.
            4 => self.next_u64() & 0xffff_ffff,
            // Large offsets with high bit set.
            5 => self.next_u64() | (1u64 << 63),
            // Near u64::MAX.
            6 => u64::MAX - (self.next_u64() % 64),
            // Arbitrary.
            _ => self.next_u64(),
        }
    }
}

#[test]
fn xhci_port_helpers_do_not_panic_on_invalid_port_indices() {
    let mut xhci = XhciController::with_port_count(1);

    // Out-of-range port indices should not panic; these helpers are used by host-side topology
    // management (WASM UI) and should be defensive.
    assert_eq!(xhci.read_portsc(1), 0);
    assert_eq!(xhci.read_portsc(usize::MAX), 0);

    xhci.write_portsc(1, 0);
    xhci.write_portsc(usize::MAX, 0);

    xhci.attach_device(1, Box::new(DummyDevice::default()));
    xhci.attach_device(usize::MAX, Box::new(DummyDevice::default()));

    xhci.detach_device(1);
    xhci.detach_device(usize::MAX);
}

#[test]
fn xhci_mmio_read_write_does_not_panic_on_malformed_guest_accesses() {
    let mut xhci = XhciController::new();
    let mut mem = SafeMemory::new(0x1000);

    // Include sizes that are commonly seen (1/2/4/8) plus deliberately odd sizes (0/3/5/16).
    const SIZES: [usize; 8] = [0, 1, 2, 3, 4, 5, 8, 16];

    // A small set of boundary offsets to ensure coverage even if the RNG loop count is reduced.
    let mmio = u64::from(XhciController::MMIO_SIZE);
    let boundary_offsets: [u64; 18] = [
        0,
        1,
        2,
        3,
        4,
        7,
        8,
        0x1f,
        0x20,
        mmio.saturating_sub(1),
        mmio,
        mmio.saturating_add(1),
        mmio.saturating_add(0x10),
        mmio.saturating_add(0x100),
        0xffff_ffff,
        u64::MAX,
        u64::MAX - 1,
        u64::MAX - 8,
    ];

    for &offset in &boundary_offsets {
        for &size in &SIZES {
            let value =
                0xA5A5_5A5A_u32 ^ (offset as u32).wrapping_mul(31) ^ (size as u32).wrapping_mul(17);

            let write_res = catch_unwind(AssertUnwindSafe(|| {
                xhci.mmio_write(&mut mem, offset, size, value)
            }));
            assert!(
                write_res.is_ok(),
                "panic in XhciController::mmio_write(offset=0x{offset:x}, size={size}, value=0x{value:x})"
            );

            let read_res =
                catch_unwind(AssertUnwindSafe(|| xhci.mmio_read(&mut mem, offset, size)));
            assert!(
                read_res.is_ok(),
                "panic in XhciController::mmio_read(offset=0x{offset:x}, size={size})"
            );
        }
    }

    // Deterministic "fuzz": fixed-seed pseudo-random read/write traffic.
    let mut rng = DeterministicRng::new(0x4d3c_2b1a_9876_5432);
    for iter in 0..5000u32 {
        let offset = rng.gen_offset(mmio);
        let size = rng.pick_from(&SIZES);
        let value = rng.next_u64() as u32;

        let write_res = catch_unwind(AssertUnwindSafe(|| {
            xhci.mmio_write(&mut mem, offset, size, value)
        }));
        assert!(
            write_res.is_ok(),
            "panic in XhciController::mmio_write iter={iter} offset=0x{offset:x} size={size} value=0x{value:x}"
        );

        let read_res = catch_unwind(AssertUnwindSafe(|| xhci.mmio_read(&mut mem, offset, size)));
        assert!(
            read_res.is_ok(),
            "panic in XhciController::mmio_read iter={iter} offset=0x{offset:x} size={size}"
        );
    }
}

#[test]
fn xhci_command_ring_link_loop_is_bounded_by_step_budget() {
    // Malformed ring: a Link TRB that points to itself with a matching cycle bit.
    //
    // A naive ring walker could loop forever. `RingCursor` must terminate once it has exhausted its
    // step budget.
    let mut mem = TestMemory::new(0x10_000);
    let ring_base: u64 = 0x1000;

    let mut link = Trb::default();
    link.parameter = ring_base;
    link.set_cycle(true);
    link.set_trb_type(TrbType::Link);
    link.set_link_toggle_cycle(false);
    link.write_to(&mut mem, ring_base);

    let mut cur = RingCursor::new(ring_base, true);
    assert_eq!(
        cur.poll(&mut mem, 8),
        RingPoll::Err(RingError::StepBudgetExceeded)
    );
    assert_eq!(cur.dequeue_ptr(), ring_base);
    assert_eq!(cur.cycle_state(), true);
}
