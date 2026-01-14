#include <cassert>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <vector>

#include "aerogpu_d3d9_objects.h"

namespace aerogpu {

// DDI entrypoint under test (implemented in aerogpu_d3d9_driver.cpp).
HRESULT AEROGPU_D3D9_CALL device_process_vertices(
    D3DDDI_HDEVICE hDevice,
    const D3DDDIARG_PROCESSVERTICES* pProcessVertices);

namespace {

// Keep local copies of the handful of D3DVERTEXELEMENT9 constants we need so the
// test can build on non-Windows hosts without the D3D9 SDK/WDK headers.
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

constexpr uint8_t kDeclTypeFloat2 = 1;
constexpr uint8_t kDeclTypeFloat4 = 3;
constexpr uint8_t kDeclTypeD3dColor = 4;
constexpr uint8_t kDeclTypeUnused = 17;
constexpr uint8_t kDeclMethodDefault = 0;
constexpr uint8_t kDeclUsageTexCoord = 5;
constexpr uint8_t kDeclUsagePositionT = 9;
constexpr uint8_t kDeclUsageColor = 10;

constexpr uint32_t kFvfXyz = 0x00000002u;
constexpr uint32_t kFvfDiffuse = 0x00000040u;
constexpr uint32_t kFvfTex1 = 0x00000100u;

float read_f32(const std::vector<uint8_t>& bytes, size_t offset) {
  assert(offset + 4 <= bytes.size());
  float v = 0.0f;
  std::memcpy(&v, bytes.data() + offset, 4);
  return v;
}

void write_f32(std::vector<uint8_t>& bytes, size_t offset, float v) {
  assert(offset + 4 <= bytes.size());
  std::memcpy(bytes.data() + offset, &v, 4);
}

void write_u32(std::vector<uint8_t>& bytes, size_t offset, uint32_t v) {
  assert(offset + 4 <= bytes.size());
  std::memcpy(bytes.data() + offset, &v, 4);
}

void test_xyz_diffuse() {
  Adapter adapter;
  Device dev(&adapter);

  dev.fvf = kFvfXyz | kFvfDiffuse;
  dev.viewport = {0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f};

  // WORLD translate +1 in X (row-major, row-vector convention).
  dev.transform_matrices[256][12] = 1.0f;

  // Source VB: XYZ|DIFFUSE (float3 + u32) = 16 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 16;
  src.storage.resize(16);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_u32(src.storage, 12, 0xAABBCCDDu);

  // Destination VB: XYZRHW|DIFFUSE (float4 + u32) = 20 bytes.
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 20;
  dst.storage.resize(20);

  // Destination vertex decl: positionT float4 at 0, color at 16.
  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  dev.streams[0].vb = &src;
  dev.streams[0].offset_bytes = 0;
  dev.streams[0].stride_bytes = 16;

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 20;

  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  // With identity view/proj and viewport (0,0,100,100):
  // object position (0,0,0) translated to (1,0,0) => ndc_x=1 => screen x=(1+1)/2*100-0.5 = 99.5
  const float x = read_f32(dst.storage, 0);
  const float y = read_f32(dst.storage, 4);
  const float z = read_f32(dst.storage, 8);
  const float rhw = read_f32(dst.storage, 12);
  assert(std::fabs(x - 99.5f) < 1e-4f);
  assert(std::fabs(y - 49.5f) < 1e-4f);
  assert(std::fabs(z - 0.0f) < 1e-4f);
  assert(std::fabs(rhw - 1.0f) < 1e-4f);

  uint32_t diffuse = 0;
  std::memcpy(&diffuse, dst.storage.data() + 16, 4);
  assert(diffuse == 0xAABBCCDDu);
}

void test_xyz_diffuse_tex1() {
  Adapter adapter;
  Device dev(&adapter);

  dev.fvf = kFvfXyz | kFvfDiffuse | kFvfTex1;
  dev.viewport = {0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f};
  dev.transform_matrices[256][12] = 1.0f;

  // Source VB: XYZ|DIFFUSE|TEX1 = float3 + u32 + float2 = 24 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 24;
  src.storage.resize(24);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_u32(src.storage, 12, 0x11223344u);
  write_f32(src.storage, 16, 0.25f);
  write_f32(src.storage, 20, 0.75f);

  // Destination VB: XYZRHW|DIFFUSE|TEX1 = float4 + u32 + float2 = 28 bytes.
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 28;
  dst.storage.resize(28);

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  dev.streams[0].vb = &src;
  dev.streams[0].offset_bytes = 0;
  dev.streams[0].stride_bytes = 24;

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 28;

  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  const float x = read_f32(dst.storage, 0);
  const float y = read_f32(dst.storage, 4);
  const float z = read_f32(dst.storage, 8);
  const float rhw = read_f32(dst.storage, 12);
  assert(std::fabs(x - 99.5f) < 1e-4f);
  assert(std::fabs(y - 49.5f) < 1e-4f);
  assert(std::fabs(z - 0.0f) < 1e-4f);
  assert(std::fabs(rhw - 1.0f) < 1e-4f);

  uint32_t diffuse = 0;
  std::memcpy(&diffuse, dst.storage.data() + 16, 4);
  assert(diffuse == 0x11223344u);

  const float u = read_f32(dst.storage, 20);
  const float v = read_f32(dst.storage, 24);
  assert(std::fabs(u - 0.25f) < 1e-4f);
  assert(std::fabs(v - 0.75f) < 1e-4f);
}

void test_xyz_diffuse_offsets() {
  Adapter adapter;
  Device dev(&adapter);

  dev.fvf = kFvfXyz | kFvfDiffuse;
  dev.viewport = {0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f};
  dev.transform_matrices[256][12] = 1.0f;

  // Source VB: 2 vertices.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 32;
  src.storage.resize(32);
  // Vertex 0 (ignored).
  write_f32(src.storage, 0, 123.0f);
  write_f32(src.storage, 4, 456.0f);
  write_f32(src.storage, 8, 789.0f);
  write_u32(src.storage, 12, 0x11111111u);
  // Vertex 1 (used).
  write_f32(src.storage, 16, 0.0f);
  write_f32(src.storage, 20, 0.0f);
  write_f32(src.storage, 24, 0.0f);
  write_u32(src.storage, 28, 0xAABBCCDDu);

  // Destination VB: 2 vertices of XYZRHW|DIFFUSE (20 bytes each).
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 40;
  dst.storage.resize(40);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  dev.streams[0].vb = &src;
  dev.streams[0].offset_bytes = 0;
  dev.streams[0].stride_bytes = 16;

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 1;
  pv.DestIndex = 1;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 20;

  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  // First vertex should remain untouched (sentinel pattern).
  for (size_t i = 0; i < 20; ++i) {
    assert(dst.storage[i] == 0xCD);
  }

  // Second vertex should contain transformed output.
  const float x = read_f32(dst.storage, 20);
  const float y = read_f32(dst.storage, 24);
  const float z = read_f32(dst.storage, 28);
  const float rhw = read_f32(dst.storage, 32);
  assert(std::fabs(x - 99.5f) < 1e-4f);
  assert(std::fabs(y - 49.5f) < 1e-4f);
  assert(std::fabs(z - 0.0f) < 1e-4f);
  assert(std::fabs(rhw - 1.0f) < 1e-4f);

  uint32_t diffuse = 0;
  std::memcpy(&diffuse, dst.storage.data() + 36, 4);
  assert(diffuse == 0xAABBCCDDu);
}

} // namespace
} // namespace aerogpu

int main() {
  aerogpu::test_xyz_diffuse();
  aerogpu::test_xyz_diffuse_tex1();
  aerogpu::test_xyz_diffuse_offsets();
  return 0;
}
