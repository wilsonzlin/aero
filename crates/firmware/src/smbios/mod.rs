//! SMBIOS (DMI) table generation.
//!
//! Windows (and many other PC OSes) expect SMBIOS to exist on BIOS-style
//! platforms for hardware inventory. This module generates a clean-room SMBIOS
//! 2.x entry point structure (EPS) plus a minimal structure table covering
//! BIOS/system/board/chassis/cpu/memory.

mod builder;
mod scan;
mod structures;

use crate::memory::MemoryBus;

pub use scan::{
    find_eps, parse_eps_table_info, parse_structure_headers, parse_structure_types,
    parse_structures, validate_eps_checksum, EpsTableInfo, SmbiosStructure, SmbiosStructureHeader,
};

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
        assert!(validate_eps_checksum(&eps));
    }

    fn read_smbios_table(mem: &mut VecMemory, eps_addr: u32) -> Vec<u8> {
        let mut eps = [0u8; builder::EPS_LENGTH as usize];
        mem.read_physical(eps_addr as u64, &mut eps);
        let table_info = parse_eps_table_info(&eps).expect("invalid SMBIOS EPS");
        let mut table = vec![0u8; table_info.table_len];
        mem.read_physical(table_info.table_addr, &mut table);
        table
    }

    #[test]
    fn system_uuid_changes_when_uuid_seed_changes() {
        fn read_type1_uuid(mem: &mut VecMemory, cfg: SmbiosConfig) -> [u8; 16] {
            write_bda_ebda_segment(mem, 0x9FC0);
            let eps_addr = SmbiosTables::build_and_write(&cfg, mem);
            let table = read_smbios_table(mem, eps_addr);
            let structures = parse_structures(&table);
            let type1 = structures
                .iter()
                .find(|s| s.header.ty == 1)
                .expect("type 1 missing");

            // SMBIOS Type 1 UUID field is at offset 8 and is 16 bytes long (relative to the start
            // of the formatted section, which includes the 4-byte structure header).
            type1.formatted[8..24].try_into().unwrap()
        }

        let base_cfg = SmbiosConfig {
            ram_bytes: 768 * 1024 * 1024,
            cpu_count: 1,
            uuid_seed: 0,
            eps_addr: None,
            table_addr: None,
        };

        let mut mem1 = VecMemory::new(2 * 1024 * 1024);
        let uuid1 = read_type1_uuid(&mut mem1, base_cfg.clone());

        let mut mem2 = VecMemory::new(2 * 1024 * 1024);
        let uuid2 = read_type1_uuid(
            &mut mem2,
            SmbiosConfig {
                uuid_seed: 1234,
                ..base_cfg
            },
        );

        assert_ne!(uuid1, uuid2, "UUID should change when uuid_seed changes");
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

        let table = read_smbios_table(&mut mem, eps_addr);
        let structures = parse_structures(&table);

        let type16 = structures
            .iter()
            .find(|s| s.header.ty == 16)
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
            .find(|s| s.header.ty == 17)
            .expect("type 17 missing");
        let size_mb = u16::from_le_bytes([type17.formatted[12], type17.formatted[13]]);
        assert_eq!(u64::from(size_mb), config.ram_bytes / (1024 * 1024));

        let type19 = structures
            .iter()
            .find(|s| s.header.ty == 19)
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
