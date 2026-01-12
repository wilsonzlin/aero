#![cfg(target_arch = "wasm32")]
#![allow(dead_code)]

pub const WASM_PAGE_BYTES: u32 = 64 * 1024;
// Keep this in sync with `crates/aero-wasm/src/guest_layout.rs` (`RUNTIME_RESERVED_BYTES`) and
// `web/src/runtime/shared_layout.ts` (`RUNTIME_RESERVED_BYTES`).
pub const RUNTIME_RESERVED_BYTES: u32 = 128 * 1024 * 1024;

/// Allocate a guest RAM region for wasm-bindgen tests without consuming the wasm heap.
///
/// `aero-wasm` uses a fixed-size allocator (`runtime_alloc`) that intentionally bounds the heap to
/// the runtime-reserved region to prevent corruption of guest RAM. In `wasm-pack test` runs, the
/// default wasm linear memory can be small enough that allocating large `Vec<u8>` buffers (used by
/// UHCI tests as synthetic guest RAM) trips the allocator's OOM handler.
///
/// Instead, we grow the wasm linear memory and return the start offset of the newly added pages.
/// The returned region:
/// - is zero-initialized by `memory.grow`
/// - lives above the current heap, so it does not compete with allocator usage
/// - can be passed to `guest_base` parameters that expect a linear-memory address
pub fn alloc_guest_region_bytes(min_bytes: u32) -> (u32, u32) {
    let reserved_pages = RUNTIME_RESERVED_BYTES.div_ceil(WASM_PAGE_BYTES);
    let current_pages = core::arch::wasm32::memory_size(0) as u32;
    if current_pages < reserved_pages {
        let prev = core::arch::wasm32::memory_grow(0, (reserved_pages - current_pages) as usize);
        assert_ne!(
            prev,
            usize::MAX,
            "wasm memory.grow failed while reserving runtime heap (requested {} pages)",
            reserved_pages - current_pages
        );
    }

    let pages = min_bytes.div_ceil(WASM_PAGE_BYTES).max(1);
    let before_pages = core::arch::wasm32::memory_size(0) as u32;

    // `memory.grow` is safe and returns the previous size (or `usize::MAX` on failure). The newly
    // allocated pages are zeroed by the engine.
    let prev = core::arch::wasm32::memory_grow(0, pages as usize);
    assert_ne!(
        prev,
        usize::MAX,
        "wasm memory.grow failed (requested {pages} pages)"
    );

    let guest_base = before_pages * WASM_PAGE_BYTES;
    let guest_size = pages * WASM_PAGE_BYTES;
    (guest_base, guest_size)
}

/// Write a little-endian u32 to an absolute linear-memory address.
pub unsafe fn write_u32(addr: u32, value: u32) {
    unsafe {
        core::ptr::write_unaligned(addr as *mut u32, value);
    }
}

/// Read a little-endian u32 from an absolute linear-memory address.
pub unsafe fn read_u32(addr: u32) -> u32 {
    unsafe { core::ptr::read_unaligned(addr as *const u32) }
}

/// Write raw bytes to an absolute linear-memory address.
pub unsafe fn write_bytes(addr: u32, bytes: &[u8]) {
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as *mut u8, bytes.len());
    }
}
