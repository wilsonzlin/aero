# AeroGPU `backing_alloc_id` (Win7 WDDM 1.1) Contract

This document defines the **stable semantics** of `backing_alloc_id` as used by
`AEROGPU_CMD_CREATE_BUFFER` / `AEROGPU_CMD_CREATE_TEXTURE2D`
(`drivers/aerogpu/protocol/aerogpu_cmd.h`) on the **Windows 7 WDDM 1.1** guest
path.

## Problem recap

`CREATE_*` commands store a `backing_alloc_id: u32` that is intended to let the
host/emulator locate the guest memory backing a resource.

On Win7, every submission can carry an **optional sideband allocation table**
describing the guest physical pages (GPA) for WDDM allocations referenced by
that submission. The ordering of that table is **not stable** across submissions.

If `backing_alloc_id` were interpreted as “slot+1 in the current submit’s
allocation list”, a resource created in one submission could silently bind to
the wrong memory in later submissions unless the UMD re-emits `CREATE_*` every
time. That is both fragile and easy to get wrong in DWM / multi-frame apps.

## Contract (chosen)

`backing_alloc_id` is a **stable per-allocation ID** (not an array index).

### Key rules

1. **Stable ID**
   - `backing_alloc_id` is a guest-chosen `u32` **allocation identifier** with
     lifetime = allocation lifetime.
   - `0` is reserved and means “no guest allocation backing” (host-allocated
     resource).

2. **Per-submit allocation table provides resolution**
   - Each submission may include an allocation table that maps:
     - `alloc_id (u32)` → `gpa (u64)` + `size (u64/u32)`
   - **The table is per-submit and may be reordered**. The host must resolve by
     `alloc_id`, not by entry position.

3. **Host resolution**
   - When processing a command that references `backing_alloc_id != 0`, the host
     must:
     - Find the entry with matching `alloc_id` in the submission’s allocation
       table.
     - Validate bounds:
       - `backing_offset_bytes + resource_size_bytes <= alloc.size_bytes`
     - Treat a missing `alloc_id` entry as a validation error.

4. **64-bit handles on x64 / collisions**
   - `alloc_id` is explicitly `u32`. Do **not** truncate 64-bit kernel pointers
     or 64-bit OS handles into `u32`.
   - If the guest needs to associate a 64-bit handle with an `alloc_id`, it must
     maintain a driver-side map `{handle64 -> alloc_id}` and allocate unique
     `alloc_id` values (monotonic counter is recommended).

5. **`CREATE_*` for an existing `resource_handle`**
   - The host treats this as a **rebind/update** of the backing memory *only if*
     all immutable resource properties match the existing resource:
       - Buffers: `size_bytes`, `usage_flags`
       - Textures: `format`, `width`, `height`, `mip_levels`, `array_layers`,
         `row_pitch_bytes`, `usage_flags`
   - If immutable properties differ, the host must treat the command as a
     validation error (the guest should `DESTROY_RESOURCE` and create a new
     handle instead).

## Where this is implemented

* Command stream: `drivers/aerogpu/protocol/aerogpu_cmd.h`
* Per-submit allocation table: `drivers/aerogpu/protocol/aerogpu_ring.h` (`aerogpu_alloc_table` /
  `aerogpu_alloc_entry`, keyed by `alloc_id`)
  * Legacy ABI note: the Win7 KMD also has a legacy submission descriptor path; its allocation list
    carries `alloc_id` in the final `u32` field (layout unchanged; see
    `drivers/aerogpu/kmd/include/aerogpu_legacy_abi.h`).
* Win7 allocation-ID persistence for shared resources: `drivers/aerogpu/protocol/aerogpu_wddm_alloc.h`
  (`aerogpu_wddm_alloc_priv`)
* Host-side command stream state machine (validation/rebind semantics): `crates/aero-gpu/src/command_processor.rs`
* Host-side executor (wgpu, guest-memory-backed uploads): `crates/aero-gpu/src/aerogpu_executor.rs`
* Browser GPU worker executor (JS, SharedArrayBuffer-backed guest RAM uploads): `web/src/workers/gpu-worker.ts`
* WASM wrapper for the command processor (decodes alloc_table bytes for validation): `crates/aero-gpu-wasm/src/lib.rs`
