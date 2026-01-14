//! WASM-side wrapper for [`aero_machine::PcMachine`].
//!
//! This exposes a full PCI-capable PC platform (including the E1000 NIC model) through
//! wasm-bindgen so the browser runtime can attach the NET_TX / NET_RX AIPC rings directly.
#![cfg(target_arch = "wasm32")]
#![forbid(unsafe_code)]

use wasm_bindgen::prelude::*;

use aero_ipc::wasm::{SharedRingBuffer, open_ring_by_kind};
use js_sys::{BigInt, Object, Reflect, SharedArrayBuffer};

use crate::RunExit;

fn js_error(message: impl core::fmt::Display) -> JsValue {
    js_sys::Error::new(&message.to_string()).into()
}

#[wasm_bindgen]
pub struct PcMachine {
    inner: aero_machine::PcMachine,
}

#[wasm_bindgen]
impl PcMachine {
    /// Create a new PC machine with a PCI E1000 NIC.
    ///
    /// Note: this is currently intended primarily for experiments/tests; it allocates its own guest
    /// RAM inside the wasm module rather than using the `guest_ram_layout` shared-memory contract.
    ///
    /// # vCPU count / SMP
    /// This constructor always uses `cpu_count=1`. `aero_machine::PcMachine` still executes only
    /// the bootstrap processor (BSP), so multi-vCPU guests are not expected to run robustly yet and
    /// the CPU count is not configurable via this JS API.
    ///
    /// See `docs/21-smp.md#status-today` and `docs/09-bios-firmware.md#smp-boot-bsp--aps`.
    #[wasm_bindgen(constructor)]
    pub fn new(ram_size_bytes: u32, smbios_uuid_seed: Option<u64>) -> Result<Self, JsValue> {
        // The BIOS expects to use the EBDA at 0x9F000, so enforce a minimum RAM size.
        let ram_size_bytes = (ram_size_bytes as u64).max(2 * 1024 * 1024);

        let cfg = aero_machine::PcMachineConfig {
            ram_size_bytes,
            cpu_count: 1,
            smbios_uuid_seed: smbios_uuid_seed.unwrap_or(0),
            enable_hda: false,
            enable_e1000: true,
            enable_xhci: false,
        };

        let inner = aero_machine::PcMachine::new_with_config(cfg)
            .map_err(|e| js_error(format!("PcMachine init failed: {e}")))?;
        Ok(Self { inner })
    }

    pub fn reset(&mut self) {
        self.inner.reset();
    }

    pub fn set_disk_image(&mut self, bytes: &[u8]) -> Result<(), JsValue> {
        self.inner
            .set_disk_image(bytes.to_vec())
            .map_err(|e| js_error(format!("set_disk_image failed: {e}")))
    }

    /// Attach the browser NET_TX / NET_RX rings to the machine's network backend.
    ///
    /// This consumes the provided ring views; callers can construct additional `SharedRingBuffer`
    /// views for their own use if needed.
    pub fn attach_l2_tunnel_rings(&mut self, tx: SharedRingBuffer, rx: SharedRingBuffer) {
        self.inner.attach_l2_tunnel_rings(tx, rx);
    }

    /// Convenience: open `NET_TX`/`NET_RX` rings from an `ioIpcSab` and attach them as an L2 tunnel.
    pub fn attach_l2_tunnel_from_io_ipc_sab(
        &mut self,
        io_ipc: SharedArrayBuffer,
    ) -> Result<(), JsValue> {
        let tx = open_ring_by_kind(
            io_ipc.clone(),
            aero_ipc::layout::io_ipc_queue_kind::NET_TX,
            0,
        )?;
        let rx = open_ring_by_kind(io_ipc, aero_ipc::layout::io_ipc_queue_kind::NET_RX, 0)?;
        self.attach_l2_tunnel_rings(tx, rx);
        Ok(())
    }

    /// Legacy/compatibility alias for [`PcMachine::attach_l2_tunnel_rings`].
    ///
    /// Some older JS runtimes refer to these rings as "NET rings" rather than "L2 tunnel rings".
    /// Prefer [`PcMachine::attach_l2_tunnel_rings`] for new code.
    pub fn attach_net_rings(&mut self, net_tx: SharedRingBuffer, net_rx: SharedRingBuffer) {
        self.attach_l2_tunnel_rings(net_tx, net_rx);
    }

    pub fn detach_network(&mut self) {
        self.inner.detach_network();
    }

    /// Legacy/compatibility alias for [`PcMachine::detach_network`].
    ///
    /// Prefer [`PcMachine::detach_network`] for new code.
    pub fn detach_net_rings(&mut self) {
        self.detach_network();
    }

    /// Poll the E1000 + ring backend bridge once (without running the CPU).
    pub fn poll_network(&mut self) {
        self.inner.poll_network();
    }

    /// Run the machine for up to `max_insts` guest instructions.
    pub fn run_slice(&mut self, max_insts: u32) -> RunExit {
        RunExit::from_native(self.inner.run_slice(max_insts as u64))
    }

    /// Returns and clears any accumulated BIOS "TTY output".
    ///
    /// This is a best-effort early-boot debug log recorded by the HLE BIOS, currently capturing:
    /// - INT 10h teletype output (AH=0Eh)
    /// - BIOS boot panic strings (e.g. missing/invalid boot sector)
    pub fn bios_tty_output(&mut self) -> Vec<u8> {
        let out = self.inner.bios_tty_output_bytes();
        self.inner.clear_bios_tty_output();
        out
    }

    /// Return the current BIOS TTY output length without copying the bytes into JS.
    pub fn bios_tty_output_len(&self) -> u32 {
        self.inner
            .bios_tty_output()
            .len()
            .min(u32::MAX as usize) as u32
    }

    /// Clear the BIOS TTY output buffer without reading it.
    pub fn clear_bios_tty_output(&mut self) {
        self.inner.clear_bios_tty_output();
    }

    /// Return best-effort stats for the attached `NET_TX`/`NET_RX` ring backend (or `null`).
    ///
    /// Values are exposed as JS `BigInt` so callers do not lose precision for long-running VMs.
    pub fn net_stats(&self) -> JsValue {
        let Some(stats) = self.inner.network_backend_l2_ring_stats() else {
            return JsValue::NULL;
        };

        let obj = Object::new();
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("tx_pushed_frames"),
            &BigInt::from(stats.tx_pushed_frames).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("tx_pushed_bytes"),
            &BigInt::from(stats.tx_pushed_bytes).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("tx_dropped_oversize"),
            &BigInt::from(stats.tx_dropped_oversize).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("tx_dropped_oversize_bytes"),
            &BigInt::from(stats.tx_dropped_oversize_bytes).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("tx_dropped_full"),
            &BigInt::from(stats.tx_dropped_full).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("tx_dropped_full_bytes"),
            &BigInt::from(stats.tx_dropped_full_bytes).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rx_popped_frames"),
            &BigInt::from(stats.rx_popped_frames).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rx_popped_bytes"),
            &BigInt::from(stats.rx_popped_bytes).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rx_dropped_oversize"),
            &BigInt::from(stats.rx_dropped_oversize).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rx_dropped_oversize_bytes"),
            &BigInt::from(stats.rx_dropped_oversize_bytes).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rx_corrupt"),
            &BigInt::from(stats.rx_corrupt).into(),
        );
        let _ = Reflect::set(
            &obj,
            &JsValue::from_str("rx_broken"),
            &JsValue::from_bool(stats.rx_broken),
        );

        obj.into()
    }
}
