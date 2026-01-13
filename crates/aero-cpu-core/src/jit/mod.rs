pub mod cache;
pub mod profile;
pub mod runtime;

/// Optional, low-overhead hook for embedders to collect JIT runtime metrics.
///
/// All methods must be infallible and cheap to call. The JIT runtime will only invoke these
/// methods when a sink is installed, and will not allocate on the hot path.
pub trait JitMetricsSink {
    fn record_cache_hit(&self);
    fn record_cache_miss(&self);
    fn record_install(&self);
    fn record_evict(&self, n: u64);
    fn record_invalidate(&self);
    fn record_stale_install_reject(&self);
    fn record_compile_request(&self);
    fn set_cache_bytes(&self, used: u64, capacity: u64);
}
