use aero_devices::pci::profile::build_canonical_io_bus;
use aero_devices::pci::{PciBdf, PciBus, PciConfigMechanism1, PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};

const PCI_VENDOR_ID_VIRTIO: u16 = 0x1af4;
const PCI_DEVICE_ID_VIRTIO_INPUT_MODERN: u16 = 0x1052;
const PCI_SUBSYSTEM_VENDOR_ID_VIRTIO: u16 = 0x1af4;
const PCI_SUBSYSTEM_DEVICE_ID_VIRTIO_INPUT_KEYBOARD: u16 = 0x0010;
const PCI_SUBSYSTEM_DEVICE_ID_VIRTIO_INPUT_MOUSE: u16 = 0x0011;
const PCI_REVISION_ID_CONTRACT_V1: u8 = 0x01;

fn cfg_addr(bdf: PciBdf, offset: u16) -> u32 {
    0x8000_0000
        | (u32::from(bdf.bus) << 16)
        | (u32::from(bdf.device) << 11)
        | (u32::from(bdf.function) << 8)
        | (u32::from(offset) & 0xFC)
}

fn read_dword(cfg: &mut PciConfigMechanism1, bus: &mut PciBus, bdf: PciBdf, offset: u16) -> u32 {
    cfg.io_write(bus, PCI_CFG_ADDR_PORT, 4, cfg_addr(bdf, offset));
    cfg.io_read(bus, PCI_CFG_DATA_PORT, 4)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PciFunctionInfo {
    bdf: PciBdf,
    vendor_id: u16,
    device_id: u16,
    revision_id: u8,
    header_type: u8,
    subsystem_vendor_id: u16,
    subsystem_device_id: u16,
}

impl PciFunctionInfo {
    fn matches_vendor_device(&self, vendor_id: u16, device_id: u16) -> bool {
        self.vendor_id == vendor_id && self.device_id == device_id
    }
}

fn read_function_info(
    cfg: &mut PciConfigMechanism1,
    bus: &mut PciBus,
    bdf: PciBdf,
) -> Option<PciFunctionInfo> {
    let id = read_dword(cfg, bus, bdf, 0x00);
    let vendor_id = (id & 0xFFFF) as u16;
    if vendor_id == 0xFFFF {
        return None;
    }
    let device_id = (id >> 16) as u16;

    let class = read_dword(cfg, bus, bdf, 0x08);
    let revision_id = (class & 0xFF) as u8;

    let hdr = read_dword(cfg, bus, bdf, 0x0C);
    let header_type = ((hdr >> 16) & 0xFF) as u8;

    let subsys = read_dword(cfg, bus, bdf, 0x2C);
    let subsystem_vendor_id = (subsys & 0xFFFF) as u16;
    let subsystem_device_id = (subsys >> 16) as u16;

    Some(PciFunctionInfo {
        bdf,
        vendor_id,
        device_id,
        revision_id,
        header_type,
        subsystem_vendor_id,
        subsystem_device_id,
    })
}

fn enumerate_bus0(cfg: &mut PciConfigMechanism1, bus: &mut PciBus) -> Vec<PciFunctionInfo> {
    let mut found = Vec::new();

    for device in 0u8..32 {
        let fn0 = PciBdf::new(0, device, 0);
        let Some(info0) = read_function_info(cfg, bus, fn0) else {
            continue;
        };
        found.push(info0);

        let functions = if info0.header_type & 0x80 != 0 { 8 } else { 1 };
        for function in 1u8..functions {
            let bdf = PciBdf::new(0, device, function);
            if let Some(info) = read_function_info(cfg, bus, bdf) {
                found.push(info);
            }
        }
    }

    found
}

#[test]
fn virtio_input_is_exposed_as_multifunction_keyboard_and_mouse_pair() {
    let mut bus = build_canonical_io_bus();
    let mut cfg = PciConfigMechanism1::new();

    let found = enumerate_bus0(&mut cfg, &mut bus);

    let keyboard = found
        .iter()
        .copied()
        .find(|info| {
            info.matches_vendor_device(PCI_VENDOR_ID_VIRTIO, PCI_DEVICE_ID_VIRTIO_INPUT_MODERN)
                && info.subsystem_vendor_id == PCI_SUBSYSTEM_VENDOR_ID_VIRTIO
                && info.subsystem_device_id == PCI_SUBSYSTEM_DEVICE_ID_VIRTIO_INPUT_KEYBOARD
        })
        .expect("missing virtio-input keyboard function in canonical PCI topology");

    assert_eq!(
        keyboard.bdf.function, 0,
        "virtio-input keyboard must be function 0 (required for multifunction discovery)"
    );
    assert_eq!(
        keyboard.revision_id, PCI_REVISION_ID_CONTRACT_V1,
        "virtio-input keyboard must report REV_01"
    );
    assert_eq!(
        keyboard.header_type, 0x80,
        "virtio-input keyboard must set the multifunction bit (header_type 0x80)"
    );

    let mouse_bdf = PciBdf::new(keyboard.bdf.bus, keyboard.bdf.device, 1);
    let mouse = found
        .iter()
        .copied()
        .find(|info| info.bdf == mouse_bdf)
        .expect("virtio-input mouse must exist at the paired function 1 BDF");

    assert!(
        mouse.matches_vendor_device(PCI_VENDOR_ID_VIRTIO, PCI_DEVICE_ID_VIRTIO_INPUT_MODERN),
        "virtio-input mouse must share vendor/device IDs with the keyboard"
    );
    assert_eq!(
        mouse.subsystem_vendor_id, PCI_SUBSYSTEM_VENDOR_ID_VIRTIO,
        "virtio-input mouse must use the virtio subsystem vendor ID"
    );
    assert_eq!(
        mouse.subsystem_device_id, PCI_SUBSYSTEM_DEVICE_ID_VIRTIO_INPUT_MOUSE,
        "virtio-input mouse must use the mouse subsystem ID"
    );
    assert_eq!(
        mouse.revision_id, PCI_REVISION_ID_CONTRACT_V1,
        "virtio-input mouse must report REV_01"
    );
    assert_eq!(
        mouse.header_type, 0x00,
        "virtio-input mouse must not advertise itself as multifunction"
    );

    let virtio_input_functions = found
        .iter()
        .filter(|info| {
            info.matches_vendor_device(PCI_VENDOR_ID_VIRTIO, PCI_DEVICE_ID_VIRTIO_INPUT_MODERN)
        })
        .count();
    assert_eq!(
        virtio_input_functions, 2,
        "expected exactly two virtio-input functions (keyboard + mouse)"
    );
}
