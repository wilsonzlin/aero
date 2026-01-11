#pragma once

#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif

#include <windows.h>
#include <stdint.h>
#include <stdio.h>
#include <stdarg.h>
#include <stdlib.h>
#include <ctype.h>
#include <errno.h>
#include <string.h>
#include <wchar.h>
#include <wctype.h>

#include <string>
#include <vector>

namespace aerogpu_test {

#ifndef ARRAYSIZE
#define ARRAYSIZE(a) (sizeof(a) / sizeof((a)[0]))
#endif

static inline void ConfigureProcessForAutomation() {
  // Suppress common modal dialogs that would otherwise hang automation (e.g. missing media, GP fault
  // crash dialogs). This is inherited by child processes (useful for aerogpu_timeout_runner.exe).
  SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
}

static inline bool HasArg(int argc, char** argv, const char* needle) {
  for (int i = 1; i < argc; ++i) {
    if (argv[i] && lstrcmpiA(argv[i], needle) == 0) {
      return true;
    }
  }
  return false;
}

static inline bool HasHelpArg(int argc, char** argv) {
  return HasArg(argc, argv, "--help") || HasArg(argc, argv, "-h") || HasArg(argc, argv, "/?");
}

static inline bool StrIContainsA(const char* haystack, const char* needle) {
  if (!haystack || !needle) {
    return false;
  }
  const size_t hlen = strlen(haystack);
  const size_t nlen = strlen(needle);
  if (nlen == 0) {
    return true;
  }
  if (nlen > hlen) {
    return false;
  }
  for (size_t i = 0; i + nlen <= hlen; ++i) {
    bool ok = true;
    for (size_t j = 0; j < nlen; ++j) {
      char a = (char)tolower((unsigned char)haystack[i + j]);
      char b = (char)tolower((unsigned char)needle[j]);
      if (a != b) {
        ok = false;
        break;
      }
    }
    if (ok) {
      return true;
    }
  }
  return false;
}

static inline bool StrIContainsW(const wchar_t* haystack, const wchar_t* needle) {
  if (!haystack || !needle) {
    return false;
  }
  const size_t hlen = wcslen(haystack);
  const size_t nlen = wcslen(needle);
  if (nlen == 0) {
    return true;
  }
  if (nlen > hlen) {
    return false;
  }
  for (size_t i = 0; i + nlen <= hlen; ++i) {
    bool ok = true;
    for (size_t j = 0; j < nlen; ++j) {
      wchar_t a = towlower(haystack[i + j]);
      wchar_t b = towlower(needle[j]);
      if (a != b) {
        ok = false;
        break;
      }
    }
    if (ok) {
      return true;
    }
  }
  return false;
}

static inline bool StrIStartsWith(const char* s, const char* prefix) {
  if (!s || !prefix) {
    return false;
  }
  while (*prefix) {
    if (!*s) {
      return false;
    }
    char a = *s++;
    char b = *prefix++;
    a = (char)tolower((unsigned char)a);
    b = (char)tolower((unsigned char)b);
    if (a != b) {
      return false;
    }
  }
  return true;
}

static inline bool GetArgValue(int argc, char** argv, const char* key, std::string* out) {
  if (!key || !out) {
    return false;
  }
  const std::string key_str(key);
  const std::string prefix = key_str + "=";
  for (int i = 1; i < argc; ++i) {
    const char* arg = argv[i];
    if (!arg) {
      continue;
    }
    if (StrIStartsWith(arg, prefix.c_str())) {
      *out = std::string(arg + prefix.size());
      return true;
    }
    if (lstrcmpiA(arg, key_str.c_str()) == 0) {
      if (i + 1 < argc && argv[i + 1]) {
        *out = std::string(argv[i + 1]);
        return true;
      }
      // Key present, but missing value.
      out->clear();
      return true;
    }
  }
  return false;
}

static inline bool ParseUint32(const std::string& s, uint32_t* out, std::string* err) {
  if (s.empty()) {
    if (err) {
      *err = "missing value";
    }
    return false;
  }
  errno = 0;
  char* end = NULL;
  unsigned long v = strtoul(s.c_str(), &end, 0);
  if (errno == ERANGE) {
    if (err) {
      *err = "out of range";
    }
    return false;
  }
  if (!end || end == s.c_str() || *end != 0) {
    if (err) {
      *err = "not a valid integer";
    }
    return false;
  }
  // On MSVC, unsigned long is 32-bit even on x64, but guard for other compilers anyway.
  if (v > 0xFFFFFFFFul) {
    if (err) {
      *err = "out of uint32 range";
    }
    return false;
  }
  if (out) {
    *out = (uint32_t)v;
  }
  return true;
}

static inline bool GetArgUint32(int argc, char** argv, const char* key, uint32_t* out) {
  std::string val;
  if (!GetArgValue(argc, argv, key, &val)) {
    return false;
  }
  if (val.empty()) {
    return false;
  }
  char* end = NULL;
  unsigned long v = strtoul(val.c_str(), &end, 0);
  if (!end || *end != 0) {
    return false;
  }
  if (out) {
    *out = (uint32_t)v;
  }
  return true;
}

static inline std::string Win32ErrorToString(DWORD err) {
  char* msg = NULL;
  DWORD flags =
      FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS;
  DWORD len = FormatMessageA(flags, NULL, err, 0, (LPSTR)&msg, 0, NULL);
  std::string out;
  if (len && msg) {
    out.assign(msg, msg + len);
    while (!out.empty() && (out[out.size() - 1] == '\r' || out[out.size() - 1] == '\n')) {
      out.resize(out.size() - 1);
    }
  } else {
    char buf[64];
    _snprintf(buf, sizeof(buf), "Win32 error %lu", (unsigned long)err);
    out = buf;
  }
  if (msg) {
    LocalFree(msg);
  }
  return out;
}

static inline std::string HresultToString(HRESULT hr) {
  // Many HRESULTs don't have useful system strings; always include the hex code.
  char buf[64];
  _snprintf(buf, sizeof(buf), "0x%08lX", (unsigned long)hr);
  std::string out = buf;

  char* msg = NULL;
  DWORD flags =
      FORMAT_MESSAGE_ALLOCATE_BUFFER | FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS;
  DWORD len = FormatMessageA(flags, NULL, (DWORD)hr, 0, (LPSTR)&msg, 0, NULL);
  if (len && msg) {
    std::string sys(msg, msg + len);
    while (!sys.empty() && (sys[sys.size() - 1] == '\r' || sys[sys.size() - 1] == '\n')) {
      sys.resize(sys.size() - 1);
    }
    if (!sys.empty()) {
      out += " (";
      out += sys;
      out += ")";
    }
  }
  if (msg) {
    LocalFree(msg);
  }
  return out;
}

static inline void PrintfStdout(const char* fmt, ...) {
  va_list ap;
  va_start(ap, fmt);
  vprintf(fmt, ap);
  va_end(ap);
  printf("\n");
}

static inline int Fail(const char* test_name, const char* fmt, ...) {
  printf("FAIL: %s: ", test_name);
  va_list ap;
  va_start(ap, fmt);
  vprintf(fmt, ap);
  va_end(ap);
  printf("\n");
  return 1;
}

static inline int FailHresult(const char* test_name, const char* what, HRESULT hr) {
  return Fail(test_name, "%s failed with %s", what, HresultToString(hr).c_str());
}

static inline bool Is64BitProcess() {
#if defined(_WIN64) || defined(_M_X64)
  return true;
#else
  return false;
#endif
}

static inline const char* GetProcessBitnessString() {
  return Is64BitProcess() ? "x64" : "x86";
}

static inline const wchar_t* ExpectedAeroGpuD3D9UmdModuleBaseName() {
  return Is64BitProcess() ? L"aerogpu_d3d9_x64.dll" : L"aerogpu_d3d9.dll";
}

static inline const wchar_t* ExpectedAeroGpuD3D10UmdModuleBaseName() {
  return Is64BitProcess() ? L"aerogpu_d3d10_x64.dll" : L"aerogpu_d3d10.dll";
}

static inline bool TryGetModuleFileNameW(HMODULE module, std::wstring* out, std::string* err) {
  if (out) {
    out->clear();
  }
  if (err) {
    err->clear();
  }
  if (!module) {
    if (err) {
      *err = "module handle is NULL";
    }
    return false;
  }

  // MAX_PATH may be too small for some setups; retry with a larger buffer if needed.
  DWORD cap = MAX_PATH;
  for (int i = 0; i < 4; ++i) {
    std::vector<wchar_t> buf(cap);
    DWORD len = GetModuleFileNameW(module, &buf[0], cap);
    if (len == 0) {
      if (err) {
        *err = "GetModuleFileNameW failed: " + Win32ErrorToString(GetLastError());
      }
      return false;
    }
    if (len < cap - 1) {
      if (out) {
        out->assign(&buf[0], &buf[0] + len);
      }
      return true;
    }
    cap *= 2;
  }
  if (err) {
    *err = "GetModuleFileNameW returned truncated path";
  }
  return false;
}

static inline bool GetLoadedModulePathByBaseName(const wchar_t* module_base_name,
                                                 std::wstring* out_path,
                                                 std::string* out_path_err) {
  if (out_path) {
    out_path->clear();
  }
  if (out_path_err) {
    out_path_err->clear();
  }
  if (!module_base_name) {
    if (out_path_err) {
      *out_path_err = "module name is NULL";
    }
    return false;
  }
  HMODULE mod = GetModuleHandleW(module_base_name);
  if (!mod) {
    return false;
  }
  std::string err;
  std::wstring path;
  if (!TryGetModuleFileNameW(mod, &path, &err)) {
    if (out_path_err) {
      *out_path_err = err;
    }
    return true;
  }
  if (out_path) {
    *out_path = path;
  }
  return true;
}

static inline void DumpLoadedAeroGpuUmdModules(const char* test_name) {
  const wchar_t* const names[] = {L"aerogpu_d3d9.dll",
                                  L"aerogpu_d3d9_x64.dll",
                                  L"aerogpu_d3d10.dll",
                                  L"aerogpu_d3d10_x64.dll"};
  bool any = false;
  for (size_t i = 0; i < ARRAYSIZE(names); ++i) {
    std::wstring path;
    std::string err;
    if (!GetLoadedModulePathByBaseName(names[i], &path, &err)) {
      continue;
    }
    any = true;
    if (!path.empty()) {
      PrintfStdout("INFO: %s: loaded module %ls => %ls", test_name, names[i], path.c_str());
    } else if (!err.empty()) {
      PrintfStdout("INFO: %s: loaded module %ls (path unavailable: %s)",
                   test_name,
                   names[i],
                   err.c_str());
    } else {
      PrintfStdout("INFO: %s: loaded module %ls (path unavailable)", test_name, names[i]);
    }
  }
  if (!any) {
    PrintfStdout("INFO: %s: no AeroGPU UMD modules currently loaded", test_name);
  }
}

static inline int RequireAeroGpuUmdLoaded(const char* test_name,
                                          const wchar_t* expected_module_base_name,
                                          const char* api_label,
                                          const char* reg_key_hint) {
  std::wstring path;
  std::string err;
  if (GetLoadedModulePathByBaseName(expected_module_base_name, &path, &err)) {
    if (!path.empty()) {
      PrintfStdout("INFO: %s: loaded AeroGPU %s UMD: %ls", test_name, api_label, path.c_str());
    } else if (!err.empty()) {
      PrintfStdout("INFO: %s: loaded AeroGPU %s UMD module %ls (path unavailable: %s)",
                   test_name,
                   api_label,
                   expected_module_base_name,
                   err.c_str());
    } else {
      PrintfStdout("INFO: %s: loaded AeroGPU %s UMD module %ls (path unavailable)",
                   test_name,
                   api_label,
                   expected_module_base_name);
    }
    return 0;
  }

  DumpLoadedAeroGpuUmdModules(test_name);
  return Fail(
      test_name,
      "expected AeroGPU %s UMD DLL %ls to be loaded in-process (process=%s), but it was not. "
      "Likely causes: incorrect INF registry keys (%s), incorrect UMD exports/decoration (stdcall), "
      "or missing DLL in System32/SysWOW64.",
      api_label,
      expected_module_base_name,
      GetProcessBitnessString(),
      reg_key_hint);
}

static inline int RequireAeroGpuD3D9UmdLoaded(const char* test_name) {
  return RequireAeroGpuUmdLoaded(test_name,
                                 ExpectedAeroGpuD3D9UmdModuleBaseName(),
                                 "D3D9",
                                 "InstalledDisplayDrivers/InstalledDisplayDriversWow");
}

static inline int RequireAeroGpuD3D10UmdLoaded(const char* test_name) {
  return RequireAeroGpuUmdLoaded(test_name,
                                 ExpectedAeroGpuD3D10UmdModuleBaseName(),
                                 "D3D10/11",
                                 "UserModeDriverName/UserModeDriverNameWow");
}

template <typename T>
class ComPtr {
 public:
  ComPtr() : ptr_(NULL) {}
  ~ComPtr() { reset(); }

  T* get() const { return ptr_; }
  T** put() {
    reset();
    return &ptr_;
  }

  void reset(T* p = NULL) {
    if (ptr_) {
      ptr_->Release();
    }
    ptr_ = p;
  }

  T* detach() {
    T* p = ptr_;
    ptr_ = NULL;
    return p;
  }

  T* operator->() const { return ptr_; }
  operator bool() const { return ptr_ != NULL; }

 private:
  T* ptr_;
  ComPtr(const ComPtr&);
  ComPtr& operator=(const ComPtr&);
};

static LRESULT CALLBACK BasicWndProc(HWND hwnd, UINT msg, WPARAM wparam, LPARAM lparam) {
  switch (msg) {
    case WM_DESTROY:
      PostQuitMessage(0);
      return 0;
    default:
      return DefWindowProcW(hwnd, msg, wparam, lparam);
  }
}

static inline HWND CreateBasicWindow(const wchar_t* class_name,
                                     const wchar_t* title,
                                     int client_width,
                                     int client_height,
                                     bool show = true) {
  HINSTANCE hinst = GetModuleHandleW(NULL);

  WNDCLASSEXW wc;
  ZeroMemory(&wc, sizeof(wc));
  wc.cbSize = sizeof(wc);
  wc.style = CS_HREDRAW | CS_VREDRAW;
  wc.lpfnWndProc = BasicWndProc;
  wc.hInstance = hinst;
  wc.hCursor = LoadCursor(NULL, IDC_ARROW);
  wc.lpszClassName = class_name;

  if (!RegisterClassExW(&wc)) {
    DWORD err = GetLastError();
    if (err != ERROR_CLASS_ALREADY_EXISTS) {
      return NULL;
    }
  }

  RECT r = {0, 0, client_width, client_height};
  AdjustWindowRect(&r, WS_OVERLAPPEDWINDOW, FALSE);

  HWND hwnd = CreateWindowExW(0,
                              class_name,
                              title,
                              WS_OVERLAPPEDWINDOW,
                              CW_USEDEFAULT,
                              CW_USEDEFAULT,
                              r.right - r.left,
                              r.bottom - r.top,
                              NULL,
                              NULL,
                              hinst,
                              NULL);
  if (!hwnd) {
    return NULL;
  }

  if (show) {
    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);
  }
  return hwnd;
}

static inline std::wstring GetModuleDir() {
  wchar_t path[MAX_PATH];
  DWORD len = GetModuleFileNameW(NULL, path, MAX_PATH);
  if (!len || len == MAX_PATH) {
    return L".\\";
  }

  for (DWORD i = len; i > 0; --i) {
    if (path[i - 1] == L'\\' || path[i - 1] == L'/') {
      path[i] = 0;
      break;
    }
  }
  return std::wstring(path);
}

static inline std::wstring JoinPath(const std::wstring& dir, const wchar_t* leaf) {
  if (dir.empty()) {
    return std::wstring(leaf);
  }
  wchar_t last = dir[dir.size() - 1];
  if (last == L'\\' || last == L'/') {
    return dir + leaf;
  }
  return dir + L"\\" + leaf;
}

static inline bool ReadFileBytes(const std::wstring& path,
                                 std::vector<unsigned char>* out,
                                 std::string* err) {
  if (!out) {
    if (err) {
      *err = "ReadFileBytes: out == NULL";
    }
    return false;
  }

  HANDLE h = CreateFileW(path.c_str(),
                         GENERIC_READ,
                         FILE_SHARE_READ,
                         NULL,
                         OPEN_EXISTING,
                         FILE_ATTRIBUTE_NORMAL,
                         NULL);
  if (h == INVALID_HANDLE_VALUE) {
    if (err) {
      *err = "CreateFileW failed: " + Win32ErrorToString(GetLastError());
    }
    return false;
  }

  LARGE_INTEGER size;
  if (!GetFileSizeEx(h, &size)) {
    if (err) {
      *err = "GetFileSizeEx failed: " + Win32ErrorToString(GetLastError());
    }
    CloseHandle(h);
    return false;
  }
  if (size.QuadPart <= 0 || size.QuadPart > 64 * 1024 * 1024) {
    if (err) {
      *err = "Unexpected file size";
    }
    CloseHandle(h);
    return false;
  }

  out->assign((size_t)size.QuadPart, 0);
  DWORD total_read = 0;
  while (total_read < (DWORD)out->size()) {
    DWORD chunk = 0;
    if (!ReadFile(h, &(*out)[total_read], (DWORD)out->size() - total_read, &chunk, NULL)) {
      if (err) {
        *err = "ReadFile failed: " + Win32ErrorToString(GetLastError());
      }
      CloseHandle(h);
      return false;
    }
    if (chunk == 0) {
      break;
    }
    total_read += chunk;
  }

  CloseHandle(h);
  if (total_read != (DWORD)out->size()) {
    if (err) {
      *err = "Short read";
    }
    return false;
  }
  return true;
}

static inline uint32_t ReadPixelBGRA(const void* data, int row_pitch, int x, int y) {
  const uint8_t* base = (const uint8_t*)data;
  const uint8_t* p = base + y * row_pitch + x * 4;
  uint32_t v = 0;
  v |= (uint32_t)p[0];
  v |= (uint32_t)p[1] << 8;
  v |= (uint32_t)p[2] << 16;
  v |= (uint32_t)p[3] << 24;
  return v;
}

static inline bool WriteBmp32BGRA(const std::wstring& path,
                                  int width,
                                  int height,
                                  const void* data,
                                  int row_pitch,
                                  std::string* err) {
  if (!data || width <= 0 || height <= 0 || row_pitch <= 0) {
    if (err) {
      *err = "Invalid BMP parameters";
    }
    return false;
  }

  HANDLE h = CreateFileW(path.c_str(),
                         GENERIC_WRITE,
                         0,
                         NULL,
                         CREATE_ALWAYS,
                         FILE_ATTRIBUTE_NORMAL,
                         NULL);
  if (h == INVALID_HANDLE_VALUE) {
    if (err) {
      *err = "CreateFileW failed: " + Win32ErrorToString(GetLastError());
    }
    return false;
  }

  BITMAPFILEHEADER bfh;
  ZeroMemory(&bfh, sizeof(bfh));
  bfh.bfType = 0x4D42;  // 'BM'
  bfh.bfOffBits = sizeof(BITMAPFILEHEADER) + sizeof(BITMAPINFOHEADER);
  bfh.bfSize = bfh.bfOffBits + width * height * 4;

  BITMAPINFOHEADER bih;
  ZeroMemory(&bih, sizeof(bih));
  bih.biSize = sizeof(BITMAPINFOHEADER);
  bih.biWidth = width;
  bih.biHeight = -height;  // top-down
  bih.biPlanes = 1;
  bih.biBitCount = 32;
  bih.biCompression = BI_RGB;
  bih.biSizeImage = width * height * 4;

  DWORD written = 0;
  if (!WriteFile(h, &bfh, sizeof(bfh), &written, NULL) || written != sizeof(bfh)) {
    if (err) {
      *err = "WriteFile(BITMAPFILEHEADER) failed: " + Win32ErrorToString(GetLastError());
    }
    CloseHandle(h);
    return false;
  }
  if (!WriteFile(h, &bih, sizeof(bih), &written, NULL) || written != sizeof(bih)) {
    if (err) {
      *err = "WriteFile(BITMAPINFOHEADER) failed: " + Win32ErrorToString(GetLastError());
    }
    CloseHandle(h);
    return false;
  }

  const uint8_t* src = (const uint8_t*)data;
  for (int y = 0; y < height; ++y) {
    const uint8_t* row = src + y * row_pitch;
    DWORD row_written = 0;
    if (!WriteFile(h, row, width * 4, &row_written, NULL) || row_written != (DWORD)(width * 4)) {
      if (err) {
        *err = "WriteFile(pixels) failed: " + Win32ErrorToString(GetLastError());
      }
      CloseHandle(h);
      return false;
    }
  }

  CloseHandle(h);
  return true;
}

}  // namespace aerogpu_test
