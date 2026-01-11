use emulator::io::usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbInResult};

#[derive(Default)]
struct OversizedInterruptIn;

impl UsbDeviceModel for OversizedInterruptIn {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    fn handle_interrupt_in(&mut self, _ep_addr: u8) -> UsbInResult {
        UsbInResult::Data(vec![0u8; 16])
    }
}

#[derive(Default)]
struct OversizedLegacyPoll;

impl UsbDeviceModel for OversizedLegacyPoll {
    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }

    #[allow(deprecated)]
    fn poll_interrupt_in(&mut self, _ep: u8) -> Option<Vec<u8>> {
        Some(vec![0u8; 16])
    }
}

#[test]
fn handle_in_transfer_truncates_handle_interrupt_in_results() {
    let mut dev = OversizedInterruptIn::default();
    assert_eq!(
        dev.handle_in_transfer(0x81, 8),
        UsbInResult::Data(vec![0u8; 8])
    );
}

#[test]
fn handle_in_transfer_truncates_legacy_poll_interrupt_in_results() {
    let mut dev = OversizedLegacyPoll::default();
    assert_eq!(
        dev.handle_in_transfer(0x81, 8),
        UsbInResult::Data(vec![0u8; 8])
    );
}
