use memory::MemoryBus;

use crate::io::audio::ac97::regs::{
    BDL_ENTRY_BYTES, BDL_IOC, CR_IOCE, CR_LVBIE, CR_RPBM, CR_RR, SR_BCIS, SR_CELV, SR_DCH, SR_LVBCI,
};

/// Audio sink for interleaved floating point samples.
///
/// The AC'97 DMA engine transports raw 16-bit samples. The controller converts
/// to `f32` in the [-1.0, 1.0] range and forwards them to this sink.
pub trait AudioSink {
    fn push_interleaved_f32(&mut self, samples: &[f32]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BufferDescriptor {
    addr: u32,
    len_words: u16,
    ioc: bool,
}

impl BufferDescriptor {
    fn read_from(mem: &mut dyn MemoryBus, bdbar: u32, index: u8) -> Self {
        let base = bdbar as u64 + (index as u64) * BDL_ENTRY_BYTES;
        let addr = mem.read_u32(base);
        let ctl = mem.read_u32(base + 4);
        let len_words = (ctl & 0xFFFF) as u16;
        let ioc = (ctl & BDL_IOC) != 0;
        Self {
            addr,
            len_words,
            ioc,
        }
    }
}

/// Minimal PCM-out DMA engine for Intel ICH AC'97 bus mastering.
///
/// Only supports playback (PCM out) and enough register behaviour to let
/// legacy drivers feed audio data through a descriptor ring.
#[derive(Debug, Clone)]
pub struct PcmOutDma {
    pub bdbar: u32,
    civ: u8,
    lvi: u8,
    sr: u16,
    cr: u8,
    picb: u16,
    piv: u8,

    // Current buffer progress in 16-bit words.
    cur_buf_pos_words: u16,
    cur_desc: Option<BufferDescriptor>,
}

impl Default for PcmOutDma {
    fn default() -> Self {
        Self {
            bdbar: 0,
            civ: 0,
            lvi: 0,
            sr: SR_DCH,
            cr: 0,
            picb: 0,
            piv: 0,
            cur_buf_pos_words: 0,
            cur_desc: None,
        }
    }
}

impl PcmOutDma {
    pub fn civ(&self) -> u8 {
        self.civ
    }

    pub fn lvi(&self) -> u8 {
        self.lvi
    }

    pub fn sr(&self) -> u16 {
        self.sr
    }

    pub fn cr(&self) -> u8 {
        self.cr
    }

    pub fn picb(&self) -> u16 {
        self.picb
    }

    pub fn piv(&self) -> u8 {
        self.piv
    }

    pub fn irq_pending(&self) -> bool {
        (self.sr & SR_BCIS != 0 && self.cr & CR_IOCE != 0)
            || (self.sr & SR_LVBCI != 0 && self.cr & CR_LVBIE != 0)
    }

    pub fn write_bdbar(&mut self, value: u32) {
        // BDL base must be 8-byte aligned; mask low bits.
        self.bdbar = value & !0x7;
    }

    pub fn write_lvi(&mut self, value: u8) {
        self.lvi = value & 0x1f;
        self.update_celv();
    }

    pub fn write_sr(&mut self, value: u16) {
        // Write-1-to-clear for interrupt status bits.
        let w1c = value & (SR_BCIS | SR_LVBCI);
        self.sr &= !w1c;
        self.update_celv();
    }

    pub fn write_cr(&mut self, value: u8) {
        let prev = self.cr;
        self.cr = value;

        if (value & CR_RR) != 0 {
            self.reset_regs();
            return;
        }

        let was_running = (prev & CR_RPBM) != 0;
        let now_running = (value & CR_RPBM) != 0;
        if !was_running && now_running {
            // Starting: clear halted bit; descriptors will be loaded on the next tick.
            self.sr &= !SR_DCH;
        } else if was_running && !now_running {
            self.sr |= SR_DCH;
        }
    }

    pub fn is_running(&self) -> bool {
        (self.cr & CR_RPBM) != 0 && (self.sr & SR_DCH) == 0
    }

    fn reset_regs(&mut self) {
        *self = Self::default();
    }

    fn update_celv(&mut self) {
        if self.civ == self.lvi {
            self.sr |= SR_CELV;
        } else {
            self.sr &= !SR_CELV;
        }
    }

    fn load_current_desc(&mut self, mem: &mut dyn MemoryBus) {
        let desc = BufferDescriptor::read_from(mem, self.bdbar, self.civ);
        self.picb = desc.len_words;
        self.cur_buf_pos_words = 0;
        self.cur_desc = Some(desc);
        self.piv = self.civ.wrapping_add(1) & 0x1f;
        self.update_celv();
    }

    /// Advance the DMA engine, transferring up to `max_words` of 16-bit samples.
    ///
    /// This method is intentionally time-agnostic. Callers should decide how
    /// often to tick based on host audio pacing.
    pub fn tick(&mut self, mem: &mut dyn MemoryBus, out: &mut impl AudioSink, max_words: u16) {
        if (self.cr & CR_RPBM) == 0 {
            return;
        }
        if (self.sr & SR_DCH) != 0 {
            // If the guest has provided new buffers (LVI moved forward), resume.
            let empty = self.civ == (self.lvi.wrapping_add(1) & 0x1f);
            if empty {
                return;
            }
            self.sr &= !SR_DCH;
        }

        if self.cur_desc.is_none() {
            self.load_current_desc(mem);
            if self.picb == 0 {
                // Zero-length buffers are treated as immediate completion.
                self.complete_current_buffer(mem);
                return;
            }
        }

        let Some(desc) = self.cur_desc else { return };

        let words_left = desc.len_words.saturating_sub(self.cur_buf_pos_words);
        let words_to_copy = max_words.min(words_left);
        if words_to_copy == 0 {
            return;
        }

        let byte_off = (self.cur_buf_pos_words as u64) * 2;
        let bytes_to_copy = (words_to_copy as usize) * 2;
        let mut raw = vec![0u8; bytes_to_copy];
        mem.read_physical(desc.addr as u64 + byte_off, &mut raw);

        let mut samples = Vec::with_capacity(words_to_copy as usize);
        for chunk in raw.chunks_exact(2) {
            let val = i16::from_le_bytes([chunk[0], chunk[1]]);
            samples.push(val as f32 / 32768.0);
        }
        out.push_interleaved_f32(&samples);

        self.cur_buf_pos_words = self.cur_buf_pos_words.saturating_add(words_to_copy);
        self.picb = self.picb.saturating_sub(words_to_copy);

        if self.picb == 0 {
            self.complete_current_buffer(mem);
        }
    }

    fn complete_current_buffer(&mut self, mem: &mut dyn MemoryBus) {
        let Some(desc) = self.cur_desc else { return };

        if desc.ioc {
            self.sr |= SR_BCIS;
        }

        let was_last_valid = self.civ == self.lvi;

        if was_last_valid {
            self.sr |= SR_LVBCI | SR_DCH;
            // Advance to the next index so the engine can resume when the guest
            // extends the ring by writing a new LVI.
            self.civ = self.civ.wrapping_add(1) & 0x1f;
            self.cur_desc = None;
            self.picb = 0;
            self.cur_buf_pos_words = 0;
            self.piv = self.civ.wrapping_add(1) & 0x1f;
            self.update_celv();
            return;
        }

        self.civ = self.civ.wrapping_add(1) & 0x1f;
        self.load_current_desc(mem);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct TestAudio {
        samples: Vec<f32>,
    }

    impl AudioSink for TestAudio {
        fn push_interleaved_f32(&mut self, samples: &[f32]) {
            self.samples.extend_from_slice(samples);
        }
    }

    #[derive(Clone, Debug)]
    struct TestMemory {
        data: Vec<u8>,
    }

    impl TestMemory {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
            }
        }

        fn write_u32(&mut self, addr: u64, value: u32) {
            let start = addr as usize;
            self.data[start..start + 4].copy_from_slice(&value.to_le_bytes());
        }

        fn write_bytes(&mut self, addr: u64, data: &[u8]) {
            let start = addr as usize;
            self.data[start..start + data.len()].copy_from_slice(data);
        }
    }

    impl MemoryBus for TestMemory {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let start = paddr as usize;
            buf.copy_from_slice(&self.data[start..start + buf.len()]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let start = paddr as usize;
            self.data[start..start + buf.len()].copy_from_slice(buf);
        }
    }

    #[test]
    fn dma_completes_ioc_buffer() {
        let mut mem = TestMemory::new(0x4000);

        let bdl_addr = 0x1000u64;
        let buf_addr = 0x2000u64;

        // One descriptor, 4 words (2 stereo frames), IOC set.
        mem.write_u32(bdl_addr, buf_addr as u32);
        mem.write_u32(bdl_addr + 4, (4u32) | BDL_IOC);

        // Audio samples: 0, 32767, -32768, 16384
        let raw: [u8; 8] = [
            0x00, 0x00, // 0
            0xFF, 0x7F, // 32767
            0x00, 0x80, // -32768
            0x00, 0x40, // 16384
        ];
        mem.write_bytes(buf_addr, &raw);

        let mut dma = PcmOutDma::default();
        dma.write_bdbar(bdl_addr as u32);
        dma.write_lvi(0);
        dma.write_cr(CR_RPBM | CR_IOCE);

        let mut audio = TestAudio::default();
        dma.tick(&mut mem, &mut audio, 4);

        assert_eq!(dma.sr() & SR_BCIS, SR_BCIS);
        assert_eq!(audio.samples.len(), 4);
        assert!((audio.samples[0] - 0.0).abs() < f32::EPSILON);
        assert!((audio.samples[1] - (32767.0 / 32768.0)).abs() < 1e-6);
        assert!((audio.samples[2] - (-1.0)).abs() < 1e-6);
        assert!((audio.samples[3] - (16384.0 / 32768.0)).abs() < 1e-6);
    }
}
