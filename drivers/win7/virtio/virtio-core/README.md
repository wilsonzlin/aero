# virtio-core (Win7 KMDF) — Virtio 1.0 PCI “modern” discovery + BAR mapping

This directory contains a small, reusable transport layer for **Virtio 1.0 PCI modern devices** on **Windows 7 (WDK 7.1, KMDF)**.

It discovers Virtio vendor-specific PCI capabilities (COMMON/NOTIFY/ISR/DEVICE config) and maps the required BAR(s) into kernel virtual space using `MmMapIoSpace`.

## Files

- `include/virtio_spec.h` — minimal Virtio 1.0 structures/constants (common config layout)
- `include/virtio_pci_caps.h` — Virtio PCI capability structs and discovery output
- `include/virtio_pci_modern.h` — public API + `VIRTIO_PCI_MODERN_DEVICE`
- `src/virtio_pci_caps.c` — PCI capability list walking + virtio cap parsing
- `src/virtio_pci_modern.c` — KMDF-facing init + BAR matching/mapping + diagnostics

## Integration (KMDF driver)

1. Add these files to your driver project:
   - Add `drivers/win7/virtio/virtio-core/src/*.c` to the build
   - Add `drivers/win7/virtio/virtio-core/include` to the include path

2. Add a `VIRTIO_PCI_MODERN_DEVICE` to your device context:

```c
#include "virtio_pci_modern.h"

typedef struct _DEVICE_CONTEXT {
    VIRTIO_PCI_MODERN_DEVICE Virtio;
    // ...
} DEVICE_CONTEXT, *PDEVICE_CONTEXT;

WDF_DECLARE_CONTEXT_TYPE_WITH_NAME(DEVICE_CONTEXT, DeviceGetContext);
```

3. In `EvtDevicePrepareHardware`, initialize + map:

```c
NTSTATUS
EvtDevicePrepareHardware(
    _In_ WDFDEVICE Device,
    _In_ WDFCMRESLIST ResourcesRaw,
    _In_ WDFCMRESLIST ResourcesTranslated)
{
    NTSTATUS status;
    PDEVICE_CONTEXT ctx = DeviceGetContext(Device);

    status = VirtioPciModernInit(Device, &ctx->Virtio);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    status = VirtioPciModernMapBars(&ctx->Virtio, ResourcesRaw, ResourcesTranslated);
    if (!NT_SUCCESS(status)) {
        VirtioPciModernUninit(&ctx->Virtio);
        return status;
    }

    /* Optional diagnostics */
    VirtioPciModernDumpBars(&ctx->Virtio);
    VirtioPciModernDumpCaps(&ctx->Virtio);

    return STATUS_SUCCESS;
}
```

4. In `EvtDeviceReleaseHardware` (or `EvtDeviceContextCleanup`), unmap + release:

```c
VOID
EvtDeviceReleaseHardware(_In_ WDFDEVICE Device, _In_ WDFCMRESLIST ResourcesTranslated)
{
    UNREFERENCED_PARAMETER(ResourcesTranslated);
    VirtioPciModernUninit(&DeviceGetContext(Device)->Virtio);
}
```

After `VirtioPciModernMapBars` succeeds, the driver can use:

- `Dev->CommonCfg` (`volatile struct virtio_pci_common_cfg *`)
- `Dev->NotifyBase` + `Dev->NotifyOffMultiplier`
- `Dev->IsrStatus`
- `Dev->DeviceCfg`

## Diagnostics

To enable `DbgPrintEx` output from this library, set a compile-time define:

```
VIRTIO_CORE_ENABLE_DIAGNOSTICS=1
```

When enabled, `VirtioPciModernDumpBars()` and `VirtioPciModernDumpCaps()` emit:

- all virtio vendor capabilities discovered in PCI config space
- BAR base addresses, resource lengths, and mapped virtual addresses

