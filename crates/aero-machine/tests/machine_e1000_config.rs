use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aero_machine::{Machine, MachineConfig, MachineError};
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
fn e1000_requires_pc_platform() {
    let err = match Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: false,
        enable_e1000: true,
        ..Default::default()
    }) {
        Ok(_) => panic!("expected Machine::new to fail when enable_e1000=true but enable_pc_platform=false"),
        Err(err) => err,
    };

    assert_eq!(err, MachineError::E1000RequiresPcPlatform);
}

#[test]
fn poll_network_does_not_poll_backend_when_e1000_is_disabled() {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_e1000: false,
        // Keep the machine minimal and deterministic for this unit test.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        ..Default::default()
    })
    .unwrap();

    let state = Rc::new(RefCell::new(BackendState::default()));
    state
        .borrow_mut()
        .rx_queue
        .push_back(vec![0xAA, 0xBB, 0xCC]);

    m.set_network_backend(Box::new(CountingBackend {
        state: state.clone(),
    }));

    m.poll_network();

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
