#pragma once

#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstdint>
#if !defined(_WIN32)
  #include <random>
#endif

#include "../include/aerogpu_d3d9_umd.h"

namespace aerogpu {

// Allocates 64-bit share tokens for D3D9Ex shared surfaces (EXPORT/IMPORT_SHARED_SURFACE).
//
// These tokens must be collision-resistant across the entire guest (multi-process) because
// the host maintains a global (share_token -> resource) map with no awareness of guest
// process boundaries.
//
// The token value itself is persisted in the WDDM allocation private driver data
// (aerogpu_wddm_alloc_priv.share_token), which dxgkrnl preserves and returns on
// OpenResource/OpenAllocation so other processes can IMPORT using the same token.
class ShareTokenAllocator {
 public:
  ShareTokenAllocator() = default;

  void set_adapter_luid(const LUID& luid) {
    adapter_luid_ = luid;
  }

  uint64_t allocate_share_token();

 private:
  // SplitMix64 mixing function (public domain). Used to scramble fallback entropy.
  static uint64_t splitmix64(uint64_t x) {
    x += 0x9E3779B97F4A7C15ULL;
    x = (x ^ (x >> 30)) * 0xBF58476D1CE4E5B9ULL;
    x = (x ^ (x >> 27)) * 0x94D049BB133111EBULL;
    return x ^ (x >> 31);
  }

  static bool fill_random_bytes(void* out, size_t len);
  uint64_t fallback_entropy(uint64_t counter) const;

  LUID adapter_luid_ = {};
  std::atomic<uint64_t> counter_{1};
};

// -----------------------------------------------------------------------------
// Implementation
// -----------------------------------------------------------------------------

inline bool ShareTokenAllocator::fill_random_bytes(void* out, size_t len) {
  if (!out || len == 0) {
    return false;
  }

#if defined(_WIN32)
  using RtlGenRandomFn = BOOLEAN(WINAPI*)(PVOID, ULONG);
  using BCryptGenRandomFn = LONG(WINAPI*)(void* hAlgorithm, unsigned char* pbBuffer, ULONG cbBuffer, ULONG dwFlags);

  static RtlGenRandomFn rtl_gen_random = []() -> RtlGenRandomFn {
    HMODULE advapi = GetModuleHandleW(L"advapi32.dll");
    if (!advapi) {
      advapi = LoadLibraryW(L"advapi32.dll");
    }
    if (!advapi) {
      return nullptr;
    }
    return reinterpret_cast<RtlGenRandomFn>(GetProcAddress(advapi, "SystemFunction036"));
  }();

  if (rtl_gen_random) {
    if (rtl_gen_random(out, static_cast<ULONG>(len)) != FALSE) {
      return true;
    }
  }

  static BCryptGenRandomFn bcrypt_gen_random = []() -> BCryptGenRandomFn {
    HMODULE bcrypt = GetModuleHandleW(L"bcrypt.dll");
    if (!bcrypt) {
      bcrypt = LoadLibraryW(L"bcrypt.dll");
    }
    if (!bcrypt) {
      return nullptr;
    }
    return reinterpret_cast<BCryptGenRandomFn>(GetProcAddress(bcrypt, "BCryptGenRandom"));
  }();

  if (bcrypt_gen_random) {
    constexpr ULONG kBcryptUseSystemPreferredRng = 0x00000002UL; // BCRYPT_USE_SYSTEM_PREFERRED_RNG
    const LONG st = bcrypt_gen_random(nullptr,
                                     reinterpret_cast<unsigned char*>(out),
                                     static_cast<ULONG>(len),
                                     kBcryptUseSystemPreferredRng);
    if (st >= 0) {
      return true;
    }
  }

  return false;
#else
  // Portable builds (unit tests) don't have access to the Win32 crypto RNG APIs;
  // use std::random_device as a best-effort entropy source.
  try {
    std::random_device rd;
    auto* dst = static_cast<uint8_t*>(out);
    size_t i = 0;
    while (i < len) {
      const unsigned int r = rd();
      for (size_t b = 0; b < sizeof(r) && i < len; ++b, ++i) {
        dst[i] = static_cast<uint8_t>((r >> (b * 8)) & 0xFFu);
      }
    }
    return true;
  } catch (...) {
    return false;
  }
#endif
}

inline uint64_t ShareTokenAllocator::fallback_entropy(uint64_t counter) const {
  const uint64_t luid64 =
      (static_cast<uint64_t>(static_cast<uint32_t>(adapter_luid_.HighPart)) << 32) |
      static_cast<uint64_t>(adapter_luid_.LowPart);

  uint64_t entropy = counter ^ splitmix64(luid64);

#if defined(_WIN32)
  entropy ^= (static_cast<uint64_t>(GetCurrentProcessId()) << 32);
  entropy ^= static_cast<uint64_t>(GetCurrentThreadId());

  LARGE_INTEGER qpc{};
  if (QueryPerformanceCounter(&qpc)) {
    entropy ^= static_cast<uint64_t>(qpc.QuadPart);
  }

  entropy ^= static_cast<uint64_t>(GetTickCount64());
#else
  entropy ^= static_cast<uint64_t>(
      std::chrono::steady_clock::now().time_since_epoch().count());
  entropy ^= static_cast<uint64_t>(reinterpret_cast<uintptr_t>(this));
#endif

  return entropy;
}

inline uint64_t ShareTokenAllocator::allocate_share_token() {
  // Prefer a cryptographically strong RNG when available so tokens are extremely
  // unlikely to collide across processes and across time.
  uint64_t token = 0;
  for (int attempt = 0; attempt < 8; ++attempt) {
    if (!fill_random_bytes(&token, sizeof(token))) {
      break;
    }
    if (token != 0) {
      return token;
    }
  }

  // Fallback: Mix per-process entropy into a 64-bit value and scramble it via
  // SplitMix64. The atomic counter ensures different calls in the same process
  // never reuse the same input value.
  for (;;) {
    const uint64_t ctr = counter_.fetch_add(1, std::memory_order_relaxed);
    token = splitmix64(fallback_entropy(ctr));
    if (token != 0) {
      return token;
    }
  }
}

} // namespace aerogpu
