#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

#include "aerogpu_feature_decode.h"

namespace {

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

bool CheckEq(const std::wstring& got, const std::wstring& expected, const char* msg) {
  if (got != expected) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

}  // namespace

int main() {
  using aerogpu::DecodeDeviceFeatureBits;
  using aerogpu::FormatDeviceFeatureBits;

  if (!Check(DecodeDeviceFeatureBits(0, 0).empty(), "empty bitset decodes to empty list")) {
    return 1;
  }
  if (!CheckEq(FormatDeviceFeatureBits(0, 0), L"(none)", "empty bitset formats as (none)")) {
    return 1;
  }

  const uint64_t known = (1ull << 0) | (1ull << 2) | (1ull << 4);  // fence_page, scanout, transfer
  const std::vector<std::wstring> decoded_known = DecodeDeviceFeatureBits(known, 0);
  if (!Check(decoded_known.size() == 3, "known bits decode count")) {
    return 1;
  }
  if (!Check(decoded_known[0] == L"fence_page" && decoded_known[1] == L"scanout" && decoded_known[2] == L"transfer",
             "known bits decode names/order")) {
    return 1;
  }

  const uint64_t unknown_lo = (1ull << 7);
  if (!CheckEq(FormatDeviceFeatureBits(unknown_lo, 0), L"unknown_bit_7", "unknown low bit formats")) {
    return 1;
  }

  const uint64_t unknown_hi = (1ull << 0);  // overall bit 64
  if (!CheckEq(FormatDeviceFeatureBits(0, unknown_hi), L"unknown_bit_64", "unknown high bit formats")) {
    return 1;
  }

  const uint64_t mix_lo = (1ull << 1) | (1ull << 6);  // cursor + unknown_bit_6
  if (!CheckEq(FormatDeviceFeatureBits(mix_lo, unknown_hi), L"cursor, unknown_bit_6, unknown_bit_64",
               "mixed known/unknown formats in ascending bit order")) {
    return 1;
  }

  return 0;
}

