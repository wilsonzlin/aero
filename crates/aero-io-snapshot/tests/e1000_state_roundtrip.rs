use aero_io_snapshot::io::net::state::{E1000DeviceState, E1000TxContextState, E1000TxPacketState};
use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError, SnapshotWriter};

#[test]
fn e1000_device_state_roundtrip_is_lossless() {
    let mut state = E1000DeviceState::default();

    state.pci_regs[0..4].copy_from_slice(&0x100E_8086u32.to_le_bytes());
    state.pci_bar0 = 0xDEAD_BEE0;
    state.pci_bar0_probe = false;
    // In probe mode, the device model resets BAR1 to 0x1 (I/O indicator bit set).
    state.pci_bar1 = 0x1;
    state.pci_bar1_probe = true;

    state.ctrl = 0x0123_4567;
    state.status = 0x89AB_CDEF;
    state.eecd = 0x1111_2222;
    state.eerd = 0x3333_4444;
    state.ctrl_ext = 0x5555_6666;
    state.mdic = 0x7777_8888;
    state.io_reg = 0x9ABC_DEF0;

    state.icr = 0x0000_0080;
    state.ims = 0xFFFF_FFFF;

    state.rctl = 0x0000_0002;
    state.tctl = 0x0000_0002;

    state.rdbal = 0x1000;
    state.rdbah = 0;
    state.rdlen = 16 * 256;
    state.rdh = 1;
    state.rdt = 2;

    state.tdbal = 0x2000;
    state.tdbah = 0;
    state.tdlen = 16 * 256;
    state.tdh = 3;
    state.tdt = 4;

    state.tx_partial = vec![1, 2, 3, 4];
    state.tx_drop = false;
    state.tx_ctx = E1000TxContextState {
        ipcss: 1,
        ipcso: 2,
        ipcse: 3,
        tucss: 4,
        tucso: 5,
        tucse: 6,
        mss: 1460,
        hdr_len: 54,
    };
    state.tx_state = E1000TxPacketState::Legacy {
        cmd: 0xA5,
        css: 7,
        cso: 8,
    };

    state.mac_addr = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    state.ra_valid = true;
    state.eeprom[0] = 0x1234;
    state.eeprom[1] = 0x5678;
    state.phy[1] = 0x0004;

    state.other_regs = vec![(0x9000, 0x1122_3344), (0x9004, 0x5566_7788)];

    state.rx_pending = vec![vec![0x11; 60]];
    state.tx_out = vec![vec![0x22; 64]];

    let snap = state.save_state();
    let mut restored = E1000DeviceState::default();
    restored.load_state(&snap).unwrap();

    assert_eq!(state, restored);
}

#[test]
fn e1000_device_state_save_is_deterministic_with_unsorted_other_regs() {
    let state = E1000DeviceState {
        other_regs: vec![(0x2000, 2), (0x1000, 1), (0x3000, 3), (0x0000, 0)],
        ..Default::default()
    };

    // Save twice: encoding should canonicalize ordering internally.
    let snap0 = state.save_state();
    let snap1 = state.save_state();
    assert_eq!(snap0, snap1);

    let mut restored = E1000DeviceState::default();
    restored.load_state(&snap0).unwrap();
    assert_eq!(
        restored.other_regs,
        vec![(0x0000, 0), (0x1000, 1), (0x2000, 2), (0x3000, 3)]
    );
}

#[test]
fn e1000_device_state_rejects_unaligned_io_reg() {
    // TAG_IO_REG = 16.
    let mut w = SnapshotWriter::new(
        <E1000DeviceState as IoSnapshot>::DEVICE_ID,
        <E1000DeviceState as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u32(16, 0x123); // low 2 bits must be 0
    let bytes = w.finish();

    let mut state = E1000DeviceState::default();
    let err = state.load_state(&bytes).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("e1000 io_reg"));
}

#[test]
fn e1000_device_state_rejects_unaligned_other_regs_key() {
    // TAG_OTHER_REGS = 90.
    // Encoding: count (u32) then count*(key u32, val u32).
    let mut w = SnapshotWriter::new(
        <E1000DeviceState as IoSnapshot>::DEVICE_ID,
        <E1000DeviceState as IoSnapshot>::DEVICE_VERSION,
    );

    let mut payload = Vec::new();
    payload.extend_from_slice(&1u32.to_le_bytes());
    payload.extend_from_slice(&0x123u32.to_le_bytes()); // unaligned
    payload.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    w.field_bytes(90, payload);

    let bytes = w.finish();
    let mut state = E1000DeviceState::default();
    let err = state.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("e1000 other_regs key")
    );
}

#[test]
fn e1000_device_state_drops_out_of_range_other_regs_keys() {
    // Keep in sync with the E1000 MMIO BAR size.
    const E1000_MMIO_SIZE: u32 = 0x20_000;

    // TAG_OTHER_REGS = 90.
    // Encoding: count (u32) then count*(key u32, val u32).
    let mut w = SnapshotWriter::new(
        <E1000DeviceState as IoSnapshot>::DEVICE_ID,
        <E1000DeviceState as IoSnapshot>::DEVICE_VERSION,
    );

    let mut payload = Vec::new();
    payload.extend_from_slice(&2u32.to_le_bytes());
    payload.extend_from_slice(&0x1000u32.to_le_bytes());
    payload.extend_from_slice(&0x1111_2222u32.to_le_bytes());
    payload.extend_from_slice(&E1000_MMIO_SIZE.to_le_bytes()); // out of range (>= MMIO size)
    payload.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    w.field_bytes(90, payload);

    let bytes = w.finish();
    let mut state = E1000DeviceState::default();
    state.load_state(&bytes).unwrap();

    assert_eq!(state.other_regs, vec![(0x1000, 0x1111_2222)]);
}

#[test]
fn e1000_device_state_rejects_unaligned_pci_bar0() {
    // TAG_PCI_BAR0 = 2.
    let mut w = SnapshotWriter::new(
        <E1000DeviceState as IoSnapshot>::DEVICE_ID,
        <E1000DeviceState as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u32(2, 0xDEAD_BEEF); // BAR0 must have low 4 bits clear
    let bytes = w.finish();

    let mut state = E1000DeviceState::default();
    let err = state.load_state(&bytes).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("e1000 pci bar0"));
}

#[test]
fn e1000_device_state_rejects_invalid_pci_bar1_io_flag() {
    // TAG_PCI_BAR1 = 4.
    let mut w = SnapshotWriter::new(
        <E1000DeviceState as IoSnapshot>::DEVICE_ID,
        <E1000DeviceState as IoSnapshot>::DEVICE_VERSION,
    );
    // BAR1 is an I/O BAR; bit0 must remain set and bit1 clear.
    w.field_u32(4, 0);
    let bytes = w.finish();

    let mut state = E1000DeviceState::default();
    let err = state.load_state(&bytes).unwrap_err();
    assert_eq!(err, SnapshotError::InvalidFieldEncoding("e1000 pci bar1"));
}

#[test]
fn e1000_device_state_rejects_pci_bar0_probe_mismatch() {
    // TAG_PCI_BAR0 = 2, TAG_PCI_BAR0_PROBE = 3.
    let mut w = SnapshotWriter::new(
        <E1000DeviceState as IoSnapshot>::DEVICE_ID,
        <E1000DeviceState as IoSnapshot>::DEVICE_VERSION,
    );
    w.field_u32(2, 0xDEAD_BEE0);
    w.field_bool(3, true);
    let bytes = w.finish();

    let mut state = E1000DeviceState::default();
    let err = state.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("e1000 pci bar0_probe")
    );
}

#[test]
fn e1000_device_state_rejects_pci_bar1_probe_mismatch() {
    // TAG_PCI_BAR1 = 4, TAG_PCI_BAR1_PROBE = 5.
    let mut w = SnapshotWriter::new(
        <E1000DeviceState as IoSnapshot>::DEVICE_ID,
        <E1000DeviceState as IoSnapshot>::DEVICE_VERSION,
    );
    // Valid I/O BAR encoding but not the probe/reset value.
    w.field_u32(4, 0xC001);
    w.field_bool(5, true);
    let bytes = w.finish();

    let mut state = E1000DeviceState::default();
    let err = state.load_state(&bytes).unwrap_err();
    assert_eq!(
        err,
        SnapshotError::InvalidFieldEncoding("e1000 pci bar1_probe")
    );
}
