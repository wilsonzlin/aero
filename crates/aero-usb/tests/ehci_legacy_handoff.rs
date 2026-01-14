use aero_usb::ehci::regs::*;
use aero_usb::ehci::EhciController;

#[test]
fn ehci_legacy_handoff_bios_to_os() {
    let mut ehci = EhciController::new();

    let hccparams = ehci.mmio_read(REG_HCCPARAMS, 4);
    let eecp = ((hccparams >> HCCPARAMS_EECP_SHIFT) & 0xff) as u64;
    assert_eq!(eecp, REG_USBLEGSUP);

    let usblegsup = ehci.mmio_read(eecp, 4);
    assert_eq!(usblegsup & 0xff, USBLEGSUP_CAPID);
    assert_ne!(
        usblegsup & USBLEGSUP_BIOS_SEM,
        0,
        "BIOS should own EHCI on reset"
    );
    assert_eq!(
        usblegsup & USBLEGSUP_OS_SEM,
        0,
        "OS should not own EHCI on reset"
    );

    // Guest requests ownership.
    ehci.mmio_write(eecp, 4, USBLEGSUP_OS_SEM);

    let usblegsup = ehci.mmio_read(eecp, 4);
    assert_ne!(usblegsup & USBLEGSUP_OS_SEM, 0);
    assert_eq!(
        usblegsup & USBLEGSUP_BIOS_SEM,
        0,
        "BIOS-owned semaphore should clear once OS-owned is set"
    );
}
