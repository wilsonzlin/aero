#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;

use aero_snapshot as snapshot;

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
