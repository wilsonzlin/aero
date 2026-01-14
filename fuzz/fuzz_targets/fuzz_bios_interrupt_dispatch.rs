#![no_main]

use std::sync::{Arc, OnceLock};

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_cpu_core::state::{gpr, mask_bits, CpuMode, CpuState, Segment};
use firmware::bios::{
    build_bios_rom, A20Gate, Bios, BiosConfig, FirmwareMemory, BDA_BASE, BIOS_BASE, EBDA_BASE,
    EBDA_SIZE,
};
use memory::{DenseMemory, MemoryBus, PhysicalMemoryBus};

const MEM_SIZE: u64 = 2 * 1024 * 1024; // 2MiB (covers full real-mode address space + headroom)
const MAX_DISK_SECTORS: usize = 32; // 16KiB max

static BIOS_ROM: OnceLock<Arc<[u8]>> = OnceLock::new();

// Lightweight memory bus wrapper that:
// - provides a bounded RAM backing,
// - supports ROM mappings,
// - implements A20 gate wraparound semantics (mask bit 20 when disabled).
struct FuzzMemory {
    a20_enabled: bool,
    inner: PhysicalMemoryBus,
}

impl FuzzMemory {
    fn new(ram_size: u64) -> Self {
        let ram = DenseMemory::new(ram_size).unwrap_or_else(|_| DenseMemory::new(0).unwrap());
        Self {
            a20_enabled: true,
            inner: PhysicalMemoryBus::new(Box::new(ram)),
        }
    }

    #[inline]
    fn translate_a20(&self, addr: u64) -> u64 {
        if self.a20_enabled {
            addr
        } else {
            addr & !(1u64 << 20)
        }
    }
}

impl A20Gate for FuzzMemory {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20_enabled = enabled;
    }

    fn a20_enabled(&self) -> bool {
        self.a20_enabled
    }
}

impl FirmwareMemory for FuzzMemory {
    fn map_rom(&mut self, base: u64, rom: Arc<[u8]>) {
        // Ignore mapping errors; this is a fuzz harness and we want to avoid
        // panicking due to duplicate mappings.
        let _ = self.inner.map_rom(base, rom);
    }
}

impl MemoryBus for FuzzMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if self.a20_enabled {
            self.inner.read_physical(paddr, buf);
            return;
        }

        for (i, slot) in buf.iter_mut().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            *slot = self.inner.read_physical_u8(addr);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if self.a20_enabled {
            self.inner.write_physical(paddr, buf);
            return;
        }

        for (i, byte) in buf.iter().copied().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            self.inner.write_physical_u8(addr, byte);
        }
    }
}

#[inline]
fn set_real_mode_seg(seg: &mut Segment, selector: u16) {
    seg.selector = selector;
    seg.base = (selector as u64) << 4;
    seg.limit = 0xFFFF;
    seg.access = 0;
}

// Minimal BIOS Data Area init, loosely mirroring `bios::ivt::init_bda` but without depending on
// private firmware modules.
fn init_bda(bus: &mut impl MemoryBus, floppy_drives: u8, hard_disks: u8) {
    // COM1 present at 0x3F8; no LPT ports.
    bus.write_u16(BDA_BASE + 0x00, 0x03F8);
    bus.write_u16(BDA_BASE + 0x02, 0);
    bus.write_u16(BDA_BASE + 0x04, 0);
    bus.write_u16(BDA_BASE + 0x06, 0);
    bus.write_u16(BDA_BASE + 0x08, 0);
    bus.write_u16(BDA_BASE + 0x0A, 0);
    bus.write_u16(BDA_BASE + 0x0C, 0);

    // EBDA segment pointer.
    bus.write_u16(BDA_BASE + 0x0E, (EBDA_BASE / 16) as u16);

    // Equipment list word (INT 11h).
    //
    // - x87 FPU present
    // - VGA/EGA video (80x25 color)
    // - one serial port
    let mut equipment: u16 = (1 << 1) | (2 << 4) | (1 << 9);
    let floppy_drives = floppy_drives.min(4);
    if floppy_drives != 0 {
        equipment |= 1 << 0;
        equipment |= ((u16::from(floppy_drives.saturating_sub(1))) & 0x3) << 6;
    }
    bus.write_u16(BDA_BASE + 0x10, equipment);

    // Keyboard flags + buffer state.
    bus.write_u16(BDA_BASE + 0x17, 0);
    // Ring buffer metadata.
    const KB_BUF_START: u16 = 0x001E;
    const KB_BUF_END: u16 = 0x003E;
    bus.write_u16(BDA_BASE + 0x1A, KB_BUF_START);
    bus.write_u16(BDA_BASE + 0x1C, KB_BUF_START);
    bus.write_u16(BDA_BASE + 0x80, KB_BUF_START);
    bus.write_u16(BDA_BASE + 0x82, KB_BUF_END);

    // Fixed disk count.
    bus.write_u8(BDA_BASE + 0x75, hard_disks.min(4));

    // Conventional memory size (KiB) up to the EBDA base.
    bus.write_u16(BDA_BASE + 0x13, (EBDA_BASE / 1024) as u16);
    // EBDA starts with a size field in KiB.
    bus.write_u16(EBDA_BASE, (EBDA_SIZE / 1024) as u16);
}

fn prepare_cpu_for_interrupt(u: &mut Unstructured<'_>, vector: u8, cpu: &mut CpuState) {
    // Common real-mode baseline: keep segments and stack sane.
    let cs: u16 = u.arbitrary().unwrap_or(0);
    let ds: u16 = u.arbitrary().unwrap_or(0);
    let es: u16 = u.arbitrary().unwrap_or(0);
    // Keep SS out of the BIOS ROM segment so interrupt-frame writes generally hit RAM.
    let mut ss: u16 = u.arbitrary().unwrap_or(0);
    if ss >= 0xF000 {
        ss = 0;
    }

    set_real_mode_seg(&mut cpu.segments.cs, cs);
    set_real_mode_seg(&mut cpu.segments.ds, ds);
    set_real_mode_seg(&mut cpu.segments.es, es);
    set_real_mode_seg(&mut cpu.segments.ss, ss);

    // General purpose registers (16-bit values in real mode).
    let mut ax: u16 = u.arbitrary().unwrap_or(0);
    let bx: u16 = u.arbitrary().unwrap_or(0);
    let mut cx: u16 = u.arbitrary().unwrap_or(0);
    let mut dx: u16 = u.arbitrary().unwrap_or(0);
    let si: u16 = u.arbitrary().unwrap_or(0);
    let di: u16 = u.arbitrary().unwrap_or(0);
    let bp: u16 = u.arbitrary().unwrap_or(0);
    let mut sp: u16 = u.arbitrary().unwrap_or(0);

    // Clamp common "count" registers so the harness stays fast even when we hit
    // handlers with linear loops (e.g. INT 10h text writes, INT 13h sector loops).
    cx &= 0x00FF;
    if cx == 0 {
        cx = 1;
    }

    // Avoid the INT 13h "AL=0 means 256 sectors" convention by forcing AL != 0
    // for the CHS/verify paths we generate below.
    if vector == 0x13 && (ax & 0x00FF) == 0 {
        ax = (ax & 0xFF00) | 1;
    }

    // Select only supported subfunctions for interrupts that would otherwise spam `eprintln!`.
    match vector {
        0x13 => {
            const AH: &[u8] = &[
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x08, 0x09, 0x0C, 0x0D, 0x10, 0x11, 0x14, 0x15,
                0x16, 0x41, 0x42,
            ];
            let sel: u8 = u.arbitrary().unwrap_or(0);
            let ah = AH[sel as usize % AH.len()];
            ax = (ax & 0x00FF) | (u16::from(ah) << 8);

            // Constrain DL to the range covered by our BDA init (0..3, 0x80..0x83).
            let drive_sel: u8 = u.arbitrary().unwrap_or(0);
            let drive = if (drive_sel & 1) == 0 {
                drive_sel & 0x03
            } else {
                0x80 | (drive_sel & 0x03)
            };
            dx = (dx & 0xFF00) | u16::from(drive);
        }
        0x15 => {
            const AX: &[u16] = &[
                0x2400, 0x2401, 0x2402, 0x2403, 0xE801, 0xE820, 0x8600, 0xC000, 0x8800,
            ];
            let sel: u8 = u.arbitrary().unwrap_or(0);
            ax = AX[sel as usize % AX.len()];
        }
        0x16 => {
            const AH: &[u8] = &[0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x0C, 0x10, 0x11, 0x12];
            let sel: u8 = u.arbitrary().unwrap_or(0);
            let ah = AH[sel as usize % AH.len()];
            ax = (ax & 0x00FF) | (u16::from(ah) << 8);
        }
        _ => {}
    }

    cpu.gpr[gpr::RAX] = ax as u64;
    cpu.gpr[gpr::RBX] = bx as u64;
    cpu.gpr[gpr::RCX] = cx as u64;
    cpu.gpr[gpr::RDX] = dx as u64;
    cpu.gpr[gpr::RSI] = si as u64;
    cpu.gpr[gpr::RDI] = di as u64;
    cpu.gpr[gpr::RBP] = bp as u64;

    // Keep SP in range for a 6-byte interrupt frame.
    sp = sp.saturating_sub(6);
    cpu.gpr[gpr::RSP] = sp as u64;

    let flags: u16 = u.arbitrary().unwrap_or(0x0200);
    cpu.set_rflags(flags as u64);
    cpu.mode = CpuMode::Real;
    cpu.halted = false;
    cpu.clear_pending_bios_int();
}

fn write_interrupt_frame(bus: &mut impl MemoryBus, cpu: &CpuState, ip: u16, cs: u16, flags: u16) {
    let sp_bits = cpu.stack_ptr_bits();
    let sp = cpu.stack_ptr();

    let base = cpu.segments.ss.base;
    let ip_sp = sp & mask_bits(sp_bits);
    let cs_sp = sp.wrapping_add(2) & mask_bits(sp_bits);
    let flags_sp = sp.wrapping_add(4) & mask_bits(sp_bits);

    let ip_addr = cpu.apply_a20(base.wrapping_add(ip_sp));
    let cs_addr = cpu.apply_a20(base.wrapping_add(cs_sp));
    let flags_addr = cpu.apply_a20(base.wrapping_add(flags_sp));

    bus.write_u16(ip_addr, ip);
    bus.write_u16(cs_addr, cs);
    bus.write_u16(flags_addr, flags);
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    // Choose one of the implemented BIOS interrupt vectors.
    const VECTORS: &[u8] = &[
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A,
    ];
    let vector_sel: u8 = u.arbitrary().unwrap_or(0);
    let vector = VECTORS[vector_sel as usize % VECTORS.len()];

    let mut mem = FuzzMemory::new(MEM_SIZE);
    let a20_initial: bool = u.arbitrary().unwrap_or(true);
    mem.set_a20_enabled(a20_initial);

    // Map the BIOS ROM into the conventional real-mode window so INT 10h/13h pointers returned by
    // handlers refer to valid bytes.
    let rom = BIOS_ROM.get_or_init(|| build_bios_rom().into());
    mem.map_rom(BIOS_BASE, Arc::clone(rom));

    // Initialize the BDA/EBDA with a stable baseline so BIOS services that rely on probing (INT 11h,
    // drive counts, etc.) can make progress.
    let floppy_drives: u8 = u.int_in_range(0u8..=4).unwrap_or(0);
    let hard_disks: u8 = u.int_in_range(0u8..=4).unwrap_or(0);
    init_bda(&mut mem, floppy_drives, hard_disks);

    let mut bios = Bios::new(BiosConfig {
        memory_size_bytes: MEM_SIZE,
        // Avoid ACPI table generation in the harness; interrupt dispatch does not require it and
        // it would introduce extra work per iteration.
        enable_acpi: false,
        ..BiosConfig::default()
    });

    // Initialize VGA text-mode BDA fields (without clearing the entire text buffer).
    bios.video.vga.set_text_mode_03h(&mut mem, false);

    // Seed a small keyboard queue so INT 16h can exercise both empty/non-empty paths.
    let key_count: u8 = u.int_in_range(0u8..=4).unwrap_or(0);
    for _ in 0..key_count {
        let key: u16 = u.arbitrary().unwrap_or(0);
        bios.push_key(key);
    }

    // Disk contents (optional, bounded).
    let sectors: usize = u.int_in_range(0usize..=MAX_DISK_SECTORS).unwrap_or(0);
    let mut disk_data = vec![0u8; sectors.saturating_mul(512)];
    for b in &mut disk_data {
        *b = u.arbitrary().unwrap_or(0);
    }
    // Occasionally force a valid boot signature so INT 18h/19h can reach the "success" path.
    let make_bootable: bool = u.arbitrary().unwrap_or(false);
    if make_bootable && disk_data.len() >= 512 {
        disk_data[510] = 0x55;
        disk_data[511] = 0xAA;
    }
    let mut disk = firmware::bios::InMemoryDisk::new(disk_data);

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.a20_enabled = mem.a20_enabled();
    prepare_cpu_for_interrupt(&mut u, vector, &mut cpu);

    // Install a synthetic interrupt frame at SS:SP matching the layout expected by the BIOS stub
    // dispatch path.
    let ret_ip: u16 = u.arbitrary().unwrap_or(0);
    let ret_cs: u16 = u.arbitrary().unwrap_or(0);
    let ret_flags: u16 = u.arbitrary().unwrap_or(0x0200);
    write_interrupt_frame(&mut mem, &cpu, ret_ip, ret_cs, ret_flags);

    bios.dispatch_interrupt(vector, &mut cpu, &mut mem, &mut disk, None);
});
