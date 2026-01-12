#![cfg(all(feature = "io-snapshot", not(target_arch = "wasm32")))]

use std::io::Cursor;

use aero_net_e1000::{E1000Device, MIN_L2_FRAME_LEN};
use aero_net_stack::packet::MacAddr;
use aero_net_stack::{
    DnsCacheEntrySnapshot, NetworkStackSnapshotState, TcpConnectionSnapshot, TcpConnectionStatus,
};
use aero_snapshot::io_snapshot_bridge::{apply_io_snapshot_to_device, device_state_from_io_snapshot};
use aero_snapshot::{
    restore_snapshot, save_snapshot, Compression, CpuState, DeviceId, DeviceState, DiskOverlayRefs,
    MmuState, Result, SaveOptions, SnapshotError as AeroSnapshotError, SnapshotMeta, SnapshotSource,
    SnapshotTarget,
};

// E1000 register offsets (subset) used to perturb the device into a non-default state.
const REG_IMS: u32 = 0x00D0;
const REG_ICS: u32 = 0x00C8;
const REG_RCTL: u32 = 0x0100;

fn make_deterministic_e1000_state() -> E1000Device {
    let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

    // Some representative register state.
    dev.mmio_write_u32_reg(REG_IMS, 0xFFFF_FFFF);
    dev.mmio_write_u32_reg(REG_ICS, 0x0000_0080); // RXT0 cause => irq_level becomes true.
    dev.mmio_write_u32_reg(REG_RCTL, 0xA5A5_A5A5);

    // Touch the IOADDR latch.
    dev.io_write_reg(0x0, 4, 0x1234);

    // Queue one pending RX frame.
    dev.enqueue_rx_frame(vec![0x11u8; MIN_L2_FRAME_LEN]);

    dev
}

fn make_deterministic_net_stack_state() -> NetworkStackSnapshotState {
    NetworkStackSnapshotState {
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
    }
}

#[derive(Clone)]
struct TestSource {
    meta: SnapshotMeta,
    devices: Vec<DeviceState>,
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
        self.devices.clone()
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
    e1000: E1000Device,
    net_stack: NetworkStackSnapshotState,
    device_states: Vec<DeviceState>,
    ram: Vec<u8>,
}

impl TestTarget {
    fn new(ram_len: usize) -> Self {
        Self {
            e1000: E1000Device::new([0; 6]),
            net_stack: NetworkStackSnapshotState::default(),
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
    let expected_e1000_state = device_state_from_io_snapshot(DeviceId::E1000, &e1000);
    let expected_net_stack_state = device_state_from_io_snapshot(DeviceId::NET_STACK, &net_stack);

    let meta = SnapshotMeta {
        snapshot_id: 1,
        parent_snapshot_id: None,
        created_unix_ms: 0,
        label: None,
    };

    let mut save_opts = SaveOptions::default();
    save_opts.ram.compression = Compression::None;
    save_opts.ram.chunk_size = 4096;

    // Intentionally return the devices in reverse order. The snapshot container must canonicalize
    // ordering so the output bytes are deterministic.
    let mut source = TestSource {
        meta: meta.clone(),
        devices: vec![expected_net_stack_state.clone(), expected_e1000_state.clone()],
        ram: Vec::new(),
    };

    let bytes1 = {
        let mut cursor = Cursor::new(Vec::new());
        save_snapshot(&mut cursor, &mut source, save_opts).unwrap();
        cursor.into_inner()
    };

    // Regression: device ordering in the container should be stable regardless of the ordering
    // returned by `SnapshotSource::device_states`.
    let bytes_sorted_devices = {
        let mut cursor = Cursor::new(Vec::new());
        let mut source_sorted = TestSource {
            meta: meta.clone(),
            devices: vec![expected_e1000_state.clone(), expected_net_stack_state.clone()],
            ram: Vec::new(),
        };
        save_snapshot(&mut cursor, &mut source_sorted, save_opts).unwrap();
        cursor.into_inner()
    };
    assert_eq!(bytes_sorted_devices, bytes1);

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
    let e1000_resaved = device_state_from_io_snapshot(DeviceId::E1000, &target.e1000);
    let net_stack_resaved = device_state_from_io_snapshot(DeviceId::NET_STACK, &target.net_stack);
    assert_eq!(e1000_resaved, expected_e1000_state);
    assert_eq!(net_stack_resaved, expected_net_stack_state);

    // Extra sanity: apply the restored blobs into fresh devices without error.
    let mut e1000_fresh = E1000Device::new([0; 6]);
    let mut net_stack_fresh = NetworkStackSnapshotState::default();
    for state in &target.device_states {
        if state.id == DeviceId::E1000 {
            apply_io_snapshot_to_device(state, &mut e1000_fresh).unwrap();
        } else if state.id == DeviceId::NET_STACK {
            apply_io_snapshot_to_device(state, &mut net_stack_fresh).unwrap();
        }
    }
    assert_eq!(
        device_state_from_io_snapshot(DeviceId::E1000, &e1000_fresh),
        expected_e1000_state
    );
    assert_eq!(
        device_state_from_io_snapshot(DeviceId::NET_STACK, &net_stack_fresh),
        expected_net_stack_state
    );

    // Re-save from the restored device state and ensure byte-for-byte determinism.
    let mut source2 = TestSource {
        meta,
        devices: vec![net_stack_resaved, e1000_resaved],
        ram: Vec::new(),
    };
    let bytes2 = {
        let mut cursor = Cursor::new(Vec::new());
        save_snapshot(&mut cursor, &mut source2, save_opts).unwrap();
        cursor.into_inner()
    };

    assert_eq!(bytes2, bytes1);
}
