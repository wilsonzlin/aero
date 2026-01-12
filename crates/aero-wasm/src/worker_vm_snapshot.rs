#![cfg(target_arch = "wasm32")]

use std::io::Cursor;

use aero_opfs::OpfsSyncFile;
use aero_snapshot::{
    CpuState, DeviceId, DeviceState, DiskOverlayRefs, MmuState, RestoreOptions, SaveOptions,
    SnapshotError, SnapshotMeta, SnapshotSource, SnapshotTarget,
};
use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

const WASM_PAGE_BYTES: u64 = 64 * 1024;
const MAX_DEVICE_BLOB_BYTES: usize = 16 * 1024 * 1024;
const MAX_DEVICE_COUNT: usize = 4096;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

fn wasm_memory_len_bytes() -> u64 {
    let pages = core::arch::wasm32::memory_size(0) as u64;
    pages.saturating_mul(WASM_PAGE_BYTES)
}

#[wasm_bindgen]
pub struct WorkerVmSnapshot {
    guest_base: u32,
    guest_size: u32,
    cpu: CpuState,
    mmu: MmuState,
    cpu_mmu_set: bool,
    devices: Vec<DeviceState>,
    restored_mmu: bool,
}

#[wasm_bindgen]
impl WorkerVmSnapshot {
    #[wasm_bindgen(constructor)]
    pub fn new(guest_base: u32, guest_size: u32) -> Result<Self, JsValue> {
        if guest_size == 0 {
            return Err(js_error("guest_size must be non-zero"));
        }

        // Keep guest RAM below the PCI MMIO BAR window (see `guest_ram_layout` contract).
        let guest_size_u64 = u64::from(guest_size).min(crate::guest_layout::PCI_MMIO_BASE);
        let guest_size: u32 = guest_size_u64
            .try_into()
            .map_err(|_| js_error("guest_size does not fit in u32"))?;

        let end = u64::from(guest_base)
            .checked_add(u64::from(guest_size))
            .ok_or_else(|| js_error("guest_base + guest_size overflow"))?;
        let mem_bytes = wasm_memory_len_bytes();
        if end > mem_bytes {
            return Err(js_error(format!(
                "guest RAM region out of bounds: guest_base=0x{guest_base:x} guest_size=0x{guest_size:x} end=0x{end:x} wasm_memory=0x{mem_bytes:x}"
            )));
        }

        Ok(Self {
            guest_base,
            guest_size,
            cpu: CpuState::default(),
            mmu: MmuState::default(),
            cpu_mmu_set: false,
            devices: Vec::new(),
            restored_mmu: false,
        })
    }

    pub fn set_cpu_state_v2(&mut self, cpu_bytes: &[u8], mmu_bytes: &[u8]) -> Result<(), JsValue> {
        let cpu = {
            let mut r = Cursor::new(cpu_bytes);
            let cpu =
                CpuState::decode_v2(&mut r).map_err(|e| js_error(format!("cpu_bytes: {e}")))?;
            if r.position() != cpu_bytes.len() as u64 {
                return Err(js_error("cpu_bytes contains trailing bytes"));
            }
            cpu
        };

        let mmu = {
            let mut r = Cursor::new(mmu_bytes);
            let mmu =
                MmuState::decode_v2(&mut r).map_err(|e| js_error(format!("mmu_bytes: {e}")))?;
            if r.position() != mmu_bytes.len() as u64 {
                return Err(js_error("mmu_bytes contains trailing bytes"));
            }
            mmu
        };

        self.cpu = cpu;
        self.mmu = mmu;
        self.cpu_mmu_set = true;
        Ok(())
    }

    pub fn add_device_state(
        &mut self,
        id: u32,
        version: u16,
        flags: u16,
        data: &[u8],
    ) -> Result<(), JsValue> {
        if data.len() > MAX_DEVICE_BLOB_BYTES {
            return Err(js_error(format!(
                "device state too large: id={id} version={version} flags={flags} len={} max={MAX_DEVICE_BLOB_BYTES}",
                data.len()
            )));
        }
        if self.devices.len() >= MAX_DEVICE_COUNT {
            return Err(js_error(format!(
                "too many devices (max {MAX_DEVICE_COUNT})"
            )));
        }
        if self
            .devices
            .iter()
            .any(|dev| dev.id.0 == id && dev.version == version && dev.flags == flags)
        {
            return Err(js_error(format!(
                "duplicate device state entry: id={id} version={version} flags={flags}"
            )));
        }

        self.devices.push(DeviceState {
            id: DeviceId(id),
            version,
            flags,
            data: data.to_vec(),
        });
        Ok(())
    }

    pub fn snapshot_full(&mut self) -> Result<Vec<u8>, JsValue> {
        if !self.cpu_mmu_set {
            return Err(js_error(
                "CPU/MMU state not set; call set_cpu_state_v2() before snapshot_full()",
            ));
        }

        let mut w = Cursor::new(Vec::new());
        aero_snapshot::save_snapshot(&mut w, self, SaveOptions::default())
            .map_err(|e| js_error(format!("Failed to write aero-snapshot (full): {e}")))?;
        Ok(w.into_inner())
    }

    pub fn restore_snapshot(&mut self, bytes: &[u8]) -> Result<JsValue, JsValue> {
        self.devices.clear();
        self.restored_mmu = false;

        let mut r = Cursor::new(bytes);
        aero_snapshot::restore_snapshot_with_options(&mut r, self, RestoreOptions::default())
            .map_err(|e| js_error(format!("Failed to restore aero-snapshot: {e}")))?;

        self.build_restore_result()
    }

    pub async fn snapshot_full_to_opfs(&mut self, path: String) -> Result<(), JsValue> {
        if !self.cpu_mmu_set {
            return Err(js_error(
                "CPU/MMU state not set; call set_cpu_state_v2() before snapshot_full_to_opfs()",
            ));
        }

        let mut file = OpfsSyncFile::create(&path)
            .await
            .map_err(|e| js_error(format!("Failed to create OPFS file {path}: {e}")))?;

        aero_snapshot::save_snapshot(&mut file, self, SaveOptions::default())
            .map_err(|e| js_error(format!("Failed to write aero-snapshot to OPFS: {e}")))?;

        file.close()
            .map_err(|e| js_error(format!("Failed to close OPFS file {path}: {e}")))?;

        Ok(())
    }

    pub async fn restore_snapshot_from_opfs(&mut self, path: String) -> Result<JsValue, JsValue> {
        self.devices.clear();
        self.restored_mmu = false;

        let mut file = OpfsSyncFile::open(&path, false)
            .await
            .map_err(|e| js_error(format!("Failed to open OPFS file {path}: {e}")))?;

        aero_snapshot::restore_snapshot_with_options(&mut file, self, RestoreOptions::default())
            .map_err(|e| js_error(format!("Failed to restore aero-snapshot from OPFS: {e}")))?;

        file.close()
            .map_err(|e| js_error(format!("Failed to close OPFS file {path}: {e}")))?;

        self.build_restore_result()
    }
}

impl WorkerVmSnapshot {
    fn ram_range_checked(&self, offset: u64, len: usize) -> aero_snapshot::Result<u32> {
        let len_u64: u64 = len
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("ram length overflow"))?;

        let end = offset
            .checked_add(len_u64)
            .ok_or(SnapshotError::Corrupt("ram offset overflow"))?;
        if end > u64::from(self.guest_size) {
            return Err(SnapshotError::Corrupt("ram access out of bounds"));
        }

        let abs = u64::from(self.guest_base)
            .checked_add(offset)
            .ok_or(SnapshotError::Corrupt("ram base overflow"))?;
        let abs_end = abs
            .checked_add(len_u64)
            .ok_or(SnapshotError::Corrupt("ram absolute offset overflow"))?;

        let mem_len = wasm_memory_len_bytes();
        if abs_end > mem_len {
            return Err(SnapshotError::Corrupt("ram access exceeds linear memory"));
        }
        if abs > u64::from(u32::MAX) {
            return Err(SnapshotError::Corrupt("ram pointer overflow"));
        }

        Ok(abs as u32)
    }

    fn build_restore_result(&self) -> Result<JsValue, JsValue> {
        let mut cpu_bytes = Vec::new();
        self.cpu
            .encode_v2(&mut cpu_bytes)
            .map_err(|e| js_error(format!("Failed to encode restored CPU state: {e}")))?;

        let mut mmu_bytes = Vec::new();
        self.mmu
            .encode_v2(&mut mmu_bytes)
            .map_err(|e| js_error(format!("Failed to encode restored MMU state: {e}")))?;

        let devices_arr = Array::new();
        for device in &self.devices {
            let obj = Object::new();
            Reflect::set(&obj, &JsValue::from_str("id"), &JsValue::from(device.id.0))
                .map_err(|e| js_error(format!("Failed to set device.id: {e:?}")))?;
            Reflect::set(
                &obj,
                &JsValue::from_str("version"),
                &JsValue::from(device.version as u32),
            )
            .map_err(|e| js_error(format!("Failed to set device.version: {e:?}")))?;
            Reflect::set(
                &obj,
                &JsValue::from_str("flags"),
                &JsValue::from(device.flags as u32),
            )
            .map_err(|e| js_error(format!("Failed to set device.flags: {e:?}")))?;
            let data = Uint8Array::from(device.data.as_slice());
            Reflect::set(&obj, &JsValue::from_str("data"), data.as_ref())
                .map_err(|e| js_error(format!("Failed to set device.data: {e:?}")))?;
            devices_arr.push(obj.as_ref());
        }

        let root = Object::new();
        let cpu = Uint8Array::from(cpu_bytes.as_slice());
        Reflect::set(&root, &JsValue::from_str("cpu"), cpu.as_ref())
            .map_err(|e| js_error(format!("Failed to set cpu bytes: {e:?}")))?;
        let mmu = Uint8Array::from(mmu_bytes.as_slice());
        Reflect::set(&root, &JsValue::from_str("mmu"), mmu.as_ref())
            .map_err(|e| js_error(format!("Failed to set mmu bytes: {e:?}")))?;
        Reflect::set(&root, &JsValue::from_str("devices"), devices_arr.as_ref())
            .map_err(|e| js_error(format!("Failed to set devices array: {e:?}")))?;

        Ok(root.into())
    }
}

impl SnapshotSource for WorkerVmSnapshot {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        // Use a deterministic, zeroed meta block. The distributed worker runtime
        // will handle any higher-level snapshot identity/labeling externally.
        SnapshotMeta::default()
    }

    fn cpu_state(&self) -> CpuState {
        self.cpu.clone()
    }

    fn mmu_state(&self) -> MmuState {
        self.mmu.clone()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        // `aero-snapshot` sorts device entries again, but ensure we still provide a
        // canonical ordering here to avoid any nondeterminism when callers
        // construct device lists via unordered maps.
        let mut devices = self.devices.clone();
        devices.sort_by_key(|device| (device.id.0, device.version, device.flags));
        devices
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.guest_size as usize
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> aero_snapshot::Result<()> {
        let abs = self.ram_range_checked(offset, buf.len())?;
        // Safety: `ram_range_checked` bounds-checks the access against both
        // guest_size and the current wasm linear memory size.
        unsafe {
            core::ptr::copy(abs as *const u8, buf.as_mut_ptr(), buf.len());
        }
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

impl SnapshotTarget for WorkerVmSnapshot {
    fn restore_cpu_state(&mut self, state: CpuState) {
        self.cpu = state;
        self.cpu_mmu_set = true;
    }

    fn restore_mmu_state(&mut self, state: MmuState) {
        self.mmu = state;
        self.cpu_mmu_set = true;
        self.restored_mmu = true;
    }

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        self.devices = states;
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.guest_size as usize
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> aero_snapshot::Result<()> {
        let abs = self.ram_range_checked(offset, data.len())?;
        // Safety: `ram_range_checked` bounds-checks the access against both
        // guest_size and the current wasm linear memory size.
        unsafe {
            core::ptr::copy(data.as_ptr(), abs as *mut u8, data.len());
        }
        Ok(())
    }

    fn post_restore(&mut self) -> aero_snapshot::Result<()> {
        if !self.restored_mmu {
            return Err(SnapshotError::Corrupt("missing MMU section"));
        }

        if self
            .devices
            .iter()
            .any(|device| device.data.len() > MAX_DEVICE_BLOB_BYTES)
        {
            return Err(SnapshotError::Corrupt("device entry too large"));
        }

        Ok(())
    }
}
