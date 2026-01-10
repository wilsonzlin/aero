use std::io::Cursor;
use std::time::{SystemTime, UNIX_EPOCH};

use aero_snapshot::{
    CpuState, DeviceId, DeviceState, DiskOverlayRefs, MmuState, Result, SaveOptions, SnapshotMeta,
    SnapshotSource, SnapshotTarget,
};

const DEFAULT_PAGE_SIZE: u32 = 4096;

#[derive(Debug, Clone)]
pub struct Vm {
    cpu: CpuState,
    mmu: MmuState,
    memory: GuestMemory,
    serial: SerialDevice,
    next_snapshot_id: u64,
    last_snapshot_id: Option<u64>,
}

impl Vm {
    pub fn new(ram_size: usize) -> Self {
        Self {
            cpu: CpuState::default(),
            mmu: MmuState::default(),
            memory: GuestMemory::new(ram_size, DEFAULT_PAGE_SIZE as usize),
            serial: SerialDevice::default(),
            next_snapshot_id: 1,
            last_snapshot_id: None,
        }
    }

    pub fn run_steps(&mut self, steps: u64) {
        for _ in 0..steps {
            self.step();
        }
    }

    fn step(&mut self) {
        let rip = self.cpu.rip;
        let addr = ((rip.wrapping_mul(13)).wrapping_add(self.cpu.rax) as usize) % self.memory.len();
        let old = self.memory.read_u8(addr);
        let new = old.wrapping_add((self.cpu.rax as u8) ^ (rip as u8));
        self.memory.write_u8(addr, new);
        self.cpu.rax = self.cpu.rax.wrapping_add(new as u64 + 1);
        self.cpu.rip = self.cpu.rip.wrapping_add(1);
        self.serial.write_byte(new ^ 0xAA);
    }

    pub fn serial_output(&self) -> &[u8] {
        &self.serial.output
    }

    pub fn memory(&self) -> &[u8] {
        &self.memory.data
    }

    pub fn take_snapshot_full(&mut self) -> Result<Vec<u8>> {
        self.take_snapshot_with_options(SaveOptions::default())
    }

    pub fn take_snapshot_dirty(&mut self) -> Result<Vec<u8>> {
        let mut options = SaveOptions::default();
        options.ram.mode = aero_snapshot::RamMode::Dirty;
        self.take_snapshot_with_options(options)
    }

    fn take_snapshot_with_options(&mut self, options: SaveOptions) -> Result<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        aero_snapshot::save_snapshot(&mut cursor, self, options)?;
        Ok(cursor.into_inner())
    }

    pub fn restore_snapshot_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        aero_snapshot::restore_snapshot(&mut Cursor::new(bytes), self)
    }
}

impl SnapshotSource for Vm {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        let snapshot_id = self.next_snapshot_id;
        self.next_snapshot_id += 1;
        let meta = SnapshotMeta {
            snapshot_id,
            parent_snapshot_id: self.last_snapshot_id,
            created_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            label: None,
        };
        self.last_snapshot_id = Some(snapshot_id);
        meta
    }

    fn cpu_state(&self) -> CpuState {
        self.cpu.clone()
    }

    fn mmu_state(&self) -> MmuState {
        self.mmu.clone()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        vec![DeviceState {
            id: DeviceId::SERIAL,
            version: 1,
            flags: 0,
            data: self.serial.encode_state(),
        }]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.memory.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        self.memory.read(offset, buf);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        Some(self.memory.take_dirty_pages())
    }
}

impl SnapshotTarget for Vm {
    fn restore_meta(&mut self, meta: SnapshotMeta) {
        self.last_snapshot_id = Some(meta.snapshot_id);
    }

    fn restore_cpu_state(&mut self, state: CpuState) {
        self.cpu = state;
    }

    fn restore_mmu_state(&mut self, state: MmuState) {
        self.mmu = state;
    }

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        for state in states {
            if state.id == DeviceId::SERIAL {
                self.serial.decode_state(&state.data);
            }
        }
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.memory.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| aero_snapshot::SnapshotError::Corrupt("ram offset overflow"))?;
        self.memory.write_restore(offset, data);
        Ok(())
    }

    fn post_restore(&mut self) -> Result<()> {
        self.memory.clear_dirty();
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct GuestMemory {
    data: Vec<u8>,
    dirty: DirtyBitmap,
}

impl GuestMemory {
    fn new(len: usize, page_size: usize) -> Self {
        let dirty = DirtyBitmap::new(len, page_size);
        Self {
            data: vec![0u8; len],
            dirty,
        }
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn read_u8(&self, addr: usize) -> u8 {
        self.data[addr]
    }

    fn write_u8(&mut self, addr: usize, val: u8) {
        self.data[addr] = val;
        self.dirty.mark_addr(addr);
    }

    fn read(&self, offset: usize, dst: &mut [u8]) {
        dst.copy_from_slice(&self.data[offset..offset + dst.len()]);
    }

    fn write_restore(&mut self, offset: usize, src: &[u8]) {
        self.data[offset..offset + src.len()].copy_from_slice(src);
        // Snapshot restore should not make pages dirty.
    }

    fn take_dirty_pages(&mut self) -> Vec<u64> {
        self.dirty.take()
    }

    fn clear_dirty(&mut self) {
        self.dirty.clear();
    }
}

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
            bits: vec![0u64; words],
            pages,
            page_size,
        }
    }

    fn mark_addr(&mut self, addr: usize) {
        let page = addr / self.page_size;
        if page < self.pages {
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

#[derive(Debug, Clone, Default)]
struct SerialDevice {
    output: Vec<u8>,
}

impl SerialDevice {
    fn write_byte(&mut self, b: u8) {
        self.output.push(b);
    }

    fn encode_state(&self) -> Vec<u8> {
        // Device payload format v1:
        //   u32: output length
        //   [u8; len]: output bytes
        let mut buf = Vec::with_capacity(4 + self.output.len());
        buf.extend_from_slice(&(self.output.len() as u32).to_le_bytes());
        buf.extend_from_slice(&self.output);
        buf
    }

    fn decode_state(&mut self, bytes: &[u8]) {
        if bytes.len() < 4 {
            return;
        }
        let len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        let available = bytes.len().saturating_sub(4);
        let take = len.min(available);
        self.output.clear();
        self.output.extend_from_slice(&bytes[4..4 + take]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_bytes(bytes: &[u8]) -> [u8; 32] {
        blake3::hash(bytes).into()
    }

    #[test]
    fn snapshot_round_trip_full_is_deterministic() {
        let mut baseline = Vm::new(256 * 1024);
        baseline.run_steps(300);
        let baseline_out = baseline.serial_output().to_vec();
        let baseline_mem = hash_bytes(baseline.memory());

        let mut vm = Vm::new(256 * 1024);
        vm.run_steps(100);
        let snap = vm.take_snapshot_full().unwrap();

        let mut resumed = Vm::new(256 * 1024);
        resumed.restore_snapshot_bytes(&snap).unwrap();
        resumed.run_steps(200);

        assert_eq!(baseline_out, resumed.serial_output());
        assert_eq!(baseline_mem, hash_bytes(resumed.memory()));
    }

    #[test]
    fn snapshot_round_trip_dirty_chain_is_deterministic() {
        let mut baseline = Vm::new(256 * 1024);
        baseline.run_steps(300);
        let baseline_out = baseline.serial_output().to_vec();
        let baseline_mem = hash_bytes(baseline.memory());

        let mut vm = Vm::new(256 * 1024);
        vm.run_steps(100);
        let base = vm.take_snapshot_full().unwrap();
        vm.run_steps(50);
        let diff = vm.take_snapshot_dirty().unwrap();

        let mut resumed = Vm::new(256 * 1024);
        resumed.restore_snapshot_bytes(&base).unwrap();
        resumed.restore_snapshot_bytes(&diff).unwrap();
        resumed.run_steps(150);

        assert_eq!(baseline_out, resumed.serial_output());
        assert_eq!(baseline_mem, hash_bytes(resumed.memory()));
    }
}
