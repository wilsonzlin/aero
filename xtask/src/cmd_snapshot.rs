use std::fs;
use std::io::{Read, Seek, SeekFrom};

use aero_snapshot::{
    Compression, CpuState, DiskOverlayRef, RamMode, SectionId, SnapshotError, SnapshotIndex,
    SnapshotSectionInfo, SnapshotTarget,
};

use crate::error::{Result, XtaskError};

const MAX_DEVICE_COUNT: u32 = 4096;
const MAX_DEVICE_ENTRY_LEN: u64 = 64 * 1024 * 1024;
const MAX_CPU_COUNT: u32 = 4096;
const MAX_VCPU_INTERNAL_LEN: u64 = 64 * 1024 * 1024;
const MAX_DISK_REFS: u32 = 1024;

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
                    .unwrap_or_else(|| "None".to_string())
            );
            println!("  created_unix_ms: {}", meta.created_unix_ms);
            println!(
                "  label: {}",
                meta.label
                    .as_deref()
                    .map(|s| format!("{s:?}"))
                    .unwrap_or_else(|| "None".to_string())
            );
        }
        None => println!("  <missing>"),
    }

    println!("Sections:");
    for section in &index.sections {
        println!(
            "  - {} v{} len={} @0x{:x}",
            section.id, section.version, section.len, section.offset
        );
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
                let chunks = ram
                    .total_len
                    .checked_add(chunk_size as u64 - 1)
                    .unwrap_or(u64::MAX)
                    / chunk_size as u64;
                println!("  chunks: {chunks}");
            }
            if let Some(dirty_count) = ram.dirty_count {
                println!("  dirty_pages: {dirty_count}");
            }
        }
        None => println!("  <missing>"),
    }

    Ok(())
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
    let has_cpu = index
        .sections
        .iter()
        .any(|s| s.id == SectionId::CPU || s.id == SectionId::CPUS);
    if !has_cpu {
        return Err(XtaskError::Message("missing CPU/CPUS section".to_string()));
    }

    let has_ram = index.sections.iter().any(|s| s.id == SectionId::RAM);
    if !has_ram {
        return Err(XtaskError::Message("missing RAM section".to_string()));
    }
    if index.ram.is_none() {
        return Err(XtaskError::Message(
            "missing or unsupported RAM section".to_string(),
        ));
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
            id if id == SectionId::RAM => {
                // RAM header validation is handled by `inspect_snapshot`.
                if section.version != 1 {
                    return Err(XtaskError::Message(
                        "unsupported RAM section version".to_string(),
                    ));
                }
            }
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
        _states: Vec<aero_snapshot::VcpuSnapshot>,
    ) -> aero_snapshot::Result<()> {
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
    if count > MAX_CPU_COUNT {
        return Err(XtaskError::Message("too many CPUs".to_string()));
    }

    for _ in 0..count {
        let entry_len = read_u64_le(&mut section_reader)?;
        if entry_len > section_reader.limit() {
            return Err(XtaskError::Message("truncated CPU entry".to_string()));
        }

        let mut entry_reader = (&mut section_reader).take(entry_len);
        validate_vcpu_entry(&mut entry_reader, section.version)?;
        // Skip any forward-compatible additions.
        std::io::copy(&mut entry_reader, &mut std::io::sink())
            .map_err(|e| XtaskError::Message(format!("read CPU entry: {e}")))?;
    }

    Ok(())
}

fn validate_vcpu_entry(entry_reader: &mut impl Read, version: u16) -> Result<()> {
    let _apic_id = read_u32_le(entry_reader)?;

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

    std::io::copy(&mut entry_reader.take(internal_len), &mut std::io::sink())
        .map_err(|e| XtaskError::Message(format!("read vCPU internal state: {e}")))?;

    Ok(())
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

    let section_end = section
        .offset
        .checked_add(section.len)
        .ok_or_else(|| XtaskError::Message("devices section overflow".to_string()))?;

    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek DEVICES: {e}")))?;

    let count = read_u32_le(file)?;
    if count > MAX_DEVICE_COUNT {
        return Err(XtaskError::Message("too many devices".to_string()));
    }

    for _ in 0..count {
        let _id = read_u32_le(file)?;
        let _version = read_u16_le(file)?;
        let _flags = read_u16_le(file)?;
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

    for _ in 0..count {
        let _ = DiskOverlayRef::decode(&mut limited)
            .map_err(|e| XtaskError::Message(format!("decode disk ref: {e}")))?;
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
