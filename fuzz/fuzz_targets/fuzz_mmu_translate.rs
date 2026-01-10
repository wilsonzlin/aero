#![no_main]

use libfuzzer_sys::fuzz_target;
use memory::{AccessType, Bus, Mmu};

const RAM_SIZE: usize = 256 * 1024;

fn canonicalize_4level(vaddr: u64) -> u64 {
    let low = vaddr & ((1u64 << 48) - 1);
    if (low >> 47) & 1 == 0 {
        low
    } else {
        low | (!0u64 << 48)
    }
}

fn write_u32(ram: &mut [u8], addr: u64, val: u32) {
    let start = addr as usize;
    if start + 4 > ram.len() {
        return;
    }
    ram[start..start + 4].copy_from_slice(&val.to_le_bytes());
}

fn write_u64(ram: &mut [u8], addr: u64, val: u64) {
    let start = addr as usize;
    if start + 8 > ram.len() {
        return;
    }
    ram[start..start + 8].copy_from_slice(&val.to_le_bytes());
}

fn parse_u64_le(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    let n = bytes.len().min(8);
    buf[..n].copy_from_slice(&bytes[..n]);
    u64::from_le_bytes(buf)
}

fuzz_target!(|data: &[u8]| {
    // Layout:
    //   0x00..0x20: CR0/CR3/CR4/EFER (4x u64 LE)
    //   0x20:       CPL (low 2 bits)
    //   0x21:       access type (0=R,1=W,2=X)
    //   0x22..0x2a: vaddr (u64 LE)
    //   0x2a..end:  guest RAM contents (page tables)
    if data.len() < 0x2a {
        return;
    }

    let cr0 = parse_u64_le(&data[0x00..0x08]);
    // Keep CR3 inside our synthetic RAM so the fuzzer can reach deeper page-walk states without
    // having to guess a large physical address space.
    let cr3 = (parse_u64_le(&data[0x08..0x10]) & !0xfff) % (RAM_SIZE as u64);
    let cr4 = parse_u64_le(&data[0x10..0x18]);
    let efer = parse_u64_le(&data[0x18..0x20]);
    let cpl = data[0x20] & 3;
    let access = match data[0x21] % 3 {
        0 => AccessType::Read,
        1 => AccessType::Write,
        _ => AccessType::Execute,
    };
    let mut vaddr = parse_u64_le(&data[0x22..0x2a]);
    let long_mode = (cr0 & (1 << 31)) != 0 && (cr4 & (1 << 5)) != 0 && (efer & (1 << 8)) != 0;
    // Long-mode translations require canonical addresses; bias inputs towards that so we spend more
    // time in the page-walk code rather than immediately returning #GP for non-canonical vaddrs.
    if long_mode && (data[0x21] & 0x80) == 0 {
        vaddr = canonicalize_4level(vaddr);
    }

    let mut bus = Bus::new(RAM_SIZE);
    let ram_init = &data[0x2a..];
    let ram = bus.ram_mut();
    ram[..ram_init.len().min(RAM_SIZE)].copy_from_slice(&ram_init[..ram_init.len().min(RAM_SIZE)]);

    // For a subset of inputs, force a well-formed page-table chain that maps `vaddr` so the fuzzer
    // spends time in deeper page-walk code paths. This overwrites a small portion of the RAM blob
    // (page tables only), while still keeping the rest of the input-driven memory state intact.
    if (cr0 & (1 << 31)) != 0 && (data[0x21] & 0x40) == 0 {
        let ram_size = RAM_SIZE as u64;
        if (cr4 & (1 << 5)) == 0 {
            // 32-bit non-PAE paging: PD -> PT -> 4KiB page.
            let pd_base = cr3 & !0xfff;
            let pd_base = pd_base % ram_size;
            let pt_base = (pd_base + 0x1000) % ram_size;
            let page_base = (pd_base + 0x2000) % ram_size;

            let v = vaddr as u32 as u64;
            let pde_index = ((v >> 22) & 0x3ff) as u64;
            let pte_index = ((v >> 12) & 0x3ff) as u64;

            let pde_addr = pd_base + pde_index * 4;
            let pte_addr = pt_base + pte_index * 4;

            // P, RW, US
            write_u32(ram, pde_addr, (pt_base as u32) | 0x7);
            write_u32(ram, pte_addr, (page_base as u32) | 0x7);
        } else if !long_mode {
            // IA-32 PAE paging: PDPT -> PD -> PT -> 4KiB page.
            let pdpt_base = cr3 & !0x1f;
            let pdpt_base = pdpt_base % ram_size;
            let pd_base = (pdpt_base + 0x1000) % ram_size;
            let pt_base = (pdpt_base + 0x2000) % ram_size;
            let page_base = (pdpt_base + 0x3000) % ram_size;

            let v = vaddr as u32 as u64;
            let pdpt_index = (v >> 30) & 0x3;
            let pd_index = (v >> 21) & 0x1ff;
            let pt_index = (v >> 12) & 0x1ff;

            // PDPTE has only P + address bits (RW/US are reserved in PAE).
            let pdpte_addr = pdpt_base + pdpt_index * 8;
            let pde_addr = pd_base + pd_index * 8;
            let pte_addr = pt_base + pt_index * 8;

            write_u64(ram, pdpte_addr, (pd_base & !0xfff) | 0x1);
            write_u64(ram, pde_addr, (pt_base & !0xfff) | 0x7);
            write_u64(ram, pte_addr, (page_base & !0xfff) | 0x7);
        } else {
            // 4-level IA-32e paging: PML4 -> PDPT -> PD -> PT -> 4KiB page.
            let pml4_base = cr3 & !0xfff;
            let pml4_base = pml4_base % ram_size;
            let pdpt_base = (pml4_base + 0x1000) % ram_size;
            let pd_base = (pml4_base + 0x2000) % ram_size;
            let pt_base = (pml4_base + 0x3000) % ram_size;
            let page_base = (pml4_base + 0x4000) % ram_size;

            let pml4_index = (vaddr >> 39) & 0x1ff;
            let pdpt_index = (vaddr >> 30) & 0x1ff;
            let pd_index = (vaddr >> 21) & 0x1ff;
            let pt_index = (vaddr >> 12) & 0x1ff;

            let pml4e_addr = pml4_base + pml4_index * 8;
            let pdpte_addr = pdpt_base + pdpt_index * 8;
            let pde_addr = pd_base + pd_index * 8;
            let pte_addr = pt_base + pt_index * 8;

            // P, RW, US
            write_u64(ram, pml4e_addr, (pdpt_base & !0xfff) | 0x7);
            write_u64(ram, pdpte_addr, (pd_base & !0xfff) | 0x7);
            write_u64(ram, pde_addr, (pt_base & !0xfff) | 0x7);
            write_u64(ram, pte_addr, (page_base & !0xfff) | 0x7);
        }
    }

    let mmu = Mmu {
        cr0,
        cr3,
        cr4,
        efer,
        cpl,
    };

    let _ = mmu.translate(&mut bus, vaddr, access);
});
