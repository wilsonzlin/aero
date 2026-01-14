#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;

use aero_snapshot as snapshot;

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

struct MismatchSource {
    ram: Vec<u8>,
}

impl snapshot::SnapshotSource for MismatchSource {
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
        // Intentionally mismatched set (CPUS={0,1}, MMUS={0,2}).
        vec![
            snapshot::VcpuMmuSnapshot {
                apic_id: 0,
                mmu: snapshot::MmuState::default(),
            },
            snapshot::VcpuMmuSnapshot {
                apic_id: 2,
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
        self.ram.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> snapshot::Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(snapshot::SnapshotError::Corrupt("ram read overflow"))?;
        if end > self.ram.len() {
            return Err(snapshot::SnapshotError::Corrupt("ram read out of bounds"));
        }
        buf.copy_from_slice(&self.ram[offset..end]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

#[test]
fn save_snapshot_rejects_mismatched_mmus_vs_cpus_apic_ids() {
    let mut source = MismatchSource { ram: vec![0u8; 64] };
    let mut out = Cursor::new(Vec::new());
    let err = snapshot::save_snapshot(&mut out, &mut source, snapshot::SaveOptions::default())
        .unwrap_err();
    assert!(matches!(
        err,
        snapshot::SnapshotError::Corrupt("MMUS entries do not match CPUS apic_id list")
    ));
}

#[derive(Default)]
struct MultiCpuTarget;

impl snapshot::SnapshotTarget for MultiCpuTarget {
    fn restore_cpu_state(&mut self, _state: snapshot::CpuState) {}

    fn restore_cpu_states(&mut self, _states: Vec<snapshot::VcpuSnapshot>) -> snapshot::Result<()> {
        Ok(())
    }

    fn restore_mmu_state(&mut self, _state: snapshot::MmuState) {}

    fn restore_mmu_states(
        &mut self,
        _states: Vec<snapshot::VcpuMmuSnapshot>,
    ) -> snapshot::Result<()> {
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
fn restore_snapshot_rejects_mismatched_mmus_vs_cpus_apic_ids() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(snapshot::SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&snapshot::SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(snapshot::SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    // Snapshot is inconsistent:
    // - CPUS apic_id list is {0,1}
    // - MMUS apic_id list is {0,2}
    //
    // Encode `MMUS` first to ensure restore validates even when section order differs.
    let mut mmus_payload = Vec::new();
    mmus_payload.extend_from_slice(&2u32.to_le_bytes()); // count
    for (apic_id, cr3) in [(0u32, 0x1000u64), (2u32, 0x2000u64)] {
        let entry = snapshot::VcpuMmuSnapshot {
            apic_id,
            mmu: snapshot::MmuState {
                cr3,
                ..snapshot::MmuState::default()
            },
        };
        let mut entry_bytes = Vec::new();
        entry.encode_v2(&mut entry_bytes).unwrap();
        mmus_payload.extend_from_slice(&(entry_bytes.len() as u64).to_le_bytes());
        mmus_payload.extend_from_slice(&entry_bytes);
    }
    push_section(&mut bytes, snapshot::SectionId::MMUS, 2, 0, &mmus_payload);

    let mut cpus_payload = Vec::new();
    cpus_payload.extend_from_slice(&2u32.to_le_bytes()); // count
    for apic_id in [0u32, 1u32] {
        let entry = snapshot::VcpuSnapshot {
            apic_id,
            cpu: snapshot::CpuState::default(),
            internal_state: Vec::new(),
        };
        let mut entry_bytes = Vec::new();
        entry.encode_v2(&mut entry_bytes).unwrap();
        cpus_payload.extend_from_slice(&(entry_bytes.len() as u64).to_le_bytes());
        cpus_payload.extend_from_slice(&entry_bytes);
    }
    push_section(&mut bytes, snapshot::SectionId::CPUS, 2, 0, &cpus_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(snapshot::RamMode::Full as u8);
    ram_payload.push(snapshot::Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, snapshot::SectionId::RAM, 1, 0, &ram_payload);

    let mut target = MultiCpuTarget;
    let err = snapshot::restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        snapshot::SnapshotError::Corrupt("MMUS entries do not match CPUS apic_id list")
    ));
}
