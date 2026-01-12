//! Intel HD Audio (HDA) controller + minimal codec model.

mod codec;
mod corb_rirb;
mod pci;
mod regs;
mod stream;

pub use codec::{HdaCodec, HdaVerbResponse};
pub use pci::HdaPciDevice;
pub use regs::{HdaMmioReg, StreamId};
pub use stream::{AudioRingBuffer, StreamFormat};

use codec::CodecAddr;
use corb_rirb::{Corb, Rirb};
use memory::MemoryBus;
use regs::*;
use stream::HdaStream;

/// Intel HD Audio controller (MMIO register block + engine state).
///
/// This is deliberately minimal: enough for OS drivers (Windows 7 `hdaudio.sys`,
/// Linux `snd-hda-intel`) to enumerate a codec and push PCM through a single
/// output stream.
#[derive(Debug)]
pub struct HdaController {
    // Global registers.
    gcap: u16,
    vmin: u8,
    vmaj: u8,
    gctl: u32,
    wakeen: u16,
    statests: u16,
    gsts: u16,
    intctl: u32,
    intsts: u32,
    posbuf: DmaPositionBufferRegs,

    // CORB/RIRB.
    corb: Corb,
    rirb: Rirb,

    // Streams.
    out_stream0: HdaStream,
    in_stream0: HdaStream,

    codec: HdaCodec,
    audio: AudioRingBuffer,
    capture: AudioRingBuffer,

    irq_line: bool,
}

impl Default for HdaController {
    fn default() -> Self {
        Self::new()
    }
}

impl HdaController {
    pub fn new() -> Self {
        let mut controller = Self {
            gcap: gcap_with_streams(1, 1, 0, 1),
            vmin: 0x00,
            vmaj: 0x01,
            gctl: 0x0,
            wakeen: 0,
            statests: 0,
            gsts: 0,
            intctl: 0,
            intsts: 0,
            posbuf: DmaPositionBufferRegs::default(),
            corb: Corb::new(),
            rirb: Rirb::new(),
            out_stream0: HdaStream::new(StreamId::Out0),
            in_stream0: HdaStream::new(StreamId::In0),
            codec: HdaCodec::new_minimal(),
            audio: AudioRingBuffer::new(48000 * 4),
            capture: AudioRingBuffer::new(48000 * 4),
            irq_line: false,
        };
        controller.reset();
        controller
    }

    pub fn irq_line(&self) -> bool {
        self.irq_line
    }

    pub fn audio_ring(&mut self) -> &mut AudioRingBuffer {
        &mut self.audio
    }

    pub fn capture_ring(&mut self) -> &mut AudioRingBuffer {
        &mut self.capture
    }

    pub fn poll(&mut self, mem: &mut dyn MemoryBus) {
        if self.gctl & GCTL_CRST == 0 {
            return;
        }

        // Command/response path.
        self.process_corb(mem);

        // Stream DMA.
        self.out_stream0
            .process(mem, &mut self.audio, &mut self.intsts);
        self.in_stream0
            .process_capture(mem, &mut self.capture, &mut self.intsts);
        self.update_position_buffer(mem);

        self.recalc_intsts_summary();
        self.update_irq_line();
    }

    pub fn mmio_read(&mut self, offset: u32, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        match HdaMmioReg::decode(offset) {
            Some(HdaMmioReg::Gcap) => (self.gcap as u64) & mask_for_size(size),
            Some(HdaMmioReg::Vmin) => (self.vmin as u64) & mask_for_size(size),
            Some(HdaMmioReg::Vmaj) => (self.vmaj as u64) & mask_for_size(size),
            Some(HdaMmioReg::Gctl) => (self.gctl as u64) & mask_for_size(size),
            Some(HdaMmioReg::Wakeen) => (self.wakeen as u64) & mask_for_size(size),
            Some(HdaMmioReg::Statests) => (self.statests as u64) & mask_for_size(size),
            Some(HdaMmioReg::Gsts) => (self.gsts as u64) & mask_for_size(size),
            Some(HdaMmioReg::Intctl) => (self.intctl as u64) & mask_for_size(size),
            Some(HdaMmioReg::Intsts) => (self.intsts as u64) & mask_for_size(size),
            Some(HdaMmioReg::Dplbase) => (self.posbuf.dplbase() as u64) & mask_for_size(size),
            Some(HdaMmioReg::Dpubase) => (self.posbuf.dpubase() as u64) & mask_for_size(size),
            Some(HdaMmioReg::Corb(reg)) => self.corb.mmio_read(reg, size),
            Some(HdaMmioReg::Rirb(reg)) => self.rirb.mmio_read(reg, size),
            Some(HdaMmioReg::Stream0(reg)) => self.out_stream0.mmio_read(reg, size),
            Some(HdaMmioReg::Stream1(reg)) => self.in_stream0.mmio_read(reg, size),
            None => 0,
        }
    }

    pub fn mmio_write(&mut self, offset: u32, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        match HdaMmioReg::decode(offset) {
            Some(HdaMmioReg::Gctl) => {
                let new_val = (value as u32) & mask_for_size(size) as u32;
                let old_crst = self.gctl & GCTL_CRST;
                self.gctl = (self.gctl & !mask_for_size(size) as u32) | new_val;

                let new_crst = self.gctl & GCTL_CRST;
                if old_crst != 0 && new_crst == 0 {
                    self.reset();
                } else if old_crst == 0 && new_crst != 0 {
                    // Leaving reset: expose codec presence immediately.
                    self.statests |= 1;
                }

                self.recalc_intsts_summary();
                self.update_irq_line();
            }
            Some(HdaMmioReg::Wakeen) => {
                self.wakeen = (value as u16) & mask_for_size(size) as u16;
            }
            Some(HdaMmioReg::Statests) => {
                // Write-1-to-clear.
                let mask = (value as u16) & mask_for_size(size) as u16;
                self.statests &= !mask;
            }
            Some(HdaMmioReg::Gsts) => {
                // Mostly read-only in real hardware; accept W1C for now.
                let mask = (value as u16) & mask_for_size(size) as u16;
                self.gsts &= !mask;
            }
            Some(HdaMmioReg::Intctl) => {
                self.intctl = (value as u32) & mask_for_size(size) as u32;
                self.recalc_intsts_summary();
                self.update_irq_line();
            }
            Some(HdaMmioReg::Intsts) => {
                // W1C for all non-summary bits.
                let mask = (value as u32) & mask_for_size(size) as u32;
                self.intsts &= !mask;
                self.recalc_intsts_summary();
                self.update_irq_line();
            }
            Some(HdaMmioReg::Dplbase) => {
                self.posbuf
                    .write_dplbase((value as u32) & mask_for_size(size) as u32);
            }
            Some(HdaMmioReg::Dpubase) => {
                self.posbuf
                    .write_dpubase((value as u32) & mask_for_size(size) as u32);
            }
            Some(HdaMmioReg::Corb(reg)) => {
                self.corb.mmio_write(reg, size, value);
            }
            Some(HdaMmioReg::Rirb(reg)) => {
                self.rirb.mmio_write(reg, size, value);
                self.recalc_intsts_summary();
                self.update_irq_line();
            }
            Some(HdaMmioReg::Stream0(reg)) => {
                self.out_stream0
                    .mmio_write(reg, size, value, &mut self.intsts);
                self.recalc_intsts_summary();
                self.update_irq_line();
            }
            Some(HdaMmioReg::Stream1(reg)) => {
                self.in_stream0
                    .mmio_write(reg, size, value, &mut self.intsts);
                self.recalc_intsts_summary();
                self.update_irq_line();
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.gctl &= !GCTL_CRST;
        self.wakeen = 0;
        self.statests = 0;
        self.gsts = 0;
        self.intctl = 0;
        self.intsts = 0;
        self.posbuf = DmaPositionBufferRegs::default();
        self.corb.reset();
        self.rirb.reset();
        self.out_stream0.reset();
        self.in_stream0.reset();
        self.codec.reset();
        self.audio.clear();
        self.capture.clear();
        self.irq_line = false;
    }

    fn update_position_buffer(&mut self, mem: &mut dyn MemoryBus) {
        if let Some(entry_addr) = self.posbuf.stream_entry_addr(StreamId::Out0.posbuf_index()) {
            mem.write_u32(entry_addr, self.out_stream0.lpib());
            mem.write_u32(entry_addr + 4, 0);
        }
        if let Some(entry_addr) = self.posbuf.stream_entry_addr(StreamId::In0.posbuf_index()) {
            mem.write_u32(entry_addr, self.in_stream0.lpib());
            mem.write_u32(entry_addr + 4, 0);
        }
    }

    fn process_corb(&mut self, mem: &mut dyn MemoryBus) {
        if !self.corb.is_running() || !self.rirb.is_running() {
            return;
        }

        while let Some(cmd) = self.corb.pop_command(mem) {
            let CodecAddr(codec_addr) = cmd.codec;
            let resp = if codec_addr != 0 {
                HdaVerbResponse {
                    data: 0,
                    ext: codec_addr as u32,
                }
            } else {
                let data = self.codec.execute_verb(cmd.nid, cmd.verb);
                HdaVerbResponse {
                    data,
                    ext: codec_addr as u32,
                }
            };
            self.rirb.push_response(mem, resp, &mut self.intsts);
        }

        self.recalc_intsts_summary();
        self.update_irq_line();
    }

    fn recalc_intsts_summary(&mut self) {
        let causes = self.intsts & !(INTSTS_GIS);
        if causes != 0 {
            self.intsts |= INTSTS_GIS;
        } else {
            self.intsts &= !INTSTS_GIS;
        }
    }

    fn update_irq_line(&mut self) {
        let gie = self.intctl & INTCTL_GIE != 0;
        let cie = self.intctl & INTCTL_CIE != 0;
        let sis0_en = self.intctl & INTCTL_SIE0 != 0;
        let sis1_en = self.intctl & INTCTL_SIE1 != 0;

        let mut pending = false;
        if gie {
            if (self.intsts & INTSTS_CIS != 0) && cie {
                pending = true;
            }
            if (self.intsts & INTSTS_SIS0 != 0) && sis0_en {
                pending = true;
            }
            if (self.intsts & INTSTS_SIS1 != 0) && sis1_en {
                pending = true;
            }
        }
        self.irq_line = pending;
    }
}

fn mask_for_size(size: usize) -> u64 {
    if size == 0 {
        return 0;
    }
    if size >= 8 {
        return u64::MAX;
    }
    (1u64 << (size * 8)) - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_for_size_supports_non_pow2_sizes() {
        assert_eq!(mask_for_size(0), 0);
        assert_eq!(mask_for_size(1), 0xFF);
        assert_eq!(mask_for_size(2), 0xFFFF);
        assert_eq!(mask_for_size(3), 0x00FF_FFFF);
        assert_eq!(mask_for_size(4), 0xFFFF_FFFF);
        assert_eq!(mask_for_size(5), 0x0000_00FF_FFFF_FFFF);
        assert_eq!(mask_for_size(6), 0x0000_FFFF_FFFF_FFFF);
        assert_eq!(mask_for_size(7), 0x00FF_FFFF_FFFF_FFFF);
        assert_eq!(mask_for_size(8), u64::MAX);
        assert_eq!(mask_for_size(9), u64::MAX);
    }

    #[derive(Clone, Debug)]
    struct TestMem {
        data: Vec<u8>,
    }

    impl TestMem {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
            }
        }

        fn read_u64(&mut self, addr: u64) -> u64 {
            let mut buf = [0u8; 8];
            self.read_physical(addr, &mut buf);
            u64::from_le_bytes(buf)
        }

        fn write_u64(&mut self, addr: u64, value: u64) {
            self.write_physical(addr, &value.to_le_bytes());
        }
    }

    impl MemoryBus for TestMem {
        fn read_physical(&mut self, addr: u64, dst: &mut [u8]) {
            let addr = addr as usize;
            dst.copy_from_slice(&self.data[addr..addr + dst.len()]);
        }

        fn write_physical(&mut self, addr: u64, src: &[u8]) {
            let addr = addr as usize;
            self.data[addr..addr + src.len()].copy_from_slice(src);
        }
    }

    #[test]
    fn corb_command_writes_rirb_response_and_interrupts() {
        let mut mem = TestMem::new(0x10_000);
        let mut hda = HdaController::new();

        // Leave reset.
        hda.mmio_write(HDA_GCTL, 4, GCTL_CRST as u64);

        // Enable controller interrupt + global interrupt.
        hda.mmio_write(HDA_INTCTL, 4, (INTCTL_GIE | INTCTL_CIE) as u64);

        // Configure CORB and RIRB with two entries each (size code 0).
        let corb_base = 0x2000u64;
        let rirb_base = 0x3000u64;
        hda.mmio_write(HDA_CORBLBASE, 4, corb_base);
        hda.mmio_write(HDA_CORBUBASE, 4, 0);
        hda.mmio_write(HDA_CORBSIZE, 1, 0);
        hda.mmio_write(HDA_RIRBLBASE, 4, rirb_base);
        hda.mmio_write(HDA_RIRBUBASE, 4, 0);
        hda.mmio_write(HDA_RIRBSIZE, 1, 0);
        hda.mmio_write(HDA_RINTCNT, 2, 1);

        // Start rings (RIRB interrupt enable too).
        hda.mmio_write(HDA_CORBCTL, 1, CORBCTL_RUN as u64);
        hda.mmio_write(HDA_RIRBCTL, 1, (RIRBCTL_RUN | RIRBCTL_INTCTL) as u64);

        // Write a GET_PARAMETER(VENDOR_ID) verb to CORB entry 1 and advance WP to 1.
        let verb = 0xF00u32 << 8;
        let cmd = verb;
        mem.write_u32(corb_base + 4, cmd);
        hda.mmio_write(HDA_CORBWP, 2, 1);

        // Process.
        hda.poll(&mut mem);

        let resp0 = mem.read_u64(rirb_base + 8);
        assert_eq!(resp0 as u32, hda.codec.vendor_id());
        assert!(hda.mmio_read(HDA_INTSTS, 4) as u32 & INTSTS_CIS != 0);
        assert!(hda.irq_line());

        // Clear controller interrupt status and verify IRQ deasserts.
        hda.mmio_write(HDA_INTSTS, 4, INTSTS_CIS as u64);
        assert_eq!(hda.mmio_read(HDA_INTSTS, 4) as u32 & INTSTS_CIS, 0);
        assert!(!hda.irq_line());
    }

    #[test]
    fn stream_bdl_processing_advances_lpib_and_queues_bytes() {
        let mut mem = TestMem::new(0x20_000);
        let mut hda = HdaController::new();
        hda.mmio_write(HDA_GCTL, 4, GCTL_CRST as u64);

        // Enable stream interrupt + global interrupt.
        hda.mmio_write(HDA_INTCTL, 4, (INTCTL_GIE | INTCTL_SIE0) as u64);

        let bdl_base = 0x4000u64;
        let buf0 = 0x5000u64;
        let buf1 = 0x6000u64;

        // Two BDL entries (LVI = 1). Only second has IOC.
        // Entry 0.
        mem.write_u64(bdl_base, buf0);
        mem.write_u32(bdl_base + 8, 8);
        mem.write_u32(bdl_base + 12, 0);
        // Entry 1.
        mem.write_u64(bdl_base + 16, buf1);
        mem.write_u32(bdl_base + 24, 8);
        mem.write_u32(bdl_base + 28, 1);

        mem.write_physical(buf0, &[1, 2, 3, 4, 5, 6, 7, 8]);
        mem.write_physical(buf1, &[9, 10, 11, 12, 13, 14, 15, 16]);

        // Program stream descriptor.
        hda.mmio_write(HDA_SD0BDPL, 4, bdl_base);
        hda.mmio_write(HDA_SD0BDPU, 4, 0);
        hda.mmio_write(HDA_SD0LVI, 2, 1);
        hda.mmio_write(HDA_SD0CBL, 4, 32); // bigger than total BDL length; no wrap in this test
        hda.mmio_write(HDA_SD0FMT, 2, 0x0011); // 48kHz, 16-bit, 2ch-ish

        // Start stream.
        let ctl = (SD_CTL_SRST | SD_CTL_RUN | SD_CTL_IOCE) as u64;
        hda.mmio_write(HDA_SD0CTL, 4, ctl);

        // One poll consumes one BDL entry (8 bytes).
        hda.poll(&mut mem);
        assert_eq!(hda.mmio_read(HDA_SD0LPIB, 4) as u32, 8);

        // Second poll consumes the second entry and triggers IOC.
        hda.poll(&mut mem);
        assert_eq!(hda.mmio_read(HDA_SD0LPIB, 4) as u32, 16);

        let queued = hda.audio_ring().drain_all();
        assert_eq!(
            queued,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );

        // IOC on second entry sets stream interrupt.
        assert!(hda.mmio_read(HDA_INTSTS, 4) as u32 & INTSTS_SIS0 != 0);
        assert!(hda.irq_line());

        // Clear stream status (SDSTS is in high byte of SDCTL register).
        hda.mmio_write(HDA_SD0CTL, 4, (SD_STS_BCIS as u64) << 24);
        assert_eq!(hda.mmio_read(HDA_INTSTS, 4) as u32 & INTSTS_SIS0, 0);
    }

    #[test]
    fn position_buffer_is_not_written_when_disabled() {
        let mut mem = TestMem::new(0x40_000);
        let mut hda = HdaController::new();
        hda.mmio_write(HDA_GCTL, 4, GCTL_CRST as u64);

        let posbuf_base = 0x7000u64;
        mem.write_u32(posbuf_base, 0xdead_beef);
        hda.mmio_write(HDA_DPUBASE, 4, 0);
        hda.mmio_write(HDA_DPLBASE, 4, posbuf_base);

        let bdl_base = 0x4000u64;
        let buf0 = 0x5000u64;
        mem.write_u64(bdl_base, buf0);
        mem.write_u32(bdl_base + 8, 8);
        mem.write_u32(bdl_base + 12, 0);
        mem.write_physical(buf0, &[1, 2, 3, 4, 5, 6, 7, 8]);

        hda.mmio_write(HDA_SD0BDPL, 4, bdl_base);
        hda.mmio_write(HDA_SD0BDPU, 4, 0);
        hda.mmio_write(HDA_SD0LVI, 2, 0);
        hda.mmio_write(HDA_SD0CBL, 4, 16);
        hda.mmio_write(HDA_SD0FMT, 2, 0x0011);

        hda.mmio_write(HDA_SD0CTL, 4, (SD_CTL_SRST | SD_CTL_RUN) as u64);
        hda.poll(&mut mem);

        assert_eq!(hda.mmio_read(HDA_SD0LPIB, 4) as u32, 8);
        assert_eq!(mem.read_u32(posbuf_base), 0xdead_beef);
    }

    #[test]
    fn position_buffer_tracks_lpib_and_wraps_at_cbl() {
        let mut mem = TestMem::new(0x80_000);
        let mut hda = HdaController::new();
        hda.mmio_write(HDA_GCTL, 4, GCTL_CRST as u64);

        let posbuf_base = 0x7000u64;
        hda.mmio_write(HDA_DPUBASE, 4, 0);
        hda.mmio_write(HDA_DPLBASE, 4, posbuf_base | DPLBASE_ENABLE as u64);

        let bdl_base = 0x4000u64;
        let buf0 = 0x5000u64;
        let buf1 = 0x6000u64;

        mem.write_u64(bdl_base, buf0);
        mem.write_u32(bdl_base + 8, 300);
        mem.write_u32(bdl_base + 12, 0);
        mem.write_u64(bdl_base + 16, buf1);
        mem.write_u32(bdl_base + 24, 300);
        mem.write_u32(bdl_base + 28, 1);

        mem.write_physical(buf0, &[0xaa; 300]);
        mem.write_physical(buf1, &[0xbb; 300]);

        hda.mmio_write(HDA_SD0BDPL, 4, bdl_base);
        hda.mmio_write(HDA_SD0BDPU, 4, 0);
        hda.mmio_write(HDA_SD0LVI, 2, 1);
        hda.mmio_write(HDA_SD0CBL, 4, 600);
        hda.mmio_write(HDA_SD0FMT, 2, 0x0011);
        hda.mmio_write(HDA_SD0CTL, 4, (SD_CTL_SRST | SD_CTL_RUN) as u64);

        hda.poll(&mut mem);
        assert_eq!(hda.mmio_read(HDA_SD0LPIB, 4) as u32, 300);
        assert_eq!(mem.read_u32(posbuf_base), 300);

        hda.poll(&mut mem);
        assert_eq!(hda.mmio_read(HDA_SD0LPIB, 4) as u32, 0);
        assert_eq!(mem.read_u32(posbuf_base), 0);
    }

    #[test]
    fn capture_stream_writes_bytes_into_guest_memory() {
        let mut mem = TestMem::new(0x20_000);
        let mut hda = HdaController::new();
        hda.mmio_write(HDA_GCTL, 4, GCTL_CRST as u64);

        let posbuf_base = 0x7000u64;
        hda.mmio_write(HDA_DPUBASE, 4, 0);
        hda.mmio_write(HDA_DPLBASE, 4, posbuf_base | DPLBASE_ENABLE as u64);

        // Enable stream interrupt + global interrupt for stream 1 (input).
        hda.mmio_write(HDA_INTCTL, 4, (INTCTL_GIE | INTCTL_SIE1) as u64);

        // Provide 8 bytes of "mic" data to be captured.
        hda.capture_ring().push(&[1, 2, 3, 4, 5, 6, 7, 8]);

        let bdl_base = 0x4000u64;
        let buf0 = 0x5000u64;

        // One BDL entry, IOC set.
        mem.write_u64(bdl_base, buf0);
        mem.write_u32(bdl_base + 8, 8);
        mem.write_u32(bdl_base + 12, 1);

        // Program stream 1 descriptor.
        hda.mmio_write(HDA_SD1BDPL, 4, bdl_base);
        hda.mmio_write(HDA_SD1BDPU, 4, 0);
        hda.mmio_write(HDA_SD1LVI, 2, 0);
        hda.mmio_write(HDA_SD1CBL, 4, 32);
        hda.mmio_write(HDA_SD1FMT, 2, 0x0010); // 48kHz, 16-bit, mono

        // Start stream.
        let ctl = (SD_CTL_SRST | SD_CTL_RUN | SD_CTL_IOCE) as u64;
        hda.mmio_write(HDA_SD1CTL, 4, ctl);

        hda.poll(&mut mem);

        assert_eq!(hda.mmio_read(HDA_SD1LPIB, 4) as u32, 8);
        assert_eq!(mem.read_u32(posbuf_base + 8), 8);
        let mut out = [0u8; 8];
        mem.read_physical(buf0, &mut out);
        assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8]);

        // IOC sets stream interrupt.
        assert!(hda.mmio_read(HDA_INTSTS, 4) as u32 & INTSTS_SIS1 != 0);
        assert!(hda.irq_line());

        // Clear stream status.
        hda.mmio_write(HDA_SD1CTL, 4, (SD_STS_BCIS as u64) << 24);
        assert_eq!(hda.mmio_read(HDA_INTSTS, 4) as u32 & INTSTS_SIS1, 0);
    }
}
