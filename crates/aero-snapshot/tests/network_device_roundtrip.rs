#![cfg(all(feature = "io-snapshot", not(target_arch = "wasm32")))]

use std::collections::HashMap;
use std::io::Cursor;

use aero_io_snapshot::io::state::IoSnapshot;
use aero_net_e1000::{E1000Device, MIN_L2_FRAME_LEN};
use aero_net_stack::packet::*;
use aero_net_stack::{Action, DnsResolved, NetworkStack, StackConfig};
use aero_snapshot::io_snapshot_bridge::{
    apply_io_snapshot_to_device, device_state_from_io_snapshot,
};
use aero_snapshot::{
    restore_snapshot, save_snapshot, Compression, CpuState, DeviceId, DeviceState, DiskOverlayRefs,
    MmuState, Result, SaveOptions, SnapshotError, SnapshotMeta, SnapshotSource, SnapshotTarget,
};
use core::net::Ipv4Addr;

const RAM_LEN: usize = 4096;

// E1000 register offsets (subset) used to perturb the device into a non-default state.
const REG_RCTL: u32 = 0x0100;
const REG_TCTL: u32 = 0x0400;
const REG_IMS: u32 = 0x00D0;

#[derive(Clone)]
struct DeviceStateSource {
    devices: Vec<DeviceState>,
    ram: Vec<u8>,
    meta: SnapshotMeta,
}

impl SnapshotSource for DeviceStateSource {
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
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        if offset + buf.len() > self.ram.len() {
            return Err(SnapshotError::Corrupt("ram read out of bounds"));
        }
        buf.copy_from_slice(&self.ram[offset..offset + buf.len()]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

struct TestSource {
    e1000: E1000Device,
    net_stack: NetworkStack,
    ram: Vec<u8>,
    meta: SnapshotMeta,
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
        vec![
            device_state_from_io_snapshot(DeviceId::E1000, &self.e1000),
            device_state_from_io_snapshot(DeviceId::NET_STACK, &self.net_stack),
        ]
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
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        if offset + buf.len() > self.ram.len() {
            return Err(SnapshotError::Corrupt("ram read out of bounds"));
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
    net_stack: NetworkStack,
    restored_states: Vec<DeviceState>,
    ram: Vec<u8>,
}

impl TestTarget {
    fn new(ram_len: usize) -> Self {
        let mut cfg = StackConfig::default();
        cfg.host_policy.enabled = true;
        Self {
            // These are deliberately initialized to defaults. Snapshot application should populate
            // the state we care about, and the test asserts the resulting TLV bytes match.
            e1000: E1000Device::new([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]),
            net_stack: NetworkStack::new(cfg),
            restored_states: Vec::new(),
            ram: vec![0u8; ram_len],
        }
    }
}

impl SnapshotTarget for TestTarget {
    fn restore_cpu_state(&mut self, _state: CpuState) {}

    fn restore_mmu_state(&mut self, _state: MmuState) {}

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        for state in states {
            if state.id == DeviceId::E1000 {
                apply_io_snapshot_to_device(&state, &mut self.e1000).unwrap();
            } else if state.id == DeviceId::NET_STACK {
                apply_io_snapshot_to_device(&state, &mut self.net_stack).unwrap();
            }
            self.restored_states.push(state);
        }
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        if offset + data.len() > self.ram.len() {
            return Err(SnapshotError::Corrupt("ram write out of bounds"));
        }
        self.ram[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }
}

fn snapshot_bytes<S: SnapshotSource>(source: &mut S, opts: SaveOptions) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, source, opts).unwrap();
    cursor.into_inner()
}

#[test]
fn networking_device_blobs_roundtrip_through_aero_snapshot_container() {
    // Build an E1000 with some non-default state.
    let mut e1000 = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
    e1000.mmio_write_u32_reg(REG_IMS, 0x1234_5678);
    e1000.mmio_write_u32_reg(REG_RCTL, 0xA5A5_A5A5);
    // Keep TCTL_EN clear to avoid starting TX processing (we just want a non-default register
    // value that should survive snapshotting).
    e1000.mmio_write_u32_reg(REG_TCTL, 0x5A5A_5A58);
    e1000.enqueue_rx_frame(vec![0x11u8; MIN_L2_FRAME_LEN]);

    let expected_e1000_mac = e1000.mac_addr();
    let expected_e1000_ims = e1000.mmio_read_u32(REG_IMS);
    let expected_e1000_rctl = e1000.mmio_read_u32(REG_RCTL);
    let expected_e1000_tctl = e1000.mmio_read_u32(REG_TCTL);

    // Build a user-space network stack with enough dynamic state to exercise encoding.
    let mut cfg = StackConfig::default();
    cfg.host_policy.enabled = true;
    let mut net_stack = NetworkStack::new(cfg);
    let guest_mac = MacAddr([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    dhcp_handshake(&mut net_stack, guest_mac);

    // Populate DNS cache.
    let dns_query = build_dns_query(0x1234, "example.com", DnsType::A as u16);
    let dns_frame = wrap_udp_ipv4_eth(
        guest_mac,
        net_stack.config().our_mac,
        net_stack.config().guest_ip,
        net_stack.config().dns_ip,
        53000,
        53,
        &dns_query,
    );
    let actions = net_stack.process_outbound_ethernet(&dns_frame, 1000);
    let (dns_req_id, name) = match actions.as_slice() {
        [Action::DnsResolve { request_id, name }] => (*request_id, name.clone()),
        _ => panic!("expected single DnsResolve action, got {actions:?}"),
    };
    let _ = net_stack.handle_dns_resolved(
        DnsResolved {
            request_id: dns_req_id,
            name,
            addr: Some(Ipv4Addr::new(93, 184, 216, 34)),
            ttl_secs: 60,
        },
        1001,
    );

    // Update the stack's internal time base so snapshots encode a meaningful `now_ms`.
    let _ = net_stack.process_outbound_ethernet(&dns_frame, 2500);

    // Expected device blobs before wrapping in the aero-snapshot container.
    let expected_e1000_state = device_state_from_io_snapshot(DeviceId::E1000, &e1000);
    let expected_net_stack_state = device_state_from_io_snapshot(DeviceId::NET_STACK, &net_stack);
    assert!(
        !expected_e1000_state.data.is_empty(),
        "unexpected empty E1000 io-snapshot blob"
    );
    assert!(
        !expected_net_stack_state.data.is_empty(),
        "unexpected empty NET_STACK io-snapshot blob"
    );

    let mut source = TestSource {
        e1000,
        net_stack,
        ram: vec![0u8; RAM_LEN],
        meta: SnapshotMeta::default(),
    };

    let mut opts = SaveOptions::default();
    opts.ram.compression = Compression::None;
    opts.ram.chunk_size = RAM_LEN as u32;

    // Determinism: saving twice from the same source should produce identical bytes.
    let bytes1 = snapshot_bytes(&mut source, opts);
    let bytes2 = snapshot_bytes(&mut source, opts);
    assert_eq!(bytes1, bytes2);

    // Determinism across device order: `save_snapshot` must canonicalize `DEVICES` ordering so the
    // outer container bytes are stable even if the source returns devices in a different order.
    let bytes_reversed_devices = snapshot_bytes(
        &mut DeviceStateSource {
            devices: vec![
                expected_net_stack_state.clone(),
                expected_e1000_state.clone(),
            ],
            ram: vec![0u8; RAM_LEN],
            meta: SnapshotMeta::default(),
        },
        opts,
    );
    assert_eq!(bytes_reversed_devices, bytes1);

    let mut target = TestTarget::new(RAM_LEN);
    restore_snapshot(&mut Cursor::new(bytes1.as_slice()), &mut target).unwrap();

    // Verify the outer aero-snapshot container preserved the blobs exactly.
    assert_eq!(target.restored_states.len(), 2);
    let mut restored: HashMap<DeviceId, DeviceState> = HashMap::new();
    for st in &target.restored_states {
        restored.insert(st.id, st.clone());
    }
    assert!(restored.contains_key(&DeviceId::E1000));
    assert!(restored.contains_key(&DeviceId::NET_STACK));
    assert_eq!(restored.len(), 2);

    assert_eq!(restored[&DeviceId::E1000], expected_e1000_state);
    assert_eq!(restored[&DeviceId::NET_STACK], expected_net_stack_state);

    // Preferred: ensure the blobs can be applied back to fresh devices and reproduce the same TLV.
    assert!(target.net_stack.is_ip_assigned());
    assert_eq!(target.e1000.mac_addr(), expected_e1000_mac);
    assert_eq!(target.e1000.mmio_read_u32(REG_IMS), expected_e1000_ims);
    assert_eq!(target.e1000.mmio_read_u32(REG_RCTL), expected_e1000_rctl);
    assert_eq!(target.e1000.mmio_read_u32(REG_TCTL), expected_e1000_tctl);

    let e1000_resaved = device_state_from_io_snapshot(DeviceId::E1000, &target.e1000);
    assert_eq!(e1000_resaved.data, expected_e1000_state.data);

    let net_stack_resaved = device_state_from_io_snapshot(DeviceId::NET_STACK, &target.net_stack);
    assert_eq!(net_stack_resaved.data, expected_net_stack_state.data);

    // Full file roundtrip: after restoring into concrete devices, re-saving a snapshot should
    // reproduce the exact same container bytes.
    let TestTarget {
        e1000,
        net_stack,
        ram,
        restored_states: _,
    } = target;
    let bytes3 = snapshot_bytes(
        &mut TestSource {
            e1000,
            net_stack,
            ram,
            meta: SnapshotMeta::default(),
        },
        opts,
    );
    assert_eq!(bytes3, bytes1);

    // Sanity: these are true aero-io-snapshot TLV blobs.
    assert_eq!(&e1000_resaved.data[..4], b"AERO");
    assert_eq!(&net_stack_resaved.data[..4], b"AERO");
    // Double-check the trait is actually in use (not an accidental dummy blob).
    assert_eq!(<NetworkStack as IoSnapshot>::DEVICE_ID, *b"NETS");
}

fn dhcp_handshake(stack: &mut NetworkStack, guest_mac: MacAddr) {
    let xid = 0x1020_3040;
    let discover = build_dhcp_discover(xid, guest_mac);
    let discover_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &discover,
    );

    // Offer.
    let _ = stack.process_outbound_ethernet(&discover_frame, 0);

    let request = build_dhcp_request(
        xid,
        guest_mac,
        stack.config().guest_ip,
        stack.config().gateway_ip,
    );
    let request_frame = wrap_udp_ipv4_eth(
        guest_mac,
        MacAddr::BROADCAST,
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        68,
        67,
        &request,
    );

    // Ack.
    let _ = stack.process_outbound_ethernet(&request_frame, 1);
    assert!(stack.is_ip_assigned());
}

fn wrap_udp_ipv4_eth(
    src_mac: MacAddr,
    dst_mac: MacAddr,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp = UdpPacketBuilder {
        src_port,
        dst_port,
        payload,
    }
    .build_vec(src_ip, dst_ip)
    .unwrap();
    let ip = Ipv4PacketBuilder {
        dscp_ecn: 0,
        identification: 1,
        flags_fragment: 0x4000, // DF
        ttl: 64,
        protocol: Ipv4Protocol::UDP,
        src_ip,
        dst_ip,
        options: &[],
        payload: &udp,
    }
    .build_vec()
    .unwrap();
    EthernetFrameBuilder {
        dest_mac: dst_mac,
        src_mac,
        ethertype: EtherType::IPV4,
        payload: &ip,
    }
    .build_vec()
    .unwrap()
}

fn build_dhcp_discover(xid: u32, mac: MacAddr) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1; // Ethernet
    out[2] = 6; // MAC len
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast
    out[28..34].copy_from_slice(&mac.0);
    out[236..240].copy_from_slice(&[99, 130, 83, 99]);
    out.extend_from_slice(&[53, 1, 1]); // DHCPDISCOVER
    out.push(255);
    out
}

fn build_dhcp_request(
    xid: u32,
    mac: MacAddr,
    requested_ip: Ipv4Addr,
    server_id: Ipv4Addr,
) -> Vec<u8> {
    let mut out = vec![0u8; 240];
    out[0] = 1; // BOOTREQUEST
    out[1] = 1;
    out[2] = 6;
    out[4..8].copy_from_slice(&xid.to_be_bytes());
    out[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
    out[28..34].copy_from_slice(&mac.0);
    out[236..240].copy_from_slice(&[99, 130, 83, 99]);
    out.extend_from_slice(&[53, 1, 3]); // DHCPREQUEST
    out.extend_from_slice(&[50, 4]);
    out.extend_from_slice(&requested_ip.octets());
    out.extend_from_slice(&[54, 4]);
    out.extend_from_slice(&server_id.octets());
    out.push(255);
    out
}

fn build_dns_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    for label in name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out
}
