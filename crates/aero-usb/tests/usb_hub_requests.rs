use aero_usb::device::{AttachedUsbDevice, UsbOutResult};
use aero_usb::hub::UsbHubDevice;
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

#[derive(Default)]
struct DummyUsbDevice;

impl UsbDeviceModel for DummyUsbDevice {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }
}

fn control_out_no_data(dev: &mut AttachedUsbDevice, setup: SetupPacket) {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
    assert!(matches!(
        dev.handle_in(0, 0),
        UsbInResult::Data(data) if data.is_empty()
    ));
}

fn control_in(dev: &mut AttachedUsbDevice, setup: SetupPacket, max_packet: usize) -> Vec<u8> {
    assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);

    let mut out = Vec::new();
    loop {
        match dev.handle_in(0, max_packet) {
            UsbInResult::Data(chunk) => {
                let n = chunk.len();
                out.extend_from_slice(&chunk);
                if n < max_packet {
                    break;
                }
            }
            UsbInResult::Nak => break,
            UsbInResult::Stall => panic!("expected control IN data"),
            UsbInResult::Timeout => panic!("unexpected TIMEOUT during control IN transfer"),
        }
    }

    // Status stage (OUT ZLP).
    assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
    out
}

#[test]
fn usb_hub_interrupt_bitmap_and_descriptor_scale_with_port_count() {
    let mut hub = UsbHubDevice::with_port_count(16);
    hub.attach(16, Box::new(DummyUsbDevice));

    let mut dev = AttachedUsbDevice::new(Box::new(hub));

    // SET_CONFIGURATION(1).
    control_out_no_data(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09,
            w_value: 1,
            w_index: 0,
            w_length: 0,
        },
    );

    // Interrupt endpoint bitmap should be 3 bytes for 16 ports (17 bits).
    let UsbInResult::Data(bitmap) = dev.handle_in(1, 3) else {
        panic!("expected Data for hub interrupt endpoint");
    };
    assert_eq!(bitmap.len(), 3);
    assert_ne!(bitmap[2] & 0x01, 0, "bit16 (port16) should be set");

    // GET_DESCRIPTOR(Hub, type=0x29) via class request.
    let hub_desc = control_in(
        &mut dev,
        SetupPacket {
            bm_request_type: 0xa0,
            b_request: 0x06,
            w_value: 0x2900,
            w_index: 0,
            w_length: 64,
        },
        64,
    );
    assert_eq!(hub_desc[0], 13);
    assert_eq!(hub_desc[1], 0x29);
    assert_eq!(hub_desc[2], 16);

    // DeviceRemovable + PortPwrCtrlMask bitmaps for 16 ports are 3 bytes each.
    assert_eq!(&hub_desc[7..10], &[0x00, 0x00, 0x00]);
    assert_eq!(&hub_desc[10..13], &[0xFE, 0xFF, 0x01]);

    // Interrupt endpoint wMaxPacketSize should match the bitmap length.
    let cfg_desc = control_in(
        &mut dev,
        SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0200,
            w_index: 0,
            w_length: 255,
        },
        64,
    );
    assert_eq!(cfg_desc[22], 3);
    assert_eq!(cfg_desc[23], 0);
}
