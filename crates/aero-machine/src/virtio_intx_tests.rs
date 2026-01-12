use aero_devices::pci::PciInterruptPin;
use aero_cpu_core::state::RFLAGS_IF;
use aero_platform::interrupts::InterruptController as PlatformInterruptController;
use aero_virtio::devices::net_offload::VirtioNetHdr;
use aero_virtio::pci::{
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::VIRTQ_DESC_F_NEXT;

use super::{Machine, MachineConfig, RunExit};

fn init_real_mode_cpu(m: &mut Machine, entry_ip: u16, rflags: u64) {
    fn set_real_segment(seg: &mut aero_cpu_core::state::Segment, selector: u16) {
        seg.selector = selector;
        seg.base = u64::from(selector) << 4;
        seg.limit = 0xFFFF;
        seg.access = 0;
    }

    m.cpu.pending = Default::default();
    set_real_segment(&mut m.cpu.state.segments.cs, 0);
    set_real_segment(&mut m.cpu.state.segments.ds, 0);
    set_real_segment(&mut m.cpu.state.segments.es, 0);
    set_real_segment(&mut m.cpu.state.segments.ss, 0);
    set_real_segment(&mut m.cpu.state.segments.fs, 0);
    set_real_segment(&mut m.cpu.state.segments.gs, 0);

    m.cpu.state.set_stack_ptr(0x8000);
    m.cpu.state.set_rip(u64::from(entry_ip));
    m.cpu.state.set_rflags(rflags);
    m.cpu.state.halted = false;

    // Ensure the real-mode IVT is in use.
    m.cpu.state.tables.idtr.base = 0;
    m.cpu.state.tables.idtr.limit = 0x03FF;
}

fn virtio_bar0_write(
    dev: &std::rc::Rc<std::cell::RefCell<aero_virtio::pci::VirtioPciDevice>>,
    offset: u64,
    data: &[u8],
) {
    dev.borrow_mut().bar0_write(offset, data);
}

fn virtio_bar0_read_u16(
    dev: &std::rc::Rc<std::cell::RefCell<aero_virtio::pci::VirtioPciDevice>>,
    offset: u64,
) -> u16 {
    let mut buf = [0u8; 2];
    dev.borrow_mut().bar0_read(offset, &mut buf);
    u16::from_le_bytes(buf)
}

fn virtio_bar0_read_u32(
    dev: &std::rc::Rc<std::cell::RefCell<aero_virtio::pci::VirtioPciDevice>>,
    offset: u64,
) -> u32 {
    let mut buf = [0u8; 4];
    dev.borrow_mut().bar0_read(offset, &mut buf);
    u32::from_le_bytes(buf)
}

fn virtio_bar0_write_u8(
    dev: &std::rc::Rc<std::cell::RefCell<aero_virtio::pci::VirtioPciDevice>>,
    offset: u64,
    value: u8,
) {
    virtio_bar0_write(dev, offset, &[value]);
}

fn virtio_bar0_write_u16(
    dev: &std::rc::Rc<std::cell::RefCell<aero_virtio::pci::VirtioPciDevice>>,
    offset: u64,
    value: u16,
) {
    virtio_bar0_write(dev, offset, &value.to_le_bytes());
}

fn virtio_bar0_write_u32(
    dev: &std::rc::Rc<std::cell::RefCell<aero_virtio::pci::VirtioPciDevice>>,
    offset: u64,
    value: u32,
) {
    virtio_bar0_write(dev, offset, &value.to_le_bytes());
}

fn virtio_bar0_write_u64(
    dev: &std::rc::Rc<std::cell::RefCell<aero_virtio::pci::VirtioPciDevice>>,
    offset: u64,
    value: u64,
) {
    virtio_bar0_write(dev, offset, &value.to_le_bytes());
}

fn write_desc(m: &mut Machine, table: u64, index: u16, addr: u64, len: u32, flags: u16, next: u16) {
    let base = table + u64::from(index) * 16;
    m.write_physical_u64(base, addr);
    m.write_physical_u32(base + 8, len);
    m.write_physical_u16(base + 12, flags);
    m.write_physical_u16(base + 14, next);
}

#[test]
fn pc_virtio_net_intx_is_synced_but_not_acknowledged_when_if0() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 8 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_net: true,
        enable_e1000: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let pci_intx = m.pci_intx_router().expect("pc platform enabled");

    let bdf = aero_devices::pci::profile::VIRTIO_NET.bdf;
    let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
    assert!(
        gsi < 16,
        "expected virtio-net INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
    );
    let expected_vector = if gsi < 8 {
        0x20u8.wrapping_add(gsi as u8)
    } else {
        0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
    };

    // Configure PIC offsets and unmask only the routed IRQ (and cascade if needed).
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        for irq in 0..16 {
            ints.pic_mut().set_masked(irq, true);
        }
        ints.pic_mut().set_masked(2, false);
        if let Ok(irq) = u8::try_from(gsi) {
            ints.pic_mut().set_masked(irq, false);
        }
    }

    // Enable PCI memory decoding + bus mastering for virtio-net so the device is allowed to DMA
    // and complete the TX descriptor chain.
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("virtio-net device missing from PCI bus");
        let command = cfg.command();
        cfg.set_command(command | (1 << 1) | (1 << 2));
    }

    let virtio = m.virtio_net.as_ref().expect("virtio-net enabled").clone();

    // Minimal virtio-net initialization: negotiate features and enable TX queue 1.
    virtio_bar0_write_u8(&virtio, 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    virtio_bar0_write_u8(
        &virtio,
        0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );

    // Accept all device features.
    virtio_bar0_write_u32(&virtio, 0x00, 0);
    let f0 = virtio_bar0_read_u32(&virtio, 0x04);
    virtio_bar0_write_u32(&virtio, 0x08, 0);
    virtio_bar0_write_u32(&virtio, 0x0c, f0);

    virtio_bar0_write_u32(&virtio, 0x00, 1);
    let f1 = virtio_bar0_read_u32(&virtio, 0x04);
    virtio_bar0_write_u32(&virtio, 0x08, 1);
    virtio_bar0_write_u32(&virtio, 0x0c, f1);

    virtio_bar0_write_u8(
        &virtio,
        0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    virtio_bar0_write_u8(
        &virtio,
        0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Configure TX queue 1.
    const TX_DESC: u64 = 0x203000;
    const TX_AVAIL: u64 = 0x204000;
    const TX_USED: u64 = 0x205000;

    virtio_bar0_write_u16(&virtio, 0x16, 1);
    assert!(virtio_bar0_read_u16(&virtio, 0x18) >= 8);
    virtio_bar0_write_u64(&virtio, 0x20, TX_DESC);
    virtio_bar0_write_u64(&virtio, 0x28, TX_AVAIL);
    virtio_bar0_write_u64(&virtio, 0x30, TX_USED);
    virtio_bar0_write_u16(&virtio, 0x1c, 1);

    // TX descriptor chain: header + payload.
    const HDR_ADDR: u64 = 0x206000;
    const PAYLOAD_ADDR: u64 = 0x206100;
    let hdr = [0u8; VirtioNetHdr::BASE_LEN];
    let payload = b"\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\x08\x00";
    m.write_physical(HDR_ADDR, &hdr);
    m.write_physical(PAYLOAD_ADDR, payload);

    write_desc(
        &mut m,
        TX_DESC,
        0,
        HDR_ADDR,
        hdr.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(&mut m, TX_DESC, 1, PAYLOAD_ADDR, payload.len() as u32, 0, 0);

    // Post the descriptor chain without writing to the notify register; `process_notified_queues`
    // should still treat it as pending because `avail.idx != next_avail`.
    m.write_physical_u16(TX_AVAIL, 0);
    m.write_physical_u16(TX_AVAIL + 2, 1);
    m.write_physical_u16(TX_AVAIL + 4, 0);
    m.write_physical_u16(TX_USED, 0);
    m.write_physical_u16(TX_USED + 2, 0);

    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        None
    );
    assert_eq!(m.read_physical_u16(TX_USED + 2), 0);

    // With IF=0, `run_slice` must not acknowledge or enqueue the interrupt, but it should still
    // sync PCI INTx sources so the PIC sees the asserted virtio-net INTx line.
    const ENTRY_IP: u16 = 0x1000;
    m.write_physical(u64::from(ENTRY_IP), &[0x90; 32]);
    init_real_mode_cpu(&mut m, ENTRY_IP, 0);
    m.cpu.state.halted = true;

    let exit = m.run_slice(5);
    assert_eq!(exit, RunExit::Halted { executed: 0 });
    assert!(m.cpu.pending.external_interrupts.is_empty());

    // The virtio TX chain should have completed, and the interrupt should remain pending in the
    // PIC (not acknowledged while IF=0).
    assert_eq!(m.read_physical_u16(TX_USED + 2), 1);
    assert_eq!(
        PlatformInterruptController::get_pending(&*interrupts.borrow()),
        Some(expected_vector)
    );

    // Once IF is set, the queued/pending interrupt should be delivered into the CPU core.
    //
    // Install a trivial real-mode ISR for the routed vector:
    // mov byte ptr [0x2000], 0xAA
    // iret
    const HANDLER_IP: u16 = 0x1100;
    m.write_physical(
        u64::from(HANDLER_IP),
        &[0xC6, 0x06, 0x00, 0x20, 0xAA, 0xCF],
    );
    let ivt_entry = u64::from(expected_vector) * 4;
    m.write_physical_u16(ivt_entry, HANDLER_IP);
    m.write_physical_u16(ivt_entry + 2, 0x0000);
    m.write_physical(0x2000, &[0x00]);

    init_real_mode_cpu(&mut m, ENTRY_IP, RFLAGS_IF);
    m.cpu.state.halted = true;
    let _ = m.run_slice(10);
    assert_eq!(m.read_physical_u8(0x2000), 0xAA);
}
