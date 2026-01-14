use aero_usb::xhci::{regs, XhciController};
use aero_usb::{ControlResponse, SetupPacket, UsbDeviceModel, UsbSpeed};

mod util;

use util::TestMemory;

fn find_ext_cap(
    xhci: &mut XhciController,
    mem: &mut TestMemory,
    first: u64,
    id: u8,
) -> Option<u64> {
    let mut off = first;
    for _ in 0..32 {
        if off == 0 {
            return None;
        }
        let cap0 = xhci.mmio_read_u32(mem, off);
        let cap_id = (cap0 & 0xff) as u8;
        if cap_id == id {
            return Some(off);
        }
        let next = ((cap0 >> 8) & 0xff) as u64;
        if next == 0 {
            return None;
        }
        off = next * 4;
    }
    None
}

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
    let Some(xecp) = find_ext_cap(
        &mut xhci,
        &mut mem,
        xecp,
        regs::EXT_CAP_ID_SUPPORTED_PROTOCOL,
    ) else {
        panic!("missing Supported Protocol extended capability");
    };

    let cap0 = xhci.mmio_read_u32(&mut mem, xecp);
    assert_eq!(
        (cap0 & 0xff) as u8,
        regs::EXT_CAP_ID_SUPPORTED_PROTOCOL,
        "expected a Supported Protocol capability"
    );

    // DWORD3: speed ID count.
    let dword3 = xhci.mmio_read_u32(&mut mem, xecp + 12);
    let psic = (dword3 & 0xf) as u8;
    let psio = ((dword3 >> 16) & 0xffff) as u16;
    assert!(
        psic >= 1,
        "Supported Protocol should expose at least one speed ID"
    );
    assert_eq!(
        psio, 4,
        "PSI descriptor table should begin immediately after DWORD3 (PSIO=4)"
    );

    let mut speed_ids = Vec::new();
    let psi_base = xecp + u64::from(psio) * 4;
    for i in 0..psic {
        let psi = xhci.mmio_read_u32(&mut mem, psi_base + (i as u64) * 4);
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
