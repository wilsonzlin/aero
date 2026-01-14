#![no_main]

use std::sync::Arc;

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_cpu_core::state::{gpr, CpuMode, CpuState, Segment};
use firmware::bios::{A20Gate, Bios, BiosConfig, FirmwareMemory, InMemoryDisk, BIOS_ALIAS_BASE, BIOS_BASE, BIOS_SIZE};
use memory::{DenseMemory, MapError, MemoryBus, PhysicalMemoryBus};

const RAM_SIZES: &[u64] = &[
    4 * 1024 * 1024,  // Smallest size that can still hold the largest built-in VBE LFB mode.
    8 * 1024 * 1024,  // Midpoint.
    16 * 1024 * 1024, // Matches the BIOS default.
];

// 1024×768×32bpp.
const MAX_VBE_LFB_BYTES: u64 = 1024 * 768 * 4;

const BIOS_VECTORS: &[u8] = &[0x10, 0x13, 0x15, 0x16, 0x1A];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Outcome {
    vector: u8,
    a20_enabled: bool,
    gpr: [u64; 16],
    rflags: u64,
    segs: [u16; 6],
    bios_tty_hash: u64,
    ram_hash: u64,
}

fn set_real_mode_seg(seg: &mut Segment, selector: u16) {
    seg.selector = selector;
    seg.base = (selector as u64) << 4;
    seg.limit = 0xFFFF;
    seg.access = 0;
}

fn fnv1a64(mut h: u64, bytes: &[u8]) -> u64 {
    // 64-bit FNV-1a.
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    if h == 0 {
        h = FNV_OFFSET;
    }
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

fn is_addr_in_range(addr: u64, start: u64, len: u64) -> bool {
    addr >= start && addr < start.saturating_add(len)
}

fn is_range_within(start: u64, len: usize, base: u64, size: u64) -> bool {
    let len_u64 = len as u64;
    let Some(end) = start.checked_add(len_u64) else {
        return false;
    };
    start >= base && end <= base.saturating_add(size)
}

struct CheckedBus {
    a20_enabled: bool,
    inner: PhysicalMemoryBus,
    oob_access: bool,
}

impl CheckedBus {
    fn new(ram_size: u64) -> Self {
        // Keep initialization bounded and fast: start from a zeroed RAM image, and let the fuzzer
        // patch specific bytes/ranges via explicit writes (see `run()`).
        let ram = DenseMemory::new(ram_size).expect("DenseMemory allocation failed");
        Self {
            a20_enabled: false,
            inner: PhysicalMemoryBus::new(Box::new(ram)),
            oob_access: false,
        }
    }

    fn translate_a20(&self, addr: u64) -> u64 {
        if self.a20_enabled {
            addr
        } else {
            addr & !(1u64 << 20)
        }
    }

    fn range_is_mapped(&self, start: u64, len: usize) -> bool {
        let len_u64 = len as u64;
        let Some(end) = start.checked_add(len_u64) else {
            return false;
        };

        // Fast path: entirely within RAM.
        if end <= self.inner.ram.size() {
            return true;
        }

        // Entirely within a single ROM region.
        for r in self.inner.rom_regions() {
            if is_range_within(start, len, r.start, r.data.len() as u64) {
                return true;
            }
        }

        // Entirely within a single MMIO region.
        for r in self.inner.mmio_regions() {
            let size = r.end.saturating_sub(r.start);
            if is_range_within(start, len, r.start, size) {
                return true;
            }
        }

        false
    }

    fn check_addr_mapped(&mut self, addr: u64) {
        let ram_size = self.inner.ram.size();
        if addr < ram_size {
            return;
        }
        if self
            .inner
            .rom_regions()
            .iter()
            .any(|r| is_addr_in_range(addr, r.start, r.data.len() as u64))
        {
            return;
        }
        if self
            .inner
            .mmio_regions()
            .iter()
            .any(|r| addr >= r.start && addr < r.end)
        {
            return;
        }
        self.oob_access = true;
    }

    fn assert_invariants(&self, pre_rom: &[(u64, usize)], pre_ram_under_bios: &[u8]) {
        // Mapping invariants: ROM mappings remain intact.
        let post_rom: Vec<(u64, usize)> = self
            .inner
            .rom_regions()
            .iter()
            .map(|r| (r.start, r.data.len()))
            .collect();
        assert_eq!(post_rom, pre_rom, "ROM mapping changed across BIOS interrupt dispatch");

        // ROM invariants: writes must not go through to underlying RAM.
        let under = self
            .inner
            .ram
            .get_slice(BIOS_BASE, BIOS_SIZE)
            .expect("BIOS_BASE should be within allocated RAM");
        assert_eq!(
            under,
            pre_ram_under_bios,
            "writes to BIOS ROM region wrote through to underlying RAM"
        );
    }

    fn assert_no_oob(&self) {
        assert!(
            !self.oob_access,
            "BIOS interrupt dispatch attempted an access outside of mapped RAM/ROM/MMIO"
        );
    }
}

impl A20Gate for CheckedBus {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20_enabled = enabled;
    }

    fn a20_enabled(&self) -> bool {
        self.a20_enabled
    }
}

impl FirmwareMemory for CheckedBus {
    fn map_rom(&mut self, base: u64, rom: Arc<[u8]>) {
        let len = rom.len();
        match self.inner.map_rom(base, rom) {
            Ok(()) => {}
            Err(MapError::Overlap) => {
                panic!("unexpected ROM mapping overlap at 0x{base:016x} (len=0x{len:x})");
            }
            Err(MapError::AddressOverflow) => {
                panic!("ROM mapping overflow at 0x{base:016x} (len=0x{len:x})");
            }
        }
    }
}

impl MemoryBus for CheckedBus {
    fn read_physical(&mut self, paddr: u64, dst: &mut [u8]) {
        if dst.is_empty() {
            return;
        }

        if self.a20_enabled && self.range_is_mapped(paddr, dst.len()) {
            self.inner.read_physical(paddr, dst);
            return;
        }

        for (i, slot) in dst.iter_mut().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            self.check_addr_mapped(addr);
            *slot = self.inner.read_physical_u8(addr);
        }
    }

    fn write_physical(&mut self, paddr: u64, src: &[u8]) {
        if src.is_empty() {
            return;
        }

        if self.a20_enabled && self.range_is_mapped(paddr, src.len()) {
            self.inner.write_physical(paddr, src);
            return;
        }

        for (i, byte) in src.iter().copied().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            self.check_addr_mapped(addr);
            self.inner.write_physical_u8(addr, byte);
        }
    }
}

fn run(data: &[u8]) -> Outcome {
    let mut u = Unstructured::new(data);

    let vector = BIOS_VECTORS
        .get(u.int_in_range(0usize..=BIOS_VECTORS.len().saturating_sub(1)).unwrap_or(0))
        .copied()
        .unwrap_or(0x10);

    let ram_size = RAM_SIZES
        .get(u.int_in_range(0usize..=RAM_SIZES.len().saturating_sub(1)).unwrap_or(0))
        .copied()
        .unwrap_or(16 * 1024 * 1024);

    let mut bus = CheckedBus::new(ram_size);

    // Seed a handful of RAM bytes from the fuzz input without requiring the input itself to be
    // megabytes large.
    let ram_patch_count: usize = u.int_in_range(0usize..=64).unwrap_or(0);
    for _ in 0..ram_patch_count {
        let addr = (u.arbitrary::<u32>().unwrap_or(0) as u64) % ram_size;
        let len = u.int_in_range(0usize..=256).unwrap_or(0);
        let bytes = u.bytes(len).unwrap_or(&[]);
        let write_len = bytes.len().min((ram_size - addr) as usize);
        if write_len == 0 {
            continue;
        }
        if let Some(dst) = bus.inner.ram.get_slice_mut(addr, write_len) {
            dst.copy_from_slice(&bytes[..write_len]);
        }
    }

    // Disk backing for INT 13h. Keep sizes small/finite (fixed set).
    let disk_sectors_options: &[usize] = &[1, 4, 32, 2880];
    let disk_sectors = disk_sectors_options
        .get(
            u.int_in_range(0usize..=disk_sectors_options.len().saturating_sub(1))
                .unwrap_or(0),
        )
        .copied()
        .unwrap_or(1);

    let disk_seed: u64 = u.arbitrary().unwrap_or(0);
    let mut disk_bytes = vec![0u8; disk_sectors.saturating_mul(512)];
    {
        let mut state = disk_seed;
        for chunk in disk_bytes.chunks_exact_mut(8) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            chunk.copy_from_slice(&state.to_le_bytes());
        }
        if !disk_bytes.is_empty() {
            // Ensure a valid boot signature so BIOS POST completes deterministically.
            if disk_bytes.len() >= 512 {
                disk_bytes[510] = 0x55;
                disk_bytes[511] = 0xAA;
            }
        }
    }
    let mut disk = InMemoryDisk::new(disk_bytes);

    let mut bios = Bios::new(BiosConfig {
        memory_size_bytes: ram_size,
        // ACPI table building isn't required for interrupt dispatch fuzzing and adds work.
        enable_acpi: false,
        ..BiosConfig::default()
    });

    // Ensure the VBE LFB is inside RAM for the chosen RAM size, even if a fuzzed INT 10h call
    // switches to the largest (1024×768×32bpp) mode.
    let lfb_base = ram_size
        .saturating_sub(MAX_VBE_LFB_BYTES)
        // Avoid clobbering the low 1MiB (BDA/EBDA/ROM/VGA) with framebuffer clears when possible.
        .max(0x0010_0000);
    bios.video.vbe.lfb_base = (lfb_base as u32).min(u32::MAX);

    // Run POST once to install ROM stubs / initialize IVT+BDA.
    let mut cpu = CpuState::new(CpuMode::Real);
    bios.post(&mut cpu, &mut bus, &mut disk);

    // Snapshot invariants after POST, before the fuzzed interrupt dispatch.
    let pre_rom: Vec<(u64, usize)> = bus
        .inner
        .rom_regions()
        .iter()
        .map(|r| (r.start, r.data.len()))
        .collect();
    assert!(
        pre_rom.iter().any(|(s, l)| *s == BIOS_BASE && *l == BIOS_SIZE),
        "expected BIOS ROM mapped at BIOS_BASE"
    );
    assert!(
        pre_rom
            .iter()
            .any(|(s, l)| *s == BIOS_ALIAS_BASE && *l == BIOS_SIZE),
        "expected BIOS ROM mapped at BIOS_ALIAS_BASE"
    );

    let pre_ram_under_bios = bus
        .inner
        .ram
        .get_slice(BIOS_BASE, BIOS_SIZE)
        .expect("BIOS_BASE should be within allocated RAM")
        .to_vec();

    // Fuzzed CPU state: keep real-mode segment semantics but allow arbitrary register values.
    cpu.mode = CpuMode::Real;
    cpu.halted = false;
    cpu.set_rip(u.arbitrary().unwrap_or(0));

    for reg in 0..cpu.gpr.len() {
        cpu.gpr[reg] = u.arbitrary().unwrap_or(0);
    }
    cpu.set_rflags(u.arbitrary().unwrap_or(0));

    let cs: u16 = u.arbitrary().unwrap_or(0);
    let ds: u16 = u.arbitrary().unwrap_or(0);
    let es: u16 = u.arbitrary().unwrap_or(0);
    let ss: u16 = u.arbitrary().unwrap_or(0);
    let fs: u16 = u.arbitrary().unwrap_or(0);
    let gs: u16 = u.arbitrary().unwrap_or(0);
    set_real_mode_seg(&mut cpu.segments.cs, cs);
    set_real_mode_seg(&mut cpu.segments.ds, ds);
    set_real_mode_seg(&mut cpu.segments.es, es);
    set_real_mode_seg(&mut cpu.segments.ss, ss);
    set_real_mode_seg(&mut cpu.segments.fs, fs);
    set_real_mode_seg(&mut cpu.segments.gs, gs);

    // Keep the CPU-side A20 view consistent with the bus, so `CpuState::apply_a20` matches bus
    // translation.
    cpu.a20_enabled = bus.a20_enabled();

    // To keep fuzzing throughput reasonable, bias the subfunction selector registers towards
    // implemented paths (avoids high-volume `eprintln!` in "unhandled" stubs).
    match vector {
        0x13 => {
            // INT 13h: select an implemented AH function.
            const AH_FUNCS: &[u8] = &[
                0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x08, 0x09, 0x0C, 0x0D, 0x10, 0x11, 0x14,
                0x15, 0x16, 0x41, 0x42, 0x43, 0x48,
            ];
            let ah = AH_FUNCS
                .get(u.int_in_range(0usize..=AH_FUNCS.len().saturating_sub(1)).unwrap_or(0))
                .copied()
                .unwrap_or(0x00);
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFF00) | ((ah as u64) << 8);
        }
        0x15 => {
            // INT 15h: pick a known AX service.
            const AX_FUNCS: &[u16] = &[
                0x2400, 0x2401, 0x2402, 0x2403, // A20 gate
                0xE801, 0xE820, // memory map
                0x8600, // wait
                0xC000, // system config params
                0x8800, // extended memory size
            ];
            let ax = AX_FUNCS
                .get(u.int_in_range(0usize..=AX_FUNCS.len().saturating_sub(1)).unwrap_or(0))
                .copied()
                .unwrap_or(0x2402);
            cpu.gpr[gpr::RAX] = (cpu.gpr[gpr::RAX] & !0xFFFF) | (ax as u64);

            if ax == 0xE820 {
                // Provide the required SMAP signature and a sensible request size so we exercise
                // deeper code paths.
                cpu.gpr[gpr::RDX] =
                    (cpu.gpr[gpr::RDX] & !0xFFFF_FFFF) | 0x534D_4150u64; // "SMAP"
                let req_size = if u.arbitrary::<bool>().unwrap_or(false) {
                    24u32
                } else {
                    20u32
                };
                cpu.gpr[gpr::RCX] =
                    (cpu.gpr[gpr::RCX] & !0xFFFF_FFFF) | (req_size as u64);
            }
        }
        _ => {}
    }

    bios.dispatch_interrupt(vector, &mut cpu, &mut bus, &mut disk, None);

    // Safety invariants.
    bus.assert_no_oob();
    bus.assert_invariants(&pre_rom, &pre_ram_under_bios);

    // Determinism/diff-friendly outcome.
    let mut tty_hash = 0u64;
    tty_hash = fnv1a64(tty_hash, bios.tty_output());

    // Sample a few RAM ranges. We intentionally hash the underlying RAM (not the ROM-overlay view)
    // so we can detect accidental ROM write-through.
    let mut ram_hash = 0u64;
    let hash_slice = |addr: u64, len: usize, ram_hash: &mut u64| {
        if let Some(slice) = bus.inner.ram.get_slice(addr, len) {
            *ram_hash = fnv1a64(*ram_hash, slice);
        }
    };
    hash_slice(0x0000_0400, 0x200, &mut ram_hash); // BDA
    hash_slice(0x0000_7C00, 512, &mut ram_hash); // boot sector
    hash_slice(0x000B_8000, 0x800, &mut ram_hash); // VGA text
    hash_slice(BIOS_BASE, BIOS_SIZE, &mut ram_hash); // underlying RAM under BIOS ROM
    hash_slice(lfb_base, 4096.min((ram_size - lfb_base) as usize), &mut ram_hash); // LFB head

    Outcome {
        vector,
        a20_enabled: bus.a20_enabled(),
        gpr: cpu.gpr,
        rflags: cpu.rflags(),
        segs: [
            cpu.segments.cs.selector,
            cpu.segments.ds.selector,
            cpu.segments.es.selector,
            cpu.segments.ss.selector,
            cpu.segments.fs.selector,
            cpu.segments.gs.selector,
        ],
        bios_tty_hash: tty_hash,
        ram_hash,
    }
}

fuzz_target!(|data: &[u8]| {
    // Basic determinism check to catch accidental use of non-deterministic sources (e.g. RNG,
    // time) in BIOS interrupt handlers.
    let a = run(data);
    let b = run(data);
    assert_eq!(a, b);
});
