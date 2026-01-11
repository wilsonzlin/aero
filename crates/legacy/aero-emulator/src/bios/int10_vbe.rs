use crate::{cpu::CpuState, devices::vbe::VbeDevice, devices::vga::VgaDevice, memory::MemoryBus};

/// VBE services are dispatched via INT 10h with `AX=4Fxx` (i.e. `AH=0x4F`).
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

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum VbeStatus {
    Success = 0x00,
    Failed = 0x01,
    NotSupportedInCurrentConfig = 0x02,
    InvalidInCurrentVideoMode = 0x03,
}

fn vbe_return(cpu: &mut CpuState, status: VbeStatus) {
    cpu.set_ax(((status as u16) << 8) | 0x004F);
    cpu.set_cf(!matches!(status, VbeStatus::Success));
}

fn di(cpu: &CpuState) -> u16 {
    (cpu.rdi & 0xFFFF) as u16
}

impl VbeServices for VbeDevice {
    fn handle_int10(&mut self, cpu: &mut CpuState, mem: &mut impl MemoryBus, _vga: &mut VgaDevice) {
        match cpu.al() {
            0x00 => {
                // Return VBE controller info block at ES:DI.
                let mut block = [0u8; 512];
                self.build_controller_info(cpu.es.selector, di(cpu), &mut block);
                mem.write_physical(cpu.es_di(), &block);
                vbe_return(cpu, VbeStatus::Success);
            }
            0x01 => {
                // Return VBE mode info block for CX at ES:DI.
                let mode = crate::devices::vbe::VbeModeId::from_raw(cpu.cx());
                let Some(info) = self.mode_info(mode) else {
                    vbe_return(cpu, VbeStatus::Failed);
                    return;
                };
                mem.write_physical(cpu.es_di(), &info);
                vbe_return(cpu, VbeStatus::Success);
            }
            0x02 => {
                // Set VBE mode in CX (supports LFB bit 0x4000).
                let mode = crate::devices::vbe::VbeModeId::from_raw(cpu.cx());
                let use_lfb = (cpu.cx() & 0x4000) != 0;
                if self.set_mode(mode, use_lfb).is_err() {
                    vbe_return(cpu, VbeStatus::Failed);
                    return;
                }
                vbe_return(cpu, VbeStatus::Success);
            }
            0x03 => {
                // Get current VBE mode.
                let Some((mode, use_lfb)) = self.current_mode() else {
                    vbe_return(cpu, VbeStatus::InvalidInCurrentVideoMode);
                    return;
                };
                let mut bx = mode.0;
                if use_lfb {
                    bx |= 0x4000;
                }
                cpu.set_bx(bx);
                vbe_return(cpu, VbeStatus::Success);
            }
            0x05 => {
                // Bank switching.
                match cpu.bh() {
                    0x00 => {
                        // Set bank (DX), window in BL.
                        if self.set_bank(cpu.bl(), cpu.dx()).is_err() {
                            vbe_return(cpu, VbeStatus::Failed);
                            return;
                        }
                        vbe_return(cpu, VbeStatus::Success);
                    }
                    0x01 => {
                        // Get bank into DX.
                        let Some(bank) = self.get_bank(cpu.bl()) else {
                            vbe_return(cpu, VbeStatus::Failed);
                            return;
                        };
                        cpu.set_dx(bank);
                        vbe_return(cpu, VbeStatus::Success);
                    }
                    _ => vbe_return(cpu, VbeStatus::Failed),
                }
            }
            _ => vbe_return(cpu, VbeStatus::NotSupportedInCurrentConfig),
        }
    }
}
