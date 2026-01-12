#include "aerogpu_d3d9_caps.h"

#include <atomic>
#include <array>
#include <cstdio>
#include <cstring>
#include <mutex>

#include "aerogpu_d3d9_objects.h"
#include "aerogpu_log.h"

#include "aerogpu_pci.h"

#if defined(_WIN32)
  #include <d3d9.h>
#endif

namespace aerogpu {
namespace {

constexpr uint32_t kBaseSupportedFormats[] = {
    22u, // D3DFMT_X8R8G8B8
    21u, // D3DFMT_A8R8G8B8
    32u, // D3DFMT_A8B8G8R8
    75u, // D3DFMT_D24S8
};

constexpr uint32_t kBcSupportedFormats[] = {
    static_cast<uint32_t>(kD3dFmtDxt1), // D3DFMT_DXT1
    static_cast<uint32_t>(kD3dFmtDxt2), // D3DFMT_DXT2
    static_cast<uint32_t>(kD3dFmtDxt3), // D3DFMT_DXT3
    static_cast<uint32_t>(kD3dFmtDxt4), // D3DFMT_DXT4
    static_cast<uint32_t>(kD3dFmtDxt5), // D3DFMT_DXT5
};

struct GetFormatPayload {
  uint32_t index;
  uint32_t format;
};

struct GetMultisampleQualityLevelsPayload {
  uint32_t format;
  uint32_t multisample_type;
  uint32_t flags;
  uint32_t quality_levels;
};

struct GetMultisampleQualityLevelsPayloadV1 {
  uint32_t format;
  uint32_t multisample_type;
  uint32_t quality_levels;
};

std::atomic<bool> g_logged_caps_once{false};

constexpr uint32_t kD3DUsageRenderTarget = 0x00000001u;
constexpr uint32_t kD3DUsageDepthStencil = 0x00000002u;

void log_unknown_get_caps_once(uint32_t type, uint32_t size) {
  constexpr uint32_t kMaxSeen = 16;
  static std::mutex mutex;
  static uint32_t seen[kMaxSeen] = {};
  static uint32_t seen_count = 0;
  static bool overflow_logged = false;

  std::lock_guard<std::mutex> lock(mutex);
  for (uint32_t i = 0; i < seen_count; ++i) {
    if (seen[i] == type) {
      return;
    }
  }

  if (seen_count < kMaxSeen) {
    seen[seen_count++] = type;
    logf("aerogpu-d3d9: GetCaps unknown type=%u size=%u\n", type, size);
    return;
  }

  if (!overflow_logged) {
    overflow_logged = true;
    logf("aerogpu-d3d9: GetCaps unknown type=%u size=%u (suppressing further unknown caps logs)\n",
         type,
         size);
  }
}

void log_unknown_query_adapter_info_once(uint32_t type, uint32_t size) {
  constexpr uint32_t kMaxSeen = 16;
  static std::mutex mutex;
  static uint32_t seen[kMaxSeen] = {};
  static uint32_t seen_count = 0;
  static bool overflow_logged = false;

  std::lock_guard<std::mutex> lock(mutex);
  for (uint32_t i = 0; i < seen_count; ++i) {
    if (seen[i] == type) {
      return;
    }
  }

  if (seen_count < kMaxSeen) {
    seen[seen_count++] = type;
    logf("aerogpu-d3d9: QueryAdapterInfo unknown type=%u size=%u\n", type, size);
    return;
  }

  if (!overflow_logged) {
    overflow_logged = true;
    logf("aerogpu-d3d9: QueryAdapterInfo unknown type=%u size=%u (suppressing further unknown adapter-info logs)\n",
         type,
         size);
  }
}

uint32_t format_ops_for_d3d9_format(uint32_t format) {
  switch (format) {
    case 75u: // D3DFMT_D24S8
      return kD3DUsageDepthStencil;
    case static_cast<uint32_t>(kD3dFmtDxt1):
    case static_cast<uint32_t>(kD3dFmtDxt2):
    case static_cast<uint32_t>(kD3dFmtDxt3):
    case static_cast<uint32_t>(kD3dFmtDxt4):
    case static_cast<uint32_t>(kD3dFmtDxt5):
      // Compressed texture formats cannot be used as render targets or depth/stencil.
      return 0u;
    default:
      return kD3DUsageRenderTarget;
  }
}

bool supports_bc_formats(const Adapter* adapter) {
#if defined(_WIN32)
  if (!adapter || !adapter->umd_private_valid) {
    return false;
  }
  const uint32_t major = adapter->umd_private.device_abi_version_u32 >> 16;
  const uint32_t minor = adapter->umd_private.device_abi_version_u32 & 0xFFFFu;
  return (major == AEROGPU_ABI_MAJOR) && (minor >= 2u);
#else
  (void)adapter;
  return true;
#endif
}

bool is_supported_format(const Adapter* adapter, uint32_t format) {
  for (uint32_t f : kBaseSupportedFormats) {
    if (f == format) {
      return true;
    }
  }
  if (supports_bc_formats(adapter)) {
    for (uint32_t f : kBcSupportedFormats) {
      if (f == format) {
        return true;
      }
    }
  }
  return false;
}

#if defined(_WIN32)

GUID make_aerogpu_adapter_guid() {
  GUID g{};
  g.Data1 = 0x5f84f5ae;
  g.Data2 = 0x6c2b;
  g.Data3 = 0x4c3f;
  g.Data4[0] = 0x8b;
  g.Data4[1] = 0x6f;
  g.Data4[2] = 0x5e;
  g.Data4[3] = 0x7d;
  g.Data4[4] = 0x3c;
  g.Data4[5] = 0x3a;
  g.Data4[6] = 0x27;
  g.Data4[7] = 0xb1;
  return g;
}

void fill_d3d9_caps(D3DCAPS9* out) {
  if (!out) {
    return;
  }

  std::memset(out, 0, sizeof(*out));

  out->DeviceType = D3DDEVTYPE_HAL;
  out->AdapterOrdinal = 0;

  out->DevCaps = D3DDEVCAPS_HWTRANSFORMANDLIGHT;

  out->Caps2 = D3DCAPS2_CANRENDERWINDOWED | D3DCAPS2_CANSHARERESOURCE;

  out->PresentationIntervals = D3DPRESENT_INTERVAL_ONE | D3DPRESENT_INTERVAL_IMMEDIATE;

  out->VertexShaderVersion = D3DVS_VERSION(2, 0);
  out->PixelShaderVersion = D3DPS_VERSION(2, 0);
  out->MaxVertexShaderConst = 256;

  out->PrimitiveMiscCaps = D3DPMISCCAPS_CLIPTLVERTS;

  out->RasterCaps = D3DPRASTERCAPS_SCISSORTEST;

  out->AlphaCmpCaps = D3DPCMPCAPS_ALWAYS;

  out->SrcBlendCaps = D3DPBLENDCAPS_ZERO | D3DPBLENDCAPS_ONE | D3DPBLENDCAPS_SRCALPHA | D3DPBLENDCAPS_INVSRCALPHA;
  out->DestBlendCaps = out->SrcBlendCaps;

  out->ShadeCaps = D3DPSHADECAPS_COLORGOURAUDRGB;

  out->TextureFilterCaps = D3DPTFILTERCAPS_MINFPOINT | D3DPTFILTERCAPS_MINFLINEAR |
                           D3DPTFILTERCAPS_MAGFPOINT | D3DPTFILTERCAPS_MAGFLINEAR |
                           D3DPTFILTERCAPS_MIPFPOINT | D3DPTFILTERCAPS_MIPFLINEAR;

  out->StretchRectFilterCaps = D3DPTFILTERCAPS_MINFPOINT | D3DPTFILTERCAPS_MINFLINEAR |
                               D3DPTFILTERCAPS_MAGFPOINT | D3DPTFILTERCAPS_MAGFLINEAR;

  out->TextureAddressCaps = D3DPTADDRESSCAPS_CLAMP | D3DPTADDRESSCAPS_WRAP;

  out->TextureCaps = D3DPTEXTURECAPS_ALPHA;

  out->MaxTextureWidth = 4096;
  out->MaxTextureHeight = 4096;
  out->MaxVolumeExtent = 256;

  out->MaxTextureRepeat = 8192;
  out->MaxTextureAspectRatio = 8192;
  out->MaxAnisotropy = 1;
  out->MaxVertexW = 1e10f;

  out->MaxSimultaneousTextures = 8;
  out->MaxTextureBlendStages = 8;
  out->MaxStreams = 16;
  out->MaxStreamStride = 2048;

  out->MaxPrimitiveCount = 0xFFFFFu;
  out->MaxVertexIndex = 0xFFFFFu;

  out->DeclTypes = D3DDTCAPS_FLOAT1 | D3DDTCAPS_FLOAT2 | D3DDTCAPS_FLOAT3 | D3DDTCAPS_FLOAT4 | D3DDTCAPS_D3DCOLOR |
                   D3DDTCAPS_UBYTE4 | D3DDTCAPS_UBYTE4N | D3DDTCAPS_SHORT2 | D3DDTCAPS_SHORT4 | D3DDTCAPS_SHORT2N |
                   D3DDTCAPS_SHORT4N | D3DDTCAPS_USHORT2N | D3DDTCAPS_USHORT4N;

  out->NumSimultaneousRTs = 1;

  out->VS20Caps.Caps = 0;
  out->VS20Caps.DynamicFlowControlDepth = 0;
  out->VS20Caps.NumTemps = 32;
  out->VS20Caps.StaticFlowControlDepth = 0;

  out->PS20Caps.Caps = 0;
  out->PS20Caps.DynamicFlowControlDepth = 0;
  out->PS20Caps.NumTemps = 32;
  out->PS20Caps.StaticFlowControlDepth = 0;
  out->PS20Caps.NumInstructionSlots = 512;

  out->PixelShader1xMaxValue = 1.0f;
}

void log_caps_once(const D3DCAPS9& caps) {
  const bool already = g_logged_caps_once.exchange(true);
  if (already) {
    return;
  }

  logf("aerogpu-d3d9: caps summary: VS=0x%08lX PS=0x%08lX MaxTex=%lux%lu Caps2=0x%08lX\n",
       (unsigned long)caps.VertexShaderVersion,
       (unsigned long)caps.PixelShaderVersion,
       (unsigned long)caps.MaxTextureWidth,
       (unsigned long)caps.MaxTextureHeight,
       (unsigned long)caps.Caps2);
  logf("aerogpu-d3d9: caps bits: RasterCaps=0x%08lX TextureCaps=0x%08lX TextureFilterCaps=0x%08lX\n",
       (unsigned long)caps.RasterCaps,
       (unsigned long)caps.TextureCaps,
       (unsigned long)caps.TextureFilterCaps);
  logf("aerogpu-d3d9: caps blend: SrcBlendCaps=0x%08lX DestBlendCaps=0x%08lX StretchRectFilterCaps=0x%08lX\n",
       (unsigned long)caps.SrcBlendCaps,
       (unsigned long)caps.DestBlendCaps,
       (unsigned long)caps.StretchRectFilterCaps);
}

void fill_adapter_identifier(D3DADAPTER_IDENTIFIER9* out) {
  if (!out) {
    return;
  }

  std::memset(out, 0, sizeof(*out));

  strncpy_s(out->Driver, "aerogpu_d3d9", _TRUNCATE);
  strncpy_s(out->Description, "AeroGPU D3D9Ex (WDDM 1.1)", _TRUNCATE);
  strncpy_s(out->DeviceName, "\\\\.\\DISPLAY1", _TRUNCATE);

  out->DriverVersion.HighPart = (0u << 16) | 0u;
  out->DriverVersion.LowPart = (1u << 16) | 0u;

  out->VendorId = AEROGPU_PCI_VENDOR_ID;
  out->DeviceId = AEROGPU_PCI_DEVICE_ID;
  out->SubSysId = (AEROGPU_PCI_SUBSYSTEM_VENDOR_ID << 16) | AEROGPU_PCI_SUBSYSTEM_ID;
  out->Revision = 0;

  out->DeviceIdentifier = make_aerogpu_adapter_guid();
  out->WHQLLevel = 0;
}

#endif // _WIN32

} // namespace

HRESULT get_caps(Adapter* adapter, const D3D9DDIARG_GETCAPS* pGetCaps) {
  if (!pGetCaps) {
    return E_INVALIDARG;
  }

  const uint32_t base_format_count =
      static_cast<uint32_t>(sizeof(kBaseSupportedFormats) / sizeof(kBaseSupportedFormats[0]));
  const bool bc_supported = supports_bc_formats(adapter);
  const uint32_t bc_format_count =
      bc_supported ? static_cast<uint32_t>(sizeof(kBcSupportedFormats) / sizeof(kBcSupportedFormats[0])) : 0u;
  const uint32_t format_count = base_format_count + bc_format_count;

  if (!pGetCaps->pData || pGetCaps->DataSize == 0) {
    return E_INVALIDARG;
  }

  switch (static_cast<D3DDDICAPS_TYPE>(pGetCaps->Type)) {
    case D3DDDICAPS_GETD3D9CAPS: {
      if (pGetCaps->DataSize < sizeof(D3DCAPS9)) {
        return E_INVALIDARG;
      }
#if defined(_WIN32)
      auto* caps = reinterpret_cast<D3DCAPS9*>(pGetCaps->pData);
      fill_d3d9_caps(caps);
      log_caps_once(*caps);
      return S_OK;
#else
      auto* caps = reinterpret_cast<D3DCAPS9*>(pGetCaps->pData);
      std::memset(caps, 0, sizeof(*caps));
      caps->Caps2 = D3DCAPS2_CANRENDERWINDOWED | D3DCAPS2_CANSHARERESOURCE;
      caps->RasterCaps = D3DPRASTERCAPS_SCISSORTEST;
      caps->TextureFilterCaps = D3DPTFILTERCAPS_MINFPOINT | D3DPTFILTERCAPS_MINFLINEAR | D3DPTFILTERCAPS_MAGFPOINT |
                                D3DPTFILTERCAPS_MAGFLINEAR;
      caps->StretchRectFilterCaps = caps->TextureFilterCaps;
      caps->SrcBlendCaps = D3DPBLENDCAPS_ZERO | D3DPBLENDCAPS_ONE | D3DPBLENDCAPS_SRCALPHA | D3DPBLENDCAPS_INVSRCALPHA;
      caps->DestBlendCaps = caps->SrcBlendCaps;
      caps->MaxTextureWidth = 4096;
      caps->MaxTextureHeight = 4096;
      caps->MaxVolumeExtent = 256;
      caps->MaxSimultaneousTextures = 8;
      caps->MaxStreams = 16;
      caps->VertexShaderVersion = D3DVS_VERSION(2, 0);
      caps->PixelShaderVersion = D3DPS_VERSION(2, 0);
      caps->MaxVertexShaderConst = 256;
      caps->PresentationIntervals = D3DPRESENT_INTERVAL_ONE | D3DPRESENT_INTERVAL_IMMEDIATE;
      caps->NumSimultaneousRTs = 1;
      caps->VS20Caps.NumTemps = 32;
      caps->PS20Caps.NumTemps = 32;
      caps->PixelShader1xMaxValue = 1.0f;
      return S_OK;
#endif
    }
    case D3DDDICAPS_GETFORMATCOUNT: {
      if (pGetCaps->DataSize < sizeof(uint32_t)) {
        return E_INVALIDARG;
      }
      *reinterpret_cast<uint32_t*>(pGetCaps->pData) = format_count;
      return S_OK;
    }
    case D3DDDICAPS_GETFORMAT: {
      if (pGetCaps->DataSize < sizeof(GetFormatPayload)) {
        return E_INVALIDARG;
      }
      auto* payload = reinterpret_cast<GetFormatPayload*>(pGetCaps->pData);
      if (payload->index >= format_count) {
        return E_INVALIDARG;
      }
      uint32_t format = 0;
      if (payload->index < base_format_count) {
        format = kBaseSupportedFormats[payload->index];
      } else {
        const uint32_t bc_index = payload->index - base_format_count;
        if (!bc_supported || bc_index >= bc_format_count) {
          return E_INVALIDARG;
        }
        format = kBcSupportedFormats[bc_index];
      }
      payload->format = format;

      // Best-effort: if the payload has room for a third uint32_t field, fill it
      // with a conservative ops/usage mask so the runtime can distinguish render
      // targets from depth/stencil formats.
      if (pGetCaps->DataSize >= 3u * sizeof(uint32_t)) {
        auto* fields = reinterpret_cast<uint32_t*>(pGetCaps->pData);
        fields[2] = format_ops_for_d3d9_format(format);
      }
      return S_OK;
    }
    case D3DDDICAPS_GETMULTISAMPLEQUALITYLEVELS: {
      if (pGetCaps->DataSize >= sizeof(GetMultisampleQualityLevelsPayload)) {
        auto* payload = reinterpret_cast<GetMultisampleQualityLevelsPayload*>(pGetCaps->pData);
        const bool supported = is_supported_format(adapter, payload->format) && (format_ops_for_d3d9_format(payload->format) != 0u);
        payload->quality_levels = (supported && payload->multisample_type == 0u) ? 1u : 0u;
        return S_OK;
      }
      if (pGetCaps->DataSize >= sizeof(GetMultisampleQualityLevelsPayloadV1)) {
        auto* payload = reinterpret_cast<GetMultisampleQualityLevelsPayloadV1*>(pGetCaps->pData);
        const bool supported = is_supported_format(adapter, payload->format) && (format_ops_for_d3d9_format(payload->format) != 0u);
        payload->quality_levels = (supported && payload->multisample_type == 0u) ? 1u : 0u;
        return S_OK;
      }
      return E_INVALIDARG;
    }
    default:
      log_unknown_get_caps_once(pGetCaps->Type, pGetCaps->DataSize);
      // Be permissive: unknown caps types should not break DWM/device bring-up.
      // Return a zeroed buffer to signal "no extra capabilities" rather than
      // failing the call.
      std::memset(pGetCaps->pData, 0, pGetCaps->DataSize);
      return S_OK;
  }
}

HRESULT query_adapter_info(Adapter* adapter, const D3D9DDIARG_QUERYADAPTERINFO* pQueryAdapterInfo) {
  if (!adapter) {
    return E_INVALIDARG;
  }
  if (!pQueryAdapterInfo) {
    return E_INVALIDARG;
  }

  if (!pQueryAdapterInfo->pPrivateDriverData || pQueryAdapterInfo->PrivateDriverDataSize == 0) {
    return E_INVALIDARG;
  }

  switch (static_cast<D3DDDI_QUERYADAPTERINFO_TYPE>(pQueryAdapterInfo->Type)) {
    case D3DDDIQUERYADAPTERINFO_GETADAPTERIDENTIFIER: {
      if (pQueryAdapterInfo->PrivateDriverDataSize < sizeof(D3DADAPTER_IDENTIFIER9)) {
        return E_INVALIDARG;
      }
#if defined(_WIN32)
      fill_adapter_identifier(reinterpret_cast<D3DADAPTER_IDENTIFIER9*>(pQueryAdapterInfo->pPrivateDriverData));
      return S_OK;
#else
      auto* ident = reinterpret_cast<D3DADAPTER_IDENTIFIER9*>(pQueryAdapterInfo->pPrivateDriverData);
      std::memset(ident, 0, sizeof(*ident));
      std::snprintf(ident->Driver, sizeof(ident->Driver), "%s", "aerogpu_d3d9");
      std::snprintf(ident->Description, sizeof(ident->Description), "%s", "AeroGPU D3D9Ex (portable)");
      std::snprintf(ident->DeviceName, sizeof(ident->DeviceName), "%s", "\\\\.\\DISPLAY1");
      ident->VendorId = AEROGPU_PCI_VENDOR_ID;
      ident->DeviceId = AEROGPU_PCI_DEVICE_ID;
      ident->SubSysId = (AEROGPU_PCI_SUBSYSTEM_VENDOR_ID << 16) | AEROGPU_PCI_SUBSYSTEM_ID;
      return S_OK;
#endif
    }
    case D3DDDIQUERYADAPTERINFO_GETADAPTERLUID: {
      if (pQueryAdapterInfo->PrivateDriverDataSize < sizeof(LUID)) {
        return E_INVALIDARG;
      }
      *reinterpret_cast<LUID*>(pQueryAdapterInfo->pPrivateDriverData) = adapter->luid;
      return S_OK;
    }
    default:
#if defined(_WIN32)
      if (pQueryAdapterInfo->PrivateDriverDataSize == sizeof(GUID)) {
        *reinterpret_cast<GUID*>(pQueryAdapterInfo->pPrivateDriverData) = make_aerogpu_adapter_guid();
        return S_OK;
      }
#endif
      log_unknown_query_adapter_info_once(pQueryAdapterInfo->Type, pQueryAdapterInfo->PrivateDriverDataSize);
      // Be permissive: unknown adapter-info queries should not break D3D9Ex/DWM
      // bring-up. Return a zeroed buffer to signal "no extra data" rather than
      // failing the call.
      std::memset(pQueryAdapterInfo->pPrivateDriverData, 0, pQueryAdapterInfo->PrivateDriverDataSize);
      return S_OK;
  }
}

} // namespace aerogpu
