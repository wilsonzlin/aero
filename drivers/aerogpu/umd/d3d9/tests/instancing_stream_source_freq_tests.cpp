#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <vector>

#include "aerogpu_d3d9_objects.h"

namespace aerogpu {

// Forward declarations for the draw entrypoints under test.
HRESULT AEROGPU_D3D9_CALL device_draw_primitive(
    D3DDDI_HDEVICE hDevice,
    D3DDDIPRIMITIVETYPE type,
    uint32_t start_vertex,
    uint32_t primitive_count);

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

const aerogpu_cmd_upload_resource* FindLastUploadForHandle(const uint8_t* buf, size_t len, aerogpu_handle_t handle) {
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

template <typename CmdT>
std::vector<const CmdT*> FindAllCmds(const uint8_t* buf, size_t len, uint32_t opcode) {
  std::vector<const CmdT*> out;
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return out;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      out.push_back(reinterpret_cast<const CmdT*>(hdr));
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return out;
}

void BindTestShaders(aerogpu::Device& dev, aerogpu::Shader& vs, aerogpu::Shader& ps) {
  dev.user_vs = &vs;
  dev.user_ps = &ps;
  dev.vs = &vs;
  dev.ps = &ps;
}

void BindTestDecl(aerogpu::Device& dev, aerogpu::VertexDecl& decl) {
  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {1, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {1, 16, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };
  decl.blob.assign(reinterpret_cast<const uint8_t*>(elems),
                   reinterpret_cast<const uint8_t*>(elems) + sizeof(elems));
  dev.vertex_decl = &decl;
}

void TestIndexedTriangleListBasic() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  aerogpu::Shader vs{};
  aerogpu::Shader ps{};
  BindTestShaders(dev, vs, ps);

  aerogpu::VertexDecl decl{};
  BindTestDecl(dev, decl);

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
  const aerogpu_cmd_upload_resource* upload_ib = FindLastUploadForHandle(buf, len, dev.up_index_buffer->handle);

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
}

void TestIndexedTriangleListInstancedDivisor() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  aerogpu::Shader vs{};
  aerogpu::Shader ps{};
  BindTestShaders(dev, vs, ps);

  aerogpu::VertexDecl decl{};
  BindTestDecl(dev, decl);

  const Vec4 vertices[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f},
      {1.0f, 0.0f, 0.0f, 1.0f},
      {0.0f, 1.0f, 0.0f, 1.0f},
  };
  aerogpu::Resource vb0{};
  vb0.handle = 0x200;
  vb0.kind = aerogpu::ResourceKind::Buffer;
  vb0.size_bytes = sizeof(vertices);
  vb0.storage.resize(sizeof(vertices));
  std::memcpy(vb0.storage.data(), vertices, sizeof(vertices));

  // 3 instances, divisor 2 => 2 elements. Element0 used for inst0+inst1, element1 for inst2.
  const InstanceData inst_elems[2] = {
      {{10.0f, 0.0f, 0.0f, 0.0f}, {1.0f, 0.0f, 0.0f, 1.0f}},
      {{20.0f, 0.0f, 0.0f, 0.0f}, {0.0f, 1.0f, 0.0f, 1.0f}},
  };
  aerogpu::Resource vb1{};
  vb1.handle = 0x201;
  vb1.kind = aerogpu::ResourceKind::Buffer;
  vb1.size_bytes = sizeof(inst_elems);
  vb1.storage.resize(sizeof(inst_elems));
  std::memcpy(vb1.storage.data(), inst_elems, sizeof(inst_elems));

  const uint16_t indices_u16[3] = {0, 1, 2};
  aerogpu::Resource ib{};
  ib.handle = 0x202;
  ib.kind = aerogpu::ResourceKind::Buffer;
  ib.size_bytes = sizeof(indices_u16);
  ib.storage.resize(sizeof(indices_u16));
  std::memcpy(ib.storage.data(), indices_u16, sizeof(indices_u16));

  dev.streams[0] = {&vb0, 0, sizeof(Vec4)};
  dev.streams[1] = {&vb1, 0, sizeof(InstanceData)};
  dev.index_buffer = &ib;
  dev.index_format = static_cast<D3DDDIFORMAT>(101); // D3DFMT_INDEX16
  dev.index_offset_bytes = 0;

  dev.stream_source_freq[0] = kD3DStreamSourceIndexedData | 3u;
  dev.stream_source_freq[1] = kD3DStreamSourceInstanceData | 2u;

  const HRESULT hr = aerogpu::device_draw_indexed_primitive(
      hDevice,
      D3DDDIPT_TRIANGLELIST,
      /*base_vertex=*/0,
      /*min_index=*/0,
      /*num_vertices=*/3,
      /*start_index=*/0,
      /*primitive_count=*/1);
  assert(hr == S_OK);

  dev.cmd.finalize();
  const uint8_t* buf = dev.cmd.data();
  const size_t len = dev.cmd.size();

  const aerogpu_cmd_upload_resource* upload1 =
      FindLastUploadForHandle(buf, len, dev.instancing_vertex_buffers[1]->handle);
  assert(upload1 != nullptr);

  // Expanded stream1: inst0 x3, inst0 x3, inst1 x3 (because divisor=2).
  const size_t expected_vb1_bytes = sizeof(InstanceData) * 9;
  assert(upload1->offset_bytes == 0);
  assert(upload1->size_bytes == expected_vb1_bytes);
  std::vector<uint8_t> expected_vb1(expected_vb1_bytes, 0);
  for (int v = 0; v < 3; ++v) {
    std::memcpy(expected_vb1.data() + (size_t)v * sizeof(InstanceData), &inst_elems[0], sizeof(InstanceData));
  }
  for (int v = 0; v < 3; ++v) {
    std::memcpy(expected_vb1.data() + (size_t)(3 + v) * sizeof(InstanceData), &inst_elems[0], sizeof(InstanceData));
  }
  for (int v = 0; v < 3; ++v) {
    std::memcpy(expected_vb1.data() + (size_t)(6 + v) * sizeof(InstanceData), &inst_elems[1], sizeof(InstanceData));
  }
  const uint8_t* payload1 = reinterpret_cast<const uint8_t*>(upload1) + sizeof(*upload1);
  assert(std::memcmp(payload1, expected_vb1.data(), expected_vb1.size()) == 0);
}

void TestIndexedTriangleListIgnoresMinIndexHint() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  aerogpu::Shader vs{};
  aerogpu::Shader ps{};
  BindTestShaders(dev, vs, ps);

  aerogpu::VertexDecl decl{};
  BindTestDecl(dev, decl);

  // Stream 0: per-vertex positions.
  const Vec4 vertices[8] = {
      {0.0f, 0.0f, 0.0f, 1.0f}, {1.0f, 0.0f, 0.0f, 1.0f}, {2.0f, 0.0f, 0.0f, 1.0f},
      {3.0f, 0.0f, 0.0f, 1.0f}, {4.0f, 0.0f, 0.0f, 1.0f}, {5.0f, 0.0f, 0.0f, 1.0f},
      {6.0f, 0.0f, 0.0f, 1.0f}, {7.0f, 0.0f, 0.0f, 1.0f},
  };
  aerogpu::Resource vb0{};
  vb0.handle = 0x260;
  vb0.kind = aerogpu::ResourceKind::Buffer;
  vb0.size_bytes = sizeof(vertices);
  vb0.storage.resize(sizeof(vertices));
  std::memcpy(vb0.storage.data(), vertices, sizeof(vertices));

  // Stream 1: per-instance data.
  const InstanceData instances[2] = {
      {{10.0f, 0.0f, 0.0f, 0.0f}, {1.0f, 0.0f, 0.0f, 1.0f}},
      {{20.0f, 0.0f, 0.0f, 0.0f}, {0.0f, 1.0f, 0.0f, 1.0f}},
  };
  aerogpu::Resource vb1{};
  vb1.handle = 0x261;
  vb1.kind = aerogpu::ResourceKind::Buffer;
  vb1.size_bytes = sizeof(instances);
  vb1.storage.resize(sizeof(instances));
  std::memcpy(vb1.storage.data(), instances, sizeof(instances));

  // Index buffer references vertices 5, 6, 7 (not 0,1,2).
  const uint16_t indices_u16[3] = {5, 6, 7};
  aerogpu::Resource ib{};
  ib.handle = 0x262;
  ib.kind = aerogpu::ResourceKind::Buffer;
  ib.size_bytes = sizeof(indices_u16);
  ib.storage.resize(sizeof(indices_u16));
  std::memcpy(ib.storage.data(), indices_u16, sizeof(indices_u16));

  dev.streams[0] = {&vb0, 0, sizeof(Vec4)};
  dev.streams[1] = {&vb1, 0, sizeof(InstanceData)};
  dev.index_buffer = &ib;
  dev.index_format = static_cast<D3DDDIFORMAT>(101); // D3DFMT_INDEX16
  dev.index_offset_bytes = 0;

  dev.stream_source_freq[0] = kD3DStreamSourceIndexedData | 2u;
  dev.stream_source_freq[1] = kD3DStreamSourceInstanceData | 1u;

  // Pass incorrect min_index/num_vertices hints; the instancing emulation should
  // derive the actual index range from the index buffer instead of failing.
  const HRESULT hr = aerogpu::device_draw_indexed_primitive(
      hDevice,
      D3DDDIPT_TRIANGLELIST,
      /*base_vertex=*/0,
      /*min_index=*/6,
      /*num_vertices=*/2,
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
  const aerogpu_cmd_upload_resource* upload_ib = FindLastUploadForHandle(buf, len, dev.up_index_buffer->handle);

  assert(upload0 != nullptr);
  assert(upload1 != nullptr);
  assert(upload_ib != nullptr);

  // Effective range is [5,8) => 3 vertices. Stream0 expanded upload should be
  // [v5,v6,v7,v5,v6,v7].
  const size_t expected_vb0_bytes = sizeof(Vec4) * 6;
  assert(upload0->size_bytes == expected_vb0_bytes);
  std::vector<uint8_t> expected_vb0(expected_vb0_bytes, 0);
  std::memcpy(expected_vb0.data() + 0, &vertices[5], sizeof(Vec4) * 3);
  std::memcpy(expected_vb0.data() + sizeof(Vec4) * 3, &vertices[5], sizeof(Vec4) * 3);
  const uint8_t* payload0 = reinterpret_cast<const uint8_t*>(upload0) + sizeof(*upload0);
  assert(std::memcmp(payload0, expected_vb0.data(), expected_vb0.size()) == 0);

  // Stream1 expanded upload should be [inst0 x3, inst1 x3].
  const size_t expected_vb1_bytes = sizeof(InstanceData) * 6;
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

  // Index upload should still be u32 [0,1,2,3,4,5] after rebasing to the
  // derived min index and concatenating instances.
  const uint32_t expected_indices_u32[6] = {0, 1, 2, 3, 4, 5};
  assert(upload_ib->size_bytes == sizeof(expected_indices_u32));
  const uint8_t* payload_ib = reinterpret_cast<const uint8_t*>(upload_ib) + sizeof(*upload_ib);
  assert(std::memcmp(payload_ib, expected_indices_u32, sizeof(expected_indices_u32)) == 0);
}

void TestIndexedTriangleListNegativeBaseVertex() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  aerogpu::Shader vs{};
  aerogpu::Shader ps{};
  BindTestShaders(dev, vs, ps);

  aerogpu::VertexDecl decl{};
  BindTestDecl(dev, decl);

  // Stream 0 has a positive byte offset and a negative base_vertex, which is a
  // valid D3D9 pattern (indices can reference vertices "before" the stream
  // offset).
  const Vec4 vertices[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f},
      {1.0f, 0.0f, 0.0f, 1.0f},
      {0.0f, 1.0f, 0.0f, 1.0f},
  };
  aerogpu::Resource vb0{};
  vb0.handle = 0x250;
  vb0.kind = aerogpu::ResourceKind::Buffer;
  vb0.size_bytes = sizeof(vertices);
  vb0.storage.resize(sizeof(vertices));
  std::memcpy(vb0.storage.data(), vertices, sizeof(vertices));

  const InstanceData instances[2] = {
      {{10.0f, 0.0f, 0.0f, 0.0f}, {1.0f, 0.0f, 0.0f, 1.0f}},
      {{20.0f, 0.0f, 0.0f, 0.0f}, {0.0f, 1.0f, 0.0f, 1.0f}},
  };
  aerogpu::Resource vb1{};
  vb1.handle = 0x251;
  vb1.kind = aerogpu::ResourceKind::Buffer;
  vb1.size_bytes = sizeof(instances);
  vb1.storage.resize(sizeof(instances));
  std::memcpy(vb1.storage.data(), instances, sizeof(instances));

  const uint16_t indices_u16[3] = {0, 1, 2};
  aerogpu::Resource ib{};
  ib.handle = 0x252;
  ib.kind = aerogpu::ResourceKind::Buffer;
  ib.size_bytes = sizeof(indices_u16);
  ib.storage.resize(sizeof(indices_u16));
  std::memcpy(ib.storage.data(), indices_u16, sizeof(indices_u16));

  dev.streams[0] = {&vb0, static_cast<uint32_t>(sizeof(Vec4) * 2), sizeof(Vec4)};
  dev.streams[1] = {&vb1, 0, sizeof(InstanceData)};
  dev.index_buffer = &ib;
  dev.index_format = static_cast<D3DDDIFORMAT>(101); // D3DFMT_INDEX16
  dev.index_offset_bytes = 0;

  dev.stream_source_freq[0] = kD3DStreamSourceIndexedData | 2u;
  dev.stream_source_freq[1] = kD3DStreamSourceInstanceData | 1u;

  const HRESULT hr = aerogpu::device_draw_indexed_primitive(
      hDevice,
      D3DDDIPT_TRIANGLELIST,
      /*base_vertex=*/-2,
      /*min_index=*/0,
      /*num_vertices=*/3,
      /*start_index=*/0,
      /*primitive_count=*/1);
  assert(hr == S_OK);

  dev.cmd.finalize();
  const uint8_t* buf = dev.cmd.data();
  const size_t len = dev.cmd.size();

  assert(dev.instancing_vertex_buffers[0] != nullptr);
  const aerogpu_cmd_upload_resource* upload0 =
      FindLastUploadForHandle(buf, len, dev.instancing_vertex_buffers[0]->handle);
  assert(upload0 != nullptr);

  // Validate expanded stream 0 upload: 2 instances => [v0,v1,v2,v0,v1,v2].
  const size_t expected_vb0_bytes = sizeof(vertices) * 2;
  assert(upload0->offset_bytes == 0);
  assert(upload0->size_bytes == expected_vb0_bytes);
  std::vector<uint8_t> expected_vb0(expected_vb0_bytes, 0);
  std::memcpy(expected_vb0.data() + 0, vertices, sizeof(vertices));
  std::memcpy(expected_vb0.data() + sizeof(vertices), vertices, sizeof(vertices));
  const uint8_t* payload0 = reinterpret_cast<const uint8_t*>(upload0) + sizeof(*upload0);
  assert(std::memcmp(payload0, expected_vb0.data(), expected_vb0.size()) == 0);
}

void TestNonIndexedTriangleListBasic() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  aerogpu::Shader vs{};
  aerogpu::Shader ps{};
  BindTestShaders(dev, vs, ps);

  aerogpu::VertexDecl decl{};
  BindTestDecl(dev, decl);

  const Vec4 vertices[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f},
      {1.0f, 0.0f, 0.0f, 1.0f},
      {0.0f, 1.0f, 0.0f, 1.0f},
  };
  aerogpu::Resource vb0{};
  vb0.handle = 0x300;
  vb0.kind = aerogpu::ResourceKind::Buffer;
  vb0.size_bytes = sizeof(vertices);
  vb0.storage.resize(sizeof(vertices));
  std::memcpy(vb0.storage.data(), vertices, sizeof(vertices));

  const InstanceData instances[2] = {
      {{10.0f, 0.0f, 0.0f, 0.0f}, {1.0f, 0.0f, 0.0f, 1.0f}},
      {{20.0f, 0.0f, 0.0f, 0.0f}, {0.0f, 1.0f, 0.0f, 1.0f}},
  };
  aerogpu::Resource vb1{};
  vb1.handle = 0x301;
  vb1.kind = aerogpu::ResourceKind::Buffer;
  vb1.size_bytes = sizeof(instances);
  vb1.storage.resize(sizeof(instances));
  std::memcpy(vb1.storage.data(), instances, sizeof(instances));

  dev.streams[0] = {&vb0, 0, sizeof(Vec4)};
  dev.streams[1] = {&vb1, 0, sizeof(InstanceData)};

  dev.stream_source_freq[0] = kD3DStreamSourceIndexedData | 2u;
  dev.stream_source_freq[1] = kD3DStreamSourceInstanceData | 1u;

  const HRESULT hr = aerogpu::device_draw_primitive(
      hDevice,
      D3DDDIPT_TRIANGLELIST,
      /*start_vertex=*/0,
      /*primitive_count=*/1);
  assert(hr == S_OK);

  dev.cmd.finalize();
  const uint8_t* buf = dev.cmd.data();
  const size_t len = dev.cmd.size();

  const auto draws = FindAllCmds<aerogpu_cmd_draw>(buf, len, AEROGPU_CMD_DRAW);
  assert(draws.size() == 1);
  assert(draws[0]->first_vertex == 0);
  assert(draws[0]->vertex_count == 6);

  const aerogpu_cmd_upload_resource* upload1 =
      FindLastUploadForHandle(buf, len, dev.instancing_vertex_buffers[1]->handle);
  assert(upload1 != nullptr);

  // Validate expanded stream 1 upload size: 2 instances * 3 vertices.
  const size_t expected_vb1_bytes = sizeof(InstanceData) * 6;
  assert(upload1->size_bytes == expected_vb1_bytes);
}

void TestNonIndexedTriangleStripDrawsPerInstance() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  aerogpu::Shader vs{};
  aerogpu::Shader ps{};
  BindTestShaders(dev, vs, ps);

  aerogpu::VertexDecl decl{};
  BindTestDecl(dev, decl);

  // Triangle strip with primitive_count=2 uses 4 vertices.
  const Vec4 vertices[4] = {
      {0.0f, 0.0f, 0.0f, 1.0f},
      {1.0f, 0.0f, 0.0f, 1.0f},
      {0.0f, 1.0f, 0.0f, 1.0f},
      {1.0f, 1.0f, 0.0f, 1.0f},
  };
  aerogpu::Resource vb0{};
  vb0.handle = 0x400;
  vb0.kind = aerogpu::ResourceKind::Buffer;
  vb0.size_bytes = sizeof(vertices);
  vb0.storage.resize(sizeof(vertices));
  std::memcpy(vb0.storage.data(), vertices, sizeof(vertices));

  const InstanceData instances[2] = {
      {{10.0f, 0.0f, 0.0f, 0.0f}, {1.0f, 0.0f, 0.0f, 1.0f}},
      {{20.0f, 0.0f, 0.0f, 0.0f}, {0.0f, 1.0f, 0.0f, 1.0f}},
  };
  aerogpu::Resource vb1{};
  vb1.handle = 0x401;
  vb1.kind = aerogpu::ResourceKind::Buffer;
  vb1.size_bytes = sizeof(instances);
  vb1.storage.resize(sizeof(instances));
  std::memcpy(vb1.storage.data(), instances, sizeof(instances));

  dev.streams[0] = {&vb0, 0, sizeof(Vec4)};
  dev.streams[1] = {&vb1, 0, sizeof(InstanceData)};

  dev.stream_source_freq[0] = kD3DStreamSourceIndexedData | 2u;
  dev.stream_source_freq[1] = kD3DStreamSourceInstanceData | 1u;

  const HRESULT hr = aerogpu::device_draw_primitive(
      hDevice,
      D3DDDIPT_TRIANGLESTRIP,
      /*start_vertex=*/0,
      /*primitive_count=*/2);
  assert(hr == S_OK);

  dev.cmd.finalize();
  const uint8_t* buf = dev.cmd.data();
  const size_t len = dev.cmd.size();

  const auto draws = FindAllCmds<aerogpu_cmd_draw>(buf, len, AEROGPU_CMD_DRAW);
  assert(draws.size() == 2);
  assert(draws[0]->first_vertex == 0);
  assert(draws[0]->vertex_count == 4);
  assert(draws[1]->first_vertex == 0);
  assert(draws[1]->vertex_count == 4);

  // Per-instance stream1 data is uploaded once per instance.
  assert(dev.instancing_vertex_buffers[1] != nullptr);
  const auto uploads = FindAllCmds<aerogpu_cmd_upload_resource>(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE);
  std::vector<const aerogpu_cmd_upload_resource*> vb1_uploads;
  for (const auto* u : uploads) {
    if (u->resource_handle == dev.instancing_vertex_buffers[1]->handle) {
      vb1_uploads.push_back(u);
    }
  }
  assert(vb1_uploads.size() == 2);
  const size_t expected_vb1_bytes = sizeof(InstanceData) * 4;
  for (size_t i = 0; i < 2; ++i) {
    const auto* upload = vb1_uploads[i];
    assert(upload->offset_bytes == 0);
    assert(upload->size_bytes == expected_vb1_bytes);
    std::vector<uint8_t> expected(expected_vb1_bytes, 0);
    for (int v = 0; v < 4; ++v) {
      std::memcpy(expected.data() + (size_t)v * sizeof(InstanceData), &instances[i], sizeof(InstanceData));
    }
    const uint8_t* payload = reinterpret_cast<const uint8_t*>(upload) + sizeof(*upload);
    assert(std::memcmp(payload, expected.data(), expected.size()) == 0);
  }
}

void TestIndexedTriangleStripUsesBaseVertexNoIndexExpansion() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  aerogpu::Shader vs{};
  aerogpu::Shader ps{};
  BindTestShaders(dev, vs, ps);

  aerogpu::VertexDecl decl{};
  BindTestDecl(dev, decl);

  // Triangle strip with primitive_count=2 uses 4 indices (and 4 vertices).
  const Vec4 vertices[4] = {
      {0.0f, 0.0f, 0.0f, 1.0f},
      {1.0f, 0.0f, 0.0f, 1.0f},
      {0.0f, 1.0f, 0.0f, 1.0f},
      {1.0f, 1.0f, 0.0f, 1.0f},
  };
  aerogpu::Resource vb0{};
  vb0.handle = 0x500;
  vb0.kind = aerogpu::ResourceKind::Buffer;
  vb0.size_bytes = sizeof(vertices);
  vb0.storage.resize(sizeof(vertices));
  std::memcpy(vb0.storage.data(), vertices, sizeof(vertices));

  const InstanceData instances[2] = {
      {{10.0f, 0.0f, 0.0f, 0.0f}, {1.0f, 0.0f, 0.0f, 1.0f}},
      {{20.0f, 0.0f, 0.0f, 0.0f}, {0.0f, 1.0f, 0.0f, 1.0f}},
  };
  aerogpu::Resource vb1{};
  vb1.handle = 0x501;
  vb1.kind = aerogpu::ResourceKind::Buffer;
  vb1.size_bytes = sizeof(instances);
  vb1.storage.resize(sizeof(instances));
  std::memcpy(vb1.storage.data(), instances, sizeof(instances));

  const uint16_t indices_u16[4] = {0, 1, 2, 3};
  aerogpu::Resource ib{};
  ib.handle = 0x502;
  ib.kind = aerogpu::ResourceKind::Buffer;
  ib.size_bytes = sizeof(indices_u16);
  ib.storage.resize(sizeof(indices_u16));
  std::memcpy(ib.storage.data(), indices_u16, sizeof(indices_u16));

  dev.streams[0] = {&vb0, 0, sizeof(Vec4)};
  dev.streams[1] = {&vb1, 0, sizeof(InstanceData)};
  dev.index_buffer = &ib;
  dev.index_format = static_cast<D3DDDIFORMAT>(101); // D3DFMT_INDEX16
  dev.index_offset_bytes = 0;

  dev.stream_source_freq[0] = kD3DStreamSourceIndexedData | 2u;
  dev.stream_source_freq[1] = kD3DStreamSourceInstanceData | 1u;

  const HRESULT hr = aerogpu::device_draw_indexed_primitive(
      hDevice,
      D3DDDIPT_TRIANGLESTRIP,
      /*base_vertex=*/0,
      /*min_index=*/0,
      /*num_vertices=*/4,
      /*start_index=*/0,
      /*primitive_count=*/2);
  assert(hr == S_OK);

  // Strip instancing is executed as one draw per instance. The app's index
  // buffer is reused (no expanded index upload is required).
  assert(dev.up_index_buffer == nullptr);

  dev.cmd.finalize();
  const uint8_t* buf = dev.cmd.data();
  const size_t len = dev.cmd.size();

  const auto draws = FindAllCmds<aerogpu_cmd_draw_indexed>(buf, len, AEROGPU_CMD_DRAW_INDEXED);
  assert(draws.size() == 2);

  assert(draws[0]->index_count == 4);
  assert(draws[0]->first_index == 0);
  assert(draws[0]->base_vertex == 0);

  assert(draws[1]->index_count == 4);
  assert(draws[1]->first_index == 0);
  assert(draws[1]->base_vertex == 0);

  // Per-instance stream1 data is uploaded once per instance.
  assert(dev.instancing_vertex_buffers[1] != nullptr);
  const auto uploads = FindAllCmds<aerogpu_cmd_upload_resource>(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE);
  std::vector<const aerogpu_cmd_upload_resource*> vb1_uploads;
  for (const auto* u : uploads) {
    if (u->resource_handle == dev.instancing_vertex_buffers[1]->handle) {
      vb1_uploads.push_back(u);
    }
  }
  assert(vb1_uploads.size() == 2);
  const size_t expected_vb1_bytes = sizeof(InstanceData) * 4;
  for (size_t i = 0; i < 2; ++i) {
    const auto* upload = vb1_uploads[i];
    assert(upload->offset_bytes == 0);
    assert(upload->size_bytes == expected_vb1_bytes);
    std::vector<uint8_t> expected(expected_vb1_bytes, 0);
    for (int v = 0; v < 4; ++v) {
      std::memcpy(expected.data() + (size_t)v * sizeof(InstanceData), &instances[i], sizeof(InstanceData));
    }
    const uint8_t* payload = reinterpret_cast<const uint8_t*>(upload) + sizeof(*upload);
    assert(std::memcmp(payload, expected.data(), expected.size()) == 0);
  }
}

void TestIndexedTriangleStripNegativeBaseVertex() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  aerogpu::Shader vs{};
  aerogpu::Shader ps{};
  BindTestShaders(dev, vs, ps);

  aerogpu::VertexDecl decl{};
  BindTestDecl(dev, decl);

  // Triangle strip with primitive_count=2 uses 4 indices (and 4 vertices).
  const Vec4 vertices[4] = {
      {0.0f, 0.0f, 0.0f, 1.0f},
      {1.0f, 0.0f, 0.0f, 1.0f},
      {0.0f, 1.0f, 0.0f, 1.0f},
      {1.0f, 1.0f, 0.0f, 1.0f},
  };
  aerogpu::Resource vb0{};
  vb0.handle = 0x600;
  vb0.kind = aerogpu::ResourceKind::Buffer;
  vb0.size_bytes = sizeof(vertices);
  vb0.storage.resize(sizeof(vertices));
  std::memcpy(vb0.storage.data(), vertices, sizeof(vertices));

  const InstanceData instances[2] = {
      {{10.0f, 0.0f, 0.0f, 0.0f}, {1.0f, 0.0f, 0.0f, 1.0f}},
      {{20.0f, 0.0f, 0.0f, 0.0f}, {0.0f, 1.0f, 0.0f, 1.0f}},
  };
  aerogpu::Resource vb1{};
  vb1.handle = 0x601;
  vb1.kind = aerogpu::ResourceKind::Buffer;
  vb1.size_bytes = sizeof(instances);
  vb1.storage.resize(sizeof(instances));
  std::memcpy(vb1.storage.data(), instances, sizeof(instances));

  const uint16_t indices_u16[4] = {0, 1, 2, 3};
  aerogpu::Resource ib{};
  ib.handle = 0x602;
  ib.kind = aerogpu::ResourceKind::Buffer;
  ib.size_bytes = sizeof(indices_u16);
  ib.storage.resize(sizeof(indices_u16));
  std::memcpy(ib.storage.data(), indices_u16, sizeof(indices_u16));

  // Base vertex -1 combined with a +1 vertex offset yields an effective base of 0.
  dev.streams[0] = {&vb0, static_cast<uint32_t>(sizeof(Vec4)), sizeof(Vec4)};
  dev.streams[1] = {&vb1, 0, sizeof(InstanceData)};
  dev.index_buffer = &ib;
  dev.index_format = static_cast<D3DDDIFORMAT>(101); // D3DFMT_INDEX16
  dev.index_offset_bytes = 0;

  dev.stream_source_freq[0] = kD3DStreamSourceIndexedData | 2u;
  dev.stream_source_freq[1] = kD3DStreamSourceInstanceData | 1u;

  const HRESULT hr = aerogpu::device_draw_indexed_primitive(
      hDevice,
      D3DDDIPT_TRIANGLESTRIP,
      /*base_vertex=*/-1,
      /*min_index=*/0,
      /*num_vertices=*/4,
      /*start_index=*/0,
      /*primitive_count=*/2);
  assert(hr == S_OK);

  // Strip instancing reuses the app index buffer by adjusting stream offsets; no
  // expanded index upload is required.
  assert(dev.up_index_buffer == nullptr);

  dev.cmd.finalize();
  const uint8_t* buf = dev.cmd.data();
  const size_t len = dev.cmd.size();

  const auto draws = FindAllCmds<aerogpu_cmd_draw_indexed>(buf, len, AEROGPU_CMD_DRAW_INDEXED);
  assert(draws.size() == 2);
  assert(draws[0]->base_vertex == 0);
  assert(draws[1]->base_vertex == 0);

  // The per-vertex stream should have been rebound with offset_bytes=0 for the
  // instanced draws, then restored to the original offset (sizeof(Vec4)).
  const auto vbs = FindAllCmds<aerogpu_cmd_set_vertex_buffers>(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS);
  std::vector<const aerogpu_cmd_set_vertex_buffers*> vb0_cmds;
  for (const auto* cmd : vbs) {
    if (cmd->start_slot == 0 && cmd->buffer_count == 1) {
      vb0_cmds.push_back(cmd);
    }
  }
  assert(vb0_cmds.size() == 2);
  const auto* bind0 =
      reinterpret_cast<const aerogpu_vertex_buffer_binding*>(reinterpret_cast<const uint8_t*>(vb0_cmds[0]) +
                                                            sizeof(*vb0_cmds[0]));
  const auto* bind1 =
      reinterpret_cast<const aerogpu_vertex_buffer_binding*>(reinterpret_cast<const uint8_t*>(vb0_cmds[1]) +
                                                            sizeof(*vb0_cmds[1]));
  assert(bind0->buffer == vb0.handle);
  assert(bind0->offset_bytes == 0);
  assert(bind1->buffer == vb0.handle);
  assert(bind1->offset_bytes == sizeof(Vec4));
}

} // namespace

int main() {
  TestIndexedTriangleListBasic();
  TestIndexedTriangleListInstancedDivisor();
  TestIndexedTriangleListIgnoresMinIndexHint();
  TestIndexedTriangleListNegativeBaseVertex();
  TestNonIndexedTriangleListBasic();
  TestNonIndexedTriangleStripDrawsPerInstance();
  TestIndexedTriangleStripUsesBaseVertexNoIndexExpansion();
  TestIndexedTriangleStripNegativeBaseVertex();
  return 0;
}
