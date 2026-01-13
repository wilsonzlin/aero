#include <cstdint>
#include <cstdio>

#include "aerogpu_d3d9_objects.h"
#include "aerogpu_pci.h"

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
    std::fprintf(stderr, "FAIL: %s: expected 0x%08X, got 0x%08X\n", what, expected, got);
    return false;
  }
  return true;
}

bool CheckEqU64(uint64_t got, uint64_t expected, const char* what) {
  if (got != expected) {
    std::fprintf(stderr, "FAIL: %s: expected %llu, got %llu\n", what,
                 static_cast<unsigned long long>(expected), static_cast<unsigned long long>(got));
    return false;
  }
  return true;
}

bool TestFormatMapping() {
  bool ok = true;

  // Numeric D3DFMT_* values from d3d9types.h. Kept local so portable builds don't
  // need the Windows SDK/WDK headers.
  constexpr uint32_t kD3dFmtA8R8G8B8 = 21u;
  constexpr uint32_t kD3dFmtX8R8G8B8 = 22u;
  constexpr uint32_t kD3dFmtA8B8G8R8 = 32u;
  constexpr uint32_t kD3dFmtD24S8 = 75u;

  ok &= CheckEqU32(aerogpu::d3d9_format_to_aerogpu(kD3dFmtA8R8G8B8), AEROGPU_FORMAT_B8G8R8A8_UNORM,
                   "d3d9_format_to_aerogpu(D3DFMT_A8R8G8B8)");
  ok &= CheckEqU32(aerogpu::d3d9_format_to_aerogpu(kD3dFmtX8R8G8B8), AEROGPU_FORMAT_B8G8R8X8_UNORM,
                   "d3d9_format_to_aerogpu(D3DFMT_X8R8G8B8)");
  ok &= CheckEqU32(aerogpu::d3d9_format_to_aerogpu(kD3dFmtA8B8G8R8), AEROGPU_FORMAT_R8G8B8A8_UNORM,
                   "d3d9_format_to_aerogpu(D3DFMT_A8B8G8R8)");
  ok &= CheckEqU32(aerogpu::d3d9_format_to_aerogpu(kD3dFmtD24S8), AEROGPU_FORMAT_D24_UNORM_S8_UINT,
                   "d3d9_format_to_aerogpu(D3DFMT_D24S8)");

  ok &= CheckEqU32(aerogpu::d3d9_format_to_aerogpu(static_cast<uint32_t>(aerogpu::kD3dFmtDxt1)),
                   AEROGPU_FORMAT_BC1_RGBA_UNORM, "d3d9_format_to_aerogpu(D3DFMT_DXT1)");
  ok &= CheckEqU32(aerogpu::d3d9_format_to_aerogpu(static_cast<uint32_t>(aerogpu::kD3dFmtDxt3)),
                   AEROGPU_FORMAT_BC2_RGBA_UNORM, "d3d9_format_to_aerogpu(D3DFMT_DXT3)");
  ok &= CheckEqU32(aerogpu::d3d9_format_to_aerogpu(static_cast<uint32_t>(aerogpu::kD3dFmtDxt5)),
                   AEROGPU_FORMAT_BC3_RGBA_UNORM, "d3d9_format_to_aerogpu(D3DFMT_DXT5)");

  // Premultiplied-alpha variants should map to the same BC formats.
  ok &= CheckEqU32(aerogpu::d3d9_format_to_aerogpu(static_cast<uint32_t>(aerogpu::kD3dFmtDxt2)),
                   AEROGPU_FORMAT_BC2_RGBA_UNORM, "d3d9_format_to_aerogpu(D3DFMT_DXT2)");
  ok &= CheckEqU32(aerogpu::d3d9_format_to_aerogpu(static_cast<uint32_t>(aerogpu::kD3dFmtDxt4)),
                   AEROGPU_FORMAT_BC3_RGBA_UNORM, "d3d9_format_to_aerogpu(D3DFMT_DXT4)");

  ok &= CheckEqU32(aerogpu::d3d9_format_to_aerogpu(/*unknown*/ 0u), AEROGPU_FORMAT_INVALID,
                   "d3d9_format_to_aerogpu(unknown)");

  // Optional 16-bit formats. If supported, ensure they map to the expected
  // AeroGPU protocol formats.
  constexpr uint32_t kD3dFmtR5G6B5 = 23u;
  constexpr uint32_t kD3dFmtX1R5G5B5 = 24u;
  constexpr uint32_t kD3dFmtA1R5G5B5 = 25u;

  const uint32_t r5g6b5 = aerogpu::d3d9_format_to_aerogpu(kD3dFmtR5G6B5);
  if (r5g6b5 != AEROGPU_FORMAT_INVALID) {
    ok &= CheckEqU32(r5g6b5, AEROGPU_FORMAT_B5G6R5_UNORM, "d3d9_format_to_aerogpu(D3DFMT_R5G6B5)");
  }

  const uint32_t x1r5g5b5 = aerogpu::d3d9_format_to_aerogpu(kD3dFmtX1R5G5B5);
  if (x1r5g5b5 != AEROGPU_FORMAT_INVALID) {
    ok &= CheckEqU32(x1r5g5b5, AEROGPU_FORMAT_B5G5R5A1_UNORM, "d3d9_format_to_aerogpu(D3DFMT_X1R5G5B5)");
  }

  const uint32_t a1r5g5b5 = aerogpu::d3d9_format_to_aerogpu(kD3dFmtA1R5G5B5);
  if (a1r5g5b5 != AEROGPU_FORMAT_INVALID) {
    ok &= CheckEqU32(a1r5g5b5, AEROGPU_FORMAT_B5G5R5A1_UNORM, "d3d9_format_to_aerogpu(D3DFMT_A1R5G5B5)");
  }

  return ok;
}

bool TestBytesPerPixel() {
  bool ok = true;

  constexpr D3DDDIFORMAT kD3dFmtA8R8G8B8 = static_cast<D3DDDIFORMAT>(21u);
  constexpr D3DDDIFORMAT kD3dFmtX8R8G8B8 = static_cast<D3DDDIFORMAT>(22u);
  constexpr D3DDDIFORMAT kD3dFmtA8B8G8R8 = static_cast<D3DDDIFORMAT>(32u);
  constexpr D3DDDIFORMAT kD3dFmtA8 = static_cast<D3DDDIFORMAT>(28u);
  constexpr D3DDDIFORMAT kD3dFmtD24S8 = static_cast<D3DDDIFORMAT>(75u);

  ok &= CheckEqU32(aerogpu::bytes_per_pixel(kD3dFmtA8R8G8B8), 4u, "bytes_per_pixel(D3DFMT_A8R8G8B8)");
  ok &= CheckEqU32(aerogpu::bytes_per_pixel(kD3dFmtX8R8G8B8), 4u, "bytes_per_pixel(D3DFMT_X8R8G8B8)");
  ok &= CheckEqU32(aerogpu::bytes_per_pixel(kD3dFmtA8B8G8R8), 4u, "bytes_per_pixel(D3DFMT_A8B8G8R8)");
  ok &= CheckEqU32(aerogpu::bytes_per_pixel(kD3dFmtA8), 1u, "bytes_per_pixel(D3DFMT_A8)");
  ok &= CheckEqU32(aerogpu::bytes_per_pixel(kD3dFmtD24S8), 4u, "bytes_per_pixel(D3DFMT_D24S8)");

  // Optional 16-bit formats (only enforced when the driver supports them).
  constexpr uint32_t kD3dFmtR5G6B5 = 23u;
  constexpr uint32_t kD3dFmtX1R5G5B5 = 24u;
  constexpr uint32_t kD3dFmtA1R5G5B5 = 25u;

  if (aerogpu::d3d9_format_to_aerogpu(kD3dFmtR5G6B5) != AEROGPU_FORMAT_INVALID) {
    ok &= CheckEqU32(aerogpu::bytes_per_pixel(static_cast<D3DDDIFORMAT>(kD3dFmtR5G6B5)), 2u,
                     "bytes_per_pixel(D3DFMT_R5G6B5)");
  }
  if (aerogpu::d3d9_format_to_aerogpu(kD3dFmtX1R5G5B5) != AEROGPU_FORMAT_INVALID) {
    ok &= CheckEqU32(aerogpu::bytes_per_pixel(static_cast<D3DDDIFORMAT>(kD3dFmtX1R5G5B5)), 2u,
                     "bytes_per_pixel(D3DFMT_X1R5G5B5)");
  }
  if (aerogpu::d3d9_format_to_aerogpu(kD3dFmtA1R5G5B5) != AEROGPU_FORMAT_INVALID) {
    ok &= CheckEqU32(aerogpu::bytes_per_pixel(static_cast<D3DDDIFORMAT>(kD3dFmtA1R5G5B5)), 2u,
                     "bytes_per_pixel(D3DFMT_A1R5G5B5)");
  }

  return ok;
}

bool TestTextureLayout() {
  bool ok = true;

  aerogpu::Texture2dLayout layout{};

  // 4x4 RGBA8, 1 mip level.
  ok &= Check(aerogpu::calc_texture2d_layout(static_cast<D3DDDIFORMAT>(21u), 4, 4, 1, 1, &layout),
              "calc_texture2d_layout(4x4, mip=1) returns true");
  ok &= CheckEqU32(layout.row_pitch_bytes, 16u, "layout.row_pitch_bytes (4x4 RGBA8)");
  ok &= CheckEqU32(layout.slice_pitch_bytes, 64u, "layout.slice_pitch_bytes (4x4 RGBA8)");
  ok &= CheckEqU64(layout.total_size_bytes, 64u, "layout.total_size_bytes (4x4 RGBA8)");

  // 8x8 RGBA8 mip chain with 4 mips: 8x8 + 4x4 + 2x2 + 1x1.
  layout = {};
  ok &= Check(aerogpu::calc_texture2d_layout(static_cast<D3DDDIFORMAT>(21u), 8, 8, 4, 1, &layout),
              "calc_texture2d_layout(8x8, mip=4) returns true");
  ok &= CheckEqU32(layout.row_pitch_bytes, 32u, "layout.row_pitch_bytes (8x8 RGBA8)");
  ok &= CheckEqU32(layout.slice_pitch_bytes, 256u, "layout.slice_pitch_bytes (8x8 RGBA8)");
  ok &= CheckEqU64(layout.total_size_bytes, 340u, "layout.total_size_bytes (8x8 RGBA8 mip chain)");

  // BC1 layout uses 4x4 blocks; dimensions are rounded up to whole blocks.
  // 5x5 -> 2x2 blocks, 8 bytes per block.
  layout = {};
  ok &= Check(aerogpu::calc_texture2d_layout(aerogpu::kD3dFmtDxt1, 5, 5, 1, 1, &layout),
              "calc_texture2d_layout(BC1 5x5, mip=1) returns true");
  ok &= CheckEqU32(layout.row_pitch_bytes, 16u, "layout.row_pitch_bytes (BC1 5x5)");
  ok &= CheckEqU32(layout.slice_pitch_bytes, 32u, "layout.slice_pitch_bytes (BC1 5x5)");
  ok &= CheckEqU64(layout.total_size_bytes, 32u, "layout.total_size_bytes (BC1 5x5)");

  return ok;
}

} // namespace

int main() {
  bool ok = true;
  ok &= TestFormatMapping();
  ok &= TestBytesPerPixel();
  ok &= TestTextureLayout();
  return ok ? 0 : 1;
}

