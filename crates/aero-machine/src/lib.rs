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
use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_cpu_core::{AssistReason, CpuCore, Exception};
use aero_devices::a20_gate::A20Gate as A20GateDevice;
use aero_devices::i8042::{I8042Ports, SharedI8042Controller};
use aero_devices::reset_ctrl::{ResetCtrl, RESET_CTRL_PORT};
use aero_devices::serial::{register_serial16550, Serial16550, SharedSerial16550};
use aero_platform::chipset::{A20GateHandle, ChipsetState};
use aero_platform::io::IoPortBus;
use aero_platform::reset::{ResetKind, ResetLatch};
use aero_snapshot as snapshot;
use firmware::bios::{A20Gate, Bios, BiosBus, BiosConfig, BlockDevice, DiskError, FirmwareMemory};
use memory::{DenseMemory, MapError, MemoryBus as _, PhysicalMemoryBus};

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

    fn mark_range(&mut self, start: u64, len: usize) {
        if len == 0 {
            return;
        }
        let len_u64 = len as u64;
        let end = start.saturating_add(len_u64).saturating_sub(1);
        let first_page = usize::try_from(start / self.page_size as u64).unwrap_or(usize::MAX);
        let last_page = usize::try_from(end / self.page_size as u64).unwrap_or(usize::MAX);
        if first_page >= self.pages {
            return;
        }
        let last_page = last_page.min(self.pages.saturating_sub(1));
        for page in first_page..=last_page {
            let word = page / 64;
            let bit = page % 64;
            if let Some(slot) = self.bits.get_mut(word) {
                *slot |= 1u64 << bit;
            }
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
        let inner = PhysicalMemoryBus::new(Box::new(ram));

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
            self.dirty.mark_range(paddr, buf.len());
            return;
        }

        let mut inner = self.inner.borrow_mut();
        for (i, byte) in buf.iter().copied().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            inner.write_physical_u8(addr, byte);
            self.dirty.mark_addr(addr);
        }
    }
}

struct Bus<'a> {
    mem: &'a mut SystemMemory,
    io: &'a mut IoPortBus,
}

impl CpuBus for Bus<'_> {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        Ok(self.mem.read_u8(vaddr))
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        Ok(self.mem.read_u16(vaddr))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        Ok(self.mem.read_u32(vaddr))
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        Ok(self.mem.read_u64(vaddr))
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        Ok(self.mem.read_u128(vaddr))
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        self.mem.write_u8(vaddr, val);
        Ok(())
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.mem.write_u16(vaddr, val);
        Ok(())
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.mem.write_u32(vaddr, val);
        Ok(())
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.mem.write_u64(vaddr, val);
        Ok(())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.mem.write_u128(vaddr, val);
        Ok(())
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        for i in 0..len {
            buf[i] = self.mem.read_u8(vaddr.wrapping_add(i as u64));
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
        self.cpu = CpuCore::new(CpuMode::Real);

        // Run firmware POST (in Rust) to initialize IVT/BDA, map BIOS stubs, and load the boot
        // sector into RAM.
        self.bios = Bios::new(BiosConfig {
            memory_size_bytes: self.cfg.ram_size_bytes,
            cpu_count: self.cfg.cpu_count,
            ..Default::default()
        });
        let bus: &mut dyn BiosBus = &mut self.mem;
        self.bios.post(&mut self.cpu.state, bus, &mut self.disk);
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

            let remaining = max_insts - executed;
            let mut bus = Bus {
                mem: &mut self.mem,
                io: &mut self.io,
            };

            let batch = run_batch_cpu_core_with_assists(
                &cfg,
                &mut self.assist,
                &mut self.cpu,
                &mut bus,
                remaining,
            );
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
        self.cpu.time.set_tsc(self.cpu.state.msr.tsc);
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
                    self.cpu.state.a20_enabled = enabled;
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
        self.cpu.pending = Default::default();
        self.mem.clear_dirty();
        self.cpu.state.a20_enabled = self.chipset.a20().enabled();
        Ok(())
    }
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
    fn snapshot_restore_clears_cpu_pending_state() {
        let cfg = MachineConfig {
            ram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };

        let mut src = Machine::new(cfg.clone()).unwrap();
        let snap = src.take_snapshot_full().unwrap();

        let mut restored = Machine::new(cfg).unwrap();
        restored.cpu.pending.raise_software_interrupt(0x80, 0);
        restored.cpu.pending.inject_external_interrupt(0x20);
        assert!(restored.cpu.pending.has_pending_event());
        assert!(!restored.cpu.pending.external_interrupts.is_empty());

        restored.restore_snapshot_bytes(&snap).unwrap();

        assert!(!restored.cpu.pending.has_pending_event());
        assert!(restored.cpu.pending.external_interrupts.is_empty());
    }
}
