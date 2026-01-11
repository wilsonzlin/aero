#include "..\\common\\aerogpu_test_common.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

struct AdapterRequirements {
  bool allow_microsoft;
  bool allow_non_aerogpu;
  bool require_umd;
  bool has_require_vid;
  bool has_require_did;
  uint32_t require_vid;
  uint32_t require_did;
};

// Minimal NT structures needed to patch a suspended child process command line in-place.
// This keeps the test single-binary while still passing the *child* handle value when we
// DuplicateHandle into the child process (handle inheritance is avoided for the shared handle).
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

static bool IsLikelyNtHandle(HANDLE h) {
  if (!h) {
    return false;
  }
  HANDLE dup = NULL;
  if (!DuplicateHandle(GetCurrentProcess(), h, GetCurrentProcess(), &dup, 0, FALSE, DUPLICATE_SAME_ACCESS) || !dup) {
    return false;
  }
  CloseHandle(dup);
  return true;
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
                "handle replacement length mismatch (existing_len=%u repl_len=%u)",
                (unsigned)existing_len,
                (unsigned)repl.size());
      *err = buf;
    }
    return false;
  }

  SIZE_T nwritten = 0;
  if (!WriteProcessMemory(child_process,
                          pos,
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

static int CheckD3D9Adapter(const char* test_name, IDirect3D9Ex* d3d, const AdapterRequirements& req) {
  D3DADAPTER_IDENTIFIER9 ident;
  ZeroMemory(&ident, sizeof(ident));
  HRESULT hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
  if (SUCCEEDED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: adapter: %s (VID=0x%04X DID=0x%04X)",
                               test_name,
                               ident.Description,
                               (unsigned)ident.VendorId,
                               (unsigned)ident.DeviceId);
    if (!req.allow_microsoft && ident.VendorId == 0x1414) {
      return aerogpu_test::Fail(test_name,
                                "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
                                "Install AeroGPU driver or pass --allow-microsoft.",
                                (unsigned)ident.VendorId,
                                (unsigned)ident.DeviceId);
    }
    if (req.has_require_vid && ident.VendorId != req.require_vid) {
      return aerogpu_test::Fail(test_name,
                                "adapter VID mismatch: got 0x%04X expected 0x%04X",
                                (unsigned)ident.VendorId,
                                (unsigned)req.require_vid);
    }
    if (req.has_require_did && ident.DeviceId != req.require_did) {
      return aerogpu_test::Fail(test_name,
                                "adapter DID mismatch: got 0x%04X expected 0x%04X",
                                (unsigned)ident.DeviceId,
                                (unsigned)req.require_did);
    }
    if (!req.allow_non_aerogpu && !req.has_require_vid && !req.has_require_did &&
        !(ident.VendorId == 0x1414 && req.allow_microsoft) &&
        !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
      return aerogpu_test::Fail(test_name,
                                "adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu "
                                "or use --require-vid/--require-did)",
                                ident.Description);
    }
  } else if (req.has_require_vid || req.has_require_did) {
    return aerogpu_test::FailHresult(
        test_name, "GetAdapterIdentifier (required for --require-vid/--require-did)", hr);
  }
  return 0;
}

static int CreateD3D9ExDevice(const char* test_name,
                              HWND hwnd,
                              int width,
                              int height,
                              const AdapterRequirements& req,
                              ComPtr<IDirect3D9Ex>* out_d3d,
                              ComPtr<IDirect3DDevice9Ex>* out_dev) {
  if (!out_d3d || !out_dev) {
    return aerogpu_test::Fail(test_name, "internal: CreateD3D9ExDevice out params are NULL");
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(test_name, "Direct3DCreate9Ex", hr);
  }

  D3DPRESENT_PARAMETERS pp;
  ZeroMemory(&pp, sizeof(pp));
  pp.BackBufferWidth = width;
  pp.BackBufferHeight = height;
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
    hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                             D3DDEVTYPE_HAL,
                             hwnd,
                             create_flags,
                             &pp,
                             NULL,
                             dev.put());
  }
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(test_name, "IDirect3D9Ex::CreateDeviceEx", hr);
  }

  int rc = CheckD3D9Adapter(test_name, d3d.get(), req);
  if (rc != 0) {
    return rc;
  }

  if (req.require_umd || (!req.allow_microsoft && !req.allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(test_name);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  out_d3d->reset(d3d.detach());
  out_dev->reset(dev.detach());
  return 0;
}

static int RunChild(int argc, char** argv, const AdapterRequirements& req, bool hidden) {
  const char* kTestName = "d3d9ex_shared_surface_stress(child)";

  std::string handle_str;
  if (!aerogpu_test::GetArgValue(argc, argv, "--shared-handle", &handle_str)) {
    return aerogpu_test::Fail(kTestName, "missing required --shared-handle in --child mode");
  }

  uintptr_t handle_value = 0;
  std::string err;
  if (!ParseUintPtr(handle_str, &handle_value, &err) || handle_value == 0) {
    return aerogpu_test::Fail(kTestName, "invalid --shared-handle: %s", err.c_str());
  }

  const HANDLE shared_handle = (HANDLE)handle_value;
  const bool shared_handle_is_nt = IsLikelyNtHandle(shared_handle);

  const int kWidth = 64;
  const int kHeight = 64;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurfaceStressChild",
                                              L"AeroGPU D3D9Ex Shared Surface Stress (Child)",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow(child) failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(kTestName, hwnd, kWidth, kHeight, req, &d3d, &dev);
  if (rc != 0) {
    return rc;
  }

  ComPtr<IDirect3DSurface9> surface;
  HRESULT hr =
      dev->OpenSharedResource(shared_handle, IID_IDirect3DSurface9, reinterpret_cast<void**>(surface.put()));
  if (FAILED(hr)) {
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    return aerogpu_test::FailHresult(kTestName, "OpenSharedResource(shared surface)", hr);
  }

  // Open the same shared surface twice. This ensures the driver can handle multiple
  // per-process allocation handles that alias the same backing alloc_id.
  ComPtr<IDirect3DSurface9> surface2;
  hr = dev->OpenSharedResource(shared_handle,
                               IID_IDirect3DSurface9,
                               reinterpret_cast<void**>(surface2.put()));
  if (FAILED(hr)) {
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    return aerogpu_test::FailHresult(kTestName, "OpenSharedResource(shared surface #2)", hr);
  }

  RECT touch = {kWidth - 4, kHeight - 4, kWidth, kHeight};
  hr = dev->ColorFill(surface.get(), &touch, D3DCOLOR_XRGB(0, 128, 255));
  if (FAILED(hr)) {
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::ColorFill(opened surface)", hr);
  }

  RECT touch2 = {0, 0, 4, 4};
  hr = dev->ColorFill(surface2.get(), &touch2, D3DCOLOR_XRGB(255, 0, 128));
  if (FAILED(hr)) {
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::ColorFill(opened surface #2)", hr);
  }
  hr = dev->Flush();
  if (FAILED(hr)) {
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::Flush", hr);
  }

  if (shared_handle_is_nt) {
    CloseHandle(shared_handle);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

static int RunParent(int argc, char** argv, const AdapterRequirements& req, bool hidden, uint32_t iterations) {
  const char* kTestName = "d3d9ex_shared_surface_stress";

  const int kWidth = 64;
  const int kHeight = 64;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurfaceStressParent",
                                              L"AeroGPU D3D9Ex Shared Surface Stress (Parent)",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow(parent) failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(kTestName, hwnd, kWidth, kHeight, req, &d3d, &dev);
  if (rc != 0) {
    return rc;
  }

  std::wstring exe_path = GetModulePath();
  if (exe_path.empty()) {
    return aerogpu_test::Fail(kTestName, "GetModuleFileNameW failed");
  }

  const DWORD kPerChildTimeoutMs = 8000;

  for (uint32_t iter = 0; iter < iterations; ++iter) {
    aerogpu_test::PrintfStdout("INFO: %s: iteration %u/%u", kTestName, (unsigned)(iter + 1), (unsigned)iterations);

    HANDLE shared_handle = NULL;
    ComPtr<IDirect3DSurface9> surface;
    HRESULT hr = dev->CreateRenderTarget(kWidth,
                                         kHeight,
                                         D3DFMT_X8R8G8B8,
                                         D3DMULTISAMPLE_NONE,
                                         0,
                                         FALSE,
                                         surface.put(),
                                         &shared_handle);
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "CreateRenderTarget(shared)", hr);
    }
    if (!shared_handle) {
      return aerogpu_test::Fail(kTestName, "CreateRenderTarget(shared) returned NULL shared handle");
    }

    const bool shared_handle_is_nt = IsLikelyNtHandle(shared_handle);
    if (shared_handle_is_nt) {
      SetHandleInformation(shared_handle, HANDLE_FLAG_INHERIT, 0);
    }

    // Ensure create/export reaches the host before the child tries to open it.
    hr = dev->Flush();
    if (FAILED(hr)) {
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::Flush(parent create)", hr);
    }

    const std::string placeholder_hex = FormatHandleHex((HANDLE)0);
    std::wstring cmdline = L"\"";
    cmdline += exe_path;
    cmdline += L"\" --child --shared-handle=";
    cmdline += std::wstring(placeholder_hex.begin(), placeholder_hex.end());
    cmdline += L" --hidden";
    if (req.allow_microsoft) {
      cmdline += L" --allow-microsoft";
    }
    if (req.allow_non_aerogpu) {
      cmdline += L" --allow-non-aerogpu";
    }
    if (req.require_umd) {
      cmdline += L" --require-umd";
    }
    if (req.has_require_vid) {
      std::string v = FormatPciIdHex(req.require_vid);
      cmdline += L" --require-vid=";
      cmdline += std::wstring(v.begin(), v.end());
    }
    if (req.has_require_did) {
      std::string v = FormatPciIdHex(req.require_did);
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
    HANDLE job = NULL;

    BOOL ok = CreateProcessW(exe_path.c_str(),
                             &cmdline_buf[0],
                             NULL,
                             NULL,
                             FALSE,
                             CREATE_SUSPENDED,
                             NULL,
                             NULL,
                             &si,
                             &pi);
    if (!ok) {
      DWORD err = GetLastError();
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      return aerogpu_test::Fail(
          kTestName, "CreateProcessW failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
    }

    job = CreateJobObjectW(NULL, NULL);
    if (job) {
      JOBOBJECT_EXTENDED_LIMIT_INFORMATION info;
      ZeroMemory(&info, sizeof(info));
      info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
      if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info, sizeof(info))) {
        CloseHandle(job);
        job = NULL;
      } else if (!AssignProcessToJobObject(job, pi.hProcess)) {
        CloseHandle(job);
        job = NULL;
      }
    }

    HANDLE child_handle_value = NULL;
    bool duplicated_into_child = false;
    DWORD duplicate_err = 0;
    if (DuplicateHandle(GetCurrentProcess(),
                        shared_handle,
                        pi.hProcess,
                        &child_handle_value,
                        0,
                        FALSE,
                        DUPLICATE_SAME_ACCESS) &&
        child_handle_value) {
      duplicated_into_child = true;
    } else {
      duplicate_err = GetLastError();
    }

    std::string child_handle_hex;
    if (duplicated_into_child) {
      child_handle_hex = FormatHandleHex(child_handle_value);
    } else {
      child_handle_hex = FormatHandleHex(shared_handle);
      aerogpu_test::PrintfStdout("INFO: %s: DuplicateHandle(into child) failed (%s); passing raw handle %s",
                                 kTestName,
                                 aerogpu_test::Win32ErrorToString(duplicate_err).c_str(),
                                 child_handle_hex.c_str());
    }

    std::string patch_err;
    if (!PatchChildCommandLineSharedHandle(pi.hProcess, child_handle_hex, &patch_err)) {
      TerminateProcess(pi.hProcess, 1);
      WaitForSingleObject(pi.hProcess, 5000);
      CloseHandle(pi.hThread);
      CloseHandle(pi.hProcess);
      if (job) {
        CloseHandle(job);
      }
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      return aerogpu_test::Fail(kTestName, "failed to patch child command line: %s", patch_err.c_str());
    }

    ResumeThread(pi.hThread);

    DWORD wait = WaitForSingleObject(pi.hProcess, kPerChildTimeoutMs);
    if (wait != WAIT_OBJECT_0) {
      TerminateProcess(pi.hProcess, 124);
      WaitForSingleObject(pi.hProcess, 5000);
      CloseHandle(pi.hThread);
      CloseHandle(pi.hProcess);
      if (job) {
        CloseHandle(job);
      }
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      return aerogpu_test::Fail(kTestName, "child timed out");
    }

    DWORD exit_code = 1;
    if (!GetExitCodeProcess(pi.hProcess, &exit_code)) {
      DWORD err = GetLastError();
      CloseHandle(pi.hThread);
      CloseHandle(pi.hProcess);
      if (job) {
        CloseHandle(job);
      }
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      return aerogpu_test::Fail(kTestName,
                                "GetExitCodeProcess failed: %s",
                                aerogpu_test::Win32ErrorToString(err).c_str());
    }

    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) {
      CloseHandle(job);
    }

    if (exit_code != 0) {
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      return aerogpu_test::Fail(kTestName, "child failed with exit code %lu", (unsigned long)exit_code);
    }

    // Parent destroys its reference after the child is done.
    surface.reset();
    hr = dev->Flush();
    if (FAILED(hr)) {
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::Flush(parent destroy)", hr);
    }
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

static int RunSharedSurfaceStressTest(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface_stress";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--iterations=N] [--hidden] [--allow-microsoft] [--allow-non-aerogpu] [--require-umd] "
        "[--require-vid=0x####] [--require-did=0x####]",
        kTestName);
    aerogpu_test::PrintfStdout("Internal: %s.exe --child --shared-handle=0x... [--hidden] ... (used by parent)", kTestName);
    return 0;
  }

  const bool child = aerogpu_test::HasArg(argc, argv, "--child");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");
  if (aerogpu_test::HasArg(argc, argv, "--show")) {
    hidden = false;
  }

  uint32_t iterations = 20;
  std::string iter_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--iterations", &iter_str) && !iter_str.empty()) {
    std::string err;
    if (!aerogpu_test::ParseUint32(iter_str, &iterations, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --iterations: %s", err.c_str());
    }
    if (iterations == 0) {
      iterations = 1;
    }
  }

  AdapterRequirements req;
  ZeroMemory(&req, sizeof(req));
  req.allow_microsoft = allow_microsoft;
  req.allow_non_aerogpu = allow_non_aerogpu;
  req.require_umd = require_umd;

  std::string require_vid_str;
  std::string require_did_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--require-vid", &require_vid_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_vid_str, &req.require_vid, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-vid: %s", err.c_str());
    }
    req.has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &req.require_did, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-did: %s", err.c_str());
    }
    req.has_require_did = true;
  }

  if (child) {
    return RunChild(argc, argv, req, hidden);
  }
  return RunParent(argc, argv, req, hidden, iterations);
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunSharedSurfaceStressTest(argc, argv);
}
