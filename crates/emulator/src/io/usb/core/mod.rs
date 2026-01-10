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
    OutData { expected: usize, received: Vec<u8> },
    StatusIn,
    StatusOut,
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

    pub fn reset(&mut self) {
        self.address = 0;
        self.pending_address = None;
        self.control = None;
        self.model.reset();
    }

    pub fn handle_setup(&mut self, setup: SetupPacket) -> UsbOutResult {
        // Starting a new SETUP always abandons any in-flight transfer.
        self.control = None;

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
                    ControlResponse::Stall => return UsbOutResult::Stall,
                }
            }
            RequestDirection::HostToDevice => {
                // Intercept SET_ADDRESS so device models don't need to track address state.
                if setup.request_type() == RequestType::Standard
                    && setup.recipient() == RequestRecipient::Device
                    && setup.b_request == USB_REQUEST_SET_ADDRESS
                    && setup.w_length == 0
                {
                    self.pending_address = Some((setup.w_value & 0x00ff) as u8);
                    ControlStage::StatusIn
                } else if setup.w_length == 0 {
                    match self.model.handle_control_request(setup, None) {
                        ControlResponse::Ack => ControlStage::StatusIn,
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
            return UsbOutResult::Stall;
        }
        let Some(state) = self.control.as_mut() else {
            return UsbOutResult::Stall;
        };

        match &mut state.stage {
            ControlStage::OutData { expected, received } => {
                received.extend_from_slice(data);
                if received.len() >= *expected {
                    let setup = state.setup;
                    match self
                        .model
                        .handle_control_request(setup, Some(received.as_slice()))
                    {
                        ControlResponse::Ack => {
                            state.stage = ControlStage::StatusIn;
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
            _ => UsbOutResult::Stall,
        }
    }

    pub fn handle_in(&mut self, endpoint: u8, max_len: usize) -> UsbInResult {
        if endpoint == 0 {
            return self.handle_control_in(max_len);
        }

        let ep_addr = 0x80 | (endpoint & 0x0f);
        match self.model.poll_interrupt_in(ep_addr) {
            Some(mut data) => {
                if data.len() > max_len {
                    data.truncate(max_len);
                }
                UsbInResult::Data(data)
            }
            None => UsbInResult::Nak,
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
            _ => UsbInResult::Stall,
        }
    }
}
