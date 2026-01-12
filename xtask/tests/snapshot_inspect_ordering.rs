#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::io::{Cursor, Read};

use aero_snapshot::{
    save_snapshot, CpuState, DeviceId, DeviceState, DiskOverlayRef, DiskOverlayRefs, MmuState,
    SaveOptions, SectionId, SnapshotMeta, SnapshotSource, VcpuSnapshot,
};
use assert_cmd::Command;
use predicates::prelude::*;

fn read_u32_le(r: &mut dyn Read) -> u32 {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).expect("read u32");
    u32::from_le_bytes(buf)
}

fn read_u64_le(r: &mut dyn Read) -> u64 {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf).expect("read u64");
    u64::from_le_bytes(buf)
}

fn rewrite_disks_section<F: FnOnce(&mut DiskOverlayRefs)>(snapshot: &mut [u8], f: F) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let disks = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::DISKS)
        .expect("DISKS section missing");

    let start = disks.offset as usize;
    let len = disks.len as usize;
    let end = start + len;

    let mut decoded = {
        let mut r = Cursor::new(&snapshot[start..end]);
        DiskOverlayRefs::decode(&mut r).unwrap()
    };

    f(&mut decoded);

    let mut out = Vec::new();
    decoded.encode(&mut out).unwrap();
    assert_eq!(
        out.len(),
        len,
        "rewritten DISKS payload must preserve length"
    );
    snapshot[start..end].copy_from_slice(&out);
}

fn rewrite_devices_section<F: FnOnce(&mut Vec<DeviceState>)>(snapshot: &mut [u8], f: F) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let devices = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::DEVICES)
        .expect("DEVICES section missing");

    let start = devices.offset as usize;
    let len = devices.len as usize;
    let end = start + len;

    let (mut states, count) = {
        let mut r = Cursor::new(&snapshot[start..end]);
        let count = read_u32_le(&mut r) as usize;
        let mut states = Vec::with_capacity(count);
        for _ in 0..count {
            states.push(
                DeviceState::decode(&mut r, aero_snapshot::limits::MAX_DEVICE_ENTRY_LEN).unwrap(),
            );
        }
        (states, count)
    };

    assert_eq!(states.len(), count);
    f(&mut states);

    let mut out = Vec::new();
    out.extend_from_slice(&(states.len() as u32).to_le_bytes());
    for s in states {
        s.encode(&mut out).unwrap();
    }
    assert_eq!(
        out.len(),
        len,
        "rewritten DEVICES payload must preserve length"
    );
    snapshot[start..end].copy_from_slice(&out);
}

fn rewrite_cpus_section_reverse(snapshot: &mut [u8]) {
    let index = aero_snapshot::inspect_snapshot(&mut Cursor::new(&snapshot)).unwrap();
    let cpus = index
        .sections
        .iter()
        .find(|s| s.id == SectionId::CPUS)
        .expect("CPUS section missing");

    let start = cpus.offset as usize;
    let len = cpus.len as usize;
    let end = start + len;

    let entries = {
        let mut r = Cursor::new(&snapshot[start..end]);
        let count = read_u32_le(&mut r) as usize;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let entry_len = read_u64_le(&mut r) as usize;
            let mut entry = vec![0u8; entry_len];
            r.read_exact(&mut entry).expect("read vCPU entry");
            entries.push(entry);
        }
        entries
    };

    let mut out = Vec::new();
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for entry in entries.into_iter().rev() {
        out.extend_from_slice(&(entry.len() as u64).to_le_bytes());
        out.extend_from_slice(&entry);
    }
    assert_eq!(
        out.len(),
        len,
        "rewritten CPUS payload must preserve length"
    );
    snapshot[start..end].copy_from_slice(&out);
}

struct TwoDiskSource;

impl SnapshotSource for TwoDiskSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 1,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: Some("inspect-disks".to_string()),
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

struct TwoDeviceSource;

impl SnapshotSource for TwoDeviceSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 2,
            parent_snapshot_id: Some(1),
            created_unix_ms: 0,
            label: Some("inspect-devices".to_string()),
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

struct TwoCpuSource;

impl SnapshotSource for TwoCpuSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 3,
            parent_snapshot_id: Some(2),
            created_unix_ms: 0,
            label: Some("inspect-cpus".to_string()),
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

#[test]
fn snapshot_inspect_notes_unsorted_disks_section() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("unsorted_disks.aerosnap");

    let mut source = TwoDiskSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let mut bytes = cursor.into_inner();

    rewrite_disks_section(&mut bytes, |disks| disks.disks.reverse());
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "note: DISKS entries are not sorted by disk_id; displaying sorted order",
        ));
}

#[test]
fn snapshot_inspect_warns_duplicate_disk_id_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dup_disks.aerosnap");

    let mut source = TwoDiskSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let mut bytes = cursor.into_inner();

    rewrite_disks_section(&mut bytes, |disks| {
        if disks.disks.len() >= 2 {
            disks.disks[1].disk_id = disks.disks[0].disk_id;
        }
    });
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "warning: duplicate disk_id entries (snapshot restore would reject this file)",
        ));
}

#[test]
fn snapshot_inspect_notes_unsorted_devices_section() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("unsorted_devices.aerosnap");

    let mut source = TwoDeviceSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let mut bytes = cursor.into_inner();

    rewrite_devices_section(&mut bytes, |states| states.reverse());
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "note: DEVICES entries are not sorted by (device_id, version, flags); displaying sorted order",
        ));
}

#[test]
fn snapshot_inspect_warns_duplicate_device_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dup_devices.aerosnap");

    let mut source = TwoDeviceSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let mut bytes = cursor.into_inner();

    rewrite_devices_section(&mut bytes, |states| {
        if states.len() >= 2 {
            states[1].id = states[0].id;
            states[1].version = states[0].version;
            states[1].flags = states[0].flags;
        }
    });
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "warning: duplicate device entries (snapshot restore would reject this file)",
        ));
}

#[test]
fn snapshot_inspect_notes_unsorted_cpus_section() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("unsorted_cpus.aerosnap");

    let mut source = TwoCpuSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    let mut bytes = cursor.into_inner();

    rewrite_cpus_section_reverse(&mut bytes);
    fs::write(&snap, &bytes).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "note: CPUS entries are not sorted by apic_id; displaying sorted order",
        ));
}

struct CpuEntrySummarySource;

impl SnapshotSource for CpuEntrySummarySource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 9,
            parent_snapshot_id: Some(8),
            created_unix_ms: 0,
            label: Some("inspect-cpus-entry-summary".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn cpu_states(&self) -> Vec<VcpuSnapshot> {
        let cpu0 = CpuState {
            rip: 0x1234,
            ..Default::default()
        };
        let cpu1 = CpuState {
            rip: 0x5678,
            halted: true,
            ..Default::default()
        };
        vec![
            VcpuSnapshot {
                apic_id: 0,
                cpu: cpu0,
                internal_state: vec![0xAA, 0xBB, 0xCC],
            },
            VcpuSnapshot {
                apic_id: 1,
                cpu: cpu1,
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

#[test]
fn snapshot_inspect_decodes_basic_cpus_entry_cpu_state() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("cpus_entry_summary.aerosnap");

    let mut source = CpuEntrySummarySource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    fs::write(&snap, cursor.into_inner()).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("CPUS:"))
        .stdout(predicate::str::contains("apic_id=0"))
        .stdout(predicate::str::contains("rip=0x1234"))
        .stdout(predicate::str::contains("internal_len=3"))
        .stdout(predicate::str::contains("apic_id=1"))
        .stdout(predicate::str::contains("rip=0x5678"))
        .stdout(predicate::str::contains("halted=true"));
}

struct AeroIoSnapshotDeviceSource;

impl SnapshotSource for AeroIoSnapshotDeviceSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 4,
            parent_snapshot_id: Some(3),
            created_unix_ms: 0,
            label: Some("inspect-device-inner-4cc".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        // `aero-io-snapshot` TLV header for a dummy device id (`UHRT`) with snapshot version 1.0.
        //
        // `cargo xtask snapshot inspect` should surface the inner device 4CC + version.
        let mut data = vec![0u8; 16];
        data[0..4].copy_from_slice(b"AERO");
        // io-snapshot format version 1.0
        data[4..6].copy_from_slice(&1u16.to_le_bytes());
        data[6..8].copy_from_slice(&0u16.to_le_bytes());
        // inner device id
        data[8..12].copy_from_slice(b"UHRT");
        // inner device snapshot version 1.0
        data[12..14].copy_from_slice(&1u16.to_le_bytes());
        data[14..16].copy_from_slice(&0u16.to_le_bytes());

        vec![DeviceState {
            id: DeviceId::USB,
            version: 1,
            flags: 0,
            data,
        }]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
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

#[test]
fn snapshot_inspect_shows_inner_aero_io_snapshot_device_id_when_present() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("inner_device_id.aerosnap");

    let mut source = AeroIoSnapshotDeviceSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    fs::write(&snap, cursor.into_inner()).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("inner=UHRT v1.0"));
}

struct DskcWrapperDeviceSource;

impl SnapshotSource for DskcWrapperDeviceSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 5,
            parent_snapshot_id: Some(4),
            created_unix_ms: 0,
            label: Some("inspect-dskc-wrapper".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        fn io_snapshot_header(id: &[u8; 4]) -> Vec<u8> {
            let mut out = vec![0u8; 16];
            out[0..4].copy_from_slice(b"AERO");
            // io-snapshot format v1.0
            out[4..6].copy_from_slice(&1u16.to_le_bytes());
            out[6..8].copy_from_slice(&0u16.to_le_bytes());
            out[8..12].copy_from_slice(id);
            // device snapshot version v1.0
            out[12..14].copy_from_slice(&1u16.to_le_bytes());
            out[14..16].copy_from_slice(&0u16.to_le_bytes());
            out
        }

        // Nested controller snapshot header: AHCI PCI (`AHCP`) v1.0.
        let nested = io_snapshot_header(b"AHCP");

        // DSKC payload: tag 1 contains an `Encoder::vec_bytes` list.
        //
        // Encode one controller at packed_bdf 00:02.0.
        let bus: u16 = 0;
        let device: u16 = 2;
        let function: u16 = 0;
        let bdf_u16: u16 = (bus << 8) | (device << 3) | function;
        let mut entry = Vec::new();
        entry.extend_from_slice(&bdf_u16.to_le_bytes());
        entry.extend_from_slice(&nested);

        let mut controllers_buf = Vec::new();
        controllers_buf.extend_from_slice(&1u32.to_le_bytes()); // count
        controllers_buf.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        controllers_buf.extend_from_slice(&entry);

        let mut data = io_snapshot_header(b"DSKC");
        data.extend_from_slice(&1u16.to_le_bytes()); // tag
        data.extend_from_slice(&(controllers_buf.len() as u32).to_le_bytes());
        data.extend_from_slice(&controllers_buf);

        vec![DeviceState {
            id: DeviceId::DISK_CONTROLLER,
            version: 1,
            flags: 0,
            data,
        }]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
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

#[test]
fn snapshot_inspect_decodes_nested_dskc_controller_headers() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("dskc_wrapper.aerosnap");

    let mut source = DskcWrapperDeviceSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    fs::write(&snap, cursor.into_inner()).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("controllers=[00:02.0 AHCP v1.0]"));
}

struct UsbcWrapperDeviceSource;

impl SnapshotSource for UsbcWrapperDeviceSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 6,
            parent_snapshot_id: Some(5),
            created_unix_ms: 0,
            label: Some("inspect-usbc-wrapper".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        fn io_snapshot_header(id: &[u8; 4]) -> Vec<u8> {
            let mut out = vec![0u8; 16];
            out[0..4].copy_from_slice(b"AERO");
            // io-snapshot format v1.0
            out[4..6].copy_from_slice(&1u16.to_le_bytes());
            out[6..8].copy_from_slice(&0u16.to_le_bytes());
            out[8..12].copy_from_slice(id);
            // device snapshot version v1.0
            out[12..14].copy_from_slice(&1u16.to_le_bytes());
            out[14..16].copy_from_slice(&0u16.to_le_bytes());
            out
        }

        let nested = io_snapshot_header(b"UHCP");

        let mut data = io_snapshot_header(b"USBC");
        // tag 1: u64 remainder
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&500_000u64.to_le_bytes());
        // tag 2: nested snapshot bytes
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&(nested.len() as u32).to_le_bytes());
        data.extend_from_slice(&nested);

        vec![DeviceState {
            id: DeviceId::USB,
            version: 1,
            flags: 0,
            data,
        }]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
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

#[test]
fn snapshot_inspect_decodes_aero_machine_usbc_wrapper_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("usbc_wrapper.aerosnap");

    let mut source = UsbcWrapperDeviceSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    fs::write(&snap, cursor.into_inner()).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "remainder=500000ns nested=UHCP v1.0",
        ));
}

struct PcicWrapperDeviceSource;

impl SnapshotSource for PcicWrapperDeviceSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 7,
            parent_snapshot_id: Some(6),
            created_unix_ms: 0,
            label: Some("inspect-pcic-wrapper".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        fn io_snapshot_header(id: &[u8; 4]) -> Vec<u8> {
            let mut out = vec![0u8; 16];
            out[0..4].copy_from_slice(b"AERO");
            // io-snapshot format v1.0
            out[4..6].copy_from_slice(&1u16.to_le_bytes());
            out[6..8].copy_from_slice(&0u16.to_le_bytes());
            out[8..12].copy_from_slice(id);
            // device snapshot version v1.0
            out[12..14].copy_from_slice(&1u16.to_le_bytes());
            out[14..16].copy_from_slice(&0u16.to_le_bytes());
            out
        }

        let cfg = io_snapshot_header(b"PCPT");
        let intx = io_snapshot_header(b"INTX");

        let mut data = io_snapshot_header(b"PCIC");
        // tag 1: cfg ports snapshot bytes
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&(cfg.len() as u32).to_le_bytes());
        data.extend_from_slice(&cfg);
        // tag 2: intx router snapshot bytes
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&(intx.len() as u32).to_le_bytes());
        data.extend_from_slice(&intx);

        vec![DeviceState {
            id: DeviceId::PCI,
            version: 1,
            flags: 0,
            data,
        }]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
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

#[test]
fn snapshot_inspect_decodes_legacy_pcic_wrapper_nested_headers() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("pcic_wrapper.aerosnap");

    let mut source = PcicWrapperDeviceSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    fs::write(&snap, cursor.into_inner()).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("cfg=PCPT v1.0 intx=INTX v1.0"));
}

struct CpuInternalDeviceSource;

impl SnapshotSource for CpuInternalDeviceSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 8,
            parent_snapshot_id: Some(7),
            created_unix_ms: 0,
            label: Some("inspect-cpu-internal".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        // `CpuInternalState` (v2) encoding:
        //   u8 interrupt_inhibit
        //   u32 pending_len
        //   u8[pending_len] pending_external_interrupts
        let interrupt_inhibit: u8 = 7;
        let pending = [0x20u8, 0x21u8, 0x28u8];
        let mut data = Vec::new();
        data.push(interrupt_inhibit);
        data.extend_from_slice(&(pending.len() as u32).to_le_bytes());
        data.extend_from_slice(&pending);

        vec![DeviceState {
            id: DeviceId::CPU_INTERNAL,
            version: 2,
            flags: 0,
            data,
        }]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
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

#[test]
fn snapshot_inspect_decodes_cpu_internal_device_state_header() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("cpu_internal.aerosnap");

    let mut source = CpuInternalDeviceSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    fs::write(&snap, cursor.into_inner()).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("CPU_INTERNAL(9)"))
        .stdout(predicate::str::contains(
            "interrupt_inhibit=7 pending_len=3",
        ))
        .stdout(predicate::str::contains(
            "pending_preview=[0x20, 0x21, 0x28]",
        ));
}

struct MemoryDeviceSource;

impl SnapshotSource for MemoryDeviceSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 10,
            parent_snapshot_id: Some(9),
            created_unix_ms: 0,
            label: Some("inspect-memory-device".to_string()),
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
            id: DeviceId::MEMORY,
            version: 1,
            flags: 0,
            data: vec![1],
        }]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
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

#[test]
fn snapshot_inspect_decodes_memory_device_state() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("memory_device.aerosnap");

    let mut source = MemoryDeviceSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    fs::write(&snap, cursor.into_inner()).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("MEMORY(11)"))
        .stdout(predicate::str::contains("a20_enabled=true"));
}

struct BiosDeviceSource;

impl SnapshotSource for BiosDeviceSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 11,
            parent_snapshot_id: Some(10),
            created_unix_ms: 0,
            label: Some("inspect-bios-device".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        // Minimal `firmware::bios::BiosSnapshot::encode` compatible payload.
        let mut data = Vec::new();
        let memory_size_bytes: u64 = 64 * 1024 * 1024;
        data.extend_from_slice(&memory_size_bytes.to_le_bytes());
        data.push(0x80); // boot_drive

        // `CmosRtcSnapshot` (14 bytes).
        data.extend_from_slice(&2026u16.to_le_bytes()); // year
        data.extend_from_slice(&[1, 2, 3, 4, 5]); // month/day/hour/minute/second
        data.extend_from_slice(&0u32.to_le_bytes()); // nanosecond
        data.extend_from_slice(&[1, 1, 0]); // bcd_mode/hour_24/daylight_savings

        // `BdaTimeSnapshot` (21 bytes).
        data.extend_from_slice(&1234u32.to_le_bytes()); // tick_count
        data.extend_from_slice(&0u128.to_le_bytes()); // tick_remainder
        data.push(0); // midnight_flag

        // e820_map len (0).
        data.extend_from_slice(&0u32.to_le_bytes());
        // keyboard_queue len (0).
        data.extend_from_slice(&0u32.to_le_bytes());

        // video_mode.
        data.push(0x03);

        // tty_output.
        let tty = b"hello";
        data.extend_from_slice(&(tty.len() as u32).to_le_bytes());
        data.extend_from_slice(tty);

        // rsdp_addr absent.
        data.push(0);

        vec![DeviceState {
            id: DeviceId::BIOS,
            version: 1,
            flags: 0,
            data,
        }]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
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

#[test]
fn snapshot_inspect_decodes_bios_device_state() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("bios_device.aerosnap");

    let mut source = BiosDeviceSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    fs::write(&snap, cursor.into_inner()).unwrap();

    Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("BIOS(10)"))
        .stdout(predicate::str::contains("boot_drive=0x80"))
        .stdout(predicate::str::contains("mem_size_bytes=67108864"))
        .stdout(predicate::str::contains("video_mode=0x03"))
        .stdout(predicate::str::contains("tty_len=5"));
}
