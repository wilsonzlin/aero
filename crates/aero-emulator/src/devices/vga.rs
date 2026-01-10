#[derive(Debug, Clone)]
pub struct VgaDevice {
    mode: u8,
    text_cols: u8,
    text_rows: u8,
    text_buffer: Vec<u16>,
    gfx_width: u16,
    gfx_height: u16,
    gfx_buffer: Vec<u8>,
}

impl Default for VgaDevice {
    fn default() -> Self {
        let mut vga = Self {
            mode: 0x03,
            text_cols: 80,
            text_rows: 25,
            text_buffer: vec![0; 80 * 25],
            gfx_width: 320,
            gfx_height: 200,
            gfx_buffer: vec![0; 320 * 200],
        };

        vga.clear_text(0x07);
        vga
    }
}

impl VgaDevice {
    pub fn mode(&self) -> u8 {
        self.mode
    }

    pub fn text_dimensions(&self) -> (u8, u8) {
        (self.text_cols, self.text_rows)
    }

    pub fn graphics_dimensions(&self) -> (u16, u16) {
        (self.gfx_width, self.gfx_height)
    }

    pub fn set_mode(&mut self, mode: u8, clear: bool) -> Result<(), ()> {
        match mode {
            0x03 => {
                self.mode = mode;
                self.text_cols = 80;
                self.text_rows = 25;
                self.text_buffer
                    .resize(self.text_cols as usize * self.text_rows as usize, 0);
                if clear {
                    self.clear_text(0x07);
                }
                Ok(())
            }
            0x13 => {
                self.mode = mode;
                self.text_cols = 40;
                self.text_rows = 25;
                self.text_buffer
                    .resize(self.text_cols as usize * self.text_rows as usize, 0);
                self.gfx_width = 320;
                self.gfx_height = 200;
                self.gfx_buffer
                    .resize(self.gfx_width as usize * self.gfx_height as usize, 0);
                if clear {
                    self.gfx_buffer.fill(0);
                    self.clear_text(0x07);
                }
                Ok(())
            }
            _ => Err(()),
        }
    }

    pub fn clear_text(&mut self, attr: u8) {
        let cell = ((attr as u16) << 8) | b' ' as u16;
        self.text_buffer.fill(cell);
    }

    pub fn write_text_cell(&mut self, row: u8, col: u8, ch: u8, attr: u8) {
        if row >= self.text_rows || col >= self.text_cols {
            return;
        }
        let idx = row as usize * self.text_cols as usize + col as usize;
        self.text_buffer[idx] = ((attr as u16) << 8) | ch as u16;
    }

    pub fn read_text_cell(&self, row: u8, col: u8) -> (u8, u8) {
        if row >= self.text_rows || col >= self.text_cols {
            return (0, 0);
        }
        let idx = row as usize * self.text_cols as usize + col as usize;
        let cell = self.text_buffer[idx];
        (cell as u8, (cell >> 8) as u8)
    }

    pub fn scroll_text_window_up(
        &mut self,
        top: u8,
        left: u8,
        bottom: u8,
        right: u8,
        lines: u8,
        blank_attr: u8,
    ) {
        if self.text_cols == 0 || self.text_rows == 0 {
            return;
        }

        let top = top.min(self.text_rows - 1);
        let bottom = bottom.min(self.text_rows - 1);
        let left = left.min(self.text_cols - 1);
        let right = right.min(self.text_cols - 1);

        if bottom < top || right < left {
            return;
        }

        let height = bottom - top + 1;
        let lines = if lines == 0 {
            height
        } else {
            lines.min(height)
        };
        let blank_cell = ((blank_attr as u16) << 8) | b' ' as u16;

        for row in top..=bottom {
            for col in left..=right {
                let dst_idx = row as usize * self.text_cols as usize + col as usize;
                let src_row = row.saturating_add(lines);
                if src_row <= bottom {
                    let src_idx = src_row as usize * self.text_cols as usize + col as usize;
                    self.text_buffer[dst_idx] = self.text_buffer[src_idx];
                } else {
                    self.text_buffer[dst_idx] = blank_cell;
                }
            }
        }
    }

    pub fn graphics_vram_mut(&mut self) -> &mut [u8] {
        &mut self.gfx_buffer
    }

    pub fn graphics_vram(&self) -> &[u8] {
        &self.gfx_buffer
    }

    pub fn write_pixel(&mut self, x: u16, y: u16, color: u8) {
        if x >= self.gfx_width || y >= self.gfx_height {
            return;
        }
        let idx = y as usize * self.gfx_width as usize + x as usize;
        self.gfx_buffer[idx] = color;
    }

    pub fn read_pixel(&self, x: u16, y: u16) -> u8 {
        if x >= self.gfx_width || y >= self.gfx_height {
            return 0;
        }
        let idx = y as usize * self.gfx_width as usize + x as usize;
        self.gfx_buffer[idx]
    }
}
