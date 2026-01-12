#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::io::Cursor;
use std::io::{Seek, Write};

use aero_snapshot::{
    Compression, CpuState, DeviceId, DeviceState, DiskOverlayRef, DiskOverlayRefs, MmuState,
    RamMode, RamWriteOptions, SaveOptions, SectionId, SnapshotMeta, SnapshotSource, VcpuSnapshot,
    SNAPSHOT_ENDIANNESS_LITTLE, SNAPSHOT_MAGIC, SNAPSHOT_VERSION_V1,
};
use assert_cmd::Command;
use predicates::prelude::*;

const MAX_DEVICES_SECTION_LEN: u64 = 256 * 1024 * 1024;

struct DummySource {
    ram: Vec<u8>,
}

impl DummySource {
    fn new(ram_len: usize) -> Self {
        let mut ram = Vec::with_capacity(ram_len);
        ram.extend((0..ram_len).map(|i| (i as u8).wrapping_mul(31)));
        Self { ram }
    }
}

impl SnapshotSource for DummySource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 1,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: Some("xtask-test".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        vec![DeviceState {
            id: DeviceId::SERIAL,
            version: 1,
            flags: 0,
            data: vec![1, 2, 3, 4],
        }]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(aero_snapshot::SnapshotError::Corrupt("ram read overflow"))?;
        buf.copy_from_slice(&self.ram[offset..end]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

fn write_snapshot(path: &std::path::Path) {
    let mut file = fs::File::create(path).unwrap();
    // Ensure we always start writing at offset 0; Windows tests occasionally reuse handles.
    file.rewind().unwrap();
    let mut source = DummySource::new(4096);
    aero_snapshot::save_snapshot(&mut file, &mut source, SaveOptions::default()).unwrap();
    file.flush().unwrap();
}

struct MultiCpuSource {
    ram: Vec<u8>,
}

impl MultiCpuSource {
    fn new(ram_len: usize) -> Self {
        let mut ram = Vec::with_capacity(ram_len);
        ram.extend((0..ram_len).map(|i| (i as u8).wrapping_mul(17)));
        Self { ram }
    }
}

impl SnapshotSource for MultiCpuSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 2,
            parent_snapshot_id: Some(1),
            created_unix_ms: 0,
            label: Some("xtask-multicpu".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn cpu_states(&self) -> Vec<VcpuSnapshot> {
        vec![
            VcpuSnapshot {
                apic_id: 0,
                cpu: CpuState::default(),
                internal_state: vec![0xAA, 0xBB, 0xCC, 0xDD],
            },
            VcpuSnapshot {
                apic_id: 1,
                cpu: CpuState::default(),
                internal_state: Vec::new(),
            },
        ]
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

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(aero_snapshot::SnapshotError::Corrupt("ram read overflow"))?;
        buf.copy_from_slice(&self.ram[offset..end]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

struct DirtyRamSource {
    ram: Vec<u8>,
    dirty_pages: Option<Vec<u64>>,
}

impl DirtyRamSource {
    fn new(ram_len: usize, dirty_pages: Vec<u64>) -> Self {
        let mut ram = Vec::with_capacity(ram_len);
        ram.extend((0..ram_len).map(|i| (i as u8).wrapping_mul(23)));
        Self {
            ram,
            dirty_pages: Some(dirty_pages),
        }
    }
}

impl SnapshotSource for DirtyRamSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 3,
            parent_snapshot_id: Some(2),
            created_unix_ms: 0,
            label: Some("xtask-dirty".to_string()),
        }
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

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(aero_snapshot::SnapshotError::Corrupt("ram read overflow"))?;
        buf.copy_from_slice(&self.ram[offset..end]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        self.dirty_pages.take()
    }
}

struct DirtyNoParentSource {
    ram: Vec<u8>,
    dirty_pages: Option<Vec<u64>>,
}

impl DirtyNoParentSource {
    fn new(ram_len: usize, dirty_pages: Vec<u64>) -> Self {
        let mut ram = Vec::with_capacity(ram_len);
        ram.extend((0..ram_len).map(|i| (i as u8).wrapping_mul(29)));
        Self {
            ram,
            dirty_pages: Some(dirty_pages),
        }
    }
}

impl SnapshotSource for DirtyNoParentSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 4,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: Some("xtask-dirty-no-parent".to_string()),
        }
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

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(aero_snapshot::SnapshotError::Corrupt("ram read overflow"))?;
        buf.copy_from_slice(&self.ram[offset..end]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        self.dirty_pages.take()
    }
}

struct TwoDeviceSource {
    ram: Vec<u8>,
}

impl TwoDeviceSource {
    fn new(ram_len: usize) -> Self {
        let mut ram = Vec::with_capacity(ram_len);
        ram.extend((0..ram_len).map(|i| (i as u8).wrapping_mul(19)));
        Self { ram }
    }
}

impl SnapshotSource for TwoDeviceSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 10,
            parent_snapshot_id: Some(9),
            created_unix_ms: 0,
            label: Some("xtask-two-devices".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        vec![
            DeviceState {
                id: DeviceId::PIT,
                version: 1,
                flags: 0,
                data: vec![1],
            },
            DeviceState {
                id: DeviceId::SERIAL,
                version: 1,
                flags: 0,
                data: vec![2, 3],
            },
        ]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(aero_snapshot::SnapshotError::Corrupt("ram read overflow"))?;
        buf.copy_from_slice(&self.ram[offset..end]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

fn read_u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap())
}

fn read_u64_le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[..8].try_into().unwrap())
}

fn corrupt_first_vcpu_internal_len(snapshot: &mut [u8]) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let cpus = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::CPUS)
        .expect("CPUS section missing");
    let start = cpus.offset as usize;
    assert!(start + 4 + 8 <= snapshot.len());
    let count = read_u32_le(&snapshot[start..start + 4]);
    assert!(count > 0);

    // First entry framing: u64 entry_len followed by the vCPU entry itself.
    let entry_len_off = start + 4;
    let entry_len = read_u64_le(&snapshot[entry_len_off..entry_len_off + 8]) as usize;
    let entry_start = entry_len_off + 8;
    assert!(entry_start + entry_len <= snapshot.len());

    // Entry layout v2: apic_id (u32) + CpuState::encode_v2 + internal_len (u64) + internal_state.
    let cpu_len = {
        let mut tmp = Vec::new();
        CpuState::default().encode_v2(&mut tmp).unwrap();
        tmp.len()
    };

    let internal_len_off = entry_start + 4 + cpu_len;
    assert!(internal_len_off + 8 <= snapshot.len());
    let old = read_u64_le(&snapshot[internal_len_off..internal_len_off + 8]);
    let new = old + 1;
    snapshot[internal_len_off..internal_len_off + 8].copy_from_slice(&new.to_le_bytes());
}

fn corrupt_second_vcpu_apic_id(snapshot: &mut [u8], new_apic_id: u32) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let cpus = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::CPUS)
        .expect("CPUS section missing");

    let mut off = cpus.offset as usize;
    assert!(off + 4 <= snapshot.len());
    let count = read_u32_le(&snapshot[off..off + 4]);
    assert!(count >= 2);
    off += 4;

    // Entry framing: u64 entry_len followed by entry payload.
    assert!(off + 8 <= snapshot.len());
    let entry_len0 = read_u64_le(&snapshot[off..off + 8]) as usize;
    let entry0_start = off + 8;
    let entry0_end = entry0_start + entry_len0;
    assert!(entry0_end <= snapshot.len());
    off = entry0_end;

    assert!(off + 8 <= snapshot.len());
    let entry_len1 = read_u64_le(&snapshot[off..off + 8]) as usize;
    let entry1_start = off + 8;
    let entry1_end = entry1_start + entry_len1;
    assert!(entry1_end <= snapshot.len());

    // vCPU entry begins with u32 apic_id.
    assert!(entry1_start + 4 <= snapshot.len());
    snapshot[entry1_start..entry1_start + 4].copy_from_slice(&new_apic_id.to_le_bytes());
}

fn append_duplicate_section(snapshot: &mut Vec<u8>, id: SectionId) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(snapshot.as_slice())).unwrap();
    let section = index
        .sections
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("{id} section missing"));
    let header_start = section
        .offset
        .checked_sub(16)
        .expect("section offset underflow") as usize;
    let payload_len: usize = section.len.try_into().expect("section len fits usize");
    let end = header_start + 16 + payload_len;
    assert!(end <= snapshot.len());
    let section_bytes = snapshot[header_start..end].to_vec();
    snapshot.extend_from_slice(&section_bytes);
}

fn move_meta_after_ram(snapshot: &[u8]) -> Vec<u8> {
    const FILE_HEADER_LEN: usize = 16;

    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(snapshot)).unwrap();
    let mut meta_section: Option<Vec<u8>> = None;
    let mut ram_section: Option<Vec<u8>> = None;
    let mut other_sections = Vec::new();

    for section in &index.sections {
        let header_start = section
            .offset
            .checked_sub(16)
            .expect("section offset underflow") as usize;
        let payload_len: usize = section.len.try_into().expect("section len fits usize");
        let end = header_start + 16 + payload_len;
        assert!(end <= snapshot.len());
        let bytes = snapshot[header_start..end].to_vec();

        match section.id {
            id if id == SectionId::META => meta_section = Some(bytes),
            id if id == SectionId::RAM => ram_section = Some(bytes),
            _ => other_sections.push(bytes),
        }
    }

    let meta_section = meta_section.expect("META section missing");
    let ram_section = ram_section.expect("RAM section missing");
    assert!(FILE_HEADER_LEN <= snapshot.len());

    let mut out = Vec::new();
    out.extend_from_slice(&snapshot[..FILE_HEADER_LEN]);
    out.extend_from_slice(&ram_section);
    for section in other_sections {
        out.extend_from_slice(&section);
    }
    out.extend_from_slice(&meta_section);
    out
}

fn corrupt_second_device_entry_to_duplicate_first(snapshot: &mut [u8]) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let devices = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::DEVICES)
        .expect("DEVICES section missing");

    let mut off = devices.offset as usize;
    let count = read_u32_le(&snapshot[off..off + 4]);
    assert!(count >= 2);
    off += 4;

    let id0 = read_u32_le(&snapshot[off..off + 4]);
    let version0 = u16::from_le_bytes(snapshot[off + 4..off + 6].try_into().unwrap());
    let flags0 = u16::from_le_bytes(snapshot[off + 6..off + 8].try_into().unwrap());
    let len0 = read_u64_le(&snapshot[off + 8..off + 16]) as usize;

    let entry1 = off + 16 + len0;
    assert!(entry1 + 8 <= (devices.offset + devices.len) as usize);
    snapshot[entry1..entry1 + 4].copy_from_slice(&id0.to_le_bytes());
    snapshot[entry1 + 4..entry1 + 6].copy_from_slice(&version0.to_le_bytes());
    snapshot[entry1 + 6..entry1 + 8].copy_from_slice(&flags0.to_le_bytes());
}

fn corrupt_devices_section_len_to_two_and_insert_ram(snapshot: &mut Vec<u8>) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let meta = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::META)
        .expect("META section missing");
    let devices = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::DEVICES)
        .expect("DEVICES section missing");

    // Section header is immediately before `offset`.
    let header_start = devices
        .offset
        .checked_sub(16)
        .expect("section offset underflow") as usize;
    let payload_start = devices.offset as usize;

    // DEVICES header layout: id(u32), version(u16), flags(u16), len(u64).
    snapshot[header_start + 8..header_start + 16].copy_from_slice(&2u64.to_le_bytes());

    // DEVICES payload is now only 2 bytes (intentionally too short for the u32 device count).
    snapshot[payload_start..payload_start + 2].fill(0);

    // Ensure the snapshot still satisfies the dirty-snapshot META contract. This helper injects a
    // `RAM` section in dirty mode (because it's the smallest framing), which requires
    // `parent_snapshot_id` to be present even if the dirty page list is empty.
    //
    // Encode a shorter META payload in-place (leave trailing bytes untouched).
    let meta_off = meta.offset as usize;
    assert!(meta_off + 26 <= snapshot.len());
    snapshot[meta_off..meta_off + 8].copy_from_slice(&1u64.to_le_bytes()); // snapshot_id
    snapshot[meta_off + 8] = 1; // parent_present
    snapshot[meta_off + 9..meta_off + 17].copy_from_slice(&0u64.to_le_bytes()); // parent_snapshot_id
    snapshot[meta_off + 17..meta_off + 25].copy_from_slice(&0u64.to_le_bytes()); // created_unix_ms
    snapshot[meta_off + 25] = 0; // label_present

    // Insert an unknown 0-length section header immediately after the truncated payload so that
    // `inspect_snapshot` still sees a structurally valid file.
    let unknown_header = payload_start + 2;
    snapshot[unknown_header..unknown_header + 4].copy_from_slice(&0u32.to_le_bytes()); // id
    snapshot[unknown_header + 4..unknown_header + 6].copy_from_slice(&1u16.to_le_bytes()); // version
    snapshot[unknown_header + 6..unknown_header + 8].copy_from_slice(&0u16.to_le_bytes()); // flags
    snapshot[unknown_header + 8..unknown_header + 16].copy_from_slice(&0u64.to_le_bytes()); // len

    // Follow with a minimal dirty-RAM section so validate_index still finds RAM.
    let ram_header = unknown_header + 16;
    snapshot[ram_header..ram_header + 4].copy_from_slice(&SectionId::RAM.0.to_le_bytes()); // id
    snapshot[ram_header + 4..ram_header + 6].copy_from_slice(&1u16.to_le_bytes()); // version
    snapshot[ram_header + 6..ram_header + 8].copy_from_slice(&0u16.to_le_bytes()); // flags
    snapshot[ram_header + 8..ram_header + 16].copy_from_slice(&24u64.to_le_bytes()); // len

    let ram_payload = ram_header + 16;
    let total_len = 4096u64;
    snapshot[ram_payload..ram_payload + 8].copy_from_slice(&total_len.to_le_bytes());
    snapshot[ram_payload + 8..ram_payload + 12].copy_from_slice(&4096u32.to_le_bytes()); // page_size
    snapshot[ram_payload + 12] = 1; // RamMode::Dirty
    snapshot[ram_payload + 13] = 0; // Compression::None
    snapshot[ram_payload + 14..ram_payload + 16].copy_from_slice(&0u16.to_le_bytes()); // reserved
    snapshot[ram_payload + 16..ram_payload + 24].copy_from_slice(&0u64.to_le_bytes()); // dirty_count

    snapshot.truncate(ram_payload + 24);
}

fn write_sparse_snapshot_with_large_devices_section(original: &[u8], out: &std::path::Path) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(original)).unwrap();
    let devices = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::DEVICES)
        .expect("DEVICES section missing");

    // Section header is immediately before `offset`.
    let header_start = (devices.offset - 16) as usize;
    let old_end = (devices.offset + devices.len) as usize;

    let new_len = MAX_DEVICES_SECTION_LEN + 1;

    let mut prefix = original[..old_end].to_vec();
    prefix[header_start + 8..header_start + 16].copy_from_slice(&new_len.to_le_bytes());

    let tail = &original[old_end..];

    let mut file = fs::File::create(out).unwrap();
    file.write_all(&prefix).unwrap();
    file.seek(std::io::SeekFrom::Start(devices.offset + new_len))
        .unwrap();
    file.write_all(tail).unwrap();
    file.flush().unwrap();
}

fn swap_first_two_dirty_page_indices(snapshot: &mut [u8]) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let ram = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::RAM)
        .expect("RAM section missing");
    let start = ram.offset as usize;
    assert!(start + 24 <= snapshot.len());
    assert_eq!(snapshot[start + 12], RamMode::Dirty as u8);
    assert_eq!(snapshot[start + 13], Compression::None as u8);

    let count = read_u64_le(&snapshot[start + 16..start + 24]);
    assert!(count >= 2);

    let entry0 = start + 24;
    let compressed_len0 = read_u32_le(&snapshot[entry0 + 12..entry0 + 16]) as usize;
    let entry1 = entry0 + 8 + 4 + 4 + compressed_len0;
    assert!(entry1 + 8 <= snapshot.len());

    let mut tmp0 = [0u8; 8];
    tmp0.copy_from_slice(&snapshot[entry0..entry0 + 8]);
    let mut tmp1 = [0u8; 8];
    tmp1.copy_from_slice(&snapshot[entry1..entry1 + 8]);
    snapshot[entry0..entry0 + 8].copy_from_slice(&tmp1);
    snapshot[entry1..entry1 + 8].copy_from_slice(&tmp0);
}

#[test]
fn snapshot_inspect_prints_meta_and_ram_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("test.aerosnap");
    write_snapshot(&snap);

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("META:"))
        .stdout(predicate::str::contains("snapshot_id: 1"))
        .stdout(predicate::str::contains("label: xtask-test"))
        .stdout(predicate::str::contains("Sections:"))
        .stdout(predicate::str::contains("offset="))
        .stdout(predicate::str::contains("DEVICES:"))
        .stdout(predicate::str::contains("SERIAL("))
        .stdout(predicate::str::contains("RAM:"))
        .stdout(predicate::str::contains("mode: full"))
        .stdout(predicate::str::contains("compression: lz4"));
}

struct UnsetDiskRefSource {
    ram: Vec<u8>,
}

impl UnsetDiskRefSource {
    fn new(ram_len: usize) -> Self {
        let mut ram = Vec::with_capacity(ram_len);
        ram.extend((0..ram_len).map(|i| (i as u8).wrapping_mul(29)));
        Self { ram }
    }
}

impl SnapshotSource for UnsetDiskRefSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 3,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: Some("xtask-unset-disks".to_string()),
        }
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
        DiskOverlayRefs {
            disks: vec![DiskOverlayRef {
                disk_id: 0,
                base_image: String::new(),
                overlay_image: String::new(),
            }],
        }
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(aero_snapshot::SnapshotError::Corrupt("ram read overflow"))?;
        buf.copy_from_slice(&self.ram[offset..end]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

fn write_snapshot_with_unset_disk_refs(path: &std::path::Path) {
    let mut file = fs::File::create(path).unwrap();
    file.rewind().unwrap();
    let mut source = UnsetDiskRefSource::new(4096);
    aero_snapshot::save_snapshot(&mut file, &mut source, SaveOptions::default()).unwrap();
    file.flush().unwrap();
}

#[test]
fn snapshot_inspect_displays_unset_disk_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("unset_disks.aerosnap");
    write_snapshot_with_unset_disk_refs(&snap);

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("DISKS:"))
        .stdout(predicate::str::contains("count: 1"))
        .stdout(predicate::str::contains("base_image=\"<unset>\""))
        .stdout(predicate::str::contains("overlay_image=\"<unset>\""));
}

#[test]
fn snapshot_validate_and_deep_validate_succeed() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("test.aerosnap");
    write_snapshot(&snap);

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid snapshot"));

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", "--deep", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid snapshot"));
}

#[test]
fn snapshot_validate_fails_on_truncated_files() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("test.aerosnap");
    write_snapshot(&snap);

    let bytes = fs::read(&snap).unwrap();
    assert!(bytes.len() > 16);

    let truncated = tmp.path().join("truncated.aerosnap");
    fs::write(&truncated, &bytes[..bytes.len() - 8]).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", truncated.to_str().unwrap()])
        .assert()
        .failure();
}

#[test]
fn snapshot_validate_supports_multi_cpu_and_rejects_corrupt_internal_len() {
    let tmp = tempfile::tempdir().unwrap();
    let good = tmp.path().join("multi.aerosnap");
    let mut source = MultiCpuSource::new(4096);

    let mut cursor = Cursor::new(Vec::new());
    aero_snapshot::save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let bytes = cursor.into_inner();

    // Sanity: snapshot should contain a CPUS section.
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&bytes)).unwrap();
    assert!(index.sections.iter().any(|s| s.id == SectionId::CPUS));

    fs::write(&good, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", good.to_str().unwrap()])
        .assert()
        .success();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", "--deep", good.to_str().unwrap()])
        .assert()
        .success();

    // Corrupt only the vCPU internal_len field (keep section framing intact).
    let mut corrupt = bytes.clone();
    corrupt_first_vcpu_internal_len(&mut corrupt);
    let corrupt_path = tmp.path().join("corrupt_internal_len.aerosnap");
    fs::write(&corrupt_path, &corrupt).unwrap();

    // Inspect should still succeed (it doesn't decode CPUS payloads).
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", corrupt_path.to_str().unwrap()])
        .assert()
        .success();

    // Validation should detect the truncated internal_state payload implied by internal_len.
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", corrupt_path.to_str().unwrap()])
        .assert()
        .failure();
}

#[test]
fn snapshot_validate_rejects_duplicate_meta_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dup_meta.aerosnap");
    write_snapshot(&snap);

    let mut bytes = fs::read(&snap).unwrap();
    append_duplicate_section(&mut bytes, SectionId::META);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("duplicate META section"));
}

#[test]
fn snapshot_validate_rejects_duplicate_cpu_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dup_cpu.aerosnap");
    write_snapshot(&snap);

    let mut bytes = fs::read(&snap).unwrap();
    append_duplicate_section(&mut bytes, SectionId::CPU);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("duplicate CPU/CPUS section"));
}

#[test]
fn snapshot_validate_rejects_duplicate_ram_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dup_ram.aerosnap");
    write_snapshot(&snap);

    let mut bytes = fs::read(&snap).unwrap();
    append_duplicate_section(&mut bytes, SectionId::RAM);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("duplicate RAM section"));
}

#[test]
fn snapshot_validate_rejects_duplicate_mmu_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dup_mmu.aerosnap");
    write_snapshot(&snap);

    let mut bytes = fs::read(&snap).unwrap();
    append_duplicate_section(&mut bytes, SectionId::MMU);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("duplicate MMU section"));
}

#[test]
fn snapshot_validate_rejects_duplicate_devices_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dup_devices_section.aerosnap");
    write_snapshot(&snap);

    let mut bytes = fs::read(&snap).unwrap();
    append_duplicate_section(&mut bytes, SectionId::DEVICES);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("duplicate DEVICES section"));
}

#[test]
fn snapshot_validate_rejects_duplicate_disks_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dup_disks_section.aerosnap");
    write_snapshot(&snap);

    let mut bytes = fs::read(&snap).unwrap();
    append_duplicate_section(&mut bytes, SectionId::DISKS);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("duplicate DISKS section"));
}

#[test]
fn snapshot_validate_rejects_duplicate_apic_ids_in_cpus_section() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dup_apic.aerosnap");

    let mut source = MultiCpuSource::new(4096);
    let mut cursor = Cursor::new(Vec::new());
    aero_snapshot::save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let mut bytes = cursor.into_inner();
    corrupt_second_vcpu_apic_id(&mut bytes, 0);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "duplicate APIC ID in CPU list (apic_id must be unique)",
        ));
}

#[test]
fn snapshot_validate_rejects_dirty_ram_page_list_not_strictly_increasing() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dirty_unsorted.aerosnap");

    let options = SaveOptions {
        ram: RamWriteOptions {
            mode: RamMode::Dirty,
            compression: Compression::None,
            page_size: 4096,
            chunk_size: 1024 * 1024,
        },
    };
    let mut source = DirtyRamSource::new(4096 * 2, vec![0, 1]);
    let mut cursor = Cursor::new(Vec::new());
    aero_snapshot::save_snapshot(&mut cursor, &mut source, options).unwrap();
    let mut bytes = cursor.into_inner();

    swap_first_two_dirty_page_indices(&mut bytes);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "dirty page list not strictly increasing",
        ));
}

#[test]
fn snapshot_validate_rejects_dirty_snapshot_missing_parent_snapshot_id() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dirty_no_parent.aerosnap");

    let options = SaveOptions {
        ram: RamWriteOptions {
            mode: RamMode::Dirty,
            compression: Compression::None,
            page_size: 4096,
            chunk_size: 1024 * 1024,
        },
    };

    let mut source = DirtyNoParentSource::new(4096, vec![0]);
    let mut cursor = Cursor::new(Vec::new());
    aero_snapshot::save_snapshot(&mut cursor, &mut source, options).unwrap();
    fs::write(&snap, cursor.into_inner()).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "dirty snapshot missing parent_snapshot_id",
        ));
}

#[test]
fn snapshot_validate_rejects_dirty_snapshot_meta_after_ram() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dirty_meta_after_ram.aerosnap");

    let options = SaveOptions {
        ram: RamWriteOptions {
            mode: RamMode::Dirty,
            compression: Compression::None,
            page_size: 4096,
            chunk_size: 1024 * 1024,
        },
    };

    let mut source = DirtyRamSource::new(4096, vec![0]);
    let mut cursor = Cursor::new(Vec::new());
    aero_snapshot::save_snapshot(&mut cursor, &mut source, options).unwrap();
    let bytes = cursor.into_inner();

    let corrupt = move_meta_after_ram(&bytes);
    fs::write(&snap, corrupt).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "dirty snapshot requires META section before RAM",
        ));
}

#[test]
fn snapshot_validate_rejects_duplicate_device_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dup_device_entry.aerosnap");

    let mut source = TwoDeviceSource::new(4096);
    let mut cursor = Cursor::new(Vec::new());
    aero_snapshot::save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let mut bytes = cursor.into_inner();
    corrupt_second_device_entry_to_duplicate_first(&mut bytes);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "duplicate device entry (id/version/flags must be unique)",
        ));
}

#[test]
fn snapshot_validate_rejects_truncated_devices_section() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("corrupt_devices.aerosnap");
    write_snapshot(&snap);

    let mut bytes = fs::read(&snap).unwrap();
    corrupt_devices_section_len_to_two_and_insert_ram(&mut bytes);
    fs::write(&snap, &bytes).unwrap();

    // Inspect should succeed: the file is still structurally parseable.
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success();

    // Validation should fail because the DEVICES section is too short to contain its u32 count.
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("device count: truncated section"));
}

#[test]
fn snapshot_validate_rejects_devices_section_too_large() {
    let tmp = tempfile::tempdir().unwrap();
    let orig_path = tmp.path().join("orig.aerosnap");
    write_snapshot(&orig_path);
    let bytes = fs::read(&orig_path).unwrap();

    let snap = tmp.path().join("devices_too_large.aerosnap");
    write_sparse_snapshot_with_large_devices_section(&bytes, &snap);

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("devices section too large"));
}

#[test]
fn snapshot_validate_accepts_unknown_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("unknown_section.aerosnap");
    write_snapshot(&snap);
    let mut bytes = fs::read(&snap).unwrap();

    // Append an unknown section with a tiny payload.
    let mut section = Vec::new();
    section.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes()); // id
    section.extend_from_slice(&1u16.to_le_bytes()); // version
    section.extend_from_slice(&0u16.to_le_bytes()); // flags
    section.extend_from_slice(&4u64.to_le_bytes()); // len
    section.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
    bytes.extend_from_slice(&section);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .success();

    // Deep validation should also tolerate unknown sections (restore_snapshot skips them).
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", "--deep", snap.to_str().unwrap()])
        .assert()
        .success();
}

struct DiskSource;

impl SnapshotSource for DiskSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 4,
            parent_snapshot_id: Some(3),
            created_unix_ms: 0,
            label: Some("xtask-disks".to_string()),
        }
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
        DiskOverlayRefs {
            disks: vec![DiskOverlayRef {
                disk_id: 0,
                base_image: "base.img".to_string(),
                overlay_image: "overlay.img".to_string(),
            }],
        }
    }

    fn ram_len(&self) -> usize {
        4096
    }

    fn read_ram(&self, _offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        buf.fill(0);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

fn write_disk_snapshot(path: &std::path::Path) -> Vec<u8> {
    let mut source = DiskSource;
    let mut cursor = Cursor::new(Vec::new());
    aero_snapshot::save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let bytes = cursor.into_inner();
    fs::write(path, &bytes).unwrap();
    bytes
}

struct TwoDiskSource;

impl SnapshotSource for TwoDiskSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 5,
            parent_snapshot_id: Some(4),
            created_unix_ms: 0,
            label: Some("xtask-two-disks".to_string()),
        }
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
        DiskOverlayRefs {
            disks: vec![
                DiskOverlayRef {
                    disk_id: 0,
                    base_image: "base0.img".to_string(),
                    overlay_image: "overlay0.img".to_string(),
                },
                DiskOverlayRef {
                    disk_id: 1,
                    base_image: "base1.img".to_string(),
                    overlay_image: "overlay1.img".to_string(),
                },
            ],
        }
    }

    fn ram_len(&self) -> usize {
        4096
    }

    fn read_ram(&self, _offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        buf.fill(0);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

fn write_two_disk_snapshot(path: &std::path::Path) -> Vec<u8> {
    let mut source = TwoDiskSource;
    let mut cursor = Cursor::new(Vec::new());
    aero_snapshot::save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let bytes = cursor.into_inner();
    fs::write(path, &bytes).unwrap();
    bytes
}

fn corrupt_disks_base_image_first_byte(snapshot: &mut [u8]) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let disks = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::DISKS)
        .expect("DISKS section missing");

    let mut off = disks.offset as usize;
    let count = read_u32_le(&snapshot[off..off + 4]);
    assert!(count > 0);
    off += 4;

    // First disk entry.
    off += 4; // disk_id
    let base_len = read_u32_le(&snapshot[off..off + 4]) as usize;
    off += 4;
    assert!(base_len > 0);
    snapshot[off] = 0xFF;
}

fn corrupt_disks_base_image_len(snapshot: &mut [u8], new_len: u32) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let disks = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::DISKS)
        .expect("DISKS section missing");

    let mut off = disks.offset as usize;
    let count = read_u32_le(&snapshot[off..off + 4]);
    assert!(count > 0);
    off += 4;

    // First disk entry.
    off += 4; // disk_id
    snapshot[off..off + 4].copy_from_slice(&new_len.to_le_bytes());
}

fn corrupt_second_disk_id_to_duplicate_first(snapshot: &mut [u8]) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let disks = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::DISKS)
        .expect("DISKS section missing");

    let mut off = disks.offset as usize;
    let end = (disks.offset + disks.len) as usize;
    assert!(off + 4 <= end);
    let count = read_u32_le(&snapshot[off..off + 4]);
    assert!(count >= 2);
    off += 4;

    assert!(off + 4 <= end);
    let disk_id0 = read_u32_le(&snapshot[off..off + 4]);
    off += 4;

    assert!(off + 4 <= end);
    let base_len0 = read_u32_le(&snapshot[off..off + 4]) as usize;
    off += 4;
    assert!(off + base_len0 <= end);
    off += base_len0;

    assert!(off + 4 <= end);
    let overlay_len0 = read_u32_le(&snapshot[off..off + 4]) as usize;
    off += 4;
    assert!(off + overlay_len0 <= end);
    off += overlay_len0;

    assert!(off + 4 <= end);
    snapshot[off..off + 4].copy_from_slice(&disk_id0.to_le_bytes());
}

fn corrupt_cpus_count_to_zero(snapshot: &mut [u8]) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let cpus = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::CPUS)
        .expect("CPUS section missing");
    let off = cpus.offset as usize;
    snapshot[off..off + 4].copy_from_slice(&0u32.to_le_bytes());
}

#[test]
fn snapshot_validate_rejects_disks_invalid_utf8() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("disks_invalid_utf8.aerosnap");
    let mut bytes = write_disk_snapshot(&snap);
    corrupt_disks_base_image_first_byte(&mut bytes);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("disk base_image: invalid utf-8"));
}

#[test]
fn snapshot_validate_rejects_duplicate_disk_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dup_disk_entry.aerosnap");
    let mut bytes = write_two_disk_snapshot(&snap);
    corrupt_second_disk_id_to_duplicate_first(&mut bytes);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "duplicate disk entry (disk_id must be unique)",
        ));
}

#[test]
fn snapshot_validate_rejects_disks_path_too_long() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("disks_path_too_long.aerosnap");
    let mut bytes = write_disk_snapshot(&snap);
    // > 64KiB
    corrupt_disks_base_image_len(&mut bytes, 64 * 1024 + 1);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("disk base_image too long"));
}

#[test]
fn snapshot_validate_rejects_disks_truncated_string_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("disks_truncated_string.aerosnap");
    let mut bytes = write_disk_snapshot(&snap);
    // Max allowed length (64KiB) but larger than the actual remaining bytes in the section.
    corrupt_disks_base_image_len(&mut bytes, 64 * 1024);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "disk base_image: truncated string bytes",
        ));
}

#[test]
fn snapshot_validate_rejects_zero_cpu_count() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("zero_cpu_count.aerosnap");

    let mut source = MultiCpuSource::new(4096);
    let mut cursor = Cursor::new(Vec::new());
    aero_snapshot::save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let mut bytes = cursor.into_inner();
    corrupt_cpus_count_to_zero(&mut bytes);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing CPU entry"));
}

#[test]
fn snapshot_validate_rejects_too_many_cpus() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("too_many_cpus.aerosnap");

    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    // CPUS section with count=257 (one more than the restore-time MAX_CPU_COUNT=256). Payload is
    // intentionally only 4 bytes: validation rejects based on the count alone.
    bytes.extend_from_slice(&SectionId::CPUS.0.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&4u64.to_le_bytes());
    bytes.extend_from_slice(&257u32.to_le_bytes());

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size

    bytes.extend_from_slice(&SectionId::RAM.0.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes.extend_from_slice(&(ram_payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&ram_payload);

    fs::write(&snap, bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("too many CPUs"));
}

struct LargeDirtySource;

impl SnapshotSource for LargeDirtySource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 3,
            parent_snapshot_id: Some(2),
            created_unix_ms: 0,
            label: Some("xtask-large-ram".to_string()),
        }
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
        // Exceeds the `--deep` safety limit (512MiB) but should still be cheap to save because
        // dirty mode with an empty dirty-page set does not read RAM.
        512 * 1024 * 1024 + 4096
    }

    fn read_ram(&self, _offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        // Should never be called for this test (no dirty pages), but keep it correct.
        buf.fill(0);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        Some(Vec::new())
    }
}

#[test]
fn snapshot_deep_validate_refuses_large_ram() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("large_dirty.aerosnap");
    let mut source = LargeDirtySource;

    let mut opts = SaveOptions::default();
    opts.ram = RamWriteOptions {
        mode: RamMode::Dirty,
        ..opts.ram
    };

    let mut file = fs::File::create(&snap).unwrap();
    file.rewind().unwrap();
    aero_snapshot::save_snapshot(&mut file, &mut source, opts).unwrap();
    file.flush().unwrap();

    // Non-deep validation should succeed (it doesn't restore RAM).
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", snap.to_str().unwrap()])
        .assert()
        .success();

    // Deep validation should refuse before attempting to restore.
    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "validate", "--deep", snap.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "refuses to restore snapshots with RAM >",
        ));
}
