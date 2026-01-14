# virtio-input notes (PCI + Windows 7)

## What is virtio-input?

`virtio-input` is a virtio device type used to deliver keyboard/mouse/tablet-style input
events from a host (or emulator) to the guest.

In this project, the Windows 7 guest will see a **PCI** device, and the Aero driver
translates virtio-input events into the Windows HID stack via a **KMDF HID minidriver**.

For the driver’s current **supported / unsupported** feature matrix and contract constraints, see:

- [`../README.md#known-limitations`](../README.md#known-limitations)

## PCI IDs (QEMU/virtio standard)

Commonly observed IDs:

- Vendor ID: `0x1AF4` (Red Hat / virtio)
- Device ID (legacy/transitional virtio-pci ID space): `0x1011`
  - Derived as: `0x1000 + (virtio_device_type - 1)`
  - virtio device type for input is **18**, so `0x1000 + (18 - 1) = 0x1011`
- Device ID (modern virtio-pci ID space): `0x1052`
  - Derived as: `0x1040 + virtio_device_type`
  - virtio device type for input is **18**, so `0x1040 + 18 = 0x1052`

The Aero emulator’s Windows 7 virtio contract v1 uses the **modern** virtio-pci
ID space (so virtio-input is `0x1052`) and the modern virtio-pci transport.

The in-tree Aero virtio-input INFs intentionally match only **contract v1** hardware IDs (revision-gated `REV_01`).
Subsystem-qualified IDs provide distinct Device Manager names:

- Keyboard: `PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01` → `inf/aero_virtio_input.inf`
- Mouse: `PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01` → `inf/aero_virtio_input.inf`
- Strict generic fallback (no `SUBSYS`): `PCI\VEN_1AF4&DEV_1052&REV_01` → legacy filename alias INF
  (`inf/virtio-input.inf.disabled` → rename to `virtio-input.inf` to enable)
  *(shown as **Aero VirtIO Input Device** when binding via that line)*
- Tablet (absolute pointer / EV_ABS): `PCI\VEN_1AF4&DEV_1052&SUBSYS_00121AF4&REV_01` → `inf/aero_virtio_tablet.inf`

The canonical keyboard/mouse INF (`inf/aero_virtio_input.inf`) is intentionally **SUBSYS-only**:

- It matches only the subsystem-qualified keyboard/mouse HWIDs (`SUBSYS_0010`/`SUBSYS_0011`) for distinct Device Manager names.
- It does **not** include the strict generic fallback model line (no `SUBSYS`).

Tablet devices bind via `inf/aero_virtio_tablet.inf` when that INF is installed (its `SUBSYS_0012...` HWID is more specific than
the generic fallback, so it wins). If the tablet INF is not installed (or the device does not expose the tablet subsystem ID),
the generic fallback entry (when enabled via the legacy alias INF) can also bind to tablet devices (but will use the generic
device name).

The repo also carries an optional legacy filename alias INF (`inf/virtio-input.inf.disabled`, rename to `virtio-input.inf` to enable):

- Intended for compatibility with workflows/tools that still reference `virtio-input.inf`.
- Also provides an opt-in strict revision-gated generic fallback model line (no `SUBSYS`): `PCI\VEN_1AF4&DEV_1052&REV_01`.
- Policy: allowed to diverge from `inf/aero_virtio_input.inf` only in the models sections (`[Aero.NTx86]` /
  `[Aero.NTamd64]`) where it adds the fallback entry. Outside those models sections, from the first section header (`[Version]`)
  onward, it is expected to remain byte-for-byte identical (banner/comments may differ; see `../scripts/check-inf-alias.py`).
- Enabling it **does** change HWID matching behavior (it enables strict generic fallback binding).
- Do not ship/install it alongside `aero_virtio_input.inf` (install only one basename at a time).

This avoids “driver installs but won’t start” confusion: the driver enforces the
contract major version at runtime, so binding to a non-contract `REV_00` device
would otherwise install successfully but fail to start (Code 10).

If you need to support a transitional virtio-input PCI function (`DEV_1011`) or a
different revision, ship a separate INF/package rather than weakening the contract
v1 binding.

Contract v1 also encodes the major version in the PCI **Revision ID** (`REV_01`).
Some QEMU virtio devices report `REV_00` by default; for contract-v1 testing under
QEMU, pass `x-pci-revision=0x01` (and preferably `disable-legacy=on`) on the
`-device virtio-*-pci,...` arguments.

If the emulator uses a non-standard ID, update the relevant INF:

- `inf/aero_virtio_input.inf` → keyboard/mouse
- `inf/aero_virtio_tablet.inf` → tablet / absolute pointer

## QEMU device names

QEMU typically exposes virtio-input over PCI using devices such as:

- `virtio-keyboard-pci`
- `virtio-mouse-pci`
- `virtio-tablet-pci`

All of these should enumerate as a virtio-input PCI function.

### Driver device-kind classification (strict)

The in-tree Windows 7 virtio-input driver is **strict by default** (Aero contract v1):

- It queries `VIRTIO_INPUT_CFG_ID_NAME` and accepts the Aero contract strings:
  - `Aero Virtio Keyboard`
  - `Aero Virtio Mouse`
  - `Aero Virtio Tablet`
- For keyboard/mouse devices, if the name is unrecognized the driver fails start (Code 10) rather than guessing.
- For tablet/absolute-pointer devices (`EV_ABS`), if the name is unrecognized **and** the PCI subsystem device ID does **not**
  indicate an Aero contract kind (`0x0010`/`0x0011`/`0x0012`), the driver can fall back to identifying the device as a tablet
  when it advertises `EV_ABS` with `ABS_X`/`ABS_Y` in `EV_BITS`.
  - When this fallback is used, the device is treated as **non-contract**:
    - PCI subsystem kind cross-check is skipped.
    - Strict `ID_DEVIDS` validation is disabled (best-effort; mismatches tolerated).
    - `ABS_INFO` is best-effort; if unavailable, the driver falls back to the translation layer’s default coordinate scaling range (`0..32767`).
- If the PCI **Subsystem Device ID** indicates a contract kind (`0x0010` keyboard, `0x0011` mouse, `0x0012` tablet),
  it is cross-checked against `ID_NAME` **for strict ID_NAME matches** and mismatches fail start (Code 10). Unknown subsystem IDs
  (`0` or other values) are allowed.

This keeps the keyboard/mouse contract deterministic (device kind comes from `ID_NAME`) while still allowing stock QEMU-style
absolute-pointer devices to be identified via `EV_BITS` when needed.

### Optional compat mode (VIO-020)

The driver also supports an **opt-in** compatibility mode for non-Aero virtio-input frontends (notably stock QEMU).

Enable it by setting the following value (Admin CMD, then reboot or disable/enable the device):

```bat
reg add HKLM\System\CurrentControlSet\Services\aero_virtio_input\Parameters ^
  /v CompatIdName /t REG_DWORD /d 1 /f
```

When enabled, the driver will:

- Accept common QEMU `ID_NAME` strings (`QEMU Virtio Keyboard`, `QEMU Virtio Mouse`, `QEMU Virtio Tablet`).
- If `ID_NAME` is not one of the known Aero/QEMU strings, infer device kind from `EV_BITS(types)`:
  - Tablet: `EV_ABS`
  - Mouse: `EV_REL`
  - Keyboard: `EV_KEY` (fallback)
- Relax strict `ID_DEVIDS` validation.
- For tablets, `ABS_INFO` is **best-effort**; if unavailable, the driver keeps the translation layer’s default coordinate scaling range (`0..32767`).

Compat mode does **not** relax Aero transport/PCI contract checks (modern virtio-pci + `DEV_1052`, `REV_01`, fixed BAR0
layout expectations, queue sizing, etc); it only affects device identification/config validation. Strict Aero contract
v1 behavior is unchanged when compat mode is disabled.

## Specification pointers

When implementing/debugging the driver logic, the primary references are:

- The **virtio specification** section for the **Input Device**
  - Event types/codes and event struct layout
  - Device discovery via virtqueues and feature bits
- Linux `virtio-input` driver as a behavioral reference (event semantics)

## Windows driver model

The driver installs under `Class=HIDClass` and registers with `hidclass.sys` as a HID
minidriver.

- INF(s): `inf/aero_virtio_input.inf`, `inf/aero_virtio_tablet.inf`
- Service name: `aero_virtio_input`
- Driver binary: `aero_virtio_input.sys`

## HID IOCTL buffer safety (METHOD_NEITHER)

Many `IOCTL_HID_*` requests (including `IOCTL_HID_WRITE_REPORT` / `IOCTL_HID_SET_OUTPUT_REPORT`) use
**METHOD_NEITHER**. When the request originates from user mode, the request's input/output buffers
and the pointers embedded in `HID_XFER_PACKET` (e.g. `reportBuffer`) may be **user-mode pointers**.

The driver must not blindly dereference these addresses. The virtio-input driver handles this by:

- Checking `WdfRequestGetRequestorMode(Request)`.
- For `UserMode`, probing/locking and mapping the relevant user addresses into system space via MDLs
  (`IoAllocateMdl` + `MmProbeAndLockPages` + `MmGetSystemAddressForMdlSafe`), and releasing MDLs on
  request cleanup.
- Keeping a fast path for `KernelMode` requests.

This also applies to `IRP_MJ_CREATE` extended attributes (EA buffers). HID collection opens can
provide a `HidCollection` EA, and the EA buffer may be a user pointer; the driver maps it before
parsing.
