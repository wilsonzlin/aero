use super::*;

#[test]
fn hub_selective_suspend_remote_wakeup_resumes_port_and_latches_suspend_change() {
    #[derive(Default)]
    struct WakeState {
        suspended: bool,
        wake_pending: bool,
        polls: u32,
    }

    #[derive(Clone)]
    struct WakeDevice(Rc<RefCell<WakeState>>);

    impl WakeDevice {
        fn new() -> Self {
            Self(Rc::new(RefCell::new(WakeState::default())))
        }

        fn request_wake(&self) {
            self.0.borrow_mut().wake_pending = true;
        }

        fn suspended(&self) -> bool {
            self.0.borrow().suspended
        }

        fn wake_pending(&self) -> bool {
            self.0.borrow().wake_pending
        }

        fn polls(&self) -> u32 {
            self.0.borrow().polls
        }
    }

    impl UsbDeviceModel for WakeDevice {
        fn handle_control_request(
            &mut self,
            _setup: SetupPacket,
            _data_stage: Option<&[u8]>,
        ) -> ControlResponse {
            ControlResponse::Ack
        }

        fn set_suspended(&mut self, suspended: bool) {
            self.0.borrow_mut().suspended = suspended;
        }

        fn poll_remote_wakeup(&mut self) -> bool {
            let mut st = self.0.borrow_mut();
            st.polls += 1;
            if st.suspended && st.wake_pending {
                st.wake_pending = false;
                true
            } else {
                false
            }
        }
    }

    let wake = WakeDevice::new();
    let mut hub = UsbHubDevice::new_with_ports(1);
    hub.configuration = 1;
    hub.attach(1, Box::new(wake.clone()));
    hub.ports[0].set_powered(true);
    hub.ports[0].set_enabled(true);

    // Selectively suspend the downstream port.
    hub.ports[0].set_suspended(true);
    let upstream_suspended = hub.upstream_suspended;
    let port_suspended = hub.ports[0].suspended;
    if let Some(dev) = hub.ports[0].device.as_mut() {
        dev.model_mut()
            .set_suspended(upstream_suspended || port_suspended);
    }

    // Clear the suspend-change latch so we can assert it gets set again by remote wake.
    hub.ports[0].suspend_change = false;
    assert!(hub.ports[0].suspended);
    assert!(wake.suspended());

    wake.request_wake();
    assert!(wake.wake_pending());
    assert_eq!(wake.polls(), 0);

    UsbDeviceModel::tick_1ms(&mut hub);

    assert_eq!(
        wake.polls(),
        1,
        "expected hub to poll remote wakeup while port is suspended"
    );
    assert!(
        !wake.wake_pending(),
        "expected remote wake request to be drained when serviced"
    );
    assert!(
        !hub.ports[0].suspended,
        "expected remote wakeup to clear selective suspend on the port"
    );
    assert!(
        hub.ports[0].suspend_change,
        "expected hub to latch C_PORT_SUSPEND after a remote wake resume"
    );
    assert!(
        !wake.suspended(),
        "expected downstream device to observe resume after remote wake"
    );
}
