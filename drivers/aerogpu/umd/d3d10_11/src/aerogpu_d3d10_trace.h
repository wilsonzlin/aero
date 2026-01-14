#pragma once

// Lightweight D3D10DDI tracing helpers (Win7 bring-up).
//
// This is intentionally "zero dependency" (no ETW, no CRT I/O beyond vsnprintf).
// When enabled, logs are emitted via OutputDebugStringA so they can be captured
// with Sysinternals DebugView on a Windows 7 VM.
//
// Enable at compile time:
//   /DAEROGPU_D3D10_TRACE=1
//
// Enable at runtime (when compiled in):
//   set AEROGPU_D3D10_TRACE=1   (high level)
//   set AEROGPU_D3D10_TRACE=2   (verbose: includes per-draw/state calls)

#ifndef AEROGPU_D3D10_TRACE
  #define AEROGPU_D3D10_TRACE 0
#endif

#if AEROGPU_D3D10_TRACE
  #include <atomic>
  #include <cstdarg>
  #include <cstdint>
  #include <cstdio>
  #include <cstring>
  #include <mutex>

  #if defined(_WIN32)
    #ifndef WIN32_LEAN_AND_MEAN
      #define WIN32_LEAN_AND_MEAN 1
    #endif
    #include <windows.h>
  #endif

namespace aerogpu::d3d10trace {

inline int level() noexcept {
  // 0 = disabled, 1 = default, 2 = verbose.
  static std::atomic<int> cached{0};
  int v = cached.load(std::memory_order_acquire);
  if (v != 0) {
    return v - 1;
  }

  int lvl = 0;
#if defined(_WIN32)
  char buf[32] = {};
  DWORD n = GetEnvironmentVariableA("AEROGPU_D3D10_TRACE", buf, static_cast<DWORD>(sizeof(buf)));
  if (n > 0 && n < sizeof(buf)) {
    if (buf[0] == '0') {
      lvl = 0;
    } else if (buf[0] >= '0' && buf[0] <= '9') {
      lvl = buf[0] - '0';
      if (lvl < 0) lvl = 0;
      if (lvl > 9) lvl = 9;
    } else {
      // Any non-empty non-numeric value enables default tracing.
      lvl = 1;
    }
  }
#endif
  // cached stores +1 so that 0 means "uninitialized".
  cached.store(lvl + 1, std::memory_order_release);
  return lvl;
}

inline void vlogf(const char* fmt, va_list args) noexcept {
  if (level() <= 0) {
    return;
  }

  // This tracing path can be called from error handling and during bring-up.
  // Keep it best-effort so it cannot throw across ABI boundaries (e.g. via
  // std::mutex lock failures).
  try {
    static std::mutex g_mu;
    static std::atomic<uint64_t> g_seq{0};

    std::lock_guard<std::mutex> lock(g_mu);

    const uint64_t seq = g_seq.fetch_add(1, std::memory_order_relaxed);

    uint32_t tid = 0;
    uint32_t ms = 0;
#if defined(_WIN32)
    tid = static_cast<uint32_t>(GetCurrentThreadId());
    ms = static_cast<uint32_t>(GetTickCount());
#endif

    char buf[2048];
    int prefix = std::snprintf(buf, sizeof(buf), "[AeroGPU:D3D10 t=%u tid=%u #%llu] ",
                               ms,
                               tid,
                               static_cast<unsigned long long>(seq));
    if (prefix < 0) {
      return;
    }

    size_t off = static_cast<size_t>(prefix);
    if (off >= sizeof(buf)) {
      off = sizeof(buf) - 1;
    }

    int wrote = std::vsnprintf(buf + off, sizeof(buf) - off, fmt, args);
    if (wrote < 0) {
      return;
    }

    // Ensure newline termination so DebugView doesn't concatenate unrelated lines.
    size_t len = std::strlen(buf);
    if (len == 0 || buf[len - 1] != '\n') {
      if (len + 1 < sizeof(buf)) {
        buf[len] = '\n';
        buf[len + 1] = '\0';
      } else {
        buf[sizeof(buf) - 2] = '\n';
        buf[sizeof(buf) - 1] = '\0';
      }
    }

#if defined(_WIN32)
    OutputDebugStringA(buf);
#else
    std::fputs(buf, stderr);
#endif
  } catch (...) {
  }
}

inline void logf(const char* fmt, ...) noexcept {
  va_list args;
  va_start(args, fmt);
  vlogf(fmt, args);
  va_end(args);
}

inline HRESULT ret_hr(const char* func, HRESULT hr) noexcept {
  if (level() <= 0) {
    return hr;
  }
  // Use a stable format so callers can grep for the function name and result.
  logf("%s -> hr=0x%08X", func, static_cast<uint32_t>(hr));
  return hr;
}

} // namespace aerogpu::d3d10trace

  #define AEROGPU_D3D10_TRACEF(...) ::aerogpu::d3d10trace::logf(__VA_ARGS__)
  #define AEROGPU_D3D10_TRACEF_VERBOSE(...) \
    do {                                    \
      if (::aerogpu::d3d10trace::level() >= 2) { \
        ::aerogpu::d3d10trace::logf(__VA_ARGS__); \
      }                                     \
    } while (0)
  #define AEROGPU_D3D10_RET_HR(hr) return ::aerogpu::d3d10trace::ret_hr(__func__, (hr))
#else
  #define AEROGPU_D3D10_TRACEF(...) ((void)0)
  #define AEROGPU_D3D10_TRACEF_VERBOSE(...) ((void)0)
  #define AEROGPU_D3D10_RET_HR(hr) return (hr)
#endif
