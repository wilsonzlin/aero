#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::SharedArrayBuffer;

use aero_audio::hda::HdaController;
use aero_audio::mem::MemoryAccess;
use aero_platform::audio::mic_bridge::MicBridge;

fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

/// Contiguous guest-physical memory view backed by the module's linear memory.
///
/// This is similar to the `LinearGuestMemory` wrapper used by the UHCI runtime, but implements
/// the `aero_audio::mem::MemoryAccess` trait used by the HDA controller model.
#[derive(Clone, Copy)]
struct LinearGuestMemory {
    guest_base: u32,
    guest_size: u32,
}

impl LinearGuestMemory {
    fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        if guest_base == 0 {
            return Err(js_error("guest_base must be non-zero"));
        }
        if guest_size == 0 {
            return Err(js_error("guest_size must be non-zero"));
        }

        let pages = core::arch::wasm32::memory_size(0) as u64;
        let mem_bytes = pages.saturating_mul(64 * 1024);

        let end = guest_base as u64 + guest_size as u64;
        if end > mem_bytes {
            return Err(js_error(&format!(
                "Guest RAM region out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size:x} end=0x{end:x} wasm_mem_bytes=0x{mem_bytes:x}"
            )));
        }

        Ok(Self {
            guest_base,
            guest_size,
        })
    }

    #[inline]
    fn translate(&self, paddr: u64, len: usize) -> Option<u32> {
        let paddr_u32 = u32::try_from(paddr).ok()?;
        if paddr_u32 >= self.guest_size {
            return None;
        }
        let end = paddr_u32.checked_add(len as u32)?;
        if end > self.guest_size {
            return None;
        }
        self.guest_base.checked_add(paddr_u32)
    }
}

impl MemoryAccess for LinearGuestMemory {
    fn read_physical(&self, addr: u64, buf: &mut [u8]) {
        let Some(linear) = self.translate(addr, buf.len()) else {
            buf.fill(0);
            return;
        };

        // Safety: `translate` bounds-checks that `[linear, linear+len)` is within the module's
        // linear memory and within the guest RAM region.
        unsafe {
            let src = core::slice::from_raw_parts(linear as *const u8, buf.len());
            buf.copy_from_slice(src);
        }
    }

    fn write_physical(&mut self, addr: u64, buf: &[u8]) {
        let Some(linear) = self.translate(addr, buf.len()) else {
            return;
        };

        // Safety: `translate` bounds-checks that `[linear, linear+len)` is within the module's
        // linear memory and within the guest RAM region.
        unsafe {
            let dst = core::slice::from_raw_parts_mut(linear as *mut u8, buf.len());
            dst.copy_from_slice(buf);
        }
    }
}

/// Minimal JS<->WASM bridge for the `aero_audio::hda::HdaController` device model.
///
/// This is intended to be owned by the browser I/O worker:
/// - JS implements the PCI config/BAR plumbing.
/// - Rust implements the full HDA MMIO + DMA engine.
///
/// Microphone samples are pulled from a `SharedArrayBuffer` mic ring buffer produced by the
/// browser audio graph (AudioWorklet or synthetic mic), using `aero_platform::audio::mic_bridge::MicBridge`.
#[wasm_bindgen]
pub struct HdaControllerBridge {
    hda: HdaController,
    mem: LinearGuestMemory,
    mic: Option<MicBridge>,
}

#[wasm_bindgen]
impl HdaControllerBridge {
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        let mem = LinearGuestMemory::new(guest_base, guest_size)?;
        Ok(Self {
            hda: HdaController::new(),
            mem,
            mic: None,
        })
    }

    /// Return whether the device's interrupt line should be asserted.
    ///
    /// This is derived from guest-visible HDA INTCTL/INTSTS state.
    pub fn irq_level(&self) -> bool {
        self.hda.irq_level()
    }

    pub fn mmio_read(&mut self, offset: u32, size: u32) -> u32 {
        let size = size as usize;
        if !matches!(size, 1 | 2 | 4) {
            return 0;
        }
        self.hda.mmio_read(offset as u64, size) as u32
    }

    pub fn mmio_write(&mut self, offset: u32, size: u32, value: u32) {
        let size = size as usize;
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        self.hda
            .mmio_write(offset as u64, size, u64::from(value));
    }

    /// Advance the HDA device model by `frames` of host time.
    ///
    /// `frames` is interpreted at the controller's configured host/output sample rate
    /// (`HdaController::output_rate_hz`, default 48kHz).
    pub fn step_frames(&mut self, frames: u32) {
        let frames = frames as usize;
        if let Some(mic) = self.mic.as_mut() {
            self.hda.process_with_capture(&mut self.mem, frames, mic);
        } else {
            self.hda.process(&mut self.mem, frames);
        }
    }

    /// Attach (or detach) a microphone capture ring buffer.
    ///
    /// When attached, captured samples are consumed from the ring buffer and fed into the HDA
    /// capture stream (SD1) via `HdaController::process_with_capture`.
    pub fn set_mic_ring_buffer(&mut self, sab: Option<SharedArrayBuffer>) -> Result<(), JsValue> {
        self.mic = match sab {
            Some(sab) => Some(MicBridge::from_shared_buffer(sab)?),
            None => None,
        };
        Ok(())
    }

    /// Set the host/input sample rate used when consuming microphone samples.
    ///
    /// This must match the sample rate of the microphone capture graph (e.g. `AudioContext.sampleRate`).
    pub fn set_capture_sample_rate_hz(&mut self, capture_sample_rate_hz: u32) {
        if capture_sample_rate_hz == 0 {
            return;
        }
        self.hda.set_capture_sample_rate_hz(capture_sample_rate_hz);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn step_frames_completes_on_oob_dma_pointer() {
        let mut guest = vec![0u8; 0x4000];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u32;

        let mut bridge = HdaControllerBridge::new(guest_base, guest_size).unwrap();

        // Bring controller out of reset.
        bridge.hda.mmio_write(0x08, 4, 0x1); // GCTL.CRST

        // Configure the codec converter to listen on stream 1, channel 0.
        // SET_STREAM_CHANNEL: verb 0x706, payload = stream<<4 | channel
        let set_stream_ch = (0x706u32 << 8) | 0x10;
        bridge.hda.codec_mut().execute_verb(2, set_stream_ch);

        // Stream format: 48kHz, 16-bit, 2ch.
        let fmt_raw: u16 = (1 << 4) | 0x1;
        // SET_CONVERTER_FORMAT (4-bit verb group 0x2 encoded in low 16 bits)
        let set_fmt = (0x200u32 << 8) | (fmt_raw as u8 as u32);
        bridge.hda.codec_mut().execute_verb(2, set_fmt);

        // Guest buffer layout: BDL is in-bounds, but it points at an out-of-bounds PCM address.
        let bdl_base = 0x1000u64;
        let pcm_len_bytes = 512u32; // 128 frames @ 16-bit stereo
        let oob_pcm_base = u64::from(guest_size) + 0x1000;

        bridge.mem.write_u64(bdl_base, oob_pcm_base);
        bridge.mem.write_u32(bdl_base + 8, pcm_len_bytes);
        bridge.mem.write_u32(bdl_base + 12, 1); // IOC=1

        // Configure stream descriptor 0.
        {
            let sd = bridge.hda.stream_mut(0);
            sd.bdpl = bdl_base as u32;
            sd.bdpu = 0;
            sd.cbl = pcm_len_bytes;
            sd.lvi = 0;
            sd.fmt = fmt_raw;
            // SRST | RUN | IOCE | stream number 1.
            sd.ctl = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 20);
        }

        // The call should complete without panicking even though the DMA address is invalid.
        bridge.step_frames(128);
    }
}
