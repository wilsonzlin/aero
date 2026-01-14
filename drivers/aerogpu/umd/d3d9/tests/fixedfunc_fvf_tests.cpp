#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <unordered_set>
#include <vector>

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_d3d9_fixedfunc_shaders.h"
#include "aerogpu_d3d9_objects.h"

namespace aerogpu {

// Host-test helper (defined in `src/aerogpu_d3d9_driver.cpp` under "Host-side
// test entrypoints"). Unit tests may call this directly when the device vtable
// does not expose SetTextureStageState in a given build configuration.
HRESULT AEROGPU_D3D9_CALL device_set_texture_stage_state(
    D3DDDI_HDEVICE hDevice,
    uint32_t stage,
    uint32_t state,
    uint32_t value);

namespace {

// Portable D3D9 FVF bits (from d3d9types.h).
constexpr uint32_t kD3dFvfXyz = 0x00000002u;
constexpr uint32_t kD3dFvfXyzRhw = 0x00000004u;
constexpr uint32_t kD3dFvfNormal = 0x00000010u;
constexpr uint32_t kD3dFvfDiffuse = 0x00000040u;
constexpr uint32_t kD3dFvfTex1 = 0x00000100u;
// D3DFVF_TEXCOORDSIZE3(1): `TEXCOORD1` is float3. For TEX1 FVFs, set 1 is unused,
// but some runtimes may leave garbage bits in the unused D3DFVF_TEXCOORDSIZE range.
constexpr uint32_t kD3dFvfTexCoordSize3_1 = 0x00040000u;

constexpr uint32_t kFvfXyzrhwDiffuse = kD3dFvfXyzRhw | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzrhwDiffuseTex1 = kD3dFvfXyzRhw | kD3dFvfDiffuse | kD3dFvfTex1;
constexpr uint32_t kFvfXyzrhwTex1 = kD3dFvfXyzRhw | kD3dFvfTex1;
constexpr uint32_t kFvfXyzDiffuse = kD3dFvfXyz | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzDiffuseTex1 = kD3dFvfXyz | kD3dFvfDiffuse | kD3dFvfTex1;
constexpr uint32_t kFvfXyzTex1 = kD3dFvfXyz | kD3dFvfTex1;
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
// Intentionally unsupported by the fixed-function stage0 subset (used to validate
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
// Source register tokens used by the fixed-function ps_2_0 token builder
// (`fixedfunc_ps20` in `aerogpu_d3d9_driver.cpp`). These validate that stage0
// argument modifiers are encoded into the generated shader bytecode.
constexpr uint32_t kPsSrcTemp0Comp = 0x06E40000u;  // (1 - r0.xyzw)
constexpr uint32_t kPsSrcTemp0W = 0x00FF0000u;     // r0.wwww (alpha replicate)
constexpr uint32_t kPsSrcInput0Comp = 0x16E40000u; // (1 - v0.xyzw)
constexpr uint32_t kPsSrcInput0W = 0x10FF0000u;    // v0.wwww (alpha replicate)

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

struct VertexXyzrhwTex1 {
  float x;
  float y;
  float z;
  float rhw;
  float u;
  float v;
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

struct VertexXyzTex1 {
  float x;
  float y;
  float z;
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
    if (dev->fvf_vertex_decl) {
      expected_input_layout = dev->fvf_vertex_decl->handle;
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
    if (!Check(dev->fixedfunc_ps != nullptr, "fixedfunc_ps created")) {
      return false;
    }
    if (!Check(dev->ps == dev->fixedfunc_ps, "fixed-function PS is bound (no texture)")) {
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
  const size_t len = dev->cmd.bytes_used();
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
    if (dev->fvf_vertex_decl_xyz_diffuse) {
      expected_input_layout = dev->fvf_vertex_decl_xyz_diffuse->handle;
      const auto& blob = dev->fvf_vertex_decl_xyz_diffuse->blob;
      decl_ok = (blob.size() == sizeof(expected_decl)) &&
                (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
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
    if (!Check(dev->fixedfunc_vs_xyz_diffuse != nullptr, "fixedfunc_vs_xyz_diffuse created")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_diffuse, "XYZ|DIFFUSE binds WVP VS")) {
      return false;
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
    if (sc->start_register != 240 || sc->vec4_count != 4) {
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
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX || sc->start_register != 240 || sc->vec4_count != 4) {
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
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX || sc->start_register != 240 || sc->vec4_count != 4) {
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
  if (!Check(CountVsConstantUploads(buf, len, /*start_register=*/240, /*vec4_count=*/4) == 1,
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
  hr = cleanup.device_funcs.pfnSetShaderConstF(cleanup.hDevice, kD3dShaderStageVs, /*start_reg=*/240, junk_vec4, 1);
  if (!Check(hr == S_OK, "SetShaderConstF(VS, c240, 1)")) {
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
  if (!Check(CountVsConstantUploads(buf, len, /*start_register=*/240, /*vec4_count=*/4) == 1,
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
  if (!Check(CountVsConstantUploads(buf, len, /*start_register=*/240, /*vec4_count=*/4) == 1,
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
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX || sc->start_register != 240 || sc->vec4_count != 4) {
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

  // Set a non-identity transform so the fixed-function WVP constant upload is
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
    if (dev->fvf_vertex_decl_xyz_diffuse) {
      expected_input_layout = dev->fvf_vertex_decl_xyz_diffuse->handle;
      const auto& blob = dev->fvf_vertex_decl_xyz_diffuse->blob;
      decl_ok = (blob.size() == sizeof(expected_decl)) &&
                (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
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
  if (!Check(decl_ok, "XYZ|DIFFUSE internal vertex decl matches expected layout")) {
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
    if (!Check(dev->fixedfunc_vs_xyz_diffuse != nullptr, "fixedfunc_vs_xyz_diffuse created")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_diffuse, "XYZ|DIFFUSE binds WVP VS")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColor),
               "XYZ|DIFFUSE VS bytecode matches kVsWvpPosColor")) {
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
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX || sc->start_register != 240 || sc->vec4_count != 4) {
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
    if (dev->fvf_vertex_decl_tex1) {
      expected_input_layout = dev->fvf_vertex_decl_tex1->handle;
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
    if (dev->fvf_vertex_decl_xyz_diffuse_tex1) {
      expected_input_layout = dev->fvf_vertex_decl_xyz_diffuse_tex1->handle;
      const auto& blob = dev->fvf_vertex_decl_xyz_diffuse_tex1->blob;
      decl_ok = (blob.size() == sizeof(expected_decl)) &&
                (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
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
    if (!Check(dev->fixedfunc_vs_xyz_diffuse_tex1 != nullptr, "fixedfunc_vs_xyz_diffuse_tex1 created")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_diffuse_tex1, "XYZ|DIFFUSE|TEX1 binds WVP VS")) {
      return false;
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
    if (sc->start_register != 240 || sc->vec4_count != 4) {
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
      {0, 16, kD3dDeclTypeFloat2, kD3dDeclMethodDefault, kD3dDeclUsageTexcoord, 0},
      {0xFF, 0, kD3dDeclTypeUnused, 0, 0, 0}, // D3DDECL_END
  };

  // Set a non-identity transform so the fixed-function WVP constant upload is
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
    if (dev->fvf_vertex_decl_xyz_diffuse_tex1) {
      expected_input_layout = dev->fvf_vertex_decl_xyz_diffuse_tex1->handle;
      const auto& blob = dev->fvf_vertex_decl_xyz_diffuse_tex1->blob;
      decl_ok = (blob.size() == sizeof(expected_decl)) &&
                (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
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
    if (!Check(dev->fixedfunc_vs_xyz_diffuse_tex1 != nullptr, "fixedfunc_vs_xyz_diffuse_tex1 created")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_diffuse_tex1, "XYZ|DIFFUSE|TEX1 binds WVP VS")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsWvpPosColorTex0),
               "XYZ|DIFFUSE|TEX1 VS bytecode matches kVsWvpPosColorTex0")) {
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
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX || sc->start_register != 240 || sc->vec4_count != 4) {
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
    if (dev->fvf_vertex_decl_tex1_nodiffuse) {
      expected_input_layout = dev->fvf_vertex_decl_tex1_nodiffuse->handle;
      const auto& blob = dev->fvf_vertex_decl_tex1_nodiffuse->blob;
      decl_ok = (blob.size() == sizeof(expected_decl)) &&
                (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
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
    if (dev->fvf_vertex_decl_xyz_tex1) {
      expected_input_layout = dev->fvf_vertex_decl_xyz_tex1->handle;
      const auto& blob = dev->fvf_vertex_decl_xyz_tex1->blob;
      decl_ok = (blob.size() == sizeof(expected_decl)) &&
                (std::memcmp(blob.data(), expected_decl, sizeof(expected_decl)) == 0);
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
    if (sc->start_register != 240 || sc->vec4_count != 4) {
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
    if (sc->start_register != 240 || sc->vec4_count != 4) {
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
    if (sc->start_register != 240 || sc->vec4_count != 4) {
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
    if (!Check(dev->vertex_decl == decl_ptr, "vertex decl restored after XYZ|TEX1 draw")) {
      return false;
    }

    if (!Check(dev->fixedfunc_vs_xyz_tex1 != nullptr, "fixedfunc_vs_xyz_tex1 created")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_tex1, "XYZ|TEX1 via decl binds WVP VS")) {
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
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX || sc->start_register != 240 || sc->vec4_count != 4) {
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

bool TestVertexDeclXyzDiffuseDrawPrimitiveVbUploadsWvpAndRestoresDecl() {
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

    if (!Check(dev->fixedfunc_vs_xyz_diffuse != nullptr, "fixedfunc_vs_xyz_diffuse created")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_diffuse, "XYZ|DIFFUSE via decl binds WVP VS")) {
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

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != 240 || sc->vec4_count != 4) {
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
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (decl xyz|diffuse)")) {
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

  return true;
}

bool TestVertexDeclXyzDiffuseTex1DrawPrimitiveVbUploadsWvpAndRestoresDecl() {
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

    if (!Check(dev->fixedfunc_vs_xyz_diffuse_tex1 != nullptr, "fixedfunc_vs_xyz_diffuse_tex1 created")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_diffuse_tex1, "XYZ|DIFFUSE|TEX1 via decl binds WVP VS")) {
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

  bool saw_wvp_constants = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX) {
      continue;
    }
    if (sc->start_register != 240 || sc->vec4_count != 4) {
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
  if (!Check(saw_wvp_constants, "SET_SHADER_CONSTANTS_F uploads expected WVP columns (decl xyz|diffuse|tex1)")) {
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

    if (!Check(dev->fixedfunc_vs_tex1_nodiffuse != nullptr, "interop created fixedfunc_vs_tex1_nodiffuse")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_tex1_nodiffuse, "interop bound fixedfunc VS (XYZRHW|TEX1)")) {
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

    if (!Check(dev->fixedfunc_vs_xyz_tex1 != nullptr, "interop created fixedfunc_vs_xyz_tex1")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_tex1, "interop bound fixedfunc VS (XYZ|TEX1)")) {
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
    if (sc->stage == AEROGPU_SHADER_STAGE_VERTEX && sc->start_register == 240 && sc->vec4_count == 4) {
      saw_wvp = true;
      break;
    }
  }
  if (!Check(saw_wvp, "PS-only interop uploaded WVP constants")) {
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

    if (!Check(dev->fixedfunc_vs_tex1_nodiffuse != nullptr, "interop created fixedfunc_vs_tex1_nodiffuse")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_tex1_nodiffuse, "interop bound fixedfunc VS (XYZRHW|TEX1)")) {
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

    if (!Check(dev->fixedfunc_vs_xyz_tex1 != nullptr, "interop created fixedfunc_vs_xyz_tex1")) {
      return false;
    }
    if (!Check(dev->vs == dev->fixedfunc_vs_xyz_tex1, "interop bound fixedfunc VS (XYZ|TEX1)")) {
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
    if (sc->stage != AEROGPU_SHADER_STAGE_VERTEX || sc->start_register != 240 || sc->vec4_count != 4) {
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
    if (!Check(dev->fixedfunc_ps_tex1 != nullptr, "fixedfunc_ps_tex1 present")) {
      return false;
    }
    if (!Check(dev->ps == dev->fixedfunc_ps_tex1, "fixed-function PS is bound")) {
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
      // Default TFACTOR is white (0xFFFFFFFF). Verify the driver uploads c0 even
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

    // TFACTOR cases: ensure the PS constant upload was emitted once (c0) and
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
        if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 0 || sc->vec4_count != 1) {
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
      {"color_diffuse_complement", kD3dTaDiffuse | kD3dTaComplement, kPsSrcInput0Comp, /*expect_texld=*/false},
      {"color_diffuse_alpha_replicate", kD3dTaDiffuse | kD3dTaAlphaReplicate, kPsSrcInput0W, /*expect_texld=*/false},
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
  // references c0.
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
  // Setting the same value again should not re-upload c0.
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
    if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 0 || sc->vec4_count != 1) {
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

  // Configure the cached light/material state directly (portable builds do not expose
  // SetLight/SetMaterial DDIs in the device vtable).
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    std::memset(&dev->lights[0], 0, sizeof(dev->lights[0]));
    dev->lights[0].Type = D3DLIGHT_DIRECTIONAL;
    dev->lights[0].Direction = {0.0f, 0.0f, -1.0f};
    dev->lights[0].Diffuse = {1.0f, 0.0f, 0.0f, 1.0f};
    dev->lights[0].Ambient = {0.0f, 0.5f, 0.0f, 1.0f};
    dev->light_valid[0] = true;
    dev->light_enabled[0] = TRUE;

    dev->material_valid = true;
    dev->material.Diffuse = {0.5f, 0.5f, 0.5f, 1.0f};
    dev->material.Ambient = {0.25f, 0.25f, 0.25f, 1.0f};
    dev->material.Emissive = {0.0f, 0.0f, 0.0f, 0.0f};

    dev->fixedfunc_lighting_dirty = true;
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

  constexpr uint32_t kLightingStart = 244u;
  constexpr uint32_t kLightingVec4 = 10u;
  if (!Check(CountVsConstantUploads(buf, len, kLightingStart, kLightingVec4) == 1,
             "lighting constant upload emitted once")) {
    return false;
  }

  const float* payload = FindVsConstantsPayload(buf, len, kLightingStart, kLightingVec4);
  if (!Check(payload != nullptr, "lighting constant payload present")) {
    return false;
  }

  const float expected[40] = {
      // c244..c246: identity world*view 3x3 columns.
      1.0f, 0.0f, 0.0f, 0.0f,
      0.0f, 1.0f, 0.0f, 0.0f,
      0.0f, 0.0f, 1.0f, 0.0f,
      // c247: light direction in view space (negated).
      0.0f, 0.0f, 1.0f, 0.0f,
      // c248..c249: light diffuse/ambient.
      1.0f, 0.0f, 0.0f, 1.0f,
      0.0f, 0.5f, 0.0f, 1.0f,
      // c250..c252: material diffuse/ambient/emissive.
      0.5f, 0.5f, 0.5f, 1.0f,
      0.25f, 0.25f, 0.25f, 1.0f,
      0.0f, 0.0f, 0.0f, 0.0f,
      // c253: global ambient (ARGB blue -> RGBA {0,0,1,1}).
      0.0f, 0.0f, 1.0f, 1.0f,
  };
  for (size_t i = 0; i < 40; ++i) {
    // Compare numerically (treat -0.0 == 0.0) instead of bitwise comparing.
    if (payload[i] != expected[i]) {
      std::fprintf(stderr, "Lighting constants mismatch:\n");
      for (size_t j = 0; j < 40; ++j) {
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
  if (!Check(CountVsConstantUploads(buf, len, kLightingStart, kLightingVec4) == 0,
             "lighting constant upload is skipped when not dirty")) {
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
  if (!Check(CountVsConstantUploads(buf, len, kLightingStart, kLightingVec4) == 1,
             "lighting constant upload re-emitted after ambient change")) {
    return false;
  }
  payload = FindVsConstantsPayload(buf, len, kLightingStart, kLightingVec4);
  if (!Check(payload != nullptr, "lighting payload present (ambient changed)")) {
    return false;
  }
  if (!Check(payload[9 * 4 + 0] == 1.0f && payload[9 * 4 + 1] == 0.0f &&
             payload[9 * 4 + 2] == 0.0f && payload[9 * 4 + 3] == 1.0f,
             "global ambient constant reflects new D3DRS_AMBIENT value")) {
    return false;
  }

  // ---------------------------------------------------------------------------
  // Change light direction: re-upload should reflect the new direction (manual dirty).
  // ---------------------------------------------------------------------------
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    dev->lights[0].Direction = {0.0f, 0.0f, 1.0f};
    dev->fixedfunc_lighting_dirty = true;
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
  if (!Check(CountVsConstantUploads(buf, len, kLightingStart, kLightingVec4) == 1,
             "lighting constant upload re-emitted after light direction change")) {
    return false;
  }
  payload = FindVsConstantsPayload(buf, len, kLightingStart, kLightingVec4);
  if (!Check(payload != nullptr, "lighting payload present (light direction changed)")) {
    return false;
  }
  if (!Check(payload[3 * 4 + 0] == 0.0f && payload[3 * 4 + 1] == 0.0f &&
             payload[3 * 4 + 2] == -1.0f && payload[3 * 4 + 3] == 0.0f,
             "light direction constant reflects updated light direction")) {
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
  if (!aerogpu::TestFvfXyzDiffuseRedundantSetFvfDoesNotReuploadWvp()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzDiffuseWvpDirtyAfterUserVsAndConstClobber()) {
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
  if (!aerogpu::TestFvfXyzNormalDiffuseLightingSelectsLitVs()) {
    return 1;
  }
  if (!aerogpu::TestFvfXyzNormalDiffuseEmitsLightingConstantsAndTracksDirty()) {
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
  if (!aerogpu::TestVertexDeclXyzDiffuseDrawPrimitiveVbUploadsWvpAndRestoresDecl()) {
    return 1;
  }
  if (!aerogpu::TestVertexDeclXyzDiffuseTex1DrawPrimitiveVbUploadsWvpAndRestoresDecl()) {
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
  if (!aerogpu::TestTextureFactorRenderStateUpdatesPsConstantWhenUsed()) {
    return 1;
  }
  return 0;
}
