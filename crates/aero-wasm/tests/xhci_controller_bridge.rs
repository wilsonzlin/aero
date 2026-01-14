#![cfg(target_arch = "wasm32")]

use aero_io_snapshot::io::state::SnapshotReader;
use aero_usb::xhci::regs;
use aero_wasm::XhciControllerBridge;
use wasm_bindgen_test::wasm_bindgen_test;

mod common;

const BRIDGE_ID: [u8; 4] = *b"XHCB";
const CTRL_ID: [u8; 4] = *b"XHCI";

const TAG_BRIDGE_CONTROLLER: u16 = 1;
const TAG_BRIDGE_TICK_COUNT: u16 = 2;

// Keep in sync with `crates/aero-usb/src/xhci/snapshot.rs`.
const TAG_CTRL_TIME_MS: u16 = 27;
const TAG_CTRL_LAST_TICK_DMA_DWORD: u16 = 28;

const PCI_COMMAND_BME: u32 = 1 << 2;

fn bridge_snapshot_tick_count(bytes: &[u8]) -> u64 {
    let r = SnapshotReader::parse(bytes, BRIDGE_ID).expect("parse XhciControllerBridge snapshot");
    r.u64(TAG_BRIDGE_TICK_COUNT)
        .expect("read tick_count")
        .unwrap_or(0)
}

fn bridge_snapshot_ctrl_bytes<'a>(bytes: &'a [u8]) -> &'a [u8] {
    let r = SnapshotReader::parse(bytes, BRIDGE_ID).expect("parse XhciControllerBridge snapshot");
    r.bytes(TAG_BRIDGE_CONTROLLER)
        .expect("missing controller state bytes")
}

fn ctrl_snapshot_time_ms(bytes: &[u8]) -> u64 {
    let r = SnapshotReader::parse(bytes, CTRL_ID).expect("parse XhciController snapshot");
    r.u64(TAG_CTRL_TIME_MS).expect("read time_ms").unwrap_or(0)
}

fn ctrl_snapshot_last_tick_dma_dword(bytes: &[u8]) -> u32 {
    let r = SnapshotReader::parse(bytes, CTRL_ID).expect("parse XhciController snapshot");
    r.u32(TAG_CTRL_LAST_TICK_DMA_DWORD)
        .expect("read last_tick_dma_dword")
        .unwrap_or(0)
}

#[wasm_bindgen_test]
fn xhci_controller_bridge_step_frames_advances_controller_time() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);
    let mut bridge = XhciControllerBridge::new(guest_base, guest_size).unwrap();

    bridge.step_frames(5);

    let snap = bridge.save_state();
    assert_eq!(bridge_snapshot_tick_count(&snap), 5);

    let ctrl_bytes = bridge_snapshot_ctrl_bytes(&snap);
    assert_eq!(
        ctrl_snapshot_time_ms(ctrl_bytes),
        5,
        "expected underlying xHCI model time to advance"
    );
}

#[wasm_bindgen_test]
fn xhci_controller_bridge_step_frames_gates_dma_on_pci_bme() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);
    let guest =
        unsafe { core::slice::from_raw_parts_mut(guest_base as *mut u8, guest_size as usize) };

    // Seed guest memory at 0x1000 with a recognizable dword.
    guest[0x1000..0x1004].copy_from_slice(&0x1234_5678u32.to_le_bytes());

    let mut bridge = XhciControllerBridge::new(guest_base, guest_size).unwrap();

    // Program CRCR to point at 0x1000 and start the controller.
    bridge.mmio_write(regs::REG_CRCR_LO as u32, 4, 0x1000);
    bridge.mmio_write(regs::REG_CRCR_HI as u32, 4, 0);
    bridge.mmio_write(regs::REG_USBCMD as u32, 4, regs::USBCMD_RUN);

    // With bus mastering disabled, stepping must not read guest RAM. The xHCI model should see
    // open-bus 0xFF values instead.
    bridge.step_frame();
    let snap = bridge.save_state();
    let ctrl_bytes = bridge_snapshot_ctrl_bytes(&snap);
    assert_eq!(
        ctrl_snapshot_last_tick_dma_dword(ctrl_bytes),
        0xffff_ffff,
        "expected tick-driven DMA read to observe open-bus while BME is disabled"
    );
    assert_eq!(
        &guest[0x1000..0x1004],
        &0x1234_5678u32.to_le_bytes(),
        "expected guest RAM to remain untouched while BME is disabled"
    );

    // Enable bus mastering and step again: the controller should now be able to DMA from guest RAM.
    bridge.set_pci_command(PCI_COMMAND_BME);
    bridge.step_frame();
    let snap2 = bridge.save_state();
    let ctrl_bytes2 = bridge_snapshot_ctrl_bytes(&snap2);
    assert_eq!(
        ctrl_snapshot_last_tick_dma_dword(ctrl_bytes2),
        0x1234_5678,
        "expected tick-driven DMA read to see guest RAM once BME is enabled"
    );
}

#[wasm_bindgen_test]
fn xhci_controller_bridge_snapshot_restore_preserves_tick_count() {
    let (guest_base, guest_size) = common::alloc_guest_region_bytes(0x8000);
    let mut bridge = XhciControllerBridge::new(guest_base, guest_size).unwrap();
    bridge.step_frames(7);

    let snap = bridge.save_state();
    assert_eq!(bridge_snapshot_tick_count(&snap), 7);

    let mut restored = XhciControllerBridge::new(guest_base, guest_size).unwrap();
    restored.load_state(&snap).unwrap();

    let snap2 = restored.save_state();
    assert_eq!(
        bridge_snapshot_tick_count(&snap2),
        7,
        "expected load_state to restore tick_count"
    );
}
