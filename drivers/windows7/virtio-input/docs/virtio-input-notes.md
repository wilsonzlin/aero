# virtio-input notes (PCI + Windows 7)

## What is virtio-input?

`virtio-input` is a virtio device type used to deliver keyboard/mouse/tablet-style input events from a host (or emulator) to the guest.

In this project, the Windows 7 guest will see a **PCI** device, and the Aero driver will translate virtio-input events into the Windows HID stack via a **KMDF HID minidriver**.

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
The INF still matches `0x1011` for compatibility with transitional
implementations.

Contract v1 also encodes the major version in the PCI **Revision ID** (`REV_01`).
Some QEMU virtio devices report `REV_00` by default; for contract-v1 testing under
QEMU, pass `x-pci-revision=0x01` (and preferably `disable-legacy=on`) on the
`-device virtio-*-pci,...` arguments.

If the emulator uses a non-standard ID, update:

- `inf/virtio-input.inf` → `[Aero.NTx86]` and `[Aero.NTamd64]`

## QEMU device names

QEMU typically exposes virtio-input over PCI using devices such as:

- `virtio-keyboard-pci`
- `virtio-mouse-pci`
- `virtio-tablet-pci`

All of these should enumerate as a virtio-input PCI function.

## Specification pointers

When implementing the driver logic, the primary references are:

- The **virtio specification** section for the **Input Device**
  - Event types/codes and event struct layout
  - Device discovery via virtqueues and feature bits
- Linux `virtio-input` driver as a behavioral reference (event semantics)

## Windows driver model (intended)

The packaging assumes:

- `Class=HIDClass` (inbox HID class driver)
- Aero driver is a **KMDF** HID minidriver
- Service name: `aero_virtio_input`

Implementation tasks will fill in the actual HID report descriptors and I/O paths.
