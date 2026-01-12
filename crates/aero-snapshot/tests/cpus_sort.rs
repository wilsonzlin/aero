use aero_snapshot as snapshot;
use std::io::{Cursor, Read, Seek, SeekFrom};

fn read_u32_le(r: &mut impl Read) -> u32 {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).expect("read u32");
    u32::from_le_bytes(buf)
}

fn read_u64_le(r: &mut impl Read) -> u64 {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf).expect("read u64");
    u64::from_le_bytes(buf)
}

#[derive(Default)]
struct CaptureTarget {
    ram: Vec<u8>,
    cpus: Option<Vec<snapshot::VcpuSnapshot>>,
}

impl snapshot::SnapshotTarget for CaptureTarget {
    fn restore_cpu_state(&mut self, _state: snapshot::CpuState) {}

    fn restore_cpu_states(&mut self, states: Vec<snapshot::VcpuSnapshot>) -> snapshot::Result<()> {
        self.cpus = Some(states);
        Ok(())
    }

    fn restore_mmu_state(&mut self, _state: snapshot::MmuState) {}

    fn restore_device_states(&mut self, _states: Vec<snapshot::DeviceState>) {}

    fn restore_disk_overlays(&mut self, _overlays: snapshot::DiskOverlayRefs) {}

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
fn restore_snapshot_sorts_cpus_by_apic_id_before_passing_to_target() {
    let mut source = MinimalSource { ram: vec![0u8; 16] };
    let mut cursor = Cursor::new(Vec::new());
    snapshot::save_snapshot(&mut cursor, &mut source, snapshot::SaveOptions::default()).unwrap();
    let mut bytes = cursor.into_inner();

    // Rewrite the CPUS section payload with the same entries but in reverse order.
    let (cpus_off, cpus_len) = {
        let mut r = Cursor::new(bytes.as_slice());
        let index = snapshot::inspect_snapshot(&mut r).expect("snapshot should be inspectable");
        let cpus = index
            .sections
            .iter()
            .find(|s| s.id == snapshot::SectionId::CPUS)
            .expect("snapshot should contain a CPUS section");
        (cpus.offset, cpus.len)
    };

    let entries = {
        let mut r = Cursor::new(bytes.as_slice());
        r.seek(SeekFrom::Start(cpus_off))
            .expect("seek to CPUS payload");

        let mut limited = r.take(cpus_len);
        let count = read_u32_le(&mut limited) as usize;
        let mut entries = Vec::with_capacity(count.min(64));
        for _ in 0..count {
            let entry_len = read_u64_le(&mut limited) as usize;
            let mut entry = vec![0u8; entry_len];
            limited.read_exact(&mut entry).expect("read entry bytes");
            entries.push(entry);
        }
        entries
    };

    let mut rewritten = Vec::new();
    rewritten.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries.into_iter().rev() {
        rewritten.extend_from_slice(&(entry.len() as u64).to_le_bytes());
        rewritten.extend_from_slice(&entry);
    }
    assert_eq!(
        u64::try_from(rewritten.len()).unwrap(),
        cpus_len,
        "rewriting CPUS section should not change the payload length"
    );

    let off = usize::try_from(cpus_off).expect("CPUS offset should fit in usize");
    let len = usize::try_from(cpus_len).expect("CPUS len should fit in usize");
    bytes[off..off + len].copy_from_slice(&rewritten);

    let mut target = CaptureTarget {
        ram: vec![0u8; 16],
        cpus: None,
    };
    snapshot::restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap();

    let cpus = target
        .cpus
        .expect("snapshot restore target did not receive CPUS section");
    let apic_ids: Vec<u32> = cpus.iter().map(|c| c.apic_id).collect();
    assert_eq!(apic_ids, vec![0, 1]);
}
