#include "..\\common\\aerogpu_test_common.h"
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

static bool PatchRemoteCommandLineHandleDigits(HANDLE child_process,
                                               const wchar_t* needle,
                                               HANDLE handle_in_child,
                                               std::string* err) {
  if (!child_process || !needle) {
    if (err) {
      *err = "PatchRemoteCommandLineHandleDigits: invalid args";
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
      *err = "ReadProcessMemory(ProcessParameters) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
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
      *err = "ReadProcessMemory(CommandLine) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }
  cmd_buf[cmd_chars] = 0;

  std::wstring cmdline(&cmd_buf[0]);
  size_t pos = cmdline.find(needle);
  if (pos == std::wstring::npos) {
    if (err) {
      std::string needle_utf8(needle, needle + wcslen(needle));
      *err = "Failed to locate handle placeholder in child command line: " + needle_utf8;
    }
    return false;
  }

  const size_t digits_pos = pos + wcslen(needle);
  const size_t digits_len = 16;
  if (digits_pos + digits_len > cmdline.size()) {
    if (err) {
      *err = "Child command line too short for fixed-width handle patch";
    }
    return false;
  }

  wchar_t digits[17];
  if (!FormatHandleHex16(handle_in_child, digits)) {
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

static int CreateD3D9ExDevice(const char* test_name,
                              HWND hwnd,
                              ComPtr<IDirect3D9Ex>* out_d3d,
                              ComPtr<IDirect3DDevice9Ex>* out_dev) {
  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
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

static int ValidateAdapter(const char* test_name,
                           IDirect3D9Ex* d3d,
                           bool allow_microsoft,
                           bool allow_non_aerogpu,
                           bool has_require_vid,
                           uint32_t require_vid,
                           bool has_require_did,
                           uint32_t require_did) {
  if (!d3d) {
    return aerogpu_test::Fail(test_name, "ValidateAdapter: d3d == NULL");
  }

  D3DADAPTER_IDENTIFIER9 ident;
  ZeroMemory(&ident, sizeof(ident));
  HRESULT hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
  if (FAILED(hr)) {
    if (has_require_vid || has_require_did) {
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

  if (!allow_microsoft && ident.VendorId == 0x1414) {
    return aerogpu_test::Fail(test_name,
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
  return 0;
}

static DWORD MakeParentColor(uint32_t iteration) {
  // Deterministic but non-trivial; alpha always 0xFF.
  const uint32_t r = (iteration * 3u + 0x11u) & 0xFFu;
  const uint32_t g = (iteration * 7u + 0x22u) & 0xFFu;
  const uint32_t b = (iteration * 11u + 0x33u) & 0xFFu;
  return D3DCOLOR_ARGB(0xFF, r, g, b);
}

static DWORD MakeChildColor(uint32_t iteration) {
  // Complement-ish transform of the parent color so both directions can be validated.
  const uint32_t r = ((iteration * 5u + 0x44u) ^ 0xAAu) & 0xFFu;
  const uint32_t g = ((iteration * 9u + 0x55u) ^ 0x55u) & 0xFFu;
  const uint32_t b = ((iteration * 13u + 0x66u) ^ 0x11u) & 0xFFu;
  return D3DCOLOR_ARGB(0xFF, r, g, b);
}

static int WaitForGpuEventQuery(const char* test_name,
                                IDirect3DDevice9Ex* dev,
                                IDirect3DQuery9* q,
                                DWORD timeout_ms) {
  if (!dev || !q) {
    return aerogpu_test::Fail(test_name, "WaitForGpuEventQuery called with NULL");
  }

  HRESULT hr = q->Issue(D3DISSUE_END);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(test_name, "IDirect3DQuery9::Issue", hr);
  }

  const DWORD start = GetTickCount();
  for (;;) {
    hr = q->GetData(NULL, 0, D3DGETDATA_FLUSH);
    if (hr == S_OK) {
      break;
    }
    if (hr != S_FALSE) {
      return aerogpu_test::FailHresult(test_name, "IDirect3DQuery9::GetData", hr);
    }
    if (GetTickCount() - start > timeout_ms) {
      return aerogpu_test::Fail(test_name, "GPU event query timed out");
    }
    Sleep(0);
  }

  return 0;
}

static int ReadSurfacePixel(const char* test_name,
                            IDirect3DDevice9Ex* dev,
                            IDirect3DSurface9* src,
                            IDirect3DSurface9* sysmem,
                            int x,
                            int y,
                            uint32_t* out_pixel) {
  if (!dev || !src || !sysmem || !out_pixel) {
    return aerogpu_test::Fail(test_name, "ReadSurfacePixel: invalid args");
  }

  HRESULT hr = dev->GetRenderTargetData(src, sysmem);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(test_name, "GetRenderTargetData", hr);
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(test_name, "IDirect3DSurface9::LockRect", hr);
  }
  *out_pixel = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, x, y);
  sysmem->UnlockRect();
  return 0;
}

static void MaybeDumpSurface(const wchar_t* file_name,
                             bool dump,
                             IDirect3DSurface9* sysmem,
                             int width,
                             int height) {
  if (!dump || !file_name || !sysmem) {
    return;
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  HRESULT hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return;
  }

  std::string err;
  aerogpu_test::WriteBmp32BGRA(aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), file_name),
                               width,
                               height,
                               lr.pBits,
                               (int)lr.Pitch,
                               &err);
  sysmem->UnlockRect();
}

struct SharedIpc {
  LONG status;  // 0=ok, non-zero=fail
  ULONGLONG shared_handle_in_parent;
};

static int RunChild(int argc, char** argv) {
  const char* kTestName = "d3d9ex_alloc_id_persistence_child";

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  const bool show = aerogpu_test::HasArg(argc, argv, "--show");

  uint32_t iterations = 64;
  aerogpu_test::GetArgUint32(argc, argv, "--iterations", &iterations);
  if (iterations == 0 || iterations > 10000) {
    return aerogpu_test::Fail(kTestName, "invalid --iterations value");
  }

  uint32_t parent_pid = 0;
  if (!aerogpu_test::GetArgUint32(argc, argv, "--parent-pid", &parent_pid) || parent_pid == 0) {
    return aerogpu_test::Fail(kTestName, "missing --parent-pid");
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
      return aerogpu_test::Fail(kTestName, "invalid --require-vid: %s", parse_err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string parse_err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &parse_err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-did: %s", parse_err.c_str());
    }
    has_require_did = true;
  }

  std::string handle_str;
  if (!aerogpu_test::GetArgValue(argc, argv, "--parent-shared-handle", &handle_str)) {
    return aerogpu_test::Fail(kTestName, "missing --parent-shared-handle");
  }
  errno = 0;
  char* end = NULL;
  unsigned __int64 hv = _strtoui64(handle_str.c_str(), &end, 0);
  if (errno == ERANGE || !end || end == handle_str.c_str() || *end != 0) {
    return aerogpu_test::Fail(kTestName, "invalid --parent-shared-handle value: %s", handle_str.c_str());
  }
  const HANDLE parent_shared_handle = (HANDLE)(uintptr_t)hv;

  std::string map_name_utf8;
  std::string ready_event_utf8;
  std::string parent_event_utf8;
  std::string child_event_utf8;
  if (!aerogpu_test::GetArgValue(argc, argv, "--ipc-map", &map_name_utf8) || map_name_utf8.empty()) {
    return aerogpu_test::Fail(kTestName, "missing --ipc-map");
  }
  if (!aerogpu_test::GetArgValue(argc, argv, "--ready-event", &ready_event_utf8) ||
      ready_event_utf8.empty()) {
    return aerogpu_test::Fail(kTestName, "missing --ready-event");
  }
  if (!aerogpu_test::GetArgValue(argc, argv, "--parent-event", &parent_event_utf8) ||
      parent_event_utf8.empty()) {
    return aerogpu_test::Fail(kTestName, "missing --parent-event");
  }
  if (!aerogpu_test::GetArgValue(argc, argv, "--child-event", &child_event_utf8) ||
      child_event_utf8.empty()) {
    return aerogpu_test::Fail(kTestName, "missing --child-event");
  }

  std::wstring map_name(map_name_utf8.begin(), map_name_utf8.end());
  std::wstring ready_name(ready_event_utf8.begin(), ready_event_utf8.end());
  std::wstring parent_name(parent_event_utf8.begin(), parent_event_utf8.end());
  std::wstring child_name(child_event_utf8.begin(), child_event_utf8.end());

  HANDLE map = OpenFileMappingW(FILE_MAP_ALL_ACCESS, FALSE, map_name.c_str());
  if (!map) {
    return aerogpu_test::Fail(kTestName,
                              "OpenFileMappingW failed: %s",
                              aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }
  SharedIpc* ipc = (SharedIpc*)MapViewOfFile(map, FILE_MAP_ALL_ACCESS, 0, 0, sizeof(SharedIpc));
  if (!ipc) {
    CloseHandle(map);
    return aerogpu_test::Fail(kTestName,
                              "MapViewOfFile failed: %s",
                              aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }

  HANDLE ready_event = OpenEventW(EVENT_MODIFY_STATE, FALSE, ready_name.c_str());
  HANDLE parent_event = OpenEventW(SYNCHRONIZE, FALSE, parent_name.c_str());
  HANDLE child_event = OpenEventW(EVENT_MODIFY_STATE, FALSE, child_name.c_str());
  if (!ready_event || !parent_event || !child_event) {
    const DWORD werr = GetLastError();
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    if (ready_event) CloseHandle(ready_event);
    if (parent_event) CloseHandle(parent_event);
    if (child_event) CloseHandle(child_event);
    return aerogpu_test::Fail(kTestName,
                              "OpenEventW failed: %s",
                              aerogpu_test::Win32ErrorToString(werr).c_str());
  }

  // Ensure the parent doesn't hang forever if we fail before signalling readiness.
  ipc->status = 1;
  ipc->shared_handle_in_parent = 0;

  const int kSize = 32;
  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExAllocIdPersistence_Child",
                                              L"AeroGPU D3D9Ex alloc_id persistence (Child)",
                                              kSize,
                                              kSize,
                                              show);
  if (!hwnd) {
    SetEvent(ready_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(kTestName, hwnd, &d3d, &dev);
  if (rc != 0) {
    SetEvent(ready_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    return rc;
  }

  rc = ValidateAdapter(kTestName,
                       d3d.get(),
                       allow_microsoft,
                       allow_non_aerogpu,
                       has_require_vid,
                       require_vid,
                       has_require_did,
                       require_did);
  if (rc != 0) {
    SetEvent(ready_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    return rc;
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(kTestName);
    if (umd_rc != 0) {
      SetEvent(ready_event);
      UnmapViewOfFile(ipc);
      CloseHandle(map);
      CloseHandle(ready_event);
      CloseHandle(parent_event);
      CloseHandle(child_event);
      return umd_rc;
    }
  }

  ComPtr<IDirect3DTexture9> parent_tex;
  HANDLE open_parent_handle = parent_shared_handle;
  HRESULT hr = dev->CreateTexture(kSize,
                                  kSize,
                                  1,
                                  D3DUSAGE_RENDERTARGET,
                                  D3DFMT_A8R8G8B8,
                                  D3DPOOL_DEFAULT,
                                  parent_tex.put(),
                                  &open_parent_handle);
  if (FAILED(hr)) {
    SetEvent(ready_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    return aerogpu_test::FailHresult(kTestName, "CreateTexture(open parent shared)", hr);
  }

  HANDLE shared_child = NULL;
  ComPtr<IDirect3DTexture9> child_tex;
  hr = dev->CreateTexture(kSize,
                          kSize,
                          1,
                          D3DUSAGE_RENDERTARGET,
                          D3DFMT_A8R8G8B8,
                          D3DPOOL_DEFAULT,
                          child_tex.put(),
                          &shared_child);
  if (FAILED(hr) || !shared_child) {
    SetEvent(ready_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    return aerogpu_test::FailHresult(kTestName, "CreateTexture(shared child)", hr);
  }

  HANDLE parent_proc = OpenProcess(PROCESS_DUP_HANDLE, FALSE, parent_pid);
  if (!parent_proc) {
    SetEvent(ready_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    return aerogpu_test::Fail(kTestName,
                              "OpenProcess(parent) failed: %s",
                              aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }
  HANDLE shared_in_parent = NULL;
  BOOL ok = DuplicateHandle(GetCurrentProcess(),
                            shared_child,
                            parent_proc,
                            &shared_in_parent,
                            0,
                            FALSE,
                            DUPLICATE_SAME_ACCESS);
  CloseHandle(parent_proc);
  if (!ok || !shared_in_parent) {
    SetEvent(ready_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    return aerogpu_test::Fail(kTestName,
                              "DuplicateHandle(child->parent) failed: %s",
                              aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }

  ipc->status = 0;
  ipc->shared_handle_in_parent = (ULONGLONG)(uintptr_t)shared_in_parent;
  SetEvent(ready_event);

  ComPtr<IDirect3DSurface9> surf_a;
  ComPtr<IDirect3DSurface9> surf_b;
  hr = parent_tex->GetSurfaceLevel(0, surf_a.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DTexture9::GetSurfaceLevel(parent)", hr);
  }
  hr = child_tex->GetSurfaceLevel(0, surf_b.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DTexture9::GetSurfaceLevel(child)", hr);
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(kSize,
                                        kSize,
                                        D3DFMT_A8R8G8B8,
                                        D3DPOOL_SYSTEMMEM,
                                        sysmem.put(),
                                        NULL);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateOffscreenPlainSurface", hr);
  }

  ComPtr<IDirect3DQuery9> q;
  hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, q.put());
  if (FAILED(hr) || !q) {
    return aerogpu_test::FailHresult(kTestName, "CreateQuery(D3DQUERYTYPE_EVENT)", hr);
  }

  for (uint32_t i = 0; i < iterations; ++i) {
    const DWORD parent_color = MakeParentColor(i);
    const DWORD child_color = MakeChildColor(i);

    DWORD wait = WaitForSingleObject(parent_event, 20000);
    if (wait != WAIT_OBJECT_0) {
      return aerogpu_test::Fail(kTestName, "timeout waiting for parent event");
    }

    uint32_t pixel = 0;
    rc = ReadSurfacePixel(kTestName, dev.get(), surf_b.get(), sysmem.get(), 2, 2, &pixel);
    if (rc != 0) {
      return rc;
    }
    if (pixel != parent_color) {
      wchar_t dump_name[128];
      _snwprintf(dump_name,
                 ARRAYSIZE(dump_name),
                 L"d3d9ex_alloc_id_persistence_child_src_%u.bmp",
                 (unsigned)i);
      dump_name[ARRAYSIZE(dump_name) - 1] = 0;
      MaybeDumpSurface(dump_name, dump, sysmem.get(), kSize, kSize);
      return aerogpu_test::Fail(kTestName,
                                "B mismatch @iter=%u: got=0x%08lX expected=0x%08lX",
                                (unsigned)i,
                                (unsigned long)pixel,
                                (unsigned long)parent_color);
    }

    hr = dev->SetRenderTarget(0, surf_b.get());
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "SetRenderTarget(B)", hr);
    }
    hr = dev->BeginScene();
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "BeginScene", hr);
    }
    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, child_color, 1.0f, 0);
    HRESULT hr_end = dev->EndScene();
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "Clear(B)", hr);
    }
    if (FAILED(hr_end)) {
      return aerogpu_test::FailHresult(kTestName, "EndScene", hr_end);
    }

    hr = dev->StretchRect(surf_b.get(), NULL, surf_a.get(), NULL, D3DTEXF_NONE);
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "StretchRect(B->A)", hr);
    }

    rc = WaitForGpuEventQuery(kTestName, dev.get(), q.get(), 5000);
    if (rc != 0) {
      return rc;
    }

    if (!SetEvent(child_event)) {
      return aerogpu_test::Fail(kTestName,
                                "SetEvent(child_event) failed: %s",
                                aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    }
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

static int RunParent(int argc, char** argv) {
  const char* kTestName = "d3d9ex_alloc_id_persistence";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--show] [--json[=PATH]] [--iterations=N] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  const bool show = aerogpu_test::HasArg(argc, argv, "--show");

  uint32_t iterations = 64;
  aerogpu_test::GetArgUint32(argc, argv, "--iterations", &iterations);
  if (iterations == 0 || iterations > 10000) {
    return aerogpu_test::Fail(kTestName, "invalid --iterations value");
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
      return aerogpu_test::Fail(kTestName, "invalid --require-vid: %s", parse_err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string parse_err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &parse_err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-did: %s", parse_err.c_str());
    }
    has_require_did = true;
  }

  const int kSize = 32;
  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExAllocIdPersistence_Parent",
                                              L"AeroGPU D3D9Ex alloc_id persistence (Parent)",
                                              kSize,
                                              kSize,
                                              show);
  if (!hwnd) {
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(kTestName, hwnd, &d3d, &dev);
  if (rc != 0) {
    return rc;
  }

  rc = ValidateAdapter(kTestName,
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
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  HANDLE shared_parent = NULL;
  ComPtr<IDirect3DTexture9> tex_a;
  HRESULT hr = dev->CreateTexture(kSize,
                                  kSize,
                                  1,
                                  D3DUSAGE_RENDERTARGET,
                                  D3DFMT_A8R8G8B8,
                                  D3DPOOL_DEFAULT,
                                  tex_a.put(),
                                  &shared_parent);
  if (FAILED(hr) || !shared_parent) {
    return aerogpu_test::FailHresult(kTestName, "CreateTexture(shared parent)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: parent shared handle=%p", kTestName, shared_parent);

  ComPtr<IDirect3DSurface9> surf_a;
  hr = tex_a->GetSurfaceLevel(0, surf_a.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DTexture9::GetSurfaceLevel(parent)", hr);
  }

  wchar_t exe_path[MAX_PATH];
  DWORD exe_len = GetModuleFileNameW(NULL, exe_path, ARRAYSIZE(exe_path));
  if (!exe_len || exe_len >= ARRAYSIZE(exe_path)) {
    return aerogpu_test::Fail(kTestName, "GetModuleFileNameW failed");
  }

  const DWORD pid = GetCurrentProcessId();

  wchar_t map_name_w[128];
  wchar_t ready_name_w[128];
  wchar_t parent_evt_w[128];
  wchar_t child_evt_w[128];
  _snwprintf(map_name_w, ARRAYSIZE(map_name_w), L"Local\\aerogpu_alloc_persist_map_%lu", (unsigned long)pid);
  _snwprintf(ready_name_w,
             ARRAYSIZE(ready_name_w),
             L"Local\\aerogpu_alloc_persist_ready_%lu",
             (unsigned long)pid);
  _snwprintf(parent_evt_w,
             ARRAYSIZE(parent_evt_w),
             L"Local\\aerogpu_alloc_persist_parent_%lu",
             (unsigned long)pid);
  _snwprintf(child_evt_w,
             ARRAYSIZE(child_evt_w),
             L"Local\\aerogpu_alloc_persist_child_%lu",
             (unsigned long)pid);
  map_name_w[ARRAYSIZE(map_name_w) - 1] = 0;
  ready_name_w[ARRAYSIZE(ready_name_w) - 1] = 0;
  parent_evt_w[ARRAYSIZE(parent_evt_w) - 1] = 0;
  child_evt_w[ARRAYSIZE(child_evt_w) - 1] = 0;

  HANDLE map = CreateFileMappingW(INVALID_HANDLE_VALUE,
                                  NULL,
                                  PAGE_READWRITE,
                                  0,
                                  (DWORD)sizeof(SharedIpc),
                                  map_name_w);
  if (!map) {
    return aerogpu_test::Fail(kTestName,
                              "CreateFileMappingW failed: %s",
                              aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }
  SharedIpc* ipc = (SharedIpc*)MapViewOfFile(map, FILE_MAP_ALL_ACCESS, 0, 0, sizeof(SharedIpc));
  if (!ipc) {
    CloseHandle(map);
    return aerogpu_test::Fail(kTestName,
                              "MapViewOfFile failed: %s",
                              aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }
  ipc->status = 1;
  ipc->shared_handle_in_parent = 0;

  HANDLE ready_event = CreateEventW(NULL, TRUE, FALSE, ready_name_w);
  HANDLE parent_event = CreateEventW(NULL, FALSE, FALSE, parent_evt_w);
  HANDLE child_event = CreateEventW(NULL, FALSE, FALSE, child_evt_w);
  if (!ready_event || !parent_event || !child_event) {
    const DWORD werr = GetLastError();
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    if (ready_event) CloseHandle(ready_event);
    if (parent_event) CloseHandle(parent_event);
    if (child_event) CloseHandle(child_event);
    return aerogpu_test::Fail(kTestName,
                              "CreateEventW failed: %s",
                              aerogpu_test::Win32ErrorToString(werr).c_str());
  }

  std::wstring cmdline = std::wstring(L"\"") + exe_path + L"\" --child --parent-pid=";
  wchar_t pid_buf[32];
  _snwprintf(pid_buf, ARRAYSIZE(pid_buf), L"%lu", (unsigned long)pid);
  pid_buf[ARRAYSIZE(pid_buf) - 1] = 0;
  cmdline += pid_buf;
  cmdline += L" --parent-shared-handle=0x0000000000000000";
  cmdline += L" --ipc-map=";
  cmdline += map_name_w;
  cmdline += L" --ready-event=";
  cmdline += ready_name_w;
  cmdline += L" --parent-event=";
  cmdline += parent_evt_w;
  cmdline += L" --child-event=";
  cmdline += child_evt_w;
  cmdline += L" --iterations=";
  wchar_t iter_buf[32];
  _snwprintf(iter_buf, ARRAYSIZE(iter_buf), L"%lu", (unsigned long)iterations);
  iter_buf[ARRAYSIZE(iter_buf) - 1] = 0;
  cmdline += iter_buf;
  if (dump) {
    cmdline += L" --dump";
  }
  if (show) {
    cmdline += L" --show";
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
    const DWORD werr = GetLastError();
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    return aerogpu_test::Fail(kTestName,
                              "CreateProcessW failed: %s",
                              aerogpu_test::Win32ErrorToString(werr).c_str());
  }

  HANDLE job = CreateJobObjectW(NULL, NULL);
  if (job) {
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION info;
    ZeroMemory(&info, sizeof(info));
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    if (!SetInformationJobObject(job, JobObjectExtendedLimitInformation, &info, sizeof(info)) ||
        !AssignProcessToJobObject(job, pi.hProcess)) {
      CloseHandle(job);
      job = NULL;
    }
  }

  HANDLE shared_in_child = NULL;
  ok = DuplicateHandle(GetCurrentProcess(),
                       shared_parent,
                       pi.hProcess,
                       &shared_in_child,
                       0,
                       FALSE,
                       DUPLICATE_SAME_ACCESS);
  if (!ok || !shared_in_child) {
    const DWORD werr = GetLastError();
    TerminateProcess(pi.hProcess, 1);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) CloseHandle(job);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    return aerogpu_test::Fail(kTestName,
                              "DuplicateHandle(parent->child) failed: %s",
                              aerogpu_test::Win32ErrorToString(werr).c_str());
  }
  if ((uintptr_t)shared_in_child == (uintptr_t)shared_parent) {
    // Extremely unlikely but possible; try again to avoid numeric collisions across processes.
    HANDLE shared_in_child2 = NULL;
    ok = DuplicateHandle(GetCurrentProcess(),
                         shared_parent,
                         pi.hProcess,
                         &shared_in_child2,
                         0,
                         FALSE,
                         DUPLICATE_SAME_ACCESS);
    if (ok && shared_in_child2) {
      HANDLE tmp = NULL;
      if (DuplicateHandle(pi.hProcess,
                          shared_in_child,
                          GetCurrentProcess(),
                          &tmp,
                          0,
                          FALSE,
                          DUPLICATE_SAME_ACCESS | DUPLICATE_CLOSE_SOURCE) &&
          tmp) {
        CloseHandle(tmp);
      }
      shared_in_child = shared_in_child2;
    }
  }
  if ((uintptr_t)shared_in_child == (uintptr_t)shared_parent) {
    TerminateProcess(pi.hProcess, 1);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) CloseHandle(job);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    return aerogpu_test::Fail(kTestName, "refusing to run: shared handle value is numerically identical");
  }

  std::string patch_err;
  if (!PatchRemoteCommandLineHandleDigits(pi.hProcess,
                                          L"--parent-shared-handle=0x",
                                          shared_in_child,
                                          &patch_err)) {
    TerminateProcess(pi.hProcess, 1);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) CloseHandle(job);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    return aerogpu_test::Fail(kTestName, "failed to patch child command line: %s", patch_err.c_str());
  }

  ResumeThread(pi.hThread);

  DWORD wait = WaitForSingleObject(ready_event, 20000);
  if (wait != WAIT_OBJECT_0) {
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 2000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) CloseHandle(job);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    return aerogpu_test::Fail(kTestName, "child ready event timed out");
  }

  if (ipc->status != 0 || ipc->shared_handle_in_parent == 0) {
    TerminateProcess(pi.hProcess, 1);
    WaitForSingleObject(pi.hProcess, 2000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) CloseHandle(job);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    return aerogpu_test::Fail(kTestName, "child init failed (ipc status=%ld)", (long)ipc->status);
  }

  const HANDLE shared_child_in_parent = (HANDLE)(uintptr_t)ipc->shared_handle_in_parent;
  aerogpu_test::PrintfStdout("INFO: %s: got child shared handle=%p", kTestName, shared_child_in_parent);

  ComPtr<IDirect3DTexture9> tex_b;
  HANDLE open_child_handle = shared_child_in_parent;
  hr = dev->CreateTexture(kSize,
                          kSize,
                          1,
                          D3DUSAGE_RENDERTARGET,
                          D3DFMT_A8R8G8B8,
                          D3DPOOL_DEFAULT,
                          tex_b.put(),
                          &open_child_handle);
  if (FAILED(hr)) {
    TerminateProcess(pi.hProcess, 1);
    WaitForSingleObject(pi.hProcess, 2000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) CloseHandle(job);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    return aerogpu_test::FailHresult(kTestName, "CreateTexture(open child shared)", hr);
  }

  ComPtr<IDirect3DSurface9> surf_b;
  hr = tex_b->GetSurfaceLevel(0, surf_b.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DTexture9::GetSurfaceLevel(child)", hr);
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(kSize,
                                        kSize,
                                        D3DFMT_A8R8G8B8,
                                        D3DPOOL_SYSTEMMEM,
                                        sysmem.put(),
                                        NULL);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateOffscreenPlainSurface", hr);
  }

  ComPtr<IDirect3DQuery9> q;
  hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, q.put());
  if (FAILED(hr) || !q) {
    return aerogpu_test::FailHresult(kTestName, "CreateQuery(D3DQUERYTYPE_EVENT)", hr);
  }

  // Drive the ping-pong loop: parent clears A and stretches into B; child validates B, clears it to
  // a different color, and stretches back into A. Parent validates A each iteration. This ensures:
  // - Both processes submit work using allocations created in different processes.
  // - Both submissions reference both alloc_ids in the same DMA buffer (StretchRect uses src+dst).
  for (uint32_t i = 0; i < iterations; ++i) {
    const DWORD parent_color = MakeParentColor(i);
    const DWORD child_color = MakeChildColor(i);

    hr = dev->SetRenderTarget(0, surf_a.get());
    if (FAILED(hr)) {
      TerminateProcess(pi.hProcess, 1);
      return aerogpu_test::FailHresult(kTestName, "SetRenderTarget(A)", hr);
    }
    hr = dev->BeginScene();
    if (FAILED(hr)) {
      TerminateProcess(pi.hProcess, 1);
      return aerogpu_test::FailHresult(kTestName, "BeginScene", hr);
    }
    hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, parent_color, 1.0f, 0);
    HRESULT hr_end = dev->EndScene();
    if (FAILED(hr)) {
      TerminateProcess(pi.hProcess, 1);
      return aerogpu_test::FailHresult(kTestName, "Clear(A)", hr);
    }
    if (FAILED(hr_end)) {
      TerminateProcess(pi.hProcess, 1);
      return aerogpu_test::FailHresult(kTestName, "EndScene", hr_end);
    }

    hr = dev->StretchRect(surf_a.get(), NULL, surf_b.get(), NULL, D3DTEXF_NONE);
    if (FAILED(hr)) {
      TerminateProcess(pi.hProcess, 1);
      return aerogpu_test::FailHresult(kTestName, "StretchRect(A->B)", hr);
    }
    rc = WaitForGpuEventQuery(kTestName, dev.get(), q.get(), 5000);
    if (rc != 0) {
      TerminateProcess(pi.hProcess, 1);
      return rc;
    }

    if (!SetEvent(parent_event)) {
      TerminateProcess(pi.hProcess, 1);
      return aerogpu_test::Fail(kTestName,
                                "SetEvent(parent_event) failed: %s",
                                aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    }

    wait = WaitForSingleObject(child_event, 20000);
    if (wait != WAIT_OBJECT_0) {
      TerminateProcess(pi.hProcess, 124);
      return aerogpu_test::Fail(kTestName, "timeout waiting for child event");
    }

    uint32_t pixel = 0;
    rc = ReadSurfacePixel(kTestName, dev.get(), surf_a.get(), sysmem.get(), 2, 2, &pixel);
    if (rc != 0) {
      TerminateProcess(pi.hProcess, 1);
      return rc;
    }
    if (pixel != child_color) {
      wchar_t dump_name[128];
      _snwprintf(dump_name,
                 ARRAYSIZE(dump_name),
                 L"d3d9ex_alloc_id_persistence_parent_dst_%u.bmp",
                 (unsigned)i);
      dump_name[ARRAYSIZE(dump_name) - 1] = 0;
      MaybeDumpSurface(dump_name, dump, sysmem.get(), kSize, kSize);
      TerminateProcess(pi.hProcess, 1);
      return aerogpu_test::Fail(kTestName,
                                "A mismatch @iter=%u: got=0x%08lX expected=0x%08lX",
                                (unsigned)i,
                                (unsigned long)pixel,
                                (unsigned long)child_color);
    }
  }

  wait = WaitForSingleObject(pi.hProcess, 20000);
  if (wait != WAIT_OBJECT_0) {
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 2000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) CloseHandle(job);
    CloseHandle(ready_event);
    CloseHandle(parent_event);
    CloseHandle(child_event);
    UnmapViewOfFile(ipc);
    CloseHandle(map);
    return aerogpu_test::Fail(kTestName, "child did not exit cleanly");
  }

  DWORD exit_code = 1;
  if (!GetExitCodeProcess(pi.hProcess, &exit_code)) {
    exit_code = 1;
  }

  CloseHandle(pi.hThread);
  CloseHandle(pi.hProcess);
  if (job) CloseHandle(job);
  CloseHandle(ready_event);
  CloseHandle(parent_event);
  CloseHandle(child_event);
  UnmapViewOfFile(ipc);
  CloseHandle(map);
  CloseHandle(shared_parent);

  if (exit_code != 0) {
    return aerogpu_test::Fail(kTestName, "child failed with exit code %lu", (unsigned long)exit_code);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  if (aerogpu_test::HasArg(argc, argv, "--child")) {
    return RunChild(argc, argv);
  }
  return RunParent(argc, argv);
}
