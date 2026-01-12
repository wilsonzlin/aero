//! PC machine integration built on [`aero_pc_platform::PcPlatform`].
//!
//! This module exists primarily for integration tests and experiments that need:
//! - a PCI-capable platform (MMIO, port I/O, PCI config ports, INTx routing),
//! - BIOS POST with PCI enumeration, and
//! - optional E1000 + ring-backed networking.
//!
#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::sync::Arc;

use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_cpu_core_with_assists, BatchExit};
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::interrupts::InterruptController as _;
use aero_cpu_core::state::{CpuMode, CpuState, RFLAGS_IF};
use aero_cpu_core::CpuCore;
use aero_net_backend::{FrameRing, L2TunnelRingBackend, L2TunnelRingBackendStats, NetworkBackend};
use aero_net_pump::tick_e1000;
use aero_pc_platform::{PcCpuBus, PcPlatform, PcPlatformConfig, ResetEvent};
use aero_platform::reset::ResetKind;
use firmware::bios::{A20Gate, Bios, BiosBus, BiosConfig, FirmwareMemory};
use memory::MapError;

use crate::pci_firmware::SharedPciConfigPortsBiosAdapter;
use crate::{GuestTime, MachineError, RunExit, VecBlockDevice};

/// Configuration for [`PcMachine`].
#[derive(Debug, Clone)]
pub struct PcMachineConfig {
    /// Guest RAM size in bytes.
    pub ram_size_bytes: u64,
    /// Number of vCPUs (currently must be 1).
    pub cpu_count: u8,

    pub enable_hda: bool,
    pub enable_e1000: bool,
}

impl Default for PcMachineConfig {
    fn default() -> Self {
        Self {
            ram_size_bytes: 64 * 1024 * 1024,
            cpu_count: 1,
            enable_hda: false,
            enable_e1000: true,
        }
    }
}

struct PlatformBiosBus<'a> {
    platform: &'a mut PcPlatform,
    mapped_roms: &'a mut HashMap<u64, usize>,
}

impl PlatformBiosBus<'_> {
    fn map_rom_checked(&mut self, base: u64, rom: Arc<[u8]>) {
        let len = rom.len();
        match self.platform.memory.map_rom(base, rom) {
            Ok(()) => {}
            Err(MapError::Overlap) => {
                // BIOS resets may re-map the same ROM windows. Treat identical overlaps as
                // idempotent, but reject unexpected overlaps to avoid silently corrupting the bus.
                if self.mapped_roms.get(&base).copied() != Some(len) {
                    panic!("unexpected ROM mapping overlap at 0x{base:016x}");
                }
            }
            Err(MapError::AddressOverflow) => {
                panic!("ROM mapping overflow at 0x{base:016x} (len=0x{len:x})")
            }
        }

        self.mapped_roms.insert(base, len);
    }
}

impl memory::MemoryBus for PlatformBiosBus<'_> {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        self.platform.memory.read_physical(paddr, buf);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.platform.memory.write_physical(paddr, buf);
    }
}

impl FirmwareMemory for PlatformBiosBus<'_> {
    fn map_rom(&mut self, base: u64, rom: Arc<[u8]>) {
        self.map_rom_checked(base, rom);
    }
}

impl A20Gate for PlatformBiosBus<'_> {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.platform.chipset.a20().set_enabled(enabled);
    }

    fn a20_enabled(&self) -> bool {
        self.platform.chipset.a20().enabled()
    }
}

/// PCI-capable PC machine: CPU + [`PcPlatform`] + BIOS + optional E1000 + network backend.
pub struct PcMachine {
    cfg: PcMachineConfig,

    pub cpu: CpuCore,
    guest_time: GuestTime,
    assist: AssistContext,
    pub bus: PcCpuBus,
    bios: Bios,
    disk: VecBlockDevice,
    mapped_roms: HashMap<u64, usize>,

    network_backend: Option<PcNetworkBackend>,
}

type DynFrameRing = Box<dyn FrameRing>;
type DynRingBackend = L2TunnelRingBackend<DynFrameRing, DynFrameRing>;

enum PcNetworkBackend {
    Ring(DynRingBackend),
    Other(Box<dyn NetworkBackend>),
}

impl NetworkBackend for PcNetworkBackend {
    fn transmit(&mut self, frame: Vec<u8>) {
        match self {
            PcNetworkBackend::Ring(backend) => backend.transmit(frame),
            PcNetworkBackend::Other(backend) => backend.transmit(frame),
        }
    }

    fn l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        match self {
            PcNetworkBackend::Ring(backend) => backend.l2_ring_stats(),
            PcNetworkBackend::Other(backend) => backend.l2_ring_stats(),
        }
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        match self {
            PcNetworkBackend::Ring(backend) => backend.poll_receive(),
            PcNetworkBackend::Other(backend) => backend.poll_receive(),
        }
    }
}

impl PcMachine {
    /// Construct a new PC machine with `ram_size_bytes` of guest RAM.
    ///
    /// This mirrors the original `PcMachine` API used by integration tests.
    pub fn new(ram_size_bytes: usize) -> Self {
        Self::new_with_config(PcMachineConfig {
            ram_size_bytes: ram_size_bytes as u64,
            cpu_count: 1,
            enable_hda: false,
            enable_e1000: false,
        })
        .expect("PcMachineConfig derived from `ram_size_bytes` should be valid")
    }

    /// Construct a new PC machine with the E1000 PCI NIC enabled.
    ///
    /// This is a convenience wrapper for deterministic native/WASM integration tests that only
    /// need the PCI-capable platform + E1000 + a network backend bridge.
    pub fn new_with_e1000(ram_size_bytes: usize, mac: Option<[u8; 6]>) -> Self {
        Self::new_with_config_and_mac(
            PcMachineConfig {
                ram_size_bytes: ram_size_bytes as u64,
                cpu_count: 1,
                enable_hda: false,
                enable_e1000: true,
            },
            mac,
        )
        .expect("PcMachineConfig derived from `ram_size_bytes` should be valid")
    }

    pub fn new_with_config(cfg: PcMachineConfig) -> Result<Self, MachineError> {
        Self::new_with_config_and_mac(cfg, None)
    }

    fn new_with_config_and_mac(
        cfg: PcMachineConfig,
        e1000_mac_addr: Option<[u8; 6]>,
    ) -> Result<Self, MachineError> {
        if cfg.cpu_count != 1 {
            return Err(MachineError::InvalidCpuCount(cfg.cpu_count));
        }

        let ram_size = usize::try_from(cfg.ram_size_bytes)
            .map_err(|_| MachineError::GuestMemoryTooLarge(cfg.ram_size_bytes))?;

        let platform = PcPlatform::new_with_config(
            ram_size,
            PcPlatformConfig {
                enable_hda: cfg.enable_hda,
                enable_e1000: cfg.enable_e1000,
                mac_addr: e1000_mac_addr,
                ..Default::default()
            },
        );

        let mut machine = Self {
            cfg,
            cpu: CpuCore::new(CpuMode::Real),
            guest_time: GuestTime::default(),
            assist: AssistContext::default(),
            bus: PcCpuBus::new(platform),
            bios: Bios::new(BiosConfig::default()),
            disk: VecBlockDevice::new(Vec::new()).expect("empty disk is valid"),
            mapped_roms: HashMap::new(),
            network_backend: None,
        };

        machine.reset();
        Ok(machine)
    }

    pub fn cpu(&self) -> &CpuState {
        &self.cpu.state
    }

    pub fn cpu_mut(&mut self) -> &mut CpuState {
        &mut self.cpu.state
    }

    pub fn platform(&self) -> &PcPlatform {
        &self.bus.platform
    }

    pub fn platform_mut(&mut self) -> &mut PcPlatform {
        &mut self.bus.platform
    }

    pub fn set_disk_image(&mut self, bytes: Vec<u8>) -> Result<(), MachineError> {
        self.disk = VecBlockDevice::new(bytes)?;
        Ok(())
    }

    /// Attach a ring-buffer-backed L2 tunnel network backend.
    pub fn attach_l2_tunnel_rings<TX: FrameRing + 'static, RX: FrameRing + 'static>(
        &mut self,
        tx: TX,
        rx: RX,
    ) {
        let tx: DynFrameRing = Box::new(tx);
        let rx: DynFrameRing = Box::new(rx);
        self.network_backend = Some(PcNetworkBackend::Ring(L2TunnelRingBackend::new(tx, rx)));
    }

    /// Convenience for native callers using [`aero_ipc::ring::RingBuffer`].
    #[cfg(not(target_arch = "wasm32"))]
    pub fn attach_l2_tunnel_rings_native(
        &mut self,
        tx: aero_ipc::ring::RingBuffer,
        rx: aero_ipc::ring::RingBuffer,
    ) {
        self.attach_l2_tunnel_rings(tx, rx);
    }

    /// Convenience for WASM/browser callers using [`aero_ipc::wasm::SharedRingBuffer`].
    #[cfg(target_arch = "wasm32")]
    pub fn attach_l2_tunnel_rings_wasm(
        &mut self,
        tx: aero_ipc::wasm::SharedRingBuffer,
        rx: aero_ipc::wasm::SharedRingBuffer,
    ) {
        self.attach_l2_tunnel_rings(tx, rx);
    }

    /// Install/replace the host-side network backend used by any emulated NICs (currently E1000).
    pub fn set_network_backend(&mut self, backend: Box<dyn NetworkBackend>) {
        self.network_backend = Some(PcNetworkBackend::Other(backend));
    }

    /// Detach (drop) any currently installed network backend.
    pub fn detach_network(&mut self) {
        self.network_backend = None;
    }

    /// Return statistics for the currently attached `NET_TX`/`NET_RX` ring backend (if present).
    pub fn network_backend_l2_ring_stats(&self) -> Option<L2TunnelRingBackendStats> {
        self.network_backend.as_ref()?.l2_ring_stats()
    }

    /// Reset the machine and transfer control to firmware POST (boot sector).
    pub fn reset(&mut self) {
        // Reset the platform in-place so host-provided device backends (e.g. disks/ISOs) remain
        // attached across reboots.
        self.bus.platform.reset();
        // Flush CPU-bus state (MMU caches, CPL tracking, etc).
        self.bus.reset();

        self.assist = AssistContext::default();
        self.cpu = CpuCore::new(CpuMode::Real);
        self.guest_time = GuestTime::new_from_cpu(&self.cpu);

        self.bios = Bios::new(BiosConfig {
            memory_size_bytes: self.cfg.ram_size_bytes,
            cpu_count: self.cfg.cpu_count,
            ..Default::default()
        });

        let mut pci_adapter =
            SharedPciConfigPortsBiosAdapter::new(self.bus.platform.pci_cfg.clone());

        let platform = &mut self.bus.platform;
        let mapped_roms = &mut self.mapped_roms;
        let mut bus = PlatformBiosBus {
            platform,
            mapped_roms,
        };
        let bios_bus: &mut dyn BiosBus = &mut bus;
        self.bios.post_with_pci(
            &mut self.cpu.state,
            bios_bus,
            &mut self.disk,
            Some(&mut pci_adapter),
        );

        // Keep the core's A20 view coherent with the chipset latch.
        self.cpu.state.a20_enabled = self.bus.platform.chipset.a20().enabled();
    }

    fn take_reset_kind(&mut self) -> Option<ResetKind> {
        // Preserve ordering, but only surface a single event per slice.
        self.bus
            .platform
            .take_reset_events()
            .into_iter()
            .next()
            .map(|ev| match ev {
                ResetEvent::Cpu => ResetKind::Cpu,
                ResetEvent::System => ResetKind::System,
            })
    }

    fn poll_and_queue_one_external_interrupt(&mut self) -> bool {
        // Synchronize PCI INTx sources (e.g. E1000) into the platform interrupt controller before
        // polling/acknowledging a pending vector.
        //
        // This must happen even when the guest cannot currently accept maskable interrupts (IF=0 /
        // interrupt shadow), and even when our external-interrupt FIFO is at capacity, so
        // level-triggered lines remain accurately asserted/deasserted until delivery is possible.
        self.bus.platform.poll_pci_intx_lines();

        // Avoid unbounded growth of the external interrupt FIFO if the guest has IF=0, interrupts
        // are inhibited, etc. Also avoids tight polling loops when a level-triggered interrupt
        // line stays asserted.
        const MAX_QUEUED_EXTERNAL_INTERRUPTS: usize = 1;
        if self.cpu.pending.external_interrupts.len() >= MAX_QUEUED_EXTERNAL_INTERRUPTS
            || self.cpu.pending.has_pending_event()
            || (self.cpu.state.rflags() & RFLAGS_IF) == 0
            || self.cpu.pending.interrupt_inhibit() != 0
        {
            return false;
        }

        let mut ctrl = self.bus.interrupt_controller();
        if let Some(vector) = ctrl.poll_interrupt() {
            self.cpu.pending.inject_external_interrupt(vector);
            return true;
        }
        false
    }

    fn tick_platform_from_cycles(&mut self, cycles: u64) {
        if cycles == 0 {
            return;
        }

        let tsc_hz = self.cpu.time.tsc_hz();
        if tsc_hz == 0 {
            return;
        }

        if self.guest_time.cpu_hz() != tsc_hz {
            // If the caller changes the deterministic TSC frequency, preserve continuity by
            // resynchronizing the fractional remainder from the pre-batch TSC value.
            let tsc_before = self.cpu.state.msr.tsc.wrapping_sub(cycles);
            self.guest_time = GuestTime::new(tsc_hz);
            self.guest_time.resync_from_tsc(tsc_before);
        }

        let delta_ns = self.guest_time.advance_guest_time_for_instructions(cycles);
        if delta_ns != 0 {
            self.bus.platform.tick(delta_ns);
        }
    }

    fn idle_tick_platform_1ms(&mut self) {
        // Only tick while halted when maskable interrupts are enabled; otherwise HLT is expected to
        // be terminal (until NMI/SMI/reset, which we do not model here).
        if (self.cpu.state.rflags() & RFLAGS_IF) == 0 {
            return;
        }

        let tsc_hz = self.cpu.time.tsc_hz();
        if tsc_hz == 0 {
            return;
        }

        // Advance 1ms worth of CPU cycles while halted so PIT/RTC/HPET/LAPIC timers can wake the CPU.
        let cycles = (tsc_hz / 1000).max(1);
        self.cpu.time.advance_cycles(cycles);
        self.cpu.state.msr.tsc = self.cpu.time.read_tsc();
        self.tick_platform_from_cycles(cycles);
    }

    /// Poll the E1000 + network backend bridge once (DMA + TX/RX frame pumping).
    ///
    /// Returns quickly when E1000 is disabled/not present.
    ///
    /// When no backend is attached, guest TX frames are still processed (DMA + descriptor
    /// completion), but are dropped on the floor.
    pub fn poll_network(&mut self) {
        let Some(e1000) = self.bus.platform.e1000() else {
            return;
        };

        // Budgets for pumping guest ↔ host frames per emulation slice.
        const MAX_TX_FRAMES_PER_POLL: usize = aero_net_pump::DEFAULT_MAX_FRAMES_PER_POLL;
        const MAX_RX_FRAMES_PER_POLL: usize = aero_net_pump::DEFAULT_MAX_FRAMES_PER_POLL;

        // Keep the device model's internal PCI config image in sync with the platform PCI config
        // space. The E1000 model gates DMA on COMMAND.BME (bit 2) by consulting its own PCI config
        // state.
        let bdf = aero_devices::pci::profile::NIC_E1000_82540EM.bdf;
        let (command, bar0_base, bar1_base) = {
            let mut pci_cfg = self.bus.platform.pci_cfg.borrow_mut();
            let cfg = pci_cfg.bus_mut().device_config(bdf);
            let command = cfg.map(|cfg| cfg.command()).unwrap_or(0);
            let bar0_base = cfg
                .and_then(|cfg| cfg.bar_range(0))
                .map(|range| range.base)
                .unwrap_or(0);
            let bar1_base = cfg
                .and_then(|cfg| cfg.bar_range(1))
                .map(|range| range.base)
                .unwrap_or(0);
            (command, bar0_base, bar1_base)
        };

        let mut dev = e1000.borrow_mut();
        dev.pci_config_write(0x04, 2, u32::from(command));
        if let Ok(bar0_base) = u32::try_from(bar0_base) {
            if bar0_base != 0 {
                dev.pci_config_write(0x10, 4, bar0_base);
            }
        }
        if let Ok(bar1_base) = u32::try_from(bar1_base) {
            if bar1_base != 0 {
                dev.pci_config_write(0x14, 4, bar1_base);
            }
        }

        // Pump:
        // 1) DMA poll
        // 2) drain guest TX -> backend (or drop when no backend is installed)
        // 3) drain backend RX -> guest
        // 4) DMA poll to flush RX into guest buffers
        tick_e1000(
            &mut dev,
            &mut self.bus.platform.memory,
            &mut self.network_backend,
            MAX_TX_FRAMES_PER_POLL,
            MAX_RX_FRAMES_PER_POLL,
        );
    }

    /// Run the CPU for at most `max_insts` guest instructions.
    pub fn run_slice(&mut self, max_insts: u64) -> RunExit {
        let mut executed = 0u64;
        let cfg = Tier0Config::from_cpuid(&self.assist.features);

        while executed < max_insts {
            // Allow storage controllers to make forward progress even while the CPU is halted.
            //
            // AHCI/IDE complete DMA asynchronously and signal completion via interrupts; those
            // interrupts must be able to wake a HLT'd CPU.
            self.bus.platform.process_ahci();
            self.bus.platform.process_ide();

            // Ordering note: `poll_network()` runs E1000 DMA (TX/RX descriptor processing), which
            // may assert PCI INTx lines (e.g. TXDW). We must poll/latch PCI INTx *after* DMA so the
            // interrupt can be delivered within the same `run_slice` call.
            self.poll_network();

            if let Some(kind) = self.take_reset_kind() {
                return RunExit::ResetRequested { kind, executed };
            }

            // Keep the core's A20 view coherent with the chipset latch.
            self.cpu.state.a20_enabled = self.bus.platform.chipset.a20().enabled();

            // Poll the platform interrupt controller (PIC/IOAPIC+LAPIC) and inject at most one
            // vector into the CPU's external interrupt FIFO.
            let _ = self.poll_and_queue_one_external_interrupt();

            let remaining = max_insts - executed;
            let batch = run_batch_cpu_core_with_assists(
                &cfg,
                &mut self.assist,
                &mut self.cpu,
                &mut self.bus,
                remaining,
            );
            executed = executed.saturating_add(batch.executed);

            // Deterministically advance platform time based on executed CPU cycles.
            self.tick_platform_from_cycles(batch.executed);

            match batch.exit {
                BatchExit::Completed => return RunExit::Completed { executed },
                BatchExit::Branch => continue,
                BatchExit::Halted => {
                    // After advancing timers, poll again so any newly-due timer interrupts are
                    // injected into `cpu.pending.external_interrupts`.
                    //
                    // Only poll after the batch when we are going to re-enter execution within the
                    // same `run_slice` call. This avoids acknowledging interrupts at the end of a
                    // slice boundary when the CPU will not execute another instruction until the
                    // host calls `run_slice` again.
                    if self.poll_and_queue_one_external_interrupt() {
                        continue;
                    }

                    // When halted, advance platform time so timer interrupts can wake the CPU.
                    self.idle_tick_platform_1ms();
                    if self.poll_and_queue_one_external_interrupt() {
                        continue;
                    }
                    return RunExit::Halted { executed };
                }
                BatchExit::BiosInterrupt(vector) => {
                    self.handle_bios_interrupt(vector);
                }
                BatchExit::Assist(reason) => return RunExit::Assist { reason, executed },
                BatchExit::Exception(exception) => {
                    return RunExit::Exception {
                        exception,
                        executed,
                    }
                }
                BatchExit::CpuExit(exit) => return RunExit::CpuExit { exit, executed },
            }
        }

        RunExit::Completed { executed }
    }

    fn handle_bios_interrupt(&mut self, vector: u8) {
        self.cpu.state.a20_enabled = self.bus.platform.chipset.a20().enabled();

        let platform = &mut self.bus.platform;
        let mapped_roms = &mut self.mapped_roms;
        let mut bus = PlatformBiosBus {
            platform,
            mapped_roms,
        };
        let bios_bus: &mut dyn BiosBus = &mut bus;
        self.bios
            .dispatch_interrupt(vector, &mut self.cpu.state, bios_bus, &mut self.disk);

        self.cpu.state.a20_enabled = self.bus.platform.chipset.a20().enabled();
    }
}
