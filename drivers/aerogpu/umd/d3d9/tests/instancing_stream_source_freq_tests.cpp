#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <vector>

#include "aerogpu_d3d9_objects.h"

namespace aerogpu {

// Forward declaration for the draw entrypoint under test.
HRESULT AEROGPU_D3D9_CALL device_draw_indexed_primitive(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    int32_t base_vertex,
    uint32_t min_index,
    uint32_t num_vertices,
    uint32_t start_index,
    uint32_t primitive_count);

} // namespace aerogpu

namespace {

// SetStreamSourceFreq encodings (from d3d9types.h).
constexpr uint32_t kD3DStreamSourceIndexedData = 0x40000000u;
constexpr uint32_t kD3DStreamSourceInstanceData = 0x80000000u;

// ABI-compatible D3DVERTEXELEMENT9 encoding.
#pragma pack(push, 1)
struct D3DVERTEXELEMENT9_COMPAT {
  uint16_t Stream;
  uint16_t Offset;
  uint8_t Type;
  uint8_t Method;
  uint8_t Usage;
  uint8_t UsageIndex;
};
#pragma pack(pop)
static_assert(sizeof(D3DVERTEXELEMENT9_COMPAT) == 8, "D3DVERTEXELEMENT9 must be 8 bytes");

constexpr uint8_t kD3dDeclTypeFloat4 = 3;
constexpr uint8_t kD3dDeclTypeUnused = 17;
constexpr uint8_t kD3dDeclMethodDefault = 0;
constexpr uint8_t kD3dDeclUsagePosition = 0;
constexpr uint8_t kD3dDeclUsageTexcoord = 5;
constexpr uint8_t kD3dDeclUsageColor = 10;

struct Vec4 {
  float x;
  float y;
  float z;
  float w;
};

struct InstanceData {
  Vec4 offset;
  Vec4 color;
};

const aerogpu_cmd_upload_resource* FindLastUploadForHandle(
    const uint8_t* buf,
    size_t len,
    aerogpu_handle_t handle) {
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return nullptr;
  }

  const aerogpu_cmd_upload_resource* found = nullptr;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_UPLOAD_RESOURCE) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(hdr);
      if (cmd->resource_handle == handle) {
        found = cmd;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return found;
}

} // namespace

int main() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  // Minimal shader bindings so ensure_draw_pipeline_locked accepts the draw (no fixed-function fallback).
  aerogpu::Shader vs{};
  aerogpu::Shader ps{};
  dev.user_vs = &vs;
  dev.user_ps = &ps;
  dev.vs = &vs;
  dev.ps = &ps;

  // Vertex declaration:
  //   stream0: POSITION float4 @0
  //   stream1: TEXCOORD0 float4 @0  (instance offset)
  //   stream1: COLOR0    float4 @16 (instance color)
  aerogpu::VertexDecl decl{};
  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {1, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {1, 16, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };
  decl.blob.assign(reinterpret_cast<const uint8_t*>(elems),
                   reinterpret_cast<const uint8_t*>(elems) + sizeof(elems));
  dev.vertex_decl = &decl;

  // Stream 0: per-vertex positions.
  const Vec4 vertices[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f},
      {1.0f, 0.0f, 0.0f, 1.0f},
      {0.0f, 1.0f, 0.0f, 1.0f},
  };
  aerogpu::Resource vb0{};
  vb0.handle = 0x100;
  vb0.kind = aerogpu::ResourceKind::Buffer;
  vb0.size_bytes = sizeof(vertices);
  vb0.storage.resize(sizeof(vertices));
  std::memcpy(vb0.storage.data(), vertices, sizeof(vertices));

  // Stream 1: per-instance data (offset + color).
  const InstanceData instances[2] = {
      {{10.0f, 0.0f, 0.0f, 0.0f}, {1.0f, 0.0f, 0.0f, 1.0f}},
      {{20.0f, 0.0f, 0.0f, 0.0f}, {0.0f, 1.0f, 0.0f, 1.0f}},
  };
  aerogpu::Resource vb1{};
  vb1.handle = 0x101;
  vb1.kind = aerogpu::ResourceKind::Buffer;
  vb1.size_bytes = sizeof(instances);
  vb1.storage.resize(sizeof(instances));
  std::memcpy(vb1.storage.data(), instances, sizeof(instances));

  // Index buffer (16-bit): [0, 1, 2].
  const uint16_t indices_u16[3] = {0, 1, 2};
  aerogpu::Resource ib{};
  ib.handle = 0x102;
  ib.kind = aerogpu::ResourceKind::Buffer;
  ib.size_bytes = sizeof(indices_u16);
  ib.storage.resize(sizeof(indices_u16));
  std::memcpy(ib.storage.data(), indices_u16, sizeof(indices_u16));

  dev.streams[0] = {&vb0, 0, sizeof(Vec4)};
  dev.streams[1] = {&vb1, 0, sizeof(InstanceData)};
  dev.index_buffer = &ib;
  dev.index_format = static_cast<D3DDDIFORMAT>(101); // D3DFMT_INDEX16
  dev.index_offset_bytes = 0;

  // Instancing state: stream 0 repeats twice, stream 1 advances per instance.
  dev.stream_source_freq[0] = kD3DStreamSourceIndexedData | 2u;
  dev.stream_source_freq[1] = kD3DStreamSourceInstanceData | 1u;

  // Draw two instances.
  const HRESULT hr = aerogpu::device_draw_indexed_primitive(
      hDevice,
      D3DDDIPT_TRIANGLELIST,
      /*base_vertex=*/0,
      /*min_index=*/0,
      /*num_vertices=*/3,
      /*start_index=*/0,
      /*primitive_count=*/1);
  assert(hr == S_OK);

  assert(dev.instancing_vertex_buffers[0] != nullptr);
  assert(dev.instancing_vertex_buffers[1] != nullptr);
  assert(dev.up_index_buffer != nullptr);

  dev.cmd.finalize();
  const uint8_t* buf = dev.cmd.data();
  const size_t len = dev.cmd.size();

  const aerogpu_cmd_upload_resource* upload0 =
      FindLastUploadForHandle(buf, len, dev.instancing_vertex_buffers[0]->handle);
  const aerogpu_cmd_upload_resource* upload1 =
      FindLastUploadForHandle(buf, len, dev.instancing_vertex_buffers[1]->handle);
  const aerogpu_cmd_upload_resource* upload_ib =
      FindLastUploadForHandle(buf, len, dev.up_index_buffer->handle);

  assert(upload0 != nullptr);
  assert(upload1 != nullptr);
  assert(upload_ib != nullptr);

  // Validate expanded stream 0 upload: 2 instances => [v0,v1,v2,v0,v1,v2].
  const size_t expected_vb0_bytes = sizeof(vertices) * 2;
  assert(upload0->offset_bytes == 0);
  assert(upload0->size_bytes == expected_vb0_bytes);
  std::vector<uint8_t> expected_vb0(expected_vb0_bytes, 0);
  std::memcpy(expected_vb0.data() + 0, vertices, sizeof(vertices));
  std::memcpy(expected_vb0.data() + sizeof(vertices), vertices, sizeof(vertices));
  const uint8_t* payload0 = reinterpret_cast<const uint8_t*>(upload0) + sizeof(*upload0);
  assert(std::memcmp(payload0, expected_vb0.data(), expected_vb0.size()) == 0);

  // Validate expanded stream 1 upload: [inst0 x3, inst1 x3].
  const size_t expected_vb1_bytes = sizeof(InstanceData) * 6;
  assert(upload1->offset_bytes == 0);
  assert(upload1->size_bytes == expected_vb1_bytes);
  std::vector<uint8_t> expected_vb1(expected_vb1_bytes, 0);
  for (int v = 0; v < 3; ++v) {
    std::memcpy(expected_vb1.data() + (size_t)v * sizeof(InstanceData), &instances[0], sizeof(InstanceData));
  }
  for (int v = 0; v < 3; ++v) {
    std::memcpy(expected_vb1.data() + (size_t)(3 + v) * sizeof(InstanceData), &instances[1], sizeof(InstanceData));
  }
  const uint8_t* payload1 = reinterpret_cast<const uint8_t*>(upload1) + sizeof(*upload1);
  assert(std::memcmp(payload1, expected_vb1.data(), expected_vb1.size()) == 0);

  // Validate expanded index upload (u32): [0,1,2,3,4,5].
  const uint32_t expected_indices_u32[6] = {0, 1, 2, 3, 4, 5};
  assert(upload_ib->offset_bytes == 0);
  assert(upload_ib->size_bytes == sizeof(expected_indices_u32));
  const uint8_t* payload_ib = reinterpret_cast<const uint8_t*>(upload_ib) + sizeof(*upload_ib);
  assert(std::memcmp(payload_ib, expected_indices_u32, sizeof(expected_indices_u32)) == 0);

  return 0;
}
