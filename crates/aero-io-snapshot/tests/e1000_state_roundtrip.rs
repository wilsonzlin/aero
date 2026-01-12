use aero_io_snapshot::io::net::state::{E1000DeviceState, E1000TxContextState, E1000TxPacketState};
use aero_io_snapshot::io::state::IoSnapshot;

#[test]
fn e1000_device_state_roundtrip_is_lossless() {
    let mut state = E1000DeviceState::default();

    state.pci_regs[0..4].copy_from_slice(&0x100E_8086u32.to_le_bytes());
    state.pci_bar0 = 0xDEAD_BEE0;
    state.pci_bar0_probe = false;
    state.pci_bar1 = 0xC001;
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
