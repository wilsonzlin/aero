//! WASM-side bridge for exposing a guest-visible virtio-snd device via virtio-pci (modern transport).
//!
//! The browser runtime wires this into the emulated PCI bus and forwards BAR0 MMIO reads/writes into
//! [`VirtioSndPciBridge::mmio_read`] / [`VirtioSndPciBridge::mmio_write`]. The virtqueue structures
//! and DMA buffers live in guest RAM inside the shared WASM linear memory; guest physical address 0
//! maps to `guest_base` (see `guest_ram_layout`).
//!
//! This bridge can optionally enable the virtio-pci legacy I/O port register block (BAR2), either:
//! - as a *transitional* device (legacy + modern), or
//! - as a legacy-only device (legacy BAR2 with modern capabilities disabled).
//!
//! Audio output is delivered to the browser via the canonical AudioWorklet `SharedArrayBuffer` ring
//! buffer (`aero_platform::audio::worklet_bridge::WorkletBridge`). Microphone capture samples are
//! consumed from the canonical mic ring buffer (`aero_platform::audio::mic_bridge::MicBridge`).
use wasm_bindgen::prelude::*;

use js_sys::{SharedArrayBuffer, Uint8Array};

use std::cell::Cell;
use std::rc::Rc;

use aero_audio::capture::AudioCaptureSource;
use aero_audio::sink::AudioSink;

use aero_io_snapshot::io::audio::state::{AudioWorkletRingState, VirtioSndPciState};
use aero_io_snapshot::io::state::IoSnapshot as _;

use aero_platform::audio::mic_bridge::MicBridge;
use aero_platform::audio::worklet_bridge::WorkletBridge;
use aero_platform::interrupts::msi::MsiMessage;

use aero_virtio::devices::snd::{
    JACK_ID_MICROPHONE, JACK_ID_SPEAKER, VIRTIO_SND_QUEUE_EVENT, VirtioSnd,
};
use aero_virtio::memory::{GuestMemory, GuestMemoryError};
use aero_virtio::pci::{InterruptSink, VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, VirtioPciDevice};

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

// Cap open-bus reads/writes so a malicious guest cannot force unbounded work.
const OPEN_BUS_MAX_LEN: usize = 64 * 1024;

struct WasmGuestMemory {
    ram_ptr: *mut u8,
    ram_bytes: u64,
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
            ram_ptr: core::ptr::with_exposed_provenance_mut(guest_base as usize),
            ram_bytes: guest_size_u64,
        })
    }

    #[inline]
    fn ram_ptr_for_range(
        &self,
        paddr: u64,
        ram_offset: u64,
        len: usize,
    ) -> Result<*mut u8, GuestMemoryError> {
        let end = ram_offset
            .checked_add(len as u64)
            .ok_or(GuestMemoryError::OutOfBounds { addr: paddr, len })?;
        if end > self.ram_bytes {
            return Err(GuestMemoryError::OutOfBounds { addr: paddr, len });
        }
        let off = usize::try_from(ram_offset)
            .map_err(|_| GuestMemoryError::OutOfBounds { addr: paddr, len })?;
        // Safety: callers ensure `ram_offset..ram_offset+len` lies within the guest RAM backing store.
        Ok(unsafe { self.ram_ptr.add(off) })
    }

    #[inline]
    fn check_open_bus(paddr: u64, len: usize) -> Result<(), GuestMemoryError> {
        if len > OPEN_BUS_MAX_LEN {
            return Err(GuestMemoryError::OutOfBounds { addr: paddr, len });
        }
        Ok(())
    }
}

impl GuestMemory for WasmGuestMemory {
    fn len(&self) -> u64 {
        guest_ram_phys_end_exclusive(self.ram_bytes)
    }

    fn read(&self, addr: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError> {
        let len = dst.len();
        if len == 0 {
            if addr > self.len() {
                return Err(GuestMemoryError::OutOfBounds { addr, len });
            }
            return Ok(());
        }

        match translate_guest_paddr_range(self.ram_bytes, addr, len) {
            GuestRamRange::Ram { ram_offset } => {
                let ptr = self.ram_ptr_for_range(addr, ram_offset, len)? as *const u8;

                // Shared-memory (threaded wasm) build: atomic byte loads to avoid Rust data-race UB.
                #[cfg(feature = "wasm-threaded")]
                {
                    use core::sync::atomic::{AtomicU8, Ordering};
                    let src = ptr as *const AtomicU8;
                    for (i, slot) in dst.iter_mut().enumerate() {
                        // Safety: we bounds-check the range and `AtomicU8` has alignment 1.
                        *slot = unsafe { (&*src.add(i)).load(Ordering::Relaxed) };
                    }
                }

                #[cfg(not(feature = "wasm-threaded"))]
                unsafe {
                    core::ptr::copy_nonoverlapping(ptr, dst.as_mut_ptr(), len);
                }

                Ok(())
            }
            GuestRamRange::Hole => {
                Self::check_open_bus(addr, len)?;
                dst.fill(0xFF);
                Ok(())
            }
            GuestRamRange::OutOfBounds => Err(GuestMemoryError::OutOfBounds { addr, len }),
        }
    }

    fn write(&mut self, addr: u64, src: &[u8]) -> Result<(), GuestMemoryError> {
        let len = src.len();
        if len == 0 {
            if addr > self.len() {
                return Err(GuestMemoryError::OutOfBounds { addr, len });
            }
            return Ok(());
        }

        match translate_guest_paddr_range(self.ram_bytes, addr, len) {
            GuestRamRange::Ram { ram_offset } => {
                let ptr = self.ram_ptr_for_range(addr, ram_offset, len)?;

                #[cfg(feature = "wasm-threaded")]
                {
                    use core::sync::atomic::{AtomicU8, Ordering};
                    let dst = ptr as *mut AtomicU8;
                    for (i, byte) in src.iter().copied().enumerate() {
                        // Safety: we bounds-check the range and `AtomicU8` has alignment 1.
                        unsafe { (&*dst.add(i)).store(byte, Ordering::Relaxed) };
                    }
                }

                #[cfg(not(feature = "wasm-threaded"))]
                unsafe {
                    core::ptr::copy_nonoverlapping(src.as_ptr(), ptr, len);
                }

                Ok(())
            }
            GuestRamRange::Hole => {
                Self::check_open_bus(addr, len)?;
                Ok(())
            }
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

    fn worklet_ring(&self) -> Option<&WorkletBridge> {
        self.ring.as_ref()
    }

    fn snapshot_ring_state(&self) -> Option<AudioWorkletRingState> {
        self.ring.as_ref().map(|r| r.snapshot_state())
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

    fn discard_buffered_samples_after_restore(&mut self) {
        if let Some(ring) = self.ring.as_mut() {
            ring.discard_buffered_samples();
            ring.reset_dropped_samples_baseline();
        }
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

    fn signal_msix(&mut self, _message: MsiMessage) {
        // MSI-X is not currently surfaced through this bridge.
    }
}

type SndDevice = VirtioSnd<OptionalWorkletSink, OptionalMicCaptureSource>;

#[wasm_bindgen]
pub struct VirtioSndPciBridge {
    mem: WasmGuestMemory,
    dev: VirtioPciDevice,
    irq_asserted: Rc<Cell<bool>>,
    legacy_io_size: u32,
    pci_command: u16,

    pending_audio_ring_state: Option<AudioWorkletRingState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtioSndPciTransportMode {
    ModernOnly,
    Transitional,
    LegacyOnly,
}

impl VirtioSndPciTransportMode {
    fn parse_js(value: Option<JsValue>) -> Result<Self, JsValue> {
        let Some(value) = value else {
            return Ok(Self::ModernOnly);
        };

        // Be permissive: accept both numeric and string modes so JS callers can use enums
        // without needing a specific exported type.
        if value.is_null() || value.is_undefined() {
            return Ok(Self::ModernOnly);
        }

        if let Some(b) = value.as_bool() {
            return Ok(if b {
                Self::Transitional
            } else {
                Self::ModernOnly
            });
        }

        if let Some(n) = value.as_f64() {
            let n = n as i32;
            return match n {
                0 => Ok(Self::ModernOnly),
                1 => Ok(Self::Transitional),
                2 => Ok(Self::LegacyOnly),
                _ => Err(js_error(format!(
                    "invalid virtio-snd pci transport mode: {n}"
                ))),
            };
        }

        if let Some(s) = value.as_string() {
            let s = s.trim().to_ascii_lowercase();
            return match s.as_str() {
                "" | "modern" | "modern-only" | "modern_only" => Ok(Self::ModernOnly),
                "transitional" => Ok(Self::Transitional),
                "legacy" | "legacy-only" | "legacy_only" => Ok(Self::LegacyOnly),
                _ => Err(js_error(format!(
                    "invalid virtio-snd pci transport mode: {s}"
                ))),
            };
        }

        Err(js_error(
            "invalid virtio-snd pci transport mode: expected string or number",
        ))
    }
}

#[wasm_bindgen]
impl VirtioSndPciBridge {
    /// Create a new virtio-snd (virtio-pci modern) bridge bound to the provided guest RAM mapping.
    ///
    /// - `guest_base` is the byte offset inside wasm linear memory where guest physical address 0
    ///   begins (see `guest_ram_layout`).
    /// - `guest_size` is the guest RAM size in bytes. Pass `0` to use "the remainder of linear
    ///   memory" as guest RAM.
    /// - `transport_mode` optionally selects the virtio-pci transport to expose:
    ///   - `"modern"` / `0` (default): modern-only (Aero Win7 virtio contract v1)
    ///   - `"transitional"` / `1`: modern + legacy I/O port BAR
    ///   - `"legacy"` / `2`: legacy I/O port BAR only (modern caps disabled)
    #[wasm_bindgen(constructor)]
    pub fn new(
        guest_base: u32,
        guest_size: u32,
        transport_mode: Option<JsValue>,
    ) -> Result<Self, JsValue> {
        let mem = WasmGuestMemory::new(guest_base, guest_size)?;

        let asserted = Rc::new(Cell::new(false));
        let irq = LegacyIrqLatch {
            asserted: asserted.clone(),
        };

        let output = OptionalWorkletSink::default();
        let capture = OptionalMicCaptureSource::default();
        let snd = VirtioSnd::new_with_capture(output, capture);

        let transport_mode = VirtioSndPciTransportMode::parse_js(transport_mode)?;
        let dev = match transport_mode {
            VirtioSndPciTransportMode::ModernOnly => {
                VirtioPciDevice::new(Box::new(snd), Box::new(irq))
            }
            VirtioSndPciTransportMode::Transitional => {
                VirtioPciDevice::new_transitional(Box::new(snd), Box::new(irq))
            }
            VirtioSndPciTransportMode::LegacyOnly => {
                VirtioPciDevice::new_legacy_only(Box::new(snd), Box::new(irq))
            }
        };
        let legacy_io_size = dev.legacy_io_size().min(u64::from(u32::MAX)) as u32;

        Ok(Self {
            mem,
            dev,
            irq_asserted: asserted,
            legacy_io_size,
            pci_command: 0,
            pending_audio_ring_state: None,
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
        self.dev.set_pci_command(self.pci_command);
    }

    /// Read from the virtio-pci BAR0 MMIO region.
    pub fn mmio_read(&mut self, offset: u32, size: u8) -> u32 {
        let size = validate_mmio_size(size);
        if size == 0 {
            return 0;
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

    /// Read from the legacy virtio-pci (0.9) I/O port register block (BAR2).
    pub fn legacy_io_read(&mut self, offset: u32, size: u8) -> u32 {
        let size = match size {
            1 | 2 | 4 => size as usize,
            _ => return 0,
        };
        let end = offset.saturating_add(size as u32);
        if self.legacy_io_size == 0 || end > self.legacy_io_size {
            return 0xffff_ffff;
        }
        let mut buf = [0u8; 4];
        self.dev.legacy_io_read(offset as u64, &mut buf[..size]);
        u32::from_le_bytes(buf)
    }

    /// Write to the legacy virtio-pci (0.9) I/O port register block (BAR2).
    pub fn legacy_io_write(&mut self, offset: u32, size: u8, value: u32) {
        let size = match size {
            1 | 2 | 4 => size as usize,
            _ => return,
        };
        let end = offset.saturating_add(size as u32);
        if self.legacy_io_size == 0 || end > self.legacy_io_size {
            return;
        }
        let bytes = value.to_le_bytes();
        self.dev.legacy_io_write(offset as u64, &bytes[..size]);

        // Legacy queue notifications are expected to be "immediate" from the guest's perspective
        // (in real hardware, the kick causes the device to begin DMA). In the browser runtime we
        // have access to guest RAM in the WASM linear memory, so we can service the notified
        // virtqueue synchronously instead of requiring periodic polling.
        if offset as u64 == VIRTIO_PCI_LEGACY_QUEUE_NOTIFY && self.bus_master_enabled() {
            self.dev.process_notified_queues(&mut self.mem);
        }
    }

    /// Back-compat alias for `legacy_io_read` (mirrors `VirtioNetPciBridge`).
    pub fn io_read(&mut self, offset: u32, size: u8) -> u32 {
        if size == 0 {
            return 0;
        }
        if !matches!(size, 1 | 2 | 4) {
            return 0xffff_ffff;
        }
        self.legacy_io_read(offset, size)
    }

    /// Back-compat alias for `legacy_io_write` (mirrors `VirtioNetPciBridge`).
    pub fn io_write(&mut self, offset: u32, size: u8, value: u32) {
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        self.legacy_io_write(offset, size, value);
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
        self.dev.driver_ok()
    }

    /// Whether the PCI INTx line should be asserted.
    pub fn irq_asserted(&self) -> bool {
        self.irq_asserted.get()
    }

    /// If an AudioWorklet ring is attached, returns its current buffered level (frames).
    ///
    /// Returns 0 if no ring is attached.
    pub fn buffer_level_frames(&mut self) -> u32 {
        self.snd_mut()
            .output_mut()
            .worklet_ring()
            .map(|r| r.buffer_level_frames())
            .unwrap_or(0)
    }

    /// If an AudioWorklet ring is attached, returns its total consumer underrun counter (missing frames).
    ///
    /// Returns 0 if no ring is attached.
    pub fn underrun_count(&mut self) -> u32 {
        self.snd_mut()
            .output_mut()
            .worklet_ring()
            .map(|r| r.underrun_count())
            .unwrap_or(0)
    }

    /// If an AudioWorklet ring is attached, returns its total producer overrun counter (frames dropped).
    ///
    /// Returns 0 if no ring is attached.
    pub fn overrun_count(&mut self) -> u32 {
        self.snd_mut()
            .output_mut()
            .worklet_ring()
            .map(|r| r.overrun_count())
            .unwrap_or(0)
    }

    /// Attach the AudioWorklet output ring buffer (producer side; AudioWorklet is the consumer).
    pub fn attach_audio_ring(
        &mut self,
        ring_sab: SharedArrayBuffer,
        capacity_frames: u32,
        channel_count: u32,
    ) -> Result<(), JsValue> {
        // Avoid borrow conflicts: take pending state before borrowing `self.dev` via `snd_mut()`.
        let mut pending_state = self.pending_audio_ring_state.take();

        {
            let snd = self.snd_mut();
            {
                let output = snd.output_mut();
                output.attach(ring_sab, capacity_frames, channel_count)?;

                // Apply a deferred ring restore if `load_state` was called before the host reattached
                // the AudioWorklet ring.
                if let Some(mut state) = pending_state.take() {
                    if let Some(ring) = output.worklet_ring() {
                        // Snapshot state stores the ring capacity for determinism/debugging, but
                        // restores must be best-effort. If the host allocates a different capacity than
                        // what was captured in the snapshot, clear the field so
                        // `WorkletBridge::restore_state` bypasses its debug-only capacity assertion.
                        if state.capacity_frames != 0
                            && state.capacity_frames != ring.capacity_frames()
                        {
                            state.capacity_frames = 0;
                        }
                        ring.restore_state(&state);
                    } else {
                        pending_state = Some(state);
                    }
                }
            }

            // Reflect whether the host has attached an output ring buffer: this maps to the
            // speaker jack in the Win7 topology miniport.
            snd.queue_jack_event(JACK_ID_SPEAKER, true);
        }

        self.pending_audio_ring_state = pending_state;
        Ok(())
    }

    pub fn detach_audio_ring(&mut self) {
        let snd = self.snd_mut();
        snd.output_mut().detach();
        snd.queue_jack_event(JACK_ID_SPEAKER, false);
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
        let snd = self.snd_mut();
        match ring_sab {
            Some(sab) => {
                snd.capture_source_mut().attach(sab)?;
                snd.queue_jack_event(JACK_ID_MICROPHONE, true);
                Ok(())
            }
            None => {
                snd.capture_source_mut().detach();
                snd.queue_jack_event(JACK_ID_MICROPHONE, false);
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

    /// Serialize the current virtio-snd PCI function state into a deterministic snapshot blob.
    ///
    /// This includes:
    /// - virtio-pci transport state (`VPCI`)
    /// - virtio-snd internal state (stream state + host sample rates)
    /// - AudioWorklet ring indices (but not audio contents)
    pub fn save_state(&mut self) -> Vec<u8> {
        let virtio_pci = self.dev.save_state();

        let snd_state;
        let attached_ring_state;
        {
            let snd = self.snd_mut();
            snd_state = snd.snapshot_state();
            attached_ring_state = snd.output_mut().snapshot_ring_state();
        }

        let ring_state = if let Some(state) = attached_ring_state {
            state
        } else if let Some(state) = self.pending_audio_ring_state.as_ref() {
            state.clone()
        } else {
            AudioWorkletRingState {
                capacity_frames: 0,
                write_pos: 0,
                read_pos: 0,
            }
        };

        let state = VirtioSndPciState {
            virtio_pci,
            snd: snd_state,
            worklet_ring: ring_state,
        };
        state.save_state()
    }

    /// Restore virtio-snd PCI function state from a snapshot blob produced by [`save_state`].
    pub fn load_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        let mut state = VirtioSndPciState::default();
        state
            .load_state(bytes)
            .map_err(|e| js_error(format!("Invalid virtio-snd snapshot: {e}")))?;

        self.dev
            .load_state(&state.virtio_pci)
            .map_err(|e| js_error(format!("Invalid virtio-pci snapshot: {e}")))?;

        // Mirror the restored PCI command register into the wrapper field so DMA gating stays
        // consistent immediately after restore (even when the surrounding PCI bus is implemented
        // outside of this wrapper).
        let mut cmd_bytes = [0u8; 2];
        self.dev.config_read(0x04, &mut cmd_bytes);
        self.pci_command = u16::from_le_bytes(cmd_bytes);

        // Restore virtio-snd internal state and clear any cached eventq buffers/events (they are
        // runtime-only and are not serialized).
        self.snd_mut().restore_state(&state.snd);

        // The virtio-snd event queue may have had guest-posted buffers popped and cached by the
        // device without producing used entries. Those cached buffers are not serialized; rewind
        // queue progress so the transport will re-pop them post-restore.
        self.dev
            .rewind_queue_next_avail_to_next_used(VIRTIO_SND_QUEUE_EVENT);

        // Microphone input samples are not serialized in the snapshot; discard any host-buffered
        // samples so capture resumes from the most recent audio.
        self.snd_mut()
            .capture_source_mut()
            .discard_buffered_samples_after_restore();

        // Restore (or defer restoring) the AudioWorklet output ring indices.
        let ring_state = state.worklet_ring;
        let mut pending_state = Some(ring_state);
        {
            let output = self.snd_mut().output_mut();
            if let Some(ring) = output.worklet_ring()
                && let Some(mut ring_state) = pending_state.take()
            {
                if ring_state.capacity_frames != 0
                    && ring_state.capacity_frames != ring.capacity_frames()
                {
                    ring_state.capacity_frames = 0;
                }
                ring.restore_state(&ring_state);
            }
        }
        self.pending_audio_ring_state = pending_state;

        Ok(())
    }

    /// Snapshot the full device state as deterministic bytes.
    pub fn snapshot_state(&mut self) -> Uint8Array {
        Uint8Array::from(self.save_state().as_slice())
    }

    /// Restore device state from deterministic snapshot bytes.
    pub fn restore_state(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.load_state(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use aero_io_snapshot::io::state::SnapshotReader;
    use aero_io_snapshot::io::virtio::state::VirtioPciTransportState;
    use aero_platform::audio::worklet_bridge::WorkletBridge;
    use aero_virtio::devices::snd::VIRTIO_SND_R_PCM_INFO;
    use aero_virtio::memory::{read_u16_le, write_u16_le, write_u32_le, write_u64_le};
    use aero_virtio::pci::{
        VIRTIO_F_RING_INDIRECT_DESC, VIRTIO_F_VERSION_1, VIRTIO_STATUS_ACKNOWLEDGE,
        VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FEATURES_OK,
    };
    use aero_virtio::queue::{VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE};
    use wasm_bindgen_test::wasm_bindgen_test;

    fn write_desc(
        mem: &mut dyn GuestMemory,
        table: u64,
        index: u16,
        addr: u64,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let base = table + u64::from(index) * 16;
        write_u64_le(mem, base, addr).unwrap();
        write_u32_le(mem, base + 8, len).unwrap();
        write_u16_le(mem, base + 12, flags).unwrap();
        write_u16_le(mem, base + 14, next).unwrap();
    }

    #[wasm_bindgen_test]
    fn host_provided_sample_rates_are_clamped_to_avoid_oom() {
        let mut bridge = VirtioSndPciBridge::new(0x1000, 0, None).unwrap();

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

    #[wasm_bindgen_test]
    fn audio_ring_metrics_return_zero_when_unattached() {
        let mut bridge = VirtioSndPciBridge::new(0x1000, 0, None).unwrap();

        assert_eq!(bridge.buffer_level_frames(), 0);
        assert_eq!(bridge.underrun_count(), 0);
        assert_eq!(bridge.overrun_count(), 0);
    }

    #[wasm_bindgen_test]
    fn bus_master_enable_gates_queue_dma_and_irqs() {
        // Back guest RAM with an isolated heap allocation so we can safely assert on memory
        // mutation.
        let mut guest = vec![0u8; 0x8000];
        let guest_base = guest.as_mut_ptr() as u32;
        let guest_size = guest.len() as u32;

        let mut bridge = VirtioSndPciBridge::new(guest_base, guest_size, None).unwrap();

        // Enable BAR0 MMIO decoding (PCI COMMAND.MEM) but leave Bus Master Enable clear.
        bridge.set_pci_command(1u32 << 1);

        // Configure virtqueue 0 (control queue) with a tiny "PCM_INFO" request so processing the
        // queue will write a response into guest memory and produce an interrupt.
        let desc_table = 0x1000u64;
        let avail = 0x2000u64;
        let used = 0x3000u64;
        let req = 0x4000u64;
        let resp = 0x5000u64;

        // Descriptor table: [request (out)] -> [response (in)].
        write_desc(
            &mut bridge.mem,
            desc_table,
            0,
            req,
            12,
            VIRTQ_DESC_F_NEXT,
            1,
        );
        write_desc(
            &mut bridge.mem,
            desc_table,
            1,
            resp,
            64,
            VIRTQ_DESC_F_WRITE,
            0,
        );

        // Request payload.
        let mut request = Vec::new();
        request.extend_from_slice(&VIRTIO_SND_R_PCM_INFO.to_le_bytes());
        request.extend_from_slice(&0u32.to_le_bytes()); // start_id
        request.extend_from_slice(&1u32.to_le_bytes()); // count
        bridge.mem.write(req, &request).unwrap();

        // Initialize avail/used rings.
        write_u16_le(&mut bridge.mem, avail, 0).unwrap(); // avail.flags (interrupts enabled)
        write_u16_le(&mut bridge.mem, avail + 2, 1).unwrap(); // avail.idx
        write_u16_le(&mut bridge.mem, avail + 4, 0).unwrap(); // avail.ring[0] = head desc index

        write_u16_le(&mut bridge.mem, used, 0).unwrap(); // used.flags
        write_u16_le(&mut bridge.mem, used + 2, 0).unwrap(); // used.idx

        // Seed the response buffer with a sentinel so we can detect DMA writes.
        bridge.mem.write(resp, &[0xAA; 64]).unwrap();

        // Program queue addresses + enable via the virtio-pci common config region.
        bridge.mmio_write(0x16, 2, 0); // queue_select = 0
        bridge.mmio_write(0x20, 4, desc_table as u32);
        bridge.mmio_write(0x24, 4, (desc_table >> 32) as u32);
        bridge.mmio_write(0x28, 4, avail as u32);
        bridge.mmio_write(0x2c, 4, (avail >> 32) as u32);
        bridge.mmio_write(0x30, 4, used as u32);
        bridge.mmio_write(0x34, 4, (used >> 32) as u32);
        bridge.mmio_write(0x1c, 2, 1); // queue_enable = 1

        // Kick queue 0 via the notify region (BAR0 + 0x1000). With BME clear, this must *not*
        // perform DMA or assert IRQ.
        bridge.mmio_write(0x1000, 4, 0);
        assert!(
            !bridge.irq_asserted(),
            "IRQ should be gated when BME is clear"
        );
        assert_eq!(
            read_u16_le(&bridge.mem, used + 2).unwrap(),
            0,
            "used.idx must not advance when BME is clear"
        );
        let mut resp_hdr = [0u8; 4];
        bridge.mem.read(resp, &mut resp_hdr).unwrap();
        assert_eq!(
            resp_hdr, [0xAA; 4],
            "response buffer must not be written when BME is clear"
        );

        // `poll()` must also be gated by PCI Bus Master Enable. A JS runtime may call `poll()` even
        // before the guest enables DMA; ensure it cannot touch guest memory in that state.
        bridge.poll();
        assert!(
            !bridge.irq_asserted(),
            "IRQ should remain gated when BME is clear"
        );
        assert_eq!(
            read_u16_le(&bridge.mem, used + 2).unwrap(),
            0,
            "used.idx must not advance when polling with BME clear"
        );
        bridge.mem.read(resp, &mut resp_hdr).unwrap();
        assert_eq!(
            resp_hdr, [0xAA; 4],
            "response buffer must not be written when polling with BME clear"
        );

        // Now enable Bus Master and kick again. The device should DMA the response and update the
        // used ring, producing an interrupt.
        bridge.set_pci_command((1u32 << 1) | (1u32 << 2));
        bridge.mmio_write(0x1000, 4, 0);

        assert!(
            bridge.irq_asserted(),
            "IRQ should assert once DMA is permitted"
        );
        assert_eq!(read_u16_le(&bridge.mem, used + 2).unwrap(), 1);
        bridge.mem.read(resp, &mut resp_hdr).unwrap();
        assert_eq!(
            resp_hdr,
            [0, 0, 0, 0],
            "response header should contain VIRTIO_SND_S_OK"
        );

        drop(guest);
    }

    #[wasm_bindgen_test]
    fn snapshot_roundtrip_is_deterministic() {
        let mut guest1 = vec![0u8; 0x4000];
        let guest_base1 = guest1.as_mut_ptr() as u32;
        let guest_size1 = guest1.len() as u32;

        let mut bridge1 = VirtioSndPciBridge::new(guest_base1, guest_size1, None).unwrap();
        bridge1.set_pci_command(1u32 << 1); // enable MMIO decode so common_cfg writes stick

        // Mutate virtio-snd state.
        bridge1.set_host_sample_rate_hz(44_100).unwrap();
        bridge1.set_capture_sample_rate_hz(48_000).unwrap();

        // Mutate virtio-pci transport state: negotiate a minimal feature set and set DRIVER_OK.
        // driver_feature_select = 1 (high 32 bits)
        bridge1.mmio_write(0x08, 4, 1);
        // driver_features[63:32] = 1 => VIRTIO_F_VERSION_1 (bit 32)
        bridge1.mmio_write(0x0c, 4, 1);

        // Also opt into indirect descriptors to exercise the low feature page.
        bridge1.mmio_write(0x08, 4, 0);
        bridge1.mmio_write(0x0c, 4, VIRTIO_F_RING_INDIRECT_DESC as u32);

        let status = VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK;
        bridge1.mmio_write(0x14, 1, status as u32);

        let snap1 = bridge1.save_state();

        let mut guest2 = vec![0u8; 0x4000];
        let guest_base2 = guest2.as_mut_ptr() as u32;
        let guest_size2 = guest2.len() as u32;

        let mut bridge2 = VirtioSndPciBridge::new(guest_base2, guest_size2, None).unwrap();
        bridge2.load_state(&snap1).unwrap();
        let snap2 = bridge2.save_state();

        assert_eq!(
            snap1, snap2,
            "save_state -> load_state -> save_state must be stable"
        );

        // Sanity-check the decoded snapshot contents.
        let mut decoded = VirtioSndPciState::default();
        decoded.load_state(&snap2).unwrap();

        assert!(
            decoded.snd.host_sample_rate_hz == 44_100
                && decoded.snd.capture_sample_rate_hz == 48_000,
            "expected host/capture sample rates to survive snapshot restore"
        );

        // Verify that the embedded virtio-pci snapshot advertises DRIVER_OK and negotiated
        // `VIRTIO_F_VERSION_1`.
        let vpci = SnapshotReader::parse(&decoded.virtio_pci, *b"VPCI").unwrap();
        let transport_bytes = vpci.bytes(2).expect("virtio-pci transport field");
        let transport = VirtioPciTransportState::decode(transport_bytes).unwrap();
        assert_ne!(
            transport.device_status & VIRTIO_STATUS_DRIVER_OK,
            0,
            "expected DRIVER_OK to survive snapshot restore"
        );
        assert_ne!(
            transport.negotiated_features & VIRTIO_F_VERSION_1,
            0,
            "expected VIRTIO_F_VERSION_1 to be negotiated"
        );

        drop(guest1);
        drop(guest2);
    }

    #[wasm_bindgen_test]
    fn snapshot_roundtrip_restores_sample_rates_and_worklet_ring_state_when_attached() {
        let capacity_frames = 256;
        let channel_count = 2;

        let mut guest1 = vec![0u8; 0x4000];
        let guest_base1 = guest1.as_mut_ptr() as u32;
        let guest_size1 = guest1.len() as u32;
        let mut bridge1 = VirtioSndPciBridge::new(guest_base1, guest_size1, None).unwrap();

        // Mutate virtio-snd state so the snapshot has meaningful internal fields.
        bridge1.set_host_sample_rate_hz(96_000).unwrap();
        bridge1.set_capture_sample_rate_hz(44_100).unwrap();

        // Attach a worklet ring and seed it with non-trivial indices.
        let ring1 = WorkletBridge::new(capacity_frames, channel_count).unwrap();
        let sab1 = ring1.shared_buffer();
        bridge1
            .attach_audio_ring(sab1.clone(), capacity_frames, channel_count)
            .unwrap();

        let expected_ring_state = AudioWorkletRingState {
            capacity_frames,
            read_pos: 7,
            write_pos: 42,
        };
        ring1.restore_state(&expected_ring_state);
        assert_eq!(bridge1.buffer_level_frames(), 35);

        let snap = bridge1.save_state();

        let mut guest2 = vec![0u8; 0x4000];
        let guest_base2 = guest2.as_mut_ptr() as u32;
        let guest_size2 = guest2.len() as u32;
        let mut bridge2 = VirtioSndPciBridge::new(guest_base2, guest_size2, None).unwrap();

        let ring2 = WorkletBridge::new(capacity_frames, channel_count).unwrap();
        let sab2 = ring2.shared_buffer();
        bridge2
            .attach_audio_ring(sab2.clone(), capacity_frames, channel_count)
            .unwrap();

        bridge2.load_state(&snap).unwrap();

        assert_eq!(bridge2.snd_mut().host_sample_rate_hz(), 96_000);
        assert_eq!(bridge2.snd_mut().capture_sample_rate_hz(), 44_100);
        assert_eq!(ring2.snapshot_state(), expected_ring_state);
        assert_eq!(bridge2.buffer_level_frames(), 35);
        assert!(
            bridge2.pending_audio_ring_state.is_none(),
            "load_state should apply ring state immediately when a ring is attached"
        );

        drop(guest1);
        drop(guest2);
    }

    #[wasm_bindgen_test]
    fn deferred_worklet_ring_restore_is_applied_on_attach() {
        let capacity_frames = 8;
        let channel_count = 2;

        // Create a snapshot with a non-trivial ring state.
        let mut guest1 = vec![0u8; 0x4000];
        let guest_base1 = guest1.as_mut_ptr() as u32;
        let guest_size1 = guest1.len() as u32;
        let mut bridge1 = VirtioSndPciBridge::new(guest_base1, guest_size1, None).unwrap();

        let ring = WorkletBridge::new(capacity_frames, channel_count).unwrap();
        let sab = ring.shared_buffer();
        bridge1
            .attach_audio_ring(sab.clone(), capacity_frames, channel_count)
            .unwrap();

        let expected = AudioWorkletRingState {
            capacity_frames,
            read_pos: 2,
            write_pos: 6,
        };
        ring.restore_state(&expected);

        let snap = bridge1.save_state();

        // Restore into a fresh bridge before attaching any AudioWorklet ring. This should stash the
        // ring state in `pending_audio_ring_state`.
        let mut guest2 = vec![0u8; 0x4000];
        let guest_base2 = guest2.as_mut_ptr() as u32;
        let guest_size2 = guest2.len() as u32;
        let mut bridge2 = VirtioSndPciBridge::new(guest_base2, guest_size2, None).unwrap();
        bridge2.load_state(&snap).unwrap();

        assert_eq!(
            bridge2.pending_audio_ring_state.as_ref(),
            Some(&expected),
            "load_state should retain worklet ring indices when no ring is attached"
        );

        // Corrupt the ring indices so we can observe the deferred restore when attaching.
        ring.restore_state(&AudioWorkletRingState {
            capacity_frames,
            read_pos: 123,
            write_pos: 125,
        });
        assert_ne!(ring.snapshot_state(), expected);

        bridge2
            .attach_audio_ring(sab, capacity_frames, channel_count)
            .unwrap();

        assert_eq!(ring.snapshot_state(), expected);
        assert!(
            bridge2.pending_audio_ring_state.is_none(),
            "pending state should be consumed once applied"
        );

        // ---- Capacity mismatch path ----
        // If the host re-attaches a ring with a different capacity than the snapshot captured,
        // restores should be best-effort and must not panic. The bridge clears the snapshot's
        // `capacity_frames` field before calling `WorkletBridge::restore_state` so the ring indices
        // are restored using the *actual* ring capacity (rather than clamping to the snapshot
        // capacity).
        let mismatch_ring_state = AudioWorkletRingState {
            capacity_frames: 16,
            read_pos: 0,
            write_pos: 20,
        };

        let virtio_pci = bridge2.dev.save_state();
        let snd_state = bridge2.snd_mut().snapshot_state();
        let mismatch_snapshot = VirtioSndPciState {
            virtio_pci,
            snd: snd_state,
            worklet_ring: mismatch_ring_state.clone(),
        };
        let mismatch_bytes = mismatch_snapshot.save_state();

        let mut guest3 = vec![0u8; 0x4000];
        let guest_base3 = guest3.as_mut_ptr() as u32;
        let guest_size3 = guest3.len() as u32;
        let mut bridge3 = VirtioSndPciBridge::new(guest_base3, guest_size3, None).unwrap();
        bridge3.load_state(&mismatch_bytes).unwrap();

        assert_eq!(
            bridge3.pending_audio_ring_state.as_ref(),
            Some(&mismatch_ring_state),
            "load_state should stash ring state when no ring is attached"
        );

        let larger_capacity = 32;
        let ring3 = WorkletBridge::new(larger_capacity, channel_count).unwrap();
        let sab3 = ring3.shared_buffer();
        bridge3
            .attach_audio_ring(sab3, larger_capacity, channel_count)
            .unwrap();

        let got = ring3.snapshot_state();
        assert_eq!(got.capacity_frames, larger_capacity);
        assert_eq!(got.read_pos, 0);
        assert_eq!(got.write_pos, 20);
        assert_eq!(
            bridge3.buffer_level_frames(),
            20,
            "mismatched restore should use the attached ring capacity (best-effort)"
        );
        assert!(
            bridge3.pending_audio_ring_state.is_none(),
            "pending state should be consumed once applied"
        );

        drop(guest1);
        drop(guest2);
    }
}
