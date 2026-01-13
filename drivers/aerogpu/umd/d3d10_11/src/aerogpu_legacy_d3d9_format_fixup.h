// Shared-surface interop helpers: legacy D3D9 shared-surface descriptors.
//
// Some Win7-era D3D9 OpenResource paths do not provide enough information to
// reconstruct a shared surface's format/width/height. AeroGPU works around this
// by encoding a minimal D3D9 surface descriptor into the preserved WDDM
// allocation private data blob (`aerogpu_wddm_alloc_priv.reserved0` via
// `AEROGPU_WDDM_ALLOC_PRIV_DESC_*` macros).
//
// The D3D10/D3D10.1/D3D11 UMDs need to "fix up" these legacy descriptors when
// opening such a shared allocation.
#pragma once

#include <cstdint>

#include "../../../protocol/aerogpu_wddm_alloc.h"

namespace aerogpu::shared_surface {

// D3D9 D3DFORMAT subset (numeric values from d3d9types.h). We intentionally
// avoid including D3D9 headers so this helper stays portable.
constexpr uint32_t kD3d9FmtA8R8G8B8 = 21;   // D3DFMT_A8R8G8B8
constexpr uint32_t kD3d9FmtX8R8G8B8 = 22;   // D3DFMT_X8R8G8B8
constexpr uint32_t kD3d9FmtR5G6B5 = 23;     // D3DFMT_R5G6B5
constexpr uint32_t kD3d9FmtX1R5G5B5 = 24;   // D3DFMT_X1R5G5B5
constexpr uint32_t kD3d9FmtA1R5G5B5 = 25;   // D3DFMT_A1R5G5B5
constexpr uint32_t kD3d9FmtA8B8G8R8 = 32;   // D3DFMT_A8B8G8R8
constexpr uint32_t kD3d9FmtX8B8G8R8 = 33;   // D3DFMT_X8B8G8R8

// DXGI_FORMAT subset (numeric values from dxgiformat.h).
constexpr uint32_t kDxgiFormatR8G8B8A8Unorm = 28;     // DXGI_FORMAT_R8G8B8A8_UNORM
constexpr uint32_t kDxgiFormatB5G6R5Unorm = 85;       // DXGI_FORMAT_B5G6R5_UNORM
constexpr uint32_t kDxgiFormatB5G5R5A1Unorm = 86;     // DXGI_FORMAT_B5G5R5A1_UNORM
constexpr uint32_t kDxgiFormatB8G8R8A8Unorm = 87;     // DXGI_FORMAT_B8G8R8A8_UNORM
constexpr uint32_t kDxgiFormatB8G8R8X8Unorm = 88;     // DXGI_FORMAT_B8G8R8X8_UNORM

inline bool D3d9FormatToDxgi(uint32_t d3d9_format, uint32_t* dxgi_format_out, uint32_t* bpp_out) {
  if (dxgi_format_out) {
    *dxgi_format_out = 0;
  }
  if (bpp_out) {
    *bpp_out = 0;
  }
  if (!dxgi_format_out || !bpp_out) {
    return false;
  }

  switch (d3d9_format) {
    case kD3d9FmtA8R8G8B8:
      *dxgi_format_out = kDxgiFormatB8G8R8A8Unorm;
      *bpp_out = 4;
      return true;
    case kD3d9FmtX8R8G8B8:
      *dxgi_format_out = kDxgiFormatB8G8R8X8Unorm;
      *bpp_out = 4;
      return true;
    case kD3d9FmtR5G6B5:
      *dxgi_format_out = kDxgiFormatB5G6R5Unorm;
      *bpp_out = 2;
      return true;
    case kD3d9FmtA1R5G5B5:
      *dxgi_format_out = kDxgiFormatB5G5R5A1Unorm;
      *bpp_out = 2;
      return true;
    case kD3d9FmtX1R5G5B5:
      // DXGI has no X1 variant; treat as B5G5R5A1 and rely on bind flags /
      // sampling conventions to ignore alpha when needed.
      *dxgi_format_out = kDxgiFormatB5G5R5A1Unorm;
      *bpp_out = 2;
      return true;
    case kD3d9FmtA8B8G8R8:
      *dxgi_format_out = kDxgiFormatR8G8B8A8Unorm;
      *bpp_out = 4;
      return true;
    case kD3d9FmtX8B8G8R8:
      // DXGI has no X8 variant; treat as UNORM and rely on bind flags / sampling
      // conventions to ignore alpha when needed.
      *dxgi_format_out = kDxgiFormatR8G8B8A8Unorm;
      *bpp_out = 4;
      return true;
    default:
      return false;
  }
}

inline bool FixupLegacyPrivForOpenResource(aerogpu_wddm_alloc_priv_v2* priv) {
  if (!priv) {
    return false;
  }
  if (priv->kind != AEROGPU_WDDM_ALLOC_KIND_UNKNOWN) {
    return true;
  }

  if (AEROGPU_WDDM_ALLOC_PRIV_DESC_PRESENT(priv->reserved0)) {
    const uint32_t d3d9_format = static_cast<uint32_t>(AEROGPU_WDDM_ALLOC_PRIV_DESC_FORMAT(priv->reserved0));
    const uint32_t width = static_cast<uint32_t>(AEROGPU_WDDM_ALLOC_PRIV_DESC_WIDTH(priv->reserved0));
    const uint32_t height = static_cast<uint32_t>(AEROGPU_WDDM_ALLOC_PRIV_DESC_HEIGHT(priv->reserved0));
    if (width == 0 || height == 0) {
      return false;
    }

    uint32_t dxgi_format = 0;
    uint32_t bpp = 0;
    if (!D3d9FormatToDxgi(d3d9_format, &dxgi_format, &bpp)) {
      return false;
    }

    const uint64_t row_pitch = static_cast<uint64_t>(width) * static_cast<uint64_t>(bpp);
    if (row_pitch == 0 || row_pitch > 0xFFFFFFFFull) {
      return false;
    }

    priv->kind = AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D;
    priv->width = width;
    priv->height = height;
    priv->format = dxgi_format;
    priv->row_pitch_bytes = static_cast<uint32_t>(row_pitch);
    return true;
  }

  // If no descriptor marker is present, treat legacy v1 blobs as generic buffers.
  if (priv->size_bytes != 0) {
    priv->kind = AEROGPU_WDDM_ALLOC_KIND_BUFFER;
    return true;
  }

  return false;
}

} // namespace aerogpu::shared_surface

