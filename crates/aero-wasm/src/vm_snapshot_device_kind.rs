use aero_snapshot::DeviceId;

/// Canonical VM snapshot device kind for the browser USB stack.
///
/// This kind intentionally does *not* encode a specific host/guest controller model (UHCI/xHCI/etc)
/// so snapshots remain controller-agnostic.
pub(crate) const DEVICE_KIND_USB: &str = "usb";
/// Legacy VM snapshot device kind that older builds emitted for `DeviceId::USB`.
pub(crate) const DEVICE_KIND_USB_UHCI: &str = "usb.uhci";
pub(crate) const DEVICE_KIND_I8042: &str = "input.i8042";
pub(crate) const DEVICE_KIND_VIRTIO_INPUT: &str = "input.virtio_input";
pub(crate) const DEVICE_KIND_AUDIO_HDA: &str = "audio.hda";
pub(crate) const DEVICE_KIND_AUDIO_VIRTIO_SND: &str = "audio.virtio_snd";
pub(crate) const DEVICE_KIND_GPU_AEROGPU: &str = "gpu.aerogpu";
pub(crate) const DEVICE_KIND_NET_E1000: &str = "net.e1000";
pub(crate) const DEVICE_KIND_NET_VIRTIO_NET: &str = "net.virtio_net";
pub(crate) const DEVICE_KIND_NET_STACK: &str = "net.stack";
pub(crate) const DEVICE_KIND_PREFIX_ID: &str = "device.";

pub(crate) fn parse_device_kind(kind: &str) -> Option<DeviceId> {
    if kind == DEVICE_KIND_USB || kind == DEVICE_KIND_USB_UHCI {
        return Some(DeviceId::USB);
    }
    if kind == DEVICE_KIND_I8042 {
        return Some(DeviceId::I8042);
    }
    if kind == DEVICE_KIND_VIRTIO_INPUT {
        return Some(DeviceId::VIRTIO_INPUT);
    }
    if kind == DEVICE_KIND_AUDIO_HDA {
        return Some(DeviceId::HDA);
    }
    if kind == DEVICE_KIND_AUDIO_VIRTIO_SND {
        return Some(DeviceId::VIRTIO_SND);
    }
    if kind == DEVICE_KIND_GPU_AEROGPU {
        return Some(DeviceId::AEROGPU);
    }
    if kind == DEVICE_KIND_NET_E1000 {
        return Some(DeviceId::E1000);
    }
    if kind == DEVICE_KIND_NET_VIRTIO_NET {
        return Some(DeviceId::VIRTIO_NET);
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
    let rest = kind.strip_prefix(DEVICE_KIND_PREFIX_ID)?;
    if !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    rest.parse::<u32>().ok().map(DeviceId)
}

pub(crate) fn kind_from_device_id(id: DeviceId) -> String {
    if id == DeviceId::USB {
        return DEVICE_KIND_USB.to_string();
    }
    if id == DeviceId::I8042 {
        return DEVICE_KIND_I8042.to_string();
    }
    if id == DeviceId::VIRTIO_INPUT {
        return DEVICE_KIND_VIRTIO_INPUT.to_string();
    }
    if id == DeviceId::HDA {
        return DEVICE_KIND_AUDIO_HDA.to_string();
    }
    if id == DeviceId::VIRTIO_SND {
        return DEVICE_KIND_AUDIO_VIRTIO_SND.to_string();
    }
    if id == DeviceId::AEROGPU {
        return DEVICE_KIND_GPU_AEROGPU.to_string();
    }
    if id == DeviceId::E1000 {
        return DEVICE_KIND_NET_E1000.to_string();
    }
    if id == DeviceId::VIRTIO_NET {
        return DEVICE_KIND_NET_VIRTIO_NET.to_string();
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
        assert_eq!(parse_device_kind("usb"), Some(DeviceId::USB));
        // Legacy alias accepted for backwards compatibility.
        assert_eq!(parse_device_kind("usb.uhci"), Some(DeviceId::USB));
        assert_eq!(parse_device_kind("input.i8042"), Some(DeviceId::I8042));
        assert_eq!(
            parse_device_kind("input.virtio_input"),
            Some(DeviceId::VIRTIO_INPUT)
        );
        assert_eq!(parse_device_kind("audio.hda"), Some(DeviceId::HDA));
        assert_eq!(
            parse_device_kind("audio.virtio_snd"),
            Some(DeviceId::VIRTIO_SND)
        );
        assert_eq!(parse_device_kind("gpu.aerogpu"), Some(DeviceId::AEROGPU));
        assert_eq!(parse_device_kind("net.e1000"), Some(DeviceId::E1000));
        assert_eq!(
            parse_device_kind("net.virtio_net"),
            Some(DeviceId::VIRTIO_NET)
        );
        assert_eq!(parse_device_kind("net.stack"), Some(DeviceId::NET_STACK));
    }

    #[test]
    fn parse_device_kind_legacy_usb_alias_canonicalizes_on_encode() {
        let id = parse_device_kind(DEVICE_KIND_USB_UHCI).expect("legacy usb.uhci should parse");
        assert_eq!(id, DeviceId::USB);
        assert_eq!(
            kind_from_device_id(id),
            DEVICE_KIND_USB,
            "encoding should emit canonical usb kind"
        );
    }

    #[test]
    fn parse_device_kind_accepts_device_dot_u32_ids() {
        assert_eq!(parse_device_kind("device.0"), Some(DeviceId(0)));
        // Parsers may accept leading zeros; encoders should canonicalize them away.
        assert_eq!(parse_device_kind("device.00999"), Some(DeviceId(999)));
        // Known numeric ids should still map to canonical string kinds when re-encoded.
        assert_eq!(
            kind_from_device_id(parse_device_kind("device.12").expect("device.12 parses")),
            DEVICE_KIND_USB
        );
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
        assert_eq!(kind_from_device_id(DeviceId::USB), "usb");
        assert_eq!(kind_from_device_id(DeviceId::I8042), "input.i8042");
        assert_eq!(
            kind_from_device_id(DeviceId::VIRTIO_INPUT),
            "input.virtio_input"
        );
        assert_eq!(kind_from_device_id(DeviceId::HDA), "audio.hda");
        assert_eq!(
            kind_from_device_id(DeviceId::VIRTIO_SND),
            "audio.virtio_snd"
        );
        assert_eq!(kind_from_device_id(DeviceId::AEROGPU), "gpu.aerogpu");
        assert_eq!(kind_from_device_id(DeviceId::E1000), "net.e1000");
        assert_eq!(kind_from_device_id(DeviceId::VIRTIO_NET), "net.virtio_net");
        assert_eq!(kind_from_device_id(DeviceId::NET_STACK), "net.stack");
        assert_eq!(kind_from_device_id(DeviceId(1234)), "device.1234");
    }
}
