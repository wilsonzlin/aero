#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since UNIX epoch.
///
/// We use epoch time rather than `Instant` so events can be correlated across
/// threads/workers when the caller forwards them to the main thread.
#[cfg(target_arch = "wasm32")]
pub fn now_ms() -> u64 {
    // `std::time::SystemTime::now()` may panic on wasm32-unknown-unknown in some
    // configurations (notably when building `std` from source for shared-memory
    // / atomics support). Use JS `Date.now()` instead.
    //
    // `Date.now()` returns a `f64` millisecond count since UNIX epoch.
    let now = js_sys::Date::now();
    if now.is_finite() && now >= 0.0 {
        now.floor() as u64
    } else {
        0
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
