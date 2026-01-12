use crate::{DeviceId, DeviceState};

use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotHeader, SnapshotResult, SnapshotVersion,
};

const IO_SNAPSHOT_MAGIC: [u8; 4] = *b"AERO";
const IO_SNAPSHOT_HEADER_LEN: usize = 4 + 2 + 2 + 4 + 2 + 2;
const IO_SNAPSHOT_FORMAT_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

fn parse_io_snapshot_header(
    bytes: &[u8],
    expected_device_id: [u8; 4],
) -> SnapshotResult<SnapshotHeader> {
    if bytes.len() < IO_SNAPSHOT_HEADER_LEN {
        return Err(SnapshotError::UnexpectedEof);
    }

    if bytes[0..4] != IO_SNAPSHOT_MAGIC {
        return Err(SnapshotError::InvalidMagic);
    }

    let format_version = SnapshotVersion {
        major: u16::from_le_bytes([bytes[4], bytes[5]]),
        minor: u16::from_le_bytes([bytes[6], bytes[7]]),
    };
    if format_version.major != IO_SNAPSHOT_FORMAT_VERSION.major {
        return Err(SnapshotError::UnsupportedFormatVersion {
            found: format_version,
            supported: IO_SNAPSHOT_FORMAT_VERSION,
        });
    }

    let device_id = [bytes[8], bytes[9], bytes[10], bytes[11]];
    // Backward compatibility: a few early device snapshot encodings accidentally used a different
    // 4CC in the `aero-io-snapshot` header. We continue accepting those legacy ids when applying a
    // snapshot blob to the correct device type.
    //
    // Note: keep this mapping minimal; `DEVICE_ID` is part of the stable on-disk contract.
    const LEGACY_NET_STACK_DEVICE_ID: [u8; 4] = [0x4e, 0x53, 0x54, 0x4b];
    let ok = device_id == expected_device_id
        || (expected_device_id == *b"NETS" && device_id == LEGACY_NET_STACK_DEVICE_ID);
    if !ok {
        return Err(SnapshotError::DeviceIdMismatch {
            expected: expected_device_id,
            found: device_id,
        });
    }

    let device_version = SnapshotVersion {
        major: u16::from_le_bytes([bytes[12], bytes[13]]),
        minor: u16::from_le_bytes([bytes[14], bytes[15]]),
    };

    Ok(SnapshotHeader {
        format_version,
        device_id,
        device_version,
    })
}

pub fn device_state_from_io_snapshot<T: IoSnapshot>(outer_id: DeviceId, dev: &T) -> DeviceState {
    DeviceState {
        id: outer_id,
        version: T::DEVICE_VERSION.major,
        flags: T::DEVICE_VERSION.minor,
        data: dev.save_state(),
    }
}

pub fn apply_io_snapshot_to_device<T: IoSnapshot>(
    state: &DeviceState,
    dev: &mut T,
) -> SnapshotResult<()> {
    let header = parse_io_snapshot_header(&state.data, T::DEVICE_ID)?;

    if state.version != header.device_version.major || state.flags != header.device_version.minor {
        return Err(SnapshotError::InvalidFieldEncoding(
            "aero-snapshot DeviceState version/flags mismatch",
        ));
    }

    if header.device_version.major != T::DEVICE_VERSION.major {
        return Err(SnapshotError::UnsupportedDeviceMajorVersion {
            found: header.device_version.major,
            supported: T::DEVICE_VERSION.major,
        });
    }

    dev.load_state(&state.data)
}
