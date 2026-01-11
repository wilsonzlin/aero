#include "..\\common\\aerogpu_test_common.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

typedef struct _AEROGPU_UNICODE_STRING {
  USHORT Length;
  USHORT MaximumLength;
  PWSTR Buffer;
} AEROGPU_UNICODE_STRING;

typedef struct _AEROGPU_RTL_USER_PROCESS_PARAMETERS {
  BYTE Reserved1[16];
  PVOID Reserved2[10];
  AEROGPU_UNICODE_STRING ImagePathName;
  AEROGPU_UNICODE_STRING CommandLine;
} AEROGPU_RTL_USER_PROCESS_PARAMETERS;

typedef struct _AEROGPU_PEB {
  BYTE Reserved1[2];
  BYTE BeingDebugged;
  BYTE Reserved2[1];
  PVOID Reserved3[2];
  PVOID Ldr;
  AEROGPU_RTL_USER_PROCESS_PARAMETERS* ProcessParameters;
} AEROGPU_PEB;

typedef struct _AEROGPU_PROCESS_BASIC_INFORMATION {
  PVOID Reserved1;
  AEROGPU_PEB* PebBaseAddress;
  PVOID Reserved2[2];
  ULONG_PTR UniqueProcessId;
  PVOID Reserved3;
} AEROGPU_PROCESS_BASIC_INFORMATION;

typedef LONG(WINAPI* NtQueryInformationProcessFn)(HANDLE,
                                                  DWORD /*ProcessInformationClass*/,
                                                  PVOID /*ProcessInformation*/,
                                                  DWORD /*ProcessInformationLength*/,
                                                  DWORD* /*ReturnLength*/);

static std::wstring GetModulePath() {
  wchar_t path[MAX_PATH];
  DWORD len = GetModuleFileNameW(NULL, path, MAX_PATH);
  if (!len || len == MAX_PATH) {
    return L"";
  }
  return std::wstring(path, path + len);
}

static std::string FormatHandleHex(HANDLE h) {
  char buf[64];
#ifdef _WIN64
  _snprintf(buf, sizeof(buf), "0x%016I64X", (unsigned __int64)(uintptr_t)h);
#else
  _snprintf(buf, sizeof(buf), "0x%08lX", (unsigned long)(uintptr_t)h);
#endif
  return std::string(buf);
}

static std::string FormatPciIdHex(uint32_t v) {
  char buf[32];
  _snprintf(buf, sizeof(buf), "0x%04X", (unsigned)v);
  return std::string(buf);
}

static int CheckAdapter(IDirect3D9Ex* d3d,
                        const char* test_name,
                        bool allow_microsoft,
                        bool allow_non_aerogpu,
                        bool has_require_vid,
                        uint32_t require_vid,
                        bool has_require_did,
                        uint32_t require_did) {
  if (!d3d) {
    return aerogpu_test::Fail(test_name, "d3d == NULL");
  }

  D3DADAPTER_IDENTIFIER9 ident;
  ZeroMemory(&ident, sizeof(ident));
  HRESULT hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
  if (SUCCEEDED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: adapter: %s (VID=0x%04X DID=0x%04X)",
                               test_name,
                               ident.Description,
                               (unsigned)ident.VendorId,
                               (unsigned)ident.DeviceId);
    if (!allow_microsoft && ident.VendorId == 0x1414) {
      return aerogpu_test::Fail(
          test_name,
          "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
          "Install AeroGPU driver or pass --allow-microsoft.",
          (unsigned)ident.VendorId,
          (unsigned)ident.DeviceId);
    }
    if (has_require_vid && ident.VendorId != require_vid) {
      return aerogpu_test::Fail(test_name,
                                "adapter VID mismatch: got 0x%04X expected 0x%04X",
                                (unsigned)ident.VendorId,
                                (unsigned)require_vid);
    }
    if (has_require_did && ident.DeviceId != require_did) {
      return aerogpu_test::Fail(test_name,
                                "adapter DID mismatch: got 0x%04X expected 0x%04X",
                                (unsigned)ident.DeviceId,
                                (unsigned)require_did);
    }
    if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
        !(ident.VendorId == 0x1414 && allow_microsoft) &&
        !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
      return aerogpu_test::Fail(test_name,
                                "adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu "
                                "or use --require-vid/--require-did)",
                                ident.Description);
    }
  } else if (has_require_vid || has_require_did) {
    return aerogpu_test::FailHresult(test_name,
                                     "GetAdapterIdentifier (required for --require-vid/--require-did)",
                                     hr);
  }

  return 0;
}

static bool ParseUintPtr(const std::string& s, uintptr_t* out, std::string* err) {
  if (s.empty()) {
    if (err) {
      *err = "missing value";
    }
    return false;
  }
  errno = 0;
  char* end = NULL;
  unsigned __int64 v = _strtoui64(s.c_str(), &end, 0);
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
  if (v > (unsigned __int64)(uintptr_t)-1) {
    if (err) {
      *err = "out of uintptr range";
    }
    return false;
  }
  if (out) {
    *out = (uintptr_t)v;
  }
  return true;
}

static HRESULT CreateD3D9ExDevice(HWND hwnd, ComPtr<IDirect3D9Ex>* out_d3d, ComPtr<IDirect3DDevice9Ex>* out_dev) {
  if (!out_d3d || !out_dev) {
    return E_POINTER;
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
    return hr;
  }

  D3DPRESENT_PARAMETERS pp;
  ZeroMemory(&pp, sizeof(pp));
  pp.BackBufferWidth = 64;
  pp.BackBufferHeight = 64;
  pp.BackBufferFormat = D3DFMT_X8R8G8B8;
  pp.BackBufferCount = 1;
  pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
  pp.hDeviceWindow = hwnd;
  pp.Windowed = TRUE;
  pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;

  ComPtr<IDirect3DDevice9Ex> dev;
  DWORD create_flags = D3DCREATE_HARDWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
  hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, create_flags, &pp, NULL, dev.put());
  if (FAILED(hr)) {
    create_flags = D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
    hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd, create_flags, &pp, NULL, dev.put());
  }
  if (FAILED(hr)) {
    return hr;
  }

  out_d3d->reset(d3d.detach());
  out_dev->reset(dev.detach());
  return S_OK;
}

static bool PatchChildCommandLineSharedHandle(HANDLE child_process,
                                              const std::string& shared_handle_hex,
                                              std::string* err) {
  if (!child_process) {
    if (err) {
      *err = "child_process == NULL";
    }
    return false;
  }

  HMODULE ntdll = GetModuleHandleW(L"ntdll.dll");
  if (!ntdll) {
    ntdll = LoadLibraryW(L"ntdll.dll");
  }
  if (!ntdll) {
    if (err) {
      *err = "LoadLibraryW(ntdll.dll) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  NtQueryInformationProcessFn nt_query =
      (NtQueryInformationProcessFn)GetProcAddress(ntdll, "NtQueryInformationProcess");
  if (!nt_query) {
    if (err) {
      *err = "GetProcAddress(NtQueryInformationProcess) failed: " +
             aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  AEROGPU_PROCESS_BASIC_INFORMATION pbi;
  ZeroMemory(&pbi, sizeof(pbi));
  DWORD ret_len = 0;
  LONG status = nt_query(child_process, 0 /*ProcessBasicInformation*/, &pbi, sizeof(pbi), &ret_len);
  if (status != 0 || !pbi.PebBaseAddress) {
    if (err) {
      char buf[64];
      _snprintf(buf, sizeof(buf), "NtQueryInformationProcess failed: 0x%08lX", (unsigned long)status);
      *err = buf;
    }
    return false;
  }

  AEROGPU_PEB peb;
  ZeroMemory(&peb, sizeof(peb));
  SIZE_T nread = 0;
  if (!ReadProcessMemory(child_process, pbi.PebBaseAddress, &peb, sizeof(peb), &nread) ||
      nread != sizeof(peb) || !peb.ProcessParameters) {
    if (err) {
      *err = "ReadProcessMemory(PEB) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  AEROGPU_RTL_USER_PROCESS_PARAMETERS params;
  ZeroMemory(&params, sizeof(params));
  nread = 0;
  if (!ReadProcessMemory(child_process, peb.ProcessParameters, &params, sizeof(params), &nread) ||
      nread != sizeof(params) || !params.CommandLine.Buffer || params.CommandLine.Length == 0) {
    if (err) {
      *err = "ReadProcessMemory(ProcessParameters) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  const size_t cmd_chars = params.CommandLine.Length / sizeof(wchar_t);
  std::vector<wchar_t> cmdline(cmd_chars + 1, 0);
  nread = 0;
  if (!ReadProcessMemory(child_process,
                         params.CommandLine.Buffer,
                         &cmdline[0],
                         params.CommandLine.Length,
                         &nread) ||
      nread != params.CommandLine.Length) {
    if (err) {
      *err = "ReadProcessMemory(CommandLine) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }
  cmdline[cmd_chars] = 0;

  const wchar_t* key = L"--shared-handle=";
  wchar_t* pos = wcsstr(&cmdline[0], key);
  if (!pos) {
    if (err) {
      *err = "could not find --shared-handle= in child command line";
    }
    return false;
  }
  pos += wcslen(key);

  std::wstring repl(shared_handle_hex.begin(), shared_handle_hex.end());
  size_t existing_len = 0;
  while (pos[existing_len] && pos[existing_len] != L' ' && pos[existing_len] != L'\t') {
    existing_len++;
  }
  if (existing_len != repl.size()) {
    if (err) {
      char buf[128];
      _snprintf(buf,
                sizeof(buf),
                "shared-handle token length mismatch: existing=%lu replacement=%lu",
                (unsigned long)existing_len,
                (unsigned long)repl.size());
      *err = buf;
    }
    return false;
  }

  const size_t replace_index = (size_t)(pos - &cmdline[0]);
  SIZE_T nwritten = 0;
  if (!WriteProcessMemory(child_process,
                          params.CommandLine.Buffer + replace_index,
                          repl.c_str(),
                          repl.size() * sizeof(wchar_t),
                          &nwritten) ||
      nwritten != repl.size() * sizeof(wchar_t)) {
    if (err) {
      *err = "WriteProcessMemory(CommandLine) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  return true;
}

static int RunChild(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface(child)";

  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");

  uint32_t require_vid = 0;
  uint32_t require_did = 0;
  bool has_require_vid = false;
  bool has_require_did = false;
  std::string require_vid_str;
  std::string require_did_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--require-vid", &require_vid_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_vid_str, &require_vid, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-vid: %s", err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-did: %s", err.c_str());
    }
    has_require_did = true;
  }

  std::string handle_str;
  if (!aerogpu_test::GetArgValue(argc, argv, "--shared-handle", &handle_str)) {
    return aerogpu_test::Fail(kTestName, "missing --shared-handle");
  }

  uintptr_t handle_val = 0;
  std::string parse_err;
  if (!ParseUintPtr(handle_str, &handle_val, &parse_err) || handle_val == 0) {
    return aerogpu_test::Fail(kTestName, "invalid --shared-handle: %s", parse_err.c_str());
  }

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurface_Child",
                                              L"AeroGPU D3D9Ex Shared Surface Child",
                                              64,
                                              64,
                                              false);
  if (!hwnd) {
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  HRESULT hr = CreateD3D9ExDevice(hwnd, &d3d, &dev);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateD3D9ExDevice", hr);
  }

  int rc = CheckAdapter(d3d.get(),
                        kTestName,
                        allow_microsoft,
                        allow_non_aerogpu,
                        has_require_vid,
                        require_vid,
                        has_require_did,
                        require_did);
  if (rc) {
    return rc;
  }

  HANDLE shared = (HANDLE)handle_val;
  ComPtr<IDirect3DTexture9> tex;
  hr = dev->CreateTexture(64,
                          64,
                          1,
                          D3DUSAGE_RENDERTARGET,
                          D3DFMT_X8R8G8B8,
                          D3DPOOL_DEFAULT,
                          tex.put(),
                          &shared);
  if (FAILED(hr) || !tex) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture(open shared)", hr);
  }

  ComPtr<IDirect3DSurface9> surf;
  hr = tex->GetSurfaceLevel(0, surf.put());
  if (FAILED(hr) || !surf) {
    return aerogpu_test::FailHresult(kTestName, "GetSurfaceLevel", hr);
  }

  hr = dev->ColorFill(surf.get(), NULL, D3DCOLOR_XRGB(0, 128, 255));
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "ColorFill(shared surface)", hr);
  }

  hr = dev->Flush();
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "Flush", hr);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

static int RunParent(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface";

  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] "
        "[--allow-non-aerogpu]",
        kTestName);
    return 0;
  }

  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");

  uint32_t require_vid = 0;
  uint32_t require_did = 0;
  bool has_require_vid = false;
  bool has_require_did = false;
  std::string require_vid_str;
  std::string require_did_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--require-vid", &require_vid_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_vid_str, &require_vid, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-vid: %s", err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-did: %s", err.c_str());
    }
    has_require_did = true;
  }

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurface",
                                              L"AeroGPU D3D9Ex Shared Surface",
                                              64,
                                              64,
                                              false);
  if (!hwnd) {
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  HRESULT hr = CreateD3D9ExDevice(hwnd, &d3d, &dev);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateD3D9ExDevice", hr);
  }

  int rc = CheckAdapter(d3d.get(),
                        kTestName,
                        allow_microsoft,
                        allow_non_aerogpu,
                        has_require_vid,
                        require_vid,
                        has_require_did,
                        require_did);
  if (rc) {
    return rc;
  }

  HANDLE shared = NULL;
  ComPtr<IDirect3DTexture9> tex;
  hr = dev->CreateTexture(64,
                          64,
                          1,
                          D3DUSAGE_RENDERTARGET,
                          D3DFMT_X8R8G8B8,
                          D3DPOOL_DEFAULT,
                          tex.put(),
                          &shared);
  if (FAILED(hr) || !tex) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture(create shared)", hr);
  }
  if (!shared) {
    return aerogpu_test::Fail(kTestName, "CreateTexture(create shared) returned NULL shared handle");
  }

  ComPtr<IDirect3DSurface9> surf;
  hr = tex->GetSurfaceLevel(0, surf.put());
  if (FAILED(hr) || !surf) {
    return aerogpu_test::FailHresult(kTestName, "GetSurfaceLevel", hr);
  }

  hr = dev->ColorFill(surf.get(), NULL, D3DCOLOR_XRGB(255, 0, 0));
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "ColorFill(parent shared surface)", hr);
  }
  hr = dev->Flush();
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "Flush(parent)", hr);
  }

  // Ensure we don't accidentally rely on handle inheritance for the D3D shared handle; the child
  // should only see it via DuplicateHandle into the child process.
  SetHandleInformation(shared, HANDLE_FLAG_INHERIT, 0);

  std::wstring exe_path = GetModulePath();
  if (exe_path.empty()) {
    CloseHandle(shared);
    return aerogpu_test::Fail(kTestName, "GetModuleFileNameW failed");
  }

  // Create the child process first with a placeholder shared-handle token, then duplicate the
  // handle into the child and patch the child command line in-place before resuming. This keeps
  // the test single-binary and avoids any extra IPC while still passing the *child* handle value.
  const std::string placeholder_hex = FormatHandleHex((HANDLE)0);
  std::wstring cmdline = L"\"";
  cmdline += exe_path;
  cmdline += L"\" --child --shared-handle=";
  cmdline += std::wstring(placeholder_hex.begin(), placeholder_hex.end());
  if (allow_microsoft) {
    cmdline += L" --allow-microsoft";
  }
  if (allow_non_aerogpu) {
    cmdline += L" --allow-non-aerogpu";
  }
  if (has_require_vid) {
    std::string v = FormatPciIdHex(require_vid);
    cmdline += L" --require-vid=";
    cmdline += std::wstring(v.begin(), v.end());
  }
  if (has_require_did) {
    std::string v = FormatPciIdHex(require_did);
    cmdline += L" --require-did=";
    cmdline += std::wstring(v.begin(), v.end());
  }

  std::vector<wchar_t> cmdline_buf(cmdline.begin(), cmdline.end());
  cmdline_buf.push_back(0);

  STARTUPINFOW si;
  ZeroMemory(&si, sizeof(si));
  si.cb = sizeof(si);

  PROCESS_INFORMATION pi;
  ZeroMemory(&pi, sizeof(pi));

  BOOL ok = CreateProcessW(exe_path.c_str(),
                           &cmdline_buf[0],
                           NULL,
                           NULL,
                           TRUE,
                           CREATE_SUSPENDED,
                           NULL,
                           NULL,
                           &si,
                           &pi);
  if (!ok) {
    DWORD err = GetLastError();
    CloseHandle(shared);
    return aerogpu_test::Fail(kTestName,
                              "CreateProcessW failed: %s",
                              aerogpu_test::Win32ErrorToString(err).c_str());
  }

  HANDLE child_handle_value = NULL;
  if (!DuplicateHandle(GetCurrentProcess(),
                       shared,
                       pi.hProcess,
                       &child_handle_value,
                       0,
                       FALSE,
                       DUPLICATE_SAME_ACCESS) ||
      !child_handle_value) {
    DWORD err = GetLastError();
    TerminateProcess(pi.hProcess, 1);
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    CloseHandle(shared);
    return aerogpu_test::Fail(kTestName,
                              "DuplicateHandle(into child) failed: %s",
                              aerogpu_test::Win32ErrorToString(err).c_str());
  }

  std::string patch_err;
  if (!PatchChildCommandLineSharedHandle(pi.hProcess, FormatHandleHex(child_handle_value), &patch_err)) {
    TerminateProcess(pi.hProcess, 1);
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    CloseHandle(shared);
    return aerogpu_test::Fail(kTestName, "failed to patch child command line: %s", patch_err.c_str());
  }

  ResumeThread(pi.hThread);
  DWORD wait = WaitForSingleObject(pi.hProcess, 30000);
  if (wait == WAIT_TIMEOUT) {
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    CloseHandle(shared);
    return aerogpu_test::Fail(kTestName, "child timed out");
  }
  if (wait != WAIT_OBJECT_0) {
    DWORD err = GetLastError();
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    CloseHandle(shared);
    return aerogpu_test::Fail(kTestName,
                              "child did not exit cleanly: %s",
                              aerogpu_test::Win32ErrorToString(err).c_str());
  }

  DWORD exit_code = 1;
  if (!GetExitCodeProcess(pi.hProcess, &exit_code)) {
    DWORD err = GetLastError();
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    CloseHandle(shared);
    return aerogpu_test::Fail(kTestName,
                              "GetExitCodeProcess failed: %s",
                              aerogpu_test::Win32ErrorToString(err).c_str());
  }

  CloseHandle(pi.hThread);
  CloseHandle(pi.hProcess);
  CloseHandle(shared);

  if (exit_code != 0) {
    return aerogpu_test::Fail(kTestName, "child failed with exit code %lu", (unsigned long)exit_code);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  if (aerogpu_test::HasArg(argc, argv, "--child")) {
    return RunChild(argc, argv);
  }
  return RunParent(argc, argv);
}
