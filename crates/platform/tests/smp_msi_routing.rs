use aero_platform::interrupts::{
    MsiMessage, MsiTrigger, PlatformInterruptMode, PlatformInterrupts,
};

#[test]
fn msi_destination_routes_to_non_bsp_lapic() {
    let mut ints = PlatformInterrupts::new_with_cpu_count(2);
    ints.set_mode(PlatformInterruptMode::Apic);

    ints.trigger_msi(MsiMessage {
        address: 0xFEE0_0000 | (1u64 << 12),
        data: 0x0051,
    });

    assert_eq!(ints.lapic(1).get_pending_vector(), Some(0x51));
    assert_eq!(ints.lapic(0).get_pending_vector(), None);
}

#[test]
fn msi_destination_ff_broadcasts_to_all_lapics() {
    let mut ints = PlatformInterrupts::new_with_cpu_count(2);
    ints.set_mode(PlatformInterruptMode::Apic);

    ints.trigger_msi(MsiMessage {
        address: 0xFEE0_0000 | (0xFFu64 << 12),
        data: 0x0053,
    });

    assert_eq!(ints.lapic(0).get_pending_vector(), Some(0x53));
    assert_eq!(ints.lapic(1).get_pending_vector(), Some(0x53));
}
