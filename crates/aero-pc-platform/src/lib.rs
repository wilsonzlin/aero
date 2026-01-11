#![forbid(unsafe_code)]

use aero_devices::a20_gate::A20Gate;
use aero_devices::acpi_pm::{AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, SharedAcpiPmIo};
use aero_devices::clock::ManualClock;
use aero_devices::i8042::{register_i8042, I8042Ports, SharedI8042Controller};
use aero_devices::irq::PlatformIrqLine;
use aero_devices::pci::{
    register_pci_config_ports, PciConfigPorts, PciEcamConfig, PciEcamMmio, PciIntxRouter,
    PciIntxRouterConfig, SharedPciConfigPorts,
};
use aero_devices::pic8259::register_pic8259_on_platform_interrupts;
use aero_devices::pit8254::{register_pit8254, Pit8254, SharedPit8254};
use aero_devices::reset_ctrl::{ResetCtrl, ResetKind, RESET_CTRL_PORT};
use aero_devices::rtc_cmos::{register_rtc_cmos, RtcCmos, SharedRtcCmos};
use aero_devices::{hpet, i8042};
use aero_interrupts::apic::{IOAPIC_MMIO_BASE, IOAPIC_MMIO_SIZE, LAPIC_MMIO_BASE, LAPIC_MMIO_SIZE};
use aero_platform::address_filter::AddressFilter;
use aero_platform::chipset::ChipsetState;
use aero_platform::interrupts::PlatformInterrupts;
use aero_platform::io::IoPortBus;
use aero_platform::memory::MemoryBus;
use memory::MmioHandler;
use std::cell::RefCell;
use std::rc::Rc;

/// Base physical address of the PCIe ECAM ("MMCONFIG") window.
///
/// This follows the QEMU Q35 convention (256MiB window at 0xB000_0000 covering buses 0..=255).
pub const PCIE_ECAM_BASE: u64 = 0xB000_0000;

pub const PCIE_ECAM_CONFIG: PciEcamConfig = PciEcamConfig {
    segment: 0,
    start_bus: 0,
    end_bus: 0xFF,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetEvent {
    Cpu,
    System,
}

struct IoApicMmio {
    interrupts: Rc<RefCell<PlatformInterrupts>>,
}

impl MmioHandler for IoApicMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let size = size.clamp(1, 8);
        let interrupts = self.interrupts.borrow_mut();
        let mut out = 0u64;
        for i in 0..size {
            let off = offset.wrapping_add(i as u64);
            let word_offset = off & !3;
            let shift = ((off & 3) * 8) as u32;
            let word = interrupts.ioapic_mmio_read(word_offset) as u64;
            let byte = (word >> shift) & 0xFF;
            out |= byte << (i * 8);
        }
        out
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let size = size.clamp(1, 8);
        let mut interrupts = self.interrupts.borrow_mut();

        let mut idx = 0usize;
        while idx < size {
            let off = offset.wrapping_add(idx as u64);
            let word_offset = off & !3;
            let start_in_word = (off & 3) as usize;
            let mut word = interrupts.ioapic_mmio_read(word_offset);

            for byte_idx in start_in_word..4 {
                if idx >= size {
                    break;
                }
                let off = offset.wrapping_add(idx as u64);
                if (off & !3) != word_offset {
                    break;
                }
                let byte = ((value >> (idx * 8)) & 0xFF) as u32;
                let shift = (byte_idx * 8) as u32;
                word &= !(0xFF_u32 << shift);
                word |= byte << shift;
                idx += 1;
            }

            interrupts.ioapic_mmio_write(word_offset, word);
        }
    }
}

struct LapicMmio {
    interrupts: Rc<RefCell<PlatformInterrupts>>,
}

impl MmioHandler for LapicMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let size = size.clamp(1, 8);
        let interrupts = self.interrupts.borrow();
        let mut buf = [0u8; 8];
        interrupts.lapic_mmio_read(offset, &mut buf[..size]);
        u64::from_le_bytes(buf)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let size = size.clamp(1, 8);
        let interrupts = self.interrupts.borrow();
        let bytes = value.to_le_bytes();
        interrupts.lapic_mmio_write(offset, &bytes[..size]);
    }
}

struct HpetMmio {
    hpet: Rc<RefCell<hpet::Hpet<ManualClock>>>,
    interrupts: Rc<RefCell<PlatformInterrupts>>,
}

impl MmioHandler for HpetMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let mut hpet = self.hpet.borrow_mut();
        let mut interrupts = self.interrupts.borrow_mut();
        hpet.mmio_read(offset, size, &mut *interrupts)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let mut hpet = self.hpet.borrow_mut();
        let mut interrupts = self.interrupts.borrow_mut();
        hpet.mmio_write(offset, size, value, &mut *interrupts);
    }
}

pub struct PcPlatform {
    pub chipset: ChipsetState,
    pub io: IoPortBus,
    pub memory: MemoryBus,
    pub interrupts: Rc<RefCell<PlatformInterrupts>>,

    pub pci_cfg: SharedPciConfigPorts,
    pub pci_intx: PciIntxRouter,
    pub acpi_pm: SharedAcpiPmIo,

    clock: ManualClock,
    pit: SharedPit8254,
    rtc: SharedRtcCmos<ManualClock, PlatformIrqLine>,
    hpet: Rc<RefCell<hpet::Hpet<ManualClock>>>,
    i8042: I8042Ports,

    reset_events: Rc<RefCell<Vec<ResetEvent>>>,
}

impl PcPlatform {
    pub fn new(ram_size: usize) -> Self {
        let chipset = ChipsetState::new(false);
        let filter = AddressFilter::new(chipset.a20());

        let mut io = IoPortBus::new();
        let mut memory = MemoryBus::new(filter, ram_size);

        let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));

        let clock = ManualClock::new();

        let reset_events = Rc::new(RefCell::new(Vec::new()));

        PlatformInterrupts::register_imcr_ports(&mut io, interrupts.clone());
        register_pic8259_on_platform_interrupts(&mut io, interrupts.clone());

        let pit = Rc::new(RefCell::new(Pit8254::new()));
        pit.borrow_mut()
            .connect_irq0_to_platform_interrupts(interrupts.clone());
        register_pit8254(&mut io, pit.clone());

        let rtc_irq8 = PlatformIrqLine::isa(interrupts.clone(), 8);
        let rtc = Rc::new(RefCell::new(RtcCmos::new(clock.clone(), rtc_irq8)));
        rtc.borrow_mut().set_memory_size_bytes(ram_size as u64);
        register_rtc_cmos(&mut io, rtc.clone());

        let i8042_ports = I8042Ports::new();
        i8042_ports.connect_irqs_to_platform_interrupts(interrupts.clone());
        let i8042_ctrl = i8042_ports.controller();

        {
            let reset_events = reset_events.clone();
            let sink = i8042::PlatformSystemControlSink::with_reset_sink(
                chipset.a20(),
                move |_kind| reset_events.borrow_mut().push(ResetEvent::System),
            );
            i8042_ctrl
                .borrow_mut()
                .set_system_control_sink(Box::new(sink));
        }
        register_i8042(&mut io, i8042_ctrl.clone());

        io.register(
            0x92,
            Box::new(A20Gate::with_reset_sink(
                chipset.a20(),
                {
                    let reset_events = reset_events.clone();
                    move |_kind| reset_events.borrow_mut().push(ResetEvent::System)
                },
            )),
        );

        let sci_irq = PlatformIrqLine::isa(interrupts.clone(), 9);
        let acpi_pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks(
            AcpiPmConfig::default(),
            AcpiPmCallbacks {
                sci_irq: Box::new(sci_irq),
                request_power_off: None,
            },
        )));
        aero_devices::acpi_pm::register_acpi_pm(&mut io, acpi_pm.clone());

        let pci_cfg = Rc::new(RefCell::new(PciConfigPorts::new()));
        register_pci_config_ports(&mut io, pci_cfg.clone());

        memory
            .map_mmio(
                PCIE_ECAM_BASE,
                PCIE_ECAM_CONFIG.window_size_bytes(),
                Box::new(PciEcamMmio::new(pci_cfg.clone(), PCIE_ECAM_CONFIG)),
            )
            .unwrap();

        // Register Reset Control Register after PCI config ports so it can own port 0xCF9.
        io.register(
            RESET_CTRL_PORT,
            Box::new(ResetCtrl::new({
                let reset_events = reset_events.clone();
                move |kind| {
                    let event = match kind {
                        ResetKind::Cpu => ResetEvent::Cpu,
                        ResetKind::System => ResetEvent::System,
                    };
                    reset_events.borrow_mut().push(event);
                }
            })),
        );

        let pci_intx = PciIntxRouter::new(PciIntxRouterConfig::default());

        let hpet = Rc::new(RefCell::new(hpet::Hpet::new_default(clock.clone())));

        memory
            .map_mmio(
            LAPIC_MMIO_BASE,
            LAPIC_MMIO_SIZE,
            Box::new(LapicMmio {
                interrupts: interrupts.clone(),
            }),
            )
            .unwrap();
        memory
            .map_mmio(
            IOAPIC_MMIO_BASE,
            IOAPIC_MMIO_SIZE,
            Box::new(IoApicMmio {
                interrupts: interrupts.clone(),
            }),
            )
            .unwrap();
        memory
            .map_mmio(
            hpet::HPET_MMIO_BASE,
            hpet::HPET_MMIO_SIZE,
            Box::new(HpetMmio {
                hpet: hpet.clone(),
                interrupts: interrupts.clone(),
            }),
            )
            .unwrap();

        Self {
            chipset,
            io,
            memory,
            interrupts,
            pci_cfg,
            pci_intx,
            acpi_pm,
            clock,
            pit,
            rtc,
            hpet,
            i8042: i8042_ports,
            reset_events,
        }
    }

    pub fn i8042_controller(&self) -> SharedI8042Controller {
        self.i8042.controller()
    }

    pub fn tick(&mut self, delta_ns: u64) {
        self.clock.advance_ns(delta_ns);
        self.pit.borrow_mut().advance_ns(delta_ns);
        self.rtc.borrow_mut().tick();

        // Keep the LAPIC timer deterministic: advance time only via `tick`.
        self.interrupts.borrow().tick(delta_ns);

        {
            let mut hpet = self.hpet.borrow_mut();
            let mut interrupts = self.interrupts.borrow_mut();
            hpet.poll(&mut *interrupts);
        }
    }

    pub fn take_reset_events(&mut self) -> Vec<ResetEvent> {
        std::mem::take(&mut *self.reset_events.borrow_mut())
    }
}
