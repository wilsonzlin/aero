# Windows 7 miniport guide: virtio-pci **modern** (PCI config access + BAR0 MMIO mapping)

This is a practical bring-up recipe for **virtio-pci modern** devices (Virtio 1.0+ “vendor capability” transport) in **Windows 7 miniports**:

* **NDIS 6.20** miniports (`MiniportInitializeEx` / `MiniportHaltEx`) — e.g. virtio-net
* **StorPort** miniports (`HwFindAdapter` / `HwInitialize`) — e.g. virtio-blk

It is intentionally **not KMDF/WDF**. The goal is to make it possible to implement:

1) reading a **256-byte PCI config snapshot** (for ID checks + vendor-cap parsing), and  
2) mapping **BAR0 MMIO** from the miniport’s resource list, and  
3) the **INTx ACK rule** for virtio (read-to-clear ISR byte).

## Shipping Win7 miniports: Win7 common miniport shim (`VirtioPciModernMiniport*`)

In this repo, the **shipping** Windows 7 miniport drivers:

* `drivers/windows7/virtio-net/` (NDIS 6.20 miniport)
* `drivers/windows7/virtio-blk/` (StorPort miniport)

use the Win7 common, WDF-free miniport shim:

* `drivers/windows7/virtio/common/include/virtio_pci_modern_miniport.h`
* `drivers/windows7/virtio/common/src/virtio_pci_modern_miniport.c`

The miniport shim is shaped for the NDIS/StorPort miniport model: the driver
does the OS-specific work up front, then passes two concrete inputs into the
shim:

* a **BAR0 MMIO mapping** (`Bar0Va` + `Bar0Length`), and
* a cached **256-byte PCI config snapshot** (for ID checks + vendor-cap parsing):
  * NDIS: `NdisMGetBusData(..., PCI_WHICHSPACE_CONFIG, cfg, 0, 256)`
  * StorPort: `StorPortGetBusData(..., PCIConfiguration, ..., cfg, 0, 256)`

The entry point is `VirtioPciModernMiniportInit`, which parses vendor
capabilities and fills a `VIRTIO_PCI_DEVICE` (COMMON/NOTIFY/ISR/DEVICE config
windows, plus helpers like `VirtioPciNegotiateFeatures`, `VirtioPciSetupQueue`,
`VirtioPciNotifyQueue`, and `VirtioPciReadIsr`).

Example:

```c
VIRTIO_PCI_DEVICE dev = {0};
NTSTATUS st = VirtioPciModernMiniportInit(&dev, bar0Va, bar0Len, cfg, sizeof(cfg));
if (!NT_SUCCESS(st)) return DEVICE_NOT_SUPPORTED;
```

This is what `drivers/windows7/virtio-net/src/aero_virtio_net.c` and
`drivers/windows7/virtio-blk/src/aero_virtio_blk.c` are wired against today.

## Alternative: generic OS-callback transport (`virtio_pci_modern_transport`)

This repo also contains a more generic, OS-callback based virtio-pci modern
transport implementation:

* `drivers/windows/virtio/pci-modern/virtio_pci_modern_transport.{c,h}`

It is not specific to miniports; you integrate it by implementing
`VIRTIO_PCI_MODERN_OS_INTERFACE` (PCI config reads, BAR mapping, stalls, and a
selector-serialization lock). This can be a better fit for other driver models
(WDM/KMDF) or codebases that want the transport layer to own BAR mapping.

The rest of this document explains the underlying mechanics (PCI config offsets,
BAR discovery/mapping, INTx ISR semantics) and is useful when wiring up either
transport or debugging contract failures.

Definitive contract for what Aero expects from virtio devices/drivers:

* [`../windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md)

---

## 0) What “virtio-pci modern” means in practice

Virtio 1.0+ PCI devices expose four required memory-mapped regions via **PCI vendor-specific capabilities** (cap ID `0x09`):

* **COMMON** config (`COMMON_CFG`) — feature negotiation, queue setup, device status
* **NOTIFY** config (`NOTIFY_CFG`) — where to write queue notifications
* **ISR** config (`ISR_CFG`) — a 1-byte, **read-to-clear** interrupt status register
* **DEVICE** config (`DEVICE_CFG`) — device-specific config struct (net/blk/etc)

On Windows 7, your miniport must:

* read PCI config space to find those vendor caps (or feed a parser that does),
* map the BAR(s) that contain those regions (this doc focuses on **BAR0**),
* treat the mapped BAR as **MMIO** (use `READ_REGISTER_*` / `WRITE_REGISTER_*`),
* for **INTx**, read the ISR byte in the interrupt routine to deassert the line.

---

## 1) Common device identity checks (config space offsets)

Before attempting to parse capabilities or touch MMIO, validate the device identity from PCI config.

For Aero, the **PCI Revision ID encodes the virtio contract major version** (not “modern vs legacy” in the general virtio sense). Contract v1 uses `REV_01`.

* Vendor ID: `0x1AF4`
* Device ID:
  * `0x1041` = virtio-net
  * `0x1042` = virtio-blk
* **Revision ID**: `0x01` = `AERO-W7-VIRTIO` contract v1

Pseudocode helpers (avoid unaligned loads):

```c
static USHORT ReadLe16(const UCHAR* p) { return (USHORT)p[0] | ((USHORT)p[1] << 8); }
static ULONG  ReadLe32(const UCHAR* p) { return (ULONG)p[0] | ((ULONG)p[1] << 8) | ((ULONG)p[2] << 16) | ((ULONG)p[3] << 24); }

// Standard PCI config offsets
#define PCI_CFG_VENDOR_ID   0x00
#define PCI_CFG_DEVICE_ID   0x02
#define PCI_CFG_REVISION_ID 0x08
#define PCI_CFG_BAR0        0x10
```

Validation:

```c
USHORT vendor = ReadLe16(&cfg[PCI_CFG_VENDOR_ID]);
USHORT device = ReadLe16(&cfg[PCI_CFG_DEVICE_ID]);
UCHAR  rev    = cfg[PCI_CFG_REVISION_ID];

if (vendor != 0x1AF4) return DEVICE_NOT_SUPPORTED;
if (device != 0x1041 && device != 0x1042) return DEVICE_NOT_SUPPORTED;
if (rev != 0x01) return DEVICE_NOT_SUPPORTED; // not an AERO-W7-VIRTIO v1 device
```

---

## 2) NDIS 6.20 miniport (`MiniportInitializeEx`): PCI config + BAR0 mapping

NDIS gives you the translated PnP resources in:

```c
PNDIS_RESOURCE_LIST res = MiniportInitParameters->AllocatedResources;
```

### 2.1) Read a 256-byte PCI config snapshot (`NdisMGetBusData`)

For a PCI miniport (`Reg.InterfaceType = NdisInterfacePci`), you can read PCI config space with `NdisMGetBusData`.

Inputs you need:

* `MiniportAdapterHandle` (argument to `MiniportInitializeEx`)
* `WhichSpace = PCI_WHICHSPACE_CONFIG` (typically `0`)
* `Offset = 0`
* `Length = 256`

WinDDK 7600-compatible pseudocode:

```c
#ifndef PCI_WHICHSPACE_CONFIG
#define PCI_WHICHSPACE_CONFIG 0
#endif

UCHAR cfg[256];
RtlZeroMemory(cfg, sizeof(cfg));

ULONG bytesRead = NdisMGetBusData(
    MiniportAdapterHandle,
    PCI_WHICHSPACE_CONFIG,
    /*Buffer=*/cfg,
    /*Offset=*/0,
    /*Length=*/sizeof(cfg));

if (bytesRead != sizeof(cfg)) {
    return NDIS_STATUS_DEVICE_FAILED;
}
```

> If you later need to *write* config registers (rare for virtio modern), the symmetric API is `NdisMSetBusData`.

### 2.2) Locate BAR0 as `CmResourceTypeMemory` / `CmResourceTypeMemoryLarge` in `PNDIS_RESOURCE_LIST`

`AllocatedResources` is a `PNDIS_RESOURCE_LIST` (NDIS typedef over `CM_PARTIAL_RESOURCE_LIST`).
Scan its `PartialDescriptors[]` for memory resources.

Most virtio-pci modern devices expose the main MMIO BAR as a `CmResourceTypeMemory` range.

On some **x64** systems, PCI MMIO ranges (especially BARs mapped **above 4 GiB**) can be reported as `CmResourceTypeMemoryLarge` instead. In that case, you must:

1. Select the correct union member (`u.Memory40/48/64`) based on `Flags` (`CM_RESOURCE_MEMORY_LARGE_40/48/64`), and
2. Decode the length back to bytes (the `Length40/48/64` fields are scaled units in the WDK).

Pseudocode (WinDDK 7600 compatible):

```c
typedef struct _ADAPTER {
    NDIS_HANDLE MiniportAdapterHandle;
    NDIS_PHYSICAL_ADDRESS Bar0Pa;
    ULONG Bar0Len;
    PUCHAR Bar0Va; // mapped VA for BAR0 MMIO
} ADAPTER;

static BOOLEAN GetMemoryRangeFromDescriptor(
    _In_  const CM_PARTIAL_RESOURCE_DESCRIPTOR* d,
    _Out_ NDIS_PHYSICAL_ADDRESS* OutPa,
    _Out_ ULONG* OutLen)
{
    if (d == NULL || OutPa == NULL || OutLen == NULL) {
        return FALSE;
    }

    if (d->Type == CmResourceTypeMemory) {
        *OutPa = d->u.Memory.Start;
        *OutLen = d->u.Memory.Length;
        return TRUE;
    }

    if (d->Type == CmResourceTypeMemoryLarge) {
        ULONGLONG lenBytes = 0;
        USHORT large = d->Flags & (CM_RESOURCE_MEMORY_LARGE_40 | CM_RESOURCE_MEMORY_LARGE_48 | CM_RESOURCE_MEMORY_LARGE_64);

        switch (large) {
            case CM_RESOURCE_MEMORY_LARGE_40:
                *OutPa = d->u.Memory40.Start;
                lenBytes = ((ULONGLONG)d->u.Memory40.Length40) << 8;   // 256B units
                break;
            case CM_RESOURCE_MEMORY_LARGE_48:
                *OutPa = d->u.Memory48.Start;
                lenBytes = ((ULONGLONG)d->u.Memory48.Length48) << 16;  // 64KiB units
                break;
            case CM_RESOURCE_MEMORY_LARGE_64:
                *OutPa = d->u.Memory64.Start;
                lenBytes = ((ULONGLONG)d->u.Memory64.Length64) << 32;  // 4GiB units
                break;
            default:
                return FALSE;
        }

        // NDIS maps an ULONG length; reject descriptors that decode beyond that.
        if (lenBytes > 0xFFFFFFFFull) {
            return FALSE;
        }

        *OutLen = (ULONG)lenBytes;
        return TRUE;
    }

    return FALSE;
}

static BOOLEAN GetMemoryRangeFromNdisResources(
    _In_  PNDIS_RESOURCE_LIST Resources,
    _Out_ NDIS_PHYSICAL_ADDRESS* OutPa,
    _Out_ ULONG* OutLen)
{
    ULONG i;
    for (i = 0; i < Resources->Count; i++) {
        PCM_PARTIAL_RESOURCE_DESCRIPTOR d = &Resources->PartialDescriptors[i];
        if (GetMemoryRangeFromDescriptor(d, OutPa, OutLen)) {
            return TRUE;
        }
    }
    return FALSE;
}
```

**Which memory descriptor is BAR0?**

* If your device has only one MMIO BAR, “first `CmResourceTypeMemory` wins” is usually enough.
* If there are multiple memory ranges, the robust approach is:
  1. read BAR0 from config space (offset `0x10`),
  2. mask off the low flag bits,
  3. pick the resource descriptor whose `Start` matches.

BAR0 masking pseudocode (32-bit memory BAR case):

```c
ULONG bar0_lo = ReadLe32(&cfg[PCI_CFG_BAR0]);
if (bar0_lo & 0x1) return DEVICE_HAS_IO_BAR0_NOT_MMIO;

ULONGLONG bar0_pa = (ULONGLONG)(bar0_lo & ~0xFULL); // 16-byte aligned for mem BARs
```

> If BAR0 is 64-bit (`(bar0_lo & 0x6) == 0x4`), BAR0 consumes BAR0+BAR1 and you must combine the high DWORD as well.

### 2.3) Map BAR0 MMIO with `NdisMMapIoSpace` (and unmap in `MiniportHaltEx`)

Once you have `(Bar0Pa, Bar0Len)` from the resource list, map it:

```c
NDIS_STATUS status = NdisMMapIoSpace(
    (PVOID*)&Adapter->Bar0Va,
    Adapter->MiniportAdapterHandle,
    Adapter->Bar0Pa,
    Adapter->Bar0Len);

if (status != NDIS_STATUS_SUCCESS) {
    Adapter->Bar0Va = NULL;
    Adapter->Bar0Len = 0;
    return status;
}
```

Cleanup in `MiniportHaltEx`:

```c
if (Adapter->Bar0Va != NULL) {
    NdisMUnmapIoSpace(
        Adapter->MiniportAdapterHandle,
        Adapter->Bar0Va,
        Adapter->Bar0Len);
    Adapter->Bar0Va = NULL;
    Adapter->Bar0Len = 0;
}
```

At this point you have an MMIO base pointer:

```c
volatile UCHAR* bar0 = (volatile UCHAR*)Adapter->Bar0Va;
```

Do **not** use `READ_PORT_*` / `WRITE_PORT_*` on it.

---

## 3) StorPort miniport (`HwFindAdapter`/`HwInitialize`): PCI config + BAR0 mapping

StorPort provides translated BAR information via:

```c
PPORT_CONFIGURATION_INFORMATION configInfo;
PACCESS_RANGE ranges = configInfo->AccessRanges;
```

### 3.1) Map BAR0 MMIO from `AccessRanges[]` (`StorPortGetDeviceBase`)

For virtio modern, you want an **MMIO** access range:

* `range->RangeInMemory` must be `TRUE`
* map it with `InIoSpace = FALSE`

Pseudocode:

```c
PACCESS_RANGE bar0 = NULL;
ULONG i;

for (i = 0; i < configInfo->NumberOfAccessRanges; i++) {
    if (configInfo->AccessRanges[i].RangeLength == 0) continue;
    if (configInfo->AccessRanges[i].RangeInMemory) {
        bar0 = &configInfo->AccessRanges[i];
        break;
    }
}

if (bar0 == NULL) return SP_RETURN_NOT_FOUND;

PVOID bar0Va = StorPortGetDeviceBase(
    DeviceExtension,
    configInfo->AdapterInterfaceType,   // typically PCIBus
    configInfo->SystemIoBusNumber,
    bar0->RangeStart,
    bar0->RangeLength,
    /*InIoSpace=*/FALSE);               // FALSE = memory-mapped

if (bar0Va == NULL) return SP_RETURN_NOT_FOUND;
```

Save:

* `bar0->RangeStart` (physical)
* `bar0->RangeLength`
* `bar0Va` (virtual)

### 3.2) Read PCI config space (`busInformation` and/or `StorPortGetBusData`)

In `HwFindAdapter`, StorPort passes a `busInformation` pointer which, for PCI, commonly points at a `PCI_COMMON_CONFIG` snapshot.

Two practical patterns:

#### Pattern A (fast path): use `busInformation` when it’s a full header

```c
PPCI_COMMON_CONFIG pci = (PPCI_COMMON_CONFIG)busInformation;
if (pci != NULL) {
    USHORT vendor = pci->VendorID;
    USHORT device = pci->DeviceID;
    UCHAR  rev    = pci->RevisionID;
    // ... validate IDs ...
}
```

This is enough for Vendor/Device/Revision checks, but it is not guaranteed to contain the full 256 bytes needed for capability parsing.

#### Pattern B (robust): always fetch 256 bytes with `StorPortGetBusData`

WinDDK 7600-compatible pseudocode:

```c
UCHAR cfg[256];
RtlZeroMemory(cfg, sizeof(cfg));

ULONG bytesRead = StorPortGetBusData(
    DeviceExtension,
    /*BusDataType=*/PCIConfiguration,
    /*SystemIoBusNumber=*/configInfo->SystemIoBusNumber,
    /*SlotNumber=*/configInfo->SlotNumber,
    /*Buffer=*/cfg,
    /*Offset=*/0,
    /*Length=*/sizeof(cfg));

if (bytesRead != sizeof(cfg)) return SP_RETURN_NOT_FOUND;
```

> Some WDKs expose `StorPortGetBusData` without an explicit `Offset` parameter (mirroring `HalGetBusData` vs `HalGetBusDataByOffset`).
> The intent is the same: you must obtain a stable 256-byte config snapshot for vendor-cap parsing.

---

## 4) Parsing virtio vendor capabilities (COMMON/NOTIFY/ISR/DEVICE)

Once you have:

* `cfg[256]` — PCI config snapshot
* BAR base addresses (at minimum BAR0’s physical base), and
* BAR0 mapped as MMIO (`bar0Va`)

…you can parse virtio vendor caps and derive register pointers.

### 4.1) Use the portable parser (recommended)

This repo includes a portable vendor-cap parser (C99, tested outside Windows):

* `drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.{h,c}`

The key API is:

```c
virtio_pci_cap_parse_result_t virtio_pci_cap_parse(
    const uint8_t *cfg_space,
    size_t cfg_space_len,
    const uint64_t bar_addrs[6],
    virtio_pci_parsed_caps_t *out_caps);
```

To feed it from a miniport, populate `bar_addrs[]` from your resource list.
If you only mapped BAR0, you can at least provide BAR0’s physical base:

```c
uint64_t bar_addrs[6] = {0};
bar_addrs[0] = (uint64_t)Bar0Pa.QuadPart; // NDIS: Adapter->Bar0Pa; StorPort: bar0->RangeStart

virtio_pci_parsed_caps_t parsed;
virtio_pci_cap_parse_result_t r = virtio_pci_cap_parse(cfg, sizeof(cfg), bar_addrs, &parsed);
if (r != VIRTIO_PCI_CAP_PARSE_OK) return DEVICE_FAILED;
```

If the parser reports that a required capability lives in a BAR other than 0, you must:

1. map that BAR as well (same technique as BAR0), and
2. use the correct `bar_addrs[bar]` and `bar_va[bar]` when forming pointers.

### 4.2) Turn parsed offsets into mapped pointers (BAR0 case)

With BAR0 mapped at `bar0Va`:

```c
volatile UCHAR* bar0 = (volatile UCHAR*)bar0Va;

volatile struct virtio_pci_common_cfg* common =
    (volatile struct virtio_pci_common_cfg*)(bar0 + parsed.common_cfg.offset);

volatile UCHAR* isr_status =
    (volatile UCHAR*)(bar0 + parsed.isr_cfg.offset); // 1 byte, read-to-clear

volatile UCHAR* device_cfg =
    (volatile UCHAR*)(bar0 + parsed.device_cfg.offset);

volatile UCHAR* notify_base =
    (volatile UCHAR*)(bar0 + parsed.notify_cfg.offset);

ULONG notify_off_multiplier = parsed.notify_off_multiplier;
```

And when notifying a queue:

```c
// queue_notify_off is read from common_cfg after selecting the queue.
USHORT queue_notify_off = READ_REGISTER_USHORT(&common->queue_notify_off);

volatile USHORT* notify_addr =
    (volatile USHORT*)(notify_base + ((ULONG)queue_notify_off * notify_off_multiplier));

// Modern virtio-pci uses a 16-bit MMIO write; the value is the queue index.
WRITE_REGISTER_USHORT(notify_addr, QueueIndex);
```

---

## 5) MMIO access rules (Win7)

For virtio-pci modern, the capability regions are **MMIO**, not port I/O.

Use:

* `READ_REGISTER_UCHAR/USHORT/ULONG`
* `WRITE_REGISTER_UCHAR/USHORT/ULONG`

Do **not** use:

* `READ_PORT_*` / `WRITE_PORT_*`

Rationale: the mapping returned by `NdisMMapIoSpace` / `StorPortGetDeviceBase(..., InIoSpace=FALSE)` is memory-mapped and must obey MMIO semantics.

---

## 6) INTx interrupt ACK/deassert rule: read ISR status byte

Virtio’s legacy **INTx** line is **level-triggered**. The device deasserts the line only when the driver reads the 1-byte **ISR status** register (`ISR_CFG` capability).

Therefore:

1. In your interrupt routine, do a **1-byte MMIO read** of `isr_status`.
2. If it reads `0`, it wasn’t your interrupt (important on shared lines); return “not handled”.
3. Otherwise, you have ACKed/deasserted the line; schedule your DPC/notification and handle the bits.

Pseudocode (generic):

```c
// Runs at DIRQL in both NDIS and StorPort.
BOOLEAN VirtioIntxIsr(...)
{
    UCHAR isr = READ_REGISTER_UCHAR(isr_status); // read-to-clear + deassert
    if (isr == 0) {
        return FALSE; // shared interrupt, not ours
    }

    // Save isr for DPC because the register is now cleared.
    Adapter->PendingIsrStatus |= isr;

    QueueDpcOrDeferredWork();
    return TRUE;
}
```

* bit 0 (`0x01`): queue interrupt
* bit 1 (`0x02`): device config change interrupt

With **MSI-X**, there is no line to deassert; do not rely on `isr_status` reads for ACK.

---

## 7) Minimal bring-up checklist (miniport)

1. Read 256 bytes of PCI config space.
2. Verify `(VendorID, DeviceID, RevisionID) == (0x1AF4, 0x1041/0x1042, 0x01)`.
3. Find BAR0 as a translated **memory** resource and map it as **MMIO**.
4. Parse virtio vendor caps (portable parser recommended) and compute MMIO pointers.
5. Access MMIO with `READ_REGISTER_*` / `WRITE_REGISTER_*`.
6. For INTx, **read ISR byte in the ISR** to ACK/deassert.
