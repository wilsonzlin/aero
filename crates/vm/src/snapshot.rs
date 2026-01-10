use std::io::Cursor;
use std::ops::Range;

use aero_snapshot::{
    CpuState, DeviceId, DeviceState, DiskOverlayRefs, MmuState, RamMode, SaveOptions, SnapshotMeta,
    SnapshotSource, SnapshotTarget,
};
use firmware::bios::BiosSnapshot;
use machine::{A20Gate, BlockDevice, CpuState as MachineCpuState, PhysicalMemory, FLAG_ALWAYS_ON};

use crate::Vm;

#[derive(Debug, Clone, Copy)]
pub struct SnapshotOptions {
    pub ram_mode: RamMode,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            ram_mode: RamMode::Full,
        }
    }
}

pub type SnapshotError = aero_snapshot::SnapshotError;

fn machine_cpu_to_snapshot(cpu: &MachineCpuState) -> CpuState {
    CpuState {
        rax: cpu.rax,
        rbx: cpu.rbx,
        rcx: cpu.rcx,
        rdx: cpu.rdx,
        rsi: cpu.rsi,
        rdi: cpu.rdi,
        rbp: cpu.rbp,
        rsp: cpu.rsp,
        r8: 0,
        r9: 0,
        r10: 0,
        r11: 0,
        r12: 0,
        r13: 0,
        r14: 0,
        r15: 0,
        rip: cpu.rip,
        rflags: cpu.rflags,
        cs: cpu.cs.selector,
        ds: cpu.ds.selector,
        es: cpu.es.selector,
        fs: 0,
        gs: 0,
        ss: cpu.ss.selector,
        xmm: [0u128; 16],
    }
}

fn snapshot_cpu_to_machine(state: CpuState, cpu: &mut MachineCpuState) {
    cpu.rax = state.rax;
    cpu.rbx = state.rbx;
    cpu.rcx = state.rcx;
    cpu.rdx = state.rdx;
    cpu.rsi = state.rsi;
    cpu.rdi = state.rdi;
    cpu.rbp = state.rbp;
    cpu.rsp = state.rsp;
    cpu.rip = state.rip;
    cpu.rflags = state.rflags | FLAG_ALWAYS_ON;
    cpu.cs.selector = state.cs;
    cpu.ds.selector = state.ds;
    cpu.es.selector = state.es;
    cpu.ss.selector = state.ss;
}

fn encode_cpu_internal(cpu: &MachineCpuState) -> Vec<u8> {
    // v1 payload:
    //  u8 pending_present
    //  [u8 pending_int]
    //  u8 halted
    let mut out = Vec::with_capacity(3);
    match cpu.pending_bios_int {
        Some(int) => {
            out.push(1);
            out.push(int);
        }
        None => out.push(0),
    }
    out.push(cpu.halted as u8);
    out
}

fn decode_cpu_internal(bytes: &[u8]) -> Result<(Option<u8>, bool), SnapshotError> {
    if bytes.is_empty() {
        return Err(SnapshotError::Corrupt("cpu_internal payload too short"));
    }
    let mut idx = 0;
    let pending = match bytes[idx] {
        0 => {
            idx += 1;
            None
        }
        1 => {
            if bytes.len() < 2 {
                return Err(SnapshotError::Corrupt("cpu_internal pending missing"));
            }
            idx += 1;
            let val = bytes[idx];
            idx += 1;
            Some(val)
        }
        _ => return Err(SnapshotError::Corrupt("cpu_internal invalid pending tag")),
    };
    if idx >= bytes.len() {
        return Err(SnapshotError::Corrupt("cpu_internal halted missing"));
    }
    let halted = bytes[idx] != 0;
    Ok((pending, halted))
}

fn encode_memory_state(mem: &PhysicalMemory) -> Vec<u8> {
    // v1 payload:
    //  u8 a20_enabled
    //  u32 range_count
    //  repeated: u64 start, u64 end
    let ranges = mem.read_only_ranges();
    let mut out = Vec::with_capacity(1 + 4 + ranges.len() * 16);
    out.push(mem.a20_enabled() as u8);
    out.extend_from_slice(&(ranges.len() as u32).to_le_bytes());
    for r in ranges {
        out.extend_from_slice(&r.start.to_le_bytes());
        out.extend_from_slice(&r.end.to_le_bytes());
    }
    out
}

fn decode_memory_state(bytes: &[u8]) -> Result<(bool, Vec<Range<u64>>), SnapshotError> {
    const MAX_RANGES: u32 = 16 * 1024;
    if bytes.len() < 5 {
        return Err(SnapshotError::Corrupt("memory payload too short"));
    }
    let a20_enabled = bytes[0] != 0;
    let count = u32::from_le_bytes(bytes[1..5].try_into().unwrap()).min(MAX_RANGES) as usize;
    let mut idx = 5usize;
    let mut ranges = Vec::with_capacity(count.min(64));
    for _ in 0..count {
        if idx + 16 > bytes.len() {
            return Err(SnapshotError::Corrupt("memory read-only range truncated"));
        }
        let start = u64::from_le_bytes(bytes[idx..idx + 8].try_into().unwrap());
        let end = u64::from_le_bytes(bytes[idx + 8..idx + 16].try_into().unwrap());
        ranges.push(start..end);
        idx += 16;
    }
    Ok((a20_enabled, ranges))
}

fn encode_bios_state(vm: &Vm<impl BlockDevice>) -> Result<Vec<u8>, SnapshotError> {
    let snapshot = vm.bios.snapshot();
    let mut buf = Vec::new();
    snapshot.encode(&mut buf)?;
    Ok(buf)
}

fn decode_bios_state(bytes: &[u8]) -> Result<BiosSnapshot, SnapshotError> {
    Ok(BiosSnapshot::decode(&mut Cursor::new(bytes))?)
}

impl<D: BlockDevice> SnapshotSource for Vm<D> {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        let snapshot_id = self.snapshot_seq;
        self.snapshot_seq = self.snapshot_seq.saturating_add(1);
        let meta = SnapshotMeta {
            snapshot_id,
            parent_snapshot_id: self.last_snapshot_id,
            created_unix_ms: 0,
            label: None,
        };
        self.last_snapshot_id = Some(snapshot_id);
        meta
    }

    fn cpu_state(&self) -> CpuState {
        machine_cpu_to_snapshot(&self.cpu)
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        let bios_state = encode_bios_state(self).expect("BiosSnapshot encoding to Vec cannot fail");
        vec![
            DeviceState {
                id: DeviceId::CPU_INTERNAL,
                version: 1,
                flags: 0,
                data: encode_cpu_internal(&self.cpu),
            },
            DeviceState {
                id: DeviceId::MEMORY,
                version: 1,
                flags: 0,
                data: encode_memory_state(&self.mem),
            },
            DeviceState {
                id: DeviceId::BIOS,
                version: 1,
                flags: 0,
                data: bios_state,
            },
        ]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.mem.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<(), SnapshotError> {
        self.mem.read_raw(offset, buf);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

impl<D: BlockDevice> SnapshotTarget for Vm<D> {
    fn restore_meta(&mut self, meta: SnapshotMeta) {
        self.last_snapshot_id = Some(meta.snapshot_id);
    }

    fn restore_cpu_state(&mut self, state: CpuState) {
        snapshot_cpu_to_machine(state, &mut self.cpu);
    }

    fn restore_mmu_state(&mut self, _state: MmuState) {}

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        for state in states {
            match state.id {
                id if id == DeviceId::CPU_INTERNAL && state.version == 1 => {
                    if let Ok((pending, halted)) = decode_cpu_internal(&state.data) {
                        self.cpu.pending_bios_int = pending;
                        self.cpu.halted = halted;
                    }
                }
                id if id == DeviceId::MEMORY && state.version == 1 => {
                    if let Ok((a20_enabled, ranges)) = decode_memory_state(&state.data) {
                        self.mem.set_a20_enabled(a20_enabled);
                        self.mem.set_read_only_ranges(ranges);
                    }
                }
                id if id == DeviceId::BIOS && state.version == 1 => {
                    if let Ok(snapshot) = decode_bios_state(&state.data) {
                        self.bios.restore_snapshot(snapshot);
                    }
                }
                _ => {}
            }
        }
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.mem.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> Result<(), SnapshotError> {
        self.mem.write_raw(offset, data);
        Ok(())
    }
}

pub fn save_vm_snapshot<D: BlockDevice>(
    vm: &mut Vm<D>,
    options: SnapshotOptions,
) -> Result<Vec<u8>, SnapshotError> {
    let mut save = SaveOptions::default();
    save.ram.mode = options.ram_mode;
    let mut cursor = Cursor::new(Vec::new());
    aero_snapshot::save_snapshot(&mut cursor, vm, save)?;
    Ok(cursor.into_inner())
}

pub fn restore_vm_snapshot<D: BlockDevice>(
    vm: &mut Vm<D>,
    bytes: &[u8],
) -> Result<(), SnapshotError> {
    aero_snapshot::restore_snapshot(&mut Cursor::new(bytes), vm)
}
