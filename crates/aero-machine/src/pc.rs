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
use aero_net_backend::{FrameRing, L2TunnelRingBackend, NetworkBackend};
use aero_pc_platform::{PcCpuBus, PcPlatform, PcPlatformConfig, ResetEvent};
use aero_platform::reset::ResetKind;
use firmware::bios::{A20Gate, Bios, BiosBus, BiosConfig, FirmwareMemory};
use memory::MapError;

use crate::pci_firmware::SharedPciConfigPortsBiosAdapter;
use crate::{MachineError, RunExit, VecBlockDevice};

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
    mapped_roms: HashMap<u64, usize>,
}

impl PlatformBiosBus<'_> {
    fn map_rom_checked(&mut self, base: u64, rom: Arc<[u8]>) {
        let len = rom.len();
        match self.platform.memory.map_rom(base, rom) {
            Ok(()) => {}
            Err(MapError::Overlap) => {
                // BIOS resets may re-map the same ROM windows. Treat identical overlaps as
                // idempotent, but reject unexpected overlaps to avoid silently corrupting the bus.
                if let Some(prev_len) = self.mapped_roms.get(&base).copied() {
                    if prev_len != len {
                        panic!("unexpected ROM mapping overlap at 0x{base:016x}");
                    }
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
    assist: AssistContext,
    pub bus: PcCpuBus,
    bios: Bios,
    disk: VecBlockDevice,

    network_backend: Option<Box<dyn NetworkBackend>>,
    e1000_mac_addr: Option<[u8; 6]>,
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
            assist: AssistContext::default(),
            bus: PcCpuBus::new(platform),
            bios: Bios::new(BiosConfig::default()),
            disk: VecBlockDevice::new(Vec::new()).expect("empty disk is valid"),
            network_backend: None,
            e1000_mac_addr,
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

    /// Reset the machine and transfer control to firmware POST (boot sector).
    pub fn reset(&mut self) {
        let ram_size = usize::try_from(self.cfg.ram_size_bytes)
            .expect("ram_size_bytes already validated in PcMachine::new");

        // Rebuild the full platform for deterministic power-on state.
        self.bus = PcCpuBus::new(PcPlatform::new_with_config(
            ram_size,
            PcPlatformConfig {
                enable_hda: self.cfg.enable_hda,
                enable_e1000: self.cfg.enable_e1000,
                mac_addr: self.e1000_mac_addr,
                ..Default::default()
            },
        ));

        self.assist = AssistContext::default();
        self.cpu = CpuCore::new(CpuMode::Real);

        self.bios = Bios::new(BiosConfig {
            memory_size_bytes: self.cfg.ram_size_bytes,
            cpu_count: self.cfg.cpu_count,
            ..Default::default()
        });

        let mut pci_adapter =
            SharedPciConfigPortsBiosAdapter::new(self.bus.platform.pci_cfg.clone());
        let mut bus = PlatformBiosBus {
            platform: &mut self.bus.platform,
            mapped_roms: HashMap::new(),
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

    fn poll_and_queue_one_external_interrupt(&mut self) {
        // Avoid unbounded growth of the external interrupt FIFO if the guest has IF=0, interrupts
        // are inhibited, etc. Also avoids tight polling loops when a level-triggered interrupt
        // line stays asserted.
        const MAX_QUEUED_EXTERNAL_INTERRUPTS: usize = 1;
        if self.cpu.pending.external_interrupts.len() >= MAX_QUEUED_EXTERNAL_INTERRUPTS
            || self.cpu.pending.has_pending_event()
            || (self.cpu.state.rflags() & RFLAGS_IF) == 0
            || self.cpu.pending.interrupt_inhibit() != 0
        {
            return;
        }

        let mut ctrl = self.bus.interrupt_controller();
        if let Some(vector) = ctrl.poll_interrupt() {
            self.cpu.pending.inject_external_interrupt(vector);
        }
    }

    /// Poll the E1000 + network backend bridge once (DMA + TX/RX frame pumping).
    ///
    /// Returns quickly when E1000 is disabled/not present, or when no backend is attached.
    pub fn poll_network(&mut self) {
        if !self.bus.platform.has_e1000() {
            return;
        }

        // Avoid draining TX frames unless we can actually deliver them to a backend.
        let Some(mut backend) = self.network_backend.take() else {
            return;
        };

        // Run DMA once before draining/pushing frames so register-only writes (like updating
        // TDT/RDT) can take effect.
        self.bus.platform.process_e1000();

        // 2) Drain guest->host frames.
        const MAX_TX_FRAMES_PER_POLL: usize = 256;
        let mut tx_budget = MAX_TX_FRAMES_PER_POLL;
        while tx_budget != 0 {
            let Some(frame) = self.bus.platform.e1000_pop_tx_frame() else {
                break;
            };
            tx_budget -= 1;
            backend.transmit(frame);
        }
 
        // 3) Drain host->guest frames.
        const MAX_RX_FRAMES_PER_POLL: usize = 256;
        let mut rx_budget = MAX_RX_FRAMES_PER_POLL;
        while rx_budget != 0 {
            let Some(frame) = backend.poll_receive() else {
                break;
            };
            rx_budget -= 1;
            self.bus.platform.e1000_enqueue_rx_frame(frame);
        }
 
        // 4) Flush RX delivery for newly enqueued frames.
        self.bus.platform.process_e1000();
        self.network_backend = Some(backend);
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

            self.poll_network();
            self.bus.platform.poll_pci_intx_lines();

            if let Some(kind) = self.take_reset_kind() {
                return RunExit::ResetRequested { kind, executed };
            }

            // Keep the core's A20 view coherent with the chipset latch.
            self.cpu.state.a20_enabled = self.bus.platform.chipset.a20().enabled();

            // Poll the platform interrupt controller (PIC/IOAPIC+LAPIC) and inject at most one
            // vector into the CPU's external interrupt FIFO.
            self.poll_and_queue_one_external_interrupt();

            let remaining = max_insts - executed;
            let batch = run_batch_cpu_core_with_assists(
                &cfg,
                &mut self.assist,
                &mut self.cpu,
                &mut self.bus,
                remaining,
            );
            executed = executed.saturating_add(batch.executed);

            match batch.exit {
                BatchExit::Completed => return RunExit::Completed { executed },
                BatchExit::Branch => continue,
                BatchExit::Halted => return RunExit::Halted { executed },
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

        let mut bus = PlatformBiosBus {
            platform: &mut self.bus.platform,
            mapped_roms: HashMap::new(),
        };
        let bios_bus: &mut dyn BiosBus = &mut bus;
        self.bios
            .dispatch_interrupt(vector, &mut self.cpu.state, bios_bus, &mut self.disk);

        self.cpu.state.a20_enabled = self.bus.platform.chipset.a20().enabled();
    }
}
