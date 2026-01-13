//! Minimal EHCI (USB 2.0) host controller model.
//!
//! This is intentionally a *bring-up* implementation: it models the capability/operational MMIO
//! registers and an EHCI root hub with per-port state machines. The schedule engine is stubbed out
//! and will be implemented by follow-up tasks (EHCI-003/004).

mod hub;
pub use hub::RootHub;

pub mod regs;

use crate::memory::MemoryBus;

use regs::*;

/// Default number of EHCI root hub ports.
///
/// We currently model **6** ports, which is a common configuration for PC-style EHCI controllers.
pub const DEFAULT_PORT_COUNT: usize = 6;

#[derive(Clone, Copy, Debug)]
struct EhciRegs {
    usbcmd: u32,
    usbsts: u32,
    usbintr: u32,
    frindex: u32,
    ctrldssegment: u32,
    periodiclistbase: u32,
    asynclistaddr: u32,
    configflag: u32,
}

impl EhciRegs {
    fn new() -> Self {
        let mut regs = Self {
            usbcmd: 0,
            usbsts: USBSTS_HCHALTED,
            usbintr: 0,
            frindex: 0,
            ctrldssegment: 0,
            periodiclistbase: 0,
            asynclistaddr: 0,
            configflag: 0,
        };
        regs.update_halted();
        regs
    }

    fn update_halted(&mut self) {
        if self.usbcmd & USBCMD_RS != 0 {
            self.usbsts &= !USBSTS_HCHALTED;
        } else {
            self.usbsts |= USBSTS_HCHALTED;
        }
    }
}

pub struct EhciController {
    regs: EhciRegs,
    hub: RootHub,
    irq_level: bool,
}

impl EhciController {
    pub fn new() -> Self {
        Self::new_with_port_count(DEFAULT_PORT_COUNT)
    }

    pub fn new_with_port_count(port_count: usize) -> Self {
        Self {
            regs: EhciRegs::new(),
            hub: RootHub::new(port_count),
            irq_level: false,
        }
    }

    pub fn irq_level(&self) -> bool {
        self.irq_level
    }

    pub fn hub_mut(&mut self) -> &mut RootHub {
        &mut self.hub
    }

    pub fn hub(&self) -> &RootHub {
        &self.hub
    }

    fn hcsparams(&self) -> u32 {
        // EHCI 1.0 spec: HCSPARAMS.N_PORTS (bits 0..=3) + PPC (bit 4).
        let n_ports = (self.hub.num_ports() as u32) & 0x0f;
        n_ports | (1 << 4)
    }

    fn hccparams(&self) -> u32 {
        // Provide a minimal-but-plausible HCCPARAMS:
        // - No 64-bit addressing (bit 0 = 0).
        // - Programmable Frame List Flag (bit 1 = 1).
        // - Asynchronous Schedule Park Capability (bit 2 = 1).
        // - No extended capabilities (EECP=0).
        0x0000_0006
    }

    fn reset_regs(&mut self) {
        self.regs = EhciRegs::new();
        self.irq_level = false;
    }

    fn update_irq(&mut self) {
        // Latch Port Change Detect if any port has pending change bits.
        if self.hub.any_port_change() {
            self.regs.usbsts |= USBSTS_PCD;
        }

        self.regs.update_halted();

        let pending = (self.regs.usbsts & USBSTS_IRQ_MASK) & (self.regs.usbintr & USBINTR_MASK);
        self.irq_level = pending != 0;
    }

    fn write_usbcmd(&mut self, value: u32) {
        if value & USBCMD_HCRESET != 0 {
            // Host Controller Reset. We reset operational state but preserve attached devices and
            // port connection state.
            self.reset_regs();
            return;
        }

        self.regs.usbcmd = value & USBCMD_WRITE_MASK;
        self.regs.update_halted();
    }

    fn write_usbsts_masked(&mut self, value: u32, write_mask: u32) {
        // USBSTS is mostly write-1-to-clear.
        let w1c = value & write_mask & USBSTS_W1C_MASK;
        self.regs.usbsts &= !w1c;
        self.regs.usbsts &= USBSTS_READ_MASK;
        self.regs.update_halted();
    }

    fn write_usbintr(&mut self, value: u32) {
        self.regs.usbintr = value & USBINTR_MASK;
    }

    fn write_frindex(&mut self, value: u32) {
        self.regs.frindex = value & FRINDEX_MASK;
    }

    fn write_ctrldssegment(&mut self, _value: u32) {
        // We model a 32-bit addressing controller (HCCPARAMS.AC64=0); CTRLDSSEGMENT is unused.
        self.regs.ctrldssegment = 0;
    }

    fn write_periodiclistbase(&mut self, value: u32) {
        self.regs.periodiclistbase = value & PERIODICLISTBASE_MASK;
    }

    fn write_asynclistaddr(&mut self, value: u32) {
        self.regs.asynclistaddr = value & ASYNCLISTADDR_MASK;
    }

    fn write_configflag(&mut self, value: u32) {
        self.regs.configflag = value & CONFIGFLAG_CF;
    }

    fn mmio_read_u8(&self, offset: u64) -> u8 {
        // Capability register dword 0 (CAPLENGTH / HCIVERSION).
        let cap0: u32 = (CAPLENGTH as u32) | ((HCIVERSION as u32) << 16);

        if offset >= REG_CAPLENGTH_HCIVERSION && offset < REG_CAPLENGTH_HCIVERSION + 4 {
            let shift = (offset - REG_CAPLENGTH_HCIVERSION) * 8;
            return (cap0 >> shift) as u8;
        }
        if offset >= REG_HCSPARAMS && offset < REG_HCSPARAMS + 4 {
            let shift = (offset - REG_HCSPARAMS) * 8;
            return (self.hcsparams() >> shift) as u8;
        }
        if offset >= REG_HCCPARAMS && offset < REG_HCCPARAMS + 4 {
            let shift = (offset - REG_HCCPARAMS) * 8;
            return (self.hccparams() >> shift) as u8;
        }
        if offset >= REG_HCSP_PORTROUTE && offset < REG_HCSP_PORTROUTE + 4 {
            return 0;
        }

        if offset >= REG_USBCMD && offset < REG_USBCMD + 4 {
            let shift = (offset - REG_USBCMD) * 8;
            return (self.regs.usbcmd >> shift) as u8;
        }
        if offset >= REG_USBSTS && offset < REG_USBSTS + 4 {
            let shift = (offset - REG_USBSTS) * 8;
            let v = self.regs.usbsts & USBSTS_READ_MASK;
            return (v >> shift) as u8;
        }
        if offset >= REG_USBINTR && offset < REG_USBINTR + 4 {
            let shift = (offset - REG_USBINTR) * 8;
            return (self.regs.usbintr >> shift) as u8;
        }
        if offset >= REG_FRINDEX && offset < REG_FRINDEX + 4 {
            let shift = (offset - REG_FRINDEX) * 8;
            return (self.regs.frindex >> shift) as u8;
        }
        if offset >= REG_CTRLDSSEGMENT && offset < REG_CTRLDSSEGMENT + 4 {
            let shift = (offset - REG_CTRLDSSEGMENT) * 8;
            return (self.regs.ctrldssegment >> shift) as u8;
        }
        if offset >= REG_PERIODICLISTBASE && offset < REG_PERIODICLISTBASE + 4 {
            let shift = (offset - REG_PERIODICLISTBASE) * 8;
            return (self.regs.periodiclistbase >> shift) as u8;
        }
        if offset >= REG_ASYNCLISTADDR && offset < REG_ASYNCLISTADDR + 4 {
            let shift = (offset - REG_ASYNCLISTADDR) * 8;
            return (self.regs.asynclistaddr >> shift) as u8;
        }
        if offset >= REG_CONFIGFLAG && offset < REG_CONFIGFLAG + 4 {
            let shift = (offset - REG_CONFIGFLAG) * 8;
            return (self.regs.configflag >> shift) as u8;
        }

        // Root hub port registers.
        if offset >= REG_PORTSC_BASE {
            let port = ((offset - REG_PORTSC_BASE) / 4) as usize;
            let off_in_port = (offset - REG_PORTSC_BASE) % 4;
            if port < self.hub.num_ports() {
                let v = self.hub.read_portsc(port);
                return (v >> (off_in_port * 8)) as u8;
            }
        }

        // The EHCI register block is typically 0x100 bytes. Offsets inside this region but not
        // explicitly modelled are reserved and read back as 0; out-of-range reads are treated as
        // open bus.
        const EHCI_MMIO_SIZE: u64 = 0x100;
        if offset < EHCI_MMIO_SIZE {
            0
        } else {
            0xff
        }
    }

    fn mmio_write_u8(&mut self, offset: u64, value: u8) {
        if offset >= REG_USBCMD && offset < REG_USBCMD + 4 {
            let shift = (offset - REG_USBCMD) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.usbcmd & mask) | ((value as u32) << shift);
            self.write_usbcmd(v);
            return;
        }
        if offset >= REG_USBSTS && offset < REG_USBSTS + 4 {
            // Masked write to avoid high-byte stores inadvertently clearing W1C bits in the low
            // byte if software performs read-modify-write sequences.
            let shift = (offset - REG_USBSTS) * 8;
            self.write_usbsts_masked((value as u32) << shift, 0xffu32 << shift);
            return;
        }
        if offset >= REG_USBINTR && offset < REG_USBINTR + 4 {
            let shift = (offset - REG_USBINTR) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.usbintr & mask) | ((value as u32) << shift);
            self.write_usbintr(v);
            return;
        }
        if offset >= REG_FRINDEX && offset < REG_FRINDEX + 4 {
            let shift = (offset - REG_FRINDEX) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.frindex & mask) | ((value as u32) << shift);
            self.write_frindex(v);
            return;
        }
        if offset >= REG_CTRLDSSEGMENT && offset < REG_CTRLDSSEGMENT + 4 {
            let shift = (offset - REG_CTRLDSSEGMENT) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.ctrldssegment & mask) | ((value as u32) << shift);
            self.write_ctrldssegment(v);
            return;
        }
        if offset >= REG_PERIODICLISTBASE && offset < REG_PERIODICLISTBASE + 4 {
            let shift = (offset - REG_PERIODICLISTBASE) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.periodiclistbase & mask) | ((value as u32) << shift);
            self.write_periodiclistbase(v);
            return;
        }
        if offset >= REG_ASYNCLISTADDR && offset < REG_ASYNCLISTADDR + 4 {
            let shift = (offset - REG_ASYNCLISTADDR) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.asynclistaddr & mask) | ((value as u32) << shift);
            self.write_asynclistaddr(v);
            return;
        }
        if offset >= REG_CONFIGFLAG && offset < REG_CONFIGFLAG + 4 {
            let shift = (offset - REG_CONFIGFLAG) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.configflag & mask) | ((value as u32) << shift);
            self.write_configflag(v);
            return;
        }

        if offset >= REG_PORTSC_BASE {
            let port = ((offset - REG_PORTSC_BASE) / 4) as usize;
            let off_in_port = (offset - REG_PORTSC_BASE) % 4;
            if port < self.hub.num_ports() {
                self.hub.write_portsc_masked(
                    port,
                    (value as u32) << (off_in_port * 8),
                    0xffu32 << (off_in_port * 8),
                );
            }
        }
    }

    pub fn mmio_read(&self, offset: u64, size: usize) -> u32 {
        let mut out = 0u32;
        for i in 0..size.min(4) {
            out |= (self.mmio_read_u8(offset.wrapping_add(i as u64)) as u32) << (i * 8);
        }
        out
    }

    pub fn mmio_write(&mut self, offset: u64, size: usize, value: u32) {
        match (offset, size) {
            (REG_USBCMD, 4) => self.write_usbcmd(value),
            (REG_USBSTS, 4) => self.write_usbsts_masked(value, 0xffff_ffff),
            (REG_USBINTR, 4) => self.write_usbintr(value),
            (REG_FRINDEX, 4) => self.write_frindex(value),
            (REG_CTRLDSSEGMENT, 4) => self.write_ctrldssegment(value),
            (REG_PERIODICLISTBASE, 4) => self.write_periodiclistbase(value),
            (REG_ASYNCLISTADDR, 4) => self.write_asynclistaddr(value),
            (REG_CONFIGFLAG, 4) => self.write_configflag(value),
            _ if offset >= REG_PORTSC_BASE && size == 4 => {
                let port = ((offset - REG_PORTSC_BASE) / 4) as usize;
                if port < self.hub.num_ports() && offset == reg_portsc(port) {
                    self.hub.write_portsc(port, value);
                } else {
                    for i in 0..size.min(4) {
                        let byte = ((value >> (i * 8)) & 0xff) as u8;
                        self.mmio_write_u8(offset.wrapping_add(i as u64), byte);
                    }
                }
            }
            _ => {
                for i in 0..size.min(4) {
                    let byte = ((value >> (i * 8)) & 0xff) as u8;
                    self.mmio_write_u8(offset.wrapping_add(i as u64), byte);
                }
            }
        }

        self.update_irq();
    }

    fn process_schedules(&mut self, mem: &mut dyn MemoryBus) {
        // EHCI-003/004 will add real schedule walking. For now, this is a stub that is safe even
        // when the schedule base pointers are 0.
        let _ = mem;
    }

    pub fn tick_1ms(&mut self, mem: &mut dyn MemoryBus) {
        self.hub.tick_1ms();

        if self.regs.usbcmd & USBCMD_RS != 0 {
            // FRINDEX is a microframe counter. We tick in 1ms increments, so add 8 microframes so
            // the microframe bits (0..=2) remain 0 at tick boundaries.
            self.regs.frindex = self.regs.frindex.wrapping_add(8) & FRINDEX_MASK;

            if self.regs.usbcmd & (USBCMD_PSE | USBCMD_ASE) != 0 {
                self.process_schedules(mem);
            }
        }

        self.update_irq();
    }
}

impl Default for EhciController {
    fn default() -> Self {
        Self::new()
    }
}
