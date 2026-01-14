#![allow(dead_code)]

use aero_usb::uhci::regs;
use aero_usb::MemoryBus;

pub const REG_USBCMD: u16 = regs::REG_USBCMD;
pub const REG_USBINTR: u16 = regs::REG_USBINTR;
pub const REG_FRBASEADD: u16 = regs::REG_FLBASEADD;
pub const REG_PORTSC1: u16 = regs::REG_PORTSC1;
pub const REG_PORTSC2: u16 = regs::REG_PORTSC2;

pub const USBCMD_RUN: u16 = regs::USBCMD_RS;
pub const USBINTR_IOC: u16 = regs::USBINTR_IOC;

// Root hub PORTSC bits (UHCI spec).
pub const PORTSC_PR: u16 = 1 << 9;

/// Convenience helper used by xHCI integration tests.
///
/// xHCI transfer execution is gated on `USBCMD.RUN`. This helper sets the bit using
/// fully-qualified register constants so individual tests don't need to import `aero_usb::xhci::regs`
/// (avoids "missing import" vs "unused import" churn under `-D warnings`).
pub fn xhci_set_run(ctrl: &mut aero_usb::xhci::XhciController) {
    ctrl.mmio_write(
        aero_usb::xhci::regs::REG_USBCMD,
        4,
        u64::from(aero_usb::xhci::regs::USBCMD_RUN),
    );
}

// UHCI link pointer bits.
pub const LINK_PTR_T: u32 = 1 << 0;
pub const LINK_PTR_Q: u32 = 1 << 1;

// UHCI TD control/status bits.
pub const TD_CTRL_ACTIVE: u32 = 1 << 23;
pub const TD_CTRL_IOC: u32 = 1 << 24;
#[allow(dead_code)]
pub const TD_CTRL_NAK: u32 = 1 << 19;
#[allow(dead_code)]
pub const TD_CTRL_STALLED: u32 = 1 << 22;
pub const TD_CTRL_ACTLEN_MASK: u32 = 0x7FF;

const TD_TOKEN_DEVADDR_SHIFT: u32 = 8;
const TD_TOKEN_ENDPT_SHIFT: u32 = 15;
const TD_TOKEN_D: u32 = 1 << 19;
const TD_TOKEN_MAXLEN_SHIFT: u32 = 21;

pub struct TestMemory {
    pub data: Vec<u8>,
}

impl TestMemory {
    pub fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    pub fn read(&self, addr: u32, buf: &mut [u8]) {
        let addr = addr as usize;
        buf.copy_from_slice(&self.data[addr..addr + buf.len()]);
    }

    pub fn write(&mut self, addr: u32, buf: &[u8]) {
        let addr = addr as usize;
        self.data[addr..addr + buf.len()].copy_from_slice(buf);
    }

    pub fn read_u32(&self, addr: u32) -> u32 {
        let addr = addr as usize;
        u32::from_le_bytes(self.data[addr..addr + 4].try_into().unwrap())
    }

    pub fn write_u32(&mut self, addr: u32, value: u32) {
        self.write(addr, &value.to_le_bytes());
    }
}

impl MemoryBus for TestMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let addr = paddr as usize;
        buf.copy_from_slice(&self.data[addr..addr + buf.len()]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let addr = paddr as usize;
        self.data[addr..addr + buf.len()].copy_from_slice(buf);
    }
}

#[derive(Default)]
pub struct Alloc {
    next: u32,
}

impl Alloc {
    pub fn new(base: u32) -> Self {
        Self { next: base }
    }

    pub fn alloc(&mut self, size: u32, align: u32) -> u32 {
        let align = align.max(1);
        let mask = align - 1;
        let aligned = (self.next + mask) & !mask;
        self.next = aligned + size;
        aligned
    }
}

pub fn td_token(pid: u8, addr: u8, ep: u8, toggle: bool, max_len: usize) -> u32 {
    let max_len_field = if max_len == 0 {
        0x7FFu32
    } else {
        (max_len as u32).saturating_sub(1)
    };
    (pid as u32)
        | ((addr as u32) << TD_TOKEN_DEVADDR_SHIFT)
        | ((ep as u32) << TD_TOKEN_ENDPT_SHIFT)
        | (if toggle { TD_TOKEN_D } else { 0 })
        | (max_len_field << TD_TOKEN_MAXLEN_SHIFT)
}

pub fn td_ctrl(active: bool, ioc: bool) -> u32 {
    let mut v = 0x7FF;
    if active {
        v |= TD_CTRL_ACTIVE;
    }
    if ioc {
        v |= TD_CTRL_IOC;
    }
    v
}

pub fn actlen(ctrl_sts: u32) -> usize {
    let field = ctrl_sts & TD_CTRL_ACTLEN_MASK;
    if field == 0x7FF {
        0
    } else {
        (field as usize) + 1
    }
}

pub fn install_frame_list(mem: &mut TestMemory, fl_base: u32, qh_addr: u32) {
    for i in 0..1024u32 {
        mem.write_u32(fl_base + i * 4, qh_addr | LINK_PTR_Q);
    }
}

pub fn write_qh(mem: &mut TestMemory, addr: u32, head: u32, element: u32) {
    mem.write_u32(addr, head);
    mem.write_u32(addr + 4, element);
}

pub fn write_td(
    mem: &mut TestMemory,
    addr: u32,
    link_ptr: u32,
    ctrl_sts: u32,
    token: u32,
    buffer: u32,
) {
    mem.write_u32(addr, link_ptr);
    mem.write_u32(addr + 4, ctrl_sts);
    mem.write_u32(addr + 8, token);
    mem.write_u32(addr + 12, buffer);
}
