use aero_audio::hda::HdaController;
use aero_audio::mem::MemoryAccess;
use aero_audio::capture::SilenceCaptureSource;

/// Guest memory wrapper used by browser/WASM bridges: out-of-bounds DMA must not panic.
///
/// This mirrors the defensive behavior required by the worker runtime:
/// - reads from invalid guest DMA addresses yield 0 bytes (zero-filled)
/// - writes to invalid guest DMA addresses are ignored
#[derive(Clone)]
struct SafeGuestMemory {
    data: Vec<u8>,
}

impl SafeGuestMemory {
    fn new(size: usize) -> Self {
        Self { data: vec![0; size] }
    }
}

impl MemoryAccess for SafeGuestMemory {
    fn read_physical(&self, addr: u64, buf: &mut [u8]) {
        if buf.is_empty() {
            return;
        }

        let Some(end) = addr.checked_add(buf.len() as u64) else {
            buf.fill(0);
            return;
        };
        if end > self.data.len() as u64 {
            buf.fill(0);
            return;
        }

        let start = addr as usize;
        buf.copy_from_slice(&self.data[start..start + buf.len()]);
    }

    fn write_physical(&mut self, addr: u64, buf: &[u8]) {
        if buf.is_empty() {
            return;
        }

        let Some(end) = addr.checked_add(buf.len() as u64) else {
            return;
        };
        if end > self.data.len() as u64 {
            return;
        }

        let start = addr as usize;
        self.data[start..start + buf.len()].copy_from_slice(buf);
    }
}

#[test]
fn hda_process_completes_on_oob_bdl_address() {
    let mut hda = HdaController::new();
    let mut mem = SafeGuestMemory::new(0x4000);

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Configure the codec converter to listen on stream 1, channel 0.
    // SET_STREAM_CHANNEL: verb 0x706, payload = stream<<4 | channel
    let set_stream_ch = (0x706u32 << 8) | 0x10;
    hda.codec_mut().execute_verb(2, set_stream_ch);

    // Stream format: 48kHz, 16-bit, 2ch.
    let fmt_raw: u16 = (1 << 4) | 0x1;
    // SET_CONVERTER_FORMAT (4-bit verb group 0x2 encoded in low 16 bits)
    let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
    hda.codec_mut().execute_verb(2, set_fmt);

    // Guest buffer layout: BDL is in-bounds, but it points at an out-of-bounds PCM address.
    let bdl_base = 0x1000u64;
    let pcm_len_bytes = 512u32; // 128 frames @ 16-bit stereo
    let oob_pcm_base = mem.data.len() as u64 + 0x1000;

    // One BDL entry pointing at an invalid buffer address.
    mem.write_u64(bdl_base, oob_pcm_base);
    mem.write_u32(bdl_base + 8, pcm_len_bytes);
    mem.write_u32(bdl_base + 12, 1); // IOC=1

    // Configure stream descriptor 0.
    {
        let sd = hda.stream_mut(0);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = pcm_len_bytes;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // SRST | RUN | IOCE | stream number 1.
        sd.ctl = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 20);
    }

    // The call should complete without panicking even though the DMA address is invalid.
    hda.process(&mut mem, 128);
}

#[test]
fn hda_process_completes_on_dma_addr_overflow() {
    let mut hda = HdaController::new();
    let mut mem = SafeGuestMemory::new(0x4000);

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Configure the codec converter to listen on stream 1, channel 0.
    // SET_STREAM_CHANNEL: verb 0x706, payload = stream<<4 | channel
    let set_stream_ch = (0x706u32 << 8) | 0x10;
    hda.codec_mut().execute_verb(2, set_stream_ch);

    // Stream format: 48kHz, 16-bit, 2ch.
    let fmt_raw: u16 = (1 << 4) | 0x1;
    // SET_CONVERTER_FORMAT (4-bit verb group 0x2 encoded in low 16 bits)
    let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
    hda.codec_mut().execute_verb(2, set_fmt);

    // Guest buffer layout: BDL is in-bounds, but the buffer address is chosen so
    // `addr + bdl_offset` will overflow on the second tick.
    let bdl_base = 0x1000u64;
    let pcm_len_bytes = 0x1000u32; // larger than the first DMA read (512 bytes)
    let near_end = u64::MAX - 100;

    mem.write_u64(bdl_base, near_end);
    mem.write_u32(bdl_base + 8, pcm_len_bytes);
    mem.write_u32(bdl_base + 12, 0);

    {
        let sd = hda.stream_mut(0);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = pcm_len_bytes;
        sd.lvi = 0;
        sd.fmt = fmt_raw;
        // SRST | RUN | stream number 1.
        sd.ctl = (1 << 0) | (1 << 1) | (1 << 20);
    }

    // First tick: consumes 512 bytes (128 frames @ 16-bit stereo), leaving a non-zero BDL offset.
    hda.process(&mut mem, 128);
    // Second tick: attempts to DMA from `near_end + 512`, which overflows; must not panic.
    hda.process(&mut mem, 128);
}

#[test]
fn hda_capture_dma_write_completes_on_oob_bdl_address() {
    let mut hda = HdaController::new();
    let mut mem = SafeGuestMemory::new(0x4000);
    let mut capture = SilenceCaptureSource;

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Configure the input converter to use stream ID 2, channel 0.
    // SET_STREAM_CHANNEL: verb 0x706, payload = stream<<4 | channel
    let set_stream_ch = (0x706u32 << 8) | 0x20;
    hda.codec_mut().execute_verb(4, set_stream_ch);

    let bdl_base = 0x1000u64;
    let oob_dst = mem.data.len() as u64 + 0x1000;
    let buf_len = 0x1000u32;

    mem.write_u64(bdl_base, oob_dst);
    mem.write_u32(bdl_base + 8, buf_len);
    mem.write_u32(bdl_base + 12, 0);

    {
        let sd = hda.stream_mut(1);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = buf_len;
        sd.lvi = 0;
        sd.fmt = 0x0010; // 48kHz, 16-bit, mono
        // SRST | RUN | stream number 2.
        sd.ctl = (1 << 0) | (1 << 1) | (2 << 20);
    }

    hda.process_with_capture(&mut mem, 128, &mut capture);
}

#[test]
fn hda_capture_dma_write_completes_on_dma_addr_overflow() {
    let mut hda = HdaController::new();
    let mut mem = SafeGuestMemory::new(0x4000);
    let mut capture = SilenceCaptureSource;

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Configure the input converter to use stream ID 2, channel 0.
    let set_stream_ch = (0x706u32 << 8) | 0x20;
    hda.codec_mut().execute_verb(4, set_stream_ch);

    let bdl_base = 0x1000u64;
    let buf_len = 0x1000u32;
    // Choose an address such that `addr + bdl_offset` will overflow on the second tick.
    let near_end = u64::MAX - 100;

    mem.write_u64(bdl_base, near_end);
    mem.write_u32(bdl_base + 8, buf_len);
    mem.write_u32(bdl_base + 12, 0);

    {
        let sd = hda.stream_mut(1);
        sd.bdpl = bdl_base as u32;
        sd.bdpu = 0;
        sd.cbl = buf_len;
        sd.lvi = 0;
        sd.fmt = 0x0010; // 48kHz, 16-bit, mono
        // SRST | RUN | stream number 2.
        sd.ctl = (1 << 0) | (1 << 1) | (2 << 20);
    }

    // First tick writes some bytes at `near_end` (SafeGuestMemory ignores the OOB/overflow write).
    hda.process_with_capture(&mut mem, 128, &mut capture);
    // Second tick would attempt to DMA at `near_end + bdl_offset` (overflow) and must not panic.
    hda.process_with_capture(&mut mem, 128, &mut capture);
}

#[test]
fn hda_process_completes_on_oob_corb_rirb_addresses() {
    let mut hda = HdaController::new();
    let mut mem = SafeGuestMemory::new(0x4000);

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Program invalid CORB + RIRB base addresses (outside guest RAM).
    let oob_base = mem.data.len() as u64 + 0x1000;
    hda.mmio_write(0x40, 4, oob_base); // CORBLBASE
    hda.mmio_write(0x44, 4, 0); // CORBUBASE
    hda.mmio_write(0x50, 4, oob_base); // RIRBLBASE
    hda.mmio_write(0x54, 4, 0); // RIRBUBASE

    // Enable CORB + RIRB DMA engines.
    hda.mmio_write(0x4c, 1, 0x02); // CORBCTL.RUN
    hda.mmio_write(0x5c, 1, 0x02); // RIRBCTL.RUN

    // Signal a pending command by advancing CORBWP.
    hda.mmio_write(0x48, 2, 0x0001);

    // Should complete without panicking even though ring base addresses are invalid.
    hda.process(&mut mem, 1);
}

#[test]
fn hda_process_completes_on_oob_posbuf_address() {
    let mut hda = HdaController::new();
    let mut mem = SafeGuestMemory::new(0x4000);

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Enable the DMA position buffer (POSBUF) with an out-of-bounds base address.
    let oob_base = mem.data.len() as u64 + 0x1000;
    hda.mmio_write(0x70, 4, oob_base | 0x1); // DPLBASE.ENABLE + base
    hda.mmio_write(0x74, 4, 0); // DPUBASE

    // Should complete without panicking; the POSBUF write is ignored by SafeGuestMemory.
    hda.process(&mut mem, 1);
}

#[test]
fn hda_process_completes_on_corb_addr_overflow() {
    let mut hda = HdaController::new();
    let mut mem = SafeGuestMemory::new(0x4000);

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // Choose a CORB base such that `corb_base + corbrp*4` overflows.
    // With CORBRP=0 and CORBWP=1, `process_corb` increments CORBRP to 1 and computes `base + 4`.
    hda.mmio_write(0x40, 4, 0xffff_fffc); // CORBLBASE = u64::MAX-3 (low 32 bits)
    hda.mmio_write(0x44, 4, 0xffff_ffff); // CORBUBASE = u64::MAX (high 32 bits)
    hda.mmio_write(0x50, 4, 0); // RIRBLBASE (unused in this test)
    hda.mmio_write(0x54, 4, 0); // RIRBUBASE

    // Enable CORB + RIRB DMA engines.
    hda.mmio_write(0x4c, 1, 0x02); // CORBCTL.RUN
    hda.mmio_write(0x5c, 1, 0x02); // RIRBCTL.RUN

    // Force one pending CORB entry.
    hda.mmio_write(0x4a, 2, 0x0000); // CORBRP=0
    hda.mmio_write(0x48, 2, 0x0001); // CORBWP=1

    // Should not panic (checked_add guards the overflow).
    hda.process(&mut mem, 0);
}

#[test]
fn hda_process_completes_on_rirb_addr_overflow() {
    let mut hda = HdaController::new();
    let mut mem = SafeGuestMemory::new(0x4000);

    // Bring controller out of reset.
    hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

    // CORB in-bounds with one dummy command.
    let corb_base = 0x1000u64;
    mem.write_u32(corb_base, 0);
    hda.mmio_write(0x40, 4, corb_base); // CORBLBASE
    hda.mmio_write(0x44, 4, 0); // CORBUBASE

    // Choose an RIRB base such that `rirb_base + rirbwp*8` overflows after the first response.
    // `write_rirb_response` increments RIRBWP from 0 to 1 and then computes `base + 8`.
    hda.mmio_write(0x50, 4, 0xffff_fff8); // RIRBLBASE = u64::MAX-7 (low 32 bits)
    hda.mmio_write(0x54, 4, 0xffff_ffff); // RIRBUBASE = u64::MAX (high 32 bits)

    // Set pointers so the first command is read from entry 0.
    hda.mmio_write(0x4a, 2, 0x00ff); // CORBRP=0xFF
    hda.mmio_write(0x48, 2, 0x0000); // CORBWP=0

    // Enable CORB + RIRB DMA engines.
    hda.mmio_write(0x4c, 1, 0x02); // CORBCTL.RUN
    hda.mmio_write(0x5c, 1, 0x02); // RIRBCTL.RUN

    // Should not panic (checked_add guards the overflow).
    hda.process(&mut mem, 0);
}
