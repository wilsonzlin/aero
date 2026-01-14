use aero_usb::uhci::regs::{
    REG_FRNUM, REG_USBCMD, REG_USBSTS, USBCMD_EGSM, USBCMD_RS, USBSTS_HCHALTED, USBSTS_USBINT,
    USBSTS_W1C_MASK,
};
use aero_usb::uhci::UhciController;

mod util;

use util::TestMemory;

fn read_u16(uhci: &UhciController, offset: u16) -> u16 {
    uhci.io_read(offset, 2) as u16
}

fn write_u16(uhci: &mut UhciController, offset: u16, value: u16) {
    uhci.io_write(offset, 2, value as u32);
}

#[test]
fn uhci_regs_run_stop_egsm_and_frnum() {
    let mut mem = TestMemory::new(0x1000);
    let mut uhci = UhciController::new();

    // After reset/new, the controller is halted and FRNUM starts at 0.
    assert_ne!(read_u16(&uhci, REG_USBSTS) & USBSTS_HCHALTED, 0);
    assert_eq!(read_u16(&uhci, REG_FRNUM), 0);

    // Setting RS clears HCHALTED.
    let cmd = read_u16(&uhci, REG_USBCMD);
    write_u16(&mut uhci, REG_USBCMD, cmd | USBCMD_RS);
    assert_eq!(read_u16(&uhci, REG_USBSTS) & USBSTS_HCHALTED, 0);

    // While running (RS=1, EGSM=0), ticking advances FRNUM.
    for _ in 0..3 {
        uhci.tick_1ms(&mut mem);
    }
    assert_eq!(read_u16(&uhci, REG_FRNUM), 3);

    // FRNUM is 11-bit and wraps.
    write_u16(&mut uhci, REG_FRNUM, 0x07fe);
    uhci.tick_1ms(&mut mem);
    assert_eq!(read_u16(&uhci, REG_FRNUM), 0x07ff);
    uhci.tick_1ms(&mut mem);
    assert_eq!(read_u16(&uhci, REG_FRNUM), 0);

    // Clearing RS halts the controller and FRNUM stops advancing.
    let cmd = read_u16(&uhci, REG_USBCMD);
    write_u16(&mut uhci, REG_USBCMD, cmd & !USBCMD_RS);
    assert_ne!(read_u16(&uhci, REG_USBSTS) & USBSTS_HCHALTED, 0);
    let frnum = read_u16(&uhci, REG_FRNUM);
    for _ in 0..10 {
        uhci.tick_1ms(&mut mem);
    }
    assert_eq!(read_u16(&uhci, REG_FRNUM), frnum);

    // Global Suspend Mode behaves as not-running in this model: HCHALTED is set and FRNUM does not
    // advance even if RS=1.
    let cmd = read_u16(&uhci, REG_USBCMD);
    write_u16(&mut uhci, REG_USBCMD, cmd | USBCMD_RS);
    assert_eq!(read_u16(&uhci, REG_USBSTS) & USBSTS_HCHALTED, 0);
    uhci.tick_1ms(&mut mem);
    let frnum = read_u16(&uhci, REG_FRNUM);

    let cmd = read_u16(&uhci, REG_USBCMD);
    write_u16(&mut uhci, REG_USBCMD, cmd | USBCMD_EGSM);
    assert_ne!(read_u16(&uhci, REG_USBSTS) & USBSTS_HCHALTED, 0);
    for _ in 0..10 {
        uhci.tick_1ms(&mut mem);
    }
    assert_eq!(read_u16(&uhci, REG_FRNUM), frnum);

    // Clearing EGSM resumes running as long as RS stays set.
    let cmd = read_u16(&uhci, REG_USBCMD);
    write_u16(&mut uhci, REG_USBCMD, cmd & !USBCMD_EGSM);
    assert_eq!(read_u16(&uhci, REG_USBSTS) & USBSTS_HCHALTED, 0);
    uhci.tick_1ms(&mut mem);
    assert_eq!(read_u16(&uhci, REG_FRNUM), (frnum + 1) & 0x07ff);
}

#[test]
fn uhci_usbsts_w1c_does_not_clear_hchalted() {
    let mut uhci = UhciController::new();

    assert_ne!(read_u16(&uhci, REG_USBSTS) & USBSTS_HCHALTED, 0);

    // Force a W1C status bit, then clear it. HCHALTED should remain unaffected.
    uhci.set_usbsts_bits(USBSTS_USBINT);
    let st = read_u16(&uhci, REG_USBSTS);
    assert_ne!(st & USBSTS_USBINT, 0);
    assert_ne!(st & USBSTS_HCHALTED, 0);

    uhci.io_write(REG_USBSTS, 2, USBSTS_W1C_MASK as u32);
    let st = read_u16(&uhci, REG_USBSTS);
    assert_eq!(st & USBSTS_USBINT, 0);
    assert_ne!(st & USBSTS_HCHALTED, 0);

    // And even attempting to clear HCHALTED via USBSTS should have no effect (it's derived).
    uhci.io_write(REG_USBSTS, 2, USBSTS_HCHALTED as u32);
    assert_ne!(read_u16(&uhci, REG_USBSTS) & USBSTS_HCHALTED, 0);
}
