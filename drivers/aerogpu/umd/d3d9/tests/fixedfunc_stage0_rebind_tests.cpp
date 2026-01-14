#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <vector>

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_test_entrypoints.h"

namespace {

// Portable D3D9 FVF bits (from d3d9types.h).
constexpr uint32_t kD3dFvfXyz = 0x00000002u;
constexpr uint32_t kD3dFvfXyzRhw = 0x00000004u;
constexpr uint32_t kD3dFvfNormal = 0x00000010u;
constexpr uint32_t kD3dFvfDiffuse = 0x00000040u;
constexpr uint32_t kD3dFvfTex1 = 0x00000100u;
constexpr uint32_t kFvfXyzDiffuse = kD3dFvfXyz | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzDiffuseTex1 = kD3dFvfXyz | kD3dFvfDiffuse | kD3dFvfTex1;
constexpr uint32_t kFvfXyzNormal = kD3dFvfXyz | kD3dFvfNormal;
constexpr uint32_t kFvfXyzNormalTex1 = kD3dFvfXyz | kD3dFvfNormal | kD3dFvfTex1;
constexpr uint32_t kFvfXyzrhwDiffuse = kD3dFvfXyzRhw | kD3dFvfDiffuse;
constexpr uint32_t kFvfXyzrhwDiffuseTex1 = kD3dFvfXyzRhw | kD3dFvfDiffuse | kD3dFvfTex1;
// D3DFVF_TEXCOORDSIZE* encodes 2 bits per texcoord set starting at bit 16. These
// bits may contain garbage for unused texcoord sets; the driver should ignore
// them (except for texcoord 0 when TEX1 is present).
constexpr uint32_t kFvfGarbageTexCoord0SizeBits = (0x2u << 16u); // float4 for tex0
constexpr uint32_t kFvfGarbageTexCoord1SizeBits = (0x2u << 18u); // float4 for tex1

// D3DTSS_* (from d3d9types.h).
constexpr uint32_t kD3dTssColorOp = 1u;

// D3DTEXTUREOP (from d3d9types.h).
constexpr uint32_t kD3dTopSelectArg1 = 2u;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
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
  if (!Check(cleanup->device_funcs.pfnDestroyResource != nullptr, "pfnDestroyResource is available")) {
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

struct BindInfo {
  size_t offset = 0;
  aerogpu_handle_t vs = 0;
  aerogpu_handle_t ps = 0;
};

std::vector<BindInfo> CollectBinds(const uint8_t* buf, size_t len) {
  std::vector<BindInfo> out;
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return out;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_BIND_SHADERS && hdr->size_bytes >= sizeof(aerogpu_cmd_bind_shaders) &&
        offset + hdr->size_bytes <= len) {
      const auto* bind = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(hdr);
      out.push_back(BindInfo{offset, bind->vs, bind->ps});
    }
    if (hdr->size_bytes == 0 || (hdr->size_bytes & 3u) != 0 || hdr->size_bytes > len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return out;
}

size_t CountOpcode(const uint8_t* buf, size_t len, uint32_t opcode) {
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }

  size_t count = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      count++;
    }
    if (hdr->size_bytes == 0 || (hdr->size_bytes & 3u) != 0 || hdr->size_bytes > len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return count;
}

template <typename VertexT>
bool TestStage0ColorOpImmediateRebindForFvf(
    uint32_t fvf,
    const VertexT* tri,
    const char* fvf_name) {
  CleanupDevice cleanup;
  if (!CreateDevice(&cleanup)) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->cmd.reset();

  HRESULT hr = cleanup.device_funcs.pfnSetFVF(cleanup.hDevice, fvf);
  if (!Check(hr == S_OK, fvf_name)) {
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
      cleanup.hDevice, D3DDDIPT_TRIANGLELIST, /*primitive_count=*/1, tri, sizeof(VertexT));
  if (!Check(hr == S_OK, "DrawPrimitiveUP")) {
    return false;
  }

  // Record where the first draw ended so we can ensure the stage-state update
  // triggers a bind without issuing another draw.
  const size_t baseline = dev->cmd.bytes_used();

  if (cleanup.device_funcs.pfnSetTextureStageState) {
    hr = cleanup.device_funcs.pfnSetTextureStageState(cleanup.hDevice,
                                                      /*stage=*/0,
                                                      kD3dTssColorOp,
                                                      kD3dTopSelectArg1);
  } else {
    hr = aerogpu::device_set_texture_stage_state(cleanup.hDevice,
                                                 /*stage=*/0,
                                                 kD3dTssColorOp,
                                                 kD3dTopSelectArg1);
  }
  if (!Check(hr == S_OK, "SetTextureStageState(stage0 COLOROP=SELECTARG1)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  const auto binds = CollectBinds(buf, len);
  if (!Check(binds.size() >= 2, "expected >= 2 BIND_SHADERS packets")) {
    return false;
  }

  aerogpu_handle_t ps_before = 0;
  aerogpu_handle_t ps_after = 0;
  for (const auto& b : binds) {
    if (b.offset < baseline) {
      ps_before = b.ps;
    } else if (ps_after == 0) {
      ps_after = b.ps;
    }
  }

  if (!Check(ps_before != 0, "expected a PS bind during first draw")) {
    return false;
  }
  if (!Check(ps_after != 0, "expected an immediate PS rebind after SetTextureStageState")) {
    return false;
  }
  if (!Check(ps_before != ps_after, "expected PS handles to differ across the rebind")) {
    return false;
  }

  // Sanity: we only issued one draw call, but still observed multiple shader binds.
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_DRAW) == 1, "expected exactly 1 DRAW packet")) {
    return false;
  }

  return true;
}

} // namespace

int main() {
  // Keep a single executable and validate immediate stage0 PS rebind behavior for
  // several supported FVFs.
  constexpr uint32_t kWhite = 0xFFFFFFFFu;

  struct VertexXyzDiffuse {
    float x;
    float y;
    float z;
    uint32_t color;
  };
  const VertexXyzDiffuse tri_xyz_diffuse[3] = {
      {-1.0f, -1.0f, 0.0f, kWhite},
      {1.0f, -1.0f, 0.0f, kWhite},
      {0.0f, 1.0f, 0.0f, kWhite},
  };

  struct VertexXyzDiffuseTex1 {
    float x;
    float y;
    float z;
    uint32_t color;
    float u;
    float v;
  };
  const VertexXyzDiffuseTex1 tri_xyz_diffuse_tex1[3] = {
      {-1.0f, -1.0f, 0.0f, kWhite, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, kWhite, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, kWhite, 0.5f, 1.0f},
  };

  struct VertexXyzNormal {
    float x;
    float y;
    float z;
    float nx;
    float ny;
    float nz;
  };
  const VertexXyzNormal tri_xyz_normal[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f, 1.0f},
      {1.0f, -1.0f, 0.0f, 0.0f, 0.0f, 1.0f},
      {0.0f, 1.0f, 0.0f, 0.0f, 0.0f, 1.0f},
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
  const VertexXyzNormalTex1 tri_xyz_normal_tex1[3] = {
      {-1.0f, -1.0f, 0.0f, 0.0f, 0.0f, 1.0f, 0.0f, 0.0f},
      {1.0f, -1.0f, 0.0f, 0.0f, 0.0f, 1.0f, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 0.0f, 0.0f, 1.0f, 0.5f, 1.0f},
  };

  struct VertexXyzrhwDiffuse {
    float x;
    float y;
    float z;
    float rhw;
    uint32_t color;
  };
  const VertexXyzrhwDiffuse tri_xyzrhw_diffuse[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, kWhite},
      {1.0f, 0.0f, 0.0f, 1.0f, kWhite},
      {0.0f, 1.0f, 0.0f, 1.0f, kWhite},
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
  const VertexXyzrhwDiffuseTex1 tri_xyzrhw_diffuse_tex1[3] = {
      {0.0f, 0.0f, 0.0f, 1.0f, kWhite, 0.0f, 0.0f},
      {1.0f, 0.0f, 0.0f, 1.0f, kWhite, 1.0f, 0.0f},
      {0.0f, 1.0f, 0.0f, 1.0f, kWhite, 0.0f, 1.0f},
  };

  if (!TestStage0ColorOpImmediateRebindForFvf(kFvfXyzDiffuseTex1,
                                              tri_xyz_diffuse_tex1,
                                              "SetFVF(XYZ|DIFFUSE|TEX1)")) {
    return 1;
  }
  if (!TestStage0ColorOpImmediateRebindForFvf(kFvfXyzDiffuseTex1 | kFvfGarbageTexCoord1SizeBits,
                                             tri_xyz_diffuse_tex1,
                                             "SetFVF(XYZ|DIFFUSE|TEX1) (unused texcoord-size bits)")) {
    return 1;
  }
  if (!TestStage0ColorOpImmediateRebindForFvf(kFvfXyzrhwDiffuseTex1,
                                              tri_xyzrhw_diffuse_tex1,
                                              "SetFVF(XYZRHW|DIFFUSE|TEX1)")) {
    return 1;
  }
  if (!TestStage0ColorOpImmediateRebindForFvf(kFvfXyzDiffuse,
                                              tri_xyz_diffuse,
                                              "SetFVF(XYZ|DIFFUSE)")) {
    return 1;
  }
  if (!TestStage0ColorOpImmediateRebindForFvf(kFvfXyzNormal,
                                              tri_xyz_normal,
                                              "SetFVF(XYZ|NORMAL)")) {
    return 1;
  }
  if (!TestStage0ColorOpImmediateRebindForFvf(kFvfXyzNormalTex1,
                                              tri_xyz_normal_tex1,
                                              "SetFVF(XYZ|NORMAL|TEX1)")) {
    return 1;
  }
  if (!TestStage0ColorOpImmediateRebindForFvf(kFvfXyzDiffuse | kFvfGarbageTexCoord0SizeBits,
                                             tri_xyz_diffuse,
                                             "SetFVF(XYZ|DIFFUSE) (unused texcoord-size bits)")) {
    return 1;
  }
  if (!TestStage0ColorOpImmediateRebindForFvf(kFvfXyzrhwDiffuse,
                                              tri_xyzrhw_diffuse,
                                              "SetFVF(XYZRHW|DIFFUSE)")) {
    return 1;
  }

  return 0;
}
