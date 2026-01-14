#![no_main]

use std::sync::Arc;

use aero_cpu_core::state::{gpr, CpuMode, CpuState, RFLAGS_RESERVED1, Segment};
use firmware::bda::{
    BDA_ACTIVE_PAGE_ADDR, BDA_CRTC_BASE_ADDR, BDA_CURSOR_POS_PAGE0_ADDR, BDA_CURSOR_SHAPE_ADDR,
    BDA_PAGE_SIZE_ADDR, BDA_SCREEN_COLS_ADDR, BDA_TEXT_ROWS_MINUS_ONE_ADDR, BDA_VIDEO_MODE_ADDR,
    BDA_VIDEO_PAGE_OFFSET_ADDR,
};
use firmware::bios::{
    A20Gate, Bios, BiosBus, BiosConfig, FirmwareMemory, InMemoryDisk, BDA_BASE, EBDA_BASE,
    EBDA_SIZE,
};
use libfuzzer_sys::fuzz_target;
use memory::{DenseMemory, MemoryBus, PhysicalMemoryBus};

/// Bound guest RAM so fuzz inputs cannot trigger large allocations.
const RAM_SIZE: u64 = 2 * 1024 * 1024; // 2 MiB

/// Bound disk size so fuzz inputs cannot trigger large allocations.
const MAX_DISK_SECTORS: usize = 4096; // 2 MiB (4096 * 512)
const MAX_DISK_BYTES: usize = MAX_DISK_SECTORS * 512;

/// Bound initial memory patch data so a single testcase can't do too much work.
const MAX_MEM_PATCH_BYTES: usize = 64 * 1024;

/// Bound injected keyboard keys.
const MAX_KEYS: usize = 4;

/// Minimal guest memory bus for BIOS fuzzing.
///
/// This mirrors the `TestBus` used in `crates/firmware/tests/el_torito_boot.rs`, but keeps behavior
/// best-effort: out-of-range accesses are ignored / return 0xFF (via `PhysicalMemoryBus`), and ROM
/// mapping failures are tolerated to avoid harness panics.
struct FuzzBus {
    a20_enabled: bool,
    inner: PhysicalMemoryBus,
}

impl FuzzBus {
    fn new(size: u64) -> Self {
        let ram = DenseMemory::new(size).expect("guest RAM allocation failed");
        Self {
            a20_enabled: false,
            inner: PhysicalMemoryBus::new(Box::new(ram)),
        }
    }

    fn translate_a20(&self, addr: u64) -> u64 {
        if self.a20_enabled {
            addr
        } else {
            addr & !(1u64 << 20)
        }
    }
}

impl A20Gate for FuzzBus {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20_enabled = enabled;
    }

    fn a20_enabled(&self) -> bool {
        self.a20_enabled
    }
}

impl FirmwareMemory for FuzzBus {
    fn map_rom(&mut self, base: u64, rom: Arc<[u8]>) {
        // BIOS POST maps ROM both in the conventional 0xF0000 window and at the reset-vector
        // alias 0xFFFF_0000. `PhysicalMemoryBus` supports sparse ROM mappings, but mapping can still
        // fail due to overlaps; tolerate failures to keep the harness panic-free.
        let _ = self.inner.map_rom(base, rom);
    }
}

impl MemoryBus for FuzzBus {
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

fn set_real_mode_seg(seg: &mut Segment, selector: u16) {
    seg.selector = selector;
    seg.base = (selector as u64) << 4;
    seg.limit = 0xFFFF;
    seg.access = 0;
}

fn init_minimal_bda(bus: &mut dyn BiosBus, boot_drive: u8) {
    // EBDA segment pointer (0x40:0x0E).
    bus.write_u16(BDA_BASE + 0x0E, (EBDA_BASE / 16) as u16);
    // Conventional memory size in KiB (0x40:0x13).
    bus.write_u16(BDA_BASE + 0x13, (EBDA_BASE / 1024) as u16);
    // EBDA size word at the start of EBDA.
    bus.write_u16(EBDA_BASE, (EBDA_SIZE / 1024) as u16);

    // Fixed disk count (0x40:0x75). This drives INT 13h drive-present probing.
    let hard_disk_count = if (0x80..=0xDF).contains(&boot_drive) {
        boot_drive.wrapping_sub(0x80).saturating_add(1)
    } else {
        0
    };
    bus.write_u8(BDA_BASE + 0x75, hard_disk_count);

    // Minimal equipment word (0x40:0x10) so INT 11h/INT 13h floppy probing has sane defaults.
    // Match `ivt::init_bda`:
    // - math coprocessor present
    // - initial video = 80x25 color
    // - one serial port (COM1)
    let mut equipment: u16 = (1 << 1) | (2 << 4) | (1 << 9);
    if boot_drive < 0x80 {
        // Report at least one floppy when booting from a floppy drive number.
        let drives = boot_drive.saturating_add(1).clamp(1, 4);
        equipment |= 1 << 0;
        equipment |= ((u16::from(drives.saturating_sub(1))) & 0x3) << 6;
    }
    bus.write_u16(BDA_BASE + 0x10, equipment);
}

fn init_minimal_video_state(bus: &mut dyn BiosBus) {
    // Many BIOS INT 10h services assume the BDA video fields have been initialized (typically by
    // POST setting mode 03h). Without this, helpers like teletype output can hit arithmetic
    // underflows (e.g. `cols - 1` when `cols == 0`).
    bus.write_u8(BDA_VIDEO_MODE_ADDR, 0x03);
    bus.write_u16(BDA_SCREEN_COLS_ADDR, 80);
    bus.write_u8(BDA_TEXT_ROWS_MINUS_ONE_ADDR, 25 - 1);
    bus.write_u16(BDA_PAGE_SIZE_ADDR, 80 * 25 * 2);
    bus.write_u16(BDA_VIDEO_PAGE_OFFSET_ADDR, 0);
    bus.write_u8(BDA_ACTIVE_PAGE_ADDR, 0);
    bus.write_u16(BDA_CRTC_BASE_ADDR, 0x3D4);
    for page in 0..8u8 {
        let addr = BDA_CURSOR_POS_PAGE0_ADDR + u64::from(page) * 2;
        bus.write_u16(addr, 0);
    }
    // Cursor shape: start=0x06, end=0x07 (word-packed as start in high byte, end in low byte).
    bus.write_u16(BDA_CURSOR_SHAPE_ADDR, 0x0607);
}

struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn take(&mut self, n: usize) -> &'a [u8] {
        let (head, tail) = self.data.split_at(n.min(self.data.len()));
        self.data = tail;
        head
    }

    fn u8(&mut self) -> u8 {
        self.take(1).first().copied().unwrap_or(0)
    }

    fn u16(&mut self) -> u16 {
        let b = self.take(2);
        let lo = b.first().copied().unwrap_or(0) as u16;
        let hi = b.get(1).copied().unwrap_or(0) as u16;
        lo | (hi << 8)
    }

    fn u32(&mut self) -> u32 {
        let b = self.take(4);
        let mut out = 0u32;
        for (i, byte) in b.iter().copied().enumerate() {
            out |= (byte as u32) << (i * 8);
        }
        out
    }

    fn u64(&mut self) -> u64 {
        let b = self.take(8);
        let mut out = 0u64;
        for (i, byte) in b.iter().copied().enumerate() {
            out |= (byte as u64) << (i * 8);
        }
        out
    }

    fn vec(&mut self, len: usize) -> Vec<u8> {
        self.take(len).to_vec()
    }
}

fn map_vector(raw: u8) -> u8 {
    // Bias towards the interrupts implemented by the legacy BIOS dispatcher, while still keeping
    // some "random vector" coverage for the default handler path.
    match raw % 12 {
        0 => 0x10,
        1 => 0x11,
        2 => 0x12,
        3 => 0x13,
        4 => 0x14,
        5 => 0x15,
        6 => 0x16,
        7 => 0x17,
        8 => 0x18,
        9 => 0x19,
        10 => 0x1A,
        _ => raw,
    }
}

fn install_interrupt_frame(bus: &mut dyn BiosBus, cpu: &mut CpuState, saved_flags: u16) {
    // `Bios::dispatch_interrupt` expects the CPU to have executed `INT` already, so SS:SP points to
    // an interrupt frame containing return IP, CS, FLAGS.
    //
    // Use wrapping semantics for SP, since real mode stack offsets are 16-bit.
    let sp_bits = cpu.stack_ptr_bits();
    let mask = aero_cpu_core::state::mask_bits(sp_bits);
    let sp = cpu.stack_ptr();

    let ip_sp = sp & mask;
    let cs_sp = sp.wrapping_add(2) & mask;
    let flags_sp = sp.wrapping_add(4) & mask;

    let ip_addr = cpu.apply_a20(cpu.segments.ss.base.wrapping_add(ip_sp));
    let cs_addr = cpu.apply_a20(cpu.segments.ss.base.wrapping_add(cs_sp));
    let flags_addr = cpu.apply_a20(cpu.segments.ss.base.wrapping_add(flags_sp));

    bus.write_u16(ip_addr, 0);
    bus.write_u16(cs_addr, 0);
    bus.write_u16(flags_addr, saved_flags | 0x0002);
}

fuzz_target!(|data: &[u8]| {
    let mut r = Reader::new(data);

    let vector_raw = r.u8();
    let vector = map_vector(vector_raw);

    let boot_drive = r.u8();
    let a20_enabled = (r.u8() & 1) != 0;

    let key_count = (r.u8() as usize) % (MAX_KEYS + 1);
    let mut keys = Vec::with_capacity(key_count);
    for _ in 0..key_count {
        keys.push(r.u16());
    }

    // Register values are modeled as full 64-bit GPRs; BIOS code generally masks as needed (16-bit
    // for most real-mode services, 32-bit for interfaces like INT 15h E820).
    let rax = r.u64();
    let rbx = r.u64();
    let rcx = r.u64();
    let rdx = r.u64();
    let rsi = r.u64();
    let rdi = r.u64();
    let rbp = r.u64();

    let ds = r.u16();
    let es = r.u16();
    let ss = r.u16();
    let sp = r.u16();

    let saved_flags = r.u16();
    let cpu_rflags = r.u16();

    let mem_offset = r.u32() as u64;
    let mem_len = (r.u16() as usize).min(MAX_MEM_PATCH_BYTES);
    let disk_len = (r.u32() as usize).min(MAX_DISK_BYTES);

    let mem_bytes = r.vec(mem_len);
    let mut disk_bytes = r.vec(disk_len);

    // Clamp disk size (and keep it sector-aligned) to avoid large allocations and reduce
    // per-iteration work.
    disk_bytes.truncate(MAX_DISK_BYTES);
    disk_bytes.truncate(disk_bytes.len() & !511);

    let mut disk = InMemoryDisk::new(disk_bytes);

    let mut bus = FuzzBus::new(RAM_SIZE);
    bus.set_a20_enabled(a20_enabled);

    init_minimal_bda(&mut bus, boot_drive);
    init_minimal_video_state(&mut bus);

    // Apply the input memory patch into guest RAM.
    if !mem_bytes.is_empty() {
        let start = mem_offset % RAM_SIZE;
        let max_len = (RAM_SIZE - start) as usize;
        let len = mem_bytes.len().min(max_len);
        bus.write_physical(start, &mem_bytes[..len]);
    }

    let mut bios = Bios::new(BiosConfig {
        memory_size_bytes: RAM_SIZE,
        boot_drive,
        // Keep fuzz iterations focused on interrupt dispatch rather than ACPI table generation.
        enable_acpi: false,
        ..BiosConfig::default()
    });
    for key in keys {
        bios.push_key(key);
    }

    let mut cpu = CpuState::new(CpuMode::Real);
    cpu.a20_enabled = bus.a20_enabled();
    cpu.rflags = (cpu_rflags as u64) | RFLAGS_RESERVED1;

    cpu.gpr[gpr::RAX] = rax;
    cpu.gpr[gpr::RBX] = rbx;
    cpu.gpr[gpr::RCX] = rcx;
    cpu.gpr[gpr::RDX] = rdx;
    cpu.gpr[gpr::RSI] = rsi;
    cpu.gpr[gpr::RDI] = rdi;
    cpu.gpr[gpr::RBP] = rbp;
    cpu.gpr[gpr::RSP] = sp as u64;

    set_real_mode_seg(&mut cpu.segments.cs, 0);
    set_real_mode_seg(&mut cpu.segments.ds, ds);
    set_real_mode_seg(&mut cpu.segments.es, es);
    set_real_mode_seg(&mut cpu.segments.ss, ss);
    set_real_mode_seg(&mut cpu.segments.fs, 0);
    set_real_mode_seg(&mut cpu.segments.gs, 0);

    install_interrupt_frame(&mut bus, &mut cpu, saved_flags);

    // Stress-test BIOS interrupt dispatch for panics / OOB on malformed guest state.
    bios.dispatch_interrupt(vector, &mut cpu, &mut bus, &mut disk, None);
});

