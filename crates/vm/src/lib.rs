//! Minimal VM wiring for the BIOS firmware tests.

use firmware::bios::{Bios, BiosBus};
use machine::{BlockDevice, CpuExit, CpuState, PhysicalMemory};

pub struct Vm<D: BlockDevice> {
    pub cpu: CpuState,
    pub mem: PhysicalMemory,
    pub bios: Bios,
    pub disk: D,
}

impl<D: BlockDevice> Vm<D> {
    pub fn new(mem_size: usize, bios: Bios, disk: D) -> Self {
        Self {
            cpu: CpuState::default(),
            mem: PhysicalMemory::new(mem_size),
            bios,
            disk,
        }
    }

    pub fn reset(&mut self) {
        let bus: &mut dyn BiosBus = &mut self.mem;
        self.bios.post(&mut self.cpu, bus, &mut self.disk);
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
}
