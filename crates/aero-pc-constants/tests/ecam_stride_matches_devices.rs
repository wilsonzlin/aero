use aero_pc_constants::{
    PCIE_ECAM_BUS_STRIDE, PCIE_ECAM_END_BUS, PCIE_ECAM_SIZE, PCIE_ECAM_START_BUS,
};

#[test]
fn ecam_bus_stride_matches_devices_crate() {
    assert_eq!(
        PCIE_ECAM_BUS_STRIDE,
        aero_devices::pci::PCIE_ECAM_BUS_STRIDE,
        "ECAM bus stride constants in aero-pc-constants and aero-devices must remain in sync"
    );
    assert_eq!(PCIE_ECAM_BUS_STRIDE, 1 << 20);
}

#[test]
fn ecam_size_matches_bus_range_and_stride() {
    let expected_size =
        (PCIE_ECAM_END_BUS as u64 - PCIE_ECAM_START_BUS as u64 + 1) * PCIE_ECAM_BUS_STRIDE;
    assert_eq!(PCIE_ECAM_SIZE, expected_size);
}
