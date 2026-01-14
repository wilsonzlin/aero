# 03 - Memory Management Unit

## Overview

Memory management is critical for both correctness and performance. Windows 7 heavily uses paging, and the MMU must accurately emulate address translation while maintaining reasonable speed through TLB caching.

> **Note (implementation):** The canonical paging/MMU implementation is `crates/aero-mmu`, integrated into the CPU core via `aero_cpu_core::PagingBus` (a `CpuBus` adapter that translates Tier-0 linear addresses using `aero-mmu`).
> - `aero-mmu`: [`crates/aero-mmu/src/lib.rs`](../crates/aero-mmu/src/lib.rs)
> - `PagingBus`: [`crates/aero-cpu-core/src/paging_bus.rs`](../crates/aero-cpu-core/src/paging_bus.rs)
>
> **Browser reality:** In the web build, guest RAM is backed by wasm32 `WebAssembly.Memory`, so the emulator is constrained to **≤ 4 GiB** of linear memory total. Large control/IPC buffers (rings, status, audio) should live in separate `SharedArrayBuffer`s.  
> See [ADR 0003](./adr/0003-shared-memory-layout.md).

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

## JIT-visible TLB (Baseline JIT Memory Fast Path)

The interpreter can call `Mmu::translate()` (and then `MemoryBus::{read,write}_physical()`) for every memory access. In JIT-compiled code this would mean an imported call per load/store, which is typically the dominant overhead in memory-heavy guest code.

To make RAM accesses cheap in JITed WASM, we expose a **small, fixed-layout TLB** in linear memory that the JIT can read directly, and we define a slow-path translation import and (for Tier-1) an MMIO exit helper:

1. `mmu_translate(cpu_ptr: i32, jit_ctx_ptr: i32, vaddr: i64, access: i32) -> i64` — page table walk + permission checks, fills the JIT-visible TLB entry, and returns the packed translation word (`phys_page_base | flags`). The `access` code currently matches `aero_jit::abi::{MMU_ACCESS_READ, MMU_ACCESS_WRITE}`. On a page fault, the runtime must exit the JIT and deliver `#PF`.
2. `jit_exit_mmio(...)` — return to the runtime when the translated access resolves to MMIO/ROM/unmapped, so the normal device/memory routing code runs. (Tier-2 currently falls back to the imported `mem_read_*` / `mem_write_*` helpers instead of using a dedicated MMIO-exit import.)

### Current WASM ABI (`cpu_ptr` + `jit_ctx_ptr`)

Both Tier-1 blocks and Tier-2 traces use a 2-pointer ABI:

- Tier-1 export: `block(cpu_ptr: i32, jit_ctx_ptr: i32) -> i64`
- Tier-2 export: `trace(cpu_ptr: i32, jit_ctx_ptr: i32) -> i64`

`cpu_ptr` points into shared WASM linear memory at the architectural `aero_cpu_core::state::CpuState`.

`jit_ctx_ptr` points to a separate JIT-only context region (below). This keeps JIT acceleration structures out of `CpuState` while still giving generated code fast, predictable access.

### Design requirements

- **JIT-friendly layout:** `#[repr(C)]`, no pointers/Vecs inside the hot structure, stable offsets.
- **Fast lookup:** direct-mapped, power-of-two entries; 2×64-bit loads on hit (`tag` + `data`).
- **Cheap invalidation:** changing `tlb_salt` (optionally derived from address-space identity + an epoch) makes a full flush O(1) without walking entries.
- **Permission bits in the cache:** allow the JIT to reject disallowed accesses without re-walking page tables.

### JIT context region (current implementation)

The JIT-visible TLB is **not embedded in `CpuState`**. Instead it lives in a dedicated linear-memory region at `jit_ctx_ptr`.

See: `crates/aero-jit-x86/src/jit_ctx.rs`

```rust
pub const JIT_TLB_ENTRIES: usize = 256; // power-of-two for masking

/// Hot entry format: two u64s to keep JIT codegen simple.
#[repr(C)]
pub struct JitTlbEntry {
    /// Tag match: ((vpn ^ tlb_salt) | 1). `0` means invalid.
    ///
    /// The `| 1` keeps the all-zero tag reserved for invalidation, and avoids an ambiguity where
    /// `vpn ^ tlb_salt == 0` would otherwise make a valid translation indistinguishable from an
    /// invalid entry (especially relevant for `vpn == 0`).
    pub tag: u64,
    /// Packed: phys_page_base | flags (low 12 bits).
    pub data: u64,
}

/// JIT-visible context header stored in WASM linear memory at `jit_ctx_ptr`.
#[repr(C)]
pub struct JitContext {
    /// Base offset of guest RAM within linear memory.
    pub ram_base: u64,
    /// Tag salt used when computing `JitTlbEntry::tag`.
    pub tlb_salt: u64,
}

impl JitContext {
    pub const RAM_BASE_OFFSET: u32 = 0;
    pub const TLB_SALT_OFFSET: u32 = 8;
    pub const TLB_OFFSET: u32 = 16; // start of the tlb[] array
}
```

At `jit_ctx_ptr + JitContext::TLB_OFFSET`, the context contains a contiguous array of `JIT_TLB_ENTRIES` entries, each 16 bytes: `{ tag: u64, data: u64 }`.

This is intentionally not a full architectural iTLB/dTLB model; it is a **JIT acceleration structure**. The interpreter/MMU can continue using a richer, higher-level TLB internally if desired, while mirroring translations into this compact representation for JIT consumption.

### `data` bit packing

`data` is a single word so the JIT can load it once and then:

- test permissions / routing flags
- mask out the physical page base
- compute `paddr = phys_page_base | (vaddr & 0xFFF)`

For simplicity, `phys_page_base` is always **4KB-aligned**, even when the guest mapping uses 2MB/1GB large pages. The slow path computes the final `paddr` for the requested `vaddr` and then inserts `phys_page_base = paddr & !0xFFF` into the JIT TLB. This keeps the JIT fast path uniform and makes the “crosses a 4KB boundary” guard sufficient.

Suggested layout:

```
data: u64
  [63:12] phys_page_base (4KB-aligned physical address base)
  [11:0]  flags

flags:
  bit 0: TLB_FLAG_READ permitted
  bit 1: TLB_FLAG_WRITE permitted
  bit 2: TLB_FLAG_EXEC permitted
  bit 3: TLB_FLAG_IS_RAM (1 = direct linear-memory access is valid; 0 = MMIO/ROM/unmapped)
  bit 4: (future/optional) CODE_WATCH (writes must notify JIT invalidation/versioning)
  bits 5..11: reserved (page size, PAT/MTRR classification, etc.)
```

In the current implementation these correspond to `aero_jit::{TLB_FLAG_READ, TLB_FLAG_WRITE, TLB_FLAG_EXEC, TLB_FLAG_IS_RAM}`; higher bits are reserved.

`IS_RAM` should only be set when the resolved physical range is backed by the emulator’s guest RAM buffer (not MMIO/ROM holes), i.e. when a direct `load/store` from WASM memory is semantically correct.

### Self-modifying code and store-side invalidation

Self-modifying code invalidation is handled by the runtime using page-version snapshots:

- `aero_cpu_core::jit::runtime::PageVersionTracker` maintains a wrapping (modulo-2^32) `u32` version counter for each 4KB guest physical page (indexed by `paddr >> 12`). Each observed write bumps the touched page versions by `1` (`u32::MAX + 1 == 0`).
- The embedding/runtime must call `JitRuntime::on_guest_write(paddr, len)` for any guest physical write that could hit RAM/code. This bumps the version for every covered 4KB page.
- When compiling a block/trace, the compiler captures metadata at the time it reads instruction bytes:
  - `JitRuntime::snapshot_meta(code_paddr, byte_len) -> CompiledBlockMeta`
  - `CompiledBlockMeta.page_versions` stores a list of `{ page, version }` snapshots for all pages covering `[code_paddr, code_paddr + byte_len)`.
- At runtime:
  - `JitRuntime::install_handle` rejects compilation results whose `CompiledBlockMeta.page_versions` no longer match the current `PageVersionTracker` (e.g. background compilation races with code writes).
  - `JitRuntime::prepare_block` lazily invalidates cached blocks whose page-version snapshots are stale and requests recompilation.

In the browser tiered execution harness (`crates/aero-wasm/src/tiered_vm.rs`), Tier-0 interpreter writes are captured by the WASM-side `CpuBus` into a bounded `GuestWriteLog` and drained into `JitRuntime::on_guest_write` after each executed block/interrupt boundary. Tier-1 slow-path store helpers (JS `env.mem_write_*`) are expected to call the exported `WasmTieredVm::on_guest_write(paddr, len)` so the embedded page-version tracker stays coherent.

Tier-1 blocks rely on these runtime snapshot checks, while Tier-2 traces additionally embed
`GuardCodeVersion { page, expected }` checks derived from the same tracker so they can deopt when the
guest modifies code after trace compilation.

> **Future/optional:** If Tier-1/Tier-2 direct RAM stores are enabled, the runtime may not observe those writes unless the JIT explicitly notifies it. One possible extension is to reserve an extra flag bit (conceptually “CODE_WATCH”) so stores can cheaply decide whether to call a write-notification helper or force a runtime exit.
>
> Tier-2 traces emit in-WASM page-version guards (at trace entry, and for loop traces on every
> iteration) so they can bail out quickly on self-modifying code.

### Lookup flow (inline in JIT)

At block entry (or first memory op), the JIT should load `ram_base` and `tlb_salt` from `jit_ctx_ptr` into WASM locals to avoid reloading them on every access.

For each inlined memory access:

1. Compute `vaddr`
2. (Optional but recommended) check `cross_page(vaddr, size)`; if it crosses a 4KB boundary, go slow-path
3. Compute:
   - `vpn = vaddr >> 12`
   - `idx = (vpn as usize) & (JIT_TLB_ENTRIES - 1)`
   - `expect_tag = (vpn ^ tlb_salt) | 1`
4. Load `entry.tag`; if mismatch → call `mmu_translate(cpu_ptr, jit_ctx_ptr, vaddr, access)`
5. Load `entry.data` and verify permission bits; if missing → call `mmu_translate(...)` again (the runtime should raise `#PF` with a correct error code)
6. If `IS_RAM` is set:
   - `phys_base = data & !0xFFF`
   - `paddr = phys_base | (vaddr & 0xFFF)`
   - perform the WASM `load/store` at `ram_base + paddr`
7. Else: call `jit_exit_mmio(...)` and return to the runtime dispatcher

### Slow-path translation contract

`mmu_translate(cpu_ptr: i32, jit_ctx_ptr: i32, vaddr: i64, access: i32) -> i64` must:

- perform a page table walk
- enforce privilege + R/W/X checks (including distinguishing instruction fetch vs data access)
- on success:
  - compute `phys_page_base` and flags
  - fill the corresponding `JitTlbEntry` (`tag` then `data`)
  - return `data` (or `phys_page_base`) to the caller so it can continue without retrying
- on failure: raise `#PF` and exit the JIT block (the exact mechanism is an implementation detail: trap, longjmp-like exit, or writing an exit reason into `CpuState`)

### Invalidation / versioning

- **CR3 write / INVPCID / global flush:** update `JitContext.tlb_salt`. Existing entries become unreachable because tags no longer match. (Optionally also clear tags to eagerly reclaim entries.)
- **INVLPG:** clear the matching entry’s `tag` (set it to 0).
- **MMIO map changes:** update `tlb_salt` (or otherwise force a flush). Otherwise, stale `TLB_FLAG_IS_RAM` classifications could cause incorrect direct RAM accesses.

> **Future/optional:** The design supports extending the `JitContext` header with additional fields like an `epoch`, per-privilege salts (`salt_user`/`salt_kernel`), and/or a separate “phys-map epoch” that can be mixed into the salt. These are not part of the current `JitContext` layout, but can be added by extending the header and keeping existing offsets stable.

## Memory Bus Implementation

### Physical Memory Regions

#### PC/Q35: ECAM + PCI/MMIO holes (non-contiguous RAM)

On the canonical PC platform, guest physical RAM is **not always a single contiguous**
`[0, total_ram)` region:

- Low RAM is usable up to `aero_pc_constants::PCIE_ECAM_BASE` (`0xB000_0000`).
- `0xB000_0000..0xC000_0000` is reserved for the PCIe **ECAM/MMCONFIG** window (`PCIE_ECAM_SIZE =
  0x1000_0000`).
- `0xC000_0000..0x1_0000_0000` is the below-4 GiB **PCI/MMIO hole** (PCI BARs + chipset MMIO).
- If `total_ram > 0xB000_0000`, the BIOS remaps the remaining RAM above 4 GiB starting at
  `0x1_0000_0000` so the configured RAM size is preserved.

This means a RAM backend cannot assume `paddr < ram_size ⇒ RAM`. It must be able to represent a
**segmented** (non-contiguous) guest-physical RAM layout and treat the holes as unmapped unless an
MMIO device claims them. Unclaimed hole reads should behave like **open bus** (return `0xFF` bytes).

See: `crates/firmware/src/bios/interrupts.rs::build_e820_map`

Implementation note: the `memory` crate already provides a helper that encodes this behavior:
`crates/memory/src/mapped.rs` (`MappedGuestMemory`).

This is used by the canonical PC physical memory buses when `ram_size > PCIE_ECAM_BASE`
(see `crates/platform/src/memory.rs::MemoryBus::wrap_pc_high_memory` and
`crates/aero-machine/src/lib.rs::SystemMemory::new`).

Even though `0x000A_0000–0x000B_FFFF` sits in the “conventional memory” area, it must be treated as
device memory: the emulator registers an `MmioRegion` for the **legacy VGA VRAM window** so
BIOS/bootloader/Windows writes to `0xB8000` (text mode) are visible on the canvas.

In the canonical machine, this is implemented by:

- `aero_gpu_vga` when `MachineConfig::enable_vga=true` (transitional boot display path), or
- the AeroGPU BAR1-backed VRAM + VRAM-backed legacy VGA/VBE decode when `MachineConfig::enable_aerogpu=true`
  (legacy decode foundation).

Separately, VBE graphics modes use a linear framebuffer (LFB) at a different physical address:

- With the transitional standalone VGA/VBE path (`MachineConfig::enable_vga=true`), the LFB base is
  a configuration knob when the PC platform is disabled (defaulting to `aero_gpu_vga::SVGA_LFB_BASE`,
  i.e. `0xE000_0000`). When the PC platform is enabled, the canonical machine exposes a
  Bochs/QEMU-compatible VGA PCI stub (currently `00:0c.0`) whose BAR0 is assigned by BIOS POST / the
  PCI resource allocator (and may be relocated when other PCI devices are present). The machine
  mirrors that assigned BAR base into the BIOS VBE `PhysBasePtr` and the VGA device model so MMIO
  routing and BIOS-reported mode info remain coherent.
- In the intended AeroGPU-owned VGA/VBE path (`MachineConfig::enable_aerogpu=true`), the firmware
  VBE mode-info `PhysBasePtr` is derived from AeroGPU BAR1:
  `PhysBasePtr = BAR1_BASE + 0x40000` (`AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES`; see
  `crates/aero-machine/src/lib.rs::VBE_LFB_OFFSET`).

See: [AeroGPU Legacy VGA/VBE Compatibility](./16-aerogpu-vga-vesa-compat.md)
and [AeroGPU PCI identity](./abi/aerogpu-pci-identity.md) (AeroGPU vs standalone VGA/VBE path).

```rust
pub struct MemoryBus {
    // Main RAM (guest physical memory)
    ram: GuestRam, // backed by shared WebAssembly.Memory (wasm32) in browser builds
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
        //
        // Note: on PC/Q35 this cannot be a simple `paddr < ram_size` check once ECAM/MMIO holes are
        // modeled; RAM may be split into low RAM + high RAM above 4GiB. The real implementation
        // must be hole-aware and translate `paddr -> ram_offset` based on the E820 map.
        if paddr < self.ram_size as u64 {
            return self.read_ram(paddr, size);
        }
        
        // Unmapped / open bus - return all 1s (byte reads = 0xFF)
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
        
        // Use TypedArray views for efficient access over shared wasm memory
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
