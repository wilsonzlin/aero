use emulator::devices::vga::{VbeControllerInfo, VbeModeInfo, VBE_BIOS_DATA_PADDR, VBE_LFB_BASE, VgaDevice};
use memory::{DenseMemory, GuestMemory};

#[test]
fn vbe_struct_sizes_are_exact() {
    assert_eq!(core::mem::size_of::<VbeControllerInfo>(), 512);
    assert_eq!(core::mem::size_of::<VbeModeInfo>(), 256);
}

#[test]
fn vbe_info_signature_and_mode_list_termination() {
    let mut vga = VgaDevice::new();
    let mut mem = DenseMemory::new(2 * 1024 * 1024).expect("allocate guest memory");

    vga.vbe_mut()
        .install_bios_data(&mut mem)
        .expect("install vbe bios data");

    let info = vga.controller_info();
    assert_eq!(&info.signature(), b"VESA");
    assert_eq!(info.version(), 0x0300);

    let far_ptr = info.video_mode_ptr();
    let seg = (far_ptr >> 16) as u16;
    let off = (far_ptr & 0xFFFF) as u16;
    let mode_list_paddr = (u32::from(seg) << 4).wrapping_add(u32::from(off));
    assert_eq!(mode_list_paddr, VBE_BIOS_DATA_PADDR);

    // Mode list is a u16 array terminated by 0xFFFF.
    let mut modes = Vec::new();
    for i in 0..128u64 {
        let mode = mem
            .read_u16_le(u64::from(mode_list_paddr) + i * 2)
            .expect("read mode list entry");
        modes.push(mode);
        if mode == 0xFFFF {
            break;
        }
    }

    assert!(modes.len() >= 2, "mode list should contain at least one mode + terminator");
    assert_eq!(*modes.last().unwrap(), 0xFFFF);
}

#[test]
fn vbe_mode_info_phys_base_ptr_is_correct() {
    let vga = VgaDevice::new();
    let info = vga.mode_info(0x118).expect("mode info for 1024x768");

    assert_eq!(info.phys_base_ptr(), VBE_LFB_BASE);
    assert_eq!(info.bits_per_pixel(), 32);
    assert_eq!(info.red_mask_size(), 8);
    assert_eq!(info.red_field_position(), 16);
    assert_eq!(info.green_mask_size(), 8);
    assert_eq!(info.green_field_position(), 8);
    assert_eq!(info.blue_mask_size(), 8);
    assert_eq!(info.blue_field_position(), 0);
    assert_eq!(info.reserved_mask_size(), 8);
    assert_eq!(info.reserved_field_position(), 24);
}

