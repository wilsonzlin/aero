#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <vector>

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_d3d9_fixedfunc_shaders.h"
#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_test_entrypoints.h"

namespace {

// Portable D3D9 FVF bits (from d3d9types.h).
constexpr uint32_t kD3dFvfXyz = 0x00000002u;
constexpr uint32_t kD3dFvfXyzRhw = 0x00000004u;
constexpr uint32_t kD3dFvfNormal = 0x00000010u;
constexpr uint32_t kD3dFvfDiffuse = 0x00000040u;
constexpr uint32_t kD3dFvfTex1 = 0x00000100u;

constexpr uint32_t kFvfXyzrhwDiffuseTex1 = kD3dFvfXyzRhw | kD3dFvfDiffuse | kD3dFvfTex1;
constexpr uint32_t kFvfXyzNormalTex1 = kD3dFvfXyz | kD3dFvfNormal | kD3dFvfTex1;

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

static_assert(sizeof(D3DVERTEXELEMENT9_COMPAT) == 8, "D3DVERTEXELEMENT9_COMPAT must be 8 bytes");

// D3DDECLTYPE values (from d3d9types.h).
constexpr uint8_t kD3dDeclTypeFloat2 = 1;
constexpr uint8_t kD3dDeclTypeFloat3 = 2;
constexpr uint8_t kD3dDeclTypeFloat4 = 3;
constexpr uint8_t kD3dDeclTypeD3dColor = 4;
constexpr uint8_t kD3dDeclTypeUnused = 17;

constexpr uint8_t kD3dDeclMethodDefault = 0;

// D3DDECLUSAGE values (from d3d9types.h).
constexpr uint8_t kD3dDeclUsagePosition = 0;
constexpr uint8_t kD3dDeclUsageNormal = 3;
constexpr uint8_t kD3dDeclUsageTexcoord = 5;
constexpr uint8_t kD3dDeclUsagePositionT = 9;
constexpr uint8_t kD3dDeclUsageColor = 10;

// Pixel shader instruction tokens (ps_2_0).
constexpr uint32_t kPsOpTexld = 0x04000042u;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

template <size_t N>
bool ShaderBytecodeEquals(const aerogpu::Shader* shader, const uint32_t (&expected)[N]) {
  if (!shader) {
    return false;
  }
  if (shader->bytecode.size() != sizeof(expected)) {
    return false;
  }
  return std::memcmp(shader->bytecode.data(), expected, sizeof(expected)) == 0;
}

bool ShaderContainsToken(const aerogpu::Shader* shader, uint32_t token) {
  if (!shader) {
    return false;
  }
  const size_t size = shader->bytecode.size();
  if (size < sizeof(uint32_t) || (size % sizeof(uint32_t)) != 0) {
    return false;
  }
  for (size_t off = 0; off < size; off += sizeof(uint32_t)) {
    uint32_t w = 0;
    std::memcpy(&w, shader->bytecode.data() + off, sizeof(uint32_t));
    if (w == token) {
      return true;
    }
  }
  return false;
}

bool ValidateStream(const uint8_t* buf, size_t capacity) {
  if (!Check(buf != nullptr, "buffer must be non-null")) {
    return false;
  }
  if (!Check(capacity >= sizeof(aerogpu_cmd_stream_header), "buffer must contain stream header")) {
    return false;
  }

  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  if (!Check(stream->magic == AEROGPU_CMD_STREAM_MAGIC, "stream magic")) {
    return false;
  }
  if (!Check(stream->abi_version == AEROGPU_ABI_VERSION_U32, "stream abi_version")) {
    return false;
  }
  if (!Check(stream->flags == AEROGPU_CMD_STREAM_FLAG_NONE, "stream flags")) {
    return false;
  }
  if (!Check(stream->size_bytes >= sizeof(aerogpu_cmd_stream_header), "stream size_bytes >= header")) {
    return false;
  }
  if (!Check(stream->size_bytes <= capacity, "stream size_bytes within capacity")) {
    return false;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset < stream->size_bytes) {
    if (!Check((offset & 3u) == 0, "packet offset 4-byte aligned")) {
      return false;
    }
    if (!Check(offset + sizeof(aerogpu_cmd_hdr) <= stream->size_bytes, "packet header within stream")) {
      return false;
    }

    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (!Check(hdr->size_bytes >= sizeof(aerogpu_cmd_hdr), "packet size >= hdr")) {
      return false;
    }
    if (!Check((hdr->size_bytes & 3u) == 0, "packet size 4-byte aligned")) {
      return false;
    }
    if (!Check(offset + hdr->size_bytes <= stream->size_bytes, "packet fits within stream")) {
      return false;
    }
    offset += hdr->size_bytes;
  }

  return Check(offset == stream->size_bytes, "parser consumed entire stream");
}

size_t CountOpcode(const uint8_t* buf, size_t capacity, uint32_t opcode) {
  if (!buf || capacity < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  if (stream->size_bytes < sizeof(aerogpu_cmd_stream_header) || stream->size_bytes > capacity) {
    return 0;
  }

  size_t count = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream->size_bytes) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      count++;
    }
    if (hdr->size_bytes == 0 || offset + hdr->size_bytes > stream->size_bytes) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return count;
}

std::vector<const aerogpu_cmd_hdr*> CollectOpcodes(const uint8_t* buf, size_t capacity, uint32_t opcode) {
  std::vector<const aerogpu_cmd_hdr*> out;
  if (!buf || capacity < sizeof(aerogpu_cmd_stream_header)) {
    return out;
  }
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  if (stream->size_bytes < sizeof(aerogpu_cmd_stream_header) || stream->size_bytes > capacity) {
    return out;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream->size_bytes) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      out.push_back(hdr);
    }
    if (hdr->size_bytes == 0 || offset + hdr->size_bytes > stream->size_bytes) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return out;
}

bool FindCreateInputLayoutBlob(
    const uint8_t* buf,
    size_t capacity,
    aerogpu_handle_t handle,
    const uint8_t** out_blob,
    uint32_t* out_blob_size_bytes) {
  if (!out_blob || !out_blob_size_bytes) {
    return false;
  }
  *out_blob = nullptr;
  *out_blob_size_bytes = 0;

  for (const aerogpu_cmd_hdr* hdr : CollectOpcodes(buf, capacity, AEROGPU_CMD_CREATE_INPUT_LAYOUT)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_create_input_layout)) {
      continue;
    }
    const auto* c = reinterpret_cast<const aerogpu_cmd_create_input_layout*>(hdr);
    if (c->input_layout_handle != handle) {
      continue;
    }
    const size_t needed = sizeof(aerogpu_cmd_create_input_layout) + static_cast<size_t>(c->blob_size_bytes);
    if (hdr->size_bytes < needed) {
      continue;
    }
    *out_blob = reinterpret_cast<const uint8_t*>(c) + sizeof(aerogpu_cmd_create_input_layout);
    *out_blob_size_bytes = c->blob_size_bytes;
    return true;
  }
  return false;
}

struct VertexXyzrhwDiffuseTex1 {
  float x;
  float y;
  float z;
  float rhw;
  uint32_t color;
  float u;
  float v;
};

struct VertexXyzNormalTex1 {
  float x;
  float y;
  float z;
  float nx;
  float ny;
  float nz;
  float u;
  float v;
};

bool TestFixedFuncVertexDeclPatternsNonCanonicalOrdering() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  dev.cmd.reset();

  // Non-canonical decl element ordering + an extra UNUSED placeholder element.
  //
  // This is XYZRHW | DIFFUSE | TEX1 (float2) but emitted as:
  //   TEX0, UNUSED, COLOR0, POSITIONT, END
  const D3DVERTEXELEMENT9_COMPAT decl_elems[] = {
      {0, 20, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0, 24, kD3dDeclTypeUnused, kD3dDeclMethodDefault, 0, 0},
      {0, 16, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = aerogpu::device_create_vertex_decl(hDevice, decl_elems, sizeof(decl_elems), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl returned S_OK")) {
    return false;
  }

  hr = aerogpu::device_set_vertex_decl(hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl returned S_OK")) {
    return false;
  }

  aerogpu_handle_t input_layout_handle = 0;
  {
    std::lock_guard<std::mutex> lock(dev.mutex);
    if (!Check(dev.vertex_decl != nullptr, "SetVertexDecl binds a vertex decl")) {
      return false;
    }
    input_layout_handle = dev.vertex_decl->handle;
    if (!Check(dev.fvf == kFvfXyzrhwDiffuseTex1, "SetVertexDecl inferred FVF == XYZRHW|DIFFUSE|TEX1")) {
      return false;
    }
  }

  // Fixed-function draw: user VS/PS are NULL by default.
  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu, 0.0f, 1.0f},
  };

  hr = aerogpu::device_draw_primitive_up(
      hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP returned S_OK")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev.mutex);

    const aerogpu::FixedFuncVariant variant = aerogpu::fixedfunc_variant_from_fvf(dev.fvf);
    if (!Check(variant == aerogpu::FixedFuncVariant::RHW_COLOR_TEX1, "implied fixedfunc variant == RHW_COLOR_TEX1")) {
      return false;
    }
    const auto& pipe = dev.fixedfunc_pipelines[static_cast<size_t>(variant)];

    if (!Check(pipe.vs != nullptr, "fixedfunc pipeline VS created (RHW_COLOR_TEX1)")) {
      return false;
    }
    if (!Check(dev.vs == pipe.vs, "fixedfunc pipeline VS is bound (RHW_COLOR_TEX1)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev.vs, aerogpu::fixedfunc::kVsPassthroughPosColorTex1),
               "fixedfunc pipeline VS bytecode matches kVsPassthroughPosColorTex1")) {
      return false;
    }

    if (!Check(pipe.ps != nullptr, "fixedfunc pipeline PS created (RHW_COLOR_TEX1)")) {
      return false;
    }
    if (!Check(dev.ps == pipe.ps, "fixedfunc pipeline PS is bound (RHW_COLOR_TEX1)")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev.ps, kPsOpTexld), "fixedfunc pipeline PS contains no texld when stage0 texture is unbound")) {
      return false;
    }
  }

  dev.cmd.finalize();
  const uint8_t* buf = dev.cmd.data();
  const size_t len = dev.cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_INPUT_LAYOUT) >= 1, "CREATE_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT) >= 1, "SET_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) >= 2, "CREATE_SHADER_DXBC emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS) >= 1, "SET_VERTEX_BUFFERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE) >= 1, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }

  // Validate that the input layout blob for our explicit vertex decl matches the
  // non-canonical declaration bytes we provided.
  const uint8_t* blob = nullptr;
  uint32_t blob_size = 0;
  if (!Check(FindCreateInputLayoutBlob(buf, len, input_layout_handle, &blob, &blob_size), "found CREATE_INPUT_LAYOUT blob")) {
    return false;
  }
  if (!Check(blob_size == sizeof(decl_elems), "input-layout blob size")) {
    return false;
  }
  if (!Check(std::memcmp(blob, decl_elems, sizeof(decl_elems)) == 0, "input-layout blob contents")) {
    return false;
  }

  // Ensure SET_INPUT_LAYOUT binds the expected handle at least once.
  bool saw_set = false;
  for (const aerogpu_cmd_hdr* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_input_layout)) {
      continue;
    }
    const auto* s = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (s->input_layout_handle == input_layout_handle) {
      saw_set = true;
      break;
    }
  }
  if (!Check(saw_set, "SET_INPUT_LAYOUT binds explicit vertex decl handle")) {
    return false;
  }

  return true;
}

bool TestFixedFuncVertexDeclPatternsNonCanonicalNormalTex1Ordering() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  dev.cmd.reset();

  // Non-canonical decl element ordering + an extra UNUSED placeholder element.
  //
  // This is XYZ | NORMAL | TEX1 (float2) but emitted as:
  //   TEX0, UNUSED, NORMAL, POSITION, END
  const D3DVERTEXELEMENT9_COMPAT decl_elems[] = {
      {0, 24, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0, 32, kD3dDeclTypeUnused, kD3dDeclMethodDefault, 0, 0},
      {0, 12, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsageNormal, 0},
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = aerogpu::device_create_vertex_decl(hDevice, decl_elems, sizeof(decl_elems), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl returned S_OK (XYZ|NORMAL|TEX1)")) {
    return false;
  }

  hr = aerogpu::device_set_vertex_decl(hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl returned S_OK (XYZ|NORMAL|TEX1)")) {
    return false;
  }

  aerogpu_handle_t input_layout_handle = 0;
  {
    std::lock_guard<std::mutex> lock(dev.mutex);
    if (!Check(dev.vertex_decl != nullptr, "SetVertexDecl binds a vertex decl")) {
      return false;
    }
    input_layout_handle = dev.vertex_decl->handle;
    if (!Check(dev.fvf == kFvfXyzNormalTex1, "SetVertexDecl inferred FVF == XYZ|NORMAL|TEX1")) {
      return false;
    }
  }

  // Fixed-function draw: user VS/PS are NULL by default.
  const VertexXyzNormalTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0.0f, 1.0f},
  };

  hr = aerogpu::device_draw_primitive_up(
      hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP returned S_OK (XYZ|NORMAL|TEX1)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev.mutex);

    const aerogpu::FixedFuncVariant variant = aerogpu::fixedfunc_variant_from_fvf(dev.fvf);
    if (!Check(variant == aerogpu::FixedFuncVariant::XYZ_NORMAL_TEX1, "implied fixedfunc variant == XYZ_NORMAL_TEX1")) {
      return false;
    }
    const auto& pipe = dev.fixedfunc_pipelines[static_cast<size_t>(variant)];

    if (!Check(pipe.vs != nullptr, "fixedfunc pipeline VS created (XYZ_NORMAL_TEX1)")) {
      return false;
    }
    if (!Check(dev.vs == pipe.vs, "fixedfunc pipeline VS is bound (XYZ_NORMAL_TEX1)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev.vs, aerogpu::fixedfunc::kVsWvpPosNormalWhiteTex0),
               "fixedfunc pipeline VS bytecode matches kVsWvpPosNormalWhiteTex0")) {
      return false;
    }

    if (!Check(pipe.ps != nullptr, "fixedfunc pipeline PS created (XYZ_NORMAL_TEX1)")) {
      return false;
    }
    if (!Check(dev.ps == pipe.ps, "fixedfunc pipeline PS is bound (XYZ_NORMAL_TEX1)")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev.ps, kPsOpTexld), "fixedfunc pipeline PS contains no texld when stage0 texture is unbound")) {
      return false;
    }
  }

  dev.cmd.finalize();
  const uint8_t* buf = dev.cmd.data();
  const size_t len = dev.cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|NORMAL|TEX1)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_INPUT_LAYOUT) >= 1, "CREATE_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT) >= 1, "SET_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) >= 2, "CREATE_SHADER_DXBC emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS) >= 1, "SET_VERTEX_BUFFERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE) >= 1, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }

  // Validate that the input layout blob for our explicit vertex decl matches the
  // non-canonical declaration bytes we provided.
  const uint8_t* blob = nullptr;
  uint32_t blob_size = 0;
  if (!Check(FindCreateInputLayoutBlob(buf, len, input_layout_handle, &blob, &blob_size), "found CREATE_INPUT_LAYOUT blob")) {
    return false;
  }
  if (!Check(blob_size == sizeof(decl_elems), "input-layout blob size")) {
    return false;
  }
  if (!Check(std::memcmp(blob, decl_elems, sizeof(decl_elems)) == 0, "input-layout blob contents")) {
    return false;
  }

  // Ensure SET_INPUT_LAYOUT binds the expected handle at least once.
  bool saw_set = false;
  for (const aerogpu_cmd_hdr* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_input_layout)) {
      continue;
    }
    const auto* s = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (s->input_layout_handle == input_layout_handle) {
      saw_set = true;
      break;
    }
  }
  if (!Check(saw_set, "SET_INPUT_LAYOUT binds explicit vertex decl handle")) {
    return false;
  }

  return true;
}

} // namespace

int main() {
  if (!TestFixedFuncVertexDeclPatternsNonCanonicalOrdering()) {
    return 1;
  }
  if (!TestFixedFuncVertexDeclPatternsNonCanonicalNormalTex1Ordering()) {
    return 1;
  }
  return 0;
}
