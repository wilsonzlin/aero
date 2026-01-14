#![cfg(not(target_arch = "wasm32"))]

use aero_snapshot as snapshot;
use std::io::Cursor;

struct EmptyMmusSource;

impl snapshot::SnapshotSource for EmptyMmusSource {
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
        // Intentionally empty: multi-vCPU snapshots require one MMU entry per vCPU.
        Vec::new()
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
fn save_snapshot_rejects_empty_mmu_states_list_for_multi_vcpu_snapshot() {
    let mut source = EmptyMmusSource;
    let mut cursor = Cursor::new(Vec::new());
    let err = snapshot::save_snapshot(&mut cursor, &mut source, snapshot::SaveOptions::default())
        .unwrap_err();
    assert!(matches!(
        err,
        snapshot::SnapshotError::Corrupt("missing MMU entry")
    ));
}

