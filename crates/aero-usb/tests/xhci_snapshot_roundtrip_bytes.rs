use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader};
use aero_usb::xhci::{regs, XhciController};

mod util;
use util::TestMemory;

#[test]
fn xhci_snapshot_roundtrips_byte_for_byte() {
    let mut mem = TestMemory::new(0x10_000);
    let mut ctrl = XhciController::new();

    // Mutate a register and advance time so the snapshot is not all-default.
    ctrl.mmio_write(&mut mem, regs::REG_DNCTRL, 4, 0x1234_5678);
    ctrl.tick_1ms_no_dma();

    let snap1 = ctrl.save_state();

    // Mutate again so restore has something to do.
    ctrl.mmio_write(&mut mem, regs::REG_DNCTRL, 4, 0xDEAD_BEEF);

    ctrl.load_state(&snap1).expect("load_state");

    let snap2 = ctrl.save_state();
    if snap1 != snap2 {
        let r1 = SnapshotReader::parse(&snap1, *b"XHCI").unwrap();
        let r2 = SnapshotReader::parse(&snap2, *b"XHCI").unwrap();
        for (tag, bytes1) in r1.iter_fields() {
            match r2.bytes(tag) {
                None => eprintln!("tag {tag} missing after restore"),
                Some(bytes2) => {
                    if bytes1 != bytes2 {
                        eprintln!(
                            "tag {tag} differs: len1={} len2={} first1={:?} first2={:?}",
                            bytes1.len(),
                            bytes2.len(),
                            &bytes1[..bytes1.len().min(16)],
                            &bytes2[..bytes2.len().min(16)]
                        );
                    }
                }
            }
        }
        for (tag, _) in r2.iter_fields() {
            if r1.bytes(tag).is_none() {
                eprintln!("tag {tag} present only after restore");
            }
        }
    }
    assert_eq!(snap1, snap2, "xhci snapshot should roundtrip byte-for-byte");
}
