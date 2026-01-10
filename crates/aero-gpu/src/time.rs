use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since UNIX epoch.
///
/// We use epoch time rather than `Instant` so events can be correlated across
/// threads/workers when the caller forwards them to the main thread.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

