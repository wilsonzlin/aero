#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct E820Entry {
    pub base: u64,
    pub length: u64,
    pub typ: u32,
}

impl E820Entry {
    pub const TYPE_RAM: u32 = 1;
    pub const TYPE_RESERVED: u32 = 2;

    pub fn end(&self) -> u64 {
        self.base.saturating_add(self.length)
    }
}

/// Generate a conservative E820 map for a simple PC-compatible machine.
pub fn build_default_e820(ram_size: u64) -> Vec<E820Entry> {
    // Layout roughly matches a typical BIOS:
    // - low memory available
    // - VGA hole + option ROM region reserved
    // - BIOS area reserved
    // - rest of RAM available
    let mut entries = Vec::new();

    let low_ram_end = 0x0009_FC00u64;
    let one_mb = 0x0010_0000u64;

    entries.push(E820Entry {
        base: 0,
        length: low_ram_end,
        typ: E820Entry::TYPE_RAM,
    });

    if low_ram_end < one_mb {
        entries.push(E820Entry {
            base: low_ram_end,
            length: one_mb - low_ram_end,
            typ: E820Entry::TYPE_RESERVED,
        });
    }

    if ram_size > one_mb {
        entries.push(E820Entry {
            base: one_mb,
            length: ram_size - one_mb,
            typ: E820Entry::TYPE_RAM,
        });
    }

    entries
}

