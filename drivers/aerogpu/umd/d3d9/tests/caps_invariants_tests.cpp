#include <cstdint>
#include <cstdio>
#include <unordered_set>
#include <vector>
 
#include "aerogpu_d3d9_objects.h"
 
namespace aerogpu {
namespace {
 
bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}
 
bool CheckEqU32(uint32_t got, uint32_t expected, const char* what) {
  if (got != expected) {
    std::fprintf(stderr, "FAIL: %s: expected %u (0x%08X), got %u (0x%08X)\n",
                 what, expected, expected, got, got);
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
 
bool CreateAdapterAndDevice(CleanupDevice* cleanup) {
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
  return true;
}
 
bool TryCreateSurface(CleanupDevice* cleanup, uint32_t format, uint32_t usage, bool expect_success) {
  if (!cleanup || !cleanup->has_device) {
    return false;
  }
  if (!Check(cleanup->device_funcs.pfnCreateResource != nullptr, "pfnCreateResource is available")) {
    return false;
  }
 
  D3D9DDIARG_CREATERESOURCE create_res{};
  create_res.type = 3u; // D3DRTYPE_TEXTURE (metadata only in AeroGPU today)
  create_res.format = format;
  create_res.width = 4;
  create_res.height = 4;
  create_res.depth = 1;
  create_res.mip_levels = 1;
  create_res.usage = usage;
  create_res.pool = 0; // D3DPOOL_DEFAULT
  create_res.size = 0;
  create_res.hResource.pDrvPrivate = nullptr;
  create_res.pSharedHandle = nullptr;
  create_res.pPrivateDriverData = nullptr;
  create_res.PrivateDriverDataSize = 0;
  create_res.wddm_hAllocation = 0;
 
  const HRESULT hr = cleanup->device_funcs.pfnCreateResource(cleanup->hDevice, &create_res);
 
  if (expect_success) {
    if (!Check(hr == S_OK, "CreateResource expected S_OK")) {
      return false;
    }
    if (!Check(create_res.hResource.pDrvPrivate != nullptr, "CreateResource returned hResource")) {
      return false;
    }
    cleanup->resources.push_back(create_res.hResource);
    return true;
  }
 
  // Failure path: ensure we did not accidentally succeed.
  if (!Check(hr == D3DERR_INVALIDCALL, "CreateResource expected D3DERR_INVALIDCALL")) {
    if (create_res.hResource.pDrvPrivate && cleanup->device_funcs.pfnDestroyResource) {
      cleanup->device_funcs.pfnDestroyResource(cleanup->hDevice, create_res.hResource);
    }
    return false;
  }
  if (create_res.hResource.pDrvPrivate && cleanup->device_funcs.pfnDestroyResource) {
    cleanup->device_funcs.pfnDestroyResource(cleanup->hDevice, create_res.hResource);
  }
  return true;
}
 
bool TestCapsFormatContract() {
  CleanupDevice cleanup;
  if (!CreateAdapterAndDevice(&cleanup)) {
    return false;
  }
 
  if (!Check(cleanup.adapter_funcs.pfnGetCaps != nullptr, "pfnGetCaps is available")) {
    return false;
  }
 
  // ---- Device caps invariants -------------------------------------------------
  D3DCAPS9 caps{};
  D3D9DDIARG_GETCAPS get_caps{};
  get_caps.Type = D3DDDICAPS_GETD3D9CAPS;
  get_caps.pData = &caps;
  get_caps.DataSize = sizeof(caps);
 
  HRESULT hr = cleanup.adapter_funcs.pfnGetCaps(cleanup.hAdapter, &get_caps);
  if (!Check(hr == S_OK, "GetCaps(GETD3D9CAPS)")) {
    return false;
  }
 
  if (!CheckEqU32(static_cast<uint32_t>(caps.DeviceType), D3DDEVTYPE_HAL, "caps.DeviceType")) {
    return false;
  }
  if (!CheckEqU32(static_cast<uint32_t>(caps.AdapterOrdinal), 0u, "caps.AdapterOrdinal")) {
    return false;
  }
  if (!Check((caps.Caps2 & D3DCAPS2_CANRENDERWINDOWED) != 0, "Caps2 includes CANRENDERWINDOWED")) {
    return false;
  }
  if (!Check((caps.Caps2 & D3DCAPS2_CANSHARERESOURCE) != 0, "Caps2 includes CANSHARERESOURCE")) {
    return false;
  }
  if (!Check(caps.VertexShaderVersion >= D3DVS_VERSION(2, 0), "VertexShaderVersion >= 2.0")) {
    return false;
  }
  if (!Check(caps.PixelShaderVersion >= D3DPS_VERSION(2, 0), "PixelShaderVersion >= 2.0")) {
    return false;
  }
 
  // Keep these conservative; they must match the implementation's internal
  // register cache sizes.
  if (!CheckEqU32(static_cast<uint32_t>(caps.MaxVertexShaderConst), 256u, "caps.MaxVertexShaderConst")) {
    return false;
  }
 
  if (!CheckEqU32(static_cast<uint32_t>(caps.MaxTextureWidth), 4096u, "caps.MaxTextureWidth")) {
    return false;
  }
  if (!CheckEqU32(static_cast<uint32_t>(caps.MaxTextureHeight), 4096u, "caps.MaxTextureHeight")) {
    return false;
  }
  if (!CheckEqU32(static_cast<uint32_t>(caps.MaxVolumeExtent), 0u, "caps.MaxVolumeExtent")) {
    return false;
  }

  // Fixed-function fallback supports FVFs with TEX1, so FVFCaps must advertise at
  // least one texture coordinate set.
  const uint32_t fvf_texcoord_count = caps.FVFCaps & D3DFVFCAPS_TEXCOORDCOUNTMASK;
  if (!Check(fvf_texcoord_count >= 1u, "FVFCaps supports at least TEX1")) {
    return false;
  }
  if (!Check(fvf_texcoord_count <= 8u, "FVFCaps texcoord count <= 8")) {
    return false;
  }

  // Patch/N-patch caps must remain conservative: the UMD only implements a
  // limited rect/tri patch subset and does not expose N-patch/quintic patches.
  const uint32_t forbidden_patch_caps = D3DDEVCAPS_NPATCHES | D3DDEVCAPS_QUINTICRTPATCHES;
  if (!Check((caps.DevCaps & forbidden_patch_caps) == 0, "DevCaps does not advertise NPatch/quintic patch support")) {
    return false;
  }
  // Regardless of whether RTPATCHES are advertised, keep the max tessellation
  // level finite and within the UMD's CPU tessellation clamp.
  if (!Check(caps.MaxNpatchTessellationLevel == caps.MaxNpatchTessellationLevel, "MaxNpatchTessellationLevel is not NaN")) {
    return false;
  }
  if (!Check(caps.MaxNpatchTessellationLevel >= 0.0f && caps.MaxNpatchTessellationLevel <= 64.0f,
             "MaxNpatchTessellationLevel within [0, 64]")) {
    return false;
  }
  if ((caps.DevCaps & D3DDEVCAPS_RTPATCHES) != 0) {
    if (!Check(caps.MaxNpatchTessellationLevel > 0.0f, "MaxNpatchTessellationLevel > 0 when RTPATCHES is advertised")) {
      return false;
    }
  }
 
  // ---- Format enumeration invariants -----------------------------------------
  uint32_t format_count = 0;
  D3D9DDIARG_GETCAPS get_fmt_count{};
  get_fmt_count.Type = D3DDDICAPS_GETFORMATCOUNT;
  get_fmt_count.pData = &format_count;
  get_fmt_count.DataSize = sizeof(format_count);
  hr = cleanup.adapter_funcs.pfnGetCaps(cleanup.hAdapter, &get_fmt_count);
  if (!Check(hr == S_OK, "GetCaps(GETFORMATCOUNT)")) {
    return false;
  }
  if (!Check(format_count > 0, "format_count > 0")) {
    return false;
  }
  if (!Check(format_count <= 64u, "format_count is not unbounded")) {
    return false;
  }
 
  struct GetFormatPayload {
    uint32_t index;
    uint32_t format;
    uint32_t ops;
  };
 
  constexpr uint32_t kD3DUsageRenderTarget = 0x00000001u;
  constexpr uint32_t kD3DUsageDepthStencil = 0x00000002u;
  constexpr uint32_t kAllowedOpsBits = kD3DUsageRenderTarget | kD3DUsageDepthStencil;
 
  std::unordered_set<uint32_t> seen_formats;
  for (uint32_t i = 0; i < format_count; ++i) {
    GetFormatPayload payload{};
    payload.index = i;
 
    D3D9DDIARG_GETCAPS get_fmt{};
    get_fmt.Type = D3DDDICAPS_GETFORMAT;
    get_fmt.pData = &payload;
    get_fmt.DataSize = sizeof(payload);
    hr = cleanup.adapter_funcs.pfnGetCaps(cleanup.hAdapter, &get_fmt);
    if (!Check(hr == S_OK, "GetCaps(GETFORMAT)")) {
      return false;
    }
 
    if (!Check(payload.format != 0, "GETFORMAT returns non-zero D3DFORMAT")) {
      return false;
    }
    if (!Check(seen_formats.insert(payload.format).second, "GETFORMAT does not return duplicates")) {
      return false;
    }
 
    // The optional ops mask must remain conservative and must not include
    // catch-all bits ("all formats supported" style values).
    if (!Check((payload.ops & ~kAllowedOpsBits) == 0, "format ops mask only uses known bits")) {
      return false;
    }
    if (!Check(payload.ops == 0u || payload.ops == kD3DUsageRenderTarget || payload.ops == kD3DUsageDepthStencil,
               "format ops mask is 0 / RenderTarget / DepthStencil")) {
      return false;
    }
 
    const uint32_t agpu_format = d3d9_format_to_aerogpu(payload.format);
    if (!Check(agpu_format != AEROGPU_FORMAT_INVALID, "advertised format maps to valid aerogpu_format")) {
      return false;
    }
 
    Texture2dLayout layout{};
    if (!Check(calc_texture2d_layout(static_cast<D3DDDIFORMAT>(payload.format), 4, 4, 1, 1, &layout),
               "layout calculation succeeds for advertised format")) {
      return false;
    }
    if (!Check(layout.total_size_bytes != 0, "layout total_size_bytes != 0")) {
      return false;
    }
 
    // If a format is advertised as supporting a usage, the CreateResource path
    // must accept that exact combination.
    const bool expect_usage0_ok = (payload.ops != kD3DUsageDepthStencil);
    if (!TryCreateSurface(&cleanup, payload.format, /*usage=*/0u, expect_usage0_ok)) {
      return false;
    }
    if (!TryCreateSurface(&cleanup,
                          payload.format,
                          /*usage=*/kD3DUsageRenderTarget,
                          /*expect_success=*/payload.ops == kD3DUsageRenderTarget)) {
      return false;
    }
    if (!TryCreateSurface(&cleanup,
                          payload.format,
                          /*usage=*/kD3DUsageDepthStencil,
                          /*expect_success=*/payload.ops == kD3DUsageDepthStencil)) {
      return false;
    }
  }
 
  return true;
}
 
} // namespace
} // namespace aerogpu
 
int main() {
  return aerogpu::TestCapsFormatContract() ? 0 : 1;
}
