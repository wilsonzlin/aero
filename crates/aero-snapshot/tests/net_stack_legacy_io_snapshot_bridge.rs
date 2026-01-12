#![cfg(all(feature = "io-snapshot", not(target_arch = "wasm32")))]

use aero_io_snapshot::io::state::IoSnapshot;
use aero_net_stack::packet::MacAddr;
use aero_net_stack::{
    DnsCacheEntrySnapshot, NetworkStackSnapshotState, TcpConnectionSnapshot, TcpConnectionStatus,
};
use aero_snapshot::io_snapshot_bridge::apply_io_snapshot_to_device;
use aero_snapshot::{DeviceId, DeviceState};

#[test]
fn apply_io_snapshot_to_device_accepts_legacy_net_stack_header_id() {
    // A non-default state so we prove we really decoded something (not just the all-zero default).
    let expected = NetworkStackSnapshotState {
        guest_mac: Some(MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x02])),
        ip_assigned: true,
        next_tcp_id: 42,
        next_dns_id: 7,
        ipv4_ident: 123,
        last_now_ms: 1_000,
        dns_cache: vec![DnsCacheEntrySnapshot {
            name: "example.com".to_string(),
            addr: core::net::Ipv4Addr::new(93, 184, 216, 34),
            expires_at_ms: 2_000,
        }],
        tcp_connections: vec![TcpConnectionSnapshot {
            id: 1,
            guest_port: 1234,
            remote_ip: core::net::Ipv4Addr::new(1, 1, 1, 1),
            remote_port: 80,
            // `load_state` always restores connections as disconnected; keep this aligned so
            // save->load->save remains deterministic.
            status: TcpConnectionStatus::Disconnected,
        }],
    };

    let mut bytes = expected.save_state();
    assert_eq!(&bytes[0..4], b"AERO");
    assert_eq!(&bytes[8..12], b"NETS");

    // Older snapshots used a different 4CC in the `aero-io-snapshot` header for this device.
    // Ensure the `aero-snapshot` io-snapshot bridge still accepts that encoding.
    const LEGACY_NET_STACK_DEVICE_ID: [u8; 4] = [0x4e, 0x53, 0x54, 0x4b];
    bytes[8..12].copy_from_slice(&LEGACY_NET_STACK_DEVICE_ID);

    let state = DeviceState {
        id: DeviceId::NET_STACK,
        version: <NetworkStackSnapshotState as IoSnapshot>::DEVICE_VERSION.major,
        flags: <NetworkStackSnapshotState as IoSnapshot>::DEVICE_VERSION.minor,
        data: bytes,
    };

    let mut restored = NetworkStackSnapshotState::default();
    apply_io_snapshot_to_device(&state, &mut restored).expect("legacy header should decode");
    assert_eq!(restored, expected);

    // Re-saving always uses the canonical 4CC.
    let resaved = restored.save_state();
    assert_eq!(&resaved[8..12], b"NETS");
}
