use aero_audio::hda::HdaController;
use aero_audio::mem::MemoryAccess;

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

