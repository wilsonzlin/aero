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

pub struct AtaDrive {
    disk: Box<dyn VirtualDisk>,
    identify: [u8; SECTOR_SIZE],
    write_cache_enabled: bool,
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

        Ok(Self {
            disk,
            identify,
            write_cache_enabled: true,
        })
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
    }

    pub fn write_cache_enabled(&self) -> bool {
        self.write_cache_enabled
    }

    pub fn snapshot_state(&self) -> aero_io_snapshot::io::storage::state::IdeAtaDeviceState {
        // The current ATA model does not track UDMA configuration, but snapshots include it for
        // compatibility with other IDE implementations.
        aero_io_snapshot::io::storage::state::IdeAtaDeviceState { udma_mode: 2 }
    }

    pub fn restore_state(&mut self, _state: &aero_io_snapshot::io::storage::state::IdeAtaDeviceState) {
        // Nothing to restore currently; disk contents are managed by the backing `VirtualDisk`.
        let _ = &self.disk;
    }
}

impl fmt::Debug for AtaDrive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AtaDrive")
            .field("sector_count", &self.sector_count())
            .field("write_cache_enabled", &self.write_cache_enabled)
            .finish()
    }
}

impl IoSnapshot for AtaDrive {
    const DEVICE_ID: [u8; 4] = *b"ATAD";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_SECTOR_COUNT: u16 = 1;
        const TAG_WRITE_CACHE_ENABLED: u16 = 2;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u64(TAG_SECTOR_COUNT, self.sector_count());
        w.field_bool(TAG_WRITE_CACHE_ENABLED, self.write_cache_enabled);
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_SECTOR_COUNT: u16 = 1;
        const TAG_WRITE_CACHE_ENABLED: u16 = 2;

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
            self.write_cache_enabled = enabled;
        }

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

    // Word 60-61: total number of user addressable sectors for 28-bit.
    let lba28 = sector_count.min(u32::MAX as u64) as u32;
    words[60] = (lba28 & 0xFFFF) as u16;
    words[61] = (lba28 >> 16) as u16;

    // Words 63: multiword DMA modes supported.
    // Mark mode 0 supported.
    words[63] = 1;

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
    }

    #[test]
    fn snapshot_rejects_sector_count_mismatch() {
        let disk = RawDisk::create(MemBackend::new(), 16 * SECTOR_SIZE as u64).unwrap();
        let drive = AtaDrive::new(Box::new(disk)).unwrap();
        let snap = drive.save_state();

        let disk2 = RawDisk::create(MemBackend::new(), 32 * SECTOR_SIZE as u64).unwrap();
        let mut restored = AtaDrive::new(Box::new(disk2)).unwrap();
        let err = restored.load_state(&snap).unwrap_err();
        assert_eq!(err, SnapshotError::InvalidFieldEncoding("ata sector_count mismatch"));
    }
}
