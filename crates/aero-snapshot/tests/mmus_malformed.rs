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

fn snapshot_header() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(snapshot::SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&snapshot::SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(snapshot::SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes
}

fn minimal_cpu_section_v2(dst: &mut Vec<u8>) {
    let mut cpu_payload = Vec::new();
    snapshot::CpuState::default().encode_v2(&mut cpu_payload).unwrap();
    push_section(dst, snapshot::SectionId::CPU, 2, 0, &cpu_payload);
}

fn minimal_ram_section(dst: &mut Vec<u8>) {
    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(snapshot::RamMode::Full as u8);
    ram_payload.push(snapshot::Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(dst, snapshot::SectionId::RAM, 1, 0, &ram_payload);
}

#[test]
fn restore_snapshot_rejects_mmus_section_with_zero_entries() {
    let mut bytes = snapshot_header();
    minimal_cpu_section_v2(&mut bytes);

    let mmus_payload = 0u32.to_le_bytes(); // count=0
    push_section(&mut bytes, snapshot::SectionId::MMUS, 2, 0, &mmus_payload);
    minimal_ram_section(&mut bytes);

    let mut target = DummyTarget;
    let err = snapshot::restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(err, snapshot::SnapshotError::Corrupt("missing MMU entry")));
}

#[test]
fn restore_snapshot_rejects_truncated_mmus_entry_len_prefix() {
    let mut bytes = snapshot_header();
    minimal_cpu_section_v2(&mut bytes);

    // MMUS payload: count=1 + entry_len prefix, but no entry bytes.
    let mut mmus_payload = Vec::new();
    mmus_payload.extend_from_slice(&1u32.to_le_bytes()); // count=1
    mmus_payload.extend_from_slice(&(64u64).to_le_bytes()); // entry_len, but entry missing
    push_section(&mut bytes, snapshot::SectionId::MMUS, 2, 0, &mmus_payload);
    minimal_ram_section(&mut bytes);

    let mut target = DummyTarget;
    let err = snapshot::restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        snapshot::SnapshotError::Corrupt("truncated MMU entry")
    ));
}

