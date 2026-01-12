use std::io::{Cursor, Read};

use crate::palette::Rgb;
use crate::VgaDevice;

/// Errors returned when decoding VGA snapshot payloads.
#[derive(Debug)]
pub enum VgaSnapshotError {
    Io(std::io::Error),
    Corrupt(&'static str),
}

impl std::fmt::Display for VgaSnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "i/o error: {err}"),
            Self::Corrupt(msg) => write!(f, "corrupt VGA snapshot: {msg}"),
        }
    }
}

impl std::error::Error for VgaSnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Corrupt(_) => None,
        }
    }
}

impl From<std::io::Error> for VgaSnapshotError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

pub type Result<T> = std::result::Result<T, VgaSnapshotError>;

const VGA_SNAPSHOT_V1_MAX_VRAM_LEN: u32 = 64 * 1024 * 1024;

fn read_u8<R: Read>(r: &mut R) -> Result<u8> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn read_bool<R: Read>(r: &mut R) -> Result<bool> {
    match read_u8(r)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(VgaSnapshotError::Corrupt("invalid bool")),
    }
}

fn read_u16_le<R: Read>(r: &mut R) -> Result<u16> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32_le<R: Read>(r: &mut R) -> Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_array<const N: usize, R: Read>(r: &mut R) -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Snapshot payload for [`crate::VgaDevice`] (version 1).
///
/// This is designed to be embedded inside `aero_snapshot::DeviceState` (`DeviceId::VGA`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VgaSnapshotV1 {
    pub misc_output: u8,

    pub sequencer_index: u8,
    pub sequencer: [u8; 5],

    pub graphics_index: u8,
    pub graphics: [u8; 9],

    pub crtc_index: u8,
    pub crtc: [u8; 25],

    pub attribute_index: u8,
    pub attribute_flip_flop_data: bool,
    pub attribute: [u8; 21],
    pub input_status1_vretrace: bool,

    pub pel_mask: u8,
    pub dac_write_index: u8,
    pub dac_write_subindex: u8,
    pub dac_read_index: u8,
    pub dac_read_subindex: u8,
    /// 256 DAC entries, stored as `[r, g, b]` in 8-bit host values.
    pub dac: [[u8; 3]; 256],

    pub vbe_index: u16,
    pub vbe_xres: u16,
    pub vbe_yres: u16,
    pub vbe_bpp: u16,
    pub vbe_enable: u16,
    pub vbe_bank: u16,
    pub vbe_virt_width: u16,
    pub vbe_virt_height: u16,
    pub vbe_x_offset: u16,
    pub vbe_y_offset: u16,

    pub latches: [u8; 4],
    pub vram: Vec<u8>,
}

impl VgaSnapshotV1 {
    pub const VERSION: u16 = 1;

    /// Decode a snapshot payload produced by [`VgaDevice::encode_snapshot_v1`].
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut r = Cursor::new(bytes);

        let misc_output = read_u8(&mut r)?;
        let sequencer_index = read_u8(&mut r)?;
        let sequencer = read_array::<5, _>(&mut r)?;

        let graphics_index = read_u8(&mut r)?;
        let graphics = read_array::<9, _>(&mut r)?;

        let crtc_index = read_u8(&mut r)?;
        let crtc = read_array::<25, _>(&mut r)?;

        let attribute_index = read_u8(&mut r)?;
        let attribute_flip_flop_data = read_bool(&mut r)?;
        let attribute = read_array::<21, _>(&mut r)?;
        let input_status1_vretrace = read_bool(&mut r)?;

        let pel_mask = read_u8(&mut r)?;
        let dac_write_index = read_u8(&mut r)?;
        let dac_write_subindex = read_u8(&mut r)?;
        let dac_read_index = read_u8(&mut r)?;
        let dac_read_subindex = read_u8(&mut r)?;

        let mut dac = [[0u8; 3]; 256];
        for entry in &mut dac {
            *entry = read_array::<3, _>(&mut r)?;
        }

        let vbe_index = read_u16_le(&mut r)?;
        let vbe_xres = read_u16_le(&mut r)?;
        let vbe_yres = read_u16_le(&mut r)?;
        let vbe_bpp = read_u16_le(&mut r)?;
        let vbe_enable = read_u16_le(&mut r)?;
        let vbe_bank = read_u16_le(&mut r)?;
        let vbe_virt_width = read_u16_le(&mut r)?;
        let vbe_virt_height = read_u16_le(&mut r)?;
        let vbe_x_offset = read_u16_le(&mut r)?;
        let vbe_y_offset = read_u16_le(&mut r)?;

        let latches = read_array::<4, _>(&mut r)?;

        let vram_len = read_u32_le(&mut r)?;
        if vram_len > VGA_SNAPSHOT_V1_MAX_VRAM_LEN {
            return Err(VgaSnapshotError::Corrupt("vram too large"));
        }
        let mut vram = vec![0u8; vram_len as usize];
        r.read_exact(&mut vram)?;

        Ok(Self {
            misc_output,
            sequencer_index,
            sequencer,
            graphics_index,
            graphics,
            crtc_index,
            crtc,
            attribute_index,
            attribute_flip_flop_data,
            attribute,
            input_status1_vretrace,
            pel_mask,
            dac_write_index,
            dac_write_subindex,
            dac_read_index,
            dac_read_subindex,
            dac,
            vbe_index,
            vbe_xres,
            vbe_yres,
            vbe_bpp,
            vbe_enable,
            vbe_bank,
            vbe_virt_width,
            vbe_virt_height,
            vbe_x_offset,
            vbe_y_offset,
            latches,
            vram,
        })
    }

    /// Encode this snapshot payload (version 1).
    pub fn encode(&self) -> Vec<u8> {
        // Payload size:
        // - registers/palette are small (~1KiB)
        // - VRAM is large (default 16MiB).
        let mut out = Vec::with_capacity(self.vram.len() + 1024);
        out.push(self.misc_output);

        out.push(self.sequencer_index);
        out.extend_from_slice(&self.sequencer);

        out.push(self.graphics_index);
        out.extend_from_slice(&self.graphics);

        out.push(self.crtc_index);
        out.extend_from_slice(&self.crtc);

        out.push(self.attribute_index);
        out.push(self.attribute_flip_flop_data as u8);
        out.extend_from_slice(&self.attribute);
        out.push(self.input_status1_vretrace as u8);

        out.push(self.pel_mask);
        out.push(self.dac_write_index);
        out.push(self.dac_write_subindex);
        out.push(self.dac_read_index);
        out.push(self.dac_read_subindex);
        for rgb in &self.dac {
            out.extend_from_slice(rgb);
        }

        out.extend_from_slice(&self.vbe_index.to_le_bytes());
        out.extend_from_slice(&self.vbe_xres.to_le_bytes());
        out.extend_from_slice(&self.vbe_yres.to_le_bytes());
        out.extend_from_slice(&self.vbe_bpp.to_le_bytes());
        out.extend_from_slice(&self.vbe_enable.to_le_bytes());
        out.extend_from_slice(&self.vbe_bank.to_le_bytes());
        out.extend_from_slice(&self.vbe_virt_width.to_le_bytes());
        out.extend_from_slice(&self.vbe_virt_height.to_le_bytes());
        out.extend_from_slice(&self.vbe_x_offset.to_le_bytes());
        out.extend_from_slice(&self.vbe_y_offset.to_le_bytes());

        out.extend_from_slice(&self.latches);

        let vram_len: u32 = self
            .vram
            .len()
            .try_into()
            .unwrap_or(VGA_SNAPSHOT_V1_MAX_VRAM_LEN);
        out.extend_from_slice(&vram_len.to_le_bytes());
        out.extend_from_slice(&self.vram);

        out
    }
}

impl VgaDevice {
    /// Capture a full VGA snapshot (v1).
    ///
    /// Note: includes the full VRAM contents (16MiB by default).
    pub fn snapshot_v1(&self) -> VgaSnapshotV1 {
        let mut dac = [[0u8; 3]; 256];
        for (dst, src) in dac.iter_mut().zip(self.dac.iter()) {
            *dst = [src.r, src.g, src.b];
        }

        VgaSnapshotV1 {
            misc_output: self.misc_output,
            sequencer_index: self.sequencer_index,
            sequencer: self.sequencer,
            graphics_index: self.graphics_index,
            graphics: self.graphics,
            crtc_index: self.crtc_index,
            crtc: self.crtc,
            attribute_index: self.attribute_index,
            attribute_flip_flop_data: self.attribute_flip_flop_data,
            attribute: self.attribute,
            input_status1_vretrace: self.input_status1_vretrace,
            pel_mask: self.pel_mask,
            dac_write_index: self.dac_write_index,
            dac_write_subindex: self.dac_write_subindex,
            dac_read_index: self.dac_read_index,
            dac_read_subindex: self.dac_read_subindex,
            dac,
            vbe_index: self.vbe_index,
            vbe_xres: self.vbe.xres,
            vbe_yres: self.vbe.yres,
            vbe_bpp: self.vbe.bpp,
            vbe_enable: self.vbe.enable,
            vbe_bank: self.vbe.bank,
            vbe_virt_width: self.vbe.virt_width,
            vbe_virt_height: self.vbe.virt_height,
            vbe_x_offset: self.vbe.x_offset,
            vbe_y_offset: self.vbe.y_offset,
            latches: self.latches,
            vram: self.vram.clone(),
        }
    }

    /// Encode a snapshot payload (v1) directly to bytes.
    ///
    /// This is equivalent to `self.snapshot_v1().encode()` but avoids cloning VRAM twice.
    pub fn encode_snapshot_v1(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.vram.len() + 1024);
        out.push(self.misc_output);

        out.push(self.sequencer_index);
        out.extend_from_slice(&self.sequencer);

        out.push(self.graphics_index);
        out.extend_from_slice(&self.graphics);

        out.push(self.crtc_index);
        out.extend_from_slice(&self.crtc);

        out.push(self.attribute_index);
        out.push(self.attribute_flip_flop_data as u8);
        out.extend_from_slice(&self.attribute);
        out.push(self.input_status1_vretrace as u8);

        out.push(self.pel_mask);
        out.push(self.dac_write_index);
        out.push(self.dac_write_subindex);
        out.push(self.dac_read_index);
        out.push(self.dac_read_subindex);
        for rgb in &self.dac {
            out.extend_from_slice(&[rgb.r, rgb.g, rgb.b]);
        }

        out.extend_from_slice(&self.vbe_index.to_le_bytes());
        out.extend_from_slice(&self.vbe.xres.to_le_bytes());
        out.extend_from_slice(&self.vbe.yres.to_le_bytes());
        out.extend_from_slice(&self.vbe.bpp.to_le_bytes());
        out.extend_from_slice(&self.vbe.enable.to_le_bytes());
        out.extend_from_slice(&self.vbe.bank.to_le_bytes());
        out.extend_from_slice(&self.vbe.virt_width.to_le_bytes());
        out.extend_from_slice(&self.vbe.virt_height.to_le_bytes());
        out.extend_from_slice(&self.vbe.x_offset.to_le_bytes());
        out.extend_from_slice(&self.vbe.y_offset.to_le_bytes());

        out.extend_from_slice(&self.latches);

        let vram_len: u32 = self
            .vram
            .len()
            .try_into()
            .unwrap_or(VGA_SNAPSHOT_V1_MAX_VRAM_LEN);
        out.extend_from_slice(&vram_len.to_le_bytes());
        out.extend_from_slice(&self.vram);

        out
    }

    /// Restore a previously captured snapshot (v1).
    pub fn restore_snapshot_v1(&mut self, snap: &VgaSnapshotV1) {
        self.misc_output = snap.misc_output;

        self.sequencer_index = snap.sequencer_index;
        self.sequencer = snap.sequencer;

        self.graphics_index = snap.graphics_index;
        self.graphics = snap.graphics;

        self.crtc_index = snap.crtc_index;
        self.crtc = snap.crtc;

        self.attribute_index = snap.attribute_index;
        self.attribute_flip_flop_data = snap.attribute_flip_flop_data;
        self.attribute = snap.attribute;
        self.input_status1_vretrace = snap.input_status1_vretrace;

        self.pel_mask = snap.pel_mask;
        self.dac_write_index = snap.dac_write_index;
        self.dac_write_subindex = snap.dac_write_subindex;
        self.dac_read_index = snap.dac_read_index;
        self.dac_read_subindex = snap.dac_read_subindex;
        for (dst, src) in self.dac.iter_mut().zip(snap.dac.iter()) {
            *dst = Rgb {
                r: src[0],
                g: src[1],
                b: src[2],
            };
        }

        self.vbe_index = snap.vbe_index;
        self.vbe.xres = snap.vbe_xres;
        self.vbe.yres = snap.vbe_yres;
        self.vbe.bpp = snap.vbe_bpp;
        self.vbe.enable = snap.vbe_enable;
        self.vbe.bank = snap.vbe_bank;
        self.vbe.virt_width = snap.vbe_virt_width;
        self.vbe.virt_height = snap.vbe_virt_height;
        self.vbe.x_offset = snap.vbe_x_offset;
        self.vbe.y_offset = snap.vbe_y_offset;

        self.latches = snap.latches;

        if self.vram.len() == snap.vram.len() {
            self.vram.copy_from_slice(&snap.vram);
        } else {
            self.vram = snap.vram.clone();
        }

        // Force the next `present()` call to re-render from restored VRAM/register state.
        self.dirty = true;
    }
}
