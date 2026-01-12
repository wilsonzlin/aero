use aero_usb::passthrough::{
    ControlResponse, SetupPacket, UsbHostAction, UsbHostCompletion, UsbHostCompletionOut,
    UsbOutResult, UsbPassthroughDevice,
};

#[test]
fn control_out_requires_data_stage_len_to_match_w_length() {
    let mut dev = UsbPassthroughDevice::new();
    let setup = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x09,
        w_value: 0,
        w_index: 0,
        w_length: 4,
    };

    let resp = dev.handle_control_request(setup, Some(&[1, 2, 3]));
    assert_eq!(resp, ControlResponse::Stall);
    assert!(dev.drain_actions().is_empty());
}

#[test]
fn control_out_w_length_zero_treats_none_and_empty_slice_as_equivalent() {
    let mut dev = UsbPassthroughDevice::new();
    let setup = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x09,
        w_value: 0,
        w_index: 0,
        w_length: 0,
    };

    assert_eq!(
        dev.handle_control_request(setup, None),
        ControlResponse::Nak
    );
    let actions = dev.drain_actions();
    assert_eq!(actions.len(), 1);
    match &actions[0] {
        UsbHostAction::ControlOut { data, .. } => assert!(data.is_empty()),
        other => panic!("unexpected action: {other:?}"),
    }

    // Retry with `Some(&[])` instead of `None`; this should not allocate a new control id or queue a
    // duplicate host action.
    assert_eq!(
        dev.handle_control_request(setup, Some(&[])),
        ControlResponse::Nak
    );
    assert!(dev.drain_actions().is_empty());
}

#[test]
fn control_out_completion_bytes_written_must_match_w_length() {
    let mut dev = UsbPassthroughDevice::new();
    let setup = SetupPacket {
        bm_request_type: 0x00,
        b_request: 0x09,
        w_value: 0,
        w_index: 0,
        w_length: 4,
    };
    let data = [1u8, 2, 3, 4];

    assert_eq!(
        dev.handle_control_request(setup, Some(&data)),
        ControlResponse::Nak
    );
    let action = dev.pop_action().expect("expected queued host action");
    let id = match action {
        UsbHostAction::ControlOut { id, data: got, .. } => {
            assert_eq!(got, data);
            id
        }
        other => panic!("unexpected action: {other:?}"),
    };

    dev.push_completion(UsbHostCompletion::ControlOut {
        id,
        result: UsbHostCompletionOut::Success { bytes_written: 3 },
    });

    let resp = dev.handle_control_request(setup, Some(&data));
    assert_eq!(resp, ControlResponse::Timeout);
}

#[test]
fn bulk_out_completion_bytes_written_must_match_payload_len() {
    let mut dev = UsbPassthroughDevice::new();
    let endpoint = 0x01;
    let data = [1u8, 2, 3, 4];

    assert_eq!(dev.handle_out_transfer(endpoint, &data), UsbOutResult::Nak);
    let action = dev.pop_action().expect("expected queued host action");
    let id = match action {
        UsbHostAction::BulkOut {
            id,
            endpoint: ep,
            data: got,
        } => {
            assert_eq!(ep, endpoint);
            assert_eq!(got, data);
            id
        }
        other => panic!("unexpected action: {other:?}"),
    };

    dev.push_completion(UsbHostCompletion::BulkOut {
        id,
        result: UsbHostCompletionOut::Success { bytes_written: 3 },
    });

    let resp = dev.handle_out_transfer(endpoint, &data);
    assert_eq!(resp, UsbOutResult::Timeout);
}
