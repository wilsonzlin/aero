#ifdef NDEBUG
  #undef NDEBUG
#endif
#include <cassert>
#include <cmath>
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <vector>

#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_test_entrypoints.h"

namespace aerogpu {

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
constexpr uint8_t kDeclTypeFloat3 = 2;
constexpr uint8_t kDeclTypeFloat4 = 3;
constexpr uint8_t kDeclTypeD3dColor = 4;
constexpr uint8_t kDeclTypeUnused = 17;
constexpr uint8_t kDeclMethodDefault = 0;
constexpr uint8_t kDeclUsageTexCoord = 5;
constexpr uint8_t kDeclUsagePositionT = 9;
constexpr uint8_t kDeclUsageColor = 10;

constexpr uint32_t kFvfXyz = 0x00000002u;
constexpr uint32_t kFvfXyzw = 0x00004002u;
constexpr uint32_t kFvfXyzrhw = 0x00000004u;
constexpr uint32_t kFvfDiffuse = 0x00000040u;
constexpr uint32_t kFvfTex1 = 0x00000100u;

// D3DPV_* flags for IDirect3DDevice9::ProcessVertices.
constexpr uint32_t kPvDoNotCopyData = 0x00000001u;

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

void write_pattern(std::vector<uint8_t>& bytes, size_t offset, size_t len, uint8_t v) {
  assert(offset + len <= bytes.size());
  std::memset(bytes.data() + offset, v, len);
}

D3DDDI_HDEVICE make_device_handle(Device* dev) {
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = dev;
  return hDevice;
}

D3DDDI_HRESOURCE make_resource_handle(Resource* res) {
  D3DDDI_HRESOURCE hRes{};
  hRes.pDrvPrivate = res;
  return hRes;
}

D3DMATRIX make_identity_matrix() {
  D3DMATRIX m{};
  m.m[0][0] = 1.0f;
  m.m[1][1] = 1.0f;
  m.m[2][2] = 1.0f;
  m.m[3][3] = 1.0f;
  return m;
}

void set_fvf_or_die(D3DDDI_HDEVICE hDevice, uint32_t fvf) {
  const HRESULT hr = device_set_fvf(hDevice, fvf);
  if (hr != S_OK) {
    // Help diagnose DDI validation failures (portable tests run without the
    // D3D9 runtime, so this is our only breadcrumb).
    std::fprintf(stderr, "device_set_fvf failed: fvf=0x%08x hr=0x%08x\n", fvf, static_cast<uint32_t>(hr));
  }
  assert(hr == S_OK);
}

void set_viewport_or_die(D3DDDI_HDEVICE hDevice, float x, float y, float w, float h, float minz, float maxz) {
  const D3DDDIVIEWPORTINFO vp = {x, y, w, h, minz, maxz};
  const HRESULT hr = device_set_viewport(hDevice, &vp);
  assert(hr == S_OK);
}

void set_world_translate_x_or_die(D3DDDI_HDEVICE hDevice, float tx) {
  D3DMATRIX world = make_identity_matrix();
  // Row-major, row-vector convention (matches `Device::transform_matrices` layout).
  world.m[3][0] = tx;
  const HRESULT hr = device_set_transform(hDevice, static_cast<D3DTRANSFORMSTATETYPE>(D3DTS_WORLD), &world);
  assert(hr == S_OK);
}

void set_stream0_or_die(D3DDDI_HDEVICE hDevice, Resource* vb, uint32_t stride_bytes, uint32_t offset_bytes = 0) {
  D3DDDI_HRESOURCE hVb = make_resource_handle(vb);
  const HRESULT hr = device_set_stream_source(hDevice, 0, hVb, offset_bytes, stride_bytes);
  assert(hr == S_OK);
}

void test_xyz_diffuse() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  // WORLD translate +1 in X.
  set_world_translate_x_or_die(hDevice, 1.0f);

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

  set_stream0_or_die(hDevice, &src, 16);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  // Some runtimes may omit DestStride; ensure we infer it from the destination
  // vertex declaration.
  pv.DestStride = 0;

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

void test_xyz_diffuse_dest_decl_position_usage0() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 16;
  src.storage.resize(16);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_u32(src.storage, 12, 0xAABBCCDDu);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 20;
  dst.storage.resize(20);

  // Destination vertex decl: some runtimes synthesize decls with Usage=0 for
  // position rather than POSITIONT. ProcessVertices should still accept it.
  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, /*POSITION=*/0, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 16);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

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
  assert(diffuse == 0xAABBCCDDu);
}

void test_process_vertices_device_lost() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  const HRESULT set_hr = device_test_force_device_lost(hDevice, E_FAIL);
  assert(set_hr == S_OK);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 20;
  dst.storage.resize(20);

  // When the device is lost, ProcessVertices should return the device-lost HRESULT
  // before validating vertex state (FVF/stream source/etc). Keep arguments simple.
  VertexDecl decl{};

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(hr == D3DERR_DEVICELOST);
}

void test_xyz_diffuse_with_pixel_shader_bound() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  // Even if a pixel shader is bound (shader-stage interop), ProcessVertices should
  // still use fixed-function vertex processing when no user VS is set.
  D3D9DDI_HSHADER fake_ps{};
  fake_ps.pDrvPrivate = reinterpret_cast<void*>(0x1);
  const HRESULT shader_hr = device_test_set_unmaterialized_user_shaders(
      hDevice, /*user_vs=*/{}, /*user_ps=*/fake_ps);
  assert(shader_hr == S_OK);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 16;
  src.storage.resize(16);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_u32(src.storage, 12, 0xAABBCCDDu);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 20;
  dst.storage.resize(20);

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 16);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

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
  assert(diffuse == 0xAABBCCDDu);
}

void test_xyz_diffuse_padded_dest_stride() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 16;
  src.storage.resize(16);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_u32(src.storage, 12, 0xAABBCCDDu);

  // Destination stride larger than the declaration's minimum.
  constexpr uint32_t kDestStride = 24;
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = kDestStride;
  dst.storage.resize(kDestStride);

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 16);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = kDestStride;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  // Padding bytes must be zeroed deterministically.
  for (size_t i = 20; i < kDestStride; ++i) {
    assert(dst.storage[i] == 0);
  }
}

void test_xyz_diffuse_inplace_overlap_safe() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  // Single buffer used as both src (XYZ|DIFFUSE, stride 16) and dst
  // (XYZRHW|DIFFUSE, stride 20). The destination range overlaps the source range
  // so ProcessVertices must stage the source bytes to avoid self-overwrite.
  Resource buf;
  buf.kind = ResourceKind::Buffer;
  buf.size_bytes = 40; // 2 * 20 bytes of output
  buf.storage.resize(40);
  std::memset(buf.storage.data(), 0, buf.storage.size());

  // Source vertex 0: x=0
  write_f32(buf.storage, 0, 0.0f);
  write_f32(buf.storage, 4, 0.0f);
  write_f32(buf.storage, 8, 0.0f);
  write_u32(buf.storage, 12, 0x11111111u);
  // Source vertex 1: x=2
  write_f32(buf.storage, 16, 2.0f);
  write_f32(buf.storage, 20, 0.0f);
  write_f32(buf.storage, 24, 0.0f);
  write_u32(buf.storage, 28, 0x22222222u);

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &buf, 16);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 2;
  pv.hDestBuffer.pDrvPrivate = &buf;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 20;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  // Vertex 0: x=(1+1)/2*100-0.5 = 99.5 (after +1 world translate)
  assert(std::fabs(read_f32(buf.storage, 0) - 99.5f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, 4) - 49.5f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, 8) - 0.0f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, 12) - 1.0f) < 1e-4f);
  uint32_t c0 = 0;
  std::memcpy(&c0, buf.storage.data() + 16, 4);
  assert(c0 == 0x11111111u);

  // Vertex 1: x=(3+1)/2*100-0.5 = 199.5
  const size_t v1 = 20;
  assert(std::fabs(read_f32(buf.storage, v1 + 0) - 199.5f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, v1 + 4) - 49.5f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, v1 + 8) - 0.0f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, v1 + 12) - 1.0f) < 1e-4f);
  uint32_t c1 = 0;
  std::memcpy(&c1, buf.storage.data() + v1 + 16, 4);
  assert(c1 == 0x22222222u);
}

void test_xyz_diffuse_tex1_inplace_overlap_safe() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse | kFvfTex1);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  // Single buffer used as both src (XYZ|DIFFUSE|TEX1, stride 24) and dst
  // (XYZRHW|DIFFUSE|TEX1, stride 28). The destination range overlaps the source
  // range, so ProcessVertices must stage the source slice before writing.
  Resource buf;
  buf.kind = ResourceKind::Buffer;
  buf.size_bytes = 56; // 2 * 28 bytes of output
  buf.storage.resize(56);
  std::memset(buf.storage.data(), 0, buf.storage.size());

  // Source vertex 0: x=0, uv=(0.1,0.2)
  write_f32(buf.storage, 0, 0.0f);
  write_f32(buf.storage, 4, 0.0f);
  write_f32(buf.storage, 8, 0.0f);
  write_u32(buf.storage, 12, 0x11111111u);
  write_f32(buf.storage, 16, 0.1f);
  write_f32(buf.storage, 20, 0.2f);
  // Source vertex 1: x=2, uv=(0.3,0.4)
  write_f32(buf.storage, 24, 2.0f);
  write_f32(buf.storage, 28, 0.0f);
  write_f32(buf.storage, 32, 0.0f);
  write_u32(buf.storage, 36, 0x22222222u);
  write_f32(buf.storage, 40, 0.3f);
  write_f32(buf.storage, 44, 0.4f);

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &buf, 24);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 2;
  pv.hDestBuffer.pDrvPrivate = &buf;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 28;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  // Vertex 0: x=99.5, uv copied.
  assert(std::fabs(read_f32(buf.storage, 0) - 99.5f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, 4) - 49.5f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, 8) - 0.0f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, 12) - 1.0f) < 1e-4f);
  uint32_t c0 = 0;
  std::memcpy(&c0, buf.storage.data() + 16, 4);
  assert(c0 == 0x11111111u);
  assert(std::fabs(read_f32(buf.storage, 20) - 0.1f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, 24) - 0.2f) < 1e-4f);

  // Vertex 1: x=199.5, uv copied.
  const size_t v1 = 28;
  assert(std::fabs(read_f32(buf.storage, v1 + 0) - 199.5f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, v1 + 4) - 49.5f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, v1 + 8) - 0.0f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, v1 + 12) - 1.0f) < 1e-4f);
  uint32_t c1 = 0;
  std::memcpy(&c1, buf.storage.data() + v1 + 16, 4);
  assert(c1 == 0x22222222u);
  assert(std::fabs(read_f32(buf.storage, v1 + 20) - 0.3f) < 1e-4f);
  assert(std::fabs(read_f32(buf.storage, v1 + 24) - 0.4f) < 1e-4f);
}

void test_xyz_diffuse_z_stays_ndc() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  // Non-default depth range: ProcessVertices output z should stay in NDC (0..1)
  // rather than being mapped to MinZ/MaxZ.
  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.25f, 0.75f);

  // Source VB: XYZ|DIFFUSE (float3 + u32) = 16 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 16;
  src.storage.resize(16);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_u32(src.storage, 12, 0x01020304u);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 20;
  dst.storage.resize(20);

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 16);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 20;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  const float z = read_f32(dst.storage, 8);
  assert(std::fabs(z - 0.0f) < 1e-4f);
}

void test_xyz_diffuse_tex1() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse | kFvfTex1);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

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

  set_stream0_or_die(hDevice, &src, 24);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  // Exercise DestStride inference from the vertex declaration (DestStride=0).
  pv.DestStride = 0;

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

void test_xyz_diffuse_tex1_do_not_copy_data_preserves_dest() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse | kFvfTex1);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

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

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 28;
  dst.storage.resize(28);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 24);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = kPvDoNotCopyData;
  pv.DestStride = 0;

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

  // Non-position fields should be untouched.
  for (size_t i = 16; i < dst.storage.size(); ++i) {
    assert(dst.storage[i] == 0xCD);
  }
}

void test_xyz_tex1() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfTex1);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  // Source VB: XYZ|TEX1 = float3 + float2 = 20 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 20;
  src.storage.resize(20);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_f32(src.storage, 12, 0.25f);
  write_f32(src.storage, 16, 0.75f);

  // Destination VB: XYZRHW|TEX1 = float4 + float2 = 24 bytes.
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 24;
  dst.storage.resize(24);

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 20);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

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

  const float u = read_f32(dst.storage, 16);
  const float v = read_f32(dst.storage, 20);
  assert(std::fabs(u - 0.25f) < 1e-4f);
  assert(std::fabs(v - 0.75f) < 1e-4f);
}

void test_xyz_tex1_defaults_white_diffuse() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfTex1);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  // Source VB: XYZ|TEX1 = float3 + float2 = 20 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 20;
  src.storage.resize(20);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_f32(src.storage, 12, 0.25f);
  write_f32(src.storage, 16, 0.75f);

  // Destination VB: request a diffuse color even though the source vertex format
  // does not include one. Fixed-function behavior should treat it as white.
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 28;
  dst.storage.resize(28);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 20);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

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
  assert(diffuse == 0xFFFFFFFFu);

  const float u = read_f32(dst.storage, 20);
  const float v = read_f32(dst.storage, 24);
  assert(std::fabs(u - 0.25f) < 1e-4f);
  assert(std::fabs(v - 0.75f) < 1e-4f);
}

void test_xyz_tex1_dest_decl_tex_usage0() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfTex1);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 20;
  src.storage.resize(20);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_f32(src.storage, 12, 0.25f);
  write_f32(src.storage, 16, 0.75f);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 28;
  dst.storage.resize(28);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  // Some runtimes synthesize decls with TEXCOORD0 Usage=0. Accept it and copy TEX0.
  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, /*Usage=*/0, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 20);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

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
  assert(diffuse == 0xFFFFFFFFu);

  const float u = read_f32(dst.storage, 20);
  const float v = read_f32(dst.storage, 24);
  assert(std::fabs(u - 0.25f) < 1e-4f);
  assert(std::fabs(v - 0.75f) < 1e-4f);
}

void test_xyz_tex1_float4_dest_decl_tex_usage0() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);
  // TEXCOORDSIZE4(0): 2 -> float4.
  set_fvf_or_die(hDevice, kFvfXyz | kFvfTex1 | (2u << 16));
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  // Source VB: XYZ|TEX1(float4) = float3 + float4 = 28 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 28;
  src.storage.resize(28);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_f32(src.storage, 12, 0.25f);
  write_f32(src.storage, 16, 0.75f);
  write_f32(src.storage, 20, 0.5f);
  write_f32(src.storage, 24, 0.125f);

  // Destination VB: XYZRHW|DIFFUSE|TEX1(float4) = float4 + u32 + float4 = 36 bytes.
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 36;
  dst.storage.resize(36);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  // Regression: TEXCOORD0 Usage=0 and Type=float4 must not be confused with the
  // position element (which is also float4).
  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat4, kDeclMethodDefault, /*Usage=*/0, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 28);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

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
  assert(diffuse == 0xFFFFFFFFu);

  const float u = read_f32(dst.storage, 20);
  const float v = read_f32(dst.storage, 24);
  const float w = read_f32(dst.storage, 28);
  const float q = read_f32(dst.storage, 32);
  assert(std::fabs(u - 0.25f) < 1e-4f);
  assert(std::fabs(v - 0.75f) < 1e-4f);
  assert(std::fabs(w - 0.5f) < 1e-4f);
  assert(std::fabs(q - 0.125f) < 1e-4f);
}

void test_xyz_tex1_float3_defaults_white_diffuse() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  // TEXCOORDSIZE3(0): 1 -> float3.
  set_fvf_or_die(hDevice, kFvfXyz | kFvfTex1 | (1u << 16));
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  // Source VB: XYZ|TEX1(float3) = float3 + float3 = 24 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 24;
  src.storage.resize(24);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_f32(src.storage, 12, 0.25f);
  write_f32(src.storage, 16, 0.75f);
  write_f32(src.storage, 20, 0.125f);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 32;
  dst.storage.resize(32);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat3, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 24);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

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
  assert(diffuse == 0xFFFFFFFFu);

  const float u = read_f32(dst.storage, 20);
  const float v = read_f32(dst.storage, 24);
  const float w = read_f32(dst.storage, 28);
  assert(std::fabs(u - 0.25f) < 1e-4f);
  assert(std::fabs(v - 0.75f) < 1e-4f);
  assert(std::fabs(w - 0.125f) < 1e-4f);
}

void test_xyzw_defaults_white_diffuse() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyzw);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);

  // Source VB: XYZW = float4 = 16 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 16;
  src.storage.resize(16);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_f32(src.storage, 12, 2.0f);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 20;
  dst.storage.resize(20);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 16);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  const float x = read_f32(dst.storage, 0);
  const float y = read_f32(dst.storage, 4);
  const float z = read_f32(dst.storage, 8);
  const float rhw = read_f32(dst.storage, 12);
  assert(std::fabs(x - 49.5f) < 1e-4f);
  assert(std::fabs(y - 49.5f) < 1e-4f);
  assert(std::fabs(z - 0.0f) < 1e-4f);
  assert(std::fabs(rhw - 0.5f) < 1e-4f);

  uint32_t diffuse = 0;
  std::memcpy(&diffuse, dst.storage.data() + 16, 4);
  assert(diffuse == 0xFFFFFFFFu);
}

void test_xyzw_diffuse() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyzw | kFvfDiffuse);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);

  // Source VB: XYZW|DIFFUSE = float4 + u32 = 20 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 20;
  src.storage.resize(20);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);
  write_f32(src.storage, 12, 2.0f);
  write_u32(src.storage, 16, 0xAABBCCDDu);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 20;
  dst.storage.resize(20);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 20);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  const float x = read_f32(dst.storage, 0);
  const float y = read_f32(dst.storage, 4);
  const float z = read_f32(dst.storage, 8);
  const float rhw = read_f32(dst.storage, 12);
  assert(std::fabs(x - 49.5f) < 1e-4f);
  assert(std::fabs(y - 49.5f) < 1e-4f);
  assert(std::fabs(z - 0.0f) < 1e-4f);
  assert(std::fabs(rhw - 0.5f) < 1e-4f);

  uint32_t diffuse = 0;
  std::memcpy(&diffuse, dst.storage.data() + 16, 4);
  assert(diffuse == 0xAABBCCDDu);
}

void test_xyz_defaults_white_diffuse() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  // Source VB: XYZ = float3 = 12 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 12;
  src.storage.resize(12);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 20;
  dst.storage.resize(20);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  // Destination vertex decl: positionT float4 at 0, color at 16.
  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 12);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

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
  assert(diffuse == 0xFFFFFFFFu);
}

void test_xyzrhw_defaults_white_diffuse() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyzrhw);
  // XYZRHW vertices should be passed through; transforms/viewport must not affect output.
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 123.0f);

  // Source VB: XYZRHW = float4 = 16 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 16;
  src.storage.resize(16);
  write_f32(src.storage, 0, 10.0f);
  write_f32(src.storage, 4, 20.0f);
  write_f32(src.storage, 8, 0.5f);
  write_f32(src.storage, 12, 2.0f);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 20;
  dst.storage.resize(20);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 16);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  const float x = read_f32(dst.storage, 0);
  const float y = read_f32(dst.storage, 4);
  const float z = read_f32(dst.storage, 8);
  const float rhw = read_f32(dst.storage, 12);
  assert(std::fabs(x - 10.0f) < 1e-4f);
  assert(std::fabs(y - 20.0f) < 1e-4f);
  assert(std::fabs(z - 0.5f) < 1e-4f);
  assert(std::fabs(rhw - 2.0f) < 1e-4f);

  uint32_t diffuse = 0;
  std::memcpy(&diffuse, dst.storage.data() + 16, 4);
  assert(diffuse == 0xFFFFFFFFu);
}

void test_xyz_do_not_copy_data_preserves_dest() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 12;
  src.storage.resize(12);
  write_f32(src.storage, 0, 0.0f);
  write_f32(src.storage, 4, 0.0f);
  write_f32(src.storage, 8, 0.0f);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 20;
  dst.storage.resize(20);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 12);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = kPvDoNotCopyData;
  pv.DestStride = 0;

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

  // D3DPV_DONOTCOPYDATA should preserve non-position destination bytes.
  uint32_t diffuse = 0;
  std::memcpy(&diffuse, dst.storage.data() + 16, 4);
  assert(diffuse == 0xCDCDCDCDu);
}

void test_xyzrhw_do_not_copy_data_preserves_dest() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyzrhw);

  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 16;
  src.storage.resize(16);
  write_f32(src.storage, 0, 10.0f);
  write_f32(src.storage, 4, 20.0f);
  write_f32(src.storage, 8, 0.5f);
  write_f32(src.storage, 12, 2.0f);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 20;
  dst.storage.resize(20);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 16);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = kPvDoNotCopyData;
  pv.DestStride = 0;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  const float x = read_f32(dst.storage, 0);
  const float y = read_f32(dst.storage, 4);
  const float z = read_f32(dst.storage, 8);
  const float rhw = read_f32(dst.storage, 12);
  assert(std::fabs(x - 10.0f) < 1e-4f);
  assert(std::fabs(y - 20.0f) < 1e-4f);
  assert(std::fabs(z - 0.5f) < 1e-4f);
  assert(std::fabs(rhw - 2.0f) < 1e-4f);

  uint32_t diffuse = 0;
  std::memcpy(&diffuse, dst.storage.data() + 16, 4);
  assert(diffuse == 0xCDCDCDCDu);
}

void test_xyzrhw_tex1_defaults_white_diffuse() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyzrhw | kFvfTex1);
  // XYZRHW vertices should be passed through; transforms/viewport must not affect output.
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 123.0f);

  // Source VB: XYZRHW|TEX1 = float4 + float2 = 24 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 24;
  src.storage.resize(24);
  write_f32(src.storage, 0, 10.0f);
  write_f32(src.storage, 4, 20.0f);
  write_f32(src.storage, 8, 0.5f);
  write_f32(src.storage, 12, 2.0f);
  write_f32(src.storage, 16, 0.25f);
  write_f32(src.storage, 20, 0.75f);

  // Destination VB: request a diffuse color even though the source vertex format
  // does not include one. Fixed-function behavior should treat it as white.
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 28;
  dst.storage.resize(28);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 24);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  const float x = read_f32(dst.storage, 0);
  const float y = read_f32(dst.storage, 4);
  const float z = read_f32(dst.storage, 8);
  const float rhw = read_f32(dst.storage, 12);
  assert(std::fabs(x - 10.0f) < 1e-4f);
  assert(std::fabs(y - 20.0f) < 1e-4f);
  assert(std::fabs(z - 0.5f) < 1e-4f);
  assert(std::fabs(rhw - 2.0f) < 1e-4f);

  uint32_t diffuse = 0;
  std::memcpy(&diffuse, dst.storage.data() + 16, 4);
  assert(diffuse == 0xFFFFFFFFu);

  const float u = read_f32(dst.storage, 20);
  const float v = read_f32(dst.storage, 24);
  assert(std::fabs(u - 0.25f) < 1e-4f);
  assert(std::fabs(v - 0.75f) < 1e-4f);
}

void test_xyzrhw_tex1_do_not_copy_data_preserves_dest() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyzrhw | kFvfTex1);

  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 24;
  src.storage.resize(24);
  write_f32(src.storage, 0, 10.0f);
  write_f32(src.storage, 4, 20.0f);
  write_f32(src.storage, 8, 0.5f);
  write_f32(src.storage, 12, 2.0f);
  write_f32(src.storage, 16, 0.25f);
  write_f32(src.storage, 20, 0.75f);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 28;
  dst.storage.resize(28);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 24);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = kPvDoNotCopyData;
  pv.DestStride = 0;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  const float x = read_f32(dst.storage, 0);
  const float y = read_f32(dst.storage, 4);
  const float z = read_f32(dst.storage, 8);
  const float rhw = read_f32(dst.storage, 12);
  assert(std::fabs(x - 10.0f) < 1e-4f);
  assert(std::fabs(y - 20.0f) < 1e-4f);
  assert(std::fabs(z - 0.5f) < 1e-4f);
  assert(std::fabs(rhw - 2.0f) < 1e-4f);

  // Non-position fields should be untouched.
  for (size_t i = 16; i < dst.storage.size(); ++i) {
    assert(dst.storage[i] == 0xCD);
  }
}

void test_process_vertices_dest_decl_ignores_other_streams() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  // Pre-transformed vertices: should be handled by the fixed-function CPU path.
  set_fvf_or_die(hDevice, kFvfXyzrhw | kFvfDiffuse);

  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 20;
  src.storage.resize(20);
  write_f32(src.storage, 0, 10.0f);
  write_f32(src.storage, 4, 20.0f);
  write_f32(src.storage, 8, 0.5f);
  write_f32(src.storage, 12, 2.0f);
  write_u32(src.storage, 16, 0xAABBCCDDu);

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  // The destination stride should be inferred from stream 0 only (20 bytes). If
  // other streams influenced the inferred stride, this destination would fail
  // bounds checks.
  dst.size_bytes = 20;
  dst.storage.resize(20);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      // Unrelated element in a different stream; must not affect stride inference.
      {1, 100, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 20);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));
  assert(dst.storage == src.storage);
}

void test_xyz_diffuse_tex1_padded_dest_stride() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse | kFvfTex1);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

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

  // Destination VB: padded stride (32 bytes per vertex).
  constexpr uint32_t kDestStride = 32;
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = kDestStride;
  dst.storage.resize(kDestStride);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 24);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = kDestStride;

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

  // Ensure padding bytes were zeroed deterministically.
  for (size_t i = 28; i < kDestStride; ++i) {
    assert(dst.storage[i] == 0);
  }
}

void test_xyz_diffuse_tex1_dest_decl_extra_elements() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse | kFvfTex1);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

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

  // Output decl with an extra unused TEXCOORD1 float2 at offset 28, which bumps
  // the inferred stride to 36 bytes.
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 36;
  dst.storage.resize(36);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0, 28, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 1},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 24);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 0;

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

  const float u0 = read_f32(dst.storage, 20);
  const float v0 = read_f32(dst.storage, 24);
  assert(std::fabs(u0 - 0.25f) < 1e-4f);
  assert(std::fabs(v0 - 0.75f) < 1e-4f);

  // TEXCOORD1 should be deterministically zero (we don't generate it).
  assert(std::fabs(read_f32(dst.storage, 28) - 0.0f) < 1e-6f);
  assert(std::fabs(read_f32(dst.storage, 32) - 0.0f) < 1e-6f);
}

void test_xyz_diffuse_offsets() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

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

  set_stream0_or_die(hDevice, &src, 16);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 1;
  pv.DestIndex = 1;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 20;

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

void test_xyz_diffuse_tex1_offsets() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  set_fvf_or_die(hDevice, kFvfXyz | kFvfDiffuse | kFvfTex1);
  set_viewport_or_die(hDevice, 0.0f, 0.0f, 100.0f, 100.0f, 0.0f, 1.0f);
  set_world_translate_x_or_die(hDevice, 1.0f);

  // Source VB: 2 vertices, each 24 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 48;
  src.storage.resize(48);

  // Vertex 0 (ignored).
  write_f32(src.storage, 0, 123.0f);
  write_f32(src.storage, 4, 456.0f);
  write_f32(src.storage, 8, 789.0f);
  write_u32(src.storage, 12, 0x11111111u);
  write_f32(src.storage, 16, 9.0f);
  write_f32(src.storage, 20, 8.0f);

  // Vertex 1 (used).
  write_f32(src.storage, 24, 0.0f);
  write_f32(src.storage, 28, 0.0f);
  write_f32(src.storage, 32, 0.0f);
  write_u32(src.storage, 36, 0x11223344u);
  write_f32(src.storage, 40, 0.25f);
  write_f32(src.storage, 44, 0.75f);

  // Destination VB: 2 vertices, 28 bytes each.
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 56;
  dst.storage.resize(56);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 24);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 1;
  pv.DestIndex = 1;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  // Exercise DestStride inference for the TEX1 variant as well.
  pv.DestStride = 0;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  // First vertex should remain untouched (sentinel pattern).
  for (size_t i = 0; i < 28; ++i) {
    assert(dst.storage[i] == 0xCD);
  }

  // Second vertex should contain transformed output.
  const float x = read_f32(dst.storage, 28);
  const float y = read_f32(dst.storage, 32);
  const float z = read_f32(dst.storage, 36);
  const float rhw = read_f32(dst.storage, 40);
  assert(std::fabs(x - 99.5f) < 1e-4f);
  assert(std::fabs(y - 49.5f) < 1e-4f);
  assert(std::fabs(z - 0.0f) < 1e-4f);
  assert(std::fabs(rhw - 1.0f) < 1e-4f);

  uint32_t diffuse = 0;
  std::memcpy(&diffuse, dst.storage.data() + 44, 4);
  assert(diffuse == 0x11223344u);

  const float u = read_f32(dst.storage, 48);
  const float v = read_f32(dst.storage, 52);
  assert(std::fabs(u - 0.25f) < 1e-4f);
  assert(std::fabs(v - 0.75f) < 1e-4f);
}

void test_copy_xyzrhw_diffuse_offsets() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  // Use a non-fixedfunc-supported FVF so the DDI falls back to the memcpy-style
  // ProcessVertices implementation (used by the Win7 smoke test path).
  set_fvf_or_die(hDevice, kFvfXyzrhw | kFvfDiffuse);

  // Source VB: XYZRHW|DIFFUSE (float4 + u32) = 20 bytes.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 5 * 20;
  src.storage.resize(src.size_bytes);

  // Vertices 0..1 are distinctive sentinels. The test uses BOTH a non-zero stream
  // offset and a non-zero SrcStartIndex, so ignoring either one should copy the
  // wrong slice.
  {
    const size_t base = 0 * 20;
    write_f32(src.storage, base + 0, -1000.0f);
    write_f32(src.storage, base + 4, -1000.0f);
    write_f32(src.storage, base + 8, -1000.0f);
    write_f32(src.storage, base + 12, 1.0f);
    write_u32(src.storage, base + 16, 0x01020304u);
  }
  {
    const size_t base = 1 * 20;
    write_f32(src.storage, base + 0, 1000.0f);
    write_f32(src.storage, base + 4, -1000.0f);
    write_f32(src.storage, base + 8, -1000.0f);
    write_f32(src.storage, base + 12, 1.0f);
    write_u32(src.storage, base + 16, 0x05060708u);
  }

  // Vertices 2..4 are the expected copied slice.
  const float verts[3][4] = {
      {10.0f, 20.0f, 0.5f, 1.0f},
      {30.0f, 40.0f, 0.5f, 1.0f},
      {50.0f, 60.0f, 0.5f, 1.0f},
  };
  const uint32_t colors[3] = {0xAABBCCDDu, 0x11223344u, 0x55667788u};
  for (size_t i = 0; i < 3; ++i) {
    const size_t base = (i + 2) * 20;
    write_f32(src.storage, base + 0, verts[i][0]);
    write_f32(src.storage, base + 4, verts[i][1]);
    write_f32(src.storage, base + 8, verts[i][2]);
    write_f32(src.storage, base + 12, verts[i][3]);
    write_u32(src.storage, base + 16, colors[i]);
  }

  // Destination VB: leave room before and after the written range so we can detect
  // DestIndex handling bugs and out-of-bounds writes.
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 10 * 20;
  dst.storage.resize(dst.size_bytes);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  // Provide a plausible destination declaration (unused by the memcpy-style
  // path, but present in the DDI args).
  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  set_stream0_or_die(hDevice, &src, 20, /*offset_bytes=*/20); // non-zero stream offset

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 1;
  pv.DestIndex = 3;
  pv.VertexCount = 3;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  pv.DestStride = 20;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  const size_t dst_stride = 20;
  const size_t dst_begin = static_cast<size_t>(pv.DestIndex) * dst_stride;
  const size_t dst_end = dst_begin + static_cast<size_t>(pv.VertexCount) * dst_stride;

  // Prefix should remain untouched (0xCD fill), verifying DestIndex handling.
  for (size_t i = 0; i < dst_begin; ++i) {
    assert(dst.storage[i] == 0xCD);
  }
  // Suffix should remain untouched (0xCD fill), catching overruns past VertexCount.
  for (size_t i = dst_end; i < dst.storage.size(); ++i) {
    assert(dst.storage[i] == 0xCD);
  }

  // Destination indices [3..5] should match source indices [1..3].
  const size_t src_off = 2 * 20;
  const size_t dst_off = static_cast<size_t>(pv.DestIndex) * dst_stride;
  assert(std::memcmp(dst.storage.data() + dst_off,
                     src.storage.data() + src_off,
                     static_cast<size_t>(pv.VertexCount) * dst_stride) == 0);
}

void test_copy_xyzrhw_diffuse_infer_dest_stride_from_decl() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  // Pre-transformed vertices (XYZRHW) should be passed through by the fixed-function
  // ProcessVertices CPU path. If the destination declaration includes extra
  // elements (e.g. TEX0) not present in the source FVF, those fields should be
  // deterministically zeroed.
  set_fvf_or_die(hDevice, kFvfXyzrhw | kFvfDiffuse);

  // Source VB: 2 vertices of XYZRHW|DIFFUSE = 20 bytes each.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 2 * 20;
  src.storage.resize(src.size_bytes);
  std::memset(src.storage.data(), 0, src.storage.size());

  // Vertex 0.
  write_f32(src.storage, 0, 10.0f);
  write_f32(src.storage, 4, 20.0f);
  write_f32(src.storage, 8, 0.5f);
  write_f32(src.storage, 12, 1.0f);
  write_u32(src.storage, 16, 0xAABBCCDDu);

  // Vertex 1.
  write_f32(src.storage, 20 + 0, 30.0f);
  write_f32(src.storage, 20 + 4, 40.0f);
  write_f32(src.storage, 20 + 8, 0.25f);
  write_f32(src.storage, 20 + 12, 2.0f);
  write_u32(src.storage, 20 + 16, 0x11223344u);

  // Destination decl includes an extra TEX0 float2 field, making the implied
  // stride 28 bytes (20 bytes of XYZRHW|DIFFUSE + 8 bytes TEX0).
  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  // Destination VB: 3 vertices worth of 28-byte stride so we can write starting
  // at DestIndex=1 and ensure the inferred stride is actually used.
  constexpr uint32_t kDstStride = 28;
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 3 * kDstStride;
  dst.storage.resize(dst.size_bytes);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  set_stream0_or_die(hDevice, &src, 20);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 1;
  pv.VertexCount = 2;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  // Exercise DestStride inference from the destination declaration.
  pv.DestStride = 0;

  // Expected result: copy the first 20 bytes of each source vertex into the
  // destination stride. The fixed-function path zeros the full destination
  // stride to produce deterministic output for elements not written by the
  // source FVF/decl mapping (e.g. dst has TEX0 but src does not), so TEX0 is
  // cleared.
  std::vector<uint8_t> expected = dst.storage;
  for (uint32_t i = 0; i < pv.VertexCount; ++i) {
    const size_t off = static_cast<size_t>(pv.DestIndex + i) * kDstStride;
    std::memset(expected.data() + off, 0, kDstStride);
    std::memcpy(expected.data() + off,
                src.storage.data() + static_cast<size_t>(pv.SrcStartIndex + i) * 20,
                20);
  }

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));
  assert(dst.storage == expected);
}

void test_process_vertices_fallback_infer_dest_stride_from_decl() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  // Force the memcpy-style fallback path (unsupported vertex processing).
  D3D9DDI_HSHADER fake_vs{};
  fake_vs.pDrvPrivate = reinterpret_cast<void*>(0x1);
  const HRESULT sh_hr = device_test_set_unmaterialized_user_shaders(hDevice, fake_vs, D3D9DDI_HSHADER{});
  assert(sh_hr == S_OK);

  set_fvf_or_die(hDevice, kFvfXyzrhw | kFvfDiffuse);

  // Source VB: 2 vertices of XYZRHW|DIFFUSE = 20 bytes each.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 2 * 20;
  src.storage.resize(src.size_bytes);
  std::memset(src.storage.data(), 0, src.storage.size());

  // Vertex 0.
  write_f32(src.storage, 0, 10.0f);
  write_f32(src.storage, 4, 20.0f);
  write_f32(src.storage, 8, 0.5f);
  write_f32(src.storage, 12, 1.0f);
  write_u32(src.storage, 16, 0xAABBCCDDu);

  // Vertex 1.
  write_f32(src.storage, 20 + 0, 30.0f);
  write_f32(src.storage, 20 + 4, 40.0f);
  write_f32(src.storage, 20 + 8, 0.25f);
  write_f32(src.storage, 20 + 12, 2.0f);
  write_u32(src.storage, 20 + 16, 0x11223344u);

  // Destination decl includes an extra TEX0 float2 field, making the implied
  // stride 28 bytes.
  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  constexpr uint32_t kDstStride = 28;
  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 3 * kDstStride;
  dst.storage.resize(dst.size_bytes);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  set_stream0_or_die(hDevice, &src, 20);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 1;
  pv.VertexCount = 2;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = 0;
  // Exercise DestStride inference in the fallback path.
  pv.DestStride = 0;

  // Expected result: copy the first 20 bytes of each source vertex into the
  // destination stride, leaving the extra TEX0 bytes untouched (0xCD).
  std::vector<uint8_t> expected = dst.storage;
  for (uint32_t i = 0; i < pv.VertexCount; ++i) {
    const size_t dst_off = static_cast<size_t>(pv.DestIndex + i) * kDstStride;
    const size_t src_off = static_cast<size_t>(pv.SrcStartIndex + i) * 20;
    std::memcpy(expected.data() + dst_off, src.storage.data() + src_off, 20);
  }

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));
  assert(dst.storage == expected);
}

void test_process_vertices_fallback_do_not_copy_data_xyzrhw() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  // Force the memcpy-style fallback path.
  D3D9DDI_HSHADER fake_vs{};
  fake_vs.pDrvPrivate = reinterpret_cast<void*>(0x1);
  const HRESULT sh_hr = device_test_set_unmaterialized_user_shaders(hDevice, fake_vs, D3D9DDI_HSHADER{});
  assert(sh_hr == S_OK);

  set_fvf_or_die(hDevice, kFvfXyzrhw | kFvfDiffuse);

  // Source VB: 1 vertex of XYZRHW|DIFFUSE.
  Resource src;
  src.kind = ResourceKind::Buffer;
  src.size_bytes = 20;
  src.storage.resize(20);
  write_f32(src.storage, 0, 10.0f);
  write_f32(src.storage, 4, 20.0f);
  write_f32(src.storage, 8, 0.5f);
  write_f32(src.storage, 12, 2.0f);
  write_u32(src.storage, 16, 0xAABBCCDDu);

  // Destination decl includes TEX0 so stride is inferred as 28 bytes.
  const D3DVERTEXELEMENT9_COMPAT elems[] = {
      {0, 0, kDeclTypeFloat4, kDeclMethodDefault, kDeclUsagePositionT, 0},
      {0, 16, kDeclTypeD3dColor, kDeclMethodDefault, kDeclUsageColor, 0},
      {0, 20, kDeclTypeFloat2, kDeclMethodDefault, kDeclUsageTexCoord, 0},
      {0xFF, 0, kDeclTypeUnused, 0, 0, 0},
  };
  VertexDecl decl;
  decl.blob.resize(sizeof(elems));
  std::memcpy(decl.blob.data(), elems, sizeof(elems));

  Resource dst;
  dst.kind = ResourceKind::Buffer;
  dst.size_bytes = 28;
  dst.storage.resize(28);
  std::memset(dst.storage.data(), 0xCD, dst.storage.size());

  set_stream0_or_die(hDevice, &src, 20);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = 0;
  pv.DestIndex = 0;
  pv.VertexCount = 1;
  pv.hDestBuffer.pDrvPrivate = &dst;
  pv.hVertexDecl.pDrvPrivate = &decl;
  pv.Flags = kPvDoNotCopyData;
  pv.DestStride = 0;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));

  // Position should be copied; non-position bytes should remain untouched.
  assert(std::memcmp(dst.storage.data(), src.storage.data(), 16) == 0);
  for (size_t i = 16; i < dst.storage.size(); ++i) {
    assert(dst.storage[i] == 0xCD);
  }
}

void test_process_vertices_fallback_inplace_overlap_dst_inside_src() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  // Force the ProcessVertices memcpy-style fallback path.
  D3D9DDI_HSHADER fake_vs{};
  fake_vs.pDrvPrivate = reinterpret_cast<void*>(0x1);
  const HRESULT sh_hr = device_test_set_unmaterialized_user_shaders(hDevice, fake_vs, D3D9DDI_HSHADER{});
  assert(sh_hr == S_OK);

  constexpr uint32_t kVertexCount = 4;
  constexpr uint32_t kSrcStride = 16;
  constexpr uint32_t kDstStride = 8;
  constexpr uint32_t kCopyStride = 8;

  Resource buf;
  buf.kind = ResourceKind::Buffer;
  buf.size_bytes = 64;
  buf.storage.resize(buf.size_bytes);
  std::memset(buf.storage.data(), 0xCD, buf.storage.size());

  // Source starts at 0, destination starts inside the source region (offset 8).
  constexpr uint32_t kSrcStartIndex = 0;
  constexpr uint32_t kDestIndex = 1;
  const size_t src_start_offset = static_cast<size_t>(kSrcStartIndex) * kSrcStride;
  const size_t dst_start_offset = static_cast<size_t>(kDestIndex) * kDstStride;

  for (uint32_t i = 0; i < kVertexCount; ++i) {
    write_pattern(buf.storage, src_start_offset + static_cast<size_t>(i) * kSrcStride, kCopyStride, static_cast<uint8_t>(0x10u + i));
  }

  const std::vector<uint8_t> initial = buf.storage;
  std::vector<uint8_t> expected = initial;
  for (uint32_t i = 0; i < kVertexCount; ++i) {
    std::memcpy(expected.data() + dst_start_offset + static_cast<size_t>(i) * kDstStride,
                initial.data() + src_start_offset + static_cast<size_t>(i) * kSrcStride,
                kCopyStride);
  }

  set_stream0_or_die(hDevice, &buf, kSrcStride);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = kSrcStartIndex;
  pv.DestIndex = kDestIndex;
  pv.VertexCount = kVertexCount;
  pv.hDestBuffer.pDrvPrivate = &buf;
  pv.hVertexDecl.pDrvPrivate = nullptr;
  pv.Flags = 0;
  pv.DestStride = kDstStride;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));
  assert(buf.storage == expected);
}

void test_process_vertices_fallback_inplace_overlap_src_inside_dst() {
  Adapter adapter;
  Device dev(&adapter);
  const D3DDDI_HDEVICE hDevice = make_device_handle(&dev);

  // Force the ProcessVertices memcpy-style fallback path.
  D3D9DDI_HSHADER fake_ps{};
  fake_ps.pDrvPrivate = reinterpret_cast<void*>(0x1);
  const HRESULT sh_hr = device_test_set_unmaterialized_user_shaders(hDevice, D3D9DDI_HSHADER{}, fake_ps);
  assert(sh_hr == S_OK);

  constexpr uint32_t kVertexCount = 4;
  constexpr uint32_t kSrcStride = 8;
  constexpr uint32_t kDstStride = 16;
  constexpr uint32_t kCopyStride = 8;

  Resource buf;
  buf.kind = ResourceKind::Buffer;
  buf.size_bytes = 64;
  buf.storage.resize(buf.size_bytes);
  std::memset(buf.storage.data(), 0xEF, buf.storage.size());

  // Destination starts at 0, source starts inside the destination region (offset 8).
  constexpr uint32_t kSrcStartIndex = 1;
  constexpr uint32_t kDestIndex = 0;
  const size_t src_start_offset = static_cast<size_t>(kSrcStartIndex) * kSrcStride;
  const size_t dst_start_offset = static_cast<size_t>(kDestIndex) * kDstStride;

  for (uint32_t i = 0; i < kVertexCount; ++i) {
    write_pattern(buf.storage, src_start_offset + static_cast<size_t>(i) * kSrcStride, kCopyStride, static_cast<uint8_t>(0x80u + i));
  }

  const std::vector<uint8_t> initial = buf.storage;
  std::vector<uint8_t> expected = initial;
  for (uint32_t i = 0; i < kVertexCount; ++i) {
    std::memcpy(expected.data() + dst_start_offset + static_cast<size_t>(i) * kDstStride,
                initial.data() + src_start_offset + static_cast<size_t>(i) * kSrcStride,
                kCopyStride);
  }

  set_stream0_or_die(hDevice, &buf, kSrcStride);

  D3DDDIARG_PROCESSVERTICES pv{};
  pv.SrcStartIndex = kSrcStartIndex;
  pv.DestIndex = kDestIndex;
  pv.VertexCount = kVertexCount;
  pv.hDestBuffer.pDrvPrivate = &buf;
  pv.hVertexDecl.pDrvPrivate = nullptr;
  pv.Flags = 0;
  pv.DestStride = kDstStride;

  const HRESULT hr = device_process_vertices(hDevice, &pv);
  assert(SUCCEEDED(hr));
  assert(buf.storage == expected);
}

} // namespace
} // namespace aerogpu

int main() {
  aerogpu::test_xyz_diffuse();
  aerogpu::test_xyz_diffuse_dest_decl_position_usage0();
  aerogpu::test_process_vertices_device_lost();
  aerogpu::test_xyz_diffuse_with_pixel_shader_bound();
  aerogpu::test_xyz_diffuse_padded_dest_stride();
  aerogpu::test_xyz_diffuse_inplace_overlap_safe();
  aerogpu::test_xyz_diffuse_tex1_inplace_overlap_safe();
  aerogpu::test_xyz_diffuse_z_stays_ndc();
  aerogpu::test_xyz_diffuse_tex1();
  aerogpu::test_xyz_diffuse_tex1_do_not_copy_data_preserves_dest();
  aerogpu::test_xyz_tex1();
  aerogpu::test_xyz_tex1_defaults_white_diffuse();
  aerogpu::test_xyz_tex1_dest_decl_tex_usage0();
  aerogpu::test_xyz_tex1_float4_dest_decl_tex_usage0();
  aerogpu::test_xyz_tex1_float3_defaults_white_diffuse();
  aerogpu::test_xyzw_defaults_white_diffuse();
  aerogpu::test_xyzw_diffuse();
  aerogpu::test_xyz_defaults_white_diffuse();
  aerogpu::test_xyzrhw_defaults_white_diffuse();
  aerogpu::test_xyz_do_not_copy_data_preserves_dest();
  aerogpu::test_xyzrhw_do_not_copy_data_preserves_dest();
  aerogpu::test_xyzrhw_tex1_defaults_white_diffuse();
  aerogpu::test_xyzrhw_tex1_do_not_copy_data_preserves_dest();
  aerogpu::test_process_vertices_dest_decl_ignores_other_streams();
  aerogpu::test_xyz_diffuse_tex1_padded_dest_stride();
  aerogpu::test_xyz_diffuse_tex1_dest_decl_extra_elements();
  aerogpu::test_xyz_diffuse_offsets();
  aerogpu::test_xyz_diffuse_tex1_offsets();
  aerogpu::test_copy_xyzrhw_diffuse_offsets();
  aerogpu::test_copy_xyzrhw_diffuse_infer_dest_stride_from_decl();
  aerogpu::test_process_vertices_fallback_infer_dest_stride_from_decl();
  aerogpu::test_process_vertices_fallback_do_not_copy_data_xyzrhw();
  aerogpu::test_process_vertices_fallback_inplace_overlap_dst_inside_src();
  aerogpu::test_process_vertices_fallback_inplace_overlap_src_inside_dst();
  return 0;
}
