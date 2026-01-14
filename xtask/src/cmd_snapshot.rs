use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};

use aero_snapshot::{
    limits, Compression, CpuState, DeviceId, DiskOverlayRefs, MmuState, RamMode, SectionId,
    SnapshotError, SnapshotIndex, SnapshotSectionInfo, SnapshotTarget, VcpuMmuSnapshot,
};

use crate::error::{Result, XtaskError};

pub fn print_help() {
    println!(
        "\
Inspect and validate Aero snapshots (`aero-snapshot`).

Usage:
  cargo xtask snapshot inspect <path>
  cargo xtask snapshot validate [--deep] <path>
  cargo xtask snapshot diff <path_a> <path_b> [--deep]

Subcommands:
  inspect    Print header, META fields, section table, and per-section summaries
            (CPU/MMU/DEVICES/CPUS/MMUS/DISKS/RAM when present).
  validate   Structural validation without decompressing RAM.
            Use --deep to fully restore/decompress into a dummy target (small files only).
  diff       Compare two snapshots and print meaningful differences (header/META/sections,
            CPU/MMU/DISKS summaries, DEVICES blob digests, RAM header fields, and a few
            sampled RAM chunk/page headers).
            Use --deep to restore/decompress (small files only) and compare RAM page hashes.
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
        "diff" => cmd_diff(it.collect()),
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
    if let Some(mmus) = index.sections.iter().find(|s| s.id == SectionId::MMUS) {
        println!("MMUS:");
        print_mmus_section_summary(&mut file, mmus);
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

            if let Some(ram_section) = index.sections.iter().find(|s| s.id == SectionId::RAM) {
                print_ram_section_samples(&mut file, ram_section);
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
    println!("  pending_bios_int_valid: {}", cpu.pending_bios_int_valid);
    if cpu.pending_bios_int_valid {
        println!("  pending_bios_int: 0x{:02x}", cpu.pending_bios_int);
    }
    println!("  irq13_pending: {}", cpu.irq13_pending);

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

fn print_mmus_section_summary(file: &mut fs::File, section: &SnapshotSectionInfo) {
    const MAX_LISTED: usize = 64;

    if section.version == 0 {
        println!("  <unsupported MMUS section version {}>", section.version);
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
            println!("  <failed to read MMU count: {e}>");
            return;
        }
    };
    if count > limits::MAX_CPU_COUNT {
        println!("  <too many MMU states: {count}>");
        return;
    }

    #[derive(Debug, Clone)]
    struct MmuSummaryEntry {
        apic_id: u32,
        entry_len: u64,
        cr0: Option<u64>,
        cr3: Option<u64>,
        cr4: Option<u64>,
        efer: Option<u64>,
        apic_base: Option<u64>,
        tsc: Option<u64>,
        decode_error: Option<String>,
    }

    let mut entries = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let pos = match file.stream_position() {
            Ok(v) => v,
            Err(e) => {
                println!("  <failed to read MMU entry: {e}>");
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
                println!("  <failed to read MMU entry length: {e}>");
                return;
            }
        };

        let entry_start = match file.stream_position() {
            Ok(v) => v,
            Err(e) => {
                println!("  <failed to read MMU entry: {e}>");
                return;
            }
        };
        let entry_end = match entry_start.checked_add(entry_len) {
            Some(v) => v,
            None => {
                println!("  <mmu entry length overflow>");
                return;
            }
        };
        if entry_end > section_end {
            println!("  <truncated section>");
            return;
        }
        if entry_len < 4 {
            println!("  <truncated MMU entry>");
            return;
        }

        let mut apic_id: u32 = 0;
        let mut cr0: Option<u64> = None;
        let mut cr3: Option<u64> = None;
        let mut cr4: Option<u64> = None;
        let mut efer: Option<u64> = None;
        let mut apic_base: Option<u64> = None;
        let mut tsc: Option<u64> = None;
        let mut decode_error: Option<String> = None;

        {
            let mut entry_reader = file.take(entry_len);
            match read_u32_le_lossy(&mut entry_reader) {
                Ok(v) => apic_id = v,
                Err(e) => decode_error = Some(format!("apic_id: {e}")),
            }

            let mmu = if section.version == 1 {
                MmuState::decode_v1(&mut entry_reader)
            } else {
                MmuState::decode_v2(&mut entry_reader)
            };

            match mmu {
                Ok(mmu) => {
                    cr0 = Some(mmu.cr0);
                    cr3 = Some(mmu.cr3);
                    cr4 = Some(mmu.cr4);
                    efer = Some(mmu.efer);
                    apic_base = Some(mmu.apic_base);
                    tsc = Some(mmu.tsc);
                }
                Err(e) => decode_error = Some(format!("mmu: {e}")),
            }
        }

        entries.push(MmuSummaryEntry {
            apic_id,
            entry_len,
            cr0,
            cr3,
            cr4,
            efer,
            apic_base,
            tsc,
            decode_error,
        });

        if let Err(e) = file.seek(SeekFrom::Start(entry_end)) {
            println!("  <failed to skip MMU entry: {e}>");
            return;
        }
    }

    let already_sorted = entries.windows(2).all(|w| w[0].apic_id <= w[1].apic_id);
    entries.sort_by_key(|e| e.apic_id);
    if !already_sorted {
        println!("  note: MMUS entries are not sorted by apic_id; displaying sorted order");
    }
    if entries.windows(2).any(|w| w[0].apic_id == w[1].apic_id) {
        println!("  warning: duplicate apic_id entries (snapshot restore would reject this file)");
    }

    println!("  count: {}", entries.len());
    for (idx, entry) in entries.iter().take(MAX_LISTED).enumerate() {
        let mut suffix = String::new();
        if let Some(cr0) = entry.cr0 {
            suffix.push_str(&format!(" cr0=0x{cr0:x}"));
        }
        if let Some(cr3) = entry.cr3 {
            suffix.push_str(&format!(" cr3=0x{cr3:x}"));
        }
        if let Some(cr4) = entry.cr4 {
            suffix.push_str(&format!(" cr4=0x{cr4:x}"));
        }
        if let Some(efer) = entry.efer {
            suffix.push_str(&format!(" efer=0x{efer:x}"));
        }
        if let Some(apic_base) = entry.apic_base {
            suffix.push_str(&format!(" apic_base=0x{apic_base:x}"));
        }
        if let Some(tsc) = entry.tsc {
            suffix.push_str(&format!(" tsc=0x{tsc:x}"));
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

    fn escape_preview_bytes(bytes: &[u8]) -> String {
        let mut out = String::new();
        for b in bytes.iter().copied() {
            match b {
                b'\n' => out.push_str("\\n"),
                b'\r' => out.push_str("\\r"),
                b'\t' => out.push_str("\\t"),
                b'\\' => out.push_str("\\\\"),
                b'"' => out.push_str("\\\""),
                0x20..=0x7e => out.push(char::from(b)),
                other => out.push_str(&format!("\\x{other:02x}")),
            }
        }
        out
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

                    // `aero_machine` USB controller wrapper (`USBC`).
                    if device_id == b"USBC" {
                        // `MachineUsbSnapshot` TLV fields:
                        // - 1: uhci_ns_remainder (u64)
                        // - 2: uhci_state (bytes, typically `UHCP`)
                        // - 3: ehci_ns_remainder (u64)
                        // - 4: ehci_state (bytes, typically `EHCP`)
                        // - 5: xhci_ns_remainder (u64) (future)
                        // - 6: xhci_state (bytes, typically `XHCP`) (future)
                        let mut uhci_ns_remainder: Option<u64> = None;
                        let mut uhci_nested: Option<DeviceInnerHeader> = None;
                        let mut ehci_ns_remainder: Option<u64> = None;
                        let mut ehci_nested: Option<DeviceInnerHeader> = None;
                        let mut xhci_ns_remainder: Option<u64> = None;
                        let mut xhci_nested: Option<DeviceInnerHeader> = None;

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
                                1 | 3 | 5 => {
                                    if field_len >= 8 {
                                        let mut buf = [0u8; 8];
                                        if file.read_exact(&mut buf).is_ok() {
                                            let v = u64::from_le_bytes(buf);
                                            match tag {
                                                1 => uhci_ns_remainder = Some(v),
                                                3 => ehci_ns_remainder = Some(v),
                                                5 => xhci_ns_remainder = Some(v),
                                                _ => {}
                                            }
                                        }
                                    }
                                    let _ = file.seek(SeekFrom::Start(field_end));
                                }
                                2 | 4 | 6 => {
                                    let mut hdr = [0u8; 16];
                                    let hdr_len =
                                        usize::try_from(u64::from(field_len).min(16)).unwrap_or(16);
                                    if hdr_len != 0 && file.read_exact(&mut hdr[..hdr_len]).is_ok()
                                    {
                                        let parsed = parse_device_inner_header(&hdr[..hdr_len]);
                                        match tag {
                                            2 => uhci_nested = parsed,
                                            4 => ehci_nested = parsed,
                                            6 => xhci_nested = parsed,
                                            _ => {}
                                        }
                                    }
                                    let _ = file.seek(SeekFrom::Start(field_end));
                                }
                                _ => {
                                    let _ = file.seek(SeekFrom::Start(field_end));
                                }
                            }
                        }

                        let mut parts: Vec<String> = Vec::new();
                        if let Some(r) = uhci_ns_remainder {
                            parts.push(format!("uhci_ns_remainder={r}ns"));
                        }
                        if let Some(nested) = uhci_nested {
                            match nested {
                                DeviceInnerHeader::IoSnapshot {
                                    device_id,
                                    device_version,
                                    ..
                                } => {
                                    let (major, minor) = device_version;
                                    parts.push(format!(
                                        "uhci_nested={} v{}.{}",
                                        format_fourcc(device_id),
                                        major,
                                        minor
                                    ));
                                }
                                DeviceInnerHeader::LegacyAero { version, flags } => {
                                    parts.push(format!(
                                        "uhci_nested=legacy-AERO v{version} flags={flags}"
                                    ));
                                }
                            }
                        }
                        if let Some(r) = ehci_ns_remainder {
                            parts.push(format!("ehci_ns_remainder={r}ns"));
                        }
                        if let Some(nested) = ehci_nested {
                            match nested {
                                DeviceInnerHeader::IoSnapshot {
                                    device_id,
                                    device_version,
                                    ..
                                } => {
                                    let (major, minor) = device_version;
                                    parts.push(format!(
                                        "ehci_nested={} v{}.{}",
                                        format_fourcc(device_id),
                                        major,
                                        minor
                                    ));
                                }
                                DeviceInnerHeader::LegacyAero { version, flags } => {
                                    parts.push(format!(
                                        "ehci_nested=legacy-AERO v{version} flags={flags}"
                                    ));
                                }
                            }
                        }
                        if let Some(r) = xhci_ns_remainder {
                            parts.push(format!("xhci_ns_remainder={r}ns"));
                        }
                        if let Some(nested) = xhci_nested {
                            match nested {
                                DeviceInnerHeader::IoSnapshot {
                                    device_id,
                                    device_version,
                                    ..
                                } => {
                                    let (major, minor) = device_version;
                                    parts.push(format!(
                                        "xhci_nested={} v{}.{}",
                                        format_fourcc(device_id),
                                        major,
                                        minor
                                    ));
                                }
                                DeviceInnerHeader::LegacyAero { version, flags } => {
                                    parts.push(format!(
                                        "xhci_nested=legacy-AERO v{version} flags={flags}"
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

            // Web runtime USB snapshot container (`AUSB`).
            //
            // Format (little-endian):
            // - magic: u32 = "AUSB"
            // - version: u16
            // - flags: u16
            // - entries...:
            //   - tag: [u8;4]
            //   - len: u32
            //   - payload: [u8;len] (may contain an `AERO` io-snapshot header).
            //
            // Note: This is not an `aero-io-snapshot` blob itself, so `inner` is `None`.
            if id == DeviceId::USB.0 && header_len >= 8 && &header[0..4] == b"AUSB" {
                fn decode_ausb_container_detail(
                    file: &mut fs::File,
                    data_start: u64,
                    data_end: u64,
                ) -> Option<String> {
                    const MAGIC: u32 = 0x4253_5541;
                    const MAX_LISTED: usize = 16;

                    file.seek(SeekFrom::Start(data_start)).ok()?;
                    let magic = read_u32_le_lossy(file).ok()?;
                    if magic != MAGIC {
                        return None;
                    }
                    if data_end - data_start < 8 {
                        return None;
                    }
                    let version = read_u16_le_lossy(file).ok()?;
                    let flags = read_u16_le_lossy(file).ok()?;

                    #[derive(Debug)]
                    struct Entry {
                        tag: [u8; 4],
                        len: u32,
                        nested: Option<DeviceInnerHeader>,
                    }

                    let mut entries: Vec<Entry> = Vec::new();
                    loop {
                        let pos = file.stream_position().ok()?;
                        if pos >= data_end {
                            break;
                        }
                        if data_end - pos < 8 {
                            // Malformed container: omit details entirely.
                            return None;
                        }
                        let mut tag = [0u8; 4];
                        file.read_exact(&mut tag).ok()?;
                        let len = read_u32_le_lossy(file).ok()?;
                        let payload_start = file.stream_position().ok()?;
                        let payload_end = payload_start.checked_add(u64::from(len))?;
                        if payload_end > data_end {
                            return None;
                        }

                        let nested = if len == 0 {
                            None
                        } else {
                            let hdr_len = usize::try_from(u64::from(len).min(16)).unwrap_or(16);
                            let mut hdr = [0u8; 16];
                            if hdr_len != 0 && file.read_exact(&mut hdr[..hdr_len]).is_ok() {
                                parse_device_inner_header(&hdr[..hdr_len])
                            } else {
                                None
                            }
                        };
                        let _ = file.seek(SeekFrom::Start(payload_end));
                        entries.push(Entry { tag, len, nested });
                    }

                    entries.sort_by_key(|e| e.tag);

                    let mut entry_strs: Vec<String> = Vec::new();
                    for entry in entries.iter().take(MAX_LISTED) {
                        let mut s = format!("{} len={}", format_fourcc(entry.tag), entry.len);
                        if let Some(nested) = entry.nested.as_ref() {
                            match nested {
                                DeviceInnerHeader::IoSnapshot {
                                    device_id,
                                    device_version,
                                    ..
                                } => {
                                    let (major, minor) = *device_version;
                                    s.push_str(&format!(
                                        " nested={} v{}.{}",
                                        format_fourcc(*device_id),
                                        major,
                                        minor
                                    ));
                                }
                                DeviceInnerHeader::LegacyAero { version, flags } => {
                                    s.push_str(&format!(
                                        " nested=legacy-AERO v{version} flags={flags}"
                                    ));
                                }
                            }
                        }
                        entry_strs.push(s);
                    }

                    let mut out = format!(" AUSB v{version} flags={flags}");
                    if !entries.is_empty() {
                        let more = if entries.len() > MAX_LISTED {
                            format!(" ... ({} more)", entries.len() - MAX_LISTED)
                        } else {
                            String::new()
                        };
                        out.push_str(&format!(" entries=[{}]{}", entry_strs.join(", "), more));
                    }
                    Some(out)
                }

                if let Some(s) = decode_ausb_container_detail(file, data_start, data_end) {
                    detail = Some(s);
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

        // Serial output log (`DeviceId::SERIAL`).
        //
        // This is stored as raw bytes (not an `aero-io-snapshot` blob). Print a small escaped
        // prefix/tail preview to make it easier to sanity check snapshots in CI and debugging.
        if id == DeviceId::SERIAL.0 && version == 1 && flags == 0 {
            let prefix = escape_preview_bytes(&header[..header_len]);
            let mut s = format!(" serial_prefix=\"{prefix}\"");

            if len > header_len as u64 {
                let tail_len: usize = usize::try_from(len.min(16)).unwrap_or(16);
                if tail_len != 0 {
                    let tail_start = data_end.saturating_sub(tail_len as u64);
                    if tail_start > data_start {
                        let mut tail = vec![0u8; tail_len];
                        if file.seek(SeekFrom::Start(tail_start)).is_ok()
                            && file.read_exact(&mut tail).is_ok()
                        {
                            let tail = escape_preview_bytes(&tail);
                            s.push_str(&format!(" serial_tail=\"{tail}\""));
                        }
                    }
                }
            }

            detail = Some(s);
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

    // `CmosRtcSnapshot` (14 bytes).
    let rtc_year = read_u16_le_lossy(file).ok()?;
    let rtc_month = read_u8_lossy(file).ok()?;
    let rtc_day = read_u8_lossy(file).ok()?;
    let rtc_hour = read_u8_lossy(file).ok()?;
    let rtc_minute = read_u8_lossy(file).ok()?;
    let rtc_second = read_u8_lossy(file).ok()?;
    let rtc_nanosecond = read_u32_le_lossy(file).ok()?;
    let rtc_bcd_mode = read_u8_lossy(file).ok()? != 0;
    let rtc_hour_24 = read_u8_lossy(file).ok()? != 0;
    let rtc_daylight_savings = read_u8_lossy(file).ok()? != 0;

    // `BdaTimeSnapshot` (21 bytes).
    let bda_tick_count = read_u32_le_lossy(file).ok()?;
    let mut bda_tick_remainder = [0u8; 16];
    file.read_exact(&mut bda_tick_remainder).ok()?;
    let bda_tick_remainder = u128::from_le_bytes(bda_tick_remainder);
    let bda_midnight_flag = read_u8_lossy(file).ok()?;

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
    let mut vbe_lfb_base: Option<u32> = None;
    let mut cpu_count: Option<u8> = None;
    let mut enable_acpi: Option<bool> = None;
    let mut boot_order: Option<Vec<String>> = None;
    let mut cd_boot_drive: Option<u8> = None;
    let mut boot_from_cd_if_present: Option<bool> = None;

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
            // v4 extension: BIOS config video overrides.
            3 => {
                let present = match read_u8_lossy(file) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                vbe_lfb_base = if present != 0 {
                    read_u32_le_lossy(file).ok()
                } else {
                    None
                };
            }
            // v5 extension: BIOS boot order / CD boot policy.
            4 => {
                let len = match read_u8_lossy(file) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let mut out = Vec::with_capacity(len as usize);
                let mut truncated = false;
                for _ in 0..len {
                    let dev = match read_u8_lossy(file) {
                        Ok(v) => v,
                        Err(_) => {
                            truncated = true;
                            break;
                        }
                    };
                    out.push(match dev {
                        0 => "hdd".to_string(),
                        1 => "cdrom".to_string(),
                        2 => "floppy".to_string(),
                        other => format!("0x{other:02x}"),
                    });
                }
                if truncated {
                    break;
                }
                boot_order = Some(out);

                cd_boot_drive = match read_u8_lossy(file) {
                    Ok(v) => Some(v),
                    Err(_) => break,
                };
                boot_from_cd_if_present = match read_u8_lossy(file) {
                    Ok(v) => Some(v != 0),
                    Err(_) => break,
                };
            }
            _ => break,
        }
    }

    let mut s = format!(
        " boot_drive=0x{boot_drive:02x} mem_size_bytes={memory_size_bytes} rtc={rtc_year:04}-{rtc_month:02}-{rtc_day:02} {rtc_hour:02}:{rtc_minute:02}:{rtc_second:02}.{rtc_nanosecond:09} rtc_bcd={rtc_bcd_mode} rtc_hour_24={rtc_hour_24} rtc_dst={rtc_daylight_savings} bda_tick_count={bda_tick_count} bda_tick_remainder={bda_tick_remainder} bda_midnight_flag={bda_midnight_flag} video_mode=0x{video_mode:02x} tty_len={tty_len} e820_len={e820_len} keys_len={keys_len}"
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
    if let Some(base) = vbe_lfb_base {
        s.push_str(&format!(" vbe_lfb_base=0x{base:08x}"));
    }
    if let Some(count) = cpu_count {
        s.push_str(&format!(" cpu_count={count}"));
    }
    if let Some(enabled) = enable_acpi {
        s.push_str(&format!(" acpi={enabled}"));
    }
    if let Some(order) = boot_order {
        s.push_str(&format!(" boot_order=[{}]", order.join(",")));
    }
    if let Some(drive) = cd_boot_drive {
        s.push_str(&format!(" cd_boot_drive=0x{drive:02x}"));
    }
    if let Some(flag) = boot_from_cd_if_present {
        s.push_str(&format!(" boot_from_cd_if_present={flag}"));
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
        a20_enabled: Option<bool>,
        pending_bios_int: Option<u8>,
        irq13_pending: Option<bool>,
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
        let mut a20_enabled: Option<bool> = None;
        let mut pending_bios_int: Option<u8> = None;
        let mut irq13_pending: Option<bool> = None;
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
                        a20_enabled = Some(cpu.a20_enabled);
                        if cpu.pending_bios_int_valid {
                            pending_bios_int = Some(cpu.pending_bios_int);
                        }
                        irq13_pending = Some(cpu.irq13_pending);
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
            a20_enabled,
            pending_bios_int,
            irq13_pending,
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
        if let Some(a20) = entry.a20_enabled {
            suffix.push_str(&format!(" a20_enabled={a20}"));
        }
        if let Some(vector) = entry.pending_bios_int {
            suffix.push_str(&format!(" pending_bios_int=0x{vector:02x}"));
        }
        if let Some(irq13) = entry.irq13_pending {
            suffix.push_str(&format!(" irq13_pending={irq13}"));
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

fn print_ram_section_samples(file: &mut fs::File, section: &SnapshotSectionInfo) {
    const MAX_SAMPLES: u64 = 4;

    if section.len < 16 {
        println!("  <truncated ram section>");
        return;
    }

    let section_end = match section.offset.checked_add(section.len) {
        Some(v) => v,
        None => {
            println!("  <invalid ram section length>");
            return;
        }
    };

    if let Err(e) = file.seek(SeekFrom::Start(section.offset)) {
        println!("  <failed to seek: {e}>");
        return;
    }

    // Decode the RAM header (duplicated from `aero-snapshot`'s RAM format) so we can show a small
    // preview of the first few entries without decompressing.
    let _total_len = match read_u64_le_lossy(file) {
        Ok(v) => v,
        Err(e) => {
            println!("  <failed to read ram header: {e}>");
            return;
        }
    };
    let page_size = match read_u32_le_lossy(file) {
        Ok(v) => v,
        Err(e) => {
            println!("  <failed to read ram page_size: {e}>");
            return;
        }
    };
    let mode = match read_u8_lossy(file) {
        Ok(v) => v,
        Err(e) => {
            println!("  <failed to read ram mode: {e}>");
            return;
        }
    };
    let _compression = match read_u8_lossy(file) {
        Ok(v) => v,
        Err(e) => {
            println!("  <failed to read ram compression: {e}>");
            return;
        }
    };
    if read_u16_le_lossy(file).is_err() {
        println!("  <failed to read ram reserved field>");
        return;
    }

    match mode {
        0 => {
            // Full snapshot: u32 chunk_size + N * (u32 uncompressed_len + u32 compressed_len +
            // compressed payload).
            let chunk_size = match read_u32_le_lossy(file) {
                Ok(v) => v,
                Err(e) => {
                    println!("  <failed to read ram chunk_size: {e}>");
                    return;
                }
            };
            println!("  chunk_samples:");
            for chunk_idx in 0..MAX_SAMPLES {
                let pos = match file.stream_position() {
                    Ok(v) => v,
                    Err(_) => break,
                };
                if pos >= section_end {
                    break;
                }
                if section_end - pos < 8 {
                    println!("    <truncated chunk header>");
                    break;
                }
                let uncompressed_len = match read_u32_le_lossy(file) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let compressed_len = match read_u32_le_lossy(file) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let payload_start = match file.stream_position() {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let payload_end = match payload_start.checked_add(u64::from(compressed_len)) {
                    Some(v) => v,
                    None => {
                        println!("    <chunk length overflow>");
                        break;
                    }
                };
                if payload_end > section_end {
                    println!("    <truncated chunk payload>");
                    break;
                }
                let offset_bytes = u64::from(chunk_idx).saturating_mul(u64::from(chunk_size));
                println!(
                    "    - chunk[{chunk_idx}] offset=0x{offset_bytes:x} uncompressed_len={uncompressed_len} compressed_len={compressed_len}"
                );
                let _ = file.seek(SeekFrom::Start(payload_end));
            }
        }
        1 => {
            // Dirty snapshot: u64 dirty_count + N * (u64 page_idx + u32 uncompressed_len +
            // u32 compressed_len + payload).
            let dirty_count = match read_u64_le_lossy(file) {
                Ok(v) => v,
                Err(e) => {
                    println!("  <failed to read ram dirty_count: {e}>");
                    return;
                }
            };
            println!("  dirty_page_samples:");
            let sample_count = dirty_count.min(MAX_SAMPLES);
            for _ in 0..sample_count {
                let pos = match file.stream_position() {
                    Ok(v) => v,
                    Err(_) => break,
                };
                if pos >= section_end {
                    break;
                }
                if section_end - pos < 16 {
                    println!("    <truncated dirty page entry>");
                    break;
                }
                let page_idx = match read_u64_le_lossy(file) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let uncompressed_len = match read_u32_le_lossy(file) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let compressed_len = match read_u32_le_lossy(file) {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let payload_start = match file.stream_position() {
                    Ok(v) => v,
                    Err(_) => break,
                };
                let payload_end = match payload_start.checked_add(u64::from(compressed_len)) {
                    Some(v) => v,
                    None => {
                        println!("    <dirty entry length overflow>");
                        break;
                    }
                };
                if payload_end > section_end {
                    println!("    <truncated dirty page payload>");
                    break;
                }
                let offset_bytes = page_idx.saturating_mul(u64::from(page_size));
                println!(
                    "    - page_idx={page_idx} offset=0x{offset_bytes:x} uncompressed_len={uncompressed_len} compressed_len={compressed_len}"
                );
                let _ = file.seek(SeekFrom::Start(payload_end));
            }
        }
        other => {
            println!("  <unknown ram mode {other}>");
        }
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

fn cmd_diff(args: Vec<String>) -> Result<()> {
    let mut deep = false;
    let mut paths: Vec<String> = Vec::new();

    for arg in args {
        match arg.as_str() {
            "--deep" => deep = true,
            other if other.starts_with('-') => {
                return Err(XtaskError::Message(format!(
                    "unknown flag for `snapshot diff`: `{other}`"
                )));
            }
            other => paths.push(other.to_string()),
        }
    }

    let [path_a, path_b] = paths.as_slice() else {
        return Err(XtaskError::Message(
            "usage: cargo xtask snapshot diff <path_a> <path_b> [--deep]".to_string(),
        ));
    };

    let file_len_a = fs::metadata(path_a)
        .map_err(|e| XtaskError::Message(format!("stat {path_a:?}: {e}")))?
        .len();
    let file_len_b = fs::metadata(path_b)
        .map_err(|e| XtaskError::Message(format!("stat {path_b:?}: {e}")))?
        .len();

    let mut file_a =
        fs::File::open(path_a).map_err(|e| XtaskError::Message(format!("open {path_a:?}: {e}")))?;
    let mut file_b =
        fs::File::open(path_b).map_err(|e| XtaskError::Message(format!("open {path_b:?}: {e}")))?;

    let index_a = aero_snapshot::inspect_snapshot(&mut file_a)
        .map_err(|e| XtaskError::Message(format!("inspect snapshot A: {e}")))?;
    let index_b = aero_snapshot::inspect_snapshot(&mut file_b)
        .map_err(|e| XtaskError::Message(format!("inspect snapshot B: {e}")))?;

    println!("Snapshot diff:");
    println!("  A: {path_a} ({file_len_a} bytes)");
    println!("  B: {path_b} ({file_len_b} bytes)");

    let mut out = DiffOutput::default();

    // Header (file header fields returned by `inspect_snapshot`).
    if index_a.version != index_b.version {
        out.diff("header.version", index_a.version, index_b.version);
    }
    if index_a.endianness != index_b.endianness {
        out.diff(
            "header.endianness",
            fmt_endianness(index_a.endianness),
            fmt_endianness(index_b.endianness),
        );
    }

    // META fields.
    diff_meta(&mut out, index_a.meta.as_ref(), index_b.meta.as_ref());

    // Section list (id/version/flags/len).
    diff_section_table(&mut out, &index_a.sections, &index_b.sections);

    // Optional best-effort diffs for small sections (fast; no deep restore).
    diff_cpu_section(&mut out, &mut file_a, &index_a.sections, &mut file_b, &index_b.sections)?;
    diff_cpus_section(&mut out, &mut file_a, &index_a.sections, &mut file_b, &index_b.sections)?;
    diff_mmu_section(&mut out, &mut file_a, &index_a.sections, &mut file_b, &index_b.sections)?;
    diff_disks_section(&mut out, &mut file_a, &index_a.sections, &mut file_b, &index_b.sections)?;

    // DEVICES section (device ids/versions/blob lengths + hash).
    diff_devices_section(
        &mut out,
        &mut file_a,
        &index_a.sections,
        &mut file_b,
        &index_b.sections,
    )?;

    // RAM header summary (mode/compression/page_size/chunk_size/dirty_count).
    diff_ram_header(&mut out, index_a.ram.as_ref(), index_b.ram.as_ref());
    diff_ram_samples(&mut out, &mut file_a, &index_a, &mut file_b, &index_b)?;

    if deep {
        deep_diff_ram(&mut out, path_a, &index_a, path_b, &index_b)?;
    }

    if out.diffs == 0 {
        println!("No differences found.");
        Ok(())
    } else {
        println!("Found {} differing field(s).", out.diffs);
        Err(XtaskError::Message("snapshots differ".to_string()))
    }
}

#[derive(Debug, Default)]
struct DiffOutput {
    diffs: usize,
}

impl DiffOutput {
    fn diff(&mut self, key: &str, a: impl std::fmt::Display, b: impl std::fmt::Display) {
        self.diffs += 1;
        println!("diff {key}: A={a} B={b}");
    }

    fn diff_msg(&mut self, key: &str, msg: impl std::fmt::Display) {
        self.diffs += 1;
        println!("diff {key}: {msg}");
    }
}

fn diff_meta(
    out: &mut DiffOutput,
    a: Option<&aero_snapshot::SnapshotMeta>,
    b: Option<&aero_snapshot::SnapshotMeta>,
) {
    match (a, b) {
        (None, None) => {}
        (Some(_), None) => out.diff_msg("META", "A=present B=<missing>"),
        (None, Some(_)) => out.diff_msg("META", "A=<missing> B=present"),
        (Some(a), Some(b)) => {
            if a.snapshot_id != b.snapshot_id {
                out.diff("META.snapshot_id", a.snapshot_id, b.snapshot_id);
            }
            if a.parent_snapshot_id != b.parent_snapshot_id {
                out.diff(
                    "META.parent_snapshot_id",
                    fmt_opt_u64(a.parent_snapshot_id),
                    fmt_opt_u64(b.parent_snapshot_id),
                );
            }
            if a.created_unix_ms != b.created_unix_ms {
                out.diff("META.created_unix_ms", a.created_unix_ms, b.created_unix_ms);
            }
            if a.label.as_deref() != b.label.as_deref() {
                out.diff(
                    "META.label",
                    fmt_opt_str(a.label.as_deref()),
                    fmt_opt_str(b.label.as_deref()),
                );
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SectionDigest {
    id: SectionId,
    version: u16,
    flags: u16,
    len: u64,
}

impl std::fmt::Display for SectionDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} v{} flags={} len={}",
            self.id, self.version, self.flags, self.len
        )
    }
}

fn diff_section_table(out: &mut DiffOutput, a: &[SnapshotSectionInfo], b: &[SnapshotSectionInfo]) {
    let a: Vec<SectionDigest> = a
        .iter()
        .map(|s| SectionDigest {
            id: s.id,
            version: s.version,
            flags: s.flags,
            len: s.len,
        })
        .collect();
    let b: Vec<SectionDigest> = b
        .iter()
        .map(|s| SectionDigest {
            id: s.id,
            version: s.version,
            flags: s.flags,
            len: s.len,
        })
        .collect();

    if a == b {
        return;
    }

    out.diff_msg("sections", "section table differs");

    println!("Sections A:");
    for (idx, s) in a.iter().enumerate() {
        println!("  [{idx}] {s}");
    }
    println!("Sections B:");
    for (idx, s) in b.iter().enumerate() {
        println!("  [{idx}] {s}");
    }
}

fn diff_cpu_section(
    out: &mut DiffOutput,
    file_a: &mut fs::File,
    sections_a: &[SnapshotSectionInfo],
    file_b: &mut fs::File,
    sections_b: &[SnapshotSectionInfo],
) -> Result<()> {
    let sec_a = sections_a.iter().find(|s| s.id == SectionId::CPU);
    let sec_b = sections_b.iter().find(|s| s.id == SectionId::CPU);

    match (sec_a, sec_b) {
        (None, None) => Ok(()),
        (Some(_), None) => {
            out.diff_msg("CPU", "A=present B=<missing>");
            Ok(())
        }
        (None, Some(_)) => {
            out.diff_msg("CPU", "A=<missing> B=present");
            Ok(())
        }
        (Some(sec_a), Some(sec_b)) => {
            let cpu_a = read_cpu_state(file_a, sec_a, "A")?;
            let cpu_b = read_cpu_state(file_b, sec_b, "B")?;

            if cpu_a.mode != cpu_b.mode {
                out.diff("CPU.mode", format!("{:?}", cpu_a.mode), format!("{:?}", cpu_b.mode));
            }
            if cpu_a.halted != cpu_b.halted {
                out.diff("CPU.halted", cpu_a.halted, cpu_b.halted);
            }
            if cpu_a.rip != cpu_b.rip {
                out.diff(
                    "CPU.rip",
                    format!("0x{:x}", cpu_a.rip),
                    format!("0x{:x}", cpu_b.rip),
                );
            }
            if cpu_a.rflags != cpu_b.rflags {
                out.diff(
                    "CPU.rflags",
                    format!("0x{:x}", cpu_a.rflags),
                    format!("0x{:x}", cpu_b.rflags),
                );
            }
            if cpu_a.a20_enabled != cpu_b.a20_enabled {
                out.diff("CPU.a20_enabled", cpu_a.a20_enabled, cpu_b.a20_enabled);
            }
            if cpu_a.pending_bios_int_valid != cpu_b.pending_bios_int_valid {
                out.diff(
                    "CPU.pending_bios_int_valid",
                    cpu_a.pending_bios_int_valid,
                    cpu_b.pending_bios_int_valid,
                );
            }
            if cpu_a.pending_bios_int != cpu_b.pending_bios_int {
                // Only meaningful when `pending_bios_int_valid` is true, but diff the raw value for
                // debugging.
                out.diff(
                    "CPU.pending_bios_int",
                    format!("0x{:02x}", cpu_a.pending_bios_int),
                    format!("0x{:02x}", cpu_b.pending_bios_int),
                );
            }
            if cpu_a.irq13_pending != cpu_b.irq13_pending {
                out.diff("CPU.irq13_pending", cpu_a.irq13_pending, cpu_b.irq13_pending);
            }

            Ok(())
        }
    }
}

fn read_cpu_state(file: &mut fs::File, section: &SnapshotSectionInfo, tag: &str) -> Result<CpuState> {
    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek CPU {tag}: {e}")))?;
    let mut limited = file.take(section.len);
    let cpu = if section.version == 1 {
        CpuState::decode_v1(&mut limited)
    } else if section.version >= 2 {
        CpuState::decode_v2(&mut limited)
    } else {
        return Err(XtaskError::Message(format!(
            "CPU {tag}: unsupported section version {}",
            section.version
        )));
    };
    cpu.map_err(|e| XtaskError::Message(format!("CPU {tag}: decode failed: {e}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VcpuDigestSummary {
    rip: u64,
    halted: bool,
    internal_len: u64,
    internal_preview: Vec<u8>,
}

fn diff_cpus_section(
    out: &mut DiffOutput,
    file_a: &mut fs::File,
    sections_a: &[SnapshotSectionInfo],
    file_b: &mut fs::File,
    sections_b: &[SnapshotSectionInfo],
) -> Result<()> {
    let sec_a = sections_a.iter().find(|s| s.id == SectionId::CPUS);
    let sec_b = sections_b.iter().find(|s| s.id == SectionId::CPUS);

    match (sec_a, sec_b) {
        (None, None) => Ok(()),
        (Some(_), None) => {
            out.diff_msg("CPUS", "A=present B=<missing>");
            Ok(())
        }
        (None, Some(_)) => {
            out.diff_msg("CPUS", "A=<missing> B=present");
            Ok(())
        }
        (Some(sec_a), Some(sec_b)) => {
            if sec_a.version != sec_b.version {
                out.diff("CPUS.version", sec_a.version, sec_b.version);
                return Ok(());
            }
            if sec_a.version == 0 {
                out.diff_msg("CPUS", "unsupported CPUS section version 0");
                return Ok(());
            }

            let digests_a = read_cpus_digests(file_a, sec_a, "A")?;
            let digests_b = read_cpus_digests(file_b, sec_b, "B")?;

            if digests_a.len() != digests_b.len() {
                out.diff("CPUS.count", digests_a.len(), digests_b.len());
            }

            let mut map_a: BTreeMap<u32, VcpuDigestSummary> = BTreeMap::new();
            let mut map_b: BTreeMap<u32, VcpuDigestSummary> = BTreeMap::new();

            for (apic_id, v) in digests_a {
                if map_a.insert(apic_id, v).is_some() {
                    return Err(XtaskError::Message(format!(
                        "CPUS A: duplicate apic_id {apic_id}"
                    )));
                }
            }
            for (apic_id, v) in digests_b {
                if map_b.insert(apic_id, v).is_some() {
                    return Err(XtaskError::Message(format!(
                        "CPUS B: duplicate apic_id {apic_id}"
                    )));
                }
            }

            let all_keys: BTreeMap<u32, ()> =
                map_a.keys().chain(map_b.keys()).map(|&k| (k, ())).collect();

            for (apic_id, ()) in all_keys {
                match (map_a.get(&apic_id), map_b.get(&apic_id)) {
                    (Some(a), Some(b)) => {
                        if a.rip != b.rip {
                            out.diff(
                                &format!("CPUS[apic_id={apic_id}].rip"),
                                format!("0x{:x}", a.rip),
                                format!("0x{:x}", b.rip),
                            );
                        }
                        if a.halted != b.halted {
                            out.diff(
                                &format!("CPUS[apic_id={apic_id}].halted"),
                                a.halted,
                                b.halted,
                            );
                        }
                        if a.internal_len != b.internal_len {
                            out.diff(
                                &format!("CPUS[apic_id={apic_id}].internal_len"),
                                a.internal_len,
                                b.internal_len,
                            );
                        }
                        if a.internal_preview != b.internal_preview {
                            out.diff(
                                &format!("CPUS[apic_id={apic_id}].internal_preview"),
                                fmt_preview_bytes(&a.internal_preview),
                                fmt_preview_bytes(&b.internal_preview),
                            );
                        }
                    }
                    (Some(_), None) => out.diff_msg(
                        &format!("CPUS[apic_id={apic_id}]"),
                        "present in A, missing in B",
                    ),
                    (None, Some(_)) => out.diff_msg(
                        &format!("CPUS[apic_id={apic_id}]"),
                        "missing in A, present in B",
                    ),
                    (None, None) => {}
                }
            }

            Ok(())
        }
    }
}

fn read_cpus_digests(
    file: &mut fs::File,
    section: &SnapshotSectionInfo,
    tag: &str,
) -> Result<Vec<(u32, VcpuDigestSummary)>> {
    let section_end = section
        .offset
        .checked_add(section.len)
        .ok_or_else(|| XtaskError::Message("section length overflow".to_string()))?;
    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek CPUS {tag}: {e}")))?;

    ensure_section_remaining(file, section_end, 4, "cpu count")?;
    let count = read_u32_le(file)?;
    if count > limits::MAX_CPU_COUNT {
        return Err(XtaskError::Message(format!(
            "CPUS {tag}: too many CPUs ({count})"
        )));
    }

    let mut out = Vec::with_capacity(count as usize);
    for idx in 0..count {
        ensure_section_remaining(file, section_end, 8, "cpu entry_len")?;
        let entry_len = read_u64_le(file)?;
        let entry_start = file
            .stream_position()
            .map_err(|e| XtaskError::Message(format!("tell CPUS {tag} entry {idx}: {e}")))?;
        let entry_end = entry_start
            .checked_add(entry_len)
            .ok_or_else(|| XtaskError::Message("cpu entry length overflow".to_string()))?;
        if entry_end > section_end {
            return Err(XtaskError::Message(format!("CPUS {tag}: truncated section")));
        }

        let mut entry_reader = file.take(entry_len);
        let apic_id = read_u32_le(&mut entry_reader)
            .map_err(|e| XtaskError::Message(format!("CPUS {tag}: apic_id: {e}")))?;

        let cpu = if section.version == 1 {
            CpuState::decode_v1(&mut entry_reader)
        } else {
            CpuState::decode_v2(&mut entry_reader)
        }
        .map_err(|e| XtaskError::Message(format!("CPUS {tag} apic_id={apic_id}: cpu: {e}")))?;

        let internal_len = read_u64_le(&mut entry_reader).map_err(|e| {
            XtaskError::Message(format!("CPUS {tag} apic_id={apic_id}: internal_len: {e}"))
        })?;

        // Preview a few bytes without reading the full internal blob.
        let preview_len = (internal_len as usize).min(8);
        let mut preview = vec![0u8; preview_len];
        if preview_len != 0 {
            entry_reader.read_exact(&mut preview).map_err(|e| {
                XtaskError::Message(format!(
                    "CPUS {tag} apic_id={apic_id}: internal_preview: {e}"
                ))
            })?;
        }

        out.push((
            apic_id,
            VcpuDigestSummary {
                rip: cpu.rip,
                halted: cpu.halted,
                internal_len,
                internal_preview: preview,
            },
        ));

        // Skip any remaining bytes in this entry.
        file.seek(SeekFrom::Start(entry_end))
            .map_err(|e| XtaskError::Message(format!("skip CPUS {tag} entry: {e}")))?;
    }

    Ok(out)
}

fn fmt_preview_bytes(bytes: &[u8]) -> String {
    let mut out = String::new();
    out.push('[');
    for (idx, b) in bytes.iter().copied().enumerate() {
        if idx != 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("0x{b:02x}"));
    }
    out.push(']');
    out
}

fn diff_mmu_section(
    out: &mut DiffOutput,
    file_a: &mut fs::File,
    sections_a: &[SnapshotSectionInfo],
    file_b: &mut fs::File,
    sections_b: &[SnapshotSectionInfo],
) -> Result<()> {
    let sec_a = sections_a.iter().find(|s| s.id == SectionId::MMU);
    let sec_b = sections_b.iter().find(|s| s.id == SectionId::MMU);

    match (sec_a, sec_b) {
        (None, None) => Ok(()),
        (Some(_), None) => {
            out.diff_msg("MMU", "A=present B=<missing>");
            Ok(())
        }
        (None, Some(_)) => {
            out.diff_msg("MMU", "A=<missing> B=present");
            Ok(())
        }
        (Some(sec_a), Some(sec_b)) => {
            let mmu_a = read_mmu_state(file_a, sec_a, "A")?;
            let mmu_b = read_mmu_state(file_b, sec_b, "B")?;

            macro_rules! diff_hex_u64 {
                ($field:literal, $a:expr, $b:expr) => {
                    if $a != $b {
                        out.diff(
                            concat!("MMU.", $field),
                            format!("0x{:x}", $a),
                            format!("0x{:x}", $b),
                        );
                    }
                };
            }

            diff_hex_u64!("cr0", mmu_a.cr0, mmu_b.cr0);
            diff_hex_u64!("cr3", mmu_a.cr3, mmu_b.cr3);
            diff_hex_u64!("cr4", mmu_a.cr4, mmu_b.cr4);
            diff_hex_u64!("efer", mmu_a.efer, mmu_b.efer);
            diff_hex_u64!("apic_base", mmu_a.apic_base, mmu_b.apic_base);
            diff_hex_u64!("tsc", mmu_a.tsc, mmu_b.tsc);
            diff_hex_u64!("gdtr_base", mmu_a.gdtr_base, mmu_b.gdtr_base);
            if mmu_a.gdtr_limit != mmu_b.gdtr_limit {
                out.diff("MMU.gdtr_limit", mmu_a.gdtr_limit, mmu_b.gdtr_limit);
            }
            diff_hex_u64!("idtr_base", mmu_a.idtr_base, mmu_b.idtr_base);
            if mmu_a.idtr_limit != mmu_b.idtr_limit {
                out.diff("MMU.idtr_limit", mmu_a.idtr_limit, mmu_b.idtr_limit);
            }

            Ok(())
        }
    }
}

fn read_mmu_state(file: &mut fs::File, section: &SnapshotSectionInfo, tag: &str) -> Result<MmuState> {
    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek MMU {tag}: {e}")))?;
    let mut limited = file.take(section.len);
    let mmu = if section.version == 1 {
        MmuState::decode_v1(&mut limited)
    } else if section.version >= 2 {
        MmuState::decode_v2(&mut limited)
    } else {
        return Err(XtaskError::Message(format!(
            "MMU {tag}: unsupported section version {}",
            section.version
        )));
    };
    mmu.map_err(|e| XtaskError::Message(format!("MMU {tag}: decode failed: {e}")))
}

fn diff_disks_section(
    out: &mut DiffOutput,
    file_a: &mut fs::File,
    sections_a: &[SnapshotSectionInfo],
    file_b: &mut fs::File,
    sections_b: &[SnapshotSectionInfo],
) -> Result<()> {
    let sec_a = sections_a.iter().find(|s| s.id == SectionId::DISKS);
    let sec_b = sections_b.iter().find(|s| s.id == SectionId::DISKS);

    match (sec_a, sec_b) {
        (None, None) => return Ok(()),
        (Some(_), None) => {
            out.diff_msg("DISKS", "A=present B=<missing>");
            return Ok(());
        }
        (None, Some(_)) => {
            out.diff_msg("DISKS", "A=<missing> B=present");
            return Ok(());
        }
        (Some(sec_a), Some(sec_b)) => {
            if sec_a.version != sec_b.version {
                out.diff("DISKS.version", sec_a.version, sec_b.version);
                return Ok(());
            }
            if sec_a.version != 1 {
                out.diff_msg(
                    "DISKS",
                    format!("unsupported DISKS section version {}", sec_a.version),
                );
                return Ok(());
            }

            let disks_a = read_disks_refs(file_a, sec_a, "A")?;
            let disks_b = read_disks_refs(file_b, sec_b, "B")?;

            if disks_a.disks.len() != disks_b.disks.len() {
                out.diff("DISKS.count", disks_a.disks.len(), disks_b.disks.len());
            }

            let order_a: Vec<u32> = disks_a.disks.iter().map(|d| d.disk_id).collect();
            let order_b: Vec<u32> = disks_b.disks.iter().map(|d| d.disk_id).collect();

            let mut map_a: BTreeMap<u32, &aero_snapshot::DiskOverlayRef> = BTreeMap::new();
            let mut map_b: BTreeMap<u32, &aero_snapshot::DiskOverlayRef> = BTreeMap::new();
            for d in &disks_a.disks {
                map_a.insert(d.disk_id, d);
            }
            for d in &disks_b.disks {
                map_b.insert(d.disk_id, d);
            }

            if order_a != order_b && map_a.keys().copied().collect::<Vec<_>>() == map_b.keys().copied().collect::<Vec<_>>() {
                out.diff_msg("DISKS.order", "same entries, different on-disk ordering");
            }

            let all_keys: BTreeMap<u32, ()> =
                map_a.keys().chain(map_b.keys()).map(|&k| (k, ())).collect();

            for (disk_id, ()) in all_keys {
                match (map_a.get(&disk_id), map_b.get(&disk_id)) {
                    (Some(a), Some(b)) => {
                        if a.base_image != b.base_image {
                            out.diff(
                                &format!("DISKS[disk_id={disk_id}].base_image"),
                                fmt_disk_ref(&a.base_image),
                                fmt_disk_ref(&b.base_image),
                            );
                        }
                        if a.overlay_image != b.overlay_image {
                            out.diff(
                                &format!("DISKS[disk_id={disk_id}].overlay_image"),
                                fmt_disk_ref(&a.overlay_image),
                                fmt_disk_ref(&b.overlay_image),
                            );
                        }
                    }
                    (Some(_), None) => out.diff_msg(
                        &format!("DISKS[disk_id={disk_id}]"),
                        "present in A, missing in B",
                    ),
                    (None, Some(_)) => out.diff_msg(
                        &format!("DISKS[disk_id={disk_id}]"),
                        "missing in A, present in B",
                    ),
                    (None, None) => {}
                }
            }

            Ok(())
        }
    }
}

fn read_disks_refs(
    file: &mut fs::File,
    section: &SnapshotSectionInfo,
    tag: &str,
) -> Result<DiskOverlayRefs> {
    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek DISKS {tag}: {e}")))?;
    let mut limited = file.take(section.len);
    DiskOverlayRefs::decode(&mut limited)
        .map_err(|e| XtaskError::Message(format!("DISKS {tag}: decode failed: {e}")))
}

fn fmt_disk_ref(path: &str) -> String {
    const MAX_CHARS: usize = 200;
    if path.is_empty() {
        return "<unset>".to_string();
    }
    let truncated: String = path.chars().take(MAX_CHARS + 1).collect();
    if truncated.chars().count() <= MAX_CHARS {
        return format!("{path:?}");
    }
    let mut s: String = path.chars().take(MAX_CHARS).collect();
    s.push('…');
    format!("{s:?}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeviceDigest {
    id: DeviceId,
    version: u16,
    flags: u16,
    len: u64,
    hash: u64,
}

impl DeviceDigest {
    fn key(&self) -> (u32, u16, u16) {
        (self.id.0, self.version, self.flags)
    }
}

fn diff_devices_section(
    out: &mut DiffOutput,
    file_a: &mut fs::File,
    sections_a: &[SnapshotSectionInfo],
    file_b: &mut fs::File,
    sections_b: &[SnapshotSectionInfo],
) -> Result<()> {
    let sec_a = sections_a.iter().find(|s| s.id == SectionId::DEVICES);
    let sec_b = sections_b.iter().find(|s| s.id == SectionId::DEVICES);

    match (sec_a, sec_b) {
        (None, None) => return Ok(()),
        (Some(_), None) => {
            out.diff_msg("DEVICES", "A=present B=<missing>");
            return Ok(());
        }
        (None, Some(_)) => {
            out.diff_msg("DEVICES", "A=<missing> B=present");
            return Ok(());
        }
        (Some(sec_a), Some(sec_b)) => {
            if sec_a.version != sec_b.version {
                out.diff("DEVICES.version", sec_a.version, sec_b.version);
                // If versions differ, the payload format might differ; don't attempt to decode.
                return Ok(());
            }
            if sec_a.version != 1 {
                out.diff_msg(
                    "DEVICES",
                    format!("unsupported DEVICES section version {}", sec_a.version),
                );
                return Ok(());
            }

            let devs_a = read_devices_digests(file_a, sec_a, "A")?;
            let devs_b = read_devices_digests(file_b, sec_b, "B")?;

            // Compare by key in sorted order so output is stable and doesn't cascade on missing
            // entries.
            let mut map_a: BTreeMap<(u32, u16, u16), DeviceDigest> = BTreeMap::new();
            let mut map_b: BTreeMap<(u32, u16, u16), DeviceDigest> = BTreeMap::new();
            for d in devs_a.iter().copied() {
                map_a.insert(d.key(), d);
            }
            for d in devs_b.iter().copied() {
                map_b.insert(d.key(), d);
            }

            // Detect ordering differences separately (the file-order list is often important when
            // debugging determinism).
            let order_a: Vec<(u32, u16, u16)> = devs_a.iter().map(|d| d.key()).collect();
            let order_b: Vec<(u32, u16, u16)> = devs_b.iter().map(|d| d.key()).collect();
            if order_a != order_b && map_a == map_b {
                out.diff_msg("DEVICES.order", "same entries, different on-disk ordering");
            }

            // Compare entries.
            let all_keys: BTreeMap<(u32, u16, u16), ()> =
                map_a.keys().chain(map_b.keys()).map(|&k| (k, ())).collect();

            for (key, ()) in all_keys {
                match (map_a.get(&key), map_b.get(&key)) {
                    (Some(a), Some(b)) => {
                        if a.len != b.len {
                            out.diff(
                                &format!(
                                    "DEVICES[{} v{} flags={}].len",
                                    DeviceId(key.0),
                                    key.1,
                                    key.2
                                ),
                                a.len,
                                b.len,
                            );
                        }
                        if a.hash != b.hash {
                            out.diff(
                                &format!(
                                    "DEVICES[{} v{} flags={}].hash",
                                    DeviceId(key.0),
                                    key.1,
                                    key.2
                                ),
                                format!("0x{:016x}", a.hash),
                                format!("0x{:016x}", b.hash),
                            );
                        }
                    }
                    (Some(_), None) => {
                        out.diff_msg(
                            &format!("DEVICES[{} v{} flags={}]", DeviceId(key.0), key.1, key.2),
                            "present in A, missing in B",
                        );
                    }
                    (None, Some(_)) => {
                        out.diff_msg(
                            &format!("DEVICES[{} v{} flags={}]", DeviceId(key.0), key.1, key.2),
                            "missing in A, present in B",
                        );
                    }
                    (None, None) => {}
                }
            }

            Ok(())
        }
    }
}

fn read_devices_digests(
    file: &mut fs::File,
    section: &SnapshotSectionInfo,
    tag: &str,
) -> Result<Vec<DeviceDigest>> {
    let section_end = section
        .offset
        .checked_add(section.len)
        .ok_or_else(|| XtaskError::Message("section length overflow".to_string()))?;
    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek DEVICES {tag}: {e}")))?;

    ensure_section_remaining(file, section_end, 4, "device count")?;
    let count = read_u32_le(file)?;
    if count > limits::MAX_DEVICE_COUNT {
        return Err(XtaskError::Message(format!(
            "DEVICES {tag}: too many devices ({count})"
        )));
    }

    let mut out = Vec::with_capacity(count as usize);
    for _ in 0..count {
        ensure_section_remaining(file, section_end, 16, "device entry header")?;
        let id = DeviceId(read_u32_le(file)?);
        let version = read_u16_le(file)?;
        let flags = read_u16_le(file)?;
        let len = read_u64_le(file)?;
        if len > limits::MAX_DEVICE_ENTRY_LEN {
            return Err(XtaskError::Message(format!(
                "DEVICES {tag}: device entry too large ({len} bytes)"
            )));
        }
        ensure_section_remaining(file, section_end, len, "device entry data")?;
        let hash = fnv1a64_hash_reader(file, len).map_err(|e| {
            XtaskError::Message(format!("DEVICES {tag}: failed to hash device blob: {e}"))
        })?;
        out.push(DeviceDigest {
            id,
            version,
            flags,
            len,
            hash,
        });
    }

    Ok(out)
}

fn diff_ram_header(
    out: &mut DiffOutput,
    a: Option<&aero_snapshot::RamHeaderSummary>,
    b: Option<&aero_snapshot::RamHeaderSummary>,
) {
    match (a, b) {
        (None, None) => {}
        (Some(_), None) => out.diff_msg("RAM", "A=present B=<missing>"),
        (None, Some(_)) => out.diff_msg("RAM", "A=<missing> B=present"),
        (Some(a), Some(b)) => {
            if a.total_len != b.total_len {
                out.diff("RAM.total_len", a.total_len, b.total_len);
            }
            if a.page_size != b.page_size {
                out.diff("RAM.page_size", a.page_size, b.page_size);
            }
            if a.mode != b.mode {
                out.diff("RAM.mode", fmt_ram_mode(a.mode), fmt_ram_mode(b.mode));
            }
            if a.compression != b.compression {
                out.diff(
                    "RAM.compression",
                    fmt_compression(a.compression),
                    fmt_compression(b.compression),
                );
            }
            if a.chunk_size != b.chunk_size {
                out.diff(
                    "RAM.chunk_size",
                    fmt_opt_u32(a.chunk_size),
                    fmt_opt_u32(b.chunk_size),
                );
            }
            if a.dirty_count != b.dirty_count {
                out.diff(
                    "RAM.dirty_count",
                    fmt_opt_u64(a.dirty_count),
                    fmt_opt_u64(b.dirty_count),
                );
            }
        }
    }
}

fn diff_ram_samples(
    out: &mut DiffOutput,
    file_a: &mut fs::File,
    index_a: &SnapshotIndex,
    file_b: &mut fs::File,
    index_b: &SnapshotIndex,
) -> Result<()> {
    const MAX_SAMPLES: usize = 3;

    let Some(sec_a) = index_a.sections.iter().find(|s| s.id == SectionId::RAM) else {
        return Ok(());
    };
    let Some(sec_b) = index_b.sections.iter().find(|s| s.id == SectionId::RAM) else {
        return Ok(());
    };

    // Only sample when both snapshots have readable RAM headers and use the same mode. Header field
    // diffs are already reported by `diff_ram_header`; this is just extra context.
    let (Some(ram_a), Some(ram_b)) = (index_a.ram.as_ref(), index_b.ram.as_ref()) else {
        return Ok(());
    };
    if ram_a.mode != ram_b.mode {
        return Ok(());
    }
    if ram_a.mode == RamMode::Full && ram_a.chunk_size != ram_b.chunk_size {
        return Ok(());
    }
    if ram_a.mode == RamMode::Dirty && ram_a.page_size != ram_b.page_size {
        return Ok(());
    }

    let samp_a = read_ram_samples(file_a, sec_a, MAX_SAMPLES, "A")?;
    let samp_b = read_ram_samples(file_b, sec_b, MAX_SAMPLES, "B")?;

    match (samp_a, samp_b) {
        (RamSamples::Full(a), RamSamples::Full(b)) => {
            for idx in 0..MAX_SAMPLES.min(a.chunks.len()).min(b.chunks.len()) {
                let (ua, ca) = a.chunks[idx];
                let (ub, cb) = b.chunks[idx];
                if ua != ub {
                    out.diff(
                        &format!("RAM.sample.chunk[{idx}].uncompressed_len"),
                        ua,
                        ub,
                    );
                }
                if ca != cb {
                    out.diff(
                        &format!("RAM.sample.chunk[{idx}].compressed_len"),
                        ca,
                        cb,
                    );
                }
            }
        }
        (RamSamples::Dirty(a), RamSamples::Dirty(b)) => {
            for idx in 0..MAX_SAMPLES.min(a.pages.len()).min(b.pages.len()) {
                let (pia, ua, ca) = a.pages[idx];
                let (pib, ub, cb) = b.pages[idx];
                if pia != pib {
                    out.diff(&format!("RAM.sample.page[{idx}].page_idx"), pia, pib);
                }
                if ua != ub {
                    out.diff(
                        &format!("RAM.sample.page[{idx}].uncompressed_len"),
                        ua,
                        ub,
                    );
                }
                if ca != cb {
                    out.diff(
                        &format!("RAM.sample.page[{idx}].compressed_len"),
                        ca,
                        cb,
                    );
                }
            }
        }
        // Mode differs or decoding mismatch; header already reports mode differences.
        _ => {}
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct FullRamSamples {
    chunks: Vec<(u32, u32)>,
}

#[derive(Debug, Clone)]
struct DirtyRamSamples {
    pages: Vec<(u64, u32, u32)>,
}

#[derive(Debug, Clone)]
enum RamSamples {
    Full(FullRamSamples),
    Dirty(DirtyRamSamples),
}

fn read_ram_samples(
    file: &mut fs::File,
    section: &SnapshotSectionInfo,
    max_samples: usize,
    tag: &str,
) -> Result<RamSamples> {
    let section_end = section
        .offset
        .checked_add(section.len)
        .ok_or_else(|| XtaskError::Message("section length overflow".to_string()))?;
    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek RAM {tag}: {e}")))?;

    ensure_section_remaining(file, section_end, 8 + 4 + 1 + 1 + 2, "RAM header")?;
    let _total_len = read_u64_le(file)?;
    let page_size = read_u32_le(file)?;
    let mode = read_u8(file)?;
    let _compression = read_u8(file)?;
    let _reserved = read_u16_le(file)?;

    match mode {
        0 => {
            // Full snapshot (chunked).
            ensure_section_remaining(file, section_end, 4, "RAM chunk_size")?;
            let chunk_size = read_u32_le(file)?;
            if chunk_size == 0 {
                return Err(XtaskError::Message(format!("RAM {tag}: invalid chunk_size")));
            }

            let mut chunks: Vec<(u32, u32)> = Vec::new();
            for _ in 0..max_samples {
                // Each entry is: u32 uncompressed_len + u32 compressed_len + payload[compressed_len].
                if file.stream_position().map_err(|e| {
                    XtaskError::Message(format!("tell RAM {tag} chunk: {e}"))
                })? >= section_end
                {
                    break;
                }
                ensure_section_remaining(file, section_end, 8, "RAM chunk header")?;
                let uncompressed_len = read_u32_le(file)?;
                let compressed_len = read_u32_le(file)?;
                chunks.push((uncompressed_len, compressed_len));

                let payload_start = file
                    .stream_position()
                    .map_err(|e| XtaskError::Message(format!("tell RAM {tag} chunk: {e}")))?;
                let payload_end = payload_start
                    .checked_add(u64::from(compressed_len))
                    .ok_or_else(|| XtaskError::Message("RAM chunk length overflow".to_string()))?;
                if payload_end > section_end {
                    return Err(XtaskError::Message(format!("RAM {tag}: truncated chunk payload")));
                }
                file.seek(SeekFrom::Start(payload_end))
                    .map_err(|e| XtaskError::Message(format!("skip RAM {tag} chunk: {e}")))?;
            }
            Ok(RamSamples::Full(FullRamSamples { chunks }))
        }
        1 => {
            // Dirty snapshot: u64 dirty_count + repeated (u64 page_idx + u32 uncompressed_len +
            // u32 compressed_len + payload).
            ensure_section_remaining(file, section_end, 8, "RAM dirty_count")?;
            let dirty_count = read_u64_le(file)?;
            let sample_count = (dirty_count as usize).min(max_samples);
            let mut pages: Vec<(u64, u32, u32)> = Vec::new();
            for _ in 0..sample_count {
                ensure_section_remaining(file, section_end, 8 + 4 + 4, "RAM dirty entry header")?;
                let page_idx = read_u64_le(file)?;
                let uncompressed_len = read_u32_le(file)?;
                let compressed_len = read_u32_le(file)?;
                pages.push((page_idx, uncompressed_len, compressed_len));

                let payload_start = file
                    .stream_position()
                    .map_err(|e| XtaskError::Message(format!("tell RAM {tag} page: {e}")))?;
                let payload_end = payload_start
                    .checked_add(u64::from(compressed_len))
                    .ok_or_else(|| XtaskError::Message("RAM dirty length overflow".to_string()))?;
                if payload_end > section_end {
                    return Err(XtaskError::Message(format!("RAM {tag}: truncated dirty payload")));
                }
                file.seek(SeekFrom::Start(payload_end))
                    .map_err(|e| XtaskError::Message(format!("skip RAM {tag} page: {e}")))?;
            }
            let _ = page_size; // currently unused; `inspect_snapshot` validates it.
            Ok(RamSamples::Dirty(DirtyRamSamples { pages }))
        }
        other => Err(XtaskError::Message(format!("RAM {tag}: unknown mode {other}"))),
    }
}

fn read_u8(r: &mut impl Read) -> Result<u8> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf)
        .map_err(|e| XtaskError::Message(format!("read u8: {e}")))?;
    Ok(buf[0])
}

fn deep_diff_ram(
    out: &mut DiffOutput,
    path_a: &str,
    index_a: &SnapshotIndex,
    path_b: &str,
    index_b: &SnapshotIndex,
) -> Result<()> {
    const MAX_DEEP_RAM_BYTES: u64 = 256 * 1024 * 1024;

    let Some(ram_a) = index_a.ram else {
        return Err(XtaskError::Message(
            "--deep requires a readable RAM header in snapshot A".to_string(),
        ));
    };
    let Some(ram_b) = index_b.ram else {
        return Err(XtaskError::Message(
            "--deep requires a readable RAM header in snapshot B".to_string(),
        ));
    };

    if ram_a.total_len > MAX_DEEP_RAM_BYTES {
        return Err(XtaskError::Message(format!(
            "--deep refuses to restore snapshots with RAM > {MAX_DEEP_RAM_BYTES} bytes (snapshot A has {})",
            ram_a.total_len
        )));
    }
    if ram_b.total_len > MAX_DEEP_RAM_BYTES {
        return Err(XtaskError::Message(format!(
            "--deep refuses to restore snapshots with RAM > {MAX_DEEP_RAM_BYTES} bytes (snapshot B has {})",
            ram_b.total_len
        )));
    }

    if ram_a.total_len != ram_b.total_len || ram_a.page_size != ram_b.page_size {
        out.diff_msg(
            "RAM.deep",
            "skipped page hash comparison due to mismatched total_len/page_size",
        );
        return Ok(());
    }

    let ram_len: usize = ram_a
        .total_len
        .try_into()
        .map_err(|_| XtaskError::Message("snapshot RAM size does not fit in usize".to_string()))?;
    let page_size: usize = ram_a
        .page_size
        .try_into()
        .map_err(|_| XtaskError::Message("snapshot page_size does not fit in usize".to_string()))?;
    if page_size == 0 {
        return Err(XtaskError::Message("invalid RAM page_size".to_string()));
    }

    let mut ram_bytes_a = vec![0u8; ram_len];
    let mut ram_bytes_b = vec![0u8; ram_len];

    {
        let mut file_a = fs::File::open(path_a)
            .map_err(|e| XtaskError::Message(format!("open {path_a:?}: {e}")))?;
        let mut target = RamCaptureTarget::new(&mut ram_bytes_a);
        aero_snapshot::restore_snapshot(&mut file_a, &mut target)
            .map_err(|e| XtaskError::Message(format!("restore snapshot A: {e}")))?;
    }
    {
        let mut file_b = fs::File::open(path_b)
            .map_err(|e| XtaskError::Message(format!("open {path_b:?}: {e}")))?;
        let mut target = RamCaptureTarget::new(&mut ram_bytes_b);
        aero_snapshot::restore_snapshot(&mut file_b, &mut target)
            .map_err(|e| XtaskError::Message(format!("restore snapshot B: {e}")))?;
    }

    // Compare page digests.
    let page_count = ram_len.div_ceil(page_size);
    let mut diff_pages: u64 = 0;
    const MAX_PAGE_DIFF_PRINT: usize = 32;
    let mut printed = 0usize;

    for page_idx in 0..page_count {
        let start = page_idx * page_size;
        let end = (start + page_size).min(ram_len);
        let ha = fnv1a64_hash_bytes(&ram_bytes_a[start..end]);
        let hb = fnv1a64_hash_bytes(&ram_bytes_b[start..end]);
        if ha != hb {
            diff_pages += 1;
            if printed < MAX_PAGE_DIFF_PRINT {
                println!("diff RAM.page[{page_idx}]: A=0x{ha:016x} B=0x{hb:016x}");
                printed += 1;
            }
        }
    }

    if diff_pages != 0 {
        out.diff_msg(
            "RAM.deep.pages",
            format!("{diff_pages} / {page_count} pages differ (showing first {printed})"),
        );
    }

    Ok(())
}

struct RamCaptureTarget<'a> {
    ram: &'a mut [u8],
}

impl<'a> RamCaptureTarget<'a> {
    fn new(ram: &'a mut [u8]) -> Self {
        Self { ram }
    }
}

impl SnapshotTarget for RamCaptureTarget<'_> {
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
        self.ram.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> aero_snapshot::Result<()> {
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

const FNV1A_64_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
const FNV1A_64_PRIME: u64 = 0x100000001b3;

fn fnv1a64_hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = FNV1A_64_OFFSET_BASIS;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV1A_64_PRIME);
    }
    hash
}

fn fnv1a64_hash_reader(r: &mut impl Read, len: u64) -> std::io::Result<u64> {
    let mut remaining = len;
    let mut hash = FNV1A_64_OFFSET_BASIS;
    let mut buf = [0u8; 16 * 1024];

    while remaining != 0 {
        let chunk_len: usize = (remaining.min(buf.len() as u64)) as usize;
        r.read_exact(&mut buf[..chunk_len])?;
        for &b in &buf[..chunk_len] {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(FNV1A_64_PRIME);
        }
        remaining -= chunk_len as u64;
    }

    Ok(hash)
}

fn fmt_endianness(tag: u8) -> String {
    match tag {
        aero_snapshot::SNAPSHOT_ENDIANNESS_LITTLE => "little".to_string(),
        other => format!("unknown({other})"),
    }
}

fn fmt_opt_u64(v: Option<u64>) -> String {
    match v {
        Some(v) => v.to_string(),
        None => "<none>".to_string(),
    }
}

fn fmt_opt_u32(v: Option<u32>) -> String {
    match v {
        Some(v) => v.to_string(),
        None => "<none>".to_string(),
    }
}

fn fmt_opt_str(v: Option<&str>) -> String {
    match v {
        Some(v) => format!("{v:?}"),
        None => "<none>".to_string(),
    }
}

fn fmt_ram_mode(mode: RamMode) -> &'static str {
    match mode {
        RamMode::Full => "full",
        RamMode::Dirty => "dirty",
    }
}

fn fmt_compression(c: Compression) -> &'static str {
    match c {
        Compression::None => "none",
        Compression::Lz4 => "lz4",
    }
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
        .filter(|s| s.id == SectionId::MMUS)
        .count()
        > 1
    {
        return Err(XtaskError::Message("duplicate MMUS section".to_string()));
    }
    let has_mmu = index.sections.iter().any(|s| s.id == SectionId::MMU);
    let has_mmus = index.sections.iter().any(|s| s.id == SectionId::MMUS);
    if has_mmu && has_mmus {
        return Err(XtaskError::Message(
            "snapshot contains both MMU and MMUS".to_string(),
        ));
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
            id if id == SectionId::MMUS => validate_mmus_section(&mut file, section)?,
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
    fn restore_mmu_states(&mut self, states: Vec<VcpuMmuSnapshot>) -> aero_snapshot::Result<()> {
        if states.is_empty() {
            return Err(SnapshotError::Corrupt("missing MMU entry"));
        }
        Ok(())
    }

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

fn validate_mmus_section(file: &mut fs::File, section: &SnapshotSectionInfo) -> Result<()> {
    file.seek(SeekFrom::Start(section.offset))
        .map_err(|e| XtaskError::Message(format!("seek MMUS: {e}")))?;

    let mut section_reader = file.take(section.len);
    let count = read_u32_le(&mut section_reader)?;
    if count == 0 {
        return Err(XtaskError::Message("missing MMU entry".to_string()));
    }
    if count > limits::MAX_CPU_COUNT {
        return Err(XtaskError::Message("too many MMU states".to_string()));
    }

    let mut seen_apic_ids = HashSet::new();
    for _ in 0..count {
        let entry_len = read_u64_le(&mut section_reader)?;
        if entry_len > section_reader.limit() {
            return Err(XtaskError::Message("truncated MMU entry".to_string()));
        }

        let mut entry_reader = (&mut section_reader).take(entry_len);
        let apic_id = read_u32_le(&mut entry_reader)?;

        // Decode the per-vCPU MMU state and ignore it. The goal is to ensure the section is well
        // formed (length prefixes match, no truncation) and matches the restore contract.
        if section.version == 1 {
            let _ = MmuState::decode_v1(&mut entry_reader)
                .map_err(|e| XtaskError::Message(format!("decode MMUS v1 entry: {e}")))?;
        } else if section.version >= 2 {
            let _ = MmuState::decode_v2(&mut entry_reader)
                .map_err(|e| XtaskError::Message(format!("decode MMUS v2 entry: {e}")))?;
        } else {
            return Err(XtaskError::Message(
                "unsupported MMUS section version".to_string(),
            ));
        }

        if !seen_apic_ids.insert(apic_id) {
            return Err(XtaskError::Message(
                "duplicate APIC ID in MMU list (apic_id must be unique)".to_string(),
            ));
        }

        // Skip any forward-compatible additions.
        std::io::copy(&mut entry_reader, &mut std::io::sink())
            .map_err(|e| XtaskError::Message(format!("read MMU entry: {e}")))?;
    }

    Ok(())
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
