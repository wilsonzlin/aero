//! Adversarial xHCI tests that exercise guest-controlled ring pointer paths.
//!
//! These are primarily regression tests against guest-induced hangs (infinite loops / unbounded
//! work) in command and transfer ring processing.

use aero_usb::xhci::command_ring::{CommandRing, CommandRingProcessor, EventRing};
use aero_usb::xhci::transfer::Ep0TransferEngine;
use aero_usb::xhci::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use aero_usb::xhci::{regs, XhciController};
use aero_usb::{ControlResponse, MemoryBus, SetupPacket, UsbDeviceModel};

#[derive(Clone)]
struct CountingMem {
    data: Vec<u8>,
    reads: usize,
    writes: usize,
    max_reads: usize,
    max_writes: usize,
}

impl CountingMem {
    fn new(size: usize, max_reads: usize, max_writes: usize) -> Self {
        Self {
            data: vec![0; size],
            reads: 0,
            writes: 0,
            max_reads,
            max_writes,
        }
    }
}

impl MemoryBus for CountingMem {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        self.reads += 1;
        assert!(
            self.reads <= self.max_reads,
            "read budget exceeded ({} > {})",
            self.reads,
            self.max_reads
        );

        let Ok(start) = usize::try_from(paddr) else {
            buf.fill(0);
            return;
        };
        let end = start.saturating_add(buf.len());
        if end > self.data.len() {
            buf.fill(0);
            return;
        }
        buf.copy_from_slice(&self.data[start..end]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.writes += 1;
        assert!(
            self.writes <= self.max_writes,
            "write budget exceeded ({} > {})",
            self.writes,
            self.max_writes
        );

        let Ok(start) = usize::try_from(paddr) else {
            return;
        };
        let end = start.saturating_add(buf.len());
        if end > self.data.len() {
            return;
        }
        self.data[start..end].copy_from_slice(buf);
    }
}

fn self_referential_link_trb(addr: u64) -> Trb {
    let mut trb = Trb::new(addr, 0, 0);
    trb.set_cycle(true);
    trb.set_trb_type(TrbType::Link);
    trb.set_link_toggle_cycle(false);
    trb
}

#[test]
fn command_ring_processor_self_link_sets_hce_and_is_bounded() {
    let mut mem = CountingMem::new(0x10_000, 32, 32);
    let mem_size = mem.data.len() as u64;

    let ring_base = 0x1000u64;
    let event_ring_base = 0x2000u64;

    self_referential_link_trb(ring_base).write_to(&mut mem, ring_base);

    let mut proc = CommandRingProcessor::new(
        mem_size,
        8,
        0x3000, // dcbaa (unused by this test)
        CommandRing {
            dequeue_ptr: ring_base,
            cycle_state: true,
        },
        EventRing::new(event_ring_base, 16),
    );

    // A buggy caller could pass an enormous max_trbs value. This must not hang.
    proc.process(&mut mem, usize::MAX);
    assert!(
        proc.host_controller_error,
        "expected command ring HCE on link loop"
    );
}

#[test]
fn xhci_controller_command_ring_self_link_sets_hce() {
    // `RingCursor::poll` uses a step budget of 256 in `XhciController::process_command_ring`, so
    // allow a little headroom.
    let mut mem = CountingMem::new(0x10_000, 300, 16);
    let ring_base = 0x1000u64;

    self_referential_link_trb(ring_base).write_to(&mut mem, ring_base);

    let mut xhci = XhciController::new();
    xhci.set_command_ring(ring_base, true);

    xhci.process_command_ring(&mut mem, usize::MAX);

    let sts = xhci.mmio_read(&mut mem, regs::REG_USBSTS, 4);
    assert_ne!(sts & regs::USBSTS_HCE, 0, "controller should latch HCE");
}

#[derive(Default)]
struct DummyDevice;

impl UsbDeviceModel for DummyDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }
}

#[test]
fn ep0_transfer_engine_self_link_faults_and_emits_event() {
    let mut mem = CountingMem::new(0x20_000, 64, 64);

    let tr_ring = 0x1000u64;
    let event_ring = 0x2000u64;

    // Ring consists of a single Link TRB pointing to itself.
    self_referential_link_trb(tr_ring).write_to(&mut mem, tr_ring);

    let mut xhci = Ep0TransferEngine::new_with_ports(1);
    xhci.set_event_ring(event_ring, 8);
    xhci.hub_mut().attach(0, Box::new(DummyDevice::default()));

    let slot_id = xhci.enable_slot(0).expect("slot allocation");
    assert!(xhci.configure_ep0(slot_id, tr_ring, true, 64));

    // Doorbell should not hang; it should fault the endpoint and emit a Transfer Event with
    // TRB Error.
    xhci.ring_doorbell(&mut mem, slot_id, 1);

    let ev = Trb::read_from(&mut mem, event_ring);
    assert_eq!(ev.trb_type(), TrbType::TransferEvent);
    assert_eq!(ev.completion_code_raw(), CompletionCode::TrbError.as_u8());
    assert_eq!(ev.parameter, tr_ring);
    // Ensure the event ring advanced.
    let next = Trb::read_from(&mut mem, event_ring + TRB_LEN as u64);
    assert!(
        !next.cycle(),
        "second event ring entry should still be empty"
    );
}

#[test]
fn ep0_transfer_engine_data_stage_work_is_bounded_per_doorbell() {
    // Control DATA transfers are packetized and can be guest-amplified by choosing a tiny
    // max-packet-size. Ensure we bound the amount of per-call work so a single doorbell can't
    // monopolize the CPU.
    struct LargeInDevice;

    impl UsbDeviceModel for LargeInDevice {
        fn handle_control_request(
            &mut self,
            setup: SetupPacket,
            _data_stage: Option<&[u8]>,
        ) -> ControlResponse {
            ControlResponse::Data(vec![0xAB; setup.w_length as usize])
        }
    }

    let mut mem = CountingMem::new(0x40_000, 64, 300);

    let tr_ring = 0x1000u64;
    let buf = 0x4000u64;

    let setup = SetupPacket {
        bm_request_type: 0xc0, // DeviceToHost | Vendor | Device
        b_request: 0x01,
        w_value: 0,
        w_index: 0,
        w_length: 4096,
    };
    let setup_bytes = [
        setup.bm_request_type,
        setup.b_request,
        setup.w_value as u8,
        (setup.w_value >> 8) as u8,
        setup.w_index as u8,
        (setup.w_index >> 8) as u8,
        setup.w_length as u8,
        (setup.w_length >> 8) as u8,
    ];

    let mut setup_trb = Trb::new(u64::from_le_bytes(setup_bytes), 0, 0);
    setup_trb.set_cycle(true);
    setup_trb.set_trb_type(TrbType::SetupStage);
    setup_trb.write_to(&mut mem, tr_ring);

    let mut data_trb = Trb::new(buf, setup.w_length as u32, 0);
    data_trb.set_cycle(true);
    data_trb.set_trb_type(TrbType::DataStage);
    data_trb.set_dir_in(true);
    data_trb.write_to(&mut mem, tr_ring + TRB_LEN as u64);

    let mut status_trb = Trb::new(0, 0, 0);
    status_trb.set_cycle(true);
    status_trb.set_trb_type(TrbType::StatusStage);
    status_trb.set_dir_in(false);
    status_trb.write_to(&mut mem, tr_ring + 2 * TRB_LEN as u64);

    let mut xhci = Ep0TransferEngine::new_with_ports(1);
    xhci.hub_mut().attach(0, Box::new(LargeInDevice));

    let slot_id = xhci.enable_slot(0).expect("slot allocation");
    assert!(xhci.configure_ep0(slot_id, tr_ring, true, 8));

    // A single doorbell should not transfer the entire payload. Prior implementations processed the
    // full DATA stage packet-by-packet in one call; this would perform 512 writes (4096/8) and blow
    // the write budget above. With a bounded per-call packet budget, we make partial progress and
    // retry on a later tick.
    xhci.ring_doorbell(&mut mem, slot_id, 1);

    let start = buf as usize;
    // Ensure we made at least some progress.
    assert_eq!(mem.data[start], 0xAB);
    // But we should not have completed the entire DATA stage in one call.
    assert_eq!(mem.data[start + setup.w_length as usize - 1], 0);
}
