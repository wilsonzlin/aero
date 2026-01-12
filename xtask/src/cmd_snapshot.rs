use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};

use aero_snapshot::{
    limits, Compression, CpuState, DeviceId, DiskOverlayRefs, MmuState, RamMode, SectionId,
    SnapshotError, SnapshotIndex, SnapshotSectionInfo, SnapshotTarget,
};

use crate::error::{Result, XtaskError};

pub fn print_help() {
    println!(
        "\
Inspect and validate Aero snapshots (`aero-snapshot`).

Usage:
  cargo xtask snapshot inspect <path>
  cargo xtask snapshot validate [--deep] <path>

Subcommands:
  inspect    Print header, META fields, section table, and per-section summaries
            (CPU/MMU/DEVICES/CPUS/DISKS/RAM when present).
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

    if let Some(cpu) = index.sections.iter().find(|s| s.id == SectionId::CPU) {
        println!("CPU:");
        print_cpu_section_summary(&mut file, cpu);
    }
    if let Some(mmu) = index.sections.iter().find(|s| s.id == SectionId::MMU) {
        println!("MMU:");
        print_mmu_section_summary(&mut file, mmu);
    }
    if let Some(devices) = index.sections.iter().find(|s| s.id == SectionId::DEVICES) {
        println!("DEVICES:");
        print_devices_section_summary(&mut file, devices);
    }
    if let Some(cpus) = index.sections.iter().find(|s| s.id == SectionId::CPUS) {
        println!("CPUS:");
        print_cpus_section_summary(&mut file, cpus);
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

fn print_cpu_section_summary(file: &mut fs::File, section: &SnapshotSectionInfo) {
    let section_end = match section.offset.checked_add(section.len) {
        Some(v) => v,
        None => {
            println!("  <invalid section length>");
            return;
        }
    };

    if section.len == 0 {
        println!("  <empty section>");
        return;
    }

    if let Err(e) = file.seek(SeekFrom::Start(section.offset)) {
        println!("  <failed to seek: {e}>");
        return;
    }

    let mut limited = file.take(section.len);
    let cpu = if section.version == 1 {
        CpuState::decode_v1(&mut limited)
    } else if section.version >= 2 {
        CpuState::decode_v2(&mut limited)
    } else {
        println!("  <unsupported CPU section version {}>", section.version);
        return;
    };

    let cpu = match cpu {
        Ok(v) => v,
        Err(e) => {
            println!("  <failed to decode CPU state: {e}>");
            return;
        }
    };

    // Best-effort summary: keep this small and stable so it is useful for quick inspection without
    // dumping large blobs like FXSAVE.
    println!("  mode: {:?}", cpu.mode);
    println!("  halted: {}", cpu.halted);
    println!("  rip: 0x{:x}", cpu.rip);
    println!("  rflags: 0x{:x}", cpu.rflags);
    println!("  a20_enabled: {}", cpu.a20_enabled);

    // Ensure we don't accidentally run past the declared section bounds when decoding corrupted
    // snapshots (defensive; decode already reads via the bounded `Take`).
    let _ = section_end;
}

fn print_mmu_section_summary(file: &mut fs::File, section: &SnapshotSectionInfo) {
    if section.len == 0 {
        println!("  <empty section>");
        return;
    }
    if let Err(e) = file.seek(SeekFrom::Start(section.offset)) {
        println!("  <failed to seek: {e}>");
        return;
    }
    let mut limited = file.take(section.len);
    let mmu = if section.version == 1 {
        MmuState::decode_v1(&mut limited)
    } else if section.version >= 2 {
        MmuState::decode_v2(&mut limited)
    } else {
        println!("  <unsupported MMU section version {}>", section.version);
        return;
    };
    let mmu = match mmu {
        Ok(v) => v,
        Err(e) => {
            println!("  <failed to decode MMU state: {e}>");
            return;
        }
    };

    // Best-effort summary: keep this small and stable; avoid printing large arrays.
    println!("  cr0: 0x{:x}", mmu.cr0);
    println!("  cr3: 0x{:x}", mmu.cr3);
    println!("  cr4: 0x{:x}", mmu.cr4);
    println!("  efer: 0x{:x}", mmu.efer);
    println!("  apic_base: 0x{:x}", mmu.apic_base);
    println!("  tsc: 0x{:x}", mmu.tsc);
    println!(
        "  gdtr: base=0x{:x} limit=0x{:x}",
        mmu.gdtr_base, mmu.gdtr_limit
    );
    println!(
        "  idtr: base=0x{:x} limit=0x{:x}",
        mmu.idtr_base, mmu.idtr_limit
    );
}

fn print_devices_section_summary(file: &mut fs::File, section: &SnapshotSectionInfo) {
    if section.version != 1 {
        println!(
            "  <unsupported DEVICES section version {}>",
            section.version
        );
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
    if count > limits::MAX_DEVICE_COUNT {
        println!("  <too many devices: {count}>");
        return;
    }

    #[derive(Debug, Clone)]
    struct DeviceSummaryEntry {
        id: u32,
        version: u16,
        flags: u16,
        len: u64,
        inner: Option<DeviceInnerHeader>,
        detail: Option<String>,
    }

    #[derive(Debug, Clone)]
    enum DeviceInnerHeader {
        IoSnapshot {
            device_id: [u8; 4],
            device_version: (u16, u16),
            format_version: (u16, u16),
        },
        LegacyAero {
            version: u16,
            flags: u16,
        },
    }

    fn is_ascii_tag_byte(b: u8) -> bool {
        b.is_ascii_alphanumeric() || b == b'_'
    }

    fn format_fourcc(id: [u8; 4]) -> String {
        if id.iter().copied().all(is_ascii_tag_byte) {
            String::from_utf8_lossy(&id).into_owned()
        } else {
            format!("0x{:02x}{:02x}{:02x}{:02x}", id[0], id[1], id[2], id[3])
        }
    }

    fn parse_device_inner_header(buf: &[u8]) -> Option<DeviceInnerHeader> {
        if buf.len() < 4 || &buf[0..4] != b"AERO" {
            return None;
        }

        // Try to detect an `aero-io-snapshot` header by checking whether the device id slot looks
        // like a 4CC. If not, fall back to the legacy 8-byte `AERO` header (`u16 version, u16
        // flags`).
        if buf.len() >= 16 {
            let device_id = [buf[8], buf[9], buf[10], buf[11]];
            if device_id.iter().copied().all(is_ascii_tag_byte) {
                let format_major = u16::from_le_bytes([buf[4], buf[5]]);
                let format_minor = u16::from_le_bytes([buf[6], buf[7]]);
                let dev_major = u16::from_le_bytes([buf[12], buf[13]]);
                let dev_minor = u16::from_le_bytes([buf[14], buf[15]]);
                return Some(DeviceInnerHeader::IoSnapshot {
                    device_id,
                    device_version: (dev_major, dev_minor),
                    format_version: (format_major, format_minor),
                });
            }
        }

        if buf.len() >= 8 {
            let version = u16::from_le_bytes([buf[4], buf[5]]);
            let flags = u16::from_le_bytes([buf[6], buf[7]]);
            return Some(DeviceInnerHeader::LegacyAero { version, flags });
        }

        None
    }

    fn bdf_to_string(bdf: u16) -> String {
        let bus = (bdf >> 8) & 0xff;
        let dev = (bdf >> 3) & 0x1f;
        let func = bdf & 0x07;
        format!("{bus:02x}:{dev:02x}.{func:x}")
    }

    let mut entries: Vec<DeviceSummaryEntry> = Vec::with_capacity(count as usize);
    for _ in 0..count {
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
            println!("  <truncated section>");
            return;
        }

        // Read a small prefix of the device payload (up to 16 bytes). This lets `inspect` infer the
        // inner `aero-io-snapshot` 4CC when present and show small debug summaries for a handful of
        // common wrapper formats.
        let header_len = usize::try_from(len.min(16)).unwrap_or(16);
        let mut header = [0u8; 16];
        if header_len != 0 {
            if let Err(e) = file.read_exact(&mut header[..header_len]) {
                println!("  <failed to read device payload header: {e}>");
                return;
            }
        }

        // Parse `aero-io-snapshot` / legacy `AERO` device header for debugging.
        let mut detail: Option<String> = None;
        let inner = if header_len >= 4 {
            let inner = parse_device_inner_header(&header[..header_len]);

            // Best-effort nested decoding for known wrapper payloads. This intentionally does not
            // affect validation; it's purely a debugging aid.
            if header_len == 16 {
                if let Some(DeviceInnerHeader::IoSnapshot { device_id, .. }) = inner.as_ref() {
                    // DiskControllersSnapshot (`DSKC`): tag 1 holds an `Encoder::vec_bytes` list of
                    // (packed_bdf: u16 + nested io-snapshot bytes) entries.
                    if device_id == b"DSKC" {
                        let mut controllers: Vec<String> = Vec::new();
                        let mut controller_count: Option<u32> = None;

                        loop {
                            let Ok(pos) = file.stream_position() else {
                                break;
                            };
                            if pos >= data_end {
                                break;
                            }
                            if data_end - pos < 6 {
                                break;
                            }
                            let Ok(tag) = read_u16_le_lossy(file) else {
                                break;
                            };
                            let Ok(field_len) = read_u32_le_lossy(file) else {
                                break;
                            };
                            let Ok(field_start) = file.stream_position() else {
                                break;
                            };
                            let Some(field_end) = field_start.checked_add(u64::from(field_len))
                            else {
                                break;
                            };
                            if field_end > data_end {
                                break;
                            }

                            if tag == 1 {
                                let Ok(count) = read_u32_le_lossy(file) else {
                                    break;
                                };
                                controller_count = Some(count);
                                for _ in 0..count {
                                    let Ok(entry_len) = read_u32_le_lossy(file) else {
                                        break;
                                    };
                                    if entry_len < 2 {
                                        break;
                                    }
                                    let Ok(entry_start) = file.stream_position() else {
                                        break;
                                    };
                                    let Some(entry_end) =
                                        entry_start.checked_add(u64::from(entry_len))
                                    else {
                                        break;
                                    };
                                    if entry_end > field_end {
                                        break;
                                    }

                                    let Ok(bdf) = read_u16_le_lossy(file) else {
                                        break;
                                    };
                                    let nested_len = u64::from(entry_len - 2);
                                    let mut nested_header = [0u8; 16];
                                    let nested_header_len =
                                        usize::try_from(nested_len.min(16)).unwrap_or(16);
                                    let nested = if nested_header_len != 0
                                        && file
                                            .read_exact(&mut nested_header[..nested_header_len])
                                            .is_ok()
                                    {
                                        parse_device_inner_header(
                                            &nested_header[..nested_header_len],
                                        )
                                    } else {
                                        None
                                    };

                                    let nested_desc = match nested {
                                        Some(DeviceInnerHeader::IoSnapshot {
                                            device_id,
                                            device_version,
                                            ..
                                        }) => {
                                            let (major, minor) = device_version;
                                            format!(
                                                "{} {} v{}.{}",
                                                bdf_to_string(bdf),
                                                format_fourcc(device_id),
                                                major,
                                                minor
                                            )
                                        }
                                        Some(DeviceInnerHeader::LegacyAero { version, flags }) => {
                                            format!(
                                                "{} legacy-AERO v{} flags={}",
                                                bdf_to_string(bdf),
                                                version,
                                                flags
                                            )
                                        }
                                        None => format!("{} <unknown>", bdf_to_string(bdf)),
                                    };
                                    controllers.push(nested_desc);

                                    let _ = file.seek(SeekFrom::Start(entry_end));
                                    if controllers.len() >= 16 {
                                        break;
                                    }
                                }
                                let _ = file.seek(SeekFrom::Start(field_end));
                                break;
                            }

                            let _ = file.seek(SeekFrom::Start(field_end));
                        }

                        if let Some(count) = controller_count {
                            if !controllers.is_empty() {
                                let more = if count as usize > controllers.len() {
                                    format!(" ... ({} more)", count as usize - controllers.len())
                                } else {
                                    String::new()
                                };
                                detail = Some(format!(
                                    " controllers=[{}]{}",
                                    controllers.join(", "),
                                    more
                                ));
                            } else {
                                detail = Some(format!(" controllers={count}"));
                            }
                        }
                    }

                    // `aero_machine` USB UHCI wrapper (`USBC`): tag 1 holds the sub-ms tick
                    // remainder (u64), tag 2 holds the nested UHCI snapshot bytes.
                    if device_id == b"USBC" {
                        let mut remainder: Option<u64> = None;
                        let mut nested: Option<DeviceInnerHeader> = None;

                        loop {
                            let Ok(pos) = file.stream_position() else {
                                break;
                            };
                            if pos >= data_end {
                                break;
                            }
                            if data_end - pos < 6 {
                                break;
                            }
                            let Ok(tag) = read_u16_le_lossy(file) else {
                                break;
                            };
                            let Ok(field_len) = read_u32_le_lossy(file) else {
                                break;
                            };
                            let Ok(field_start) = file.stream_position() else {
                                break;
                            };
                            let Some(field_end) = field_start.checked_add(u64::from(field_len))
                            else {
                                break;
                            };
                            if field_end > data_end {
                                break;
                            }

                            match tag {
                                1 if field_len == 8 => {
                                    let mut buf = [0u8; 8];
                                    if file.read_exact(&mut buf).is_ok() {
                                        remainder = Some(u64::from_le_bytes(buf));
                                    }
                                    let _ = file.seek(SeekFrom::Start(field_end));
                                }
                                2 => {
                                    let mut hdr = [0u8; 16];
                                    let hdr_len =
                                        usize::try_from(u64::from(field_len).min(16)).unwrap_or(16);
                                    if hdr_len != 0 && file.read_exact(&mut hdr[..hdr_len]).is_ok()
                                    {
                                        nested = parse_device_inner_header(&hdr[..hdr_len]);
                                    }
                                    let _ = file.seek(SeekFrom::Start(field_end));
                                }
                                _ => {
                                    let _ = file.seek(SeekFrom::Start(field_end));
                                }
                            }
                        }

                        let mut parts: Vec<String> = Vec::new();
                        if let Some(r) = remainder {
                            parts.push(format!("remainder={r}ns"));
                        }
                        if let Some(nested) = nested {
                            match nested {
                                DeviceInnerHeader::IoSnapshot {
                                    device_id,
                                    device_version,
                                    ..
                                } => {
                                    let (major, minor) = device_version;
                                    parts.push(format!(
                                        "nested={} v{}.{}",
                                        format_fourcc(device_id),
                                        major,
                                        minor
                                    ));
                                }
                                DeviceInnerHeader::LegacyAero { version, flags } => {
                                    parts.push(format!(
                                        "nested=legacy-AERO v{version} flags={flags}"
                                    ));
                                }
                            }
                        }
                        if !parts.is_empty() {
                            detail = Some(format!(" {}", parts.join(" ")));
                        }
                    }

                    // Legacy PCI core wrapper (`PCIC`): tag 1 nests `PCPT`, tag 2 nests `INTX`.
                    if device_id == b"PCIC" {
                        let mut cfg: Option<DeviceInnerHeader> = None;
                        let mut intx: Option<DeviceInnerHeader> = None;

                        loop {
                            let Ok(pos) = file.stream_position() else {
                                break;
                            };
                            if pos >= data_end {
                                break;
                            }
                            if data_end - pos < 6 {
                                break;
                            }
                            let Ok(tag) = read_u16_le_lossy(file) else {
                                break;
                            };
                            let Ok(field_len) = read_u32_le_lossy(file) else {
                                break;
                            };
                            let Ok(field_start) = file.stream_position() else {
                                break;
                            };
                            let Some(field_end) = field_start.checked_add(u64::from(field_len))
                            else {
                                break;
                            };
                            if field_end > data_end {
                                break;
                            }

                            if tag == 1 || tag == 2 {
                                let mut hdr = [0u8; 16];
                                let hdr_len =
                                    usize::try_from(u64::from(field_len).min(16)).unwrap_or(16);
                                let parsed = if hdr_len != 0
                                    && file.read_exact(&mut hdr[..hdr_len]).is_ok()
                                {
                                    parse_device_inner_header(&hdr[..hdr_len])
                                } else {
                                    None
                                };
                                if let Some(parsed) = parsed {
                                    if tag == 1 {
                                        cfg = Some(parsed);
                                    } else {
                                        intx = Some(parsed);
                                    }
                                }
                            }

                            let _ = file.seek(SeekFrom::Start(field_end));
                        }

                        let mut parts: Vec<String> = Vec::new();
                        if let Some(cfg) = cfg {
                            match cfg {
                                DeviceInnerHeader::IoSnapshot {
                                    device_id,
                                    device_version,
                                    ..
                                } => {
                                    let (major, minor) = device_version;
                                    parts.push(format!(
                                        "cfg={} v{}.{}",
                                        format_fourcc(device_id),
                                        major,
                                        minor
                                    ));
                                }
                                DeviceInnerHeader::LegacyAero { version, flags } => {
                                    parts.push(format!("cfg=legacy-AERO v{version} flags={flags}"));
                                }
                            }
                        }
                        if let Some(intx) = intx {
                            match intx {
                                DeviceInnerHeader::IoSnapshot {
                                    device_id,
                                    device_version,
                                    ..
                                } => {
                                    let (major, minor) = device_version;
                                    parts.push(format!(
                                        "intx={} v{}.{}",
                                        format_fourcc(device_id),
                                        major,
                                        minor
                                    ));
                                }
                                DeviceInnerHeader::LegacyAero { version, flags } => {
                                    parts
                                        .push(format!("intx=legacy-AERO v{version} flags={flags}"));
                                }
                            }
                        }
                        if !parts.is_empty() {
                            detail = Some(format!(" {}", parts.join(" ")));
                        }
                    }
                }
            }

            // Special-case decoding for `aero_snapshot::DeviceId::CPU_INTERNAL` (raw bytes, not an
            // `aero-io-snapshot` TLV).
            //
            // This entry is expected to store `aero_snapshot::CpuInternalState` (v2) and is useful
            // to inspect when debugging interrupt-shadow/pending IRQ behavior across snapshots.
            if id == DeviceId::CPU_INTERNAL.0 && version == 2 && flags == 0 && header_len >= 5 {
                let interrupt_inhibit = header[0];
                let pending_len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]);

                let expected_len = 1u64
                    .saturating_add(4)
                    .saturating_add(u64::from(pending_len));
                let mut s =
                    format!(" interrupt_inhibit={interrupt_inhibit} pending_len={pending_len}");
                if expected_len != len {
                    s.push_str(&format!(" (expected_len={expected_len})"));
                }

                let preview_avail = header_len.saturating_sub(5);
                let preview_len = preview_avail.min(pending_len as usize).min(8);
                if preview_len != 0 {
                    s.push_str(" pending_preview=[");
                    for (idx, b) in header[5..5 + preview_len].iter().copied().enumerate() {
                        if idx != 0 {
                            s.push_str(", ");
                        }
                        s.push_str(&format!("0x{b:02x}"));
                    }
                    s.push(']');
                }

                detail = Some(s);
            }

            inner
        } else {
            None
        };

        // Machine memory/chipset glue state (`DeviceId::MEMORY`).
        if id == DeviceId::MEMORY.0 && version == 1 && flags == 0 && header_len >= 1 {
            let a20_enabled = header[0] != 0;
            detail = Some(format!(" a20_enabled={a20_enabled}"));
        }

        // Firmware/BIOS runtime state (`DeviceId::BIOS`).
        //
        // This payload is currently emitted by `firmware::bios::BiosSnapshot::encode` and is not an
        // `aero-io-snapshot` blob, so `inspect` does a small best-effort decode here.
        if id == DeviceId::BIOS.0 && version == 1 && flags == 0 {
            if let Some(s) = decode_bios_device_detail(file, data_start, data_end) {
                detail = Some(s);
            }
        }

        entries.push(DeviceSummaryEntry {
            id,
            version,
            flags,
            len,
            inner,
            detail,
        });

        if let Err(e) = file.seek(SeekFrom::Start(data_end)) {
            println!("  <failed to skip device payload: {e}>");
            return;
        }
    }

    let already_sorted = entries
        .windows(2)
        .all(|w| (w[0].id, w[0].version, w[0].flags) <= (w[1].id, w[1].version, w[1].flags));
    entries.sort_by_key(|e| (e.id, e.version, e.flags));
    if !already_sorted {
        println!(
            "  note: DEVICES entries are not sorted by (device_id, version, flags); displaying sorted order"
        );
    }
    if entries
        .windows(2)
        .any(|w| (w[0].id, w[0].version, w[0].flags) == (w[1].id, w[1].version, w[1].flags))
    {
        println!("  warning: duplicate device entries (snapshot restore would reject this file)");
    }

    println!("  count: {}", entries.len());

    const MAX_LISTED: usize = 64;
    for (idx, entry) in entries.iter().take(MAX_LISTED).enumerate() {
        let inner = match &entry.inner {
            Some(DeviceInnerHeader::IoSnapshot {
                device_id,
                device_version,
                format_version,
            }) => {
                let (major, minor) = *device_version;
                let (fmt_major, fmt_minor) = *format_version;
                let mut suffix =
                    format!(" inner={} v{}.{}", format_fourcc(*device_id), major, minor);
                if fmt_major != 1 || fmt_minor != 0 {
                    suffix.push_str(&format!(" fmt{}.{}", fmt_major, fmt_minor));
                }
                if major != entry.version || minor != entry.flags {
                    suffix.push_str(&format!(
                        " (outer v{}.{} mismatch)",
                        entry.version, entry.flags
                    ));
                }
                suffix
            }
            Some(DeviceInnerHeader::LegacyAero { version, flags }) => {
                format!(" inner=legacy-AERO v{version} flags={flags}")
            }
            None => String::new(),
        };
        let detail = entry.detail.as_deref().unwrap_or("");
        println!(
            "  - {}: {} version={} flags={} len={}{}{}",
            idx,
            DeviceId(entry.id),
            entry.version,
            entry.flags,
            entry.len,
            inner,
            detail,
        );
    }
    if entries.len() > MAX_LISTED {
        println!(
            "  ... {} more device entries omitted",
            entries.len() - MAX_LISTED
        );
    }
}

fn decode_bios_device_detail(
    file: &mut fs::File,
    data_start: u64,
    data_end: u64,
) -> Option<String> {
    fn skip_to(file: &mut fs::File, data_end: u64, skip: u64) -> Option<()> {
        let pos = file.stream_position().ok()?;
        let next = pos.checked_add(skip)?;
        if next > data_end {
            return None;
        }
        file.seek(SeekFrom::Start(next)).ok()?;
        Some(())
    }

    file.seek(SeekFrom::Start(data_start)).ok()?;

    let memory_size_bytes = read_u64_le_lossy(file).ok()?;
    let boot_drive = read_u8_lossy(file).ok()?;

    // `CmosRtcSnapshot` (14 bytes) + `BdaTimeSnapshot` (21 bytes).
    skip_to(file, data_end, 14 + 21)?;

    // E820 map.
    let e820_len = read_u32_le_lossy(file).ok()?;
    skip_to(file, data_end, 24u64.saturating_mul(u64::from(e820_len)))?;

    // Keyboard queue.
    let keys_len = read_u32_le_lossy(file).ok()?;
    skip_to(file, data_end, 2u64.saturating_mul(u64::from(keys_len)))?;

    let video_mode = read_u8_lossy(file).ok()?;

    // TTY output.
    let tty_len = read_u32_le_lossy(file).ok()?;
    skip_to(file, data_end, u64::from(tty_len))?;

    // RSDP addr (optional).
    let rsdp_present = read_u8_lossy(file).ok()?;
    let rsdp_addr = match rsdp_present {
        0 => None,
        1 => Some(read_u64_le_lossy(file).ok()?),
        _ => None,
    };

    // Optional extension blocks (best-effort).
    let mut last_int13_status: Option<u8> = None;
    let mut vbe_mode: Option<u16> = None;
    let mut cpu_count: Option<u8> = None;
    let mut enable_acpi: Option<bool> = None;

    loop {
        let pos = file.stream_position().ok()?;
        if pos >= data_end {
            break;
        }
        let tag = match read_u8_lossy(file) {
            Ok(v) => v,
            Err(_) => break,
        };
        match tag {
            // v2 extension: last INT 13h status + VBE state.
            1 => {
                last_int13_status = read_u8_lossy(file).ok();

                // `VbeSnapshot` starts with a presence byte for `current_mode`.
                let mode_tag = match read_u8_lossy(file) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                match mode_tag {
                    0 => {}
                    1 => vbe_mode = read_u16_le_lossy(file).ok(),
                    _ => break,
                }

                // Skip the rest of the VBE snapshot (fixed-size fields + palette).
                // lfb_base(u32) + bank(u16) + logical_width(u16) + bytes_per_scan_line(u16)
                // + display_start_x(u16) + display_start_y(u16) + dac_width(u8) + palette(1024).
                skip_to(file, data_end, 4 + 2 + 2 + 2 + 2 + 2 + 1 + 1024)?;
            }
            // v3 extension: BIOS config + firmware table placement metadata.
            2 => {
                cpu_count = read_u8_lossy(file).ok();
                enable_acpi = read_u8_lossy(file).ok().map(|v| v != 0);

                // Skip `AcpiPlacement` (5 * u64) + `pirq_to_gsi` ([u32; 4]).
                skip_to(file, data_end, 40 + 16)?;

                for _ in 0..2 {
                    let present = match read_u8_lossy(file) {
                        Ok(v) => v,
                        Err(_) => return None,
                    };
                    if present != 0 {
                        // base(u64) + len(u64)
                        skip_to(file, data_end, 16)?;
                    }
                }
                let present = match read_u8_lossy(file) {
                    Ok(v) => v,
                    Err(_) => return None,
                };
                if present != 0 {
                    skip_to(file, data_end, 4)?;
                }
            }
            _ => break,
        }
    }

    let mut s = format!(
        " boot_drive=0x{boot_drive:02x} mem_size_bytes={memory_size_bytes} video_mode=0x{video_mode:02x} tty_len={tty_len} e820_len={e820_len} keys_len={keys_len}"
    );
    if let Some(addr) = rsdp_addr {
        s.push_str(&format!(" rsdp_addr=0x{addr:x}"));
    }
    if let Some(status) = last_int13_status {
        s.push_str(&format!(" last_int13_status=0x{status:02x}"));
    }
    if let Some(mode) = vbe_mode {
        s.push_str(&format!(" vbe_mode=0x{mode:04x}"));
    }
    if let Some(count) = cpu_count {
        s.push_str(&format!(" cpu_count={count}"));
    }
    if let Some(enabled) = enable_acpi {
        s.push_str(&format!(" acpi={enabled}"));
    }

    Some(s)
}

fn print_cpus_section_summary(file: &mut fs::File, section: &SnapshotSectionInfo) {
    const MAX_LISTED: usize = 64;

    if section.version == 0 {
        println!("  <unsupported CPUS section version {}>", section.version);
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
            println!("  <failed to read CPU count: {e}>");
            return;
        }
    };
    if count > limits::MAX_CPU_COUNT {
        println!("  <too many CPUs: {count}>");
        return;
    }

    #[derive(Debug, Clone)]
    struct CpuSummaryEntry {
        apic_id: u32,
        entry_len: u64,
        rip: Option<u64>,
        mode: Option<String>,
        halted: Option<bool>,
        internal_len: Option<u64>,
        internal_preview: Option<Vec<u8>>,
        decode_error: Option<String>,
    }

    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let pos = match file.stream_position() {
            Ok(v) => v,
            Err(e) => {
                println!("  <failed to read CPU entry: {e}>");
                return;
            }
        };
        if pos >= section_end {
            println!("  <truncated section>");
            return;
        }
        if section_end - pos < 8 {
            println!("  <truncated section>");
            return;
        }

        let entry_len = match read_u64_le_lossy(file) {
            Ok(v) => v,
            Err(e) => {
                println!("  <failed to read CPU entry length: {e}>");
                return;
            }
        };
        let entry_start = match file.stream_position() {
            Ok(v) => v,
            Err(e) => {
                println!("  <failed to read CPU entry: {e}>");
                return;
            }
        };
        let entry_end = match entry_start.checked_add(entry_len) {
            Some(v) => v,
            None => {
                println!("  <cpu entry length overflow>");
                return;
            }
        };
        if entry_end > section_end {
            println!("  <truncated section>");
            return;
        }
        if entry_len < 4 {
            println!("  <truncated CPU entry>");
            return;
        }

        let mut apic_id: u32 = 0;
        let mut rip: Option<u64> = None;
        let mut mode: Option<String> = None;
        let mut halted: Option<bool> = None;
        let mut internal_len: Option<u64> = None;
        let mut internal_preview: Option<Vec<u8>> = None;
        let mut decode_error: Option<String> = None;

        {
            let mut entry_reader = file.take(entry_len);
            match read_u32_le_lossy(&mut entry_reader) {
                Ok(v) => apic_id = v,
                Err(e) => decode_error = Some(format!("apic_id: {e}")),
            }

            let cpu = if section.version == 1 {
                CpuState::decode_v1(&mut entry_reader)
            } else {
                CpuState::decode_v2(&mut entry_reader)
            };
            match cpu {
                Ok(cpu) => {
                    rip = Some(cpu.rip);
                    if section.version >= 2 {
                        mode = Some(format!("{:?}", cpu.mode));
                        halted = Some(cpu.halted);
                    }
                }
                Err(e) => {
                    decode_error = Some(format!("cpu: {e}"));
                }
            }

            match read_u64_le_lossy(&mut entry_reader) {
                Ok(v) => {
                    internal_len = Some(v);
                    if v != 0 {
                        let preview_len = (v as usize).min(8);
                        if preview_len != 0 {
                            let mut preview = vec![0u8; preview_len];
                            match entry_reader.read_exact(&mut preview) {
                                Ok(()) => internal_preview = Some(preview),
                                Err(e) => {
                                    if decode_error.is_none() {
                                        decode_error = Some(format!("internal_state: {e}"));
                                    }
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    if decode_error.is_none() {
                        decode_error = Some(format!("internal_len: {e}"));
                    }
                }
            }
        }

        entries.push(CpuSummaryEntry {
            apic_id,
            entry_len,
            rip,
            mode,
            halted,
            internal_len,
            internal_preview,
            decode_error,
        });

        if let Err(e) = file.seek(SeekFrom::Start(entry_end)) {
            println!("  <failed to skip CPU entry: {e}>");
            return;
        }
    }

    let already_sorted = entries.windows(2).all(|w| w[0].apic_id <= w[1].apic_id);
    entries.sort_by_key(|e| e.apic_id);
    if !already_sorted {
        println!("  note: CPUS entries are not sorted by apic_id; displaying sorted order");
    }
    if entries.windows(2).any(|w| w[0].apic_id == w[1].apic_id) {
        println!("  warning: duplicate apic_id entries (snapshot restore would reject this file)");
    }

    println!("  count: {}", entries.len());
    for (idx, entry) in entries.iter().take(MAX_LISTED).enumerate() {
        let mut suffix = String::new();
        if let Some(rip) = entry.rip {
            suffix.push_str(&format!(" rip=0x{rip:x}"));
        }
        if let Some(mode) = entry.mode.as_deref() {
            suffix.push_str(&format!(" mode={mode}"));
        }
        if let Some(halted) = entry.halted {
            suffix.push_str(&format!(" halted={halted}"));
        }
        if let Some(internal_len) = entry.internal_len {
            suffix.push_str(&format!(" internal_len={internal_len}"));
        }
        if let Some(preview) = entry.internal_preview.as_deref() {
            suffix.push_str(" internal_preview=[");
            for (idx, b) in preview.iter().copied().enumerate() {
                if idx != 0 {
                    suffix.push_str(", ");
                }
                suffix.push_str(&format!("0x{b:02x}"));
            }
            suffix.push(']');
        }
        if let Some(err) = entry.decode_error.as_deref() {
            suffix.push_str(&format!(" <{err}>"));
        }
        println!(
            "  - {}: apic_id={} entry_len={}{}",
            idx, entry.apic_id, entry.entry_len, suffix
        );
    }
    if entries.len() > MAX_LISTED {
        println!("  ... ({} more)", entries.len() - MAX_LISTED);
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
    let Ok(mut disks) = DiskOverlayRefs::decode(&mut limited) else {
        println!("  <failed to decode DISKS payload>");
        return;
    };

    let already_sorted = disks.disks.windows(2).all(|w| w[0].disk_id <= w[1].disk_id);
    disks.disks.sort_by_key(|disk| disk.disk_id);
    if !already_sorted {
        println!("  note: DISKS entries are not sorted by disk_id; displaying sorted order");
    }
    if disks.disks.windows(2).any(|w| w[0].disk_id == w[1].disk_id) {
        println!("  warning: duplicate disk_id entries (snapshot restore would reject this file)");
    }

    println!("  count: {}", disks.disks.len());
    for (idx, disk) in disks.disks.iter().take(MAX_PRINT_DISKS).enumerate() {
        fn truncate(s: &str, max_chars: usize) -> String {
            if s.chars().count() <= max_chars {
                return s.to_string();
            }
            let mut out: String = s.chars().take(max_chars).collect();
            out.push('…');
            out
        }

        fn display_disk_ref(s: &str, max_chars: usize) -> String {
            if s.is_empty() {
                // Some snapshot adapters intentionally emit placeholder disk entries with empty
                // fields to preserve stable `disk_id` mappings even when a host backend is not
                // configured.
                "<unset>".to_string()
            } else {
                truncate(s, max_chars)
            }
        }

        let base = display_disk_ref(&disk.base_image, MAX_PRINT_CHARS);
        let overlay = display_disk_ref(&disk.overlay_image, MAX_PRINT_CHARS);

        // Convenience hint: Aero's canonical Windows 7 storage topology uses stable `disk_id`s.
        // (See `docs/05-storage-topology-win7.md`.)
        let win7_slot = match disk.disk_id {
            0 => Some("win7: primary_hdd (AHCI port 0)"),
            1 => Some("win7: install_media (IDE secondary master ATAPI)"),
            2 => Some("win7: ide_primary_master (IDE primary master ATA)"),
            _ => None,
        };
        println!(
            "  - [{idx}] disk_id={}{} base_image={base:?} overlay_image={overlay:?}",
            disk.disk_id,
            win7_slot
                .map(|slot| format!(" ({slot})"))
                .unwrap_or_default(),
        );
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

fn read_u8_lossy(r: &mut impl Read) -> std::io::Result<u8> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf)?;
    Ok(buf[0])
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
    if count > limits::MAX_CPU_COUNT {
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
    if internal_len > limits::MAX_VCPU_INTERNAL_LEN {
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
    if section.len > limits::MAX_DEVICES_SECTION_LEN {
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
    if count > limits::MAX_DEVICE_COUNT {
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
        if len > limits::MAX_DEVICE_ENTRY_LEN {
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
    if count > limits::MAX_DISK_REFS {
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
        validate_string_u32_utf8(&mut limited, limits::MAX_DISK_PATH_LEN, "disk base_image")?;
        validate_string_u32_utf8(
            &mut limited,
            limits::MAX_DISK_PATH_LEN,
            "disk overlay_image",
        )?;
    }

    Ok(())
}

fn validate_ram_section(file: &mut fs::File, section: &SnapshotSectionInfo) -> Result<()> {
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
    if page_size == 0 || page_size > limits::MAX_RAM_PAGE_SIZE {
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
            if chunk_size == 0 || chunk_size > limits::MAX_RAM_CHUNK_SIZE {
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
