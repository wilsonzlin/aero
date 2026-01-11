use std::io::{Cursor, ErrorKind};

use aero_snapshot::{
    restore_snapshot, save_snapshot, Compression, CpuState, DeviceState, DiskOverlayRefs, MmuState,
    RamMode, RamWriteOptions, Result, SaveOptions, SectionId, SnapshotError, SnapshotMeta,
    SnapshotSource, SnapshotTarget, SNAPSHOT_ENDIANNESS_LITTLE, SNAPSHOT_MAGIC,
    SNAPSHOT_VERSION_V1,
};

#[derive(Default)]
struct DummyTarget {
    ram: Vec<u8>,
}

impl DummyTarget {
    fn new(ram_len: usize) -> Self {
        Self {
            ram: vec![0u8; ram_len],
        }
    }
}

impl SnapshotTarget for DummyTarget {
    fn restore_cpu_state(&mut self, _state: CpuState) {}

    fn restore_mmu_state(&mut self, _state: MmuState) {}

    fn restore_device_states(&mut self, _states: Vec<DeviceState>) {}

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        if offset + data.len() > self.ram.len() {
            return Err(SnapshotError::Corrupt("ram write out of bounds"));
        }
        self.ram[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }
}

struct MinimalSource {
    ram: Vec<u8>,
}

impl SnapshotSource for MinimalSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta::default()
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        Vec::new()
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        buf.copy_from_slice(&self.ram[offset..offset + buf.len()]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

fn push_section(dst: &mut Vec<u8>, id: SectionId, version: u16, flags: u16, payload: &[u8]) {
    dst.extend_from_slice(&id.0.to_le_bytes());
    dst.extend_from_slice(&version.to_le_bytes());
    dst.extend_from_slice(&flags.to_le_bytes());
    dst.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    dst.extend_from_slice(payload);
}

#[test]
fn restore_snapshot_rejects_truncated_unknown_section_payload() {
    let options = SaveOptions {
        ram: RamWriteOptions {
            compression: Compression::None,
            chunk_size: 1024,
            ..RamWriteOptions::default()
        },
    };

    let mut source = MinimalSource { ram: vec![0u8; 8] };
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, options).unwrap();
    let mut bytes = cursor.into_inner();

    // Append an unknown section with a claimed payload length that exceeds the remaining bytes.
    bytes.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // unknown section id
    bytes.extend_from_slice(&1u16.to_le_bytes()); // version
    bytes.extend_from_slice(&0u16.to_le_bytes()); // flags
    bytes.extend_from_slice(&10u64.to_le_bytes()); // section_len
    bytes.push(0xAA); // only 1 byte of payload, should trigger UnexpectedEof

    let mut target = DummyTarget::new(8);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    match err {
        SnapshotError::Io(e) => assert_eq!(e.kind(), ErrorKind::UnexpectedEof),
        other => panic!("expected io UnexpectedEof, got {other:?}"),
    }
}

#[test]
fn restore_snapshot_rejects_truncated_cpu_v2_extension_prefix() {
    let options = SaveOptions {
        ram: RamWriteOptions {
            compression: Compression::None,
            chunk_size: 1024,
            ..RamWriteOptions::default()
        },
    };

    let mut source = MinimalSource { ram: vec![0u8; 8] };
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, options).unwrap();
    let bytes = cursor.into_inner();

    // Locate the CPU section (id=CPU, version=2) so we can truncate the optional extension length
    // prefix to 1 byte while keeping the overall section framing consistent.
    let mut off = 16usize; // global header size
    let mut cpu_header_off = None;
    let mut cpu_payload_off = 0usize;
    let mut cpu_len = 0usize;
    while off + 16 <= bytes.len() {
        let id = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        let version = u16::from_le_bytes(bytes[off + 4..off + 6].try_into().unwrap());
        let len = u64::from_le_bytes(bytes[off + 8..off + 16].try_into().unwrap()) as usize;
        let payload_off = off + 16;

        if id == SectionId::CPU.0 && version == 2 {
            cpu_header_off = Some(off);
            cpu_payload_off = payload_off;
            cpu_len = len;
            break;
        }

        off = payload_off + len;
    }

    let cpu_header_off = cpu_header_off.expect("expected CPU section");
    assert!(cpu_len > 7, "CPU section payload unexpectedly short");

    // CPU v2 encodes a 4-byte extension length + ext bytes at the end. Remove 7 bytes (3 bytes of
    // the length prefix + 4 bytes of ext data), leaving a 1-byte prefix fragment.
    let new_len = cpu_len - 7;

    let mut corrupted = Vec::with_capacity(bytes.len() - 7);
    corrupted.extend_from_slice(&bytes[..cpu_header_off]);
    // Copy section id/version/flags (first 8 bytes of section header).
    corrupted.extend_from_slice(&bytes[cpu_header_off..cpu_header_off + 8]);
    corrupted.extend_from_slice(&(new_len as u64).to_le_bytes());
    corrupted.extend_from_slice(&bytes[cpu_payload_off..cpu_payload_off + new_len]);
    // Append remaining sections.
    corrupted.extend_from_slice(&bytes[cpu_payload_off + cpu_len..]);

    let mut target = DummyTarget::new(8);
    let err = restore_snapshot(&mut Cursor::new(corrupted), &mut target).unwrap_err();
    match err {
        SnapshotError::Io(e) => assert_eq!(e.kind(), ErrorKind::UnexpectedEof),
        other => panic!("expected UnexpectedEof, got {other:?}"),
    }
}

#[test]
fn restore_snapshot_rejects_dirty_ram_count_exceeding_max_pages() {
    let total_len = 4096u64;
    let page_size = 4096u32;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut cpu_payload = Vec::new();
    CpuState::default().encode(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 1, 0, &cpu_payload);

    // Dirty RAM snapshot whose `count` exceeds max_pages (= ceil(total_len / page_size) = 1).
    let count = 2u64;
    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&total_len.to_le_bytes());
    ram_payload.extend_from_slice(&page_size.to_le_bytes());
    ram_payload.push(RamMode::Dirty as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes());
    ram_payload.extend_from_slice(&count.to_le_bytes());

    // Include valid page entries so this snapshot would otherwise decode without error.
    for _ in 0..count {
        ram_payload.extend_from_slice(&0u64.to_le_bytes()); // page_idx
        ram_payload.extend_from_slice(&(total_len as u32).to_le_bytes()); // uncompressed_len
        ram_payload.extend_from_slice(&(total_len as u32).to_le_bytes()); // compressed_len
        ram_payload.extend_from_slice(&vec![0u8; total_len as usize]);
    }
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(total_len as usize);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("too many dirty pages")
    ));
}
