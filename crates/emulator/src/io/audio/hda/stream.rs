use memory::MemoryBus;

use super::mask_for_size;
use super::regs::*;
use crate::io::audio::dsp::pcm::{PcmSampleFormat, PcmSpec};

#[derive(Debug, Clone)]
pub struct AudioRingBuffer {
    buf: Vec<u8>,
    read: usize,
    write: usize,
    len: usize,
}

impl AudioRingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0; capacity.max(1)],
            read: 0,
            write: 0,
            len: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.read = 0;
        self.write = 0;
        self.len = 0;
    }

    pub fn push(&mut self, data: &[u8]) {
        let cap = self.buf.len();
        if data.is_empty() || cap == 0 {
            return;
        }

        // If the write exceeds capacity, keep only the newest `cap` bytes.
        if data.len() >= cap {
            let start = data.len() - cap;
            self.buf.copy_from_slice(&data[start..]);
            self.read = 0;
            self.write = 0;
            self.len = cap;
            return;
        }

        let free = cap - self.len;
        if data.len() > free {
            // Drop the oldest bytes to make room.
            let drop = data.len() - free;
            self.read = (self.read + drop) % cap;
            self.len -= drop;
        }

        let first = (cap - self.write).min(data.len());
        self.buf[self.write..self.write + first].copy_from_slice(&data[..first]);
        let remaining = data.len() - first;
        if remaining != 0 {
            self.buf[..remaining].copy_from_slice(&data[first..]);
        }
        self.write = (self.write + data.len()) % cap;
        self.len += data.len();
    }

    /// Pop up to `out.len()` bytes into `out`, returning the number of bytes written.
    pub fn pop_into(&mut self, out: &mut [u8]) -> usize {
        let to_read = out.len().min(self.len);
        if to_read == 0 {
            return 0;
        }

        let cap = self.buf.len();
        let first = (cap - self.read).min(to_read);
        out[..first].copy_from_slice(&self.buf[self.read..self.read + first]);
        let remaining = to_read - first;
        if remaining != 0 {
            out[first..to_read].copy_from_slice(&self.buf[..remaining]);
        }

        self.read = (self.read + to_read) % cap;
        self.len -= to_read;
        to_read
    }

    /// Drain all buffered bytes into the provided output buffer.
    ///
    /// This is the preferred API for hot paths because it allows callers to reuse
    /// allocations (unlike [`Self::drain_all`], which returns a newly allocated `Vec`).
    pub fn drain_all_into(&mut self, out: &mut Vec<u8>) {
        out.clear();
        if self.len == 0 {
            return;
        }
        out.reserve(self.len);

        let cap = self.buf.len();
        let first = (cap - self.read).min(self.len);
        out.extend_from_slice(&self.buf[self.read..self.read + first]);
        let remaining = self.len - first;
        if remaining != 0 {
            out.extend_from_slice(&self.buf[..remaining]);
        }

        self.read = self.write;
        self.len = 0;
    }

    pub fn drain_all(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        self.drain_all_into(&mut out);
        out
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub struct StreamFormat {
    pub sample_rate: u32,
    pub bits_per_sample: u8,
    pub channels: u8,
}

impl StreamFormat {
    pub fn from_hda_fmt(fmt: u16) -> Self {
        // This matches the common HDA encoding used by Linux and QEMU. It is
        // sufficient for 44.1/48kHz and 16-bit, which is what Windows
        // typically configures initially.
        let base = if fmt & (1 << 14) != 0 { 44100 } else { 48000 };

        let mult = match (fmt >> 11) & 0x7 {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => 4,
            4 => 5,
            5 => 6,
            6 => 7,
            7 => 8,
            _ => 1,
        };
        let div = match (fmt >> 8) & 0x7 {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => 4,
            4 => 5,
            5 => 6,
            6 => 7,
            7 => 8,
            _ => 1,
        };

        let bits = match (fmt >> 4) & 0x7 {
            0 => 8,
            1 => 16,
            2 => 20,
            3 => 24,
            4 => 32,
            _ => 16,
        };
        let channels = ((fmt & 0xF) + 1) as u8;

        Self {
            sample_rate: (base * mult) / div,
            bits_per_sample: bits,
            channels,
        }
    }

    /// Convert this HDA stream format into a DSP [`PcmSpec`].
    ///
    /// HDA encodes integer PCM formats only; floating-point formats are not
    /// expressible in the stream `FMT` register.
    pub fn to_pcm_spec(&self) -> Option<PcmSpec> {
        let format = match self.bits_per_sample {
            8 => PcmSampleFormat::U8,
            16 => PcmSampleFormat::I16,
            20 => PcmSampleFormat::I20In32,
            24 => PcmSampleFormat::I24In32,
            32 => PcmSampleFormat::I32,
            _ => return None,
        };

        let channels = self.channels as usize;
        if channels == 0 {
            return None;
        }

        Some(PcmSpec {
            format,
            channels,
            sample_rate: self.sample_rate,
        })
    }
}

#[derive(Debug, Copy, Clone)]
struct BdlEntry {
    addr: u64,
    len: u32,
    ioc: bool,
}

#[derive(Debug)]
pub struct HdaStream {
    id: StreamId,

    ctl: u32, // low 24 bits
    sts: u8,
    lpib: u32,
    cbl: u32,
    lvi: u16,
    fifow: u16,
    fifos: u16,
    fmt: u16,
    bdpl: u32,
    bdpu: u32,

    bdl_index: u16,
    bdl_offset: u32,

    dma_scratch: Vec<u8>,
}

impl HdaStream {
    pub fn new(id: StreamId) -> Self {
        Self {
            id,
            ctl: 0,
            sts: 0,
            lpib: 0,
            cbl: 0,
            lvi: 0,
            fifow: 0,
            fifos: 0x20,
            fmt: 0,
            bdpl: 0,
            bdpu: 0,
            bdl_index: 0,
            bdl_offset: 0,
            dma_scratch: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.ctl = 0;
        self.sts = 0;
        self.lpib = 0;
        self.cbl = 0;
        self.lvi = 0;
        self.fifow = 0;
        self.fifos = 0x20;
        self.fmt = 0;
        self.bdpl = 0;
        self.bdpu = 0;
        self.bdl_index = 0;
        self.bdl_offset = 0;
        self.dma_scratch.clear();
    }

    pub fn lpib(&self) -> u32 {
        self.lpib
    }

    fn bdl_base(&self) -> u64 {
        ((self.bdpu as u64) << 32) | (self.bdpl as u64 & !0x7F)
    }

    fn is_running(&self) -> bool {
        (self.ctl & SD_CTL_RUN != 0) && (self.ctl & SD_CTL_SRST != 0)
    }

    pub fn mmio_read(&self, reg: StreamReg, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        match reg {
            StreamReg::CtlSts => {
                ((self.sts as u32 as u64) << 24 | self.ctl as u64) & mask_for_size(size)
            }
            StreamReg::Lpib => self.lpib as u64 & mask_for_size(size),
            StreamReg::Cbl => self.cbl as u64 & mask_for_size(size),
            StreamReg::Lvi => self.lvi as u64 & mask_for_size(size),
            StreamReg::Fifow => self.fifow as u64 & mask_for_size(size),
            StreamReg::Fifos => self.fifos as u64 & mask_for_size(size),
            StreamReg::Fmt => self.fmt as u64 & mask_for_size(size),
            StreamReg::Bdpl => self.bdpl as u64 & mask_for_size(size),
            StreamReg::Bdpu => self.bdpu as u64 & mask_for_size(size),
        }
    }

    pub fn mmio_write(&mut self, reg: StreamReg, size: usize, value: u64, intsts: &mut u32) {
        if size == 0 {
            return;
        }
        let value = value & mask_for_size(size);
        match reg {
            StreamReg::CtlSts => {
                let w = value as u32;
                let new_ctl = w & 0x00FF_FFFF;
                let sts_clear = (w >> 24) as u8;
                if sts_clear != 0 {
                    self.sts &= !sts_clear;
                    if self.sts & SD_STS_BCIS == 0 {
                        *intsts &= !self.intsts_bit();
                    }
                }

                let old_ctl = self.ctl;
                self.ctl = new_ctl;

                // Stream reset deasserted -> reset internal state.
                if (old_ctl & SD_CTL_SRST != 0) && (new_ctl & SD_CTL_SRST == 0) {
                    self.lpib = 0;
                    self.sts = 0;
                    self.bdl_index = 0;
                    self.bdl_offset = 0;
                    *intsts &= !self.intsts_bit();
                }
            }
            StreamReg::Lpib => {
                // Read-only in hardware.
                let _ = value;
            }
            StreamReg::Cbl => self.cbl = value as u32,
            StreamReg::Lvi => self.lvi = value as u16,
            StreamReg::Fifow => self.fifow = value as u16,
            StreamReg::Fifos => {
                let _ = value;
            }
            StreamReg::Fmt => self.fmt = value as u16,
            StreamReg::Bdpl => self.bdpl = value as u32,
            StreamReg::Bdpu => self.bdpu = value as u32,
        }
    }

    pub fn process(
        &mut self,
        mem: &mut dyn MemoryBus,
        audio: &mut AudioRingBuffer,
        intsts: &mut u32,
    ) {
        if !self.is_running() {
            return;
        }
        if self.cbl == 0 {
            return;
        }

        // Consume at most one BDL entry per call. Real hardware is paced by the
        // link; callers are expected to invoke `poll()` periodically.
        let max_entries = self.lvi as usize + 1;
        for _ in 0..max_entries.max(1) {
            let entry = self.read_bdl_entry(mem, self.bdl_index);
            let remaining = entry.len.saturating_sub(self.bdl_offset);
            if remaining == 0 {
                // Skip empty entries.
                self.finish_bdl_entry(entry, intsts);
                continue;
            }

            let format = StreamFormat::from_hda_fmt(self.fmt);
            let bytes_per_sample = (u32::from(format.bits_per_sample).div_ceil(8)).max(1);
            let bytes_per_frame = bytes_per_sample.saturating_mul(format.channels as u32);
            let bytes_per_tick = bytes_per_frame.saturating_mul(128).max(1);
            let take = remaining.min(bytes_per_tick);

            let bytes = take as usize;
            self.dma_scratch.resize(bytes, 0);
            mem.read_physical(
                entry.addr + self.bdl_offset as u64,
                &mut self.dma_scratch[..bytes],
            );
            audio.push(&self.dma_scratch[..bytes]);

            self.bdl_offset += take;
            self.lpib = self.lpib.wrapping_add(take) % self.cbl;
            self.finish_bdl_entry(entry, intsts);
            break;
        }
    }

    pub fn process_capture(
        &mut self,
        mem: &mut dyn MemoryBus,
        capture: &mut AudioRingBuffer,
        intsts: &mut u32,
    ) {
        if !self.is_running() {
            return;
        }
        if self.cbl == 0 {
            return;
        }

        let max_entries = self.lvi as usize + 1;
        for _ in 0..max_entries.max(1) {
            let entry = self.read_bdl_entry(mem, self.bdl_index);
            let remaining = entry.len.saturating_sub(self.bdl_offset);
            if remaining == 0 {
                self.finish_bdl_entry(entry, intsts);
                continue;
            }

            let format = StreamFormat::from_hda_fmt(self.fmt);
            let bytes_per_sample = (u32::from(format.bits_per_sample).div_ceil(8)).max(1);
            let bytes_per_frame = bytes_per_sample.saturating_mul(format.channels as u32);
            let bytes_per_tick = bytes_per_frame.saturating_mul(128).max(1);
            let take = remaining.min(bytes_per_tick);

            let bytes = take as usize;
            self.dma_scratch.resize(bytes, 0);

            // Fill from capture buffer; any missing bytes remain as zero (silence).
            let read = capture.pop_into(&mut self.dma_scratch[..bytes]);
            if read < bytes {
                self.dma_scratch[read..bytes].fill(0);
            }
            mem.write_physical(
                entry.addr + self.bdl_offset as u64,
                &self.dma_scratch[..bytes],
            );

            self.bdl_offset += take;
            self.lpib = self.lpib.wrapping_add(take) % self.cbl;
            self.finish_bdl_entry(entry, intsts);
            break;
        }
    }

    fn intsts_bit(&self) -> u32 {
        match self.id {
            StreamId::Out0 => INTSTS_SIS0,
            StreamId::In0 => INTSTS_SIS1,
        }
    }

    fn finish_bdl_entry(&mut self, entry: BdlEntry, intsts: &mut u32) {
        if self.bdl_offset < entry.len {
            return;
        }
        self.bdl_offset = 0;
        if entry.ioc {
            self.sts |= SD_STS_BCIS;
            if self.ctl & SD_CTL_IOCE != 0 {
                *intsts |= self.intsts_bit();
            }
        }

        if self.bdl_index >= self.lvi {
            self.bdl_index = 0;
        } else {
            self.bdl_index += 1;
        }
    }

    fn read_bdl_entry(&self, mem: &mut dyn MemoryBus, index: u16) -> BdlEntry {
        let addr = self.bdl_base() + (index as u64) * 16;
        let buf_addr = read_u64(mem, addr);
        let len = mem.read_u32(addr + 8);
        let flags = mem.read_u32(addr + 12);
        BdlEntry {
            addr: buf_addr,
            len,
            ioc: (flags & 1) != 0,
        }
    }
}

fn read_u64(mem: &mut dyn MemoryBus, addr: u64) -> u64 {
    let mut buf = [0u8; 8];
    mem.read_physical(addr, &mut buf);
    u64::from_le_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::AudioRingBuffer;
    use super::StreamFormat;
    use crate::io::audio::dsp::pcm::PcmSampleFormat;

    #[test]
    fn ring_buffer_push_and_drain_roundtrip() {
        let mut rb = AudioRingBuffer::new(4);
        rb.push(&[1, 2, 3]);
        assert_eq!(rb.len(), 3);

        let drained = rb.drain_all();
        assert_eq!(drained, vec![1, 2, 3]);
        assert_eq!(rb.len(), 0);
    }

    #[test]
    fn ring_buffer_wrap_and_overflow_keeps_newest_bytes() {
        let mut rb = AudioRingBuffer::new(4);
        rb.push(&[1, 2, 3]);
        rb.push(&[4, 5]);

        assert_eq!(rb.drain_all(), vec![2, 3, 4, 5]);
    }

    #[test]
    fn ring_buffer_large_push_truncates_to_capacity() {
        let mut rb = AudioRingBuffer::new(4);
        rb.push(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(rb.drain_all(), vec![3, 4, 5, 6]);
    }

    #[test]
    fn ring_buffer_pop_into_reads_and_removes_bytes() {
        let mut rb = AudioRingBuffer::new(8);
        rb.push(&[1, 2, 3, 4]);
        let mut out = [0u8; 3];
        assert_eq!(rb.pop_into(&mut out), 3);
        assert_eq!(out, [1, 2, 3]);
        assert_eq!(rb.drain_all(), vec![4]);
    }

    #[test]
    fn stream_format_to_pcm_spec_maps_bits_and_channels() {
        let fmt = StreamFormat {
            sample_rate: 48_000,
            bits_per_sample: 16,
            channels: 2,
        };
        let spec = fmt.to_pcm_spec().unwrap();
        assert_eq!(spec.format, PcmSampleFormat::I16);
        assert_eq!(spec.channels, 2);
        assert_eq!(spec.sample_rate, 48_000);
    }
}
