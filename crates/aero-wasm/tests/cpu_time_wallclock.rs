#![cfg(target_arch = "wasm32")]

use aero_cpu_core::time::{DEFAULT_TSC_HZ, TimeSource};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen_test]
fn cpu_time_wallclock_mode_does_not_panic_on_wasm() {
    // `std::time::Instant::now()` panics on wasm32-unknown-unknown. This exercise ensures our
    // wallclock time source uses a wasm-safe `Instant` implementation.
    let mut time = TimeSource::new_wallclock(DEFAULT_TSC_HZ);
    let t0 = time.read_tsc();
    let t1 = time.read_tsc();
    assert!(t1 >= t0);
}
