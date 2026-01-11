#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageSize {
    Size4K,
    Size2M,
    Size4M,
    Size1G,
}

impl PageSize {
    pub const fn shift(self) -> u32 {
        match self {
            Self::Size4K => 12,
            Self::Size2M => 21,
            Self::Size4M => 22,
            Self::Size1G => 30,
        }
    }

    pub const fn size(self) -> u64 {
        1u64 << self.shift()
    }

    pub const fn mask(self) -> u64 {
        self.size() - 1
    }

    pub const fn align_down(self, addr: u64) -> u64 {
        addr & !self.mask()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TlbEntry {
    pub vbase: u64,
    pub pbase: u64,
    pub page_size: PageSize,
    pub writable: bool,
    pub user: bool,
    pub nx: bool,
    pub global: bool,
}

impl TlbEntry {
    pub fn contains(&self, vaddr: u64) -> bool {
        self.page_size.align_down(vaddr) == self.vbase
    }

    pub fn translate(&self, vaddr: u64) -> u64 {
        self.pbase | (vaddr & self.page_size.mask())
    }
}

#[derive(Default, Debug)]
pub struct Tlb {
    entries: Vec<TlbEntry>,
}

impl Tlb {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn lookup(&self, vaddr: u64) -> Option<TlbEntry> {
        self.entries
            .iter()
            .copied()
            .find(|entry| entry.contains(vaddr))
    }

    pub fn insert(&mut self, entry: TlbEntry) {
        self.entries.retain(|existing| {
            !(existing.vbase == entry.vbase && existing.page_size == entry.page_size)
        });
        self.entries.push(entry);
    }

    pub fn invalidate_page(&mut self, vaddr: u64) {
        self.entries.retain(|entry| !entry.contains(vaddr));
    }

    pub fn flush_on_cr3_write(&mut self, pge_enabled: bool) {
        if pge_enabled {
            self.entries.retain(|entry| entry.global);
        } else {
            self.entries.clear();
        }
    }
}
