# GPU Trace Format (AeroGPU)
This document specifies the on-disk format for **Aero GPU command traces** (“gpu-trace”).
The intent is to make **graphics bugs reproducible** by recording the guest→host GPU
command stream (including shader blobs and resource uploads) and replaying it
deterministically in isolation.

> Status: **v2** (adds raw `aerogpu_cmd.h` submission capture). Backwards-incompatible
> changes must bump `container_version` in the file header.

---

## Goals / Non-goals

### Goals
- Record the GPU command stream **in submission order** (the exact packet bytes
  emitted by the guest-facing GPU command processor).
- Record referenced **resource uploads/snapshots** (buffers/textures) as bytes
  (either inline in the packet stream or as `Blob` records, depending on the command ABI).
- Record **shader blobs** (DXBC) and optionally translated text (WGSL, GLSL ES 3.00),
  either inline in packets or as `Blob` records.
- Record **frame boundaries** (begin-frame + present markers).
- Provide a **frame TOC** for random access / quick seeking.
- Be stable enough to share in CI artifacts and between machines/browsers.

### Non-goals (v1)
- Distributed/streaming traces (v1 assumes the entire trace is available locally).
- Compression (can be layered externally or added as a future record flag).
- Capturing CPU state; this is a GPU-only trace.

---

## File structure (binary, little-endian)

All multi-byte integers are **little-endian**.

```
┌───────────────────────┐
│ TraceHeader (32 bytes)│
├───────────────────────┤
│ meta_json (N bytes)   │  UTF-8 JSON, length = meta_len
├───────────────────────┤
│ record stream         │  variable; see "Records"
├───────────────────────┤
│ TraceToc              │  variable; located via footer
├───────────────────────┤
│ TraceFooter (32 bytes)│
└───────────────────────┘
```

### TraceHeader (32 bytes)

| Field               | Type  | Value / Meaning |
|---------------------|-------|-----------------|
| `magic`             | [u8;8]| `"AEROGPUT"` |
| `header_size`       | u32   | Must be 32 for v1 |
| `container_version` | u32   | Trace container version (v1 = 1, v2 = 2) |
| `command_abi_version` | u32 | Version of the **GPU command packet ABI** recorded in the trace (for canonical WDDM AeroGPU A3A0 traces: `AEROGPU_ABI_VERSION_U32` from `drivers/aerogpu/protocol/aerogpu_pci.h`) |
| `flags`             | u32   | Reserved (0 for v1) |
| `meta_len`          | u32   | Length in bytes of UTF-8 JSON metadata blob |
| `reserved`          | u32   | Must be 0 for v1 |

`meta_json` is implementation-defined but must include at least:
- `emulator_version` (string)
- `command_abi_version` (number; should match header)

Example:
```json
{
  "emulator_version": "0.0.0-dev",
  "command_abi_version": 65536,
  "notes": "optional"
}
```

### TraceFooter (32 bytes)

| Field               | Type  | Value / Meaning |
|---------------------|-------|-----------------|
| `magic`             | [u8;8]| `"AEROGPUF"` |
| `footer_size`       | u32   | Must be 32 for v1 |
| `container_version` | u32   | Must match header `container_version` |
| `toc_offset`        | u64   | Absolute file offset of `TraceToc` |
| `toc_len`           | u64   | Length in bytes of `TraceToc` |

---

## Records

The record stream is a sequence of records:

### RecordHeader (8 bytes)

| Field         | Type | Meaning |
|---------------|------|---------|
| `record_type` | u8   | See `RecordType` |
| `flags`       | u8   | Reserved (0 for v1) |
| `reserved`    | u16  | Must be 0 for v1 |
| `payload_len` | u32  | Length in bytes of payload that follows |

### RecordType

| Type | Name        | Payload |
|------|-------------|---------|
| 0x01 | BeginFrame  | `u32 frame_index` |
| 0x02 | Present     | `u32 frame_index` |
| 0x03 | Packet      | Raw command packet bytes (length = `payload_len`) |
| 0x04 | Blob        | `BlobHeader` + blob bytes |
| 0x05 | AerogpuSubmission | Structured submission record referencing blobs (v2+) |

### Blob record

Blob payload begins with `BlobHeader` (16 bytes):

| Field     | Type | Meaning |
|-----------|------|---------|
| `blob_id` | u64  | Unique ID (referenced by other records and by command packets in ABIs that support blob references; see appendices) |
| `kind`    | u32  | See `BlobKind` |
| `reserved`| u32  | Must be 0 for v1 |

Blob bytes follow immediately after `BlobHeader` and run to the end of the record payload.

### BlobKind

| Kind | Name              | Typical content |
|------|-------------------|-----------------|
| 0x01 | BufferData        | Exact bytes uploaded to a buffer |
| 0x02 | TextureData       | Raw texture subresource bytes (format described by command packets) |
| 0x03 | ShaderDxbc        | DXBC bytecode blob |
| 0x04 | ShaderWgsl        | WGSL UTF-8 text |
| 0x05 | ShaderGlslEs300   | GLSL ES 3.00 UTF-8 text (for WebGL2 fallback) |
| 0x100 | AerogpuCmdStream | Raw `aerogpu_cmd_stream_header` + command packets |
| 0x101 | AerogpuAllocTable | Raw `aerogpu_alloc_table_header` + entries (if present) |
| 0x102 | AerogpuAllocMemory | Raw guest memory bytes for an allocation table entry |

### `AerogpuSubmission` record (v2+)

The payload is a little-endian struct:

```
u32 record_version;          // = 1
u32 header_size;             // bytes of the fixed header (v1 = 56)
u32 submit_flags;            // `aerogpu_submit_desc.flags`
u32 context_id;              // `aerogpu_submit_desc.context_id`
u32 engine_id;               // `aerogpu_submit_desc.engine_id`
u32 reserved0;               // = 0
u64 signal_fence;            // `aerogpu_submit_desc.signal_fence`
u64 cmd_stream_blob_id;      // BlobKind::AerogpuCmdStream
u64 alloc_table_blob_id;     // BlobKind::AerogpuAllocTable (0 if not present)
u32 memory_range_count;
u32 reserved1;               // = 0
// followed by `memory_range_count` entries:
u32 alloc_id;
u32 flags;
u64 gpa;
u64 size_bytes;
u64 blob_id;                 // BlobKind::AerogpuAllocMemory
```

The intent is to record the *exact* `aerogpu_cmd.h` byte stream submitted by the guest plus any
required guest memory snapshots (allocation table entries) so traces can be replayed headlessly.

---

## TraceToc (frame index / random access)

The TOC enables fast seeking to a specific frame without scanning the entire record stream.

### TOC header

| Field        | Type  | Meaning |
|--------------|-------|---------|
| `magic`      | [u8;8]| `"AEROTOC\0"` |
| `toc_version`| u32   | v1 = 1 |
| `frame_count`| u32   | Number of frames in the trace |

### Frame entry (32 bytes each)

| Field           | Type | Meaning |
|----------------|------|---------|
| `frame_index`   | u32  | Monotonic (0..N-1) |
| `flags`         | u32  | Reserved (0 for v1) |
| `start_offset`  | u64  | Absolute offset of the `BeginFrame` record |
| `present_offset`| u64  | Absolute offset of the `Present` record (0 if missing) |
| `end_offset`    | u64  | Absolute offset *immediately after* the last record in the frame |

---

## Determinism notes

To make traces replayable on other machines/browsers:
- Command packets must be recorded **after** translation from guest APIs (e.g. D3D9)
  into the stable AeroGPU command ABI.
- All resource/shader data needed for replay must be serialized into the trace
  (either inline in command streams/packets or as `Blob` records) — replayers must never rely on guest RAM.
- Shader sources should be recorded:
  - DXBC (for postmortem analysis) and
  - a backend-consumable representation (WGSL for WebGPU, GLSL ES 3.00 for WebGL2 fallback).

---

## Appendix A: Minimal command ABI v1 (used by the reference replayer + tests)

This repository includes a tiny **reference** command ABI so we can produce a deterministic
triangle trace fixture and replay it in CI.

> Real AeroGPU traces are expected to record the canonical WDDM AeroGPU ring/opcode stream as
> defined by [`drivers/aerogpu/protocol`](../../drivers/aerogpu/protocol/README.md)
> (and mirrored in [`emulator/protocol`](../../emulator/protocol)).
> The ABI below exists only to validate the trace container + replayer plumbing.
>
> (The Win7/WDDM AeroGPU ABI lives in `drivers/aerogpu/protocol/*`; this appendix is about the
> trace container format, not a particular guest driver ABI.)

### Packet encoding

Every `RecordType::Packet` is a sequence of little-endian `u32` dwords:

| Dword | Type | Meaning |
|------:|------|---------|
| 0 | u32 | `opcode` |
| 1 | u32 | `total_dwords` including this 2-dword header |
| 2.. | u32[] | Opcode-specific payload |

### Opcodes

All IDs are `u32`. Blob IDs are `u64` split into `(lo: u32, hi: u32)`.

| Opcode | Name | Payload dwords |
|-------:|------|----------------|
| 0x0001 | `CREATE_BUFFER` | `buffer_id`, `size_bytes`, `usage` |
| 0x0002 | `UPLOAD_BUFFER` | `buffer_id`, `offset_bytes`, `data_len_bytes`, `blob_id_lo`, `blob_id_hi` |
| 0x0003 | `CREATE_SHADER` | `shader_id`, `stage` (0=VS,1=FS), `glsl_blob_id_lo`, `glsl_blob_id_hi`, `wgsl_blob_id_lo`, `wgsl_blob_id_hi`, `dxbc_blob_id_lo`, `dxbc_blob_id_hi` |
| 0x0004 | `CREATE_PIPELINE` | `pipeline_id`, `vs_shader_id`, `fs_shader_id` |
| 0x0005 | `SET_PIPELINE` | `pipeline_id` |
| 0x0006 | `SET_VERTEX_BUFFER` | `buffer_id`, `stride_bytes`, `position_offset_bytes`, `color_offset_bytes` |
| 0x0007 | `SET_VIEWPORT` | `width_px`, `height_px` |
| 0x0008 | `CLEAR` | `r_f32_bits`, `g_f32_bits`, `b_f32_bits`, `a_f32_bits` |
| 0x0009 | `DRAW` | `vertex_count`, `first_vertex` |
| 0x000A | `PRESENT` | *(no payload)* |

---

## Appendix B: Canonical AeroGPU command ABI (A3A0 / WDDM)

The canonical Windows 7 WDDM AeroGPU ABI in this repository is the **versioned A3A0 protocol**
defined in `drivers/aerogpu/protocol/`:

- `drivers/aerogpu/protocol/aerogpu_cmd.h` (command buffer + packet formats; bytes recorded)
- `drivers/aerogpu/protocol/aerogpu_ring.h` (ring submission ordering + fence semantics)
- `drivers/aerogpu/protocol/aerogpu_pci.h` (ABI versioning: `AEROGPU_ABI_VERSION_U32`)

For container v2 traces, the recommended representation for the canonical WDDM AeroGPU path is:

- `RecordType::AerogpuSubmission` referencing
  - `BlobKind::AerogpuCmdStream` (raw `aerogpu_cmd_stream_header` + command packets), and optionally
  - `BlobKind::AerogpuAllocTable` / `BlobKind::AerogpuAllocMemory` (allocation table + snapshots).

### Inline uploads / shader blobs (A3A0)

Some A3A0 command packets embed variable-length data directly in the command stream. Examples
include `AEROGPU_CMD_UPLOAD_RESOURCE` (resource bytes) and `AEROGPU_CMD_CREATE_SHADER_DXBC` (DXBC).

**Rule:** traces store these bytes **inline** as part of the recorded command stream bytes
(either inside `RecordType::Packet` payloads or inside `BlobKind::AerogpuCmdStream`). The trace does
not extract them into separate `Blob` records with packet fields patched to blob IDs.

### `command_abi_version` (A3A0)

For canonical WDDM AeroGPU A3A0 traces, the trace header’s `command_abi_version` MUST equal
`AEROGPU_ABI_VERSION_U32` as defined in `drivers/aerogpu/protocol/aerogpu_pci.h`:

```
(AEROGPU_ABI_MAJOR << 16) | AEROGPU_ABI_MINOR
```
