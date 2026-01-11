#pragma once

#include <stdarg.h>
#include <stdbool.h>

// Lightweight logging intended for early D3D10/11 bring-up. On Windows this
// emits OutputDebugStringA so logs can be collected with DebugView/WinDbg. In
// non-Windows builds it compiles to a no-op.

#if defined(_WIN32)

// Returns whether logging is currently enabled.
bool aerogpu_d3d10_11_log_enabled();
// Enables/disables logging at runtime.
void aerogpu_d3d10_11_log_set_enabled(bool enabled);

// Logs a formatted message (printf-style). Implementations will prefix with
// "AEROGPU_D3D11DDI:" and ensure a trailing newline.
void aerogpu_d3d10_11_logf(const char* fmt, ...);
void aerogpu_d3d10_11_vlogf(const char* fmt, va_list args);

#define AEROGPU_D3D10_11_LOG(...)                               \
  do {                                                          \
    if (aerogpu_d3d10_11_log_enabled()) {                        \
      aerogpu_d3d10_11_logf(__VA_ARGS__);                        \
    }                                                           \
  } while (0)

#define AEROGPU_D3D10_11_LOG_CALL() AEROGPU_D3D10_11_LOG("%s", __func__)

#else

static inline bool aerogpu_d3d10_11_log_enabled() {
  return false;
}

static inline void aerogpu_d3d10_11_log_set_enabled(bool /*enabled*/) {}

static inline void aerogpu_d3d10_11_logf(const char* /*fmt*/, ...) {}

static inline void aerogpu_d3d10_11_vlogf(const char* /*fmt*/, va_list /*args*/) {}

#define AEROGPU_D3D10_11_LOG(...) \
  do {                            \
  } while (0)

#define AEROGPU_D3D10_11_LOG_CALL() \
  do {                              \
  } while (0)

#endif

