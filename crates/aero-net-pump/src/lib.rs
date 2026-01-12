//! Glue logic for pumping Ethernet frames between emulated NICs and host backends.
//!
//! Many integration layers (native emulator, WASM runtime, future machine models) need the same
//! deterministic "per tick" glue:
//! 1. Run NIC DMA (`poll(mem)`).
//! 2. Drain guest TX frames to a host [`NetworkBackend`], with a bounded budget.
//! 3. Poll backend RX frames and enqueue into the NIC, with a bounded budget.
//! 4. Run NIC DMA again to flush newly enqueued RX frames into guest buffers.
//!
//! ## Usage (borrowed tick-style pump)
//!
//! ```no_run
//! use aero_net_backend::NetworkBackend;
//! use aero_net_e1000::E1000Device;
//! use aero_net_pump::E1000TickPump;
//! use memory::MemoryBus;
//!
//! struct DummyMem;
//! impl MemoryBus for DummyMem {
//!     fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {}
//!     fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {}
//! }
//!
//! struct DummyBackend;
//! impl NetworkBackend for DummyBackend {
//!     fn transmit(&mut self, _frame: Vec<u8>) {}
//! }
//!
//! let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
//! // The E1000 model gates all DMA on PCI COMMAND.BME (bit 2).
//! nic.pci_config_write(0x04, 2, 0x4);
//!
//! let mut mem = DummyMem;
//! let mut backend = DummyBackend;
//! let pump = E1000TickPump::new(64, 64);
//! pump.tick(&mut nic, &mut mem, &mut backend);
//! ```
#![forbid(unsafe_code)]

use aero_net_backend::NetworkBackend;
use aero_net_e1000::E1000Device;
use memory::MemoryBus;

/// Default frame budget for each direction per [`E1000Pump::poll`] call.
pub const DEFAULT_MAX_FRAMES_PER_POLL: usize = 256;

/// Number of frames pumped in each direction during a tick/poll.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PumpCounts {
    /// Guest → host frames forwarded to the backend via [`NetworkBackend::transmit`].
    pub tx_frames: usize,
    /// Host → guest frames fetched from the backend via [`NetworkBackend::poll_receive`] and
    /// queued into the NIC.
    ///
    /// Frames that are too small/large for the E1000 model are dropped and are **not** counted.
    pub rx_frames: usize,
}

/// Configuration-only pump helper for integration layers that *borrow* the NIC and backend.
///
/// This matches the common "tick" style used by emulator main loops: the pump stores budgets and
/// `tick()` is called once per emulation slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct E1000TickPump {
    pub max_tx_frames_per_tick: usize,
    pub max_rx_frames_per_tick: usize,
}

impl Default for E1000TickPump {
    fn default() -> Self {
        Self {
            max_tx_frames_per_tick: DEFAULT_MAX_FRAMES_PER_POLL,
            max_rx_frames_per_tick: DEFAULT_MAX_FRAMES_PER_POLL,
        }
    }
}

impl E1000TickPump {
    pub fn new(max_tx_frames_per_tick: usize, max_rx_frames_per_tick: usize) -> Self {
        Self {
            max_tx_frames_per_tick,
            max_rx_frames_per_tick,
        }
    }

    pub fn tick<B: NetworkBackend + ?Sized>(
        &self,
        nic: &mut E1000Device,
        mem: &mut dyn MemoryBus,
        backend: &mut B,
    ) {
        tick_e1000(
            nic,
            mem,
            backend,
            self.max_tx_frames_per_tick,
            self.max_rx_frames_per_tick,
        );
    }

    pub fn tick_with_counts<B: NetworkBackend + ?Sized>(
        &self,
        nic: &mut E1000Device,
        mem: &mut dyn MemoryBus,
        backend: &mut B,
    ) -> PumpCounts {
        tick_e1000_with_counts(
            nic,
            mem,
            backend,
            self.max_tx_frames_per_tick,
            self.max_rx_frames_per_tick,
        )
    }
}

/// Pump frames between a borrowed [`E1000Device`] and a borrowed [`NetworkBackend`].
///
/// This is the low-level, allocation-free pump primitive intended for integration layers that
/// already own the NIC and backend (e.g. the PC platform, a WASM runtime, or a canonical machine).
///
/// Note: [`E1000Device`] gates *all* DMA (descriptor reads/writes, RX buffer writes, etc.) on the
/// PCI Bus Master Enable bit (PCI command register bit 2). If bus mastering is not enabled, the
/// pump will not make progress delivering frames into guest memory. Callers are expected to set
/// this bit during PCI enumeration (or in tests via `nic.pci_config_write(0x04, 2, 0x4)`).
///
/// Ordering is deterministic and mirrors virtio-net:
/// 1) `nic.poll(mem)` to process DMA and publish queued guest TX.
/// 2) Drain up to `max_tx_frames_per_tick` from `nic.pop_tx_frame()` and call `backend.transmit`.
/// 3) Drain up to `max_rx_frames_per_tick` from `backend.poll_receive()` and call
///    `nic.enqueue_rx_frame`.
/// 4) Call `nic.poll(mem)` again to flush newly enqueued RX into guest buffers.
pub fn tick_e1000<B: NetworkBackend + ?Sized>(
    nic: &mut E1000Device,
    mem: &mut dyn MemoryBus,
    backend: &mut B,
    max_tx_frames_per_tick: usize,
    max_rx_frames_per_tick: usize,
) {
    let _ = tick_e1000_with_counts(
        nic,
        mem,
        backend,
        max_tx_frames_per_tick,
        max_rx_frames_per_tick,
    );
}

/// Like [`tick_e1000`], but returns the number of frames processed in each direction.
pub fn tick_e1000_with_counts<B: NetworkBackend + ?Sized>(
    nic: &mut E1000Device,
    mem: &mut dyn MemoryBus,
    backend: &mut B,
    max_tx_frames_per_tick: usize,
    max_rx_frames_per_tick: usize,
) -> PumpCounts {
    let mut counts = PumpCounts::default();

    // Step 1: allow the NIC to process descriptor rings (DMA).
    nic.poll(mem);

    // Step 2: forward guest TX frames.
    for _ in 0..max_tx_frames_per_tick {
        let Some(frame) = nic.pop_tx_frame() else {
            break;
        };
        backend.transmit(frame);
        counts.tx_frames += 1;
    }

    // Step 3: inject host RX frames.
    for _ in 0..max_rx_frames_per_tick {
        let Some(frame) = backend.poll_receive() else {
            break;
        };
        // `E1000Device::enqueue_rx_frame` already drops invalid frames, but we
        // pre-filter here so `PumpCounts::rx_frames` accurately reflects frames
        // actually queued into the NIC.
        if frame.len() < aero_net_e1000::MIN_L2_FRAME_LEN
            || frame.len() > aero_net_e1000::MAX_L2_FRAME_LEN
        {
            continue;
        }
        nic.enqueue_rx_frame(frame);
        counts.rx_frames += 1;
    }

    // Step 4: flush injected RX frames into guest buffers.
    nic.poll(mem);

    counts
}

/// Moves Ethernet frames between an [`E1000Device`] and a host-side [`NetworkBackend`].
#[derive(Debug)]
pub struct E1000Pump<B> {
    nic: E1000Device,
    backend: B,

    max_tx_frames_per_poll: usize,
    max_rx_frames_per_poll: usize,
}

impl<B: NetworkBackend> E1000Pump<B> {
    /// Create a pump with default budgets.
    pub fn new(nic: E1000Device, backend: B) -> Self {
        Self::with_budgets(
            nic,
            backend,
            DEFAULT_MAX_FRAMES_PER_POLL,
            DEFAULT_MAX_FRAMES_PER_POLL,
        )
    }

    /// Create a pump with explicit budgets.
    pub fn with_budgets(
        nic: E1000Device,
        backend: B,
        max_tx_frames_per_poll: usize,
        max_rx_frames_per_poll: usize,
    ) -> Self {
        Self {
            nic,
            backend,
            max_tx_frames_per_poll,
            max_rx_frames_per_poll,
        }
    }

    /// Run one pump iteration.
    pub fn poll(&mut self, mem: &mut dyn MemoryBus) {
        tick_e1000(
            &mut self.nic,
            mem,
            &mut self.backend,
            self.max_tx_frames_per_poll,
            self.max_rx_frames_per_poll,
        );
    }

    /// Run one pump iteration and return the number of frames processed in each direction.
    pub fn poll_with_counts(&mut self, mem: &mut dyn MemoryBus) -> PumpCounts {
        tick_e1000_with_counts(
            &mut self.nic,
            mem,
            &mut self.backend,
            self.max_tx_frames_per_poll,
            self.max_rx_frames_per_poll,
        )
    }

    pub fn nic(&self) -> &E1000Device {
        &self.nic
    }

    pub fn nic_mut(&mut self) -> &mut E1000Device {
        &mut self.nic
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn into_parts(self) -> (E1000Device, B) {
        (self.nic, self.backend)
    }

    pub fn max_tx_frames_per_poll(&self) -> usize {
        self.max_tx_frames_per_poll
    }

    pub fn max_rx_frames_per_poll(&self) -> usize {
        self.max_rx_frames_per_poll
    }

    pub fn set_max_tx_frames_per_poll(&mut self, value: usize) {
        self.max_tx_frames_per_poll = value;
    }

    pub fn set_max_rx_frames_per_poll(&mut self, value: usize) {
        self.max_rx_frames_per_poll = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use aero_net_backend::L2TunnelBackend;
    use aero_net_stack::packet::*;
    use aero_net_stack::{NetStackBackend, StackConfig};
    use core::net::Ipv4Addr;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    struct TestMem {
        mem: Vec<u8>,
    }

    impl TestMem {
        fn new(size: usize) -> Self {
            Self { mem: vec![0; size] }
        }

        fn write(&mut self, addr: u64, bytes: &[u8]) {
            let addr = addr as usize;
            self.mem[addr..addr + bytes.len()].copy_from_slice(bytes);
        }

        fn read_vec(&self, addr: u64, len: usize) -> Vec<u8> {
            let addr = addr as usize;
            self.mem[addr..addr + len].to_vec()
        }
    }

    impl MemoryBus for TestMem {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let addr = paddr as usize;
            buf.copy_from_slice(&self.mem[addr..addr + buf.len()]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let addr = paddr as usize;
            self.mem[addr..addr + buf.len()].copy_from_slice(buf);
        }
    }

    fn write_u64_le(mem: &mut TestMem, addr: u64, v: u64) {
        mem.write(addr, &v.to_le_bytes());
    }

    /// Minimal legacy TX descriptor layout (16 bytes).
    fn write_tx_desc(mem: &mut TestMem, addr: u64, buf_addr: u64, len: u16, cmd: u8, status: u8) {
        write_u64_le(mem, addr, buf_addr);
        mem.write(addr + 8, &len.to_le_bytes());
        mem.write(addr + 10, &[0u8]); // cso
        mem.write(addr + 11, &[cmd]);
        mem.write(addr + 12, &[status]);
        mem.write(addr + 13, &[0u8]); // css
        mem.write(addr + 14, &0u16.to_le_bytes()); // special
    }

    /// Minimal legacy RX descriptor layout (16 bytes).
    fn write_rx_desc(mem: &mut TestMem, addr: u64, buf_addr: u64, status: u8) {
        write_u64_le(mem, addr, buf_addr);
        mem.write(addr + 8, &0u16.to_le_bytes()); // length
        mem.write(addr + 10, &0u16.to_le_bytes()); // checksum
        mem.write(addr + 12, &[status]);
        mem.write(addr + 13, &[0u8]); // errors
        mem.write(addr + 14, &0u16.to_le_bytes()); // special
    }

    fn build_test_frame(payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(14 + payload.len());
        // Ethernet header (dst/src/ethertype).
        frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
        frame.extend_from_slice(&0x0800u16.to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    fn configure_tx_ring(dev: &mut E1000Device, desc_base: u32, desc_count: u32) {
        dev.mmio_write_u32_reg(0x3800, desc_base); // TDBAL
        dev.mmio_write_u32_reg(0x3804, 0); // TDBAH
        dev.mmio_write_u32_reg(0x3808, desc_count * 16); // TDLEN
        dev.mmio_write_u32_reg(0x3810, 0); // TDH
        dev.mmio_write_u32_reg(0x3818, 0); // TDT
        dev.mmio_write_u32_reg(0x0400, 1 << 1); // TCTL.EN
    }

    fn configure_rx_ring(dev: &mut E1000Device, desc_base: u32, desc_count: u32, tail: u32) {
        dev.mmio_write_u32_reg(0x2800, desc_base); // RDBAL
        dev.mmio_write_u32_reg(0x2804, 0); // RDBAH
        dev.mmio_write_u32_reg(0x2808, desc_count * 16); // RDLEN
        dev.mmio_write_u32_reg(0x2810, 0); // RDH
        dev.mmio_write_u32_reg(0x2818, tail); // RDT
        dev.mmio_write_u32_reg(0x0100, 1 << 1); // RCTL.EN (defaults to 2048 buffer)
    }

    #[test]
    fn guest_to_host_is_pumped_to_backend() {
        let mut mem = TestMem::new(0x40_000);
        let nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let backend = L2TunnelBackend::new();
        let mut pump = E1000Pump::new(nic, backend);
        pump.nic_mut().pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        configure_tx_ring(pump.nic_mut(), 0x1000, 4);

        let pkt_out = build_test_frame(b"guest->host");
        mem.write(0x4000, &pkt_out);
        write_tx_desc(
            &mut mem,
            0x1000,
            0x4000,
            pkt_out.len() as u16,
            0b0000_1001, // EOP|RS
            0,
        );
        pump.nic_mut().mmio_write_u32_reg(0x3818, 1); // TDT

        pump.poll(&mut mem);

        assert_eq!(pump.backend_mut().drain_tx_frames(), vec![pkt_out]);
    }

    #[test]
    fn owning_pump_poll_with_counts_reports_progress() {
        let mut mem = TestMem::new(0x80_000);
        let nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let backend = L2TunnelBackend::new();
        let mut pump = E1000Pump::with_budgets(nic, backend, 8, 8);
        pump.nic_mut().pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        // One TX frame.
        configure_tx_ring(pump.nic_mut(), 0x1000, 4);
        let tx_frame = build_test_frame(b"tx");
        mem.write(0x4000, &tx_frame);
        write_tx_desc(
            &mut mem,
            0x1000,
            0x4000,
            tx_frame.len() as u16,
            0b0000_1001, // EOP|RS
            0,
        );
        pump.nic_mut().mmio_write_u32_reg(0x3818, 1); // TDT

        // One RX frame.
        configure_rx_ring(pump.nic_mut(), 0x2000, 2, 1);
        write_rx_desc(&mut mem, 0x2000, 0x3000, 0);
        write_rx_desc(&mut mem, 0x2010, 0x3400, 0);
        let rx_frame = build_test_frame(b"rx");
        pump.backend_mut().push_rx_frame(rx_frame.clone());

        let counts0 = pump.poll_with_counts(&mut mem);
        assert_eq!(
            counts0,
            PumpCounts {
                tx_frames: 1,
                rx_frames: 1,
            }
        );
        assert_eq!(pump.backend_mut().drain_tx_frames(), vec![tx_frame]);
        assert_eq!(mem.read_vec(0x3000, rx_frame.len()), rx_frame);

        // Second poll should have nothing left to do.
        let counts1 = pump.poll_with_counts(&mut mem);
        assert_eq!(
            counts1,
            PumpCounts {
                tx_frames: 0,
                rx_frames: 0,
            }
        );
    }

    #[test]
    fn tick_function_supports_trait_object_backend() {
        let mut mem = TestMem::new(0x40_000);
        let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        nic.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        #[derive(Clone, Default)]
        struct RecordingBackend {
            tx_log: Rc<RefCell<Vec<Vec<u8>>>>,
        }

        impl NetworkBackend for RecordingBackend {
            fn transmit(&mut self, frame: Vec<u8>) {
                self.tx_log.borrow_mut().push(frame);
            }
        }

        let tx_log = Rc::new(RefCell::new(Vec::new()));
        let mut backend: Box<dyn NetworkBackend> = Box::new(RecordingBackend {
            tx_log: tx_log.clone(),
        });

        configure_tx_ring(&mut nic, 0x1000, 4);

        let pkt_out = build_test_frame(b"guest->host");
        mem.write(0x4000, &pkt_out);
        write_tx_desc(
            &mut mem,
            0x1000,
            0x4000,
            pkt_out.len() as u16,
            0b0000_1001, // EOP|RS
            0,
        );
        nic.mmio_write_u32_reg(0x3818, 1); // TDT

        tick_e1000(&mut nic, &mut mem, backend.as_mut(), 16, 16);

        assert_eq!(&*tx_log.borrow(), &[pkt_out]);
    }

    #[test]
    fn tick_with_counts_reports_processed_frames() {
        let mut mem = TestMem::new(0x80_000);
        let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        nic.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        let mut backend = L2TunnelBackend::new();

        // Two TX frames but a budget of 1 per tick.
        configure_tx_ring(&mut nic, 0x1000, 4);
        let tx0 = build_test_frame(b"tx0");
        let tx1 = build_test_frame(b"tx1");
        mem.write(0x4000, &tx0);
        mem.write(0x4200, &tx1);
        write_tx_desc(&mut mem, 0x1000, 0x4000, tx0.len() as u16, 0b0000_1001, 0);
        write_tx_desc(&mut mem, 0x1010, 0x4200, tx1.len() as u16, 0b0000_1001, 0);
        nic.mmio_write_u32_reg(0x3818, 2); // TDT

        // Two RX frames but a budget of 1 per tick.
        // Use 4 descriptors so at least 2 are usable without the guest updating the tail.
        configure_rx_ring(&mut nic, 0x2000, 4, 3);
        write_rx_desc(&mut mem, 0x2000, 0x3000, 0);
        write_rx_desc(&mut mem, 0x2010, 0x3400, 0);
        write_rx_desc(&mut mem, 0x2020, 0x3800, 0);
        write_rx_desc(&mut mem, 0x2030, 0x3C00, 0);

        let rx0 = build_test_frame(b"rx0");
        let rx1 = build_test_frame(b"rx1");
        backend.push_rx_frame(rx0.clone());
        backend.push_rx_frame(rx1.clone());

        let pump = E1000TickPump::new(1, 1);
        let counts0 = pump.tick_with_counts(&mut nic, &mut mem, &mut backend);
        assert_eq!(
            counts0,
            PumpCounts {
                tx_frames: 1,
                rx_frames: 1,
            }
        );
        assert_eq!(backend.drain_tx_frames(), vec![tx0.clone()]);
        assert_eq!(mem.read_vec(0x3000, rx0.len()), rx0);

        let counts1 = pump.tick_with_counts(&mut nic, &mut mem, &mut backend);
        assert_eq!(
            counts1,
            PumpCounts {
                tx_frames: 1,
                rx_frames: 1,
            }
        );
        assert_eq!(backend.drain_tx_frames(), vec![tx1]);
        assert_eq!(mem.read_vec(0x3400, rx1.len()), rx1);
    }

    #[test]
    fn host_to_guest_is_pumped_into_guest_memory() {
        let mut mem = TestMem::new(0x40_000);
        let nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let backend = L2TunnelBackend::new();
        let mut pump = E1000Pump::new(nic, backend);
        pump.nic_mut().pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        configure_rx_ring(pump.nic_mut(), 0x2000, 2, 1);
        write_rx_desc(&mut mem, 0x2000, 0x3000, 0);
        write_rx_desc(&mut mem, 0x2010, 0x3400, 0);

        let pkt_in = build_test_frame(b"host->guest");
        pump.backend_mut().push_rx_frame(pkt_in.clone());

        pump.poll(&mut mem);

        assert_eq!(mem.read_vec(0x3000, pkt_in.len()), pkt_in);
    }

    #[test]
    fn tx_budget_limits_frames_per_poll() {
        let mut mem = TestMem::new(0x80_000);
        let nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let backend = L2TunnelBackend::new();
        let mut pump = E1000Pump::with_budgets(nic, backend, 1, DEFAULT_MAX_FRAMES_PER_POLL);
        pump.nic_mut().pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        // Ring of 4 descriptors with tail=3 -> 3 frames in a single guest TX batch.
        configure_tx_ring(pump.nic_mut(), 0x1000, 4);

        let frames = [
            build_test_frame(b"tx0"),
            build_test_frame(b"tx1"),
            build_test_frame(b"tx2"),
        ];

        for (i, frame) in frames.iter().enumerate() {
            let buf = 0x4000 + (i as u64) * 0x200;
            let desc = 0x1000 + (i as u64) * 16;
            mem.write(buf, frame);
            write_tx_desc(
                &mut mem,
                desc,
                buf,
                frame.len() as u16,
                0b0000_1001, // EOP|RS
                0,
            );
        }
        pump.nic_mut().mmio_write_u32_reg(0x3818, 3); // TDT

        pump.poll(&mut mem);
        assert_eq!(pump.backend_mut().drain_tx_frames(), vec![frames[0].clone()]);

        pump.poll(&mut mem);
        assert_eq!(pump.backend_mut().drain_tx_frames(), vec![frames[1].clone()]);

        pump.poll(&mut mem);
        assert_eq!(pump.backend_mut().drain_tx_frames(), vec![frames[2].clone()]);
    }

    #[test]
    fn rx_budget_limits_frames_per_poll() {
        let mut mem = TestMem::new(0x80_000);
        let nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let backend = L2TunnelBackend::new();
        let mut pump = E1000Pump::with_budgets(nic, backend, DEFAULT_MAX_FRAMES_PER_POLL, 1);
        pump.nic_mut().pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        // RX rings keep one descriptor unused to distinguish full/empty conditions.
        // desc_count=4, tail=3 gives us 3 usable RX descriptors (indices 0..2).
        configure_rx_ring(pump.nic_mut(), 0x2000, 4, 3);

        let bufs = [0x3000u64, 0x3400, 0x3800, 0x3C00];
        for (i, buf) in bufs.iter().enumerate() {
            write_rx_desc(&mut mem, 0x2000 + (i as u64) * 16, *buf, 0);
        }

        let frames = [
            build_test_frame(b"rx0"),
            build_test_frame(b"rx1"),
            build_test_frame(b"rx2"),
        ];

        for frame in &frames {
            pump.backend_mut().push_rx_frame(frame.clone());
        }

        pump.poll(&mut mem);
        assert_eq!(mem.read_vec(bufs[0], frames[0].len()), frames[0]);
        assert_ne!(mem.read_vec(bufs[1], frames[1].len()), frames[1]);

        pump.poll(&mut mem);
        assert_eq!(mem.read_vec(bufs[1], frames[1].len()), frames[1]);
        assert_ne!(mem.read_vec(bufs[2], frames[2].len()), frames[2]);

        pump.poll(&mut mem);
        assert_eq!(mem.read_vec(bufs[2], frames[2].len()), frames[2]);
    }
    #[test]
    fn rx_invalid_frames_are_dropped_and_not_counted() {
        #[derive(Default)]
        struct Backend {
            rx: VecDeque<Vec<u8>>,
        }

        impl NetworkBackend for Backend {
            fn transmit(&mut self, _frame: Vec<u8>) {}

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                self.rx.pop_front()
            }
        }

        let mut mem = TestMem::new(0x80_000);
        let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        nic.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        configure_rx_ring(&mut nic, 0x2000, 2, 1);
        write_rx_desc(&mut mem, 0x2000, 0x3000, 0);
        write_rx_desc(&mut mem, 0x2010, 0x3400, 0);

        let valid = build_test_frame(b"ok");

        let mut backend = Backend {
            rx: VecDeque::from([
                vec![],                      // invalid (empty)
                vec![0xAA; 13],              // invalid (< 14)
                valid.clone(),               // valid
            ]),
        };

        let counts = tick_e1000_with_counts(&mut nic, &mut mem, &mut backend, 0, 3);
        assert_eq!(
            counts,
            PumpCounts {
                tx_frames: 0,
                rx_frames: 1,
            }
        );
        assert!(backend.rx.is_empty());
        assert_eq!(mem.read_vec(0x3000, valid.len()), valid);
    }

    #[test]
    fn rx_budget_prevents_infinite_backend_loop() {
        #[derive(Default)]
        struct InfiniteRxBackend {
            calls: usize,
            frame: Vec<u8>,
        }

        impl NetworkBackend for InfiniteRxBackend {
            fn transmit(&mut self, _frame: Vec<u8>) {}

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                self.calls += 1;
                Some(self.frame.clone())
            }
        }

        let mut mem = TestMem::new(0x80_000);
        let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        nic.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        // Enough RX descriptors to accept multiple frames in a single flush.
        configure_rx_ring(&mut nic, 0x2000, 4, 3);
        write_rx_desc(&mut mem, 0x2000, 0x3000, 0);
        write_rx_desc(&mut mem, 0x2010, 0x3400, 0);
        write_rx_desc(&mut mem, 0x2020, 0x3800, 0);
        write_rx_desc(&mut mem, 0x2030, 0x3C00, 0);

        let frame = build_test_frame(b"infinite");
        let mut backend = InfiniteRxBackend {
            calls: 0,
            frame: frame.clone(),
        };

        let pump = E1000TickPump::new(0, 5);
        let counts = pump.tick_with_counts(&mut nic, &mut mem, &mut backend);

        assert_eq!(backend.calls, 5);
        assert_eq!(
            counts,
            PumpCounts {
                tx_frames: 0,
                rx_frames: 5,
            }
        );

        // At least one frame should have been written into guest memory.
        assert_eq!(mem.read_vec(0x3000, frame.len()), frame);
    }

    #[test]
    fn tick_orders_tx_before_polling_backend_rx() {
        #[derive(Default)]
        struct OrderingBackend {
            events: Vec<&'static str>,
            tx: Vec<Vec<u8>>,
            rx: VecDeque<Vec<u8>>,
        }

        impl NetworkBackend for OrderingBackend {
            fn transmit(&mut self, frame: Vec<u8>) {
                self.events.push("tx");
                self.tx.push(frame);
            }

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                let frame = self.rx.pop_front()?;
                self.events.push("rx");
                Some(frame)
            }
        }

        let mut mem = TestMem::new(0x80_000);
        let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        nic.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        // One TX frame.
        configure_tx_ring(&mut nic, 0x1000, 4);
        let tx_frame = build_test_frame(b"tx-first");
        mem.write(0x4000, &tx_frame);
        write_tx_desc(
            &mut mem,
            0x1000,
            0x4000,
            tx_frame.len() as u16,
            0b0000_1001, // EOP|RS
            0,
        );
        nic.mmio_write_u32_reg(0x3818, 1); // TDT

        // One RX descriptor.
        configure_rx_ring(&mut nic, 0x2000, 2, 1);
        write_rx_desc(&mut mem, 0x2000, 0x3000, 0);
        write_rx_desc(&mut mem, 0x2010, 0x3400, 0);

        let rx_frame = build_test_frame(b"rx-second");

        let mut backend = OrderingBackend::default();
        backend.rx.push_back(rx_frame.clone());

        let pump = E1000TickPump::new(16, 16);
        pump.tick(&mut nic, &mut mem, &mut backend);

        assert_eq!(backend.events, vec!["tx", "rx"]);
        assert_eq!(backend.tx, vec![tx_frame]);
        assert_eq!(mem.read_vec(0x3000, rx_frame.len()), rx_frame);
    }

    #[test]
    fn rx_delivery_waits_until_bus_master_is_enabled() {
        let mut mem = TestMem::new(0x80_000);
        let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);

        // RX ring: one usable descriptor.
        configure_rx_ring(&mut nic, 0x2000, 2, 1);
        write_rx_desc(&mut mem, 0x2000, 0x3000, 0);
        write_rx_desc(&mut mem, 0x2010, 0x3400, 0);

        let frame = build_test_frame(b"rx");

        let mut backend = L2TunnelBackend::new();
        backend.push_rx_frame(frame.clone());

        // With bus mastering disabled (default), the pump can dequeue the frame from the backend
        // and enqueue it into the NIC, but the NIC must not DMA into guest memory yet.
        let counts0 = tick_e1000_with_counts(&mut nic, &mut mem, &mut backend, 0, 1);
        assert_eq!(
            counts0,
            PumpCounts {
                tx_frames: 0,
                rx_frames: 1,
            }
        );
        assert_eq!(backend.poll_receive(), None);
        assert_eq!(mem.read_vec(0x3000, frame.len()), vec![0u8; frame.len()]);

        // Enable bus mastering and tick again: the pending frame should now be DMA-written to the
        // guest RX buffer.
        nic.pci_config_write(0x04, 2, 0x4); // Bus Master Enable
        let counts1 = tick_e1000_with_counts(&mut nic, &mut mem, &mut backend, 0, 0);
        assert_eq!(
            counts1,
            PumpCounts {
                tx_frames: 0,
                rx_frames: 0,
            }
        );
        assert_eq!(mem.read_vec(0x3000, frame.len()), frame);
    }

    #[test]
    fn tx_out_is_drained_even_if_bus_master_is_disabled_after_dma() {
        let mut mem = TestMem::new(0x80_000);
        let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        nic.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        // One TX descriptor.
        configure_tx_ring(&mut nic, 0x1000, 4);
        let frame = build_test_frame(b"tx");
        mem.write(0x4000, &frame);
        write_tx_desc(
            &mut mem,
            0x1000,
            0x4000,
            frame.len() as u16,
            0b0000_1001, // EOP|RS
            0,
        );
        nic.mmio_write_u32_reg(0x3818, 1); // TDT

        let mut backend = L2TunnelBackend::new();

        // Process DMA so the frame lands in the NIC's host TX queue, but do not drain it to the
        // backend yet.
        let counts0 = tick_e1000_with_counts(&mut nic, &mut mem, &mut backend, 0, 0);
        assert_eq!(
            counts0,
            PumpCounts {
                tx_frames: 0,
                rx_frames: 0,
            }
        );

        // Disable bus mastering after DMA has completed.
        nic.pci_config_write(0x04, 2, 0x0);

        // Even with bus mastering disabled, the already-produced host TX queue should still be
        // drained to the backend (bus mastering only gates DMA, not the host-facing queue).
        let counts1 = tick_e1000_with_counts(&mut nic, &mut mem, &mut backend, 1, 0);
        assert_eq!(
            counts1,
            PumpCounts {
                tx_frames: 1,
                rx_frames: 0,
            }
        );
        assert_eq!(backend.drain_tx_frames(), vec![frame]);
    }

    #[test]
    fn zero_budgets_do_not_call_backend() {
        struct PanicBackend;

        impl NetworkBackend for PanicBackend {
            fn transmit(&mut self, _frame: Vec<u8>) {
                panic!("unexpected transmit with zero budget");
            }

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                panic!("unexpected poll_receive with zero budget");
            }
        }

        let mut mem = TestMem::new(0x80_000);
        let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        nic.pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        // Prepare a TX descriptor so the NIC would have something to publish to its TX queue.
        configure_tx_ring(&mut nic, 0x1000, 4);
        let tx_frame = build_test_frame(b"tx");
        mem.write(0x4000, &tx_frame);
        write_tx_desc(
            &mut mem,
            0x1000,
            0x4000,
            tx_frame.len() as u16,
            0b0000_1001, // EOP|RS
            0,
        );
        nic.mmio_write_u32_reg(0x3818, 1); // TDT

        // Prepare an RX ring; backend won't be polled due to zero budget.
        configure_rx_ring(&mut nic, 0x2000, 2, 1);
        write_rx_desc(&mut mem, 0x2000, 0x3000, 0);
        write_rx_desc(&mut mem, 0x2010, 0x3400, 0);

        let mut backend = PanicBackend;
        let counts = tick_e1000_with_counts(&mut nic, &mut mem, &mut backend, 0, 0);
        assert_eq!(
            counts,
            PumpCounts {
                tx_frames: 0,
                rx_frames: 0,
            }
        );

        // The NIC should still have processed its rings during the initial poll and enqueued the
        // TX frame into its host-facing TX queue (but we did not drain it due to budget 0).
        assert_eq!(nic.pop_tx_frame().as_deref(), Some(tx_frame.as_slice()));
    }

    #[test]
    fn same_poll_backend_response_is_delivered_to_guest() {
        #[derive(Default)]
        struct EchoBackend {
            tx: Vec<Vec<u8>>,
            rx: VecDeque<Vec<u8>>,
        }

        impl NetworkBackend for EchoBackend {
            fn transmit(&mut self, frame: Vec<u8>) {
                // Record the outbound frame.
                self.tx.push(frame.clone());

                // Immediately enqueue a response frame so the pump can deliver it within the same
                // poll iteration.
                let mut resp = frame;
                if let Some(last) = resp.last_mut() {
                    *last ^= 0xFF;
                }
                self.rx.push_back(resp);
            }

            fn poll_receive(&mut self) -> Option<Vec<u8>> {
                self.rx.pop_front()
            }
        }

        let mut mem = TestMem::new(0x40_000);
        let nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let backend = EchoBackend::default();
        let mut pump = E1000Pump::with_budgets(nic, backend, 1, 1);
        pump.nic_mut().pci_config_write(0x04, 2, 0x4); // Bus Master Enable

        // Set up a single TX descriptor and a single usable RX descriptor.
        configure_tx_ring(pump.nic_mut(), 0x1000, 4);
        configure_rx_ring(pump.nic_mut(), 0x2000, 2, 1);
        write_rx_desc(&mut mem, 0x2000, 0x3000, 0);
        write_rx_desc(&mut mem, 0x2010, 0x3400, 0);

        let pkt_out = build_test_frame(b"ping");
        mem.write(0x4000, &pkt_out);
        write_tx_desc(
            &mut mem,
            0x1000,
            0x4000,
            pkt_out.len() as u16,
            0b0000_1001, // EOP|RS
            0,
        );
        pump.nic_mut().mmio_write_u32_reg(0x3818, 1); // TDT

        pump.poll(&mut mem);

        // Verify TX reached the backend.
        assert_eq!(pump.backend().tx, vec![pkt_out.clone()]);

        // Verify the response was written into the guest RX buffer within the same poll.
        let mut expected = pkt_out;
        *expected.last_mut().unwrap() ^= 0xFF;
        assert_eq!(mem.read_vec(0x3000, expected.len()), expected);
    }

    struct DeterministicNetStackBackend {
        inner: NetStackBackend,
        now_ms: u64,
    }

    impl DeterministicNetStackBackend {
        fn new(cfg: StackConfig) -> Self {
            Self {
                inner: NetStackBackend::new(cfg),
                now_ms: 0,
            }
        }

        fn stack(&self) -> &aero_net_stack::NetworkStack {
            self.inner.stack()
        }
    }

    impl NetworkBackend for DeterministicNetStackBackend {
        fn transmit(&mut self, frame: Vec<u8>) {
            // Drive the stack with a deterministic monotonically increasing clock so this test does
            // not rely on wall-clock time.
            self.now_ms = self.now_ms.saturating_add(1);
            self.inner.transmit_at(frame, self.now_ms);
        }

        fn poll_receive(&mut self) -> Option<Vec<u8>> {
            self.inner.poll_receive()
        }
    }

    fn wrap_udp_ipv4_eth(
        src_mac: MacAddr,
        dst_mac: MacAddr,
        src_ip: Ipv4Addr,
        dst_ip: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let udp = UdpPacketBuilder {
            src_port,
            dst_port,
            payload,
        }
        .build_vec(src_ip, dst_ip)
        .expect("build UDP packet");
        let ip = Ipv4PacketBuilder {
            dscp_ecn: 0,
            identification: 1,
            flags_fragment: 0x4000, // DF
            ttl: 64,
            protocol: Ipv4Protocol::UDP,
            src_ip,
            dst_ip,
            options: &[],
            payload: &udp,
        }
        .build_vec()
        .expect("build IPv4 packet");
        EthernetFrameBuilder {
            dest_mac: dst_mac,
            src_mac,
            ethertype: EtherType::IPV4,
            payload: &ip,
        }
        .build_vec()
        .expect("build Ethernet frame")
    }

    fn build_dhcp_discover(xid: u32, mac: MacAddr) -> Vec<u8> {
        let mut out = vec![0u8; 240];
        out[0] = 1; // BOOTREQUEST
        out[1] = 1; // Ethernet
        out[2] = 6; // MAC len
        out[4..8].copy_from_slice(&xid.to_be_bytes());
        out[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // broadcast
        out[28..34].copy_from_slice(&mac.0);
        out[236..240].copy_from_slice(&[99, 130, 83, 99]); // DHCP magic cookie
        out.extend_from_slice(&[53, 1, 1]); // DHCPDISCOVER
        out.push(255); // end
        out
    }

    fn build_dhcp_request(
        xid: u32,
        mac: MacAddr,
        requested_ip: Ipv4Addr,
        server_id: Ipv4Addr,
    ) -> Vec<u8> {
        let mut out = vec![0u8; 240];
        out[0] = 1; // BOOTREQUEST
        out[1] = 1;
        out[2] = 6;
        out[4..8].copy_from_slice(&xid.to_be_bytes());
        out[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
        out[28..34].copy_from_slice(&mac.0);
        out[236..240].copy_from_slice(&[99, 130, 83, 99]);
        out.extend_from_slice(&[53, 1, 3]); // DHCPREQUEST
        out.extend_from_slice(&[50, 4]); // requested IP
        out.extend_from_slice(&requested_ip.octets());
        out.extend_from_slice(&[54, 4]); // server identifier
        out.extend_from_slice(&server_id.octets());
        out.push(255);
        out
    }

    fn parse_dhcp_from_frame(frame: &[u8]) -> DhcpMessage {
        let eth = EthernetFrame::parse(frame).expect("parse Ethernet frame");
        assert_eq!(
            eth.ethertype(),
            EtherType::IPV4,
            "expected IPv4 ethertype"
        );
        let ip = Ipv4Packet::parse(eth.payload()).expect("parse IPv4 packet");
        assert_eq!(ip.protocol(), Ipv4Protocol::UDP, "expected UDP protocol");
        let udp = UdpPacket::parse(ip.payload()).expect("parse UDP packet");
        assert_eq!(udp.src_port(), 67, "expected DHCP server src port");
        assert_eq!(udp.dst_port(), 68, "expected DHCP client dst port");
        DhcpMessage::parse(udp.payload()).expect("parse DHCP message")
    }

    fn read_rx_desc_len_status(mem: &TestMem, addr: u64) -> (u16, u8) {
        let len_bytes = mem.read_vec(addr + 8, 2);
        let len = u16::from_le_bytes([len_bytes[0], len_bytes[1]]);
        let status = mem.read_vec(addr + 12, 1)[0];
        (len, status)
    }

    #[test]
    fn net_test_e1000_netstack_dhcp_001() {
        let mut mem = TestMem::new(0x20_000);

        let guest_mac_bytes = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
        let guest_mac = MacAddr(guest_mac_bytes);

        let mut nic = E1000Device::new(guest_mac_bytes);
        // The E1000 model gates all DMA on PCI COMMAND.BME (bit 2).
        nic.pci_config_write(0x04, 2, 0x4);

        configure_tx_ring(&mut nic, 0x1000, 4);

        // RX rings keep one descriptor unused to distinguish full/empty conditions.
        // desc_count=8, tail=7 gives us 7 usable RX descriptors (indices 0..6).
        let rx_desc_count = 8u32;
        configure_rx_ring(&mut nic, 0x2000, rx_desc_count, rx_desc_count - 1);

        let rx_bufs: Vec<u64> = (0..rx_desc_count)
            .map(|i| 0x3000u64 + (i as u64) * 0x800) // 2048 bytes per buffer
            .collect();
        for (i, buf) in rx_bufs.iter().enumerate() {
            write_rx_desc(&mut mem, 0x2000 + (i as u64) * 16, *buf, 0);
        }

        let mut backend = DeterministicNetStackBackend::new(StackConfig::default());

        // --- DHCP DISCOVER → OFFER ---
        let xid = 0x1020_3040;
        let discover = build_dhcp_discover(xid, guest_mac);
        let discover_frame = wrap_udp_ipv4_eth(
            guest_mac,
            MacAddr::BROADCAST,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::BROADCAST,
            68,
            67,
            &discover,
        );

        mem.write(0x10_000, &discover_frame);
        write_tx_desc(
            &mut mem,
            0x1000,
            0x10_000,
            discover_frame.len() as u16,
            0b0000_1001, // EOP|RS
            0,
        );
        nic.mmio_write_u32_reg(0x3818, 1); // TDT

        let counts0 = tick_e1000_with_counts(&mut nic, &mut mem, &mut backend, 16, 16);
        assert_eq!(counts0.tx_frames, 1, "expected 1 guest TX frame to backend");
        assert_eq!(
            counts0.rx_frames, 2,
            "expected 2 backend RX frames (broadcast + unicast DHCP OFFER)"
        );

        const DD_EOP: u8 = 0b0000_0011;
        let (rx0_len, rx0_status) = read_rx_desc_len_status(&mem, 0x2000);
        let (rx1_len, rx1_status) = read_rx_desc_len_status(&mem, 0x2010);
        assert!(
            rx0_len > 0,
            "RX desc 0 should have non-zero length after DMA (got {rx0_len})"
        );
        assert!(
            rx1_len > 0,
            "RX desc 1 should have non-zero length after DMA (got {rx1_len})"
        );
        assert_eq!(
            rx0_status & DD_EOP,
            DD_EOP,
            "RX desc 0 should have DD|EOP set (status={rx0_status:#04x})"
        );
        assert_eq!(
            rx1_status & DD_EOP,
            DD_EOP,
            "RX desc 1 should have DD|EOP set (status={rx1_status:#04x})"
        );

        let offer0_frame = mem.read_vec(rx_bufs[0], rx0_len as usize);
        let offer1_frame = mem.read_vec(rx_bufs[1], rx1_len as usize);
        let offer0 = parse_dhcp_from_frame(&offer0_frame);
        let offer1 = parse_dhcp_from_frame(&offer1_frame);
        assert_eq!(offer0.transaction_id, xid, "offer0 XID mismatch");
        assert_eq!(offer1.transaction_id, xid, "offer1 XID mismatch");
        assert!(
            offer0.message_type == DhcpMessageType::Offer
                || offer1.message_type == DhcpMessageType::Offer,
            "expected DHCP OFFER, got offer0={:?} offer1={:?}",
            offer0.message_type,
            offer1.message_type
        );

        // --- DHCP REQUEST → ACK ---
        let request = build_dhcp_request(
            xid,
            guest_mac,
            backend.stack().config().guest_ip,
            backend.stack().config().gateway_ip,
        );
        let request_frame = wrap_udp_ipv4_eth(
            guest_mac,
            MacAddr::BROADCAST,
            Ipv4Addr::UNSPECIFIED,
            Ipv4Addr::BROADCAST,
            68,
            67,
            &request,
        );

        mem.write(0x11_000, &request_frame);
        write_tx_desc(
            &mut mem,
            0x1010,
            0x11_000,
            request_frame.len() as u16,
            0b0000_1001, // EOP|RS
            0,
        );
        nic.mmio_write_u32_reg(0x3818, 2); // TDT

        let counts1 = tick_e1000_with_counts(&mut nic, &mut mem, &mut backend, 16, 16);
        assert_eq!(counts1.tx_frames, 1, "expected 1 guest TX frame to backend");
        assert_eq!(
            counts1.rx_frames, 2,
            "expected 2 backend RX frames (broadcast + unicast DHCP ACK)"
        );

        let (rx2_len, rx2_status) = read_rx_desc_len_status(&mem, 0x2020);
        let (rx3_len, rx3_status) = read_rx_desc_len_status(&mem, 0x2030);
        assert!(
            rx2_len > 0,
            "RX desc 2 should have non-zero length after DMA (got {rx2_len})"
        );
        assert!(
            rx3_len > 0,
            "RX desc 3 should have non-zero length after DMA (got {rx3_len})"
        );
        assert_eq!(
            rx2_status & DD_EOP,
            DD_EOP,
            "RX desc 2 should have DD|EOP set (status={rx2_status:#04x})"
        );
        assert_eq!(
            rx3_status & DD_EOP,
            DD_EOP,
            "RX desc 3 should have DD|EOP set (status={rx3_status:#04x})"
        );

        let ack0_frame = mem.read_vec(rx_bufs[2], rx2_len as usize);
        let ack1_frame = mem.read_vec(rx_bufs[3], rx3_len as usize);
        let ack0 = parse_dhcp_from_frame(&ack0_frame);
        let ack1 = parse_dhcp_from_frame(&ack1_frame);
        assert_eq!(ack0.transaction_id, xid, "ack0 XID mismatch");
        assert_eq!(ack1.transaction_id, xid, "ack1 XID mismatch");
        assert_eq!(ack0.message_type, DhcpMessageType::Ack);
        assert_eq!(ack1.message_type, DhcpMessageType::Ack);
        assert!(
            backend.stack().is_ip_assigned(),
            "expected backend stack to mark IP assigned after DHCP ACK"
        );
    }
}
