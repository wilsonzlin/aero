use crate::InvpcidType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageSize {
    Size4K,
    Size2M,
    Size4M,
    Size1G,
}

impl PageSize {
    #[inline]
    pub(crate) const fn bytes(self) -> u64 {
        match self {
            PageSize::Size4K => 4 * 1024,
            PageSize::Size2M => 2 * 1024 * 1024,
            PageSize::Size4M => 4 * 1024 * 1024,
            PageSize::Size1G => 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TlbEntry {
    vbase: u64,
    pbase: u64,
    page_size: PageSize,
    pub(crate) user: bool,
    pub(crate) writable: bool,
    pub(crate) nx: bool,
    pub(crate) global: bool,
    /// Physical address of the leaf paging-structure entry (PTE/PDE/PDPTE).
    pub(crate) leaf_addr: u64,
    /// `true` for PAE/long-mode entries (64-bit), `false` for legacy 32-bit entries.
    pub(crate) leaf_is_64: bool,
    /// Cached state of the leaf dirty bit. Used to lazily set D on write hits.
    pub(crate) dirty: bool,
    pcid: u16,
    valid: bool,
}

impl Default for TlbEntry {
    fn default() -> Self {
        Self {
            vbase: 0,
            pbase: 0,
            page_size: PageSize::Size4K,
            user: false,
            writable: false,
            nx: false,
            global: false,
            leaf_addr: 0,
            leaf_is_64: true,
            dirty: false,
            pcid: 0,
            valid: false,
        }
    }
}

impl TlbEntry {
    pub(crate) fn new(
        vbase: u64,
        pbase: u64,
        page_size: PageSize,
        user: bool,
        writable: bool,
        nx: bool,
        global: bool,
        pcid: u16,
        leaf_addr: u64,
        leaf_is_64: bool,
        dirty: bool,
    ) -> Self {
        Self {
            vbase,
            pbase,
            page_size,
            user,
            writable,
            nx,
            global,
            leaf_addr,
            leaf_is_64,
            dirty,
            pcid,
            valid: true,
        }
    }

    #[inline]
    pub(crate) fn translate(&self, vaddr: u64) -> u64 {
        debug_assert!(vaddr >= self.vbase);
        let offset = vaddr - self.vbase;
        self.pbase + offset
    }

    #[inline]
    fn matches(&self, vaddr: u64, pcid: u16) -> bool {
        if !self.valid {
            return false;
        }
        if !self.global && self.pcid != pcid {
            return false;
        }
        let mask = self.page_size.bytes() - 1;
        (vaddr & !mask) == self.vbase
    }
}

const WAYS: usize = 4;
const SETS: usize = 64; // 256 entries per bank, 4-way set associative.

#[derive(Debug, Clone)]
struct TlbSet {
    entries: [[TlbEntry; WAYS]; SETS],
    next_way: [u8; SETS],
}

impl TlbSet {
    fn new() -> Self {
        Self {
            entries: [[TlbEntry::default(); WAYS]; SETS],
            next_way: [0; SETS],
        }
    }

    fn lookup(&self, vaddr: u64, pcid: u16) -> Option<&TlbEntry> {
        // Try larger pages first so we don't miss a large-page entry due to
        // indexing differences.
        for page_size in [PageSize::Size1G, PageSize::Size4M, PageSize::Size2M, PageSize::Size4K] {
            let vbase = vaddr & !(page_size.bytes() - 1);
            let tag = vbase >> 12;
            let set = set_index(tag);
            for way in 0..WAYS {
                let entry = &self.entries[set][way];
                if entry.page_size == page_size && entry.vbase == vbase && entry.matches(vaddr, pcid) {
                    return Some(entry);
                }
            }
        }
        None
    }

    fn insert(&mut self, entry: TlbEntry) {
        let tag = entry.vbase >> 12;
        let set = set_index(tag);

        // Replace existing entry if present.
        for way in 0..WAYS {
            let cur = &mut self.entries[set][way];
            if cur.valid
                && cur.vbase == entry.vbase
                && cur.page_size == entry.page_size
                && (cur.global || cur.pcid == entry.pcid)
            {
                *cur = entry;
                return;
            }
        }

        let way = self.next_way[set] as usize % WAYS;
        self.next_way[set] = self.next_way[set].wrapping_add(1);
        self.entries[set][way] = entry;
    }

    fn invalidate_address(&mut self, vaddr: u64) {
        for page_size in [PageSize::Size1G, PageSize::Size4M, PageSize::Size2M, PageSize::Size4K] {
            let vbase = vaddr & !(page_size.bytes() - 1);
            let tag = vbase >> 12;
            let set = set_index(tag);
            for way in 0..WAYS {
                let entry = &mut self.entries[set][way];
                if entry.valid && entry.vbase == vbase && entry.page_size == page_size {
                    entry.valid = false;
                }
            }
        }
    }

    fn flush_all(&mut self) {
        for set in 0..SETS {
            for way in 0..WAYS {
                self.entries[set][way].valid = false;
            }
        }
    }

    fn flush_non_global(&mut self) {
        for set in 0..SETS {
            for way in 0..WAYS {
                let entry = &mut self.entries[set][way];
                if entry.valid && !entry.global {
                    entry.valid = false;
                }
            }
        }
    }

    fn flush_pcid(&mut self, pcid: u16, include_global: bool) {
        for set in 0..SETS {
            for way in 0..WAYS {
                let entry = &mut self.entries[set][way];
                if !entry.valid {
                    continue;
                }
                if entry.global {
                    if include_global {
                        entry.valid = false;
                    }
                    continue;
                }
                if entry.pcid == pcid {
                    entry.valid = false;
                }
            }
        }
    }

    fn set_dirty(&mut self, vaddr: u64, pcid: u16) -> bool {
        for page_size in [PageSize::Size1G, PageSize::Size4M, PageSize::Size2M, PageSize::Size4K] {
            let vbase = vaddr & !(page_size.bytes() - 1);
            let tag = vbase >> 12;
            let set = set_index(tag);
            for way in 0..WAYS {
                let entry = &mut self.entries[set][way];
                if entry.page_size == page_size && entry.vbase == vbase && entry.matches(vaddr, pcid) {
                    entry.dirty = true;
                    return true;
                }
            }
        }
        false
    }
}

#[inline]
fn set_index(tag: u64) -> usize {
    // Simple xor folding; tag already has the page-size alignment baked in.
    let x = tag ^ (tag >> 17) ^ (tag >> 35);
    (x as usize) & (SETS - 1)
}

#[derive(Debug, Clone)]
pub(crate) struct Tlb {
    itlb: TlbSet,
    dtlb: TlbSet,
}

impl Tlb {
    pub(crate) fn new() -> Self {
        Self {
            itlb: TlbSet::new(),
            dtlb: TlbSet::new(),
        }
    }

    #[inline]
    pub(crate) fn lookup(&self, vaddr: u64, is_exec: bool, pcid: u16) -> Option<&TlbEntry> {
        if is_exec {
            self.itlb.lookup(vaddr, pcid)
        } else {
            self.dtlb.lookup(vaddr, pcid)
        }
    }

    #[inline]
    pub(crate) fn insert(&mut self, is_exec: bool, entry: TlbEntry) {
        if is_exec {
            self.itlb.insert(entry);
        } else {
            self.dtlb.insert(entry);
        }
    }

    pub(crate) fn invalidate_address(&mut self, vaddr: u64) {
        self.itlb.invalidate_address(vaddr);
        self.dtlb.invalidate_address(vaddr);
    }

    pub(crate) fn set_dirty(&mut self, vaddr: u64, is_exec: bool, pcid: u16) -> bool {
        if is_exec {
            self.itlb.set_dirty(vaddr, pcid)
        } else {
            self.dtlb.set_dirty(vaddr, pcid)
        }
    }

    pub(crate) fn flush_all(&mut self) {
        self.itlb.flush_all();
        self.dtlb.flush_all();
    }

    pub(crate) fn on_cr3_write(&mut self, pge: bool, pcid_enabled: bool, new_pcid: u16, no_flush: bool) {
        if pcid_enabled {
            // If CR3[63] (no-flush) is clear, invalidate entries for the PCID
            // being loaded. (Entries for other PCIDs remain, and global entries
            // remain regardless.)
            if !no_flush {
                self.itlb.flush_pcid(new_pcid, false);
                self.dtlb.flush_pcid(new_pcid, false);
            }
            return;
        }

        if pge {
            self.itlb.flush_non_global();
            self.dtlb.flush_non_global();
        } else {
            self.flush_all();
        }
    }

    pub(crate) fn invpcid(&mut self, pcid: u16, kind: InvpcidType) {
        match kind {
            InvpcidType::IndividualAddress(vaddr) => {
                // We don't store per-entry addresses beyond base, so just use the
                // standard invalidation.
                let _ = pcid;
                self.invalidate_address(vaddr);
            }
            InvpcidType::SingleContext => {
                self.itlb.flush_pcid(pcid, false);
                self.dtlb.flush_pcid(pcid, false);
            }
            InvpcidType::AllIncludingGlobal => self.flush_all(),
            InvpcidType::AllExcludingGlobal => {
                self.itlb.flush_non_global();
                self.dtlb.flush_non_global();
            }
        }
    }
}
