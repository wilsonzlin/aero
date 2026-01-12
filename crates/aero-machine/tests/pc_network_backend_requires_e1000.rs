use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aero_machine::pc::PcMachine;
use aero_net_backend::NetworkBackend;

#[derive(Default)]
struct BackendState {
    poll_receive_calls: usize,
    rx_queue: VecDeque<Vec<u8>>,
}

struct CountingBackend {
    state: Rc<RefCell<BackendState>>,
}

impl NetworkBackend for CountingBackend {
    fn transmit(&mut self, _frame: Vec<u8>) {}

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        let mut state = self.state.borrow_mut();
        state.poll_receive_calls += 1;
        state.rx_queue.pop_front()
    }
}

#[test]
fn pc_machine_poll_network_does_not_poll_backend_when_e1000_is_disabled() {
    // `PcMachine::new` intentionally disables E1000 to keep the baseline PC machine minimal.
    let mut pc = PcMachine::new(2 * 1024 * 1024);
    assert!(!pc.bus.platform.has_e1000());

    let state = Rc::new(RefCell::new(BackendState::default()));
    state
        .borrow_mut()
        .rx_queue
        .push_back(vec![0xAA, 0xBB, 0xCC]);

    pc.set_network_backend(Box::new(CountingBackend {
        state: state.clone(),
    }));

    pc.poll_network();

    let state = state.borrow();
    assert_eq!(
        state.poll_receive_calls, 0,
        "backend must not be drained when there is no NIC to deliver frames into"
    );
    assert_eq!(
        state.rx_queue.len(),
        1,
        "frames must remain queued in the backend when E1000 is disabled"
    );
}
