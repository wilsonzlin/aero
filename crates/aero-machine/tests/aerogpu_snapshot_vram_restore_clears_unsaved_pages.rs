use std::io::{Cursor, Read};

use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot::{inspect_snapshot, DeviceId, SectionId};
use pretty_assertions::assert_eq;

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

fn extract_aerogpu_device_entry(snapshot: &[u8]) -> (u16, Vec<u8>) {
    let devices_section = {
        let mut cursor = Cursor::new(snapshot);
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
    let mut r = Cursor::new(&snapshot[start..end]);
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

    aerogpu_entry.expect("snapshot should contain an AeroGPU device entry")
}

fn parse_v2_page_presence(data: &[u8]) -> (usize, usize, Vec<bool>) {
    let mut off = 0usize;

    // Skip BAR0 regs (same ordering as `encode_aerogpu_snapshot_v2`).
    let _abi_version = read_u32(data, &mut off);
    let _features = read_u64(data, &mut off);
    let _ring_gpa = read_u64(data, &mut off);
    let _ring_size_bytes = read_u32(data, &mut off);
    let _ring_control = read_u32(data, &mut off);
    let _fence_gpa = read_u64(data, &mut off);
    let _completed_fence = read_u64(data, &mut off);
    let _irq_status = read_u32(data, &mut off);
    let _irq_enable = read_u32(data, &mut off);
    let _scanout0_enable = read_u32(data, &mut off);
    let _scanout0_width = read_u32(data, &mut off);
    let _scanout0_height = read_u32(data, &mut off);
    let _scanout0_format = read_u32(data, &mut off);
    let _scanout0_pitch_bytes = read_u32(data, &mut off);
    let _scanout0_fb_gpa = read_u64(data, &mut off);
    let _scanout0_vblank_seq = read_u64(data, &mut off);
    let _scanout0_vblank_time_ns = read_u64(data, &mut off);
    let _scanout0_vblank_period_ns = read_u32(data, &mut off);
    let _cursor_enable = read_u32(data, &mut off);
    let _cursor_x = read_u32(data, &mut off);
    let _cursor_y = read_u32(data, &mut off);
    let _cursor_hot_x = read_u32(data, &mut off);
    let _cursor_hot_y = read_u32(data, &mut off);
    let _cursor_width = read_u32(data, &mut off);
    let _cursor_height = read_u32(data, &mut off);
    let _cursor_format = read_u32(data, &mut off);
    let _cursor_fb_gpa = read_u64(data, &mut off);
    let _cursor_pitch_bytes = read_u32(data, &mut off);
    let _wddm_scanout_active = read_u8(data, &mut off) != 0;

    let vram_len = read_u32(data, &mut off) as usize;
    let page_size = read_u32(data, &mut off) as usize;
    assert_ne!(vram_len, 0);
    assert_eq!(page_size, 4096);
    let page_count = read_u32(data, &mut off) as usize;

    let total_pages = vram_len.div_ceil(page_size);
    let mut present = vec![false; total_pages];

    for _ in 0..page_count {
        let idx = read_u32(data, &mut off) as usize;
        let len = read_u32(data, &mut off) as usize;
        assert!(idx < total_pages, "page index out of range");
        assert!(len > 0 && len <= page_size);
        present[idx] = true;
        off += len;
    }

    (vram_len, page_size, present)
}

#[test]
fn aerogpu_snapshot_restore_clears_unsaved_vram_pages() {
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

    let mut src = Machine::new(cfg.clone()).unwrap();
    let aerogpu_bdf = src
        .aerogpu_bdf()
        .expect("expected AeroGPU device present");
    {
        let pci_cfg = src.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let command = bus.read_config(aerogpu_bdf, 0x04, 2) as u16;
        bus.write_config(
            aerogpu_bdf,
            0x04,
            2,
            u32::from(command | (1 << 1) | (1 << 2)),
        );
    }
    let src_bar1 = src
        .pci_bar_base(aerogpu_bdf, profile::AEROGPU_BAR1_VRAM_INDEX)
        .expect("expected AeroGPU BAR1 base");
    assert_ne!(src_bar1, 0);

    let snap = src.take_snapshot_full().unwrap();
    let (version, data) = extract_aerogpu_device_entry(&snap);
    assert_eq!(version, 2, "expected AeroGPU snapshot version v2");

    let (_vram_len, page_size, present) = parse_v2_page_presence(&data);
    let unsaved_page_idx = present
        .iter()
        .position(|&present| !present)
        .expect("expected at least one unsaved (all-zero) VRAM page in sparse snapshot");

    // Sanity: the chosen page should read back as all-zero in the source machine.
    let src_page =
        src.read_physical_bytes(src_bar1 + (unsaved_page_idx * page_size) as u64, page_size);
    assert!(
        src_page.iter().all(|&b| b == 0),
        "expected chosen unsaved page to be all-zero in the source machine"
    );

    // Restore into a new machine, but first poison the unsaved page with non-zero bytes. Restore
    // should reset VRAM to a deterministic baseline before applying the sparse page list, clearing
    // this stale data.
    let mut dst = Machine::new(cfg).unwrap();
    dst.reset();
    let aerogpu_bdf = dst
        .aerogpu_bdf()
        .expect("expected AeroGPU device present");
    {
        let pci_cfg = dst.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let command = bus.read_config(aerogpu_bdf, 0x04, 2) as u16;
        bus.write_config(
            aerogpu_bdf,
            0x04,
            2,
            u32::from(command | (1 << 1) | (1 << 2)),
        );
    }
    let dst_bar1 = dst
        .pci_bar_base(aerogpu_bdf, profile::AEROGPU_BAR1_VRAM_INDEX)
        .expect("expected AeroGPU BAR1 base");
    assert_ne!(dst_bar1, 0);

    let poison = [0xCCu8; 16];
    let poison_gpa = dst_bar1 + (unsaved_page_idx * page_size) as u64;
    dst.write_physical(poison_gpa, &poison);
    assert_eq!(dst.read_physical_bytes(poison_gpa, poison.len()), poison);

    dst.restore_snapshot_bytes(&snap).unwrap();

    let after = dst.read_physical_bytes(poison_gpa, poison.len());
    assert_eq!(after, vec![0u8; poison.len()]);
}
