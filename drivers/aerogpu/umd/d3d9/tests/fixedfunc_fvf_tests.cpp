#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <mutex>
#include <unordered_set>
#include <vector>

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_d3d9_fixedfunc_shaders.h"
#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_test_entrypoints.h"
#include "fixedfunc_test_constants.h"

namespace aerogpu {

namespace {

// Portable D3D9 FVF bits (from d3d9types.h).
constexpr uint32_t kD3dFvfXyz = 0x00000002u;
constexpr uint32_t kD3dFvfXyzRhw = 0x00000004u;
constexpr uint32_t kD3dFvfNormal = 0x00000010u;
constexpr uint32_t kD3dFvfDiffuse = 0x00000040u;
constexpr uint32_t kD3dFvfTex1 = 0x00000100u;
// D3DFVF_TEXCOORDSIZE*(0) bits (two bits at offset 16) for TEXCOORD0.
// Encoding: 0 -> float2, 1 -> float3, 2 -> float4, 3 -> float1.
constexpr uint32_t kD3dFvfTexCoordSize1_0 = 0x00030000u;
constexpr uint32_t kD3dFvfTexCoordSize3_0 = 0x00010000u;
constexpr uint32_t kD3dFvfTexCoordSize4_0 = 0x00020000u;
// D3DFVF_TEXCOORDSIZE3(1): `TEXCOORD1` is float3. For TEX1 FVFs, set 1 is unused,
// but some runtimes may leave garbage bits in the unused D3DFVF_TEXCOORDSIZE range.
constexpr uint32_t kD3dFvfTexCoordSize3_1 = 0x00040000u;

constexpr uint32_t kFvfXyzrhwDiffuse = kD3dFvfXyzRhw | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzrhwDiffuseTex1 = kD3dFvfXyzRhw | kD3dFvfDiffuse | kD3dFvfTex1;
constexpr uint32_t kFvfXyzrhwTex1 = kD3dFvfXyzRhw | kD3dFvfTex1;
constexpr uint32_t kFvfXyzDiffuse = kD3dFvfXyz | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzDiffuseTex1 = kD3dFvfXyz | kD3dFvfDiffuse | kD3dFvfTex1;
constexpr uint32_t kFvfXyzTex1 = kD3dFvfXyz | kD3dFvfTex1;
constexpr uint32_t kFvfXyzNormal = kD3dFvfXyz | kD3dFvfNormal;
constexpr uint32_t kFvfXyzNormalTex1 = kD3dFvfXyz | kD3dFvfNormal | kD3dFvfTex1;
constexpr uint32_t kFvfXyzNormalDiffuse = kD3dFvfXyz | kD3dFvfNormal | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzNormalDiffuseTex1 = kD3dFvfXyz | kD3dFvfNormal | kD3dFvfDiffuse | kD3dFvfTex1;

// D3D9 shader stage IDs used by the DDI (from d3d9umddi.h). Keep local numeric
// definitions so portable builds don't require the Windows SDK/WDK.
constexpr uint32_t kD3dShaderStageVs = 0u;
constexpr uint32_t kD3dShaderStagePs = 1u;

// D3DTSS_* texture stage state IDs (from d3d9types.h).
constexpr uint32_t kD3dTssColorOp = 1u;
constexpr uint32_t kD3dTssColorArg1 = 2u;
constexpr uint32_t kD3dTssColorArg2 = 3u;
constexpr uint32_t kD3dTssAlphaOp = 4u;
constexpr uint32_t kD3dTssAlphaArg1 = 5u;
constexpr uint32_t kD3dTssAlphaArg2 = 6u;
// D3DTEXTUREOP values (from d3d9types.h).
constexpr uint32_t kD3dTopDisable = 1u;
constexpr uint32_t kD3dTopSelectArg1 = 2u;
constexpr uint32_t kD3dTopSelectArg2 = 3u;
constexpr uint32_t kD3dTopModulate = 4u;
constexpr uint32_t kD3dTopModulate2x = 5u;
constexpr uint32_t kD3dTopModulate4x = 6u;
constexpr uint32_t kD3dTopAdd = 7u;
constexpr uint32_t kD3dTopAddSigned = 8u;
constexpr uint32_t kD3dTopSubtract = 10u;
constexpr uint32_t kD3dTopBlendDiffuseAlpha = 12u;
constexpr uint32_t kD3dTopBlendTextureAlpha = 13u;
// Intentionally unsupported by the fixed-function texture stage subset (used to validate
// draw-time guardrails).
constexpr uint32_t kD3dTopAddSmooth = 11u; // D3DTOP_ADDSMOOTH

// D3DTA_* source selector (from d3d9types.h).
constexpr uint32_t kD3dTaDiffuse = 0u;
constexpr uint32_t kD3dTaCurrent = 1u;
constexpr uint32_t kD3dTaTexture = 2u;
constexpr uint32_t kD3dTaTFactor = 3u;
constexpr uint32_t kD3dTaSpecular = 4u;
constexpr uint32_t kD3dTaComplement = 0x10u;
constexpr uint32_t kD3dTaAlphaReplicate = 0x20u;

// D3DRS_* render state IDs (from d3d9types.h).
constexpr uint32_t kD3dRsAmbient = 26u;       // D3DRS_AMBIENT
constexpr uint32_t kD3dRsLighting = 137u;     // D3DRS_LIGHTING
constexpr uint32_t kD3dRsTextureFactor = 60u; // D3DRS_TEXTUREFACTOR

// D3DTRANSFORMSTATETYPE numeric values (from d3d9types.h).
constexpr uint32_t kD3dTransformView = 2u;
constexpr uint32_t kD3dTransformProjection = 3u;
constexpr uint32_t kD3dTransformWorld0 = 256u;

// Pixel shader instruction tokens (ps_2_0).
constexpr uint32_t kPsOpAdd = 0x04000002u;
constexpr uint32_t kPsOpMul = 0x04000005u;
constexpr uint32_t kPsOpTexld = 0x04000042u;
// Sampler source tokens (`sN`), as encoded by the fixed-function ps_2_0 token
// builder (`src_sampler` in `aerogpu_d3d9_driver.cpp`).
constexpr uint32_t kPsSampler0 = 0x20E40800u;
constexpr uint32_t kPsSampler1 = 0x20E40801u;
constexpr uint32_t kPsSampler2 = 0x20E40802u;
constexpr uint32_t kPsSampler3 = 0x20E40803u;
// Source register tokens used by the fixed-function ps_2_0 token builder
// (`fixedfunc_ps20` in `aerogpu_d3d9_driver.cpp`). These validate that stage0
// argument modifiers are encoded into the generated shader bytecode.
constexpr uint32_t kPsSrcTemp0Comp = 0x06E40000u;  // (1 - r0.xyzw)
constexpr uint32_t kPsSrcTemp0W = 0x00FF0000u;     // r0.wwww (alpha replicate)
constexpr uint32_t kPsSrcTemp0WComp = 0x06FF0000u; // (1 - r0.wwww) (complement + alpha replicate)
constexpr uint32_t kPsSrcInput0Comp = 0x16E40000u; // (1 - v0.xyzw)
constexpr uint32_t kPsSrcInput0W = 0x10FF0000u;    // v0.wwww (alpha replicate)
constexpr uint32_t kPsSrcInput0WComp = 0x16FF0000u; // (1 - v0.wwww) (complement + alpha replicate)

uint32_t F32Bits(float f) {
  uint32_t u = 0;
  std::memcpy(&u, &f, sizeof(u));
  return u;
}

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

template <size_t N>
bool ShaderBytecodeEquals(const Shader* shader, const uint32_t (&expected)[N]) {
  if (!shader) {
    return false;
  }
  if (shader->bytecode.size() != sizeof(expected)) {
    return false;
  }
  return std::memcmp(shader->bytecode.data(), expected, sizeof(expected)) == 0;
}

bool ShaderContainsToken(const Shader* shader, uint32_t token) {
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

size_t ShaderCountToken(const Shader* shader, uint32_t token) {
  if (!shader) {
    return 0;
  }
  const size_t size = shader->bytecode.size();
  if (size < sizeof(uint32_t) || (size % sizeof(uint32_t)) != 0) {
    return 0;
  }
  size_t count = 0;
  for (size_t off = 0; off < size; off += sizeof(uint32_t)) {
    uint32_t w = 0;
    std::memcpy(&w, shader->bytecode.data() + off, sizeof(uint32_t));
    if (w == token) {
      ++count;
    }
  }
  return count;
}

uint32_t ShaderTexldSamplerMask(const Shader* shader) {
  if (!shader) {
    return 0;
  }
  const size_t size = shader->bytecode.size();
  if (size < sizeof(uint32_t) || (size % sizeof(uint32_t)) != 0) {
    return 0;
  }

  const uint8_t* bytes = shader->bytecode.data();
  const size_t word_count = size / sizeof(uint32_t);
  if (word_count < 2) {
    return 0;
  }

  auto ReadWord = [&](size_t idx) -> uint32_t {
    uint32_t w = 0;
    std::memcpy(&w, bytes + idx * sizeof(uint32_t), sizeof(uint32_t));
    return w;
  };

  uint32_t mask = 0;
  // Skip version token at word 0.
  for (size_t i = 1; i < word_count;) {
    const uint32_t inst = ReadWord(i);
    if (inst == 0x0000FFFFu) { // end
      break;
    }
    const uint32_t len = inst >> 24;
    if (len == 0 || i + len > word_count) {
      break;
    }
    if (inst == kPsOpTexld && len >= 4) {
      const uint32_t sampler = ReadWord(i + 3);
      if (sampler >= kPsSampler0) {
        const uint32_t reg = sampler - kPsSampler0;
        if (reg < 16) {
          mask |= (1u << reg);
        }
      }
    }
    i += len;
  }
  return mask;
}

bool ShaderReferencesConstRegister(const Shader* shader, uint32_t reg_index) {
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
    // D3D9 shader token encoding: constant register operands are encoded with
    // register-type == CONST (0x2 in the high register-type field).
    if ((w & 0x70000000u) != 0x20000000u) {
      continue;
    }
    if ((w & 0x7FFu) == (reg_index & 0x7FFu)) {
      return true;
    }
  }
  return false;
}

size_t StreamBytesUsed(const uint8_t* buf, size_t capacity) {
  if (!buf || capacity < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }

  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  const size_t used = stream->size_bytes;
  if (used < sizeof(aerogpu_cmd_stream_header) || used > capacity) {
    return 0;
  }
  return used;
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
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (stream_len == 0) {
    return 0;
  }

  size_t count = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      count++;
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return count;
}

std::vector<const aerogpu_cmd_hdr*> CollectOpcodes(const uint8_t* buf, size_t capacity, uint32_t opcode) {
  std::vector<const aerogpu_cmd_hdr*> out;
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (stream_len == 0) {
    return out;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      out.push_back(hdr);
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return out;
}

// Helper (defined later): count SET_SHADER_CONSTANTS_F uploads for a given VS
// constant range.
size_t CountVsConstantUploads(const uint8_t* buf,
                              size_t capacity,
                              uint32_t start_register,
                              uint32_t vec4_count);

// Helper (defined later): locate the payload for a VS SET_SHADER_CONSTANTS_F
// upload matching the requested register range.
const float* FindVsConstantsPayload(const uint8_t* buf,
                                    size_t capacity,
                                    uint32_t start_register,
                                    uint32_t vec4_count);

struct CleanupDevice {
  D3D9DDI_ADAPTERFUNCS adapter_funcs{};
  D3D9DDI_DEVICEFUNCS device_funcs{};  
  D3DDDI_HADAPTER hAdapter{};
  D3DDDI_HDEVICE hDevice{};
  std::vector<D3DDDI_HRESOURCE> resources{};
  std::vector<D3D9DDI_HVERTEXDECL> vertex_decls{};
  std::vector<D3D9DDI_HSHADER> shaders{};
  bool has_adapter = false;
  bool has_device = false;

  ~CleanupDevice() {
    if (has_device && device_funcs.pfnDestroyShader) {
      for (auto& s : shaders) {
        if (s.pDrvPrivate) {
          device_funcs.pfnDestroyShader(hDevice, s);
        }
      }
    }
    if (has_device && device_funcs.pfnDestroyVertexDecl) {
      for (auto& d : vertex_decls) {
        if (d.pDrvPrivate) {
          device_funcs.pfnDestroyVertexDecl(hDevice, d);
        }
      }
    }
    if (has_device && device_funcs.pfnDestroyResource) {
      for (auto& r : resources) {
        if (r.pDrvPrivate) {
          device_funcs.pfnDestroyResource(hDevice, r);
        }
      }
    }
    if (has_device && device_funcs.pfnDestroyDevice) {
      device_funcs.pfnDestroyDevice(hDevice);
    }
    if (has_adapter && adapter_funcs.pfnCloseAdapter) {
      adapter_funcs.pfnCloseAdapter(hAdapter);
    }
  }
};

bool CreateDevice(CleanupDevice* cleanup) {
  if (!cleanup) {
    return false;
  }

  D3DDDIARG_OPENADAPTER2 open{};
  open.Interface = 1;
  open.Version = 1;
  D3DDDI_ADAPTERCALLBACKS callbacks{};
  D3DDDI_ADAPTERCALLBACKS2 callbacks2{};
  open.pAdapterCallbacks = &callbacks;
  open.pAdapterCallbacks2 = &callbacks2;
  open.pAdapterFuncs = &cleanup->adapter_funcs;

  HRESULT hr = ::OpenAdapter2(&open);
  if (!Check(hr == S_OK, "OpenAdapter2")) {
    return false;
  }
  if (!Check(open.hAdapter.pDrvPrivate != nullptr, "OpenAdapter2 returned adapter handle")) {
    return false;
  }
  cleanup->hAdapter = open.hAdapter;
  cleanup->has_adapter = true;

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup->adapter_funcs.pfnCreateDevice(&create_dev, &cleanup->device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  if (!Check(create_dev.hDevice.pDrvPrivate != nullptr, "CreateDevice returned device handle")) {
    return false;
  }
  cleanup->hDevice = create_dev.hDevice;
  cleanup->has_device = true;

  if (!Check(cleanup->device_funcs.pfnSetFVF != nullptr, "pfnSetFVF is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnCreateVertexDecl != nullptr, "pfnCreateVertexDecl is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnSetVertexDecl != nullptr, "pfnSetVertexDecl is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnCreateResource != nullptr, "pfnCreateResource is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnSetTexture != nullptr, "pfnSetTexture is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnDestroyResource != nullptr, "pfnDestroyResource is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnCreateShader != nullptr, "pfnCreateShader is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnSetShader != nullptr, "pfnSetShader is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnDestroyShader != nullptr, "pfnDestroyShader is available")) {
    return false;
  }
  return true;
}

bool CreateDummyTexture(CleanupDevice* cleanup, D3DDDI_HRESOURCE* out_tex) {
  if (!cleanup || !out_tex) {
    return false;
  }

  // D3DFMT_X8R8G8B8 = 22.
  D3D9DDIARG_CREATERESOURCE create_res{};
  create_res.type = 3u; // D3DRTYPE_TEXTURE (conventional value; AeroGPU currently treats this as metadata)
  create_res.format = 22u;
  create_res.width = 2;
  create_res.height = 2;
  create_res.depth = 1;
  create_res.mip_levels = 1;
  create_res.usage = 0;
  create_res.pool = 0;
  create_res.size = 0;
  create_res.hResource.pDrvPrivate = nullptr;
  create_res.pSharedHandle = nullptr;
  create_res.pPrivateDriverData = nullptr;
  create_res.PrivateDriverDataSize = 0;
  create_res.wddm_hAllocation = 0;

  HRESULT hr = cleanup->device_funcs.pfnCreateResource(cleanup->hDevice, &create_res);
  if (!Check(hr == S_OK, "CreateResource(texture2d)")) {
    return false;
  }
  if (!Check(create_res.hResource.pDrvPrivate != nullptr, "CreateResource returned hResource")) {
    return false;
  }

  cleanup->resources.push_back(create_res.hResource);
  *out_tex = create_res.hResource;
  return true;
}

struct VertexXyzrhwDiffuse {
  float x;
  float y;
  float z;
  float rhw;
  uint32_t color;
};

struct VertexXyzrhwDiffuseTex1 {
  float x;
  float y;
  float z;
  float rhw;
  uint32_t color;
  float u;
  float v;
};

struct VertexXyzrhwDiffuseTex1F1 {
  float x;
  float y;
  float z;
  float rhw;
  uint32_t color;
  float u;
};

struct VertexXyzrhwDiffuseTex1F3 {
  float x;
  float y;
  float z;
  float rhw;
  uint32_t color;
  float u;
  float v;
  float w;
};

struct VertexXyzrhwDiffuseTex1F4 {
  float x;
  float y;
  float z;
  float rhw;
  uint32_t color;
  float u;
  float v;
  float w;
  float q;
};

struct VertexXyzrhwTex1 {
  float x;
  float y;
  float z;
  float rhw;
  float u;
  float v;
};

struct VertexXyzrhwTex1F1 {
  float x;
  float y;
  float z;
  float rhw;
  float u;
};

struct VertexXyzrhwTex1F3 {
  float x;
  float y;
  float z;
  float rhw;
  float u;
  float v;
  float w;
};

struct VertexXyzrhwTex1F4 {
  float x;
  float y;
  float z;
  float rhw;
  float u;
  float v;
  float w;
  float q;
};

struct VertexXyzDiffuse {
  float x;
  float y;
  float z;
  uint32_t color;
};

struct VertexXyzDiffuseTex1 {
  float x;
  float y;
  float z;
  uint32_t color;
  float u;
  float v;
};

struct VertexXyzDiffuseTex1F1 {
  float x;
  float y;
  float z;
  uint32_t color;
  float u;
};

struct VertexXyzDiffuseTex1F3 {
  float x;
  float y;
  float z;
  uint32_t color;
  float u;
  float v;
  float w;
};

struct VertexXyzDiffuseTex1F4 {
  float x;
  float y;
  float z;
  uint32_t color;
  float u;
  float v;
  float w;
  float q;
};

struct VertexXyzTex1 {
  float x;
  float y;
  float z;
  float u;
  float v;
};

struct VertexXyzTex1F1 {
  float x;
  float y;
  float z;
  float u;
};

struct VertexXyzTex1F3 {
  float x;
  float y;
  float z;
  float u;
  float v;
  float w;
};

struct VertexXyzTex1F4 {
  float x;
  float y;
  float z;
  float u;
  float v;
  float w;
  float q;
};

struct VertexXyzNormal {
  float x;
  float y;
  float z;
  float nx;
  float ny;
  float nz;
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

struct VertexXyzNormalDiffuse {
  float x;
  float y;
  float z;
  float nx;
  float ny;
  float nz;
  uint32_t color;
};

struct VertexXyzNormalDiffuseTex1 {
  float x;
  float y;
  float z;
  float nx;
  float ny;
  float nz;
  uint32_t color;
  float u;
  float v;
};

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

constexpr uint8_t kD3dDeclTypeFloat2 = 1;
constexpr uint8_t kD3dDeclTypeFloat3 = 2;
constexpr uint8_t kD3dDeclTypeFloat4 = 3;
constexpr uint8_t kD3dDeclTypeFloat1 = 0;
constexpr uint8_t kD3dDeclTypeD3dColor = 4;
constexpr uint8_t kD3dDeclTypeUnused = 17;

constexpr uint8_t kD3dDeclMethodDefault = 0;

constexpr uint8_t kD3dDeclUsagePosition = 0;
constexpr uint8_t kD3dDeclUsageNormal = 3;
constexpr uint8_t kD3dDeclUsageTexcoord = 5;
constexpr uint8_t kD3dDeclUsagePositionT = 9;
constexpr uint8_t kD3dDeclUsageColor = 10;

bool TestFvfXyzrhwDiffuseEmitsSaneCommands() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  aerogpu_handle_t expected_input_layout = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzrhwDiffuse);
    if (variant != FixedFuncVariant::NONE) {
      const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
      if (pipe.vertex_decl) {
        expected_input_layout = pipe.vertex_decl->handle;
      }
    }
  }
  if (!Check(expected_input_layout != 0, "SetFVF created internal input layout")) {
    return false;
  }

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle)")) {
    return false;
  }

  // With no bound texture, the fixed-function fallback should not select a
  // texture-sampling PS even though the D3D9 default stage0 COLOROP is MODULATE.
  // (This is a common configuration for untextured apps that never touch stage
  // state but rely on vertex diffuse.)
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(dev->fvf);
    if (!Check(variant != FixedFuncVariant::NONE, "fixed-function variant recognized")) {
      return false;
    }
    const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
    if (!Check(pipe.ps != nullptr, "fixed-function PS created")) {
      return false;
    }
    if (!Check(dev->ps == pipe.ps, "fixed-function PS is bound (no texture)")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld),
               "fixed-function PS does not contain texld (no texture -> passthrough)")) {
      return false;
    }
  }

  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->up_vertex_buffer) {
      expected_vb = dev->up_vertex_buffer->handle;
    }
  }
  if (!Check(expected_vb != 0, "DrawPrimitiveUP created scratch vertex buffer")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZRHW|DIFFUSE)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) >= 2, "CREATE_SHADER_DXBC emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT) >= 1, "SET_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS) >= 1, "SET_VERTEX_BUFFERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }

  // Validate shader creation includes both stages.
  bool saw_vs = false;
  bool saw_ps = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
    const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
    if (cs->stage == AEROGPU_SHADER_STAGE_VERTEX) {
      saw_vs = true;
    } else if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
      saw_ps = true;
    }
  }
  if (!Check(saw_vs && saw_ps, "CREATE_SHADER_DXBC includes VS and PS stages")) {
    return false;
  }

  // Validate the input layout being set matches the internal FVF declaration.
  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal FVF layout handle")) {
    return false;
  }

  // Validate at least one vertex buffer binding references the scratch UP buffer.
  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(reinterpret_cast<const uint8_t*>(svb) +
                                                                                  sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzrhwDiffuse)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds scratch UP buffer")) {
    return false;
  }

  // Validate draw parameters (trianglelist => 3 vertices).
  bool saw_draw3 = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_DRAW)) {
    const auto* d = reinterpret_cast<const aerogpu_cmd_draw*>(hdr);
    if (d->vertex_count == 3 && d->instance_count == 1) {
      saw_draw3 = true;
      break;
    }
  }
  if (!Check(saw_draw3, "DRAW has expected vertex_count=3 instance_count=1")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs != 0 && last_bind->ps != 0, "BIND_SHADERS binds non-zero VS/PS")) {
    return false;
  }

  return true;
}

bool TestFvfXyzDiffuseEmitsInputLayoutAndShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  // XYZ vertices are transformed to clip-space by the fixed-function WVP vertex
  // shader (internal VS + reserved constant range `c240..c243`). With identity
  // transforms, these inputs are already clip-space.
  const VertexXyzDiffuse tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) >= 2, "CREATE_SHADER_DXBC emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT) >= 1, "SET_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS) >= 1, "SET_VERTEX_BUFFERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs != 0 && last_bind->ps != 0, "BIND_SHADERS binds non-zero VS/PS")) {
    return false;
  }

  return true;
}

bool TestFvfXyzDiffuseEmitsTransformConstantsAndDecl() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_decl[] = {
      // stream, offset, type, method, usage, usage_index
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  // Fixed-function emulation for XYZ vertices uses a WVP vertex shader and
  // uploads the matrix into reserved VS constants c240..c243 as column vectors.
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };

  aerogpu_handle_t expected_input_layout = 0;
  aerogpu_handle_t expected_vb = 0;
  bool decl_ok = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzDiffuse);
    if (variant != FixedFuncVariant::NONE) {
      const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
      if (pipe.vertex_decl) {
        expected_input_layout = pipe.vertex_decl->handle;
        const auto& blob = pipe.vertex_decl->blob;
        decl_ok = (blob.size() == sizeof(expected_decl)) &&
                  (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
      }
    }
  }
  // Set a simple world translation; view/projection are identity.
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }
  if (!Check(expected_input_layout != 0, "SetFVF XYZ|DIFFUSE created internal input layout")) {
    return false;
  }
  if (!Check(decl_ok, "XYZ|DIFFUSE internal vertex decl matches expected layout")) {
    return false;
  }

  const VertexXyzDiffuse tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz diffuse)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzDiffuse);
    if (variant != FixedFuncVariant::NONE) {
      const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
      if (!Check(pipe.vs != nullptr, "fixedfunc VS created (XYZ|DIFFUSE)")) {
        return false;
      }
      if (!Check(dev->vs == pipe.vs, "XYZ|DIFFUSE binds fixed-function VS")) {
        return false;
      }
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColor),
               "XYZ|DIFFUSE VS bytecode matches kVsWvpPosColor")) {
      return false;
    }
    if (dev->up_vertex_buffer) {
      expected_vb = dev->up_vertex_buffer->handle;
      if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri),
                 "scratch VB storage contains uploaded vertices")) {
        return false;
      }
      if (!Check(std::memcmp(dev->up_vertex_buffer->storage.data(), tri, sizeof(tri)) == 0,
                 "scratch VB contains original XYZ|DIFFUSE vertices (no CPU conversion)")) {
        return false;
      }
    }
  }
  if (!Check(expected_vb != 0, "scratch VB handle non-zero (XYZ|DIFFUSE)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE WVP VS)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE) >= 1, "UPLOAD_RESOURCE emitted")) {
    return false;
  }

  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal XYZ|DIFFUSE layout handle")) {
    return false;
  }

  // Validate at least one vertex buffer binding references the scratch UP buffer
  // with the original stride.
  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(reinterpret_cast<const uint8_t*>(svb) +
                                                                                  sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzDiffuse)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds scratch UP buffer (XYZ|DIFFUSE original stride)")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != kFixedfuncMatrixStartRegister || sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (XYZ|DIFFUSE)")) {
    return false;
  }

  return true;
}

bool TestFvfXyzDiffuseWvpUploadNotDuplicatedByFirstDraw() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Activate fixed-function XYZ|DIFFUSE (WVP VS path).
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  // Provide a simple non-identity WORLD0 so WVP is observable.
  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  D3DMATRIX world{};
  world.m[0][0] = 1.0f;
  world.m[1][1] = 1.0f;
  world.m[2][2] = 1.0f;
  world.m[3][3] = 1.0f;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }

  const VertexXyzDiffuse tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz diffuse)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE WVP caching)")) {
    return false;
  }

  // Ensure the first draw doesn't redundantly re-upload WVP constants if
  // SetTransform already uploaded them eagerly.
  size_t wvp_uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX ||
        sc->start_register != kFixedfuncMatrixStartRegister ||
        sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (!Check(hdr->size_bytes >= need, "SET_SHADER_CONSTANTS_F contains WVP payload")) {
      return false;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      ++wvp_uploads;
    }
  }
  if (!Check(wvp_uploads == 1, "WVP constants uploaded once (cached)")) {
    return false;
  }

  return true;
}

bool TestFvfXyzDiffuseRedundantSetTransformDoesNotReuploadWvp() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Activate fixed-function XYZ|DIFFUSE (WVP VS path).
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  // Provide a simple non-identity WORLD0 so WVP is observable.
  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  D3DMATRIX world{};
  world.m[0][0] = 1.0f;
  world.m[1][1] = 1.0f;
  world.m[2][2] = 1.0f;
  world.m[3][3] = 1.0f;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;

  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD) initial")) {
    return false;
  }

  const VertexXyzDiffuse tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz diffuse) first")) {
    return false;
  }

  // Redundantly set the same matrix again; should not force a fixed-function WVP
  // re-upload on the next draw.
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD) redundant")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz diffuse) second")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE redundant SetTransform)")) {
    return false;
  }

  size_t wvp_uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX ||
        sc->start_register != kFixedfuncMatrixStartRegister ||
        sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (!Check(hdr->size_bytes >= need, "SET_SHADER_CONSTANTS_F contains WVP payload")) {
      return false;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      ++wvp_uploads;
    }
  }
  if (!Check(wvp_uploads == 1, "WVP constants uploaded once despite redundant SetTransform")) {
    return false;
  }

  return true;
}

bool TestFvfXyzDiffuseMultiplyTransformEagerUploadNotDuplicatedByFirstDraw() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnMultiplyTransform != nullptr, "pfnMultiplyTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Activate fixed-function XYZ|DIFFUSE (WVP VS path).
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  // Apply a simple non-identity WORLD0 via MultiplyTransform so WVP is observable.
  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  D3DMATRIX world_mul{};
  world_mul.m[0][0] = 1.0f;
  world_mul.m[1][1] = 1.0f;
  world_mul.m[2][2] = 1.0f;
  world_mul.m[3][3] = 1.0f;
  world_mul.m[3][0] = tx;
  world_mul.m[3][1] = ty;
  world_mul.m[3][2] = tz;

  hr = cleanup.device_funcs.pfnMultiplyTransform(cleanup.hDevice, kD3dTransformWorld0, &world_mul);
  if (!Check(hr == S_OK, "MultiplyTransform(WORLD0, translation)")) {
    return false;
  }
  // MultiplyTransform eagerly refreshes the fixed-function WVP constant range
  // when WVP rendering is active.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_matrix_dirty, "MultiplyTransform eagerly cleared fixedfunc_matrix_dirty")) {
      return false;
    }
  }

  const VertexXyzDiffuse tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz diffuse)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE MultiplyTransform WVP caching)")) {
    return false;
  }

  // Ensure the first draw doesn't redundantly re-upload WVP constants if
  // MultiplyTransform already uploaded them eagerly.
  size_t wvp_uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX ||
        sc->start_register != kFixedfuncMatrixStartRegister ||
        sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (!Check(hdr->size_bytes >= need, "SET_SHADER_CONSTANTS_F contains WVP payload")) {
      return false;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      ++wvp_uploads;
    }
  }
  if (!Check(wvp_uploads == 1, "WVP constants uploaded once (MultiplyTransform cached)")) {
    return false;
  }

  return true;
}

bool TestFvfXyzDiffuseWvpDirtyAfterUserVsAndConstClobber() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShaderConstF != nullptr, "pfnSetShaderConstF is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Use a fixed-function XYZ|DIFFUSE draw so WVP constants are required.
  const VertexXyzDiffuse tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };

  // First draw: uploads WVP and clears the dirty flag.
  dev->cmd.reset();
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(initial XYZ|DIFFUSE)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(initial XYZ|DIFFUSE)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count) == 1,
             "initial draw emits one WVP constant upload")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_matrix_dirty, "initial draw cleared fixedfunc_matrix_dirty")) {
      return false;
    }
  }

  // If the app writes overlapping VS constants (c240..c243), the fixed-function WVP
  // constants must be treated as clobbered and re-uploaded.
  const float junk_vec4[4] = {123.0f, 456.0f, 789.0f, 1011.0f};
  hr = cleanup.device_funcs.pfnSetShaderConstF(
      cleanup.hDevice, kD3dShaderStageVs, /*start_reg=*/kFixedfuncMatrixStartRegister, junk_vec4, 1);
  if (!Check(hr == S_OK, "SetShaderConstF(VS, fixedfunc WVP start, 1)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fixedfunc_matrix_dirty, "SetShaderConstF overlap marks fixedfunc_matrix_dirty")) {
      return false;
    }
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(after const clobber)")) {
    return false;
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(after const clobber)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count) == 1,
             "WVP constant upload re-emitted after const clobber")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_matrix_dirty, "const-clobber draw cleared fixedfunc_matrix_dirty")) {
      return false;
    }
  }

  // If the app binds a user VS, it may write overlapping constants. Ensure the
  // driver forces a WVP constant re-upload when switching back to fixed-function.
  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStageVs,
                                            fixedfunc::kVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS user)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fixedfunc_matrix_dirty, "binding user VS marks fixedfunc_matrix_dirty")) {
      return false;
    }
  }

  // Unbind the user VS. This call should switch back to fixed-function pipeline
  // and re-upload WVP constants immediately (without waiting for a draw).
  D3D9DDI_HSHADER hNull{};
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStageVs, hNull);
  if (!Check(hr == S_OK, "SetShader(VS NULL)")) {
    return false;
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(after VS unbind)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count) == 1,
             "WVP constant upload re-emitted after switching back from user VS")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_matrix_dirty, "SetShader(VS NULL) cleared fixedfunc_matrix_dirty")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzNormalDiffuseLightingDirtyAfterUserVsAndConstClobber() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShaderConstF != nullptr, "pfnSetShaderConstF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateShader != nullptr, "pfnCreateShader is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShader != nullptr, "pfnSetShader is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Use a fixed-function XYZ|NORMAL|DIFFUSE draw so lighting constants are required.
  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  // First draw: uploads lighting constants and clears the dirty flag.
  dev->cmd.reset();
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF000000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.0f, 0.0f, -1.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(initial lit draw)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(initial lit draw)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "initial draw emits one lighting constant upload")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_lighting_dirty, "initial draw cleared fixedfunc_lighting_dirty")) {
      return false;
    }
  }

  // If the app writes overlapping VS constants (c208..c236), the fixed-function lighting
  // constants must be treated as clobbered and re-uploaded.
  const float junk_vec4[4] = {123.0f, 456.0f, 789.0f, 1011.0f};
  hr = cleanup.device_funcs.pfnSetShaderConstF(
      cleanup.hDevice, kD3dShaderStageVs, /*start_reg=*/kFixedfuncLightingStartRegister, junk_vec4, 1);
  if (!Check(hr == S_OK, "SetShaderConstF(VS, fixedfunc lighting start, 1)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fixedfunc_lighting_dirty, "SetShaderConstF overlap marks fixedfunc_lighting_dirty")) {
      return false;
    }
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(after lighting const clobber)")) {
    return false;
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(after lighting const clobber)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "lighting constant upload re-emitted after const clobber")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_lighting_dirty, "const-clobber draw cleared fixedfunc_lighting_dirty")) {
      return false;
    }
  }

  // If the app binds a user VS, it may write overlapping constants. Ensure the
  // driver forces a lighting constant re-upload when switching back to fixed-function.
  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStageVs,
                                            fixedfunc::kVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS user)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fixedfunc_lighting_dirty, "binding user VS marks fixedfunc_lighting_dirty")) {
      return false;
    }
  }

  // Simulate a user shader clobbering the reserved fixed-function lighting constant range.
  hr = cleanup.device_funcs.pfnSetShaderConstF(
      cleanup.hDevice, kD3dShaderStageVs, /*start_reg=*/kFixedfuncLightingStartRegister, junk_vec4, 1);
  if (!Check(hr == S_OK, "SetShaderConstF(VS, fixedfunc lighting start, 1; user VS bound)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fixedfunc_lighting_dirty,
               "SetShaderConstF overlap keeps fixedfunc_lighting_dirty while user VS is bound")) {
      return false;
    }
  }

  // Unbind the user VS. This call should switch back to fixed-function pipeline
  // and refresh lighting constants once (either eagerly here or lazily on the
  // next draw) if the user shader clobbered the reserved range.
  D3D9DDI_HSHADER hNull{};
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStageVs, hNull);
  if (!Check(hr == S_OK, "SetShader(VS NULL)")) {
    return false;
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(after VS unbind)")) {
    return false;
  }
  const size_t unbind_uploads =
      CountVsConstantUploads(buf, len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  if (!Check(unbind_uploads <= 1, "after VS unbind: at most one lighting constant upload")) {
    return false;
  }

  // Perform a draw. If lighting constants weren't refreshed eagerly above, this
  // draw must refresh them before executing.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(after VS unbind)")) {
    return false;
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(draw after VS unbind)")) {
    return false;
  }
  const size_t draw_uploads =
      CountVsConstantUploads(buf, len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  if (!Check(draw_uploads <= 1, "draw after VS unbind: at most one lighting constant upload")) {
    return false;
  }
  if (!Check(unbind_uploads + draw_uploads == 1,
             "lighting constants refreshed once after switching back from user VS")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_lighting_dirty, "draw after VS unbind cleared fixedfunc_lighting_dirty")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzDiffuseRedundantSetFvfDoesNotReuploadWvp() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  const VertexXyzDiffuse tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz diffuse) first")) {
    return false;
  }

  // Many D3D9 runtimes set the same FVF repeatedly. This should not cause the
  // fixed-function WVP constant registers to be redundantly re-uploaded.
  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE) redundant")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz diffuse) second")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE redundant SetFVF)")) {
    return false;
  }

  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, 0.0f,
      0.0f, 1.0f, 0.0f, 0.0f,
      0.0f, 0.0f, 1.0f, 0.0f,
      0.0f, 0.0f, 0.0f, 1.0f,
  };

  size_t wvp_uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX ||
        sc->start_register != kFixedfuncMatrixStartRegister ||
        sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (!Check(hdr->size_bytes >= need, "SET_SHADER_CONSTANTS_F contains WVP payload")) {
      return false;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      ++wvp_uploads;
    }
  }
  if (!Check(wvp_uploads == 1, "WVP constants uploaded once despite redundant SetFVF")) {
    return false;
  }

  return true;
}

bool TestSetShaderConstFDedupSkipsRedundantUpload() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShaderConstF != nullptr, "pfnSetShaderConstF is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  const float data[8] = {
      1.0f, 2.0f, 3.0f, 4.0f,
      5.0f, 6.0f, 7.0f, 8.0f,
  };
  HRESULT hr = cleanup.device_funcs.pfnSetShaderConstF(cleanup.hDevice,
                                                       kD3dShaderStageVs,
                                                       /*start_reg=*/0,
                                                       data,
                                                       /*vec4_count=*/2);
  if (!Check(hr == S_OK, "SetShaderConstF(VS, c0..c1) first")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetShaderConstF(cleanup.hDevice,
                                               kD3dShaderStageVs,
                                               /*start_reg=*/0,
                                               data,
                                               /*vec4_count=*/2);
  if (!Check(hr == S_OK, "SetShaderConstF(VS, c0..c1) second (redundant)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(shader const dedup)")) {
    return false;
  }

  size_t uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX || sc->start_register != 0 || sc->vec4_count != 2) {
      continue;
    }
    const size_t need = sizeof(*sc) + sizeof(data);
    if (!Check(hdr->size_bytes >= need, "SET_SHADER_CONSTANTS_F contains payload")) {
      return false;
    }
    const float* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
    if (std::memcmp(payload, data, sizeof(data)) == 0) {
      ++uploads;
    }
  }
  if (!Check(uploads == 1, "SetShaderConstF dedup emits one upload")) {
    return false;
  }

  return true;
}

bool TestSetShaderConstFStateBlockCapturesRedundantSet() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShaderConstF != nullptr, "pfnSetShaderConstF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Seed a known constant value.
  const float a[4] = {1.0f, 2.0f, 3.0f, 4.0f};
  const float b[4] = {9.0f, 10.0f, 11.0f, 12.0f};
  HRESULT hr = cleanup.device_funcs.pfnSetShaderConstF(cleanup.hDevice,
                                                       kD3dShaderStageVs,
                                                       /*start_reg=*/0,
                                                       a,
                                                       /*vec4_count=*/1);
  if (!Check(hr == S_OK, "SetShaderConstF seed")) {
    return false;
  }

  // Record a state block that redundantly sets the same constant again.
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetShaderConstF(cleanup.hDevice,
                                               kD3dShaderStageVs,
                                               /*start_reg=*/0,
                                               a,
                                               /*vec4_count=*/1);
  if (!Check(hr == S_OK, "SetShaderConstF redundant (recorded)")) {
    return false;
  }
  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Change the constant to a different value so ApplyStateBlock must re-upload.
  hr = cleanup.device_funcs.pfnSetShaderConstF(cleanup.hDevice,
                                               kD3dShaderStageVs,
                                               /*start_reg=*/0,
                                               b,
                                               /*vec4_count=*/1);
  if (!Check(hr == S_OK, "SetShaderConstF change-to-B")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock const)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_a = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX || sc->start_register != 0 || sc->vec4_count != 1) {
      continue;
    }
    const size_t need = sizeof(*sc) + sizeof(a);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
    if (std::memcmp(payload, a, sizeof(a)) == 0) {
      saw_a = true;
      break;
    }
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return Check(saw_a, "ApplyStateBlock re-uploads recorded constants");
}

bool TestApplyStateBlockUploadsTextureFactorConstantWhenUsed() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: select TFACTOR for both color and alpha so the fixed-function PS
  // references c255.
  if (!SetTextureStageState(/*stage=*/0, kD3dTssColorOp, kD3dTopSelectArg1, "SetTextureStageState(COLOROP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0, kD3dTssColorArg1, kD3dTaTFactor, "SetTextureStageState(COLORARG1=TFACTOR)")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaArg1, kD3dTaTFactor, "SetTextureStageState(ALPHAARG1=TFACTOR)")) {
    return false;
  }
  // Ensure the stage chain terminates at stage0.
  if (!SetTextureStageState(/*stage=*/1, kD3dTssColorOp, kD3dTopDisable, "SetTextureStageState(stage1 COLOROP=DISABLE)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(baseline tfactor draw)")) {
    return false;
  }

  const uint32_t tf_a = 0xFF010203u;
  const uint32_t tf_b = 0xFF3366CCu;

  // Seed a known texture factor constant (A) so ApplyStateBlock must re-upload
  // the value when switching to B.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsTextureFactor, tf_a);
  if (!Check(hr == S_OK, "SetRenderState(TEXTUREFACTOR=A)")) {
    return false;
  }

  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsTextureFactor, tf_b);
  if (!Check(hr == S_OK, "SetRenderState(TEXTUREFACTOR=B) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  // Restore A before applying the state block.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsTextureFactor, tf_a);
  if (!Check(hr == S_OK, "SetRenderState(TEXTUREFACTOR=A) restore")) {
    DeleteSb();
    return false;
  }

  // Isolate ApplyStateBlock's command emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(TEXTUREFACTOR=B)")) {
    DeleteSb();
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock texturefactor)")) {
    DeleteSb();
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    DeleteSb();
    return false;
  }

  bool saw_set_render_state = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_RENDER_STATE)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_render_state)) {
      continue;
    }
    const auto* rs = reinterpret_cast<const aerogpu_cmd_set_render_state*>(hdr);
    if (rs->state == kD3dRsTextureFactor && rs->value == tf_b) {
      saw_set_render_state = true;
      break;
    }
  }
  if (!Check(saw_set_render_state, "ApplyStateBlock emits SET_RENDER_STATE(TEXTUREFACTOR=B)")) {
    DeleteSb();
    return false;
  }

  const float expected_a = static_cast<float>((tf_b >> 24) & 0xFFu) * (1.0f / 255.0f);
  const float expected_r = static_cast<float>((tf_b >> 16) & 0xFFu) * (1.0f / 255.0f);
  const float expected_g = static_cast<float>((tf_b >> 8) & 0xFFu) * (1.0f / 255.0f);
  const float expected_bf = static_cast<float>((tf_b >> 0) & 0xFFu) * (1.0f / 255.0f);
  const float expected_vec[4] = {expected_r, expected_g, expected_bf, expected_a};

  size_t uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 255 || sc->vec4_count != 1) {
      continue;
    }
    const size_t need = sizeof(*sc) + sizeof(expected_vec);
    if (!Check(hdr->size_bytes >= need, "SET_SHADER_CONSTANTS_F contains payload (tfactor ApplyStateBlock)")) {
      DeleteSb();
      return false;
    }
    const auto* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
    if (!Check(std::fabs(payload[0] - expected_vec[0]) < 1e-6f &&
                   std::fabs(payload[1] - expected_vec[1]) < 1e-6f &&
                   std::fabs(payload[2] - expected_vec[2]) < 1e-6f &&
                   std::fabs(payload[3] - expected_vec[3]) < 1e-6f,
               "TFACTOR constant payload matches expected RGBA (ApplyStateBlock)")) {
      DeleteSb();
      return false;
    }
    ++uploads;
  }
  if (!Check(uploads == 1, "ApplyStateBlock uploads TFACTOR constant exactly once")) {
    DeleteSb();
    return false;
  }

  DeleteSb();
  return true;
}

bool TestApplyStateBlockUploadsWvpConstantsForTransformChanges() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  // Apply a simple transform so WVP upload is deterministic.
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;

  constexpr float tx_a = 2.0f;
  constexpr float ty_a = 3.0f;
  constexpr float tz_a = 0.0f;
  constexpr float tx_b = 5.0f;
  constexpr float ty_b = 6.0f;
  constexpr float tz_b = 0.0f;
  const float expected_wvp_cols_b[16] = {
      1.0f, 0.0f, 0.0f, tx_b,
      0.0f, 1.0f, 0.0f, ty_b,
      0.0f, 0.0f, 1.0f, tz_b,
      0.0f, 0.0f, 0.0f, 1.0f,
  };

  D3DMATRIX world_a = identity;
  world_a.m[3][0] = tx_a;
  world_a.m[3][1] = ty_a;
  world_a.m[3][2] = tz_a;
  D3DMATRIX world_b = identity;
  world_b.m[3][0] = tx_b;
  world_b.m[3][1] = ty_b;
  world_b.m[3][2] = tz_b;

  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world_a);
  if (!Check(hr == S_OK, "SetTransform(WORLD=A)")) {
    return false;
  }

  const VertexXyzDiffuse tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(baseline draw)")) {
    return false;
  }

  D3D9DDI_HSTATEBLOCK hSb{};
  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world_b);
  if (!Check(hr == S_OK, "SetTransform(WORLD=B) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore A before applying the state block.
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world_a);
  if (!Check(hr == S_OK, "SetTransform(WORLD=A) restore")) {
    DeleteSb();
    return false;
  }

  // Isolate ApplyStateBlock command emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(WORLD=B)")) {
    DeleteSb();
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock transform)")) {
    DeleteSb();
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0,
             "ApplyStateBlock emits no CREATE_SHADER_DXBC (transform-only)")) {
    DeleteSb();
    return false;
  }

  size_t uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != kFixedfuncMatrixStartRegister || sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(*sc) + sizeof(expected_wvp_cols_b);
    if (!Check(hdr->size_bytes >= need, "SET_SHADER_CONSTANTS_F contains payload (ApplyStateBlock WVP)")) {
      DeleteSb();
      return false;
    }
    const auto* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
    if (!Check(std::memcmp(payload, expected_wvp_cols_b, sizeof(expected_wvp_cols_b)) == 0,
               "ApplyStateBlock uploads expected WVP columns")) {
      DeleteSb();
      return false;
    }
    ++uploads;
  }
  if (!Check(uploads == 1, "ApplyStateBlock uploads WVP constants exactly once")) {
    DeleteSb();
    return false;
  }

  DeleteSb();
  return true;
}

bool TestApplyStateBlockDuringStateBlockRecordingCapturesShaderBindings() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Use a supported FVF so the PS-only interop path can always synthesize a VS.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  // Create two user PS objects (distinct handles; bytecode can be identical).
  D3D9DDI_HSHADER hPsA{};
  D3D9DDI_HSHADER hPsB{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPsA);
  if (!Check(hr == S_OK, "CreateShader(PS A)")) {
    return false;
  }
  cleanup.shaders.push_back(hPsA);
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPsB);
  if (!Check(hr == S_OK, "CreateShader(PS B)")) {
    return false;
  }
  cleanup.shaders.push_back(hPsB);

  Shader* ps_a = nullptr;
  Shader* ps_b = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_a = reinterpret_cast<Shader*>(hPsA.pDrvPrivate);
    ps_b = reinterpret_cast<Shader*>(hPsB.pDrvPrivate);
  }
  if (!Check(ps_a != nullptr && ps_b != nullptr, "PS A/B driver pointers")) {
    return false;
  }

  // Bind PS A.
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPsA);
  if (!Check(hr == S_OK, "SetShader(PS=A)")) {
    return false;
  }

  // Create a state block that redundantly binds PS A (it will be a no-op when applied).
  D3D9DDI_HSTATEBLOCK hSbApply{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(sb_apply)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPsA);
  if (!Check(hr == S_OK, "SetShader(PS=A) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSbApply);
  if (!Check(hr == S_OK, "EndStateBlock(sb_apply)")) {
    return false;
  }
  if (!Check(hSbApply.pDrvPrivate != nullptr, "EndStateBlock(sb_apply) returned handle")) {
    return false;
  }

  // Record a second state block while invoking ApplyStateBlock(sb_apply).
  //
  // Critical behavior: ApplyStateBlock must record shader bindings into the active
  // recording state block even if the apply is a no-op (PS A is already bound).
  D3D9DDI_HSTATEBLOCK hSbRecorded{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(sb_recorded)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbApply);
    return false;
  }
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSbApply);
  if (!Check(hr == S_OK, "ApplyStateBlock(sb_apply) during recording")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbApply);
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSbRecorded);
  if (!Check(hr == S_OK, "EndStateBlock(sb_recorded)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbApply);
    return false;
  }
  if (!Check(hSbRecorded.pDrvPrivate != nullptr, "EndStateBlock(sb_recorded) returned handle")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbApply);
    return false;
  }
  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbApply);

  // Switch to PS B.
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPsB);
  if (!Check(hr == S_OK, "SetShader(PS=B)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbRecorded);
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->user_ps == ps_b, "PS B is the current user PS")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbRecorded);
      return false;
    }
  }

  // Applying the recorded state block should restore PS A. If ApplyStateBlock did
  // not record the shader state during sb_recorded recording, this would be a no-op
  // and PS B would remain bound.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSbRecorded);
  if (!Check(hr == S_OK, "ApplyStateBlock(sb_recorded) restores PS A")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbRecorded);
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->user_ps == ps_a, "ApplyStateBlock restores user PS A")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbRecorded);
      return false;
    }
    if (!Check(dev->ps == ps_a, "ApplyStateBlock rebinds PS A")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbRecorded);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock during recording)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbRecorded);
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0,
             "ApplyStateBlock emits no CREATE_SHADER_DXBC (shaders already created)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbRecorded);
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "ApplyStateBlock emits BIND_SHADERS")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbRecorded);
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->ps == ps_a->handle, "ApplyStateBlock binds PS A")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbRecorded);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSbRecorded);
  return true;
}

bool TestApplyStateBlockShaderConstIAndBEmitCommands() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShaderConstI != nullptr, "pfnSetShaderConstI is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShaderConstB != nullptr, "pfnSetShaderConstB is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  constexpr uint32_t kStageVs = kD3dShaderStageVs;

  constexpr uint32_t i_start = 10u;
  const int32_t i_a[4] = {9, 9, 9, 9};
  const int32_t i_b[4] = {1, 2, 3, 4};

  constexpr uint32_t b_start = 5u;
  const BOOL b_a[3] = {FALSE, FALSE, FALSE};
  const BOOL b_b[3] = {TRUE, FALSE, TRUE};

  // Seed initial A values so ApplyStateBlock changes are observable.
  HRESULT hr = cleanup.device_funcs.pfnSetShaderConstI(cleanup.hDevice, kStageVs, i_start, i_a, /*vec4_count=*/1u);
  if (!Check(hr == S_OK, "SetShaderConstI seed A")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetShaderConstB(cleanup.hDevice, kStageVs, b_start, b_a, /*bool_count=*/3u);
  if (!Check(hr == S_OK, "SetShaderConstB seed A")) {
    return false;
  }

  // Record B values into a state block.
  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(const I/B)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetShaderConstI(cleanup.hDevice, kStageVs, i_start, i_b, /*vec4_count=*/1u);
  if (!Check(hr == S_OK, "SetShaderConstI record B")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetShaderConstB(cleanup.hDevice, kStageVs, b_start, b_b, /*bool_count=*/3u);
  if (!Check(hr == S_OK, "SetShaderConstB record B")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(const I/B)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore A values before applying the state block.
  hr = cleanup.device_funcs.pfnSetShaderConstI(cleanup.hDevice, kStageVs, i_start, i_a, /*vec4_count=*/1u);
  if (!Check(hr == S_OK, "SetShaderConstI restore A")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  hr = cleanup.device_funcs.pfnSetShaderConstB(cleanup.hDevice, kStageVs, b_start, b_a, /*bool_count=*/3u);
  if (!Check(hr == S_OK, "SetShaderConstB restore A")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Isolate ApplyStateBlock's command emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(const I/B)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const uint32_t base = i_start * 4u;
    if (!Check(dev->vs_consts_i[base + 0] == i_b[0] &&
                   dev->vs_consts_i[base + 1] == i_b[1] &&
                   dev->vs_consts_i[base + 2] == i_b[2] &&
                   dev->vs_consts_i[base + 3] == i_b[3],
               "ApplyStateBlock updates vs_consts_i")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->vs_consts_b[b_start + 0] == 1u &&
                   dev->vs_consts_b[b_start + 1] == 0u &&
                   dev->vs_consts_b[b_start + 2] == 1u,
               "ApplyStateBlock updates vs_consts_b")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock const I/B)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_i = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_I)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_shader_constants_i)) {
      continue;
    }
    const auto* ci = reinterpret_cast<const aerogpu_cmd_set_shader_constants_i*>(hdr);
    if (ci->stage != AEROGPU_SHADER_STAGE_VERTEX || ci->start_register != i_start || ci->vec4_count != 1u) {
      continue;
    }
    const size_t payload_size = static_cast<size_t>(ci->vec4_count) * 4u * sizeof(int32_t);
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_i) + payload_size;
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* payload = reinterpret_cast<const int32_t*>(reinterpret_cast<const uint8_t*>(ci) +
                                                          sizeof(aerogpu_cmd_set_shader_constants_i));
    if (payload[0] == i_b[0] && payload[1] == i_b[1] && payload[2] == i_b[2] && payload[3] == i_b[3]) {
      saw_i = true;
      break;
    }
  }
  if (!Check(saw_i, "ApplyStateBlock emits SET_SHADER_CONSTANTS_I payload")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_b = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_B)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_shader_constants_b)) {
      continue;
    }
    const auto* cb = reinterpret_cast<const aerogpu_cmd_set_shader_constants_b*>(hdr);
    if (cb->stage != AEROGPU_SHADER_STAGE_VERTEX || cb->start_register != b_start || cb->bool_count != 3u) {
      continue;
    }
    const size_t payload_size = static_cast<size_t>(cb->bool_count) * sizeof(uint32_t);
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_b) + payload_size;
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* payload = reinterpret_cast<const uint32_t*>(reinterpret_cast<const uint8_t*>(cb) +
                                                            sizeof(aerogpu_cmd_set_shader_constants_b));
    if (payload[0] == 1u && payload[1] == 0u && payload[2] == 1u) {
      saw_b = true;
      break;
    }
  }
  if (!Check(saw_b, "ApplyStateBlock emits SET_SHADER_CONSTANTS_B payload")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestApplyStateBlockSamplerStateCapturesRedundantSet() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetSamplerState != nullptr, "pfnSetSamplerState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  constexpr uint32_t kStage0 = 0u;
  constexpr uint32_t kStateMagFilter = 5u; // D3DSAMP_MAGFILTER
  constexpr uint32_t kValueA = 1u;
  constexpr uint32_t kValueB = 0u;

  // Seed sampler state A so a redundant SetSamplerState inside Begin/EndStateBlock
  // still needs to be recorded into the state block (DDI semantics).
  HRESULT hr = cleanup.device_funcs.pfnSetSamplerState(cleanup.hDevice, kStage0, kStateMagFilter, kValueA);
  if (!Check(hr == S_OK, "SetSamplerState seed A")) {
    return false;
  }

  // Record a redundant SetSamplerState(A). This should be captured into the
  // state block even if the driver skips the redundant command emission.
  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(sampler state redundant)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetSamplerState(cleanup.hDevice, kStage0, kStateMagFilter, kValueA);
  if (!Check(hr == S_OK, "SetSamplerState redundant recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(sampler state redundant)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Change the sampler state to B so ApplyStateBlock must restore A.
  hr = cleanup.device_funcs.pfnSetSamplerState(cleanup.hDevice, kStage0, kStateMagFilter, kValueB);
  if (!Check(hr == S_OK, "SetSamplerState change-to-B")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Isolate ApplyStateBlock's command emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(sampler state redundant)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->sampler_states[kStage0][kStateMagFilter] == kValueA, "ApplyStateBlock restores sampler state A")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock sampler state)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_sampler = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SAMPLER_STATE)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_sampler_state)) {
      continue;
    }
    const auto* ss = reinterpret_cast<const aerogpu_cmd_set_sampler_state*>(hdr);
    if (ss->shader_stage == AEROGPU_SHADER_STAGE_PIXEL &&
        ss->slot == kStage0 &&
        ss->state == kStateMagFilter &&
        ss->value == kValueA) {
      saw_sampler = true;
      break;
    }
  }
  if (!Check(saw_sampler, "ApplyStateBlock emits SET_SAMPLER_STATE with expected values")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestApplyStateBlockVertexDeclCapturesRedundantSet() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateVertexDecl != nullptr, "pfnCreateVertexDecl is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetVertexDecl != nullptr, "pfnSetVertexDecl is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Decl A: XYZRHW|DIFFUSE (positionT + color).
  const D3DVERTEXELEMENT9_COMPAT decl_a_blob[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0, 16, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDeclA{};
  HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_a_blob, static_cast<uint32_t>(sizeof(decl_a_blob)), &hDeclA);
  if (!Check(hr == S_OK, "CreateVertexDecl(A=XYZRHW|DIFFUSE)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDeclA);

  // Decl B: XYZRHW|TEX1 (positionT + texcoord0).
  const D3DVERTEXELEMENT9_COMPAT decl_b_blob[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDeclB{};
  hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_b_blob, static_cast<uint32_t>(sizeof(decl_b_blob)), &hDeclB);
  if (!Check(hr == S_OK, "CreateVertexDecl(B=XYZRHW|TEX1)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDeclB);

  auto* decl_a = reinterpret_cast<VertexDecl*>(hDeclA.pDrvPrivate);
  auto* decl_b = reinterpret_cast<VertexDecl*>(hDeclB.pDrvPrivate);
  if (!Check(decl_a != nullptr && decl_b != nullptr, "vertex decl pointers")) {
    return false;
  }

  const aerogpu_handle_t decl_a_handle = decl_a->handle;
  if (!Check(decl_a_handle != 0, "decl A handle non-zero")) {
    return false;
  }

  // Seed decl A so a redundant SetVertexDecl(A) within Begin/EndStateBlock still
  // needs to be captured for state block semantics.
  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDeclA);
  if (!Check(hr == S_OK, "SetVertexDecl(A) seed")) {
    return false;
  }

  // Record redundant SetVertexDecl(A).
  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(vertex decl redundant)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDeclA);
  if (!Check(hr == S_OK, "SetVertexDecl(A) redundant recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(vertex decl redundant)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Switch to decl B so applying the state block must restore decl A.
  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDeclB);
  if (!Check(hr == S_OK, "SetVertexDecl(B) change-to-B")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Isolate ApplyStateBlock's command emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(vertex decl redundant)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vertex_decl == decl_a, "ApplyStateBlock restores vertex decl A")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->fvf == kFvfXyzrhwDiffuse, "ApplyStateBlock restores implied FVF for decl A")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock vertex decl)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_INPUT_LAYOUT) == 0, "ApplyStateBlock emits no CREATE_INPUT_LAYOUT")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_decl = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_input_layout)) {
      continue;
    }
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == decl_a_handle) {
      saw_decl = true;
      break;
    }
  }
  if (!Check(saw_decl, "ApplyStateBlock emits SET_INPUT_LAYOUT for decl A")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestCaptureStateBlockStreamSourceFreqAffectsApplyStateBlock() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCaptureStateBlock != nullptr, "pfnCaptureStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  constexpr uint32_t kStream0 = 0u;
  // D3DSTREAMSOURCE_INSTANCEDATA (bit 31) | 2/4. Values are stored as-is in the
  // D3D9 state cache and consumed by the UMD's instancing expansion logic.
  constexpr uint32_t kFreqRecorded = 0x80000002u;
  constexpr uint32_t kFreqCaptured = 0x80000004u;
  constexpr uint32_t kFreqOther = 1u;

  // Record a state block that touches only stream-source frequency.
  D3D9DDI_HSTATEBLOCK hSb{};
  HRESULT hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(stream source freq)")) {
    return false;
  }
  hr = aerogpu::device_set_stream_source_freq(cleanup.hDevice, kStream0, kFreqRecorded);
  if (!Check(hr == S_OK, "SetStreamSourceFreq recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(stream source freq)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Mutate stream-source frequency and then capture into the recorded state
  // block so ApplyStateBlock must restore the captured value.
  hr = aerogpu::device_set_stream_source_freq(cleanup.hDevice, kStream0, kFreqCaptured);
  if (!Check(hr == S_OK, "SetStreamSourceFreq(captured)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  hr = cleanup.device_funcs.pfnCaptureStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "CaptureStateBlock(stream source freq)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Change stream source freq again so ApplyStateBlock does something observable.
  hr = aerogpu::device_set_stream_source_freq(cleanup.hDevice, kStream0, kFreqOther);
  if (!Check(hr == S_OK, "SetStreamSourceFreq(other)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Isolate ApplyStateBlock's command emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(stream source freq)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->stream_source_freq[kStream0] == kFreqCaptured, "ApplyStateBlock restores captured stream source freq")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock stream source freq)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(len == sizeof(aerogpu_cmd_stream_header), "ApplyStateBlock emits no commands for stream source freq")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestCaptureStateBlockUsesEffectiveViewportFromRenderTarget() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateStateBlock != nullptr, "pfnCreateStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCaptureStateBlock != nullptr, "pfnCaptureStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderTarget != nullptr, "pfnSetRenderTarget is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetViewport != nullptr, "pfnSetViewport is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Create a state block that includes viewport state, then capture after binding
  // a render target while the cached viewport is still unset (Width/Height <= 0).
  // CaptureStateBlock should store the effective viewport derived from the RT.
  D3D9DDI_HSTATEBLOCK hSb{};
  // D3DSBT_PIXELSTATE = 2 (matches d3d9types.h). Pixel state blocks include
  // render targets + viewport/scissor.
  HRESULT hr = cleanup.device_funcs.pfnCreateStateBlock(cleanup.hDevice, /*type_u32=*/2u, &hSb);
  if (!Check(hr == S_OK, "CreateStateBlock(PIXELSTATE)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "CreateStateBlock returned handle")) {
    return false;
  }

  // Bind a render-target surface with a non-default size.
  constexpr uint32_t rt_w = 640u;
  constexpr uint32_t rt_h = 480u;
  D3D9DDIARG_CREATERESOURCE create_rt{};
  create_rt.type = 1u;    // D3DRTYPE_SURFACE
  create_rt.format = 22u; // D3DFMT_X8R8G8B8
  create_rt.width = rt_w;
  create_rt.height = rt_h;
  create_rt.depth = 1;
  create_rt.mip_levels = 1;
  create_rt.usage = 0x00000001u; // D3DUSAGE_RENDERTARGET
  create_rt.pool = 0;
  create_rt.size = 0;
  create_rt.hResource.pDrvPrivate = nullptr;
  create_rt.pSharedHandle = nullptr;
  create_rt.pPrivateDriverData = nullptr;
  create_rt.PrivateDriverDataSize = 0;
  create_rt.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_rt);
  if (!Check(hr == S_OK, "CreateResource(render target surface)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(create_rt.hResource.pDrvPrivate != nullptr, "CreateResource returned RT handle")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  cleanup.resources.push_back(create_rt.hResource);

  hr = cleanup.device_funcs.pfnSetRenderTarget(cleanup.hDevice, /*slot=*/0, create_rt.hResource);
  if (!Check(hr == S_OK, "SetRenderTarget(RT0)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Capture the current state into the state block. Since we never called
  // SetViewport, the effective viewport should be RT-sized.
  hr = cleanup.device_funcs.pfnCaptureStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "CaptureStateBlock")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Set a different explicit viewport so ApplyStateBlock must restore the
  // captured effective viewport.
  D3DDDIVIEWPORTINFO vp_other{};
  vp_other.X = 5.0f;
  vp_other.Y = 6.0f;
  vp_other.Width = 128.0f;
  vp_other.Height = 256.0f;
  vp_other.MinZ = 0.25f;
  vp_other.MaxZ = 0.75f;
  hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp_other);
  if (!Check(hr == S_OK, "SetViewport(other)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(CaptureStateBlock viewport)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->viewport.X == 0.0f &&
                   dev->viewport.Y == 0.0f &&
                   dev->viewport.Width == static_cast<float>(rt_w) &&
                   dev->viewport.Height == static_cast<float>(rt_h) &&
                   dev->viewport.MinZ == 0.0f &&
                   dev->viewport.MaxZ == 1.0f,
               "ApplyStateBlock restores effective viewport derived from RT")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock CaptureStateBlock viewport)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_viewport = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VIEWPORT)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_viewport)) {
      continue;
    }
    const auto* vp = reinterpret_cast<const aerogpu_cmd_set_viewport*>(hdr);
    if (vp->x_f32 == F32Bits(0.0f) &&
        vp->y_f32 == F32Bits(0.0f) &&
        vp->width_f32 == F32Bits(static_cast<float>(rt_w)) &&
        vp->height_f32 == F32Bits(static_cast<float>(rt_h)) &&
        vp->min_depth_f32 == F32Bits(0.0f) &&
        vp->max_depth_f32 == F32Bits(1.0f)) {
      saw_viewport = true;
      break;
    }
  }
  if (!Check(saw_viewport, "ApplyStateBlock emits SET_VIEWPORT for effective RT-sized viewport")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestCaptureStateBlockUsesEffectiveScissorRectFromRenderTarget() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateStateBlock != nullptr, "pfnCreateStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCaptureStateBlock != nullptr, "pfnCaptureStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderTarget != nullptr, "pfnSetRenderTarget is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetScissorRect != nullptr, "pfnSetScissorRect is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Create a pixel state block so it includes scissor state. Capture after binding
  // an RT and enabling scissor without ever calling SetScissorRect: the driver
  // should fall back to a viewport/RT-sized scissor rect rather than leaving it
  // unset.
  D3D9DDI_HSTATEBLOCK hSb{};
  // D3DSBT_PIXELSTATE = 2 (matches d3d9types.h).
  HRESULT hr = cleanup.device_funcs.pfnCreateStateBlock(cleanup.hDevice, /*type_u32=*/2u, &hSb);
  if (!Check(hr == S_OK, "CreateStateBlock(PIXELSTATE)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "CreateStateBlock returned handle")) {
    return false;
  }

  // Bind a render-target surface with a known size.
  constexpr uint32_t rt_w = 640u;
  constexpr uint32_t rt_h = 480u;
  D3D9DDIARG_CREATERESOURCE create_rt{};
  create_rt.type = 1u;    // D3DRTYPE_SURFACE
  create_rt.format = 22u; // D3DFMT_X8R8G8B8
  create_rt.width = rt_w;
  create_rt.height = rt_h;
  create_rt.depth = 1;
  create_rt.mip_levels = 1;
  create_rt.usage = 0x00000001u; // D3DUSAGE_RENDERTARGET
  create_rt.pool = 0;
  create_rt.size = 0;
  create_rt.hResource.pDrvPrivate = nullptr;
  create_rt.pSharedHandle = nullptr;
  create_rt.pPrivateDriverData = nullptr;
  create_rt.PrivateDriverDataSize = 0;
  create_rt.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_rt);
  if (!Check(hr == S_OK, "CreateResource(render target surface)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(create_rt.hResource.pDrvPrivate != nullptr, "CreateResource returned RT handle")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  cleanup.resources.push_back(create_rt.hResource);

  hr = cleanup.device_funcs.pfnSetRenderTarget(cleanup.hDevice, /*slot=*/0, create_rt.hResource);
  if (!Check(hr == S_OK, "SetRenderTarget(RT0)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Enable scissor via render state without calling SetScissorRect. The UMD should
  // fix up the unset rect to a viewport/RT-sized rect.
  constexpr uint32_t kD3dRsScissorTestEnable = 174u; // D3DRS_SCISSORTESTENABLE
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsScissorTestEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(SCISSORTESTENABLE=1)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Capture scissor state into the block.
  hr = cleanup.device_funcs.pfnCaptureStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "CaptureStateBlock(scissor)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Change scissor state to a different user-set rect so ApplyStateBlock must
  // restore the captured RT-sized (non-user-set) rect.
  RECT other{};
  other.left = 10;
  other.top = 20;
  other.right = 110;
  other.bottom = 220;
  hr = cleanup.device_funcs.pfnSetScissorRect(cleanup.hDevice, &other, TRUE);
  if (!Check(hr == S_OK, "SetScissorRect(other)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(CaptureStateBlock scissor)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->scissor_enabled == TRUE, "ApplyStateBlock restores scissor enabled")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->render_states[kD3dRsScissorTestEnable] == 1u, "ApplyStateBlock restores render state 174")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(!dev->scissor_rect_user_set, "ApplyStateBlock restores scissor_rect_user_set=false")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->scissor_rect.left == 0 &&
                   dev->scissor_rect.top == 0 &&
                   dev->scissor_rect.right == static_cast<LONG>(rt_w) &&
                   dev->scissor_rect.bottom == static_cast<LONG>(rt_h),
               "ApplyStateBlock restores effective RT-sized scissor rect")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock CaptureStateBlock scissor)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_scissor = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SCISSOR)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_scissor)) {
      continue;
    }
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_scissor*>(hdr);
    if (sc->x == 0 && sc->y == 0 && sc->width == static_cast<int32_t>(rt_w) && sc->height == static_cast<int32_t>(rt_h)) {
      saw_scissor = true;
      break;
    }
  }
  if (!Check(saw_scissor, "ApplyStateBlock emits SET_SCISSOR for effective RT-sized rect")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestApplyStateBlockRenderStateCapturesRedundantSet() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // D3DRS_CULLMODE (numeric value from d3d9types.h).
  constexpr uint32_t kD3dRsCullMode = 22u;
  constexpr uint32_t kCullNone = 1u;
  constexpr uint32_t kCullCw = 2u;

  // Seed the render state to a known value.
  HRESULT hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsCullMode, kCullNone);
  if (!Check(hr == S_OK, "SetRenderState(CULLMODE=NONE) seed")) {
    return false;
  }

  // Record a redundant SetRenderState(CULLMODE=NONE). D3D9 state blocks must
  // still capture it even if the UMD skips redundant command emission.
  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(redundant render state)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsCullMode, kCullNone);
  if (!Check(hr == S_OK, "SetRenderState(CULLMODE=NONE) redundant recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(redundant render state)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Change the render state so applying the state block must restore it.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsCullMode, kCullCw);
  if (!Check(hr == S_OK, "SetRenderState(CULLMODE=CW) change-to-B")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(redundant render state)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->render_states[kD3dRsCullMode] == kCullNone, "ApplyStateBlock restores CULLMODE=NONE")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock redundant render state)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_rs = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_RENDER_STATE)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_render_state)) {
      continue;
    }
    const auto* rs = reinterpret_cast<const aerogpu_cmd_set_render_state*>(hdr);
    if (rs->state == kD3dRsCullMode && rs->value == kCullNone) {
      saw_rs = true;
      break;
    }
  }
  if (!Check(saw_rs, "ApplyStateBlock emits SET_RENDER_STATE(CULLMODE=NONE)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestApplyStateBlockScissorRenderStateEmitsSetScissor() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetScissorRect != nullptr, "pfnSetScissorRect is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  constexpr uint32_t kD3dRsScissorTestEnable = 174u; // D3DRS_SCISSORTESTENABLE
  RECT rect{};
  rect.left = 10;
  rect.top = 20;
  rect.right = 110;
  rect.bottom = 220;

  // Enable scissor with a known rect so a subsequent redundant SetRenderState can
  // be recorded without causing the state block itself to capture the dedicated
  // scissor state (only the render state).
  HRESULT hr = cleanup.device_funcs.pfnSetScissorRect(cleanup.hDevice, &rect, TRUE);
  if (!Check(hr == S_OK, "SetScissorRect(enable; set rect)")) {
    return false;
  }

  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(scissor enable via render state)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsScissorTestEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(SCISSORTESTENABLE=TRUE) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(scissor enable via render state)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Disable scissor again before applying the state block.
  hr = cleanup.device_funcs.pfnSetScissorRect(cleanup.hDevice, &rect, FALSE);
  if (!Check(hr == S_OK, "SetScissorRect(disable; keep rect)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Isolate ApplyStateBlock's command emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(scissor enable render state)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->scissor_enabled == TRUE, "ApplyStateBlock enables scissor")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->render_states[kD3dRsScissorTestEnable] == 1u, "render state 174 updated")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->scissor_rect.left == rect.left &&
                   dev->scissor_rect.top == rect.top &&
                   dev->scissor_rect.right == rect.right &&
                   dev->scissor_rect.bottom == rect.bottom,
               "scissor rect preserved")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock scissor enable)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_scissor = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SCISSOR)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_scissor)) {
      continue;
    }
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_scissor*>(hdr);
    if (sc->x == rect.left && sc->y == rect.top &&
        sc->width == (rect.right - rect.left) &&
        sc->height == (rect.bottom - rect.top)) {
      saw_scissor = true;
      break;
    }
  }
  if (!Check(saw_scissor, "ApplyStateBlock emits SET_SCISSOR with expected rect")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_rs = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_RENDER_STATE)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_render_state)) {
      continue;
    }
    const auto* rs = reinterpret_cast<const aerogpu_cmd_set_render_state*>(hdr);
    if (rs->state == kD3dRsScissorTestEnable && rs->value == 1u) {
      saw_rs = true;
      break;
    }
  }
  if (!Check(saw_rs, "ApplyStateBlock emits SET_RENDER_STATE(SCISSORTESTENABLE=TRUE)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestApplyStateBlockScissorRectEmitsSetScissor() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetScissorRect != nullptr, "pfnSetScissorRect is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  constexpr uint32_t kD3dRsScissorTestEnable = 174u; // D3DRS_SCISSORTESTENABLE

  RECT rect_a{};
  rect_a.left = 0;
  rect_a.top = 0;
  rect_a.right = 50;
  rect_a.bottom = 60;

  RECT rect_b{};
  rect_b.left = 10;
  rect_b.top = 20;
  rect_b.right = 110;
  rect_b.bottom = 220;

  // Start from scissor disabled at A so applying the state block is observable.
  HRESULT hr = cleanup.device_funcs.pfnSetScissorRect(cleanup.hDevice, &rect_a, FALSE);
  if (!Check(hr == S_OK, "SetScissorRect(A, disabled)")) {
    return false;
  }

  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(scissor rect)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetScissorRect(cleanup.hDevice, &rect_b, TRUE);
  if (!Check(hr == S_OK, "SetScissorRect(B, enabled) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(scissor rect)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore scissor A/disabled before applying the state block.
  hr = cleanup.device_funcs.pfnSetScissorRect(cleanup.hDevice, &rect_a, FALSE);
  if (!Check(hr == S_OK, "SetScissorRect(A, disabled) restore before apply")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Isolate ApplyStateBlock's command emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(scissor rect)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->scissor_enabled == TRUE, "ApplyStateBlock enables scissor")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->render_states[kD3dRsScissorTestEnable] == 1u, "render state 174 updated")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->scissor_rect_user_set, "scissor_rect_user_set is preserved")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->scissor_rect.left == rect_b.left &&
                   dev->scissor_rect.top == rect_b.top &&
                   dev->scissor_rect.right == rect_b.right &&
                   dev->scissor_rect.bottom == rect_b.bottom,
               "ApplyStateBlock restores scissor rect")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock scissor rect)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_scissor = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SCISSOR)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_scissor)) {
      continue;
    }
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_scissor*>(hdr);
    if (sc->x == rect_b.left && sc->y == rect_b.top &&
        sc->width == (rect_b.right - rect_b.left) &&
        sc->height == (rect_b.bottom - rect_b.top)) {
      saw_scissor = true;
      break;
    }
  }
  if (!Check(saw_scissor, "ApplyStateBlock emits SET_SCISSOR with expected rect")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_rs = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_RENDER_STATE)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_render_state)) {
      continue;
    }
    const auto* rs = reinterpret_cast<const aerogpu_cmd_set_render_state*>(hdr);
    if (rs->state == kD3dRsScissorTestEnable && rs->value == 1u) {
      saw_rs = true;
      break;
    }
  }
  if (!Check(saw_rs, "ApplyStateBlock emits SET_RENDER_STATE(SCISSORTESTENABLE=TRUE)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestApplyStateBlockViewportEmitsSetViewport() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetViewport != nullptr, "pfnSetViewport is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  D3DDDIVIEWPORTINFO vp_a{};
  vp_a.X = 0.0f;
  vp_a.Y = 0.0f;
  vp_a.Width = 256.0f;
  vp_a.Height = 512.0f;
  vp_a.MinZ = 0.0f;
  vp_a.MaxZ = 1.0f;

  HRESULT hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp_a);
  if (!Check(hr == S_OK, "SetViewport(A)")) {
    return false;
  }

  D3DDDIVIEWPORTINFO vp_b{};
  vp_b.X = 10.0f;
  vp_b.Y = 20.0f;
  vp_b.Width = 300.0f;
  vp_b.Height = 400.0f;
  vp_b.MinZ = 0.25f;
  vp_b.MaxZ = 0.75f;

  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(SetViewport(B))")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp_b);
  if (!Check(hr == S_OK, "SetViewport(B) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(SetViewport(B))")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  // Restore viewport A before applying the state block.
  hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp_a);
  if (!Check(hr == S_OK, "SetViewport(A) restore before apply")) {
    DeleteSb();
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(SetViewport(B))")) {
    DeleteSb();
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock viewport)")) {
    DeleteSb();
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    DeleteSb();
    return false;
  }

  bool saw_viewport = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VIEWPORT)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_viewport)) {
      continue;
    }
    const auto* vp = reinterpret_cast<const aerogpu_cmd_set_viewport*>(hdr);
    if (vp->x_f32 == F32Bits(vp_b.X) &&
        vp->y_f32 == F32Bits(vp_b.Y) &&
        vp->width_f32 == F32Bits(vp_b.Width) &&
        vp->height_f32 == F32Bits(vp_b.Height) &&
        vp->min_depth_f32 == F32Bits(vp_b.MinZ) &&
        vp->max_depth_f32 == F32Bits(vp_b.MaxZ)) {
      saw_viewport = true;
      break;
    }
  }
  if (!Check(saw_viewport, "ApplyStateBlock emits SET_VIEWPORT with expected values")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->viewport.X == vp_b.X &&
                   dev->viewport.Y == vp_b.Y &&
                   dev->viewport.Width == vp_b.Width &&
                   dev->viewport.Height == vp_b.Height &&
                   dev->viewport.MinZ == vp_b.MinZ &&
                   dev->viewport.MaxZ == vp_b.MaxZ,
               "ApplyStateBlock updates cached viewport state")) {
      DeleteSb();
      return false;
    }
  }

  DeleteSb();
  return true;
}

bool TestSetViewportSanitizesNonFiniteValues() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetViewport != nullptr, "pfnSetViewport is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  const float nan = std::numeric_limits<float>::quiet_NaN();
  const float inf = std::numeric_limits<float>::infinity();
  D3DDDIVIEWPORTINFO vp{};
  vp.X = nan;
  vp.Y = inf;
  vp.Width = nan;
  vp.Height = inf;
  vp.MinZ = nan;
  vp.MaxZ = inf;

  HRESULT hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport(non-finite values)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(std::isfinite(dev->viewport.X) && dev->viewport.X == 0.0f, "viewport X sanitized to 0")) {
      return false;
    }
    if (!Check(std::isfinite(dev->viewport.Y) && dev->viewport.Y == 0.0f, "viewport Y sanitized to 0")) {
      return false;
    }
    if (!Check(std::isfinite(dev->viewport.Width) && dev->viewport.Width == 1.0f, "viewport Width sanitized to 1")) {
      return false;
    }
    if (!Check(std::isfinite(dev->viewport.Height) && dev->viewport.Height == 1.0f, "viewport Height sanitized to 1")) {
      return false;
    }
    if (!Check(std::isfinite(dev->viewport.MinZ) && dev->viewport.MinZ == 0.0f, "viewport MinZ sanitized to 0")) {
      return false;
    }
    if (!Check(std::isfinite(dev->viewport.MaxZ) && dev->viewport.MaxZ == 1.0f, "viewport MaxZ sanitized to 1")) {
      return false;
    }
  }

  return true;
}

bool TestApplyStateBlockStreamSourceAndIndexBufferEmitCommands() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateResource != nullptr, "pfnCreateResource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetIndices != nullptr, "pfnSetIndices is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  auto CreateBuffer = [&](uint32_t size_bytes, const char* what, D3DDDI_HRESOURCE* out_res) -> bool {
    if (!out_res) {
      return false;
    }
    D3D9DDIARG_CREATERESOURCE create{};
    create.type = 0u;
    create.format = 0u;
    create.width = 0;
    create.height = 0;
    create.depth = 0;
    create.mip_levels = 1;
    create.usage = 0;
    create.pool = 0;
    create.size = size_bytes;
    create.hResource.pDrvPrivate = nullptr;
    create.pSharedHandle = nullptr;
    create.pPrivateDriverData = nullptr;
    create.PrivateDriverDataSize = 0;
    create.wddm_hAllocation = 0;
    const HRESULT hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create);
    if (!Check(hr == S_OK, what)) {
      return false;
    }
    if (!Check(create.hResource.pDrvPrivate != nullptr, "CreateResource returned handle")) {
      return false;
    }
    cleanup.resources.push_back(create.hResource);
    *out_res = create.hResource;
    return true;
  };

  D3DDDI_HRESOURCE vb_a{};
  D3DDDI_HRESOURCE vb_b{};
  D3DDDI_HRESOURCE ib_a{};
  D3DDDI_HRESOURCE ib_b{};
  if (!CreateBuffer(/*size_bytes=*/256u, "CreateResource(vb A)", &vb_a)) {
    return false;
  }
  if (!CreateBuffer(/*size_bytes=*/256u, "CreateResource(vb B)", &vb_b)) {
    return false;
  }
  if (!CreateBuffer(/*size_bytes=*/64u, "CreateResource(ib A)", &ib_a)) {
    return false;
  }
  if (!CreateBuffer(/*size_bytes=*/64u, "CreateResource(ib B)", &ib_b)) {
    return false;
  }

  auto* vb_a_res = reinterpret_cast<Resource*>(vb_a.pDrvPrivate);
  auto* vb_b_res = reinterpret_cast<Resource*>(vb_b.pDrvPrivate);
  auto* ib_a_res = reinterpret_cast<Resource*>(ib_a.pDrvPrivate);
  auto* ib_b_res = reinterpret_cast<Resource*>(ib_b.pDrvPrivate);
  if (!Check(vb_a_res && vb_b_res && ib_a_res && ib_b_res, "resource pointers")) {
    return false;
  }

  constexpr uint32_t stride = 16u;
  constexpr uint32_t offset = 0u;
  constexpr D3DDDIFORMAT kIndex16 = static_cast<D3DDDIFORMAT>(101);

  // Start from stream/indices A.
  HRESULT hr = cleanup.device_funcs.pfnSetStreamSource(cleanup.hDevice, /*stream=*/0, vb_a, offset, stride);
  if (!Check(hr == S_OK, "SetStreamSource(A)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetIndices(cleanup.hDevice, ib_a, kIndex16, /*offset_bytes=*/0);
  if (!Check(hr == S_OK, "SetIndices(A)")) {
    return false;
  }

  // Record stream/indices B in a state block.
  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(stream+indices)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetStreamSource(cleanup.hDevice, /*stream=*/0, vb_b, offset, stride);
  if (!Check(hr == S_OK, "SetStreamSource(B) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetIndices(cleanup.hDevice, ib_b, kIndex16, /*offset_bytes=*/0);
  if (!Check(hr == S_OK, "SetIndices(B) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(stream+indices)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore A before applying the state block.
  hr = cleanup.device_funcs.pfnSetStreamSource(cleanup.hDevice, /*stream=*/0, vb_a, offset, stride);
  if (!Check(hr == S_OK, "SetStreamSource(A) restore before apply")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  hr = cleanup.device_funcs.pfnSetIndices(cleanup.hDevice, ib_a, kIndex16, /*offset_bytes=*/0);
  if (!Check(hr == S_OK, "SetIndices(A) restore before apply")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Isolate ApplyStateBlock's command emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(stream+indices)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->streams[0].vb == vb_b_res, "ApplyStateBlock updates stream0 VB")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->streams[0].stride_bytes == stride, "ApplyStateBlock updates stream0 stride")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->streams[0].offset_bytes == offset, "ApplyStateBlock updates stream0 offset")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->index_buffer == ib_b_res, "ApplyStateBlock updates index buffer")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(dev->index_offset_bytes == 0u, "ApplyStateBlock updates index offset")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock stream+indices)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Validate stream source command.
  bool saw_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->start_slot != 0 || svb->buffer_count != 1) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) + sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* binding = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(
        reinterpret_cast<const uint8_t*>(svb) + sizeof(aerogpu_cmd_set_vertex_buffers));
    if (binding->buffer == vb_b_res->handle && binding->stride_bytes == stride && binding->offset_bytes == offset) {
      saw_vb = true;
      break;
    }
  }
  if (!Check(saw_vb, "ApplyStateBlock emits SET_VERTEX_BUFFERS for stream0")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Validate index buffer command.
  bool saw_ib = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INDEX_BUFFER)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_index_buffer)) {
      continue;
    }
    const auto* sib = reinterpret_cast<const aerogpu_cmd_set_index_buffer*>(hdr);
    if (sib->buffer == ib_b_res->handle && sib->format == AEROGPU_INDEX_FORMAT_UINT16 && sib->offset_bytes == 0u) {
      saw_ib = true;
      break;
    }
  }
  if (!Check(saw_ib, "ApplyStateBlock emits SET_INDEX_BUFFER for expected IB")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestApplyStateBlockRenderTargetEmitsSetRenderTargets() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateResource != nullptr, "pfnCreateResource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderTarget != nullptr, "pfnSetRenderTarget is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  auto CreateRenderTarget = [&](uint32_t width, uint32_t height, const char* what, D3DDDI_HRESOURCE* out_res) -> bool {
    if (!out_res) {
      return false;
    }
    D3D9DDIARG_CREATERESOURCE create_rt{};
    create_rt.type = 1u;    // D3DRTYPE_SURFACE
    create_rt.format = 22u; // D3DFMT_X8R8G8B8
    create_rt.width = width;
    create_rt.height = height;
    create_rt.depth = 1;
    create_rt.mip_levels = 1;
    create_rt.usage = 0x00000001u; // D3DUSAGE_RENDERTARGET
    create_rt.pool = 0;
    create_rt.size = 0;
    create_rt.hResource.pDrvPrivate = nullptr;
    create_rt.pSharedHandle = nullptr;
    create_rt.pPrivateDriverData = nullptr;
    create_rt.PrivateDriverDataSize = 0;
    create_rt.wddm_hAllocation = 0;

    const HRESULT hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_rt);
    if (!Check(hr == S_OK, what)) {
      return false;
    }
    if (!Check(create_rt.hResource.pDrvPrivate != nullptr, "CreateResource returned RT handle")) {
      return false;
    }
    cleanup.resources.push_back(create_rt.hResource);
    *out_res = create_rt.hResource;
    return true;
  };

  D3DDDI_HRESOURCE rt_a{};
  D3DDDI_HRESOURCE rt_b{};
  if (!CreateRenderTarget(/*width=*/64u, /*height=*/64u, "CreateResource(RT A)", &rt_a)) {
    return false;
  }
  if (!CreateRenderTarget(/*width=*/64u, /*height=*/64u, "CreateResource(RT B)", &rt_b)) {
    return false;
  }

  auto* rt_a_res = reinterpret_cast<Resource*>(rt_a.pDrvPrivate);
  auto* rt_b_res = reinterpret_cast<Resource*>(rt_b.pDrvPrivate);
  if (!Check(rt_a_res && rt_b_res, "RT resource pointers")) {
    return false;
  }

  // Start from RT A.
  HRESULT hr = cleanup.device_funcs.pfnSetRenderTarget(cleanup.hDevice, /*slot=*/0, rt_a);
  if (!Check(hr == S_OK, "SetRenderTarget(RT A)")) {
    return false;
  }

  // Record RT B in a state block.
  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(SetRenderTarget B)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderTarget(cleanup.hDevice, /*slot=*/0, rt_b);
  if (!Check(hr == S_OK, "SetRenderTarget(RT B) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(SetRenderTarget B)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore RT A before applying the state block.
  hr = cleanup.device_funcs.pfnSetRenderTarget(cleanup.hDevice, /*slot=*/0, rt_a);
  if (!Check(hr == S_OK, "SetRenderTarget(RT A) restore before apply")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Isolate ApplyStateBlock's command emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(SetRenderTarget B)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->render_targets[0] == rt_b_res, "ApplyStateBlock updates RT0")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock render target)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_rt = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_RENDER_TARGETS)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_render_targets)) {
      continue;
    }
    const auto* rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(hdr);
    if (rt->color_count >= 1 && rt->colors[0] == rt_b_res->handle) {
      saw_rt = true;
      break;
    }
  }
  if (!Check(saw_rt, "ApplyStateBlock emits SET_RENDER_TARGETS for expected RT0")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestApplyStateBlockDepthStencilEmitsSetRenderTargets() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateResource != nullptr, "pfnCreateResource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderTarget != nullptr, "pfnSetRenderTarget is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetDepthStencil != nullptr, "pfnSetDepthStencil is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  auto CreateSurface = [&](uint32_t format, uint32_t usage, const char* what, D3DDDI_HRESOURCE* out_res) -> bool {
    if (!out_res) {
      return false;
    }
    D3D9DDIARG_CREATERESOURCE create{};
    create.type = 1u; // D3DRTYPE_SURFACE
    create.format = format;
    create.width = 64;
    create.height = 64;
    create.depth = 1;
    create.mip_levels = 1;
    create.usage = usage;
    create.pool = 0;
    create.size = 0;
    create.hResource.pDrvPrivate = nullptr;
    create.pSharedHandle = nullptr;
    create.pPrivateDriverData = nullptr;
    create.PrivateDriverDataSize = 0;
    create.wddm_hAllocation = 0;
    const HRESULT hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create);
    if (!Check(hr == S_OK, what)) {
      return false;
    }
    if (!Check(create.hResource.pDrvPrivate != nullptr, "CreateResource returned handle")) {
      return false;
    }
    cleanup.resources.push_back(create.hResource);
    *out_res = create.hResource;
    return true;
  };

  // Create RT0 (color) and two depth-stencil surfaces.
  D3DDDI_HRESOURCE rt{};
  D3DDDI_HRESOURCE ds_a{};
  D3DDDI_HRESOURCE ds_b{};
  // RT: X8R8G8B8 (22), usage=RENDERTARGET (1).
  if (!CreateSurface(/*format=*/22u, /*usage=*/0x00000001u, "CreateResource(RT)", &rt)) {
    return false;
  }
  // DS: D24S8 (75), usage=DEPTHSTENCIL (2).
  if (!CreateSurface(/*format=*/75u, /*usage=*/0x00000002u, "CreateResource(DS A)", &ds_a)) {
    return false;
  }
  if (!CreateSurface(/*format=*/75u, /*usage=*/0x00000002u, "CreateResource(DS B)", &ds_b)) {
    return false;
  }

  auto* rt_res = reinterpret_cast<Resource*>(rt.pDrvPrivate);
  auto* ds_a_res = reinterpret_cast<Resource*>(ds_a.pDrvPrivate);
  auto* ds_b_res = reinterpret_cast<Resource*>(ds_b.pDrvPrivate);
  if (!Check(rt_res && ds_a_res && ds_b_res, "surface resource pointers")) {
    return false;
  }

  // Start from RT0 + DS A.
  HRESULT hr = cleanup.device_funcs.pfnSetRenderTarget(cleanup.hDevice, /*slot=*/0, rt);
  if (!Check(hr == S_OK, "SetRenderTarget(RT0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetDepthStencil(cleanup.hDevice, ds_a);
  if (!Check(hr == S_OK, "SetDepthStencil(DS A)")) {
    return false;
  }

  // Record DS B in a state block.
  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(SetDepthStencil B)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetDepthStencil(cleanup.hDevice, ds_b);
  if (!Check(hr == S_OK, "SetDepthStencil(DS B) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(SetDepthStencil B)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore DS A before applying the state block.
  hr = cleanup.device_funcs.pfnSetDepthStencil(cleanup.hDevice, ds_a);
  if (!Check(hr == S_OK, "SetDepthStencil(DS A) restore before apply")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Isolate ApplyStateBlock's command emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(SetDepthStencil B)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->depth_stencil == ds_b_res, "ApplyStateBlock updates depth-stencil")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock depth-stencil)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_ds = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_RENDER_TARGETS)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_render_targets)) {
      continue;
    }
    const auto* pkt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(hdr);
    if (pkt->depth_stencil == ds_b_res->handle && pkt->color_count >= 1 && pkt->colors[0] == rt_res->handle) {
      saw_ds = true;
      break;
    }
  }
  if (!Check(saw_ds, "ApplyStateBlock emits SET_RENDER_TARGETS with expected depth-stencil")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestApplyStateBlockToleratesUnsupportedTextureStageState() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Start from a supported texture stage configuration.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "stage0 COLORARG2=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(baseline supported stage0)")) {
    return false;
  }

  // Record a state block that applies an unsupported stage0 op.
  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(unsupported stage0 op)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopAddSmooth, "stage0 COLOROP=ADDSMOOTH (recorded)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(unsupported stage0 op)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore a supported op so ApplyStateBlock transitions into the unsupported
  // state (rather than being a no-op).
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE (restore)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // ApplyStateBlock must tolerate unsupported stage state; it should not fail and
  // should not try to (re)bind a fixed-function PS for it.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(unsupported stage0 op) succeeds")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock unsupported stage0 op)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) == 0, "ApplyStateBlock emits no BIND_SHADERS")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // With the unsupported state applied, draws should fail cleanly with INVALIDCALL
  // and must not emit commands.
  const size_t before_draw = dev->cmd.bytes_used();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(unsupported stage0 op) => INVALIDCALL")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(dev->cmd.bytes_used() == before_draw, "invalid draw emits no new commands")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestApplyStateBlockFvfChangeReuploadsWvpConstantsAfterConstClobber() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShaderConstF != nullptr, "pfnSetShaderConstF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Seed stable transforms and upload fixed-function WVP constants in a matrix-using
  // fixed-function path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;

  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };

  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;

  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_matrix_dirty, "initial WVP upload cleared fixedfunc_matrix_dirty")) {
      return false;
    }
  }

  // Switch to a non-matrix fixed-function path so subsequent VS constant writes can
  // clobber the reserved range without automatically triggering a WVP reupload.
  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  // Record a state block that switches back to the matrix path (SetFVF records both
  // FVF and the internal vertex decl binding).
  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(FVF=XYZ|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(FVF=XYZ|DIFFUSE)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Clear the dirty bit in the recorded (matrix) path so the subsequent ApplyStateBlock
  // must set it due to the FVF transition, not due to leftover state.
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD) clears dirty in matrix path")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_matrix_dirty, "matrix path cleared fixedfunc_matrix_dirty before ApplyStateBlock")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  // Return to the non-matrix FVF and ensure fixedfunc_matrix_dirty is false.
  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE) before ApplyStateBlock")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_matrix_dirty, "non-matrix path has fixedfunc_matrix_dirty=false")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  // Clobber the reserved matrix constant range (c240..c243) while in a non-matrix path.
  // This should *not* mark fixedfunc_matrix_dirty because the current FVF doesn't use it.
  float clobber[16] = {
      9.0f, 8.0f, 7.0f, 6.0f,
      5.0f, 4.0f, 3.0f, 2.0f,
      1.0f, 9.0f, 8.0f, 7.0f,
      6.0f, 5.0f, 4.0f, 3.0f,
  };
  hr = cleanup.device_funcs.pfnSetShaderConstF(cleanup.hDevice,
                                               kD3dShaderStageVs,
                                               /*start_reg=*/kFixedfuncMatrixStartRegister,
                                               clobber,
                                               /*vec4_count=*/kFixedfuncMatrixVec4Count);
  if (!Check(hr == S_OK, "SetShaderConstF clobber matrix range")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_matrix_dirty, "clobber under XYZRHW does not set fixedfunc_matrix_dirty")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  // Applying the state block should switch to XYZ|DIFFUSE and re-upload WVP constants
  // (fixing the clobbered reserved range).
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(FVF=XYZ|DIFFUSE)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fvf == kFvfXyzDiffuse, "ApplyStateBlock updated FVF to XYZ|DIFFUSE")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(!dev->fixedfunc_matrix_dirty, "ApplyStateBlock cleared fixedfunc_matrix_dirty")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock FVF switch WVP)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0,
             "ApplyStateBlock emits no CREATE_SHADER_DXBC (FVF switch only)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  bool saw_upload = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != kFixedfuncMatrixStartRegister || sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(*sc) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_upload = true;
      break;
    }
  }
  if (!Check(saw_upload, "ApplyStateBlock uploads expected WVP columns after clobber")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return true;
}

bool TestApplyStateBlockUpdatesFixedfuncPsWhenTextureBindingChanges() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Bind textures for stage0 and stage1.
  D3DDDI_HRESOURCE hTex0{};
  D3DDDI_HRESOURCE hTex1{};
  if (!CreateDummyTexture(&cleanup, &hTex0) || !CreateDummyTexture(&cleanup, &hTex1)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: modulate tex0 * diffuse.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "stage0 COLORARG2=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage1: add current + tex1 (uses texture1).
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopAdd, "stage1 COLOROP=ADD")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg1, kD3dTaCurrent, "stage1 COLORARG1=CURRENT")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg2, kD3dTaTexture, "stage1 COLORARG2=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssAlphaOp, kD3dTopDisable, "stage1 ALPHAOP=DISABLE")) {
    return false;
  }

  // Terminate at stage2.
  if (!SetTextureStageState(2, kD3dTssColorOp, kD3dTopDisable, "stage2 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  // Baseline: stage1 texture bound => 2 texld (s0+s1).
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 texture bound baseline)")) {
    return false;
  }

  Shader* ps_bound = nullptr;
  Shader* ps_unbound = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_bound = dev->ps;
  }
  if (!Check(ps_bound != nullptr, "PS bound (stage1 texture bound)")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps_bound, kPsOpTexld) == 2, "stage1 texture bound => exactly 2 texld")) {
    return false;
  }
  if (!Check(ShaderTexldSamplerMask(ps_bound) == 0x3u, "stage1 texture bound => texld uses samplers s0 and s1")) {
    return false;
  }

  // Baseline: unbind texture1 while stage1 still references TEXTURE => stage chain
  // terminates at stage1 and the PS must not sample stage1.
  {
    D3DDDI_HRESOURCE null_tex{};
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, null_tex);
    if (!Check(hr == S_OK, "SetTexture(stage1=null)")) {
      return false;
    }
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 texture unbound baseline)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_unbound = dev->ps;
  }
  if (!Check(ps_unbound != nullptr, "PS bound (stage1 texture unbound)")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps_unbound, kPsOpTexld) == 1, "stage1 texture unbound => exactly 1 texld")) {
    return false;
  }
  if (!Check(ShaderTexldSamplerMask(ps_unbound) == 0x1u, "stage1 texture unbound => texld uses only sampler s0")) {
    return false;
  }

  // Restore texture1 so ApplyStateBlock can unbind it.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1=rebind)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 texture rebound baseline)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == ps_bound, "stage1 texture rebound reuses cached PS variant")) {
      return false;
    }
  }

  // Record a state block that unbinds texture1.
  D3D9DDI_HSTATEBLOCK hSb{};
  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock")) {
    return false;
  }
  {
    D3DDDI_HRESOURCE null_tex{};
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, null_tex);
    if (!Check(hr == S_OK, "SetTexture(stage1=null) recorded")) {
      return false;
    }
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore texture1 again before applying the state block.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1=rebind after record)")) {
    DeleteSb();
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 texture bound before ApplyStateBlock)")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == ps_bound, "PS bound (stage1 texture bound) before ApplyStateBlock")) {
      DeleteSb();
      return false;
    }
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(texture1 unbind)")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->textures[1] == nullptr, "ApplyStateBlock unbinds stage1 texture")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->ps == ps_unbound, "ApplyStateBlock updates fixed-function PS for missing stage1 texture")) {
      DeleteSb();
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock texture1 unbind)")) {
    DeleteSb();
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    DeleteSb();
    return false;
  }

  bool saw_set_tex1_null = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_texture)) {
      continue;
    }
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 1 && st->texture == 0) {
      saw_set_tex1_null = true;
      break;
    }
  }
  if (!Check(saw_set_tex1_null, "ApplyStateBlock emits SET_TEXTURE(stage1=null)")) {
    DeleteSb();
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "ApplyStateBlock emits BIND_SHADERS")) {
    DeleteSb();
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->ps == ps_unbound->handle, "ApplyStateBlock binds stage1-missing PS")) {
    DeleteSb();
    return false;
  }

  DeleteSb();
  return true;
}

bool TestApplyStateBlockUpdatesFixedfuncPsForStageStateInVsOnlyInterop() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  D3DDDI_HRESOURCE hTex1{};
  if (!CreateDummyTexture(&cleanup, &hTex0) || !CreateDummyTexture(&cleanup, &hTex1)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: modulate tex0 * diffuse.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "stage0 COLORARG2=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage1: add current + tex1 (uses texture1). We'll toggle only COLOROP between
  // ADD and DISABLE so the signature is stable and we avoid intermediate variants.
  if (!SetTextureStageState(1, kD3dTssColorArg1, kD3dTaCurrent, "stage1 COLORARG1=CURRENT")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg2, kD3dTaTexture, "stage1 COLORARG2=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssAlphaOp, kD3dTopDisable, "stage1 ALPHAOP=DISABLE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopAdd, "stage1 COLOROP=ADD (enable)")) {
    return false;
  }
  if (!SetTextureStageState(2, kD3dTssColorOp, kD3dTopDisable, "stage2 COLOROP=DISABLE")) {
    return false;
  }

  // Bind only a user VS (PS stays NULL) to enter VS-only interop mode.
  D3D9DDI_HSHADER hUserVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStageVs,
                                            fixedfunc::kVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kVsPassthroughPosColor)),
                                            &hUserVs);
  if (!Check(hr == S_OK, "CreateShader(user VS)")) {
    return false;
  }
  cleanup.shaders.push_back(hUserVs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStageVs, hUserVs);
  if (!Check(hr == S_OK, "SetShader(VS=user)")) {
    return false;
  }

  Shader* user_vs = nullptr;
  Shader* ps_enabled = nullptr;
  Shader* ps_disabled = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    user_vs = dev->user_vs;
    ps_enabled = dev->ps;
    if (!Check(user_vs != nullptr, "user VS bound")) {
      return false;
    }
    if (!Check(dev->vs == user_vs, "VS-only interop binds the user VS")) {
      return false;
    }
  }
  if (!Check(ps_enabled != nullptr, "PS bound (stage1 enabled; VS-only interop)")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps_enabled, kPsOpTexld) == 2, "stage1 enabled => exactly 2 texld (VS-only interop)")) {
    return false;
  }
  if (!Check(ShaderTexldSamplerMask(ps_enabled) == 0x3u, "stage1 enabled => texld uses samplers s0 and s1 (VS-only interop)")) {
    return false;
  }

  // Disable stage1 (COLOROP=DISABLE): should switch to a stage0-only PS.
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_disabled = dev->ps;
  }
  if (!Check(ps_disabled != nullptr, "PS bound (stage1 disabled; VS-only interop)")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps_disabled, kPsOpTexld) == 1, "stage1 disabled => exactly 1 texld (VS-only interop)")) {
    return false;
  }
  if (!Check(ShaderTexldSamplerMask(ps_disabled) == 0x1u, "stage1 disabled => texld uses only sampler s0 (VS-only interop)")) {
    return false;
  }

  // Re-enable stage1.
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopAdd, "stage1 COLOROP=ADD (re-enable)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == ps_enabled, "stage1 re-enable reuses cached PS variant (VS-only interop)")) {
      return false;
    }
  }

  // Record a state block that disables stage1.
  D3D9DDI_HSTATEBLOCK hSb{};
  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore stage1 enabled so ApplyStateBlock can toggle it back off.
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopAdd, "stage1 COLOROP=ADD before ApplyStateBlock")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == ps_enabled, "PS enabled before ApplyStateBlock (VS-only interop)")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->vs == user_vs, "VS still bound before ApplyStateBlock (VS-only interop)")) {
      DeleteSb();
      return false;
    }
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(stage1 disable; VS-only interop)")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == ps_disabled, "ApplyStateBlock updates fixed-function PS (VS-only interop)")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->vs == user_vs, "ApplyStateBlock preserves VS binding (VS-only interop)")) {
      DeleteSb();
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock stage-state; VS-only interop)")) {
    DeleteSb();
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0,
             "ApplyStateBlock emits no CREATE_SHADER_DXBC (VS-only interop)")) {
    DeleteSb();
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "ApplyStateBlock emits BIND_SHADERS (VS-only interop)")) {
    DeleteSb();
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs == user_vs->handle, "ApplyStateBlock binds user VS")) {
    DeleteSb();
    return false;
  }
  if (!Check(last_bind->ps == ps_disabled->handle, "ApplyStateBlock binds stage1-disabled PS")) {
    DeleteSb();
    return false;
  }

  DeleteSb();
  return true;
}

bool TestApplyStateBlockUpdatesFixedfuncPsWhenTextureBindingChangesInVsOnlyInterop() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  D3DDDI_HRESOURCE hTex1{};
  if (!CreateDummyTexture(&cleanup, &hTex0) || !CreateDummyTexture(&cleanup, &hTex1)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: modulate tex0 * diffuse.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "stage0 COLORARG2=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage1: add current + tex1.
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopAdd, "stage1 COLOROP=ADD")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg1, kD3dTaCurrent, "stage1 COLORARG1=CURRENT")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg2, kD3dTaTexture, "stage1 COLORARG2=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssAlphaOp, kD3dTopDisable, "stage1 ALPHAOP=DISABLE")) {
    return false;
  }
  if (!SetTextureStageState(2, kD3dTssColorOp, kD3dTopDisable, "stage2 COLOROP=DISABLE")) {
    return false;
  }

  // Bind only a user VS (PS stays NULL).
  D3D9DDI_HSHADER hUserVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStageVs,
                                            fixedfunc::kVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kVsPassthroughPosColor)),
                                            &hUserVs);
  if (!Check(hr == S_OK, "CreateShader(user VS)")) {
    return false;
  }
  cleanup.shaders.push_back(hUserVs);
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStageVs, hUserVs);
  if (!Check(hr == S_OK, "SetShader(VS=user)")) {
    return false;
  }

  Shader* user_vs = nullptr;
  Shader* ps_bound = nullptr;
  Shader* ps_unbound = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    user_vs = dev->user_vs;
    ps_bound = dev->ps;
    if (!Check(user_vs != nullptr, "user VS bound")) {
      return false;
    }
    if (!Check(dev->vs == user_vs, "VS-only interop binds the user VS")) {
      return false;
    }
  }
  if (!Check(ps_bound != nullptr, "PS bound (stage1 texture bound; VS-only interop)")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps_bound, kPsOpTexld) == 2, "stage1 bound => 2 texld (VS-only interop)")) {
    return false;
  }
  if (!Check(ShaderTexldSamplerMask(ps_bound) == 0x3u, "stage1 bound => texld uses s0+s1 (VS-only interop)")) {
    return false;
  }

  // Unbind texture1 while stage1 still references TEXTURE => should switch to a
  // stage0-only PS (no stage1 sampling).
  {
    D3DDDI_HRESOURCE null_tex{};
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, null_tex);
    if (!Check(hr == S_OK, "SetTexture(stage1=null)")) {
      return false;
    }
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_unbound = dev->ps;
    if (!Check(dev->vs == user_vs, "VS preserved after texture unbind (VS-only interop)")) {
      return false;
    }
  }
  if (!Check(ps_unbound != nullptr, "PS bound (stage1 texture unbound; VS-only interop)")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps_unbound, kPsOpTexld) == 1, "stage1 unbound => 1 texld (VS-only interop)")) {
    return false;
  }
  if (!Check(ShaderTexldSamplerMask(ps_unbound) == 0x1u, "stage1 unbound => texld uses s0 only (VS-only interop)")) {
    return false;
  }

  // Restore texture1.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1=rebind)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == ps_bound, "stage1 rebind reuses cached PS variant (VS-only interop)")) {
      return false;
    }
  }

  // Record a state block that unbinds texture1.
  D3D9DDI_HSTATEBLOCK hSb{};
  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock")) {
    return false;
  }
  {
    D3DDDI_HRESOURCE null_tex{};
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, null_tex);
    if (!Check(hr == S_OK, "SetTexture(stage1=null) recorded")) {
      return false;
    }
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore texture1 again before applying the state block.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1=rebind after record)")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == ps_bound, "PS bound before ApplyStateBlock (VS-only interop)")) {
      DeleteSb();
      return false;
    }
  }

  // Isolate ApplyStateBlock emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(texture1 unbind; VS-only interop)")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->textures[1] == nullptr, "ApplyStateBlock unbinds stage1 texture")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->ps == ps_unbound, "ApplyStateBlock updates fixed-function PS (VS-only interop)")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->vs == user_vs, "ApplyStateBlock preserves VS binding (VS-only interop)")) {
      DeleteSb();
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock texture1 unbind; VS-only interop)")) {
    DeleteSb();
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    DeleteSb();
    return false;
  }

  bool saw_set_tex1_null = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_set_texture)) {
      continue;
    }
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 1 && st->texture == 0) {
      saw_set_tex1_null = true;
      break;
    }
  }
  if (!Check(saw_set_tex1_null, "ApplyStateBlock emits SET_TEXTURE(stage1=null)")) {
    DeleteSb();
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "ApplyStateBlock emits BIND_SHADERS")) {
    DeleteSb();
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs == user_vs->handle, "ApplyStateBlock binds user VS")) {
    DeleteSb();
    return false;
  }
  if (!Check(last_bind->ps == ps_unbound->handle, "ApplyStateBlock binds stage1-missing PS")) {
    DeleteSb();
    return false;
  }

  DeleteSb();
  return true;
}

bool TestApplyStateBlockFogRenderStateAffectsNextDraw() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;    // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;     // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u; // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;     // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;       // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;       // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.25f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.25f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  auto DrawAndCaptureShaders = [&](const char* msg, Shader** out_vs, Shader** out_ps) -> bool {
    dev->cmd.reset();
    const HRESULT hr2 = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
    if (!Check(hr2 == S_OK, msg)) {
      return false;
    }
    if (out_vs || out_ps) {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (out_vs) {
        *out_vs = dev->vs;
      }
      if (out_ps) {
        *out_ps = dev->ps;
      }
    }
    return true;
  };

  // First, pre-create both fog-off and fog-on fixed-function shader variants so
  // the ApplyStateBlock path can be validated without shader creation noise.
  Shader* vs_off = nullptr;
  Shader* ps_off = nullptr;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0)")) {
    return false;
  }
  if (!DrawAndCaptureShaders("DrawPrimitiveUP(fog off; seed variant)", &vs_off, &ps_off)) {
    return false;
  }
  if (!Check(vs_off != nullptr && ps_off != nullptr, "fog off: fixed-function shaders bound")) {
    return false;
  }

  constexpr float fog_start_a = 0.25f;
  constexpr float fog_end_a = 0.75f;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start_a));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART=A)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end_a));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND=A)")) {
    return false;
  }

  Shader* vs_on = nullptr;
  Shader* ps_on = nullptr;
  if (!DrawAndCaptureShaders("DrawPrimitiveUP(fog on; seed variant)", &vs_on, &ps_on)) {
    return false;
  }
  if (!Check(vs_on != nullptr && ps_on != nullptr, "fog on: fixed-function shaders bound")) {
    return false;
  }
  if (!Check(vs_on != vs_off, "fog toggle selects distinct VS variant")) {
    return false;
  }
  if (!Check(ps_on != ps_off, "fog toggle selects distinct PS variant")) {
    return false;
  }

  // Switch back to fog off and draw once so the current bindings are the non-fog
  // variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=0; return to baseline)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0; return to baseline)")) {
    return false;
  }
  Shader* vs_off2 = nullptr;
  Shader* ps_off2 = nullptr;
  if (!DrawAndCaptureShaders("DrawPrimitiveUP(fog off; baseline)", &vs_off2, &ps_off2)) {
    return false;
  }
  if (!Check(vs_off2 == vs_off, "fog disable reuses the fog-off VS variant")) {
    return false;
  }
  if (!Check(ps_off2 == ps_off, "fog disable reuses the fog-off PS variant")) {
    return false;
  }

  // Record a state block enabling fog with a new set of fog constants (B). Note
  // that state-block recording still updates the current device state, so we
  // restore A (fog off) afterwards before applying the block.
  constexpr float fog_start_b = 0.125f;
  constexpr float fog_end_b = 0.625f;
  constexpr float inv_range_b = 2.0f;

  D3D9DDI_HSTATEBLOCK hSb{};
  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(fog enable)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFF00FF00u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=green) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start_b));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART=B) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end_b));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND=B) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(fog enable)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore baseline fog-off state with the original constants (A).
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=0) restore")) {
    DeleteSb();
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0) restore")) {
    DeleteSb();
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red) restore")) {
    DeleteSb();
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start_a));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART=A) restore")) {
    DeleteSb();
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end_a));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND=A) restore")) {
    DeleteSb();
    return false;
  }

  const float expected[8] = {
      // c1: fog color (RGBA from ARGB green).
      0.0f, 1.0f, 0.0f, 1.0f,
      // c2: fog params (x=fog_start, y=inv_fog_range, z/w unused).
      fog_start_b, inv_range_b, 0.0f, 0.0f,
  };

  auto StreamHasExpectedFogConstants = [&](const uint8_t* buf, size_t len) -> bool {
    for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
      const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
      if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 1u || sc->vec4_count != 2u) {
        continue;
      }
      const size_t need = sizeof(*sc) + sizeof(expected);
      if (hdr->size_bytes < need) {
        continue;
      }
      const auto* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
      if (std::memcmp(payload, expected, sizeof(expected)) == 0) {
        return true;
      }
    }
    return false;
  };

  // Apply the fog state block (B). Some implementations may choose to upload fog
  // constants during ApplyStateBlock; others may defer uploads until the next
  // draw. Accept either behavior as long as the subsequent draw sees the correct
  // shader variant and constant values.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(fog enable; B)")) {
    DeleteSb();
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* apply_buf = dev->cmd.data();
  const size_t apply_len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(apply_buf, apply_len), "ValidateStream(ApplyStateBlock fog)")) {
    DeleteSb();
    return false;
  }
  if (!Check(CountOpcode(apply_buf, apply_len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0,
             "ApplyStateBlock emits no CREATE_SHADER_DXBC (fog)")) {
    DeleteSb();
    return false;
  }
  const bool apply_has_fog_upload = StreamHasExpectedFogConstants(apply_buf, apply_len);
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->render_states[kD3dRsFogEnable] == 1u, "ApplyStateBlock updates FOGENABLE")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->render_states[kD3dRsFogTableMode] == kD3dFogLinear, "ApplyStateBlock updates FOGTABLEMODE")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->render_states[kD3dRsFogColor] == 0xFF00FF00u, "ApplyStateBlock updates FOGCOLOR")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->render_states[kD3dRsFogStart] == F32Bits(fog_start_b), "ApplyStateBlock updates FOGSTART")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->render_states[kD3dRsFogEnd] == F32Bits(fog_end_b), "ApplyStateBlock updates FOGEND")) {
      DeleteSb();
      return false;
    }
  }

  // Next draw must select the fog shader variant and ensure fog constants match B.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(after ApplyStateBlock fog)")) {
    DeleteSb();
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* draw_buf = dev->cmd.data();
  const size_t draw_len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(draw_buf, draw_len), "ValidateStream(draw after ApplyStateBlock fog)")) {
    DeleteSb();
    return false;
  }
  if (!Check(CountOpcode(draw_buf, draw_len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0,
             "draw after ApplyStateBlock emits no CREATE_SHADER_DXBC (fog)")) {
    DeleteSb();
    return false;
  }
  const bool draw_has_fog_upload = StreamHasExpectedFogConstants(draw_buf, draw_len);
  if (!Check(apply_has_fog_upload || draw_has_fog_upload,
             "fog constants uploaded via ApplyStateBlock or next draw")) {
    DeleteSb();
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs == vs_on, "draw after ApplyStateBlock binds fog VS variant")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->ps == ps_on, "draw after ApplyStateBlock binds fog PS variant")) {
      DeleteSb();
      return false;
    }

    // Fog constants are uploaded into PS registers c1..c2.
    const float* cached_color = dev->ps_consts_f + 1u * 4u;
    const float* cached_params = dev->ps_consts_f + 2u * 4u;
    if (!Check(std::memcmp(cached_color, &expected[0], sizeof(float) * 4u) == 0,
               "ps_consts_f contains fog color from state block (B)")) {
      DeleteSb();
      return false;
    }
    if (!Check(std::memcmp(cached_params, &expected[4], sizeof(float) * 4u) == 0,
               "ps_consts_f contains fog params from state block (B)")) {
      DeleteSb();
      return false;
    }
  }

  DeleteSb();
  return true;
}

bool TestApplyStateBlockFogDoesNotSelectFogPsInVsOnlyInterop() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateShader != nullptr, "pfnCreateShader is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShader != nullptr, "pfnSetShader is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;    // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;     // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u; // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;     // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;       // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;       // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Use a trivial fixed-function stage chain (diffuse passthrough) so the only
  // difference between shader variants is fog. Also ensure ALPHAOP is valid to
  // avoid spurious INVALIDCALL failures.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopSelectArg1, "stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaDiffuse, "stage0 COLORARG1=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u},
      {1.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.25f, 1.0f, 0xFF00FF00u},
  };

  // Pre-create both the fog-off and fog-on fixed-function pixel shaders in full
  // fixed-function mode so the VS-only interop ApplyStateBlock path can be
  // validated without shader creation noise.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(seed fog-off PS)")) {
    return false;
  }

  Shader* ps_off = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_off = dev->ps;
  }
  if (!Check(ps_off != nullptr, "fog off: PS bound")) {
    return false;
  }

  constexpr float fog_start = 0.25f;
  constexpr float fog_end = 0.75f;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(seed fog-on PS)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* fog_buf = dev->cmd.data();
  const size_t fog_len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(fog_buf, fog_len), "ValidateStream(fog-on seed draw)")) {
    return false;
  }

  Shader* ps_fog = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_fog = dev->ps;
  }
  if (!Check(ps_fog != nullptr, "fog on: PS bound")) {
    return false;
  }
  if (!Check(ps_fog != ps_off, "fog toggle selects distinct fixed-function PS variant")) {
    return false;
  }

  // Switch back to fog-off baseline before entering VS-only interop mode.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=0 restore)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0 restore)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(return to fog-off baseline)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == ps_off, "fog-off baseline reuses fog-off PS variant")) {
      return false;
    }
  }

  // Bind only a user VS (PS stays NULL) to enter VS-only interop. Fog must not
  // affect the fixed-function PS selection in this mode (it would require fog
  // coordinates that user VS output layouts can't guarantee).
  D3D9DDI_HSHADER hUserVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStageVs,
                                            fixedfunc::kVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kVsPassthroughPosColor)),
                                            &hUserVs);
  if (!Check(hr == S_OK, "CreateShader(user VS)")) {
    return false;
  }
  cleanup.shaders.push_back(hUserVs);
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStageVs, hUserVs);
  if (!Check(hr == S_OK, "SetShader(VS=user)")) {
    return false;
  }

  Shader* user_vs = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    user_vs = dev->user_vs;
    if (!Check(user_vs != nullptr, "user VS bound")) {
      return false;
    }
    if (!Check(dev->vs == user_vs, "VS-only interop binds the user VS")) {
      return false;
    }
    if (!Check(dev->ps == ps_off, "VS-only interop uses fog-off PS variant (baseline)")) {
      return false;
    }
  }

  // Record a state block that enables fog.
  D3D9DDI_HSTATEBLOCK hSb{};
  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(fog enable)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1 recorded)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR recorded)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red recorded)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART recorded)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND recorded)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(fog enable)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore baseline fog-off render state before applying the block.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=0 restore before apply)")) {
    DeleteSb();
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0 restore before apply)")) {
    DeleteSb();
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == ps_off, "pre-apply: PS is fog-off variant")) {
      DeleteSb();
      return false;
    }
  }

  // Apply the fog state block in VS-only interop mode. The fixed-function PS must
  // remain the fog-off variant.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(fog enable; VS-only interop)")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs == user_vs, "ApplyStateBlock preserves user VS (VS-only interop)")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->ps == ps_off, "ApplyStateBlock does not select fog PS (VS-only interop)")) {
      DeleteSb();
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock fog; VS-only interop)")) {
    DeleteSb();
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    DeleteSb();
    return false;
  }

  // Fog constants must not be uploaded because the fog PS variant must not be
  // selected when a user VS is bound.
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage == AEROGPU_SHADER_STAGE_PIXEL && sc->start_register == 1u && sc->vec4_count == 2u) {
      DeleteSb();
      return Check(false, "ApplyStateBlock must not upload fog constants in VS-only interop");
    }
  }

  // Any shader rebinds must keep the fog-off PS bound.
  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!binds.empty()) {
    const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
    if (!Check(last_bind->vs == user_vs->handle, "ApplyStateBlock binds user VS (if rebinding)")) {
      DeleteSb();
      return false;
    }
    if (!Check(last_bind->ps == ps_off->handle, "ApplyStateBlock binds fog-off PS (if rebinding)")) {
      DeleteSb();
      return false;
    }
  }

  // Draw once: PS must still be fog-off and must not upload fog constants.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(after ApplyStateBlock fog; VS-only interop)")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs == user_vs, "draw preserves user VS (VS-only interop)")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->ps == ps_off, "draw preserves fog-off PS (VS-only interop)")) {
      DeleteSb();
      return false;
    }
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(draw after ApplyStateBlock fog; VS-only interop)")) {
    DeleteSb();
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0,
             "draw after ApplyStateBlock emits no CREATE_SHADER_DXBC (VS-only interop)")) {
    DeleteSb();
    return false;
  }
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage == AEROGPU_SHADER_STAGE_PIXEL && sc->start_register == 1u && sc->vec4_count == 2u) {
      DeleteSb();
      return Check(false, "draw must not upload fog constants in VS-only interop");
    }
  }

  DeleteSb();
  return true;
}

bool TestApplyStateBlockFogDoesNotSelectFogVsInPsOnlyInterop() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateShader != nullptr, "pfnCreateShader is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShader != nullptr, "pfnSetShader is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;    // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;     // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u; // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;     // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;       // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;       // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|TEX1)")) {
    return false;
  }

  // Bind only a user pixel shader (VS stays NULL) to enter PS-only interop.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  // Baseline: fog disabled.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0)")) {
    return false;
  }

  const VertexXyzTex1 tri[3] = {
      {-1.0f, -1.0f, 0.25f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.25f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.25f, 0.0f, 1.0f},
  };

  // Seed bindings/variants and ensure we are on the non-fog fixed-function VS.
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop baseline)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->user_vs == nullptr, "PS-only interop: user VS is NULL")) {
      return false;
    }
    if (!Check(dev->user_ps != nullptr, "PS-only interop: user PS is bound")) {
      return false;
    }
    if (!Check(dev->vs != nullptr, "PS-only interop: synthesized VS is bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsTransformPosWhiteTex1),
               "PS-only interop baseline uses non-fog VS variant")) {
      return false;
    }
  }

  constexpr float fog_start = 0.25f;
  constexpr float fog_end = 0.75f;

  // Record a state block that enables fog.
  D3D9DDI_HSTATEBLOCK hSb{};
  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(fog enable; PS-only interop)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1 recorded)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR recorded)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR recorded)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART recorded)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND recorded)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(fog enable; PS-only interop)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore baseline fog-off state so ApplyStateBlock actually transitions state.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=0 restore before apply)")) {
    DeleteSb();
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0 restore before apply)")) {
    DeleteSb();
    return false;
  }

  // Apply the fog state block. In PS-only interop mode, fog must NOT select fog
  // VS variants and must NOT upload fixed-function fog PS constants (c1..c2)
  // since the user PS is bound.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(fog enable; PS-only interop)")) {
    DeleteSb();
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->user_vs == nullptr, "PS-only interop: user VS remains NULL")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->vs != nullptr, "PS-only interop: synthesized VS remains bound")) {
      DeleteSb();
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsTransformPosWhiteTex1),
               "ApplyStateBlock fog does not select fog VS variant (PS-only interop)")) {
      DeleteSb();
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock fog; PS-only interop)")) {
    DeleteSb();
    return false;
  }

  size_t fog_const_uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage == AEROGPU_SHADER_STAGE_PIXEL && sc->start_register == 1u && sc->vec4_count == 2u) {
      ++fog_const_uploads;
    }
  }
  if (!Check(fog_const_uploads == 0, "ApplyStateBlock fog: does not upload fog PS constants in PS-only interop")) {
    DeleteSb();
    return false;
  }

  // Draw once: ensure fog is still ignored for VS selection and does not upload
  // fog constants (this draw must not clobber user PS constants).
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(after ApplyStateBlock fog; PS-only interop)")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsTransformPosWhiteTex1),
               "draw after ApplyStateBlock fog does not select fog VS variant (PS-only interop)")) {
      DeleteSb();
      return false;
    }
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  const size_t draw_len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, draw_len), "ValidateStream(draw after ApplyStateBlock fog; PS-only interop)")) {
    DeleteSb();
    return false;
  }
  fog_const_uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, draw_len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage == AEROGPU_SHADER_STAGE_PIXEL && sc->start_register == 1u && sc->vec4_count == 2u) {
      ++fog_const_uploads;
    }
  }
  if (!Check(fog_const_uploads == 0, "draw after ApplyStateBlock fog: does not upload fog PS constants in PS-only interop")) {
    DeleteSb();
    return false;
  }

  DeleteSb();
  return true;
}

bool TestApplyStateBlockLightingEnableReuploadsConstantsAfterClobber() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShaderConstF != nullptr, "pfnSetShaderConstF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  constexpr uint32_t kD3dRsLighting = 137u; // D3DRS_LIGHTING

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }

  // Force stable lighting constants by using identity transforms.
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &identity);
  if (!Check(hr == S_OK, "SetTransform(WORLD0 identity)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW identity)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION identity)")) {
    return false;
  }

  // Keep texture stage state trivial to avoid requiring textures.
  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopSelectArg1, "stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaDiffuse, "stage0 COLORARG1=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  // First, create and capture the lit VS variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  Shader* vs_lit = nullptr;
  {
    dev->cmd.reset();
    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(lit seed)")) {
      return false;
    }
    dev->cmd.finalize();
    const uint8_t* buf = dev->cmd.data();
    const size_t len = dev->cmd.bytes_used();
    if (!Check(ValidateStream(buf, len), "ValidateStream(lit seed draw)")) {
      return false;
    }
    if (!Check(CountVsConstantUploads(buf,
                                      len,
                                      kFixedfuncLightingStartRegister,
                                      kFixedfuncLightingVec4Count) == 1,
               "lit seed draw uploads lighting constants")) {
      return false;
    }
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    vs_lit = dev->vs;
  }
  if (!Check(vs_lit != nullptr, "lit seed draw binds VS")) {
    return false;
  }

  // Disable lighting and draw once to capture the unlit VS variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 0u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=FALSE)")) {
    return false;
  }
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(unlit seed)")) {
    return false;
  }
  Shader* vs_unlit = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    vs_unlit = dev->vs;
  }
  if (!Check(vs_unlit != nullptr, "unlit seed draw binds VS")) {
    return false;
  }
  if (!Check(vs_unlit != vs_lit, "lighting toggle selects distinct VS variants")) {
    return false;
  }

  // Simulate an app clobbering the reserved fixed-function lighting constant
  // range while lighting is disabled.
  const float junk[4] = {123.0f, 456.0f, 789.0f, 1011.0f};
  hr = cleanup.device_funcs.pfnSetShaderConstF(cleanup.hDevice,
                                               kD3dShaderStageVs,
                                               /*start_reg=*/kFixedfuncLightingStartRegister,
                                               junk,
                                               /*vec4_count=*/1);
  if (!Check(hr == S_OK, "SetShaderConstF(VS, lighting const clobber)")) {
    return false;
  }

  // Record a state block enabling lighting.
  D3D9DDI_HSTATEBLOCK hSb{};
  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(LIGHTING=TRUE)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore lighting disabled so ApplyStateBlock can toggle it back on.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 0u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=FALSE restore before apply)")) {
    DeleteSb();
    return false;
  }

  // ApplyStateBlock may choose to refresh lighting constants eagerly or defer
  // until the next draw; accept either behavior but require that the reserved
  // lighting constant range is refreshed exactly once.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(LIGHTING=TRUE)")) {
    DeleteSb();
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* apply_buf = dev->cmd.data();
  const size_t apply_len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(apply_buf, apply_len), "ValidateStream(ApplyStateBlock lighting)")) {
    DeleteSb();
    return false;
  }
  const size_t apply_uploads =
      CountVsConstantUploads(apply_buf, apply_len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  if (!Check(apply_uploads <= 1, "ApplyStateBlock lighting: at most one lighting constant upload")) {
    DeleteSb();
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(after ApplyStateBlock lighting)")) {
    DeleteSb();
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* draw_buf = dev->cmd.data();
  const size_t draw_len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(draw_buf, draw_len), "ValidateStream(draw after ApplyStateBlock lighting)")) {
    DeleteSb();
    return false;
  }
  const size_t draw_uploads =
      CountVsConstantUploads(draw_buf, draw_len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  if (!Check(draw_uploads <= 1, "draw after ApplyStateBlock lighting: at most one lighting constant upload")) {
    DeleteSb();
    return false;
  }
  if (!Check(apply_uploads + draw_uploads == 1,
             "lighting constants refreshed once after ApplyStateBlock(LIGHTING=TRUE)")) {
    DeleteSb();
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->render_states[kD3dRsLighting] == 1u, "ApplyStateBlock enables lighting")) {
      DeleteSb();
      return false;
    }
    if (!Check(dev->vs == vs_lit, "draw after ApplyStateBlock selects lit VS variant")) {
      DeleteSb();
      return false;
    }
    if (!Check(!dev->fixedfunc_lighting_dirty, "draw after ApplyStateBlock clears fixedfunc_lighting_dirty")) {
      DeleteSb();
      return false;
    }
  }

  DeleteSb();
  return true;
}

bool TestApplyStateBlockLight1ChangeReuploadsLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }

  // Use identity transforms so light data maps cleanly into the fixed-function
  // lighting constant block.
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &identity);
  if (!Check(hr == S_OK, "SetTransform(WORLD0 identity)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW identity)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION identity)")) {
    return false;
  }

  // Keep texture stage state trivial so the fixed-function PS doesn't require any textures.
  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopSelectArg1, "stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaDiffuse, "stage0 COLORARG1=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Configure two enabled directional lights so light1 is packed into slot1.
  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.0f, 0.0f, -1.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DLIGHT9 light1_old{};
  light1_old.Type = D3DLIGHT_DIRECTIONAL;
  light1_old.Direction = {1.0f, 0.0f, 0.0f};
  light1_old.Diffuse = {0.0f, 1.0f, 0.0f, 1.0f};
  light1_old.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/1, &light1_old);
  if (!Check(hr == S_OK, "SetLight(1)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/1, TRUE);
  if (!Check(hr == S_OK, "LightEnable(1, TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  // Seed shaders and the lighting constant cache.
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(seed lighting constants)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_lighting_dirty, "seed draw cleared fixedfunc_lighting_dirty")) {
      return false;
    }
  }

  // Record a state block that changes light1 diffuse (green -> blue).
  D3DLIGHT9 light1_new = light1_old;
  light1_new.Diffuse = {0.0f, 0.0f, 1.0f, 1.0f};

  D3D9DDI_HSTATEBLOCK hSb{};
  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(SetLight(1) blue)")) {
    return false;
  }
  hr = device_set_light(cleanup.hDevice, /*index=*/1, &light1_new);
  if (!Check(hr == S_OK, "SetLight(1 blue) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(SetLight(1) blue)")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore the old light1 values so ApplyStateBlock transitions state.
  hr = device_set_light(cleanup.hDevice, /*index=*/1, &light1_old);
  if (!Check(hr == S_OK, "SetLight(1 old restore)")) {
    DeleteSb();
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(clear dirty after restore)")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_lighting_dirty, "restore draw cleared fixedfunc_lighting_dirty")) {
      DeleteSb();
      return false;
    }
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(SetLight(1) blue)")) {
    DeleteSb();
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* apply_buf = dev->cmd.data();
  const size_t apply_len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(apply_buf, apply_len), "ValidateStream(ApplyStateBlock light1 change)")) {
    DeleteSb();
    return false;
  }
  if (!Check(CountOpcode(apply_buf, apply_len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0,
             "ApplyStateBlock emits no CREATE_SHADER_DXBC (light1 change)")) {
    DeleteSb();
    return false;
  }
  const size_t apply_uploads =
      CountVsConstantUploads(apply_buf, apply_len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  if (!Check(apply_uploads <= 1, "ApplyStateBlock light1 change: at most one lighting constant upload")) {
    DeleteSb();
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(after ApplyStateBlock light1 change)")) {
    DeleteSb();
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* draw_buf = dev->cmd.data();
  const size_t draw_len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(draw_buf, draw_len), "ValidateStream(draw after ApplyStateBlock light1 change)")) {
    DeleteSb();
    return false;
  }
  if (!Check(CountOpcode(draw_buf, draw_len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0,
             "draw after ApplyStateBlock emits no CREATE_SHADER_DXBC (light1 change)")) {
    DeleteSb();
    return false;
  }
  const size_t draw_uploads =
      CountVsConstantUploads(draw_buf, draw_len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  if (!Check(draw_uploads <= 1, "draw after ApplyStateBlock light1 change: at most one lighting constant upload")) {
    DeleteSb();
    return false;
  }
  if (!Check(apply_uploads + draw_uploads == 1,
             "light1 change refreshed lighting constants once (apply or draw)")) {
    DeleteSb();
    return false;
  }

  const float* payload = FindVsConstantsPayload(apply_buf, apply_len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  if (!payload) {
    payload = FindVsConstantsPayload(draw_buf, draw_len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  }
  if (!Check(payload != nullptr, "lighting constants payload present for light1 change")) {
    DeleteSb();
    return false;
  }

  // Directional slot1 diffuse is packed at c215 (slot base 214 + 1).
  constexpr uint32_t kLight1DiffuseRel = (215u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kLight1DiffuseRel * 4 + 0] == 0.0f &&
                 payload[kLight1DiffuseRel * 4 + 1] == 0.0f &&
                 payload[kLight1DiffuseRel * 4 + 2] == 1.0f &&
                 payload[kLight1DiffuseRel * 4 + 3] == 1.0f,
             "ApplyStateBlock light1 change updates slot1 diffuse to blue")) {
    DeleteSb();
    return false;
  }

  DeleteSb();
  return true;
}

bool TestApplyStateBlockLight1DisableReuploadsLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }

  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &identity);
  if (!Check(hr == S_OK, "SetTransform(WORLD0 identity)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW identity)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION identity)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopSelectArg1, "stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaDiffuse, "stage0 COLORARG1=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.0f, 0.0f, -1.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DLIGHT9 light1{};
  light1.Type = D3DLIGHT_DIRECTIONAL;
  light1.Direction = {1.0f, 0.0f, 0.0f};
  light1.Diffuse = {0.0f, 1.0f, 0.0f, 1.0f};
  light1.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/1, &light1);
  if (!Check(hr == S_OK, "SetLight(1)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/1, TRUE);
  if (!Check(hr == S_OK, "LightEnable(1, TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  // Seed constants for the two-light configuration.
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(seed two-light constants)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_lighting_dirty, "seed draw cleared fixedfunc_lighting_dirty")) {
      return false;
    }
  }

  // Record a state block that disables light1.
  D3D9DDI_HSTATEBLOCK hSb{};
  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock(LightEnable(1, FALSE))")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/1, FALSE);
  if (!Check(hr == S_OK, "LightEnable(1, FALSE) recorded")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock(LightEnable(1, FALSE))")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore light1 enabled so ApplyStateBlock transitions state.
  hr = device_light_enable(cleanup.hDevice, /*index=*/1, TRUE);
  if (!Check(hr == S_OK, "LightEnable(1, TRUE) restore")) {
    DeleteSb();
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(clear dirty after restore)")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(!dev->fixedfunc_lighting_dirty, "restore draw cleared fixedfunc_lighting_dirty")) {
      DeleteSb();
      return false;
    }
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock(LightEnable(1, FALSE))")) {
    DeleteSb();
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* apply_buf = dev->cmd.data();
  const size_t apply_len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(apply_buf, apply_len), "ValidateStream(ApplyStateBlock light1 disable)")) {
    DeleteSb();
    return false;
  }
  if (!Check(CountOpcode(apply_buf, apply_len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0,
             "ApplyStateBlock emits no CREATE_SHADER_DXBC (light1 disable)")) {
    DeleteSb();
    return false;
  }
  const size_t apply_uploads =
      CountVsConstantUploads(apply_buf, apply_len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  if (!Check(apply_uploads <= 1, "ApplyStateBlock light1 disable: at most one lighting constant upload")) {
    DeleteSb();
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(after ApplyStateBlock light1 disable)")) {
    DeleteSb();
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* draw_buf = dev->cmd.data();
  const size_t draw_len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(draw_buf, draw_len), "ValidateStream(draw after ApplyStateBlock light1 disable)")) {
    DeleteSb();
    return false;
  }
  if (!Check(CountOpcode(draw_buf, draw_len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0,
             "draw after ApplyStateBlock emits no CREATE_SHADER_DXBC (light1 disable)")) {
    DeleteSb();
    return false;
  }
  const size_t draw_uploads =
      CountVsConstantUploads(draw_buf, draw_len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  if (!Check(draw_uploads <= 1, "draw after ApplyStateBlock light1 disable: at most one lighting constant upload")) {
    DeleteSb();
    return false;
  }
  if (!Check(apply_uploads + draw_uploads == 1,
             "light1 disable refreshed lighting constants once (apply or draw)")) {
    DeleteSb();
    return false;
  }

  const float* payload = FindVsConstantsPayload(apply_buf, apply_len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  if (!payload) {
    payload = FindVsConstantsPayload(draw_buf, draw_len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  }
  if (!Check(payload != nullptr, "lighting constants payload present for light1 disable")) {
    DeleteSb();
    return false;
  }

  // With only one directional light enabled, slot1 diffuse (c215) should be zero.
  constexpr uint32_t kLight1DiffuseRel = (215u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kLight1DiffuseRel * 4 + 0] == 0.0f &&
                 payload[kLight1DiffuseRel * 4 + 1] == 0.0f &&
                 payload[kLight1DiffuseRel * 4 + 2] == 0.0f &&
                 payload[kLight1DiffuseRel * 4 + 3] == 0.0f,
             "ApplyStateBlock light1 disable clears slot1 diffuse")) {
    DeleteSb();
    return false;
  }

  DeleteSb();
  return true;
}

bool TestFvfXyzDiffuseDrawPrimitiveVbUploadsWvpAndBindsVb() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitive != nullptr, "pfnDrawPrimitive is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_decl[] = {
      // stream, offset, type, method, usage, usage_index
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  // Provide a non-identity transform so the fixed-function WVP constant upload is
  // easy to spot (WVP columns are uploaded into c240..c243).
  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  // Fixed-function emulation for XYZ vertices uses a WVP vertex shader and
  // uploads the matrix into reserved VS constants c240..c243 as column vectors.
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };

  aerogpu_handle_t expected_input_layout = 0;
  bool decl_ok = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzDiffuse);
    if (variant != FixedFuncVariant::NONE) {
      const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
      if (pipe.vertex_decl) {
        expected_input_layout = pipe.vertex_decl->handle;
        const auto& blob = pipe.vertex_decl->blob;
        decl_ok = (blob.size() == sizeof(expected_decl)) &&
                  (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
      }
    }
  }

  // Set a simple world translation; view/projection are identity.
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }

  if (!Check(expected_input_layout != 0, "SetFVF XYZ|DIFFUSE created internal input layout")) {
    return false;
  }
  if (!Check(decl_ok, "XYZ|DIFFUSE internal vertex decl matches expected layout (VB draw)")) {
    return false;
  }

  // Create a VB (non-UP draw path) with a leading dummy vertex, then draw starting
  // at vertex 1. This exercises `start_vertex` handling in the draw packet.
  const VertexXyzDiffuse verts[4] = {
      {123.0f, 456.0f, 0.0f, 0xFFFFFFFFu},
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };

  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyz|diffuse)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock_dev(dev->mutex);
    auto* vb = reinterpret_cast<Resource*>(create_vb.hResource.pDrvPrivate);
    expected_vb = vb ? vb->handle : 0;
  }
  if (!Check(expected_vb != 0, "vb handle non-zero")) {
    return false;
  }

  D3D9DDIARG_LOCK lock{};
  lock.hResource = create_vb.hResource;
  lock.offset_bytes = 0;
  lock.size_bytes = 0;
  lock.flags = 0;
  D3DDDI_LOCKEDBOX box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(vertex buffer xyz|diffuse)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock returns pData")) {
    return false;
  }
  std::memcpy(box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = create_vb.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(vertex buffer xyz|diffuse)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb xyz|diffuse)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitive(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*start_vertex=*/1, /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawPrimitive(triangle xyz|diffuse)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock_dev(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzDiffuse);
    if (!Check(variant == FixedFuncVariant::XYZ_COLOR, "fixedfunc_variant_from_fvf(XYZ|DIFFUSE) == XYZ_COLOR")) {
      return false;
    }
    const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
    if (!Check(pipe.vs != nullptr, "XYZ|DIFFUSE fixed-function VS created")) {
      return false;
    }
    if (!Check(dev->vs == pipe.vs, "XYZ|DIFFUSE binds WVP VS")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColor), "XYZ|DIFFUSE VS bytecode matches kVsWvpPosColor")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer == nullptr, "VB draw does not allocate scratch UP buffer")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE VB draw)")) {
    return false;
  }

  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal XYZ|DIFFUSE layout handle (VB draw)")) {
    return false;
  }

  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(
        reinterpret_cast<const uint8_t*>(svb) + sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzDiffuse)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds the created VB (XYZ|DIFFUSE)")) {
    return false;
  }

  bool saw_draw = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_DRAW)) {
    const auto* d = reinterpret_cast<const aerogpu_cmd_draw*>(hdr);
    if (d->first_vertex == 1 && d->vertex_count == 3 && d->instance_count == 1) {
      saw_draw = true;
      break;
    }
  }
  if (!Check(saw_draw, "DRAW uses start_vertex=1 vertex_count=3 instance_count=1")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != kFixedfuncMatrixStartRegister || sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (VB draw)")) {
    return false;
  }

  return true;
}

bool TestFvfXyzrhwDiffuseLightingEnabledStillDraws() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW|DIFFUSE; lighting=on)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "VS bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsPassthroughPosColor),
               "XYZRHW|DIFFUSE uses kVsPassthroughPosColor even when lighting is enabled")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZRHW|DIFFUSE; lighting=on)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS emitted")) {
    return false;
  }
  for (const auto* hdr : binds) {
    const auto* bs = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
    if (!Check(bs->vs != 0 && bs->ps != 0, "BIND_SHADERS binds non-zero VS/PS")) {
      return false;
    }
  }

  // Sanity-check that fixed-function lighting constant uploads are not emitted for
  // pre-transformed XYZRHW vertices (D3DRS_LIGHTING is ignored).
  bool saw_lighting_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage == AEROGPU_SHADER_STAGE_VERTEX &&
        sc->start_register == kFixedfuncLightingStartRegister &&
        sc->vec4_count == kFixedfuncLightingVec4Count) {
      saw_lighting_constants = true;
      break;
    }
  }
  if (!Check(!saw_lighting_constants, "XYZRHW draws do not upload fixed-function lighting constants")) {
    return false;
  }

  return true;
}

bool TestFvfXyzDiffuseLightingEnabledFailsInvalidCall() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  // MVP behavior: if fixed-function lighting is enabled with an FVF that does not
  // supply normals (and is not pre-transformed XYZRHW), the fixed-function
  // fallback fails the draw cleanly with D3DERR_INVALIDCALL (rather than silently
  // selecting the unlit VS).
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  const VertexXyzDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, 0xFFFFFFFFu},
  };

  const size_t before_draw = dev->cmd.bytes_used();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(XYZ|DIFFUSE; lighting=on) => INVALIDCALL")) {
    return false;
  }
  if (!Check(dev->cmd.bytes_used() == before_draw, "invalid draw emits no new commands")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE; lighting=on => INVALIDCALL)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 0, "invalid draw does not emit DRAW packets")) {
    return false;
  }

  return true;
}

bool TestFvfXyzrhwDiffuseTex1EmitsTextureAndShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  aerogpu_handle_t expected_input_layout = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzrhwDiffuseTex1);
    if (variant != FixedFuncVariant::NONE) {
      const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
      if (pipe.vertex_decl) {
        expected_input_layout = pipe.vertex_decl->handle;
      }
    }
  }
  if (!Check(expected_input_layout != 0, "SetFVF TEX1 created internal input layout")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  auto* tex = reinterpret_cast<Resource*>(hTex.pDrvPrivate);
  if (!Check(tex != nullptr, "texture resource pointer")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle tex1)")) {
    return false;
  }

  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (dev->up_vertex_buffer) {
      expected_vb = dev->up_vertex_buffer->handle;
    }
  }
  if (!Check(expected_vb != 0, "DrawPrimitiveUP TEX1 created scratch vertex buffer")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_TEXTURE) >= 1, "SET_TEXTURE emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) >= 2, "CREATE_SHADER_DXBC emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT) >= 1, "SET_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS) >= 1, "SET_VERTEX_BUFFERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }

  // Validate shader creation includes both stages.
  bool saw_vs = false;
  bool saw_ps = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
    const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
    if (cs->stage == AEROGPU_SHADER_STAGE_VERTEX) {
      saw_vs = true;
    } else if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
      saw_ps = true;
    }
  }
  if (!Check(saw_vs && saw_ps, "CREATE_SHADER_DXBC includes VS and PS stages (TEX1)")) {
    return false;
  }

  // Validate the input layout being set matches the internal FVF declaration.
  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal TEX1 FVF layout handle")) {
    return false;
  }

  // Validate at least one vertex buffer binding references the scratch UP buffer.
  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(reinterpret_cast<const uint8_t*>(svb) +
                                                                                  sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzrhwDiffuseTex1)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds scratch UP buffer (TEX1)")) {
    return false;
  }

  // Validate draw parameters (trianglelist => 3 vertices).
  bool saw_draw3 = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_DRAW)) {
    const auto* d = reinterpret_cast<const aerogpu_cmd_draw*>(hdr);
    if (d->vertex_count == 3 && d->instance_count == 1) {
      saw_draw3 = true;
      break;
    }
  }
  if (!Check(saw_draw3, "DRAW has expected vertex_count=3 instance_count=1 (TEX1)")) {
    return false;
  }

  const auto set_textures = CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE);
  if (!Check(!set_textures.empty(), "SET_TEXTURE packets collected")) {
    return false;
  }
  const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(set_textures.back());
  if (!Check(st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL, "SET_TEXTURE shader_stage == PIXEL")) {
    return false;
  }
  if (!Check(st->slot == 0, "SET_TEXTURE slot == 0")) {
    return false;
  }
  if (!Check(st->texture == tex->handle, "SET_TEXTURE uses created texture handle")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs != 0 && last_bind->ps != 0, "BIND_SHADERS binds non-zero VS/PS")) {
    return false;
  }

  return true;
}

bool TestFvfXyzDiffuseTex1EmitsTextureAndShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const VertexXyzDiffuseTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz tex1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_TEXTURE) >= 1, "SET_TEXTURE emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) >= 2, "CREATE_SHADER_DXBC emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT) >= 1, "SET_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS) >= 1, "SET_VERTEX_BUFFERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }

  const auto set_textures = CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE);
  if (!Check(!set_textures.empty(), "SET_TEXTURE packets collected")) {
    return false;
  }
  const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(set_textures.back());
  if (!Check(st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL, "SET_TEXTURE shader_stage == PIXEL")) {
    return false;
  }
  if (!Check(st->slot == 0, "SET_TEXTURE slot == 0")) {
    return false;
  }
  if (!Check(st->texture != 0, "SET_TEXTURE texture handle non-zero")) {
    return false;
  }

  return true;
}

bool TestFixedfuncTex1SupportsTexcoordSizeBits() {
  struct Case {
    const char* name = nullptr;
    uint32_t tex0_size_bits = 0;
    uint8_t decl_type = kD3dDeclTypeFloat2;
    // For XYZRHW draws.
    const void* tri_xyzrhw = nullptr;
    uint32_t stride_xyzrhw = 0;
    // For XYZ draws.
    const void* tri_xyz = nullptr;
    uint32_t stride_xyz = 0;
  };

  const VertexXyzrhwDiffuseTex1F1 tri_xyzrhw_f1[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.5f},
  };
  const VertexXyzrhwDiffuseTex1F3 tri_xyzrhw_f3[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f, 0.0f},
  };
  const VertexXyzrhwDiffuseTex1F4 tri_xyzrhw_f4[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f, 0.0f, 1.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f, 0.0f, 1.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f, 0.0f, 1.0f},
  };

  const VertexXyzDiffuseTex1F1 tri_xyz_f1[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 0.0f},
      {1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 1.0f},
      {-1.0f, 1.0f, 0.0f, 0xFFFFFFFFu, 0.0f},
  };
  const VertexXyzDiffuseTex1F3 tri_xyz_f3[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 1.0f, 0.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 1.0f, 0.0f},
  };
  const VertexXyzDiffuseTex1F4 tri_xyz_f4[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 0.0f, 0.0f, 1.0f},
      {1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 1.0f, 0.0f, 0.0f, 1.0f},
      {-1.0f, 1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 1.0f, 0.0f, 1.0f},
  };

  const Case cases[] = {
      {"texcoord0_float1",
       kD3dFvfTexCoordSize1_0,
       kD3dDeclTypeFloat1,
       tri_xyzrhw_f1,
       static_cast<uint32_t>(sizeof(VertexXyzrhwDiffuseTex1F1)),
       tri_xyz_f1,
       static_cast<uint32_t>(sizeof(VertexXyzDiffuseTex1F1))},
      {"texcoord0_float3",
       kD3dFvfTexCoordSize3_0,
       kD3dDeclTypeFloat3,
       tri_xyzrhw_f3,
       static_cast<uint32_t>(sizeof(VertexXyzrhwDiffuseTex1F3)),
       tri_xyz_f3,
       static_cast<uint32_t>(sizeof(VertexXyzDiffuseTex1F3))},
      {"texcoord0_float4",
       kD3dFvfTexCoordSize4_0,
       kD3dDeclTypeFloat4,
       tri_xyzrhw_f4,
       static_cast<uint32_t>(sizeof(VertexXyzrhwDiffuseTex1F4)),
       tri_xyz_f4,
       static_cast<uint32_t>(sizeof(VertexXyzDiffuseTex1F4))},
  };

  for (const auto& c : cases) {
    // -------------------------------------------------------------------------
    // XYZRHW | DIFFUSE | TEX1 with non-default TEXCOORDSIZE0
    // -------------------------------------------------------------------------
    {
      CleanupDevice cleanup;
      if (!CreateDevice(&cleanup)) {
        return false;
      }

      auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
      if (!Check(dev != nullptr, "device pointer")) {
        return false;
      }

      dev->cmd.reset();

      const uint32_t fvf = kFvfXyzrhwDiffuseTex1 | c.tex0_size_bits;
      HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, fvf);
      if (!Check(hr == S_OK, c.name)) {
        return false;
      }

      VertexDecl* expected_decl = nullptr;
      {
        std::lock_guard<std::mutex> lock(dev->mutex);
        const auto it = dev->fvf_vertex_decl_cache.find(fvf);
        if (!Check(it != dev->fvf_vertex_decl_cache.end(), "custom FVF decl cached (XYZRHW)")) {
          return false;
        }
        expected_decl = it->second;
        if (!Check(expected_decl != nullptr, "custom FVF decl non-null (XYZRHW)")) {
          return false;
        }
        if (!Check(dev->vertex_decl == expected_decl, "custom FVF decl bound (XYZRHW)")) {
          return false;
        }
      }

      const D3DVERTEXELEMENT9_COMPAT expected_blob[] = {
          {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
          {0, 16, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
          {0, 20, c.decl_type, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
          {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
      };
      if (!Check(expected_decl->blob.size() == sizeof(expected_blob), "custom decl blob size (XYZRHW)")) {
        return false;
      }
      if (!Check(std::memcmp(expected_decl->blob.data(), expected_blob, sizeof(expected_blob)) == 0,
                 "custom decl blob matches expected layout (XYZRHW)")) {
        return false;
      }

      D3DDDI_HRESOURCE hTex{};
      if (!CreateDummyTexture(&cleanup, &hTex)) {
        return false;
      }
      hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
      if (!Check(hr == S_OK, "SetTexture(stage0)")) {
        return false;
      }

      hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
          cleanup.hDevice,
          D3DDDIPT_TRIANGLELIST,
          /*primitive_count=*/1,
          c.tri_xyzrhw,
          c.stride_xyzrhw);
      if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW custom TEXCOORDSIZE0)")) {
        std::fprintf(stderr, "FAIL: %s: DrawPrimitiveUP(XYZRHW) hr=0x%08x\n", c.name, static_cast<unsigned>(hr));
        return false;
      }

      {
        std::lock_guard<std::mutex> lock(dev->mutex);
        if (!Check(dev->vertex_decl == expected_decl, "custom FVF decl preserved at draw (XYZRHW)")) {
          return false;
        }
        if (!Check(dev->ps != nullptr, "PS bound (XYZRHW)")) {
          return false;
        }
        if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "PS contains texld (XYZRHW)")) {
          return false;
        }
        if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "PS contains mul (XYZRHW)")) {
          return false;
        }
      }
    }

    // -------------------------------------------------------------------------
    // XYZ | DIFFUSE | TEX1 WVP path with non-default TEXCOORDSIZE0
    // -------------------------------------------------------------------------
    {
      CleanupDevice cleanup;
      if (!CreateDevice(&cleanup)) {
        return false;
      }

      auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
      if (!Check(dev != nullptr, "device pointer")) {
        return false;
      }

      dev->cmd.reset();

      const uint32_t fvf = kFvfXyzDiffuseTex1 | c.tex0_size_bits;
      HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, fvf);
      if (!Check(hr == S_OK, c.name)) {
        return false;
      }

      VertexDecl* expected_decl = nullptr;
      {
        std::lock_guard<std::mutex> lock(dev->mutex);
        const auto it = dev->fvf_vertex_decl_cache.find(fvf);
        if (!Check(it != dev->fvf_vertex_decl_cache.end(), "custom FVF decl cached (XYZ)")) {
          return false;
        }
        expected_decl = it->second;
        if (!Check(expected_decl != nullptr, "custom FVF decl non-null (XYZ)")) {
          return false;
        }
        if (!Check(dev->vertex_decl == expected_decl, "custom FVF decl bound (XYZ)")) {
          return false;
        }
      }

      const D3DVERTEXELEMENT9_COMPAT expected_blob[] = {
          {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
          {0, 12, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
          {0, 16, c.decl_type, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
          {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
      };
      if (!Check(expected_decl->blob.size() == sizeof(expected_blob), "custom decl blob size (XYZ)")) {
        return false;
      }
      if (!Check(std::memcmp(expected_decl->blob.data(), expected_blob, sizeof(expected_blob)) == 0,
                 "custom decl blob matches expected layout (XYZ)")) {
        return false;
      }

      D3DDDI_HRESOURCE hTex{};
      if (!CreateDummyTexture(&cleanup, &hTex)) {
        return false;
      }
      hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
      if (!Check(hr == S_OK, "SetTexture(stage0)")) {
        return false;
      }

      hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
          cleanup.hDevice,
          D3DDDIPT_TRIANGLELIST,
          /*primitive_count=*/1,
          c.tri_xyz,
          c.stride_xyz);
      if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ custom TEXCOORDSIZE0)")) {
        std::fprintf(stderr, "FAIL: %s: DrawPrimitiveUP(XYZ) hr=0x%08x\n", c.name, static_cast<unsigned>(hr));
        return false;
      }

      {
        std::lock_guard<std::mutex> lock(dev->mutex);
        if (!Check(dev->vertex_decl == expected_decl, "custom FVF decl preserved at draw (XYZ)")) {
          return false;
        }
        const FixedFuncVariant variant = fixedfunc_variant_from_fvf(dev->fvf);
        if (!Check(variant == FixedFuncVariant::XYZ_COLOR_TEX1, "implied fixedfunc variant == XYZ_COLOR_TEX1")) {
          return false;
        }
        const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
        if (!Check(pipe.vs != nullptr, "fixedfunc pipeline VS created (XYZ_COLOR_TEX1)")) {
          return false;
        }
        if (!Check(dev->vs == pipe.vs, "XYZ custom TEXCOORDSIZE0 binds WVP VS")) {
          return false;
        }
        if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColorTex0),
                   "XYZ custom TEXCOORDSIZE0 VS bytecode matches kVsWvpPosColorTex0")) {
          return false;
        }
        if (!Check(dev->ps != nullptr, "PS bound (XYZ)")) {
          return false;
        }
        if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "PS contains texld (XYZ)")) {
          return false;
        }
        if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "PS contains mul (XYZ)")) {
          return false;
        }
      }
    }
  }

  return true;
}

bool TestFixedfuncTex1NoDiffuseSupportsTexcoordSizeBits() {
  struct Case {
    const char* name = nullptr;
    uint32_t tex0_size_bits = 0;
    uint8_t decl_type = kD3dDeclTypeFloat2;
    // For XYZRHW | TEX1 draws.
    const void* tri_xyzrhw = nullptr;
    uint32_t stride_xyzrhw = 0;
    // For XYZ | TEX1 draws.
    const void* tri_xyz = nullptr;
    uint32_t stride_xyz = 0;
  };

  const VertexXyzrhwTex1F1 tri_xyzrhw_f1[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 1.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0.5f},
  };
  const VertexXyzrhwTex1F3 tri_xyzrhw_f3[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f, 0.25f},
      {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f, 0.25f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f, 0.25f},
  };
  const VertexXyzrhwTex1F4 tri_xyzrhw_f4[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f, 0.25f, 1.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f, 0.25f, 1.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f, 0.25f, 1.0f},
  };

  const VertexXyzTex1F1 tri_xyz_f1[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f},
  };
  const VertexXyzTex1F3 tri_xyz_f3[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f, 0.25f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f, 0.25f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f, 0.25f},
  };
  const VertexXyzTex1F4 tri_xyz_f4[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f, 0.25f, 1.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f, 0.25f, 1.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f, 0.25f, 1.0f},
  };

  const Case cases[] = {
      {"texcoord0_float1",
       kD3dFvfTexCoordSize1_0,
       kD3dDeclTypeFloat1,
       tri_xyzrhw_f1,
       static_cast<uint32_t>(sizeof(VertexXyzrhwTex1F1)),
       tri_xyz_f1,
       static_cast<uint32_t>(sizeof(VertexXyzTex1F1))},
      {"texcoord0_float3",
       kD3dFvfTexCoordSize3_0,
       kD3dDeclTypeFloat3,
       tri_xyzrhw_f3,
       static_cast<uint32_t>(sizeof(VertexXyzrhwTex1F3)),
       tri_xyz_f3,
       static_cast<uint32_t>(sizeof(VertexXyzTex1F3))},
      {"texcoord0_float4",
       kD3dFvfTexCoordSize4_0,
       kD3dDeclTypeFloat4,
       tri_xyzrhw_f4,
       static_cast<uint32_t>(sizeof(VertexXyzrhwTex1F4)),
       tri_xyz_f4,
       static_cast<uint32_t>(sizeof(VertexXyzTex1F4))},
  };

  for (const auto& c : cases) {
    // -------------------------------------------------------------------------
    // XYZRHW | TEX1 with non-default TEXCOORDSIZE0
    // -------------------------------------------------------------------------
    {
      CleanupDevice cleanup;
      if (!CreateDevice(&cleanup)) {
        return false;
      }

      auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
      if (!Check(dev != nullptr, "device pointer")) {
        return false;
      }

      const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
        HRESULT hr2 = S_OK;
        if (cleanup.device_funcs.pfnSetTextureStageState) {
          hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
        } else {
          hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
        }
        return Check(hr2 == S_OK, msg);
      };

      dev->cmd.reset();

      const uint32_t fvf = kFvfXyzrhwTex1 | c.tex0_size_bits;
      HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, fvf);
      if (!Check(hr == S_OK, c.name)) {
        return false;
      }

      VertexDecl* expected_decl = nullptr;
      {
        std::lock_guard<std::mutex> lock(dev->mutex);
        const auto it = dev->fvf_vertex_decl_cache.find(fvf);
        if (!Check(it != dev->fvf_vertex_decl_cache.end(), "custom FVF decl cached (XYZRHW|TEX1)")) {
          return false;
        }
        expected_decl = it->second;
        if (!Check(expected_decl != nullptr, "custom FVF decl non-null (XYZRHW|TEX1)")) {
          return false;
        }
        if (!Check(dev->vertex_decl == expected_decl, "custom FVF decl bound (XYZRHW|TEX1)")) {
          return false;
        }
      }

      const D3DVERTEXELEMENT9_COMPAT expected_blob[] = {
          {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
          {0, 16, c.decl_type, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
          {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
      };
      if (!Check(expected_decl->blob.size() == sizeof(expected_blob), "custom decl blob size (XYZRHW|TEX1)")) {
        return false;
      }
      if (!Check(std::memcmp(expected_decl->blob.data(), expected_blob, sizeof(expected_blob)) == 0,
                 "custom decl blob matches expected layout (XYZRHW|TEX1)")) {
        return false;
      }

      D3DDDI_HRESOURCE hTex{};
      if (!CreateDummyTexture(&cleanup, &hTex)) {
        return false;
      }
      hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
      if (!Check(hr == S_OK, "SetTexture(stage0)")) {
        return false;
      }

      // Ensure a known stage0 state (modulate texture with vertex diffuse; VS supplies white diffuse).
      if (!SetTextureStageState(/*stage=*/0,
                                kD3dTssColorOp,
                                kD3dTopModulate,
                                "XYZRHW|TEX1: SetTextureStageState(COLOROP=MODULATE) succeeds")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0,
                                kD3dTssColorArg1,
                                kD3dTaTexture,
                                "XYZRHW|TEX1: SetTextureStageState(COLORARG1=TEXTURE) succeeds")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0,
                                kD3dTssColorArg2,
                                kD3dTaDiffuse,
                                "XYZRHW|TEX1: SetTextureStageState(COLORARG2=DIFFUSE) succeeds")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0,
                                kD3dTssAlphaOp,
                                kD3dTopSelectArg1,
                                "XYZRHW|TEX1: SetTextureStageState(ALPHAOP=SELECTARG1) succeeds")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0,
                                kD3dTssAlphaArg1,
                                kD3dTaTexture,
                                "XYZRHW|TEX1: SetTextureStageState(ALPHAARG1=TEXTURE) succeeds")) {
        return false;
      }

      hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
          cleanup.hDevice,
          D3DDDIPT_TRIANGLELIST,
          /*primitive_count=*/1,
          c.tri_xyzrhw,
          c.stride_xyzrhw);
      if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW|TEX1 custom TEXCOORDSIZE0)")) {
        std::fprintf(stderr, "FAIL: %s: DrawPrimitiveUP(XYZRHW|TEX1) hr=0x%08x\n", c.name, static_cast<unsigned>(hr));
        return false;
      }

      {
        std::lock_guard<std::mutex> lock(dev->mutex);
        if (!Check(dev->vertex_decl == expected_decl, "custom FVF decl preserved at draw (XYZRHW|TEX1)")) {
          return false;
        }
        const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(FixedFuncVariant::RHW_TEX1)];
        if (!Check(pipe.vs != nullptr, "fixedfunc RHW_TEX1 VS created")) {
          return false;
        }
        if (!Check(dev->vs == pipe.vs, "XYZRHW|TEX1 binds nodiffuse passthrough VS")) {
          return false;
        }
        if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsPassthroughPosWhiteTex1),
                   "XYZRHW|TEX1 VS bytecode matches kVsPassthroughPosWhiteTex1")) {
          return false;
        }
        if (!Check(dev->ps != nullptr, "PS bound (XYZRHW|TEX1)")) {
          return false;
        }
        if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "PS contains texld (XYZRHW|TEX1)")) {
          return false;
        }
        if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "PS contains mul (XYZRHW|TEX1)")) {
          return false;
        }
      }
    }

    // -------------------------------------------------------------------------
    // XYZ | TEX1 WVP path with non-default TEXCOORDSIZE0
    // -------------------------------------------------------------------------
    {
      CleanupDevice cleanup;
      if (!CreateDevice(&cleanup)) {
        return false;
      }

      auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
      if (!Check(dev != nullptr, "device pointer")) {
        return false;
      }

      const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
        HRESULT hr2 = S_OK;
        if (cleanup.device_funcs.pfnSetTextureStageState) {
          hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
        } else {
          hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
        }
        return Check(hr2 == S_OK, msg);
      };

      dev->cmd.reset();

      const uint32_t fvf = kFvfXyzTex1 | c.tex0_size_bits;
      HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, fvf);
      if (!Check(hr == S_OK, c.name)) {
        return false;
      }

      VertexDecl* expected_decl = nullptr;
      {
        std::lock_guard<std::mutex> lock(dev->mutex);
        const auto it = dev->fvf_vertex_decl_cache.find(fvf);
        if (!Check(it != dev->fvf_vertex_decl_cache.end(), "custom FVF decl cached (XYZ|TEX1)")) {
          return false;
        }
        expected_decl = it->second;
        if (!Check(expected_decl != nullptr, "custom FVF decl non-null (XYZ|TEX1)")) {
          return false;
        }
        if (!Check(dev->vertex_decl == expected_decl, "custom FVF decl bound (XYZ|TEX1)")) {
          return false;
        }
      }

      const D3DVERTEXELEMENT9_COMPAT expected_blob[] = {
          {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
          {0, 12, c.decl_type, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
          {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
      };
      if (!Check(expected_decl->blob.size() == sizeof(expected_blob), "custom decl blob size (XYZ|TEX1)")) {
        return false;
      }
      if (!Check(std::memcmp(expected_decl->blob.data(), expected_blob, sizeof(expected_blob)) == 0,
                 "custom decl blob matches expected layout (XYZ|TEX1)")) {
        return false;
      }

      // Ensure a stable WVP (identity) so the WVP path can't observe uninitialized transforms.
      if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
        return false;
      }
      D3DMATRIX identity{};
      identity.m[0][0] = 1.0f;
      identity.m[1][1] = 1.0f;
      identity.m[2][2] = 1.0f;
      identity.m[3][3] = 1.0f;
      hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
      if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
        return false;
      }
      hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
      if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
        return false;
      }
      hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &identity);
      if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
        return false;
      }

      D3DDDI_HRESOURCE hTex{};
      if (!CreateDummyTexture(&cleanup, &hTex)) {
        return false;
      }
      hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
      if (!Check(hr == S_OK, "SetTexture(stage0)")) {
        return false;
      }

      // Ensure a known stage0 state (modulate texture with vertex diffuse; VS supplies white diffuse).
      if (!SetTextureStageState(/*stage=*/0,
                                kD3dTssColorOp,
                                kD3dTopModulate,
                                "XYZ|TEX1: SetTextureStageState(COLOROP=MODULATE) succeeds")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0,
                                kD3dTssColorArg1,
                                kD3dTaTexture,
                                "XYZ|TEX1: SetTextureStageState(COLORARG1=TEXTURE) succeeds")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0,
                                kD3dTssColorArg2,
                                kD3dTaDiffuse,
                                "XYZ|TEX1: SetTextureStageState(COLORARG2=DIFFUSE) succeeds")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0,
                                kD3dTssAlphaOp,
                                kD3dTopSelectArg1,
                                "XYZ|TEX1: SetTextureStageState(ALPHAOP=SELECTARG1) succeeds")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0,
                                kD3dTssAlphaArg1,
                                kD3dTaTexture,
                                "XYZ|TEX1: SetTextureStageState(ALPHAARG1=TEXTURE) succeeds")) {
        return false;
      }

      hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
          cleanup.hDevice,
          D3DDDIPT_TRIANGLELIST,
          /*primitive_count=*/1,
          c.tri_xyz,
          c.stride_xyz);
      if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|TEX1 custom TEXCOORDSIZE0)")) {
        std::fprintf(stderr, "FAIL: %s: DrawPrimitiveUP(XYZ|TEX1) hr=0x%08x\n", c.name, static_cast<unsigned>(hr));
        return false;
      }

      {
        std::lock_guard<std::mutex> lock(dev->mutex);
        if (!Check(dev->vertex_decl == expected_decl, "custom FVF decl preserved at draw (XYZ|TEX1)")) {
          return false;
        }
        const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(FixedFuncVariant::XYZ_TEX1)];
        if (!Check(pipe.vs != nullptr, "fixedfunc XYZ_TEX1 VS created")) {
          return false;
        }
        if (!Check(dev->vs == pipe.vs, "XYZ|TEX1 binds WVP VS")) {
          return false;
        }
        if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsTransformPosWhiteTex1),
                   "XYZ|TEX1 VS bytecode matches kVsTransformPosWhiteTex1")) {
          return false;
        }
        if (!Check(dev->ps != nullptr, "PS bound (XYZ|TEX1)")) {
          return false;
        }
        if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "PS contains texld (XYZ|TEX1)")) {
          return false;
        }
        if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "PS contains mul (XYZ|TEX1)")) {
          return false;
        }
      }
    }
  }

  return true;
}

bool TestFvfXyzDiffuseTex1EmitsTransformConstantsAndDecl() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_decl[] = {
      // stream, offset, type, method, usage, usage_index
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };

  aerogpu_handle_t expected_input_layout = 0;
  aerogpu_handle_t expected_vb = 0;
  bool decl_ok = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzDiffuseTex1);
    if (variant != FixedFuncVariant::NONE) {
      const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
      if (pipe.vertex_decl) {
        expected_input_layout = pipe.vertex_decl->handle;
        const auto& blob = pipe.vertex_decl->blob;
        decl_ok = (blob.size() == sizeof(expected_decl)) &&
                  (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
      }
    }
  }
  // Set a simple world translation; view/projection are identity.
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }
  if (!Check(expected_input_layout != 0, "SetFVF XYZ|DIFFUSE|TEX1 created internal input layout")) {
    return false;
  }
  if (!Check(decl_ok, "XYZ|DIFFUSE|TEX1 internal vertex decl matches expected layout")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const VertexXyzDiffuseTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz diffuse tex1)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzDiffuseTex1);
    if (variant != FixedFuncVariant::NONE) {
      const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
      if (!Check(pipe.vs != nullptr, "fixedfunc VS created (XYZ|DIFFUSE|TEX1)")) {
        return false;
      }
      if (!Check(dev->vs == pipe.vs, "XYZ|DIFFUSE|TEX1 binds fixed-function VS")) {
        return false;
      }
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColorTex0),
               "XYZ|DIFFUSE|TEX1 VS bytecode matches kVsWvpPosColorTex0")) {
      return false;
    }
    if (dev->up_vertex_buffer) {
      expected_vb = dev->up_vertex_buffer->handle;
      if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri),
                 "scratch VB storage contains uploaded vertices (TEX1)")) {
        return false;
      }
      if (!Check(std::memcmp(dev->up_vertex_buffer->storage.data(), tri, sizeof(tri)) == 0,
                 "scratch VB contains original XYZ|DIFFUSE|TEX1 vertices (no CPU conversion)")) {
        return false;
      }
    }
  }
  if (!Check(expected_vb != 0, "scratch VB handle non-zero (XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE|TEX1 WVP VS)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_TEXTURE) >= 1, "SET_TEXTURE emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE) >= 1, "UPLOAD_RESOURCE emitted")) {
    return false;
  }

  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal XYZ|DIFFUSE|TEX1 layout handle")) {
    return false;
  }

  // Validate at least one vertex buffer binding references the scratch UP buffer
  // with the original stride.
  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(reinterpret_cast<const uint8_t*>(svb) +
                                                                                  sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzDiffuseTex1)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds scratch UP buffer (XYZ|DIFFUSE|TEX1 original stride)")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != kFixedfuncMatrixStartRegister || sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  return true;
}

bool TestFvfXyzDiffuseTex1DrawPrimitiveVbUploadsWvpAndBindsVb() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitive != nullptr, "pfnDrawPrimitive is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_decl[] = {
      // stream, offset, type, method, usage, usage_index
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexCoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  // Provide a non-identity transform so the fixed-function WVP constant upload is
  // easy to spot (WVP columns are uploaded into c240..c243).
  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  // Fixed-function emulation for XYZ vertices uses a WVP vertex shader and
  // uploads the matrix into reserved VS constants c240..c243 as column vectors.
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };

  aerogpu_handle_t expected_input_layout = 0;
  bool decl_ok = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzDiffuseTex1);
    if (variant != FixedFuncVariant::NONE) {
      const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
      if (pipe.vertex_decl) {
        expected_input_layout = pipe.vertex_decl->handle;
        const auto& blob = pipe.vertex_decl->blob;
        decl_ok = (blob.size() == sizeof(expected_decl)) &&
                  (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
      }
    }
  }

  // Set a simple world translation; view/projection are identity.
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }

  if (!Check(expected_input_layout != 0, "SetFVF XYZ|DIFFUSE|TEX1 created internal input layout")) {
    return false;
  }
  if (!Check(decl_ok, "XYZ|DIFFUSE|TEX1 internal vertex decl matches expected layout (VB draw)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  // Create a VB (non-UP draw path) with a leading dummy vertex, then draw starting
  // at vertex 1. This exercises `start_vertex` handling in the draw packet.
  const VertexXyzDiffuseTex1 verts[4] = {
      {123.0f, 456.0f, 0.0f, 0xFFFFFFFFu, 9.0f, 9.0f},
      {-1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyz|diffuse|tex1)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock_dev(dev->mutex);
    auto* vb = reinterpret_cast<Resource*>(create_vb.hResource.pDrvPrivate);
    expected_vb = vb ? vb->handle : 0;
  }
  if (!Check(expected_vb != 0, "vb handle non-zero")) {
    return false;
  }

  D3D9DDIARG_LOCK lock{};
  lock.hResource = create_vb.hResource;
  lock.offset_bytes = 0;
  lock.size_bytes = 0;
  lock.flags = 0;
  D3DDDI_LOCKEDBOX box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(vertex buffer xyz|diffuse|tex1)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock returns pData")) {
    return false;
  }
  std::memcpy(box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = create_vb.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(vertex buffer xyz|diffuse|tex1)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzDiffuseTex1));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb xyz|diffuse|tex1)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitive(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*start_vertex=*/1, /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawPrimitive(triangle xyz|diffuse|tex1)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock_dev(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzDiffuseTex1);
    if (!Check(variant == FixedFuncVariant::XYZ_COLOR_TEX1, "fixedfunc_variant_from_fvf(XYZ|DIFFUSE|TEX1) == XYZ_COLOR_TEX1")) {
      return false;
    }
    const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
    if (!Check(pipe.vs != nullptr, "XYZ|DIFFUSE|TEX1 fixed-function VS created")) {
      return false;
    }
    if (!Check(dev->vs == pipe.vs, "XYZ|DIFFUSE|TEX1 binds WVP VS")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColorTex0), "XYZ|DIFFUSE|TEX1 VS bytecode matches kVsWvpPosColorTex0")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer == nullptr, "VB draw does not allocate scratch UP buffer (TEX1)")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE|TEX1 VB draw)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_TEXTURE) >= 1, "SET_TEXTURE emitted")) {
    return false;
  }

  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal XYZ|DIFFUSE|TEX1 layout handle (VB draw)")) {
    return false;
  }

  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(
        reinterpret_cast<const uint8_t*>(svb) + sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzDiffuseTex1)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds the created VB (XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  bool saw_draw = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_DRAW)) {
    const auto* d = reinterpret_cast<const aerogpu_cmd_draw*>(hdr);
    if (d->first_vertex == 1 && d->vertex_count == 3 && d->instance_count == 1) {
      saw_draw = true;
      break;
    }
  }
  if (!Check(saw_draw, "DRAW uses start_vertex=1 vertex_count=3 instance_count=1 (TEX1)")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != kFixedfuncMatrixStartRegister || sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (VB draw TEX1)")) {
    return false;
  }

  return true;
}

bool TestFvfXyzrhwTex1EmitsTextureAndShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|TEX1)")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_decl[] = {
      // stream, offset, type, method, usage, usage_index
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  aerogpu_handle_t expected_input_layout = 0;
  bool decl_ok = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzrhwTex1);
    if (variant != FixedFuncVariant::NONE) {
      const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
      if (pipe.vertex_decl) {
        expected_input_layout = pipe.vertex_decl->handle;
        const auto& blob = pipe.vertex_decl->blob;
        decl_ok = (blob.size() == sizeof(expected_decl)) &&
                  (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
      }
    }
  }
  if (!Check(expected_input_layout != 0, "SetFVF XYZRHW|TEX1 created internal input layout")) {
    return false;
  }
  if (!Check(decl_ok, "XYZRHW|TEX1 internal vertex decl matches expected layout")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const VertexXyzrhwTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyzrhw tex1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZRHW|TEX1)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_TEXTURE) >= 1, "SET_TEXTURE emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) >= 2, "CREATE_SHADER_DXBC emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT) >= 1, "SET_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS) >= 1, "SET_VERTEX_BUFFERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }

  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal XYZRHW|TEX1 layout handle")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs != 0 && last_bind->ps != 0, "BIND_SHADERS binds non-zero VS/PS")) {
    return false;
  }

  return true;
}

bool TestFvfXyzTex1EmitsTransformConstantsAndDecl() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|TEX1)")) {
    return false;
  }

  const D3DVERTEXELEMENT9_COMPAT expected_decl[] = {
      // stream, offset, type, method, usage, usage_index
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };

  aerogpu_handle_t expected_input_layout = 0;
  bool decl_ok = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzTex1);
    if (variant != FixedFuncVariant::NONE) {
      const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
      if (pipe.vertex_decl) {
        expected_input_layout = pipe.vertex_decl->handle;
        const auto& blob = pipe.vertex_decl->blob;
        decl_ok = (blob.size() == sizeof(expected_decl)) &&
                  (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
      }
    }
  }
  // Set a simple world translation; view/projection are identity.
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }
  if (!Check(expected_input_layout != 0, "SetFVF XYZ|TEX1 created internal input layout")) {
    return false;
  }
  if (!Check(decl_ok, "XYZ|TEX1 internal vertex decl matches expected layout")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const VertexXyzTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz tex1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|TEX1)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_TEXTURE) >= 1, "SET_TEXTURE emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F) >= 1, "SET_SHADER_CONSTANTS_F emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) >= 2, "CREATE_SHADER_DXBC emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT) >= 1, "SET_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS) >= 1, "SET_VERTEX_BUFFERS emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }

  bool saw_expected_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == expected_input_layout) {
      saw_expected_layout = true;
      break;
    }
  }
  if (!Check(saw_expected_layout, "SET_INPUT_LAYOUT uses internal XYZ|TEX1 layout handle")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != kFixedfuncMatrixStartRegister || sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns")) {
    return false;
  }

  return true;
}

bool TestFvfXyzTex1DrawPrimitiveVbUploadsWvpAndBindsVb() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitive != nullptr, "pfnDrawPrimitive is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|TEX1)")) {
    return false;
  }

  // Set a non-identity transform so the fixed-function WVP constant upload is
  // easy to spot (WVP columns are uploaded into c240..c243).
  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  // Set a simple world translation; view/projection are identity.
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  // Create a vertex buffer (non-UP path) and populate it via Lock/Unlock.
  const VertexXyzTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;   // Buffer type is inferred from `size` by the UMD.
  create_vb.format = 0u; // Unused for buffers.
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(tri);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* vb = reinterpret_cast<Resource*>(create_vb.hResource.pDrvPrivate);
    expected_vb = vb ? vb->handle : 0;
  }
  if (!Check(expected_vb != 0, "vb handle non-zero")) {
    return false;
  }

  D3D9DDIARG_LOCK lock{};
  lock.hResource = create_vb.hResource;
  lock.offset_bytes = 0;
  lock.size_bytes = 0;
  lock.flags = 0;
  D3DDDI_LOCKEDBOX box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(vertex buffer)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock returns pData")) {
    return false;
  }
  std::memcpy(box.pData, tri, sizeof(tri));

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = create_vb.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(vertex buffer)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitive(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*start_vertex=*/0, /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawPrimitive(triangle xyz tex1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|TEX1 VB draw)")) {
    return false;
  }

  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(
        reinterpret_cast<const uint8_t*>(svb) + sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzTex1)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds the created VB")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != kFixedfuncMatrixStartRegister || sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (VB draw)")) {
    return false;
  }

  return true;
}

bool TestVertexDeclXyzrhwTex1InfersFvfAndBindsShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Create and bind a vertex decl matching XYZRHW|TEX1.
  const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl(XYZRHW|TEX1)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDecl);

  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl(XYZRHW|TEX1)")) {
    return false;
  }

  // Verify implied FVF inference.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fvf == kFvfXyzrhwTex1, "SetVertexDecl inferred FVF == XYZRHW|TEX1")) {
      return false;
    }
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const VertexXyzrhwTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW|TEX1 via decl)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZRHW|TEX1 via decl)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_INPUT_LAYOUT) >= 1, "CREATE_INPUT_LAYOUT emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }

  // Ensure the decl's input layout handle is bound (not an internal FVF decl).
  aerogpu_handle_t decl_handle = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* decl = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
    decl_handle = decl ? decl->handle : 0;
  }
  if (!Check(decl_handle != 0, "vertex decl handle non-zero")) {
    return false;
  }
  bool saw_decl_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == decl_handle) {
      saw_decl_layout = true;
      break;
    }
  }
  if (!Check(saw_decl_layout, "SET_INPUT_LAYOUT binds the explicit decl layout")) {
    return false;
  }

  return true;
}

bool TestVertexDeclXyzTex1InfersFvfAndUploadsWvp() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Create and bind a vertex decl matching XYZ|TEX1.
  const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl(XYZ|TEX1)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDecl);

  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl(XYZ|TEX1)")) {
    return false;
  }

  // Verify implied FVF inference.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fvf == kFvfXyzTex1, "SetVertexDecl inferred FVF == XYZ|TEX1")) {
      return false;
    }
  }

  // Provide a simple transform to ensure the WVP constant upload is observable.
  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  // Set a simple world translation; view/projection are identity.
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const VertexXyzTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|TEX1 via decl)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|TEX1 via decl)")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != kFixedfuncMatrixStartRegister || sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (decl path)")) {
    return false;
  }

  return true;
}

bool TestVertexDeclXyzTex1DrawPrimitiveVbUploadsWvpAndBindsVb() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitive != nullptr, "pfnDrawPrimitive is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Create and bind a vertex decl matching XYZ|TEX1 (no SetFVF call). The driver
  // should infer the implied FVF and use the fixed-function WVP VS path.
  const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl(XYZ|TEX1)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDecl);

  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl(XYZ|TEX1)")) {
    return false;
  }

  aerogpu_handle_t decl_handle = 0;
  VertexDecl* decl_ptr = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fvf == kFvfXyzTex1, "SetVertexDecl inferred FVF == XYZ|TEX1")) {
      return false;
    }
    decl_ptr = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
    decl_handle = decl_ptr ? decl_ptr->handle : 0;
  }
  if (!Check(decl_handle != 0, "explicit XYZ|TEX1 decl handle non-zero")) {
    return false;
  }

  // Set a simple world translation; view/projection are identity.
  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  // Create a VB with a leading dummy vertex so we can draw with start_vertex=1.
  const VertexXyzTex1 verts[4] = {
      {123.0f, 456.0f, 0.0f, 9.0f, 9.0f},
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyz|tex1 via decl)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock_dev(dev->mutex);
    auto* vb = reinterpret_cast<Resource*>(create_vb.hResource.pDrvPrivate);
    expected_vb = vb ? vb->handle : 0;
  }
  if (!Check(expected_vb != 0, "vb handle non-zero (decl xyz|tex1)")) {
    return false;
  }

  D3D9DDIARG_LOCK lock{};
  lock.hResource = create_vb.hResource;
  lock.offset_bytes = 0;
  lock.size_bytes = 0;
  lock.flags = 0;
  D3DDDI_LOCKEDBOX box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(vertex buffer xyz|tex1 via decl)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock returns pData")) {
    return false;
  }
  std::memcpy(box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = create_vb.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(vertex buffer xyz|tex1 via decl)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb xyz|tex1 via decl)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitive(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*start_vertex=*/1, /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawPrimitive(XYZ|TEX1 via decl)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock_dev(dev->mutex);
    // Ensure the draw didn't change the explicitly bound vertex decl.
    if (!Check(dev->vertex_decl == decl_ptr, "vertex decl preserved after XYZ|TEX1 draw")) {
      return false;
    }

    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzTex1);
    if (!Check(variant == FixedFuncVariant::XYZ_TEX1, "fixedfunc_variant_from_fvf(XYZ|TEX1) == XYZ_TEX1 (decl path)")) {
      return false;
    }
    const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
    if (!Check(pipe.vs != nullptr, "XYZ|TEX1 fixed-function VS created (decl path)")) {
      return false;
    }
    if (!Check(dev->vs == pipe.vs, "XYZ|TEX1 via decl binds VS")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsTransformPosWhiteTex1),
               "XYZ|TEX1 via decl VS bytecode matches kVsTransformPosWhiteTex1")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer == nullptr, "VB draw does not allocate scratch UP buffer (decl xyz|tex1)")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|TEX1 VB draw via decl)")) {
    return false;
  }

  bool saw_decl_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == decl_handle) {
      saw_decl_layout = true;
    }
  }
  if (!Check(saw_decl_layout, "SET_INPUT_LAYOUT binds explicit decl (XYZ|TEX1 VB draw)")) {
    return false;
  }

  bool saw_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(
        reinterpret_cast<const uint8_t*>(svb) + sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzTex1)) {
        saw_vb = true;
        break;
      }
    }
    if (saw_vb) {
      break;
    }
  }
  if (!Check(saw_vb, "SET_VERTEX_BUFFERS binds the created VB (decl xyz|tex1)")) {
    return false;
  }

  bool saw_draw = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_DRAW)) {
    const auto* d = reinterpret_cast<const aerogpu_cmd_draw*>(hdr);
    if (d->first_vertex == 1 && d->vertex_count == 3 && d->instance_count == 1) {
      saw_draw = true;
      break;
    }
  }
  if (!Check(saw_draw, "DRAW uses start_vertex=1 vertex_count=3 instance_count=1 (decl xyz|tex1)")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX ||
        sc->start_register != kFixedfuncMatrixStartRegister ||
        sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (decl xyz|tex1 VB draw)")) {
    return false;
  }

  return true;
}

bool TestVertexDeclXyzDiffuseDrawPrimitiveVbUploadsWvpAndKeepsDecl() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitive != nullptr, "pfnDrawPrimitive is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Create and bind a vertex decl matching XYZ|DIFFUSE (no SetFVF call). The
  // driver should infer the implied FVF and bind the fixed-function WVP shader
  // while preserving the application's explicit declaration.
  const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl(XYZ|DIFFUSE)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDecl);

  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl(XYZ|DIFFUSE)")) {
    return false;
  }

  aerogpu_handle_t decl_handle = 0;
  VertexDecl* decl_ptr = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fvf == kFvfXyzDiffuse, "SetVertexDecl inferred FVF == XYZ|DIFFUSE")) {
      return false;
    }
    decl_ptr = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
    decl_handle = decl_ptr ? decl_ptr->handle : 0;
  }
  if (!Check(decl_handle != 0, "explicit XYZ|DIFFUSE decl handle non-zero")) {
    return false;
  }

  // Set a simple world translation; view/projection are identity.
  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  // Fixed-function emulation for XYZ vertices uses a WVP vertex shader and
  // uploads the matrix into reserved VS constants c240..c243 as column vectors.
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }

  // Create a VB with a leading dummy vertex so we can draw with start_vertex=1.
  const VertexXyzDiffuse verts[4] = {
      {123.0f, 456.0f, 0.0f, 0xFFFFFFFFu},
      {-1.0f, -1.0f, 0.0f, 0xFFFF0000u},
      {1.0f, -1.0f, 0.0f, 0xFF00FF00u},
      {-1.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };

  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyz|diffuse via decl)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);
  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock_dev(dev->mutex);
    auto* vb = reinterpret_cast<Resource*>(create_vb.hResource.pDrvPrivate);
    expected_vb = vb ? vb->handle : 0;
  }
  if (!Check(expected_vb != 0, "vb handle non-zero (decl xyz|diffuse)")) {
    return false;
  }

  D3D9DDIARG_LOCK lock{};
  lock.hResource = create_vb.hResource;
  lock.offset_bytes = 0;
  lock.size_bytes = 0;
  lock.flags = 0;
  D3DDDI_LOCKEDBOX box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(vertex buffer xyz|diffuse via decl)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock returns pData")) {
    return false;
  }
  std::memcpy(box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = create_vb.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(vertex buffer xyz|diffuse via decl)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb xyz|diffuse via decl)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitive(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*start_vertex=*/1, /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawPrimitive(XYZ|DIFFUSE via decl)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock_dev(dev->mutex);
    // Ensure the draw didn't change the explicitly bound vertex decl.
    if (!Check(dev->vertex_decl == decl_ptr, "vertex decl preserved after XYZ|DIFFUSE draw")) {
      return false;
    }

    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzDiffuse);
    if (!Check(variant == FixedFuncVariant::XYZ_COLOR, "fixedfunc_variant_from_fvf(XYZ|DIFFUSE) == XYZ_COLOR (decl path)")) {
      return false;
    }
    const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
    if (!Check(pipe.vs != nullptr, "XYZ|DIFFUSE fixed-function VS created (decl path)")) {
      return false;
    }
    if (!Check(dev->vs == pipe.vs, "XYZ|DIFFUSE via decl binds WVP VS")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColor),
               "XYZ|DIFFUSE via decl VS bytecode matches kVsWvpPosColor")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "XYZ|DIFFUSE via decl binds PS")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld),
               "XYZ|DIFFUSE via decl without texture binds PS without texld")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer == nullptr, "VB draw via decl does not allocate scratch UP buffer")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE VB draw via decl)")) {
    return false;
  }

  bool saw_decl_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == decl_handle) {
      saw_decl_layout = true;
      break;
    }
  }
  if (!Check(saw_decl_layout, "SET_INPUT_LAYOUT binds explicit decl (XYZ|DIFFUSE VB draw)")) {
    return false;
  }

  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(
        reinterpret_cast<const uint8_t*>(svb) + sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzDiffuse)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds the created VB (decl xyz|diffuse)")) {
    return false;
  }

  bool saw_draw = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_DRAW)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_draw)) {
      continue;
    }
    const auto* d = reinterpret_cast<const aerogpu_cmd_draw*>(hdr);
    if (d->first_vertex == 1 && d->vertex_count == 3 && d->instance_count == 1) {
      saw_draw = true;
      break;
    }
  }
  if (!Check(saw_draw, "DRAW uses start_vertex=1 vertex_count=3 instance_count=1 (decl xyz|diffuse)")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != kFixedfuncMatrixStartRegister || sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (decl xyz|diffuse VB draw)")) {
    return false;
  }

  return true;
}

bool TestVertexDeclXyzDiffuseTex1DrawPrimitiveVbUploadsWvpAndKeepsDecl() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitive != nullptr, "pfnDrawPrimitive is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Create and bind a vertex decl matching XYZ|DIFFUSE|TEX1 (no SetFVF call). The
  // driver should infer the implied FVF and bind the fixed-function WVP shader
  // while preserving the application's explicit declaration.
  const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeD3dColor, kD3dDeclMethodDefault, kD3dDeclUsageColor, 0},
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl(XYZ|DIFFUSE|TEX1)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDecl);

  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl(XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  aerogpu_handle_t decl_handle = 0;
  VertexDecl* decl_ptr = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fvf == kFvfXyzDiffuseTex1, "SetVertexDecl inferred FVF == XYZ|DIFFUSE|TEX1")) {
      return false;
    }
    decl_ptr = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
    decl_handle = decl_ptr ? decl_ptr->handle : 0;
  }
  if (!Check(decl_handle != 0, "explicit XYZ|DIFFUSE|TEX1 decl handle non-zero")) {
    return false;
  }

  // Set a simple world translation; view/projection are identity.
  constexpr float tx = 2.0f;
  constexpr float ty = 3.0f;
  constexpr float tz = 0.0f;
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, tx,
      0.0f, 1.0f, 0.0f, ty,
      0.0f, 0.0f, 1.0f, tz,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = tx;
  world.m[3][1] = ty;
  world.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  // Create a VB with a leading dummy vertex so we can draw with start_vertex=1.
  const VertexXyzDiffuseTex1 verts[4] = {
      {123.0f, 456.0f, 0.0f, 0xFFFFFFFFu, 9.0f, 9.0f},
      {-1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyz|diffuse|tex1 via decl)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);
  aerogpu_handle_t expected_vb = 0;
  {
    std::lock_guard<std::mutex> lock_dev(dev->mutex);
    auto* vb = reinterpret_cast<Resource*>(create_vb.hResource.pDrvPrivate);
    expected_vb = vb ? vb->handle : 0;
  }
  if (!Check(expected_vb != 0, "vb handle non-zero (decl xyz|diffuse|tex1)")) {
    return false;
  }

  D3D9DDIARG_LOCK lock{};
  lock.hResource = create_vb.hResource;
  lock.offset_bytes = 0;
  lock.size_bytes = 0;
  lock.flags = 0;
  D3DDDI_LOCKEDBOX box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(vertex buffer xyz|diffuse|tex1 via decl)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock returns pData")) {
    return false;
  }
  std::memcpy(box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = create_vb.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(vertex buffer xyz|diffuse|tex1 via decl)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzDiffuseTex1));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb xyz|diffuse|tex1 via decl)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitive(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*start_vertex=*/1, /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawPrimitive(XYZ|DIFFUSE|TEX1 via decl)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock_dev(dev->mutex);
    // Ensure the draw didn't change the explicitly bound vertex decl.
    if (!Check(dev->vertex_decl == decl_ptr, "vertex decl preserved after XYZ|DIFFUSE|TEX1 draw")) {
      return false;
    }

    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzDiffuseTex1);
    if (!Check(variant == FixedFuncVariant::XYZ_COLOR_TEX1, "fixedfunc_variant_from_fvf(XYZ|DIFFUSE|TEX1) == XYZ_COLOR_TEX1 (decl path)")) {
      return false;
    }
    const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
    if (!Check(pipe.vs != nullptr, "XYZ|DIFFUSE|TEX1 fixed-function VS created (decl path)")) {
      return false;
    }
    if (!Check(dev->vs == pipe.vs, "XYZ|DIFFUSE|TEX1 via decl binds WVP VS")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColorTex0),
               "XYZ|DIFFUSE|TEX1 via decl VS bytecode matches kVsWvpPosColorTex0")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "XYZ|DIFFUSE|TEX1 via decl binds PS")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld),
               "XYZ|DIFFUSE|TEX1 via decl binds PS that samples texture (texld)")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer == nullptr, "VB draw via decl does not allocate scratch UP buffer (tex1)")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|DIFFUSE|TEX1 VB draw via decl)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_SET_TEXTURE) >= 1, "SET_TEXTURE emitted")) {
    return false;
  }

  bool saw_decl_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == decl_handle) {
      saw_decl_layout = true;
      break;
    }
  }
  if (!Check(saw_decl_layout, "SET_INPUT_LAYOUT binds explicit decl (XYZ|DIFFUSE|TEX1 VB draw)")) {
    return false;
  }

  bool saw_expected_vb = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_VERTEX_BUFFERS)) {
    const auto* svb = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(hdr);
    if (svb->buffer_count == 0) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_vertex_buffers) +
                        static_cast<size_t>(svb->buffer_count) * sizeof(aerogpu_vertex_buffer_binding);
    if (hdr->size_bytes < need) {
      continue;
    }
    const auto* bindings = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(
        reinterpret_cast<const uint8_t*>(svb) + sizeof(aerogpu_cmd_set_vertex_buffers));
    for (uint32_t i = 0; i < svb->buffer_count; ++i) {
      if (bindings[i].buffer == expected_vb && bindings[i].stride_bytes == sizeof(VertexXyzDiffuseTex1)) {
        saw_expected_vb = true;
        break;
      }
    }
    if (saw_expected_vb) {
      break;
    }
  }
  if (!Check(saw_expected_vb, "SET_VERTEX_BUFFERS binds the created VB (decl xyz|diffuse|tex1)")) {
    return false;
  }

  bool saw_draw = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_DRAW)) {
    if (hdr->size_bytes < sizeof(aerogpu_cmd_draw)) {
      continue;
    }
    const auto* d = reinterpret_cast<const aerogpu_cmd_draw*>(hdr);
    if (d->first_vertex == 1 && d->vertex_count == 3 && d->instance_count == 1) {
      saw_draw = true;
      break;
    }
  }
  if (!Check(saw_draw, "DRAW uses start_vertex=1 vertex_count=3 instance_count=1 (decl xyz|diffuse|tex1)")) {
    return false;
  }

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != kFixedfuncMatrixStartRegister || sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
      saw_wvp_constants = true;
      break;
    }
  }
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (decl xyz|diffuse|tex1 VB draw)")) {
    return false;
  }

  return true;
}

bool TestSetTextureStageStateUpdatesPsForTex1NoDiffuseFvfs() {
  // ---------------------------------------------------------------------------
  // XYZRHW | TEX1
  // ---------------------------------------------------------------------------
  {
    CleanupDevice cleanup;
    if (!CreateDevice(&cleanup)) {
      return false;
    }

    auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
    if (!Check(dev != nullptr, "device pointer")) {
      return false;
    }

    const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
      HRESULT hr2 = S_OK;
      if (cleanup.device_funcs.pfnSetTextureStageState) {
        hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      } else {
        hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
      }
      return Check(hr2 == S_OK, msg);
    };

    dev->cmd.reset();

    HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwTex1);
    if (!Check(hr == S_OK, "SetFVF(XYZRHW|TEX1)")) {
      return false;
    }

    D3DDDI_HRESOURCE hTex{};
    if (!CreateDummyTexture(&cleanup, &hTex)) {
      return false;
    }

    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
    if (!Check(hr == S_OK, "SetTexture(stage0)")) {
      return false;
    }

    // Ensure a known starting point for stage0 state (matches D3D9 defaults).
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopModulate,
                              "XYZRHW|TEX1: SetTextureStageState(COLOROP=MODULATE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg1,
                              kD3dTaTexture,
                              "XYZRHW|TEX1: SetTextureStageState(COLORARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg2,
                              kD3dTaDiffuse,
                              "XYZRHW|TEX1: SetTextureStageState(COLORARG2=DIFFUSE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaOp,
                              kD3dTopSelectArg1,
                              "XYZRHW|TEX1: SetTextureStageState(ALPHAOP=SELECTARG1) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg1,
                              kD3dTaTexture,
                              "XYZRHW|TEX1: SetTextureStageState(ALPHAARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg2,
                              kD3dTaDiffuse,
                              "XYZRHW|TEX1: SetTextureStageState(ALPHAARG2=DIFFUSE) succeeds")) {
      return false;
    }

    const VertexXyzrhwTex1 tri[3] = {
        {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
        {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
        {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
    };

    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyzrhw tex1)")) {
      return false;
    }

    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZRHW|TEX1: PS bound after draw")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "XYZRHW|TEX1: PS contains texld")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "XYZRHW|TEX1: PS contains mul")) {
        return false;
      }
    }

    // Validate SetTexture(stage0) hot-swaps the internal fixed-function PS variant
    // when fixed-function is active (no user shaders bound).
    {
      D3DDDI_HRESOURCE null_tex{};
      hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, null_tex);
      if (!Check(hr == S_OK, "XYZRHW|TEX1: SetTexture(stage0=null) succeeds")) {
        return false;
      }
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZRHW|TEX1: PS still bound after SetTexture(null)")) {
        return false;
      }
      if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "XYZRHW|TEX1: passthrough PS does not contain texld")) {
        return false;
      }
      if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "XYZRHW|TEX1: passthrough PS does not contain mul")) {
        return false;
      }
    }
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
    if (!Check(hr == S_OK, "XYZRHW|TEX1: SetTexture(stage0=texture) succeeds")) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZRHW|TEX1: PS still bound after SetTexture(texture)")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "XYZRHW|TEX1: restored PS contains texld")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "XYZRHW|TEX1: restored PS contains mul")) {
        return false;
      }
    }

    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopDisable,
                              "XYZRHW|TEX1: SetTextureStageState(COLOROP=DISABLE) succeeds")) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZRHW|TEX1: PS still bound after SetTextureStageState")) {
        return false;
      }
      if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "XYZRHW|TEX1: disable PS does not contain texld")) {
        return false;
      }
      if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "XYZRHW|TEX1: disable PS does not contain mul")) {
        return false;
      }
    }
  }

  // ---------------------------------------------------------------------------
  // XYZ | TEX1
  // ---------------------------------------------------------------------------
  {
    CleanupDevice cleanup;
    if (!CreateDevice(&cleanup)) {
      return false;
    }

    auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
    if (!Check(dev != nullptr, "device pointer")) {
      return false;
    }

    const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
      HRESULT hr2 = S_OK;
      if (cleanup.device_funcs.pfnSetTextureStageState) {
        hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      } else {
        hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
      }
      return Check(hr2 == S_OK, msg);
    };

    dev->cmd.reset();

    HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzTex1);
    if (!Check(hr == S_OK, "SetFVF(XYZ|TEX1)")) {
      return false;
    }

    D3DDDI_HRESOURCE hTex{};
    if (!CreateDummyTexture(&cleanup, &hTex)) {
      return false;
    }

    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
    if (!Check(hr == S_OK, "SetTexture(stage0)")) {
      return false;
    }

    // Ensure a known starting point for stage0 state (matches D3D9 defaults).
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopModulate,
                              "XYZ|TEX1: SetTextureStageState(COLOROP=MODULATE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg1,
                              kD3dTaTexture,
                              "XYZ|TEX1: SetTextureStageState(COLORARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg2,
                              kD3dTaDiffuse,
                              "XYZ|TEX1: SetTextureStageState(COLORARG2=DIFFUSE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaOp,
                              kD3dTopSelectArg1,
                              "XYZ|TEX1: SetTextureStageState(ALPHAOP=SELECTARG1) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg1,
                              kD3dTaTexture,
                              "XYZ|TEX1: SetTextureStageState(ALPHAARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg2,
                              kD3dTaDiffuse,
                              "XYZ|TEX1: SetTextureStageState(ALPHAARG2=DIFFUSE) succeeds")) {
      return false;
    }

    const VertexXyzTex1 tri[3] = {
        {0.0f, 0.0f, 0.0f, 0.0f, 0.0f},
        {1.0f, 0.0f, 0.0f, 1.0f, 0.0f},
        {0.0f, 1.0f, 0.0f, 0.0f, 1.0f},
    };

    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz tex1)")) {
      return false;
    }

    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZ|TEX1: PS bound after draw")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "XYZ|TEX1: PS contains texld")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "XYZ|TEX1: PS contains mul")) {
        return false;
      }
    }

    // Validate SetTexture(stage0) hot-swaps the internal fixed-function PS variant
    // when fixed-function is active (no user shaders bound).
    {
      D3DDDI_HRESOURCE null_tex{};
      hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, null_tex);
      if (!Check(hr == S_OK, "XYZ|TEX1: SetTexture(stage0=null) succeeds")) {
        return false;
      }
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZ|TEX1: PS still bound after SetTexture(null)")) {
        return false;
      }
      if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "XYZ|TEX1: passthrough PS does not contain texld")) {
        return false;
      }
      if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "XYZ|TEX1: passthrough PS does not contain mul")) {
        return false;
      }
    }
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
    if (!Check(hr == S_OK, "XYZ|TEX1: SetTexture(stage0=texture) succeeds")) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZ|TEX1: PS still bound after SetTexture(texture)")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "XYZ|TEX1: restored PS contains texld")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "XYZ|TEX1: restored PS contains mul")) {
        return false;
      }
    }

    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopDisable,
                              "XYZ|TEX1: SetTextureStageState(COLOROP=DISABLE) succeeds")) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZ|TEX1: PS still bound after SetTextureStageState")) {
        return false;
      }
      if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "XYZ|TEX1: disable PS does not contain texld")) {
        return false;
      }
      if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "XYZ|TEX1: disable PS does not contain mul")) {
        return false;
      }
    }
  }

  return true;
}

bool TestSetTextureStageStateUpdatesPsForLitTex1Fvfs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  dev->cmd.reset();

  // SetFVF should ignore garbage D3DFVF_TEXCOORDSIZE bits for unused texcoord sets.
  const uint32_t fvf = kFvfXyzNormalDiffuseTex1 | kD3dFvfTexCoordSize3_1;
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, fvf);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE|TEX1 + garbage TEXCOORDSIZE bits)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  // Ensure a known starting point for stage0 state (matches D3D9 defaults).
  if (!SetTextureStageState(/*stage=*/0,
                            kD3dTssColorOp,
                            kD3dTopModulate,
                            "XYZ|NORMAL|DIFFUSE|TEX1: SetTextureStageState(COLOROP=MODULATE) succeeds")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0,
                            kD3dTssColorArg1,
                            kD3dTaTexture,
                            "XYZ|NORMAL|DIFFUSE|TEX1: SetTextureStageState(COLORARG1=TEXTURE) succeeds")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0,
                            kD3dTssColorArg2,
                            kD3dTaDiffuse,
                            "XYZ|NORMAL|DIFFUSE|TEX1: SetTextureStageState(COLORARG2=DIFFUSE) succeeds")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0,
                            kD3dTssAlphaOp,
                            kD3dTopSelectArg1,
                            "XYZ|NORMAL|DIFFUSE|TEX1: SetTextureStageState(ALPHAOP=SELECTARG1) succeeds")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0,
                            kD3dTssAlphaArg1,
                            kD3dTaTexture,
                            "XYZ|NORMAL|DIFFUSE|TEX1: SetTextureStageState(ALPHAARG1=TEXTURE) succeeds")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0,
                            kD3dTssAlphaArg2,
                            kD3dTaDiffuse,
                            "XYZ|NORMAL|DIFFUSE|TEX1: SetTextureStageState(ALPHAARG2=DIFFUSE) succeeds")) {
    return false;
  }

  const VertexXyzNormalDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(triangle xyz normal diffuse tex1)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "XYZ|NORMAL|DIFFUSE|TEX1: PS bound after draw")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "XYZ|NORMAL|DIFFUSE|TEX1: PS contains texld")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "XYZ|NORMAL|DIFFUSE|TEX1: PS contains mul")) {
      return false;
    }
  }

  // Validate SetTexture(stage0) hot-swaps the internal fixed-function PS variant
  // when fixed-function is active (no user shaders bound).
  {
    D3DDDI_HRESOURCE null_tex{};
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, null_tex);
    if (!Check(hr == S_OK, "XYZ|NORMAL|DIFFUSE|TEX1: SetTexture(stage0=null) succeeds")) {
      return false;
    }
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "XYZ|NORMAL|DIFFUSE|TEX1: PS still bound after SetTexture(null)")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "XYZ|NORMAL|DIFFUSE|TEX1: passthrough PS does not contain texld")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "XYZ|NORMAL|DIFFUSE|TEX1: passthrough PS does not contain mul")) {
      return false;
    }
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "XYZ|NORMAL|DIFFUSE|TEX1: SetTexture(stage0=texture) succeeds")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "XYZ|NORMAL|DIFFUSE|TEX1: PS still bound after SetTexture(texture)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "XYZ|NORMAL|DIFFUSE|TEX1: restored PS contains texld")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "XYZ|NORMAL|DIFFUSE|TEX1: restored PS contains mul")) {
      return false;
    }
  }

  if (!SetTextureStageState(/*stage=*/0,
                            kD3dTssColorOp,
                            kD3dTopDisable,
                            "XYZ|NORMAL|DIFFUSE|TEX1: SetTextureStageState(COLOROP=DISABLE) succeeds")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "XYZ|NORMAL|DIFFUSE|TEX1: PS still bound after SetTextureStageState")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "XYZ|NORMAL|DIFFUSE|TEX1: disable PS does not contain texld")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "XYZ|NORMAL|DIFFUSE|TEX1: disable PS does not contain mul")) {
      return false;
    }
  }

  return true;
}

bool TestPsOnlyInteropXyzrhwTex1SynthesizesVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|TEX1)")) {
    return false;
  }

  // Bind only a user pixel shader (VS stays NULL). D3D9 expects the runtime to
  // interop fixed-function on the missing stage.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  const VertexXyzrhwTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop XYZRHW|TEX1)")) {
    return false;
  }

  aerogpu_handle_t expected_vs = 0;
  aerogpu_handle_t expected_ps = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* user_ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
    if (!Check(user_ps != nullptr, "user PS pointer")) {
      return false;
    }
    expected_ps = user_ps->handle;

    if (!Check(dev->user_vs == nullptr, "PS-only interop: user_vs is NULL")) {
      return false;
    }
    if (!Check(dev->user_ps == user_ps, "PS-only interop: user_ps is bound")) {
      return false;
    }

    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzrhwTex1);
    if (!Check(variant != FixedFuncVariant::NONE, "fixed-function variant recognized")) {
      return false;
    }
    const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
    if (!Check(pipe.vs != nullptr, "interop created fixed-function VS (XYZRHW|TEX1)")) {
      return false;
    }
    if (!Check(dev->vs == pipe.vs, "interop bound fixed-function VS (XYZRHW|TEX1)")) {
      return false;
    }
    if (!Check(dev->ps == user_ps, "interop kept user PS bound")) {
      return false;
    }
    expected_vs = dev->vs ? dev->vs->handle : 0;
    if (!Check(expected_vs != 0, "synthesized VS handle non-zero")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsPassthroughPosWhiteTex1),
               "synthesized VS bytecode matches kVsPassthroughPosWhiteTex1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop XYZRHW|TEX1)")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs == expected_vs, "BIND_SHADERS uses synthesized VS handle")) {
    return false;
  }
  if (!Check(last_bind->ps == expected_ps, "BIND_SHADERS uses user PS handle")) {
    return false;
  }

  return true;
}

bool TestPsOnlyInteropXyzTex1SynthesizesVsAndUploadsWvp() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|TEX1)")) {
    return false;
  }

  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  const VertexXyzTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop XYZ|TEX1)")) {
    return false;
  }

  aerogpu_handle_t expected_vs = 0;
  aerogpu_handle_t expected_ps = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* user_ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
    if (!Check(user_ps != nullptr, "user PS pointer")) {
      return false;
    }
    expected_ps = user_ps->handle;

    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzTex1);
    if (!Check(variant != FixedFuncVariant::NONE, "fixed-function variant recognized")) {
      return false;
    }
    const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
    if (!Check(pipe.vs != nullptr, "interop created fixed-function VS (XYZ|TEX1)")) {
      return false;
    }
    if (!Check(dev->vs == pipe.vs, "interop bound fixed-function VS (XYZ|TEX1)")) {
      return false;
    }
    if (!Check(dev->ps == user_ps, "interop kept user PS bound")) {
      return false;
    }
    expected_vs = dev->vs ? dev->vs->handle : 0;
    if (!Check(expected_vs != 0, "synthesized VS handle non-zero")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsTransformPosWhiteTex1),
               "synthesized VS bytecode matches kVsTransformPosWhiteTex1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop XYZ|TEX1)")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs == expected_vs, "BIND_SHADERS uses synthesized VS handle")) {
    return false;
  }
  if (!Check(last_bind->ps == expected_ps, "BIND_SHADERS uses user PS handle")) {
    return false;
  }

  // The synthesized fixed-function VS for `XYZ | TEX1` requires a WVP upload
  // (reserved register range c240..c243).
  bool saw_wvp = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage == AEROGPU_SHADER_STAGE_VERTEX &&
        sc->start_register == kFixedfuncMatrixStartRegister &&
        sc->vec4_count == kFixedfuncMatrixVec4Count) {
      saw_wvp = true;
      break;
    }
  }
  if (!Check(saw_wvp, "PS-only interop uploaded WVP constants")) {
    return false;
  }
  return true;
}

bool TestPsOnlyInteropXyzTex1FogEnabledDoesNotSelectFogVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;     // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;      // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;  // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;      // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;        // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;        // D3DFOG_LINEAR

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|TEX1)")) {
    return false;
  }

  // Bind only a user pixel shader (VS stays NULL). Fog must not change the
  // synthesized fixed-function VS output layout in PS-only interop mode.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(0.2f));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(0.8f));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  const VertexXyzTex1 tri[3] = {
      {-1.0f, -1.0f, 0.25f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.25f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.25f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop XYZ|TEX1; fog enabled)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* user_ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
    if (!Check(user_ps != nullptr, "user PS pointer")) {
      return false;
    }
    if (!Check(dev->user_vs == nullptr, "PS-only interop: user_vs is NULL")) {
      return false;
    }
    if (!Check(dev->user_ps == user_ps, "PS-only interop: user_ps is bound")) {
      return false;
    }
    if (!Check(dev->vs != nullptr, "PS-only interop: synthesized VS is bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsTransformPosWhiteTex1),
               "PS-only interop: fog does not select fog VS variant")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop fog enabled)")) {
    return false;
  }

  // PS-only interop must not upload fixed-function fog constants (reserved PS
  // registers c1..c2) since the user PS is bound.
  size_t fog_const_uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage == AEROGPU_SHADER_STAGE_PIXEL && sc->start_register == 1 && sc->vec4_count == 2) {
      ++fog_const_uploads;
    }
  }
  if (!Check(fog_const_uploads == 0, "PS-only interop: does not upload fog PS constants")) {
    return false;
  }

  return true;
}

bool TestPsOnlyInteropXyzNormalIgnoresLightingAndDoesNotUploadLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormal);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL)")) {
    return false;
  }

  // Bind only a user pixel shader (VS stays NULL). The bring-up behavior
  // intentionally ignores fixed-function lighting under shader-stage interop to
  // avoid clobbering user VS constants with the large c208..c236 lighting block.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  const VertexXyzNormal tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormal));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop XYZ|NORMAL; lighting=on)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "PS-only interop: synthesized VS is bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalWhite),
               "PS-only interop: lighting ignored and unlit VS selected")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop XYZ|NORMAL)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "PS-only interop: DRAW emitted")) {
    return false;
  }

  // The synthesized fixed-function VS for `XYZ | NORMAL` uses the WVP constant
  // range (c240..c243) even when lighting is enabled but ignored under interop.
  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count) >= 1,
             "PS-only interop: uploaded WVP constants")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 0,
             "PS-only interop: does not upload fixed-function lighting constants")) {
    return false;
  }

  return true;
}

bool TestPsOnlyInteropXyzNormalTex1IgnoresLightingAndDoesNotUploadLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|TEX1)")) {
    return false;
  }

  // Bind only a user pixel shader (VS stays NULL). The bring-up behavior
  // intentionally ignores fixed-function lighting under shader-stage interop to
  // avoid clobbering user VS constants with the large c208..c236 lighting block.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  const VertexXyzNormalTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, /*u=*/0.0f, /*v=*/0.0f},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, /*u=*/1.0f, /*v=*/0.0f},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, /*u=*/0.0f, /*v=*/1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop XYZ|NORMAL|TEX1; lighting=on)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "PS-only interop: synthesized VS is bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalWhiteTex0),
               "PS-only interop: lighting ignored and unlit TEX1 VS selected")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop XYZ|NORMAL|TEX1)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "PS-only interop: DRAW emitted")) {
    return false;
  }

  // The synthesized fixed-function VS for `XYZ | NORMAL | TEX1` uses the WVP
  // constant range (c240..c243) even when lighting is enabled but ignored under
  // interop.
  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count) >= 1,
             "PS-only interop: uploaded WVP constants")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count) == 0,
             "PS-only interop: does not upload fixed-function lighting constants")) {
    return false;
  }

  return true;
}

bool TestPsOnlyInteropXyzNormalDiffuseIgnoresLightingAndDoesNotUploadLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }

  // Bind only a user pixel shader (VS stays NULL). The bring-up behavior
  // intentionally ignores fixed-function lighting under shader-stage interop to
  // avoid clobbering user VS constants with the large c208..c236 lighting block.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop XYZ|NORMAL|DIFFUSE; lighting=on)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "PS-only interop: synthesized VS is bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalDiffuse),
               "PS-only interop: lighting ignored and unlit normal+diffuse VS selected")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "PS-only interop: DRAW emitted")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count) >= 1,
             "PS-only interop: uploaded WVP constants")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count) == 0,
             "PS-only interop: does not upload fixed-function lighting constants")) {
    return false;
  }

  return true;
}

bool TestPsOnlyInteropXyzNormalDiffuseTex1IgnoresLightingAndDoesNotUploadLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE|TEX1)")) {
    return false;
  }

  // Bind only a user pixel shader (VS stays NULL). The bring-up behavior
  // intentionally ignores fixed-function lighting under shader-stage interop to
  // avoid clobbering user VS constants with the large c208..c236 lighting block.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu, /*u=*/0.0f, /*v=*/0.0f},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu, /*u=*/1.0f, /*v=*/0.0f},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu, /*u=*/0.0f, /*v=*/1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop XYZ|NORMAL|DIFFUSE|TEX1; lighting=on)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "PS-only interop: synthesized VS is bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalDiffuseTex1),
               "PS-only interop: lighting ignored and unlit normal+diffuse+tex1 VS selected")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop XYZ|NORMAL|DIFFUSE|TEX1)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "PS-only interop: DRAW emitted")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count) >= 1,
             "PS-only interop: uploaded WVP constants")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count) == 0,
             "PS-only interop: does not upload fixed-function lighting constants")) {
    return false;
  }

  return true;
}

bool TestPsOnlyInteropVertexDeclXyzrhwTex1SynthesizesVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Bind an explicit vertex decl matching XYZRHW|TEX1 (no SetFVF call). The driver
  // should infer the implied FVF and still be able to synthesize the fixed-function
  // VS when only a pixel shader is bound.
  const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
      {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl(XYZRHW|TEX1)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDecl);

  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl(XYZRHW|TEX1)")) {
    return false;
  }

  aerogpu_handle_t decl_handle = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fvf == kFvfXyzrhwTex1, "SetVertexDecl inferred FVF == XYZRHW|TEX1")) {
      return false;
    }
    auto* decl = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
    decl_handle = decl ? decl->handle : 0;
  }
  if (!Check(decl_handle != 0, "explicit decl handle non-zero")) {
    return false;
  }

  // Bind only a user pixel shader.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  const VertexXyzrhwTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop via decl XYZRHW|TEX1)")) {
    return false;
  }

  aerogpu_handle_t expected_vs = 0;
  aerogpu_handle_t expected_ps = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* user_ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
    if (!Check(user_ps != nullptr, "user PS pointer")) {
      return false;
    }
    expected_ps = user_ps->handle;

    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzrhwTex1);
    if (variant != FixedFuncVariant::NONE) {
      const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
      if (!Check(pipe.vs != nullptr, "interop created fixedfunc VS (XYZRHW|TEX1)")) {
        return false;
      }
      if (!Check(dev->vs == pipe.vs, "interop bound fixedfunc VS (XYZRHW|TEX1)")) {
        return false;
      }
    }
    if (!Check(dev->ps == user_ps, "interop kept user PS bound")) {
      return false;
    }
    expected_vs = dev->vs ? dev->vs->handle : 0;
    if (!Check(expected_vs != 0, "synthesized VS handle non-zero")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsPassthroughPosWhiteTex1),
               "synthesized VS bytecode matches kVsPassthroughPosWhiteTex1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop via decl XYZRHW|TEX1)")) {
    return false;
  }

  // Explicit vertex decl input layout must remain bound (no SetFVF internal decl).
  bool saw_decl_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == decl_handle) {
      saw_decl_layout = true;
      break;
    }
  }
  if (!Check(saw_decl_layout, "SET_INPUT_LAYOUT binds the explicit decl layout")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs == expected_vs, "BIND_SHADERS uses synthesized VS handle")) {
    return false;
  }
  if (!Check(last_bind->ps == expected_ps, "BIND_SHADERS uses user PS handle")) {
    return false;
  }

  return true;
}

bool TestPsOnlyInteropVertexDeclXyzTex1SynthesizesVsAndUploadsWvp() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Bind an explicit vertex decl matching XYZ|TEX1 (no SetFVF call). The driver
  // should infer the implied FVF and still be able to synthesize the WVP fixed-function
  // VS when only a pixel shader is bound.
  const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
      {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
      {0, 12, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
  };

  D3D9DDI_HVERTEXDECL hDecl{};
  HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
      cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl(XYZ|TEX1)")) {
    return false;
  }
  cleanup.vertex_decls.push_back(hDecl);

  hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl(XYZ|TEX1)")) {
    return false;
  }

  aerogpu_handle_t decl_handle = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fvf == kFvfXyzTex1, "SetVertexDecl inferred FVF == XYZ|TEX1")) {
      return false;
    }
    auto* decl = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
    decl_handle = decl ? decl->handle : 0;
  }
  if (!Check(decl_handle != 0, "explicit decl handle non-zero")) {
    return false;
  }

  // Bind only a user pixel shader.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            fixedfunc::kPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS passthrough)")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS passthrough)")) {
    return false;
  }

  const VertexXyzTex1 tri[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only interop via decl XYZ|TEX1)")) {
    return false;
  }

  aerogpu_handle_t expected_vs = 0;
  aerogpu_handle_t expected_ps = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto* user_ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
    if (!Check(user_ps != nullptr, "user PS pointer")) {
      return false;
    }
    expected_ps = user_ps->handle;

    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(kFvfXyzTex1);
    if (variant != FixedFuncVariant::NONE) {
      const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
      if (!Check(pipe.vs != nullptr, "interop created fixedfunc VS (XYZ|TEX1)")) {
        return false;
      }
      if (!Check(dev->vs == pipe.vs, "interop bound fixedfunc VS (XYZ|TEX1)")) {
        return false;
      }
    }
    if (!Check(dev->ps == user_ps, "interop kept user PS bound")) {
      return false;
    }
    expected_vs = dev->vs ? dev->vs->handle : 0;
    if (!Check(expected_vs != 0, "synthesized VS handle non-zero")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsTransformPosWhiteTex1),
               "synthesized VS bytecode matches kVsTransformPosWhiteTex1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only interop via decl XYZ|TEX1)")) {
    return false;
  }

  // Explicit vertex decl input layout must remain bound (no SetFVF internal decl).
  bool saw_decl_layout = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT)) {
    const auto* il = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(hdr);
    if (il->input_layout_handle == decl_handle) {
      saw_decl_layout = true;
      break;
    }
  }
  if (!Check(saw_decl_layout, "SET_INPUT_LAYOUT binds the explicit decl layout")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs == expected_vs, "BIND_SHADERS uses synthesized VS handle")) {
    return false;
  }
  if (!Check(last_bind->ps == expected_ps, "BIND_SHADERS uses user PS handle")) {
    return false;
  }

  // Expect a WVP upload for the fixed-function VS (identity columns by default).
  constexpr float kIdentityCols[16] = {
      1.0f, 0.0f, 0.0f, 0.0f,
      0.0f, 1.0f, 0.0f, 0.0f,
      0.0f, 0.0f, 1.0f, 0.0f,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  bool saw_identity_wvp = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX ||
        sc->start_register != kFixedfuncMatrixStartRegister ||
        sc->vec4_count != kFixedfuncMatrixVec4Count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(kIdentityCols);
    if (hdr->size_bytes < need) {
      continue;
    }
    const float* payload = reinterpret_cast<const float*>(
        reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
    if (std::memcmp(payload, kIdentityCols, sizeof(kIdentityCols)) == 0) {
      saw_identity_wvp = true;
      break;
    }
  }
  if (!Check(saw_identity_wvp, "PS-only interop (decl XYZ|TEX1) uploaded identity WVP constants")) {
    return false;
  }

  return true;
}

bool TestSetTextureStageStateUpdatesPsForTex1NoDiffuseVertexDeclFvfs() {
  // ---------------------------------------------------------------------------
  // XYZRHW | TEX1 via SetVertexDecl (implied FVF)
  // ---------------------------------------------------------------------------
  {
    CleanupDevice cleanup;
    if (!CreateDevice(&cleanup)) {
      return false;
    }

    auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
    if (!Check(dev != nullptr, "device pointer")) {
      return false;
    }

    dev->cmd.reset();

    const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
      HRESULT hr2 = S_OK;
      if (cleanup.device_funcs.pfnSetTextureStageState) {
        hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      } else {
        hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
      }
      return Check(hr2 == S_OK, msg);
    };

    const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
        {0, 0, kD3dDeclTypeFloat4, kD3dDeclMethodDefault, kD3dDeclUsagePositionT, 0},
        {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
        {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
    };

    D3D9DDI_HVERTEXDECL hDecl{};
    HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
        cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
    if (!Check(hr == S_OK, "CreateVertexDecl(XYZRHW|TEX1)")) {
      return false;
    }
    cleanup.vertex_decls.push_back(hDecl);

    hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
    if (!Check(hr == S_OK, "SetVertexDecl(XYZRHW|TEX1)")) {
      return false;
    }

    aerogpu_handle_t decl_handle = 0;
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->fvf == kFvfXyzrhwTex1, "SetVertexDecl inferred FVF == XYZRHW|TEX1")) {
        return false;
      }
      auto* decl = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
      if (!Check(decl != nullptr, "vertex decl pointer")) {
        return false;
      }
      decl_handle = decl->handle;
    }
    if (!Check(decl_handle != 0, "explicit decl handle non-zero")) {
      return false;
    }

    // Ensure a known starting point for stage0 state (matches D3D9 defaults).
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopModulate,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(COLOROP=MODULATE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg1,
                              kD3dTaTexture,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(COLORARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg2,
                              kD3dTaDiffuse,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(COLORARG2=DIFFUSE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaOp,
                              kD3dTopSelectArg1,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(ALPHAOP=SELECTARG1) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg1,
                              kD3dTaTexture,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(ALPHAARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg2,
                              kD3dTaDiffuse,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(ALPHAARG2=DIFFUSE) succeeds")) {
      return false;
    }

    D3DDDI_HRESOURCE hTex{};
    if (!CreateDummyTexture(&cleanup, &hTex)) {
      return false;
    }
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
    if (!Check(hr == S_OK, "SetTexture(stage0)")) {
      return false;
    }

    const VertexXyzrhwTex1 tri[3] = {
        {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
        {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
        {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
    };
    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW|TEX1 via decl)")) {
      return false;
    }

    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZRHW|TEX1 via decl: PS bound after draw")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "XYZRHW|TEX1 via decl: PS contains texld")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "XYZRHW|TEX1 via decl: PS contains mul")) {
        return false;
      }
    }

    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopDisable,
                              "XYZRHW|TEX1 via decl: SetTextureStageState(COLOROP=DISABLE) succeeds")) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZRHW|TEX1 via decl: PS still bound after SetTextureStageState")) {
        return false;
      }
      if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "XYZRHW|TEX1 via decl: disable PS does not contain texld")) {
        return false;
      }
      if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "XYZRHW|TEX1 via decl: disable PS does not contain mul")) {
        return false;
      }
    }

    dev->cmd.finalize();
    const uint8_t* buf = dev->cmd.data();
    const size_t len = dev->cmd.bytes_used();
    if (!Check(ValidateStream(buf, len), "ValidateStream(XYZRHW|TEX1 via decl stage-state update)")) {
      return false;
    }
    // Ensure we never rebound an internal SetFVF decl: the explicit decl handle must
    // remain the active input layout.
    const auto layouts = CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT);
    if (!Check(!layouts.empty(), "SET_INPUT_LAYOUT packets collected")) {
      return false;
    }
    const auto* last_layout = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(layouts.back());
    if (!Check(last_layout->input_layout_handle == decl_handle,
               "XYZRHW|TEX1 via decl: SET_INPUT_LAYOUT uses explicit decl handle")) {
      return false;
    }
  }

  // ---------------------------------------------------------------------------
  // XYZ | TEX1 via SetVertexDecl (implied FVF)
  // ---------------------------------------------------------------------------
  {
    CleanupDevice cleanup;
    if (!CreateDevice(&cleanup)) {
      return false;
    }

    auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
    if (!Check(dev != nullptr, "device pointer")) {
      return false;
    }

    dev->cmd.reset();

    const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
      HRESULT hr2 = S_OK;
      if (cleanup.device_funcs.pfnSetTextureStageState) {
        hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      } else {
        hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
      }
      return Check(hr2 == S_OK, msg);
    };

    const D3DVERTEXELEMENT9_COMPAT decl_blob[] = {
        {0, 0, kD3dDeclTypeFloat3, kD3dDeclMethodDefault, kD3dDeclUsagePosition, 0},
        {0, 12, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
        {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0},
    };

    D3D9DDI_HVERTEXDECL hDecl{};
    HRESULT hr = cleanup.device_funcs.pfnCreateVertexDecl(
        cleanup.hDevice, decl_blob, static_cast<uint32_t>(sizeof(decl_blob)), &hDecl);
    if (!Check(hr == S_OK, "CreateVertexDecl(XYZ|TEX1)")) {
      return false;
    }
    cleanup.vertex_decls.push_back(hDecl);

    hr = cleanup.device_funcs.pfnSetVertexDecl(cleanup.hDevice, hDecl);
    if (!Check(hr == S_OK, "SetVertexDecl(XYZ|TEX1)")) {
      return false;
    }

    aerogpu_handle_t decl_handle = 0;
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->fvf == kFvfXyzTex1, "SetVertexDecl inferred FVF == XYZ|TEX1")) {
        return false;
      }
      auto* decl = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);
      if (!Check(decl != nullptr, "vertex decl pointer")) {
        return false;
      }
      decl_handle = decl->handle;
    }
    if (!Check(decl_handle != 0, "explicit decl handle non-zero")) {
      return false;
    }

    // Ensure a known starting point for stage0 state (matches D3D9 defaults).
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopModulate,
                              "XYZ|TEX1 via decl: SetTextureStageState(COLOROP=MODULATE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg1,
                              kD3dTaTexture,
                              "XYZ|TEX1 via decl: SetTextureStageState(COLORARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorArg2,
                              kD3dTaDiffuse,
                              "XYZ|TEX1 via decl: SetTextureStageState(COLORARG2=DIFFUSE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaOp,
                              kD3dTopSelectArg1,
                              "XYZ|TEX1 via decl: SetTextureStageState(ALPHAOP=SELECTARG1) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg1,
                              kD3dTaTexture,
                              "XYZ|TEX1 via decl: SetTextureStageState(ALPHAARG1=TEXTURE) succeeds")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssAlphaArg2,
                              kD3dTaDiffuse,
                              "XYZ|TEX1 via decl: SetTextureStageState(ALPHAARG2=DIFFUSE) succeeds")) {
      return false;
    }

    D3DDDI_HRESOURCE hTex{};
    if (!CreateDummyTexture(&cleanup, &hTex)) {
      return false;
    }
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
    if (!Check(hr == S_OK, "SetTexture(stage0)")) {
      return false;
    }

    const VertexXyzTex1 tri[3] = {
        {0.0f, 0.0f, 0.0f, 0.0f, 0.0f},
        {1.0f, 0.0f, 0.0f, 1.0f, 0.0f},
        {0.0f, 1.0f, 0.0f, 0.0f, 1.0f},
    };
    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|TEX1 via decl)")) {
      return false;
    }

    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZ|TEX1 via decl: PS bound after draw")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "XYZ|TEX1 via decl: PS contains texld")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "XYZ|TEX1 via decl: PS contains mul")) {
        return false;
      }
    }

    if (!SetTextureStageState(/*stage=*/0,
                              kD3dTssColorOp,
                              kD3dTopDisable,
                              "XYZ|TEX1 via decl: SetTextureStageState(COLOROP=DISABLE) succeeds")) {
      return false;
    }
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "XYZ|TEX1 via decl: PS still bound after SetTextureStageState")) {
        return false;
      }
      if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "XYZ|TEX1 via decl: disable PS does not contain texld")) {
        return false;
      }
      if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "XYZ|TEX1 via decl: disable PS does not contain mul")) {
        return false;
      }
    }

    dev->cmd.finalize();
    const uint8_t* buf = dev->cmd.data();
    const size_t len = dev->cmd.bytes_used();
    if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|TEX1 via decl stage-state update)")) {
      return false;
    }
    const auto layouts = CollectOpcodes(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT);
    if (!Check(!layouts.empty(), "SET_INPUT_LAYOUT packets collected")) {
      return false;
    }
    const auto* last_layout = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(layouts.back());
    if (!Check(last_layout->input_layout_handle == decl_handle,
               "XYZ|TEX1 via decl: SET_INPUT_LAYOUT uses explicit decl handle")) {
      return false;
    }
  }

  return true;
}

bool TestGetTextureStageStateRoundTrips() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnGetTextureStageState != nullptr, "pfnGetTextureStageState is available")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value) -> HRESULT {
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      return cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    }
    return aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
  };

  uint32_t value = 0;
  HRESULT hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, &value);
  if (!Check(hr == S_OK, "GetTextureStageState(stage0 COLOROP)")) {
    return false;
  }
  if (!Check(value == kD3dTopModulate, "Default stage0 COLOROP=MODULATE")) {
    return false;
  }

  value = 0;
  hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/0, kD3dTssAlphaOp, &value);
  if (!Check(hr == S_OK, "GetTextureStageState(stage0 ALPHAOP)")) {
    return false;
  }
  if (!Check(value == kD3dTopSelectArg1, "Default stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }

  value = 0;
  hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/1, kD3dTssColorOp, &value);
  if (!Check(hr == S_OK, "GetTextureStageState(stage1 COLOROP)")) {
    return false;
  }
  if (!Check(value == kD3dTopDisable, "Default stage1 COLOROP=DISABLE")) {
    return false;
  }

  // Set + get should round-trip.
  hr = SetTextureStageState(/*stage=*/0, kD3dTssAlphaOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(stage0 ALPHAOP=DISABLE)")) {
    return false;
  }
  value = 0;
  hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/0, kD3dTssAlphaOp, &value);
  if (!Check(hr == S_OK, "GetTextureStageState(stage0 ALPHAOP) after set")) {
    return false;
  }
  if (!Check(value == kD3dTopDisable, "stage0 ALPHAOP round-trips")) {
    return false;
  }

  // Validate invalid parameters: stage out of range.
  hr = SetTextureStageState(/*stage=*/16, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == D3DERR_INVALIDCALL, "SetTextureStageState(stage=16) returns INVALIDCALL")) {
    return false;
  }
  value = 0xDEADBEEFu;
  hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/16, kD3dTssColorOp, &value);
  if (!Check(hr == D3DERR_INVALIDCALL, "GetTextureStageState(stage=16) returns INVALIDCALL")) {
    return false;
  }
  if (!Check(value == 0u, "GetTextureStageState(stage=16) zeroes output")) {
    return false;
  }

  // Validate invalid parameters: state out of range.
  hr = SetTextureStageState(/*stage=*/0, /*state=*/256, kD3dTopDisable);
  if (!Check(hr == D3DERR_INVALIDCALL, "SetTextureStageState(state=256) returns INVALIDCALL")) {
    return false;
  }
  value = 0xDEADBEEFu;
  hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/0, /*state=*/256, &value);
  if (!Check(hr == D3DERR_INVALIDCALL, "GetTextureStageState(state=256) returns INVALIDCALL")) {
    return false;
  }
  if (!Check(value == 0u, "GetTextureStageState(state=256) zeroes output")) {
    return false;
  }

  // Validate invalid parameters: null output pointer.
  hr = cleanup.device_funcs.pfnGetTextureStageState(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, nullptr);
  if (!Check(hr == E_INVALIDARG, "GetTextureStageState(pValue=null) returns E_INVALIDARG")) {
    return false;
  }

  return true;
}

bool TestStageStateChangeRebindsShadersIfImplemented() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      const HRESULT hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      return Check(hr2 == S_OK, msg);
    }
    // Fallback for minimal portable builds that don't expose SetTextureStageState.
    const HRESULT hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    return Check(hr2 == S_OK, msg);
  };

  // Ensure a known starting point for stage0 state (matches D3D9 defaults).
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "SetTextureStageState(COLOROP=MODULATE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "SetTextureStageState(COLORARG1=TEXTURE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "SetTextureStageState(COLORARG2=DIFFUSE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg2, kD3dTaDiffuse, "SetTextureStageState(ALPHAARG2=DIFFUSE)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  const auto DrawTri = [&](const char* tag) -> bool {
    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
    return Check(hr == S_OK, tag);
  };

  const auto ExpectFixedfuncPsTokens = [&](const char* tag, bool expect_texld, bool expect_mul) -> bool {
    std::lock_guard<std::mutex> lock(dev->mutex);
    const FixedFuncVariant variant = fixedfunc_variant_from_fvf(dev->fvf);
    if (!Check(variant != FixedFuncVariant::NONE, "fixed-function variant recognized")) {
      return false;
    }
    const auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(variant)];
    if (!Check(pipe.ps != nullptr, "fixed-function PS present")) {
      return false;
    }
    if (!Check(dev->ps == pipe.ps, "fixed-function PS is bound")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld) == expect_texld, "PS texld token expectation")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpMul) == expect_mul, "PS mul token expectation")) {
      return false;
    }
    return Check(true, tag);
  };

  // Default stage0: COLOR = TEXTURE * DIFFUSE, ALPHA = TEXTURE.
  if (!DrawTri("DrawPrimitiveUP(first)")) {
    return false;
  }
  if (!ExpectFixedfuncPsTokens("fixed-function PS tokens (modulate/texture)",
                               /*expect_texld=*/true,
                               /*expect_mul=*/true)) {
    return false;
  }

  // Stage0: COLOR = TEXTURE * DIFFUSE, ALPHAOP = DISABLE (alpha from diffuse/current).
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "SetTextureStageState(ALPHAOP=DISABLE)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(second)")) {
    return false;
  }
  if (!ExpectFixedfuncPsTokens("fixed-function PS tokens (modulate/diffuse)",
                               /*expect_texld=*/true,
                               /*expect_mul=*/true)) {
    return false;
  }

  // Stage0: COLOR = TEXTURE * DIFFUSE, ALPHA = TEXTURE * DIFFUSE.
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopModulate, "SetTextureStageState(ALPHAOP=MODULATE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE) (modulate)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg2, kD3dTaDiffuse, "SetTextureStageState(ALPHAARG2=DIFFUSE) (modulate)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(third)")) {
    return false;
  }
  if (!ExpectFixedfuncPsTokens("fixed-function PS tokens (modulate/modulate)",
                               /*expect_texld=*/true,
                               /*expect_mul=*/true)) {
    return false;
  }

  // Stage0: COLOR = TEXTURE, ALPHA = TEXTURE * DIFFUSE.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopSelectArg1, "SetTextureStageState(COLOROP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "SetTextureStageState(COLORARG1=TEXTURE) (select)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(fourth)")) {
    return false;
  }
  if (!ExpectFixedfuncPsTokens("fixed-function PS tokens (texture/modulate)",
                               /*expect_texld=*/true,
                               /*expect_mul=*/true)) {
    return false;
  }

  // Stage0: COLOR = TEXTURE, ALPHA = TEXTURE.
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE) (select)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(fifth)")) {
    return false;
  }
  if (!ExpectFixedfuncPsTokens("fixed-function PS tokens (texture/texture)",
                               /*expect_texld=*/true,
                               /*expect_mul=*/false)) {
    return false;
  }

  // Stage0: COLOR = TEXTURE, ALPHAOP = DISABLE (alpha from diffuse/current).
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "SetTextureStageState(ALPHAOP=DISABLE) (texture)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(sixth)")) {
    return false;
  }
  if (!ExpectFixedfuncPsTokens("fixed-function PS tokens (texture/diffuse)",
                               /*expect_texld=*/true,
                               /*expect_mul=*/false)) {
    return false;
  }

  // Stage0: COLOR = DIFFUSE, ALPHA = TEXTURE.
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaDiffuse, "SetTextureStageState(COLORARG1=DIFFUSE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1) (diffuse)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE) (diffuse)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(seventh)")) {
    return false;
  }
  if (!ExpectFixedfuncPsTokens("fixed-function PS tokens (diffuse/texture)",
                               /*expect_texld=*/true,
                               /*expect_mul=*/false)) {
    return false;
  }

  // Stage0: COLOR = DIFFUSE, ALPHA = TEXTURE * DIFFUSE.
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopModulate, "SetTextureStageState(ALPHAOP=MODULATE) (diffuse)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE) (diffuse modulate)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg2, kD3dTaDiffuse, "SetTextureStageState(ALPHAARG2=DIFFUSE) (diffuse modulate)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(eighth)")) {
    return false;
  }
  if (!ExpectFixedfuncPsTokens("fixed-function PS tokens (diffuse/modulate)",
                               /*expect_texld=*/true,
                               /*expect_mul=*/true)) {
    return false;
  }

  // Stage0: COLOROP=DISABLE disables the entire stage, so alpha comes from diffuse/current.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopDisable, "SetTextureStageState(COLOROP=DISABLE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1) (disable)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE) (disable)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(ninth)")) {
    return false;
  }
  if (!ExpectFixedfuncPsTokens("fixed-function PS tokens (disable -> passthrough)",
                               /*expect_texld=*/false,
                               /*expect_mul=*/false)) {
    return false;
  }

  // Restore default stage0 and ensure the shader rebinds back to texturing.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "SetTextureStageState(COLOROP=MODULATE) (restore)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "SetTextureStageState(COLORARG1=TEXTURE) (restore)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "SetTextureStageState(COLORARG2=DIFFUSE) (restore)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1) (restore)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE) (restore)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg2, kD3dTaDiffuse, "SetTextureStageState(ALPHAARG2=DIFFUSE) (restore)")) {
    return false;
  }
  if (!DrawTri("DrawPrimitiveUP(tenth)")) {
    return false;
  }
  if (!ExpectFixedfuncPsTokens("fixed-function PS tokens (restore modulate/texture)",
                               /*expect_texld=*/true,
                               /*expect_mul=*/true)) {
    return false;
  }

  // If texture0 is unbound, do not select a texture-sampling shader even when stage0
  // state requests texturing.
  {
    D3DDDI_HRESOURCE null_tex{};
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, null_tex);
    if (!Check(hr == S_OK, "SetTexture(stage0=null)")) {
      return false;
    }
  }
  if (!DrawTri("DrawPrimitiveUP(eleventh)")) {
    return false;
  }
  if (!ExpectFixedfuncPsTokens("fixed-function PS tokens (no texture -> passthrough)",
                               /*expect_texld=*/false,
                               /*expect_mul=*/false)) {
    return false;
  }

  // Rebind texture and set an unsupported stage0 op. Setting the state should
  // succeed, but draws should fail cleanly with D3DERR_INVALIDCALL and must not
  // emit additional commands.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0=rebind)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopAddSmooth, "SetTextureStageState(COLOROP=ADDSMOOTH) succeeds")) {
    return false;
  }
  const size_t before_bad_draw = dev->cmd.bytes_used();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP unsupported stage0 => D3DERR_INVALIDCALL")) {
    return false;
  }
  if (!Check(dev->cmd.bytes_used() == before_bad_draw, "unsupported draw emits no new commands")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(stage-state change)")) {
    return false;
  }

  return true;
}

bool TestStage0OpExpansionSelectsShadersAndCaches() {
  struct Case {
    const char* name;
    // Stage0 state.
    uint32_t color_op = kD3dTopSelectArg1;
    uint32_t color_arg1 = kD3dTaDiffuse;
    uint32_t color_arg2 = kD3dTaDiffuse;
    uint32_t alpha_op = kD3dTopSelectArg1;
    uint32_t alpha_arg1 = kD3dTaTexture;
    uint32_t alpha_arg2 = kD3dTaDiffuse;

    // Optional render-state setup.
    bool set_tfactor = false;
    uint32_t tfactor = 0u;
    bool uses_tfactor = false;

    // Expected fixed-function PS token usage.
    bool expect_texld = false;
    bool expect_add = false;
    bool expect_mul = false;
  };

  const Case cases[] = {
      // Extended ops (RGB path). Keep ALPHA=TEXTURE so RGB expectations match common D3D9 usage.
      {"add", kD3dTopAdd, kD3dTaTexture, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       /*expect_texld=*/true, /*expect_add=*/true, /*expect_mul=*/false},
      {"addsigned", kD3dTopAddSigned, kD3dTaTexture, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       /*expect_texld=*/true, /*expect_add=*/true, /*expect_mul=*/false},
      {"blendtexturealpha", kD3dTopBlendTextureAlpha, kD3dTaTexture, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       /*expect_texld=*/true, /*expect_add=*/true, /*expect_mul=*/true},
      {"blenddiffusealpha_tex", kD3dTopBlendDiffuseAlpha, kD3dTaTexture, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       /*expect_texld=*/true, /*expect_add=*/true, /*expect_mul=*/true},
      {"blenddiffusealpha_tfactor", kD3dTopBlendDiffuseAlpha, kD3dTaDiffuse, kD3dTaTFactor, kD3dTopSelectArg1, kD3dTaDiffuse, kD3dTaDiffuse,
       /*set_tfactor=*/true, 0xFF3366CCu, /*uses_tfactor=*/true,
       /*expect_texld=*/false, /*expect_add=*/true, /*expect_mul=*/true},
      {"subtract_tex_minus_diff", kD3dTopSubtract, kD3dTaTexture, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       /*expect_texld=*/true, /*expect_add=*/true, /*expect_mul=*/false},
      {"subtract_diff_minus_tex", kD3dTopSubtract, kD3dTaDiffuse, kD3dTaTexture, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       /*expect_texld=*/true, /*expect_add=*/true, /*expect_mul=*/false},
      {"modulate2x", kD3dTopModulate2x, kD3dTaTexture, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       /*expect_texld=*/true, /*expect_add=*/true, /*expect_mul=*/true},
      {"modulate4x", kD3dTopModulate4x, kD3dTaTexture, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTexture, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/false,
       /*expect_texld=*/true, /*expect_add=*/true, /*expect_mul=*/true},

      // TFACTOR source (select arg1).
      {"tfactor_select", kD3dTopSelectArg1, kD3dTaTFactor, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTFactor, kD3dTaDiffuse,
       /*set_tfactor=*/true, 0xFF3366CCu, /*uses_tfactor=*/true,
       /*expect_texld=*/false, /*expect_add=*/false, /*expect_mul=*/false},
      // Default TFACTOR is white (0xFFFFFFFF). Verify the driver uploads c255 even
      // if the app never explicitly sets D3DRS_TEXTUREFACTOR.
      {"tfactor_default", kD3dTopSelectArg1, kD3dTaTFactor, kD3dTaDiffuse, kD3dTopSelectArg1, kD3dTaTFactor, kD3dTaDiffuse,
       /*set_tfactor=*/false, 0u, /*uses_tfactor=*/true,
       /*expect_texld=*/false, /*expect_add=*/false, /*expect_mul=*/false},
  };

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  for (const auto& c : cases) {
    CleanupDevice cleanup;
    if (!CreateDevice(&cleanup)) {
      return false;
    }
    auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
    if (!Check(dev != nullptr, "device pointer")) {
      return false;
    }

    dev->cmd.reset();

    HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
    if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
      return false;
    }

    // Most cases require a bound texture so the stage0 path can sample it.
    // For the TFACTOR-only shader, binding a texture is optional but harmless.
    D3DDDI_HRESOURCE hTex{};
    if (!CreateDummyTexture(&cleanup, &hTex)) {
      return false;
    }
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
    if (!Check(hr == S_OK, "SetTexture(stage0)")) {
      return false;
    }

    if (c.set_tfactor) {
      hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsTextureFactor, c.tfactor);
      if (!Check(hr == S_OK, "SetRenderState(TEXTUREFACTOR)")) {
        return false;
      }
    }

    const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* name) -> bool {
      HRESULT hr2 = S_OK;
      if (cleanup.device_funcs.pfnSetTextureStageState) {
        hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      } else {
        hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
      }
      if (hr2 == S_OK) {
        return true;
      }
      std::fprintf(stderr, "FAIL: %s: SetTextureStageState(%s) hr=0x%08x\n", c.name, name, static_cast<unsigned>(hr2));
      return false;
    };

    // Override stage0 state.
    //
    // SetTextureStageState normally updates the stage0 fixed-function PS selection on
    // each call. To avoid creating intermediate PS variants (and emitting extra
    // CREATE_SHADER_DXBC packets), temporarily bind a dummy user PS so the stage0
    // selection hook is suppressed until we're done setting all state.
    {
      const uint8_t dummy_dxbc[] = {0x44, 0x58, 0x42, 0x43, 0x11, 0x22, 0x33, 0x44};
      D3D9DDI_HSHADER hDummyPs{};
      hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                                kD3dShaderStagePs,
                                                dummy_dxbc,
                                                static_cast<uint32_t>(sizeof(dummy_dxbc)),
                                                &hDummyPs);
      if (!Check(hr == S_OK, "CreateShader(dummy PS)")) {
        return false;
      }
      cleanup.shaders.push_back(hDummyPs);

      hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hDummyPs);
      if (!Check(hr == S_OK, "SetShader(PS=dummy)")) {
        return false;
      }

      if (!SetTextureStageState(/*stage=*/0, kD3dTssColorOp, c.color_op, "COLOROP")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0, kD3dTssColorArg1, c.color_arg1, "COLORARG1")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0, kD3dTssColorArg2, c.color_arg2, "COLORARG2")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaOp, c.alpha_op, "ALPHAOP")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaArg1, c.alpha_arg1, "ALPHAARG1")) {
        return false;
      }
      if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaArg2, c.alpha_arg2, "ALPHAARG2")) {
        return false;
      }

      D3D9DDI_HSHADER null_shader{};
      hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, null_shader);
      if (!Check(hr == S_OK, "SetShader(PS=NULL)")) {
        return false;
      }
    }

    // Draw twice: the first draw may create/bind the internal fixed-function PS,
    // the second draw should reuse it without re-emitting CREATE_SHADER_DXBC.
    for (int i = 0; i < 2; ++i) {
      hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
          cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
      if (!Check(hr == S_OK, c.name)) {
        return false;
      }
    }

    // Validate the bound PS matches the expected variant.
    std::vector<uint8_t> expected_ps_bytes;
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      if (!Check(dev->ps != nullptr, "PS must be bound")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld) == c.expect_texld, "PS texld token expectation")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpAdd) == c.expect_add, "PS add token expectation")) {
        return false;
      }
      if (!Check(ShaderContainsToken(dev->ps, kPsOpMul) == c.expect_mul, "PS mul token expectation")) {
        return false;
      }
      expected_ps_bytes = dev->ps->bytecode;
    }
    if (!Check(!expected_ps_bytes.empty(), "expected PS bytecode non-empty")) {
      return false;
    }

    dev->cmd.finalize();
    const uint8_t* buf = dev->cmd.data();
    const size_t len = dev->cmd.bytes_used();
    if (!Check(ValidateStream(buf, len), "ValidateStream(stage0 op expansion)")) {
      return false;
    }

    // Confirm the fixed-function PS variant is created at most once (cached across both draws).
    size_t create_count = 0;
    for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
      const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
      if (cs->stage != AEROGPU_SHADER_STAGE_PIXEL) {
        continue;
      }
      if (cs->dxbc_size_bytes != expected_ps_bytes.size()) {
        continue;
      }
      const size_t need = sizeof(aerogpu_cmd_create_shader_dxbc) + expected_ps_bytes.size();
      if (hdr->size_bytes < need) {
        continue;
      }
      const void* payload = reinterpret_cast<const uint8_t*>(cs) + sizeof(aerogpu_cmd_create_shader_dxbc);
      if (std::memcmp(payload, expected_ps_bytes.data(), expected_ps_bytes.size()) == 0) {
        ++create_count;
      }
    }
    if (!Check(create_count == 1, "fixed-function PS CREATE_SHADER_DXBC emitted once (cached)")) {
      return false;
    }

    // TFACTOR cases: ensure the PS constant upload was emitted once (c255) and
    // contains the expected normalized RGBA value.
    if (c.uses_tfactor) {
      const uint32_t expected_tf = c.set_tfactor ? c.tfactor : 0xFFFFFFFFu;
      const float expected_a = static_cast<float>((expected_tf >> 24) & 0xFFu) * (1.0f / 255.0f);
      const float expected_r = static_cast<float>((expected_tf >> 16) & 0xFFu) * (1.0f / 255.0f);
      const float expected_g = static_cast<float>((expected_tf >> 8) & 0xFFu) * (1.0f / 255.0f);
      const float expected_b = static_cast<float>((expected_tf >> 0) & 0xFFu) * (1.0f / 255.0f);
      const float expected_vec[4] = {expected_r, expected_g, expected_b, expected_a};

      size_t tfactor_uploads = 0;
      for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
        const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
        if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 255 || sc->vec4_count != 1) {
          continue;
        }
        if (!Check(hdr->size_bytes >= sizeof(*sc) + sizeof(expected_vec), "SET_SHADER_CONSTANTS_F contains payload")) {
          return false;
        }
        const auto* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
        if (!Check(std::fabs(payload[0] - expected_vec[0]) < 1e-6f &&
                       std::fabs(payload[1] - expected_vec[1]) < 1e-6f &&
                       std::fabs(payload[2] - expected_vec[2]) < 1e-6f &&
                       std::fabs(payload[3] - expected_vec[3]) < 1e-6f,
                   "TFACTOR constant payload matches expected RGBA")) {
          return false;
        }
        ++tfactor_uploads;
      }
      if (!Check(tfactor_uploads == 1, "TFACTOR constant upload emitted once (cached)")) {
        return false;
      }
    }
  }

  return true;
}

bool TestStage0ArgModifiersEmitSourceMods() {
  struct Case {
    const char* name = nullptr;
    uint32_t color_arg1 = kD3dTaTexture;
    uint32_t expected_src_token = 0;
    bool expect_texld = false;
  };

  const Case cases[] = {
      {"color_texture_complement", kD3dTaTexture | kD3dTaComplement, kPsSrcTemp0Comp, /*expect_texld=*/true},
      {"color_texture_alpha_replicate", kD3dTaTexture | kD3dTaAlphaReplicate, kPsSrcTemp0W, /*expect_texld=*/true},
      {"color_texture_comp_alpha_replicate",
       kD3dTaTexture | kD3dTaComplement | kD3dTaAlphaReplicate,
       kPsSrcTemp0WComp,
       /*expect_texld=*/true},
      {"color_diffuse_complement", kD3dTaDiffuse | kD3dTaComplement, kPsSrcInput0Comp, /*expect_texld=*/false},
      {"color_diffuse_alpha_replicate", kD3dTaDiffuse | kD3dTaAlphaReplicate, kPsSrcInput0W, /*expect_texld=*/false},
      {"color_diffuse_comp_alpha_replicate",
       kD3dTaDiffuse | kD3dTaComplement | kD3dTaAlphaReplicate,
       kPsSrcInput0WComp,
       /*expect_texld=*/false},
  };

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  for (const auto& c : cases) {
    CleanupDevice cleanup;
    if (!CreateDevice(&cleanup)) {
      return false;
    }

    auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
    if (!Check(dev != nullptr, "device pointer")) {
      return false;
    }

    dev->cmd.reset();

    HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
    if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
      return false;
    }

    D3DDDI_HRESOURCE hTex{};
    if (!CreateDummyTexture(&cleanup, &hTex)) {
      return false;
    }
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
    if (!Check(hr == S_OK, "SetTexture(stage0)")) {
      return false;
    }

    const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
      HRESULT hr2 = S_OK;
      if (cleanup.device_funcs.pfnSetTextureStageState) {
        hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      } else {
        hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
      }
      if (!Check(hr2 == S_OK, msg)) {
        std::fprintf(stderr, "FAIL: %s (SetTextureStageState %s) hr=0x%08x\n", c.name, msg, static_cast<unsigned>(hr2));
        return false;
      }
      return true;
    };

    if (!SetTextureStageState(/*stage=*/0, kD3dTssColorOp, kD3dTopSelectArg1, "COLOROP=SELECTARG1")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0, kD3dTssColorArg1, c.color_arg1, "COLORARG1")) {
      return false;
    }
    // Disable alpha stage so alpha replicate tokens are driven only by COLORARG1.
    if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaOp, kD3dTopDisable, "ALPHAOP=DISABLE")) {
      return false;
    }

    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
    if (!Check(hr == S_OK, c.name)) {
      return false;
    }

    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "PS must be bound")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld) == c.expect_texld, "PS texld token expectation")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, c.expected_src_token), "PS contains expected source-mod token")) {
      return false;
    }
  }

  return true;
}

bool TestStage0IgnoresUnusedArgsAndOps() {
  struct Case {
    const char* name = nullptr;
    // Stage0 state.
    uint32_t color_op = kD3dTopSelectArg1;
    uint32_t color_arg1 = kD3dTaDiffuse;
    uint32_t color_arg2 = kD3dTaDiffuse;
    uint32_t alpha_op = kD3dTopDisable;
    uint32_t alpha_arg1 = kD3dTaDiffuse;
    uint32_t alpha_arg2 = kD3dTaDiffuse;
    // Expectations.
    bool expect_texld = false;
  };

  const Case cases[] = {
      // COLOROP=DISABLE disables the entire stage; alpha op/args must be ignored,
      // even if they are otherwise unsupported.
      {"color_disable_ignores_unsupported_alphaop",
       kD3dTopDisable, kD3dTaDiffuse, kD3dTaDiffuse,
       kD3dTopAddSmooth, kD3dTaTexture, kD3dTaDiffuse,
       /*expect_texld=*/false},

      // SELECTARG1 uses only ARG1; ARG2 should not be decoded/validated.
      {"selectarg1_ignores_colorarg2",
       kD3dTopSelectArg1, kD3dTaDiffuse, kD3dTaSpecular,
       kD3dTopDisable, kD3dTaDiffuse, kD3dTaSpecular,
       /*expect_texld=*/false},

      // SELECTARG2 uses only ARG2; ARG1 should not be decoded/validated.
      {"selectarg2_ignores_colorarg1",
       kD3dTopSelectArg2, kD3dTaSpecular, kD3dTaDiffuse,
       kD3dTopDisable, kD3dTaDiffuse, kD3dTaSpecular,
       /*expect_texld=*/false},

      // ALPHAOP=SELECTARG1 uses only ALPHAARG1; ALPHAARG2 should not be decoded/validated.
      {"selectarg1_ignores_alphaarg2",
       kD3dTopSelectArg1, kD3dTaDiffuse, kD3dTaDiffuse,
       kD3dTopSelectArg1, kD3dTaDiffuse, kD3dTaSpecular,
       /*expect_texld=*/false},
  };

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  for (const auto& c : cases) {
    CleanupDevice cleanup;
    if (!CreateDevice(&cleanup)) {
      return false;
    }

    auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
    if (!Check(dev != nullptr, "device pointer")) {
      return false;
    }

    dev->cmd.reset();

    HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
    if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
      return false;
    }

    D3DDDI_HRESOURCE hTex{};
    if (!CreateDummyTexture(&cleanup, &hTex)) {
      return false;
    }
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
    if (!Check(hr == S_OK, "SetTexture(stage0)")) {
      return false;
    }

    const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
      HRESULT hr2 = S_OK;
      if (cleanup.device_funcs.pfnSetTextureStageState) {
        hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
      } else {
        hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
      }
      if (!Check(hr2 == S_OK, msg)) {
        std::fprintf(stderr, "FAIL: %s (SetTextureStageState %s) hr=0x%08x\n", c.name, msg, static_cast<unsigned>(hr2));
        return false;
      }
      return true;
    };

    if (!SetTextureStageState(/*stage=*/0, kD3dTssColorOp, c.color_op, "COLOROP")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0, kD3dTssColorArg1, c.color_arg1, "COLORARG1")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0, kD3dTssColorArg2, c.color_arg2, "COLORARG2")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaOp, c.alpha_op, "ALPHAOP")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaArg1, c.alpha_arg1, "ALPHAARG1")) {
      return false;
    }
    if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaArg2, c.alpha_arg2, "ALPHAARG2")) {
      return false;
    }

    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
    if (!Check(hr == S_OK, c.name)) {
      return false;
    }

    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "PS must be bound")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld) == c.expect_texld, "PS texld token expectation")) {
      return false;
    }
  }

  return true;
}

bool TestStage0CurrentCanonicalizesToDiffuse() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: SELECTARG1 with COLORARG1=CURRENT (treated as DIFFUSE at stage0).
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopSelectArg1, "SetTextureStageState(COLOROP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaCurrent, "SetTextureStageState(COLORARG1=CURRENT)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "SetTextureStageState(ALPHAOP=DISABLE)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(CURRENT)")) {
    return false;
  }

  Shader* ps_current = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_current = dev->ps;
  }
  if (!Check(ps_current != nullptr, "PS bound after CURRENT draw")) {
    return false;
  }

  // Switch to DIFFUSE. This should reuse the same cached stage0 PS variant.
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaDiffuse, "SetTextureStageState(COLORARG1=DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(DIFFUSE)")) {
    return false;
  }

  Shader* ps_diffuse = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_diffuse = dev->ps;
  }
  if (!Check(ps_diffuse != nullptr, "PS bound after DIFFUSE draw")) {
    return false;
  }
  return Check(ps_current == ps_diffuse, "CURRENT canonicalizes to DIFFUSE (reuse cached PS)");
}

bool TestStage0NoTextureCanonicalizesAndReusesShader() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  // Use a TEX1 FVF so stage0 state changes are expected to influence PS selection
  // when fixed-function is active.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Explicitly clear texture0 so any stage0 state that references TEXTURE is
  // canonicalized to DISABLE at draw time.
  {
    D3DDDI_HRESOURCE null_tex{};
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, null_tex);
    if (!Check(hr == S_OK, "SetTexture(stage0=null)")) {
      return false;
    }
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Start from a supported stage0 state that would normally require sampling texture0.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "SetTextureStageState(COLOROP=MODULATE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "SetTextureStageState(COLORARG1=TEXTURE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "SetTextureStageState(COLORARG2=DIFFUSE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage0 no texture; initial)")) {
    return false;
  }

  Shader* initial_ps = nullptr;
  size_t initial_sig_cache_size = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    initial_ps = dev->ps;
    initial_sig_cache_size = dev->fixedfunc_ps_variant_cache.size();
    if (!Check(initial_ps != nullptr, "PS bound after initial no-texture draw")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(initial_ps, kPsOpTexld), "no-texture stage0 canonicalizes (no texld)")) {
      return false;
    }
  }

  // Isolate stage-state-driven changes. With texture0 still null, changing stage0
  // state should *not* create a new PS variant (canonicalized to DISABLE).
  dev->cmd.reset();

  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopAdd, "SetTextureStageState(COLOROP=ADD)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "SetTextureStageState(ALPHAOP=DISABLE)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == initial_ps, "PS unchanged after stage0 state change (texture0 null)")) {
      return false;
    }
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == initial_sig_cache_size,
               "stage0 signature cache size unchanged (texture0 null)")) {
      return false;
    }
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage0 no texture; after state change)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == initial_ps, "PS unchanged after draw (texture0 null)")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "still no texld after stage0 state change")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(stage0 no-texture canonicalization)")) {
    return false;
  }
  // With shaders already created by the initial draw, changing stage state (while
  // texture0 is still null) must not trigger shader creation.
  return Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0,
               "no CREATE_SHADER_DXBC emitted for stage0 changes with texture0 null");
}

bool TestStage1TextureEnableAddsSecondTexld() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Bind texture0 but leave texture1 null initially. Stage1 state will reference
  // TEXTURE, so stage1 should be dropped until texture1 is bound.
  D3DDDI_HRESOURCE hTex0{};
  if (!CreateDummyTexture(&cleanup, &hTex0)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex1{};
  if (!CreateDummyTexture(&cleanup, &hTex1)) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: modulate tex0 * diffuse.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "stage0 COLORARG2=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: add current + tex1. Requires TEXTURE at stage1, so should only take
  // effect once texture1 is bound.
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopAdd, "stage1 COLOROP=ADD")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg1, kD3dTaCurrent, "stage1 COLORARG1=CURRENT")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg2, kD3dTaTexture, "stage1 COLORARG2=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssAlphaOp, kD3dTopDisable, "stage1 ALPHAOP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  // Draw without texture1: stage1 must be dropped, so PS should reference only s0.
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 texture null)")) {
    return false;
  }
  Shader* ps_no_tex1 = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_no_tex1 = dev->ps;
  }
  if (!Check(ps_no_tex1 != nullptr, "PS bound (stage1 texture null)")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps_no_tex1, kPsOpTexld) == 1, "stage1 disabled => exactly 1 texld")) {
    return false;
  }
  if (!Check(ShaderTexldSamplerMask(ps_no_tex1) == 0x1u, "stage1 disabled => texld uses only sampler s0")) {
    return false;
  }

  // Bind texture1 and draw again: stage1 should now be enabled, producing a PS
  // that contains a second texld and references s1.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 texture bound)")) {
    return false;
  }
  Shader* ps_with_tex1 = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_with_tex1 = dev->ps;
  }
  if (!Check(ps_with_tex1 != nullptr, "PS bound (stage1 texture bound)")) {
    return false;
  }
  if (!Check(ps_with_tex1 != ps_no_tex1, "binding texture1 changes fixed-function PS variant")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps_with_tex1, kPsOpTexld) == 2, "stage1 enabled => exactly 2 texld")) {
    return false;
  }
  return Check(ShaderTexldSamplerMask(ps_with_tex1) == 0x3u, "stage1 enabled => texld uses samplers s0 and s1");
}

bool TestStage1ColorDisableIgnoresUnsupportedAlphaAndDoesNotSampleTexture1() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  if (!CreateDummyTexture(&cleanup, &hTex0)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex1{};
  if (!CreateDummyTexture(&cleanup, &hTex1)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: modulate tex0 * diffuse.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "stage0 COLORARG2=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: explicitly disabled via COLOROP=DISABLE. Per D3D9 semantics, this
  // terminates the stage chain; stage1 alpha state must be ignored.
  //
  // Set unsupported alpha state + unsupported color args to ensure draw-time PS
  // selection does not attempt to decode/validate them.
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg1, kD3dTaSpecular, "stage1 COLORARG1=SPECULAR (unsupported)")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssAlphaOp, kD3dTopAddSmooth, "stage1 ALPHAOP=ADDSMOOTH (unsupported)")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssAlphaArg1, kD3dTaTexture, "stage1 ALPHAARG1=TEXTURE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 COLOROP=DISABLE)")) {
    return false;
  }

  Shader* ps = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps = dev->ps;
  }
  if (!Check(ps != nullptr, "PS bound")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps, kPsOpTexld) == 1, "stage1 disabled => exactly 1 texld")) {
    return false;
  }
  return Check(ShaderTexldSamplerMask(ps) == 0x1u, "stage1 disabled => texld uses only sampler s0");
}

bool TestStage0NoTextureAllowsStage1ToSampleTexture1WithSampler1() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex1{};
  if (!CreateDummyTexture(&cleanup, &hTex1)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: no texture; pass diffuse through unchanged.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopSelectArg1, "stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaDiffuse, "stage0 COLORARG1=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage1: sample texture1. This should emit a single texld, but it must
  // reference sampler1 (s1), not sampler0 (s0), since stage0 does not sample.
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopSelectArg1, "stage1 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg1, kD3dTaTexture, "stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssAlphaOp, kD3dTopDisable, "stage1 ALPHAOP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage0 no-texture; stage1 texture1)")) {
    return false;
  }

  Shader* ps = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps = dev->ps;
  }
  if (!Check(ps != nullptr, "PS bound")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps, kPsOpTexld) == 1, "exactly 1 texld")) {
    return false;
  }
  return Check(ShaderTexldSamplerMask(ps) == 0x2u, "texld uses only sampler s1");
}

bool TestStage2SamplingUsesSampler2EvenIfStage1DoesNotSample() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Bind textures for stage0 and stage2. Stage1 remains unbound and must not be sampled.
  D3DDDI_HRESOURCE hTex0{};
  if (!CreateDummyTexture(&cleanup, &hTex0)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex2{};
  if (!CreateDummyTexture(&cleanup, &hTex2)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: modulate tex0 * diffuse.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "stage0 COLORARG2=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage1: enable stage1 but do not sample any textures (passthrough CURRENT).
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopSelectArg1, "stage1 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg1, kD3dTaCurrent, "stage1 COLORARG1=CURRENT")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssAlphaOp, kD3dTopDisable, "stage1 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage2: sample texture2 (must use sampler2 even though stage1 does not sample).
  if (!SetTextureStageState(2, kD3dTssColorOp, kD3dTopSelectArg1, "stage2 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(2, kD3dTssColorArg1, kD3dTaTexture, "stage2 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(2, kD3dTssAlphaOp, kD3dTopDisable, "stage2 ALPHAOP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage2 texture2)")) {
    return false;
  }

  Shader* ps = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps = dev->ps;
  }
  if (!Check(ps != nullptr, "PS bound")) {
    return false;
  }

  // Stage0 and stage2 should each contribute a texld.
  if (!Check(ShaderCountToken(ps, kPsOpTexld) == 2, "exactly 2 texld")) {
    return false;
  }
  return Check(ShaderTexldSamplerMask(ps) == 0x5u, "texld uses samplers s0 and s2");
}

bool TestStage1MissingTextureDisablesStage2Sampling() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Bind textures for stage0 and stage2, but leave stage1 unbound. Stage1 state
  // will request texturing, so the stage chain must terminate before stage2.
  D3DDDI_HRESOURCE hTex0{};
  if (!CreateDummyTexture(&cleanup, &hTex0)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex2{};
  if (!CreateDummyTexture(&cleanup, &hTex2)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }

  {
    // Explicitly clear stage1 to document the intended test setup.
    D3DDDI_HRESOURCE null_tex{};
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, null_tex);
    if (!Check(hr == S_OK, "SetTexture(stage1=null)")) {
      return false;
    }
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: sample texture0.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "stage0 COLORARG2=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage1: request texturing, but the texture is unbound, so the stage chain
  // must terminate here.
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopSelectArg1, "stage1 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg1, kD3dTaTexture, "stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssAlphaOp, kD3dTopDisable, "stage1 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage2: would sample texture2 if reached; must be ignored due to stage1's
  // missing texture.
  if (!SetTextureStageState(2, kD3dTssColorOp, kD3dTopSelectArg1, "stage2 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(2, kD3dTssColorArg1, kD3dTaTexture, "stage2 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(2, kD3dTssAlphaOp, kD3dTopDisable, "stage2 ALPHAOP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 missing texture)")) {
    return false;
  }

  Shader* ps = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps = dev->ps;
  }
  if (!Check(ps != nullptr, "PS bound")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps, kPsOpTexld) == 1, "stage1 missing => exactly 1 texld")) {
    return false;
  }
  return Check(ShaderTexldSamplerMask(ps) == 0x1u, "stage1 missing => texld uses only sampler s0");
}

bool TestStage1BlendTextureAlphaRequiresTextureEvenWithoutTextureArgs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Bind textures for stage0 and stage2, but leave stage1 unbound. Stage1 uses
  // BLENDTEXTUREALPHA, which implicitly requires the stage texture even if its
  // args are DIFFUSE/CURRENT.
  D3DDDI_HRESOURCE hTex0{};
  if (!CreateDummyTexture(&cleanup, &hTex0)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex2{};
  if (!CreateDummyTexture(&cleanup, &hTex2)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }

  {
    D3DDDI_HRESOURCE null_tex{};
    hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, null_tex);
    if (!Check(hr == S_OK, "SetTexture(stage1=null)")) {
      return false;
    }
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: sample texture0.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "stage0 COLORARG2=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage1: BLENDTEXTUREALPHA uses texture alpha as the blend factor regardless
  // of arg sources, so stage1 must be treated as sampling its texture.
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopBlendTextureAlpha, "stage1 COLOROP=BLENDTEXTUREALPHA")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg1, kD3dTaDiffuse, "stage1 COLORARG1=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg2, kD3dTaCurrent, "stage1 COLORARG2=CURRENT")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssAlphaOp, kD3dTopDisable, "stage1 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage2: would sample texture2 if reached.
  if (!SetTextureStageState(2, kD3dTssColorOp, kD3dTopSelectArg1, "stage2 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(2, kD3dTssColorArg1, kD3dTaTexture, "stage2 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(2, kD3dTssAlphaOp, kD3dTopDisable, "stage2 ALPHAOP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 blendtexturealpha missing texture)")) {
    return false;
  }

  Shader* ps = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps = dev->ps;
  }
  if (!Check(ps != nullptr, "PS bound")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps, kPsOpTexld) == 1, "stage1 missing => exactly 1 texld")) {
    return false;
  }
  return Check(ShaderTexldSamplerMask(ps) == 0x1u, "stage1 missing => texld uses only sampler s0");
}

bool TestStage3SamplingUsesSampler3EvenIfStage1AndStage2DoNotSample() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Bind textures for stage0 and stage3. Stage1 and stage2 remain unbound but
  // are configured to not sample, so the stage chain must still reach stage3.
  D3DDDI_HRESOURCE hTex0{};
  if (!CreateDummyTexture(&cleanup, &hTex0)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex3{};
  if (!CreateDummyTexture(&cleanup, &hTex3)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3);
  if (!Check(hr == S_OK, "SetTexture(stage3)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: sample texture0.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "stage0 COLORARG2=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage1: passthrough CURRENT (no sampling).
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopSelectArg1, "stage1 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorArg1, kD3dTaCurrent, "stage1 COLORARG1=CURRENT")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssAlphaOp, kD3dTopDisable, "stage1 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage2: passthrough CURRENT (no sampling).
  if (!SetTextureStageState(2, kD3dTssColorOp, kD3dTopSelectArg1, "stage2 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(2, kD3dTssColorArg1, kD3dTaCurrent, "stage2 COLORARG1=CURRENT")) {
    return false;
  }
  if (!SetTextureStageState(2, kD3dTssAlphaOp, kD3dTopDisable, "stage2 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage3: sample texture3 (must use sampler3 even though stage1/stage2 don't sample).
  if (!SetTextureStageState(3, kD3dTssColorOp, kD3dTopSelectArg1, "stage3 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(3, kD3dTssColorArg1, kD3dTaTexture, "stage3 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(3, kD3dTssAlphaOp, kD3dTopDisable, "stage3 ALPHAOP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage3 texture3)")) {
    return false;
  }

  Shader* ps = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps = dev->ps;
  }
  if (!Check(ps != nullptr, "PS bound")) {
    return false;
  }

  // Stage0 and stage3 should each contribute a texld.
  if (!Check(ShaderCountToken(ps, kPsOpTexld) == 2, "exactly 2 texld")) {
    return false;
  }
  return Check(ShaderTexldSamplerMask(ps) == 0x9u, "texld uses samplers s0 and s3");
}

bool TestApplyStateBlockUpdatesFixedfuncPsForTextureStageState() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  D3DDDI_HRESOURCE hTex1{};
  if (!CreateDummyTexture(&cleanup, &hTex0) || !CreateDummyTexture(&cleanup, &hTex1)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: modulate tex0 * diffuse.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "stage0 COLORARG2=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }

  // Stage1: add current + tex1, and disable alpha stage.
  const auto SetStage1Enabled = [&]() -> bool {
    if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopAdd, "stage1 COLOROP=ADD")) {
      return false;
    }
    if (!SetTextureStageState(1, kD3dTssColorArg1, kD3dTaCurrent, "stage1 COLORARG1=CURRENT")) {
      return false;
    }
    if (!SetTextureStageState(1, kD3dTssColorArg2, kD3dTaTexture, "stage1 COLORARG2=TEXTURE")) {
      return false;
    }
    if (!SetTextureStageState(1, kD3dTssAlphaOp, kD3dTopDisable, "stage1 ALPHAOP=DISABLE")) {
      return false;
    }
    return true;
  };

  const auto SetStage1Disabled = [&]() -> bool {
    return SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE");
  };

  if (!SetStage1Enabled()) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 enabled baseline)")) {
    return false;
  }

  Shader* ps_enabled = nullptr;
  Shader* ps_disabled = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_enabled = dev->ps;
  }
  if (!Check(ps_enabled != nullptr, "PS bound (stage1 enabled)")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps_enabled, kPsOpTexld) == 2, "stage1 enabled => exactly 2 texld")) {
    return false;
  }
  if (!Check(ShaderTexldSamplerMask(ps_enabled) == 0x3u, "stage1 enabled => texld uses samplers s0 and s1")) {
    return false;
  }

  if (!SetStage1Disabled()) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 disabled baseline)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_disabled = dev->ps;
  }
  if (!Check(ps_disabled != nullptr, "PS bound (stage1 disabled)")) {
    return false;
  }
  if (!Check(ShaderCountToken(ps_disabled, kPsOpTexld) == 1, "stage1 disabled => exactly 1 texld")) {
    return false;
  }
  if (!Check(ShaderTexldSamplerMask(ps_disabled) == 0x1u, "stage1 disabled => texld uses only sampler s0")) {
    return false;
  }

  // Restore stage1 enabled so ApplyStateBlock can toggle it back off.
  if (!SetStage1Enabled()) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 re-enabled baseline)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == ps_enabled, "stage1 re-enable reuses cached PS variant")) {
      return false;
    }
  }

  D3D9DDI_HSTATEBLOCK hSb{};
  auto DeleteSb = [&]() {
    if (hSb.pDrvPrivate) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      hSb.pDrvPrivate = nullptr;
    }
  };

  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock")) {
    return false;
  }
  if (!SetStage1Disabled()) {
    return false;
  }
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore stage1 enabled again before applying the state block.
  if (!SetStage1Enabled()) {
    DeleteSb();
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 enabled before ApplyStateBlock)")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == ps_enabled, "PS enabled before ApplyStateBlock")) {
      DeleteSb();
      return false;
    }
  }

  // Isolate ApplyStateBlock's command emission.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock")) {
    DeleteSb();
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps == ps_disabled, "ApplyStateBlock updates fixed-function PS")) {
      DeleteSb();
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock stage-state)")) {
    DeleteSb();
    return false;
  }

  // Since both PS variants were created by the earlier baseline draws, applying
  // the state block should only re-bind, not create a new PS.
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) == 0, "ApplyStateBlock emits no CREATE_SHADER_DXBC")) {
    DeleteSb();
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "ApplyStateBlock emits BIND_SHADERS")) {
    DeleteSb();
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->ps == ps_disabled->handle, "ApplyStateBlock binds stage1-disabled PS")) {
    DeleteSb();
    return false;
  }

  DeleteSb();
  return true;
}

bool TestStage0UnsupportedArgFailsAtDraw() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // First, draw once with a supported stage0 state to establish a baseline and
  // ensure shaders are created.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "SetTextureStageState(COLOROP=MODULATE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "SetTextureStageState(COLORARG1=TEXTURE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "SetTextureStageState(COLORARG2=DIFFUSE)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(baseline stage0 supported)")) {
    return false;
  }

  // Now set an unsupported stage0 argument. State setting must succeed (cached
  // for Get*/state blocks), but draws must fail cleanly with INVALIDCALL and must
  // not emit commands.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopSelectArg1, "SetTextureStageState(COLOROP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaSpecular, "SetTextureStageState(COLORARG1=SPECULAR) succeeds")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "SetTextureStageState(ALPHAOP=DISABLE)")) {
    return false;
  }

  const size_t before_bad_draw = dev->cmd.bytes_used();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(unsupported stage0 arg) => D3DERR_INVALIDCALL")) {
    return false;
  }
  return Check(dev->cmd.bytes_used() == before_bad_draw, "unsupported stage0 arg draw emits no new commands");
}

bool TestStage0VariantCacheEvictsOldShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  // Create a tiny dummy user PS so stage-state updates don't immediately trigger
  // fixed-function stage0 selection while we're mutating multiple stage0 fields.
  const uint8_t dummy_dxbc[] = {0x44, 0x58, 0x42, 0x43, 0x11, 0x22, 0x33, 0x44};
  D3D9DDI_HSHADER hDummyPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            dummy_dxbc,
                                            static_cast<uint32_t>(sizeof(dummy_dxbc)),
                                            &hDummyPs);
  if (!Check(hr == S_OK, "CreateShader(dummy PS)")) {
    return false;
  }
  cleanup.shaders.push_back(hDummyPs);

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    if (!Check(hr2 == S_OK, msg)) {
      std::fprintf(stderr, "FAIL: SetTextureStageState(%s) hr=0x%08x\n", msg, static_cast<unsigned>(hr2));
      return false;
    }
    return true;
  };

  // All supported stage0 arg variants (sources + modifiers).
  const uint32_t arg_vals[] = {
      kD3dTaDiffuse,
      kD3dTaDiffuse | kD3dTaComplement,
      kD3dTaDiffuse | kD3dTaAlphaReplicate,
      kD3dTaDiffuse | kD3dTaComplement | kD3dTaAlphaReplicate,
      kD3dTaTexture,
      kD3dTaTexture | kD3dTaComplement,
      kD3dTaTexture | kD3dTaAlphaReplicate,
      kD3dTaTexture | kD3dTaComplement | kD3dTaAlphaReplicate,
      kD3dTaTFactor,
      kD3dTaTFactor | kD3dTaComplement,
      kD3dTaTFactor | kD3dTaAlphaReplicate,
      kD3dTaTFactor | kD3dTaComplement | kD3dTaAlphaReplicate,
  };

  // Use a few different ops to reduce the chance of bytecode aliasing and to
  // force stage0 PS variant cache churn.
  const uint32_t ops[] = {
      kD3dTopModulate,
      kD3dTopAdd,
      kD3dTopSubtract,
      kD3dTopAddSigned,
      kD3dTopModulate2x,
      kD3dTopModulate4x,
  };

  auto CountPixelShaderCreates = [&](const uint8_t* buf, size_t len) -> size_t {
    size_t count = 0;
    for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
      const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
      if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
        ++count;
      }
    }
    return count;
  };

  size_t ps_creates = 0;
  bool done = false;
  for (uint32_t op : ops) {
    for (uint32_t arg1 : arg_vals) {
      for (uint32_t arg2 : arg_vals) {
        // Suppress stage0 selection while setting multiple fields.
        hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hDummyPs);
        if (!Check(hr == S_OK, "SetShader(PS=dummy)")) {
          return false;
        }

        if (!SetTextureStageState(0, kD3dTssColorOp, op, "COLOROP")) {
          return false;
        }
        if (!SetTextureStageState(0, kD3dTssColorArg1, arg1, "COLORARG1")) {
          return false;
        }
        if (!SetTextureStageState(0, kD3dTssColorArg2, arg2, "COLORARG2")) {
          return false;
        }
        if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "ALPHAOP=DISABLE")) {
          return false;
        }

        // Trigger stage0 selection by unbinding the user PS. Reset the command
        // stream first so we can detect whether a new PS was created.
        dev->cmd.reset();
        D3D9DDI_HSHADER null_shader{};
        hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, null_shader);
        if (!Check(hr == S_OK, "SetShader(PS=NULL)")) {
          return false;
        }

        dev->cmd.finalize();
        const uint8_t* buf = dev->cmd.data();
        const size_t len = dev->cmd.bytes_used();
        if (!Check(ValidateStream(buf, len), "ValidateStream(stage0 variant churn)")) {
          return false;
        }

        ps_creates += CountPixelShaderCreates(buf, len);
        if (ps_creates > 100) {
          done = true;
          break;
        }
      }
      if (done) {
        break;
      }
    }
    if (done) {
      break;
    }
  }

  if (!Check(ps_creates > 100, "created > 100 unique stage0 PS variants")) {
    return false;
  }

  size_t cached_variants = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    for (Shader* ps : dev->fixedfunc_ps_variants) {
      if (ps) {
        ++cached_variants;
      }
    }
  }
  return Check(cached_variants == 100, "stage0 PS variant array cache is capped at 100 entries");
}

bool TestStage0SignatureCacheDoesNotPointAtEvictedShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  // Create a tiny dummy user PS so stage-state updates don't immediately trigger
  // fixed-function stage0 selection while we're mutating multiple stage0 fields.
  const uint8_t dummy_dxbc[] = {0x44, 0x58, 0x42, 0x43, 0x11, 0x22, 0x33, 0x44};
  D3D9DDI_HSHADER hDummyPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStagePs,
                                            dummy_dxbc,
                                            static_cast<uint32_t>(sizeof(dummy_dxbc)),
                                            &hDummyPs);
  if (!Check(hr == S_OK, "CreateShader(dummy PS)")) {
    return false;
  }
  cleanup.shaders.push_back(hDummyPs);

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    if (!Check(hr2 == S_OK, msg)) {
      std::fprintf(stderr, "FAIL: SetTextureStageState(%s) hr=0x%08x\n", msg, static_cast<unsigned>(hr2));
      return false;
    }
    return true;
  };

  // All supported stage0 arg variants (sources + modifiers).
  const uint32_t arg_vals[] = {
      kD3dTaDiffuse,
      kD3dTaDiffuse | kD3dTaComplement,
      kD3dTaDiffuse | kD3dTaAlphaReplicate,
      kD3dTaDiffuse | kD3dTaComplement | kD3dTaAlphaReplicate,
      kD3dTaTexture,
      kD3dTaTexture | kD3dTaComplement,
      kD3dTaTexture | kD3dTaAlphaReplicate,
      kD3dTaTexture | kD3dTaComplement | kD3dTaAlphaReplicate,
      kD3dTaTFactor,
      kD3dTaTFactor | kD3dTaComplement,
      kD3dTaTFactor | kD3dTaAlphaReplicate,
      kD3dTaTFactor | kD3dTaComplement | kD3dTaAlphaReplicate,
  };

  // Use a few different ops to reduce the chance of bytecode aliasing and to
  // force stage0 PS variant cache churn.
  const uint32_t ops[] = {
      kD3dTopModulate,
      kD3dTopAdd,
      kD3dTopSubtract,
      kD3dTopAddSigned,
      kD3dTopModulate2x,
      kD3dTopModulate4x,
  };

  auto CountPixelShaderCreates = [&](const uint8_t* buf, size_t len) -> size_t {
    size_t count = 0;
    for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
      const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
      if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
        ++count;
      }
    }
    return count;
  };

  size_t ps_creates = 0;
  bool done = false;
  for (uint32_t op : ops) {
    for (uint32_t arg1 : arg_vals) {
      for (uint32_t arg2 : arg_vals) {
        // Suppress stage0 selection while setting multiple fields.
        hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, hDummyPs);
        if (!Check(hr == S_OK, "SetShader(PS=dummy)")) {
          return false;
        }

        if (!SetTextureStageState(0, kD3dTssColorOp, op, "COLOROP")) {
          return false;
        }
        if (!SetTextureStageState(0, kD3dTssColorArg1, arg1, "COLORARG1")) {
          return false;
        }
        if (!SetTextureStageState(0, kD3dTssColorArg2, arg2, "COLORARG2")) {
          return false;
        }
        if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "ALPHAOP=DISABLE")) {
          return false;
        }

        // Trigger stage0 selection by unbinding the user PS. Reset the command
        // stream first so we can detect whether a new PS was created.
        dev->cmd.reset();
        D3D9DDI_HSHADER null_shader{};
        hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStagePs, null_shader);
        if (!Check(hr == S_OK, "SetShader(PS=NULL)")) {
          return false;
        }

        dev->cmd.finalize();
        const uint8_t* buf = dev->cmd.data();
        const size_t len = dev->cmd.bytes_used();
        if (!Check(ValidateStream(buf, len), "ValidateStream(stage0 signature cache churn)")) {
          return false;
        }

        ps_creates += CountPixelShaderCreates(buf, len);
        if (ps_creates > 100) {
          done = true;
          break;
        }
      }
      if (done) {
        break;
      }
    }
    if (done) {
      break;
    }
  }

  if (!Check(ps_creates > 100, "created > 100 unique stage0 PS variants")) {
    return false;
  }

  // Validate that the signature->shader map does not retain pointers to evicted
  // shaders (use-after-free). All cached shader pointers must reference a live
  // entry in the bounded fixed-function PS variant array.
  std::lock_guard<std::mutex> lock(dev->mutex);
  std::unordered_set<const Shader*> live;
  for (const Shader* ps : dev->fixedfunc_ps_variants) {
    if (ps) {
      live.insert(ps);
    }
  }
  if (!Check(live.size() == 100, "stage0 PS variant array cache is capped at 100 entries")) {
    return false;
  }
  if (!Check(!dev->fixedfunc_ps_variant_cache.empty(), "stage0 signature cache populated")) {
    return false;
  }
  for (const auto& it : dev->fixedfunc_ps_variant_cache) {
    const uint64_t sig = it.first;
    const Shader* ps = it.second;
    if (!ps) {
      std::fprintf(stderr, "FAIL: stage0 signature cache maps sig=0x%llx to null shader\n",
                   static_cast<unsigned long long>(sig));
      return false;
    }
    if (live.find(ps) == live.end()) {
      std::fprintf(stderr, "FAIL: stage0 signature cache maps sig=0x%llx to non-live shader ptr=%p\n",
                   static_cast<unsigned long long>(sig),
                   reinterpret_cast<const void*>(ps));
      return false;
    }
  }
  return true;
}

bool TestTextureFactorRenderStateUpdatesPsConstantWhenUsed() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: select TFACTOR for both color and alpha so the fixed-function PS
  // references c255.
  if (!SetTextureStageState(/*stage=*/0, kD3dTssColorOp, kD3dTopSelectArg1, "SetTextureStageState(COLOROP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0, kD3dTssColorArg1, kD3dTaTFactor, "SetTextureStageState(COLORARG1=TFACTOR)")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaArg1, kD3dTaTFactor, "SetTextureStageState(ALPHAARG1=TFACTOR)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(initial tfactor draw)")) {
    return false;
  }

  // Isolate render-state-driven updates.
  dev->cmd.reset();

  const uint32_t tf = 0xFF3366CCu;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsTextureFactor, tf);
  if (!Check(hr == S_OK, "SetRenderState(TEXTUREFACTOR=0xFF3366CC)")) {
    return false;
  }
  // Setting the same value again should not re-upload c255.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsTextureFactor, tf);
  if (!Check(hr == S_OK, "SetRenderState(TEXTUREFACTOR=0xFF3366CC) again")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(texturefactor renderstate update)")) {
    return false;
  }

  const float expected_a = static_cast<float>((tf >> 24) & 0xFFu) * (1.0f / 255.0f);
  const float expected_r = static_cast<float>((tf >> 16) & 0xFFu) * (1.0f / 255.0f);
  const float expected_g = static_cast<float>((tf >> 8) & 0xFFu) * (1.0f / 255.0f);
  const float expected_b = static_cast<float>((tf >> 0) & 0xFFu) * (1.0f / 255.0f);
  const float expected_vec[4] = {expected_r, expected_g, expected_b, expected_a};

  size_t uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 255 || sc->vec4_count != 1) {
      continue;
    }
    const size_t need = sizeof(*sc) + sizeof(expected_vec);
    if (!Check(hdr->size_bytes >= need, "SET_SHADER_CONSTANTS_F contains payload (tfactor)")) {
      return false;
    }
    const auto* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
    if (!Check(std::fabs(payload[0] - expected_vec[0]) < 1e-6f &&
                   std::fabs(payload[1] - expected_vec[1]) < 1e-6f &&
                   std::fabs(payload[2] - expected_vec[2]) < 1e-6f &&
                   std::fabs(payload[3] - expected_vec[3]) < 1e-6f,
               "TFACTOR constant payload matches expected RGBA (render state update)")) {
      return false;
    }
    ++uploads;
  }
  if (!Check(uploads == 1, "TFACTOR constant upload emitted once for render-state updates")) {
    return false;
  }

  return true;
}

bool TestTextureFactorConstantReuploadAfterPsConstClobber() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShaderConstF != nullptr, "pfnSetShaderConstF is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: select TFACTOR for both color and alpha so the fixed-function PS
  // references c255.
  if (!SetTextureStageState(/*stage=*/0, kD3dTssColorOp, kD3dTopSelectArg1, "SetTextureStageState(COLOROP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0, kD3dTssColorArg1, kD3dTaTFactor, "SetTextureStageState(COLORARG1=TFACTOR)")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1)")) {
    return false;
  }
  if (!SetTextureStageState(/*stage=*/0, kD3dTssAlphaArg1, kD3dTaTFactor, "SetTextureStageState(ALPHAARG1=TFACTOR)")) {
    return false;
  }

  // Set TEXTUREFACTOR so the driver uploads c255 (and seeds the PS const cache).
  const uint32_t tf = 0xFF3366CCu;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsTextureFactor, tf);
  if (!Check(hr == S_OK, "SetRenderState(TEXTUREFACTOR=0xFF3366CC)")) {
    return false;
  }

  // Clobber c255 with a user PS constant write.
  const float junk[4] = {123.0f, 456.0f, 789.0f, 1011.0f};
  hr = cleanup.device_funcs.pfnSetShaderConstF(cleanup.hDevice, kD3dShaderStagePs, /*start_reg=*/255u, junk, /*vec4_count=*/1u);
  if (!Check(hr == S_OK, "SetShaderConstF(PS, c255 clobber)")) {
    return false;
  }

  // Capture only the draw-time restore.
  dev->cmd.reset();

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(after c255 clobber)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(tfactor clobber restore)")) {
    return false;
  }

  // Ensure the fixed-function PS references c255 so the restore is meaningful.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "tfactor clobber restore: PS bound")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E400FFu), "tfactor clobber restore: PS references c255")) {
      return false;
    }
  }

  const float expected_a = static_cast<float>((tf >> 24) & 0xFFu) * (1.0f / 255.0f);
  const float expected_r = static_cast<float>((tf >> 16) & 0xFFu) * (1.0f / 255.0f);
  const float expected_g = static_cast<float>((tf >> 8) & 0xFFu) * (1.0f / 255.0f);
  const float expected_b = static_cast<float>((tf >> 0) & 0xFFu) * (1.0f / 255.0f);
  const float expected_vec[4] = {expected_r, expected_g, expected_b, expected_a};

  size_t uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 255u || sc->vec4_count != 1u) {
      continue;
    }
    const size_t need = sizeof(*sc) + sizeof(expected_vec);
    if (!Check(hdr->size_bytes >= need, "tfactor clobber restore: SET_SHADER_CONSTANTS_F contains payload")) {
      return false;
    }
    const auto* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
    if (!Check(std::fabs(payload[0] - expected_vec[0]) < 1e-6f &&
                   std::fabs(payload[1] - expected_vec[1]) < 1e-6f &&
                   std::fabs(payload[2] - expected_vec[2]) < 1e-6f &&
                   std::fabs(payload[3] - expected_vec[3]) < 1e-6f,
               "tfactor clobber restore: payload matches expected RGBA")) {
      return false;
    }
    ++uploads;
  }
  if (!Check(uploads == 1, "tfactor clobber restore: c255 constant uploaded once")) {
    return false;
  }

  return true;
}

size_t CountVsConstantUploads(const uint8_t* buf,
                              size_t capacity,
                              uint32_t start_register,
                              uint32_t vec4_count) {
  size_t count = 0;
  for (const auto* hdr : CollectOpcodes(buf, capacity, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != start_register || sc->vec4_count != vec4_count) {
      continue;
    }
    ++count;
  }
  return count;
}

const float* FindVsConstantsPayload(const uint8_t* buf,
                                    size_t capacity,
                                    uint32_t start_register,
                                    uint32_t vec4_count) {
  for (const auto* hdr : CollectOpcodes(buf, capacity, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != start_register || sc->vec4_count != vec4_count) {
      continue;
    }
    const size_t need = sizeof(aerogpu_cmd_set_shader_constants_f) + static_cast<size_t>(vec4_count) * 4u * sizeof(float);
    if (hdr->size_bytes < need) {
      continue;
    }
    return reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
  }
  return nullptr;
}

bool TestFvfXyzNormalLightingSelectsLitVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormal);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL)")) {
    return false;
  }

  const VertexXyzNormal tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f},
  };

  // Lighting off: select the unlit "white diffuse" variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 0u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=FALSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormal));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL; lighting=off)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "VS bound (unlit)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalWhite),
               "VS bytecode == fixedfunc::kVsWvpPosNormalWhite (unlit)")) {
      return false;
    }
  }

  // Lighting on: select the lit variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormal));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL; lighting=on)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "VS bound (lit)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpLitPosNormal),
               "VS bytecode == fixedfunc::kVsWvpLitPosNormal (lit)")) {
      return false;
    }
    // Ensure the lit shader references the reserved lighting constant layout
    // (c208..c236) rather than the legacy c244+ range.
    if (!Check(ShaderReferencesConstRegister(dev->vs, kFixedfuncLightingStartRegister),
               "lit VS references lighting start register c208")) {
      return false;
    }
    if (!Check(!ShaderReferencesConstRegister(dev->vs, 244u), "lit VS does not reference legacy c244 layout")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzNormalTex1LightingSelectsLitVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|TEX1)")) {
    return false;
  }

  const VertexXyzNormalTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, /*u=*/0.0f, /*v=*/0.0f},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, /*u=*/1.0f, /*v=*/0.0f},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, /*u=*/0.0f, /*v=*/1.0f},
  };

  // Lighting off: select the unlit "white diffuse" variant that passes TEXCOORD0.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 0u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=FALSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|TEX1; lighting=off)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "VS bound (unlit)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalWhiteTex0),
               "VS bytecode == fixedfunc::kVsWvpPosNormalWhiteTex0 (unlit)")) {
      return false;
    }
  }

  // Lighting on: select the lit TEX1 variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|TEX1; lighting=on)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "VS bound (lit)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpLitPosNormalTex1),
               "VS bytecode == fixedfunc::kVsWvpLitPosNormalTex1 (lit)")) {
      return false;
    }
    if (!Check(ShaderReferencesConstRegister(dev->vs, kFixedfuncLightingStartRegister),
               "lit TEX1 VS references lighting start register c208")) {
      return false;
    }
    if (!Check(!ShaderReferencesConstRegister(dev->vs, 244u), "lit TEX1 VS does not reference legacy c244 layout")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzNormalEmitsLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormal);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Global ambient: blue (ARGB).
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF0000FFu);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=blue)")) {
    return false;
  }

  // Configure fixed-function lighting state via host-test entrypoints (portable
  // builds do not expose SetLight/SetMaterial in the device vtable).
  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.5f, -0.25f, 1.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.5f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }
  D3DMATERIAL9 mat{};
  mat.Diffuse = {0.5f, 0.5f, 0.5f, 1.0f};
  mat.Ambient = {0.25f, 0.25f, 0.25f, 1.0f};
  mat.Emissive = {0.125f, 0.25f, 0.5f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  const VertexXyzNormal tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f},
  };

  dev->cmd.reset();

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormal));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL; lighting constants)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|NORMAL lighting constants)")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count) == 1,
             "WVP constant upload emitted once")) {
    return false;
  }
  // Fixed-function lighting constants are uploaded as a single contiguous block.
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "lighting constant upload emitted once")) {
    return false;
  }

  const float* wvp = FindVsConstantsPayload(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count);
  if (!Check(wvp != nullptr, "WVP constants payload present")) {
    return false;
  }
  // Default transforms are identity; spot-check a few stable lanes.
  if (!Check(wvp[0] == 1.0f && wvp[5] == 1.0f && wvp[10] == 1.0f && wvp[15] == 1.0f,
             "WVP constants look like an identity matrix")) {
    return false;
  }

  const float* lighting = FindVsConstantsPayload(buf,
                                                 len,
                                                 kFixedfuncLightingStartRegister,
                                                 kFixedfuncLightingVec4Count);
  if (!Check(lighting != nullptr, "lighting constants payload present")) {
    return false;
  }
  // c236: global ambient (blue ARGB -> RGBA {0,0,1,1}).
  if (!Check(lighting[kFixedfuncLightingGlobalAmbientRel * 4 + 0] == 0.0f &&
             lighting[kFixedfuncLightingGlobalAmbientRel * 4 + 1] == 0.0f &&
             lighting[kFixedfuncLightingGlobalAmbientRel * 4 + 2] == 1.0f &&
             lighting[kFixedfuncLightingGlobalAmbientRel * 4 + 3] == 1.0f,
             "global ambient constant reflects D3DRS_AMBIENT")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalTex1EmitsLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|TEX1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Global ambient: blue (ARGB).
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF0000FFu);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=blue)")) {
    return false;
  }

  // Configure fixed-function lighting state via host-test entrypoints (portable
  // builds do not expose SetLight/SetMaterial in the device vtable).
  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.25f, 0.5f, -1.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.5f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }
  D3DMATERIAL9 mat{};
  mat.Diffuse = {0.5f, 0.5f, 0.5f, 1.0f};
  mat.Ambient = {0.25f, 0.25f, 0.25f, 1.0f};
  mat.Emissive = {0.125f, 0.25f, 0.5f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  const VertexXyzNormalTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, /*u=*/0.0f, /*v=*/0.0f},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, /*u=*/1.0f, /*v=*/0.0f},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, /*u=*/0.0f, /*v=*/1.0f},
  };

  dev->cmd.reset();

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|TEX1; lighting constants)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|NORMAL|TEX1 lighting constants)")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count) == 1,
             "WVP constant upload emitted once (TEX1)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "lighting constant upload emitted once (TEX1)")) {
    return false;
  }

  const float* lighting = FindVsConstantsPayload(buf,
                                                 len,
                                                 kFixedfuncLightingStartRegister,
                                                 kFixedfuncLightingVec4Count);
  if (!Check(lighting != nullptr, "lighting constants payload present (TEX1)")) {
    return false;
  }
  if (!Check(lighting[kFixedfuncLightingGlobalAmbientRel * 4 + 0] == 0.0f &&
             lighting[kFixedfuncLightingGlobalAmbientRel * 4 + 1] == 0.0f &&
             lighting[kFixedfuncLightingGlobalAmbientRel * 4 + 2] == 1.0f &&
             lighting[kFixedfuncLightingGlobalAmbientRel * 4 + 3] == 1.0f,
             "global ambient constant reflects D3DRS_AMBIENT (TEX1)")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseLightingSelectsLitVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  // Lighting off: select the unlit variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 0u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=FALSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|DIFFUSE; lighting=off)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "VS bound (unlit)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalDiffuse),
               "VS bytecode == fixedfunc::kVsWvpPosNormalDiffuse (unlit)")) {
      return false;
    }
  }

  // Lighting on: select the lit variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|DIFFUSE; lighting=on)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "VS bound (lit)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpLitPosNormalDiffuse),
               "VS bytecode == fixedfunc::kVsWvpLitPosNormalDiffuse (lit)")) {
      return false;
    }
    if (!Check(ShaderReferencesConstRegister(dev->vs, kFixedfuncLightingStartRegister),
               "lit normal+diffuse VS references lighting start register c208")) {
      return false;
    }
    if (!Check(!ShaderReferencesConstRegister(dev->vs, 244u),
               "lit normal+diffuse VS does not reference legacy c244 layout")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzNormalDiffuseTex1LightingSelectsLitVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE|TEX1)")) {
    return false;
  }

  const VertexXyzNormalDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu, /*u=*/0.0f, /*v=*/0.0f},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu, /*u=*/1.0f, /*v=*/0.0f},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu, /*u=*/0.0f, /*v=*/1.0f},
  };

  // Lighting off: select the unlit variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 0u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=FALSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|DIFFUSE|TEX1; lighting=off)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "VS bound (unlit)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalDiffuseTex1),
               "VS bytecode == fixedfunc::kVsWvpPosNormalDiffuseTex1 (unlit)")) {
      return false;
    }
  }

  // Lighting on: select the lit TEX1 variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|DIFFUSE|TEX1; lighting=on)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "VS bound (lit)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpLitPosNormalDiffuseTex1),
               "VS bytecode == fixedfunc::kVsWvpLitPosNormalDiffuseTex1 (lit)")) {
      return false;
    }
    // Ensure the lit shader references the reserved lighting constant layout
    // (c208..c236) rather than the legacy c244+ range.
    if (!Check(ShaderReferencesConstRegister(dev->vs, kFixedfuncLightingStartRegister),
               "lit VS references lighting start register c208")) {
      return false;
    }
    if (!Check(!ShaderReferencesConstRegister(dev->vs, 244u), "lit VS does not reference legacy c244 layout")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzNormalDiffuseTex1EmitsLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE|TEX1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Global ambient: blue (ARGB).
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF0000FFu);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=blue)")) {
    return false;
  }

  // Configure fixed-function lighting state via host-test entrypoints (portable
  // builds do not expose SetLight/SetMaterial in the device vtable).
  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.25f, 0.5f, -1.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.5f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }
  D3DMATERIAL9 mat{};
  mat.Diffuse = {0.5f, 0.5f, 0.5f, 1.0f};
  mat.Ambient = {0.25f, 0.25f, 0.25f, 1.0f};
  mat.Emissive = {0.125f, 0.25f, 0.5f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  const VertexXyzNormalDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu, /*u=*/0.0f, /*v=*/0.0f},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu, /*u=*/1.0f, /*v=*/0.0f},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu, /*u=*/0.0f, /*v=*/1.0f},
  };

  dev->cmd.reset();

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|DIFFUSE|TEX1; lighting constants)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "VS bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpLitPosNormalDiffuseTex1),
               "VS bytecode == fixedfunc::kVsWvpLitPosNormalDiffuseTex1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(XYZ|NORMAL|DIFFUSE|TEX1 lighting constants)")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count) == 1,
             "WVP constant upload emitted once (NORMAL|DIFFUSE|TEX1)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count) == 1,
             "lighting constant upload emitted once (NORMAL|DIFFUSE|TEX1)")) {
    return false;
  }

  const float* lighting = FindVsConstantsPayload(buf, len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count);
  if (!Check(lighting != nullptr, "lighting constants payload present (NORMAL|DIFFUSE|TEX1)")) {
    return false;
  }
  if (!Check(lighting[kFixedfuncLightingGlobalAmbientRel * 4 + 0] == 0.0f &&
                 lighting[kFixedfuncLightingGlobalAmbientRel * 4 + 1] == 0.0f &&
                 lighting[kFixedfuncLightingGlobalAmbientRel * 4 + 2] == 1.0f &&
                 lighting[kFixedfuncLightingGlobalAmbientRel * 4 + 3] == 1.0f,
             "global ambient constant reflects D3DRS_AMBIENT (NORMAL|DIFFUSE|TEX1)")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseEmitsLightingConstantsAndTracksDirty() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Global ambient: blue (ARGB).
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF0000FFu);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=blue)")) {
    return false;
  }

  // Configure fixed-function lighting state via host-test entrypoints (portable
  // builds do not expose SetLight/SetMaterial in the device vtable).
  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.0f, 0.0f, -1.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.5f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }
  D3DMATERIAL9 mat{};
  mat.Diffuse = {0.5f, 0.5f, 0.5f, 1.0f};
  mat.Ambient = {0.25f, 0.25f, 0.25f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  // ---------------------------------------------------------------------------
  // First draw: emits the lighting constant block once.
  // ---------------------------------------------------------------------------
  dev->cmd.reset();

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(lighting constants; first)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(lighting constants; first)")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "lighting constant upload emitted once")) {
    return false;
  }

  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "lighting constant payload present")) {
    return false;
  }

  constexpr size_t kLightingFloatCount = static_cast<size_t>(kFixedfuncLightingVec4Count) * 4u;
  const float expected[kLightingFloatCount] = {
      // c208..c210: identity world*view columns 0..2 (w contains translation).
      1.0f, 0.0f, 0.0f, 0.0f,
      0.0f, 1.0f, 0.0f, 0.0f,
      0.0f, 0.0f, 1.0f, 0.0f,

      // Directional slot0 (c211..c213): light direction (vertex->light), diffuse, ambient.
      0.0f, 0.0f, 1.0f, 0.0f,
      1.0f, 0.0f, 0.0f, 1.0f,
      0.0f, 0.5f, 0.0f, 1.0f,

      // Directional slot1..slot3: unused.
      0.0f, 0.0f, 0.0f, 0.0f, // c214
      0.0f, 0.0f, 0.0f, 0.0f, // c215
      0.0f, 0.0f, 0.0f, 0.0f, // c216
      0.0f, 0.0f, 0.0f, 0.0f, // c217
      0.0f, 0.0f, 0.0f, 0.0f, // c218
      0.0f, 0.0f, 0.0f, 0.0f, // c219
      0.0f, 0.0f, 0.0f, 0.0f, // c220
      0.0f, 0.0f, 0.0f, 0.0f, // c221
      0.0f, 0.0f, 0.0f, 0.0f, // c222

      // Point slot0..slot1: unused.
      0.0f, 0.0f, 0.0f, 0.0f, // c223
      0.0f, 0.0f, 0.0f, 0.0f, // c224
      0.0f, 0.0f, 0.0f, 0.0f, // c225
      0.0f, 0.0f, 0.0f, 0.0f, // c226
      0.0f, 0.0f, 0.0f, 0.0f, // c227
      0.0f, 0.0f, 0.0f, 0.0f, // c228
      0.0f, 0.0f, 0.0f, 0.0f, // c229
      0.0f, 0.0f, 0.0f, 0.0f, // c230
      0.0f, 0.0f, 0.0f, 0.0f, // c231
      0.0f, 0.0f, 0.0f, 0.0f, // c232

      // c233..c235: material diffuse/ambient/emissive.
      0.5f, 0.5f, 0.5f, 1.0f,
      0.25f, 0.25f, 0.25f, 1.0f,
      0.0f, 0.0f, 0.0f, 0.0f,

      // c236: global ambient (ARGB blue -> RGBA {0,0,1,1}).
      0.0f, 0.0f, 1.0f, 1.0f,
  };
  for (size_t i = 0; i < kLightingFloatCount; ++i) {
    // Compare numerically (treat -0.0 == 0.0) instead of bitwise comparing.
    if (payload[i] != expected[i]) {
      std::fprintf(stderr, "Lighting constants mismatch:\n");
      for (size_t j = 0; j < kLightingFloatCount; ++j) {
        std::fprintf(stderr, "  [%02zu] got=%f expected=%f\n", j, payload[j], expected[j]);
      }
      return Check(false, "lighting constant payload matches expected values");
    }
  }

  // ---------------------------------------------------------------------------
  // Second draw: should not re-upload lighting constants if nothing changed.
  // ---------------------------------------------------------------------------
  dev->cmd.reset();

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(lighting constants; second)")) {
    return false;
  }

  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(lighting constants; second)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 0,
             "lighting constant upload is skipped when not dirty")) {
    return false;
  }

  // ---------------------------------------------------------------------------
  // Enable a second light: should mark the lighting block dirty and re-upload.
  // (Portable builds use the host-side SetLight/LightEnable entrypoints.)
  // ---------------------------------------------------------------------------
  D3DLIGHT9 light_slot1{};
  light_slot1.Type = D3DLIGHT_DIRECTIONAL;
  light_slot1.Direction = {0.0f, 0.0f, -1.0f};
  light_slot1.Diffuse = {0.0f, 1.0f, 0.0f, 1.0f};
  light_slot1.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/1, &light_slot1);
  if (!Check(hr == S_OK, "SetLight(1)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/1, TRUE);
  if (!Check(hr == S_OK, "LightEnable(1, TRUE)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(lighting constants; light1 enabled)")) {
    return false;
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(lighting constants; light1 enabled)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "lighting constant upload re-emitted after enabling light1")) {
    return false;
  }
  payload = FindVsConstantsPayload(buf,
                                   len,
                                   kFixedfuncLightingStartRegister,
                                   kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "lighting payload present (light1 enabled)")) {
    return false;
  }
  constexpr uint32_t kLight1DiffuseRel = (215u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kLight1DiffuseRel * 4 + 0] == 0.0f && payload[kLight1DiffuseRel * 4 + 1] == 1.0f &&
             payload[kLight1DiffuseRel * 4 + 2] == 0.0f && payload[kLight1DiffuseRel * 4 + 3] == 1.0f,
             "directional light1 diffuse is packed into slot1")) {
    return false;
  }

  // ---------------------------------------------------------------------------
  // Change D3DRS_AMBIENT: should mark the lighting block dirty and re-upload.
  // ---------------------------------------------------------------------------
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=red)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(lighting constants; ambient changed)")) {
    return false;
  }

  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(lighting constants; ambient changed)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "lighting constant upload re-emitted after ambient change")) {
    return false;
  }
  payload = FindVsConstantsPayload(buf,
                                   len,
                                   kFixedfuncLightingStartRegister,
                                   kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "lighting payload present (ambient changed)")) {
    return false;
  }
  if (!Check(payload[kFixedfuncLightingGlobalAmbientRel * 4 + 0] == 1.0f &&
             payload[kFixedfuncLightingGlobalAmbientRel * 4 + 1] == 0.0f &&
             payload[kFixedfuncLightingGlobalAmbientRel * 4 + 2] == 0.0f &&
             payload[kFixedfuncLightingGlobalAmbientRel * 4 + 3] == 1.0f,
             "global ambient constant reflects new D3DRS_AMBIENT value")) {
    return false;
  }

  // ---------------------------------------------------------------------------
  // Change light direction: re-upload should reflect the new direction.
  // ---------------------------------------------------------------------------
  D3DLIGHT9 light1 = light0;
  light1.Direction = {0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light1);
  if (!Check(hr == S_OK, "SetLight(direction changed)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(lighting constants; light direction changed)")) {
    return false;
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(lighting constants; light direction changed)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "lighting constant upload re-emitted after light direction change")) {
    return false;
  }
  payload = FindVsConstantsPayload(buf,
                                   len,
                                   kFixedfuncLightingStartRegister,
                                   kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "lighting payload present (light direction changed)")) {
    return false;
  }
  constexpr uint32_t kLight0DirRel = (211u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kLight0DirRel * 4 + 0] == 0.0f && payload[kLight0DirRel * 4 + 1] == 0.0f &&
             payload[kLight0DirRel * 4 + 2] == -1.0f && payload[kLight0DirRel * 4 + 3] == 0.0f,
             "light direction constant reflects updated light direction")) {
    return false;
  }

  // ---------------------------------------------------------------------------
  // Disable light1: should mark the lighting block dirty and clear slot1.
  // ---------------------------------------------------------------------------
  hr = device_light_enable(cleanup.hDevice, /*index=*/1, FALSE);
  if (!Check(hr == S_OK, "LightEnable(1, FALSE)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(lighting constants; light1 disabled)")) {
    return false;
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(lighting constants; light1 disabled)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "lighting constant upload re-emitted after disabling light1")) {
    return false;
  }
  payload = FindVsConstantsPayload(buf,
                                   len,
                                   kFixedfuncLightingStartRegister,
                                   kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "lighting payload present (light1 disabled)")) {
    return false;
  }
  if (!Check(payload[kLight1DiffuseRel * 4 + 0] == 0.0f &&
             payload[kLight1DiffuseRel * 4 + 1] == 0.0f &&
             payload[kLight1DiffuseRel * 4 + 2] == 0.0f &&
             payload[kLight1DiffuseRel * 4 + 3] == 0.0f,
             "directional light1 diffuse cleared when the light is disabled")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseRedundantRenderStateDoesNotReuploadLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  dev->cmd.reset();
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  // Keep global ambient deterministic.
  constexpr uint32_t kAmbientBlack = 0xFF000000u;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, kAmbientBlack);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.0f, 0.0f, -1.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(lighting constants; initial)")) {
    return false;
  }

  // Benign redundant render-state writes that should not cause re-upload of the
  // fixed-function lighting constant block.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, kAmbientBlack);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black redundant)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE redundant)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(lighting constants; after redundant render state)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(redundant render state)")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "redundant render state: lighting constants uploaded once")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseRedundantDirtyTriggersDoNotReuploadLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF000000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  // Identity transforms.
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &identity);
  if (!Check(hr == S_OK, "SetTransform(WORLD0 identity)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW identity)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION identity)")) {
    return false;
  }

  // Configure a single directional light + material.
  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.0f, 0.0f, -1.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  // Capture both draws in one command stream so we can count lighting uploads
  // across redundant "dirty" state changes.
  dev->cmd.reset();

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(lighting constants; initial)")) {
    return false;
  }

  // These API calls all conservatively mark the fixed-function lighting constant
  // block dirty, even if the state didn't actually change. The driver should
  // avoid re-uploading the lighting constants when the computed constant block
  // is identical to the cached VS constant range.
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW identity redundant)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE redundant)")) {
    return false;
  }
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0 redundant)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE redundant)")) {
    return false;
  }
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial(redundant)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(lighting constants; after redundant dirty triggers)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(redundant dirty triggers)")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "redundant dirty triggers: lighting constants uploaded once")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseNormalizesDirectionalLightDirection() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  // Keep global ambient deterministic.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF000000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  // Provide a non-unit direction; the driver should normalize it when packing
  // the vertex->light direction constant.
  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {3.0f, 4.0f, 0.0f}; // length 5
  light0.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(direction normalization)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(direction normalization)")) {
    return false;
  }

  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "direction normalization: lighting payload present")) {
    return false;
  }

  constexpr uint32_t kLight0DirRel = (211u - kFixedfuncLightingStartRegister);
  const float x = payload[kLight0DirRel * 4 + 0];
  const float y = payload[kLight0DirRel * 4 + 1];
  const float z = payload[kLight0DirRel * 4 + 2];
  const float w = payload[kLight0DirRel * 4 + 3];

  // D3D direction is the direction the light rays travel. The shader expects
  // vertex->light, so the driver negates it. For (3,4,0), normalized is (0.6,0.8,0),
  // so the expected vertex->light is (-0.6,-0.8,0).
  if (!Check(std::fabs(x - (-0.6f)) < 1e-6f &&
                 std::fabs(y - (-0.8f)) < 1e-6f &&
                 std::fabs(z - 0.0f) < 1e-6f &&
                 std::fabs(w - 0.0f) < 1e-6f,
             "direction normalization: packed dir == normalized vertex->light")) {
    return false;
  }

  const float len2 = x * x + y * y + z * z;
  if (!Check(std::fabs(len2 - 1.0f) < 1e-5f, "direction normalization: packed direction is unit length")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseNormalizesDirectionalLightDirectionAfterViewTransform() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF000000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  // Use a VIEW matrix with non-unit scale so direction normalization must occur
  // after the view transform (not just on the input direction).
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX view = identity;
  view.m[0][0] = 2.0f; // scale X
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &identity);
  if (!Check(hr == S_OK, "SetTransform(WORLD0 identity)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION identity)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &view);
  if (!Check(hr == S_OK, "SetTransform(VIEW scaled)")) {
    return false;
  }

  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {1.0f, 0.0f, 0.0f};
  light0.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(view-scaled direction normalization)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(view-scaled direction normalization)")) {
    return false;
  }

  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "view-scaled normalization: lighting payload present")) {
    return false;
  }

  constexpr uint32_t kLight0DirRel = (211u - kFixedfuncLightingStartRegister);
  const float x = payload[kLight0DirRel * 4 + 0];
  const float y = payload[kLight0DirRel * 4 + 1];
  const float z = payload[kLight0DirRel * 4 + 2];
  const float w = payload[kLight0DirRel * 4 + 3];

  // With VIEW scaling X by 2, the unnormalized view-space direction is (-2,0,0),
  // but the driver should renormalize it to unit length (-1,0,0).
  if (!Check(std::fabs(x - (-1.0f)) < 1e-6f &&
                 std::fabs(y - 0.0f) < 1e-6f &&
                 std::fabs(z - 0.0f) < 1e-6f &&
                 std::fabs(w - 0.0f) < 1e-6f,
             "view-scaled normalization: packed dir is renormalized after view transform")) {
    return false;
  }
  const float len2 = x * x + y * y + z * z;
  if (!Check(std::fabs(len2 - 1.0f) < 1e-5f, "view-scaled normalization: packed direction is unit length")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseGlobalAmbientPreservesAlphaChannel() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Use a non-opaque ambient color to validate alpha handling in the ARGB->RGBA
  // conversion used by fixed-function lighting.
  constexpr uint32_t kAmbientGreenAlpha0 = 0x0000FF00u; // A=0, R=0, G=255, B=0
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, kAmbientGreenAlpha0);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=green alpha0)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(global ambient alpha)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(global ambient alpha)")) {
    return false;
  }

  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "global ambient alpha: lighting payload present")) {
    return false;
  }

  if (!Check(payload[kFixedfuncLightingGlobalAmbientRel * 4 + 0] == 0.0f &&
                 payload[kFixedfuncLightingGlobalAmbientRel * 4 + 1] == 1.0f &&
                 payload[kFixedfuncLightingGlobalAmbientRel * 4 + 2] == 0.0f &&
                 payload[kFixedfuncLightingGlobalAmbientRel * 4 + 3] == 0.0f,
             "global ambient alpha: ARGB->RGBA conversion preserves alpha channel")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseLightingOffDoesNotUploadLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Use an FVF that carries normals and would otherwise use the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }

  // Lighting disabled: must not upload the fixed-function lighting constant range.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 0u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=FALSE)")) {
    return false;
  }
  // Still set AMBIENT to ensure the lighting block would differ if it were uploaded.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF0000FFu);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=blue)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(lighting disabled)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "VS bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalDiffuse),
               "lighting off: selected unlit VS variant")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(lighting disabled)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count) == 0,
             "lighting off: does not upload fixed-function lighting constants")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseProjectionChangeReuploadsWvpButNotLightingConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF000000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.0f, 0.0f, -1.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  // First draw: emits both WVP and lighting constant uploads.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(initial)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(initial)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count) == 1,
             "initial: emits WVP constant upload")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count) == 1,
             "initial: emits lighting constant upload")) {
    return false;
  }

  // Change only PROJECTION. This must re-upload the WVP constants but must NOT
  // re-upload the lighting constants (lighting depends only on WORLD0 and VIEW).
  D3DMATRIX proj = identity;
  proj.m[0][0] = 2.0f;

  // Important: SetTransform may upload the WVP constants eagerly for fixed-function
  // draws. Capture the SetTransform and subsequent draw in the same command stream
  // so we can assert that:
  // - WVP is uploaded exactly once,
  // - lighting constants are not re-uploaded.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &proj);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION scaled)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(after projection change)")) {
    return false;
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(after projection change)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncMatrixStartRegister, kFixedfuncMatrixVec4Count) == 1,
             "projection change: re-uploads WVP constants")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf, len, kFixedfuncLightingStartRegister, kFixedfuncLightingVec4Count) == 0,
             "projection change: does not re-upload lighting constants")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseDisablingLight0ShiftsPackedLights() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  // Keep global ambient deterministic.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF000000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  // Configure two directional lights via the host-test entrypoints.
  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.0f, 0.0f, -1.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DLIGHT9 light1 = light0;
  light1.Diffuse = {0.0f, 1.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/1, &light1);
  if (!Check(hr == S_OK, "SetLight(1)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/1, TRUE);
  if (!Check(hr == S_OK, "LightEnable(1, TRUE)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  // Baseline: with both lights enabled, light0 is packed into slot0 and light1 into slot1.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(baseline; two lights)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(baseline; two lights)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "baseline: lighting constant upload emitted once")) {
    return false;
  }
  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "baseline: lighting constants payload present")) {
    return false;
  }
  constexpr uint32_t kLight0DiffuseRel = (212u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kLight1DiffuseRel = (215u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kLight0DiffuseRel * 4 + 0] == 1.0f && payload[kLight0DiffuseRel * 4 + 1] == 0.0f &&
             payload[kLight0DiffuseRel * 4 + 2] == 0.0f && payload[kLight0DiffuseRel * 4 + 3] == 1.0f,
             "baseline: slot0 diffuse == light0 (red)")) {
    return false;
  }
  if (!Check(payload[kLight1DiffuseRel * 4 + 0] == 0.0f && payload[kLight1DiffuseRel * 4 + 1] == 1.0f &&
             payload[kLight1DiffuseRel * 4 + 2] == 0.0f && payload[kLight1DiffuseRel * 4 + 3] == 1.0f,
             "baseline: slot1 diffuse == light1 (green)")) {
    return false;
  }

  // Disable light0: light1 should shift down to slot0 and slot1 should become unused (zero).
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, FALSE);
  if (!Check(hr == S_OK, "LightEnable(0, FALSE)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(light0 disabled)")) {
    return false;
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(light0 disabled)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "lighting constant upload re-emitted after disabling light0")) {
    return false;
  }
  payload = FindVsConstantsPayload(buf,
                                   len,
                                   kFixedfuncLightingStartRegister,
                                   kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "lighting constants payload present (light0 disabled)")) {
    return false;
  }
  if (!Check(payload[kLight0DiffuseRel * 4 + 0] == 0.0f && payload[kLight0DiffuseRel * 4 + 1] == 1.0f &&
             payload[kLight0DiffuseRel * 4 + 2] == 0.0f && payload[kLight0DiffuseRel * 4 + 3] == 1.0f,
             "slot0 diffuse shifted to light1 after disabling light0")) {
    return false;
  }
  if (!Check(payload[kLight1DiffuseRel * 4 + 0] == 0.0f && payload[kLight1DiffuseRel * 4 + 1] == 0.0f &&
             payload[kLight1DiffuseRel * 4 + 2] == 0.0f && payload[kLight1DiffuseRel * 4 + 3] == 0.0f,
             "slot1 diffuse cleared after light0 disable shift")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffusePacksMultipleLights() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Configure two enabled directional lights via host-test entrypoints.
  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.0f, 0.0f, -1.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DLIGHT9 light1 = light0;
  light1.Diffuse = {0.0f, 1.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/1, &light1);
  if (!Check(hr == S_OK, "SetLight(1)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/1, TRUE);
  if (!Check(hr == S_OK, "LightEnable(1, TRUE)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(two directional lights)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(two directional lights)")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "two lights: lighting constant upload emitted once")) {
    return false;
  }
  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "two lights: payload present")) {
    return false;
  }

  // Slot1 diffuse (c215) should reflect the second directional light's diffuse.
  constexpr uint32_t kLight1DiffuseRel = (215u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kLight1DiffuseRel * 4 + 0] == 0.0f &&
                 payload[kLight1DiffuseRel * 4 + 1] == 1.0f &&
                 payload[kLight1DiffuseRel * 4 + 2] == 0.0f &&
                 payload[kLight1DiffuseRel * 4 + 3] == 1.0f,
             "two lights: second directional light diffuse packed into slot1")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffusePacksPointLightConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Configure a single point light (packed into point slot0: c223..c227).
  D3DLIGHT9 light{};
  light.Type = D3DLIGHT_POINT;
  light.Position = {1.0f, 2.0f, 3.0f};
  light.Diffuse = {0.25f, 0.5f, 0.75f, 1.0f};
  light.Ambient = {0.0f, 0.25f, 0.0f, 1.0f};
  light.Attenuation0 = 2.0f; // inv_att0 = 0.5
  light.Range = 4.0f;        // inv_range2 = 1/16 = 0.0625
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light);
  if (!Check(hr == S_OK, "SetLight(point0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(point0, TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(point light)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(point light)")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "point light: lighting constant upload emitted once")) {
    return false;
  }
  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "point light: payload present")) {
    return false;
  }

  constexpr uint32_t kPoint0PosRel = (223u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint0DiffuseRel = (224u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint0AmbientRel = (225u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint0InvAtt0Rel = (226u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint0InvRange2Rel = (227u - kFixedfuncLightingStartRegister);

  if (!Check(payload[kPoint0PosRel * 4 + 0] == 1.0f &&
             payload[kPoint0PosRel * 4 + 1] == 2.0f &&
             payload[kPoint0PosRel * 4 + 2] == 3.0f &&
             payload[kPoint0PosRel * 4 + 3] == 1.0f,
             "point light: c223 packs view-space position")) {
    return false;
  }
  if (!Check(payload[kPoint0DiffuseRel * 4 + 0] == 0.25f &&
             payload[kPoint0DiffuseRel * 4 + 1] == 0.5f &&
             payload[kPoint0DiffuseRel * 4 + 2] == 0.75f &&
             payload[kPoint0DiffuseRel * 4 + 3] == 1.0f,
             "point light: c224 packs diffuse")) {
    return false;
  }
  if (!Check(payload[kPoint0AmbientRel * 4 + 0] == 0.0f &&
             payload[kPoint0AmbientRel * 4 + 1] == 0.25f &&
             payload[kPoint0AmbientRel * 4 + 2] == 0.0f &&
             payload[kPoint0AmbientRel * 4 + 3] == 1.0f,
             "point light: c225 packs ambient")) {
    return false;
  }
  if (!Check(payload[kPoint0InvAtt0Rel * 4 + 0] == 0.5f &&
             payload[kPoint0InvAtt0Rel * 4 + 1] == 0.5f &&
             payload[kPoint0InvAtt0Rel * 4 + 2] == 0.5f &&
             payload[kPoint0InvAtt0Rel * 4 + 3] == 0.5f,
             "point light: c226 packs inv_att0")) {
    return false;
  }
  if (!Check(payload[kPoint0InvRange2Rel * 4 + 0] == 0.0625f &&
             payload[kPoint0InvRange2Rel * 4 + 1] == 0.0625f &&
             payload[kPoint0InvRange2Rel * 4 + 2] == 0.0625f &&
             payload[kPoint0InvRange2Rel * 4 + 3] == 0.0625f,
             "point light: c227 packs inv_range2")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffusePointLightAtt0AndRangeFallbacks() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Configure a single point light with degenerate attenuation/range. The driver
  // clamps these to safe defaults to avoid INF/NaN constants.
  D3DLIGHT9 light{};
  light.Type = D3DLIGHT_POINT;
  light.Position = {1.0f, 2.0f, 3.0f};
  light.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  light.Attenuation0 = 0.0f; // clamped to 1.0 => inv_att0 = 1.0
  light.Range = 0.0f;        // clamped => inv_range2 = 0.0
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light);
  if (!Check(hr == S_OK, "SetLight(point0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(point0, TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(point light att0/range fallback)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(point light att0/range fallback)")) {
    return false;
  }

  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "lighting payload present")) {
    return false;
  }

  constexpr uint32_t kPoint0InvAtt0Rel = (226u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint0InvRange2Rel = (227u - kFixedfuncLightingStartRegister);

  if (!Check(payload[kPoint0InvAtt0Rel * 4 + 0] == 1.0f &&
             payload[kPoint0InvAtt0Rel * 4 + 1] == 1.0f &&
             payload[kPoint0InvAtt0Rel * 4 + 2] == 1.0f &&
             payload[kPoint0InvAtt0Rel * 4 + 3] == 1.0f,
             "point light: inv_att0 falls back to 1.0 when att0 <= 0")) {
    return false;
  }
  if (!Check(payload[kPoint0InvRange2Rel * 4 + 0] == 0.0f &&
             payload[kPoint0InvRange2Rel * 4 + 1] == 0.0f &&
             payload[kPoint0InvRange2Rel * 4 + 2] == 0.0f &&
             payload[kPoint0InvRange2Rel * 4 + 3] == 0.0f,
             "point light: inv_range2 falls back to 0.0 when range <= 0")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseDisablingPointLight0ShiftsPackedPointLights() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  // Keep global ambient deterministic.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF000000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  // Configure two enabled point lights (packed into point slot0 and slot1).
  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_POINT;
  light0.Position = {1.0f, 2.0f, 3.0f};
  light0.Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  light0.Attenuation0 = 1.0f;
  light0.Range = 2.0f;
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(point0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(point0, TRUE)")) {
    return false;
  }

  D3DLIGHT9 light1 = light0;
  light1.Position = {4.0f, 5.0f, 6.0f};
  light1.Diffuse = {0.0f, 1.0f, 0.0f, 1.0f};
  light1.Attenuation0 = 2.0f;
  light1.Range = 4.0f;
  hr = device_set_light(cleanup.hDevice, /*index=*/1, &light1);
  if (!Check(hr == S_OK, "SetLight(point1)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/1, TRUE);
  if (!Check(hr == S_OK, "LightEnable(point1, TRUE)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  // Baseline: with both lights enabled, light0 is packed into point slot0 and light1 into point slot1.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(baseline; two point lights)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(baseline; two point lights)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "baseline: lighting constant upload emitted once")) {
    return false;
  }
  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "baseline: lighting constants payload present")) {
    return false;
  }

  constexpr uint32_t kPoint0PosRel = (223u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint0DiffuseRel = (224u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint1PosRel = (228u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint1DiffuseRel = (229u - kFixedfuncLightingStartRegister);

  if (!Check(payload[kPoint0PosRel * 4 + 0] == 1.0f && payload[kPoint0PosRel * 4 + 1] == 2.0f &&
             payload[kPoint0PosRel * 4 + 2] == 3.0f && payload[kPoint0PosRel * 4 + 3] == 1.0f,
             "baseline: point slot0 position == light0")) {
    return false;
  }
  if (!Check(payload[kPoint0DiffuseRel * 4 + 0] == 1.0f && payload[kPoint0DiffuseRel * 4 + 1] == 0.0f &&
             payload[kPoint0DiffuseRel * 4 + 2] == 0.0f && payload[kPoint0DiffuseRel * 4 + 3] == 1.0f,
             "baseline: point slot0 diffuse == light0 (red)")) {
    return false;
  }
  if (!Check(payload[kPoint1PosRel * 4 + 0] == 4.0f && payload[kPoint1PosRel * 4 + 1] == 5.0f &&
             payload[kPoint1PosRel * 4 + 2] == 6.0f && payload[kPoint1PosRel * 4 + 3] == 1.0f,
             "baseline: point slot1 position == light1")) {
    return false;
  }
  if (!Check(payload[kPoint1DiffuseRel * 4 + 0] == 0.0f && payload[kPoint1DiffuseRel * 4 + 1] == 1.0f &&
             payload[kPoint1DiffuseRel * 4 + 2] == 0.0f && payload[kPoint1DiffuseRel * 4 + 3] == 1.0f,
             "baseline: point slot1 diffuse == light1 (green)")) {
    return false;
  }

  // Disable point light0: point light1 should shift down to point slot0 and point slot1 should become unused (zero).
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, FALSE);
  if (!Check(hr == S_OK, "LightEnable(point0, FALSE)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(point0 disabled)")) {
    return false;
  }
  dev->cmd.finalize();
  buf = dev->cmd.data();
  len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(point0 disabled)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "lighting constant upload re-emitted after disabling point0")) {
    return false;
  }
  payload = FindVsConstantsPayload(buf,
                                  len,
                                  kFixedfuncLightingStartRegister,
                                  kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "lighting constants payload present (point0 disabled)")) {
    return false;
  }

  if (!Check(payload[kPoint0PosRel * 4 + 0] == 4.0f && payload[kPoint0PosRel * 4 + 1] == 5.0f &&
             payload[kPoint0PosRel * 4 + 2] == 6.0f && payload[kPoint0PosRel * 4 + 3] == 1.0f,
             "point slot0 position shifted to light1 after disabling point0")) {
    return false;
  }
  if (!Check(payload[kPoint0DiffuseRel * 4 + 0] == 0.0f && payload[kPoint0DiffuseRel * 4 + 1] == 1.0f &&
             payload[kPoint0DiffuseRel * 4 + 2] == 0.0f && payload[kPoint0DiffuseRel * 4 + 3] == 1.0f,
             "point slot0 diffuse shifted to light1 after disabling point0")) {
    return false;
  }
  if (!Check(payload[kPoint1PosRel * 4 + 0] == 0.0f && payload[kPoint1PosRel * 4 + 1] == 0.0f &&
             payload[kPoint1PosRel * 4 + 2] == 0.0f && payload[kPoint1PosRel * 4 + 3] == 0.0f,
             "point slot1 position cleared after point0 disable shift")) {
    return false;
  }
  if (!Check(payload[kPoint1DiffuseRel * 4 + 0] == 0.0f && payload[kPoint1DiffuseRel * 4 + 1] == 0.0f &&
             payload[kPoint1DiffuseRel * 4 + 2] == 0.0f && payload[kPoint1DiffuseRel * 4 + 3] == 0.0f,
             "point slot1 diffuse cleared after point0 disable shift")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseTreatsSpotLightsAsPointLights() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Configure a single spot light. The driver treats SPOT lights as POINT lights
  // for its bring-up lighting subset, so constants should be packed into the
  // point light register range (c223..).
  D3DLIGHT9 spot{};
  spot.Type = D3DLIGHT_SPOT;
  spot.Position = {7.0f, 8.0f, 9.0f};
  spot.Direction = {0.0f, 0.0f, -1.0f};
  spot.Diffuse = {0.5f, 0.25f, 0.0f, 1.0f};
  spot.Ambient = {0.0f, 0.25f, 0.0f, 1.0f};
  spot.Attenuation0 = 2.0f; // inv_att0 = 0.5
  spot.Range = 4.0f;        // inv_range2 = 1/16 = 0.0625
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &spot);
  if (!Check(hr == S_OK, "SetLight(spot0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(spot0, TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(spot light)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(spot light)")) {
    return false;
  }

  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "spot light: lighting payload present")) {
    return false;
  }

  constexpr uint32_t kPoint0PosRel = (223u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint0DiffuseRel = (224u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint0AmbientRel = (225u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint0InvAtt0Rel = (226u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint0InvRange2Rel = (227u - kFixedfuncLightingStartRegister);

  if (!Check(payload[kPoint0PosRel * 4 + 0] == 7.0f &&
             payload[kPoint0PosRel * 4 + 1] == 8.0f &&
             payload[kPoint0PosRel * 4 + 2] == 9.0f &&
             payload[kPoint0PosRel * 4 + 3] == 1.0f,
             "spot light: packed into point slot0 position")) {
    return false;
  }
  if (!Check(payload[kPoint0DiffuseRel * 4 + 0] == 0.5f &&
             payload[kPoint0DiffuseRel * 4 + 1] == 0.25f &&
             payload[kPoint0DiffuseRel * 4 + 2] == 0.0f &&
             payload[kPoint0DiffuseRel * 4 + 3] == 1.0f,
             "spot light: packed into point slot0 diffuse")) {
    return false;
  }
  if (!Check(payload[kPoint0AmbientRel * 4 + 0] == 0.0f &&
             payload[kPoint0AmbientRel * 4 + 1] == 0.25f &&
             payload[kPoint0AmbientRel * 4 + 2] == 0.0f &&
             payload[kPoint0AmbientRel * 4 + 3] == 1.0f,
             "spot light: packed into point slot0 ambient")) {
    return false;
  }
  if (!Check(payload[kPoint0InvAtt0Rel * 4 + 0] == 0.5f &&
             payload[kPoint0InvAtt0Rel * 4 + 1] == 0.5f &&
             payload[kPoint0InvAtt0Rel * 4 + 2] == 0.5f &&
             payload[kPoint0InvAtt0Rel * 4 + 3] == 0.5f,
             "spot light: packed into point slot0 inv_att0")) {
    return false;
  }
  if (!Check(payload[kPoint0InvRange2Rel * 4 + 0] == 0.0625f &&
             payload[kPoint0InvRange2Rel * 4 + 1] == 0.0625f &&
             payload[kPoint0InvRange2Rel * 4 + 2] == 0.0625f &&
             payload[kPoint0InvRange2Rel * 4 + 3] == 0.0625f,
             "spot light: packed into point slot0 inv_range2")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseIgnoresExtraDirectionalLightsBeyondFixedfuncLimit() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF000000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  // Enable 5 directional lights. The fixed-function lighting constant layout
  // exposes only 4 directional slots (c211..c222), so the 5th light must be
  // ignored.
  struct LightColor {
    float r;
    float g;
    float b;
  };
  const LightColor colors[5] = {
      {1.0f, 0.0f, 0.0f}, // light0: red
      {0.0f, 1.0f, 0.0f}, // light1: green
      {0.0f, 0.0f, 1.0f}, // light2: blue
      {1.0f, 1.0f, 0.0f}, // light3: yellow
      {1.0f, 0.0f, 1.0f}, // light4: magenta (should be ignored)
  };
  for (uint32_t i = 0; i < 5; ++i) {
    D3DLIGHT9 light{};
    light.Type = D3DLIGHT_DIRECTIONAL;
    light.Direction = {0.0f, 0.0f, -1.0f};
    light.Diffuse = {colors[i].r, colors[i].g, colors[i].b, 1.0f};
    light.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
    hr = device_set_light(cleanup.hDevice, /*index=*/i, &light);
    if (!Check(hr == S_OK, "SetLight(directional)")) {
      return false;
    }
    hr = device_light_enable(cleanup.hDevice, /*index=*/i, TRUE);
    if (!Check(hr == S_OK, "LightEnable(directional, TRUE)")) {
      return false;
    }
  }

  // Ensure directional overflow does not stop subsequent point lights from being
  // packed (a naive implementation might `break` when dir slots are full).
  D3DLIGHT9 point{};
  point.Type = D3DLIGHT_POINT;
  point.Position = {1.0f, 2.0f, 3.0f};
  point.Diffuse = {0.25f, 0.5f, 0.75f, 1.0f};
  point.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  point.Attenuation0 = 1.0f;
  point.Range = 1.0f;
  hr = device_set_light(cleanup.hDevice, /*index=*/5, &point);
  if (!Check(hr == S_OK, "SetLight(point after directional overflow)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/5, TRUE);
  if (!Check(hr == S_OK, "LightEnable(point after directional overflow, TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(5 directional lights)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(5 directional lights)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "directional overflow: lighting constant upload emitted once")) {
    return false;
  }
  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "directional overflow: lighting payload present")) {
    return false;
  }

  // Directional slot diffuse registers: c212, c215, c218, c221.
  constexpr uint32_t kSlot0DiffuseRel = (212u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kSlot1DiffuseRel = (215u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kSlot2DiffuseRel = (218u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kSlot3DiffuseRel = (221u - kFixedfuncLightingStartRegister);

  const auto check_diffuse = [&](uint32_t rel, const char* name, const LightColor& c) -> bool {
    return Check(payload[rel * 4 + 0] == c.r && payload[rel * 4 + 1] == c.g && payload[rel * 4 + 2] == c.b &&
                     payload[rel * 4 + 3] == 1.0f,
                 name);
  };
  if (!check_diffuse(kSlot0DiffuseRel, "directional overflow: slot0 diffuse == light0 (red)", colors[0])) {
    return false;
  }
  if (!check_diffuse(kSlot1DiffuseRel, "directional overflow: slot1 diffuse == light1 (green)", colors[1])) {
    return false;
  }
  if (!check_diffuse(kSlot2DiffuseRel, "directional overflow: slot2 diffuse == light2 (blue)", colors[2])) {
    return false;
  }
  if (!check_diffuse(kSlot3DiffuseRel, "directional overflow: slot3 diffuse == light3 (yellow)", colors[3])) {
    return false;
  }

  // Point slot0 diffuse register: c224.
  constexpr uint32_t kPoint0PosRel = (223u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint0DiffuseRel = (224u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kPoint0PosRel * 4 + 0] == 1.0f &&
             payload[kPoint0PosRel * 4 + 1] == 2.0f &&
             payload[kPoint0PosRel * 4 + 2] == 3.0f &&
             payload[kPoint0PosRel * 4 + 3] == 1.0f,
             "directional overflow: point slot0 position packed")) {
    return false;
  }
  if (!Check(payload[kPoint0DiffuseRel * 4 + 0] == 0.25f &&
             payload[kPoint0DiffuseRel * 4 + 1] == 0.5f &&
             payload[kPoint0DiffuseRel * 4 + 2] == 0.75f &&
             payload[kPoint0DiffuseRel * 4 + 3] == 1.0f,
             "directional overflow: point slot0 diffuse packed")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseIgnoresExtraPointLightsBeyondFixedfuncLimit() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF000000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  // Enable 3 point lights. The fixed-function lighting constant layout exposes
  // only 2 point slots (c223..c232), so the 3rd light must be ignored.
  struct PointDesc {
    float px;
    float py;
    float pz;
    float r;
    float g;
    float b;
  };
  const PointDesc points[3] = {
      {1.0f, 2.0f, 3.0f, 1.0f, 0.0f, 0.0f}, // point0: red
      {4.0f, 5.0f, 6.0f, 0.0f, 1.0f, 0.0f}, // point1: green
      {7.0f, 8.0f, 9.0f, 0.0f, 0.0f, 1.0f}, // point2: blue (should be ignored)
  };
  for (uint32_t i = 0; i < 3; ++i) {
    D3DLIGHT9 light{};
    light.Type = D3DLIGHT_POINT;
    light.Position = {points[i].px, points[i].py, points[i].pz};
    light.Diffuse = {points[i].r, points[i].g, points[i].b, 1.0f};
    light.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
    light.Attenuation0 = 1.0f;
    light.Range = 1.0f;
    hr = device_set_light(cleanup.hDevice, /*index=*/i, &light);
    if (!Check(hr == S_OK, "SetLight(point)")) {
      return false;
    }
    hr = device_light_enable(cleanup.hDevice, /*index=*/i, TRUE);
    if (!Check(hr == S_OK, "LightEnable(point, TRUE)")) {
      return false;
    }
  }

  // Ensure point overflow does not stop subsequent directional lights from being
  // packed (a naive implementation might `break` when point slots are full).
  D3DLIGHT9 dir{};
  dir.Type = D3DLIGHT_DIRECTIONAL;
  dir.Direction = {0.0f, 0.0f, -1.0f};
  dir.Diffuse = {0.25f, 0.5f, 0.75f, 1.0f};
  dir.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/3, &dir);
  if (!Check(hr == S_OK, "SetLight(directional after point overflow)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/3, TRUE);
  if (!Check(hr == S_OK, "LightEnable(directional after point overflow, TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(3 point lights)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(3 point lights)")) {
    return false;
  }
  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "point overflow: lighting constant upload emitted once")) {
    return false;
  }
  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "point overflow: lighting payload present")) {
    return false;
  }

  constexpr uint32_t kPoint0PosRel = (223u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint0DiffuseRel = (224u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint1PosRel = (228u - kFixedfuncLightingStartRegister);
  constexpr uint32_t kPoint1DiffuseRel = (229u - kFixedfuncLightingStartRegister);

  if (!Check(payload[kPoint0PosRel * 4 + 0] == points[0].px &&
             payload[kPoint0PosRel * 4 + 1] == points[0].py &&
             payload[kPoint0PosRel * 4 + 2] == points[0].pz &&
             payload[kPoint0PosRel * 4 + 3] == 1.0f,
             "point overflow: slot0 position == point0")) {
    return false;
  }
  if (!Check(payload[kPoint0DiffuseRel * 4 + 0] == points[0].r &&
             payload[kPoint0DiffuseRel * 4 + 1] == points[0].g &&
             payload[kPoint0DiffuseRel * 4 + 2] == points[0].b &&
             payload[kPoint0DiffuseRel * 4 + 3] == 1.0f,
             "point overflow: slot0 diffuse == point0 (red)")) {
    return false;
  }
  if (!Check(payload[kPoint1PosRel * 4 + 0] == points[1].px &&
             payload[kPoint1PosRel * 4 + 1] == points[1].py &&
             payload[kPoint1PosRel * 4 + 2] == points[1].pz &&
             payload[kPoint1PosRel * 4 + 3] == 1.0f,
             "point overflow: slot1 position == point1")) {
    return false;
  }
  if (!Check(payload[kPoint1DiffuseRel * 4 + 0] == points[1].r &&
             payload[kPoint1DiffuseRel * 4 + 1] == points[1].g &&
             payload[kPoint1DiffuseRel * 4 + 2] == points[1].b &&
             payload[kPoint1DiffuseRel * 4 + 3] == 1.0f,
             "point overflow: slot1 diffuse == point1 (green)")) {
    return false;
  }

  // Directional slot0 diffuse register: c212.
  constexpr uint32_t kSlot0DiffuseRel = (212u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kSlot0DiffuseRel * 4 + 0] == 0.25f &&
             payload[kSlot0DiffuseRel * 4 + 1] == 0.5f &&
             payload[kSlot0DiffuseRel * 4 + 2] == 0.75f &&
             payload[kSlot0DiffuseRel * 4 + 3] == 1.0f,
             "point overflow: directional slot0 diffuse packed")) {
    return false;
  }

  // Ensure the ignored 3rd point light did not clobber material constants (c233..c235).
  constexpr uint32_t kMatDiffuseRel = (233u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kMatDiffuseRel * 4 + 0] == 1.0f &&
             payload[kMatDiffuseRel * 4 + 1] == 1.0f &&
             payload[kMatDiffuseRel * 4 + 2] == 1.0f &&
             payload[kMatDiffuseRel * 4 + 3] == 1.0f,
             "point overflow: material diffuse constant preserved")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseTransformsLightDirectionByView() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Set VIEW to a 90-degree rotation about Z (row-vector convention):
  //   [ 0  1  0  0 ]
  //   [-1  0  0  0 ]
  //   [ 0  0  1  0 ]
  //   [ 0  0  0  1 ]
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX view = identity;
  view.m[0][0] = 0.0f;
  view.m[0][1] = 1.0f;
  view.m[1][0] = -1.0f;
  view.m[1][1] = 0.0f;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &identity);
  if (!Check(hr == S_OK, "SetTransform(WORLD0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &view);
  if (!Check(hr == S_OK, "SetTransform(VIEW rotated)")) {
    return false;
  }

  // Configure a directional light whose direction will change under the view transform.
  // D3D9 direction is the direction light rays travel; the shader expects vertex->light,
  // so the driver negates it.
  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {1.0f, 0.0f, 0.0f};
  light0.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(rotated view directional light)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(rotated view directional light)")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "rotated view: lighting constant upload emitted once")) {
    return false;
  }
  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "rotated view: lighting payload present")) {
    return false;
  }

  // c208..c210 should match the world*view columns 0..2 (WORLD0 is identity).
  // For the view matrix above, expected columns are:
  //   c208 = {0, -1, 0, 0}
  //   c209 = {1,  0, 0, 0}
  //   c210 = {0,  0, 1, 0}
  if (!Check(payload[0] == 0.0f && payload[1] == -1.0f && payload[2] == 0.0f && payload[3] == 0.0f &&
             payload[4] == 1.0f && payload[5] == 0.0f && payload[6] == 0.0f && payload[7] == 0.0f &&
             payload[8] == 0.0f && payload[9] == 0.0f && payload[10] == 1.0f && payload[11] == 0.0f,
             "rotated view: c208..c210 pack world*view columns")) {
    return false;
  }

  // Directional slot0 direction (c211) should reflect view-space vertex->light:
  // - Light direction (rays) = (1,0,0)
  // - Transform into view space: (1,0,0) * view3x3 = (0,1,0)
  // - Negate for vertex->light: (0,-1,0)
  constexpr uint32_t kLight0DirRel = (211u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kLight0DirRel * 4 + 0] == 0.0f &&
             payload[kLight0DirRel * 4 + 1] == -1.0f &&
             payload[kLight0DirRel * 4 + 2] == 0.0f &&
             payload[kLight0DirRel * 4 + 3] == 0.0f,
             "rotated view: directional light direction transformed into view space")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseTransformsPointLightPositionByView() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Translate VIEW so point lights are transformed into view space.
  constexpr float tx = 2.0f;
  constexpr float ty = -3.0f;
  constexpr float tz = 4.0f;
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX view = identity;
  view.m[3][0] = tx;
  view.m[3][1] = ty;
  view.m[3][2] = tz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &identity);
  if (!Check(hr == S_OK, "SetTransform(WORLD0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &view);
  if (!Check(hr == S_OK, "SetTransform(VIEW translated)")) {
    return false;
  }

  D3DLIGHT9 point{};
  point.Type = D3DLIGHT_POINT;
  point.Position = {1.0f, 2.0f, 3.0f};
  point.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  point.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  point.Attenuation0 = 1.0f;
  point.Range = 1.0f;
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &point);
  if (!Check(hr == S_OK, "SetLight(point0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(point0, TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(translated view point light)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(translated view point light)")) {
    return false;
  }

  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "lighting payload present")) {
    return false;
  }

  // c208..c210: world*view columns 0..2 (w contains translation).
  if (!Check(payload[0] == 1.0f && payload[1] == 0.0f && payload[2] == 0.0f && payload[3] == tx &&
             payload[4] == 0.0f && payload[5] == 1.0f && payload[6] == 0.0f && payload[7] == ty &&
             payload[8] == 0.0f && payload[9] == 0.0f && payload[10] == 1.0f && payload[11] == tz,
             "translated view: c208..c210 pack world*view columns")) {
    return false;
  }

  // Point slot0 position (c223) should include view translation.
  constexpr uint32_t kPoint0PosRel = (223u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kPoint0PosRel * 4 + 0] == 1.0f + tx &&
             payload[kPoint0PosRel * 4 + 1] == 2.0f + ty &&
             payload[kPoint0PosRel * 4 + 2] == 3.0f + tz &&
             payload[kPoint0PosRel * 4 + 3] == 1.0f,
             "translated view: point light position transformed into view space")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseTransformsPointLightPositionByViewRotation() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Rotate VIEW 90 degrees about Z (row-vector convention):
  //   [ 0  1  0  0 ]
  //   [-1  0  0  0 ]
  //   [ 0  0  1  0 ]
  //   [ 0  0  0  1 ]
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX view = identity;
  view.m[0][0] = 0.0f;
  view.m[0][1] = 1.0f;
  view.m[1][0] = -1.0f;
  view.m[1][1] = 0.0f;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &identity);
  if (!Check(hr == S_OK, "SetTransform(WORLD0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &view);
  if (!Check(hr == S_OK, "SetTransform(VIEW rotated)")) {
    return false;
  }

  D3DLIGHT9 point{};
  point.Type = D3DLIGHT_POINT;
  point.Position = {1.0f, 2.0f, 3.0f};
  point.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  point.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  point.Attenuation0 = 1.0f;
  point.Range = 1.0f;
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &point);
  if (!Check(hr == S_OK, "SetLight(point0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(point0, TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(rotated view point light)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(rotated view point light)")) {
    return false;
  }

  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "lighting payload present")) {
    return false;
  }

  // c208..c210 should match the world*view columns 0..2 (WORLD0 is identity).
  if (!Check(payload[0] == 0.0f && payload[1] == -1.0f && payload[2] == 0.0f && payload[3] == 0.0f &&
             payload[4] == 1.0f && payload[5] == 0.0f && payload[6] == 0.0f && payload[7] == 0.0f &&
             payload[8] == 0.0f && payload[9] == 0.0f && payload[10] == 1.0f && payload[11] == 0.0f,
             "rotated view: c208..c210 pack world*view columns")) {
    return false;
  }

  // Point slot0 position (c223) should be transformed into view space by the view rotation:
  // (1,2,3) -> (-2,1,3).
  constexpr uint32_t kPoint0PosRel = (223u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kPoint0PosRel * 4 + 0] == -2.0f &&
             payload[kPoint0PosRel * 4 + 1] == 1.0f &&
             payload[kPoint0PosRel * 4 + 2] == 3.0f &&
             payload[kPoint0PosRel * 4 + 3] == 1.0f,
             "rotated view: point light position transformed into view space")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseDoesNotTransformLightDirectionByWorld() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Set WORLD to a 90-degree rotation about Z (row-vector convention) while keeping VIEW identity.
  // Fixed-function lighting should transform light directions by VIEW only, not world*view.
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[0][0] = 0.0f;
  world.m[0][1] = 1.0f;
  world.m[1][0] = -1.0f;
  world.m[1][1] = 0.0f;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD0 rotated)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW identity)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION identity)")) {
    return false;
  }

  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  // Direction points along +X; with VIEW identity, the driver should produce vertex->light = (-1,0,0).
  light0.Direction = {1.0f, 0.0f, 0.0f};
  light0.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(directional0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(directional0, TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(world rotated directional light)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(world rotated directional light)")) {
    return false;
  }

  if (!Check(CountVsConstantUploads(buf,
                                    len,
                                    kFixedfuncLightingStartRegister,
                                    kFixedfuncLightingVec4Count) == 1,
             "world rotated: lighting constant upload emitted once")) {
    return false;
  }
  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "world rotated: lighting payload present")) {
    return false;
  }

  // c208..c210 should match the world*view columns 0..2 (VIEW is identity so this is WORLD0).
  if (!Check(payload[0] == 0.0f && payload[1] == -1.0f && payload[2] == 0.0f && payload[3] == 0.0f &&
             payload[4] == 1.0f && payload[5] == 0.0f && payload[6] == 0.0f && payload[7] == 0.0f &&
             payload[8] == 0.0f && payload[9] == 0.0f && payload[10] == 1.0f && payload[11] == 0.0f,
             "world rotated: c208..c210 pack world*view columns")) {
    return false;
  }

  // Directional slot0 direction (c211) should be transformed by VIEW only (identity), so it must
  // not be affected by the WORLD rotation above.
  constexpr uint32_t kLight0DirRel = (211u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kLight0DirRel * 4 + 0] == -1.0f &&
             payload[kLight0DirRel * 4 + 1] == 0.0f &&
             payload[kLight0DirRel * 4 + 2] == 0.0f &&
             payload[kLight0DirRel * 4 + 3] == 0.0f,
             "world rotated: directional light direction transformed by VIEW only")) {
    return false;
  }

  return true;
}

bool TestFvfXyzNormalDiffuseDoesNotTransformPointLightPositionByWorld() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Activate the fixed-function lit path.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Set WORLD translation while keeping VIEW identity.
  // Point light positions should be transformed by VIEW only, not by world*view.
  constexpr float wx = 5.0f;
  constexpr float wy = -7.0f;
  constexpr float wz = 3.0f;
  D3DMATRIX identity{};
  identity.m[0][0] = 1.0f;
  identity.m[1][1] = 1.0f;
  identity.m[2][2] = 1.0f;
  identity.m[3][3] = 1.0f;
  D3DMATRIX world = identity;
  world.m[3][0] = wx;
  world.m[3][1] = wy;
  world.m[3][2] = wz;
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world);
  if (!Check(hr == S_OK, "SetTransform(WORLD0 translated)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformView, &identity);
  if (!Check(hr == S_OK, "SetTransform(VIEW identity)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformProjection, &identity);
  if (!Check(hr == S_OK, "SetTransform(PROJECTION identity)")) {
    return false;
  }

  D3DLIGHT9 point{};
  point.Type = D3DLIGHT_POINT;
  point.Position = {1.0f, 2.0f, 3.0f};
  point.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  point.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  point.Attenuation0 = 1.0f;
  point.Range = 1.0f;
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &point);
  if (!Check(hr == S_OK, "SetLight(point0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(point0, TRUE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.0f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(world translated point light)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(world translated point light)")) {
    return false;
  }

  const float* payload = FindVsConstantsPayload(buf,
                                                len,
                                                kFixedfuncLightingStartRegister,
                                                kFixedfuncLightingVec4Count);
  if (!Check(payload != nullptr, "lighting payload present")) {
    return false;
  }

  // c208..c210: world*view columns 0..2 (VIEW identity so this is WORLD0, including translation).
  if (!Check(payload[0] == 1.0f && payload[1] == 0.0f && payload[2] == 0.0f && payload[3] == wx &&
             payload[4] == 0.0f && payload[5] == 1.0f && payload[6] == 0.0f && payload[7] == wy &&
             payload[8] == 0.0f && payload[9] == 0.0f && payload[10] == 1.0f && payload[11] == wz,
             "world translated: c208..c210 pack world*view columns")) {
    return false;
  }

  // Point slot0 position (c223) should be transformed by VIEW only (identity), so it must not
  // include WORLD translation.
  constexpr uint32_t kPoint0PosRel = (223u - kFixedfuncLightingStartRegister);
  if (!Check(payload[kPoint0PosRel * 4 + 0] == 1.0f &&
             payload[kPoint0PosRel * 4 + 1] == 2.0f &&
             payload[kPoint0PosRel * 4 + 2] == 3.0f &&
             payload[kPoint0PosRel * 4 + 3] == 1.0f,
             "world translated: point light position transformed by VIEW only")) {
    return false;
  }

  return true;
}

bool TestFixedfuncFogTogglesShaderVariant() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;     // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;      // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;  // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;      // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;        // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;        // D3DFOG_LINEAR

  dev->cmd.reset();

  // Force a fixed-function draw that uses a fixed-function fallback VS+PS (no user shaders).
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Start with fog disabled.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.25f, 1.0f, 0xFF00FF00u, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(fog off)")) {
    return false;
  }

  Shader* vs_off = nullptr;
  Shader* ps_off = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    vs_off = dev->vs;
    ps_off = dev->ps;
    if (!Check(vs_off != nullptr, "VS bound (fog off)")) {
      return false;
    }
    if (!Check(ps_off != nullptr, "PS bound (fog off)")) {
      return false;
    }
  }

  // Enable linear fog and draw again; fixed-function fallback should select a new
  // VS+PS variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(0.2f));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(0.8f));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(fog on)")) {
    return false;
  }

  Shader* vs_on = nullptr;
  Shader* ps_on = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    vs_on = dev->vs;
    ps_on = dev->ps;
    if (!Check(vs_on != nullptr, "VS bound (fog on)")) {
      return false;
    }
    if (!Check(ps_on != nullptr, "PS bound (fog on)")) {
      return false;
    }
  }

  if (!Check(vs_on != vs_off, "fog toggle changes fixed-function VS variant")) {
    return false;
  }
  if (!Check(ps_on != ps_off, "fog toggle changes fixed-function PS variant")) {
    return false;
  }

  if (!Check(ShaderContainsToken(ps_on, kPsOpAdd), "fog PS contains add opcode")) {
    return false;
  }
  if (!Check(ShaderContainsToken(ps_on, kPsOpMul), "fog PS contains mul opcode")) {
    return false;
  }
  if (!Check(ShaderContainsToken(ps_on, 0x20E40001u), "fog PS references c1 (fog color)")) {
    return false;
  }

  return true;
}

bool TestFixedfuncFogEmitsConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;     // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;      // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;  // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;      // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;        // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;        // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Enable linear fog and pick values with exact float representations so payload
  // comparisons are stable.
  constexpr float fog_start = 0.25f;
  constexpr float fog_end = 0.75f;
  constexpr float inv_range = 2.0f;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  // Capture only the draw-time fog constant upload in the command stream.
  dev->cmd.reset();

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.25f, 1.0f, 0xFF00FF00u, 0.0f, 1.0f},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(fog enabled)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(fog constants)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "fog enabled: DRAW emitted")) {
    return false;
  }

  const float expected[8] = {
      // c1: fog color (RGBA from ARGB red).
      1.0f, 0.0f, 0.0f, 1.0f,
      // c2: fog params (x=fog_start, y=inv_fog_range, z/w unused).
      fog_start, inv_range, 0.0f, 0.0f,
  };

  size_t uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 1u || sc->vec4_count != 2u) {
      continue;
    }
    const size_t need = sizeof(*sc) + sizeof(expected);
    if (!Check(hdr->size_bytes >= need, "fog constants: SET_SHADER_CONSTANTS_F contains payload")) {
      return false;
    }
    const auto* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
    if (std::memcmp(payload, expected, sizeof(expected)) != 0) {
      return Check(false, "fog constants payload matches expected c1/c2 data");
    }
    ++uploads;
  }
  if (!Check(uploads == 1, "fog constants uploaded once")) {
    return false;
  }

  return true;
}

bool TestFixedfuncFogConstantsDedupAndReuploadOnChange() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;     // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;      // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;  // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;      // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;        // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;        // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Enable linear fog and pick values with exact float representations so payload
  // comparisons are stable.
  constexpr float fog_start = 0.25f;
  constexpr float fog_end = 0.75f;
  constexpr float inv_range = 2.0f;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.25f, 1.0f, 0xFF00FF00u, 0.0f, 1.0f},
  };

  auto count_fog_uploads_with_expected_payload = [&](const float expected[8]) -> size_t {
    size_t uploads = 0;
    const uint8_t* buf = dev->cmd.data();
    const size_t len = dev->cmd.bytes_used();
    for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
      const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
      if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 1u || sc->vec4_count != 2u) {
        continue;
      }
      const size_t need = sizeof(*sc) + sizeof(float) * 8u;
      if (hdr->size_bytes < need) {
        continue;
      }
      const auto* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
      if (std::memcmp(payload, expected, sizeof(float) * 8u) == 0) {
        ++uploads;
      }
    }
    return uploads;
  };

  // First draw: expect an upload of fog constants.
  {
    dev->cmd.reset();
    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(fog enabled; first draw)")) {
      return false;
    }
    dev->cmd.finalize();
    const uint8_t* buf = dev->cmd.data();
    const size_t len = dev->cmd.bytes_used();
    if (!Check(ValidateStream(buf, len), "ValidateStream(fog constants first draw)")) {
      return false;
    }
    const float expected[8] = {
        // c1: fog color (RGBA from ARGB red).
        1.0f, 0.0f, 0.0f, 1.0f,
        // c2: fog params (x=fog_start, y=inv_fog_range, z/w unused).
        fog_start, inv_range, 0.0f, 0.0f,
    };
    if (!Check(count_fog_uploads_with_expected_payload(expected) == 1, "fog constants first draw: uploaded once")) {
      return false;
    }
  }

  // Second draw without changing fog state: expect no redundant constant upload.
  {
    dev->cmd.reset();
    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(fog enabled; second draw)")) {
      return false;
    }
    dev->cmd.finalize();
    const uint8_t* buf = dev->cmd.data();
    const size_t len = dev->cmd.bytes_used();
    if (!Check(ValidateStream(buf, len), "ValidateStream(fog constants second draw)")) {
      return false;
    }
    size_t fog_uploads = 0;
    for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
      const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
      if (sc->stage == AEROGPU_SHADER_STAGE_PIXEL && sc->start_register == 1u && sc->vec4_count == 2u) {
        ++fog_uploads;
      }
    }
    if (!Check(fog_uploads == 0, "fog constants second draw: no fog constant upload")) {
      return false;
    }
  }

  // Change fog color: expect a fresh constant upload with new payload.
  {
    dev->cmd.reset();
    hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFF00FF00u);
    if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=green)")) {
      return false;
    }

    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(fog enabled; after fogcolor change)")) {
      return false;
    }
    dev->cmd.finalize();
    const uint8_t* buf = dev->cmd.data();
    const size_t len = dev->cmd.bytes_used();
    if (!Check(ValidateStream(buf, len), "ValidateStream(fog constants after color change)")) {
      return false;
    }

    const float expected[8] = {
        // c1: fog color (RGBA from ARGB green).
        0.0f, 1.0f, 0.0f, 1.0f,
        // c2: fog params unchanged.
        fog_start, inv_range, 0.0f, 0.0f,
    };
    if (!Check(count_fog_uploads_with_expected_payload(expected) == 1,
               "fog constants after color change: uploaded once")) {
      return false;
    }
  }

  // Fourth draw without further changes: expect no redundant upload.
  {
    dev->cmd.reset();
    hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
        cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
    if (!Check(hr == S_OK, "DrawPrimitiveUP(fog enabled; fourth draw)")) {
      return false;
    }
    dev->cmd.finalize();
    const uint8_t* buf = dev->cmd.data();
    const size_t len = dev->cmd.bytes_used();
    if (!Check(ValidateStream(buf, len), "ValidateStream(fog constants fourth draw)")) {
      return false;
    }
    size_t fog_uploads = 0;
    for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
      const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
      if (sc->stage == AEROGPU_SHADER_STAGE_PIXEL && sc->start_register == 1u && sc->vec4_count == 2u) {
        ++fog_uploads;
      }
    }
    if (!Check(fog_uploads == 0, "fog constants fourth draw: no fog constant upload")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncFogConstantsReuploadAfterPsConstClobber() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShaderConstF != nullptr, "pfnSetShaderConstF is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;     // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;      // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;  // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;      // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;        // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;        // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Enable linear fog with float-friendly values so the expected payload compares
  // bitwise.
  constexpr float fog_start = 0.25f;
  constexpr float fog_end = 0.75f;
  constexpr float inv_range = 2.0f;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.25f, 1.0f, 0xFF00FF00u, 0.0f, 1.0f},
  };

  // First draw: emits fog constants and seeds the PS constant cache.
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(initial fog draw)")) {
    return false;
  }

  // Simulate an app clobbering the reserved fog constant range (c1..c2).
  const float junk[8] = {123.0f, 456.0f, 789.0f, 1011.0f, 1112.0f, 1314.0f, 1516.0f, 1718.0f};
  hr = cleanup.device_funcs.pfnSetShaderConstF(cleanup.hDevice, kD3dShaderStagePs, /*start_reg=*/1u, junk, /*vec4_count=*/2u);
  if (!Check(hr == S_OK, "SetShaderConstF(PS, c1..c2 clobber)")) {
    return false;
  }

  // Capture only the draw-time fog constant restore.
  dev->cmd.reset();

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(after fog const clobber)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(fog const clobber restore)")) {
    return false;
  }

  const float expected[8] = {
      // c1: fog color (RGBA from ARGB red).
      1.0f, 0.0f, 0.0f, 1.0f,
      // c2: fog params (x=fog_start, y=inv_fog_range, z/w unused).
      fog_start, inv_range, 0.0f, 0.0f,
  };

  size_t uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 1u || sc->vec4_count != 2u) {
      continue;
    }
    const size_t need = sizeof(*sc) + sizeof(expected);
    if (!Check(hdr->size_bytes >= need, "fog clobber restore: SET_SHADER_CONSTANTS_F contains payload")) {
      return false;
    }
    const auto* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
    if (std::memcmp(payload, expected, sizeof(expected)) != 0) {
      return Check(false, "fog clobber restore: payload matches expected c1/c2 data");
    }
    ++uploads;
  }
  if (!Check(uploads == 1, "fog clobber restore: fog constants uploaded once")) {
    return false;
  }

  return true;
}

bool TestXyzrhwConversionIgnoresViewportMinMaxZ() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetViewport != nullptr, "pfnSetViewport is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Use non-default viewport MinZ/MaxZ to ensure pre-transformed POSITIONT.z is
  // treated as D3D9 NDC depth (0..1) rather than being "unmapped" from MinZ/MaxZ.
  //
  // For bring-up, AeroGPU intentionally ignores MinZ/MaxZ for XYZRHW conversion
  // (see d3d9 docs/README limitations).
  D3DDDIVIEWPORTINFO vp{};
  vp.X = 0.0f;
  vp.Y = 0.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.25f;
  vp.MaxZ = 0.75f;
  HRESULT hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport(MinZ/MaxZ)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  dev->cmd.reset();

  // Choose Z == MinZ so an implementation that incorrectly unmapped MinZ/MaxZ
  // would convert depth to 0 (rather than leaving it at 0.25).
  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.25f, 0.5f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.25f, 0.5f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.25f, 0.5f, 0xFFFFFFFFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW conversion ignores MinZ/MaxZ)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "XYZRHW conversion: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri),
               "XYZRHW conversion: scratch VB storage contains uploaded vertices")) {
      return false;
    }

    float clip_z = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(clip_w == 2.0f, "XYZRHW conversion: produced clip_w == 1/rhw")) {
      return false;
    }
    // Expect clip_z = z * clip_w (z treated as NDC depth).
    if (!Check(clip_z == 0.5f, "XYZRHW conversion: ignores MinZ/MaxZ (clip_z == z*w)")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwIndexedConversionIgnoresViewportMinMaxZ() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetViewport != nullptr, "pfnSetViewport is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetIndices != nullptr, "pfnSetIndices is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawIndexedPrimitive != nullptr, "pfnDrawIndexedPrimitive is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Use non-default viewport MinZ/MaxZ to ensure the VB/IB indexed draw path's
  // CPU XYZRHW expansion also treats POSITIONT.z as D3D9 NDC depth (0..1).
  D3DDDIVIEWPORTINFO vp{};
  vp.X = 0.0f;
  vp.Y = 0.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.25f;
  vp.MaxZ = 0.75f;
  HRESULT hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport(MinZ/MaxZ)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  // Create + fill a VB with three XYZRHW vertices (z == MinZ).
  const VertexXyzrhwDiffuse verts[3] = {
      {0.0f, 0.0f, 0.25f, 0.5f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.25f, 0.5f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.25f, 0.5f, 0xFFFFFFFFu},
  };
  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyzrhw|diffuse)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  D3D9DDIARG_LOCK lock_vb{};
  lock_vb.hResource = create_vb.hResource;
  lock_vb.offset_bytes = 0;
  lock_vb.size_bytes = 0;
  lock_vb.flags = 0;
  D3DDDI_LOCKEDBOX vb_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_vb, &vb_box);
  if (!Check(hr == S_OK, "Lock(vb xyzrhw|diffuse)")) {
    return false;
  }
  if (!Check(vb_box.pData != nullptr, "Lock(vb) returns pData")) {
    return false;
  }
  std::memcpy(vb_box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock_vb{};
  unlock_vb.hResource = create_vb.hResource;
  unlock_vb.offset_bytes = 0;
  unlock_vb.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_vb);
  if (!Check(hr == S_OK, "Unlock(vb xyzrhw|diffuse)")) {
    return false;
  }

  // Create + fill an index buffer (0,1,2).
  const uint16_t indices[3] = {0u, 1u, 2u};
  D3D9DDIARG_CREATERESOURCE create_ib{};
  create_ib.type = 0u;
  create_ib.format = 0u;
  create_ib.width = 0;
  create_ib.height = 0;
  create_ib.depth = 0;
  create_ib.mip_levels = 1;
  create_ib.usage = 0;
  create_ib.pool = 0;
  create_ib.size = sizeof(indices);
  create_ib.hResource.pDrvPrivate = nullptr;
  create_ib.pSharedHandle = nullptr;
  create_ib.pPrivateDriverData = nullptr;
  create_ib.PrivateDriverDataSize = 0;
  create_ib.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_ib);
  if (!Check(hr == S_OK, "CreateResource(index buffer u16)")) {
    return false;
  }
  if (!Check(create_ib.hResource.pDrvPrivate != nullptr, "CreateResource returned ib handle")) {
    return false;
  }
  cleanup.resources.push_back(create_ib.hResource);

  D3D9DDIARG_LOCK lock_ib{};
  lock_ib.hResource = create_ib.hResource;
  lock_ib.offset_bytes = 0;
  lock_ib.size_bytes = 0;
  lock_ib.flags = 0;
  D3DDDI_LOCKEDBOX ib_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_ib, &ib_box);
  if (!Check(hr == S_OK, "Lock(ib u16)")) {
    return false;
  }
  if (!Check(ib_box.pData != nullptr, "Lock(ib) returns pData")) {
    return false;
  }
  std::memcpy(ib_box.pData, indices, sizeof(indices));

  D3D9DDIARG_UNLOCK unlock_ib{};
  unlock_ib.hResource = create_ib.hResource;
  unlock_ib.offset_bytes = 0;
  unlock_ib.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_ib);
  if (!Check(hr == S_OK, "Unlock(ib u16)")) {
    return false;
  }

  // Bind VB/IB and draw.
  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb xyzrhw|diffuse)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetIndices(cleanup.hDevice, create_ib.hResource, static_cast<D3DDDIFORMAT>(101), 0);
  if (!Check(hr == S_OK, "SetIndices(ib index16)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawIndexedPrimitive(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*base_vertex=*/0, /*min_index=*/0, /*num_vertices=*/3, /*start_index=*/0,
      /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawIndexedPrimitive(XYZRHW indexed conversion ignores MinZ/MaxZ)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "indexed XYZRHW conversion: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(verts),
               "indexed XYZRHW conversion: scratch VB storage contains uploaded vertices")) {
      return false;
    }

    float clip_z = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(clip_w == 2.0f, "indexed XYZRHW conversion: produced clip_w == 1/rhw")) {
      return false;
    }
    if (!Check(clip_z == 0.5f, "indexed XYZRHW conversion: ignores MinZ/MaxZ (clip_z == z*w)")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwIndexedConversionAppliesViewportXyAndPixelCenterBias() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetViewport != nullptr, "pfnSetViewport is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetIndices != nullptr, "pfnSetIndices is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawIndexedPrimitive != nullptr, "pfnDrawIndexedPrimitive is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Use a non-zero viewport origin and verify the indexed XYZRHW expansion path
  // applies viewport X/Y and the -0.5 pixel-center convention.
  D3DDDIVIEWPORTINFO vp{};
  vp.X = 10.0f;
  vp.Y = 20.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  HRESULT hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport(X/Y origin)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  // Create + fill a VB with three XYZRHW vertices. Place the first vertex at
  // (vp.X-0.5, vp.Y-0.5) so it maps to NDC (-1,+1).
  const VertexXyzrhwDiffuse verts[3] = {
      {9.5f, 19.5f, 0.25f, 0.5f, 0xFFFFFFFFu},
      {9.5f + 256.0f, 19.5f, 0.25f, 0.5f, 0xFFFFFFFFu},
      {9.5f, 19.5f + 256.0f, 0.25f, 0.5f, 0xFFFFFFFFu},
  };
  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyzrhw|diffuse)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  D3D9DDIARG_LOCK lock_vb{};
  lock_vb.hResource = create_vb.hResource;
  lock_vb.offset_bytes = 0;
  lock_vb.size_bytes = 0;
  lock_vb.flags = 0;
  D3DDDI_LOCKEDBOX vb_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_vb, &vb_box);
  if (!Check(hr == S_OK, "Lock(vb xyzrhw|diffuse)")) {
    return false;
  }
  if (!Check(vb_box.pData != nullptr, "Lock(vb) returns pData")) {
    return false;
  }
  std::memcpy(vb_box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock_vb{};
  unlock_vb.hResource = create_vb.hResource;
  unlock_vb.offset_bytes = 0;
  unlock_vb.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_vb);
  if (!Check(hr == S_OK, "Unlock(vb xyzrhw|diffuse)")) {
    return false;
  }

  // Create + fill an index buffer (0,1,2).
  const uint16_t indices[3] = {0u, 1u, 2u};
  D3D9DDIARG_CREATERESOURCE create_ib{};
  create_ib.type = 0u;
  create_ib.format = 0u;
  create_ib.width = 0;
  create_ib.height = 0;
  create_ib.depth = 0;
  create_ib.mip_levels = 1;
  create_ib.usage = 0;
  create_ib.pool = 0;
  create_ib.size = sizeof(indices);
  create_ib.hResource.pDrvPrivate = nullptr;
  create_ib.pSharedHandle = nullptr;
  create_ib.pPrivateDriverData = nullptr;
  create_ib.PrivateDriverDataSize = 0;
  create_ib.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_ib);
  if (!Check(hr == S_OK, "CreateResource(index buffer u16)")) {
    return false;
  }
  if (!Check(create_ib.hResource.pDrvPrivate != nullptr, "CreateResource returned ib handle")) {
    return false;
  }
  cleanup.resources.push_back(create_ib.hResource);

  D3D9DDIARG_LOCK lock_ib{};
  lock_ib.hResource = create_ib.hResource;
  lock_ib.offset_bytes = 0;
  lock_ib.size_bytes = 0;
  lock_ib.flags = 0;
  D3DDDI_LOCKEDBOX ib_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_ib, &ib_box);
  if (!Check(hr == S_OK, "Lock(ib u16)")) {
    return false;
  }
  if (!Check(ib_box.pData != nullptr, "Lock(ib) returns pData")) {
    return false;
  }
  std::memcpy(ib_box.pData, indices, sizeof(indices));

  D3D9DDIARG_UNLOCK unlock_ib{};
  unlock_ib.hResource = create_ib.hResource;
  unlock_ib.offset_bytes = 0;
  unlock_ib.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_ib);
  if (!Check(hr == S_OK, "Unlock(ib u16)")) {
    return false;
  }

  // Bind VB/IB and draw.
  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb xyzrhw|diffuse)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetIndices(cleanup.hDevice, create_ib.hResource, static_cast<D3DDDIFORMAT>(101), 0);
  if (!Check(hr == S_OK, "SetIndices(ib index16)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawIndexedPrimitive(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*base_vertex=*/0, /*min_index=*/0, /*num_vertices=*/3, /*start_index=*/0,
      /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawIndexedPrimitive(XYZRHW indexed conversion applies viewport X/Y)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "indexed viewport X/Y: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(verts),
               "indexed viewport X/Y: scratch VB storage contains uploaded vertices")) {
      return false;
    }

    float clip_x = 0.0f;
    float clip_y = 0.0f;
    float clip_z = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_x, dev->up_vertex_buffer->storage.data() + 0, sizeof(float));
    std::memcpy(&clip_y, dev->up_vertex_buffer->storage.data() + 4, sizeof(float));
    std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(clip_w == 2.0f, "indexed viewport X/Y: clip_w == 1/rhw")) {
      return false;
    }
    if (!Check(clip_x == -2.0f, "indexed viewport X/Y: clip_x == -w (ndc_x=-1)")) {
      return false;
    }
    if (!Check(clip_y == 2.0f, "indexed viewport X/Y: clip_y == +w (ndc_y=+1)")) {
      return false;
    }
    if (!Check(clip_z == 0.5f, "indexed viewport X/Y: clip_z == z*w")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwIndexedConversionRhwZeroFallsBackToW1() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetViewport != nullptr, "pfnSetViewport is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetIndices != nullptr, "pfnSetIndices is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawIndexedPrimitive != nullptr, "pfnDrawIndexedPrimitive is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Ensure viewport dims are non-zero so we don't accidentally use a 1x1 default.
  D3DDDIVIEWPORTINFO vp{};
  vp.X = 0.0f;
  vp.Y = 0.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  HRESULT hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  // Create + fill a VB with rhw == 0 (w should fall back to 1).
  const VertexXyzrhwDiffuse verts[3] = {
      {0.0f, 0.0f, 0.25f, 0.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.25f, 0.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.25f, 0.0f, 0xFFFFFFFFu},
  };
  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyzrhw|diffuse; rhw=0)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  D3D9DDIARG_LOCK lock_vb{};
  lock_vb.hResource = create_vb.hResource;
  lock_vb.offset_bytes = 0;
  lock_vb.size_bytes = 0;
  lock_vb.flags = 0;
  D3DDDI_LOCKEDBOX vb_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_vb, &vb_box);
  if (!Check(hr == S_OK, "Lock(vb rhw=0)")) {
    return false;
  }
  if (!Check(vb_box.pData != nullptr, "Lock(vb rhw=0) returns pData")) {
    return false;
  }
  std::memcpy(vb_box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock_vb{};
  unlock_vb.hResource = create_vb.hResource;
  unlock_vb.offset_bytes = 0;
  unlock_vb.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_vb);
  if (!Check(hr == S_OK, "Unlock(vb rhw=0)")) {
    return false;
  }

  // Create + fill an index buffer (0,1,2).
  const uint16_t indices[3] = {0u, 1u, 2u};
  D3D9DDIARG_CREATERESOURCE create_ib{};
  create_ib.type = 0u;
  create_ib.format = 0u;
  create_ib.width = 0;
  create_ib.height = 0;
  create_ib.depth = 0;
  create_ib.mip_levels = 1;
  create_ib.usage = 0;
  create_ib.pool = 0;
  create_ib.size = sizeof(indices);
  create_ib.hResource.pDrvPrivate = nullptr;
  create_ib.pSharedHandle = nullptr;
  create_ib.pPrivateDriverData = nullptr;
  create_ib.PrivateDriverDataSize = 0;
  create_ib.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_ib);
  if (!Check(hr == S_OK, "CreateResource(index buffer u16)")) {
    return false;
  }
  if (!Check(create_ib.hResource.pDrvPrivate != nullptr, "CreateResource returned ib handle")) {
    return false;
  }
  cleanup.resources.push_back(create_ib.hResource);

  D3D9DDIARG_LOCK lock_ib{};
  lock_ib.hResource = create_ib.hResource;
  lock_ib.offset_bytes = 0;
  lock_ib.size_bytes = 0;
  lock_ib.flags = 0;
  D3DDDI_LOCKEDBOX ib_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_ib, &ib_box);
  if (!Check(hr == S_OK, "Lock(ib u16)")) {
    return false;
  }
  if (!Check(ib_box.pData != nullptr, "Lock(ib) returns pData")) {
    return false;
  }
  std::memcpy(ib_box.pData, indices, sizeof(indices));

  D3D9DDIARG_UNLOCK unlock_ib{};
  unlock_ib.hResource = create_ib.hResource;
  unlock_ib.offset_bytes = 0;
  unlock_ib.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_ib);
  if (!Check(hr == S_OK, "Unlock(ib u16)")) {
    return false;
  }

  // Bind VB/IB and draw.
  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb rhw=0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetIndices(cleanup.hDevice, create_ib.hResource, static_cast<D3DDDIFORMAT>(101), 0);
  if (!Check(hr == S_OK, "SetIndices(ib index16)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawIndexedPrimitive(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*base_vertex=*/0, /*min_index=*/0, /*num_vertices=*/3, /*start_index=*/0,
      /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawIndexedPrimitive(XYZRHW indexed; rhw=0)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "indexed rhw=0: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(verts),
               "indexed rhw=0: scratch VB storage contains uploaded vertices")) {
      return false;
    }

    float clip_z = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(clip_w == 1.0f, "indexed rhw=0: clip_w falls back to 1")) {
      return false;
    }
    if (!Check(clip_z == 0.25f, "indexed rhw=0: clip_z == z*w (w=1)")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwIndexedConversionRhwNaNFallsBackToW1() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetIndices != nullptr, "pfnSetIndices is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawIndexedPrimitive != nullptr, "pfnDrawIndexedPrimitive is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  // rhw is the reciprocal clip-space w. Non-finite rhw values are meaningless,
  // but the bring-up conversion must keep the math finite (w falls back to 1).
  const float nan = std::numeric_limits<float>::quiet_NaN();
  const VertexXyzrhwDiffuse verts[3] = {
      {0.0f, 0.0f, 0.25f, nan, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.25f, nan, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.25f, nan, 0xFFFFFFFFu},
  };

  // Create + fill a VB.
  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyzrhw|diffuse; rhw=NaN)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  D3D9DDIARG_LOCK lock_vb{};
  lock_vb.hResource = create_vb.hResource;
  lock_vb.offset_bytes = 0;
  lock_vb.size_bytes = 0;
  lock_vb.flags = 0;
  D3DDDI_LOCKEDBOX vb_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_vb, &vb_box);
  if (!Check(hr == S_OK, "Lock(vb xyzrhw|diffuse; rhw=NaN)")) {
    return false;
  }
  if (!Check(vb_box.pData != nullptr, "Lock(vb) returns pData")) {
    return false;
  }
  std::memcpy(vb_box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock_vb{};
  unlock_vb.hResource = create_vb.hResource;
  unlock_vb.offset_bytes = 0;
  unlock_vb.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_vb);
  if (!Check(hr == S_OK, "Unlock(vb xyzrhw|diffuse; rhw=NaN)")) {
    return false;
  }

  // Create + fill an index buffer (0,1,2).
  const uint16_t indices[3] = {0u, 1u, 2u};
  D3D9DDIARG_CREATERESOURCE create_ib{};
  create_ib.type = 0u;
  create_ib.format = 0u;
  create_ib.width = 0;
  create_ib.height = 0;
  create_ib.depth = 0;
  create_ib.mip_levels = 1;
  create_ib.usage = 0;
  create_ib.pool = 0;
  create_ib.size = sizeof(indices);
  create_ib.hResource.pDrvPrivate = nullptr;
  create_ib.pSharedHandle = nullptr;
  create_ib.pPrivateDriverData = nullptr;
  create_ib.PrivateDriverDataSize = 0;
  create_ib.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_ib);
  if (!Check(hr == S_OK, "CreateResource(index buffer u16)")) {
    return false;
  }
  if (!Check(create_ib.hResource.pDrvPrivate != nullptr, "CreateResource returned ib handle")) {
    return false;
  }
  cleanup.resources.push_back(create_ib.hResource);

  D3D9DDIARG_LOCK lock_ib{};
  lock_ib.hResource = create_ib.hResource;
  lock_ib.offset_bytes = 0;
  lock_ib.size_bytes = 0;
  lock_ib.flags = 0;
  D3DDDI_LOCKEDBOX ib_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_ib, &ib_box);
  if (!Check(hr == S_OK, "Lock(ib u16)")) {
    return false;
  }
  if (!Check(ib_box.pData != nullptr, "Lock(ib) returns pData")) {
    return false;
  }
  std::memcpy(ib_box.pData, indices, sizeof(indices));

  D3D9DDIARG_UNLOCK unlock_ib{};
  unlock_ib.hResource = create_ib.hResource;
  unlock_ib.offset_bytes = 0;
  unlock_ib.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_ib);
  if (!Check(hr == S_OK, "Unlock(ib u16)")) {
    return false;
  }

  // Bind VB/IB and draw.
  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb rhw=NaN)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetIndices(cleanup.hDevice, create_ib.hResource, static_cast<D3DDDIFORMAT>(101), 0);
  if (!Check(hr == S_OK, "SetIndices(ib index16)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawIndexedPrimitive(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*base_vertex=*/0, /*min_index=*/0, /*num_vertices=*/3, /*start_index=*/0,
      /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawIndexedPrimitive(XYZRHW indexed; rhw=NaN)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "indexed rhw=NaN: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(verts),
               "indexed rhw=NaN: scratch VB storage contains uploaded vertices")) {
      return false;
    }

    float clip_z = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(std::isfinite(clip_w), "indexed rhw=NaN: clip_w is finite")) {
      return false;
    }
    if (!Check(clip_w == 1.0f, "indexed rhw=NaN: clip_w falls back to 1")) {
      return false;
    }
    if (!Check(clip_z == 0.25f, "indexed rhw=NaN: clip_z == z*w (w=1)")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwIndexedConversionRhwInfFallsBackToW1() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetIndices != nullptr, "pfnSetIndices is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawIndexedPrimitive != nullptr, "pfnDrawIndexedPrimitive is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  // rhw is the reciprocal clip-space w. Non-finite rhw values are meaningless,
  // but the bring-up conversion must keep the math finite (w falls back to 1).
  const float inf = std::numeric_limits<float>::infinity();
  const VertexXyzrhwDiffuse verts[3] = {
      {0.0f, 0.0f, 0.25f, inf, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.25f, inf, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.25f, inf, 0xFFFFFFFFu},
  };

  // Create + fill a VB.
  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyzrhw|diffuse; rhw=Inf)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  D3D9DDIARG_LOCK lock_vb{};
  lock_vb.hResource = create_vb.hResource;
  lock_vb.offset_bytes = 0;
  lock_vb.size_bytes = 0;
  lock_vb.flags = 0;
  D3DDDI_LOCKEDBOX vb_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_vb, &vb_box);
  if (!Check(hr == S_OK, "Lock(vb xyzrhw|diffuse; rhw=Inf)")) {
    return false;
  }
  if (!Check(vb_box.pData != nullptr, "Lock(vb) returns pData")) {
    return false;
  }
  std::memcpy(vb_box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock_vb{};
  unlock_vb.hResource = create_vb.hResource;
  unlock_vb.offset_bytes = 0;
  unlock_vb.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_vb);
  if (!Check(hr == S_OK, "Unlock(vb xyzrhw|diffuse; rhw=Inf)")) {
    return false;
  }

  // Create + fill an index buffer (0,1,2).
  const uint16_t indices[3] = {0u, 1u, 2u};
  D3D9DDIARG_CREATERESOURCE create_ib{};
  create_ib.type = 0u;
  create_ib.format = 0u;
  create_ib.width = 0;
  create_ib.height = 0;
  create_ib.depth = 0;
  create_ib.mip_levels = 1;
  create_ib.usage = 0;
  create_ib.pool = 0;
  create_ib.size = sizeof(indices);
  create_ib.hResource.pDrvPrivate = nullptr;
  create_ib.pSharedHandle = nullptr;
  create_ib.pPrivateDriverData = nullptr;
  create_ib.PrivateDriverDataSize = 0;
  create_ib.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_ib);
  if (!Check(hr == S_OK, "CreateResource(index buffer u16)")) {
    return false;
  }
  if (!Check(create_ib.hResource.pDrvPrivate != nullptr, "CreateResource returned ib handle")) {
    return false;
  }
  cleanup.resources.push_back(create_ib.hResource);

  D3D9DDIARG_LOCK lock_ib{};
  lock_ib.hResource = create_ib.hResource;
  lock_ib.offset_bytes = 0;
  lock_ib.size_bytes = 0;
  lock_ib.flags = 0;
  D3DDDI_LOCKEDBOX ib_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_ib, &ib_box);
  if (!Check(hr == S_OK, "Lock(ib u16)")) {
    return false;
  }
  if (!Check(ib_box.pData != nullptr, "Lock(ib) returns pData")) {
    return false;
  }
  std::memcpy(ib_box.pData, indices, sizeof(indices));

  D3D9DDIARG_UNLOCK unlock_ib{};
  unlock_ib.hResource = create_ib.hResource;
  unlock_ib.offset_bytes = 0;
  unlock_ib.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_ib);
  if (!Check(hr == S_OK, "Unlock(ib u16)")) {
    return false;
  }

  // Bind VB/IB and draw.
  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb rhw=Inf)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetIndices(cleanup.hDevice, create_ib.hResource, static_cast<D3DDDIFORMAT>(101), 0);
  if (!Check(hr == S_OK, "SetIndices(ib index16)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawIndexedPrimitive(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*base_vertex=*/0, /*min_index=*/0, /*num_vertices=*/3, /*start_index=*/0,
      /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawIndexedPrimitive(XYZRHW indexed; rhw=Inf)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "indexed rhw=Inf: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(verts),
               "indexed rhw=Inf: scratch VB storage contains uploaded vertices")) {
      return false;
    }

    float clip_z = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(std::isfinite(clip_w), "indexed rhw=Inf: clip_w is finite")) {
      return false;
    }
    if (!Check(clip_w == 1.0f, "indexed rhw=Inf: clip_w falls back to 1")) {
      return false;
    }
    if (!Check(clip_z == 0.25f, "indexed rhw=Inf: clip_z == z*w (w=1)")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwIndexedConversionNonFiniteXyzFallsBackToViewportCenter() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetViewport != nullptr, "pfnSetViewport is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetIndices != nullptr, "pfnSetIndices is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawIndexedPrimitive != nullptr, "pfnDrawIndexedPrimitive is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Use a non-zero viewport origin so the "fallback to viewport center" behavior
  // is exercised.
  D3DDDIVIEWPORTINFO vp{};
  vp.X = 10.0f;
  vp.Y = 20.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  HRESULT hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  // XYZRHW conversion should keep math finite even if the app supplies NaN/Inf
  // coordinates; fall back to viewport center (NDC 0,0) and z=0.
  const float nan = std::numeric_limits<float>::quiet_NaN();
  const VertexXyzrhwDiffuse verts[3] = {
      {nan, nan, nan, 1.0f, 0xFFFFFFFFu},
      {nan, nan, nan, 1.0f, 0xFFFFFFFFu},
      {nan, nan, nan, 1.0f, 0xFFFFFFFFu},
  };

  // Create + fill a VB.
  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyzrhw|diffuse; xyz=NaN)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  D3D9DDIARG_LOCK lock_vb{};
  lock_vb.hResource = create_vb.hResource;
  lock_vb.offset_bytes = 0;
  lock_vb.size_bytes = 0;
  lock_vb.flags = 0;
  D3DDDI_LOCKEDBOX vb_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_vb, &vb_box);
  if (!Check(hr == S_OK, "Lock(vb xyzrhw|diffuse; xyz=NaN)")) {
    return false;
  }
  if (!Check(vb_box.pData != nullptr, "Lock(vb) returns pData")) {
    return false;
  }
  std::memcpy(vb_box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock_vb{};
  unlock_vb.hResource = create_vb.hResource;
  unlock_vb.offset_bytes = 0;
  unlock_vb.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_vb);
  if (!Check(hr == S_OK, "Unlock(vb xyzrhw|diffuse; xyz=NaN)")) {
    return false;
  }

  // Create + fill an index buffer (0,1,2).
  const uint16_t indices[3] = {0u, 1u, 2u};
  D3D9DDIARG_CREATERESOURCE create_ib{};
  create_ib.type = 0u;
  create_ib.format = 0u;
  create_ib.width = 0;
  create_ib.height = 0;
  create_ib.depth = 0;
  create_ib.mip_levels = 1;
  create_ib.usage = 0;
  create_ib.pool = 0;
  create_ib.size = sizeof(indices);
  create_ib.hResource.pDrvPrivate = nullptr;
  create_ib.pSharedHandle = nullptr;
  create_ib.pPrivateDriverData = nullptr;
  create_ib.PrivateDriverDataSize = 0;
  create_ib.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_ib);
  if (!Check(hr == S_OK, "CreateResource(index buffer u16)")) {
    return false;
  }
  if (!Check(create_ib.hResource.pDrvPrivate != nullptr, "CreateResource returned ib handle")) {
    return false;
  }
  cleanup.resources.push_back(create_ib.hResource);

  D3D9DDIARG_LOCK lock_ib{};
  lock_ib.hResource = create_ib.hResource;
  lock_ib.offset_bytes = 0;
  lock_ib.size_bytes = 0;
  lock_ib.flags = 0;
  D3DDDI_LOCKEDBOX ib_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_ib, &ib_box);
  if (!Check(hr == S_OK, "Lock(ib u16)")) {
    return false;
  }
  if (!Check(ib_box.pData != nullptr, "Lock(ib) returns pData")) {
    return false;
  }
  std::memcpy(ib_box.pData, indices, sizeof(indices));

  D3D9DDIARG_UNLOCK unlock_ib{};
  unlock_ib.hResource = create_ib.hResource;
  unlock_ib.offset_bytes = 0;
  unlock_ib.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_ib);
  if (!Check(hr == S_OK, "Unlock(ib u16)")) {
    return false;
  }

  // Bind VB/IB and draw.
  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb xyz=NaN)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetIndices(cleanup.hDevice, create_ib.hResource, static_cast<D3DDDIFORMAT>(101), 0);
  if (!Check(hr == S_OK, "SetIndices(ib index16)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawIndexedPrimitive(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*base_vertex=*/0, /*min_index=*/0, /*num_vertices=*/3, /*start_index=*/0,
      /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawIndexedPrimitive(XYZRHW indexed; xyz=NaN)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "indexed xyz=NaN: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(verts),
               "indexed xyz=NaN: scratch VB storage contains uploaded vertices")) {
      return false;
    }

    float clip_x = 0.0f;
    float clip_y = 0.0f;
    float clip_z = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_x, dev->up_vertex_buffer->storage.data() + 0, sizeof(float));
    std::memcpy(&clip_y, dev->up_vertex_buffer->storage.data() + 4, sizeof(float));
    std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(std::isfinite(clip_x) && std::isfinite(clip_y) && std::isfinite(clip_z) && std::isfinite(clip_w),
               "indexed xyz=NaN: clip coords are finite")) {
      return false;
    }
    if (!Check(clip_w == 1.0f, "indexed xyz=NaN: clip_w == 1")) {
      return false;
    }
    if (!Check(clip_x == 0.0f, "indexed xyz=NaN: clip_x == 0 at viewport center")) {
      return false;
    }
    if (!Check(clip_y == 0.0f, "indexed xyz=NaN: clip_y == 0 at viewport center")) {
      return false;
    }
    if (!Check(clip_z == 0.0f, "indexed xyz=NaN: clip_z == 0 (z sanitized)")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwConversionAppliesViewportXyAndPixelCenterBias() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetViewport != nullptr, "pfnSetViewport is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Use a non-zero viewport origin and verify the XYZRHW -> clip conversion
  // correctly inverts the D3D9 viewport transform (including the -0.5 pixel
  // center convention).
  D3DDDIVIEWPORTINFO vp{};
  vp.X = 10.0f;
  vp.Y = 20.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  HRESULT hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport(X/Y origin)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  dev->cmd.reset();

  // Choose a vertex exactly on the top-left edge of the viewport in D3D9's
  // pixel-center convention: x = vp.X - 0.5, y = vp.Y - 0.5.
  //
  // This should invert to NDC (-1, +1).
  const VertexXyzrhwDiffuse tri[3] = {
      {9.5f, 19.5f, 0.25f, 0.5f, 0xFFFFFFFFu},
      {9.5f + 256.0f, 19.5f, 0.25f, 0.5f, 0xFFFFFFFFu},
      {9.5f, 19.5f + 256.0f, 0.25f, 0.5f, 0xFFFFFFFFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW viewport X/Y)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "viewport X/Y: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri), "viewport X/Y: scratch VB contains vertices")) {
      return false;
    }

    float clip_x = 0.0f;
    float clip_y = 0.0f;
    float clip_z = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_x, dev->up_vertex_buffer->storage.data() + 0, sizeof(float));
    std::memcpy(&clip_y, dev->up_vertex_buffer->storage.data() + 4, sizeof(float));
    std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(clip_w == 2.0f, "viewport X/Y: clip_w == 1/rhw")) {
      return false;
    }
    if (!Check(clip_x == -2.0f, "viewport X/Y: clip_x == -w (ndc_x=-1)")) {
      return false;
    }
    if (!Check(clip_y == 2.0f, "viewport X/Y: clip_y == +w (ndc_y=+1)")) {
      return false;
    }
    if (!Check(clip_z == 0.5f, "viewport X/Y: clip_z == z*w")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwConversionRhwZeroFallsBackToW1() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  dev->cmd.reset();

  // rhw == 0 is not meaningful, but the bring-up conversion uses a safe fallback
  // (w=1) rather than dividing by zero.
  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.25f, 0.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.25f, 0.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.25f, 0.0f, 0xFFFFFFFFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW rhw=0)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "rhw=0: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri), "rhw=0: scratch VB contains vertices")) {
      return false;
    }

    float clip_z = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(clip_w == 1.0f, "rhw=0: clip_w falls back to 1")) {
      return false;
    }
    if (!Check(clip_z == 0.25f, "rhw=0: clip_z == z*w (w=1)")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwConversionRhwNaNFallsBackToW1() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  dev->cmd.reset();
 
  // rhw is the reciprocal clip-space w. Non-finite rhw values are meaningless,
  // but the bring-up conversion must keep the math finite (w falls back to 1).
  const float nan = std::numeric_limits<float>::quiet_NaN();
  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.25f, nan, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.25f, nan, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.25f, nan, 0xFFFFFFFFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW rhw=NaN)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "rhw=NaN: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri), "rhw=NaN: scratch VB contains vertices")) {
      return false;
    }

    float clip_z = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(std::isfinite(clip_w), "rhw=NaN: clip_w is finite")) {
      return false;
    }
    if (!Check(clip_w == 1.0f, "rhw=NaN: clip_w falls back to 1")) {
      return false;
    }
    if (!Check(clip_z == 0.25f, "rhw=NaN: clip_z == z*w (w=1)")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwConversionRhwInfFallsBackToW1() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  dev->cmd.reset();

  // rhw is the reciprocal clip-space w. Non-finite rhw values are meaningless,
  // but the bring-up conversion must keep the math finite (w falls back to 1).
  const float inf = std::numeric_limits<float>::infinity();
  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.25f, inf, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.25f, inf, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.25f, inf, 0xFFFFFFFFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW rhw=Inf)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "rhw=Inf: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri), "rhw=Inf: scratch VB contains vertices")) {
      return false;
    }

    float clip_z = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(std::isfinite(clip_w), "rhw=Inf: clip_w is finite")) {
      return false;
    }
    if (!Check(clip_w == 1.0f, "rhw=Inf: clip_w falls back to 1")) {
      return false;
    }
    if (!Check(clip_z == 0.25f, "rhw=Inf: clip_z == z*w (w=1)")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwConversionNonFiniteXyzFallsBackToViewportCenter() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetViewport != nullptr, "pfnSetViewport is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Use a non-zero viewport origin so the "fallback to viewport center" behavior
  // is exercised.
  D3DDDIVIEWPORTINFO vp{};
  vp.X = 10.0f;
  vp.Y = 20.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  HRESULT hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  dev->cmd.reset();

  // XYZRHW conversion should keep math finite even if the app supplies NaN/Inf
  // coordinates; fall back to viewport center (NDC 0,0) and z=0.
  const float nan = std::numeric_limits<float>::quiet_NaN();
  const VertexXyzrhwDiffuse tri[3] = {
      {nan, nan, nan, 1.0f, 0xFFFFFFFFu},
      {nan, nan, nan, 1.0f, 0xFFFFFFFFu},
      {nan, nan, nan, 1.0f, 0xFFFFFFFFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW xyz=NaN)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "xyz=NaN: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri), "xyz=NaN: scratch VB contains vertices")) {
      return false;
    }

    float clip_x = 0.0f;
    float clip_y = 0.0f;
    float clip_z = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_x, dev->up_vertex_buffer->storage.data() + 0, sizeof(float));
    std::memcpy(&clip_y, dev->up_vertex_buffer->storage.data() + 4, sizeof(float));
    std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(std::isfinite(clip_x) && std::isfinite(clip_y) && std::isfinite(clip_z) && std::isfinite(clip_w),
               "xyz=NaN: clip coords are finite")) {
      return false;
    }
    if (!Check(clip_w == 1.0f, "xyz=NaN: clip_w == 1")) {
      return false;
    }
    if (!Check(clip_x == 0.0f, "xyz=NaN: clip_x == 0 at viewport center")) {
      return false;
    }
    if (!Check(clip_y == 0.0f, "xyz=NaN: clip_y == 0 at viewport center")) {
      return false;
    }
    if (!Check(clip_z == 0.0f, "xyz=NaN: clip_z == 0 (z sanitized)")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwConversionUsesEffectiveViewportFromRenderTarget() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderTarget != nullptr, "pfnSetRenderTarget is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Some runtimes rely on the implicit default viewport (full render target)
  // without ever calling SetViewport. Validate that XYZRHW conversion uses the
  // effective viewport derived from the current render target when the cached
  // viewport is unset (Width/Height <= 0).
  //
  // We pick a small render target so if the conversion accidentally falls back
  // to adapter dimensions (1024x768 by default) the expected NDC values differ.
  constexpr float rt_w = 640.0f;
  constexpr float rt_h = 480.0f;

  // Create a dummy render-target surface.
  D3D9DDIARG_CREATERESOURCE create_rt{};
  create_rt.type = 1u;   // D3DRTYPE_SURFACE
  create_rt.format = 22u; // D3DFMT_X8R8G8B8
  create_rt.width = static_cast<uint32_t>(rt_w);
  create_rt.height = static_cast<uint32_t>(rt_h);
  create_rt.depth = 1;
  create_rt.mip_levels = 1;
  create_rt.usage = 0x00000001u; // D3DUSAGE_RENDERTARGET
  create_rt.pool = 0;
  create_rt.size = 0;
  create_rt.hResource.pDrvPrivate = nullptr;
  create_rt.pSharedHandle = nullptr;
  create_rt.pPrivateDriverData = nullptr;
  create_rt.PrivateDriverDataSize = 0;
  create_rt.wddm_hAllocation = 0;

  HRESULT hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_rt);
  if (!Check(hr == S_OK, "CreateResource(render target surface)")) {
    return false;
  }
  if (!Check(create_rt.hResource.pDrvPrivate != nullptr, "CreateResource returned RT handle")) {
    return false;
  }
  cleanup.resources.push_back(create_rt.hResource);

  hr = cleanup.device_funcs.pfnSetRenderTarget(cleanup.hDevice, /*slot=*/0, create_rt.hResource);
  if (!Check(hr == S_OK, "SetRenderTarget(RT0)")) {
    return false;
  }

  // Do not call SetViewport: rely on viewport_effective_locked() fallback.
  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  dev->cmd.reset();

  // Vertex at the center of the effective viewport should map to NDC (0,0).
  const float cx = rt_w * 0.5f - 0.5f;
  const float cy = rt_h * 0.5f - 0.5f;
  const VertexXyzrhwDiffuse tri[3] = {
      {cx, cy, 0.25f, 1.0f, 0xFFFFFFFFu},
      {cx + 1.0f, cy, 0.25f, 1.0f, 0xFFFFFFFFu},
      {cx, cy + 1.0f, 0.25f, 1.0f, 0xFFFFFFFFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW effective viewport from RT)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "effective viewport from RT: scratch VB created")) {
      return false;
    }

    float clip_x = 0.0f;
    float clip_y = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_x, dev->up_vertex_buffer->storage.data() + 0, sizeof(float));
    std::memcpy(&clip_y, dev->up_vertex_buffer->storage.data() + 4, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(clip_w == 1.0f, "effective viewport from RT: clip_w == 1")) {
      return false;
    }
    if (!Check(clip_x == 0.0f, "effective viewport from RT: clip_x == 0 at center")) {
      return false;
    }
    if (!Check(clip_y == 0.0f, "effective viewport from RT: clip_y == 0 at center")) {
      return false;
    }
  }

  return true;
}

bool TestXyzrhwIndexedConversionUsesEffectiveViewportFromRenderTarget() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderTarget != nullptr, "pfnSetRenderTarget is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnLock != nullptr, "pfnLock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnUnlock != nullptr, "pfnUnlock is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "pfnSetStreamSource is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetIndices != nullptr, "pfnSetIndices is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawIndexedPrimitive != nullptr, "pfnDrawIndexedPrimitive is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Match TestXyzrhwConversionUsesEffectiveViewportFromRenderTarget(), but exercise
  // the VB/IB indexed draw path's XYZRHW expansion/conversion loop.
  constexpr float rt_w = 640.0f;
  constexpr float rt_h = 480.0f;

  // Create a dummy render-target surface and bind it to slot 0.
  D3D9DDIARG_CREATERESOURCE create_rt{};
  create_rt.type = 1u;    // D3DRTYPE_SURFACE
  create_rt.format = 22u; // D3DFMT_X8R8G8B8
  create_rt.width = static_cast<uint32_t>(rt_w);
  create_rt.height = static_cast<uint32_t>(rt_h);
  create_rt.depth = 1;
  create_rt.mip_levels = 1;
  create_rt.usage = 0x00000001u; // D3DUSAGE_RENDERTARGET
  create_rt.pool = 0;
  create_rt.size = 0;
  create_rt.hResource.pDrvPrivate = nullptr;
  create_rt.pSharedHandle = nullptr;
  create_rt.pPrivateDriverData = nullptr;
  create_rt.PrivateDriverDataSize = 0;
  create_rt.wddm_hAllocation = 0;

  HRESULT hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_rt);
  if (!Check(hr == S_OK, "CreateResource(render target surface)")) {
    return false;
  }
  if (!Check(create_rt.hResource.pDrvPrivate != nullptr, "CreateResource returned RT handle")) {
    return false;
  }
  cleanup.resources.push_back(create_rt.hResource);

  hr = cleanup.device_funcs.pfnSetRenderTarget(cleanup.hDevice, /*slot=*/0, create_rt.hResource);
  if (!Check(hr == S_OK, "SetRenderTarget(RT0)")) {
    return false;
  }

  // Do not call SetViewport: rely on viewport_effective_locked() fallback.
  hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  // Vertex at the center of the effective viewport should map to NDC (0,0).
  const float cx = rt_w * 0.5f - 0.5f;
  const float cy = rt_h * 0.5f - 0.5f;
  const VertexXyzrhwDiffuse verts[3] = {
      {cx, cy, 0.25f, 1.0f, 0xFFFFFFFFu},
      {cx + 1.0f, cy, 0.25f, 1.0f, 0xFFFFFFFFu},
      {cx, cy + 1.0f, 0.25f, 1.0f, 0xFFFFFFFFu},
  };

  // Create + fill a VB.
  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0u;
  create_vb.format = 0u;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pPrivateDriverData = nullptr;
  create_vb.PrivateDriverDataSize = 0;
  create_vb.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer xyzrhw|diffuse)")) {
    return false;
  }
  if (!Check(create_vb.hResource.pDrvPrivate != nullptr, "CreateResource returned vb handle")) {
    return false;
  }
  cleanup.resources.push_back(create_vb.hResource);

  D3D9DDIARG_LOCK lock_vb{};
  lock_vb.hResource = create_vb.hResource;
  lock_vb.offset_bytes = 0;
  lock_vb.size_bytes = 0;
  lock_vb.flags = 0;
  D3DDDI_LOCKEDBOX vb_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_vb, &vb_box);
  if (!Check(hr == S_OK, "Lock(vb xyzrhw|diffuse)")) {
    return false;
  }
  if (!Check(vb_box.pData != nullptr, "Lock(vb) returns pData")) {
    return false;
  }
  std::memcpy(vb_box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock_vb{};
  unlock_vb.hResource = create_vb.hResource;
  unlock_vb.offset_bytes = 0;
  unlock_vb.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_vb);
  if (!Check(hr == S_OK, "Unlock(vb xyzrhw|diffuse)")) {
    return false;
  }

  // Create + fill an index buffer (0,1,2).
  const uint16_t indices[3] = {0u, 1u, 2u};
  D3D9DDIARG_CREATERESOURCE create_ib{};
  create_ib.type = 0u;
  create_ib.format = 0u;
  create_ib.width = 0;
  create_ib.height = 0;
  create_ib.depth = 0;
  create_ib.mip_levels = 1;
  create_ib.usage = 0;
  create_ib.pool = 0;
  create_ib.size = sizeof(indices);
  create_ib.hResource.pDrvPrivate = nullptr;
  create_ib.pSharedHandle = nullptr;
  create_ib.pPrivateDriverData = nullptr;
  create_ib.PrivateDriverDataSize = 0;
  create_ib.wddm_hAllocation = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &create_ib);
  if (!Check(hr == S_OK, "CreateResource(index buffer u16)")) {
    return false;
  }
  if (!Check(create_ib.hResource.pDrvPrivate != nullptr, "CreateResource returned ib handle")) {
    return false;
  }
  cleanup.resources.push_back(create_ib.hResource);

  D3D9DDIARG_LOCK lock_ib{};
  lock_ib.hResource = create_ib.hResource;
  lock_ib.offset_bytes = 0;
  lock_ib.size_bytes = 0;
  lock_ib.flags = 0;
  D3DDDI_LOCKEDBOX ib_box{};
  hr = cleanup.device_funcs.pfnLock(cleanup.hDevice, &lock_ib, &ib_box);
  if (!Check(hr == S_OK, "Lock(ib u16)")) {
    return false;
  }
  if (!Check(ib_box.pData != nullptr, "Lock(ib) returns pData")) {
    return false;
  }
  std::memcpy(ib_box.pData, indices, sizeof(indices));

  D3D9DDIARG_UNLOCK unlock_ib{};
  unlock_ib.hResource = create_ib.hResource;
  unlock_ib.offset_bytes = 0;
  unlock_ib.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(cleanup.hDevice, &unlock_ib);
  if (!Check(hr == S_OK, "Unlock(ib u16)")) {
    return false;
  }

  // Bind VB/IB and draw.
  hr = cleanup.device_funcs.pfnSetStreamSource(
      cleanup.hDevice, /*stream=*/0, create_vb.hResource, /*offset=*/0, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "SetStreamSource(stream0=vb xyzrhw|diffuse)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetIndices(cleanup.hDevice, create_ib.hResource, static_cast<D3DDDIFORMAT>(101), 0);
  if (!Check(hr == S_OK, "SetIndices(ib index16)")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawIndexedPrimitive(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*base_vertex=*/0, /*min_index=*/0, /*num_vertices=*/3, /*start_index=*/0,
      /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawIndexedPrimitive(XYZRHW effective viewport from RT)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->up_vertex_buffer != nullptr, "indexed effective viewport from RT: scratch VB created")) {
      return false;
    }
    if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(verts),
               "indexed effective viewport from RT: scratch VB contains vertices")) {
      return false;
    }

    float clip_x = 0.0f;
    float clip_y = 0.0f;
    float clip_w = 0.0f;
    std::memcpy(&clip_x, dev->up_vertex_buffer->storage.data() + 0, sizeof(float));
    std::memcpy(&clip_y, dev->up_vertex_buffer->storage.data() + 4, sizeof(float));
    std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
    if (!Check(clip_w == 1.0f, "indexed effective viewport from RT: clip_w == 1")) {
      return false;
    }
    if (!Check(clip_x == 0.0f, "indexed effective viewport from RT: clip_x == 0 at center")) {
      return false;
    }
    if (!Check(clip_y == 0.0f, "indexed effective viewport from RT: clip_y == 0 at center")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncFogVertexModeEmitsConstants() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;     // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;      // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;  // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;      // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;        // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dRsFogVertexMode = 140u; // D3DRS_FOGVERTEXMODE
  constexpr uint32_t kD3dFogLinear = 3u;        // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Enable linear fog via FOGVERTEXMODE (FOGTABLEMODE must be NONE so vertex fog
  // is the active mode).
  constexpr float fog_start = 0.25f;
  constexpr float fog_end = 0.75f;
  constexpr float inv_range = 2.0f;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogVertexMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGVERTEXMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  dev->cmd.reset();

  // Use an RHW != 1 vertex (w != 1) so fixed-function fog must divide clip_z by
  // clip_w to recover POSITIONT.z (our fixed-function fog coordinate convention).
  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, 0.5f, 0xFF00FF00u, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.25f, 0.5f, 0xFF00FF00u, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.25f, 0.5f, 0xFF00FF00u, 0.0f, 1.0f},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(fog vertex mode)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "fog vertex mode: VS bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsPassthroughPosColorTex1Fog),
               "fog vertex mode: selected fog VS variant")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->vs, /*rcp=*/0x03000006u), "fog vertex mode: VS divides by w (rcp)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->vs, /*mul=*/0x04000005u), "fog vertex mode: VS divides by w (mul)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->vs, /*v0.wwww=*/0x10FF0000u), "fog vertex mode: VS reads v0.w")) {
      return false;
    }
    if (dev->up_vertex_buffer) {
      // Verify XYZRHW conversion produced clip_w != 1 and clip_z = ndc_z * clip_w
      // so `clip_z / clip_w` recovers the original POSITIONT.z.
      if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri),
                 "fog vertex mode: scratch VB storage contains uploaded vertices")) {
        return false;
      }
      float clip_z = 0.0f;
      float clip_w = 0.0f;
      std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
      std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
      if (!Check(clip_w == 2.0f, "fog vertex mode: XYZRHW conversion produced clip_w == 2")) {
        return false;
      }
      if (!Check(clip_z == 0.5f, "fog vertex mode: XYZRHW conversion produced clip_z == z*w")) {
        return false;
      }
    }
    if (!Check(dev->ps != nullptr, "fog vertex mode: PS bound")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u), "fog vertex mode: PS references c1 (fog color)")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(fog vertex mode constants)")) {
    return false;
  }

  const float expected[8] = {
      // c1: fog color (RGBA from ARGB red).
      1.0f, 0.0f, 0.0f, 1.0f,
      // c2: fog params (x=fog_start, y=inv_fog_range, z/w unused).
      fog_start, inv_range, 0.0f, 0.0f,
  };

  size_t uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 1u || sc->vec4_count != 2u) {
      continue;
    }
    const size_t need = sizeof(*sc) + sizeof(expected);
    if (!Check(hdr->size_bytes >= need, "fog vertex mode: SET_SHADER_CONSTANTS_F contains payload")) {
      return false;
    }
    const auto* payload = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
    if (std::memcmp(payload, expected, sizeof(expected)) != 0) {
      return Check(false, "fog vertex mode payload matches expected c1/c2 data");
    }
    ++uploads;
  }
  if (!Check(uploads == 1, "fog vertex mode constants uploaded once")) {
    return false;
  }

  return true;
}

bool TestFixedfuncFogRhwColorSelectsFogVsAndUsesWDivision() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;    // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;     // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u; // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;     // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;       // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;       // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  // Enable table fog (linear) and choose exactly-representable floats so payload
  // comparisons are stable.
  constexpr float fog_start = 0.25f;
  constexpr float fog_end = 0.75f;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  dev->cmd.reset();

  // Use RHW != 1 so fog VS must divide clip-space z by clip-space w before
  // emitting TEXCOORD0.z for the fog PS.
  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.25f, 0.5f, 0xFF00FF00u},
      {1.0f, 0.0f, 0.25f, 0.5f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.25f, 0.5f, 0xFF00FF00u},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW|DIFFUSE; fog enabled)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "fog RHW_COLOR: VS bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsPassthroughPosColorFog),
               "fog RHW_COLOR: selected kVsPassthroughPosColorFog")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->vs, /*rcp=*/0x03000006u), "fog RHW_COLOR: VS divides by w (rcp)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->vs, /*mul=*/0x04000005u), "fog RHW_COLOR: VS divides by w (mul)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->vs, /*v0.wwww=*/0x10FF0000u), "fog RHW_COLOR: VS reads v0.w")) {
      return false;
    }
    if (dev->up_vertex_buffer) {
      if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri),
                 "fog RHW_COLOR: scratch VB storage contains uploaded vertices")) {
        return false;
      }
      float clip_z = 0.0f;
      float clip_w = 0.0f;
      std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
      std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
      if (!Check(clip_w == 2.0f, "fog RHW_COLOR: XYZRHW conversion produced clip_w == 2")) {
        return false;
      }
      if (!Check(clip_z == 0.5f, "fog RHW_COLOR: XYZRHW conversion produced clip_z == z*w")) {
        return false;
      }
    }
    if (!Check(dev->ps != nullptr, "fog RHW_COLOR: PS bound")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u), "fog RHW_COLOR: PS references c1 (fog color)")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncFogRhwTex1SelectsFogVsAndUsesWDivision() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;      // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;       // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;   // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;       // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;         // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dRsFogVertexMode = 140u; // D3DRS_FOGVERTEXMODE
  constexpr uint32_t kD3dFogLinear = 3u;         // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|TEX1)")) {
    return false;
  }

  // Enable linear fog via FOGVERTEXMODE (FOGTABLEMODE must be NONE so vertex fog
  // is the active mode).
  constexpr float fog_start = 0.25f;
  constexpr float fog_end = 0.75f;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogVertexMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGVERTEXMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  dev->cmd.reset();

  // Use RHW != 1 so fog VS must divide clip-space z by clip-space w before
  // emitting TEXCOORD0.z for the fog PS.
  const VertexXyzrhwTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, 0.5f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.25f, 0.5f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.25f, 0.5f, 0.0f, 1.0f},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZRHW|TEX1; fog enabled)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "fog RHW_TEX1: VS bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsPassthroughPosWhiteTex1Fog),
               "fog RHW_TEX1: selected kVsPassthroughPosWhiteTex1Fog")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->vs, /*rcp=*/0x03000006u), "fog RHW_TEX1: VS divides by w (rcp)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->vs, /*mul=*/0x04000005u), "fog RHW_TEX1: VS divides by w (mul)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->vs, /*v0.wwww=*/0x10FF0000u), "fog RHW_TEX1: VS reads v0.w")) {
      return false;
    }
    if (dev->up_vertex_buffer) {
      if (!Check(dev->up_vertex_buffer->storage.size() >= sizeof(tri),
                 "fog RHW_TEX1: scratch VB storage contains uploaded vertices")) {
        return false;
      }
      float clip_z = 0.0f;
      float clip_w = 0.0f;
      std::memcpy(&clip_z, dev->up_vertex_buffer->storage.data() + 8, sizeof(float));
      std::memcpy(&clip_w, dev->up_vertex_buffer->storage.data() + 12, sizeof(float));
      if (!Check(clip_w == 2.0f, "fog RHW_TEX1: XYZRHW conversion produced clip_w == 2")) {
        return false;
      }
      if (!Check(clip_z == 0.5f, "fog RHW_TEX1: XYZRHW conversion produced clip_z == z*w")) {
        return false;
      }
    }
    if (!Check(dev->ps != nullptr, "fog RHW_TEX1: PS bound")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u), "fog RHW_TEX1: PS references c1 (fog color)")) {
      return false;
    }
  }

  return true;
}

bool TestVsOnlyInteropIgnoresFogState() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateShader != nullptr, "pfnCreateShader is available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetShader != nullptr, "pfnSetShader is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;    // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;     // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u; // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;     // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;       // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;       // D3DFOG_LINEAR

  // Use a vertex format + stage state that produces a non-trivial fixed-function
  // PS (one texld). We'll then enable fog and verify that in VS-only interop
  // mode the PS does *not* gain fog behavior or fog constant uploads.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  if (!CreateDummyTexture(&cleanup, &hTex0)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: modulate tex0 * diffuse.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopModulate, "stage0 COLOROP=MODULATE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg2, kD3dTaDiffuse, "stage0 COLORARG2=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE")) {
    return false;
  }

  // Bind only a user VS (PS stays NULL) to enter VS-only interop mode.
  D3D9DDI_HSHADER hUserVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3dShaderStageVs,
                                            fixedfunc::kVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(fixedfunc::kVsPassthroughPosColor)),
                                            &hUserVs);
  if (!Check(hr == S_OK, "CreateShader(user VS)")) {
    return false;
  }
  cleanup.shaders.push_back(hUserVs);
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3dShaderStageVs, hUserVs);
  if (!Check(hr == S_OK, "SetShader(VS=user)")) {
    return false;
  }

  // Enable fog while still in VS-only interop. The fixed-function PS key
  // intentionally ignores fog in this mode so the PS input layout remains stable.
  constexpr float fog_start = 0.25f;
  constexpr float fog_end = 0.75f;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1; VS-only interop)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR; VS-only interop)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red; VS-only interop)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART; VS-only interop)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND; VS-only interop)")) {
    return false;
  }

  // Draw once and ensure the command stream does not upload fog constants (c1..c2).
  dev->cmd.reset();
  const VertexXyzrhwDiffuseTex1 tri[3] = {
      // Treat POSITIONT as clip-space (since the user VS does a pass-through mov oPos, v0).
      {-1.0f, -1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {-1.0f, 1.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only interop; fog enabled)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->user_vs != nullptr && dev->vs == dev->user_vs, "VS-only interop: user VS bound")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "VS-only interop: PS bound")) {
      return false;
    }
    if (!Check(ShaderCountToken(dev->ps, kPsOpTexld) == 1, "VS-only interop: stage0 PS contains 1 texld")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, 0x20E40001u), "VS-only interop: PS does not reference fog c1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(VS-only interop fog)")) {
    return false;
  }
  size_t fog_uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage == AEROGPU_SHADER_STAGE_PIXEL && sc->start_register == 1u && sc->vec4_count == 2u) {
      ++fog_uploads;
    }
  }
  if (!Check(fog_uploads == 0, "VS-only interop: does not upload fog constants")) {
    return false;
  }

  return true;
}

bool TestFixedfuncFogTableModeTakesPrecedenceOverVertexMode() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;     // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;      // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;  // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;      // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;        // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dRsFogVertexMode = 140u; // D3DRS_FOGVERTEXMODE
  constexpr uint32_t kD3dFogExp = 1u;           // D3DFOG_EXP
  constexpr uint32_t kD3dFogLinear = 3u;        // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // D3D9 semantics: if table fog is enabled (mode != NONE), it takes precedence
  // over vertex fog. Since AeroGPU only implements LINEAR, a non-LINEAR table
  // mode should disable fog entirely even if vertex mode is LINEAR.
  constexpr float fog_start = 0.25f;
  constexpr float fog_end = 0.75f;
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogVertexMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGVERTEXMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogExp);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=EXP)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(fog_start));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(fog_end));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  dev->cmd.reset();

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.25f, 1.0f, 0xFF00FF00u, 0.0f, 1.0f},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzrhwDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(table fog precedence)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "table fog precedence: VS bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsPassthroughPosColorTex1),
               "table fog precedence: fog VS not selected")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "table fog precedence: PS bound")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, 0x20E40001u),
               "table fog precedence: PS does not reference c1 (fog color)")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(table fog precedence)")) {
    return false;
  }

  // Ensure fog constant uploads (pixel shader c1..c2) did not occur.
  size_t fog_const_uploads = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage == AEROGPU_SHADER_STAGE_PIXEL && sc->start_register == 1u && sc->vec4_count == 2u) {
      ++fog_const_uploads;
    }
  }
  if (!Check(fog_const_uploads == 0, "table fog precedence: does not upload fog constants")) {
    return false;
  }

  return true;
}

bool TestFvfXyzDiffuseFogSelectsFogVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;    // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;     // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u; // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;     // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;       // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;       // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: output diffuse (no texture sampling). Disable later stages.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopSelectArg1, "stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaDiffuse, "stage0 COLORARG1=DIFFUSE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE")) {
    return false;
  }

  // Enable linear fog.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(0.2f));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(0.8f));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  dev->cmd.reset();
  const VertexXyzDiffuse tri[3] = {
      {0.0f, 0.0f, 0.25f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.25f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.25f, 0xFFFFFFFFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|DIFFUSE; fog enabled)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "XYZ|DIFFUSE fog: VS bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColorFog),
               "XYZ|DIFFUSE fog: selected kVsWvpPosColorFog")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "XYZ|DIFFUSE fog: PS bound")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u), "XYZ|DIFFUSE fog: PS references c1 (fog color)")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzDiffuseTex1FogSelectsFogVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;    // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;     // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u; // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;     // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;       // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;       // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  if (!CreateDummyTexture(&cleanup, &hTex0)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: output texture0. Disable later stages.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopSelectArg1, "stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE")) {
    return false;
  }

  // Enable linear fog.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(0.2f));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(0.8f));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  dev->cmd.reset();
  const VertexXyzDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.25f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.25f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|DIFFUSE|TEX1; fog enabled)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "XYZ|DIFFUSE|TEX1 fog: VS bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColorTex0Fog),
               "XYZ|DIFFUSE|TEX1 fog: selected kVsWvpPosColorTex0Fog")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "XYZ|DIFFUSE|TEX1 fog: PS bound")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u), "XYZ|DIFFUSE|TEX1 fog: PS references c1 (fog color)")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzTex1FogSelectsFogVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;    // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;     // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u; // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;     // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;       // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;       // D3DFOG_LINEAR

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  if (!CreateDummyTexture(&cleanup, &hTex0)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  const auto SetTextureStageState = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (cleanup.device_funcs.pfnSetTextureStageState) {
      hr2 = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, state, value);
    }
    return Check(hr2 == S_OK, msg);
  };

  // Stage0: output texture0. Disable later stages.
  if (!SetTextureStageState(0, kD3dTssColorOp, kD3dTopSelectArg1, "stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssColorArg1, kD3dTaTexture, "stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  if (!SetTextureStageState(0, kD3dTssAlphaOp, kD3dTopDisable, "stage0 ALPHAOP=DISABLE")) {
    return false;
  }
  if (!SetTextureStageState(1, kD3dTssColorOp, kD3dTopDisable, "stage1 COLOROP=DISABLE")) {
    return false;
  }

  // Enable linear fog.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(0.2f));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(0.8f));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  dev->cmd.reset();
  const VertexXyzTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.25f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.25f, 0.0f, 1.0f},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|TEX1; fog enabled)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "XYZ|TEX1 fog: VS bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsTransformPosWhiteTex1Fog),
               "XYZ|TEX1 fog: selected kVsTransformPosWhiteTex1Fog")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "XYZ|TEX1 fog: PS bound")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u), "XYZ|TEX1 fog: PS references c1 (fog color)")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzNormalFogSelectsFogVsAndLitFogVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;     // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;      // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;  // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;      // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;        // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;        // D3DFOG_LINEAR
  constexpr uint32_t kD3dRsLighting = 137u;     // D3DRS_LIGHTING
  constexpr uint32_t kD3dRsAmbient = 26u;       // D3DRS_AMBIENT

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormal);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL)")) {
    return false;
  }

  // Enable linear fog.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(0.2f));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(0.8f));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  // Lighting off: should select the unlit fog VS variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 0u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=FALSE)")) {
    return false;
  }

  const VertexXyzNormal tri[3] = {
      {0.0f, 0.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f},
      {1.0f, 0.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f},
      {0.0f, 1.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormal));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL; fog on; lighting off)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "XYZ|NORMAL fog: VS bound (lighting off)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalWhiteFog),
               "XYZ|NORMAL fog: selected unlit fog VS variant")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "XYZ|NORMAL fog: PS bound (lighting off)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u), "XYZ|NORMAL fog: PS references c1 (fog color)")) {
      return false;
    }
  }

  // Lighting on: should select lit+fog VS variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF000000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.0f, 0.0f, -1.0f};
  light0.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormal));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL; fog on; lighting on)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "XYZ|NORMAL fog: VS bound (lighting on)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpLitPosNormalFog),
               "XYZ|NORMAL fog: selected lit fog VS variant")) {
      return false;
    }
    if (!Check(ShaderReferencesConstRegister(dev->vs, kFixedfuncLightingStartRegister),
               "XYZ|NORMAL fog: lit fog VS references lighting start register c208")) {
      return false;
    }
    if (!Check(!ShaderReferencesConstRegister(dev->vs, 244u),
               "XYZ|NORMAL fog: lit fog VS does not reference legacy c244 layout")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "XYZ|NORMAL fog: PS bound (lighting on)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u), "XYZ|NORMAL fog: PS references c1 (fog color)")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzNormalTex1FogSelectsFogVsAndLitFogVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;     // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;      // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;  // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;      // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;        // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;        // D3DFOG_LINEAR
  constexpr uint32_t kD3dRsLighting = 137u;     // D3DRS_LIGHTING
  constexpr uint32_t kD3dRsAmbient = 26u;       // D3DRS_AMBIENT

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|TEX1)")) {
    return false;
  }

  // Enable linear fog.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(0.2f));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(0.8f));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  // Lighting off: should select the unlit fog VS variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 0u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=FALSE)")) {
    return false;
  }

  const VertexXyzNormalTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, /*u=*/0.0f, /*v=*/0.0f},
      {1.0f, 0.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, /*u=*/1.0f, /*v=*/0.0f},
      {0.0f, 1.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, /*u=*/0.0f, /*v=*/1.0f},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|TEX1; fog on; lighting off)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "XYZ|NORMAL|TEX1 fog: VS bound (lighting off)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalWhiteTex0Fog),
               "XYZ|NORMAL|TEX1 fog: selected unlit fog VS variant")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "XYZ|NORMAL|TEX1 fog: PS bound (lighting off)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u), "XYZ|NORMAL|TEX1 fog: PS references c1 (fog color)")) {
      return false;
    }
  }

  // Lighting on: should select lit+fog VS variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF000000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.0f, 0.0f, -1.0f};
  light0.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|TEX1; fog on; lighting on)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "XYZ|NORMAL|TEX1 fog: VS bound (lighting on)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpLitPosNormalTex1Fog),
               "XYZ|NORMAL|TEX1 fog: selected lit fog VS variant")) {
      return false;
    }
    if (!Check(ShaderReferencesConstRegister(dev->vs, kFixedfuncLightingStartRegister),
               "XYZ|NORMAL|TEX1 fog: lit fog VS references lighting start register c208")) {
      return false;
    }
    if (!Check(!ShaderReferencesConstRegister(dev->vs, 244u),
               "XYZ|NORMAL|TEX1 fog: lit fog VS does not reference legacy c244 layout")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "XYZ|NORMAL|TEX1 fog: PS bound (lighting on)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u), "XYZ|NORMAL|TEX1 fog: PS references c1 (fog color)")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzNormalDiffuseFogSelectsFogVsAndLitFogVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;    // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;     // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u; // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;     // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;       // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;       // D3DFOG_LINEAR
  constexpr uint32_t kD3dRsLighting = 137u;    // D3DRS_LIGHTING
  constexpr uint32_t kD3dRsAmbient = 26u;      // D3DRS_AMBIENT

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE)")) {
    return false;
  }

  // Enable linear fog.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(0.2f));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(0.8f));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  // Lighting off: should select the unlit fog VS variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 0u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=FALSE)")) {
    return false;
  }

  const VertexXyzNormalDiffuse tri[3] = {
      {0.0f, 0.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|DIFFUSE; fog on; lighting off)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "XYZ|NORMAL|DIFFUSE fog: VS bound (lighting off)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalDiffuseFog),
               "XYZ|NORMAL|DIFFUSE fog: selected unlit fog VS variant")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "XYZ|NORMAL|DIFFUSE fog: PS bound (lighting off)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u), "XYZ|NORMAL|DIFFUSE fog: PS references c1 (fog color)")) {
      return false;
    }
  }

  // Lighting on: should select lit+fog VS variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsAmbient, 0xFF000000u);
  if (!Check(hr == S_OK, "SetRenderState(AMBIENT=black)")) {
    return false;
  }

  D3DLIGHT9 light0{};
  light0.Type = D3DLIGHT_DIRECTIONAL;
  light0.Direction = {0.0f, 0.0f, -1.0f};
  light0.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  light0.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  hr = device_set_light(cleanup.hDevice, /*index=*/0, &light0);
  if (!Check(hr == S_OK, "SetLight(0)")) {
    return false;
  }
  hr = device_light_enable(cleanup.hDevice, /*index=*/0, TRUE);
  if (!Check(hr == S_OK, "LightEnable(0, TRUE)")) {
    return false;
  }

  D3DMATERIAL9 mat{};
  mat.Diffuse = {1.0f, 1.0f, 1.0f, 1.0f};
  mat.Ambient = {0.0f, 0.0f, 0.0f, 1.0f};
  mat.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};
  hr = device_set_material(cleanup.hDevice, &mat);
  if (!Check(hr == S_OK, "SetMaterial")) {
    return false;
  }

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|DIFFUSE; fog on; lighting on)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "XYZ|NORMAL|DIFFUSE fog: VS bound (lighting on)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpLitPosNormalDiffuseFog),
               "XYZ|NORMAL|DIFFUSE fog: selected lit fog VS variant")) {
      return false;
    }
    if (!Check(ShaderReferencesConstRegister(dev->vs, kFixedfuncLightingStartRegister),
               "XYZ|NORMAL|DIFFUSE fog: lit fog VS references lighting start register c208")) {
      return false;
    }
    if (!Check(!ShaderReferencesConstRegister(dev->vs, 244u),
               "XYZ|NORMAL|DIFFUSE fog: lit fog VS does not reference legacy c244 layout")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "XYZ|NORMAL|DIFFUSE fog: PS bound (lighting on)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u), "XYZ|NORMAL|DIFFUSE fog: PS references c1 (fog color)")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzNormalDiffuseTex1FogSelectsFogVsWhenLightingOff() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;    // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;     // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u; // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;     // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;       // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;       // D3DFOG_LINEAR
  constexpr uint32_t kD3dRsLighting = 137u;    // D3DRS_LIGHTING

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE|TEX1)")) {
    return false;
  }

  // Enable linear fog.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR=red)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(0.2f));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(0.8f));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  // Lighting off: should select the unlit fog VS variant.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 0u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=FALSE)")) {
    return false;
  }

  const VertexXyzNormalDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu, /*u=*/0.0f, /*v=*/0.0f},
      {1.0f, 0.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu, /*u=*/1.0f, /*v=*/0.0f},
      {0.0f, 1.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFFFFFFFFu, /*u=*/0.0f, /*v=*/1.0f},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(XYZ|NORMAL|DIFFUSE|TEX1; fog on; lighting off)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "XYZ|NORMAL|DIFFUSE|TEX1 fog: VS bound (lighting off)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosNormalDiffuseTex1Fog),
               "XYZ|NORMAL|DIFFUSE|TEX1 fog: selected unlit fog VS variant")) {
      return false;
    }
    if (!Check(dev->ps != nullptr, "XYZ|NORMAL|DIFFUSE|TEX1 fog: PS bound (lighting off)")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, 0x20E40001u),
               "XYZ|NORMAL|DIFFUSE|TEX1 fog: PS references c1 (fog color)")) {
      return false;
    }
  }

  return true;
}

bool TestFvfXyzNormalDiffuseTex1FogLightingSelectsLitFogVs() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;     // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;      // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;  // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;      // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;        // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;        // D3DFOG_LINEAR

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzNormalDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|NORMAL|DIFFUSE|TEX1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  // Start with fog disabled.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0)")) {
    return false;
  }

  const VertexXyzNormalDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFF00FF00u, /*u=*/0.0f, /*v=*/0.0f},
      {1.0f, 0.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFF00FF00u, /*u=*/1.0f, /*v=*/0.0f},
      {0.0f, 1.0f, 0.25f, /*nx=*/0.0f, /*ny=*/0.0f, /*nz=*/1.0f, 0xFF00FF00u, /*u=*/0.0f, /*v=*/1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(fog off; lit TEX1)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "VS bound (fog off)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpLitPosNormalDiffuseTex1),
               "fog off: VS bytecode == fixedfunc::kVsWvpLitPosNormalDiffuseTex1")) {
      return false;
    }
  }

  // Enable linear fog and draw again; fixed-function fallback should select a new
  // VS+PS variant (lit + fog).
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, F32Bits(0.2f));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, F32Bits(0.8f));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexXyzNormalDiffuseTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(fog on; lit TEX1)")) {
    return false;
  }

  Shader* vs_on = nullptr;
  Shader* ps_on = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    vs_on = dev->vs;
    ps_on = dev->ps;
    if (!Check(vs_on != nullptr, "VS bound (fog on)")) {
      return false;
    }
    if (!Check(ps_on != nullptr, "PS bound (fog on)")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(vs_on, fixedfunc::kVsWvpLitPosNormalDiffuseTex1Fog),
               "fog on: VS bytecode == fixedfunc::kVsWvpLitPosNormalDiffuseTex1Fog")) {
      return false;
    }
    if (!Check(ShaderReferencesConstRegister(vs_on, kFixedfuncLightingStartRegister),
               "fog on: lit fog VS references lighting start register c208")) {
      return false;
    }
    if (!Check(!ShaderReferencesConstRegister(vs_on, 244u), "fog on: lit fog VS does not reference legacy c244 layout")) {
      return false;
    }
  }

  if (!Check(ShaderContainsToken(ps_on, kPsOpAdd), "fog PS contains add opcode")) {
    return false;
  }
  if (!Check(ShaderContainsToken(ps_on, kPsOpMul), "fog PS contains mul opcode")) {
    return false;
  }
  if (!Check(ShaderContainsToken(ps_on, 0x20E40001u), "fog PS references c1 (fog color)")) {
    return false;
  }

  return true;
}

} // namespace
} // namespace aerogpu

int main() {
  if (!aerogpu::TestFvfXyzrhwDiffuseEmitsSaneCommands()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzrhwDiffuseLightingEnabledStillDraws()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseLightingEnabledFailsInvalidCall()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseEmitsInputLayoutAndShaders()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseEmitsTransformConstantsAndDecl()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseWvpUploadNotDuplicatedByFirstDraw()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseRedundantSetTransformDoesNotReuploadWvp()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseMultiplyTransformEagerUploadNotDuplicatedByFirstDraw()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseRedundantSetFvfDoesNotReuploadWvp()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseWvpDirtyAfterUserVsAndConstClobber()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseLightingDirtyAfterUserVsAndConstClobber()) {
    return 1;
  }
  if (!aerogpu::TestSetShaderConstFDedupSkipsRedundantUpload()) {
    return 1;
  }
  if (!aerogpu::TestSetShaderConstFStateBlockCapturesRedundantSet()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockUploadsTextureFactorConstantWhenUsed()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockUploadsWvpConstantsForTransformChanges()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockDuringStateBlockRecordingCapturesShaderBindings()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockShaderConstIAndBEmitCommands()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockSamplerStateCapturesRedundantSet()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockVertexDeclCapturesRedundantSet()) {
    return 1;
  }
  if (!aerogpu::TestCaptureStateBlockStreamSourceFreqAffectsApplyStateBlock()) {
    return 1;
  }
  if (!aerogpu::TestCaptureStateBlockUsesEffectiveViewportFromRenderTarget()) {
    return 1;
  }
  if (!aerogpu::TestCaptureStateBlockUsesEffectiveScissorRectFromRenderTarget()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockRenderStateCapturesRedundantSet()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockScissorRenderStateEmitsSetScissor()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockScissorRectEmitsSetScissor()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockViewportEmitsSetViewport()) {
    return 1;
  }
  if (!aerogpu::TestSetViewportSanitizesNonFiniteValues()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockStreamSourceAndIndexBufferEmitCommands()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockRenderTargetEmitsSetRenderTargets()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockDepthStencilEmitsSetRenderTargets()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockToleratesUnsupportedTextureStageState()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockFvfChangeReuploadsWvpConstantsAfterConstClobber()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockUpdatesFixedfuncPsWhenTextureBindingChanges()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockUpdatesFixedfuncPsForStageStateInVsOnlyInterop()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockUpdatesFixedfuncPsWhenTextureBindingChangesInVsOnlyInterop()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockFogRenderStateAffectsNextDraw()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockFogDoesNotSelectFogPsInVsOnlyInterop()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockFogDoesNotSelectFogVsInPsOnlyInterop()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockLightingEnableReuploadsConstantsAfterClobber()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockLight1ChangeReuploadsLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockLight1DisableReuploadsLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseDrawPrimitiveVbUploadsWvpAndBindsVb()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzrhwDiffuseTex1EmitsTextureAndShaders()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseTex1EmitsTextureAndShaders()) {
    return 1;
  }
  if (!aerogpu::TestFixedfuncTex1SupportsTexcoordSizeBits()) {
    return 1;
  }
  if (!aerogpu::TestFixedfuncTex1NoDiffuseSupportsTexcoordSizeBits()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseTex1EmitsTransformConstantsAndDecl()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseTex1DrawPrimitiveVbUploadsWvpAndBindsVb()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzrhwTex1EmitsTextureAndShaders()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzTex1EmitsTransformConstantsAndDecl()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzTex1DrawPrimitiveVbUploadsWvpAndBindsVb()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalLightingSelectsLitVs()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalTex1LightingSelectsLitVs()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalEmitsLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalTex1EmitsLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseLightingSelectsLitVs()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseEmitsLightingConstantsAndTracksDirty()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseRedundantRenderStateDoesNotReuploadLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseRedundantDirtyTriggersDoNotReuploadLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseNormalizesDirectionalLightDirection()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseNormalizesDirectionalLightDirectionAfterViewTransform()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseGlobalAmbientPreservesAlphaChannel()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseLightingOffDoesNotUploadLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseProjectionChangeReuploadsWvpButNotLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseDisablingLight0ShiftsPackedLights()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffusePacksMultipleLights()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffusePacksPointLightConstants()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffusePointLightAtt0AndRangeFallbacks()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseDisablingPointLight0ShiftsPackedPointLights()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseTreatsSpotLightsAsPointLights()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseIgnoresExtraDirectionalLightsBeyondFixedfuncLimit()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseIgnoresExtraPointLightsBeyondFixedfuncLimit()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseTransformsLightDirectionByView()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseTransformsPointLightPositionByView()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseTransformsPointLightPositionByViewRotation()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseDoesNotTransformLightDirectionByWorld()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseDoesNotTransformPointLightPositionByWorld()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseTex1LightingSelectsLitVs()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseTex1EmitsLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestVertexDeclXyzrhwTex1InfersFvfAndBindsShaders()) {
    return 1;
  }
  if (!aerogpu::TestVertexDeclXyzTex1InfersFvfAndUploadsWvp()) {
    return 1;
  }
  if (!aerogpu::TestVertexDeclXyzTex1DrawPrimitiveVbUploadsWvpAndBindsVb()) {
    return 1;
  }
  if (!aerogpu::TestVertexDeclXyzDiffuseDrawPrimitiveVbUploadsWvpAndKeepsDecl()) {
    return 1;
  }
  if (!aerogpu::TestVertexDeclXyzDiffuseTex1DrawPrimitiveVbUploadsWvpAndKeepsDecl()) {
    return 1;
  }
  if (!aerogpu::TestSetTextureStageStateUpdatesPsForTex1NoDiffuseFvfs()) {
    return 1;
  }
  if (!aerogpu::TestSetTextureStageStateUpdatesPsForLitTex1Fvfs()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropXyzrhwTex1SynthesizesVs()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropXyzTex1SynthesizesVsAndUploadsWvp()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropXyzTex1FogEnabledDoesNotSelectFogVs()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropXyzNormalIgnoresLightingAndDoesNotUploadLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropXyzNormalTex1IgnoresLightingAndDoesNotUploadLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropXyzNormalDiffuseIgnoresLightingAndDoesNotUploadLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropXyzNormalDiffuseTex1IgnoresLightingAndDoesNotUploadLightingConstants()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropVertexDeclXyzrhwTex1SynthesizesVs()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyInteropVertexDeclXyzTex1SynthesizesVsAndUploadsWvp()) {
    return 1;
  }
  if (!aerogpu::TestSetTextureStageStateUpdatesPsForTex1NoDiffuseVertexDeclFvfs()) {
    return 1;
  }
  if (!aerogpu::TestGetTextureStageStateRoundTrips()) {
    return 1;
  }
  if (!aerogpu::TestStageStateChangeRebindsShadersIfImplemented()) {
    return 1;
  }
  if (!aerogpu::TestStage0OpExpansionSelectsShadersAndCaches()) {
    return 1;
  }
  if (!aerogpu::TestStage0ArgModifiersEmitSourceMods()) {
    return 1;
  }
  if (!aerogpu::TestStage0IgnoresUnusedArgsAndOps()) {
    return 1;
  }
  if (!aerogpu::TestStage0CurrentCanonicalizesToDiffuse()) {
    return 1;
  }
  if (!aerogpu::TestStage0NoTextureCanonicalizesAndReusesShader()) {
    return 1;
  }
  if (!aerogpu::TestStage1TextureEnableAddsSecondTexld()) {
    return 1;
  }
  if (!aerogpu::TestStage1ColorDisableIgnoresUnsupportedAlphaAndDoesNotSampleTexture1()) {
    return 1;
  }
  if (!aerogpu::TestStage0NoTextureAllowsStage1ToSampleTexture1WithSampler1()) {
    return 1;
  }
  if (!aerogpu::TestStage2SamplingUsesSampler2EvenIfStage1DoesNotSample()) {
    return 1;
  }
  if (!aerogpu::TestStage1MissingTextureDisablesStage2Sampling()) {
    return 1;
  }
  if (!aerogpu::TestStage1BlendTextureAlphaRequiresTextureEvenWithoutTextureArgs()) {
    return 1;
  }
  if (!aerogpu::TestStage3SamplingUsesSampler3EvenIfStage1AndStage2DoNotSample()) {
    return 1;
  }
  if (!aerogpu::TestApplyStateBlockUpdatesFixedfuncPsForTextureStageState()) {
    return 1;
  }
  if (!aerogpu::TestStage0UnsupportedArgFailsAtDraw()) {
    return 1;
  }
  if (!aerogpu::TestStage0VariantCacheEvictsOldShaders()) {
    return 1;
  }
  if (!aerogpu::TestStage0SignatureCacheDoesNotPointAtEvictedShaders()) {
    return 1;
  }
  if (!aerogpu::TestTextureFactorRenderStateUpdatesPsConstantWhenUsed()) {
    return 1;
  }
  if (!aerogpu::TestTextureFactorConstantReuploadAfterPsConstClobber()) {
    return 1;
  }
  if (!aerogpu::TestFixedfuncFogEmitsConstants()) {
    return 1;
  }
  if (!aerogpu::TestFixedfuncFogConstantsDedupAndReuploadOnChange()) {
    return 1;
  }
  if (!aerogpu::TestFixedfuncFogConstantsReuploadAfterPsConstClobber()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwConversionIgnoresViewportMinMaxZ()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwIndexedConversionIgnoresViewportMinMaxZ()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwIndexedConversionAppliesViewportXyAndPixelCenterBias()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwIndexedConversionRhwZeroFallsBackToW1()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwIndexedConversionRhwNaNFallsBackToW1()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwIndexedConversionRhwInfFallsBackToW1()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwIndexedConversionNonFiniteXyzFallsBackToViewportCenter()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwConversionAppliesViewportXyAndPixelCenterBias()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwConversionRhwZeroFallsBackToW1()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwConversionRhwNaNFallsBackToW1()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwConversionRhwInfFallsBackToW1()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwConversionNonFiniteXyzFallsBackToViewportCenter()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwConversionUsesEffectiveViewportFromRenderTarget()) {
    return 1;
  }
  if (!aerogpu::TestXyzrhwIndexedConversionUsesEffectiveViewportFromRenderTarget()) {
    return 1;
  }
  if (!aerogpu::TestFixedfuncFogVertexModeEmitsConstants()) {
    return 1;
  }
  if (!aerogpu::TestFixedfuncFogRhwTex1SelectsFogVsAndUsesWDivision()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyInteropIgnoresFogState()) {
    return 1;
  }
  if (!aerogpu::TestFixedfuncFogRhwColorSelectsFogVsAndUsesWDivision()) {
    return 1;
  }
  if (!aerogpu::TestFixedfuncFogTableModeTakesPrecedenceOverVertexMode()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseFogSelectsFogVs()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseTex1FogSelectsFogVs()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzTex1FogSelectsFogVs()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalFogSelectsFogVsAndLitFogVs()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalTex1FogSelectsFogVsAndLitFogVs()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseFogSelectsFogVsAndLitFogVs()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseTex1FogSelectsFogVsWhenLightingOff()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseTex1FogLightingSelectsLitFogVs()) {
    return 1;
  }
  if (!aerogpu::TestFixedfuncFogTogglesShaderVariant()) {
    return 1;
  }
  return 0;
}
