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

inline const wchar_t* FeatureBitIndexToName(uint32_t bit_index) {
  switch (bit_index) {
    case 0:
      return L"fence_page";
    case 1:
      return L"cursor";
    case 2:
      return L"scanout";
    case 3:
      return L"vblank";
    case 4:
      return L"transfer";
    default:
      return nullptr;
  }
}

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

  for (uint32_t bit = 0; bit < 128; ++bit) {
    const bool is_set = (bit < 64) ? ((features_lo & (1ull << bit)) != 0ull)
                                   : ((features_hi & (1ull << (bit - 64))) != 0ull);
    if (!is_set) {
      continue;
    }

    const wchar_t* known = FeatureBitIndexToName(bit);
    if (known) {
      out.push_back(known);
      continue;
    }

    out.push_back(std::wstring(L"unknown_bit_") + Uint32ToWstring(bit));
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
