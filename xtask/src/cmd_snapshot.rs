use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};

use aero_snapshot::{
    Compression, CpuState, DeviceId, DiskOverlayRefs, RamMode, SectionId, SnapshotError,
    SnapshotIndex, SnapshotSectionInfo, SnapshotTarget,
};

use crate::error::{Result, XtaskError};

const MAX_DEVICE_COUNT: u32 = 4096;
const MAX_DEVICE_ENTRY_LEN: u64 = 64 * 1024 * 1024;
const MAX_DEVICES_SECTION_LEN: u64 = 256 * 1024 * 1024;
const MAX_CPU_COUNT: u32 = 4096;
const MAX_VCPU_INTERNAL_LEN: u64 = 64 * 1024 * 1024;
const MAX_DISK_REFS: u32 = 256;
const MAX_DISK_PATH_LEN: u32 = 64 * 1024;

pub fn print_help() {
    println!(
        "\
Inspect and validate Aero snapshots (`aero-snapshot`).

Usage:
  cargo xtask snapshot inspect <path>
  cargo xtask snapshot validate [--deep] <path>

Subcommands:
  inspect    Print header, META fields, section table, and RAM encoding summary.
  validate   Structural validation without decompressing RAM.
            Use --deep to fully restore/decompress into a dummy target (small files only).
"
    );
}

pub fn cmd(args: Vec<String>) -> Result<()> {
    if args.is_empty() || args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }

    let mut it = args.into_iter();
    let Some(sub) = it.next() else {
        print_help();
        return Ok(());
    };

    match sub.as_str() {
        "inspect" => cmd_inspect(it.collect()),
        "validate" => cmd_validate(it.collect()),
        other => Err(XtaskError::Message(format!(
            "unknown `snapshot` subcommand `{other}` (run `cargo xtask snapshot --help`)"
        ))),
    }
}

fn cmd_inspect(args: Vec<String>) -> Result<()> {
    let [path] = args.as_slice() else {
        return Err(XtaskError::Message(
            "usage: cargo xtask snapshot inspect <path>".to_string(),
        ));
    };

    let file_len = fs::metadata(path)
        .map_err(|e| XtaskError::Message(format!("stat {path:?}: {e}")))?
        .len();

    let mut file =
        fs::File::open(path).map_err(|e| XtaskError::Message(format!("open {path:?}: {e}")))?;
    let index = aero_snapshot::inspect_snapshot(&mut file)
        .map_err(|e| XtaskError::Message(format!("inspect snapshot: {e}")))?;

    println!("Snapshot: {path}");
    println!("File size: {file_len} bytes");

    println!("Header:");
    println!(
        "  magic: {}",
        std::str::from_utf8(aero_snapshot::SNAPSHOT_MAGIC).unwrap_or("<bin>")
    );
    println!("  version: {}", index.version);
    println!(
        "  endianness: {}",
        match index.endianness {
            aero_snapshot::SNAPSHOT_ENDIANNESS_LITTLE => "little",
            other => {
                // `inspect_snapshot` validates endianness, but keep this resilient.
                println!("  endianness_tag: {other}");
                "unknown"
            }
        }
    );

    println!("META:");
    match &index.meta {
        Some(meta) => {
            println!("  snapshot_id: {}", meta.snapshot_id);
            println!(
                "  parent_snapshot_id: {}",
                meta.parent_snapshot_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "none".to_string())
            );
            println!("  created_unix_ms: {}", meta.created_unix_ms);
            println!("  label: {}", meta.label.as_deref().unwrap_or("none"));
        }
        None => println!("  <missing>"),
    }

    println!("Sections:");
    for section in &index.sections {
        println!(
            "  - {} v{} flags={} offset=0x{:x} len={}",
            section.id, section.version, section.flags, section.offset, section.len
        );
    }

    if let Some(devices) = index.sections.iter().find(|s| s.id == SectionId::DEVICES) {
        println!("DEVICES:");
        print_devices_section_summary(&mut file, devices);
    }
    if let Some(disks) = index.sections.iter().find(|s| s.id == SectionId::DISKS) {
        println!("DISKS:");
        print_disks_section_summary(&mut file, disks);
    }

    println!("RAM:");
    match &index.ram {
        Some(ram) => {
            println!("  total_len: {} bytes", ram.total_len);
            println!("  page_size: {} bytes", ram.page_size);
            println!(
                "  mode: {}",
                match ram.mode {
                    RamMode::Full => "full",
                    RamMode::Dirty => "dirty",
                }
            );
            println!(
                "  compression: {}",
                match ram.compression {
                    Compression::None => "none",
                    Compression::Lz4 => "lz4",
                }
            );
            if let Some(chunk_size) = ram.chunk_size {
                println!("  chunk_size: {} bytes", chunk_size);
                let chunk_count = ram.total_len.div_ceil(chunk_size as u64);
                println!("  chunk_count: {chunk_count}");
            }
            if let Some(dirty_count) = ram.dirty_count {
                println!("  dirty_count: {dirty_count}");
            }
        }
        None => println!("  <missing>"),
    }

    Ok(())
}

fn print_devices_section_summary(file: &mut fs::File, section: &SnapshotSectionInfo) {
    if section.version != 1 {
        println!("  <unsupported DEVICES section version {}>", section.version);
        return;
    }

    let section_end = match section.offset.checked_add(section.len) {
        Some(v) => v,
        None => {
            println!("  <invalid section length>");
            return;
        }
    };

    if section.len < 4 {
        println!("  <truncated section>");
        return;
    }

    if let Err(e) = file.seek(SeekFrom::Start(section.offset)) {
        println!("  <failed to seek: {e}>");
        return;
    }

    let count = match read_u32_le_lossy(file) {
        Ok(v) => v,
        Err(e) => {
            println!("  <failed to read device count: {e}>");
            return;
        }
    };
    println!("  count: {count}");

    const MAX_LISTED: u32 = 64;
    for idx in 0..count {
        if idx >= MAX_LISTED {
            println!("  ... {} more device entries omitted", count - MAX_LISTED);
            break;
        }

        let pos = match file.stream_position() {
            Ok(v) => v,
            Err(e) => {
                println!("  <failed to read device entry: {e}>");
                return;
            }
        };
        if pos >= section_end {
            println!("  <truncated section>");
            return;
        }

        // Device entry header: id(u32), version(u16), flags(u16), len(u64).
        if section_end - pos < 16 {
            println!("  <truncated section>");
            return;
        }
        let id = match read_u32_le_lossy(file) {
            Ok(v) => v,
            Err(e) => {
                println!("  <failed to read device id: {e}>");
                return;
            }
        };
        let version = match read_u16_le_lossy(file) {
            Ok(v) => v,
            Err(e) => {
                println!("  <failed to read device version: {e}>");
                return;
            }
        };
        let flags = match read_u16_le_lossy(file) {
            Ok(v) => v,
            Err(e) => {
                println!("  <failed to read device flags: {e}>");
                return;
            }
        };
        let len = match read_u64_le_lossy(file) {
            Ok(v) => v,
            Err(e) => {
                println!("  <failed to read device length: {e}>");
                return;
            }
        };

        let data_start = match file.stream_position() {
            Ok(v) => v,
            Err(e) => {
                println!("  <failed to read device entry: {e}>");
                return;
            }
        };
        let data_end = match data_start.checked_add(len) {
            Some(v) => v,
            None => {
                println!("  <device length overflow>");
                return;
            }
        };
        if data_end > section_end {
            println!(
                "  - {}: {} version={} flags={} len={} <truncated>",
                idx,
                DeviceId(id),
                version,
                flags,
                len
            );
            return;
        }

        println!(
            "  - {}: {} version={} flags={} len={}",
            idx,
            DeviceId(id),
            version,
            flags,
            len
        );

        if let Err(e) = file.seek(SeekFrom::Start(data_end)) {
            println!("  <failed to skip device payload: {e}>");
            return;
        }
    }
}

fn print_disks_section_summary(file: &mut fs::File, section: &SnapshotSectionInfo) {
    const MAX_PRINT_CHARS: usize = 200;
    const MAX_PRINT_DISKS: usize = 64;

    if section.version != 1 {
        println!("  <unsupported DISKS section version {}>", section.version);
        return;
    }
    if let Err(e) = file.seek(SeekFrom::Start(section.offset)) {
        println!("  <failed to seek: {e}>");
        return;
    }
    let mut limited = file.take(section.len);
    let Ok(disks) = DiskOverlayRefs::decode(&mut limited) else {
        println!("  <failed to decode DISKS payload>");
        return;
    };

    println!("  count: {}", disks.disks.len());
    for (idx, disk) in disks.disks.iter().take(MAX_PRINT_DISKS).enumerate() {
        fn truncate(s: &str, max_chars: usize) -> String {
            if s.chars().count() <= max_chars {
                return s.to_string();
            }
            let mut out: String = s.chars().take(max_chars).collect();
            out.push_str("…");
            out
        }

        let base = truncate(&disk.base_image, MAX_PRINT_CHARS);
        let overlay = truncate(&disk.overlay_image, MAX_PRINT_CHARS);
        println!("  - [{idx}] disk_id={} base_image={base:?} overlay_image={overlay:?}", disk.disk_id);
    }
    if disks.disks.len() > MAX_PRINT_DISKS {
        println!("  ... ({} more)", disks.disks.len() - MAX_PRINT_DISKS);
    }
}

fn read_u16_le_lossy(r: &mut impl Read) -> std::io::Result<u16> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32_le_lossy(r: &mut impl Read) -> std::io::Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64_le_lossy(r: &mut impl Read) -> std::io::Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn cmd_validate(args: Vec<String>) -> Result<()> {
    let mut deep = false;
    let mut path: Option<String> = None;

    for arg in args {
        match arg.as_str() {
            "--deep" => deep = true,
            other if other.starts_with('-') => {
                return Err(XtaskError::Message(format!(
                    "unknown flag for `snapshot validate`: `{other}`"
                )));
            }
            other => {
                if path.is_some() {
                    return Err(XtaskError::Message(
                        "usage: cargo xtask snapshot validate [--deep] <path>".to_string(),
                    ));
                }
                path = Some(other.to_string());
            }
        }
    }

    let path = path.ok_or_else(|| {
        XtaskError::Message("usage: cargo xtask snapshot validate [--deep] <path>".to_string())
    })?;

    let mut file =
        fs::File::open(&path).map_err(|e| XtaskError::Message(format!("open {path:?}: {e}")))?;
    let index = aero_snapshot::inspect_snapshot(&mut file)
        .map_err(|e| XtaskError::Message(format!("inspect snapshot: {e}")))?;

    validate_index(&path, &index)?;

    if deep {
        deep_validate(&path, &index)?;
    }

    println!("valid snapshot");
    Ok(())
}

fn validate_index(path: &str, index: &SnapshotIndex) -> Result<()> {
    // Validate section-level invariants that `aero-snapshot`'s restore path enforces, so
    // `cargo xtask snapshot validate` agrees with what a real restore would accept.
    //
    // (Inspection is allowed to show duplicate/ambiguous structures; validation should not.)
    let cpu_section_count = index
        .sections
        .iter()
        .filter(|s| s.id == SectionId::CPU || s.id == SectionId::CPUS)
        .count();
    if cpu_section_count == 0 {
        return Err(XtaskError::Message("missing CPU/CPUS section".to_string()));
    }
    if cpu_section_count > 1 {
        return Err(XtaskError::Message(
            "duplicate CPU/CPUS section".to_string(),
        ));
    }

    let ram_section_count = index
        .sections
        .iter()
        .filter(|s| s.id == SectionId::RAM)
        .count();
    if ram_section_count == 0 {
        return Err(XtaskError::Message("missing RAM section".to_string()));
    }
    if ram_section_count > 1 {
        return Err(XtaskError::Message("duplicate RAM section".to_string()));
    }

    if index
        .sections
        .iter()
        .filter(|s| s.id == SectionId::META)
        .count()
        > 1
    {
        return Err(XtaskError::Message("duplicate META section".to_string()));
    }
    if index
        .sections
        .iter()
        .filter(|s| s.id == SectionId::MMU)
        .count()
        > 1
    {
        return Err(XtaskError::Message("duplicate MMU section".to_string()));
    }
    if index
        .sections
        .iter()
        .filter(|s| s.id == SectionId::DEVICES)
        .count()
        > 1
    {
        return Err(XtaskError::Message("duplicate DEVICES section".to_string()));
    }
    if index
        .sections
        .iter()
        .filter(|s| s.id == SectionId::DISKS)
        .count()
        > 1
    {
        return Err(XtaskError::Message("duplicate DISKS section".to_string()));
    }
    if index.ram.is_none() {
        return Err(XtaskError::Message(
            "missing or unsupported RAM section".to_string(),
        ));
    }

    if let Some(ram) = &index.ram {
        if ram.mode == RamMode::Dirty {
            let meta_offset = index
                .sections
                .iter()
                .find(|s| s.id == SectionId::META)
                .map(|s| s.offset);
            let ram_offset = index
                .sections
                .iter()
                .find(|s| s.id == SectionId::RAM)
                .map(|s| s.offset);

            // For non-seekable restore paths, dirty snapshots must provide META before RAM so the
            // parent snapshot id can be validated before applying diffs.
            if meta_offset.is_none()
                || ram_offset.is_none()
                || meta_offset.unwrap() > ram_offset.unwrap()
            {
                return Err(XtaskError::Message(
                    "dirty snapshot requires META section before RAM".to_string(),
                ));
            }

            let meta = index.meta.as_ref().ok_or_else(|| {
                XtaskError::Message("dirty snapshot requires META section before RAM".to_string())
            })?;
            if meta.parent_snapshot_id.is_none() {
                return Err(XtaskError::Message(
                    "dirty snapshot missing parent_snapshot_id".to_string(),
                ));
            }
        }
    }

    let mut file =
        fs::File::open(path).map_err(|e| XtaskError::Message(format!("open {path:?}: {e}")))?;

    for section in &index.sections {
        match section.id {
            id if id == SectionId::META => validate_meta_section(&mut file, section)?,
            id if id == SectionId::CPU => validate_cpu_section(&mut file, section)?,
            id if id == SectionId::CPUS => validate_cpus_section(&mut file, section)?,
            id if id == SectionId::MMU => validate_mmu_section(&mut file, section)?,
            id if id == SectionId::DEVICES => validate_devices_section(&mut file, section)?,
            id if id == SectionId::DISKS => validate_disks_section(&mut file, section)?,
            id if id == SectionId::RAM => validate_ram_section(&mut file, section)?,
            _ => {}
        }
    }

    Ok(())
}

fn deep_validate(path: &str, index: &SnapshotIndex) -> Result<()> {
    const MAX_DEEP_RAM_BYTES: u64 = 512 * 1024 * 1024;

    let ram = index
        .ram
        .ok_or_else(|| XtaskError::Message("missing RAM section".to_string()))?;
    if ram.total_len > MAX_DEEP_RAM_BYTES {
        return Err(XtaskError::Message(format!(
            "--deep refuses to restore snapshots with RAM > {MAX_DEEP_RAM_BYTES} bytes (found {})",
            ram.total_len
        )));
    }

    let ram_len: usize = ram
        .total_len
        .try_into()
        .map_err(|_| XtaskError::Message("snapshot RAM size does not fit in usize".to_string()))?;

    let mut file =
        fs::File::open(path).map_err(|e| XtaskError::Message(format!("open {path:?}: {e}")))?;
    let mut target = DeepValidateTarget { ram_len };
    aero_snapshot::restore_snapshot(&mut file, &mut target)
        .map_err(|e| XtaskError::Message(format!("restore snapshot: {e}")))?;
    Ok(())
}

struct DeepValidateTarget {
    ram_len: usize,
}

impl SnapshotTarget for DeepValidateTarget {
    fn restore_cpu_state(&mut self, _state: aero_snapshot::CpuState) {}

    fn restore_cpu_states(
        &mut self,
        states: Vec<aero_snapshot::VcpuSnapshot>,
    ) -> aero_snapshot::Result<()> {
        if states.is_empty() {
            return Err(SnapshotError::Corrupt("missing CPU entry"));
        }
        Ok(())
    }

    fn restore_mmu_state(&mut self, _state: aero_snapshot::MmuState) {}

    fn restore_device_states(&mut self, _states: Vec<aero_snapshot::DeviceState>) {}

    fn restore_disk_overlays(&mut self, _overlays: aero_snapshot::DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.ram_len
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> aero_snapshot::Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        if offset + data.len() > self.ram_len {
            return Err(SnapshotError::Corrupt("ram write out of bounds"));
        }
        Ok(())
    }
}

fn validate_meta_section(file: &mut fs::File, section: &SnapshotSectionInfo) -> Result<()> {
    if section.version != 1 {
        return Err(XtaskError::Message(
            "unsupported META section version".to_string(),
        ));
    }
    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek META: {e}")))?;
    let mut limited = file.take(section.len);
    aero_snapshot::SnapshotMeta::decode(&mut limited)
        .map_err(|e| XtaskError::Message(format!("decode META: {e}")))?;
    Ok(())
}

fn validate_cpu_section(file: &mut fs::File, section: &SnapshotSectionInfo) -> Result<()> {
    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek CPU: {e}")))?;
    let mut limited = file.take(section.len);
    if section.version == 1 {
        let _ = CpuState::decode_v1(&mut limited)
            .map_err(|e| XtaskError::Message(format!("decode CPU v1: {e}")))?;
        return Ok(());
    }
    if section.version >= 2 {
        let _ = CpuState::decode_v2(&mut limited)
            .map_err(|e| XtaskError::Message(format!("decode CPU v2: {e}")))?;
        return Ok(());
    }
    Err(XtaskError::Message(
        "unsupported CPU section version".to_string(),
    ))
}

fn validate_cpus_section(file: &mut fs::File, section: &SnapshotSectionInfo) -> Result<()> {
    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek CPUS: {e}")))?;

    let mut section_reader = file.take(section.len);
    let count = read_u32_le(&mut section_reader)?;
    if count == 0 {
        return Err(XtaskError::Message("missing CPU entry".to_string()));
    }
    if count > MAX_CPU_COUNT {
        return Err(XtaskError::Message("too many CPUs".to_string()));
    }

    let mut seen_apic_ids = HashSet::new();
    for _ in 0..count {
        let entry_len = read_u64_le(&mut section_reader)?;
        if entry_len > section_reader.limit() {
            return Err(XtaskError::Message("truncated CPU entry".to_string()));
        }

        let mut entry_reader = (&mut section_reader).take(entry_len);
        let apic_id = validate_vcpu_entry(&mut entry_reader, section.version)?;
        if !seen_apic_ids.insert(apic_id) {
            return Err(XtaskError::Message(
                "duplicate APIC ID in CPU list (apic_id must be unique)".to_string(),
            ));
        }
        // Skip any forward-compatible additions.
        std::io::copy(&mut entry_reader, &mut std::io::sink())
            .map_err(|e| XtaskError::Message(format!("read CPU entry: {e}")))?;
    }

    Ok(())
}

fn validate_vcpu_entry(entry_reader: &mut impl Read, version: u16) -> Result<u32> {
    let apic_id = read_u32_le(entry_reader)?;

    if version == 1 {
        let _ = CpuState::decode_v1(entry_reader)
            .map_err(|e| XtaskError::Message(format!("decode vCPU CPU v1: {e}")))?;
    } else if version >= 2 {
        let _ = CpuState::decode_v2(entry_reader)
            .map_err(|e| XtaskError::Message(format!("decode vCPU CPU v2: {e}")))?;
    } else {
        return Err(XtaskError::Message(
            "unsupported CPUS section version".to_string(),
        ));
    }

    let internal_len = read_u64_le(entry_reader)?;
    if internal_len > MAX_VCPU_INTERNAL_LEN {
        return Err(XtaskError::Message(
            "vCPU internal state too large".to_string(),
        ));
    }

    let mut internal_reader = entry_reader.take(internal_len);
    std::io::copy(&mut internal_reader, &mut std::io::sink())
        .map_err(|e| XtaskError::Message(format!("read vCPU internal state: {e}")))?;
    if internal_reader.limit() != 0 {
        return Err(XtaskError::Message(
            "truncated vCPU internal state".to_string(),
        ));
    }

    Ok(apic_id)
}

fn validate_mmu_section(file: &mut fs::File, section: &SnapshotSectionInfo) -> Result<()> {
    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek MMU: {e}")))?;
    let mut limited = file.take(section.len);
    if section.version == 1 {
        let _ = aero_snapshot::MmuState::decode_v1(&mut limited)
            .map_err(|e| XtaskError::Message(format!("decode MMU v1: {e}")))?;
        return Ok(());
    }
    if section.version >= 2 {
        let _ = aero_snapshot::MmuState::decode_v2(&mut limited)
            .map_err(|e| XtaskError::Message(format!("decode MMU v2: {e}")))?;
        return Ok(());
    }
    Err(XtaskError::Message(
        "unsupported MMU section version".to_string(),
    ))
}

fn validate_devices_section(file: &mut fs::File, section: &SnapshotSectionInfo) -> Result<()> {
    if section.version != 1 {
        return Err(XtaskError::Message(
            "unsupported DEVICES section version".to_string(),
        ));
    }
    if section.len > MAX_DEVICES_SECTION_LEN {
        return Err(XtaskError::Message("devices section too large".to_string()));
    }

    let section_end = section
        .offset
        .checked_add(section.len)
        .ok_or_else(|| XtaskError::Message("devices section overflow".to_string()))?;

    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek DEVICES: {e}")))?;

    ensure_section_remaining(file, section_end, 4, "device count")?;
    let count = read_u32_le(file)?;
    if count > MAX_DEVICE_COUNT {
        return Err(XtaskError::Message("too many devices".to_string()));
    }

    let mut seen = HashSet::new();
    for _ in 0..count {
        ensure_section_remaining(file, section_end, 4 + 2 + 2 + 8, "device entry header")?;
        let id = read_u32_le(file)?;
        let version = read_u16_le(file)?;
        let flags = read_u16_le(file)?;
        if !seen.insert((id, version, flags)) {
            return Err(XtaskError::Message(
                "duplicate device entry (id/version/flags must be unique)".to_string(),
            ));
        }
        let len = read_u64_le(file)?;
        if len > MAX_DEVICE_ENTRY_LEN {
            return Err(XtaskError::Message("device entry too large".to_string()));
        }

        let pos = file
            .stream_position()
            .map_err(|e| XtaskError::Message(format!("tell DEVICES: {e}")))?;
        let data_end = pos
            .checked_add(len)
            .ok_or_else(|| XtaskError::Message("device length overflow".to_string()))?;
        if data_end > section_end {
            return Err(XtaskError::Message("device entry truncated".to_string()));
        }
        file.seek(SeekFrom::Start(data_end))
            .map_err(|e| XtaskError::Message(format!("seek DEVICES entry: {e}")))?;
    }

    Ok(())
}

fn validate_disks_section(file: &mut fs::File, section: &SnapshotSectionInfo) -> Result<()> {
    if section.version != 1 {
        return Err(XtaskError::Message(
            "unsupported DISKS section version".to_string(),
        ));
    }

    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek DISKS: {e}")))?;

    let mut limited = file.take(section.len);
    let count = read_u32_le(&mut limited)?;
    if count > MAX_DISK_REFS {
        return Err(XtaskError::Message("too many disks".to_string()));
    }

    let mut seen = HashSet::new();
    for _ in 0..count {
        let disk_id = read_u32_le(&mut limited)?;
        if !seen.insert(disk_id) {
            return Err(XtaskError::Message(
                "duplicate disk entry (disk_id must be unique)".to_string(),
            ));
        }
        validate_string_u32_utf8(&mut limited, MAX_DISK_PATH_LEN, "disk base_image")?;
        validate_string_u32_utf8(&mut limited, MAX_DISK_PATH_LEN, "disk overlay_image")?;
    }

    Ok(())
}

fn validate_ram_section(file: &mut fs::File, section: &SnapshotSectionInfo) -> Result<()> {
    const MAX_PAGE_SIZE: u32 = 2 * 1024 * 1024;
    const MAX_CHUNK_SIZE: u32 = 64 * 1024 * 1024;

    if section.version != 1 {
        return Err(XtaskError::Message(
            "unsupported RAM section version".to_string(),
        ));
    }

    let section_end = section
        .offset
        .checked_add(section.len)
        .ok_or_else(|| XtaskError::Message("ram section overflow".to_string()))?;

    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek RAM: {e}")))?;

    ensure_section_remaining(file, section_end, 16, "ram header")?;
    let total_len = read_u64_le(file)?;
    let page_size = read_u32_le(file)?;
    if page_size == 0 || page_size > MAX_PAGE_SIZE {
        return Err(XtaskError::Message("invalid page size".to_string()));
    }

    let mode = {
        let mut b = [0u8; 1];
        file.read_exact(&mut b)
            .map_err(|e| XtaskError::Message(format!("read ram mode: {e}")))?;
        match b[0] {
            0 => RamMode::Full,
            1 => RamMode::Dirty,
            _ => return Err(XtaskError::Message("invalid ram mode".to_string())),
        }
    };

    let compression = {
        let mut b = [0u8; 1];
        file.read_exact(&mut b)
            .map_err(|e| XtaskError::Message(format!("read ram compression: {e}")))?;
        match b[0] {
            0 => Compression::None,
            1 => Compression::Lz4,
            _ => return Err(XtaskError::Message("invalid compression kind".to_string())),
        }
    };

    let _reserved = read_u16_le(file)?;

    match mode {
        RamMode::Full => {
            ensure_section_remaining(file, section_end, 4, "chunk_size")?;
            let chunk_size = read_u32_le(file)?;
            if chunk_size == 0 || chunk_size > MAX_CHUNK_SIZE {
                return Err(XtaskError::Message("invalid chunk size".to_string()));
            }

            let mut offset = 0u64;
            while offset < total_len {
                ensure_section_remaining(file, section_end, 8, "chunk header")?;
                let expected_uncompressed = (total_len - offset).min(u64::from(chunk_size)) as u32;
                let uncompressed_len = read_u32_le(file)?;
                if uncompressed_len != expected_uncompressed {
                    return Err(XtaskError::Message(
                        "chunk uncompressed length mismatch".to_string(),
                    ));
                }
                let compressed_len = read_u32_le(file)?;
                validate_compressed_len(compression, uncompressed_len, compressed_len)?;

                let payload_len: u64 = compressed_len.into();
                ensure_section_remaining(file, section_end, payload_len, "chunk payload")?;
                file.seek(SeekFrom::Current(i64::try_from(payload_len).map_err(
                    |_| XtaskError::Message("chunk payload too large".to_string()),
                )?))
                .map_err(|e| XtaskError::Message(format!("seek chunk payload: {e}")))?;

                offset = offset
                    .checked_add(u64::from(uncompressed_len))
                    .ok_or_else(|| XtaskError::Message("ram length overflow".to_string()))?;
            }
        }
        RamMode::Dirty => {
            ensure_section_remaining(file, section_end, 8, "dirty_count")?;
            let count = read_u64_le(file)?;

            let page_size_u64 = u64::from(page_size);
            let max_pages = total_len
                .checked_add(page_size_u64 - 1)
                .ok_or_else(|| XtaskError::Message("ram length overflow".to_string()))?
                / page_size_u64;
            if count > max_pages {
                return Err(XtaskError::Message("too many dirty pages".to_string()));
            }

            let mut prev_page_idx: Option<u64> = None;
            for _ in 0..count {
                ensure_section_remaining(file, section_end, 16, "dirty page header")?;
                let page_idx = read_u64_le(file)?;
                if let Some(prev) = prev_page_idx {
                    if page_idx <= prev {
                        return Err(XtaskError::Message(
                            "dirty page list not strictly increasing".to_string(),
                        ));
                    }
                }
                prev_page_idx = Some(page_idx);

                let offset = page_idx
                    .checked_mul(page_size_u64)
                    .ok_or_else(|| XtaskError::Message("dirty page offset overflow".to_string()))?;
                if offset >= total_len {
                    return Err(XtaskError::Message("dirty page out of range".to_string()));
                }

                let expected_uncompressed = (total_len - offset).min(page_size_u64) as u32;
                let uncompressed_len = read_u32_le(file)?;
                if uncompressed_len != expected_uncompressed {
                    return Err(XtaskError::Message(
                        "dirty page uncompressed length mismatch".to_string(),
                    ));
                }
                let compressed_len = read_u32_le(file)?;
                validate_compressed_len(compression, uncompressed_len, compressed_len)?;

                let payload_len: u64 = compressed_len.into();
                ensure_section_remaining(file, section_end, payload_len, "dirty page payload")?;
                file.seek(SeekFrom::Current(i64::try_from(payload_len).map_err(
                    |_| XtaskError::Message("dirty page payload too large".to_string()),
                )?))
                .map_err(|e| XtaskError::Message(format!("seek dirty page payload: {e}")))?;
            }
        }
    }

    Ok(())
}

fn validate_compressed_len(
    compression: Compression,
    uncompressed_len: u32,
    compressed_len: u32,
) -> Result<()> {
    match compression {
        Compression::None => {
            if compressed_len != uncompressed_len {
                return Err(XtaskError::Message(
                    "compressed_len must equal uncompressed_len for no compression".to_string(),
                ));
            }
        }
        Compression::Lz4 => {
            let max = lz4_flex::block::get_maximum_output_size(uncompressed_len as usize) as u32;
            if compressed_len > max {
                return Err(XtaskError::Message("lz4 chunk too large".to_string()));
            }
        }
    }
    Ok(())
}

fn validate_string_u32_utf8(r: &mut impl Read, max_len: u32, what: &str) -> Result<()> {
    let len = read_u32_le(r)?;
    if len > max_len {
        return Err(XtaskError::Message(format!("{what} too long")));
    }
    let mut limited = r.take(len as u64);
    validate_utf8_bytes(&mut limited, what)?;
    if limited.limit() != 0 {
        return Err(XtaskError::Message(format!(
            "{what}: truncated string bytes"
        )));
    }
    Ok(())
}

fn validate_utf8_bytes<R: Read>(r: &mut std::io::Take<R>, what: &str) -> Result<()> {
    const CHUNK: usize = 8 * 1024;
    let mut buf = [0u8; CHUNK];
    let mut tmp = [0u8; CHUNK + 4];
    let mut carry = [0u8; 4];
    let mut carry_len = 0usize;

    loop {
        let n = r
            .read(&mut buf)
            .map_err(|e| XtaskError::Message(format!("{what}: {e}")))?;
        if n == 0 {
            break;
        }

        tmp[..carry_len].copy_from_slice(&carry[..carry_len]);
        tmp[carry_len..carry_len + n].copy_from_slice(&buf[..n]);
        let slice = &tmp[..carry_len + n];

        match std::str::from_utf8(slice) {
            Ok(_) => carry_len = 0,
            Err(e) => match e.error_len() {
                Some(_) => return Err(XtaskError::Message(format!("{what}: invalid utf-8"))),
                None => {
                    let valid = e.valid_up_to();
                    let remaining = &slice[valid..];
                    carry_len = remaining.len();
                    carry[..carry_len].copy_from_slice(remaining);
                }
            },
        }
    }

    if r.limit() != 0 {
        return Err(XtaskError::Message(format!(
            "{what}: truncated string bytes"
        )));
    }
    if carry_len != 0 {
        return Err(XtaskError::Message(format!("{what}: invalid utf-8")));
    }

    Ok(())
}

fn read_u16_le(r: &mut impl Read) -> Result<u16> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)
        .map_err(|e| XtaskError::Message(format!("read u16: {e}")))?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32_le(r: &mut impl Read) -> Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)
        .map_err(|e| XtaskError::Message(format!("read u32: {e}")))?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u64_le(r: &mut impl Read) -> Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)
        .map_err(|e| XtaskError::Message(format!("read u64: {e}")))?;
    Ok(u64::from_le_bytes(buf))
}

fn ensure_section_remaining(
    file: &mut fs::File,
    section_end: u64,
    need: u64,
    what: &str,
) -> Result<()> {
    let pos = file
        .stream_position()
        .map_err(|e| XtaskError::Message(format!("tell {what}: {e}")))?;
    let end = pos
        .checked_add(need)
        .ok_or_else(|| XtaskError::Message("section offset overflow".to_string()))?;
    if end > section_end {
        return Err(XtaskError::Message(format!("{what}: truncated section")));
    }
    Ok(())
}
