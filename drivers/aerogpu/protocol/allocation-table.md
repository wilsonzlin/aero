# AeroGPU per-submit allocation table + `backing_alloc_id` contract

This document is the **source of truth** for how AeroGPU command packets reference **guest-backed memory** via `backing_alloc_id`, and how the host resolves those IDs through the **per-submit allocation table**.

This contract is used by:

- Win7 UMDs when emitting `CREATE_BUFFER` / `CREATE_TEXTURE2D` packets (`aerogpu_cmd.h`),
- the Win7 KMD when building the per-submit allocation table (`aerogpu_ring.h`), and
- the emulator/host when validating and executing submissions.

## Problem statement

AeroGPU packets can reference guest physical memory via a compact `alloc_id` (`backing_alloc_id`).

The host cannot treat a guest physical address as stable for the lifetime of a WDDM allocation:
WDDM can remap/move allocations between submits. Therefore, every submission that might require
guest-memory access must supply a **per-submit table** mapping:

```
alloc_id -> (gpa, size_bytes, flags)
```

## `alloc_id` namespaces and stability

`alloc_id` is a **stable identifier for a WDDM allocation**, not a per-submit index.

The namespace split is defined in `aerogpu_wddm_alloc.h`:

- `0` is reserved/invalid.
- `1..0x7fffffff` (`AEROGPU_WDDM_ALLOC_ID_UMD_MAX`): **UMD-owned** IDs.
  - Must be stable for the lifetime of the underlying WDDM allocation.
  - Must be collision-resistant across guest processes (DWM may batch surfaces from many processes
    in one submission).
  - Shared allocations must persist `alloc_id` in the WDDM allocation private-data blob so another
    process can recover it on `OpenResource`.
- `0x80000000..0xffffffff` (`AEROGPU_WDDM_ALLOC_ID_KMD_MIN`): **KMD-reserved** IDs.
  - May be used for runtime/kernel allocations created without an AeroGPU private-data blob.

### Aliasing (multiple handles, same `alloc_id`)

Windows may create multiple WDDM allocation handles that refer to the **same underlying
allocation** (e.g. CreateAllocation vs OpenAllocation for a shared resource).

All aliases **must share the same `alloc_id`**. The host’s allocation lookup is keyed by `alloc_id`,
not by WDDM handle value.

### Collision rules

Within a single submission, the allocation table is a map keyed by `alloc_id`. Therefore:

- The KMD must **deduplicate** identical aliases safely.
- If the same `alloc_id` would map to **different** `(gpa, size)` in a single submit, it is a
  collision and the submission must fail.

## Allocation table format (`aerogpu_ring.h`)

The submit descriptor (`aerogpu_submit_desc`) optionally points to an allocation table:

```c
uint64_t alloc_table_gpa;        /* 0 if not present */
uint32_t alloc_table_size_bytes; /* 0 if not present */
```

If present, the table bytes are:

```
[aerogpu_alloc_table_header]
[aerogpu_alloc_entry entries...]
```

`aerogpu_alloc_table_header::size_bytes` is the total size including the header and all entries.

## Host-side validation rules

These rules apply **everywhere the alloc table is consumed** (software executor, GPU backend, etc).

### Descriptor-level rules

- `alloc_table_gpa` and `alloc_table_size_bytes` must be both zero (absent) or both non-zero
  (present).
- The host may omit parsing if the table is absent.

### Header-level rules

- `magic == AEROGPU_ALLOC_TABLE_MAGIC`.
- ABI major version must match. Minor may be newer.
- `size_bytes >= sizeof(aerogpu_alloc_table_header)`.
- `size_bytes <= alloc_table_size_bytes` (descriptor-provided mapping size).
- `entry_stride_bytes >= sizeof(aerogpu_alloc_entry)` (forward-compatible extension space).
- `entry_count * entry_stride_bytes` must fit within `size_bytes`.

### Entry-level rules

For each entry:

- `alloc_id != 0`
- `size_bytes != 0`
- `gpa + size_bytes` must not overflow `u64`
- `alloc_id` must be unique within the table (duplicates are an error).

### Cross-check rules with the command stream

- Any packet that references `backing_alloc_id != 0` requires that:
  - the allocation table is present for the submission, and
  - the referenced `alloc_id` exists in that table.

## Guest-side requirements (Win7/WDDM 1.1)

On Win7, the KMD builds the per-submit allocation table from the submission’s WDDM allocation list (`DXGK_ALLOCATIONLIST`), and only allocations that appear in that list can contribute `alloc_id → gpa` entries.

Therefore, any UMD packet that references `backing_alloc_id != 0` must ensure the corresponding WDDM allocation handle is included in the submit allocation list for that submission (even if the resource is not currently bound; `RESOURCE_DIRTY_RANGE` is a common case).

## Backing interpretation (`aerogpu_cmd.h`)

### `CREATE_BUFFER`

- `backing_alloc_id == 0` means the buffer is host-allocated (no guest backing).
- Otherwise the backing range is:

```
base_gpa = alloc_table[backing_alloc_id].gpa
buffer_bytes = [base_gpa + backing_offset_bytes,
                base_gpa + backing_offset_bytes + size_bytes)
```

The host must validate `backing_offset_bytes + size_bytes <= alloc.size_bytes`.

### `CREATE_TEXTURE2D`

When `backing_alloc_id != 0`, textures are backed by a **linear** guest-memory layout:

```
base_gpa = alloc_table[backing_alloc_id].gpa
row0 = base_gpa + backing_offset_bytes
rowN = row0 + N * row_pitch_bytes
```

The host must validate:

- `row_pitch_bytes != 0`
- `row_pitch_bytes >= width * bytes_per_pixel(format)`
- `backing_offset_bytes + row_pitch_bytes * height <= alloc.size_bytes`

> MVP: Win7 shared-surface interop currently assumes `mip_levels=1` and `array_layers=1`.

## `READONLY` semantics

`aerogpu_alloc_entry.flags` includes `AEROGPU_ALLOC_FLAG_READONLY`.

READONLY means:

- The host must **never write** to this allocation’s guest backing memory.
- Any command that requests a guest-memory writeback to a READONLY allocation must be rejected
  (validation error). This includes explicit writeback flags (e.g. `COPY_* WRITEBACK_DST`) and any
  implicit writeback path the host implements.

## Fence ordering

If a submission requests any guest-memory writeback, the host must only signal/advance the fence
after those writebacks are complete and visible to the guest.
