use emulator::devices::vga::vbe;

use crate::{
    cpu::CpuState,
    memory::{real_addr, MemoryBus},
};

use super::Bios;

const VBE_SUCCESS: u16 = 0x004F;
const VBE_FAIL: u16 = 0x014F;

impl Bios {
    pub fn handle_int10_vbe(&mut self, cpu: &mut CpuState, memory: &mut impl MemoryBus) {
        match cpu.ax() {
            0x4F00 => {
                let dest = real_addr(cpu.es(), cpu.di());
                self.video.vbe.write_controller_info(memory, dest);
                vbe_success(cpu);
            }
            0x4F01 => {
                let mode = cpu.cx();
                let dest = real_addr(cpu.es(), cpu.di());
                if self.video.vbe.write_mode_info(memory, mode, dest) {
                    vbe_success(cpu);
                } else {
                    vbe_failure(cpu);
                }
            }
            0x4F02 => {
                let raw = cpu.bx();
                let mode = raw & 0x3FFF;
                let no_clear = raw & 0x8000 != 0;
                // bit 14 is "linear framebuffer requested". This implementation always provides an
                // LFB; we accept the request bit but do not require it.
                if self.video.vbe.set_mode(memory, mode, no_clear) {
                    vbe_success(cpu);
                } else {
                    vbe_failure(cpu);
                }
            }
            0x4F03 => {
                cpu.set_bx(self.video.vbe.current_mode.unwrap_or(0));
                vbe_success(cpu);
            }
            0x4F05 => {
                // Display window control (bank switching)
                let sub = cpu.bh();
                let window = cpu.bl();

                if window != 0 {
                    vbe_failure(cpu);
                    return;
                }

                match sub {
                    0x00 => {
                        self.video.vbe.bank = cpu.dx();
                        vbe_success(cpu);
                    }
                    0x01 => {
                        cpu.set_dx(self.video.vbe.bank);
                        vbe_success(cpu);
                    }
                    _ => vbe_failure(cpu),
                }
            }
            0x4F06 => {
                // Set/Get Logical Scan Line Length
                let sub = cpu.bl();
                match sub {
                    0x00 => {
                        // Set in pixels
                        let pixels = cpu.cx();
                        if let Some(mode) = self
                            .video
                            .vbe
                            .current_mode
                            .and_then(|m| self.video.vbe.find_mode(m))
                        {
                            let bytes_per_line = pixels.saturating_mul(mode.bytes_per_pixel());
                            self.video.vbe.logical_width_pixels = pixels.max(mode.width);
                            self.video.vbe.bytes_per_scan_line =
                                bytes_per_line.max(mode.bytes_per_scan_line());
                            cpu.set_bx(self.video.vbe.bytes_per_scan_line);
                            cpu.set_cx(self.video.vbe.logical_width_pixels);
                            cpu.set_dx(self.video.vbe.max_scan_lines());
                            vbe_success(cpu);
                        } else {
                            vbe_failure(cpu);
                        }
                    }
                    0x01 => {
                        // Get
                        cpu.set_bx(self.video.vbe.bytes_per_scan_line);
                        cpu.set_cx(self.video.vbe.logical_width_pixels);
                        cpu.set_dx(self.video.vbe.max_scan_lines());
                        vbe_success(cpu);
                    }
                    0x02 => {
                        // Set in bytes
                        let bytes = cpu.cx();
                        if let Some(mode) = self
                            .video
                            .vbe
                            .current_mode
                            .and_then(|m| self.video.vbe.find_mode(m))
                        {
                            let bpp = mode.bytes_per_pixel();
                            let pixels = if bpp == 0 { 0 } else { bytes / bpp };
                            self.video.vbe.bytes_per_scan_line = bytes.max(mode.bytes_per_scan_line());
                            self.video.vbe.logical_width_pixels = pixels.max(mode.width);
                            cpu.set_bx(self.video.vbe.bytes_per_scan_line);
                            cpu.set_cx(self.video.vbe.logical_width_pixels);
                            cpu.set_dx(self.video.vbe.max_scan_lines());
                            vbe_success(cpu);
                        } else {
                            vbe_failure(cpu);
                        }
                    }
                    0x03 => {
                        // Get maximum
                        if let Some(mode) = self
                            .video
                            .vbe
                            .current_mode
                            .and_then(|m| self.video.vbe.find_mode(m))
                        {
                            let max_bytes = self
                                .video
                                .vbe
                                .bytes_per_scan_line
                                .max(mode.bytes_per_scan_line());
                            let max_pixels = self.video.vbe.logical_width_pixels.max(mode.width);
                            cpu.set_bx(max_bytes);
                            cpu.set_cx(max_pixels);
                            vbe_success(cpu);
                        } else {
                            vbe_failure(cpu);
                        }
                    }
                    _ => vbe_failure(cpu),
                }
            }
            0x4F07 => {
                // Set/Get Display Start
                let sub = cpu.bl();
                match sub {
                    0x00 => {
                        self.video.vbe.display_start_x = cpu.cx();
                        self.video.vbe.display_start_y = cpu.dx();
                        vbe_success(cpu);
                    }
                    0x01 => {
                        cpu.set_cx(self.video.vbe.display_start_x);
                        cpu.set_dx(self.video.vbe.display_start_y);
                        vbe_success(cpu);
                    }
                    _ => vbe_failure(cpu),
                }
            }
            0x4F08 => {
                // Set/Get DAC Palette Format
                let sub = cpu.bl();
                match sub {
                    0x00 => {
                        let bits = cpu.bh();
                        if bits == 6 || bits == 8 {
                            self.video.vbe.dac_width_bits = bits;
                            vbe_success(cpu);
                        } else {
                            vbe_failure(cpu);
                        }
                    }
                    0x01 => {
                        cpu.set_bh(self.video.vbe.dac_width_bits);
                        vbe_success(cpu);
                    }
                    _ => vbe_failure(cpu),
                }
            }
            0x4F09 => {
                // Set/Get Palette Data
                let sub = cpu.bl() & 0x7F; // ignore wait-for-retrace bit
                let count = cpu.cx() as usize;
                let start = cpu.dx() as usize;
                let addr = real_addr(cpu.es(), cpu.di());

                if start >= 256 || start + count > 256 {
                    vbe_failure(cpu);
                    return;
                }

                match sub {
                    0x00 => {
                        let bytes = count * 4;
                        let mut tmp = vec![0u8; bytes];
                        memory.read_bytes(addr, &mut tmp);
                        self.video.vbe.palette[start * 4..start * 4 + bytes].copy_from_slice(&tmp);
                        vbe_success(cpu);
                    }
                    0x01 => {
                        let bytes = count * 4;
                        memory.write_bytes(
                            addr,
                            &self.video.vbe.palette[start * 4..start * 4 + bytes],
                        );
                        vbe_success(cpu);
                    }
                    _ => vbe_failure(cpu),
                }
            }
            0x4F15 => handle_ddc(cpu, memory),
            _ => vbe_failure(cpu),
        }
    }
}

fn vbe_success(cpu: &mut CpuState) {
    cpu.set_ax(VBE_SUCCESS);
    cpu.clear_cf();
}

fn vbe_failure(cpu: &mut CpuState) {
    cpu.set_ax(VBE_FAIL);
    cpu.set_cf();
}

fn handle_ddc(cpu: &mut CpuState, memory: &mut impl MemoryBus) {
    match cpu.bl() {
        0x00 => {
            // Report DDC2 + EDID support.
            cpu.set_ax(VBE_SUCCESS);
            cpu.set_bx(0x0200);
            cpu.clear_cf();
        }
        0x01 => {
            let Some(edid) = vbe::read_edid(cpu.dx()) else {
                vbe_failure(cpu);
                return;
            };

            let addr = real_addr(cpu.es(), cpu.di());
            memory.write_bytes(addr, &edid);

            cpu.set_ax(VBE_SUCCESS);
            cpu.clear_cf();
        }
        _ => vbe_failure(cpu),
    }
}

