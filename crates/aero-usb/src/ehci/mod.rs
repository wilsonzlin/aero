//! Minimal EHCI (USB 2.0) host controller model.
//!
//! Design notes + emulator contracts: see `docs/usb-ehci.md`.
//!
//! This is intentionally a *bring-up* implementation: it models the capability/operational MMIO
//! registers and an EHCI root hub with per-port state machines. Minimal schedule engines are
//! implemented for:
//! - the **asynchronous** schedule (QH/qTD), used for high-speed control + bulk/interrupt transfers
//!   (sufficient for Windows enumeration and WebUSB passthrough)
//! - the **periodic** schedule (frame list + interrupt QH/qTD), used for HID-style polling
//!
//! ## Companion controller handoff
//!
//! Real EHCI controllers expose port-routing knobs so firmware/OSes can decide whether a root hub
//! port is serviced by EHCI or by a "companion" OHCI/UHCI controller:
//! - `CONFIGFLAG` (global configure flag)
//! - `PORTSC[n].PORT_OWNER` (per-port owner bit)
//!
//! This model implements the guest-visible semantics needed by Windows/Linux EHCI drivers:
//! - `CONFIGFLAG` is read/write, and on 0→1 claims all ports for EHCI (clears `PORT_OWNER`).
//! - Clearing `CONFIGFLAG` (1→0) releases all ports back to companion ownership (sets `PORT_OWNER`).
//! - Writes that change `PORT_OWNER` assert `USBSTS.PCD` (Port Change Detect).
//!
//! ## Schedule robustness
//!
//! EHCI schedule structures live in guest memory and are therefore entirely guest-controlled. This
//! model defends against malformed or adversarial schedules by enforcing strict per-tick traversal
//! bounds and cycle detection. On detecting a runaway schedule, the controller reports a Host
//! System Error (`USBSTS.HSE`) and halts.

mod hub;
pub use hub::RootHub;

mod schedule;
mod schedule_async;
mod schedule_periodic;

pub mod regs;

use crate::memory::MemoryBus;
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use regs::*;
use schedule_async::{process_async_schedule, AsyncScheduleContext};
use schedule_periodic::{process_periodic_frame, PeriodicScheduleContext};

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
        let running = (self.usbcmd & USBCMD_RS) != 0;
        if running {
            self.usbsts &= !USBSTS_HCHALTED;
        } else {
            self.usbsts |= USBSTS_HCHALTED;
        }

        // EHCI schedule status bits reflect whether each schedule is enabled and the controller is
        // running. Many OS drivers (including Windows/Linux) poll these bits after toggling
        // USBCMD.PSE/ASE.
        if running && (self.usbcmd & USBCMD_PSE) != 0 {
            self.usbsts |= USBSTS_PSS;
        } else {
            self.usbsts &= !USBSTS_PSS;
        }
        if running && (self.usbcmd & USBCMD_ASE) != 0 {
            self.usbsts |= USBSTS_ASS;
        } else {
            self.usbsts &= !USBSTS_ASS;
        }
    }

    fn advance_1ms(&mut self) {
        // FRINDEX is a microframe counter. We tick in 1ms increments, so add 8 microframes so the
        // microframe bits (0..=2) remain 0 at tick boundaries.
        let prev = self.frindex;
        let next = self.frindex.wrapping_add(8) & FRINDEX_MASK;
        if next < prev {
            // Frame List Rollover (FLR) is a W1C status bit that latches when FRINDEX wraps.
            self.usbsts |= USBSTS_FLR;
        }
        self.frindex = next;
    }
}

pub struct EhciController {
    regs: EhciRegs,
    hub: RootHub,
    irq_level: bool,
    usblegsup: u32,
    usblegctlsts: u32,
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
            // Start with BIOS ownership so guest OS stacks that implement the EHCI "BIOS handoff"
            // sequence can request ownership via the OS semaphore.
            usblegsup: USBLEGSUP_HEADER | USBLEGSUP_BIOS_SEM,
            usblegctlsts: 0,
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

    /// Traverse attached USB topology and clear any host-side asynchronous state that cannot be
    /// resumed after restoring a snapshot (e.g. WebUSB passthrough actions backed by JS Promises).
    ///
    /// This does not alter guest-visible USB state.
    pub fn reset_host_state_for_restore(&mut self) {
        let hub = self.hub_mut();
        for port in 0..hub.num_ports() {
            if let Some(mut dev) = hub.port_device_mut(port) {
                dev.reset_host_state_for_restore();
            }
        }
    }

    /// Forces status bits in USBSTS for tests and diagnostics.
    ///
    /// Reserved bits are masked out; the HCHALTED bit is driven by `USBCMD.RS` and should not be
    /// set manually.
    pub fn set_usbsts_bits(&mut self, bits: u32) {
        let bits = bits & (USBSTS_READ_MASK & !USBSTS_HCHALTED);
        self.regs.usbsts |= bits;
        self.update_irq();
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
        // - EECP points at the USB Legacy Support extended capability.
        0x0000_0006 | ((EECP_OFFSET as u32) << HCCPARAMS_EECP_SHIFT)
    }

    fn reset_regs(&mut self) {
        self.regs = EhciRegs::new();
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

    fn schedule_fault(&mut self, _err: schedule::ScheduleError) {
        // Treat schedule traversal runaway/cycles as a Host System Error. This is an observable
        // error condition for the guest (USBSTS.HSE) and is severe enough that real controllers
        // typically halt.
        self.regs.usbsts |= USBSTS_HSE | USBSTS_USBERRINT;

        // Halt the controller to avoid re-walking the same malformed schedule every tick.
        self.regs.usbcmd &= !USBCMD_RS;
        self.regs.update_halted();
        self.update_irq();
    }

    fn write_usbcmd(&mut self, value: u32) {
        if value & USBCMD_HCRESET != 0 {
            // Host Controller Reset. We reset operational state but preserve attached devices and
            // port connection state.
            //
            // Reset also clears CONFIGFLAG, which on real hardware routes ports back to companion
            // controllers until the guest re-claims them. Propagate this default routing decision
            // to any mux-backed ports so machine-level resets don't leave the mux stuck in the
            // previous CONFIGFLAG state.
            self.reset_regs();
            self.hub.set_configflag(false);
            self.hub.set_all_port_owner(true);
            return;
        }

        // USBCMD.IAAD is a "doorbell" bit: software sets it and hardware clears it after the async
        // advance interrupt has been posted. Guests may write USBCMD multiple times while waiting
        // for completion (e.g. toggling PSE/ASE), so we treat IAAD as *set-only* from the software
        // perspective and preserve it across writes until the controller services it.
        let preserve_iaad = self.regs.usbcmd & USBCMD_IAAD;
        let mut cmd = value & (USBCMD_WRITE_MASK & !USBCMD_IAAD);
        if (value & USBCMD_IAAD) != 0 || preserve_iaad != 0 {
            cmd |= USBCMD_IAAD;
        }
        self.regs.usbcmd = cmd;
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
        let prev = self.regs.configflag & CONFIGFLAG_CF;
        let next = value & CONFIGFLAG_CF;
        self.regs.configflag = next;

        // On real hardware, CONFIGFLAG is used to route *all* ports to EHCI (1) or to companion
        // controllers (0). We model this by toggling PORTSC.PORT_OWNER on all ports.
        if prev == next {
            return;
        }

        // Propagate CONFIGFLAG routing changes to any mux-backed ports.
        self.hub.set_configflag(next != 0);

        let changed = if prev == 0 && next != 0 {
            // Claim ports for EHCI: clear PORT_OWNER.
            self.hub.set_all_port_owner(false)
        } else if prev != 0 && next == 0 {
            // Release ports to companion: set PORT_OWNER.
            self.hub.set_all_port_owner(true)
        } else {
            false
        };

        if changed {
            self.regs.usbsts |= USBSTS_PCD;
        }
    }

    fn mmio_read_u8(&self, offset: u64) -> u8 {
        // Capability register dword 0 (CAPLENGTH / HCIVERSION).
        let cap0: u32 = (CAPLENGTH as u32) | ((HCIVERSION as u32) << 16);

        if (REG_CAPLENGTH_HCIVERSION..REG_CAPLENGTH_HCIVERSION + 4).contains(&offset) {
            let shift = (offset - REG_CAPLENGTH_HCIVERSION) * 8;
            return (cap0 >> shift) as u8;
        }
        if (REG_HCSPARAMS..REG_HCSPARAMS + 4).contains(&offset) {
            let shift = (offset - REG_HCSPARAMS) * 8;
            return (self.hcsparams() >> shift) as u8;
        }
        if (REG_HCCPARAMS..REG_HCCPARAMS + 4).contains(&offset) {
            let shift = (offset - REG_HCCPARAMS) * 8;
            return (self.hccparams() >> shift) as u8;
        }
        if (REG_HCSP_PORTROUTE..REG_HCSP_PORTROUTE + 4).contains(&offset) {
            return 0;
        }

        if (REG_USBCMD..REG_USBCMD + 4).contains(&offset) {
            let shift = (offset - REG_USBCMD) * 8;
            return (self.regs.usbcmd >> shift) as u8;
        }
        if (REG_USBSTS..REG_USBSTS + 4).contains(&offset) {
            let shift = (offset - REG_USBSTS) * 8;
            // USBSTS.PSS/ASS are read-only schedule status bits. Keep them fully derived from
            // USBCMD so they cannot be latched by internal state (e.g. snapshots/tests).
            let mut v = self.regs.usbsts & (USBSTS_READ_MASK & !(USBSTS_PSS | USBSTS_ASS));
            // Derive schedule-status bits (EHCI USBSTS.PSS/ASS) from the command register. These
            // are read-only in hardware and allow drivers to observe whether schedules are active.
            if self.regs.usbcmd & USBCMD_RS != 0 {
                if self.regs.usbcmd & USBCMD_PSE != 0 {
                    v |= USBSTS_PSS;
                }
                if self.regs.usbcmd & USBCMD_ASE != 0 {
                    v |= USBSTS_ASS;
                }
            }
            v &= USBSTS_READ_MASK;
            return (v >> shift) as u8;
        }
        if (REG_USBINTR..REG_USBINTR + 4).contains(&offset) {
            let shift = (offset - REG_USBINTR) * 8;
            return (self.regs.usbintr >> shift) as u8;
        }
        if (REG_FRINDEX..REG_FRINDEX + 4).contains(&offset) {
            let shift = (offset - REG_FRINDEX) * 8;
            return (self.regs.frindex >> shift) as u8;
        }
        if (REG_CTRLDSSEGMENT..REG_CTRLDSSEGMENT + 4).contains(&offset) {
            let shift = (offset - REG_CTRLDSSEGMENT) * 8;
            return (self.regs.ctrldssegment >> shift) as u8;
        }
        if (REG_PERIODICLISTBASE..REG_PERIODICLISTBASE + 4).contains(&offset) {
            let shift = (offset - REG_PERIODICLISTBASE) * 8;
            return (self.regs.periodiclistbase >> shift) as u8;
        }
        if (REG_ASYNCLISTADDR..REG_ASYNCLISTADDR + 4).contains(&offset) {
            let shift = (offset - REG_ASYNCLISTADDR) * 8;
            return (self.regs.asynclistaddr >> shift) as u8;
        }
        if (REG_CONFIGFLAG..REG_CONFIGFLAG + 4).contains(&offset) {
            let shift = (offset - REG_CONFIGFLAG) * 8;
            return (self.regs.configflag >> shift) as u8;
        }

        // EHCI extended capabilities (USB Legacy Support).
        if (REG_USBLEGSUP..REG_USBLEGSUP + 4).contains(&offset) {
            let shift = (offset - REG_USBLEGSUP) * 8;
            return (self.usblegsup >> shift) as u8;
        }
        if (REG_USBLEGCTLSTS..REG_USBLEGCTLSTS + 4).contains(&offset) {
            let shift = (offset - REG_USBLEGCTLSTS) * 8;
            return (self.usblegctlsts >> shift) as u8;
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
        if (REG_USBCMD..REG_USBCMD + 4).contains(&offset) {
            let shift = (offset - REG_USBCMD) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.usbcmd & mask) | ((value as u32) << shift);
            self.write_usbcmd(v);
            return;
        }
        if (REG_USBSTS..REG_USBSTS + 4).contains(&offset) {
            // Masked write to avoid high-byte stores inadvertently clearing W1C bits in the low
            // byte if software performs read-modify-write sequences.
            let shift = (offset - REG_USBSTS) * 8;
            self.write_usbsts_masked((value as u32) << shift, 0xffu32 << shift);
            return;
        }
        if (REG_USBINTR..REG_USBINTR + 4).contains(&offset) {
            let shift = (offset - REG_USBINTR) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.usbintr & mask) | ((value as u32) << shift);
            self.write_usbintr(v);
            return;
        }
        if (REG_FRINDEX..REG_FRINDEX + 4).contains(&offset) {
            let shift = (offset - REG_FRINDEX) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.frindex & mask) | ((value as u32) << shift);
            self.write_frindex(v);
            return;
        }
        if (REG_CTRLDSSEGMENT..REG_CTRLDSSEGMENT + 4).contains(&offset) {
            let shift = (offset - REG_CTRLDSSEGMENT) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.ctrldssegment & mask) | ((value as u32) << shift);
            self.write_ctrldssegment(v);
            return;
        }
        if (REG_PERIODICLISTBASE..REG_PERIODICLISTBASE + 4).contains(&offset) {
            let shift = (offset - REG_PERIODICLISTBASE) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.periodiclistbase & mask) | ((value as u32) << shift);
            self.write_periodiclistbase(v);
            return;
        }
        if (REG_ASYNCLISTADDR..REG_ASYNCLISTADDR + 4).contains(&offset) {
            let shift = (offset - REG_ASYNCLISTADDR) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.asynclistaddr & mask) | ((value as u32) << shift);
            self.write_asynclistaddr(v);
            return;
        }
        if (REG_CONFIGFLAG..REG_CONFIGFLAG + 4).contains(&offset) {
            let shift = (offset - REG_CONFIGFLAG) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.regs.configflag & mask) | ((value as u32) << shift);
            self.write_configflag(v);
            return;
        }

        // EHCI extended capabilities (USB Legacy Support).
        if (REG_USBLEGSUP..REG_USBLEGSUP + 4).contains(&offset) {
            let shift = (offset - REG_USBLEGSUP) * 8;
            let mask = !(0xffu32 << shift);
            let v = (self.usblegsup & mask) | ((value as u32) << shift);

            // CAPID/NEXT are read-only; only the semaphore bits are writable.
            self.usblegsup = (v & USBLEGSUP_RW_MASK) | USBLEGSUP_HEADER;

            // When OS-owned is set, emulate BIOS handoff by clearing BIOS-owned.
            if self.usblegsup & USBLEGSUP_OS_SEM != 0 {
                self.usblegsup &= !USBLEGSUP_BIOS_SEM;
            }
            return;
        }
        if (REG_USBLEGCTLSTS..REG_USBLEGCTLSTS + 4).contains(&offset) {
            let shift = (offset - REG_USBLEGCTLSTS) * 8;
            let mask = !(0xffu32 << shift);
            self.usblegctlsts = (self.usblegctlsts & mask) | ((value as u32) << shift);
            return;
        }

        if offset >= REG_PORTSC_BASE {
            let port = ((offset - REG_PORTSC_BASE) / 4) as usize;
            let off_in_port = (offset - REG_PORTSC_BASE) % 4;
            if port < self.hub.num_ports() {
                let old_owner = (self.hub.read_portsc(port) & PORTSC_PO) != 0;
                self.hub.write_portsc_masked(
                    port,
                    (value as u32) << (off_in_port * 8),
                    0xffu32 << (off_in_port * 8),
                );
                let new_owner = (self.hub.read_portsc(port) & PORTSC_PO) != 0;
                if old_owner != new_owner {
                    self.regs.usbsts |= USBSTS_PCD;
                }
            }
        }
    }

    pub fn mmio_read(&self, offset: u64, size: usize) -> u32 {
        let size = size.min(4);
        if size == 0 {
            return 0;
        }

        // Treat invalid/out-of-range reads as open bus. Do not allow `offset + i` to wrap around to
        // low MMIO offsets (which would alias valid registers) when callers pass large/overflowing
        // offsets.
        let open_bus = if size >= 4 {
            u32::MAX
        } else {
            (1u32 << (size * 8)) - 1
        };
        let Some(end) = offset.checked_add(size as u64) else {
            return open_bus;
        };
        if end > u64::from(regs::MMIO_SIZE) {
            return open_bus;
        }

        let mut out = 0u32;
        for i in 0..size {
            let off = offset + i as u64;
            out |= (self.mmio_read_u8(off) as u32) << (i * 8);
        }
        out & open_bus
    }

    pub fn mmio_write(&mut self, offset: u64, size: usize, value: u32) {
        let size_bytes = size.min(4);
        if size_bytes == 0 {
            return;
        }
        // Treat out-of-range writes as no-ops. Use checked arithmetic so callers cannot cause MMIO
        // offset wraparound (e.g. writing to `u64::MAX - 1` would otherwise alias offsets 0..=2).
        let Some(end) = offset.checked_add(size_bytes as u64) else {
            return;
        };
        if end > u64::from(regs::MMIO_SIZE) {
            return;
        }

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
                    let old_owner = (self.hub.read_portsc(port) & PORTSC_PO) != 0;
                    self.hub.write_portsc(port, value);
                    let new_owner = (self.hub.read_portsc(port) & PORTSC_PO) != 0;
                    if old_owner != new_owner {
                        self.regs.usbsts |= USBSTS_PCD;
                    }
                } else {
                    for i in 0..size_bytes {
                        let byte = ((value >> (i * 8)) & 0xff) as u8;
                        self.mmio_write_u8(offset + i as u64, byte);
                    }
                }
            }
            _ => {
                for i in 0..size_bytes {
                    let byte = ((value >> (i * 8)) & 0xff) as u8;
                    self.mmio_write_u8(offset + i as u64, byte);
                }
            }
        }

        self.update_irq();
    }

    fn process_schedules(
        &mut self,
        mem: &mut dyn MemoryBus,
    ) -> Result<(), schedule::ScheduleError> {
        if self.regs.usbcmd & USBCMD_ASE != 0 && self.regs.asynclistaddr != 0 {
            let mut ctx = AsyncScheduleContext {
                mem,
                hub: &mut self.hub,
                usbsts: &mut self.regs.usbsts,
            };
            process_async_schedule(&mut ctx, self.regs.asynclistaddr)?;
        }

        // Periodic schedule (interrupt endpoints) using PERIODICLISTBASE + FRINDEX.
        //
        // This is still guest-controlled memory; `process_periodic_frame` is responsible for
        // enforcing traversal bounds and reporting schedule faults.
        if (self.regs.usbcmd & USBCMD_PSE) != 0 && self.regs.periodiclistbase != 0 {
            let mut ctx = PeriodicScheduleContext {
                mem,
                hub: &mut self.hub,
                usbsts: &mut self.regs.usbsts,
            };
            process_periodic_frame(&mut ctx, self.regs.periodiclistbase, self.regs.frindex)?;
        }

        Ok(())
    }

    fn service_async_advance_doorbell(&mut self) {
        if self.regs.usbcmd & USBCMD_IAAD == 0 {
            return;
        }

        // Deterministically service the doorbell at the end of a tick after the schedule engine
        // has had a chance to observe any async list modifications.
        self.regs.usbcmd &= !USBCMD_IAAD;
        self.regs.usbsts |= USBSTS_IAA;
    }

    pub fn tick_1ms(&mut self, mem: &mut dyn MemoryBus) {
        self.hub.tick_1ms();

        if self.regs.usbcmd & USBCMD_RS != 0 {
            // When PCI Bus Master Enable is cleared, integrations typically provide a MemoryBus
            // adapter that returns open-bus reads (0xFF) and ignores writes. Avoid interpreting that
            // open-bus data as real schedule structures by skipping all schedule processing while
            // DMA is disabled.
            if mem.dma_enabled() {
                if self.regs.usbcmd & (USBCMD_PSE | USBCMD_ASE) != 0 {
                    if let Err(err) = self.process_schedules(mem) {
                        self.schedule_fault(err);
                        return;
                    }
                }

                self.service_async_advance_doorbell();
            }
            self.regs.advance_1ms();
        }

        self.update_irq();
    }
}

impl Default for EhciController {
    fn default() -> Self {
        Self::new()
    }
}

impl IoSnapshot for EhciController {
    const DEVICE_ID: [u8; 4] = *b"EHCI";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_USBCMD: u16 = 1;
        const TAG_USBSTS: u16 = 2;
        const TAG_USBINTR: u16 = 3;
        const TAG_FRINDEX: u16 = 4;
        const TAG_PERIODICLISTBASE: u16 = 5;
        const TAG_ASYNCLISTADDR: u16 = 6;
        const TAG_CONFIGFLAG: u16 = 7;
        const TAG_ROOT_HUB_PORTS: u16 = 8;
        const TAG_CTRLDSSEGMENT: u16 = 9;
        const TAG_USBLEGSUP: u16 = 10;
        const TAG_USBLEGCTLSTS: u16 = 11;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u32(TAG_USBCMD, self.regs.usbcmd);
        w.field_u32(TAG_USBSTS, self.regs.usbsts);
        w.field_u32(TAG_USBINTR, self.regs.usbintr);
        w.field_u32(TAG_FRINDEX, self.regs.frindex);
        w.field_u32(TAG_PERIODICLISTBASE, self.regs.periodiclistbase);
        w.field_u32(TAG_ASYNCLISTADDR, self.regs.asynclistaddr);
        w.field_u32(TAG_CONFIGFLAG, self.regs.configflag);
        w.field_bytes(TAG_ROOT_HUB_PORTS, self.hub.save_snapshot_ports());
        w.field_u32(TAG_CTRLDSSEGMENT, self.regs.ctrldssegment);
        w.field_u32(TAG_USBLEGSUP, self.usblegsup);
        w.field_u32(TAG_USBLEGCTLSTS, self.usblegctlsts);
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_USBCMD: u16 = 1;
        const TAG_USBSTS: u16 = 2;
        const TAG_USBINTR: u16 = 3;
        const TAG_FRINDEX: u16 = 4;
        const TAG_PERIODICLISTBASE: u16 = 5;
        const TAG_ASYNCLISTADDR: u16 = 6;
        const TAG_CONFIGFLAG: u16 = 7;
        const TAG_ROOT_HUB_PORTS: u16 = 8;
        const TAG_CTRLDSSEGMENT: u16 = 9;
        const TAG_USBLEGSUP: u16 = 10;
        const TAG_USBLEGCTLSTS: u16 = 11;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Reset controller-local derived state without disturbing attached device instances.
        self.regs = EhciRegs::new();
        self.irq_level = false;
        self.usblegsup = USBLEGSUP_HEADER | USBLEGSUP_BIOS_SEM;
        self.usblegctlsts = 0;

        if let Some(usbcmd) = r.u32(TAG_USBCMD)? {
            self.regs.usbcmd = usbcmd & USBCMD_WRITE_MASK;
        }
        if let Some(usbsts) = r.u32(TAG_USBSTS)? {
            self.regs.usbsts = usbsts & USBSTS_READ_MASK;
        }
        if let Some(usbintr) = r.u32(TAG_USBINTR)? {
            self.regs.usbintr = usbintr & USBINTR_MASK;
        }
        if let Some(frindex) = r.u32(TAG_FRINDEX)? {
            self.regs.frindex = frindex & FRINDEX_MASK;
        }

        if let Some(periodic) = r.u32(TAG_PERIODICLISTBASE)? {
            self.regs.periodiclistbase = periodic & PERIODICLISTBASE_MASK;
        }
        if let Some(async_addr) = r.u32(TAG_ASYNCLISTADDR)? {
            self.regs.asynclistaddr = async_addr & ASYNCLISTADDR_MASK;
        }
        if let Some(config) = r.u32(TAG_CONFIGFLAG)? {
            // Avoid calling `write_configflag` here: it mutates PORTSC ownership bits. The snapshot contains
            // the per-port owner state already.
            self.regs.configflag = config & CONFIGFLAG_CF;
        }
        self.hub
            .set_configflag_for_restore(self.regs.configflag & CONFIGFLAG_CF != 0);
        // We currently model a 32-bit addressing controller (HCCPARAMS.AC64=0); CTRLDSSEGMENT is unused.
        if let Some(seg) = r.u32(TAG_CTRLDSSEGMENT)? {
            self.write_ctrldssegment(seg);
        }

        if let Some(buf) = r.bytes(TAG_ROOT_HUB_PORTS) {
            self.hub.load_snapshot_ports(buf)?;
        }

        if let Some(usblegsup) = r.u32(TAG_USBLEGSUP)? {
            let mut v = (usblegsup & USBLEGSUP_RW_MASK) | USBLEGSUP_HEADER;
            if v & USBLEGSUP_OS_SEM != 0 {
                v &= !USBLEGSUP_BIOS_SEM;
            }
            self.usblegsup = v;
        }
        if let Some(usblegctlsts) = r.u32(TAG_USBLEGCTLSTS)? {
            self.usblegctlsts = usblegctlsts;
        }

        self.update_irq();
        Ok(())
    }
}
