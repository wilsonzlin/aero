mod util;

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use aero_usb::xhci::context::SlotContext;
use aero_usb::xhci::transfer::{write_trb, Trb, TrbType};
use aero_usb::xhci::{CommandCompletionCode, XhciController};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult, UsbOutResult};

use util::{xhci_set_run, Alloc, TestMemory};

fn make_empty_normal_trb(cycle: bool) -> Trb {
    let mut dword3 = 0u32;
    if cycle {
        dword3 |= 1;
    }
    dword3 |= (u32::from(TrbType::Normal.raw())) << 10;
    Trb::from_dwords(0, 0, 0, dword3)
}

fn ep_addr_from_endpoint_id(endpoint_id: u8) -> u8 {
    let ep_num = endpoint_id / 2;
    let is_in = (endpoint_id & 1) != 0;
    ep_num | if is_in { 0x80 } else { 0x00 }
}

#[derive(Clone)]
struct CountingNakDevice {
    called: Rc<RefCell<HashSet<u8>>>,
}

impl UsbDeviceModel for CountingNakDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_in_transfer(&mut self, ep_addr: u8, _max_len: usize) -> UsbInResult {
        self.called.borrow_mut().insert(ep_addr);
        UsbInResult::Nak
    }

    fn handle_out_transfer(&mut self, ep_addr: u8, _data: &[u8]) -> UsbOutResult {
        self.called.borrow_mut().insert(ep_addr);
        UsbOutResult::Nak
    }
}

#[test]
fn xhci_tick_round_robins_active_endpoints_to_avoid_starvation() {
    let mut mem = TestMemory::new(0x200_000);
    let mut alloc = Alloc::new(0x1000);

    let dcbaa = alloc.alloc(0x200, 0x40) as u64;

    // A single root port with an external hub gives us enough topology to bind multiple slots.
    let mut xhci = XhciController::with_port_count(1);
    xhci.set_dcbaap(dcbaa);
    xhci.attach_hub(0, 15).expect("attach hub");

    // Attach 9 devices behind the hub (enough slots to exceed MAX_TRBS_PER_TICK when we ring all
    // non-control endpoints).
    let mut call_sets: Vec<Rc<RefCell<HashSet<u8>>>> = Vec::new();
    for hub_port in 1u8..=9 {
        let calls = Rc::new(RefCell::new(HashSet::new()));
        call_sets.push(calls.clone());
        xhci.attach_at_path(
            &[0, hub_port],
            Box::new(CountingNakDevice { called: calls }),
        )
        .expect("attach device");
    }

    // Drop any Port Status Change events so they don't affect the test.
    while xhci.pop_pending_event().is_some() {}

    xhci_set_run(&mut xhci);
    let mut slot_ids = Vec::new();
    for hub_port in 1u8..=9 {
        let completion = xhci.enable_slot(&mut mem);
        assert_eq!(
            completion.completion_code,
            CommandCompletionCode::Success,
            "enable slot"
        );
        let slot_id = completion.slot_id;
        slot_ids.push(slot_id);

        let mut slot_ctx = SlotContext::default();
        slot_ctx.set_root_hub_port_number(1);
        slot_ctx
            .set_route_string_from_root_ports(&[hub_port])
            .expect("route string");
        let completion = xhci.address_device(slot_id, slot_ctx);
        assert_eq!(
            completion.completion_code,
            CommandCompletionCode::Success,
            "address device"
        );
    }

    // Configure and ring all non-control endpoints (DCI 2..=31) for each slot. Each endpoint has a
    // single Normal TRB with a matching cycle bit so the controller considers it runnable.
    for &slot_id in &slot_ids {
        for endpoint_id in 2u8..=31 {
            let trb_addr = alloc.alloc(16, 0x10) as u64;
            write_trb(&mut mem, trb_addr, make_empty_normal_trb(true));
            xhci.set_endpoint_ring(slot_id, endpoint_id, trb_addr, true);
            xhci.ring_doorbell(slot_id, endpoint_id);
        }
    }

    // Tick once: budget is 256 endpoints per tick, but we activated 9 * 30 = 270 endpoints. The
    // last slot's final endpoints will not be processed on the first tick.
    xhci.tick(&mut mem);

    let calls_after_first = call_sets[8].borrow().clone();
    let mut expected_first = HashSet::new();
    for endpoint_id in 2u8..=17 {
        expected_first.insert(ep_addr_from_endpoint_id(endpoint_id));
    }
    assert_eq!(calls_after_first, expected_first);

    // Tick again: the controller should rotate the active endpoint list so the endpoints that were
    // skipped due to budget exhaustion are processed.
    xhci.tick(&mut mem);

    let calls_after_second = call_sets[8].borrow().clone();
    let mut expected_all = HashSet::new();
    for endpoint_id in 2u8..=31 {
        expected_all.insert(ep_addr_from_endpoint_id(endpoint_id));
    }
    assert_eq!(calls_after_second, expected_all);
}
