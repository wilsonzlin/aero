#![cfg(not(target_arch = "wasm32"))]

use aero_x86::tier1::decode_one_mode;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

struct CountingAlloc;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // libtest runs each `#[test]` in its own thread, and the harness may
    // allocate concurrently on other threads (result reporting, output capture,
    // etc.). We only want to count allocations performed by the decoder on the
    // current test thread.
    static COUNT_ALLOC: Cell<bool> = const { Cell::new(false) };
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && COUNT_ALLOC.with(|c| c.get()) {
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
fn tier1_decode_one_mode_does_not_allocate_per_instruction() {
    // Warm-up: allow any one-time allocations (e.g., lazy init in dependencies)
    // to happen before we begin counting.
    let bytes = [0x48, 0x89, 0xD8]; // MOV RAX, RBX
    let _ = decode_one_mode(0x1000, &bytes, 64);

    ALLOCATIONS.store(0, Ordering::Relaxed);

    COUNT_ALLOC.with(|c| c.set(true));
    for _ in 0..10_000 {
        let inst = decode_one_mode(0x1000, &bytes, 64);
        assert_eq!(inst.len, 3);
    }
    COUNT_ALLOC.with(|c| c.set(false));

    assert_eq!(
        ALLOCATIONS.load(Ordering::Relaxed),
        0,
        "tier1 decoder allocated during hot-path decode"
    );
}
