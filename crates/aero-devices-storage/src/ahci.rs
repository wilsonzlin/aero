//! AHCI (Advanced Host Controller Interface) controller emulation.
//!
//! Windows 7's in-box SATA driver (`storahci.sys`) expects an AHCI HBA with a working
//! command list engine, PRDT-based DMA into guest memory, and interrupts.
//!
//! This module implements enough of the AHCI 1.x programming model for early boot:
//! - HBA memory registers (CAP/GHC/IS/PI/VS)
//! - Per-port registers (CLB/FB/IS/IE/CMD/TFD/SIG/SSTS/CI)
//! - Command list parsing (command header + command table + PRDT)
//! - ATA commands: IDENTIFY, READ DMA EXT, WRITE DMA EXT, FLUSH CACHE(_EXT), SET FEATURES

use std::io;
use std::fmt;

use aero_storage::SECTOR_SIZE;

use crate::ata::{
    AtaDrive, ATA_CMD_FLUSH_CACHE, ATA_CMD_FLUSH_CACHE_EXT, ATA_CMD_IDENTIFY, ATA_CMD_READ_DMA_EXT,
    ATA_CMD_SET_FEATURES, ATA_CMD_WRITE_DMA_EXT, ATA_ERROR_ABRT, ATA_STATUS_DRDY, ATA_STATUS_DSC,
    ATA_STATUS_ERR,
};
use crate::bus::{GuestMemory, GuestMemoryExt};
use crate::IrqLine;

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

const PORT_CMD_ST: u32 = 1 << 0;
const PORT_CMD_FRE: u32 = 1 << 4;
const PORT_CMD_FR: u32 = 1 << 14;
const PORT_CMD_CR: u32 = 1 << 15;

const PORT_IS_DHRS: u32 = 1 << 0;

/// SATA drive signature (PxSIG) for an ATA device.
const SATA_SIG_ATA: u32 = 0x0000_0101;

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
            ghc: 0,
            cap2: 0,
            bohc: 0,
            vs: 0x0001_0300, // AHCI 1.3
        }
    }

    fn reset(&mut self) {
        self.ghc = 0;
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
            let ssts = (1 << 8) | (1 << 4) | 3;
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
}

#[derive(Debug)]
struct AhciPort {
    regs: PortRegs,
    drive: Option<AtaDrive>,
}

impl AhciPort {
    fn new() -> Self {
        Self {
            regs: PortRegs::new(false),
            drive: None,
        }
    }

    fn attach_drive(&mut self, drive: AtaDrive) {
        self.drive = Some(drive);
        self.regs = PortRegs::new(true);
        self.regs.update_running_bits();
    }

    fn clear_drive(&mut self) {
        self.drive = None;
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
        let mut pi = 0u32;
        for (idx, port) in self.ports.iter().enumerate() {
            if port.drive.is_some() {
                pi |= 1 << idx;
            }
        }
        pi
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
        let any_enabled_pending = self.hba.ghc & GHC_IE != 0
            && self
                .ports
                .iter()
                .any(|p| p.regs.is & p.regs.ie != 0);
        self.irq.set_level(any_enabled_pending);
    }

    fn reset(&mut self) {
        self.hba.reset();
        for port in &mut self.ports {
            let present = port.drive.is_some();
            port.regs = PortRegs::new(present);
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
            HBA_REG_BOHC => self.hba.bohc = val,
            _ if offset >= PORT_BASE => self.write_port_u32(offset, val),
            _ => {}
        }
        self.update_irq();
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
                // Only allow guest to set ST/FRE and a few common bits; preserve read-only bits.
                let preserved = port.regs.cmd & (PORT_CMD_FR | PORT_CMD_CR);
                port.regs.cmd = preserved | (val & (PORT_CMD_ST | PORT_CMD_FRE));
                port.regs.update_running_bits();
            }
            PORT_REG_SCTL => port.regs.sctl = val,
            PORT_REG_SERR => {
                // Write 1 to clear.
                port.regs.serr &= !val;
            }
            PORT_REG_SACT => port.regs.sact = val,
            PORT_REG_CI => {
                // Writing to CI sets command issue bits.
                port.regs.ci |= val;
            }
            _ => {}
        }
    }

    /// Process any pending command list entries.
    ///
    /// A full emulator should call this when the guest writes to PxCI, or on a periodic tick.
    pub fn process(&mut self, mem: &mut dyn GuestMemory) {
        for port_idx in 0..self.ports.len() {
            self.process_port(port_idx, mem);
        }
        self.update_irq();
    }

    fn process_port(&mut self, port_idx: usize, mem: &mut dyn GuestMemory) {
        let Some(port) = self.ports.get_mut(port_idx) else {
            return;
        };
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

            match process_command_slot(drive, &mut port.regs, slot, mem) {
                Ok(()) => {}
                Err(_) => {
                    // Report an aborted command via task file status/error.
                    let status = ATA_STATUS_DRDY | ATA_STATUS_ERR;
                    port.regs.tfd = (status as u32) | ((ATA_ERROR_ABRT as u32) << 8);
                    write_d2h_fis(mem, port.regs.fb, status, ATA_ERROR_ABRT);
                    port.regs.is |= PORT_IS_DHRS;
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

fn decode_port_offset(offset: u64) -> (usize, u64) {
    let port_idx = ((offset - PORT_BASE) / PORT_STRIDE) as usize;
    let reg_off = (offset - PORT_BASE) % PORT_STRIDE;
    (port_idx, reg_off)
}

fn process_command_slot(
    drive: &mut AtaDrive,
    port_regs: &mut PortRegs,
    slot: usize,
    mem: &mut dyn GuestMemory,
) -> io::Result<()> {
    let header_addr = port_regs.clb + (slot as u64) * 32;
    let header = CommandHeader::read(mem, header_addr);

    // Command table always contains the command FIS at offset 0.
    let mut cfis = [0u8; 64];
    mem.read(header.ctba, &mut cfis);

    // Register Host to Device FIS
    if cfis[0] != 0x27 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported FIS type"));
    }

    let command = cfis[2];
    match command {
        ATA_CMD_IDENTIFY => {
            let identify = drive.identify_sector();
            dma_write_from_host_buffer(mem, &header, identify)?;
            complete_command(mem, port_regs, slot, (identify.len()) as u32);
        }
        ATA_CMD_READ_DMA_EXT => {
            let lba = extract_lba48(&cfis);
            let sector_count = extract_sector_count(&cfis);
            let byte_len = sector_count as usize * SECTOR_SIZE;
            let mut data = vec![0u8; byte_len];
            drive.read_sectors(lba, &mut data)?;
            dma_write_from_host_buffer(mem, &header, &data)?;
            complete_command(mem, port_regs, slot, byte_len as u32);
        }
        ATA_CMD_WRITE_DMA_EXT => {
            let lba = extract_lba48(&cfis);
            let sector_count = extract_sector_count(&cfis);
            let byte_len = sector_count as usize * SECTOR_SIZE;
            let data = dma_read_into_host_buffer(mem, &header, byte_len)?;
            drive.write_sectors(lba, &data)?;
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
    fn read(mem: &dyn GuestMemory, addr: u64) -> Self {
        let flags = mem.read_u32(addr);
        let ctba_lo = mem.read_u32(addr + 8) as u64;
        let ctba_hi = mem.read_u32(addr + 12) as u64;
        let ctba = ctba_lo | (ctba_hi << 32);
        let prdtl = ((flags >> 20) & 0x0FFF) as u16;

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
    fn read(mem: &dyn GuestMemory, addr: u64) -> Self {
        let dba_lo = mem.read_u32(addr) as u64;
        let dba_hi = mem.read_u32(addr + 4) as u64;
        let dba = dba_lo | (dba_hi << 32);
        let dbc_ioc = mem.read_u32(addr + 12);
        let dbc = (dbc_ioc & 0x003F_FFFF) + 1;
        Self { dba, dbc }
    }
}

fn dma_write_from_host_buffer(
    mem: &mut dyn GuestMemory,
    header: &CommandHeader,
    src: &[u8],
) -> io::Result<()> {
    let mut remaining = src;

    for i in 0..header.prdt_entries() as u64 {
        if remaining.is_empty() {
            break;
        }
        let prd_addr = header.ctba + 0x80 + i * 16;
        let prd = PrdtEntry::read(mem, prd_addr);
        let chunk_len = prd.dbc.min(remaining.len() as u32) as usize;

        mem.write(prd.dba, &remaining[..chunk_len]);
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

fn dma_read_into_host_buffer(
    mem: &dyn GuestMemory,
    header: &CommandHeader,
    byte_len: usize,
) -> io::Result<Vec<u8>> {
    let mut out = vec![0u8; byte_len];
    let mut written = 0usize;

    for i in 0..header.prdt_entries() as u64 {
        if written >= out.len() {
            break;
        }
        let prd_addr = header.ctba + 0x80 + i * 16;
        let prd = PrdtEntry::read(mem, prd_addr);
        let chunk_len = prd.dbc.min((out.len() - written) as u32) as usize;

        mem.read(prd.dba, &mut out[written..written + chunk_len]);
        written += chunk_len;
    }

    if written != out.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "PRDT too small for DMA read",
        ));
    }

    Ok(out)
}

fn complete_command(
    mem: &mut dyn GuestMemory,
    port_regs: &mut PortRegs,
    slot: usize,
    bytes_transferred: u32,
) {
    // Update PRDBC (DW1).
    let header_addr = port_regs.clb + (slot as u64) * 32;
    mem.write_u32(header_addr + 4, bytes_transferred);

    let status = ATA_STATUS_DRDY | ATA_STATUS_DSC;
    port_regs.tfd = (status as u32) | (0u32 << 8);

    write_d2h_fis(mem, port_regs.fb, status, 0);

    // Signal completion.
    port_regs.is |= PORT_IS_DHRS;
}

fn write_d2h_fis(mem: &mut dyn GuestMemory, fb: u64, status: u8, error: u8) {
    // Received FIS layout places the D2H Register FIS at offset 0x40.
    let mut fis = [0u8; 20];
    fis[0] = 0x34; // FIS_TYPE_REG_D2H
    fis[1] = 1 << 6; // I bit (interrupt)
    fis[2] = status;
    fis[3] = error;
    mem.write(fb + 0x40, &fis);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{TestIrqLine, TestMemory};
    use aero_storage::{MemBackend, RawDisk, VirtualDisk};

    fn setup_controller() -> (AhciController, TestIrqLine, TestMemory, AtaDrive) {
        let irq = TestIrqLine::default();
        let ctl = AhciController::new(Box::new(irq.clone()), 1);
        let mem = TestMemory::new(0x20_000);
        let capacity = 32 * SECTOR_SIZE as u64;
        let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let drive = AtaDrive::new(Box::new(disk)).unwrap();
        (ctl, irq, mem, drive)
    }

    fn write_cmd_header(mem: &mut TestMemory, clb: u64, slot: usize, ctba: u64, prdtl: u16, write: bool) {
        let cfl = 5u32;
        let w = if write { 1u32 << 6 } else { 0 };
        let flags = cfl | w | ((prdtl as u32) << 20);
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

    fn write_cfis(mem: &mut TestMemory, ctba: u64, command: u8, lba: u64, count: u16) {
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

        mem.write(ctba, &cfis);
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
        write_cfis(&mut mem, ctba, ATA_CMD_IDENTIFY, 0, 0);
        write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);

        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        assert_eq!(irq.level(), true);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_CI), 0);
        assert_eq!(ctl.read_u32(PORT_BASE + PORT_REG_IS) & PORT_IS_DHRS, PORT_IS_DHRS);

        let mut out = [0u8; SECTOR_SIZE];
        mem.read(data_buf, &mut out);
        assert_eq!(out[0], 0x40);

        // Clear the interrupt.
        ctl.write_u32(PORT_BASE + PORT_REG_IS, PORT_IS_DHRS);
        assert_eq!(irq.level(), false);
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
        write_cfis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 4, 1);
        write_prdt(&mut mem, ctba, 0, data_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut out = [0u8; 4];
        mem.read(data_buf, &mut out);
        assert_eq!(out, [9, 8, 7, 6]);

        // WRITE DMA EXT (LBA=5, 1 sector).
        let write_buf = 0x6000;
        mem.write(write_buf, &[1, 2, 3, 4]);
        // Pad remaining bytes so the disk write path can read a full sector.
        mem.write(write_buf + 4, &vec![0u8; SECTOR_SIZE - 4]);

        write_cmd_header(&mut mem, clb, 0, ctba, 1, true);
        write_cfis(&mut mem, ctba, ATA_CMD_WRITE_DMA_EXT, 5, 1);
        write_prdt(&mut mem, ctba, 0, write_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        // Verify by reading back via READ DMA EXT into a new buffer.
        let verify_buf = 0x7000;
        write_cmd_header(&mut mem, clb, 0, ctba, 1, false);
        write_cfis(&mut mem, ctba, ATA_CMD_READ_DMA_EXT, 5, 1);
        write_prdt(&mut mem, ctba, 0, verify_buf, SECTOR_SIZE as u32);
        ctl.write_u32(PORT_BASE + PORT_REG_CI, 1);
        ctl.process(&mut mem);

        let mut verify = [0u8; 4];
        mem.read(verify_buf, &mut verify);
        assert_eq!(verify, [1, 2, 3, 4]);
        assert_eq!(irq.level(), true);
    }
}
