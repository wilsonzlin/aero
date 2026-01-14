//! Intel HD Audio (HDA) controller + minimal codec model.

mod codec;
mod corb_rirb;
mod pci;
pub mod regs;
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
        let mut out = 0u64;
        let capped = size.min(8);
        for i in 0..capped {
            out |= u64::from(self.mmio_read_u8(offset + i as u32)) << (i * 8);
        }
        out
    }

    pub fn mmio_write(&mut self, offset: u32, size: usize, value: u64) {
        // Some guests use a 32-bit write to SDnCTL where the upper byte is used as a write-1-to-clear
        // mask for SDnSTS. If the low 24 bits are all zero, treat it as a status-only write so we
        // don't accidentally stop the stream (matches canonical `aero-audio` behaviour).
        if size == 4 {
            let v = value as u32;
            let sts_clear = (v >> 24) as u8;
            let ctl = v & 0x00ff_ffff;
            match offset {
                HDA_SD0CTL => {
                    if sts_clear != 0 {
                        self.out_stream0.mmio_write_byte(
                            StreamReg::CtlSts,
                            3,
                            sts_clear,
                            &mut self.intsts,
                        );
                    }
                    if ctl != 0 || sts_clear == 0 {
                        for i in 0..3u8 {
                            self.out_stream0.mmio_write_byte(
                                StreamReg::CtlSts,
                                i,
                                ((ctl >> (u32::from(i) * 8)) & 0xff) as u8,
                                &mut self.intsts,
                            );
                        }
                    }

                    self.recalc_intsts_summary();
                    self.update_irq_line();
                    return;
                }
                HDA_SD1CTL => {
                    if sts_clear != 0 {
                        self.in_stream0.mmio_write_byte(
                            StreamReg::CtlSts,
                            3,
                            sts_clear,
                            &mut self.intsts,
                        );
                    }
                    if ctl != 0 || sts_clear == 0 {
                        for i in 0..3u8 {
                            self.in_stream0.mmio_write_byte(
                                StreamReg::CtlSts,
                                i,
                                ((ctl >> (u32::from(i) * 8)) & 0xff) as u8,
                                &mut self.intsts,
                            );
                        }
                    }

                    self.recalc_intsts_summary();
                    self.update_irq_line();
                    return;
                }
                _ => {}
            }
        }

        let capped = size.min(8);
        for i in 0..capped {
            let b = ((value >> (i * 8)) & 0xff) as u8;
            self.mmio_write_u8(offset + i as u32, b);
        }

        self.recalc_intsts_summary();
        self.update_irq_line();
    }

    fn mmio_read_u8(&mut self, offset: u32) -> u8 {
        let Some(decoded) = HdaMmioReg::decode_byte(offset) else {
            return 0;
        };

        let shift = u32::from(decoded.byte) * 8;
        match decoded.reg {
            HdaMmioReg::Gcap => ((u32::from(self.gcap) >> shift) & 0xff) as u8,
            HdaMmioReg::Vmin => self.vmin,
            HdaMmioReg::Vmaj => self.vmaj,
            HdaMmioReg::Gctl => ((self.gctl >> shift) & 0xff) as u8,
            HdaMmioReg::Wakeen => ((u32::from(self.wakeen) >> shift) & 0xff) as u8,
            HdaMmioReg::Statests => ((u32::from(self.statests) >> shift) & 0xff) as u8,
            HdaMmioReg::Gsts => ((u32::from(self.gsts) >> shift) & 0xff) as u8,
            HdaMmioReg::Intctl => ((self.intctl >> shift) & 0xff) as u8,
            HdaMmioReg::Intsts => ((self.intsts >> shift) & 0xff) as u8,
            HdaMmioReg::Dplbase => ((self.posbuf.dplbase() >> shift) & 0xff) as u8,
            HdaMmioReg::Dpubase => ((self.posbuf.dpubase() >> shift) & 0xff) as u8,
            HdaMmioReg::Corb(reg) => {
                let width = match reg {
                    CorbReg::Lbase | CorbReg::Ubase => 4,
                    CorbReg::Wp | CorbReg::Rp => 2,
                    CorbReg::Ctl | CorbReg::Sts | CorbReg::Size => 1,
                };
                let full = self.corb.mmio_read(reg, width);
                ((full >> shift) & 0xff) as u8
            }
            HdaMmioReg::Rirb(reg) => {
                let width = match reg {
                    RirbReg::Lbase | RirbReg::Ubase => 4,
                    RirbReg::Wp | RirbReg::RintCnt => 2,
                    RirbReg::Ctl | RirbReg::Sts | RirbReg::Size => 1,
                };
                let full = self.rirb.mmio_read(reg, width);
                ((full >> shift) & 0xff) as u8
            }
            HdaMmioReg::Stream0(reg) => self.out_stream0.mmio_read_byte(reg, decoded.byte),
            HdaMmioReg::Stream1(reg) => self.in_stream0.mmio_read_byte(reg, decoded.byte),
        }
    }

    fn mmio_write_u8(&mut self, offset: u32, value: u8) {
        let Some(decoded) = HdaMmioReg::decode_byte(offset) else {
            return;
        };

        let shift = u32::from(decoded.byte) * 8;
        match decoded.reg {
            // Read-only registers.
            HdaMmioReg::Gcap | HdaMmioReg::Vmin | HdaMmioReg::Vmaj => {}

            HdaMmioReg::Gctl => {
                let old = self.gctl;
                let mask = 0xffu32 << shift;
                self.gctl = (self.gctl & !mask) | (u32::from(value) << shift);

                let old_crst = old & GCTL_CRST;
                let new_crst = self.gctl & GCTL_CRST;
                if old_crst != 0 && new_crst == 0 {
                    self.reset();
                } else if old_crst == 0 && new_crst != 0 {
                    // Leaving reset: expose codec presence immediately.
                    self.statests |= 1;
                }
            }
            HdaMmioReg::Wakeen => {
                let mask = 0xffu16 << shift;
                self.wakeen = (self.wakeen & !mask) | (u16::from(value) << shift);
            }
            HdaMmioReg::Statests => {
                // Write-1-to-clear.
                let mask = u16::from(value) << shift;
                self.statests &= !mask;
            }
            HdaMmioReg::Gsts => {
                // Mostly read-only in real hardware; accept W1C for now.
                let mask = u16::from(value) << shift;
                self.gsts &= !mask;
            }
            HdaMmioReg::Intctl => {
                let mask = 0xffu32 << shift;
                self.intctl = (self.intctl & !mask) | (u32::from(value) << shift);
            }
            HdaMmioReg::Intsts => {
                // W1C for all non-summary bits.
                let mask = u32::from(value) << shift;
                self.intsts &= !mask;
            }
            HdaMmioReg::Dplbase => {
                let old = self.posbuf.dplbase();
                let mask = 0xffu32 << shift;
                let new = (old & !mask) | (u32::from(value) << shift);
                self.posbuf.write_dplbase(new);
            }
            HdaMmioReg::Dpubase => {
                let old = self.posbuf.dpubase();
                let mask = 0xffu32 << shift;
                let new = (old & !mask) | (u32::from(value) << shift);
                self.posbuf.write_dpubase(new);
            }
            HdaMmioReg::Corb(reg) => {
                let width = match reg {
                    CorbReg::Lbase | CorbReg::Ubase => 4,
                    CorbReg::Wp | CorbReg::Rp => 2,
                    CorbReg::Ctl | CorbReg::Sts | CorbReg::Size => 1,
                };
                if width == 1 {
                    self.corb.mmio_write(reg, 1, value as u64);
                    return;
                }

                let full = self.corb.mmio_read(reg, width);
                let mask = 0xffu64 << shift;
                let new = (full & !mask) | (u64::from(value) << shift);
                self.corb.mmio_write(reg, width, new);
            }
            HdaMmioReg::Rirb(reg) => {
                let width = match reg {
                    RirbReg::Lbase | RirbReg::Ubase => 4,
                    RirbReg::Wp | RirbReg::RintCnt => 2,
                    RirbReg::Ctl | RirbReg::Sts | RirbReg::Size => 1,
                };
                if width == 1 {
                    self.rirb.mmio_write(reg, 1, value as u64);
                    return;
                }

                let full = self.rirb.mmio_read(reg, width);
                let mask = 0xffu64 << shift;
                let new = (full & !mask) | (u64::from(value) << shift);
                self.rirb.mmio_write(reg, width, new);
            }
            HdaMmioReg::Stream0(reg) => {
                self.out_stream0
                    .mmio_write_byte(reg, decoded.byte, value, &mut self.intsts)
            }
            HdaMmioReg::Stream1(reg) => {
                self.in_stream0
                    .mmio_write_byte(reg, decoded.byte, value, &mut self.intsts)
            }
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

        // Process a bounded number of commands per poll to avoid hangs even if the guest
        // programs an invalid CORB read/write pointer pair.
        //
        // CORB pointers are expected to always be within the current ring size, but the guest
        // can shrink CORBSIZE after setting CORBWP for a larger ring. That would previously
        // cause `Corb::pop_command` to never observe `rp == wp`, resulting in an infinite loop.
        self.corb.sanitize_pointers();
        let max_cmds = self.corb.entries() as usize;
        for _ in 0..max_cmds {
            let Some(cmd) = self.corb.pop_command(mem) else {
                break;
            };
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

    #[derive(Default)]
    struct PanicMem;

    impl MemoryBus for PanicMem {
        fn read_physical(&mut self, _addr: u64, _dst: &mut [u8]) {
            panic!("unexpected guest memory read");
        }

        fn write_physical(&mut self, _addr: u64, _src: &[u8]) {
            panic!("unexpected guest memory write");
        }
    }

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

    #[test]
    fn gcap_vmin_vmaj_can_be_read_as_one_dword() {
        let mut hda = HdaController::new();

        let gcap = hda.mmio_read(HDA_GCAP, 2) as u32;
        let vmin = hda.mmio_read(HDA_VMIN, 1) as u32;
        let vmaj = hda.mmio_read(HDA_VMAJ, 1) as u32;

        // Sanity check to ensure we're not trivially packing zeroes.
        assert_eq!(vmin, 0);
        assert_eq!(vmaj, 1);

        let packed = hda.mmio_read(HDA_GCAP, 4) as u32;
        assert_eq!(packed, gcap | (vmin << 16) | (vmaj << 24));
    }

    #[test]
    fn stream_status_is_byte_accessible_and_w1c() {
        let mut hda = HdaController::new();

        // Force a status byte with bits 0..=2 set.
        hda.out_stream0.test_set_sts(0b111);

        let sts_off = HDA_SD0CTL + 3;
        assert_eq!(hda.mmio_read(sts_off, 1) as u8, 0b111);

        // W1C the BCIS bit.
        hda.mmio_write(sts_off, 1, 1u64 << 2);
        assert_eq!(hda.mmio_read(sts_off, 1) as u8, 0b011);
    }

    #[test]
    fn stream_lvi_and_fifow_allow_word_access() {
        let mut hda = HdaController::new();

        hda.mmio_write(HDA_SD0LVI, 2, 0x1234);
        hda.mmio_write(HDA_SD0FIFOW, 2, 0x5678);

        // SDnLVI is 8 bits in the Intel HDA spec; upper bits are reserved and must read as 0.
        assert_eq!(hda.mmio_read(HDA_SD0LVI, 2) as u16, 0x0034);
        assert_eq!(hda.mmio_read(HDA_SD0FIFOW, 2) as u16, 0x5678);

        // A 32-bit access at SD0LVI spans LVI + FIFOW.
        assert_eq!(hda.mmio_read(HDA_SD0LVI, 4) as u32, 0x5678_0034);
    }

    #[test]
    fn stream_status_dword_write_does_not_clear_ctl_bits() {
        let mut hda = HdaController::new();

        // Set some control bits, and a status byte with BCIS set.
        hda.mmio_write(
            HDA_SD0CTL,
            4,
            (SD_CTL_SRST | SD_CTL_RUN | SD_CTL_IOCE) as u64,
        );
        hda.out_stream0.test_set_sts(0b111);

        let before = hda.mmio_read(HDA_SD0CTL, 4) as u32;
        let before_ctl = before & 0x00ff_ffff;
        assert_ne!(before_ctl, 0);

        // Clear BCIS via a dword write that only sets the status byte.
        hda.mmio_write(HDA_SD0CTL, 4, (SD_STS_BCIS as u64) << 24);

        let after = hda.mmio_read(HDA_SD0CTL, 4) as u32;
        let after_ctl = after & 0x00ff_ffff;
        let after_sts = (after >> 24) as u8;

        assert_eq!(after_ctl, before_ctl);
        assert_eq!(after_sts, 0b011);
    }

    #[test]
    fn mmio_reads_and_writes_do_not_panic_for_common_sizes() {
        let mut hda = HdaController::new();
        let limit = HdaPciDevice::MMIO_BAR_SIZE;

        for offset in 0..limit {
            for size in [1usize, 2, 4] {
                if offset + (size as u32) > limit {
                    continue;
                }
                let _ = hda.mmio_read(offset, size);
                hda.mmio_write(offset, size, 0);
            }
        }
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

    #[derive(Debug)]
    struct ReadPanicMem {
        data: Vec<u8>,
        reads: usize,
        max_reads: usize,
    }

    impl ReadPanicMem {
        fn new(size: usize, max_reads: usize) -> Self {
            Self {
                data: vec![0; size],
                reads: 0,
                max_reads,
            }
        }
    }

    impl MemoryBus for ReadPanicMem {
        fn read_physical(&mut self, addr: u64, dst: &mut [u8]) {
            self.reads = self.reads.saturating_add(1);
            assert!(
                self.reads <= self.max_reads,
                "guest memory reads exceeded limit ({}), likely due to an infinite loop",
                self.max_reads
            );
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
    fn corb_size_shrink_with_stale_wp_does_not_spin_forever() {
        // Regression test: if CORBSIZE is shrunk after programming CORBWP for a larger ring,
        // a stale (now out-of-range) WP must not cause CORB processing to spin indefinitely.
        let mut mem = ReadPanicMem::new(0x10_000, 64);
        let mut hda = HdaController::new();

        // Leave reset.
        hda.mmio_write(HDA_GCTL, 4, GCTL_CRST as u64);

        // Program ring buffer bases.
        let corb_base = 0x2000u64;
        let rirb_base = 0x3000u64;
        hda.mmio_write(HDA_CORBLBASE, 4, corb_base);
        hda.mmio_write(HDA_CORBUBASE, 4, 0);
        hda.mmio_write(HDA_RIRBLBASE, 4, rirb_base);
        hda.mmio_write(HDA_RIRBUBASE, 4, 0);

        // Start with a large CORB, then write a WP that would be out-of-range for a 2-entry ring.
        hda.mmio_write(HDA_CORBSIZE, 1, 2); // 256 entries
        hda.mmio_write(HDA_CORBWP, 2, 5);
        assert_eq!(hda.mmio_read(HDA_CORBWP, 2), 5);

        // Shrink CORB to 2 entries without changing WP.
        hda.mmio_write(HDA_CORBSIZE, 1, 0); // 2 entries

        // Provide at least one valid command in the new ring range.
        mem.write_u32(corb_base + 4, 0);

        // Configure a minimal RIRB; it must be running for CORB processing to happen.
        hda.mmio_write(HDA_RIRBSIZE, 1, 0);
        hda.mmio_write(HDA_RINTCNT, 2, 1);
        hda.mmio_write(HDA_CORBCTL, 1, CORBCTL_RUN as u64);
        hda.mmio_write(HDA_RIRBCTL, 1, RIRBCTL_RUN as u64);

        // If CORB processing regresses into an infinite loop, ReadPanicMem will abort quickly.
        hda.poll(&mut mem);
    }

    #[test]
    fn corb_rirb_base_ignores_low_reserved_bits() {
        let mut mem = TestMem::new(0x10_000);
        let mut hda = HdaController::new();

        // Leave reset.
        hda.mmio_write(HDA_GCTL, 4, GCTL_CRST as u64);

        // Enable controller interrupt + global interrupt.
        hda.mmio_write(HDA_INTCTL, 4, (INTCTL_GIE | INTCTL_CIE) as u64);

        // Program CORB/RIRB base addresses with low bits set; per spec these are
        // reserved (128-byte alignment) and must be ignored by the DMA engines.
        let corb_base = 0x2000u64;
        let rirb_base = 0x3000u64;
        hda.mmio_write(HDA_CORBLBASE, 4, corb_base | 0x7f);
        hda.mmio_write(HDA_CORBUBASE, 4, 0);
        hda.mmio_write(HDA_CORBSIZE, 1, 0);
        hda.mmio_write(HDA_RIRBLBASE, 4, rirb_base | 0x7f);
        hda.mmio_write(HDA_RIRBUBASE, 4, 0);
        hda.mmio_write(HDA_RIRBSIZE, 1, 0);
        hda.mmio_write(HDA_RINTCNT, 2, 1);

        // Start rings (RIRB interrupt enable too).
        hda.mmio_write(HDA_CORBCTL, 1, CORBCTL_RUN as u64);
        hda.mmio_write(HDA_RIRBCTL, 1, (RIRBCTL_RUN | RIRBCTL_INTCTL) as u64);

        // Write a GET_PARAMETER(VENDOR_ID) verb to CORB entry 1 at the aligned
        // base address and advance WP to 1.
        let verb = 0xF00u32 << 8;
        mem.write_u32(corb_base + 4, verb);
        hda.mmio_write(HDA_CORBWP, 2, 1);

        hda.poll(&mut mem);

        // Response must appear at aligned base address too.
        let resp0 = mem.read_u64(rirb_base + 8);
        assert_eq!(resp0 as u32, hda.codec.vendor_id());
        assert!(hda.mmio_read(HDA_INTSTS, 4) as u32 & INTSTS_CIS != 0);
        assert!(hda.irq_line());
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

    #[test]
    fn poll_corb_overflow_does_not_touch_guest_memory() {
        let mut mem = PanicMem;
        let mut hda = HdaController::new();

        // Leave reset.
        hda.mmio_write(HDA_GCTL, 4, GCTL_CRST as u64);

        // CORB base is 128-byte aligned; use a large ring index (rp=31 -> next_rp=32) so that
        // base + rp*4 overflows.
        hda.mmio_write(HDA_CORBUBASE, 4, 0xFFFF_FFFF);
        hda.mmio_write(HDA_CORBLBASE, 4, 0xFFFF_FFFF);
        hda.mmio_write(HDA_CORBSIZE, 1, 0x2); // 256 entries
        hda.mmio_write(HDA_CORBRP, 2, 31);
        hda.mmio_write(HDA_CORBWP, 2, 32);

        // Start rings so `poll()` enters the CORB processing loop.
        hda.mmio_write(HDA_CORBCTL, 1, CORBCTL_RUN as u64);
        hda.mmio_write(HDA_RIRBCTL, 1, RIRBCTL_RUN as u64);

        // With hardened address math, this should not panic and should not touch guest memory.
        hda.poll(&mut mem);
    }
}
