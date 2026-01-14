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
struct DummyTarget;

impl snapshot::SnapshotTarget for DummyTarget {
    fn restore_cpu_state(&mut self, _state: snapshot::CpuState) {}

    fn restore_mmu_state(&mut self, _state: snapshot::MmuState) {}

    fn restore_device_states(&mut self, _states: Vec<snapshot::DeviceState>) {}

    fn restore_disk_overlays(&mut self, _overlays: snapshot::DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        0
    }

    fn write_ram(&mut self, _offset: u64, _data: &[u8]) -> snapshot::Result<()> {
        Ok(())
    }
}

struct DuplicateMmuApicIdSource;

impl snapshot::SnapshotSource for DuplicateMmuApicIdSource {
    fn snapshot_meta(&mut self) -> snapshot::SnapshotMeta {
        snapshot::SnapshotMeta::default()
    }

    fn cpu_state(&self) -> snapshot::CpuState {
        snapshot::CpuState::default()
    }

    fn cpu_states(&self) -> Vec<snapshot::VcpuSnapshot> {
        vec![
            snapshot::VcpuSnapshot {
                apic_id: 0,
                cpu: snapshot::CpuState::default(),
                internal_state: Vec::new(),
            },
            snapshot::VcpuSnapshot {
                apic_id: 1,
                cpu: snapshot::CpuState::default(),
                internal_state: Vec::new(),
            },
        ]
    }

    fn mmu_state(&self) -> snapshot::MmuState {
        snapshot::MmuState::default()
    }

    fn mmu_states(&self) -> Vec<snapshot::VcpuMmuSnapshot> {
        vec![
            snapshot::VcpuMmuSnapshot {
                apic_id: 0,
                mmu: snapshot::MmuState::default(),
            },
            snapshot::VcpuMmuSnapshot {
                apic_id: 0,
                mmu: snapshot::MmuState::default(),
            },
        ]
    }

    fn device_states(&self) -> Vec<snapshot::DeviceState> {
        Vec::new()
    }

    fn disk_overlays(&self) -> snapshot::DiskOverlayRefs {
        snapshot::DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        0
    }

    fn read_ram(&self, _offset: u64, _buf: &mut [u8]) -> snapshot::Result<()> {
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

#[test]
fn save_snapshot_rejects_duplicate_apic_ids_in_mmu_states_list() {
    let mut source = DuplicateMmuApicIdSource;
    let mut cursor = Cursor::new(Vec::new());
    let err = snapshot::save_snapshot(&mut cursor, &mut source, snapshot::SaveOptions::default())
        .unwrap_err();
    assert!(matches!(
        err,
        snapshot::SnapshotError::Corrupt("duplicate APIC ID in MMU list (apic_id must be unique)")
    ));
}

#[test]
fn restore_snapshot_rejects_duplicate_apic_ids_in_mmus_section() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(snapshot::SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&snapshot::SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(snapshot::SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut cpu_payload = Vec::new();
    snapshot::CpuState::default()
        .encode_v2(&mut cpu_payload)
        .unwrap();
    push_section(&mut bytes, snapshot::SectionId::CPU, 2, 0, &cpu_payload);

    // MMUS section with 2 entries that both use apic_id=0.
    let mut entry = Vec::new();
    entry.extend_from_slice(&0u32.to_le_bytes());
    snapshot::MmuState::default().encode_v2(&mut entry).unwrap();

    let mut mmus_payload = Vec::new();
    mmus_payload.extend_from_slice(&2u32.to_le_bytes());
    for _ in 0..2 {
        mmus_payload.extend_from_slice(&(entry.len() as u64).to_le_bytes());
        mmus_payload.extend_from_slice(&entry);
    }
    push_section(&mut bytes, snapshot::SectionId::MMUS, 2, 0, &mmus_payload);

    // Minimal RAM section (0-length full snapshot).
    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(snapshot::RamMode::Full as u8);
    ram_payload.push(snapshot::Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, snapshot::SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget;
    let err = snapshot::restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        snapshot::SnapshotError::Corrupt("duplicate APIC ID in MMU list (apic_id must be unique)")
    ));
}
