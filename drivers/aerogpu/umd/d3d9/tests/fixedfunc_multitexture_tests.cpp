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
constexpr uint32_t kD3dFvfXyzrhw = 0x00000004u;
constexpr uint32_t kD3dFvfDiffuse = 0x00000040u;
constexpr uint32_t kD3dFvfTex1 = 0x00000100u;
constexpr uint32_t kFvfXyzrhwDiffuseTex1 = kD3dFvfXyzrhw | kD3dFvfDiffuse | kD3dFvfTex1;
 
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
constexpr uint32_t kD3dTopAdd = 7u;
constexpr uint32_t kD3dTopBlendTextureAlpha = 13u;
constexpr uint32_t kD3dTopAddSmooth = 11u;
 
// D3DTA_* sources (from d3d9types.h).
constexpr uint32_t kD3dTaDiffuse = 0u;
constexpr uint32_t kD3dTaCurrent = 1u;
constexpr uint32_t kD3dTaTexture = 2u;
constexpr uint32_t kD3dTaTFactor = 3u;
 
// Pixel shader instruction token (ps_2_0).
constexpr uint32_t kPsOpTexld = 0x04000042u;
// Sampler source register token base (s0 == 0x20E40800). Matches
// `fixedfunc_ps20::src_sampler` in `src/aerogpu_d3d9_driver.cpp`.
constexpr uint32_t kPsSamplerTokenBase = 0x20E40800u;

// D3DERR_INVALIDCALL (from d3d9.h / d3d9types.h). Define it locally so portable
// builds don't require D3D9 headers.
constexpr HRESULT kD3DErrInvalidCall = static_cast<HRESULT>(0x8876086CUL);

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}
 
size_t CountToken(const aerogpu::Shader* shader, uint32_t token) {
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

uint32_t TexldSamplerMask(const aerogpu::Shader* shader) {
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
      if (sampler >= kPsSamplerTokenBase) {
        const uint32_t reg = sampler - kPsSamplerTokenBase;
        if (reg < 16) {
          mask |= (1u << reg);
        }
      }
    }
    i += len;
  }
  return mask;
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
    if (hdr->size_bytes == 0 || (hdr->size_bytes & 3u) != 0 || hdr->size_bytes > stream_len - offset) {
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
  std::vector<D3DDDI_HRESOURCE> resources{};
  bool has_adapter = false;
  bool has_device = false;
 
  ~CleanupDevice() {
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
  if (!Check(cleanup->device_funcs.pfnDrawPrimitiveUP != nullptr, "pfnDrawPrimitiveUP is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnCreateResource != nullptr, "pfnCreateResource is available")) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnSetTexture != nullptr, "pfnSetTexture is available")) {
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
  create_res.type = 3u; // D3DRTYPE_TEXTURE
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
 
struct VertexXyzrhwDiffuseTex1 {
  float x;
  float y;
  float z;
  float rhw;
  uint32_t color;
  float u;
  float v;
};
 
bool TestFixedfuncTwoStageEmitsTwoTexldAndRebinds() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }
 
  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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
 
  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }
 
  // Stage1: CURRENT = tex1 * CURRENT (modulate). This forces stage1 active and
  // requires sampling both stage0 and stage1 textures.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }
 
  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };
 
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(first)")) {
    return false;
  }
 
  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) >= 2, "fixed-function PS contains >= 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "fixed-function PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_before = dev->ps->handle;
  }
  if (!Check(ps_before != 0, "first draw bound non-zero PS handle")) {
    return false;
  }
 
  // Change stage1 state to force a different shader variant.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopAdd);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=ADD")) {
    return false;
  }
  // Ensure stage2 is explicitly disabled so the stage chain ends deterministically.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=DISABLE")) {
    return false;
  }
 
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(second)")) {
    return false;
  }
 
  aerogpu_handle_t ps_after = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound after stage1 change")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) >= 2, "second fixed-function PS contains >= 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "second fixed-function PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_after = dev->ps->handle;
  }
  if (!Check(ps_after != 0, "second draw bound non-zero PS handle")) {
    return false;
  }
  if (!Check(ps_before != ps_after, "stage1 state change causes PS handle change")) {
    return false;
  }
 
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
 
  // Validate that both textures were bound.
  bool saw_tex0 = false;
  bool saw_tex1 = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage != AEROGPU_SHADER_STAGE_PIXEL) {
      continue;
    }
    if (st->slot == 0 && st->texture != 0) {
      saw_tex0 = true;
    }
    if (st->slot == 1 && st->texture != 0) {
      saw_tex1 = true;
    }
  }
  if (!Check(saw_tex0, "command stream binds texture slot 0")) {
    return false;
  }
  if (!Check(saw_tex1, "command stream binds texture slot 1")) {
    return false;
  }
 
  // Validate shader binds.
  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  const auto* last_bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(binds.back());
  if (!Check(last_bind->vs != 0 && last_bind->ps != 0, "BIND_SHADERS binds non-zero VS/PS")) {
    return false;
  }
 
  bool saw_ps_before = false;
  bool saw_ps_after = false;
  for (const auto* hdr : binds) {
    const auto* b = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
    if (b->ps == ps_before) {
      saw_ps_before = true;
    }
    if (b->ps == ps_after) {
      saw_ps_after = true;
    }
  }
  if (!Check(saw_ps_before, "command stream contains a bind for the first PS")) {
    return false;
  }
  if (!Check(saw_ps_after, "command stream contains a bind for the updated PS")) {
    return false;
  }
 
  return true;
}

bool TestFixedfuncUnboundStage1TextureTruncatesChainAndDoesNotRebind() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1 requests texturing, but does not have a texture bound. The driver
  // should defensively truncate the stage chain rather than emitting a shader
  // that samples an unbound slot.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  // First draw selects a stage0-only fixed-function PS (tex0 is bound, tex1 is not).
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(first, stage1 texture missing)")) {
    return false;
  }

  const aerogpu::Shader* ps_ptr_before = nullptr;
  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 1, "fixed-function PS contains exactly 1 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x1u, "fixed-function PS texld uses only sampler s0")) {
      return false;
    }
    ps_ptr_before = dev->ps;
    ps_before = dev->ps->handle;
  }
  if (!Check(ps_before != 0, "first draw bound non-zero PS handle")) {
    return false;
  }

  // Change stage1 state. Because stage1 is ignored (texture unbound), this must
  // not create/bind a different PS variant.
  dev->cmd.reset();
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopAdd);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=ADD")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(second, stage1 texture missing)")) {
    return false;
  }

  const aerogpu::Shader* ps_ptr_after = nullptr;
  aerogpu_handle_t ps_after = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound after stage1 change")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 1, "second PS contains exactly 1 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x1u, "second PS texld uses only sampler s0")) {
      return false;
    }
    ps_ptr_after = dev->ps;
    ps_after = dev->ps->handle;
  }
  if (!Check(ps_after == ps_before, "stage1 state change (missing texture) keeps PS handle stable")) {
    return false;
  }
  if (!Check(ps_ptr_after == ps_ptr_before, "stage1 state change (missing texture) keeps PS pointer stable")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  // The second draw should not create a new pixel shader.
  size_t ps_creates = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
    const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
    if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
      ++ps_creates;
    }
  }
  if (!Check(ps_creates == 0, "second draw emits no CREATE_SHADER_DXBC for PS")) {
    return false;
  }

  // And it should not bind any non-null stage1 texture.
  bool saw_tex1 = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 1 && st->texture != 0) {
      saw_tex1 = true;
    }
  }
  if (!Check(!saw_tex1, "command stream does not bind a stage1 texture when unbound")) {
    return false;
  }

  return true;
}

bool TestFixedfuncUnboundStage1TextureDoesNotTruncateWhenStage1DoesNotSample() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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
  D3DDDI_HRESOURCE hTex2{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }
  // Stage1 intentionally left unbound.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1 does not sample its texture, so leaving it unbound must not truncate
  // the chain: stage2 should still execute.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage2 samples texture2.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Ensure stage3 is explicitly disabled so the chain ends deterministically.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 missing but stage1 doesn't sample)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "stage1 doesn't sample => PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x5u, "stage1 doesn't sample => PS texld uses samplers s0 and s2")) {
      return false;
    }
  }

  const aerogpu::Shader* ps_ptr_before = nullptr;
  aerogpu_handle_t ps_before = 0;
  size_t cache_size_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_ptr_before = dev->ps;
    ps_before = dev->ps ? dev->ps->handle : 0;
    cache_size_before = dev->fixedfunc_ps_variant_cache.size();
  }
  if (!Check(ps_before != 0, "draw bound non-zero PS handle")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  bool saw_tex0 = false;
  bool saw_tex2 = false;
  bool saw_tex1_non_null = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage != AEROGPU_SHADER_STAGE_PIXEL) {
      continue;
    }
    if (st->slot == 0 && st->texture != 0) {
      saw_tex0 = true;
    }
    if (st->slot == 2 && st->texture != 0) {
      saw_tex2 = true;
    }
    if (st->slot == 1 && st->texture != 0) {
      saw_tex1_non_null = true;
    }
  }
  if (!Check(saw_tex0, "command stream binds texture slot 0")) {
    return false;
  }
  if (!Check(saw_tex2, "command stream binds texture slot 2")) {
    return false;
  }
  if (!Check(!saw_tex1_non_null, "command stream does not bind texture slot 1 when stage1 texture is unbound")) {
    return false;
  }

  // Binding/unbinding stage1's texture should not affect fixed-function PS
  // selection when stage1 does not sample it.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1=bind, stage1 unused)")) {
    return false;
  }

  dev->cmd.finalize();
  buf = dev->cmd.data();
  const size_t len2 = dev->cmd.bytes_used();
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage1=bind, unused) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_BIND_SHADERS).empty(),
             "SetTexture(stage1=bind, unused) emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage1=bind, unused) emits no DRAW")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound after stage1 bind")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "unused stage1 bind keeps PS pointer stable")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "unused stage1 bind keeps PS handle stable")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "unused stage1 bind => PS still contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x5u, "unused stage1 bind => PS still uses samplers s0 and s2")) {
      return false;
    }
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "unused stage1 bind does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  dev->cmd.reset();
  D3DDDI_HRESOURCE null_tex{};
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, null_tex);
  if (!Check(hr == S_OK, "SetTexture(stage1=null, stage1 unused)")) {
    return false;
  }

  dev->cmd.finalize();
  buf = dev->cmd.data();
  const size_t len3 = dev->cmd.bytes_used();
  if (!Check(CollectOpcodes(buf, len3, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage1=null, unused) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len3, AEROGPU_CMD_BIND_SHADERS).empty(),
             "SetTexture(stage1=null, unused) emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len3, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage1=null, unused) emits no DRAW")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound after stage1 unbind")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "unused stage1 unbind keeps PS pointer stable")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "unused stage1 unbind keeps PS handle stable")) {
      return false;
    }
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "unused stage1 unbind does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncUnboundStage1TextureTruncatesWhenStage1UsesTextureInAlphaOnly() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Ensure stage1/2/3 are disabled initially so we can capture the baseline
  // stage0-only PS handle.
  for (uint32_t stage = 1; stage <= 3; ++stage) {
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorOp, kD3dTopDisable);
    if (!Check(hr == S_OK, "TSS stageN COLOROP=DISABLE")) {
      return false;
    }
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage0 baseline)")) {
    return false;
  }

  const aerogpu::Shader* ps_ptr_stage0 = nullptr;
  aerogpu_handle_t ps_stage0 = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 1, "baseline => PS contains exactly 1 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x1u, "baseline => PS texld uses sampler s0")) {
      return false;
    }
    ps_ptr_stage0 = dev->ps;
    ps_stage0 = dev->ps->handle;
  }
  if (!Check(ps_stage0 != 0, "baseline bound non-zero PS handle")) {
    return false;
  }

  // Configure stage1 so it would sample texture1 only in the alpha path:
  // COLOR = CURRENT, ALPHA = TEXTURE.
  //
  // Set ALPHAOP/ALPHAARG1 first while stage1 COLOROP is still DISABLE to avoid
  // creating intermediate PS variants during state setup.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=TEXTURE (alpha-only sampling)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=CURRENT")) {
    return false;
  }

  // Now enable stage1. Since stage1's texture is unbound but stage1 uses TEXTURE
  // in the alpha path, this must still truncate the chain back to stage0-only.
  dev->cmd.reset();
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=SELECTARG1")) {
    return false;
  }

  const aerogpu::Shader* ps_ptr_after = nullptr;
  aerogpu_handle_t ps_after = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 1, "stage1 alpha-only missing => PS contains exactly 1 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x1u, "stage1 alpha-only missing => PS texld uses sampler s0")) {
      return false;
    }
    ps_ptr_after = dev->ps;
    ps_after = dev->ps->handle;
  }
  if (!Check(ps_ptr_after == ps_ptr_stage0, "stage1 alpha-only missing => PS pointer matches stage0 baseline")) {
    return false;
  }
  if (!Check(ps_after == ps_stage0, "stage1 alpha-only missing => PS handle matches stage0 baseline")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  size_t ps_creates = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
    const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
    if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
      ++ps_creates;
    }
  }
  if (!Check(ps_creates == 0, "stage1 alpha-only missing => stage1 enable emits no CREATE_SHADER_DXBC")) {
    return false;
  }

  return true;
}

bool TestFixedfuncBindUnbindStage1TextureRebindsPixelShader() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1 requests texturing, but starts out with texture1 unbound.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  // Draw once with stage1 missing => stage0-only PS.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 missing)")) {
    return false;
  }

  aerogpu_handle_t ps_stage0 = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound (stage1 missing)")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 1, "stage1 missing => PS contains exactly 1 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x1u, "stage1 missing => PS texld uses only sampler s0")) {
      return false;
    }
    ps_stage0 = dev->ps->handle;
  }
  if (!Check(ps_stage0 != 0, "stage1 missing => bound non-zero PS handle")) {
    return false;
  }

  // Bind texture1. This should eagerly select a new PS variant that samples s1.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1=bind)")) {
    return false;
  }

  aerogpu_handle_t ps_stage1 = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound after stage1 bind")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) >= 2, "stage1 bind => PS contains >= 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "stage1 bind => PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_stage1 = dev->ps->handle;
  }
  if (!Check(ps_stage1 != 0, "stage1 bind => bound non-zero PS handle")) {
    return false;
  }
  if (!Check(ps_stage1 != ps_stage0, "stage1 bind => PS handle changed")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  bool saw_tex1_bind = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 1 && st->texture != 0) {
      saw_tex1_bind = true;
    }
  }
  if (!Check(saw_tex1_bind, "SetTexture(stage1=bind) emits non-null texture bind")) {
    return false;
  }

  bool saw_ps_create = false;
  bool saw_vs_create = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
    const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
    if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
      saw_ps_create = true;
    } else if (cs->stage == AEROGPU_SHADER_STAGE_VERTEX) {
      saw_vs_create = true;
    }
  }
  if (!Check(saw_ps_create, "SetTexture(stage1=bind) emits CREATE_SHADER_DXBC for PS")) {
    return false;
  }
  if (!Check(!saw_vs_create, "SetTexture(stage1=bind) does not emit CREATE_SHADER_DXBC for VS")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  bool saw_ps_bind = false;
  for (const auto* hdr : binds) {
    const auto* b = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
    if (b->ps == ps_stage1 && b->vs != 0) {
      saw_ps_bind = true;
      break;
    }
  }
  if (!Check(saw_ps_bind, "SetTexture(stage1=bind) emits BIND_SHADERS for the updated PS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage1=bind) emits no DRAW commands")) {
    return false;
  }

  // Unbind stage1 texture again; should revert to the stage0-only PS and should
  // not need to create a new shader.
  dev->cmd.reset();
  D3DDDI_HRESOURCE null_tex{};
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, null_tex);
  if (!Check(hr == S_OK, "SetTexture(stage1=null)")) {
    return false;
  }

  aerogpu_handle_t ps_stage0_again = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound after stage1 unbind")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 1, "stage1 unbind => PS contains exactly 1 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x1u, "stage1 unbind => PS texld uses only sampler s0")) {
      return false;
    }
    ps_stage0_again = dev->ps->handle;
  }
  if (!Check(ps_stage0_again == ps_stage0, "stage1 unbind => PS handle restored to stage0-only")) {
    return false;
  }

  dev->cmd.finalize();
  buf = dev->cmd.data();
  const size_t len2 = dev->cmd.bytes_used();

  bool saw_tex1_unbind = false;
  for (const auto* hdr : CollectOpcodes(buf, len2, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 1 && st->texture == 0) {
      saw_tex1_unbind = true;
    }
  }
  if (!Check(saw_tex1_unbind, "SetTexture(stage1=null) emits null texture bind")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage1=null) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage1=null) emits no DRAW commands")) {
    return false;
  }

  return true;
}

bool TestFixedfuncSwitchStage1TextureDoesNotRebindPixelShader() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  D3DDDI_HRESOURCE hTex1a{};
  D3DDDI_HRESOURCE hTex1b{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1a) ||
      !CreateDummyTexture(&cleanup, &hTex1b)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1a);
  if (!Check(hr == S_OK, "SetTexture(stage1=texA)")) {
    return false;
  }

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1 samples its texture.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }
  // Terminate the stage chain.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  // Draw once to ensure the stage0+stage1 PS is created and bound.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 texA)")) {
    return false;
  }

  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "baseline => PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "baseline => PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_before = dev->ps->handle;
  }

  // Switching stage1 textures (non-null to non-null) must not change the PS
  // variant (only the bound sampler resource).
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1b);
  if (!Check(hr == S_OK, "SetTexture(stage1=texB)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "SetTexture(stage1=texB) keeps PS handle stable")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "texB => PS still contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "texB => PS still uses samplers s0 and s1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  bool saw_tex1_bind = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 1 && st->texture != 0) {
      saw_tex1_bind = true;
      break;
    }
  }
  if (!Check(saw_tex1_bind, "SetTexture(stage1=texB) emits non-null texture bind")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage1=texB) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "SetTexture(stage1=texB) emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage1=texB) emits no DRAW")) {
    return false;
  }

  return true;
}

bool TestFixedfuncSwitchStage0TextureDoesNotRebindPixelShader() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0a{};
  D3DDDI_HRESOURCE hTex0b{};
  D3DDDI_HRESOURCE hTex1{};
  if (!CreateDummyTexture(&cleanup, &hTex0a) ||
      !CreateDummyTexture(&cleanup, &hTex0b) ||
      !CreateDummyTexture(&cleanup, &hTex1)) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0a);
  if (!Check(hr == S_OK, "SetTexture(stage0=texA)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1)")) {
    return false;
  }

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1 samples its texture.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }
  // Terminate the stage chain.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  // Draw once to ensure the stage0+stage1 PS is created and bound.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage0 texA)")) {
    return false;
  }

  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "baseline => PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "baseline => PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_before = dev->ps->handle;
  }

  // Switching stage0 textures (non-null to non-null) must not change the PS
  // variant (only the bound sampler resource).
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0b);
  if (!Check(hr == S_OK, "SetTexture(stage0=texB)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "SetTexture(stage0=texB) keeps PS handle stable")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "texB => PS still contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "texB => PS still uses samplers s0 and s1")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  bool saw_tex0_bind = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 0 && st->texture != 0) {
      saw_tex0_bind = true;
      break;
    }
  }
  if (!Check(saw_tex0_bind, "SetTexture(stage0=texB) emits non-null texture bind")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage0=texB) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "SetTexture(stage0=texB) emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage0=texB) emits no DRAW")) {
    return false;
  }

  return true;
}

bool TestFixedfuncSwitchStage2TextureDoesNotRebindPixelShader() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  D3DDDI_HRESOURCE hTex1{};
  D3DDDI_HRESOURCE hTex2a{};
  D3DDDI_HRESOURCE hTex2b{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2a) ||
      !CreateDummyTexture(&cleanup, &hTex2b)) {
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
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2a);
  if (!Check(hr == S_OK, "SetTexture(stage2=texA)")) {
    return false;
  }

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: CURRENT = tex1 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage2: CURRENT = tex2 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Terminate the stage chain.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  // Draw once to ensure the stage0+stage1+stage2 PS is created and bound.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage2 texA)")) {
    return false;
  }

  aerogpu_handle_t ps_before = 0;
  size_t cache_size_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 3, "baseline => PS contains exactly 3 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x7u, "baseline => PS texld uses samplers s0, s1, s2")) {
      return false;
    }
    ps_before = dev->ps->handle;
    cache_size_before = dev->fixedfunc_ps_variant_cache.size();
  }

  // Switching stage2 textures (non-null to non-null) must not change the PS
  // variant (only the bound sampler resource).
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2b);
  if (!Check(hr == S_OK, "SetTexture(stage2=texB)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "SetTexture(stage2=texB) keeps PS handle stable")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 3, "texB => PS still contains exactly 3 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x7u, "texB => PS still uses samplers s0, s1, s2")) {
      return false;
    }
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "SetTexture(stage2=texB) does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  bool saw_tex2_bind = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 2 && st->texture != 0) {
      saw_tex2_bind = true;
      break;
    }
  }
  if (!Check(saw_tex2_bind, "SetTexture(stage2=texB) emits non-null texture bind")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage2=texB) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "SetTexture(stage2=texB) emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage2=texB) emits no DRAW")) {
    return false;
  }

  return true;
}

bool TestFixedfuncBindUnbindStage2TextureRebindsPixelShader() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  D3DDDI_HRESOURCE hTex1{};
  D3DDDI_HRESOURCE hTex2{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2)) {
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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: CURRENT = tex1 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage2 requests texturing, but starts out with texture2 unbound.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAARG1=CURRENT")) {
    return false;
  }
  // Ensure stage3 is explicitly disabled so the stage chain ends deterministically.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  // Draw once with stage2 missing => stage0+stage1 PS.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage2 missing)")) {
    return false;
  }

  aerogpu_handle_t ps_stage1 = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound (stage2 missing)")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "stage2 missing => PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "stage2 missing => PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_stage1 = dev->ps->handle;
  }
  if (!Check(ps_stage1 != 0, "stage2 missing => bound non-zero PS handle")) {
    return false;
  }

  // Bind texture2. This should eagerly select a new PS variant that samples s2.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2=bind)")) {
    return false;
  }

  aerogpu_handle_t ps_stage2 = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound after stage2 bind")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 3, "stage2 bind => PS contains exactly 3 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x7u, "stage2 bind => PS texld uses samplers s0, s1, s2")) {
      return false;
    }
    ps_stage2 = dev->ps->handle;
  }
  if (!Check(ps_stage2 != 0, "stage2 bind => bound non-zero PS handle")) {
    return false;
  }
  if (!Check(ps_stage2 != ps_stage1, "stage2 bind => PS handle changed")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  bool saw_tex2_bind = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 2 && st->texture != 0) {
      saw_tex2_bind = true;
    }
  }
  if (!Check(saw_tex2_bind, "SetTexture(stage2=bind) emits non-null texture bind")) {
    return false;
  }

  bool saw_ps_create = false;
  bool saw_vs_create = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
    const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
    if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
      saw_ps_create = true;
    } else if (cs->stage == AEROGPU_SHADER_STAGE_VERTEX) {
      saw_vs_create = true;
    }
  }
  if (!Check(saw_ps_create, "SetTexture(stage2=bind) emits CREATE_SHADER_DXBC for PS")) {
    return false;
  }
  if (!Check(!saw_vs_create, "SetTexture(stage2=bind) does not emit CREATE_SHADER_DXBC for VS")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  bool saw_ps_bind = false;
  for (const auto* hdr : binds) {
    const auto* b = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
    if (b->ps == ps_stage2 && b->vs != 0) {
      saw_ps_bind = true;
      break;
    }
  }
  if (!Check(saw_ps_bind, "SetTexture(stage2=bind) emits BIND_SHADERS for the updated PS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage2=bind) emits no DRAW commands")) {
    return false;
  }

  // Unbind stage2 texture again; should revert to the stage0+stage1 PS and should
  // not need to create a new shader.
  dev->cmd.reset();
  D3DDDI_HRESOURCE null_tex{};
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, null_tex);
  if (!Check(hr == S_OK, "SetTexture(stage2=null)")) {
    return false;
  }

  aerogpu_handle_t ps_stage1_again = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound after stage2 unbind")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "stage2 unbind => PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "stage2 unbind => PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_stage1_again = dev->ps->handle;
  }
  if (!Check(ps_stage1_again == ps_stage1, "stage2 unbind => PS handle restored to stage0+stage1")) {
    return false;
  }

  dev->cmd.finalize();
  buf = dev->cmd.data();
  const size_t len2 = dev->cmd.bytes_used();

  bool saw_tex2_unbind = false;
  for (const auto* hdr : CollectOpcodes(buf, len2, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 2 && st->texture == 0) {
      saw_tex2_unbind = true;
    }
  }
  if (!Check(saw_tex2_unbind, "SetTexture(stage2=null) emits null texture bind")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage2=null) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  const auto binds2 = CollectOpcodes(buf, len2, AEROGPU_CMD_BIND_SHADERS);
  bool saw_ps_unbind = false;
  for (const auto* hdr : binds2) {
    const auto* b = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
    if (b->ps == ps_stage1 && b->vs != 0) {
      saw_ps_unbind = true;
      break;
    }
  }
  if (!Check(saw_ps_unbind, "SetTexture(stage2=null) emits BIND_SHADERS for restored PS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage2=null) emits no DRAW commands")) {
    return false;
  }

  return true;
}

bool TestFixedfuncUnboundStage2TextureTruncatesBeforeStage3() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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
  D3DDDI_HRESOURCE hTex3{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex3)) {
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
  // Stage2 intentionally left unbound.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3);
  if (!Check(hr == S_OK, "SetTexture(stage3)")) {
    return false;
  }

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: CURRENT = tex1 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage2 requests texturing, but stage2 texture is unbound. The driver should
  // truncate the chain and ignore stage3.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage3: would be active if not for stage2 truncation. Use an unsupported op
  // to ensure later stage state does not affect draw validation when the chain
  // is truncated.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopAddSmooth);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=ADDSMOOTH (unsupported, should be ignored)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAARG1=CURRENT")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage2 missing texture)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "stage2 missing => PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "stage2 missing => PS texld uses samplers s0 and s1")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncUnboundStage2TextureTruncatesWhenStage2UsesBlendTextureAlpha() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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
  D3DDDI_HRESOURCE hTex3{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex3)) {
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
  // Stage2 intentionally left unbound.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3);
  if (!Check(hr == S_OK, "SetTexture(stage3)")) {
    return false;
  }

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: CURRENT = tex1 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage2 uses BLENDTEXTUREALPHA, which consumes texture alpha as the blend
  // factor even if neither arg source is TEXTURE. With stage2 texture unbound,
  // the driver must still truncate the chain to avoid sampling slot 2.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopBlendTextureAlpha);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=BLENDTEXTUREALPHA")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG1=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage3: would be active if not for stage2 truncation.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAARG1=CURRENT")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage2 blendtexturealpha missing texture)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2,
               "stage2 BLENDTEXTUREALPHA missing => PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u,
               "stage2 BLENDTEXTUREALPHA missing => PS texld uses samplers s0 and s1")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncUnboundStage2TextureTruncatesWhenStage2UsesBlendTextureAlphaInAlphaOnly() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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
  D3DDDI_HRESOURCE hTex3{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex3)) {
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
  // Stage2 intentionally left unbound.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3);
  if (!Check(hr == S_OK, "SetTexture(stage3)")) {
    return false;
  }

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: CURRENT = tex1 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage2 uses BLENDTEXTUREALPHA in the alpha combiner only. This consumes texture alpha as the blend factor
  // regardless of arg sources. With stage2 texture unbound, the driver must still truncate the chain to avoid
  // sampling slot 2.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG1=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaOp, kD3dTopBlendTextureAlpha);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAOP=BLENDTEXTUREALPHA")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAARG1=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAARG2=CURRENT")) {
    return false;
  }

  // Stage3: would be active if not for stage2 truncation.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAARG1=CURRENT")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage2 alpha blendtexturealpha missing texture)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2,
               "stage2 ALPHAOP=BLENDTEXTUREALPHA missing => PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u,
               "stage2 ALPHAOP=BLENDTEXTUREALPHA missing => PS texld uses samplers s0 and s1")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncUnboundStage3TextureTruncatesChainAndDoesNotRebind() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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
  D3DDDI_HRESOURCE hTex2{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2)) {
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
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }
  // Stage3 intentionally left unbound.

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: CURRENT = tex1 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage2: CURRENT = tex2 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage3 requests texturing, but stage3 texture is unbound. The driver should
  // truncate the chain and ignore stage3 state.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAARG1=CURRENT")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage3 missing texture)")) {
    return false;
  }

  const aerogpu::Shader* ps_ptr_before = nullptr;
  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 3, "stage3 missing => PS contains exactly 3 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x7u, "stage3 missing => PS texld uses samplers s0, s1, s2")) {
      return false;
    }
    ps_ptr_before = dev->ps;
    ps_before = dev->ps->handle;
  }
  if (!Check(ps_before != 0, "draw bound non-zero PS handle")) {
    return false;
  }

  // Change stage3 state. Because stage3 is ignored (texture unbound), this must
  // not create/bind a different PS variant.
  dev->cmd.reset();
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopAdd);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=ADD")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  size_t ps_creates = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
    const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
    if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
      ++ps_creates;
    }
  }
  if (!Check(ps_creates == 0, "stage3 state change emits no CREATE_SHADER_DXBC for PS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(), "stage3 state change emits no BIND_SHADERS")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "stage3 state change keeps PS pointer stable")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "stage3 state change keeps PS handle stable")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 3, "PS still contains exactly 3 texld after stage3 change")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x7u, "PS still uses samplers s0, s1, s2 after stage3 change")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncBindUnbindStage3TextureRebindsPixelShader() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  D3DDDI_HRESOURCE hTex1{};
  D3DDDI_HRESOURCE hTex2{};
  D3DDDI_HRESOURCE hTex3{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2) ||
      !CreateDummyTexture(&cleanup, &hTex3)) {
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
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }
  // Stage3 intentionally left unbound.

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: CURRENT = tex1 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage2: CURRENT = tex2 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage3 requests texturing, but starts out with texture3 unbound.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAARG1=CURRENT")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  // Draw once with stage3 missing => stage0+stage1+stage2 PS.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage3 missing)")) {
    return false;
  }

  aerogpu_handle_t ps_stage2 = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound (stage3 missing)")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 3, "stage3 missing => PS contains exactly 3 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x7u, "stage3 missing => PS texld uses samplers s0, s1, s2")) {
      return false;
    }
    ps_stage2 = dev->ps->handle;
  }
  if (!Check(ps_stage2 != 0, "stage3 missing => bound non-zero PS handle")) {
    return false;
  }

  // Bind texture3. This should eagerly select a new PS variant that samples s3.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3);
  if (!Check(hr == S_OK, "SetTexture(stage3=bind)")) {
    return false;
  }

  aerogpu_handle_t ps_stage3 = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound after stage3 bind")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 4, "stage3 bind => PS contains exactly 4 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0xFu, "stage3 bind => PS texld uses samplers s0..s3")) {
      return false;
    }
    ps_stage3 = dev->ps->handle;
  }
  if (!Check(ps_stage3 != 0, "stage3 bind => bound non-zero PS handle")) {
    return false;
  }
  if (!Check(ps_stage3 != ps_stage2, "stage3 bind => PS handle changed")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  bool saw_tex3_bind = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 3 && st->texture != 0) {
      saw_tex3_bind = true;
    }
  }
  if (!Check(saw_tex3_bind, "SetTexture(stage3=bind) emits non-null texture bind")) {
    return false;
  }

  bool saw_ps_create = false;
  bool saw_vs_create = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
    const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
    if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
      saw_ps_create = true;
    } else if (cs->stage == AEROGPU_SHADER_STAGE_VERTEX) {
      saw_vs_create = true;
    }
  }
  if (!Check(saw_ps_create, "SetTexture(stage3=bind) emits CREATE_SHADER_DXBC for PS")) {
    return false;
  }
  if (!Check(!saw_vs_create, "SetTexture(stage3=bind) does not emit CREATE_SHADER_DXBC for VS")) {
    return false;
  }

  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  bool saw_ps_bind = false;
  for (const auto* hdr : binds) {
    const auto* b = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
    if (b->ps == ps_stage3 && b->vs != 0) {
      saw_ps_bind = true;
      break;
    }
  }
  if (!Check(saw_ps_bind, "SetTexture(stage3=bind) emits BIND_SHADERS for the updated PS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage3=bind) emits no DRAW commands")) {
    return false;
  }

  // Unbind stage3 texture again; should revert to the stage0+stage1+stage2 PS and
  // should not need to create a new shader.
  dev->cmd.reset();
  D3DDDI_HRESOURCE null_tex{};
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, null_tex);
  if (!Check(hr == S_OK, "SetTexture(stage3=null)")) {
    return false;
  }

  aerogpu_handle_t ps_stage2_again = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound after stage3 unbind")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 3, "stage3 unbind => PS contains exactly 3 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x7u, "stage3 unbind => PS texld uses samplers s0, s1, s2")) {
      return false;
    }
    ps_stage2_again = dev->ps->handle;
  }
  if (!Check(ps_stage2_again == ps_stage2, "stage3 unbind => PS handle restored to stage0+stage1+stage2")) {
    return false;
  }

  dev->cmd.finalize();
  buf = dev->cmd.data();
  const size_t len2 = dev->cmd.bytes_used();

  bool saw_tex3_unbind = false;
  for (const auto* hdr : CollectOpcodes(buf, len2, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 3 && st->texture == 0) {
      saw_tex3_unbind = true;
    }
  }
  if (!Check(saw_tex3_unbind, "SetTexture(stage3=null) emits null texture bind")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage3=null) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  const auto binds2 = CollectOpcodes(buf, len2, AEROGPU_CMD_BIND_SHADERS);
  bool saw_ps_unbind = false;
  for (const auto* hdr : binds2) {
    const auto* b = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
    if (b->ps == ps_stage2 && b->vs != 0) {
      saw_ps_unbind = true;
      break;
    }
  }
  if (!Check(saw_ps_unbind, "SetTexture(stage3=null) emits BIND_SHADERS for restored PS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage3=null) emits no DRAW commands")) {
    return false;
  }

  return true;
}

bool TestFixedfuncSwitchStage3TextureDoesNotRebindPixelShader() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  D3DDDI_HRESOURCE hTex1{};
  D3DDDI_HRESOURCE hTex2{};
  D3DDDI_HRESOURCE hTex3a{};
  D3DDDI_HRESOURCE hTex3b{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2) ||
      !CreateDummyTexture(&cleanup, &hTex3a) ||
      !CreateDummyTexture(&cleanup, &hTex3b)) {
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
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3a);
  if (!Check(hr == S_OK, "SetTexture(stage3=texA)")) {
    return false;
  }

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1-3: CURRENT = texN * CURRENT. Keep alpha as passthrough CURRENT.
  for (uint32_t stage = 1; stage <= 3; ++stage) {
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorOp, kD3dTopModulate);
    if (!Check(hr == S_OK, "TSS stageN COLOROP=MODULATE")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorArg1, kD3dTaTexture);
    if (!Check(hr == S_OK, "TSS stageN COLORARG1=TEXTURE")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorArg2, kD3dTaCurrent);
    if (!Check(hr == S_OK, "TSS stageN COLORARG2=CURRENT")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssAlphaOp, kD3dTopSelectArg1);
    if (!Check(hr == S_OK, "TSS stageN ALPHAOP=SELECTARG1")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssAlphaArg1, kD3dTaCurrent);
    if (!Check(hr == S_OK, "TSS stageN ALPHAARG1=CURRENT")) {
      return false;
    }
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  // Draw once to ensure the stage0+stage1+stage2+stage3 PS is created and bound.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage3 texA)")) {
    return false;
  }

  aerogpu_handle_t ps_before = 0;
  size_t cache_size_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 4, "baseline => PS contains exactly 4 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0xFu, "baseline => PS texld uses samplers s0..s3")) {
      return false;
    }
    ps_before = dev->ps->handle;
    cache_size_before = dev->fixedfunc_ps_variant_cache.size();
  }

  // Switching stage3 textures (non-null to non-null) must not change the PS
  // variant (only the bound sampler resource).
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3b);
  if (!Check(hr == S_OK, "SetTexture(stage3=texB)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "SetTexture(stage3=texB) keeps PS handle stable")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 4, "texB => PS still contains exactly 4 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0xFu, "texB => PS still uses samplers s0..s3")) {
      return false;
    }
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "SetTexture(stage3=texB) does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  bool saw_tex3_bind = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && st->slot == 3 && st->texture != 0) {
      saw_tex3_bind = true;
      break;
    }
  }
  if (!Check(saw_tex3_bind, "SetTexture(stage3=texB) emits non-null texture bind")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage3=texB) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "SetTexture(stage3=texB) emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage3=texB) emits no DRAW")) {
    return false;
  }

  return true;
}

bool TestFixedfuncUnboundStage3TextureDoesNotTruncateWhenStage3DoesNotSample() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  D3DDDI_HRESOURCE hTex1{};
  D3DDDI_HRESOURCE hTex2{};
  D3DDDI_HRESOURCE hTex3{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2) ||
      !CreateDummyTexture(&cleanup, &hTex3)) {
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
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }
  // Stage3 intentionally left unbound, but stage3 will not sample it.

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: CURRENT = tex1 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage2: CURRENT = tex2 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage3 does not sample its texture: CURRENT = CURRENT. Even though stage3 is
  // active, its texture binding/unbinding must not affect fixed-function PS
  // selection.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG1=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAARG1=CURRENT")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage3 missing but stage3 doesn't sample)")) {
    return false;
  }

  const aerogpu::Shader* ps_ptr_before = nullptr;
  aerogpu_handle_t ps_before = 0;
  size_t cache_size_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 3, "stage3 doesn't sample => PS contains exactly 3 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x7u, "stage3 doesn't sample => PS texld uses samplers s0..s2")) {
      return false;
    }
    ps_ptr_before = dev->ps;
    ps_before = dev->ps->handle;
    cache_size_before = dev->fixedfunc_ps_variant_cache.size();
  }
  if (!Check(ps_before != 0, "draw bound non-zero PS handle")) {
    return false;
  }

  // Bind a stage3 texture. This must not create/rebind PS variants since stage3
  // does not sample it.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3);
  if (!Check(hr == S_OK, "SetTexture(stage3=bind, unused)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage3=bind, unused) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "SetTexture(stage3=bind, unused) emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage3=bind, unused) emits no DRAW")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound after stage3 bind")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "unused stage3 bind keeps PS pointer stable")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "unused stage3 bind keeps PS handle stable")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 3, "unused stage3 bind => PS still contains exactly 3 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x7u, "unused stage3 bind => PS still uses samplers s0..s2")) {
      return false;
    }
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "unused stage3 bind does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  // Unbind stage3 texture again; should also not create/rebind PS variants.
  dev->cmd.reset();
  D3DDDI_HRESOURCE null_tex{};
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, null_tex);
  if (!Check(hr == S_OK, "SetTexture(stage3=null, unused)")) {
    return false;
  }

  dev->cmd.finalize();
  buf = dev->cmd.data();
  const size_t len2 = dev->cmd.bytes_used();
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage3=null, unused) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_BIND_SHADERS).empty(),
             "SetTexture(stage3=null, unused) emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage3=null, unused) emits no DRAW")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound after stage3 unbind")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "unused stage3 unbind keeps PS pointer stable")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "unused stage3 unbind keeps PS handle stable")) {
      return false;
    }
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "unused stage3 unbind does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncIgnoresUnusedColorArg2ForSelectArg1() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: select tex1. COLORARG2 is intentionally set to an invalid value and
  // must be ignored (SELECTARG1 only consumes ARG1).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  constexpr uint32_t kInvalidArg = 0x80000000u;
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kInvalidArg);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=invalid (ignored)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(selectarg1 ignores arg2)")) {
    return false;
  }

  const aerogpu::Shader* ps_ptr_before = nullptr;
  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_ptr_before = dev->ps;
    ps_before = dev->ps->handle;
  }

  // Changing the unused arg2 should not create/bind a new shader variant.
  dev->cmd.reset();
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, 0x40000000u);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=invalid2 (ignored)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "unused arg2 change emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(), "unused arg2 change emits no BIND_SHADERS")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "unused arg2 change keeps PS handle stable")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "unused arg2 change keeps PS pointer stable")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncIgnoresUnusedColorArg1ForSelectArg2() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: select tex1 via ARG2. COLORARG1 is intentionally invalid and must be
  // ignored (SELECTARG2 only consumes ARG2).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopSelectArg2);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=SELECTARG2")) {
    return false;
  }
  constexpr uint32_t kInvalidArg = 0x80000000u;
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kInvalidArg);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=invalid (ignored)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(selectarg2 ignores arg1)")) {
    return false;
  }

  const aerogpu::Shader* ps_ptr_before = nullptr;
  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_ptr_before = dev->ps;
    ps_before = dev->ps->handle;
  }

  // Changing the unused arg1 should not create/bind a new shader variant.
  dev->cmd.reset();
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, 0x40000000u);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=invalid2 (ignored)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "unused arg1 change emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(), "unused arg1 change emits no BIND_SHADERS")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "unused arg1 change keeps PS handle stable")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "unused arg1 change keeps PS pointer stable")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncIgnoresUnusedAlphaArg2ForSelectArg1() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: select tex1. ALPHAARG2 is intentionally set to an invalid value and
  // must be ignored (SELECTARG1 only consumes ARG1).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }
  constexpr uint32_t kInvalidArg = 0x80000000u;
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg2, kInvalidArg);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG2=invalid (ignored)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(alpha SELECTARG1 ignores arg2)")) {
    return false;
  }

  const aerogpu::Shader* ps_ptr_before = nullptr;
  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_ptr_before = dev->ps;
    ps_before = dev->ps->handle;
  }

  // Changing the unused alpha arg2 should not create/bind a new shader variant.
  dev->cmd.reset();
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg2, 0x40000000u);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG2=invalid2 (ignored)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "unused alpha arg2 change emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "unused alpha arg2 change emits no BIND_SHADERS")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "unused alpha arg2 change keeps PS handle stable")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "unused alpha arg2 change keeps PS pointer stable")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncIgnoresUnusedAlphaArg1ForSelectArg2() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: select tex1. ALPHAARG1 is intentionally invalid and must be ignored
  // (SELECTARG2 only consumes ARG2).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg2);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG2")) {
    return false;
  }
  constexpr uint32_t kInvalidArg = 0x80000000u;
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kInvalidArg);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=invalid (ignored)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(alpha SELECTARG2 ignores arg1)")) {
    return false;
  }

  const aerogpu::Shader* ps_ptr_before = nullptr;
  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_ptr_before = dev->ps;
    ps_before = dev->ps->handle;
  }

  // Changing the unused alpha arg1 should not create/bind a new shader variant.
  dev->cmd.reset();
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, 0x40000000u);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=invalid2 (ignored)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "unused alpha arg1 change emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "unused alpha arg1 change emits no BIND_SHADERS")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "unused alpha arg1 change keeps PS handle stable")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "unused alpha arg1 change keeps PS pointer stable")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncStage1TFactorUploadsPsConstantOnRenderStateChange() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: uses TFACTOR (no additional texturing) so the fixed-function PS must
  // consume c255.
  //
  // Set args while stage1 is still disabled (default) to avoid generating
  // intermediate PS variants during setup.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTFactor);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TFACTOR")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }
  // Enable stage1.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=SELECTARG1")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 tfactor)")) {
    return false;
  }

  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    // Stage0 samples tex0, stage1 uses only TFACTOR (no tex1).
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 1, "stage1 tfactor => PS contains exactly 1 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x1u, "stage1 tfactor => PS texld uses sampler s0")) {
      return false;
    }
    ps_before = dev->ps->handle;
  }

  // Changing TEXTUREFACTOR should upload the new value into c255 when the active
  // fixed-function stage chain references TFACTOR, without changing the PS
  // variant itself.
  dev->cmd.reset();

  constexpr uint32_t kD3dRsTextureFactor = 60u; // D3DRS_TEXTUREFACTOR
  constexpr uint32_t kTf = 0xFF000000u;         // ARGB => {r,g,b,a} = {0,0,0,1}
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsTextureFactor, kTf);
  if (!Check(hr == S_OK, "SetRenderState(TEXTUREFACTOR)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  bool saw_tf_upload = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 255u || sc->vec4_count != 1u) {
      continue;
    }
    const auto* data = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
    if (data[0] == 0.0f && data[1] == 0.0f && data[2] == 0.0f && data[3] == 1.0f) {
      saw_tf_upload = true;
      break;
    }
  }
  if (!Check(saw_tf_upload, "SetRenderState(TEXTUREFACTOR) uploads PS constant c255")) {
    return false;
  }

  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetRenderState(TEXTUREFACTOR) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "SetRenderState(TEXTUREFACTOR) emits no BIND_SHADERS")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "SetRenderState(TEXTUREFACTOR) keeps PS handle stable")) {
      return false;
    }
  }

  // Setting the same TEXTUREFACTOR again must be a no-op: no redundant constant
  // upload and no redundant render-state command packet.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsTextureFactor, kTf);
  if (!Check(hr == S_OK, "SetRenderState(TEXTUREFACTOR, same value)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf2 = dev->cmd.data();
  const size_t len2 = dev->cmd.bytes_used();
  if (!Check(len2 == sizeof(aerogpu_cmd_stream_header), "SetRenderState(TEXTUREFACTOR, same) emits no packets")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf2, len2, AEROGPU_CMD_SET_SHADER_CONSTANTS_F).empty(),
             "SetRenderState(TEXTUREFACTOR, same) emits no SET_SHADER_CONSTANTS_F")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf2, len2, AEROGPU_CMD_SET_RENDER_STATE).empty(),
             "SetRenderState(TEXTUREFACTOR, same) emits no SET_RENDER_STATE")) {
    return false;
  }

  return true;
}

bool TestFixedfuncStage1TFactorInAlphaUploadsPsConstantOnRenderStateChange() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1 uses TFACTOR in the alpha combiner (no additional texturing).
  //
  // Set args while stage1 is still disabled (default) to avoid generating
  // intermediate PS variants during setup.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=CURRENT (no color sampling)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaTFactor);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=TFACTOR")) {
    return false;
  }
  // Enable stage1.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=SELECTARG1")) {
    return false;
  }

  // Ensure the stage chain ends deterministically.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 alpha tfactor)")) {
    return false;
  }

  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    // Stage0 samples tex0, stage1 uses only TFACTOR (no tex1).
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 1, "stage1 alpha tfactor => PS contains exactly 1 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x1u, "stage1 alpha tfactor => PS texld uses sampler s0")) {
      return false;
    }
    ps_before = dev->ps->handle;
  }

  // Changing TEXTUREFACTOR should upload the new value into c255 when the active
  // fixed-function stage chain references TFACTOR, without changing the PS
  // variant itself.
  dev->cmd.reset();

  constexpr uint32_t kD3dRsTextureFactor = 60u; // D3DRS_TEXTUREFACTOR
  constexpr uint32_t kTf = 0xFF000000u;         // ARGB => {r,g,b,a} = {0,0,0,1}
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsTextureFactor, kTf);
  if (!Check(hr == S_OK, "SetRenderState(TEXTUREFACTOR)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  bool saw_tf_upload = false;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage != AEROGPU_SHADER_STAGE_PIXEL || sc->start_register != 255u || sc->vec4_count != 1u) {
      continue;
    }
    const auto* data = reinterpret_cast<const float*>(reinterpret_cast<const uint8_t*>(sc) + sizeof(*sc));
    if (data[0] == 0.0f && data[1] == 0.0f && data[2] == 0.0f && data[3] == 1.0f) {
      saw_tf_upload = true;
      break;
    }
  }
  if (!Check(saw_tf_upload, "SetRenderState(TEXTUREFACTOR) uploads PS constant c255 (alpha tfactor)")) {
    return false;
  }

  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetRenderState(TEXTUREFACTOR) emits no CREATE_SHADER_DXBC (alpha tfactor)")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "SetRenderState(TEXTUREFACTOR) emits no BIND_SHADERS (alpha tfactor)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "SetRenderState(TEXTUREFACTOR) keeps PS handle stable (alpha tfactor)")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncUnusedTfactorDoesNotUploadPsConstant() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState is available")) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1 selects tex1. COLORARG2 is set to TFACTOR but must be ignored
  // (SELECTARG1 only consumes ARG1). This ensures that changing TEXTUREFACTOR
  // does not upload PS constant c255 when the stage chain doesn't actually use
  // it.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaTFactor);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=TFACTOR (unused)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 selectarg1 ignores tfactor arg2)")) {
    return false;
  }

  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    // Stage0 and stage1 both sample textures; TFACTOR is unused.
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "unused tfactor => PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "unused tfactor => PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_before = dev->ps->handle;
  }

  // Changing TEXTUREFACTOR should not upload c255 when the active stage chain
  // doesn't actually reference TFACTOR.
  dev->cmd.reset();

  constexpr uint32_t kD3dRsTextureFactor = 60u; // D3DRS_TEXTUREFACTOR
  constexpr uint32_t kTf = 0xFF000000u;         // ARGB => {r,g,b,a} = {0,0,0,1}
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, kD3dRsTextureFactor, kTf);
  if (!Check(hr == S_OK, "SetRenderState(TEXTUREFACTOR)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_SHADER_CONSTANTS_F)) {
    const auto* sc = reinterpret_cast<const aerogpu_cmd_set_shader_constants_f*>(hdr);
    if (sc->stage == AEROGPU_SHADER_STAGE_PIXEL && sc->start_register == 255u) {
      return Check(false, "unused tfactor => SetRenderState(TEXTUREFACTOR) must not upload PS constant c255");
    }
  }

  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "unused tfactor => SetRenderState(TEXTUREFACTOR) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "unused tfactor => SetRenderState(TEXTUREFACTOR) emits no BIND_SHADERS")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "unused tfactor => SetRenderState(TEXTUREFACTOR) keeps PS handle stable")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncUnboundStage0TextureTruncatesChainToZeroStages() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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

  // Stage0 intentionally left unbound. Bind a stage1 texture anyway to ensure it
  // is ignored when the chain truncates at stage0.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1)")) {
    return false;
  }

  // Stage0 requests texturing, but stage0 texture is unbound. The driver should
  // truncate the stage chain and fall back to a stage0-disabled (diffuse-only) PS.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1 uses an unsupported op, but must be ignored because the stage chain is
  // already truncated due to stage0 missing its texture.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopAddSmooth);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=ADDSMOOTH (unsupported, should be ignored)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage0 texture missing)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 0, "stage0 missing => PS contains no texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0, "stage0 missing => PS uses no samplers")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncUnboundStage0TextureDoesNotTruncateWhenStage0DoesNotSample() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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

  // Stage0 intentionally left unbound. Bind a stage1 texture which will be sampled.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1)")) {
    return false;
  }

  // Stage0 does not sample: CURRENT = CURRENT (canonicalized to DIFFUSE).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=CURRENT (no sampling)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=CURRENT (no sampling)")) {
    return false;
  }

  // Stage1 samples tex1.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Ensure the stage chain ends deterministically.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage0 missing but stage0 doesn't sample)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 1, "stage0 doesn't sample => PS contains exactly 1 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x2u, "stage0 doesn't sample => PS texld uses only sampler s1")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncStage0CurrentIsCanonicalizedToDiffuse() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  // Stage0 does not sample any textures.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Explicitly disable stage1 so the stage chain ends deterministically.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=DISABLE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  // Draw once to ensure the fixed-function PS is created and bound.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage0 CURRENT baseline)")) {
    return false;
  }

  const aerogpu::Shader* ps_ptr_before = nullptr;
  aerogpu_handle_t ps_before = 0;
  size_t cache_size_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 0, "stage0 CURRENT => PS contains no texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0, "stage0 CURRENT => PS uses no samplers")) {
      return false;
    }
    ps_ptr_before = dev->ps;
    ps_before = dev->ps->handle;
    cache_size_before = dev->fixedfunc_ps_variant_cache.size();
  }
  if (!Check(ps_before != 0, "stage0 CURRENT => bound non-zero PS handle")) {
    return false;
  }

  // Switch stage0 from CURRENT to DIFFUSE. The driver canonicalizes stage0 CURRENT
  // to DIFFUSE, so this state change should not create/bind a new shader variant
  // (and should not grow the signature cache).
  dev->cmd.reset();
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaDiffuse);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=DIFFUSE (canonicalized)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaDiffuse);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=DIFFUSE (canonicalized)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "stage0 CURRENT->DIFFUSE emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "stage0 CURRENT->DIFFUSE emits no BIND_SHADERS")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "stage0 CURRENT->DIFFUSE keeps PS pointer stable")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "stage0 CURRENT->DIFFUSE keeps PS handle stable")) {
      return false;
    }
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "stage0 CURRENT->DIFFUSE does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncUnboundStage2TextureDoesNotTruncateWhenStage2DoesNotSample() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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
  D3DDDI_HRESOURCE hTex2{};
  D3DDDI_HRESOURCE hTex3{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2) ||
      !CreateDummyTexture(&cleanup, &hTex3)) {
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
  // Stage2 intentionally left unbound, but stage2 will not sample it.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3);
  if (!Check(hr == S_OK, "SetTexture(stage3)")) {
    return false;
  }

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: CURRENT = tex1 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage2 does not sample its texture: CURRENT = CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 COLORARG1=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage3 samples texture3.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage3 ALPHAARG1=CURRENT")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage2 missing but stage2 doesn't sample)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 3, "stage2 doesn't sample => PS contains exactly 3 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0xBu, "stage2 doesn't sample => PS texld uses samplers s0, s1, s3")) {
      return false;
    }
  }

  const aerogpu::Shader* ps_ptr_before = nullptr;
  aerogpu_handle_t ps_before = 0;
  size_t cache_size_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    ps_ptr_before = dev->ps;
    ps_before = dev->ps ? dev->ps->handle : 0;
    cache_size_before = dev->fixedfunc_ps_variant_cache.size();
  }
  if (!Check(ps_before != 0, "draw bound non-zero PS handle")) {
    return false;
  }

  // Binding/unbinding an unused stage texture must not affect fixed-function PS
  // selection when the stage state does not sample it.
  dev->cmd.reset();
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2=bind, unused)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage2=bind, unused) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "SetTexture(stage2=bind, unused) emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage2=bind, unused) emits no DRAW")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound after stage2 bind")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "unused stage2 bind keeps PS pointer stable")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "unused stage2 bind keeps PS handle stable")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 3, "unused stage2 bind => PS still contains exactly 3 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0xBu, "unused stage2 bind => PS still uses samplers s0, s1, s3")) {
      return false;
    }
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "unused stage2 bind does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  // Unbind stage2 texture again; should also not create/rebind PS variants.
  dev->cmd.reset();
  D3DDDI_HRESOURCE null_tex{};
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, null_tex);
  if (!Check(hr == S_OK, "SetTexture(stage2=null, unused)")) {
    return false;
  }

  dev->cmd.finalize();
  buf = dev->cmd.data();
  const size_t len2 = dev->cmd.bytes_used();
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "SetTexture(stage2=null, unused) emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_BIND_SHADERS).empty(),
             "SetTexture(stage2=null, unused) emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len2, AEROGPU_CMD_DRAW).empty(), "SetTexture(stage2=null, unused) emits no DRAW")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound after stage2 unbind")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "unused stage2 unbind keeps PS pointer stable")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "unused stage2 unbind keeps PS handle stable")) {
      return false;
    }
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "unused stage2 unbind does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncStage0DisableTruncatesChainAndIgnoresAlphaAndLaterStages() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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
  D3DDDI_HRESOURCE hTex2{};
  D3DDDI_HRESOURCE hTex3{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2) ||
      !CreateDummyTexture(&cleanup, &hTex3)) {
    return false;
  }

  // Bind textures anyway so sampler state is not a factor; stage0 DISABLE must
  // suppress all fixed-function texturing regardless.
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/0, hTex0);
  if (!Check(hr == S_OK, "SetTexture(stage0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/1, hTex1);
  if (!Check(hr == S_OK, "SetTexture(stage1)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3);
  if (!Check(hr == S_OK, "SetTexture(stage3)")) {
    return false;
  }

  // Stage0 disables the entire fixed-function stage chain. Stage0 alpha op is
  // set to an unsupported value to ensure it is ignored when COLOROP=DISABLE.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=DISABLE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopAddSmooth);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=ADDSMOOTH (ignored)")) {
    return false;
  }

  // Stage1-3 use unsupported ops, but must be ignored since stage0 disables the chain.
  for (uint32_t stage = 1; stage <= 3; ++stage) {
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorOp, kD3dTopAddSmooth);
    if (!Check(hr == S_OK, "TSS stageN COLOROP=ADDSMOOTH (unsupported, should be ignored)")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorArg1, kD3dTaTexture);
    if (!Check(hr == S_OK, "TSS stageN COLORARG1=TEXTURE")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssAlphaOp, kD3dTopAddSmooth);
    if (!Check(hr == S_OK, "TSS stageN ALPHAOP=ADDSMOOTH (unsupported, should be ignored)")) {
      return false;
    }
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage0 disable)")) {
    return false;
  }

  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 0, "stage0 disable => PS contains no texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0, "stage0 disable => PS uses no samplers")) {
      return false;
    }
    ps_before = dev->ps->handle;
  }
  if (!Check(ps_before != 0, "stage0 disable => bound non-zero PS handle")) {
    return false;
  }

  // Changing later-stage state must not create/bind a new PS since stage0 disables
  // the stage chain.
  dev->cmd.reset();
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopAdd);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=ADD (ignored)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "stage0 disable => later stage change emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "stage0 disable => later stage change emits no BIND_SHADERS")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "stage0 disable => later stage change keeps PS handle stable")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncStage1DisableTruncatesChainAndIgnoresLaterStages() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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
  D3DDDI_HRESOURCE hTex2{};
  D3DDDI_HRESOURCE hTex3{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2) ||
      !CreateDummyTexture(&cleanup, &hTex3)) {
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
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3);
  if (!Check(hr == S_OK, "SetTexture(stage3)")) {
    return false;
  }

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: disable the stage chain. Stage1 alpha op is set to an unsupported
  // value to ensure it is ignored when COLOROP=DISABLE.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=DISABLE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopAddSmooth);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=ADDSMOOTH (ignored)")) {
    return false;
  }

  // Stage2/3 configured beyond the disabled stage to ensure they are ignored.
  // Stage3 uses an unsupported op to validate that later stage state does not
  // affect draw validation when stage1 disables the chain.
  for (uint32_t stage = 2; stage <= 3; ++stage) {
    const uint32_t colorop = (stage == 3) ? kD3dTopAddSmooth : kD3dTopModulate;
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorOp, colorop);
    if (stage == 3) {
      if (!Check(hr == S_OK, "TSS stage3 COLOROP=ADDSMOOTH (unsupported, should be ignored)")) {
        return false;
      }
    } else {
      if (!Check(hr == S_OK, "TSS stage2 COLOROP=MODULATE")) {
        return false;
      }
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorArg1, kD3dTaTexture);
    if (!Check(hr == S_OK, "TSS stageN COLORARG1=TEXTURE")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorArg2, kD3dTaCurrent);
    if (!Check(hr == S_OK, "TSS stageN COLORARG2=CURRENT")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssAlphaOp, kD3dTopSelectArg1);
    if (!Check(hr == S_OK, "TSS stageN ALPHAOP=SELECTARG1")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssAlphaArg1, kD3dTaCurrent);
    if (!Check(hr == S_OK, "TSS stageN ALPHAARG1=CURRENT")) {
      return false;
    }
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage1 disable)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 1, "stage1 disable => PS contains exactly 1 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x1u, "stage1 disable => PS texld uses only sampler s0")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncStage2DisableTruncatesChainAndIgnoresLaterStages() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, kFvfXyzrhwDiffuseTex1);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0{};
  D3DDDI_HRESOURCE hTex1{};
  D3DDDI_HRESOURCE hTex2{};
  D3DDDI_HRESOURCE hTex3{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2) ||
      !CreateDummyTexture(&cleanup, &hTex3)) {
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
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3);
  if (!Check(hr == S_OK, "SetTexture(stage3)")) {
    return false;
  }

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1: CURRENT = tex1 * CURRENT.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Stage2: disable the stage chain. Stage2 alpha op is set to an unsupported
  // value to ensure it is ignored when COLOROP=DISABLE.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=DISABLE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssAlphaOp, kD3dTopAddSmooth);
  if (!Check(hr == S_OK, "TSS stage2 ALPHAOP=ADDSMOOTH (ignored)")) {
    return false;
  }

  // Stage3 configured beyond the disabled stage to ensure it is ignored.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopAddSmooth);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=ADDSMOOTH (unsupported, should be ignored)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage3 COLORARG1=TEXTURE")) {
    return false;
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  // Draw once to bind the stage0+stage1 PS.
  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(stage2 disable)")) {
    return false;
  }

  const aerogpu::Shader* ps_ptr_before = nullptr;
  aerogpu_handle_t ps_before = 0;
  size_t cache_size_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) == 2, "stage2 disable => PS contains exactly 2 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0x3u, "stage2 disable => PS texld uses samplers s0 and s1")) {
      return false;
    }
    ps_ptr_before = dev->ps;
    ps_before = dev->ps->handle;
    cache_size_before = dev->fixedfunc_ps_variant_cache.size();
  }
  if (!Check(ps_before != 0, "stage2 disable => bound non-zero PS handle")) {
    return false;
  }

  // Changing later-stage state must not create/bind a new PS since stage2 disables
  // the stage chain.
  dev->cmd.reset();
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopAdd);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=ADD (ignored)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "stage2 disable => later stage change emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(),
             "stage2 disable => later stage change emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "stage2 disable => later stage change emits no DRAW")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps == ps_ptr_before, "stage2 disable => later stage change keeps PS pointer stable")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "stage2 disable => later stage change keeps PS handle stable")) {
      return false;
    }
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "stage2 disable => later stage change does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncUnsupportedStage1OpFailsDrawWithInvalidCall() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1 uses an unsupported op. State-setting should succeed, but fixed-function draws must
  // fail with INVALIDCALL.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopAddSmooth);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=ADDSMOOTH (unsupported)")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaArg1, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAARG1=CURRENT")) {
    return false;
  }

  // Explicitly disable stage2 so stage-chain evaluation is deterministic.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=DISABLE")) {
    return false;
  }

  size_t cache_size_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    cache_size_before = dev->fixedfunc_ps_variant_cache.size();
  }

  // Isolate the draw attempt. Unsupported fixed-function draws should not emit any shader binds
  // or UP uploads.
  dev->cmd.reset();

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == kD3DErrInvalidCall, "DrawPrimitiveUP(unsupported stage1) returns D3DERR_INVALIDCALL")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(len == sizeof(aerogpu_cmd_stream_header), "unsupported draw emits no packets")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "unsupported draw emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(), "unsupported draw emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "unsupported draw emits no DRAW")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "unsupported draw does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncInvalidStage1ArgFailsDrawWithInvalidCall() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1 uses a valid op, but an invalid arg in a *used* slot. State-setting
  // should succeed, but draws must fail with INVALIDCALL.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=SELECTARG1")) {
    return false;
  }
  constexpr uint32_t kInvalidArg = 0x80000000u;
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kInvalidArg);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=invalid (unsupported at draw time)")) {
    return false;
  }
  // Terminate the stage chain deterministically.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=DISABLE")) {
    return false;
  }

  size_t cache_size_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    cache_size_before = dev->fixedfunc_ps_variant_cache.size();
  }

  // Isolate the draw attempt. Unsupported fixed-function draws should not emit any shader binds
  // or UP uploads.
  dev->cmd.reset();

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == kD3DErrInvalidCall, "DrawPrimitiveUP(invalid stage1 arg) returns D3DERR_INVALIDCALL")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(len == sizeof(aerogpu_cmd_stream_header), "unsupported draw emits no packets")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "unsupported draw emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(), "unsupported draw emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "unsupported draw emits no DRAW")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "unsupported draw does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncUnsupportedStage1AlphaOpFailsDrawWithInvalidCall() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1 uses a supported color op, but an unsupported alpha op. State-setting
  // should succeed, but fixed-function draws must fail with INVALIDCALL.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorOp, kD3dTopModulate);
  if (!Check(hr == S_OK, "TSS stage1 COLOROP=MODULATE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssColorArg2, kD3dTaCurrent);
  if (!Check(hr == S_OK, "TSS stage1 COLORARG2=CURRENT")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 1, kD3dTssAlphaOp, kD3dTopAddSmooth);
  if (!Check(hr == S_OK, "TSS stage1 ALPHAOP=ADDSMOOTH (unsupported)")) {
    return false;
  }

  // Terminate the stage chain deterministically.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 2, kD3dTssColorOp, kD3dTopDisable);
  if (!Check(hr == S_OK, "TSS stage2 COLOROP=DISABLE")) {
    return false;
  }

  size_t cache_size_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    cache_size_before = dev->fixedfunc_ps_variant_cache.size();
  }

  // Isolate the draw attempt. Unsupported fixed-function draws should not emit any shader binds
  // or UP uploads.
  dev->cmd.reset();

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == kD3DErrInvalidCall, "DrawPrimitiveUP(unsupported alpha op) returns D3DERR_INVALIDCALL")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(len == sizeof(aerogpu_cmd_stream_header), "unsupported draw emits no packets")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC).empty(),
             "unsupported draw emits no CREATE_SHADER_DXBC")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS).empty(), "unsupported draw emits no BIND_SHADERS")) {
    return false;
  }
  if (!Check(CollectOpcodes(buf, len, AEROGPU_CMD_DRAW).empty(), "unsupported draw emits no DRAW")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->fixedfunc_ps_variant_cache.size() == cache_size_before,
               "unsupported draw does not grow fixedfunc_ps_variant_cache")) {
      return false;
    }
  }

  return true;
}

bool TestFixedfuncFourStageEmitsFourTexldAndRebindsOnStage3Change() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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
  D3DDDI_HRESOURCE hTex2{};
  D3DDDI_HRESOURCE hTex3{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2) ||
      !CreateDummyTexture(&cleanup, &hTex3)) {
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
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3);
  if (!Check(hr == S_OK, "SetTexture(stage3)")) {
    return false;
  }

  // Stage0: CURRENT = tex0 (both color and alpha).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  // Stage1-3: CURRENT = texN * CURRENT. Keep alpha as passthrough CURRENT to
  // avoid additional alpha-specific ops that could complicate token counting.
  for (uint32_t stage = 1; stage <= 3; ++stage) {
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorOp, kD3dTopModulate);
    if (!Check(hr == S_OK, "TSS stageN COLOROP=MODULATE")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorArg1, kD3dTaTexture);
    if (!Check(hr == S_OK, "TSS stageN COLORARG1=TEXTURE")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorArg2, kD3dTaCurrent);
    if (!Check(hr == S_OK, "TSS stageN COLORARG2=CURRENT")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssAlphaOp, kD3dTopSelectArg1);
    if (!Check(hr == S_OK, "TSS stageN ALPHAOP=SELECTARG1")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssAlphaArg1, kD3dTaCurrent);
    if (!Check(hr == S_OK, "TSS stageN ALPHAARG1=CURRENT")) {
      return false;
    }
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(first 4-stage)")) {
    return false;
  }

  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) >= 4, "4-stage fixed-function PS contains >= 4 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0xFu, "4-stage fixed-function PS texld uses samplers s0..s3")) {
      return false;
    }
    ps_before = dev->ps->handle;
  }
  if (!Check(ps_before != 0, "first draw bound non-zero PS handle")) {
    return false;
  }

  // Change stage3 op to force a different shader variant. Stage3 must remain
  // active and continue sampling its texture, so use ADD rather than DISABLE.
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 3, kD3dTssColorOp, kD3dTopAdd);
  if (!Check(hr == S_OK, "TSS stage3 COLOROP=ADD")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(second 4-stage after stage3 change)")) {
    return false;
  }

  aerogpu_handle_t ps_after = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound after stage3 change")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) >= 4, "second 4-stage PS contains >= 4 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0xFu, "second 4-stage PS texld uses samplers s0..s3")) {
      return false;
    }
    ps_after = dev->ps->handle;
  }
  if (!Check(ps_after != 0, "second draw bound non-zero PS handle")) {
    return false;
  }
  if (!Check(ps_after != ps_before, "stage3 state change causes PS handle change")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  // Validate that all textures were bound at least once.
  bool saw_tex[4] = {false, false, false, false};
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_SET_TEXTURE)) {
    const auto* st = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
    if (st->shader_stage != AEROGPU_SHADER_STAGE_PIXEL) {
      continue;
    }
    if (st->slot < 4 && st->texture != 0) {
      saw_tex[st->slot] = true;
    }
  }
  if (!Check(saw_tex[0] && saw_tex[1] && saw_tex[2] && saw_tex[3], "command stream binds texture slots 0..3")) {
    return false;
  }

  // Validate shader binds include both PS handles.
  const auto binds = CollectOpcodes(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(!binds.empty(), "BIND_SHADERS packets collected")) {
    return false;
  }
  bool saw_ps_before = false;
  bool saw_ps_after = false;
  for (const auto* hdr : binds) {
    const auto* b = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
    if (b->ps == ps_before) {
      saw_ps_before = true;
    }
    if (b->ps == ps_after) {
      saw_ps_after = true;
    }
  }
  if (!Check(saw_ps_before && saw_ps_after, "command stream binds both PS variants")) {
    return false;
  }

  return true;
}

bool TestFixedfuncStage4StateIsIgnoredBeyondMaxTextureStages() {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
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
  D3DDDI_HRESOURCE hTex2{};
  D3DDDI_HRESOURCE hTex3{};
  if (!CreateDummyTexture(&cleanup, &hTex0) ||
      !CreateDummyTexture(&cleanup, &hTex1) ||
      !CreateDummyTexture(&cleanup, &hTex2) ||
      !CreateDummyTexture(&cleanup, &hTex3)) {
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
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/2, hTex2);
  if (!Check(hr == S_OK, "SetTexture(stage2)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, /*stage=*/3, hTex3);
  if (!Check(hr == S_OK, "SetTexture(stage3)")) {
    return false;
  }

  // Configure a 4-stage chain (0..3).
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 COLOROP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssColorArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 COLORARG1=TEXTURE")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaOp, kD3dTopSelectArg1);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAOP=SELECTARG1")) {
    return false;
  }
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 0, kD3dTssAlphaArg1, kD3dTaTexture);
  if (!Check(hr == S_OK, "TSS stage0 ALPHAARG1=TEXTURE")) {
    return false;
  }

  for (uint32_t stage = 1; stage <= 3; ++stage) {
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorOp, kD3dTopModulate);
    if (!Check(hr == S_OK, "TSS stageN COLOROP=MODULATE")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorArg1, kD3dTaTexture);
    if (!Check(hr == S_OK, "TSS stageN COLORARG1=TEXTURE")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssColorArg2, kD3dTaCurrent);
    if (!Check(hr == S_OK, "TSS stageN COLORARG2=CURRENT")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssAlphaOp, kD3dTopSelectArg1);
    if (!Check(hr == S_OK, "TSS stageN ALPHAOP=SELECTARG1")) {
      return false;
    }
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, stage, kD3dTssAlphaArg1, kD3dTaCurrent);
    if (!Check(hr == S_OK, "TSS stageN ALPHAARG1=CURRENT")) {
      return false;
    }
  }

  const VertexXyzrhwDiffuseTex1 tri[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 0.0f},
      {16.0f, 0.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 1.0f, 0.0f},
      {0.0f, 16.0f, 0.0f, 1.0f, 0xFFFFFFFFu, 0.0f, 1.0f},
  };

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(first 4-stage)")) {
    return false;
  }

  aerogpu_handle_t ps_before = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS bound")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) >= 4, "fixed-function PS contains >= 4 texld")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0xFu, "fixed-function PS texld uses samplers s0..s3")) {
      return false;
    }
    ps_before = dev->ps->handle;
  }
  if (!Check(ps_before != 0, "first draw bound non-zero PS handle")) {
    return false;
  }

  // Stage4 is beyond the maximum supported fixed-function stage chain. Set an
  // unsupported stage-state op and ensure it is ignored (draws succeed, PS stays
  // stable).
  dev->cmd.reset();
  hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice, 4, kD3dTssColorOp, kD3dTopAddSmooth);
  if (!Check(hr == S_OK, "TSS stage4 COLOROP=ADDSMOOTH (ignored)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(tri[0]));
  if (!Check(hr == S_OK, "DrawPrimitiveUP(second 4-stage, stage4 invalid)")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->ps != nullptr, "fixed-function PS still bound")) {
      return false;
    }
    if (!Check(dev->ps->handle == ps_before, "stage4 state change keeps PS handle stable")) {
      return false;
    }
    if (!Check(CountToken(dev->ps, kPsOpTexld) >= 4, "still contains >= 4 texld after stage4 state")) {
      return false;
    }
    if (!Check(TexldSamplerMask(dev->ps) == 0xFu, "still uses samplers s0..s3 after stage4 state")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  size_t ps_creates = 0;
  for (const auto* hdr : CollectOpcodes(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC)) {
    const auto* cs = reinterpret_cast<const aerogpu_cmd_create_shader_dxbc*>(hdr);
    if (cs->stage == AEROGPU_SHADER_STAGE_PIXEL) {
      ++ps_creates;
    }
  }
  if (!Check(ps_creates == 0, "stage4 state does not create a new fixed-function PS")) {
    return false;
  }

  return true;
}

} // namespace

int main() {
  if (!TestFixedfuncTwoStageEmitsTwoTexldAndRebinds()) {
    return 1;
  }
  if (!TestFixedfuncUnboundStage1TextureTruncatesChainAndDoesNotRebind()) {
    return 1;
  }
  if (!TestFixedfuncUnboundStage1TextureDoesNotTruncateWhenStage1DoesNotSample()) {
    return 1;
  }
  if (!TestFixedfuncUnboundStage1TextureTruncatesWhenStage1UsesTextureInAlphaOnly()) {
    return 1;
  }
  if (!TestFixedfuncBindUnbindStage1TextureRebindsPixelShader()) {
    return 1;
  }
  if (!TestFixedfuncSwitchStage1TextureDoesNotRebindPixelShader()) {
    return 1;
  }
  if (!TestFixedfuncSwitchStage0TextureDoesNotRebindPixelShader()) {
    return 1;
  }
  if (!TestFixedfuncSwitchStage2TextureDoesNotRebindPixelShader()) {
    return 1;
  }
  if (!TestFixedfuncUnboundStage0TextureTruncatesChainToZeroStages()) {
    return 1;
  }
  if (!TestFixedfuncUnboundStage0TextureDoesNotTruncateWhenStage0DoesNotSample()) {
    return 1;
  }
  if (!TestFixedfuncStage0CurrentIsCanonicalizedToDiffuse()) {
    return 1;
  }
  if (!TestFixedfuncStage0DisableTruncatesChainAndIgnoresAlphaAndLaterStages()) {
    return 1;
  }
  if (!TestFixedfuncUnboundStage2TextureTruncatesBeforeStage3()) {
    return 1;
  }
  if (!TestFixedfuncUnboundStage2TextureTruncatesWhenStage2UsesBlendTextureAlpha()) {
    return 1;
  }
  if (!TestFixedfuncUnboundStage2TextureTruncatesWhenStage2UsesBlendTextureAlphaInAlphaOnly()) {
    return 1;
  }
  if (!TestFixedfuncBindUnbindStage2TextureRebindsPixelShader()) {
    return 1;
  }
  if (!TestFixedfuncUnboundStage3TextureTruncatesChainAndDoesNotRebind()) {
    return 1;
  }
  if (!TestFixedfuncBindUnbindStage3TextureRebindsPixelShader()) {
    return 1;
  }
  if (!TestFixedfuncSwitchStage3TextureDoesNotRebindPixelShader()) {
    return 1;
  }
  if (!TestFixedfuncUnboundStage3TextureDoesNotTruncateWhenStage3DoesNotSample()) {
    return 1;
  }
  if (!TestFixedfuncIgnoresUnusedColorArg2ForSelectArg1()) {
    return 1;
  }
  if (!TestFixedfuncIgnoresUnusedColorArg1ForSelectArg2()) {
    return 1;
  }
  if (!TestFixedfuncIgnoresUnusedAlphaArg2ForSelectArg1()) {
    return 1;
  }
  if (!TestFixedfuncIgnoresUnusedAlphaArg1ForSelectArg2()) {
    return 1;
  }
  if (!TestFixedfuncStage1TFactorUploadsPsConstantOnRenderStateChange()) {
    return 1;
  }
  if (!TestFixedfuncStage1TFactorInAlphaUploadsPsConstantOnRenderStateChange()) {
    return 1;
  }
  if (!TestFixedfuncUnusedTfactorDoesNotUploadPsConstant()) {
    return 1;
  }
  if (!TestFixedfuncUnboundStage2TextureDoesNotTruncateWhenStage2DoesNotSample()) {
    return 1;
  }
  if (!TestFixedfuncStage1DisableTruncatesChainAndIgnoresLaterStages()) {
    return 1;
  }
  if (!TestFixedfuncStage2DisableTruncatesChainAndIgnoresLaterStages()) {
    return 1;
  }
  if (!TestFixedfuncUnsupportedStage1OpFailsDrawWithInvalidCall()) {
    return 1;
  }
  if (!TestFixedfuncInvalidStage1ArgFailsDrawWithInvalidCall()) {
    return 1;
  }
  if (!TestFixedfuncUnsupportedStage1AlphaOpFailsDrawWithInvalidCall()) {
    return 1;
  }
  if (!TestFixedfuncFourStageEmitsFourTexldAndRebindsOnStage3Change()) {
    return 1;
  }
  if (!TestFixedfuncStage4StateIsIgnoredBeyondMaxTextureStages()) {
    return 1;
  }
  return 0;
}
