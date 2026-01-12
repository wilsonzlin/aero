//! Glue logic for pumping Ethernet frames between emulated NICs and host backends.
//!
//! Many integration layers (native emulator, WASM runtime, future machine models) need the same
//! deterministic "per tick" glue:
//! 1. Run NIC DMA (`poll(mem)`).
//! 2. Drain guest TX frames to a host [`NetworkBackend`], with a bounded budget.
//! 3. Poll backend RX frames and enqueue into the NIC, with a bounded budget.
//! 4. Run NIC DMA again to flush newly enqueued RX frames into guest buffers.
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
        &mut self,
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
        &mut self,
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

        let mut pump = E1000TickPump::new(1, 1);
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

        let mut pump = E1000TickPump::new(0, 5);
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

        let mut pump = E1000TickPump::new(16, 16);
        pump.tick(&mut nic, &mut mem, &mut backend);

        assert_eq!(backend.events, vec!["tx", "rx"]);
        assert_eq!(backend.tx, vec![tx_frame]);
        assert_eq!(mem.read_vec(0x3000, rx_frame.len()), rx_frame);
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
}
