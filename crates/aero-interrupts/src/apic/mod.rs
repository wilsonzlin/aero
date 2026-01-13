mod io_apic;
mod local_apic;

pub use io_apic::{IoApic, IoApicId, IOAPIC_MMIO_BASE, IOAPIC_MMIO_SIZE};
pub use local_apic::{
    DeliveryMode, DestinationShorthand, Icr, IcrNotifier, LapicInterruptSink, Level, LocalApic,
    LAPIC_MMIO_BASE, LAPIC_MMIO_SIZE,
};
