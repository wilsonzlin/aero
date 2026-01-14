use aero_usb::xhci::context::{XhciRouteString, XhciRouteStringError, XHCI_ROUTE_STRING_MAX_DEPTH};

#[test]
fn route_string_encodes_ports_as_4_bit_values() {
    // Device directly attached to a root port => route string 0.
    let rs = XhciRouteString::from_raw(0).unwrap();
    assert_eq!(rs.ports_from_root(), Vec::<u8>::new());
    assert_eq!(rs.ports_to_root(), Vec::<u8>::new());

    // One hub tier.
    let rs = XhciRouteString::encode_from_root(&[3]).unwrap();
    assert_eq!(rs.raw(), 0x3);
    assert_eq!(rs.ports_from_root(), vec![3]);

    // Two hub tiers: root -> port2 -> port5 -> device
    let rs = XhciRouteString::encode_from_root(&[2, 5]).unwrap();
    // Route String stores the port closest to the device in the least significant nibble.
    assert_eq!(rs.raw(), 0x25);
    assert_eq!(rs.ports_from_root(), vec![2, 5]);
    assert_eq!(rs.ports_to_root(), vec![5, 2]);

    // Invalid port numbers: 0 is reserved for the terminator, and 16 doesn't fit in a nibble.
    assert_eq!(
        XhciRouteString::encode_from_root(&[0]).unwrap_err(),
        XhciRouteStringError::InvalidPort { port: 0, max: 15 }
    );
    assert_eq!(
        XhciRouteString::encode_from_root(&[16]).unwrap_err(),
        XhciRouteStringError::InvalidPort { port: 16, max: 15 }
    );
}

#[test]
fn route_string_rejects_gaps_and_overflow() {
    // Bits outside the 20-bit field are not representable in the slot context.
    assert_eq!(
        XhciRouteString::from_raw(0x01_0000_00),
        Err(XhciRouteStringError::OutOfRange(0x01_0000_00))
    );

    // Encountering a terminator then seeing a later non-zero nibble indicates a "hole" in the
    // route, which is not representable by xHCI.
    assert_eq!(
        XhciRouteString::from_raw(0x020),
        Err(XhciRouteStringError::NonZeroAfterTerminator(0x020))
    );

    // Depth cannot exceed 5 tiers.
    let too_deep = vec![1u8; XHCI_ROUTE_STRING_MAX_DEPTH + 1];
    assert_eq!(
        XhciRouteString::encode_from_root(&too_deep).unwrap_err(),
        XhciRouteStringError::TooDeep {
            depth: XHCI_ROUTE_STRING_MAX_DEPTH + 1,
            max: XHCI_ROUTE_STRING_MAX_DEPTH,
        }
    );
}
