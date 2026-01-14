use crate::{
    bda::BiosDataArea,
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
                    // Many BIOSes report "VESA mode active" via INT 10h AH=0F by storing 0x6F in
                    // the BDA's video mode byte.
                    BiosDataArea::write_video_mode(memory, 0x6F);
                    self.video_mode = 0x6F;
                    vbe_success(cpu);
                } else {
                    vbe_failure(cpu);
                }
            }
            0x4F03 => {
                // Report the current VBE mode.
                //
                // Per common VBE conventions, include bit 14 ("linear framebuffer enabled") when a
                // VBE mode is active. Some boot code will call 4F03 and then pass the returned BX
                // straight back into 4F01/4F02; our 4F01 implementation already masks off the
                // high bits for this reason.
                let mut mode = self.video.vbe.current_mode.unwrap_or(0);
                if mode != 0 {
                    mode |= 0x4000;
                }
                cpu.set_bx(mode);
                vbe_success(cpu);
            }
            0x4F05 => {
                // Display window control (bank switching)
                // VBE function 4F05h:
                // - BH: window number (0 = A, 1 = B)
                // - BL: subfunction (0 = set, 1 = get)
                let window = cpu.bh();
                let sub = cpu.bl();

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
                            let bpp = mode.bytes_per_pixel();
                            let bytes_per_line = if bpp == 0 {
                                0
                            } else {
                                // Derive scanline length in bytes from the requested pixel count.
                                // Clamp to the largest value representable in `u16` while remaining
                                // whole-pixel aligned (multiple of bytes-per-pixel).
                                let bpp_u32 = u32::from(bpp);
                                let pixels_u32 = u32::from(pixels);
                                let bytes_u32 = pixels_u32.saturating_mul(bpp_u32);
                                let max_aligned =
                                    (u32::from(u16::MAX) / bpp_u32).saturating_mul(bpp_u32);
                                bytes_u32.min(max_aligned) as u16
                            };
                            self.video.vbe.bytes_per_scan_line =
                                bytes_per_line.max(mode.bytes_per_scan_line());
                            let logical_pixels = if bpp == 0 {
                                0
                            } else {
                                self.video.vbe.bytes_per_scan_line / bpp
                            };
                            self.video.vbe.logical_width_pixels = logical_pixels.max(mode.width);
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
                            // VBE subfunction 4F06 BL=2 sets the logical scan line length in
                            // *bytes*.
                            //
                            // Some guests use byte-granular strides that are not representable as
                            // `virt_width_pixels * bytes_per_pixel` (Bochs VBE_DISPI only supports
                            // pixel-granular strides). Preserve the caller-provided byte pitch so
                            // scanout/panning can honor odd byte strides.
                            //
                            // Note: the stride still must not be smaller than the minimum pitch
                            // implied by the mode's resolution and pixel format.
                            self.video.vbe.bytes_per_scan_line =
                                bytes.max(mode.bytes_per_scan_line());

                            let bpp = mode.bytes_per_pixel();
                            // Derive a virtual width in pixels by flooring the byte pitch. This
                            // matches the contract tested by `boot_int10_vbe_panning` and keeps the
                            // value monotonic when the guest tweaks the stride.
                            let pixels = if bpp == 0 { 0 } else { self.video.vbe.bytes_per_scan_line / bpp };
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
                            // Return the maximum number of scan lines available for the current (max)
                            // scan line length.
                            //
                            // Some software expects DX to be updated for BL=0x03 just like the other
                            // 4F06 subfunctions. Leaving DX unchanged can cause callers to treat the
                            // value as uninitialized.
                            let bytes_per_line = u32::from(max_bytes.max(1));
                            let total_bytes =
                                u32::from(self.video.vbe.total_memory_64kb_blocks) * 64 * 1024;
                            let max_lines = total_bytes / bytes_per_line;
                            cpu.set_dx(max_lines.min(u16::MAX as u32) as u16);
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
                // Bit 7 of BL requests the operation happen during vertical retrace.
                //
                // We do not model retrace timing here, but accept the bit for compatibility.
                let sub = cpu.bl() & 0x7F;
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
                            // Keep palette entries coherent when changing DAC width.
                            //
                            // The VBE palette services (4F09) interpret palette components in units
                            // of the current DAC width. When switching between 6-bit and 8-bit
                            // modes, scale the stored palette values so colors remain the same.
                            //
                            // This matters for guests that:
                            // 1) read the default BIOS palette in 6-bit mode,
                            // 2) switch to 8-bit DAC, and
                            // 3) expect `4F09 Get Palette Data` to return 8-bit values representing
                            //    the same colors.
                            let old_bits = self.video.vbe.dac_width_bits;
                            if old_bits != bits {
                                match (old_bits, bits) {
                                    (6, 8) => {
                                        for entry in self.video.vbe.palette.chunks_exact_mut(4) {
                                            for c in &mut entry[..3] {
                                                let v6 = *c & 0x3F;
                                                *c = (v6 << 2) | (v6 >> 4);
                                            }
                                        }
                                    }
                                    (8, 6) => {
                                        for entry in self.video.vbe.palette.chunks_exact_mut(4) {
                                            for c in &mut entry[..3] {
                                                *c >>= 2;
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
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
                        // The VBE palette format is determined by the configured DAC width
                        // (`4F08h`): 6-bit or 8-bit components.
                        //
                        // In 6-bit mode, be permissive and accept 8-bit component values by
                        // downscaling `0..=255` to `0..=63` when any component of an entry exceeds
                        // `0x3F`. This matches common VGA DAC programming behavior and avoids
                        // returning out-of-range palette values via `4F09h Get Palette Data`.
                        if self.video.vbe.dac_width_bits == 6 {
                            for entry in tmp.chunks_exact_mut(4) {
                                let is_8bit = entry[..3].iter().any(|&v| v > 0x3F);
                                if is_8bit {
                                    for c in &mut entry[..3] {
                                        *c >>= 2;
                                    }
                                } else {
                                    for c in &mut entry[..3] {
                                        *c &= 0x3F;
                                    }
                                }
                            }
                        }
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
            let Some(edid) = aero_edid::read_edid(cpu.dx()) else {
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
