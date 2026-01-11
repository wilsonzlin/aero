#include "aerogpu_d3d9_caps.h"

#include <atomic>
#include <cstring>

#include "aerogpu_log.h"

#include "aerogpu_pci.h"

#if defined(_WIN32)
  #include <d3d9.h>
#endif

namespace aerogpu {
namespace {

constexpr uint32_t kSupportedFormats[] = {
    22u, // D3DFMT_X8R8G8B8
    21u, // D3DFMT_A8R8G8B8
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
  out->SubSysId = AEROGPU_PCI_SUBSYSTEM_ID;
  out->Revision = 0;

  out->DeviceIdentifier = make_aerogpu_adapter_guid();
  out->WHQLLevel = 0;
}

#endif // _WIN32

} // namespace

HRESULT get_caps(Adapter*, const AEROGPU_D3D9DDIARG_GETCAPS* pGetCaps) {
  if (!pGetCaps) {
    return E_INVALIDARG;
  }

  logf("aerogpu-d3d9: GetCaps type=%u data_size=%u\n", pGetCaps->type, pGetCaps->data_size);

  if (!pGetCaps->pData || pGetCaps->data_size == 0) {
    return E_INVALIDARG;
  }

#if defined(_WIN32)
  const uint32_t format_count =
      static_cast<uint32_t>(sizeof(kSupportedFormats) / sizeof(kSupportedFormats[0]));

  if (pGetCaps->data_size >= sizeof(D3DCAPS9)) {
    auto* caps = reinterpret_cast<D3DCAPS9*>(pGetCaps->pData);
    fill_d3d9_caps(caps);
    log_caps_once(*caps);
    return S_OK;
  }

  if (pGetCaps->data_size == sizeof(uint32_t)) {
    *reinterpret_cast<uint32_t*>(pGetCaps->pData) = format_count;
    return S_OK;
  }

  if (pGetCaps->data_size >= sizeof(GetMultisampleQualityLevelsPayload)) {
    auto* payload = reinterpret_cast<GetMultisampleQualityLevelsPayload*>(pGetCaps->pData);
    if (payload->format < format_count) {
      auto* fmt = reinterpret_cast<GetFormatPayload*>(pGetCaps->pData);
      fmt->format = kSupportedFormats[fmt->index];
      return S_OK;
    }

    payload->quality_levels = 0;
    return S_OK;
  }

  if (pGetCaps->data_size >= sizeof(GetMultisampleQualityLevelsPayloadV1)) {
    auto* payload = reinterpret_cast<GetMultisampleQualityLevelsPayloadV1*>(pGetCaps->pData);
    if (payload->format < format_count) {
      auto* fmt = reinterpret_cast<GetFormatPayload*>(pGetCaps->pData);
      fmt->format = kSupportedFormats[fmt->index];
      return S_OK;
    }

    payload->quality_levels = 0;
    return S_OK;
  }

  if (pGetCaps->data_size >= sizeof(GetFormatPayload)) {
    auto* payload = reinterpret_cast<GetFormatPayload*>(pGetCaps->pData);
    const uint32_t idx = payload->index;
    if (idx >= format_count) {
      return E_INVALIDARG;
    }
    payload->format = kSupportedFormats[idx];
    return S_OK;
  }

  logf("aerogpu-d3d9: GetCaps unsupported payload (type=%u size=%u)\n", pGetCaps->type, pGetCaps->data_size);
  return E_INVALIDARG;
#else
  (void)kSupportedFormats;
  return E_NOTIMPL;
#endif
}

HRESULT query_adapter_info(Adapter*, const AEROGPU_D3D9DDIARG_QUERYADAPTERINFO* pQueryAdapterInfo) {
  if (!pQueryAdapterInfo) {
    return E_INVALIDARG;
  }

  logf("aerogpu-d3d9: QueryAdapterInfo type=%u size=%u\n",
       pQueryAdapterInfo->type,
       pQueryAdapterInfo->private_driver_data_size);

  if (!pQueryAdapterInfo->pPrivateDriverData || pQueryAdapterInfo->private_driver_data_size == 0) {
    return E_INVALIDARG;
  }

#if defined(_WIN32)
  if (pQueryAdapterInfo->private_driver_data_size >= sizeof(D3DADAPTER_IDENTIFIER9)) {
    fill_adapter_identifier(reinterpret_cast<D3DADAPTER_IDENTIFIER9*>(pQueryAdapterInfo->pPrivateDriverData));
    return S_OK;
  }

  if (pQueryAdapterInfo->private_driver_data_size == sizeof(GUID)) {
    *reinterpret_cast<GUID*>(pQueryAdapterInfo->pPrivateDriverData) = make_aerogpu_adapter_guid();
    return S_OK;
  }
#endif

  logf("aerogpu-d3d9: QueryAdapterInfo unsupported query (type=%u size=%u)\n",
       pQueryAdapterInfo->type,
       pQueryAdapterInfo->private_driver_data_size);
  return E_INVALIDARG;
}

} // namespace aerogpu
