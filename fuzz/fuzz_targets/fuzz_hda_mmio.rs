#![no_main]

use arbitrary::Unstructured;
use libfuzzer_sys::fuzz_target;

use aero_audio::hda::{HdaController, HDA_MMIO_SIZE};
use aero_audio::mem::MemoryAccess;

const RAM_SIZE: usize = 0x10_000; // Fixed-size guest RAM: keep allocations bounded.
const MAX_OPS: usize = 256;
const MAX_PROCESS_FRAMES: usize = 2048;

// Minimal "safe" guest memory implementation for fuzzing:
// - OOB reads return zero-filled bytes
// - OOB writes are ignored
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

fn verb_12(verb_id: u16, payload8: u8) -> u32 {
    ((verb_id as u32) << 8) | (payload8 as u32)
}

fuzz_target!(|data: &[u8]| {
    let mut u = Unstructured::new(data);

    #[derive(Clone, Copy)]
    enum Op {
        MmioWrite {
            offset: u64,
            size: usize,
            value: u64,
        },
        MmioRead {
            offset: u64,
            size: usize,
        },
        Process {
            frames: usize,
        },
    }

    // Parse the (bounded) operation sequence first, then use the remaining bytes as guest RAM
    // backing. This keeps both op selection and RAM contents attacker-controlled.
    let ops_count: usize = u.int_in_range(0usize..=MAX_OPS).unwrap_or(0);
    let mut ops = Vec::with_capacity(ops_count);
    for _ in 0..ops_count {
        let kind: u8 = u.int_in_range(0u8..=15).unwrap_or(0);
        match kind {
            0..=9 => {
                let offset_raw: u16 = u.arbitrary().unwrap_or(0);
                // Keep offsets near the MMIO window but allow some OOB for negative testing.
                let offset = (offset_raw as u64) % (HDA_MMIO_SIZE as u64 + 0x40);
                let size_sel: u8 = u.int_in_range(0u8..=3).unwrap_or(0);
                let size = match size_sel {
                    0 => 1,
                    1 => 2,
                    2 => 4,
                    _ => 8, // invalid (device should ignore)
                };
                let value: u64 = u.arbitrary().unwrap_or(0);
                ops.push(Op::MmioWrite {
                    offset,
                    size,
                    value,
                });
            }
            10..=12 => {
                let offset_raw: u16 = u.arbitrary().unwrap_or(0);
                let offset = (offset_raw as u64) % (HDA_MMIO_SIZE as u64 + 0x40);
                let size_sel: u8 = u.int_in_range(0u8..=3).unwrap_or(0);
                let size = match size_sel {
                    0 => 1,
                    1 => 2,
                    2 => 4,
                    _ => 8,
                };
                ops.push(Op::MmioRead { offset, size });
            }
            _ => {
                let frames: u16 = u.arbitrary().unwrap_or(0);
                let frames = (frames as usize).min(MAX_PROCESS_FRAMES);
                ops.push(Op::Process { frames });
            }
        }
    }

    let mut mem = FuzzGuestMemory::new(RAM_SIZE);
    // Seed guest RAM from the remaining bytes so MMIO-programmed pointers can reference
    // attacker-controlled data without allocating dynamically.
    {
        let init_len = u.len();
        let init = u.bytes(init_len).unwrap_or(&[]);
        mem.seed_from(init);
    }

    let mut hda = HdaController::new();

    // Bias towards a functional configuration so `process()` reaches deeper DMA/codec code paths.
    // The fuzzer's MMIO ops can immediately undo this, which is fine.
    //
    // Enable controller (GCTL.CRST).
    hda.mmio_write(0x08, 4, 1);

    // Configure codec stream IDs to be non-zero so stream processing is reachable.
    // - output widget nid=2: SET_STREAM_CHANNEL (0x706), payload high nibble = stream id.
    // - input widget nid=4: SET_STREAM_CHANNEL (0x706)
    hda.codec_mut().execute_verb(2, verb_12(0x706, 0x10)); // stream id 1
    hda.codec_mut().execute_verb(4, verb_12(0x706, 0x20)); // stream id 2

    // Place a single-entry BDL for each stream inside guest RAM.
    // Output stream (descriptor 0).
    let out_bdl_base = 0x1000u64;
    let out_buf_base = 0x3000u64;
    let out_buf_len = 0x1000u32;
    mem.write_u64(out_bdl_base, out_buf_base);
    mem.write_u32(out_bdl_base + 8, out_buf_len);
    mem.write_u32(out_bdl_base + 12, 1); // IOC

    // Capture stream (descriptor 1).
    let in_bdl_base = 0x2000u64;
    let in_buf_base = 0x4000u64;
    let in_buf_len = 0x1000u32;
    mem.write_u64(in_bdl_base, in_buf_base);
    mem.write_u32(in_bdl_base + 8, in_buf_len);
    mem.write_u32(in_bdl_base + 12, 1); // IOC

    // Stream setup via public helpers (not MMIO) so the subsequent fuzzed MMIO ops can mutate from
    // a "working" state.
    {
        let sd = hda.stream_mut(0);
        sd.ctl = (1 << 0) | (1 << 1) | (1u32 << 20); // SRST|RUN|STRM=1
        sd.cbl = out_buf_len;
        sd.lvi = 0;
        sd.fmt = 0x0011; // 48kHz, 16-bit, stereo
        sd.bdpl = out_bdl_base as u32;
        sd.bdpu = 0;
    }
    // HdaController currently exposes 1 output + 1 input stream (2 total). Keep this as a direct
    // index to avoid relying on internal/private fields.
    {
        let sd = hda.stream_mut(1);
        sd.ctl = (1 << 0) | (1 << 1) | (2u32 << 20); // SRST|RUN|STRM=2
        sd.cbl = in_buf_len;
        sd.lvi = 0;
        sd.fmt = 0x0010; // 48kHz, 16-bit, mono
        sd.bdpl = in_bdl_base as u32;
        sd.bdpu = 0;
    }

    for op in ops {
        match op {
            Op::MmioWrite {
                offset,
                size,
                value,
            } => hda.mmio_write(offset, size, value),
            Op::MmioRead { offset, size } => {
                let _ = hda.mmio_read(offset, size);
            }
            Op::Process { frames } => hda.process(&mut mem, frames),
        }
    }
});
