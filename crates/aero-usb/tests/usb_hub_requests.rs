use aero_usb::hub::UsbHubDevice;
use aero_usb::usb::{SetupPacket, UsbDevice, UsbHandshake};

#[derive(Default)]
struct DummyUsbDevice;

impl UsbDevice for DummyUsbDevice {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }

    fn reset(&mut self) {}

    fn address(&self) -> u8 {
        0
    }

    fn handle_setup(&mut self, _setup: SetupPacket) {}

    fn handle_out(&mut self, _ep: u8, _data: &[u8]) -> UsbHandshake {
        UsbHandshake::Ack { bytes: 0 }
    }

    fn handle_in(&mut self, _ep: u8, _buf: &mut [u8]) -> UsbHandshake {
        UsbHandshake::Ack { bytes: 0 }
    }
}

fn control_no_data(dev: &mut dyn UsbDevice, setup: SetupPacket) {
    dev.handle_setup(setup);
    let mut buf = [0u8; 0];
    assert!(matches!(
        dev.handle_in(0, &mut buf),
        UsbHandshake::Ack { .. }
    ));
}

fn control_in(dev: &mut dyn UsbDevice, setup: SetupPacket, max_packet: usize) -> Vec<u8> {
    dev.handle_setup(setup);

    let mut out = Vec::new();
    let mut buf = vec![0u8; max_packet];
    loop {
        match dev.handle_in(0, &mut buf) {
            UsbHandshake::Ack { bytes } => {
                out.extend_from_slice(&buf[..bytes]);
                if bytes < max_packet {
                    break;
                }
            }
            UsbHandshake::Nak => break,
            UsbHandshake::Stall | UsbHandshake::Timeout => panic!("expected control IN data"),
        }
    }

    // Status stage (OUT ZLP).
    assert!(matches!(dev.handle_out(0, &[]), UsbHandshake::Ack { .. }));
    out
}

#[test]
fn usb_hub_interrupt_bitmap_and_descriptor_scale_with_port_count() {
    let mut hub = UsbHubDevice::with_port_count(8);
    hub.attach(8, Box::new(DummyUsbDevice));

    // SET_CONFIGURATION(1).
    control_no_data(
        &mut hub,
        SetupPacket {
            request_type: 0x00,
            request: 0x09,
            value: 1,
            index: 0,
            length: 0,
        },
    );

    // Interrupt endpoint bitmap should be 2 bytes for 8 ports (9 bits).
    let mut bitmap = [0u8; 2];
    let UsbHandshake::Ack { bytes } = hub.handle_in(1, &mut bitmap) else {
        panic!("expected Ack for hub interrupt endpoint");
    };
    assert_eq!(bytes, 2);
    assert_ne!(bitmap[1] & 0x01, 0, "bit8 (port8) should be set");

    // GET_DESCRIPTOR(Hub, type=0x29) via class request.
    let hub_desc = control_in(
        &mut hub,
        SetupPacket {
            request_type: 0xa0,
            request: 0x06,
            value: 0x2900,
            index: 0,
            length: 64,
        },
        64,
    );
    assert_eq!(hub_desc[0], 11);
    assert_eq!(hub_desc[1], 0x29);
    assert_eq!(hub_desc[2], 8);

    // DeviceRemovable + PortPwrCtrlMask bitmaps for 8 ports are 2 bytes each.
    assert_eq!(&hub_desc[7..9], &[0x00, 0x00]);
    assert_eq!(&hub_desc[9..11], &[0xFE, 0x01]);

    // Interrupt endpoint wMaxPacketSize should match the bitmap length.
    let cfg_desc = control_in(
        &mut hub,
        SetupPacket {
            request_type: 0x80,
            request: 0x06,
            value: 0x0200,
            index: 0,
            length: 255,
        },
        64,
    );
    assert_eq!(cfg_desc[22], 2);
    assert_eq!(cfg_desc[23], 0);
}
