use aero_bios::firmware::{BlockDevice, DiskError, Memory, NullKeyboard, VbeServices};
use aero_bios::{Bios, BiosConfig, RealModeCpu};
use std::cell::Cell;
use std::rc::Rc;

const VGA_TEXT_BASE: u32 = 0x000B_8000;
const VGA_MODE13_BASE: u32 = 0x000A_0000;

const BDA_BASE: u32 = 0x0400;
const BDA_VIDEO_MODE_ADDR: u32 = BDA_BASE + 0x49;
const BDA_TEXT_COLUMNS_ADDR: u32 = BDA_BASE + 0x4A;
const BDA_VIDEO_PAGE_SIZE_ADDR: u32 = BDA_BASE + 0x4C;
const BDA_CURSOR_POS_ADDR: u32 = BDA_BASE + 0x50;

const MODE13_BYTES_PER_PAGE: usize = 320 * 200;

struct TestMemory {
    bytes: Vec<u8>,
}

impl TestMemory {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
        }
    }
}

impl Memory for TestMemory {
    fn read_u8(&self, paddr: u32) -> u8 {
        self.bytes[paddr as usize]
    }

    fn read_u16(&self, paddr: u32) -> u16 {
        let lo = self.read_u8(paddr) as u16;
        let hi = self.read_u8(paddr + 1) as u16;
        lo | (hi << 8)
    }

    fn read_u32(&self, paddr: u32) -> u32 {
        let b0 = self.read_u8(paddr) as u32;
        let b1 = self.read_u8(paddr + 1) as u32;
        let b2 = self.read_u8(paddr + 2) as u32;
        let b3 = self.read_u8(paddr + 3) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    fn write_u8(&mut self, paddr: u32, v: u8) {
        self.bytes[paddr as usize] = v;
    }

    fn write_u16(&mut self, paddr: u32, v: u16) {
        self.write_u8(paddr, v as u8);
        self.write_u8(paddr + 1, (v >> 8) as u8);
    }

    fn write_u32(&mut self, paddr: u32, v: u32) {
        self.write_u8(paddr, v as u8);
        self.write_u8(paddr + 1, (v >> 8) as u8);
        self.write_u8(paddr + 2, (v >> 16) as u8);
        self.write_u8(paddr + 3, (v >> 24) as u8);
    }
}

struct DummyDisk;

impl BlockDevice for DummyDisk {
    fn read_sector(&mut self, _lba: u64, _buf512: &mut [u8; 512]) -> Result<(), DiskError> {
        Err(DiskError::IoError)
    }

    fn write_sector(&mut self, _lba: u64, _buf512: &[u8; 512]) -> Result<(), DiskError> {
        Err(DiskError::IoError)
    }

    fn sector_count(&self) -> u64 {
        0
    }
}

fn call_int10(bios: &mut Bios, cpu: &mut RealModeCpu, mem: &mut TestMemory) {
    let mut disk = DummyDisk;
    let mut kbd = NullKeyboard;
    bios.handle_interrupt(0x10, cpu, mem, &mut disk, &mut kbd);
}

fn read_text_cell(mem: &TestMemory, row: u32, col: u32) -> (u8, u8) {
    let addr = VGA_TEXT_BASE + (row * 80 + col) * 2;
    (mem.read_u8(addr), mem.read_u8(addr + 1))
}

#[test]
fn int10_mode03_teletype_scrolling_updates_bda_and_vram() {
    let mut bios = Bios::new(BiosConfig::default());
    let mut cpu = RealModeCpu::default();
    let mut mem = TestMemory::new(2 * 1024 * 1024);

    cpu.set_ah(0x00);
    cpu.set_al(0x03);
    call_int10(&mut bios, &mut cpu, &mut mem);

    assert_eq!(mem.read_u8(BDA_VIDEO_MODE_ADDR), 0x03);
    assert_eq!(mem.read_u16(BDA_TEXT_COLUMNS_ADDR), 80);

    for i in 0..26u8 {
        cpu.set_ah(0x0E);
        cpu.set_al(b'A' + i);
        cpu.set_bh(0);
        cpu.set_bl(0x07);
        call_int10(&mut bios, &mut cpu, &mut mem);

        if i != 25 {
            cpu.set_al(b'\r');
            call_int10(&mut bios, &mut cpu, &mut mem);
            cpu.set_al(b'\n');
            call_int10(&mut bios, &mut cpu, &mut mem);
        }
    }

    // One scroll should have occurred (25 rows, 26 lines printed).
    assert_eq!(read_text_cell(&mem, 0, 0).0, b'B');
    assert_eq!(read_text_cell(&mem, 23, 0).0, b'Y');
    assert_eq!(read_text_cell(&mem, 24, 0).0, b'Z');

    // Cursor position for page 0 is stored as row:col word at BDA+0x50.
    assert_eq!(mem.read_u16(BDA_CURSOR_POS_ADDR), 0x1801);
}

#[test]
fn int10_cursor_position_and_shape_roundtrip() {
    let mut bios = Bios::new(BiosConfig::default());
    let mut cpu = RealModeCpu::default();
    let mut mem = TestMemory::new(2 * 1024 * 1024);

    cpu.set_ah(0x00);
    cpu.set_al(0x03);
    call_int10(&mut bios, &mut cpu, &mut mem);

    cpu.set_ah(0x02);
    cpu.set_bh(0);
    cpu.set_dh(5);
    cpu.set_dl(10);
    call_int10(&mut bios, &mut cpu, &mut mem);

    cpu.set_ah(0x01);
    cpu.set_ch(1);
    cpu.set_cl(2);
    call_int10(&mut bios, &mut cpu, &mut mem);

    cpu.set_ah(0x03);
    cpu.set_bh(0);
    call_int10(&mut bios, &mut cpu, &mut mem);

    assert_eq!((cpu.dh(), cpu.dl()), (5, 10));
    assert_eq!((cpu.ch(), cpu.cl()), (1, 2));
}

#[test]
fn int10_scroll_up_al0_clears_window_with_blank_attribute() {
    let mut bios = Bios::new(BiosConfig::default());
    let mut cpu = RealModeCpu::default();
    let mut mem = TestMemory::new(2 * 1024 * 1024);

    cpu.set_ah(0x00);
    cpu.set_al(0x03);
    call_int10(&mut bios, &mut cpu, &mut mem);

    // Fill a couple cells with non-blank content.
    mem.write_u8(VGA_TEXT_BASE, b'X');
    mem.write_u8(VGA_TEXT_BASE + 1, 0x07);

    cpu.set_ah(0x06);
    cpu.set_al(0x00); // clear
    cpu.set_bh(0x1E); // blank attribute
    cpu.set_ch(0);
    cpu.set_cl(0);
    cpu.set_dh(24);
    cpu.set_dl(79);
    call_int10(&mut bios, &mut cpu, &mut mem);

    assert_eq!(read_text_cell(&mem, 0, 0), (b' ', 0x1E));
    assert_eq!(read_text_cell(&mem, 24, 79), (b' ', 0x1E));
}

#[test]
fn int10_write_char_attr_repeat_does_not_move_cursor() {
    let mut bios = Bios::new(BiosConfig::default());
    let mut cpu = RealModeCpu::default();
    let mut mem = TestMemory::new(2 * 1024 * 1024);

    cpu.set_ah(0x00);
    cpu.set_al(0x03);
    call_int10(&mut bios, &mut cpu, &mut mem);

    cpu.set_ah(0x02);
    cpu.set_bh(0);
    cpu.set_dh(0);
    cpu.set_dl(0);
    call_int10(&mut bios, &mut cpu, &mut mem);

    cpu.set_ah(0x09);
    cpu.set_al(b'X');
    cpu.set_bh(0);
    cpu.set_bl(0x1E);
    cpu.set_cx(3);
    call_int10(&mut bios, &mut cpu, &mut mem);

    assert_eq!(read_text_cell(&mem, 0, 0), (b'X', 0x1E));
    assert_eq!(read_text_cell(&mem, 0, 1), (b'X', 0x1E));
    assert_eq!(read_text_cell(&mem, 0, 2), (b'X', 0x1E));

    assert_eq!(mem.read_u16(BDA_CURSOR_POS_ADDR), 0x0000);
}

#[test]
fn int10_write_char_only_repeat_preserves_existing_attributes() {
    let mut bios = Bios::new(BiosConfig::default());
    let mut cpu = RealModeCpu::default();
    let mut mem = TestMemory::new(2 * 1024 * 1024);

    cpu.set_ah(0x00);
    cpu.set_al(0x03);
    call_int10(&mut bios, &mut cpu, &mut mem);

    // Seed attributes at (0,0) and (0,1).
    mem.write_u8(VGA_TEXT_BASE, b'A');
    mem.write_u8(VGA_TEXT_BASE + 1, 0x2F);
    mem.write_u8(VGA_TEXT_BASE + 2, b'A');
    mem.write_u8(VGA_TEXT_BASE + 3, 0x2F);

    cpu.set_ah(0x02);
    cpu.set_bh(0);
    cpu.set_dh(0);
    cpu.set_dl(0);
    call_int10(&mut bios, &mut cpu, &mut mem);

    cpu.set_ah(0x0A);
    cpu.set_al(b'B');
    cpu.set_bh(0);
    cpu.set_cx(2);
    call_int10(&mut bios, &mut cpu, &mut mem);

    assert_eq!(read_text_cell(&mem, 0, 0), (b'B', 0x2F));
    assert_eq!(read_text_cell(&mem, 0, 1), (b'B', 0x2F));
}

#[test]
fn int10_mode13_clears_vram_and_updates_bda() {
    let mut bios = Bios::new(BiosConfig::default());
    let mut cpu = RealModeCpu::default();
    let mut mem = TestMemory::new(2 * 1024 * 1024);

    // Seed VRAM with non-zero bytes.
    mem.bytes[VGA_MODE13_BASE as usize..VGA_MODE13_BASE as usize + MODE13_BYTES_PER_PAGE]
        .fill(0xFF);

    cpu.set_ah(0x00);
    cpu.set_al(0x13);
    call_int10(&mut bios, &mut cpu, &mut mem);

    assert_eq!(mem.read_u8(BDA_VIDEO_MODE_ADDR), 0x13);
    assert_eq!(mem.read_u16(BDA_VIDEO_PAGE_SIZE_ADDR), 0xFA00);

    assert_eq!(mem.read_u8(VGA_MODE13_BASE), 0x00);
    assert_eq!(
        mem.read_u8(VGA_MODE13_BASE + (MODE13_BYTES_PER_PAGE as u32 - 1)),
        0x00
    );
}

#[test]
fn int10_vbe_calls_return_not_supported() {
    let mut bios = Bios::new(BiosConfig::default());
    let mut cpu = RealModeCpu::default();
    let mut mem = TestMemory::new(2 * 1024 * 1024);

    cpu.set_ax(0x4F00);
    call_int10(&mut bios, &mut cpu, &mut mem);

    assert!(cpu.cf());
    assert_eq!(cpu.ax(), 0x024F);
}

#[test]
fn int10_vbe_calls_dispatch_to_handler() {
    struct Probe {
        called: Rc<Cell<bool>>,
    }

    impl VbeServices for Probe {
        fn handle_int10(&mut self, cpu: &mut RealModeCpu, _mem: &mut dyn Memory) {
            self.called.set(true);
            cpu.set_ax(0x004F);
            cpu.set_cf(false);
        }
    }

    let called = Rc::new(Cell::new(false));
    let mut bios = Bios::new(BiosConfig::default());
    bios.set_vbe_handler(Box::new(Probe {
        called: called.clone(),
    }));

    let mut cpu = RealModeCpu::default();
    let mut mem = TestMemory::new(2 * 1024 * 1024);

    cpu.set_ax(0x4F00);
    call_int10(&mut bios, &mut cpu, &mut mem);

    assert!(called.get());
    assert!(!cpu.cf());
    assert_eq!(cpu.ax(), 0x004F);
}
