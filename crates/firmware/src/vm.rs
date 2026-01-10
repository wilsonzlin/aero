use crate::legacy_bios::LegacyBios;
use crate::bus::Bus;
use crate::realmode::RealModeCpu;

#[derive(Debug, Clone)]
pub struct VmError(pub String);

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for VmError {}

pub struct RealModeVm<'a, B: Bus> {
    pub bus: &'a mut B,
    pub bios: &'a mut LegacyBios,
    pub cpu: RealModeCpu,
    pub halted: bool,
}

impl<'a, B: Bus> RealModeVm<'a, B> {
    pub fn new(bus: &'a mut B, bios: &'a mut LegacyBios) -> Self {
        Self {
            bus,
            bios,
            cpu: RealModeCpu {
                cs: 0,
                ip: 0x7C00,
                ds: 0,
                es: 0,
                ss: 0,
                esp: 0,
                ..RealModeCpu::default()
            },
            halted: false,
        }
    }

    pub fn load(&mut self, paddr: u32, bytes: &[u8]) {
        self.bus.write(paddr, bytes);
    }

    fn fetch_u8(&mut self) -> u8 {
        let b = self
            .bus
            .read_u8(RealModeCpu::seg_off(self.cpu.cs, self.cpu.ip));
        self.cpu.ip = self.cpu.ip.wrapping_add(1);
        b
    }

    fn fetch_u16(&mut self) -> u16 {
        let lo = self.fetch_u8() as u16;
        let hi = self.fetch_u8() as u16;
        lo | (hi << 8)
    }

    fn fetch_u32(&mut self) -> u32 {
        let b0 = self.fetch_u8() as u32;
        let b1 = self.fetch_u8() as u32;
        let b2 = self.fetch_u8() as u32;
        let b3 = self.fetch_u8() as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    fn set_reg16(&mut self, reg: u8, val: u16) {
        match reg {
            0 => self.cpu.set_ax(val),
            1 => self.cpu.set_cx(val),
            2 => self.cpu.set_dx(val),
            3 => self.cpu.set_bx(val),
            4 => self.cpu.esp = (self.cpu.esp & 0xFFFF_0000) | (val as u32),
            5 => self.cpu.ebp = (self.cpu.ebp & 0xFFFF_0000) | (val as u32),
            6 => self.cpu.esi = (self.cpu.esi & 0xFFFF_0000) | (val as u32),
            7 => self.cpu.edi = (self.cpu.edi & 0xFFFF_0000) | (val as u32),
            _ => {}
        }
    }

    fn set_reg32(&mut self, reg: u8, val: u32) {
        match reg {
            0 => self.cpu.eax = val,
            1 => self.cpu.ecx = val,
            2 => self.cpu.edx = val,
            3 => self.cpu.ebx = val,
            4 => self.cpu.esp = val,
            5 => self.cpu.ebp = val,
            6 => self.cpu.esi = val,
            7 => self.cpu.edi = val,
            _ => {}
        }
    }

    fn set_reg8(&mut self, reg: u8, val: u8) {
        match reg {
            0 => self.cpu.set_al(val),
            1 => self.cpu.ecx = (self.cpu.ecx & 0xFFFF_FF00) | (val as u32),
            2 => self.cpu.edx = (self.cpu.edx & 0xFFFF_FF00) | (val as u32),
            3 => self.cpu.ebx = (self.cpu.ebx & 0xFFFF_FF00) | (val as u32),
            4 => self.cpu.set_ah(val),
            5 => self.cpu.ecx = (self.cpu.ecx & 0xFFFF_00FF) | ((val as u32) << 8),
            6 => self.cpu.edx = (self.cpu.edx & 0xFFFF_00FF) | ((val as u32) << 8),
            7 => self.cpu.ebx = (self.cpu.ebx & 0xFFFF_00FF) | ((val as u32) << 8),
            _ => {}
        }
    }

    pub fn step(&mut self) -> Result<(), VmError> {
        if self.halted {
            return Ok(());
        }

        let mut op32 = false;
        let mut opcode = self.fetch_u8();
        if opcode == 0x66 {
            op32 = true;
            opcode = self.fetch_u8();
        }

        match opcode {
            0xB0..=0xB7 => {
                // MOV r8, imm8
                let reg = (opcode - 0xB0) as u8;
                let imm = self.fetch_u8();
                self.set_reg8(reg, imm);
            }
            0xB8..=0xBF => {
                let reg = (opcode - 0xB8) as u8;
                if op32 {
                    let imm = self.fetch_u32();
                    self.set_reg32(reg, imm);
                } else {
                    let imm = self.fetch_u16();
                    self.set_reg16(reg, imm);
                }
            }
            0x8E => {
                // MOV Sreg, r/m16 (only DS/ES from AX are needed for tests).
                let modrm = self.fetch_u8();
                match modrm {
                    0xD8 => self.cpu.ds = self.cpu.ax(), // mov ds, ax
                    0xC0 => self.cpu.es = self.cpu.ax(), // mov es, ax
                    _ => {
                        return Err(VmError(format!(
                            "unsupported 0x8E modrm {modrm:#x} at {:#x}",
                            self.cpu.phys_ip()
                        )));
                    }
                }
            }
            0xCD => {
                let int = self.fetch_u8();
                self.bios.handle_interrupt(int, self.bus, &mut self.cpu);
            }
            0xA3 => {
                let off = self.fetch_u16();
                let phys = RealModeCpu::seg_off(self.cpu.ds, off);
                self.bus.write_u16(phys, self.cpu.ax());
            }
            0xA2 => {
                let off = self.fetch_u16();
                let phys = RealModeCpu::seg_off(self.cpu.ds, off);
                self.bus.write_u8(phys, self.cpu.al());
            }
            0xA0 => {
                let off = self.fetch_u16();
                let phys = RealModeCpu::seg_off(self.cpu.ds, off);
                let val = self.bus.read_u8(phys);
                self.cpu.set_al(val);
            }
            0xC7 => {
                let modrm = self.fetch_u8();
                if modrm != 0x06 {
                    return Err(VmError(format!(
                        "unsupported 0xC7 modrm {modrm:#x} at {:#x}",
                        self.cpu.phys_ip()
                    )));
                }
                let addr = self.fetch_u16();
                let imm = self.fetch_u16();
                let phys = RealModeCpu::seg_off(self.cpu.ds, addr);
                self.bus.write_u16(phys, imm);
            }
            0xF4 => {
                self.halted = true;
            }
            0xEB => {
                let rel = self.fetch_u8() as i8;
                self.cpu.ip = self.cpu.ip.wrapping_add(rel as i16 as u16);
            }
            _ => {
                return Err(VmError(format!(
                    "unsupported opcode {opcode:#x} at {:#x}",
                    self.cpu.phys_ip()
                )));
            }
        }

        Ok(())
    }

    pub fn run_until<F>(&mut self, max_steps: usize, mut predicate: F) -> Result<(), VmError>
    where
        F: FnMut(&mut Self) -> bool,
    {
        for _ in 0..max_steps {
            if predicate(self) {
                return Ok(());
            }
            self.step()?;
            if self.halted {
                return Ok(());
            }
        }
        Err(VmError(format!(
            "VM did not finish within {max_steps} steps"
        )))
    }
}
