use std::collections::BTreeSet;
use std::ops::Range;

use crate::bus::Bus;
use crate::e820::E820Entry;

fn checksum8(bytes: &[u8]) -> u8 {
    bytes
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b))
}

fn expect_sig(bytes: &[u8], expected: &[u8]) -> Result<(), String> {
    if bytes.len() < expected.len() || &bytes[..expected.len()] != expected {
        return Err(format!(
            "expected signature {:?}, got {:?}",
            expected,
            bytes.get(..expected.len()).unwrap_or(bytes)
        ));
    }
    Ok(())
}

fn read_sdt_length<B: Bus>(bus: &mut B, addr: u32) -> u32 {
    bus.read_u32(addr + 4)
}

fn read_sdt_bytes<B: Bus>(bus: &mut B, addr: u32) -> Vec<u8> {
    let length = read_sdt_length(bus, addr) as usize;
    let mut bytes = vec![0u8; length];
    bus.read(addr, &mut bytes);
    bytes
}

fn read_rsdp_bytes<B: Bus>(bus: &mut B, addr: u32) -> Vec<u8> {
    let mut len_buf = [0u8; 4];
    bus.read(addr + 20, &mut len_buf);
    let length = u32::from_le_bytes(len_buf) as usize;
    let mut bytes = vec![0u8; length];
    bus.read(addr, &mut bytes);
    bytes
}

fn decode_rsdt_entries(rsdt: &[u8]) -> Vec<u32> {
    let len = rsdt.len().saturating_sub(36);
    let count = len / 4;
    (0..count)
        .map(|i| {
            let start = 36 + i * 4;
            u32::from_le_bytes(rsdt[start..start + 4].try_into().unwrap())
        })
        .collect()
}

fn decode_xsdt_entries(xsdt: &[u8]) -> Vec<u64> {
    let len = xsdt.len().saturating_sub(36);
    let count = len / 8;
    (0..count)
        .map(|i| {
            let start = 36 + i * 8;
            u64::from_le_bytes(xsdt[start..start + 8].try_into().unwrap())
        })
        .collect()
}

fn extract_fadt_dsdt_ptr(fadt: &[u8]) -> Option<u32> {
    if fadt.len() < 44 {
        return None;
    }
    Some(u32::from_le_bytes(fadt[40..44].try_into().unwrap()))
}

fn extract_hpet_base(hpet: &[u8]) -> Option<u64> {
    // HPET base address GAS begins at offset 40 within the table:
    // header (36) + event_timer_block_id (4) = 40
    if hpet.len() < 52 {
        return None;
    }
    Some(u64::from_le_bytes(hpet[44..52].try_into().unwrap()))
}

fn madt_has_lapic_and_ioapic(madt: &[u8]) -> (bool, bool) {
    if madt.len() < 44 {
        return (false, false);
    }
    let mut has_lapic = false;
    let mut has_ioapic = false;
    let mut off = 44usize;
    while off + 2 <= madt.len() {
        let typ = madt[off];
        let len = madt[off + 1] as usize;
        if len < 2 || off + len > madt.len() {
            break;
        }
        match typ {
            0 => has_lapic = true,
            1 => has_ioapic = true,
            _ => {}
        }
        off += len;
    }
    (has_lapic, has_ioapic)
}

pub fn validate_acpi<B: Bus>(
    bus: &mut B,
    rsdp_addr: u32,
    expected_hpet_base: u64,
) -> Result<(), String> {
    let rsdp = read_rsdp_bytes(bus, rsdp_addr);
    expect_sig(&rsdp, b"RSD PTR ")?;

    if checksum8(&rsdp[0..20]) != 0 {
        return Err("RSDP checksum over first 20 bytes is not zero".into());
    }
    if checksum8(&rsdp) != 0 {
        return Err("RSDP extended checksum is not zero".into());
    }

    if rsdp.len() < 36 {
        return Err(format!("RSDP length {} too small", rsdp.len()));
    }
    let rsdt_addr = u32::from_le_bytes(rsdp[16..20].try_into().unwrap());
    let xsdt_addr = u64::from_le_bytes(rsdp[24..32].try_into().unwrap());
    if rsdt_addr == 0 || xsdt_addr == 0 {
        return Err(format!(
            "RSDP missing table pointers (rsdt={rsdt_addr:#x}, xsdt={xsdt_addr:#x})"
        ));
    }

    let rsdt = read_sdt_bytes(bus, rsdt_addr);
    expect_sig(&rsdt, b"RSDT")?;
    if checksum8(&rsdt) != 0 {
        return Err("RSDT checksum is not zero".into());
    }
    let xsdt = read_sdt_bytes(bus, xsdt_addr as u32);
    expect_sig(&xsdt, b"XSDT")?;
    if checksum8(&xsdt) != 0 {
        return Err("XSDT checksum is not zero".into());
    }

    let mut addrs = BTreeSet::new();
    for addr in decode_rsdt_entries(&rsdt) {
        addrs.insert(addr as u64);
    }
    for addr in decode_xsdt_entries(&xsdt) {
        addrs.insert(addr);
    }

    if addrs.is_empty() {
        return Err("RSDT/XSDT did not enumerate any tables".into());
    }

    let mut fadt = None;
    let mut madt = None;
    let mut hpet = None;

    for addr in addrs {
        let table = read_sdt_bytes(bus, addr as u32);
        if checksum8(&table) != 0 {
            let sig = String::from_utf8_lossy(&table.get(0..4).unwrap_or_default());
            return Err(format!("ACPI table {sig} @ {addr:#x} failed checksum"));
        }

        match table.get(0..4) {
            Some(b"FACP") => fadt = Some((addr as u32, table)),
            Some(b"APIC") => madt = Some((addr as u32, table)),
            Some(b"HPET") => hpet = Some((addr as u32, table)),
            _ => {}
        }
    }

    let (_fadt_addr, fadt_bytes) = fadt.ok_or_else(|| "missing FADT (FACP) table".to_string())?;
    let dsdt_ptr =
        extract_fadt_dsdt_ptr(&fadt_bytes).ok_or_else(|| "FADT too small".to_string())?;
    if dsdt_ptr == 0 {
        return Err("FADT DSDT pointer is zero".into());
    }
    let dsdt = read_sdt_bytes(bus, dsdt_ptr);
    expect_sig(&dsdt, b"DSDT")?;
    if checksum8(&dsdt) != 0 {
        return Err("DSDT checksum is not zero".into());
    }

    let (_madt_addr, madt_bytes) = madt.ok_or_else(|| "missing MADT (APIC) table".to_string())?;
    let (has_lapic, has_ioapic) = madt_has_lapic_and_ioapic(&madt_bytes);
    if !has_lapic || !has_ioapic {
        return Err(format!(
            "MADT missing required entries (lapic={has_lapic}, ioapic={has_ioapic})"
        ));
    }

    let (_hpet_addr, hpet_bytes) = hpet.ok_or_else(|| "missing HPET table".to_string())?;
    let hpet_base = extract_hpet_base(&hpet_bytes).ok_or_else(|| "HPET table too small".to_string())?;
    if hpet_base != expected_hpet_base {
        return Err(format!(
            "HPET base mismatch: table={hpet_base:#x}, expected={expected_hpet_base:#x}"
        ));
    }

    Ok(())
}

pub fn validate_e820(entries: &[E820Entry], reserved: &[Range<u64>]) -> Result<(), String> {
    if entries.is_empty() {
        return Err("E820 map is empty".into());
    }

    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|e| e.base);

    for (idx, entry) in sorted.iter().enumerate() {
        if entry.length == 0 {
            return Err(format!("E820 entry {idx} has zero length"));
        }
        if idx > 0 {
            let prev = &sorted[idx - 1];
            if entry.base < prev.end() {
                return Err(format!(
                    "E820 overlap: entry {idx} [{:#x}-{:#x}) overlaps previous [{:#x}-{:#x})",
                    entry.base,
                    entry.end(),
                    prev.base,
                    prev.end()
                ));
            }
        }
    }

    for res in reserved {
        for entry in &sorted {
            if entry.typ != E820Entry::TYPE_RAM {
                continue;
            }
            let start = entry.base.max(res.start);
            let end = entry.end().min(res.end);
            if start < end {
                return Err(format!(
                    "E820 marks reserved range [{:#x}-{:#x}) as RAM via entry [{:#x}-{:#x})",
                    res.start,
                    res.end,
                    entry.base,
                    entry.end()
                ));
            }
        }
    }

    Ok(())
}
