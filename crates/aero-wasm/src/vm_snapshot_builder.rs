//! WASM-side VM snapshot builder/restorer.
//!
//! The web runtime runs the emulator CPU loop in one worker and OPFS snapshot I/O in another.
//! Both workers share a single `WebAssembly.Memory` (shared linear memory), so snapshot save/restore
//! on the I/O worker must read/write guest RAM directly from that shared memory without requiring a
//! `WasmVm` instance.
//!
//! This module exports the functions the web I/O worker probes for dynamically:
//! - `vm_snapshot_save_to_opfs(path, cpu, mmu, devices)`
//! - `vm_snapshot_restore_from_opfs(path)`
//!
//! Node-based wasm-bindgen tests do not provide OPFS, so we also export in-memory helpers:
//! - `vm_snapshot_save(cpu, mmu, devices) -> Uint8Array`
//! - `vm_snapshot_restore(bytes) -> { cpu, mmu, devices? }`
#![cfg(target_arch = "wasm32")]

use std::io::{Cursor, Read, Seek, Write};

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

use js_sys::{Array, Object, Reflect, Uint8Array};

use aero_snapshot::{CpuState, DeviceId, DeviceState, DiskOverlayRefs, MmuState, SnapshotMeta};

const MAX_STATE_BLOB_BYTES: usize = 4 * 1024 * 1024;
const MAX_DEVICE_BLOB_BYTES: usize = 4 * 1024 * 1024;

const DEVICE_KIND_USB_UHCI: &str = "usb.uhci";
const DEVICE_KIND_I8042: &str = "input.i8042";
const DEVICE_KIND_AUDIO_HDA: &str = "audio.hda";
const DEVICE_KIND_PREFIX_ID: &str = "device.";

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn wasm_memory_byte_len() -> u64 {
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(64 * 1024)
}

fn guest_region_from_layout_contract() -> Result<(u32, usize), JsValue> {
    let guest_base = crate::guest_layout::align_up(
        crate::guest_layout::RUNTIME_RESERVED_BYTES,
        crate::guest_layout::WASM_PAGE_BYTES,
    );
    let mem_bytes = wasm_memory_byte_len();

    if guest_base > mem_bytes {
        return Err(js_error(format!(
            "Guest RAM base exceeds wasm linear memory: guest_base=0x{guest_base:x} wasm_mem=0x{mem_bytes:x}"
        )));
    }
    let guest_size_u64 = mem_bytes.saturating_sub(guest_base);
    if guest_size_u64 == 0 {
        return Err(js_error(format!(
            "Guest RAM region is empty: guest_base=0x{guest_base:x} wasm_mem=0x{mem_bytes:x}"
        )));
    }

    let guest_base_u32: u32 = guest_base
        .try_into()
        .map_err(|_| js_error("guest_base does not fit in u32"))?;
    let guest_size: usize = guest_size_u64
        .try_into()
        .map_err(|_| js_error("guest_size does not fit in usize"))?;
    Ok((guest_base_u32, guest_size))
}

fn decode_cpu_state(bytes: &[u8]) -> Result<CpuState, JsValue> {
    if bytes.len() > MAX_STATE_BLOB_BYTES {
        return Err(js_error(format!(
            "CPU state blob too large: {} bytes (max {MAX_STATE_BLOB_BYTES})",
            bytes.len()
        )));
    }
    CpuState::decode_v2(&mut Cursor::new(bytes))
        .map_err(|e| js_error(format!("Failed to decode CPU state v2: {e}")))
}

fn decode_mmu_state(bytes: &[u8]) -> Result<MmuState, JsValue> {
    if bytes.len() > MAX_STATE_BLOB_BYTES {
        return Err(js_error(format!(
            "MMU state blob too large: {} bytes (max {MAX_STATE_BLOB_BYTES})",
            bytes.len()
        )));
    }
    MmuState::decode_v2(&mut Cursor::new(bytes))
        .map_err(|e| js_error(format!("Failed to decode MMU state v2: {e}")))
}

fn device_version_flags_from_aero_io_snapshot(bytes: &[u8]) -> (u16, u16) {
    // Preferred path: `aero-io-snapshot` format uses a 16-byte header (see `aero-io-snapshot`):
    // - magic: b"AERO" (4 bytes)
    // - format_version: u16 major, u16 minor
    // - device_id: [u8; 4]
    // - device_version: u16 major, u16 minor
    //
    // We store the device version pair in the VM snapshot's `DeviceState` version/flags fields so
    // that future VM-level logic can interpret the contained device blob without re-parsing it.
    //
    // Legacy path: some JS-only device snapshots also start with "AERO" but use a shorter header:
    // - magic: b"AERO" (4 bytes)
    // - version: u16
    // - flags: u16
    //
    // Detect the io-snapshot header by checking that the 4-byte device id region looks like an
    // ASCII tag.
    const IO_HEADER_LEN: usize = 16;
    if bytes.len() < 4 || &bytes[0..4] != b"AERO" {
        return (1, 0);
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
            return (major, minor);
        }
    }

    if bytes.len() >= 8 {
        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        let flags = u16::from_le_bytes([bytes[6], bytes[7]]);
        return (version, flags);
    }

    (1, 0)
}

fn parse_device_kind(kind: &str) -> Option<DeviceId> {
    if kind == DEVICE_KIND_USB_UHCI {
        return Some(DeviceId::USB);
    }
    if kind == DEVICE_KIND_I8042 {
        return Some(DeviceId::I8042);
    }
    if kind == DEVICE_KIND_AUDIO_HDA {
        return Some(DeviceId::HDA);
    }

    // For forward compatibility, unknown device ids can be surfaced as `device.<id>` strings.
    // When a snapshot is re-saved, accept this spelling and preserve the numeric device id.
    if let Some(rest) = kind.strip_prefix(DEVICE_KIND_PREFIX_ID) {
        if let Ok(id) = rest.parse::<u32>() {
            return Some(DeviceId(id));
        }
    }

    None
}

fn kind_from_device_id(id: DeviceId) -> String {
    if id == DeviceId::USB {
        return DEVICE_KIND_USB_UHCI.to_string();
    }
    if id == DeviceId::I8042 {
        return DEVICE_KIND_I8042.to_string();
    }
    if id == DeviceId::HDA {
        return DEVICE_KIND_AUDIO_HDA.to_string();
    }

    // For unknown device ids, preserve them as `device.<id>` entries so callers can roundtrip them
    // back through `vm_snapshot_save(_to_opfs)` without losing state.
    format!("{DEVICE_KIND_PREFIX_ID}{}", id.0)
}

fn parse_devices_js(devices: JsValue) -> Result<Vec<DeviceState>, JsValue> {
    if devices.is_null() || devices.is_undefined() {
        return Ok(Vec::new());
    }

    let arr: Array = devices
        .dyn_into()
        .map_err(|_| js_error("devices must be an array"))?;

    let kind_key = JsValue::from_str("kind");
    let bytes_key = JsValue::from_str("bytes");

    let mut out = Vec::with_capacity(arr.length() as usize);
    for (idx, entry) in arr.iter().enumerate() {
        let obj: Object = entry
            .dyn_into()
            .map_err(|_| js_error(format!("devices[{idx}] must be an object")))?;

        let kind_val = Reflect::get(&obj, &kind_key)
            .map_err(|_| js_error(format!("devices[{idx}].kind missing")))?;
        let kind = kind_val.as_string().ok_or_else(|| {
            js_error(format!(
                "devices[{idx}].kind must be a string (got {kind_val:?})"
            ))
        })?;

        let bytes_val = Reflect::get(&obj, &bytes_key)
            .map_err(|_| js_error(format!("devices[{idx}].bytes missing")))?;
        let bytes_arr: Uint8Array = bytes_val
            .dyn_into()
            .map_err(|_| js_error(format!("devices[{idx}].bytes must be a Uint8Array")))?;

        let len = bytes_arr.length() as usize;
        if len > MAX_DEVICE_BLOB_BYTES {
            return Err(js_error(format!(
                "Device blob too large for kind '{kind}': {len} bytes (max {MAX_DEVICE_BLOB_BYTES})"
            )));
        }
        let data = bytes_arr.to_vec();

        let id = parse_device_kind(&kind)
            .ok_or_else(|| js_error(format!("Unknown device kind '{kind}'")))?;
        let (version, flags) = device_version_flags_from_aero_io_snapshot(&data);

        out.push(DeviceState {
            id,
            version,
            flags,
            data,
        });
    }

    Ok(out)
}

fn build_devices_js(states: Vec<DeviceState>) -> Result<Option<Array>, JsValue> {
    if states.is_empty() {
        return Ok(None);
    }

    let out = Array::new();
    let kind_key = JsValue::from_str("kind");
    let bytes_key = JsValue::from_str("bytes");

    for state in states {
        let obj = Object::new();
        let kind = kind_from_device_id(state.id);
        Reflect::set(&obj, &kind_key, &JsValue::from_str(&kind))
            .map_err(|_| js_error("Failed to build device state object (kind)"))?;
        let bytes = Uint8Array::from(state.data.as_slice());
        Reflect::set(&obj, &bytes_key, bytes.as_ref())
            .map_err(|_| js_error("Failed to build device state object (bytes)"))?;
        out.push(&obj);
    }

    Ok(Some(out))
}

struct WasmSnapshotSource {
    guest_base: u32,
    guest_size: usize,
    cpu: CpuState,
    mmu: MmuState,
    devices: Vec<DeviceState>,
}

impl aero_snapshot::SnapshotSource for WasmSnapshotSource {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        // Deterministic meta so repeated saves of identical inputs produce stable bytes.
        SnapshotMeta {
            snapshot_id: 0,
            parent_snapshot_id: None,
            created_unix_ms: 0,
            label: None,
        }
    }

    fn cpu_state(&self) -> CpuState {
        self.cpu.clone()
    }

    fn mmu_state(&self) -> MmuState {
        self.mmu.clone()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        self.devices.clone()
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.guest_size
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        use aero_snapshot::SnapshotError;

        let offset: usize = offset
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(SnapshotError::Corrupt("ram read overflow"))?;
        if end > self.guest_size {
            return Err(SnapshotError::Corrupt("ram read out of bounds"));
        }

        let base = u64::from(self.guest_base);
        let ptr_u64 = base
            .checked_add(offset as u64)
            .ok_or(SnapshotError::Corrupt("ram base overflow"))?;

        // Safety: we bounds-check against `guest_size` computed from the wasm linear memory size.
        unsafe {
            core::ptr::copy_nonoverlapping(ptr_u64 as *const u8, buf.as_mut_ptr(), buf.len());
        }
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

struct WasmSnapshotTarget {
    guest_base: u32,
    guest_size: usize,
    cpu: Option<CpuState>,
    mmu: Option<MmuState>,
    devices: Vec<DeviceState>,
}

impl WasmSnapshotTarget {
    fn new(guest_base: u32, guest_size: usize) -> Self {
        Self {
            guest_base,
            guest_size,
            cpu: None,
            mmu: None,
            devices: Vec::new(),
        }
    }
}

impl aero_snapshot::SnapshotTarget for WasmSnapshotTarget {
    fn restore_cpu_state(&mut self, state: CpuState) {
        self.cpu = Some(state);
    }

    fn restore_mmu_state(&mut self, state: MmuState) {
        self.mmu = Some(state);
    }

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        self.devices = states;
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.guest_size
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> aero_snapshot::Result<()> {
        use aero_snapshot::SnapshotError;

        let offset: usize = offset
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(data.len())
            .ok_or(SnapshotError::Corrupt("ram write overflow"))?;
        if end > self.guest_size {
            return Err(SnapshotError::Corrupt("ram write out of bounds"));
        }

        let base = u64::from(self.guest_base);
        let ptr_u64 = base
            .checked_add(offset as u64)
            .ok_or(SnapshotError::Corrupt("ram base overflow"))?;

        // Safety: bounds-checked above.
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), ptr_u64 as *mut u8, data.len());
        }
        Ok(())
    }
}

fn snapshot_save_to<W: Write + Seek>(
    mut w: W,
    cpu: Uint8Array,
    mmu: Uint8Array,
    devices: JsValue,
) -> Result<(), JsValue> {
    let (guest_base, guest_size) = guest_region_from_layout_contract()?;

    let cpu_bytes = cpu.to_vec();
    let mmu_bytes = mmu.to_vec();

    let cpu = decode_cpu_state(&cpu_bytes)?;
    let mmu = decode_mmu_state(&mmu_bytes)?;
    let devices = parse_devices_js(devices)?;

    let mut source = WasmSnapshotSource {
        guest_base,
        guest_size,
        cpu,
        mmu,
        devices,
    };

    aero_snapshot::save_snapshot(&mut w, &mut source, aero_snapshot::SaveOptions::default())
        .map_err(|e| js_error(format!("Failed to save snapshot: {e}")))?;
    Ok(())
}

fn snapshot_restore_from<R: Read>(
    mut r: R,
) -> Result<(CpuState, MmuState, Vec<DeviceState>), JsValue> {
    let (guest_base, guest_size) = guest_region_from_layout_contract()?;
    let mut target = WasmSnapshotTarget::new(guest_base, guest_size);

    aero_snapshot::restore_snapshot_checked(
        &mut r,
        &mut target,
        aero_snapshot::RestoreOptions::default(),
    )
    .map_err(|e| js_error(format!("Failed to restore snapshot: {e}")))?;

    let cpu = target
        .cpu
        .ok_or_else(|| js_error("snapshot missing CPU state"))?;
    let mmu = target
        .mmu
        .ok_or_else(|| js_error("snapshot missing MMU state"))?;
    Ok((cpu, mmu, target.devices))
}

fn build_restore_result(
    cpu: CpuState,
    mmu: MmuState,
    devices: Vec<DeviceState>,
) -> Result<JsValue, JsValue> {
    let mut cpu_bytes = Vec::new();
    cpu.encode_v2(&mut cpu_bytes)
        .map_err(|e| js_error(format!("Failed to encode CPU state v2: {e}")))?;
    let mut mmu_bytes = Vec::new();
    mmu.encode_v2(&mut mmu_bytes)
        .map_err(|e| js_error(format!("Failed to encode MMU state v2: {e}")))?;

    let cpu_js = Uint8Array::from(cpu_bytes.as_slice());
    let mmu_js = Uint8Array::from(mmu_bytes.as_slice());

    let obj = Object::new();
    Reflect::set(&obj, &JsValue::from_str("cpu"), cpu_js.as_ref())
        .map_err(|_| js_error("Failed to build restore object (cpu)"))?;
    Reflect::set(&obj, &JsValue::from_str("mmu"), mmu_js.as_ref())
        .map_err(|_| js_error("Failed to build restore object (mmu)"))?;

    if let Some(devices_js) = build_devices_js(devices)? {
        Reflect::set(&obj, &JsValue::from_str("devices"), devices_js.as_ref())
            .map_err(|_| js_error("Failed to build restore object (devices)"))?;
    }

    Ok(obj.into())
}

// -------------------------------------------------------------------------------------------------
// WASM exports: OPFS-backed snapshots
// -------------------------------------------------------------------------------------------------

#[wasm_bindgen]
pub async fn vm_snapshot_save_to_opfs(
    path: String,
    cpu: Uint8Array,
    mmu: Uint8Array,
    devices: JsValue,
) -> Result<(), JsValue> {
    let mut file = aero_opfs::OpfsSyncFile::create(&path)
        .await
        .map_err(|e| js_error(format!("Failed to create OPFS snapshot file '{path}': {e}")))?;

    snapshot_save_to(&mut file, cpu, mmu, devices)?;

    file.close()
        .map_err(|e| js_error(format!("Failed to close OPFS snapshot file '{path}': {e}")))?;
    Ok(())
}

#[wasm_bindgen]
pub async fn vm_snapshot_restore_from_opfs(path: String) -> Result<JsValue, JsValue> {
    let mut file = aero_opfs::OpfsSyncFile::open(&path, false)
        .await
        .map_err(|e| js_error(format!("Failed to open OPFS snapshot file '{path}': {e}")))?;

    let (cpu, mmu, devices) = snapshot_restore_from(&mut file)?;

    file.close()
        .map_err(|e| js_error(format!("Failed to close OPFS snapshot file '{path}': {e}")))?;

    build_restore_result(cpu, mmu, devices)
}

// -------------------------------------------------------------------------------------------------
// WASM exports: in-memory snapshots (for wasm-pack tests / non-OPFS environments)
// -------------------------------------------------------------------------------------------------

#[wasm_bindgen]
pub fn vm_snapshot_save(
    cpu: Uint8Array,
    mmu: Uint8Array,
    devices: JsValue,
) -> Result<Uint8Array, JsValue> {
    let mut cursor = Cursor::new(Vec::new());
    snapshot_save_to(&mut cursor, cpu, mmu, devices)?;
    Ok(Uint8Array::from(cursor.get_ref().as_slice()))
}

#[wasm_bindgen]
pub fn vm_snapshot_restore(bytes: Uint8Array) -> Result<JsValue, JsValue> {
    let data = bytes.to_vec();
    let (cpu, mmu, devices) = snapshot_restore_from(Cursor::new(data))?;
    build_restore_result(cpu, mmu, devices)
}
