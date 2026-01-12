#![cfg(all(feature = "io-snapshot", not(target_arch = "wasm32")))]

use std::collections::BTreeMap;
use std::io::Cursor;

use aero_io_snapshot::io::network::state::{
    DhcpLease, Ipv4Addr, LegacyNetworkStackState, NatKey, NatProtocol, NatValue, ProxyConnStatus,
    ProxyConnection,
};
use aero_io_snapshot::io::state::codec::Decoder;
use aero_io_snapshot::io::state::{codec::Encoder, IoSnapshot, SnapshotError, SnapshotReader};
use aero_snapshot::io_snapshot_bridge::{apply_io_snapshot_to_device, device_state_from_io_snapshot};
use aero_snapshot::{
    restore_snapshot, save_snapshot, Compression, CpuState, DeviceId, DeviceState, DiskOverlayRefs,
    MmuState, Result, SaveOptions, SnapshotError as AeroSnapshotError, SnapshotMeta, SnapshotSource,
    SnapshotTarget,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct E1000DummyState {
    mac_addr: [u8; 6],
    irq_level: bool,
    rx_pending: Vec<Vec<u8>>,
    tx_out: Vec<Vec<u8>>,
    regs: BTreeMap<u32, u32>,
}

impl Default for E1000DummyState {
    fn default() -> Self {
        Self {
            mac_addr: [0; 6],
            irq_level: false,
            rx_pending: Vec::new(),
            tx_out: Vec::new(),
            regs: BTreeMap::new(),
        }
    }
}

impl IoSnapshot for E1000DummyState {
    const DEVICE_ID: [u8; 4] = *b"E1K0";
    const DEVICE_VERSION: aero_io_snapshot::io::state::SnapshotVersion =
        aero_io_snapshot::io::state::SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_MAC: u16 = 1;
        const TAG_IRQ_LEVEL: u16 = 2;
        const TAG_RX_PENDING: u16 = 3;
        const TAG_TX_OUT: u16 = 4;
        const TAG_REGS: u16 = 5;

        let mut w = aero_io_snapshot::io::state::SnapshotWriter::new(
            Self::DEVICE_ID,
            Self::DEVICE_VERSION,
        );
        w.field_bytes(TAG_MAC, self.mac_addr.to_vec());
        w.field_bool(TAG_IRQ_LEVEL, self.irq_level);
        w.field_bytes(
            TAG_RX_PENDING,
            Encoder::new().vec_bytes(&self.rx_pending).finish(),
        );
        w.field_bytes(TAG_TX_OUT, Encoder::new().vec_bytes(&self.tx_out).finish());

        let mut regs = Encoder::new().u32(self.regs.len() as u32);
        for (k, v) in &self.regs {
            regs = regs.u32(*k).u32(*v);
        }
        w.field_bytes(TAG_REGS, regs.finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> aero_io_snapshot::io::state::SnapshotResult<()> {
        const TAG_MAC: u16 = 1;
        const TAG_IRQ_LEVEL: u16 = 2;
        const TAG_RX_PENDING: u16 = 3;
        const TAG_TX_OUT: u16 = 4;
        const TAG_REGS: u16 = 5;

        const MAX_QUEUE_ENTRIES: usize = 1024;
        const MAX_REGS: usize = 8192;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(mac) = r.bytes(TAG_MAC) {
            if mac.len() != 6 {
                return Err(SnapshotError::InvalidFieldEncoding("mac"));
            }
            self.mac_addr.copy_from_slice(mac);
        }
        self.irq_level = r.bool(TAG_IRQ_LEVEL)?.unwrap_or(false);

        self.rx_pending.clear();
        if let Some(buf) = r.bytes(TAG_RX_PENDING) {
            let mut d = Decoder::new(buf);
            let queue = d.vec_bytes()?;
            if queue.len() > MAX_QUEUE_ENTRIES {
                return Err(SnapshotError::InvalidFieldEncoding("rx queue too large"));
            }
            self.rx_pending = queue;
            d.finish()?;
        }

        self.tx_out.clear();
        if let Some(buf) = r.bytes(TAG_TX_OUT) {
            let mut d = Decoder::new(buf);
            let queue = d.vec_bytes()?;
            if queue.len() > MAX_QUEUE_ENTRIES {
                return Err(SnapshotError::InvalidFieldEncoding("tx queue too large"));
            }
            self.tx_out = queue;
            d.finish()?;
        }

        self.regs.clear();
        if let Some(buf) = r.bytes(TAG_REGS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_REGS {
                return Err(SnapshotError::InvalidFieldEncoding("too many regs"));
            }
            for _ in 0..count {
                let k = d.u32()?;
                let v = d.u32()?;
                self.regs.insert(k, v);
            }
            d.finish()?;
        }

        Ok(())
    }
}

fn make_deterministic_e1000_state() -> E1000DummyState {
    let mut regs = BTreeMap::new();
    regs.insert(0x00, 0xDEAD_BEEF);
    regs.insert(0x04, 0x1234_5678);

    E1000DummyState {
        mac_addr: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
        irq_level: true,
        rx_pending: vec![b"rx0".to_vec(), b"rx1".to_vec()],
        tx_out: vec![b"tx0".to_vec()],
        regs,
    }
}

fn make_deterministic_net_stack_state() -> LegacyNetworkStackState {
    let mut nat = BTreeMap::new();
    nat.insert(
        NatKey {
            proto: NatProtocol::Tcp,
            inside_ip: Ipv4Addr::new(10, 0, 2, 15),
            inside_port: 1234,
            outside_port: 40000,
        },
        NatValue {
            remote_ip: Ipv4Addr::new(1, 1, 1, 1),
            remote_port: 80,
            last_seen_tick: 42,
        },
    );

    let mut tcp_proxy_conns = BTreeMap::new();
    tcp_proxy_conns.insert(
        7,
        ProxyConnection {
            id: 7,
            remote_ip: Ipv4Addr::new(203, 0, 113, 1),
            remote_port: 443,
            // NetworkStackState::load_state always restores as Disconnected, regardless of the
            // saved status. Keep the initial state aligned so save->load->save is deterministic.
            status: ProxyConnStatus::Disconnected,
        },
    );

    LegacyNetworkStackState {
        mac_addr: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
        dhcp_lease: Some(DhcpLease {
            ip: Ipv4Addr::new(10, 0, 2, 15),
            gateway: Ipv4Addr::new(10, 0, 2, 2),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            lease_time_secs: 3600,
            acquired_at_tick: 123,
        }),
        nat,
        next_conn_id: 8,
        tcp_proxy_conns,
    }
}

#[derive(Clone)]
struct TestSource {
    meta: SnapshotMeta,
    e1000: E1000DummyState,
    net_stack: LegacyNetworkStackState,
    ram: Vec<u8>,
}

impl SnapshotSource for TestSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        self.meta.clone()
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        // Construct canonical device states, then intentionally reverse the ordering so the
        // snapshot writer must re-sort them deterministically.
        let mut states = vec![
            device_state_from_io_snapshot(DeviceId::E1000, &self.e1000),
            device_state_from_io_snapshot(DeviceId::NET_STACK, &self.net_stack),
        ];
        states.sort_by_key(|dev| (dev.id.0, dev.version, dev.flags));
        states.reverse();
        states
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| AeroSnapshotError::Corrupt("ram offset overflow"))?;
        if offset + buf.len() > self.ram.len() {
            return Err(AeroSnapshotError::Corrupt("ram read out of bounds"));
        }
        buf.copy_from_slice(&self.ram[offset..offset + buf.len()]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

struct TestTarget {
    e1000: E1000DummyState,
    net_stack: LegacyNetworkStackState,
    device_states: Vec<DeviceState>,
    ram: Vec<u8>,
}

impl TestTarget {
    fn new(ram_len: usize) -> Self {
        Self {
            e1000: E1000DummyState::default(),
            net_stack: LegacyNetworkStackState::default(),
            device_states: Vec::new(),
            ram: vec![0u8; ram_len],
        }
    }
}

impl SnapshotTarget for TestTarget {
    fn restore_cpu_state(&mut self, _state: CpuState) {}

    fn restore_mmu_state(&mut self, _state: MmuState) {}

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        self.device_states = states.clone();
        for state in &states {
            if state.id == DeviceId::E1000 {
                apply_io_snapshot_to_device(state, &mut self.e1000).unwrap();
            } else if state.id == DeviceId::NET_STACK {
                apply_io_snapshot_to_device(state, &mut self.net_stack).unwrap();
            }
        }
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| AeroSnapshotError::Corrupt("ram offset overflow"))?;
        if offset + data.len() > self.ram.len() {
            return Err(AeroSnapshotError::Corrupt("ram write out of bounds"));
        }
        self.ram[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }
}

#[test]
fn network_device_blobs_roundtrip_is_deterministic_and_ordered() {
    let e1000 = make_deterministic_e1000_state();
    let net_stack = make_deterministic_net_stack_state();

    let meta = SnapshotMeta {
        snapshot_id: 1,
        parent_snapshot_id: None,
        created_unix_ms: 0,
        label: None,
    };

    let mut save_opts = SaveOptions::default();
    save_opts.ram.compression = Compression::None;
    save_opts.ram.chunk_size = 4096;

    let mut source = TestSource {
        meta: meta.clone(),
        e1000: e1000.clone(),
        net_stack: net_stack.clone(),
        ram: Vec::new(),
    };

    let bytes1 = {
        let mut cursor = Cursor::new(Vec::new());
        save_snapshot(&mut cursor, &mut source, save_opts).unwrap();
        cursor.into_inner()
    };

    let mut target = TestTarget::new(0);
    restore_snapshot(&mut Cursor::new(&bytes1), &mut target).unwrap();

    assert_eq!(target.device_states.len(), 2);
    assert!(
        target.device_states.iter().any(|s| s.id == DeviceId::E1000),
        "restored snapshot missing E1000 device entry"
    );
    assert!(
        target
            .device_states
            .iter()
            .any(|s| s.id == DeviceId::NET_STACK),
        "restored snapshot missing NET_STACK device entry"
    );

    // Regression: device ordering should be canonical/stable in the snapshot output regardless of
    // the input ordering returned by SnapshotSource::device_states.
    let mut expected = target.device_states.clone();
    expected.sort_by_key(|dev| (dev.id.0, dev.version, dev.flags));
    assert_eq!(
        target.device_states, expected,
        "device ordering should be sorted by (id, version, flags)"
    );

    // The restored blobs should be applicable to fresh devices without error. Our SnapshotTarget
    // applies them immediately; additionally assert the in-memory state is exactly restored.
    assert_eq!(target.e1000, e1000);
    assert_eq!(target.net_stack, net_stack);

    // Re-save from the restored device state and ensure byte-for-byte determinism.
    let mut source2 = TestSource {
        meta,
        e1000: target.e1000.clone(),
        net_stack: target.net_stack.clone(),
        ram: Vec::new(),
    };
    let bytes2 = {
        let mut cursor = Cursor::new(Vec::new());
        save_snapshot(&mut cursor, &mut source2, save_opts).unwrap();
        cursor.into_inner()
    };

    assert_eq!(bytes2, bytes1);
}
