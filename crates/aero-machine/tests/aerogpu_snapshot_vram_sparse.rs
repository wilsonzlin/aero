use std::io::{Cursor, Read};

use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot::{inspect_snapshot, DeviceId, SectionId};

fn read_u16_le(r: &mut Cursor<&[u8]>) -> u16 {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf).unwrap();
    u16::from_le_bytes(buf)
}

fn read_u32_le(r: &mut Cursor<&[u8]>) -> u32 {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).unwrap();
    u32::from_le_bytes(buf)
}

fn read_u64_le(r: &mut Cursor<&[u8]>) -> u64 {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf).unwrap();
    u64::from_le_bytes(buf)
}

fn read_u8(bytes: &[u8], off: &mut usize) -> u8 {
    let b = *bytes.get(*off).unwrap();
    *off += 1;
    b
}

fn read_u32(bytes: &[u8], off: &mut usize) -> u32 {
    let end = off.checked_add(4).unwrap();
    let slice = bytes.get(*off..end).unwrap();
    *off = end;
    u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]])
}

fn read_u64(bytes: &[u8], off: &mut usize) -> u64 {
    let end = off.checked_add(8).unwrap();
    let slice = bytes.get(*off..end).unwrap();
    *off = end;
    u64::from_le_bytes([
        slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
    ])
}

#[test]
fn aerogpu_snapshot_vram_sparse_page_list_tracks_only_non_zero_pages() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: false,
        enable_aerogpu: true,
        // Keep the machine minimal.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut vm = Machine::new(cfg).unwrap();

    // Force VRAM into a deterministic state: clear the snapshotted VRAM prefix then dirty a single
    // page. This avoids depending on whatever the BIOS or device init may have drawn.
    let aerogpu_bdf = vm.aerogpu().expect("expected AeroGPU device present");

    // Ensure BAR1 MMIO decode is enabled.
    {
        let pci_cfg = vm.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let command = bus.read_config(aerogpu_bdf, 0x04, 2) as u16;
        // COMMAND.MEM (bit1) + COMMAND.BME (bit2).
        bus.write_config(
            aerogpu_bdf,
            0x04,
            2,
            u32::from(command | (1 << 1) | (1 << 2)),
        );
    }

    let bar1_base = vm
        .pci_bar_base(aerogpu_bdf, profile::AEROGPU_BAR1_VRAM_INDEX)
        .expect("expected AeroGPU BAR1 base");
    assert_ne!(bar1_base, 0);

    // Clear the legacy VRAM snapshot prefix (V2 encoding snapshots at most `DEFAULT_VRAM_SIZE`).
    let zero_chunk = vec![0u8; 64 * 1024];
    let vram_len = aero_gpu_vga::DEFAULT_VRAM_SIZE;
    for offset in (0..vram_len).step_by(zero_chunk.len()) {
        let len = (vram_len - offset).min(zero_chunk.len());
        vm.write_physical(bar1_base + offset as u64, &zero_chunk[..len]);
    }

    // Dirty a single page at a stable offset within VRAM.
    let dirty_offset = 0x1234usize;
    let dirty_pattern = [0xA5u8; 16];
    vm.write_physical(bar1_base + dirty_offset as u64, &dirty_pattern);

    let snap = vm.take_snapshot_full().unwrap();

    // Find the DEVICES section so we can inspect the AeroGPU device state payload size.
    let devices_section = {
        let mut cursor = Cursor::new(&snap);
        let index = inspect_snapshot(&mut cursor).unwrap();
        index
            .sections
            .iter()
            .find(|s| s.id == SectionId::DEVICES)
            .expect("snapshot should contain DEVICES section")
            .to_owned()
    };

    let start = devices_section.offset as usize;
    let end = start + devices_section.len as usize;
    let mut r = Cursor::new(&snap[start..end]);
    let count = read_u32_le(&mut r) as usize;

    let mut aerogpu_entry = None;
    for _ in 0..count {
        let id = DeviceId(read_u32_le(&mut r));
        let version = read_u16_le(&mut r);
        let _flags = read_u16_le(&mut r);
        let len = read_u64_le(&mut r) as usize;
        let mut data = vec![0u8; len];
        r.read_exact(&mut data).unwrap();
        if id == DeviceId::AEROGPU {
            aerogpu_entry = Some((version, data));
        }
    }

    let (version, data) = aerogpu_entry.expect("snapshot should contain an AeroGPU device entry");
    assert_eq!(version, 2, "expected AeroGPU snapshot version v2");

    // V2 snapshots use a sparse page list for VRAM. Even with a 16MiB VRAM prefix, the payload
    // should remain small when only one page is non-zero (no full VRAM dump).
    assert!(
        data.len() < 64 * 1024,
        "expected sparse AeroGPU snapshot payload; got {} bytes",
        data.len()
    );

    // Decode the VRAM sparse header and ensure the page list contains only our dirty page.
    let mut off = 0usize;
    // BAR0 regs (same ordering as `encode_aerogpu_snapshot_v2`).
    let _abi_version = read_u32(&data, &mut off);
    let _features = read_u64(&data, &mut off);
    let _ring_gpa = read_u64(&data, &mut off);
    let _ring_size_bytes = read_u32(&data, &mut off);
    let _ring_control = read_u32(&data, &mut off);
    let _fence_gpa = read_u64(&data, &mut off);
    let _completed_fence = read_u64(&data, &mut off);
    let _irq_status = read_u32(&data, &mut off);
    let _irq_enable = read_u32(&data, &mut off);
    let _scanout0_enable = read_u32(&data, &mut off);
    let _scanout0_width = read_u32(&data, &mut off);
    let _scanout0_height = read_u32(&data, &mut off);
    let _scanout0_format = read_u32(&data, &mut off);
    let _scanout0_pitch_bytes = read_u32(&data, &mut off);
    let _scanout0_fb_gpa = read_u64(&data, &mut off);
    let _scanout0_vblank_seq = read_u64(&data, &mut off);
    let _scanout0_vblank_time_ns = read_u64(&data, &mut off);
    let _scanout0_vblank_period_ns = read_u32(&data, &mut off);
    let _cursor_enable = read_u32(&data, &mut off);
    let _cursor_x = read_u32(&data, &mut off);
    let _cursor_y = read_u32(&data, &mut off);
    let _cursor_hot_x = read_u32(&data, &mut off);
    let _cursor_hot_y = read_u32(&data, &mut off);
    let _cursor_width = read_u32(&data, &mut off);
    let _cursor_height = read_u32(&data, &mut off);
    let _cursor_format = read_u32(&data, &mut off);
    let _cursor_fb_gpa = read_u64(&data, &mut off);
    let _cursor_pitch_bytes = read_u32(&data, &mut off);
    let _wddm_scanout_active = read_u8(&data, &mut off) != 0;

    let vram_len = read_u32(&data, &mut off);
    let page_size = read_u32(&data, &mut off);
    let page_count = read_u32(&data, &mut off);

    assert_ne!(vram_len, 0);
    assert!(
        (vram_len as usize) <= aero_gpu_vga::DEFAULT_VRAM_SIZE,
        "unexpected VRAM snapshot length: {vram_len}"
    );
    assert_eq!(page_size, 4096, "unexpected VRAM snapshot page size");
    assert_eq!(page_count, 1, "expected exactly one non-zero VRAM page");

    // Ensure the one page entry corresponds to the bytes we dirtied.
    let idx = read_u32(&data, &mut off) as usize;
    let len = read_u32(&data, &mut off) as usize;
    assert_eq!(len, 4096);
    assert_eq!(idx, dirty_offset / 4096);
    let page_bytes = data.get(off..off + len).unwrap();
    let in_page_off = dirty_offset % 4096;
    assert_eq!(
        &page_bytes[in_page_off..in_page_off + dirty_pattern.len()],
        dirty_pattern
    );
}
