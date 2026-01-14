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
        let is_ascii_tag = id
            .iter()
            .all(|b| matches!(*b, b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b'_'));
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

    #[test]
    fn parses_gpu_vram_chunk_header_as_legacy_aero() {
        // `gpu.vram` chunk payloads begin with an 8-byte legacy `AERO` header so the snapshot builder
        // can derive distinct `(version, flags)` tuples for each chunk (flags = chunk_index).
        //
        // The payload is larger than 16 bytes and includes non-ASCII bytes in the io-snapshot
        // device-id slot (`bytes[8..12]`), so this should *not* be interpreted as an
        // `aero-io-snapshot` TLV header.
        let mut bytes = [0u8; 24];
        bytes[0..4].copy_from_slice(b"AERO");
        // version = 1, flags = 7
        bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
        bytes[6..8].copy_from_slice(&7u16.to_le_bytes());
        // Non-ASCII marker in the would-be device_id slot (matches `gpu.vram` chunk magic: 0x01415256).
        bytes[8..12].copy_from_slice(&[0x56, 0x52, 0x41, 0x01]);
        // Would be interpreted as device_version by the io-snapshot parser; ensure we don't use it.
        bytes[12..16].copy_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd]);

        assert_eq!(parse_vm_snapshot_device_version_flags(&bytes), Some((1, 7)));
    }
}
