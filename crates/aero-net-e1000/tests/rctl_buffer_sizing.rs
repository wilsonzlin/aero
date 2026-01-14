use aero_net_e1000::{E1000Device, MAX_L2_FRAME_LEN, MIN_L2_FRAME_LEN};
use memory::MemoryBus;

const REG_RCTL: u32 = 0x0100;
const REG_RDBAL: u32 = 0x2800;
const REG_RDBAH: u32 = 0x2804;
const REG_RDLEN: u32 = 0x2808;
const REG_RDH: u32 = 0x2810;
const REG_RDT: u32 = 0x2818;

const RCTL_EN: u32 = 1 << 1;
const RCTL_BSIZE_SHIFT: u32 = 16;
const RCTL_BSEX: u32 = 1 << 25;

const RXD_ERR_RXE: u8 = 1 << 7;

struct TestDma {
    mem: Vec<u8>,
}

impl TestDma {
    fn new(size: usize) -> Self {
        Self {
            mem: vec![0u8; size],
        }
    }

    fn write(&mut self, addr: u64, bytes: &[u8]) {
        let addr = addr as usize;
        self.mem[addr..addr + bytes.len()].copy_from_slice(bytes);
    }

    fn read_vec(&self, addr: u64, len: usize) -> Vec<u8> {
        let addr = addr as usize;
        self.mem[addr..addr + len].to_vec()
    }
}

impl MemoryBus for TestDma {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let addr = paddr as usize;
        buf.copy_from_slice(&self.mem[addr..addr + buf.len()]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let addr = paddr as usize;
        self.mem[addr..addr + buf.len()].copy_from_slice(buf);
    }
}

fn write_u64_le(dma: &mut TestDma, addr: u64, v: u64) {
    dma.write(addr, &v.to_le_bytes());
}

/// Minimal legacy RX descriptor layout (16 bytes).
fn write_rx_desc(dma: &mut TestDma, addr: u64, buf_addr: u64) {
    write_u64_le(dma, addr, buf_addr);
    dma.write(addr + 8, &0u16.to_le_bytes()); // length
    dma.write(addr + 10, &0u16.to_le_bytes()); // checksum
    dma.write(addr + 12, &[0u8]); // status
    dma.write(addr + 13, &[0u8]); // errors
    dma.write(addr + 14, &0u16.to_le_bytes()); // special
}

fn read_rx_desc_fields(dma: &mut TestDma, addr: u64) -> (u16, u8, u8) {
    let mut desc_bytes = [0u8; 16];
    dma.read_physical(addr, &mut desc_bytes);
    let length = u16::from_le_bytes([desc_bytes[8], desc_bytes[9]]);
    let status = desc_bytes[12];
    let errors = desc_bytes[13];
    (length, status, errors)
}

fn build_frame_with_len(len: usize, fill: u8) -> Vec<u8> {
    assert!(len >= MIN_L2_FRAME_LEN);
    let payload_len = len - MIN_L2_FRAME_LEN;
    let mut frame = Vec::with_capacity(len);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    frame.extend_from_slice(&0x0800u16.to_be_bytes());
    frame.extend(std::iter::repeat_n(fill, payload_len));
    frame
}

#[test]
fn rctl_bsize_bsex_variants_control_rx_dma_truncation_behavior() {
    #[derive(Clone, Copy)]
    struct Variant {
        name: &'static str,
        bsex: bool,
        bsize: u32,
        buf_len: usize,
    }

    let variants = [
        Variant {
            name: "2048",
            bsex: false,
            bsize: 0b00,
            buf_len: 2048,
        },
        Variant {
            name: "1024",
            bsex: false,
            bsize: 0b01,
            buf_len: 1024,
        },
        Variant {
            name: "512",
            bsex: false,
            bsize: 0b10,
            buf_len: 512,
        },
        Variant {
            name: "256",
            bsex: false,
            bsize: 0b11,
            buf_len: 256,
        },
        Variant {
            name: "16k",
            bsex: true,
            bsize: 0b00,
            buf_len: 16 * 1024,
        },
        Variant {
            name: "8k",
            bsex: true,
            bsize: 0b01,
            buf_len: 8 * 1024,
        },
        Variant {
            name: "4k",
            bsex: true,
            bsize: 0b10,
            buf_len: 4 * 1024,
        },
        // The device model treats the reserved (BSEX=1, BSIZE=0b11) case as 2048 (like the
        // implementation's `rx_buf_len()` mapping).
        Variant {
            name: "bsex1_bsize11_reserved",
            bsex: true,
            bsize: 0b11,
            buf_len: 2048,
        },
    ];

    for v in variants {
        let mut dev = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        dev.pci_config_write(0x04, 2, 0x4); // Bus Master Enable
        let mut dma = TestDma::new(0x20_000);

        // Configure RX ring: 2 descriptors at 0x2000 (1 usable).
        dev.mmio_write_u32_reg(REG_RDBAL, 0x2000);
        dev.mmio_write_u32_reg(REG_RDBAH, 0);
        dev.mmio_write_u32_reg(REG_RDLEN, 2 * 16);
        dev.mmio_write_u32_reg(REG_RDH, 0);
        dev.mmio_write_u32_reg(REG_RDT, 1);

        let mut rctl = RCTL_EN | (v.bsize << RCTL_BSIZE_SHIFT);
        if v.bsex {
            rctl |= RCTL_BSEX;
        }
        dev.mmio_write_u32_reg(REG_RCTL, rctl);

        write_rx_desc(&mut dma, 0x2000, 0x3000);
        write_rx_desc(&mut dma, 0x2010, 0x3400);

        // Initialize the guest buffer with a sentinel pattern.
        dma.write(0x3000, &vec![0xCCu8; v.buf_len]);

        // If the configured buffer is smaller than MAX_L2_FRAME_LEN, deliver a frame that is too
        // large for the buffer (but still within the model's MAX_L2_FRAME_LEN) to confirm the
        // device doesn't DMA a truncated frame and sets RXE.
        if v.buf_len < MAX_L2_FRAME_LEN {
            let frame_len = v.buf_len + 1;
            assert!(
                frame_len <= MAX_L2_FRAME_LEN,
                "test invariant: oversize frame must still be <= MAX_L2_FRAME_LEN"
            );
            let frame = build_frame_with_len(frame_len, 0xA5);
            dev.receive_frame(&mut dma, &frame);

            assert_eq!(
                dma.read_vec(0x3000, v.buf_len),
                vec![0xCCu8; v.buf_len],
                "buffer should not be written for variant {}",
                v.name
            );

            let (len, status, errors) = read_rx_desc_fields(&mut dma, 0x2000);
            assert_eq!(len, 0, "descriptor length should be 0 for {}", v.name);
            assert_eq!(status & 0x03, 0x03, "DD|EOP should be set for {}", v.name);
            assert_eq!(
                errors & RXD_ERR_RXE,
                RXD_ERR_RXE,
                "RXE should be set for {}",
                v.name
            );
        } else {
            // Otherwise, the model's MAX_L2_FRAME_LEN always fits; verify a max-size frame is DMA'd
            // correctly and no errors are set.
            let frame = build_frame_with_len(MAX_L2_FRAME_LEN, 0x5A);
            dev.receive_frame(&mut dma, &frame);

            let out = dma.read_vec(0x3000, frame.len());
            assert_eq!(out, frame, "frame bytes mismatch for {}", v.name);
            // The device should not write beyond the frame length.
            assert_eq!(
                dma.read_vec(0x3000 + frame.len() as u64, 16),
                vec![0xCCu8; 16],
                "unexpected buffer overwrite beyond end-of-frame for {}",
                v.name
            );

            let (len, status, errors) = read_rx_desc_fields(&mut dma, 0x2000);
            assert_eq!(
                len as usize, MAX_L2_FRAME_LEN,
                "descriptor length mismatch for {}",
                v.name
            );
            assert_eq!(status & 0x03, 0x03, "DD|EOP should be set for {}", v.name);
            assert_eq!(errors, 0, "unexpected errors for {}", v.name);
        }
    }
}
