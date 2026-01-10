#![no_main]

use libfuzzer_sys::fuzz_target;
use memory::{AccessType, Bus, Mmu};

const RAM_SIZE: usize = 256 * 1024;

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
    let cr3 = parse_u64_le(&data[0x08..0x10]);
    let cr4 = parse_u64_le(&data[0x10..0x18]);
    let efer = parse_u64_le(&data[0x18..0x20]);
    let cpl = data[0x20] & 3;
    let access = match data[0x21] % 3 {
        0 => AccessType::Read,
        1 => AccessType::Write,
        _ => AccessType::Execute,
    };
    let vaddr = parse_u64_le(&data[0x22..0x2a]);

    let mut bus = Bus::new(RAM_SIZE);
    let ram_init = &data[0x2a..];
    bus.ram_mut()[..ram_init.len().min(RAM_SIZE)]
        .copy_from_slice(&ram_init[..ram_init.len().min(RAM_SIZE)]);

    let mmu = Mmu {
        cr0,
        cr3,
        cr4,
        efer,
        cpl,
    };

    let _ = mmu.translate(&mut bus, vaddr, access);
});
