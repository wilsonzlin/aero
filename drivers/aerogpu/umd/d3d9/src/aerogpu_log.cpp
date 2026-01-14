#include "aerogpu_log.h"

#include <cstdio>
#include <mutex>

#if defined(_WIN32)
  #ifndef WIN32_LEAN_AND_MEAN
    #define WIN32_LEAN_AND_MEAN
  #endif
  #include <windows.h>
#endif

namespace aerogpu {

namespace {
std::mutex g_log_mutex;
}

void vlogf(const char* fmt, va_list args) noexcept {
  try {
    std::lock_guard<std::mutex> lock(g_log_mutex);

    char buf[2048];
    int n = vsnprintf(buf, sizeof(buf), fmt, args);
    if (n < 0) {
      return;
    }

#if defined(_WIN32)
    OutputDebugStringA(buf);
#else
    fputs(buf, stderr);
#endif
  } catch (...) {
  }
}

void logf(const char* fmt, ...) noexcept {
  va_list args;
  va_start(args, fmt);
  vlogf(fmt, args);
  va_end(args);
}

} // namespace aerogpu
