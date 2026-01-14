use aero_usb::xhci::regs::*;
use aero_usb::xhci::XhciController;

mod util;

use util::TestMemory;

fn find_ext_cap(
    xhci: &mut XhciController,
    mem: &mut TestMemory,
    first: u64,
    id: u8,
) -> Option<u64> {
    // xHCI Extended Capabilities form a linked list. The "next" pointer is expressed in DWORDs.
    // Keep a small iteration bound so malformed guest-visible structures cannot loop forever.
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

#[test]
fn hccparams1_xecp_points_to_extended_capabilities() {
    let mut xhci = XhciController::with_port_count(4);
    let mut mem = TestMemory::new(XhciController::MMIO_SIZE as usize);

    let hccparams1 = xhci.mmio_read_u32(&mut mem, cap::HCCPARAMS1 as u64);
    assert_eq!(
        hccparams1 & HCCPARAMS1_CSZ_64B,
        0,
        "MVP assumes 32-byte contexts (HCCPARAMS1.CSZ=0)"
    );
    let xecp_dwords = (hccparams1 >> 16) & 0xffff;
    assert_ne!(xecp_dwords, 0, "HCCPARAMS1.xECP must be non-zero");

    let xecp_bytes = (xecp_dwords as u64) * 4;
    assert!(
        xecp_bytes < XhciController::MMIO_SIZE as u64,
        "xECP must point within the MMIO BAR"
    );
}

#[test]
fn supported_protocol_capability_usb2_matches_port_count() {
    let port_count = 4u8;
    let mut xhci = XhciController::with_port_count(port_count);
    let mut mem = TestMemory::new(XhciController::MMIO_SIZE as usize);

    let hccparams1 = xhci.mmio_read_u32(&mut mem, cap::HCCPARAMS1 as u64);
    let xecp = (((hccparams1 >> 16) & 0xffff) as u64) * 4;
    let Some(xecp) = find_ext_cap(&mut xhci, &mut mem, xecp, EXT_CAP_ID_SUPPORTED_PROTOCOL) else {
        panic!("missing Supported Protocol extended capability");
    };

    // Extended capability header (DWORD0).
    let cap0 = xhci.mmio_read_u32(&mut mem, xecp);
    assert_eq!(
        (cap0 & 0xff) as u8,
        EXT_CAP_ID_SUPPORTED_PROTOCOL,
        "expected a Supported Protocol capability"
    );
    assert_eq!(
        ((cap0 >> 8) & 0xff) as u8,
        0,
        "Supported Protocol capability should terminate the list (next=0)"
    );
    assert_eq!(
        ((cap0 >> 16) & 0xffff) as u16,
        USB_REVISION_2_0,
        "USB2 Supported Protocol revision should be 0x0200 (USB 2.0)"
    );

    // DWORD1: protocol name string.
    let name = xhci.mmio_read_u32(&mut mem, xecp + 4);
    assert_eq!(name, PROTOCOL_NAME_USB2);

    // DWORD2: port offset + count.
    let ports = xhci.mmio_read_u32(&mut mem, xecp + 8);
    let port_offset = (ports & 0xff) as u8;
    let port_count_cap = ((ports >> 8) & 0xff) as u8;
    assert_eq!(port_offset, 1, "port offset is 1-based");
    assert_eq!(port_count_cap, port_count);

    // DWORD3: speed ID count.
    let dword3 = xhci.mmio_read_u32(&mut mem, xecp + 12);
    let psic = (dword3 & 0xf) as u8;
    let psio = ((dword3 >> 16) & 0xffff) as u16;
    assert_eq!(
        psio, 4,
        "PSI descriptor table should start at DWORD4 (PSIO=4)"
    );
    assert!(
        psic >= 2,
        "USB2 Supported Protocol should expose at least LS+FS PSI descriptors"
    );

    // Ensure we included low-speed and full-speed entries (drivers use PSIT to map PORTSC.PS).
    let mut has_low = false;
    let mut has_full = false;
    let psi_base = xecp + u64::from(psio) * 4;
    for i in 0..psic {
        let psi = xhci.mmio_read_u32(&mut mem, psi_base + (i as u64) * 4);
        let psit = ((psi >> 4) & 0xf) as u8;
        if psit == PSI_TYPE_LOW {
            has_low = true;
        }
        if psit == PSI_TYPE_FULL {
            has_full = true;
        }
    }
    assert!(has_low, "missing low-speed PSI descriptor");
    assert!(has_full, "missing full-speed PSI descriptor");
}
