#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_net_e1000::{E1000Device, E1000_MMIO_SIZE};
use memory::{MemoryBus, MmioHandler};

const MAX_INPUT_LEN: usize = 4096;
const MEM_SIZE: usize = 64 * 1024;
const MAX_OPS: usize = 128;

struct BoundedMem {
    mem: Vec<u8>,
}

impl BoundedMem {
    fn new() -> Self {
        Self {
            mem: vec![0u8; MEM_SIZE],
        }
    }

    fn seed_from(&mut self, data: &[u8]) {
        let n = data.len().min(self.mem.len());
        self.mem[..n].copy_from_slice(&data[..n]);
    }
}

impl MemoryBus for BoundedMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let Ok(start) = usize::try_from(paddr) else {
            buf.fill(0);
            return;
        };
        let Some(end) = start.checked_add(buf.len()) else {
            buf.fill(0);
            return;
        };
        if start >= self.mem.len() {
            buf.fill(0);
            return;
        }
        let available_end = end.min(self.mem.len());
        let available_len = available_end - start;
        buf[..available_len].copy_from_slice(&self.mem[start..available_end]);
        if available_len < buf.len() {
            buf[available_len..].fill(0);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let Ok(start) = usize::try_from(paddr) else {
            return;
        };
        let Some(end) = start.checked_add(buf.len()) else {
            return;
        };
        if start >= self.mem.len() {
            return;
        }
        let available_end = end.min(self.mem.len());
        let available_len = available_end - start;
        self.mem[start..available_end].copy_from_slice(&buf[..available_len]);
    }
}

fn mmio_size_from_tag(tag: u8) -> usize {
    match tag & 0b11 {
        0 => 1,
        1 => 2,
        2 => 4,
        _ => 8,
    }
}

fuzz_target!(|data: &[u8]| {
    let data = &data[..data.len().min(MAX_INPUT_LEN)];

    let mut mem = BoundedMem::new();
    mem.seed_from(data);

    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

    // Enable bus mastering so `poll()` exercises descriptor DMA paths.
    dev.pci_config_write(0x04, 2, 0x4);

    let mut u = Unstructured::new(data);
    let ops: usize = u.int_in_range(0usize..=MAX_OPS).unwrap_or(0);

    for _ in 0..ops {
        let tag: u8 = u.arbitrary().unwrap_or(0);
        match tag % 4 {
            // MMIO write.
            0 | 1 | 2 => {
                let off_raw: u32 = u.arbitrary().unwrap_or(0);
                // Allow slightly out-of-range offsets to stress range checks without growing
                // unbounded maps.
                let off = (off_raw % (E1000_MMIO_SIZE + 0x1000)) as u64;
                let size = mmio_size_from_tag(tag);
                let value: u64 = u.arbitrary().unwrap_or(0);
                MmioHandler::write(&mut dev, off, size, value);
            }
            // Poll (DMA execution).
            _ => {
                dev.poll(&mut mem);
                // Drain any produced TX frames to keep memory usage bounded.
                for _ in 0..16 {
                    if dev.pop_tx_frame().is_none() {
                        break;
                    }
                }
            }
        }
    }

    // Final poll to flush any pending DMA side-effects.
    dev.poll(&mut mem);
    for _ in 0..16 {
        if dev.pop_tx_frame().is_none() {
            break;
        }
    }
});

