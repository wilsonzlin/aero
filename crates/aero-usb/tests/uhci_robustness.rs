use aero_usb::uhci::regs;
use aero_usb::uhci::UhciController;
use aero_usb::MemoryBus;

const FRAME_LIST_BASE: u32 = 0x1000;
const QH_ADDR: u32 = 0x2000;
const TD_ADDR: u32 = 0x3000;

// UHCI link pointer bits.
const LINK_PTR_T: u32 = 1 << 0;
const LINK_PTR_Q: u32 = 1 << 1;

struct TestMem {
    data: Vec<u8>,
}

impl TestMem {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn write_u32(&mut self, addr: u32, value: u32) {
        let addr = addr as usize;
        self.data[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
    }
}

impl MemoryBus for TestMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let addr = paddr as usize;
        buf.copy_from_slice(&self.data[addr..addr + buf.len()]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let addr = paddr as usize;
        self.data[addr..addr + buf.len()].copy_from_slice(buf);
    }
}

#[test]
fn uhci_zero_frame_list_entry_does_not_hang() {
    let mut ctrl = UhciController::new();
    let mut mem = TestMem::new(0x4000);

    // Frame list is all zeros by default: entry 0 is a non-terminated link pointer to address 0,
    // which would spin forever without treating addr=0 as terminated.
    ctrl.io_write(regs::REG_FLBASEADD, 4, FRAME_LIST_BASE);
    ctrl.io_write(
        regs::REG_USBCMD,
        2,
        (regs::USBCMD_RS | regs::USBCMD_CF | regs::USBCMD_MAXP) as u32,
    );

    let fr0 = ctrl.io_read(regs::REG_FRNUM, 2) as u16;
    ctrl.tick_1ms(&mut mem);
    let fr1 = ctrl.io_read(regs::REG_FRNUM, 2) as u16;
    assert_eq!(fr1, (fr0 + 1) & 0x07ff);

    let sts = ctrl.io_read(regs::REG_USBSTS, 2) as u16;
    assert_eq!(sts & (regs::USBSTS_USBERRINT | regs::USBSTS_HSE), 0);
}

#[test]
fn uhci_qh_horizontal_self_loop_sets_hse_and_errint_and_clears_on_w1c() {
    let mut ctrl = UhciController::new();
    let mut mem = TestMem::new(0x4000);

    ctrl.io_write(regs::REG_FLBASEADD, 4, FRAME_LIST_BASE);
    mem.write_u32(FRAME_LIST_BASE, QH_ADDR | LINK_PTR_Q);

    // QH horizontal link points back to itself (cycle); element is terminated.
    mem.write_u32(QH_ADDR, QH_ADDR | LINK_PTR_Q);
    mem.write_u32(QH_ADDR + 4, LINK_PTR_T);

    ctrl.io_write(regs::REG_USBINTR, 2, regs::USBINTR_TIMEOUT_CRC as u32);
    ctrl.io_write(regs::REG_USBCMD, 2, regs::USBCMD_RS as u32);

    ctrl.tick_1ms(&mut mem);

    let sts = ctrl.io_read(regs::REG_USBSTS, 2) as u16;
    assert_ne!(sts & regs::USBSTS_USBERRINT, 0);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert!(ctrl.irq_level());

    // USBSTS is write-1-to-clear.
    ctrl.io_write(
        regs::REG_USBSTS,
        2,
        (regs::USBSTS_USBERRINT | regs::USBSTS_HSE) as u32,
    );
    let sts2 = ctrl.io_read(regs::REG_USBSTS, 2) as u16;
    assert_eq!(sts2 & (regs::USBSTS_USBERRINT | regs::USBSTS_HSE), 0);
    assert!(!ctrl.irq_level());
}

#[test]
fn uhci_td_self_loop_sets_hse_and_errint() {
    let mut ctrl = UhciController::new();
    let mut mem = TestMem::new(0x4000);

    ctrl.io_write(regs::REG_FLBASEADD, 4, FRAME_LIST_BASE);
    mem.write_u32(FRAME_LIST_BASE, TD_ADDR);

    // TD next pointer points back to itself; status is inactive (0).
    mem.write_u32(TD_ADDR, TD_ADDR);

    ctrl.io_write(regs::REG_USBINTR, 2, regs::USBINTR_TIMEOUT_CRC as u32);
    ctrl.io_write(regs::REG_USBCMD, 2, regs::USBCMD_RS as u32);

    ctrl.tick_1ms(&mut mem);

    let sts = ctrl.io_read(regs::REG_USBSTS, 2) as u16;
    assert_ne!(sts & regs::USBSTS_USBERRINT, 0);
    assert_ne!(sts & regs::USBSTS_HSE, 0);
    assert!(ctrl.irq_level());
}
