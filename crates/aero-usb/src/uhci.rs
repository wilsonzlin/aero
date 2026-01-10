use crate::memory::GuestMemory;
use crate::usb::{SetupPacket, UsbBus, UsbHandshake, UsbPid, UsbSpeed};

const REG_USBCMD: u16 = 0x00;
const REG_USBSTS: u16 = 0x02;
const REG_USBINTR: u16 = 0x04;
const REG_FRNUM: u16 = 0x06;
const REG_FRBASEADD: u16 = 0x08;
const REG_SOFMOD: u16 = 0x0C;
const REG_PORTSC1: u16 = 0x10;
const REG_PORTSC2: u16 = 0x12;

const USBCMD_RUN: u16 = 1 << 0;
const USBCMD_HCRESET: u16 = 1 << 1;
const USBCMD_GRESET: u16 = 1 << 2;
const USBCMD_EGSM: u16 = 1 << 3;
const USBCMD_FGR: u16 = 1 << 4;
const USBCMD_SWDBG: u16 = 1 << 5;
const USBCMD_CF: u16 = 1 << 6;
const USBCMD_MAXP: u16 = 1 << 7;

const USBCMD_WRITABLE_MASK: u16 = USBCMD_RUN
    | USBCMD_HCRESET
    | USBCMD_GRESET
    | USBCMD_EGSM
    | USBCMD_FGR
    | USBCMD_SWDBG
    | USBCMD_CF
    | USBCMD_MAXP;

const USBSTS_USBINT: u16 = 1 << 0;
const USBSTS_USBERRINT: u16 = 1 << 1;
const USBSTS_HC_HALT: u16 = 1 << 5;

const USBINTR_TIMEOUT_CRC: u16 = 1 << 0;
const USBINTR_IOC: u16 = 1 << 2;

const PORTSC_CCS: u16 = 1 << 0;
const PORTSC_CSC: u16 = 1 << 1;
const PORTSC_PED: u16 = 1 << 2;
const PORTSC_PEDC: u16 = 1 << 3;
const PORTSC_LSDA: u16 = 1 << 8;
const PORTSC_PR: u16 = 1 << 9;

const TD_CTRL_ACTLEN_MASK: u32 = 0x7FF;
const TD_CTRL_BITSTUFF: u32 = 1 << 17;
const TD_CTRL_CRCERR: u32 = 1 << 18;
const TD_CTRL_NAK: u32 = 1 << 19;
const TD_CTRL_BABBLE: u32 = 1 << 20;
const TD_CTRL_DBUFERR: u32 = 1 << 21;
const TD_CTRL_STALLED: u32 = 1 << 22;
const TD_CTRL_ACTIVE: u32 = 1 << 23;
const TD_CTRL_IOC: u32 = 1 << 24;

const TD_TOKEN_PID_MASK: u32 = 0xFF;
const TD_TOKEN_DEVADDR_SHIFT: u32 = 8;
const TD_TOKEN_DEVADDR_MASK: u32 = 0x7F << TD_TOKEN_DEVADDR_SHIFT;
const TD_TOKEN_ENDPT_SHIFT: u32 = 15;
const TD_TOKEN_ENDPT_MASK: u32 = 0x0F << TD_TOKEN_ENDPT_SHIFT;
const TD_TOKEN_MAXLEN_SHIFT: u32 = 21;
const TD_TOKEN_MAXLEN_MASK: u32 = 0x7FF << TD_TOKEN_MAXLEN_SHIFT;

const LINK_PTR_T: u32 = 1 << 0;
const LINK_PTR_Q: u32 = 1 << 1;

const MAX_LINK_TRAVERSAL: usize = 4096;

pub trait InterruptController {
    fn raise_irq(&mut self, irq: u8);
    fn lower_irq(&mut self, irq: u8);
}

#[derive(Debug, Clone, Copy)]
struct UhciTd {
    link_ptr: u32,
    ctrl_sts: u32,
    token: u32,
    buffer: u32,
}

impl UhciTd {
    fn read(mem: &dyn GuestMemory, addr: u32) -> Self {
        Self {
            link_ptr: mem.read_u32(addr),
            ctrl_sts: mem.read_u32(addr + 0x04),
            token: mem.read_u32(addr + 0x08),
            buffer: mem.read_u32(addr + 0x0C),
        }
    }

    fn write_status(mem: &mut dyn GuestMemory, addr: u32, ctrl_sts: u32) {
        mem.write_u32(addr + 0x04, ctrl_sts);
    }
}

#[derive(Debug, Clone, Copy)]
struct UhciQh {
    head: u32,
    element: u32,
}

impl UhciQh {
    fn read(mem: &dyn GuestMemory, addr: u32) -> Self {
        Self {
            head: mem.read_u32(addr),
            element: mem.read_u32(addr + 0x04),
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct PortState {
    reg: u16,
    reset_timer_ms: u8,
}

impl PortState {
    fn value(&self, bus: &UsbBus, idx: usize) -> u16 {
        let mut v = self.reg;
        if let Some(port) = bus.port(idx) {
            if port.connected {
                v |= PORTSC_CCS;
            } else {
                v &= !PORTSC_CCS;
            }
            if port.enabled {
                v |= PORTSC_PED;
            } else {
                v &= !PORTSC_PED;
            }
            if let Some(dev) = port.device.as_ref() {
                if dev.speed() == UsbSpeed::Low {
                    v |= PORTSC_LSDA;
                } else {
                    v &= !PORTSC_LSDA;
                }
            } else {
                v &= !PORTSC_LSDA;
            }
        }
        v
    }

    fn set_connected(&mut self, connected: bool) {
        let was = self.reg & PORTSC_CCS != 0;
        if connected != was {
            self.reg |= PORTSC_CSC;
        }
        if connected {
            self.reg |= PORTSC_CCS;
        } else {
            self.reg &= !PORTSC_CCS;
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        let was = self.reg & PORTSC_PED != 0;
        if enabled != was {
            self.reg |= PORTSC_PEDC;
        }
        if enabled {
            self.reg |= PORTSC_PED;
        } else {
            self.reg &= !PORTSC_PED;
        }
    }
}

/// A minimal UHCI host controller model.
///
/// The intent is to provide enough behaviour for an OS UHCI driver to enumerate USB HID devices.
/// It is *not* a full UHCI implementation (no isochronous, no bandwidth accounting, etc.).
pub struct UhciController {
    io_base: u16,
    irq_line: u8,

    usbcmd: u16,
    usbsts: u16,
    usbintr: u16,
    frnum: u16,
    frbaseadd: u32,
    sofmod: u8,

    ports: [PortState; 2],
    bus: UsbBus,
}

impl UhciController {
    pub fn new(io_base: u16, irq_line: u8) -> Self {
        let mut ctrl = Self {
            io_base,
            irq_line,
            usbcmd: 0,
            usbsts: USBSTS_HC_HALT,
            usbintr: 0,
            frnum: 0,
            frbaseadd: 0,
            sofmod: 0x40,
            ports: [PortState::default(), PortState::default()],
            bus: UsbBus::new(2),
        };
        ctrl.reset_controller();
        ctrl
    }

    pub fn bus(&self) -> &UsbBus {
        &self.bus
    }

    pub fn bus_mut(&mut self) -> &mut UsbBus {
        &mut self.bus
    }

    pub fn io_base(&self) -> u16 {
        self.io_base
    }

    pub fn set_io_base(&mut self, io_base: u16) {
        self.io_base = io_base;
    }

    pub fn irq_line(&self) -> u8 {
        self.irq_line
    }

    pub fn set_irq_line(&mut self, irq_line: u8) {
        self.irq_line = irq_line;
    }

    pub fn connect_device(&mut self, port: usize, device: Box<dyn crate::usb::UsbDevice>) {
        self.bus.connect(port, device);
        self.ports[port].set_connected(true);
    }

    pub fn disconnect_device(&mut self, port: usize) {
        self.bus.disconnect(port);
        self.ports[port].set_connected(false);
        self.ports[port].set_enabled(false);
    }

    fn reset_controller(&mut self) {
        // Default to 64-byte maximum packet size; Windows programs this bit, but
        // exposing it as already enabled matches common UHCI implementations.
        self.usbcmd = USBCMD_MAXP;
        self.usbsts = USBSTS_HC_HALT;
        self.usbintr = 0;
        self.frnum = 0;
        self.frbaseadd = 0;
        self.sofmod = 0x40;
    }

    fn running(&self) -> bool {
        self.usbcmd & USBCMD_RUN != 0
    }

    fn update_irq(&mut self, irq: &mut dyn InterruptController) {
        let asserted = (self.usbsts & USBSTS_USBINT != 0 && self.usbintr & USBINTR_IOC != 0)
            || (self.usbsts & USBSTS_USBERRINT != 0 && self.usbintr & USBINTR_TIMEOUT_CRC != 0);

        if asserted {
            irq.raise_irq(self.irq_line);
        } else {
            irq.lower_irq(self.irq_line);
        }
    }

    pub fn port_read(&mut self, port: u16, size: usize) -> u32 {
        let Some(offset) = port.checked_sub(self.io_base) else {
            return 0xFFFF_FFFF;
        };

        let value = match offset {
            REG_USBCMD => self.usbcmd as u32,
            REG_USBSTS => self.usbsts as u32,
            REG_USBINTR => self.usbintr as u32,
            REG_FRNUM => (self.frnum & 0x07FF) as u32,
            REG_FRBASEADD => self.frbaseadd,
            REG_SOFMOD => self.sofmod as u32,
            REG_PORTSC1 => self.ports[0].value(&self.bus, 0) as u32,
            REG_PORTSC2 => self.ports[1].value(&self.bus, 1) as u32,
            _ => 0xFFFF_FFFF,
        };

        match size {
            1 => value & 0xFF,
            2 => value & 0xFFFF,
            4 => value,
            _ => 0xFFFF_FFFF,
        }
    }

    pub fn port_write(
        &mut self,
        port: u16,
        size: usize,
        value: u32,
        irq: &mut dyn InterruptController,
    ) {
        let Some(offset) = port.checked_sub(self.io_base) else {
            return;
        };

        let value16 = (value & 0xFFFF) as u16;
        match (offset, size) {
            (REG_USBCMD, 2) => {
                let was_running = self.running();
                let next_cmd = value16 & USBCMD_WRITABLE_MASK;

                if next_cmd & (USBCMD_HCRESET | USBCMD_GRESET) != 0 {
                    self.reset_controller();
                    self.update_irq(irq);
                    return;
                }

                self.usbcmd = next_cmd;
                if was_running && !self.running() {
                    self.usbsts |= USBSTS_HC_HALT;
                } else if !was_running && self.running() {
                    self.usbsts &= !USBSTS_HC_HALT;
                }
                self.update_irq(irq);
            }
            (REG_USBSTS, 2) => {
                // Write-1-to-clear.
                self.usbsts &= !value16;
                self.update_irq(irq);
            }
            (REG_USBINTR, 2) => {
                self.usbintr = value16 & 0x000F;
                self.update_irq(irq);
            }
            (REG_FRNUM, 2) => {
                self.frnum = value16 & 0x07FF;
            }
            (REG_FRBASEADD, 4) => {
                self.frbaseadd = value & 0xFFFF_F000;
            }
            (REG_SOFMOD, 1) => {
                self.sofmod = (value & 0xFF) as u8;
            }
            (REG_PORTSC1, 2) => self.write_portsc(0, value16),
            (REG_PORTSC2, 2) => self.write_portsc(1, value16),
            _ => {}
        }
    }

    fn write_portsc(&mut self, idx: usize, value: u16) {
        let port = &mut self.ports[idx];

        if value & PORTSC_CSC != 0 {
            port.reg &= !PORTSC_CSC;
        }
        if value & PORTSC_PEDC != 0 {
            port.reg &= !PORTSC_PEDC;
        }

        if value & PORTSC_PR != 0 {
            // Start reset sequence; complete asynchronously in step_frame().
            port.reg |= PORTSC_PR;
            port.reset_timer_ms = 50;
            self.bus.reset_port(idx);
        }

        // Port enable is writable.
        if value & PORTSC_PED != 0 {
            port.set_enabled(true);
            if let Some(p) = self.bus.port_mut(idx) {
                p.enabled = true;
            }
        } else if value & PORTSC_PED == 0 {
            // Some drivers clear PED to disable the port.
            port.set_enabled(false);
            if let Some(p) = self.bus.port_mut(idx) {
                p.enabled = false;
            }
        }
    }

    pub fn step_frame(&mut self, mem: &mut dyn GuestMemory, irq: &mut dyn InterruptController) {
        for (idx, port) in self.ports.iter_mut().enumerate() {
            if port.reset_timer_ms == 0 {
                continue;
            }
            port.reset_timer_ms = port.reset_timer_ms.saturating_sub(1);
            if port.reset_timer_ms == 0 {
                port.reg &= !PORTSC_PR;
                port.set_enabled(true);
                if let Some(p) = self.bus.port_mut(idx) {
                    p.enabled = true;
                }
            }
        }

        if !self.running() || self.frbaseadd == 0 {
            self.usbsts |= USBSTS_HC_HALT;
            self.update_irq(irq);
            return;
        }

        let frame_idx = (self.frnum & 0x03FF) as u32;
        let entry_addr = self.frbaseadd + frame_idx * 4;
        let link = mem.read_u32(entry_addr);

        self.process_link(mem, link);

        self.frnum = (self.frnum + 1) & 0x07FF;
        self.update_irq(irq);
    }

    fn process_link(&mut self, mem: &mut dyn GuestMemory, mut link: u32) {
        let mut traversed = 0usize;
        while traversed < MAX_LINK_TRAVERSAL {
            traversed += 1;
            if link & LINK_PTR_T != 0 {
                break;
            }
            let addr = link & 0xFFFF_FFF0;
            if link & LINK_PTR_Q != 0 {
                let qh = UhciQh::read(mem, addr);
                self.process_qh(mem, addr, qh.element);
                link = qh.head;
                continue;
            }

            let td = UhciTd::read(mem, addr);
            let _ = self.process_td(mem, addr, td);
            link = td.link_ptr;
        }
    }

    fn process_qh(&mut self, mem: &mut dyn GuestMemory, qh_addr: u32, mut element: u32) {
        let mut traversed = 0usize;
        while traversed < MAX_LINK_TRAVERSAL {
            traversed += 1;
            if element & LINK_PTR_T != 0 {
                break;
            }
            let addr = element & 0xFFFF_FFF0;
            if element & LINK_PTR_Q != 0 {
                let qh = UhciQh::read(mem, addr);
                self.process_qh(mem, addr, qh.element);
                element = qh.head;
                continue;
            }

            let td = UhciTd::read(mem, addr);
            if td.ctrl_sts & TD_CTRL_ACTIVE == 0 {
                break;
            }
            match self.process_td(mem, addr, td) {
                TdAdvance::Continue(next) => {
                    element = next;
                    mem.write_u32(qh_addr + 0x04, next);
                }
                TdAdvance::Stop => break,
            }
        }
    }

    fn set_usbint(&mut self) {
        self.usbsts |= USBSTS_USBINT;
    }

    fn set_usberr(&mut self) {
        self.usbsts |= USBSTS_USBERRINT;
    }

    fn process_td(&mut self, mem: &mut dyn GuestMemory, td_addr: u32, td: UhciTd) -> TdAdvance {
        if td.ctrl_sts & TD_CTRL_ACTIVE == 0 {
            return TdAdvance::Stop;
        }

        let pid_raw = (td.token & TD_TOKEN_PID_MASK) as u8;
        let Some(pid) = UsbPid::from_u8(pid_raw) else {
            self.complete_td(mem, td_addr, td.ctrl_sts | TD_CTRL_STALLED, 0);
            self.set_usberr();
            return TdAdvance::Stop;
        };

        let devaddr = ((td.token & TD_TOKEN_DEVADDR_MASK) >> TD_TOKEN_DEVADDR_SHIFT) as u8;
        let endpt = ((td.token & TD_TOKEN_ENDPT_MASK) >> TD_TOKEN_ENDPT_SHIFT) as u8;
        let maxlen_raw = (td.token & TD_TOKEN_MAXLEN_MASK) >> TD_TOKEN_MAXLEN_SHIFT;
        let maxlen = if maxlen_raw == 0x7FF {
            0usize
        } else {
            (maxlen_raw as usize) + 1
        };

        let mut tmp = vec![0u8; maxlen];
        let handshake = match pid {
            UsbPid::Setup => {
                if maxlen < 8 {
                    UsbHandshake::Timeout
                } else {
                    mem.read(td.buffer, &mut tmp[..8]);
                    let mut bytes = [0u8; 8];
                    bytes.copy_from_slice(&tmp[..8]);
                    let setup = SetupPacket::parse(bytes);
                    self.bus.handle_setup(devaddr, setup)
                }
            }
            UsbPid::Out => {
                if maxlen != 0 {
                    mem.read(td.buffer, &mut tmp);
                }
                self.bus.handle_out(devaddr, endpt, &tmp)
            }
            UsbPid::In => self.bus.handle_in(devaddr, endpt, &mut tmp),
        };

        match handshake {
            UsbHandshake::Ack { bytes } => {
                if pid == UsbPid::In && bytes != 0 {
                    mem.write(td.buffer, &tmp[..bytes]);
                }
                self.complete_td(mem, td_addr, td.ctrl_sts, bytes);

                if td.ctrl_sts & TD_CTRL_IOC != 0 {
                    self.set_usbint();
                }
                TdAdvance::Continue(td.link_ptr)
            }
            UsbHandshake::Nak => {
                let mut ctrl = td.ctrl_sts | TD_CTRL_NAK;
                ctrl &= !(TD_CTRL_BITSTUFF
                    | TD_CTRL_CRCERR
                    | TD_CTRL_BABBLE
                    | TD_CTRL_DBUFERR
                    | TD_CTRL_STALLED);
                UhciTd::write_status(mem, td_addr, ctrl);
                TdAdvance::Stop
            }
            UsbHandshake::Stall => {
                let mut ctrl = td.ctrl_sts | TD_CTRL_STALLED;
                ctrl &= !TD_CTRL_ACTIVE;
                ctrl &= !(TD_CTRL_NAK
                    | TD_CTRL_BITSTUFF
                    | TD_CTRL_CRCERR
                    | TD_CTRL_BABBLE
                    | TD_CTRL_DBUFERR);
                ctrl = (ctrl & !TD_CTRL_ACTLEN_MASK) | 0x7FF;
                UhciTd::write_status(mem, td_addr, ctrl);
                self.set_usberr();
                if td.ctrl_sts & TD_CTRL_IOC != 0 {
                    self.set_usbint();
                }
                TdAdvance::Stop
            }
            UsbHandshake::Timeout => {
                let mut ctrl = td.ctrl_sts | TD_CTRL_CRCERR;
                ctrl &= !TD_CTRL_ACTIVE;
                ctrl &= !(TD_CTRL_NAK
                    | TD_CTRL_BITSTUFF
                    | TD_CTRL_BABBLE
                    | TD_CTRL_DBUFERR
                    | TD_CTRL_STALLED);
                ctrl = (ctrl & !TD_CTRL_ACTLEN_MASK) | 0x7FF;
                UhciTd::write_status(mem, td_addr, ctrl);
                self.set_usberr();
                if td.ctrl_sts & TD_CTRL_IOC != 0 {
                    self.set_usbint();
                }
                TdAdvance::Stop
            }
        }
    }

    fn complete_td(
        &mut self,
        mem: &mut dyn GuestMemory,
        td_addr: u32,
        ctrl_sts: u32,
        bytes: usize,
    ) {
        let mut ctrl = ctrl_sts;
        ctrl &= !TD_CTRL_ACTIVE;
        ctrl &= !(TD_CTRL_BITSTUFF
            | TD_CTRL_CRCERR
            | TD_CTRL_NAK
            | TD_CTRL_BABBLE
            | TD_CTRL_DBUFERR
            | TD_CTRL_STALLED);

        let actlen = if bytes == 0 {
            0x7FF
        } else {
            (bytes as u32) - 1
        };
        ctrl = (ctrl & !TD_CTRL_ACTLEN_MASK) | (actlen & TD_CTRL_ACTLEN_MASK);
        UhciTd::write_status(mem, td_addr, ctrl);
    }
}

#[derive(Debug, Clone, Copy)]
enum TdAdvance {
    Continue(u32),
    Stop,
}

/// Minimal PCI config space for the UHCI controller.
///
/// This is a small helper (not currently wired into a full PCI bus) so that higher-level code can
/// expose the correct PCI class/subclass/prog-if values (Serial bus / USB / UHCI).
#[derive(Clone)]
pub struct UhciPciConfig {
    pub vendor_id: u16,
    pub device_id: u16,
    pub class_code: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision_id: u8,
    pub bar_io: u32,
    pub interrupt_line: u8,
    pub interrupt_pin: u8,
}

impl Default for UhciPciConfig {
    fn default() -> Self {
        Self {
            vendor_id: 0x8086, // Intel
            device_id: 0x7112, // PIIX4 UHCI (commonly supported by Windows in-box drivers)
            class_code: 0x0C,
            subclass: 0x03,
            prog_if: 0x00,
            revision_id: 0x01,
            bar_io: 0,
            interrupt_line: 0x0B,
            interrupt_pin: 0x01, // INTA#
        }
    }
}

/// A minimal PCI function wrapper for [`UhciController`].
///
/// Windows 7 includes `usbuhci.sys` and can bind to UHCI controllers purely from PCI class codes.
/// This wrapper models the BAR probe/relocation behaviour required by PCI enumeration code.
pub struct UhciPciDevice {
    config: [u8; 256],
    bar4: u32,
    bar4_probe: bool,
    pub controller: UhciController,
}

impl UhciPciDevice {
    /// UHCI uses an I/O port register block.
    pub const IO_BAR_SIZE: u32 = 0x20;
    /// BAR4 is the traditional location for the UHCI I/O BAR on Intel PIIX/ICH devices.
    const BAR4_OFFSET: usize = 0x20;

    pub fn new(io_base: u16, irq_line: u8, cfg: UhciPciConfig) -> Self {
        let mut config = [0u8; 256];
        config[0x00..0x02].copy_from_slice(&cfg.vendor_id.to_le_bytes());
        config[0x02..0x04].copy_from_slice(&cfg.device_id.to_le_bytes());

        // Revision + class codes.
        config[0x08] = cfg.revision_id;
        config[0x09] = cfg.prog_if;
        config[0x0A] = cfg.subclass;
        config[0x0B] = cfg.class_code;
        config[0x0E] = 0x00; // header type

        // Subsystem IDs: mirror vendor/device by default.
        config[0x2C..0x2E].copy_from_slice(&cfg.vendor_id.to_le_bytes());
        config[0x2E..0x30].copy_from_slice(&cfg.device_id.to_le_bytes());

        let bar4 = (u32::from(io_base) & 0xFFFF_FFFC) | 0x1;
        config[Self::BAR4_OFFSET..Self::BAR4_OFFSET + 4].copy_from_slice(&bar4.to_le_bytes());

        config[0x3C] = irq_line;
        config[0x3D] = cfg.interrupt_pin;

        Self {
            config,
            bar4,
            bar4_probe: false,
            controller: UhciController::new(io_base, irq_line),
        }
    }

    pub fn config_read(&mut self, offset: u16, size: usize) -> u32 {
        assert!(matches!(size, 1 | 2 | 4));
        let off = offset as usize;

        if size == 4 && off == Self::BAR4_OFFSET {
            return if self.bar4_probe {
                // I/O BAR size mask response. Bit0 must remain set.
                (!(Self::IO_BAR_SIZE - 1) & 0xFFFF_FFFC) | 0x1
            } else {
                self.bar4
            };
        }

        let mut value = 0u32;
        for i in 0..size {
            value |= u32::from(self.config[off + i]) << (8 * i);
        }
        value
    }

    pub fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        assert!(matches!(size, 1 | 2 | 4));
        let off = offset as usize;

        if size == 4 && off == Self::BAR4_OFFSET {
            if value == 0xFFFF_FFFF {
                self.bar4_probe = true;
                self.bar4 = 0x1;
            } else {
                self.bar4_probe = false;
                self.bar4 = (value & 0xFFFF_FFFC) | 0x1;
                self.controller
                    .set_io_base((self.bar4 & 0xFFFF) as u16 & !(Self::IO_BAR_SIZE as u16 - 1));
            }
            self.config[off..off + 4].copy_from_slice(&self.bar4.to_le_bytes());
            return;
        }

        if size == 1 && off == 0x3C {
            self.config[off] = value as u8;
            self.controller.set_irq_line(value as u8);
            return;
        }

        for i in 0..size {
            self.config[off + i] = ((value >> (8 * i)) & 0xFF) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usb::{UsbDevice, UsbHandshake};

    struct TestMemory {
        data: Vec<u8>,
    }

    impl TestMemory {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
            }
        }
    }

    impl GuestMemory for TestMemory {
        fn read(&self, addr: u32, buf: &mut [u8]) {
            let addr = addr as usize;
            buf.copy_from_slice(&self.data[addr..addr + buf.len()]);
        }

        fn write(&mut self, addr: u32, buf: &[u8]) {
            let addr = addr as usize;
            self.data[addr..addr + buf.len()].copy_from_slice(buf);
        }
    }

    #[derive(Default)]
    struct TestIrq {
        raised: bool,
        last_irq: Option<u8>,
    }

    impl InterruptController for TestIrq {
        fn raise_irq(&mut self, irq: u8) {
            self.raised = true;
            self.last_irq = Some(irq);
        }

        fn lower_irq(&mut self, _irq: u8) {
            self.raised = false;
        }
    }

    struct SimpleInDevice {
        payload: Vec<u8>,
    }

    impl SimpleInDevice {
        fn new(payload: &[u8]) -> Self {
            Self {
                payload: payload.to_vec(),
            }
        }
    }

    impl UsbDevice for SimpleInDevice {
        fn as_any(&self) -> &dyn core::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
            self
        }

        fn reset(&mut self) {}

        fn address(&self) -> u8 {
            0
        }

        fn handle_setup(&mut self, _setup: SetupPacket) {}

        fn handle_out(&mut self, _ep: u8, _data: &[u8]) -> UsbHandshake {
            UsbHandshake::Ack { bytes: 0 }
        }

        fn handle_in(&mut self, _ep: u8, buf: &mut [u8]) -> UsbHandshake {
            let len = buf.len().min(self.payload.len());
            buf[..len].copy_from_slice(&self.payload[..len]);
            UsbHandshake::Ack { bytes: len }
        }
    }

    #[test]
    fn uhci_processes_qh_td_chain_and_sets_actlen() {
        let io_base = 0x2000;
        let mut ctrl = UhciController::new(io_base, 11);
        ctrl.connect_device(0, Box::new(SimpleInDevice::new(b"ABCD")));

        let mut mem = TestMemory::new(0x8000);
        let mut irq = TestIrq::default();

        // Enable port and interrupts.
        ctrl.port_write(io_base + REG_PORTSC1, 2, PORTSC_PED as u32, &mut irq);
        ctrl.port_write(io_base + REG_USBINTR, 2, USBINTR_IOC as u32, &mut irq);

        // Frame list base.
        ctrl.port_write(io_base + REG_FRBASEADD, 4, 0x1000, &mut irq);
        for i in 0..1024u32 {
            mem.write_u32(0x1000 + i * 4, LINK_PTR_T);
        }
        mem.write_u32(0x1000, 0x2000 | LINK_PTR_Q);

        // Queue head -> TD.
        mem.write_u32(0x2000, LINK_PTR_T);
        mem.write_u32(0x2004, 0x3000);

        // TD: IN to addr0/ep0, 4 bytes.
        let maxlen_field = (4u32 - 1) << TD_TOKEN_MAXLEN_SHIFT;
        let token = 0x69u32
            | (0u32 << TD_TOKEN_DEVADDR_SHIFT)
            | (0u32 << TD_TOKEN_ENDPT_SHIFT)
            | maxlen_field;
        mem.write_u32(0x3000, LINK_PTR_T);
        mem.write_u32(0x3004, TD_CTRL_ACTIVE | TD_CTRL_IOC | 0x7FF);
        mem.write_u32(0x3008, token);
        mem.write_u32(0x300C, 0x4000);

        ctrl.port_write(io_base + REG_USBCMD, 2, USBCMD_RUN as u32, &mut irq);
        ctrl.step_frame(&mut mem, &mut irq);

        let data = &mem.data[0x4000..0x4004];
        assert_eq!(data, b"ABCD");

        // Hardware should advance the QH element pointer as TDs complete.
        assert_eq!(mem.read_u32(0x2004), LINK_PTR_T);

        let ctrl_sts = mem.read_u32(0x3004);
        assert_eq!(ctrl_sts & TD_CTRL_ACTIVE, 0);
        assert_eq!(ctrl_sts & TD_CTRL_ACTLEN_MASK, 3);

        assert!(ctrl.usbsts & USBSTS_USBINT != 0);
        assert!(irq.raised);
        assert_eq!(irq.last_irq, Some(11));
    }

    struct NakThenAckDevice {
        ready: bool,
    }

    impl NakThenAckDevice {
        fn new() -> Self {
            Self { ready: false }
        }
    }

    impl UsbDevice for NakThenAckDevice {
        fn as_any(&self) -> &dyn core::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
            self
        }

        fn reset(&mut self) {
            self.ready = false;
        }

        fn address(&self) -> u8 {
            0
        }

        fn handle_setup(&mut self, _setup: SetupPacket) {}

        fn handle_out(&mut self, _ep: u8, _data: &[u8]) -> UsbHandshake {
            UsbHandshake::Ack { bytes: 0 }
        }

        fn handle_in(&mut self, _ep: u8, buf: &mut [u8]) -> UsbHandshake {
            if !self.ready {
                return UsbHandshake::Nak;
            }
            let payload = [1u8, 2, 3];
            let len = buf.len().min(payload.len());
            buf[..len].copy_from_slice(&payload[..len]);
            UsbHandshake::Ack { bytes: len }
        }
    }

    #[test]
    fn uhci_nak_leaves_td_active_until_data_available() {
        let io_base = 0x3000;
        let mut ctrl = UhciController::new(io_base, 11);
        ctrl.connect_device(0, Box::new(NakThenAckDevice::new()));

        let mut mem = TestMemory::new(0x9000);
        let mut irq = TestIrq::default();

        ctrl.port_write(io_base + REG_PORTSC1, 2, PORTSC_PED as u32, &mut irq);
        ctrl.port_write(io_base + REG_FRBASEADD, 4, 0x1000, &mut irq);
        for i in 0..1024u32 {
            mem.write_u32(0x1000 + i * 4, 0x2000 | LINK_PTR_Q);
        }

        mem.write_u32(0x2000, LINK_PTR_T);
        mem.write_u32(0x2004, 0x3000);

        let maxlen_field = (3u32 - 1) << TD_TOKEN_MAXLEN_SHIFT;
        let token = 0x69u32 | maxlen_field; // IN, addr0/ep0
        mem.write_u32(0x3000, LINK_PTR_T);
        mem.write_u32(0x3004, TD_CTRL_ACTIVE | 0x7FF);
        mem.write_u32(0x3008, token);
        mem.write_u32(0x300C, 0x4000);

        ctrl.port_write(io_base + REG_USBCMD, 2, USBCMD_RUN as u32, &mut irq);

        ctrl.step_frame(&mut mem, &mut irq);
        let ctrl_sts = mem.read_u32(0x3004);
        assert!(ctrl_sts & TD_CTRL_ACTIVE != 0);
        assert!(ctrl_sts & TD_CTRL_NAK != 0);
        assert_eq!(mem.read_u32(0x2004), 0x3000);

        // Mark device ready and ensure the next frame completes the TD.
        let dev = ctrl
            .bus_mut()
            .port_mut(0)
            .unwrap()
            .device
            .as_mut()
            .unwrap()
            .as_any_mut()
            .downcast_mut::<NakThenAckDevice>()
            .unwrap();
        dev.ready = true;

        ctrl.step_frame(&mut mem, &mut irq);
        let ctrl_sts = mem.read_u32(0x3004);
        assert_eq!(ctrl_sts & TD_CTRL_ACTIVE, 0);
        assert_eq!(mem.data[0x4000..0x4003], [1, 2, 3]);
        assert_eq!(mem.read_u32(0x2004), LINK_PTR_T);
    }

    #[test]
    fn pci_bar_probe_and_relocation_updates_controller_io_base() {
        let cfg = UhciPciConfig::default();
        let mut dev = UhciPciDevice::new(0x2000, 11, cfg);

        // BAR probe: write all-1s and read back the size mask.
        dev.config_write(0x20, 4, 0xFFFF_FFFF);
        let mask = dev.config_read(0x20, 4);
        assert_eq!(
            mask,
            (!(UhciPciDevice::IO_BAR_SIZE - 1) & 0xFFFF_FFFC) | 0x1
        );

        // Relocate BAR4.
        dev.config_write(0x20, 4, 0x4000);
        assert_eq!(dev.config_read(0x20, 4), 0x4001);
        assert_eq!(dev.controller.io_base(), 0x4000);
    }
}
