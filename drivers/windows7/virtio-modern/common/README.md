# Aero Windows 7 Virtio PCI Modern Transport (Contract v1)

This directory contains a small, reusable **virtio-pci modern** transport layer for Aero’s Windows 7 drivers.

It is designed to be usable from **non-KMDF** drivers (StorPort miniport, NDIS miniport, plain WDM) and therefore **does not depend on any `WDF*` types**.

The implementation is **contract v1 only**: it assumes Aero’s fixed BAR0 MMIO layout and does **not** parse PCI virtio capabilities.

## Contract scope (what this library implements)

See `docs/windows7-virtio-driver-contract.md` for the binding contract. This transport implements:

- Fixed BAR0 MMIO layout (BAR size **>= 0x4000**):
  - `common_cfg` @ `0x0000`
  - `notify`     @ `0x1000`
  - `isr`        @ `0x2000`
  - `device_cfg` @ `0x3000`
- `notify_off_multiplier = 4`
- 64-bit feature negotiation via `*_feature_select` (always requires `VIRTIO_F_VERSION_1`)
- Queue programming via `queue_select`:
  - read `queue_size` and `queue_notify_off`
  - program `queue_desc/queue_avail/queue_used` using **32-bit** MMIO accesses
  - set `queue_enable`
- Notify doorbell writes
- INTx ISR **read-to-ack** (`isr[0]`)
- Device config reads with `config_generation` retry logic

Not implemented (out of scope for contract v1):

- Legacy/transitional virtio-pci I/O port transport
- Packed virtqueues
- MSI-X setup (INTx/ISR semantics only; MSI-X can be added later)

## Files

- `include/aero_virtio_pci_modern.h` – public API and `virtio_pci_common_cfg` layout
- `src/aero_virtio_pci_modern.c` – implementation (no WDF dependencies)

## Integrating from a driver (StorPort / NDIS / WDM)

1. **Map BAR0 MMIO** using your driver model’s APIs (examples):
   - WDM: `MmMapIoSpace`
   - StorPort: `StorPortGetDeviceBase`
   - NDIS: `NdisMMapIoSpace` (or framework equivalent)

2. **Initialize the transport**:

```c
AERO_VIRTIO_PCI_MODERN_DEVICE vdev;
NTSTATUS status = AeroVirtioPciModernInitFromBar0(&vdev, bar0_va, bar0_len);
```

3. **Negotiate features** (stops at `FEATURES_OK`; the caller typically sets `DRIVER_OK` after queues are ready):

```c
ULONGLONG negotiated = 0;
status = AeroVirtioNegotiateFeatures(&vdev,
                                     /* Required */ 0,
                                     /* Wanted   */ my_feature_mask,
                                     &negotiated);
```

4. **Discover queues and read per-queue properties**:

```c
USHORT n = AeroVirtioGetNumQueues(&vdev);

USHORT q_size = 0, q_notify_off = 0;
status = AeroVirtioQueryQueue(&vdev, /*QueueIndex*/ 0, &q_size, &q_notify_off);
```

5. **Allocate split-ring memory** (descriptor table + avail ring + used ring), then **program and enable** the queue:

```c
status = AeroVirtioSetupQueue(&vdev, 0, desc_pa, avail_pa, used_pa);
```

6. **Kick / notify**:

```c
/* queue_notify_off comes from AeroVirtioQueryQueue() */
AeroVirtioNotifyQueue(&vdev, /*QueueIndex*/ 0, q_notify_off);
```

7. **INTx ISR (read-to-ack)**:

```c
UCHAR isr = AeroVirtioReadIsr(&vdev);
if (isr & VIRTIO_PCI_ISR_QUEUE) { /* drain used rings */ }
if (isr & VIRTIO_PCI_ISR_CONFIG) { /* re-read device config */ }
```

8. **Device-specific config read with generation retry**:

```c
MY_DEVICE_CFG cfg;
status = AeroVirtioReadDeviceConfig(&vdev, 0, &cfg, sizeof(cfg));
```

## Licensing

All code in this directory is dual-licensed under **MIT OR Apache-2.0** (see SPDX headers).

