use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use serde::Serialize;

#[derive(Clone)]
pub struct Stats {
    inner: Arc<StatsInner>,
}

struct StatsInner {
    started_at: Instant,
    next_connection_id: AtomicU64,
    active_tcp_connections: AtomicU64,
    bytes_client_to_target_total: AtomicU64,
    bytes_target_to_client_total: AtomicU64,
}

impl Stats {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(StatsInner {
                started_at: Instant::now(),
                next_connection_id: AtomicU64::new(1),
                active_tcp_connections: AtomicU64::new(0),
                bytes_client_to_target_total: AtomicU64::new(0),
                bytes_target_to_client_total: AtomicU64::new(0),
            }),
        }
    }

    pub fn next_connection_id(&self) -> u64 {
        self.inner
            .next_connection_id
            .fetch_add(1, Ordering::Relaxed)
    }

    pub fn tcp_connection_opened(&self) {
        self.inner
            .active_tcp_connections
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn tcp_connection_closed(&self) {
        self.inner
            .active_tcp_connections
            .fetch_sub(1, Ordering::Relaxed);
    }

    pub fn add_bytes_client_to_target(&self, bytes: u64) {
        self.inner
            .bytes_client_to_target_total
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn add_bytes_target_to_client(&self, bytes: u64) {
        self.inner
            .bytes_target_to_client_total
            .fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn snapshot(&self, dns_cache_size: usize) -> StatsSnapshot {
        let bytes_client_to_target_total = self
            .inner
            .bytes_client_to_target_total
            .load(Ordering::Relaxed);
        let bytes_target_to_client_total = self
            .inner
            .bytes_target_to_client_total
            .load(Ordering::Relaxed);

        StatsSnapshot {
            active_tcp_connections: self.inner.active_tcp_connections.load(Ordering::Relaxed),
            bytes_client_to_target_total,
            bytes_target_to_client_total,
            bytes_total: bytes_client_to_target_total + bytes_target_to_client_total,
            dns_cache_size,
            uptime_seconds: self.inner.started_at.elapsed().as_secs(),
            version: env!("CARGO_PKG_VERSION"),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StatsSnapshot {
    pub active_tcp_connections: u64,
    pub bytes_client_to_target_total: u64,
    pub bytes_target_to_client_total: u64,
    pub bytes_total: u64,
    pub dns_cache_size: usize,
    pub uptime_seconds: u64,
    pub version: &'static str,
}
