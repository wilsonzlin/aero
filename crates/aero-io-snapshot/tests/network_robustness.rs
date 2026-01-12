use aero_io_snapshot::io::network::state::LegacyNetworkStackState;
use aero_io_snapshot::io::network::state::{
    Ipv4Addr, NatKey, NatProtocol, NatValue, ProxyConnStatus, ProxyConnection,
};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};

#[test]
fn network_snapshot_rejects_excessive_nat_entry_count() {
    const TAG_NAT: u16 = 3;

    let mut w = SnapshotWriter::new(
        LegacyNetworkStackState::DEVICE_ID,
        LegacyNetworkStackState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_NAT, u32::MAX.to_le_bytes().to_vec());

    let mut state = LegacyNetworkStackState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject excessive NAT count");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("too many nat entries")
    );
}

#[test]
fn network_snapshot_rejects_excessive_tcp_connection_count() {
    const TAG_TCP_CONNS: u16 = 5;

    let mut w = SnapshotWriter::new(
        LegacyNetworkStackState::DEVICE_ID,
        LegacyNetworkStackState::DEVICE_VERSION,
    );
    w.field_bytes(TAG_TCP_CONNS, u32::MAX.to_le_bytes().to_vec());

    let mut state = LegacyNetworkStackState::default();
    let err = state
        .load_state(&w.finish())
        .expect_err("snapshot should reject excessive TCP connection count");
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("too many tcp proxy connections")
    );
}

#[test]
fn network_snapshot_save_state_clamps_nat_and_tcp_connections() {
    // Keep in sync with `aero-io-snapshot/src/io/network/state.rs`.
    const MAX_NAT_ENTRIES: usize = 65_536;
    const MAX_TCP_PROXY_CONNS: usize = 65_536;

    let mut state = LegacyNetworkStackState::default();

    // NAT table: insert MAX+1 entries. The extra entry uses a different inside_ip so that it sorts
    // *after* the first MAX entries, making it easy to assert it was dropped.
    for i in 0..=MAX_NAT_ENTRIES {
        let inside_ip = if i < MAX_NAT_ENTRIES {
            Ipv4Addr::new(10, 0, 2, 0)
        } else {
            Ipv4Addr::new(10, 0, 2, 1)
        };
        let port = (i % 65_536) as u16;
        state.nat.insert(
            NatKey {
                proto: NatProtocol::Tcp,
                inside_ip,
                inside_port: port,
                outside_port: port,
            },
            NatValue {
                remote_ip: Ipv4Addr::new(93, 184, 216, 34),
                remote_port: 80,
                last_seen_tick: i as u64,
            },
        );
    }

    // TCP proxy connections: same idea; insert MAX+1 so the largest ID can be dropped deterministically.
    for i in 0..=MAX_TCP_PROXY_CONNS {
        state.tcp_proxy_conns.insert(
            i as u32,
            ProxyConnection {
                id: i as u32,
                remote_ip: Ipv4Addr::new(1, 1, 1, 1),
                remote_port: 443,
                status: ProxyConnStatus::Connected,
            },
        );
    }

    let bytes = state.save_state();
    let mut decoded = LegacyNetworkStackState::default();
    decoded.load_state(&bytes).expect("snapshot should decode");

    assert_eq!(decoded.nat.len(), MAX_NAT_ENTRIES);
    assert!(
        !decoded
            .nat
            .keys()
            .any(|k| k.inside_ip == Ipv4Addr::new(10, 0, 2, 1)),
        "expected the extra NAT entry (inside_ip 10.0.2.1) to be dropped"
    );

    assert_eq!(decoded.tcp_proxy_conns.len(), MAX_TCP_PROXY_CONNS);
    assert!(
        !decoded
            .tcp_proxy_conns
            .contains_key(&(MAX_TCP_PROXY_CONNS as u32)),
        "expected the largest TCP connection ID to be dropped"
    );
}
