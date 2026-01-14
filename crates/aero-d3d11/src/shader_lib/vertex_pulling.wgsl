// IA vertex pulling helper library.
//
// Binding scheme (group 2):
// - binding 0: uniform IaMeta (per-slot base offset + stride)
// - binding 1..3: IA vertex buffers (storage, read) in compacted slot order (max 3)
//
// This file intentionally contains only IA-related declarations and helpers; compute prepass
// shaders are expected to declare their own outputs.

// Keep this conservative: this WGSL helper is used in test compute pipelines that also need an
// output storage buffer, and downlevel adapters can expose as few as 4 storage buffers per stage.
const IA_MAX_VERTEX_BUFFERS: u32 = 3u;

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

fn ia_load_u32(slot: u32, byte_addr: u32) -> u32 {
  // Load a single dword from a byte address, handling unaligned byte addresses by stitching two
  // adjacent u32 reads (mirrors D3D's ByteAddressBuffer behavior).
  //
  // This matters because D3D11 IA vertex-buffer base offsets are byte-granular, but WebGPU storage
  // buffer bindings typically require 256-byte alignment, so the runtime binds the full buffer at
  // offset 0 and applies the D3D offset in shader code.
  //
  // Guard against out-of-range slots so we don't index out-of-bounds in the metadata array.
  if (slot >= IA_MAX_VERTEX_BUFFERS) {
    return 0u;
  }
  let word_index: u32 = byte_addr >> 2u;
  let shift: u32 = (byte_addr & 3u) * 8u;

  switch slot {
    case 0u: {
      let word_count: u32 = arrayLength(&ia_vb0.data);
      if (word_index >= word_count) { return 0u; }
      let lo: u32 = ia_vb0.data[word_index];
      if (shift == 0u) { return lo; }
      let hi: u32 = select(0u, ia_vb0.data[word_index + 1u], (word_index + 1u) < word_count);
      return (lo >> shift) | (hi << (32u - shift));
    }
    case 1u: {
      let word_count: u32 = arrayLength(&ia_vb1.data);
      if (word_index >= word_count) { return 0u; }
      let lo: u32 = ia_vb1.data[word_index];
      if (shift == 0u) { return lo; }
      let hi: u32 = select(0u, ia_vb1.data[word_index + 1u], (word_index + 1u) < word_count);
      return (lo >> shift) | (hi << (32u - shift));
    }
    case 2u: {
      let word_count: u32 = arrayLength(&ia_vb2.data);
      if (word_index >= word_count) { return 0u; }
      let lo: u32 = ia_vb2.data[word_index];
      if (shift == 0u) { return lo; }
      let hi: u32 = select(0u, ia_vb2.data[word_index + 1u], (word_index + 1u) < word_count);
      return (lo >> shift) | (hi << (32u - shift));
    }
    default: { return 0u; }
  }
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

// DXGI_FORMAT_R8G8B8A8_SNORM
fn ia_load_r8g8b8a8_snorm(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec4<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  return unpack4x8snorm(packed);
}

// DXGI_FORMAT_R8G8B8A8_UINT
fn ia_load_r8g8b8a8_uint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec4<u32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  return vec4<u32>(
    packed & 0xffu,
    (packed >> 8u) & 0xffu,
    (packed >> 16u) & 0xffu,
    (packed >> 24u) & 0xffu
  );
}

// DXGI_FORMAT_R8G8B8A8_SINT
fn ia_load_r8g8b8a8_sint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec4<i32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  let x = (bitcast<i32>(packed << 24u)) >> 24u;
  let y = (bitcast<i32>(packed << 16u)) >> 24u;
  let z = (bitcast<i32>(packed << 8u)) >> 24u;
  let w = bitcast<i32>(packed) >> 24u;
  return vec4<i32>(x, y, z, w);
}

// DXGI_FORMAT_R8G8_UNORM
fn ia_load_r8g8_unorm(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec2<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  let r = f32(packed & 0xffu) / 255.0;
  let g = f32((packed >> 8u) & 0xffu) / 255.0;
  return vec2<f32>(r, g);
}

// DXGI_FORMAT_R8G8_SNORM
fn ia_load_r8g8_snorm(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec2<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  return unpack4x8snorm(packed).xy;
}

// DXGI_FORMAT_R8G8_UINT
fn ia_load_r8g8_uint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec2<u32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  return vec2<u32>(
    packed & 0xffu,
    (packed >> 8u) & 0xffu
  );
}

// DXGI_FORMAT_R8G8_SINT
fn ia_load_r8g8_sint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec2<i32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  let x = (bitcast<i32>(packed << 24u)) >> 24u;
  let y = (bitcast<i32>(packed << 16u)) >> 24u;
  return vec2<i32>(x, y);
}

// DXGI_FORMAT_R10G10B10A2_UNORM
fn ia_load_r10g10b10a2_unorm(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec4<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  let r = f32(packed & 0x3ffu) / 1023.0;
  let g = f32((packed >> 10u) & 0x3ffu) / 1023.0;
  let b = f32((packed >> 20u) & 0x3ffu) / 1023.0;
  let a = f32((packed >> 30u) & 0x3u) / 3.0;
  return vec4<f32>(r, g, b, a);
}

// DXGI_FORMAT_R16_FLOAT
fn ia_load_r16_float(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> f32 {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  return unpack2x16float(packed).x;
}

// DXGI_FORMAT_R16_UNORM
fn ia_load_r16_unorm(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> f32 {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  return unpack2x16unorm(packed).x;
}

// DXGI_FORMAT_R16_SNORM
fn ia_load_r16_snorm(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> f32 {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  return unpack2x16snorm(packed).x;
}

// DXGI_FORMAT_R16G16_FLOAT
fn ia_load_r16g16_float(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec2<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  return unpack2x16float(packed);
}

// DXGI_FORMAT_R16G16_UNORM
fn ia_load_r16g16_unorm(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec2<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  return unpack2x16unorm(packed);
}

// DXGI_FORMAT_R16G16_SNORM
fn ia_load_r16g16_snorm(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec2<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  return unpack2x16snorm(packed);
}

// DXGI_FORMAT_R16G16B16A16_FLOAT
fn ia_load_r16g16b16a16_float(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec4<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let p0 = ia_load_u32(slot, addr + 0u);
  let p1 = ia_load_u32(slot, addr + 4u);
  let a = unpack2x16float(p0);
  let b = unpack2x16float(p1);
  return vec4<f32>(a.x, a.y, b.x, b.y);
}

// DXGI_FORMAT_R16G16B16A16_UNORM
fn ia_load_r16g16b16a16_unorm(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec4<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let p0 = ia_load_u32(slot, addr + 0u);
  let p1 = ia_load_u32(slot, addr + 4u);
  let a = unpack2x16unorm(p0);
  let b = unpack2x16unorm(p1);
  return vec4<f32>(a.x, a.y, b.x, b.y);
}

// DXGI_FORMAT_R16G16B16A16_SNORM
fn ia_load_r16g16b16a16_snorm(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec4<f32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let p0 = ia_load_u32(slot, addr + 0u);
  let p1 = ia_load_u32(slot, addr + 4u);
  let a = unpack2x16snorm(p0);
  let b = unpack2x16snorm(p1);
  return vec4<f32>(a.x, a.y, b.x, b.y);
}

// DXGI_FORMAT_R32_UINT
fn ia_load_r32_uint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> u32 {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  return ia_load_u32(slot, addr);
}

// DXGI_FORMAT_R32G32_UINT
fn ia_load_r32g32_uint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec2<u32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  return vec2<u32>(
    ia_load_u32(slot, addr + 0u),
    ia_load_u32(slot, addr + 4u),
  );
}

// DXGI_FORMAT_R32G32B32_UINT
fn ia_load_r32g32b32_uint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec3<u32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  return vec3<u32>(
    ia_load_u32(slot, addr + 0u),
    ia_load_u32(slot, addr + 4u),
    ia_load_u32(slot, addr + 8u),
  );
}

// DXGI_FORMAT_R32G32B32A32_UINT
fn ia_load_r32g32b32a32_uint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec4<u32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  return vec4<u32>(
    ia_load_u32(slot, addr + 0u),
    ia_load_u32(slot, addr + 4u),
    ia_load_u32(slot, addr + 8u),
    ia_load_u32(slot, addr + 12u),
  );
}

// DXGI_FORMAT_R16_UINT
fn ia_load_r16_uint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> u32 {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  return packed & 0xffffu;
}

// DXGI_FORMAT_R16_SINT
fn ia_load_r16_sint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> i32 {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  return (bitcast<i32>(packed << 16u)) >> 16u;
}

// DXGI_FORMAT_R16G16_UINT
fn ia_load_r16g16_uint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec2<u32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  let x = packed & 0xffffu;
  let y = packed >> 16u;
  return vec2<u32>(x, y);
}

// DXGI_FORMAT_R16G16_SINT
fn ia_load_r16g16_sint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec2<i32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let packed = ia_load_u32(slot, addr);
  let lo = (bitcast<i32>(packed << 16u)) >> 16u;
  let hi = bitcast<i32>(packed) >> 16u;
  return vec2<i32>(lo, hi);
}

// DXGI_FORMAT_R16G16B16A16_UINT
fn ia_load_r16g16b16a16_uint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec4<u32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let p0 = ia_load_u32(slot, addr + 0u);
  let p1 = ia_load_u32(slot, addr + 4u);
  let x0 = p0 & 0xffffu;
  let y0 = p0 >> 16u;
  let x1 = p1 & 0xffffu;
  let y1 = p1 >> 16u;
  return vec4<u32>(x0, y0, x1, y1);
}

// DXGI_FORMAT_R16G16B16A16_SINT
fn ia_load_r16g16b16a16_sint(slot: u32, vertex_index: u32, element_offset_bytes: u32) -> vec4<i32> {
  let addr = ia_vertex_byte_addr(slot, vertex_index, element_offset_bytes);
  let p0 = ia_load_u32(slot, addr + 0u);
  let p1 = ia_load_u32(slot, addr + 4u);
  let lo0 = (bitcast<i32>(p0 << 16u)) >> 16u;
  let hi0 = bitcast<i32>(p0) >> 16u;
  let lo1 = (bitcast<i32>(p1 << 16u)) >> 16u;
  let hi1 = bitcast<i32>(p1) >> 16u;
  return vec4<i32>(lo0, hi0, lo1, hi1);
}
