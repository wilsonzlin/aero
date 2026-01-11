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
#![allow(deprecated)]

use std::cell::RefCell;
use std::fmt;
use std::io::{Cursor, Read, Seek, Write};
use std::rc::Rc;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_with_assists, BatchExit};
use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::state::{gpr, CpuMode, CpuState, Segment as CoreSegment};
use aero_cpu_core::{AssistReason, CpuCore, Exception};
use aero_devices::a20_gate::A20Gate as A20GateDevice;
use aero_devices::i8042::{I8042Ports, SharedI8042Controller};
use aero_devices::reset_ctrl::{ResetCtrl, RESET_CTRL_PORT};
use aero_devices::serial::{register_serial16550, Serial16550, SharedSerial16550};
use aero_platform::chipset::{A20GateHandle, ChipsetState};
use aero_platform::io::IoPortBus;
use aero_platform::reset::{ResetKind, ResetLatch};
use aero_snapshot as snapshot;
use firmware::bios::{build_bios_rom, Bios, BiosBus, BiosConfig, BIOS_BASE};
use machine::{
    A20Gate as FirmwareA20Gate, BlockDevice, CpuState as FirmwareCpuState, FirmwareMemory,
    MemoryAccess, Segment as FirmwareSegment,
};
use memory::{DenseMemory, PhysicalMemoryBus};

const FAST_A20_PORT: u16 = 0x92;
const SNAPSHOT_DIRTY_PAGE_SIZE: u32 = 4096;

/// Configuration for [`Machine`].
#[derive(Debug, Clone)]
pub struct MachineConfig {
    /// Guest RAM size in bytes.
    pub ram_size_bytes: u64,
    /// Number of vCPUs (currently must be 1).
    pub cpu_count: u8,
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
}

impl RunExit {
    /// Number of guest instructions executed in this slice (best-effort).
    pub fn executed(&self) -> u64 {
        match *self {
            RunExit::Completed { executed }
            | RunExit::Halted { executed }
            | RunExit::ResetRequested { executed, .. }
            | RunExit::Assist { executed, .. }
            | RunExit::Exception { executed, .. } => executed,
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
        if data.len() % 512 != 0 {
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
    fn read_sector(&mut self, lba: u64, buf: &mut [u8; 512]) -> Result<(), machine::DiskError> {
        let idx = usize::try_from(lba).map_err(|_| machine::DiskError::OutOfRange)?;
        let start = idx.checked_mul(512).ok_or(machine::DiskError::OutOfRange)?;
        let end = start
            .checked_add(512)
            .ok_or(machine::DiskError::OutOfRange)?;
        let src = self
            .data
            .get(start..end)
            .ok_or(machine::DiskError::OutOfRange)?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn size_in_sectors(&self) -> u64 {
        (self.data.len() / 512) as u64
    }
}

#[derive(Debug, Clone)]
struct DirtyBitmap {
    bits: Vec<u64>,
    pages: usize,
    page_size: usize,
}

impl DirtyBitmap {
    fn new(mem_len: u64, page_size: u32) -> Self {
        let page_size = page_size as usize;
        let pages = usize::try_from(
            mem_len
                .checked_add((page_size as u64).saturating_sub(1))
                .unwrap_or(mem_len)
                / (page_size as u64),
        )
        .unwrap_or(0);
        let words = pages.div_ceil(64);
        Self {
            bits: vec![0u64; words],
            pages,
            page_size,
        }
    }

    fn mark_addr(&mut self, addr: u64) {
        let page = usize::try_from(addr / self.page_size as u64).unwrap_or(usize::MAX);
        if page >= self.pages {
            return;
        }
        let word = page / 64;
        let bit = page % 64;
        if let Some(slot) = self.bits.get_mut(word) {
            *slot |= 1u64 << bit;
        }
    }

    fn take(&mut self) -> Vec<u64> {
        let mut pages = Vec::new();
        for (word_idx, word) in self.bits.iter_mut().enumerate() {
            let mut w = *word;
            if w == 0 {
                continue;
            }
            *word = 0;
            while w != 0 {
                let bit = w.trailing_zeros() as usize;
                let page = word_idx * 64 + bit;
                if page < self.pages {
                    pages.push(page as u64);
                }
                w &= !(1u64 << bit);
            }
        }
        pages
    }

    fn clear(&mut self) {
        self.bits.fill(0);
    }
}

struct SystemMemory {
    a20: A20GateHandle,
    inner: RefCell<PhysicalMemoryBus>,
    dirty: DirtyBitmap,
}

impl SystemMemory {
    fn new(ram_size_bytes: u64, a20: A20GateHandle) -> Result<Self, MachineError> {
        let ram = DenseMemory::new(ram_size_bytes)
            .map_err(|_| MachineError::GuestMemoryTooLarge(ram_size_bytes))?;
        let mut inner = PhysicalMemoryBus::new(Box::new(ram));

        // Map the BIOS ROM at the conventional window, and also at the 4GiB alias used by the
        // architectural reset vector.
        let rom: Arc<[u8]> = build_bios_rom().into();
        let _ = inner.map_rom(BIOS_BASE, rom.clone());
        let _ = inner.map_rom(0xFFFF_0000, rom);

        Ok(Self {
            a20,
            inner: RefCell::new(inner),
            dirty: DirtyBitmap::new(ram_size_bytes, SNAPSHOT_DIRTY_PAGE_SIZE),
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
        self.dirty.take()
    }

    fn clear_dirty(&mut self) {
        self.dirty.clear();
    }
}

impl FirmwareA20Gate for SystemMemory {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20.set_enabled(enabled);
    }

    fn a20_enabled(&self) -> bool {
        self.a20.enabled()
    }
}

impl FirmwareMemory for SystemMemory {
    fn map_rom(&mut self, base: u64, rom: &[u8]) {
        let rom: Arc<[u8]> = rom.to_vec().into();
        let _ = self.inner.borrow_mut().map_rom(base, rom);
    }
}

impl MemoryAccess for SystemMemory {
    fn read_u8(&self, addr: u64) -> u8 {
        let addr = self.translate_a20(addr);
        self.inner.borrow_mut().read_physical_u8(addr)
    }

    fn write_u8(&mut self, addr: u64, val: u8) {
        let addr = self.translate_a20(addr);
        self.inner.borrow_mut().write_physical_u8(addr, val);
        self.dirty.mark_addr(addr);
    }

    fn fetch_code(&self, _addr: u64, _len: usize) -> &[u8] {
        &[]
    }
}

struct Bus<'a> {
    mem: &'a mut SystemMemory,
    io: &'a mut IoPortBus,
}

impl CpuBus for Bus<'_> {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        Ok(MemoryAccess::read_u8(&*self.mem, vaddr))
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        Ok(MemoryAccess::read_u16(&*self.mem, vaddr))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        Ok(MemoryAccess::read_u32(&*self.mem, vaddr))
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        Ok(MemoryAccess::read_u64(&*self.mem, vaddr))
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        Ok(MemoryAccess::read_u128(&*self.mem, vaddr))
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        MemoryAccess::write_u8(self.mem, vaddr, val);
        Ok(())
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        MemoryAccess::write_u16(self.mem, vaddr, val);
        Ok(())
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        MemoryAccess::write_u32(self.mem, vaddr, val);
        Ok(())
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        MemoryAccess::write_u64(self.mem, vaddr, val);
        Ok(())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        MemoryAccess::write_u128(self.mem, vaddr, val);
        Ok(())
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        for i in 0..len {
            buf[i] = MemoryAccess::read_u8(&*self.mem, vaddr + i as u64);
        }
        Ok(buf)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        Ok(self.io.read(port, size as u8) as u64)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        self.io.write(port, size as u8, val as u32);
        Ok(())
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
    bios: Bios,
    disk: VecBlockDevice,

    serial: Option<SharedSerial16550>,
    i8042: Option<SharedI8042Controller>,
    serial_log: Vec<u8>,

    next_snapshot_id: u64,
    last_snapshot_id: Option<u64>,
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
            bios: Bios::new(BiosConfig::default()),
            disk: VecBlockDevice::new(Vec::new()).expect("empty disk is valid"),
            serial: None,
            i8042: None,
            serial_log: Vec::new(),
            next_snapshot_id: 1,
            last_snapshot_id: None,
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

    /// Take (drain) all serial output accumulated so far.
    pub fn take_serial_output(&mut self) -> Vec<u8> {
        self.flush_serial();
        std::mem::take(&mut self.serial_log)
    }

    /// Inject a browser-style keyboard code into the i8042 controller, if present.
    pub fn inject_browser_key(&mut self, code: &str, pressed: bool) {
        if let Some(ctrl) = &self.i8042 {
            ctrl.borrow_mut().inject_browser_key(code, pressed);
        }
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
        self.flush_serial();
        if let Some(uart) = &self.serial {
            let _ = uart.borrow_mut().take_tx();
        }
        self.serial_log.clear();
        self.reset_latch.clear();

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

        // Reset chipset lines.
        self.chipset.a20().set_enabled(false);

        // Rebuild port I/O devices for deterministic power-on state.
        self.io = IoPortBus::new();

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

        // Run firmware POST (in Rust) to initialize IVT/BDA, map BIOS stubs, and load the boot
        // sector into RAM.
        self.bios = Bios::new(BiosConfig {
            memory_size_bytes: self.cfg.ram_size_bytes,
            cpu_count: self.cfg.cpu_count,
            ..Default::default()
        });
        let mut fw_cpu = FirmwareCpuState::default();
        let bus: &mut dyn BiosBus = &mut self.mem;
        self.bios.post(&mut fw_cpu, bus, &mut self.disk);

        // Reset non-ABI CPU bookkeeping (pending events + virtual time) before syncing the
        // firmware-provided register state back into the Tier-0 core state.
        self.cpu = CpuCore::new(CpuMode::Real);
        sync_firmware_to_core(&fw_cpu, &mut self.cpu.state);
        self.cpu.state.halted = false;
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
        self.mem.clear_dirty();
    }

    /// Run the CPU for at most `max_insts` guest instructions.
    pub fn run_slice(&mut self, max_insts: u64) -> RunExit {
        let mut executed = 0u64;
        while executed < max_insts {
            if let Some(kind) = self.reset_latch.take() {
                self.flush_serial();
                return RunExit::ResetRequested { kind, executed };
            }

            // Keep the core's A20 view coherent with the chipset latch.
            self.cpu.state.a20_enabled = self.chipset.a20().enabled();

            let remaining = max_insts - executed;
            let mut bus = Bus {
                mem: &mut self.mem,
                io: &mut self.io,
            };

            let batch =
                run_batch_with_assists(&mut self.assist, &mut self.cpu, &mut bus, remaining);
            executed = executed.saturating_add(batch.executed);

            match batch.exit {
                BatchExit::Completed => {
                    self.flush_serial();
                    return RunExit::Completed { executed };
                }
                BatchExit::Branch => continue,
                BatchExit::Halted => {
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
            }
        }

        self.flush_serial();
        RunExit::Completed { executed }
    }

    fn handle_bios_interrupt(&mut self, vector: u8) {
        let mut fw_cpu = FirmwareCpuState::default();
        sync_core_to_firmware(&self.cpu.state, &mut fw_cpu);
        let bus: &mut dyn BiosBus = &mut self.mem;
        self.bios
            .dispatch_interrupt(vector, &mut fw_cpu, bus, &mut self.disk);
        sync_firmware_to_core(&fw_cpu, &mut self.cpu.state);
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
        let bios_snapshot = self.bios.snapshot(&self.mem);
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
        snapshot::apply_cpu_state_to_cpu_core(&state, &mut self.cpu);
    }

    fn restore_mmu_state(&mut self, state: snapshot::MmuState) {
        snapshot::apply_mmu_state_to_cpu_core(&state, &mut self.cpu);
    }

    fn restore_device_states(&mut self, states: Vec<snapshot::DeviceState>) {
        for state in states {
            match state.id {
                snapshot::DeviceId::BIOS => {
                    if state.version != 1 {
                        continue;
                    }
                    let snapshot =
                        match firmware::bios::BiosSnapshot::decode(&mut Cursor::new(&state.data)) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };
                    self.bios.restore_snapshot(snapshot, &mut self.mem);
                }
                snapshot::DeviceId::MEMORY => {
                    if state.version != 1 {
                        continue;
                    }
                    let enabled = state.data.first().copied().unwrap_or(0) != 0;
                    self.chipset.a20().set_enabled(enabled);
                    self.cpu.a20_enabled = enabled;
                }
                snapshot::DeviceId::SERIAL => {
                    if state.version != 1 {
                        continue;
                    }
                    if let Some(uart) = &self.serial {
                        let _ = uart.borrow_mut().take_tx();
                    }
                    self.serial_log = state.data;
                }
                _ => {}
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
        self.reset_latch.clear();
        self.assist = AssistContext::default();
        self.mem.clear_dirty();
        self.cpu.a20_enabled = self.chipset.a20().enabled();
        Ok(())
    }
}

fn set_real_mode_seg(seg: &mut CoreSegment, selector: u16) {
    seg.selector = selector;
    seg.base = (selector as u64) << 4;
    seg.limit = 0xFFFF;
    seg.access = 0;
}

fn sync_firmware_to_core(fw: &FirmwareCpuState, core: &mut CpuState) {
    core.mode = CpuMode::Real;
    core.halted = fw.halted;
    core.clear_pending_bios_int();

    core.gpr[gpr::RAX] = fw.rax;
    core.gpr[gpr::RCX] = fw.rcx;
    core.gpr[gpr::RDX] = fw.rdx;
    core.gpr[gpr::RBX] = fw.rbx;
    core.gpr[gpr::RSP] = fw.rsp;
    core.gpr[gpr::RBP] = fw.rbp;
    core.gpr[gpr::RSI] = fw.rsi;
    core.gpr[gpr::RDI] = fw.rdi;
    core.gpr[gpr::R8] = 0;
    core.gpr[gpr::R9] = 0;
    core.gpr[gpr::R10] = 0;
    core.gpr[gpr::R11] = 0;
    core.gpr[gpr::R12] = 0;
    core.gpr[gpr::R13] = 0;
    core.gpr[gpr::R14] = 0;
    core.gpr[gpr::R15] = 0;

    core.set_rip(fw.rip);
    core.set_rflags(fw.rflags);

    set_real_mode_seg(&mut core.segments.cs, fw.cs.selector);
    set_real_mode_seg(&mut core.segments.ds, fw.ds.selector);
    set_real_mode_seg(&mut core.segments.es, fw.es.selector);
    set_real_mode_seg(&mut core.segments.ss, fw.ss.selector);
    set_real_mode_seg(&mut core.segments.fs, 0);
    set_real_mode_seg(&mut core.segments.gs, 0);
}

fn sync_core_to_firmware(core: &CpuState, fw: &mut FirmwareCpuState) {
    fw.rax = core.gpr[gpr::RAX];
    fw.rbx = core.gpr[gpr::RBX];
    fw.rcx = core.gpr[gpr::RCX];
    fw.rdx = core.gpr[gpr::RDX];
    fw.rsi = core.gpr[gpr::RSI];
    fw.rdi = core.gpr[gpr::RDI];
    fw.rbp = core.gpr[gpr::RBP];
    fw.rsp = core.gpr[gpr::RSP];
    fw.rip = core.rip();
    fw.rflags = core.rflags();

    fw.cs = FirmwareSegment {
        selector: core.segments.cs.selector,
    };
    fw.ds = FirmwareSegment {
        selector: core.segments.ds.selector,
    };
    fw.es = FirmwareSegment {
        selector: core.segments.es.selector,
    };
    fw.ss = FirmwareSegment {
        selector: core.segments.ss.selector,
    };

    fw.pending_bios_int = None;
    fw.halted = core.halted;
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

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
}
