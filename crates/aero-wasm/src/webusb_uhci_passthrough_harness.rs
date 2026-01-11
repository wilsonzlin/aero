use js_sys::{Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

use aero_usb::uhci::UhciController;
use aero_usb::{MemoryBus, SetupPacket as BusSetupPacket};

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
const REQ_SET_CONFIGURATION: u8 = 0x09;

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

#[derive(Clone, Copy)]
struct VecMemory {
    base: u32,
    len: u32,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        const WASM_PAGE_BYTES: u32 = 64 * 1024;

        let size_u32: u32 = size
            .try_into()
            .expect("VecMemory size must fit in u32 for wasm32");
        let pages = size_u32.div_ceil(WASM_PAGE_BYTES).max(1);
        let before_pages = core::arch::wasm32::memory_size(0) as u32;
        let prev = core::arch::wasm32::memory_grow(0, pages as usize);
        assert_ne!(
            prev,
            usize::MAX,
            "wasm memory.grow failed (requested {pages} pages)"
        );

        Self {
            base: before_pages * WASM_PAGE_BYTES,
            len: pages * WASM_PAGE_BYTES,
        }
    }

    fn clear(&mut self) {
        // Safety: `base`/`len` describe an allocated region in wasm linear memory.
        unsafe {
            core::ptr::write_bytes(self.base as *mut u8, 0, self.len as usize);
        }
    }

    fn linear_addr(&self, addr: u32, len: usize) -> u32 {
        let addr_u64 = addr as u64;
        let len_u64 = len as u64;
        let end = addr_u64
            .checked_add(len_u64)
            .expect("VecMemory address overflow");
        assert!(
            end <= self.len as u64,
            "VecMemory OOB: addr=0x{addr:x} len=0x{len:x} mem_len=0x{:x}",
            self.len
        );

        let linear = (self.base as u64)
            .checked_add(addr_u64)
            .expect("VecMemory linear address overflow");
        u32::try_from(linear).expect("VecMemory linear address must fit in u32")
    }

    fn read_u32(&self, addr: u32) -> u32 {
        let linear = self.linear_addr(addr, 4);
        // Safety: `linear_addr` bounds checks against the allocated linear-memory region.
        unsafe { core::ptr::read_unaligned(linear as *const u32) }
    }

    fn write_u32(&mut self, addr: u32, value: u32) {
        let linear = self.linear_addr(addr, 4);
        // Safety: `linear_addr` bounds checks against the allocated linear-memory region.
        unsafe { core::ptr::write_unaligned(linear as *mut u32, value) }
    }
}

impl MemoryBus for VecMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let addr = u32::try_from(paddr).expect("VecMemory address must fit in u32");
        let linear = self.linear_addr(addr, buf.len());
        // Safety: `linear_addr` bounds checks; `buf` is a valid slice.
        unsafe {
            core::ptr::copy_nonoverlapping(linear as *const u8, buf.as_mut_ptr(), buf.len());
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let addr = u32::try_from(paddr).expect("VecMemory address must fit in u32");
        let linear = self.linear_addr(addr, buf.len());
        // Safety: `linear_addr` bounds checks; `buf` is a valid slice.
        unsafe {
            core::ptr::copy_nonoverlapping(buf.as_ptr(), linear as *mut u8, buf.len());
        }
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
    SetConfiguration,
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

    fn collect_in_bytes(&self, mem: &mut VecMemory) -> Vec<u8> {
        if !self.direction_in {
            return Vec::new();
        }
        let mut out = Vec::new();
        for (td_addr, buf_addr) in &self.data_tds {
            let ctrl_sts = VecMemory::read_u32(mem, *td_addr + 4);
            let got = td_actlen(ctrl_sts);
            if got == 0 {
                continue;
            }
            let mut tmp = vec![0u8; got];
            mem.read_physical(u64::from(*buf_addr), &mut tmp);
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
    bytes[0] = setup.bm_request_type;
    bytes[1] = setup.b_request;
    bytes[2..4].copy_from_slice(&setup.w_value.to_le_bytes());
    bytes[4..6].copy_from_slice(&setup.w_index.to_le_bytes());
    bytes[6..8].copy_from_slice(&setup.w_length.to_le_bytes());
    mem.write_physical(u64::from(setup_buf), &bytes);

    let mut tds = Vec::new();
    tds.push((setup_td, setup_buf, 8usize, PID_SETUP, false));

    let mut remaining = setup.w_length as usize;
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
    bytes[0] = setup.bm_request_type;
    bytes[1] = setup.b_request;
    bytes[2..4].copy_from_slice(&setup.w_value.to_le_bytes());
    bytes[4..6].copy_from_slice(&setup.w_index.to_le_bytes());
    bytes[6..8].copy_from_slice(&setup.w_length.to_le_bytes());
    mem.write_physical(u64::from(setup_buf), &bytes);

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
    webusb: UsbWebUsbPassthroughDevice,
    alloc: Alloc,

    qh_addr: u32,
    fl_base: u32,

    phase: HarnessPhase,
    phase_detail: String,
    reset_remaining: u8,
    frames_stepped: u32,
    total_actions_drained: u32,
    total_completions_pushed: u32,

    max_packet: usize,
    pending_chain: Option<ControlChain>,
    last_error: Option<String>,
    device_descriptor: Vec<u8>,
    config_descriptor: Vec<u8>,
    config_total_len: usize,
    config_value: u8,
}

impl WebUsbUhciPassthroughHarness {
    fn init_with_mem(mut mem: VecMemory) -> Self {
        let mut ctrl = UhciController::new();
        let webusb = UsbWebUsbPassthroughDevice::new();
        ctrl.hub_mut().attach(0, Box::new(webusb.clone()));

        mem.clear();
        let alloc = Alloc::new(0x3000);

        let fl_base = 0x1000;
        let qh_addr = 0x2000;

        // Program frame list base and enable IOC interrupts (not strictly needed, but keeps the model realistic).
        ctrl.io_write(REG_FRBASEADD, 4, fl_base);
        ctrl.io_write(REG_USBINTR, 2, USBINTR_IOC as u32);

        // Start port reset; we will tick for 50 frames before enabling RUN.
        ctrl.io_write(REG_PORTSC1, 2, PORTSC_PR as u32);

        // Install a terminating QH so we can patch in TD chains later.
        write_qh(&mut mem, qh_addr, LINK_PTR_T, LINK_PTR_T);
        install_frame_list(&mut mem, fl_base, qh_addr);

        Self {
            ctrl,
            mem,
            webusb,
            alloc,
            qh_addr,
            fl_base,
            phase: HarnessPhase::ResetPort,
            phase_detail: "resetting port".to_string(),
            reset_remaining: 50,
            frames_stepped: 0,
            total_actions_drained: 0,
            total_completions_pushed: 0,
            max_packet: 8,
            pending_chain: None,
            last_error: None,
            device_descriptor: Vec::new(),
            config_descriptor: Vec::new(),
            config_total_len: 0,
            config_value: 1,
        }
    }
}

#[wasm_bindgen]
impl WebUsbUhciPassthroughHarness {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::init_with_mem(VecMemory::new(0x40000))
    }

    /// Human-readable state for UI debugging.
    pub fn state(&self) -> String {
        format!("{:?}: {}", self.phase, self.phase_detail)
    }

    pub fn reset(&mut self) {
        // Reuse the backing guest-memory region so repeated resets don't keep growing the wasm
        // linear memory (which cannot be shrunk).
        *self = Self::init_with_mem(self.mem);
    }

    pub fn status(&self) -> JsValue {
        let summary = self.webusb.pending_summary();

        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("phase"),
            &JsValue::from_str(&format!("{:?}", self.phase)),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("phaseDetail"),
            &JsValue::from_str(&self.phase_detail),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("complete"),
            &JsValue::from_bool(matches!(self.phase, HarnessPhase::Done)),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("resetRemaining"),
            &JsValue::from_f64(self.reset_remaining as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("framesStepped"),
            &JsValue::from_f64(self.frames_stepped as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("totalActionsDrained"),
            &JsValue::from_f64(self.total_actions_drained as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("totalCompletionsPushed"),
            &JsValue::from_f64(self.total_completions_pushed as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("maxPacket"),
            &JsValue::from_f64(self.max_packet as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("configTotalLen"),
            &JsValue::from_f64(self.config_total_len as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("configValue"),
            &JsValue::from_f64(self.config_value as f64),
        );

        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("queuedActions"),
            &JsValue::from_f64(summary.queued_actions as f64),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("queuedCompletions"),
            &JsValue::from_f64(summary.queued_completions as f64),
        );
        let inflight_control = summary
            .inflight_control
            .map(|id| JsValue::from_f64(id as f64))
            .unwrap_or(JsValue::NULL);
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("inflightControl"),
            &inflight_control,
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("inflightEndpoints"),
            &JsValue::from_f64(summary.inflight_endpoints as f64),
        );

        let device_desc = if self.device_descriptor.is_empty() {
            JsValue::NULL
        } else {
            Uint8Array::from(self.device_descriptor.as_slice()).into()
        };
        let config_desc = if self.config_descriptor.is_empty() {
            JsValue::NULL
        } else {
            Uint8Array::from(self.config_descriptor.as_slice()).into()
        };
        let _ = Reflect::set(&obj, &JsValue::from_str("deviceDescriptor"), &device_desc);
        let _ = Reflect::set(&obj, &JsValue::from_str("configDescriptor"), &config_desc);
        obj.into()
    }

    pub fn tick(&mut self) -> JsValue {
        self.frames_stepped = self.frames_stepped.saturating_add(1);
        // Drive one UHCI frame worth of work.
        self.ctrl.tick_1ms(&mut self.mem);

        if let Some(chain) = &self.pending_chain {
            if let Some(err) = chain.error_reason(&self.mem) {
                self.phase = HarnessPhase::Error;
                self.phase_detail = err;
                self.pending_chain = None;
                return self.status();
            }
        }
        if let Some(err) = self.last_error.take() {
            self.phase = HarnessPhase::Error;
            self.phase_detail = err;
            self.pending_chain = None;
            return self.status();
        }

        match self.phase {
            HarnessPhase::ResetPort => {
                if self.reset_remaining > 0 {
                    self.reset_remaining -= 1;
                    self.phase_detail = format!("port reset... {}/50", 50 - self.reset_remaining);
                    return self.status();
                }

                // Enable the controller once the port reset window is done.
                self.ctrl.io_write(REG_USBCMD, 2, USBCMD_RUN as u32);

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
                        bm_request_type: 0x80,
                        b_request: 0x06,
                        w_value: 0x0100,
                        w_index: 0,
                        w_length: 8,
                    },
                ));
            }
            HarnessPhase::GetDeviceDesc8 => {
                if let Some(chain) = &self.pending_chain {
                    if !chain.is_complete(&self.mem) {
                        return self.status();
                    }
                    let bytes = chain.collect_in_bytes(&mut self.mem);
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
                            bm_request_type: 0x80,
                            b_request: 0x06,
                            w_value: 0x0100,
                            w_index: 0,
                            w_length: 18,
                        },
                    ));
                }
            }
            HarnessPhase::GetDeviceDesc18 => {
                if let Some(chain) = &self.pending_chain {
                    if !chain.is_complete(&self.mem) {
                        return self.status();
                    }
                    self.device_descriptor = chain.collect_in_bytes(&mut self.mem);

                    self.phase = HarnessPhase::SetAddress;
                    self.phase_detail = "SET_ADDRESS(1)".to_string();
                    self.pending_chain = Some(build_control_out_no_data_chain(
                        &mut self.mem,
                        &mut self.alloc,
                        self.qh_addr,
                        self.fl_base,
                        0,
                        BusSetupPacket {
                            bm_request_type: 0x00,
                            b_request: REQ_SET_ADDRESS,
                            w_value: 1,
                            w_index: 0,
                            w_length: 0,
                        },
                    ));
                }
            }
            HarnessPhase::SetAddress => {
                if let Some(chain) = &self.pending_chain {
                    if !chain.is_complete(&self.mem) {
                        return self.status();
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
                            bm_request_type: 0x80,
                            b_request: 0x06,
                            w_value: 0x0200,
                            w_index: 0,
                            w_length: 9,
                        },
                    ));
                }
            }
            HarnessPhase::GetConfigDesc9 => {
                if let Some(chain) = &self.pending_chain {
                    if !chain.is_complete(&self.mem) {
                        return self.status();
                    }
                    let bytes = chain.collect_in_bytes(&mut self.mem);
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
                            bm_request_type: 0x80,
                            b_request: 0x06,
                            w_value: 0x0200,
                            w_index: 0,
                            w_length: self.config_total_len.min(u16::MAX as usize) as u16,
                        },
                    ));
                }
            }
            HarnessPhase::GetConfigDescFull => {
                if let Some(chain) = &self.pending_chain {
                    if !chain.is_complete(&self.mem) {
                        return self.status();
                    }
                    self.config_descriptor = chain.collect_in_bytes(&mut self.mem);
                    self.phase = HarnessPhase::SetConfiguration;
                    self.phase_detail = format!("SET_CONFIGURATION({})", self.config_value);
                    self.pending_chain = Some(build_control_out_no_data_chain(
                        &mut self.mem,
                        &mut self.alloc,
                        self.qh_addr,
                        self.fl_base,
                        1,
                        BusSetupPacket {
                            bm_request_type: 0x00,
                            b_request: REQ_SET_CONFIGURATION,
                            w_value: self.config_value as u16,
                            w_index: 0,
                            w_length: 0,
                        },
                    ));
                }
            }
            HarnessPhase::SetConfiguration => {
                if let Some(chain) = &self.pending_chain {
                    if !chain.is_complete(&self.mem) {
                        return self.status();
                    }
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

        self.status()
    }

    /// Drain all queued UsbHostAction objects.
    pub fn drain_actions(&mut self) -> Result<JsValue, JsValue> {
        let actions = self.webusb.drain_actions();
        self.total_actions_drained = self
            .total_actions_drained
            .saturating_add(actions.len() as u32);
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
                UsbHostCompletionIn::Stall => {
                    self.last_error = Some("WebUSB bulkIn stalled".to_string())
                }
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
        self.webusb.push_completion(completion);
        self.total_completions_pushed = self.total_completions_pushed.saturating_add(1);
        Ok(())
    }
}
