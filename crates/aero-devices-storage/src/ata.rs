use std::fmt;
use std::io;

use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_storage::{DiskError, VirtualDisk, SECTOR_SIZE};

pub const ATA_STATUS_BSY: u8 = 0x80;
pub const ATA_STATUS_DRDY: u8 = 0x40;
pub const ATA_STATUS_DF: u8 = 0x20;
pub const ATA_STATUS_DSC: u8 = 0x10;
pub const ATA_STATUS_DRQ: u8 = 0x08;
pub const ATA_STATUS_ERR: u8 = 0x01;

pub const ATA_ERROR_ABRT: u8 = 0x04;

pub const ATA_CMD_IDENTIFY: u8 = 0xEC;
pub const ATA_CMD_IDENTIFY_PACKET: u8 = 0xA1;
pub const ATA_CMD_PACKET: u8 = 0xA0;
pub const ATA_CMD_READ_SECTORS: u8 = 0x20;
pub const ATA_CMD_READ_SECTORS_EXT: u8 = 0x24;
pub const ATA_CMD_READ_DMA: u8 = 0xC8;
pub const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
pub const ATA_CMD_WRITE_SECTORS: u8 = 0x30;
pub const ATA_CMD_WRITE_SECTORS_EXT: u8 = 0x34;
pub const ATA_CMD_WRITE_DMA: u8 = 0xCA;
pub const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
pub const ATA_CMD_FLUSH_CACHE: u8 = 0xE7;
pub const ATA_CMD_FLUSH_CACHE_EXT: u8 = 0xEA;
pub const ATA_CMD_SET_FEATURES: u8 = 0xEF;

/// Highest Ultra DMA mode we advertise and accept.
///
/// This is primarily guest-visible through IDENTIFY word 88 and `SET FEATURES / subcommand 0x03`.
/// PIIX3-era controllers commonly top out at UDMA2 (UDMA/33), so we default to that.
pub const ATA_MAX_UDMA_MODE: u8 = 2;

/// Highest Multiword DMA mode we advertise and accept.
pub const ATA_MAX_MWDMA_MODE: u8 = 2;

const ATA_SUPPORTED_UDMA_MASK: u8 = (1u8 << (ATA_MAX_UDMA_MODE + 1)) - 1;
const ATA_SUPPORTED_MWDMA_MASK: u8 = (1u8 << (ATA_MAX_MWDMA_MODE + 1)) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtaTransferModeSelectError(pub u8);

impl fmt::Display for AtaTransferModeSelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid ATA transfer mode select byte 0x{:02x}", self.0)
    }
}

impl std::error::Error for AtaTransferModeSelectError {}
pub struct AtaDrive {
    disk: Box<dyn VirtualDisk>,
    identify: [u8; SECTOR_SIZE],
    write_cache_enabled: bool,
    /// Current negotiated DMA transfer mode as selected via SET FEATURES / 0x03.
    ///
    /// We only model the mode bits as guest-visible state (IDENTIFY + snapshots). Transfer
    /// semantics are not currently affected.
    udma_enabled: bool,
    /// Currently-selected Ultra DMA mode number (0..=6 by ATA spec). This is only meaningful when
    /// `udma_enabled` is true.
    ///
    /// We clamp accepted modes to [`ATA_MAX_UDMA_MODE`].
    udma_mode: u8,
    /// Currently-selected Multiword DMA mode number (0..=2 by ATA spec). This is only meaningful
    /// when `udma_enabled` is false (i.e. MWDMA selected).
    mwdma_mode: u8,
}

impl AtaDrive {
    pub fn new(disk: Box<dyn VirtualDisk>) -> io::Result<Self> {
        let capacity = disk.capacity_bytes();
        if !capacity.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "disk capacity is not a multiple of 512-byte sectors",
            ));
        }

        let sector_count = capacity / SECTOR_SIZE as u64;
        let identify = build_identify_sector(sector_count);

        let mut drive = Self {
            disk,
            identify,
            write_cache_enabled: true,
            // Conservative default that matches the previous hard-coded snapshot value.
            udma_enabled: true,
            udma_mode: 2,
            mwdma_mode: 0,
        };
        drive.update_identify_transfer_mode_words();
        drive.sync_identify_write_cache_enabled();

        Ok(drive)
    }

    fn set_identify_word(&mut self, word: usize, value: u16) {
        let start = word * 2;
        self.identify[start..start + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn update_identify_transfer_mode_words(&mut self) {
        // Word 63: Multiword DMA modes supported/selected.
        //
        // Lower byte: supported (bit 0 => mode0, bit 1 => mode1, bit2 => mode2).
        // Upper byte: active (bit 8 => mode0, bit9 => mode1, bit10 => mode2).
        let mwdma_supported = u16::from(ATA_SUPPORTED_MWDMA_MASK);
        let mwdma_active = if !self.udma_enabled && self.mwdma_mode <= ATA_MAX_MWDMA_MODE {
            1u16 << (8 + self.mwdma_mode)
        } else {
            0
        };
        self.set_identify_word(63, mwdma_supported | mwdma_active);

        // Word 88: Ultra DMA modes supported/selected.
        //
        // Lower byte: supported (bit 0 => UDMA0 …).
        // Upper byte: active.
        let udma_supported = u16::from(ATA_SUPPORTED_UDMA_MASK);
        let udma_active = if self.udma_enabled && self.udma_mode <= ATA_MAX_UDMA_MODE {
            1u16 << (8 + self.udma_mode)
        } else {
            0
        };
        self.set_identify_word(88, udma_supported | udma_active);
    }

    /// Apply the ATA SET FEATURES (subcommand 0x03) transfer mode select byte.
    ///
    /// The transfer mode select byte is written to the Sector Count register.
    pub fn set_transfer_mode_select(
        &mut self,
        mode_select: u8,
    ) -> Result<(), AtaTransferModeSelectError> {
        match mode_select {
            0x40..=0x47 => {
                // Ultra DMA: 0x40 | mode
                let mode = mode_select & 0x07;
                if mode > ATA_MAX_UDMA_MODE {
                    return Err(AtaTransferModeSelectError(mode_select));
                }
                self.udma_enabled = true;
                self.udma_mode = mode;
            }
            0x20..=0x27 => {
                // Multiword DMA: 0x20 | mode
                let mode = mode_select & 0x07;
                if mode > ATA_MAX_MWDMA_MODE {
                    return Err(AtaTransferModeSelectError(mode_select));
                }
                self.udma_enabled = false;
                self.mwdma_mode = mode;
            }
            _ => return Err(AtaTransferModeSelectError(mode_select)),
        }

        self.update_identify_transfer_mode_words();
        Ok(())
    }

    pub fn sector_count(&self) -> u64 {
        self.disk.capacity_bytes() / SECTOR_SIZE as u64
    }

    pub fn identify_sector(&self) -> &[u8; SECTOR_SIZE] {
        &self.identify
    }

    pub fn read_sectors(&mut self, lba: u64, buffer: &mut [u8]) -> io::Result<()> {
        self.disk.read_sectors(lba, buffer).map_err(map_disk_error)
    }

    pub fn write_sectors(&mut self, lba: u64, buffer: &[u8]) -> io::Result<()> {
        self.disk.write_sectors(lba, buffer).map_err(map_disk_error)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.disk.flush().map_err(map_disk_error)
    }

    pub fn set_write_cache_enabled(&mut self, enabled: bool) {
        self.write_cache_enabled = enabled;
        self.sync_identify_write_cache_enabled();
    }

    pub fn write_cache_enabled(&self) -> bool {
        self.write_cache_enabled
    }

    fn sync_identify_write_cache_enabled(&mut self) {
        // ATA IDENTIFY words:
        // - Word 82: command set supported
        // - Word 85: command set enabled
        //
        // Bit 5 in both words is the write cache feature.
        const WORD_CMD_SET_SUPPORTED: usize = 82;
        const WORD_CMD_SET_ENABLED: usize = 85;
        const WRITE_CACHE_BIT: u16 = 1 << 5;

        let supported = self.identify_word(WORD_CMD_SET_SUPPORTED) & WRITE_CACHE_BIT != 0;
        let mut enabled_word = self.identify_word(WORD_CMD_SET_ENABLED);
        enabled_word &= !WRITE_CACHE_BIT;
        if supported && self.write_cache_enabled {
            enabled_word |= WRITE_CACHE_BIT;
        }
        self.set_identify_word(WORD_CMD_SET_ENABLED, enabled_word);
    }

    fn identify_word(&self, idx: usize) -> u16 {
        let off = idx * 2;
        u16::from_le_bytes([self.identify[off], self.identify[off + 1]])
    }

    pub fn snapshot_state(&self) -> aero_io_snapshot::io::storage::state::IdeAtaDeviceState {
        // Snapshots historically tracked only the UDMA mode number for IDE ATA devices.
        //
        // We preserve that shape for compatibility, and encode non-UDMA modes by setting the high
        // bit. This keeps the common case (UDMA enabled) stable (0..=6) while allowing snapshots
        // to round-trip Multiword DMA selections deterministically.
        //
        // Legacy snapshots created before Multiword DMA was represented may store `0xFF` as a
        // sentinel for "UDMA disabled"; we continue to accept that on restore.
        aero_io_snapshot::io::storage::state::IdeAtaDeviceState {
            udma_mode: if self.udma_enabled {
                self.udma_mode
            } else {
                0x80 | (self.mwdma_mode & 0x07)
            },
        }
    }

    pub fn restore_state(
        &mut self,
        state: &aero_io_snapshot::io::storage::state::IdeAtaDeviceState,
    ) {
        // Restore negotiated transfer mode state (guest-visible via IDENTIFY and snapshots).
        if state.udma_mode == 0xFF {
            // Legacy sentinel: UDMA disabled, MWDMA mode unknown.
            self.udma_enabled = false;
            self.mwdma_mode = 0;
        } else if (state.udma_mode & 0x80) != 0 {
            // New encoding: high bit indicates MWDMA selection.
            self.udma_enabled = false;
            self.mwdma_mode = (state.udma_mode & 0x07).min(ATA_MAX_MWDMA_MODE);
        } else {
            self.udma_enabled = true;
            self.udma_mode = state.udma_mode.min(ATA_MAX_UDMA_MODE);
        }
        self.update_identify_transfer_mode_words();
    }
}

impl fmt::Debug for AtaDrive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtaDrive")
            .field("sector_count", &self.sector_count())
            .field("write_cache_enabled", &self.write_cache_enabled)
            .field("udma_enabled", &self.udma_enabled)
            .field("udma_mode", &self.udma_mode)
            .field("mwdma_mode", &self.mwdma_mode)
            .finish()
    }
}

impl IoSnapshot for AtaDrive {
    const DEVICE_ID: [u8; 4] = *b"ATAD";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_SECTOR_COUNT: u16 = 1;
        const TAG_WRITE_CACHE_ENABLED: u16 = 2;
        const TAG_UDMA_ENABLED: u16 = 3;
        const TAG_UDMA_MODE: u16 = 4;
        const TAG_MWDMA_MODE: u16 = 5;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u64(TAG_SECTOR_COUNT, self.sector_count());
        w.field_bool(TAG_WRITE_CACHE_ENABLED, self.write_cache_enabled);
        w.field_bool(TAG_UDMA_ENABLED, self.udma_enabled);
        w.field_u8(TAG_UDMA_MODE, self.udma_mode);
        w.field_u8(TAG_MWDMA_MODE, self.mwdma_mode);
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_SECTOR_COUNT: u16 = 1;
        const TAG_WRITE_CACHE_ENABLED: u16 = 2;
        const TAG_UDMA_ENABLED: u16 = 3;
        const TAG_UDMA_MODE: u16 = 4;
        const TAG_MWDMA_MODE: u16 = 5;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(sector_count) = r.u64(TAG_SECTOR_COUNT)? {
            if sector_count != self.sector_count() {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "ata sector_count mismatch",
                ));
            }
        }
        if let Some(enabled) = r.bool(TAG_WRITE_CACHE_ENABLED)? {
            self.set_write_cache_enabled(enabled);
        }

        if let Some(udma_enabled) = r.bool(TAG_UDMA_ENABLED)? {
            self.udma_enabled = udma_enabled;
        }
        if let Some(udma_mode) = r.u8(TAG_UDMA_MODE)? {
            if udma_mode > ATA_MAX_UDMA_MODE {
                return Err(SnapshotError::InvalidFieldEncoding("ata udma_mode"));
            }
            self.udma_mode = udma_mode;
        }
        if let Some(mwdma_mode) = r.u8(TAG_MWDMA_MODE)? {
            if mwdma_mode > ATA_MAX_MWDMA_MODE {
                return Err(SnapshotError::InvalidFieldEncoding("ata mwdma_mode"));
            }
            self.mwdma_mode = mwdma_mode;
        }
        self.update_identify_transfer_mode_words();

        Ok(())
    }
}

fn build_identify_sector(sector_count: u64) -> [u8; SECTOR_SIZE] {
    // ATA IDENTIFY DEVICE is a 256-word structure in little-endian word order.
    // String fields are stored with bytes swapped within each word.
    let mut words = [0u16; 256];

    // Word 0: general configuration: non-removable, hard disk.
    words[0] = 0x0040;

    // Provide plausible geometry (mostly ignored when LBA is supported).
    words[1] = 16383; // cylinders
    words[3] = 16; // heads
    words[6] = 63; // sectors/track

    // Words 10-19: serial number (20 bytes).
    write_ata_string(&mut words, 10, 10, "AERO0000000000000000");

    // Words 23-26: firmware revision (8 bytes).
    write_ata_string(&mut words, 23, 4, "0.1");

    // Words 27-46: model number (40 bytes).
    write_ata_string(&mut words, 27, 20, "Aero Virtual ATA Disk");

    // Word 47: max sectors per interrupt on READ/WRITE MULTIPLE. We don't implement it.
    words[47] = 0;

    // Word 49: capabilities.
    // Bit 9: LBA supported, Bit 8: DMA supported.
    words[49] = (1 << 9) | (1 << 8);

    // Word 53: field validity.
    //
    // Set bit 2 to indicate word 88 (UDMA modes) is valid. Some guests consult this bit before
    // parsing word 88.
    words[53] = 1 << 2;

    // Word 60-61: total number of user addressable sectors for 28-bit.
    let lba28 = sector_count.min(u32::MAX as u64) as u32;
    words[60] = (lba28 & 0xFFFF) as u16;
    words[61] = (lba28 >> 16) as u16;

    // Words 63: multiword DMA modes supported/selected. Filled in by
    // `AtaDrive::update_identify_transfer_mode_words()` since it depends on negotiated state.
    words[63] = 0;

    // Words 80: major version number.
    words[80] = 0x007E; // up to ATA/ATAPI-8 (a loose claim, but common in emulators)

    // Words 82-84: command set supported. Mark FLUSH CACHE and SET FEATURES as supported.
    words[82] = 1 << 5; // write cache
    words[83] = 1 << 10; // 48-bit addressing supported.
    words[84] = 0;

    // Words 85-87: command set enabled.
    words[85] = words[82];
    words[86] = words[83];
    words[87] = words[84];

    // Words 88: Ultra DMA modes supported/selected. Filled in by
    // `AtaDrive::update_identify_transfer_mode_words()`.
    words[88] = 0;

    // Words 100-103: total number of user addressable sectors for 48-bit.
    let lba48 = sector_count;
    words[100] = (lba48 & 0xFFFF) as u16;
    words[101] = ((lba48 >> 16) & 0xFFFF) as u16;
    words[102] = ((lba48 >> 32) & 0xFFFF) as u16;
    words[103] = ((lba48 >> 48) & 0xFFFF) as u16;

    let mut out = [0u8; SECTOR_SIZE];
    for (idx, word) in words.into_iter().enumerate() {
        out[idx * 2..idx * 2 + 2].copy_from_slice(&word.to_le_bytes());
    }
    out
}

fn map_disk_error(err: DiskError) -> io::Error {
    // Storage controllers surface errors via ATA status registers rather than rich error codes.
    // Map any disk-layer error to an opaque I/O failure for the device logic.
    io::Error::other(err)
}

fn write_ata_string(words: &mut [u16; 256], start: usize, len_words: usize, s: &str) {
    let mut bytes = vec![b' '; len_words * 2];
    let s_bytes = s.as_bytes();
    let copy_len = s_bytes.len().min(bytes.len());
    bytes[..copy_len].copy_from_slice(&s_bytes[..copy_len]);

    for i in 0..len_words {
        // ATA strings are stored as big-endian within each 16-bit word.
        let hi = bytes[i * 2];
        let lo = bytes[i * 2 + 1];
        words[start + i] = u16::from_be_bytes([hi, lo]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_io_snapshot::io::state::{IoSnapshot, SnapshotError};
    use aero_storage::{MemBackend, RawDisk};

    #[test]
    fn identify_sector_contains_capacity() {
        let capacity = 1024 * SECTOR_SIZE as u64;
        let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let drive = AtaDrive::new(Box::new(disk)).unwrap();
        let id = drive.identify_sector();

        // Word 60-61 should contain 1024 sectors (fits in 28-bit).
        let w60 = u16::from_le_bytes([id[120], id[121]]) as u32;
        let w61 = u16::from_le_bytes([id[122], id[123]]) as u32;
        let lba28 = w60 | (w61 << 16);
        assert_eq!(lba28, 1024);
    }

    #[test]
    fn snapshot_roundtrip_preserves_write_cache_enabled() {
        let capacity = 16 * SECTOR_SIZE as u64;
        let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let mut drive = AtaDrive::new(Box::new(disk)).unwrap();
        drive.set_write_cache_enabled(false);

        let snap = drive.save_state();

        let disk2 = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let mut restored = AtaDrive::new(Box::new(disk2)).unwrap();
        assert!(restored.write_cache_enabled());
        restored.load_state(&snap).unwrap();
        assert!(!restored.write_cache_enabled());

        // IDENTIFY DEVICE word 85 bit 5 should reflect restored write-cache state.
        let id = restored.identify_sector();
        let w85 = u16::from_le_bytes([id[170], id[171]]);
        assert_eq!(w85 & (1 << 5), 0);
    }

    #[test]
    fn snapshot_rejects_sector_count_mismatch() {
        let disk = RawDisk::create(MemBackend::new(), 16 * SECTOR_SIZE as u64).unwrap();
        let drive = AtaDrive::new(Box::new(disk)).unwrap();
        let snap = drive.save_state();

        let disk2 = RawDisk::create(MemBackend::new(), 32 * SECTOR_SIZE as u64).unwrap();
        let mut restored = AtaDrive::new(Box::new(disk2)).unwrap();
        let err = restored.load_state(&snap).unwrap_err();
        assert_eq!(
            err,
            SnapshotError::InvalidFieldEncoding("ata sector_count mismatch")
        );
    }

    #[test]
    fn identify_sector_reflects_write_cache_enabled() {
        let capacity = 16 * SECTOR_SIZE as u64;
        let disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
        let mut drive = AtaDrive::new(Box::new(disk)).unwrap();

        fn word(id: &[u8; SECTOR_SIZE], idx: usize) -> u16 {
            let off = idx * 2;
            u16::from_le_bytes([id[off], id[off + 1]])
        }

        let mut id_enabled = [0u8; SECTOR_SIZE];
        id_enabled.copy_from_slice(drive.identify_sector());
        // Word 82 reports support; it should remain stable.
        assert_ne!(word(&id_enabled, 82) & (1 << 5), 0);
        assert_ne!(word(&id_enabled, 85) & (1 << 5), 0);

        drive.set_write_cache_enabled(false);
        let mut id_disabled = [0u8; SECTOR_SIZE];
        id_disabled.copy_from_slice(drive.identify_sector());
        assert_ne!(word(&id_disabled, 82) & (1 << 5), 0);
        assert_eq!(word(&id_disabled, 85) & (1 << 5), 0);
        assert_ne!(
            id_enabled, id_disabled,
            "IDENTIFY data should change when cache toggles"
        );

        drive.set_write_cache_enabled(true);
        let mut id_enabled_again = [0u8; SECTOR_SIZE];
        id_enabled_again.copy_from_slice(drive.identify_sector());
        assert_ne!(word(&id_enabled_again, 82) & (1 << 5), 0);
        assert_ne!(word(&id_enabled_again, 85) & (1 << 5), 0);
        assert_eq!(
            id_enabled, id_enabled_again,
            "IDENTIFY data should return to its original value after toggling back"
        );
    }
}
