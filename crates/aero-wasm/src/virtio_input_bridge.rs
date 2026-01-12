//! WASM bridge for exposing `aero-virtio`'s virtio-input device model via the modern virtio-pci
//! transport.
//!
//! The browser runtime uses this wrapper as the backing implementation for a PCI function. JS
//! forwards BAR0 MMIO accesses into [`VirtioInputPciDevice::mmio_read`] /
//! [`VirtioInputPciDevice::mmio_write`], and the wrapper reads/writes virtqueue structures directly
//! from the shared guest RAM region inside the WASM linear memory.

use aero_virtio::devices::input::{VirtioInput, VirtioInputDeviceKind};
use aero_virtio::memory::GuestMemory;
use aero_virtio::pci::{InterruptSink, VirtioPciDevice, VIRTIO_STATUS_DRIVER_OK};
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

    fn signal_msix(&mut self, _vector: u16) {
        // The web runtime currently wires up INTx; keep basic accounting for observability.
        self.msix_count.set(self.msix_count.get().saturating_add(1));
    }
}

/// Rust-native wrapper around a virtio-input device exposed via virtio-pci (modern transport).
///
/// This type is used by the wasm-exported [`VirtioInputPciDevice`] and is also exercised by native
/// unit tests in this crate.
pub struct VirtioInputPciDeviceCore {
    kind: VirtioInputDeviceKind,
    pci: VirtioPciDevice,
    irq: InterruptState,
}

impl VirtioInputPciDeviceCore {
    pub fn new(kind: VirtioInputDeviceKind) -> Self {
        let (irq, sink) = InterruptState::new();
        let input = VirtioInput::new(kind);
        let pci = VirtioPciDevice::new(Box::new(input), Box::new(sink));
        Self { kind, pci, irq }
    }

    fn input_mut(&mut self) -> &mut VirtioInput {
        self.pci
            .device_mut::<VirtioInput>()
            .expect("VirtioPciDevice should contain a VirtioInput device")
    }

    pub fn mmio_read(&mut self, offset: u64, size: u8) -> u32 {
        let size = validate_mmio_size(size);
        if size == 0 {
            return 0xFFFF_FFFF;
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

    pub fn mmio_write(
        &mut self,
        offset: u64,
        size: u8,
        value: u32,
        mem: &mut dyn GuestMemory,
    ) {
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
        self.pci.process_notified_queues(mem);
    }

    pub fn poll(&mut self, mem: &mut dyn GuestMemory) {
        self.pci.poll(mem);
    }

    pub fn driver_ok(&mut self) -> bool {
        let status = self.mmio_read(0x14, 1) as u8;
        (status & VIRTIO_STATUS_DRIVER_OK) != 0
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

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::*;
    use aero_virtio::memory::{GuestMemory as VirtioGuestMemory, GuestMemoryError};
    use wasm_bindgen::prelude::*;

    fn js_error(message: impl core::fmt::Display) -> JsValue {
        js_sys::Error::new(&message.to_string()).into()
    }

    fn wasm_memory_byte_len() -> u64 {
        let pages = core::arch::wasm32::memory_size(0) as u64;
        pages.saturating_mul(64 * 1024)
    }

    #[derive(Clone, Copy)]
    struct WasmGuestMemory {
        guest_base: u32,
        guest_size: u64,
    }

    impl WasmGuestMemory {
        fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
            if guest_base == 0 {
                return Err(js_error("guestBase must be non-zero"));
            }
 
            let mem_len = wasm_memory_byte_len();
            let guest_base_u64 = u64::from(guest_base);
            if guest_base_u64 >= mem_len {
                return Err(js_error(format!(
                    "Guest RAM mapping out of bounds: guest_base=0x{guest_base:x} wasm_mem=0x{mem_len:x}"
                )));
            }

            // Match other WASM bridges (e.g. UHCI/E1000): treat `guest_size=0` as "use the
            // remainder of linear memory".
            let guest_size_u64 = if guest_size == 0 {
                mem_len - guest_base_u64
            } else {
                u64::from(guest_size)
            };

            let end = guest_base_u64
                .checked_add(guest_size_u64)
                .ok_or_else(|| js_error("guestBase + guestSize overflow"))?;
            if end > mem_len {
                return Err(js_error(format!(
                    "Guest RAM mapping out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size_u64:x} end=0x{end:x} wasm_mem=0x{mem_len:x}"
                )));
            }

            Ok(Self {
                guest_base,
                guest_size: guest_size_u64,
            })
        }

        #[inline]
        fn linear_offset(&self, paddr: u64, len: usize) -> Result<u32, GuestMemoryError> {
            let len_u64 = len as u64;
            let end = paddr
                .checked_add(len_u64)
                .ok_or(GuestMemoryError::OutOfBounds { addr: paddr, len })?;
            if end > self.guest_size {
                return Err(GuestMemoryError::OutOfBounds { addr: paddr, len });
            }
            let linear = u64::from(self.guest_base)
                .checked_add(paddr)
                .ok_or(GuestMemoryError::OutOfBounds { addr: paddr, len })?;
            u32::try_from(linear).map_err(|_| GuestMemoryError::OutOfBounds { addr: paddr, len })
        }
    }

    impl VirtioGuestMemory for WasmGuestMemory {
        fn len(&self) -> u64 {
            self.guest_size
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
                return Ok(&[]);
            }
            let linear = self.linear_offset(addr, len)?;
            // Safety: `linear_offset` bounds-checks against the configured guest region, and wasm
            // linear memory does not relocate when it grows.
            unsafe { Ok(core::slice::from_raw_parts(linear as *const u8, len)) }
        }

        fn get_slice_mut(&mut self, addr: u64, len: usize) -> Result<&mut [u8], GuestMemoryError> {
            if len == 0 {
                // Safety: a zero-length slice may be created from a dangling pointer.
                return Ok(unsafe {
                    core::slice::from_raw_parts_mut(core::ptr::NonNull::<u8>::dangling().as_ptr(), 0)
                });
            }
            let linear = self.linear_offset(addr, len)?;
            // Safety: `linear_offset` bounds-checks against the configured guest region.
            unsafe { Ok(core::slice::from_raw_parts_mut(linear as *mut u8, len)) }
        }
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
        pub fn new(guest_base: u32, guest_size: u32, kind: String) -> Result<Self, JsValue> {
            let kind_enum = match kind.as_str() {
                "keyboard" => VirtioInputDeviceKind::Keyboard,
                "mouse" => VirtioInputDeviceKind::Mouse,
                _ => {
                    return Err(js_error(
                        r#"Invalid virtio-input kind (expected \"keyboard\" or \"mouse\")"#,
                    ));
                }
            };

            let mem = WasmGuestMemory::new(guest_base, guest_size)?;
            let inner = VirtioInputPciDeviceCore::new(kind_enum);
            Ok(Self { inner, mem })
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
