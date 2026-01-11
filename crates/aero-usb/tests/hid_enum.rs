use aero_usb::hid::keyboard::UsbHidKeyboardHandle;
use aero_usb::uhci::regs::{
    REG_FLBASEADD, REG_PORTSC1, REG_USBCMD, REG_USBINTR, USBCMD_MAXP, USBCMD_RS, USBINTR_IOC,
};
use aero_usb::uhci::UhciController;
use aero_usb::web::keyboard_code_to_hid_usage;
use aero_usb::{MemoryBus, SetupPacket};

const PORTSC_PR: u16 = 1 << 9;

const LINK_PTR_T: u32 = 1 << 0;
const LINK_PTR_Q: u32 = 1 << 1;

const TD_CTRL_ACTIVE: u32 = 1 << 23;
const TD_CTRL_IOC: u32 = 1 << 24;
const TD_CTRL_ACTLEN_MASK: u32 = 0x7FF;

const TD_TOKEN_DEVADDR_SHIFT: u32 = 8;
const TD_TOKEN_ENDPT_SHIFT: u32 = 15;
const TD_TOKEN_D: u32 = 1 << 19;
const TD_TOKEN_MAXLEN_SHIFT: u32 = 21;

struct TestMemory {
    data: Vec<u8>,
}

impl TestMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn read_u32(&mut self, addr: u32) -> u32 {
        MemoryBus::read_u32(self, addr as u64)
    }

    fn write_u32(&mut self, addr: u32, value: u32) {
        MemoryBus::write_u32(self, addr as u64, value);
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
struct Alloc {
    next: u32,
}

impl Alloc {
    fn new(base: u32) -> Self {
        Self { next: base }
    }

    fn alloc(&mut self, size: u32, align: u32) -> u32 {
        let aligned = (self.next + (align - 1)) & !(align - 1);
        self.next = aligned + size;
        aligned
    }
}

fn td_token(pid: u8, addr: u8, ep: u8, toggle: bool, max_len: usize) -> u32 {
    let max_len_field = if max_len == 0 {
        0x7FFu32
    } else {
        (max_len as u32) - 1
    };
    (pid as u32)
        | ((addr as u32) << TD_TOKEN_DEVADDR_SHIFT)
        | ((ep as u32) << TD_TOKEN_ENDPT_SHIFT)
        | (if toggle { TD_TOKEN_D } else { 0 })
        | (max_len_field << TD_TOKEN_MAXLEN_SHIFT)
}

fn td_ctrl(active: bool, ioc: bool) -> u32 {
    let mut v = 0x7FF;
    if active {
        v |= TD_CTRL_ACTIVE;
    }
    if ioc {
        v |= TD_CTRL_IOC;
    }
    v
}

fn actlen(ctrl_sts: u32) -> usize {
    let field = ctrl_sts & TD_CTRL_ACTLEN_MASK;
    if field == 0x7FF {
        0
    } else {
        (field as usize) + 1
    }
}

fn install_frame_list(mem: &mut TestMemory, fl_base: u32, qh_addr: u32) {
    for i in 0..1024u32 {
        mem.write_u32(fl_base + i * 4, qh_addr | LINK_PTR_Q);
    }
}

fn write_qh(mem: &mut TestMemory, addr: u32, head: u32, element: u32) {
    mem.write_u32(addr, head);
    mem.write_u32(addr + 4, element);
}

fn write_td(
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

#[allow(clippy::too_many_arguments)]
fn control_in(
    ctrl: &mut UhciController,
    mem: &mut TestMemory,
    alloc: &mut Alloc,
    fl_base: u32,
    devaddr: u8,
    max_packet: usize,
    setup: SetupPacket,
) -> Vec<u8> {
    let qh_addr = alloc.alloc(0x20, 0x10);
    let setup_buf = alloc.alloc(8, 0x10);
    let setup_td = alloc.alloc(0x20, 0x10);

    let mut bytes = [0u8; 8];
    bytes[0] = setup.bm_request_type;
    bytes[1] = setup.b_request;
    bytes[2..4].copy_from_slice(&setup.w_value.to_le_bytes());
    bytes[4..6].copy_from_slice(&setup.w_index.to_le_bytes());
    bytes[6..8].copy_from_slice(&setup.w_length.to_le_bytes());
    mem.write_physical(setup_buf as u64, &bytes);

    let mut tds = Vec::new();
    tds.push((setup_td, setup_buf, 8usize, 0x2D, false)); // SETUP, DATA0

    let mut remaining = setup.w_length as usize;
    let mut toggle = true;
    while remaining != 0 {
        let chunk = remaining.min(max_packet);
        let buf = alloc.alloc(chunk as u32, 0x10);
        let td = alloc.alloc(0x20, 0x10);
        tds.push((td, buf, chunk, 0x69, toggle)); // IN
        toggle = !toggle;
        remaining -= chunk;
    }

    // Status stage: OUT zero-length, DATA1.
    let status_td = alloc.alloc(0x20, 0x10);
    tds.push((status_td, 0, 0, 0xE1, true));

    for i in 0..tds.len() {
        let (td_addr, buf_addr, len, pid, dtoggle) = tds[i];
        let link = if i + 1 == tds.len() {
            LINK_PTR_T
        } else {
            tds[i + 1].0
        };
        let ioc = i + 1 == tds.len();
        write_td(
            mem,
            td_addr,
            link,
            td_ctrl(true, ioc),
            td_token(pid, devaddr, 0, dtoggle, len),
            buf_addr,
        );
    }

    write_qh(mem, qh_addr, LINK_PTR_T, setup_td);
    install_frame_list(mem, fl_base, qh_addr);

    // One frame is enough for this emulator model (no NAKs during enumeration).
    ctrl.tick_1ms(mem);

    let mut out = Vec::new();
    for (td_addr, buf_addr, _len, pid, _) in tds {
        if pid != 0x69 {
            continue;
        }
        let ctrl_sts = mem.read_u32(td_addr + 4);
        let got = actlen(ctrl_sts);
        let mut tmp = vec![0u8; got];
        mem.read_physical(buf_addr as u64, &mut tmp);
        out.extend_from_slice(&tmp);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn control_no_data(
    ctrl: &mut UhciController,
    mem: &mut TestMemory,
    alloc: &mut Alloc,
    fl_base: u32,
    devaddr: u8,
    setup: SetupPacket,
) {
    let qh_addr = alloc.alloc(0x20, 0x10);
    let setup_buf = alloc.alloc(8, 0x10);
    let setup_td = alloc.alloc(0x20, 0x10);
    let status_td = alloc.alloc(0x20, 0x10);

    let mut bytes = [0u8; 8];
    bytes[0] = setup.bm_request_type;
    bytes[1] = setup.b_request;
    bytes[2..4].copy_from_slice(&setup.w_value.to_le_bytes());
    bytes[4..6].copy_from_slice(&setup.w_index.to_le_bytes());
    bytes[6..8].copy_from_slice(&setup.w_length.to_le_bytes());
    mem.write_physical(setup_buf as u64, &bytes);

    write_td(
        mem,
        setup_td,
        status_td,
        td_ctrl(true, false),
        td_token(0x2D, devaddr, 0, false, 8),
        setup_buf,
    );
    // Status stage: IN zero-length, DATA1.
    write_td(
        mem,
        status_td,
        LINK_PTR_T,
        td_ctrl(true, true),
        td_token(0x69, devaddr, 0, true, 0),
        0,
    );
    write_qh(mem, qh_addr, LINK_PTR_T, setup_td);
    install_frame_list(mem, fl_base, qh_addr);

    ctrl.tick_1ms(mem);
}

#[test]
fn enumerate_hid_keyboard_and_receive_keypress_report() {
    let mut ctrl = UhciController::new();
    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.hub_mut().attach(0, Box::new(keyboard.clone()));

    let mut mem = TestMemory::new(0x40000);
    let mut alloc = Alloc::new(0x2000);

    let fl_base = 0x1000;
    ctrl.io_write(REG_FLBASEADD, 4, fl_base);
    ctrl.io_write(REG_USBINTR, 2, USBINTR_IOC as u32);

    // Reset + enable port 1.
    ctrl.io_write(REG_PORTSC1, 2, PORTSC_PR as u32);
    for _ in 0..50 {
        ctrl.tick_1ms(&mut mem);
    }

    ctrl.io_write(REG_USBCMD, 2, (USBCMD_RS | USBCMD_MAXP) as u32);

    let mut ep0_max_packet = 8usize;

    // GET_DESCRIPTOR(Device) - first 8 bytes (host learns max packet size).
    let dev_desc8 = control_in(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        ep0_max_packet,
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0100,
            w_index: 0,
            w_length: 8,
        },
    );
    assert_eq!(dev_desc8.len(), 8);
    assert_eq!(dev_desc8[0], 18); // bLength of the full descriptor (18 bytes)
    assert_eq!(dev_desc8[1], 0x01);
    assert_eq!(dev_desc8[2..4], [0x00, 0x02]); // bcdUSB = 2.00
    ep0_max_packet = dev_desc8[7] as usize;
    assert_eq!(ep0_max_packet, 0x40);

    // GET_DESCRIPTOR(Device) - full descriptor now that we know max packet size.
    let dev_desc = control_in(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        ep0_max_packet,
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0100,
            w_index: 0,
            w_length: 18,
        },
    );
    assert_eq!(dev_desc.len(), 18);
    assert_eq!(dev_desc[0], 18);
    assert_eq!(dev_desc[1], 0x01);
    assert_eq!(dev_desc[7], 0x40);

    // SET_ADDRESS(1). Status stage still targets address 0.
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        0,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // GET_DESCRIPTOR(Configuration) from the new address.
    let cfg_desc = control_in(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        ep0_max_packet,
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0200,
            w_index: 0,
            w_length: 34,
        },
    );
    assert_eq!(cfg_desc.len(), 34);
    assert_eq!(cfg_desc[1], 0x02);

    // SET_CONFIGURATION(1).
    control_no_data(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // GET_DESCRIPTOR(Report) for interface 0.
    let report_desc = control_in(
        &mut ctrl,
        &mut mem,
        &mut alloc,
        fl_base,
        1,
        ep0_max_packet,
        SetupPacket {
            bm_request_type: 0x81,
            b_request: 0x06,
            w_value: 0x2200,
            w_index: 0,
            w_length: 63,
        },
    );
    assert!(report_desc.starts_with(&[0x05, 0x01, 0x09, 0x06]));

    // Schedule an interrupt IN transfer (ep1) to fetch a key report.
    let qh_addr = alloc.alloc(0x20, 0x10);
    let td_addr = alloc.alloc(0x20, 0x10);
    let buf_addr = alloc.alloc(8, 0x10);

    write_td(
        &mut mem,
        td_addr,
        LINK_PTR_T,
        td_ctrl(true, true),
        td_token(0x69, 1, 1, false, 8),
        buf_addr,
    );
    write_qh(&mut mem, qh_addr, LINK_PTR_T, td_addr);
    install_frame_list(&mut mem, fl_base, qh_addr);

    ctrl.tick_1ms(&mut mem);
    assert_eq!(mem.read_u32(td_addr + 4) & TD_CTRL_ACTIVE, TD_CTRL_ACTIVE);

    // Inject key press.
    let usage = keyboard_code_to_hid_usage("KeyA").unwrap();
    keyboard.key_event(usage, true);

    ctrl.tick_1ms(&mut mem);
    let ctrl_sts = mem.read_u32(td_addr + 4);
    assert_eq!(ctrl_sts & TD_CTRL_ACTIVE, 0);

    let report = &mem.data[buf_addr as usize..buf_addr as usize + 8];
    assert_eq!(report[0], 0); // modifiers
    assert_eq!(report[2], usage);
    assert!(ctrl.irq_level());
}
