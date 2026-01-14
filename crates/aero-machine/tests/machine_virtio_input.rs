use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile;
use aero_devices::pci::PciInterruptPin;
use aero_machine::{Machine, MachineConfig};
use aero_platform::interrupts::InterruptController;
use aero_virtio::devices::input::{
    VirtioInput, VirtioInputEvent, BTN_BACK, BTN_FORWARD, BTN_LEFT, BTN_TASK, EV_KEY, EV_LED,
    EV_REL, EV_SYN, KEY_A, KEY_B, LED_CAPSL, REL_HWHEEL, REL_WHEEL, REL_X, REL_Y, SYN_REPORT,
    VIRTIO_INPUT_CFG_EV_BITS, VIRTIO_INPUT_CFG_ID_DEVIDS, VIRTIO_INPUT_CFG_ID_NAME,
};
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
fn virtio_input_device_cfg_mmio_exposes_expected_name_devids_and_ev_bits() {
    let mut m = new_test_machine();
    enable_a20(&mut m);

    let cases = [
        (
            profile::VIRTIO_INPUT_KEYBOARD.bdf,
            "Aero Virtio Keyboard",
            // bustype=PCI(0x0006), vendor=0x1af4, product=0x0001, version=0x0001
            [0x06, 0x00, 0xF4, 0x1A, 0x01, 0x00, 0x01, 0x00],
            0x03u8, // EV_SYN + EV_KEY
            true,   // EV_LED
        ),
        (
            profile::VIRTIO_INPUT_MOUSE.bdf,
            "Aero Virtio Mouse",
            // bustype=PCI(0x0006), vendor=0x1af4, product=0x0002, version=0x0001
            [0x06, 0x00, 0xF4, 0x1A, 0x02, 0x00, 0x01, 0x00],
            0x07u8, // EV_SYN + EV_KEY + EV_REL
            false,  // EV_LED
        ),
    ];

    for (bdf, expected_name, expected_devids, expected_ev_bits0, expect_led) in cases {
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

        let dev_cfg = bar0_base + u64::from(profile::VIRTIO_DEVICE_CFG_BAR0_OFFSET);

        // ------------------------------
        // VIRTIO_INPUT_CFG_ID_NAME (str)
        // ------------------------------
        m.write_physical_u8(dev_cfg, VIRTIO_INPUT_CFG_ID_NAME);
        m.write_physical_u8(dev_cfg + 1, 0);
        let size = m.read_physical_u8(dev_cfg + 2);
        assert_ne!(size, 0, "expected non-zero name size for {bdf:?}");
        let payload = m.read_physical_bytes(dev_cfg + 8, usize::from(size));
        assert_eq!(
            *payload.last().unwrap(),
            0,
            "expected name to be null-terminated for {bdf:?}"
        );
        let nul = payload
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(payload.len());
        let name = std::str::from_utf8(&payload[..nul]).expect("valid UTF-8 name");
        assert_eq!(name, expected_name, "{bdf:?} name mismatch");

        // --------------------------------
        // VIRTIO_INPUT_CFG_ID_DEVIDS (8B)
        // --------------------------------
        m.write_physical_u8(dev_cfg, VIRTIO_INPUT_CFG_ID_DEVIDS);
        m.write_physical_u8(dev_cfg + 1, 0);
        let size = m.read_physical_u8(dev_cfg + 2);
        assert_eq!(size, 8, "{bdf:?} expected devids size=8");
        let devids = m.read_physical_bytes(dev_cfg + 8, 8);
        assert_eq!(devids, expected_devids, "{bdf:?} devids mismatch");

        // --------------------------------
        // VIRTIO_INPUT_CFG_EV_BITS (bitmap)
        // --------------------------------
        m.write_physical_u8(dev_cfg, VIRTIO_INPUT_CFG_EV_BITS);
        m.write_physical_u8(dev_cfg + 1, 0); // subsel = 0 -> event type bitmap
        let size = m.read_physical_u8(dev_cfg + 2);
        assert_eq!(size, 128, "{bdf:?} expected ev bitmap size=128");
        let ev_bits = m.read_physical_bytes(dev_cfg + 8, 3);
        assert_eq!(
            ev_bits[0] & 0x07,
            expected_ev_bits0,
            "{bdf:?} event type bits mismatch"
        );
        // EV_LED is bit 17 -> byte 2, bit 1.
        let has_led = (ev_bits[(EV_LED / 8) as usize] & (1u8 << (EV_LED % 8))) != 0;
        assert_eq!(has_led, expect_led, "{bdf:?} EV_LED presence mismatch");

        // Sanity: EV_SYN and EV_KEY must always be advertised.
        assert_ne!(
            ev_bits[(EV_SYN / 8) as usize] & (1u8 << (EV_SYN % 8)),
            0,
            "{bdf:?} must advertise EV_SYN"
        );
        assert_ne!(
            ev_bits[(EV_KEY / 8) as usize] & (1u8 << (EV_KEY % 8)),
            0,
            "{bdf:?} must advertise EV_KEY"
        );

        // Mouse must advertise EV_REL; keyboard must not.
        let has_rel = (ev_bits[(EV_REL / 8) as usize] & (1u8 << (EV_REL % 8))) != 0;
        assert_eq!(
            has_rel,
            bdf == profile::VIRTIO_INPUT_MOUSE.bdf,
            "{bdf:?} EV_REL presence mismatch"
        );

        // --------------------------------
        // VIRTIO_INPUT_CFG_EV_BITS sub-bitmaps
        // --------------------------------
        // Key bitmap: keyboard must advertise KEY_A; mouse must advertise BTN_LEFT.
        m.write_physical_u8(dev_cfg, VIRTIO_INPUT_CFG_EV_BITS);
        m.write_physical_u8(dev_cfg + 1, EV_KEY as u8);
        let size = m.read_physical_u8(dev_cfg + 2);
        assert_eq!(size, 128, "{bdf:?} expected key bitmap size=128");
        let key_bits = m.read_physical_bytes(dev_cfg + 8, 128);
        if bdf == profile::VIRTIO_INPUT_KEYBOARD.bdf {
            let has_key_a = (key_bits[(KEY_A / 8) as usize] & (1u8 << (KEY_A % 8))) != 0;
            assert!(has_key_a, "{bdf:?} must advertise KEY_A");
        } else {
            let has_btn_left = (key_bits[(BTN_LEFT / 8) as usize] & (1u8 << (BTN_LEFT % 8))) != 0;
            assert!(has_btn_left, "{bdf:?} must advertise BTN_LEFT");
            // Extra mouse buttons are advertised to match the web runtime's expanded
            // `InputEventType.MouseButtons` bitmask support.
            for &code in &[BTN_FORWARD, BTN_BACK, BTN_TASK] {
                let present = (key_bits[(code / 8) as usize] & (1u8 << (code % 8))) != 0;
                assert!(present, "{bdf:?} must advertise BTN code {code}");
            }
        }

        // LED bitmap: keyboard must advertise LED_CAPSL; mouse should not.
        m.write_physical_u8(dev_cfg, VIRTIO_INPUT_CFG_EV_BITS);
        m.write_physical_u8(dev_cfg + 1, EV_LED as u8);
        let size = m.read_physical_u8(dev_cfg + 2);
        assert_eq!(size, 128, "{bdf:?} expected led bitmap size=128");
        let led_bits = m.read_physical_bytes(dev_cfg + 8, 1);
        let has_capsl_led = (led_bits[(LED_CAPSL / 8) as usize] & (1u8 << (LED_CAPSL % 8))) != 0;
        assert_eq!(
            has_capsl_led,
            bdf == profile::VIRTIO_INPUT_KEYBOARD.bdf,
            "{bdf:?} LED_CAPSL presence mismatch"
        );

        // REL bitmap: mouse must advertise REL_X/REL_Y/REL_WHEEL/REL_HWHEEL.
        if bdf == profile::VIRTIO_INPUT_MOUSE.bdf {
            m.write_physical_u8(dev_cfg, VIRTIO_INPUT_CFG_EV_BITS);
            m.write_physical_u8(dev_cfg + 1, EV_REL as u8);
            let size = m.read_physical_u8(dev_cfg + 2);
            assert_eq!(size, 128, "{bdf:?} expected rel bitmap size=128");
            let rel_bits = m.read_physical_bytes(dev_cfg + 8, 2);
            for &code in &[REL_X, REL_Y, REL_WHEEL, REL_HWHEEL] {
                assert_ne!(
                    rel_bits[(code / 8) as usize] & (1u8 << (code % 8)),
                    0,
                    "{bdf:?} must advertise REL code {code}"
                );
            }
        }
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
    let notify_off = m.read_physical_u16(bar0_base + COMMON + 0x1e);
    let notify_addr = bar0_base + NOTIFY + u64::from(notify_off) * NOTIFY_MULT;
    m.write_physical_u16(notify_addr, 0);
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

    let notify_off = m.read_physical_u16(bar0_base + COMMON + 0x1e);
    let notify_addr = bar0_base + NOTIFY + u64::from(notify_off) * NOTIFY_MULT;
    m.write_physical_u16(notify_addr, 0);
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

#[test]
fn virtio_input_inject_key_syncs_legacy_intx_into_pic() {
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

    // Configure event queue 0 with 2 writable event buffers (each is one `virtio_input_event`).
    let desc = 0x00a0_0000;
    let avail = 0x00a1_0000;
    let used = 0x00a2_0000;
    let event0 = 0x00a3_0000;
    let event1 = 0x00a3_1000;

    let zero_page = vec![0u8; 0x1000];
    m.write_physical(desc, &zero_page);
    m.write_physical(avail, &zero_page);
    m.write_physical(used, &zero_page);
    m.write_physical(event0, &[0u8; 8]);
    m.write_physical(event1, &[0u8; 8]);

    // Select queue 0 and program ring addresses.
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

    // Notify queue 0 so the platform pops and caches the buffers.
    m.write_physical_u16(bar0_base + NOTIFY, 0);
    m.process_virtio_input();
    assert_eq!(m.read_physical_u16(used + 2), 0);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let expected_vector = {
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let irq = u8::try_from(gsi).expect("virtio-input gsi must fit in u8");
        assert!(
            irq < 16,
            "expected virtio-input to route to a legacy PIC IRQ (got GSI {gsi})"
        );

        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        for i in 0..16 {
            ints.pic_mut().set_masked(i, true);
        }
        ints.pic_mut().set_masked(2, false); // cascade
        ints.pic_mut().set_masked(irq, false);

        if irq < 8 {
            0x20u8.wrapping_add(irq)
        } else {
            0x28u8.wrapping_add(irq.wrapping_sub(8))
        }
    };

    assert_eq!(interrupts.borrow().get_pending(), None);

    // One key injection yields EV_KEY + EV_SYN and should assert legacy INTx and make the PIC
    // vector visible without requiring a `run_slice` call.
    m.inject_virtio_key(KEY_A, true);
    assert_eq!(interrupts.borrow().get_pending(), Some(expected_vector));
}

#[test]
fn virtio_input_input_batch_syncs_legacy_intx_into_pic() {
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

    // Configure event queue 0 with 2 writable event buffers (each is one `virtio_input_event`).
    let desc = 0x00e0_0000;
    let avail = 0x00e1_0000;
    let used = 0x00e2_0000;
    let event0 = 0x00e3_0000;
    let event1 = 0x00e3_1000;

    let zero_page = vec![0u8; 0x1000];
    m.write_physical(desc, &zero_page);
    m.write_physical(avail, &zero_page);
    m.write_physical(used, &zero_page);
    m.write_physical(event0, &[0u8; 8]);
    m.write_physical(event1, &[0u8; 8]);

    // Select queue 0 and program ring addresses.
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

    // Notify queue 0 so the platform pops and caches the buffers.
    m.write_physical_u16(bar0_base + NOTIFY, 0);
    m.process_virtio_input();
    assert_eq!(m.read_physical_u16(used + 2), 0);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let expected_vector = {
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let irq = u8::try_from(gsi).expect("virtio-input gsi must fit in u8");
        assert!(
            irq < 16,
            "expected virtio-input to route to a legacy PIC IRQ (got GSI {gsi})"
        );

        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        for i in 0..16 {
            ints.pic_mut().set_masked(i, true);
        }
        ints.pic_mut().set_masked(2, false); // cascade
        ints.pic_mut().set_masked(irq, false);

        if irq < 8 {
            0x20u8.wrapping_add(irq)
        } else {
            0x28u8.wrapping_add(irq.wrapping_sub(8))
        }
    };

    assert_eq!(interrupts.borrow().get_pending(), None);

    // InputEventQueue batch: HID keyboard usage 0x04 (A) pressed.
    let words: [u32; 6] = [
        1, 0, 6, // InputEventType.KeyHidUsage
        0, 0x0104, // usage=0x04 | (pressed=1 << 8)
        0,
    ];
    m.inject_input_batch(&words);

    assert_eq!(m.read_physical_u16(used + 2), 2);
    assert_eq!(m.read_physical_bytes(event0, 8), &[1, 0, 30, 0, 1, 0, 0, 0]);
    assert_eq!(m.read_physical_bytes(event1, 8), &[0u8; 8]);

    assert_eq!(interrupts.borrow().get_pending(), Some(expected_vector));
}

#[test]
fn virtio_input_input_batch_routes_mouse_events_via_virtio() {
    let mut m = new_test_machine();
    enable_a20(&mut m);

    let bdf = profile::VIRTIO_INPUT_MOUSE.bdf;

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

    // Configure event queue 0 with 8 writable event buffers:
    // - MouseMove -> REL_X + REL_Y + SYN
    // - MouseButtons -> BTN_LEFT + SYN
    // - MouseWheel -> REL_WHEEL + REL_HWHEEL + SYN
    // Total = 8 events.
    let desc = 0x00f0_0000;
    let avail = 0x00f1_0000;
    let used = 0x00f2_0000;
    let bufs = [
        0x00f3_0000,
        0x00f3_0010,
        0x00f3_0020,
        0x00f3_0030,
        0x00f3_0040,
        0x00f3_0050,
        0x00f3_0060,
        0x00f3_0070,
    ];

    let zero_page = vec![0u8; 0x1000];
    m.write_physical(desc, &zero_page);
    m.write_physical(avail, &zero_page);
    m.write_physical(used, &zero_page);
    for &buf in &bufs {
        m.write_physical(buf, &[0u8; 8]);
    }

    // Select queue 0 and program ring addresses.
    m.write_physical_u16(bar0_base + COMMON + 0x16, 0);
    m.write_physical_u64(bar0_base + COMMON + 0x20, desc);
    m.write_physical_u64(bar0_base + COMMON + 0x28, avail);
    m.write_physical_u64(bar0_base + COMMON + 0x30, used);
    m.write_physical_u16(bar0_base + COMMON + 0x1c, 1);

    for (i, &buf) in bufs.iter().enumerate() {
        write_desc(&mut m, desc, i as u16, buf, 8, VIRTQ_DESC_F_WRITE);
    }

    // Post all descriptor chains.
    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, bufs.len() as u16);
    for (i, _) in bufs.iter().enumerate() {
        m.write_physical_u16(avail + 4 + (i as u64) * 2, i as u16);
    }
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Notify queue 0 so the platform pops and caches the buffers.
    m.write_physical_u16(bar0_base + NOTIFY, 0);
    m.process_virtio_input();
    assert_eq!(m.read_physical_u16(used + 2), 0);

    // InputEventQueue batch:
    // - MouseMove: dx=+5, dy=-3 (PS/2 dy up => -3 means down)
    // - MouseButtons: left pressed
    // - MouseWheel: dz=+1 (wheel up), dx=+2 (scroll right / HWHEEL)
    let words: [u32; 14] = [
        3,
        0,
        // InputEventType.MouseMove
        2,
        0,
        5,
        (-3i32) as u32,
        // InputEventType.MouseButtons
        3,
        0,
        0x01,
        0,
        // InputEventType.MouseWheel
        4,
        0,
        1,
        2,
    ];
    m.inject_input_batch(&words);

    assert_eq!(m.read_physical_u16(used + 2), 8, "expected 8 used entries");

    let expected: [(u16, u16, i32); 8] = [
        (EV_REL, REL_X, 5),
        // InputEventQueue uses PS/2 dy-up; the machine converts to Linux REL_Y where + is down.
        (EV_REL, REL_Y, 3),
        (EV_SYN, SYN_REPORT, 0),
        (EV_KEY, BTN_LEFT, 1),
        (EV_SYN, SYN_REPORT, 0),
        (EV_REL, REL_WHEEL, 1),
        (EV_REL, REL_HWHEEL, 2),
        (EV_SYN, SYN_REPORT, 0),
    ];

    for (&buf, (ty, code, val)) in bufs.iter().zip(expected) {
        let got = m.read_physical_bytes(buf, 8);
        let type_ = u16::from_le_bytes([got[0], got[1]]);
        let code_ = u16::from_le_bytes([got[2], got[3]]);
        let value_ = i32::from_le_bytes([got[4], got[5], got[6], got[7]]);
        assert_eq!((type_, code_, value_), (ty, code, val));
    }
}

#[test]
fn virtio_input_input_batch_routes_extra_mouse_buttons_via_virtio() {
    let mut m = new_test_machine();
    enable_a20(&mut m);

    let bdf = profile::VIRTIO_INPUT_MOUSE.bdf;

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

    // Configure event queue 0 with 4 writable event buffers:
    // - BTN_FORWARD down + SYN
    // - BTN_FORWARD up + SYN
    let desc = 0x00f0_0000;
    let avail = 0x00f1_0000;
    let used = 0x00f2_0000;
    let bufs = [0x00f3_0000, 0x00f3_0010, 0x00f3_0020, 0x00f3_0030];

    let zero_page = vec![0u8; 0x1000];
    m.write_physical(desc, &zero_page);
    m.write_physical(avail, &zero_page);
    m.write_physical(used, &zero_page);
    for &buf in &bufs {
        m.write_physical(buf, &[0u8; 8]);
    }

    // Select queue 0 and program ring addresses.
    m.write_physical_u16(bar0_base + COMMON + 0x16, 0);
    m.write_physical_u64(bar0_base + COMMON + 0x20, desc);
    m.write_physical_u64(bar0_base + COMMON + 0x28, avail);
    m.write_physical_u64(bar0_base + COMMON + 0x30, used);
    m.write_physical_u16(bar0_base + COMMON + 0x1c, 1);

    for (i, &buf) in bufs.iter().enumerate() {
        write_desc(&mut m, desc, i as u16, buf, 8, VIRTQ_DESC_F_WRITE);
    }

    // Post all descriptor chains.
    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, bufs.len() as u16);
    for (i, _) in bufs.iter().enumerate() {
        m.write_physical_u16(avail + 4 + (i as u64) * 2, i as u16);
    }
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Notify queue 0 so the platform pops and caches the buffers.
    m.write_physical_u16(bar0_base + NOTIFY, 0);
    m.process_virtio_input();
    assert_eq!(m.read_physical_u16(used + 2), 0);

    // InputEventQueue batch:
    // - MouseButtons: BTN_FORWARD down (bit 5), then up.
    let words: [u32; 10] = [
        2,
        0,
        // InputEventType.MouseButtons
        3,
        0,
        0x20,
        0,
        // InputEventType.MouseButtons
        3,
        0,
        0,
        0,
    ];
    m.inject_input_batch(&words);

    assert_eq!(m.read_physical_u16(used + 2), 4, "expected 4 used entries");

    let expected: [(u16, u16, i32); 4] = [
        (EV_KEY, BTN_FORWARD, 1),
        (EV_SYN, SYN_REPORT, 0),
        (EV_KEY, BTN_FORWARD, 0),
        (EV_SYN, SYN_REPORT, 0),
    ];

    for (&buf, (ty, code, val)) in bufs.iter().zip(expected) {
        let got = m.read_physical_bytes(buf, 8);
        let type_ = u16::from_le_bytes([got[0], got[1]]);
        let code_ = u16::from_le_bytes([got[2], got[3]]);
        let value_ = i32::from_le_bytes([got[4], got[5], got[6], got[7]]);
        assert_eq!((type_, code_, value_), (ty, code, val));
    }
}

#[test]
fn virtio_input_intx_is_gated_on_pci_command_intx_disable() {
    let mut m = new_test_machine();
    enable_a20(&mut m);

    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;

    // Enable PCI memory decoding + bus mastering (DMA), but disable legacy INTx delivery via
    // COMMAND.INTX_DISABLE (bit 10).
    let pci_cfg = m
        .pci_config_ports()
        .expect("pci config ports should exist when pc platform is enabled");
    let cmd = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().read_config(bdf, 0x04, 2) as u16
    };
    set_pci_command(&m, bdf, cmd | (1 << 1) | (1 << 2) | (1 << 10));

    let bar0_base = read_bar0_base(&m, bdf);
    assert_ne!(bar0_base, 0);

    // Canonical virtio capability layout for Aero profiles (BAR0 offsets).
    const COMMON: u64 = profile::VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
    const NOTIFY: u64 = profile::VIRTIO_NOTIFY_CFG_BAR0_OFFSET as u64;

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

    // Configure event queue 0 with 2 writable event buffers so a single key press (EV_KEY + EV_SYN)
    // can be delivered.
    let desc = 0x00d0_0000;
    let avail = 0x00d1_0000;
    let used = 0x00d2_0000;
    let event0 = 0x00d3_0000;
    let event1 = 0x00d3_1000;

    let zero_page = vec![0u8; 0x1000];
    m.write_physical(desc, &zero_page);
    m.write_physical(avail, &zero_page);
    m.write_physical(used, &zero_page);
    m.write_physical(event0, &[0u8; 8]);
    m.write_physical(event1, &[0u8; 8]);

    // Select queue 0 and program ring addresses.
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

    // Notify queue 0 so the platform pops and caches the buffers.
    m.write_physical_u16(bar0_base + NOTIFY, 0);
    m.process_virtio_input();
    assert_eq!(m.read_physical_u16(used + 2), 0);

    let interrupts = m.platform_interrupts().expect("pc platform enabled");
    let pci_intx = m.pci_intx_router().expect("pc platform enabled");
    let expected_vector = {
        let gsi = pci_intx.borrow().gsi_for_intx(bdf, PciInterruptPin::IntA);
        let irq = u8::try_from(gsi).expect("virtio-input gsi must fit in u8");
        assert!(
            irq < 16,
            "expected virtio-input to route to a legacy PIC IRQ (got GSI {gsi})"
        );

        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        for i in 0..16 {
            ints.pic_mut().set_masked(i, true);
        }
        ints.pic_mut().set_masked(2, false); // cascade
        ints.pic_mut().set_masked(irq, false);

        if irq < 8 {
            0x20u8.wrapping_add(irq)
        } else {
            0x28u8.wrapping_add(irq.wrapping_sub(8))
        }
    };

    assert_eq!(interrupts.borrow().get_pending(), None);

    // With COMMAND.INTX_DISABLE set, injecting an event should still deliver the event into guest
    // memory but must not raise a legacy PIC interrupt.
    m.inject_virtio_key(KEY_A, true);
    assert_eq!(m.read_physical_u16(used + 2), 2);
    assert_eq!(
        interrupts.borrow().get_pending(),
        None,
        "expected no PIC interrupt while COMMAND.INTX_DISABLE is set"
    );

    // Clearing COMMAND.INTX_DISABLE should allow the still-pending virtio IRQ latch to assert INTx
    // on the next poll.
    set_pci_command(&m, bdf, cmd | (1 << 1) | (1 << 2));
    m.poll_pci_intx_lines();
    assert_eq!(interrupts.borrow().get_pending(), Some(expected_vector));
}

#[test]
fn virtio_input_eventq_dma_is_gated_on_pci_bus_master_enable() {
    let mut m = new_test_machine();
    enable_a20(&mut m);

    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;

    // Enable PCI memory decoding but keep Bus Master Enable (BME) clear initially.
    let pci_cfg = m
        .pci_config_ports()
        .expect("pci config ports should exist when pc platform is enabled");
    let cmd = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().read_config(bdf, 0x04, 2) as u16
    };
    set_pci_command(&m, bdf, (cmd | (1 << 1)) & !(1 << 2));

    let bar0_base = read_bar0_base(&m, bdf);
    assert_ne!(bar0_base, 0);

    // Canonical virtio capability layout for Aero profiles (BAR0 offsets).
    const COMMON: u64 = profile::VIRTIO_COMMON_CFG_BAR0_OFFSET as u64;
    const NOTIFY: u64 = profile::VIRTIO_NOTIFY_CFG_BAR0_OFFSET as u64;

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

    // Configure event queue 0 with 4 writable event buffers so a key press+release (EV_KEY+EV_SYN x2)
    // can be delivered once DMA is permitted.
    let desc = 0x00b0_0000;
    let avail = 0x00b1_0000;
    let used = 0x00b2_0000;
    let event0 = 0x00b3_0000;
    let event1 = 0x00b3_0010;
    let event2 = 0x00b3_0020;
    let event3 = 0x00b3_0030;

    let zero_page = vec![0u8; 0x1000];
    m.write_physical(desc, &zero_page);
    m.write_physical(avail, &zero_page);
    m.write_physical(used, &zero_page);
    for buf in [event0, event1, event2, event3] {
        m.write_physical(buf, &[0u8; 8]);
    }

    // Select queue 0 and program ring addresses.
    m.write_physical_u16(bar0_base + COMMON + 0x16, 0);
    m.write_physical_u64(bar0_base + COMMON + 0x20, desc);
    m.write_physical_u64(bar0_base + COMMON + 0x28, avail);
    m.write_physical_u64(bar0_base + COMMON + 0x30, used);
    m.write_physical_u16(bar0_base + COMMON + 0x1c, 1);

    write_desc(&mut m, desc, 0, event0, 8, VIRTQ_DESC_F_WRITE);
    write_desc(&mut m, desc, 1, event1, 8, VIRTQ_DESC_F_WRITE);
    write_desc(&mut m, desc, 2, event2, 8, VIRTQ_DESC_F_WRITE);
    write_desc(&mut m, desc, 3, event3, 8, VIRTQ_DESC_F_WRITE);

    // Post all descriptor chains.
    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, 4);
    m.write_physical_u16(avail + 4, 0);
    m.write_physical_u16(avail + 6, 1);
    m.write_physical_u16(avail + 8, 2);
    m.write_physical_u16(avail + 10, 3);
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Notify queue 0, but DMA should be gated while BME=0.
    m.write_physical_u16(bar0_base + NOTIFY, 0);
    m.process_virtio_input();
    assert_eq!(
        m.read_physical_u16(used + 2),
        0,
        "expected no DMA while PCI BME=0"
    );

    // Host injects key press + release while BME is disabled. The events should remain buffered in
    // the device model without touching guest memory.
    m.inject_virtio_key(KEY_A, true);
    m.inject_virtio_key(KEY_A, false);

    assert_eq!(m.read_physical_u16(used + 2), 0);
    assert_eq!(m.read_physical_bytes(event0, 8), &[0u8; 8]);
    assert_eq!(m.read_physical_bytes(event2, 8), &[0u8; 8]);

    let virtio_kb = m.virtio_input_keyboard().expect("virtio-input enabled");
    let pending = virtio_kb
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .unwrap()
        .pending_events_len();
    assert_eq!(pending, 4, "expected 4 queued events (press+release)");

    // Now enable Bus Master Enable and allow the device to DMA. The pending events should be
    // delivered immediately into the already-posted buffers.
    set_pci_command(&m, bdf, cmd | (1 << 1) | (1 << 2));
    m.process_virtio_input();

    assert_eq!(m.read_physical_u16(used + 2), 4);
    assert_eq!(m.read_physical_bytes(event0, 8), &[1, 0, 30, 0, 1, 0, 0, 0]);
    assert_eq!(m.read_physical_bytes(event2, 8), &[1, 0, 30, 0, 0, 0, 0, 0]);

    let pending = virtio_kb
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .unwrap()
        .pending_events_len();
    assert_eq!(
        pending, 0,
        "expected all pending events to be delivered once DMA is enabled"
    );
}

#[test]
fn virtio_input_statusq_dma_is_gated_on_pci_bus_master_enable() {
    let mut m = new_test_machine();
    enable_a20(&mut m);

    let bdf = profile::VIRTIO_INPUT_KEYBOARD.bdf;

    // Enable PCI memory decoding but keep Bus Master Enable (BME) clear initially.
    let pci_cfg = m
        .pci_config_ports()
        .expect("pci config ports should exist when pc platform is enabled");
    let cmd = {
        let mut pci_cfg = pci_cfg.borrow_mut();
        pci_cfg.bus_mut().read_config(bdf, 0x04, 2) as u16
    };
    set_pci_command(&m, bdf, (cmd | (1 << 1)) & !(1 << 2));

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

    // Configure statusq (queue 1) with one buffer containing:
    // - EV_LED / LED_CAPSL / 1
    // - EV_SYN / SYN_REPORT / 0
    let desc = 0x00c0_0000;
    let avail = 0x00c1_0000;
    let used = 0x00c2_0000;
    let buf0 = 0x00c3_0000;

    let mut payload = [0u8; 16];
    payload[0..2].copy_from_slice(&EV_LED.to_le_bytes());
    payload[2..4].copy_from_slice(&LED_CAPSL.to_le_bytes());
    payload[4..8].copy_from_slice(&1i32.to_le_bytes());
    payload[8..10].copy_from_slice(&EV_SYN.to_le_bytes());
    payload[10..12].copy_from_slice(&SYN_REPORT.to_le_bytes());
    payload[12..16].copy_from_slice(&0i32.to_le_bytes());

    let zero_page = vec![0u8; 0x1000];
    m.write_physical(desc, &zero_page);
    m.write_physical(avail, &zero_page);
    m.write_physical(used, &zero_page);
    m.write_physical(buf0, &payload);

    m.write_physical_u16(bar0_base + COMMON + 0x16, 1); // queue_select = 1
    m.write_physical_u64(bar0_base + COMMON + 0x20, desc);
    m.write_physical_u64(bar0_base + COMMON + 0x28, avail);
    m.write_physical_u64(bar0_base + COMMON + 0x30, used);
    m.write_physical_u16(bar0_base + COMMON + 0x1c, 1); // queue_enable

    write_desc(&mut m, desc, 0, buf0, payload.len() as u32, 0);
    m.write_physical_u16(avail, 0);
    m.write_physical_u16(avail + 2, 1);
    m.write_physical_u16(avail + 4, 0);
    m.write_physical_u16(used, 0);
    m.write_physical_u16(used + 2, 0);

    // Notify queue 1, but DMA should be gated while BME=0, so the chain must not be consumed.
    let notify_off = m.read_physical_u16(bar0_base + COMMON + 0x1e);
    let notify_addr = bar0_base + NOTIFY + u64::from(notify_off) * NOTIFY_MULT;
    m.write_physical_u16(notify_addr, 0);

    m.process_virtio_input();
    assert_eq!(
        m.read_physical_u16(used + 2),
        0,
        "expected statusq not to be consumed while BME=0"
    );
    let virtio_kb = m.virtio_input_keyboard().expect("virtio-input enabled");
    let leds = virtio_kb
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .unwrap()
        .leds_mask();
    assert_eq!(leds, 0, "expected no LED state update while BME=0");

    // Enable Bus Master Enable and allow the device to DMA. The queued statusq buffer should be
    // consumed immediately and update the LED state.
    set_pci_command(&m, bdf, cmd | (1 << 1) | (1 << 2));
    m.process_virtio_input();
    assert_eq!(m.read_physical_u16(used + 2), 1);

    let leds = virtio_kb
        .borrow_mut()
        .device_mut::<VirtioInput>()
        .unwrap()
        .leds_mask();
    assert_eq!(
        leds, 0x02,
        "expected Caps Lock LED bit to be set once DMA is enabled"
    );
}
