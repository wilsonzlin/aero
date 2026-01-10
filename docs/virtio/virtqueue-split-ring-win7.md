# Virtio 1.0 split virtqueues (split ring) — Windows 7 KMDF implementation guide

This document is a **design/reference guide** for implementing **Virtio 1.0 split virtqueues** in a **Windows 7 KMDF guest driver**, with specific attention to the features that matter for performance and correctness:

* `VIRTIO_F_RING_EVENT_IDX` (event index)
* `VIRTIO_F_RING_INDIRECT_DESC` (indirect descriptors)

It is written as “single source of truth” material you can use to implement queues for `virtio-input` first, and then reuse for `virtio-blk`, `virtio-net`, etc.

Project context (Aero): this guide is **transport-agnostic** and focuses only on virtqueue mechanics. If you are implementing drivers for Aero specifically, also consult the definitive device/feature contract:

* [`docs/windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md)

## Scope / non-scope

**In scope**

* Split ring virtqueues (Virtio 1.0 “split virtqueue”)
* Descriptor management (free list, cookies, out-of-order completion)
* Correct publish/consume ordering (barriers)
* Wraparound-safe index handling (`u16` modulo 65536)
* Event-idx notify + interrupt rearm algorithms
* Indirect descriptor table usage + a KMDF-friendly pool strategy

**Out of scope**

* Packed ring (`VIRTIO_F_RING_PACKED`) — not covered here
* Transport-specific wiring (PCI legacy vs PCI modern vs MMIO):
  * This doc is **transport-agnostic**.
  * It calls out **where** “notify” happens, but not **how** your transport performs it.
* Full feature negotiation, reset/bring-up, MSI-X, DPC/ISR plumbing — only referenced where it impacts virtqueue ordering.

## Terminology (Virtio spec + Linux `vring.h` names)

* **Descriptor table** (`struct vring_desc desc[QueueSize]`): describes DMA buffers.
* **Avail ring** (`struct vring_avail`): written by **driver**, consumed by **device**.
* **Used ring** (`struct vring_used`): written by **device**, consumed by **driver**.
* **Index values** (`avail->idx`, `used->idx`): monotonically increasing `u16` counters (wrap at 65536). Ring slots are addressed via `idx % QueueSize`.
* **Head descriptor index**: the first descriptor index of a chain; this is what goes in `avail->ring[]` and comes back as `used_elem.id`.

Virtio fields are defined as **little-endian** (`__le16`, `__le32`, `__le64`). Windows 7 x86/x64 is little-endian, so byte swaps are typically no-ops, but you should still treat the on-wire/in-memory format as little-endian.

### Windows-friendly note on “READ_ONCE/WRITE_ONCE” and endian helpers

This document uses a few Linux-style helper names in pseudocode because they precisely describe intent:

* `READ_ONCE(x)` / `WRITE_ONCE(x, v)` mean “perform exactly one load/store of this shared field and don’t let the compiler fold or duplicate it”.
  * In a Windows KMDF driver, this is commonly achieved with a `volatile` access to the ring field (plus the explicit `KeMemoryBarrier()` ordering points described later).
* `cpu_to_le16/32/64()` and `le16/32/64_to_cpu()` are endian conversion helpers.
  * On Windows 7 x86/x64 these are identity operations, but keeping them in code makes the virtio format explicit.

## 1) Split ring layout and field semantics

### 1.1 Descriptor table (`struct vring_desc`)

```c
/* Matches Linux include/uapi/linux/virtio_ring.h */
struct vring_desc {
    __le64 addr;   /* Guest-physical (DMA) address of buffer */
    __le32 len;    /* Length in bytes */
    __le16 flags;  /* VRING_DESC_F_* */
    __le16 next;   /* Next descriptor index if VRING_DESC_F_NEXT set */
};
```

**Field semantics**

* `addr`: DMA address the device will read/write. Must be device-accessible.
* `len`: size of the buffer at `addr`.
* `flags`:
  * `VRING_DESC_F_NEXT` (1): `next` is valid, chain continues.
  * `VRING_DESC_F_WRITE` (2): device **writes** into this buffer (driver reads).
  * `VRING_DESC_F_INDIRECT` (4): this descriptor points to an *indirect table* (see §8).
* `next`: next descriptor index in the chain (0..QueueSize-1), only valid when `NEXT` is set.

Notes:

* A “request” (TX) or “buffer” (RX) is typically represented as a **descriptor chain**:
  * request header (device reads, no `WRITE`)
  * payload buffers (read or write depending on direction)
  * status byte (device writes, `WRITE`)
* **Out-of-order completion is allowed**: the device may return used entries in any order.

### 1.2 Avail ring (`struct vring_avail`)

```c
struct vring_avail {
    __le16 flags;     /* VRING_AVAIL_F_* */
    __le16 idx;       /* driver increments when it adds entries */
    __le16 ring[];    /* QueueSize entries: head descriptor indices */
    /* __le16 used_event; // only if VIRTIO_F_RING_EVENT_IDX */
};
```

**Field semantics**

* `flags`:
  * `VRING_AVAIL_F_NO_INTERRUPT` (1): hint to device to suppress interrupts.
    * With `VIRTIO_F_RING_EVENT_IDX` negotiated, the driver should primarily use `used_event` (§7), not this flag.
* `idx`: driver-written producer index. Increments by number of new entries added.
* `ring[i]`: head descriptor index for entry `i` (where `i = avail_idx % QueueSize`).

### 1.3 Used ring (`struct vring_used` and `struct vring_used_elem`)

```c
struct vring_used_elem {
    __le32 id;   /* head descriptor index written by driver */
    __le32 len;  /* bytes written by device into WRITE buffers (often) */
};

struct vring_used {
    __le16 flags;          /* VRING_USED_F_* */
    __le16 idx;            /* device increments when it adds entries */
    struct vring_used_elem ring[]; /* QueueSize entries */
    /* __le16 avail_event; // only if VIRTIO_F_RING_EVENT_IDX */
};
```

**Field semantics**

* `used->flags`:
  * `VRING_USED_F_NO_NOTIFY` (1): hint to driver to suppress kicks (driver→device notify).
    * With `VIRTIO_F_RING_EVENT_IDX` negotiated, the driver should primarily use `avail_event` (§7), not this flag.
* `used->idx`: device-written producer index. Increments by number of completions posted.
* `used_elem.id`: the head descriptor index originally placed in `avail->ring[]`.
* `used_elem.len`: depends on device type; commonly:
  * For RX-style buffers (device writes), it’s the number of bytes written.
  * For TX-style buffers (device reads), it may be 0 or the number of bytes consumed (device-specific).

## 2) Ring memory layout, sizing, and alignment

Split rings are three logical regions:

```
<base>
  +-------------------------------+
  | vring_desc desc[QueueSize]    | 16 * QueueSize bytes (16B aligned)
  +-------------------------------+
  | vring_avail                   | 2B aligned
  |   flags (u16), idx (u16)      |
  |   ring[QueueSize] (u16[])     |
  |   used_event (u16) [event-idx]|
  +-------------------------------+
  | padding to align used ring    |
  +-------------------------------+
  | vring_used                    | 4B aligned (legacy: QueueAlign)
  |   flags (u16), idx (u16)      |
  |   ring[QueueSize] (used_elem) |
  |   avail_event (u16) [event-idx]|
  +-------------------------------+
```

### 2.1 Alignment rules (what must be true)

* `vring_desc` entries are 16 bytes; the descriptor table should be **16-byte aligned**.
* `vring_avail` must be at least **2-byte aligned** (it contains `u16` elements).
* `vring_used` must be at least **4-byte aligned** (it contains `u32` elements).
* **Legacy virtio (PCI legacy / transitional)**: the **start of the used ring** must be aligned to `QueueAlign` (usually 4096).
* Recommendation for Windows: allocate a **page-aligned common buffer** and align sub-structures conservatively; this avoids transport quirks and makes debugging easier.

### 2.2 Layout calculation (contiguous allocation)

Many implementations allocate the whole ring as one contiguous DMA buffer and compute offsets similarly to Linux `vring_size()` / `vring_init()`.

Let:

* `N = QueueSize`
* `ALIGN = QueueAlign` (legacy) or a chosen alignment for used ring (modern can use 4 or page size)
* `HAS_EVENT_IDX = negotiated(VIRTIO_F_RING_EVENT_IDX)`

Sizes:

* `desc_bytes = 16 * N`
* `avail_bytes = 4 + 2*N + (HAS_EVENT_IDX ? 2 : 0)`
  * (`flags`+`idx` = 4, plus `ring[N]`, plus optional `used_event`)
* `used_bytes = 4 + 8*N + (HAS_EVENT_IDX ? 2 : 0)`
  * (`flags`+`idx` = 4, plus `used_elem[N]`, plus optional `avail_event`)

Offsets:

* `desc_off = 0`
* `avail_off = desc_off + desc_bytes`
* `used_off  = align_up(avail_off + avail_bytes, ALIGN)`
* `total_bytes = used_off + used_bytes`

`align_up(x, a) = (x + (a-1)) & ~(a-1)` for power-of-two alignments.

**Important**: In **PCI legacy**, the device derives `used_off` using `QueueAlign`. In **PCI modern**, you program `queue_desc`, `queue_avail`, and `queue_used` separately, so you may pick any safe offsets (still obeying the per-structure alignment rules).

### 2.3 Windows KMDF allocation recommendation

Use a DMA common buffer so the ring is device-accessible and nonpaged:

* `WdfCommonBufferCreate(...)` for the ring (and optionally for an indirect table pool, §8.3).
* Use `WdfCommonBufferGetAlignedVirtualAddress()` for the CPU VA.
* Use `WdfCommonBufferGetAlignedLogicalAddress()` (or the DMA enabler’s logical address) for the device-visible address used in descriptors or queue config.
* **Initialize the ring memory** before enabling the queue:
  * easiest: zero the whole allocated ring buffer
  * at minimum: set `avail->flags/idx` and `used->flags/idx` to 0 so the first `last_used_idx` starts from a known value.

Even when the transport supports non-contiguous memory via IOMMU, a single common buffer keeps the implementation simple and Win7-friendly.

## 3) Driver-side virtqueue state (what to track)

In addition to the shared ring structures, the driver maintains private state. A typical per-virtqueue state (names align with Linux where helpful):

* `free_head` (`u16`): head of the free descriptor list.
* `num_free` (`u16`): number of free descriptors available.
* `avail_idx` (`u16`): driver shadow of `avail->idx` (next slot index to fill).
* `last_used_idx` (`u16`): driver shadow of used consumption point (next used entry to process).
* `num_added` (`u16` or `u32`): how many entries were added since the last notify decision.
* `cookie[QueueSize]`: per-head context pointer (e.g., WDFREQUEST, buffer object, or request tag).
  * Only valid for head indices currently “in flight”.
  * Required because `used_elem.id` returns only the head index, and completions may be out-of-order.

### 3.1 Required invariants

Keep these invariants true at all times (encode them in asserts in your driver if possible):

* `num_free <= QueueSize`
* Outstanding descriptors `QueueSize - num_free` must not exceed `QueueSize`.
* Each in-flight head descriptor index has a valid `cookie[head] != NULL`.
* `avail_idx - last_used_idx` (as `u16`) must never exceed `QueueSize`.
  * Practically, this is enforced by descriptor accounting (`num_free`) and not over-posting.

## 4) Descriptor allocation/free: free list + cookie mapping

### 4.1 Free list representation

Use `desc[i].next` as the free-list pointer when a descriptor is free.

Initialization:

* For `i = 0..N-2`: `desc[i].next = i + 1`
* For `i = N-1`: `desc[i].next = 0xFFFF` (sentinel)
* `free_head = 0`
* `num_free = N`

Even though the device never follows “free” descriptors, keep indices stored in little-endian form for consistency (`cpu_to_le16()` / `le16_to_cpu()`), especially if you later reuse the same accessors for in-flight chains.

### 4.2 Allocate a chain (typical algorithm)

To allocate `k` descriptors for a request/buffer:

1. Check `num_free >= k`. If not, fail (queue full).
2. Pop `k` descriptors from the free list (`free_head`), remember them in a temporary list.
3. Fill each descriptor’s `addr`, `len`, `flags`.
4. Link them:
   * For all but the last: set `VRING_DESC_F_NEXT`, set `next` to the next index.
   * For the last: clear `VRING_DESC_F_NEXT`.
5. The first allocated index is the **head**:
   * Store `cookie[head] = <context>` before publishing to `avail`.

Failure path: if you partially allocate and then fail to build (e.g., DMA mapping fails), walk the temporary list and push descriptors back to the free list (restoring `num_free`).

### 4.3 Free a chain (completion path)

When consuming a `used_elem`:

1. Read `head = used_elem.id` (mask to `u16`).
2. Lookup `ctx = cookie[head]`; clear `cookie[head] = NULL`.
3. Walk the descriptor chain starting from `head`:
   * Record `next` before overwriting.
   * Push each descriptor back onto the free list by writing its `next = free_head`, updating `free_head`, incrementing `num_free`.
   * Stop when `VRING_DESC_F_NEXT` is not set.
4. Complete the request / recycle the RX buffer using `ctx`.

**Out-of-order completion**: do not assume `head` values return in the same order you posted them.

## 5) Publishing to the avail ring (driver → device)

The ordering here is the most common source of virtqueue bugs. The device must not observe an `avail->idx` increment before it can safely read:

* the `avail->ring[]` slot you wrote, and
* the descriptors and buffers reachable from that head index.

### 5.1 Publish algorithm (single entry)

Assume `slot = avail_idx % QueueSize` (or `avail_idx & (QueueSize-1)` if `QueueSize` is power-of-two).

1. **Write descriptor(s)** (`desc[head]`, chain, and any data the device will read).
2. Write `avail->ring[slot] = head`.
3. **wmb()**: ensure descriptors + ring slot are visible before idx update.
4. Increment `avail_idx` (wrap in `u16`).
5. Write `avail->idx = avail_idx`.
6. `num_added++`.
7. Optionally **wmb()** before the transport “notify” register write (some transports/bridges benefit).

On Windows, there isn’t a distinct “wmb” primitive; use a full barrier:

* `KeMemoryBarrier()` or `MemoryBarrier()`

### 5.2 Notify decision (when to “kick”)

After one or more additions (often batched), decide whether to notify the device:

* Without `VIRTIO_F_RING_EVENT_IDX`: honor `used->flags & VRING_USED_F_NO_NOTIFY`.
* With `VIRTIO_F_RING_EVENT_IDX`: use `used->avail_event` and `vring_need_event()` (§7.2).

**Where notify happens**: transport-specific. Examples:

* PCI legacy: write queue number to I/O port `VIRTIO_PCI_QUEUE_NOTIFY`
* PCI modern: write queue number to MMIO `notify_off + queue_notify_off`
* MMIO transport: write to `QueueNotify` register

This doc only defines *when* the notify should occur.

## 6) Consuming the used ring (device → driver)

To consume completions safely, you must ensure that after observing `used->idx` has advanced, subsequent reads see:

* the corresponding `used_elem` entries, and
* any DMA writes the device performed into `VRING_DESC_F_WRITE` buffers.

### 6.1 Consume algorithm

1. Read `used_idx = used->idx` (as `u16`).
2. If `used_idx == last_used_idx`: nothing new, exit.
3. **rmb()**: ensure used elements + buffer contents are visible after seeing `used_idx`.
4. Loop while `last_used_idx != used_idx`:
   * `slot = last_used_idx % QueueSize`
   * `e = used->ring[slot]`
   * `head = (u16)e.id`
   * `len = e.len`
   * Lookup cookie/context for `head`
   * Process completion / RX data
   * Free descriptor chain starting at `head`
   * `last_used_idx++` (wrap in `u16`)

Again, on Windows use `KeMemoryBarrier()` / `MemoryBarrier()` as the rmb.

### 6.2 Wraparound-safe loop patterns (`u16` modulo 65536)

Treat indices as `UINT16` and let unsigned wrap do the work:

```c
UINT16 used_idx = le16_to_cpu(READ_ONCE(vq->used->idx));
if (used_idx != vq->last_used_idx) {
    KeMemoryBarrier(); /* rmb */
    while (vq->last_used_idx != used_idx) {
        UINT16 slot = vq->last_used_idx & (vq->qsz - 1); /* if power-of-two */
        struct vring_used_elem e = vq->used->ring[slot];
        handle_used(e);
        vq->last_used_idx++;
    }
}
```

If you cannot rely on power-of-two queue sizes, use `% vq->qsz` instead of `& (vq->qsz - 1)`.

Also useful: compute “how many new used entries” in a wrap-safe way:

```c
UINT16 pending = (UINT16)(used_idx - vq->last_used_idx);
/* pending is 0..65535, but should never exceed QueueSize */
```

## 7) `VIRTIO_F_RING_EVENT_IDX` (event index)

`VIRTIO_F_RING_EVENT_IDX` replaces the coarse “NO_NOTIFY/NO_INTERRUPT” hints with explicit indices to reduce notification overhead and prevent lost wakeups.

* Virtio 1.0 feature bit: **29**

### 7.1 Extra fields in ring layout

When negotiated, two extra `u16` fields are defined:

* `avail->used_event` (driver-written): “interrupt me when `used->idx` reaches this”
* `used->avail_event` (device-written): “kick me when `avail->idx` reaches this”

Layout positions (matching Linux helpers):

```c
/* Pointer to avail->used_event */
__le16 *vring_used_event(struct vring_avail *avail, UINT16 qsz) {
    return &avail->ring[qsz];
}

/* Pointer to used->avail_event */
__le16 *vring_avail_event(struct vring_used *used, UINT16 qsz) {
    return (__le16 *)&used->ring[qsz];
}
```

### 7.2 `vring_need_event(event, new, old)` (wrap-safe)

Use the Linux/virtio-standard formula:

```c
static __forceinline BOOLEAN vring_need_event(UINT16 event, UINT16 new, UINT16 old)
{
    return (UINT16)(new - event - 1) < (UINT16)(new - old);
}
```

All values are `u16` and wrap naturally.

Interpretation:

* `old` is the previous producer index value.
* `new` is the updated producer index value (after adding/completing entries).
* `event` is the peer-provided threshold index.
* The function returns true if crossing from `old` to `new` should trigger a notification.

### 7.3 Driver → device notify decision (kick) using `used->avail_event`

When the driver adds entries:

* `old = avail_idx` before additions
* `new = avail_idx` after additions (and after writing `avail->idx`)
* `event = used->avail_event` (device-written)

Notify if:

* `vring_need_event(event, new, old)` is true

Typical batching pattern:

```c
UINT16 old = vq->avail_idx;
add_buffers(...);            /* updates vq->avail_idx, vq->avail->idx, vq->num_added */
UINT16 new = vq->avail_idx;

if (vq->num_added) {
    BOOLEAN kick;
    if (vq->features & VIRTIO_F_RING_EVENT_IDX) {
        UINT16 event = le16_to_cpu(READ_ONCE(*vring_avail_event(vq->used, vq->qsz)));
        kick = vring_need_event(event, new, old);
    } else {
        kick = !(le16_to_cpu(READ_ONCE(vq->used->flags)) & VRING_USED_F_NO_NOTIFY);
    }

    vq->num_added = 0;
    if (kick) {
        KeMemoryBarrier(); /* optional wmb before notify */
        transport_notify_queue(vq);
    }
}
```

### 7.4 Driver interrupt suppression/enable using `avail->used_event`

With event-idx, the driver should control interrupt timing by writing `avail->used_event`.

#### Disable interrupts (while processing)

To request “don’t interrupt me for the work I’ve already seen”, set:

* `avail->used_event = last_used_idx - 1`

This is the standard Linux pattern: it pushes the event threshold “behind” the current consumption point, making it unlikely the device will decide an interrupt is needed while you are draining the ring in a tight loop.

#### Enable interrupts (rearm) + race avoidance pattern

The classic lost-interrupt race is:

1. Driver drains used ring.
2. Driver enables interrupts.
3. Device posts a used entry *between* the driver’s last check and the enable, so no interrupt is generated and the driver goes to sleep.

Avoid this by:

1. **Enable**: `avail->used_event = last_used_idx`
2. **Barrier**
3. **Re-check** `used->idx` (if advanced, drain again instead of sleeping)

Concrete pattern:

```c
for (;;) {
    /* Drain used ring */
    UINT16 used_idx = le16_to_cpu(READ_ONCE(vq->used->idx));
    if (used_idx != vq->last_used_idx) {
        KeMemoryBarrier(); /* rmb: see used elems + DMA writes */
        while (vq->last_used_idx != used_idx) {
            process_one_used(vq);
            vq->last_used_idx++;
        }
    }

    /* Rearm interrupts for “next completion” */
    WRITE_ONCE(*vring_used_event(vq->avail, vq->qsz), cpu_to_le16(vq->last_used_idx));
    KeMemoryBarrier(); /* wmb: ensure used_event visible before re-check */

    /* Race check: did device add more used entries after we drained? */
    if (le16_to_cpu(READ_ONCE(vq->used->idx)) == vq->last_used_idx) {
        break; /* safe to return; next completion should interrupt */
    }

    /* Otherwise, loop and drain again */
}
```

## 8) `VIRTIO_F_RING_INDIRECT_DESC` (indirect descriptors)

Indirect descriptors let you represent a request with an arbitrary-length scatter/gather list while consuming only **one** descriptor in the main descriptor table.

* Virtio 1.0 feature bit: **28**

### 8.1 How indirect works

Instead of a normal chain in the main `desc[]`, the driver posts a single descriptor:

* `desc[head].flags` includes `VRING_DESC_F_INDIRECT`
* `desc[head].addr` points to an **indirect table** in DMA memory
* `desc[head].len` is the size in bytes of that indirect table (`16 * k` for `k` entries)
* `desc[head].next` is ignored (do not set `VRING_DESC_F_NEXT` on the head)

The indirect table is an array of `struct vring_desc` entries. The device interprets it exactly like a normal descriptor chain, except indices are relative to the indirect table (0..k-1).

### 8.2 Requirements / constraints (must enforce)

* Feature must be negotiated: `VIRTIO_F_RING_INDIRECT_DESC`
* The indirect table must be:
  * **DMA-accessible** by the device
  * **16-byte aligned**
  * `len` must be a multiple of 16
* **No nested indirect**:
  * Indirect table entries must not set `VRING_DESC_F_INDIRECT`.
* The indirect table’s `next` values refer to entries *within the indirect table*, not the main ring.

### 8.3 KMDF-friendly allocation strategy (pool in a common buffer)

For Windows 7 KMDF, a practical approach is to preallocate a fixed pool of indirect tables in a `WdfCommonBuffer`:

* One common buffer for:
  * the virtqueue ring memory, and
  * `N_tables` indirect tables of fixed maximum length (`k_max` descriptors each)
* Maintain a free list of table indices (similar to descriptor free list).
* Associate the allocated indirect table with the **head descriptor** via the cookie/state:
  * `cookie[head]` can point to a struct that includes `indirect_table_index` (or the VA/PA).
* On completion:
  * free the main head descriptor index back to the main descriptor free list
  * return the indirect table to the pool

This avoids dynamic allocation at DISPATCH_LEVEL and guarantees DMA-accessible, aligned memory.

### 8.4 Minimal indirect descriptor example (pseudocode)

Assume you want to submit a request with `k` scatter/gather segments, but only spend **one** main-ring descriptor:

```c
/* Allocate one main-ring head descriptor (as usual) */
UINT16 head = VqAllocDesc(vq);

/* Allocate an indirect table from a preallocated common-buffer-backed pool */
INDIRECT_TABLE *t = IndirectPoolAlloc(vq->indirect_pool);
struct vring_desc *it = t->va;          /* CPU VA to table */
UINT64 it_pa = t->pa.QuadPart;          /* DMA address of table */

/* Fill k indirect descriptors */
for (UINT16 i = 0; i < k; i++) {
    UINT16 flags = segments[i].device_writes ? VRING_DESC_F_WRITE : 0;
    if (i != k - 1) {
        flags |= VRING_DESC_F_NEXT;
    }
    it[i].addr  = cpu_to_le64(segments[i].pa);
    it[i].len   = cpu_to_le32(segments[i].len);
    it[i].flags = cpu_to_le16(flags);
    it[i].next  = cpu_to_le16((i != k - 1) ? (i + 1) : 0);
}
/* Last entry must not have NEXT set (and next is ignored). */

/* Publish the indirect table via the single main descriptor */
vq->desc[head].addr  = cpu_to_le64(it_pa);
vq->desc[head].len   = cpu_to_le32((UINT32)(k * sizeof(struct vring_desc)));
vq->desc[head].flags = cpu_to_le16(VRING_DESC_F_INDIRECT);

/* Track the indirect table in the per-head cookie/state so it can be freed on completion */
vq->cookie[head] = MakeCookie(ctx, t);

/* Then publish head into avail ring as usual (§5) */
```

Key points:

* The indirect table must be DMA-accessible and 16-byte aligned.
* Do not set `VRING_DESC_F_INDIRECT` on any **indirect table entry** (no nesting).
* Free the indirect table back to the pool when `used_elem.id == head` completes.

## 9) Windows 7 / KMDF specifics (practical constraints)

### 9.1 DMA memory and “common buffers”

* The ring (and indirect tables, if used) must live in device-accessible DMA memory.
* `WdfCommonBufferCreate` is the simplest Win7-compatible mechanism.
* Ensure the common buffer is **nonpaged** and accessible at DISPATCH_LEVEL.

### 9.2 Memory barriers (still required on x86)

Even on x86/x64, you must insert barriers exactly as the Virtio spec requires:

* The **compiler** can reorder ordinary memory operations.
* The **device** may observe memory through DMA in ways that do not match the CPU’s program order (posted writes, bridges, virtualization layers).
* The virtqueue algorithm is defined in terms of explicit ordering points.

Use:

* `KeMemoryBarrier()` or `MemoryBarrier()` as a full barrier for both wmb/rmb points in this doc.

### 9.3 IRQL and locking

Virtqueue operations commonly occur in:

* ISR (minimal work; typically just acknowledges and schedules a DPC)
* DPC at **DISPATCH_LEVEL** (draining `used`, posting more buffers, rearming)

Guidelines:

* Protect the virtqueue state (`free_head`, `num_free`, `avail_idx`, `last_used_idx`, cookies, indirect pool) with a **spin lock**:
  * `WdfSpinLockAcquire/Release`, or `KeAcquireSpinLockAtDpcLevel` if already at DISPATCH_LEVEL.
* Do not allocate pageable memory or call pageable routines at DISPATCH_LEVEL.
  * Preallocate RX buffers and indirect tables up front (lookaside lists or common buffers).

## 10) End-to-end pseudocode example (virtio-input style RX queue)

This example shows a typical “device writes events into driver-provided buffers” virtqueue. It includes:

* init
* posting RX buffers
* notify decision
* interrupt/DPC draining used entries
* event-idx interrupt rearm race avoidance

> Notes:
> * This is pseudocode; adapt the DMA mapping and transport notify to your device/stack.
> * `QueueSize` is assumed to be a power of two for `& (QueueSize-1)` masking.

```c
typedef struct _VQ {
    UINT16 qsz;
    struct vring_desc  *desc;
    struct vring_avail *avail;
    struct vring_used  *used;
    __le16 *used_event;   /* &avail->ring[qsz] if EVENT_IDX */
    __le16 *avail_event;  /* (u16*)&used->ring[qsz] if EVENT_IDX */

    UINT16 free_head;
    UINT16 num_free;

    UINT16 avail_idx;      /* shadow producer */
    UINT16 last_used_idx;  /* shadow consumer */
    UINT16 num_added;

    VOID *cookie[MAX_QSZ]; /* per head index */

    WDFSPINLOCK lock;
    UINT64 features;
} VQ;

static __forceinline SIZE_T align_up(SIZE_T x, SIZE_T a)
{
    return (x + (a - 1)) & ~(a - 1);
}

VOID VirtQueueInit(VQ *vq, VOID *ring_va, UINT16 qsz, UINT64 features, SIZE_T queue_align)
{
    vq->qsz = qsz;
    vq->features = features;

    /* Compute desc/avail/used pointers using §2 formulas for your transport. */
    SIZE_T desc_off  = 0;
    SIZE_T avail_off = desc_off + (SIZE_T)16 * qsz;
    SIZE_T avail_len = 4 + (SIZE_T)2 * qsz + ((features & VIRTIO_F_RING_EVENT_IDX) ? 2 : 0);
    SIZE_T used_off  = align_up(avail_off + avail_len, queue_align /* legacy QueueAlign (often 4096) */);

    vq->desc  = (struct vring_desc *)((PUCHAR)ring_va + desc_off);
    vq->avail = (struct vring_avail *)((PUCHAR)ring_va + avail_off);
    vq->used  = (struct vring_used  *)((PUCHAR)ring_va + used_off);

    if (features & VIRTIO_F_RING_EVENT_IDX) {
        vq->used_event  = &vq->avail->ring[qsz];
        vq->avail_event = (__le16 *)&vq->used->ring[qsz];
    }

    /* Init free list */
    for (UINT16 i = 0; i < qsz - 1; i++) {
        vq->desc[i].next = cpu_to_le16(i + 1);
    }
    vq->desc[qsz - 1].next = cpu_to_le16(0xFFFF);
    vq->free_head = 0;
    vq->num_free = qsz;

    vq->avail_idx = 0;
    vq->last_used_idx = 0;
    vq->num_added = 0;

    /* Clear rings visible to device */
    vq->avail->flags = cpu_to_le16(0);
    vq->avail->idx   = cpu_to_le16(0);
    vq->used->flags  = cpu_to_le16(0);
    vq->used->idx    = cpu_to_le16(0);

    if (features & VIRTIO_F_RING_EVENT_IDX) {
        /* Start with interrupts enabled for first completion */
        WRITE_ONCE(*vq->used_event, cpu_to_le16(0));
    }

    for (UINT16 i = 0; i < qsz; i++) {
        vq->cookie[i] = NULL;
    }
}

static UINT16 VqAllocDesc(VQ *vq)
{
    if (vq->num_free == 0) return 0xFFFF;
    UINT16 idx = vq->free_head;
    vq->free_head = le16_to_cpu(vq->desc[idx].next);
    vq->num_free--;
    return idx;
}

static VOID VqFreeChain(VQ *vq, UINT16 head)
{
    UINT16 i = head;
    for (;;) {
        UINT16 flags = le16_to_cpu(vq->desc[i].flags);
        UINT16 next  = le16_to_cpu(vq->desc[i].next);

        /* push back to free list */
        vq->desc[i].next = cpu_to_le16(vq->free_head);
        vq->free_head = i;
        vq->num_free++;

        if (!(flags & VRING_DESC_F_NEXT))
            break;
        i = next;
    }
}

static VOID VqAddRxBuffer(VQ *vq, PHYSICAL_ADDRESS buf_pa, UINT32 buf_len, VOID *ctx)
{
    UINT16 head = VqAllocDesc(vq);
    if (head == 0xFFFF) return; /* queue full */

    /* Single WRITE descriptor (device writes event bytes) */
    vq->desc[head].addr  = cpu_to_le64(buf_pa.QuadPart);
    vq->desc[head].len   = cpu_to_le32(buf_len);
    vq->desc[head].flags = cpu_to_le16(VRING_DESC_F_WRITE);

    vq->cookie[head] = ctx;

    UINT16 slot = vq->avail_idx & (vq->qsz - 1);
    vq->avail->ring[slot] = cpu_to_le16(head);

    KeMemoryBarrier(); /* wmb: desc + ring slot visible before idx */
    vq->avail_idx++;
    WRITE_ONCE(vq->avail->idx, cpu_to_le16(vq->avail_idx));

    vq->num_added++;
}

static BOOLEAN VqKickIfNeeded(VQ *vq, UINT16 old_avail)
{
    if (!vq->num_added) return FALSE;

    UINT16 new_avail = vq->avail_idx;
    BOOLEAN kick;

    if (vq->features & VIRTIO_F_RING_EVENT_IDX) {
        UINT16 event = le16_to_cpu(READ_ONCE(*vq->avail_event));
        kick = vring_need_event(event, new_avail, old_avail);
    } else {
        kick = !(le16_to_cpu(READ_ONCE(vq->used->flags)) & VRING_USED_F_NO_NOTIFY);
    }

    vq->num_added = 0;
    if (kick) {
        KeMemoryBarrier(); /* optional wmb before notify MMIO */
        transport_notify_queue(vq);
    }
    return kick;
}

/* Called during init or when refilling RX buffers */
VOID VirtQueuePostInitialRx(VQ *vq, RX_BUF_POOL *pool, UINT32 count)
{
    WdfSpinLockAcquire(vq->lock);

    UINT16 old_avail = vq->avail_idx;
    for (UINT32 i = 0; i < count; i++) {
        RX_BUF *b = PoolGet(pool);
        VqAddRxBuffer(vq, b->pa, b->len, b);
    }

    VqKickIfNeeded(vq, old_avail);
    WdfSpinLockRelease(vq->lock);
}

/* Interrupt/DPC path: drain used ring and rearm (event-idx safe) */
VOID VirtQueueDpc(VQ *vq)
{
    WdfSpinLockAcquire(vq->lock);

    if (vq->features & VIRTIO_F_RING_EVENT_IDX) {
        /* suppress interrupts while draining */
        WRITE_ONCE(*vq->used_event, cpu_to_le16((UINT16)(vq->last_used_idx - 1)));
        KeMemoryBarrier();
    }

    for (;;) {
        UINT16 used_idx = le16_to_cpu(READ_ONCE(vq->used->idx));
        if (used_idx == vq->last_used_idx)
            break;

        KeMemoryBarrier(); /* rmb: see used elems + DMA writes */

        while (vq->last_used_idx != used_idx) {
            UINT16 slot = vq->last_used_idx & (vq->qsz - 1);
            struct vring_used_elem e = vq->used->ring[slot];
            UINT16 head = (UINT16)le32_to_cpu(e.id);
            UINT32 len  = le32_to_cpu(e.len);

            RX_BUF *b = (RX_BUF *)vq->cookie[head];
            vq->cookie[head] = NULL;

            /* Consume data written by device (len bytes) */
            HandleInputEventBytes(b->va, len);

            /* Recycle: free desc chain, then re-post buffer */
            VqFreeChain(vq, head);
            PoolPutBackOrRepost(vq, b);

            vq->last_used_idx++;
        }
    }

    if (vq->features & VIRTIO_F_RING_EVENT_IDX) {
        /* rearm interrupts + race check */
        for (;;) {
            WRITE_ONCE(*vq->used_event, cpu_to_le16(vq->last_used_idx));
            KeMemoryBarrier();
            if (le16_to_cpu(READ_ONCE(vq->used->idx)) == vq->last_used_idx)
                break;

            /* device posted more while rearming; drain again */
            UINT16 used_idx = le16_to_cpu(READ_ONCE(vq->used->idx));
            KeMemoryBarrier();
            while (vq->last_used_idx != used_idx) {
                process_one_used_and_repost(vq);
                vq->last_used_idx++;
            }
        }
    }

    WdfSpinLockRelease(vq->lock);
}
```
