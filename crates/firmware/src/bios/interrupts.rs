use aero_cpu_core::state::{
    gpr, mask_bits, CpuState, FLAG_CF, FLAG_DF, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF,
};

use super::{
    disk_err_to_int13_status, set_real_mode_seg, Bios, BiosBus, BiosMemoryBus, BlockDevice,
    BDA_BASE, EBDA_BASE, EBDA_SIZE,
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
    let sp_bits = cpu.stack_ptr_bits();
    let sp = cpu.stack_ptr();
    let flags_sp = sp.wrapping_add(4) & mask_bits(sp_bits);
    let flags_addr = cpu.apply_a20(cpu.segments.ss.base.wrapping_add(flags_sp));
    let saved_flags = bus.read_u16(flags_addr);

    match vector {
        0x10 => handle_int10(bios, cpu, bus),
        0x11 => handle_int11(cpu, bus),
        0x12 => handle_int12(cpu, bus),
        0x13 => handle_int13(bios, cpu, bus, disk),
        0x15 => handle_int15(bios, cpu, bus),
        0x16 => handle_int16(bios, cpu),
        0x19 => handle_int19(bios, cpu, bus, disk),
        0x1A => handle_int1a(bios, cpu, bus),
        _ => {
            // Safe default: do nothing and return.
            eprintln!("BIOS: unhandled interrupt {:02x}", vector);
        }
    }

    // Merge the flags the handler set into the saved FLAGS image so the stub's IRET
    // returns them to the caller, while preserving IF from the original interrupt frame.
    const RETURN_MASK: u16 = (FLAG_CF | FLAG_PF | FLAG_ZF | FLAG_SF | FLAG_DF | FLAG_OF) as u16;
    let new_flags = (saved_flags & !RETURN_MASK) | ((cpu.rflags() as u16) & RETURN_MASK) | 0x0002;
    bus.write_u16(flags_addr, new_flags);
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

fn handle_int12(cpu: &mut CpuState, bus: &mut dyn BiosBus) {
    // Get conventional memory size (KiB).
    //
    // Return the BIOS Data Area base memory size word (at 0x413). POST initializes this to the
    // amount of memory below the EBDA.
    let base_mem_kb = bus.read_u16(BDA_BASE + 0x13);
    cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (base_mem_kb as u64);
    cpu.rflags &= !FLAG_CF;
}

fn handle_int19(bios: &mut Bios, cpu: &mut CpuState, bus: &mut dyn BiosBus, disk: &mut dyn BlockDevice) {
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
    // After IRET, the CPU will begin executing the boot sector at 0000:7C00 with SS:SP reset to
    // 0000:7C00 (matching POST's boot handoff).
    const BOOT_ADDR: u64 = 0x7C00;
    const STACK_AFTER_IRET: u16 = 0x7C00;
    const STACK_BEFORE_IRET: u16 = STACK_AFTER_IRET.wrapping_sub(6);

    let mut sector = [0u8; 512];
    match disk.read_sector(0, &mut sector) {
        Ok(()) => {}
        Err(_) => {
            bios.tty_output.extend_from_slice(b"Disk read error\n");
            cpu.halted = true;
            return;
        }
    }
    if sector[510] != 0x55 || sector[511] != 0xAA {
        bios.tty_output.extend_from_slice(b"Invalid boot signature\n");
        cpu.halted = true;
        return;
    }

    bus.write_physical(BOOT_ADDR, &sector);

    // Register setup per BIOS conventions (matches `Bios::boot`).
    cpu.gpr[gpr::RAX] = 0;
    cpu.gpr[gpr::RBX] = 0;
    cpu.gpr[gpr::RCX] = 0;
    cpu.gpr[gpr::RDX] = bios.config.boot_drive as u64; // DL
    cpu.gpr[gpr::RSI] = 0;
    cpu.gpr[gpr::RDI] = 0;
    cpu.gpr[gpr::RBP] = 0;

    // Use a clean 0000:7C00 stack. We must set SP to 7BFA so the following IRET lands with
    // SP=7C00.
    set_real_mode_seg(&mut cpu.segments.ss, 0x0000);
    cpu.gpr[gpr::RSP] = STACK_BEFORE_IRET as u64;

    // Data segments: most boot sectors expect DS=ES=0.
    set_real_mode_seg(&mut cpu.segments.ds, 0x0000);
    set_real_mode_seg(&mut cpu.segments.es, 0x0000);

    // Build the synthetic IRET frame: IP, CS, FLAGS (word each).
    let frame_base = cpu.apply_a20(cpu.segments.ss.base.wrapping_add(STACK_BEFORE_IRET as u64));
    bus.write_u16(frame_base, BOOT_ADDR as u16); // IP
    bus.write_u16(frame_base + 2, 0x0000); // CS
    bus.write_u16(frame_base + 4, 0x0202); // IF=1 + reserved bit 1

    cpu.rflags &= !FLAG_CF;
}

fn handle_int10(bios: &mut Bios, cpu: &mut CpuState, bus: &mut dyn BiosBus) {
    // Keep the historical "TTY output" buffer for tests/debugging.
    let ah = ((cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8;
    if ah == 0x0E {
        bios.tty_output.push((cpu.gpr[gpr::RAX] & 0xFF) as u8);
    }

    // Bridge machine CPU state + memory bus to the firmware-side INT 10h implementation.
    let mut fw_cpu = FirmwareCpuState {
        rax: cpu.gpr[gpr::RAX],
        rbx: cpu.gpr[gpr::RBX],
        rcx: cpu.gpr[gpr::RCX],
        rdx: cpu.gpr[gpr::RDX],
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
) {
    let ah = ((cpu.gpr[gpr::RAX] >> 8) & 0xFF) as u8;
    let drive = (cpu.gpr[gpr::RDX] & 0xFF) as u8;

    match ah {
        0x00 => {
            // Reset disk system.
            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64;
        }
        0x01 => {
            // Get status of last disk operation.
            let status = bios.last_int13_status;
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | ((status as u64) << 8);
            if status == 0 {
                cpu.rflags &= !FLAG_CF;
            } else {
                cpu.rflags |= FLAG_CF;
            }
        }
        0x02 => {
            // Read sectors (CHS).
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

            // Minimal fixed geometry.
            let spt = 63u32;
            let heads = 16u32;
            if sector == 0 || sector > spt as u16 {
                bios.last_int13_status = 0x01;
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | ((0x01u64) << 8);
                return;
            }

            let lba = ((cylinder * heads + head) * spt + (sector as u32 - 1)) as u64;
            let bx = cpu.gpr[gpr::RBX] & 0xFFFF;
            let dst = cpu.apply_a20(cpu.segments.es.base.wrapping_add(bx));

            // Many real BIOS implementations use DMA for this path and require the transfer
            // buffer not cross a 64KiB physical boundary.
            let total_bytes = (count as u64) * 512;
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
                let mut buf = [0u8; 512];
                match disk.read_sector(lba + i, &mut buf) {
                    Ok(()) => {
                        bus.write_physical(dst + i * 512, &buf);
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

            cpu.gpr[gpr::RCX] = (cpu.gpr[gpr::RCX] & !0xFFFF) | (cl as u64) | ((ch as u64) << 8);
            // DL = number of drives; DH = max head.
            cpu.gpr[gpr::RDX] = (cpu.gpr[gpr::RDX] & !0xFFFF) | 1u64 | ((dh as u64) << 8);
            cpu.gpr[gpr::RAX] &= !0xFF00u64;
            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
        }
        0x15 => {
            // Get disk type.
            if drive < 0x80 {
                cpu.gpr[gpr::RAX] = 0;
            } else {
                cpu.gpr[gpr::RAX] = 0x0300;
            }
            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
        }
        0x41 => {
            // Extensions check.
            if (cpu.gpr[gpr::RBX] & 0xFFFF) == 0x55AA && drive >= 0x80 {
                // Report EDD 3.0 (AH=0x30) and that we support 42h + 48h.
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (0x30u64 << 8);
                cpu.gpr[gpr::RBX] = (cpu.gpr[gpr::RBX] & !0xFFFF) | 0xAA55;
                cpu.gpr[gpr::RCX] = (cpu.gpr[gpr::RCX] & !0xFFFF) | 0x0005;
                bios.last_int13_status = 0;
                cpu.rflags &= !FLAG_CF;
            } else {
                bios.last_int13_status = 0x01;
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x01u64 << 8);
            }
        }
        0x42 => {
            // Extended read via Disk Address Packet (DAP) at DS:SI.
            if drive < 0x80 {
                bios.last_int13_status = 0x01;
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x01u64 << 8);
                return;
            }

            let si = cpu.gpr[gpr::RSI] & 0xFFFF;
            let dap_addr = cpu.apply_a20(cpu.segments.ds.base.wrapping_add(si));
            let dap_size = bus.read_u8(dap_addr);
            if dap_size != 0x10 && dap_size != 0x18 {
                bios.last_int13_status = 0x01;
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x01u64 << 8);
                return;
            }

            if bus.read_u8(dap_addr + 1) != 0 {
                bios.last_int13_status = 0x01;
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x01u64 << 8);
                return;
            }

            let count = bus.read_u16(dap_addr + 2) as u64;
            if count == 0 {
                bios.last_int13_status = 0x01;
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x01u64 << 8);
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
                bios.last_int13_status = 0x04;
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x04u64 << 8);
                return;
            };
            if end > disk.size_in_sectors() {
                bios.last_int13_status = 0x04;
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x04u64 << 8);
                return;
            }

            for i in 0..count {
                let mut buf = [0u8; 512];
                match disk.read_sector(lba + i, &mut buf) {
                    Ok(()) => bus.write_physical(dst + i * 512, &buf),
                    Err(e) => {
                        cpu.rflags |= FLAG_CF;
                        let status = disk_err_to_int13_status(e);
                        bios.last_int13_status = status;
                        cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | ((status as u64) << 8);
                        return;
                    }
                }
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
            if drive < 0x80 {
                bios.last_int13_status = 0x01;
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x01u64 << 8);
                return;
            }

            let si = cpu.gpr[gpr::RSI] & 0xFFFF;
            let table_addr = cpu.apply_a20(cpu.segments.ds.base.wrapping_add(si));
            let buf_size = bus.read_u16(table_addr) as usize;
            if buf_size < 0x1A {
                bios.last_int13_status = 0x01;
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x01u64 << 8);
                return;
            }

            // Fill the EDD drive parameter table (subset).
            // We write as much as the caller says they can accept.
            let write_len = buf_size.min(0x1E) as u16;
            bus.write_u16(table_addr, write_len);
            if buf_size >= 4 {
                bus.write_u16(table_addr + 2, 0); // flags
            }
            if buf_size >= 8 {
                bus.write_u32(table_addr + 4, 1024); // cylinders
            }
            if buf_size >= 12 {
                bus.write_u32(table_addr + 8, 16); // heads
            }
            if buf_size >= 16 {
                bus.write_u32(table_addr + 12, 63); // sectors/track
            }
            if buf_size >= 24 {
                bus.write_u64(table_addr + 16, disk.size_in_sectors());
            }
            if buf_size >= 26 {
                bus.write_u16(table_addr + 24, 512); // bytes/sector
            }
            if buf_size >= 30 {
                bus.write_u32(table_addr + 26, 0); // DPTE pointer (unused)
            }

            bios.last_int13_status = 0;
            cpu.rflags &= !FLAG_CF;
            cpu.gpr[gpr::RAX] &= !0xFF00u64;
        }
        _ => {
            eprintln!("BIOS: unhandled INT 13h AH={ah:02x}");
            bios.last_int13_status = 0x01;
            cpu.rflags |= FLAG_CF;
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x01u64 << 8);
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
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x86u64 << 8);
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
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x86u64 << 8);
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
            0x88 => {
                // Extended memory size (KB above 1MB).
                let ext_kb = bios.config.memory_size_bytes.saturating_sub(1024 * 1024) / 1024;
                cpu.gpr[gpr::RAX] = ext_kb.min(0xFFFF);
                cpu.rflags &= !FLAG_CF;
            }
            _ => {
                eprintln!("BIOS: unhandled INT 15h AX={ax:04x}");
                cpu.rflags |= FLAG_CF;
                cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & 0xFF) | (0x86u64 << 8);
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
            // Our BIOS models the keyboard buffer as an unbounded FIFO queue, so this always
            // succeeds (real hardware returns CF=1 when the 32-byte BIOS data area ring buffer is
            // full).
            let key = (cpu.gpr[gpr::RCX] & 0xFFFF) as u16;
            bios.keyboard_queue.push_back(key);
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
            eprintln!("BIOS: unhandled INT 16h AH={ah:02x}");
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
        ivt, A20Gate, BiosConfig, InMemoryDisk, TestMemory, BDA_BASE, EBDA_BASE, PCIE_ECAM_BASE,
        PCIE_ECAM_SIZE,
    };
    use super::*;
    use aero_cpu_core::state::{gpr, CpuMode, CpuState, FLAG_CF, FLAG_ZF};
    use memory::MemoryBus as _;

    #[test]
    fn int13_ext_read_reads_lba_into_memory() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let mut disk_bytes = vec![0u8; 512 * 4];
        disk_bytes[512..1024].fill(0xAA);
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0500;
        cpu.gpr[gpr::RDX] = 0x80; // DL = HDD0
        cpu.gpr[gpr::RAX] = 0x4200; // AH=42h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        cpu.a20_enabled = mem.a20_enabled();
        let dap_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0500);
        mem.write_u8(dap_addr, 0x10);
        mem.write_u8(dap_addr + 1, 0x00);
        mem.write_u16(dap_addr + 2, 1); // count
        mem.write_u16(dap_addr + 4, 0x1000); // offset
        mem.write_u16(dap_addr + 6, 0x0000); // segment
        mem.write_u64(dap_addr + 8, 1); // LBA

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);

        let buf = mem.read_bytes(0x1000, 512);
        assert_eq!(buf, vec![0xAA; 512]);
    }

    #[test]
    fn int13_ext_get_drive_params_reports_sector_count() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; 512 * 8];
        let sectors = (disk_bytes.len() / 512) as u64;
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0600;
        cpu.gpr[gpr::RDX] = 0x80; // DL = HDD0
        cpu.gpr[gpr::RAX] = 0x4800; // AH=48h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        cpu.a20_enabled = mem.a20_enabled();
        let table_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0600);
        mem.write_u16(table_addr, 0x1E); // buffer size

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0);

        assert_eq!(mem.read_u64(table_addr + 16), sectors);
        assert_eq!(mem.read_u16(table_addr + 24), 512);
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
    fn int13_get_status_reports_last_error() {
        let mut bios = Bios::new(super::super::BiosConfig::default());
        let disk_bytes = vec![0u8; 512];
        let mut disk = InMemoryDisk::new(disk_bytes);

        let mut cpu = CpuState::new(CpuMode::Real);
        set_real_mode_seg(&mut cpu.segments.ds, 0);
        cpu.gpr[gpr::RSI] = 0x0700;
        cpu.gpr[gpr::RDX] = 0x80; // DL = HDD0
        cpu.gpr[gpr::RAX] = 0x4200; // AH=42h

        let mut mem = TestMemory::new(2 * 1024 * 1024);
        cpu.a20_enabled = mem.a20_enabled();
        let dap_addr = cpu.apply_a20(cpu.segments.ds.base + 0x0700);
        mem.write_u8(dap_addr, 0x10);
        mem.write_u8(dap_addr + 1, 0x00);
        mem.write_u16(dap_addr + 2, 1); // count
        mem.write_u16(dap_addr + 4, 0x1000); // offset
        mem.write_u16(dap_addr + 6, 0x0000); // segment
        mem.write_u64(dap_addr + 8, 1); // LBA (out of range)

        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk);
        assert_ne!(cpu.rflags & FLAG_CF, 0);
        assert_eq!((cpu.gpr[gpr::RAX] >> 8) & 0xFF, 0x04);

        cpu.gpr[gpr::RAX] = 0x0100; // AH=01h
        handle_int13(&mut bios, &mut cpu, &mut mem, &mut disk);
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
        ivt::init_bda(&mut mem);

        handle_int12(&mut cpu, &mut mem);

        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, (EBDA_BASE / 1024) as u16);
    }

    #[test]
    fn int19_loads_boot_sector_and_installs_iret_frame_to_jump_to_7c00() {
        let mut bios = Bios::new(BiosConfig {
            boot_drive: 0x80,
            ..BiosConfig::default()
        });
        let mut cpu = CpuState::new(CpuMode::Real);

        let mut sector = [0u8; 512];
        sector[0] = 0xAA;
        sector[1] = 0xBB;
        sector[510] = 0x55;
        sector[511] = 0xAA;
        let mut disk = InMemoryDisk::from_boot_sector(sector);
        let mut mem = TestMemory::new(2 * 1024 * 1024);

        handle_int19(&mut bios, &mut cpu, &mut mem, &mut disk);

        assert_eq!(cpu.halted, false);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.gpr[gpr::RDX] as u8, 0x80);
        assert_eq!(cpu.segments.ss.selector, 0x0000);
        assert_eq!(cpu.gpr[gpr::RSP] as u16, 0x7BFA);

        let loaded = mem.read_bytes(0x7C00, 512);
        assert_eq!(loaded[0], 0xAA);
        assert_eq!(loaded[1], 0xBB);
        assert_eq!(loaded[510], 0x55);
        assert_eq!(loaded[511], 0xAA);

        // Verify the synthetic IRET frame at 0000:7BFA.
        assert_eq!(mem.read_u16(0x7BFA), 0x7C00); // IP
        assert_eq!(mem.read_u16(0x7BFC), 0x0000); // CS
        assert_eq!(mem.read_u16(0x7BFE), 0x0202); // FLAGS
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

        bios.push_key(0x1234);

        // AH=11h (check for extended keystroke) should not dequeue.
        cpu.gpr[gpr::RAX] = 0x1100;
        handle_int16(&mut bios, &mut cpu);
        assert_eq!(cpu.rflags & FLAG_CF, 0);
        assert_eq!(cpu.rflags & FLAG_ZF, 0);
        assert_eq!(cpu.gpr[gpr::RAX] as u16, 0x1234);

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
        let mut last_end = 0u64;
        for entry in &map {
            assert!(
                entry.base >= last_end,
                "E820 entries overlap or are out of order: last_end=0x{last_end:x}, entry={entry:?}"
            );
            last_end = entry.base.saturating_add(entry.length);
        }
    }
}
