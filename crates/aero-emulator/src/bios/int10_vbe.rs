use crate::{cpu::CpuState, devices::vga::VgaDevice, memory::MemoryBus};

pub trait VbeServices {
    fn handle_int10(&mut self, cpu: &mut CpuState, mem: &mut impl MemoryBus, vga: &mut VgaDevice);
}

#[derive(Default)]
pub struct NoVbe;

impl VbeServices for NoVbe {
    fn handle_int10(
        &mut self,
        cpu: &mut CpuState,
        _mem: &mut impl MemoryBus,
        _vga: &mut VgaDevice,
    ) {
        // VBE functions return AL=0x4F and AH=status. Set CF on failure.
        cpu.set_ax(0x024F);
        cpu.set_cf(true);
    }
}
