use std::io::Cursor;

use aero_devices::pci::profile::{AEROGPU_BAR0_INDEX, AEROGPU_BAR1_VRAM_INDEX};
use aero_machine::{Machine, MachineConfig, ScanoutSource};
use aero_protocol::aerogpu::aerogpu_pci as pci;
use aero_snapshot as snapshot;

fn read_mmio_u64(m: &mut Machine, base: u64, lo: u32, hi: u32) -> u64 {
    let lo = m.read_physical_u32(base + u64::from(lo));
    let hi = m.read_physical_u32(base + u64::from(hi));
    u64::from(lo) | (u64::from(hi) << 32)
}

fn write_mmio_u64(m: &mut Machine, base: u64, lo: u32, hi: u32, value: u64) {
    m.write_physical_u32(base + u64::from(lo), value as u32);
    m.write_physical_u32(base + u64::from(hi), (value >> 32) as u32);
}

fn encode_aerogpu_snapshot_v1_from_machine(
    m: &mut Machine,
    bar0_base: u64,
    bar1_base: u64,
    vram_len: usize,
) -> Vec<u8> {
    let abi_version = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_ABI_VERSION));
    let features = {
        let lo =
            m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FEATURES_LO)) as u64;
        let hi =
            m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_FEATURES_HI)) as u64;
        (hi << 32) | lo
    };

    let ring_gpa = read_mmio_u64(
        m,
        bar0_base,
        pci::AEROGPU_MMIO_REG_RING_GPA_LO,
        pci::AEROGPU_MMIO_REG_RING_GPA_HI,
    );
    let ring_size_bytes =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES));
    let ring_control =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL));

    let fence_gpa = read_mmio_u64(
        m,
        bar0_base,
        pci::AEROGPU_MMIO_REG_FENCE_GPA_LO,
        pci::AEROGPU_MMIO_REG_FENCE_GPA_HI,
    );
    let completed_fence = read_mmio_u64(
        m,
        bar0_base,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO,
        pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI,
    );

    let irq_status = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_STATUS));
    let irq_enable = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE));

    let scanout0_enable =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE));
    let scanout0_width =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH));
    let scanout0_height =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT));
    let scanout0_format =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT));
    let scanout0_pitch_bytes =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES));
    let scanout0_fb_gpa = read_mmio_u64(
        m,
        bar0_base,
        pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO,
        pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI,
    );

    let scanout0_vblank_seq = read_mmio_u64(
        m,
        bar0_base,
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO,
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI,
    );
    let scanout0_vblank_time_ns = read_mmio_u64(
        m,
        bar0_base,
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO,
        pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI,
    );
    let scanout0_vblank_period_ns =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS));

    let cursor_enable =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_ENABLE));
    let cursor_x = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_X));
    let cursor_y = m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_Y));
    let cursor_hot_x =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HOT_X));
    let cursor_hot_y =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y));
    let cursor_width =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_WIDTH));
    let cursor_height =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT));
    let cursor_format =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_FORMAT));
    let cursor_fb_gpa = read_mmio_u64(
        m,
        bar0_base,
        pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO,
        pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI,
    );
    let cursor_pitch_bytes =
        m.read_physical_u32(bar0_base + u64::from(pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES));

    // Host-only latch.
    let wddm_scanout_active = m
        .aerogpu_mmio()
        .expect("missing AeroGPU MMIO device")
        .borrow()
        .scanout0_state()
        .wddm_scanout_active;

    let vram_bytes = m.read_physical_bytes(bar1_base, vram_len);

    let mut out = Vec::with_capacity(256 + vram_bytes.len());
    out.extend_from_slice(&abi_version.to_le_bytes());
    out.extend_from_slice(&features.to_le_bytes());

    out.extend_from_slice(&ring_gpa.to_le_bytes());
    out.extend_from_slice(&ring_size_bytes.to_le_bytes());
    out.extend_from_slice(&ring_control.to_le_bytes());

    out.extend_from_slice(&fence_gpa.to_le_bytes());
    out.extend_from_slice(&completed_fence.to_le_bytes());

    out.extend_from_slice(&irq_status.to_le_bytes());
    out.extend_from_slice(&irq_enable.to_le_bytes());

    out.extend_from_slice(&scanout0_enable.to_le_bytes());
    out.extend_from_slice(&scanout0_width.to_le_bytes());
    out.extend_from_slice(&scanout0_height.to_le_bytes());
    out.extend_from_slice(&scanout0_format.to_le_bytes());
    out.extend_from_slice(&scanout0_pitch_bytes.to_le_bytes());
    out.extend_from_slice(&scanout0_fb_gpa.to_le_bytes());

    out.extend_from_slice(&scanout0_vblank_seq.to_le_bytes());
    out.extend_from_slice(&scanout0_vblank_time_ns.to_le_bytes());
    out.extend_from_slice(&scanout0_vblank_period_ns.to_le_bytes());

    out.extend_from_slice(&cursor_enable.to_le_bytes());
    out.extend_from_slice(&cursor_x.to_le_bytes());
    out.extend_from_slice(&cursor_y.to_le_bytes());
    out.extend_from_slice(&cursor_hot_x.to_le_bytes());
    out.extend_from_slice(&cursor_hot_y.to_le_bytes());
    out.extend_from_slice(&cursor_width.to_le_bytes());
    out.extend_from_slice(&cursor_height.to_le_bytes());
    out.extend_from_slice(&cursor_format.to_le_bytes());
    out.extend_from_slice(&cursor_fb_gpa.to_le_bytes());
    out.extend_from_slice(&cursor_pitch_bytes.to_le_bytes());

    out.push(wddm_scanout_active as u8);

    let vram_len_u32: u32 = vram_len.try_into().expect("vram_len fits u32");
    out.extend_from_slice(&vram_len_u32.to_le_bytes());
    out.extend_from_slice(&vram_bytes);
    out
}

struct AerogpuV1SnapshotSource<'a> {
    machine: &'a mut Machine,
    aerogpu_v1_data: Vec<u8>,
}

impl snapshot::SnapshotSource for AerogpuV1SnapshotSource<'_> {
    fn snapshot_meta(&mut self) -> snapshot::SnapshotMeta {
        snapshot::SnapshotSource::snapshot_meta(&mut *self.machine)
    }

    fn cpu_state(&self) -> snapshot::CpuState {
        snapshot::SnapshotSource::cpu_state(&*self.machine)
    }

    fn cpu_states(&self) -> Vec<snapshot::VcpuSnapshot> {
        snapshot::SnapshotSource::cpu_states(&*self.machine)
    }

    fn mmu_state(&self) -> snapshot::MmuState {
        snapshot::SnapshotSource::mmu_state(&*self.machine)
    }

    fn mmu_states(&self) -> Vec<snapshot::VcpuMmuSnapshot> {
        snapshot::SnapshotSource::mmu_states(&*self.machine)
    }

    fn device_states(&self) -> Vec<snapshot::DeviceState> {
        let mut devices = snapshot::SnapshotSource::device_states(&*self.machine);
        for state in &mut devices {
            if state.id == snapshot::DeviceId::AEROGPU {
                state.version = 1;
                state.data = self.aerogpu_v1_data.clone();
            }
        }
        devices
    }

    fn disk_overlays(&self) -> snapshot::DiskOverlayRefs {
        snapshot::SnapshotSource::disk_overlays(&*self.machine)
    }

    fn ram_len(&self) -> usize {
        snapshot::SnapshotSource::ram_len(&*self.machine)
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> snapshot::Result<()> {
        snapshot::SnapshotSource::read_ram(&*self.machine, offset, buf)
    }

    fn dirty_page_size(&self) -> u32 {
        snapshot::SnapshotSource::dirty_page_size(&*self.machine)
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        snapshot::SnapshotSource::take_dirty_pages(&mut *self.machine)
    }
}

#[test]
fn aerogpu_snapshot_v1_is_still_restorable_for_backward_compat() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the machine minimal/deterministic for this snapshot test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    };

    let mut vm = Machine::new(cfg.clone()).unwrap();
    let bdf = vm.aerogpu_bdf().expect("AeroGPU device should be present");
    let bar0 = vm
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be mapped");
    let bar1 = vm
        .pci_bar_base(bdf, AEROGPU_BAR1_VRAM_INDEX)
        .expect("AeroGPU BAR1 should be mapped");

    // Ensure PCI MMIO decode + bus mastering so BAR0 accesses route to the device and WDDM scanout
    // readback (treated as DMA) is permitted.
    {
        let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("AeroGPU PCI function missing");
        cfg.set_command(cfg.command() | (1 << 1) | (1 << 2));
    }

    // VRAM pattern in the snapshotted prefix.
    let vram_len = 4096usize;
    let vram_off = 0x200u64;
    let vram_pattern = [0x11u8, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88];
    vm.write_physical(bar1 + vram_off, &vram_pattern);

    // Program a simple, valid WDDM scanout0 config to ensure the host-only `wddm_scanout_active`
    // latch is exercised by the v1 snapshot decoder.
    let fb_gpa = 0x0010_0000u64;
    vm.write_physical(fb_gpa, &[0x00, 0x00, 0xFF, 0x00]); // BGRX red
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH), 1);
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT), 1);
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT),
        pci::AerogpuFormat::B8G8R8X8Unorm as u32,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES),
        4,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO),
        fb_gpa as u32,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI),
        (fb_gpa >> 32) as u32,
    );
    vm.write_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE), 1);
    assert_eq!(vm.active_scanout_source(), ScanoutSource::Wddm);

    // Also program some other BAR0 regs to ensure they survive v1 restore.
    let ring_gpa = 0x0001_8000u64;
    write_mmio_u64(
        &mut vm,
        bar0,
        pci::AEROGPU_MMIO_REG_RING_GPA_LO,
        pci::AEROGPU_MMIO_REG_RING_GPA_HI,
        ring_gpa,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES),
        0x2000,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL),
        pci::AEROGPU_RING_CONTROL_ENABLE,
    );
    vm.write_physical_u32(
        bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE),
        pci::AEROGPU_IRQ_FENCE | pci::AEROGPU_IRQ_SCANOUT_VBLANK,
    );

    // Read back the canonical values and embed them in a v1-format AeroGPU device entry.
    let ring_gpa_before = read_mmio_u64(
        &mut vm,
        bar0,
        pci::AEROGPU_MMIO_REG_RING_GPA_LO,
        pci::AEROGPU_MMIO_REG_RING_GPA_HI,
    );
    let ring_size_before =
        vm.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES));
    let ring_control_before =
        vm.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL));
    let irq_enable_before =
        vm.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE));

    let aerogpu_v1_data = encode_aerogpu_snapshot_v1_from_machine(&mut vm, bar0, bar1, vram_len);
    let mut cursor = Cursor::new(Vec::new());
    let mut source = AerogpuV1SnapshotSource {
        machine: &mut vm,
        aerogpu_v1_data,
    };
    snapshot::save_snapshot(&mut cursor, &mut source, snapshot::SaveOptions::default()).unwrap();
    let snap = cursor.into_inner();

    let mut restored = Machine::new(cfg).unwrap();
    restored.reset();
    restored.restore_snapshot_bytes(&snap).unwrap();

    assert_eq!(restored.active_scanout_source(), ScanoutSource::Wddm);

    let bdf = restored
        .aerogpu_bdf()
        .expect("AeroGPU device should be present");
    let bar0 = restored
        .pci_bar_base(bdf, AEROGPU_BAR0_INDEX)
        .expect("AeroGPU BAR0 should be mapped");
    let bar1 = restored
        .pci_bar_base(bdf, AEROGPU_BAR1_VRAM_INDEX)
        .expect("AeroGPU BAR1 should be mapped");

    let ring_gpa_after = read_mmio_u64(
        &mut restored,
        bar0,
        pci::AEROGPU_MMIO_REG_RING_GPA_LO,
        pci::AEROGPU_MMIO_REG_RING_GPA_HI,
    );
    assert_eq!(ring_gpa_after, ring_gpa_before);
    assert_eq!(
        restored.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES)),
        ring_size_before
    );
    assert_eq!(
        restored.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_RING_CONTROL)),
        ring_control_before
    );
    assert_eq!(
        restored.read_physical_u32(bar0 + u64::from(pci::AEROGPU_MMIO_REG_IRQ_ENABLE)),
        irq_enable_before
    );

    let restored_vram = restored.read_physical_bytes(bar1 + vram_off, vram_pattern.len());
    assert_eq!(restored_vram, vram_pattern);
}
