use aero_snapshot::DeviceId;

pub(crate) const DEVICE_KIND_USB_UHCI: &str = "usb.uhci";
pub(crate) const DEVICE_KIND_I8042: &str = "input.i8042";
pub(crate) const DEVICE_KIND_AUDIO_HDA: &str = "audio.hda";
pub(crate) const DEVICE_KIND_NET_E1000: &str = "net.e1000";
pub(crate) const DEVICE_KIND_NET_STACK: &str = "net.stack";
pub(crate) const DEVICE_KIND_PREFIX_ID: &str = "device.";

pub(crate) fn parse_device_kind(kind: &str) -> Option<DeviceId> {
    if kind == DEVICE_KIND_USB_UHCI {
        return Some(DeviceId::USB);
    }
    if kind == DEVICE_KIND_I8042 {
        return Some(DeviceId::I8042);
    }
    if kind == DEVICE_KIND_AUDIO_HDA {
        return Some(DeviceId::HDA);
    }
    if kind == DEVICE_KIND_NET_E1000 {
        return Some(DeviceId::E1000);
    }
    if kind == DEVICE_KIND_NET_STACK {
        return Some(DeviceId::NET_STACK);
    }

    // For forward compatibility, unknown device ids can be surfaced as `device.<id>` strings.
    // When a snapshot is re-saved, accept this spelling and preserve the numeric device id.
    //
    // Grammar note: the JS side treats `device.<id>` as a stable wire spelling and intentionally
    // restricts `<id>` to ASCII decimal digits (no `+`, no `-`) so roundtrips are deterministic and
    // unambiguous. Keep Rust consistent.
    if let Some(rest) = kind.strip_prefix(DEVICE_KIND_PREFIX_ID)
        && rest.chars().all(|c| c.is_ascii_digit())
        && let Ok(id) = rest.parse::<u32>()
    {
        return Some(DeviceId(id));
    }

    None
}

pub(crate) fn kind_from_device_id(id: DeviceId) -> String {
    if id == DeviceId::USB {
        return DEVICE_KIND_USB_UHCI.to_string();
    }
    if id == DeviceId::I8042 {
        return DEVICE_KIND_I8042.to_string();
    }
    if id == DeviceId::HDA {
        return DEVICE_KIND_AUDIO_HDA.to_string();
    }
    if id == DeviceId::E1000 {
        return DEVICE_KIND_NET_E1000.to_string();
    }
    if id == DeviceId::NET_STACK {
        return DEVICE_KIND_NET_STACK.to_string();
    }

    // For unknown device ids, preserve them as `device.<id>` entries so callers can roundtrip them
    // back through `vm_snapshot_save(_to_opfs)` without losing state.
    format!("{DEVICE_KIND_PREFIX_ID}{}", id.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_device_kind_accepts_known_device_kinds() {
        assert_eq!(parse_device_kind("usb.uhci"), Some(DeviceId::USB));
        assert_eq!(parse_device_kind("input.i8042"), Some(DeviceId::I8042));
        assert_eq!(parse_device_kind("audio.hda"), Some(DeviceId::HDA));
        assert_eq!(parse_device_kind("net.e1000"), Some(DeviceId::E1000));
        assert_eq!(parse_device_kind("net.stack"), Some(DeviceId::NET_STACK));
    }

    #[test]
    fn parse_device_kind_accepts_device_dot_u32_ids() {
        assert_eq!(parse_device_kind("device.0"), Some(DeviceId(0)));
        // Parsers may accept leading zeros; encoders should canonicalize them away.
        assert_eq!(parse_device_kind("device.00999"), Some(DeviceId(999)));
        assert_eq!(
            parse_device_kind("device.4294967295"),
            Some(DeviceId(u32::MAX))
        );
    }

    #[test]
    fn parse_device_kind_rejects_invalid_device_dot_ids() {
        assert_eq!(parse_device_kind("device."), None);
        assert_eq!(parse_device_kind("device.4294967296"), None);
        assert_eq!(parse_device_kind("device.-1"), None);
        assert_eq!(parse_device_kind("device.+1"), None);
        assert_eq!(parse_device_kind("device.1x"), None);
    }

    #[test]
    fn kind_from_device_id_returns_stable_kinds() {
        assert_eq!(kind_from_device_id(DeviceId::USB), "usb.uhci");
        assert_eq!(kind_from_device_id(DeviceId::I8042), "input.i8042");
        assert_eq!(kind_from_device_id(DeviceId::HDA), "audio.hda");
        assert_eq!(kind_from_device_id(DeviceId::E1000), "net.e1000");
        assert_eq!(kind_from_device_id(DeviceId::NET_STACK), "net.stack");
        assert_eq!(kind_from_device_id(DeviceId(1234)), "device.1234");
    }
}
