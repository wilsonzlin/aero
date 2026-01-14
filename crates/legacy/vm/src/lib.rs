//! Minimal VM wiring for the BIOS firmware tests.
//!
//! This crate is intentionally small and is only used by unit/integration tests.

#![allow(deprecated)]

mod snapshot;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_cpu_core_with_assists, BatchExit};
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::mem::CpuBus;
use aero_cpu_core::state::CpuMode;
use aero_cpu_core::{CpuCore, Exception};
use firmware::bios::{A20Gate, Bios, BiosBus, BlockDevice, FirmwareMemory};
use memory::{
    GuestMemory, GuestMemoryError, GuestMemoryResult, MapError, MemoryBus as _, PhysicalMemoryBus,
};

pub use snapshot::{SnapshotError, SnapshotOptions};

const DIRTY_PAGE_SIZE: usize = 4096;

#[derive(Debug, Clone)]
struct DirtyBitmap {
    bits: Vec<u64>,
    pages: usize,
    page_size: usize,
}

impl DirtyBitmap {
    fn new(mem_len: usize, page_size: usize) -> Self {
        let pages = mem_len.div_ceil(page_size);
        let words = pages.div_ceil(64);
        Self {
            bits: vec![0; words],
            pages,
            page_size,
        }
    }

    fn mark_range(&mut self, start: usize, len: usize) {
        if len == 0 {
            return;
        }
        let end = start.saturating_add(len).saturating_sub(1);
        let first_page = start / self.page_size;
        let last_page = end / self.page_size;
        for page in first_page..=last_page {
            if page >= self.pages {
                break;
            }
            let word = page / 64;
            let bit = page % 64;
            self.bits[word] |= 1u64 << bit;
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

#[derive(Debug)]
struct DirtyRam {
    data: Box<[u8]>,
    dirty: DirtyBitmap,
}

impl DirtyRam {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0u8; size].into_boxed_slice(),
            dirty: DirtyBitmap::new(size, DIRTY_PAGE_SIZE),
        }
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn read_raw(&self, addr: u64, buf: &mut [u8]) {
        let start: usize = addr
            .try_into()
            .unwrap_or_else(|_| panic!("address out of range: 0x{addr:016x}"));
        let end = start
            .checked_add(buf.len())
            .unwrap_or_else(|| panic!("address overflow: 0x{addr:016x}+0x{:x}", buf.len()));
        assert!(
            end <= self.data.len(),
            "raw read out of bounds: 0x{addr:016x}+0x{:x} (mem=0x{:x})",
            buf.len(),
            self.data.len()
        );
        buf.copy_from_slice(&self.data[start..end]);
    }

    fn write_raw(&mut self, addr: u64, buf: &[u8]) {
        let start: usize = addr
            .try_into()
            .unwrap_or_else(|_| panic!("address out of range: 0x{addr:016x}"));
        let end = start
            .checked_add(buf.len())
            .unwrap_or_else(|| panic!("address overflow: 0x{addr:016x}+0x{:x}", buf.len()));
        assert!(
            end <= self.data.len(),
            "raw write out of bounds: 0x{addr:016x}+0x{:x} (mem=0x{:x})",
            buf.len(),
            self.data.len()
        );
        self.data[start..end].copy_from_slice(buf);
    }

    fn write_dirty(&mut self, addr: u64, buf: &[u8]) -> GuestMemoryResult<()> {
        let start: usize = addr.try_into().map_err(|_| GuestMemoryError::OutOfRange {
            paddr: addr,
            len: buf.len(),
            size: self.len() as u64,
        })?;
        let end = start
            .checked_add(buf.len())
            .ok_or(GuestMemoryError::OutOfRange {
                paddr: addr,
                len: buf.len(),
                size: self.len() as u64,
            })?;
        if end > self.data.len() {
            return Err(GuestMemoryError::OutOfRange {
                paddr: addr,
                len: buf.len(),
                size: self.len() as u64,
            });
        }
        self.data[start..end].copy_from_slice(buf);
        self.dirty.mark_range(start, buf.len());
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Vec<u64> {
        self.dirty.take()
    }

    fn clear_dirty(&mut self) {
        self.dirty.clear();
    }
}

#[derive(Debug, Clone)]
struct SharedDirtyRam {
    inner: Rc<RefCell<DirtyRam>>,
}

impl SharedDirtyRam {
    fn new(size: usize) -> Self {
        Self {
            inner: Rc::new(RefCell::new(DirtyRam::new(size))),
        }
    }

    fn len(&self) -> usize {
        self.inner.borrow().len()
    }

    fn read_raw(&self, addr: u64, buf: &mut [u8]) {
        self.inner.borrow().read_raw(addr, buf);
    }

    fn write_raw(&self, addr: u64, buf: &[u8]) {
        self.inner.borrow_mut().write_raw(addr, buf);
    }

    fn take_dirty_pages(&self) -> Vec<u64> {
        self.inner.borrow_mut().take_dirty_pages()
    }

    fn clear_dirty(&self) {
        self.inner.borrow_mut().clear_dirty();
    }
}

impl GuestMemory for SharedDirtyRam {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn read_into(&self, paddr: u64, dst: &mut [u8]) -> GuestMemoryResult<()> {
        let start: usize = paddr.try_into().map_err(|_| GuestMemoryError::OutOfRange {
            paddr,
            len: dst.len(),
            size: self.size(),
        })?;
        let end = start
            .checked_add(dst.len())
            .ok_or(GuestMemoryError::OutOfRange {
                paddr,
                len: dst.len(),
                size: self.size(),
            })?;
        if end > self.len() {
            return Err(GuestMemoryError::OutOfRange {
                paddr,
                len: dst.len(),
                size: self.size(),
            });
        }
        dst.copy_from_slice(&self.inner.borrow().data[start..end]);
        Ok(())
    }

    fn write_from(&mut self, paddr: u64, src: &[u8]) -> GuestMemoryResult<()> {
        self.inner.borrow_mut().write_dirty(paddr, src)
    }
}

/// VM memory implementation:
/// - RAM with dirty tracking (for `aero_snapshot` dirty-page snapshots)
/// - ROM mapping for the BIOS image (via [`FirmwareMemory`])
/// - A20 gating applied to physical reads/writes
pub struct VmMemory {
    a20_enabled: bool,
    ram: SharedDirtyRam,
    inner: PhysicalMemoryBus,
}

impl VmMemory {
    pub fn new(size: usize) -> Self {
        let ram = SharedDirtyRam::new(size);
        let inner = PhysicalMemoryBus::new(Box::new(ram.clone()));
        Self {
            a20_enabled: false,
            ram,
            inner,
        }
    }

    pub fn len(&self) -> usize {
        self.ram.len()
    }

    /// Read bytes directly from the backing store (no A20 translation, no ROM overlays).
    pub fn read_raw(&self, addr: u64, buf: &mut [u8]) {
        self.ram.read_raw(addr, buf);
    }

    /// Write bytes directly into the backing store (no A20 translation, no ROM overlays, does not
    /// mark dirty pages).
    pub fn write_raw(&mut self, addr: u64, buf: &[u8]) {
        self.ram.write_raw(addr, buf);
    }

    pub fn take_dirty_pages(&mut self) -> Vec<u64> {
        self.ram.take_dirty_pages()
    }

    pub fn clear_dirty(&mut self) {
        self.ram.clear_dirty();
    }

    pub fn read_bytes(&mut self, addr: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        self.read_physical(addr, &mut out);
        out
    }

    fn translate_a20(&self, addr: u64) -> u64 {
        if self.a20_enabled {
            addr
        } else {
            addr & !(1u64 << 20)
        }
    }
}

impl firmware::bios::A20Gate for VmMemory {
    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20_enabled = enabled;
    }

    fn a20_enabled(&self) -> bool {
        self.a20_enabled
    }
}

impl FirmwareMemory for VmMemory {
    fn map_rom(&mut self, base: u64, rom: Arc<[u8]>) {
        let len = rom.len();
        match self.inner.map_rom(base, rom) {
            Ok(()) => {}
            Err(MapError::Overlap) => {
                let already_mapped = self
                    .inner
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

impl memory::MemoryBus for VmMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        if self.a20_enabled {
            self.inner.read_physical(paddr, buf);
            return;
        }

        for (i, slot) in buf.iter_mut().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            *slot = self.inner.read_physical_u8(addr);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        if self.a20_enabled {
            self.inner.write_physical(paddr, buf);
            return;
        }

        for (i, byte) in buf.iter().copied().enumerate() {
            let addr = self.translate_a20(paddr.wrapping_add(i as u64));
            self.inner.write_physical_u8(addr, byte);
        }
    }
}

struct VmCpuBus<'a> {
    mem: &'a mut VmMemory,
    serial: &'a mut Vec<u8>,
}

impl CpuBus for VmCpuBus<'_> {
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

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Ok(0)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        // Tiny UART stub: capture COM1 output written by real-mode tests.
        //
        // The legacy `vm` harness does not emulate full UART behaviour. It only records writes to
        // the COM1 data register (0x3F8) so integration tests can assert that guest code executed.
        if port == 0x3F8 {
            match size {
                1 => self.serial.push(val as u8),
                2 => self.serial.extend_from_slice(&(val as u16).to_le_bytes()),
                4 => self.serial.extend_from_slice(&(val as u32).to_le_bytes()),
                8 => self.serial.extend_from_slice(&val.to_le_bytes()),
                _ => return Err(Exception::InvalidOpcode),
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuExit {
    Continue,
    Halt,
    BiosInterrupt(u8),
}

#[deprecated(
    note = "This toy VM was used for early firmware tests; use `aero_machine::Machine` (crates/aero-machine) instead"
)]
pub struct Vm<D: BlockDevice> {
    pub cpu: CpuCore,
    pub mem: VmMemory,
    pub bios: Bios,
    pub disk: D,
    serial: Vec<u8>,
    assist: AssistContext,
    snapshot_seq: u64,
    last_snapshot_id: Option<u64>,
}

impl<D: BlockDevice> Vm<D> {
    pub fn new(mem_size: usize, bios: Bios, disk: D) -> Self {
        Self {
            cpu: CpuCore::new(CpuMode::Real),
            mem: VmMemory::new(mem_size),
            bios,
            disk,
            serial: Vec::new(),
            assist: AssistContext::default(),
            snapshot_seq: 1,
            last_snapshot_id: None,
        }
    }

    pub fn reset(&mut self) {
        self.assist = AssistContext::default();
        self.cpu = CpuCore::new(CpuMode::Real);
        self.cpu.a20_enabled = self.mem.a20_enabled();
        self.serial.clear();

        let bus: &mut dyn BiosBus = &mut self.mem;
        self.bios.post(&mut self.cpu, bus, &mut self.disk, None);

        self.cpu.a20_enabled = self.mem.a20_enabled();
        self.mem.clear_dirty();
    }

    pub fn step(&mut self) -> CpuExit {
        if self.cpu.halted {
            return CpuExit::Halt;
        }

        // Keep the CPU's A20 view coherent with the memory bus latch.
        self.cpu.a20_enabled = self.mem.a20_enabled();

        let mut bus = VmCpuBus {
            mem: &mut self.mem,
            serial: &mut self.serial,
        };
        let cfg = Tier0Config::from_cpuid(&self.assist.features);
        let res =
            run_batch_cpu_core_with_assists(&cfg, &mut self.assist, &mut self.cpu, &mut bus, 1);

        match res.exit {
            BatchExit::Completed | BatchExit::Branch => CpuExit::Continue,
            BatchExit::Halted => CpuExit::Halt,
            BatchExit::BiosInterrupt(vector) => {
                let bus: &mut dyn BiosBus = &mut self.mem;
                self.bios
                    .dispatch_interrupt(vector, &mut self.cpu, bus, &mut self.disk, None);
                self.cpu.a20_enabled = self.mem.a20_enabled();
                CpuExit::BiosInterrupt(vector)
            }
            BatchExit::Assist(reason) => panic!("unhandled CPU assist: {reason:?}"),
            BatchExit::Exception(e) => panic!("cpu exception: {e:?}"),
            BatchExit::CpuExit(e) => panic!("cpu exit: {e:?}"),
        }
    }

    pub fn save_snapshot(&mut self, options: SnapshotOptions) -> Result<Vec<u8>, SnapshotError> {
        snapshot::save_vm_snapshot(self, options)
    }

    pub fn restore_snapshot(&mut self, bytes: &[u8]) -> Result<(), SnapshotError> {
        snapshot::restore_vm_snapshot(self, bytes)
    }

    pub fn serial_output(&self) -> &[u8] {
        &self.serial
    }

    pub(crate) fn snapshot_meta(&mut self) -> aero_snapshot::SnapshotMeta {
        let snapshot_id = self.snapshot_seq;
        self.snapshot_seq = self.snapshot_seq.saturating_add(1);
        let meta = aero_snapshot::SnapshotMeta {
            snapshot_id,
            parent_snapshot_id: self.last_snapshot_id,
            created_unix_ms: 0,
            label: None,
        };
        self.last_snapshot_id = Some(snapshot_id);
        meta
    }

    pub(crate) fn set_last_snapshot_id(&mut self, snapshot_id: u64) {
        self.last_snapshot_id = Some(snapshot_id);
        self.snapshot_seq = self.snapshot_seq.max(snapshot_id.saturating_add(1));
    }

    pub(crate) fn last_snapshot_id(&self) -> Option<u64> {
        self.last_snapshot_id
    }
}
