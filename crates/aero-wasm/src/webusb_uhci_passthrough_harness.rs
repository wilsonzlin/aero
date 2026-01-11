use wasm_bindgen::prelude::*;

use aero_usb::GuestMemory;
use aero_usb::uhci::{InterruptController, UhciController};
use aero_usb::usb::SetupPacket as BusSetupPacket;

use aero_usb::passthrough::{
    UsbHostCompletion, UsbHostCompletionIn, UsbHostCompletionOut, UsbWebUsbPassthroughDevice,
};

// Minimal UHCI register offsets / bits (mirrors the test harness in `crates/aero-usb/tests/hid_enum.rs`).
const REG_USBCMD: u16 = 0x00;
const REG_USBINTR: u16 = 0x04;
const REG_FRBASEADD: u16 = 0x08;
const REG_PORTSC1: u16 = 0x10;

const USBCMD_RUN: u16 = 1 << 0;
const USBINTR_IOC: u16 = 1 << 2;
const PORTSC_PR: u16 = 1 << 9;

// UHCI link pointer bits.
const LINK_PTR_T: u32 = 1 << 0;
const LINK_PTR_Q: u32 = 1 << 1;

// UHCI TD control/token fields.
const TD_CTRL_ACTIVE: u32 = 1 << 23;
const TD_CTRL_IOC: u32 = 1 << 24;
const TD_CTRL_ACTLEN_MASK: u32 = 0x7FF;

// Error bits (mirrors `aero_usb::uhci` constants).
const TD_CTRL_BITSTUFF: u32 = 1 << 17;
const TD_CTRL_CRCERR: u32 = 1 << 18;
const TD_CTRL_BABBLE: u32 = 1 << 20;
const TD_CTRL_DBUFERR: u32 = 1 << 21;
const TD_CTRL_STALLED: u32 = 1 << 22;

const TD_TOKEN_DEVADDR_SHIFT: u32 = 8;
const TD_TOKEN_ENDPT_SHIFT: u32 = 15;
const TD_TOKEN_D: u32 = 1 << 19;
const TD_TOKEN_MAXLEN_SHIFT: u32 = 21;

// PIDs.
const PID_SETUP: u8 = 0x2D;
const PID_IN: u8 = 0x69;
const PID_OUT: u8 = 0xE1;

// Standard requests.
const REQ_SET_ADDRESS: u8 = 0x05;

fn td_token(pid: u8, addr: u8, ep: u8, toggle: bool, max_len: usize) -> u32 {
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

fn td_actlen(ctrl_sts: u32) -> usize {
    let field = ctrl_sts & TD_CTRL_ACTLEN_MASK;
    if field == 0x7FF {
        0
    } else {
        (field as usize) + 1
    }
}

#[derive(Default)]
struct DummyIrq;

impl InterruptController for DummyIrq {
    fn raise_irq(&mut self, _irq: u8) {}
    fn lower_irq(&mut self, _irq: u8) {}
}

struct VecMemory {
    data: Vec<u8>,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn read_u32(&self, addr: u32) -> u32 {
        let addr = addr as usize;
        u32::from_le_bytes(self.data[addr..addr + 4].try_into().unwrap())
    }

    fn write_u32(&mut self, addr: u32, value: u32) {
        self.write(addr, &value.to_le_bytes());
    }
}

impl GuestMemory for VecMemory {
    fn read(&self, addr: u32, buf: &mut [u8]) {
        let addr = addr as usize;
        buf.copy_from_slice(&self.data[addr..addr + buf.len()]);
    }

    fn write(&mut self, addr: u32, buf: &[u8]) {
        let addr = addr as usize;
        self.data[addr..addr + buf.len()].copy_from_slice(buf);
    }
}

struct Alloc {
    next: u32,
}

impl Alloc {
    fn new(start: u32) -> Self {
        Self { next: start }
    }

    fn alloc(&mut self, size: u32, align: u32) -> u32 {
        let align = align.max(1);
        let mask = align - 1;
        let addr = (self.next + mask) & !mask;
        self.next = addr + size;
        addr
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HarnessPhase {
    ResetPort,
    GetDeviceDesc8,
    GetDeviceDesc18,
    SetAddress,
    GetConfigDesc9,
    GetConfigDescFull,
    Done,
    Error,
}

struct ControlChain {
    last_td: u32,
    td_addrs: Vec<u32>,
    data_tds: Vec<(u32, u32)>, // (td_addr, buf_addr)
    direction_in: bool,
}

impl ControlChain {
    fn is_complete(&self, mem: &VecMemory) -> bool {
        mem.read_u32(self.last_td + 4) & TD_CTRL_ACTIVE == 0
    }

    fn error_reason(&self, mem: &VecMemory) -> Option<String> {
        for td_addr in &self.td_addrs {
            let ctrl_sts = mem.read_u32(*td_addr + 4);
            if ctrl_sts & TD_CTRL_ACTIVE != 0 {
                continue;
            }

            // Ignore the "NAK" bit for detection; it's used while ACTIVE.
            let err_bits = ctrl_sts
                & (TD_CTRL_STALLED
                    | TD_CTRL_CRCERR
                    | TD_CTRL_BABBLE
                    | TD_CTRL_DBUFERR
                    | TD_CTRL_BITSTUFF);
            if err_bits == 0 {
                continue;
            }

            let mut reasons = Vec::new();
            if err_bits & TD_CTRL_STALLED != 0 {
                reasons.push("stall");
            }
            if err_bits & TD_CTRL_CRCERR != 0 {
                reasons.push("crc/timeout");
            }
            if err_bits & TD_CTRL_BABBLE != 0 {
                reasons.push("babble");
            }
            if err_bits & TD_CTRL_DBUFERR != 0 {
                reasons.push("dbuferr");
            }
            if err_bits & TD_CTRL_BITSTUFF != 0 {
                reasons.push("bitstuff");
            }

            let reason = if reasons.is_empty() {
                "unknown".to_string()
            } else {
                reasons.join("+")
            };
            return Some(format!(
                "UHCI TD error at 0x{td_addr:08x}: {reason} (ctrl_sts=0x{ctrl_sts:08x})"
            ));
        }
        None
    }

    fn collect_in_bytes(&self, mem: &VecMemory) -> Vec<u8> {
        if !self.direction_in {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (td_addr, buf_addr) in &self.data_tds {
            let ctrl_sts = mem.read_u32(*td_addr + 4);
            let got = td_actlen(ctrl_sts);
            if got == 0 {
                continue;
            }
            let mut tmp = vec![0u8; got];
            mem.read(*buf_addr, &mut tmp);
            out.extend_from_slice(&tmp);
        }
        out
    }
}

fn install_frame_list(mem: &mut VecMemory, fl_base: u32, qh_addr: u32) {
    for i in 0..1024u32 {
        mem.write_u32(fl_base + i * 4, qh_addr | LINK_PTR_Q);
    }
}

fn write_qh(mem: &mut VecMemory, addr: u32, head: u32, element: u32) {
    mem.write_u32(addr, head);
    mem.write_u32(addr + 4, element);
}

fn write_td(mem: &mut VecMemory, addr: u32, link_ptr: u32, ctrl_sts: u32, token: u32, buffer: u32) {
    mem.write_u32(addr, link_ptr);
    mem.write_u32(addr + 4, ctrl_sts);
    mem.write_u32(addr + 8, token);
    mem.write_u32(addr + 12, buffer);
}

fn build_control_in_chain(
    mem: &mut VecMemory,
    alloc: &mut Alloc,
    qh_addr: u32,
    fl_base: u32,
    devaddr: u8,
    max_packet: usize,
    setup: BusSetupPacket,
) -> ControlChain {
    let setup_buf = alloc.alloc(8, 0x10);
    let setup_td = alloc.alloc(0x20, 0x10);

    let mut bytes = [0u8; 8];
    bytes[0] = setup.request_type;
    bytes[1] = setup.request;
    bytes[2..4].copy_from_slice(&setup.value.to_le_bytes());
    bytes[4..6].copy_from_slice(&setup.index.to_le_bytes());
    bytes[6..8].copy_from_slice(&setup.length.to_le_bytes());
    mem.write(setup_buf, &bytes);

    let mut tds = Vec::new();
    tds.push((setup_td, setup_buf, 8usize, PID_SETUP, false));

    let mut remaining = setup.length as usize;
    let mut toggle = true;
    let mut data_tds = Vec::new();
    while remaining != 0 {
        let chunk = remaining.min(max_packet);
        let buf = alloc.alloc(chunk as u32, 0x10);
        let td = alloc.alloc(0x20, 0x10);
        tds.push((td, buf, chunk, PID_IN, toggle));
        data_tds.push((td, buf));
        toggle = !toggle;
        remaining -= chunk;
    }

    let status_td = alloc.alloc(0x20, 0x10);
    tds.push((status_td, 0, 0, PID_OUT, true));

    let td_addrs: Vec<u32> = tds.iter().map(|(td_addr, _, _, _, _)| *td_addr).collect();

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

    ControlChain {
        last_td: status_td,
        td_addrs,
        data_tds,
        direction_in: true,
    }
}

fn build_control_out_no_data_chain(
    mem: &mut VecMemory,
    alloc: &mut Alloc,
    qh_addr: u32,
    fl_base: u32,
    devaddr: u8,
    setup: BusSetupPacket,
) -> ControlChain {
    let setup_buf = alloc.alloc(8, 0x10);
    let setup_td = alloc.alloc(0x20, 0x10);
    let status_td = alloc.alloc(0x20, 0x10);

    let mut bytes = [0u8; 8];
    bytes[0] = setup.request_type;
    bytes[1] = setup.request;
    bytes[2..4].copy_from_slice(&setup.value.to_le_bytes());
    bytes[4..6].copy_from_slice(&setup.index.to_le_bytes());
    bytes[6..8].copy_from_slice(&setup.length.to_le_bytes());
    mem.write(setup_buf, &bytes);

    write_td(
        mem,
        setup_td,
        status_td,
        td_ctrl(true, false),
        td_token(PID_SETUP, devaddr, 0, false, 8),
        setup_buf,
    );
    // Status stage: IN zero-length, DATA1.
    write_td(
        mem,
        status_td,
        LINK_PTR_T,
        td_ctrl(true, true),
        td_token(PID_IN, devaddr, 0, true, 0),
        0,
    );

    write_qh(mem, qh_addr, LINK_PTR_T, setup_td);
    install_frame_list(mem, fl_base, qh_addr);

    let td_addrs = vec![setup_td, status_td];

    ControlChain {
        last_td: status_td,
        td_addrs,
        data_tds: Vec::new(),
        direction_in: false,
    }
}

#[wasm_bindgen]
pub struct WebUsbUhciPassthroughHarness {
    ctrl: UhciController,
    mem: VecMemory,
    irq: DummyIrq,
    alloc: Alloc,

    qh_addr: u32,
    fl_base: u32,

    phase: HarnessPhase,
    phase_detail: String,
    reset_remaining: u8,

    max_packet: usize,
    pending_chain: Option<ControlChain>,
    last_error: Option<String>,
    device_descriptor: Vec<u8>,
    config_descriptor: Vec<u8>,
    config_total_len: usize,
    config_value: u8,
}

impl WebUsbUhciPassthroughHarness {
    fn passthrough_device_mut(&mut self) -> &mut UsbWebUsbPassthroughDevice {
        let port = self
            .ctrl
            .bus_mut()
            .port_mut(0)
            .expect("UHCI port 0 exists");
        let dev = port.device.as_mut().expect("UHCI port 0 device attached");
        dev.as_any_mut()
            .downcast_mut::<UsbWebUsbPassthroughDevice>()
            .expect("UHCI port 0 device is UsbWebUsbPassthroughDevice")
    }
}

#[wasm_bindgen]
impl WebUsbUhciPassthroughHarness {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        let io_base = 0x5000;
        let mut ctrl = UhciController::new(io_base, 11);

        ctrl.connect_device(0, Box::new(UsbWebUsbPassthroughDevice::new()));

        let mut mem = VecMemory::new(0x40000);
        let mut irq = DummyIrq::default();
        let alloc = Alloc::new(0x3000);

        let fl_base = 0x1000;
        let qh_addr = 0x2000;

        // Program frame list base and enable IOC interrupts (not strictly needed, but keeps the model realistic).
        ctrl.port_write(io_base + REG_FRBASEADD, 4, fl_base, &mut irq);
        ctrl.port_write(io_base + REG_USBINTR, 2, USBINTR_IOC as u32, &mut irq);

        // Start port reset; we will tick for 50 frames before enabling RUN.
        ctrl.port_write(io_base + REG_PORTSC1, 2, PORTSC_PR as u32, &mut irq);

        // Install a terminating QH so we can patch in TD chains later.
        write_qh(&mut mem, qh_addr, LINK_PTR_T, LINK_PTR_T);
        install_frame_list(&mut mem, fl_base, qh_addr);

        Self {
            ctrl,
            mem,
            irq,
            alloc,
            qh_addr,
            fl_base,
            phase: HarnessPhase::ResetPort,
            phase_detail: "resetting port".to_string(),
            reset_remaining: 50,
            max_packet: 8,
            pending_chain: None,
            last_error: None,
            device_descriptor: Vec::new(),
            config_descriptor: Vec::new(),
            config_total_len: 0,
            config_value: 1,
        }
    }

    /// Human-readable state for UI debugging.
    pub fn state(&self) -> String {
        format!("{:?}: {}", self.phase, self.phase_detail)
    }

    pub fn tick(&mut self) {
        // Drive one UHCI frame worth of work.
        self.ctrl.step_frame(&mut self.mem, &mut self.irq);

        if let Some(chain) = &self.pending_chain {
            if let Some(err) = chain.error_reason(&self.mem) {
                self.phase = HarnessPhase::Error;
                self.phase_detail = err;
                self.pending_chain = None;
                return;
            }
        }
        if let Some(err) = self.last_error.take() {
            self.phase = HarnessPhase::Error;
            self.phase_detail = err;
            self.pending_chain = None;
            return;
        }

        match self.phase {
            HarnessPhase::ResetPort => {
                if self.reset_remaining > 0 {
                    self.reset_remaining -= 1;
                    self.phase_detail = format!("port reset... {}/50", 50 - self.reset_remaining);
                    return;
                }

                // Enable the controller once the port reset window is done.
                let io_base = self.ctrl.io_base();
                self.ctrl
                    .port_write(io_base + REG_USBCMD, 2, USBCMD_RUN as u32, &mut self.irq);

                self.phase = HarnessPhase::GetDeviceDesc8;
                self.phase_detail = "GET_DESCRIPTOR(Device, 8)".to_string();
                self.pending_chain = Some(build_control_in_chain(
                    &mut self.mem,
                    &mut self.alloc,
                    self.qh_addr,
                    self.fl_base,
                    0,
                    self.max_packet,
                    BusSetupPacket {
                        request_type: 0x80,
                        request: 0x06,
                        value: 0x0100,
                        index: 0,
                        length: 8,
                    },
                ));
            }
            HarnessPhase::GetDeviceDesc8 => {
                if let Some(chain) = &self.pending_chain {
                    if !chain.is_complete(&self.mem) {
                        return;
                    }
                    let bytes = chain.collect_in_bytes(&self.mem);
                    if bytes.len() >= 8 {
                        self.max_packet = bytes[7] as usize;
                        if self.max_packet == 0 {
                            self.max_packet = 8;
                        }
                    }
                    self.phase = HarnessPhase::GetDeviceDesc18;
                    self.phase_detail =
                        format!("GET_DESCRIPTOR(Device, 18) max_packet={}", self.max_packet);
                    self.pending_chain = Some(build_control_in_chain(
                        &mut self.mem,
                        &mut self.alloc,
                        self.qh_addr,
                        self.fl_base,
                        0,
                        self.max_packet,
                        BusSetupPacket {
                            request_type: 0x80,
                            request: 0x06,
                            value: 0x0100,
                            index: 0,
                            length: 18,
                        },
                    ));
                }
            }
            HarnessPhase::GetDeviceDesc18 => {
                if let Some(chain) = &self.pending_chain {
                    if !chain.is_complete(&self.mem) {
                        return;
                    }
                    self.device_descriptor = chain.collect_in_bytes(&self.mem);

                    self.phase = HarnessPhase::SetAddress;
                    self.phase_detail = "SET_ADDRESS(1)".to_string();
                    self.pending_chain = Some(build_control_out_no_data_chain(
                        &mut self.mem,
                        &mut self.alloc,
                        self.qh_addr,
                        self.fl_base,
                        0,
                        BusSetupPacket {
                            request_type: 0x00,
                            request: REQ_SET_ADDRESS,
                            value: 1,
                            index: 0,
                            length: 0,
                        },
                    ));
                }
            }
            HarnessPhase::SetAddress => {
                if let Some(chain) = &self.pending_chain {
                    if !chain.is_complete(&self.mem) {
                        return;
                    }

                    self.phase = HarnessPhase::GetConfigDesc9;
                    self.phase_detail = "GET_DESCRIPTOR(Config, 9)".to_string();
                    self.pending_chain = Some(build_control_in_chain(
                        &mut self.mem,
                        &mut self.alloc,
                        self.qh_addr,
                        self.fl_base,
                        1,
                        self.max_packet,
                        BusSetupPacket {
                            request_type: 0x80,
                            request: 0x06,
                            value: 0x0200,
                            index: 0,
                            length: 9,
                        },
                    ));
                }
            }
            HarnessPhase::GetConfigDesc9 => {
                if let Some(chain) = &self.pending_chain {
                    if !chain.is_complete(&self.mem) {
                        return;
                    }
                    let bytes = chain.collect_in_bytes(&self.mem);
                    if bytes.len() >= 9 {
                        self.config_total_len = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
                        self.config_value = bytes[5];
                    }
                    if self.config_total_len == 0 {
                        self.config_total_len = 9;
                    }

                    self.phase = HarnessPhase::GetConfigDescFull;
                    self.phase_detail =
                        format!("GET_DESCRIPTOR(Config, {})", self.config_total_len);
                    self.pending_chain = Some(build_control_in_chain(
                        &mut self.mem,
                        &mut self.alloc,
                        self.qh_addr,
                        self.fl_base,
                        1,
                        self.max_packet,
                        BusSetupPacket {
                            request_type: 0x80,
                            request: 0x06,
                            value: 0x0200,
                            index: 0,
                            length: self.config_total_len.min(u16::MAX as usize) as u16,
                        },
                    ));
                }
            }
            HarnessPhase::GetConfigDescFull => {
                if let Some(chain) = &self.pending_chain {
                    if !chain.is_complete(&self.mem) {
                        return;
                    }
                    self.config_descriptor = chain.collect_in_bytes(&self.mem);
                    self.phase = HarnessPhase::Done;
                    self.phase_detail = format!(
                        "done (device_desc={} bytes, config_desc={} bytes, config_value={})",
                        self.device_descriptor.len(),
                        self.config_descriptor.len(),
                        self.config_value
                    );
                    self.pending_chain = None;
                }
            }
            HarnessPhase::Done | HarnessPhase::Error => {}
        }
    }

    /// Drain all queued UsbHostAction objects.
    pub fn drain_actions(&mut self) -> Result<JsValue, JsValue> {
        let actions = self.passthrough_device_mut().drain_actions();
        if actions.is_empty() {
            // Keep polling cheap when the harness is idle.
            return Ok(JsValue::NULL);
        }
        serde_wasm_bindgen::to_value(&actions).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Push a single host completion into the harness.
    pub fn push_completion(&mut self, completion: JsValue) -> Result<(), JsValue> {
        let completion: UsbHostCompletion = serde_wasm_bindgen::from_value(completion)
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        match &completion {
            UsbHostCompletion::ControlIn { result, .. } => match result {
                UsbHostCompletionIn::Stall => {
                    self.last_error = Some("WebUSB controlIn stalled".to_string())
                }
                UsbHostCompletionIn::Error { message } => self.last_error = Some(message.clone()),
                _ => {}
            },
            UsbHostCompletion::ControlOut { result, .. } => match result {
                UsbHostCompletionOut::Stall => {
                    self.last_error = Some("WebUSB controlOut stalled".to_string())
                }
                UsbHostCompletionOut::Error { message } => self.last_error = Some(message.clone()),
                _ => {}
            },
            UsbHostCompletion::BulkIn { result, .. } => match result {
                UsbHostCompletionIn::Stall => self.last_error = Some("WebUSB bulkIn stalled".to_string()),
                UsbHostCompletionIn::Error { message } => self.last_error = Some(message.clone()),
                _ => {}
            },
            UsbHostCompletion::BulkOut { result, .. } => match result {
                UsbHostCompletionOut::Stall => {
                    self.last_error = Some("WebUSB bulkOut stalled".to_string())
                }
                UsbHostCompletionOut::Error { message } => self.last_error = Some(message.clone()),
                _ => {}
            },
        }
        self.passthrough_device_mut().push_completion(completion);
        Ok(())
    }
}
