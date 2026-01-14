use memory::MemoryBus;

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

    /// Bytes per PCM sample as transported over the HDA link.
    ///
    /// Note that 20/24-bit HDA formats are carried in 32-bit containers.
    pub fn bytes_per_sample(&self) -> u32 {
        match self.bits_per_sample {
            8 => 1,
            16 => 2,
            20 | 24 | 32 => 4,
            _ => 0,
        }
    }

    pub fn bytes_per_frame(&self) -> u32 {
        self.bytes_per_sample()
            .saturating_mul(u32::from(self.channels))
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
    lvi: u8,
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

    #[cfg(test)]
    pub fn mmio_read(&self, reg: StreamReg, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let mut out = 0u64;
        let capped = size.min(8);
        for i in 0..capped {
            out |= (u64::from(self.mmio_read_byte(reg, i as u8))) << (i * 8);
        }
        out
    }

    #[cfg(test)]
    pub fn mmio_write(&mut self, reg: StreamReg, size: usize, value: u64, intsts: &mut u32) {
        if size == 0 {
            return;
        }
        let capped = size.min(8);
        for i in 0..capped {
            let b = ((value >> (i * 8)) & 0xff) as u8;
            self.mmio_write_byte(reg, i as u8, b, intsts);
        }
    }

    pub fn mmio_read_byte(&self, reg: StreamReg, byte: u8) -> u8 {
        match reg {
            StreamReg::CtlSts => match byte {
                0..=2 => ((self.ctl >> (u32::from(byte) * 8)) & 0xff) as u8,
                3 => self.sts,
                _ => 0,
            },
            StreamReg::Lpib => {
                if byte < 4 {
                    ((self.lpib >> (u32::from(byte) * 8)) & 0xff) as u8
                } else {
                    0
                }
            }
            StreamReg::Cbl => {
                if byte < 4 {
                    ((self.cbl >> (u32::from(byte) * 8)) & 0xff) as u8
                } else {
                    0
                }
            }
            StreamReg::Lvi => match byte {
                0 => self.lvi,
                // SDnLVI is 8-bit; upper bits are reserved and must read as 0.
                _ => 0,
            },
            StreamReg::Fifow => {
                if byte < 2 {
                    ((self.fifow >> (u32::from(byte) * 8)) & 0xff) as u8
                } else {
                    0
                }
            }
            StreamReg::Fifos => {
                if byte < 2 {
                    ((self.fifos >> (u32::from(byte) * 8)) & 0xff) as u8
                } else {
                    0
                }
            }
            StreamReg::Fmt => {
                if byte < 2 {
                    ((self.fmt >> (u32::from(byte) * 8)) & 0xff) as u8
                } else {
                    0
                }
            }
            StreamReg::Bdpl => {
                if byte < 4 {
                    ((self.bdpl >> (u32::from(byte) * 8)) & 0xff) as u8
                } else {
                    0
                }
            }
            StreamReg::Bdpu => {
                if byte < 4 {
                    ((self.bdpu >> (u32::from(byte) * 8)) & 0xff) as u8
                } else {
                    0
                }
            }
        }
    }

    pub fn mmio_write_byte(&mut self, reg: StreamReg, byte: u8, value: u8, intsts: &mut u32) {
        match reg {
            StreamReg::CtlSts => match byte {
                0..=2 => {
                    let shift = u32::from(byte) * 8;
                    let mask = 0xffu32 << shift;
                    let old_ctl = self.ctl;
                    self.ctl = (self.ctl & !mask) | (u32::from(value) << shift);

                    // Stream reset deasserted -> reset internal state.
                    if (old_ctl & SD_CTL_SRST != 0) && (self.ctl & SD_CTL_SRST == 0) {
                        self.lpib = 0;
                        self.sts = 0;
                        self.bdl_index = 0;
                        self.bdl_offset = 0;
                        *intsts &= !self.intsts_bit();
                    }
                }
                3 => {
                    // Status byte: write-1-to-clear.
                    if value != 0 {
                        self.sts &= !value;
                        if self.sts & SD_STS_BCIS == 0 {
                            *intsts &= !self.intsts_bit();
                        }
                    }
                }
                _ => {}
            },
            StreamReg::Lpib => {
                // Read-only in hardware.
                let _ = (byte, value);
            }
            StreamReg::Cbl => {
                if byte < 4 {
                    let shift = u32::from(byte) * 8;
                    let mask = 0xffu32 << shift;
                    self.cbl = (self.cbl & !mask) | (u32::from(value) << shift);
                }
            }
            StreamReg::Lvi => {
                // SDnLVI is architecturally an 8-bit "last valid index" into the 256-entry BDL.
                // Mask the guest-provided value (and ignore higher bytes) so pathological writes
                // (e.g. 0xffff) cannot drive out-of-range descriptor indexing.
                if byte == 0 {
                    self.lvi = value;
                    // Keep the stream's BDL cursor consistent with the guest-programmed LVI. This
                    // matches the canonical model behaviour and avoids out-of-range BDL entry
                    // reads if the guest shrinks LVI while a stream is running.
                    if self.bdl_index > u16::from(self.lvi) {
                        self.bdl_index = 0;
                        self.bdl_offset = 0;
                    }
                }
            }
            StreamReg::Fifow => {
                if byte < 2 {
                    let shift = u32::from(byte) * 8;
                    let mask = 0xffu16 << shift;
                    self.fifow = (self.fifow & !mask) | (u16::from(value) << shift);
                }
            }
            StreamReg::Fifos => {
                // Read-only in hardware.
                let _ = (byte, value);
            }
            StreamReg::Fmt => {
                if byte < 2 {
                    let shift = u32::from(byte) * 8;
                    let mask = 0xffu16 << shift;
                    self.fmt = (self.fmt & !mask) | (u16::from(value) << shift);
                }
            }
            StreamReg::Bdpl => {
                if byte < 4 {
                    let shift = u32::from(byte) * 8;
                    let mask = 0xffu32 << shift;
                    self.bdpl = (self.bdpl & !mask) | (u32::from(value) << shift);
                }
            }
            StreamReg::Bdpu => {
                if byte < 4 {
                    let shift = u32::from(byte) * 8;
                    let mask = 0xffu32 << shift;
                    self.bdpu = (self.bdpu & !mask) | (u32::from(value) << shift);
                }
            }
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
        let max_entries = usize::from(self.lvi) + 1;
        for _ in 0..max_entries.max(1) {
            let entry = self.read_bdl_entry(mem, self.bdl_index);
            let remaining = entry.len.saturating_sub(self.bdl_offset);
            if remaining == 0 {
                // Skip empty entries.
                self.finish_bdl_entry(entry, intsts);
                continue;
            }

            let format = StreamFormat::from_hda_fmt(self.fmt);
            let bytes_per_frame = format.bytes_per_frame();
            let bytes_per_tick = bytes_per_frame.saturating_mul(128).max(1);
            let take = remaining.min(bytes_per_tick);

            let bytes = take as usize;
            self.dma_scratch.resize(bytes, 0);
            let Some(dma_addr) = entry.addr.checked_add(self.bdl_offset as u64) else {
                // Guest programmed an address that overflows u64 arithmetic.
                // Treat this entry as zero-length and do not touch guest memory.
                self.finish_bdl_entry(
                    BdlEntry {
                        addr: 0,
                        len: 0,
                        ioc: false,
                    },
                    intsts,
                );
                continue;
            };
            // `read_physical` reads bytes in the range `[dma_addr, dma_addr + bytes)`. Even if
            // `dma_addr` computed successfully, guard against `dma_addr + bytes` overflowing.
            if dma_addr.checked_add(bytes as u64).is_none() {
                self.finish_bdl_entry(
                    BdlEntry {
                        addr: 0,
                        len: 0,
                        ioc: false,
                    },
                    intsts,
                );
                continue;
            }
            mem.read_physical(dma_addr, &mut self.dma_scratch[..bytes]);
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

        let max_entries = usize::from(self.lvi) + 1;
        for _ in 0..max_entries.max(1) {
            let entry = self.read_bdl_entry(mem, self.bdl_index);
            let remaining = entry.len.saturating_sub(self.bdl_offset);
            if remaining == 0 {
                self.finish_bdl_entry(entry, intsts);
                continue;
            }

            let format = StreamFormat::from_hda_fmt(self.fmt);
            let bytes_per_frame = format.bytes_per_frame();
            let bytes_per_tick = bytes_per_frame.saturating_mul(128).max(1);
            let take = remaining.min(bytes_per_tick);

            let bytes = take as usize;
            self.dma_scratch.resize(bytes, 0);

            let Some(dma_addr) = entry.addr.checked_add(self.bdl_offset as u64) else {
                // Guest programmed an address that overflows u64 arithmetic.
                // Treat this entry as zero-length and do not touch guest memory.
                self.finish_bdl_entry(
                    BdlEntry {
                        addr: 0,
                        len: 0,
                        ioc: false,
                    },
                    intsts,
                );
                continue;
            };
            // `write_physical` writes bytes in the range `[dma_addr, dma_addr + bytes)`. Even if
            // `dma_addr` computed successfully, guard against `dma_addr + bytes` overflowing.
            if dma_addr.checked_add(bytes as u64).is_none() {
                self.finish_bdl_entry(
                    BdlEntry {
                        addr: 0,
                        len: 0,
                        ioc: false,
                    },
                    intsts,
                );
                continue;
            }

            // Fill from capture buffer; any missing bytes remain as zero (silence).
            let read = capture.pop_into(&mut self.dma_scratch[..bytes]);
            if read < bytes {
                self.dma_scratch[read..bytes].fill(0);
            }
            mem.write_physical(dma_addr, &self.dma_scratch[..bytes]);

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

        if self.bdl_index >= u16::from(self.lvi) {
            self.bdl_index = 0;
        } else {
            self.bdl_index += 1;
        }
    }

    fn read_bdl_entry(&self, mem: &mut dyn MemoryBus, index: u16) -> BdlEntry {
        let Some(offset) = (index as u64).checked_mul(16) else {
            return BdlEntry {
                addr: 0,
                len: 0,
                ioc: false,
            };
        };
        let Some(addr) = self.bdl_base().checked_add(offset) else {
            return BdlEntry {
                addr: 0,
                len: 0,
                ioc: false,
            };
        };

        // Guard against overflow for the remaining fields within the 16-byte BDL entry.
        let Some(len_addr) = addr.checked_add(8) else {
            return BdlEntry {
                addr: 0,
                len: 0,
                ioc: false,
            };
        };
        let Some(flags_addr) = addr.checked_add(12) else {
            return BdlEntry {
                addr: 0,
                len: 0,
                ioc: false,
            };
        };
        // Reading the `flags` u32 touches bytes `[flags_addr, flags_addr + 4)`. Guard against
        // `flags_addr + 4` overflowing even if the descriptor base address computed successfully.
        if flags_addr.checked_add(4).is_none() {
            return BdlEntry {
                addr: 0,
                len: 0,
                ioc: false,
            };
        };

        let buf_addr = read_u64(mem, addr);
        let len = mem.read_u32(len_addr);
        let flags = mem.read_u32(flags_addr);
        BdlEntry {
            addr: buf_addr,
            len,
            ioc: (flags & 1) != 0,
        }
    }
}

#[cfg(test)]
impl HdaStream {
    pub(crate) fn test_set_sts(&mut self, value: u8) {
        self.sts = value;
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
    use super::HdaStream;
    use super::StreamFormat;
    use super::{StreamId, SD_CTL_RUN, SD_CTL_SRST};
    use crate::io::audio::dsp::pcm::PcmSampleFormat;
    use memory::MemoryBus;
    use std::collections::BTreeMap;
    use std::panic::AssertUnwindSafe;

    #[derive(Clone, Debug)]
    struct CountingMem {
        data: Vec<u8>,
        reads: usize,
        max_reads: usize,
        max_read_addr: u64,
    }

    impl CountingMem {
        fn new(size: usize, max_reads: usize) -> Self {
            Self {
                data: vec![0; size],
                reads: 0,
                max_reads,
                max_read_addr: 0,
            }
        }
    }

    impl MemoryBus for CountingMem {
        fn read_physical(&mut self, addr: u64, dst: &mut [u8]) {
            self.reads += 1;
            assert!(
                self.reads <= self.max_reads,
                "excessive guest memory reads: {} > {}",
                self.reads,
                self.max_reads
            );
            if !dst.is_empty() {
                self.max_read_addr = self
                    .max_read_addr
                    .max(addr.saturating_add(dst.len() as u64 - 1));
            }

            let base = addr as usize;
            for (i, slot) in dst.iter_mut().enumerate() {
                *slot = self.data.get(base + i).copied().unwrap_or(0);
            }
        }

        fn write_physical(&mut self, addr: u64, src: &[u8]) {
            let base = addr as usize;
            for (i, byte) in src.iter().copied().enumerate() {
                if let Some(slot) = self.data.get_mut(base + i) {
                    *slot = byte;
                }
            }
        }
    }

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

    #[test]
    fn stream_process_tick_size_uses_32bit_containers_for_20bit_samples() {
        const BDL_BASE: u64 = 0x1000;
        const BUF0: u64 = 0x2000;
        const ENTRY_LEN: u32 = 4096;
        const EXPECTED_TICK_BYTES: usize = 4 * 2 * 128; // 20-bit stereo in 32-bit containers.

        let mut stream = HdaStream::new(StreamId::Out0);
        stream.ctl = SD_CTL_SRST | SD_CTL_RUN;
        stream.cbl = ENTRY_LEN;
        stream.lvi = 0;
        stream.fmt = 0x0021; // 48kHz, 20-bit, stereo.
        stream.bdpl = BDL_BASE as u32;
        stream.bdpu = 0;

        let mut mem = CountingMem::new(0x6000, 32);
        // BDL entry 0 points at BUF0.
        mem.data[BDL_BASE as usize..BDL_BASE as usize + 8].copy_from_slice(&BUF0.to_le_bytes());
        mem.data[BDL_BASE as usize + 8..BDL_BASE as usize + 12]
            .copy_from_slice(&ENTRY_LEN.to_le_bytes());
        mem.data[BDL_BASE as usize + 12..BDL_BASE as usize + 16]
            .copy_from_slice(&0u32.to_le_bytes());

        for i in 0..ENTRY_LEN as usize {
            mem.data[BUF0 as usize + i] = (i & 0xff) as u8;
        }

        let mut audio = AudioRingBuffer::new(ENTRY_LEN as usize);
        let mut intsts = 0u32;

        stream.process(&mut mem, &mut audio, &mut intsts);

        let drained = audio.drain_all();
        assert_eq!(drained.len(), EXPECTED_TICK_BYTES);
        assert_eq!(
            drained,
            mem.data[BUF0 as usize..BUF0 as usize + EXPECTED_TICK_BYTES]
        );
    }

    #[test]
    fn stream_process_capture_tick_size_uses_32bit_containers_for_20bit_samples() {
        const BDL_BASE: u64 = 0x1000;
        const BUF0: u64 = 0x3000;
        const ENTRY_LEN: u32 = 4096;
        const EXPECTED_TICK_BYTES: usize = 4 * 2 * 128;

        let mut stream = HdaStream::new(StreamId::In0);
        stream.ctl = SD_CTL_SRST | SD_CTL_RUN;
        stream.cbl = ENTRY_LEN;
        stream.lvi = 0;
        stream.fmt = 0x0021; // 48kHz, 20-bit, stereo.
        stream.bdpl = BDL_BASE as u32;
        stream.bdpu = 0;

        let mut mem = CountingMem::new(0x8000, 32);
        mem.data[BDL_BASE as usize..BDL_BASE as usize + 8].copy_from_slice(&BUF0.to_le_bytes());
        mem.data[BDL_BASE as usize + 8..BDL_BASE as usize + 12]
            .copy_from_slice(&ENTRY_LEN.to_le_bytes());
        mem.data[BDL_BASE as usize + 12..BDL_BASE as usize + 16]
            .copy_from_slice(&0u32.to_le_bytes());

        // Prefill the buffer with a sentinel so we can check that only the first tick is written.
        for b in &mut mem.data[BUF0 as usize..BUF0 as usize + ENTRY_LEN as usize] {
            *b = 0xaa;
        }

        let input: Vec<u8> = (0..(EXPECTED_TICK_BYTES + 100))
            .map(|i| (i & 0xff) as u8)
            .collect();
        let mut capture = AudioRingBuffer::new(ENTRY_LEN as usize);
        capture.push(&input);
        let before = capture.len();

        let mut intsts = 0u32;
        stream.process_capture(&mut mem, &mut capture, &mut intsts);

        assert_eq!(before - capture.len(), EXPECTED_TICK_BYTES);
        assert_eq!(
            mem.data[BUF0 as usize..BUF0 as usize + EXPECTED_TICK_BYTES],
            input[..EXPECTED_TICK_BYTES]
        );
        assert!(mem.data
            [BUF0 as usize + EXPECTED_TICK_BYTES..BUF0 as usize + EXPECTED_TICK_BYTES + 16]
            .iter()
            .all(|&b| b == 0xaa));
    }

    #[test]
    fn stream_lvi_write_masks_to_8_bit() {
        let mut stream = HdaStream::new(super::StreamId::Out0);
        let mut intsts = 0u32;

        stream.mmio_write(super::StreamReg::Lvi, 2, 0xffff, &mut intsts);
        assert_eq!(stream.mmio_read(super::StreamReg::Lvi, 2), 0x00ff);

        stream.mmio_write(super::StreamReg::Lvi, 2, 0x1234, &mut intsts);
        assert_eq!(stream.mmio_read(super::StreamReg::Lvi, 2), 0x0034);
    }

    #[test]
    fn stream_process_is_bounded_with_large_lvi_and_empty_descriptors() {
        const BDL_BASE: u64 = 0x1000;

        let mut stream = HdaStream::new(super::StreamId::Out0);
        let mut stream_intsts = 0u32;
        stream.mmio_write(super::StreamReg::Bdpl, 4, BDL_BASE, &mut stream_intsts);
        stream.mmio_write(super::StreamReg::Bdpu, 4, 0, &mut stream_intsts);
        stream.mmio_write(super::StreamReg::Cbl, 4, 16, &mut stream_intsts);
        stream.mmio_write(super::StreamReg::Lvi, 2, 0xffff, &mut stream_intsts);
        stream.mmio_write(
            super::StreamReg::CtlSts,
            4,
            (super::SD_CTL_RUN | super::SD_CTL_SRST) as u64,
            &mut stream_intsts,
        );

        let mut mem = CountingMem::new(0x2000, 2048);
        let mut audio = AudioRingBuffer::new(64);
        let mut intsts = 0u32;

        stream.process(&mut mem, &mut audio, &mut intsts);

        assert_eq!(stream.mmio_read(super::StreamReg::Lvi, 2), 0x00ff);
        let max_bdl_index = mem.max_read_addr.saturating_sub(BDL_BASE) / 16;
        assert!(
            max_bdl_index <= 0xff,
            "BDL index exceeded architectural 8-bit range: {max_bdl_index:#x}"
        );
        assert!(audio.is_empty());
    }

    #[test]
    fn stream_process_capture_is_bounded_with_large_lvi_and_empty_descriptors() {
        const BDL_BASE: u64 = 0x1000;

        let mut stream = HdaStream::new(super::StreamId::In0);
        let mut stream_intsts = 0u32;
        stream.mmio_write(super::StreamReg::Bdpl, 4, BDL_BASE, &mut stream_intsts);
        stream.mmio_write(super::StreamReg::Bdpu, 4, 0, &mut stream_intsts);
        stream.mmio_write(super::StreamReg::Cbl, 4, 16, &mut stream_intsts);
        stream.mmio_write(super::StreamReg::Lvi, 2, 0xffff, &mut stream_intsts);
        stream.mmio_write(
            super::StreamReg::CtlSts,
            4,
            (super::SD_CTL_RUN | super::SD_CTL_SRST) as u64,
            &mut stream_intsts,
        );

        let mut mem = CountingMem::new(0x2000, 2048);
        let mut capture = AudioRingBuffer::new(64);
        let mut intsts = 0u32;

        stream.process_capture(&mut mem, &mut capture, &mut intsts);

        assert_eq!(stream.mmio_read(super::StreamReg::Lvi, 2), 0x00ff);
        let max_bdl_index = mem.max_read_addr.saturating_sub(BDL_BASE) / 16;
        assert!(
            max_bdl_index <= 0xff,
            "BDL index exceeded architectural 8-bit range: {max_bdl_index:#x}"
        );
        assert!(capture.is_empty());
    }

    #[derive(Default)]
    struct PanicMem;

    impl MemoryBus for PanicMem {
        fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
            panic!("unexpected guest memory read");
        }

        fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
            panic!("unexpected guest memory write");
        }
    }

    #[test]
    fn stream_process_bdl_address_overflow_does_not_touch_memory() {
        let mut stream = HdaStream::new(StreamId::Out0);
        stream.ctl = SD_CTL_SRST | SD_CTL_RUN;
        stream.cbl = 1;
        stream.lvi = 0;

        // Align BDPL to 128 and choose a base that overflows when adding index*16 (index=8).
        stream.bdpl = 0xFFFF_FF80;
        stream.bdpu = 0xFFFF_FFFF;
        stream.bdl_index = 8;

        let mut mem = PanicMem;
        let mut audio = AudioRingBuffer::new(16);
        let mut intsts = 0u32;

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            stream.process(&mut mem, &mut audio, &mut intsts)
        }));
        assert!(
            result.is_ok(),
            "process() must not panic on overflowing BDL base/index arithmetic"
        );
        assert!(audio.is_empty());
        assert_eq!(intsts, 0);
    }

    #[test]
    fn stream_process_capture_bdl_address_overflow_does_not_touch_memory() {
        let mut stream = HdaStream::new(StreamId::In0);
        stream.ctl = SD_CTL_SRST | SD_CTL_RUN;
        stream.cbl = 1;
        stream.lvi = 0;

        stream.bdpl = 0xFFFF_FF80;
        stream.bdpu = 0xFFFF_FFFF;
        stream.bdl_index = 8;

        let mut mem = PanicMem;
        let mut capture = AudioRingBuffer::new(16);
        capture.push(&[1, 2, 3, 4]);
        let before = capture.len();
        let mut intsts = 0u32;

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            stream.process_capture(&mut mem, &mut capture, &mut intsts)
        }));
        assert!(
            result.is_ok(),
            "process_capture() must not panic on overflowing BDL base/index arithmetic"
        );
        assert_eq!(capture.len(), before);
        assert_eq!(intsts, 0);
    }

    struct StrictReadMem {
        bytes: BTreeMap<u64, u8>,
    }

    impl StrictReadMem {
        fn new() -> Self {
            Self {
                bytes: BTreeMap::new(),
            }
        }

        fn write(&mut self, addr: u64, buf: &[u8]) {
            for (i, b) in buf.iter().copied().enumerate() {
                self.bytes.insert(addr + i as u64, b);
            }
        }
    }

    impl MemoryBus for StrictReadMem {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            for (i, slot) in buf.iter_mut().enumerate() {
                let addr = paddr
                    .checked_add(i as u64)
                    .expect("unexpected overflow while reading descriptor");
                *slot = *self
                    .bytes
                    .get(&addr)
                    .unwrap_or_else(|| panic!("unexpected read at {addr:#x}"));
            }
        }

        fn write_physical(&mut self, paddr: u64, _buf: &[u8]) {
            panic!("unexpected guest memory write at {paddr:#x}");
        }
    }

    #[test]
    fn stream_process_dma_address_overflow_does_not_read_buffer() {
        let mut mem = StrictReadMem::new();

        let bdl_base = 0x1000u64;
        let entry_addr = u64::MAX - 1;
        let entry_len = 8u32;
        let entry_flags = 0u32;
        mem.write(bdl_base, &entry_addr.to_le_bytes());
        mem.write(bdl_base + 8, &entry_len.to_le_bytes());
        mem.write(bdl_base + 12, &entry_flags.to_le_bytes());

        let mut stream = HdaStream::new(StreamId::Out0);
        stream.ctl = SD_CTL_SRST | SD_CTL_RUN;
        stream.cbl = 1;
        stream.lvi = 0;
        stream.bdpl = bdl_base as u32;
        stream.bdpu = 0;
        stream.bdl_index = 0;
        stream.bdl_offset = 4; // entry_addr + 4 overflows for entry_addr=u64::MAX-1

        let mut audio = AudioRingBuffer::new(16);
        let mut intsts = 0u32;

        stream.process(&mut mem, &mut audio, &mut intsts);
        assert!(audio.is_empty());
        assert_eq!(intsts, 0);
    }

    #[test]
    fn stream_process_capture_dma_address_overflow_does_not_write_buffer_or_consume_capture() {
        let mut mem = StrictReadMem::new();

        let bdl_base = 0x2000u64;
        let entry_addr = u64::MAX - 1;
        let entry_len = 8u32;
        let entry_flags = 0u32;
        mem.write(bdl_base, &entry_addr.to_le_bytes());
        mem.write(bdl_base + 8, &entry_len.to_le_bytes());
        mem.write(bdl_base + 12, &entry_flags.to_le_bytes());

        let mut stream = HdaStream::new(StreamId::In0);
        stream.ctl = SD_CTL_SRST | SD_CTL_RUN;
        stream.cbl = 1;
        stream.lvi = 0;
        stream.bdpl = bdl_base as u32;
        stream.bdpu = 0;
        stream.bdl_index = 0;
        stream.bdl_offset = 4;

        let mut capture = AudioRingBuffer::new(16);
        capture.push(&[1, 2, 3, 4]);
        let before = capture.len();

        let mut intsts = 0u32;
        stream.process_capture(&mut mem, &mut capture, &mut intsts);

        assert_eq!(capture.len(), before);
        assert_eq!(intsts, 0);
    }

    #[test]
    fn stream_lvi_shrink_resets_bdl_cursor() {
        const BDL_BASE: u64 = 0x1000;
        const BUF0: u64 = 0x2000;

        let mut stream = HdaStream::new(super::StreamId::Out0);
        let mut stream_intsts = 0u32;
        stream.mmio_write(super::StreamReg::Bdpl, 4, BDL_BASE, &mut stream_intsts);
        stream.mmio_write(super::StreamReg::Bdpu, 4, 0, &mut stream_intsts);
        stream.mmio_write(super::StreamReg::Cbl, 4, 1, &mut stream_intsts);
        stream.mmio_write(super::StreamReg::Lvi, 2, 1, &mut stream_intsts);
        stream.mmio_write(
            super::StreamReg::CtlSts,
            4,
            (super::SD_CTL_RUN | super::SD_CTL_SRST) as u64,
            &mut stream_intsts,
        );

        let mut mem = CountingMem::new(0x3000, 128);
        // BDL entry 0: 1 byte at BUF0, no IOC.
        mem.data[BDL_BASE as usize..BDL_BASE as usize + 8].copy_from_slice(&BUF0.to_le_bytes());
        mem.data[BDL_BASE as usize + 8..BDL_BASE as usize + 12]
            .copy_from_slice(&1u32.to_le_bytes());
        mem.data[BDL_BASE as usize + 12..BDL_BASE as usize + 16]
            .copy_from_slice(&0u32.to_le_bytes());
        mem.data[BUF0 as usize] = 0xaa;

        let mut audio = AudioRingBuffer::new(8);
        let mut intsts = 0u32;

        // Consume the first (and only) byte, advancing to BDL index 1.
        stream.process(&mut mem, &mut audio, &mut intsts);
        assert_eq!(stream.bdl_index, 1);

        // Shrink LVI to 0; the cursor should reset back to entry 0.
        stream.mmio_write(super::StreamReg::Lvi, 2, 0, &mut intsts);
        assert_eq!(stream.lvi, 0);
        assert_eq!(stream.bdl_index, 0);
        assert_eq!(stream.bdl_offset, 0);
    }

    #[test]
    fn bdl_entry_address_overflow_is_treated_as_empty_entry() {
        struct PanicMem;

        impl MemoryBus for PanicMem {
            fn read_physical(&mut self, _addr: u64, _dst: &mut [u8]) {
                panic!("unexpected guest memory read");
            }

            fn write_physical(&mut self, _addr: u64, _src: &[u8]) {
                panic!("unexpected guest memory write");
            }
        }

        let mut stream = HdaStream::new(super::StreamId::Out0);
        let mut intsts = 0u32;
        // Place the BDL base near u64::MAX so index * 16 will overflow for small indices.
        stream.mmio_write(super::StreamReg::Bdpl, 4, 0xffff_ff80, &mut intsts);
        stream.mmio_write(super::StreamReg::Bdpu, 4, 0xffff_ffff, &mut intsts);

        let mut mem = PanicMem;
        let entry = stream.read_bdl_entry(&mut mem, 8);
        assert_eq!(entry.addr, 0);
        assert_eq!(entry.len, 0);
        assert!(!entry.ioc);
    }

    #[test]
    fn stream_process_20bit_stereo_ticks_use_32bit_containers() {
        const BDL_BASE: u64 = 0x1000;
        const BUF_ADDR: u64 = 0x2000;

        // HDA FMT encoding: 48kHz base, mult=1, div=1, 20-bit, 2 channels.
        const FMT_20BIT_STEREO: u16 = 0x21;

        let mut stream = HdaStream::new(super::StreamId::Out0);
        let mut stream_intsts = 0u32;
        stream.mmio_write(super::StreamReg::Bdpl, 4, BDL_BASE, &mut stream_intsts);
        stream.mmio_write(super::StreamReg::Bdpu, 4, 0, &mut stream_intsts);
        stream.mmio_write(
            super::StreamReg::Fmt,
            2,
            FMT_20BIT_STEREO as u64,
            &mut stream_intsts,
        );
        stream.mmio_write(super::StreamReg::Cbl, 4, 2048, &mut stream_intsts);
        stream.mmio_write(super::StreamReg::Lvi, 2, 0, &mut stream_intsts);
        stream.mmio_write(
            super::StreamReg::CtlSts,
            4,
            (super::SD_CTL_RUN | super::SD_CTL_SRST) as u64,
            &mut stream_intsts,
        );

        let expected = 4usize * 2usize * 128usize;
        let entry_len = (expected * 2) as u32;

        let mut mem = CountingMem::new(0x4000, 64);
        let bdl_off = BDL_BASE as usize;
        mem.data[bdl_off..bdl_off + 8].copy_from_slice(&BUF_ADDR.to_le_bytes());
        mem.data[bdl_off + 8..bdl_off + 12].copy_from_slice(&entry_len.to_le_bytes());
        mem.data[bdl_off + 12..bdl_off + 16].copy_from_slice(&0u32.to_le_bytes());
        for i in 0..entry_len as usize {
            if let Some(slot) = mem.data.get_mut(BUF_ADDR as usize + i) {
                *slot = (i & 0xff) as u8;
            }
        }

        let mut audio = AudioRingBuffer::new(4096);
        let mut intsts = 0u32;
        stream.process(&mut mem, &mut audio, &mut intsts);

        assert_eq!(audio.len(), expected);
    }

    #[test]
    fn stream_process_bdl_flags_range_overflow_does_not_touch_memory() {
        let mut stream = HdaStream::new(StreamId::Out0);
        stream.ctl = SD_CTL_SRST | SD_CTL_RUN;
        stream.cbl = 1;
        stream.lvi = 0;

        // With the 128-byte aligned base below, bdl_index=7 yields:
        //   addr = base + 7*16 = u64::MAX - 15
        //   flags_addr = addr + 12 = u64::MAX - 3
        // Reading flags as a u32 would require computing `flags_addr + 4`, which overflows.
        stream.bdpl = 0xFFFF_FF80;
        stream.bdpu = 0xFFFF_FFFF;
        stream.bdl_index = 7;

        let mut mem = PanicMem;
        let mut audio = AudioRingBuffer::new(16);
        let mut intsts = 0u32;
        stream.process(&mut mem, &mut audio, &mut intsts);
        assert!(audio.is_empty());
        assert_eq!(intsts, 0);
    }

    #[test]
    fn stream_process_dma_range_overflow_does_not_read_buffer() {
        let mut mem = StrictReadMem::new();

        let bdl_base = 0x3000u64;
        let entry_addr = u64::MAX - 3;
        let entry_len = 4u32;
        let entry_flags = 0u32;
        mem.write(bdl_base, &entry_addr.to_le_bytes());
        mem.write(bdl_base + 8, &entry_len.to_le_bytes());
        mem.write(bdl_base + 12, &entry_flags.to_le_bytes());

        let mut stream = HdaStream::new(StreamId::Out0);
        stream.ctl = SD_CTL_SRST | SD_CTL_RUN;
        stream.cbl = 4;
        stream.lvi = 0;
        stream.bdpl = bdl_base as u32;
        stream.bdpu = 0;
        stream.bdl_index = 0;
        stream.bdl_offset = 0;

        let mut audio = AudioRingBuffer::new(16);
        let mut intsts = 0u32;
        stream.process(&mut mem, &mut audio, &mut intsts);
        assert!(audio.is_empty());
        assert_eq!(intsts, 0);
    }

    #[test]
    fn stream_process_capture_dma_range_overflow_does_not_write_buffer_or_consume_capture() {
        let mut mem = StrictReadMem::new();

        let bdl_base = 0x4000u64;
        let entry_addr = u64::MAX - 3;
        let entry_len = 4u32;
        let entry_flags = 0u32;
        mem.write(bdl_base, &entry_addr.to_le_bytes());
        mem.write(bdl_base + 8, &entry_len.to_le_bytes());
        mem.write(bdl_base + 12, &entry_flags.to_le_bytes());

        let mut stream = HdaStream::new(StreamId::In0);
        stream.ctl = SD_CTL_SRST | SD_CTL_RUN;
        stream.cbl = 4;
        stream.lvi = 0;
        stream.bdpl = bdl_base as u32;
        stream.bdpu = 0;
        stream.bdl_index = 0;
        stream.bdl_offset = 0;

        let mut capture = AudioRingBuffer::new(16);
        capture.push(&[1, 2, 3, 4]);
        let before = capture.len();

        let mut intsts = 0u32;
        stream.process_capture(&mut mem, &mut capture, &mut intsts);

        assert_eq!(capture.len(), before);
        assert_eq!(intsts, 0);
    }
}
