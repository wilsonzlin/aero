//! AeroGPU: a paravirtual GPU device model.
//!
//! This module implements the *host-side* virtual device model: a command ring, a doorbell,
//! an interrupt status register, and a minimal command processor.
//!
//! The ABI itself is defined in [`protocol`].

mod mmio;
mod pci;
mod protocol;
mod ring;

pub use mmio::*;
pub use pci::*;
pub use protocol::*;
pub use ring::*;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

/// Abstraction over guest physical memory for `UPDATE_SURFACE`.
pub trait GuestMemory: Send + Sync + 'static {
    fn read(&self, guest_phys_addr: u64, dst: &mut [u8]) -> Result<(), GuestMemoryError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestMemoryError {
    OutOfBounds,
}

#[derive(Debug)]
struct Surface {
    width: u32,
    height: u32,
    format: SurfaceFormat,
    /// Row-major tightly packed RGBA8 for now.
    pixels: Vec<u8>,
}

impl Surface {
    fn new(width: u32, height: u32, format: SurfaceFormat) -> Result<Self, StatusCode> {
        let pixel_bytes = format.bytes_per_pixel();
        let len = width
            .checked_mul(height)
            .and_then(|px| px.checked_mul(pixel_bytes))
            .ok_or(StatusCode::OutOfMemory)?;
        Ok(Self {
            width,
            height,
            format,
            pixels: vec![0; len as usize],
        })
    }
}

#[derive(Debug, Default)]
struct Framebuffer {
    width: u32,
    height: u32,
    format: SurfaceFormat,
    pixels: Vec<u8>,
}

/// Host-side AeroGPU device model.
///
/// This is designed to be integrated into an emulator "CPU worker" that handles MMIO accesses.
pub struct AerogpuDevice {
    pub caps: Caps,
    cmd_ring: Arc<RingBuffer>,
    evt_ring: Arc<RingBuffer>,

    doorbell: Arc<Doorbell>,
    irq: Arc<IrqState>,

    guest_mem: Arc<dyn GuestMemory>,

    shared_state: Arc<Mutex<DeviceState>>,

    stop_flag: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

struct Doorbell {
    seq: AtomicU32,
    lock: Mutex<()>,
    cv: Condvar,
}

impl Doorbell {
    fn new() -> Self {
        Self {
            seq: AtomicU32::new(0),
            lock: Mutex::new(()),
            cv: Condvar::new(),
        }
    }

    fn load(&self) -> u32 {
        self.seq.load(Ordering::Acquire)
    }

    fn store(&self, value: u32) {
        self.seq.store(value, Ordering::Release);
        self.cv.notify_all();
    }

    fn ring(&self) {
        self.seq.fetch_add(1, Ordering::AcqRel);
        self.cv.notify_one();
    }

    fn wait_for_change(&self, last: u32, stop_flag: &AtomicBool) -> u32 {
        let mut guard = self.lock.lock().expect("mutex poisoned");
        while self.seq.load(Ordering::Acquire) == last && !stop_flag.load(Ordering::Acquire) {
            guard = self.cv.wait(guard).expect("mutex poisoned");
        }
        self.seq.load(Ordering::Acquire)
    }
}

struct IrqState {
    status: AtomicU32,
    lock: Mutex<()>,
    cv: Condvar,
}

impl IrqState {
    fn new() -> Self {
        Self {
            status: AtomicU32::new(0),
            lock: Mutex::new(()),
            cv: Condvar::new(),
        }
    }

    fn read(&self) -> u32 {
        self.status.load(Ordering::Acquire)
    }

    fn ack(&self, value: u32) {
        self.status.fetch_and(!value, Ordering::AcqRel);
    }

    fn raise(&self, bits: u32) {
        self.status.fetch_or(bits, Ordering::AcqRel);
        self.cv.notify_all();
    }

    fn reset(&self) {
        self.status.store(0, Ordering::Release);
        self.cv.notify_all();
    }

    #[allow(dead_code)]
    fn wait_for(&self, mask: u32) {
        let mut guard = self.lock.lock().expect("mutex poisoned");
        while (self.status.load(Ordering::Acquire) & mask) == 0 {
            guard = self.cv.wait(guard).expect("mutex poisoned");
        }
    }
}

#[derive(Debug)]
struct DeviceState {
    next_surface_id: u32,
    surfaces: HashMap<u32, Surface>,
    framebuffer: Framebuffer,
}

impl DeviceState {
    fn new() -> Self {
        Self {
            next_surface_id: 1,
            surfaces: HashMap::new(),
            framebuffer: Framebuffer::default(),
        }
    }
}

impl AerogpuDevice {
    pub fn new(guest_mem: Arc<dyn GuestMemory>, cfg: DeviceConfig) -> Self {
        let caps = Caps::default_caps();
        let (cmd_ring, _cmd_prod, cmd_cons) = RingBuffer::new(cfg.cmd_ring_size_bytes).split();
        let (evt_ring, evt_prod, _evt_cons) = RingBuffer::new(cfg.evt_ring_size_bytes).split();

        let shared_state = Arc::new(Mutex::new(DeviceState::new()));
        let doorbell = Arc::new(Doorbell::new());
        let irq = Arc::new(IrqState::new());
        let stop_flag = Arc::new(AtomicBool::new(false));

        let worker = {
            let cmd_cons = cmd_cons;
            let evt_prod = evt_prod;
            let doorbell = Arc::clone(&doorbell);
            let irq = Arc::clone(&irq);
            let stop_flag = Arc::clone(&stop_flag);
            let shared_state = Arc::clone(&shared_state);
            let guest_mem = Arc::clone(&guest_mem);
            thread::spawn(move || {
                gpu_worker_loop(
                    cmd_cons,
                    evt_prod,
                    doorbell,
                    irq,
                    stop_flag,
                    shared_state,
                    guest_mem,
                );
            })
        };

        Self {
            caps,
            cmd_ring,
            evt_ring,
            doorbell,
            irq,
            guest_mem,
            shared_state,
            stop_flag,
            worker: Some(worker),
        }
    }

    /// Create a producer handle for the guest/CPU side to write commands into the device ring.
    pub fn cmd_ring_producer(&self) -> RingProducer {
        RingProducer::new(Arc::clone(&self.cmd_ring))
    }

    /// Create a consumer handle for the guest/CPU side to read completion/error events.
    pub fn evt_ring_consumer(&self) -> RingConsumer {
        RingConsumer::new(Arc::clone(&self.evt_ring))
    }

    /// Read the last presented framebuffer pixels (RGBA8).
    pub fn framebuffer(&self) -> FramebufferSnapshot {
        let state = self.shared_state.lock().expect("mutex poisoned");
        FramebufferSnapshot {
            width: state.framebuffer.width,
            height: state.framebuffer.height,
            format: state.framebuffer.format,
            pixels: state.framebuffer.pixels.clone(),
        }
    }

    /// Minimal MMIO read handler.
    pub fn mmio_read_u32(&self, offset: u64) -> u32 {
        mmio::read_u32(self, offset)
    }

    /// Minimal MMIO write handler.
    pub fn mmio_write_u32(&mut self, offset: u64, value: u32) {
        mmio::write_u32(self, offset, value)
    }

    fn reset(&mut self) {
        // Stop worker.
        if let Some(worker) = self.worker.take() {
            self.stop_flag.store(true, Ordering::Release);
            self.doorbell.ring();
            let _ = worker.join();
        }

        // Reset shared state and rings.
        self.cmd_ring.reset();
        self.evt_ring.reset();
        self.irq.reset();
        self.doorbell.store(0);
        self.stop_flag.store(false, Ordering::Release);
        *self.shared_state.lock().expect("mutex poisoned") = DeviceState::new();

        // Restart worker.
        let cmd_cons = self.cmd_ring.consumer();
        let evt_prod = self.evt_ring.producer();
        let doorbell = Arc::clone(&self.doorbell);
        let irq = Arc::clone(&self.irq);
        let stop_flag = Arc::clone(&self.stop_flag);
        let shared_state = Arc::clone(&self.shared_state);
        let guest_mem = Arc::clone(&self.guest_mem);
        self.worker = Some(thread::spawn(move || {
            gpu_worker_loop(
                cmd_cons,
                evt_prod,
                doorbell,
                irq,
                stop_flag,
                shared_state,
                guest_mem,
            );
        }));
    }
}

impl Drop for AerogpuDevice {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.stop_flag.store(true, Ordering::Release);
            self.doorbell.ring();
            let _ = worker.join();
        }
    }
}

#[derive(Debug, Clone)]
pub struct FramebufferSnapshot {
    pub width: u32,
    pub height: u32,
    pub format: SurfaceFormat,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct DeviceConfig {
    pub cmd_ring_size_bytes: u32,
    pub evt_ring_size_bytes: u32,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            cmd_ring_size_bytes: 64 * 1024,
            evt_ring_size_bytes: 16 * 1024,
        }
    }
}

fn gpu_worker_loop(
    mut cmd_cons: RingConsumer,
    mut evt_prod: RingProducer,
    doorbell: Arc<Doorbell>,
    irq: Arc<IrqState>,
    stop_flag: Arc<AtomicBool>,
    shared_state: Arc<Mutex<DeviceState>>,
    guest_mem: Arc<dyn GuestMemory>,
) {
    let mut last_seq = doorbell.load();
    loop {
        // Process any already-enqueued work.
        process_available(
            &mut cmd_cons,
            &mut evt_prod,
            &irq,
            &shared_state,
            guest_mem.as_ref(),
        );

        if stop_flag.load(Ordering::Acquire) {
            break;
        }

        // Block until someone rings the doorbell.
        last_seq = doorbell.wait_for_change(last_seq, &stop_flag);
    }
}

fn process_available(
    cmd_cons: &mut RingConsumer,
    evt_prod: &mut RingProducer,
    irq: &IrqState,
    shared_state: &Mutex<DeviceState>,
    guest_mem: &dyn GuestMemory,
) {
    loop {
        let cmd_bytes = match cmd_cons.try_pop() {
            Ok(Some(bytes)) => bytes,
            Ok(None) => break,
            Err(err) => {
                // Corrupt ring entry. Resetting the ring avoids deadlocking the device.
                let opcode = match err {
                    RingPopError::Corrupt(info) => info.opcode,
                };
                let event = EventCmdStatus::new(opcode, StatusCode::InvalidSize, [0; 4]);
                let _ = evt_prod.try_push(&event.encode());
                irq.raise(IrqBits::CMD_PROCESSED);
                break;
            }
        };

        let (opcode, status, data) = process_command(&cmd_bytes, shared_state, guest_mem);

        let event = EventCmdStatus::new(opcode, status, data);
        let _ = evt_prod.try_push(&event.encode()); // If event ring is full, drop the event.

        // Signal "some command processed" even on errors (guest can read event ring).
        irq.raise(IrqBits::CMD_PROCESSED);

        if opcode == Opcode::PRESENT {
            irq.raise(IrqBits::PRESENT_DONE);
        }
    }
}

fn process_command(
    bytes: &[u8],
    shared_state: &Mutex<DeviceState>,
    guest_mem: &dyn GuestMemory,
) -> (u32, StatusCode, [u32; 4]) {
    let Some(hdr) = CmdHeader::decode(bytes) else {
        return (0, StatusCode::InvalidSize, [0; 4]);
    };

    let opcode = hdr.opcode;
    let payload = &bytes[CmdHeader::SIZE_BYTES..];

    match opcode {
        Opcode::NOP => (opcode, StatusCode::Ok, [0; 4]),
        Opcode::CREATE_SURFACE => {
            let Some(cmd) = CmdCreateSurface::decode(payload) else {
                return (opcode, StatusCode::InvalidSize, [0; 4]);
            };
            if cmd.width == 0 || cmd.height == 0 {
                return (opcode, StatusCode::InvalidArgument, [0; 4]);
            }
            let format = match SurfaceFormat::from_u32(cmd.format) {
                Some(f) => f,
                None => return (opcode, StatusCode::UnsupportedFormat, [0; 4]),
            };

            let mut state = shared_state.lock().expect("mutex poisoned");
            let surface_id = state.next_surface_id;
            state.next_surface_id = state.next_surface_id.wrapping_add(1).max(1);

            match Surface::new(cmd.width, cmd.height, format) {
                Ok(surface) => {
                    state.surfaces.insert(surface_id, surface);
                    (
                        opcode,
                        StatusCode::Ok,
                        [surface_id, cmd.width, cmd.height, cmd.format],
                    )
                }
                Err(status) => (opcode, status, [0; 4]),
            }
        }
        Opcode::UPDATE_SURFACE => {
            let Some(cmd) = CmdUpdateSurface::decode(payload) else {
                return (opcode, StatusCode::InvalidSize, [0; 4]);
            };

            let mut state = shared_state.lock().expect("mutex poisoned");
            let Some(surface) = state.surfaces.get_mut(&cmd.surface_id) else {
                return (
                    opcode,
                    StatusCode::SurfaceNotFound,
                    [cmd.surface_id, 0, 0, 0],
                );
            };
            let row_bytes = (surface.width * surface.format.bytes_per_pixel()) as usize;
            if cmd.stride < row_bytes as u32 {
                return (
                    opcode,
                    StatusCode::InvalidArgument,
                    [cmd.surface_id, 0, 0, 0],
                );
            }
            let stride = cmd.stride as usize;

            for y in 0..surface.height as usize {
                let src_addr = cmd
                    .guest_phys_addr
                    .checked_add((y * stride) as u64)
                    .unwrap_or(u64::MAX);
                let dst = &mut surface.pixels[y * row_bytes..(y + 1) * row_bytes];
                if guest_mem.read(src_addr, dst).is_err() {
                    return (
                        opcode,
                        StatusCode::GuestMemoryFault,
                        [cmd.surface_id, 0, 0, 0],
                    );
                }
            }

            (opcode, StatusCode::Ok, [cmd.surface_id, 0, 0, 0])
        }
        Opcode::CLEAR_RGBA => {
            let Some(cmd) = CmdClearRgba::decode(payload) else {
                return (opcode, StatusCode::InvalidSize, [0; 4]);
            };
            let mut state = shared_state.lock().expect("mutex poisoned");
            let Some(surface) = state.surfaces.get_mut(&cmd.surface_id) else {
                return (
                    opcode,
                    StatusCode::SurfaceNotFound,
                    [cmd.surface_id, 0, 0, 0],
                );
            };

            let rgba = cmd.rgba.to_le_bytes();
            for px in surface.pixels.chunks_exact_mut(4) {
                px.copy_from_slice(&rgba);
            }

            (opcode, StatusCode::Ok, [cmd.surface_id, 0, 0, 0])
        }
        Opcode::DRAW_TRIANGLE_TEST => {
            let Some(cmd) = CmdDrawTriangleTest::decode(payload) else {
                return (opcode, StatusCode::InvalidSize, [0; 4]);
            };
            let mut state = shared_state.lock().expect("mutex poisoned");
            let Some(surface) = state.surfaces.get_mut(&cmd.surface_id) else {
                return (
                    opcode,
                    StatusCode::SurfaceNotFound,
                    [cmd.surface_id, 0, 0, 0],
                );
            };
            if surface.format != SurfaceFormat::Rgba8888 {
                return (
                    opcode,
                    StatusCode::UnsupportedFormat,
                    [cmd.surface_id, 0, 0, 0],
                );
            }

            draw_test_triangle(surface);
            (opcode, StatusCode::Ok, [cmd.surface_id, 0, 0, 0])
        }
        Opcode::PRESENT => {
            let Some(cmd) = CmdPresent::decode(payload) else {
                return (opcode, StatusCode::InvalidSize, [0; 4]);
            };
            let mut state = shared_state.lock().expect("mutex poisoned");
            let Some(surface) = state.surfaces.get(&cmd.surface_id) else {
                return (
                    opcode,
                    StatusCode::SurfaceNotFound,
                    [cmd.surface_id, 0, 0, 0],
                );
            };

            let (width, height, format, pixels) = (
                surface.width,
                surface.height,
                surface.format,
                surface.pixels.clone(),
            );
            state.framebuffer.width = width;
            state.framebuffer.height = height;
            state.framebuffer.format = format;
            state.framebuffer.pixels = pixels;

            (opcode, StatusCode::Ok, [cmd.surface_id, 0, 0, 0])
        }
        _ => (opcode, StatusCode::InvalidOpcode, [0; 4]),
    }
}

fn draw_test_triangle(surface: &mut Surface) {
    let w = surface.width as i32;
    let h = surface.height as i32;
    if w <= 0 || h <= 0 {
        return;
    }

    // Fixed triangle in "pixel-center * 2" coordinates for deterministic integer rasterization.
    let v0 = (w / 2 * 2, h / 4 * 2);
    let v1 = (w / 4 * 2, (h * 3 / 4) * 2);
    let v2 = ((w * 3 / 4) * 2, (h * 3 / 4) * 2);

    fn edge(a: (i32, i32), b: (i32, i32), p: (i32, i32)) -> i32 {
        (p.0 - a.0) * (b.1 - a.1) - (p.1 - a.1) * (b.0 - a.0)
    }

    let red = [0xFF, 0x00, 0x00, 0xFF];

    for y in 0..h {
        for x in 0..w {
            let p = (x * 2 + 1, y * 2 + 1);
            let e0 = edge(v1, v2, p);
            let e1 = edge(v2, v0, p);
            let e2 = edge(v0, v1, p);
            let inside = (e0 >= 0 && e1 >= 0 && e2 >= 0) || (e0 <= 0 && e1 <= 0 && e2 <= 0);
            if inside {
                let idx = ((y * w + x) * 4) as usize;
                surface.pixels[idx..idx + 4].copy_from_slice(&red);
            }
        }
    }
}
