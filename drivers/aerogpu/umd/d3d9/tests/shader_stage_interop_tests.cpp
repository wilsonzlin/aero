#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <vector>

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_d3d9_fixedfunc_shaders.h"
#include "aerogpu_d3d9_objects.h"
#include "fixedfunc_test_constants.h"

namespace aerogpu {

constexpr uint32_t kD3d9ShaderStageVs = 0u;
constexpr uint32_t kD3d9ShaderStagePs = 1u;

// Host-test helper (defined in `src/aerogpu_d3d9_driver.cpp` under "Host-side test
// entrypoints") used to simulate a user-visible shader state without requiring
// a DDI call sequence.
HRESULT AEROGPU_D3D9_CALL device_test_set_unmaterialized_user_shaders(
    D3DDDI_HDEVICE hDevice,
    D3D9DDI_HSHADER user_vs,
    D3D9DDI_HSHADER user_ps);

// Host-test helper for SetTextureStageState. Portable host-side test builds may
// compile a minimal D3D9DDI_DEVICEFUNCS table without `pfnSetTextureStageState`,
// so tests should call this directly instead of relying on the vtable member.
HRESULT AEROGPU_D3D9_CALL device_set_texture_stage_state(
    D3DDDI_HDEVICE hDevice,
    uint32_t stage,
    uint32_t state,
    uint32_t value);
constexpr uint32_t kFvfXyzrhwDiffuse = kD3dFvfXyzRhw | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzDiffuse = kD3dFvfXyz | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzrhwTex1 = kD3dFvfXyzRhw | kD3dFvfTex1;
constexpr uint32_t kFvfXyzTex1 = kD3dFvfXyz | kD3dFvfTex1;
constexpr uint32_t kFvfUnsupportedXyz = kD3dFvfXyz;

// D3DTSS_* texture stage state IDs (from d3d9types.h).
constexpr uint32_t kD3dTssColorOp = 1u;
constexpr uint32_t kD3dTssColorArg1 = 2u;
constexpr uint32_t kD3dTssAlphaOp = 4u;
constexpr uint32_t kD3dTssAlphaArg1 = 5u;
// D3DTEXTUREOP values (from d3d9types.h).
constexpr uint32_t kD3dTopDisable = 1u;
constexpr uint32_t kD3dTopSelectArg1 = 2u;
// Intentionally unsupported by the fixed-function texture stage subset.
constexpr uint32_t kD3dTopAddSigned2x = 9u; // D3DTOP_ADDSIGNED2X
// D3DTA_* source selector values (from d3d9types.h).
constexpr uint32_t kD3dTaCurrent = 1u;  // D3DTA_CURRENT
constexpr uint32_t kD3dTaSpecular = 4u; // D3DTA_SPECULAR (unsupported by fixed-function texture stage subset)

// D3DRS_* render state IDs (from d3d9types.h).
constexpr uint32_t kD3dRsLighting = 137u; // D3DRS_LIGHTING

// Trivial vs_2_0 token stream (no declaration):
//   mov oPos, v0
//   mov oD0, v1
//   mov oT0, v0
//   end
static constexpr uint32_t kUserVsPassthroughPosColor[] = {
    0xFFFE0200u, // vs_2_0
    0x03000001u, // mov
    0x400F0000u, // oPos.xyzw
    0x10E40000u, // v0.xyzw
    0x03000001u, // mov
    0x500F0000u, // oD0.xyzw
    0x10E40001u, // v1.xyzw
    0x03000001u, // mov
    0x600F0000u, // oT0.xyzw
    0x10E40000u, // v0.xyzw
    0x0000FFFFu, // end
};

// Trivial ps_2_0 token stream (no declaration):
//   mov oC0, v0
//   end
static constexpr uint32_t kUserPsPassthroughColor[] = {
    0xFFFF0200u, // ps_2_0
    0x03000001u, // mov
    0x000F0800u, // oC0.xyzw
    0x10E40000u, // v0.xyzw
    0x0000FFFFu, // end
};

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

// Minimal ps_2_0 instruction tokens used by fixed-function PS selection.
constexpr uint32_t kPsOpMul = 0x04000005u;
constexpr uint32_t kPsOpTexld = 0x04000042u;

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

struct CmdLoc {
  const aerogpu_cmd_hdr* hdr = nullptr;
  size_t offset = 0;
};

CmdLoc FindLastOpcode(const uint8_t* buf, size_t capacity, uint32_t opcode) {
  CmdLoc out{};
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (stream_len == 0) {
    return out;
  }
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      out.hdr = hdr;
      out.offset = offset;
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return out;
}

struct CleanupDevice {
  D3D9DDI_ADAPTERFUNCS adapter_funcs{};
  D3D9DDI_DEVICEFUNCS device_funcs{};
  D3DDDI_HADAPTER hAdapter{};
  D3DDDI_HDEVICE hDevice{};
  std::vector<D3DDDI_HRESOURCE> resources;
  std::vector<D3D9DDI_HSHADER> shaders;
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

  if (!Check(cleanup->device_funcs.pfnSetFVF != nullptr, "pfnSetFVF")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnCreateShader != nullptr, "pfnCreateShader")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnSetShader != nullptr, "pfnSetShader")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnCreateResource != nullptr, "pfnCreateResource")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnSetTexture != nullptr, "pfnSetTexture")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnDestroyShader != nullptr, "pfnDestroyShader")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnDestroyResource != nullptr, "pfnDestroyResource")) {
    return false;
  }
  return true;
}

bool CreateDummyTexture(CleanupDevice* cleanup, D3DDDI_HRESOURCE* out_tex) {
  if (!cleanup || !out_tex) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnCreateResource != nullptr, "pfnCreateResource")) {
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

bool CheckNoNullShaderBinds(const uint8_t* buf, size_t capacity) {
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (stream_len == 0) {
    return false;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (!Check(bind->vs != 0 && bind->ps != 0, "BIND_SHADERS must not bind null handles")) {
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

bool TestColorFillDoesNotBindNullShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnColorFill != nullptr, "pfnColorFill")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDestroyResource != nullptr, "pfnDestroyResource")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }

  // Repro: ensure the "saved" shader state for the blit helper is null (common
  // immediately after device creation). The command stream must never contain
  // BIND_SHADERS with vs==0 or ps==0.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs == nullptr && dev->ps == nullptr &&
                   dev->user_vs == nullptr && dev->user_ps == nullptr,
               "initial shader bindings are null")) {
      return false;
    }
  }

  dev->cmd.reset();

  D3D9DDIARG_COLORFILL fill{};
  fill.hDst = hTex;
  fill.pRect = nullptr;
  fill.color_argb = 0xFF112233u;
  fill.flags = 0;
  HRESULT hr = cleanup.device_funcs.pfnColorFill(cleanup.hDevice, &fill);
  if (!Check(hr == S_OK, "ColorFill")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ColorFill)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  return CheckNoNullShaderBinds(buf, len);
}

bool TestBltDoesNotBindNullShaders() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnBlt != nullptr, "pfnBlt")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDestroyResource != nullptr, "pfnDestroyResource")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  D3DDDI_HRESOURCE hSrc{};
  D3DDDI_HRESOURCE hDst{};
  if (!CreateDummyTexture(&cleanup, &hSrc)) {
    return false;
  }
  if (!CreateDummyTexture(&cleanup, &hDst)) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs == nullptr && dev->ps == nullptr &&
                   dev->user_vs == nullptr && dev->user_ps == nullptr,
               "initial shader bindings are null")) {
      return false;
    }
  }

  dev->cmd.reset();

  D3D9DDIARG_BLT blt{};
  blt.hSrc = hSrc;
  blt.hDst = hDst;
  blt.pSrcRect = nullptr;
  blt.pDstRect = nullptr;
  blt.filter = 0;
  blt.flags = 0;
  HRESULT hr = cleanup.device_funcs.pfnBlt(cleanup.hDevice, &blt);
  if (!Check(hr == S_OK, "Blt")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(Blt)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  return CheckNoNullShaderBinds(buf, len);
}

struct VertexXyzrhwDiffuse {
  float x;
  float y;
  float z;
  float rhw;
  uint32_t color;
};

struct VertexXyzrhwTex1 {
  float x;
  float y;
  float z;
  float rhw;
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

struct VertexXyzDiffuse {
  float x;
  float y;
  float z;
  uint32_t color;
};

bool TestVsOnlyBindsFixedfuncPs() {
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

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  auto* vs = reinterpret_cast<Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS)")) {
    return false;
  }

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(VS-only)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }

  // Ensure at least one bind references the user VS and binds a non-null PS.
  bool saw_user_vs_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (!Check(bind->vs != 0 && bind->ps != 0, "BIND_SHADERS must not bind null handles")) {
        return false;
      }
      if (bind->vs == vs_handle) {
        saw_user_vs_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_vs_bind, "saw BIND_SHADERS with user VS handle");
}

bool TestVsOnlyStage0StateUpdatesFixedfuncPs() {
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

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  // Bind VS only: the driver should bind a fixed-function PS fallback.
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS)")) {
    return false;
  }

  // With no texture bound, the fallback PS should be passthrough.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: initial PS does not contain texld")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: initial PS does not contain mul")) {
      return false;
    }
  }

  // Bind texture0: the stage0 PS should update immediately.
  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound after SetTexture")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: SetTexture PS contains texld")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: SetTexture PS contains mul")) {
      return false;
    }
  }

  // Disable stage0: PS should switch back to passthrough.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(COLOROP=DISABLE)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound after SetTextureStageState")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: DISABLE PS does not contain texld")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: DISABLE PS does not contain mul")) {
      return false;
    }
  }

  return true;
}

bool TestVsOnlyFogEnabledDoesNotSelectFogFixedfuncPs() {
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

  // Portable D3DRS_* numeric values (from d3d9types.h).
  constexpr uint32_t kD3dRsFogEnable = 28u;     // D3DRS_FOGENABLE
  constexpr uint32_t kD3dRsFogColor = 34u;      // D3DRS_FOGCOLOR
  constexpr uint32_t kD3dRsFogTableMode = 35u;  // D3DRS_FOGTABLEMODE
  constexpr uint32_t kD3dRsFogStart = 36u;      // D3DRS_FOGSTART (float bits)
  constexpr uint32_t kD3dRsFogEnd = 37u;        // D3DRS_FOGEND   (float bits)
  constexpr uint32_t kD3dFogLinear = 3u;        // D3DFOG_LINEAR

  // Set up VS-only interop: user VS bound, no user PS.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuse);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS)")) {
    return false;
  }

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.25f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.25f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.25f, 1.0f, 0xFF0000FFu},
  };

  // Baseline draw with fog disabled; record the selected fixed-function PS.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnable, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGENABLE=0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogTableMode, 0u);
  if (!Check(hr == S_OK, "SetRenderState(FOGTABLEMODE=0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only, fog off)")) {
    return false;
  }

  Shader* ps_off = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_off = dev->ps;
    if (!Check(ps_off != nullptr, "VS-only: fixed-function PS bound (fog off)")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(ps_off, 0x20E40001u), "VS-only: fog-off PS does not reference c1 (fog color)")) {
      return false;
    }
  }

  // Reset the stream so we can validate that fog does not trigger fog constant uploads.
  dev->cmd.reset();

  // Enable linear fog. In VS-only interop, fog must be ignored (the fixed-function
  // PS fallback must not expect TEXCOORD0.z fog coordinates from the user VS).
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
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogStart, 0x3E4CCCCDu /*0.2f*/);
  if (!Check(hr == S_OK, "SetRenderState(FOGSTART)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsFogEnd, 0x3F4CCCCDu /*0.8f*/);
  if (!Check(hr == S_OK, "SetRenderState(FOGEND)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only, fog on)")) {
    return false;
  }

  Shader* ps_on = nullptr;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_on = dev->ps;
    if (!Check(ps_on != nullptr, "VS-only: fixed-function PS bound (fog on)")) {
      return false;
    }
    if (!Check(ps_on == ps_off, "VS-only: fog does not change fixed-function PS selection")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(ps_on, 0x20E40001u), "VS-only: fog-on PS still does not reference c1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(VS-only fog enabled)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }

  // Ensure fog constant uploads (pixel shader c1..c2) did not occur.
  size_t fog_const_uploads = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_SET_SHADER_CONSTANTS_F &&
        hdr->size_bytes >= sizeof(aerogpu_cmd_set_shader_constants_f)) {
      const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
      if (sc->stage == AEROGPU_SHADER_STAGE_PIXEL && sc->start_register == 1 && sc->vec4_count == 2) {
        ++fog_const_uploads;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  if (!Check(fog_const_uploads == 0, "VS-only: fog does not upload fixed-function fog constants")) {
    return false;
  }

  return CheckNoNullShaderBinds(buf, len);
}

bool TestVsOnlyUnsupportedStage0StateSetShaderSucceedsDrawFails() {
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

  // Set an unsupported stage0 op. This should not make subsequent state-setting
  // (including shader-stage interop) fail; draws should fail cleanly with INVALIDCALL.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, kD3dTopAddSigned2x);
  if (!Check(hr == S_OK, "SetTextureStageState(COLOROP=ADDSIGNED2X) succeeds")) {
    return false;
  }

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  auto* vs = reinterpret_cast<Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS) succeeds even when stage0 is unsupported")) {
    return false;
  }

  const size_t baseline = dev->cmd.bytes_used();

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(VS-only, unsupported stage0) returns INVALIDCALL")) {
    return false;
  }
  if (!Check(dev->cmd.bytes_used() == baseline, "unsupported draw emits no new commands")) {
    return false;
  }

  // Restore a supported stage0 op and ensure draws recover.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(COLOROP=DISABLE) succeeds")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only, stage0 DISABLE) succeeds")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(VS-only: unsupported stage0 then DISABLE)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 1, "exactly one DRAW opcode emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW_INDEXED) == 0, "no DRAW_INDEXED opcodes emitted")) {
    return false;
  }
  if (!Check(CheckNoNullShaderBinds(buf, len), "BIND_SHADERS must not bind null handles")) {
    return false;
  }

  // Ensure we observed a bind that included the user VS handle (interop path binds
  // user VS + internal PS).
  bool saw_user_vs_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (bind->vs == vs_handle) {
        saw_user_vs_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_vs_bind, "saw BIND_SHADERS with user VS handle");
}

bool TestVsOnlyUnsupportedStage0ArgStateSetShaderSucceedsDrawFails() {
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

  // Configure an unsupported stage0 argument source (SPECULAR).
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "SetTextureStageState(COLOROP=SELECTARG1) succeeds")) {
    return false;
  }
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssColorArg1, kD3dTaSpecular);
  if (!Check(hr == S_OK, "SetTextureStageState(COLORARG1=SPECULAR) succeeds")) {
    return false;
  }
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssAlphaOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(ALPHAOP=DISABLE) succeeds")) {
    return false;
  }

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  auto* vs = reinterpret_cast<Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS) succeeds even when stage0 arg is unsupported")) {
    return false;
  }

  const size_t baseline = dev->cmd.bytes_used();

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(VS-only, unsupported stage0 arg) returns INVALIDCALL")) {
    return false;
  }
  if (!Check(dev->cmd.bytes_used() == baseline, "unsupported draw emits no new commands")) {
    return false;
  }

  // Restore a supported stage0 op and ensure draws recover.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(COLOROP=DISABLE) succeeds (recover)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only, recovered stage0) succeeds")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(VS-only: unsupported stage0 arg then DISABLE)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 1, "exactly one DRAW opcode emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW_INDEXED) == 0, "no DRAW_INDEXED opcodes emitted")) {
    return false;
  }
  if (!Check(CheckNoNullShaderBinds(buf, len), "BIND_SHADERS must not bind null handles")) {
    return false;
  }

  bool saw_user_vs_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (bind->vs == vs_handle) {
        saw_user_vs_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_vs_bind, "saw BIND_SHADERS with user VS handle");
}

bool TestVsOnlyUnsupportedStage0AlphaOpSetShaderSucceedsDrawFails() {
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

  // Bind a stage0 texture so stage0 uses the texture-sampling path.
  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  // Make stage0 unsupported by setting an unsupported ALPHAOP.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssAlphaOp, kD3dTopAddSigned2x);
  if (!Check(hr == S_OK, "SetTextureStageState(ALPHAOP=ADDSIGNED2X) succeeds")) {
    return false;
  }

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  auto* vs = reinterpret_cast<Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS) succeeds even when stage0 alpha op is unsupported")) {
    return false;
  }

  // With stage0 unsupported, the VS-only interop path must fall back to a safe
  // passthrough PS (no texld/mul).
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: fallback PS does not contain texld")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: fallback PS does not contain mul")) {
      return false;
    }
  }

  const size_t baseline = dev->cmd.bytes_used();

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(VS-only, unsupported stage0 alpha op) returns INVALIDCALL")) {
    return false;
  }
  if (!Check(dev->cmd.bytes_used() == baseline, "unsupported draw emits no new commands")) {
    return false;
  }

  // Restore a supported alpha op and ensure draws recover.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssAlphaOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(ALPHAOP=DISABLE) succeeds (recover)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound after recover")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: recovered PS contains texld")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: recovered PS contains mul")) {
      return false;
    }
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only, recovered stage0 alpha op) succeeds")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(VS-only: alpha op unsupported then DISABLE)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 1, "exactly one DRAW opcode emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW_INDEXED) == 0, "no DRAW_INDEXED opcodes emitted")) {
    return false;
  }
  if (!Check(CheckNoNullShaderBinds(buf, len), "BIND_SHADERS must not bind null handles")) {
    return false;
  }

  bool saw_user_vs_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (bind->vs == vs_handle) {
        saw_user_vs_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_vs_bind, "saw BIND_SHADERS with user VS handle");
}

bool TestVsOnlyUnsupportedStage0AlphaArgStateSetShaderSucceedsDrawFails() {
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

  // Bind a stage0 texture so stage0 uses the texture-sampling path.
  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  // Configure an unsupported stage0 alpha argument source (SPECULAR).
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "SetTextureStageState(ALPHAOP=SELECTARG1) succeeds")) {
    return false;
  }
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssAlphaArg1, kD3dTaSpecular);
  if (!Check(hr == S_OK, "SetTextureStageState(ALPHAARG1=SPECULAR) succeeds")) {
    return false;
  }

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  auto* vs = reinterpret_cast<Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS) succeeds even when stage0 alpha arg is unsupported")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: fallback PS does not contain texld")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: fallback PS does not contain mul")) {
      return false;
    }
  }

  const size_t baseline = dev->cmd.bytes_used();

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(VS-only, unsupported stage0 alpha arg) returns INVALIDCALL")) {
    return false;
  }
  if (!Check(dev->cmd.bytes_used() == baseline, "unsupported draw emits no new commands")) {
    return false;
  }

  // Restore a supported alpha op and ensure draws recover. Disabling ALPHAOP also
  // ensures the unsupported ALPHAARG1 source is ignored.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssAlphaOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(ALPHAOP=DISABLE) succeeds (recover)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound after recover")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: recovered PS contains texld")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: recovered PS contains mul")) {
      return false;
    }
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only, recovered stage0 alpha arg) succeeds")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(VS-only: alpha arg unsupported then DISABLE)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 1, "exactly one DRAW opcode emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW_INDEXED) == 0, "no DRAW_INDEXED opcodes emitted")) {
    return false;
  }
  if (!Check(CheckNoNullShaderBinds(buf, len), "BIND_SHADERS must not bind null handles")) {
    return false;
  }

  bool saw_user_vs_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (bind->vs == vs_handle) {
        saw_user_vs_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_vs_bind, "saw BIND_SHADERS with user VS handle");
}

bool TestVsOnlyUnsupportedStage1StateSetShaderSucceedsDrawFails() {
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

  // Bind a stage0 texture so stage1 is actually evaluated by the fixed-function
  // stage-state decoder (otherwise stage0 would short-circuit to passthrough).
  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  // Enable stage1 with an unsupported op. This should not make shader binding
  // (state setting) fail, but draws must fail with INVALIDCALL.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/1, kD3dTssColorOp, kD3dTopAddSigned2x);
  if (!Check(hr == S_OK, "SetTextureStageState(stage1 COLOROP=ADDSIGNED2X) succeeds")) {
    return false;
  }

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  auto* vs = reinterpret_cast<Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS) succeeds even when stage1 is unsupported")) {
    return false;
  }

  // With stage1 unsupported, the VS-only interop path must fall back to a safe
  // passthrough PS (no texld/mul).
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: fallback PS does not contain texld")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: fallback PS does not contain mul")) {
      return false;
    }
  }

  const size_t baseline = dev->cmd.bytes_used();

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(VS-only, unsupported stage1) returns INVALIDCALL")) {
    return false;
  }
  if (!Check(dev->cmd.bytes_used() == baseline, "unsupported draw emits no new commands")) {
    return false;
  }

  // Disable stage1 to restore a supported stage chain (stage0 MODULATE with a
  // bound texture).
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/1, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(stage1 COLOROP=DISABLE) succeeds (recover)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound after recover")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: recovered PS contains texld")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: recovered PS contains mul")) {
      return false;
    }
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only, recovered stage1) succeeds")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(VS-only: stage1 unsupported then DISABLE)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 1, "exactly one DRAW opcode emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW_INDEXED) == 0, "no DRAW_INDEXED opcodes emitted")) {
    return false;
  }
  if (!Check(CheckNoNullShaderBinds(buf, len), "BIND_SHADERS must not bind null handles")) {
    return false;
  }

  bool saw_user_vs_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (bind->vs == vs_handle) {
        saw_user_vs_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_vs_bind, "saw BIND_SHADERS with user VS handle");
}

bool TestVsOnlyUnsupportedStage2StateSetShaderSucceedsDrawFails() {
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

  // Ensure stage0 is active (otherwise stage0 would short-circuit to passthrough
  // and subsequent stages would not be evaluated).
  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  // Enable stage1 in a supported way without requiring a stage1 texture (use
  // CURRENT so we don't sample an unbound stage1 slot).
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/1, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "SetTextureStageState(stage1 COLOROP=SELECTARG1) succeeds")) {
    return false;
  }
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/1, kD3dTssColorArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "SetTextureStageState(stage1 COLORARG1=CURRENT) succeeds")) {
    return false;
  }
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/1, kD3dTssAlphaOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(stage1 ALPHAOP=DISABLE) succeeds")) {
    return false;
  }

  // Enable stage2 with an unsupported op. This should not make shader binding
  // (state setting) fail, but draws must fail with INVALIDCALL.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/2, kD3dTssColorOp, kD3dTopAddSigned2x);
  if (!Check(hr == S_OK, "SetTextureStageState(stage2 COLOROP=ADDSIGNED2X) succeeds")) {
    return false;
  }

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  auto* vs = reinterpret_cast<Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS) succeeds even when stage2 is unsupported")) {
    return false;
  }

  // With stage2 unsupported, the VS-only interop path must fall back to a safe
  // passthrough PS (no texld/mul).
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: fallback PS does not contain texld")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: fallback PS does not contain mul")) {
      return false;
    }
  }

  const size_t baseline = dev->cmd.bytes_used();

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(VS-only, unsupported stage2) returns INVALIDCALL")) {
    return false;
  }
  if (!Check(dev->cmd.bytes_used() == baseline, "unsupported draw emits no new commands")) {
    return false;
  }

  // Disable stage2 to restore a supported stage chain.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(stage2 COLOROP=DISABLE) succeeds (recover)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound after recover")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: recovered PS contains texld")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: recovered PS contains mul")) {
      return false;
    }
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only, recovered stage2) succeeds")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(VS-only: stage2 unsupported then DISABLE)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 1, "exactly one DRAW opcode emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW_INDEXED) == 0, "no DRAW_INDEXED opcodes emitted")) {
    return false;
  }
  if (!Check(CheckNoNullShaderBinds(buf, len), "BIND_SHADERS must not bind null handles")) {
    return false;
  }

  bool saw_user_vs_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (bind->vs == vs_handle) {
        saw_user_vs_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_vs_bind, "saw BIND_SHADERS with user VS handle");
}

bool TestVsOnlyUnsupportedStage3StateSetShaderSucceedsDrawFails() {
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

  // Ensure stage0 is active (otherwise stage0 would short-circuit to passthrough
  // and subsequent stages would not be evaluated).
  D3DDDI_HRESOURCE hTex{};
  if (!CreateDummyTexture(&cleanup, &hTex)) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }

  // Enable stage1 and stage2 in a supported way without requiring their textures
  // (use CURRENT so we don't sample unbound slots). This ensures stage3 is
  // actually evaluated by the fixed-function stage-state decoder.
  for (uint32_t stage = 1; stage <= 2; ++stage) {
    hr = device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorOp, kD3dTopSelectArg1);
    if (!Check(hr == S_OK, "SetTextureStageState(stageN COLOROP=SELECTARG1) succeeds")) {
      return false;
    }
    hr = device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorArg1, kD3dTaCurrent);
    if (!Check(hr == S_OK, "SetTextureStageState(stageN COLORARG1=CURRENT) succeeds")) {
      return false;
    }
    hr = device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssAlphaOp, kD3dTopDisable);
    if (!Check(hr == S_OK, "SetTextureStageState(stageN ALPHAOP=DISABLE) succeeds")) {
      return false;
    }
  }

  // Enable stage3 with an unsupported op. This should not make shader binding
  // (state setting) fail, but draws must fail with INVALIDCALL.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/3, kD3dTssColorOp, kD3dTopAddSigned2x);
  if (!Check(hr == S_OK, "SetTextureStageState(stage3 COLOROP=ADDSIGNED2X) succeeds")) {
    return false;
  }

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  auto* vs = reinterpret_cast<Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS) succeeds even when stage3 is unsupported")) {
    return false;
  }

  // With stage3 unsupported, the VS-only interop path must fall back to a safe
  // passthrough PS (no texld/mul).
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: fallback PS does not contain texld")) {
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: fallback PS does not contain mul")) {
      return false;
    }
  }

  const size_t baseline = dev->cmd.bytes_used();

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(VS-only, unsupported stage3) returns INVALIDCALL")) {
    return false;
  }
  if (!Check(dev->cmd.bytes_used() == baseline, "unsupported draw emits no new commands")) {
    return false;
  }

  // Disable stage3 to restore a supported stage chain.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/3, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(stage3 COLOROP=DISABLE) succeeds (recover)")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "VS-only: PS bound after recover")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpTexld), "VS-only: recovered PS contains texld")) {
      return false;
    }
    if (!Check(ShaderContainsToken(dev->ps, kPsOpMul), "VS-only: recovered PS contains mul")) {
      return false;
    }
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only, recovered stage3) succeeds")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(VS-only: stage3 unsupported then DISABLE)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 1, "exactly one DRAW opcode emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW_INDEXED) == 0, "no DRAW_INDEXED opcodes emitted")) {
    return false;
  }
  if (!Check(CheckNoNullShaderBinds(buf, len), "BIND_SHADERS must not bind null handles")) {
    return false;
  }

  bool saw_user_vs_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (bind->vs == vs_handle) {
        saw_user_vs_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_vs_bind, "saw BIND_SHADERS with user VS handle");
}

bool TestVsOnlyUnsupportedStage0DestroyShaderSucceedsAndRebinds() {
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

  // Unsupported stage0 op: shader binding must still succeed, but draws must fail.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, kD3dTopAddSigned2x);
  if (!Check(hr == S_OK, "SetTextureStageState(COLOROP=ADDSIGNED2X) succeeds")) {
    return false;
  }

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  const size_t vs_index = cleanup.shaders.size();
  cleanup.shaders.push_back(hVs);

  const auto* vs = reinterpret_cast<const Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS) succeeds with unsupported stage0")) {
    return false;
  }

  // Destroy the currently bound user VS. This must succeed and must rebind a
  // non-null shader pair before emitting DESTROY_SHADER so the command stream is
  // valid and never references a freed handle.
  hr = cleanup.device_funcs.pfnDestroyShader(cleanup.hDevice, hVs);
  if (!Check(hr == S_OK, "DestroyShader(VS) succeeds with unsupported stage0")) {
    return false;
  }
  // Prevent CleanupDevice from destroying the same shader again.
  cleanup.shaders[vs_index].pDrvPrivate = nullptr;

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(DestroyShader)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DESTROY_SHADER) >= 1, "DESTROY_SHADER emitted")) {
    return false;
  }
  if (!Check(CheckNoNullShaderBinds(buf, len), "BIND_SHADERS must not bind null handles")) {
    return false;
  }

  // Validate that the last BIND_SHADERS before each DESTROY_SHADER does not bind
  // the shader handle being destroyed.
  bool saw_bind = false;
  aerogpu_handle_t last_vs = 0;
  aerogpu_handle_t last_ps = 0;

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      last_vs = bind->vs;
      last_ps = bind->ps;
      saw_bind = true;
    }
    if (hdr->opcode == AEROGPU_CMD_DESTROY_SHADER && hdr->size_bytes >= sizeof(aerogpu_cmd_destroy_shader)) {
      const auto* destroy = reinterpret_cast<const aerogpu_cmd_destroy_shader*>(hdr);
      if (!Check(saw_bind, "saw BIND_SHADERS before DESTROY_SHADER")) {
        return false;
      }
      if (!Check(last_vs != destroy->shader_handle && last_ps != destroy->shader_handle,
                 "DESTROY_SHADER handle not bound by last BIND_SHADERS")) {
        return false;
      }
      if (destroy->shader_handle == vs_handle) {
        // For the shader we destroyed, we expect the last bind before destroy to
        // not reference the old VS handle.
        if (!Check(last_vs != vs_handle, "rebound away from user VS before destroy")) {
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

bool TestVsOnlyUnsupportedStage0DestroyPixelShaderSucceedsAndRebinds() {
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

  // Unsupported stage0 op: state-setting must succeed, but draws must fail once we
  // return to VS-only interop (after destroying the user PS).
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, kD3dTopAddSigned2x);
  if (!Check(hr == S_OK, "SetTextureStageState(COLOROP=ADDSIGNED2X) succeeds")) {
    return false;
  }

  // Create user VS.
  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  const auto* vs = reinterpret_cast<const Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  // Create user PS.
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStagePs,
                                            kUserPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(kUserPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS)")) {
    return false;
  }
  if (!Check(hPs.pDrvPrivate != nullptr, "CreateShader(PS) returned handle")) {
    return false;
  }
  const size_t ps_index = cleanup.shaders.size();
  cleanup.shaders.push_back(hPs);

  const auto* ps = reinterpret_cast<const Shader*>(hPs.pDrvPrivate);
  const aerogpu_handle_t ps_handle = ps ? ps->handle : 0;

  // Bind VS only (VS-only interop; should bind a safe fallback PS even though
  // stage0 is unsupported).
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS) succeeds with unsupported stage0")) {
    return false;
  }

  // Bind PS too (full programmable pipeline).
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS) succeeds")) {
    return false;
  }

  // Reset stream so we only observe the rebind + destroy sequence.
  dev->cmd.reset();

  // Destroy the currently bound user PS. This must succeed and must rebind a
  // non-null shader pair before emitting DESTROY_SHADER so the command stream is
  // valid and never references a freed handle.
  hr = cleanup.device_funcs.pfnDestroyShader(cleanup.hDevice, hPs);
  if (!Check(hr == S_OK, "DestroyShader(PS) succeeds with unsupported stage0")) {
    return false;
  }
  // Prevent CleanupDevice from destroying the same shader again.
  cleanup.shaders[ps_index].pDrvPrivate = nullptr;

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(DestroyShader PS)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DESTROY_SHADER) >= 1, "DESTROY_SHADER emitted")) {
    return false;
  }
  if (!Check(CheckNoNullShaderBinds(buf, len), "BIND_SHADERS must not bind null handles")) {
    return false;
  }

  bool saw_bind = false;
  aerogpu_handle_t last_vs = 0;
  aerogpu_handle_t last_ps = 0;
  bool saw_destroyed_ps = false;

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      last_vs = bind->vs;
      last_ps = bind->ps;
      saw_bind = true;
    }
    if (hdr->opcode == AEROGPU_CMD_DESTROY_SHADER && hdr->size_bytes >= sizeof(aerogpu_cmd_destroy_shader)) {
      const auto* destroy = reinterpret_cast<const aerogpu_cmd_destroy_shader*>(hdr);
      if (destroy->shader_handle == ps_handle) {
        saw_destroyed_ps = true;
        if (!Check(saw_bind, "saw BIND_SHADERS before DESTROY_SHADER(PS)")) {
          return false;
        }
        if (!Check(last_ps != ps_handle, "rebound away from user PS before destroy")) {
          return false;
        }
        if (!Check(last_vs == vs_handle, "kept user VS bound when destroying PS")) {
          return false;
        }
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }

  return Check(saw_destroyed_ps, "saw DESTROY_SHADER for user PS handle");
}

bool TestVsOnlyUnsupportedStage0ApplyStateBlockSetShaderSucceedsDrawFails() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock")) {
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

  // Make stage0 unsupported *before* applying the state block that binds a VS.
  // Regression: ApplyStateBlock must still succeed; only draws should fail.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, kD3dTopAddSigned2x);
  if (!Check(hr == S_OK, "SetTextureStageState(COLOROP=ADDSIGNED2X) succeeds")) {
    return false;
  }

  // Create a user VS for VS-only interop.
  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  auto* vs = reinterpret_cast<Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  // Record a state block that binds the VS (and leaves PS unset => VS-only interop).
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS) during BeginStateBlock")) {
    return false;
  }
  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Clear VS so ApplyStateBlock must re-bind it (and hit the VS-only interop path).
  D3D9DDI_HSHADER null_shader{};
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, null_shader);
  if (!Check(hr == S_OK, "SetShader(VS=null)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Reset the command stream so we only observe ApplyStateBlock + the failing draw.
  dev->cmd.reset();

  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock succeeds with unsupported stage0 + VS-only interop")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  const size_t baseline = dev->cmd.bytes_used();

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(VS-only, unsupported stage0) returns INVALIDCALL")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(dev->cmd.bytes_used() == baseline, "unsupported draw emits no new commands")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock unsupported stage0)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 0, "no DRAW opcodes emitted")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW_INDEXED) == 0, "no DRAW_INDEXED opcodes emitted")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CheckNoNullShaderBinds(buf, len), "BIND_SHADERS must not bind null handles")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Ensure ApplyStateBlock resulted in a bind referencing the user VS handle.
  bool saw_user_vs_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (bind->vs == vs_handle) {
        saw_user_vs_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return Check(saw_user_vs_bind, "saw BIND_SHADERS with user VS handle after ApplyStateBlock");
}

bool TestVsOnlyApplyStateBlockSetsUnsupportedStage0StateSucceedsDrawFails() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock")) {
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

  // Create a user VS for VS-only interop.
  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  auto* vs = reinterpret_cast<Shader*>(hVs.pDrvPrivate);
  const aerogpu_handle_t vs_handle = vs ? vs->handle : 0;

  // Record a state block that *sets* an unsupported stage0 state and binds a VS.
  // Regression: ApplyStateBlock must tolerate the unsupported stage state and
  // succeed; only draws should fail.
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock")) {
    return false;
  }
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, kD3dTopAddSigned2x);
  if (!Check(hr == S_OK, "SetTextureStageState(COLOROP=ADDSIGNED2X) during BeginStateBlock succeeds")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, hVs);
  if (!Check(hr == S_OK, "SetShader(VS) during BeginStateBlock succeeds")) {
    return false;
  }
  D3D9DDI_HSTATEBLOCK hSb{};
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &hSb);
  if (!Check(hr == S_OK, "EndStateBlock")) {
    return false;
  }
  if (!Check(hSb.pDrvPrivate != nullptr, "EndStateBlock returned handle")) {
    return false;
  }

  // Restore a supported stage0 state and clear VS so ApplyStateBlock must set
  // both stage state and VS again.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "SetTextureStageState(COLOROP=DISABLE) succeeds (restore)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  D3D9DDI_HSHADER null_shader{};
  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStageVs, null_shader);
  if (!Check(hr == S_OK, "SetShader(VS=null)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Reset the command stream so we only observe ApplyStateBlock + the failing draw.
  dev->cmd.reset();

  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, hSb);
  if (!Check(hr == S_OK, "ApplyStateBlock succeeds when applying unsupported stage0 state + VS-only interop")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Applying the state block must have bound a safe passthrough PS (no texld/mul).
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "ApplyStateBlock: PS bound")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpTexld), "ApplyStateBlock: fallback PS does not contain texld")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
    if (!Check(!ShaderContainsToken(dev->ps, kPsOpMul), "ApplyStateBlock: fallback PS does not contain mul")) {
      cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
      return false;
    }
  }

  const size_t baseline = dev->cmd.bytes_used();

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(VS-only, unsupported stage0) returns INVALIDCALL")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(dev->cmd.bytes_used() == baseline, "unsupported draw emits no new commands")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(ApplyStateBlock applies unsupported stage0 state)")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 0, "no DRAW opcodes emitted")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW_INDEXED) == 0, "no DRAW_INDEXED opcodes emitted")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }
  if (!Check(CheckNoNullShaderBinds(buf, len), "BIND_SHADERS must not bind null handles")) {
    cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
    return false;
  }

  // Ensure ApplyStateBlock resulted in a bind referencing the user VS handle.
  bool saw_user_vs_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (bind->vs == vs_handle) {
        saw_user_vs_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }

  cleanup.device_funcs.pfnDeleteStateBlock(cleanup.hDevice, hSb);
  return Check(saw_user_vs_bind, "saw BIND_SHADERS with user VS handle after ApplyStateBlock");
}

bool TestPsOnlyBindsFixedfuncVs() {
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

  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStagePs,
                                            kUserPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(kUserPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS)")) {
    return false;
  }
  if (!Check(hPs.pDrvPrivate != nullptr, "CreateShader(PS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  auto* ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
  const aerogpu_handle_t ps_handle = ps ? ps->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS)")) {
    return false;
  }

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }

  // Ensure at least one bind references the user PS and binds a non-null VS.
  bool saw_user_ps_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (!Check(bind->vs != 0 && bind->ps != 0, "BIND_SHADERS must not bind null handles")) {
        return false;
      }
      if (bind->ps == ps_handle) {
        saw_user_ps_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_ps_bind, "saw BIND_SHADERS with user PS handle");
}

bool TestPsOnlyBindsFixedfuncVsTex1() {
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

  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStagePs,
                                            kUserPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(kUserPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS)")) {
    return false;
  }
  if (!Check(hPs.pDrvPrivate != nullptr, "CreateShader(PS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  auto* ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
  const aerogpu_handle_t ps_handle = ps ? ps->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS)")) {
    return false;
  }

  const VertexXyzrhwTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only, XYZRHW|TEX1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only, TEX1)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }

  bool saw_user_ps_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (!Check(bind->vs != 0 && bind->ps != 0, "BIND_SHADERS must not bind null handles")) {
        return false;
      }
      if (bind->ps == ps_handle) {
        saw_user_ps_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_ps_bind, "saw BIND_SHADERS with user PS handle");
}

bool TestPsOnlyBindsFixedfuncVsXyzTex1() {
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
                                            kD3d9ShaderStagePs,
                                            kUserPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(kUserPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS)")) {
    return false;
  }
  if (!Check(hPs.pDrvPrivate != nullptr, "CreateShader(PS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  auto* ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
  const aerogpu_handle_t ps_handle = ps ? ps->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS)")) {
    return false;
  }

  const VertexXyzTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only, XYZ|TEX1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only, XYZ|TEX1)")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }

  bool saw_user_ps_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (!Check(bind->vs != 0 && bind->ps != 0, "BIND_SHADERS must not bind null handles")) {
        return false;
      }
      if (bind->ps == ps_handle) {
        saw_user_ps_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return Check(saw_user_ps_bind, "saw BIND_SHADERS with user PS handle");
}

bool TestPsOnlyXyzTex1LightingEnabledStillDraws() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState")) {
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

  // Bind a user PS (VS stays NULL).
  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStagePs,
                                            kUserPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(kUserPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS)")) {
    return false;
  }
  if (!Check(hPs.pDrvPrivate != nullptr, "CreateShader(PS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  auto* ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
  const aerogpu_handle_t ps_handle = ps ? ps->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS)")) {
    return false;
  }

  // Lighting is not implemented under PS-only interop (to avoid clobbering user
  // VS constants with the large lighting block). It must also not cause
  // spurious INVALIDCALL errors for FVFs without normals.
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsLighting, 1u);
  if (!Check(hr == S_OK, "SetRenderState(LIGHTING=TRUE)")) {
    return false;
  }

  const VertexXyzTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only, XYZ|TEX1; lighting=on)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->vs != nullptr, "PS-only: synthesized VS is bound")) {
      return false;
    }
    if (!Check(ShaderBytecodeEquals(dev->vs, fixedfunc::kVsTransformPosWhiteTex1),
               "PS-only: synthesized VS bytecode matches kVsTransformPosWhiteTex1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only XYZ|TEX1 lighting=on)")) {
    return false;
  }

  // Lighting constant uploads must not be emitted under PS-only interop.
  size_t lighting_uploads = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_SET_SHADER_CONSTANTS_F &&
        hdr->size_bytes >= sizeof(aerogpu_cmd_set_shader_constants_f)) {
      const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
      if (sc->stage == AEROGPU_SHADER_STAGE_VERTEX &&
          sc->start_register == kFixedfuncLightingStartRegister &&
          sc->vec4_count == kFixedfuncLightingVec4Count) {
        lighting_uploads++;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  if (!Check(lighting_uploads == 0, "PS-only XYZ|TEX1 does not upload lighting constants")) {
    return false;
  }

  bool saw_user_ps_bind = false;
  offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (!Check(bind->vs != 0 && bind->ps != 0, "BIND_SHADERS must not bind null handles")) {
        return false;
      }
      if (bind->ps == ps_handle) {
        saw_user_ps_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  if (!Check(saw_user_ps_bind, "saw BIND_SHADERS with user PS handle")) {
    return false;
  }
  return CheckNoNullShaderBinds(buf, len);
}

bool TestPsOnlyIgnoresUnsupportedStage0State() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }
  dev->cmd.reset();

  // PS-only interop: bind a user PS but leave VS unset so the draw path injects
  // a fixed-function VS fallback derived from the active FVF.
  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZ|TEX1)")) {
    return false;
  }

  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStagePs,
                                            kUserPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(kUserPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS)")) {
    return false;
  }
  if (!Check(hPs.pDrvPrivate != nullptr, "CreateShader(PS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  auto* ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
  const aerogpu_handle_t ps_handle = ps ? ps->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS)")) {
    return false;
  }

  // Set an intentionally unsupported stage0 texture op. Since a user PS is
  // bound, fixed-function stage-state emulation must be ignored (D3D9 semantics)
  // and the draw must still succeed.
  hr = device_set_texture_stage_state(cleanup.hDevice, /*stage=*/0, kD3dTssColorOp, kD3dTopAddSigned2x);
  if (!Check(hr == S_OK, "SetTextureStageState(COLOROP=ADDSIGNED2X)")) {
    return false;
  }

  const VertexXyzTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzTex1));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only, unsupported stage0 state)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only, unsupported stage0 state)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }

  bool saw_user_ps_bind = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (!Check(bind->vs != 0 && bind->ps != 0, "BIND_SHADERS must not bind null handles")) {
        return false;
      }
      if (bind->ps == ps_handle) {
        saw_user_ps_bind = true;
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  if (!Check(saw_user_ps_bind, "saw BIND_SHADERS with user PS handle")) {
    return false;
  }
  return CheckNoNullShaderBinds(buf, len);
}

bool TestPsOnlyXyzDiffuseBindsWvpVs() {
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

  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStagePs,
                                            kUserPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(kUserPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS)")) {
    return false;
  }
  if (!Check(hPs.pDrvPrivate != nullptr, "CreateShader(PS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  auto* ps = reinterpret_cast<Shader*>(hPs.pDrvPrivate);
  const aerogpu_handle_t ps_handle = ps ? ps->handle : 0;

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS)")) {
    return false;
  }

  // Force a deterministic WVP upload. Fixed-function XYZ interop uses an internal
  // WVP VS variant and uploads the matrix into c240..c243 as column vectors.
  const float expected_wvp_cols[16] = {
      1.0f, 0.0f, 0.0f, 0.0f,
      0.0f, 1.0f, 0.0f, 0.0f,
      0.0f, 0.0f, 1.0f, 0.0f,
      0.0f, 0.0f, 0.0f, 1.0f,
  };
  // Use the public SetTransform DDI so the driver's dirty tracking and stateblock
  // recording paths are exercised (avoid poking `Device::transform_matrices`
  // directly in host-side tests).
  constexpr uint32_t kD3dTransformView = 2u;
  constexpr uint32_t kD3dTransformProjection = 3u;
  constexpr uint32_t kD3dTransformWorld0 = 256u;
  if (!Check(cleanup.device_funcs.pfnSetTransform != nullptr, "pfnSetTransform is available")) {
    return false;
  }
  // Ensure the driver must actually re-upload the fixed-function WVP constants
  // in this command stream: the device may have already populated c240..c243
  // during earlier setup (e.g. when synthesizing the fixed-function VS).
  if (!Check(cleanup.device_funcs.pfnSetShaderConstF != nullptr, "pfnSetShaderConstF is available")) {
    return false;
  }
  {
    const float zeros[16] = {};
    hr = cleanup.device_funcs.pfnSetShaderConstF(cleanup.hDevice,
                                                 kD3d9ShaderStageVs,
                                                 /*start_reg=*/kFixedfuncMatrixStartRegister,
                                                 zeros,
                                                 /*vec4_count=*/kFixedfuncMatrixVec4Count);
    if (!Check(hr == S_OK, "SetShaderConstF(clobber fixedfunc WVP range)")) {
      return false;
    }
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

  // Force `fixedfunc_matrix_dirty` for the next draw by toggling WORLD0 away
  // from identity and back. This avoids relying on redundant SetTransform calls
  // to force constant uploads (the driver may skip uploads when the matrix
  // value is unchanged).
  D3DMATRIX world_tmp = identity;
  world_tmp.m[3][0] = 1.0f; // translation x
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &world_tmp);
  if (!Check(hr == S_OK, "SetTransform(WORLD0) temporary")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTransform(cleanup.hDevice, kD3dTransformWorld0, &identity);
  if (!Check(hr == S_OK, "SetTransform(WORLD0)")) {
    return false;
  }

  const VertexXyzDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(PS-only, XYZ|DIFFUSE)")) {
    return false;
  }

  aerogpu_handle_t wvp_vs_handle = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    auto& pipe = dev->fixedfunc_pipelines[static_cast<size_t>(FixedFuncVariant::XYZ_COLOR)];
    if (!Check(pipe.vs != nullptr, "fixedfunc XYZ_COLOR VS created")) {
      return false;
    }
    wvp_vs_handle = pipe.vs->handle;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(PS-only XYZ|DIFFUSE)")) {
    return false;
  }

  bool saw_wvp_vs_bind = false;
  bool saw_wvp_constants = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = StreamBytesUsed(buf, len);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders)) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      if (!Check(bind->vs != 0 && bind->ps != 0, "BIND_SHADERS must not bind null handles")) {
        return false;
      }
      if (bind->ps == ps_handle && bind->vs == wvp_vs_handle) {
        saw_wvp_vs_bind = true;
      }
    }
    if (hdr->opcode == AEROGPU_CMD_SET_SHADER_CONSTANTS_F &&
        hdr->size_bytes >= sizeof(aerogpu_cmd_set_shader_constants_f) + sizeof(expected_wvp_cols)) {
      const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
      if (sc->stage == AEROGPU_SHADER_STAGE_VERTEX &&
          sc->start_register == kFixedfuncMatrixStartRegister &&
          sc->vec4_count == kFixedfuncMatrixVec4Count) {
        const float* payload = reinterpret_cast<const float*>(
            reinterpret_cast<const uint8_t*>(sc) + sizeof(aerogpu_cmd_set_shader_constants_f));
        if (std::memcmp(payload, expected_wvp_cols, sizeof(expected_wvp_cols)) == 0) {
          saw_wvp_constants = true;
        }
      }
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }

  if (!Check(saw_wvp_vs_bind, "saw BIND_SHADERS with WVP VS handle + user PS handle")) {
    return false;
  }
  return Check(saw_wvp_constants, "PS-only XYZ|DIFFUSE uploaded identity WVP constants");
}

bool TestUnsupportedFvfPsOnlyFailsWithoutDraw() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }
  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfUnsupportedXyz);
  if (!Check(hr == S_OK, "SetFVF(unsupported XYZ)")) {
    return false;
  }

  D3D9DDI_HSHADER hPs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStagePs,
                                            kUserPsPassthroughColor,
                                            static_cast<uint32_t>(sizeof(kUserPsPassthroughColor)),
                                            &hPs);
  if (!Check(hr == S_OK, "CreateShader(PS)")) {
    return false;
  }
  if (!Check(hPs.pDrvPrivate != nullptr, "CreateShader(PS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hPs);

  hr = cleanup.device_funcs.pfnSetShader(cleanup.hDevice, kD3d9ShaderStagePs, hPs);
  if (!Check(hr == S_OK, "SetShader(PS)")) {
    return false;
  }

  const size_t baseline_size = dev->cmd.size();

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == D3DERR_INVALIDCALL, "DrawPrimitiveUP(PS-only, unsupported FVF) returns INVALIDCALL")) {
    return false;
  }

  // Draw-time shader binding/validation runs before any UP uploads, so the draw
  // must fail without emitting any draw packets.
  if (!Check(dev->cmd.size() == baseline_size, "no additional commands emitted")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(negative)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 0, "no DRAW opcodes emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW_INDEXED) == 0, "no DRAW_INDEXED opcodes emitted")) {
    return false;
  }

  // The command stream must never contain null shader binds.
  CmdLoc bind = FindLastOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (bind.hdr) {
    const auto* bind_cmd = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(bind.hdr);
    if (!Check(bind_cmd->vs != 0 && bind_cmd->ps != 0, "BIND_SHADERS must not bind null handles")) {
      return false;
    }
  }
  return true;
}

bool TestDrawShaderRestoreSkipsNullSavedShaders() {
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

  D3D9DDI_HSHADER hVs{};
  hr = cleanup.device_funcs.pfnCreateShader(cleanup.hDevice,
                                            kD3d9ShaderStageVs,
                                            kUserVsPassthroughPosColor,
                                            static_cast<uint32_t>(sizeof(kUserVsPassthroughPosColor)),
                                            &hVs);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hVs.pDrvPrivate != nullptr, "CreateShader(VS) returned handle")) {
    return false;
  }
  cleanup.shaders.push_back(hVs);

  // Repro: simulate a caller-visible VS-only state where the internal bound
  // pipeline hasn't been materialized yet (dev->vs/dev->ps are null). The draw
  // path injects an internal fixed-function PS for the draw; restoring the
  // pre-draw state must not emit a BIND_SHADERS packet with null handles.
  hr = device_test_set_unmaterialized_user_shaders(cleanup.hDevice, hVs, D3D9DDI_HSHADER{});
  if (!Check(hr == S_OK, "device_test_set_unmaterialized_user_shaders(VS-only)")) {
    return false;
  }

  const VertexXyzrhwDiffuse tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFF0000u},
      {1.0f, 0.0f, 0.0f, 1.0f, 0xFF00FF00u},
      {0.0f, 1.0f, 0.0f, 1.0f, 0xFF0000FFu},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, 1, tri, sizeof(VertexXyzrhwDiffuse));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(VS-only, null saved pipeline)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(ValidateStream(buf, len), "ValidateStream(draw restore)")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) >= 1, "DRAW emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS) >= 1, "BIND_SHADERS emitted")) {
    return false;
  }
  return CheckNoNullShaderBinds(buf, len);
}

} // namespace aerogpu

int main() {
  if (!aerogpu::TestVsOnlyBindsFixedfuncPs()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyStage0StateUpdatesFixedfuncPs()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyFogEnabledDoesNotSelectFogFixedfuncPs()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyUnsupportedStage0StateSetShaderSucceedsDrawFails()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyUnsupportedStage0ArgStateSetShaderSucceedsDrawFails()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyUnsupportedStage0AlphaOpSetShaderSucceedsDrawFails()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyUnsupportedStage0AlphaArgStateSetShaderSucceedsDrawFails()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyUnsupportedStage1StateSetShaderSucceedsDrawFails()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyUnsupportedStage2StateSetShaderSucceedsDrawFails()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyUnsupportedStage3StateSetShaderSucceedsDrawFails()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyUnsupportedStage0DestroyShaderSucceedsAndRebinds()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyUnsupportedStage0DestroyPixelShaderSucceedsAndRebinds()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyUnsupportedStage0ApplyStateBlockSetShaderSucceedsDrawFails()) {
    return 1;
  }
  if (!aerogpu::TestVsOnlyApplyStateBlockSetsUnsupportedStage0StateSucceedsDrawFails()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyBindsFixedfuncVs()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyBindsFixedfuncVsTex1()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyBindsFixedfuncVsXyzTex1()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyXyzTex1LightingEnabledStillDraws()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyIgnoresUnsupportedStage0State()) {
    return 1;
  }
  if (!aerogpu::TestPsOnlyXyzDiffuseBindsWvpVs()) {
    return 1;
  }
  if (!aerogpu::TestUnsupportedFvfPsOnlyFailsWithoutDraw()) {
    return 1;
  }
  if (!aerogpu::TestDrawShaderRestoreSkipsNullSavedShaders()) {
    return 1;
  }
  if (!aerogpu::TestColorFillDoesNotBindNullShaders()) {
    return 1;
  }
  if (!aerogpu::TestBltDoesNotBindNullShaders()) {
    return 1;
  }
  return 0;
}
