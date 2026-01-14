#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <vector>

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_d3d9_objects.h"

#include "aerogpu_cmd.h"

#include "../include/aerogpu_d3d9_umd.h"

// aerogpu_d3d9_driver.cpp helpers (not part of the public UMD header).
namespace aerogpu {
HRESULT AEROGPU_D3D9_CALL device_set_texture_stage_state(D3DDDI_HDEVICE hDevice,
                                                         uint32_t stage,
                                                         uint32_t state,
                                                         uint32_t value);
} // namespace aerogpu

namespace {

// D3DERR_INVALIDCALL from d3d9.h.
constexpr HRESULT kD3DErrInvalidCall = 0x8876086CUL;
constexpr uint32_t kD3d9ShaderStageVs = 0u;
constexpr uint32_t kD3d9ShaderStagePs = 1u;

// D3DFVF subset (numeric values from d3d9types.h).
constexpr uint32_t kD3dFvfXyz = 0x00000002u;
constexpr uint32_t kD3dFvfXyzRhw = 0x00000004u;
constexpr uint32_t kD3dFvfDiffuse = 0x00000040u;
constexpr uint32_t kFvfXyzrhwDiffuse = kD3dFvfXyzRhw | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzDiffuse = kD3dFvfXyz | kD3dFvfDiffuse;

struct VertexXyzrhwDiffuse {
  float x;
  float y;
  float z;
  float rhw;
  uint32_t color;
};

struct VertexXyzDiffuse {
  float x;
  float y;
  float z;
  uint32_t color;
};

// Minimal vs_2_0:
//   mov oPos, v0
//   mov oD0, v1
//   end
static const uint32_t kVsPassthroughPosColor[] = {
    0xFFFE0200u,
    0x03000001u, 0x400F0000u, 0x10E40000u,
     0x03000001u, 0x500F0000u, 0x10E40001u,
     0x0000FFFFu,
};

// Minimal ps_2_0:
//   mov oC0, v0
//   end
static const uint32_t kPsPassthroughColor[] = {
    0xFFFF0200u,
     0x03000001u, 0x000F0800u, 0x10E40000u,
     0x0000FFFFu,
};

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg ? msg : "(null)");
    return false;
  }
  return true;
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

size_t CountFogConstantUploads(const uint8_t* buf, size_t capacity) {
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (stream_len == 0) {
    return 0;
  }

  constexpr uint32_t kPsStage = AEROGPU_SHADER_STAGE_PIXEL;
  constexpr uint32_t kFogColorRegister = 1u;
  constexpr uint32_t kFogVec4Count = 2u; // c1..c2

  size_t count = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_SET_SHADER_CONSTANTS_F &&
        hdr->size_bytes >= sizeof(aerogpu_cmd_set_shader_constants_f)) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(buf + offset);
      if (cmd->stage == kPsStage && cmd->start_register == kFogColorRegister && cmd->vec4_count == kFogVec4Count) {
        ++count;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return count;
}

bool ValidateNoBindAfterDestroy(const uint8_t* buf, size_t capacity) {
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (!Check(stream_len != 0, "stream must be non-empty and finalized")) {
    return false;
  }

  std::vector<aerogpu_handle_t> destroyed_handles;

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_DESTROY_SHADER && hdr->size_bytes >= sizeof(aerogpu_cmd_destroy_shader)) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_destroy_shader*>(hdr);
      if (cmd->shader_handle != 0) {
        destroyed_handles.push_back(cmd->shader_handle);
      }
    } else if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      for (aerogpu_handle_t h : destroyed_handles) {
        if (!Check(cmd->vs != h, "BIND_SHADERS observed with VS referencing destroyed handle")) {
          return false;
        }
        if (!Check(cmd->ps != h, "BIND_SHADERS observed with PS referencing destroyed handle")) {
          return false;
        }
      }
    }

    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }

  return true;
}

bool ValidateNoDrawWithNullShaders(const uint8_t* buf, size_t capacity) {
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (!Check(stream_len != 0, "stream must be non-empty and finalized")) {
    return false;
  }
  aerogpu_handle_t cur_vs = 0;
  aerogpu_handle_t cur_ps = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      cur_vs = cmd->vs;
      cur_ps = cmd->ps;
    } else if (hdr->opcode == AEROGPU_CMD_DRAW || hdr->opcode == AEROGPU_CMD_DRAW_INDEXED) {
      if (!Check(cur_vs != 0, "draw observed with VS==0")) {
        return false;
      }
      if (!Check(cur_ps != 0, "draw observed with PS==0")) {
        return false;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return true;
}

uint32_t F32Bits(float f) {
  uint32_t u = 0;
  static_assert(sizeof(u) == sizeof(f), "F32Bits assumes 32-bit float");
  std::memcpy(&u, &f, sizeof(u));
  return u;
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

struct D3d9Context {
  D3D9DDI_ADAPTERFUNCS adapter_funcs{};
  D3D9DDI_DEVICEFUNCS device_funcs{};
  D3DDDI_HADAPTER hAdapter{};
  D3DDDI_HDEVICE hDevice{};
  bool has_adapter = false;
  bool has_device = false;

  // Callback tables must outlive the adapter/device; the UMD stores raw pointers.
  D3DDDI_ADAPTERCALLBACKS callbacks{};
  D3DDDI_ADAPTERCALLBACKS2 callbacks2{};

  ~D3d9Context() {
    if (has_device && device_funcs.pfnDestroyDevice) {
      device_funcs.pfnDestroyDevice(hDevice);
    }
    if (has_adapter && adapter_funcs.pfnCloseAdapter) {
      adapter_funcs.pfnCloseAdapter(hAdapter);
    }
  }
};

bool InitD3d9(D3d9Context* ctx) {
  if (!ctx) {
    return false;
  }

  D3DDDIARG_OPENADAPTER2 open{};
  open.Interface = 1;
  open.Version = 1;
  open.pAdapterCallbacks = &ctx->callbacks;
  open.pAdapterCallbacks2 = &ctx->callbacks2;
  open.pAdapterFuncs = &ctx->adapter_funcs;

  HRESULT hr = ::OpenAdapter2(&open);
  if (!Check(hr == S_OK, "OpenAdapter2")) {
    return false;
  }
  if (!Check(open.hAdapter.pDrvPrivate != nullptr, "OpenAdapter2 returned adapter handle")) {
    return false;
  }
  ctx->hAdapter = open.hAdapter;
  ctx->has_adapter = true;

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  if (!Check(ctx->adapter_funcs.pfnCreateDevice != nullptr, "adapter pfnCreateDevice")) {
    return false;
  }
  hr = ctx->adapter_funcs.pfnCreateDevice(&create_dev, &ctx->device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  if (!Check(create_dev.hDevice.pDrvPrivate != nullptr, "CreateDevice returned device handle")) {
    return false;
  }
  ctx->hDevice = create_dev.hDevice;
  ctx->has_device = true;
  return true;
}

aerogpu::Device* GetDevice(const D3d9Context& ctx) {
  return reinterpret_cast<aerogpu::Device*>(ctx.hDevice.pDrvPrivate);
}

bool TestPsOnlyDrawBindsFallbackVs() {
  D3d9Context ctx;
  if (!InitD3d9(&ctx)) {
    return false;
  }
  aerogpu::Device* dev = GetDevice(ctx);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Set FVF: XYZRHW|DIFFUSE (minimal fixed-function VS fallback path used by this test).
  if (!Check(ctx.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF")) {
    return false;
  }
  HRESULT hr = ctx.device_funcs.pfnSetFVF(ctx.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(SUCCEEDED(hr), "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }
  // Create a simple pixel shader that outputs interpolated vertex color.
  if (!Check(ctx.device_funcs.pfnCreateShader != nullptr, "pfnCreateShader")) {
    return false;
  }
  D3D9DDI_HSHADER hPs{};
  hr = ctx.device_funcs.pfnCreateShader(ctx.hDevice,
                                        kD3d9ShaderStagePs,
                                        kPsPassthroughColor,
                                        static_cast<uint32_t>(sizeof(kPsPassthroughColor)),
                                        &hPs);
  if (!Check(SUCCEEDED(hr) && hPs.pDrvPrivate != nullptr, "CreateShader(PS)")) {
    return false;
  }
  // Bind only PS; leave VS unset.
  if (!Check(ctx.device_funcs.pfnSetShader != nullptr, "pfnSetShader")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetShader(ctx.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(SUCCEEDED(hr), "SetShader(PS)")) {
    return false;
  }
  const VertexXyzrhwDiffuse verts[3] = {
      {10.0f, 10.0f, 0.5f, 1.0f, 0xFF0000FFu},
      {20.0f, 10.0f, 0.5f, 1.0f, 0xFF0000FFu},
      {15.0f, 20.0f, 0.5f, 1.0f, 0xFF0000FFu},
  };
  if (!Check(ctx.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP")) {
    return false;
  }
  hr = ctx.device_funcs.pfnDrawPrimitiveUP(ctx.hDevice,
                                           D3DDDIPT_TRIANGLELIST,
                                           /*primitive_count=*/1,
                                           verts,
                                           static_cast<uint32_t>(sizeof(VertexXyzrhwDiffuse)));
  if (!Check(SUCCEEDED(hr), "device_draw_primitive_up (ps only)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t cap = dev->cmd.bytes_used();
  if (!Check(CountOpcode(buf, cap, AEROGPU_CMD_DRAW) == 1, "expected exactly one DRAW packet")) {
    return false;
  }
  return ValidateNoDrawWithNullShaders(buf, cap);
}

bool TestPsOnlyDrawBindsFallbackVsXyzDiffuse() {
  D3d9Context ctx;
  if (!InitD3d9(&ctx)) {
    return false;
  }
  aerogpu::Device* dev = GetDevice(ctx);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // XYZ|DIFFUSE is supported by the fixed-function fallback. For PS-only interop
  // (VS is NULL), the driver binds the internal fixed-function WVP VS variant
  // and uploads WVP into the reserved high VS constant range (`c240..c243`).
  if (!Check(ctx.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF")) {
    return false;
  }
  HRESULT hr = ctx.device_funcs.pfnSetFVF(ctx.hDevice, kFvfXyzDiffuse);
  if (!Check(SUCCEEDED(hr), "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  if (!Check(ctx.device_funcs.pfnCreateShader != nullptr, "pfnCreateShader")) {
    return false;
  }
  D3D9DDI_HSHADER hPs{};
  hr = ctx.device_funcs.pfnCreateShader(ctx.hDevice,
                                        kD3d9ShaderStagePs,
                                        kPsPassthroughColor,
                                        static_cast<uint32_t>(sizeof(kPsPassthroughColor)),
                                        &hPs);
  if (!Check(SUCCEEDED(hr) && hPs.pDrvPrivate != nullptr, "CreateShader(PS)")) {
    return false;
  }

  if (!Check(ctx.device_funcs.pfnSetShader != nullptr, "pfnSetShader")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetShader(ctx.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(SUCCEEDED(hr), "SetShader(PS)")) {
    return false;
  }

  const VertexXyzDiffuse verts[3] = {
      {-0.5f, -0.5f, 0.0f, 0xFFFFFFFFu},
      {0.5f, -0.5f, 0.0f, 0xFFFFFFFFu},
      {0.0f, 0.5f, 0.0f, 0xFFFFFFFFu},
  };

  if (!Check(ctx.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP")) {
    return false;
  }
  hr = ctx.device_funcs.pfnDrawPrimitiveUP(ctx.hDevice,
                                           D3DDDIPT_TRIANGLELIST,
                                           /*primitive_count=*/1,
                                           verts,
                                           static_cast<uint32_t>(sizeof(VertexXyzDiffuse)));
  if (!Check(SUCCEEDED(hr), "device_draw_primitive_up (ps only, xyz|diffuse)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t cap = dev->cmd.bytes_used();
  if (!Check(CountOpcode(buf, cap, AEROGPU_CMD_DRAW) == 1, "expected exactly one DRAW packet")) {
    return false;
  }
  return ValidateNoDrawWithNullShaders(buf, cap);
}
bool TestVsOnlyDrawBindsFallbackPs() {
  D3d9Context ctx;
  if (!InitD3d9(&ctx)) {
    return false;
  }
  aerogpu::Device* dev = GetDevice(ctx);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Create a minimal vertex shader; pixel shader remains unset (fixed-function PS fallback).
  if (!Check(ctx.device_funcs.pfnCreateShader != nullptr, "pfnCreateShader")) {
    return false;
  }
  D3D9DDI_HSHADER hVs{};
  HRESULT hr = ctx.device_funcs.pfnCreateShader(ctx.hDevice,
                                                kD3d9ShaderStageVs,
                                                kVsPassthroughPosColor,
                                                static_cast<uint32_t>(sizeof(kVsPassthroughPosColor)),
                                                &hVs);
  if (!Check(SUCCEEDED(hr) && hVs.pDrvPrivate != nullptr, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(ctx.device_funcs.pfnSetShader != nullptr, "pfnSetShader")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetShader(ctx.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(SUCCEEDED(hr), "SetShader(VS)")) {
    return false;
  }
  const VertexXyzrhwDiffuse verts[3] = {
      {0.0f, 0.0f, 0.5f, 1.0f, 0xFF00FF00u},
      {1.0f, 0.0f, 0.5f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.5f, 1.0f, 0xFF00FF00u},
  };
  if (!Check(ctx.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP")) {
    return false;
  }
  hr = ctx.device_funcs.pfnDrawPrimitiveUP(ctx.hDevice,
                                           D3DDDIPT_TRIANGLELIST,
                                           /*primitive_count=*/1,
                                           verts,
                                           static_cast<uint32_t>(sizeof(VertexXyzrhwDiffuse)));
  if (!Check(SUCCEEDED(hr), "device_draw_primitive_up (vs only)")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t cap = dev->cmd.bytes_used();
  if (!Check(CountOpcode(buf, cap, AEROGPU_CMD_DRAW) == 1, "expected exactly one DRAW packet")) {
    return false;
  }
  return ValidateNoDrawWithNullShaders(buf, cap);
}

bool TestVsOnlyStage0PsUpdateDoesNotRebindDestroyedShader() {
  D3d9Context ctx;
  if (!InitD3d9(&ctx)) {
    return false;
  }
  aerogpu::Device* dev = GetDevice(ctx);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Use a supported FVF to bind a known input layout; this test is focused on
  // interop PS replacement + command stream ordering, not on vertex format.
  if (!Check(ctx.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF")) {
    return false;
  }
  HRESULT hr = ctx.device_funcs.pfnSetFVF(ctx.hDevice, kFvfXyzDiffuse);
  if (!Check(SUCCEEDED(hr), "SetFVF(XYZ|DIFFUSE)")) {
    return false;
  }

  // Bind a user VS and explicitly clear PS (VS-only interop => fixed-function PS fallback).
  if (!Check(ctx.device_funcs.pfnCreateShader != nullptr, "pfnCreateShader")) {
    return false;
  }
  D3D9DDI_HSHADER hVs{};
  hr = ctx.device_funcs.pfnCreateShader(ctx.hDevice,
                                        kD3d9ShaderStageVs,
                                        kVsPassthroughPosColor,
                                        static_cast<uint32_t>(sizeof(kVsPassthroughPosColor)),
                                        &hVs);
  if (!Check(SUCCEEDED(hr) && hVs.pDrvPrivate != nullptr, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(ctx.device_funcs.pfnSetShader != nullptr, "pfnSetShader")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetShader(ctx.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(SUCCEEDED(hr), "SetShader(VS)")) {
    return false;
  }
  D3D9DDI_HSHADER null_ps{};
  hr = ctx.device_funcs.pfnSetShader(ctx.hDevice, kD3d9ShaderStagePs, null_ps);
  if (!Check(SUCCEEDED(hr), "SetShader(PS=NULL)")) {
    return false;
  }

  // Create and bind a dummy texture0 so stage0 PS selection can choose a texture
  // variant (forcing a fixed-function PS replacement).
  if (!Check(ctx.device_funcs.pfnCreateResource != nullptr, "pfnCreateResource")) {
    return false;
  }
  D3D9DDIARG_CREATERESOURCE create_tex{};
  create_tex.type = 3u; // D3DRTYPE_TEXTURE
  create_tex.format = 22u; // D3DFMT_X8R8G8B8
  create_tex.width = 1;
  create_tex.height = 1;
  create_tex.depth = 1;
  create_tex.mip_levels = 1;
  create_tex.usage = 0;
  create_tex.pool = 0;
  create_tex.size = 0;
  create_tex.hResource.pDrvPrivate = nullptr;
  create_tex.pSharedHandle = nullptr;
  create_tex.pPrivateDriverData = nullptr;
  create_tex.PrivateDriverDataSize = 0;
  create_tex.wddm_hAllocation = 0;
  hr = ctx.device_funcs.pfnCreateResource(ctx.hDevice, &create_tex);
  if (!Check(SUCCEEDED(hr) && create_tex.hResource.pDrvPrivate != nullptr, "CreateResource(texture)")) {
    return false;
  }
  if (!Check(ctx.device_funcs.pfnSetTexture != nullptr, "pfnSetTexture")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetTexture(ctx.hDevice, /*stage=*/0, create_tex.hResource);
  if (!Check(SUCCEEDED(hr), "SetTexture(stage0)")) {
    return false;
  }

  // Force stage0 to sample the bound texture.
  constexpr uint32_t kD3dTssColorOp = 1u;    // D3DTSS_COLOROP
  constexpr uint32_t kD3dTssColorArg1 = 2u;  // D3DTSS_COLORARG1
  constexpr uint32_t kD3dTssAlphaOp = 4u;    // D3DTSS_ALPHAOP
  constexpr uint32_t kD3dTssAlphaArg1 = 5u;  // D3DTSS_ALPHAARG1
  constexpr uint32_t kD3dTopSelectArg1 = 2u; // D3DTOP_SELECTARG1
  constexpr uint32_t kD3dTaTexture = 2u;     // D3DTA_TEXTURE

  const auto SetTss = [&](uint32_t stage, uint32_t state, uint32_t value, const char* msg) -> bool {
    HRESULT hr2 = S_OK;
    if (ctx.device_funcs.pfnSetTextureStageState) {
      hr2 = ctx.device_funcs.pfnSetTextureStageState(ctx.hDevice, stage, state, value);
    } else {
      hr2 = aerogpu::device_set_texture_stage_state(ctx.hDevice, stage, state, value);
    }
    return Check(SUCCEEDED(hr2), msg);
  };

  if (!SetTss(/*stage=*/0, kD3dTssColorOp, kD3dTopSelectArg1, "SetTextureStageState(COLOROP=SELECTARG1)")) {
    return false;
  }
  if (!SetTss(/*stage=*/0, kD3dTssColorArg1, kD3dTaTexture, "SetTextureStageState(COLORARG1=TEXTURE)")) {
    return false;
  }
  if (!SetTss(/*stage=*/0, kD3dTssAlphaOp, kD3dTopSelectArg1, "SetTextureStageState(ALPHAOP=SELECTARG1)")) {
    return false;
  }
  if (!SetTss(/*stage=*/0, kD3dTssAlphaArg1, kD3dTaTexture, "SetTextureStageState(ALPHAARG1=TEXTURE)")) {
    return false;
  }

  // Draw: select the internal fixed-function PS for stage0 (based on texture stage state)
  // and ensure we never emit null shader binds.
  const VertexXyzDiffuse verts[3] = {
      {-0.5f, -0.5f, 0.0f, 0xFFFFFFFFu},
      {0.5f, -0.5f, 0.0f, 0xFFFFFFFFu},
      {0.0f, 0.5f, 0.0f, 0xFFFFFFFFu},
  };
  if (!Check(ctx.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP")) {
    return false;
  }
  hr = ctx.device_funcs.pfnDrawPrimitiveUP(ctx.hDevice,
                                           D3DDDIPT_TRIANGLELIST,
                                           /*primitive_count=*/1,
                                           verts,
                                           static_cast<uint32_t>(sizeof(VertexXyzDiffuse)));
  if (!Check(SUCCEEDED(hr), "DrawPrimitiveUP(VS-only, stage0 texture)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t cap = dev->cmd.bytes_used();
  if (!ValidateNoDrawWithNullShaders(buf, cap)) {
    return false;
  }
  return ValidateNoBindAfterDestroy(buf, cap);
}

bool TestDestroyShaderDoesNotBindAfterDestroy() {
  D3d9Context ctx;
  if (!InitD3d9(&ctx)) {
    return false;
  }
  aerogpu::Device* dev = GetDevice(ctx);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  if (!Check(ctx.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF")) {
    return false;
  }
  HRESULT hr = ctx.device_funcs.pfnSetFVF(ctx.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(SUCCEEDED(hr), "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  if (!Check(ctx.device_funcs.pfnCreateShader != nullptr, "pfnCreateShader")) {
    return false;
  }
  D3D9DDI_HSHADER hVs{};
  hr = ctx.device_funcs.pfnCreateShader(ctx.hDevice,
                                        kD3d9ShaderStageVs,
                                        kVsPassthroughPosColor,
                                        static_cast<uint32_t>(sizeof(kVsPassthroughPosColor)),
                                        &hVs);
  if (!Check(SUCCEEDED(hr) && hVs.pDrvPrivate != nullptr, "CreateShader(VS)")) {
    return false;
  }
  D3D9DDI_HSHADER hPs1{};
  hr = ctx.device_funcs.pfnCreateShader(ctx.hDevice,
                                        kD3d9ShaderStagePs,
                                        kPsPassthroughColor,
                                        static_cast<uint32_t>(sizeof(kPsPassthroughColor)),
                                        &hPs1);
  if (!Check(SUCCEEDED(hr) && hPs1.pDrvPrivate != nullptr, "CreateShader(PS)")) {
    return false;
  }

  if (!Check(ctx.device_funcs.pfnSetShader != nullptr, "pfnSetShader")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetShader(ctx.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(SUCCEEDED(hr), "SetShader(VS)")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetShader(ctx.hDevice, kD3d9ShaderStagePs, hPs1);
  if (!Check(SUCCEEDED(hr), "SetShader(PS)")) {
    return false;
  }

  if (!Check(ctx.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP")) {
    return false;
  }
  const VertexXyzrhwDiffuse verts[3] = {
      {0.0f, 0.0f, 0.5f, 1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.5f, 1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.5f, 1.0f, 0xFFFFFFFFu},
  };
  hr = ctx.device_funcs.pfnDrawPrimitiveUP(ctx.hDevice,
                                           D3DDDIPT_TRIANGLELIST,
                                           /*primitive_count=*/1,
                                           verts,
                                           static_cast<uint32_t>(sizeof(VertexXyzrhwDiffuse)));
  if (!Check(SUCCEEDED(hr), "DrawPrimitiveUP(before DestroyShader)")) {
    return false;
  }

  if (!Check(ctx.device_funcs.pfnDestroyShader != nullptr, "pfnDestroyShader")) {
    return false;
  }
  hr = ctx.device_funcs.pfnDestroyShader(ctx.hDevice, hPs1);
  if (!Check(SUCCEEDED(hr), "DestroyShader(PS)")) {
    return false;
  }

  // Re-bind a new PS after destroying the previous one. This forces a BIND_SHADERS
  // packet after the DESTROY_SHADER, which our validator checks for stale handles.
  D3D9DDI_HSHADER hPs2{};
  hr = ctx.device_funcs.pfnCreateShader(ctx.hDevice,
                                        kD3d9ShaderStagePs,
                                        kPsPassthroughColor,
                                        static_cast<uint32_t>(sizeof(kPsPassthroughColor)),
                                        &hPs2);
  if (!Check(SUCCEEDED(hr) && hPs2.pDrvPrivate != nullptr, "CreateShader(PS2)")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetShader(ctx.hDevice, kD3d9ShaderStagePs, hPs2);
  if (!Check(SUCCEEDED(hr), "SetShader(PS2)")) {
    return false;
  }
  hr = ctx.device_funcs.pfnDrawPrimitiveUP(ctx.hDevice,
                                           D3DDDIPT_TRIANGLELIST,
                                           /*primitive_count=*/1,
                                           verts,
                                           static_cast<uint32_t>(sizeof(VertexXyzrhwDiffuse)));
  if (!Check(SUCCEEDED(hr), "DrawPrimitiveUP(after DestroyShader)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t cap = dev->cmd.bytes_used();
  if (!Check(CountOpcode(buf, cap, AEROGPU_CMD_DESTROY_SHADER) >= 1, "expected DESTROY_SHADER packet")) {
    return false;
  }
  if (!ValidateNoDrawWithNullShaders(buf, cap)) {
    return false;
  }
  return ValidateNoBindAfterDestroy(buf, cap);
}
bool TestPsOnlyUnsupportedFvfFailsWithoutDraw() {
  D3d9Context ctx;
  if (!InitD3d9(&ctx)) {
    return false;
  }
  aerogpu::Device* dev = GetDevice(ctx);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Pick an unsupported FVF (XYZ only; no XYZRHW).
  if (!Check(ctx.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF")) {
    return false;
  }
  HRESULT hr = ctx.device_funcs.pfnSetFVF(ctx.hDevice, kD3dFvfXyz);
  if (!Check(SUCCEEDED(hr), "SetFVF(XYZ)")) {
    return false;
  }
  if (!Check(ctx.device_funcs.pfnCreateShader != nullptr, "pfnCreateShader")) {
    return false;
  }
  D3D9DDI_HSHADER hPs{};
  hr = ctx.device_funcs.pfnCreateShader(ctx.hDevice,
                                        kD3d9ShaderStagePs,
                                        kPsPassthroughColor,
                                        static_cast<uint32_t>(sizeof(kPsPassthroughColor)),
                                        &hPs);
  if (!Check(SUCCEEDED(hr) && hPs.pDrvPrivate != nullptr, "CreateShader(PS)")) {
    return false;
  }
  if (!Check(ctx.device_funcs.pfnSetShader != nullptr, "pfnSetShader")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetShader(ctx.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(SUCCEEDED(hr), "SetShader(PS)")) {
    return false;
  }
  const VertexXyzrhwDiffuse verts[3] = {
      {0.0f, 0.0f, 0.5f, 1.0f, 0xFFFFFFFFu},
      {1.0f, 0.0f, 0.5f, 1.0f, 0xFFFFFFFFu},
      {0.0f, 1.0f, 0.5f, 1.0f, 0xFFFFFFFFu},
  };
  if (!Check(ctx.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP")) {
    return false;
  }
  hr = ctx.device_funcs.pfnDrawPrimitiveUP(ctx.hDevice,
                                           D3DDDIPT_TRIANGLELIST,
                                           /*primitive_count=*/1,
                                           verts,
                                           static_cast<uint32_t>(sizeof(VertexXyzrhwDiffuse)));
  if (!Check(hr == kD3DErrInvalidCall, "expected D3DERR_INVALIDCALL for unsupported fixed-function VS fallback")) {
    return false;
  }
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t cap = dev->cmd.bytes_used();
  return Check(CountOpcode(buf, cap, AEROGPU_CMD_DRAW) == 0, "expected no DRAW packets on INVALIDCALL");
}

bool TestFixedfuncFogRhwColorSelectsFogPs() {
  D3d9Context ctx;
  if (!InitD3d9(&ctx)) {
    return false;
  }
  aerogpu::Device* dev = GetDevice(ctx);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }
  if (!Check(ctx.device_funcs.pfnSetFVF != nullptr, "pfnSetFVF")) {
    return false;
  }
  if (!Check(ctx.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState")) {
    return false;
  }
  if (!Check(ctx.device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP")) {
    return false;
  }

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;     // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;      // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;  // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;      // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;        // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;        // D3DFOG_LINEAR

  // c1 (fog color) as encoded by D3D9 shader bytecode.
  constexpr uint32_t kPsSrcConst1 = 0x20E40001u;

  // Pick an FVF without TEX1: RHW_COLOR. This variant does not have a dedicated fog VS
  // variant, but the base passthrough VS still writes TEXCOORD0 from position, so the
  // fog PS can safely read TEXCOORD0.z.
  HRESULT hr = ctx.device_funcs.pfnSetFVF(ctx.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  const VertexXyzrhwDiffuse verts[3] = {
      {0.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u},
      {1.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.25f, 1.0f, 0xFF00FF00u},
  };

  // Baseline draw with fog disabled; record the selected fixed-function PS.
  hr = ctx.device_funcs.pfnSetRenderState(ctx.hDevice, kD3dRsFogEnable, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=0)")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetRenderState(ctx.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0)")) {
    return false;
  }
  hr = ctx.device_funcs.pfnDrawPrimitiveUP(ctx.hDevice,
                                           D3DDDIPT_TRIANGLELIST,
                                           /*primitive_count=*/1,
                                           verts,
                                           static_cast<uint32_t>(sizeof(VertexXyzrhwDiffuse)));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(fog off)")) {
    return false;
  }

  aerogpu::Shader* ps_off = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_off = dev->ps;
  }
  if (!Check(ps_off != nullptr, "PS bound (fog off)")) {
    return false;
  }
  if (!Check(!ShaderContainsToken(ps_off, kPsSrcConst1), "fog-off PS does not reference c1 (fog color)")) {
    return false;
  }

  // Enable linear fog and draw again; fixed-function fallback should select a new PS variant.
  hr = ctx.device_funcs.pfnSetRenderState(ctx.hDevice, kD3dRsFogEnable, 1u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=1)")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetRenderState(ctx.hDevice, kD3dRsFogTableMode, kD3dFogLinear);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=LINEAR)")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetRenderState(ctx.hDevice, kD3dRsFogColor, 0xFFFF0000u);
  if (!Check(hr == S_OK, "SetRenderState(FOGCOLOR)")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetRenderState(ctx.hDevice, kD3dRsFogStart, F32Bits(0.2f));
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = ctx.device_funcs.pfnSetRenderState(ctx.hDevice, kD3dRsFogEnd, F32Bits(0.8f));
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  // Capture only the fog-enabled draw and its associated constant uploads.
  dev->cmd.reset();

  hr = ctx.device_funcs.pfnDrawPrimitiveUP(ctx.hDevice,
                                           D3DDDIPT_TRIANGLELIST,
                                           /*primitive_count=*/1,
                                           verts,
                                           static_cast<uint32_t>(sizeof(VertexXyzrhwDiffuse)));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(fog on)")) {
    return false;
  }

  aerogpu::Shader* ps_on = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_on = dev->ps;
  }
  if (!Check(ps_on != nullptr, "PS bound (fog on)")) {
    return false;
  }
  if (!Check(ps_on != ps_off, "fog toggle changes fixed-function PS variant (RHW_COLOR)")) {
    return false;
  }
  if (!Check(ShaderContainsToken(ps_on, kPsSrcConst1), "fog-on PS references c1 (fog color)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(CountFogConstantUploads(buf, len) >= 1, "fog enabled: emits fog PS constants (c1..c2)")) {
    return false;
  }
  return true;
}

} // namespace

int main() {
  bool ok = true;
  ok = ok && TestPsOnlyDrawBindsFallbackVs();
  ok = ok && TestPsOnlyDrawBindsFallbackVsXyzDiffuse();
  ok = ok && TestVsOnlyDrawBindsFallbackPs();
  ok = ok && TestVsOnlyStage0PsUpdateDoesNotRebindDestroyedShader();
  ok = ok && TestDestroyShaderDoesNotBindAfterDestroy();
  ok = ok && TestPsOnlyUnsupportedFvfFailsWithoutDraw();
  ok = ok && TestFixedfuncFogRhwColorSelectsFogPs();
  if (ok) {
    std::fprintf(stdout, "PASS\n");
    return 0;
  }
  return 1;
}
