// IA vertex pulling helper library.
//
// Binding scheme (group 2):
// - binding 0: uniform IaMeta (per-slot base offset + stride)
// - binding 1..8: IA vertex buffers (storage, read) in compacted slot order (max 8)
//
// This file intentionally contains only IA-related declarations and helpers; compute prepass
// shaders are expected to declare their own outputs.

const IA_MAX_VERTEX_BUFFERS: u32 = 8u;

struct IaMeta {
  // Per-slot metadata: .x = base_offset_bytes, .y = stride_bytes
  vb: array<vec4<u32>, IA_MAX_VERTEX_BUFFERS>,
};

struct ByteAddressBuffer {
  data: array<u32>,
};

@group(2) @binding(0) var<uniform> ia_meta: IaMeta;

@group(2) @binding(1) var<storage, read> ia_vb0: ByteAddressBuffer;
@group(2) @binding(2) var<storage, read> ia_vb1: ByteAddressBuffer;
@group(2) @binding(3) var<storage, read> ia_vb2: ByteAddressBuffer;
@group(2) @binding(4) var<storage, read> ia_vb3: ByteAddressBuffer;
@group(2) @binding(5) var<storage, read> ia_vb4: ByteAddressBuffer;
@group(2) @binding(6) var<storage, read> ia_vb5: ByteAddressBuffer;
@group(2) @binding(7) var<storage, read> ia_vb6: ByteAddressBuffer;
@group(2) @binding(8) var<storage, read> ia_vb7: ByteAddressBuffer;

fn ia_bab_load_u32(buf: ptr<storage, ByteAddressBuffer, read>, byte_addr: u32) -> u32 {
  // Load a single dword from a byte address, handling unaligned byte addresses by stitching two
  // adjacent u32 reads (mirrors D3D's ByteAddressBuffer behavior).
  //
  // This matters because D3D11 IA vertex-buffer base offsets are byte-granular, but WebGPU storage
  // buffer bindings typically require 256-byte alignment, so the runtime binds the full buffer at
  // offset 0 and applies the D3D offset in shader code.
  let word_index: u32 = byte_addr >> 2u;
  let shift: u32 = (byte_addr & 3u) * 8u;
  let word_count: u32 = arrayLength(&(*buf).data);
  if (word_index >= word_count) {
    return 0u;
  }
  let lo: u32 = (*buf).data[word_index];
  if (shift == 0u) {
    return lo;
  }
  let hi: u32 =
      select(0u, (*buf).data[word_index + 1u], (word_index + 1u) < word_count);
  return (lo >> shift) | (hi << (32u - shift));
}

fn ia_load_u32(slot: u32, byte_addr: u32) -> u32 {
  // Guard against out-of-range slots so we don't index out-of-bounds in the metadata array.
  if (slot >= IA_MAX_VERTEX_BUFFERS) {
    return 0u;
  }
  if (slot == 0u) { return ia_bab_load_u32(&ia_vb0, byte_addr); }
  if (slot == 1u) { return ia_bab_load_u32(&ia_vb1, byte_addr); }
  if (slot == 2u) { return ia_bab_load_u32(&ia_vb2, byte_addr); }
  if (slot == 3u) { return ia_bab_load_u32(&ia_vb3, byte_addr); }
  if (slot == 4u) { return ia_bab_load_u32(&ia_vb4, byte_addr); }
  if (slot == 5u) { return ia_bab_load_u32(&ia_vb5, byte_addr); }
  if (slot == 6u) { return ia_bab_load_u32(&ia_vb6, byte_addr); }
  return ia_bab_load_u32(&ia_vb7, byte_addr);
}

fn ia_load_f32(slot: u32, byte_addr: u32) -> f32 {
  return bitcast<f32>(ia_load_u32(slot, byte_addr));
}

fn ia_vertex_byte_addr(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> u32 {
  // Note: `slot` is validated in `ia_load_u32`, but keep this guarded too so callers that compute
  // addresses separately don't risk invalid uniform indexing.
  if (slot >= IA_MAX_VERTEX_BUFFERS) {
    return 0u;
  }
  let m = ia_meta.vb[slot];
  let base = m.x;
  let stride = m.y;
  return base + vertex_index * stride + element_offset_bytes;
}

// DXGI_FORMAT_R32G32B32_FLOAT
fn ia_load_r32g32b32_float(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec3<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  return vec3<f32>(
    ia_load_f32(slot, addr + 0u),
    ia_load_f32(slot, addr + 4u),
    ia_load_f32(slot, addr + 8u),
  );
}

// DXGI_FORMAT_R32G32_FLOAT
fn ia_load_r32g32_float(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec2<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  return vec2<f32>(
    ia_load_f32(slot, addr + 0u),
    ia_load_f32(slot, addr + 4u),
  );
}

// DXGI_FORMAT_R8G8B8A8_UNORM
fn ia_load_r8g8b8a8_unorm(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec4<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  let r = f32(packed & 0xffu) / 255.0;
  let g = f32((packed >> 8u) & 0xffu) / 255.0;
  let b = f32((packed >> 16u) & 0xffu) / 255.0;
  let a = f32((packed >> 24u) & 0xffu) / 255.0;
  return vec4<f32>(r, g, b, a);
}
