#![forbid(unsafe_code)]

use aero_audio::hda_pci::HdaPciDevice;
use aero_devices::a20_gate::A20Gate;
use aero_devices::acpi_pm::{AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, SharedAcpiPmIo};
use aero_devices::clock::ManualClock;
use aero_devices::i8042::{register_i8042, I8042Ports, SharedI8042Controller};
use aero_devices::irq::PlatformIrqLine;
use aero_devices::pci::{
    bios_post, register_pci_config_ports, PciBarDefinition, PciBarRange, PciBdf, PciConfigPorts,
    PciDevice, PciEcamConfig, PciEcamMmio, PciInterruptPin, PciIntxRouter, PciIntxRouterConfig,
    PciResourceAllocator, PciResourceAllocatorConfig, SharedPciConfigPorts,
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

mod cpu_core;
pub use cpu_core::{PcCpuBus, PcInterruptController};

/// Base physical address of the PCIe ECAM ("MMCONFIG") window.
///
/// This follows the QEMU Q35 convention (256MiB window at 0xB000_0000 covering buses 0..=255).
pub const PCIE_ECAM_BASE: u64 = aero_pc_constants::PCIE_ECAM_BASE;

pub const PCIE_ECAM_CONFIG: PciEcamConfig = PciEcamConfig {
    segment: aero_pc_constants::PCIE_ECAM_SEGMENT,
    start_bus: aero_pc_constants::PCIE_ECAM_START_BUS,
    end_bus: aero_pc_constants::PCIE_ECAM_END_BUS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetEvent {
    Cpu,
    System,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PcPlatformConfig {
    pub enable_hda: bool,
}

struct HdaPciConfigDevice {
    config: aero_devices::pci::PciConfigSpace,
}

impl HdaPciConfigDevice {
    fn new() -> Self {
        let mut config = aero_devices::pci::profile::HDA_ICH6.build_config_space();
        config.set_bar_definition(
            0,
            PciBarDefinition::Mmio32 {
                size: HdaPciDevice::MMIO_BAR_SIZE,
                prefetchable: false,
            },
        );
        Self { config }
    }
}

impl PciDevice for HdaPciConfigDevice {
    fn config(&self) -> &aero_devices::pci::PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut aero_devices::pci::PciConfigSpace {
        &mut self.config
    }
}

#[derive(Clone)]
struct PciMmioWindow {
    base: u64,
    pci_cfg: SharedPciConfigPorts,

    // Device handlers registered in this MMIO window.
    hda: Option<Rc<RefCell<HdaPciDevice>>>,
    hda_bdf: PciBdf,
}

impl PciMmioWindow {
    fn hda_bar0(&self) -> Option<(bool, PciBarRange)> {
        let mut pci_cfg = self.pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let cfg = bus.device_config(self.hda_bdf)?;
        let mem_enabled = (cfg.command() & 0x2) != 0;
        let bar0 = cfg.bar_range(0)?;
        Some((mem_enabled, bar0))
    }

    fn map_hda(&mut self, paddr: u64, size: usize) -> Option<(Rc<RefCell<HdaPciDevice>>, u64)> {
        let hda = self.hda.as_ref()?.clone();
        let (mem_enabled, bar0) = self.hda_bar0()?;
        if !mem_enabled || bar0.base == 0 {
            return None;
        }

        let access_end = paddr.checked_add(size as u64)?;
        let bar_end = bar0.base.saturating_add(bar0.size);
        if paddr < bar0.base || access_end > bar_end {
            return None;
        }

        Some((hda, paddr - bar0.base))
    }
}

impl MmioHandler for PciMmioWindow {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let Some(paddr) = self.base.checked_add(offset) else {
            return all_ones(size);
        };
        let Some((hda, dev_offset)) = self.map_hda(paddr, size) else {
            return all_ones(size);
        };
        let mut hda = hda.borrow_mut();
        hda.read(dev_offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        let Some(paddr) = self.base.checked_add(offset) else {
            return;
        };
        let Some((hda, dev_offset)) = self.map_hda(paddr, size) else {
            return;
        };
        let mut hda = hda.borrow_mut();
        hda.write(dev_offset, size, value);
    }
}

fn all_ones(size: usize) -> u64 {
    match size {
        0 => 0,
        1 => 0xff,
        2 => 0xffff,
        3 => 0x00ff_ffff,
        4 => 0xffff_ffff,
        5 => 0x0000_ffff_ffff,
        6 => 0x00ff_ffff_ffff,
        7 => 0x00ff_ffff_ffff_ffff,
        _ => u64::MAX,
    }
}

struct HdaDmaMemory<'a> {
    mem: RefCell<&'a mut MemoryBus>,
}

impl aero_audio::mem::MemoryAccess for HdaDmaMemory<'_> {
    fn read_physical(&self, addr: u64, buf: &mut [u8]) {
        self.mem.borrow_mut().read_physical(addr, buf);
    }

    fn write_physical(&mut self, addr: u64, buf: &[u8]) {
        self.mem.borrow_mut().write_physical(addr, buf);
    }
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

    pub hda: Option<Rc<RefCell<HdaPciDevice>>>,

    pci_allocator: PciResourceAllocator,

    clock: ManualClock,
    pit: SharedPit8254,
    rtc: SharedRtcCmos<ManualClock, PlatformIrqLine>,
    hpet: Rc<RefCell<hpet::Hpet<ManualClock>>>,
    i8042: I8042Ports,

    reset_events: Rc<RefCell<Vec<ResetEvent>>>,
}

impl PcPlatform {
    pub fn new(ram_size: usize) -> Self {
        Self::new_with_config(ram_size, PcPlatformConfig::default())
    }

    pub fn new_with_hda(ram_size: usize) -> Self {
        Self::new_with_config(ram_size, PcPlatformConfig { enable_hda: true })
    }

    pub fn new_with_config(ram_size: usize, config: PcPlatformConfig) -> Self {
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
            let sink =
                i8042::PlatformSystemControlSink::with_reset_sink(chipset.a20(), move |_kind| {
                    reset_events.borrow_mut().push(ResetEvent::System)
                });
            i8042_ctrl
                .borrow_mut()
                .set_system_control_sink(Box::new(sink));
        }
        register_i8042(&mut io, i8042_ctrl.clone());

        io.register(
            0x92,
            Box::new(A20Gate::with_reset_sink(chipset.a20(), {
                let reset_events = reset_events.clone();
                move |_kind| reset_events.borrow_mut().push(ResetEvent::System)
            })),
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
        let pci_allocator_config = PciResourceAllocatorConfig::default();
        let mut pci_allocator = PciResourceAllocator::new(pci_allocator_config.clone());

        let hda = if config.enable_hda {
            let profile = aero_devices::pci::profile::HDA_ICH6;
            let bdf = profile.bdf;

            let hda = Rc::new(RefCell::new(HdaPciDevice::new()));

            let mut dev = HdaPciConfigDevice::new();
            pci_intx.configure_device_intx(bdf, Some(PciInterruptPin::IntA), dev.config_mut());
            pci_cfg
                .borrow_mut()
                .bus_mut()
                .add_device(bdf, Box::new(dev));

            Some(hda)
        } else {
            None
        };

        {
            let mut pci_cfg = pci_cfg.borrow_mut();
            bios_post(pci_cfg.bus_mut(), &mut pci_allocator).unwrap();
        }

        // Map the PCI MMIO window used by `PciResourceAllocator` so BAR reprogramming is reflected
        // immediately without needing MMIO unmap/remap support in `MemoryBus`.
        memory
            .map_mmio(
                pci_allocator_config.mmio_base,
                pci_allocator_config.mmio_size,
                Box::new(PciMmioWindow {
                    base: pci_allocator_config.mmio_base,
                    pci_cfg: pci_cfg.clone(),
                    hda: hda.clone(),
                    hda_bdf: aero_devices::pci::profile::HDA_ICH6.bdf,
                }),
            )
            .unwrap();

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
            hda,
            pci_allocator,
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

    pub fn reset_pci(&mut self) {
        let mut pci_cfg = self.pci_cfg.borrow_mut();
        bios_post(pci_cfg.bus_mut(), &mut self.pci_allocator).unwrap();
    }

    pub fn process_hda(&mut self, output_frames: usize) {
        let Some(hda) = self.hda.as_ref() else {
            return;
        };
        let mut hda = hda.borrow_mut();
        let mut mem = HdaDmaMemory {
            mem: RefCell::new(&mut self.memory),
        };
        hda.controller_mut().process(&mut mem, output_frames);
    }

    pub fn poll_pci_intx_lines(&mut self) {
        let Some(hda) = self.hda.as_ref() else {
            return;
        };

        let level = hda.borrow().irq_level();
        let bdf = aero_devices::pci::profile::HDA_ICH6.bdf;

        self.pci_intx.set_intx_level(
            bdf,
            PciInterruptPin::IntA,
            level,
            &mut *self.interrupts.borrow_mut(),
        );
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
