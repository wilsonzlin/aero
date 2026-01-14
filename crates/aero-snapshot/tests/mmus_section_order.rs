#![cfg(not(target_arch = "wasm32"))]

use std::io::Cursor;

use aero_snapshot::{
    restore_snapshot, Compression, CpuState, DeviceState, DiskOverlayRefs, MmuState, RamMode,
    Result, SectionId, SnapshotError, SnapshotTarget, VcpuMmuSnapshot, VcpuSnapshot,
    SNAPSHOT_ENDIANNESS_LITTLE, SNAPSHOT_MAGIC, SNAPSHOT_VERSION_V1,
};

fn push_section(dst: &mut Vec<u8>, id: SectionId, version: u16, flags: u16, payload: &[u8]) {
    dst.extend_from_slice(&id.0.to_le_bytes());
    dst.extend_from_slice(&version.to_le_bytes());
    dst.extend_from_slice(&flags.to_le_bytes());
    dst.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    dst.extend_from_slice(payload);
}

fn snapshot_header() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(&SNAPSHOT_VERSION_V1.to_le_bytes());
    bytes.push(SNAPSHOT_ENDIANNESS_LITTLE);
    bytes.push(0);
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes
}

fn minimal_ram_payload() -> Vec<u8> {
    let mut ram_payload = Vec::new();
    ram_payload.extend_from_slice(&0u64.to_le_bytes()); // total_len
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // page_size
    ram_payload.push(RamMode::Full as u8);
    ram_payload.push(Compression::None as u8);
    ram_payload.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ram_payload.extend_from_slice(&4096u32.to_le_bytes()); // chunk_size
    ram_payload
}

fn cpus_payload(apic_ids: &[u32]) -> Vec<u8> {
    let mut cpus_payload = Vec::new();
    cpus_payload.extend_from_slice(&(apic_ids.len() as u32).to_le_bytes());

    for &apic_id in apic_ids {
        let vcpu = VcpuSnapshot {
            apic_id,
            cpu: CpuState::default(),
            internal_state: Vec::new(),
        };
        let mut entry = Vec::new();
        vcpu.encode_v2(&mut entry).unwrap();
        cpus_payload.extend_from_slice(&(entry.len() as u64).to_le_bytes());
        cpus_payload.extend_from_slice(&entry);
    }

    cpus_payload
}

fn mmu_payload_v2(cr3: u64) -> Vec<u8> {
    let mut payload = Vec::new();
    MmuState {
        cr3,
        ..MmuState::default()
    }
    .encode_v2(&mut payload)
    .unwrap();
    payload
}

fn mmus_payload_v2(entries: &[(u32, u64)]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&(entries.len() as u32).to_le_bytes());

    for &(apic_id, cr3) in entries {
        let mut entry = Vec::new();
        entry.extend_from_slice(&apic_id.to_le_bytes());
        MmuState {
            cr3,
            ..MmuState::default()
        }
        .encode_v2(&mut entry)
        .unwrap();
        payload.extend_from_slice(&(entry.len() as u64).to_le_bytes());
        payload.extend_from_slice(&entry);
    }

    payload
}

#[derive(Default)]
struct CaptureTarget {
    cpu_calls: Vec<Vec<VcpuSnapshot>>,
    mmu_calls: Vec<Vec<VcpuMmuSnapshot>>,
}

impl SnapshotTarget for CaptureTarget {
    fn restore_cpu_state(&mut self, _state: CpuState) {}

    fn restore_cpu_states(&mut self, states: Vec<VcpuSnapshot>) -> Result<()> {
        self.cpu_calls.push(states);
        Ok(())
    }

    fn restore_mmu_state(&mut self, _state: MmuState) {
        panic!("unexpected restore_mmu_state call; expected restore_mmu_states");
    }

    fn restore_mmu_states(&mut self, states: Vec<VcpuMmuSnapshot>) -> Result<()> {
        self.mmu_calls.push(states);
        Ok(())
    }

    fn restore_device_states(&mut self, _states: Vec<DeviceState>) {}

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        0
    }

    fn write_ram(&mut self, _offset: u64, _data: &[u8]) -> Result<()> {
        Err(SnapshotError::Corrupt("unexpected RAM write"))
    }
}

fn restore_into_capture_target(bytes: Vec<u8>) -> CaptureTarget {
    let mut target = CaptureTarget::default();
    restore_snapshot(&mut Cursor::new(bytes), &mut target).unwrap();
    target
}

#[test]
fn legacy_mmu_section_order_is_independent_of_cpus_section_order() {
    let cpus = cpus_payload(&[0, 1]);
    let mmu = mmu_payload_v2(0x1234);
    let ram = minimal_ram_payload();

    let variants = [
        // Variant A: CPUS before MMU.
        {
            let mut bytes = snapshot_header();
            push_section(&mut bytes, SectionId::CPUS, 2, 0, &cpus);
            push_section(&mut bytes, SectionId::MMU, 2, 0, &mmu);
            push_section(&mut bytes, SectionId::RAM, 1, 0, &ram);
            bytes
        },
        // Variant B: MMU before CPUS.
        {
            let mut bytes = snapshot_header();
            push_section(&mut bytes, SectionId::MMU, 2, 0, &mmu);
            push_section(&mut bytes, SectionId::CPUS, 2, 0, &cpus);
            push_section(&mut bytes, SectionId::RAM, 1, 0, &ram);
            bytes
        },
    ];

    for bytes in variants {
        let target = restore_into_capture_target(bytes);
        let mmus = target
            .mmu_calls
            .last()
            .expect("restore_mmu_states was not called");
        assert_eq!(mmus.len(), 2);

        let mut apic_ids: Vec<u32> = mmus.iter().map(|m| m.apic_id).collect();
        apic_ids.sort_unstable();
        assert_eq!(apic_ids, vec![0, 1]);

        assert!(
            mmus.iter().all(|m| m.mmu.cr3 == 0x1234),
            "legacy MMU state should be replicated across vCPUs"
        );
    }
}

#[test]
fn mmus_section_order_is_independent_of_cpus_section_order() {
    let cpus = cpus_payload(&[0, 1]);

    // Intentionally encode entries out of order to ensure restore canonicalizes ordering by apic_id.
    let mmus = mmus_payload_v2(&[(1, 0x2000), (0, 0x1000)]);
    let ram = minimal_ram_payload();

    let variants = [
        // Variant A: CPUS before MMUS.
        {
            let mut bytes = snapshot_header();
            push_section(&mut bytes, SectionId::CPUS, 2, 0, &cpus);
            push_section(&mut bytes, SectionId::MMUS, 2, 0, &mmus);
            push_section(&mut bytes, SectionId::RAM, 1, 0, &ram);
            bytes
        },
        // Variant B: MMUS before CPUS.
        {
            let mut bytes = snapshot_header();
            push_section(&mut bytes, SectionId::MMUS, 2, 0, &mmus);
            push_section(&mut bytes, SectionId::CPUS, 2, 0, &cpus);
            push_section(&mut bytes, SectionId::RAM, 1, 0, &ram);
            bytes
        },
    ];

    for bytes in variants {
        let target = restore_into_capture_target(bytes);
        let mmus = target
            .mmu_calls
            .last()
            .expect("restore_mmu_states was not called");

        assert_eq!(
            mmus.iter().map(|m| m.apic_id).collect::<Vec<_>>(),
            vec![0, 1],
            "MMUS entries should be passed to the target sorted by apic_id"
        );
        assert_eq!(mmus[0].mmu.cr3, 0x1000);
        assert_eq!(mmus[1].mmu.cr3, 0x2000);
    }
}
