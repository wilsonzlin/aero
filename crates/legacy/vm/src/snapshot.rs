use std::io::Cursor;

use aero_cpu_core::assist::AssistContext;
use aero_snapshot::{
    apply_cpu_state_to_cpu_core, apply_mmu_state_to_cpu_core, cpu_state_from_cpu_core,
    mmu_state_from_cpu_core, DeviceId, DeviceState, DiskOverlayRefs, MmuState, RamMode,
    RestoreOptions, SaveOptions, SnapshotMeta, SnapshotSource, SnapshotTarget,
};
use firmware::bios::{A20Gate, BiosSnapshot, BlockDevice};

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
        Vm::snapshot_meta(self)
    }

    fn cpu_state(&self) -> aero_snapshot::CpuState {
        cpu_state_from_cpu_core(&self.cpu)
    }

    fn mmu_state(&self) -> MmuState {
        mmu_state_from_cpu_core(&self.cpu)
    }

    fn device_states(&self) -> Vec<DeviceState> {
        let bios_state = encode_bios_state(self).expect("BiosSnapshot encoding to Vec cannot fail");
        vec![DeviceState {
            id: DeviceId::BIOS,
            version: 1,
            flags: 0,
            data: bios_state,
        }]
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

    fn dirty_page_size(&self) -> u32 {
        super::DIRTY_PAGE_SIZE as u32
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        Some(self.mem.take_dirty_pages())
    }
}

impl<D: BlockDevice> SnapshotTarget for Vm<D> {
    fn restore_meta(&mut self, meta: SnapshotMeta) {
        Vm::set_last_snapshot_id(self, meta.snapshot_id);
    }

    fn restore_cpu_state(&mut self, state: aero_snapshot::CpuState) {
        apply_cpu_state_to_cpu_core(&state, &mut self.cpu);
        self.mem.set_a20_enabled(state.a20_enabled);
    }

    fn restore_mmu_state(&mut self, state: MmuState) {
        apply_mmu_state_to_cpu_core(&state, &mut self.cpu);
    }

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        for state in states {
            if state.id == DeviceId::BIOS && state.version == 1 {
                if let Ok(snapshot) = decode_bios_state(&state.data) {
                    self.bios.restore_snapshot(snapshot, &mut self.mem);
                }
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

    fn post_restore(&mut self) -> Result<(), SnapshotError> {
        // Snapshot payloads store the A20 latch in the CPU state; keep the memory translation in
        // sync so subsequent guest execution uses the restored addressing behaviour.
        self.mem.set_a20_enabled(self.cpu.a20_enabled);
        self.mem.clear_dirty();
        self.assist = AssistContext::default();
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

pub fn restore_vm_snapshot<D: BlockDevice>(vm: &mut Vm<D>, bytes: &[u8]) -> Result<(), SnapshotError> {
    let expected_parent_snapshot_id = Vm::last_snapshot_id(vm);
    aero_snapshot::restore_snapshot_with_options(
        &mut Cursor::new(bytes),
        vm,
        RestoreOptions {
            expected_parent_snapshot_id,
        },
    )
}
