mod cpu_core;
mod error;
mod format;
mod inspect;
mod io;
mod ram;
mod types;

#[cfg(feature = "io-snapshot")]
pub mod io_snapshot_bridge;
#[cfg(feature = "io-snapshot")]
pub use crate::io_snapshot_bridge::{apply_io_snapshot_to_device, device_state_from_io_snapshot};

pub use crate::cpu_core::{
    apply_cpu_internal_state_to_cpu_core, apply_cpu_state_to_cpu_core, apply_mmu_state_to_cpu_core,
    cpu_core_from_snapshot, cpu_internal_state_from_cpu_core, cpu_state_from_cpu_core,
    mmu_state_from_cpu_core, snapshot_from_cpu_core,
};
pub use crate::error::{Result, SnapshotError};
pub use crate::format::{
    DeviceId, SectionId, SNAPSHOT_ENDIANNESS_LITTLE, SNAPSHOT_MAGIC, SNAPSHOT_VERSION_V1,
};
pub use crate::inspect::{
    inspect_snapshot, read_snapshot_meta, RamHeaderSummary, SnapshotIndex, SnapshotSectionInfo,
};
pub use crate::ram::{Compression, RamMode, RamWriteOptions};
pub use crate::types::{
    CpuInternalState, CpuMode, CpuState, DeviceState, DiskOverlayRef, DiskOverlayRefs, FpuState,
    MmuState, SegmentState, SnapshotMeta, VcpuSnapshot,
};

use std::io::{Read, Seek, SeekFrom, Write};

use crate::io::{ReadLeExt, WriteLeExt};

const DUPLICATE_DEVICE_ENTRY: &str = "duplicate device entry (id/version/flags must be unique)";
const DUPLICATE_DISK_ENTRY: &str = "duplicate disk entry (disk_id must be unique)";
const DUPLICATE_APIC_ID: &str = "duplicate APIC ID in CPU list (apic_id must be unique)";

#[derive(Debug, Clone, Copy, Default)]
pub struct SaveOptions {
    pub ram: RamWriteOptions,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RestoreOptions {
    /// For dirty-page snapshots, the expected parent snapshot id this diff should be applied on
    /// top of.
    ///
    /// Full snapshots are standalone and ignore this field.
    pub expected_parent_snapshot_id: Option<u64>,
}

pub trait SnapshotSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta;
    fn cpu_state(&self) -> CpuState;
    fn cpu_states(&self) -> Vec<VcpuSnapshot> {
        vec![VcpuSnapshot {
            apic_id: 0,
            cpu: self.cpu_state(),
            internal_state: Vec::new(),
        }]
    }
    fn mmu_state(&self) -> MmuState;
    fn device_states(&self) -> Vec<DeviceState>;
    fn disk_overlays(&self) -> DiskOverlayRefs;

    fn ram_len(&self) -> usize;
    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<()>;

    /// Dirty page size (in bytes) used by [`SnapshotSource::take_dirty_pages`].
    ///
    /// Defaults to 4096 bytes for backward compatibility.
    ///
    /// When saving dirty RAM snapshots (`RamMode::Dirty`), `SaveOptions.ram.page_size` **must**
    /// equal this value to ensure dirty page indices are interpreted correctly.
    fn dirty_page_size(&self) -> u32 {
        4096
    }

    /// Return and clear the set of dirty pages since the last snapshot.
    ///
    /// Each page index is `offset / dirty_page_size()`, i.e. indices are measured in units of
    /// [`SnapshotSource::dirty_page_size`].
    fn take_dirty_pages(&mut self) -> Option<Vec<u64>>;
}

pub trait SnapshotTarget {
    /// Called once at the start of a snapshot restore, after validating the snapshot file header.
    ///
    /// Snapshot restore is not transactional: callers may observe partial state if decode fails
    /// partway through (e.g. due to corrupt inputs). `pre_restore` exists so targets can clear
    /// transient "restore-only" state before any sections are applied, even when the caller uses
    /// `aero_snapshot::restore_snapshot` directly instead of a higher-level wrapper.
    fn pre_restore(&mut self) {}
    fn restore_meta(&mut self, _meta: SnapshotMeta) {}
    fn restore_cpu_state(&mut self, state: CpuState);
    fn restore_cpu_states(&mut self, states: Vec<VcpuSnapshot>) -> Result<()> {
        if states.len() != 1 {
            return Err(SnapshotError::Corrupt(
                "snapshot contains multiple CPUs but target only supports one",
            ));
        }
        let cpu = states
            .into_iter()
            .next()
            .ok_or(SnapshotError::Corrupt("missing CPU entry"))?;
        self.restore_cpu_state(cpu.cpu);
        Ok(())
    }
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
    if options.ram.mode == RamMode::Dirty {
        let source_page_size = source.dirty_page_size();
        if options.ram.page_size != source_page_size {
            return Err(SnapshotError::DirtyPageSizeMismatch {
                options: options.ram.page_size,
                dirty_page_size: source_page_size,
            });
        }
    }

    write_file_header(w)?;

    write_section(w, SectionId::META, 1, 0, |w| {
        let meta = source.snapshot_meta();
        meta.encode(w)
    })?;

    let mut cpus = source.cpu_states();
    cpus.sort_by_key(|cpu| cpu.apic_id);
    if cpus.windows(2).any(|w| w[0].apic_id == w[1].apic_id) {
        return Err(SnapshotError::Corrupt(DUPLICATE_APIC_ID));
    }
    if cpus.len() == 1 && cpus[0].apic_id == 0 && cpus[0].internal_state.is_empty() {
        write_section(w, SectionId::CPU, 2, 0, |w| cpus[0].cpu.encode_v2(w))?;
    } else {
        write_section(w, SectionId::CPUS, 2, 0, |w| {
            let count: u32 = cpus
                .len()
                .try_into()
                .map_err(|_| SnapshotError::Corrupt("too many CPUs"))?;
            w.write_u32_le(count)?;

            for cpu in &cpus {
                let mut entry = Vec::new();
                cpu.encode_v2(&mut entry)?;
                let entry_len: u64 = entry
                    .len()
                    .try_into()
                    .map_err(|_| SnapshotError::Corrupt("CPU entry too large"))?;
                w.write_u64_le(entry_len)?;
                w.write_bytes(&entry)?;
            }
            Ok(())
        })?;
    }

    write_section(w, SectionId::MMU, 2, 0, |w| source.mmu_state().encode_v2(w))?;

    write_section(w, SectionId::DEVICES, 1, 0, |w| {
        let mut devices = source.device_states();
        devices.sort_by_key(|device| (device.id.0, device.version, device.flags));
        if devices.windows(2).any(|w| {
            w[0].id.0 == w[1].id.0 && w[0].version == w[1].version && w[0].flags == w[1].flags
        }) {
            return Err(SnapshotError::Corrupt(DUPLICATE_DEVICE_ENTRY));
        }
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
        if disks.disks.windows(2).any(|w| w[0].disk_id == w[1].disk_id) {
            return Err(SnapshotError::Corrupt(DUPLICATE_DISK_ENTRY));
        }
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
                let dirty_pages = source
                    .take_dirty_pages()
                    .ok_or(SnapshotError::Corrupt("dirty-page tracking not available"))?;

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
    restore_snapshot_impl(r, target, None, None)
}

/// Restore a snapshot, validating the dirty-page parent contract without requiring `Seek`.
///
/// Full snapshots are standalone and ignore `opts.expected_parent_snapshot_id`.
///
/// Dirty-page snapshots are diffs and require an exact match between the snapshot's
/// `SnapshotMeta.parent_snapshot_id` and `opts.expected_parent_snapshot_id`.
///
/// For non-seekable readers, dirty snapshots must place the `META` section *before* the `RAM`
/// section so the parent can be validated before applying diffs.
pub fn restore_snapshot_checked<R: Read, T: SnapshotTarget>(
    r: &mut R,
    target: &mut T,
    opts: RestoreOptions,
) -> Result<()> {
    restore_snapshot_impl(r, target, Some(opts), None)
}

pub fn restore_snapshot_with_options<R: Read + Seek, T: SnapshotTarget>(
    r: &mut R,
    target: &mut T,
    opts: RestoreOptions,
) -> Result<()> {
    let start_pos = r.stream_position()?;

    let prescan = prescan_snapshot(r)?;
    if prescan.ram_mode == Some(RamMode::Dirty) {
        let meta = prescan
            .meta
            .as_ref()
            .ok_or(SnapshotError::Corrupt("missing META section"))?;
        if meta.parent_snapshot_id != opts.expected_parent_snapshot_id {
            return Err(SnapshotError::Corrupt("snapshot parent mismatch"));
        }
    }

    r.seek(SeekFrom::Start(start_pos))?;
    restore_snapshot_impl(r, target, Some(opts), prescan.meta)
}

#[derive(Debug, Clone)]
struct SnapshotPrescan {
    meta: Option<SnapshotMeta>,
    ram_mode: Option<RamMode>,
}

fn prescan_snapshot<R: Read + Seek>(r: &mut R) -> Result<SnapshotPrescan> {
    read_file_header(r)?;

    let mut meta = None;
    let mut ram_mode = None;

    while let Some(header) = read_section_header(r)? {
        let payload_start = r.stream_position()?;
        match header.id {
            id if id == SectionId::META => {
                if header.version == 1 {
                    let mut section_reader = r.take(header.len);
                    meta = Some(SnapshotMeta::decode(&mut section_reader)?);
                }
            }
            id if id == SectionId::RAM => {
                if header.version == 1 {
                    let mut section_reader = r.take(header.len);
                    let _total_len = section_reader.read_u64_le()?;
                    let _page_size = section_reader.read_u32_le()?;
                    ram_mode = Some(RamMode::from_u8(section_reader.read_u8()?)?);
                    let _compression = section_reader.read_u8()?;
                    let _reserved = section_reader.read_u16_le()?;
                }
            }
            _ => {}
        }

        let end = payload_start
            .checked_add(header.len)
            .ok_or(SnapshotError::Corrupt("section length overflow"))?;
        r.seek(SeekFrom::Start(end))?;

        if meta.is_some() && ram_mode.is_some() {
            break;
        }
    }

    Ok(SnapshotPrescan { meta, ram_mode })
}

fn restore_snapshot_impl<R: Read, T: SnapshotTarget>(
    r: &mut R,
    target: &mut T,
    opts: Option<RestoreOptions>,
    prescanned_meta: Option<SnapshotMeta>,
) -> Result<()> {
    read_file_header(r)?;
    target.pre_restore();

    const MAX_DEVICES_SECTION_LEN: u64 = 256 * 1024 * 1024;
    const MAX_DEVICE_COUNT: usize = 4096;
    const MAX_CPU_COUNT: usize = 256;

    // For dirty snapshots, we must validate parent snapshot id before applying RAM diffs.
    //
    // For `restore_snapshot` and `restore_snapshot_checked`, this means `META` must appear before
    // `RAM` (since we cannot seek back, and buffering full RAM diffs would be unsafe/expensive).
    //
    // For `restore_snapshot_with_options`, the prescan provides `prescanned_meta`, allowing
    // snapshots that place `META` after `RAM` to still be restored safely.
    const DIRTY_META_MUST_PRECEDE_RAM: &str = "dirty snapshot requires META section before RAM";

    let mut meta: Option<SnapshotMeta> = None;

    let mut seen_meta_section = false;
    let mut seen_mmu_section = false;
    let mut seen_devices_section = false;
    let mut seen_disks_section = false;

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
                    if seen_meta_section {
                        return Err(SnapshotError::Corrupt("duplicate META section"));
                    }
                    seen_meta_section = true;
                    let decoded = SnapshotMeta::decode(&mut section_reader)?;
                    meta = Some(decoded.clone());
                    target.restore_meta(decoded);
                }
            }
            id if id == SectionId::CPU => {
                if header.version == 1 {
                    if seen_cpu {
                        return Err(SnapshotError::Corrupt("duplicate CPU/CPUS section"));
                    }
                    let cpu = CpuState::decode_v1(&mut section_reader)?;
                    target.restore_cpu_states(vec![VcpuSnapshot {
                        apic_id: 0,
                        cpu,
                        internal_state: Vec::new(),
                    }])?;
                    seen_cpu = true;
                } else if header.version >= 2 {
                    if seen_cpu {
                        return Err(SnapshotError::Corrupt("duplicate CPU/CPUS section"));
                    }
                    let cpu = CpuState::decode_v2(&mut section_reader)?;
                    target.restore_cpu_states(vec![VcpuSnapshot {
                        apic_id: 0,
                        cpu,
                        internal_state: Vec::new(),
                    }])?;
                    seen_cpu = true;
                }
            }
            id if id == SectionId::CPUS => {
                if header.version == 1 {
                    if seen_cpu {
                        return Err(SnapshotError::Corrupt("duplicate CPU/CPUS section"));
                    }
                    let count = section_reader.read_u32_le()? as usize;
                    if count > MAX_CPU_COUNT {
                        return Err(SnapshotError::Corrupt("too many CPUs"));
                    }
                    let mut cpus = Vec::with_capacity(count.min(64));
                    for _ in 0..count {
                        let entry_len = section_reader.read_u64_le()?;
                        let mut entry_reader = (&mut section_reader).take(entry_len);
                        let cpu = VcpuSnapshot::decode_v1(&mut entry_reader, 64 * 1024 * 1024)?;
                        // Skip any forward-compatible additions to the vCPU entry.
                        std::io::copy(&mut entry_reader, &mut std::io::sink())?;
                        cpus.push(cpu);
                    }
                    // Provide deterministic ordering to snapshot targets regardless of snapshot file
                    // ordering. Snapshot encoding already canonicalizes by `apic_id`, but older or
                    // external snapshot producers might not.
                    cpus.sort_by_key(|cpu| cpu.apic_id);
                    if cpus.windows(2).any(|w| w[0].apic_id == w[1].apic_id) {
                        return Err(SnapshotError::Corrupt(DUPLICATE_APIC_ID));
                    }
                    target.restore_cpu_states(cpus)?;
                    seen_cpu = true;
                } else if header.version >= 2 {
                    if seen_cpu {
                        return Err(SnapshotError::Corrupt("duplicate CPU/CPUS section"));
                    }
                    let count = section_reader.read_u32_le()? as usize;
                    if count > MAX_CPU_COUNT {
                        return Err(SnapshotError::Corrupt("too many CPUs"));
                    }
                    let mut cpus = Vec::with_capacity(count.min(64));
                    for _ in 0..count {
                        let entry_len = section_reader.read_u64_le()?;
                        let mut entry_reader = (&mut section_reader).take(entry_len);
                        let cpu = VcpuSnapshot::decode_v2(&mut entry_reader, 64 * 1024 * 1024)?;
                        // Skip any forward-compatible additions to the vCPU entry.
                        std::io::copy(&mut entry_reader, &mut std::io::sink())?;
                        cpus.push(cpu);
                    }
                    // Provide deterministic ordering to snapshot targets regardless of snapshot file
                    // ordering. Snapshot encoding already canonicalizes by `apic_id`, but older or
                    // external snapshot producers might not.
                    cpus.sort_by_key(|cpu| cpu.apic_id);
                    if cpus.windows(2).any(|w| w[0].apic_id == w[1].apic_id) {
                        return Err(SnapshotError::Corrupt(DUPLICATE_APIC_ID));
                    }
                    target.restore_cpu_states(cpus)?;
                    seen_cpu = true;
                }
            }
            id if id == SectionId::MMU => {
                if header.version == 1 {
                    if seen_mmu_section {
                        return Err(SnapshotError::Corrupt("duplicate MMU section"));
                    }
                    seen_mmu_section = true;
                    let mmu = MmuState::decode_v1(&mut section_reader)?;
                    target.restore_mmu_state(mmu);
                } else if header.version >= 2 {
                    if seen_mmu_section {
                        return Err(SnapshotError::Corrupt("duplicate MMU section"));
                    }
                    seen_mmu_section = true;
                    let mmu = MmuState::decode_v2(&mut section_reader)?;
                    target.restore_mmu_state(mmu);
                }
            }
            id if id == SectionId::DEVICES => {
                if header.version == 1 {
                    if seen_devices_section {
                        return Err(SnapshotError::Corrupt("duplicate DEVICES section"));
                    }
                    seen_devices_section = true;
                    let count = section_reader.read_u32_le()? as usize;
                    if count > MAX_DEVICE_COUNT {
                        return Err(SnapshotError::Corrupt("too many devices"));
                    }
                    let mut devices = Vec::with_capacity(count.min(64));
                    for _ in 0..count {
                        let device = DeviceState::decode(&mut section_reader, 64 * 1024 * 1024)?;
                        devices.push(device);
                    }
                    // Provide deterministic ordering to snapshot targets regardless of snapshot file
                    // ordering. Snapshot encoding already canonicalizes by `(device_id, version, flags)`,
                    // but older or external snapshot producers might not.
                    devices.sort_by_key(|device| (device.id.0, device.version, device.flags));
                    if devices.windows(2).any(|w| {
                        w[0].id.0 == w[1].id.0
                            && w[0].version == w[1].version
                            && w[0].flags == w[1].flags
                    }) {
                        return Err(SnapshotError::Corrupt(DUPLICATE_DEVICE_ENTRY));
                    }
                    target.restore_device_states(devices);
                }
            }
            id if id == SectionId::DISKS => {
                if header.version == 1 {
                    if seen_disks_section {
                        return Err(SnapshotError::Corrupt("duplicate DISKS section"));
                    }
                    seen_disks_section = true;
                    let mut disks = DiskOverlayRefs::decode(&mut section_reader)?;
                    // Provide deterministic ordering to snapshot targets regardless of snapshot file
                    // ordering. Snapshot encoding already canonicalizes by `disk_id`, but older or
                    // external snapshot producers might not.
                    disks.disks.sort_by_key(|disk| disk.disk_id);
                    if disks.disks.windows(2).any(|w| w[0].disk_id == w[1].disk_id) {
                        return Err(SnapshotError::Corrupt(DUPLICATE_DISK_ENTRY));
                    }
                    target.restore_disk_overlays(disks);
                }
            }
            id if id == SectionId::RAM => {
                if header.version == 1 {
                    if seen_ram {
                        return Err(SnapshotError::Corrupt("duplicate RAM section"));
                    }
                    let expected_len = target.ram_len() as u64;

                    // Read the fixed-size RAM header so we can validate the dirty-parent contract
                    // before applying any RAM writes.
                    let mut ram_header = [0u8; 16];
                    section_reader.read_exact(&mut ram_header)?;
                    let ram_mode = RamMode::from_u8(ram_header[12])?;

                    if ram_mode == RamMode::Dirty {
                        let meta = meta
                            .as_ref()
                            .or(prescanned_meta.as_ref())
                            .ok_or(SnapshotError::Corrupt(DIRTY_META_MUST_PRECEDE_RAM))?;

                        if meta.parent_snapshot_id.is_none() {
                            return Err(SnapshotError::Corrupt(
                                "dirty snapshot missing parent_snapshot_id",
                            ));
                        }

                        if let Some(opts) = opts {
                            if meta.parent_snapshot_id != opts.expected_parent_snapshot_id {
                                return Err(SnapshotError::Corrupt("snapshot parent mismatch"));
                            }
                        }
                    }

                    {
                        let mut replay =
                            std::io::Cursor::new(ram_header).chain(&mut section_reader);
                        ram::decode_ram_section_into(&mut replay, expected_len, |offset, data| {
                            target.write_ram(offset, data)
                        })?;
                    }

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
        return Err(SnapshotError::Corrupt("missing CPU/CPUS section"));
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    use proptest::prelude::*;
    use std::io::Cursor;

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

    #[derive(Clone)]
    struct DirtyPageSizeSource {
        ram: Vec<u8>,
        dirty_pages: Vec<u64>,
        dirty_page_size: u32,
    }

    impl DirtyPageSizeSource {
        fn new(dirty_page_size: u32) -> Self {
            let ram_len = 16 * 1024;
            let mut ram = vec![0u8; ram_len];
            for (idx, byte) in ram.iter_mut().enumerate() {
                *byte = idx as u8;
            }
            Self {
                ram,
                dirty_pages: vec![0, 1],
                dirty_page_size,
            }
        }
    }

    impl SnapshotSource for DirtyPageSizeSource {
        fn snapshot_meta(&mut self) -> SnapshotMeta {
            SnapshotMeta::default()
        }

        fn cpu_state(&self) -> CpuState {
            CpuState::default()
        }

        fn mmu_state(&self) -> MmuState {
            MmuState::default()
        }

        fn device_states(&self) -> Vec<DeviceState> {
            Vec::new()
        }

        fn disk_overlays(&self) -> DiskOverlayRefs {
            DiskOverlayRefs::default()
        }

        fn ram_len(&self) -> usize {
            self.ram.len()
        }

        fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
            let offset: usize = offset
                .try_into()
                .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
            let end = offset
                .checked_add(buf.len())
                .ok_or(SnapshotError::Corrupt("ram read overflow"))?;
            if end > self.ram.len() {
                return Err(SnapshotError::Corrupt("ram read out of bounds"));
            }
            buf.copy_from_slice(&self.ram[offset..end]);
            Ok(())
        }

        fn dirty_page_size(&self) -> u32 {
            self.dirty_page_size
        }

        fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
            Some(std::mem::take(&mut self.dirty_pages))
        }
    }

    #[test]
    fn save_snapshot_rejects_dirty_page_size_mismatch() {
        let mut source = DirtyPageSizeSource::new(8192);
        let mut options = SaveOptions::default();
        options.ram.mode = RamMode::Dirty;
        options.ram.page_size = 4096;

        let mut out = Cursor::new(Vec::new());
        let err = save_snapshot(&mut out, &mut source, options).unwrap_err();
        assert!(
            matches!(
                err,
                SnapshotError::DirtyPageSizeMismatch {
                    options: 4096,
                    dirty_page_size: 8192
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn save_snapshot_allows_matching_dirty_page_size() {
        let mut source = DirtyPageSizeSource::new(8192);
        let mut options = SaveOptions::default();
        options.ram.mode = RamMode::Dirty;
        options.ram.page_size = 8192;

        let mut out = Cursor::new(Vec::new());
        save_snapshot(&mut out, &mut source, options).unwrap();
        assert!(!out.into_inner().is_empty());
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

    #[derive(Clone)]
    struct DirtyBitmap {
        bits: Vec<u64>,
        pages: usize,
        page_size: usize,
    }

    impl DirtyBitmap {
        fn new(mem_len: usize, page_size: usize) -> Self {
            let pages = mem_len.div_ceil(page_size);
            let words = pages.div_ceil(64);
            Self {
                bits: vec![0u64; words],
                pages,
                page_size,
            }
        }

        fn mark_addr(&mut self, addr: usize) {
            let page = addr / self.page_size;
            if page < self.pages {
                let word = page / 64;
                let bit = page % 64;
                self.bits[word] |= 1u64 << bit;
            }
        }

        fn take(&mut self) -> Vec<u64> {
            let mut pages = Vec::new();
            for (word_idx, word) in self.bits.iter_mut().enumerate() {
                let mut w = *word;
                if w == 0 {
                    continue;
                }
                *word = 0;
                while w != 0 {
                    let bit = w.trailing_zeros() as usize;
                    let page = word_idx * 64 + bit;
                    if page < self.pages {
                        pages.push(page as u64);
                    }
                    w &= !(1u64 << bit);
                }
            }
            pages
        }
    }

    #[derive(Clone)]
    struct DummySource {
        cpu: CpuState,
        mmu: MmuState,
        ram: Vec<u8>,
        dirty: DirtyBitmap,
        next_snapshot_id: u64,
        last_snapshot_id: Option<u64>,
    }

    impl DummySource {
        fn new(ram_len: usize, page_size: usize) -> Self {
            Self {
                cpu: CpuState::default(),
                mmu: MmuState::default(),
                ram: vec![0u8; ram_len],
                dirty: DirtyBitmap::new(ram_len, page_size),
                next_snapshot_id: 1,
                last_snapshot_id: None,
            }
        }

        fn write_u8(&mut self, addr: usize, val: u8) {
            self.ram[addr] = val;
            self.dirty.mark_addr(addr);
        }
    }

    impl SnapshotSource for DummySource {
        fn snapshot_meta(&mut self) -> SnapshotMeta {
            let snapshot_id = self.next_snapshot_id;
            self.next_snapshot_id += 1;
            let meta = SnapshotMeta {
                snapshot_id,
                parent_snapshot_id: self.last_snapshot_id,
                created_unix_ms: 0,
                label: None,
            };
            self.last_snapshot_id = Some(snapshot_id);
            meta
        }

        fn cpu_state(&self) -> CpuState {
            self.cpu.clone()
        }

        fn mmu_state(&self) -> MmuState {
            self.mmu.clone()
        }

        fn device_states(&self) -> Vec<DeviceState> {
            Vec::new()
        }

        fn disk_overlays(&self) -> DiskOverlayRefs {
            DiskOverlayRefs::default()
        }

        fn ram_len(&self) -> usize {
            self.ram.len()
        }

        fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
            let offset: usize = offset
                .try_into()
                .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
            buf.copy_from_slice(&self.ram[offset..offset + buf.len()]);
            Ok(())
        }

        fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
            Some(self.dirty.take())
        }
    }

    fn snapshot_bytes<S: SnapshotSource>(source: &mut S, options: SaveOptions) -> Result<Vec<u8>> {
        let mut cursor = Cursor::new(Vec::new());
        save_snapshot(&mut cursor, source, options)?;
        Ok(cursor.into_inner())
    }

    #[test]
    fn restore_snapshot_with_options_validates_dirty_parent_chain() {
        let page_size = 4096usize;
        let ram_len = page_size * 2;
        let mut source = DummySource::new(ram_len, page_size);

        source.write_u8(0, 0xAA);
        source.write_u8(page_size, 0xBB);
        let base_mem = source.ram.clone();

        let base_bytes = snapshot_bytes(&mut source, SaveOptions::default()).unwrap();
        let base_snapshot_id = source.last_snapshot_id.unwrap();

        source.write_u8(1, 0xCC);
        let expected_final_mem = source.ram.clone();

        let mut dirty_opts = SaveOptions::default();
        dirty_opts.ram.mode = RamMode::Dirty;
        let diff_bytes = snapshot_bytes(&mut source, dirty_opts).unwrap();

        // Full snapshots are standalone and ignore the expected parent id option.
        let mut full_target = DummyTarget::new(ram_len);
        restore_snapshot_with_options(
            &mut Cursor::new(base_bytes.as_slice()),
            &mut full_target,
            RestoreOptions {
                expected_parent_snapshot_id: Some(12345),
            },
        )
        .unwrap();
        assert_eq!(full_target.ram, base_mem);

        // Dirty snapshots require an exact match on the expected parent id.
        let mut target = DummyTarget::new(ram_len);
        restore_snapshot(&mut Cursor::new(base_bytes.as_slice()), &mut target).unwrap();

        let err = restore_snapshot_with_options(
            &mut Cursor::new(diff_bytes.as_slice()),
            &mut target,
            RestoreOptions {
                expected_parent_snapshot_id: None,
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SnapshotError::Corrupt("snapshot parent mismatch")
        ));

        restore_snapshot_with_options(
            &mut Cursor::new(diff_bytes.as_slice()),
            &mut target,
            RestoreOptions {
                expected_parent_snapshot_id: Some(base_snapshot_id),
            },
        )
        .unwrap();
        assert_eq!(target.ram, expected_final_mem);
    }
}
