use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig};
use aero_virtio::devices::input::{VirtioInput, VirtioInputEvent, EV_KEY, KEY_A, KEY_B};
use aero_virtio::pci::{
    VIRTIO_STATUS_ACKNOWLEDGE, VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FEATURES_OK,
};
use aero_virtio::queue::VIRTQ_DESC_F_WRITE;
use pretty_assertions::{assert_eq, assert_ne};

fn enable_a20(m: &mut Machine) {
    // Fast A20 gate at port 0x92: bit1 enables A20.
    m.io_write(A20_GATE_PORT, 1, 0x02);
}

fn read_bar0_base(m: &Machine, bdf: aero_devices::pci::PciBdf) -> u64 {
    let pci_cfg = m
        .pci_config_ports()
        .expect("pci config ports should exist when pc platform is enabled");
    let (bar0_lo, bar0_hi) = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        (bus.read_config(bdf, 0x10, 4), bus.read_config(bdf, 0x14, 4))
    };
    (u64::from(bar0_hi) << 32) | u64::from(bar0_lo & 0xFFFF_FFF0)
}

fn set_pci_command(m: &Machine, bdf: aero_devices::pci::PciBdf, command: u16) {
    let pci_cfg = m
        .pci_config_ports()
        .expect("pci config ports should exist when pc platform is enabled");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let bus = pci_cfg.bus_mut();
    let cfg = bus
        .device_config_mut(bdf)
        .expect("virtio-input function missing from pci bus");
    cfg.set_command(command);
}

fn write_desc(m: &mut Machine, table: u64, index: u16, addr: u64, len: u32, flags: u16) {
    let base = table + u64::from(index) * 16;
    m.write_physical_u64(base, addr);
    m.write_physical_u32(base + 8, len);
    m.write_physical_u16(base + 12, flags);
    m.write_physical_u16(base + 14, 0);
}

fn new_test_machine() -> Machine {
    Machine::new(MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep the machine minimal/deterministic for these integration tests.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        enable_virtio_net: false,
        enable_uhci: false,
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn virtio_input_pci_identity_matches_profile_for_both_functions() {
    let m = new_test_machine();
    let pci_cfg = m
        .pci_config_ports()
        .expect("pci config ports should exist when pc platform is enabled");
    let mut pci_cfg = pci_cfg.borrow_mut();
    let bus = pci_cfg.bus_mut();

    for p in [profile::VIRTIO_INPUT_KEYBOARD, profile::VIRTIO_INPUT_MOUSE] {
        let bdf = p.bdf;

        let vendor_id = bus.read_config(bdf, 0x00, 2) as u16;
        let device_id = bus.read_config(bdf, 0x02, 2) as u16;
        let revision_id = bus.read_config(bdf, 0x08, 1) as u8;
        let prog_if = bus.read_config(bdf, 0x09, 1) as u8;
        let sub_class = bus.read_config(bdf, 0x0a, 1) as u8;
        let base_class = bus.read_config(bdf, 0x0b, 1) as u8;
        let header_type = bus.read_config(bdf, 0x0e, 1) as u8;
        let subsystem_vendor_id = bus.read_config(bdf, 0x2c, 2) as u16;
        let subsystem_id = bus.read_config(bdf, 0x2e, 2) as u16;

        assert_eq!(vendor_id, p.vendor_id, "{bdf:?} vendor_id mismatch");
        assert_eq!(device_id, p.device_id, "{bdf:?} device_id mismatch");
        assert_eq!(revision_id, p.revision_id, "{bdf:?} revision_id mismatch");
        assert_eq!(
            (base_class, sub_class, prog_if),
            (p.class.base_class, p.class.sub_class, p.class.prog_if),
            "{bdf:?} class code mismatch"
        );
        assert_eq!(header_type, p.header_type, "{bdf:?} header_type mismatch");
        assert_eq!(
            subsystem_vendor_id, p.subsystem_vendor_id,
            "{bdf:?} subsystem_vendor_id mismatch"
        );
        assert_eq!(
            subsystem_id, p.subsystem_id,
            "{bdf:?} subsystem_id mismatch"
        );
    }
}

#[test]
fn virtio_input_bar0_is_assigned_and_mmio_reaches_transport_for_both_functions() {
    let mut m = new_test_machine();

    for bdf in [
        profile::VIRTIO_INPUT_KEYBOARD.bdf,
        profile::VIRTIO_INPUT_MOUSE.bdf,
    ] {
        // Enable PCI memory decoding (BAR0 MMIO).
        let pci_cfg = m
            .pci_config_ports()
            .expect("pci config ports should exist when pc platform is enabled");
        let cmd = {
            let mut pci_cfg = pci_cfg.borrow_mut();
            pci_cfg.bus_mut().read_config(bdf, 0x04, 2) as u16
        };
        set_pci_command(&m, bdf, cmd | (1 << 1));

        let bar0_base = read_bar0_base(&m, bdf);
        assert_ne!(bar0_base, 0, "{bdf:?} BAR0 must be assigned by BIOS POST");

        const COMMON: u64 = profile::VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
        let before = m.read_physical_u8(bar0_base + COMMON + 0x14);
        // Exercise an MMIO write + readback through the PCI MMIO router.
        m.write_physical_u8(bar0_base + COMMON + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
        let after = m.read_physical_u8(bar0_base + COMMON + 0x14);
        assert_ne!(
            before, after,
            "{bdf:?} MMIO write did not change device_status"
        );
        assert_eq!(after, VIRTIO_STATUS_ACKNOWLEDGE);
    }
}

#[test]
fn virtio_input_eventq_delivers_injected_event_end_to_end() {
    let mut m = new_test_machine();
    enable_a20(&mut m);

    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;

    // Enable PCI memory decoding + bus mastering (DMA).
    let pci_cfg = m
        .pci_config_ports()
        .expect("pci config ports should exist when pc platform is enabled");
    let cmd = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().read_config(bdf, 0x04, 2) as u16
    };
    set_pci_command(&m, bdf, cmd | (1 << 1) | (1 << 2));

    let bar0_base = read_bar0_base(&m, bdf);
    assert_ne!(bar0_base, 0);

    // Canonical virtio capability layout for Aero profiles (BAR0 offsets).
    const COMMON: u64 = profile::VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
    const NOTIFY: u64 = profile::VIRTIO_NOTIFY_CFG_BAR0_OFFSET as u64;
    const NOTIFY_MULT: u64 = profile::VIRTIO_NOTIFY_OFF_MULTIPLIER as u64;

    // Minimal feature negotiation: accept all device features and reach DRIVER_OK.
    m.write_physical_u8(bar0_base + COMMON + 0x14, VIRTIO_STATUS_ACKNOWLEDGE);
    m.write_physical_u8(
        bar0_base + COMMON + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER,
    );
    m.write_physical_u32(bar0_base + COMMON, 0);
    let f0 = m.read_physical_u32(bar0_base + COMMON + 0x04);
    m.write_physical_u32(bar0_base + COMMON + 0x08, 0);
    m.write_physical_u32(bar0_base + COMMON + 0x0c, f0);
    m.write_physical_u32(bar0_base + COMMON, 1);
    let f1 = m.read_physical_u32(bar0_base + COMMON + 0x04);
    m.write_physical_u32(bar0_base + COMMON + 0x08, 1);
    m.write_physical_u32(bar0_base + COMMON + 0x0c, f1);
    m.write_physical_u8(
        bar0_base + COMMON + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK,
    );
    m.write_physical_u8(
        bar0_base + COMMON + 0x14,
        VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK,
    );

    // Configure event queue 0.
    let desc = 0x0080_0000;
    let avail = 0x0081_0000;
    let used = 0x0082_0000;
    let event_buf = 0x0083_0000;

    let zero_page = vec![0u8; 0x1000];
    m.write_physical(desc, &zero_page);
    m.write_physical(avail, &zero_page);
    m.write_physical(used, &zero_page);

    m.write_physical(event_buf, &[0u8; 8]);

    // Select queue 0 and program ring addresses.
    m.write_physical_u16(bar0_base + COMMON + 0x16, 0);
    m.write_physical_u64(bar0_base + COMMON + 0x20, desc);
    m.write_physical_u64(bar0_base + COMMON + 0x28, avail);
    m.write_physical_u64(bar0_base + COMMON + 0x30, used);
    m.write_physical_u16(bar0_base + COMMON + 0x1c, 1);

    write_desc(&mut m, desc, 0, event_buf, 8, VIRTQ_DESC_F_WRITE);

    // Post the descriptor chain.
    m.write_physical_u16(avail, 0); // flags
    m.write_physical_u16(avail + 2, 1); // idx
    m.write_physical_u16(avail + 4, 0); // ring[0] = desc 0
    m.write_physical_u16(used, 0); // flags
    m.write_physical_u16(used + 2, 0); // idx

    // Notify queue 0 so the platform pops and caches the buffer.
    m.write_physical_u16(bar0_base + NOTIFY + 0 * NOTIFY_MULT, 0);
    m.process_virtio_input();

    assert_eq!(m.read_physical_u16(used + 2), 0);

    // Host injects an EV_KEY event and the guest buffer should receive it.
    let virtio_kb = m.virtio_input_keyboard().expect("virtio-input enabled");
    virtio_kb
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .unwrap()
        .push_event(VirtioInputEvent {
            type_: EV_KEY,
            code: KEY_A,
            value: 1,
        });
    m.process_virtio_input();

    assert_eq!(m.read_physical_u16(used + 2), 1);
    let len = m.read_physical_u32(used + 8);
    assert_eq!(len, 8);
    assert_eq!(
        m.read_physical_bytes(event_buf, 8),
        &[1, 0, 30, 0, 1, 0, 0, 0]
    );
}

#[test]
fn snapshot_restore_roundtrips_virtio_input_queue_progress_without_replaying_events() {
    let mut m = new_test_machine();
    enable_a20(&mut m);

    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;

    // Enable PCI memory decoding + bus mastering (DMA).
    let pci_cfg = m
        .pci_config_ports()
        .expect("pci config ports should exist when pc platform is enabled");
    let cmd = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().read_config(bdf, 0x04, 2) as u16
    };
    set_pci_command(&m, bdf, cmd | (1 << 1) | (1 << 2));

    let bar0_base = read_bar0_base(&m, bdf);
    assert_ne!(bar0_base, 0);

    const COMMON: u64 = profile::VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
    const NOTIFY: u64 = profile::VIRTIO_NOTIFY_CFG_BAR0_OFFSET as u64;
    const NOTIFY_MULT: u64 = profile::VIRTIO_NOTIFY_OFF_MULTIPLIER as u64;

    // Configure event queue 0 with two buffers.
    let desc = 0x0090_0000;
    let avail = 0x0091_0000;
    let used = 0x0092_0000;
    let event0 = 0x0093_0000;
    let event1 = 0x0093_0010;

    let zero_page = vec![0u8; 0x1000];
    m.write_physical(desc, &zero_page);
    m.write_physical(avail, &zero_page);
    m.write_physical(used, &zero_page);
    m.write_physical(event0, &[0u8; 8]);
    m.write_physical(event1, &[0u8; 8]);

    m.write_physical_u16(bar0_base + COMMON + 0x16, 0);
    m.write_physical_u64(bar0_base + COMMON + 0x20, desc);
    m.write_physical_u64(bar0_base + COMMON + 0x28, avail);
    m.write_physical_u64(bar0_base + COMMON + 0x30, used);
    m.write_physical_u16(bar0_base + COMMON + 0x1c, 1);

    write_desc(&mut m, desc, 0, event0, 8, VIRTQ_DESC_F_WRITE);
    write_desc(&mut m, desc, 1, event1, 8, VIRTQ_DESC_F_WRITE);

    // Post both descriptor chains.
    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, 2);
    m.write_physical_u16(avail + 4, 0);
    m.write_physical_u16(avail + 6, 1);
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    m.write_physical_u16(bar0_base + NOTIFY + 0 * NOTIFY_MULT, 0);
    m.process_virtio_input();

    // Deliver one event (consumes buffer 0) leaving buffer 1 outstanding.
    let virtio_kb = m.virtio_input_keyboard().expect("virtio-input enabled");
    virtio_kb
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .unwrap()
        .push_event(VirtioInputEvent {
            type_: EV_KEY,
            code: KEY_A,
            value: 1,
        });
    m.process_virtio_input();
    assert_eq!(m.read_physical_u16(used + 2), 1);
    assert_eq!(m.read_physical_bytes(event0, 8), &[1, 0, 30, 0, 1, 0, 0, 0]);

    let snapshot = m.take_snapshot_full().expect("snapshot should succeed");

    // Mutate state after snapshot: deliver another event (consumes buffer 1).
    virtio_kb
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .unwrap()
        .push_event(VirtioInputEvent {
            type_: EV_KEY,
            code: KEY_B,
            value: 1,
        });
    m.process_virtio_input();
    assert_eq!(m.read_physical_u16(used + 2), 2);
    assert_eq!(m.read_physical_bytes(event1, 8), &[1, 0, 48, 0, 1, 0, 0, 0]);

    m.restore_snapshot_bytes(&snapshot)
        .expect("restore should succeed");

    // After restore we should be back to the snapshot point: only the first used entry is present
    // and the second buffer has not been consumed/overwritten.
    assert_eq!(m.read_physical_u16(used + 2), 1);
    assert_eq!(m.read_physical_bytes(event0, 8), &[1, 0, 30, 0, 1, 0, 0, 0]);
    assert_eq!(m.read_physical_bytes(event1, 8), &[0u8; 8]);

    // Post-restore, injecting a new event should consume the outstanding second buffer (descriptor
    // index 1) without replaying the already-consumed first buffer.
    let virtio_kb = m.virtio_input_keyboard().expect("virtio-input enabled");
    virtio_kb
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .unwrap()
        .push_event(VirtioInputEvent {
            type_: EV_KEY,
            code: KEY_B,
            value: 1,
        });
    m.process_virtio_input();

    assert_eq!(m.read_physical_u16(used + 2), 2);
    assert_eq!(m.read_physical_bytes(event1, 8), &[1, 0, 48, 0, 1, 0, 0, 0]);
    assert_eq!(m.read_physical_bytes(event0, 8), &[1, 0, 30, 0, 1, 0, 0, 0]);
}
