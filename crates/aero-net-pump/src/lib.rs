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
        // Step 1: allow the NIC to process descriptor rings (DMA).
        self.nic.poll(mem);

        // Step 2: forward guest TX frames.
        for _ in 0..self.max_tx_frames_per_poll {
            let Some(frame) = self.nic.pop_tx_frame() else {
                break;
            };
            self.backend.transmit(frame);
        }

        // Step 3: inject host RX frames.
        for _ in 0..self.max_rx_frames_per_poll {
            let Some(frame) = self.backend.poll_receive() else {
                break;
            };
            self.nic.enqueue_rx_frame(frame);
        }

        // Step 4: flush injected RX frames into guest buffers.
        self.nic.poll(mem);
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
    fn host_to_guest_is_pumped_into_guest_memory() {
        let mut mem = TestMem::new(0x40_000);
        let nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let backend = L2TunnelBackend::new();
        let mut pump = E1000Pump::new(nic, backend);

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
}

