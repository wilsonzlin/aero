mod error;
mod format;
mod io;
mod ram;
mod types;

#[cfg(feature = "io-snapshot")]
pub mod io_snapshot_bridge;

pub use crate::error::{Result, SnapshotError};
pub use crate::format::{
    DeviceId, SectionId, SNAPSHOT_ENDIANNESS_LITTLE, SNAPSHOT_MAGIC, SNAPSHOT_VERSION_V1,
};
pub use crate::ram::{Compression, RamMode, RamWriteOptions};
pub use crate::types::{
    CpuState, DeviceState, DiskOverlayRef, DiskOverlayRefs, MmuState, SnapshotMeta,
};

use std::io::{Read, Seek, SeekFrom, Write};

use crate::io::{ReadLeExt, WriteLeExt};

#[derive(Debug, Clone, Copy)]
pub struct SaveOptions {
    pub ram: RamWriteOptions,
}

impl Default for SaveOptions {
    fn default() -> Self {
        Self {
            ram: RamWriteOptions::default(),
        }
    }
}

pub trait SnapshotSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta;
    fn cpu_state(&self) -> CpuState;
    fn mmu_state(&self) -> MmuState;
    fn device_states(&self) -> Vec<DeviceState>;
    fn disk_overlays(&self) -> DiskOverlayRefs;

    fn ram_len(&self) -> usize;
    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<()>;

    /// Return and clear the set of dirty pages since the last snapshot. Each page index is
    /// `offset / page_size` where `page_size` is `SaveOptions.ram.page_size`.
    fn take_dirty_pages(&mut self) -> Option<Vec<u64>>;
}

pub trait SnapshotTarget {
    fn restore_meta(&mut self, _meta: SnapshotMeta) {}
    fn restore_cpu_state(&mut self, state: CpuState);
    fn restore_mmu_state(&mut self, state: MmuState);
    fn restore_device_states(&mut self, states: Vec<DeviceState>);
    fn restore_disk_overlays(&mut self, overlays: DiskOverlayRefs);

    fn ram_len(&self) -> usize;
    fn write_ram(&mut self, offset: u64, data: &[u8]) -> Result<()>;

    fn post_restore(&mut self) -> Result<()> {
        Ok(())
    }
}

pub fn save_snapshot<W: Write + Seek, S: SnapshotSource>(
    w: &mut W,
    source: &mut S,
    options: SaveOptions,
) -> Result<()> {
    write_file_header(w)?;

    write_section(w, SectionId::META, 1, 0, |w| {
        let meta = source.snapshot_meta();
        meta.encode(w)
    })?;

    write_section(w, SectionId::CPU, 1, 0, |w| source.cpu_state().encode(w))?;

    write_section(w, SectionId::MMU, 1, 0, |w| source.mmu_state().encode(w))?;

    write_section(w, SectionId::DEVICES, 1, 0, |w| {
        let mut devices = source.device_states();
        devices.sort_by_key(|device| (device.id.0, device.version, device.flags));
        let count: u32 = devices
            .len()
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("too many devices"))?;
        w.write_u32_le(count)?;
        for device in devices {
            device.encode(w)?;
        }
        Ok(())
    })?;

    write_section(w, SectionId::DISKS, 1, 0, |w| {
        let mut disks = source.disk_overlays();
        disks.disks.sort_by_key(|disk| disk.disk_id);
        disks.encode(w)
    })?;

    write_section(w, SectionId::RAM, 1, 0, |w| {
        let total_len = source.ram_len() as u64;

        let dirty_pages = match options.ram.mode {
            RamMode::Full => {
                // Clearing dirty bits on full snapshots makes incremental snapshots deterministic.
                let _ = source.take_dirty_pages();
                None
            }
            RamMode::Dirty => {
                let mut dirty_pages = source
                    .take_dirty_pages()
                    .ok_or(SnapshotError::Corrupt("dirty-page tracking not available"))?;
                dirty_pages.sort_unstable();
                dirty_pages.dedup();

                let page_size = u64::from(options.ram.page_size);
                if page_size == 0 {
                    return Err(SnapshotError::Corrupt("invalid page size"));
                }
                let max_pages = total_len
                    .checked_add(page_size - 1)
                    .ok_or(SnapshotError::Corrupt("ram length overflow"))?
                    / page_size;
                if dirty_pages.iter().any(|&page_idx| page_idx >= max_pages) {
                    return Err(SnapshotError::Corrupt("dirty page out of range"));
                }

                Some(dirty_pages)
            }
        };

        ram::encode_ram_section(
            w,
            total_len,
            options.ram,
            dirty_pages.as_deref(),
            |offset, buf| source.read_ram(offset, buf),
        )
    })?;

    Ok(())
}

pub fn restore_snapshot<R: Read, T: SnapshotTarget>(r: &mut R, target: &mut T) -> Result<()> {
    read_file_header(r)?;

    const MAX_DEVICES_SECTION_LEN: u64 = 256 * 1024 * 1024;
    const MAX_DEVICE_COUNT: usize = 4096;

    let mut seen_cpu = false;
    let mut seen_ram = false;

    while let Some(header) = read_section_header(r)? {
        if header.id == SectionId::DEVICES && header.len > MAX_DEVICES_SECTION_LEN {
            return Err(SnapshotError::Corrupt("devices section too large"));
        }

        let mut section_reader = r.take(header.len);
        match header.id {
            id if id == SectionId::META => {
                if header.version == 1 {
                    let meta = SnapshotMeta::decode(&mut section_reader)?;
                    target.restore_meta(meta);
                }
            }
            id if id == SectionId::CPU => {
                if header.version == 1 {
                    let cpu = CpuState::decode(&mut section_reader)?;
                    target.restore_cpu_state(cpu);
                    seen_cpu = true;
                }
            }
            id if id == SectionId::MMU => {
                if header.version == 1 {
                    let mmu = MmuState::decode(&mut section_reader)?;
                    target.restore_mmu_state(mmu);
                }
            }
            id if id == SectionId::DEVICES => {
                if header.version == 1 {
                    let count = section_reader.read_u32_le()? as usize;
                    if count > MAX_DEVICE_COUNT {
                        return Err(SnapshotError::Corrupt("too many devices"));
                    }
                    let mut devices = Vec::with_capacity(count.min(64));
                    for _ in 0..count {
                        devices.push(DeviceState::decode(&mut section_reader, 64 * 1024 * 1024)?);
                    }
                    target.restore_device_states(devices);
                }
            }
            id if id == SectionId::DISKS => {
                if header.version == 1 {
                    let disks = DiskOverlayRefs::decode(&mut section_reader)?;
                    target.restore_disk_overlays(disks);
                }
            }
            id if id == SectionId::RAM => {
                if header.version == 1 {
                    let expected_len = target.ram_len() as u64;
                    ram::decode_ram_section_into(
                        &mut section_reader,
                        expected_len,
                        |offset, data| target.write_ram(offset, data),
                    )?;
                    seen_ram = true;
                }
            }
            _ => {
                // Unknown section; skip.
            }
        }

        // Consume any trailing bytes (forward-compatible additions inside known sections).
        std::io::copy(&mut section_reader, &mut std::io::sink())?;
        if section_reader.limit() != 0 {
            return Err(SnapshotError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "truncated section payload",
            )));
        }
    }

    if !seen_cpu {
        return Err(SnapshotError::Corrupt("missing CPU section"));
    }
    if !seen_ram {
        return Err(SnapshotError::Corrupt("missing RAM section"));
    }
    target.post_restore()?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct SectionHeader {
    id: SectionId,
    version: u16,
    len: u64,
}

fn write_file_header<W: Write>(w: &mut W) -> Result<()> {
    w.write_bytes(SNAPSHOT_MAGIC)?;
    w.write_u16_le(SNAPSHOT_VERSION_V1)?;
    w.write_u8(SNAPSHOT_ENDIANNESS_LITTLE)?;
    w.write_u8(0)?; // reserved
    w.write_u32_le(0)?; // flags/reserved
    Ok(())
}

fn read_file_header<R: Read>(r: &mut R) -> Result<()> {
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic)?;
    if &magic != SNAPSHOT_MAGIC {
        return Err(SnapshotError::InvalidMagic);
    }
    let version = r.read_u16_le()?;
    if version != SNAPSHOT_VERSION_V1 {
        return Err(SnapshotError::UnsupportedVersion(version));
    }
    let endianness = r.read_u8()?;
    if endianness != SNAPSHOT_ENDIANNESS_LITTLE {
        return Err(SnapshotError::InvalidEndianness(endianness));
    }
    let _reserved = r.read_u8()?;
    let _flags = r.read_u32_le()?;
    Ok(())
}

fn write_section<W: Write + Seek>(
    w: &mut W,
    id: SectionId,
    version: u16,
    flags: u16,
    f: impl FnOnce(&mut W) -> Result<()>,
) -> Result<()> {
    let header_pos = w.stream_position()?;
    w.write_u32_le(id.0)?;
    w.write_u16_le(version)?;
    w.write_u16_le(flags)?;
    w.write_u64_le(0)?; // placeholder len

    let payload_start = w.stream_position()?;
    f(w)?;
    let payload_end = w.stream_position()?;

    let len = payload_end
        .checked_sub(payload_start)
        .ok_or(SnapshotError::Corrupt("stream position underflow"))?;

    w.seek(SeekFrom::Start(header_pos + 8))?;
    w.write_u64_le(len)?;
    w.seek(SeekFrom::Start(payload_end))?;
    Ok(())
}

fn read_section_header<R: Read>(r: &mut R) -> Result<Option<SectionHeader>> {
    let mut first = [0u8; 1];
    match r.read(&mut first)? {
        0 => return Ok(None),
        1 => {}
        _ => unreachable!("read() with 1-byte buffer"),
    }
    let mut tag_bytes = [0u8; 4];
    tag_bytes[0] = first[0];
    r.read_exact(&mut tag_bytes[1..])?;
    let id = SectionId(u32::from_le_bytes(tag_bytes));
    let version = r.read_u16_le()?;
    let _flags = r.read_u16_le()?;
    let len = r.read_u64_le()?;
    Ok(Some(SectionHeader { id, version, len }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use proptest::prelude::*;

    #[derive(Default)]
    struct DummyTarget {
        ram: Vec<u8>,
    }

    impl DummyTarget {
        fn new(ram_len: usize) -> Self {
            Self {
                ram: vec![0u8; ram_len],
            }
        }
    }

    impl SnapshotTarget for DummyTarget {
        fn restore_cpu_state(&mut self, _state: CpuState) {}
        fn restore_mmu_state(&mut self, _state: MmuState) {}
        fn restore_device_states(&mut self, _states: Vec<DeviceState>) {}
        fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

        fn ram_len(&self) -> usize {
            self.ram.len()
        }

        fn write_ram(&mut self, offset: u64, data: &[u8]) -> Result<()> {
            let offset: usize = offset
                .try_into()
                .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
            if offset + data.len() > self.ram.len() {
                return Err(SnapshotError::Corrupt("ram write out of bounds"));
            }
            self.ram[offset..offset + data.len()].copy_from_slice(data);
            Ok(())
        }
    }

    proptest! {
        // "Fuzz" the decoder. This is not a replacement for coverage-guided fuzzing, but it does
        // guard against panics on corrupted/truncated inputs.
        #[test]
        fn decoder_never_panics(data in proptest::collection::vec(any::<u8>(), 0..8192)) {
            let mut target = DummyTarget::new(1024);
            let _ = restore_snapshot(&mut std::io::Cursor::new(&data), &mut target);
        }
    }
}
