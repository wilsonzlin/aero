#![cfg(not(target_arch = "wasm32"))]

use std::io::{Cursor, ErrorKind};

use aero_snapshot::{
    inspect_snapshot, limits, restore_snapshot, save_snapshot, Compression, CpuInternalState,
    CpuState, DeviceId, DeviceState, DiskOverlayRef, DiskOverlayRefs, MmuState, RamMode,
    RamWriteOptions, Result, SaveOptions, SectionId, SnapshotError, SnapshotMeta, SnapshotSource,
    SnapshotTarget, VcpuSnapshot, SNAPSHOT_ENDIANNESS_LITTLE, SNAPSHOT_MAGIC, SNAPSHOT_VERSION_V1,
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

struct DuplicateDeviceSource {
    ram: Vec<u8>,
}

impl SnapshotSource for DuplicateDeviceSource {
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
        vec![
            DeviceState {
                id: DeviceId::PCI,
                version: 1,
                flags: 0,
                data: vec![0xAA],
            },
            DeviceState {
                id: DeviceId::PCI,
                version: 1,
                flags: 0,
                data: vec![0xBB],
            },
        ]
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

struct DuplicateDiskSource {
    ram: Vec<u8>,
}

impl SnapshotSource for DuplicateDiskSource {
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
        DiskOverlayRefs {
            disks: vec![
                DiskOverlayRef {
                    disk_id: 0,
                    base_image: "base.img".to_string(),
                    overlay_image: "overlay_a.img".to_string(),
                },
                DiskOverlayRef {
                    disk_id: 0,
                    base_image: "base.img".to_string(),
                    overlay_image: "overlay_b.img".to_string(),
                },
            ],
        }
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

struct DuplicateCpuSource {
    ram: Vec<u8>,
}

impl SnapshotSource for DuplicateCpuSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta::default()
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn cpu_states(&self) -> Vec<VcpuSnapshot> {
        vec![
            VcpuSnapshot {
                apic_id: 1,
                cpu: CpuState::default(),
                internal_state: Vec::new(),
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

struct EmptyCpuSource {
    ram: Vec<u8>,
}

impl SnapshotSource for EmptyCpuSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta::default()
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn cpu_states(&self) -> Vec<VcpuSnapshot> {
        Vec::new()
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

    fn read_ram(&self, _offset: u64, _buf: &mut [u8]) -> Result<()> {
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

struct TooManyCpuSource {
    ram: Vec<u8>,
}

impl SnapshotSource for TooManyCpuSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta::default()
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn cpu_states(&self) -> Vec<VcpuSnapshot> {
        let count = limits::MAX_CPU_COUNT + 1;
        let mut out = Vec::with_capacity(count as usize);
        for apic_id in 0..count {
            out.push(VcpuSnapshot {
                apic_id,
                cpu: CpuState::default(),
                internal_state: Vec::new(),
            });
        }
        out
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

    fn read_ram(&self, _offset: u64, _buf: &mut [u8]) -> Result<()> {
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

struct TooManyDeviceSource {
    ram: Vec<u8>,
}

impl SnapshotSource for TooManyDeviceSource {
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
        let count = limits::MAX_DEVICE_COUNT + 1;
        let mut out = Vec::with_capacity(count as usize);
        for id in 0..count {
            out.push(DeviceState {
                id: DeviceId(id),
                version: 1,
                flags: 0,
                data: Vec::new(),
            });
        }
        out
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn read_ram(&self, _offset: u64, _buf: &mut [u8]) -> Result<()> {
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

struct TooManyDiskSource {
    ram: Vec<u8>,
}

impl SnapshotSource for TooManyDiskSource {
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
        let count = limits::MAX_DISK_REFS + 1;
        let mut disks = Vec::with_capacity(count as usize);
        for disk_id in 0..count {
            disks.push(DiskOverlayRef {
                disk_id,
                base_image: "base.img".to_string(),
                overlay_image: "overlay.img".to_string(),
            });
        }
        DiskOverlayRefs { disks }
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn read_ram(&self, _offset: u64, _buf: &mut [u8]) -> Result<()> {
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

struct LongDiskPathSource {
    ram: Vec<u8>,
}

impl SnapshotSource for LongDiskPathSource {
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
        DiskOverlayRefs {
            disks: vec![DiskOverlayRef {
                disk_id: 0,
                base_image: "x".repeat((limits::MAX_DISK_PATH_LEN + 1) as usize),
                overlay_image: "overlay.img".to_string(),
            }],
        }
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn read_ram(&self, _offset: u64, _buf: &mut [u8]) -> Result<()> {
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

fn minimal_snapshot_with_ram_payload(ram_payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut cpu_payload = Vec::new();
    CpuState::default().encode(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 1, 0, &cpu_payload);
    push_section(&mut bytes, SectionId::RAM, 1, 0, ram_payload);
    bytes
}

#[test]
fn cpu_internal_state_encode_rejects_too_many_pending_interrupts() {
    let state = CpuInternalState {
        interrupt_inhibit: 0,
        pending_external_interrupts: vec![0u8; (limits::MAX_PENDING_INTERRUPTS + 1) as usize],
    };
    let err = state.to_device_state().unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("too many pending interrupts")
    ));
}

#[test]
fn save_snapshot_rejects_duplicate_device_entries() {
    let options = SaveOptions {
        ram: RamWriteOptions {
            compression: Compression::None,
            chunk_size: 1024,
            ..RamWriteOptions::default()
        },
    };

    let mut source = DuplicateDeviceSource { ram: vec![0u8; 8] };
    let mut cursor = Cursor::new(Vec::new());
    let err = save_snapshot(&mut cursor, &mut source, options).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("duplicate device entry (id/version/flags must be unique)")
    ));
}

#[test]
fn restore_snapshot_rejects_duplicate_device_entries() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut cpu_payload = Vec::new();
    CpuState::default().encode_v2(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 2, 0, &cpu_payload);

    let mut devices_payload = Vec::new();
    devices_payload.extend_from_slice(&2u32.to_le_bytes());
    DeviceState {
        id: DeviceId::PCI,
        version: 1,
        flags: 0,
        data: vec![0xAA],
    }
    .encode(&mut devices_payload)
    .unwrap();
    DeviceState {
        id: DeviceId::PCI,
        version: 1,
        flags: 0,
        data: vec![0xBB],
    }
    .encode(&mut devices_payload)
    .unwrap();
    push_section(&mut bytes, SectionId::DEVICES, 1, 0, &devices_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("duplicate device entry (id/version/flags must be unique)")
    ));
}

#[test]
fn save_snapshot_rejects_duplicate_disk_entries() {
    let options = SaveOptions {
        ram: RamWriteOptions {
            compression: Compression::None,
            chunk_size: 1024,
            ..RamWriteOptions::default()
        },
    };

    let mut source = DuplicateDiskSource { ram: vec![0u8; 8] };
    let mut cursor = Cursor::new(Vec::new());
    let err = save_snapshot(&mut cursor, &mut source, options).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("duplicate disk entry (disk_id must be unique)")
    ));
}

#[test]
fn restore_snapshot_rejects_duplicate_disk_entries() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut cpu_payload = Vec::new();
    CpuState::default().encode_v2(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 2, 0, &cpu_payload);

    let mut disks_payload = Vec::new();
    DiskOverlayRefs {
        disks: vec![
            DiskOverlayRef {
                disk_id: 0,
                base_image: "base.img".to_string(),
                overlay_image: "overlay_a.img".to_string(),
            },
            DiskOverlayRef {
                disk_id: 0,
                base_image: "base.img".to_string(),
                overlay_image: "overlay_b.img".to_string(),
            },
        ],
    }
    .encode(&mut disks_payload)
    .unwrap();
    push_section(&mut bytes, SectionId::DISKS, 1, 0, &disks_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("duplicate disk entry (disk_id must be unique)")
    ));
}

#[test]
fn save_snapshot_rejects_duplicate_apic_ids_in_cpu_list() {
    let options = SaveOptions {
        ram: RamWriteOptions {
            compression: Compression::None,
            chunk_size: 1024,
            ..RamWriteOptions::default()
        },
    };

    let mut source = DuplicateCpuSource { ram: vec![0u8; 8] };
    let mut cursor = Cursor::new(Vec::new());
    let err = save_snapshot(&mut cursor, &mut source, options).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("duplicate APIC ID in CPU list (apic_id must be unique)")
    ));
}

#[test]
fn save_snapshot_rejects_empty_cpu_list() {
    let options = SaveOptions {
        ram: RamWriteOptions {
            compression: Compression::None,
            chunk_size: 1024,
            ..RamWriteOptions::default()
        },
    };

    let mut source = EmptyCpuSource { ram: Vec::new() };
    let mut cursor = Cursor::new(Vec::new());
    let err = save_snapshot(&mut cursor, &mut source, options).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("missing CPU entry")));
}

#[test]
fn save_snapshot_rejects_too_many_cpus() {
    let options = SaveOptions {
        ram: RamWriteOptions {
            compression: Compression::None,
            chunk_size: 1024,
            ..RamWriteOptions::default()
        },
    };

    let mut source = TooManyCpuSource { ram: Vec::new() };
    let mut cursor = Cursor::new(Vec::new());
    let err = save_snapshot(&mut cursor, &mut source, options).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("too many CPUs")));
}

#[test]
fn save_snapshot_rejects_too_many_devices() {
    let options = SaveOptions {
        ram: RamWriteOptions {
            compression: Compression::None,
            chunk_size: 1024,
            ..RamWriteOptions::default()
        },
    };

    let mut source = TooManyDeviceSource { ram: Vec::new() };
    let mut cursor = Cursor::new(Vec::new());
    let err = save_snapshot(&mut cursor, &mut source, options).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("too many devices")));
}

#[test]
fn save_snapshot_rejects_too_many_disks() {
    let options = SaveOptions {
        ram: RamWriteOptions {
            compression: Compression::None,
            chunk_size: 1024,
            ..RamWriteOptions::default()
        },
    };

    let mut source = TooManyDiskSource { ram: Vec::new() };
    let mut cursor = Cursor::new(Vec::new());
    let err = save_snapshot(&mut cursor, &mut source, options).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("too many disks")));
}

#[test]
fn save_snapshot_rejects_disk_paths_exceeding_max_len() {
    let options = SaveOptions {
        ram: RamWriteOptions {
            compression: Compression::None,
            chunk_size: 1024,
            ..RamWriteOptions::default()
        },
    };

    let mut source = LongDiskPathSource { ram: Vec::new() };
    let mut cursor = Cursor::new(Vec::new());
    let err = save_snapshot(&mut cursor, &mut source, options).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("disk base_image too long")
    ));
}

#[test]
fn restore_snapshot_rejects_duplicate_apic_ids_in_cpus_section() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut cpus_payload = Vec::new();
    cpus_payload.extend_from_slice(&2u32.to_le_bytes());
    for _ in 0..2 {
        let vcpu = VcpuSnapshot {
            apic_id: 1,
            cpu: CpuState::default(),
            internal_state: Vec::new(),
        };
        let mut entry = Vec::new();
        vcpu.encode_v2(&mut entry).unwrap();
        cpus_payload.extend_from_slice(&(entry.len() as u64).to_le_bytes());
        cpus_payload.extend_from_slice(&entry);
    }
    push_section(&mut bytes, SectionId::CPUS, 2, 0, &cpus_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("duplicate APIC ID in CPU list (apic_id must be unique)")
    ));
}

#[test]
fn restore_snapshot_rejects_cpus_section_with_zero_entries() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let cpus_payload = 0u32.to_le_bytes();
    push_section(&mut bytes, SectionId::CPUS, 2, 0, &cpus_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("missing CPU entry")));
}

#[test]
fn restore_snapshot_rejects_excessive_cpu_count() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let cpus_payload = u32::MAX.to_le_bytes();
    push_section(&mut bytes, SectionId::CPUS, 2, 0, &cpus_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("too many CPUs")));
}

#[test]
fn restore_snapshot_rejects_meta_label_too_long() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    // META payload with label_present=1 but length prefix exceeds MAX_LABEL_LEN (4KiB). No label
    // bytes are needed because the decoder should reject based on the length prefix alone.
    let mut meta_payload = Vec::new();
    meta_payload.extend_from_slice(&1u64.to_le_bytes()); // snapshot_id
    meta_payload.push(0); // parent_present
    meta_payload.extend_from_slice(&0u64.to_le_bytes()); // created_unix_ms
    meta_payload.push(1); // label_present
    meta_payload.extend_from_slice(&(limits::MAX_LABEL_LEN + 1).to_le_bytes()); // label len
    push_section(&mut bytes, SectionId::META, 1, 0, &meta_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("label too long")));
}

#[test]
fn restore_snapshot_rejects_meta_label_invalid_utf8() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut meta_payload = Vec::new();
    meta_payload.extend_from_slice(&1u64.to_le_bytes()); // snapshot_id
    meta_payload.push(0); // parent_present
    meta_payload.extend_from_slice(&0u64.to_le_bytes()); // created_unix_ms
    meta_payload.push(1); // label_present
    meta_payload.extend_from_slice(&1u32.to_le_bytes()); // label len
    meta_payload.push(0xFF); // invalid UTF-8 byte
    push_section(&mut bytes, SectionId::META, 1, 0, &meta_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("label: invalid utf-8")
    ));
}

#[test]
fn restore_snapshot_rejects_meta_label_truncated_string_bytes() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut meta_payload = Vec::new();
    meta_payload.extend_from_slice(&1u64.to_le_bytes()); // snapshot_id
    meta_payload.push(0); // parent_present
    meta_payload.extend_from_slice(&0u64.to_le_bytes()); // created_unix_ms
    meta_payload.push(1); // label_present
    meta_payload.extend_from_slice(&1u32.to_le_bytes()); // label len
                                                         // Missing label byte.
    push_section(&mut bytes, SectionId::META, 1, 0, &meta_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("label: truncated string bytes")
    ));
}

#[test]
fn restore_snapshot_rejects_disks_base_image_too_long() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    // DISKS payload with base_image length prefix exceeding MAX_DISK_PATH_LEN (64KiB). No string
    // bytes are needed because the decoder should reject based on the length prefix alone.
    let mut disks_payload = Vec::new();
    disks_payload.extend_from_slice(&1u32.to_le_bytes()); // count
    disks_payload.extend_from_slice(&0u32.to_le_bytes()); // disk_id
    disks_payload.extend_from_slice(&(limits::MAX_DISK_PATH_LEN + 1).to_le_bytes()); // base_image len
    push_section(&mut bytes, SectionId::DISKS, 1, 0, &disks_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("disk base_image too long")
    ));
}

#[test]
fn restore_snapshot_rejects_disks_invalid_utf8() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut disks_payload = Vec::new();
    disks_payload.extend_from_slice(&1u32.to_le_bytes()); // count
    disks_payload.extend_from_slice(&0u32.to_le_bytes()); // disk_id
    disks_payload.extend_from_slice(&1u32.to_le_bytes()); // base_image len
    disks_payload.push(0xFF); // invalid UTF-8 byte
    disks_payload.extend_from_slice(&0u32.to_le_bytes()); // overlay_image len
    push_section(&mut bytes, SectionId::DISKS, 1, 0, &disks_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("disk base_image: invalid utf-8")
    ));
}

#[test]
fn restore_snapshot_rejects_disks_truncated_string_bytes() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    // DISKS payload with base_image length prefix set but no bytes following.
    let mut disks_payload = Vec::new();
    disks_payload.extend_from_slice(&1u32.to_le_bytes()); // count
    disks_payload.extend_from_slice(&0u32.to_le_bytes()); // disk_id
    disks_payload.extend_from_slice(&1u32.to_le_bytes()); // base_image len
    push_section(&mut bytes, SectionId::DISKS, 1, 0, &disks_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("disk base_image: truncated string bytes")
    ));
}

#[test]
fn restore_snapshot_rejects_truncated_device_entry() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    // DEVICES payload containing 1 device entry, but with a non-zero length and no payload bytes.
    let mut devices_payload = Vec::new();
    devices_payload.extend_from_slice(&1u32.to_le_bytes()); // count
    devices_payload.extend_from_slice(&DeviceId::PCI.0.to_le_bytes()); // id
    devices_payload.extend_from_slice(&1u16.to_le_bytes()); // version
    devices_payload.extend_from_slice(&0u16.to_le_bytes()); // flags
    devices_payload.extend_from_slice(&1u64.to_le_bytes()); // len (but missing 1 byte)
    push_section(&mut bytes, SectionId::DEVICES, 1, 0, &devices_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("device entry truncated")
    ));
}

#[test]
fn restore_snapshot_rejects_duplicate_meta_sections() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut meta_payload = Vec::new();
    SnapshotMeta::default().encode(&mut meta_payload).unwrap();
    push_section(&mut bytes, SectionId::META, 1, 0, &meta_payload);
    push_section(&mut bytes, SectionId::META, 1, 0, &meta_payload);

    let mut cpu_payload = Vec::new();
    CpuState::default().encode_v2(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 2, 0, &cpu_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("duplicate META section")
    ));
}

#[test]
fn restore_snapshot_rejects_duplicate_cpu_sections() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut cpu_payload = Vec::new();
    CpuState::default().encode_v2(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 2, 0, &cpu_payload);
    push_section(&mut bytes, SectionId::CPU, 2, 0, &cpu_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("duplicate CPU/CPUS section")
    ));
}

#[test]
fn restore_snapshot_rejects_duplicate_mmu_sections() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut cpu_payload = Vec::new();
    CpuState::default().encode_v2(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 2, 0, &cpu_payload);

    let mut mmu_payload = Vec::new();
    MmuState::default().encode_v2(&mut mmu_payload).unwrap();
    push_section(&mut bytes, SectionId::MMU, 2, 0, &mmu_payload);
    push_section(&mut bytes, SectionId::MMU, 2, 0, &mmu_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("duplicate MMU section")
    ));
}

#[test]
fn restore_snapshot_rejects_duplicate_devices_sections() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut cpu_payload = Vec::new();
    CpuState::default().encode_v2(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 2, 0, &cpu_payload);

    let devices_payload = 0u32.to_le_bytes();
    push_section(&mut bytes, SectionId::DEVICES, 1, 0, &devices_payload);
    push_section(&mut bytes, SectionId::DEVICES, 1, 0, &devices_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("duplicate DEVICES section")
    ));
}

#[test]
fn restore_snapshot_rejects_duplicate_disks_sections() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut cpu_payload = Vec::new();
    CpuState::default().encode_v2(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 2, 0, &cpu_payload);

    let mut disks_payload = Vec::new();
    DiskOverlayRefs::default()
        .encode(&mut disks_payload)
        .unwrap();
    push_section(&mut bytes, SectionId::DISKS, 1, 0, &disks_payload);
    push_section(&mut bytes, SectionId::DISKS, 1, 0, &disks_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("duplicate DISKS section")
    ));
}

#[test]
fn restore_snapshot_rejects_duplicate_ram_sections() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let mut cpu_payload = Vec::new();
    CpuState::default().encode_v2(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 2, 0, &cpu_payload);

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);
    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("duplicate RAM section")
    ));
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
fn restore_snapshot_rejects_truncated_cpus_entry_len() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let vcpu = VcpuSnapshot {
        apic_id: 0,
        cpu: CpuState::default(),
        internal_state: Vec::new(),
    };
    let mut entry = Vec::new();
    vcpu.encode_v2(&mut entry).unwrap();

    // Declare an entry_len larger than the bytes present in the section. Previously, restore would
    // accept this (since it didn't validate entry_len against section bounds).
    let mut cpus_payload = Vec::new();
    cpus_payload.extend_from_slice(&1u32.to_le_bytes());
    cpus_payload.extend_from_slice(&((entry.len() as u64) + 1).to_le_bytes());
    cpus_payload.extend_from_slice(&entry);
    push_section(&mut bytes, SectionId::CPUS, 2, 0, &cpus_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("truncated CPU entry")));
}

#[test]
fn restore_snapshot_rejects_truncated_vcpu_internal_state() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    // Valid vCPU entry, then corrupt the internal_len field to claim 1 byte when there are 0.
    let vcpu = VcpuSnapshot {
        apic_id: 0,
        cpu: CpuState::default(),
        internal_state: Vec::new(),
    };
    let mut entry = Vec::new();
    vcpu.encode_v2(&mut entry).unwrap();

    // Entry layout v2: apic_id (u32) + CpuState::encode_v2 + internal_len (u64) + internal_state.
    let cpu_len = {
        let mut tmp = Vec::new();
        CpuState::default().encode_v2(&mut tmp).unwrap();
        tmp.len()
    };
    let internal_len_off = 4 + cpu_len;
    entry[internal_len_off..internal_len_off + 8].copy_from_slice(&1u64.to_le_bytes());

    let mut cpus_payload = Vec::new();
    cpus_payload.extend_from_slice(&1u32.to_le_bytes());
    cpus_payload.extend_from_slice(&(entry.len() as u64).to_le_bytes());
    cpus_payload.extend_from_slice(&entry);
    push_section(&mut bytes, SectionId::CPUS, 2, 0, &cpus_payload);

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("truncated vCPU internal state")
    ));
}

#[test]
fn restore_snapshot_rejects_cpu_v2_extension_too_large() {
    let options = SaveOptions {
        ram: RamWriteOptions {
            compression: Compression::None,
            chunk_size: 1024,
            ..RamWriteOptions::default()
        },
    };

    let mut source = MinimalSource { ram: Vec::new() };
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, options).unwrap();
    let mut bytes = cursor.into_inner();

    let cpu = {
        let mut r = Cursor::new(bytes.as_slice());
        let index = inspect_snapshot(&mut r).unwrap();
        index
            .sections
            .iter()
            .find(|s| s.id == SectionId::CPU)
            .expect("CPU section missing")
            .clone()
    };
    assert!(
        cpu.len >= 8,
        "CPU section unexpectedly short for v2 extension"
    );
    let ext_len_off = (cpu.offset + cpu.len - 8) as usize;
    let too_large = limits::MAX_CPU_V2_EXT_LEN + 1;
    bytes[ext_len_off..ext_len_off + 4].copy_from_slice(&too_large.to_le_bytes());

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("cpu v2 extension too large")
    ));
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

    let meta = SnapshotMeta {
        snapshot_id: 2,
        parent_snapshot_id: Some(1),
        created_unix_ms: 0,
        label: None,
    };
    let mut meta_payload = Vec::new();
    meta.encode(&mut meta_payload).unwrap();
    push_section(&mut bytes, SectionId::META, 1, 0, &meta_payload);

    let mut cpu_payload = Vec::new();
    CpuState::default().encode(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 2, 0, &cpu_payload);

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

#[test]
fn restore_snapshot_rejects_dirty_ram_page_list_not_strictly_increasing() {
    let total_len = 2 * 4096u64;
    let page_size = 4096u32;

    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());

    let meta = SnapshotMeta {
        snapshot_id: 2,
        parent_snapshot_id: Some(1),
        created_unix_ms: 0,
        label: None,
    };
    let mut meta_payload = Vec::new();
    meta.encode(&mut meta_payload).unwrap();
    push_section(&mut bytes, SectionId::META, 1, 0, &meta_payload);

    let mut cpu_payload = Vec::new();
    CpuState::default().encode(&mut cpu_payload).unwrap();
    push_section(&mut bytes, SectionId::CPU, 2, 0, &cpu_payload);

    let count = 2u64;
    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&total_len.to_le_bytes());
    ram_payload.extend_from_slice(&page_size.to_le_bytes());
    ram_payload.push(RamMode::Dirty as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes());
    ram_payload.extend_from_slice(&count.to_le_bytes());

    // Unsorted page list: 1 then 0. The decoder now requires strict increasing order.
    ram_payload.extend_from_slice(&1u64.to_le_bytes()); // page_idx
    ram_payload.extend_from_slice(&page_size.to_le_bytes()); // uncompressed_len
    ram_payload.extend_from_slice(&page_size.to_le_bytes()); // compressed_len
    ram_payload.extend_from_slice(&vec![0xAAu8; page_size as usize]);

    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // page_idx
    ram_payload.extend_from_slice(&page_size.to_le_bytes()); // uncompressed_len
    ram_payload.extend_from_slice(&page_size.to_le_bytes()); // compressed_len
    ram_payload.extend_from_slice(&vec![0xBBu8; page_size as usize]);

    push_section(&mut bytes, SectionId::RAM, 1, 0, &ram_payload);

    let mut target = DummyTarget::new(total_len as usize);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(
        err,
        SnapshotError::Corrupt("dirty page list not strictly increasing")
    ));
}

#[test]
fn inspect_and_restore_reject_invalid_ram_page_size() {
    // `ram::MAX_PAGE_SIZE` is 2 MiB; keep the test payload minimal by setting total_len=0.
    let invalid_page_size = limits::MAX_RAM_PAGE_SIZE + 1;

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&invalid_page_size.to_le_bytes());
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes());
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size

    let bytes = minimal_snapshot_with_ram_payload(&ram_payload);

    let err = inspect_snapshot(&mut Cursor::new(bytes.as_slice())).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("invalid page size")));

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("invalid page size")));
}

#[test]
fn inspect_and_restore_reject_invalid_ram_chunk_size() {
    // `ram::MAX_CHUNK_SIZE` is 64 MiB; keep the test payload minimal by setting total_len=0.
    let invalid_chunk_size = limits::MAX_RAM_CHUNK_SIZE + 1;

    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes());
    ram_payload.extend_from_slice(&invalid_chunk_size.to_le_bytes());

    let bytes = minimal_snapshot_with_ram_payload(&ram_payload);

    let err = inspect_snapshot(&mut Cursor::new(bytes.as_slice())).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("invalid chunk size")));

    let mut target = DummyTarget::new(0);
    let err = restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap_err();
    assert!(matches!(err, SnapshotError::Corrupt("invalid chunk size")));
}
