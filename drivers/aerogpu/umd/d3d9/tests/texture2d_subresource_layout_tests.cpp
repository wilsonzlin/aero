#include <algorithm>
#include <cstdint>
#include <cstdio>
 
#include "aerogpu_d3d9_objects.h"
 
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
 
bool CalcExpectedPitch(D3DDDIFORMAT format, uint32_t w, uint32_t h, uint32_t* row_pitch, uint32_t* slice_pitch) {
  if (!row_pitch || !slice_pitch) {
    return false;
  }
  w = std::max(1u, w);
  h = std::max(1u, h);
 
  uint64_t row = 0;
  uint64_t slice = 0;
 
  if (aerogpu::is_block_compressed_format(format)) {
    const uint32_t block_bytes = aerogpu::block_bytes_per_4x4(format);
    if (block_bytes == 0) {
      return false;
    }
    const uint32_t blocks_w = std::max(1u, (w + 3u) / 4u);
    const uint32_t blocks_h = std::max(1u, (h + 3u) / 4u);
    row = static_cast<uint64_t>(blocks_w) * block_bytes;
    slice = row * blocks_h;
  } else {
    const uint32_t bpp = aerogpu::bytes_per_pixel(format);
    row = static_cast<uint64_t>(w) * bpp;
    slice = row * h;
  }
 
  if (row == 0 || slice == 0) {
    return false;
  }
  if (row > 0xFFFFFFFFull || slice > 0xFFFFFFFFull) {
    return false;
  }
 
  *row_pitch = static_cast<uint32_t>(row);
  *slice_pitch = static_cast<uint32_t>(slice);
  return true;
}
 
bool RunCase(const char* name,
             D3DDDIFORMAT format,
             uint32_t width,
             uint32_t height,
             uint32_t mip_levels,
             uint32_t array_layers) {
  bool ok = true;
 
  width = std::max(1u, width);
  height = std::max(1u, height);
  mip_levels = std::max(1u, mip_levels);
  array_layers = std::max(1u, array_layers);
 
  uint64_t offset = 0;
  for (uint32_t layer = 0; layer < array_layers; ++layer) {
    uint32_t w = width;
    uint32_t h = height;
 
    for (uint32_t mip = 0; mip < mip_levels; ++mip) {
      uint32_t exp_row = 0;
      uint32_t exp_slice = 0;
      if (!CalcExpectedPitch(format, w, h, &exp_row, &exp_slice)) {
        std::fprintf(stderr, "FAIL: %s: expected pitch calc failed layer=%u mip=%u\n", name, layer, mip);
        return false;
      }

      const uint64_t start = offset;
      const uint64_t end = start + static_cast<uint64_t>(exp_slice);
  
      {
        aerogpu::Texture2dSubresourceLayout got{};
        char msg[256] = {};
        std::snprintf(msg, sizeof(msg), "%s: calc_texture2d_subresource_layout_for_offset(layer=%u,mip=%u,start)",
                      name, layer, mip);
        ok &= Check(aerogpu::calc_texture2d_subresource_layout_for_offset(
                        format, width, height, mip_levels, array_layers, start, &got),
                    msg);
  
        char what_start[256] = {};
        std::snprintf(what_start, sizeof(what_start), "%s: subresource_start(layer=%u,mip=%u,start)", name, layer, mip);
        ok &= CheckEqU64(got.subresource_start_bytes, start, what_start);
  
        char what_end[256] = {};
        std::snprintf(what_end, sizeof(what_end), "%s: subresource_end(layer=%u,mip=%u,start)", name, layer, mip);
        ok &= CheckEqU64(got.subresource_end_bytes, end, what_end);
  
        char what_row[256] = {};
        std::snprintf(what_row, sizeof(what_row), "%s: row_pitch(layer=%u,mip=%u,start)", name, layer, mip);
        ok &= CheckEqU32(got.row_pitch_bytes, exp_row, what_row);
  
        char what_slice[256] = {};
        std::snprintf(what_slice, sizeof(what_slice), "%s: slice_pitch(layer=%u,mip=%u,start)", name, layer, mip);
        ok &= CheckEqU32(got.slice_pitch_bytes, exp_slice, what_slice);
      }
 
      // Offsets are derived from D3D9's OffsetToLock. Some runtimes can pass an
      // offset within the subresource; ensure we still match the correct mip's
      // pitches in that case.
      {
        aerogpu::Texture2dSubresourceLayout got{};
        const uint64_t within = start + static_cast<uint64_t>(exp_slice) / 2u;
        char msg[256] = {};
        std::snprintf(msg, sizeof(msg), "%s: calc_texture2d_subresource_layout_for_offset(layer=%u,mip=%u,within)",
                      name, layer, mip);
        ok &= Check(aerogpu::calc_texture2d_subresource_layout_for_offset(
                        format, width, height, mip_levels, array_layers, within, &got),
                    msg);
  
        char what_start[256] = {};
        std::snprintf(what_start, sizeof(what_start), "%s: subresource_start(layer=%u,mip=%u,within)", name, layer, mip);
        ok &= CheckEqU64(got.subresource_start_bytes, start, what_start);
  
        char what_end[256] = {};
        std::snprintf(what_end, sizeof(what_end), "%s: subresource_end(layer=%u,mip=%u,within)", name, layer, mip);
        ok &= CheckEqU64(got.subresource_end_bytes, end, what_end);
  
        char what_row[256] = {};
        std::snprintf(what_row, sizeof(what_row), "%s: row_pitch(layer=%u,mip=%u,within)", name, layer, mip);
        ok &= CheckEqU32(got.row_pitch_bytes, exp_row, what_row);
 
        char what_slice[256] = {};
        std::snprintf(what_slice, sizeof(what_slice), "%s: slice_pitch(layer=%u,mip=%u,within)", name, layer, mip);
        ok &= CheckEqU32(got.slice_pitch_bytes, exp_slice, what_slice);
      }
  
      // Validate boundary behavior: offset at the end of a subresource should
      // map to the *next* subresource (except for the final subresource, where
      // offset==total_size is out-of-bounds).
      const bool last_subresource = (layer + 1 == array_layers) && (mip + 1 == mip_levels);
      if (!last_subresource) {
        uint32_t next_w = 0;
        uint32_t next_h = 0;
        if (mip + 1 < mip_levels) {
          next_w = std::max(1u, w / 2);
          next_h = std::max(1u, h / 2);
        } else {
          next_w = width;
          next_h = height;
        }
  
        uint32_t exp_next_row = 0;
        uint32_t exp_next_slice = 0;
        if (!CalcExpectedPitch(format, next_w, next_h, &exp_next_row, &exp_next_slice)) {
          std::fprintf(stderr, "FAIL: %s: expected pitch calc failed at boundary layer=%u mip=%u\n", name, layer, mip);
          return false;
        }
  
        aerogpu::Texture2dSubresourceLayout got_next{};
        char msg[256] = {};
        std::snprintf(msg, sizeof(msg), "%s: boundary(end) maps to next subresource (layer=%u,mip=%u)", name, layer, mip);
        ok &= Check(aerogpu::calc_texture2d_subresource_layout_for_offset(
                        format, width, height, mip_levels, array_layers, end, &got_next),
                    msg);
  
        char what_start[256] = {};
        std::snprintf(what_start, sizeof(what_start), "%s: boundary subresource_start(layer=%u,mip=%u)", name, layer, mip);
        ok &= CheckEqU64(got_next.subresource_start_bytes, end, what_start);
  
        char what_end[256] = {};
        std::snprintf(what_end, sizeof(what_end), "%s: boundary subresource_end(layer=%u,mip=%u)", name, layer, mip);
        ok &= CheckEqU64(got_next.subresource_end_bytes, end + static_cast<uint64_t>(exp_next_slice), what_end);
  
        char what_row[256] = {};
        std::snprintf(what_row, sizeof(what_row), "%s: boundary row_pitch(layer=%u,mip=%u)", name, layer, mip);
        ok &= CheckEqU32(got_next.row_pitch_bytes, exp_next_row, what_row);
  
        char what_slice[256] = {};
        std::snprintf(what_slice, sizeof(what_slice), "%s: boundary slice_pitch(layer=%u,mip=%u)", name, layer, mip);
        ok &= CheckEqU32(got_next.slice_pitch_bytes, exp_next_slice, what_slice);
      }
  
      offset += exp_slice;
      w = std::max(1u, w / 2);
      h = std::max(1u, h / 2);
    }
  }
 
  aerogpu::Texture2dLayout layout{};
  ok &= Check(aerogpu::calc_texture2d_layout(format, width, height, mip_levels, array_layers, &layout),
              "calc_texture2d_layout returns true");
  ok &= CheckEqU64(layout.total_size_bytes, offset, "total_size_bytes matches packed subresource sum");
 
  aerogpu::Texture2dSubresourceLayout out_of_bounds{};
  ok &= Check(!aerogpu::calc_texture2d_subresource_layout_for_offset(
                  format, width, height, mip_levels, array_layers, offset, &out_of_bounds),
              "offset==total_size_bytes returns false");
  if (offset != UINT64_MAX) {
    ok &= Check(!aerogpu::calc_texture2d_subresource_layout_for_offset(
                    format, width, height, mip_levels, array_layers, offset + 1, &out_of_bounds),
                "offset>total_size_bytes returns false");
  }
  
  return ok;
}
 
} // namespace
 
int main() {
  bool ok = true;
 
  // Odd-size mip chain to validate clamp-to-1 behavior.
  // 7x5 RGBA8 with 6 mips => 7x5, 3x2, 1x1, 1x1, 1x1, 1x1.
  ok &= RunCase("RGBA8 7x5 mips=6 layers=3", static_cast<D3DDDIFORMAT>(21u), 7, 5, 6, 3);
 
  // BC1 uses 4x4 blocks; ensure pitch calculations follow block rounding for
  // both the base level and smaller mips.
  ok &= RunCase("BC1 7x5 mips=5 layers=2", aerogpu::kD3dFmtDxt1, 7, 5, 5, 2);

  // BC2 uses 16-byte blocks (DXT2/DXT3 family); verify the 16-byte block path
  // for the DXT3 format encoding.
  ok &= RunCase("BC2 7x5 mips=5 layers=2", aerogpu::kD3dFmtDxt3, 7, 5, 5, 2);

  // BC3 uses 16-byte blocks (DXT5 family); verify the 16-byte block path.
  ok &= RunCase("BC3 7x5 mips=5 layers=2", aerogpu::kD3dFmtDxt5, 7, 5, 5, 2);

  // Uncompressed 16-bit format: validate bytes-per-pixel pitch computation.
  ok &= RunCase("R5G6B5 13x7 mips=4 layers=2", static_cast<D3DDDIFORMAT>(23u), 13, 7, 4, 2);

  return ok ? 0 : 1;
}
 
