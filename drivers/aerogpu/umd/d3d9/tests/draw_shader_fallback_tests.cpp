#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_d3d9_objects.h"

#include "aerogpu_cmd.h"

#include "../include/aerogpu_d3d9_umd.h"

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

  // Set FVF: XYZRHW|DIFFUSE (the only fixed-function VS fallback path supported by the UMD today).
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

  // XYZ|DIFFUSE is supported by the fixed-function fallback and requires the internal
  // WVP VS variant when the app leaves VS NULL.
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

} // namespace

int main() {
  bool ok = true;
  ok = ok && TestPsOnlyDrawBindsFallbackVs();
  ok = ok && TestPsOnlyDrawBindsFallbackVsXyzDiffuse();
  ok = ok && TestVsOnlyDrawBindsFallbackPs();
  ok = ok && TestPsOnlyUnsupportedFvfFailsWithoutDraw();
  if (ok) {
    std::fprintf(stdout, "PASS\n");
    return 0;
  }
  return 1;
}
