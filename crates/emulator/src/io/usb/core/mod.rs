use crate::io::usb::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
};

const USB_REQUEST_SET_ADDRESS: u8 = 0x05;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbOutResult {
    Ack,
    Nak,
    Stall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbInResult {
    Data(Vec<u8>),
    Nak,
    Stall,
}

#[derive(Debug, Clone)]
enum ControlStage {
    InData { data: Vec<u8>, offset: usize },
    InDataPending,
    OutData { expected: usize, received: Vec<u8> },
    StatusIn,
    StatusInPending { data: Option<Vec<u8>> },
    StatusOut,
    StatusOutPending,
}

#[derive(Debug, Clone)]
struct ControlState {
    setup: SetupPacket,
    stage: ControlStage,
}

/// A USB device attached behind the UHCI root hub.
///
/// This wrapper tracks the device address and provides an endpoint-0 control pipe
/// state machine over a [`UsbDeviceModel`] which operates at the SETUP request level.
pub struct AttachedUsbDevice {
    address: u8,
    pending_address: Option<u8>,
    control: Option<ControlState>,
    model: Box<dyn UsbDeviceModel>,
}

impl AttachedUsbDevice {
    pub fn new(model: Box<dyn UsbDeviceModel>) -> Self {
        Self {
            address: 0,
            pending_address: None,
            control: None,
            model,
        }
    }

    pub fn address(&self) -> u8 {
        self.address
    }

    pub fn model_mut(&mut self) -> &mut dyn UsbDeviceModel {
        &mut *self.model
    }

    pub fn as_hub(&self) -> Option<&dyn crate::io::usb::hub::UsbHub> {
        self.model.as_hub()
    }

    pub fn as_hub_mut(&mut self) -> Option<&mut dyn crate::io::usb::hub::UsbHub> {
        self.model.as_hub_mut()
    }

    pub fn reset(&mut self) {
        self.address = 0;
        self.pending_address = None;
        self.control = None;
        self.model.reset();
    }

    pub fn tick_1ms(&mut self) {
        self.model.tick_1ms();
    }

    pub fn device_mut_for_address(&mut self, address: u8) -> Option<&mut AttachedUsbDevice> {
        if self.address == address {
            return Some(self);
        }
        self.model.child_device_mut_for_address(address)
    }

    pub fn handle_setup(&mut self, setup: SetupPacket) -> UsbOutResult {
        // Starting a new SETUP always aborts any in-flight control transfer.
        if self.control.is_some() {
            self.model.cancel_control_transfer();
        }
        self.control = None;
        // `SET_ADDRESS` only takes effect after the STATUS stage; if the control transfer was
        // aborted by a new SETUP, the pending address must be discarded.
        self.pending_address = None;

        let stage = match setup.request_direction() {
            RequestDirection::DeviceToHost => {
                let resp = self.model.handle_control_request(setup, None);
                match resp {
                    ControlResponse::Data(mut data) => {
                        let requested = setup.w_length as usize;
                        if data.len() > requested {
                            data.truncate(requested);
                        }
                        if requested == 0 || data.is_empty() {
                            ControlStage::StatusOut
                        } else {
                            ControlStage::InData { data, offset: 0 }
                        }
                    }
                    ControlResponse::Ack => ControlStage::StatusOut,
                    ControlResponse::Nak => {
                        if setup.w_length == 0 {
                            ControlStage::StatusOutPending
                        } else {
                            ControlStage::InDataPending
                        }
                    }
                    ControlResponse::Stall => return UsbOutResult::Stall,
                }
            }
            RequestDirection::HostToDevice => {
                // Intercept SET_ADDRESS so device models don't need to track address state.
                if setup.request_type() == RequestType::Standard
                    && setup.recipient() == RequestRecipient::Device
                    && setup.b_request == USB_REQUEST_SET_ADDRESS
                {
                    if setup.w_index != 0 || setup.w_length != 0 || setup.w_value > 127 {
                        return UsbOutResult::Stall;
                    }
                    self.pending_address = Some((setup.w_value & 0x00ff) as u8);
                    ControlStage::StatusIn
                } else if setup.w_length == 0 {
                    match self.model.handle_control_request(setup, None) {
                        ControlResponse::Ack => ControlStage::StatusIn,
                        ControlResponse::Nak => ControlStage::StatusInPending { data: None },
                        ControlResponse::Stall => return UsbOutResult::Stall,
                        ControlResponse::Data(_) => return UsbOutResult::Stall,
                    }
                } else {
                    ControlStage::OutData {
                        expected: setup.w_length as usize,
                        received: Vec::with_capacity(setup.w_length as usize),
                    }
                }
            }
        };

        self.control = Some(ControlState { setup, stage });
        UsbOutResult::Ack
    }

    pub fn handle_out(&mut self, endpoint: u8, data: &[u8]) -> UsbOutResult {
        if endpoint != 0 {
            let ep_addr = endpoint & 0x0f;
            return self.model.handle_out_transfer(ep_addr, data);
        }
        let Some(state) = self.control.as_mut() else {
            return UsbOutResult::Stall;
        };

        match &mut state.stage {
            ControlStage::OutData { expected, received } => {
                let remaining = expected.saturating_sub(received.len());
                let chunk_len = remaining.min(data.len());
                received.extend_from_slice(&data[..chunk_len]);
                if received.len() >= *expected {
                    let setup = state.setup;
                    match self
                        .model
                        .handle_control_request(setup, Some(received.as_slice()))
                    {
                        ControlResponse::Ack => {
                            state.stage = ControlStage::StatusIn;
                            return UsbOutResult::Ack;
                        }
                        ControlResponse::Nak => {
                            // Control OUT transfers carry their payload in the DATA stage; once we
                            // have buffered all bytes we can ACK the final DATA TD and represent
                            // "still waiting for the host-side transfer" by NAKing the STATUS stage.
                            state.stage = ControlStage::StatusInPending {
                                data: Some(received.clone()),
                            };
                            return UsbOutResult::Ack;
                        }
                        ControlResponse::Stall => return UsbOutResult::Stall,
                        ControlResponse::Data(_) => return UsbOutResult::Stall,
                    }
                }
                UsbOutResult::Ack
            }
            ControlStage::StatusOut => {
                if !data.is_empty() {
                    return UsbOutResult::Stall;
                }
                self.control = None;
                UsbOutResult::Ack
            }
            ControlStage::StatusOutPending => {
                if !data.is_empty() {
                    return UsbOutResult::Stall;
                }
                let setup = state.setup;
                match self.model.handle_control_request(setup, None) {
                    ControlResponse::Nak => UsbOutResult::Nak,
                    ControlResponse::Stall => UsbOutResult::Stall,
                    // Whether the model reports `Ack` or `Data([])`, we treat this as the
                    // completion point for the whole control transfer (status stage).
                    ControlResponse::Ack | ControlResponse::Data(_) => {
                        self.control = None;
                        UsbOutResult::Ack
                    }
                }
            }
            _ => UsbOutResult::Stall,
        }
    }

    pub fn handle_in(&mut self, endpoint: u8, max_len: usize) -> UsbInResult {
        if endpoint == 0 {
            return self.handle_control_in(max_len);
        }

        let ep_addr = 0x80 | (endpoint & 0x0f);
        match self.model.handle_in_transfer(ep_addr, max_len) {
            UsbInResult::Data(mut data) => {
                if data.len() > max_len {
                    data.truncate(max_len);
                }
                UsbInResult::Data(data)
            }
            other => other,
        }
    }

    fn handle_control_in(&mut self, max_len: usize) -> UsbInResult {
        let Some(state) = self.control.as_mut() else {
            return UsbInResult::Stall;
        };

        match &mut state.stage {
            ControlStage::InData { data, offset } => {
                let remaining = data.len().saturating_sub(*offset);
                let chunk_len = remaining.min(max_len);
                let chunk = data[*offset..*offset + chunk_len].to_vec();
                *offset += chunk_len;
                if *offset >= data.len() {
                    state.stage = ControlStage::StatusOut;
                }
                UsbInResult::Data(chunk)
            }
            ControlStage::InDataPending => {
                let setup = state.setup;
                match self.model.handle_control_request(setup, None) {
                    ControlResponse::Nak => UsbInResult::Nak,
                    ControlResponse::Stall => UsbInResult::Stall,
                    ControlResponse::Ack => {
                        state.stage = ControlStage::StatusOut;
                        UsbInResult::Data(Vec::new())
                    }
                    ControlResponse::Data(mut data) => {
                        let requested = setup.w_length as usize;
                        if data.len() > requested {
                            data.truncate(requested);
                        }
                        if requested == 0 || data.is_empty() {
                            state.stage = ControlStage::StatusOut;
                            return UsbInResult::Data(Vec::new());
                        }

                        let chunk_len = data.len().min(max_len);
                        let chunk = data[..chunk_len].to_vec();
                        if chunk_len >= data.len() {
                            state.stage = ControlStage::StatusOut;
                        } else {
                            state.stage = ControlStage::InData {
                                data,
                                offset: chunk_len,
                            };
                        }
                        UsbInResult::Data(chunk)
                    }
                }
            }
            ControlStage::StatusIn => {
                if max_len != 0 {
                    return UsbInResult::Stall;
                }
                if let Some(addr) = self.pending_address.take() {
                    self.address = addr;
                }
                self.control = None;
                UsbInResult::Data(Vec::new())
            }
            ControlStage::StatusInPending { data } => {
                if max_len != 0 {
                    return UsbInResult::Stall;
                }
                let setup = state.setup;
                match self.model.handle_control_request(setup, data.as_deref()) {
                    ControlResponse::Nak => UsbInResult::Nak,
                    ControlResponse::Ack => {
                        if let Some(addr) = self.pending_address.take() {
                            self.address = addr;
                        }
                        self.control = None;
                        UsbInResult::Data(Vec::new())
                    }
                    ControlResponse::Stall | ControlResponse::Data(_) => UsbInResult::Stall,
                }
            }
            _ => UsbInResult::Stall,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct AckModel;

    impl UsbDeviceModel for AckModel {
        fn handle_control_request(
            &mut self,
            _setup: SetupPacket,
            _data_stage: Option<&[u8]>,
        ) -> ControlResponse {
            ControlResponse::Ack
        }
    }

    #[test]
    fn set_address_invalid_fields_stall() {
        let mut dev = AttachedUsbDevice::new(Box::new(AckModel::default()));

        // Invalid address (must be <= 127).
        assert_eq!(
            dev.handle_setup(SetupPacket {
                bm_request_type: 0x00, // HostToDevice | Standard | Device
                b_request: USB_REQUEST_SET_ADDRESS,
                w_value: 200,
                w_index: 0,
                w_length: 0,
            }),
            UsbOutResult::Stall
        );

        // Invalid wIndex.
        assert_eq!(
            dev.handle_setup(SetupPacket {
                bm_request_type: 0x00,
                b_request: USB_REQUEST_SET_ADDRESS,
                w_value: 5,
                w_index: 1,
                w_length: 0,
            }),
            UsbOutResult::Stall
        );

        // Invalid wLength.
        assert_eq!(
            dev.handle_setup(SetupPacket {
                bm_request_type: 0x00,
                b_request: USB_REQUEST_SET_ADDRESS,
                w_value: 5,
                w_index: 0,
                w_length: 1,
            }),
            UsbOutResult::Stall
        );
    }

    #[test]
    fn new_setup_aborts_pending_set_address() {
        let mut dev = AttachedUsbDevice::new(Box::new(AckModel::default()));

        let set_address = SetupPacket {
            bm_request_type: 0x00,
            b_request: USB_REQUEST_SET_ADDRESS,
            w_value: 5,
            w_index: 0,
            w_length: 0,
        };
        assert_eq!(dev.handle_setup(set_address), UsbOutResult::Ack);

        // Abort the SET_ADDRESS request before the status stage is executed.
        let other = SetupPacket {
            bm_request_type: 0x00,
            b_request: 0x09, // SET_CONFIGURATION
            w_value: 1,
            w_index: 0,
            w_length: 0,
        };
        assert_eq!(dev.handle_setup(other), UsbOutResult::Ack);

        // Completing the subsequent status stage must not apply the old pending address.
        assert_eq!(dev.handle_in(0, 0), UsbInResult::Data(Vec::new()));
        assert_eq!(dev.address(), 0);
    }

    #[test]
    fn new_setup_invokes_cancel_control_transfer_hook() {
        use std::cell::Cell;
        use std::rc::Rc;

        #[derive(Clone)]
        struct CancelCounter(Rc<Cell<u32>>);

        struct CancelModel {
            counter: CancelCounter,
        }

        impl UsbDeviceModel for CancelModel {
            fn cancel_control_transfer(&mut self) {
                self.counter.0.set(self.counter.0.get() + 1);
            }

            fn handle_control_request(
                &mut self,
                _setup: SetupPacket,
                _data_stage: Option<&[u8]>,
            ) -> ControlResponse {
                ControlResponse::Ack
            }
        }

        let counter = CancelCounter(Rc::new(Cell::new(0)));
        let model = CancelModel {
            counter: counter.clone(),
        };
        let mut dev = AttachedUsbDevice::new(Box::new(model));

        let setup1 = SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0100,
            w_index: 0,
            w_length: 4,
        };
        let setup2 = SetupPacket {
            bm_request_type: 0x80,
            b_request: 0x06,
            w_value: 0x0200,
            w_index: 0,
            w_length: 4,
        };

        assert_eq!(dev.handle_setup(setup1), UsbOutResult::Ack);
        // Second SETUP before the first transfer completes should trigger cancel.
        assert_eq!(dev.handle_setup(setup2), UsbOutResult::Ack);

        assert_eq!(counter.0.get(), 1);
    }
}
