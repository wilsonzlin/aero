//! WASM bridge for exposing `aero-virtio`'s virtio-input device model via the modern virtio-pci
//! transport.
//!
//! The browser runtime uses this wrapper as the backing implementation for a PCI function. JS
//! forwards BAR0 MMIO accesses into [`VirtioInputPciDevice::mmio_read`] /
//! [`VirtioInputPciDevice::mmio_write`], and the wrapper reads/writes virtqueue structures directly
//! from the shared guest RAM region inside the WASM linear memory.

use aero_platform::interrupts::msi::MsiMessage;
use aero_virtio::devices::input::{VirtioInput, VirtioInputDeviceKind};
use aero_virtio::memory::GuestMemory;
use aero_virtio::pci::{
    InterruptSink, VIRTIO_PCI_LEGACY_QUEUE_NOTIFY, VirtioPciDevice,
};
use std::cell::Cell;
use std::rc::Rc;

fn validate_mmio_size(size: u8) -> usize {
    match size {
        1 | 2 | 4 => size as usize,
        _ => 0,
    }
}

#[derive(Clone)]
struct InterruptState {
    asserted: Rc<Cell<bool>>,
    #[allow(dead_code)]
    raise_count: Rc<Cell<u64>>,
    #[allow(dead_code)]
    msix_count: Rc<Cell<u64>>,
}

impl InterruptState {
    fn new() -> (Self, InterruptTracker) {
        let asserted = Rc::new(Cell::new(false));
        let raise_count = Rc::new(Cell::new(0));
        let msix_count = Rc::new(Cell::new(0));
        let state = Self {
            asserted: asserted.clone(),
            raise_count: raise_count.clone(),
            msix_count: msix_count.clone(),
        };
        let sink = InterruptTracker {
            asserted,
            raise_count,
            msix_count,
        };
        (state, sink)
    }

    fn asserted(&self) -> bool {
        self.asserted.get()
    }
}

struct InterruptTracker {
    asserted: Rc<Cell<bool>>,
    raise_count: Rc<Cell<u64>>,
    msix_count: Rc<Cell<u64>>,
}

impl InterruptSink for InterruptTracker {
    fn raise_legacy_irq(&mut self) {
        self.raise_count
            .set(self.raise_count.get().saturating_add(1));
        self.asserted.set(true);
    }

    fn lower_legacy_irq(&mut self) {
        self.asserted.set(false);
    }

    fn signal_msix(&mut self, _message: MsiMessage) {
        // The web runtime currently wires up INTx; keep basic accounting for observability.
        self.msix_count.set(self.msix_count.get().saturating_add(1));
    }
}

#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtioPciTransportMode {
    ModernOnly,
    Transitional,
    LegacyOnly,
}

/// Rust-native wrapper around a virtio-input device exposed via virtio-pci (modern transport).
///
/// This type is used by the wasm-exported [`VirtioInputPciDevice`] and is also exercised by native
/// unit tests in this crate.
pub struct VirtioInputPciDeviceCore {
    kind: VirtioInputDeviceKind,
    pci: VirtioPciDevice,
    irq: InterruptState,
    pci_command: u16,
    legacy_io_size: u32,
}

impl VirtioInputPciDeviceCore {
    pub fn new(kind: VirtioInputDeviceKind) -> Self {
        Self::new_with_transport(kind, VirtioPciTransportMode::ModernOnly)
    }

    fn new_with_transport(kind: VirtioInputDeviceKind, transport: VirtioPciTransportMode) -> Self {
        let (irq, sink) = InterruptState::new();
        let input = VirtioInput::new(kind);
        let pci = match transport {
            VirtioPciTransportMode::ModernOnly => {
                VirtioPciDevice::new(Box::new(input), Box::new(sink))
            }
            VirtioPciTransportMode::Transitional => {
                VirtioPciDevice::new_transitional(Box::new(input), Box::new(sink))
            }
            VirtioPciTransportMode::LegacyOnly => {
                VirtioPciDevice::new_legacy_only(Box::new(input), Box::new(sink))
            }
        };
        let legacy_io_size = pci.legacy_io_size().min(u64::from(u32::MAX)) as u32;
        Self {
            kind,
            pci,
            irq,
            pci_command: 0,
            legacy_io_size,
        }
    }

    fn bus_master_enabled(&self) -> bool {
        (self.pci_command & (1 << 2)) != 0
    }

    /// Mirror the guest-written PCI command register (0x04, low 16 bits) into this wrapper.
    ///
    /// This is used to enforce PCI Bus Master Enable gating for DMA.
    pub fn set_pci_command(&mut self, command: u32) {
        self.pci_command = (command & 0xffff) as u16;
        self.pci.set_pci_command(self.pci_command);
    }

    fn input_mut(&mut self) -> &mut VirtioInput {
        self.pci
            .device_mut::<VirtioInput>()
            .expect("VirtioPciDevice should contain a VirtioInput device")
    }

    pub fn mmio_read(&mut self, offset: u64, size: u8) -> u32 {
        let size = validate_mmio_size(size);
        if size == 0 {
            return 0;
        }

        let mut buf = [0u8; 4];
        self.pci.bar0_read(offset, &mut buf[..size]);
        match size {
            1 => u32::from(buf[0]),
            2 => u32::from(u16::from_le_bytes([buf[0], buf[1]])),
            4 => u32::from_le_bytes(buf),
            _ => unreachable!(),
        }
    }

    pub fn mmio_write(&mut self, offset: u64, size: u8, value: u32, mem: &mut dyn GuestMemory) {
        let size = validate_mmio_size(size);
        if size == 0 {
            return;
        }
        let bytes = value.to_le_bytes();
        self.pci.bar0_write(offset, &bytes[..size]);
        // The virtio-pci notify region is write-only and records pending queue notifications.
        // In the browser/WASM integration we have direct access to guest RAM, so process notified
        // queues immediately (so buffers posted during driver init are consumed even if the host
        // does not call `poll()` until later).
        //
        // Only DMA when PCI Bus Master Enable is set (command bit 2).
        if self.bus_master_enabled() {
            self.pci.process_notified_queues(mem);
        }
    }

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
        self.pci.legacy_io_read(offset as u64, &mut buf[..size]);
        u32::from_le_bytes(buf)
    }

    pub fn legacy_io_write(
        &mut self,
        offset: u32,
        size: u8,
        value: u32,
        mem: &mut dyn GuestMemory,
    ) {
        let size = match size {
            1 | 2 | 4 => size as usize,
            _ => return,
        };
        let end = offset.saturating_add(size as u32);
        if self.legacy_io_size == 0 || end > self.legacy_io_size {
            return;
        }
        let bytes = value.to_le_bytes();
        self.pci.legacy_io_write(offset as u64, &bytes[..size]);

        // Legacy queue notifications are expected to be "immediate" from the guest's
        // perspective (in real hardware, the kick causes the device to begin DMA). When we have
        // direct access to guest RAM, service the notified virtqueue synchronously.
        if offset as u64 == VIRTIO_PCI_LEGACY_QUEUE_NOTIFY && self.bus_master_enabled() {
            self.pci.process_notified_queues(mem);
        }
    }

    pub fn poll(&mut self, mem: &mut dyn GuestMemory) {
        // Only DMA when PCI Bus Master Enable is set (command bit 2).
        if !self.bus_master_enabled() {
            return;
        }
        self.pci.poll(mem);
    }

    pub fn driver_ok(&mut self) -> bool {
        self.pci.driver_ok()
    }

    pub fn irq_asserted(&self) -> bool {
        self.irq.asserted()
    }

    pub fn inject_key(&mut self, linux_key: u16, pressed: bool, mem: &mut dyn GuestMemory) {
        if self.kind != VirtioInputDeviceKind::Keyboard {
            return;
        }
        self.input_mut().inject_key(linux_key, pressed);
        self.poll(mem);
    }

    pub fn inject_rel(&mut self, dx: i32, dy: i32, mem: &mut dyn GuestMemory) {
        if self.kind != VirtioInputDeviceKind::Mouse {
            return;
        }
        self.input_mut().inject_rel_move(dx, dy);
        self.poll(mem);
    }

    pub fn inject_button(&mut self, btn: u16, pressed: bool, mem: &mut dyn GuestMemory) {
        if self.kind != VirtioInputDeviceKind::Mouse {
            return;
        }
        self.input_mut().inject_button(btn, pressed);
        self.poll(mem);
    }

    pub fn inject_wheel(&mut self, delta: i32, mem: &mut dyn GuestMemory) {
        if self.kind != VirtioInputDeviceKind::Mouse {
            return;
        }
        self.input_mut().inject_wheel(delta);
        self.poll(mem);
    }
}

// -------------------------------------------------------------------------------------------------
// WASM export
// -------------------------------------------------------------------------------------------------

#[cfg(any(target_arch = "wasm32", test))]
mod wasm_guest_memory {
    use aero_virtio::memory::{GuestMemory as VirtioGuestMemory, GuestMemoryError};

    use crate::guest_phys::{
        GuestRamRange, guest_ram_phys_end_exclusive, translate_guest_paddr_range,
    };

    // Cap open-bus slices so a malicious guest cannot force unbounded allocations.
    const OPEN_BUS_SLICE_MAX: usize = 64 * 1024;
    static OPEN_BUS_BYTES: [u8; OPEN_BUS_SLICE_MAX] = [0xFF; OPEN_BUS_SLICE_MAX];

    pub(super) struct WasmGuestMemory {
        /// Pointer to the start of the *mapped* RAM window.
        ram_ptr: *mut u8,
        /// Byte offset within the contiguous guest RAM backing store that corresponds to `ram_ptr`.
        ram_offset_base: u64,
        /// Length (in bytes) of the mapped RAM window.
        ram_window_len: u64,
        /// Total guest RAM size in bytes (contiguous backing store length).
        ram_bytes: u64,
        /// Scratch sink for open-bus writes (writes must not affect future reads).
        open_bus_write: Vec<u8>,
    }

    #[cfg(target_arch = "wasm32")]
    fn wasm_memory_byte_len() -> u64 {
        let pages = core::arch::wasm32::memory_size(0) as u64;
        pages.saturating_mul(64 * 1024)
    }

    #[cfg(target_arch = "wasm32")]
    fn js_error(message: impl core::fmt::Display) -> wasm_bindgen::JsValue {
        js_sys::Error::new(&message.to_string()).into()
    }

    impl WasmGuestMemory {
        #[cfg(target_arch = "wasm32")]
        pub(super) fn new(guest_base: u32, guest_size: u32) -> Result<Self, wasm_bindgen::JsValue> {
            if guest_base == 0 {
                return Err(js_error("guestBase must be non-zero"));
            }

            let mem_len = wasm_memory_byte_len();
            let guest_base_u64 = u64::from(guest_base);
            // Allow `guest_base == wasm_mem_len` when `guest_size == 0` (empty guest RAM) to match
            // the `guest_ram_layout` contract used by other WASM bridges.
            if guest_base_u64 > mem_len {
                return Err(js_error(format!(
                    "Guest RAM mapping out of bounds: guest_base=0x{guest_base:x} wasm_mem=0x{mem_len:x}"
                )));
            }

            // Treat `guest_size=0` as "use the remainder of linear memory".
            let guest_size_u64 = if guest_size == 0 {
                mem_len - guest_base_u64
            } else {
                u64::from(guest_size)
            };
            // Keep guest RAM below the PCI MMIO BAR window (see `guest_ram_layout` contract).
            let guest_size_u64 = guest_size_u64.min(crate::guest_layout::PCI_MMIO_BASE);

            let end = guest_base_u64
                .checked_add(guest_size_u64)
                .ok_or_else(|| js_error("guestBase + guestSize overflow"))?;
            if end > mem_len {
                return Err(js_error(format!(
                    "Guest RAM mapping out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size_u64:x} end=0x{end:x} wasm_mem=0x{mem_len:x}"
                )));
            }

            Ok(Self {
                ram_ptr: guest_base as *mut u8,
                ram_offset_base: 0,
                ram_window_len: guest_size_u64,
                ram_bytes: guest_size_u64,
                open_bus_write: Vec::new(),
            })
        }

        #[cfg(test)]
        pub(super) fn new_for_test(
            ram_bytes: u64,
            ram_offset_base: u64,
            backing: &mut [u8],
        ) -> Self {
            Self {
                ram_ptr: backing.as_mut_ptr(),
                ram_offset_base,
                ram_window_len: backing.len() as u64,
                ram_bytes,
                open_bus_write: Vec::new(),
            }
        }

        #[inline]
        fn ram_slice(
            &self,
            paddr: u64,
            ram_offset: u64,
            len: usize,
        ) -> Result<&[u8], GuestMemoryError> {
            let end = ram_offset
                .checked_add(len as u64)
                .ok_or(GuestMemoryError::OutOfBounds { addr: paddr, len })?;
            let window_end = self
                .ram_offset_base
                .checked_add(self.ram_window_len)
                .ok_or(GuestMemoryError::OutOfBounds { addr: paddr, len })?;
            if ram_offset < self.ram_offset_base || end > window_end {
                return Err(GuestMemoryError::OutOfBounds { addr: paddr, len });
            }
            let rel = ram_offset - self.ram_offset_base;
            let rel_usize = usize::try_from(rel)
                .map_err(|_| GuestMemoryError::OutOfBounds { addr: paddr, len })?;

            // Safety: callers ensure `ram_offset..ram_offset+len` lies within the mapped window.
            unsafe {
                Ok(core::slice::from_raw_parts(
                    self.ram_ptr.add(rel_usize),
                    len,
                ))
            }
        }

        #[inline]
        fn ram_slice_mut(
            &mut self,
            paddr: u64,
            ram_offset: u64,
            len: usize,
        ) -> Result<&mut [u8], GuestMemoryError> {
            let end = ram_offset
                .checked_add(len as u64)
                .ok_or(GuestMemoryError::OutOfBounds { addr: paddr, len })?;
            let window_end = self
                .ram_offset_base
                .checked_add(self.ram_window_len)
                .ok_or(GuestMemoryError::OutOfBounds { addr: paddr, len })?;
            if ram_offset < self.ram_offset_base || end > window_end {
                return Err(GuestMemoryError::OutOfBounds { addr: paddr, len });
            }
            let rel = ram_offset - self.ram_offset_base;
            let rel_usize = usize::try_from(rel)
                .map_err(|_| GuestMemoryError::OutOfBounds { addr: paddr, len })?;

            // Safety: callers ensure `ram_offset..ram_offset+len` lies within the mapped window.
            unsafe {
                Ok(core::slice::from_raw_parts_mut(
                    self.ram_ptr.add(rel_usize),
                    len,
                ))
            }
        }

        #[inline]
        fn open_bus_slice(
            &self,
            paddr: u64,
            len: usize,
        ) -> Result<&'static [u8], GuestMemoryError> {
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

    impl VirtioGuestMemory for WasmGuestMemory {
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
                    core::slice::from_raw_parts_mut(
                        core::ptr::NonNull::<u8>::dangling().as_ptr(),
                        0,
                    )
                });
            }

            match translate_guest_paddr_range(self.ram_bytes, addr, len) {
                GuestRamRange::Ram { ram_offset } => self.ram_slice_mut(addr, ram_offset, len),
                GuestRamRange::Hole => self.open_bus_slice_mut(addr, len),
                GuestRamRange::OutOfBounds => Err(GuestMemoryError::OutOfBounds { addr, len }),
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use wasm_bindgen::prelude::*;

    use super::wasm_guest_memory::WasmGuestMemory;

    fn js_error(message: impl core::fmt::Display) -> JsValue {
        js_sys::Error::new(&message.to_string()).into()
    }

    /// WASM export: virtio-input device exposed as a virtio-pci BAR0 MMIO region.
    #[wasm_bindgen]
    pub struct VirtioInputPciDevice {
        inner: VirtioInputPciDeviceCore,
        mem: WasmGuestMemory,
    }

    #[wasm_bindgen]
    impl VirtioInputPciDevice {
        /// Create a new virtio-input virtio-pci device wrapper.
        ///
        /// `guest_base` and `guest_size` come from the shared guest RAM layout contract
        /// (`web/src/runtime/shared_layout.ts`).
        ///
        /// When `guest_size == 0`, the device treats the remainder of wasm linear memory as guest
        /// RAM (mirrors `UhciControllerBridge`).
        #[wasm_bindgen(constructor)]
        pub fn new(
            guest_base: u32,
            guest_size: u32,
            kind: String,
            transport_mode: Option<JsValue>,
        ) -> Result<Self, JsValue> {
            let kind_enum = match kind.as_str() {
                "keyboard" => VirtioInputDeviceKind::Keyboard,
                "mouse" => VirtioInputDeviceKind::Mouse,
                _ => {
                    return Err(js_error(
                        r#"Invalid virtio-input kind (expected \"keyboard\" or \"mouse\")"#,
                    ));
                }
            };

            let transport = match transport_mode {
                None => VirtioPciTransportMode::ModernOnly,
                Some(value) => {
                    // Be permissive: accept both numeric and string modes so JS callers can use
                    // enums without needing a specific exported type.
                    if value.is_null() || value.is_undefined() {
                        VirtioPciTransportMode::ModernOnly
                    } else if let Some(b) = value.as_bool() {
                        if b {
                            VirtioPciTransportMode::Transitional
                        } else {
                            VirtioPciTransportMode::ModernOnly
                        }
                    } else if let Some(n) = value.as_f64() {
                        match n as i32 {
                            0 => VirtioPciTransportMode::ModernOnly,
                            1 => VirtioPciTransportMode::Transitional,
                            2 => VirtioPciTransportMode::LegacyOnly,
                            _ => {
                                return Err(js_error(format!(
                                    "invalid virtio-input pci transport mode: {n}"
                                )));
                            }
                        }
                    } else if let Some(s) = value.as_string() {
                        let s = s.trim().to_ascii_lowercase();
                        match s.as_str() {
                            "" | "modern" | "modern-only" | "modern_only" => {
                                VirtioPciTransportMode::ModernOnly
                            }
                            "transitional" => VirtioPciTransportMode::Transitional,
                            "legacy" | "legacy-only" | "legacy_only" => {
                                VirtioPciTransportMode::LegacyOnly
                            }
                            _ => {
                                return Err(js_error(format!(
                                    "invalid virtio-input pci transport mode: {s}"
                                )));
                            }
                        }
                    } else {
                        return Err(js_error(
                            "invalid virtio-input pci transport mode: expected string or number",
                        ));
                    }
                }
            };

            let mem = WasmGuestMemory::new(guest_base, guest_size)?;
            let inner = VirtioInputPciDeviceCore::new_with_transport(kind_enum, transport);
            Ok(Self { inner, mem })
        }

        /// Mirror the guest-written PCI command register (0x04, low 16 bits) into the device.
        ///
        /// This is required for enforcing PCI Bus Master Enable gating for DMA.
        pub fn set_pci_command(&mut self, command: u32) {
            self.inner.set_pci_command(command);
        }

        /// Read from the virtio-pci BAR0 MMIO region.
        pub fn mmio_read(&mut self, offset: u32, size: u8) -> u32 {
            self.inner.mmio_read(u64::from(offset), size)
        }

        /// Write to the virtio-pci BAR0 MMIO region.
        pub fn mmio_write(&mut self, offset: u32, size: u8, value: u32) {
            self.inner
                .mmio_write(u64::from(offset), size, value, &mut self.mem);
        }

        /// Read from the legacy virtio-pci (0.9) I/O port register block (BAR2).
        pub fn legacy_io_read(&mut self, offset: u32, size: u8) -> u32 {
            self.inner.legacy_io_read(offset, size)
        }

        /// Write to the legacy virtio-pci (0.9) I/O port register block (BAR2).
        pub fn legacy_io_write(&mut self, offset: u32, size: u8, value: u32) {
            self.inner
                .legacy_io_write(offset, size, value, &mut self.mem);
        }

        /// Back-compat alias for `legacy_io_read` (mirrors `VirtioNetPciBridge`).
        pub fn io_read(&mut self, offset: u32, size: u8) -> u32 {
            self.legacy_io_read(offset, size)
        }

        /// Back-compat alias for `legacy_io_write` (mirrors `VirtioNetPciBridge`).
        pub fn io_write(&mut self, offset: u32, size: u8, value: u32) {
            self.legacy_io_write(offset, size, value);
        }

        /// Read alias retained for older call sites.
        pub fn bar0_read(&mut self, offset: u32, size: u8) -> u32 {
            self.mmio_read(offset, size)
        }

        /// Write alias retained for older call sites.
        pub fn bar0_write(&mut self, offset: u32, size: u8, value: u32) {
            self.mmio_write(offset, size, value)
        }

        /// Process pending queue work (device-driven paths, completed buffers, interrupts).
        pub fn poll(&mut self) {
            self.inner.poll(&mut self.mem);
        }

        /// Whether the guest driver has set `VIRTIO_STATUS_DRIVER_OK`.
        pub fn driver_ok(&mut self) -> bool {
            self.inner.driver_ok()
        }

        /// Current INTx asserted state (level-triggered).
        pub fn irq_asserted(&self) -> bool {
            self.inner.irq_asserted()
        }

        /// Inject a Linux input key code event (keyboard devices only).
        pub fn inject_key(&mut self, linux_key: u32, pressed: bool) {
            let Ok(code) = u16::try_from(linux_key) else {
                return;
            };
            self.inner.inject_key(code, pressed, &mut self.mem);
        }

        /// Inject a relative movement event (mouse devices only).
        pub fn inject_rel(&mut self, dx: i32, dy: i32) {
            self.inner.inject_rel(dx, dy, &mut self.mem);
        }

        /// Alias for `inject_rel`.
        pub fn inject_rel_move(&mut self, dx: i32, dy: i32) {
            self.inject_rel(dx, dy)
        }

        /// Inject a mouse button event (mouse devices only).
        pub fn inject_button(&mut self, btn: u32, pressed: bool) {
            let Ok(code) = u16::try_from(btn) else {
                return;
            };
            self.inner.inject_button(code, pressed, &mut self.mem);
        }

        /// Inject a mouse wheel event (mouse devices only).
        pub fn inject_wheel(&mut self, delta: i32) {
            self.inner.inject_wheel(delta, &mut self.mem);
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm::VirtioInputPciDevice;

#[cfg(test)]
mod remap_tests {
    use super::wasm_guest_memory::WasmGuestMemory;

    use super::{VirtioInputDeviceKind, VirtioInputPciDeviceCore};
    use aero_virtio::memory::GuestMemory;

    #[test]
    fn virtio_wasm_guest_memory_maps_high_ram_above_4gib() {
        // Simulate a guest with low RAM up to the PCIe ECAM base and 8KiB of remapped high RAM.
        let pcie_ecam_base = aero_pc_constants::PCIE_ECAM_BASE;
        let ram_bytes = pcie_ecam_base + 0x2000;

        // Only allocate the high-RAM portion and map it as a window starting at the low-RAM end.
        // This avoids requiring a multi-GB allocation in the unit test.
        let mut high = vec![0u8; 0x2000];
        high[0..4].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);

        let mem = WasmGuestMemory::new_for_test(ram_bytes, pcie_ecam_base, high.as_mut_slice());

        let slice = mem.get_slice(0x1_0000_0000, 4).expect("high RAM slice");
        assert_eq!(slice, &[0x11, 0x22, 0x33, 0x44]);
        assert_eq!(slice.as_ptr(), high.as_ptr());
    }

    #[test]
    fn mmio_read_size0_is_noop() {
        let mut dev = VirtioInputPciDeviceCore::new(VirtioInputDeviceKind::Keyboard);
        assert_eq!(dev.mmio_read(0, 0), 0);
    }
}
