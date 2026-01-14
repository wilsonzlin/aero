use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotVersion, SnapshotWriter};
use aero_usb::device::AttachedUsbDevice;
use aero_usb::hid::UsbHidKeyboardHandle;
use aero_usb::xhci::{regs, XhciController, PORTSC_PED, PORTSC_PR};
use aero_usb::{MemoryBus, SetupPacket, UsbInResult, UsbOutResult};

#[derive(Default)]
struct PanicMem;

impl MemoryBus for PanicMem {
    fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
        panic!("unexpected DMA read");
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        panic!("unexpected DMA write");
    }
}

struct TestMem {
    data: Vec<u8>,
}

impl TestMem {
    fn new(size: usize) -> Self {
        Self { data: vec![0; size] }
    }
}

impl MemoryBus for TestMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let start = paddr as usize;
        let end = start.saturating_add(buf.len());
        if end > self.data.len() {
            buf.fill(0);
            return;
        }
        buf.copy_from_slice(&self.data[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let start = paddr as usize;
        let end = start.saturating_add(buf.len());
        if end > self.data.len() {
            return;
        }
        self.data[start..end].copy_from_slice(buf);
    }
}

fn control_no_data(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(
        matches!(dev.handle_in(0, 0), UsbInResult::Data(data) if data.is_empty()),
        "expected ACK for status stage"
    );
}

#[test]
fn xhci_snapshot_save_state_is_parseable_and_has_unique_tags() {
    let ctrl = XhciController::new();
    let bytes = ctrl.save_state();

    // Regression test for duplicate TLV field tags: `SnapshotReader::parse` rejects duplicates.
    let r = SnapshotReader::parse(&bytes, *b"XHCI").expect("parse xHCI snapshot");
    assert!(r.bytes(11).is_some(), "expected ports field under tag 11");
    assert!(
        r.bytes(12).is_some(),
        "expected host_controller_error field under tag 12"
    );
}

#[test]
fn xhci_snapshot_roundtrip_preserves_ports_and_device_state() {
    let mut ctrl = XhciController::new();
    let mut mem = PanicMem;
    let portsc_off = regs::port::portsc_offset(0);

    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.attach_device(0, Box::new(keyboard.clone()));

    // Reset the port so it becomes enabled (PED=1) before snapshotting.
    ctrl.mmio_write(&mut mem, portsc_off, 4, PORTSC_PR);
    for _ in 0..50 {
        ctrl.tick_1ms_no_dma();
    }

    // Minimal enumeration/configuration so we can observe device state roundtrip.
    let dev = ctrl
        .find_device_by_topology(1, &[])
        .expect("expected device behind root port 1");
    control_no_data(
        dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    let before_portsc = ctrl.read_portsc(0);
    assert!(
        before_portsc & PORTSC_PED != 0,
        "expected port to be enabled before snapshot"
    );

    let bytes = ctrl.save_state();

    // Restore into a new controller with a pre-attached keyboard handle. This exercises the
    // snapshot's ability to load into existing host-provided device instances.
    let mut restored = XhciController::new();
    let keyboard_restored = UsbHidKeyboardHandle::new();
    restored.attach_device(0, Box::new(keyboard_restored.clone()));
    restored.load_state(&bytes).expect("load snapshot");

    assert!(
        keyboard_restored.configured(),
        "expected configured state to roundtrip through snapshot"
    );
    assert_eq!(
        restored
            .find_device_by_topology(1, &[])
            .expect("device should still be attached")
            .address(),
        1,
        "expected device address to roundtrip"
    );

    let after_portsc = restored.read_portsc(0);
    assert!(
        after_portsc & PORTSC_PED != 0,
        "expected port enabled bit to roundtrip through snapshot"
    );
}

#[test]
fn xhci_snapshot_loads_legacy_tag_mapping_for_ports_and_hce() {
    let mut ctrl = XhciController::new();
    let mut mem = PanicMem;
    let portsc_off = regs::port::portsc_offset(0);

    let keyboard = UsbHidKeyboardHandle::new();
    ctrl.attach_device(0, Box::new(keyboard.clone()));

    // Reset the port so it becomes enabled (PED=1) before snapshotting.
    ctrl.mmio_write(&mut mem, portsc_off, 4, PORTSC_PR);
    for _ in 0..50 {
        ctrl.tick_1ms_no_dma();
    }

    // Minimal enumeration/configuration so we can observe device state roundtrip.
    let dev = ctrl
        .find_device_by_topology(1, &[])
        .expect("expected device behind root port 1");
    control_no_data(
        dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x05, // SET_ADDRESS
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    control_no_data(
        dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );
    assert!(keyboard.configured(), "expected keyboard to be configured");

    let before_portsc = ctrl.read_portsc(0);
    assert!(
        before_portsc & PORTSC_PED != 0,
        "expected port to be enabled before snapshot"
    );

    let bytes = ctrl.save_state();
    let r = SnapshotReader::parse(&bytes, *b"XHCI").expect("parse snapshot");

    // Cover both historical and mixed tag mappings:
    //
    // - Legacy (0.3) mapping:
    //   - tag 11: host_controller_error
    //   - tag 12: ports
    // - Current (0.4+) mapping:
    //   - tag 11: ports
    //   - tag 12: host_controller_error
    //
    // Some intermediate snapshots had a 0.4 header while still using the legacy tag layout (and
    // vice versa), so we exercise multiple combinations of header version + tag layout.
    for (device_version, swap_tags) in [
        // Expected legacy 0.3 snapshot.
        (SnapshotVersion::new(0, 3), true),
        // 0.4 header but legacy tag mapping.
        (SnapshotVersion::new(0, 4), true),
        // 0.3 header but 0.4 tag mapping.
        (SnapshotVersion::new(0, 3), false),
    ] {
        let mut w = SnapshotWriter::new(*b"XHCI", device_version);
        for (tag, field) in r.iter_fields() {
            let out_tag = if swap_tags {
                match tag {
                    11 => 12,
                    12 => 11,
                    other => other,
                }
            } else {
                tag
            };
            w.field_bytes(out_tag, field.to_vec());
        }
        let legacy_bytes = w.finish();

        let mut restored = XhciController::new();
        let keyboard_restored = UsbHidKeyboardHandle::new();
        restored.attach_device(0, Box::new(keyboard_restored.clone()));
        restored
            .load_state(&legacy_bytes)
            .unwrap_or_else(|e| panic!("load legacy snapshot version {}: {e}", device_version));

        assert!(
            keyboard_restored.configured(),
            "expected configured state to roundtrip through legacy snapshot"
        );
        assert_eq!(
            restored
                .find_device_by_topology(1, &[])
                .expect("device should still be attached")
                .address(),
            1,
            "expected device address to roundtrip"
        );

        let after_portsc = restored.read_portsc(0);
        assert!(
            after_portsc & PORTSC_PED != 0,
            "expected port enabled bit to roundtrip through legacy snapshot"
        );
    }
}

#[test]
fn xhci_snapshot_roundtrip_preserves_tick_time_and_dma_probe_state() {
    let mut ctrl = XhciController::new();
    let mut mem = TestMem::new(0x4000);

    // Point CRCR at a known address and seed a dword there so `tick_1ms_with_dma` updates the
    // controller's internal DMA probe state.
    let crcr_ptr = 0x1000u64;
    mem.write_u32(crcr_ptr, 0xaabb_ccdd);

    ctrl.mmio_write(&mut mem, regs::REG_CRCR_LO, 4, crcr_ptr as u32);
    ctrl.mmio_write(&mut mem, regs::REG_CRCR_HI, 4, (crcr_ptr >> 32) as u32);
    ctrl.mmio_write(&mut mem, regs::REG_USBCMD, 4, regs::USBCMD_RUN);

    // Advance time and run the tick-driven DMA probe.
    for _ in 0..3 {
        ctrl.tick_1ms_with_dma(&mut mem);
    }

    let bytes = ctrl.save_state();
    let r = SnapshotReader::parse(&bytes, *b"XHCI").expect("parse xHCI snapshot");

    // Snapshot v0.7 stores the 1ms tick counter + last tick DMA dword.
    assert_eq!(r.u64(27).unwrap().unwrap(), 3);
    assert_eq!(r.u32(28).unwrap().unwrap(), 0xaabb_ccdd);

    // Ensure restore preserves the fields as well (not just save).
    let mut restored = XhciController::new();
    restored.load_state(&bytes).expect("load snapshot");
    let bytes2 = restored.save_state();
    let r2 = SnapshotReader::parse(&bytes2, *b"XHCI").expect("parse restored snapshot");
    assert_eq!(r2.u64(27).unwrap().unwrap(), 3);
    assert_eq!(r2.u32(28).unwrap().unwrap(), 0xaabb_ccdd);
}
