# Virtio Input (virtio 1.1): Keyboard + Mouse

## Why virtio-input

PS/2 is simple but has limited throughput and higher per-event overhead. Once the guest has a virtio driver installed, **virtio-input** provides a fast paravirtual path for keyboard/mouse events with low latency and fewer emulated side effects.

See also:

- [`virtio/virtqueue-split-ring-win7.md`](./virtio/virtqueue-split-ring-win7.md) — split-ring virtqueue implementation guide for Windows 7 KMDF drivers (descriptor mgmt, ordering/barriers, EVENT_IDX, indirect).
- [`windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.
- [`virtio-input-test-plan.md`](./virtio-input-test-plan.md) — end-to-end validation plan (Rust device-model tests, Win7 driver tests, web runtime routing).

This repo implements virtio-input as a **single multi-function PCI device** (AERO-W7-VIRTIO contract v1):

- Function 0: `Aero Virtio Keyboard` (`SUBSYS 0x0010`, `header_type = 0x80`)
- Function 1: `Aero Virtio Mouse` (relative pointer, `SUBSYS 0x0011`)

Each function is a standard virtio 1.1 device (`VIRTIO_ID_INPUT`) with its own virtqueues.

When installed with the in-tree Windows 7 driver (`drivers/windows7/virtio-input/inf/aero_virtio_input.inf`), these two PCI functions appear as **separate named devices** in Windows Device Manager (HIDClass):

- **Aero VirtIO Keyboard**
- **Aero VirtIO Mouse**

---

## Device model overview

Virtio-input uses two virtqueues:

| Queue | Direction | Purpose |
|------:|-----------|---------|
| eventq | device → driver | Input events (`virtio_input_event`) |
| statusq | driver → device | Output events (e.g. LED state) |

Events use the Linux input ABI layout:

```text
struct virtio_input_event {
  le16 type;   // EV_KEY / EV_REL / EV_SYN / ...
  le16 code;   // KEY_* / BTN_* / REL_* / SYN_REPORT / ...
  le32 value;  // 1/0 for keys, deltas for REL_*, 0 for SYN_REPORT
};
```

The implementation emits a `EV_SYN / SYN_REPORT` event after each logical batch, matching the conventional input event stream format.

---

## Code map (where things live)

Virtio-input spans both the Rust device model and the browser runtime wiring. In the browser runtime, the integration path depends on
`vmRuntime`:

- **Virtio-input device model (Rust):** `crates/aero-virtio/src/devices/input.rs`
- **Browser runtime wiring (`vmRuntime="legacy"`):**
  - TypeScript PCI wrapper + event injection: `web/src/io/devices/virtio_input.ts`
  - PCI registration helper (canonical BDF for the keyboard function): `web/src/workers/io_virtio_input_register.ts`
  - IO worker integration + routing decisions: `web/src/workers/io.worker.ts`
- **Canonical full-system machine wiring (used by `vmRuntime="machine"`):**
  - The canonical `aero_machine::Machine` supports virtio-input behind an explicit config flag:
    - `MachineConfig.enable_virtio_input = true` (requires `MachineConfig.enable_pc_platform = true`)
    - Fixed PCI BDFs:
      - `00:0A.0` — virtio-input keyboard (`aero_devices::pci::profile::VIRTIO_INPUT_KEYBOARD`)
      - `00:0A.1` — virtio-input mouse (`aero_devices::pci::profile::VIRTIO_INPUT_MOUSE`)
    - Interrupts: both **INTx** (baseline) and **MSI-X** are supported.
      - MSI(-X) delivery is wired through `VirtioMsixInterruptSink` → `PlatformInterrupts::trigger_msi`
        (see `crates/aero-machine/src/lib.rs::VirtioMsixInterruptSink`).
      - Legacy INTx delivery is polled/routed via the machine’s PCI INTx sync loop (not via the virtio
        interrupt sink), so it remains deterministic across devices.
      - Regression test: `crates/aero-machine/tests/virtio_input_msix.rs`.
  - In the JS/WASM “single machine” API (`crates/aero-wasm::Machine`), virtio-input is opt-in at construction time:
    - `Machine.new_with_options(..., { enable_virtio_input: true })`
    - Injection APIs: `inject_virtio_key/rel/button/wheel` (Linux `evdev`-style codes)
    - Driver status probes: `virtio_input_keyboard_driver_ok()` / `virtio_input_mouse_driver_ok()`
  - Browser runtime integration lives in the machine CPU worker: `web/src/workers/machine_cpu.worker.ts`.

---

## Config space queries (required by the virtio-input driver)

Virtio-input uses a small device-specific config region where the driver:

1. Writes `{select, subsel}`
2. Reads `size`
3. Reads `u.*` payload bytes

The implementation supports at least:

- `VIRTIO_INPUT_CFG_ID_NAME` (device name string)
- `VIRTIO_INPUT_CFG_ID_SERIAL` (string, currently `"0"`)
- `VIRTIO_INPUT_CFG_ID_DEVIDS` (`bustype/vendor/product/version`)
- `VIRTIO_INPUT_CFG_EV_BITS`:
  - `subsel = 0` → event type bitmap (`EV_SYN`, `EV_KEY`, `EV_REL`, `EV_LED`)
  - `subsel = EV_KEY` → supported key/button bitmap
  - `subsel = EV_REL` → supported rel bitmap (`REL_X`, `REL_Y`, `REL_WHEEL`, `REL_HWHEEL`)
  - `subsel = EV_LED` → supported LED bitmap (`LED_*`, keyboard only)

---

## Host/browser input integration

The capture layer (IN-CAPTURE) should be able to inject the same high-level input events into either:

- PS/2 (boot + early install)
- virtio-input (once guest driver is active)

Runtime routing is typically:

- **Auto mode**: PS/2 until the guest sets `DRIVER_OK` for the virtio-input device, then switch to virtio-input.
- Optional developer modes: PS/2 only, virtio only.

---

## Windows 7 driver (minimal test-signed approach)

Windows 7 has no in-box virtio-input driver. A minimal approach is to ship a custom, test-signed driver that:

1. Binds to the virtio-input PCI function (Aero Win7 contract v1 uses Vendor/Device `PCI\VEN_1AF4&DEV_1052` with PCI Revision ID `0x01` / `REV_01`).
2. Negotiates virtio features and sets `DRIVER_OK`.
3. Creates a HID keyboard + HID mouse interface for Windows by translating `virtio_input_event` streams into HID reports.
4. Optionally forwards LED state changes (Caps Lock / Num Lock / Scroll Lock) from Windows to `statusq`.
   - The contract requires that **all** `statusq` descriptors are consumed/completed (contents may be ignored).
   - The in-tree Win7 guest selftest can validate this end-to-end via `--test-input-led` (marker: `virtio-input-led`), and the
     host harness can enforce it with PowerShell `-WithInputLed` / Python `--with-input-led`.

Contract note:

- `AERO-W7-VIRTIO` v1 encodes the contract major version in the PCI Revision ID (`REV_01`).
- The in-tree Win7 virtio-input INFs are intentionally **revision-gated** (match only `...&REV_01` HWIDs), so QEMU-style
  `REV_00` virtio-input devices will not bind unless you override the revision (for example `x-pci-revision=0x01`).
  - The canonical keyboard/mouse INF (`drivers/windows7/virtio-input/inf/aero_virtio_input.inf`) includes:
    - subsystem-qualified keyboard/mouse HWIDs (`SUBSYS_0010` / `SUBSYS_0011`) for distinct Device Manager names
      (**Aero VirtIO Keyboard** / **Aero VirtIO Mouse**), and
    - a strict revision-gated generic fallback HWID (no `SUBSYS`): `PCI\VEN_1AF4&DEV_1052&REV_01`
      (Device Manager name: **Aero VirtIO Input Device**).
  - The repo also carries an optional legacy filename alias INF
    (`drivers/windows7/virtio-input/inf/virtio-input.inf.disabled`; rename to `virtio-input.inf` to enable) for basename
    compatibility with older tooling/workflows.
    - Alias sync policy: from the first section header (`[Version]`) onward, it is expected to be byte-for-byte identical to
      `aero_virtio_input.inf` (only the leading banner/comments may differ; see
      `drivers/windows7/virtio-input/scripts/check-inf-alias.py`; CI enforces this via
      `scripts/ci/check-windows7-virtio-contract-consistency.py`).
    - Because it is identical, enabling the alias does **not** change HWID matching behavior. Enabling the alias is **not**
      required for generic fallback binding.
  - Tablet devices bind via the separate tablet INF (`drivers/windows7/virtio-input/inf/aero_virtio_tablet.inf`,
    `SUBSYS_00121AF4`). That HWID is more specific, so it wins over the generic fallback when both driver packages are installed.
    If the tablet INF is not installed, the generic fallback entry can also bind to tablet devices (but will use the generic
    device name).
  - Do not ship/install both `aero_virtio_input.inf` and `virtio-input.inf` at the same time (duplicate overlapping INFs can
    lead to confusing PnP driver selection). Ship/install **only one** of the two filenames at a time.
- The driver also validates the Revision ID at runtime.

### Installation flow (test signing)

1. **Enable test signing** (guest):
   - Run: `bcdedit /set testsigning on`
   - Reboot the VM
2. **Install the test certificate** used to sign the driver:
    - Import into **Trusted Root Certification Authorities**
    - Import into **Trusted Publishers**
    - For the in-tree Win7 driver, see `drivers/windows7/virtio-input/README.md` for helper scripts (`make-cert.ps1` / `install-test-cert.ps1`) and the full signing workflow.
3. **Install the driver**:
    - Device Manager → the virtio-input PCI device (often appears as “Unknown device”)
    - “Update driver” → “Have Disk…” → point at the driver `.inf`
4. **Verify**:
   - A new HID keyboard and HID mouse appear
   - The emulator can detect `DRIVER_OK` and switch input routing to virtio-input

### In-tree driver source (this repo)

The canonical Windows 7 virtio-input driver source lives at:

- `drivers/windows7/virtio-input/` (INF: `inf/aero_virtio_input.inf`, service: `aero_virtio_input`)

The repo also carries an optional legacy alias INF (`inf/virtio-input.inf.disabled`; rename to `virtio-input.inf` to enable)
for compatibility with older workflows/tools that still reference `virtio-input.inf`. It is a filename alias only.

Policy:

- `inf/aero_virtio_input.inf` includes:
  - subsystem-qualified keyboard/mouse model lines (`SUBSYS_0010` / `SUBSYS_0011`) for distinct Device Manager naming, and
  - the strict revision-gated generic fallback model line (no `SUBSYS`): `PCI\VEN_1AF4&DEV_1052&REV_01`.
- The legacy alias INF is expected to remain byte-for-byte identical to `inf/aero_virtio_input.inf` from `[Version]` onward
  (only the leading banner/comments may differ; see `drivers/windows7/virtio-input/scripts/check-inf-alias.py`).
- Because it is identical, enabling the alias does **not** change HWID matching behavior.
- Do not ship/install both filenames at the same time: ship/install **only one** of the two INF basenames to avoid duplicate,
  overlapping bindings.

### Notes

- If you want the absolute smallest driver surface area for Windows 7, a KMDF driver that exposes a HID interface is typically the pragmatic choice.
- The status queue is optional for basic input, but supporting LED updates is useful for parity with PS/2 keyboard behavior.
