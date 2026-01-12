//! Glue logic for pumping Ethernet frames between emulated NICs and host backends.
//!
//! Many integration layers (native emulator, WASM runtime, future machine models) need the same
//! deterministic "per tick" glue:
//! 1. Run NIC DMA (`poll(mem)`).
//! 2. Drain guest TX frames to a host [`NetworkBackend`], with a bounded budget.
//! 3. Poll backend RX frames and enqueue into the NIC, with a bounded budget.
//! 4. Run NIC DMA again to flush newly enqueued RX frames into guest buffers.

use aero_net_backend::NetworkBackend;
use aero_net_e1000::E1000Device;
use memory::MemoryBus;

/// Per-tick pump for an [`E1000Device`].
#[derive(Debug, Clone)]
pub struct E1000Pump {
    /// Maximum number of guest → host frames to forward in one [`tick`].
    pub max_tx_frames_per_tick: usize,
    /// Maximum number of host → guest frames to inject in one [`tick`].
    pub max_rx_frames_per_tick: usize,
}

impl E1000Pump {
    /// Construct a pump with the given TX/RX budgets.
    pub fn new(max_tx_frames_per_tick: usize, max_rx_frames_per_tick: usize) -> Self {
        Self {
            max_tx_frames_per_tick,
            max_rx_frames_per_tick,
        }
    }

    /// Pump frames between the guest NIC and the host backend.
    ///
    /// Ordering is deterministic and intentionally mirrors virtio-net:
    /// 1) `nic.poll(mem)` to process DMA and publish queued guest TX.
    /// 2) Drain up to `max_tx_frames_per_tick` from `nic.pop_tx_frame()` into `backend.transmit`.
    /// 3) Drain up to `max_rx_frames_per_tick` from `backend.poll_receive()` into
    ///    `nic.enqueue_rx_frame`.
    /// 4) `nic.poll(mem)` again to flush newly enqueued RX into guest buffers.
    pub fn tick(
        &mut self,
        nic: &mut E1000Device,
        mem: &mut dyn MemoryBus,
        backend: &mut impl NetworkBackend,
    ) {
        // Step 1: allow the NIC to process descriptor rings (DMA).
        nic.poll(mem);

        // Step 2: forward guest TX frames.
        for _ in 0..self.max_tx_frames_per_tick {
            let Some(frame) = nic.pop_tx_frame() else {
                break;
            };
            backend.transmit(frame);
        }

        // Step 3: inject host RX frames.
        for _ in 0..self.max_rx_frames_per_tick {
            let Some(frame) = backend.poll_receive() else {
                break;
            };
            nic.enqueue_rx_frame(frame);
        }

        // Step 4: flush injected RX frames into guest buffers.
        nic.poll(mem);
    }
}

impl Default for E1000Pump {
    fn default() -> Self {
        // Conservative defaults; callers can tune based on their tick frequency / performance
        // needs.
        Self {
            max_tx_frames_per_tick: 64,
            max_rx_frames_per_tick: 64,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    const REG_RDBAL: u32 = 0x2800;
    const REG_RDBAH: u32 = 0x2804;
    const REG_RDLEN: u32 = 0x2808;
    const REG_RDH: u32 = 0x2810;
    const REG_RDT: u32 = 0x2818;

    const REG_TDBAL: u32 = 0x3800;
    const REG_TDBAH: u32 = 0x3804;
    const REG_TDLEN: u32 = 0x3808;
    const REG_TDH: u32 = 0x3810;
    const REG_TDT: u32 = 0x3818;

    const REG_RCTL: u32 = 0x0100;
    const REG_TCTL: u32 = 0x0400;

    const RCTL_EN: u32 = 1 << 1;
    const TCTL_EN: u32 = 1 << 1;

    const TXD_CMD_EOP: u8 = 1 << 0;
    const TXD_CMD_RS: u8 = 1 << 3;

    const RXD_STAT_DD: u8 = 1 << 0;
    const RXD_STAT_EOP: u8 = 1 << 1;

    #[derive(Clone, Debug)]
    struct TestMem {
        buf: Vec<u8>,
    }

    impl TestMem {
        fn new(size: usize) -> Self {
            Self { buf: vec![0; size] }
        }

        fn write(&mut self, addr: u64, bytes: &[u8]) {
            let addr = usize::try_from(addr).unwrap();
            self.buf[addr..addr + bytes.len()].copy_from_slice(bytes);
        }

        fn read_vec(&self, addr: u64, len: usize) -> Vec<u8> {
            let addr = usize::try_from(addr).unwrap();
            self.buf[addr..addr + len].to_vec()
        }
    }

    impl MemoryBus for TestMem {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let addr = usize::try_from(paddr).unwrap();
            buf.copy_from_slice(&self.buf[addr..addr + buf.len()]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let addr = usize::try_from(paddr).unwrap();
            self.buf[addr..addr + buf.len()].copy_from_slice(buf);
        }
    }

    fn build_frame(tag: u8) -> Vec<u8> {
        // Ethernet header: dst(6) + src(6) + ethertype(2).
        let mut frame = Vec::with_capacity(aero_net_e1000::MIN_L2_FRAME_LEN);
        frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, tag]);
        frame.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0xFF]);
        frame.extend_from_slice(&0x0800u16.to_be_bytes());
        debug_assert!(frame.len() >= aero_net_e1000::MIN_L2_FRAME_LEN);
        frame
    }

    fn write_tx_desc(mem: &mut TestMem, desc_addr: u64, buf_addr: u64, len: u16, cmd: u8) {
        let mut desc = [0u8; 16];
        desc[0..8].copy_from_slice(&buf_addr.to_le_bytes());
        desc[8..10].copy_from_slice(&len.to_le_bytes());
        desc[10] = 0; // cso
        desc[11] = cmd;
        desc[12] = 0; // status
        desc[13] = 0; // css
        desc[14..16].copy_from_slice(&0u16.to_le_bytes());
        mem.write(desc_addr, &desc);
    }

    fn write_rx_desc(mem: &mut TestMem, desc_addr: u64, buf_addr: u64) {
        let mut desc = [0u8; 16];
        desc[0..8].copy_from_slice(&buf_addr.to_le_bytes());
        // remaining fields zero
        mem.write(desc_addr, &desc);
    }

    #[derive(Default, Debug)]
    struct VecBackend {
        tx: Vec<Vec<u8>>,
        rx: VecDeque<Vec<u8>>,
        respond_on_transmit: bool,
    }

    impl VecBackend {
        fn push_rx(&mut self, frame: Vec<u8>) {
            self.rx.push_back(frame);
        }
    }

    impl NetworkBackend for VecBackend {
        fn transmit(&mut self, frame: Vec<u8>) {
            if self.respond_on_transmit {
                // Echo-ish response with distinct destination tag so tests can tell it apart.
                let mut resp = frame.clone();
                if !resp.is_empty() {
                    resp[5] ^= 0xAA;
                }
                self.rx.push_back(resp);
            }
            self.tx.push(frame);
        }

        fn poll_receive(&mut self) -> Option<Vec<u8>> {
            self.rx.pop_front()
        }
    }

    fn setup_tx_ring(nic: &mut E1000Device, mem: &mut TestMem, frames: &[Vec<u8>]) {
        let ring_base = 0x1000u64;
        let desc_count = 4u32;
        let tdl = 16u32 * desc_count;

        nic.mmio_write_u32(mem, REG_TDBAL, ring_base as u32);
        nic.mmio_write_u32(mem, REG_TDBAH, 0);
        nic.mmio_write_u32(mem, REG_TDLEN, tdl);
        nic.mmio_write_u32(mem, REG_TDH, 0);

        // Leave TCTL disabled until after we've set TDT, so `tick()`'s initial `nic.poll()`
        // is responsible for publishing TX frames.
        nic.mmio_write_u32(mem, REG_TCTL, 0);

        for (i, frame) in frames.iter().enumerate() {
            let buf_addr = 0x2000u64 + (i as u64) * 0x100;
            mem.write(buf_addr, frame);
            write_tx_desc(
                mem,
                ring_base + (i as u64) * 16,
                buf_addr,
                frame.len() as u16,
                TXD_CMD_EOP | TXD_CMD_RS,
            );
        }

        nic.mmio_write_u32(mem, REG_TDT, frames.len() as u32);
        nic.mmio_write_u32(mem, REG_TCTL, TCTL_EN);
    }

    fn setup_rx_ring(nic: &mut E1000Device, mem: &mut TestMem, desc_count: u32) -> u64 {
        assert!(desc_count >= 2, "need at least 2 RX descriptors");
        let ring_base = 0x3000u64;
        let rdlen = 16u32 * desc_count;

        nic.mmio_write_u32(mem, REG_RDBAL, ring_base as u32);
        nic.mmio_write_u32(mem, REG_RDBAH, 0);
        nic.mmio_write_u32(mem, REG_RDLEN, rdlen);
        nic.mmio_write_u32(mem, REG_RDH, 0);
        nic.mmio_write_u32(mem, REG_RDT, desc_count - 1);
        nic.mmio_write_u32(mem, REG_RCTL, RCTL_EN);

        for i in 0..desc_count {
            let buf_addr = 0x4000u64 + (i as u64) * 0x100;
            write_rx_desc(mem, ring_base + (i as u64) * 16, buf_addr);
        }

        ring_base
    }

    #[test]
    fn tx_forwarding_guest_frames_reach_backend() {
        let mut mem = TestMem::new(0x20_000);
        let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let mut backend = VecBackend::default();

        let frame = build_frame(0x01);
        setup_tx_ring(&mut nic, &mut mem, &[frame.clone()]);

        let mut pump = E1000Pump::new(16, 16);
        pump.tick(&mut nic, &mut mem, &mut backend);

        assert_eq!(backend.tx, vec![frame]);
    }

    #[test]
    fn rx_injection_backend_frames_written_to_guest_buffers() {
        let mut mem = TestMem::new(0x20_000);
        let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let mut backend = VecBackend::default();
        let ring_base = setup_rx_ring(&mut nic, &mut mem, 2);

        let frame = build_frame(0x10);
        backend.push_rx(frame.clone());

        let mut pump = E1000Pump::new(0, 16);
        pump.tick(&mut nic, &mut mem, &mut backend);

        // Frame should be DMA-written to the first RX buffer (descriptor 0).
        let buf_addr = 0x4000u64;
        assert_eq!(mem.read_vec(buf_addr, frame.len()), frame);

        // Descriptor 0 should be marked done with correct length.
        let desc0 = mem.read_vec(ring_base, 16);
        let len = u16::from_le_bytes(desc0[8..10].try_into().unwrap()) as usize;
        let status = desc0[12];
        assert_eq!(len, frame.len());
        assert_eq!(status & (RXD_STAT_DD | RXD_STAT_EOP), RXD_STAT_DD | RXD_STAT_EOP);
    }

    #[test]
    fn budget_enforcement_limits_frames_per_tick_and_preserves_rest() {
        let mut mem = TestMem::new(0x40_000);
        let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let mut backend = VecBackend::default();

        let tx1 = build_frame(0x01);
        let tx2 = build_frame(0x02);
        setup_tx_ring(&mut nic, &mut mem, &[tx1.clone(), tx2.clone()]);

        setup_rx_ring(&mut nic, &mut mem, 4);
        let rx1 = build_frame(0xA1);
        let rx2 = build_frame(0xA2);
        backend.push_rx(rx1.clone());
        backend.push_rx(rx2.clone());

        let mut pump = E1000Pump::new(1, 1);

        pump.tick(&mut nic, &mut mem, &mut backend);
        assert_eq!(backend.tx, vec![tx1.clone()]);
        assert_eq!(backend.rx.len(), 1, "one RX frame should remain in backend queue");

        pump.tick(&mut nic, &mut mem, &mut backend);
        assert_eq!(backend.tx, vec![tx1, tx2]);
        assert_eq!(backend.rx.len(), 0);
    }

    #[test]
    fn same_tick_backend_response_is_delivered_to_guest_rx() {
        let mut mem = TestMem::new(0x40_000);
        let mut nic = E1000Device::new([0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let mut backend = VecBackend {
            respond_on_transmit: true,
            ..VecBackend::default()
        };

        let tx_frame = build_frame(0x33);
        setup_tx_ring(&mut nic, &mut mem, &[tx_frame.clone()]);
        setup_rx_ring(&mut nic, &mut mem, 2);

        let mut pump = E1000Pump::new(8, 8);
        pump.tick(&mut nic, &mut mem, &mut backend);

        // Backend should have observed the guest TX.
        assert_eq!(backend.tx, vec![tx_frame.clone()]);

        // And its generated response should have been DMA-written into the guest RX buffer in
        // the same tick.
        let mut expected_resp = tx_frame;
        expected_resp[5] ^= 0xAA;
        assert_eq!(mem.read_vec(0x4000u64, expected_resp.len()), expected_resp);
    }
}

