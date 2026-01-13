#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_audio::hda::HdaController;
use aero_audio::mem::MemoryAccess;

const RAM_SIZE: usize = 0x10_000;
const MAX_ITERS: usize = 16;

// HDA MMIO offsets used by this fuzzer (mirrors `aero_audio::hda`).
const REG_GCTL: u64 = 0x08;

const REG_CORBLBASE: u64 = 0x40;
const REG_CORBUBASE: u64 = 0x44;
const REG_CORBWP: u64 = 0x48;
const REG_CORBRP: u64 = 0x4a;
const REG_CORBCTL: u64 = 0x4c;
const REG_CORBSIZE: u64 = 0x4e;

const REG_RIRBLBASE: u64 = 0x50;
const REG_RIRBUBASE: u64 = 0x54;
const REG_RIRBWP: u64 = 0x58;
const REG_RIRBCTL: u64 = 0x5c;
const REG_RIRBSIZE: u64 = 0x5e;

const CORBCTL_RUN: u8 = 1 << 1;
const RIRBCTL_RUN: u8 = 1 << 1;
const RIRBCTL_RINTCTL: u8 = 1 << 0;

#[derive(Clone)]
struct FuzzGuestMemory {
    data: Vec<u8>,
}

impl FuzzGuestMemory {
    fn new(size: usize) -> Self {
        Self { data: vec![0; size] }
    }

    fn seed_from(&mut self, bytes: &[u8]) {
        let n = bytes.len().min(self.data.len());
        self.data[..n].copy_from_slice(&bytes[..n]);
    }
}

impl MemoryAccess for FuzzGuestMemory {
    fn read_physical(&self, addr: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }
        let Ok(start) = usize::try_from(addr) else {
            buf.fill(0);
            return;
        };
        if start >= self.data.len() {
            buf.fill(0);
            return;
        }
        let available = self.data.len() - start;
        let n = buf.len().min(available);
        buf[..n].copy_from_slice(&self.data[start..start + n]);
        if n < buf.len() {
            buf[n..].fill(0);
        }
    }

    fn write_physical(&mut self, addr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }
        let Ok(start) = usize::try_from(addr) else {
            return;
        };
        if start >= self.data.len() {
            return;
        }
        let available = self.data.len() - start;
        let n = buf.len().min(available);
        self.data[start..start + n].copy_from_slice(&buf[..n]);
    }
}

fn ring_entries(sel: u8) -> usize {
    match sel & 0x3 {
        0 => 2,
        1 => 16,
        _ => 256,
    }
}

fn place_region(seed: u64, mem_size: usize, align: usize, size: usize) -> u64 {
    if mem_size <= size {
        return 0;
    }
    let max_start = mem_size - size;
    let mut off = (seed as usize) % (max_start + 1);
    if align.is_power_of_two() {
        off &= !(align - 1);
    }
    off as u64
}

fuzz_target!(|data: &[u8]| {
    // Seed guest RAM directly from the fuzzer input so ring buffers placed near address 0 can be
    // attacker-controlled even if the structured decoder consumes many bytes.
    let mut mem = FuzzGuestMemory::new(RAM_SIZE);
    mem.seed_from(data);

    let mut u = Unstructured::new(data);

    let mut hda = HdaController::new();
    // Enable controller so CORB processing is active.
    hda.mmio_write(REG_GCTL, 4, 1);

    // Randomly select ring sizes; keep them consistent between CORB and RIRB so we get a good mix
    // of wrap-around cases.
    let corb_sel: u8 = u.int_in_range(0u8..=3).unwrap_or(2);
    let rirb_sel: u8 = u.int_in_range(0u8..=3).unwrap_or(2);
    hda.mmio_write(REG_CORBSIZE, 1, (corb_sel & 0x3) as u64);
    hda.mmio_write(REG_RIRBSIZE, 1, (rirb_sel & 0x3) as u64);

    let corb_entries = ring_entries(corb_sel);
    let rirb_entries = ring_entries(rirb_sel);

    let corb_seed: u64 = u.arbitrary().unwrap_or(0);
    let rirb_seed: u64 = u.arbitrary().unwrap_or(0);
    let corb_base = place_region(corb_seed, RAM_SIZE, 128, corb_entries * 4);
    let rirb_base = place_region(rirb_seed, RAM_SIZE, 128, rirb_entries * 8);

    hda.mmio_write(REG_CORBLBASE, 4, corb_base);
    hda.mmio_write(REG_CORBUBASE, 4, 0);
    hda.mmio_write(REG_RIRBLBASE, 4, rirb_base);
    hda.mmio_write(REG_RIRBUBASE, 4, 0);

    // Populate CORB contents with attacker-controlled verbs.
    for i in 0..corb_entries {
        let cmd: u32 = u.arbitrary().unwrap_or(0);
        mem.write_u32(corb_base.wrapping_add((i as u64) * 4), cmd);
    }

    let iters: usize = u.int_in_range(0usize..=MAX_ITERS).unwrap_or(0);
    for _ in 0..iters {
        // Randomly mutate a few CORB slots per iteration to allow changing verbs without requiring
        // the fuzzer to fully refill the ring every time.
        let muts: u8 = u.int_in_range(0u8..=8).unwrap_or(0);
        for _ in 0..muts {
            let idx: u16 = u.arbitrary().unwrap_or(0);
            let idx = (idx as usize) % corb_entries.max(1);
            let cmd: u32 = u.arbitrary().unwrap_or(0);
            mem.write_u32(corb_base.wrapping_add((idx as u64) * 4), cmd);
        }

        let corb_run: bool = u.arbitrary().unwrap_or(false);
        let rirb_run: bool = u.arbitrary().unwrap_or(false);
        let rirb_int: bool = u.arbitrary().unwrap_or(false);

        hda.mmio_write(REG_CORBCTL, 1, (corb_run as u8 * CORBCTL_RUN) as u64);
        hda.mmio_write(
            REG_RIRBCTL,
            1,
            ((rirb_run as u8 * RIRBCTL_RUN) | (rirb_int as u8 * RIRBCTL_RINTCTL)) as u64,
        );

        let corbwp: u8 = u.arbitrary().unwrap_or(0);
        let corbrp: u8 = u.arbitrary().unwrap_or(0);
        let corbrp_reset: bool = u.arbitrary().unwrap_or(false);
        hda.mmio_write(REG_CORBWP, 2, corbwp as u64);
        let rp_val: u16 = (corbrp as u16) | ((corbrp_reset as u16) << 15);
        hda.mmio_write(REG_CORBRP, 2, rp_val as u64);

        let rirbwp: u8 = u.arbitrary().unwrap_or(0);
        let rirbwp_reset: bool = u.arbitrary().unwrap_or(false);
        let wp_val: u16 = (rirbwp as u16) | ((rirbwp_reset as u16) << 15);
        hda.mmio_write(REG_RIRBWP, 2, wp_val as u64);

        // CORB processing is performed from `process()`. `output_frames=0` exercises CORB/RIRB
        // state machines without doing stream DMA/resampling work.
        hda.process(&mut mem, 0);
    }
});
