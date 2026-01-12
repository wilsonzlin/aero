//! WASM-side bridge for exposing a guest-visible virtio-snd device via virtio-pci (modern transport).
//!
//! The browser runtime wires this into the emulated PCI bus and forwards BAR0 MMIO reads/writes into
//! [`VirtioSndPciBridge::mmio_read`] / [`VirtioSndPciBridge::mmio_write`]. The virtqueue structures
//! and DMA buffers live in guest RAM inside the shared WASM linear memory; guest physical address 0
//! maps to `guest_base` (see `guest_ram_layout`).
//!
//! Audio output is delivered to the browser via the canonical AudioWorklet `SharedArrayBuffer` ring
//! buffer (`aero_platform::audio::worklet_bridge::WorkletBridge`). Microphone capture samples are
//! consumed from the canonical mic ring buffer (`aero_platform::audio::mic_bridge::MicBridge`).
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

use js_sys::SharedArrayBuffer;

use std::cell::Cell;
use std::rc::Rc;

use aero_audio::capture::AudioCaptureSource;
use aero_audio::sink::AudioSink;

use aero_platform::audio::mic_bridge::MicBridge;
use aero_platform::audio::worklet_bridge::WorkletBridge;

use aero_virtio::devices::snd::VirtioSnd;
use aero_virtio::memory::{GuestMemory, GuestMemoryError};
use aero_virtio::pci::{InterruptSink, VIRTIO_STATUS_DRIVER_OK, VirtioPciDevice};

use crate::guest_phys::{GuestRamRange, guest_ram_phys_end_exclusive, translate_guest_paddr_range};

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn wasm_memory_byte_len() -> u64 {
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
}

fn clamp_host_sample_rate_hz(rate_hz: u32) -> u32 {
    // Use the shared audio clamp constant so the WASM surface stays consistent with other bridges
    // and snapshot restore behavior.
    rate_hz.clamp(1, aero_audio::MAX_HOST_SAMPLE_RATE_HZ)
}

#[inline]
fn validate_mmio_size(size: u8) -> usize {
    match size {
        1 | 2 | 4 => size as usize,
        _ => 0,
    }
}

// Cap open-bus slices so a malicious guest cannot force unbounded allocations.
const OPEN_BUS_SLICE_MAX: usize = 64 * 1024;
static OPEN_BUS_BYTES: [u8; OPEN_BUS_SLICE_MAX] = [0xFF; OPEN_BUS_SLICE_MAX];

struct WasmGuestMemory {
    ram_ptr: *mut u8,
    ram_bytes: u64,
    open_bus_write: Vec<u8>,
}

impl WasmGuestMemory {
    fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        if guest_base == 0 {
            return Err(js_error("guest_base must be non-zero"));
        }

        let mem_bytes = wasm_memory_byte_len();
        let guest_base_u64 = u64::from(guest_base);
        if guest_base_u64 > mem_bytes {
            return Err(js_error(format!(
                "guest RAM mapping out of bounds: guest_base=0x{guest_base:x} wasm_mem=0x{mem_bytes:x}"
            )));
        }

        // Match other WASM bridges: treat `guest_size = 0` as "use remainder of linear memory".
        let guest_size_u64 = if guest_size == 0 {
            mem_bytes.saturating_sub(guest_base_u64)
        } else {
            u64::from(guest_size)
        };
        // Keep guest RAM below the PCI MMIO BAR window (see `guest_ram_layout` contract).
        let guest_size_u64 = guest_size_u64.min(crate::guest_layout::PCI_MMIO_BASE);

        if guest_size_u64 == 0 {
            return Err(js_error("guest_size must be non-zero"));
        }

        let end = guest_base_u64
            .checked_add(guest_size_u64)
            .ok_or_else(|| js_error("guest_base + guest_size overflow"))?;
        if end > mem_bytes {
            return Err(js_error(format!(
                "guest RAM out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size_u64:x} wasm_mem=0x{mem_bytes:x}"
            )));
        }

        Ok(Self {
            ram_ptr: guest_base as *mut u8,
            ram_bytes: guest_size_u64,
            open_bus_write: Vec::new(),
        })
    }

    #[inline]
    fn ram_slice<'a>(
        &'a self,
        paddr: u64,
        ram_offset: u64,
        len: usize,
    ) -> Result<&'a [u8], GuestMemoryError> {
        let end = ram_offset
            .checked_add(len as u64)
            .ok_or(GuestMemoryError::OutOfBounds { addr: paddr, len })?;
        if end > self.ram_bytes {
            return Err(GuestMemoryError::OutOfBounds { addr: paddr, len });
        }
        let off = usize::try_from(ram_offset)
            .map_err(|_| GuestMemoryError::OutOfBounds { addr: paddr, len })?;
        // Safety: `ram_offset..ram_offset+len` lies within the configured guest RAM backing store.
        unsafe { Ok(core::slice::from_raw_parts(self.ram_ptr.add(off), len)) }
    }

    #[inline]
    fn ram_slice_mut<'a>(
        &'a mut self,
        paddr: u64,
        ram_offset: u64,
        len: usize,
    ) -> Result<&'a mut [u8], GuestMemoryError> {
        let end = ram_offset
            .checked_add(len as u64)
            .ok_or(GuestMemoryError::OutOfBounds { addr: paddr, len })?;
        if end > self.ram_bytes {
            return Err(GuestMemoryError::OutOfBounds { addr: paddr, len });
        }
        let off = usize::try_from(ram_offset)
            .map_err(|_| GuestMemoryError::OutOfBounds { addr: paddr, len })?;
        // Safety: `ram_offset..ram_offset+len` lies within the configured guest RAM backing store.
        unsafe { Ok(core::slice::from_raw_parts_mut(self.ram_ptr.add(off), len)) }
    }

    #[inline]
    fn open_bus_slice(&self, paddr: u64, len: usize) -> Result<&'static [u8], GuestMemoryError> {
        if len > OPEN_BUS_SLICE_MAX {
            return Err(GuestMemoryError::OutOfBounds { addr: paddr, len });
        }
        Ok(&OPEN_BUS_BYTES[..len])
    }

    #[inline]
    fn open_bus_slice_mut(
        &mut self,
        paddr: u64,
        len: usize,
    ) -> Result<&mut [u8], GuestMemoryError> {
        if len > OPEN_BUS_SLICE_MAX {
            return Err(GuestMemoryError::OutOfBounds { addr: paddr, len });
        }
        if self.open_bus_write.len() < len {
            self.open_bus_write.resize(len, 0xFF);
        } else {
            self.open_bus_write[..len].fill(0xFF);
        }
        Ok(&mut self.open_bus_write[..len])
    }
}

impl GuestMemory for WasmGuestMemory {
    fn len(&self) -> u64 {
        guest_ram_phys_end_exclusive(self.ram_bytes)
    }

    fn read(&self, addr: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        dst.copy_from_slice(self.get_slice(addr, dst.len())?);
        Ok(())
    }

    fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), GuestMemoryError> {
        self.get_slice_mut(addr, src.len())?.copy_from_slice(src);
        Ok(())
    }

    fn get_slice(&self, addr: u64, len: usize) -> Result<&[u8], GuestMemoryError> {
        if len == 0 {
            if addr > self.len() {
                return Err(GuestMemoryError::OutOfBounds { addr, len });
            }
            return Ok(&[]);
        }

        match translate_guest_paddr_range(self.ram_bytes, addr, len) {
            GuestRamRange::Ram { ram_offset } => self.ram_slice(addr, ram_offset, len),
            GuestRamRange::Hole => self.open_bus_slice(addr, len),
            GuestRamRange::OutOfBounds => Err(GuestMemoryError::OutOfBounds { addr, len }),
        }
    }

    fn get_slice_mut(&mut self, addr: u64, len: usize) -> Result<&mut [u8], GuestMemoryError> {
        if len == 0 {
            if addr > self.len() {
                return Err(GuestMemoryError::OutOfBounds { addr, len });
            }
            // Safety: a zero-length slice may be created from a dangling pointer.
            return Ok(unsafe {
                core::slice::from_raw_parts_mut(core::ptr::NonNull::<u8>::dangling().as_ptr(), 0)
            });
        }

        match translate_guest_paddr_range(self.ram_bytes, addr, len) {
            GuestRamRange::Ram { ram_offset } => self.ram_slice_mut(addr, ram_offset, len),
            GuestRamRange::Hole => self.open_bus_slice_mut(addr, len),
            GuestRamRange::OutOfBounds => Err(GuestMemoryError::OutOfBounds { addr, len }),
        }
    }
}

#[derive(Default)]
struct OptionalWorkletSink {
    ring: Option<WorkletBridge>,
}

impl OptionalWorkletSink {
    fn attach(
        &mut self,
        ring: SharedArrayBuffer,
        capacity_frames: u32,
        channel_count: u32,
    ) -> Result<(), JsValue> {
        if capacity_frames == 0 {
            return Err(js_error("capacity_frames must be non-zero"));
        }
        if channel_count != 2 {
            return Err(js_error(
                "channel_count must be 2 for virtio-snd playback (stereo)",
            ));
        }
        let bridge = WorkletBridge::from_shared_buffer(ring, capacity_frames, channel_count)?;
        self.ring = Some(bridge);
        Ok(())
    }

    fn detach(&mut self) {
        self.ring = None;
    }
}

impl AudioSink for OptionalWorkletSink {
    fn push_interleaved_f32(&mut self, samples: &[f32]) {
        let Some(ring) = self.ring.as_ref() else {
            return;
        };
        let _ = ring.write_f32_interleaved(samples);
    }
}

#[derive(Default)]
struct OptionalMicCaptureSource {
    ring: Option<MicBridge>,
}

impl OptionalMicCaptureSource {
    fn attach(&mut self, ring: SharedArrayBuffer) -> Result<(), JsValue> {
        let mut bridge = MicBridge::from_shared_buffer(ring)?;
        // Microphone capture is latency-sensitive; if the AudioWorklet producer ran before the
        // guest attached the ring, discard any buffered samples so capture starts from the most
        // recent audio.
        bridge.discard_buffered_samples();
        bridge.reset_dropped_samples_baseline();
        self.ring = Some(bridge);
        Ok(())
    }

    fn detach(&mut self) {
        self.ring = None;
    }
}

impl AudioCaptureSource for OptionalMicCaptureSource {
    fn read_mono_f32(&mut self, out: &mut [f32]) -> usize {
        let Some(ring) = self.ring.as_ref() else {
            return 0;
        };
        ring.read_f32_into(out) as usize
    }

    fn take_dropped_samples(&mut self) -> u64 {
        let Some(ring) = self.ring.as_mut() else {
            return 0;
        };
        ring.take_dropped_samples_delta()
    }
}

#[derive(Clone)]
struct LegacyIrqLatch {
    asserted: Rc<Cell<bool>>,
}

impl InterruptSink for LegacyIrqLatch {
    fn raise_legacy_irq(&mut self) {
        self.asserted.set(true);
    }

    fn lower_legacy_irq(&mut self) {
        self.asserted.set(false);
    }

    fn signal_msix(&mut self, _vector: u16) {
        // MSI-X is not currently surfaced through this bridge.
    }
}

type SndDevice = VirtioSnd<OptionalWorkletSink, OptionalMicCaptureSource>;

#[wasm_bindgen]
pub struct VirtioSndPciBridge {
    mem: WasmGuestMemory,
    dev: VirtioPciDevice,
    irq_asserted: Rc<Cell<bool>>,
    pci_command: u16,
}

#[wasm_bindgen]
impl VirtioSndPciBridge {
    /// Create a new virtio-snd (virtio-pci modern) bridge bound to the provided guest RAM mapping.
    ///
    /// - `guest_base` is the byte offset inside wasm linear memory where guest physical address 0
    ///   begins (see `guest_ram_layout`).
    /// - `guest_size` is the guest RAM size in bytes. Pass `0` to use "the remainder of linear
    ///   memory" as guest RAM.
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        let mem = WasmGuestMemory::new(guest_base, guest_size)?;

        let asserted = Rc::new(Cell::new(false));
        let irq = LegacyIrqLatch {
            asserted: asserted.clone(),
        };

        let output = OptionalWorkletSink::default();
        let capture = OptionalMicCaptureSource::default();
        let snd = VirtioSnd::new_with_capture(output, capture);
        let dev = VirtioPciDevice::new(Box::new(snd), Box::new(irq));

        Ok(Self {
            mem,
            dev,
            irq_asserted: asserted,
            pci_command: 0,
        })
    }

    #[inline]
    fn bus_master_enabled(&self) -> bool {
        (self.pci_command & (1 << 2)) != 0
    }

    fn snd_mut(&mut self) -> &mut SndDevice {
        self.dev
            .device_mut::<SndDevice>()
            .expect("VirtioPciDevice should contain a VirtioSnd device")
    }

    /// Mirror the guest-written PCI command register (0x04, low 16 bits) into the WASM device
    /// wrapper.
    ///
    /// This is used to enforce PCI Bus Master Enable gating for DMA. In a JS runtime, the PCI
    /// configuration space lives in TypeScript (`PciBus`), so the WASM bridge must be updated via
    /// this explicit hook.
    pub fn set_pci_command(&mut self, command: u32) {
        self.pci_command = (command & 0xffff) as u16;
    }

    /// Read from the virtio-pci BAR0 MMIO region.
    pub fn mmio_read(&mut self, offset: u32, size: u8) -> u32 {
        let size = validate_mmio_size(size);
        if size == 0 {
            return 0xFFFF_FFFF;
        }

        let mut buf = [0u8; 4];
        self.dev.bar0_read(u64::from(offset), &mut buf[..size]);
        u32::from_le_bytes(buf)
    }

    /// Write to the virtio-pci BAR0 MMIO region.
    pub fn mmio_write(&mut self, offset: u32, size: u8, value: u32) {
        let size = validate_mmio_size(size);
        if size == 0 {
            return;
        }

        let bytes = value.to_le_bytes();
        self.dev.bar0_write(u64::from(offset), &bytes[..size]);
        // BAR0 writes are side-effect-free w.r.t guest RAM; execute any notified queues
        // synchronously now that we have access to guest memory.
        //
        // Only DMA when PCI Bus Master Enable is set (command bit 2).
        if self.bus_master_enabled() {
            self.dev.process_notified_queues(&mut self.mem);
        }
    }

    /// Process pending virtqueue work and deliver interrupts.
    pub fn poll(&mut self) {
        // Only DMA when PCI Bus Master Enable is set (command bit 2).
        if !self.bus_master_enabled() {
            return;
        }
        self.dev.poll(&mut self.mem);
    }

    /// Whether the guest driver has set `VIRTIO_STATUS_DRIVER_OK`.
    pub fn driver_ok(&mut self) -> bool {
        // Common_cfg.device_status is at offset 0x14 (contract v1).
        let status = self.mmio_read(0x14, 1) as u8;
        (status & VIRTIO_STATUS_DRIVER_OK) != 0
    }

    /// Whether the PCI INTx line should be asserted.
    pub fn irq_asserted(&self) -> bool {
        self.irq_asserted.get()
    }

    /// Attach the AudioWorklet output ring buffer (producer side; AudioWorklet is the consumer).
    pub fn attach_audio_ring(
        &mut self,
        ring_sab: SharedArrayBuffer,
        capacity_frames: u32,
        channel_count: u32,
    ) -> Result<(), JsValue> {
        self.snd_mut()
            .output_mut()
            .attach(ring_sab, capacity_frames, channel_count)
    }

    pub fn detach_audio_ring(&mut self) {
        self.snd_mut().output_mut().detach();
    }

    /// Convenience helper: attach/detach the audio ring buffer using an `Option`.
    ///
    /// Mirrors other audio bridges which use `set_*_ring_buffer(undefined)` to detach.
    pub fn set_audio_ring_buffer(
        &mut self,
        ring_sab: Option<SharedArrayBuffer>,
        capacity_frames: u32,
        channel_count: u32,
    ) -> Result<(), JsValue> {
        match ring_sab {
            Some(sab) => self.attach_audio_ring(sab, capacity_frames, channel_count),
            None => {
                self.detach_audio_ring();
                Ok(())
            }
        }
    }

    /// Attach/detach the microphone capture ring buffer (consumer side; AudioWorklet is the producer).
    ///
    /// This does not configure the capture sample rate; call `set_capture_sample_rate_hz` separately.
    pub fn set_mic_ring_buffer(
        &mut self,
        ring_sab: Option<SharedArrayBuffer>,
    ) -> Result<(), JsValue> {
        match ring_sab {
            Some(sab) => self.snd_mut().capture_source_mut().attach(sab),
            None => {
                self.snd_mut().capture_source_mut().detach();
                Ok(())
            }
        }
    }

    /// Set the host/output sample rate used for the playback/TX resampling path.
    pub fn set_host_sample_rate_hz(&mut self, rate: u32) -> Result<(), JsValue> {
        if rate == 0 {
            return Err(js_error("rate must be non-zero"));
        }
        self.snd_mut()
            .set_host_sample_rate_hz(clamp_host_sample_rate_hz(rate));
        Ok(())
    }

    /// Set the host/input sample rate used for the capture/RX resampling path.
    pub fn set_capture_sample_rate_hz(&mut self, rate: u32) -> Result<(), JsValue> {
        if rate == 0 {
            return Err(js_error("rate must be non-zero"));
        }
        self.snd_mut()
            .set_capture_sample_rate_hz(clamp_host_sample_rate_hz(rate));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn host_provided_sample_rates_are_clamped_to_avoid_oom() {
        let mut bridge = VirtioSndPciBridge::new(0x1000, 0).unwrap();

        bridge.set_host_sample_rate_hz(u32::MAX).unwrap();
        assert_eq!(
            bridge.snd_mut().host_sample_rate_hz(),
            aero_audio::MAX_HOST_SAMPLE_RATE_HZ
        );

        bridge.set_capture_sample_rate_hz(u32::MAX).unwrap();
        assert_eq!(
            bridge.snd_mut().capture_sample_rate_hz(),
            aero_audio::MAX_HOST_SAMPLE_RATE_HZ
        );
    }
}
