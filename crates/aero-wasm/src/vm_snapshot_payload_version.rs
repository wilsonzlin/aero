/// Parse the `(version, flags)` metadata pair that Aero stores in the VM snapshot `DeviceState`
/// header for a given device payload blob.
///
/// Snapshot device payloads commonly use an `aero-io-snapshot`-shaped header:
///
/// ```text
/// magic[4] = "AERO"
/// format_version: u16 major, u16 minor
/// device_id: [u8; 4]
/// device_version: u16 major, u16 minor
/// ```
///
/// VM snapshots store the device version pair (`device_version.major`, `device_version.minor`) in
/// the outer `DeviceState` fields `(version, flags)` so snapshot tooling can reason about the inner
/// blob without re-parsing it.
///
/// Some legacy JS-only device snapshots also begin with "AERO" but use a shorter header:
///
/// ```text
/// magic[4] = "AERO"
/// version: u16
/// flags: u16
/// ```
///
/// Additionally, USB snapshot payloads may be a multi-controller container starting with "AUSB":
///
/// ```text
/// magic[4] = "AUSB"
/// version: u16
/// flags: u16
/// ```
///
/// For all of these, this helper returns the `(version, flags)` pair. If the payload does not have
/// a recognized header, this returns `None` and callers should fall back to an appropriate default.
pub(crate) fn parse_vm_snapshot_device_version_flags(bytes: &[u8]) -> Option<(u16, u16)> {
    // USB snapshots may be a multi-controller container starting with the magic "AUSB".
    if bytes.len() >= 8 && &bytes[0..4] == b"AUSB" {
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        let flags = u16::from_le_bytes([bytes[6], bytes[7]]);
        return Some((version, flags));
    }

    // Preferred path: `aero-io-snapshot` format uses a 16-byte header.
    //
    // Detect the io-snapshot header by checking that the 4-byte device id region looks like an
    // ASCII tag, and fall back to the legacy 8-byte `AERO` header otherwise.
    const IO_HEADER_LEN: usize = 16;
    if bytes.len() < 4 || &bytes[0..4] != b"AERO" {
        return None;
    }

    if bytes.len() >= IO_HEADER_LEN {
        let id = &bytes[8..12];
        let is_ascii_tag = id.iter().all(|b| match *b {
            b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'_' => true,
            _ => false,
        });
        if is_ascii_tag {
            let major = u16::from_le_bytes([bytes[12], bytes[13]]);
            let minor = u16::from_le_bytes([bytes[14], bytes[15]]);
            return Some((major, minor));
        }
    }

    if bytes.len() >= 8 {
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        let flags = u16::from_le_bytes([bytes[6], bytes[7]]);
        return Some((version, flags));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ausb_container_header() {
        let bytes = [
            b'A', b'U', b'S', b'B', // magic
            0x34, 0x12, // version = 0x1234
            0x78, 0x56, // flags = 0x5678
        ];
        assert_eq!(
            parse_vm_snapshot_device_version_flags(&bytes),
            Some((0x1234, 0x5678))
        );
    }

    #[test]
    fn parses_aero_io_snapshot_header() {
        // Minimal `aero-io-snapshot`-shaped header (16 bytes).
        let bytes = [
            b'A', b'E', b'R', b'O', // magic
            0x01, 0x00, 0x00, 0x00, // format_version 1.0 (ignored)
            b'U', b'H', b'R', b'T', // device_id tag
            0x03, 0x00, // device_version.major = 3
            0x07, 0x00, // device_version.minor = 7
        ];
        assert_eq!(parse_vm_snapshot_device_version_flags(&bytes), Some((3, 7)));
    }

    #[test]
    fn parses_legacy_aero_header() {
        let bytes = [
            b'A', b'E', b'R', b'O', // magic
            0x02, 0x00, // version = 2
            0x09, 0x00, // flags = 9
        ];
        assert_eq!(parse_vm_snapshot_device_version_flags(&bytes), Some((2, 9)));
    }
}

