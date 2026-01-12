use emulator::io::pci::PciDevice;
use emulator::io::usb::hid::keyboard::UsbHidKeyboardHandle;
use emulator::io::usb::hub::UsbHubDevice;
use emulator::io::usb::uhci::regs::*;
use emulator::io::usb::uhci::{UhciController, UhciPciDevice};
use emulator::io::usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult};
use emulator::io::PortIO;
use memory::{Bus, MemoryBus};
use std::cell::RefCell;
use std::rc::Rc;

const FRAME_LIST_BASE: u32 = 0x1000;
const QH_ADDR: u32 = 0x2000;
const TD0: u32 = 0x3000;

const PID_IN: u8 = 0x69;

const TD_STATUS_ACTIVE: u32 = 1 << 23;
const TD_CTRL_IOC: u32 = 1 << 24;

const PCI_COMMAND_IO: u32 = 1 << 0;
const PCI_COMMAND_BME: u32 = 1 << 2;

fn new_test_uhci(io_base: u16) -> UhciPciDevice {
    let mut uhci = UhciPciDevice::new(UhciController::new(), io_base);
    // The UHCI PortIO wrapper models PCI COMMAND gating (IO decoding + bus mastering). Real guests
    // enable these bits before programming the controller; the unit tests should as well.
    uhci.config_write(0x04, 2, PCI_COMMAND_IO | PCI_COMMAND_BME);
    uhci
}

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

fn control_no_data(dev: &mut emulator::io::usb::core::AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(matches!(dev.handle_in(0, 0), UsbInResult::Data(d) if d.is_empty()));
}

fn init_frame_list(mem: &mut Bus, qh_addr: u32) {
    for i in 0..1024u32 {
        // QH pointer (bit1) to `qh_addr`.
        mem.write_u32((FRAME_LIST_BASE + i * 4) as u64, qh_addr | 0x2);
    }
}

#[test]
fn uhci_usbcmd_roundtrips_extended_bits_and_halted_tracks_rs() {
    let mut uhci = new_test_uhci(0);

    // Reset state: halted until RS is set.
    assert_ne!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_HCHALTED, 0);

    // Writes should preserve common driver bits (MAXP/CF) and ignore unknown bits.
    uhci.port_write(REG_USBCMD, 2, (USBCMD_CF | USBCMD_MAXP | 0xff00) as u32);
    assert_eq!(
        uhci.port_read(REG_USBCMD, 2) as u16,
        USBCMD_CF | USBCMD_MAXP
    );
    assert_ne!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_HCHALTED, 0);

    // Setting RS clears HALTED.
    uhci.port_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_CF | USBCMD_MAXP) as u32);
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
    assert_ne!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_HCHALTED, 0);
}

#[test]
fn uhci_hcreset_restores_default_register_state() {
    let mut uhci = new_test_uhci(0);

    uhci.port_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_CF | USBCMD_MAXP) as u32);
    uhci.port_write(REG_USBINTR, 2, 0x0f);
    uhci.port_write(REG_FRNUM, 2, 0x0555);
    uhci.port_write(REG_FLBASEADD, 4, 0x1234_5000);
    uhci.port_write(REG_SOFMOD, 1, 12);

    // Host controller reset is write-1-to-reset and self-clears in USBCMD.
    uhci.port_write(REG_USBCMD, 2, USBCMD_HCRESET as u32);

    assert_eq!(uhci.port_read(REG_USBCMD, 2) as u16, USBCMD_MAXP);
    assert_ne!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_HCHALTED, 0);
    assert_eq!(uhci.port_read(REG_USBINTR, 2) as u16, 0);
    assert_eq!(uhci.port_read(REG_FRNUM, 2) as u16, 0);
    assert_eq!(uhci.port_read(REG_FLBASEADD, 4), 0);
    assert_eq!(uhci.port_read(REG_SOFMOD, 1) as u8, 64);
}

#[test]
fn uhci_global_reset_resets_state_and_latches_greset_until_cleared() {
    let mut uhci = new_test_uhci(0);

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
    assert_ne!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_HCHALTED, 0);
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

    let mut uhci = new_test_uhci(0);
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
fn uhci_usbint_sets_even_when_interrupts_disabled() {
    #[derive(Clone)]
    struct DummyInDevice;

    impl UsbDeviceModel for DummyInDevice {
        fn handle_control_request(
            &mut self,
            _setup: SetupPacket,
            _data_stage: Option<&[u8]>,
        ) -> ControlResponse {
            ControlResponse::Stall
        }

        fn handle_in_transfer(&mut self, _ep: u8, _max_len: usize) -> UsbInResult {
            UsbInResult::Data(vec![0xaa])
        }
    }

    const BUF: u32 = 0x4000;

    let mut mem = Bus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    write_td(
        &mut mem,
        TD0,
        1,
        TD_STATUS_ACTIVE | TD_CTRL_IOC,
        td_token(PID_IN, 0, 1, 0, 1),
        BUF,
    );
    write_qh(&mut mem, QH_ADDR, TD0);

    let mut uhci = new_test_uhci(0);
    uhci.controller.hub_mut().attach(0, Box::new(DummyInDevice));
    uhci.controller.hub_mut().force_enable_for_tests(0);

    uhci.port_write(REG_FLBASEADD, 4, FRAME_LIST_BASE);
    uhci.port_write(REG_USBINTR, 2, 0);
    uhci.port_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_MAXP) as u32);

    uhci.tick_1ms(&mut mem);

    assert_ne!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_USBINT, 0);
    assert!(!uhci.irq_level());

    // Enabling IOC interrupts after the fact should raise IRQ immediately (level-triggered).
    uhci.port_write(REG_USBINTR, 2, USBINTR_IOC as u32);
    assert!(uhci.irq_level());
}

#[test]
fn uhci_ioc_error_sets_usbint_and_can_irq() {
    let mut mem = Bus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    // IOC IN TD to an address with no device attached: will error, but IOC should still latch USBINT.
    write_td(
        &mut mem,
        TD0,
        1,
        TD_STATUS_ACTIVE | TD_CTRL_IOC,
        td_token(PID_IN, 5, 1, 0, 8),
        0,
    );
    write_qh(&mut mem, QH_ADDR, TD0);

    let mut uhci = new_test_uhci(0);
    uhci.port_write(REG_FLBASEADD, 4, FRAME_LIST_BASE);
    uhci.port_write(REG_USBINTR, 2, 0);
    uhci.port_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_MAXP) as u32);

    uhci.tick_1ms(&mut mem);

    let sts = uhci.port_read(REG_USBSTS, 2) as u16;
    assert_ne!(sts & USBSTS_USBERRINT, 0);
    assert_ne!(sts & USBSTS_USBINT, 0);
    assert!(!uhci.irq_level());

    // Enabling IOC interrupts after the fact should raise IRQ, even though the TD errored.
    uhci.port_write(REG_USBINTR, 2, USBINTR_IOC as u32);
    assert!(uhci.irq_level());
}

#[test]
fn uhci_usberrint_sets_even_when_interrupts_disabled() {
    let mut mem = Bus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    // IN TD to a missing device (no IOC): should set USBERRINT but not USBINT.
    write_td(
        &mut mem,
        TD0,
        1,
        TD_STATUS_ACTIVE,
        td_token(PID_IN, 5, 1, 0, 8),
        0,
    );
    write_qh(&mut mem, QH_ADDR, TD0);

    let mut uhci = new_test_uhci(0);
    uhci.port_write(REG_FLBASEADD, 4, FRAME_LIST_BASE);
    uhci.port_write(REG_USBINTR, 2, 0);
    uhci.port_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_MAXP) as u32);

    uhci.tick_1ms(&mut mem);

    let sts = uhci.port_read(REG_USBSTS, 2) as u16;
    assert_ne!(sts & USBSTS_USBERRINT, 0);
    assert_eq!(sts & USBSTS_USBINT, 0);
    assert!(!uhci.irq_level());

    // Enabling error interrupts after the fact should raise IRQ (level-triggered).
    uhci.port_write(REG_USBINTR, 2, USBINTR_TIMEOUT_CRC as u32);
    assert!(uhci.irq_level());

    // Clearing the status should drop IRQ again.
    uhci.port_write(REG_USBSTS, 2, USBSTS_USBERRINT as u32);
    assert!(!uhci.irq_level());
}

#[test]
fn uhci_short_packet_does_not_irq_when_short_interrupt_disabled() {
    #[derive(Clone)]
    struct ShortInDevice;

    impl UsbDeviceModel for ShortInDevice {
        fn handle_control_request(
            &mut self,
            _setup: SetupPacket,
            _data_stage: Option<&[u8]>,
        ) -> ControlResponse {
            ControlResponse::Stall
        }

        fn handle_in_transfer(&mut self, _ep: u8, _max_len: usize) -> UsbInResult {
            // Always return a short packet.
            UsbInResult::Data(vec![0xaa, 0xbb])
        }
    }

    const BUF: u32 = 0x4000;
    const TD_CTRL_SPD: u32 = 1 << 29;

    let mut mem = Bus::new(0x20000);
    init_frame_list(&mut mem, QH_ADDR);

    write_td(
        &mut mem,
        TD0,
        1,
        TD_STATUS_ACTIVE | TD_CTRL_SPD,
        td_token(PID_IN, 0, 1, 0, 8),
        BUF,
    );
    write_qh(&mut mem, QH_ADDR, TD0);

    let mut uhci = new_test_uhci(0);
    uhci.controller.hub_mut().attach(0, Box::new(ShortInDevice));
    uhci.controller.hub_mut().force_enable_for_tests(0);

    uhci.port_write(REG_FLBASEADD, 4, FRAME_LIST_BASE);
    // Enable IOC interrupts only; short-packet interrupts disabled.
    uhci.port_write(REG_USBINTR, 2, USBINTR_IOC as u32);
    uhci.port_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_MAXP) as u32);

    uhci.tick_1ms(&mut mem);

    // Status bit still latches.
    assert_ne!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_USBINT, 0);
    // But IRQ should not assert because only a short-packet event occurred.
    assert!(!uhci.irq_level());

    // Enabling short-packet interrupts afterwards should cause the pending USBINT to assert IRQ.
    uhci.port_write(REG_USBINTR, 2, (USBINTR_IOC | USBINTR_SHORT_PACKET) as u32);
    assert!(uhci.irq_level());
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

    let mut uhci = new_test_uhci(0);
    uhci.port_write(REG_FLBASEADD, 4, FRAME_LIST_BASE);
    uhci.port_write(REG_USBINTR, 2, USBINTR_TIMEOUT_CRC as u32);
    uhci.port_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_MAXP) as u32);

    uhci.tick_1ms(&mut mem);

    assert_ne!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_USBERRINT, 0);

    // Writing the USBSTS high byte should not clear low-byte W1C bits.
    uhci.port_write(REG_USBSTS + 1, 1, 0xff);
    assert_ne!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_USBERRINT, 0);

    // Clear via an 8-bit W1C write to the low byte.
    uhci.port_write(REG_USBSTS, 1, USBSTS_USBERRINT as u32);
    assert_eq!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_USBERRINT, 0);
}

#[test]
fn uhci_pci_bar_relocation_updates_io_window() {
    let mut uhci = new_test_uhci(0x1000);

    uhci.port_write(0x1000 + REG_USBCMD, 2, USBCMD_CF as u32);
    assert_eq!(uhci.port_read(0x1000 + REG_USBCMD, 2) as u16, USBCMD_CF);

    // Move BAR4 to 0x2000.
    uhci.config_write(0x20, 4, 0x2001);
    assert_eq!(uhci.config_read(0x20, 4), 0x2001);

    // Old window is now unmapped.
    assert_eq!(uhci.port_read(0x1000 + REG_USBCMD, 2), u32::from(u16::MAX));

    // New window works.
    uhci.port_write(0x2000 + REG_USBCMD, 2, (USBCMD_CF | USBCMD_MAXP) as u32);
    assert_eq!(
        uhci.port_read(0x2000 + REG_USBCMD, 2) as u16,
        USBCMD_CF | USBCMD_MAXP
    );
}

#[test]
fn uhci_register_block_supports_byte_accesses() {
    let mut uhci = new_test_uhci(0);

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
    assert_eq!(uhci.port_read(REG_USBINTR, 2) as u16, USBINTR_TIMEOUT_CRC);
}

#[test]
fn uhci_register_block_supports_dword_accesses() {
    let mut uhci = new_test_uhci(0);

    uhci.port_write(REG_USBCMD, 2, (USBCMD_CF | USBCMD_MAXP) as u32);
    uhci.controller.set_usbsts_bits(USBSTS_USBERRINT);

    // A 32-bit read from offset 0 must return USBCMD in the low half and USBSTS in the high half.
    let v = uhci.port_read(REG_USBCMD, 4);
    assert_eq!(v as u16, USBCMD_CF | USBCMD_MAXP);
    assert_ne!(((v >> 16) as u16) & USBSTS_USBERRINT, 0);

    // A 32-bit write should behave like the corresponding byte/word writes.
    let w = ((USBSTS_USBERRINT as u32) << 16) | (USBCMD_CF | USBCMD_MAXP) as u32;
    uhci.port_write(REG_USBCMD, 4, w);
    assert_eq!(uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_USBERRINT, 0);
    assert_eq!(
        uhci.port_read(REG_USBCMD, 2) as u16,
        USBCMD_CF | USBCMD_MAXP
    );
}

#[test]
fn uhci_reserved_register_bytes_read_as_zero() {
    let uhci = new_test_uhci(0);

    // SOFMOD is an 8-bit register at 0x0C. Real drivers may use 16/32-bit I/O; reserved bytes in
    // the decoded 0x20-byte UHCI register window should read back as 0 so wide reads don't return
    // spurious 0xFF in the upper bytes.
    assert_eq!(uhci.port_read(REG_SOFMOD, 2) as u16, 0x0040);
    assert_eq!(uhci.port_read(REG_SOFMOD, 4), 0x0000_0040);
}

#[test]
fn uhci_portsc_high_byte_write_does_not_clear_change_bits() {
    let mut uhci = new_test_uhci(0);
    let keyboard = UsbHidKeyboardHandle::new();
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(keyboard.clone()));
    uhci.controller.hub_mut().force_enable_for_tests(0);

    const PORTSC_CSC: u16 = 1 << 1;
    const PORTSC_PEDC: u16 = 1 << 3;
    const PORTSC_SUSP: u16 = 1 << 12;

    let before = uhci.port_read(REG_PORTSC1, 2) as u16;
    assert_ne!(before & PORTSC_CSC, 0);
    assert_ne!(before & PORTSC_PEDC, 0);

    // SUSP is bit12, i.e. bit4 of the high byte.
    uhci.port_write(REG_PORTSC1 + 1, 1, 0x10);

    let after = uhci.port_read(REG_PORTSC1, 2) as u16;
    assert_ne!(after & PORTSC_SUSP, 0);
    // High-byte writes must not clear low-byte W1C bits.
    assert_ne!(after & PORTSC_CSC, 0);
    assert_ne!(after & PORTSC_PEDC, 0);
}

#[test]
fn uhci_fgr_latches_resume_detect_and_can_irq() {
    let mut uhci = new_test_uhci(0);

    // Enable resume interrupts.
    uhci.port_write(REG_USBINTR, 2, USBINTR_RESUME as u32);
    assert!(!uhci.irq_level());

    // Raising FGR latches RESUMEDETECT in USBSTS and asserts IRQ.
    uhci.port_write(REG_USBCMD, 2, (USBCMD_MAXP | USBCMD_RS | USBCMD_FGR) as u32);
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
fn uhci_port_resume_detect_latches_resume_sts_and_can_irq() {
    #[derive(Clone)]
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

    let mut mem = Bus::new(0x1000);
    let mut uhci = new_test_uhci(0);
    uhci.controller.hub_mut().attach(0, Box::new(DummyDevice));

    // Enable resume interrupts.
    uhci.port_write(REG_USBINTR, 2, USBINTR_RESUME as u32);
    assert!(!uhci.irq_level());

    // A port-level resume-detect event should latch the global USBSTS bit and assert IRQ.
    uhci.controller.hub_mut().force_resume_detect_for_tests(0);
    uhci.tick_1ms(&mut mem);
    assert_ne!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_RESUMEDETECT,
        0
    );
    assert!(uhci.irq_level());

    // W1C clears the latched status.
    uhci.port_write(REG_USBSTS, 2, USBSTS_RESUMEDETECT as u32);
    assert_eq!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_RESUMEDETECT,
        0
    );
    assert!(!uhci.irq_level());
}

#[test]
fn uhci_suspended_hid_device_can_remote_wake_and_trigger_resume_irq() {
    const PORTSC_PED: u16 = 1 << 2;
    const PORTSC_RD: u16 = 1 << 6;
    const PORTSC_SUSP: u16 = 1 << 12;

    let mut mem = Bus::new(0x1000);
    let mut uhci = new_test_uhci(0);
    let keyboard = UsbHidKeyboardHandle::new();
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(keyboard.clone()));
    uhci.controller.hub_mut().force_enable_for_tests(0);

    // Configure the device and enable remote wakeup.
    {
        let dev = uhci
            .controller
            .hub_mut()
            .device_mut_for_address(0)
            .expect("device should be reachable at address 0");
        control_no_data(
            dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
        );
        control_no_data(
            dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x03, // SET_FEATURE
                w_value: 1,      // DEVICE_REMOTE_WAKEUP
                w_index: 0,
                w_length: 0,
            },
        );
    }

    // Enable resume IRQs and enter port suspend.
    uhci.port_write(REG_USBINTR, 2, USBINTR_RESUME as u32);
    uhci.port_write(REG_PORTSC1, 2, (PORTSC_PED | PORTSC_SUSP) as u32);

    // User input should create a remote wakeup event while suspended.
    keyboard.key_event(4, true); // HID usage 4 = 'a'
    uhci.tick_1ms(&mut mem);

    assert_ne!(uhci.port_read(REG_PORTSC1, 2) as u16 & PORTSC_RD, 0);
    assert_ne!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_RESUMEDETECT,
        0
    );
    assert!(uhci.irq_level());
}

#[test]
fn uhci_remote_wakeup_only_triggers_for_activity_while_suspended() {
    const PORTSC_PED: u16 = 1 << 2;
    const PORTSC_RD: u16 = 1 << 6;
    const PORTSC_SUSP: u16 = 1 << 12;

    let mut mem = Bus::new(0x1000);
    let mut uhci = new_test_uhci(0);
    let keyboard = UsbHidKeyboardHandle::new();
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(keyboard.clone()));
    uhci.controller.hub_mut().force_enable_for_tests(0);

    // Configure the device and enable remote wakeup.
    {
        let dev = uhci
            .controller
            .hub_mut()
            .device_mut_for_address(0)
            .expect("device should be reachable at address 0");
        control_no_data(
            dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
        );
        control_no_data(
            dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x03, // SET_FEATURE
                w_value: 1,      // DEVICE_REMOTE_WAKEUP
                w_index: 0,
                w_length: 0,
            },
        );
    }

    // Generate input *before* suspend; this should not later be interpreted as a remote wake.
    keyboard.key_event(4, true);

    uhci.port_write(REG_USBINTR, 2, USBINTR_RESUME as u32);
    uhci.port_write(REG_PORTSC1, 2, (PORTSC_PED | PORTSC_SUSP) as u32);
    uhci.tick_1ms(&mut mem);

    assert_eq!(uhci.port_read(REG_PORTSC1, 2) as u16 & PORTSC_RD, 0);
    assert_eq!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_RESUMEDETECT,
        0
    );
    assert!(!uhci.irq_level());
}

#[test]
fn uhci_remote_wakeup_propagates_through_external_hub() {
    const PORTSC_PED: u16 = 1 << 2;
    const PORTSC_RD: u16 = 1 << 6;
    const PORTSC_SUSP: u16 = 1 << 12;

    let mut mem = Bus::new(0x1000);
    let mut uhci = new_test_uhci(0);

    let keyboard = UsbHidKeyboardHandle::new();
    let mut hub = UsbHubDevice::new();
    hub.attach(1, Box::new(keyboard.clone()));
    uhci.controller.hub_mut().attach(0, Box::new(hub));
    uhci.controller.hub_mut().force_enable_for_tests(0);

    // Enumerate the hub itself at address 0 -> address 1, then configure it.
    {
        let dev = uhci
            .controller
            .hub_mut()
            .device_mut_for_address(0)
            .expect("hub should be reachable at address 0");
        control_no_data(
            dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x05, // SET_ADDRESS
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
        );
    }
    {
        let dev = uhci
            .controller
            .hub_mut()
            .device_mut_for_address(1)
            .expect("hub should be reachable at address 1");
        control_no_data(
            dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
        );
    }

    // Power + reset hub downstream port 1 to make the keyboard reachable.
    {
        let dev = uhci
            .controller
            .hub_mut()
            .device_mut_for_address(1)
            .expect("hub should be reachable at address 1");
        control_no_data(
            dev,
            SetupPacket {
                bm_request_type: 0x23, // HostToDevice | Class | Other
                b_request: 0x03,       // SET_FEATURE
                w_value: 8,            // PORT_POWER
                w_index: 1,
                w_length: 0,
            },
        );
        control_no_data(
            dev,
            SetupPacket {
                bm_request_type: 0x23, // HostToDevice | Class | Other
                b_request: 0x03,       // SET_FEATURE
                w_value: 4,            // PORT_RESET
                w_index: 1,
                w_length: 0,
            },
        );
    }
    for _ in 0..50 {
        uhci.tick_1ms(&mut mem);
    }

    // Configure the downstream keyboard and enable remote wakeup.
    {
        let dev = uhci
            .controller
            .hub_mut()
            .device_mut_for_address(0)
            .expect("downstream device should be reachable at address 0");
        control_no_data(
            dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x09, // SET_CONFIGURATION
                w_value: 1,
                w_index: 0,
                w_length: 0,
            },
        );
        control_no_data(
            dev,
            SetupPacket {
                bm_request_type: 0x00,
                b_request: 0x03, // SET_FEATURE
                w_value: 1,      // DEVICE_REMOTE_WAKEUP
                w_index: 0,
                w_length: 0,
            },
        );
    }

    // Enable resume IRQs and enter port suspend.
    uhci.port_write(REG_USBINTR, 2, USBINTR_RESUME as u32);
    uhci.port_write(REG_PORTSC1, 2, (PORTSC_PED | PORTSC_SUSP) as u32);

    // User input should create a remote wakeup event while suspended.
    keyboard.key_event(4, true); // HID usage 4 = 'a'
    uhci.tick_1ms(&mut mem);

    assert_ne!(uhci.port_read(REG_PORTSC1, 2) as u16 & PORTSC_RD, 0);
    assert_ne!(
        uhci.port_read(REG_USBSTS, 2) as u16 & USBSTS_RESUMEDETECT,
        0
    );
    assert!(uhci.irq_level());
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

    let mut uhci = new_test_uhci(0);
    uhci.controller.hub_mut().attach(
        0,
        Box::new(ResetCountingDevice {
            resets: resets.clone(),
        }),
    );

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
    let mut uhci = new_test_uhci(0);

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

#[test]
fn uhci_portsc_suspend_resume_bits_roundtrip() {
    #[derive(Clone)]
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

    const PORTSC_PED: u16 = 1 << 2;
    const PORTSC_SUSP: u16 = 1 << 12;
    const PORTSC_RESUME: u16 = 1 << 13;

    let mut uhci = new_test_uhci(0);
    uhci.controller.hub_mut().attach(0, Box::new(DummyDevice));

    // Setting SUSP should latch and be visible on readback.
    uhci.port_write(REG_PORTSC1, 2, (PORTSC_PED | PORTSC_SUSP) as u32);
    assert_ne!(uhci.port_read(REG_PORTSC1, 2) as u16 & PORTSC_SUSP, 0);

    // Setting RESUME should also latch independently.
    uhci.port_write(
        REG_PORTSC1,
        2,
        (PORTSC_PED | PORTSC_SUSP | PORTSC_RESUME) as u32,
    );
    let st = uhci.port_read(REG_PORTSC1, 2) as u16;
    assert_ne!(st & PORTSC_SUSP, 0);
    assert_ne!(st & PORTSC_RESUME, 0);

    // Clearing both bits.
    uhci.port_write(REG_PORTSC1, 2, PORTSC_PED as u32);
    let st = uhci.port_read(REG_PORTSC1, 2) as u16;
    assert_eq!(st & (PORTSC_SUSP | PORTSC_RESUME), 0);
}

#[test]
fn uhci_portsc_suspend_resume_bits_clear_on_detach() {
    #[derive(Clone)]
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

    const PORTSC_PED: u16 = 1 << 2;
    const PORTSC_SUSP: u16 = 1 << 12;
    const PORTSC_RESUME: u16 = 1 << 13;

    let mut uhci = new_test_uhci(0);
    uhci.controller.hub_mut().attach(0, Box::new(DummyDevice));

    uhci.port_write(
        REG_PORTSC1,
        2,
        (PORTSC_PED | PORTSC_SUSP | PORTSC_RESUME) as u32,
    );
    assert_ne!(
        uhci.port_read(REG_PORTSC1, 2) as u16 & (PORTSC_SUSP | PORTSC_RESUME),
        0
    );

    uhci.controller.hub_mut().detach(0);
    assert_eq!(
        uhci.port_read(REG_PORTSC1, 2) as u16 & (PORTSC_SUSP | PORTSC_RESUME),
        0
    );
}

#[test]
fn uhci_greset_clears_portsc_suspend_resume_bits() {
    #[derive(Clone)]
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

    const PORTSC_PED: u16 = 1 << 2;
    const PORTSC_SUSP: u16 = 1 << 12;
    const PORTSC_RESUME: u16 = 1 << 13;

    let mut uhci = new_test_uhci(0);
    uhci.controller.hub_mut().attach(0, Box::new(DummyDevice));

    uhci.port_write(
        REG_PORTSC1,
        2,
        (PORTSC_PED | PORTSC_SUSP | PORTSC_RESUME) as u32,
    );
    assert_ne!(
        uhci.port_read(REG_PORTSC1, 2) as u16 & (PORTSC_SUSP | PORTSC_RESUME),
        0
    );

    // Global reset should clear transient port suspend/resume state.
    uhci.port_write(REG_USBCMD, 2, (USBCMD_GRESET | USBCMD_MAXP) as u32);
    assert_eq!(
        uhci.port_read(REG_PORTSC1, 2) as u16 & (PORTSC_SUSP | PORTSC_RESUME),
        0
    );
}

#[test]
fn uhci_regs_power_on_defaults() {
    let uhci = new_test_uhci(0);

    let usbcmd = uhci.port_read(REG_USBCMD, 2) as u16;
    assert_eq!(usbcmd, USBCMD_MAXP);

    let usbsts = uhci.port_read(REG_USBSTS, 2) as u16;
    assert_eq!(usbsts, USBSTS_HCHALTED);
    assert_eq!(usbsts & !USBSTS_READ_MASK, 0);

    let usbintr = uhci.port_read(REG_USBINTR, 2) as u16;
    assert_eq!(usbintr, 0);

    let frnum = uhci.port_read(REG_FRNUM, 2) as u16;
    assert_eq!(frnum, 0);

    let flbaseadd = uhci.port_read(REG_FLBASEADD, 4);
    assert_eq!(flbaseadd, 0);

    let sofmod = uhci.port_read(REG_SOFMOD, 1) as u8;
    assert_eq!(sofmod, 0x40);
}

#[test]
fn uhci_regs_frnum_flbaseadd_sofmod_masks() {
    let mut uhci = new_test_uhci(0);

    // FRNUM is 11 bits (0..10).
    uhci.port_write(REG_FRNUM, 2, 0xffff);
    assert_eq!(uhci.port_read(REG_FRNUM, 2) as u16, 0x07ff);

    // FLBASEADD is 4KiB aligned.
    uhci.port_write(REG_FLBASEADD, 4, 0x12345);
    assert_eq!(uhci.port_read(REG_FLBASEADD, 4), 0x12000);

    // SOFMOD is read/write.
    uhci.port_write(REG_SOFMOD, 1, 0x12);
    assert_eq!(uhci.port_read(REG_SOFMOD, 1) as u8, 0x12);
}

#[test]
fn uhci_regs_w1c_usb_sts() {
    let mut uhci = new_test_uhci(0);

    uhci.controller.set_usbsts_bits(
        USBSTS_USBINT
            | USBSTS_USBERRINT
            | USBSTS_RESUMEDETECT
            | USBSTS_HOSTSYSERR
            | USBSTS_HCPROCERR,
    );

    let usbsts = uhci.port_read(REG_USBSTS, 2) as u16;
    assert_eq!(
        usbsts,
        USBSTS_HCHALTED
            | USBSTS_USBINT
            | USBSTS_USBERRINT
            | USBSTS_RESUMEDETECT
            | USBSTS_HOSTSYSERR
            | USBSTS_HCPROCERR
    );

    // Writing 1 clears, writing 0 does not. Writes must not affect HCHALTED.
    uhci.port_write(
        REG_USBSTS,
        2,
        u32::from(USBSTS_USBINT | USBSTS_HOSTSYSERR | USBSTS_HCHALTED),
    );

    let usbsts = uhci.port_read(REG_USBSTS, 2) as u16;
    assert_eq!(
        usbsts,
        USBSTS_HCHALTED | USBSTS_USBERRINT | USBSTS_RESUMEDETECT | USBSTS_HCPROCERR
    );

    // All-zero write should be a no-op.
    uhci.port_write(REG_USBSTS, 2, 0);
    assert_eq!(uhci.port_read(REG_USBSTS, 2) as u16, usbsts);

    // Clear remaining W1C bits.
    uhci.port_write(REG_USBSTS, 2, u32::from(USBSTS_W1C_MASK));
    assert_eq!(uhci.port_read(REG_USBSTS, 2) as u16, USBSTS_HCHALTED);
}

#[test]
fn uhci_regs_hcreset_self_clears_and_resets_registers() {
    let mut uhci = new_test_uhci(0);
    let keyboard = UsbHidKeyboardHandle::new();
    uhci.controller
        .hub_mut()
        .attach(0, Box::new(keyboard.clone()));

    let portsc_before = uhci.port_read(REG_PORTSC1, 2) as u16;

    // Dirty a few registers.
    uhci.port_write(
        REG_USBCMD,
        2,
        u32::from(USBCMD_RS | USBCMD_CF | USBCMD_SWDBG | USBCMD_MAXP),
    );
    uhci.port_write(REG_USBINTR, 2, u32::from(USBINTR_MASK));
    uhci.port_write(REG_FRNUM, 2, 0x07ff);
    uhci.port_write(REG_FLBASEADD, 4, 0x12345);
    uhci.port_write(REG_SOFMOD, 1, 0x12);
    uhci.controller
        .set_usbsts_bits(USBSTS_USBINT | USBSTS_USBERRINT | USBSTS_RESUMEDETECT);

    assert!(uhci.irq_level());

    // Trigger a host controller reset.
    uhci.port_write(REG_USBCMD, 2, u32::from(USBCMD_HCRESET));

    // HCRESET is self-clearing and the register block returns to defaults.
    let usbcmd = uhci.port_read(REG_USBCMD, 2) as u16;
    assert_eq!(usbcmd & USBCMD_HCRESET, 0);
    assert_eq!(usbcmd, USBCMD_MAXP);

    assert_eq!(uhci.port_read(REG_USBINTR, 2) as u16, 0);
    assert_eq!(uhci.port_read(REG_FRNUM, 2) as u16, 0);
    assert_eq!(uhci.port_read(REG_FLBASEADD, 4), 0);
    assert_eq!(uhci.port_read(REG_SOFMOD, 1) as u8, 0x40);
    assert_eq!(uhci.port_read(REG_USBSTS, 2) as u16, USBSTS_HCHALTED);
    assert!(!uhci.irq_level());

    // Host controller reset should not detach the root-hub device.
    let portsc_after = uhci.port_read(REG_PORTSC1, 2) as u16;
    assert_eq!(portsc_after, portsc_before);
}

#[test]
fn uhci_regs_irq_gating() {
    let mut uhci = new_test_uhci(0);

    // USBINT gating (IOC/short packet).
    uhci.port_write(REG_USBINTR, 2, 0);
    uhci.port_write(REG_USBSTS, 2, u32::from(USBSTS_W1C_MASK));
    uhci.controller.set_usbsts_bits(USBSTS_USBINT);
    assert!(!uhci.irq_level());

    uhci.port_write(REG_USBINTR, 2, u32::from(USBINTR_IOC));
    assert!(uhci.irq_level());

    uhci.port_write(REG_USBINTR, 2, 0);
    assert!(!uhci.irq_level());

    uhci.port_write(REG_USBINTR, 2, u32::from(USBINTR_SHORT_PACKET));
    assert!(uhci.irq_level());

    uhci.port_write(REG_USBSTS, 2, u32::from(USBSTS_USBINT));
    assert!(!uhci.irq_level());

    // USBERRINT gating.
    uhci.port_write(REG_USBINTR, 2, 0);
    uhci.port_write(REG_USBSTS, 2, u32::from(USBSTS_W1C_MASK));
    uhci.controller.set_usbsts_bits(USBSTS_USBERRINT);
    assert!(!uhci.irq_level());

    uhci.port_write(REG_USBINTR, 2, u32::from(USBINTR_TIMEOUT_CRC));
    assert!(uhci.irq_level());

    uhci.port_write(REG_USBSTS, 2, u32::from(USBSTS_USBERRINT));
    assert!(!uhci.irq_level());

    // RESUMEDETECT gating.
    uhci.port_write(REG_USBINTR, 2, 0);
    uhci.port_write(REG_USBSTS, 2, u32::from(USBSTS_W1C_MASK));
    uhci.controller.set_usbsts_bits(USBSTS_RESUMEDETECT);
    assert!(!uhci.irq_level());

    uhci.port_write(REG_USBINTR, 2, u32::from(USBINTR_RESUME));
    assert!(uhci.irq_level());

    uhci.port_write(REG_USBSTS, 2, u32::from(USBSTS_RESUMEDETECT));
    assert!(!uhci.irq_level());
}
