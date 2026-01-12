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

// Reserve a tiny tail region at the end of the runtime heap so JS/WASM can safely use a
// deterministic scratch word for linear-memory wiring probes (and other tiny out-of-band
// instrumentation) without risking clobbering a real Rust allocation.
//
// This is intentionally small (<< 1 page) so it doesn't materially reduce available heap.
const HEAP_TAIL_GUARD_BYTES: usize = 64;

// Ensure the tail guard is large enough for the JS-side memory wiring probes.
//
// `web/src/runtime/wasm_memory_probe.ts` spreads probe contexts across a 16-word window (64 bytes)
// to reduce cross-worker races, so reserve at least that much space at the end of the runtime
// reserved region.
const _: () = assert!(HEAP_TAIL_GUARD_BYTES >= 64);

unsafe extern "C" {
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
        let heap_base = unsafe { &__heap_base as *const u8 as usize };

        // wasm-bindgen tests (and other non-worker contexts) may instantiate the module with a
        // small default linear memory. The runtime/guest layout contract assumes at least
        // `RUNTIME_RESERVED_BYTES` is available for the Rust runtime region, so attempt to grow
        // memory up to that floor before initializing the heap.
        //
        // If memory cannot be grown (e.g. imported with a fixed max), we fall back to the current
        // size and keep the allocator bounded to avoid out-of-bounds access.
        let page_bytes = WASM_PAGE_BYTES as usize;
        let reserved_bytes = RUNTIME_RESERVED_BYTES as usize;
        let cur_pages = core::arch::wasm32::memory_size(0) as usize;
        let cur_bytes = cur_pages.saturating_mul(page_bytes);
        if cur_bytes < reserved_bytes {
            let desired_pages = reserved_bytes.div_ceil(page_bytes);
            let delta_pages = desired_pages.saturating_sub(cur_pages);
            if delta_pages > 0 {
                // `memory_grow` returns the previous page count, or `usize::MAX` on failure.
                // Ignore failures: we will clamp below based on the actual resulting size.
                let _ = core::arch::wasm32::memory_grow(0, delta_pages);
            }
        }

        // Clamp the heap end to the *actual* current memory size so the allocator
        // remains safe even in non-worker contexts where the module is
        // instantiated with a small default memory.
        //
        // In the worker/shared-memory configuration, the imported memory is
        // allocated as `runtime_reserved + guest_size`, so this effectively caps
        // runtime allocations to `[heap_base, runtime_reserved)`.
        let pages = core::arch::wasm32::memory_size(0) as usize;
        let mem_bytes = pages.saturating_mul(page_bytes);
        // Keep a small guard at the end so probes can safely touch e.g. the last 4 bytes
        // of the runtime-reserved region (immediately below guest RAM) without overlapping
        // the Rust allocator's usable heap range.
        let heap_end = min(mem_bytes, reserved_bytes).saturating_sub(HEAP_TAIL_GUARD_BYTES);

        if heap_end <= heap_base {
            // No heap available; leave uninitialized. Allocation will fail and
            // Rust will abort/panic rather than corrupt memory.
            return;
        }

        unsafe {
            self.heap
                .lock()
                .init(heap_base as *mut u8, heap_end - heap_base);
        }
    }
}

unsafe impl GlobalAlloc for RuntimeAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.ensure_init();
        unsafe { GlobalAlloc::alloc(&self.heap, layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.ensure_init();
        unsafe { GlobalAlloc::dealloc(&self.heap, ptr, layout) }
    }
}

// This is intentionally `cfg(target_arch = "wasm32")` only. Host builds (tests,
// tooling) should keep using the default system allocator.
#[global_allocator]
static GLOBAL_ALLOC: RuntimeAllocator = RuntimeAllocator::new();
