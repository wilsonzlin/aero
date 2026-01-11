use std::cell::Cell;
use std::rc::Rc;

use emulator::io::usb::core::{AttachedUsbDevice, UsbInResult, UsbOutResult};
use emulator::io::usb::hub::{RootHub, UsbHub};
use emulator::io::usb::{ControlResponse, SetupPacket, UsbDeviceModel};

#[derive(Clone)]
struct TestHubControl {
    port_enabled: Vec<Rc<Cell<bool>>>,
}

impl TestHubControl {
    fn set_port_enabled(&self, port: usize, enabled: bool) {
        self.port_enabled[port].set(enabled);
    }
}

struct TestHubPort {
    device: Option<AttachedUsbDevice>,
    enabled: Rc<Cell<bool>>,
}

struct TestHub {
    ports: Vec<TestHubPort>,
}

impl TestHub {
    fn new(num_ports: usize) -> (Self, TestHubControl) {
        let mut ports = Vec::with_capacity(num_ports);
        let mut enabled = Vec::with_capacity(num_ports);
        for _ in 0..num_ports {
            let flag = Rc::new(Cell::new(false));
            enabled.push(flag.clone());
            ports.push(TestHubPort {
                device: None,
                enabled: flag,
            });
        }

        (Self { ports }, TestHubControl { port_enabled: enabled })
    }
}

impl UsbHub for TestHub {
    fn tick_1ms(&mut self) {
        for p in &mut self.ports {
            if !p.enabled.get() {
                continue;
            }
            if let Some(dev) = p.device.as_mut() {
                if let Some(hub) = dev.as_hub_mut() {
                    hub.tick_1ms();
                }
            }
        }
    }

    fn downstream_device_mut_for_address(&mut self, address: u8) -> Option<&mut AttachedUsbDevice> {
        for p in &mut self.ports {
            if !p.enabled.get() {
                continue;
            }
            let Some(dev) = p.device.as_mut() else {
                continue;
            };
            if dev.address() == address {
                return Some(dev);
            }
            if let Some(hub) = dev.as_hub_mut() {
                if let Some(found) = hub.downstream_device_mut_for_address(address) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn downstream_device_mut(&mut self, port: usize) -> Option<&mut AttachedUsbDevice> {
        self.ports.get_mut(port)?.device.as_mut()
    }

    fn attach_downstream(&mut self, port: usize, model: Box<dyn UsbDeviceModel>) {
        self.ports[port].device = Some(AttachedUsbDevice::new(model));
    }

    fn detach_downstream(&mut self, port: usize) {
        self.ports[port].device = None;
    }

    fn num_ports(&self) -> usize {
        self.ports.len()
    }
}

impl UsbDeviceModel for TestHub {
    fn get_device_descriptor(&self) -> &[u8] {
        &[]
    }

    fn get_config_descriptor(&self) -> &[u8] {
        &[]
    }

    fn get_hid_report_descriptor(&self) -> &[u8] {
        &[]
    }

    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Ack
    }

    fn poll_interrupt_in(&mut self, _ep: u8) -> Option<Vec<u8>> {
        None
    }

    fn as_hub(&self) -> Option<&dyn UsbHub> {
        Some(self)
    }

    fn as_hub_mut(&mut self) -> Option<&mut dyn UsbHub> {
        Some(self)
    }
}

struct TestLeaf;

impl UsbDeviceModel for TestLeaf {
    fn get_device_descriptor(&self) -> &[u8] {
        &[]
    }

    fn get_config_descriptor(&self) -> &[u8] {
        &[]
    }

    fn get_hid_report_descriptor(&self) -> &[u8] {
        &[]
    }

    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Ack
    }

    fn poll_interrupt_in(&mut self, _ep: u8) -> Option<Vec<u8>> {
        None
    }
}

fn set_address(dev: &mut AttachedUsbDevice, address: u8) {
    let setup = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x05, // SET_ADDRESS
        w_value: address as u16,
        w_index: 0,
        w_length: 0,
    };

    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert_eq!(dev.handle_in(0, 0), UsbInResult::Data(Vec::new()));
    assert_eq!(dev.address(), address);
}

#[test]
fn root_hub_routes_to_device_behind_single_hub() {
    let mut root = RootHub::new();
    let (hub, hub_ctl) = TestHub::new(4);

    root.attach(0, Box::new(hub));
    root.force_enable_for_tests(0);

    {
        let hub_dev = root
            .device_mut_for_address(0)
            .expect("hub should be reachable at address 0");
        set_address(hub_dev, 1);
    }

    root.attach_at_path(&[0, 1], Box::new(TestLeaf))
        .expect("attach leaf behind hub");
    hub_ctl.set_port_enabled(0, true);

    {
        let leaf = root
            .device_mut_for_address(0)
            .expect("leaf should be reachable at address 0");
        set_address(leaf, 5);
    }

    assert_eq!(
        root.device_mut_for_address(5)
            .expect("leaf should be routable by address")
            .address(),
        5
    );
}

#[test]
fn root_hub_routes_to_device_behind_nested_hubs() {
    let mut root = RootHub::new();
    let (hub1, hub1_ctl) = TestHub::new(2);

    root.attach(0, Box::new(hub1));
    root.force_enable_for_tests(0);

    {
        let hub_dev = root
            .device_mut_for_address(0)
            .expect("hub1 should be reachable at address 0");
        set_address(hub_dev, 1);
    }

    let (hub2, hub2_ctl) = TestHub::new(2);
    root.attach_at_path(&[0, 1], Box::new(hub2))
        .expect("attach hub2 behind hub1");
    hub1_ctl.set_port_enabled(0, true);

    {
        let hub2_dev = root
            .device_mut_for_address(0)
            .expect("hub2 should be reachable at address 0 once hub1 port is enabled");
        set_address(hub2_dev, 2);
    }

    root.attach_at_path(&[0, 1, 2], Box::new(TestLeaf))
        .expect("attach leaf behind hub2");
    hub2_ctl.set_port_enabled(1, true);

    {
        let leaf = root
            .device_mut_for_address(0)
            .expect("leaf should be reachable at address 0");
        set_address(leaf, 7);
    }

    assert_eq!(
        root.device_mut_for_address(7)
            .expect("leaf should be routable by address")
            .address(),
        7
    );
}

#[test]
fn disabled_downstream_port_is_not_routable() {
    let mut root = RootHub::new();
    let (hub, hub_ctl) = TestHub::new(1);

    root.attach(0, Box::new(hub));
    root.force_enable_for_tests(0);

    {
        let hub_dev = root.device_mut_for_address(0).unwrap();
        set_address(hub_dev, 1);
    }

    root.attach_at_path(&[0, 1], Box::new(TestLeaf)).unwrap();
    hub_ctl.set_port_enabled(0, true);

    {
        let leaf = root.device_mut_for_address(0).unwrap();
        set_address(leaf, 5);
    }

    assert!(root.device_mut_for_address(5).is_some());
    hub_ctl.set_port_enabled(0, false);
    assert!(root.device_mut_for_address(5).is_none());
}
