//! Minimal AHCI (SATA) controller emulation.
//!
//! The goal of this module is *not* a full AHCI implementation; it is focused
//! on the subset needed for Windows 7's inbox AHCI driver (`msahci.sys`; `storahci.sys` on Win8+)
//! to enumerate a single SATA disk and perform DMA-based I/O reliably.

pub mod command;
pub mod fis;
pub mod registers;

use crate::io::pci::{MmioDevice, PciConfigSpace, PciDevice};
use crate::io::storage::disk::{DiskBackend, DiskError};
use memory::MemoryBus;

use command::{CommandHeader, PrdEntry};
use fis::{build_reg_d2h_fis, RegH2dFis};
use registers::*;

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotResult, SnapshotVersion};
use aero_io_snapshot::io::storage::state::{AhciControllerState, AhciHbaState, AhciPortState};

const ATA_CMD_IDENTIFY_DEVICE: u8 = 0xec;
const ATA_CMD_SET_FEATURES: u8 = 0xef;
const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
const ATA_CMD_FLUSH_CACHE: u8 = 0xe7;
const ATA_CMD_FLUSH_CACHE_EXT: u8 = 0xea;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AhciError {
    Disk(DiskError),
    InvalidPrdt,
    InvalidFis,
    UnsupportedCommand(u8),
}

impl From<DiskError> for AhciError {
    fn from(value: DiskError) -> Self {
        Self::Disk(value)
    }
}

#[derive(Debug, Clone)]
struct HbaRegs {
    cap: u32,
    ghc: u32,
    is: u32,
    pi: u32,
    vs: u32,
    cap2: u32,
    bohc: u32,
}

impl HbaRegs {
    fn new(num_ports: u32) -> Self {
        let np = num_ports.saturating_sub(1) & CAP_NP_MASK;
        let ncs = 31u32 << 8;
        let cap = CAP_S64A | ncs | np;

        Self {
            cap,
            ghc: GHC_AE,
            is: 0,
            pi: (1u32 << num_ports) - 1,
            vs: 0x0001_0300, // AHCI 1.3
            cap2: 1,         // BOH
            bohc: 0,
        }
    }
}

#[derive(Debug, Clone)]
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
    sntf: u32,
    fbs: u32,
}

impl PortRegs {
    fn new_disk_present() -> Self {
        // Present + link up (DET=3), 3.0Gbps (SPD=2), active (IPM=1).
        let ssts = 0x0000_0123;
        let status = (ATA_SR_DRDY | ATA_SR_DSC) as u32;
        Self {
            clb: 0,
            fb: 0,
            is: 0,
            ie: 0,
            cmd: 0,
            tfd: status,
            sig: SATA_SIG_ATA,
            ssts,
            sctl: 0,
            serr: 0,
            sact: 0,
            ci: 0,
            sntf: 0,
            fbs: 0,
        }
    }

    fn set_tfd(&mut self, status: u8, error: u8) {
        self.tfd = (status as u32) | ((error as u32) << 8);
    }
}

pub struct AhciController {
    hba: HbaRegs,
    port0: PortRegs,
    disk: Box<dyn DiskBackend>,
    irq_level: bool,
}

impl AhciController {
    pub const ABAR_SIZE: u64 = 0x2000;

    pub fn new(disk: Box<dyn DiskBackend>) -> Self {
        let mut this = Self {
            hba: HbaRegs::new(1),
            port0: PortRegs::new_disk_present(),
            disk,
            irq_level: false,
        };
        this.sync_cmd_running_bits();
        this
    }

    pub fn irq_level(&self) -> bool {
        self.irq_level
    }

    pub fn poll(&mut self, mem: &mut dyn MemoryBus) {
        self.process_port0(mem);
        self.update_irq();
    }

    fn reset(&mut self) {
        self.hba = HbaRegs::new(1);
        self.port0 = PortRegs::new_disk_present();
        self.irq_level = false;
        self.sync_cmd_running_bits();
    }

    fn sync_cmd_running_bits(&mut self) {
        // Maintain read-only FR/CR bits based on ST/FRE.
        let mut cmd = self.port0.cmd & !(PXCMD_FR | PXCMD_CR);
        if cmd & PXCMD_FRE != 0 {
            cmd |= PXCMD_FR;
        }
        if cmd & PXCMD_ST != 0 {
            cmd |= PXCMD_CR;
        }
        self.port0.cmd = cmd;
    }

    fn update_hba_is(&mut self) {
        let mut is = 0u32;
        if self.port0.is != 0 {
            is |= 1 << 0;
        }
        self.hba.is = is;
    }

    fn update_irq(&mut self) {
        self.update_hba_is();

        let global_ie = self.hba.ghc & GHC_IE != 0;
        let pending = global_ie && (self.port0.is & self.port0.ie) != 0;

        self.irq_level = pending;
    }

    fn set_port_interrupt(&mut self, bits: u32) {
        self.port0.is |= bits;
        self.update_irq();
    }

    fn clear_port_interrupt(&mut self, w1c: u32) {
        self.port0.is &= !w1c;
        self.update_irq();
    }

    fn process_port0(&mut self, mem: &mut dyn MemoryBus) {
        self.sync_cmd_running_bits();

        // Only process commands if FRE + ST are enabled.
        if (self.port0.cmd & (PXCMD_ST | PXCMD_FRE)) != (PXCMD_ST | PXCMD_FRE) {
            return;
        }

        while self.port0.ci != 0 {
            let slot = self.port0.ci.trailing_zeros() as u8;
            let slot_mask = 1u32 << slot;

            // Mark busy.
            self.port0.set_tfd(ATA_SR_BSY, 0);

            let result = self.execute_slot(mem, slot);

            // Clear command bit regardless of success; Windows expects progress.
            self.port0.ci &= !slot_mask;

            let is_bits = match result {
                Ok(()) => {
                    self.port0.set_tfd(ATA_SR_DRDY | ATA_SR_DSC, 0);
                    self.write_d2h_fis(mem, ATA_SR_DRDY | ATA_SR_DSC, 0);
                    PXIS_DHRS
                }
                Err(err) => {
                    // Abort the command: set ERR status + ABRT error.
                    let status = ATA_SR_DRDY | ATA_SR_DSC | ATA_SR_ERR;
                    let error = 0x04;
                    self.port0.set_tfd(status, error);
                    self.write_d2h_fis(mem, status, error);

                    // For now we only log via debug formatting in the error
                    // path (no external logger dependency).
                    let _ = err;
                    PXIS_DHRS | PXIS_TFES
                }
            };

            self.set_port_interrupt(is_bits);
        }
    }

    fn execute_slot(&mut self, mem: &mut dyn MemoryBus, slot: u8) -> Result<(), AhciError> {
        let clb = self.port0.clb;
        if clb == 0 {
            return Err(AhciError::InvalidPrdt);
        }
        let header_addr = clb + (slot as u64) * CommandHeader::SIZE as u64;
        let header = CommandHeader::read_from(mem, header_addr);

        let mut cfis = [0u8; 64];
        mem.read_physical(header.ctba, &mut cfis);
        let fis_len = usize::from(header.cfl_bytes).min(cfis.len());
        let fis = RegH2dFis::parse(&cfis[..fis_len]).ok_or(AhciError::InvalidFis)?;

        match fis.command {
            ATA_CMD_IDENTIFY_DEVICE => self.cmd_identify(mem, header_addr, &header),
            ATA_CMD_READ_DMA_EXT => self.cmd_read_dma_ext(mem, header_addr, &header, fis),
            ATA_CMD_WRITE_DMA_EXT => self.cmd_write_dma_ext(mem, header_addr, &header, fis),
            ATA_CMD_FLUSH_CACHE => self.cmd_flush(mem, header_addr, &header),
            ATA_CMD_FLUSH_CACHE_EXT => self.cmd_flush(mem, header_addr, &header),
            ATA_CMD_SET_FEATURES => self.cmd_set_features(mem, header_addr, &header),
            other => Err(AhciError::UnsupportedCommand(other)),
        }
    }

    fn read_prdt(
        &self,
        mem: &mut dyn MemoryBus,
        header: &CommandHeader,
    ) -> Result<Vec<PrdEntry>, AhciError> {
        if header.prdt_len == 0 {
            return Err(AhciError::InvalidPrdt);
        }
        let mut prdt = Vec::with_capacity(header.prdt_len as usize);
        let mut addr = header.ctba + 0x80;
        for _ in 0..header.prdt_len {
            prdt.push(PrdEntry::read_from(mem, addr));
            addr += PrdEntry::SIZE as u64;
        }
        Ok(prdt)
    }

    fn dma_write_to_guest(
        &self,
        mem: &mut dyn MemoryBus,
        prdt: &[PrdEntry],
        src: &[u8],
    ) -> Result<usize, AhciError> {
        let mut offset = 0usize;
        for prd in prdt {
            if offset >= src.len() {
                break;
            }
            let len = prd.byte_count().min(src.len() - offset);
            mem.write_physical(prd.dba, &src[offset..offset + len]);
            offset += len;
        }
        if offset != src.len() {
            return Err(AhciError::InvalidPrdt);
        }
        Ok(offset)
    }

    fn dma_read_from_guest(
        &self,
        mem: &mut dyn MemoryBus,
        prdt: &[PrdEntry],
        dst: &mut [u8],
    ) -> Result<usize, AhciError> {
        let mut offset = 0usize;
        for prd in prdt {
            if offset >= dst.len() {
                break;
            }
            let len = prd.byte_count().min(dst.len() - offset);
            mem.read_physical(prd.dba, &mut dst[offset..offset + len]);
            offset += len;
        }
        if offset != dst.len() {
            return Err(AhciError::InvalidPrdt);
        }
        Ok(offset)
    }

    fn cmd_identify(
        &mut self,
        mem: &mut dyn MemoryBus,
        header_addr: u64,
        header: &CommandHeader,
    ) -> Result<(), AhciError> {
        let prdt = self.read_prdt(mem, header)?;

        let identify = build_identify_data(self.disk.total_sectors(), self.disk.sector_size());
        let transferred = self.dma_write_to_guest(mem, &prdt, &identify)?;
        CommandHeader::write_prdbc(mem, header_addr, transferred as u32);
        Ok(())
    }

    fn cmd_read_dma_ext(
        &mut self,
        mem: &mut dyn MemoryBus,
        header_addr: u64,
        header: &CommandHeader,
        fis: RegH2dFis,
    ) -> Result<(), AhciError> {
        let sector_size = self.disk.sector_size() as usize;
        let sectors = if fis.sector_count == 0 {
            65_536u32
        } else {
            fis.sector_count as u32
        } as usize;
        let byte_len = sector_size
            .checked_mul(sectors)
            .ok_or(AhciError::InvalidPrdt)?;

        let mut buf = vec![0u8; byte_len];
        self.disk.read_sectors(fis.lba, &mut buf)?;

        let prdt = self.read_prdt(mem, header)?;
        let transferred = self.dma_write_to_guest(mem, &prdt, &buf)?;
        CommandHeader::write_prdbc(mem, header_addr, transferred as u32);
        Ok(())
    }

    fn cmd_write_dma_ext(
        &mut self,
        mem: &mut dyn MemoryBus,
        header_addr: u64,
        header: &CommandHeader,
        fis: RegH2dFis,
    ) -> Result<(), AhciError> {
        let sector_size = self.disk.sector_size() as usize;
        let sectors = if fis.sector_count == 0 {
            65_536u32
        } else {
            fis.sector_count as u32
        } as usize;
        let byte_len = sector_size
            .checked_mul(sectors)
            .ok_or(AhciError::InvalidPrdt)?;

        let mut buf = vec![0u8; byte_len];
        let prdt = self.read_prdt(mem, header)?;
        let transferred = self.dma_read_from_guest(mem, &prdt, &mut buf)?;
        CommandHeader::write_prdbc(mem, header_addr, transferred as u32);

        self.disk.write_sectors(fis.lba, &buf)?;
        Ok(())
    }

    fn cmd_flush(
        &mut self,
        mem: &mut dyn MemoryBus,
        header_addr: u64,
        _header: &CommandHeader,
    ) -> Result<(), AhciError> {
        self.disk.flush()?;
        CommandHeader::write_prdbc(mem, header_addr, 0);
        Ok(())
    }

    fn cmd_set_features(
        &mut self,
        mem: &mut dyn MemoryBus,
        header_addr: u64,
        _header: &CommandHeader,
    ) -> Result<(), AhciError> {
        // Windows probes several feature sets (e.g. enabling write cache). We
        // accept and report success without modelling the feature state.
        CommandHeader::write_prdbc(mem, header_addr, 0);
        Ok(())
    }

    fn write_d2h_fis(&self, mem: &mut dyn MemoryBus, status: u8, error: u8) {
        if self.port0.fb == 0 {
            return;
        }
        let fis = build_reg_d2h_fis(status, error);
        // D2H Register FIS is stored at offset 0x40 of the received FIS area.
        mem.write_physical(self.port0.fb + 0x40, &fis);
    }

    fn port0_mmio_read_dword(&mut self, offset: u64) -> u32 {
        match offset {
            PX_CLB => self.port0.clb as u32,
            PX_CLBU => (self.port0.clb >> 32) as u32,
            PX_FB => self.port0.fb as u32,
            PX_FBU => (self.port0.fb >> 32) as u32,
            PX_IS => self.port0.is,
            PX_IE => self.port0.ie,
            PX_CMD => self.port0.cmd,
            PX_TFD => self.port0.tfd,
            PX_SIG => self.port0.sig,
            PX_SSTS => self.port0.ssts,
            PX_SCTL => self.port0.sctl,
            PX_SERR => self.port0.serr,
            PX_SACT => self.port0.sact,
            PX_CI => self.port0.ci,
            PX_SNTF => self.port0.sntf,
            PX_FBS => self.port0.fbs,
            _ => 0,
        }
    }

    fn port0_mmio_write_dword(&mut self, mem: &mut dyn MemoryBus, offset: u64, value: u32) {
        match offset {
            PX_CLB => self.port0.clb = (self.port0.clb & 0xffff_ffff_0000_0000) | value as u64,
            PX_CLBU => {
                self.port0.clb = (self.port0.clb & 0x0000_0000_ffff_ffff) | ((value as u64) << 32)
            }
            PX_FB => self.port0.fb = (self.port0.fb & 0xffff_ffff_0000_0000) | value as u64,
            PX_FBU => {
                self.port0.fb = (self.port0.fb & 0x0000_0000_ffff_ffff) | ((value as u64) << 32)
            }
            PX_IS => self.clear_port_interrupt(value),
            PX_IE => {
                self.port0.ie = value;
                self.update_irq();
            }
            PX_CMD => {
                // Ignore read-only bits.
                self.port0.cmd = value & !(PXCMD_FR | PXCMD_CR);
                self.sync_cmd_running_bits();
                self.update_irq();
            }
            PX_SCTL => self.port0.sctl = value,
            PX_SERR => {
                // Write-1-to-clear.
                self.port0.serr &= !value;
            }
            PX_SACT => self.port0.sact = value,
            PX_CI => {
                self.port0.ci = value;
                self.poll(mem);
            }
            PX_SNTF => self.port0.sntf = value,
            PX_FBS => self.port0.fbs = value,
            _ => {}
        }
    }

    fn hba_mmio_read_dword(&mut self, offset: u64) -> u32 {
        match offset {
            HBA_CAP => self.hba.cap,
            HBA_GHC => self.hba.ghc,
            HBA_IS => self.hba.is,
            HBA_PI => self.hba.pi,
            HBA_VS => self.hba.vs,
            HBA_CAP2 => self.hba.cap2,
            HBA_BOHC => self.hba.bohc,
            _ => 0,
        }
    }

    fn hba_mmio_write_dword(&mut self, offset: u64, value: u32) {
        match offset {
            HBA_GHC => {
                if value & GHC_HR != 0 {
                    self.reset();
                    return;
                }

                // Only IE and AE are writable in this minimal model.
                let ie = value & GHC_IE;
                let ae = value & GHC_AE;
                self.hba.ghc = ae | ie;
                self.update_irq();
            }
            HBA_IS => {
                // Write-1-to-clear.
                self.hba.is &= !value;
                // Also clear port status for any cleared port bits so the
                // computed value remains consistent.
                if value & 1 != 0 {
                    self.port0.is = 0;
                }
                self.update_irq();
            }
            HBA_BOHC => self.hba.bohc = value,
            _ => {}
        }
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
            ports: vec![AhciPortState {
                clb: self.port0.clb,
                fb: self.port0.fb,
                is: self.port0.is,
                ie: self.port0.ie,
                cmd: self.port0.cmd,
                tfd: self.port0.tfd,
                sig: self.port0.sig,
                ssts: self.port0.ssts,
                sctl: self.port0.sctl,
                serr: self.port0.serr,
                sact: self.port0.sact,
                ci: self.port0.ci,
            }],
        }
    }

    pub fn restore_state(&mut self, state: &AhciControllerState) {
        // Reset internal-only / derived state to a deterministic baseline first.
        // The canonical AHCI snapshot schema only includes a subset of registers.
        self.hba = HbaRegs::new(1);
        self.port0 = PortRegs::new_disk_present();

        self.hba.cap = state.hba.cap;
        self.hba.ghc = state.hba.ghc;
        self.hba.cap2 = state.hba.cap2;
        self.hba.bohc = state.hba.bohc;
        self.hba.vs = state.hba.vs;

        // Apply port0 register state if present. Any extra emulator-only registers
        // (e.g. PxSNTF, PxFBS) restore to their default values.
        if let Some(p) = state.ports.first() {
            self.port0.clb = p.clb;
            self.port0.fb = p.fb;
            self.port0.is = p.is;
            self.port0.ie = p.ie;
            self.port0.cmd = p.cmd;
            self.port0.tfd = p.tfd;
            self.port0.sig = p.sig;
            self.port0.ssts = p.ssts;
            self.port0.sctl = p.sctl;
            self.port0.serr = p.serr;
            self.port0.sact = p.sact;
            self.port0.ci = p.ci;
        }

        // Recompute derived bits / IRQ line level.
        self.sync_cmd_running_bits();
        self.update_irq();
    }
}

impl IoSnapshot for AhciController {
    // NOTE: This device snapshots via the canonical AHCI schema defined in
    // `aero-io-snapshot` (`AhciControllerState`). This ensures that
    // `{DEVICE_ID="AHCI", major=1}` maps to a single unambiguous encoding across the workspace.
    const DEVICE_ID: [u8; 4] = <AhciControllerState as IoSnapshot>::DEVICE_ID;
    const DEVICE_VERSION: SnapshotVersion = <AhciControllerState as IoSnapshot>::DEVICE_VERSION;

    fn save_state(&self) -> Vec<u8> {
        self.snapshot_state().save_state()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        let mut state = AhciControllerState::default();
        state.load_state(bytes)?;
        self.restore_state(&state);
        Ok(())
    }
}

impl MmioDevice for AhciController {
    fn mmio_read(&mut self, _mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        let aligned = offset & !3;
        let shift = (offset & 3) * 8;
        let value = if aligned >= HBA_PORTS_BASE {
            let port_off = aligned - HBA_PORTS_BASE;
            if port_off < HBA_PORT_STRIDE {
                self.port0_mmio_read_dword(port_off)
            } else {
                0
            }
        } else {
            self.hba_mmio_read_dword(aligned)
        };

        match size {
            1 => (value >> shift) & 0xff,
            2 => (value >> shift) & 0xffff,
            4 => value,
            _ => 0,
        }
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        let aligned = offset & !3;
        let shift = (offset & 3) * 8;

        let value32 = match size {
            1 => (value & 0xff) << shift,
            2 => (value & 0xffff) << shift,
            4 => value,
            _ => return,
        };

        // For sub-dword writes, merge with current value.
        let merged = if size == 4 {
            value32
        } else {
            let cur = self.mmio_read(mem, aligned, 4);
            let mask = match size {
                1 => 0xffu32 << shift,
                2 => 0xffffu32 << shift,
                _ => 0,
            };
            (cur & !mask) | value32
        };

        if aligned >= HBA_PORTS_BASE {
            let port_off = aligned - HBA_PORTS_BASE;
            if port_off < HBA_PORT_STRIDE {
                self.port0_mmio_write_dword(mem, port_off, merged);
            }
        } else {
            self.hba_mmio_write_dword(aligned, merged);
        }
    }
}

impl AhciController {
    /// Convenience wrapper around the [`MmioDevice`] trait methods.
    pub fn mmio_read_u32(&mut self, mem: &mut dyn MemoryBus, offset: u64) -> u32 {
        self.mmio_read(mem, offset, 4)
    }

    /// Convenience wrapper around the [`MmioDevice`] trait methods.
    pub fn mmio_write_u32(&mut self, mem: &mut dyn MemoryBus, offset: u64, value: u32) {
        self.mmio_write(mem, offset, 4, value);
    }
}

/// A PCI wrapper that exposes the AHCI controller as a class-code compatible
/// SATA/AHCI device (0x010601) with an ABAR MMIO BAR.
pub struct AhciPciDevice {
    config: PciConfigSpace,
    pub abar: u32,
    abar_probe: bool,
    pub controller: AhciController,
}

impl AhciPciDevice {
    pub fn new(controller: AhciController, abar: u32) -> Self {
        let mut config = PciConfigSpace::new();

        // Vendor / device (Intel ICH9 AHCI).
        config.set_u16(0x00, 0x8086);
        config.set_u16(0x02, 0x2922);

        // Class code: mass storage / SATA / AHCI 1.0.
        config.write(0x09, 1, 0x01); // prog IF
        config.write(0x0a, 1, 0x06); // subclass
        config.write(0x0b, 1, 0x01); // class

        // BAR5 (ABAR) at 0x24.
        // Non-prefetchable 32-bit MMIO.
        config.set_u32(0x24, abar & 0xffff_fff0);

        // Interrupt pin INTA#.
        config.write(0x3d, 1, 1);

        Self {
            config,
            abar,
            abar_probe: false,
            controller,
        }
    }
}

impl PciDevice for AhciPciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if offset == 0x24 && size == 4 {
            if self.abar_probe {
                return !(AhciController::ABAR_SIZE as u32 - 1) & 0xffff_fff0;
            }
            return self.abar;
        }
        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        // Allow BAR relocation and command register toggles.
        if offset == 0x24 && size == 4 {
            if value == 0xffff_ffff {
                self.abar_probe = true;
                self.abar = 0;
                self.config.write(offset, size, 0);
                return;
            }
            self.abar_probe = false;
            self.abar = value & 0xffff_fff0;
            self.config.write(offset, size, self.abar);
            return;
        }
        self.config.write(offset, size, value);
    }
}

impl MmioDevice for AhciPciDevice {
    fn mmio_read(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        self.controller.mmio_read(mem, offset, size)
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        self.controller.mmio_write(mem, offset, size, value)
    }
}

fn build_identify_data(total_sectors: u64, sector_size: u32) -> [u8; 512] {
    let mut words = [0u16; 256];

    // Word 0: general configuration (non-removable, hard disk).
    words[0] = 0x0040;

    // Provide plausible legacy CHS geometry (mostly ignored when LBA is supported).
    words[1] = 16383; // cylinders
    words[3] = 16; // heads
    words[6] = 63; // sectors/track

    // Word 49: capabilities (LBA + DMA).
    words[49] = (1 << 9) | (1 << 8);

    // Word 60-61: total number of user addressable sectors (LBA28).
    let lba28 = total_sectors.min(0x0fff_ffff) as u32;
    words[60] = (lba28 & 0xffff) as u16;
    words[61] = (lba28 >> 16) as u16;

    // Word 63: multiword DMA modes supported (we only claim mode 0).
    words[63] = 1;

    // Word 80: major version number.
    // Claiming ATA/ATAPI-8 is common in emulators and is well supported by OS drivers.
    words[80] = 0x007e;

    // Words 82-84: command sets supported.
    // Word 82 bit5: write cache supported.
    words[82] = 1 << 5;

    // Word 83: command sets supported (LBA48).
    words[83] = 1 << 10;
    // Words 85-87: command sets enabled.
    words[85] = words[82];
    words[86] = words[83];

    // Word 100-103: total number of user addressable sectors (LBA48).
    let lba48 = total_sectors;
    words[100] = (lba48 & 0xffff) as u16;
    words[101] = ((lba48 >> 16) & 0xffff) as u16;
    words[102] = ((lba48 >> 32) & 0xffff) as u16;
    words[103] = ((lba48 >> 48) & 0xffff) as u16;

    // Logical sector size (words 117-118) expressed in words if word106 indicates validity.
    // Windows 7 is happy with the default 512-byte assumption; only expose it when it is
    // not 512.
    if sector_size != 512 {
        // Word 106: physical sector size / logical sector size information valid.
        words[106] = 1 << 14;
        let words_per_sector = sector_size / 2;
        words[117] = (words_per_sector & 0xffff) as u16;
        words[118] = (words_per_sector >> 16) as u16;
    }

    write_ata_string(&mut words[10..20], "AERO0000000000000000"); // serial (20 chars)
    write_ata_string(&mut words[23..27], "1.0 "); // firmware rev (8 chars)
    write_ata_string(&mut words[27..47], "Aero Virtual SATA Disk"); // model (40 chars)

    let mut out = [0u8; 512];
    for (i, w) in words.iter().enumerate() {
        out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
    }
    out
}

fn write_ata_string(words: &mut [u16], s: &str) {
    // ATA strings are space padded and byte-swapped within each u16.
    let mut bytes = vec![b' '; words.len() * 2];
    let src = s.as_bytes();
    let len = src.len().min(bytes.len());
    bytes[..len].copy_from_slice(&src[..len]);

    for (i, w) in words.iter_mut().enumerate() {
        let a = bytes[i * 2];
        let b = bytes[i * 2 + 1];
        *w = u16::from_be_bytes([a, b]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::storage::disk::MemDisk;
    use memory::MemoryBus;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Debug)]
    struct VecMemory {
        data: Vec<u8>,
    }

    impl VecMemory {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
            }
        }

        fn range(&self, paddr: u64, len: usize) -> core::ops::Range<usize> {
            let start = usize::try_from(paddr).expect("paddr too large for VecMemory");
            let end = start.checked_add(len).expect("address wrap");
            assert!(end <= self.data.len(), "out-of-bounds physical access");
            start..end
        }
    }

    impl MemoryBus for VecMemory {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let range = self.range(paddr, buf.len());
            buf.copy_from_slice(&self.data[range]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let range = self.range(paddr, buf.len());
            self.data[range].copy_from_slice(buf);
        }
    }

    #[derive(Clone)]
    struct SharedDisk(Arc<Mutex<MemDisk>>);

    impl DiskBackend for SharedDisk {
        fn sector_size(&self) -> u32 {
            self.0.lock().unwrap().sector_size()
        }

        fn total_sectors(&self) -> u64 {
            self.0.lock().unwrap().total_sectors()
        }

        fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), DiskError> {
            self.0.lock().unwrap().read_sectors(lba, buf)
        }

        fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), DiskError> {
            self.0.lock().unwrap().write_sectors(lba, buf)
        }

        fn flush(&mut self) -> Result<(), DiskError> {
            self.0.lock().unwrap().flush()
        }
    }

    fn build_cmd_header(cfl_dwords: u32, write: bool, prdt_len: u16, ctba: u64) -> [u8; 32] {
        let mut buf = [0u8; 32];
        let mut dw0 = cfl_dwords & 0x1f;
        if write {
            dw0 |= 1 << 6;
        }
        dw0 |= (prdt_len as u32) << 16;
        buf[0..4].copy_from_slice(&dw0.to_le_bytes());
        buf[8..12].copy_from_slice(&(ctba as u32).to_le_bytes());
        buf[12..16].copy_from_slice(&((ctba >> 32) as u32).to_le_bytes());
        buf
    }

    fn write_reg_h2d_fis(mem: &mut VecMemory, addr: u64, cmd: u8, lba: u64, count: u16) {
        let mut fis = [0u8; 64];
        fis[0] = fis::FIS_TYPE_REG_H2D;
        fis[1] = 0x80;
        fis[2] = cmd;
        fis[7] = 0x40; // LBA mode
        fis[4] = (lba & 0xff) as u8;
        fis[5] = ((lba >> 8) & 0xff) as u8;
        fis[6] = ((lba >> 16) & 0xff) as u8;
        fis[8] = ((lba >> 24) & 0xff) as u8;
        fis[9] = ((lba >> 32) & 0xff) as u8;
        fis[10] = ((lba >> 40) & 0xff) as u8;
        fis[12] = (count & 0xff) as u8;
        fis[13] = (count >> 8) as u8;
        mem.write_physical(addr, &fis);
    }

    fn write_prd(mem: &mut VecMemory, addr: u64, dba: u64, byte_count: u32) {
        let dbc = (byte_count - 1) | (1u32 << 31);
        mem.write_u32(addr, dba as u32);
        mem.write_u32(addr + 4, (dba >> 32) as u32);
        mem.write_u32(addr + 12, dbc);
    }

    #[test]
    fn read_dma_ext_transfers_data_and_sets_interrupts() {
        let disk = Arc::new(Mutex::new(MemDisk::new(16)));
        // Fill sectors 2..4 with deterministic bytes.
        {
            let mut d = disk.lock().unwrap();
            for (i, b) in d.data_mut().iter_mut().enumerate() {
                *b = (i & 0xff) as u8;
            }
        }
        let shared_disk = SharedDisk(disk.clone());

        let mut mem = VecMemory::new(0x10_000);
        let mut controller = AhciController::new(Box::new(shared_disk));

        let clb = 0x1000u64;
        let fb = 0x2000u64;
        let ctba = 0x3000u64;
        let dst = 0x4000u64;

        // Program controller registers.
        controller.mmio_write_u32(&mut mem, HBA_GHC, GHC_AE | GHC_IE);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CLB, clb as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CLBU, (clb >> 32) as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_FB, fb as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_FBU, (fb >> 32) as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_IE, PXIE_DHRE);
        controller.mmio_write_u32(
            &mut mem,
            HBA_PORTS_BASE + PX_CMD,
            PXCMD_FRE | PXCMD_ST | PXCMD_SUD,
        );

        // Command header for slot 0.
        let header = build_cmd_header(5, false, 1, ctba);
        mem.write_physical(clb, &header);

        // Command table (CFIS + PRDT).
        write_reg_h2d_fis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 2, 2);
        write_prd(&mut mem, ctba + 0x80, dst, 1024);

        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CI, 1);
        controller.poll(&mut mem);

        // Data should have been DMA'd into dst.
        let mut got = vec![0u8; 1024];
        mem.read_physical(dst, &mut got);
        let disk_guard = disk.lock().unwrap();
        let expected = &disk_guard.data()[2 * 512..4 * 512];
        assert_eq!(got, expected);

        // Command completion bookkeeping.
        assert_eq!(controller.port0.ci, 0);
        assert_ne!(controller.port0.is & PXIS_DHRS, 0);
        assert_ne!(controller.hba.is & 1, 0);
        assert!(controller.irq_level());

        // Clear interrupt.
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_IS, PXIS_DHRS);
        assert_eq!(controller.port0.is & PXIS_DHRS, 0);
        assert!(!controller.irq_level());
    }

    #[test]
    fn write_dma_ext_supports_scatter_gather_with_odd_lengths() {
        let disk = Arc::new(Mutex::new(MemDisk::new(16)));
        let shared_disk = SharedDisk(disk.clone());

        let mut mem = VecMemory::new(0x20_000);
        let mut controller = AhciController::new(Box::new(shared_disk));

        let clb = 0x1000u64;
        let fb = 0x2000u64;
        let ctba = 0x3000u64;
        let src0 = 0x5000u64;
        let src1 = 0x6000u64;

        // Prepare 2 sectors of data split oddly across two PRDs.
        let mut data = vec![0u8; 1024];
        for (i, b) in data.iter_mut().enumerate() {
            *b = 255 - (i as u8);
        }
        mem.write_physical(src0, &data[..1000]);
        mem.write_physical(src1, &data[1000..]);

        controller.mmio_write_u32(&mut mem, HBA_GHC, GHC_AE | GHC_IE);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CLB, clb as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_FB, fb as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_IE, PXIE_DHRE);
        controller.mmio_write_u32(
            &mut mem,
            HBA_PORTS_BASE + PX_CMD,
            PXCMD_FRE | PXCMD_ST | PXCMD_SUD,
        );

        let header = build_cmd_header(5, true, 2, ctba);
        mem.write_physical(clb, &header);

        write_reg_h2d_fis(&mut mem, ctba, ATA_CMD_WRITE_DMA_EXT, 4, 2);
        write_prd(&mut mem, ctba + 0x80, src0, 1000);
        write_prd(&mut mem, ctba + 0x90, src1, 24);

        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CI, 1);
        controller.poll(&mut mem);

        let d = disk.lock().unwrap();
        let written = &d.data()[4 * 512..6 * 512];
        assert_eq!(written, &data[..]);
        assert!(controller.irq_level());
    }

    #[test]
    fn identify_device_returns_lba48_capacity_and_model_string() {
        let disk = Arc::new(Mutex::new(MemDisk::new(128)));
        let shared_disk = SharedDisk(disk.clone());

        let mut mem = VecMemory::new(0x20_000);
        let mut controller = AhciController::new(Box::new(shared_disk));

        let clb = 0x1000u64;
        let fb = 0x2000u64;
        let ctba = 0x3000u64;
        let dst = 0x7000u64;

        controller.mmio_write_u32(&mut mem, HBA_GHC, GHC_AE | GHC_IE);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CLB, clb as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_FB, fb as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_IE, PXIE_DHRE);
        controller.mmio_write_u32(
            &mut mem,
            HBA_PORTS_BASE + PX_CMD,
            PXCMD_FRE | PXCMD_ST | PXCMD_SUD,
        );

        let header = build_cmd_header(5, false, 1, ctba);
        mem.write_physical(clb, &header);

        write_reg_h2d_fis(&mut mem, ctba, ATA_CMD_IDENTIFY_DEVICE, 0, 0);
        write_prd(&mut mem, ctba + 0x80, dst, 512);

        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CI, 1);
        controller.poll(&mut mem);

        let mut identify = [0u8; 512];
        mem.read_physical(dst, &mut identify);
        let word0 = u16::from_le_bytes([identify[0], identify[1]]);
        assert_eq!(word0, 0x0040);

        // Word 83 bit10: LBA48 supported.
        let word83 = u16::from_le_bytes([identify[83 * 2], identify[83 * 2 + 1]]);
        assert_ne!(word83 & (1 << 10), 0);

        // Words 100-103: total sectors.
        let mut lba48 = 0u64;
        for i in 0..4 {
            let w =
                u16::from_le_bytes([identify[(100 + i) * 2], identify[(100 + i) * 2 + 1]]) as u64;
            lba48 |= w << (i * 16);
        }
        assert_eq!(lba48, 128);

        // Model string at words 27-46 (byte swapped).
        let mut model_bytes = [0u8; 40];
        model_bytes.copy_from_slice(&identify[27 * 2..27 * 2 + 40]);
        // Undo ATA byte swapping.
        for chunk in model_bytes.chunks_exact_mut(2) {
            chunk.swap(0, 1);
        }
        let model = core::str::from_utf8(&model_bytes).unwrap().trim();
        assert_eq!(model, "Aero Virtual SATA Disk");
    }

    #[test]
    fn snapshot_roundtrip_preserves_interrupt_and_mmio_state() {
        let disk = Arc::new(Mutex::new(MemDisk::new(16)));
        // Populate the disk with deterministic content.
        {
            let mut d = disk.lock().unwrap();
            for (i, b) in d.data_mut().iter_mut().enumerate() {
                *b = (i & 0xff) as u8;
            }
        }
        let shared_disk = SharedDisk(disk.clone());

        let mut mem = VecMemory::new(0x20_000);
        let mut controller = AhciController::new(Box::new(shared_disk.clone()));

        let clb = 0x1000u64;
        let fb = 0x2000u64;
        let ctba = 0x3000u64;
        let dst = 0x4000u64;

        controller.mmio_write_u32(&mut mem, HBA_GHC, GHC_AE | GHC_IE);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CLB, clb as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_FB, fb as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_IE, PXIE_DHRE);
        controller.mmio_write_u32(
            &mut mem,
            HBA_PORTS_BASE + PX_CMD,
            PXCMD_FRE | PXCMD_ST | PXCMD_SUD,
        );

        let header = build_cmd_header(5, false, 1, ctba);
        mem.write_physical(clb, &header);
        write_reg_h2d_fis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 2, 1);
        write_prd(&mut mem, ctba + 0x80, dst, 512);

        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CI, 1);
        assert!(controller.irq_level());

        let snap = controller.save_state();

        let mut restored = AhciController::new(Box::new(shared_disk));
        restored.load_state(&snap).unwrap();

        assert!(restored.irq_level());
        assert_eq!(
            restored.mmio_read_u32(&mut mem, HBA_PORTS_BASE + PX_IS) & PXIS_DHRS,
            PXIS_DHRS
        );

        // Clearing the interrupt after restore should deassert the IRQ line.
        restored.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_IS, PXIS_DHRS);
        assert!(!restored.irq_level());
    }

    #[test]
    fn snapshot_roundtrip_preserves_programmed_regs_and_allows_dma_after_restore() {
        let disk = Arc::new(Mutex::new(MemDisk::new(16)));
        // Populate the disk with deterministic content.
        {
            let mut d = disk.lock().unwrap();
            for (i, b) in d.data_mut().iter_mut().enumerate() {
                *b = (i & 0xff) as u8;
            }
        }
        let shared_disk = SharedDisk(disk.clone());

        let mut mem = VecMemory::new(0x20_000);
        let mut controller = AhciController::new(Box::new(shared_disk.clone()));

        let clb = 0x1000u64;
        let fb = 0x2000u64;
        let ctba = 0x3000u64;
        let dst = 0x4000u64;

        // Program the controller but do not issue any command yet.
        controller.mmio_write_u32(&mut mem, HBA_GHC, GHC_AE | GHC_IE);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CLB, clb as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CLBU, (clb >> 32) as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_FB, fb as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_FBU, (fb >> 32) as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_IE, PXIE_DHRE);
        controller.mmio_write_u32(
            &mut mem,
            HBA_PORTS_BASE + PX_CMD,
            PXCMD_FRE | PXCMD_ST | PXCMD_SUD,
        );

        assert!(!controller.irq_level(), "no pending interrupt before snapshot");

        let snap = controller.save_state();

        let mut restored = AhciController::new(Box::new(shared_disk));
        restored.load_state(&snap).unwrap();

        // BAR-relevant state should roundtrip; the restored controller should not
        // spuriously assert the IRQ line.
        assert_eq!(
            restored.mmio_read_u32(&mut mem, HBA_PORTS_BASE + PX_CLB) as u64
                | ((restored.mmio_read_u32(&mut mem, HBA_PORTS_BASE + PX_CLBU) as u64) << 32),
            clb
        );
        assert_eq!(
            restored.mmio_read_u32(&mut mem, HBA_PORTS_BASE + PX_FB) as u64
                | ((restored.mmio_read_u32(&mut mem, HBA_PORTS_BASE + PX_FBU) as u64) << 32),
            fb
        );
        assert!(!restored.irq_level(), "no pending interrupt after restore");

        // Issue a DMA command after restore and ensure it still completes.
        let header = build_cmd_header(5, false, 1, ctba);
        mem.write_physical(clb, &header);
        write_reg_h2d_fis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 2, 1);
        write_prd(&mut mem, ctba + 0x80, dst, 512);

        restored.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CI, 1);
        restored.poll(&mut mem);

        let mut got = [0u8; 512];
        mem.read_physical(dst, &mut got);
        let disk_guard = disk.lock().unwrap();
        let expected = &disk_guard.data()[2 * 512..3 * 512];
        assert_eq!(&got[..], expected);
        assert!(restored.irq_level());

        restored.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_IS, PXIS_DHRS);
        assert!(!restored.irq_level());
    }

    #[test]
    fn read_dma_ext_out_of_bounds_sets_tfes_and_error_status() {
        let disk = Arc::new(Mutex::new(MemDisk::new(4)));
        let shared_disk = SharedDisk(disk);

        let mut mem = VecMemory::new(0x10_000);
        let mut controller = AhciController::new(Box::new(shared_disk));

        let clb = 0x1000u64;
        let fb = 0x2000u64;
        let ctba = 0x3000u64;
        let dst = 0x4000u64;

        controller.mmio_write_u32(&mut mem, HBA_GHC, GHC_AE | GHC_IE);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CLB, clb as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_FB, fb as u32);
        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_IE, PXIE_TFEE);
        controller.mmio_write_u32(
            &mut mem,
            HBA_PORTS_BASE + PX_CMD,
            PXCMD_FRE | PXCMD_ST | PXCMD_SUD,
        );

        let header = build_cmd_header(5, false, 1, ctba);
        mem.write_physical(clb, &header);

        write_reg_h2d_fis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 10, 1);
        write_prd(&mut mem, ctba + 0x80, dst, 512);

        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_CI, 1);

        // Command should complete with error.
        assert_eq!(controller.port0.ci, 0);
        assert_ne!(controller.port0.is & PXIS_TFES, 0);
        assert_ne!(controller.port0.tfd & (ATA_SR_ERR as u32), 0);
        assert!(controller.irq_level());

        controller.mmio_write_u32(&mut mem, HBA_PORTS_BASE + PX_IS, PXIS_TFES | PXIS_DHRS);
        assert_eq!(controller.port0.is, 0);
        assert!(!controller.irq_level());
    }
}
