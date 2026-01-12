use crate::io::storage::{
    disk::{DiskBackend, DiskError, DiskResult},
    SECTOR_SIZE,
};

use aero_io_snapshot::io::storage::state::{IdeAtaDeviceState, MAX_IDE_DATA_BUFFER_BYTES};

pub struct AtaDevice {
    backend: Box<dyn DiskBackend>,
    model: String,
    serial: String,
    firmware: String,
    udma_mode: u8,
}

impl AtaDevice {
    pub fn new(backend: Box<dyn DiskBackend>, model: impl Into<String>) -> Self {
        Self {
            backend,
            model: model.into(),
            serial: "AERO0000000000000001".to_string(),
            firmware: "0.1".to_string(),
            udma_mode: 2,
        }
    }

    pub fn supports_dma(&self) -> bool {
        true
    }

    pub fn sector_bytes(&self, sectors: u64) -> DiskResult<Vec<u8>> {
        let len = sectors
            .checked_mul(SECTOR_SIZE as u64)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(DiskError::InvalidBufferLength)?;
        if len > MAX_IDE_DATA_BUFFER_BYTES {
            return Err(DiskError::InvalidBufferLength);
        }
        Ok(vec![0u8; len])
    }

    pub fn identify_data(&self) -> Vec<u8> {
        let mut words = [0u16; 256];

        // Word 0: general configuration (non-removable ATA device).
        words[0] = 0x0040;

        // Fake CHS geometry (legacy).
        words[1] = 16383;
        words[3] = 16;
        words[6] = 63;

        // Words 10-19: serial number (20 bytes).
        write_ata_string(&mut words[10..20], &self.serial, 20);

        // Words 23-26: firmware revision (8 bytes).
        write_ata_string(&mut words[23..27], &self.firmware, 8);

        // Words 27-46: model number (40 bytes).
        write_ata_string(&mut words[27..47], &self.model, 40);

        // Word 47: max sectors per interrupt (0 = vendor specific) + multiple mode.
        words[47] = 0x8000;

        // Word 49: capabilities: LBA + DMA.
        words[49] = (1 << 9) | (1 << 8);

        // Word 53: words 54-58 and 88 are valid.
        words[53] = 0x0006;

        // Word 60-61: total LBA28 sectors (clamped to 28-bit max).
        let total_sectors = self.backend.total_sectors();
        let lba28 = total_sectors.min(0x0FFF_FFFF) as u32;
        words[60] = (lba28 & 0xFFFF) as u16;
        words[61] = (lba28 >> 16) as u16;

        // Word 63: Multiword DMA modes supported (mode 2) + selected.
        words[63] = 0x0004 | 0x0400;

        // Word 80: major version number (ATA/ATAPI-6).
        words[80] = 0x007E;

        // Word 82-84: supported command sets.
        words[82] = 1 << 14; // NOP
        words[83] = 1 << 10; // LBA48

        // Word 85-87: enabled command sets.
        words[85] = words[82];
        words[86] = 0;
        words[87] = words[83];

        // Word 88: Ultra DMA modes supported + selected.
        // Support up to mode 2 by default.
        let udma_supported = 0x0007;
        let udma_mode = self.udma_mode.min(7);
        let udma_selected = 1u16 << (8 + udma_mode as u16);
        words[88] = udma_supported | udma_selected;

        // Words 100-103: total LBA48 sectors.
        let lba48 = total_sectors;
        words[100] = (lba48 & 0xFFFF) as u16;
        words[101] = ((lba48 >> 16) & 0xFFFF) as u16;
        words[102] = ((lba48 >> 32) & 0xFFFF) as u16;
        words[103] = ((lba48 >> 48) & 0xFFFF) as u16;

        // Word 106: physical/logical sector size (512 bytes logical).
        words[106] = 0x6000;

        let mut out = vec![0u8; 512];
        for (i, w) in words.iter().enumerate() {
            out[i * 2..i * 2 + 2].copy_from_slice(&w.to_le_bytes());
        }
        out
    }

    pub fn pio_read(&mut self, lba: u64, sectors: u64) -> DiskResult<Vec<u8>> {
        if self.backend.sector_size() != SECTOR_SIZE as u32 {
            return Err(DiskError::InvalidBufferLength);
        }
        let len = sectors
            .checked_mul(SECTOR_SIZE as u64)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(DiskError::InvalidBufferLength)?;
        if len > MAX_IDE_DATA_BUFFER_BYTES {
            return Err(DiskError::InvalidBufferLength);
        }
        let mut buf = vec![0u8; len];
        self.backend.read_sectors(lba, &mut buf)?;
        Ok(buf)
    }

    pub fn pio_write(&mut self, lba: u64, sectors: u64, data: &[u8]) -> DiskResult<()> {
        if self.backend.sector_size() != SECTOR_SIZE as u32 {
            return Err(DiskError::InvalidBufferLength);
        }
        let expected = sectors
            .checked_mul(SECTOR_SIZE as u64)
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(DiskError::InvalidBufferLength)?;
        if expected > MAX_IDE_DATA_BUFFER_BYTES {
            return Err(DiskError::InvalidBufferLength);
        }
        let slice = if data.len() >= expected {
            &data[..expected]
        } else {
            data
        };
        self.backend.write_sectors(lba, slice)?;
        Ok(())
    }

    pub fn flush(&mut self) -> DiskResult<()> {
        self.backend.flush()
    }

    pub fn set_features(&mut self, feature: u8, sector_count: u8) {
        // Compatibility-first: accept common SET FEATURES subcommands used by
        // Windows IDE drivers. Unsupported subcommands are treated as no-ops.
        match feature {
            0x03 => {
                // Set transfer mode (sector_count encodes the desired mode).
                if (sector_count & 0xF8) == 0x40 {
                    self.udma_mode = sector_count & 0x07;
                }
            }
            0x02 | 0x82 => {
                // Enable/disable write cache.
            }
            _ => {}
        }
    }

    pub fn snapshot_state(&self) -> IdeAtaDeviceState {
        IdeAtaDeviceState {
            udma_mode: self.udma_mode,
        }
    }

    pub fn restore_state(&mut self, state: &IdeAtaDeviceState) {
        self.udma_mode = state.udma_mode.min(7);
    }
}

fn write_ata_string(dst_words: &mut [u16], src: &str, byte_len: usize) {
    let mut bytes = vec![b' '; byte_len];
    let src_bytes = src.as_bytes();
    let copy_len = src_bytes.len().min(byte_len);
    bytes[..copy_len].copy_from_slice(&src_bytes[..copy_len]);

    for (i, word) in dst_words.iter_mut().enumerate() {
        let idx = i * 2;
        if idx + 1 >= bytes.len() {
            break;
        }
        // ATA strings are stored with bytes swapped within each 16-bit word.
        *word = u16::from_be_bytes([bytes[idx], bytes[idx + 1]]);
    }
}
