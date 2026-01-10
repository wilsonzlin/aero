use firmware::smbios::{SmbiosConfig, SmbiosTables};
use firmware::memory::{MemoryBus, VecMemory};

fn scan_for_eps(mem: &VecMemory) -> Option<u32> {
    // SMBIOS spec: search first KB of EBDA (if present), else scan 0xF0000-0xFFFFF.
    let mut ebda_seg_bytes = [0u8; 2];
    mem.read_physical(0x40E, &mut ebda_seg_bytes);
    let ebda_seg = u16::from_le_bytes(ebda_seg_bytes);
    if ebda_seg != 0 {
        let ebda_base = (ebda_seg as u32) << 4;
        if let Some(addr) = scan_region(mem, ebda_base, 1024) {
            return Some(addr);
        }
    }
    scan_region(mem, 0xF0000, 0x10000)
}

fn scan_region(mem: &VecMemory, base: u32, len: usize) -> Option<u32> {
    let mut buf = vec![0u8; len];
    mem.read_physical(base as u64, &mut buf);
    for off in (0..len.saturating_sub(4)).step_by(16) {
        if &buf[off..off + 4] == b"_SM_" {
            return Some(base + off as u32);
        }
    }
    None
}

#[test]
fn host_memory_scan_finds_eps_and_parses_table() {
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    mem.write_physical(0x40E, &0x9FC0u16.to_le_bytes());

    let config = SmbiosConfig {
        ram_bytes: 512 * 1024 * 1024,
        ..Default::default()
    };
    let eps_addr = SmbiosTables::build_and_write(&config, &mut mem);

    let scanned = scan_for_eps(&mem).expect("EPS not found by scan");
    assert_eq!(scanned, eps_addr);

    // Parse EPS enough to sanity-check the table is reachable and ends with Type 127.
    let mut eps = [0u8; 0x1F];
    mem.read_physical(eps_addr as u64, &mut eps);
    let table_len = u16::from_le_bytes([eps[0x16], eps[0x17]]) as usize;
    let table_addr = u32::from_le_bytes([eps[0x18], eps[0x19], eps[0x1A], eps[0x1B]]);

    let mut table = vec![0u8; table_len];
    mem.read_physical(table_addr as u64, &mut table);

    // Walk structures until end-of-table.
    let mut i = 0usize;
    let mut saw_end = false;
    while i < table.len() {
        let ty = table[i];
        let len = table[i + 1] as usize;
        i += len;
        // Skip string-set.
        loop {
            if i + 1 >= table.len() {
                panic!("unterminated string-set");
            }
            if table[i] == 0 && table[i + 1] == 0 {
                i += 2;
                break;
            }
            i += 1;
        }
        if ty == 127 {
            saw_end = true;
            break;
        }
    }

    assert!(
        saw_end,
        "SMBIOS table did not contain Type 127 end-of-table"
    );
}
