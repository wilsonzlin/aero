use aero_io_snapshot::io::state::IoSnapshot;
use aero_net_stack::{
    DnsCacheEntrySnapshot, NetworkStackSnapshotState, TcpConnectionSnapshot, TcpConnectionStatus,
};
use core::net::Ipv4Addr;

#[test]
fn snapshot_save_state_clamps_dns_cache_and_filters_oversized_names() {
    // Keep in sync with `aero-net-stack/src/snapshot.rs`.
    const MAX_DNS_CACHE_ENTRIES: usize = 65_536;
    const MAX_DNS_NAME_BYTES: usize = 1024;

    let mut state = NetworkStackSnapshotState::default();

    // Insert an entry with an oversized name (should be filtered by save_state).
    let oversized_name = "x".repeat(MAX_DNS_NAME_BYTES + 1);
    state.dns_cache.push(DnsCacheEntrySnapshot {
        name: oversized_name.clone(),
        addr: Ipv4Addr::new(1, 1, 1, 1),
        expires_at_ms: 123,
    });

    // Add MAX+1 valid entries so we can prove `save_state` clamps to MAX after filtering.
    for i in 0..(MAX_DNS_CACHE_ENTRIES + 1) {
        state.dns_cache.push(DnsCacheEntrySnapshot {
            name: format!("n{i}"),
            addr: Ipv4Addr::new(8, 8, 8, 8),
            expires_at_ms: 999_999,
        });
    }

    let bytes = state.save_state();
    let mut decoded = NetworkStackSnapshotState::default();
    decoded.load_state(&bytes).expect("snapshot should decode");

    assert_eq!(decoded.dns_cache.len(), MAX_DNS_CACHE_ENTRIES);
    assert_eq!(decoded.dns_cache[0].name, "n0");
    assert_eq!(
        decoded.dns_cache[MAX_DNS_CACHE_ENTRIES - 1].name,
        format!("n{}", MAX_DNS_CACHE_ENTRIES - 1)
    );
    assert!(
        !decoded.dns_cache.iter().any(|e| e.name == oversized_name),
        "oversized dns name should not be serialized"
    );
}

#[test]
fn snapshot_save_state_clamps_tcp_connection_bookkeeping() {
    // Keep in sync with `aero-net-stack/src/snapshot.rs`.
    const MAX_TCP_CONNECTIONS: usize = 65_536;

    let mut state = NetworkStackSnapshotState::default();

    // Use reverse order to ensure save_state sorting is actually applied.
    for i in (0..(MAX_TCP_CONNECTIONS + 10)).rev() {
        state.tcp_connections.push(TcpConnectionSnapshot {
            id: i as u32,
            guest_port: 40000,
            remote_ip: Ipv4Addr::new(93, 184, 216, 34),
            remote_port: 80,
            status: TcpConnectionStatus::Connected,
        });
    }

    let bytes = state.save_state();
    let mut decoded = NetworkStackSnapshotState::default();
    decoded.load_state(&bytes).expect("snapshot should decode");

    assert_eq!(decoded.tcp_connections.len(), MAX_TCP_CONNECTIONS);
    assert_eq!(decoded.tcp_connections[0].id, 0);
    assert_eq!(
        decoded.tcp_connections[MAX_TCP_CONNECTIONS - 1].id,
        (MAX_TCP_CONNECTIONS - 1) as u32
    );
}
