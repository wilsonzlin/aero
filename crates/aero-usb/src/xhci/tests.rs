use super::command_ring::{CommandRing, CommandRingProcessor, EventRing};
use super::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use crate::MemoryBus;

struct TestMem {
    data: Vec<u8>,
}

impl TestMem {
    fn new(size: usize) -> Self {
        Self { data: vec![0; size] }
    }

    fn len(&self) -> usize {
        self.data.len()
    }

    fn write_u32(&mut self, addr: u64, value: u32) {
        let addr = addr as usize;
        self.data[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u64(&mut self, addr: u64, value: u64) {
        let addr = addr as usize;
        self.data[addr..addr + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn read_trb(&mut self, addr: u64) -> Trb {
        let mut bytes = [0u8; TRB_LEN];
        self.read_physical(addr, &mut bytes);
        Trb::from_bytes(bytes)
    }

    fn write_trb(&mut self, addr: u64, trb: Trb) {
        self.write_physical(addr, &trb.to_bytes());
    }
}

impl MemoryBus for TestMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let start = paddr as usize;
        let end = start.saturating_add(buf.len());
        if end > self.data.len() {
            // Out-of-bounds reads return all-zeros.
            buf.fill(0);
            return;
        }
        buf.copy_from_slice(&self.data[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let start = paddr as usize;
        let end = start.saturating_add(buf.len());
        if end > self.data.len() {
            // Ignore out-of-bounds writes.
            return;
        }
        self.data[start..end].copy_from_slice(buf);
    }
}

fn event_completion_code(ev: Trb) -> u8 {
    (ev.status >> 24) as u8
}

#[test]
fn noop_and_evaluate_context_emit_events_and_update_ep0_mps() {
    let mut mem = TestMem::new(0x20_000);
    let mem_size = mem.len() as u64;

    // All xHCI context pointers are 64-byte aligned in our model.
    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;
    let input_ctx = 0x3000u64;
    let cmd_ring = 0x4000u64;
    let event_ring = 0x5000u64;

    let max_slots = 8;

    // DCBAA[1] -> dev_ctx.
    mem.write_u64(dcbaa + 8, dev_ctx);

    // Device context EP0 starts with MPS=8.
    // Endpoint Context dword 1 bits 16..31 = Max Packet Size.
    mem.write_u32(dev_ctx + 0x20 + 4, 8u32 << 16);

    // Input control context: Drop=0, Add = Slot + EP0.
    mem.write_u32(input_ctx + 0x00, 0);
    mem.write_u32(input_ctx + 0x04, (1 << 0) | (1 << 1));

    // Input EP0 context requests MPS=64 and Interval=5.
    mem.write_u32(input_ctx + 0x40 + 0, 5u32 << 16);
    mem.write_u32(input_ctx + 0x40 + 4, 64u32 << 16);
    // TR Dequeue Pointer: copy-through field; choose an arbitrary aligned value.
    mem.write_u32(input_ctx + 0x40 + 8, 0xdead_bee0);
    mem.write_u32(input_ctx + 0x40 + 12, 0x0000_0000);

    // Command ring:
    //  - TRB0: No-Op Command
    //  - TRB1: Evaluate Context (slot 1, input_ctx)
    //  - TRB2: Link back to cmd_ring base, toggle cycle state
    {
        let mut trb0 = Trb::new(0, 0, 0);
        trb0.set_trb_type(TrbType::NoOpCommand);
        trb0.set_slot_id(0);
        trb0.set_cycle(true);
        mem.write_trb(cmd_ring + 0 * 16, trb0);
    }
    {
        let mut trb1 = Trb::new(input_ctx, 0, 0);
        trb1.set_trb_type(TrbType::EvaluateContextCommand);
        trb1.set_slot_id(1);
        trb1.set_cycle(true);
        mem.write_trb(cmd_ring + 1 * 16, trb1);
    }
    {
        let mut link = Trb::new(cmd_ring & !0x0f, 0, 0);
        link.set_trb_type(TrbType::Link);
        link.set_link_toggle_cycle(true);
        link.set_cycle(true);
        mem.write_trb(cmd_ring + 2 * 16, link);
    }

    let processor = CommandRingProcessor::new(
        mem_size,
        max_slots,
        dcbaa,
        CommandRing {
            dequeue_ptr: cmd_ring,
            cycle_state: true,
        },
        EventRing::new(event_ring, 16),
    );
    let mut processor = processor;

    processor.process(&mut mem, 16);
    assert!(
        !processor.host_controller_error,
        "processor should not enter HCE"
    );

    // Link TRBs are consumed but do not generate events; we should have exactly two command
    // completion events.
    let ev0 = mem.read_trb(event_ring + 0 * 16);
    let ev1 = mem.read_trb(event_ring + 1 * 16);

    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(ev1.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev0), CompletionCode::Success.as_u8());
    assert_eq!(event_completion_code(ev1), CompletionCode::Success.as_u8());
    assert_eq!(ev0.pointer(), cmd_ring + 0 * 16);
    assert_eq!(ev1.pointer(), cmd_ring + 1 * 16);

    // EP0 max packet size should have been updated to 64.
    let mut buf = [0u8; 4];
    mem.read_physical(dev_ctx + 0x20 + 4, &mut buf);
    let out_dw1 = u32::from_le_bytes(buf);
    assert_eq!((out_dw1 >> 16) & 0xffff, 64);

    // Command ring should have followed the Link TRB and toggled the consumer cycle state.
    assert_eq!(processor.command_ring.dequeue_ptr, cmd_ring);
    assert_eq!(processor.command_ring.cycle_state, false);

    // Simulate the guest adding another No-Op command after the cycle-state toggle by overwriting
    // TRB0 with cycle=0.
    {
        let mut trb0 = Trb::new(0, 0, 0);
        trb0.set_trb_type(TrbType::NoOpCommand);
        trb0.set_slot_id(0);
        trb0.set_cycle(false);
        mem.write_trb(cmd_ring + 0 * 16, trb0);
    }

    processor.process(&mut mem, 16);
    assert!(
        !processor.host_controller_error,
        "processor should not enter HCE after wrap"
    );

    let ev2 = mem.read_trb(event_ring + 2 * 16);
    assert_eq!(ev2.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev2), CompletionCode::Success.as_u8());
    assert_eq!(ev2.pointer(), cmd_ring + 0 * 16);
}

#[test]
fn evaluate_context_rejects_unsupported_context_flags() {
    let mut mem = TestMem::new(0x20_000);
    let mem_size = mem.len() as u64;

    let dcbaa = 0x1000u64;
    let dev_ctx = 0x2000u64;
    let input_ctx = 0x3000u64;
    let cmd_ring = 0x4000u64;
    let event_ring = 0x5000u64;

    mem.write_u64(dcbaa + 8, dev_ctx);
    mem.write_u32(dev_ctx + 0x20 + 4, 8u32 << 16);

    // Add EP0 + EP1 (unsupported).
    mem.write_u32(input_ctx + 0x00, 0);
    mem.write_u32(input_ctx + 0x04, (1 << 1) | (1 << 2));
    mem.write_u32(input_ctx + 0x40 + 4, 64u32 << 16);

    {
        let mut cmd = Trb::new(input_ctx, 0, 0);
        cmd.set_trb_type(TrbType::EvaluateContextCommand);
        cmd.set_slot_id(1);
        cmd.set_cycle(true);
        mem.write_trb(cmd_ring, cmd);
    }

    let mut processor = CommandRingProcessor::new(
        mem_size,
        8,
        dcbaa,
        CommandRing {
            dequeue_ptr: cmd_ring,
            cycle_state: true,
        },
        EventRing::new(event_ring, 16),
    );

    processor.process(&mut mem, 16);
    assert!(!processor.host_controller_error);

    let ev0 = mem.read_trb(event_ring);
    assert_eq!(ev0.trb_type(), TrbType::CommandCompletionEvent);
    assert_eq!(event_completion_code(ev0), CompletionCode::ParameterError.as_u8());

    // Ensure we didn't update EP0 despite the input asking for MPS=64.
    let mut buf = [0u8; 4];
    mem.read_physical(dev_ctx + 0x20 + 4, &mut buf);
    let out_dw1 = u32::from_le_bytes(buf);
    assert_eq!((out_dw1 >> 16) & 0xffff, 8);
}
