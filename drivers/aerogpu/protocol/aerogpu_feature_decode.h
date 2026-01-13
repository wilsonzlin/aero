#pragma once

/*
 * AeroGPU device feature bit decoding helpers (C++).
 *
 * This header is intentionally separate from the stable C ABI headers (e.g.
 * `aerogpu_pci.h`) so host-side unit tests and small bring-up tools can
 * consistently render FEATURES_LO/HI as human-readable names.
 *
 * - Known bits are mapped to stable, lowercase names.
 * - Unknown bits are rendered as `unknown_bit_<n>` where <n> is the bit index
 *   in the 128-bit (lo/hi) feature set.
 *
 * Note: This file provides C++ helpers only. Including it from C is a no-op.
 */

#ifdef __cplusplus

#include <stddef.h>
#include <stdint.h>

#include <string>
#include <vector>

namespace aerogpu {

inline std::wstring Uint32ToWstring(uint32_t v) {
  // VS2010 does not provide std::to_wstring; implement a tiny conversion.
  wchar_t buf[32];
  wchar_t* p = buf + (sizeof(buf) / sizeof(buf[0]));
  *--p = 0;
  do {
    *--p = (wchar_t)(L'0' + (v % 10u));
    v /= 10u;
  } while (v != 0u);
  return std::wstring(p);
}

inline std::vector<std::wstring> DecodeDeviceFeatureBits(uint64_t features_lo, uint64_t features_hi) {
  std::vector<std::wstring> out;

  // Print known features first in a stable, user-oriented order.
  // (This order is intentionally not the same as numeric bit order.)
  struct KnownFeature {
    uint32_t bit_index;
    const wchar_t* name;
  };

  static const KnownFeature kKnown[] = {
      {1, L"cursor"},
      {2, L"scanout"},
      {3, L"vblank"},
      {0, L"fence_page"},
      {4, L"transfer"},
      {5, L"error_info"},
  };

  uint64_t known_mask_lo = 0;
  uint64_t known_mask_hi = 0;
  for (size_t i = 0; i < (sizeof(kKnown) / sizeof(kKnown[0])); ++i) {
    const uint32_t bit = kKnown[i].bit_index;
    if (bit < 64) {
      known_mask_lo |= (1ull << bit);
    } else {
      known_mask_hi |= (1ull << (bit - 64));
    }
  }

  for (size_t i = 0; i < (sizeof(kKnown) / sizeof(kKnown[0])); ++i) {
    const uint32_t bit = kKnown[i].bit_index;
    const bool is_set = (bit < 64) ? ((features_lo & (1ull << bit)) != 0ull)
                                   : ((features_hi & (1ull << (bit - 64))) != 0ull);
    if (is_set) {
      out.push_back(kKnown[i].name);
    }
  }

  // Append any set-but-unknown bits as `unknown_bit_<n>` (numeric bit index),
  // ordered by increasing bit index.
  const uint64_t unknown_lo = features_lo & ~known_mask_lo;
  const uint64_t unknown_hi = features_hi & ~known_mask_hi;
  for (uint32_t bit = 0; bit < 64; ++bit) {
    if ((unknown_lo & (1ull << bit)) != 0ull) {
      out.push_back(std::wstring(L"unknown_bit_") + Uint32ToWstring(bit));
    }
  }
  for (uint32_t bit = 0; bit < 64; ++bit) {
    if ((unknown_hi & (1ull << bit)) != 0ull) {
      out.push_back(std::wstring(L"unknown_bit_") + Uint32ToWstring(64u + bit));
    }
  }

  return out;
}

inline std::wstring FormatDeviceFeatureBits(uint64_t features_lo, uint64_t features_hi) {
  const std::vector<std::wstring> names = DecodeDeviceFeatureBits(features_lo, features_hi);
  if (names.empty()) {
    return L"(none)";
  }

  std::wstring out;
  for (size_t i = 0; i < names.size(); ++i) {
    if (i) {
      out += L", ";
    }
    out += names[i];
  }
  return out;
}

}  // namespace aerogpu

#endif  // __cplusplus
