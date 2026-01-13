use aero_usb::xhci::{regs, XhciController};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbSpeed};

mod util;

use util::TestMemory;

#[derive(Debug)]
struct DummyFullSpeedDevice;

impl UsbDeviceModel for DummyFullSpeedDevice {
    fn speed(&self) -> UsbSpeed {
        UsbSpeed::Full
    }

    fn handle_control_request(
        &mut self,
        _setup: SetupPacket,
        _data_stage: Option<&[u8]>,
    ) -> ControlResponse {
        ControlResponse::Stall
    }
}

#[test]
fn xhci_exposes_supported_protocol_ext_cap_for_usb2_ports() {
    let mut xhci = XhciController::with_port_count(2);
    let mut mem = TestMemory::new(XhciController::MMIO_SIZE as usize);

    // Walk the extended capability list starting at HCCPARAMS1.xECP.
    let hccparams1 = xhci.mmio_read_u32(&mut mem, regs::cap::HCCPARAMS1 as u64);
    let xecp_dwords = (hccparams1 >> 16) & 0xffff;
    assert_ne!(xecp_dwords, 0, "HCCPARAMS1.xECP must be non-zero");
    let xecp = (xecp_dwords as u64) * 4;

    let cap0 = xhci.mmio_read_u32(&mut mem, xecp);
    assert_eq!(
        (cap0 & 0xff) as u8,
        regs::EXT_CAP_ID_SUPPORTED_PROTOCOL,
        "first extended capability should be Supported Protocol"
    );

    // DWORD3: speed ID count.
    let dword3 = xhci.mmio_read_u32(&mut mem, xecp + 12);
    let psic = (dword3 & 0xf) as u8;
    assert!(psic >= 1, "Supported Protocol should expose at least one speed ID");

    let mut speed_ids = Vec::new();
    for i in 0..psic {
        let psi = xhci.mmio_read_u32(&mut mem, xecp + 16 + (i as u64) * 4);
        speed_ids.push((psi & 0x0f) as u8);
    }

    // Connect a full-speed device and ensure PORTSC.PS reports a speed ID defined by PSI entries.
    xhci.attach_device(0, Box::new(DummyFullSpeedDevice));

    // xHCI port registers begin at OperationalBase (CAPLENGTH) + 0x400 with 0x10 bytes per port.
    let portsc_offset = u64::from(regs::CAPLENGTH_BYTES) + 0x400;
    let portsc = xhci.mmio_read_u32(&mut mem, portsc_offset);
    let ps = ((portsc >> 10) & 0x0f) as u8;
    assert!(
        speed_ids.contains(&ps),
        "PORTSC speed ID {ps} must be defined by Supported Protocol speed IDs {speed_ids:?}"
    );

    assert_eq!(
        ps,
        regs::PSIV_FULL_SPEED,
        "full-speed devices should report the PSIV for the full-speed PSI descriptor"
    );
}
