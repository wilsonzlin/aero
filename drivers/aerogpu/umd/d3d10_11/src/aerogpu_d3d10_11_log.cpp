#include "aerogpu_d3d10_11_log.h"

#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <mutex>

#if defined(_WIN32)
  #ifndef WIN32_LEAN_AND_MEAN
    #define WIN32_LEAN_AND_MEAN 1
  #endif
  #ifndef NOMINMAX
    #define NOMINMAX 1
  #endif
  #include <windows.h>
#endif

namespace {

#if defined(_WIN32)
std::mutex g_log_mutex;
std::once_flag g_log_init_once;
std::atomic<bool> g_log_enabled{false};
FILE* g_log_file = nullptr;

bool parse_env_bool(const char* env_value, bool default_value) {
  if (!env_value || !env_value[0]) {
    return default_value;
  }
  switch (env_value[0]) {
    case '0':
    case 'n':
    case 'N':
    case 'f':
    case 'F':
      return false;
    case '1':
    case 'y':
    case 'Y':
    case 't':
    case 'T':
      return true;
    default:
      return default_value;
  }
}

void log_init() {
  bool enabled_default = false;
#if defined(_DEBUG)
  enabled_default = true;
#endif

  const char* enabled_env = std::getenv("AEROGPU_D3D10_11_LOG");
  g_log_enabled.store(parse_env_bool(enabled_env, enabled_default), std::memory_order_relaxed);

  const char* file_env = std::getenv("AEROGPU_D3D10_11_LOG_FILE");
  if (file_env && file_env[0]) {
    g_log_file = std::fopen(file_env, "a");
  }
}
#endif

} // namespace

#if defined(_WIN32)

bool aerogpu_d3d10_11_log_enabled() noexcept {
  try {
    std::call_once(g_log_init_once, log_init);
    return g_log_enabled.load(std::memory_order_relaxed);
  } catch (...) {
    return false;
  }
}

void aerogpu_d3d10_11_log_set_enabled(bool enabled) noexcept {
  try {
    std::call_once(g_log_init_once, log_init);
    g_log_enabled.store(enabled, std::memory_order_relaxed);
  } catch (...) {
  }
}

void aerogpu_d3d10_11_vlogf(const char* fmt, va_list args) noexcept {
  try {
    if (!aerogpu_d3d10_11_log_enabled()) {
      return;
    }

    std::lock_guard<std::mutex> lock(g_log_mutex);

    char msg[2048];
    int n = vsnprintf(msg, sizeof(msg), fmt, args);
    if (n < 0) {
      return;
    }

    char buf[2304];
    int m = snprintf(buf, sizeof(buf), "AEROGPU_D3D11DDI: %s\n", msg);
    if (m < 0) {
      return;
    }

    OutputDebugStringA(buf);
    if (g_log_file) {
      fputs(buf, g_log_file);
      fflush(g_log_file);
    }
  } catch (...) {
  }
}

void aerogpu_d3d10_11_logf(const char* fmt, ...) noexcept {
  va_list args;
  va_start(args, fmt);
  aerogpu_d3d10_11_vlogf(fmt, args);
  va_end(args);
}

#endif // defined(_WIN32)
