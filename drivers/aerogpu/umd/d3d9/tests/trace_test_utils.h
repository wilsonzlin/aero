#pragma once

#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <sstream>
#include <string>

#if defined(_WIN32)
  #ifndef WIN32_LEAN_AND_MEAN
    #define WIN32_LEAN_AND_MEAN 1
  #endif
  #ifndef NOMINMAX
    #define NOMINMAX 1
  #endif
  #include <windows.h>
#else
  #include <unistd.h>
#endif

namespace aerogpu_d3d9_trace_test {

inline void set_env(const char* name, const char* value) {
#if defined(_WIN32)
  if (!name) {
    return;
  }
  if (value) {
    SetEnvironmentVariableA(name, value);
  } else {
    SetEnvironmentVariableA(name, nullptr);
  }
#else
  if (!name) {
    return;
  }
  if (value) {
    setenv(name, value, 1);
  } else {
    unsetenv(name);
  }
#endif
}

inline std::string slurp_file(const std::string& path) {
  std::ifstream in(path.c_str(), std::ios::in | std::ios::binary);
  std::stringstream ss;
  ss << in.rdbuf();
  return ss.str();
}

inline std::string make_unique_log_path(const char* stem) {
  if (!stem) {
    stem = "aerogpu_d3d9_trace_test";
  }
#if defined(_WIN32)
  const unsigned long pid = GetCurrentProcessId();
#else
  const unsigned long pid = static_cast<unsigned long>(getpid());
#endif
  char buf[256];
  std::snprintf(buf, sizeof(buf), "%s.%lu.log", stem, pid);
  return std::string(buf);
}

inline int fail(const char* msg) {
  std::fprintf(stdout, "FAIL: %s\n", msg ? msg : "(null)");
  return 1;
}

} // namespace aerogpu_d3d9_trace_test

