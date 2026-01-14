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
    let aerogpu_bdf = vm.aerogpu_bdf().expect("expected AeroGPU device present");

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

    // We want to validate that VRAM snapshots are sparse without doing a huge BAR1 MMIO clear (BAR1
    // writes are byte/word MMIO and are intentionally slow). Instead:
    // 1) Take an initial snapshot and record which VRAM pages are already non-zero (e.g. BIOS text
    //    buffer init).
    // 2) Dirty a page that was previously zero.
    // 3) Take another snapshot and ensure:
    //    - the page count increases by exactly 1
    //    - the new page's payload contains our bytes
    //    - the snapshot payload stays small (i.e. doesn't dump the full 16MiB VRAM prefix)

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

    fn parse_v2_page_list(
        data: &[u8],
        target_idx: Option<usize>,
    ) -> (u32, u32, u32, Vec<bool>, Option<&[u8]>) {
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

        let vram_len = read_u32(data, &mut off);
        let page_size = read_u32(data, &mut off);
        let page_count = read_u32(data, &mut off);

        assert_ne!(vram_len, 0);
        assert!(
            (vram_len as usize) <= aero_gpu_vga::DEFAULT_VRAM_SIZE,
            "unexpected VRAM snapshot length: {vram_len}"
        );
        assert_eq!(page_size, 4096, "unexpected VRAM snapshot page size");

        let total_pages = (vram_len as usize).div_ceil(page_size as usize);
        let mut present = vec![false; total_pages];
        let mut target_payload = None;

        for _ in 0..page_count {
            let idx = read_u32(data, &mut off) as usize;
            let len = read_u32(data, &mut off) as usize;
            assert!(idx < total_pages, "page index out of range");
            assert!(len > 0 && len <= page_size as usize);
            let end = off + len;
            let payload = data.get(off..end).unwrap();
            if Some(idx) == target_idx {
                target_payload = Some(payload);
            }
            present[idx] = true;
            off = end;
        }

        (vram_len, page_size, page_count, present, target_payload)
    }

    let snap0 = vm.take_snapshot_full().unwrap();
    let (version0, data0) = extract_aerogpu_device_entry(&snap0);
    assert_eq!(version0, 2, "expected AeroGPU snapshot version v2");

    let (_vram_len0, page_size0, page_count0, present0, _payload0) =
        parse_v2_page_list(&data0, None);

    // Pick a page that wasn't present in the snapshot page list (i.e. all-zero).
    let target_page_idx = present0
        .iter()
        .position(|&present| !present)
        .expect("expected at least one zero VRAM page in the snapshotted prefix");

    // Dirty the chosen page with a recognizable byte pattern.
    let dirty_pattern = [0xA5u8; 16];
    let dirty_gpa = bar1_base + (target_page_idx * page_size0 as usize) as u64;
    vm.write_physical(dirty_gpa, &dirty_pattern);

    let snap1 = vm.take_snapshot_full().unwrap();
    let (version1, data1) = extract_aerogpu_device_entry(&snap1);
    assert_eq!(version1, 2, "expected AeroGPU snapshot version v2");

    // Ensure the payload is still substantially smaller than a full 16MiB VRAM dump.
    assert!(
        data1.len() < aero_gpu_vga::DEFAULT_VRAM_SIZE / 2,
        "expected sparse AeroGPU snapshot payload; got {} bytes",
        data1.len()
    );

    let (_vram_len1, _page_size1, page_count1, _present1, payload) =
        parse_v2_page_list(&data1, Some(target_page_idx));
    assert_eq!(page_count1, page_count0 + 1);
    let payload = payload.expect("expected dirty page to appear in snapshot page list");
    assert!(
        payload.len() >= dirty_pattern.len(),
        "page payload too short"
    );
    assert_eq!(&payload[..dirty_pattern.len()], dirty_pattern);
}
