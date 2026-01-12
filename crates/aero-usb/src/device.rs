use alloc::boxed::Box;
use alloc::vec::Vec;
use core::any::Any;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use crate::hid::composite::UsbCompositeHidInput;
use crate::hid::{
    UsbCompositeHidInputHandle, UsbHidGamepad, UsbHidGamepadHandle, UsbHidKeyboard,
    UsbHidKeyboardHandle, UsbHidMouse, UsbHidMouseHandle, UsbHidPassthrough,
    UsbHidPassthroughHandle,
};
use crate::hub::UsbHubDevice;
use crate::{
    ControlResponse, RequestDirection, RequestRecipient, RequestType, SetupPacket, UsbDeviceModel,
    UsbSpeed,
};

const USB_REQUEST_SET_ADDRESS: u8 = 0x05;
const USB_REQUEST_SET_CONFIGURATION: u8 = 0x09;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbOutResult {
    Ack,
    Nak,
    Stall,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UsbInResult {
    Data(Vec<u8>),
    Nak,
    Stall,
    Timeout,
}

#[derive(Debug, Clone)]
enum ControlStage {
    InData {
        data: Vec<u8>,
        offset: usize,
    },
    /// Emit a terminating zero-length packet (ZLP) for a control-IN data stage.
    ///
    /// This is required when the device provides fewer bytes than `wLength` but the final
    /// packet is not "short" (i.e. its length equals the host's `max_len` for the DATA TD).
    InDataZlp,
    InDataPending,
    OutData {
        expected: usize,
        received: Vec<u8>,
    },
    StatusIn,
    StatusInPending {
        data: Option<Vec<u8>>,
    },
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
/// This wrapper tracks the device address and provides an endpoint-0 control pipe state machine
/// over a [`UsbDeviceModel`] which operates at the SETUP request level.
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

    pub(crate) fn try_new_from_snapshot(bytes: &[u8]) -> SnapshotResult<Option<Self>> {
        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;
        let Some(model_snapshot) = r.bytes(ADEV_TAG_MODEL_SNAPSHOT) else {
            return Ok(None);
        };
        let Some(model) = try_new_usb_device_model_from_snapshot(model_snapshot)? else {
            return Ok(None);
        };
        Ok(Some(Self::new(model)))
    }

    pub fn address(&self) -> u8 {
        self.address
    }

    pub fn speed(&self) -> UsbSpeed {
        self.model.speed()
    }

    pub fn model_mut(&mut self) -> &mut dyn UsbDeviceModel {
        &mut *self.model
    }

    pub fn model(&self) -> &dyn UsbDeviceModel {
        &*self.model
    }

    pub fn as_hub(&self) -> Option<&dyn crate::hub::UsbHub> {
        self.model.as_hub()
    }

    pub fn as_hub_mut(&mut self) -> Option<&mut dyn crate::hub::UsbHub> {
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

        // Intercept SET_ADDRESS so device models don't need to track address state.
        //
        // USB 2.0 spec 9.4.6: SET_ADDRESS is always HostToDevice with wLength=0 and wIndex=0.
        // We always virtualize it and never forward it to the underlying model/physical device.
        if setup.request_type() == RequestType::Standard
            && setup.recipient() == RequestRecipient::Device
            && setup.b_request == USB_REQUEST_SET_ADDRESS
        {
            if setup.request_direction() != RequestDirection::HostToDevice
                || setup.w_index != 0
                || setup.w_length != 0
                || setup.w_value > 127
            {
                return UsbOutResult::Stall;
            }

            self.pending_address = Some((setup.w_value & 0x00ff) as u8);
            self.control = Some(ControlState {
                setup,
                stage: ControlStage::StatusIn,
            });
            return UsbOutResult::Ack;
        }

        // Defer SET_CONFIGURATION to the STATUS stage.
        //
        // Synchronous device models mutate their configuration state when they observe the
        // `SET_CONFIGURATION` request. If we forwarded it at SETUP time, a subsequent SETUP could
        // abort the control transfer but still leave the device in the configured state. By
        // postponing delivery until the STATUS stage we ensure the configuration only takes effect
        // once the request has completed.
        if setup.request_type() == RequestType::Standard
            && setup.recipient() == RequestRecipient::Device
            && setup.b_request == USB_REQUEST_SET_CONFIGURATION
            && setup.request_direction() == RequestDirection::HostToDevice
            && setup.w_length == 0
        {
            self.control = Some(ControlState {
                setup,
                stage: ControlStage::StatusInPending { data: None },
            });
            return UsbOutResult::Ack;
        }

        let stage = match setup.request_direction() {
            RequestDirection::DeviceToHost => {
                let resp = self.model.handle_control_request(setup, None);
                match resp {
                    ControlResponse::Data(mut data) => {
                        let requested = setup.w_length as usize;
                        if data.len() > requested {
                            data.truncate(requested);
                        }
                        if requested == 0 {
                            ControlStage::StatusOut
                        } else {
                            // Even a zero-length data stage (ZLP) is a valid completion for
                            // control-IN requests where `wLength > 0`. The guest will still issue
                            // an IN TD for the DATA stage, so model this as a one-shot IN DATA
                            // stage that can return an empty packet.
                            ControlStage::InData { data, offset: 0 }
                        }
                    }
                    ControlResponse::Ack => {
                        if setup.w_length == 0 {
                            ControlStage::StatusOut
                        } else {
                            ControlStage::InData {
                                data: Vec::new(),
                                offset: 0,
                            }
                        }
                    }
                    ControlResponse::Nak => {
                        if setup.w_length == 0 {
                            ControlStage::StatusOutPending
                        } else {
                            ControlStage::InDataPending
                        }
                    }
                    ControlResponse::Stall => return UsbOutResult::Stall,
                    ControlResponse::Timeout => return UsbOutResult::Timeout,
                }
            }
            RequestDirection::HostToDevice => {
                if setup.w_length == 0 {
                    match self.model.handle_control_request(setup, None) {
                        ControlResponse::Ack => ControlStage::StatusIn,
                        ControlResponse::Nak => ControlStage::StatusInPending { data: None },
                        ControlResponse::Stall => return UsbOutResult::Stall,
                        ControlResponse::Timeout => return UsbOutResult::Timeout,
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

    /// Handle an OUT transaction to endpoint number `endpoint`.
    ///
    /// `endpoint` is the endpoint **number** (0..=15), not the endpoint address (e.g. not `0x01`).
    pub fn handle_out(&mut self, endpoint: u8, data: &[u8]) -> UsbOutResult {
        debug_assert!(
            (endpoint & 0xF0) == 0,
            "handle_out expects an endpoint number (0..=15), got {endpoint:#04x}"
        );
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
                        ControlResponse::Timeout => {
                            self.control = None;
                            return UsbOutResult::Timeout;
                        }
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
                    ControlResponse::Timeout => {
                        self.control = None;
                        UsbOutResult::Timeout
                    }
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

    /// Handle an IN transaction to endpoint number `endpoint`.
    ///
    /// `endpoint` is the endpoint **number** (0..=15), not the endpoint address (e.g. not `0x81`).
    pub fn handle_in(&mut self, endpoint: u8, max_len: usize) -> UsbInResult {
        debug_assert!(
            (endpoint & 0xF0) == 0,
            "handle_in expects an endpoint number (0..=15), got {endpoint:#04x}"
        );
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
                if max_len == 0 {
                    return UsbInResult::Stall;
                }
                let requested = state.setup.w_length as usize;
                let remaining = data.len().saturating_sub(*offset);
                let chunk_len = remaining.min(max_len);
                let chunk = data[*offset..*offset + chunk_len].to_vec();
                *offset += chunk_len;
                if *offset >= data.len() {
                    if data.len() < requested && chunk_len == max_len {
                        state.stage = ControlStage::InDataZlp;
                    } else {
                        state.stage = ControlStage::StatusOut;
                    }
                }
                UsbInResult::Data(chunk)
            }
            ControlStage::InDataZlp => {
                // Always treat the ZLP as the final DATA packet, then transition to the STATUS OUT
                // stage.
                state.stage = ControlStage::StatusOut;
                UsbInResult::Data(Vec::new())
            }
            ControlStage::InDataPending => {
                let setup = state.setup;
                match self.model.handle_control_request(setup, None) {
                    ControlResponse::Nak => UsbInResult::Nak,
                    ControlResponse::Stall => UsbInResult::Stall,
                    ControlResponse::Timeout => {
                        self.control = None;
                        UsbInResult::Timeout
                    }
                    ControlResponse::Ack => {
                        state.stage = ControlStage::StatusOut;
                        UsbInResult::Data(Vec::new())
                    }
                    ControlResponse::Data(mut data) => {
                        if max_len == 0 {
                            return UsbInResult::Stall;
                        }
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
                            if data.len() < requested && chunk_len == max_len {
                                state.stage = ControlStage::InDataZlp;
                            } else {
                                state.stage = ControlStage::StatusOut;
                            }
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
                    ControlResponse::Timeout => {
                        self.control = None;
                        UsbInResult::Timeout
                    }
                    ControlResponse::Stall | ControlResponse::Data(_) => UsbInResult::Stall,
                }
            }
            _ => UsbInResult::Stall,
        }
    }
}

fn encode_setup_packet(enc: Encoder, setup: SetupPacket) -> Encoder {
    enc.u8(setup.bm_request_type)
        .u8(setup.b_request)
        .u16(setup.w_value)
        .u16(setup.w_index)
        .u16(setup.w_length)
}

fn decode_setup_packet(d: &mut Decoder<'_>) -> SnapshotResult<SetupPacket> {
    Ok(SetupPacket {
        bm_request_type: d.u8()?,
        b_request: d.u8()?,
        w_value: d.u16()?,
        w_index: d.u16()?,
        w_length: d.u16()?,
    })
}

fn encode_control_state(state: &ControlState) -> Vec<u8> {
    let mut enc = encode_setup_packet(Encoder::new(), state.setup);

    let stage_tag: u8 = match &state.stage {
        ControlStage::InData { .. } => 0,
        ControlStage::InDataZlp => 1,
        ControlStage::InDataPending => 2,
        ControlStage::OutData { .. } => 3,
        ControlStage::StatusIn => 4,
        ControlStage::StatusInPending { .. } => 5,
        ControlStage::StatusOut => 6,
        ControlStage::StatusOutPending => 7,
    };

    enc = enc.u8(stage_tag);
    match &state.stage {
        ControlStage::InData { data, offset } => {
            enc = enc.vec_u8(data).u32(*offset as u32);
        }
        ControlStage::InDataZlp | ControlStage::InDataPending | ControlStage::StatusIn => {}
        ControlStage::OutData { expected, received } => {
            enc = enc.u32(*expected as u32).vec_u8(received);
        }
        ControlStage::StatusInPending { data } => {
            if let Some(buf) = data {
                enc = enc.bool(true).vec_u8(buf);
            } else {
                enc = enc.bool(false);
            }
        }
        ControlStage::StatusOut | ControlStage::StatusOutPending => {}
    }

    enc.finish()
}

fn decode_control_state(buf: &[u8]) -> SnapshotResult<ControlState> {
    let mut d = Decoder::new(buf);
    let setup = decode_setup_packet(&mut d)?;
    let stage_tag = d.u8()?;

    let stage = match stage_tag {
        0 => {
            let data = d.vec_u8()?;
            let offset = d.u32()? as usize;
            if offset > data.len() {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "control in_data offset",
                ));
            }
            ControlStage::InData { data, offset }
        }
        1 => ControlStage::InDataZlp,
        2 => ControlStage::InDataPending,
        3 => {
            let expected = d.u32()? as usize;
            let received = d.vec_u8()?;
            if received.len() > expected {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "control out_data exceeds expected length",
                ));
            }
            ControlStage::OutData { expected, received }
        }
        4 => ControlStage::StatusIn,
        5 => {
            let has_data = d.bool()?;
            let data = if has_data { Some(d.vec_u8()?) } else { None };
            ControlStage::StatusInPending { data }
        }
        6 => ControlStage::StatusOut,
        7 => ControlStage::StatusOutPending,
        _ => return Err(SnapshotError::InvalidFieldEncoding("control stage")),
    };

    d.finish()?;

    Ok(ControlState { setup, stage })
}

const ADEV_TAG_ADDRESS: u16 = 1;
const ADEV_TAG_PENDING_ADDRESS: u16 = 2;
const ADEV_TAG_CONTROL: u16 = 3;
const ADEV_TAG_MODEL_SNAPSHOT: u16 = 4;

impl IoSnapshot for AttachedUsbDevice {
    const DEVICE_ID: [u8; 4] = *b"ADEV";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 1);

    fn save_state(&self) -> Vec<u8> {
        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u8(ADEV_TAG_ADDRESS, self.address);
        if let Some(addr) = self.pending_address {
            w.field_u8(ADEV_TAG_PENDING_ADDRESS, addr);
        }
        if let Some(control) = self.control.as_ref() {
            w.field_bytes(ADEV_TAG_CONTROL, encode_control_state(control));
        }
        if let Some(model) = save_usb_device_model_snapshot(&*self.model) {
            w.field_bytes(ADEV_TAG_MODEL_SNAPSHOT, model);
        }
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Reset endpoint-0 / addressing state while preserving the underlying device model.
        self.address = 0;
        self.pending_address = None;
        self.control = None;

        self.address = r.u8(ADEV_TAG_ADDRESS)?.unwrap_or(0);
        self.pending_address = r.u8(ADEV_TAG_PENDING_ADDRESS)?;
        if let Some(buf) = r.bytes(ADEV_TAG_CONTROL) {
            self.control = Some(decode_control_state(buf)?);
        }
        if let Some(model) = r.bytes(ADEV_TAG_MODEL_SNAPSHOT) {
            apply_usb_device_model_snapshot(&mut *self.model, model)?;
        }

        Ok(())
    }
}

fn try_new_usb_device_model_from_snapshot(
    model_snapshot: &[u8],
) -> SnapshotResult<Option<Box<dyn UsbDeviceModel>>> {
    // `SnapshotReader::parse` requires a concrete expected device ID. Peek at the snapshot header
    // and dispatch to known model types.
    if model_snapshot.len() < 16 {
        return Err(SnapshotError::UnexpectedEof);
    }
    if model_snapshot[0..4] != *b"AERO" {
        return Err(SnapshotError::InvalidMagic);
    }
    let device_id = [
        model_snapshot[8],
        model_snapshot[9],
        model_snapshot[10],
        model_snapshot[11],
    ];

    match &device_id {
        b"UHUB" => {
            const TAG_NUM_PORTS: u16 = 5;
            let r = SnapshotReader::parse(model_snapshot, UsbHubDevice::DEVICE_ID)?;
            r.ensure_device_major(UsbHubDevice::DEVICE_VERSION.major)?;

            let num_ports = r.u32(TAG_NUM_PORTS)?.unwrap_or(4);
            if !(1..=u8::MAX as u32).contains(&num_ports) {
                return Err(SnapshotError::InvalidFieldEncoding("hub port count"));
            }

            Ok(Some(Box::new(UsbHubDevice::new_with_ports(
                num_ports as usize,
            ))))
        }
        b"UKBD" => Ok(Some(Box::new(UsbHidKeyboardHandle::new()))),
        b"UMSE" => Ok(Some(Box::new(UsbHidMouseHandle::new()))),
        b"UGPD" => Ok(Some(Box::new(UsbHidGamepadHandle::new()))),
        b"UCMP" => Ok(Some(Box::new(UsbCompositeHidInputHandle::new()))),
        b"HIDP" => Ok(UsbHidPassthroughHandle::try_new_from_snapshot(model_snapshot)?
            .map(|h| Box::new(h) as Box<dyn UsbDeviceModel>)),
        b"WUSB" => Ok(Some(Box::new(crate::UsbWebUsbPassthroughDevice::new()))),
        _ => Ok(None),
    }
}

fn save_usb_device_model_snapshot(model: &dyn UsbDeviceModel) -> Option<Vec<u8>> {
    let any = model as &dyn Any;

    if let Some(dev) = any.downcast_ref::<UsbHubDevice>() {
        return Some(dev.save_state());
    }

    // Prefer handle types where available so host integrations can still clone the handle after
    // downcasting the restored device tree.
    if let Some(dev) = any.downcast_ref::<UsbHidKeyboardHandle>() {
        return Some(dev.save_state());
    }
    if let Some(dev) = any.downcast_ref::<UsbHidMouseHandle>() {
        return Some(dev.save_state());
    }
    if let Some(dev) = any.downcast_ref::<UsbHidGamepadHandle>() {
        return Some(dev.save_state());
    }
    if let Some(dev) = any.downcast_ref::<UsbCompositeHidInputHandle>() {
        return Some(dev.save_state());
    }
    if let Some(dev) = any.downcast_ref::<UsbHidPassthroughHandle>() {
        return Some(dev.save_state());
    }

    // Concrete device types (less ergonomic for host access, but still supported).
    if let Some(dev) = any.downcast_ref::<UsbHidKeyboard>() {
        return Some(dev.save_state());
    }
    if let Some(dev) = any.downcast_ref::<UsbHidMouse>() {
        return Some(dev.save_state());
    }
    if let Some(dev) = any.downcast_ref::<UsbHidGamepad>() {
        return Some(dev.save_state());
    }
    if let Some(dev) = any.downcast_ref::<UsbCompositeHidInput>() {
        return Some(dev.save_state());
    }
    if let Some(dev) = any.downcast_ref::<UsbHidPassthrough>() {
        return Some(dev.save_state());
    }

    if let Some(dev) = any.downcast_ref::<crate::UsbWebUsbPassthroughDevice>() {
        return Some(dev.save_state());
    }

    None
}

fn apply_usb_device_model_snapshot(
    model: &mut dyn UsbDeviceModel,
    bytes: &[u8],
) -> SnapshotResult<()> {
    // `SnapshotReader::parse` requires a concrete expected device ID. Peek at the snapshot header
    // and dispatch to known model types.
    if bytes.len() < 16 {
        return Err(SnapshotError::UnexpectedEof);
    }
    if bytes[0..4] != *b"AERO" {
        return Err(SnapshotError::InvalidMagic);
    }
    let device_id = [bytes[8], bytes[9], bytes[10], bytes[11]];

    let any = model as &mut dyn Any;

    match &device_id {
        b"UHUB" => {
            if let Some(dev) = any.downcast_mut::<UsbHubDevice>() {
                return dev.load_state(bytes);
            }
        }
        b"UKBD" => {
            if let Some(dev) = any.downcast_mut::<UsbHidKeyboardHandle>() {
                return dev.load_state(bytes);
            }
            if let Some(dev) = any.downcast_mut::<UsbHidKeyboard>() {
                return dev.load_state(bytes);
            }
        }
        b"UMSE" => {
            if let Some(dev) = any.downcast_mut::<UsbHidMouseHandle>() {
                return dev.load_state(bytes);
            }
            if let Some(dev) = any.downcast_mut::<UsbHidMouse>() {
                return dev.load_state(bytes);
            }
        }
        b"UGPD" => {
            if let Some(dev) = any.downcast_mut::<UsbHidGamepadHandle>() {
                return dev.load_state(bytes);
            }
            if let Some(dev) = any.downcast_mut::<UsbHidGamepad>() {
                return dev.load_state(bytes);
            }
        }
        b"UCMP" => {
            if let Some(dev) = any.downcast_mut::<UsbCompositeHidInputHandle>() {
                return dev.load_state(bytes);
            }
            if let Some(dev) = any.downcast_mut::<UsbCompositeHidInput>() {
                return dev.load_state(bytes);
            }
        }
        b"HIDP" => {
            if let Some(dev) = any.downcast_mut::<UsbHidPassthroughHandle>() {
                return dev.load_state(bytes);
            }
            if let Some(dev) = any.downcast_mut::<UsbHidPassthrough>() {
                return dev.load_state(bytes);
            }
        }
        b"WUSB" => {
            if let Some(dev) = any.downcast_mut::<crate::UsbWebUsbPassthroughDevice>() {
                return dev.load_state(bytes);
            }
        }
        _ => {}
    }

    Ok(())
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
        let mut dev = AttachedUsbDevice::new(Box::new(AckModel));

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

        // Invalid direction (must be HostToDevice).
        assert_eq!(
            dev.handle_setup(SetupPacket {
                bm_request_type: 0x80, // DeviceToHost | Standard | Device
                b_request: USB_REQUEST_SET_ADDRESS,
                w_value: 5,
                w_index: 0,
                w_length: 0,
            }),
            UsbOutResult::Stall
        );
    }

    #[test]
    fn new_setup_aborts_pending_set_address() {
        let mut dev = AttachedUsbDevice::new(Box::new(AckModel));

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

    #[test]
    fn control_in_zero_length_data_stage_is_delivered() {
        struct ZeroLenModel;

        impl UsbDeviceModel for ZeroLenModel {
            fn handle_control_request(
                &mut self,
                _setup: SetupPacket,
                _data_stage: Option<&[u8]>,
            ) -> ControlResponse {
                ControlResponse::Data(Vec::new())
            }
        }

        let mut dev = AttachedUsbDevice::new(Box::new(ZeroLenModel));
        let setup = SetupPacket {
            bm_request_type: 0x80,
            b_request: 0xff,
            w_value: 0,
            w_index: 0,
            w_length: 8,
        };

        assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
        // IN DATA stage should complete with a ZLP, not STALL.
        assert_eq!(dev.handle_in(0, 8), UsbInResult::Data(Vec::new()));
        // Status OUT stage.
        assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
    }

    #[test]
    fn control_in_ack_with_wlength_produces_zlp_data_stage() {
        struct AckInModel;

        impl UsbDeviceModel for AckInModel {
            fn handle_control_request(
                &mut self,
                _setup: SetupPacket,
                _data_stage: Option<&[u8]>,
            ) -> ControlResponse {
                ControlResponse::Ack
            }
        }

        let mut dev = AttachedUsbDevice::new(Box::new(AckInModel));
        let setup = SetupPacket {
            bm_request_type: 0x80,
            b_request: 0xff,
            w_value: 0,
            w_index: 0,
            w_length: 8,
        };

        assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
        assert_eq!(dev.handle_in(0, 8), UsbInResult::Data(Vec::new()));
        assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
    }

    #[test]
    fn control_in_data_stage_emits_terminating_zlp_when_length_is_multiple_of_packet_size() {
        struct FixedInModel;

        impl UsbDeviceModel for FixedInModel {
            fn handle_control_request(
                &mut self,
                setup: SetupPacket,
                _data_stage: Option<&[u8]>,
            ) -> ControlResponse {
                if setup.bm_request_type == 0xc0 && setup.b_request == 0x10 {
                    ControlResponse::Data((0u8..16).collect())
                } else {
                    ControlResponse::Stall
                }
            }
        }

        // Host requests 64 bytes; device only returns 16 bytes. Since 16 is an exact multiple of
        // the 8-byte packet size used by the host, the device must send a terminating ZLP to
        // indicate the end of the data stage.
        let mut dev = AttachedUsbDevice::new(Box::new(FixedInModel));
        let setup = SetupPacket {
            bm_request_type: 0xc0,
            b_request: 0x10,
            w_value: 0,
            w_index: 0,
            w_length: 64,
        };

        assert_eq!(dev.handle_setup(setup), UsbOutResult::Ack);
        assert_eq!(dev.handle_in(0, 8), UsbInResult::Data((0u8..8).collect()));
        assert_eq!(dev.handle_in(0, 8), UsbInResult::Data((8u8..16).collect()));
        assert_eq!(dev.handle_in(0, 8), UsbInResult::Data(Vec::new()));
        assert_eq!(dev.handle_out(0, &[]), UsbOutResult::Ack);
    }
}
