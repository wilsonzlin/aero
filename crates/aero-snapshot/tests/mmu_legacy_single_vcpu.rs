use aero_snapshot as snapshot;
use std::io::Cursor;

struct SingleCpuCpusSectionSource {
    ram: Vec<u8>,
}

impl snapshot::SnapshotSource for SingleCpuCpusSectionSource {
    fn snapshot_meta(&mut self) -> snapshot::SnapshotMeta {
        snapshot::SnapshotMeta::default()
    }

    fn cpu_state(&self) -> snapshot::CpuState {
        snapshot::CpuState::default()
    }

    fn cpu_states(&self) -> Vec<snapshot::VcpuSnapshot> {
        // Force `save_snapshot` to use the `CPUS` section (instead of legacy `CPU`) even though
        // there is only one vCPU.
        vec![snapshot::VcpuSnapshot {
            apic_id: 0,
            cpu: snapshot::CpuState::default(),
            internal_state: vec![0xAA],
        }]
    }

    fn mmu_state(&self) -> snapshot::MmuState {
        snapshot::MmuState::default()
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

    fn read_ram(&self, _offset: u64, _buf: &mut [u8]) -> snapshot::Result<()> {
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

#[test]
fn save_snapshot_single_vcpu_cpus_section_still_uses_legacy_mmu_section() {
    let mut source = SingleCpuCpusSectionSource { ram: Vec::new() };
    let mut cursor = Cursor::new(Vec::new());
    snapshot::save_snapshot(&mut cursor, &mut source, snapshot::SaveOptions::default()).unwrap();

    let mut r = Cursor::new(cursor.into_inner());
    let index = snapshot::inspect_snapshot(&mut r).expect("snapshot should be inspectable");

    assert!(
        index.sections.iter().any(|s| s.id == snapshot::SectionId::CPUS),
        "expected save_snapshot to encode a single-vCPU state using CPUS when internal_state is present"
    );
    assert!(
        index
            .sections
            .iter()
            .any(|s| s.id == snapshot::SectionId::MMU),
        "expected save_snapshot to preserve the legacy MMU section for single-vCPU snapshots"
    );
    assert!(
        !index
            .sections
            .iter()
            .any(|s| s.id == snapshot::SectionId::MMUS),
        "expected save_snapshot to avoid MMUS for single-vCPU snapshots"
    );
}
