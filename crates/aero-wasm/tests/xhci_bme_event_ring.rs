#![cfg(target_arch = "wasm32")]

use aero_usb::xhci::interrupter::IMAN_IE;
use aero_usb::xhci::trb::{Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController};
use aero_wasm::XhciControllerBridge;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

fn linear_addr(guest_base: u32, paddr: u64, len: usize) -> u32 {
    let end = paddr
        .checked_add(len as u64)
        .expect("guest physical address overflow");
    let guest_base_u64 = u64::from(guest_base);
    let linear = guest_base_u64
        .checked_add(paddr)
        .expect("guest_base + paddr overflow");
    let linear_end = guest_base_u64
        .checked_add(end)
        .expect("guest_base + end overflow");
    assert!(
        linear_end <= u64::from(u32::MAX),
        "linear addr out of u32 range"
    );
    assert!(
        linear_end >= linear,
        "linear addr range should not wrap"
    );
    u32::try_from(linear).expect("linear address should fit in u32")
}

fn write_erst_entry(guest_base: u32, erstba: u64, seg_base: u64, seg_size_trbs: u32) {
    // xHCI ERST entry layout (16 bytes):
    // - DWORD0..1: segment base address
    // - DWORD2: segment size in TRBs
    // - DWORD3: reserved
    let linear = linear_addr(guest_base, erstba, 16);
    unsafe {
        common::write_bytes(linear, &seg_base.to_le_bytes());
        common::write_u32(linear + 8, seg_size_trbs);
        common::write_u32(linear + 12, 0);
    }
}

fn read_bytes(guest_base: u32, paddr: u64, out: &mut [u8]) {
    let linear = linear_addr(guest_base, paddr, out.len());
    unsafe {
        core::ptr::copy_nonoverlapping(linear as *const u8, out.as_mut_ptr(), out.len());
    }
}

#[wasm_bindgen_test]
fn xhci_step_frame_drains_event_ring_when_bme_enabled() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);
    // Sanity check: keep the test addresses within the allocated guest region.
    assert!(
        guest_size as u64 >= 0x4000,
        "guest region too small for xHCI event ring test"
    );

    let mut bridge =
        XhciControllerBridge::new(guest_base, guest_size).expect("new XhciControllerBridge");

    // Configure a simple 1-segment event ring at 0x2000 with an ERST entry at 0x1000.
    let erstba: u64 = 0x1000;
    let ring_base: u64 = 0x2000;
    write_erst_entry(guest_base, erstba, ring_base, 4);

    bridge.mmio_write(regs::REG_INTR0_ERSTSZ as u32, 4, 1);
    bridge.mmio_write(regs::REG_INTR0_ERSTBA_LO as u32, 4, erstba as u32);
    bridge.mmio_write(regs::REG_INTR0_ERSTBA_HI as u32, 4, (erstba >> 32) as u32);
    bridge.mmio_write(regs::REG_INTR0_ERDP_LO as u32, 4, ring_base as u32);
    bridge.mmio_write(regs::REG_INTR0_ERDP_HI as u32, 4, (ring_base >> 32) as u32);
    // Keep interrupter 0 enabled (IMAN.IE). This is the default, but make it explicit for the test.
    bridge.mmio_write(regs::REG_INTR0_IMAN as u32, 4, IMAN_IE);

    // Attaching a device generates a port status change event, but it should *not* be DMA'd into the
    // guest event ring until bus mastering (DMA) is enabled.
    bridge.attach_hub(0, 4).expect("attach hub at root port 0");

    // Event ring starts empty/zeroed.
    let mut trb_bytes = [0u8; TRB_LEN];
    read_bytes(guest_base, ring_base, &mut trb_bytes);
    assert_eq!(trb_bytes, [0u8; TRB_LEN]);
    assert!(
        !bridge.irq_asserted(),
        "IRQ should not assert before event ring is serviced"
    );

    // Step one frame with PCI BME=0 (default). This advances internal port timers but must not DMA.
    bridge.step_frame();
    assert!(
        !bridge.irq_asserted(),
        "IRQ should remain deasserted while BME is disabled"
    );
    read_bytes(guest_base, ring_base, &mut trb_bytes);
    assert_eq!(
        trb_bytes,
        [0u8; TRB_LEN],
        "event ring should remain untouched while BME is disabled"
    );

    // Enable bus mastering and step again: pending events should be delivered into the guest event
    // ring and IRQ should assert.
    bridge.set_pci_command(1 << 2);
    bridge.step_frame();
    assert!(
        bridge.irq_asserted(),
        "IRQ should assert once the event is enqueued into the guest ring"
    );

    read_bytes(guest_base, ring_base, &mut trb_bytes);
    assert_ne!(
        trb_bytes,
        [0u8; TRB_LEN],
        "expected event TRB to be written into guest memory"
    );

    let trb = Trb::from_bytes(trb_bytes);
    assert!(trb.cycle(), "producer cycle bit should be set on initial enqueue");
    assert_eq!(trb.trb_type(), TrbType::PortStatusChangeEvent);
    let port_id = (trb.parameter >> regs::PSC_EVENT_PORT_ID_SHIFT) as u8;
    assert_eq!(port_id, 1, "root port 0 should use Port ID 1 in event TRBs");

    // Keep the test aligned with the public xHCI constants: the MMIO window must remain 64KiB.
    assert_eq!(XhciController::MMIO_SIZE, 0x1_0000);
}

