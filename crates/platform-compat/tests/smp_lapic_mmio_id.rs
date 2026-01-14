use platform::interrupts::PlatformInterrupts;

#[test]
fn per_cpu_lapic_mmio_view_exposes_correct_apic_id() {
    let ints = PlatformInterrupts::new_with_cpu_count(4);

    for cpu in 0..4usize {
        let mut buf = [0u8; 4];
        ints.lapic_mmio_read_for_cpu(cpu, 0x20, &mut buf);
        let value = u32::from_le_bytes(buf);
        assert_eq!(value >> 24, cpu as u32);
    }
}
