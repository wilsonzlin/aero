# Virtio Input (virtio 1.1): Keyboard + Mouse

## Why virtio-input

PS/2 is simple but has limited throughput and higher per-event overhead. Once the guest has a virtio driver installed, **virtio-input** provides a fast paravirtual path for keyboard/mouse events with low latency and fewer emulated side effects.

See also:

- [`virtio/virtqueue-split-ring-win7.md`](./virtio/virtqueue-split-ring-win7.md) — split-ring virtqueue implementation guide for Windows 7 KMDF drivers (descriptor mgmt, ordering/barriers, EVENT_IDX, indirect).
- [`windows7-virtio-driver-contract.md`](./windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.

This repo implements **two virtio-input devices**:

- `Aero Virtio Keyboard`
- `Aero Virtio Mouse` (relative pointer)

Each device is a standard virtio 1.1 device (`VIRTIO_ID_INPUT`).

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
  - `subsel = EV_REL` → supported rel bitmap (`REL_X`, `REL_Y`, `REL_WHEEL`)
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

1. Binds to the virtio-input PCI function (standard virtio vendor/device IDs).
2. Negotiates virtio features and sets `DRIVER_OK`.
3. Creates a HID keyboard + HID mouse interface for Windows by translating `virtio_input_event` streams into HID reports.
4. Optionally forwards LED state changes (Caps Lock / Num Lock / Scroll Lock) from Windows to `statusq`.

### Installation flow (test signing)

1. **Enable test signing** (guest):
   - Run: `bcdedit /set testsigning on`
   - Reboot the VM
2. **Install the test certificate** used to sign the driver:
   - Import into **Trusted Root Certification Authorities**
   - Import into **Trusted Publishers**
3. **Install the driver**:
   - Device Manager → the virtio-input PCI device (often appears as “Unknown device”)
   - “Update driver” → “Have Disk…” → point at the driver `.inf`
4. **Verify**:
   - A new HID keyboard and HID mouse appear
   - The emulator can detect `DRIVER_OK` and switch input routing to virtio-input

### Notes

- If you want the absolute smallest driver surface area for Windows 7, a KMDF driver that exposes a HID interface is typically the pragmatic choice.
- The status queue is optional for basic input, but supporting LED updates is useful for parity with PS/2 keyboard behavior.
