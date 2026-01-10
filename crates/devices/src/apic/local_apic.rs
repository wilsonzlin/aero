use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Minimal Local APIC model used as an interrupt sink for external interrupt controllers (e.g. IOAPIC).
///
/// At this stage we only model:
/// - external interrupt injection (vector delivery)
/// - an EOI notification hook to support level-triggered interrupts in the IOAPIC
pub struct LocalApic {
    apic_id: u8,
    pending: Mutex<VecDeque<u8>>,
    eoi_notifiers: Mutex<Vec<Arc<dyn Fn(u8) + Send + Sync>>>,
}

impl LocalApic {
    pub fn new(apic_id: u8) -> Self {
        Self {
            apic_id,
            pending: Mutex::new(VecDeque::new()),
            eoi_notifiers: Mutex::new(Vec::new()),
        }
    }

    pub fn apic_id(&self) -> u8 {
        self.apic_id
    }

    /// Fetch the next pending vector that was injected into this LAPIC.
    pub fn pop_pending(&self) -> Option<u8> {
        self.pending.lock().unwrap().pop_front()
    }

    /// Register a callback that is invoked when [`LocalApic::eoi`] is called.
    pub fn register_eoi_notifier(&self, notifier: Arc<dyn Fn(u8) + Send + Sync>) {
        self.eoi_notifiers.lock().unwrap().push(notifier);
    }

    /// Acknowledge end-of-interrupt for an in-service vector.
    ///
    /// For now, this is a pure notification mechanism used to feed back into the IOAPIC
    /// model's Remote-IRR handling for level-triggered interrupts.
    pub fn eoi(&self, vector: u8) {
        let notifiers = self.eoi_notifiers.lock().unwrap().clone();
        for notifier in notifiers {
            notifier(vector);
        }
    }
}

/// Interface used by interrupt controllers (IOAPIC, PIC, etc.) to inject interrupts into a LAPIC.
pub trait LapicInterruptSink: Send + Sync {
    fn apic_id(&self) -> u8;
    fn inject_external_interrupt(&self, vector: u8);
}

impl LapicInterruptSink for LocalApic {
    fn apic_id(&self) -> u8 {
        self.apic_id()
    }

    fn inject_external_interrupt(&self, vector: u8) {
        self.pending.lock().unwrap().push_back(vector);
    }
}
