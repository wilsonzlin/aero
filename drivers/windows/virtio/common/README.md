# Windows virtio common (split virtqueue)

This directory contains a small, reusable **Virtio 1.0 split virtqueue** implementation intended for **Windows 7 KMDF** guest drivers.

See also:

- [`docs/virtio/virtqueue-split-ring-win7.md`](../../../../docs/virtio/virtqueue-split-ring-win7.md) — split-ring virtqueue implementation guide (algorithms, ordering/barriers, EVENT_IDX, indirect).
- [`docs/windows7-virtio-driver-contract.md`](../../../../docs/windows7-virtio-driver-contract.md) — Aero’s definitive virtio device/feature/transport contract.

Highlights:

- Split ring only (descriptor table + avail ring + used ring)
- Optional support for:
  - `VIRTIO_F_RING_EVENT_IDX`
  - `VIRTIO_F_RING_INDIRECT_DESC` (requires a caller-provided indirect table pool)
- No WDF dependencies in the core virtqueue module (it uses primitive WDK types like `UINT16/UINT32/UINT64/BOOLEAN` only)
- Safe to call at `<= DISPATCH_LEVEL` (no allocations; callers provide all memory)

## Files

- `virtio_ring.h` – spec-accurate vring structs/constants for split rings.
  - Uses `ring[1]` + helpers (instead of C99 flexible arrays) for WDK 7.1 compatibility.
- `virtio_osdep.h` – minimal portability layer:
  - barriers (`VIRTIO_MB/RMB/WMB`)
  - volatile read/write helpers (`VirtioReadU16`, …)
  - `VIRTIO_ALIGN_UP` and `VirtioZeroMemory`
- `virtio_sg_pfn.h/.c` – WDF-free scatter/gather builder:
  - `VirtioSgBuildFromPfns()` converts PFN lists into `VIRTQ_SG[]` (coalescing contiguous PFNs).
  - Kernel-mode wrappers build `VIRTQ_SG[]` from an MDL chain without allocations (DISPATCH_LEVEL safe).
- `virtqueue_split.h/.c` – the split virtqueue engine.

## Basic usage pattern

### 1) Allocate queue state

`VIRTQ_SPLIT` has trailing per-descriptor metadata storage, so allocate using:

```c
size_t bytes = VirtqSplitStateSize(qsz);
VIRTQ_SPLIT* vq = ExAllocatePoolWithTag(NonPagedPool, bytes, 'qriV');
```

### 2) Allocate ring memory (DMA common buffer)

For a contiguous ring allocation (legacy-friendly layout):

```c
size_t ring_bytes = VirtqSplitRingMemSize(qsz, ring_align, event_idx);
// Allocate ring_bytes in DMA-visible memory and obtain:
//   ring_va (CPU VA) and ring_pa (device DMA address)
```

Notes:

- `ring_va`/`ring_pa` must be **16-byte aligned** (descriptor table alignment).
- `ring_align` must be a power-of-two and **>= 4** (used ring contains 32-bit fields).

### 3) (Optional) Allocate an indirect descriptor table pool

If `VIRTIO_F_RING_INDIRECT_DESC` is negotiated, callers may supply a pool:

```c
size_t pool_bytes = table_count * indirect_max_desc * sizeof(VIRTQ_DESC);
// Allocate pool_bytes in DMA-visible memory and obtain:
//   indirect_pool_va and indirect_pool_pa
```

### 4) Initialize

```c
NTSTATUS status = VirtqSplitInit(vq, qsz,
                                event_idx,
                                indirect_desc,
                                ring_va, ring_pa, ring_align,
                                indirect_pool_va, indirect_pool_pa,
                                table_count, indirect_max_desc);
```

### 5) Submit buffers + (maybe) kick

If your payload is MDL-backed, use `virtio_sg_pfn.h` to build a `VIRTQ_SG[]` first:

```c
ULONG max_sg = VirtioSgMaxElemsForMdl(Mdl, ByteOffset, ByteLength);
VIRTQ_SG sg[32];
UINT16 sg_count = 0;

NTSTATUS status;
if (max_sg > RTL_NUMBER_OF(sg)) {
  // Either allocate a larger array from nonpaged pool, prefer INDIRECT_DESC, or fail.
  return STATUS_BUFFER_TOO_SMALL;
}

status = VirtioSgBuildFromMdl(Mdl, ByteOffset, ByteLength,
                                      /*device_write=*/TRUE,
                                      sg, (UINT16)RTL_NUMBER_OF(sg),
                                      &sg_count);
```

```c
UINT16 head;
status = VirtqSplitAddBuffer(vq, sg, sg_count, cookie, &head);
VirtqSplitPublish(vq, head);

if (VirtqSplitKickPrepare(vq)) {
  // transport-specific notify register write goes here
}
VirtqSplitKickCommit(vq);
```

### 6) Consume used buffers

```c
while (VirtqSplitHasUsed(vq)) {
  void* cookie;
  UINT32 len;
  if (NT_SUCCESS(VirtqSplitGetUsed(vq, &cookie, &len))) {
    // handle completion
  }
}
```

### 7) Interrupt suppression

Use these helpers around “arm interrupts + sleep” patterns:

```c
VirtqSplitDisableInterrupts(vq);
// ... drain used ring ...
if (VirtqSplitEnableInterrupts(vq)) {
  // safe to sleep: no pending used entries after enabling
} else {
  // work is pending; poll/drain again
}
```

## Tests

User-mode simulation tests live in `tests/`.

### CMake / ctest (same as CI)

From the repository root:

```sh
cmake -S . -B build-virtio-tests -DAERO_VIRTIO_BUILD_TESTS=ON -DAERO_AEROGPU_BUILD_TESTS=OFF
cmake --build build-virtio-tests --config Release
ctest --test-dir build-virtio-tests --output-on-failure -C Release
```

### GNU Make (manual)

```sh
cd drivers/windows/virtio/common/tests
make test
```
