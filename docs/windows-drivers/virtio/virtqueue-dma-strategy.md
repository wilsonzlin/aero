# Virtqueue DMA Strategy (Windows 7 KMDF, virtio 1.0 PCI modern)

This document describes a **recommended DMA and memory-allocation strategy** for Aero’s Windows 7 KMDF virtio drivers when implementing **virtio 1.0 PCI “modern”** devices using the **split virtqueue** layout (descriptor table + avail ring + used ring).

See also:

* [`../../virtio/virtqueue-split-ring-win7.md`](../../virtio/virtqueue-split-ring-win7.md) — split-ring virtqueue algorithms (descriptor mgmt, ordering/barriers, EVENT_IDX, indirect).
* [`../../windows7-virtio-driver-contract.md`](../../windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.
* `drivers/windows/virtio/common/` — reference split virtqueue implementation (helpers like `VirtqSplitRingMemSize`, `VirtqSplitInit`, `VirtqSplitAddBuffer`, ...), plus a WDF-free PFN/MDL → SG builder (`virtio_sg_pfn.h/.c`).

The focus is on:

* Allocating virtqueue rings / indirect tables in DMA-safe memory.
* Mapping I/O buffers (IRP MDLs) into virtio descriptors robustly.
* Correct cache + ordering rules so the device and CPU agree on ring contents.

Non-goals:

* Full virtio feature negotiation walkthrough (except where it affects DMA: `INDIRECT_DESC`, `EVENT_IDX`).
* Packed virtqueues (virtio 1.1+).

---

## 1) DMA enabler creation (profile selection, MaximumLength, SG limits)

### 1.1 Preferred profile: 64-bit duplex, fallback to 32-bit duplex

Virtio queues frequently involve **both directions** at the driver level (e.g., virtio-blk read: header OUT, data IN, status IN). Even though an individual DMA transaction is still single-direction, using a **duplex** profile avoids artificial restrictions and is the right default for a driver that will have both read and write DMA activity.

Recommendation:

1. Try `WdfDmaProfileScatterGather64Duplex`
2. If unsupported, fall back to `WdfDmaProfileScatterGather32Duplex`

Notes:

* On a 32-bit OS build, everything is <4GiB anyway; using the 32-bit profile is typically fine.
* On systems with an IOMMU / DMA remapping, the “device address” you must give virtio is not necessarily the CPU physical address. **Use the DMA framework’s mapped addresses** (logical/bus addresses), not `MmGetPhysicalAddress`.

Pseudo-code:

```c
NTSTATUS CreateDmaEnabler(
    _In_ WDFDEVICE Device,
    _In_ size_t MaximumLength,
    _In_ ULONG MaxSgElements,
    _Out_ WDFDMAENABLER* DmaEnablerOut
    )
{
    NTSTATUS status;
    WDFDMAENABLER dma = NULL;
    WDF_DMA_ENABLER_CONFIG cfg;

    WDF_DMA_ENABLER_CONFIG_INIT(&cfg, WdfDmaProfileScatterGather64Duplex, MaximumLength);
    status = WdfDmaEnablerCreate(Device, &cfg, WDF_NO_OBJECT_ATTRIBUTES, &dma);

    if (!NT_SUCCESS(status)) {
        WDF_DMA_ENABLER_CONFIG_INIT(&cfg, WdfDmaProfileScatterGather32Duplex, MaximumLength);
        status = WdfDmaEnablerCreate(Device, &cfg, WDF_NO_OBJECT_ATTRIBUTES, &dma);
        if (!NT_SUCCESS(status)) {
            return status;
        }
    }

    // Clamp WDF’s SG list length so we can always represent it in virtio descriptors.
    WdfDmaEnablerSetMaximumScatterGatherElements(dma, MaxSgElements);

    *DmaEnablerOut = dma;
    return STATUS_SUCCESS;
}
```

### 1.2 Choosing `MaximumLength` (per-transaction length)

`MaximumLength` is the **maximum number of bytes** that a single DMA transaction may map. Pick a value that:

* Covers the driver’s largest “one request” payload (or the largest MDL you will map in one go).
* Does not explode memory usage in WDF’s DMA map-register/bounce-buffer paths.
* Fits within the device’s limitations (e.g., virtio-blk `seg_max`/`size_max` if present).

Important behavioral note:

* If the request buffer exceeds `MaximumLength`, WDF can split the work into multiple DMA programming phases (multiple `EvtProgramDma` calls for the same transaction). That model doesn’t map cleanly to a “one virtio request = one descriptor chain” design, so prefer **`MaximumLength >= max_request_bytes`**.

Concrete recommendations:

* virtio-blk: choose `MaximumLength = min(OS_max_transfer, device_size_max_if_known)`; a conservative default is **1 MiB** per request.
* virtio-net: choose `MaximumLength` around typical packet aggregation sizes; **64 KiB** is a safe upper bound for a single packet buffer chain, but large-send/TSO paths may want more if supported.

The key is consistency with your descriptor budgeting (indirect table sizing) described below.

### 1.3 Choosing `MaxSgElements` (`WdfDmaEnablerSetMaximumScatterGatherElements`)

If your virtqueue implementation can only represent **N** scatter-gather segments for “the data payload”, then you must clamp WDF to **at most N**, or you risk receiving an `SCATTER_GATHER_LIST` you cannot encode.

Concrete recommendation:

* Size the per-request indirect descriptor table to a fixed maximum (example below), and set:
  * `MaxSgElements = MaxIndirectDescriptorsForData`
* Additionally reserve descriptors for protocol headers/status that are not part of the data SG list.

Important behavioral note:

* If the MDL requires more than `MaxSgElements` segments under the platform’s DMA constraints, WDF can again split the DMA programming into multiple phases. For virtio queues, it’s usually better to **bound request sizes** so you never hit this.

Example budgeting for virtio-blk using one indirect table per request:

```
Indirect table layout:
  [0] = virtio-blk header (OUT, common buffer)
  [1..N] = data SG elements (IN for READ, OUT for WRITE, from WDF SG list)
  [N+1] = status byte (IN, common buffer)

MaxIndirectDescriptorsTotal = N + 2
MaxSgElements (WDF) = N
```

Pitfall: do not confuse **virtqueue ring size** (number of ring descriptors) with **scatter-gather element count**. With `INDIRECT_DESC`, each request consumes **1** ring descriptor, but still needs `N + overhead` indirect descriptors.

---

## 2) Rings/indirect tables: WDF common buffers vs `MmAllocateContiguousMemory*`

### Recommendation: prefer `WdfCommonBufferCreateWithConfig`

For virtqueue rings (desc/avail/used) and indirect descriptor tables, prefer **WDF common buffers** over raw `MmAllocateContiguousMemory*` allocations.

Why:

* **DMA adapter aware**: the allocation is made for the correct DMA adapter associated with the PCI device.
* **IOMMU / bounce support**: WDF can ensure the device can access the memory even if the platform requires mapping/bouncing. The returned address is a **device DMA address**.
* **Returns the correct address type**: `WdfCommonBufferGetAlignedLogicalAddress()` returns the bus/logical address that should be programmed into the virtio device. `MmAllocateContiguousMemory*` only gives you CPU-physical contiguity, not the device’s view.
* **Lifetime management**: the common buffer is a WDF object that can be parented to the device/queue object and automatically freed.

Avoid `MmAllocateContiguousMemory*` for production drivers because it:

* Encourages using `MmGetPhysicalAddress`/CPU physical addresses (wrong with IOMMU).
* Is not integrated with WDF’s DMA resource accounting.
* Tends to fail more often under memory pressure / fragmentation.

### Recommended common-buffer configuration

* `CacheEnabled = FALSE` for rings/indirect tables (descriptor rings are small; correctness beats cache speed).
* `RequestedAlignment = PAGE_SIZE` to simplify virtqueue layout math and to satisfy all virtqueue alignment constraints trivially.

Pseudo-code:

```c
WDF_COMMON_BUFFER_CONFIG cbcfg;
WDF_COMMON_BUFFER_CONFIG_INIT(&cbcfg, /*PoolType*/ NonPagedPool);
cbcfg.CacheEnabled = FALSE;
cbcfg.RequestedAlignment = PAGE_SIZE;

status = WdfCommonBufferCreateWithConfig(
    DmaEnabler,
    RingBytes,
    WDF_NO_OBJECT_ATTRIBUTES,
    &cbcfg,
    &Vq->RingCommonBuffer);

Vq->RingVa = WdfCommonBufferGetAlignedVirtualAddress(Vq->RingCommonBuffer);
Vq->RingPa = WdfCommonBufferGetAlignedLogicalAddress(Vq->RingCommonBuffer); // device address
```

---

## 3) Split virtqueue ring alignment + exact size formulas

Virtio split-ring layout has three regions:

1. `virtq_desc` table (array of descriptors)
2. `virtq_avail` ring (driver → device)
3. `virtq_used` ring (device → driver)

### 3.1 Alignment requirements

* Descriptor table: **16-byte aligned**
* Avail ring: **2-byte aligned**
* Used ring: **4-byte aligned**

Recommendation:

* Allocate a single common buffer for the whole ring with **PAGE_SIZE alignment** and then compute offsets with `align_up()`. A page-aligned base is automatically aligned for 16/4/2.

### 3.2 Size formulas (with/without `EVENT_IDX`)

Let:

* `qsz` = virtqueue size (`QueueSize`), in descriptors
* `event_idx` = negotiated `VIRTIO_F_RING_EVENT_IDX` (true/false)

Then:

**Descriptor table**

* Each `virtq_desc` is 16 bytes
* `desc_len = 16 * qsz`

**Avail ring**

* Fields: `u16 flags; u16 idx; u16 ring[qsz];` and optionally `u16 used_event;`
* `avail_len = 4 + (2 * qsz) + (event_idx ? 2 : 0)`

**Used ring**

* Each `virtq_used_elem` is 8 bytes (`u32 id; u32 len;`)
* Fields: `u16 flags; u16 idx; struct virtq_used_elem ring[qsz];` and optionally `u16 avail_event;`
* `used_len = 4 + (8 * qsz) + (event_idx ? 2 : 0)`

### 3.3 Computing offsets (pseudo-code)

```c
static __forceinline size_t AlignUp(size_t x, size_t a)
{
    return (x + (a - 1)) & ~(a - 1);
}

void VirtqComputeLayout(
    _In_  USHORT qsz,
    _In_  BOOLEAN event_idx,
    _Out_ size_t* desc_off,
    _Out_ size_t* avail_off,
    _Out_ size_t* used_off,
    _Out_ size_t* total_bytes
    )
{
    size_t desc_len  = 16u * (size_t)qsz;
    size_t avail_len = 4u + 2u * (size_t)qsz + (event_idx ? 2u : 0u);
    size_t used_len  = 4u + 8u * (size_t)qsz + (event_idx ? 2u : 0u);

    *desc_off  = AlignUp(0, 16);
    *avail_off = AlignUp(*desc_off + desc_len, 2);
    *used_off  = AlignUp(*avail_off + avail_len, 4);

    // Optional but recommended: round up to a whole page for simpler debug and future growth.
    *total_bytes = AlignUp(*used_off + used_len, PAGE_SIZE);
}
```

Pitfall: Do not assume `avail_off + avail_len` is already 4-byte aligned (it often isn’t), so always `align_up()` before placing the used ring.

---

## 4) Buffer mapping approaches (robust vs direct)

### 4.1 Robust path: `WDFDMATRANSACTION` → `EvtProgramDma` → `SCATTER_GATHER_LIST`

Use this path for “real” DMA correctness:

* Works with IOMMU / DMA remapping.
* Works with devices limited to 32-bit addresses (WDF will bounce/map as needed).
* Gives you properly coalesced SG segments per the platform’s DMA constraints.

Recommended flow (per request):

1. Allocate/prepare a per-request context that owns:
   * A ring descriptor index (1 if using `INDIRECT_DESC`)
   * A slot in a common-buffer “indirect table pool”
   * A `WDFDMATRANSACTION` (for the data payload MDL)
2. Initialize the DMA transaction for the request’s main data MDL:
   * Direction depends on operation (virtio-blk READ = device writes into memory → `WdfDmaDirectionReadFromDevice`)
3. `WdfDmaTransactionExecute()` triggers `EvtProgramDma(...)` with an `SCATTER_GATHER_LIST`
4. In `EvtProgramDma`, build the virtio descriptor chain using:
   * Common-buffer header/status descriptors (small, fixed)
   * SG list elements for the data payload (variable)
5. Publish the head descriptor to the avail ring and notify the device.
6. **Keep the DMA transaction alive** until the device reports completion via the used ring, then call:
   * `WdfDmaTransactionDmaCompletedFinal(Transaction, 0 /* ignored */);`

Why “keep it alive”:

* If WDF had to allocate bounce buffers/map registers, the addresses given in the SG list are only valid while the transaction is active.
* Completing/finalizing the transaction too early can lead to device DMA into freed/reused mappings.

IRQL/locking note:

* `EvtProgramDma` can run at **DISPATCH_LEVEL**. Treat it like a DPC: no pageable code, no blocking waits, and avoid lock inversion with your completion path.

#### (Aero repo) reusable mapping helpers

The Aero repo includes a small, reusable helper layer that wraps the KMDF DMA transaction flow and converts the mapped
`SCATTER_GATHER_LIST` into a virtio-friendly `(addr,len)` list:

* `windows-drivers/virtio-kmdf/common/virtio_sg.h`
* `windows-drivers/virtio-kmdf/common/virtio_sg_wdfdma.c`

Key API:

* `VirtioWdfDmaStartMapping(...)` — starts the DMA transaction, builds `VIRTIO_SG_ELEM[]` from the `SCATTER_GATHER_LIST`,
  and keeps the transaction alive until completion.
* `VirtioWdfDmaCompleteAndRelease(...)` — call on virtqueue completion to finalize the DMA transaction and free resources.

On success, the returned `VIRTIO_WDFDMA_MAPPING` contains `mapping->Sg.Elems[0..mapping->Sg.Count-1]` (device/bus
addresses + lengths) ready to be copied into virtio descriptors/indirect tables.

Note: the helper is intentionally **single-shot** (one DMA programming callback must cover the entire buffer). Ensure your
DMA enabler’s `MaximumLength` and `MaxScatterGatherElements` are sized to match the largest request you will submit.

Pseudo-code outline:

```c
BOOLEAN EvtProgramDma(
    WDFDMATRANSACTION Transaction,
    WDFDEVICE Device,
    PVOID Context,
    WDF_DMA_DIRECTION Direction,
    PSCATTER_GATHER_LIST SgList
    )
{
    REQUEST_CTX* req = Context;

    // Build indirect table in common-buffer memory.
    VIRTQ_DESC* itbl = req->IndirectDescVa;
    ULONG n = 0;

    // 1) protocol header (common buffer, OUT)
    itbl[n++] = MakeDesc(req->HdrDmaAddr, sizeof(req->Hdr), /*write=*/FALSE);

    // 2) data payload (from WDF SG list; direction depends on request)
    for (ULONG i = 0; i < SgList->NumberOfElements; i++) {
        itbl[n++] = MakeDesc(SgList->Elements[i].Address, SgList->Elements[i].Length,
                             /*write=*/(Direction == WdfDmaDirectionReadFromDevice));
    }

    // 3) status byte (common buffer, IN)
    itbl[n++] = MakeDesc(req->StatusDmaAddr, 1, /*write=*/TRUE);

    // Ring descriptor points at the indirect table (device reads indirect table)
    Vq->Desc[req->RingDescIndex] =
        MakeIndirectDesc(req->IndirectTableDmaAddr, n * sizeof(VIRTQ_DESC));

    VirtqPublishAvail(Vq, req->RingDescIndex);
    return TRUE;
}
```

### 4.2 Direct path (emulated-only): build SG from MDL PFN array

This approach is sometimes used in hypervisor/emulated environments where:

* The “device” effectively sees guest physical memory directly.
* There is no IOMMU remapping.
* You control the platform assumptions.

Strategy:

1. Use `MmGetMdlPfnArray(Mdl)` to obtain PFNs.
2. Coalesce physically-contiguous PFNs into `(Address, Length)` segments.
3. Use those addresses directly in virtio descriptors.

Pseudo-code for coalescing PFNs:

```c
PMDL mdl = ...;
PPFN_NUMBER pfns = MmGetMdlPfnArray(mdl);
ULONG pages = ADDRESS_AND_SIZE_TO_SPAN_PAGES(MmGetMdlVirtualAddress(mdl), MmGetMdlByteCount(mdl));
ULONG offset = MmGetMdlByteOffset(mdl);
ULONG remaining = MmGetMdlByteCount(mdl);

for (ULONG p = 0; p < pages; ) {
    PFN_NUMBER first = pfns[p];
    ULONG run = 1;
    while ((p + run) < pages && pfns[p + run] == first + run) {
        run++;
    }

    PHYSICAL_ADDRESS pa;
    pa.QuadPart = ((ULONGLONG)first << PAGE_SHIFT);

    size_t seg_len = (size_t)run * PAGE_SIZE;
    size_t seg_off = (p == 0) ? offset : 0;
    size_t seg_use = min(seg_len - seg_off, (size_t)remaining);

    EmitVirtioDesc(pa.QuadPart + seg_off, (ULONG)seg_use, ...);

    remaining -= (ULONG)seg_use;
    p += run;
}
```

Cache flushing:

* For **OUT** buffers (device reads from memory): flush CPU writes before notifying the device:
  * `KeFlushIoBuffers(mdl, FALSE /* ReadOperation */, TRUE /* DmaOperation */)`
* For **IN** buffers (device writes to memory): ensure dirty CPU cache lines won’t overwrite device data and invalidate CPU caches so reads see device writes:
  * `KeFlushIoBuffers(mdl, TRUE /* ReadOperation */, TRUE /* DmaOperation */)` before handing the buffer to the device, and again after completion if the CPU will read the data.

Pitfalls of the direct path (why it’s “emulated-only”):

* PFN-derived “physical” addresses are not valid device addresses with IOMMU/bounce.
 * On systems with 32-bit DMA limitations, the device may not be able to reach high PFNs.
 * You must do your own SG element limiting/coalescing; WDF normally handles this.

#### (Aero repo) direct MDL → PFN mapping helper

For drivers that intentionally use PFN-derived physical addresses (e.g., in fully emulated environments), Aero includes a
direct mapping helper that walks an MDL chain and emits per-page segments, coalescing physically-contiguous PFNs:

* `windows-drivers/virtio-kmdf/common/virtio_sg.h`
* `windows-drivers/virtio-kmdf/common/virtio_sg.c`
* WDF-free (WDM-friendly) variant: `drivers/windows/virtio/common/virtio_sg_pfn.h/.c`

Key API:

* `VirtioSgMaxElemsForMdl(...)` — worst-case upper bound on element count (pages spanned).
* `VirtioSgBuildFromMdl(...)` — builds `VIRTIO_SG_ELEM[]` in caller-provided storage (no allocations; DISPATCH_LEVEL safe).

### 4.3 Mixed-direction chains: recommended pattern (virtio-blk, virtio-net)

Virtio request chains typically contain a mix of:

* device-readable buffers (OUT, driver → device)
* device-writable buffers (IN, device → driver)

Instead of trying to represent an entire mixed chain via a single WDF DMA transaction:

**Recommended pattern**

* Allocate small, fixed-size per-request buffers (headers/status) from a **common-buffer slab**.
  * Always DMA-safe, device-addressable, and physically contiguous.
* Use a WDF DMA transaction **only for the bulk data payload**, which is naturally single-direction.

Examples:

* **virtio-blk READ**
  * header (OUT, common buffer)
  * data (IN, DMA transaction SG list)
  * status (IN, common buffer)
* **virtio-blk WRITE**
  * header (OUT, common buffer)
  * data (OUT, DMA transaction SG list)
  * status (IN, common buffer)
* **virtio-net TX**
  * virtio-net header (OUT, common buffer)
  * payload (OUT, DMA transaction SG list)
* **virtio-net RX**
  * virtio-net header (IN, common buffer)
  * payload buffers (IN, DMA transaction SG list, or pre-posted buffers)

### 4.4 Indirect descriptors: make each request consume 1 ring descriptor

If `VIRTIO_F_RING_INDIRECT_DESC` is negotiated:

* Each in-flight request uses **one** entry in the main ring descriptor table.
* That ring descriptor points to a per-request indirect table in common-buffer memory.

Benefits:

* Simplifies “queue full” handling (max in-flight requests == ring size).
* Reduces ring descriptor pressure when a request has many SG segments.
* Keeps the main ring stable: a single `avail->ring[i] = head_desc` publish per request.

Pitfall: the indirect table memory must be described by a single `(addr,len)` in the ring descriptor, so allocate it from **physically contiguous, DMA-safe memory** (common buffer). Do not build indirect tables in nonpaged pool unless you also map them as a single contiguous DMA segment (which is usually not possible).

---

## 5) Cache coherency and memory ordering (barriers)

### 5.1 Common buffer caching (`CacheEnabled`)

* `CacheEnabled = FALSE` (uncached) is the safest default for rings/indirect tables.
  * Avoids subtle coherency issues on non-coherent platforms.
  * Rings are tiny, so performance impact is negligible.
* If `CacheEnabled = TRUE` (cached):
  * You must ensure the device sees descriptor writes promptly and that CPU sees device writes to used ring promptly.
  * In practice on x86 this is usually coherent, but the driver should not rely on it.
  * If in doubt, allocate rings uncached.

For MDL-mapped data buffers:

* Robust/WDF path: calling `WdfDmaTransactionDmaCompletedFinal` is part of the contract that makes CPU/device views consistent.
* Direct PFN path: use `KeFlushIoBuffers` as described above.

### 5.2 Required barriers when publishing to the avail ring

Rule: the device must never observe `avail->idx` incremented before it can observe the descriptor contents and the corresponding `avail->ring[]` entry.

Pseudo-code:

```c
void VirtqPublishAvail(_Inout_ VIRTQ* vq, _In_ USHORT head_desc)
{
    USHORT idx = vq->avail->idx; // local snapshot

    vq->avail->ring[idx % vq->qsz] = head_desc;

    // Ensure all descriptor writes + ring entry write are globally visible
    // before we publish the new avail->idx.
    KeMemoryBarrier();

    vq->avail->idx = (USHORT)(idx + 1);

    // Ensure avail->idx is visible before ringing the doorbell/MMIO notify.
    KeMemoryBarrier();
    VirtioQueueNotify(vq);
}
```

If `EVENT_IDX` is negotiated, your notify decision is gated by `used_event` in the avail ring; ensure you use barriers around reading `used_event` and writing `avail->idx` per the virtio spec.

### 5.3 Required barriers when consuming from the used ring

Rule: the driver must not read `used->ring[]` entries before it has observed the corresponding `used->idx`.

Pseudo-code:

```c
void VirtqConsumeUsed(_Inout_ VIRTQ* vq)
{
    USHORT used_idx = vq->used->idx;

    // Ensure subsequent reads of used->ring see entries associated with used_idx.
    KeMemoryBarrier();

    while (vq->last_used_idx != used_idx) {
        ULONG i = vq->last_used_idx % vq->qsz;
        struct virtq_used_elem e = vq->used->ring[i];

        vq->last_used_idx++;

        CompleteRequestById(e.id, e.len);
    }
}
```

---

## 6) Cleanup requirements (PnP stop/start, resource rebalance)

### 6.1 What to free in `EvtDeviceReleaseHardware` (and why)

Windows can stop/start a device without unloading the driver (PnP stop for rebalance, surprise remove, etc.). `EvtDeviceReleaseHardware` is where you must release resources that are tied to the current hardware resources / DMA adapter.

Free/delete in `EvtDeviceReleaseHardware`:

* Virtqueue ring common buffers (desc/avail/used allocation).
* Indirect table pool common buffers (if separate from ring).
* Header/status common-buffer slabs.
* WDF DMA objects that are adapter-specific:
  * `WDFDMAENABLER` (if created per hardware instance)
  * Any persistent DMA transactions tied to queues

Reason:

* The DMA adapter and its constraints can change after stop/start (different resources, different remapping behavior). Reusing old “device addresses” across a rebalance is a correctness bug.

### 6.2 Quiescing outstanding DMA before freeing rings

Before freeing any ring/indirect-table memory, you must ensure:

* The virtio device is no longer executing DMA that references those addresses.
* All in-flight `WDFDMATRANSACTION`s have been finalized/completed so WDF can tear down mappings/bounce buffers safely.

Recommended quiesce sequence:

1. Stop submitting new requests (fail or queue them in software).
2. Disable interrupts / notifications for the queue.
3. Reset the virtio device / virtqueue (transport-specific), so the device will not DMA further.
4. Drain/cancel all outstanding requests:
   * For each in-flight request that has an active DMA transaction:
     * Call `WdfDmaTransactionDmaCompletedFinal(Transaction, 0);`
     * Then `WdfDmaTransactionRelease(Transaction);` (or delete the transaction object if one-shot)
     * Complete the WDFREQUEST with an error status (`STATUS_CANCELLED`/`STATUS_DEVICE_NOT_CONNECTED`).
5. Only then delete/free the common buffers.

Pitfall: freeing the ring common buffer while the device is still running can produce memory corruption that looks like “random” crashes (the device continues DMA into freed pages).

---

## Common pitfalls checklist

* **Using CPU physical addresses instead of DMA-mapped (logical) addresses**
  * Wrong: `MmGetPhysicalAddress(buffer)`
  * Right: `WdfCommonBufferGetAlignedLogicalAddress()` for common buffers; `SCATTER_GATHER_LIST->Elements[i].Address` for MDL buffers.
* **Queue full interactions with DMA callbacks**
  * Don’t start (`WdfDmaTransactionExecute`) a transaction if you cannot guarantee a ring slot (or an indirect-table slot). Otherwise `EvtProgramDma` fires when the queue is full and you have to either fail or defer while holding DMA resources.
* **Descriptor-count limits**
  * If the device or driver only supports N segments, clamp WDF with `WdfDmaEnablerSetMaximumScatterGatherElements`.
  * Ensure indirect tables have enough entries for `header + N + status`.
* **Indirect table contiguity**
  * The indirect table must be reachable via a single descriptor `(addr,len)`. Allocate it from a common buffer (or an equivalent DMA-safe contiguous allocation).
* **32-bit limitations**
  * If forced into 32-bit DMA, you must rely on the DMA framework (WDF) to map/bounce into reachable addresses. Do not assume “all RAM <4GiB”.
* **Cache / coherency bugs**
  * Use uncached common buffers for rings/indirect tables.
  * Use `KeMemoryBarrier()` around ring index publication/consumption.
  * If doing PFN-based direct mapping, flush with `KeFlushIoBuffers`.
* **EVENT_IDX gotchas**
  * The extra `used_event/avail_event` fields change structure sizes. Make sure your offset/size formulas match the negotiated feature set.
