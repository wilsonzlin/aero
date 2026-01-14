use aero_io_snapshot::io::state::IoSnapshot;
use aero_usb::xhci::trb::{Trb, TrbType};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::MemoryBus;

mod util;
use util::TestMemory;

fn write_erst_entry<M: MemoryBus + ?Sized>(
    mem: &mut M,
    erstba: u64,
    seg_base: u64,
    seg_size_trbs: u32,
) {
    MemoryBus::write_u64(mem, erstba, seg_base);
    MemoryBus::write_u32(mem, erstba + 8, seg_size_trbs);
    MemoryBus::write_u32(mem, erstba + 12, 0);
}

#[test]
fn xhci_snapshot_roundtrip_preserves_host_controller_error() {
    let mut mem = TestMemory::new(0x20_000);
    let mut xhci = XhciController::new();

    // Force Host Controller Error (HCE) via an invalid guest event ring configuration that is still
    // fully in-bounds (so the test memory bus doesn't panic):
    // - ERSTSZ=1, ERSTBA points at a valid ERST entry
    // - ERST entry has a zero segment size (invalid per xHCI spec)
    // - ERDP points at the segment base (still in-range)
    let erstba = 0x1000u64;
    let ring_base = 0x2000u64;
    write_erst_entry(&mut mem, erstba, ring_base, 0);

    xhci.mmio_write(regs::REG_INTR0_ERSTSZ, 4, 1);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_LO, 4, erstba);
    xhci.mmio_write(regs::REG_INTR0_ERSTBA_HI, 4, erstba >> 32);
    xhci.mmio_write(regs::REG_INTR0_ERDP_LO, 4, ring_base);
    xhci.mmio_write(regs::REG_INTR0_ERDP_HI, 4, ring_base >> 32);

    let mut evt = Trb::default();
    evt.set_trb_type(TrbType::PortStatusChangeEvent);
    xhci.post_event(evt);
    xhci.service_event_ring(&mut mem);

    let sts = xhci.mmio_read(regs::REG_USBSTS, 4) as u32;
    assert_ne!(sts & regs::USBSTS_HCE, 0, "controller should latch HCE");

    let bytes = xhci.save_state();

    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");

    let sts2 = restored.mmio_read(regs::REG_USBSTS, 4) as u32;
    assert_ne!(
        sts2 & regs::USBSTS_HCE,
        0,
        "host controller error should roundtrip through snapshot"
    );

    // HCE should still only clear via a controller reset.
    restored.mmio_write(regs::REG_USBCMD, 4, u64::from(regs::USBCMD_HCRST));
    let sts3 = restored.mmio_read(regs::REG_USBSTS, 4) as u32;
    assert_eq!(
        sts3 & regs::USBSTS_HCE,
        0,
        "controller reset should clear HCE after restore"
    );
}
