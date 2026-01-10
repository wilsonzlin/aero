use machine::{BlockDevice, CpuState, FLAG_CF, FLAG_DF, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF};

use super::{
    disk_err_to_int13_status, seg, Bios, BiosBus, BIOS_BASE, BIOS_SIZE, EBDA_BASE, EBDA_SIZE,
};

pub const E820_RAM: u32 = 1;
pub const E820_RESERVED: u32 = 2;
pub const E820_ACPI: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct E820Entry {
    pub base: u64,
    pub length: u64,
    pub region_type: u32,
    pub extended_attributes: u32,
}

pub fn dispatch_interrupt(
    bios: &mut Bios,
    vector: u8,
    cpu: &mut CpuState,
    bus: &mut dyn BiosBus,
    disk: &mut dyn BlockDevice,
) {
    // The CPU has executed `INT` already, and we're now running in a tiny ROM
    // stub that begins with `HLT`. The stack layout is:
    //   [SS:SP+0]  return IP
    //   [SS:SP+2]  return CS
    //   [SS:SP+4]  return FLAGS
    let sp = cpu.rsp as u16;
    let flags_addr = cpu.linear_addr(cpu.ss, sp.wrapping_add(4));
    let saved_flags = bus.read_u16(flags_addr);

    match vector {
        0x10 => handle_int10(bios, cpu, bus),
        0x13 => handle_int13(cpu, bus, disk),
        0x15 => handle_int15(bios, cpu, bus),
        0x16 => handle_int16(bios, cpu),
        _ => {
            // Safe default: do nothing and return.
            eprintln!("BIOS: unhandled interrupt {:02x}", vector);
        }
    }

    // Merge the flags the handler set into the saved FLAGS image so the stub's IRET
    // returns them to the caller, while preserving IF from the original interrupt frame.
    const RETURN_MASK: u16 = (FLAG_CF | FLAG_PF | FLAG_ZF | FLAG_SF | FLAG_DF | FLAG_OF) as u16;
    let new_flags = (saved_flags & !RETURN_MASK) | ((cpu.rflags as u16) & RETURN_MASK) | 0x0002;
    bus.write_u16(flags_addr, new_flags);
}

fn handle_int10(bios: &mut Bios, cpu: &mut CpuState, _bus: &mut dyn BiosBus) {
    let ah = ((cpu.rax >> 8) & 0xFF) as u8;
    match ah {
        0x00 => {
            // Set video mode (AL).
            let mode = (cpu.rax & 0xFF) as u8;
            bios.video_mode = mode;
            cpu.rflags &= !FLAG_CF;
        }
        0x0E => {
            // TTY output (AL).
            let ch = (cpu.rax & 0xFF) as u8;
            bios.tty_output.push(ch);
            cpu.rflags &= !FLAG_CF;
        }
        0x0F => {
            // Get current video mode.
            cpu.rax = (bios.video_mode as u64) | ((80u64) << 8);
            cpu.rbx &= !0xFFu64; // BH = active page (0)
            cpu.rflags &= !FLAG_CF;
        }
        _ => {
            eprintln!("BIOS: unhandled INT 10h AH={ah:02x}");
            cpu.rflags &= !FLAG_CF;
        }
    }
}

fn handle_int13(cpu: &mut CpuState, bus: &mut dyn BiosBus, disk: &mut dyn BlockDevice) {
    let ah = ((cpu.rax >> 8) & 0xFF) as u8;
    let drive = (cpu.rdx & 0xFF) as u8;

    match ah {
        0x00 => {
            // Reset disk system.
            cpu.rflags &= !FLAG_CF;
            cpu.rax &= !0xFF00u64;
        }
        0x02 => {
            // Read sectors (CHS).
            let count = (cpu.rax & 0xFF) as u8;
            let ch = (cpu.rcx & 0xFF) as u8;
            let cl = ((cpu.rcx >> 8) & 0xFF) as u8;
            let dh = ((cpu.rdx >> 8) & 0xFF) as u8;

            let sector = (cl & 0x3F) as u16;
            let cylinder = ((ch as u16) | (((cl as u16) & 0xC0) << 2)) as u32;
            let head = dh as u32;

            // Minimal fixed geometry.
            let spt = 63u32;
            let heads = 16u32;
            if sector == 0 || sector > spt as u16 {
                cpu.rflags |= FLAG_CF;
                cpu.rax = (cpu.rax & 0xFF) | ((0x01u64) << 8);
                return;
            }

            let lba = ((cylinder * heads + head) * spt + (sector as u32 - 1)) as u64;
            let dst = cpu.linear_addr(cpu.es, (cpu.rbx & 0xFFFF) as u16);

            for i in 0..count as u64 {
                let mut buf = [0u8; 512];
                match disk.read_sector(lba + i, &mut buf) {
                    Ok(()) => {
                        bus.write_physical(dst + i * 512, &buf);
                    }
                    Err(e) => {
                        cpu.rflags |= FLAG_CF;
                        let status = disk_err_to_int13_status(e);
                        cpu.rax = (cpu.rax & 0xFF) | ((status as u64) << 8);
                        return;
                    }
                }
            }

            cpu.rflags &= !FLAG_CF;
            cpu.rax &= !0xFF00u64; // AH=0
            let _ = drive;
        }
        0x08 => {
            // Get drive parameters (very small subset).
            // Return: CF clear, AH=0, CH/CL/DH describe geometry.
            let cylinders = 1024u16;
            let heads = 16u8;
            let spt = 63u8;

            let cyl_minus1 = cylinders - 1;
            let ch = (cyl_minus1 & 0xFF) as u8;
            let cl = (spt & 0x3F) | (((cyl_minus1 >> 2) as u8) & 0xC0);
            let dh = heads - 1;

            cpu.rcx = (cpu.rcx & !0xFFFF) | (ch as u64) | ((cl as u64) << 8);
            cpu.rdx = (cpu.rdx & !0xFF00) | ((dh as u64) << 8);
            cpu.rax &= !0xFF00u64;
            cpu.rflags &= !FLAG_CF;
        }
        0x15 => {
            // Get disk type.
            if drive < 0x80 {
                cpu.rax = 0;
            } else {
                cpu.rax = 0x0300;
            }
            cpu.rflags &= !FLAG_CF;
        }
        0x41 => {
            // Extensions check.
            if (cpu.rbx & 0xFFFF) == 0x55AA {
                cpu.rax = (cpu.rax & 0xFF) | (0x30u64 << 8);
                cpu.rbx = (cpu.rbx & !0xFFFF) | 0xAA55;
                cpu.rcx = (cpu.rcx & !0xFFFF) | 0x0007;
                cpu.rflags &= !FLAG_CF;
            } else {
                cpu.rflags |= FLAG_CF;
            }
        }
        0x42 => {
            // Extended read via Disk Address Packet (DAP) at DS:SI.
            let dap_addr = cpu.linear_addr(cpu.ds, (cpu.rsi & 0xFFFF) as u16);
            let dap_size = bus.read_u8(dap_addr);
            if dap_size < 0x10 {
                cpu.rflags |= FLAG_CF;
                cpu.rax = (cpu.rax & 0xFF) | (0x01u64 << 8);
                return;
            }

            let count = bus.read_u16(dap_addr + 2) as u64;
            let buf_off = bus.read_u16(dap_addr + 4);
            let buf_seg = bus.read_u16(dap_addr + 6);
            let lba = bus.read_u64(dap_addr + 8);
            let dst = cpu.linear_addr(seg(buf_seg), buf_off);

            for i in 0..count {
                let mut buf = [0u8; 512];
                match disk.read_sector(lba + i, &mut buf) {
                    Ok(()) => bus.write_physical(dst + i * 512, &buf),
                    Err(e) => {
                        cpu.rflags |= FLAG_CF;
                        let status = disk_err_to_int13_status(e);
                        cpu.rax = (cpu.rax & 0xFF) | ((status as u64) << 8);
                        return;
                    }
                }
            }

            cpu.rflags &= !FLAG_CF;
            cpu.rax &= !0xFF00u64;
        }
        _ => {
            eprintln!("BIOS: unhandled INT 13h AH={ah:02x}");
            cpu.rflags |= FLAG_CF;
            cpu.rax = (cpu.rax & 0xFF) | (0x01u64 << 8);
        }
    }
}

fn handle_int15(bios: &mut Bios, cpu: &mut CpuState, bus: &mut dyn BiosBus) {
    let ax = (cpu.rax & 0xFFFF) as u16;
    let ah = (ax >> 8) as u8;
    match (ah, ax) {
        (0xE8, 0xE820) => {
            // E820 memory map.
            if (cpu.rdx & 0xFFFF_FFFF) != 0x534D_4150 {
                cpu.rflags |= FLAG_CF;
                return;
            }

            if bios.e820_map.is_empty() {
                bios.e820_map = build_e820_map(bios.config.memory_size_bytes);
            }

            let idx = (cpu.rbx & 0xFFFF_FFFF) as usize;
            if idx >= bios.e820_map.len() {
                cpu.rflags |= FLAG_CF;
                return;
            }
            let entry = bios.e820_map[idx];

            let dst = cpu.linear_addr(cpu.es, (cpu.rdi & 0xFFFF) as u16);
            bus.write_u64(dst, entry.base);
            bus.write_u64(dst + 8, entry.length);
            bus.write_u32(dst + 16, entry.region_type);

            cpu.rax = 0x534D_4150;
            cpu.rcx = 20;
            cpu.rbx = if idx + 1 < bios.e820_map.len() {
                (idx as u64) + 1
            } else {
                0
            };
            cpu.rflags &= !FLAG_CF;
        }
        (0x88, _) => {
            // Extended memory size (KB above 1MB).
            let ext_kb = bios.config.memory_size_bytes.saturating_sub(1024 * 1024) / 1024;
            cpu.rax = ext_kb.min(0xFFFF) as u64;
            cpu.rflags &= !FLAG_CF;
        }
        _ => {
            eprintln!("BIOS: unhandled INT 15h AX={ax:04x}");
            cpu.rflags |= FLAG_CF;
        }
    }
}

fn handle_int16(bios: &mut Bios, cpu: &mut CpuState) {
    let ah = ((cpu.rax >> 8) & 0xFF) as u8;
    match ah {
        0x00 => {
            // Read keystroke (blocking in real BIOS; we return 0 if none).
            if let Some(k) = bios.keyboard_queue.pop_front() {
                cpu.rax = (cpu.rax & !0xFFFF) | (k as u64);
                cpu.rflags &= !FLAG_ZF;
            } else {
                cpu.rax &= !0xFFFF;
                cpu.rflags |= FLAG_ZF;
            }
            cpu.rflags &= !FLAG_CF;
        }
        0x01 => {
            // Check for keystroke (ZF=1 if none).
            if let Some(&k) = bios.keyboard_queue.front() {
                cpu.rax = (cpu.rax & !0xFFFF) | (k as u64);
                cpu.rflags &= !FLAG_ZF;
            } else {
                cpu.rflags |= FLAG_ZF;
            }
            cpu.rflags &= !FLAG_CF;
        }
        _ => {
            eprintln!("BIOS: unhandled INT 16h AH={ah:02x}");
            cpu.rflags |= FLAG_CF;
        }
    }
}

fn build_e820_map(total_memory: u64) -> Vec<E820Entry> {
    let mut map = Vec::new();

    // Conventional memory (0 - EBDA).
    map.push(E820Entry {
        base: 0,
        length: EBDA_BASE,
        region_type: E820_RAM,
        extended_attributes: 1,
    });

    // EBDA reserved.
    map.push(E820Entry {
        base: EBDA_BASE,
        length: EBDA_SIZE as u64,
        region_type: E820_RESERVED,
        extended_attributes: 1,
    });

    // VGA / option ROM area up to the ACPI table region.
    map.push(E820Entry {
        base: 0x000A_0000,
        length: 0x0004_0000, // 0xA0000-0xE0000
        region_type: E820_RESERVED,
        extended_attributes: 1,
    });

    // ACPI tables region (below BIOS ROM).
    map.push(E820Entry {
        base: super::ACPI_TABLE_BASE,
        length: super::ACPI_TABLE_SIZE as u64,
        region_type: E820_ACPI,
        extended_attributes: 1,
    });

    // BIOS ROM.
    map.push(E820Entry {
        base: BIOS_BASE,
        length: BIOS_SIZE as u64,
        region_type: E820_RESERVED,
        extended_attributes: 1,
    });

    // Extended memory (1MiB+).
    if total_memory > 0x0010_0000 {
        let length = total_memory - 0x0010_0000;
        map.push(E820Entry {
            base: 0x0010_0000,
            length,
            region_type: E820_RAM,
            extended_attributes: 1,
        });
    }

    map
}
