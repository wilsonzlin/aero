#include <cstdint>
#include <cstdio>

#include "aerogpu_wddm_alloc.h"

// The legacy D3D9->DXGI shared-surface fixup helper lives in the D3D10/11 UMD.
#include "aerogpu_legacy_d3d9_format_fixup.h"

namespace {

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

bool CheckFixup(uint32_t d3d9_format, uint32_t expected_dxgi_format, uint32_t expected_bpp) {
  constexpr uint32_t kWidth = 13;
  constexpr uint32_t kHeight = 7;

  aerogpu_wddm_alloc_priv_v2 priv{};
  priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
  priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION_2;
  priv.kind = AEROGPU_WDDM_ALLOC_KIND_UNKNOWN;
  priv.reserved0 = AEROGPU_WDDM_ALLOC_PRIV_DESC_PACK(d3d9_format, kWidth, kHeight);

  if (!Check(aerogpu::shared_surface::FixupLegacyPrivForOpenResource(&priv), "FixupLegacyPrivForOpenResource")) {
    return false;
  }
  if (!Check(priv.kind == AEROGPU_WDDM_ALLOC_KIND_TEXTURE2D, "fixup kind=TEXTURE2D")) {
    return false;
  }
  if (!Check(priv.width == kWidth, "fixup width")) {
    return false;
  }
  if (!Check(priv.height == kHeight, "fixup height")) {
    return false;
  }
  if (!Check(priv.format == expected_dxgi_format, "fixup dxgi format")) {
    return false;
  }
  const uint32_t expected_pitch = kWidth * expected_bpp;
  if (!Check(priv.row_pitch_bytes == expected_pitch, "fixup row_pitch_bytes")) {
    return false;
  }
  return true;
}

} // namespace

int main() {
  // Reuse the shared-surface fixup helper's numeric constants to keep tests and
  // UMD fixup logic in sync.
  using aerogpu::shared_surface::kDxgiFormatB5G6R5Unorm;
  using aerogpu::shared_surface::kDxgiFormatB5G5R5A1Unorm;
  using aerogpu::shared_surface::kD3d9FmtR5G6B5;
  using aerogpu::shared_surface::kD3d9FmtX1R5G5B5;
  using aerogpu::shared_surface::kD3d9FmtA1R5G5B5;

  if (!CheckFixup(kD3d9FmtR5G6B5, kDxgiFormatB5G6R5Unorm, /*bpp=*/2)) {
    return 1;
  }
  if (!CheckFixup(kD3d9FmtA1R5G5B5, kDxgiFormatB5G5R5A1Unorm, /*bpp=*/2)) {
    return 1;
  }
  if (!CheckFixup(kD3d9FmtX1R5G5B5, kDxgiFormatB5G5R5A1Unorm, /*bpp=*/2)) {
    return 1;
  }

  return 0;
}
