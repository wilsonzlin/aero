use std::cell::RefCell;
use std::hint::black_box;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::wasm_bindgen;

struct MemBuffers {
    src: Vec<u8>,
    dst: Vec<u8>,
    size: usize,
}

impl MemBuffers {
    fn new() -> Self {
        Self {
            src: Vec::new(),
            dst: Vec::new(),
            size: 0,
        }
    }

    fn ensure_size(&mut self, size: usize) {
        if self.size == size {
            return;
        }

        self.src.resize(size, 0);
        self.dst.resize(size, 0);

        // Deterministic content for repeatability across runs/browsers.
        // The specific pattern is not important; only that it is stable.
        for (i, b) in self.src.iter_mut().enumerate() {
            *b = ((i as u32).wrapping_mul(31).wrapping_add(7) & 0xff) as u8;
        }
        for (i, b) in self.dst.iter_mut().enumerate() {
            *b = ((i as u32).wrapping_mul(17).wrapping_add(3) & 0xff) as u8;
        }

        self.size = size;
    }
}

thread_local! {
    static MEM_BUFFERS: RefCell<MemBuffers> = RefCell::new(MemBuffers::new());
}

/// Tight integer ALU loop (adds/xors/muls/rotates).
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn bench_integer_alu(iters: u32) -> u64 {
    let mut x: u64 = 0x1234_5678_9abc_def0;
    let mut y: u64 = 0xfedc_ba98_7654_3210;
    let mut acc: u64 = 0;

    for i in 0..iters {
        x = x.wrapping_add(y).rotate_left(13);
        y = y
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .wrapping_add(i as u64);
        acc ^= x.wrapping_add(y);
        acc = acc.rotate_right(7) ^ y;
    }

    acc ^ x ^ y ^ (iters as u64)
}

/// Branch-heavy loop with deterministic pseudo-random branching.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn bench_branchy(iters: u32) -> u64 {
    let mut state: u32 = 0x6d2b_79f5;
    let mut acc: u64 = 0;

    for _ in 0..iters {
        // LCG step (deterministic).
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);

        if state & 0x8000_0000 != 0 {
            acc = acc.wrapping_add(state as u64);
            acc ^= acc.rotate_left(11);
        } else {
            acc = acc.wrapping_sub(state as u64);
            acc ^= acc.rotate_right(9);
        }
    }

    acc ^ (state as u64)
}

/// Memory copy bandwidth benchmark.
///
/// Returns a small checksum to prevent dead-code elimination; the copy itself is kept opaque by
/// passing pointers through `black_box`.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn bench_memcpy(bytes: u32, iters: u32) -> u64 {
    let bytes = bytes as usize;
    if bytes == 0 || iters == 0 {
        return 0;
    }

    MEM_BUFFERS.with(|cell| {
        let mut buffers = cell.borrow_mut();
        let buffers = &mut *buffers;
        buffers.ensure_size(bytes);

        // Hide pointers/len from the optimizer so the copy isn't "proven redundant".
        let src_ptr = black_box(buffers.src.as_ptr());
        let dst_ptr = black_box(buffers.dst.as_mut_ptr());
        let len = black_box(bytes);

        let mut checksum: u64 = 0;
        for i in 0..iters {
            unsafe {
                std::ptr::copy_nonoverlapping(src_ptr, dst_ptr, len);
            }

            // Touch a byte that depends on `i` to make each iteration contribute to the output.
            let idx = ((i as usize).wrapping_mul(31)) % len;
            unsafe {
                checksum = checksum.wrapping_add(*dst_ptr.add(idx) as u64);
            }
        }

        checksum
    })
}

/// Mixed workload: hash-like streaming over memory.
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub fn bench_hash(bytes: u32, iters: u32) -> u64 {
    let bytes = bytes as usize;
    if bytes == 0 || iters == 0 {
        return 0;
    }

    MEM_BUFFERS.with(|cell| {
        let mut buffers = cell.borrow_mut();
        buffers.ensure_size(bytes);

        // FNV-1a over a deterministic buffer.
        let data = &buffers.src[..bytes];
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for i in 0..iters {
            hash ^= i as u64;
            for &b in data {
                hash ^= b as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alu_is_deterministic() {
        assert_eq!(bench_integer_alu(0), 0x1234_5678_9abc_def0 ^ 0xfedc_ba98_7654_3210);
        assert_eq!(bench_integer_alu(10), 0x8966_833f_b068_df5b);
    }

    #[test]
    fn branchy_is_deterministic() {
        assert_eq!(bench_branchy(0), 0x6d2b_79f5);
        assert_eq!(bench_branchy(10), 0x7b63_12c6_ddd8_1ef8);
    }

    #[test]
    fn memcpy_is_deterministic() {
        assert_eq!(bench_memcpy(0, 10), 0);
        assert_eq!(bench_memcpy(16, 10), 0x0000_0000_0000_0543);
    }

    #[test]
    fn hash_is_deterministic() {
        assert_eq!(bench_hash(0, 10), 0);
        assert_eq!(bench_hash(16, 3), 0x041f_6341_c54d_25e6);
    }
}
