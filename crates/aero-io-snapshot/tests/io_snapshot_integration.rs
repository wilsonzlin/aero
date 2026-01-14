use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use aero_devices_input::I8042Controller;
use aero_io_snapshot::io::network::state::{Ipv4Addr, LegacyNetworkStackState, TcpRestorePolicy};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_io_snapshot::io::storage::state::{
    DiskBackend, DiskBackendState, DiskLayerState, LocalDiskBackendKind, LocalDiskBackendState,
};
use aero_storage::SECTOR_SIZE;

struct InMemoryDiskBackend {
    key: String,
    store: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
}

impl DiskBackend for InMemoryDiskBackend {
    fn read_at(&self, offset: u64, buf: &mut [u8]) {
        let store = self.store.lock().unwrap();
        let data = store.get(&self.key).unwrap();
        let start = offset as usize;
        buf.copy_from_slice(&data[start..start + buf.len()]);
    }

    fn write_at(&mut self, offset: u64, data: &[u8]) {
        let mut store = self.store.lock().unwrap();
        let disk = store.get_mut(&self.key).unwrap();
        let start = offset as usize;
        disk[start..start + data.len()].copy_from_slice(data);
    }

    fn flush(&mut self) {}
}

fn open_disk_backend(
    store: &Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    key: &str,
    size_bytes: u64,
) -> Box<dyn DiskBackend> {
    let mut guard = store.lock().unwrap();
    guard
        .entry(key.to_string())
        .or_insert_with(|| vec![0u8; size_bytes as usize]);
    Box::new(InMemoryDiskBackend {
        key: key.to_string(),
        store: store.clone(),
    })
}

#[test]
fn scripted_io_snapshot_restore() {
    // Script: disk write + keyboard + "network connect".
    let store: Arc<Mutex<BTreeMap<String, Vec<u8>>>> = Arc::new(Mutex::new(BTreeMap::new()));

    let mut disk = DiskLayerState::new(
        DiskBackendState::Local(LocalDiskBackendState {
            kind: LocalDiskBackendKind::Other,
            key: "disk0".to_string(),
            overlay: None,
        }),
        4096,
        SECTOR_SIZE,
    );
    let key = match &disk.backend {
        DiskBackendState::Local(local) => local.key.clone(),
        DiskBackendState::Remote(_) => unreachable!("test uses local backend"),
    };
    disk.attach_backend(open_disk_backend(&store, &key, disk.size_bytes));

    let mut i8042 = I8042Controller::new();
    i8042.inject_browser_key("KeyA", true);
    i8042.inject_browser_key("KeyA", false);

    let mut net = LegacyNetworkStackState::default();
    let conn_id = net.open_tcp_connection(Ipv4Addr::new(93, 184, 216, 34), 80);

    let mut sector = vec![0u8; SECTOR_SIZE];
    sector[..11].copy_from_slice(b"hello world");
    disk.write_sector(0, &sector);

    // Coordinator-level snapshot requirement: force storage flush.
    disk.flush();

    // Coordinator bundle format (deterministic TLV).
    let mut w = SnapshotWriter::new(*b"IOCO", SnapshotVersion::new(1, 0));
    w.field_bytes(1, disk.save_state());
    w.field_bytes(2, i8042.save_state());
    w.field_bytes(3, net.save_state());
    let bundle = w.finish();

    // Reset and restore.
    let r = SnapshotReader::parse(&bundle, *b"IOCO").unwrap();

    let mut disk2 = DiskLayerState::new(
        DiskBackendState::Local(LocalDiskBackendState {
            kind: LocalDiskBackendKind::Other,
            key: "ignored".to_string(),
            overlay: None,
        }),
        0,
        1,
    );
    disk2.load_state(r.bytes(1).unwrap()).unwrap();
    let key2 = match &disk2.backend {
        DiskBackendState::Local(local) => local.key.clone(),
        DiskBackendState::Remote(_) => unreachable!("test uses local backend"),
    };
    disk2.attach_backend(open_disk_backend(&store, &key2, disk2.size_bytes));

    let mut i8042_2 = I8042Controller::new();
    i8042_2.load_state(r.bytes(2).unwrap()).unwrap();

    let mut net2 = LegacyNetworkStackState::default();
    net2.load_state(r.bytes(3).unwrap()).unwrap();
    net2.apply_tcp_restore_policy(TcpRestorePolicy::Reconnect);

    // Disk write persisted via flush + reopenable backend.
    let read_back = disk2.read_sector(0);
    assert_eq!(&read_back[..11], b"hello world");

    // Pending keyboard bytes preserved.
    assert_eq!(i8042_2.read_port(0x60), 0x1e); // 'A' make (Set-1)
    assert_eq!(i8042_2.read_port(0x60), 0x9e); // 'A' break (Set-1)

    // Network connection survives as reconnecting (per policy).
    assert!(net2.tcp_proxy_conns.contains_key(&conn_id));
    assert_eq!(net2.tcp_proxy_conns[&conn_id].status as u8, 3);
}
