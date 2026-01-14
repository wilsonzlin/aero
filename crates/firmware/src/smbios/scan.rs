//! Host-side SMBIOS scanning and parsing helpers.
//!
//! Aero firmware generates an SMBIOS 2.x Entry Point Structure (EPS) plus a
//! structure table in guest physical memory. Many consumers (unit tests,
//! diagnostics tooling) need to:
//! - locate the EPS via the SMBIOS-specified scan algorithm,
//! - validate the EPS checksums, and
//! - walk the structure table.
//!
//! The helpers in this module are intentionally small and defensive so they can
//! be reused across crates without duplicating SMBIOS parsing logic.
//!
//! References:
//! - SMBIOS 2.1+ "Entry Point Structure" (`_SM_` + `_DMI_`) scan rules.

use crate::memory::MemoryBus;

const BDA_EBDA_SEGMENT_PADDR: u64 = 0x0000_040E;

const EBDA_SCAN_LEN: usize = 1024; // first KiB

const BIOS_SCAN_BASE: u64 = 0x000F_0000;
const BIOS_SCAN_LEN: usize = 0x10000; // 0xF0000..0xFFFFF inclusive

const EPS_ANCHOR: &[u8; 4] = b"_SM_";
const DMI_ANCHOR: &[u8; 5] = b"_DMI_";

/// Parsed SMBIOS EPS fields needed to locate the structure table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpsTableInfo {
    /// Physical address of the SMBIOS structure table.
    pub table_addr: u64,
    /// Length (in bytes) of the structure table.
    pub table_len: usize,
}

/// Parsed SMBIOS structure header fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmbiosStructureHeader {
    /// SMBIOS structure type.
    pub ty: u8,
    /// Length of the formatted section, including the 4-byte header.
    pub len: u8,
    /// SMBIOS structure handle.
    pub handle: u16,
}

/// Scan guest memory for an SMBIOS 2.x Entry Point Structure (EPS) and return
/// its physical address.
///
/// Scan order per SMBIOS spec:
/// 1. First 1KiB of the EBDA (if present)
/// 2. 0xF0000..0xFFFFF on 16-byte boundaries
///
/// This helper only identifies the `_SM_` anchor; callers should validate the
/// checksum with [`validate_eps_checksum`] before trusting other fields.
pub fn find_eps(mem: &mut impl MemoryBus) -> Option<u64> {
    // SMBIOS spec: EBDA segment pointer is stored at BDA offset 0x0E (0x040E).
    let ebda_seg = mem.read_u16(BDA_EBDA_SEGMENT_PADDR);
    if ebda_seg != 0 {
        let ebda_base = (ebda_seg as u64) << 4;

        // Defensive sanity check: EBDA should live below VGA memory (0xA0000)
        // and above the conventional "base memory" area (0x80000).
        if (0x80000..0xA0000).contains(&ebda_base) {
            if let Some(addr) = scan_region_for_anchor(mem, ebda_base, EBDA_SCAN_LEN) {
                return Some(addr);
            }
        }
    }

    scan_region_for_anchor(mem, BIOS_SCAN_BASE, BIOS_SCAN_LEN)
}

fn scan_region_for_anchor(mem: &mut impl MemoryBus, base: u64, len: usize) -> Option<u64> {
    if len < EPS_ANCHOR.len() {
        return None;
    }

    let mut buf = vec![0u8; len];
    mem.read_physical(base, &mut buf);

    // SMBIOS scan granularity is 16 bytes.
    let last = len.saturating_sub(EPS_ANCHOR.len());
    for off in (0..=last).step_by(16) {
        if buf.get(off..off + EPS_ANCHOR.len()) == Some(EPS_ANCHOR) {
            return base.checked_add(off as u64);
        }
    }
    None
}

/// Validate the checksum(s) for an SMBIOS 2.x Entry Point Structure (EPS).
///
/// SMBIOS 2.x defines two checksums:
/// - The primary checksum covers `eps[0..eps_len)`.
/// - The intermediate checksum covers `eps[0x10..eps_len)`.
///
/// Returns `false` for malformed inputs (too short, bad anchors, etc.).
pub fn validate_eps_checksum(eps: &[u8]) -> bool {
    // Need at least anchor + checksum byte + length byte.
    if eps.len() < 6 {
        return false;
    }
    if eps.get(0..4) != Some(EPS_ANCHOR) {
        return false;
    }

    let eps_len = eps[5] as usize;
    if eps_len == 0 || eps_len > eps.len() {
        return false;
    }

    let primary_sum = eps[..eps_len]
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    if primary_sum != 0 {
        return false;
    }

    // `_DMI_` anchor must be present for SMBIOS 2.x EPS; it starts at offset 0x10.
    if eps_len < 0x10 + DMI_ANCHOR.len() {
        return false;
    }
    if eps.get(0x10..0x10 + DMI_ANCHOR.len()) != Some(DMI_ANCHOR) {
        return false;
    }

    let intermediate_sum = eps[0x10..eps_len]
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    intermediate_sum == 0
}

/// Parse the structure table physical address and length from an SMBIOS 2.x EPS.
///
/// Returns `None` for malformed inputs.
pub fn parse_eps_table_info(eps: &[u8]) -> Option<EpsTableInfo> {
    // Needs fields up to and including table address (offset 0x18..0x1B).
    if eps.len() < 0x1C {
        return None;
    }

    // Validate anchors/checksums before trusting offsets.
    if !validate_eps_checksum(eps) {
        return None;
    }
    // Ensure the declared EPS length covers the fields we parse. This also guarantees the table
    // address/length bytes are covered by the primary checksum.
    let eps_len = eps.get(5).copied()? as usize;
    if eps_len < 0x1C {
        return None;
    }

    let table_len = u16::from_le_bytes([*eps.get(0x16)?, *eps.get(0x17)?]) as usize;
    let table_addr = u32::from_le_bytes([
        *eps.get(0x18)?,
        *eps.get(0x19)?,
        *eps.get(0x1A)?,
        *eps.get(0x1B)?,
    ]) as u64;

    Some(EpsTableInfo {
        table_addr,
        table_len,
    })
}

/// Walk an SMBIOS structure table and return the structure types encountered.
///
/// The parser stops after the first Type 127 ("End-of-table") structure, or
/// earlier if the input is malformed/truncated.
///
/// The SMBIOS structure format is:
/// - Fixed header (`type`, `len`, `handle`)
/// - Formatted section (`len` bytes total including header)
/// - String-set terminated by a double-NUL (`0x00 0x00`)
pub fn parse_structure_types(table: &[u8]) -> Vec<u8> {
    parse_structure_headers(table)
        .into_iter()
        .map(|h| h.ty)
        .collect()
}

/// Walk an SMBIOS structure table and return the structure headers encountered.
///
/// The parser stops after the first Type 127 ("End-of-table") structure, or
/// earlier if the input is malformed/truncated.
pub fn parse_structure_headers(table: &[u8]) -> Vec<SmbiosStructureHeader> {
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < table.len() {
        // Need at least type + length + handle.
        if table.len() - i < 4 {
            break;
        }
        let ty = table[i];
        let len = table[i + 1];
        let len_usize = len as usize;

        // `len` includes the 4-byte header (type/len/handle).
        if len_usize < 4 {
            break;
        }

        let formatted_end = match i.checked_add(len_usize) {
            Some(v) => v,
            None => break,
        };
        if formatted_end > table.len() {
            break;
        }

        let handle = u16::from_le_bytes([table[i + 2], table[i + 3]]);

        // Walk the string-set until the double-NUL terminator.
        let mut j = formatted_end;
        let mut found = false;
        while j + 1 < table.len() {
            if table[j] == 0 && table[j + 1] == 0 {
                j += 2;
                found = true;
                break;
            }
            j += 1;
        }
        if !found {
            break;
        }

        out.push(SmbiosStructureHeader { ty, len, handle });
        i = j;

        if ty == 127 {
            break;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_structure_types_is_defensive() {
        // Empty input should not panic.
        assert!(parse_structure_types(&[]).is_empty());

        // Truncated header.
        assert!(parse_structure_types(&[0]).is_empty());

        // Invalid length (<4).
        assert!(parse_structure_types(&[0, 1, 0, 0]).is_empty());

        // Valid header + unterminated string-set: should not panic, should stop gracefully.
        let table = [
            0u8, 4, 0, 0, // type 0, len 4
            0x41, 0x42, 0x43, // string data without terminator
        ];
        assert!(parse_structure_types(&table).is_empty());
    }
}
