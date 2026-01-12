use super::{
    BiosBus, BDA_BASE, BIOS_SEGMENT, DEFAULT_INT_STUB_OFFSET, DISKETTE_PARAM_TABLE_OFFSET,
    EBDA_BASE, EBDA_SIZE, FIXED_DISK_PARAM_TABLE_OFFSET, INT10_STUB_OFFSET, INT13_STUB_OFFSET,
    INT15_STUB_OFFSET, INT16_STUB_OFFSET, INT1A_STUB_OFFSET, IVT_BASE,
};

const BDA_EBDA_SEGMENT_OFFSET: u64 = 0x0E; // 0x40E absolute
const BDA_COM_PORTS_OFFSET: u64 = 0x00; // 0x400 absolute
const BDA_LPT_PORTS_OFFSET: u64 = 0x08; // 0x408 absolute
const BDA_EQUIPMENT_WORD_OFFSET: u64 = 0x10; // 0x410 absolute
const BDA_KEYBOARD_FLAGS_OFFSET: u64 = 0x17; // 0x417 absolute
const BDA_KEYBOARD_BUF_HEAD_OFFSET: u64 = 0x1A; // 0x41A absolute
const BDA_KEYBOARD_BUF_TAIL_OFFSET: u64 = 0x1C; // 0x41C absolute
const BDA_KEYBOARD_BUF_START: u16 = 0x001E; // 0x40:0x1E -> 0x41E absolute
const BDA_KEYBOARD_BUF_END: u16 = 0x003E; // 0x40:0x3E -> 0x43E absolute
const BDA_KEYBOARD_BUF_START_PTR_OFFSET: u64 = 0x80; // 0x480 absolute
const BDA_KEYBOARD_BUF_END_PTR_OFFSET: u64 = 0x82; // 0x482 absolute
const BDA_HARD_DISK_COUNT_OFFSET: u64 = 0x75; // 0x475 absolute
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
        0x1E => DISKETTE_PARAM_TABLE_OFFSET,
        0x41 | 0x46 => FIXED_DISK_PARAM_TABLE_OFFSET,
        _ => DEFAULT_INT_STUB_OFFSET,
    }
}

pub fn init_bda(bus: &mut dyn BiosBus, boot_drive: u8) {
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
    // We advertise floppy drives only when the configured boot drive is a floppy (`DL < 0x80`),
    // since DOS-era software commonly probes INT 11h to decide whether to create A:/B:.
    //
    // Bit layout reference (IBM PC/AT convention):
    // - bit 0: diskette drive(s) installed
    // - bit 1: math coprocessor
    // - bits 4-5: initial video mode (2 = 80x25 color)
    // - bits 6-7: number of diskette drives - 1
    // - bits 9-11: number of serial ports
    let mut equipment: u16 = (1 << 1) | (2 << 4) | (1 << 9);
    if boot_drive < 0x80 {
        let drives = boot_drive.saturating_add(1).clamp(1, 4);
        equipment |= 1 << 0;
        equipment |= ((u16::from(drives.saturating_sub(1))) & 0x3) << 6;
    }
    bus.write_u16(BDA_BASE + BDA_EQUIPMENT_WORD_OFFSET, equipment);

    // Keyboard flags + buffer state.
    //
    // We do not model the legacy BDA ring buffer as the source of truth (keyboard input is
    // buffered in `Bios::keyboard_queue`), but initializing these fields keeps the BDA in a
    // consistent "no keys pending" state for software that probes them directly.
    bus.write_u16(BDA_BASE + BDA_KEYBOARD_FLAGS_OFFSET, 0);
    bus.write_u16(
        BDA_BASE + BDA_KEYBOARD_BUF_HEAD_OFFSET,
        BDA_KEYBOARD_BUF_START,
    );
    bus.write_u16(
        BDA_BASE + BDA_KEYBOARD_BUF_TAIL_OFFSET,
        BDA_KEYBOARD_BUF_START,
    );
    bus.write_u16(
        BDA_BASE + BDA_KEYBOARD_BUF_START_PTR_OFFSET,
        BDA_KEYBOARD_BUF_START,
    );
    bus.write_u16(
        BDA_BASE + BDA_KEYBOARD_BUF_END_PTR_OFFSET,
        BDA_KEYBOARD_BUF_END,
    );

    // Number of hard disks installed (used by some bootloaders/DOS utilities).
    //
    // This BIOS currently models exactly one boot device (backed by the single [`BlockDevice`]
    // passed to POST/interrupt handlers). Reflect that in the BDA:
    // - If booting from a floppy (`DL < 0x80`), report *no* hard disks installed.
    // - If booting from a hard disk (`DL >= 0x80`), report enough drives to include the boot drive.
    let hard_disk_count = if boot_drive >= 0x80 {
        boot_drive.wrapping_sub(0x80).saturating_add(1)
    } else {
        0
    };
    bus.write_u8(BDA_BASE + BDA_HARD_DISK_COUNT_OFFSET, hard_disk_count);

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
    fn init_bda_initializes_core_fields() {
        let mut mem = TestMemory::new(2 * 1024 * 1024);
        init_bda(&mut mem, 0x80);

        assert_eq!(mem.read_u16(BDA_BASE + BDA_COM_PORTS_OFFSET), 0x03F8);
        assert_eq!(mem.read_u16(BDA_BASE + BDA_COM_PORTS_OFFSET + 2), 0);
        assert_eq!(mem.read_u16(BDA_BASE + BDA_COM_PORTS_OFFSET + 4), 0);
        assert_eq!(mem.read_u16(BDA_BASE + BDA_COM_PORTS_OFFSET + 6), 0);
        assert_eq!(mem.read_u16(BDA_BASE + BDA_LPT_PORTS_OFFSET), 0);
        assert_eq!(mem.read_u16(BDA_BASE + BDA_LPT_PORTS_OFFSET + 2), 0);
        assert_eq!(mem.read_u16(BDA_BASE + BDA_LPT_PORTS_OFFSET + 4), 0);

        assert_eq!(mem.read_u16(BDA_BASE + BDA_EQUIPMENT_WORD_OFFSET), 0x0222);

        // Keyboard state should start "empty".
        assert_eq!(mem.read_u16(BDA_BASE + BDA_KEYBOARD_FLAGS_OFFSET), 0);
        assert_eq!(
            mem.read_u16(BDA_BASE + BDA_KEYBOARD_BUF_HEAD_OFFSET),
            BDA_KEYBOARD_BUF_START
        );
        assert_eq!(
            mem.read_u16(BDA_BASE + BDA_KEYBOARD_BUF_TAIL_OFFSET),
            BDA_KEYBOARD_BUF_START
        );
        assert_eq!(
            mem.read_u16(BDA_BASE + BDA_KEYBOARD_BUF_START_PTR_OFFSET),
            BDA_KEYBOARD_BUF_START
        );
        assert_eq!(
            mem.read_u16(BDA_BASE + BDA_KEYBOARD_BUF_END_PTR_OFFSET),
            BDA_KEYBOARD_BUF_END
        );
        assert_eq!(mem.read_u8(BDA_BASE + BDA_HARD_DISK_COUNT_OFFSET), 1);
    }

    #[test]
    fn init_bda_advertises_floppy_in_equipment_word_when_booting_from_floppy() {
        let mut mem = TestMemory::new(2 * 1024 * 1024);
        init_bda(&mut mem, 0x00);

        // 0x0223 = 0x0222 + diskette-present bit.
        assert_eq!(mem.read_u16(BDA_BASE + BDA_EQUIPMENT_WORD_OFFSET), 0x0223);
        // A floppy-only system should not advertise any fixed disks.
        assert_eq!(mem.read_u8(BDA_BASE + BDA_HARD_DISK_COUNT_OFFSET), 0);
    }
}
