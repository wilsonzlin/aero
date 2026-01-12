use aero_devices::irq::PlatformIrqLine;
use aero_devices_storage::ahci::AhciController;
use aero_devices_storage::ide::IdeController;
use aero_platform::interrupts::PlatformInterrupts;
use std::cell::RefCell;
use std::rc::Rc;

#[test]
fn ahci_controller_accepts_platform_irq_line() {
    // This is a compile-time + smoke-test that `aero-devices-storage` uses the shared
    // `aero_devices::irq::IrqLine` trait, so platform IRQ lines can be passed directly without any
    // adapter glue.
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    let irq = PlatformIrqLine::gsi(interrupts, 24);

    // Construction should compile and not panic.
    let mut ahci = AhciController::new(Box::new(irq), 1);

    // Touch a register to ensure the controller can call through the trait object.
    ahci.write_u32(0x04, 0);
}

#[test]
fn ide_controller_accepts_platform_irq_lines() {
    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    let irq14 = PlatformIrqLine::isa(interrupts.clone(), 14);
    let irq15 = PlatformIrqLine::isa(interrupts, 15);

    // Construction should compile and not panic.
    let mut ide = IdeController::new(Box::new(irq14), Box::new(irq15));

    // Simple IO reads/writes should compile and run with platform IRQ lines.
    let _ = ide.read_u8(0x1F0);
    ide.write_u8(0x1F0, 0);
}
