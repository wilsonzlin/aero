//! Canonical full-system "machine" integration layer for Aero.
//!
//! This crate composes the canonical CPU core (`aero_cpu_core`), firmware (`firmware::bios`),
//! physical memory bus (`memory`), and device models (`aero-devices` / `aero-platform`) into a
//! single VM-like interface that is usable from both:
//! - native Rust integration tests, and
//! - `wasm32` builds via `crates/aero-wasm`.
//!
//! The intention is to make "which machine runs in the browser?" an explicit, stable answer:
//! **`aero_machine::Machine`**.
#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::fmt;
use std::io::{Cursor, Read, Seek, Write};
use std::rc::Rc;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_cpu_core_with_assists, BatchExit};
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::interrupts::CpuExit;
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_cpu_core::{AssistReason, CpuCore, Exception};
use aero_devices::a20_gate::A20Gate as A20GateDevice;
use aero_devices::acpi_pm::{
    register_acpi_pm, AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, SharedAcpiPmIo,
};
use aero_devices::clock::ManualClock;
use aero_devices::hpet;
use aero_devices::i8042::{I8042Ports, SharedI8042Controller};
use aero_devices::irq::PlatformIrqLine;
use aero_devices::pci::{
    register_pci_config_ports, PciConfigPorts, PciCoreSnapshot, PciIntxRouter,
    PciIntxRouterConfig, SharedPciConfigPorts,
};
use aero_devices::pic8259::register_pic8259_on_platform_interrupts;
use aero_devices::pit8254::{register_pit8254, Pit8254, SharedPit8254};
use aero_devices::reset_ctrl::{ResetCtrl, RESET_CTRL_PORT};
use aero_devices::rtc_cmos::{register_rtc_cmos, RtcCmos, SharedRtcCmos};
use aero_devices::serial::{register_serial16550, Serial16550, SharedSerial16550};
pub use aero_devices_input::Ps2MouseButton;
use aero_net_backend::{FrameRing, L2TunnelRingBackend, NetworkBackend};
use aero_platform::chipset::{A20GateHandle, ChipsetState};
use aero_platform::interrupts::{InterruptController as PlatformInterruptController, PlatformInterrupts};
use aero_platform::io::IoPortBus;
use aero_platform::reset::{ResetKind, ResetLatch};
use aero_snapshot as snapshot;
use firmware::bios::{A20Gate, Bios, BiosBus, BiosConfig, BlockDevice, DiskError, FirmwareMemory};
use memory::{
    DenseMemory, DirtyGuestMemory, DirtyTracker, MapError, MemoryBus as _, PhysicalMemoryBus,
};

mod pci_firmware;
pub use pci_firmware::{
    PciBusBiosAdapter, PciConfigPortsBiosAdapter, SharedPciConfigPortsBiosAdapter,
};

const FAST_A20_PORT: u16 = 0x92;
const SNAPSHOT_DIRTY_PAGE_SIZE: u32 = 4096;
const SNAPSHOT_MAX_PENDING_EXTERNAL_INTERRUPTS: usize = 1024 * 1024;
const NS_PER_SEC: u128 = 1_000_000_000;

pub mod pc;
pub use pc::{PcMachine, PcMachineConfig};

/// Configuration for [`Machine`].
#[derive(Debug, Clone)]
pub struct MachineConfig {
    /// Guest RAM size in bytes.
    pub ram_size_bytes: u64,
    /// Number of vCPUs (currently must be 1).
    pub cpu_count: u8,
    /// Whether to attach canonical PC platform devices (PIC/APIC/PIT/RTC/PCI/ACPI PM/HPET).
    ///
    /// This is currently opt-in to keep the default machine minimal and deterministic.
    pub enable_pc_platform: bool,
    /// Whether to attach a COM1 16550 serial device at `0x3F8`.
    pub enable_serial: bool,
    /// Whether to attach a legacy i8042 controller at ports `0x60/0x64`.
    pub enable_i8042: bool,
    /// Whether to attach a "fast A20" gate device at port `0x92`.
    pub enable_a20_gate: bool,
    /// Whether to attach a reset control device at port `0xCF9`.
    pub enable_reset_ctrl: bool,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            ram_size_bytes: 64 * 1024 * 1024,
            cpu_count: 1,
            enable_pc_platform: false,
            enable_serial: true,
            enable_i8042: true,
            enable_a20_gate: true,
            enable_reset_ctrl: true,
        }
    }
}

/// A single-step/run invocation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunExit {
    /// The slice completed because `max_insts` was reached.
    Completed { executed: u64 },
    /// The CPU executed `HLT`.
    Halted { executed: u64 },
    /// The guest requested a reset (e.g. via port `0xCF9`).
    ResetRequested { kind: ResetKind, executed: u64 },
    /// Execution stopped because the CPU core needs host assistance.
    Assist { reason: AssistReason, executed: u64 },
    /// Execution stopped due to an exception/fault.
    Exception { exception: Exception, executed: u64 },
    /// Execution stopped due to a fatal CPU exit condition (e.g. triple fault).
    CpuExit { exit: CpuExit, executed: u64 },
}

impl RunExit {
    /// Number of guest instructions executed in this slice (best-effort).
    pub fn executed(&self) -> u64 {
        match *self {
            RunExit::Completed { executed }
            | RunExit::Halted { executed }
            | RunExit::ResetRequested { executed, .. }
            | RunExit::Assist { executed, .. }
            | RunExit::Exception { executed, .. }
            | RunExit::CpuExit { executed, .. } => executed,
        }
    }
}

/// Errors returned when constructing or configuring a [`Machine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineError {
    InvalidCpuCount(u8),
    InvalidDiskSize(usize),
    GuestMemoryTooLarge(u64),
}

impl fmt::Display for MachineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MachineError::InvalidCpuCount(count) => {
                write!(
                    f,
                    "unsupported cpu_count {count} (only 1 is supported today)"
                )
            }
            MachineError::InvalidDiskSize(len) => write!(
                f,
                "disk image length {len} is not a multiple of 512 (BIOS sector size)"
            ),
            MachineError::GuestMemoryTooLarge(size) => write!(
                f,
                "guest RAM size {size} bytes does not fit in the current platform's usize"
            ),
        }
    }
}

impl std::error::Error for MachineError {}

/// In-memory block device backed by a `Vec<u8>` of 512-byte sectors.
#[derive(Debug, Clone)]
pub struct VecBlockDevice {
    data: Vec<u8>,
}

impl VecBlockDevice {
    pub fn new(mut data: Vec<u8>) -> Result<Self, MachineError> {
        if !data.len().is_multiple_of(512) {
            return Err(MachineError::InvalidDiskSize(data.len()));
        }
        if data.is_empty() {
            // Ensure at least one sector exists so BIOS boot attempts are deterministic.
            data.resize(512, 0);
        }
        Ok(Self { data })
    }

    pub fn from_sector0(sector0: [u8; 512]) -> Self {
        Self {
            data: sector0.to_vec(),
        }
    }
}

impl BlockDevice for VecBlockDevice {
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; 512]) -> Result<(), DiskError> {
        let idx = usize::try_from(lba).map_err(|_| DiskError::OutOfRange)?;
        let start = idx.checked_mul(512).ok_or(DiskError::OutOfRange)?;
        let end = start.checked_add(512).ok_or(DiskError::OutOfRange)?;
        let src = self.data.get(start..end).ok_or(DiskError::OutOfRange)?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn size_in_sectors(&self) -> u64 {
        (self.data.len() / 512) as u64
    }
}

struct SystemMemory {
    a20: A20GateHandle,
    inner: RefCell<PhysicalMemoryBus>,
    dirty: DirtyTracker,
}

impl SystemMemory {
    fn new(ram_size_bytes: u64, a20: A20GateHandle) -> Result<Self, MachineError> {
        let ram = DenseMemory::new(ram_size_bytes)
            .map_err(|_| MachineError::GuestMemoryTooLarge(ram_size_bytes))?;
        let (ram, dirty) = DirtyGuestMemory::new(Box::new(ram), SNAPSHOT_DIRTY_PAGE_SIZE);
        let inner = PhysicalMemoryBus::new(Box::new(ram));

        Ok(Self {
            a20,
            inner: RefCell::new(inner),
            dirty,
        })
    }

    fn translate_a20(&self, addr: u64) -> u64 {
        if self.a20.enabled() {
            addr
        } else {
            addr & !(1u64 << 20)
        }
    }

    fn take_dirty_pages(&mut self) -> Vec<u64> {
        self.dirty.take_dirty_pages()
    }

    fn clear_dirty(&mut self) {
        self.dirty.clear_dirty();
    }
}

impl A20Gate for SystemMemory {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20.set_enabled(enabled);
    }

    fn a20_enabled(&self) -> bool {
        self.a20.enabled()
    }
}

impl FirmwareMemory for SystemMemory {
    fn map_rom(&mut self, base: u64, rom: Arc<[u8]>) {
        let len = rom.len();
        let mut inner = self.inner.borrow_mut();
        match inner.map_rom(base, rom) {
            Ok(()) => {}
            Err(MapError::Overlap) => {
                // BIOS resets may re-map the same ROM windows. Treat identical overlaps as
                // idempotent, but reject unexpected overlaps to avoid silently corrupting the bus.
                let already_mapped = inner
                    .rom_regions()
                    .iter()
                    .any(|r| r.start == base && r.data.len() == len);
                if !already_mapped {
                    panic!("unexpected ROM mapping overlap at 0x{base:016x}");
                }
            }
            Err(MapError::AddressOverflow) => {
                panic!("ROM mapping overflow at 0x{base:016x} (len=0x{len:x})")
            }
        }
    }
}

impl memory::MemoryBus for SystemMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if self.a20.enabled() {
            self.inner.borrow_mut().read_physical(paddr, buf);
            return;
        }

        let mut inner = self.inner.borrow_mut();
        for (i, slot) in buf.iter_mut().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            *slot = inner.read_physical_u8(addr);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if self.a20.enabled() {
            self.inner.borrow_mut().write_physical(paddr, buf);
            return;
        }

        let mut inner = self.inner.borrow_mut();
        for (i, byte) in buf.iter().copied().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            inner.write_physical_u8(addr, byte);
        }
    }
}

struct PhysBus<'a> {
    mem: &'a mut SystemMemory,
}

impl aero_mmu::MemoryBus for PhysBus<'_> {
    fn read_u8(&mut self, paddr: u64) -> u8 {
        self.mem.read_u8(paddr)
    }

    fn read_u16(&mut self, paddr: u64) -> u16 {
        self.mem.read_u16(paddr)
    }

    fn read_u32(&mut self, paddr: u64) -> u32 {
        self.mem.read_u32(paddr)
    }

    fn read_u64(&mut self, paddr: u64) -> u64 {
        self.mem.read_u64(paddr)
    }

    fn write_u8(&mut self, paddr: u64, value: u8) {
        self.mem.write_u8(paddr, value);
    }

    fn write_u16(&mut self, paddr: u64, value: u16) {
        self.mem.write_u16(paddr, value);
    }

    fn write_u32(&mut self, paddr: u64, value: u32) {
        self.mem.write_u32(paddr, value);
    }

    fn write_u64(&mut self, paddr: u64, value: u64) {
        self.mem.write_u64(paddr, value);
    }
}

/// Canonical Aero machine: CPU + physical memory + port I/O devices + firmware.
pub struct Machine {
    cfg: MachineConfig,
    chipset: ChipsetState,
    reset_latch: ResetLatch,

    cpu: CpuCore,
    assist: AssistContext,
    mem: SystemMemory,
    io: IoPortBus,

    // Optional PC platform devices. These are behind `Rc<RefCell<_>>` so their host wiring
    // survives snapshot restore (devices reset their internal state but preserve callbacks/irq
    // lines).
    platform_clock: Option<ManualClock>,
    interrupts: Option<Rc<RefCell<PlatformInterrupts>>>,
    pit: Option<SharedPit8254>,
    rtc: Option<SharedRtcCmos<ManualClock, PlatformIrqLine>>,
    pci_cfg: Option<SharedPciConfigPorts>,
    pci_intx: Option<Rc<RefCell<PciIntxRouter>>>,
    acpi_pm: Option<SharedAcpiPmIo<ManualClock>>,
    hpet: Option<Rc<RefCell<hpet::Hpet<ManualClock>>>>,

    bios: Bios,
    disk: VecBlockDevice,
    network_backend: Option<Box<dyn NetworkBackend>>,

    serial: Option<SharedSerial16550>,
    i8042: Option<SharedI8042Controller>,
    serial_log: Vec<u8>,

    next_snapshot_id: u64,
    last_snapshot_id: Option<u64>,

    // Temporary storage used during snapshot restore: `restore_device_states` decodes
    // CPU_INTERNAL but `post_restore` resets `cpu.pending` to a baseline default before
    // applying this machine-defined state back on top.
    restored_cpu_internal: Option<snapshot::CpuInternalState>,

    /// Remainder used when converting CPU cycles (TSC ticks) into nanoseconds for deterministic
    /// platform device ticking.
    ///
    /// This is `total_cycles * 1e9 mod tsc_hz`, carried across batches to avoid long-run drift.
    tsc_ns_remainder: u64,
}

impl Machine {
    pub fn new(cfg: MachineConfig) -> Result<Self, MachineError> {
        if cfg.cpu_count != 1 {
            return Err(MachineError::InvalidCpuCount(cfg.cpu_count));
        }

        let chipset = ChipsetState::new(false);
        let mem = SystemMemory::new(cfg.ram_size_bytes, chipset.a20())?;

        let mut machine = Self {
            cfg,
            chipset,
            reset_latch: ResetLatch::new(),
            cpu: CpuCore::new(CpuMode::Real),
            assist: AssistContext::default(),
            mem,
            io: IoPortBus::new(),
            platform_clock: None,
            interrupts: None,
            pit: None,
            rtc: None,
            pci_cfg: None,
            pci_intx: None,
            acpi_pm: None,
            hpet: None,
            bios: Bios::new(BiosConfig::default()),
            disk: VecBlockDevice::new(Vec::new()).expect("empty disk is valid"),
            network_backend: None,
            serial: None,
            i8042: None,
            serial_log: Vec::new(),
            next_snapshot_id: 1,
            last_snapshot_id: None,
            restored_cpu_internal: None,
            tsc_ns_remainder: 0,
        };

        machine.reset();
        Ok(machine)
    }

    /// Returns the current CPU state.
    pub fn cpu(&self) -> &CpuState {
        &self.cpu.state
    }

    /// Mutable access to the current CPU state (debug/testing only).
    pub fn cpu_mut(&mut self) -> &mut CpuState {
        &mut self.cpu.state
    }

    /// Replace the attached disk image.
    pub fn set_disk_image(&mut self, bytes: Vec<u8>) -> Result<(), MachineError> {
        self.disk = VecBlockDevice::new(bytes)?;
        Ok(())
    }

    /// Install/replace the host-side network backend used by any emulated NICs.
    ///
    /// Note: this backend is *external* state (e.g. a live tunnel connection) and is intentionally
    /// not included in snapshots. Callers should either:
    /// - re-attach after restoring a snapshot, or
    /// - call [`Machine::detach_network`] before snapshotting to make the lifecycle explicit.
    pub fn set_network_backend(&mut self, backend: Box<dyn NetworkBackend>) {
        self.network_backend = Some(backend);
    }

    /// Attach a ring-buffer-backed L2 tunnel network backend (NET_TX / NET_RX).
    pub fn attach_l2_tunnel_rings<TX: FrameRing + 'static, RX: FrameRing + 'static>(
        &mut self,
        tx: TX,
        rx: RX,
    ) {
        self.set_network_backend(Box::new(L2TunnelRingBackend::new(tx, rx)));
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

    /// Detach (drop) any currently installed network backend.
    pub fn detach_network(&mut self) {
        self.network_backend = None;
    }

    /// Debug/testing helper: read a single guest physical byte.
    pub fn read_physical_u8(&mut self, paddr: u64) -> u8 {
        self.mem.read_u8(paddr)
    }

    /// Debug/testing helper: read a little-endian u16 from guest physical memory.
    pub fn read_physical_u16(&mut self, paddr: u64) -> u16 {
        self.mem.read_u16(paddr)
    }

    /// Debug/testing helper: read a range of guest physical memory into a new buffer.
    pub fn read_physical_bytes(&mut self, paddr: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        self.mem.read_physical(paddr, &mut out);
        out
    }

    /// Debug/testing helper: read from an I/O port.
    pub fn io_read(&mut self, port: u16, size: u8) -> u32 {
        self.io.read(port, size)
    }

    /// Debug/testing helper: write to an I/O port.
    pub fn io_write(&mut self, port: u16, size: u8, value: u32) {
        self.io.write(port, size, value);
    }

    /// Returns the shared manual clock backing platform timer devices, if the PC platform is
    /// enabled.
    pub fn platform_clock(&self) -> Option<ManualClock> {
        self.platform_clock.clone()
    }

    /// Returns the platform interrupt controller complex (PIC + IOAPIC + LAPIC), if present.
    pub fn platform_interrupts(&self) -> Option<Rc<RefCell<PlatformInterrupts>>> {
        self.interrupts.clone()
    }

    /// Returns the PCI config mechanism #1 ports device, if present.
    pub fn pci_config_ports(&self) -> Option<SharedPciConfigPorts> {
        self.pci_cfg.clone()
    }

    /// Returns the PCI INTx router, if present.
    pub fn pci_intx_router(&self) -> Option<Rc<RefCell<PciIntxRouter>>> {
        self.pci_intx.clone()
    }

    /// Returns the PIT 8254 device, if present.
    pub fn pit(&self) -> Option<SharedPit8254> {
        self.pit.clone()
    }

    /// Returns the RTC CMOS device, if present.
    pub fn rtc(&self) -> Option<SharedRtcCmos<ManualClock, PlatformIrqLine>> {
        self.rtc.clone()
    }

    /// Returns the ACPI PM I/O device, if present.
    pub fn acpi_pm(&self) -> Option<SharedAcpiPmIo<ManualClock>> {
        self.acpi_pm.clone()
    }

    /// Returns the HPET device, if present.
    pub fn hpet(&self) -> Option<Rc<RefCell<hpet::Hpet<ManualClock>>>> {
        self.hpet.clone()
    }

    /// Advance deterministic platform time and poll any timer devices.
    ///
    /// This is a testing/debugging helper; the canonical CPU stepping loop does not currently
    /// advance platform time automatically.
    pub fn tick_platform(&mut self, delta_ns: u64) {
        if let Some(clock) = &self.platform_clock {
            clock.advance_ns(delta_ns);
        }

        if let Some(acpi_pm) = &self.acpi_pm {
            acpi_pm.borrow_mut().advance_ns(delta_ns);
        }

        if let Some(pit) = &self.pit {
            pit.borrow_mut().advance_ns(delta_ns);
        }

        if let Some(rtc) = &self.rtc {
            rtc.borrow_mut().tick();
        }

        if let Some(interrupts) = &self.interrupts {
            interrupts.borrow().tick(delta_ns);
        }

        if let (Some(hpet), Some(interrupts)) = (&self.hpet, &self.interrupts) {
            let mut hpet = hpet.borrow_mut();
            let mut interrupts = interrupts.borrow_mut();
            hpet.poll(&mut *interrupts);
        }
    }

    fn tick_platform_from_cycles(&mut self, cycles: u64) {
        if self.platform_clock.is_none() {
            return;
        }

        let tsc_hz = self.cpu.time.tsc_hz();
        if tsc_hz == 0 || cycles == 0 {
            return;
        }

        let acc = (cycles as u128) * NS_PER_SEC + (self.tsc_ns_remainder as u128);
        let delta_ns_u128 = acc / (tsc_hz as u128);
        self.tsc_ns_remainder = (acc % (tsc_hz as u128)) as u64;

        let delta_ns = delta_ns_u128.min(u64::MAX as u128) as u64;
        if delta_ns != 0 {
            self.tick_platform(delta_ns);
        }
    }

    fn idle_tick_platform_1ms(&mut self) {
        if self.platform_clock.is_none() {
            return;
        }

        // Only tick while halted when maskable interrupts are enabled; otherwise HLT is expected to
        // be terminal (until NMI/SMI/reset, which we do not model yet).
        if (self.cpu.state.rflags() & aero_cpu_core::state::RFLAGS_IF) == 0 {
            return;
        }

        let tsc_hz = self.cpu.time.tsc_hz();
        if tsc_hz == 0 {
            return;
        }

        // Advance 1ms worth of CPU cycles while halted so timer devices can wake the CPU.
        let cycles = (tsc_hz / 1000).max(1);
        self.cpu.time.advance_cycles(cycles);
        self.cpu.state.msr.tsc = self.cpu.time.read_tsc();
        self.tick_platform_from_cycles(cycles);
    }

    fn resync_tsc_ns_remainder_from_tsc(&mut self) {
        let tsc_hz = self.cpu.time.tsc_hz();
        if self.platform_clock.is_none() || tsc_hz == 0 {
            self.tsc_ns_remainder = 0;
            return;
        }
        let tsc = self.cpu.state.msr.tsc;
        self.tsc_ns_remainder = ((tsc as u128) * NS_PER_SEC % (tsc_hz as u128)) as u64;
    }

    /// Take (drain) all serial output accumulated so far.
    pub fn take_serial_output(&mut self) -> Vec<u8> {
        self.flush_serial();
        std::mem::take(&mut self.serial_log)
    }

    /// Return a copy of the serial output accumulated so far without draining it.
    ///
    /// This is intentionally a cloning API: callers that only need a byte count should prefer
    /// [`Machine::serial_output_len`].
    pub fn serial_output_bytes(&mut self) -> Vec<u8> {
        self.flush_serial();
        self.serial_log.clone()
    }

    /// Return the number of bytes currently buffered in the serial output log.
    ///
    /// This is a cheap alternative to [`Machine::take_serial_output`] for callers that only need a
    /// byte count (e.g. UI progress indicators) and want to avoid copying large buffers.
    pub fn serial_output_len(&mut self) -> u64 {
        self.flush_serial();
        u64::try_from(self.serial_log.len()).unwrap_or(u64::MAX)
    }

    /// Inject a browser-style keyboard code into the i8042 controller, if present.
    pub fn inject_browser_key(&mut self, code: &str, pressed: bool) {
        if let Some(ctrl) = &self.i8042 {
            ctrl.borrow_mut().inject_browser_key(code, pressed);
        }
    }

    /// Inject relative mouse motion into the i8042 controller, if present.
    ///
    /// `dx` is positive to the right and `dy` is positive down (browser-style). The underlying PS/2
    /// mouse model converts this into PS/2 packet coordinates (+Y is up).
    pub fn inject_mouse_motion(&mut self, dx: i32, dy: i32, wheel: i32) {
        if let Some(ctrl) = &self.i8042 {
            ctrl.borrow_mut().inject_mouse_motion(dx, dy, wheel);
        }
    }

    /// Inject a PS/2 mouse button transition into the i8042 controller, if present.
    pub fn inject_mouse_button(&mut self, button: Ps2MouseButton, pressed: bool) {
        if let Some(ctrl) = &self.i8042 {
            ctrl.borrow_mut().inject_mouse_button(button, pressed);
        }
    }

    pub fn inject_mouse_left(&mut self, pressed: bool) {
        self.inject_mouse_button(Ps2MouseButton::Left, pressed);
    }

    pub fn inject_mouse_right(&mut self, pressed: bool) {
        self.inject_mouse_button(Ps2MouseButton::Right, pressed);
    }

    pub fn inject_mouse_middle(&mut self, pressed: bool) {
        self.inject_mouse_button(Ps2MouseButton::Middle, pressed);
    }

    pub fn take_snapshot_full(&mut self) -> snapshot::Result<Vec<u8>> {
        self.take_snapshot_with_options(snapshot::SaveOptions::default())
    }

    pub fn save_snapshot_full_to<W: Write + Seek>(&mut self, w: &mut W) -> snapshot::Result<()> {
        self.save_snapshot_to(w, snapshot::SaveOptions::default())
    }

    pub fn take_snapshot_dirty(&mut self) -> snapshot::Result<Vec<u8>> {
        let mut options = snapshot::SaveOptions::default();
        options.ram.mode = snapshot::RamMode::Dirty;
        self.take_snapshot_with_options(options)
    }

    pub fn save_snapshot_dirty_to<W: Write + Seek>(&mut self, w: &mut W) -> snapshot::Result<()> {
        let mut options = snapshot::SaveOptions::default();
        options.ram.mode = snapshot::RamMode::Dirty;
        self.save_snapshot_to(w, options)
    }

    pub fn restore_snapshot_bytes(&mut self, bytes: &[u8]) -> snapshot::Result<()> {
        self.restore_snapshot_from_checked(&mut Cursor::new(bytes))
    }

    pub fn restore_snapshot_from<R: Read>(&mut self, r: &mut R) -> snapshot::Result<()> {
        snapshot::restore_snapshot(r, self)
    }

    pub fn restore_snapshot_from_checked<R: Read + Seek>(
        &mut self,
        r: &mut R,
    ) -> snapshot::Result<()> {
        // Restoring a snapshot is conceptually "rewinding time", so discard any accumulated host
        // output/state from the current execution.
        self.detach_network();
        self.flush_serial();
        if let Some(uart) = &self.serial {
            let _ = uart.borrow_mut().take_tx();
        }
        self.serial_log.clear();
        self.reset_latch.clear();
        self.restored_cpu_internal = None;

        let expected_parent_snapshot_id = self.last_snapshot_id;
        snapshot::restore_snapshot_with_options(
            r,
            self,
            snapshot::RestoreOptions {
                expected_parent_snapshot_id,
            },
        )
    }

    fn save_snapshot_to<W: Write + Seek>(
        &mut self,
        w: &mut W,
        options: snapshot::SaveOptions,
    ) -> snapshot::Result<()> {
        self.flush_serial();
        snapshot::save_snapshot(w, self, options)
    }

    fn take_snapshot_with_options(
        &mut self,
        options: snapshot::SaveOptions,
    ) -> snapshot::Result<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        self.save_snapshot_to(&mut cursor, options)?;
        Ok(cursor.into_inner())
    }

    /// Reset the machine and transfer control to firmware POST (boot sector).
    pub fn reset(&mut self) {
        self.reset_latch.clear();
        self.serial_log.clear();
        self.tsc_ns_remainder = 0;

        // Reset chipset lines.
        self.chipset.a20().set_enabled(false);

        // Rebuild port I/O devices for deterministic power-on state.
        self.io = IoPortBus::new();

        if self.cfg.enable_pc_platform {
            let clock = ManualClock::new();
            let interrupts: Rc<RefCell<PlatformInterrupts>> =
                Rc::new(RefCell::new(PlatformInterrupts::new()));

            PlatformInterrupts::register_imcr_ports(&mut self.io, interrupts.clone());
            register_pic8259_on_platform_interrupts(&mut self.io, interrupts.clone());

            let pit: SharedPit8254 = Rc::new(RefCell::new(Pit8254::new()));
            pit.borrow_mut()
                .connect_irq0_to_platform_interrupts(interrupts.clone());
            register_pit8254(&mut self.io, pit.clone());

            let rtc_irq8 = PlatformIrqLine::isa(interrupts.clone(), 8);
            let rtc: SharedRtcCmos<ManualClock, PlatformIrqLine> =
                Rc::new(RefCell::new(RtcCmos::new(clock.clone(), rtc_irq8)));
            rtc.borrow_mut()
                .set_memory_size_bytes(self.cfg.ram_size_bytes);
            register_rtc_cmos(&mut self.io, rtc.clone());

            // Wire ACPI PM to the shared deterministic platform clock so `PM_TMR` progresses only
            // when the host advances `ManualClock` (via `Machine::tick_platform`).
            let acpi_pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks_and_clock(
                AcpiPmConfig::default(),
                AcpiPmCallbacks {
                    sci_irq: Box::new(PlatformIrqLine::isa(interrupts.clone(), 9)),
                    request_power_off: None,
                },
                clock.clone(),
            )));
            register_acpi_pm(&mut self.io, acpi_pm.clone());

            let pci_cfg: SharedPciConfigPorts = Rc::new(RefCell::new(PciConfigPorts::new()));
            register_pci_config_ports(&mut self.io, pci_cfg.clone());

            let pci_intx = Rc::new(RefCell::new(PciIntxRouter::new(
                PciIntxRouterConfig::default(),
            )));

            let hpet = Rc::new(RefCell::new(hpet::Hpet::new_default(clock.clone())));

            self.platform_clock = Some(clock);
            self.interrupts = Some(interrupts);
            self.pit = Some(pit);
            self.rtc = Some(rtc);
            self.pci_cfg = Some(pci_cfg);
            self.pci_intx = Some(pci_intx);
            self.acpi_pm = Some(acpi_pm);
            self.hpet = Some(hpet);
        } else {
            self.platform_clock = None;
            self.interrupts = None;
            self.pit = None;
            self.rtc = None;
            self.pci_cfg = None;
            self.pci_intx = None;
            self.acpi_pm = None;
            self.hpet = None;
        }

        if self.cfg.enable_serial {
            let uart: SharedSerial16550 = Rc::new(RefCell::new(Serial16550::new(0x3F8)));
            register_serial16550(&mut self.io, uart.clone());
            self.serial = Some(uart);
        } else {
            self.serial = None;
        }

        if self.cfg.enable_a20_gate {
            let dev = A20GateDevice::with_reset_sink(self.chipset.a20(), self.reset_latch.clone());
            self.io.register(FAST_A20_PORT, Box::new(dev));
        }

        if self.cfg.enable_reset_ctrl {
            self.io.register(
                RESET_CTRL_PORT,
                Box::new(ResetCtrl::new(self.reset_latch.clone())),
            );
        }

        if self.cfg.enable_i8042 {
            let ports = I8042Ports::new();
            let ctrl = ports.controller();
            aero_devices::i8042::register_i8042(&mut self.io, ctrl.clone());

            ctrl.borrow_mut().set_system_control_sink(Box::new(
                aero_devices::i8042::PlatformSystemControlSink::with_reset_sink(
                    self.chipset.a20(),
                    self.reset_latch.clone(),
                ),
            ));

            self.i8042 = Some(ctrl);
        } else {
            self.i8042 = None;
        }

        self.assist = AssistContext::default();
        self.cpu = CpuCore::new(CpuMode::Real);

        // Run firmware POST (in Rust) to initialize IVT/BDA, map BIOS stubs, and load the boot
        // sector into RAM.
        self.bios = Bios::new(BiosConfig {
            memory_size_bytes: self.cfg.ram_size_bytes,
            cpu_count: self.cfg.cpu_count,
            ..Default::default()
        });
        let bus: &mut dyn BiosBus = &mut self.mem;
        if let Some(pci_cfg) = &self.pci_cfg {
            let mut pci = SharedPciConfigPortsBiosAdapter::new(pci_cfg.clone());
            self.bios
                .post_with_pci(&mut self.cpu.state, bus, &mut self.disk, Some(&mut pci));
        } else {
            self.bios.post(&mut self.cpu.state, bus, &mut self.disk);
        }
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
        self.mem.clear_dirty();
    }

    /// Run the CPU for at most `max_insts` guest instructions.
    pub fn run_slice(&mut self, max_insts: u64) -> RunExit {
        let mut executed = 0u64;
        let cfg = Tier0Config::default();
        while executed < max_insts {
            if let Some(kind) = self.reset_latch.take() {
                self.flush_serial();
                return RunExit::ResetRequested { kind, executed };
            }

            // Keep the core's A20 view coherent with the chipset latch.
            self.cpu.state.a20_enabled = self.chipset.a20().enabled();

            // Poll the platform interrupt controller (PIC/IOAPIC+LAPIC) and enqueue at most one
            // pending external interrupt vector into the CPU core.
            //
            // Tier-0 only delivers interrupts that are already present in
            // `cpu.pending.external_interrupts`; it does not poll an interrupt controller itself.
            //
            // Keep this polling bounded so a level-triggered interrupt line that remains asserted
            // cannot cause an unbounded growth of the external interrupt FIFO when the guest has
            // interrupts masked (IF=0) or otherwise cannot accept delivery yet.
            const MAX_QUEUED_EXTERNAL_INTERRUPTS: usize = 1;
            if self.cpu.pending.external_interrupts.len() < MAX_QUEUED_EXTERNAL_INTERRUPTS {
                if let Some(interrupts) = &self.interrupts {
                    let mut interrupts = interrupts.borrow_mut();
                    if let Some(vector) = PlatformInterruptController::get_pending(&*interrupts) {
                        PlatformInterruptController::acknowledge(&mut *interrupts, vector);
                        self.cpu.pending.inject_external_interrupt(vector);
                    }
                }
            }

            let remaining = max_insts - executed;
            let phys = PhysBus { mem: &mut self.mem };
            let mut bus = aero_cpu_core::PagingBus::new_with_io(phys, &mut self.io);

            let batch = run_batch_cpu_core_with_assists(
                &cfg,
                &mut self.assist,
                &mut self.cpu,
                &mut bus,
                remaining,
            );
            executed = executed.saturating_add(batch.executed);

            // Deterministically advance platform time based on executed cycles.
            self.tick_platform_from_cycles(batch.executed);

            match batch.exit {
                BatchExit::Completed => {
                    self.flush_serial();
                    return RunExit::Completed { executed };
                }
                BatchExit::Branch => continue,
                BatchExit::Halted => {
                    // When halted, advance platform time so timer interrupts can wake the CPU.
                    self.idle_tick_platform_1ms();
                    self.flush_serial();
                    return RunExit::Halted { executed };
                }
                BatchExit::BiosInterrupt(vector) => {
                    self.handle_bios_interrupt(vector);
                }
                BatchExit::Assist(reason) => {
                    self.flush_serial();
                    return RunExit::Assist { reason, executed };
                }
                BatchExit::Exception(exception) => {
                    self.flush_serial();
                    return RunExit::Exception {
                        exception,
                        executed,
                    };
                }
                BatchExit::CpuExit(exit) => {
                    self.flush_serial();
                    return RunExit::CpuExit { exit, executed };
                }
            }
        }

        self.flush_serial();
        RunExit::Completed { executed }
    }

    fn handle_bios_interrupt(&mut self, vector: u8) {
        // Keep the core's A20 view coherent with the chipset latch while executing BIOS services.
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
        let bus: &mut dyn BiosBus = &mut self.mem;
        self.bios
            .dispatch_interrupt(vector, &mut self.cpu.state, bus, &mut self.disk);
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
    }

    fn flush_serial(&mut self) {
        let Some(uart) = &self.serial else {
            return;
        };
        let mut uart = uart.borrow_mut();
        let tx = uart.take_tx();
        if !tx.is_empty() {
            self.serial_log.extend_from_slice(&tx);
        }
    }
}

impl snapshot::SnapshotSource for Machine {
    fn snapshot_meta(&mut self) -> snapshot::SnapshotMeta {
        let snapshot_id = self.next_snapshot_id;
        self.next_snapshot_id = self.next_snapshot_id.saturating_add(1);

        #[cfg(target_arch = "wasm32")]
        let created_unix_ms = 0u64;
        #[cfg(not(target_arch = "wasm32"))]
        let created_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);

        let meta = snapshot::SnapshotMeta {
            snapshot_id,
            parent_snapshot_id: self.last_snapshot_id,
            created_unix_ms,
            label: None,
        };
        self.last_snapshot_id = Some(snapshot_id);
        meta
    }

    fn cpu_state(&self) -> snapshot::CpuState {
        snapshot::cpu_state_from_cpu_core(&self.cpu)
    }

    fn mmu_state(&self) -> snapshot::MmuState {
        snapshot::mmu_state_from_cpu_core(&self.cpu)
    }

    fn device_states(&self) -> Vec<snapshot::DeviceState> {
        const V1: u16 = 1;
        let mut devices = Vec::new();

        // Firmware snapshot: required for deterministic BIOS interrupt behavior.
        let bios_snapshot = self.bios.snapshot();
        let mut bios_bytes = Vec::new();
        if bios_snapshot.encode(&mut bios_bytes).is_ok() {
            devices.push(snapshot::DeviceState {
                id: snapshot::DeviceId::BIOS,
                version: V1,
                flags: 0,
                data: bios_bytes,
            });
        }

        // Memory/chipset glue.
        devices.push(snapshot::DeviceState {
            id: snapshot::DeviceId::MEMORY,
            version: V1,
            flags: 0,
            data: vec![self.chipset.a20().enabled() as u8],
        });

        // Accumulated serial output (drained from the UART by `Machine::run_slice`).
        devices.push(snapshot::DeviceState {
            id: snapshot::DeviceId::SERIAL,
            version: V1,
            flags: 0,
            data: self.serial_log.clone(),
        });

        // Optional PC platform devices.
        //
        // Note: We snapshot the combined PIC + IOAPIC + LAPIC router state via
        // `PlatformInterrupts` under the historical `DeviceId::APIC` ID.
        if let Some(interrupts) = &self.interrupts {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::APIC,
                &*interrupts.borrow(),
            ));
        }
        if let Some(pit) = &self.pit {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::PIT,
                &*pit.borrow(),
            ));
        }
        if let Some(rtc) = &self.rtc {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::RTC,
                &*rtc.borrow(),
            ));
        }
        if let Some(pci_cfg) = &self.pci_cfg {
            if let Some(pci_intx) = &self.pci_intx {
                // Store PCI core state under a single outer `DeviceId::PCI` entry to avoid
                // duplicate `(id, version, flags)` tuples in `aero_snapshot` while still capturing
                // both:
                // - PCI config mechanism + config-space/BAR state (`PCPT`)
                // - PCI INTx routing + asserted level refcounts (`INTX`)
                let mut pci_cfg = pci_cfg.borrow_mut();
                let mut pci_intx = pci_intx.borrow_mut();
                let core = PciCoreSnapshot::new(&mut *pci_cfg, &mut *pci_intx);
                devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                    snapshot::DeviceId::PCI,
                    &core,
                ));
            } else {
                // Fallback: config ports only.
                devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                    snapshot::DeviceId::PCI,
                    &*pci_cfg.borrow(),
                ));
            }
        }
        if let Some(acpi_pm) = &self.acpi_pm {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::ACPI_PM,
                &*acpi_pm.borrow(),
            ));
        }
        if let Some(hpet) = &self.hpet {
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::HPET,
                &*hpet.borrow(),
            ));
        }

        if let Some(ctrl) = &self.i8042 {
            let ctrl = ctrl.borrow();
            devices.push(snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::I8042,
                &*ctrl,
            ));
        }

        // CPU_INTERNAL: non-architectural Tier-0 bookkeeping required for deterministic resume.
        let cpu_internal = snapshot::CpuInternalState {
            interrupt_inhibit: self.cpu.pending.interrupt_inhibit(),
            pending_external_interrupts: self
                .cpu
                .pending
                .external_interrupts
                .iter()
                .copied()
                .take(SNAPSHOT_MAX_PENDING_EXTERNAL_INTERRUPTS)
                .collect(),
        };
        devices.push(
            cpu_internal
                .to_device_state()
                .unwrap_or_else(|_| snapshot::DeviceState {
                    id: snapshot::DeviceId::CPU_INTERNAL,
                    version: snapshot::CpuInternalState::VERSION,
                    flags: 0,
                    // `CpuInternalState::encode` for a default/empty state.
                    data: vec![0, 0, 0, 0, 0],
                }),
        );
        devices
    }

    fn disk_overlays(&self) -> snapshot::DiskOverlayRefs {
        snapshot::DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        usize::try_from(self.cfg.ram_size_bytes).unwrap_or(0)
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> snapshot::Result<()> {
        let end = offset
            .checked_add(buf.len() as u64)
            .ok_or(snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        if end > self.cfg.ram_size_bytes {
            return Err(snapshot::SnapshotError::Corrupt("ram read out of range"));
        }
        self.mem.inner.borrow_mut().read_physical(offset, buf);
        Ok(())
    }

    fn dirty_page_size(&self) -> u32 {
        SNAPSHOT_DIRTY_PAGE_SIZE
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        Some(self.mem.take_dirty_pages())
    }
}

impl snapshot::SnapshotTarget for Machine {
    fn restore_meta(&mut self, meta: snapshot::SnapshotMeta) {
        self.last_snapshot_id = Some(meta.snapshot_id);
        self.next_snapshot_id = self
            .next_snapshot_id
            .max(meta.snapshot_id.saturating_add(1));
    }

    fn restore_cpu_state(&mut self, state: snapshot::CpuState) {
        // Clear any stale restore-only state before applying new snapshot sections.
        self.restored_cpu_internal = None;
        snapshot::apply_cpu_state_to_cpu_core(&state, &mut self.cpu);
    }

    fn restore_mmu_state(&mut self, state: snapshot::MmuState) {
        snapshot::apply_mmu_state_to_cpu_core(&state, &mut self.cpu);
        self.cpu.time.set_tsc(self.cpu.state.msr.tsc);
    }

    fn restore_device_states(&mut self, states: Vec<snapshot::DeviceState>) {
        use std::collections::HashMap;

        // Clear any stale restore-only state before applying new snapshot sections.
        self.restored_cpu_internal = None;

        // Restore ordering must be explicit and independent of snapshot file ordering so device
        // state is deterministic (especially for interrupt lines and PCI INTx routing).
        let mut by_id: HashMap<snapshot::DeviceId, snapshot::DeviceState> =
            HashMap::with_capacity(states.len());
        for state in states {
            // Snapshot format already rejects duplicate (id, version, flags) tuples; for multiple
            // entries with the same outer ID (forward-compatible versions), prefer the first one.
            by_id.entry(state.id).or_insert(state);
        }

        // Firmware snapshot: required for deterministic BIOS interrupt behaviour.
        if let Some(state) = by_id.remove(&snapshot::DeviceId::BIOS) {
            if state.version == 1 {
                if let Ok(snapshot) =
                    firmware::bios::BiosSnapshot::decode(&mut Cursor::new(&state.data))
                {
                    self.bios.restore_snapshot(snapshot, &mut self.mem);
                }
            }
        }

        // Memory/chipset glue.
        if let Some(state) = by_id.remove(&snapshot::DeviceId::MEMORY) {
            if state.version == 1 {
                let enabled = state.data.first().copied().unwrap_or(0) != 0;
                self.chipset.a20().set_enabled(enabled);
                self.cpu.state.a20_enabled = enabled;
            }
        }

        // Accumulated serial output.
        if let Some(state) = by_id.remove(&snapshot::DeviceId::SERIAL) {
            if state.version == 1 {
                if let Some(uart) = &self.serial {
                    let _ = uart.borrow_mut().take_tx();
                }
                self.serial_log = state.data;
            }
        }

        // Optional PC platform devices.

        // 1) Restore interrupt controller complex first.
        let mut restored_interrupts = false;
        if let (Some(interrupts), Some(state)) =
            (&self.interrupts, by_id.remove(&snapshot::DeviceId::APIC))
        {
            let mut interrupts = interrupts.borrow_mut();
            let _ =
                snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *interrupts);
            restored_interrupts = true;
        }

        let mut restored_pci_intx = false;
        // 2) Restore PCI devices (config ports + INTx router).
        //
        // Newer snapshots store both under a single `DeviceId::PCI` entry using
        // `aero_devices::pci::PciCoreSnapshot`.
        let pci_state = by_id.remove(&snapshot::DeviceId::PCI);
        let pci_cfg_state = by_id.remove(&snapshot::DeviceId::PCI_CFG);

        if let Some(state) = pci_state {
            if let (Some(pci_cfg), Some(pci_intx)) = (&self.pci_cfg, &self.pci_intx) {
                let mut pci_cfg = pci_cfg.borrow_mut();
                let mut pci_intx = pci_intx.borrow_mut();
                let mut core = PciCoreSnapshot::new(&mut *pci_cfg, &mut *pci_intx);
                match snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut core) {
                    Ok(()) => {
                        restored_pci_intx = true;
                    }
                    Err(_) => {
                        // Backward compatibility: older snapshots stored only `PciConfigPorts`
                        // (`PCPT`) under `DeviceId::PCI`.
                        let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                            &state,
                            &mut *pci_cfg,
                        );
                    }
                }
            } else if let Some(pci_cfg) = &self.pci_cfg {
                let mut pci_cfg = pci_cfg.borrow_mut();
                let _ =
                    snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *pci_cfg);
            }
        } else if let (Some(pci_cfg), Some(state)) = (&self.pci_cfg, pci_cfg_state) {
            // Older snapshots used a dedicated `DeviceId::PCI_CFG` entry for config ports.
            let mut pci_cfg = pci_cfg.borrow_mut();
            let _ =
                snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *pci_cfg);
        }

        // Backward compatibility: older snapshots stored INTx routing separately.
        if !restored_pci_intx {
            if let (Some(pci_intx), Some(state)) =
                (&self.pci_intx, by_id.remove(&snapshot::DeviceId::PCI_INTX))
            {
                let mut pci_intx = pci_intx.borrow_mut();
                let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                    &state,
                    &mut *pci_intx,
                );
                restored_pci_intx = true;
            }
        }

        // 3) After restoring both the interrupt controller and the PCI INTx router, re-drive any
        // asserted level-triggered GSIs into the interrupt sink.
        if restored_interrupts && restored_pci_intx {
            if let (Some(pci_intx), Some(interrupts)) = (&self.pci_intx, &self.interrupts) {
                let pci_intx = pci_intx.borrow();
                let mut interrupts = interrupts.borrow_mut();
                pci_intx.sync_levels_to_sink(&mut *interrupts);
            }
        }

        // 4) Restore PIT + RTC + ACPI PM (these can drive IRQ lines during load_state()).
        if let (Some(pit), Some(state)) = (&self.pit, by_id.remove(&snapshot::DeviceId::PIT)) {
            let mut pit = pit.borrow_mut();
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *pit);
        }
        if let (Some(rtc), Some(state)) = (&self.rtc, by_id.remove(&snapshot::DeviceId::RTC)) {
            let mut rtc = rtc.borrow_mut();
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *rtc);
        }
        if let (Some(acpi_pm), Some(state)) =
            (&self.acpi_pm, by_id.remove(&snapshot::DeviceId::ACPI_PM))
        {
            let mut acpi_pm = acpi_pm.borrow_mut();
            let _ =
                snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *acpi_pm);
        }

        // 5) Restore HPET.
        let mut restored_hpet = false;
        if let (Some(hpet), Some(state)) = (&self.hpet, by_id.remove(&snapshot::DeviceId::HPET)) {
            let mut hpet = hpet.borrow_mut();
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(&state, &mut *hpet);
            restored_hpet = true;
        }

        // 6) After HPET restore, poll once so any level-triggered lines implied by restored
        // interrupt status are asserted immediately.
        if restored_hpet {
            if let (Some(hpet), Some(interrupts)) = (&self.hpet, &self.interrupts) {
                let mut hpet = hpet.borrow_mut();
                let mut interrupts = interrupts.borrow_mut();
                hpet.poll(&mut *interrupts);
            }
        }

        // Restore i8042 after the interrupt controller complex so any restored IRQ pulses are
        // delivered into the correct sink state.
        if let (Some(ctrl), Some(state)) = (&self.i8042, by_id.remove(&snapshot::DeviceId::I8042)) {
            let _ = snapshot::io_snapshot_bridge::apply_io_snapshot_to_device(
                &state,
                &mut *ctrl.borrow_mut(),
            );
        }

        // CPU_INTERNAL: machine-defined CPU bookkeeping (applied in `post_restore`).
        if let Some(state) = by_id.remove(&snapshot::DeviceId::CPU_INTERNAL) {
            if let Ok(decoded) = snapshot::CpuInternalState::from_device_state(&state) {
                self.restored_cpu_internal = Some(decoded);
            }
        }
    }

    fn restore_disk_overlays(&mut self, _overlays: snapshot::DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        usize::try_from(self.cfg.ram_size_bytes).unwrap_or(0)
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> snapshot::Result<()> {
        let end = offset
            .checked_add(data.len() as u64)
            .ok_or(snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        if end > self.cfg.ram_size_bytes {
            return Err(snapshot::SnapshotError::Corrupt("ram write out of range"));
        }
        self.mem.inner.borrow_mut().write_physical(offset, data);
        Ok(())
    }

    fn post_restore(&mut self) -> snapshot::Result<()> {
        // Network backends are external host state (e.g. live proxy connections) and are not part
        // of the snapshot format. Ensure we always drop any previously attached backend after
        // restoring, even if the caller bypasses the `Machine::restore_snapshot_*` helper methods
        // and drives snapshot restore directly via `aero_snapshot::restore_snapshot`.
        self.detach_network();
        self.reset_latch.clear();
        self.assist = AssistContext::default();
        // Reset non-architectural interrupt bookkeeping to a deterministic baseline. If the
        // snapshot contains a CPU_INTERNAL device entry, apply its fields back on top.
        let cpu_internal = self.restored_cpu_internal.take();
        self.cpu.pending = Default::default();
        if let Some(cpu_internal) = cpu_internal {
            self.cpu
                .pending
                .set_interrupt_inhibit(cpu_internal.interrupt_inhibit);
            self.cpu.pending.external_interrupts = cpu_internal.pending_external_interrupts.into();
        }
        self.mem.clear_dirty();
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
        self.resync_tsc_ns_remainder_from_tsc();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::io::{Cursor, Read};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn build_serial_boot_sector(message: &[u8]) -> [u8; 512] {
        let mut sector = [0u8; 512];
        let mut i = 0usize;

        // mov dx, 0x3f8
        sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
        i += 3;

        for &b in message {
            // mov al, imm8
            sector[i..i + 2].copy_from_slice(&[0xB0, b]);
            i += 2;
            // out dx, al
            sector[i] = 0xEE;
            i += 1;
        }

        // hlt
        sector[i] = 0xF4;

        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    fn build_paged_serial_boot_sector(message: &[u8]) -> [u8; 512] {
        assert!(!message.is_empty());
        assert!(message.len() <= 32, "test boot sector message too long");

        // Identity-map the code page (0x7000) so execution continues after enabling paging.
        //
        // Map a separate linear page (0x4000) to a different physical page (0x2000) containing
        // the output message. If paging is not active, the guest will read from physical 0x4000
        // instead and the serial output will not match.
        const PD_BASE: u16 = 0x1000;
        const PT_BASE: u16 = 0x3000;
        const MSG_PHYS_BASE: u16 = 0x2000;
        const MSG_LINEAR_BASE: u16 = 0x4000;

        let mut sector = [0u8; 512];
        let mut i = 0usize;

        // xor ax, ax
        sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
        i += 2;
        // mov ds, ax
        sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
        i += 2;

        // Write the message bytes into a physical RAM page (MSG_PHYS_BASE).
        for (off, &b) in message.iter().enumerate() {
            let addr = MSG_PHYS_BASE.wrapping_add(off as u16);
            // mov byte ptr [addr], imm8
            sector[i..i + 5].copy_from_slice(&[0xC6, 0x06, addr as u8, (addr >> 8) as u8, b]);
            i += 5;
        }

        // PDE[0] -> page table at PT_BASE (present + RW).
        let pde0: u32 = (PT_BASE as u32) | 0x3;
        // 66 c7 06 <disp16> <imm32>
        sector[i..i + 9].copy_from_slice(&[
            0x66,
            0xC7,
            0x06,
            (PD_BASE & 0xFF) as u8,
            (PD_BASE >> 8) as u8,
            (pde0 & 0xFF) as u8,
            ((pde0 >> 8) & 0xFF) as u8,
            ((pde0 >> 16) & 0xFF) as u8,
            ((pde0 >> 24) & 0xFF) as u8,
        ]);
        i += 9;

        // PTE[MSG_LINEAR_BASE >> 12] -> MSG_PHYS_BASE (present + RW).
        let pte_msg_off = PT_BASE.wrapping_add(((MSG_LINEAR_BASE as u32 >> 12) * 4) as u16);
        let pte_msg: u32 = (MSG_PHYS_BASE as u32) | 0x3;
        sector[i..i + 9].copy_from_slice(&[
            0x66,
            0xC7,
            0x06,
            (pte_msg_off & 0xFF) as u8,
            (pte_msg_off >> 8) as u8,
            (pte_msg & 0xFF) as u8,
            ((pte_msg >> 8) & 0xFF) as u8,
            ((pte_msg >> 16) & 0xFF) as u8,
            ((pte_msg >> 24) & 0xFF) as u8,
        ]);
        i += 9;

        // PTE[0x7000 >> 12] -> 0x7000 (code page identity map; present + RW).
        let pte_code_off = PT_BASE.wrapping_add(((0x7000u32 >> 12) * 4) as u16);
        let pte_code: u32 = 0x7000 | 0x3;
        sector[i..i + 9].copy_from_slice(&[
            0x66,
            0xC7,
            0x06,
            (pte_code_off & 0xFF) as u8,
            (pte_code_off >> 8) as u8,
            (pte_code & 0xFF) as u8,
            ((pte_code >> 8) & 0xFF) as u8,
            ((pte_code >> 16) & 0xFF) as u8,
            ((pte_code >> 24) & 0xFF) as u8,
        ]);
        i += 9;

        // mov eax, PD_BASE (32-bit immediate)
        sector[i..i + 6].copy_from_slice(&[
            0x66,
            0xB8,
            (PD_BASE & 0xFF) as u8,
            (PD_BASE >> 8) as u8,
            0x00,
            0x00,
        ]);
        i += 6;
        // mov cr3, eax
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xD8]);
        i += 3;

        // mov eax, cr0
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x20, 0xC0]);
        i += 3;
        // or eax, 0x8000_0000
        sector[i..i + 6].copy_from_slice(&[0x66, 0x0D, 0x00, 0x00, 0x00, 0x80]);
        i += 6;
        // mov cr0, eax
        sector[i..i + 3].copy_from_slice(&[0x0F, 0x22, 0xC0]);
        i += 3;

        // mov dx, 0x3f8
        sector[i..i + 3].copy_from_slice(&[0xBA, 0xF8, 0x03]);
        i += 3;

        for (off, _) in message.iter().enumerate() {
            let addr = MSG_LINEAR_BASE.wrapping_add(off as u16);
            // mov al, moffs8
            sector[i..i + 3].copy_from_slice(&[0xA0, addr as u8, (addr >> 8) as u8]);
            i += 3;
            // out dx, al
            sector[i] = 0xEE;
            i += 1;
        }

        // hlt
        sector[i] = 0xF4;

        sector[510] = 0x55;
        sector[511] = 0xAA;
        sector
    }

    #[test]
    fn boots_mbr_and_writes_to_serial() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        let boot = build_serial_boot_sector(b"OK\n");
        m.set_disk_image(boot.to_vec()).unwrap();
        m.reset();

        for _ in 0..100 {
            match m.run_slice(10_000) {
                RunExit::Halted { .. } => break,
                RunExit::Completed { .. } => continue,
                other => panic!("unexpected exit: {other:?}"),
            }
        }

        let out = m.take_serial_output();
        assert_eq!(out, b"OK\n");
    }

    #[test]
    fn snapshot_restore_drops_network_backend_even_when_restoring_via_snapshot_crate() {
        struct DropBackend {
            dropped: Arc<AtomicUsize>,
        }

        impl aero_net_backend::NetworkBackend for DropBackend {
            fn transmit(&mut self, _frame: Vec<u8>) {}
        }

        impl Drop for DropBackend {
            fn drop(&mut self) {
                self.dropped.fetch_add(1, Ordering::SeqCst);
            }
        }

        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };
        let mut m = Machine::new(cfg).unwrap();
        let snap = m.take_snapshot_full().unwrap();

        let dropped = Arc::new(AtomicUsize::new(0));
        m.set_network_backend(Box::new(DropBackend {
            dropped: dropped.clone(),
        }));

        // Restore via the snapshot crate directly (bypasses `Machine::restore_snapshot_*` helpers).
        snapshot::restore_snapshot(&mut Cursor::new(&snap), &mut m).unwrap();
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn paging_translation_and_io_work_together() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        let boot = build_paged_serial_boot_sector(b"OK\n");
        m.set_disk_image(boot.to_vec()).unwrap();
        m.reset();

        for _ in 0..200 {
            match m.run_slice(10_000) {
                RunExit::Halted { .. } => break,
                RunExit::Completed { .. } => continue,
                other => panic!("unexpected exit: {other:?}"),
            }
        }

        let out = m.take_serial_output();
        assert_eq!(out, b"OK\n");
    }

    #[test]
    fn snapshot_restore_syncs_time_source_with_ia32_tsc() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.time.set_tsc(0x1234);
        src.cpu.state.msr.tsc = 0x1234;
        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        assert_eq!(restored.cpu.state.msr.tsc, 0x1234);
        assert_eq!(restored.cpu.time.read_tsc(), 0x1234);
    }

    #[test]
    fn snapshot_restore_roundtrips_cpu_internal_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.pending.inhibit_interrupts_for_one_instruction();
        src.cpu.pending.inject_external_interrupt(0x20);
        src.cpu.pending.inject_external_interrupt(0x21);
        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.cpu.pending.set_interrupt_inhibit(0);
        restored.cpu.pending.inject_external_interrupt(0x33);
        restored.cpu.pending.raise_software_interrupt(0x80, 0);
        restored.restore_snapshot_bytes(&snap).unwrap();

        assert!(!restored.cpu.pending.has_pending_event());
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 1);
        assert_eq!(
            restored
                .cpu
                .pending
                .external_interrupts
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![0x20, 0x21]
        );
    }

    fn strip_cpu_internal_device_state(bytes: &[u8]) -> Vec<u8> {
        const FILE_HEADER_LEN: usize = 16;
        const SECTION_HEADER_LEN: usize = 16;

        let mut r = Cursor::new(bytes);
        let mut file_header = [0u8; FILE_HEADER_LEN];
        r.read_exact(&mut file_header).unwrap();

        let mut out = Vec::with_capacity(bytes.len());
        out.extend_from_slice(&file_header);

        let mut removed = 0usize;

        while (r.position() as usize) < bytes.len() {
            let mut section_header = [0u8; SECTION_HEADER_LEN];
            // Valid snapshots end cleanly at EOF.
            if let Err(e) = r.read_exact(&mut section_header) {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    break;
                }
                panic!("failed to read section header: {e}");
            }

            let id = u32::from_le_bytes(section_header[0..4].try_into().unwrap());
            let version = u16::from_le_bytes(section_header[4..6].try_into().unwrap());
            let flags = u16::from_le_bytes(section_header[6..8].try_into().unwrap());
            let len = u64::from_le_bytes(section_header[8..16].try_into().unwrap());

            let mut payload = vec![0u8; len as usize];
            r.read_exact(&mut payload).unwrap();

            if id != snapshot::SectionId::DEVICES.0 {
                out.extend_from_slice(&section_header);
                out.extend_from_slice(&payload);
                continue;
            }

            let mut pr = Cursor::new(&payload);
            let mut count_bytes = [0u8; 4];
            pr.read_exact(&mut count_bytes).unwrap();
            let count = u32::from_le_bytes(count_bytes) as usize;

            let mut kept = Vec::new();
            for _ in 0..count {
                let mut dev_header = [0u8; 16];
                pr.read_exact(&mut dev_header).unwrap();
                let dev_id = u32::from_le_bytes(dev_header[0..4].try_into().unwrap());
                let dev_len = u64::from_le_bytes(dev_header[8..16].try_into().unwrap());
                let mut dev_data = vec![0u8; dev_len as usize];
                pr.read_exact(&mut dev_data).unwrap();

                if dev_id == snapshot::DeviceId::CPU_INTERNAL.0 {
                    removed += 1;
                    continue;
                }

                let mut bytes = Vec::with_capacity(dev_header.len() + dev_data.len());
                bytes.extend_from_slice(&dev_header);
                bytes.extend_from_slice(&dev_data);
                kept.push(bytes);
            }

            assert_eq!(
                pr.position() as usize,
                payload.len(),
                "devices section parse did not consume full payload"
            );

            let mut new_payload = Vec::new();
            let new_count: u32 = kept.len().try_into().unwrap();
            new_payload.extend_from_slice(&new_count.to_le_bytes());
            for dev in kept {
                new_payload.extend_from_slice(&dev);
            }
            let new_len: u64 = new_payload.len().try_into().unwrap();

            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&version.to_le_bytes());
            out.extend_from_slice(&flags.to_le_bytes());
            out.extend_from_slice(&new_len.to_le_bytes());
            out.extend_from_slice(&new_payload);
        }

        assert!(removed > 0, "snapshot did not contain a CPU_INTERNAL entry");
        out
    }

    #[test]
    fn restore_snapshot_without_cpu_internal_clears_pending_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.pending.set_interrupt_inhibit(7);
        src.cpu.pending.inject_external_interrupt(0x20);
        src.cpu.pending.inject_external_interrupt(0x21);
        let snap = src.take_snapshot_full().unwrap();
        let snap_without_cpu_internal = strip_cpu_internal_device_state(&snap);

        let mut restored = Machine::new(cfg).unwrap();
        restored.cpu.pending.set_interrupt_inhibit(1);
        restored.cpu.pending.inject_external_interrupt(0x33);
        restored.cpu.pending.raise_software_interrupt(0x80, 0);

        restored
            .restore_snapshot_bytes(&snap_without_cpu_internal)
            .unwrap();

        assert!(!restored.cpu.pending.has_pending_event());
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 0);
        assert!(restored.cpu.pending.external_interrupts.is_empty());
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 0);
    }

    #[test]
    fn snapshot_restore_preserves_cpu_internal_interrupt_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.cpu.pending.inject_external_interrupt(0x20);
        src.cpu.pending.inject_external_interrupt(0x21);
        src.cpu.pending.inhibit_interrupts_for_one_instruction();

        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        // Ensure restore does not merge with pre-existing state.
        restored.cpu.pending.inject_external_interrupt(0x99);
        restored.cpu.pending.set_interrupt_inhibit(0);

        restored.restore_snapshot_bytes(&snap).unwrap();

        let restored_irqs: Vec<u8> = restored
            .cpu
            .pending
            .external_interrupts
            .iter()
            .copied()
            .collect();
        assert_eq!(restored_irqs, vec![0x20, 0x21]);
        assert_eq!(restored.cpu.pending.interrupt_inhibit(), 1);
    }

    #[test]
    fn inject_keyboard_and_mouse_produces_i8042_output_bytes() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();
        let ctrl = m.i8042.as_ref().expect("i8042 enabled").clone();

        m.inject_browser_key("KeyA", true);
        m.inject_browser_key("KeyA", false);

        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x1e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x9e);

        // Enable mouse reporting so injected motion generates stream packets.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD4);
            dev.write_port(0x60, 0xF4);
        }
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xFA); // ACK

        m.inject_mouse_motion(10, 5, 0);
        let packet: Vec<u8> = (0..3).map(|_| ctrl.borrow_mut().read_port(0x60)).collect();
        assert_eq!(packet, vec![0x28, 10, 0xFB]);
    }

    #[test]
    fn snapshot_restore_preserves_i8042_pending_output_bytes() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        src.inject_browser_key("KeyA", true);
        src.inject_browser_key("KeyA", false);
        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        let ctrl = restored.i8042.as_ref().expect("i8042 enabled").clone();
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x1e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x9e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x00);
    }

    #[test]
    fn snapshot_restore_preserves_i8042_output_port_and_pending_write() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        let ctrl = src.i8042.as_ref().expect("i8042 enabled").clone();
        {
            let mut dev = ctrl.borrow_mut();
            // Set an initial output-port value.
            dev.write_port(0x64, 0xD1);
            dev.write_port(0x60, 0x03);

            // Leave an in-flight "write output port" pending write.
            dev.write_port(0x64, 0xD1);
        }

        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&snap).unwrap();

        let ctrl = restored.i8042.as_ref().expect("i8042 enabled").clone();
        let mut dev = ctrl.borrow_mut();

        // Verify output port preserved.
        dev.write_port(0x64, 0xD0);
        assert_eq!(dev.read_port(0x60), 0x03);

        // Verify pending write preserved and targets the output port.
        dev.write_port(0x60, 0x01);
        dev.write_port(0x64, 0xD0);
        assert_eq!(dev.read_port(0x60), 0x01);
    }

    #[test]
    fn restoring_i8042_state_resynchronizes_platform_a20() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let src = Machine::new(cfg.clone()).unwrap();
        let ctrl = src.i8042.as_ref().expect("i8042 enabled").clone();

        // Save a snapshot with A20 disabled in the controller output port.
        {
            let mut dev = ctrl.borrow_mut();
            dev.write_port(0x64, 0xD1);
            dev.write_port(0x60, 0x01);
        }
        assert!(!src.chipset.a20().enabled());

        let state = {
            let dev = ctrl.borrow();
            snapshot::io_snapshot_bridge::device_state_from_io_snapshot(
                snapshot::DeviceId::I8042,
                &*dev,
            )
        };

        // Simulate restoring into an environment where A20 is currently enabled.
        let mut restored = Machine::new(cfg).unwrap();
        restored.chipset.a20().set_enabled(true);
        assert!(restored.chipset.a20().enabled());

        snapshot::SnapshotTarget::restore_device_states(&mut restored, vec![state]);

        assert!(!restored.chipset.a20().enabled());
    }

    #[test]
    fn i8042_injection_apis_are_noops_when_disabled() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            enable_i8042: false,
            ..Default::default()
        })
        .unwrap();

        // Should not panic.
        m.inject_browser_key("KeyA", true);
        m.inject_mouse_motion(1, 2, 3);
        m.inject_mouse_button(Ps2MouseButton::Left, true);

        assert!(m.i8042.is_none());
        let devices = snapshot::SnapshotSource::device_states(&m);
        assert!(
            devices.iter().all(|d| d.id != snapshot::DeviceId::I8042),
            "i8042 device state should not be emitted when disabled"
        );
    }

    #[test]
    fn dirty_snapshot_roundtrip_preserves_i8042_pending_output_bytes() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut vm = Machine::new(cfg.clone()).unwrap();
        vm.inject_browser_key("KeyA", true);
        vm.inject_browser_key("KeyA", false);
        let base = vm.take_snapshot_full().unwrap();

        vm.inject_browser_key("KeyB", true);
        vm.inject_browser_key("KeyB", false);
        let diff = vm.take_snapshot_dirty().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&base).unwrap();
        restored.restore_snapshot_bytes(&diff).unwrap();

        let ctrl = restored.i8042.as_ref().expect("i8042 enabled").clone();
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x1e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x9e);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x30);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0xB0);
        assert_eq!(ctrl.borrow_mut().read_port(0x60), 0x00);
    }

    #[test]
    fn dirty_tracking_includes_device_writes_to_ram() {
        let mut m = Machine::new(MachineConfig {
            ram_size_bytes: 16 * 1024 * 1024,
            ..Default::default()
        })
        .unwrap();

        // `Machine::new` performs a reset which clears dirty pages.
        assert!(m.mem.take_dirty_pages().is_empty());

        // Simulate a DMA/device write by bypassing the CPU memory wrapper and writing directly to
        // the underlying physical bus.
        m.mem
            .inner
            .borrow_mut()
            .write_physical(0x2000, &[0xAA, 0xBB, 0xCC, 0xDD]);

        assert_eq!(m.mem.take_dirty_pages(), vec![2]);

        // Drain semantics.
        assert!(m.mem.take_dirty_pages().is_empty());
    }

    #[test]
    fn dirty_snapshot_includes_device_writes_to_ram() {
        let cfg = MachineConfig {
            ram_size_bytes: 16 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        let base = src.take_snapshot_full().unwrap();

        // Simulate a DMA/device write by bypassing `SystemMemory` and writing directly to the
        // physical bus RAM backend.
        let addr = 0x2000u64;
        let data = [0xAAu8, 0xBB, 0xCC, 0xDD];
        src.mem.inner.borrow_mut().write_physical(addr, &data);

        // Take a dirty snapshot diff and ensure the restored VM observes the change.
        let diff = src.take_snapshot_dirty().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.restore_snapshot_bytes(&base).unwrap();
        restored.restore_snapshot_bytes(&diff).unwrap();

        assert_eq!(restored.read_physical_bytes(addr, data.len()), data);
    }
}
