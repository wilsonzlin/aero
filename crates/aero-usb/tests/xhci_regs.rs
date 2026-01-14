use aero_usb::xhci::XhciController;

// Capability register offsets.
const REG_CAPLENGTH: u64 = 0x00;
const REG_HCIVERSION: u64 = 0x02;
const REG_DBOFF: u64 = 0x14;
const REG_RTSOFF: u64 = 0x18;

// Operational register offsets relative to OP base (CAPLENGTH).
const OP_USBCMD: u64 = 0x00;
const OP_USBSTS: u64 = 0x04;
const OP_CRCR: u64 = 0x18;

// Bits.
const USBCMD_RS: u32 = 1 << 0;
const USBCMD_HCRST: u32 = 1 << 1;
const USBSTS_HCH: u32 = 1 << 0;

#[test]
fn caplength_and_hciversion_are_stable() {
    let mut xhci = XhciController::new();

    let caplength0 = xhci.mmio_read(REG_CAPLENGTH, 1) as u8;
    let hciversion0 = xhci.mmio_read(REG_HCIVERSION, 2) as u16;

    // Read again; values must be stable and deterministic.
    assert_eq!(caplength0, xhci.mmio_read(REG_CAPLENGTH, 1) as u8);
    assert_eq!(hciversion0, xhci.mmio_read(REG_HCIVERSION, 2) as u16);

    assert!(
        caplength0 >= 0x20,
        "CAPLENGTH must leave space for base cap regs"
    );
    assert_ne!(hciversion0, 0, "HCIVERSION must be non-zero");
}

#[test]
fn dboff_and_rtsoff_within_mmio_window() {
    let mut xhci = XhciController::new();

    let dboff = xhci.mmio_read(REG_DBOFF, 4) as u32;
    let rtsoff = xhci.mmio_read(REG_RTSOFF, 4) as u32;

    assert!(
        (dboff as u64) < u64::from(XhciController::MMIO_SIZE),
        "DBOFF ({dboff:#x}) must point inside MMIO window"
    );
    assert!(
        (rtsoff as u64) < u64::from(XhciController::MMIO_SIZE),
        "RTSOFF ({rtsoff:#x}) must point inside MMIO window"
    );
}

#[test]
fn usbcmd_usbsts_run_stop_reset() {
    let mut xhci = XhciController::new();

    let caplength = xhci.mmio_read(REG_CAPLENGTH, 1) as u64;
    let op_base = caplength;

    let usbcmd = op_base + OP_USBCMD;
    let usbsts = op_base + OP_USBSTS;
    let crcr = op_base + OP_CRCR;

    // Fresh controller should come up halted.
    assert_ne!(xhci.mmio_read(usbsts, 4) as u32 & USBSTS_HCH, 0);

    // Run.
    xhci.mmio_write(usbcmd, 4, USBCMD_RS as u64);
    assert_eq!(xhci.mmio_read(usbsts, 4) as u32 & USBSTS_HCH, 0);

    // Stop.
    xhci.mmio_write(usbcmd, 4, 0);
    assert_ne!(xhci.mmio_read(usbsts, 4) as u32 & USBSTS_HCH, 0);

    // Program CRCR, then issue HCRST and ensure it clears controller state.
    xhci.mmio_write(crcr, 8, 0x1234_5000 | 0x1);
    assert_ne!(xhci.mmio_read(crcr, 8), 0);

    xhci.mmio_write(usbcmd, 4, USBCMD_HCRST as u64);

    let cmd_after = xhci.mmio_read(usbcmd, 4) as u32;
    assert_eq!(cmd_after & USBCMD_HCRST, 0, "HCRST should be self-clearing");
    assert_eq!(
        cmd_after & USBCMD_RS,
        0,
        "HCRST should leave controller stopped"
    );
    assert_ne!(
        xhci.mmio_read(usbsts, 4) as u32 & USBSTS_HCH,
        0,
        "Controller should be halted after reset"
    );
    assert_eq!(xhci.mmio_read(crcr, 8), 0, "HCRST should clear CRCR");
}

#[test]
fn unsupported_offsets_read_zero() {
    let mut xhci = XhciController::new();

    // Pick an offset in the reserved capability space (HCCPARAMS2, unimplemented by the model).
    let v = xhci.mmio_read(0x1c, 4) as u32;
    assert_eq!(v, 0);
}
