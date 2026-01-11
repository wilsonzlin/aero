//! Guest RAM layout enforcement for wasm32.
//!
//! The browser embeds guest RAM inside the wasm32 linear memory, but the Rust/WASM
//! runtime also uses that same linear memory for:
//! - stack
//! - statics/TLS
//! - heap allocations (`Vec`, `String`, wasm-bindgen shims, etc.)
//!
//! To avoid guest RAM corrupting runtime state (and vice-versa), the project
//! reserves a fixed low-address region for the runtime and maps guest physical
//! address 0 at `guest_base` (see ADR0003 addendum).
//!
//! This module enforces the contract by replacing Rust's default `dlmalloc`
//! allocator with a fixed-size allocator whose heap is bounded to the runtime
//! reserved region. If the runtime tries to allocate more than the reserved
//! bytes, allocations fail (panic/abort) instead of silently corrupting guest RAM.

#![cfg(target_arch = "wasm32")]

use core::alloc::{GlobalAlloc, Layout};
use core::cmp::min;
use core::sync::atomic::{AtomicU8, Ordering};
use linked_list_allocator::LockedHeap;

use crate::guest_layout::{RUNTIME_RESERVED_BYTES, WASM_PAGE_BYTES};

extern "C" {
    static __heap_base: u8;
}

struct RuntimeAllocator {
    heap: LockedHeap,
    // 0 = uninitialized, 1 = initializing, 2 = initialized.
    state: AtomicU8,
}

impl RuntimeAllocator {
    const fn new() -> Self {
        Self {
            heap: LockedHeap::empty(),
            state: AtomicU8::new(0),
        }
    }

    fn ensure_init(&self) {
        if self.state.load(Ordering::Acquire) == 2 {
            return;
        }

        if self
            .state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            unsafe { self.init() };
            self.state.store(2, Ordering::Release);
            return;
        }

        while self.state.load(Ordering::Acquire) != 2 {
            core::hint::spin_loop();
        }
    }

    unsafe fn init(&self) {
        // `&__heap_base as *const u8` yields the heap base pointer as a linear
        // memory address (wasm-ld provides it as a global).
        let heap_base = &__heap_base as *const u8 as usize;

        // Clamp the heap end to the *actual* current memory size so the allocator
        // remains safe even in non-worker contexts where the module is
        // instantiated with a small default memory.
        //
        // In the worker/shared-memory configuration, the imported memory is
        // allocated as `runtime_reserved + guest_size`, so this effectively caps
        // runtime allocations to `[heap_base, runtime_reserved)`.
        let pages = core::arch::wasm32::memory_size(0) as usize;
        let mem_bytes = pages * WASM_PAGE_BYTES as usize;
        let heap_end = min(mem_bytes, RUNTIME_RESERVED_BYTES as usize);

        if heap_end <= heap_base {
            // No heap available; leave uninitialized. Allocation will fail and
            // Rust will abort/panic rather than corrupt memory.
            return;
        }

        self.heap.lock().init(heap_base, heap_end - heap_base);
    }
}

unsafe impl GlobalAlloc for RuntimeAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.ensure_init();
        GlobalAlloc::alloc(&self.heap, layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.ensure_init();
        GlobalAlloc::dealloc(&self.heap, ptr, layout)
    }
}

// This is intentionally `cfg(target_arch = "wasm32")` only. Host builds (tests,
// tooling) should keep using the default system allocator.
#[global_allocator]
static GLOBAL_ALLOC: RuntimeAllocator = RuntimeAllocator::new();
