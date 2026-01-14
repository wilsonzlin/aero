#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <vector>

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_test_entrypoints.h"

namespace {

// Portable D3D9 FVF bits (from d3d9types.h).
constexpr uint32_t kD3dFvfXyz = 0x00000002u;
constexpr uint32_t kD3dFvfXyzRhw = 0x00000004u;
constexpr uint32_t kD3dFvfXyzw = 0x00004002u;
constexpr uint32_t kD3dFvfXyzB4 = 0x0000000Cu;
constexpr uint32_t kD3dFvfNormal = 0x00000010u;
constexpr uint32_t kD3dFvfPSize = 0x00000020u;
constexpr uint32_t kD3dFvfDiffuse = 0x00000040u;
constexpr uint32_t kD3dFvfSpecular = 0x00000080u;
constexpr uint32_t kD3dFvfTex1 = 0x00000100u;
constexpr uint32_t kD3dFvfTex2 = 0x00000200u;
constexpr uint32_t kD3dFvfLastBetaUByte4 = 0x00001000u;

// D3DFVF_TEXCOUNT_* encoding bits (4-bit field).
constexpr uint32_t kD3dFvfTexCountMask = 0x00000F00u;
constexpr uint32_t kD3dFvfTexCountShift = 8u;

// D3DFVF_TEXCOORDSIZE* encoding (2 bits per set starting at bit 16).
constexpr uint32_t kD3dFvfTextureFormat2 = 0u;
constexpr uint32_t kD3dFvfTextureFormat3 = 1u;
constexpr uint32_t kD3dFvfTextureFormat4 = 2u;
constexpr uint32_t kD3dFvfTextureFormat1 = 3u;

constexpr uint32_t D3dFvfTexCoordSizeBits(uint32_t coord_index) {
  return 16u + coord_index * 2u;
}
constexpr uint32_t D3dFvfTexCoordSize1(uint32_t coord_index) {
  return kD3dFvfTextureFormat1 << D3dFvfTexCoordSizeBits(coord_index);
}
constexpr uint32_t D3dFvfTexCoordSize2(uint32_t coord_index) {
  return kD3dFvfTextureFormat2 << D3dFvfTexCoordSizeBits(coord_index);
}
constexpr uint32_t D3dFvfTexCoordSize3(uint32_t coord_index) {
  return kD3dFvfTextureFormat3 << D3dFvfTexCoordSizeBits(coord_index);
}
constexpr uint32_t D3dFvfTexCoordSize4(uint32_t coord_index) {
  return kD3dFvfTextureFormat4 << D3dFvfTexCoordSizeBits(coord_index);
}

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
constexpr uint8_t kD3dDeclTypeFloat1 = 0;
constexpr uint8_t kD3dDeclTypeFloat2 = 1;
constexpr uint8_t kD3dDeclTypeFloat3 = 2;
constexpr uint8_t kD3dDeclTypeFloat4 = 3;
constexpr uint8_t kD3dDeclTypeD3dColor = 4;
constexpr uint8_t kD3dDeclTypeUByte4 = 5;
constexpr uint8_t kD3dDeclTypeUnused = 17;

constexpr uint8_t kD3dDeclMethodDefault = 0;

// D3DDECLUSAGE values (from d3d9types.h).
constexpr uint8_t kD3dDeclUsagePosition = 0;
constexpr uint8_t kD3dDeclUsageBlendWeight = 1;
constexpr uint8_t kD3dDeclUsageBlendIndices = 2;
constexpr uint8_t kD3dDeclUsageNormal = 3;
constexpr uint8_t kD3dDeclUsagePSize = 4;
constexpr uint8_t kD3dDeclUsageTexcoord = 5;
constexpr uint8_t kD3dDeclUsagePositionT = 9;
constexpr uint8_t kD3dDeclUsageColor = 10;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
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

bool BlobEqualsDecl(const uint8_t* blob, uint32_t blob_size, const D3DVERTEXELEMENT9_COMPAT* expected, size_t expected_count) {
  if (!blob || !expected) {
    return false;
  }
  const size_t expected_bytes = expected_count * sizeof(D3DVERTEXELEMENT9_COMPAT);
  if (blob_size != expected_bytes) {
    return false;
  }
  return std::memcmp(blob, expected, expected_bytes) == 0;
}

bool TestFvfVertexDeclTranslation() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;

  dev.cmd.reset();

  // ---------------------------------------------------------------------------
  // Exercise 3 "new" FVFs beyond the fixed-function bring-up subset.
  // ---------------------------------------------------------------------------
  const uint32_t kFvfA = kD3dFvfXyz | kD3dFvfNormal | kD3dFvfDiffuse | kD3dFvfTex1 | D3dFvfTexCoordSize3(0);
  const uint32_t kFvfB = kD3dFvfXyzRhw | kD3dFvfDiffuse | kD3dFvfSpecular | kD3dFvfTex2;
  const uint32_t kFvfC = kD3dFvfXyz | kD3dFvfNormal | kD3dFvfSpecular | kD3dFvfTex2 | D3dFvfTexCoordSize1(0) |
                         D3dFvfTexCoordSize4(1);
  const uint32_t kFvfD =
      kD3dFvfXyzw | kD3dFvfNormal | kD3dFvfPSize | kD3dFvfDiffuse | kD3dFvfSpecular | kD3dFvfTex1;
  const uint32_t kFvfE = kD3dFvfXyzB4 | kD3dFvfLastBetaUByte4 | kD3dFvfNormal | kD3dFvfTex1;

  auto set_and_get_layout = [&](uint32_t fvf, aerogpu_handle_t* out_handle) -> bool {
    if (!out_handle) {
      return false;
    }
    *out_handle = 0;
    HRESULT hr = aerogpu::device_set_fvf(hDevice, fvf);
    if (!Check(hr == S_OK, "SetFVF returned S_OK")) {
      return false;
    }
    std::lock_guard<std::mutex> lock(dev.mutex);
    if (!dev.vertex_decl) {
      return Check(false, "SetFVF must bind an internal vertex declaration");
    }
    *out_handle = dev.vertex_decl->handle;
    return Check(*out_handle != 0, "SetFVF produced non-zero input-layout handle");
  };

  aerogpu_handle_t layout_a0 = 0;
  aerogpu_handle_t layout_b0 = 0;
  aerogpu_handle_t layout_c0 = 0;
  aerogpu_handle_t layout_d0 = 0;
  aerogpu_handle_t layout_e0 = 0;
  aerogpu_handle_t layout_a1 = 0;
  aerogpu_handle_t layout_b1 = 0;
  aerogpu_handle_t layout_c1 = 0;
  aerogpu_handle_t layout_d1 = 0;
  aerogpu_handle_t layout_e1 = 0;

  if (!set_and_get_layout(kFvfA, &layout_a0)) {
    return false;
  }
  if (!set_and_get_layout(kFvfB, &layout_b0)) {
    return false;
  }
  if (!set_and_get_layout(kFvfC, &layout_c0)) {
    return false;
  }
  if (!set_and_get_layout(kFvfD, &layout_d0)) {
    return false;
  }
  if (!set_and_get_layout(kFvfE, &layout_e0)) {
    return false;
  }
  // Repeat to validate caching (no new CREATE_INPUT_LAYOUT for the same FVF).
  if (!set_and_get_layout(kFvfA, &layout_a1)) {
    return false;
  }
  if (!set_and_get_layout(kFvfB, &layout_b1)) {
    return false;
  }
  if (!set_and_get_layout(kFvfC, &layout_c1)) {
    return false;
  }
  if (!set_and_get_layout(kFvfD, &layout_d1)) {
    return false;
  }
  if (!set_and_get_layout(kFvfE, &layout_e1)) {
    return false;
  }

  if (!Check(layout_a0 == layout_a1, "FVF A input layout handle is cached")) {
    return false;
  }
  if (!Check(layout_b0 == layout_b1, "FVF B input layout handle is cached")) {
    return false;
  }
  if (!Check(layout_c0 == layout_c1, "FVF C input layout handle is cached")) {
    return false;
  }
  if (!Check(layout_d0 == layout_d1, "FVF D input layout handle is cached")) {
    return false;
  }
  if (!Check(layout_e0 == layout_e1, "FVF E input layout handle is cached")) {
    return false;
  }

  dev.cmd.finalize();
  const uint8_t* buf = dev.cmd.data();
  const size_t len = dev.cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream")) {
    return false;
  }

  // Exactly one CREATE_INPUT_LAYOUT per distinct FVF.
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_INPUT_LAYOUT) == 5, "expected 5 CREATE_INPUT_LAYOUT packets")) {
    return false;
  }

  // Validate blob contents for each FVF.
  const D3DVERTEXELEMENT9_COMPAT expected_a[] = {
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsageNormal, 0},
      {0, 24, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0, 28, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };
  const D3DVERTEXELEMENT9_COMPAT expected_b[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0, 16, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0, 20, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 1},
      {0, 24, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0, 32, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 1},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };
  const D3DVERTEXELEMENT9_COMPAT expected_c[] = {
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsageNormal, 0},
      {0, 24, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 1},
      {0, 28, kD3dDeclTypeFloat1, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0, 32, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 1},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };
  const D3DVERTEXELEMENT9_COMPAT expected_d[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 16, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsageNormal, 0},
      {0, 28, kD3dDeclTypeFloat1, kD3dDeclMethodDefault, kD3dDeclUsagePSize, 0},
      {0, 32, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0, 36, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 1},
      {0, 40, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };
  const D3DVERTEXELEMENT9_COMPAT expected_e[] = {
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsageBlendWeight, 0},
      {0, 28, kD3dDeclTypeUByte4, kD3dDeclMethodDefault, kD3dDeclUsageBlendIndices, 0},
      {0, 32, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsageNormal, 0},
      {0, 44, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  const uint8_t* blob = nullptr;
  uint32_t blob_size = 0;
  if (!Check(FindCreateInputLayoutBlob(buf, len, layout_a0, &blob, &blob_size), "found CREATE_INPUT_LAYOUT for FVF A")) {
    return false;
  }
  if (!Check(BlobEqualsDecl(blob, blob_size, expected_a, std::size(expected_a)), "FVF A input-layout blob")) {
    return false;
  }

  blob = nullptr;
  blob_size = 0;
  if (!Check(FindCreateInputLayoutBlob(buf, len, layout_b0, &blob, &blob_size), "found CREATE_INPUT_LAYOUT for FVF B")) {
    return false;
  }
  if (!Check(BlobEqualsDecl(blob, blob_size, expected_b, std::size(expected_b)), "FVF B input-layout blob")) {
    return false;
  }

  blob = nullptr;
  blob_size = 0;
  if (!Check(FindCreateInputLayoutBlob(buf, len, layout_c0, &blob, &blob_size), "found CREATE_INPUT_LAYOUT for FVF C")) {
    return false;
  }
  if (!Check(BlobEqualsDecl(blob, blob_size, expected_c, std::size(expected_c)), "FVF C input-layout blob")) {
    return false;
  }

  blob = nullptr;
  blob_size = 0;
  if (!Check(FindCreateInputLayoutBlob(buf, len, layout_d0, &blob, &blob_size), "found CREATE_INPUT_LAYOUT for FVF D")) {
    return false;
  }
  if (!Check(BlobEqualsDecl(blob, blob_size, expected_d, std::size(expected_d)), "FVF D input-layout blob")) {
    return false;
  }

  blob = nullptr;
  blob_size = 0;
  if (!Check(FindCreateInputLayoutBlob(buf, len, layout_e0, &blob, &blob_size), "found CREATE_INPUT_LAYOUT for FVF E")) {
    return false;
  }
  if (!Check(BlobEqualsDecl(blob, blob_size, expected_e, std::size(expected_e)), "FVF E input-layout blob")) {
    return false;
  }

  // Ensure SET_INPUT_LAYOUT binds the expected handles at least once.
  auto saw_set = [&](aerogpu_handle_t handle) -> bool {
    for (const aerogpu_cmd_hdr* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
      if (hdr->size_bytes < sizeof(aerogpu_cmd_set_input_layout)) {
        continue;
      }
      const auto* s = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
      if (s->input_layout_handle == handle) {
        return true;
      }
    }
    return false;
  };

  if (!Check(saw_set(layout_a0), "SET_INPUT_LAYOUT binds FVF A handle")) {
    return false;
  }
  if (!Check(saw_set(layout_b0), "SET_INPUT_LAYOUT binds FVF B handle")) {
    return false;
  }
  if (!Check(saw_set(layout_c0), "SET_INPUT_LAYOUT binds FVF C handle")) {
    return false;
  }
  if (!Check(saw_set(layout_d0), "SET_INPUT_LAYOUT binds FVF D handle")) {
    return false;
  }
  if (!Check(saw_set(layout_e0), "SET_INPUT_LAYOUT binds FVF E handle")) {
    return false;
  }

  return true;
}

bool TestSetFvfTexcoordSizeBits() {
  aerogpu::Adapter adapter{};
  aerogpu::Device dev(&adapter);
  D3DDDI_HDEVICE hDevice{};
  hDevice.pDrvPrivate = &dev;
  dev.cmd.reset();

  // SetFVF should bind an internal vertex declaration matching the FVF's
  // D3DFVF_TEXCOORDSIZE* encoding (input layout translation for user shaders).
  //
  // Note: both fixed-function draws and patch tessellation consume `TEXCOORD0`
  // using the conventional D3D9 semantics:
  // - `float1`: uses `.x` as `u` and treats `v = 0`
  // - `float2/float3/float4`: uses `.xy` as `(u, v)` (extra components are ignored)
  const uint32_t kFvfA = kD3dFvfXyzRhw | kD3dFvfDiffuse | D3dFvfTexCoordSize3(0); // TEX0 unused; size bits ignored
  const uint32_t kFvfB = kD3dFvfXyzRhw | kD3dFvfDiffuse | kD3dFvfTex1 | D3dFvfTexCoordSize1(0); // TEX0=float1
  const uint32_t kFvfC = kD3dFvfXyzRhw | kD3dFvfTex1 | D3dFvfTexCoordSize3(0);                  // TEX0=float3
  const uint32_t kFvfD = kD3dFvfXyz | kD3dFvfTex1 | D3dFvfTexCoordSize4(0);                      // TEX0=float4

  auto set_and_get_layout = [&](uint32_t fvf, aerogpu_handle_t* out_handle) -> bool {
    if (!out_handle) {
      return false;
    }
    *out_handle = 0;
    HRESULT hr = aerogpu::device_set_fvf(hDevice, fvf);
    if (!Check(hr == S_OK, "SetFVF returned S_OK")) {
      return false;
    }
    std::lock_guard<std::mutex> lock(dev.mutex);
    if (!dev.vertex_decl) {
      return Check(false, "SetFVF must bind an internal vertex declaration");
    }
    *out_handle = dev.vertex_decl->handle;
    return Check(*out_handle != 0, "SetFVF produced non-zero input-layout handle");
  };

  aerogpu_handle_t layout_a0 = 0;
  aerogpu_handle_t layout_b0 = 0;
  aerogpu_handle_t layout_c0 = 0;
  aerogpu_handle_t layout_d0 = 0;
  aerogpu_handle_t layout_a1 = 0;
  aerogpu_handle_t layout_b1 = 0;
  aerogpu_handle_t layout_c1 = 0;
  aerogpu_handle_t layout_d1 = 0;

  if (!set_and_get_layout(kFvfA, &layout_a0)) {
    return false;
  }
  if (!set_and_get_layout(kFvfB, &layout_b0)) {
    return false;
  }
  if (!set_and_get_layout(kFvfC, &layout_c0)) {
    return false;
  }
  if (!set_and_get_layout(kFvfD, &layout_d0)) {
    return false;
  }
  // Repeat to validate caching (no new CREATE_INPUT_LAYOUT for the same FVF).
  if (!set_and_get_layout(kFvfA, &layout_a1)) {
    return false;
  }
  if (!set_and_get_layout(kFvfB, &layout_b1)) {
    return false;
  }
  if (!set_and_get_layout(kFvfC, &layout_c1)) {
    return false;
  }
  if (!set_and_get_layout(kFvfD, &layout_d1)) {
    return false;
  }

  if (!Check(layout_a0 == layout_a1, "FVF A input layout handle is cached")) {
    return false;
  }
  if (!Check(layout_b0 == layout_b1, "FVF B input layout handle is cached")) {
    return false;
  }
  if (!Check(layout_c0 == layout_c1, "FVF C input layout handle is cached")) {
    return false;
  }
  if (!Check(layout_d0 == layout_d1, "FVF D input layout handle is cached")) {
    return false;
  }

  dev.cmd.finalize();
  const uint8_t* buf = dev.cmd.data();
  const size_t len = dev.cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream")) {
    return false;
  }

  // Exactly one CREATE_INPUT_LAYOUT per distinct FVF.
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_INPUT_LAYOUT) == 4, "expected 4 CREATE_INPUT_LAYOUT packets")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_a[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0, 16, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };
  const D3DVERTEXELEMENT9_COMPAT expected_b[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0, 16, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0, 20, kD3dDeclTypeFloat1, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };
  const D3DVERTEXELEMENT9_COMPAT expected_c[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0, 16, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };
  const D3DVERTEXELEMENT9_COMPAT expected_d[] = {
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  const uint8_t* blob = nullptr;
  uint32_t blob_size = 0;
  if (!Check(FindCreateInputLayoutBlob(buf, len, layout_a0, &blob, &blob_size), "found CREATE_INPUT_LAYOUT for FVF A")) {
    return false;
  }
  if (!Check(BlobEqualsDecl(blob, blob_size, expected_a, std::size(expected_a)), "FVF A input-layout blob")) {
    return false;
  }

  blob = nullptr;
  blob_size = 0;
  if (!Check(FindCreateInputLayoutBlob(buf, len, layout_b0, &blob, &blob_size), "found CREATE_INPUT_LAYOUT for FVF B")) {
    return false;
  }
  if (!Check(BlobEqualsDecl(blob, blob_size, expected_b, std::size(expected_b)), "FVF B input-layout blob")) {
    return false;
  }

  blob = nullptr;
  blob_size = 0;
  if (!Check(FindCreateInputLayoutBlob(buf, len, layout_c0, &blob, &blob_size), "found CREATE_INPUT_LAYOUT for FVF C")) {
    return false;
  }
  if (!Check(BlobEqualsDecl(blob, blob_size, expected_c, std::size(expected_c)), "FVF C input-layout blob")) {
    return false;
  }

  blob = nullptr;
  blob_size = 0;
  if (!Check(FindCreateInputLayoutBlob(buf, len, layout_d0, &blob, &blob_size), "found CREATE_INPUT_LAYOUT for FVF D")) {
    return false;
  }
  if (!Check(BlobEqualsDecl(blob, blob_size, expected_d, std::size(expected_d)), "FVF D input-layout blob")) {
    return false;
  }

  // Ensure SET_INPUT_LAYOUT binds the expected handles at least once.
  auto saw_set = [&](aerogpu_handle_t handle) -> bool {
    for (const aerogpu_cmd_hdr* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
      if (hdr->size_bytes < sizeof(aerogpu_cmd_set_input_layout)) {
        continue;
      }
      const auto* s = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
      if (s->input_layout_handle == handle) {
        return true;
      }
    }
    return false;
  };

  if (!Check(saw_set(layout_a0), "SET_INPUT_LAYOUT binds FVF A handle")) {
    return false;
  }
  if (!Check(saw_set(layout_b0), "SET_INPUT_LAYOUT binds FVF B handle")) {
    return false;
  }
  if (!Check(saw_set(layout_c0), "SET_INPUT_LAYOUT binds FVF C handle")) {
    return false;
  }
  if (!Check(saw_set(layout_d0), "SET_INPUT_LAYOUT binds FVF D handle")) {
    return false;
  }

  return true;
}

} // namespace

int main() {
  if (!TestFvfVertexDeclTranslation()) {
    return 1;
  }
  if (!TestSetFvfTexcoordSizeBits()) {
    return 1;
  }
  return 0;
}
