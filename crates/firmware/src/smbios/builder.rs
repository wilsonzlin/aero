use super::structures;
use super::SmbiosConfig;
use crate::memory::MemoryBus;

pub const EPS_LENGTH: u8 = 0x1F;

const BDA_EBDA_SEGMENT_PADDR: u64 = 0x0000_040E;
const BIOS_SCAN_BASE: u32 = 0x000F_0000;

#[derive(Clone, Copy, Debug)]
pub struct Placement {
    pub eps_addr: u32,
    pub table_addr: u32,
}

#[derive(Clone, Debug)]
pub struct BuiltTable {
    pub bytes: Vec<u8>,
    pub structure_count: u16,
    pub max_structure_size: u16,
}

pub fn choose_placement<M: MemoryBus>(config: &SmbiosConfig, mem: &M) -> Placement {
    let eps_addr = if let Some(addr) = config.eps_addr {
        align_up(addr, 16)
    } else if let Some(ebda_base) = read_ebda_base(mem) {
        // Keep within the first 1KB of EBDA (per spec), leaving space for other tables.
        align_up(ebda_base + 0x100, 16)
    } else {
        BIOS_SCAN_BASE
    };

    let table_addr = if let Some(addr) = config.table_addr {
        align_up(addr, 16)
    } else if let Some(ebda_base) = read_ebda_base(mem) {
        align_up(ebda_base + 0x200, 16)
    } else {
        align_up(BIOS_SCAN_BASE + 0x200, 16)
    };

    Placement {
        eps_addr,
        table_addr,
    }
}

fn read_ebda_base<M: MemoryBus>(mem: &M) -> Option<u32> {
    let mut seg_bytes = [0u8; 2];
    mem.read_physical(BDA_EBDA_SEGMENT_PADDR, &mut seg_bytes);
    let segment = u16::from_le_bytes(seg_bytes);
    if segment == 0 {
        return None;
    }

    let base = (segment as u32) << 4;

    // Sanity check: EBDA should live below VGA memory (0xA0000).
    if base < 0x80000 || base >= 0xA0000 {
        return None;
    }

    Some(base)
}

pub fn build_structure_table(config: &SmbiosConfig) -> BuiltTable {
    let mut builder = TableBuilder::new();
    structures::push_all(config, &mut builder);
    builder.finish()
}

pub fn build_eps(table: &BuiltTable, table_phys_addr: u32) -> [u8; EPS_LENGTH as usize] {
    let mut eps = [0u8; EPS_LENGTH as usize];
    eps[0..4].copy_from_slice(b"_SM_");
    eps[4] = 0; // checksum filled later
    eps[5] = EPS_LENGTH;
    eps[6] = 2; // SMBIOS major
    eps[7] = 1; // SMBIOS minor
    eps[8..10].copy_from_slice(&table.max_structure_size.to_le_bytes());
    eps[10] = 0; // Entry point revision
    eps[11..16].fill(0); // formatted area

    eps[16..21].copy_from_slice(b"_DMI_");
    eps[21] = 0; // intermediate checksum filled later
    eps[22..24].copy_from_slice(&(table.bytes.len() as u16).to_le_bytes());
    eps[24..28].copy_from_slice(&table_phys_addr.to_le_bytes());
    eps[28..30].copy_from_slice(&table.structure_count.to_le_bytes());
    eps[30] = 0x21; // SMBIOS BCD Revision

    // Intermediate checksum (from _DMI_ to end of EPS).
    eps[21] = checksum_byte(&eps[16..]);
    // Primary checksum (whole EPS).
    eps[4] = checksum_byte(&eps);

    eps
}

pub struct TableBuilder {
    table: Vec<u8>,
    structure_count: u16,
    max_structure_size: u16,
}

impl TableBuilder {
    pub fn new() -> Self {
        Self {
            table: Vec::new(),
            structure_count: 0,
            max_structure_size: 0,
        }
    }

    pub fn push_structure(&mut self, ty: u8, handle: u16, formatted: &[u8], strings: &[&str]) {
        let start = self.table.len();
        let length = 4usize + formatted.len();
        assert!(length <= u8::MAX as usize, "SMBIOS structure too large");

        self.table.push(ty);
        self.table.push(length as u8);
        self.table.extend_from_slice(&handle.to_le_bytes());
        self.table.extend_from_slice(formatted);

        for s in strings {
            self.table.extend_from_slice(s.as_bytes());
            self.table.push(0);
        }
        // SMBIOS structures terminate their string-set with an *additional* NUL
        // after the last string terminator. That results in two consecutive
        // NULs total at the end of the string-set.
        if strings.is_empty() {
            self.table.extend_from_slice(&[0, 0]);
        } else {
            self.table.push(0);
        }

        let size = self.table.len() - start;
        self.max_structure_size = self.max_structure_size.max(size as u16);
        self.structure_count = self.structure_count.wrapping_add(1);
    }

    pub fn finish(self) -> BuiltTable {
        BuiltTable {
            bytes: self.table,
            structure_count: self.structure_count,
            max_structure_size: self.max_structure_size,
        }
    }
}

fn checksum_byte(bytes: &[u8]) -> u8 {
    let sum = bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
    0u8.wrapping_sub(sum)
}

fn align_up(value: u32, align: u32) -> u32 {
    debug_assert!(align.is_power_of_two());
    (value + (align - 1)) & !(align - 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::VecMemory;

    #[test]
    fn default_placement_uses_ebda_when_available() {
        let mut mem = VecMemory::new(2 * 1024 * 1024);
        mem.write_physical(BDA_EBDA_SEGMENT_PADDR, &0x9FC0u16.to_le_bytes());

        let placement = choose_placement(&SmbiosConfig::default(), &mem);
        assert!(placement.eps_addr >= 0x9FC00);
        assert!(placement.eps_addr < 0xA0000);
        assert!(placement.table_addr >= 0x9FC00);
        assert!(placement.table_addr < 0xA0000);
    }

    #[test]
    fn eps_checksums_are_correct() {
        let table = BuiltTable {
            bytes: vec![0, 0, 0, 0, 0, 0],
            structure_count: 1,
            max_structure_size: 6,
        };
        let eps = build_eps(&table, 0x1234_5678);
        let sum_all = eps.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
        assert_eq!(sum_all, 0);
        let sum_intermediate = eps[0x10..].iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
        assert_eq!(sum_intermediate, 0);
    }
}
