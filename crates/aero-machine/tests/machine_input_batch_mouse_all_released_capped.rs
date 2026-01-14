#![cfg(not(target_arch = "wasm32"))]

use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig};
use aero_virtio::devices::input::VirtioInput;
use aero_virtio::pci::VIRTIO_STATUS_DRIVER_OK;

#[test]
fn inject_input_batch_mouse_all_released_sync_is_bounded_per_call() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_virtio_input: true,
        // Keep the machine minimal/deterministic for this regression test.
        enable_i8042: false,
        enable_uhci: false,
        enable_synthetic_usb_hid: false,
        enable_serial: false,
        enable_vga: false,
        enable_reset_ctrl: false,
        enable_debugcon: false,
        enable_e1000: false,
        enable_virtio_net: false,
        enable_ahci: false,
        enable_nvme: false,
        enable_ide: false,
        enable_virtio_blk: false,
        ..Default::default()
    })
    .unwrap();

    let virtio_mouse = m
        .virtio_input_mouse()
        .expect("virtio-input mouse should be present");
    assert_eq!(
        virtio_mouse
            .borrow()
            .device::<VirtioInput>()
            .expect("virtio-input mouse should be a VirtioInput device")
            .pending_events_len(),
        0,
        "sanity: virtio-input mouse should start without pending events"
    );

    // Flip the virtio device to `DRIVER_OK` so `inject_input_batch` will perform the best-effort
    // virtio sync in the "unknown all released" branch.
    assert!(
        !m.virtio_input_mouse_driver_ok(),
        "virtio-input mouse should start without DRIVER_OK"
    );
    let bdf = profile::VIRTIO_INPUT_MOUSE.bdf;
    let pci_cfg = m.pci_config_ports().expect("PCI config ports should be present");
    {
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config_mut(bdf)
            .expect("virtio-input mouse PCI config should exist");
        let mut cmd = cfg.command();
        // Enable BAR MMIO decoding so the BAR0 write is accepted.
        cmd |= 0x0002; // COMMAND.MEM
        // Keep bus mastering disabled so `process_virtio_input()` does not attempt DMA.
        cmd &= !0x0004; // COMMAND.BME
        cfg.set_command(cmd);
    }
    let bar0 = m
        .pci_bar_base(bdf, profile::VIRTIO_BAR0_INDEX)
        .expect("virtio-input BAR0 must be assigned by BIOS POST");
    assert_ne!(bar0, 0, "virtio-input BAR0 base must be non-zero");
    m.write_physical_u8(bar0 + 0x14, VIRTIO_STATUS_DRIVER_OK);
    assert!(m.virtio_input_mouse_driver_ok(), "expected DRIVER_OK");

    // Malicious/untrusted batches could contain repeated "buttons=0" events. The machine should
    // perform the expensive "all released" sync at most once per `inject_input_batch` call.
    let words: [u32; 10] = [
        2, 0, // header: 2 events
        3, 0, 0, 0, // MouseButtons=0
        3, 0, 0, 0, // MouseButtons=0 again
    ];
    m.inject_input_batch(&words);

    assert_eq!(
        virtio_mouse
            .borrow()
            .device::<VirtioInput>()
            .unwrap()
            .pending_events_len(),
        16,
        "expected a single virtio all-release sync (8 buttons * (EV_KEY + SYN))"
    );
}

