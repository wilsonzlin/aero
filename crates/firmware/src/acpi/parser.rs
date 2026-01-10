//! Minimal ACPI parser helpers used by tests.

use crate::acpi::builder::checksum8;
use crate::acpi::structures::{ACPI_HEADER_SIZE, RSDP_CHECKSUM_LEN_V1, RSDP_V2_SIZE};
use memory::GuestMemory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedHeader {
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub oem_table_id: [u8; 8],
}

pub fn parse_header(bytes: &[u8]) -> Option<ParsedHeader> {
    if bytes.len() < ACPI_HEADER_SIZE {
        return None;
    }
    let signature: [u8; 4] = bytes[0..4].try_into().ok()?;
    let length = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let revision = bytes[8];
    let checksum = bytes[9];
    let oem_id: [u8; 6] = bytes[10..16].try_into().ok()?;
    let oem_table_id: [u8; 8] = bytes[16..24].try_into().ok()?;
    Some(ParsedHeader {
        signature,
        length,
        revision,
        checksum,
        oem_id,
        oem_table_id,
    })
}

pub fn validate_table_checksum(table: &[u8]) -> bool {
    checksum8(table) == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedRsdpV2 {
    pub oem_id: [u8; 6],
    pub revision: u8,
    pub rsdt_address: u32,
    pub length: u32,
    pub xsdt_address: u64,
}

pub fn parse_rsdp_v2(bytes: &[u8]) -> Option<ParsedRsdpV2> {
    if bytes.len() < RSDP_V2_SIZE {
        return None;
    }
    if &bytes[0..8] != b"RSD PTR " {
        return None;
    }
    if checksum8(&bytes[..RSDP_CHECKSUM_LEN_V1]) != 0 {
        return None;
    }
    if checksum8(&bytes[..RSDP_V2_SIZE]) != 0 {
        return None;
    }
    let oem_id: [u8; 6] = bytes[9..15].try_into().ok()?;
    let revision = bytes[15];
    let rsdt_address = u32::from_le_bytes(bytes[16..20].try_into().ok()?);
    let length = u32::from_le_bytes(bytes[20..24].try_into().ok()?);
    let xsdt_address = u64::from_le_bytes(bytes[24..32].try_into().ok()?);
    Some(ParsedRsdpV2 {
        oem_id,
        revision,
        rsdt_address,
        length,
        xsdt_address,
    })
}

pub fn parse_rsdt_entries(rsdt_bytes: &[u8]) -> Option<Vec<u32>> {
    let hdr = parse_header(rsdt_bytes)?;
    if &hdr.signature != b"RSDT" {
        return None;
    }
    let length = hdr.length as usize;
    if length < ACPI_HEADER_SIZE || length > rsdt_bytes.len() {
        return None;
    }
    if (length - ACPI_HEADER_SIZE) % 4 != 0 {
        return None;
    }
    let entries_bytes = &rsdt_bytes[ACPI_HEADER_SIZE..length];
    let mut out = Vec::new();
    for chunk in entries_bytes.chunks_exact(4) {
        out.push(u32::from_le_bytes(chunk.try_into().ok()?));
    }
    Some(out)
}

pub fn parse_xsdt_entries(xsdt_bytes: &[u8]) -> Option<Vec<u64>> {
    let hdr = parse_header(xsdt_bytes)?;
    if &hdr.signature != b"XSDT" {
        return None;
    }
    let length = hdr.length as usize;
    if length < ACPI_HEADER_SIZE || length > xsdt_bytes.len() {
        return None;
    }
    if (length - ACPI_HEADER_SIZE) % 8 != 0 {
        return None;
    }
    let entries_bytes = &xsdt_bytes[ACPI_HEADER_SIZE..length];
    let mut out = Vec::new();
    for chunk in entries_bytes.chunks_exact(8) {
        out.push(u64::from_le_bytes(chunk.try_into().ok()?));
    }
    Some(out)
}

/// Scan a memory region for an RSDP (v2) on 16-byte boundaries.
pub fn find_rsdp_in_memory<M: GuestMemory>(
    mem: &M,
    start: u64,
    end: u64,
) -> Option<u64> {
    let mut addr = (start + 15) & !15;
    let mut sig = [0u8; 8];
    while addr + (RSDP_V2_SIZE as u64) <= end {
        mem.read_into(addr, &mut sig).ok()?;
        if &sig == b"RSD PTR " {
            let mut buf = [0u8; RSDP_V2_SIZE];
            mem.read_into(addr, &mut buf).ok()?;
            if parse_rsdp_v2(&buf).is_some() {
                return Some(addr);
            }
        }
        addr += 16;
    }
    None
}
