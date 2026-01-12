use aero_platform::interrupts::{InterruptController, InterruptInput, PlatformInterrupts};

#[test]
fn pic_irqs_are_masked_by_default_after_platform_interrupts_new() {
    let mut ints = PlatformInterrupts::new();

    // IRQ0 is typically driven by the PIT/HPET very early in boot. Ensure it does not get
    // delivered until the guest unmasks it.
    ints.raise_irq(InterruptInput::IsaIrq(0));
    assert_eq!(ints.get_pending(), None);

    ints.pic_mut().set_masked(0, false);
    assert_eq!(ints.get_pending(), Some(0x08));
}

