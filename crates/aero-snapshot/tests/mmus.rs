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
        self.ram.len()
    }

    fn write_ram(&mut self, _offset: u64, _data: &[u8]) -> snapshot::Result<()> {
        Ok(())
    }
}

struct TwoCpuSource {
    ram: Vec<u8>,
}

impl snapshot::SnapshotSource for TwoCpuSource {
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
                mmu: snapshot::MmuState {
                    cr3: 0xAAAA,
                    ..snapshot::MmuState::default()
                },
            },
            snapshot::VcpuMmuSnapshot {
                apic_id: 1,
                mmu: snapshot::MmuState {
                    cr3: 0xBBBB,
                    ..snapshot::MmuState::default()
                },
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

    fn read_ram(&self, _offset: u64, _buf: &mut [u8]) -> snapshot::Result<()> {
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

#[test]
fn restore_snapshot_sorts_mmus_by_apic_id_before_passing_to_target() {
    let mut source = TwoCpuSource { ram: vec![0u8; 0] };
    let mut cursor = Cursor::new(Vec::new());
    snapshot::save_snapshot(&mut cursor, &mut source, snapshot::SaveOptions::default()).unwrap();
    let mut bytes = cursor.into_inner();

    // Rewrite the MMUS section payload with the same entries but in reverse order.
    let (mmus_off, mmus_len) = {
        let mut r = Cursor::new(bytes.as_slice());
        let index = snapshot::inspect_snapshot(&mut r).expect("snapshot should be inspectable");
        let mmus = index
            .sections
            .iter()
            .find(|s| s.id == snapshot::SectionId::MMUS)
            .expect("snapshot should contain a MMUS section");
        (mmus.offset, mmus.len)
    };

    let entries = {
        let mut r = Cursor::new(bytes.as_slice());
        r.seek(SeekFrom::Start(mmus_off))
            .expect("seek to MMUS payload");

        let mut limited = r.take(mmus_len);
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
        mmus_len,
        "rewriting MMUS section should not change the payload length"
    );

    let off = usize::try_from(mmus_off).expect("MMUS offset should fit in usize");
    let len = usize::try_from(mmus_len).expect("MMUS len should fit in usize");
    bytes[off..off + len].copy_from_slice(&rewritten);

    let mut target = CaptureTarget {
        ram: vec![],
        mmus: None,
    };
    snapshot::restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap();

    let mmus = target
        .mmus
        .expect("snapshot restore target did not receive MMUS section");
    let apic_ids: Vec<u32> = mmus.iter().map(|m| m.apic_id).collect();
    assert_eq!(apic_ids, vec![0, 1]);
    assert_eq!(mmus[0].mmu.cr3, 0xAAAA);
    assert_eq!(mmus[1].mmu.cr3, 0xBBBB);
}

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

#[test]
fn restore_legacy_cpus_plus_single_mmu_replica_calls_restore_mmu_states() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(snapshot::SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&snapshot::SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(snapshot::SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

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

    let mmu = snapshot::MmuState {
        cr3: 0xDEAD_BEEF,
        ..snapshot::MmuState::default()
    };
    let mut mmu_payload = Vec::new();
    mmu.encode_v2(&mut mmu_payload).unwrap();
    push_section(&mut bytes, snapshot::SectionId::MMU, 2, 0, &mmu_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(snapshot::RamMode::Full as u8);
    ram_payload.push(snapshot::Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, snapshot::SectionId::RAM, 1, 0, &ram_payload);

    let mut target = CaptureTarget {
        ram: vec![],
        mmus: None,
    };
    snapshot::restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap();

    let mmus = target
        .mmus
        .expect("snapshot restore target did not receive MMU state list");
    let apic_ids: Vec<u32> = mmus.iter().map(|m| m.apic_id).collect();
    assert_eq!(apic_ids, vec![0, 1]);
    assert_eq!(mmus[0].mmu.cr3, 0xDEAD_BEEF);
    assert_eq!(mmus[1].mmu.cr3, 0xDEAD_BEEF);
}
