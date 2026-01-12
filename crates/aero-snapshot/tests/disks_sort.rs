use aero_snapshot as snapshot;
use std::io::{Cursor, Read, Seek, SeekFrom};

#[derive(Default)]
struct CaptureTarget {
    ram: Vec<u8>,
    disks: Option<snapshot::DiskOverlayRefs>,
}

impl snapshot::SnapshotTarget for CaptureTarget {
    fn restore_cpu_state(&mut self, _state: snapshot::CpuState) {}

    fn restore_mmu_state(&mut self, _state: snapshot::MmuState) {}

    fn restore_device_states(&mut self, _states: Vec<snapshot::DeviceState>) {}

    fn restore_disk_overlays(&mut self, overlays: snapshot::DiskOverlayRefs) {
        self.disks = Some(overlays);
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> snapshot::Result<()> {
        let offset = usize::try_from(offset)
            .map_err(|_| snapshot::SnapshotError::Corrupt("ram write offset overflow"))?;
        let end = offset
            .checked_add(data.len())
            .ok_or(snapshot::SnapshotError::Corrupt(
                "ram write offset overflow",
            ))?;
        if end > self.ram.len() {
            return Err(snapshot::SnapshotError::Corrupt("ram write out of range"));
        }
        self.ram[offset..end].copy_from_slice(data);
        Ok(())
    }
}

struct MinimalSource {
    ram: Vec<u8>,
}

impl snapshot::SnapshotSource for MinimalSource {
    fn snapshot_meta(&mut self) -> snapshot::SnapshotMeta {
        snapshot::SnapshotMeta {
            snapshot_id: 1,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: None,
        }
    }

    fn cpu_state(&self) -> snapshot::CpuState {
        snapshot::CpuState::default()
    }

    fn mmu_state(&self) -> snapshot::MmuState {
        snapshot::MmuState::default()
    }

    fn device_states(&self) -> Vec<snapshot::DeviceState> {
        Vec::new()
    }

    fn disk_overlays(&self) -> snapshot::DiskOverlayRefs {
        snapshot::DiskOverlayRefs {
            disks: vec![
                snapshot::DiskOverlayRef {
                    disk_id: 0,
                    base_image: "a.base".to_string(),
                    overlay_image: "a.ovl".to_string(),
                },
                snapshot::DiskOverlayRef {
                    disk_id: 1,
                    base_image: "b.base".to_string(),
                    overlay_image: "b.ovl".to_string(),
                },
            ],
        }
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> snapshot::Result<()> {
        let offset = usize::try_from(offset)
            .map_err(|_| snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        if end > self.ram.len() {
            return Err(snapshot::SnapshotError::Corrupt("ram read out of range"));
        }
        buf.copy_from_slice(&self.ram[offset..end]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

#[test]
fn restore_snapshot_sorts_disks_by_disk_id_before_passing_to_target() {
    let mut source = MinimalSource { ram: vec![0u8; 16] };
    let mut cursor = Cursor::new(Vec::new());
    snapshot::save_snapshot(&mut cursor, &mut source, snapshot::SaveOptions::default()).unwrap();
    let mut bytes = cursor.into_inner();

    // Rewrite the DISKS section payload with the same entries but in reverse order.
    let (disks_off, disks_len) = {
        let mut r = Cursor::new(bytes.as_slice());
        let index = snapshot::inspect_snapshot(&mut r).expect("snapshot should be inspectable");
        let disks = index
            .sections
            .iter()
            .find(|s| s.id == snapshot::SectionId::DISKS)
            .expect("snapshot should contain a DISKS section");
        (disks.offset, disks.len)
    };

    let mut overlays = {
        let mut r = Cursor::new(bytes.as_slice());
        r.seek(SeekFrom::Start(disks_off))
            .expect("seek to DISKS payload");
        let mut limited = r.take(disks_len);
        snapshot::DiskOverlayRefs::decode(&mut limited).expect("decode DISKS payload")
    };
    overlays.disks.reverse();

    let mut rewritten = Vec::new();
    overlays
        .encode(&mut rewritten)
        .expect("encode DISKS payload");
    assert_eq!(
        u64::try_from(rewritten.len()).unwrap(),
        disks_len,
        "rewriting DISKS section should not change the payload length"
    );

    let off = usize::try_from(disks_off).expect("DISKS offset should fit in usize");
    let len = usize::try_from(disks_len).expect("DISKS len should fit in usize");
    bytes[off..off + len].copy_from_slice(&rewritten);

    let mut target = CaptureTarget {
        ram: vec![0u8; 16],
        disks: None,
    };
    snapshot::restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap();

    let overlays = target
        .disks
        .expect("snapshot restore target did not receive DISKS section");
    let disk_ids: Vec<u32> = overlays.disks.iter().map(|d| d.disk_id).collect();
    assert_eq!(disk_ids, vec![0, 1]);
}
