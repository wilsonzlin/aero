use aero_machine::{Machine, MachineConfig};

fn fnv1a64(mut hash: u64, bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;
    if hash == 0 {
        hash = FNV_OFFSET;
    }
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn framebuffer_hash(framebuffer: &[u32]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for px in framebuffer {
        hash = fnv1a64(hash, &px.to_ne_bytes());
    }
    hash
}

#[test]
fn vga_text_mmio_writes_show_up_in_rendered_output() {
    let cfg = MachineConfig {
        enable_vga: true,
        enable_aerogpu: false,
        ..Default::default()
    };
    let mut m = Machine::new(cfg).unwrap();

    // Force deterministic baseline.
    {
        let mut addr = 0xB8000u64;
        let mut remaining = 0x8000usize; // 32KiB text window
        const ZERO: [u8; 4096] = [0; 4096];
        while remaining != 0 {
            let len = remaining.min(ZERO.len());
            m.write_physical(addr, &ZERO[..len]);
            addr = addr.saturating_add(len as u64);
            remaining -= len;
        }
    }

    // Disable cursor for deterministic output (CRTC index 0x0A).
    m.io_write(0x3D4, 1, 0x0A);
    m.io_write(0x3D5, 1, 0x20);

    // Write "A" at the top-left cell with light grey on blue through machine memory MMIO.
    m.write_physical_u8(0xB8000, b'A');
    m.write_physical_u8(0xB8001, 0x1F);

    m.display_present();
    assert_eq!(
        framebuffer_hash(m.display_framebuffer()),
        0x5cfe440e33546065
    );
}
