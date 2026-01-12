use std::rc::Rc;

use aero_devices::pci::{PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_machine::{Machine, MachineConfig};

#[test]
fn pc_platform_shared_devices_keep_rc_identity_across_resets() {
    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        ..Default::default()
    })
    .unwrap();

    let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
    let ptr = Rc::as_ptr(&pci_cfg);

    let vga = m.vga().expect("pc platform enabled implies VGA");
    let vga_ptr = Rc::as_ptr(&vga);

    // Mutate a PCI config-space register via the config mechanism #1 ports and verify it changes.
    //
    // We use bus0:dev0:func0 (host bridge) command register at offset 0x04. The firmware PCI
    // enumerator only touches the Interrupt Line register (0x3C), so a reset should restore this
    // value back to its default.
    {
        let mut pci = pci_cfg.borrow_mut();
        // Select offset 0x04 (command/status dword).
        pci.io_write(PCI_CFG_ADDR_PORT, 4, 0x8000_0000 | 0x04);
        // Enable I/O + memory + bus master.
        pci.io_write(PCI_CFG_DATA_PORT, 4, 0x0000_0007);

        pci.io_write(PCI_CFG_ADDR_PORT, 4, 0x8000_0000 | 0x04);
        let got = pci.io_read(PCI_CFG_DATA_PORT, 4);
        assert_eq!(got & 0xFFFF, 0x0007);
    }

    // Reset multiple times; shared device `Rc` identities must remain stable.
    m.reset();
    m.reset();

    let pci_cfg_after = m.pci_config_ports().expect("pc platform enabled");
    let ptr_after = Rc::as_ptr(&pci_cfg_after);
    assert_eq!(ptr, ptr_after, "pci_config_ports Rc identity changed across reset");

    let vga_after = m.vga().expect("pc platform enabled implies VGA");
    let vga_ptr_after = Rc::as_ptr(&vga_after);
    assert_eq!(vga_ptr, vga_ptr_after, "VGA Rc identity changed across reset");

    // Verify internal state reset happened without swapping the instance.
    {
        let mut pci = pci_cfg_after.borrow_mut();
        pci.io_write(PCI_CFG_ADDR_PORT, 4, 0x8000_0000 | 0x04);
        let got = pci.io_read(PCI_CFG_DATA_PORT, 4);
        assert_eq!(got & 0xFFFF, 0x0000);
    }
}
