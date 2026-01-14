use super::SharedPlatformInterrupts;
use aero_interrupts::apic::{IoApic, LocalApic};
use memory::MmioHandler;
use std::sync::{Arc, Mutex};

enum LapicMmioBackend {
    Direct(Arc<LocalApic>),
    Platform(SharedPlatformInterrupts),
}

/// MMIO adapter for [`LocalApic`] compatible with `memory::PhysicalMemoryBus`.
pub struct LapicMmio {
    backend: LapicMmioBackend,
}

impl LapicMmio {
    pub fn new(lapic: Arc<LocalApic>) -> Self {
        Self {
            backend: LapicMmioBackend::Direct(lapic),
        }
    }

    /// Construct a LAPIC MMIO adapter that routes through a shared [`SharedPlatformInterrupts`]
    /// handle.
    ///
    /// This is intended for platform integrations where the interrupt controller complex can be
    /// reset in-place (replacing the underlying LAPIC/IOAPIC models) while MMIO mappings remain
    /// persistent.
    pub fn from_platform_interrupts(interrupts: SharedPlatformInterrupts) -> Self {
        Self {
            backend: LapicMmioBackend::Platform(interrupts),
        }
    }
}

impl MmioHandler for LapicMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let size = size.clamp(1, 8);
        let mut buf = [0u8; 8];
        match &self.backend {
            LapicMmioBackend::Direct(lapic) => lapic.mmio_read(offset, &mut buf[..size]),
            LapicMmioBackend::Platform(interrupts) => {
                interrupts
                    .borrow()
                    .lapic_mmio_read(offset, &mut buf[..size]);
            }
        }
        u64::from_le_bytes(buf)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let size = size.clamp(1, 8);
        let bytes = value.to_le_bytes();
        match &self.backend {
            LapicMmioBackend::Direct(lapic) => lapic.mmio_write(offset, &bytes[..size]),
            LapicMmioBackend::Platform(interrupts) => {
                interrupts.borrow().lapic_mmio_write(offset, &bytes[..size]);
            }
        }
    }
}

fn ioapic_read_u32_bytes<C, R>(ctx: &mut C, offset: u64, size: usize, mut read_word: R) -> u64
where
    R: FnMut(&mut C, u64) -> u32,
{
    let size = size.clamp(1, 8);
    let mut out = 0u64;
    for i in 0..size {
        let off = offset.wrapping_add(i as u64);
        let word_offset = off & !3;
        let shift = ((off & 3) * 8) as u32;
        let word = u64::from(read_word(ctx, word_offset));
        let byte = (word >> shift) & 0xFF;
        out |= byte << (i * 8);
    }
    out
}

fn ioapic_write_u32_bytes<C, R, W>(
    ctx: &mut C,
    offset: u64,
    size: usize,
    value: u64,
    mut read_word: R,
    mut write_word: W,
) where
    R: FnMut(&mut C, u64) -> u32,
    W: FnMut(&mut C, u64, u32),
{
    let size = size.clamp(1, 8);
    let bytes = value.to_le_bytes();
    let mut idx = 0usize;
    while idx < size {
        let off = offset.wrapping_add(idx as u64);
        let word_offset = off & !3;
        let start_in_word = (off & 3) as usize;
        let mut word = read_word(ctx, word_offset);

        for byte_idx in start_in_word..4 {
            if idx >= size {
                break;
            }
            let off = offset.wrapping_add(idx as u64);
            if (off & !3) != word_offset {
                break;
            }
            let byte = bytes[idx] as u32;
            let shift = (byte_idx * 8) as u32;
            word &= !(0xFF_u32 << shift);
            word |= byte << shift;
            idx += 1;
        }

        write_word(ctx, word_offset, word);
    }
}

enum IoApicMmioBackend {
    Direct(Arc<Mutex<IoApic>>),
    Platform(SharedPlatformInterrupts),
}

/// MMIO adapter for [`IoApic`] compatible with `memory::PhysicalMemoryBus`.
///
/// The IOAPIC programming model is based around 32-bit `IOREGSEL` and `IOWIN` registers.
/// This adapter therefore implements sub-32-bit accesses by performing a read-modify-write on
/// the containing 32-bit word.
pub struct IoApicMmio {
    backend: IoApicMmioBackend,
}

impl IoApicMmio {
    pub fn new(ioapic: Arc<Mutex<IoApic>>) -> Self {
        Self {
            backend: IoApicMmioBackend::Direct(ioapic),
        }
    }

    /// Construct an IOAPIC MMIO adapter that routes through a shared
    /// [`SharedPlatformInterrupts`] handle.
    ///
    /// See [`LapicMmio::from_platform_interrupts`] for motivation.
    pub fn from_platform_interrupts(interrupts: SharedPlatformInterrupts) -> Self {
        Self {
            backend: IoApicMmioBackend::Platform(interrupts),
        }
    }
}

impl MmioHandler for IoApicMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        match &self.backend {
            IoApicMmioBackend::Direct(ioapic) => {
                let mut ioapic = ioapic.lock().unwrap();
                ioapic_read_u32_bytes(&mut *ioapic, offset, size, |ioapic, word_offset| {
                    ioapic.mmio_read(word_offset, 4) as u32
                })
            }
            IoApicMmioBackend::Platform(interrupts) => {
                let mut interrupts = interrupts.borrow_mut();
                ioapic_read_u32_bytes(&mut *interrupts, offset, size, |ints, word_offset| {
                    ints.ioapic_mmio_read(word_offset)
                })
            }
        }
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        match &self.backend {
            IoApicMmioBackend::Direct(ioapic) => {
                let mut ioapic = ioapic.lock().unwrap();
                ioapic_write_u32_bytes(
                    &mut *ioapic,
                    offset,
                    size,
                    value,
                    |ioapic, word_offset| ioapic.mmio_read(word_offset, 4) as u32,
                    |ioapic, word_offset, word| {
                        ioapic.mmio_write(word_offset, 4, u64::from(word));
                    },
                );
            }
            IoApicMmioBackend::Platform(interrupts) => {
                let mut interrupts = interrupts.borrow_mut();
                ioapic_write_u32_bytes(
                    &mut *interrupts,
                    offset,
                    size,
                    value,
                    |ints, word_offset| ints.ioapic_mmio_read(word_offset),
                    |ints, word_offset, word| ints.ioapic_mmio_write(word_offset, word),
                );
            }
        }
    }
}
