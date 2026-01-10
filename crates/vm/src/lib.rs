//! Minimal VM wiring for the BIOS firmware tests.

mod snapshot;

use firmware::bios::{Bios, BiosBus};
use machine::{BlockDevice, CpuExit, CpuState, PhysicalMemory};

pub use snapshot::{SnapshotError, SnapshotOptions};

pub struct Vm<D: BlockDevice> {
    pub cpu: CpuState,
    pub mem: PhysicalMemory,
    pub bios: Bios,
    pub disk: D,
    snapshot_seq: u64,
    last_snapshot_id: Option<u64>,
}

impl<D: BlockDevice> Vm<D> {
    pub fn new(mem_size: usize, bios: Bios, disk: D) -> Self {
        Self {
            cpu: CpuState::default(),
            mem: PhysicalMemory::new(mem_size),
            bios,
            disk,
            snapshot_seq: 1,
            last_snapshot_id: None,
        }
    }

    pub fn reset(&mut self) {
        let bus: &mut dyn BiosBus = &mut self.mem;
        self.bios.post(&mut self.cpu, bus, &mut self.disk);
        self.mem.clear_dirty();
    }

    pub fn step(&mut self) -> CpuExit {
        let exit = self.cpu.step(&mut self.mem);
        if let CpuExit::BiosInterrupt(vector) = exit {
            let bus: &mut dyn BiosBus = &mut self.mem;
            self.bios
                .dispatch_interrupt(vector, &mut self.cpu, bus, &mut self.disk);
        }
        exit
    }

    pub fn save_snapshot(&mut self, options: SnapshotOptions) -> Result<Vec<u8>, SnapshotError> {
        snapshot::save_vm_snapshot(self, options)
    }

    pub fn restore_snapshot(&mut self, bytes: &[u8]) -> Result<(), SnapshotError> {
        snapshot::restore_vm_snapshot(self, bytes)
    }
}
