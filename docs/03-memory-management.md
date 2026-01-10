# 03 - Memory Management Unit

## Overview

Memory management is critical for both correctness and performance. Windows 7 heavily uses paging, and the MMU must accurately emulate address translation while maintaining reasonable speed through TLB caching.

---

## x86-64 Paging Modes

### Paging Evolution

| Mode | CR0.PG | CR4.PAE | EFER.LME | Page Size | Virtual Address |
|------|--------|---------|----------|-----------|-----------------|
| No Paging | 0 | - | - | - | Physical = Virtual |
| 32-bit | 1 | 0 | 0 | 4KB/4MB | 32-bit |
| PAE | 1 | 1 | 0 | 4KB/2MB | 32-bit |
| Long Mode (4-level) | 1 | 1 | 1 | 4KB/2MB/1GB | 48-bit |
| Long Mode (5-level) | 1 | 1 | 1 | 4KB/2MB/1GB | 57-bit |

Windows 7 64-bit uses **4-level paging** (48-bit virtual addresses).

### 4-Level Page Table Structure

```
┌─────────────────────────────────────────────────────────────────┐
│            64-bit Virtual Address (48 bits used)                 │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  63    48 47    39 38    30 29    21 20    12 11        0       │
│  ┌──────┬────────┬────────┬────────┬────────┬───────────┐       │
│  │ Sign │  PML4  │  PDPT  │   PD   │   PT   │  Offset   │       │
│  │ Ext  │ Index  │ Index  │ Index  │ Index  │ (4KB)     │       │
│  │(16b) │ (9b)   │ (9b)   │ (9b)   │ (9b)   │ (12b)     │       │
│  └──────┴────────┴────────┴────────┴────────┴───────────┘       │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                    Page Table Walk                               │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  CR3 (PML4 Base)                                                │
│       │                                                          │
│       ▼                                                          │
│  ┌─────────────┐                                                │
│  │    PML4     │◀── PML4 Index (bits 47:39)                     │
│  │   (512)     │                                                │
│  └──────┬──────┘                                                │
│         │ PML4E                                                  │
│         ▼                                                        │
│  ┌─────────────┐                                                │
│  │    PDPT     │◀── PDPT Index (bits 38:30)                     │
│  │   (512)     │                                                │
│  └──────┬──────┘                                                │
│         │ PDPTE (may be 1GB page if PS=1)                       │
│         ▼                                                        │
│  ┌─────────────┐                                                │
│  │     PD      │◀── PD Index (bits 29:21)                       │
│  │   (512)     │                                                │
│  └──────┬──────┘                                                │
│         │ PDE (may be 2MB page if PS=1)                         │
│         ▼                                                        │
│  ┌─────────────┐                                                │
│  │     PT      │◀── PT Index (bits 20:12)                       │
│  │   (512)     │                                                │
│  └──────┬──────┘                                                │
│         │ PTE                                                    │
│         ▼                                                        │
│  ┌─────────────┐                                                │
│  │Physical Page│◀── Page Offset (bits 11:0)                     │
│  │   (4KB)     │                                                │
│  └─────────────┘                                                │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

### Page Table Entry Format (64-bit)

```
┌─────────────────────────────────────────────────────────────────┐
│                    Page Table Entry (PTE)                        │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  63 62  59 58 52 51            12 11  9 8 7 6 5 4 3 2 1 0       │
│  ┌──┬─────┬─────┬────────────────┬─────┬─┬─┬─┬─┬─┬─┬─┬─┬─┐      │
│  │XD│ Ign │Prot │  Physical Addr │Avail│G│0│D│A│C│T│U│W│P│      │
│  │  │ Key │ Key │   (40 bits)    │     │ │ │ │ │D│W│/│/│ │      │
│  │  │     │     │                │     │ │ │ │ │ │ │S│R│ │      │
│  └──┴─────┴─────┴────────────────┴─────┴─┴─┴─┴─┴─┴─┴─┴─┴─┘      │
│                                                                  │
│  Bit   Name         Description                                  │
│  ───   ────         ───────────                                  │
│  0     P (Present)  Page is present in memory                    │
│  1     R/W          0=Read-only, 1=Read/Write                    │
│  2     U/S          0=Supervisor, 1=User                         │
│  3     PWT          Page Write-Through                           │
│  4     PCD          Page Cache Disable                           │
│  5     A (Accessed) Page has been read                           │
│  6     D (Dirty)    Page has been written                        │
│  7     PS/PAT       Page Size (PDE) or PAT (PTE)                 │
│  8     G (Global)   Global page (not flushed on CR3 switch)      │
│  63    XD           Execute Disable (NX bit)                     │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## MMU Implementation

### Core Translation Logic

```rust
pub struct Mmu {
    tlb: Tlb,
    cr3: u64,
    cr0: u64,
    cr4: u64,
    efer: u64,
}

impl Mmu {
    pub fn translate(&mut self, vaddr: u64, access: AccessType) -> Result<PhysAddr, PageFault> {
        // Check if paging is enabled
        if !self.paging_enabled() {
            return Ok(PhysAddr(vaddr));  // Identity mapping
        }
        
        // TLB lookup first
        if let Some(entry) = self.tlb.lookup(vaddr, access) {
            return Ok(entry.physical_address(vaddr));
        }
        
        // Page table walk
        let entry = self.walk_page_tables(vaddr, access)?;
        
        // Update TLB
        self.tlb.insert(vaddr, entry);
        
        Ok(entry.physical_address(vaddr))
    }
    
    fn walk_page_tables(&mut self, vaddr: u64, access: AccessType) -> Result<TlbEntry, PageFault> {
        let pml4_base = self.cr3 & PML4_ADDR_MASK;
        
        // Level 4: PML4
        let pml4_index = (vaddr >> 39) & 0x1FF;
        let pml4e_addr = pml4_base + pml4_index * 8;
        let pml4e = self.read_physical_u64(pml4e_addr);
        
        if !self.check_entry_present(pml4e, vaddr, access, PageFaultLevel::Pml4)? {
            return Err(PageFault::new(vaddr, access, PageFaultLevel::Pml4));
        }
        
        // Level 3: PDPT
        let pdpt_base = pml4e & PAGE_ADDR_MASK;
        let pdpt_index = (vaddr >> 30) & 0x1FF;
        let pdpte_addr = pdpt_base + pdpt_index * 8;
        let pdpte = self.read_physical_u64(pdpte_addr);
        
        if !self.check_entry_present(pdpte, vaddr, access, PageFaultLevel::Pdpt)? {
            return Err(PageFault::new(vaddr, access, PageFaultLevel::Pdpt));
        }
        
        // Check for 1GB page
        if pdpte & PAGE_SIZE_BIT != 0 {
            return Ok(self.create_tlb_entry_1gb(pdpte, vaddr, access));
        }
        
        // Level 2: PD
        let pd_base = pdpte & PAGE_ADDR_MASK;
        let pd_index = (vaddr >> 21) & 0x1FF;
        let pde_addr = pd_base + pd_index * 8;
        let pde = self.read_physical_u64(pde_addr);
        
        if !self.check_entry_present(pde, vaddr, access, PageFaultLevel::Pd)? {
            return Err(PageFault::new(vaddr, access, PageFaultLevel::Pd));
        }
        
        // Check for 2MB page
        if pde & PAGE_SIZE_BIT != 0 {
            return Ok(self.create_tlb_entry_2mb(pde, vaddr, access));
        }
        
        // Level 1: PT
        let pt_base = pde & PAGE_ADDR_MASK;
        let pt_index = (vaddr >> 12) & 0x1FF;
        let pte_addr = pt_base + pt_index * 8;
        let pte = self.read_physical_u64(pte_addr);
        
        if !self.check_entry_present(pte, vaddr, access, PageFaultLevel::Pt)? {
            return Err(PageFault::new(vaddr, access, PageFaultLevel::Pt));
        }
        
        // Set accessed/dirty bits
        self.update_access_bits(pte_addr, pte, access);
        
        Ok(self.create_tlb_entry_4kb(pte, vaddr, access))
    }
    
    fn check_entry_present(&self, entry: u64, vaddr: u64, access: AccessType, level: PageFaultLevel) -> Result<bool, PageFault> {
        // Check present bit
        if entry & PRESENT_BIT == 0 {
            return Err(PageFault::not_present(vaddr, access, level));
        }
        
        // Check permissions
        let is_write = access == AccessType::Write;
        let is_user = self.current_privilege_level() == 3;
        let is_execute = access == AccessType::Execute;
        
        // Write permission
        if is_write && entry & WRITABLE_BIT == 0 {
            // Check if WP bit in CR0 is set (write protect)
            if self.cr0 & CR0_WP != 0 || is_user {
                return Err(PageFault::protection(vaddr, access, level));
            }
        }
        
        // User permission
        if is_user && entry & USER_BIT == 0 {
            return Err(PageFault::protection(vaddr, access, level));
        }
        
        // Execute permission (NX bit)
        if is_execute && self.nx_enabled() && entry & NX_BIT != 0 {
            return Err(PageFault::protection(vaddr, access, level));
        }
        
        Ok(true)
    }
}
```

---

## TLB (Translation Lookaside Buffer)

### TLB Structure

```rust
pub struct Tlb {
    // Separate TLBs for different page sizes (like real hardware)
    itlb_4kb: TlbSet<64>,   // Instruction TLB, 4KB pages
    itlb_large: TlbSet<32>, // Instruction TLB, 2MB/1GB pages
    dtlb_4kb: TlbSet<64>,   // Data TLB, 4KB pages
    dtlb_large: TlbSet<32>, // Data TLB, 2MB/1GB pages
    
    // Second-level TLB (unified)
    stlb: TlbSet<512>,
    
    // Global entries (not flushed on CR3 switch)
    global_entries: HashMap<u64, TlbEntry>,
    
    // PCID support
    current_pcid: u16,
}

#[derive(Clone, Copy)]
pub struct TlbEntry {
    virtual_page: u64,      // Virtual page number
    physical_page: u64,     // Physical page number
    permissions: TlbPerms,  // R/W/X permissions
    page_size: PageSize,    // 4KB, 2MB, or 1GB
    global: bool,           // Global page flag
    pcid: u16,              // Process Context ID
    valid: bool,
}

pub struct TlbSet<const N: usize> {
    entries: [TlbEntry; N],
    // 4-way set associative
    ways: usize,
}

impl<const N: usize> TlbSet<N> {
    pub fn lookup(&self, vaddr: u64, access: AccessType) -> Option<&TlbEntry> {
        let vpn = vaddr >> 12;  // Virtual page number
        let set_index = (vpn as usize) % (N / 4);
        
        // Check all ways in the set
        for way in 0..4 {
            let entry = &self.entries[set_index * 4 + way];
            if entry.valid && entry.matches(vaddr, self.current_pcid) {
                // Check permissions
                if entry.permits(access) {
                    return Some(entry);
                }
            }
        }
        
        None
    }
    
    pub fn insert(&mut self, entry: TlbEntry) {
        let vpn = entry.virtual_page;
        let set_index = (vpn as usize) % (N / 4);
        
        // Find invalid entry or LRU replacement
        let way = self.find_replacement_way(set_index);
        self.entries[set_index * 4 + way] = entry;
    }
}
```

### TLB Invalidation

```rust
impl Tlb {
    /// Invalidate single page (INVLPG instruction)
    pub fn invalidate_page(&mut self, vaddr: u64) {
        let vpn = vaddr >> 12;
        
        // Invalidate in all TLBs
        self.itlb_4kb.invalidate_vpn(vpn);
        self.itlb_large.invalidate_vpn(vpn);
        self.dtlb_4kb.invalidate_vpn(vpn);
        self.dtlb_large.invalidate_vpn(vpn);
        self.stlb.invalidate_vpn(vpn);
    }
    
    /// Flush entire TLB (CR3 write, MOV to CR3)
    pub fn flush(&mut self) {
        // Keep global entries if CR4.PGE is set
        if self.cr4_pge_enabled {
            self.invalidate_non_global();
        } else {
            self.invalidate_all();
        }
    }
    
    /// Flush TLB entries for specific PCID (INVPCID instruction)
    pub fn invalidate_pcid(&mut self, pcid: u16, invalidate_type: InvpcidType) {
        match invalidate_type {
            InvpcidType::IndividualAddress(vaddr) => {
                self.invalidate_page_pcid(vaddr, pcid);
            }
            InvpcidType::SingleContext => {
                self.invalidate_all_pcid(pcid);
            }
            InvpcidType::AllIncludingGlobal => {
                self.invalidate_all();
            }
            InvpcidType::AllExcludingGlobal => {
                self.invalidate_non_global();
            }
        }
    }
}
```

---

## Memory Bus Implementation

### Physical Memory Regions

```rust
pub struct MemoryBus {
    // Main RAM (guest physical memory)
    ram: SharedArrayBuffer,
    ram_size: usize,
    
    // Memory-mapped I/O regions
    mmio_regions: Vec<MmioRegion>,
    
    // ROM regions
    bios_rom: Vec<u8>,
    option_roms: Vec<OptionRom>,
}

pub struct MmioRegion {
    start: u64,
    end: u64,
    handler: Box<dyn MmioHandler>,
}

impl MemoryBus {
    pub fn read_physical(&self, paddr: u64, size: usize) -> u64 {
        // Check for MMIO first
        if let Some(region) = self.find_mmio_region(paddr) {
            return region.handler.read(paddr - region.start, size);
        }
        
        // Check for ROM
        if self.is_rom_region(paddr) {
            return self.read_rom(paddr, size);
        }
        
        // Regular RAM
        if paddr < self.ram_size as u64 {
            return self.read_ram(paddr, size);
        }
        
        // Unmapped - return all 1s
        !0u64
    }
    
    pub fn write_physical(&mut self, paddr: u64, size: usize, value: u64) {
        // Check for MMIO
        if let Some(region) = self.find_mmio_region_mut(paddr) {
            region.handler.write(paddr - region.start, size, value);
            return;
        }
        
        // ROM is read-only (ignore writes)
        if self.is_rom_region(paddr) {
            return;
        }
        
        // Regular RAM
        if paddr < self.ram_size as u64 {
            self.write_ram(paddr, size, value);
        }
    }
    
    fn read_ram(&self, paddr: u64, size: usize) -> u64 {
        let offset = paddr as usize;
        
        // Use SharedArrayBuffer views for efficient access
        match size {
            1 => self.ram_u8[offset] as u64,
            2 => u16::from_le_bytes(self.ram[offset..offset+2].try_into().unwrap()) as u64,
            4 => u32::from_le_bytes(self.ram[offset..offset+4].try_into().unwrap()) as u64,
            8 => u64::from_le_bytes(self.ram[offset..offset+8].try_into().unwrap()),
            _ => panic!("Invalid size"),
        }
    }
}
```

### MMIO Handlers

```rust
pub trait MmioHandler: Send + Sync {
    fn read(&self, offset: u64, size: usize) -> u64;
    fn write(&mut self, offset: u64, size: usize, value: u64);
}

// Example: Local APIC MMIO
pub struct LocalApicMmio {
    apic_state: ApicState,
}

impl MmioHandler for LocalApicMmio {
    fn read(&self, offset: u64, size: usize) -> u64 {
        match offset {
            APIC_ID_OFFSET => self.apic_state.id as u64,
            APIC_VERSION_OFFSET => APIC_VERSION as u64,
            APIC_TPR_OFFSET => self.apic_state.tpr as u64,
            APIC_PPR_OFFSET => self.apic_state.ppr as u64,
            APIC_EOI_OFFSET => 0,  // Write-only
            APIC_LDR_OFFSET => self.apic_state.ldr as u64,
            APIC_SVR_OFFSET => self.apic_state.svr as u64,
            APIC_ISR_BASE..=APIC_ISR_END => self.read_isr(offset),
            APIC_TMR_BASE..=APIC_TMR_END => self.read_tmr(offset),
            APIC_IRR_BASE..=APIC_IRR_END => self.read_irr(offset),
            APIC_ICR_LOW_OFFSET => self.apic_state.icr as u64,
            APIC_ICR_HIGH_OFFSET => (self.apic_state.icr >> 32) as u64,
            APIC_LVT_TIMER_OFFSET => self.apic_state.lvt_timer as u64,
            APIC_TIMER_INITIAL_OFFSET => self.apic_state.timer_initial as u64,
            APIC_TIMER_CURRENT_OFFSET => self.get_timer_current() as u64,
            APIC_TIMER_DIVIDE_OFFSET => self.apic_state.timer_divide as u64,
            _ => {
                log::warn!("APIC: Unknown read offset 0x{:x}", offset);
                0
            }
        }
    }
    
    fn write(&mut self, offset: u64, size: usize, value: u64) {
        match offset {
            APIC_TPR_OFFSET => self.apic_state.tpr = value as u32,
            APIC_EOI_OFFSET => self.handle_eoi(),
            APIC_LDR_OFFSET => self.apic_state.ldr = value as u32,
            APIC_SVR_OFFSET => {
                self.apic_state.svr = value as u32;
                if value & APIC_SVR_ENABLE == 0 {
                    self.disable_apic();
                }
            }
            APIC_ICR_LOW_OFFSET => {
                self.apic_state.icr = (self.apic_state.icr & 0xFFFFFFFF00000000) | (value as u64);
                self.send_ipi();
            }
            APIC_ICR_HIGH_OFFSET => {
                self.apic_state.icr = (self.apic_state.icr & 0xFFFFFFFF) | (value << 32);
            }
            APIC_LVT_TIMER_OFFSET => self.apic_state.lvt_timer = value as u32,
            APIC_TIMER_INITIAL_OFFSET => {
                self.apic_state.timer_initial = value as u32;
                self.start_timer();
            }
            APIC_TIMER_DIVIDE_OFFSET => self.apic_state.timer_divide = value as u32,
            _ => log::warn!("APIC: Unknown write offset 0x{:x}", offset),
        }
    }
}
```

---

## Page Fault Handling

```rust
pub struct PageFault {
    pub faulting_address: u64,  // CR2 value
    pub error_code: u32,
    pub access_type: AccessType,
}

impl PageFault {
    pub fn deliver(&self, cpu: &mut CpuState) {
        // Set CR2 to faulting address
        cpu.cr2 = self.faulting_address;
        
        // Build error code
        // Bit 0: P - Present (0 = not present, 1 = protection violation)
        // Bit 1: W/R - Write (0 = read, 1 = write)
        // Bit 2: U/S - User (0 = supervisor, 1 = user)
        // Bit 3: RSVD - Reserved bit violation
        // Bit 4: I/D - Instruction fetch
        // Bit 5: PK - Protection key violation
        // Bit 6: SS - Shadow stack
        // Bit 15: SGX - SGX violation
        
        let error_code = self.error_code;
        
        // Deliver #PF (vector 14) with error code
        cpu.raise_exception(Exception::PageFault, Some(error_code));
    }
}
```

---

## Memory Optimization Strategies

### Sparse Memory Allocation

```rust
// Don't allocate all 4GB at startup
// Use sparse allocation with on-demand page creation

pub struct SparseMemory {
    // Map 2MB chunks to actual allocations
    chunks: HashMap<u64, Box<[u8; 2 * 1024 * 1024]>>,
    total_size: u64,
}

impl SparseMemory {
    pub fn read(&mut self, paddr: u64) -> u8 {
        let chunk_addr = paddr & !((2 << 20) - 1);
        
        if let Some(chunk) = self.chunks.get(&chunk_addr) {
            let offset = (paddr - chunk_addr) as usize;
            chunk[offset]
        } else {
            // Unaccessed memory reads as 0
            0
        }
    }
    
    pub fn write(&mut self, paddr: u64, value: u8) {
        let chunk_addr = paddr & !((2 << 20) - 1);
        
        let chunk = self.chunks.entry(chunk_addr).or_insert_with(|| {
            // Allocate on first write
            Box::new([0u8; 2 * 1024 * 1024])
        });
        
        let offset = (paddr - chunk_addr) as usize;
        chunk[offset] = value;
    }
}
```

### Copy-on-Write for Disk Images

```rust
// Don't modify the base disk image
// Track writes in a separate overlay

pub struct CowDiskImage {
    base: FileHandle,           // Read-only base image
    overlay: HashMap<u64, Vec<u8>>,  // Sector -> modified data
    sector_size: usize,
}

impl CowDiskImage {
    pub fn read_sector(&self, lba: u64, buffer: &mut [u8]) {
        if let Some(data) = self.overlay.get(&lba) {
            // Return modified sector
            buffer.copy_from_slice(data);
        } else {
            // Read from base image
            self.base.read_at(lba * self.sector_size as u64, buffer);
        }
    }
    
    pub fn write_sector(&mut self, lba: u64, data: &[u8]) {
        // Write to overlay only
        self.overlay.insert(lba, data.to_vec());
    }
    
    pub fn save_overlay(&self, path: &str) {
        // Save modified sectors for persistence
    }
    
    pub fn merge_to_image(&mut self) {
        // Apply overlay to create new image
    }
}
```

---

## DMA (Direct Memory Access)

### DMA Controller Emulation

```rust
pub struct DmaController {
    channels: [DmaChannel; 8],
    page_registers: [u8; 8],
    command: u8,
    status: u8,
    mask: u8,
}

pub struct DmaChannel {
    base_address: u16,
    base_count: u16,
    current_address: u16,
    current_count: u16,
    mode: u8,
    page: u8,
}

impl DmaController {
    pub fn transfer(&mut self, channel: usize, memory: &mut MemoryBus, device_buffer: &mut [u8]) {
        let ch = &mut self.channels[channel];
        let page = self.page_registers[channel];
        
        // Calculate physical address (ISA DMA uses 24-bit addresses)
        let addr = ((page as u32) << 16) | (ch.current_address as u32);
        
        let mode = DmaMode::from(ch.mode);
        let count = ch.current_count as usize + 1;
        
        match mode.transfer_type() {
            DmaTransferType::Read => {
                // Memory to device
                for i in 0..count.min(device_buffer.len()) {
                    device_buffer[i] = memory.read_physical(addr as u64 + i as u64, 1) as u8;
                }
            }
            DmaTransferType::Write => {
                // Device to memory
                for i in 0..count.min(device_buffer.len()) {
                    memory.write_physical(addr as u64 + i as u64, 1, device_buffer[i] as u64);
                }
            }
            DmaTransferType::Verify => {
                // No actual transfer
            }
        }
        
        // Update channel state
        if mode.auto_init() {
            ch.current_address = ch.base_address;
            ch.current_count = ch.base_count;
        } else {
            ch.current_address = ch.current_address.wrapping_add(count as u16);
            ch.current_count = ch.current_count.wrapping_sub(count as u16);
        }
        
        // Set terminal count in status
        if ch.current_count == 0xFFFF {
            self.status |= 1 << channel;
        }
    }
}
```

---

## Performance Considerations

### TLB Size vs Performance

| TLB Size | L1 Hit Rate | Page Walk Cost | Recommendation |
|----------|-------------|----------------|----------------|
| 32 entries | ~85% | High | Too small |
| 64 entries | ~92% | Medium | Minimum viable |
| 128 entries | ~96% | Low | Good balance |
| 256 entries | ~98% | Very low | Recommended |
| 512+ entries | ~99% | Minimal | Memory expensive |

### Memory Access Patterns

```rust
// Batch memory accesses for better cache utilization
impl MemoryBus {
    pub fn read_batch(&self, requests: &[(u64, usize)], results: &mut [u64]) {
        // Sort by address for sequential access
        let mut sorted: Vec<_> = requests.iter().enumerate().collect();
        sorted.sort_by_key(|(_, (addr, _))| *addr);
        
        for (original_idx, (addr, size)) in sorted {
            results[original_idx] = self.read_physical(*addr, *size);
        }
    }
}
```

---

## Next Steps

- See [CPU Emulation](./02-cpu-emulation.md) for instruction implementation
- See [Graphics Subsystem](./04-graphics-subsystem.md) for video memory
- See [Storage Subsystem](./05-storage-subsystem.md) for disk I/O
