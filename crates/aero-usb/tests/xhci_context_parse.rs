use aero_usb::xhci::context::{EndpointType, InputContext32};
use aero_usb::MemoryBus;

struct TestMemoryBus {
    mem: Vec<u8>,
}

impl TestMemoryBus {
    fn new(size: usize) -> Self {
        Self { mem: vec![0; size] }
    }

    fn write(&mut self, addr: u64, data: &[u8]) {
        let addr = addr as usize;
        self.mem[addr..addr + data.len()].copy_from_slice(data);
    }

    fn write_u32(&mut self, addr: u64, value: u32) {
        self.write(addr, &value.to_le_bytes());
    }
}

impl MemoryBus for TestMemoryBus {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let addr = paddr as usize;
        buf.copy_from_slice(&self.mem[addr..addr + buf.len()]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let addr = paddr as usize;
        self.mem[addr..addr + buf.len()].copy_from_slice(buf);
    }
}

#[test]
fn parse_input_context_slot_and_endpoint_context() {
    let mut mem = TestMemoryBus::new(0x4000);

    let input_ctx_base = 0x1000u64;

    // Input Control Context (32 bytes): Drop=0, Add Slot (bit0) + Endpoint ID 3 (bit3).
    mem.write_u32(input_ctx_base + 0, 0);
    mem.write_u32(input_ctx_base + 4, (1 << 0) | (1 << 3));

    // Slot Context at index 1.
    // Root Hub Port Number lives in DWORD1 bits 7:0.
    let slot_ctx = input_ctx_base + 0x20;
    let speed_id = 3u32; // High speed (value doesn't matter for this test).
    let context_entries = 3u32;
    mem.write_u32(slot_ctx + 0, (speed_id << 20) | (context_entries << 27));
    mem.write_u32(slot_ctx + 4, 5); // port = 5

    // Endpoint Context for Device Context index 3 (Endpoint 1 IN, typically).
    let ep_ctx = input_ctx_base + 0x20 * 4; // Input ctx index = device idx + 1 => 3 + 1 = 4.
    let ep_type_raw = 6u32; // Bulk IN.
    let max_packet_size = 512u32;
    mem.write_u32(ep_ctx + 4, (ep_type_raw << 3) | (max_packet_size << 16));

    // TR Dequeue Pointer is in DWORD2/3. Bit0 = DCS, bits 63:4 are pointer.
    let tr_dequeue_ptr = 0x2000u64;
    let tr_dequeue_with_dcs = tr_dequeue_ptr | 1;
    mem.write_u32(ep_ctx + 8, tr_dequeue_with_dcs as u32);
    mem.write_u32(ep_ctx + 12, (tr_dequeue_with_dcs >> 32) as u32);

    let ictx = InputContext32::new(input_ctx_base);
    let slot = ictx.slot_context(&mut mem).expect("slot ctx");
    assert_eq!(slot.root_hub_port_number(), 5);

    let ep = ictx.endpoint_context(&mut mem, 3).expect("endpoint ctx");
    assert_eq!(ep.endpoint_type(), EndpointType::BulkIn);
    assert_eq!(ep.tr_dequeue_pointer(), tr_dequeue_ptr);
    assert!(ep.dcs());
}
