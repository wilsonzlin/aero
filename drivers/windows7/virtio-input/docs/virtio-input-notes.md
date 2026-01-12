# virtio-input notes (PCI + Windows 7)

## What is virtio-input?

`virtio-input` is a virtio device type used to deliver keyboard/mouse/tablet-style input
events from a host (or emulator) to the guest.

In this project, the Windows 7 guest will see a **PCI** device, and the Aero driver
translates virtio-input events into the Windows HID stack via a **KMDF HID minidriver**.

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

The in-tree Aero virtio-input INF (`inf/aero_virtio_input.inf`) intentionally matches only **contract v1**
hardware IDs:

- `PCI\VEN_1AF4&DEV_1052&REV_01` (and the more-specific `...&SUBSYS_...&REV_01` variants)

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

If the emulator uses a non-standard ID, update:

- `inf/aero_virtio_input.inf` → `[Aero.NTx86]` and `[Aero.NTamd64]`

## QEMU device names

QEMU typically exposes virtio-input over PCI using devices such as:

- `virtio-keyboard-pci`
- `virtio-mouse-pci`
- `virtio-tablet-pci`

All of these should enumerate as a virtio-input PCI function.

## Specification pointers

When implementing/debugging the driver logic, the primary references are:

- The **virtio specification** section for the **Input Device**
  - Event types/codes and event struct layout
  - Device discovery via virtqueues and feature bits
- Linux `virtio-input` driver as a behavioral reference (event semantics)

## Windows driver model

The driver installs under `Class=HIDClass` and registers with `hidclass.sys` as a HID
minidriver.

- INF: `inf/aero_virtio_input.inf`
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
