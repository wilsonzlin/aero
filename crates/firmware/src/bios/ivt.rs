use super::{
    BiosBus, BDA_BASE, BIOS_SEGMENT, DEFAULT_INT_STUB_OFFSET, EBDA_BASE, EBDA_SIZE,
    INT10_STUB_OFFSET, INT13_STUB_OFFSET, INT15_STUB_OFFSET, INT16_STUB_OFFSET, INT1A_STUB_OFFSET,
    IVT_BASE,
};

const BDA_EBDA_SEGMENT_OFFSET: u64 = 0x0E; // 0x40E absolute
const BDA_COM_PORTS_OFFSET: u64 = 0x00; // 0x400 absolute
const BDA_LPT_PORTS_OFFSET: u64 = 0x08; // 0x408 absolute
const BDA_EQUIPMENT_WORD_OFFSET: u64 = 0x10; // 0x410 absolute
const BDA_MEM_SIZE_KB_OFFSET: u64 = 0x13; // 0x413 absolute

pub fn init_ivt(bus: &mut dyn BiosBus) {
    for vector in 0u16..=0xFF {
        let offset = handler_offset(vector as u8);
        let addr = IVT_BASE + (vector as u64) * 4;
        bus.write_u16(addr, offset);
        bus.write_u16(addr + 2, BIOS_SEGMENT);
    }
}

fn handler_offset(vector: u8) -> u16 {
    match vector {
        0x10 => INT10_STUB_OFFSET,
        0x13 => INT13_STUB_OFFSET,
        0x15 => INT15_STUB_OFFSET,
        0x16 => INT16_STUB_OFFSET,
        0x1A => INT1A_STUB_OFFSET,
        _ => DEFAULT_INT_STUB_OFFSET,
    }
}

pub fn init_bda(bus: &mut dyn BiosBus) {
    // Base I/O addresses for standard devices.
    //
    // These are consumed by some guests directly (rather than using BIOS INT 14h/17h). Populate a
    // minimal, PC-compatible configuration:
    // - COM1 present at 0x3F8
    // - no LPT ports
    bus.write_u16(BDA_BASE + BDA_COM_PORTS_OFFSET, 0x03F8); // COM1
    bus.write_u16(BDA_BASE + BDA_COM_PORTS_OFFSET + 2, 0x0000); // COM2
    bus.write_u16(BDA_BASE + BDA_COM_PORTS_OFFSET + 4, 0x0000); // COM3
    bus.write_u16(BDA_BASE + BDA_COM_PORTS_OFFSET + 6, 0x0000); // COM4
    bus.write_u16(BDA_BASE + BDA_LPT_PORTS_OFFSET, 0x0000); // LPT1
    bus.write_u16(BDA_BASE + BDA_LPT_PORTS_OFFSET + 2, 0x0000); // LPT2
    bus.write_u16(BDA_BASE + BDA_LPT_PORTS_OFFSET + 4, 0x0000); // LPT3

    // EBDA segment pointer.
    let ebda_segment = (EBDA_BASE / 16) as u16;
    bus.write_u16(BDA_BASE + BDA_EBDA_SEGMENT_OFFSET, ebda_segment);

    // Equipment list word (INT 11h).
    //
    // This BIOS models a minimal PC-compatible platform:
    // - VGA/EGA video (starts in 80x25 text mode)
    // - x87 FPU present (the CPU core implements x87)
    // - one serial port (COM1)
    //
    // We intentionally do not advertise floppy drives or game/parallel ports.
    //
    // Bit layout reference (IBM PC/AT convention):
    // - bit 1: math coprocessor
    // - bits 4-5: initial video mode (2 = 80x25 color)
    // - bits 9-11: number of serial ports
    let equipment: u16 = (1 << 1) | (2 << 4) | (1 << 9);
    bus.write_u16(BDA_BASE + BDA_EQUIPMENT_WORD_OFFSET, equipment);

    // Conventional memory size in KiB (up to EBDA).
    let base_mem_kb = (EBDA_BASE / 1024) as u16;
    bus.write_u16(BDA_BASE + BDA_MEM_SIZE_KB_OFFSET, base_mem_kb);

    // EBDA starts with a size field in KiB (per IBM PC/AT convention).
    bus.write_u16(EBDA_BASE, (EBDA_SIZE / 1024) as u16);
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bios::TestMemory;
    use memory::MemoryBus as _;

    #[test]
    fn init_bda_initializes_equipment_word() {
        let mut mem = TestMemory::new(2 * 1024 * 1024);
        init_bda(&mut mem);

        assert_eq!(mem.read_u16(BDA_BASE + BDA_COM_PORTS_OFFSET), 0x03F8);
        assert_eq!(mem.read_u16(BDA_BASE + BDA_COM_PORTS_OFFSET + 2), 0);
        assert_eq!(mem.read_u16(BDA_BASE + BDA_COM_PORTS_OFFSET + 4), 0);
        assert_eq!(mem.read_u16(BDA_BASE + BDA_COM_PORTS_OFFSET + 6), 0);
        assert_eq!(mem.read_u16(BDA_BASE + BDA_LPT_PORTS_OFFSET), 0);
        assert_eq!(mem.read_u16(BDA_BASE + BDA_LPT_PORTS_OFFSET + 2), 0);
        assert_eq!(mem.read_u16(BDA_BASE + BDA_LPT_PORTS_OFFSET + 4), 0);

        assert_eq!(mem.read_u16(BDA_BASE + BDA_EQUIPMENT_WORD_OFFSET), 0x0222);
    }
}
