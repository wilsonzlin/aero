use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use super::{GsiLevelSink, PciConfigPorts, PciIntxRouter};

/// PCI core snapshot wrapper that combines `PciConfigPorts` (`PCPT`) and `PciIntxRouter` (`INTX`)
/// into a single `aero-io-snapshot` TLV blob (inner `PCIC`).
///
/// This exists so snapshot adapters can store PCI core state as a *single* outer snapshot entry
/// (typically `aero_snapshot::DeviceId::PCI`), while still capturing both the config ports state
/// (`PCPT`) and the INTx routing state (`INTX`).
///
/// `aero-snapshot` rejects duplicate `(device_id, version, flags)` tuples in the `DEVICES` section.
/// Since both `PCPT` and `INTX` currently snapshot as `SnapshotVersion (1.0)`, they cannot both be
/// stored as two separate `(DeviceId::PCI, 1, 0)` entries. This wrapper avoids that collision by
/// nesting both io-snapshots under one outer `DeviceId::PCI` entry.
///
/// Backward compatibility note: older snapshot adapters may store PCI core state as split entries
/// using `DeviceId::PCI_CFG` (`PCPT`) and `DeviceId::PCI_INTX` (`INTX`). New snapshot adapters
/// should prefer the single-entry `DeviceId::PCI` + `PCIC` wrapper convention.
///
/// Restore note: the INTx router snapshot captures internal assertion refcounts, but it cannot
/// directly manipulate the platform interrupt sink during `load_state()`. Callers should invoke
/// [`PciIntxRouter::sync_levels_to_sink()`] after restoring both the router and the platform
/// interrupt controller to re-drive asserted GSIs.
pub struct PciCoreSnapshot<'a> {
    cfg_ports: &'a mut PciConfigPorts,
    intx_router: &'a mut PciIntxRouter,
}

impl<'a> PciCoreSnapshot<'a> {
    pub fn new(cfg_ports: &'a mut PciConfigPorts, intx_router: &'a mut PciIntxRouter) -> Self {
        Self {
            cfg_ports,
            intx_router,
        }
    }

    /// Convenience wrapper around [`PciIntxRouter::sync_levels_to_sink()`].
    pub fn sync_intx_levels_to_sink(&self, sink: &mut dyn GsiLevelSink) {
        self.intx_router.sync_levels_to_sink(sink);
    }
}

impl IoSnapshot for PciCoreSnapshot<'_> {
    const DEVICE_ID: [u8; 4] = *b"PCIC";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_CFG_PORTS: u16 = 1;
        const TAG_INTX_ROUTER: u16 = 2;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_bytes(TAG_CFG_PORTS, self.cfg_ports.save_state());
        w.field_bytes(TAG_INTX_ROUTER, self.intx_router.save_state());
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_CFG_PORTS: u16 = 1;
        const TAG_INTX_ROUTER: u16 = 2;

        // `PCPT` can grow to ~20MiB in the worst case (full 256/32/8 PCI topology).
        const MAX_CFG_PORTS_SNAPSHOT_LEN: usize = 32 * 1024 * 1024;
        // `INTX` should stay comfortably under a few MiB, even with many asserted sources.
        const MAX_INTX_ROUTER_SNAPSHOT_LEN: usize = 4 * 1024 * 1024;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(buf) = r.bytes(TAG_CFG_PORTS) {
            if buf.len() > MAX_CFG_PORTS_SNAPSHOT_LEN {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "PCPT snapshot too large",
                ));
            }
            self.cfg_ports.load_state(buf)?;
        }

        if let Some(buf) = r.bytes(TAG_INTX_ROUTER) {
            if buf.len() > MAX_INTX_ROUTER_SNAPSHOT_LEN {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "INTX snapshot too large",
                ));
            }
            self.intx_router.load_state(buf)?;
        }

        Ok(())
    }
}
