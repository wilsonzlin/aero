#![cfg(not(target_arch = "wasm32"))]

use std::fs;
use std::io::Cursor;

use aero_snapshot::{
    save_snapshot, CpuState, DeviceId, DeviceState, DiskOverlayRefs, MmuState, SaveOptions,
    SnapshotMeta, SnapshotSource,
};
use assert_cmd::Command;

fn io_snapshot_header(id: &[u8; 4], device_version: (u16, u16)) -> Vec<u8> {
    let (major, minor) = device_version;
    let mut out = vec![0u8; 16];
    out[0..4].copy_from_slice(b"AERO");
    // io-snapshot format v1.0
    out[4..6].copy_from_slice(&1u16.to_le_bytes());
    out[6..8].copy_from_slice(&0u16.to_le_bytes());
    out[8..12].copy_from_slice(id);
    // device snapshot version
    out[12..14].copy_from_slice(&major.to_le_bytes());
    out[14..16].copy_from_slice(&minor.to_le_bytes());
    out
}

struct UsbcMultiControllerSource;

impl SnapshotSource for UsbcMultiControllerSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 100,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: Some("inspect-usbc-multi-controller".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        let nested_uhci = io_snapshot_header(b"UHCP", (1, 0));
        let nested_ehci = io_snapshot_header(b"EHCP", (1, 0));
        let nested_xhci = io_snapshot_header(b"XHCP", (1, 0));

        let mut data = io_snapshot_header(b"USBC", (1, 1));
        // UHCI remainder + nested bytes.
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&500_000u64.to_le_bytes());
        data.extend_from_slice(&2u16.to_le_bytes());
        data.extend_from_slice(&(nested_uhci.len() as u32).to_le_bytes());
        data.extend_from_slice(&nested_uhci);
        // EHCI remainder + nested bytes.
        data.extend_from_slice(&3u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&250_000u64.to_le_bytes());
        data.extend_from_slice(&4u16.to_le_bytes());
        data.extend_from_slice(&(nested_ehci.len() as u32).to_le_bytes());
        data.extend_from_slice(&nested_ehci);
        // xHCI remainder + nested bytes (newer snapshots may include this).
        data.extend_from_slice(&5u16.to_le_bytes());
        data.extend_from_slice(&8u32.to_le_bytes());
        data.extend_from_slice(&125_000u64.to_le_bytes());
        data.extend_from_slice(&6u16.to_le_bytes());
        data.extend_from_slice(&(nested_xhci.len() as u32).to_le_bytes());
        data.extend_from_slice(&nested_xhci);

        vec![DeviceState {
            id: DeviceId::USB,
            version: 1,
            flags: 1,
            data,
        }]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        4096
    }

    fn read_ram(&self, _offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        buf.fill(0);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

#[test]
fn snapshot_inspect_decodes_usbc_multi_controller_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("usbc_multi.aerosnap");

    let mut source = UsbcMultiControllerSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    fs::write(&snap, cursor.into_inner()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("inner=USBC"));
    assert!(stdout.contains("nested=UHCP"));
    assert!(stdout.contains("nested=EHCP"));
}

struct AusbContainerSource;

impl SnapshotSource for AusbContainerSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta {
            snapshot_id: 101,
            parent_snapshot_id: Some(100),
            created_unix_ms: 0,
            label: Some("inspect-ausb-container".to_string()),
        }
    }

    fn cpu_state(&self) -> CpuState {
        CpuState::default()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        let uhci = io_snapshot_header(b"UHCP", (1, 0));
        let xhci = io_snapshot_header(b"XHCP", (1, 0));

        // Encode a minimal `AUSB` container with tags out of order to ensure `inspect` prints a
        // deterministic, sorted view.
        let mut data = Vec::new();
        data.extend_from_slice(&0x4253_5541u32.to_le_bytes()); // "AUSB"
        data.extend_from_slice(&1u16.to_le_bytes()); // version
        data.extend_from_slice(&0u16.to_le_bytes()); // flags

        // Entry: XHCI.
        data.extend_from_slice(b"XHCI");
        data.extend_from_slice(&(xhci.len() as u32).to_le_bytes());
        data.extend_from_slice(&xhci);

        // Entry: UHCI.
        data.extend_from_slice(b"UHCI");
        data.extend_from_slice(&(uhci.len() as u32).to_le_bytes());
        data.extend_from_slice(&uhci);

        vec![DeviceState {
            id: DeviceId::USB,
            version: 1,
            flags: 0,
            data,
        }]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        4096
    }

    fn read_ram(&self, _offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        buf.fill(0);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

#[test]
fn snapshot_inspect_decodes_ausb_container_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let snap = tmp.path().join("ausb_container.aerosnap");

    let mut source = AusbContainerSource;
    let mut cursor = Cursor::new(Vec::new());
    save_snapshot(&mut cursor, &mut source, SaveOptions::default()).unwrap();
    fs::write(&snap, cursor.into_inner()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["snapshot", "inspect", snap.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("AUSB"));
    assert!(stdout.contains("UHCI len="));
    assert!(stdout.contains("XHCI len="));
    assert!(stdout.contains("nested=UHCP"));
    assert!(stdout.contains("nested=XHCP"));

    // Deterministic listing order.
    let uhci_pos = stdout.find("entries=[UHCI").expect("UHCI entry missing");
    let xhci_pos = stdout.find("XHCI len=").expect("XHCI entry missing");
    assert!(
        uhci_pos < xhci_pos,
        "expected `UHCI` to appear before `XHCI` in stdout:\n{stdout}"
    );
}
