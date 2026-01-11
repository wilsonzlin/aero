use std::io::Cursor;

use aero_snapshot::{
    save_snapshot, Compression, CpuMode, CpuState, DeviceId, DeviceState, DiskOverlayRef,
    DiskOverlayRefs, MmuState, RamMode, RamWriteOptions, SaveOptions, SegmentState, SnapshotMeta,
    SnapshotSource,
};
use rand::{rngs::StdRng, seq::SliceRandom, SeedableRng};

#[derive(Clone)]
struct RandomOrderSource {
    meta: SnapshotMeta,
    cpu: CpuState,
    mmu: MmuState,
    devices: Vec<DeviceState>,
    disks: Vec<DiskOverlayRef>,
    ram: Vec<u8>,
    dirty_pages: Option<Vec<u64>>,
}

impl SnapshotSource for RandomOrderSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        self.meta.clone()
    }

    fn cpu_state(&self) -> CpuState {
        self.cpu.clone()
    }

    fn mmu_state(&self) -> MmuState {
        self.mmu.clone()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        self.devices.clone()
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs {
            disks: self.disks.clone(),
        }
    }

    fn ram_len(&self) -> usize {
        self.ram.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        buf.copy_from_slice(&self.ram[offset..offset + buf.len()]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        self.dirty_pages.take()
    }
}

fn make_source(seed: u64) -> RandomOrderSource {
    let mut rng = StdRng::seed_from_u64(seed);

    let meta = SnapshotMeta {
        snapshot_id: 123,
        parent_snapshot_id: Some(122),
        created_unix_ms: 456,
        label: Some("determinism-test".to_string()),
    };

    let cpu = CpuState {
        rax: 1,
        rbx: 2,
        rcx: 3,
        rdx: 4,
        rsi: 5,
        rdi: 6,
        rbp: 7,
        rsp: 8,
        r8: 9,
        r9: 10,
        r10: 11,
        r11: 12,
        r12: 13,
        r13: 14,
        r14: 15,
        r15: 16,
        rip: 17,
        rflags: 18,
        mode: CpuMode::Real,
        halted: false,
        cs: SegmentState::real_mode(19),
        ds: SegmentState::real_mode(20),
        es: SegmentState::real_mode(21),
        fs: SegmentState::real_mode(22),
        gs: SegmentState::real_mode(23),
        ss: SegmentState::real_mode(24),
        ..CpuState::default()
    };

    let mmu = MmuState {
        cr0: 0x8000_0011,
        cr2: 0x1234,
        cr3: 0x5678,
        cr4: 0x2000,
        cr8: 0,
        efer: 0x500,
        gdtr_base: 0x1000,
        gdtr_limit: 0x30,
        idtr_base: 0x2000,
        idtr_limit: 0x40,
        ..MmuState::default()
    };

    let mut devices = vec![
        DeviceState {
            id: DeviceId::PCI,
            version: 2,
            flags: 0,
            data: vec![2],
        },
        DeviceState {
            id: DeviceId::VGA,
            version: 1,
            flags: 7,
            data: vec![7],
        },
        DeviceState {
            id: DeviceId::PCI,
            version: 1,
            flags: 0,
            data: vec![1],
        },
        DeviceState {
            id: DeviceId::PIT,
            version: 1,
            flags: 0,
            data: vec![3],
        },
    ];
    devices.shuffle(&mut rng);

    let mut disks = vec![
        DiskOverlayRef {
            disk_id: 2,
            base_image: "base2.img".to_string(),
            overlay_image: "overlay2.img".to_string(),
        },
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
    ];
    disks.shuffle(&mut rng);

    // Ensure max_pages uses ceil(total_len / page_size) by including the final partial page.
    let ram_len = 4 * 4096 + 1;
    let mut ram = vec![0u8; ram_len];
    for (idx, b) in ram.iter_mut().enumerate() {
        *b = idx as u8;
    }

    let mut dirty_pages = vec![4u64, 2, 0, 4, 1, 2];
    dirty_pages.shuffle(&mut rng);

    RandomOrderSource {
        meta,
        cpu,
        mmu,
        devices,
        disks,
        ram,
        dirty_pages: Some(dirty_pages),
    }
}

#[test]
fn save_snapshot_is_deterministic_across_input_orders() {
    let options = SaveOptions {
        ram: RamWriteOptions {
            mode: RamMode::Dirty,
            compression: Compression::None,
            ..RamWriteOptions::default()
        },
    };

    let mut canonical: Option<Vec<u8>> = None;

    for seed in 0..16u64 {
        let mut source = make_source(seed);
        let mut cursor = Cursor::new(Vec::new());
        save_snapshot(&mut cursor, &mut source, options).unwrap();

        let bytes = cursor.into_inner();
        match &canonical {
            Some(expected) => assert_eq!(&bytes, expected),
            None => canonical = Some(bytes),
        }
    }
}
