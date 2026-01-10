use emulator::devices::vga::vbe;

use crate::{cpu::CpuState, memory::MemoryBus};

use super::Bios;

const VBE_SUCCESS: u16 = 0x004F;
const VBE_FAIL: u16 = 0x014F;

impl Bios {
    pub fn handle_int10_vbe(&mut self, cpu: &mut CpuState, memory: &mut impl MemoryBus) {
        match cpu.ax() {
            0x4F15 => handle_ddc(cpu, memory),
            _ => {
                cpu.set_ax(VBE_FAIL);
                cpu.set_cf();
            }
        }
    }
}

fn handle_ddc(cpu: &mut CpuState, memory: &mut impl MemoryBus) {
    match cpu.bl() {
        0x00 => {
            cpu.set_ax(VBE_SUCCESS);
            cpu.set_bx(0x0200);
            cpu.clear_cf();
        }
        0x01 => {
            let Some(edid) = vbe::read_edid(cpu.dx()) else {
                cpu.set_ax(VBE_FAIL);
                cpu.set_cf();
                return;
            };

            let addr = ((cpu.es() as u64) << 4) + (cpu.di() as u64);
            for (i, byte) in edid.iter().enumerate() {
                memory.write_u8(addr + i as u64, *byte);
            }

            cpu.set_ax(VBE_SUCCESS);
            cpu.clear_cf();
        }
        _ => {
            cpu.set_ax(VBE_FAIL);
            cpu.set_cf();
        }
    }
}

