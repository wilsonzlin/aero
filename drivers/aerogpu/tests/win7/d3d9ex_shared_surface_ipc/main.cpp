#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

// Minimal NT structures needed to patch a suspended child process command line in-place.
// Keep this self-contained (avoid winternl.h) so the test builds cleanly with the VS2010 + Win7 SDK
// toolchain.
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

typedef LONG(WINAPI* NtQueryInformationProcessFn)(HANDLE /*ProcessHandle*/,
                                                  DWORD /*ProcessInformationClass*/,
                                                  PVOID /*ProcessInformation*/,
                                                  DWORD /*ProcessInformationLength*/,
                                                  DWORD* /*ReturnLength*/);

static bool FormatHandleHex16(HANDLE h, wchar_t out_digits[17]) {
  if (!out_digits) {
    return false;
  }
  unsigned __int64 v = (unsigned __int64)(uintptr_t)h;
  // Always use a 16-digit representation so we can patch a fixed-width placeholder in the child.
  // This works for both 32-bit and 64-bit handles (32-bit handles just have leading zeros).
  _snwprintf(out_digits, 17, L"%016I64X", v);
  out_digits[16] = 0;
  return true;
}

static bool PatchRemoteCommandLineSharedHandle(HANDLE child_process,
                                               HANDLE shared_handle_in_child,
                                               std::string* err) {
  if (!child_process) {
    if (err) {
      *err = "PatchRemoteCommandLineSharedHandle: invalid process handle";
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
  NtQueryInformationProcessFn NtQueryInformationProcess =
      (NtQueryInformationProcessFn)GetProcAddress(ntdll, "NtQueryInformationProcess");
  if (!NtQueryInformationProcess) {
    if (err) {
      *err = "GetProcAddress(NtQueryInformationProcess) failed";
    }
    return false;
  }

  AEROGPU_PROCESS_BASIC_INFORMATION pbi;
  ZeroMemory(&pbi, sizeof(pbi));
  DWORD ret_len = 0;
  LONG status = NtQueryInformationProcess(child_process,
                                          0 /*ProcessBasicInformation*/,
                                          &pbi,
                                          sizeof(pbi),
                                          &ret_len);
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
  SIZE_T bytes = 0;
  if (!ReadProcessMemory(child_process, pbi.PebBaseAddress, &peb, sizeof(peb), &bytes) ||
      bytes != sizeof(peb) || !peb.ProcessParameters) {
    if (err) {
      *err = "ReadProcessMemory(PEB) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  AEROGPU_RTL_USER_PROCESS_PARAMETERS params;
  ZeroMemory(&params, sizeof(params));
  bytes = 0;
  if (!ReadProcessMemory(child_process,
                         peb.ProcessParameters,
                         &params,
                         sizeof(params),
                         &bytes) ||
      bytes != sizeof(params) || !params.CommandLine.Buffer || params.CommandLine.Length == 0) {
    if (err) {
      *err = "ReadProcessMemory(ProcessParameters) failed: " +
             aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  if (params.CommandLine.Length % sizeof(wchar_t) != 0) {
    if (err) {
      *err = "Child command line length is not wchar_t aligned";
    }
    return false;
  }

  const size_t cmd_chars = (size_t)(params.CommandLine.Length / sizeof(wchar_t));
  std::vector<wchar_t> cmd_buf(cmd_chars + 1, 0);
  bytes = 0;
  if (!ReadProcessMemory(child_process,
                         params.CommandLine.Buffer,
                         &cmd_buf[0],
                         params.CommandLine.Length,
                         &bytes) ||
      bytes != params.CommandLine.Length) {
    if (err) {
      *err = "ReadProcessMemory(CommandLine) failed: " +
             aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }
  cmd_buf[cmd_chars] = 0;

  const std::wstring needle = L"--shared-handle=0x";
  std::wstring cmdline(&cmd_buf[0]);
  size_t pos = cmdline.find(needle);
  if (pos == std::wstring::npos) {
    if (err) {
      *err = "Failed to locate --shared-handle=0x in child command line";
    }
    return false;
  }
  const size_t digits_pos = pos + needle.size();
  const size_t digits_len = 16;
  if (digits_pos + digits_len > cmdline.size()) {
    if (err) {
      *err = "Child command line too short for fixed-width shared handle patch";
    }
    return false;
  }

  wchar_t digits[17];
  if (!FormatHandleHex16(shared_handle_in_child, digits)) {
    if (err) {
      *err = "FormatHandleHex16 failed";
    }
    return false;
  }

  // Patch only the digits in-place. This avoids changing UNICODE_STRING length fields.
  SIZE_T written = 0;
  LPVOID remote_dst = (LPVOID)(params.CommandLine.Buffer + digits_pos);  // NOLINT
  if (!WriteProcessMemory(child_process,
                          remote_dst,
                          digits,
                          digits_len * sizeof(wchar_t),
                          &written) ||
      written != digits_len * sizeof(wchar_t)) {
    if (err) {
      *err =
          "WriteProcessMemory(CommandLine digits) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  return true;
}

static int CreateD3D9ExDevice(aerogpu_test::TestReporter* reporter,
                              const char* test_name,
                              HWND hwnd,
                              ComPtr<IDirect3D9Ex>* out_d3d,
                              ComPtr<IDirect3DDevice9Ex>* out_dev) {
  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
    if (reporter) {
      return reporter->FailHresult("Direct3DCreate9Ex", hr);
    }
    return aerogpu_test::FailHresult(test_name, "Direct3DCreate9Ex", hr);
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
  hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                           D3DDEVTYPE_HAL,
                           hwnd,
                           create_flags,
                           &pp,
                           NULL,
                           dev.put());
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
    if (reporter) {
      return reporter->FailHresult("IDirect3D9Ex::CreateDeviceEx", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3D9Ex::CreateDeviceEx", hr);
  }

  if (out_d3d) {
    out_d3d->reset(d3d.detach());
  }
  if (out_dev) {
    out_dev->reset(dev.detach());
  }
  return 0;
}

static int ValidateAdapter(aerogpu_test::TestReporter* reporter,
                           const char* test_name,
                           IDirect3D9Ex* d3d,
                           bool allow_microsoft,
                           bool allow_non_aerogpu,
                           bool has_require_vid,
                           uint32_t require_vid,
                           bool has_require_did,
                           uint32_t require_did) {
  if (!d3d) {
    if (reporter) {
      return reporter->Fail("ValidateAdapter: d3d == NULL");
    }
    return aerogpu_test::Fail(test_name, "ValidateAdapter: d3d == NULL");
  }

  D3DADAPTER_IDENTIFIER9 ident;
  ZeroMemory(&ident, sizeof(ident));
  HRESULT hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
  if (FAILED(hr)) {
    if (has_require_vid || has_require_did) {
      if (reporter) {
        return reporter->FailHresult("GetAdapterIdentifier (required for --require-vid/--require-did)", hr);
      }
      return aerogpu_test::FailHresult(test_name,
                                       "GetAdapterIdentifier (required for --require-vid/--require-did)",
                                       hr);
    }
    return 0;
  }

  aerogpu_test::PrintfStdout("INFO: %s: adapter: %s (VID=0x%04X DID=0x%04X)",
                             test_name,
                             ident.Description,
                             (unsigned)ident.VendorId,
                             (unsigned)ident.DeviceId);
  if (reporter) {
    reporter->SetAdapterInfoA(ident.Description, ident.VendorId, ident.DeviceId);
  }

  if (!allow_microsoft && ident.VendorId == 0x1414) {
    if (reporter) {
      return reporter->Fail("refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
                            "Install AeroGPU driver or pass --allow-microsoft.",
                            (unsigned)ident.VendorId,
                            (unsigned)ident.DeviceId);
    }
    return aerogpu_test::Fail(test_name,
                              "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
                              "Install AeroGPU driver or pass --allow-microsoft.",
                              (unsigned)ident.VendorId,
                              (unsigned)ident.DeviceId);
  }
  if (has_require_vid && ident.VendorId != require_vid) {
    if (reporter) {
      return reporter->Fail("adapter VID mismatch: got 0x%04X expected 0x%04X",
                            (unsigned)ident.VendorId,
                            (unsigned)require_vid);
    }
    return aerogpu_test::Fail(test_name,
                              "adapter VID mismatch: got 0x%04X expected 0x%04X",
                              (unsigned)ident.VendorId,
                              (unsigned)require_vid);
  }
  if (has_require_did && ident.DeviceId != require_did) {
    if (reporter) {
      return reporter->Fail("adapter DID mismatch: got 0x%04X expected 0x%04X",
                            (unsigned)ident.DeviceId,
                            (unsigned)require_did);
    }
    return aerogpu_test::Fail(test_name,
                              "adapter DID mismatch: got 0x%04X expected 0x%04X",
                              (unsigned)ident.DeviceId,
                              (unsigned)require_did);
  }
  if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
      !(ident.VendorId == 0x1414 && allow_microsoft) &&
      !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
    if (reporter) {
      return reporter->Fail("adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu "
                            "or use --require-vid/--require-did)",
                            ident.Description);
    }
    return aerogpu_test::Fail(test_name,
                              "adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu "
                              "or use --require-vid/--require-did)",
                              ident.Description);
  }
  return 0;
}

static int RunConsumer(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface_ipc_consumer";
  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const std::wstring dump_bmp_path =
      aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9ex_shared_surface_ipc.bmp");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");

  uint32_t expected_debug_token = 0;
  bool has_expected_debug_token = false;
  std::string expected_token_str;
  bool has_expected_token_arg =
      aerogpu_test::GetArgValue(argc, argv, "--expected-debug-token", &expected_token_str);
  if (!has_expected_token_arg) {
    // Backwards compat: older test binaries used the name "expected-share-token" even though this is
    // a debug-only token returned by AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE (not the protocol share_token).
    has_expected_token_arg =
        aerogpu_test::GetArgValue(argc, argv, "--expected-share-token", &expected_token_str);
  }
  if (has_expected_token_arg) {
    std::string parse_err;
    if (!aerogpu_test::ParseUint32(expected_token_str, &expected_debug_token, &parse_err) ||
        expected_debug_token == 0) {
      return reporter.Fail("invalid --expected-debug-token: %s", parse_err.c_str());
    }
    has_expected_debug_token = true;
  }

  uint32_t require_vid = 0;
  uint32_t require_did = 0;
  bool has_require_vid = false;
  bool has_require_did = false;
  std::string require_vid_str;
  std::string require_did_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--require-vid", &require_vid_str)) {
    std::string parse_err;
    if (!aerogpu_test::ParseUint32(require_vid_str, &require_vid, &parse_err)) {
      return reporter.Fail("invalid --require-vid: %s", parse_err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string parse_err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &parse_err)) {
      return reporter.Fail("invalid --require-did: %s", parse_err.c_str());
    }
    has_require_did = true;
  }

  std::string handle_str;
  if (!aerogpu_test::GetArgValue(argc, argv, "--shared-handle", &handle_str)) {
    return reporter.Fail("missing --shared-handle");
  }

  errno = 0;
  char* end = NULL;
  unsigned __int64 hv = _strtoui64(handle_str.c_str(), &end, 0);
  if (errno == ERANGE || !end || end == handle_str.c_str() || *end != 0) {
    return reporter.Fail("invalid --shared-handle value: %s", handle_str.c_str());
  }

  const HANDLE shared_handle = (HANDLE)(uintptr_t)hv;
  aerogpu_test::PrintfStdout("INFO: %s: shared-handle=%p", kTestName, shared_handle);

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurfaceIPC_Consumer",
                                              L"AeroGPU D3D9Ex Shared Surface IPC (Consumer)",
                                              64,
                                              64,
                                              false);
  if (!hwnd) {
    return reporter.Fail("CreateBasicWindow failed");
  }

  if (has_expected_debug_token) {
    uint32_t token = 0;
    std::string map_err;
    if (!aerogpu_test::kmt::MapSharedHandleDebugTokenFromHwnd(hwnd, shared_handle, &token, &map_err)) {
      return reporter.Fail("MAP_SHARED_HANDLE failed: %s", map_err.c_str());
    }
    aerogpu_test::PrintfStdout("INFO: %s: MAP_SHARED_HANDLE debug_token=%lu (expected=%lu)",
                               kTestName,
                               (unsigned long)token,
                               (unsigned long)expected_debug_token);
    if (token != expected_debug_token) {
      return reporter.Fail("MAP_SHARED_HANDLE token mismatch: got=%lu expected=%lu",
                           (unsigned long)token,
                           (unsigned long)expected_debug_token);
    }
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(&reporter, kTestName, hwnd, &d3d, &dev);
  if (rc != 0) {
    return rc;
  }

  rc = ValidateAdapter(&reporter,
                       kTestName,
                       d3d.get(),
                       allow_microsoft,
                       allow_non_aerogpu,
                       has_require_vid,
                       require_vid,
                       has_require_did,
                       require_did);
  if (rc != 0) {
    return rc;
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  HANDLE open_handle = shared_handle;
  ComPtr<IDirect3DTexture9> tex;
  HRESULT hr = dev->CreateTexture(64,
                                  64,
                                  1,
                                  D3DUSAGE_RENDERTARGET,
                                  D3DFMT_A8R8G8B8,
                                  D3DPOOL_DEFAULT,
                                  tex.put(),
                                  &open_handle);
  if (FAILED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: CreateTexture(open shared) failed with %s; trying OpenSharedResource",
                               kTestName,
                               aerogpu_test::HresultToString(hr).c_str());
    hr = dev->OpenSharedResource(shared_handle,
                                 IID_IDirect3DTexture9,
                                 reinterpret_cast<void**>(tex.put()));
    if (FAILED(hr)) {
      return reporter.FailHresult("CreateTexture/OpenSharedResource(open shared)", hr);
    }
  } else {
    if (open_handle != shared_handle) {
      aerogpu_test::PrintfStdout("INFO: %s: CreateTexture updated shared handle: %p -> %p",
                                 kTestName,
                                 shared_handle,
                                 open_handle);
    }
  }

  ComPtr<IDirect3DSurface9> surf;
  hr = tex->GetSurfaceLevel(0, surf.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DTexture9::GetSurfaceLevel", hr);
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(64,
                                        64,
                                        D3DFMT_A8R8G8B8,
                                        D3DPOOL_SYSTEMMEM,
                                        sysmem.put(),
                                        NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateOffscreenPlainSurface", hr);
  }

  hr = dev->GetRenderTargetData(surf.get(), sysmem.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetRenderTargetData(shared)", hr);
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DSurface9::LockRect", hr);
  }

  const uint32_t pixel = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 2, 2);
  sysmem->UnlockRect();

  const uint32_t expected = 0xFF112233u;  // BGRA = (0x33,0x22,0x11,0xFF).
  if ((pixel & 0x00FFFFFFu) != (expected & 0x00FFFFFFu)) {
    if (dump) {
      HRESULT hr_dump = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
      if (SUCCEEDED(hr_dump)) {
        std::string dump_err;
        if (!aerogpu_test::WriteBmp32BGRA(
                dump_bmp_path,
                64,
                64,
                lr.pBits,
                (int)lr.Pitch,
                &dump_err)) {
          aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, dump_err.c_str());
        } else {
          reporter.AddArtifactPathW(dump_bmp_path);
        }
        sysmem->UnlockRect();
      }
    }
    return reporter.Fail("pixel mismatch: got=0x%08lX expected=0x%08lX",
                         (unsigned long)pixel,
                         (unsigned long)expected);
  }

  if (dump) {
    HRESULT hr_dump = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
    if (SUCCEEDED(hr_dump)) {
      std::string dump_err;
      if (!aerogpu_test::WriteBmp32BGRA(
              dump_bmp_path,
              64,
              64,
              lr.pBits,
              (int)lr.Pitch,
              &dump_err)) {
        aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, dump_err.c_str());
      } else {
        reporter.AddArtifactPathW(dump_bmp_path);
      }
      sysmem->UnlockRect();
    }
  }

  return reporter.Pass();
}

static int RunProducer(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface_ipc";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--show] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] "
        "[--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const std::wstring bmp_path =
      aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9ex_shared_surface_ipc.bmp");
  if (dump) {
    // Ensure we don't report a stale BMP from a previous run if the consumer fails before dumping.
    DeleteFileW(bmp_path.c_str());
  }
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  const bool show = aerogpu_test::HasArg(argc, argv, "--show");

  uint32_t require_vid = 0;
  uint32_t require_did = 0;
  bool has_require_vid = false;
  bool has_require_did = false;
  std::string require_vid_str;
  std::string require_did_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--require-vid", &require_vid_str)) {
    std::string parse_err;
    if (!aerogpu_test::ParseUint32(require_vid_str, &require_vid, &parse_err)) {
      return reporter.Fail("invalid --require-vid: %s", parse_err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string parse_err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &parse_err)) {
      return reporter.Fail("invalid --require-did: %s", parse_err.c_str());
    }
    has_require_did = true;
  }

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurfaceIPC_Producer",
                                              L"AeroGPU D3D9Ex Shared Surface IPC (Producer)",
                                              64,
                                              64,
                                              show);
  if (!hwnd) {
    return reporter.Fail("CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(&reporter, kTestName, hwnd, &d3d, &dev);
  if (rc != 0) {
    return rc;
  }
  rc = ValidateAdapter(&reporter,
                       kTestName,
                       d3d.get(),
                       allow_microsoft,
                       allow_non_aerogpu,
                       has_require_vid,
                       require_vid,
                       has_require_did,
                       require_did);
  if (rc != 0) {
    return rc;
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  HANDLE shared = NULL;
  ComPtr<IDirect3DTexture9> tex;
  HRESULT hr = dev->CreateTexture(64,
                                  64,
                                  1,
                                  D3DUSAGE_RENDERTARGET,
                                  D3DFMT_A8R8G8B8,
                                  D3DPOOL_DEFAULT,
                                  tex.put(),
                                  &shared);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture(shared)", hr);
  }
  if (!shared) {
    return reporter.Fail("CreateTexture returned NULL shared handle");
  }
  aerogpu_test::PrintfStdout("INFO: %s: created shared texture handle=%p", kTestName, shared);

  ComPtr<IDirect3DSurface9> rt;
  hr = tex->GetSurfaceLevel(0, rt.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DTexture9::GetSurfaceLevel", hr);
  }

  hr = dev->SetRenderTarget(0, rt.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetRenderTarget(shared)", hr);
  }

  const DWORD clear_color = D3DCOLOR_ARGB(0xFF, 0x11, 0x22, 0x33);  // 0xFF112233.
  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginScene", hr);
  }
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, clear_color, 1.0f, 0);
  HRESULT hr_end = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("Clear(shared)", hr);
  }
  if (FAILED(hr_end)) {
    return reporter.FailHresult("EndScene", hr_end);
  }

  // Ensure the clear has completed before the consumer opens/reads the surface.
  ComPtr<IDirect3DQuery9> q;
  hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, q.put());
  if (FAILED(hr) || !q) {
    return reporter.FailHresult("CreateQuery(D3DQUERYTYPE_EVENT)", hr);
  }
  hr = q->Issue(D3DISSUE_END);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DQuery9::Issue", hr);
  }

  const DWORD start = GetTickCount();
  for (;;) {
    hr = q->GetData(NULL, 0, D3DGETDATA_FLUSH);
    if (hr == S_OK) {
      break;
    }
    if (hr != S_FALSE) {
      return reporter.FailHresult("IDirect3DQuery9::GetData", hr);
    }
    if (GetTickCount() - start > 5000) {
      return reporter.Fail("GPU event query timed out");
    }
    Sleep(0);
  }

  wchar_t exe_path[MAX_PATH];
  DWORD exe_len = GetModuleFileNameW(NULL, exe_path, ARRAYSIZE(exe_path));
  if (!exe_len || exe_len >= ARRAYSIZE(exe_path)) {
    return reporter.Fail("GetModuleFileNameW failed");
  }

  uint32_t debug_token = 0;
  std::string map_err;
  const bool have_debug_token =
      aerogpu_test::kmt::MapSharedHandleDebugTokenFromHwnd(hwnd, shared, &debug_token, &map_err);
  if (have_debug_token) {
    aerogpu_test::PrintfStdout("INFO: %s: MAP_SHARED_HANDLE debug_token=%lu", kTestName, (unsigned long)debug_token);
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: MAP_SHARED_HANDLE unavailable (%s); skipping token validation",
                               kTestName,
                               map_err.c_str());
  }

  // Create the consumer suspended with a fixed-width placeholder for --shared-handle=0x...
  // We patch the placeholder digits in the child's command line before resuming it.
  std::wstring cmdline = std::wstring(L"\"") + exe_path +
                         L"\" --consumer --shared-handle=0x0000000000000000";
  if (have_debug_token) {
    wchar_t token_buf[32];
    _snwprintf(token_buf, ARRAYSIZE(token_buf), L"0x%08lX", (unsigned long)debug_token);
    token_buf[ARRAYSIZE(token_buf) - 1] = 0;
    cmdline += L" --expected-debug-token=";
    cmdline += token_buf;
  }
  if (dump) {
    cmdline += L" --dump";
  }
  if (has_require_vid) {
    cmdline += L" --require-vid=";
    cmdline += std::wstring(require_vid_str.begin(), require_vid_str.end());
  }
  if (has_require_did) {
    cmdline += L" --require-did=";
    cmdline += std::wstring(require_did_str.begin(), require_did_str.end());
  }
  if (allow_microsoft) {
    cmdline += L" --allow-microsoft";
  }
  if (allow_non_aerogpu) {
    cmdline += L" --allow-non-aerogpu";
  }
  if (require_umd) {
    cmdline += L" --require-umd";
  }
  std::vector<wchar_t> cmdline_buf(cmdline.begin(), cmdline.end());
  cmdline_buf.push_back(0);

  STARTUPINFOW si;
  ZeroMemory(&si, sizeof(si));
  si.cb = sizeof(si);

  PROCESS_INFORMATION pi;
  ZeroMemory(&pi, sizeof(pi));

  BOOL ok = CreateProcessW(exe_path,
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
    return reporter.Fail("CreateProcessW failed: %s", aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }

  HANDLE job = CreateJobObjectW(NULL, NULL);
  if (job) {
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION info;
    ZeroMemory(&info, sizeof(info));
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info, sizeof(info))) {
      aerogpu_test::PrintfStdout("INFO: %s: SetInformationJobObject(KILL_ON_JOB_CLOSE) failed: %s",
                                 kTestName,
                                 aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
      CloseHandle(job);
      job = NULL;
    } else if (!AssignProcessToJobObject(job, pi.hProcess)) {
      aerogpu_test::PrintfStdout("INFO: %s: AssignProcessToJobObject failed: %s",
                                 kTestName,
                                 aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
      CloseHandle(job);
      job = NULL;
    }
  }

  // If the shared handle is a real NT handle, duplicate it into the consumer process so the
  // consumer can use the *child* handle value.
  //
  // When possible, try to avoid a numeric collision between the producer and consumer handle values
  // to catch bugs where the driver accidentally treats the raw numeric value as a stable key.
  //
  // Note: some D3D9Ex implementations use "token" shared handles that are not real NT handles and
  // cannot be duplicated with DuplicateHandle. In that case we fall back to passing the raw numeric
  // handle value to the consumer.
  HANDLE shared_in_child = NULL;
  ok = DuplicateHandle(GetCurrentProcess(),
                       shared,
                       pi.hProcess,
                       &shared_in_child,
                       0,
                       FALSE,
                       DUPLICATE_SAME_ACCESS);
  const bool duplicated_into_child = (ok && shared_in_child != NULL);
  if (!duplicated_into_child) {
    DWORD werr = GetLastError();
    aerogpu_test::PrintfStdout("INFO: %s: DuplicateHandle failed (%s); falling back to raw handle value %p",
                               kTestName,
                               aerogpu_test::Win32ErrorToString(werr).c_str(),
                               shared);
    shared_in_child = shared;
  } else {
    aerogpu_test::PrintfStdout(
        "INFO: %s: duplicated shared handle into consumer: %p (producer) -> %p (consumer)",
        kTestName,
        shared,
        shared_in_child);
    if ((uintptr_t)shared_in_child == (uintptr_t)shared) {
      // It's possible (though unlikely) for the duplicated handle to end up with the same numeric
      // value in the child. Try duplicating again so we can still cover the "numeric instability"
      // case without failing spuriously.
      HANDLE shared_in_child2 = NULL;
      ok = DuplicateHandle(GetCurrentProcess(),
                           shared,
                           pi.hProcess,
                           &shared_in_child2,
                           0,
                           FALSE,
                           DUPLICATE_SAME_ACCESS);
      if (ok && shared_in_child2 != NULL && (uintptr_t)shared_in_child2 != (uintptr_t)shared) {
        shared_in_child = shared_in_child2;
        aerogpu_test::PrintfStdout(
            "INFO: %s: re-duplicated shared handle to avoid numeric collision: now %p (consumer)",
            kTestName,
            shared_in_child);
      } else {
        aerogpu_test::PrintfStdout(
            "INFO: %s: duplicated shared handle is numerically identical across processes; continuing anyway",
            kTestName);
      }
    }
  }

  std::string patch_err;
  if (!PatchRemoteCommandLineSharedHandle(pi.hProcess, shared_in_child, &patch_err)) {
    TerminateProcess(pi.hProcess, 1);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) {
      CloseHandle(job);
    }
    return reporter.Fail("failed to patch consumer command line: %s", patch_err.c_str());
  }

  ResumeThread(pi.hThread);

  DWORD wait = WaitForSingleObject(pi.hProcess, 20000);
  if (wait != WAIT_OBJECT_0) {
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 2000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) {
      CloseHandle(job);
    }
    return reporter.Fail("consumer timed out");
  }

  DWORD exit_code = 1;
  if (!GetExitCodeProcess(pi.hProcess, &exit_code)) {
    exit_code = 1;
  }

  CloseHandle(pi.hThread);
  CloseHandle(pi.hProcess);
  if (job) {
    CloseHandle(job);
  }

  if (dump) {
    reporter.AddArtifactPathIfExistsW(bmp_path);
  }
  if (exit_code != 0) {
    return reporter.Fail("consumer failed with exit code %lu", (unsigned long)exit_code);
  }
  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  if (aerogpu_test::HasArg(argc, argv, "--consumer")) {
    return RunConsumer(argc, argv);
  }
  return RunProducer(argc, argv);
}
