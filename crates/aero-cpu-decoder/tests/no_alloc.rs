use aero_cpu_decoder::{decode_one, DecodeMode};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingAlloc;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

#[test]
fn decode_one_does_not_allocate_per_instruction() {
    // Warm-up: allow any one-time allocations (e.g., lazy init in dependencies)
    // to happen before we begin counting.
    let bytes = [0x48, 0x89, 0xD8]; // MOV RAX, RBX
    let _ = decode_one(DecodeMode::Bits64, 0x1000, &bytes).expect("warmup decode");

    ALLOCATIONS.store(0, Ordering::Relaxed);

    for _ in 0..10_000 {
        let inst = decode_one(DecodeMode::Bits64, 0x1000, &bytes).expect("decode");
        assert_eq!(inst.len(), 3);
    }

    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "decoder allocated during hot-path decode"
    );
}

