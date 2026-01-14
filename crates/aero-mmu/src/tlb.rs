use crate::InvpcidType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
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
#[repr(u8)]
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
    /// Cached `pbase - vbase` (wrapping) so translation can use `vaddr + delta`
    /// (wrapping) instead of `pbase + (vaddr - vbase)`.
    paddr_delta: u64,
    /// Physical address of the leaf paging-structure entry (PTE/PDE/PDPTE).
    pub(crate) leaf_addr: u64,
    pcid: u16,
    flags: u8,
    page_size: PageSize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TlbHit<'a> {
    pub(crate) entry: &'a TlbEntry,
    set: u8,
    way: u8,
}

impl<'a> TlbHit<'a> {
    #[inline]
    pub(crate) fn set(&self) -> usize {
        self.set as usize
    }

    #[inline]
    pub(crate) fn way(&self) -> usize {
        self.way as usize
    }
}

const FLAG_USER: u8 = 1 << 0;
const FLAG_WRITABLE: u8 = 1 << 1;
const FLAG_NX: u8 = 1 << 2;
const FLAG_GLOBAL: u8 = 1 << 3;
const FLAG_LEAF_64: u8 = 1 << 4;
const FLAG_DIRTY: u8 = 1 << 5;
const FLAG_VALID: u8 = 1 << 6;

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
            paddr_delta: 0,
            leaf_addr: 0,
            pcid: 0,
            flags: 0,
            page_size: PageSize::Size4K,
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
        let mut flags = FLAG_VALID;
        if user {
            flags |= FLAG_USER;
        }
        if writable {
            flags |= FLAG_WRITABLE;
        }
        if nx {
            flags |= FLAG_NX;
        }
        if global {
            flags |= FLAG_GLOBAL;
        }
        if leaf_is_64 {
            flags |= FLAG_LEAF_64;
        }
        if dirty {
            flags |= FLAG_DIRTY;
        }
        Self {
            vbase,
            paddr_delta: pbase.wrapping_sub(vbase),
            leaf_addr,
            pcid,
            flags,
            page_size,
        }
    }

    #[inline]
    pub(crate) fn translate(&self, vaddr: u64) -> u64 {
        debug_assert!(vaddr >= self.vbase);
        vaddr.wrapping_add(self.paddr_delta)
    }

    #[inline]
    pub(crate) fn user(&self) -> bool {
        self.flags & FLAG_USER != 0
    }

    #[inline]
    pub(crate) fn writable(&self) -> bool {
        self.flags & FLAG_WRITABLE != 0
    }

    #[inline]
    pub(crate) fn nx(&self) -> bool {
        self.flags & FLAG_NX != 0
    }

    #[inline]
    fn global(&self) -> bool {
        self.flags & FLAG_GLOBAL != 0
    }

    #[inline]
    pub(crate) fn leaf_is_64(&self) -> bool {
        self.flags & FLAG_LEAF_64 != 0
    }

    #[inline]
    pub(crate) fn dirty(&self) -> bool {
        self.flags & FLAG_DIRTY != 0
    }

    #[inline]
    fn valid(&self) -> bool {
        self.flags & FLAG_VALID != 0
    }

    #[inline]
    fn matches_pcid<const PCID_ENABLED: bool>(&self, pcid: u16) -> bool {
        let flags = self.flags;
        if PCID_ENABLED {
            flags & FLAG_VALID != 0 && (flags & FLAG_GLOBAL != 0 || self.pcid == pcid)
        } else {
            flags & FLAG_VALID != 0
        }
    }

    #[inline]
    fn invalidate(&mut self) {
        self.flags &= !FLAG_VALID;
    }

    #[inline]
    fn set_dirty(&mut self) {
        self.flags |= FLAG_DIRTY;
    }
}

const WAYS: usize = 4;
const SETS: usize = 64; // 256 entries per bank, 4-way set associative.

#[derive(Debug, Clone)]
struct TlbSet {
    entries: [[TlbEntry; WAYS]; SETS],
    next_way: [u8; SETS],
    // Counts of valid large-page entries currently present in the set.
    //
    // When all are 0, `lookup` can take a fast 4KiB-only path that skips
    // large-page probes and the per-way page-size compare.
    count_1g: u16,
    count_4m: u16,
    count_2m: u16,
}

impl TlbSet {
    fn new() -> Self {
        Self {
            entries: [[TlbEntry::default(); WAYS]; SETS],
            next_way: [0; SETS],
            count_1g: 0,
            count_4m: 0,
            count_2m: 0,
        }
    }

    #[inline]
    fn lookup(
        &self,
        vaddr: u64,
        pcid: u16,
        pcid_enabled: bool,
        page_sizes: TlbLookupPageSizes,
    ) -> Option<TlbHit<'_>> {
        if pcid_enabled {
            self.lookup_impl::<true>(vaddr, pcid, page_sizes)
        } else {
            self.lookup_impl::<false>(vaddr, pcid, page_sizes)
        }
    }

    #[inline]
    fn lookup_impl<const PCID_ENABLED: bool>(
        &self,
        vaddr: u64,
        pcid: u16,
        page_sizes: TlbLookupPageSizes,
    ) -> Option<TlbHit<'_>> {
        // Common case: TLB contains only 4KiB entries (no large pages have been
        // inserted since the last full flush), so we can skip page-size probes
        // and the page-size compare in the way loop.
        if (self.count_1g | self.count_2m | self.count_4m) == 0 {
            let vbase = vaddr & !0xfff;
            let tag = vaddr >> 12;
            let set = set_index(tag);
            for way in 0..WAYS {
                let entry = &self.entries[set][way];
                if entry.vbase == vbase && entry.matches_pcid::<PCID_ENABLED>(pcid) {
                    debug_assert_eq!(entry.page_size, PageSize::Size4K);
                    return Some(TlbHit {
                        entry,
                        set: set as u8,
                        way: way as u8,
                    });
                }
            }
            return None;
        }

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
                        && entry.matches_pcid::<PCID_ENABLED>(pcid)
                    {
                        return Some(TlbHit {
                            entry,
                            set: set as u8,
                            way: way as u8,
                        });
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
                if self.count_2m != 0 {
                    lookup_page_size!(PageSize::Size2M);
                }
                lookup_page_size!(PageSize::Size4K);
            }
            TlbLookupPageSizes::Size4MAnd4K => {
                if self.count_4m != 0 {
                    lookup_page_size!(PageSize::Size4M);
                }
                lookup_page_size!(PageSize::Size4K);
            }
            TlbLookupPageSizes::Size1G2MAnd4K => {
                if self.count_1g != 0 {
                    lookup_page_size!(PageSize::Size1G);
                }
                if self.count_2m != 0 {
                    lookup_page_size!(PageSize::Size2M);
                }
                lookup_page_size!(PageSize::Size4K);
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
            if cur.valid()
                && cur.vbase == entry.vbase
                && cur.page_size == entry.page_size
                && (cur.global() || cur.pcid == entry.pcid)
            {
                *cur = entry;
                return;
            }
        }

        let way = self.next_way[set] as usize % WAYS;
        self.next_way[set] = self.next_way[set].wrapping_add(1);

        let old = self.entries[set][way];
        if old.valid() {
            match old.page_size {
                PageSize::Size1G => {
                    debug_assert!(self.count_1g > 0);
                    self.count_1g -= 1;
                }
                PageSize::Size4M => {
                    debug_assert!(self.count_4m > 0);
                    self.count_4m -= 1;
                }
                PageSize::Size2M => {
                    debug_assert!(self.count_2m > 0);
                    self.count_2m -= 1;
                }
                PageSize::Size4K => {}
            }
        }

        self.entries[set][way] = entry;

        match entry.page_size {
            PageSize::Size1G => self.count_1g += 1,
            PageSize::Size4M => self.count_4m += 1,
            PageSize::Size2M => self.count_2m += 1,
            PageSize::Size4K => {}
        }
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
                if entry.valid() && entry.vbase == vbase && entry.page_size == page_size {
                    entry.invalidate();
                    match page_size {
                        PageSize::Size1G => {
                            debug_assert!(self.count_1g > 0);
                            self.count_1g -= 1;
                        }
                        PageSize::Size4M => {
                            debug_assert!(self.count_4m > 0);
                            self.count_4m -= 1;
                        }
                        PageSize::Size2M => {
                            debug_assert!(self.count_2m > 0);
                            self.count_2m -= 1;
                        }
                        PageSize::Size4K => {}
                    }
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
                if !entry.valid() || entry.page_size != page_size || entry.vbase != vbase {
                    continue;
                }
                if entry.global() {
                    if include_global {
                        entry.invalidate();
                        match page_size {
                            PageSize::Size1G => {
                                debug_assert!(self.count_1g > 0);
                                self.count_1g -= 1;
                            }
                            PageSize::Size4M => {
                                debug_assert!(self.count_4m > 0);
                                self.count_4m -= 1;
                            }
                            PageSize::Size2M => {
                                debug_assert!(self.count_2m > 0);
                                self.count_2m -= 1;
                            }
                            PageSize::Size4K => {}
                        }
                    }
                    continue;
                }
                if entry.pcid == pcid {
                    entry.invalidate();
                    match page_size {
                        PageSize::Size1G => {
                            debug_assert!(self.count_1g > 0);
                            self.count_1g -= 1;
                        }
                        PageSize::Size4M => {
                            debug_assert!(self.count_4m > 0);
                            self.count_4m -= 1;
                        }
                        PageSize::Size2M => {
                            debug_assert!(self.count_2m > 0);
                            self.count_2m -= 1;
                        }
                        PageSize::Size4K => {}
                    }
                }
            }
        }
    }

    fn flush_all(&mut self) {
        self.count_1g = 0;
        self.count_4m = 0;
        self.count_2m = 0;
        for set in 0..SETS {
            for way in 0..WAYS {
                self.entries[set][way].invalidate();
            }
        }
    }

    fn flush_non_global(&mut self) {
        self.count_1g = 0;
        self.count_4m = 0;
        self.count_2m = 0;
        for set in 0..SETS {
            for way in 0..WAYS {
                let entry = &mut self.entries[set][way];
                if entry.valid() && !entry.global() {
                    entry.invalidate();
                    continue;
                }
                if entry.valid() {
                    match entry.page_size {
                        PageSize::Size1G => self.count_1g += 1,
                        PageSize::Size4M => self.count_4m += 1,
                        PageSize::Size2M => self.count_2m += 1,
                        PageSize::Size4K => {}
                    }
                }
            }
        }
    }

    fn flush_pcid(&mut self, pcid: u16, include_global: bool) {
        self.count_1g = 0;
        self.count_4m = 0;
        self.count_2m = 0;
        for set in 0..SETS {
            for way in 0..WAYS {
                let entry = &mut self.entries[set][way];
                if !entry.valid() {
                    continue;
                }
                if entry.global() {
                    if include_global {
                        entry.invalidate();
                        continue;
                    }
                } else if entry.pcid == pcid {
                    entry.invalidate();
                    continue;
                }
                match entry.page_size {
                    PageSize::Size1G => self.count_1g += 1,
                    PageSize::Size4M => self.count_4m += 1,
                    PageSize::Size2M => self.count_2m += 1,
                    PageSize::Size4K => {}
                }
            }
        }
    }

    #[inline]
    fn set_dirty_hit(&mut self, set: usize, way: usize) {
        let entry = &mut self.entries[set][way];
        debug_assert!(entry.valid());
        entry.set_dirty();
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
        pcid_enabled: bool,
        page_sizes: TlbLookupPageSizes,
    ) -> Option<TlbHit<'_>> {
        if is_exec {
            self.itlb.lookup(vaddr, pcid, pcid_enabled, page_sizes)
        } else {
            self.dtlb.lookup(vaddr, pcid, pcid_enabled, page_sizes)
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
    pub(crate) fn set_dirty_slot(&mut self, set: usize, way: usize) {
        self.dtlb.set_dirty_hit(set, way);
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
