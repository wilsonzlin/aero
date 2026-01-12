use aero_snapshot as snapshot;
use std::io::{Cursor, Read, Seek, SeekFrom};

fn read_u32_le(r: &mut impl Read) -> u32 {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).expect("read u32");
    u32::from_le_bytes(buf)
}

#[derive(Default)]
struct CaptureTarget {
    ram: Vec<u8>,
    devices: Option<Vec<snapshot::DeviceState>>,
}

impl snapshot::SnapshotTarget for CaptureTarget {
    fn restore_cpu_state(&mut self, _state: snapshot::CpuState) {}

    fn restore_mmu_state(&mut self, _state: snapshot::MmuState) {}

    fn restore_device_states(&mut self, states: Vec<snapshot::DeviceState>) {
        self.devices = Some(states);
    }

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

    fn mmu_state(&self) -> snapshot::MmuState {
        snapshot::MmuState::default()
    }

    fn device_states(&self) -> Vec<snapshot::DeviceState> {
        vec![
            snapshot::DeviceState {
                id: snapshot::DeviceId::VGA,
                version: 1,
                flags: 0,
                data: vec![0xAA],
            },
            snapshot::DeviceState {
                id: snapshot::DeviceId::PCI,
                version: 2,
                flags: 0,
                data: vec![0xBB],
            },
            snapshot::DeviceState {
                id: snapshot::DeviceId::PCI,
                version: 1,
                flags: 1,
                data: vec![0xCC],
            },
        ]
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
fn restore_snapshot_sorts_devices_by_key_before_passing_to_target() {
    let mut source = MinimalSource { ram: vec![0u8; 16] };
    let mut cursor = Cursor::new(Vec::new());
    snapshot::save_snapshot(&mut cursor, &mut source, snapshot::SaveOptions::default()).unwrap();
    let mut bytes = cursor.into_inner();

    // Rewrite the DEVICES section payload with the same entries but in reverse order.
    let (devices_off, devices_len) = {
        let mut r = Cursor::new(bytes.as_slice());
        let index = snapshot::inspect_snapshot(&mut r).expect("snapshot should be inspectable");
        let devices = index
            .sections
            .iter()
            .find(|s| s.id == snapshot::SectionId::DEVICES)
            .expect("snapshot should contain a DEVICES section");
        (devices.offset, devices.len)
    };

    let mut devices = {
        let mut r = Cursor::new(bytes.as_slice());
        r.seek(SeekFrom::Start(devices_off))
            .expect("seek to DEVICES payload");

        let mut limited = r.take(devices_len);
        let count = read_u32_le(&mut limited) as usize;

        let mut out = Vec::with_capacity(count.min(64));
        for _ in 0..count {
            out.push(
                snapshot::DeviceState::decode(&mut limited, snapshot::limits::MAX_DEVICE_ENTRY_LEN)
                    .expect("decode device entry"),
            );
        }
        out
    };
    devices.reverse();

    let mut rewritten = Vec::new();
    rewritten.extend_from_slice(&(devices.len() as u32).to_le_bytes());
    for dev in devices {
        dev.encode(&mut rewritten).expect("encode device entry");
    }
    assert_eq!(
        u64::try_from(rewritten.len()).unwrap(),
        devices_len,
        "rewriting DEVICES section should not change the payload length"
    );

    let off = usize::try_from(devices_off).expect("DEVICES offset should fit in usize");
    let len = usize::try_from(devices_len).expect("DEVICES len should fit in usize");
    bytes[off..off + len].copy_from_slice(&rewritten);

    let mut target = CaptureTarget {
        ram: vec![0u8; 16],
        devices: None,
    };
    snapshot::restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap();

    let devices = target
        .devices
        .expect("snapshot restore target did not receive DEVICES section");
    let keys: Vec<(u32, u16, u16)> = devices
        .iter()
        .map(|d| (d.id.0, d.version, d.flags))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}
