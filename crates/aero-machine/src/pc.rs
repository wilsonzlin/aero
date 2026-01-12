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
use std::time::Duration;

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
use memory::{DenseMemory, GuestMemory, MapError, SparseMemory};

use crate::pci_firmware::SharedPciConfigPortsBiosAdapter;
use crate::{GuestTime, MachineError, RunExit, SharedDisk, SPARSE_RAM_THRESHOLD_BYTES};

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
    disk: SharedDisk,
    mapped_roms: HashMap<u64, usize>,

    network_backend: Option<Box<dyn NetworkBackend>>,
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

        let ram_size_bytes = cfg.ram_size_bytes;
        let ram: Box<dyn GuestMemory> = if ram_size_bytes <= SPARSE_RAM_THRESHOLD_BYTES {
            let ram = DenseMemory::new(ram_size_bytes)
                .map_err(|_| MachineError::GuestMemoryTooLarge(ram_size_bytes))?;
            Box::new(ram)
        } else {
            let ram = SparseMemory::new(ram_size_bytes)
                .map_err(|_| MachineError::GuestMemoryTooLarge(ram_size_bytes))?;
            Box::new(ram)
        };

        let platform = PcPlatform::new_with_config_and_ram(
            ram,
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
            disk: SharedDisk::from_bytes(Vec::new()).expect("empty disk is valid"),
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
        self.disk.set_bytes(bytes)?;
        Ok(())
    }

    /// Returns a cloneable handle to the machine's canonical disk backend.
    ///
    /// This is the same disk used by BIOS INT13 services. Callers that want BIOS and a storage
    /// controller (AHCI/NVMe/virtio-blk) to share the same image can attach this handle as the
    /// controller backend.
    pub fn shared_disk(&self) -> SharedDisk {
        self.disk.clone()
    }

    /// Attach a ring-buffer-backed L2 tunnel network backend.
    pub fn attach_l2_tunnel_rings<TX: FrameRing + 'static, RX: FrameRing + 'static>(
        &mut self,
        tx: TX,
        rx: RX,
    ) {
        self.network_backend = Some(Box::new(L2TunnelRingBackend::new(tx, rx)));
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
        self.network_backend = Some(backend);
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
            // Keep BIOS time-of-day / BDA tick count advancing deterministically alongside the
            // platform timers. This is required for INT 1Ah AH=00h to report a progressing time
            // source, even though the platform PIT interrupt is modeled separately.
            self.bios.advance_time(
                &mut self.bus.platform.memory,
                Duration::from_nanos(delta_ns),
            );
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
            // Allow DMA-capable devices to make forward progress even while the CPU is halted.
            //
            // Storage controllers and NICs complete work asynchronously and signal completion via
            // interrupts; those interrupts must be able to wake a HLT'd CPU.
            self.bus.platform.process_ahci();
            self.bus.platform.process_nvme();
            self.bus.platform.process_virtio_blk();
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

            let mut remaining = max_insts - executed;
            // Keep `CpuState::a20_enabled` synchronized with the chipset latch at instruction
            // boundaries.
            //
            // When A20 is disabled in real/v8086 mode, `CpuState::apply_a20` masks bit 20. Enabling
            // A20 via port I/O updates the chipset immediately, but `a20_enabled` is only synced
            // here in the outer loop. Run a single instruction per batch while A20 is disabled so
            // an enable transition is observed before the next instruction executes.
            if matches!(self.cpu.state.mode, CpuMode::Real | CpuMode::Vm86)
                && !self.cpu.state.a20_enabled
            {
                remaining = remaining.min(1);
            }
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

            if let Some(kind) = self.take_reset_kind() {
                return RunExit::ResetRequested { kind, executed };
            }

            match batch.exit {
                BatchExit::Completed => {
                    // Like `Machine::run_slice`, we may intentionally run smaller Tier-0 batches
                    // (e.g. while A20 is disabled) so we can resync `CpuState::a20_enabled` at an
                    // instruction boundary. Only treat `BatchExit::Completed` as a slice completion
                    // once we've hit the caller's instruction budget.
                    if executed >= max_insts {
                        return RunExit::Completed { executed };
                    }
                    continue;
                }
                BatchExit::Branch => continue,
                BatchExit::Halted => {
                    // After advancing timers, poll again so any newly-due timer interrupts are
                    // injected into `cpu.pending.external_interrupts`.
                    //
                    // Only poll after the batch when we are going to re-enter execution within the
                    // same `run_slice` call. This avoids acknowledging interrupts at the end of a
                    // slice boundary when the CPU will not execute another instruction until the
                    // host calls `run_slice` again.
                    //
                    // Also poll DMA-capable devices once more here so guests that kick a device and
                    // then immediately execute `HLT` can still be woken by the resulting interrupt
                    // within the same `run_slice` call.
                    self.bus.platform.process_ahci();
                    self.bus.platform.process_nvme();
                    self.bus.platform.process_virtio_blk();
                    self.bus.platform.process_ide();
                    self.poll_network();
                    if self.poll_and_queue_one_external_interrupt() {
                        continue;
                    }

                    // When halted, advance platform time so timer interrupts can wake the CPU.
                    self.idle_tick_platform_1ms();
                    self.poll_network();
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::PcMachine;
    use aero_cpu_core::state::CpuMode;
    use aero_cpu_core::state::RFLAGS_IF;
    use aero_cpu_core::CpuCore;
    use aero_devices::pci::profile::NIC_E1000_82540EM;
    use aero_devices::pci::PciInterruptPin;
    use aero_net_e1000::ICR_TXDW;
    use aero_platform::interrupts::InterruptController as PlatformInterruptController;
    use memory::MemoryBus as _;

    #[test]
    fn pc_machine_e1000_intx_is_synced_but_not_acknowledged_when_pending_event() {
        let mut pc = PcMachine::new_with_e1000(2 * 1024 * 1024, None);

        let bdf = NIC_E1000_82540EM.bdf;
        let gsi = pc
            .bus
            .platform
            .pci_intx
            .gsi_for_intx(bdf, PciInterruptPin::IntA);
        assert!(
            gsi < 16,
            "expected E1000 INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
        );
        let expected_vector = if gsi < 8 {
            0x20u8.wrapping_add(gsi as u8)
        } else {
            0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
        };

        // Configure the legacy PIC to use the standard remapped offsets and unmask the routed IRQ.
        {
            let mut ints = pc.bus.platform.interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            for irq in 0..16 {
                ints.pic_mut().set_masked(irq, true);
            }
            // If the routed GSI maps to the slave PIC, ensure cascade (IRQ2) is unmasked as well.
            ints.pic_mut().set_masked(2, false);
            if let Ok(irq) = u8::try_from(gsi) {
                if irq < 16 {
                    ints.pic_mut().set_masked(irq, false);
                }
            }
        }

        // Assert E1000 INTx level by enabling + setting a cause bit.
        let e1000 = pc.bus.platform.e1000().expect("e1000 enabled");
        {
            let mut dev = e1000.borrow_mut();
            dev.mmio_write_u32_reg(0x00D0, ICR_TXDW); // IMS
            dev.mmio_write_u32_reg(0x00C8, ICR_TXDW); // ICS
            assert!(dev.irq_level());
        }

        // Prior to syncing/polling, the INTx level should not yet be visible to the interrupt
        // controller.
        assert_eq!(
            PlatformInterruptController::get_pending(&*pc.bus.platform.interrupts.borrow()),
            None
        );

        // Ensure IF=1 so the early-return path is specifically due to the pending event.
        pc.cpu.state.set_rflags(RFLAGS_IF);
        pc.cpu.pending.raise_software_interrupt(0x80, 0);
        assert!(
            pc.cpu.pending.has_pending_event(),
            "test setup: expected a pending event to block external interrupt delivery"
        );

        // Even though a pending event blocks delivery/acknowledge, the machine must still sync PCI
        // INTx sources so the PIC sees the asserted line.
        let queued = pc.poll_and_queue_one_external_interrupt();
        assert!(
            !queued,
            "poll_and_queue_one_external_interrupt should not enqueue/ack while a pending event exists"
        );
        assert!(pc.cpu.pending.external_interrupts.is_empty());
        assert_eq!(
            PlatformInterruptController::get_pending(&*pc.bus.platform.interrupts.borrow()),
            Some(expected_vector)
        );
    }

    fn write_ivt_entry(pc: &mut PcMachine, vector: u8, ip: u16, cs: u16) {
        let base = u64::from(vector) * 4;
        pc.bus.platform.memory.write_u16(base, ip);
        pc.bus.platform.memory.write_u16(base + 2, cs);
    }

    fn init_real_mode_cpu(pc: &mut PcMachine, entry_ip: u16, rflags: u64) {
        pc.cpu = CpuCore::new(CpuMode::Real);

        for seg in [
            &mut pc.cpu.state.segments.cs,
            &mut pc.cpu.state.segments.ds,
            &mut pc.cpu.state.segments.es,
            &mut pc.cpu.state.segments.ss,
            &mut pc.cpu.state.segments.fs,
            &mut pc.cpu.state.segments.gs,
        ] {
            seg.selector = 0;
            seg.base = 0;
            seg.limit = 0xFFFF;
            seg.access = 0;
        }

        pc.cpu.state.set_stack_ptr(0x7000);
        pc.cpu.state.set_rip(u64::from(entry_ip));
        pc.cpu.state.set_rflags(rflags);
        pc.cpu.state.halted = false;
    }

    #[test]
    fn pc_machine_e1000_tx_dma_completion_wakes_hlt_in_same_slice() {
        // Like `Machine`, `PcMachine::run_slice` should poll DMA-capable devices when a Tier-0
        // batch exits via `HLT`. Otherwise a guest that kicks E1000 TX and immediately halts would
        // require a second host `run_slice` call to observe the interrupt.

        let mut pc = PcMachine::new_with_e1000(2 * 1024 * 1024, None);

        let bdf = NIC_E1000_82540EM.bdf;
        let gsi = pc
            .bus
            .platform
            .pci_intx
            .gsi_for_intx(bdf, PciInterruptPin::IntA);
        assert!(
            gsi < 16,
            "expected E1000 INTx to route to legacy PIC IRQ (<16), got gsi={gsi}"
        );
        let expected_vector = if gsi < 8 {
            0x20u8.wrapping_add(gsi as u8)
        } else {
            0x28u8.wrapping_add((gsi as u8).wrapping_sub(8))
        };

        // Configure the legacy PIC to use the standard remapped offsets and unmask the routed IRQ.
        {
            let mut ints = pc.bus.platform.interrupts.borrow_mut();
            ints.pic_mut().set_offsets(0x20, 0x28);
            // If the routed GSI maps to the slave PIC, ensure cascade (IRQ2) is unmasked as well.
            ints.pic_mut().set_masked(2, false);
            if let Ok(irq) = u8::try_from(gsi) {
                if irq < 16 {
                    ints.pic_mut().set_masked(irq, false);
                }
            }
        }

        // Resolve BAR0 MMIO and BAR1 I/O bases assigned by BIOS POST.
        let (bar0_base, bar1_base) = {
            let mut pci_cfg = pc.bus.platform.pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config(bdf)
                .expect("E1000 device missing from PCI bus");
            let bar0_base = cfg.bar_range(0).expect("missing E1000 BAR0").base;
            let bar1_base = cfg.bar_range(1).expect("missing E1000 BAR1").base;
            (bar0_base, bar1_base)
        };
        let ioaddr_port = u16::try_from(bar1_base).expect("E1000 BAR1 should fit in u16 I/O space");
        let iodata_port = ioaddr_port.wrapping_add(4);

        // Enable PCI decoding + bus mastering (required for E1000 DMA).
        {
            let mut pci_cfg = pc.bus.platform.pci_cfg.borrow_mut();
            let cfg = pci_cfg
                .bus_mut()
                .device_config_mut(bdf)
                .expect("E1000 device missing from PCI bus");
            cfg.set_command(0x7); // IO + MEM + BME
        }

        // Guest memory layout for TX descriptor ring + packet bytes.
        let tx_ring_base = 0x3000u64;
        let pkt_base = 0x4000u64;
        const MIN_L2_FRAME_LEN: usize = 14;
        let frame = vec![0x11u8; MIN_L2_FRAME_LEN];

        // Write packet bytes + legacy TX descriptor 0 (EOP|RS).
        pc.bus.platform.memory.write_physical(pkt_base, &frame);
        let mut desc = [0u8; 16];
        desc[0..8].copy_from_slice(&pkt_base.to_le_bytes());
        desc[8..10].copy_from_slice(&(frame.len() as u16).to_le_bytes());
        desc[11] = (1 << 0) | (1 << 3); // EOP|RS
        pc.bus
            .platform
            .memory
            .write_physical(tx_ring_base, &desc);

        // Program E1000 TX ring over MMIO (BAR0) and enable TXDW interrupts.
        pc.bus
            .platform
            .memory
            .write_u32(bar0_base + 0x3800, tx_ring_base as u32); // TDBAL
        pc.bus.platform.memory.write_u32(bar0_base + 0x3804, 0); // TDBAH
        pc.bus.platform.memory.write_u32(bar0_base + 0x3808, 16 * 4); // TDLEN (4 descriptors)
        pc.bus.platform.memory.write_u32(bar0_base + 0x3810, 0); // TDH
        pc.bus.platform.memory.write_u32(bar0_base + 0x3818, 0); // TDT
        pc.bus.platform.memory.write_u32(bar0_base + 0x0400, 1 << 1); // TCTL.EN
        pc.bus.platform.memory.write_u32(bar0_base + 0x00D0, ICR_TXDW); // IMS = TXDW

        // Install a real-mode ISR for the routed vector that records its execution and clears the
        // E1000 interrupt by reading ICR via BAR1.
        const HANDLER_IP: u16 = 0x1100;
        let mut handler = Vec::new();
        handler.extend_from_slice(&[0xC6, 0x06, 0x00, 0x20, 0xAA]); // mov byte ptr [0x2000], 0xAA
        handler.extend_from_slice(&[0xBA, ioaddr_port as u8, (ioaddr_port >> 8) as u8]); // mov dx, ioaddr_port
        handler.extend_from_slice(&[0x66, 0xB8]);
        handler.extend_from_slice(&0x00C0u32.to_le_bytes()); // mov eax, ICR
        handler.extend_from_slice(&[0x66, 0xEF]); // out dx, eax
        handler.extend_from_slice(&[0xBA, iodata_port as u8, (iodata_port >> 8) as u8]); // mov dx, iodata_port
        handler.extend_from_slice(&[0x66, 0xED]); // in eax, dx
        handler.push(0xCF); // iret
        pc.bus
            .platform
            .memory
            .write_physical(u64::from(HANDLER_IP), &handler);
        write_ivt_entry(&mut pc, expected_vector, HANDLER_IP, 0x0000);

        // Guest program:
        //   ; write TDT=1 via BAR1 I/O then HLT (wait for TXDW interrupt)
        //   mov dx, ioaddr_port
        //   mov eax, 0x3818 (TDT)
        //   out dx, eax
        //   mov dx, iodata_port
        //   mov eax, 1
        //   out dx, eax
        //   hlt
        //   hlt
        const ENTRY_IP: u16 = 0x1000;
        let mut code = Vec::new();
        code.extend_from_slice(&[0xBA, ioaddr_port as u8, (ioaddr_port >> 8) as u8]); // mov dx, ioaddr_port
        code.extend_from_slice(&[0x66, 0xB8]);
        code.extend_from_slice(&0x3818u32.to_le_bytes()); // mov eax, TDT
        code.extend_from_slice(&[0x66, 0xEF]); // out dx, eax
        code.extend_from_slice(&[0xBA, iodata_port as u8, (iodata_port >> 8) as u8]); // mov dx, iodata_port
        code.extend_from_slice(&[0x66, 0xB8]);
        code.extend_from_slice(&1u32.to_le_bytes()); // mov eax, 1
        code.extend_from_slice(&[0x66, 0xEF]); // out dx, eax
        code.extend_from_slice(&[0xF4, 0xF4]); // hlt; hlt
        pc.bus
            .platform
            .memory
            .write_physical(u64::from(ENTRY_IP), &code);
        pc.bus.platform.memory.write_u8(0x2000, 0);

        init_real_mode_cpu(&mut pc, ENTRY_IP, RFLAGS_IF);

        // One slice should be sufficient: guest kicks TX, halts, machine polls E1000 DMA in the
        // halted path, delivers INTx, runs ISR, and then re-halts.
        let _ = pc.run_slice(200);
        assert_eq!(pc.bus.platform.memory.read_u8(0x2000), 0xAA);
    }
}
