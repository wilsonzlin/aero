use std::cell::RefCell;
use std::rc::Rc;

use aero_devices::pci::profile::NIC_E1000_82540EM;
use aero_machine::{Machine, MachineConfig};
use aero_net_backend::NetworkBackend;
use pretty_assertions::assert_eq;

#[derive(Default)]
struct SharedTxBackend {
    frames: Rc<RefCell<Vec<Vec<u8>>>>,
}

impl SharedTxBackend {
    fn new(shared: Rc<RefCell<Vec<Vec<u8>>>>) -> Self {
        Self { frames: shared }
    }
}

impl NetworkBackend for SharedTxBackend {
    fn transmit(&mut self, frame: Vec<u8>) {
        self.frames.borrow_mut().push(frame);
    }
}

#[test]
fn e1000_mmio_tx_reaches_backend() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_e1000: true,
        ..Default::default()
    })
    .unwrap();

    // Attach a dummy backend that records guest→host frames.
    let tx_frames = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
    m.set_network_backend(Box::new(SharedTxBackend::new(tx_frames.clone())));

    // Enable PCI Bus Mastering so the NIC is allowed to DMA.
    let bdf = NIC_E1000_82540EM.bdf;
    let pci_cfg = m
        .pci_config_ports()
        .expect("pci config ports should exist when pc platform is enabled");
    let cmd = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().read_config(bdf, 0x04, 2)
    };
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg
            .bus_mut()
            .write_config(bdf, 0x04, 2, cmd | (1 << 2));
    }

    // Resolve BAR0 MMIO base address.
    let bar0 = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().read_config(bdf, 0x10, 4)
    };
    let mmio_base = u64::from(bar0 & 0xFFFF_FFF0);
    assert_ne!(mmio_base, 0, "BAR0 must be assigned by platform PCI POST");

    // Guest RAM layout for the synthetic TX ring.
    let ring_base = 0x1000u64;
    let pkt_base = 0x2000u64;

    // Prepare a fixed-size Ethernet frame (>= minimum L2 frame).
    let pkt = vec![0x11u8; 60];
    m.write_physical(pkt_base, &pkt);

    // Legacy TX descriptor at ring_base.
    // buffer_addr (u64) | length (u16) | cso (u8) | cmd (u8) | status (u8) | css (u8) | special (u16)
    let mut desc = [0u8; 16];
    desc[0..8].copy_from_slice(&pkt_base.to_le_bytes());
    desc[8..10].copy_from_slice(&(pkt.len() as u16).to_le_bytes());
    desc[10] = 0; // cso
    desc[11] = 0b0000_1001; // EOP | RS
    desc[12] = 0; // status
    desc[13] = 0; // css
    desc[14..16].copy_from_slice(&0u16.to_le_bytes());
    m.write_physical(ring_base, &desc);

    // Program the NIC TX ring via MMIO (offsets match the E1000 register map).
    m.write_physical_u32(mmio_base + 0x3800, ring_base as u32); // TDBAL
    m.write_physical_u32(mmio_base + 0x3804, 0); // TDBAH
    m.write_physical_u32(mmio_base + 0x3808, 16 * 4); // TDLEN (4 descriptors)
    m.write_physical_u32(mmio_base + 0x3810, 0); // TDH
    m.write_physical_u32(mmio_base + 0x3818, 0); // TDT
    m.write_physical_u32(mmio_base + 0x0400, 1 << 1); // TCTL.EN

    // Submit descriptor 0 by advancing tail to 1, then poll the device.
    m.write_physical_u32(mmio_base + 0x3818, 1);
    m.poll_network();

    let frames = tx_frames.borrow();
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0], pkt);
}
