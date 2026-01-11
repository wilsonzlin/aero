use emulator::io::pci::PciDevice;
use emulator::io::usb::{ControlResponse, SetupPacket, UsbDeviceModel};
use emulator::io::usb::uhci::regs::*;
use emulator::io::usb::uhci::{UhciController, UhciPciDevice};
use emulator::io::PortIO;
use memory::{Bus, MemoryBus};
use std::cell::RefCell;
use std::rc::Rc;

const FRAME_LIST_BASE: u32 = 0x1000;
const QH_ADDR: u32 = 0x2000;
const TD0: u32 = 0x3000;

const PID_IN: u8 = 0x69;

const TD_STATUS_ACTIVE: u32 = 1 << 23;

fn td_token(pid: u8, addr: u8, ep: u8, toggle: u8, max_len: usize) -> u32 {
    let max_field = if max_len == 0 {
        0x7ffu32
    } else {
        (max_len as u32) - 1
    };
    (pid as u32)
        | ((addr as u32) << 8)
        | ((ep as u32) << 15)
        | ((toggle as u32) << 19)
        | (max_field << 21)
}

fn write_td(mem: &mut Bus, addr: u32, link: u32, status: u32, token: u32, buffer: u32) {
    mem.write_u32(addr as u64, link);
    mem.write_u32(addr.wrapping_add(4) as u64, status);
    mem.write_u32(addr.wrapping_add(8) as u64, token);
    mem.write_u32(addr.wrapping_add(12) as u64, buffer);
}

fn write_qh(mem: &mut Bus, addr: u32, elem: u32) {
    mem.write_u32(addr as u64, 1); // horizontal terminate
    mem.write_u32(addr.wrapping_add(4) as u64, elem);
}

fn init_frame_list(mem: &mut Bus, qh_addr: u32) {
    for i in 0..1024u32 {
        // QH pointer (bit1) to `qh_addr`.
        mem.write_u32((FRAME_LIST_BASE + i * 4) as u64, qh_addr | 0x2);
    }
}

#[test]
fn uhci_usbcmd_roundtrips_extended_bits_and_halted_tracks_rs() {
    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);

    // Reset state: halted until RS is set.
    assert_ne!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_HCHALTED,
        0
    );

    // Writes should preserve common driver bits (MAXP/CF) and ignore unknown bits.
    uhci.port_write(
        REG_USBCMD,
        2,
        (USBCMD_CF | USBCMD_MAXP | 0xff00) as u32,
    );
    assert_eq!(
        uhci.port_read(REG_USBCMD, 2) as u16,
        USBCMD_CF | USBCMD_MAXP
    );
    assert_ne!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_HCHALTED,
        0
    );

    // Setting RS clears HALTED.
    uhci.port_write(
        REG_USBCMD,
        2,
        (USBCMD_RS | USBCMD_CF | USBCMD_MAXP) as u32,
    );
    assert_eq!(
        uhci.port_read(REG_USBCMD, 2) as u16,
        USBCMD_RS | USBCMD_CF | USBCMD_MAXP
    );
    assert_eq!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_HCHALTED, 0);

    // Clearing RS sets HALTED.
    uhci.port_write(REG_USBCMD, 2, (USBCMD_CF | USBCMD_MAXP) as u32);
    assert_eq!(
        uhci.port_read(REG_USBCMD, 2) as u16,
        USBCMD_CF | USBCMD_MAXP
    );
    assert_ne!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_HCHALTED,
        0
    );
}

#[test]
fn uhci_hcreset_restores_default_register_state() {
    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);

    uhci.port_write(
        REG_USBCMD,
        2,
        (USBCMD_RS | USBCMD_CF | USBCMD_MAXP) as u32,
    );
    uhci.port_write(REG_USBINTR, 2, 0x0f);
    uhci.port_write(REG_FRNUM, 2, 0x0555);
    uhci.port_write(REG_FLBASEADD, 4, 0x1234_5000);
    uhci.port_write(REG_SOFMOD, 1, 12);

    // Host controller reset is write-1-to-reset and self-clears in USBCMD.
    uhci.port_write(REG_USBCMD, 2, USBCMD_HCRESET as u32);

    assert_eq!(uhci.port_read(REG_USBCMD, 2) as u16, USBCMD_MAXP);
    assert_ne!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_HCHALTED,
        0
    );
    assert_eq!(uhci.port_read(REG_USBINTR, 2) as u16, 0);
    assert_eq!(uhci.port_read(REG_FRNUM, 2) as u16, 0);
    assert_eq!(uhci.port_read(REG_FLBASEADD, 4), 0);
    assert_eq!(uhci.port_read(REG_SOFMOD, 1) as u8, 64);
}

#[test]
fn uhci_global_reset_resets_state_and_latches_greset_until_cleared() {
    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);

    uhci.port_write(REG_USBINTR, 2, 0x0f);
    uhci.port_write(REG_FRNUM, 2, 0x0123);
    uhci.port_write(REG_FLBASEADD, 4, 0x1234_5000);

    // GRESET resets controller state, but the GRESET bit itself is latched until software clears it.
    uhci.port_write(
        REG_USBCMD,
        2,
        (USBCMD_GRESET | USBCMD_CF | USBCMD_MAXP) as u32,
    );

    assert_eq!(
        uhci.port_read(REG_USBCMD, 2) as u16,
        USBCMD_GRESET | USBCMD_CF | USBCMD_MAXP
    );
    assert_ne!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_HCHALTED,
        0
    );
    assert_eq!(uhci.port_read(REG_USBINTR, 2) as u16, 0);
    assert_eq!(uhci.port_read(REG_FRNUM, 2) as u16, 0);
    assert_eq!(uhci.port_read(REG_FLBASEADD, 4), 0);

    // Clearing GRESET leaves other writable bits intact.
    uhci.port_write(REG_USBCMD, 2, (USBCMD_CF | USBCMD_MAXP) as u32);
    assert_eq!(
        uhci.port_read(REG_USBCMD, 2) as u16,
        USBCMD_CF | USBCMD_MAXP
    );
}

#[test]
fn uhci_usbsts_write_1_to_clear_clears_latched_interrupt_bits() {
    let mut mem = Bus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    // Schedule a single TD to an address with no device attached to force an error interrupt.
    write_td(
        &mut mem,
        TD0,
        1, // terminate
        TD_STATUS_ACTIVE,
        td_token(PID_IN, 5, 0, 0, 8),
        0,
    );
    write_qh(&mut mem, QH_ADDR, TD0);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    uhci.port_write(REG_FLBASEADD, 4, FRAME_LIST_BASE);
    uhci.port_write(REG_USBINTR, 2, USBINTR_TIMEOUT_CRC as u32);
    uhci.port_write(REG_USBCMD, 2, USBCMD_RS as u32);

    uhci.tick_1ms(&mut mem);

    let sts = uhci.port_read(REG_USBSTS, 2) as u16;
    assert_ne!(sts & USBSTS_USBERRINT, 0);
    assert!(uhci.irq_level());

    // W1C should clear the latched status and drop IRQ.
    uhci.port_write(REG_USBSTS, 2, USBSTS_USBERRINT as u32);
    let sts = uhci.port_read(REG_USBSTS, 2) as u16;
    assert_eq!(sts & USBSTS_USBERRINT, 0);
    assert!(!uhci.irq_level());
}

#[test]
fn uhci_usbsts_byte_writes_do_not_cross_clear_w1c_bits() {
    let mut mem = Bus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    // Schedule a single TD to an address with no device attached to force an error interrupt.
    write_td(
        &mut mem,
        TD0,
        1, // terminate
        TD_STATUS_ACTIVE,
        td_token(PID_IN, 5, 0, 0, 8),
        0,
    );
    write_qh(&mut mem, QH_ADDR, TD0);

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    uhci.port_write(REG_FLBASEADD, 4, FRAME_LIST_BASE);
    uhci.port_write(REG_USBINTR, 2, USBINTR_TIMEOUT_CRC as u32);
    uhci.port_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_MAXP) as u32);

    uhci.tick_1ms(&mut mem);

    assert_ne!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_USBERRINT,
        0
    );

    // Writing the USBSTS high byte should not clear low-byte W1C bits.
    uhci.port_write(REG_USBSTS + 1, 1, 0xff);
    assert_ne!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_USBERRINT,
        0
    );

    // Clear via an 8-bit W1C write to the low byte.
    uhci.port_write(REG_USBSTS, 1, USBSTS_USBERRINT as u32);
    assert_eq!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_USBERRINT,
        0
    );
}

#[test]
fn uhci_pci_bar_relocation_updates_io_window() {
    let mut uhci = UhciPciDevice::new(UhciController::new(), 0x1000);

    uhci.port_write(0x1000 + REG_USBCMD, 2, USBCMD_CF as u32);
    assert_eq!(uhci.port_read(0x1000 + REG_USBCMD, 2) as u16, USBCMD_CF);

    // Move BAR4 to 0x2000.
    uhci.config_write(0x20, 4, 0x2001);
    assert_eq!(uhci.config_read(0x20, 4), 0x2001);

    // Old window is now unmapped.
    assert_eq!(uhci.port_read(0x1000 + REG_USBCMD, 2), u32::MAX);

    // New window works.
    uhci.port_write(0x2000 + REG_USBCMD, 2, (USBCMD_CF | USBCMD_MAXP) as u32);
    assert_eq!(
        uhci.port_read(0x2000 + REG_USBCMD, 2) as u16,
        USBCMD_CF | USBCMD_MAXP
    );
}

#[test]
fn uhci_register_block_supports_byte_accesses() {
    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);

    // Default USBCMD has MAXP set (bit7, low byte).
    assert_eq!(uhci.port_read(REG_USBCMD, 1) as u8, USBCMD_MAXP as u8);
    assert_eq!(uhci.port_read(REG_USBCMD + 1, 1) as u8, 0);

    // Set RS via an 8-bit write (must include MAXP to preserve it since MAXP is in the same byte).
    uhci.port_write(REG_USBCMD, 1, (USBCMD_MAXP | USBCMD_RS) as u32);
    assert_eq!(
        uhci.port_read(REG_USBCMD, 2) as u16 & (USBCMD_MAXP | USBCMD_RS),
        USBCMD_MAXP | USBCMD_RS
    );
    assert_eq!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_HCHALTED, 0);

    // USBINTR is also byte-accessible (low byte only).
    uhci.port_write(REG_USBINTR, 1, USBINTR_TIMEOUT_CRC as u32);
    assert_eq!(
        uhci.port_read(REG_USBINTR, 2) as u16,
        USBINTR_TIMEOUT_CRC
    );
}

#[test]
fn uhci_fgr_latches_resume_detect_and_can_irq() {
    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);

    // Enable resume interrupts.
    uhci.port_write(REG_USBINTR, 2, USBINTR_RESUME as u32);
    assert!(!uhci.irq_level());

    // Raising FGR latches RESUMEDETECT in USBSTS and asserts IRQ.
    uhci.port_write(
        REG_USBCMD,
        2,
        (USBCMD_MAXP | USBCMD_RS | USBCMD_FGR) as u32,
    );
    assert_ne!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_RESUMEDETECT,
        0
    );
    assert!(uhci.irq_level());

    // W1C should clear RESUMEDETECT and drop IRQ.
    uhci.port_write(REG_USBSTS, 2, USBSTS_RESUMEDETECT as u32);
    assert_eq!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_RESUMEDETECT,
        0
    );
    assert!(!uhci.irq_level());
}

#[test]
fn uhci_greset_resets_attached_devices() {
    #[derive(Clone)]
    struct ResetCountingDevice {
        resets: Rc<RefCell<u32>>,
    }

    impl UsbDeviceModel for ResetCountingDevice {
        fn reset(&mut self) {
            *self.resets.borrow_mut() += 1;
        }

        fn handle_control_request(
            &mut self,
            _setup: SetupPacket,
            _data_stage: Option<&[u8]>,
        ) -> ControlResponse {
            ControlResponse::Stall
        }
    }

    let resets = Rc::new(RefCell::new(0));

    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(ResetCountingDevice { resets: resets.clone() }));

    // Assert global reset; model should see a bus reset.
    uhci.port_write(REG_USBCMD, 2, (USBCMD_GRESET | USBCMD_MAXP) as u32);
    assert_eq!(*resets.borrow(), 1);

    // Re-writing GRESET while still asserted should not retrigger the reset.
    uhci.port_write(REG_USBCMD, 2, (USBCMD_GRESET | USBCMD_MAXP) as u32);
    assert_eq!(*resets.borrow(), 1);

    // Clear GRESET, then re-assert: should retrigger.
    uhci.port_write(REG_USBCMD, 2, USBCMD_MAXP as u32);
    uhci.port_write(REG_USBCMD, 2, (USBCMD_GRESET | USBCMD_MAXP) as u32);
    assert_eq!(*resets.borrow(), 2);
}

#[test]
fn uhci_egsm_suspends_frame_counter() {
    let mut mem = Bus::new(0x1000);
    let mut uhci = UhciPciDevice::new(UhciController::new(), 0);

    // Start the controller.
    uhci.port_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_MAXP) as u32);
    let fr0 = uhci.port_read(REG_FRNUM, 2) as u16;
    uhci.tick_1ms(&mut mem);
    let fr1 = uhci.port_read(REG_FRNUM, 2) as u16;
    assert_eq!(fr1, fr0.wrapping_add(1) & 0x07ff);

    // Enter global suspend mode: controller should stop advancing FRNUM.
    uhci.port_write(
        REG_USBCMD,
        2,
        (USBCMD_RS | USBCMD_MAXP | USBCMD_EGSM) as u32,
    );
    let fr_before = uhci.port_read(REG_FRNUM, 2) as u16;
    for _ in 0..5 {
        uhci.tick_1ms(&mut mem);
    }
    let fr_after = uhci.port_read(REG_FRNUM, 2) as u16;
    assert_eq!(fr_after, fr_before);

    // Resume: FRNUM advances again.
    uhci.port_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_MAXP) as u32);
    uhci.tick_1ms(&mut mem);
    let fr_resume = uhci.port_read(REG_FRNUM, 2) as u16;
    assert_eq!(fr_resume, fr_after.wrapping_add(1) & 0x07ff);
}
