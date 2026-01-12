use crate::io::state::codec::Decoder;
use crate::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use std::collections::BTreeMap;

/// Canonical disk controller snapshot wrapper (`DSKC`).
///
/// This nests multiple controller `aero-io-snapshot` blobs keyed by PCI BDF (bus/device/function).
/// The TLV tag is the packed BDF:
///
/// ```text
/// tag = (bus << 8) | (device << 3) | function
/// ```
///
/// This wrapper exists because `aero-snapshot` enforces uniqueness on the outer
/// `(DeviceId, version, flags)` tuple. Many storage controllers start at io-snapshot version
/// `1.0`, so storing multiple controllers as separate `DeviceId::DISK_CONTROLLER` entries would
/// collide.
///
/// Restore code is expected to ignore unknown/extra controller entries if the target machine does
/// not contain that controller.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DiskControllersSnapshot {
    controllers: BTreeMap<u16, Vec<u8>>,
}

impl DiskControllersSnapshot {
    /// Legacy encoding used by some older snapshots: the controller map is stored as a single
    /// field (tag=1) containing a length-prefixed vector of `(bdf_u16, snapshot_bytes)` entries.
    ///
    /// New encoders should not use this format (it cannot naturally represent a controller at
    /// packed BDF `0x0001` without ambiguity), but decoders should continue to accept it for
    /// backward compatibility.
    const LEGACY_TAG_CONTROLLERS: u16 = 1;

    /// Defensive limit to keep snapshot decoding bounded when parsing legacy wrapper payloads.
    const MAX_LEGACY_CONTROLLER_COUNT: usize = 256;

    /// Defensive limit to keep snapshot decoding bounded when parsing legacy wrapper payloads.
    const MAX_LEGACY_CONTROLLER_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;

    /// Pack a PCI BDF triplet into the canonical `DSKC` field tag.
    #[inline]
    pub const fn bdf_tag(bus: u8, device: u8, function: u8) -> u16 {
        ((bus as u16) << 8) | (((device as u16) & 0x1F) << 3) | ((function as u16) & 0x07)
    }

    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, bdf_tag: u16, snapshot: Vec<u8>) -> Option<Vec<u8>> {
        self.controllers.insert(bdf_tag, snapshot)
    }

    pub fn get(&self, bdf_tag: u16) -> Option<&[u8]> {
        self.controllers.get(&bdf_tag).map(|v| v.as_slice())
    }

    pub fn iter(&self) -> std::collections::btree_map::Iter<'_, u16, Vec<u8>> {
        self.controllers.iter()
    }

    pub fn controllers(&self) -> &BTreeMap<u16, Vec<u8>> {
        &self.controllers
    }

    pub fn controllers_mut(&mut self) -> &mut BTreeMap<u16, Vec<u8>> {
        &mut self.controllers
    }

    pub fn is_empty(&self) -> bool {
        self.controllers.is_empty()
    }

    pub fn len(&self) -> usize {
        self.controllers.len()
    }

    fn load_legacy_controllers_field(&mut self, field: &[u8]) -> SnapshotResult<()> {
        let mut out: BTreeMap<u16, Vec<u8>> = BTreeMap::new();

        let mut d = Decoder::new(field);

        // We manually decode the vector to validate the declared count/lengths before any large
        // allocations.
        let count = d.u32()? as usize;
        if count > Self::MAX_LEGACY_CONTROLLER_COUNT {
            return Err(SnapshotError::InvalidFieldEncoding("disk controller count"));
        }

        for _ in 0..count {
            let len = d.u32()? as usize;
            if len < 2 {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "disk controller entry too short",
                ));
            }
            if len - 2 > Self::MAX_LEGACY_CONTROLLER_SNAPSHOT_BYTES {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "disk controller snapshot too large",
                ));
            }
            let entry = d.bytes(len)?;
            let bdf = u16::from_le_bytes([entry[0], entry[1]]);
            let nested = entry[2..].to_vec();
            if out.insert(bdf, nested).is_some() {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "disk controller duplicate bdf",
                ));
            }
        }

        d.finish()?;

        self.controllers = out;
        Ok(())
    }
}

impl IoSnapshot for DiskControllersSnapshot {
    const DEVICE_ID: [u8; 4] = *b"DSKC";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        for (&bdf, snap) in &self.controllers {
            w.field_bytes(bdf, snap.clone());
        }
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        // Snapshots may be loaded from untrusted sources; keep nested payloads bounded.
        const MAX_CONTROLLER_SNAPSHOT_LEN: usize = 64 * 1024 * 1024;

        self.controllers.clear();

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Backward compatibility: older snapshots stored all controllers inside a single field
        // (tag=1). Detect that shape and decode it if present.
        //
        // We only treat this as legacy when there is a single field with tag=1 and the payload
        // does not look like an `aero-io-snapshot` blob (which always starts with `b"AERO"`).
        if let Some(legacy_field) = r.bytes(Self::LEGACY_TAG_CONTROLLERS) {
            if r.iter_fields().count() == 1 && !legacy_field.starts_with(b"AERO") {
                return self.load_legacy_controllers_field(legacy_field);
            }
        }

        for (tag, field) in r.iter_fields() {
            if field.len() > MAX_CONTROLLER_SNAPSHOT_LEN {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "DSKC controller snapshot too large",
                ));
            }
            self.controllers.insert(tag, field.to_vec());
        }

        Ok(())
    }
}
