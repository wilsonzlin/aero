#![cfg(not(target_arch = "wasm32"))]

use aero_snapshot as snapshot;
use std::io::Cursor;

fn push_section(
    dst: &mut Vec<u8>,
    id: snapshot::SectionId,
    version: u16,
    flags: u16,
    payload: &[u8],
) {
    dst.extend_from_slice(&id.0.to_le_bytes());
    dst.extend_from_slice(&version.to_le_bytes());
    dst.extend_from_slice(&flags.to_le_bytes());
    dst.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    dst.extend_from_slice(payload);
}

#[derive(Default)]
struct CaptureTarget {
    mmus: Option<Vec<snapshot::VcpuMmuSnapshot>>,
}

impl snapshot::SnapshotTarget for CaptureTarget {
    fn restore_cpu_state(&mut self, _state: snapshot::CpuState) {}

    fn restore_cpu_states(&mut self, _states: Vec<snapshot::VcpuSnapshot>) -> snapshot::Result<()> {
        Ok(())
    }

    fn restore_mmu_state(&mut self, _state: snapshot::MmuState) {
        panic!("restore_mmu_state should not be called for this test");
    }

    fn restore_mmu_states(
        &mut self,
        states: Vec<snapshot::VcpuMmuSnapshot>,
    ) -> snapshot::Result<()> {
        self.mmus = Some(states);
        Ok(())
    }

    fn restore_device_states(&mut self, _states: Vec<snapshot::DeviceState>) {}

    fn restore_disk_overlays(&mut self, _overlays: snapshot::DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        0
    }

    fn write_ram(&mut self, _offset: u64, _data: &[u8]) -> snapshot::Result<()> {
        Ok(())
    }
}

#[test]
fn restore_snapshot_decodes_mmus_v1_and_sorts_by_apic_id() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(snapshot::SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&snapshot::SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(snapshot::SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    // CPUS section with two vCPUs.
    let mut cpus_payload = Vec::new();
    cpus_payload.extend_from_slice(&2u32.to_le_bytes());
    for apic_id in [0u32, 1u32] {
        let vcpu = snapshot::VcpuSnapshot {
            apic_id,
            cpu: snapshot::CpuState::default(),
            internal_state: Vec::new(),
        };
        let mut entry = Vec::new();
        vcpu.encode_v2(&mut entry).unwrap();
        cpus_payload.extend_from_slice(&(entry.len() as u64).to_le_bytes());
        cpus_payload.extend_from_slice(&entry);
    }
    push_section(&mut bytes, snapshot::SectionId::CPUS, 2, 0, &cpus_payload);

    // MMUS v1 payload with entries intentionally out of order.
    let mut mmus_payload = Vec::new();
    mmus_payload.extend_from_slice(&2u32.to_le_bytes());

    let entry_apic1 = {
        let mut entry = Vec::new();
        entry.extend_from_slice(&1u32.to_le_bytes());
        snapshot::MmuState {
            cr3: 0xBBBB,
            ..snapshot::MmuState::default()
        }
        .encode_v1(&mut entry)
        .unwrap();
        entry
    };
    mmus_payload.extend_from_slice(&(entry_apic1.len() as u64).to_le_bytes());
    mmus_payload.extend_from_slice(&entry_apic1);

    let entry_apic0 = {
        let mut entry = Vec::new();
        entry.extend_from_slice(&0u32.to_le_bytes());
        snapshot::MmuState {
            cr3: 0xAAAA,
            ..snapshot::MmuState::default()
        }
        .encode_v1(&mut entry)
        .unwrap();
        entry
    };
    mmus_payload.extend_from_slice(&(entry_apic0.len() as u64).to_le_bytes());
    mmus_payload.extend_from_slice(&entry_apic0);

    push_section(&mut bytes, snapshot::SectionId::MMUS, 1, 0, &mmus_payload);

    // Minimal RAM section (0-length full snapshot).
    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(snapshot::RamMode::Full as u8);
    ram_payload.push(snapshot::Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, snapshot::SectionId::RAM, 1, 0, &ram_payload);

    let mut target = CaptureTarget::default();
    snapshot::restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap();

    let mmus = target.mmus.expect("target did not receive MMUS list");
    assert_eq!(mmus.len(), 2);
    assert_eq!(mmus[0].apic_id, 0);
    assert_eq!(mmus[1].apic_id, 1);
    assert_eq!(mmus[0].mmu.cr3, 0xAAAA);
    assert_eq!(mmus[1].mmu.cr3, 0xBBBB);
}
