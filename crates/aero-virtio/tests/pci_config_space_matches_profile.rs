use std::collections::BTreeMap;

use aero_devices::pci::capabilities::PCI_CAP_ID_VENDOR_SPECIFIC;
use aero_devices::pci::msix::PCI_CAP_ID_MSIX;
use aero_devices::pci::profile::{
    PciDeviceProfile, VIRTIO_BLK, VIRTIO_INPUT_KEYBOARD, VIRTIO_INPUT_MOUSE, VIRTIO_INPUT_TABLET,
    VIRTIO_NET,
};
use aero_devices::pci::{PciBarDefinition, PciConfigSpace, PciDevice as _};

use aero_virtio::devices::blk::{MemDisk, VirtioBlk};
use aero_virtio::devices::input::{VirtioInput, VirtioInputDeviceKind};
use aero_virtio::devices::net::{LoopbackNet, VirtioNet};
use aero_virtio::pci::{InterruptLog, VirtioPciDevice};

#[cfg(feature = "snd")]
use aero_devices::pci::profile::VIRTIO_SND;

#[cfg(feature = "snd")]
use aero_virtio::devices::snd::VirtioSnd;

#[derive(Debug, Clone, PartialEq, Eq)]
struct VirtioVendorSpecificCap {
    /// Config-space offset where the capability structure begins.
    cap_offset: u8,
    /// Virtio `cfg_type` (common, notify, isr, device, ...).
    cfg_type: u8,
    /// BAR index the capability points at.
    bar: u8,
    /// Offset within the BAR.
    bar_offset: u32,
    /// Length of the mapped region within the BAR.
    bar_len: u32,
    /// `notify_off_multiplier` (only present for notify capability).
    notify_off_multiplier: Option<u32>,
}

fn read_u8(cfg: &mut PciConfigSpace, offset: u16) -> u8 {
    cfg.read(offset, 1) as u8
}

fn read_u16(cfg: &mut PciConfigSpace, offset: u16) -> u16 {
    cfg.read(offset, 2) as u16
}

fn read_u32(cfg: &mut PciConfigSpace, offset: u16) -> u32 {
    cfg.read(offset, 4)
}

fn extract_virtio_vendor_caps(cfg: &mut PciConfigSpace) -> BTreeMap<u8, VirtioVendorSpecificCap> {
    let mut caps = BTreeMap::new();
    for cap in cfg.capability_list() {
        if cap.id != PCI_CAP_ID_VENDOR_SPECIFIC {
            continue;
        }

        let cap_offset = cap.offset;
        let cfg_type = read_u8(cfg, u16::from(cap_offset) + 3);
        let bar = read_u8(cfg, u16::from(cap_offset) + 4);
        let bar_offset = read_u32(cfg, u16::from(cap_offset) + 8);
        let bar_len = read_u32(cfg, u16::from(cap_offset) + 12);

        let cap_len = read_u8(cfg, u16::from(cap_offset) + 2);
        let notify_off_multiplier = if cfg_type == 2 && cap_len >= 20 {
            Some(read_u32(cfg, u16::from(cap_offset) + 16))
        } else {
            None
        };

        let prev = caps.insert(
            cfg_type,
            VirtioVendorSpecificCap {
                cap_offset,
                cfg_type,
                bar,
                bar_offset,
                bar_len,
                notify_off_multiplier,
            },
        );
        assert!(
            prev.is_none(),
            "duplicate vendor-specific capability with cfg_type={cfg_type} at offset 0x{cap_offset:02x}",
        );
    }
    caps
}

fn assert_config_space_matches_profile(
    virtio_cfg: &mut PciConfigSpace,
    profile_cfg: &mut PciConfigSpace,
    profile: PciDeviceProfile,
) {
    let mut mismatches: Vec<String> = Vec::new();

    let virtio_vendor_id = read_u16(virtio_cfg, 0x00);
    let profile_vendor_id = read_u16(profile_cfg, 0x00);
    if virtio_vendor_id != profile_vendor_id {
        mismatches.push(format!(
            "vendor_id mismatch: virtio=0x{virtio_vendor_id:04x} profile=0x{profile_vendor_id:04x}"
        ));
    }

    let virtio_device_id = read_u16(virtio_cfg, 0x02);
    let profile_device_id = read_u16(profile_cfg, 0x02);
    if virtio_device_id != profile_device_id {
        mismatches.push(format!(
            "device_id mismatch: virtio=0x{virtio_device_id:04x} profile=0x{profile_device_id:04x}"
        ));
    }

    let virtio_revision_id = read_u8(virtio_cfg, 0x08);
    let profile_revision_id = read_u8(profile_cfg, 0x08);
    if virtio_revision_id != profile_revision_id {
        mismatches.push(format!(
            "revision_id mismatch: virtio=0x{virtio_revision_id:02x} profile=0x{profile_revision_id:02x}"
        ));
    }

    let virtio_class = (
        read_u8(virtio_cfg, 0x0b),
        read_u8(virtio_cfg, 0x0a),
        read_u8(virtio_cfg, 0x09),
    );
    let profile_class = (
        read_u8(profile_cfg, 0x0b),
        read_u8(profile_cfg, 0x0a),
        read_u8(profile_cfg, 0x09),
    );
    if virtio_class != profile_class {
        mismatches.push(format!(
            "class_code mismatch: virtio={virtio_class:02x?} profile={profile_class:02x?} (base, sub, prog_if)"
        ));
    }

    let virtio_header_type = read_u8(virtio_cfg, 0x0e);
    let profile_header_type = read_u8(profile_cfg, 0x0e);
    if virtio_header_type != profile_header_type {
        mismatches.push(format!(
            "header_type mismatch: virtio=0x{virtio_header_type:02x} profile=0x{profile_header_type:02x}"
        ));
    }

    let virtio_subsystem_vendor_id = read_u16(virtio_cfg, 0x2c);
    let profile_subsystem_vendor_id = read_u16(profile_cfg, 0x2c);
    if virtio_subsystem_vendor_id != profile_subsystem_vendor_id {
        mismatches.push(format!(
            "subsystem_vendor_id mismatch: virtio=0x{virtio_subsystem_vendor_id:04x} profile=0x{profile_subsystem_vendor_id:04x}"
        ));
    }

    let virtio_subsystem_id = read_u16(virtio_cfg, 0x2e);
    let profile_subsystem_id = read_u16(profile_cfg, 0x2e);
    if virtio_subsystem_id != profile_subsystem_id {
        mismatches.push(format!(
            "subsystem_id mismatch: virtio=0x{virtio_subsystem_id:04x} profile=0x{profile_subsystem_id:04x}"
        ));
    }

    let virtio_bar0 = virtio_cfg.bar_definition(0);
    let profile_bar0 = profile_cfg.bar_definition(0);
    if virtio_bar0 != profile_bar0 {
        mismatches.push(format!(
            "BAR0 definition mismatch:\n  virtio: {virtio_bar0:?}\n  profile: {profile_bar0:?}"
        ));
    } else if let Some(PciBarDefinition::Mmio64 { size, .. }) = virtio_bar0 {
        if size == 0 {
            mismatches.push("BAR0 size is 0 (unexpected)".to_string());
        }
    }

    let virtio_caps = extract_virtio_vendor_caps(virtio_cfg);
    let profile_caps = extract_virtio_vendor_caps(profile_cfg);
    if virtio_caps != profile_caps {
        mismatches.push(format!(
            "virtio vendor-specific capability layout mismatch:\n  virtio: {virtio_caps:#?}\n  profile: {profile_caps:#?}"
        ));
    }

    // Verify that non-vendor-specific capabilities (notably MSI-X) also match the canonical
    // profiles. Historically the profiles omitted MSI-X while the virtio transport exposed it,
    // which made guests unable to enable MSI-X through the canonical config space.
    let virtio_msix_off = virtio_cfg
        .capability_list()
        .into_iter()
        .find(|cap| cap.id == PCI_CAP_ID_MSIX)
        .map(|cap| cap.offset);
    let profile_msix_off = profile_cfg
        .capability_list()
        .into_iter()
        .find(|cap| cap.id == PCI_CAP_ID_MSIX)
        .map(|cap| cap.offset);
    if virtio_msix_off != profile_msix_off {
        let virtio_off = virtio_msix_off
            .map(|off| format!("0x{off:02x}"))
            .unwrap_or_else(|| "none".to_string());
        let profile_off = profile_msix_off
            .map(|off| format!("0x{off:02x}"))
            .unwrap_or_else(|| "none".to_string());
        mismatches.push(format!(
            "MSI-X capability offset mismatch: virtio={virtio_off} profile={profile_off}"
        ));
    }
    if let (Some(virtio_msix_off), Some(profile_msix_off)) = (virtio_msix_off, profile_msix_off) {
        let virtio_base = u16::from(virtio_msix_off);
        let profile_base = u16::from(profile_msix_off);
        let virtio_ctrl = read_u16(virtio_cfg, virtio_base + 0x02);
        let profile_ctrl = read_u16(profile_cfg, profile_base + 0x02);
        if virtio_ctrl != profile_ctrl {
            mismatches.push(format!(
                "MSI-X control mismatch: virtio=0x{virtio_ctrl:04x} profile=0x{profile_ctrl:04x}"
            ));
        }
        let virtio_table = read_u32(virtio_cfg, virtio_base + 0x04);
        let profile_table = read_u32(profile_cfg, profile_base + 0x04);
        if virtio_table != profile_table {
            mismatches.push(format!(
                "MSI-X table register mismatch: virtio=0x{virtio_table:08x} profile=0x{profile_table:08x}"
            ));
        }
        let virtio_pba = read_u32(virtio_cfg, virtio_base + 0x08);
        let profile_pba = read_u32(profile_cfg, profile_base + 0x08);
        if virtio_pba != profile_pba {
            mismatches.push(format!(
                "MSI-X PBA register mismatch: virtio=0x{virtio_pba:08x} profile=0x{profile_pba:08x}"
            ));
        }
    }

    if !mismatches.is_empty() {
        panic!(
            "PCI config-space drift detected for {}:\n{}",
            profile.name,
            mismatches.join("\n")
        );
    }
}

#[test]
fn virtio_pci_config_space_matches_canonical_profiles() {
    // virtio-net
    {
        let mut dev = VirtioPciDevice::new(
            Box::new(VirtioNet::new(
                LoopbackNet::default(),
                [0x52, 0x54, 0x00, 0, 0, 0x29],
            )),
            Box::new(InterruptLog::default()),
        );
        let mut profile_cfg = VIRTIO_NET.build_config_space();
        assert_config_space_matches_profile(dev.config_mut(), &mut profile_cfg, VIRTIO_NET);
    }

    // virtio-blk
    {
        let mut dev = VirtioPciDevice::new(
            Box::new(VirtioBlk::new(Box::new(MemDisk::new(512 * 1024)))),
            Box::new(InterruptLog::default()),
        );
        let mut profile_cfg = VIRTIO_BLK.build_config_space();
        assert_config_space_matches_profile(dev.config_mut(), &mut profile_cfg, VIRTIO_BLK);
    }

    // virtio-input keyboard (function 0, multi-function header type)
    {
        let mut dev = VirtioPciDevice::new(
            Box::new(VirtioInput::new(VirtioInputDeviceKind::Keyboard)),
            Box::new(InterruptLog::default()),
        );
        let mut profile_cfg = VIRTIO_INPUT_KEYBOARD.build_config_space();
        assert_config_space_matches_profile(
            dev.config_mut(),
            &mut profile_cfg,
            VIRTIO_INPUT_KEYBOARD,
        );
    }

    // virtio-input mouse (function 1)
    {
        let mut dev = VirtioPciDevice::new(
            Box::new(VirtioInput::new(VirtioInputDeviceKind::Mouse)),
            Box::new(InterruptLog::default()),
        );
        let mut profile_cfg = VIRTIO_INPUT_MOUSE.build_config_space();
        assert_config_space_matches_profile(dev.config_mut(), &mut profile_cfg, VIRTIO_INPUT_MOUSE);
    }

    // virtio-input tablet (optional function 2)
    {
        let mut dev = VirtioPciDevice::new(
            Box::new(VirtioInput::new(VirtioInputDeviceKind::Tablet)),
            Box::new(InterruptLog::default()),
        );
        let mut profile_cfg = VIRTIO_INPUT_TABLET.build_config_space();
        assert_config_space_matches_profile(
            dev.config_mut(),
            &mut profile_cfg,
            VIRTIO_INPUT_TABLET,
        );
    }

    // virtio-snd
    #[cfg(feature = "snd")]
    {
        let mut dev = VirtioPciDevice::new(
            Box::new(VirtioSnd::new(
                aero_audio::ring::AudioRingBuffer::new_stereo(8),
            )),
            Box::new(InterruptLog::default()),
        );
        let mut profile_cfg = VIRTIO_SND.build_config_space();
        assert_config_space_matches_profile(dev.config_mut(), &mut profile_cfg, VIRTIO_SND);
    }
}
