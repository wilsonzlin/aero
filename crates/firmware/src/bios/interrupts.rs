use aero_cpu_core::state::{
    gpr, mask_bits, CpuState, FLAG_CF, FLAG_DF, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF,
};

use super::{
    disk_err_to_int13_status, set_real_mode_seg, Bios, BiosBus, BiosMemoryBus, BlockDevice,
    CdromDevice, DiskError, ElToritoBootMediaType, BDA_BASE, BDA_KEYBOARD_BUF_HEAD_OFFSET,
    BDA_KEYBOARD_BUF_START, BDA_KEYBOARD_BUF_TAIL_OFFSET, BIOS_SEGMENT, CDROM_SECTOR_SIZE,
    DISKETTE_PARAM_TABLE_OFFSET, EBDA_BASE, EBDA_SIZE, FIXED_DISK_PARAM_TABLE_OFFSET,
    KEYBOARD_QUEUE_CAPACITY, BIOS_SECTOR_SIZE,
};
use crate::cpu::CpuState as FirmwareCpuState;

pub const E820_RAM: u32 = 1;
pub const E820_RESERVED: u32 = 2;
pub const E820_ACPI: u32 = 3;
pub const E820_NVS: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct E820Entry {
    pub base: u64,
    pub length: u64,
    pub region_type: u32,
    pub extended_attributes: u32,
}

/// Adapter that presents a 2048-byte-sector [`CdromDevice`] as a 512-byte-sector [`BlockDevice`]
/// by splitting each ISO block into 4 BIOS sectors.
///
/// This mirrors the El Torito + INT 13h CD logic, which operates on 512-byte sectors internally
/// while exposing 2048-byte-sector LBAs to the guest for CD drives.
struct CdromAsBlockDevice<'a> {
    cdrom: &'a mut dyn CdromDevice,
    cached_lba: Option<u64>,
    cached: [u8; CDROM_SECTOR_SIZE],
}

impl<'a> CdromAsBlockDevice<'a> {
    fn new(cdrom: &'a mut dyn CdromDevice) -> Self {
        Self {
            cdrom,
            cached_lba: None,
            cached: [0u8; CDROM_SECTOR_SIZE],
        }
    }
}

impl BlockDevice for CdromAsBlockDevice<'_> {
    fn read_sector(
        &mut self,
        lba: u64,
        buf: &mut [u8; BIOS_SECTOR_SIZE],
    ) -> Result<(), DiskError> {
        let iso_lba = lba / 4;
        let sub = (lba % 4) as usize;
        if iso_lba >= self.cdrom.size_in_sectors() {
            return Err(DiskError::OutOfRange);
        }
        if self.cached_lba != Some(iso_lba) {
            self.cdrom.read_sector(iso_lba, &mut self.cached)?;
            self.cached_lba = Some(iso_lba);
        }
        let start = sub * BIOS_SECTOR_SIZE;
        buf.copy_from_slice(&self.cached[start..start + BIOS_SECTOR_SIZE]);
        Ok(())
    }

    fn size_in_sectors(&self) -> u64 {
        self.cdrom.size_in_sectors().saturating_mul(4)
    }
}

pub fn dispatch_interrupt(
    bios: &mut Bios,
    vector: u8,
    cpu: &mut CpuState,
    bus: &mut dyn BiosBus,
    disk: &mut dyn BlockDevice,
    cdrom: Option<&mut dyn CdromDevice>,
) {
    dispatch_interrupt_with_cdrom(bios, vector, cpu, bus, disk, cdrom);
}

pub fn dispatch_interrupt_with_cdrom(
    bios: &mut Bios,
    vector: u8,
    cpu: &mut CpuState,
    bus: &mut dyn BiosBus,
    disk: &mut dyn BlockDevice,
    cdrom: Option<&mut dyn CdromDevice>,
) {
    sync_keyboard_bda(bios, bus);

    // The CPU has executed `INT` already, and we're now running in a tiny ROM
    // stub that begins with `HLT`. The stack layout is:
    //   [SS:SP+0]  return IP
    //   [SS:SP+2]  return CS
    //   [SS:SP+4]  return FLAGS
    let sp_bits = cpu.stack_ptr_bits();
    let sp = cpu.stack_ptr();
    let flags_sp = sp.wrapping_add(4) & mask_bits(sp_bits);
    let flags_addr = cpu.apply_a20(cpu.segments.ss.base.wrapping_add(flags_sp));
    let saved_flags = bus.read_u16(flags_addr);

    match vector {
        0x10 => handle_int10(bios, cpu, bus),
        0x11 => handle_int11(cpu, bus),
        0x12 => handle_int12(cpu, bus),
        0x13 => handle_int13(bios, cpu, bus, disk, cdrom),
        0x14 => handle_int14(cpu, bus),
        0x15 => handle_int15(bios, cpu, bus),
        0x16 => handle_int16(bios, cpu),
        0x17 => handle_int17(cpu, bus),
        0x18 => handle_int18(bios, cpu, bus, disk, cdrom),
        0x19 => handle_int19(bios, cpu, bus, disk, cdrom),
        0x1A => handle_int1a(bios, cpu, bus),
        _ => {
            // Safe default: do nothing and return.
            // Emit a BIOS-visible diagnostic, but rate-limit it so we don't spam the TTY buffer for
            // guests that probe many vectors.
            const LOG_LIMIT: u32 = 16;
            let count = bios.unhandled_interrupt_log_count;
            bios.unhandled_interrupt_log_count = bios.unhandled_interrupt_log_count.wrapping_add(1);
            if count < LOG_LIMIT {
                let msg = format!("BIOS: unhandled interrupt {vector:02x}\n");
                bios.push_tty_bytes(msg.as_bytes());
            } else if count == LOG_LIMIT {
                bios.push_tty_bytes(b"BIOS: further unhandled interrupts suppressed\n");
            }
        }
    }

    // INT 16h (and BIOS extensions) may have mutated `keyboard_queue`; keep the BDA mirror in sync
    // for callers that probe it directly.
    sync_keyboard_bda(bios, bus);

    // Merge the flags the handler set into the saved FLAGS image so the stub's IRET
    // returns them to the caller, while preserving IF from the original interrupt frame.
    const RETURN_MASK: u16 = (FLAG_CF | FLAG_PF | FLAG_ZF | FLAG_SF | FLAG_DF | FLAG_OF) as u16;
    let new_flags = (saved_flags & !RETURN_MASK) | ((cpu.rflags() as u16) & RETURN_MASK) | 0x0002;
    bus.write_u16(flags_addr, new_flags);
}

pub(super) fn sync_keyboard_bda(bios: &Bios, bus: &mut dyn BiosBus) {
    // Mirror the BIOS keyboard queue into the conventional BIOS Data Area ring buffer so software
    // that probes 0x40:0x1A/0x1C (head/tail) can observe pending keys without using INT 16h.
    //
    // The canonical source of truth remains `bios.keyboard_queue`; we treat the BDA as a
    // best-effort compatibility mirror.
    // The classic ring buffer uses head==tail to indicate empty, so we leave one entry unused to
    // avoid ambiguity.
    let max_words = KEYBOARD_QUEUE_CAPACITY;
    let used = bios.keyboard_queue.len().min(max_words);

    for (i, key) in bios.keyboard_queue.iter().take(used).enumerate() {
        let addr = BDA_BASE + u64::from(BDA_KEYBOARD_BUF_START) + (i as u64) * 2;
        bus.write_u16(addr, *key);
    }
    // Clear unused slots so stale data is less likely to confuse direct BDA readers.
    for i in used..max_words {
        let addr = BDA_BASE + u64::from(BDA_KEYBOARD_BUF_START) + (i as u64) * 2;
        bus.write_u16(addr, 0);
    }

    bus.write_u16(
        BDA_BASE + BDA_KEYBOARD_BUF_HEAD_OFFSET,
        BDA_KEYBOARD_BUF_START,
    );
    let tail = BDA_KEYBOARD_BUF_START.wrapping_add((used as u16) * 2);
    bus.write_u16(BDA_BASE + BDA_KEYBOARD_BUF_TAIL_OFFSET, tail);
}

#[cfg(test)]
fn keyboard_bda_head(bus: &mut dyn BiosBus) -> u16 {
    bus.read_u16(BDA_BASE + BDA_KEYBOARD_BUF_HEAD_OFFSET)
}

#[cfg(test)]
fn keyboard_bda_tail(bus: &mut dyn BiosBus) -> u16 {
    bus.read_u16(BDA_BASE + BDA_KEYBOARD_BUF_TAIL_OFFSET)
}

fn handle_int11(cpu: &mut CpuState, bus: &mut dyn BiosBus) {
    // Get equipment list.
    //
    // Return the BIOS Data Area equipment flags word. This is a common legacy probing interface
    // used by DOS-era software.
    let equip_flags = bus.read_u16(BDA_BASE + 0x10);
    cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (equip_flags as u64);
    cpu.rflags &= !FLAG_CF;
}

fn handle_int14(cpu: &mut CpuState, bus: &mut dyn BiosBus) {
    // Serial port services.
    //
    // This BIOS does not currently emulate UART registers, but exposing the INT 14h surface helps
    // DOS-era software probe for COM ports.
    //
    // We derive port presence from the BDA COM port base address table (0x40:0x00).
    //
    // Return convention: AH=line status, AL=modem status (BIOS-compatible).
    let ah = ((cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8;
    let port = (cpu.gpr[gpr::RDX] & 0xFFFF) as u16;
    let com_base = if port < 4 {
        bus.read_u16(BDA_BASE + (port as u64) * 2)
    } else {
        0
    };

    let present = com_base != 0;
    let mut line_status: u8 = if present { 0x60 } else { 0x80 }; // THR empty + TSR empty, or timeout
    let mut al_out: u8 = 0;

    match ah {
        0x00 => {
            // Initialize port.
            // We ignore the line control bits in AL; just report status.
        }
        0x01 => {
            // Transmit character (blocking on real hardware).
            // We ignore the character and report success if the port exists.
        }
        0x02 => {
            // Receive character (blocking on real hardware).
            //
            // We don't model RX input; report timeout if the port exists but no data is available.
            if present {
                line_status = 0x80;
            }
            al_out = 0;
        }
        0x03 => {
            // Get port status.
        }
        _ => {
            // Unknown function: timeout/error.
            line_status = 0x80;
        }
    }

    let ax = (u16::from(line_status) << 8) | u16::from(al_out);
    cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (ax as u64);
    // INT 14h does not define flag outputs; keep CF clear so callers treat it as implemented.
    cpu.rflags &= !FLAG_CF;
}

fn handle_int12(cpu: &mut CpuState, bus: &mut dyn BiosBus) {
    // Get conventional memory size (KiB).
    //
    // Return the BIOS Data Area base memory size word (at 0x413). POST initializes this to the
    // amount of memory below the EBDA.
    let base_mem_kb = bus.read_u16(BDA_BASE + 0x13);
    cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (base_mem_kb as u64);
    cpu.rflags &= !FLAG_CF;
}

fn handle_int17(cpu: &mut CpuState, bus: &mut dyn BiosBus) {
    // Printer services.
    //
    // Like INT 14h, we don't currently emulate the device registers. We treat the BDA LPT base
    // address table (0x40:0x08) as the source of truth for port presence.
    //
    // Return convention: AH=status.
    let port = (cpu.gpr[gpr::RDX] & 0xFFFF) as u16;
    let lpt_base = if port < 3 {
        bus.read_u16(BDA_BASE + 0x08 + (port as u64) * 2)
    } else {
        0
    };

    // Status bits (subset; IBM PC/AT convention):
    // - bit 0: timeout
    // - bit 4: selected
    // - bit 7: not busy
    let status: u8 = if lpt_base != 0 { 0x90 } else { 0x01 };

    let ah = ((cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8;
    match ah {
        0x00 => {
            // Print character.
        }
        0x01 => {
            // Initialize printer.
        }
        0x02 => {
            // Get printer status.
        }
        _ => {
            // Unknown function: report timeout.
        }
    }

    cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFF00) | ((status as u64) << 8);
    cpu.rflags &= !FLAG_CF;
}

fn handle_int18(
    bios: &mut Bios,
    cpu: &mut CpuState,
    bus: &mut dyn BiosBus,
    disk: &mut dyn BlockDevice,
    cdrom: Option<&mut dyn CdromDevice>,
) {
    // ROM BASIC / boot failure fallback.
    //
    // When no ROM BASIC is present, many BIOSes chain INT 18h to INT 19h to retry boot.
    handle_int19(bios, cpu, bus, disk, cdrom);
}

fn handle_int19(
    bios: &mut Bios,
    cpu: &mut CpuState,
    bus: &mut dyn BiosBus,
    disk: &mut dyn BlockDevice,
    cdrom: Option<&mut dyn CdromDevice>,
) {
    // Bootstrap loader.
    //
    // INT 19h is traditionally used to re-run the boot sequence without a full POST. The real BIOS
    // typically does not return to the caller. Our BIOS interrupt dispatch mechanism always
    // resumes execution at a ROM-stub `IRET`, so we emulate the non-returning control transfer by
    // installing a new IRET frame on a fresh real-mode stack.
    //
    // The stub IRET will:
    // - pop IP, CS, FLAGS from SS:SP
    // - clear the BIOS hypercall marker (`pending_bios_int_valid`)
    //
    const STACK_AFTER_IRET: u16 = 0x7C00;
    const STACK_BEFORE_IRET: u16 = STACK_AFTER_IRET.wrapping_sub(6);

    // Load the configured boot device (MBR or El Torito CD) into RAM and initialize registers,
    // matching POST's boot conventions.
    let boot_drive = bios.config.boot_drive;
    let boot_result = if (0xE0..=0xEF).contains(&boot_drive) {
        if let Some(cdrom) = cdrom {
            let mut cd_disk = CdromAsBlockDevice::new(cdrom);
            bios.boot_from_configured_device(cpu, bus, &mut cd_disk)
        } else {
            bios.boot_from_configured_device(cpu, bus, disk)
        }
    } else {
        bios.boot_from_configured_device(cpu, bus, disk)
    };

    let (entry_cs, entry_ip) = match boot_result {
        Ok(v) => v,
        Err(msg) => {
            bios.bios_panic(cpu, bus, msg);
            return;
        }
    };

    // Use a clean 0000:7C00 stack. We must set SP to 7BFA so the following IRET lands with
    // SP=7C00.
    set_real_mode_seg(&mut cpu.segments.ss, 0x0000);
    cpu.gpr[gpr::RSP] = STACK_BEFORE_IRET as u64;

    // Build the synthetic IRET frame: IP, CS, FLAGS (word each).
    let frame_base = cpu.apply_a20(cpu.segments.ss.base.wrapping_add(STACK_BEFORE_IRET as u64));
    bus.write_u16(frame_base, entry_ip); // IP
    bus.write_u16(frame_base + 2, entry_cs); // CS
    bus.write_u16(frame_base + 4, 0x0202); // IF=1 + reserved bit 1

    cpu.rflags &= !FLAG_CF;
}

fn handle_int10(bios: &mut Bios, cpu: &mut CpuState, bus: &mut dyn BiosBus) {
    // Keep the historical "TTY output" buffer for tests/debugging.
    let ah = ((cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8;
    if ah == 0x0E {
        bios.push_tty_byte((cpu.gpr[gpr::RAX] & 0xFF) as u8);
    }

    // Bridge machine CPU state + memory bus to the firmware-side INT 10h implementation.
    let mut fw_cpu = FirmwareCpuState {
        rax: cpu.gpr[gpr::RAX],
        rbx: cpu.gpr[gpr::RBX],
        rcx: cpu.gpr[gpr::RCX],
        rdx: cpu.gpr[gpr::RDX],
        rbp: cpu.gpr[gpr::RBP],
        rsi: cpu.gpr[gpr::RSI],
        rdi: cpu.gpr[gpr::RDI],
        rflags: 0, // INT 10h does not define flag inputs; start with CF clear.
        ds: cpu.segments.ds.selector,
        es: cpu.segments.es.selector,
    };

    bios.handle_int10(&mut fw_cpu, &mut BiosMemoryBus::new(bus));

    cpu.gpr[gpr::RAX] = fw_cpu.rax;
    cpu.gpr[gpr::RBX] = fw_cpu.rbx;
    cpu.gpr[gpr::RCX] = fw_cpu.rcx;
    cpu.gpr[gpr::RDX] = fw_cpu.rdx;
    cpu.gpr[gpr::RBP] = fw_cpu.rbp;
    cpu.gpr[gpr::RSI] = fw_cpu.rsi;
    cpu.gpr[gpr::RDI] = fw_cpu.rdi;
    set_real_mode_seg(&mut cpu.segments.ds, fw_cpu.ds);
    set_real_mode_seg(&mut cpu.segments.es, fw_cpu.es);
    cpu.set_flag(FLAG_CF, fw_cpu.cf());
}

fn handle_int13(
    bios: &mut Bios,
    cpu: &mut CpuState,
    bus: &mut dyn BiosBus,
    disk: &mut dyn BlockDevice,
    mut cdrom: Option<&mut dyn CdromDevice>,
) {
    let ah = ((cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8;
    let drive = (cpu.gpr[gpr::RDX] & 0xFF) as u8;
    let cdrom_present = cdrom.is_some();

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DriveKind {
        Floppy,
        Hdd,
        Cd,
    }

    fn classify_drive(drive: u8) -> Option<DriveKind> {
        match drive {
            0x00..=0x7F => Some(DriveKind::Floppy),
            0x80..=0xDF => Some(DriveKind::Hdd),
            0xE0..=0xEF => Some(DriveKind::Cd),
            _ => None,
        }
    }

    fn set_error(bios: &mut Bios, cpu: &mut CpuState, status: u8) {
        bios.last_int13_status = status;
        cpu.rflags |= FLAG_CF;
        // For most INT 13h errors, BIOSes return AL=0 (no sectors transferred) alongside AH=status.
        cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | ((status as u64) << 8);
    }

    fn floppy_drive_count(bus: &mut dyn BiosBus) -> u8 {
        // Determined by the BDA equipment word (INT 11h).
        let equip = bus.read_u16(BDA_BASE + 0x10);
        if (equip & 1) == 0 {
            return 0;
        }
        let count_minus1 = ((equip >> 6) & 0x3) as u8;
        count_minus1.saturating_add(1)
    }

    fn fixed_drive_count(bus: &mut dyn BiosBus) -> u8 {
        bus.read_u8(BDA_BASE + 0x75)
    }

    fn drive_present(bios: &Bios, bus: &mut dyn BiosBus, drive: u8, cdrom_present: bool) -> bool {
        match classify_drive(drive) {
            Some(DriveKind::Floppy) => drive < floppy_drive_count(bus),
            Some(DriveKind::Hdd) => {
                let idx = drive.wrapping_sub(0x80);
                idx < fixed_drive_count(bus)
            }
            Some(DriveKind::Cd) => cdrom_present || bios.config.boot_drive == drive,
            None => false,
        }
    }

    fn geometry_for_drive(drive: u8, total_sectors: u64) -> (u16, u8, u8) {
        if drive < 0x80 {
            // Floppy disk (heuristic by media size; fallback is a reasonable default).
            match total_sectors {
                2880 => (80, 2, 18), // 1.44 MiB (3.5")
                2400 => (80, 2, 15), // 1.2 MiB (5.25")
                1440 => (80, 2, 9),  // 720 KiB (3.5")
                720 => (40, 2, 9),   // 360 KiB (5.25")
                360 => (40, 1, 9),   // 180 KiB
                320 => (40, 2, 8),   // 160 KiB
                _ => {
                    let heads = 2u8;
                    let spt = 18u8;
                    let denom = u64::from(heads) * u64::from(spt);
                    let cyl = (if denom != 0 { total_sectors / denom } else { 1 }).clamp(1, 1024);
                    (cyl as u16, heads, spt)
                }
            }
        } else {
            // Fixed disk (minimal geometry; matches legacy tests + common boot expectations).
            (1024, 16, 63)
        }
    }

    // El Torito disk emulation services (INT 13h AH=4Bh).
    //
    // This is used by some CD boot images (notably ISOLINUX) to query the boot catalog location
    // and/or terminate BIOS disk emulation.
    if ah == 0x4B {
        let al = (cpu.gpr[gpr::RAX] & 0xFF) as u8;

        if let Some(info) = bios.el_torito_boot_info {
            // The El Torito interface is scoped to the boot drive.
            if drive != info.boot_drive {
                set_error(bios, cpu, 0x01);
                return;
            }

            match al {
                0x00 => {
                    // Terminate disk emulation.
                    //
                    // In "no emulation" mode there's nothing to terminate; report success as a
                    // no-op (common BIOS behaviour).
                    if info.media_type != ElToritoBootMediaType::NoEmulation {
                        set_error(bios, cpu, 0x01);
                        return;
                    }

                    bios.last_int13_status = 0;
                    cpu.rflags &= !FLAG_CF;
                    cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
                }
                0x01 => {
                    // Get disk emulation status.
                    //
                    // ES:DI points to a caller-supplied buffer. The first byte may contain the
                    // buffer size; if it is non-zero, enforce a minimum size.
                    const PACKET_SIZE: u8 = 0x13;
                    let di = cpu.gpr[gpr::RDI] & 0xFFFF;
                    let packet_addr = cpu.apply_a20(cpu.segments.es.base.wrapping_add(di));
                    let caller_len = bus.read_u8(packet_addr);
                    if caller_len != 0 && caller_len < PACKET_SIZE {
                        set_error(bios, cpu, 0x01);
                        return;
                    }

                    bus.write_u8(packet_addr, PACKET_SIZE);
                    bus.write_u8(packet_addr + 1, info.media_type as u8);
                    bus.write_u8(packet_addr + 2, info.boot_drive);
                    bus.write_u8(packet_addr + 3, info.controller_index);
                    // LBA of the boot image (RBA in El Torito terminology).
                    bus.write_u32(packet_addr + 4, info.boot_image_lba.unwrap_or(0));
                    // LBA of the boot catalog.
                    bus.write_u32(packet_addr + 8, info.boot_catalog_lba.unwrap_or(0));
                    bus.write_u16(packet_addr + 12, info.load_segment.unwrap_or(0));
                    bus.write_u16(packet_addr + 14, info.sector_count.unwrap_or(0));
                    // Reserved bytes.
                    bus.write_u8(packet_addr + 16, 0);
                    bus.write_u8(packet_addr + 17, 0);
                    bus.write_u8(packet_addr + 18, 0);

                    bios.last_int13_status = 0;
                    cpu.rflags &= !FLAG_CF;
                    cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
                }
                _ => {
                    set_error(bios, cpu, 0x01);
                }
            }
        } else {
            // Best-effort compatibility: If POST didn't capture El Torito metadata, still report
            // success for callers probing a CD-ROM drive number by returning consistent zeros for
            // unknown fields rather than failing the entire call.
            if classify_drive(drive) != Some(DriveKind::Cd) {
                set_error(bios, cpu, 0x01);
                return;
            }

            match al {
                0x00 => {
                    bios.last_int13_status = 0;
                    cpu.rflags &= !FLAG_CF;
                    cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
                }
                0x01 => {
                    const PACKET_SIZE: u8 = 0x13;
                    let di = cpu.gpr[gpr::RDI] & 0xFFFF;
                    let packet_addr = cpu.apply_a20(cpu.segments.es.base.wrapping_add(di));
                    let caller_len = bus.read_u8(packet_addr);
                    if caller_len != 0 && caller_len < PACKET_SIZE {
                        set_error(bios, cpu, 0x01);
                        return;
                    }

                    bus.write_u8(packet_addr, PACKET_SIZE);
                    bus.write_u8(packet_addr + 1, ElToritoBootMediaType::NoEmulation as u8);
                    bus.write_u8(packet_addr + 2, drive);
                    bus.write_u8(packet_addr + 3, 0); // controller index
                    bus.write_u32(packet_addr + 4, 0); // boot image LBA
                    bus.write_u32(packet_addr + 8, 0); // boot catalog LBA
                    bus.write_u16(packet_addr + 12, 0); // load segment
                    bus.write_u16(packet_addr + 14, 0); // sector count
                                                        // Reserved bytes.
                    bus.write_u8(packet_addr + 16, 0);
                    bus.write_u8(packet_addr + 17, 0);
                    bus.write_u8(packet_addr + 18, 0);

                    bios.last_int13_status = 0;
                    cpu.rflags &= !FLAG_CF;
                    cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
                }
                _ => {
                    set_error(bios, cpu, 0x01);
                }
            }
        }

        return;
    }

    let drive_kind = classify_drive(drive);
    if drive_kind == Some(DriveKind::Cd) {
        // CD-ROM INT 13h semantics
        // -----------------------
        //
        // For `DL` in 0xE0..=0xEF (the conventional BIOS range for El Torito / ATAPI CD-ROM drives),
        // we follow the EDD/El Torito convention used by SeaBIOS:
        //
        // - AH=48h ("EDD get drive parameters") reports **bytes/sector = 2048**.
        // - AH=42h ("EDD extended read") interprets the DAP's `LBA` and `count` in **2048-byte
        //   sectors** and transfers `count * 2048` bytes.
        //
        // This behavior is required by Windows bootloaders like Win7's `etfsboot.com`, which read
        // ISO 9660 logical blocks (2048 bytes) from the CD boot drive via EDD.
        //
        // Backend note: when a [`CdromDevice`] backend is provided, reads operate on 2048-byte
        // sectors directly. If no CD backend is provided, we fall back to treating `disk` as a raw
        // ISO image exposed as 512-byte sectors (4 x 512-byte sectors per CD-ROM logical block).
        match ah {
            0x00 => {
                // Reset disk system.
                if !drive_present(bios, bus, drive, cdrom_present) {
                    set_error(bios, cpu, 0x01);
                    return;
                }
                bios.last_int13_status = 0;
                cpu.rflags &= !FLAG_CF;
                cpu.gpr[gpr::RAX] &= !0xFF00u64;
            }
            0x01 => {
                // Get status of last disk operation.
                if !drive_present(bios, bus, drive, cdrom_present) {
                    set_error(bios, cpu, 0x01);
                    return;
                }
                let status = bios.last_int13_status;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | ((status as u64) << 8);
                if status == 0 {
                    cpu.rflags &= !FLAG_CF;
                } else {
                    cpu.rflags |= FLAG_CF;
                }
            }
            0x15 => {
                // Get disk type.
                if !drive_present(bios, bus, drive, cdrom_present) {
                    set_error(bios, cpu, 0x01);
                    return;
                }
                let total_2048 = cdrom
                    .as_deref()
                    .map(|cdrom| cdrom.size_in_sectors())
                    .unwrap_or_else(|| disk.size_in_sectors() / 4);
                let sectors_u32 = u32::try_from(total_2048).unwrap_or(u32::MAX);
                let cx = (sectors_u32 >> 16) as u16;
                let dx = (sectors_u32 & 0xFFFF) as u16;
                cpu.gpr[gpr::RCX] = (cpu.gpr[gpr::RCX] & !0xFFFF) | (cx as u64);
                cpu.gpr[gpr::RDX] = (cpu.gpr[gpr::RDX] & !0xFFFF) | (dx as u64);
                // Convention: report type 0x03 ("fixed disk") to indicate "present".
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | 0x0300;
                bios.last_int13_status = 0;
                cpu.rflags &= !FLAG_CF;
            }
            0x03 | 0x05 => {
                // CHS write / format track.
                if !drive_present(bios, bus, drive, cdrom_present) {
                    set_error(bios, cpu, 0x01);
                    return;
                }
                bios.last_int13_status = 0x03; // write protected
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x03u64 << 8);
            }
            0x41 => {
                // Extensions check.
                if !drive_present(bios, bus, drive, cdrom_present) {
                    set_error(bios, cpu, 0x01);
                    return;
                }
                if (cpu.gpr[gpr::RBX] & 0xFFFF) == 0x55AA {
                    // EDD extensions installation check.
                    //
                    // Strategy A: We advertise EDD 3.0 (AH=0x30) and implement the 0x42-byte
                    // "Drive Parameter Table" returned by AH=48h when the caller provides a large
                    // enough buffer.
                    //
                    // Some bootloaders/OS probes treat a mismatch between the reported EDD version
                    // (AH=41h) and the returned AH=48h table size as a BIOS bug and will refuse to
                    // use EDD services.
                    cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x30u64 << 8);
                    cpu.gpr[gpr::RBX] = (cpu.gpr[gpr::RBX] & !0xFFFF) | 0xAA55;
                    // Feature bitmap (Phoenix EDD spec):
                    // - bit 0: functions 42h-44h supported (extended read/write/verify)
                    // - bit 1: functions 45h-47h supported (lock/eject/seek)
                    // - bit 2: function 48h supported (drive parameter table)
                    cpu.gpr[gpr::RCX] = (cpu.gpr[gpr::RCX] & !0xFFFF) | 0x0005;
                    bios.last_int13_status = 0;
                    cpu.rflags &= !FLAG_CF;
                } else {
                    set_error(bios, cpu, 0x01);
                }
            }
            0x42 => {
                // Extended read via Disk Address Packet (DAP) at DS:SI.
                if !drive_present(bios, bus, drive, cdrom_present) {
                    set_error(bios, cpu, 0x01);
                    return;
                }

                let si = cpu.gpr[gpr::RSI] & 0xFFFF;
                let dap_addr = cpu.apply_a20(cpu.segments.ds.base.wrapping_add(si));
                let dap_size = bus.read_u8(dap_addr);
                if dap_size != 0x10 && dap_size != 0x18 {
                    set_error(bios, cpu, 0x01);
                    return;
                }

                if bus.read_u8(dap_addr + 1) != 0 {
                    set_error(bios, cpu, 0x01);
                    return;
                }

                let count_2048 = bus.read_u16(dap_addr + 2) as u64;
                if count_2048 == 0 {
                    set_error(bios, cpu, 0x01);
                    return;
                }
                let buf_off = bus.read_u16(dap_addr + 4);
                let buf_seg = bus.read_u16(dap_addr + 6);
                let lba_2048 = bus.read_u64(dap_addr + 8);
                let mut dst = cpu.apply_a20(((buf_seg as u64) << 4).wrapping_add(buf_off as u64));

                if dap_size == 0x18 {
                    // 24-byte DAP includes a 64-bit flat pointer at offset 16.
                    let buf64 = bus.read_u64(dap_addr + 16);
                    if buf64 != 0 {
                        dst = cpu.apply_a20(buf64);
                    }
                }

                if let Some(cdrom) = cdrom.as_deref_mut() {
                    let Some(end_2048) = lba_2048.checked_add(count_2048) else {
                        set_error(bios, cpu, 0x04);
                        return;
                    };
                    if end_2048 > cdrom.size_in_sectors() {
                        set_error(bios, cpu, 0x04);
                        return;
                    }

                    for i in 0..count_2048 {
                        let mut buf = [0u8; 2048];
                        match cdrom.read_sector(lba_2048 + i, &mut buf) {
                            Ok(()) => bus.write_physical(dst + i * 2048, &buf),
                            Err(e) => {
                                let status = disk_err_to_int13_status(e);
                                set_error(bios, cpu, status);
                                return;
                            }
                        }
                    }
                } else {
                    // Compatibility fallback: treat `disk` as an ISO image stored as 512-byte
                    // sectors, and map CD-ROM LBAs to 4x512 sectors.
                    let iso_total_2048 = disk.size_in_sectors() / 4;
                    let Some(end_2048) = lba_2048.checked_add(count_2048) else {
                        set_error(bios, cpu, 0x04);
                        return;
                    };
                    if end_2048 > iso_total_2048 {
                        set_error(bios, cpu, 0x04);
                        return;
                    }

                    let Some(lba_512) = lba_2048.checked_mul(4) else {
                        set_error(bios, cpu, 0x04);
                        return;
                    };
                    let Some(count_512) = count_2048.checked_mul(4) else {
                        set_error(bios, cpu, 0x04);
                        return;
                    };

                    for i in 0..count_512 {
                        let mut buf = [0u8; BIOS_SECTOR_SIZE];
                        match disk.read_sector(lba_512 + i, &mut buf) {
                            Ok(()) => bus.write_physical(dst + i * BIOS_SECTOR_SIZE as u64, &buf),
                            Err(e) => {
                                let status = disk_err_to_int13_status(e);
                                set_error(bios, cpu, status);
                                return;
                            }
                        }
                    }
                }

                bios.last_int13_status = 0;
                cpu.rflags &= !FLAG_CF;
                cpu.gpr[gpr::RAX] &= !0xFF00u64;
            }
            0x43 => {
                // Extended write via Disk Address Packet (EDD): CD media is write-protected.
                if !drive_present(bios, bus, drive, cdrom_present) {
                    set_error(bios, cpu, 0x01);
                    return;
                }

                bios.last_int13_status = 0x03; // write protected
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x03u64 << 8);
            }
            0x44 => {
                // Extended verify via Disk Address Packet (EDD) for CD-ROM media (2048-byte
                // sectors).
                //
                // We treat this as a bounds check: if the requested ISO LBA range is valid,
                // succeed.
                if !drive_present(bios, bus, drive, cdrom_present) {
                    set_error(bios, cpu, 0x01);
                    return;
                }

                let si = cpu.gpr[gpr::RSI] & 0xFFFF;
                let dap_addr = cpu.apply_a20(cpu.segments.ds.base.wrapping_add(si));
                let dap_size = bus.read_u8(dap_addr);
                if dap_size != 0x10 && dap_size != 0x18 {
                    set_error(bios, cpu, 0x01);
                    return;
                }
                if bus.read_u8(dap_addr + 1) != 0 {
                    set_error(bios, cpu, 0x01);
                    return;
                }

                let count_2048 = bus.read_u16(dap_addr + 2) as u64;
                if count_2048 == 0 {
                    set_error(bios, cpu, 0x01);
                    return;
                }
                let lba_2048 = bus.read_u64(dap_addr + 8);

                let total_2048 = cdrom
                    .as_deref()
                    .map(|cdrom| cdrom.size_in_sectors())
                    .unwrap_or_else(|| disk.size_in_sectors() / 4);
                let Some(end_2048) = lba_2048.checked_add(count_2048) else {
                    set_error(bios, cpu, 0x04);
                    return;
                };
                if end_2048 > total_2048 {
                    set_error(bios, cpu, 0x04);
                    return;
                }

                bios.last_int13_status = 0;
                cpu.rflags &= !FLAG_CF;
                cpu.gpr[gpr::RAX] &= !0xFF00u64;
            }
            0x48 => {
                // Extended get drive parameters (EDD) for CD-ROM media (2048-byte sectors).
                if !drive_present(bios, bus, drive, cdrom_present) {
                    set_error(bios, cpu, 0x01);
                    return;
                }
                let total_2048 = cdrom
                    .as_deref()
                    .map(|cdrom| cdrom.size_in_sectors())
                    .unwrap_or_else(|| disk.size_in_sectors() / 4);

                let si = cpu.gpr[gpr::RSI] & 0xFFFF;
                let table_addr = cpu.apply_a20(cpu.segments.ds.base.wrapping_add(si));
                let buf_size = bus.read_u16(table_addr) as usize;
                const EDD_PARAMS_MIN_SIZE: usize = 0x1A;
                const EDD_PARAMS_V2_SIZE: usize = 0x1E;
                const EDD_PARAMS_V3_SIZE: usize = 0x42;
                if buf_size < EDD_PARAMS_MIN_SIZE {
                    set_error(bios, cpu, 0x01);
                    return;
                }

                // Fill the EDD drive parameter table.
                //
                // EDD 3.0 defines a 0x42-byte structure. For compatibility with older callers we
                // also support (and may return) smaller table sizes (down to 0x1A bytes).
                //
                // To avoid claiming a size we didn't fully populate, we only ever return one of
                // the standard EDD structure sizes:
                // - 0x42 (EDD 3.0) when the caller provides >= 0x42 bytes
                // - 0x1E (EDD 2.x) when the caller provides >= 0x1E bytes but < 0x42
                // - 0x1A (EDD 1.1) when the caller provides 0x1A..0x1D bytes
                let write_len = if buf_size >= EDD_PARAMS_V3_SIZE {
                    EDD_PARAMS_V3_SIZE
                } else if buf_size >= EDD_PARAMS_V2_SIZE {
                    EDD_PARAMS_V2_SIZE
                } else {
                    EDD_PARAMS_MIN_SIZE
                };
                bus.write_u16(table_addr, write_len as u16);

                if write_len >= 4 {
                    bus.write_u16(table_addr + 2, 0); // flags
                }
                if write_len >= 8 {
                    bus.write_u32(table_addr + 4, 1024); // cylinders (placeholder)
                }
                if write_len >= 12 {
                    bus.write_u32(table_addr + 8, 16); // heads (placeholder)
                }
                if write_len >= 16 {
                    bus.write_u32(table_addr + 12, 63); // sectors/track (placeholder)
                }
                if write_len >= 24 {
                    bus.write_u64(table_addr + 16, total_2048);
                }
                if write_len >= 26 {
                    bus.write_u16(table_addr + 24, 2048); // bytes/sector
                }
                if write_len >= 30 {
                    bus.write_u32(table_addr + 26, 0); // DPTE pointer (unused)
                }

                if write_len >= EDD_PARAMS_V3_SIZE {
                    // EDD 3.0 drive parameter table extension.
                    //
                    // Offsets and layout follow the Phoenix EDD 3.0 spec, as implemented by common
                    // BIOSes and consumed by OS probes (e.g. Linux's `edd_device_params`).
                    let interface_type: [u8; 8] = *b"ATAPI   ";

                    bus.write_u16(table_addr + 0x1E, 0xBEDD); // key
                    bus.write_u8(table_addr + 0x20, 0x1E); // device path info length
                    bus.write_u8(table_addr + 0x21, 0x00); // reserved
                    bus.write_u16(table_addr + 0x22, 0x0000); // reserved

                    // Host bus type (4 bytes) and interface type (8 bytes) strings.
                    for (i, b) in b"PCI ".iter().copied().enumerate() {
                        bus.write_u8(table_addr + 0x24 + i as u64, b);
                    }
                    for (i, b) in interface_type.iter().copied().enumerate() {
                        bus.write_u8(table_addr + 0x28 + i as u64, b);
                    }

                    // interface_path (8), device_path (8) and reserved (1) are all zero.
                    for off in 0x30u64..0x40 {
                        bus.write_u8(table_addr + off, 0);
                    }
                    bus.write_u8(table_addr + 0x40, 0);

                    // Compute the 8-bit checksum so that the sum of the device path info bytes
                    // (host bus type .. checksum) is 0 modulo 256.
                    let mut sum: u8 = 0;
                    for off in 0x24..0x42 {
                        // Skip checksum byte while summing; it's at the last byte (0x41).
                        if off == 0x41 {
                            continue;
                        }
                        sum = sum.wrapping_add(bus.read_u8(table_addr + off));
                    }
                    let checksum = (0u8).wrapping_sub(sum);
                    bus.write_u8(table_addr + 0x41, checksum);
                }

                bios.last_int13_status = 0;
                cpu.rflags &= !FLAG_CF;
                cpu.gpr[gpr::RAX] &= !0xFF00u64;
            }
            _ => {
                // Legacy CHS functions are not supported for CD-ROM drives; extensions are
                // sufficient for El Torito boot paths.
                if !drive_present(bios, bus, drive, cdrom_present) {
                    set_error(bios, cpu, 0x01);
                    return;
                }
                set_error(bios, cpu, 0x01);
            }
        }
        return;
    }

    match ah {
        0x00 => {
            // Reset disk system.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64;
        }
        0x01 => {
            // Get status of last disk operation.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            let status = bios.last_int13_status;
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | ((status as u64) << 8);
            if status == 0 {
                cpu.rflags &= !FLAG_CF;
            } else {
                cpu.rflags |= FLAG_CF;
            }
        }
        0x02 => {
            // Read sectors (CHS).
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            let mut count = (cpu.gpr[gpr::RAX] & 0xFF) as u16;
            if count == 0 {
                // INT 13h AH=02h uses AL=0 as 256 sectors.
                count = 256;
            }
            let cl = (cpu.gpr[gpr::RCX] & 0xFF) as u8;
            let ch = ((cpu.gpr[gpr::RCX] >> 8) & 0xFF) as u8;
            let dh = ((cpu.gpr[gpr::RDX] >> 8) & 0xFF) as u8;

            let sector = (cl & 0x3F) as u16;
            let cylinder = ((ch as u16) | (((cl as u16) & 0xC0) << 2)) as u32;
            let head = dh as u32;

            let (cylinders, heads, spt) = geometry_for_drive(drive, disk.size_in_sectors());
            let spt = u32::from(spt);
            let heads = u32::from(heads);
            let cylinders = u32::from(cylinders);

            if sector == 0 || sector > spt as u16 || head >= heads || cylinder >= cylinders {
                set_error(bios, cpu, 0x01);
                return;
            }

            let lba = ((cylinder * heads + head) * spt + (sector as u32 - 1)) as u64;
            let bx = cpu.gpr[gpr::RBX] & 0xFFFF;
            let dst = cpu.apply_a20(cpu.segments.es.base.wrapping_add(bx));

            // Many real BIOS implementations use DMA for this path and require the transfer
            // buffer not cross a 64KiB physical boundary.
            let total_bytes = (count as u64) * BIOS_SECTOR_SIZE as u64;
            let Some(end_addr) = dst.checked_add(total_bytes.saturating_sub(1)) else {
                bios.last_int13_status = 0x09;
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x09u64 << 8);
                return;
            };
            if (dst & 0xFFFF_0000) != (end_addr & 0xFFFF_0000) {
                bios.last_int13_status = 0x09; // data boundary error
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x09u64 << 8);
                return;
            }

            for i in 0..count as u64 {
                let mut buf = [0u8; BIOS_SECTOR_SIZE];
                match disk.read_sector(lba + i, &mut buf) {
                    Ok(()) => {
                        bus.write_physical(dst + i * BIOS_SECTOR_SIZE as u64, &buf);
                    }
                    Err(e) => {
                        cpu.rflags |= FLAG_CF;
                        let status = disk_err_to_int13_status(e);
                        bios.last_int13_status = status;
                        // AH=status, AL=sectors transferred.
                        cpu.gpr[gpr::RAX] =
                            (cpu.gpr[gpr::RAX] & !0xFFFF) | (i & 0xFF) | ((status as u64) << 8);
                        return;
                    }
                }
            }

            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            // AH=0 on success, AL = sectors transferred.
            let transferred = if count == 256 { 0u64 } else { count as u64 };
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | transferred;
            let _ = drive;
        }
        0x03 => {
            // Write sectors (CHS).
            //
            // The BIOS disk interface is currently backed by a read-only [`BlockDevice`]. Report
            // write-protect rather than "function unsupported" so DOS-era software can degrade
            // gracefully.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            bios.last_int13_status = 0x03; // write protected
            cpu.rflags |= FLAG_CF;
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x03u64 << 8);
        }
        0x05 => {
            // Format track (CHS).
            //
            // Like other write operations, formatting is not supported with the current read-only
            // [`BlockDevice`] implementation.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }

            bios.last_int13_status = 0x03; // write protected
            cpu.rflags |= FLAG_CF;
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x03u64 << 8);
        }
        0x04 => {
            // Verify sectors (CHS).
            //
            // Verify is like a read without transferring data into memory. We implement it by
            // reading sectors and discarding the contents.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            let mut count = (cpu.gpr[gpr::RAX] & 0xFF) as u16;
            if count == 0 {
                count = 256;
            }
            let cl = (cpu.gpr[gpr::RCX] & 0xFF) as u8;
            let ch = ((cpu.gpr[gpr::RCX] >> 8) & 0xFF) as u8;
            let dh = ((cpu.gpr[gpr::RDX] >> 8) & 0xFF) as u8;

            let sector = (cl & 0x3F) as u16;
            let cylinder = ((ch as u16) | (((cl as u16) & 0xC0) << 2)) as u32;
            let head = dh as u32;

            let (cylinders, heads, spt) = geometry_for_drive(drive, disk.size_in_sectors());
            let spt = u32::from(spt);
            let heads = u32::from(heads);
            let cylinders = u32::from(cylinders);

            if sector == 0 || sector > spt as u16 || head >= heads || cylinder >= cylinders {
                set_error(bios, cpu, 0x01);
                return;
            }

            let lba = ((cylinder * heads + head) * spt + (sector as u32 - 1)) as u64;
            for i in 0..count as u64 {
                let mut buf = [0u8; BIOS_SECTOR_SIZE];
                if let Err(e) = disk.read_sector(lba + i, &mut buf) {
                    cpu.rflags |= FLAG_CF;
                    let status = disk_err_to_int13_status(e);
                    bios.last_int13_status = status;
                    cpu.gpr[gpr::RAX] =
                        (cpu.gpr[gpr::RAX] & !0xFFFF) | (i & 0xFF) | ((status as u64) << 8);
                    return;
                }
            }

            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            let verified = if count == 256 { 0u64 } else { count as u64 };
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | verified;
        }
        0x08 => {
            // Get drive parameters (very small subset).
            // Return: CF clear, AH=0, CH/CL/DH describe geometry.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            let (cylinders, heads, spt) = geometry_for_drive(drive, disk.size_in_sectors());
            let drive_type = if drive < 0x80 {
                match disk.size_in_sectors() {
                    5760 => 0x05, // 2.88 MiB
                    2880 => 0x04, // 1.44 MiB
                    2400 => 0x02, // 1.2 MiB
                    1440 => 0x03, // 720 KiB
                    720 => 0x01,  // 360 KiB
                    _ => 0x00,
                }
            } else {
                0x00
            };

            let cyl_minus1 = cylinders - 1;
            let ch = (cyl_minus1 & 0xFF) as u8;
            let cl = (spt & 0x3F) | (((cyl_minus1 >> 2) as u8) & 0xC0);
            let dh = heads - 1;
            let dl = if drive < 0x80 {
                floppy_drive_count(bus)
            } else {
                fixed_drive_count(bus)
            };
            let table_off = if drive < 0x80 {
                DISKETTE_PARAM_TABLE_OFFSET
            } else {
                FIXED_DISK_PARAM_TABLE_OFFSET
            };

            cpu.gpr[gpr::RCX] = (cpu.gpr[gpr::RCX] & !0xFFFF) | (cl as u64) | ((ch as u64) << 8);
            // DL = number of drives; DH = max head.
            cpu.gpr[gpr::RDX] = (cpu.gpr[gpr::RDX] & !0xFFFF) | (dl as u64) | ((dh as u64) << 8);
            // Return a pointer to the appropriate parameter table (classic BIOS convention).
            set_real_mode_seg(&mut cpu.segments.es, BIOS_SEGMENT);
            cpu.gpr[gpr::RDI] = (cpu.gpr[gpr::RDI] & !0xFFFF) | (table_off as u64);
            // For floppy drives, many BIOSes also report a drive type code in BL.
            cpu.gpr[gpr::RBX] = (cpu.gpr[gpr::RBX] & !0xFF) | (drive_type as u64);
            cpu.gpr[gpr::RAX] &= !0xFF00u64;
            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
        }
        0x09 => {
            // Initialize drive parameters.
            //
            // Real BIOS implementations may use this to configure controller timing based on drive
            // type. Our disk interface is fully emulated in software, so this is a no-op that
            // validates drive presence.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
        }
        0x0C => {
            // Seek (CHS).
            //
            // Real hardware performs a mechanical seek; we model disk I/O synchronously so this is
            // a validation/no-op path.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }

            let cl = (cpu.gpr[gpr::RCX] & 0xFF) as u8;
            let ch = ((cpu.gpr[gpr::RCX] >> 8) & 0xFF) as u8;
            let dh = ((cpu.gpr[gpr::RDX] >> 8) & 0xFF) as u8;

            let cylinder = ((ch as u16) | (((cl as u16) & 0xC0) << 2)) as u32;
            let head = dh as u32;

            let (cylinders, heads, _) = geometry_for_drive(drive, disk.size_in_sectors());
            if head >= u32::from(heads) || cylinder >= u32::from(cylinders) {
                set_error(bios, cpu, 0x01);
                return;
            }

            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
        }
        0x10 => {
            // Check drive ready.
            //
            // We model disk I/O synchronously; if the drive exists, it is always ready.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64;
        }
        0x11 => {
            // Recalibrate drive.
            //
            // Real hardware would seek back to cylinder 0. We model disk I/O synchronously, so this
            // is a no-op that validates drive presence.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
        }
        0x0D => {
            // Alternate disk reset (often used for hard disks).
            //
            // Treat this as equivalent to AH=00h reset.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
        }
        0x14 => {
            // Controller diagnostics.
            //
            // Hardware BIOSes use this to run controller self-tests. We treat the emulated
            // controller as always healthy.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
        }
        0x15 => {
            // Get disk type.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            if drive < 0x80 {
                // Floppy drive present (with change-line support).
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | 0x0200;
            } else {
                // Fixed disk present. Return sector count in CX:DX (32-bit).
                let sectors_u32 = u32::try_from(disk.size_in_sectors()).unwrap_or(u32::MAX);
                let cx = (sectors_u32 >> 16) as u16;
                let dx = (sectors_u32 & 0xFFFF) as u16;
                cpu.gpr[gpr::RCX] = (cpu.gpr[gpr::RCX] & !0xFFFF) | (cx as u64);
                cpu.gpr[gpr::RDX] = (cpu.gpr[gpr::RDX] & !0xFFFF) | (dx as u64);
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | 0x0300;
            }
            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
        }
        0x16 => {
            // Get disk change status.
            //
            // DOS programs use this to detect when a floppy disk is swapped. We do not model a
            // disk-change line; always report "not changed" and succeed.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
        }
        0x41 => {
            // Extensions check.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            if (cpu.gpr[gpr::RBX] & 0xFFFF) == 0x55AA && drive >= 0x80 {
                // EDD extensions installation check.
                //
                // Strategy A: We advertise EDD 3.0 (AH=0x30) and implement the 0x42-byte
                // "Drive Parameter Table" returned by AH=48h when the caller provides a large
                // enough buffer.
                //
                // Some bootloaders/OS probes treat a mismatch between the reported EDD version
                // (AH=41h) and the returned AH=48h table size as a BIOS bug and will refuse to
                // use EDD services.
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x30u64 << 8);
                cpu.gpr[gpr::RBX] = (cpu.gpr[gpr::RBX] & !0xFFFF) | 0xAA55;
                // Feature bitmap (Phoenix EDD spec):
                // - bit 0: functions 42h-44h supported (extended read/write/verify)
                // - bit 1: functions 45h-47h supported (lock/eject/seek)
                // - bit 2: function 48h supported (drive parameter table)
                cpu.gpr[gpr::RCX] = (cpu.gpr[gpr::RCX] & !0xFFFF) | 0x0005;
                bios.last_int13_status = 0;
                cpu.rflags &= !FLAG_CF;
            } else {
                set_error(bios, cpu, 0x01);
            }
        }
        0x42 => {
            // Extended read via Disk Address Packet (DAP) at DS:SI.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            if drive < 0x80 {
                set_error(bios, cpu, 0x01);
                return;
            }

            let si = cpu.gpr[gpr::RSI] & 0xFFFF;
            let dap_addr = cpu.apply_a20(cpu.segments.ds.base.wrapping_add(si));
            let dap_size = bus.read_u8(dap_addr);
            if dap_size != 0x10 && dap_size != 0x18 {
                set_error(bios, cpu, 0x01);
                return;
            }

            if bus.read_u8(dap_addr + 1) != 0 {
                set_error(bios, cpu, 0x01);
                return;
            }

            let count = bus.read_u16(dap_addr + 2) as u64;
            if count == 0 {
                set_error(bios, cpu, 0x01);
                return;
            }
            let buf_off = bus.read_u16(dap_addr + 4);
            let buf_seg = bus.read_u16(dap_addr + 6);
            let lba = bus.read_u64(dap_addr + 8);
            let mut dst = cpu.apply_a20(((buf_seg as u64) << 4).wrapping_add(buf_off as u64));

            if dap_size == 0x18 {
                // 24-byte DAP includes a 64-bit flat pointer at offset 16.
                let buf64 = bus.read_u64(dap_addr + 16);
                if buf64 != 0 {
                    dst = cpu.apply_a20(buf64);
                }
            }

            let Some(end) = lba.checked_add(count) else {
                set_error(bios, cpu, 0x04);
                return;
            };
            if end > disk.size_in_sectors() {
                set_error(bios, cpu, 0x04);
                return;
            }

            for i in 0..count {
                let mut buf = [0u8; BIOS_SECTOR_SIZE];
                match disk.read_sector(lba + i, &mut buf) {
                    Ok(()) => bus.write_physical(dst + i * BIOS_SECTOR_SIZE as u64, &buf),
                    Err(e) => {
                        let status = disk_err_to_int13_status(e);
                        set_error(bios, cpu, status);
                        return;
                    }
                }
            }

            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64;
        }
        0x43 => {
            // Extended write via Disk Address Packet (EDD).
            //
            // Not supported with the current read-only [`BlockDevice`] implementation.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            if drive < 0x80 {
                set_error(bios, cpu, 0x01);
                return;
            }

            bios.last_int13_status = 0x03; // write protected
            cpu.rflags |= FLAG_CF;
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x03u64 << 8);
        }
        0x44 => {
            // Extended verify via Disk Address Packet (EDD).
            //
            // We treat this as a bounds check: if the requested LBA range is valid, succeed.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            if drive < 0x80 {
                set_error(bios, cpu, 0x01);
                return;
            }

            let si = cpu.gpr[gpr::RSI] & 0xFFFF;
            let dap_addr = cpu.apply_a20(cpu.segments.ds.base.wrapping_add(si));
            let dap_size = bus.read_u8(dap_addr);
            if dap_size != 0x10 && dap_size != 0x18 {
                set_error(bios, cpu, 0x01);
                return;
            }
            if bus.read_u8(dap_addr + 1) != 0 {
                set_error(bios, cpu, 0x01);
                return;
            }

            let count = bus.read_u16(dap_addr + 2) as u64;
            if count == 0 {
                set_error(bios, cpu, 0x01);
                return;
            }
            let lba = bus.read_u64(dap_addr + 8);

            let Some(end) = lba.checked_add(count) else {
                set_error(bios, cpu, 0x04);
                return;
            };
            if end > disk.size_in_sectors() {
                set_error(bios, cpu, 0x04);
                return;
            }

            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64;
        }
        0x48 => {
            // Extended get drive parameters (EDD).
            //
            // DS:SI points to a caller-supplied buffer; the first WORD is the
            // buffer size in bytes.
            if !drive_present(bios, bus, drive, cdrom_present) {
                set_error(bios, cpu, 0x01);
                return;
            }
            if drive < 0x80 {
                set_error(bios, cpu, 0x01);
                return;
            }

            let si = cpu.gpr[gpr::RSI] & 0xFFFF;
            let table_addr = cpu.apply_a20(cpu.segments.ds.base.wrapping_add(si));
            let buf_size = bus.read_u16(table_addr) as usize;
            const EDD_PARAMS_MIN_SIZE: usize = 0x1A;
            const EDD_PARAMS_V2_SIZE: usize = 0x1E;
            const EDD_PARAMS_V3_SIZE: usize = 0x42;
            if buf_size < EDD_PARAMS_MIN_SIZE {
                set_error(bios, cpu, 0x01);
                return;
            }

            // Fill the EDD drive parameter table.
            //
            // EDD 3.0 defines a 0x42-byte structure. For compatibility with older callers we
            // also support (and may return) smaller table sizes (down to 0x1A bytes).
            //
            // To avoid claiming a size we didn't fully populate, we only ever return one of the
            // standard EDD structure sizes:
            // - 0x42 (EDD 3.0) when the caller provides >= 0x42 bytes
            // - 0x1E (EDD 2.x) when the caller provides >= 0x1E bytes but < 0x42
            // - 0x1A (EDD 1.1) when the caller provides 0x1A..0x1D bytes
            let write_len = if buf_size >= EDD_PARAMS_V3_SIZE {
                EDD_PARAMS_V3_SIZE
            } else if buf_size >= EDD_PARAMS_V2_SIZE {
                EDD_PARAMS_V2_SIZE
            } else {
                EDD_PARAMS_MIN_SIZE
            };
            bus.write_u16(table_addr, write_len as u16);

            if write_len >= 4 {
                bus.write_u16(table_addr + 2, 0); // flags
            }
            if write_len >= 8 {
                bus.write_u32(table_addr + 4, 1024); // cylinders
            }
            if write_len >= 12 {
                bus.write_u32(table_addr + 8, 16); // heads
            }
            if write_len >= 16 {
                bus.write_u32(table_addr + 12, 63); // sectors/track
            }
            if write_len >= 24 {
                bus.write_u64(table_addr + 16, disk.size_in_sectors());
            }
            if write_len >= 26 {
                bus.write_u16(table_addr + 24, BIOS_SECTOR_SIZE as u16); // bytes/sector
            }
            if write_len >= 30 {
                bus.write_u32(table_addr + 26, 0); // DPTE pointer (unused)
            }

            if write_len >= EDD_PARAMS_V3_SIZE {
                // EDD 3.0 drive parameter table extension.
                //
                // Offsets and layout follow the Phoenix EDD 3.0 spec, as implemented by common
                // BIOSes and consumed by OS probes (e.g. Linux's `edd_device_params`).
                //
                // We fill only identification strings; the interface/device path fields are left
                // as zeroes (sufficient for most callers).
                let interface_type: [u8; 8] = if drive >= 0xE0 {
                    *b"ATAPI   "
                } else {
                    *b"ATA     "
                };

                bus.write_u16(table_addr + 0x1E, 0xBEDD); // key
                bus.write_u8(table_addr + 0x20, 0x1E); // device path info length
                bus.write_u8(table_addr + 0x21, 0x00); // reserved
                bus.write_u16(table_addr + 0x22, 0x0000); // reserved

                // Host bus type (4 bytes) and interface type (8 bytes) strings.
                for (i, b) in b"PCI ".iter().copied().enumerate() {
                    bus.write_u8(table_addr + 0x24 + i as u64, b);
                }
                for (i, b) in interface_type.iter().copied().enumerate() {
                    bus.write_u8(table_addr + 0x28 + i as u64, b);
                }

                // interface_path (8), device_path (8) and reserved (1) are all zero.
                for off in 0x30u64..0x40 {
                    bus.write_u8(table_addr + off, 0);
                }
                bus.write_u8(table_addr + 0x40, 0);

                // Compute the 8-bit checksum so that the sum of the device path info bytes
                // (host bus type .. checksum) is 0 modulo 256.
                let mut sum: u8 = 0;
                for off in 0x24..0x42 {
                    // Skip checksum byte while summing; it's at the last byte (0x41).
                    if off == 0x41 {
                        continue;
                    }
                    sum = sum.wrapping_add(bus.read_u8(table_addr + off));
                }
                let checksum = (0u8).wrapping_sub(sum);
                bus.write_u8(table_addr + 0x41, checksum);
            }

            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64;
        }
        _ => {
            const LOG_LIMIT: u32 = 16;
            let count = bios.unhandled_interrupt_log_count;
            bios.unhandled_interrupt_log_count = bios.unhandled_interrupt_log_count.wrapping_add(1);
            if count < LOG_LIMIT {
                let msg = format!("BIOS: unhandled INT 13h AH={ah:02x}\n");
                bios.push_tty_bytes(msg.as_bytes());
            } else if count == LOG_LIMIT {
                bios.push_tty_bytes(b"BIOS: further unhandled interrupts suppressed\n");
            }
            set_error(bios, cpu, 0x01);
        }
    }
}

fn handle_int15(bios: &mut Bios, cpu: &mut CpuState, bus: &mut dyn BiosBus) {
    let ax = (cpu.gpr[gpr::RAX] & 0xFFFF) as u16;
    match ax {
        0x2400 => {
            // Disable A20 gate.
            bus.set_a20_enabled(false);
            cpu.a20_enabled = bus.a20_enabled();
            cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
            cpu.rflags &= !FLAG_CF;
        }
        0x2401 => {
            // Enable A20 gate.
            bus.set_a20_enabled(true);
            cpu.a20_enabled = bus.a20_enabled();
            cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
            cpu.rflags &= !FLAG_CF;
        }
        0x2402 => {
            // Query A20 gate status: AL=0 disabled / AL=1 enabled.
            let al = if bus.a20_enabled() { 1u64 } else { 0u64 };
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | al;
            cpu.rflags &= !FLAG_CF;
        }
        0x2403 => {
            // Get A20 support (bitmask of supported methods).
            // We advertise keyboard controller + port 0x92 + INT15 methods.
            cpu.gpr[gpr::RBX] = (cpu.gpr[gpr::RBX] & !0xFFFF) | 0x0007;
            cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
            cpu.rflags &= !FLAG_CF;
        }
        0xE801 => {
            // Alternative extended memory query used by many bootloaders.
            if bios.e820_map.is_empty() {
                bios.e820_map = build_e820_map(
                    bios.config.memory_size_bytes,
                    bios.acpi_reclaimable,
                    bios.acpi_nvs,
                );
            }

            let (ax_kb, bx_blocks) = e801_from_e820(&bios.e820_map);
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (ax_kb as u64);
            cpu.gpr[gpr::RBX] = (cpu.gpr[gpr::RBX] & !0xFFFF) | (bx_blocks as u64);
            cpu.gpr[gpr::RCX] = (cpu.gpr[gpr::RCX] & !0xFFFF) | (ax_kb as u64);
            cpu.gpr[gpr::RDX] = (cpu.gpr[gpr::RDX] & !0xFFFF) | (bx_blocks as u64);
            cpu.rflags &= !FLAG_CF;
        }
        0xE820 => {
            // E820 memory map.
            if (cpu.gpr[gpr::RDX] & 0xFFFF_FFFF) != 0x534D_4150 {
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x86u64 << 8);
                return;
            }
            let req_size = (cpu.gpr[gpr::RCX] & 0xFFFF_FFFF) as u32;
            if req_size < 20 {
                cpu.rflags |= FLAG_CF;
                return;
            }

            if bios.e820_map.is_empty() {
                bios.e820_map = build_e820_map(
                    bios.config.memory_size_bytes,
                    bios.acpi_reclaimable,
                    bios.acpi_nvs,
                );
            }

            let idx = (cpu.gpr[gpr::RBX] & 0xFFFF_FFFF) as usize;
            if idx >= bios.e820_map.len() {
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x86u64 << 8);
                return;
            }
            let entry = bios.e820_map[idx];

            let di = cpu.gpr[gpr::RDI] & 0xFFFF;
            let dst = cpu.apply_a20(cpu.segments.es.base.wrapping_add(di));
            bus.write_u64(dst, entry.base);
            bus.write_u64(dst + 8, entry.length);
            bus.write_u32(dst + 16, entry.region_type);
            let resp_size = if req_size >= 24 { 24 } else { 20 };
            if resp_size >= 24 {
                bus.write_u32(dst + 20, entry.extended_attributes);
            }

            cpu.gpr[gpr::RAX] = 0x534D_4150;
            cpu.gpr[gpr::RCX] = resp_size as u64;
            cpu.gpr[gpr::RBX] = if idx + 1 < bios.e820_map.len() {
                (idx as u64) + 1
            } else {
                0
            };
            cpu.rflags &= !FLAG_CF;
        }
        0x8600 => {
            // Wait (CX:DX microseconds).
            //
            // We do not emulate wall-clock delays in the HLE BIOS; report success immediately so
            // callers that use this for simple hardware delay loops do not fail.
            cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
            cpu.rflags &= !FLAG_CF;
        }
        _ => match (ax >> 8) as u8 {
            0xC0 => {
                // Get system configuration parameters.
                //
                // Provide a small, PC-compatible "system configuration parameters" table in the
                // EBDA. DOS-era software sometimes uses this to probe for machine features without
                // relying on specific hardware I/O ports.
                //
                // Return:
                // - CF=0 on success
                // - AH=0
                // - ES:BX points to the table
                const TABLE_OFF: u16 = 0x0020;
                let table_seg = (super::EBDA_BASE / 16) as u16;
                let table_addr = (u64::from(table_seg) << 4) + u64::from(TABLE_OFF);

                // Table format (minimal):
                // - offset 0: WORD length in bytes
                // - offset 2+: implementation-defined feature bytes
                bus.write_u16(table_addr, 0x0010);
                for i in 2..0x10 {
                    bus.write_u8(table_addr + i, 0);
                }

                // ES:BX -> table.
                set_real_mode_seg(&mut cpu.segments.es, table_seg);
                cpu.gpr[gpr::RBX] = (cpu.gpr[gpr::RBX] & !0xFFFF) | (TABLE_OFF as u64);
                cpu.gpr[gpr::RAX] &= !0xFF00u64; // AH=0
                cpu.rflags &= !FLAG_CF;
            }
            0x88 => {
                // Extended memory size (KB above 1MB).
                let ext_kb = bios.config.memory_size_bytes.saturating_sub(1024 * 1024) / 1024;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | ext_kb.min(0xFFFF);
                cpu.rflags &= !FLAG_CF;
            }
            _ => {
                const LOG_LIMIT: u32 = 16;
                let count = bios.unhandled_interrupt_log_count;
                bios.unhandled_interrupt_log_count =
                    bios.unhandled_interrupt_log_count.wrapping_add(1);
                if count < LOG_LIMIT {
                    let msg = format!("BIOS: unhandled INT 15h AX={ax:04x}\n");
                    bios.push_tty_bytes(msg.as_bytes());
                } else if count == LOG_LIMIT {
                    bios.push_tty_bytes(b"BIOS: further unhandled interrupts suppressed\n");
                }
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x86u64 << 8);
            }
        },
    }
}

fn e801_from_e820(map: &[E820Entry]) -> (u16, u16) {
    const ONE_MIB: u64 = 0x0010_0000;
    const SIXTEEN_MIB: u64 = 0x0100_0000;
    const FOUR_GIB: u64 = 0x1_0000_0000;

    let bytes_1m_to_16m = sum_e820_ram(map, ONE_MIB, SIXTEEN_MIB);
    let bytes_16m_to_4g = sum_e820_ram(map, SIXTEEN_MIB, FOUR_GIB);

    let ax_kb = (bytes_1m_to_16m / 1024).min(0x3C00) as u16;
    let bx_blocks = (bytes_16m_to_4g / 65536).min(0xFFFF) as u16;
    (ax_kb, bx_blocks)
}

fn sum_e820_ram(map: &[E820Entry], start: u64, end: u64) -> u64 {
    let mut total = 0u64;
    for entry in map {
        // INT 15h E801 is a legacy sizing interface; treat ACPI reclaimable + NVS ranges as
        // "memory present" so small firmware-reserved windows do not perturb the reported size.
        if !matches!(entry.region_type, E820_RAM | E820_ACPI | E820_NVS) || entry.length == 0 {
            continue;
        }
        let entry_start = entry.base;
        let entry_end = entry.base.saturating_add(entry.length);
        let overlap_start = entry_start.max(start);
        let overlap_end = entry_end.min(end);
        if overlap_end > overlap_start {
            total = total.saturating_add(overlap_end - overlap_start);
        }
    }
    total
}

fn handle_int16(bios: &mut Bios, cpu: &mut CpuState) {
    let ah = ((cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8;
    match ah {
        0x00 => {
            // Read keystroke (blocking in real BIOS; we return 0 if none).
            if let Some(k) = bios.keyboard_queue.pop_front() {
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (k as u64);
                cpu.rflags &= !FLAG_ZF;
            } else {
                cpu.gpr[gpr::RAX] &= !0xFFFF;
                cpu.rflags |= FLAG_ZF;
            }
            cpu.rflags &= !FLAG_CF;
        }
        0x02 => {
            // Get shift flags (returns AL).
            //
            // We do not currently track keyboard modifier state in the BIOS data area; return 0
            // (no modifiers/locks active) but report success so bootloaders that probe this
            // function do not treat it as unimplemented.
            cpu.gpr[gpr::RAX] &= !0xFF;
            cpu.rflags &= !FLAG_CF;
        }
        0x12 => {
            // Get extended shift flags (returns AX).
            //
            // Like AH=02h, we do not currently track keyboard modifier state; report all flags
            // cleared but indicate success.
            cpu.gpr[gpr::RAX] &= !0xFFFF;
            cpu.rflags &= !FLAG_CF;
        }
        0x01 => {
            // Check for keystroke (ZF=1 if none).
            if let Some(&k) = bios.keyboard_queue.front() {
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (k as u64);
                cpu.rflags &= !FLAG_ZF;
            } else {
                cpu.rflags |= FLAG_ZF;
            }
            cpu.rflags &= !FLAG_CF;
        }
        0x03 => {
            // Set typematic rate/delay.
            //
            // We do not currently emulate the i8042 typematic timing; accept the request and
            // report success.
            cpu.rflags &= !FLAG_CF;
        }
        0x04 => {
            // Set keyboard click (AT and later).
            //
            // We do not emulate the PC speaker key click; accept the request and report success.
            cpu.rflags &= !FLAG_CF;
        }
        0x05 => {
            // Store keystroke in buffer (CH=scan code, CL=ASCII).
            //
            // This is used by some BIOS extensions and DOS programs to inject keyboard input.
            // Our BIOS models the keyboard buffer as a bounded FIFO queue (matching the BIOS Data
            // Area ring buffer capacity), so this always succeeds by dropping the oldest entry
            // when full (real hardware returns CF=1 when the 32-byte BIOS data area ring buffer is
            // full).
            let key = (cpu.gpr[gpr::RCX] & 0xFFFF) as u16;
            bios.push_key(key);
            cpu.rflags &= !FLAG_CF;
        }
        0x0C => {
            // Flush keyboard buffer and invoke another keyboard function (AL).
            //
            // Enhanced BIOSes support AH=0Ch as a "flush buffer then call" helper. We model the
            // keyboard buffer as a FIFO queue, so flushing is equivalent to clearing it.
            let al = (cpu.gpr[gpr::RAX] & 0xFF) as u8;
            bios.keyboard_queue.clear();

            // Call the requested function by rewriting AH and re-dispatching. This matches the
            // documented semantics: registers/flags are returned as if the subfunction was invoked
            // directly.
            if al == 0x0C {
                // Prevent infinite recursion if a guest passes the flush opcode itself.
                cpu.rflags |= FLAG_CF;
            } else {
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFF00) | ((al as u64) << 8);
                handle_int16(bios, cpu);
            }
        }
        0x10 => {
            // Read extended keystroke (blocking in real BIOS; we return 0 if none).
            //
            // For now this behaves like AH=00h: the BIOS does not distinguish "extended"
            // vs "non-extended" keys in the queue representation.
            if let Some(k) = bios.keyboard_queue.pop_front() {
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (k as u64);
                cpu.rflags &= !FLAG_ZF;
            } else {
                cpu.gpr[gpr::RAX] &= !0xFFFF;
                cpu.rflags |= FLAG_ZF;
            }
            cpu.rflags &= !FLAG_CF;
        }
        0x11 => {
            // Check for extended keystroke (ZF=1 if none).
            //
            // For now this behaves like AH=01h.
            if let Some(&k) = bios.keyboard_queue.front() {
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (k as u64);
                cpu.rflags &= !FLAG_ZF;
            } else {
                cpu.rflags |= FLAG_ZF;
            }
            cpu.rflags &= !FLAG_CF;
        }
        _ => {
            const LOG_LIMIT: u32 = 16;
            let count = bios.unhandled_interrupt_log_count;
            bios.unhandled_interrupt_log_count = bios.unhandled_interrupt_log_count.wrapping_add(1);
            if count < LOG_LIMIT {
                let msg = format!("BIOS: unhandled INT 16h AH={ah:02x}\n");
                bios.push_tty_bytes(msg.as_bytes());
            } else if count == LOG_LIMIT {
                bios.push_tty_bytes(b"BIOS: further unhandled interrupts suppressed\n");
            }
            cpu.rflags |= FLAG_CF;
        }
    }
}

fn handle_int1a(bios: &mut Bios, cpu: &mut CpuState, bus: &mut dyn BiosBus) {
    let ah = ((cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8;
    match ah {
        0x00 => {
            // Get system time: CX:DX = ticks since midnight, AL = midnight flag.
            let ticks = bios.bda_time.tick_count();
            let midnight_flag = bios.bda_time.midnight_flag();

            cpu.gpr[gpr::RCX] = (cpu.gpr[gpr::RCX] & !0xFFFF) | ((ticks >> 16) as u64);
            cpu.gpr[gpr::RDX] = (cpu.gpr[gpr::RDX] & !0xFFFF) | ((ticks & 0xFFFF) as u64);
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFF) | (midnight_flag as u64);
            cpu.rflags &= !FLAG_CF;

            bios.bda_time.clear_midnight_flag();
            bios.bda_time.write_to_bda(bus);
        }
        0x01 => {
            // Set system time from CX:DX.
            let ticks = (((cpu.gpr[gpr::RCX] & 0xFFFF) as u32) << 16)
                | ((cpu.gpr[gpr::RDX] & 0xFFFF) as u32);
            bios.bda_time.set_tick_count(bus, ticks);
            let _ = bios
                .rtc
                .set_time_of_day(super::BdaTime::duration_from_ticks(ticks));

            cpu.gpr[gpr::RAX] &= !0xFF00;
            cpu.rflags &= !FLAG_CF;
        }
        0x02 => {
            // Read RTC time.
            let time = bios.rtc.read_time();
            let cx = ((time.hour as u16) << 8) | (time.minute as u16);
            let dx = ((time.second as u16) << 8) | (time.daylight_savings as u16);
            cpu.gpr[gpr::RCX] = (cpu.gpr[gpr::RCX] & !0xFFFF) | (cx as u64);
            cpu.gpr[gpr::RDX] = (cpu.gpr[gpr::RDX] & !0xFFFF) | (dx as u64);
            cpu.gpr[gpr::RAX] &= !0xFF00;
            cpu.rflags &= !FLAG_CF;
        }
        0x03 => {
            // Set RTC time.
            let cx = (cpu.gpr[gpr::RCX] & 0xFFFF) as u16;
            let dx = (cpu.gpr[gpr::RDX] & 0xFFFF) as u16;
            let hour = (cx >> 8) as u8;
            let minute = (cx & 0xFF) as u8;
            let second = (dx >> 8) as u8;
            let daylight_savings = (dx & 0xFF) as u8;

            match bios
                .rtc
                .set_time_cmos(hour, minute, second, daylight_savings)
            {
                Ok(()) => {
                    bios.bda_time = super::BdaTime::from_rtc(&bios.rtc);
                    bios.bda_time.write_to_bda(bus);
                    cpu.gpr[gpr::RAX] &= !0xFF00;
                    cpu.rflags &= !FLAG_CF;
                }
                Err(_) => {
                    cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFF00) | (1u64 << 8);
                    cpu.rflags |= FLAG_CF;
                }
            }
        }
        0x04 => {
            // Read RTC date.
            let date = bios.rtc.read_date();
            let cx = ((date.century as u16) << 8) | (date.year as u16);
            let dx = ((date.month as u16) << 8) | (date.day as u16);
            cpu.gpr[gpr::RCX] = (cpu.gpr[gpr::RCX] & !0xFFFF) | (cx as u64);
            cpu.gpr[gpr::RDX] = (cpu.gpr[gpr::RDX] & !0xFFFF) | (dx as u64);
            cpu.gpr[gpr::RAX] &= !0xFF00;
            cpu.rflags &= !FLAG_CF;
        }
        0x05 => {
            // Set RTC date.
            let cx = (cpu.gpr[gpr::RCX] & 0xFFFF) as u16;
            let dx = (cpu.gpr[gpr::RDX] & 0xFFFF) as u16;
            let century = (cx >> 8) as u8;
            let year = (cx & 0xFF) as u8;
            let month = (dx >> 8) as u8;
            let day = (dx & 0xFF) as u8;

            match bios.rtc.set_date_cmos(century, year, month, day) {
                Ok(()) => {
                    cpu.gpr[gpr::RAX] &= !0xFF00;
                    cpu.rflags &= !FLAG_CF;
                }
                Err(_) => {
                    cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFF00) | (1u64 << 8);
                    cpu.rflags |= FLAG_CF;
                }
            }
        }
        _ => {
            // Common extension probe: PCI BIOS interface uses AH=0xB1.
            //
            // We don't currently expose a PCI BIOS interface surface (callers should use native
            // config space + ACPI), but returning a conventional status code avoids confusing
            // legacy probes that expect AH to be set on failure.
            if ah == 0xB1 {
                // 0x81 = function not supported.
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFF00) | (0x81u64 << 8);
            }
            cpu.rflags |= FLAG_CF;
        }
    }
}

fn build_e820_map(
    total_memory: u64,
    acpi_region: Option<(u64, u64)>,
    nvs_region: Option<(u64, u64)>,
) -> Vec<E820Entry> {
    let mut map = Vec::new();
    const ONE_MIB: u64 = 0x0010_0000;
    // PCIe ECAM / MMCONFIG window reserved for PCI configuration space accesses.
    //
    // These constants are re-exported from `bios` (source-of-truth: `aero-pc-constants`) so the
    // ECAM window stays consistent across:
    // - ACPI `MCFG` generation
    // - platform MMIO mapping
    // - E820 reservations
    const PCIE_ECAM_BASE: u64 = super::PCIE_ECAM_BASE;
    const PCIE_ECAM_SIZE: u64 = super::PCIE_ECAM_SIZE;
    // The start of the "low memory" window available for RAM below 4GiB.
    //
    // The ECAM region lives immediately below the typical PCI BAR allocation window, so any RAM
    // beyond this address must be remapped above 4GiB to keep the ECAM window reserved.
    const LOW_RAM_END: u64 = PCIE_ECAM_BASE;
    // Typical x86 systems reserve a PCI/MMIO window below 4GiB. This must be
    // reported via E820 so OSes (notably Windows) do not treat device MMIO as RAM.
    const PCI_HOLE_START: u64 = 0xC000_0000;
    const PCI_HOLE_END: u64 = 0x1_0000_0000;

    fn push_region(entries: &mut Vec<E820Entry>, base: u64, end: u64, region_type: u32) {
        if end <= base {
            return;
        }
        entries.push(E820Entry {
            base,
            length: end - base,
            region_type,
            extended_attributes: 1,
        });
    }

    fn push_ram_split_by_reserved(
        entries: &mut Vec<E820Entry>,
        base: u64,
        end: u64,
        reserved: &[(u64, u64, u32)],
    ) {
        if end <= base {
            return;
        }

        let mut cursor = base;
        for &(r_base, r_len, r_type) in reserved {
            let r_end = r_base.saturating_add(r_len);
            let mut a_start = r_base.clamp(base, end);
            let a_end = r_end.clamp(base, end);
            if a_end <= a_start {
                continue;
            }

            // The reserved windows are expected to be sorted by base, but may still overlap if a
            // caller provides inconsistent ACPI/NVS placements. Clamp to `cursor` so we never emit
            // overlapping E820 entries.
            if a_end <= cursor {
                continue;
            }
            if a_start < cursor {
                a_start = cursor;
            }

            if a_start > cursor {
                push_region(entries, cursor, a_start, E820_RAM);
            }
            push_region(entries, a_start, a_end, r_type);
            cursor = a_end;
        }

        if end > cursor {
            push_region(entries, cursor, end, E820_RAM);
        }
    }

    let mut reserved = Vec::new();
    if let Some((base, len)) = acpi_region {
        reserved.push((base, len, E820_ACPI));
    }
    if let Some((base, len)) = nvs_region {
        reserved.push((base, len, E820_NVS));
    }
    reserved.sort_by_key(|(base, _, _)| *base);

    // Conventional memory (0 - EBDA).
    //
    // Clamp the usable RAM entry to the configured guest RAM size so we never report more RAM
    // than actually exists (e.g. for pathological/defensive configurations like `total_memory=0`).
    push_region(&mut map, 0, EBDA_BASE.min(total_memory), E820_RAM);

    // EBDA reserved.
    push_region(
        &mut map,
        EBDA_BASE,
        EBDA_BASE + EBDA_SIZE as u64,
        E820_RESERVED,
    );

    // VGA/BIOS/option ROM region.
    push_region(&mut map, 0x000A_0000, ONE_MIB, E820_RESERVED);

    if total_memory <= ONE_MIB {
        // Guest RAM smaller than 1MiB is unusual, but the map is still well-formed:
        // - Conventional RAM is clamped above.
        // - EBDA/VGA regions remain reserved.
        return map;
    }

    // Low extended memory: [1MiB, LOW_RAM_END) with reserved splits.
    let low_ram_end = total_memory.min(LOW_RAM_END);
    push_ram_split_by_reserved(&mut map, ONE_MIB, low_ram_end, &reserved);

    // PCI/MMIO hole + high memory remap when total RAM exceeds the low RAM window.
    if total_memory > LOW_RAM_END {
        // Reserve the PCIe ECAM window (MCFG / config space).
        push_region(
            &mut map,
            PCIE_ECAM_BASE,
            PCIE_ECAM_BASE.saturating_add(PCIE_ECAM_SIZE),
            E820_RESERVED,
        );

        // Reserve the remaining PCI/MMIO window below 4GiB.
        push_region(&mut map, PCI_HOLE_START, PCI_HOLE_END, E820_RESERVED);

        let high_ram_len = total_memory - LOW_RAM_END;
        let high_ram_end = PCI_HOLE_END.saturating_add(high_ram_len);
        push_ram_split_by_reserved(&mut map, PCI_HOLE_END, high_ram_end, &reserved);
    }

    map
}

#[cfg(test)]
mod tests {
    use super::super::{
        ivt, A20Gate, BiosConfig, BlockDevice, CdromDevice, DiskError, ElToritoBootInfo,
        ElToritoBootMediaType, InMemoryCdrom, InMemoryDisk, TestMemory, BDA_BASE, EBDA_BASE,
        EBDA_SIZE, MAX_TTY_OUTPUT_BYTES, PCIE_ECAM_BASE, PCIE_ECAM_SIZE,
    };
    use super::*;
    use aero_cpu_core::state::{gpr, CpuMode, CpuState, FLAG_CF, FLAG_ZF};
    use memory::MemoryBus as _;

    #[test]
    fn int10_teletype_tty_output_is_a_bounded_rolling_log() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);
        bios.video
            .vga
            .set_text_mode_03h(&mut super::super::BiosMemoryBus::new(&mut mem), true);

        let total = MAX_TTY_OUTPUT_BYTES + 1024;
        for i in 0..total {
            cpu.gpr[gpr::RAX] = 0x0E00 | ((i % 256) as u64);
            handle_int10(&mut bios, &mut cpu, &mut mem);
        }

        let out = bios.tty_output();
        assert_eq!(out.len(), MAX_TTY_OUTPUT_BYTES);

        let start = total - MAX_TTY_OUTPUT_BYTES;
        assert_eq!(out[0], (start % 256) as u8);
        assert_eq!(out[out.len() - 1], ((total - 1) % 256) as u8);

        let tail = &out[out.len() - 16..];
        let expected_tail: Vec<u8> = (total - 16..total).map(|i| (i % 256) as u8).collect();
        assert_eq!(tail, expected_tail.as_slice());
    }

    #[test]
    fn unhandled_interrupt_log_rate_limit_is_per_bios_instance() {
        const LOG_LIMIT: usize = 16;

        let mut bios1 = Bios::new(BiosConfig::default());
        let mut cpu1 = CpuState::new(CpuMode::Real);
        let mut mem1 = TestMemory::new(2 * 1024 * 1024);
        let mut disk1 = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);

        ivt::init_bda(&mut mem1, 0x80);
        cpu1.a20_enabled = mem1.a20_enabled();
        set_real_mode_seg(&mut cpu1.segments.ss, 0);
        cpu1.gpr[gpr::RSP] = 0x1000;
        mem1.write_u16(0x1000 + 4, 0x0002);

        // Drive the rate limiter past the suppression threshold on the first BIOS instance.
        for _ in 0..(LOG_LIMIT + 1) {
            bios1.dispatch_interrupt(0x77, &mut cpu1, &mut mem1, &mut disk1, None);
        }

        let out1 = String::from_utf8_lossy(bios1.tty_output());
        let msg1 = "BIOS: unhandled interrupt 77\n";
        assert_eq!(out1.matches(msg1).count(), LOG_LIMIT);
        assert_eq!(
            out1.matches("BIOS: further unhandled interrupts suppressed\n")
                .count(),
            1
        );

        // A fresh BIOS instance should start logging from scratch, even if another instance has
        // already exhausted its rate limit.
        let mut bios2 = Bios::new(BiosConfig::default());
        let mut cpu2 = CpuState::new(CpuMode::Real);
        let mut mem2 = TestMemory::new(2 * 1024 * 1024);
        let mut disk2 = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);

        ivt::init_bda(&mut mem2, 0x80);
        cpu2.a20_enabled = mem2.a20_enabled();
        set_real_mode_seg(&mut cpu2.segments.ss, 0);
        cpu2.gpr[gpr::RSP] = 0x1000;
        mem2.write_u16(0x1000 + 4, 0x0002);

        bios2.dispatch_interrupt(0x78, &mut cpu2, &mut mem2, &mut disk2, None);

        let out2 = String::from_utf8_lossy(bios2.tty_output());
        assert_eq!(out2.matches("BIOS: unhandled interrupt 78\n").count(), 1);
        assert_eq!(
            out2.matches("BIOS: further unhandled interrupts suppressed\n")
                .count(),
            0
        );
    }

    #[test]
    fn int13_ext_read_reads_lba_into_memory() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 4];
        disk_bytes[BIOS_SECTOR_SIZE..BIOS_SECTOR_SIZE * 2].fill(0xAA);
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0500;
        cpu.gpr[gpr::RDX] = 0x80; // DL = HDD0
        cpu.gpr[gpr::RAX] = 0x4200; // AH=42h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);
        cpu.a20_enabled = mem.a20_enabled();
        let dap_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0500);
        mem.write_u8(dap_addr, 0x10);
        mem.write_u8(dap_addr + 1, 0x00);
        mem.write_u16(dap_addr + 2, 1); // count
        mem.write_u16(dap_addr + 4, 0x1000); // offset
        mem.write_u16(dap_addr + 6, 0x0000); // segment
        mem.write_u64(dap_addr + 8, 1); // LBA

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);

        let buf = mem.read_bytes(0x1000, BIOS_SECTOR_SIZE);
        assert_eq!(buf, vec![0xAA; BIOS_SECTOR_SIZE]);
    }

    #[test]
    fn int13_ext_read_24byte_dap_uses_flat_pointer_hdd() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 4];
        disk_bytes[BIOS_SECTOR_SIZE..BIOS_SECTOR_SIZE * 2].fill(0xAA);
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0500;
        cpu.gpr[gpr::RDX] = 0x80; // DL = HDD0
        cpu.gpr[gpr::RAX] = 0x4200; // AH=42h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);
        mem.set_a20_enabled(true);
        cpu.a20_enabled = mem.a20_enabled();

        // Use a destination above 1MiB so a segment:offset pointer cannot address it in real mode.
        const FLAT_DST: u64 = 0x0011_0000;

        // Set segment:offset to garbage; ensure the BIOS ignores it when the 64-bit flat pointer
        // is non-zero.
        let bogus_off: u16 = 0x5678;
        let bogus_seg: u16 = 0x1234;
        let bogus_dst = ((bogus_seg as u64) << 4).wrapping_add(bogus_off as u64);
        mem.write_physical(bogus_dst, &vec![0x55; BIOS_SECTOR_SIZE]);

        let dap_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0500);
        mem.write_u8(dap_addr, 0x18); // 24-byte DAP
        mem.write_u8(dap_addr + 1, 0x00);
        mem.write_u16(dap_addr + 2, 1); // count
        mem.write_u16(dap_addr + 4, bogus_off);
        mem.write_u16(dap_addr + 6, bogus_seg);
        mem.write_u64(dap_addr + 8, 1); // LBA
        mem.write_u64(dap_addr + 16, FLAT_DST); // flat buffer pointer

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);

        let buf = mem.read_bytes(FLAT_DST, BIOS_SECTOR_SIZE);
        assert_eq!(buf, vec![0xAA; BIOS_SECTOR_SIZE]);
        let bogus_buf = mem.read_bytes(bogus_dst, BIOS_SECTOR_SIZE);
        assert_eq!(bogus_buf, vec![0x55; BIOS_SECTOR_SIZE]);
    }

    #[test]
    fn int13_edd_extensions_check_hdd_succeeds_and_reports_features() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x4100; // AH=41h
        cpu.gpr[gpr::RBX] = 0x55AA;
        cpu.gpr[gpr::RDX] = 0x80; // DL=HDD0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RBX] as u16, 0xAA55);
        let cx = cpu.gpr[gpr::RCX] as u16;
        assert_eq!(cx & 0x0005, 0x0005); // at least bits 0 + 2 (read DAP + get params)

        // AH returns an EDD version on success; avoid over-constraining it.
        let ah = ((cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8;
        assert_ne!(ah, 0x01);
    }

    #[test]
    fn int13_edd_extensions_check_cd_succeeds_and_reports_features() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x4100; // AH=41h
        cpu.gpr[gpr::RBX] = 0x55AA;
        cpu.gpr[gpr::RDX] = 0xE0; // DL=CD0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0xE0);

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RBX] as u16, 0xAA55);
        let cx = cpu.gpr[gpr::RCX] as u16;
        assert_eq!(cx & 0x0005, 0x0005);

        let ah = ((cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8;
        assert_ne!(ah, 0x01);
    }

    #[test]
    fn int13_edd_extensions_check_rejects_bad_signature_word() {
        // HDD path.
        {
            let mut bios = Bios::new(BiosConfig::default());
            let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);
            let mut cpu = CpuState::new(CpuMode::Real);
            cpu.gpr[gpr::RAX] = 0x4100;
            cpu.gpr[gpr::RBX] = 0x1234; // not 0x55AA
            cpu.gpr[gpr::RDX] = 0x80;

            let mut mem = TestMemory::new(2 * 1024 * 1024);
            ivt::init_bda(&mut mem, 0x80);

            handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

            assert_ne!(cpu.rflags & FLAG_CF, 0);
            assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0x01);
        }

        // CD path.
        {
            let mut bios = Bios::new(BiosConfig {
                boot_drive: 0xE0,
                ..BiosConfig::default()
            });
            let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);
            let mut cpu = CpuState::new(CpuMode::Real);
            cpu.gpr[gpr::RAX] = 0x4100;
            cpu.gpr[gpr::RBX] = 0x1234; // not 0x55AA
            cpu.gpr[gpr::RDX] = 0xE0;

            let mut mem = TestMemory::new(2 * 1024 * 1024);
            ivt::init_bda(&mut mem, 0xE0);

            handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

            assert_ne!(cpu.rflags & FLAG_CF, 0);
            assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0x01);
        }
    }

    #[test]
    fn int13_ext_get_drive_params_reports_sector_count() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 8];
        let sectors = (disk_bytes.len() / BIOS_SECTOR_SIZE) as u64;
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0600;
        cpu.gpr[gpr::RDX] = 0x80; // DL = HDD0
        cpu.gpr[gpr::RAX] = 0x4800; // AH=48h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);
        cpu.a20_enabled = mem.a20_enabled();
        let table_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0600);
        mem.write_u16(table_addr, 0x1E); // buffer size

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);

        assert_eq!(mem.read_u64(table_addr + 16), sectors);
        assert_eq!(mem.read_u16(table_addr + 24), BIOS_SECTOR_SIZE as u16);
    }

    #[derive(Debug)]
    struct PatternCdrom {
        sectors: u64,
    }

    impl PatternCdrom {
        fn new(sectors: u64) -> Self {
            Self { sectors }
        }
    }

    impl CdromDevice for PatternCdrom {
        fn read_sector(&mut self, lba: u64, buf: &mut [u8; 2048]) -> Result<(), DiskError> {
            if lba >= self.sectors {
                return Err(DiskError::OutOfRange);
            }
            for (i, slot) in buf.iter_mut().enumerate() {
                *slot = (lba as u8).wrapping_add(i as u8);
            }
            Ok(())
        }

        fn size_in_sectors(&self) -> u64 {
            self.sectors
        }
    }

    #[test]
    fn int13_ext_check_succeeds_for_cdrom_drive() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);
        let mut cdrom = PatternCdrom::new(16);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x4100; // AH=41h
        cpu.gpr[gpr::RBX] = 0x55AA;
        cpu.gpr[gpr::RDX] = 0xE0; // DL = CD0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0xE0);

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, Some(&mut cdrom));

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RBX] as u16, 0xAA55);
        let cx = cpu.gpr[gpr::RCX] as u16;
        assert_eq!(cx & 0x0005, 0x0005); // AH=42h + AH=48h supported

        // AH returns an EDD version on success; avoid over-constraining it.
        let ah = ((cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8;
        assert_ne!(ah, 0x01);
    }

    #[test]
    fn int13_ext_read_cd_out_of_range_fails_with_stable_error_code() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);
        let mut cdrom = PatternCdrom::new(4);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0500;
        cpu.gpr[gpr::RDX] = 0xE0;
        cpu.gpr[gpr::RAX] = 0x4200; // AH=42h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0xE0);
        cpu.a20_enabled = mem.a20_enabled();

        let dap_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0500);
        mem.write_u8(dap_addr, 0x10);
        mem.write_u8(dap_addr + 1, 0x00);
        mem.write_u16(dap_addr + 2, 1); // count
        mem.write_u16(dap_addr + 4, 0x2000); // offset
        mem.write_u16(dap_addr + 6, 0x0000); // segment
        mem.write_u64(dap_addr + 8, 4); // LBA == size => out of range

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, Some(&mut cdrom));

        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0x04);
    }

    #[test]
    fn int13_chs_read_is_rejected_for_cdrom_drive() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);
        let mut cdrom = PatternCdrom::new(8);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.es, 0);
        cpu.gpr[gpr::RBX] = 0x1000;
        cpu.gpr[gpr::RAX] = 0x0201; // AH=02h read, AL=1
        cpu.gpr[gpr::RCX] = 0x0001; // CH=0, CL=1
        cpu.gpr[gpr::RDX] = 0xE0;

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0xE0);

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, Some(&mut cdrom));

        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0x01);
    }

    #[test]
    fn int13_ext_get_drive_params_cd_reports_2048_bytes_per_sector() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let iso_sectors = 4;
        let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);
        let mut cdrom = PatternCdrom::new(iso_sectors);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0600;
        cpu.gpr[gpr::RDX] = 0xE0; // DL = CD0
        cpu.gpr[gpr::RAX] = 0x4800; // AH=48h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0xE0);
        cpu.a20_enabled = mem.a20_enabled();
        let table_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0600);
        mem.write_u16(table_addr, 0x1E); // buffer size

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, Some(&mut cdrom));

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
        assert_eq!(mem.read_u64(table_addr + 16), iso_sectors);
        assert_eq!(mem.read_u16(table_addr + 24), 2048);
    }

    #[test]
    fn int13_ext_verify_cd_uses_cdrom_backend_size_when_provided() {
        // When a real `CdromDevice` backend is supplied, use its size for bounds-checking rather
        // than falling back to `disk.size_in_sectors()/4` (which is only meaningful for the
        // in-memory ISO compatibility backend).
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]); // too small to back even 1 ISO sector
        let mut cdrom = PatternCdrom::new(16);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0500;
        cpu.gpr[gpr::RDX] = 0xE0; // DL = CD0
        cpu.gpr[gpr::RAX] = 0x4400; // AH=44h extended verify

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0xE0);
        cpu.a20_enabled = mem.a20_enabled();

        let dap_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0500);
        mem.write_u8(dap_addr, 0x10);
        mem.write_u8(dap_addr + 1, 0x00);
        mem.write_u16(dap_addr + 2, 1); // count (2048-byte sectors)
        mem.write_u16(dap_addr + 4, 0x0000); // dst offset (ignored for verify)
        mem.write_u16(dap_addr + 6, 0x0000); // dst segment (ignored for verify)
        mem.write_u64(dap_addr + 8, 0); // ISO LBA

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, Some(&mut cdrom));

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
    }

    #[test]
    fn int13_ext_read_cd_reads_2048b_sector_into_memory() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);
        let mut cdrom = PatternCdrom::new(32);
        let lba = 1u64;

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0500;
        cpu.gpr[gpr::RDX] = 0xE0; // DL = CD0
        cpu.gpr[gpr::RAX] = 0x4200; // AH=42h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0xE0);
        cpu.a20_enabled = mem.a20_enabled();

        // Sentinel-fill so we can assert the BIOS writes exactly 2048 bytes.
        for off in 0..4096u64 {
            mem.write_u8(0x2000 + off, 0xAA);
        }

        let dap_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0500);
        mem.write_u8(dap_addr, 0x10);
        mem.write_u8(dap_addr + 1, 0x00);
        mem.write_u16(dap_addr + 2, 1); // count (2048-byte sectors)
        mem.write_u16(dap_addr + 4, 0x2000); // offset
        mem.write_u16(dap_addr + 6, 0x0000); // segment
        mem.write_u64(dap_addr + 8, lba); // ISO LBA

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, Some(&mut cdrom));

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
        let buf = mem.read_bytes(0x2000, 2048);
        let mut expected = vec![0u8; 2048];
        for (i, slot) in expected.iter_mut().enumerate() {
            *slot = (lba as u8).wrapping_add(i as u8);
        }
        assert_eq!(buf, expected);
        assert_eq!(mem.read_u8(0x2000 + 2048), 0xAA);
    }

    #[test]
    fn int13_ext_read_cd_works_when_booted_from_hdd_if_cdrom_backend_present() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0x80,
            ..BiosConfig::default()
        });
        let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);
        let mut cdrom = PatternCdrom::new(32);
        let lba = 1u64;

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0500;
        cpu.gpr[gpr::RDX] = 0xE0; // DL = CD0
        cpu.gpr[gpr::RAX] = 0x4200; // AH=42h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);
        cpu.a20_enabled = mem.a20_enabled();

        // Sentinel-fill so we can assert the BIOS writes exactly 2048 bytes.
        for off in 0..4096u64 {
            mem.write_u8(0x2000 + off, 0xAA);
        }

        let dap_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0500);
        mem.write_u8(dap_addr, 0x10);
        mem.write_u8(dap_addr + 1, 0x00);
        mem.write_u16(dap_addr + 2, 1); // count (2048-byte sectors)
        mem.write_u16(dap_addr + 4, 0x2000); // offset
        mem.write_u16(dap_addr + 6, 0x0000); // segment
        mem.write_u64(dap_addr + 8, lba); // ISO LBA

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, Some(&mut cdrom));

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
        let buf = mem.read_bytes(0x2000, 2048);
        let mut expected = vec![0u8; 2048];
        for (i, slot) in expected.iter_mut().enumerate() {
            *slot = (lba as u8).wrapping_add(i as u8);
        }
        assert_eq!(buf, expected);
        assert_eq!(mem.read_u8(0x2000 + 2048), 0xAA);
    }

    #[test]
    fn int13_ext_read_cd_24byte_dap_uses_flat_pointer_and_2048b_sectors() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });

        let mut disk_bytes = vec![0u8; 2048 * 4];
        let mut expected = vec![0u8; 2048];
        expected[0..BIOS_SECTOR_SIZE].fill(0x11);
        expected[BIOS_SECTOR_SIZE..BIOS_SECTOR_SIZE * 2].fill(0x22);
        expected[BIOS_SECTOR_SIZE * 2..BIOS_SECTOR_SIZE * 3].fill(0x33);
        expected[BIOS_SECTOR_SIZE * 3..BIOS_SECTOR_SIZE * 4].fill(0x44);
        disk_bytes[2048..4096].copy_from_slice(&expected); // ISO LBA 1
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0500;
        cpu.gpr[gpr::RDX] = 0xE0; // DL = CD0
        cpu.gpr[gpr::RAX] = 0x4200; // AH=42h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0xE0);
        mem.set_a20_enabled(true);
        cpu.a20_enabled = mem.a20_enabled();

        const FLAT_DST: u64 = 0x0011_0000;

        let bogus_off: u16 = 0x0BAD;
        let bogus_seg: u16 = 0xB002;
        let bogus_dst = ((bogus_seg as u64) << 4).wrapping_add(bogus_off as u64);
        mem.write_physical(bogus_dst, &vec![0x77; 2048]);

        let dap_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0500);
        mem.write_u8(dap_addr, 0x18); // 24-byte DAP
        mem.write_u8(dap_addr + 1, 0x00);
        mem.write_u16(dap_addr + 2, 1); // count (2048-byte sectors)
        mem.write_u16(dap_addr + 4, bogus_off);
        mem.write_u16(dap_addr + 6, bogus_seg);
        mem.write_u64(dap_addr + 8, 1); // ISO LBA
        mem.write_u64(dap_addr + 16, FLAT_DST); // flat buffer pointer

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);

        let buf = mem.read_bytes(FLAT_DST, 2048);
        assert_eq!(buf, expected);
        let bogus_buf = mem.read_bytes(bogus_dst, 2048);
        assert_eq!(bogus_buf, vec![0x77; 2048]);
    }

    #[test]
    fn int13_ext_read_cd_24byte_dap_uses_flat_64bit_destination_pointer() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let mut disk_bytes = vec![0u8; 2048 * 4];
        let mut expected = vec![0u8; 2048];
        expected[0..BIOS_SECTOR_SIZE].fill(0x11);
        expected[BIOS_SECTOR_SIZE..BIOS_SECTOR_SIZE * 2].fill(0x22);
        expected[BIOS_SECTOR_SIZE * 2..BIOS_SECTOR_SIZE * 3].fill(0x33);
        expected[BIOS_SECTOR_SIZE * 3..BIOS_SECTOR_SIZE * 4].fill(0x44);
        disk_bytes[2048..4096].copy_from_slice(&expected); // ISO LBA 1
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0500;
        cpu.gpr[gpr::RDX] = 0xE0; // DL = CD0
        cpu.gpr[gpr::RAX] = 0x4200; // AH=42h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0xE0);

        // Use a >1MiB destination address, which requires A20 to be enabled.
        mem.set_a20_enabled(true);
        cpu.a20_enabled = mem.a20_enabled();

        let dap_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0500);
        mem.write_u8(dap_addr, 0x18);
        mem.write_u8(dap_addr + 1, 0x00);
        mem.write_u16(dap_addr + 2, 1); // count (2048-byte sectors)
        mem.write_u16(dap_addr + 4, 0x2000); // offset (should be ignored)
        mem.write_u16(dap_addr + 6, 0x0000); // segment (should be ignored)
        mem.write_u64(dap_addr + 8, 1); // ISO LBA
        const FLAT_DST: u64 = 0x0011_0000;
        mem.write_u64(dap_addr + 16, FLAT_DST); // flat 64-bit destination pointer

        // Fill the segment:offset destination with a sentinel pattern to prove it's ignored.
        let sentinel = vec![0xCC; 2048];
        mem.write_physical(0x2000, &sentinel);
        // Also seed the real destination with a different sentinel so we can see the overwrite.
        mem.write_physical(FLAT_DST, &vec![0xDD; 2048]);

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
        assert_eq!(mem.read_bytes(FLAT_DST, 2048), expected);
        assert_eq!(mem.read_bytes(0x2000, 2048), sentinel);
    }

    #[test]
    fn int13_eltorito_services_return_zeroed_fields_without_boot_metadata() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);

        let mut cpu = CpuState::new(CpuMode::Real);

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        cpu.a20_enabled = mem.a20_enabled();

        // Terminate disk emulation should succeed as a no-op for CD boot drive numbers even when
        // POST hasn't provided El Torito metadata.
        cpu.gpr[gpr::RAX] = 0x4B00;
        cpu.gpr[gpr::RDX] = 0xE0;
        cpu.rflags |= FLAG_CF;
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);

        // Get disk emulation status should fill the ES:DI buffer with best-effort fields.
        cpu.gpr[gpr::RAX] = 0x4B01; // AH=4Bh AL=01h get status
        cpu.gpr[gpr::RDX] = 0xE0; // DL=CD-ROM boot drive (typical)
        set_real_mode_seg(&mut cpu.segments.es, 0);
        cpu.gpr[gpr::RDI] = 0x0500;
        mem.write_u8(0x0500, 0x13);

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);

        assert_eq!(mem.read_u8(0x0500), 0x13);
        assert_eq!(mem.read_u8(0x0501), 0x00); // no emulation
        assert_eq!(mem.read_u8(0x0502), 0xE0); // boot drive
        assert_eq!(mem.read_u8(0x0503), 0x00); // controller index
        assert_eq!(mem.read_u32(0x0504), 0);
        assert_eq!(mem.read_u32(0x0508), 0);
        assert_eq!(mem.read_u16(0x050C), 0);
        assert_eq!(mem.read_u16(0x050E), 0);
        assert_eq!(mem.read_u8(0x0510), 0);
        assert_eq!(mem.read_u8(0x0511), 0);
        assert_eq!(mem.read_u8(0x0512), 0);

        // Verify the returned results remain stable across repeated probes.
        cpu.gpr[gpr::RAX] = 0x4B01;
        cpu.gpr[gpr::RBX] = 0xDEAD;
        cpu.gpr[gpr::RCX] = 0xBEEF;
        cpu.rflags |= FLAG_CF;
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
        assert_eq!(mem.read_u32(0x0504), 0);
    }

    #[test]
    fn int13_eltorito_terminate_and_get_status_succeed_for_no_emulation_boot() {
        let mut bios = Bios::new(BiosConfig::default());
        bios.el_torito_boot_info = Some(ElToritoBootInfo {
            media_type: ElToritoBootMediaType::NoEmulation,
            boot_drive: 0xE0,
            controller_index: 0,
            boot_catalog_lba: Some(0x1234_5678),
            boot_image_lba: Some(0x8765_4321),
            load_segment: Some(0x07C0),
            sector_count: Some(8),
        });
        let mut disk = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.a20_enabled = mem.a20_enabled();

        // Terminate disk emulation should succeed as a no-op in no-emulation mode.
        cpu.gpr[gpr::RAX] = 0x4B00; // AH=4Bh AL=00h terminate emulation
        cpu.gpr[gpr::RDX] = 0xE0;
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);

        // Get disk emulation status should fill the ES:DI buffer.
        set_real_mode_seg(&mut cpu.segments.es, 0);
        cpu.gpr[gpr::RDI] = 0x0500;
        mem.write_u8(0x0500, 0x13);
        cpu.gpr[gpr::RAX] = 0x4B01; // get status
        cpu.gpr[gpr::RDX] = 0xE0;
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);

        assert_eq!(mem.read_u8(0x0500), 0x13);
        assert_eq!(mem.read_u8(0x0501), 0x00); // no emulation
        assert_eq!(mem.read_u8(0x0502), 0xE0); // boot drive
        assert_eq!(mem.read_u8(0x0503), 0x00); // controller index
        assert_eq!(mem.read_u32(0x0504), 0x8765_4321);
        assert_eq!(mem.read_u32(0x0508), 0x1234_5678);
        assert_eq!(mem.read_u16(0x050C), 0x07C0);
        assert_eq!(mem.read_u16(0x050E), 8);
        assert_eq!(mem.read_u8(0x0510), 0);
        assert_eq!(mem.read_u8(0x0511), 0);
        assert_eq!(mem.read_u8(0x0512), 0);
    }

    #[test]
    fn int13_eltorito_get_status_reflects_metadata_cached_by_post_cd_boot() {
        const ISO_BLOCK_BYTES: usize = 2048;
        const ISO9660_STANDARD_IDENTIFIER: &[u8; 5] = b"CD001";
        const ISO9660_VERSION: u8 = 1;

        fn write_iso_block(img: &mut [u8], iso_lba: usize, block: &[u8; ISO_BLOCK_BYTES]) {
            let off = iso_lba * ISO_BLOCK_BYTES;
            img[off..off + ISO_BLOCK_BYTES].copy_from_slice(block);
        }

        fn build_el_torito_boot_system_id() -> [u8; 32] {
            let mut out = [b' '; 32];
            out[..b"EL TORITO SPECIFICATION".len()].copy_from_slice(b"EL TORITO SPECIFICATION");
            out
        }

        fn build_minimal_iso_no_emulation(
            boot_catalog_lba: u32,
            boot_image_lba: u32,
            boot_image_blocks: &[[u8; ISO_BLOCK_BYTES]],
            load_segment: u16,
            sector_count: u16,
        ) -> Vec<u8> {
            let total_blocks = (boot_image_lba as usize)
                .saturating_add(boot_image_blocks.len())
                .max(32);
            let mut img = vec![0u8; total_blocks * ISO_BLOCK_BYTES];

            // Primary Volume Descriptor at LBA16 (type 1).
            let mut pvd = [0u8; ISO_BLOCK_BYTES];
            pvd[0] = 0x01;
            pvd[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
            pvd[6] = ISO9660_VERSION;
            write_iso_block(&mut img, 16, &pvd);

            // Boot Record Volume Descriptor at LBA17 (type 0).
            let mut brvd = [0u8; ISO_BLOCK_BYTES];
            brvd[0] = 0x00;
            brvd[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
            brvd[6] = ISO9660_VERSION;
            brvd[7..39].copy_from_slice(&build_el_torito_boot_system_id());
            brvd[0x47..0x4B].copy_from_slice(&boot_catalog_lba.to_le_bytes());
            write_iso_block(&mut img, 17, &brvd);

            // Volume Descriptor Set Terminator at LBA18 (type 255).
            let mut term = [0u8; ISO_BLOCK_BYTES];
            term[0] = 0xFF;
            term[1..6].copy_from_slice(ISO9660_STANDARD_IDENTIFIER);
            term[6] = ISO9660_VERSION;
            write_iso_block(&mut img, 18, &term);

            // Boot Catalog at `boot_catalog_lba`.
            let mut catalog = [0u8; ISO_BLOCK_BYTES];
            let mut validation = [0u8; 32];
            validation[0] = 0x01; // header id
            validation[0x1E] = 0x55;
            validation[0x1F] = 0xAA;
            let mut sum: u16 = 0;
            for chunk in validation.chunks_exact(2) {
                sum = sum.wrapping_add(u16::from_le_bytes([chunk[0], chunk[1]]));
            }
            let checksum = (0u16).wrapping_sub(sum);
            validation[0x1C..0x1E].copy_from_slice(&checksum.to_le_bytes());
            catalog[0..32].copy_from_slice(&validation);

            let mut initial = [0u8; 32];
            initial[0] = 0x88; // bootable
            initial[1] = 0x00; // no emulation
            initial[2..4].copy_from_slice(&load_segment.to_le_bytes());
            initial[6..8].copy_from_slice(&sector_count.to_le_bytes());
            initial[8..12].copy_from_slice(&boot_image_lba.to_le_bytes());
            catalog[32..64].copy_from_slice(&initial);
            write_iso_block(&mut img, boot_catalog_lba as usize, &catalog);

            for (i, block) in boot_image_blocks.iter().enumerate() {
                write_iso_block(&mut img, boot_image_lba as usize + i, block);
            }

            img
        }

        let boot_catalog_lba = 20u32;
        let boot_image_lba = 21u32;
        let load_segment = 0x9000u16;
        let sector_count = 8u16;
        let boot_image_blocks = [[0x11u8; ISO_BLOCK_BYTES], [0x22u8; ISO_BLOCK_BYTES]];
        let img = build_minimal_iso_no_emulation(
            boot_catalog_lba,
            boot_image_lba,
            &boot_image_blocks,
            load_segment,
            sector_count,
        );

        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(16 * 1024 * 1024);
        // Provide a dummy HDD for the BIOS BlockDevice slot and the ISO as a separate 2048-byte
        // sector CD-ROM backend.
        let mut hdd = InMemoryDisk::new(vec![0u8; BIOS_SECTOR_SIZE]);
        let mut cdrom = InMemoryCdrom::new(img);

        bios.post(&mut cpu, &mut mem, &mut hdd, Some(&mut cdrom));

        assert_eq!(
            bios.el_torito_boot_info,
            Some(ElToritoBootInfo {
                media_type: ElToritoBootMediaType::NoEmulation,
                boot_drive: 0xE0,
                controller_index: 0,
                boot_catalog_lba: Some(boot_catalog_lba),
                boot_image_lba: Some(boot_image_lba),
                load_segment: Some(load_segment),
                sector_count: Some(sector_count),
            })
        );

        // Query disk emulation status (AH=4Bh AL=01h).
        set_real_mode_seg(&mut cpu.segments.es, 0);
        cpu.gpr[gpr::RDI] = 0x0500;
        cpu.gpr[gpr::RDX] = 0xE0;
        cpu.gpr[gpr::RAX] = 0x4B01;
        mem.write_u8(0x0500, 0x13);

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut hdd, Some(&mut cdrom));

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);

        assert_eq!(mem.read_u8(0x0500), 0x13);
        assert_eq!(mem.read_u8(0x0501), 0x00); // no emulation
        assert_eq!(mem.read_u8(0x0502), 0xE0); // boot drive
        assert_eq!(mem.read_u8(0x0503), 0x00); // controller index
        assert_eq!(mem.read_u32(0x0504), boot_image_lba);
        assert_eq!(mem.read_u32(0x0508), boot_catalog_lba);
        assert_eq!(mem.read_u16(0x050C), load_segment);
        assert_eq!(mem.read_u16(0x050E), sector_count);
        assert_eq!(mem.read_u8(0x0510), 0);
        assert_eq!(mem.read_u8(0x0511), 0);
        assert_eq!(mem.read_u8(0x0512), 0);

        // Ensure EDD extended reads (AH=42h) can read ISO9660 logical blocks (2048 bytes) from the
        // CD boot drive after POST.
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0520;
        cpu.gpr[gpr::RDX] = 0xE0;
        cpu.gpr[gpr::RAX] = 0x4200;

        // Disk Address Packet (16 bytes) at 0000:0520.
        mem.write_u8(0x0520, 0x10); // size
        mem.write_u8(0x0521, 0);
        mem.write_u16(0x0522, 1); // count (2048-byte sectors)
        mem.write_u16(0x0524, 0x2000); // buffer offset
        mem.write_u16(0x0526, 0x0000); // buffer segment
        mem.write_u64(0x0528, 16); // ISO LBA 16 (primary volume descriptor)

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut hdd, Some(&mut cdrom));

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        let sector = mem.read_bytes(0x2000, 2048);
        assert_eq!(&sector[1..6], b"CD001");
    }

    #[test]
    fn int13_edd30_extension_check_and_drive_params_table_are_consistent() {
        // AH=41h advertises EDD 3.0 (AH=0x30). When the caller supplies a 0x42-byte buffer for
        // AH=48h, BIOS must return a 0x42-byte parameter table (including the 0xBEDD key) or
        // stricter OS probes may treat the BIOS as buggy.
        for drive in [0x80u8, 0xE0u8] {
            let mut bios = Bios::new(BiosConfig {
                boot_drive: drive,
                ..BiosConfig::default()
            });
            // CD-ROM drives use 2048-byte sectors internally, but the BIOS disk backend is defined
            // in terms of 512-byte sectors. Keep the test buffer simple and let INT 13h handle the
            // conversion logic for CD drives.
            let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 8];
            let mut disk = InMemoryDisk::new(disk_bytes);
            let mut cpu = CpuState::new(CpuMode::Real);
            let mut mem = TestMemory::new(2 * 1024 * 1024);
            ivt::init_bda(&mut mem, drive);
            cpu.a20_enabled = mem.a20_enabled();

            cpu.gpr[gpr::RAX] = 0x4100; // AH=41h
            cpu.gpr[gpr::RBX] = 0x55AA;
            cpu.gpr[gpr::RDX] = drive as u64; // DL
            handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

            assert_eq!(cpu.rflags & FLAG_CF, 0);
            assert_eq!(((cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8, 0x30);

            set_real_mode_seg(&mut cpu.segments.ds, 0);
            cpu.gpr[gpr::RSI] = 0x0600;
            cpu.gpr[gpr::RDX] = drive as u64; // DL
            cpu.gpr[gpr::RAX] = 0x4800; // AH=48h

            let table_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0600);
            for i in 0..0x42u64 {
                mem.write_u8(table_addr + i, 0xCC);
            }
            mem.write_u16(table_addr, 0x42); // buffer size

            handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

            assert_eq!(cpu.rflags & FLAG_CF, 0);
            assert_eq!(mem.read_u16(table_addr), 0x42);
            assert_eq!(mem.read_u16(table_addr + 0x1E), 0xBEDD);
            assert_eq!(mem.read_u8(table_addr + 0x20), 0x1E);

            assert_eq!(mem.read_bytes(table_addr + 0x24, 4), b"PCI ".to_vec());

            let expected_iface = if drive >= 0xE0 {
                b"ATAPI   ".to_vec()
            } else {
                b"ATA     ".to_vec()
            };
            assert_eq!(mem.read_bytes(table_addr + 0x28, 8), expected_iface);
            assert_eq!(mem.read_bytes(table_addr + 0x30, 16), vec![0u8; 16]);

            // Verify checksum: sum(host_bus..checksum) must be 0 mod 256.
            let mut sum: u8 = 0;
            for b in mem.read_bytes(table_addr + 0x24, 0x1E) {
                sum = sum.wrapping_add(b);
            }
            assert_eq!(sum, 0);
        }
    }

    #[test]
    fn int13_ext_get_drive_params_rounds_small_buffers_down_to_edd11_size() {
        // EDD defines standard sizes for the drive parameter table:
        // - 0x1A (EDD 1.1)
        // - 0x1E (EDD 2.x)
        // - 0x42 (EDD 3.0)
        //
        // Some callers provide a buffer slightly larger than 0x1A but smaller than 0x1E; BIOSes
        // should round down to the largest supported structure size (0x1A) instead of returning a
        // non-standard, partially-defined size.
        for (drive, expected_sectors, expected_bps) in
            [(0x80u8, 8u64, 512u16), (0xE0u8, 2u64, 2048u16)]
        {
            let mut bios = Bios::new(BiosConfig {
                boot_drive: drive,
                ..BiosConfig::default()
            });
            let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 8];
            let mut disk = InMemoryDisk::new(disk_bytes);

            let mut cpu = CpuState::new(CpuMode::Real);
            set_real_mode_seg(&mut cpu.segments.ds, 0);
            cpu.gpr[gpr::RSI] = 0x0600;
            cpu.gpr[gpr::RDX] = drive as u64; // DL
            cpu.gpr[gpr::RAX] = 0x4800; // AH=48h

            let mut mem = TestMemory::new(2 * 1024 * 1024);
            ivt::init_bda(&mut mem, drive);
            cpu.a20_enabled = mem.a20_enabled();

            let table_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0600);
            for i in 0..0x1Bu64 {
                mem.write_u8(table_addr + i, 0xCC);
            }
            mem.write_u16(table_addr, 0x1B); // buffer size (non-standard)

            handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

            assert_eq!(cpu.rflags & FLAG_CF, 0);
            assert_eq!(mem.read_u16(table_addr), 0x1A);
            assert_eq!(mem.read_u64(table_addr + 16), expected_sectors);
            assert_eq!(mem.read_u16(table_addr + 24), expected_bps);
            // Ensure we didn't scribble past the returned size (0x1A).
            assert_eq!(mem.read_u8(table_addr + 0x1A), 0xCC);
        }
    }

    #[test]
    fn int13_chs_read_floppy_maps_head1_sector1_to_lba18() {
        // 1.44MiB floppy = 2880 sectors. Cylinder 0, head 1, sector 1 corresponds to LBA 18.
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 2880];
        disk_bytes[18 * BIOS_SECTOR_SIZE] = 0xCC;
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.es, 0);
        cpu.gpr[gpr::RBX] = 0x1000;
        cpu.gpr[gpr::RAX] = 0x0201; // AH=02h read, AL=1 sector
        cpu.gpr[gpr::RCX] = 0x0001; // CH=0, CL=1 (sector 1)
        cpu.gpr[gpr::RDX] = 0x0100; // DH=1 (head), DL=0 (floppy 0)

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x00);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
        assert_eq!(mem.read_u8(0x1000), 0xCC);
    }

    #[test]
    fn int13_get_drive_parameters_floppy_reports_1440k_geometry() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 2880];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x0800; // AH=08h
        cpu.gpr[gpr::RDX] = 0x0000; // DL=floppy 0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x00);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        // CH=79 (cylinders-1), CL=18 (sectors per track).
        assert_eq!(cpu.gpr[gpr::RCX] as u16, 0x4F12);
        // DH=heads-1=1, DL=drive count=1
        assert_eq!(cpu.gpr[gpr::RDX] as u16, 0x0101);
        assert_eq!(cpu.segments.es.selector, super::super::BIOS_SEGMENT);
        assert_eq!(
            cpu.gpr[gpr::RDI] as u16,
            super::super::DISKETTE_PARAM_TABLE_OFFSET
        );
        assert_eq!(cpu.gpr[gpr::RBX] as u8, 0x04);
    }

    #[test]
    fn int13_get_drive_parameters_fixed_disk_reports_geometry_and_param_table_pointer() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 8];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x0800; // AH=08h
        cpu.gpr[gpr::RDX] = 0x0080; // DL=HDD0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RCX] as u16, 0xFFFF);
        assert_eq!(cpu.gpr[gpr::RDX] as u16, 0x0F01);
        assert_eq!(cpu.segments.es.selector, super::super::BIOS_SEGMENT);
        assert_eq!(
            cpu.gpr[gpr::RDI] as u16,
            super::super::FIXED_DISK_PARAM_TABLE_OFFSET
        );
        assert_eq!(cpu.gpr[gpr::RBX] as u8, 0);
    }

    #[test]
    fn int13_get_disk_type_floppy_reports_present() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 2880];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x1500; // AH=15h
        cpu.gpr[gpr::RDX] = 0x0000; // DL=floppy 0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x00);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0x02);
    }

    #[test]
    fn int13_get_disk_type_cd_reports_present_and_2048_sector_count() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        // 10 logical CD sectors (2048 bytes each) => 40 512-byte sectors.
        let disk_bytes = vec![0u8; 2048 * 10];
        let mut disk = InMemoryDisk::new(disk_bytes);
        let expected_2048_sectors = u32::try_from(disk.size_in_sectors() / 4).unwrap();

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x1500; // AH=15h
        cpu.gpr[gpr::RDX] = 0x00E0; // DL=CD0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0xE0);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0x03);
        let sectors = ((cpu.gpr[gpr::RCX] as u16 as u32) << 16) | (cpu.gpr[gpr::RDX] as u16 as u32);
        assert_eq!(sectors, expected_2048_sectors);
    }

    #[test]
    fn int13_get_drive_parameters_cd_returns_failure() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let disk_bytes = vec![0u8; 2048 * 10];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x0800; // AH=08h
        cpu.gpr[gpr::RDX] = 0x00E0; // DL=CD0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0xE0);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x0100);
    }

    #[test]
    fn int13_verify_sectors_chs_reads_without_writing_memory() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 2880];
        disk_bytes[18 * BIOS_SECTOR_SIZE] = 0xCC;
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.es, 0);
        cpu.gpr[gpr::RBX] = 0x1000;
        cpu.gpr[gpr::RAX] = 0x0401; // AH=04h verify, AL=1 sector
        cpu.gpr[gpr::RCX] = 0x0001; // CH=0, CL=1
        cpu.gpr[gpr::RDX] = 0x0100; // DH=1, DL=0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x00);
        // Pre-fill memory so we can detect unexpected writes.
        mem.write_u8(0x1000, 0xAA);

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
        assert_eq!(mem.read_u8(0x1000), 0xAA);
    }

    #[test]
    fn int13_chs_write_reports_write_protected() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 4];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.es, 0);
        cpu.gpr[gpr::RBX] = 0x1000;
        cpu.gpr[gpr::RAX] = 0x0301; // AH=03h write, AL=1 sector
        cpu.gpr[gpr::RCX] = 0x0001; // CH=0, CL=1
        cpu.gpr[gpr::RDX] = 0x0080; // DH=0, DL=0x80 (HDD0)

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);
        mem.write_u8(0x1000, 0xCC);

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x0300);
    }

    #[test]
    fn int13_ext_write_reports_write_protected() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 4];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0500;
        cpu.gpr[gpr::RDX] = 0x80; // DL = HDD0
        cpu.gpr[gpr::RAX] = 0x4300; // AH=43h extended write

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);
        cpu.a20_enabled = mem.a20_enabled();
        let dap_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0500);
        mem.write_u8(dap_addr, 0x10);
        mem.write_u8(dap_addr + 1, 0x00);
        mem.write_u16(dap_addr + 2, 1); // count
        mem.write_u16(dap_addr + 4, 0x1000); // offset
        mem.write_u16(dap_addr + 6, 0x0000); // segment
        mem.write_u64(dap_addr + 8, 0); // LBA

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x0300);
    }

    #[test]
    fn int13_format_track_reports_write_protected() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 2880];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x0500; // AH=05h format track
        cpu.gpr[gpr::RCX] = 0x0001; // CH=0, CL=1
        cpu.gpr[gpr::RDX] = 0x0000; // DH=0, DL=floppy 0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x00);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x0300);
    }

    #[test]
    fn int13_seek_reports_success_for_valid_chs() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 2880];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x0C00; // AH=0Ch seek
        cpu.gpr[gpr::RCX] = 0x0000; // CH=0, CL=0 (cylinder 0)
        cpu.gpr[gpr::RDX] = 0x0100; // DH=1, DL=floppy 0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x00);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
    }

    #[test]
    fn int13_seek_reports_error_for_invalid_cylinder() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 2880];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x0C00; // AH=0Ch seek
                                    // Cylinder 80 (out of range for 1.44MiB floppy: cylinders are 0..=79).
        cpu.gpr[gpr::RCX] = 0x5000; // CH=0x50, CL=0
        cpu.gpr[gpr::RDX] = 0x0000; // DH=0, DL=floppy 0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x00);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0x01);
    }

    #[test]
    fn int13_check_drive_ready_reports_success() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x1000; // AH=10h
        cpu.gpr[gpr::RDX] = 0x0000; // DL=0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x00);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
    }

    #[test]
    fn int13_initialize_drive_parameters_reports_success() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 2880];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x0900; // AH=09h initialize drive parameters
        cpu.gpr[gpr::RDX] = 0x0000; // DL=floppy 0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x00);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
    }

    #[test]
    fn int13_controller_diagnostics_reports_success() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 2880];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x1400; // AH=14h controller diagnostic
        cpu.gpr[gpr::RDX] = 0x0000; // DL=floppy 0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x00);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
    }

    #[test]
    fn int13_recalibrate_reports_success() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 2880];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x1100; // AH=11h recalibrate
        cpu.gpr[gpr::RDX] = 0x0000; // DL=floppy 0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x00);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
    }

    #[test]
    fn int13_alternate_reset_reports_success() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x0D00; // AH=0Dh alternate reset
        cpu.gpr[gpr::RDX] = 0x0080; // DL=HDD0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
    }

    #[test]
    fn int13_get_disk_change_status_reports_not_changed() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE * 2880];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RAX] = 0x1600; // AH=16h
        cpu.gpr[gpr::RDX] = 0x0000; // DL=floppy 0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x00);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
    }

    #[test]
    fn int13_reset_and_get_status_fail_for_nonexistent_drive() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        cpu.gpr[gpr::RDX] = 0x0000; // DL=floppy 0

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        // Booting from a hard disk -> do not advertise any floppy drives in the BDA.
        ivt::init_bda(&mut mem, 0x80);

        cpu.gpr[gpr::RAX] = 0x0000; // AH=00h reset
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);
        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0x01);

        cpu.gpr[gpr::RAX] = 0x0100; // AH=01h get status
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);
        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0x01);
    }

    #[test]
    fn int13_read_sectors_on_nonexistent_drive_reports_zero_sectors_transferred() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.es, 0);
        cpu.gpr[gpr::RBX] = 0x1000;
        cpu.gpr[gpr::RAX] = 0x0201; // AH=02h read, AL=1 sector
        cpu.gpr[gpr::RCX] = 0x0001; // CH=0, CL=1
        cpu.gpr[gpr::RDX] = 0x0000; // DH=0, DL=floppy 0 (but no floppy advertised)

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x0100);
    }

    #[test]
    fn int15_a20_services_toggle_bus_masking() {
        let mut bios = Bios::new(BiosConfig {
            memory_size_bytes: 2 * 1024 * 1024,
            boot_drive: 0x80,
            ..BiosConfig::default()
        });
        let mut bus = TestMemory::new(2 * 1024 * 1024);
        let mut cpu = CpuState::new(CpuMode::Real);

        // Enable A20 and verify 1MiB is distinct.
        cpu.gpr[gpr::RAX] = 0x2401;
        handle_int15(&mut bios, &mut cpu, &mut bus);
        assert_eq!(cpu.rflags & FLAG_CF, 0, "CF should be cleared");
        bus.write_u8(0x0, 0x11);
        bus.write_u8(0x1_00000, 0x22);
        assert_eq!(bus.read_u8(0x0), 0x11);
        assert_eq!(bus.read_u8(0x1_00000), 0x22);

        // Disable A20 and verify wraparound.
        cpu.gpr[gpr::RAX] = 0x2400;
        handle_int15(&mut bios, &mut cpu, &mut bus);
        assert_eq!(bus.read_u8(0x1_00000), 0x11);
    }

    #[test]
    fn int15_a20_query_reports_state() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut bus = TestMemory::new(2 * 1024 * 1024);
        let mut cpu = CpuState::new(CpuMode::Real);

        cpu.gpr[gpr::RAX] = 0x2402;
        handle_int15(&mut bios, &mut cpu, &mut bus);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x0000);

        cpu.gpr[gpr::RAX] = 0x2401;
        handle_int15(&mut bios, &mut cpu, &mut bus);
        cpu.gpr[gpr::RAX] = 0x2402;
        handle_int15(&mut bios, &mut cpu, &mut bus);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x0001);
    }

    #[test]
    fn int15_e801_returns_expected_values() {
        struct Case {
            mem: u64,
        }

        let cases = [
            Case {
                mem: 512 * 1024 * 1024,
            },
            Case {
                mem: 2 * 1024 * 1024 * 1024,
            },
            Case {
                mem: 4 * 1024 * 1024 * 1024,
                // With a 256MiB PCIe ECAM window at 0xB0000000 and a 1GiB PCI/MMIO
                // hole (0xC0000000..4GiB), only 0xB0000000 bytes of RAM exist below
                // 4GiB. The remainder is remapped above 4GiB and does not count
                // toward INT 15h E801's BX value.
            },
        ];

        for case in cases {
            // E801 AX reports KB in the 1MiB..16MiB window (capped at 15MiB = 0x3C00 KB).
            let expected_ax: u16 = 0x3C00;
            // E801 BX reports 64KiB blocks in the 16MiB..4GiB window.
            let expected_bx: u16 = if case.mem <= PCIE_ECAM_BASE {
                // No ECAM hole in low RAM yet.
                ((case.mem - 0x0100_0000) / 65536) as u16
            } else {
                // Low RAM stops at the ECAM base; anything above is remapped above 4GiB.
                ((PCIE_ECAM_BASE - 0x0100_0000) / 65536) as u16
            };
            let mut bios = Bios::new(BiosConfig {
                memory_size_bytes: case.mem,
                boot_drive: 0x80,
                ..BiosConfig::default()
            });
            let mut bus = TestMemory::new(2 * 1024 * 1024);
            let mut cpu = CpuState::new(CpuMode::Real);
            cpu.gpr[gpr::RAX] = 0xE801;
            handle_int15(&mut bios, &mut cpu, &mut bus);

            assert_eq!(cpu.rflags & FLAG_CF, 0);
            assert_eq!(cpu.gpr[gpr::RAX] as u16, expected_ax);
            assert_eq!(cpu.gpr[gpr::RBX] as u16, expected_bx);
            assert_eq!(cpu.gpr[gpr::RCX] as u16, expected_ax);
            assert_eq!(cpu.gpr[gpr::RDX] as u16, expected_bx);
        }
    }

    #[test]
    fn int15_wait_returns_success() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut bus = TestMemory::new(2 * 1024 * 1024);
        let mut cpu = CpuState::new(CpuMode::Real);

        cpu.gpr[gpr::RAX] = 0x8600; // AH=86h Wait
        cpu.gpr[gpr::RCX] = 0x0001;
        cpu.gpr[gpr::RDX] = 0x0002;
        handle_int15(&mut bios, &mut cpu, &mut bus);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
    }

    #[test]
    fn int15_get_system_configuration_parameters_returns_table_in_ebda() {
        let mut bios = Bios::new(BiosConfig::default());
        let mut bus = TestMemory::new(2 * 1024 * 1024);
        let mut cpu = CpuState::new(CpuMode::Real);

        cpu.gpr[gpr::RAX] = 0xC000; // AH=C0h
        handle_int15(&mut bios, &mut cpu, &mut bus);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);
        assert_eq!(cpu.segments.es.selector, (EBDA_BASE / 16) as u16);
        assert_eq!(cpu.gpr[gpr::RBX] as u16, 0x0020);

        let table_addr = EBDA_BASE + 0x20;
        assert_eq!(bus.read_u16(table_addr), 0x0010);
    }

    #[test]
    fn int13_get_status_reports_last_error() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; BIOS_SECTOR_SIZE];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0700;
        cpu.gpr[gpr::RDX] = 0x80; // DL = HDD0
        cpu.gpr[gpr::RAX] = 0x4200; // AH=42h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);
        cpu.a20_enabled = mem.a20_enabled();
        let dap_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0700);
        mem.write_u8(dap_addr, 0x10);
        mem.write_u8(dap_addr + 1, 0x00);
        mem.write_u16(dap_addr + 2, 1); // count
        mem.write_u16(dap_addr + 4, 0x1000); // offset
        mem.write_u16(dap_addr + 6, 0x0000); // segment
        mem.write_u64(dap_addr + 8, 1); // LBA (out of range)

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);
        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0x04);

        cpu.gpr[gpr::RAX] = 0x0100; // AH=01h
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk, None);
        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0x04);
    }

    #[test]
    fn int11_reports_bda_equipment_word() {
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(2 * 1024 * 1024);
        mem.write_u16(BDA_BASE + 0x10, 0xABCD);

        handle_int11(&mut cpu, &mut mem);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0xABCD);
    }

    #[test]
    fn int12_reports_conventional_memory_kb() {
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);

        handle_int12(&mut cpu, &mut mem);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, (EBDA_BASE / 1024) as u16);
    }

    #[test]
    fn int1a_pci_bios_probes_report_function_not_supported() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(2 * 1024 * 1024);

        // PCI BIOS presence check uses AX=B101h.
        cpu.gpr[gpr::RAX] = 0xB101;
        handle_int1a(&mut bios, &mut cpu, &mut mem);

        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) as u8, 0x81);
    }

    #[test]
    fn int14_status_reports_timeout_for_missing_port_and_ready_for_com1() {
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);

        // COM1 present.
        cpu.gpr[gpr::RAX] = 0x0300; // AH=03h status
        cpu.gpr[gpr::RDX] = 0x0000; // DX=0 -> COM1
        handle_int14(&mut cpu, &mut mem);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) as u8, 0x60);

        // COM2 missing.
        cpu.gpr[gpr::RAX] = 0x0300;
        cpu.gpr[gpr::RDX] = 0x0001; // DX=1 -> COM2
        handle_int14(&mut cpu, &mut mem);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) as u8, 0x80);
    }

    #[test]
    fn int14_receive_reports_timeout_when_no_data_available() {
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);

        cpu.gpr[gpr::RAX] = 0x0200; // AH=02h receive
        cpu.gpr[gpr::RDX] = 0x0000; // COM1
        handle_int14(&mut cpu, &mut mem);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) as u8, 0x80);
        assert_eq!(cpu.gpr[gpr::RAX] as u8, 0);
    }

    #[test]
    fn int17_status_reports_timeout_when_lpt_absent_and_ready_when_present() {
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);

        cpu.gpr[gpr::RAX] = 0x0200; // AH=02h status
        cpu.gpr[gpr::RDX] = 0x0000; // LPT1
        handle_int17(&mut cpu, &mut mem);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) as u8, 0x01);

        // Advertise LPT1 in BDA.
        mem.write_u16(BDA_BASE + 0x08, 0x0378);
        cpu.gpr[gpr::RAX] = 0x0200;
        cpu.gpr[gpr::RDX] = 0x0000;
        handle_int17(&mut cpu, &mut mem);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) as u8, 0x90);
    }

    #[test]
    fn keyboard_queue_is_mirrored_into_bda_ring_buffer() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        bios.push_key(0x1234);
        bios.push_key(0x5678);

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);

        sync_keyboard_bda(&bios, &mut mem);

        assert_eq!(keyboard_bda_head(&mut mem), BDA_KEYBOARD_BUF_START);
        assert_eq!(
            keyboard_bda_tail(&mut mem),
            BDA_KEYBOARD_BUF_START.wrapping_add(4)
        );
        assert_eq!(
            mem.read_u16(BDA_BASE + u64::from(BDA_KEYBOARD_BUF_START)),
            0x1234
        );
        assert_eq!(
            mem.read_u16(BDA_BASE + u64::from(BDA_KEYBOARD_BUF_START.wrapping_add(2))),
            0x5678
        );
    }

    #[test]
    fn int19_loads_boot_sector_and_installs_iret_frame_to_jump_to_7c00() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0x80,
            ..BiosConfig::default()
        });
        let mut cpu = CpuState::new(CpuMode::Real);

        let mut sector = [0u8; BIOS_SECTOR_SIZE];
        sector[0] = 0xAA;
        sector[1] = 0xBB;
        sector[510] = 0x55;
        sector[511] = 0xAA;
        let mut disk = InMemoryDisk::from_boot_sector(sector);
        let mut mem = TestMemory::new(2 * 1024 * 1024);

        handle_int19(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert!(!cpu.halted);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RDX] as u8, 0x80);
        assert_eq!(cpu.segments.ss.selector, 0x0000);
        assert_eq!(cpu.gpr[gpr::RSP] as u16, 0x7BFA);

        let loaded = mem.read_bytes(0x7C00, BIOS_SECTOR_SIZE);
        assert_eq!(loaded[0], 0xAA);
        assert_eq!(loaded[1], 0xBB);
        assert_eq!(loaded[510], 0x55);
        assert_eq!(loaded[511], 0xAA);

        // Verify the synthetic IRET frame at 0000:7BFA.
        assert_eq!(mem.read_u16(0x7BFA), 0x7C00); // IP
        assert_eq!(mem.read_u16(0x7BFC), 0x0000); // CS
        assert_eq!(mem.read_u16(0x7BFE), 0x0202); // FLAGS
    }

    fn write_iso_block(img: &mut [u8], iso_lba: usize, block: &[u8; 2048]) {
        let off = iso_lba * 2048;
        img[off..off + 2048].copy_from_slice(block);
    }

    fn build_minimal_iso_no_emulation(
        boot_catalog_lba: u32,
        boot_image_lba: u32,
        boot_image_bytes: &[u8; 2048],
        load_segment: u16,
        sector_count: u16,
    ) -> Vec<u8> {
        // Allocate enough blocks for the volume descriptors + boot catalog + boot image.
        let total_blocks = (boot_image_lba as usize).saturating_add(1).max(32);
        let mut img = vec![0u8; total_blocks * 2048];

        // Primary Volume Descriptor at LBA16 (type 1).
        let mut pvd = [0u8; 2048];
        pvd[0] = 0x01;
        pvd[1..6].copy_from_slice(b"CD001");
        pvd[6] = 0x01;
        write_iso_block(&mut img, 16, &pvd);

        // Boot Record Volume Descriptor at LBA17 (type 0).
        let mut brvd = [0u8; 2048];
        brvd[0] = 0x00;
        brvd[1..6].copy_from_slice(b"CD001");
        brvd[6] = 0x01;
        // Space-padded El Torito boot system id.
        brvd[7..39].fill(b' ');
        let id = b"EL TORITO SPECIFICATION";
        brvd[7..7 + id.len()].copy_from_slice(id);
        brvd[0x47..0x4B].copy_from_slice(&boot_catalog_lba.to_le_bytes());
        write_iso_block(&mut img, 17, &brvd);

        // Volume Descriptor Set Terminator at LBA18 (type 255).
        let mut term = [0u8; 2048];
        term[0] = 0xFF;
        term[1..6].copy_from_slice(b"CD001");
        term[6] = 0x01;
        write_iso_block(&mut img, 18, &term);

        // Boot Catalog at `boot_catalog_lba`.
        let mut catalog = [0u8; 2048];
        let mut validation = [0u8; 32];
        validation[0] = 0x01; // header id
        validation[1] = 0x00; // platform id (x86)
        validation[30] = 0x55;
        validation[31] = 0xAA;
        let mut sum: u16 = 0;
        for chunk in validation.chunks_exact(2) {
            sum = sum.wrapping_add(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        let checksum = (0u16).wrapping_sub(sum);
        validation[28..30].copy_from_slice(&checksum.to_le_bytes());
        catalog[0..32].copy_from_slice(&validation);

        let mut initial = [0u8; 32];
        initial[0] = 0x88; // bootable
        initial[1] = 0x00; // no emulation
        initial[2..4].copy_from_slice(&load_segment.to_le_bytes());
        initial[6..8].copy_from_slice(&sector_count.to_le_bytes());
        initial[8..12].copy_from_slice(&boot_image_lba.to_le_bytes());
        catalog[32..64].copy_from_slice(&initial);

        write_iso_block(&mut img, boot_catalog_lba as usize, &catalog);
        write_iso_block(&mut img, boot_image_lba as usize, boot_image_bytes);

        img
    }

    #[test]
    fn int19_loads_eltorito_boot_image_and_installs_iret_frame_to_jump_to_boot_segment() {
        const BOOT_CATALOG_LBA: u32 = 20;
        const BOOT_IMAGE_LBA: u32 = 21;
        const LOAD_SEGMENT: u16 = 0x2000;

        let mut boot_image = [0u8; 2048];
        boot_image[..8].copy_from_slice(b"INT19CD!");
        boot_image[510] = 0x55;
        boot_image[511] = 0xAA;

        let iso = build_minimal_iso_no_emulation(
            BOOT_CATALOG_LBA,
            BOOT_IMAGE_LBA,
            &boot_image,
            LOAD_SEGMENT,
            4,
        );
        let mut disk = InMemoryDisk::new(iso);

        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0xE0,
            ..BiosConfig::default()
        });
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(2 * 1024 * 1024);

        handle_int19(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert!(!cpu.halted);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RDX] as u8, 0xE0);
        assert_eq!(cpu.segments.ds.selector, 0x0000);
        assert_eq!(cpu.segments.es.selector, 0x0000);
        assert_eq!(cpu.segments.ss.selector, 0x0000);
        assert_eq!(cpu.gpr[gpr::RSP] as u16, 0x7BFA);

        // Verify the synthetic IRET frame at 0000:7BFA.
        assert_eq!(mem.read_u16(0x7BFA), 0x0000); // IP
        assert_eq!(mem.read_u16(0x7BFC), LOAD_SEGMENT); // CS
        assert_eq!(mem.read_u16(0x7BFE), 0x0202); // FLAGS

        // Verify the boot image was loaded to the catalog-specified segment.
        let load_addr = (u64::from(LOAD_SEGMENT)) << 4;
        let loaded = mem.read_bytes(load_addr, 2048);
        assert_eq!(&loaded[..8], b"INT19CD!");

        // Ensure El Torito boot metadata is available for INT 13h AH=4Bh queries.
        assert_eq!(
            bios.el_torito_boot_info,
            Some(ElToritoBootInfo {
                media_type: ElToritoBootMediaType::NoEmulation,
                boot_drive: 0xE0,
                controller_index: 0,
                boot_catalog_lba: Some(BOOT_CATALOG_LBA),
                boot_image_lba: Some(BOOT_IMAGE_LBA),
                load_segment: Some(LOAD_SEGMENT),
                sector_count: Some(4),
            })
        );
    }

    #[test]
    fn int18_chains_to_int19_bootstrap_loader() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0x80,
            ..BiosConfig::default()
        });
        let mut cpu = CpuState::new(CpuMode::Real);

        let mut sector = [0u8; BIOS_SECTOR_SIZE];
        sector[0] = 0xAA;
        sector[510] = 0x55;
        sector[511] = 0xAA;
        let mut disk = InMemoryDisk::from_boot_sector(sector);
        let mut mem = TestMemory::new(2 * 1024 * 1024);

        handle_int18(&mut bios, &mut cpu, &mut mem, &mut disk, None);

        assert!(!cpu.halted);
        assert_eq!(cpu.gpr[gpr::RSP] as u16, 0x7BFA);
        assert_eq!(mem.read_u8(0x7C00), 0xAA);
    }

    #[test]
    fn int16_get_shift_flags_reports_zero() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut cpu = CpuState::new(CpuMode::Real);

        cpu.gpr[gpr::RAX] = 0x0200; // AH=02h
        handle_int16(&mut bios, &mut cpu);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] & 0xFF) as u8, 0);
    }

    #[test]
    fn int16_get_extended_shift_flags_reports_zero() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut cpu = CpuState::new(CpuMode::Real);

        cpu.gpr[gpr::RAX] = 0x1200; // AH=12h
        handle_int16(&mut bios, &mut cpu);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0);
    }

    #[test]
    fn int16_extended_read_and_check_use_keyboard_queue() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut cpu = CpuState::new(CpuMode::Real);
        let mut mem = TestMemory::new(2 * 1024 * 1024);
        ivt::init_bda(&mut mem, 0x80);

        bios.push_key(0x1234);
        sync_keyboard_bda(&bios, &mut mem);

        // AH=11h (check for extended keystroke) should not dequeue.
        cpu.gpr[gpr::RAX] = 0x1100;
        handle_int16(&mut bios, &mut cpu);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.rflags & FLAG_ZF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x1234);
        // BDA should still show one key pending.
        sync_keyboard_bda(&bios, &mut mem);
        assert_eq!(keyboard_bda_head(&mut mem), BDA_KEYBOARD_BUF_START);
        assert_eq!(
            keyboard_bda_tail(&mut mem),
            BDA_KEYBOARD_BUF_START.wrapping_add(2)
        );

        // AH=01h should still see the same key (queue not drained).
        cpu.gpr[gpr::RAX] = 0x0100;
        handle_int16(&mut bios, &mut cpu);
        assert_eq!(cpu.rflags & FLAG_ZF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x1234);

        // AH=10h (read extended keystroke) should dequeue.
        cpu.gpr[gpr::RAX] = 0x1000;
        handle_int16(&mut bios, &mut cpu);
        assert_eq!(cpu.rflags & FLAG_ZF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x1234);
        sync_keyboard_bda(&bios, &mut mem);
        assert_eq!(keyboard_bda_head(&mut mem), BDA_KEYBOARD_BUF_START);
        assert_eq!(keyboard_bda_tail(&mut mem), BDA_KEYBOARD_BUF_START);

        // Now the queue should be empty.
        cpu.gpr[gpr::RAX] = 0x1100;
        handle_int16(&mut bios, &mut cpu);
        assert_ne!(cpu.rflags & FLAG_ZF, 0);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
    }

    #[test]
    fn int16_store_keystroke_appends_to_keyboard_queue() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut cpu = CpuState::new(CpuMode::Real);

        bios.push_key(0x1111);

        // AH=05h stores key from CX without consuming the existing queue head.
        cpu.gpr[gpr::RAX] = 0x0500;
        cpu.gpr[gpr::RCX] = 0x2222;
        handle_int16(&mut bios, &mut cpu);
        assert_eq!(cpu.rflags & FLAG_CF, 0);

        // First key should still be at the front.
        cpu.gpr[gpr::RAX] = 0x0100;
        handle_int16(&mut bios, &mut cpu);
        assert_eq!(cpu.rflags & FLAG_ZF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x1111);

        // Read consumes the first key.
        cpu.gpr[gpr::RAX] = 0x0000;
        handle_int16(&mut bios, &mut cpu);
        assert_eq!(cpu.rflags & FLAG_ZF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x1111);

        // Now we should observe the stored key.
        cpu.gpr[gpr::RAX] = 0x0000;
        handle_int16(&mut bios, &mut cpu);
        assert_eq!(cpu.rflags & FLAG_ZF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x2222);
    }

    #[test]
    fn int16_set_typematic_rate_delay_reports_success() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut cpu = CpuState::new(CpuMode::Real);

        cpu.gpr[gpr::RAX] = 0x031F; // AH=03h, AL=typematic value
        handle_int16(&mut bios, &mut cpu);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
    }

    #[test]
    fn int16_set_keyboard_click_reports_success() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut cpu = CpuState::new(CpuMode::Real);

        cpu.gpr[gpr::RAX] = 0x0401; // AH=04h, AL=enable click
        handle_int16(&mut bios, &mut cpu);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
    }

    #[test]
    fn int16_flush_buffer_clears_pending_keystrokes() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut cpu = CpuState::new(CpuMode::Real);

        bios.push_key(0x1111);
        bios.push_key(0x2222);

        // AH=0Ch, AL=01h: flush then check for keystroke.
        cpu.gpr[gpr::RAX] = 0x0C01;
        handle_int16(&mut bios, &mut cpu);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_ne!(cpu.rflags & FLAG_ZF, 0);

        // The queue should be empty.
        cpu.gpr[gpr::RAX] = 0x0100;
        handle_int16(&mut bios, &mut cpu);
        assert_ne!(cpu.rflags & FLAG_ZF, 0);
    }

    #[test]
    fn e820_reserves_pcie_ecam_window() {
        const FOUR_GIB: u64 = 4 * 1024 * 1024 * 1024;
        let map = build_e820_map(FOUR_GIB, None, None);

        assert!(
            map.iter().any(|e| {
                e.base == PCIE_ECAM_BASE
                    && e.length == PCIE_ECAM_SIZE
                    && e.region_type == E820_RESERVED
            }),
            "E820 should reserve the PCIe ECAM window at 0x{PCIE_ECAM_BASE:x}..0x{:x}",
            PCIE_ECAM_BASE + PCIE_ECAM_SIZE
        );

        let expected_high_len = FOUR_GIB - PCIE_ECAM_BASE;
        assert!(
            map.iter().any(|e| {
                e.base == 0x1_0000_0000
                    && e.length == expected_high_len
                    && e.region_type == E820_RAM
            }),
            "E820 should remap RAM above 4GiB to preserve the configured memory size"
        );

        for entry in &map {
            if entry.region_type != E820_RAM || entry.length == 0 {
                continue;
            }
            let entry_end = entry.base.saturating_add(entry.length);
            let overlap_start = entry.base.max(PCIE_ECAM_BASE);
            let overlap_end = entry_end.min(PCIE_ECAM_BASE.saturating_add(PCIE_ECAM_SIZE));
            assert!(
                overlap_end <= overlap_start,
                "RAM entry overlaps ECAM window: {entry:?}"
            );
        }
    }

    fn assert_e820_sorted_and_non_overlapping(map: &[E820Entry]) {
        let mut last_end = 0u64;
        for entry in map {
            assert!(
                entry.base >= last_end,
                "E820 entries overlap or are out of order: last_end=0x{last_end:x}, entry={entry:?}"
            );
            last_end = entry.base.saturating_add(entry.length);
        }
    }

    fn assert_no_e820_entry_overlaps_range(map: &[E820Entry], start: u64, end: u64) {
        for entry in map {
            let entry_end = entry.base.saturating_add(entry.length);
            let overlap_start = entry.base.max(start);
            let overlap_end = entry_end.min(end);
            assert!(
                overlap_end <= overlap_start,
                "E820 entry overlaps reserved window 0x{start:x}..0x{end:x}: {entry:?}"
            );
        }
    }

    fn sum_e820_ram(map: &[E820Entry]) -> u64 {
        map.iter()
            .filter(|e| e.region_type == E820_RAM)
            .map(|e| e.length)
            .sum()
    }

    #[test]
    fn e820_ram_below_ecam_does_not_claim_pci_windows() {
        const TOTAL_MEMORY: u64 = 512 * 1024 * 1024;
        const ONE_MIB: u64 = 0x0010_0000;
        const ECAM_BASE: u64 = 0xB000_0000;
        const ECAM_SIZE: u64 = 0x1000_0000;
        const PCI_HOLE_START: u64 = 0xC000_0000;
        const PCI_HOLE_END: u64 = 0x1_0000_0000;

        // Lock down the expected ECAM window. `cargo test -p firmware` does not run dependency
        // tests, so validate the re-exported constants here as well.
        assert_eq!(PCIE_ECAM_BASE, ECAM_BASE);
        assert_eq!(PCIE_ECAM_SIZE, ECAM_SIZE);

        let map = build_e820_map(TOTAL_MEMORY, None, None);
        assert_e820_sorted_and_non_overlapping(&map);

        // When RAM ends below the PCIe ECAM window, the E820 map should not mention the ECAM or
        // PCI/MMIO hole ranges at all.
        assert_no_e820_entry_overlaps_range(&map, ECAM_BASE, ECAM_BASE + ECAM_SIZE);
        assert_no_e820_entry_overlaps_range(&map, PCI_HOLE_START, PCI_HOLE_END);

        // Ensure the amount of RAM described by E820 matches the configured guest memory, minus
        // the legacy VGA + EBDA reserved region within the first MiB.
        let expected_ram = TOTAL_MEMORY - (ONE_MIB - EBDA_BASE);
        let ram_reported = sum_e820_ram(&map);
        assert_eq!(
            ram_reported, expected_ram,
            "Unexpected total RAM reported by E820: expected=0x{expected_ram:x}, got=0x{ram_reported:x}, map={map:?}"
        );
    }

    #[test]
    fn e820_ram_above_ecam_reserves_pci_windows_and_remaps_high_memory() {
        const ONE_MIB: u64 = 0x0010_0000;
        const ECAM_BASE: u64 = 0xB000_0000;
        const ECAM_SIZE: u64 = 0x1000_0000;
        const PCI_HOLE_START: u64 = 0xC000_0000;
        const PCI_HOLE_END: u64 = 0x1_0000_0000;

        assert_eq!(PCIE_ECAM_BASE, ECAM_BASE);
        assert_eq!(PCIE_ECAM_SIZE, ECAM_SIZE);

        // Guest RAM extends past the low-RAM end (ECAM base), so the portion that would overlap
        // the ECAM window must be remapped above 4GiB.
        let total_memory = ECAM_BASE + 256 * 1024 * 1024;
        assert_eq!(total_memory, PCI_HOLE_START);

        let map = build_e820_map(total_memory, None, None);
        assert_e820_sorted_and_non_overlapping(&map);

        // The ECAM window must be reserved exactly.
        assert!(
            map.iter().any(|e| {
                e.base == ECAM_BASE && e.length == ECAM_SIZE && e.region_type == E820_RESERVED
            }),
            "E820 should reserve ECAM window 0x{ECAM_BASE:x}..0x{:x}, map={map:?}",
            ECAM_BASE + ECAM_SIZE
        );

        // The remaining PCI/MMIO hole (below 4GiB) must also be reserved exactly.
        assert!(
            map.iter().any(|e| {
                e.base == PCI_HOLE_START
                    && e.length == PCI_HOLE_END - PCI_HOLE_START
                    && e.region_type == E820_RESERVED
            }),
            "E820 should reserve PCI/MMIO hole 0x{PCI_HOLE_START:x}..0x{PCI_HOLE_END:x}, map={map:?}"
        );

        // RAM above the ECAM base is remapped to start at 4GiB.
        let expected_high_len = total_memory - ECAM_BASE;
        assert!(
            map.iter().any(|e| {
                e.base == PCI_HOLE_END && e.length == expected_high_len && e.region_type == E820_RAM
            }),
            "E820 should expose remapped high RAM at 0x{PCI_HOLE_END:x} length 0x{expected_high_len:x}, map={map:?}"
        );

        // Ensure no RAM entry overlaps the ECAM or PCI hole ranges.
        for entry in map.iter().filter(|e| e.region_type == E820_RAM) {
            let entry_end = entry.base.saturating_add(entry.length);
            assert!(
                entry_end <= ECAM_BASE || entry.base >= ECAM_BASE + ECAM_SIZE,
                "Low RAM must not extend into the ECAM window: {entry:?}"
            );
            assert!(
                entry_end <= PCI_HOLE_START || entry.base >= PCI_HOLE_END,
                "RAM must not overlap the PCI/MMIO hole: {entry:?}"
            );
        }

        // Total RAM should still match the configured guest memory, minus the legacy VGA + EBDA
        // reserved region within the first MiB.
        let expected_ram = total_memory - (ONE_MIB - EBDA_BASE);
        let ram_reported = sum_e820_ram(&map);
        assert_eq!(
            ram_reported, expected_ram,
            "Unexpected total RAM reported by E820: expected=0x{expected_ram:x}, got=0x{ram_reported:x}, map={map:?}"
        );
    }

    #[test]
    fn e820_with_zero_guest_ram_reports_no_ram_entries() {
        let map = build_e820_map(0, None, None);
        assert!(
            map.iter()
                .filter(|e| e.region_type == E820_RAM && e.length != 0)
                .count()
                == 0,
            "E820 should not report RAM when total_memory is 0: {map:?}"
        );
    }

    #[test]
    fn e820_reserved_windows_do_not_produce_overlapping_entries() {
        // Deliberately provide overlapping firmware-reserved regions to ensure the map builder
        // remains well-formed (no overlaps) even under inconsistent inputs.
        let total = 64 * 1024 * 1024;
        let base = 0x0010_0000;
        let acpi = (base + 0x1000, 0x2000); // 0x101000..0x103000
        let nvs = (base + 0x2000, 0x3000); // 0x102000..0x105000 (overlaps ACPI)
        let map = build_e820_map(total, Some(acpi), Some(nvs));

        // Ensure strict non-overlap and sortedness by base.
        assert_e820_sorted_and_non_overlapping(&map);
    }

    #[test]
    fn e820_map_invariants_hold_across_guest_ram_sizes_and_acpi_variants() {
        const ONE_MIB: u64 = 0x0010_0000;
        const FOUR_GIB: u64 = 0x1_0000_0000;
        const PCI_HOLE_START: u64 = 0xC000_0000;
        const PCI_HOLE_END: u64 = FOUR_GIB;

        #[derive(Clone, Copy)]
        struct Variant {
            name: &'static str,
            acpi: Option<(u64, u64)>,
            nvs: Option<(u64, u64)>,
        }

        // These are intentionally fixed placements so a single loop can cover a wide range of
        // `total_memory` values. Small guests will naturally clamp/ignore the regions.
        let variants: &[Variant] = &[
            Variant {
                name: "none",
                acpi: None,
                nvs: None,
            },
            Variant {
                name: "acpi_low",
                acpi: Some((0x0020_0000, 0x0002_0000)), // 2MiB..2MiB+128KiB
                nvs: None,
            },
            Variant {
                name: "acpi_nvs_overlap_low",
                acpi: Some((0x0020_0000, 0x0000_4000)), // 2MiB..2MiB+16KiB
                nvs: Some((0x0020_2000, 0x0000_4000)),  // overlaps ACPI by 8KiB
            },
            Variant {
                name: "acpi_low_nvs_high",
                acpi: Some((0x0020_0000, 0x0001_0000)), // 2MiB..2MiB+64KiB
                nvs: Some((FOUR_GIB + 0x0020_0000, 0x0001_0000)), // 4GiB+2MiB..+64KiB
            },
            Variant {
                name: "acpi_straddles_ecam_base",
                acpi: Some((PCIE_ECAM_BASE - 0x3000, 0x4000)), // crosses into the ECAM window
                nvs: None,
            },
        ];

        let ram_sizes: &[u64] = &[
            0,
            64 * 1024,
            640 * 1024,
            ONE_MIB,
            16 * ONE_MIB,
            PCIE_ECAM_BASE - 1,
            PCIE_ECAM_BASE,
            PCIE_ECAM_BASE + 1,
            FOUR_GIB,
            6 * 1024 * 1024 * 1024,
        ];

        fn overlaps(a_base: u64, a_len: u64, b_base: u64, b_len: u64) -> bool {
            let a_end = a_base.saturating_add(a_len);
            let b_end = b_base.saturating_add(b_len);
            a_base.max(b_base) < a_end.min(b_end)
        }

        fn assert_no_ram_overlap(
            ctx: &str,
            map: &[E820Entry],
            window_base: u64,
            window_len: u64,
            window_desc: &str,
        ) {
            for entry in map {
                if entry.region_type != E820_RAM {
                    continue;
                }
                assert!(
                    !overlaps(entry.base, entry.length, window_base, window_len),
                    "{ctx}: RAM entry overlaps {window_desc} window [0x{window_base:x}, 0x{:x}): entry={entry:?} map={map:?}",
                    window_base.saturating_add(window_len),
                );
            }
        }

        fn assert_reserved_window_present(
            ctx: &str,
            map: &[E820Entry],
            base: u64,
            len: u64,
            expected_type: u32,
            desc: &str,
        ) {
            assert!(
                map.iter().any(|e| e.base == base && e.length == len && e.region_type == expected_type),
                "{ctx}: missing {desc} window entry: expected base=0x{base:x} len=0x{len:x} type={expected_type}, map={map:?}",
            );
        }

        fn assert_reserved_input_is_reflected(
            ctx: &str,
            map: &[E820Entry],
            input: Option<(u64, u64)>,
            input_type: u32,
            range_base: u64,
            range_end: u64,
            range_desc: &str,
        ) {
            let Some((base, len)) = input else {
                return;
            };
            let end = base.saturating_add(len);
            let clipped_base = base.max(range_base);
            let clipped_end = end.min(range_end);
            if clipped_end <= clipped_base {
                // No intersection with this RAM range.
                return;
            }
            let clipped_len = clipped_end - clipped_base;

            // We don't require an exact 1:1 entry mapping (overlapping inputs can truncate), but we
            // do require:
            // - the reserved input is not reported as RAM
            // - there exists at least one entry of the reserved type intersecting the clipped
            //   window
            assert_no_ram_overlap(ctx, map, clipped_base, clipped_len, range_desc);
            assert!(
                map.iter()
                    .any(|e| e.region_type == input_type && overlaps(e.base, e.length, clipped_base, clipped_len)),
                "{ctx}: reserved {range_desc} window [0x{clipped_base:x}, 0x{clipped_end:x}) not reflected as type={input_type}; map={map:?}",
            );
        }

        for &total_memory in ram_sizes {
            for &variant in variants {
                let ctx = format!(
                    "e820 invariant failure: total_memory=0x{total_memory:x} ({total_memory} bytes), variant={}",
                    variant.name
                );
                let map = build_e820_map(total_memory, variant.acpi, variant.nvs);

                // Basic structural invariants: sorted, non-overlapping, and no zero-length entries.
                let mut last_end = 0u64;
                for (idx, entry) in map.iter().enumerate() {
                    assert!(
                        entry.length != 0,
                        "{ctx}: zero-length entry at index {idx}: {entry:?} map={map:?}"
                    );
                    assert!(
                        entry.base >= last_end,
                        "{ctx}: entries overlap or are out of order at index {idx}: last_end=0x{last_end:x}, entry={entry:?} map={map:?}"
                    );
                    assert!(
                        matches!(
                            entry.region_type,
                            E820_RAM | E820_RESERVED | E820_ACPI | E820_NVS
                        ),
                        "{ctx}: unexpected region type {} at index {idx}: {entry:?} map={map:?}",
                        entry.region_type
                    );
                    last_end = entry.base.saturating_add(entry.length);
                }

                if total_memory > PCIE_ECAM_BASE {
                    // When RAM extends beyond the low window, we must:
                    // - reserve ECAM + the remaining PCI/MMIO hole below 4GiB
                    // - remap the remainder of RAM above 4GiB
                    assert_reserved_window_present(
                        &ctx,
                        &map,
                        PCIE_ECAM_BASE,
                        PCIE_ECAM_SIZE,
                        E820_RESERVED,
                        "PCIe ECAM",
                    );
                    assert_reserved_window_present(
                        &ctx,
                        &map,
                        PCI_HOLE_START,
                        PCI_HOLE_END - PCI_HOLE_START,
                        E820_RESERVED,
                        "PCI/MMIO hole",
                    );
                    assert!(
                        map.iter().any(|e| e.base == FOUR_GIB
                            && e.region_type == E820_RAM
                            && e.length != 0),
                        "{ctx}: expected remapped high RAM entry starting at 4GiB, map={map:?}"
                    );

                    // RAM entries must never overlap the reserved ECAM / PCI windows.
                    assert_no_ram_overlap(&ctx, &map, PCIE_ECAM_BASE, PCIE_ECAM_SIZE, "ECAM");
                    assert_no_ram_overlap(
                        &ctx,
                        &map,
                        PCI_HOLE_START,
                        PCI_HOLE_END - PCI_HOLE_START,
                        "PCI/MMIO",
                    );
                } else {
                    assert!(
                        map.iter().all(|e| e.base < FOUR_GIB),
                        "{ctx}: did not expect any entries at/above 4GiB when total_memory <= PCIE_ECAM_BASE (0x{PCIE_ECAM_BASE:x}), map={map:?}"
                    );
                    assert!(
                        !map.iter().any(|e| e.region_type == E820_RAM && e.base >= FOUR_GIB),
                        "{ctx}: did not expect high RAM remap entries when total_memory <= PCIE_ECAM_BASE (0x{PCIE_ECAM_BASE:x}), map={map:?}"
                    );
                }

                // Validate that (clipped) ACPI/NVS inputs are not reported as RAM and show up with
                // the expected region type in whichever RAM window they intersect.
                //
                // Low RAM window: [1MiB, min(total_memory, PCIE_ECAM_BASE)).
                if total_memory > ONE_MIB {
                    let low_end = total_memory.min(PCIE_ECAM_BASE);
                    assert_reserved_input_is_reflected(
                        &ctx,
                        &map,
                        variant.acpi,
                        E820_ACPI,
                        ONE_MIB,
                        low_end,
                        "ACPI (low RAM)",
                    );
                    assert_reserved_input_is_reflected(
                        &ctx,
                        &map,
                        variant.nvs,
                        E820_NVS,
                        ONE_MIB,
                        low_end,
                        "NVS (low RAM)",
                    );
                }

                // High remap window: [4GiB, 4GiB + (total_memory - PCIE_ECAM_BASE)) when remapping.
                if total_memory > PCIE_ECAM_BASE {
                    let high_end = FOUR_GIB.saturating_add(total_memory - PCIE_ECAM_BASE);
                    assert_reserved_input_is_reflected(
                        &ctx,
                        &map,
                        variant.acpi,
                        E820_ACPI,
                        FOUR_GIB,
                        high_end,
                        "ACPI (high RAM)",
                    );
                    assert_reserved_input_is_reflected(
                        &ctx,
                        &map,
                        variant.nvs,
                        E820_NVS,
                        FOUR_GIB,
                        high_end,
                        "NVS (high RAM)",
                    );
                }
            }
        }
    }

    fn collect_e820_via_int15(total_memory: u64) -> Vec<E820Entry> {
        let mut bios = Bios::new(BiosConfig {
            memory_size_bytes: total_memory,
            boot_drive: 0x80,
            ..BiosConfig::default()
        });
        let mut bus = TestMemory::new(2 * 1024 * 1024);
        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.es, 0);

        // Buffer in low memory where INT 15h writes the E820 entry.
        const BUF_OFF: u16 = 0x0500;
        cpu.gpr[gpr::RDI] = BUF_OFF as u64;

        let mut entries = Vec::new();
        let mut idx: u64 = 0;
        loop {
            cpu.gpr[gpr::RAX] = 0xE820;
            cpu.gpr[gpr::RDX] = 0x534D_4150; // 'SMAP'
            cpu.gpr[gpr::RCX] = 24; // request extended attributes
            cpu.gpr[gpr::RBX] = idx;

            handle_int15(&mut bios, &mut cpu, &mut bus);
            assert_eq!(
                cpu.rflags & FLAG_CF,
                0,
                "INT 15h E820 failed for total_memory=0x{total_memory:x}, idx={idx}"
            );
            assert_eq!(
                cpu.gpr[gpr::RAX],
                0x534D_4150,
                "INT 15h E820 did not return 'SMAP' signature"
            );
            assert_eq!(
                cpu.gpr[gpr::RCX],
                24,
                "INT 15h E820 should return 24 bytes when caller requests extended attributes"
            );

            let buf_addr = u64::from(BUF_OFF);
            entries.push(E820Entry {
                base: bus.read_u64(buf_addr),
                length: bus.read_u64(buf_addr + 8),
                region_type: bus.read_u32(buf_addr + 16),
                extended_attributes: bus.read_u32(buf_addr + 20),
            });

            idx = cpu.gpr[gpr::RBX] & 0xFFFF_FFFF;
            if idx == 0 {
                break;
            }
        }
        entries
    }

    fn assert_e820_sorted_non_overlapping(entries: &[E820Entry]) {
        let mut last_end = 0u64;
        for entry in entries {
            assert_ne!(entry.length, 0, "E820 should not emit zero-length entries");
            assert!(
                entry.base >= last_end,
                "E820 entries overlap or are out of order: last_end=0x{last_end:x}, entry={entry:?}"
            );
            last_end = entry.base.saturating_add(entry.length);
        }
    }

    fn sum_usable_ram(entries: &[E820Entry]) -> u64 {
        entries
            .iter()
            .filter(|e| e.region_type == E820_RAM)
            .map(|e| e.length)
            .sum()
    }

    fn overlap_len(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> u64 {
        let start = a_start.max(b_start);
        let end = a_end.min(b_end);
        end.saturating_sub(start)
    }

    fn expected_usable_ram(total_memory: u64) -> u64 {
        // Guest RAM exists everywhere E820 reports as RAM, including the legacy reserved windows
        // below 1MiB (EBDA + VGA/BIOS).
        //
        // The amount of *usable* RAM is therefore the configured RAM size minus any reserved bytes
        // that overlap real RAM below 1MiB. PCI/ECAM holes do not reduce usable RAM because those
        // bytes are remapped above 4GiB.
        const ONE_MIB: u64 = 0x0010_0000;
        const VGA_START: u64 = 0x000A_0000;
        let ebda_start = EBDA_BASE;
        let ebda_end = EBDA_BASE + EBDA_SIZE as u64;
        let vga_start = VGA_START;
        let vga_end = ONE_MIB;
        let reserved_low = overlap_len(ebda_start, ebda_end, 0, total_memory)
            + overlap_len(vga_start, vga_end, 0, total_memory);
        total_memory.saturating_sub(reserved_low)
    }

    #[test]
    fn int15_e820_layout_across_guest_ram_sizes() {
        // These values are part of the platform contract (Q35-style layout). The firmware tests
        // should fail loudly if they drift, even though `aero-pc-constants`' own tests do not run
        // when executing `cargo test -p firmware`.
        assert_eq!(PCIE_ECAM_BASE, 0xB000_0000);
        assert_eq!(PCIE_ECAM_SIZE, 0x1000_0000);
        assert_eq!(EBDA_BASE, 0x0009_F000);
        assert_eq!(EBDA_SIZE, 0x1000);

        struct Case {
            name: &'static str,
            total_memory: u64,
            expected: Vec<E820Entry>,
        }

        const ONE_MIB: u64 = 0x0010_0000;
        const FOUR_GIB: u64 = 0x1_0000_0000;
        const VGA_START: u64 = 0x000A_0000;

        let cases = [
            // RAM < 640KiB edge case: ensure we clamp the conventional RAM entry so we never
            // report more usable RAM than exists.
            Case {
                name: "512KiB",
                total_memory: 512 * 1024,
                expected: vec![
                    E820Entry {
                        base: 0x0000_0000,
                        length: 512 * 1024,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: EBDA_BASE,
                        length: EBDA_SIZE as u64,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: VGA_START,
                        length: ONE_MIB - VGA_START,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                ],
            },
            Case {
                name: "16MiB",
                total_memory: 16 * 1024 * 1024,
                expected: vec![
                    E820Entry {
                        base: 0x0000_0000,
                        length: EBDA_BASE,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: EBDA_BASE,
                        length: EBDA_SIZE as u64,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: VGA_START,
                        length: ONE_MIB - VGA_START,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: ONE_MIB,
                        length: 16 * 1024 * 1024 - ONE_MIB,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                ],
            },
            Case {
                name: "32MiB",
                total_memory: 32 * 1024 * 1024,
                expected: vec![
                    E820Entry {
                        base: 0x0000_0000,
                        length: EBDA_BASE,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: EBDA_BASE,
                        length: EBDA_SIZE as u64,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: VGA_START,
                        length: ONE_MIB - VGA_START,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: ONE_MIB,
                        length: 32 * 1024 * 1024 - ONE_MIB,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                ],
            },
            Case {
                name: "just below ECAM base",
                total_memory: PCIE_ECAM_BASE - 1,
                expected: vec![
                    E820Entry {
                        base: 0x0000_0000,
                        length: EBDA_BASE,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: EBDA_BASE,
                        length: EBDA_SIZE as u64,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: VGA_START,
                        length: ONE_MIB - VGA_START,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: ONE_MIB,
                        length: (PCIE_ECAM_BASE - 1) - ONE_MIB,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                ],
            },
            Case {
                name: "exactly ECAM base",
                total_memory: PCIE_ECAM_BASE,
                expected: vec![
                    E820Entry {
                        base: 0x0000_0000,
                        length: EBDA_BASE,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: EBDA_BASE,
                        length: EBDA_SIZE as u64,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: VGA_START,
                        length: ONE_MIB - VGA_START,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: ONE_MIB,
                        length: PCIE_ECAM_BASE - ONE_MIB,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                ],
            },
            Case {
                name: "just above ECAM base",
                total_memory: PCIE_ECAM_BASE + 16 * 1024 * 1024,
                expected: vec![
                    E820Entry {
                        base: 0x0000_0000,
                        length: EBDA_BASE,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: EBDA_BASE,
                        length: EBDA_SIZE as u64,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: VGA_START,
                        length: ONE_MIB - VGA_START,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: ONE_MIB,
                        length: PCIE_ECAM_BASE - ONE_MIB,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: PCIE_ECAM_BASE,
                        length: PCIE_ECAM_SIZE,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: 0xC000_0000,
                        length: FOUR_GIB - 0xC000_0000,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: FOUR_GIB,
                        length: 16 * 1024 * 1024,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                ],
            },
            Case {
                name: "exactly PCI hole start (ECAM base + ECAM size)",
                total_memory: 0xC000_0000,
                expected: vec![
                    E820Entry {
                        base: 0x0000_0000,
                        length: EBDA_BASE,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: EBDA_BASE,
                        length: EBDA_SIZE as u64,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: VGA_START,
                        length: ONE_MIB - VGA_START,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: ONE_MIB,
                        length: PCIE_ECAM_BASE - ONE_MIB,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: PCIE_ECAM_BASE,
                        length: PCIE_ECAM_SIZE,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: 0xC000_0000,
                        length: FOUR_GIB - 0xC000_0000,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: FOUR_GIB,
                        length: PCIE_ECAM_SIZE,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                ],
            },
            Case {
                name: "4GiB",
                total_memory: FOUR_GIB,
                expected: vec![
                    E820Entry {
                        base: 0x0000_0000,
                        length: EBDA_BASE,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: EBDA_BASE,
                        length: EBDA_SIZE as u64,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: VGA_START,
                        length: ONE_MIB - VGA_START,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: ONE_MIB,
                        length: PCIE_ECAM_BASE - ONE_MIB,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: PCIE_ECAM_BASE,
                        length: PCIE_ECAM_SIZE,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: 0xC000_0000,
                        length: FOUR_GIB - 0xC000_0000,
                        region_type: E820_RESERVED,
                        extended_attributes: 1,
                    },
                    E820Entry {
                        base: FOUR_GIB,
                        length: FOUR_GIB - PCIE_ECAM_BASE,
                        region_type: E820_RAM,
                        extended_attributes: 1,
                    },
                ],
            },
        ];

        for case in cases {
            let entries = collect_e820_via_int15(case.total_memory);
            assert_e820_sorted_non_overlapping(&entries);

            assert_eq!(
                entries, case.expected,
                "Unexpected E820 map for {} (total_memory=0x{:x})",
                case.name, case.total_memory
            );

            // All returned entries must include the "enabled" extended attributes bit.
            for entry in &entries {
                assert_eq!(
                    entry.extended_attributes, 1,
                    "E820 entry should be marked enabled: {entry:?}"
                );
            }

            // Total usable RAM should equal the configured guest size minus the legacy reserved
            // low-memory windows, regardless of whether RAM is remapped above 4GiB.
            assert_eq!(
                sum_usable_ram(&entries),
                expected_usable_ram(case.total_memory),
                "Unexpected total usable RAM for {} (total_memory=0x{:x})",
                case.name,
                case.total_memory
            );

            // Regression checks: ensure no RAM overlaps the EBDA or VGA/BIOS reserved windows.
            let reserved_low = [
                (EBDA_BASE, EBDA_BASE + EBDA_SIZE as u64),
                (VGA_START, ONE_MIB),
            ];
            for entry in &entries {
                if entry.region_type != E820_RAM {
                    continue;
                }
                let entry_end = entry.base.saturating_add(entry.length);
                for &(r_base, r_end) in &reserved_low {
                    let overlap_start = entry.base.max(r_base);
                    let overlap_end = entry_end.min(r_end);
                    assert!(
                        overlap_end <= overlap_start,
                        "RAM entry overlaps reserved low-memory window: entry={entry:?}, reserved=0x{r_base:x}..0x{r_end:x}"
                    );
                }
            }
        }
    }
}
