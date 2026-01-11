use crate::clock::{Clock, NullClock};
use std::sync::{Arc, Mutex};

pub const LAPIC_MMIO_BASE: u64 = 0xFEE0_0000;
pub const LAPIC_MMIO_SIZE: u64 = 0x1000;

const REG_ID: u64 = 0x20;
const REG_VERSION: u64 = 0x30;
const REG_TPR: u64 = 0x80;
const REG_PPR: u64 = 0xA0;
const REG_EOI: u64 = 0xB0;
const REG_SVR: u64 = 0xF0;

const REG_ISR_BASE: u64 = 0x100;
const REG_IRR_BASE: u64 = 0x200;

const REG_ICR_LOW: u64 = 0x300;
const REG_ICR_HIGH: u64 = 0x310;

const REG_LVT_TIMER: u64 = 0x320;
const REG_INITIAL_COUNT: u64 = 0x380;
const REG_CURRENT_COUNT: u64 = 0x390;
const REG_DIVIDE_CONFIG: u64 = 0x3E0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerMode {
    OneShot,
    Periodic,
}

#[derive(Debug, Clone)]
struct LapicState {
    id: u8,
    tpr: u8,
    svr: u32,
    icr_low: u32,
    icr_high: u32,
    irr: [u32; 8],
    isr: [u32; 8],
    lvt_timer: u32,
    divide_config: u32,
    initial_count: u32,
    next_timer_deadline_ns: Option<u64>,
}

impl LapicState {
    fn new(id: u8) -> Self {
        Self {
            id,
            tpr: 0,
            // Real hardware resets the spurious vector register to 0xFF (software
            // enable bit cleared). Keeping the vector non-zero helps guests that
            // read SVR before programming it.
            svr: 0xFF,
            icr_low: 0,
            icr_high: 0,
            irr: [0; 8],
            isr: [0; 8],
            // Mask timer by default.
            lvt_timer: 1 << 16,
            divide_config: 0,
            initial_count: 0,
            next_timer_deadline_ns: None,
        }
    }

    fn enabled(&self) -> bool {
        (self.svr & (1 << 8)) != 0
    }

    fn ppr(&self) -> u8 {
        let tpr = self.tpr & 0xF0;
        let isr_class = bitmap_highest_set_bit(&self.isr).map(vector_priority_class).unwrap_or(0);
        tpr.max(isr_class)
    }

    fn highest_deliverable_vector(&self, ppr: u8) -> Option<u8> {
        for vector in (0u16..=255).rev() {
            let vector = vector as u8;
            if bitmap_is_set(&self.irr, vector) && vector_priority_class(vector) > ppr {
                return Some(vector);
            }
        }
        None
    }

    fn ack(&mut self, vector: u8) -> bool {
        if !self.enabled() {
            return false;
        }
        if !bitmap_is_set(&self.irr, vector) {
            return false;
        }

        let ppr = self.ppr();
        if vector_priority_class(vector) <= ppr {
            return false;
        }

        bitmap_clear(&mut self.irr, vector);
        bitmap_set(&mut self.isr, vector);
        true
    }

    fn eoi(&mut self) -> Option<u8> {
        let vector = bitmap_highest_set_bit(&self.isr)?;
        bitmap_clear(&mut self.isr, vector);
        Some(vector)
    }

    fn inject_fixed_interrupt_inner(&mut self, vector: u8) {
        if !self.enabled() {
            return;
        }
        bitmap_set(&mut self.irr, vector);
    }

    fn timer_mode(&self) -> TimerMode {
        match (self.lvt_timer >> 17) & 0b11 {
            0b01 => TimerMode::Periodic,
            _ => TimerMode::OneShot,
        }
    }

    fn timer_masked(&self) -> bool {
        (self.lvt_timer & (1 << 16)) != 0
    }

    fn timer_vector(&self) -> u8 {
        (self.lvt_timer & 0xFF) as u8
    }

    fn timer_divisor(&self) -> u32 {
        match self.divide_config & 0x0B {
            0x0 => 2,
            0x1 => 4,
            0x2 => 8,
            0x3 => 16,
            0x8 => 32,
            0x9 => 64,
            0xA => 128,
            0xB => 1,
            _ => 2,
        }
    }

    fn timer_tick_ns(&self) -> u64 {
        self.timer_divisor() as u64
    }

    fn current_count_at(&self, now: u64) -> u32 {
        let Some(deadline) = self.next_timer_deadline_ns else {
            return 0;
        };

        if self.initial_count == 0 {
            return 0;
        }

        if now >= deadline {
            return 0;
        }

        let tick_ns = self.timer_tick_ns();
        let remaining_ns = deadline - now;
        let remaining_ticks = (remaining_ns + tick_ns - 1) / tick_ns;
        remaining_ticks.min(u64::from(u32::MAX)) as u32
    }

    fn write_divide_config(&mut self, now: u64, value: u32) {
        self.poll_timer(now);

        let remaining_ticks = self.current_count_at(now);
        self.divide_config = value;
        if remaining_ticks == 0 {
            return;
        }

        let tick_ns = self.timer_tick_ns();
        self.next_timer_deadline_ns =
            Some(now.saturating_add((remaining_ticks as u64).saturating_mul(tick_ns)));
    }

    fn write_initial_count(&mut self, now: u64, value: u32) {
        self.poll_timer(now);

        self.initial_count = value;
        if value == 0 {
            self.next_timer_deadline_ns = None;
            return;
        }

        let tick_ns = self.timer_tick_ns();
        let period_ns = (value as u64).saturating_mul(tick_ns);
        self.next_timer_deadline_ns = Some(now.saturating_add(period_ns));
    }

    fn poll_timer(&mut self, now: u64) {
        let Some(deadline) = self.next_timer_deadline_ns else {
            return;
        };

        if self.initial_count == 0 {
            self.next_timer_deadline_ns = None;
            return;
        }

        if now < deadline {
            return;
        }

        let tick_ns = self.timer_tick_ns();
        let period_ns = (self.initial_count as u64).saturating_mul(tick_ns);
        if period_ns == 0 {
            self.next_timer_deadline_ns = None;
            return;
        }

        if self.enabled() && !self.timer_masked() {
            self.inject_fixed_interrupt_inner(self.timer_vector());
        }

        match self.timer_mode() {
            TimerMode::OneShot => {
                self.next_timer_deadline_ns = None;
            }
            TimerMode::Periodic => {
                let elapsed = now - deadline;
                let periods = elapsed / period_ns + 1;
                self.next_timer_deadline_ns =
                    Some(deadline.saturating_add(periods.saturating_mul(period_ns)));
            }
        }
    }

    fn read_u32(&mut self, now: u64, offset: u64) -> u32 {
        match offset {
            REG_ID => (self.id as u32) << 24,
            REG_VERSION => 0x0006_0014,
            REG_TPR => u32::from(self.tpr),
            REG_PPR => u32::from(self.ppr()),
            REG_EOI => 0,
            REG_SVR => self.svr,
            REG_ICR_LOW => self.icr_low,
            REG_ICR_HIGH => self.icr_high,
            REG_LVT_TIMER => self.lvt_timer,
            REG_INITIAL_COUNT => self.initial_count,
            REG_CURRENT_COUNT => u32::from(self.current_count_at(now)),
            REG_DIVIDE_CONFIG => self.divide_config,
            _ if is_apic_array_register(offset, REG_ISR_BASE) => {
                let idx = apic_array_index(offset, REG_ISR_BASE);
                self.isr[idx]
            }
            _ if is_apic_array_register(offset, REG_IRR_BASE) => {
                let idx = apic_array_index(offset, REG_IRR_BASE);
                self.irr[idx]
            }
            _ => 0,
        }
    }

    fn write_u32(&mut self, now: u64, offset: u64, value: u32) -> Option<u8> {
        match offset {
            REG_ID => self.id = (value >> 24) as u8,
            REG_TPR => self.tpr = value as u8,
            REG_EOI => return self.eoi(),
            REG_SVR => self.svr = value,
            REG_ICR_LOW => self.icr_low = value,
            REG_ICR_HIGH => self.icr_high = value,
            REG_LVT_TIMER => self.lvt_timer = value,
            REG_DIVIDE_CONFIG => self.write_divide_config(now, value),
            REG_INITIAL_COUNT => self.write_initial_count(now, value),
            _ => {}
        }

        None
    }
}

/// Interface used by interrupt controllers (IOAPIC, PIC, etc.) to inject interrupts into a LAPIC.
pub trait LapicInterruptSink: Send + Sync {
    fn apic_id(&self) -> u8;
    fn inject_external_interrupt(&self, vector: u8);
}

/// Local APIC (LAPIC) model with a MMIO register page and an APIC timer.
pub struct LocalApic {
    clock: Arc<dyn Clock + Send + Sync>,
    state: Mutex<LapicState>,
    eoi_notifiers: Mutex<Vec<Arc<dyn Fn(u8) + Send + Sync>>>,
}

impl LocalApic {
    pub fn new(apic_id: u8) -> Self {
        Self::with_clock(Arc::new(NullClock), apic_id)
    }

    pub fn with_clock(clock: Arc<dyn Clock + Send + Sync>, apic_id: u8) -> Self {
        Self {
            clock,
            state: Mutex::new(LapicState::new(apic_id)),
            eoi_notifiers: Mutex::new(Vec::new()),
        }
    }

    pub fn apic_id(&self) -> u8 {
        self.state.lock().unwrap().id
    }

    pub fn enabled(&self) -> bool {
        self.state.lock().unwrap().enabled()
    }

    pub fn poll(&self) {
        let now = self.clock.now_ns();
        self.state.lock().unwrap().poll_timer(now);
    }

    /// Injects a fixed interrupt vector into the LAPIC IRR.
    ///
    /// If the LAPIC is disabled (`SVR[8] == 0`), the interrupt is dropped.
    pub fn inject_fixed_interrupt(&self, vector: u8) {
        let now = self.clock.now_ns();
        let mut state = self.state.lock().unwrap();
        state.poll_timer(now);
        state.inject_fixed_interrupt_inner(vector);
    }

    /// Returns the highest-priority deliverable interrupt vector (if any).
    ///
    /// Priority is approximated by the APIC "priority class" (`vector[7:4]`),
    /// with ties broken by choosing the numerically higher vector.
    pub fn get_pending_vector(&self) -> Option<u8> {
        let now = self.clock.now_ns();
        let mut state = self.state.lock().unwrap();
        state.poll_timer(now);

        if !state.enabled() {
            return None;
        }

        let ppr = state.ppr();
        state.highest_deliverable_vector(ppr)
    }

    pub fn is_pending(&self, vector: u8) -> bool {
        let state = self.state.lock().unwrap();
        bitmap_is_set(&state.irr, vector)
    }

    /// Acknowledge delivery of `vector`, moving it from IRR → ISR.
    pub fn ack(&self, vector: u8) -> bool {
        let now = self.clock.now_ns();
        let mut state = self.state.lock().unwrap();
        state.poll_timer(now);
        state.ack(vector)
    }

    pub fn eoi(&self) {
        let now = self.clock.now_ns();
        let vector = {
            let mut state = self.state.lock().unwrap();
            state.poll_timer(now);
            state.eoi()
        };

        if let Some(vector) = vector {
            self.notify_eoi(vector);
        }
    }

    /// Register a callback that is invoked when the guest writes the EOI register.
    pub fn register_eoi_notifier(&self, notifier: Arc<dyn Fn(u8) + Send + Sync>) {
        self.eoi_notifiers.lock().unwrap().push(notifier);
    }

    fn notify_eoi(&self, vector: u8) {
        let notifiers = self.eoi_notifiers.lock().unwrap().clone();
        for notifier in notifiers {
            notifier(vector);
        }
    }

    pub fn mmio_read(&self, offset: u64, data: &mut [u8]) {
        if data.is_empty() {
            return;
        }

        let now = self.clock.now_ns();
        let mut state = self.state.lock().unwrap();
        state.poll_timer(now);

        for (idx, byte) in data.iter_mut().enumerate() {
            let off = offset.wrapping_add(idx as u64);
            if off >= LAPIC_MMIO_SIZE {
                *byte = 0;
                continue;
            }

            let word_offset = off & !3;
            let word = state.read_u32(now, word_offset);
            let shift = ((off & 3) * 8) as u32;
            *byte = ((word >> shift) & 0xFF) as u8;
        }
    }

    pub fn mmio_write(&self, offset: u64, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        let now = self.clock.now_ns();
        let mut eoi_vectors = Vec::new();
        {
            let mut state = self.state.lock().unwrap();
            state.poll_timer(now);

            let mut idx = 0usize;
            while idx < data.len() {
                let off = offset.wrapping_add(idx as u64);
                if off >= LAPIC_MMIO_SIZE {
                    idx += 1;
                    continue;
                }

                let word_offset = off & !3;
                let start_in_word = (off & 3) as usize;
                let mut word = state.read_u32(now, word_offset);

                for byte_idx in start_in_word..4 {
                    if idx >= data.len() {
                        break;
                    }

                    let off = offset.wrapping_add(idx as u64);
                    if off >= LAPIC_MMIO_SIZE {
                        idx += 1;
                        continue;
                    }

                    if (off & !3) != word_offset {
                        break;
                    }

                    let shift = (byte_idx * 8) as u32;
                    word &= !(0xFF_u32 << shift);
                    word |= (data[idx] as u32) << shift;
                    idx += 1;
                }

                if let Some(vector) = state.write_u32(now, word_offset, word) {
                    eoi_vectors.push(vector);
                }
            }
        }

        for vector in eoi_vectors {
            self.notify_eoi(vector);
        }
    }
}

impl LapicInterruptSink for LocalApic {
    fn apic_id(&self) -> u8 {
        self.apic_id()
    }

    fn inject_external_interrupt(&self, vector: u8) {
        self.inject_fixed_interrupt(vector);
    }
}

fn is_apic_array_register(offset: u64, base: u64) -> bool {
    offset >= base && offset < base + 0x80 && (offset - base) % 0x10 == 0
}

fn apic_array_index(offset: u64, base: u64) -> usize {
    ((offset - base) / 0x10) as usize
}

fn bitmap_word_index(vector: u8) -> usize {
    (vector / 32) as usize
}

fn bitmap_bit_mask(vector: u8) -> u32 {
    1u32 << (vector % 32)
}

fn bitmap_is_set(bitmap: &[u32; 8], vector: u8) -> bool {
    (bitmap[bitmap_word_index(vector)] & bitmap_bit_mask(vector)) != 0
}

fn bitmap_set(bitmap: &mut [u32; 8], vector: u8) {
    let idx = bitmap_word_index(vector);
    bitmap[idx] |= bitmap_bit_mask(vector);
}

fn bitmap_clear(bitmap: &mut [u32; 8], vector: u8) {
    let idx = bitmap_word_index(vector);
    bitmap[idx] &= !bitmap_bit_mask(vector);
}

fn bitmap_highest_set_bit(bitmap: &[u32; 8]) -> Option<u8> {
    for (idx, word) in bitmap.iter().enumerate().rev() {
        if *word == 0 {
            continue;
        }
        let bit = 31 - word.leading_zeros();
        let vector = idx as u32 * 32 + bit;
        return Some(vector as u8);
    }
    None
}

fn vector_priority_class(vector: u8) -> u8 {
    vector & 0xF0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug, Default)]
    struct TestClock {
        now_ns: AtomicU64,
    }

    impl TestClock {
        fn advance(&self, delta_ns: u64) {
            self.now_ns.fetch_add(delta_ns, Ordering::SeqCst);
        }
    }

    impl Clock for TestClock {
        fn now_ns(&self) -> u64 {
            self.now_ns.load(Ordering::SeqCst)
        }
    }

    fn read_u32(apic: &LocalApic, offset: u64) -> u32 {
        let mut buf = [0u8; 4];
        apic.mmio_read(offset, &mut buf);
        u32::from_le_bytes(buf)
    }

    fn write_u32(apic: &LocalApic, offset: u64, value: u32) {
        apic.mmio_write(offset, &value.to_le_bytes());
    }

    #[test]
    fn mmio_smoke() {
        let clock = Arc::new(TestClock::default());
        let apic = LocalApic::with_clock(clock, 0x2);

        assert_eq!(read_u32(&apic, REG_ID), 0x02_00_00_00);
        assert_eq!(read_u32(&apic, REG_VERSION), 0x0006_0014);

        write_u32(&apic, REG_TPR, 0x70);
        assert_eq!(read_u32(&apic, REG_TPR), 0x70);

        write_u32(&apic, REG_SVR, (1 << 8) | 0xFF);
        assert_eq!(read_u32(&apic, REG_SVR), (1 << 8) | 0xFF);
    }

    #[test]
    fn interrupt_priority_and_eoi() {
        let clock = Arc::new(TestClock::default());
        let apic = LocalApic::with_clock(clock, 0);
        write_u32(&apic, REG_SVR, 1 << 8);

        apic.inject_fixed_interrupt(0x30);
        apic.inject_fixed_interrupt(0x31);

        let vec = apic.get_pending_vector().expect("pending");
        assert_eq!(vec, 0x31);
        assert!(apic.ack(vec));

        assert_eq!(apic.get_pending_vector(), None);
        apic.eoi();

        let vec = apic.get_pending_vector().expect("pending");
        assert_eq!(vec, 0x30);
        assert!(apic.ack(vec));
    }

    #[test]
    fn periodic_timer_fires() {
        let clock = Arc::new(TestClock::default());
        let apic = LocalApic::with_clock(clock.clone(), 0);

        write_u32(&apic, REG_SVR, 1 << 8);
        write_u32(&apic, REG_LVT_TIMER, 0x40 | (1 << 17));
        write_u32(&apic, REG_DIVIDE_CONFIG, 0xB);
        write_u32(&apic, REG_INITIAL_COUNT, 10);

        assert_eq!(read_u32(&apic, REG_CURRENT_COUNT), 10);

        clock.advance(9);
        apic.poll();
        assert_eq!(apic.get_pending_vector(), None);

        clock.advance(1);
        apic.poll();
        assert_eq!(apic.get_pending_vector(), Some(0x40));
        assert!(apic.ack(0x40));
        apic.eoi();

        clock.advance(10);
        apic.poll();
        assert_eq!(apic.get_pending_vector(), Some(0x40));
    }

    #[test]
    fn one_shot_timer_fires_once() {
        let clock = Arc::new(TestClock::default());
        let apic = LocalApic::with_clock(clock.clone(), 0);

        write_u32(&apic, REG_SVR, 1 << 8);
        write_u32(&apic, REG_LVT_TIMER, 0x40); // one-shot (no periodic bit)
        write_u32(&apic, REG_DIVIDE_CONFIG, 0xB);
        write_u32(&apic, REG_INITIAL_COUNT, 10);

        clock.advance(10);
        apic.poll();
        assert_eq!(apic.get_pending_vector(), Some(0x40));
        assert!(apic.ack(0x40));
        apic.eoi();

        clock.advance(10);
        apic.poll();
        assert_eq!(apic.get_pending_vector(), None);
    }
}

