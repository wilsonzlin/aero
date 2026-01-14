use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};
use aero_devices::pci::{PciBarKind, PciBarRange, PciBdf, PciResourceAllocatorConfig};
use aero_pc_constants::{
    PCIE_ECAM_BASE, PCIE_ECAM_END_BUS, PCIE_ECAM_SEGMENT, PCIE_ECAM_SIZE, PCIE_ECAM_START_BUS,
};
use aero_pc_platform::{PcPlatform, PcPlatformConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MmioWindow {
    start: u64,
    end: u64, // exclusive
}

impl MmioWindow {
    fn contains(&self, start: u64, end: u64) -> bool {
        start >= self.start && end <= self.end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IoWindow {
    start: u64,
    end: u64, // exclusive
}

impl IoWindow {
    fn contains(&self, start: u64, end: u64) -> bool {
        start >= self.start && end <= self.end
    }
}

fn ranges_overlap(a_start: u64, a_end: u64, b_start: u64, b_end: u64) -> bool {
    a_start < b_end && a_end > b_start
}

fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn read_u64_le(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(u64::from_le_bytes(
        bytes.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

/// Parse ACPI AML `PkgLength` encoding.
///
/// Returns (length, length_field_bytes).
///
/// Note: the returned `length` is the raw AML `PkgLength` value, which (per spec) includes the
/// byte length of the `PkgLength` field itself, but does *not* include the opcode byte(s).
///
/// For multi-byte encodings, bits 4-5 of the lead byte are reserved and should be zero.
fn parse_pkg_length(bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    let b0 = *bytes.get(offset)?;
    let follow_bytes = (b0 >> 6) as usize;
    let mut len: usize = if follow_bytes == 0 {
        (b0 & 0x3F) as usize
    } else {
        // Reserved bits; the aero-acpi generator should always emit these as zero.
        if (b0 & 0x30) != 0 {
            return None;
        }
        (b0 & 0x0F) as usize
    };
    for i in 0..follow_bytes {
        let b = *bytes.get(offset + 1 + i)?;
        len |= (b as usize) << (4 + i * 8);
    }
    Some((len, 1 + follow_bytes))
}

/// Parse an AML integer object at `offset`.
///
/// Returns (value, bytes_consumed).
fn parse_integer(bytes: &[u8], offset: usize) -> Option<(u64, usize)> {
    match *bytes.get(offset)? {
        0x00 => Some((0, 1)),                              // ZeroOp
        0x01 => Some((1, 1)),                              // OneOp
        0x0A => Some((*bytes.get(offset + 1)? as u64, 2)), // BytePrefix
        0x0B => Some((
            u16::from_le_bytes(bytes.get(offset + 1..offset + 3)?.try_into().ok()?) as u64,
            3,
        )),
        0x0C => Some((
            u32::from_le_bytes(bytes.get(offset + 1..offset + 5)?.try_into().ok()?) as u64,
            5,
        )),
        0x0E => Some((
            u64::from_le_bytes(bytes.get(offset + 1..offset + 9)?.try_into().ok()?),
            9,
        )),
        _ => None,
    }
}

/// Extract the PCI0 `_CRS` buffer contents from the DSDT AML emitted by `aero-acpi`.
fn extract_pci0_crs_bytes(aml: &[u8]) -> Vec<u8> {
    let mut i = 0usize;
    while i + 2 < aml.len() {
        // DeviceOp = ExtOpPrefix(0x5B) + 0x82.
        if aml[i] == 0x5B && aml[i + 1] == 0x82 {
            let pkg_start = i + 2;
            let (pkg_len, pkg_len_bytes) =
                parse_pkg_length(aml, pkg_start).expect("DeviceOp PkgLength should parse");
            let payload_start = pkg_start + pkg_len_bytes;
            let payload_len = pkg_len
                .checked_sub(pkg_len_bytes)
                .expect("DeviceOp PkgLength should include its own encoding bytes");
            let payload_end = payload_start
                .checked_add(payload_len)
                .expect("DeviceOp payload end overflow");
            assert!(
                payload_end <= aml.len(),
                "DeviceOp payload overruns AML (end=0x{payload_end:x} len=0x{:x})",
                aml.len()
            );
            assert!(
                payload_start + 4 <= payload_end,
                "DeviceOp payload too small for NameSeg"
            );
            let name = &aml[payload_start..payload_start + 4];
            if name == b"PCI0" {
                let body_start = payload_start + 4;
                let body_end = payload_end;

                // Look for: NameOp (0x08) + NameSeg("_CRS") + BufferOp (0x11).
                let mut j = body_start;
                while j + 6 <= body_end {
                    if aml[j] == 0x08 && &aml[j + 1..j + 5] == b"_CRS" {
                        let buf_op = j + 5;
                        assert_eq!(
                            aml.get(buf_op).copied(),
                            Some(0x11),
                            "PCI0._CRS should be a BufferOp (0x11)"
                        );

                        let buf_pkg_start = buf_op + 1;
                        let (buf_pkg_len, buf_pkg_len_bytes) = parse_pkg_length(aml, buf_pkg_start)
                            .expect("BufferOp PkgLength should parse");
                        let buf_payload_start = buf_pkg_start + buf_pkg_len_bytes;
                        let buf_payload_len = buf_pkg_len
                            .checked_sub(buf_pkg_len_bytes)
                            .expect("BufferOp PkgLength should include its own encoding bytes");
                        let buf_payload_end = buf_payload_start
                            .checked_add(buf_payload_len)
                            .expect("BufferOp payload end overflow");
                        assert!(
                            buf_payload_end <= body_end,
                            "PCI0._CRS buffer overruns PCI0 device body"
                        );

                        // Buffer payload: <Integer buffer_size> + raw bytes.
                        let (buf_size, buf_size_len) = parse_integer(aml, buf_payload_start)
                            .expect("PCI0._CRS buffer size integer should parse");
                        let buf_size_usize =
                            usize::try_from(buf_size).expect("PCI0._CRS buffer size overflow");
                        let bytes_start = buf_payload_start + buf_size_len;
                        let bytes_end = bytes_start
                            .checked_add(buf_size_usize)
                            .expect("PCI0._CRS bytes end overflow");
                        assert!(
                            bytes_end <= buf_payload_end,
                            "PCI0._CRS buffer bytes out of bounds"
                        );
                        return aml[bytes_start..bytes_end].to_vec();
                    }
                    j += 1;
                }

                panic!("PCI0._CRS not found inside PCI0 device");
            }

            // Skip past the DeviceOp payload (it contains the nested objects).
            i = payload_end;
            continue;
        }

        i += 1;
    }

    panic!("PCI0 device not found in DSDT AML");
}

/// Parse address windows from a `ResourceTemplate` byte buffer (the contents of a `_CRS` buffer).
fn parse_mmio_windows_from_crs(crs: &[u8]) -> Vec<MmioWindow> {
    let mut windows = Vec::new();
    let mut i = 0usize;

    while i < crs.len() {
        let tag = crs[i];
        // EndTag (small item, tag=0x79 length=0).
        if tag == 0x79 {
            break;
        }

        // Small items have the high bit clear.
        if (tag & 0x80) == 0 {
            let len = (tag & 0x07) as usize;
            i = i
                .checked_add(1 + len)
                .expect("ACPI small descriptor overflow");
            continue;
        }

        // Large item: tag + 16-bit length.
        let len = usize::from(read_u16_le(crs, i + 1).expect("ACPI large descriptor length"));
        let body_start = i + 3;
        let body_end = body_start
            .checked_add(len)
            .expect("ACPI large descriptor overflow");
        assert!(body_end <= crs.len(), "ACPI large descriptor truncated");

        match tag {
            // DWord Address Space Descriptor
            0x87 if len >= 0x17 => {
                let resource_type = crs[i + 3];
                if resource_type == 0x00 {
                    let start = u64::from(read_u32_le(crs, i + 10).expect("dword.min"));
                    let length = u64::from(read_u32_le(crs, i + 22).expect("dword.length"));
                    windows.push(MmioWindow {
                        start,
                        end: start.saturating_add(length),
                    });
                }
            }
            // QWord Address Space Descriptor
            0x8A if len >= 0x2B => {
                let resource_type = crs[i + 3];
                if resource_type == 0x00 {
                    let start = read_u64_le(crs, i + 14).expect("qword.min");
                    let length = read_u64_le(crs, i + 38).expect("qword.length");
                    windows.push(MmioWindow {
                        start,
                        end: start.saturating_add(length),
                    });
                }
            }
            _ => {}
        }

        i = body_end;
    }

    windows
}

fn parse_io_windows_from_crs(crs: &[u8]) -> Vec<IoWindow> {
    let mut windows = Vec::new();
    let mut i = 0usize;

    while i < crs.len() {
        let tag = crs[i];
        // EndTag (small item, tag=0x79 length=0).
        if tag == 0x79 {
            break;
        }

        // Small items have the high bit clear.
        if (tag & 0x80) == 0 {
            let len = (tag & 0x07) as usize;
            i = i
                .checked_add(1 + len)
                .expect("ACPI small descriptor overflow");
            continue;
        }

        // Large item: tag + 16-bit length.
        let len = usize::from(read_u16_le(crs, i + 1).expect("ACPI large descriptor length"));
        let body_start = i + 3;
        let body_end = body_start
            .checked_add(len)
            .expect("ACPI large descriptor overflow");
        assert!(body_end <= crs.len(), "ACPI large descriptor truncated");

        match tag {
            // Word Address Space Descriptor (I/O).
            0x88 if len >= 0x0D => {
                let resource_type = crs[i + 3];
                if resource_type == 0x01 {
                    let start = u64::from(read_u16_le(crs, i + 8).expect("word.min"));
                    let length = u64::from(read_u16_le(crs, i + 14).expect("word.length"));
                    windows.push(IoWindow {
                        start,
                        end: start.saturating_add(length),
                    });
                }
            }
            _ => {}
        }

        i = body_end;
    }

    windows
}

#[test]
fn pci_mmio_bars_stay_within_acpi_mmio_window_and_do_not_overlap_ecam() {
    let mut pc = PcPlatform::new_with_config(
        32 * 1024 * 1024,
        PcPlatformConfig {
            enable_e1000: true,
            enable_virtio_blk: true,
            ..Default::default()
        },
    );

    // Ensure BAR allocation + decoding is applied (even if the constructor changes).
    pc.reset_pci();

    let mmio_bars: Vec<(PciBdf, u8, PciBarRange)> = {
        let mut pci_cfg = pc.pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let mut out = Vec::new();
        for bdf in bus.iter_device_addrs() {
            let cfg = bus
                .device_config(bdf)
                .unwrap_or_else(|| panic!("PCI device disappeared: {bdf:?}"));
            for bar in 0u8..6 {
                let Some(range) = cfg.bar_range(bar) else {
                    continue;
                };
                if range.base == 0 {
                    continue;
                }
                if !matches!(range.kind, PciBarKind::Mmio32 | PciBarKind::Mmio64) {
                    continue;
                }
                out.push((bdf, bar, range));
            }
        }
        out
    };

    assert!(
        !mmio_bars.is_empty(),
        "test expected at least one MMIO BAR to be allocated"
    );

    let io_bars: Vec<(PciBdf, u8, PciBarRange)> = {
        let mut pci_cfg = pc.pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let mut out = Vec::new();
        for bdf in bus.iter_device_addrs() {
            let cfg = bus
                .device_config(bdf)
                .unwrap_or_else(|| panic!("PCI device disappeared: {bdf:?}"));
            for bar in 0u8..6 {
                let Some(range) = cfg.bar_range(bar) else {
                    continue;
                };
                if range.base == 0 {
                    continue;
                }
                if !matches!(range.kind, PciBarKind::Io) {
                    continue;
                }
                out.push((bdf, bar, range));
            }
        }
        out
    };

    // Basic sanity: PCI BAR bases should be naturally aligned to their size.
    for (bdf, bar, range) in &mmio_bars {
        assert!(
            range.size != 0,
            "PCI MMIO BAR has zero size: {bdf:?} BAR{bar} {range:?}"
        );
        assert!(
            range.base % range.size == 0,
            "PCI MMIO BAR base is not aligned to its size: {bdf:?} BAR{bar} base=0x{:x} size=0x{:x}",
            range.base,
            range.size
        );
    }

    let acpi_cfg = AcpiConfig {
        // Enable PCIe-friendly config space access via MMCONFIG/ECAM. This must match the PC
        // platform memory map (`aero-pc-constants`).
        pcie_ecam_base: PCIE_ECAM_BASE,
        pcie_segment: PCIE_ECAM_SEGMENT,
        pcie_start_bus: PCIE_ECAM_START_BUS,
        pcie_end_bus: PCIE_ECAM_END_BUS,
        ..Default::default()
    };

    let dsdt = AcpiTables::build(&acpi_cfg, AcpiPlacement::default()).dsdt;
    let aml = &dsdt[36..];
    let crs = extract_pci0_crs_bytes(aml);
    let mmio_windows = parse_mmio_windows_from_crs(&crs);
    let io_windows = parse_io_windows_from_crs(&crs);

    assert!(
        !mmio_windows.is_empty(),
        "expected PCI0._CRS to advertise at least one MMIO window"
    );
    assert!(
        !io_windows.is_empty(),
        "expected PCI0._CRS to advertise at least one I/O window"
    );

    let ecam_start = PCIE_ECAM_BASE;
    let ecam_end = ecam_start + PCIE_ECAM_SIZE;

    // PCI0._CRS should not advertise the ECAM/MMCONFIG region as part of the general-purpose
    // PCI MMIO aperture (it is described separately by MCFG). If this regresses, guests may
    // allocate BARs into ECAM and corrupt config space accesses.
    for w in &mmio_windows {
        assert!(
            !ranges_overlap(w.start, w.end, ecam_start, ecam_end),
            "ACPI PCI0._CRS MMIO window overlaps ECAM: window=[0x{:x}..0x{:x}) ecam=[0x{ecam_start:x}..0x{ecam_end:x})",
            w.start,
            w.end
        );
    }

    // I/O BAR ranges should also fit inside the ACPI-declared I/O windows.
    for &(bdf, bar, range) in &io_bars {
        let start = range.base;
        let end = range.base.saturating_add(range.size);
        assert!(
            io_windows.iter().any(|w| w.contains(start, end)),
            "PCI I/O BAR outside ACPI-declared PCI0._CRS I/O windows: {bdf:?} BAR{bar} start=0x{start:x} end=0x{end:x} windows={io_windows:?}",
        );
    }

    // Ensure the allocator's I/O aperture is contained within the ACPI-declared windows.
    let alloc_cfg = PciResourceAllocatorConfig::default();
    let alloc_io_start = u64::from(alloc_cfg.io_base);
    let alloc_io_end = alloc_io_start.saturating_add(u64::from(alloc_cfg.io_size));
    assert!(
        io_windows
            .iter()
            .any(|w| w.contains(alloc_io_start, alloc_io_end)),
        "PciResourceAllocatorConfig::default() I/O window is not fully contained in any ACPI PCI0._CRS I/O window: allocator=[0x{alloc_io_start:x}..0x{alloc_io_end:x}) windows={io_windows:?}",
    );

    // Ensure the allocator's entire MMIO aperture is contained within the ACPI-declared windows.
    // This makes the test fail if `PciResourceAllocatorConfig::default()` drifts outside `_CRS`,
    // even if the current set of devices doesn't allocate enough BAR space to hit the edges.
    let alloc_start = alloc_cfg.mmio_base;
    let alloc_end = alloc_start
        .checked_add(alloc_cfg.mmio_size)
        .expect("allocator MMIO window overflow");
    assert!(
        mmio_windows.iter().any(|w| w.contains(alloc_start, alloc_end)),
        "PciResourceAllocatorConfig::default() MMIO window is not fully contained in any ACPI PCI0._CRS MMIO window: allocator=[0x{alloc_start:x}..0x{alloc_end:x}) windows={mmio_windows:?}",
    );
    assert!(
        !ranges_overlap(alloc_start, alloc_end, ecam_start, ecam_end),
        "PciResourceAllocatorConfig::default() MMIO window overlaps ECAM: allocator=[0x{alloc_start:x}..0x{alloc_end:x}) ecam=[0x{ecam_start:x}..0x{ecam_end:x})",
    );

    for &(bdf, bar, range) in &mmio_bars {
        let start = range.base;
        let end = range
            .base
            .checked_add(range.size)
            .unwrap_or_else(|| panic!("BAR range overflow: {bdf:?} BAR{bar} {range:?}"));

        assert!(
            start >= u64::from(acpi_cfg.pci_mmio_base),
            "PCI MMIO BAR below ACPI pci_mmio_base: {bdf:?} BAR{bar} start=0x{start:x} pci_mmio_base=0x{:x}",
            acpi_cfg.pci_mmio_base
        );

        assert!(
            mmio_windows.iter().any(|w| w.contains(start, end)),
            "PCI MMIO BAR outside ACPI-declared PCI0._CRS MMIO windows: {bdf:?} BAR{bar} start=0x{start:x} end=0x{end:x} windows={mmio_windows:?}",
        );

        assert!(
            !ranges_overlap(start, end, ecam_start, ecam_end),
            "PCI MMIO BAR overlaps ECAM window: {bdf:?} BAR{bar} [0x{start:x}..0x{end:x}) overlaps ECAM [0x{ecam_start:x}..0x{ecam_end:x})",
        );
    }

    // Ensure BAR ranges don't overlap each other (a regression in the allocator could violate
    // this even if everything still falls within the larger ACPI window).
    let mut bar_ranges = mmio_bars
        .iter()
        .copied()
        .map(|(bdf, bar, range)| {
            let start = range.base;
            let end = range.base.saturating_add(range.size);
            (start, end, bdf, bar)
        })
        .collect::<Vec<_>>();
    bar_ranges.sort_by_key(|(start, _, _, _)| *start);
    for win in bar_ranges.windows(2) {
        let (a_start, a_end, a_bdf, a_bar) = win[0];
        let (b_start, _b_end, b_bdf, b_bar) = win[1];
        assert!(
            a_end <= b_start,
            "PCI MMIO BAR ranges overlap: {a_bdf:?} BAR{a_bar} [0x{a_start:x}..0x{a_end:x}) overlaps {b_bdf:?} BAR{b_bar} starting at 0x{b_start:x}"
        );
    }
}
