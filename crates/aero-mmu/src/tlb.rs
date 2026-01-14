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

/// Which page sizes may exist in the TLB for the current paging mode.
///
/// This allows the hottest TLB paths (`lookup` / `set_dirty`) to avoid scanning
/// page sizes that cannot occur (e.g. 4MiB pages in long mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TlbLookupPageSizes {
    /// Only 4KiB pages are possible.
    Only4K,
    /// 2MiB or 4KiB pages are possible.
    Size2MAnd4K,
    /// 4MiB or 4KiB pages are possible.
    Size4MAnd4K,
    /// 1GiB, 2MiB, or 4KiB pages are possible.
    Size1G2MAnd4K,
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

#[derive(Debug, Clone, Copy)]
pub(crate) struct TlbEntryAttributes {
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        vbase: u64,
        pbase: u64,
        page_size: PageSize,
        pcid: u16,
        attrs: TlbEntryAttributes,
    ) -> Self {
        let TlbEntryAttributes {
            user,
            writable,
            nx,
            global,
            leaf_addr,
            leaf_is_64,
            dirty,
        } = attrs;
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
    pub(crate) fn vbase(&self) -> u64 {
        self.vbase
    }

    #[inline]
    pub(crate) fn page_size(&self) -> PageSize {
        self.page_size
    }

    #[inline]
    fn matches_pcid(&self, pcid: u16) -> bool {
        self.valid && (self.global || self.pcid == pcid)
    }
}

const WAYS: usize = 4;
const SETS: usize = 64; // 256 entries per bank, 4-way set associative.

#[derive(Debug, Clone)]
struct TlbSet {
    entries: [[TlbEntry; WAYS]; SETS],
    next_way: [u8; SETS],
    has_1g: bool,
    has_4m: bool,
    has_2m: bool,
}

impl TlbSet {
    fn new() -> Self {
        Self {
            entries: [[TlbEntry::default(); WAYS]; SETS],
            next_way: [0; SETS],
            has_1g: false,
            has_4m: false,
            has_2m: false,
        }
    }

    #[inline]
    fn lookup(&self, vaddr: u64, pcid: u16, page_sizes: TlbLookupPageSizes) -> Option<&TlbEntry> {
        macro_rules! lookup_page_size {
            ($page_size:expr) => {{
                let page_size = $page_size;
                let vbase = vaddr & !(page_size.bytes() - 1);
                let tag = vbase >> 12;
                let set = set_index(tag);
                for way in 0..WAYS {
                    let entry = &self.entries[set][way];
                    if entry.page_size == page_size
                        && entry.vbase == vbase
                        && entry.matches_pcid(pcid)
                    {
                        return Some(entry);
                    }
                }
            }};
        }

        // Try larger pages first so we don't miss a large-page entry due to
        // indexing differences.
        match page_sizes {
            TlbLookupPageSizes::Only4K => {
                lookup_page_size!(PageSize::Size4K);
            }
            TlbLookupPageSizes::Size2MAnd4K => {
                if self.has_2m {
                    lookup_page_size!(PageSize::Size2M);
                }
                lookup_page_size!(PageSize::Size4K);
            }
            TlbLookupPageSizes::Size4MAnd4K => {
                if self.has_4m {
                    lookup_page_size!(PageSize::Size4M);
                }
                lookup_page_size!(PageSize::Size4K);
            }
            TlbLookupPageSizes::Size1G2MAnd4K => {
                if self.has_1g {
                    lookup_page_size!(PageSize::Size1G);
                }
                if self.has_2m {
                    lookup_page_size!(PageSize::Size2M);
                }
                lookup_page_size!(PageSize::Size4K);
            }
        }

        None
    }

    fn insert(&mut self, entry: TlbEntry) {
        // Track which page sizes exist in the set so lookup can skip scanning
        // sizes that haven't been inserted yet.
        match entry.page_size {
            PageSize::Size1G => self.has_1g = true,
            PageSize::Size4M => self.has_4m = true,
            PageSize::Size2M => self.has_2m = true,
            PageSize::Size4K => {}
        }

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

    fn invalidate_address_all(&mut self, vaddr: u64) {
        for page_size in [
            PageSize::Size1G,
            PageSize::Size4M,
            PageSize::Size2M,
            PageSize::Size4K,
        ] {
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

    fn invalidate_address_pcid(&mut self, vaddr: u64, pcid: u16, include_global: bool) {
        for page_size in [
            PageSize::Size1G,
            PageSize::Size4M,
            PageSize::Size2M,
            PageSize::Size4K,
        ] {
            let vbase = vaddr & !(page_size.bytes() - 1);
            let tag = vbase >> 12;
            let set = set_index(tag);
            for way in 0..WAYS {
                let entry = &mut self.entries[set][way];
                if !entry.valid || entry.page_size != page_size || entry.vbase != vbase {
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

    fn flush_all(&mut self) {
        self.has_1g = false;
        self.has_4m = false;
        self.has_2m = false;
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

    #[inline]
    fn set_dirty_known(&mut self, vbase: u64, page_size: PageSize, pcid: u16) -> bool {
        let tag = vbase >> 12;
        let set = set_index(tag);
        for way in 0..WAYS {
            let entry = &mut self.entries[set][way];
            if entry.page_size == page_size && entry.vbase == vbase && entry.matches_pcid(pcid) {
                entry.dirty = true;
                return true;
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
    pub(crate) fn lookup(
        &self,
        vaddr: u64,
        is_exec: bool,
        pcid: u16,
        page_sizes: TlbLookupPageSizes,
    ) -> Option<&TlbEntry> {
        if is_exec {
            self.itlb.lookup(vaddr, pcid, page_sizes)
        } else {
            self.dtlb.lookup(vaddr, pcid, page_sizes)
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

    pub(crate) fn invalidate_address_all(&mut self, vaddr: u64) {
        self.itlb.invalidate_address_all(vaddr);
        self.dtlb.invalidate_address_all(vaddr);
    }

    pub(crate) fn invalidate_address_pcid(&mut self, vaddr: u64, pcid: u16, include_global: bool) {
        self.itlb
            .invalidate_address_pcid(vaddr, pcid, include_global);
        self.dtlb
            .invalidate_address_pcid(vaddr, pcid, include_global);
    }

    #[inline]
    pub(crate) fn set_dirty(&mut self, vbase: u64, page_size: PageSize, is_exec: bool, pcid: u16) {
        if is_exec {
            let _ = self.itlb.set_dirty_known(vbase, page_size, pcid);
        } else {
            let _ = self.dtlb.set_dirty_known(vbase, page_size, pcid);
        }
    }

    pub(crate) fn flush_all(&mut self) {
        self.itlb.flush_all();
        self.dtlb.flush_all();
    }

    pub(crate) fn on_cr3_write(
        &mut self,
        pge: bool,
        pcid_enabled: bool,
        new_pcid: u16,
        no_flush: bool,
    ) {
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
                self.invalidate_address_pcid(vaddr, pcid, false);
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
