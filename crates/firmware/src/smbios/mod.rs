//! SMBIOS (DMI) table generation.
//!
//! Windows (and many other PC OSes) expect SMBIOS to exist on BIOS-style
//! platforms for hardware inventory. This module generates a clean-room SMBIOS
//! 2.x entry point structure (EPS) plus a minimal structure table covering
//! BIOS/system/board/chassis/cpu/memory.

mod builder;
mod structures;

use crate::memory::MemoryBus;

/// Configuration for SMBIOS table generation.
#[derive(Clone, Debug)]
pub struct SmbiosConfig {
    /// Total guest RAM size in bytes.
    pub ram_bytes: u64,

    /// Number of virtual CPUs. SMBIOS will expose one Type 4 structure per CPU.
    pub cpu_count: u8,

    /// Deterministic seed used to generate the SMBIOS Type 1 UUID.
    pub uuid_seed: u64,

    /// Optional physical address to place the SMBIOS entry point structure at.
    /// If `None`, the builder will attempt to place it in the EBDA and fall
    /// back to the conventional scan region (0xF0000-0xFFFFF).
    pub eps_addr: Option<u32>,

    /// Optional physical address to place the SMBIOS structure table at.
    pub table_addr: Option<u32>,
}

impl Default for SmbiosConfig {
    fn default() -> Self {
        Self {
            ram_bytes: 512 * 1024 * 1024,
            cpu_count: 1,
            uuid_seed: 0,
            eps_addr: None,
            table_addr: None,
        }
    }
}

/// Builder and writer for SMBIOS tables.
pub struct SmbiosTables;

impl SmbiosTables {
    /// Build SMBIOS tables and write them into guest physical memory.
    ///
    /// Returns the physical address of the SMBIOS 2.x Entry Point Structure.
    pub fn build_and_write<M: MemoryBus>(config: &SmbiosConfig, mem: &mut M) -> u32 {
        let placement = builder::choose_placement(config, mem);

        let table = builder::build_structure_table(config);
        mem.write_physical(placement.table_addr as u64, &table.bytes);

        let eps = builder::build_eps(&table, placement.table_addr);
        mem.write_physical(placement.eps_addr as u64, &eps);

        placement.eps_addr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::VecMemory;

    fn write_bda_ebda_segment(mem: &mut VecMemory, segment: u16) {
        let addr = 0x40E;
        mem.write_physical(addr, &segment.to_le_bytes());
    }

    fn checksum_ok(bytes: &[u8]) -> bool {
        bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b)) == 0
    }

    #[test]
    fn eps_checksum_validates() {
        let mut mem = VecMemory::new(2 * 1024 * 1024);
        write_bda_ebda_segment(&mut mem, 0x9FC0);

        let config = SmbiosConfig {
            ram_bytes: 1024 * 1024 * 1024,
            ..Default::default()
        };
        let eps_addr = SmbiosTables::build_and_write(&config, &mut mem);

        let mut eps = [0u8; builder::EPS_LENGTH as usize];
        mem.read_physical(eps_addr as u64, &mut eps);
        assert_eq!(&eps[0..4], b"_SM_");
        assert!(checksum_ok(&eps));
        assert!(checksum_ok(&eps[0x10..]));
    }

    #[derive(Debug)]
    struct ParsedStructure {
        ty: u8,
        formatted: Vec<u8>,
    }

    fn parse_eps(mem: &VecMemory, eps_addr: u32) -> (u32, u16) {
        let mut eps = [0u8; builder::EPS_LENGTH as usize];
        mem.read_physical(eps_addr as u64, &mut eps);
        assert_eq!(&eps[0..4], b"_SM_");
        assert!(checksum_ok(&eps));

        let table_len = u16::from_le_bytes([eps[0x16], eps[0x17]]);
        let table_addr = u32::from_le_bytes([eps[0x18], eps[0x19], eps[0x1A], eps[0x1B]]);
        (table_addr, table_len)
    }

    fn parse_table(table: &[u8]) -> Vec<ParsedStructure> {
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < table.len() {
            let ty = table[i];
            let len = table[i + 1] as usize;
            let formatted = table[i..i + len].to_vec();
            let mut j = i + len;

            // Skip strings (we only need formatted fields for these tests).
            loop {
                if j + 1 >= table.len() {
                    panic!("unterminated string-set");
                }
                if table[j] == 0 && table[j + 1] == 0 {
                    j += 2;
                    break;
                }
                j += 1;
            }

            out.push(ParsedStructure { ty, formatted });
            i = j;
            if ty == 127 {
                break;
            }
        }
        out
    }

    #[test]
    fn memory_sizes_match_config() {
        let mut mem = VecMemory::new(2 * 1024 * 1024);
        write_bda_ebda_segment(&mut mem, 0x9FC0);

        let config = SmbiosConfig {
            ram_bytes: 768 * 1024 * 1024,
            cpu_count: 1,
            uuid_seed: 1234,
            eps_addr: None,
            table_addr: None,
        };
        let eps_addr = SmbiosTables::build_and_write(&config, &mut mem);

        let (table_addr, table_len) = parse_eps(&mem, eps_addr);
        let mut table = vec![0u8; table_len as usize];
        mem.read_physical(table_addr as u64, &mut table);

        let structures = parse_table(&table);

        let type16 = structures
            .iter()
            .find(|s| s.ty == 16)
            .expect("type 16 missing");
        let max_capacity_kb = u32::from_le_bytes([
            type16.formatted[7],
            type16.formatted[8],
            type16.formatted[9],
            type16.formatted[10],
        ]);
        assert_eq!(u64::from(max_capacity_kb), config.ram_bytes / 1024);

        let type17 = structures
            .iter()
            .find(|s| s.ty == 17)
            .expect("type 17 missing");
        let size_mb = u16::from_le_bytes([type17.formatted[12], type17.formatted[13]]);
        assert_eq!(u64::from(size_mb), config.ram_bytes / (1024 * 1024));

        let type19 = structures
            .iter()
            .find(|s| s.ty == 19)
            .expect("type 19 missing");
        let start_kb = u32::from_le_bytes([
            type19.formatted[4],
            type19.formatted[5],
            type19.formatted[6],
            type19.formatted[7],
        ]);
        let end_kb = u32::from_le_bytes([
            type19.formatted[8],
            type19.formatted[9],
            type19.formatted[10],
            type19.formatted[11],
        ]);
        assert_eq!(u64::from(start_kb), 0);
        assert_eq!(u64::from(end_kb) + 1, config.ram_bytes / 1024);
    }
}
