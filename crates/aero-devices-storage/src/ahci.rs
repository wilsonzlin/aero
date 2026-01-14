//! AHCI (Advanced Host Controller Interface) controller emulation.
//!
//! Windows 7's in-box AHCI miniport driver (`msahci.sys`) expects an AHCI HBA with a working
//! command list engine, PRDT-based DMA into guest memory, and interrupts.
//!
//! This module implements enough of the AHCI 1.x programming model for early boot:
//! - HBA memory registers (CAP/GHC/IS/PI/VS)
//! - Per-port registers (CLB/FB/IS/IE/CMD/TFD/SIG/SSTS/CI)
//! - Command list parsing (command header + command table + PRDT)
//! - ATA commands: IDENTIFY, READ/WRITE DMA (28-bit + EXT), READ/WRITE SECTORS (PIO 28-bit + EXT),
//!   FLUSH CACHE(_EXT), SET FEATURES

use std::fmt;
use std::io;

use crate::ata::{
    AtaDrive, ATA_CMD_FLUSH_CACHE, ATA_CMD_FLUSH_CACHE_EXT, ATA_CMD_IDENTIFY, ATA_CMD_READ_DMA,
    ATA_CMD_READ_DMA_EXT, ATA_CMD_READ_SECTORS, ATA_CMD_READ_SECTORS_EXT, ATA_CMD_SET_FEATURES,
    ATA_CMD_WRITE_DMA, ATA_CMD_WRITE_DMA_EXT, ATA_CMD_WRITE_SECTORS, ATA_CMD_WRITE_SECTORS_EXT,
    ATA_ERROR_ABRT, ATA_STATUS_BSY, ATA_STATUS_DRDY, ATA_STATUS_DSC, ATA_STATUS_ERR,
};
use aero_devices::irq::IrqLine;
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotResult, SnapshotVersion};
use aero_io_snapshot::io::storage::state::{AhciControllerState, AhciHbaState, AhciPortState};
use aero_storage::SECTOR_SIZE;
use memory::MemoryBus;

const HBA_REG_CAP: u64 = 0x00;
const HBA_REG_GHC: u64 = 0x04;
const HBA_REG_IS: u64 = 0x08;
const HBA_REG_PI: u64 = 0x0C;
const HBA_REG_VS: u64 = 0x10;
const HBA_REG_CAP2: u64 = 0x24;
const HBA_REG_BOHC: u64 = 0x28;

const PORT_BASE: u64 = 0x100;
const PORT_STRIDE: u64 = 0x80;

const PORT_REG_CLB: u64 = 0x00;
const PORT_REG_CLBU: u64 = 0x04;
const PORT_REG_FB: u64 = 0x08;
const PORT_REG_FBU: u64 = 0x0C;
const PORT_REG_IS: u64 = 0x10;
const PORT_REG_IE: u64 = 0x14;
const PORT_REG_CMD: u64 = 0x18;
const PORT_REG_TFD: u64 = 0x20;
const PORT_REG_SIG: u64 = 0x24;
const PORT_REG_SSTS: u64 = 0x28;
const PORT_REG_SCTL: u64 = 0x2C;
const PORT_REG_SERR: u64 = 0x30;
const PORT_REG_SACT: u64 = 0x34;
const PORT_REG_CI: u64 = 0x38;

const GHC_HR: u32 = 1 << 0;
const GHC_IE: u32 = 1 << 1;
const GHC_AE: u32 = 1 << 31;

// CAP2 bits.
const CAP2_BOH: u32 = 1 << 0;

// BOHC bits (subset).
const BOHC_BOS: u32 = 1 << 0;
const BOHC_OOS: u32 = 1 << 1;
const BOHC_SOOE: u32 = 1 << 2;
const BOHC_OOC: u32 = 1 << 3;
const _BOHC_BB: u32 = 1 << 4;

const PORT_CMD_ST: u32 = 1 << 0;
const PORT_CMD_FRE: u32 = 1 << 4;
const PORT_CMD_FR: u32 = 1 << 14;
const PORT_CMD_CR: u32 = 1 << 15;

const PORT_IS_DHRS: u32 = 1 << 0;
const PORT_IS_TFES: u32 = 1 << 30;

/// SATA drive signature (PxSIG) for an ATA device.
const SATA_SIG_ATA: u32 = 0x0000_0101;

// PxSSTS DET field values.
const SSTS_DET_MASK: u32 = 0x0F;
const SSTS_DET_NO_DEVICE: u32 = 0;
const SSTS_DET_DEVICE_PRESENT_NO_PHY: u32 = 1;
const SSTS_DET_DEVICE_PRESENT_PHY: u32 = 3;

// PxSSTS SPD/IPM "plausible defaults" for a present device.
const SSTS_SPD_GEN1: u32 = 1 << 4;
const SSTS_IPM_ACTIVE: u32 = 1 << 8;

// PxSCTL.DET (Device Detection Initialization) values.
const SCTL_DET_MASK: u32 = 0x0F;
const SCTL_DET_COMRESET: u32 = 1;
const SCTL_DET_DISABLE: u32 = 4;
// Some guests use DET=2 for "disable" even though DET=4 is the commonly-implemented value.
const SCTL_DET_DISABLE_ALT: u32 = 2;

// PxSERR bits (SATA SError). We only model a couple of bits to help guests that expect
// reset/abort events to latch some error condition.
const SERR_ERR_PROTOCOL: u32 = 1 << 4;
const SERR_DIAG_PHYRDY_CHANGE: u32 = 1 << 16;

#[derive(Debug, Clone, Copy)]
struct HbaRegs {
    cap: u32,
    ghc: u32,
    cap2: u32,
    bohc: u32,
    vs: u32,
}

impl HbaRegs {
    fn new(num_ports: usize) -> Self {
        // CAP.NP is number of ports minus 1.
        let np = (num_ports.saturating_sub(1) as u32) & 0x1F;
        // CAP.NCS is number of command slots minus 1.
        let ncs = 31u32 << 8; // 32 slots
                              // CAP.S64A indicates 64-bit addressing is supported.
        let s64a = 1u32 << 31;
        Self {
            cap: np | ncs | s64a,
            // QEMU/ICH9-style: controller comes up in AHCI mode with AE set.
            ghc: GHC_AE,
            // Advertise BIOS/OS handoff (BOHC) capability since we expose the register.
            cap2: CAP2_BOH,
            bohc: 0,
            vs: 0x0001_0300, // AHCI 1.3
        }
    }

    fn reset(&mut self) {
        // Preserve ICH9/QEMU behaviour: stay in AHCI mode (AE=1) while clearing IE.
        self.ghc = GHC_AE;
        self.bohc = 0;
    }
}

#[derive(Debug, Clone, Copy)]
struct PortRegs {
    clb: u64,
    fb: u64,
    is: u32,
    ie: u32,
    cmd: u32,
    tfd: u32,
    sig: u32,
    ssts: u32,
    sctl: u32,
    serr: u32,
    sact: u32,
    ci: u32,
}

impl PortRegs {
    fn new(present: bool) -> Self {
        let (sig, ssts, tfd) = if present {
            // DET=3 (device present), SPD=1 (Gen1), IPM=1 (active).
            let ssts = SSTS_IPM_ACTIVE | SSTS_SPD_GEN1 | SSTS_DET_DEVICE_PRESENT_PHY;
            let status = ATA_STATUS_DRDY | ATA_STATUS_DSC;
            (SATA_SIG_ATA, ssts, status as u32)
        } else {
            (0, 0, 0)
        };
        Self {
            clb: 0,
            fb: 0,
            is: 0,
            ie: 0,
            cmd: 0,
            tfd,
            sig,
            ssts,
            sctl: 0,
            serr: 0,
            sact: 0,
            ci: 0,
        }
    }

    fn running(&self) -> bool {
        self.cmd & PORT_CMD_ST != 0
    }

    fn fis_receive_enabled(&self) -> bool {
        self.cmd & PORT_CMD_FRE != 0
    }

    fn update_running_bits(&mut self) {
        // We implement FR and CR as immediate reflections of FRE/ST. Real hardware has stop
        // sequences; for synchronous emulation this approximation is sufficient.
        let running = self.running();
        let fre = self.fis_receive_enabled();

        self.cmd &= !(PORT_CMD_FR | PORT_CMD_CR);
        if fre {
            self.cmd |= PORT_CMD_FR;
        }
        if running {
            self.cmd |= PORT_CMD_CR;
        }
    }

    fn sctl_det(&self) -> u32 {
        self.sctl & SCTL_DET_MASK
    }
}

#[derive(Debug)]
struct AhciPort {
    present: bool,
    regs: PortRegs,
    drive: Option<AtaDrive>,
}

impl AhciPort {
    fn new() -> Self {
        Self {
            present: false,
            regs: PortRegs::new(false),
            drive: None,
        }
    }

    fn attach_drive(&mut self, drive: AtaDrive) {
        self.drive = Some(drive);
        if !self.present {
            self.present = true;
            self.regs = PortRegs::new(true);
        }
        self.regs.update_running_bits();
    }

    fn clear_drive(&mut self) {
        self.drive = None;
        self.present = false;
        self.regs = PortRegs::new(false);
        self.regs.update_running_bits();
    }
}

pub struct AhciController {
    hba: HbaRegs,
    ports: Vec<AhciPort>,
    irq: Box<dyn IrqLine>,
}

impl AhciController {
    pub fn new(irq: Box<dyn IrqLine>, num_ports: usize) -> Self {
        assert!((1..=32).contains(&num_ports));
        Self {
            hba: HbaRegs::new(num_ports),
            ports: (0..num_ports).map(|_| AhciPort::new()).collect(),
            irq,
        }
    }

    pub fn attach_drive(&mut self, port: usize, drive: AtaDrive) {
        self.ports[port].attach_drive(drive);
    }

    pub fn detach_drive(&mut self, port: usize) {
        self.ports[port].clear_drive();
    }

    fn ports_implemented(&self) -> u32 {
        // AHCI PI (Ports Implemented) is a hardware strap indicating which ports exist in the HBA.
        // It should not depend on whether a drive is currently attached.
        //
        // Guests (including Windows' inbox AHCI drivers like `msahci.sys` / `storahci.sys`)
        // enumerate ports using this bitmask, then check per-port status registers (e.g. PxSSTS)
        // to determine whether a device is present.
        let ports = self.ports.len();
        if ports >= 32 {
            u32::MAX
        } else {
            (1u32 << ports) - 1
        }
    }

    fn hba_is(&self) -> u32 {
        let mut is = 0u32;
        for (idx, port) in self.ports.iter().enumerate() {
            if port.regs.is != 0 {
                is |= 1 << idx;
            }
        }
        is
    }

    fn update_irq(&self) {
        let any_enabled_pending =
            self.hba.ghc & GHC_IE != 0 && self.ports.iter().any(|p| p.regs.is & p.regs.ie != 0);
        self.irq.set_level(any_enabled_pending);
    }

    /// Reset controller state back to the power-on baseline while preserving attached drives.
    ///
    /// This intentionally does **not** drop any [`AtaDrive`] backends currently attached to ports.
    pub fn reset(&mut self) {
        self.hba.reset();
        for port in &mut self.ports {
            port.regs = PortRegs::new(port.present);
            port.regs.update_running_bits();
        }
        self.update_irq();
    }

    pub fn read_u32(&mut self, offset: u64) -> u32 {
        match offset {
            HBA_REG_CAP => self.hba.cap,
            HBA_REG_GHC => self.hba.ghc,
            HBA_REG_IS => self.hba_is(),
            HBA_REG_PI => self.ports_implemented(),
            HBA_REG_VS => self.hba.vs,
            HBA_REG_CAP2 => self.hba.cap2,
            HBA_REG_BOHC => self.hba.bohc,
            _ if offset >= PORT_BASE => self.read_port_u32(offset),
            _ => 0,
        }
    }

    fn read_port_u32(&mut self, offset: u64) -> u32 {
        let (port_idx, reg_off) = decode_port_offset(offset);
        let Some(port) = self.ports.get_mut(port_idx) else {
            return 0;
        };
        match reg_off {
            PORT_REG_CLB => port.regs.clb as u32,
            PORT_REG_CLBU => (port.regs.clb >> 32) as u32,
            PORT_REG_FB => port.regs.fb as u32,
            PORT_REG_FBU => (port.regs.fb >> 32) as u32,
            PORT_REG_IS => port.regs.is,
            PORT_REG_IE => port.regs.ie,
            PORT_REG_CMD => port.regs.cmd,
            PORT_REG_TFD => port.regs.tfd,
            PORT_REG_SIG => port.regs.sig,
            PORT_REG_SSTS => port.regs.ssts,
            PORT_REG_SCTL => port.regs.sctl,
            PORT_REG_SERR => port.regs.serr,
            PORT_REG_SACT => port.regs.sact,
            PORT_REG_CI => port.regs.ci,
            _ => 0,
        }
    }

    pub fn write_u32(&mut self, offset: u64, val: u32) {
        match offset {
            HBA_REG_GHC => self.write_ghc(val),
            HBA_REG_IS => self.write_hba_is(val),
            HBA_REG_BOHC => self.write_bohc(val),
            _ if offset >= PORT_BASE => self.write_port_u32(offset, val),
            _ => {}
        }
        self.update_irq();
    }

    fn write_bohc(&mut self, val: u32) {
        // BIOS/OS handoff is (largely) a firmware concern, but some guests probe BOHC when
        // CAP2.BOH is set. We implement a small subset of the AHCI semantics to avoid wedging:
        // - BOS/OOS/SOOE are treated as simple R/W bits.
        // - OOC is treated as W1C (write-1-to-clear).
        // - When OOS transitions from 0->1, we immediately clear BOS (there is no BIOS) and set
        //   OOC to indicate the ownership change event.
        //
        // Bits outside this subset are ignored/read-as-zero.
        let old = self.hba.bohc;
        let old_oos = old & BOHC_OOS != 0;

        // Apply W1C for OOC first.
        let mut next = old;
        if val & BOHC_OOC != 0 {
            next &= !BOHC_OOC;
        }

        // Update writable bits.
        next &= !(BOHC_BOS | BOHC_OOS | BOHC_SOOE);
        next |= val & (BOHC_BOS | BOHC_OOS | BOHC_SOOE);

        // No BIOS exists in this emulation; if the OS claims ownership, immediately grant it.
        let new_oos = next & BOHC_OOS != 0;
        if !old_oos && new_oos {
            next &= !BOHC_BOS;
            next |= BOHC_OOC;
        }

        // Always report BIOS busy as 0 (not modelled).
        next &= !_BOHC_BB;

        self.hba.bohc = next;
    }

    fn write_ghc(&mut self, val: u32) {
        if val & GHC_HR != 0 {
            self.reset();
            return;
        }

        // Preserve AE/IE and ignore reserved bits. AE is required for AHCI mode.
        let masked = val & (GHC_IE | GHC_AE);
        self.hba.ghc = masked;
    }

    fn write_hba_is(&mut self, val: u32) {
        // Clearing bits in the global IS is equivalent to clearing the per-port IS.
        for (idx, port) in self.ports.iter_mut().enumerate() {
            if val & (1 << idx) != 0 {
                port.regs.is = 0;
            }
        }
    }

    fn write_port_u32(&mut self, offset: u64, val: u32) {
        let (port_idx, reg_off) = decode_port_offset(offset);
        let Some(port) = self.ports.get_mut(port_idx) else {
            return;
        };

        match reg_off {
            PORT_REG_CLB => {
                port.regs.clb = (port.regs.clb & 0xFFFF_FFFF_0000_0000) | val as u64;
            }
            PORT_REG_CLBU => {
                port.regs.clb = (port.regs.clb & 0x0000_0000_FFFF_FFFF) | ((val as u64) << 32);
            }
            PORT_REG_FB => {
                port.regs.fb = (port.regs.fb & 0xFFFF_FFFF_0000_0000) | val as u64;
            }
            PORT_REG_FBU => {
                port.regs.fb = (port.regs.fb & 0x0000_0000_FFFF_FFFF) | ((val as u64) << 32);
            }
            PORT_REG_IS => {
                // Write 1 to clear.
                port.regs.is &= !val;
            }
            PORT_REG_IE => port.regs.ie = val,
            PORT_REG_CMD => {
                // Preserve read-only bits (FR/CR) but otherwise allow the guest to program the
                // full PxCMD register. Windows sets bits like SUD/POD in addition to ST/FRE.
                port.regs.cmd = val & !(PORT_CMD_FR | PORT_CMD_CR);
                port.regs.update_running_bits();
            }
            PORT_REG_SCTL => {
                // Model the minimal PxSCTL.DET COMRESET sequence that many AHCI drivers use for
                // link bring-up.
                //
                // Typical flow:
                //   PxSCTL.DET = 1 (COMRESET)
                //   PxSCTL.DET = 0 (idle)
                //   Poll PxSSTS/PxTFD for device present/ready.
                //
                // We emulate this synchronously: DET=1 immediately forces the port into a
                // transient "resetting" view. A subsequent transition back to DET=0 immediately
                // completes the reset, restoring link status/task-file status for any attached
                // drive.
                let old_det = port.regs.sctl & SCTL_DET_MASK;
                let new_det = val & SCTL_DET_MASK;

                port.regs.sctl = val;

                // COMRESET asserted: drop link status and mark the device busy.
                if new_det == SCTL_DET_COMRESET {
                    // A link reset aborts any in-flight commands and clears transient status.
                    port.regs.ci = 0;
                    port.regs.sact = 0;
                    port.regs.is = 0;

                    // Clear old SERR state, but latch a basic diagnostic bit so guests can observe
                    // that a reset event occurred. PxSERR is W1C (handled in PORT_REG_SERR).
                    port.regs.serr = SERR_DIAG_PHYRDY_CHANGE;

                    // Report a transient "device present but no communication" state if a drive is
                    // attached. This avoids guests interpreting the reset as a hot-unplug.
                    port.regs.ssts = if port.present {
                        SSTS_DET_DEVICE_PRESENT_NO_PHY
                    } else {
                        SSTS_DET_NO_DEVICE
                    };
                    port.regs.sig = 0;
                    port.regs.tfd = if port.present {
                        u32::from(ATA_STATUS_BSY)
                    } else {
                        0
                    };
                }

                // DET=4 disables the port (PHY offline). Some guests use DET=2 for this too; treat
                // it as an alias.
                if matches!(new_det, SCTL_DET_DISABLE | SCTL_DET_DISABLE_ALT) {
                    port.regs.ci = 0;
                    port.regs.sact = 0;
                    port.regs.is = 0;
                    port.regs.serr = 0;
                    // DET=4 indicates the PHY is offline.
                    port.regs.ssts = 4;
                    port.regs.sig = 0;
                    port.regs.tfd = 0;
                }

                // COMRESET deasserted: if we were previously in DET=1, bring the link back up.
                if old_det == SCTL_DET_COMRESET && new_det == 0 {
                    if port.present {
                        // DET=3 (device present), SPD=1 (Gen1), IPM=1 (active).
                        port.regs.ssts =
                            SSTS_IPM_ACTIVE | SSTS_SPD_GEN1 | SSTS_DET_DEVICE_PRESENT_PHY;
                        port.regs.sig = SATA_SIG_ATA;
                        port.regs.tfd = u32::from(ATA_STATUS_DRDY | ATA_STATUS_DSC);

                        // Signal that the device-to-host register FIS has been received.
                        port.regs.is |= PORT_IS_DHRS;
                    } else {
                        port.regs.ssts = SSTS_DET_NO_DEVICE;
                        port.regs.sig = 0;
                        port.regs.tfd = 0;
                    }
                }

                // If the guest re-enables a previously disabled port, complete link bring-up
                // synchronously (similar to COMRESET completion).
                if matches!(old_det, SCTL_DET_DISABLE | SCTL_DET_DISABLE_ALT) && new_det == 0 {
                    if port.present {
                        port.regs.ssts =
                            SSTS_IPM_ACTIVE | SSTS_SPD_GEN1 | SSTS_DET_DEVICE_PRESENT_PHY;
                        port.regs.sig = SATA_SIG_ATA;
                        port.regs.tfd = u32::from(ATA_STATUS_DRDY | ATA_STATUS_DSC);
                        port.regs.is |= PORT_IS_DHRS;
                    } else {
                        port.regs.ssts = SSTS_DET_NO_DEVICE;
                        port.regs.sig = 0;
                        port.regs.tfd = 0;
                    }
                }
            }
            PORT_REG_SERR => {
                // Write 1 to clear.
                port.regs.serr &= !val;
            }
            PORT_REG_SACT => port.regs.sact = val,
            PORT_REG_CI => {
                // Writing to CI sets command issue bits.
                port.regs.ci |= val;

                // If the port is operational, reflect that work is pending via PxTFD.BSY.
                // This helps guests that poll TFD rather than relying exclusively on interrupts.
                if val != 0
                    && port.present
                    && port.regs.running()
                    && port.regs.fis_receive_enabled()
                    && port.regs.sctl_det() != SCTL_DET_COMRESET
                    && (port.regs.ssts & SSTS_DET_MASK) == SSTS_DET_DEVICE_PRESENT_PHY
                {
                    port.regs.tfd = u32::from(ATA_STATUS_BSY);
                }
            }
            _ => {}
        }
    }

    /// Process any pending command list entries.
    ///
    /// A full emulator should call this when the guest writes to PxCI, or on a periodic tick.
    pub fn process(&mut self, mem: &mut dyn MemoryBus) {
        for port_idx in 0..self.ports.len() {
            self.process_port(port_idx, mem);
        }
        self.update_irq();
    }

    fn process_port(&mut self, port_idx: usize, mem: &mut dyn MemoryBus) {
        let Some(port) = self.ports.get_mut(port_idx) else {
            return;
        };

        // If COMRESET is asserted (PxSCTL.DET=1), commands must not execute.
        if port.regs.sctl_det() == SCTL_DET_COMRESET {
            return;
        }
        // Only execute commands when the PHY is established.
        if (port.regs.ssts & SSTS_DET_MASK) != SSTS_DET_DEVICE_PRESENT_PHY {
            return;
        }
        let Some(drive) = port.drive.as_mut() else {
            return;
        };
        if !port.regs.running() || !port.regs.fis_receive_enabled() {
            return;
        }
        if port.regs.clb == 0 || port.regs.fb == 0 {
            return;
        }

        while port.regs.ci != 0 {
            let slot = port.regs.ci.trailing_zeros() as usize;
            let bit = 1u32 << slot;
            if slot >= 32 {
                port.regs.ci &= !bit;
                continue;
            }

            // Mark the task file as busy while we process the command.
            port.regs.tfd = u32::from(ATA_STATUS_BSY);

            match process_command_slot(drive, &mut port.regs, slot, mem) {
                Ok(()) => {}
                Err(_) => {
                    // Report an aborted command via task file status/error.
                    let status = ATA_STATUS_DRDY | ATA_STATUS_DSC | ATA_STATUS_ERR;
                    port.regs.tfd = (status as u32) | ((ATA_ERROR_ABRT as u32) << 8);
                    port.regs.serr |= SERR_ERR_PROTOCOL;
                    write_d2h_fis(mem, port.regs.fb, status, ATA_ERROR_ABRT);
                    port.regs.is |= PORT_IS_DHRS | PORT_IS_TFES;
                }
            }

            port.regs.ci &= !bit;
        }
    }
}

impl fmt::Debug for AhciController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AhciController")
            .field("hba", &self.hba)
            .field("ports", &self.ports)
            .finish_non_exhaustive()
    }
}

impl AhciController {
    pub fn snapshot_state(&self) -> AhciControllerState {
        AhciControllerState {
            hba: AhciHbaState {
                cap: self.hba.cap,
                ghc: self.hba.ghc,
                cap2: self.hba.cap2,
                bohc: self.hba.bohc,
                vs: self.hba.vs,
            },
            ports: self
                .ports
                .iter()
                .map(|p| AhciPortState {
                    clb: p.regs.clb,
                    fb: p.regs.fb,
                    is: p.regs.is,
                    ie: p.regs.ie,
                    cmd: p.regs.cmd,
                    tfd: p.regs.tfd,
                    sig: p.regs.sig,
                    ssts: p.regs.ssts,
                    sctl: p.regs.sctl,
                    serr: p.regs.serr,
                    sact: p.regs.sact,
                    ci: p.regs.ci,
                })
                .collect(),
        }
    }

    pub fn restore_state(&mut self, state: &AhciControllerState) {
        self.hba.cap = state.hba.cap;
        self.hba.ghc = state.hba.ghc;
        self.hba.cap2 = state.hba.cap2;
        self.hba.bohc = state.hba.bohc;
        self.hba.vs = state.hba.vs;

        // Reset ports to a deterministic baseline and clear transient host-side disk backends.
        // The platform is responsible for re-attaching disks post-restore.
        for port in &mut self.ports {
            port.drive = None;
            port.present = false;
            port.regs = PortRegs::new(false);
            port.regs.update_running_bits();
        }

        // Apply saved port register state, clamping to the controller's configured port count.
        let count = state.ports.len().min(self.ports.len());
        for (idx, p) in state.ports.iter().take(count).enumerate() {
            let port = &mut self.ports[idx];
            port.regs.clb = p.clb;
            port.regs.fb = p.fb;
            port.regs.is = p.is;
            port.regs.ie = p.ie;
            port.regs.cmd = p.cmd;
            port.regs.tfd = p.tfd;
            port.regs.sig = p.sig;
            port.regs.ssts = p.ssts;
            port.regs.sctl = p.sctl;
            port.regs.serr = p.serr;
            port.regs.sact = p.sact;
            port.regs.ci = p.ci;
            port.present = p.sig != 0 || p.ssts != 0;
            port.regs.update_running_bits();
        }

        self.update_irq();
    }
}

impl IoSnapshot for AhciController {
    const DEVICE_ID: [u8; 4] = <AhciControllerState as IoSnapshot>::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion = <AhciControllerState as IoSnapshot>::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        // This AHCI model completes commands synchronously in [`AhciController::process`]. Any
        // outstanding work is fully represented by guest-visible registers (e.g. PxCI/PxSACT), so
        // we don't need additional in-flight bookkeeping in the snapshot.
        self.snapshot_state().save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        let mut state = AhciControllerState::default();
        state.load_state(bytes)?;
        self.restore_state(&state);
        Ok(())
    }
}

fn decode_port_offset(offset: u64) -> (usize, u64) {
    let port_idx = ((offset - PORT_BASE) / PORT_STRIDE) as usize;
    let reg_off = (offset - PORT_BASE) % PORT_STRIDE;
    (port_idx, reg_off)
}

fn process_command_slot(
    drive: &mut AtaDrive,
    port_regs: &mut PortRegs,
    slot: usize,
    mem: &mut dyn MemoryBus,
) -> io::Result<()> {
    // Guest-controlled DMA base addresses can be arbitrary; use wrapping arithmetic so malformed
    // values cannot trigger an overflow panic when fuzzing/debug overflow checks are enabled.
    let header_addr = port_regs.clb.wrapping_add((slot as u64).wrapping_mul(32));
    let header = CommandHeader::read(mem, header_addr);

    // Command table always contains the command FIS at offset 0.
    let mut cfis = [0u8; 64];
    mem.read_physical(header.ctba, &mut cfis);

    // Register Host to Device FIS
    if cfis[0] != 0x27 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported FIS type",
        ));
    }

    let command = cfis[2];
    match command {
        ATA_CMD_IDENTIFY => {
            let identify = drive.identify_sector();
            dma_write_from_host_buffer(mem, &header, identify)?;
            complete_command(mem, port_regs, slot, (identify.len()) as u32);
        }
        ATA_CMD_READ_DMA | ATA_CMD_READ_SECTORS => {
            // Even for PIO opcodes, AHCI uses PRDT scatter/gather DMA.
            let lba = extract_lba28(&cfis)?;
            let sector_count = extract_sector_count_28(&cfis);
            let byte_len = sector_count as usize * SECTOR_SIZE;
            dma_read_sectors_into_guest(mem, &header, drive, lba, byte_len)?;
            complete_command(mem, port_regs, slot, byte_len as u32);
        }
        ATA_CMD_READ_DMA_EXT | ATA_CMD_READ_SECTORS_EXT => {
            // Even for PIO opcodes, AHCI uses PRDT scatter/gather DMA.
            let lba = extract_lba48(&cfis);
            let sector_count = extract_sector_count(&cfis);
            let byte_len = sector_count as usize * SECTOR_SIZE;
            dma_read_sectors_into_guest(mem, &header, drive, lba, byte_len)?;
            complete_command(mem, port_regs, slot, byte_len as u32);
        }
        ATA_CMD_WRITE_DMA | ATA_CMD_WRITE_SECTORS => {
            // Even for PIO opcodes, AHCI uses PRDT scatter/gather DMA.
            let lba = extract_lba28(&cfis)?;
            let sector_count = extract_sector_count_28(&cfis);
            let byte_len = sector_count as usize * SECTOR_SIZE;
            dma_write_sectors_from_guest(mem, &header, drive, lba, byte_len)?;
            complete_command(mem, port_regs, slot, byte_len as u32);
        }
        ATA_CMD_WRITE_DMA_EXT | ATA_CMD_WRITE_SECTORS_EXT => {
            // Even for PIO opcodes, AHCI uses PRDT scatter/gather DMA.
            let lba = extract_lba48(&cfis);
            let sector_count = extract_sector_count(&cfis);
            let byte_len = sector_count as usize * SECTOR_SIZE;
            dma_write_sectors_from_guest(mem, &header, drive, lba, byte_len)?;
            complete_command(mem, port_regs, slot, byte_len as u32);
        }
        ATA_CMD_FLUSH_CACHE | ATA_CMD_FLUSH_CACHE_EXT => {
            drive.flush()?;
            complete_command(mem, port_regs, slot, 0);
        }
        ATA_CMD_SET_FEATURES => {
            // Subcommand is in Features (low byte).
            match cfis[3] {
                0x02 => drive.set_write_cache_enabled(true),
                0x82 => drive.set_write_cache_enabled(false),
                _ => {}
            }
            complete_command(mem, port_regs, slot, 0);
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported ATA command 0x{command:02x}"),
            ));
        }
    }

    Ok(())
}

fn extract_lba28(cfis: &[u8; 64]) -> io::Result<u64> {
    // 28-bit LBA fields follow the legacy task file layout:
    // - cfis[4..7] = LBA0..2
    // - cfis[7] bits 0..3 = LBA3 (high nibble)
    // - cfis[7] bit 6 (0x40) must be set to indicate LBA mode.
    if cfis[7] & 0x40 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "ATA command requires LBA addressing mode",
        ));
    }

    Ok((cfis[4] as u64)
        | ((cfis[5] as u64) << 8)
        | ((cfis[6] as u64) << 16)
        | (((cfis[7] & 0x0F) as u64) << 24))
}

fn extract_lba48(cfis: &[u8; 64]) -> u64 {
    // LBA fields:
    // cfis[4..7]  = LBA0..2
    // cfis[8..11] = LBA3..5
    (cfis[4] as u64)
        | ((cfis[5] as u64) << 8)
        | ((cfis[6] as u64) << 16)
        | ((cfis[8] as u64) << 24)
        | ((cfis[9] as u64) << 32)
        | ((cfis[10] as u64) << 40)
}

fn extract_sector_count_28(cfis: &[u8; 64]) -> u32 {
    // In legacy (28-bit) commands, the sector count register is only 8-bit and 0 means 256.
    match cfis[12] {
        0 => 256,
        v => v as u32,
    }
}

fn extract_sector_count(cfis: &[u8; 64]) -> u32 {
    let count = (cfis[12] as u32) | ((cfis[13] as u32) << 8);
    if count == 0 {
        65536
    } else {
        count
    }
}

#[derive(Debug, Clone, Copy)]
struct CommandHeader {
    ctba: u64,
    prdtl: u16,
}

impl CommandHeader {
    fn read(mem: &mut dyn MemoryBus, addr: u64) -> Self {
        let flags = mem.read_u32(addr);
        let ctba_lo = mem.read_u32(addr.wrapping_add(8)) as u64;
        let ctba_hi = mem.read_u32(addr.wrapping_add(12)) as u64;
        let ctba = ctba_lo | (ctba_hi << 32);
        // AHCI command header DW0 bits 16..31 hold the PRDT length (entry count).
        let prdtl = ((flags >> 16) & 0xFFFF) as u16;

        Self { ctba, prdtl }
    }

    fn prdt_entries(&self) -> u16 {
        self.prdtl
    }
}

#[derive(Debug, Clone, Copy)]
struct PrdtEntry {
    dba: u64,
    dbc: u32,
}

impl PrdtEntry {
    fn read(mem: &mut dyn MemoryBus, addr: u64) -> Self {
        let dba_lo = mem.read_u32(addr) as u64;
        let dba_hi = mem.read_u32(addr.wrapping_add(4)) as u64;
        let dba = dba_lo | (dba_hi << 32);
        let dbc_ioc = mem.read_u32(addr.wrapping_add(12));
        let dbc = (dbc_ioc & 0x003F_FFFF) + 1;
        Self { dba, dbc }
    }
}

fn try_alloc_zeroed(len: usize) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::OutOfMemory,
            "failed to allocate AHCI DMA buffer",
        )
    })?;
    buf.resize(len, 0);
    Ok(buf)
}

fn dma_read_sectors_into_guest(
    mem: &mut dyn MemoryBus,
    header: &CommandHeader,
    drive: &mut AtaDrive,
    mut lba: u64,
    byte_len: usize,
) -> io::Result<()> {
    // Avoid allocating a potentially huge contiguous buffer for the full transfer. Instead, stream
    // through the PRDT scatter/gather list and DMA in bounded chunks.
    const MAX_DMA_CHUNK_BYTES: usize = 256 * 1024; // must remain a multiple of 512
    debug_assert!(MAX_DMA_CHUNK_BYTES.is_multiple_of(SECTOR_SIZE));

    if byte_len == 0 {
        return Ok(());
    }

    // Guard against pathological PRDT lists that would otherwise turn a single synchronous DMA
    // operation into an extremely long loop. Real guests use relatively small PRDTs.
    const MAX_PRDT_ENTRIES_PER_COMMAND: u16 = 32_768;
    let prdt_entries = header.prdt_entries();
    if prdt_entries > MAX_PRDT_ENTRIES_PER_COMMAND {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PRDT too large for DMA read",
        ));
    }
    if prdt_entries == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "PRDT too small for DMA write",
        ));
    }

    let mut remaining = byte_len;
    let mut scratch = try_alloc_zeroed(MAX_DMA_CHUNK_BYTES)?;

    for i in 0..prdt_entries as u64 {
        if remaining == 0 {
            break;
        }
        let prd_addr = header
            .ctba
            .wrapping_add(0x80)
            .wrapping_add(i.wrapping_mul(16));
        let prd = PrdtEntry::read(mem, prd_addr);
        let mut seg_remaining = (prd.dbc as usize).min(remaining);

        if !seg_remaining.is_multiple_of(SECTOR_SIZE) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unaligned PRDT length for ATA read DMA",
            ));
        }

        let mut seg_off = 0usize;
        while seg_remaining != 0 {
            let chunk_len = seg_remaining.min(MAX_DMA_CHUNK_BYTES);
            let dst = prd.dba.wrapping_add(seg_off as u64);

            drive.read_sectors(lba, &mut scratch[..chunk_len])?;
            mem.write_physical(dst, &scratch[..chunk_len]);

            lba = lba.wrapping_add((chunk_len / SECTOR_SIZE) as u64);
            remaining -= chunk_len;
            seg_remaining -= chunk_len;
            seg_off += chunk_len;
        }
    }

    if remaining != 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "PRDT too small for DMA write",
        ));
    }

    Ok(())
}

fn dma_write_sectors_from_guest(
    mem: &mut dyn MemoryBus,
    header: &CommandHeader,
    drive: &mut AtaDrive,
    mut lba: u64,
    byte_len: usize,
) -> io::Result<()> {
    // Avoid allocating a potentially huge contiguous buffer for the full transfer. Instead, stream
    // through the PRDT scatter/gather list and DMA in bounded chunks.
    const MAX_DMA_CHUNK_BYTES: usize = 256 * 1024; // must remain a multiple of 512
    debug_assert!(MAX_DMA_CHUNK_BYTES.is_multiple_of(SECTOR_SIZE));

    if byte_len == 0 {
        return Ok(());
    }

    // Guard against pathological PRDT lists that would otherwise turn a single synchronous DMA
    // operation into an extremely long loop. Real guests use relatively small PRDTs.
    const MAX_PRDT_ENTRIES_PER_COMMAND: u16 = 32_768;
    let prdt_entries = header.prdt_entries();
    if prdt_entries > MAX_PRDT_ENTRIES_PER_COMMAND {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PRDT too large for DMA write",
        ));
    }
    if prdt_entries == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "PRDT too small for DMA read",
        ));
    }

    let mut remaining = byte_len;
    let mut scratch = try_alloc_zeroed(MAX_DMA_CHUNK_BYTES)?;

    for i in 0..prdt_entries as u64 {
        if remaining == 0 {
            break;
        }
        let prd_addr = header
            .ctba
            .wrapping_add(0x80)
            .wrapping_add(i.wrapping_mul(16));
        let prd = PrdtEntry::read(mem, prd_addr);
        let mut seg_remaining = (prd.dbc as usize).min(remaining);

        if !seg_remaining.is_multiple_of(SECTOR_SIZE) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unaligned PRDT length for ATA write DMA",
            ));
        }

        let mut seg_off = 0usize;
        while seg_remaining != 0 {
            let chunk_len = seg_remaining.min(MAX_DMA_CHUNK_BYTES);
            let src = prd.dba.wrapping_add(seg_off as u64);

            mem.read_physical(src, &mut scratch[..chunk_len]);
            drive.write_sectors(lba, &scratch[..chunk_len])?;

            lba = lba.wrapping_add((chunk_len / SECTOR_SIZE) as u64);
            remaining -= chunk_len;
            seg_remaining -= chunk_len;
            seg_off += chunk_len;
        }
    }

    if remaining != 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "PRDT too small for DMA read",
        ));
    }

    Ok(())
}

fn dma_write_from_host_buffer(
    mem: &mut dyn MemoryBus,
    header: &CommandHeader,
    src: &[u8],
) -> io::Result<()> {
    let mut remaining = src;

    for i in 0..header.prdt_entries() as u64 {
        if remaining.is_empty() {
            break;
        }
        let prd_addr = header
            .ctba
            .wrapping_add(0x80)
            .wrapping_add(i.wrapping_mul(16));
        let prd = PrdtEntry::read(mem, prd_addr);
        let chunk_len = prd.dbc.min(remaining.len() as u32) as usize;

        mem.write_physical(prd.dba, &remaining[..chunk_len]);
        remaining = &remaining[chunk_len..];
    }

    if !remaining.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "PRDT too small for DMA write",
        ));
    }

    Ok(())
}

fn complete_command(
    mem: &mut dyn MemoryBus,
    port_regs: &mut PortRegs,
    slot: usize,
    bytes_transferred: u32,
) {
    // Update PRDBC (DW1).
    let header_addr = port_regs.clb.wrapping_add((slot as u64).wrapping_mul(32));
    mem.write_u32(header_addr.wrapping_add(4), bytes_transferred);

    let status = ATA_STATUS_DRDY | ATA_STATUS_DSC;
    port_regs.tfd = u32::from(status);

    write_d2h_fis(mem, port_regs.fb, status, 0);

    // Signal completion.
    port_regs.is |= PORT_IS_DHRS;
}

fn write_d2h_fis(mem: &mut dyn MemoryBus, fb: u64, status: u8, error: u8) {
    // Received FIS layout places the D2H Register FIS at offset 0x40.
    let mut fis = [0u8; 20];
    fis[0] = 0x34; // FIS_TYPE_REG_D2H
    fis[1] = 1 << 6; // I bit (interrupt)
    fis[2] = status;
    fis[3] = error;
    mem.write_physical(fb.wrapping_add(0x40), &fis);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{TestIrqLine, TestMemory};
    use aero_storage::{AeroSparseConfig, AeroSparseDisk, MemBackend, RawDisk, VirtualDisk};
    use memory::MemoryBus;

    fn port_reg(port: usize, reg: u64) -> u64 {
        PORT_BASE + (port as u64) * PORT_STRIDE + reg
    }

    fn setup_controller() -> (AhciController, TestIrqLine, TestMemory, AtaDrive) {
        let irq = TestIrqLine::default();
        let ctl = AhciController::new(Box::new(irq.clone()), 1);
        let mem = TestMemory::new(0x20_000);
        let capacity = 32 * SECTOR_SIZE as u64;
        let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let drive = AtaDrive::new(Box::new(disk)).unwrap();
        (ctl, irq, mem, drive)
    }

    fn write_cmd_header(
        mem: &mut TestMemory,
        clb: u64,
        slot: usize,
        ctba: u64,
        prdtl: u16,
        write: bool,
    ) {
        let cfl = 5u32;
        let w = if write { 1u32 << 6 } else { 0 };
        let flags = cfl | w | ((prdtl as u32) << 16);
        let addr = clb + (slot as u64) * 32;
        mem.write_u32(addr, flags);
        mem.write_u32(addr + 4, 0); // PRDBC
        mem.write_u32(addr + 8, ctba as u32);
        mem.write_u32(addr + 12, (ctba >> 32) as u32);
    }

    fn write_prdt(mem: &mut TestMemory, ctba: u64, entry: usize, dba: u64, dbc: u32) {
        let addr = ctba + 0x80 + (entry as u64) * 16;
        mem.write_u32(addr, dba as u32);
        mem.write_u32(addr + 4, (dba >> 32) as u32);
        mem.write_u32(addr + 8, 0);
        // DBC field stores byte_count-1 in bits 0..21.
        mem.write_u32(addr + 12, (dbc - 1) & 0x003F_FFFF);
    }

    fn write_cfis48(mem: &mut TestMemory, ctba: u64, command: u8, lba: u64, count: u16) {
        let mut cfis = [0u8; 64];
        cfis[0] = 0x27;
        cfis[1] = 0x80;
        cfis[2] = command;
        cfis[7] = 0x40; // LBA mode

        cfis[4] = (lba & 0xFF) as u8;
        cfis[5] = ((lba >> 8) & 0xFF) as u8;
        cfis[6] = ((lba >> 16) & 0xFF) as u8;
        cfis[8] = ((lba >> 24) & 0xFF) as u8;
        cfis[9] = ((lba >> 32) & 0xFF) as u8;
        cfis[10] = ((lba >> 40) & 0xFF) as u8;

        cfis[12] = (count & 0xFF) as u8;
        cfis[13] = (count >> 8) as u8;

        mem.write_physical(ctba, &cfis);
    }

    fn write_cfis(mem: &mut TestMemory, ctba: u64, command: u8, lba: u64, count: u16) {
        write_cfis48(mem, ctba, command, lba, count);
    }

    fn write_cfis_28(mem: &mut TestMemory, ctba: u64, command: u8, lba: u64, count: u8) {
        assert!(lba <= 0x0FFF_FFFF);
        let mut cfis = [0u8; 64];
        cfis[0] = 0x27;
        cfis[1] = 0x80;
        cfis[2] = command;
        // 28-bit LBA: bits 24..27 are stored in the Device register low nibble when LBA mode is
        // enabled.
        cfis[7] = 0xE0 | ((lba >> 24) as u8 & 0x0F);

        cfis[4] = (lba & 0xFF) as u8;
        cfis[5] = ((lba >> 8) & 0xFF) as u8;
        cfis[6] = ((lba >> 16) & 0xFF) as u8;

        cfis[12] = count;

        mem.write_physical(ctba, &cfis);
    }

    fn make_drive_with_sector_marker(marker: [u8; 4], lba: u64) -> AtaDrive {
        let capacity = 64 * SECTOR_SIZE as u64;
        let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let mut sector = vec![0u8; SECTOR_SIZE];
        sector[0..4].copy_from_slice(&marker);
        disk.write_sectors(lba, &sector).unwrap();
        AtaDrive::new(Box::new(disk)).unwrap()
    }

    fn write_cfis28(mem: &mut TestMemory, ctba: u64, command: u8, lba: u64, count: u8) {
        write_cfis_28(mem, ctba, command, lba, count);
    }

    #[test]
    fn identify_dma_and_interrupt() {
        let (mut ctl, irq, mut mem, drive) = setup_controller();
        ctl.attach_drive(0, drive);

        // Program HBA and port.
        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;
        let data_buf = 0x4000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CLBU, 0);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FBU, 0);
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis48(&mut mem, ctba, ATA_CMD_IDENTIFY, 0, 0);
        write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);

        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        assert!(irq.level());
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_CI), 0);
        assert_eq!(
            ctl.read_u32(PORT_BASE + PORT_REG_IS) & PORT_IS_DHRS,
            PORT_IS_DHRS
        );

        let mut out = [0u8; SECTOR_SIZE];
        mem.read_physical(data_buf, &mut out);
        assert_eq!(out[0], 0x40);

        // Clear the interrupt.
        ctl.write_u32(PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
        assert!(!irq.level());
    }

    #[test]
    fn reset_preserves_attached_drive() {
        let (mut ctl, irq, mut mem, drive) = setup_controller();
        ctl.attach_drive(0, drive);

        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;
        let data_buf = 0x4000;

        let program_regs = |ctl: &mut AhciController| {
            ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
            ctl.write_u32(PORT_BASE + PORT_REG_CLBU, 0);
            ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
            ctl.write_u32(PORT_BASE + PORT_REG_FBU, 0);
            ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
            ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
            ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);
        };

        let run_identify = |ctl: &mut AhciController, mem: &mut TestMemory| {
            write_cmd_header(mem, clb, 0, ctba, 1, false);
            write_cfis48(mem, ctba, ATA_CMD_IDENTIFY, 0, 0);
            write_prdt(mem, ctba, 0, data_buf, SECTOR_SIZE as u32);

            ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
            ctl.process(mem);

            assert!(irq.level(), "IDENTIFY should assert IRQ");
            assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_CI), 0);
            assert_ne!(ctl.read_u32(PORT_BASE + PORT_REG_IS) & PORT_IS_DHRS, 0);

            let mut out = [0u8; SECTOR_SIZE];
            mem.read_physical(data_buf, &mut out);
            assert_eq!(out[0], 0x40);

            // Clear the interrupt so we can observe the post-reset behavior cleanly.
            ctl.write_u32(PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
            assert!(!irq.level());
        };

        program_regs(&mut ctl);
        run_identify(&mut ctl, &mut mem);

        ctl.reset();
        assert!(
            !irq.level(),
            "reset should clear pending interrupt level even with a drive attached"
        );

        // After reset, registers must be reprogrammed, but the attached drive should still be
        // present and respond to IDENTIFY.
        program_regs(&mut ctl);
        run_identify(&mut ctl, &mut mem);
    }

    #[test]
    fn comreset_sequence_sets_link_up_and_ready_and_interrupts() {
        let (mut ctl, irq, _mem, drive) = setup_controller();
        ctl.attach_drive(0, drive);

        // Enable the DHRS interrupt so we can observe link-reset completion.
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);

        // Assert COMRESET.
        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, 1);
        // DET=1 (device present, no communication) while COMRESET is asserted.
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SSTS) & 0xF, 1);
        assert_ne!(
            ctl.read_u32(PORT_BASE + PORT_REG_TFD) & (ATA_STATUS_BSY as u32),
            0
        );
        assert!(!irq.level(), "COMRESET assert should not raise DHRS");

        // Deassert COMRESET and complete synchronously.
        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, 0);

        // DET=3, SPD=1, IPM=1.
        let ssts = ctl.read_u32(PORT_BASE + PORT_REG_SSTS);
        assert_eq!(ssts & 0xF, 3);
        assert_eq!((ssts >> 4) & 0xF, 1);
        assert_eq!((ssts >> 8) & 0xF, 1);

        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SIG), SATA_SIG_ATA);
        assert_eq!(
            ctl.read_u32(PORT_BASE + PORT_REG_TFD) & 0xFF,
            (ATA_STATUS_DRDY | ATA_STATUS_DSC) as u32
        );

        assert_eq!(
            ctl.read_u32(PORT_BASE + PORT_REG_IS) & PORT_IS_DHRS,
            PORT_IS_DHRS
        );
        assert!(irq.level(), "reset completion should assert IRQ via DHRS");

        // Clear the interrupt and verify the line deasserts.
        ctl.write_u32(PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
        assert!(!irq.level());
        assert_eq!(irq.transitions(), vec![true, false]);
    }

    #[test]
    fn comreset_blocks_command_processing_until_deassert() {
        let (mut ctl, _irq, mut mem, drive) = setup_controller();
        ctl.attach_drive(0, drive);

        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;
        let data_buf = 0x4000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CLBU, 0);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FBU, 0);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis(&mut mem, ctba, ATA_CMD_IDENTIFY, 0, 0);
        write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);

        // Fill the destination buffer with a marker so we can detect unexpected DMA.
        mem.write_physical(data_buf, &vec![0xAA; SECTOR_SIZE]);

        // Assert COMRESET and issue a command while the link is down.
        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, 1);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        // Command must remain pending while the port is resetting.
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_CI) & 1, 1);
        let mut out = [0u8; SECTOR_SIZE];
        mem.read_physical(data_buf, &mut out);
        assert_eq!(out[0], 0xAA);

        // Deassert COMRESET and process again; the command should now complete.
        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, 0);
        ctl.process(&mut mem);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_CI), 0);
        mem.read_physical(data_buf, &mut out);
        assert_eq!(out[0], 0x40);
    }

    #[test]
    fn comreset_assert_clears_inflight_state() {
        let (mut ctl, _irq, _mem, drive) = setup_controller();
        ctl.attach_drive(0, drive);

        // Seed various in-flight / sticky registers.
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 0x1);
        ctl.write_u32(PORT_BASE + PORT_REG_SACT, 0x1234);
        // PxIS/PxSERR are W1C from the guest POV; seed them directly to validate COMRESET clears.
        ctl.ports[0].regs.is = PORT_IS_DHRS | PORT_IS_TFES;
        ctl.ports[0].regs.serr = 0xFFFF_FFFF;

        // Assert COMRESET.
        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, 1);

        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_CI), 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SACT), 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_IS), 0);
        assert_eq!(
            ctl.read_u32(PORT_BASE + PORT_REG_SERR),
            SERR_DIAG_PHYRDY_CHANGE
        );

        // Link should be in the transient "present, no communication" state with the device busy.
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SSTS) & 0xF, 1);
        assert_ne!(
            ctl.read_u32(PORT_BASE + PORT_REG_TFD) & (ATA_STATUS_BSY as u32),
            0
        );
    }

    #[test]
    fn comreset_without_drive_keeps_port_empty_and_no_irq() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq.clone()), 1);

        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);

        // COMRESET assert/deassert with no drive attached.
        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, 1);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SSTS) & 0xF, 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SIG), 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_TFD), 0);
        assert!(!irq.level());

        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SSTS) & 0xF, 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SIG), 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_TFD), 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_IS), 0);
        assert!(!irq.level(), "no device => no DHRS IRQ on reset completion");
    }

    #[test]
    fn sctl_det_disable_offlines_and_reenables_port() {
        let (mut ctl, irq, _mem, drive) = setup_controller();
        ctl.attach_drive(0, drive);

        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);

        // Disable the interface (DET=4).
        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, 4);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SSTS) & 0xF, 4);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SIG), 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_TFD), 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_IS), 0);
        assert!(!irq.level());

        // Re-enable (DET=0) and complete link-up synchronously.
        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, 0);

        let ssts = ctl.read_u32(PORT_BASE + PORT_REG_SSTS);
        assert_eq!(ssts & 0xF, 3);
        assert_eq!((ssts >> 4) & 0xF, 1);
        assert_eq!((ssts >> 8) & 0xF, 1);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SIG), SATA_SIG_ATA);
        assert_eq!(
            ctl.read_u32(PORT_BASE + PORT_REG_TFD) & 0xFF,
            (ATA_STATUS_DRDY | ATA_STATUS_DSC) as u32
        );
        assert_ne!(ctl.read_u32(PORT_BASE + PORT_REG_IS) & PORT_IS_DHRS, 0);
        assert!(irq.level());

        ctl.write_u32(PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
        assert!(!irq.level());
    }

    #[test]
    fn sctl_det_disable_alt_value2_is_accepted() {
        let (mut ctl, irq, _mem, drive) = setup_controller();
        ctl.attach_drive(0, drive);

        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);

        // Some guests use DET=2 to disable the interface; accept it as an alias for DET=4.
        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, 2);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SSTS) & 0xF, 4);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SIG), 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_TFD), 0);
        assert!(!irq.level());

        // Re-enable (DET=0).
        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SSTS) & 0xF, 3);
        assert!(irq.level());

        ctl.write_u32(PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
        assert!(!irq.level());
    }

    #[test]
    fn ports_implemented_reflects_hba_port_count_not_drive_presence() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq), 1);
        // With one port, PI should report bit0 set even if no drive is attached.
        assert_eq!(ctl.read_u32(HBA_REG_PI), 1);
    }

    #[test]
    fn read_write_dma_ext_roundtrip() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq.clone()), 1);
        let mut mem = TestMemory::new(0x20_000);

        // Disk with one sector at LBA=4 containing a marker.
        let capacity = 64 * SECTOR_SIZE as u64;
        let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let mut sector = vec![0u8; SECTOR_SIZE];
        sector[0..4].copy_from_slice(&[9, 8, 7, 6]);
        disk.write_sectors(4, &sector).unwrap();
        ctl.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());

        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;
        let data_buf = 0x5000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        // READ DMA EXT (LBA=4, 1 sector).
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis48(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 4, 1);
        write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut out = [0u8; 4];
        mem.read_physical(data_buf, &mut out);
        assert_eq!(out, [9, 8, 7, 6]);

        // WRITE DMA EXT (LBA=5, 1 sector).
        let write_buf = 0x6000;
        mem.write_physical(write_buf, &[1, 2, 3, 4]);
        // Pad remaining bytes so the disk write path can read a full sector.
        mem.write_physical(write_buf + 4, &vec![0u8; SECTOR_SIZE - 4]);

        write_cmd_header(&mut mem, clb, 0, ctba, 1, true);
        write_cfis48(&mut mem, ctba, ATA_CMD_WRITE_DMA_EXT, 5, 1);
        write_prdt(&mut mem, ctba, 0, write_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        // Verify by reading back via READ DMA EXT into a new buffer.
        let verify_buf = 0x7000;
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis48(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 5, 1);
        write_prdt(&mut mem, ctba, 0, verify_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut verify = [0u8; 4];
        mem.read_physical(verify_buf, &mut verify);
        assert_eq!(verify, [1, 2, 3, 4]);
        assert!(irq.level());
    }

    #[test]
    fn read_write_dma_28_roundtrip_and_lba_high_nibble() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq.clone()), 1);
        let mut mem = TestMemory::new(0x20_000);

        // Use a sparse disk so we can access LBAs that exercise the 28-bit high nibble in the
        // Device register without allocating gigabytes of backing storage.
        let high_lba: u64 = 0x0100_0000;
        let cfg = AeroSparseConfig {
            disk_size_bytes: (high_lba + 16) * SECTOR_SIZE as u64,
            block_size_bytes: 1024 * 1024,
        };
        let mut disk = AeroSparseDisk::create(MemBackend::new(), cfg).unwrap();

        // Pre-populate the high LBA with a marker to validate parsing of bits 24..27.
        let mut sector = vec![0u8; SECTOR_SIZE];
        sector[0..4].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        disk.write_sectors(high_lba, &sector).unwrap();

        ctl.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());

        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;
        let data_buf = 0x5000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CLBU, 0);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FBU, 0);
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        // READ DMA (28-bit) from high_lba.
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis28(&mut mem, ctba, ATA_CMD_READ_DMA, high_lba, 1);
        write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut out = [0u8; 4];
        mem.read_physical(data_buf, &mut out);
        assert_eq!(out, [0xAA, 0xBB, 0xCC, 0xDD]);

        // WRITE DMA (28-bit) to high_lba+1.
        let write_buf = 0x6000;
        mem.write_physical(write_buf, &[1, 2, 3, 4]);
        mem.write_physical(write_buf + 4, &vec![0u8; SECTOR_SIZE - 4]);

        write_cmd_header(&mut mem, clb, 0, ctba, 1, true);
        write_cfis28(&mut mem, ctba, ATA_CMD_WRITE_DMA, high_lba + 1, 1);
        write_prdt(&mut mem, ctba, 0, write_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        // Verify using READ DMA (28-bit).
        let verify_buf = 0x7000;
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis28(&mut mem, ctba, ATA_CMD_READ_DMA, high_lba + 1, 1);
        write_prdt(&mut mem, ctba, 0, verify_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut verify = [0u8; 4];
        mem.read_physical(verify_buf, &mut verify);
        assert_eq!(verify, [1, 2, 3, 4]);
        assert!(irq.level());
    }

    #[test]
    fn read_write_pio_28_roundtrip() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq.clone()), 1);
        let mut mem = TestMemory::new(0x20_000);

        // Disk with one sector at LBA=4 containing a marker.
        let capacity = 64 * SECTOR_SIZE as u64;
        let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let mut sector = vec![0u8; SECTOR_SIZE];
        sector[0..4].copy_from_slice(&[9, 8, 7, 6]);
        disk.write_sectors(4, &sector).unwrap();
        ctl.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());

        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;
        let data_buf = 0x5000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        // READ SECTORS (PIO) (LBA=4, 1 sector).
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis28(&mut mem, ctba, ATA_CMD_READ_SECTORS, 4, 1);
        write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut out = [0u8; 4];
        mem.read_physical(data_buf, &mut out);
        assert_eq!(out, [9, 8, 7, 6]);

        // WRITE SECTORS (PIO) (LBA=5, 1 sector).
        let write_buf = 0x6000;
        mem.write_physical(write_buf, &[1, 2, 3, 4]);
        mem.write_physical(write_buf + 4, &vec![0u8; SECTOR_SIZE - 4]);

        write_cmd_header(&mut mem, clb, 0, ctba, 1, true);
        write_cfis28(&mut mem, ctba, ATA_CMD_WRITE_SECTORS, 5, 1);
        write_prdt(&mut mem, ctba, 0, write_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        // Verify by reading back via READ SECTORS (PIO).
        let verify_buf = 0x7000;
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis28(&mut mem, ctba, ATA_CMD_READ_SECTORS, 5, 1);
        write_prdt(&mut mem, ctba, 0, verify_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut verify = [0u8; 4];
        mem.read_physical(verify_buf, &mut verify);
        assert_eq!(verify, [1, 2, 3, 4]);
        assert!(irq.level());
    }

    #[test]
    fn read_write_pio_ext_roundtrip() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq.clone()), 1);
        let mut mem = TestMemory::new(0x20_000);

        // Use a sparse disk so we can validate 48-bit LBA parsing via the high LBA bytes.
        let high_lba: u64 = 0x0100_0000;
        let cfg = AeroSparseConfig {
            disk_size_bytes: (high_lba + 16) * SECTOR_SIZE as u64,
            block_size_bytes: 1024 * 1024,
        };
        let mut disk = AeroSparseDisk::create(MemBackend::new(), cfg).unwrap();

        let mut sector = vec![0u8; SECTOR_SIZE];
        sector[0..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        disk.write_sectors(high_lba, &sector).unwrap();
        ctl.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());

        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;
        let data_buf = 0x5000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        // READ SECTORS EXT (PIO 48-bit) (LBA=high_lba, 1 sector).
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis48(&mut mem, ctba, ATA_CMD_READ_SECTORS_EXT, high_lba, 1);
        write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut out = [0u8; 4];
        mem.read_physical(data_buf, &mut out);
        assert_eq!(out, [0x11, 0x22, 0x33, 0x44]);

        // WRITE SECTORS EXT (PIO 48-bit) (LBA=high_lba+1, 1 sector).
        let write_buf = 0x6000;
        mem.write_physical(write_buf, &[1, 2, 3, 4]);
        mem.write_physical(write_buf + 4, &vec![0u8; SECTOR_SIZE - 4]);

        write_cmd_header(&mut mem, clb, 0, ctba, 1, true);
        write_cfis48(&mut mem, ctba, ATA_CMD_WRITE_SECTORS_EXT, high_lba + 1, 1);
        write_prdt(&mut mem, ctba, 0, write_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        // Verify by reading back via READ SECTORS EXT.
        let verify_buf = 0x7000;
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis48(&mut mem, ctba, ATA_CMD_READ_SECTORS_EXT, high_lba + 1, 1);
        write_prdt(&mut mem, ctba, 0, verify_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut verify = [0u8; 4];
        mem.read_physical(verify_buf, &mut verify);
        assert_eq!(verify, [1, 2, 3, 4]);
        assert!(irq.level());
    }

    #[test]
    fn read_dma_28_roundtrip() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq.clone()), 1);
        let mut mem = TestMemory::new(0x20_000);

        // Disk with one sector at LBA=4 containing a marker.
        let capacity = 64 * SECTOR_SIZE as u64;
        let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let mut sector = vec![0u8; SECTOR_SIZE];
        sector[0..4].copy_from_slice(&[9, 8, 7, 6]);
        disk.write_sectors(4, &sector).unwrap();
        ctl.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());

        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;
        let data_buf = 0x5000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        // READ DMA (28-bit) (LBA=4, 1 sector).
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis_28(&mut mem, ctba, ATA_CMD_READ_DMA, 4, 1);
        write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut out = [0u8; 4];
        mem.read_physical(data_buf, &mut out);
        assert_eq!(out, [9, 8, 7, 6]);
        assert!(irq.level());
    }

    #[test]
    fn write_dma_28_roundtrip() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq.clone()), 1);
        let mut mem = TestMemory::new(0x20_000);

        let capacity = 64 * SECTOR_SIZE as u64;
        let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        ctl.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());

        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        // WRITE DMA (28-bit) (LBA=5, 1 sector).
        let write_buf = 0x6000;
        mem.write_physical(write_buf, &[1, 2, 3, 4]);
        mem.write_physical(write_buf + 4, &vec![0u8; SECTOR_SIZE - 4]);

        write_cmd_header(&mut mem, clb, 0, ctba, 1, true);
        write_cfis_28(&mut mem, ctba, ATA_CMD_WRITE_DMA, 5, 1);
        write_prdt(&mut mem, ctba, 0, write_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        // Verify via READ DMA EXT.
        let verify_buf = 0x7000;
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 5, 1);
        write_prdt(&mut mem, ctba, 0, verify_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut verify = [0u8; 4];
        mem.read_physical(verify_buf, &mut verify);
        assert_eq!(verify, [1, 2, 3, 4]);
        assert!(irq.level());
    }

    #[test]
    fn read_write_dma_28bit_roundtrip_includes_device_nibble() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq.clone()), 1);
        let mut mem = TestMemory::new(0x20_000);

        // Use LBAs that require the device register nibble (bits 24..27) for 28-bit commands.
        let lba_read: u64 = 0x0100_0004;
        let lba_write: u64 = 0x0100_0005;

        // Create a large *sparse* disk so we can use high LBAs without allocating gigabytes.
        let sector_count = lba_write + 0x10;
        let capacity_bytes = sector_count * SECTOR_SIZE as u64;
        let mut disk = AeroSparseDisk::create(
            MemBackend::new(),
            AeroSparseConfig {
                disk_size_bytes: capacity_bytes,
                block_size_bytes: 1024 * 1024,
            },
        )
        .unwrap();

        let mut sector = vec![0u8; SECTOR_SIZE];
        sector[0..4].copy_from_slice(&[9, 8, 7, 6]);
        disk.write_sectors(lba_read, &sector).unwrap();
        ctl.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());

        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;
        let data_buf = 0x5000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        // READ DMA (28-bit) (LBA=0x0100_0004, 1 sector).
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis_28(&mut mem, ctba, ATA_CMD_READ_DMA, lba_read, 1);
        write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut out = [0u8; 4];
        mem.read_physical(data_buf, &mut out);
        assert_eq!(out, [9, 8, 7, 6]);

        // WRITE DMA (28-bit) (LBA=0x0100_0005, 1 sector).
        let write_buf = 0x6000;
        mem.write_physical(write_buf, &[1, 2, 3, 4]);
        mem.write_physical(write_buf + 4, &vec![0u8; SECTOR_SIZE - 4]);

        write_cmd_header(&mut mem, clb, 0, ctba, 1, true);
        write_cfis_28(&mut mem, ctba, ATA_CMD_WRITE_DMA, lba_write, 1);
        write_prdt(&mut mem, ctba, 0, write_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        // Verify by reading back via READ DMA (28-bit).
        let verify_buf = 0x7000;
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis_28(&mut mem, ctba, ATA_CMD_READ_DMA, lba_write, 1);
        write_prdt(&mut mem, ctba, 0, verify_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut verify = [0u8; 4];
        mem.read_physical(verify_buf, &mut verify);
        assert_eq!(verify, [1, 2, 3, 4]);
        assert!(irq.level());
    }

    #[test]
    fn read_write_sectors_28bit_roundtrip() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq.clone()), 1);
        let mut mem = TestMemory::new(0x20_000);

        let capacity = 64 * SECTOR_SIZE as u64;
        let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let mut sector = vec![0u8; SECTOR_SIZE];
        sector[0..4].copy_from_slice(&[9, 8, 7, 6]);
        disk.write_sectors(4, &sector).unwrap();
        ctl.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());

        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;
        let data_buf = 0x5000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        // READ SECTORS (28-bit) (LBA=4, 1 sector).
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis_28(&mut mem, ctba, ATA_CMD_READ_SECTORS, 4, 1);
        write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut out = [0u8; 4];
        mem.read_physical(data_buf, &mut out);
        assert_eq!(out, [9, 8, 7, 6]);

        // WRITE SECTORS (28-bit) (LBA=5, 1 sector).
        let write_buf = 0x6000;
        mem.write_physical(write_buf, &[1, 2, 3, 4]);
        mem.write_physical(write_buf + 4, &vec![0u8; SECTOR_SIZE - 4]);

        write_cmd_header(&mut mem, clb, 0, ctba, 1, true);
        write_cfis_28(&mut mem, ctba, ATA_CMD_WRITE_SECTORS, 5, 1);
        write_prdt(&mut mem, ctba, 0, write_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        // Verify by reading back via READ SECTORS (28-bit).
        let verify_buf = 0x7000;
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis_28(&mut mem, ctba, ATA_CMD_READ_SECTORS, 5, 1);
        write_prdt(&mut mem, ctba, 0, verify_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut verify = [0u8; 4];
        mem.read_physical(verify_buf, &mut verify);
        assert_eq!(verify, [1, 2, 3, 4]);
        assert!(irq.level());
    }

    #[test]
    fn read_write_sectors_ext_sanity() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq.clone()), 1);
        let mut mem = TestMemory::new(0x20_000);

        let capacity = 64 * SECTOR_SIZE as u64;
        let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let mut sector = vec![0u8; SECTOR_SIZE];
        sector[0..4].copy_from_slice(&[9, 8, 7, 6]);
        disk.write_sectors(4, &sector).unwrap();
        ctl.attach_drive(0, AtaDrive::new(Box::new(disk)).unwrap());

        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;
        let data_buf = 0x5000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        // READ SECTORS EXT (48-bit) (LBA=4, 1 sector).
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis(&mut mem, ctba, ATA_CMD_READ_SECTORS_EXT, 4, 1);
        write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut out = [0u8; 4];
        mem.read_physical(data_buf, &mut out);
        assert_eq!(out, [9, 8, 7, 6]);

        // WRITE SECTORS EXT (48-bit) (LBA=5, 1 sector).
        let write_buf = 0x6000;
        mem.write_physical(write_buf, &[1, 2, 3, 4]);
        mem.write_physical(write_buf + 4, &vec![0u8; SECTOR_SIZE - 4]);

        write_cmd_header(&mut mem, clb, 0, ctba, 1, true);
        write_cfis(&mut mem, ctba, ATA_CMD_WRITE_SECTORS_EXT, 5, 1);
        write_prdt(&mut mem, ctba, 0, write_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        // Verify by reading back via READ SECTORS EXT (48-bit).
        let verify_buf = 0x7000;
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis(&mut mem, ctba, ATA_CMD_READ_SECTORS_EXT, 5, 1);
        write_prdt(&mut mem, ctba, 0, verify_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut verify = [0u8; 4];
        mem.read_physical(verify_buf, &mut verify);
        assert_eq!(verify, [1, 2, 3, 4]);
        assert!(irq.level());
    }

    #[test]
    fn sector_count_zero_semantics() {
        let cfis = [0u8; 64];
        assert_eq!(extract_sector_count_28(&cfis), 256);
        assert_eq!(extract_sector_count(&cfis), 65536);
    }

    #[test]
    fn pi_reports_all_ports_implemented_even_without_drives() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq), 4);

        assert_eq!(ctl.read_u32(HBA_REG_PI), 0b1111);
    }

    #[test]
    fn two_ports_irq_gating_and_w1c_clearing() {
        // Ensure:
        // - Multiple ports can complete commands in a single process() tick.
        // - HBA.IS aggregates per-port PxIS bits.
        // - IRQ assertion is gated by (GHC.IE && (PxIS & PxIE) != 0 for any port).
        // - PxIS uses W1C semantics and IRQ deasserts once no enabled pending interrupts remain.

        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq.clone()), 2);
        let mut mem = TestMemory::new(0x20_000);

        // Attach drives to both ports.
        let capacity = 32 * SECTOR_SIZE as u64;
        let disk0 = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let disk1 = RawDisk::create(MemBackend::new(), capacity).unwrap();
        ctl.attach_drive(0, AtaDrive::new(Box::new(disk0)).unwrap());
        ctl.attach_drive(1, AtaDrive::new(Box::new(disk1)).unwrap());

        let p0 = PORT_BASE;
        let p1 = PORT_BASE + PORT_STRIDE;

        // Program both ports (CLB/FB/CMD/IE). Leave global GHC.IE disabled so IRQ must not assert.
        let clb0 = 0x1000;
        let fb0 = 0x2000;
        let ctba0 = 0x3000;
        let data0 = 0x4000;

        let clb1 = 0x5000;
        let fb1 = 0x6000;
        let ctba1 = 0x7000;
        let data1 = 0x8000;

        for (pbase, clb, fb) in [(p0, clb0, fb0), (p1, clb1, fb1)] {
            ctl.write_u32(pbase + PORT_REG_CLB, clb as u32);
            ctl.write_u32(pbase + PORT_REG_CLBU, 0);
            ctl.write_u32(pbase + PORT_REG_FB, fb as u32);
            ctl.write_u32(pbase + PORT_REG_FBU, 0);
            ctl.write_u32(pbase + PORT_REG_IE, PORT_IS_DHRS);
            ctl.write_u32(pbase + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);
        }

        // IDENTIFY for both ports, issued before a single process() call.
        write_cmd_header(&mut mem, clb0, 0, ctba0, 1, false);
        write_cfis(&mut mem, ctba0, ATA_CMD_IDENTIFY, 0, 0);
        write_prdt(&mut mem, ctba0, 0, data0, SECTOR_SIZE as u32);

        write_cmd_header(&mut mem, clb1, 0, ctba1, 1, false);
        write_cfis(&mut mem, ctba1, ATA_CMD_IDENTIFY, 0, 0);
        write_prdt(&mut mem, ctba1, 0, data1, SECTOR_SIZE as u32);

        ctl.write_u32(p0 + PORT_REG_CI, 1);
        ctl.write_u32(p1 + PORT_REG_CI, 1);

        ctl.process(&mut mem);

        // Per-port interrupt status should be set for both ports.
        assert_ne!(ctl.read_u32(p0 + PORT_REG_IS) & PORT_IS_DHRS, 0);
        assert_ne!(ctl.read_u32(p1 + PORT_REG_IS) & PORT_IS_DHRS, 0);

        // HBA.IS aggregates pending port interrupts even if they are not enabled for IRQ.
        assert_eq!(ctl.read_u32(HBA_REG_IS), 0b11);

        // GHC.IE is still disabled: IRQ must remain deasserted.
        assert!(!irq.level());

        // Enabling GHC.IE must immediately assert IRQ because enabled pending interrupts exist.
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        assert!(irq.level());

        // Disabling GHC.IE must deassert IRQ even though PxIS and PxIE remain set.
        ctl.write_u32(HBA_REG_GHC, GHC_AE);
        assert!(!irq.level());

        // Re-enabling GHC.IE should re-assert due to the still-pending enabled interrupts.
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        assert!(irq.level());

        // Disabling all per-port IE bits must deassert IRQ even though PxIS is still set.
        ctl.write_u32(p0 + PORT_REG_IE, 0);
        ctl.write_u32(p1 + PORT_REG_IE, 0);
        assert!(!irq.level());
        assert_eq!(
            ctl.read_u32(HBA_REG_IS),
            0b11,
            "disabling PxIE must not affect aggregated HBA.IS"
        );

        // Re-enable PxIE only on port 1; IRQ should assert (port 1 has pending enabled PxIS).
        ctl.write_u32(p1 + PORT_REG_IE, PORT_IS_DHRS);
        assert!(irq.level());

        // Clear port 1's interrupt status via W1C; IRQ should now deassert because:
        // - port 0 still has pending PxIS but PxIE is disabled
        // - port 1 no longer has pending PxIS
        ctl.write_u32(p1 + PORT_REG_IS, PORT_IS_DHRS);
        assert!(!irq.level());
        assert_eq!(ctl.read_u32(HBA_REG_IS), 0b01);

        // Re-enable PxIE for port 0, and IRQ should assert due to its pending PxIS.
        ctl.write_u32(p0 + PORT_REG_IE, PORT_IS_DHRS);
        assert!(irq.level());

        // Clear the remaining pending interrupt; IRQ should deassert and HBA.IS clear.
        ctl.write_u32(p0 + PORT_REG_IS, PORT_IS_DHRS);
        assert!(!irq.level());
        assert_eq!(ctl.read_u32(HBA_REG_IS), 0);

        // Sanity-check that IDENTIFY DMA wrote data for both ports.
        let mut out0 = [0u8; SECTOR_SIZE];
        mem.read_physical(data0, &mut out0);
        assert_eq!(out0[0], 0x40);

        let mut out1 = [0u8; SECTOR_SIZE];
        mem.read_physical(data1, &mut out1);
        assert_eq!(out1[0], 0x40);

        // Ensure the IRQ line toggled as expected.
        assert_eq!(
            irq.transitions(),
            vec![true, false, true, false, true, false, true, false],
            "unexpected IRQ transition sequence; check IRQ gating logic"
        );
    }

    #[test]
    fn multi_port_pi_and_per_port_register_isolation() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq.clone()), 4);
        let mut mem = TestMemory::new(0x40_000);

        // Attach drives to a subset of ports. PI must still report all implemented ports.
        ctl.attach_drive(0, make_drive_with_sector_marker([0xAA, 0xBB, 0xCC, 0xDD], 4));
        ctl.attach_drive(2, make_drive_with_sector_marker([0x11, 0x22, 0x33, 0x44], 4));
        assert_eq!(ctl.read_u32(HBA_REG_PI), 0b1111);

        // Program HBA and ports.
        let p0_clb = 0x1000;
        let p0_fb = 0x2000;
        let p0_ctba = 0x3000;
        let p0_data = 0x4000;

        let p2_clb = 0x5000;
        let p2_fb = 0x6000;
        let p2_ctba = 0x7000;
        let p2_data = 0x8000;

        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);

        for (port, clb, fb) in [(0usize, p0_clb, p0_fb), (2usize, p2_clb, p2_fb)] {
            ctl.write_u32(port_reg(port, PORT_REG_CLB), clb as u32);
            ctl.write_u32(port_reg(port, PORT_REG_CLBU), 0);
            ctl.write_u32(port_reg(port, PORT_REG_FB), fb as u32);
            ctl.write_u32(port_reg(port, PORT_REG_FBU), 0);
            ctl.write_u32(port_reg(port, PORT_REG_IE), PORT_IS_DHRS);
            ctl.write_u32(port_reg(port, PORT_REG_CMD), PORT_CMD_ST | PORT_CMD_FRE);
        }

        // Set up READ DMA EXT commands for both ports, but only issue the command on port 0 first.
        write_cmd_header(&mut mem, p0_clb, 0, p0_ctba, 1, false);
        write_cfis(&mut mem, p0_ctba, ATA_CMD_READ_DMA_EXT, 4, 1);
        write_prdt(&mut mem, p0_ctba, 0, p0_data, SECTOR_SIZE as u32);

        write_cmd_header(&mut mem, p2_clb, 0, p2_ctba, 1, false);
        write_cfis(&mut mem, p2_ctba, ATA_CMD_READ_DMA_EXT, 4, 1);
        write_prdt(&mut mem, p2_ctba, 0, p2_data, SECTOR_SIZE as u32);

        ctl.write_u32(port_reg(0, PORT_REG_CI), 1);
        assert_eq!(
            ctl.read_u32(port_reg(2, PORT_REG_CI)),
            0,
            "writing PxCI on one port must not affect other ports"
        );

        ctl.process(&mut mem);

        assert!(irq.level(), "port 0 command completion should assert IRQ");
        assert_eq!(ctl.read_u32(port_reg(0, PORT_REG_CI)), 0);
        assert_ne!(ctl.read_u32(port_reg(0, PORT_REG_IS)) & PORT_IS_DHRS, 0);

        let mut out0 = [0u8; 4];
        mem.read_physical(p0_data, &mut out0);
        assert_eq!(out0, [0xAA, 0xBB, 0xCC, 0xDD]);

        // Port 2 should not observe any completion status until we issue its command.
        assert_eq!(
            ctl.read_u32(port_reg(2, PORT_REG_IS)),
            0,
            "completion on one port must not set PxIS on other ports"
        );
        let mut out2 = [0u8; 4];
        mem.read_physical(p2_data, &mut out2);
        assert_eq!(
            out2,
            [0, 0, 0, 0],
            "DMA for one port must not write into another port's PRDT buffer"
        );

        // Clear port 0 interrupt status. IRQ should drop, since port 2 has no pending interrupt.
        ctl.write_u32(port_reg(0, PORT_REG_IS), PORT_IS_DHRS);
        assert!(!irq.level());

        // Now issue and process the command on port 2.
        ctl.write_u32(port_reg(2, PORT_REG_CI), 1);
        ctl.process(&mut mem);

        assert!(irq.level(), "port 2 command completion should assert IRQ");
        assert_eq!(ctl.read_u32(port_reg(2, PORT_REG_CI)), 0);
        assert_ne!(ctl.read_u32(port_reg(2, PORT_REG_IS)) & PORT_IS_DHRS, 0);
        assert_eq!(
            ctl.read_u32(port_reg(0, PORT_REG_IS)),
            0,
            "processing another port must not re-set cleared PxIS bits"
        );

        mem.read_physical(p2_data, &mut out2);
        assert_eq!(out2, [0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn cap2_boh_and_bohc_reads_writes_are_stable() {
        let irq = TestIrqLine::default();
        let mut ctl = AhciController::new(Box::new(irq), 1);

        // CAP2.BOH must be set if BOHC is exposed.
        assert_ne!(ctl.read_u32(HBA_REG_CAP2) & CAP2_BOH, 0);

        // Claim OS ownership; emulation should immediately clear BOS.
        ctl.write_u32(HBA_REG_BOHC, BOHC_OOS);
        let bohc = ctl.read_u32(HBA_REG_BOHC);
        assert_ne!(bohc & BOHC_OOS, 0);
        assert_eq!(bohc & BOHC_BOS, 0);

        // OOC is W1C and should be safe to clear even if set.
        ctl.write_u32(HBA_REG_BOHC, BOHC_OOC);
        assert_eq!(ctl.read_u32(HBA_REG_BOHC) & BOHC_OOC, 0);
    }

    #[test]
    fn unsupported_command_sets_tfes_and_dhrs_and_clears_irq() {
        let (mut ctl, irq, mut mem, drive) = setup_controller();
        ctl.attach_drive(0, drive);

        // Program HBA and port.
        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CLBU, 0);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FBU, 0);
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS | PORT_IS_TFES);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        // Issue an unsupported command (0x00).
        write_cmd_header(&mut mem, clb, 0, ctba, 0, false);
        write_cfis48(&mut mem, ctba, 0x00, 0, 0);

        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        assert!(irq.level());
        assert_eq!(
            ctl.read_u32(PORT_BASE + PORT_REG_IS) & (PORT_IS_DHRS | PORT_IS_TFES),
            PORT_IS_DHRS | PORT_IS_TFES
        );

        let tfd = ctl.read_u32(PORT_BASE + PORT_REG_TFD);
        assert_ne!(tfd & (ATA_STATUS_ERR as u32), 0);
        assert_eq!(((tfd >> 8) & 0xFF) as u8, ATA_ERROR_ABRT);

        // Clearing interrupt bits should deassert the IRQ line.
        ctl.write_u32(PORT_BASE + PORT_REG_IS, PORT_IS_DHRS | PORT_IS_TFES);
        assert!(!irq.level());
    }

    #[test]
    fn dma_rejects_excessive_prdt_entries() {
        // The AHCI PRDT entry count is guest-controlled. Ensure we reject pathological counts
        // early (without iterating or allocating per-entry buffers).
        let (_ctl, _irq, mut mem, mut drive) = setup_controller();
        let header = CommandHeader {
            ctba: 0,
            prdtl: 40_000,
        };

        let err =
            dma_read_sectors_into_guest(&mut mem, &header, &mut drive, 0, SECTOR_SIZE).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(err.to_string(), "PRDT too large for DMA read");

        let err = dma_write_sectors_from_guest(&mut mem, &header, &mut drive, 0, SECTOR_SIZE)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(err.to_string(), "PRDT too large for DMA write");
    }

    #[test]
    fn dma_rejects_empty_prdt() {
        // PRDTL=0 cannot satisfy any non-empty DMA transfer.
        let (_ctl, _irq, mut mem, mut drive) = setup_controller();
        let header = CommandHeader { ctba: 0, prdtl: 0 };

        let err =
            dma_read_sectors_into_guest(&mut mem, &header, &mut drive, 0, SECTOR_SIZE).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(err.to_string(), "PRDT too small for DMA write");

        let err = dma_write_sectors_from_guest(&mut mem, &header, &mut drive, 0, SECTOR_SIZE)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert_eq!(err.to_string(), "PRDT too small for DMA read");
    }

    #[test]
    fn comreset_allows_reidentify_and_updates_ssts_det() {
        let (mut ctl, irq, mut mem, drive) = setup_controller();
        ctl.attach_drive(0, drive);

        // Program HBA and port.
        let clb = 0x1000;
        let fb = 0x2000;
        let ctba = 0x3000;
        let data_buf = 0x4000;

        ctl.write_u32(PORT_BASE + PORT_REG_CLB, clb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CLBU, 0);
        ctl.write_u32(PORT_BASE + PORT_REG_FB, fb as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_FBU, 0);
        ctl.write_u32(HBA_REG_GHC, GHC_IE | GHC_AE);
        ctl.write_u32(PORT_BASE + PORT_REG_IE, PORT_IS_DHRS);
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);

        let run_identify = |ctl: &mut AhciController, mem: &mut TestMemory| {
            write_cmd_header(mem, clb, 0, ctba, 1, false);
            write_cfis(mem, ctba, ATA_CMD_IDENTIFY, 0, 0);
            write_prdt(mem, ctba, 0, data_buf, SECTOR_SIZE as u32);

            ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
            ctl.process(mem);

            assert!(irq.level(), "IDENTIFY should assert IRQ");
            assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_CI), 0);

            let mut out = [0u8; SECTOR_SIZE];
            mem.read_physical(data_buf, &mut out);
            assert_eq!(out[0], 0x40);

            ctl.write_u32(PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
            assert!(!irq.level());
        };

        // Initial IDENTIFY should succeed with DET=3.
        assert_eq!(
            ctl.read_u32(PORT_BASE + PORT_REG_SSTS) & SSTS_DET_MASK,
            SSTS_DET_DEVICE_PRESENT_PHY
        );
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SIG), SATA_SIG_ATA);
        assert_eq!(
            ctl.read_u32(PORT_BASE + PORT_REG_TFD) & 0xFF,
            (ATA_STATUS_DRDY | ATA_STATUS_DSC) as u32
        );
        run_identify(&mut ctl, &mut mem);

        // Issue COMRESET (DET=1) then release it back to DET=0.
        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, SCTL_DET_COMRESET);
        assert_ne!(ctl.read_u32(PORT_BASE + PORT_REG_TFD) & (ATA_STATUS_BSY as u32), 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SIG), 0);
        // While reset is asserted, report device present but PHY not established.
        assert_eq!(
            ctl.read_u32(PORT_BASE + PORT_REG_SSTS) & SSTS_DET_MASK,
            SSTS_DET_DEVICE_PRESENT_NO_PHY
        );

        ctl.write_u32(PORT_BASE + PORT_REG_SCTL, 0);
        assert_eq!(
            ctl.read_u32(PORT_BASE + PORT_REG_SSTS) & SSTS_DET_MASK,
            SSTS_DET_DEVICE_PRESENT_PHY
        );
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SIG), SATA_SIG_ATA);
        assert_eq!(
            ctl.read_u32(PORT_BASE + PORT_REG_TFD) & 0xFF,
            (ATA_STATUS_DRDY | ATA_STATUS_DSC) as u32
        );

        // Port reset stops the command/FIS engines; re-enable and IDENTIFY again.
        ctl.write_u32(PORT_BASE + PORT_REG_CMD, PORT_CMD_ST | PORT_CMD_FRE);
        run_identify(&mut ctl, &mut mem);
    }

    #[test]
    fn pxserr_is_write_one_to_clear() {
        let (mut ctl, _irq, _mem, _drive) = setup_controller();

        ctl.ports[0].regs.serr = 0xA5A5_A5A5;

        // Only bits written as '1' should clear.
        ctl.write_u32(PORT_BASE + PORT_REG_SERR, 0x0000_F0F0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SERR), 0xA5A5_0505);

        // Writing zeros should be a no-op.
        ctl.write_u32(PORT_BASE + PORT_REG_SERR, 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_SERR), 0xA5A5_0505);
    }
}
