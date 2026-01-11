#include "..\\common\\aerogpu_test_common.h"

#include <d3d9.h>
#include <winternl.h>

using aerogpu_test::ComPtr;

typedef NTSTATUS(WINAPI* NtQueryInformationProcessFn)(HANDLE ProcessHandle,
                                                      PROCESSINFOCLASS ProcessInformationClass,
                                                      PVOID ProcessInformation,
                                                      ULONG ProcessInformationLength,
                                                      PULONG ReturnLength);

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
    if (err) {
      *err = "GetModuleHandleW(ntdll.dll) failed";
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

  PROCESS_BASIC_INFORMATION pbi;
  ZeroMemory(&pbi, sizeof(pbi));
  ULONG ret_len = 0;
  NTSTATUS status = NtQueryInformationProcess(child_process,
                                              ProcessBasicInformation,
                                              &pbi,
                                              sizeof(pbi),
                                              &ret_len);
  if (status != 0) {
    if (err) {
      char buf[64];
      _snprintf(buf, sizeof(buf), "NtQueryInformationProcess failed: 0x%08lX",
                (unsigned long)status);
      *err = buf;
    }
    return false;
  }

  PEB peb;
  ZeroMemory(&peb, sizeof(peb));
  SIZE_T bytes = 0;
  if (!ReadProcessMemory(child_process, pbi.PebBaseAddress, &peb, sizeof(peb), &bytes) ||
      bytes != sizeof(peb)) {
    if (err) {
      *err = "ReadProcessMemory(PEB) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  RTL_USER_PROCESS_PARAMETERS params;
  ZeroMemory(&params, sizeof(params));
  bytes = 0;
  if (!ReadProcessMemory(child_process,
                         peb.ProcessParameters,
                         &params,
                         sizeof(params),
                         &bytes) ||
      bytes != sizeof(params)) {
    if (err) {
      *err = "ReadProcessMemory(ProcessParameters) failed: " +
             aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  UNICODE_STRING cmd = params.CommandLine;
  if (!cmd.Buffer || cmd.Length == 0) {
    if (err) {
      *err = "Child command line buffer missing";
    }
    return false;
  }
  if (cmd.Length % sizeof(wchar_t) != 0) {
    if (err) {
      *err = "Child command line length is not wchar_t aligned";
    }
    return false;
  }

  const size_t cmd_chars = (size_t)(cmd.Length / sizeof(wchar_t));
  std::vector<wchar_t> cmd_buf(cmd_chars + 1, 0);
  bytes = 0;
  if (!ReadProcessMemory(child_process, cmd.Buffer, &cmd_buf[0], cmd.Length, &bytes) ||
      bytes != cmd.Length) {
    if (err) {
      *err =
          "ReadProcessMemory(CommandLine) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
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
  LPVOID remote_dst =
      (LPVOID)((uintptr_t)cmd.Buffer + digits_pos * sizeof(wchar_t));  // NOLINT
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

static int RunConsumer(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface_ipc_consumer";

  std::string handle_str;
  if (!aerogpu_test::GetArgValue(argc, argv, "--shared-handle", &handle_str)) {
    return aerogpu_test::Fail(kTestName, "missing --shared-handle");
  }

  errno = 0;
  char* end = NULL;
  unsigned __int64 hv = _strtoui64(handle_str.c_str(), &end, 0);
  if (errno == ERANGE || !end || end == handle_str.c_str() || *end != 0) {
    return aerogpu_test::Fail(kTestName, "invalid --shared-handle value: %s", handle_str.c_str());
  }

  const HANDLE shared_handle = (HANDLE)(uintptr_t)hv;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurfaceIPC_Consumer",
                                              L"AeroGPU D3D9Ex Shared Surface IPC (Consumer)",
                                              64,
                                              64,
                                              false);
  if (!hwnd) {
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(kTestName, hwnd, &d3d, &dev);
  if (rc != 0) {
    return rc;
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
    return aerogpu_test::FailHresult(kTestName, "CreateTexture(open shared)", hr);
  }

  ComPtr<IDirect3DSurface9> surf;
  hr = tex->GetSurfaceLevel(0, surf.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DTexture9::GetSurfaceLevel", hr);
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(64,
                                        64,
                                        D3DFMT_A8R8G8B8,
                                        D3DPOOL_SYSTEMMEM,
                                        sysmem.put(),
                                        NULL);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateOffscreenPlainSurface", hr);
  }

  hr = dev->GetRenderTargetData(surf.get(), sysmem.get());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "GetRenderTargetData(shared)", hr);
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DSurface9::LockRect", hr);
  }

  const uint32_t pixel = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 2, 2);
  sysmem->UnlockRect();

  const uint32_t expected = 0xFF112233u;  // BGRA = (0x33,0x22,0x11,0xFF).
  if ((pixel & 0x00FFFFFFu) != (expected & 0x00FFFFFFu)) {
    return aerogpu_test::Fail(kTestName,
                              "pixel mismatch: got=0x%08lX expected=0x%08lX",
                              (unsigned long)pixel,
                              (unsigned long)expected);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

static int RunProducer(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface_ipc";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--hidden] [--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] "
        "[--allow-non-aerogpu]",
        kTestName);
    return 0;
  }

  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");

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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurfaceIPC_Producer",
                                              L"AeroGPU D3D9Ex Shared Surface IPC (Producer)",
                                              64,
                                              64,
                                              !hidden);
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
    return aerogpu_test::FailHresult(kTestName, "CreateTexture(shared)", hr);
  }
  if (!shared) {
    return aerogpu_test::Fail(kTestName, "CreateTexture returned NULL shared handle");
  }

  ComPtr<IDirect3DSurface9> rt;
  hr = tex->GetSurfaceLevel(0, rt.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DTexture9::GetSurfaceLevel", hr);
  }

  hr = dev->SetRenderTarget(0, rt.get());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "SetRenderTarget(shared)", hr);
  }

  const DWORD clear_color = D3DCOLOR_ARGB(0xFF, 0x11, 0x22, 0x33);  // 0xFF112233.
  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "BeginScene", hr);
  }
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, clear_color, 1.0f, 0);
  dev->EndScene();
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "Clear(shared)", hr);
  }

  // Ensure the clear has completed before the consumer opens/reads the surface.
  ComPtr<IDirect3DQuery9> q;
  hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, q.put());
  if (FAILED(hr) || !q) {
    return aerogpu_test::FailHresult(kTestName, "CreateQuery(D3DQUERYTYPE_EVENT)", hr);
  }
  hr = q->Issue(D3DISSUE_END);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DQuery9::Issue", hr);
  }

  const DWORD start = GetTickCount();
  for (;;) {
    hr = q->GetData(NULL, 0, D3DGETDATA_FLUSH);
    if (hr == S_OK) {
      break;
    }
    if (hr != S_FALSE) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DQuery9::GetData", hr);
    }
    if (GetTickCount() - start > 5000) {
      return aerogpu_test::Fail(kTestName, "GPU event query timed out");
    }
    Sleep(0);
  }

  wchar_t exe_path[MAX_PATH];
  DWORD exe_len = GetModuleFileNameW(NULL, exe_path, ARRAYSIZE(exe_path));
  if (!exe_len || exe_len >= ARRAYSIZE(exe_path)) {
    return aerogpu_test::Fail(kTestName, "GetModuleFileNameW failed");
  }

  // Create the consumer suspended with a fixed-width placeholder for --shared-handle=0x...
  // We patch the placeholder digits in the child's command line before resuming it.
  const std::wstring cmdline = std::wstring(L"\"") + exe_path +
                               L"\" --consumer --shared-handle=0x0000000000000000";
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
    return aerogpu_test::Fail(kTestName,
                              "CreateProcessW failed: %s",
                              aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }

  // Duplicate the shared handle into the consumer process. The numeric value must differ across
  // processes; otherwise a buggy driver could accidentally use the raw handle value as a stable key.
  HANDLE shared_in_child = NULL;
  ok = DuplicateHandle(GetCurrentProcess(),
                       shared,
                       pi.hProcess,
                       &shared_in_child,
                       0,
                       FALSE,
                       DUPLICATE_SAME_ACCESS);
  if (!ok) {
    DWORD werr = GetLastError();
    TerminateProcess(pi.hProcess, 1);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    return aerogpu_test::Fail(kTestName,
                              "DuplicateHandle failed: %s",
                              aerogpu_test::Win32ErrorToString(werr).c_str());
  }
  if ((uintptr_t)shared_in_child == (uintptr_t)shared) {
    // Extremely unlikely but possible if the consumer's handle table happens to allocate the same
    // numeric value. Duplicate again to guarantee numeric instability across processes.
    HANDLE shared_in_child2 = NULL;
    ok = DuplicateHandle(GetCurrentProcess(),
                         shared,
                         pi.hProcess,
                         &shared_in_child2,
                         0,
                         FALSE,
                         DUPLICATE_SAME_ACCESS);
    if (ok) {
      shared_in_child = shared_in_child2;
    }
  }
  if ((uintptr_t)shared_in_child == (uintptr_t)shared) {
    TerminateProcess(pi.hProcess, 1);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    return aerogpu_test::Fail(kTestName,
                              "refusing to run: shared handle value is numerically identical across processes");
  }

  std::string patch_err;
  if (!PatchRemoteCommandLineSharedHandle(pi.hProcess, shared_in_child, &patch_err)) {
    TerminateProcess(pi.hProcess, 1);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    return aerogpu_test::Fail(kTestName, "failed to patch consumer command line: %s", patch_err.c_str());
  }

  ResumeThread(pi.hThread);

  DWORD wait = WaitForSingleObject(pi.hProcess, 10000);
  if (wait != WAIT_OBJECT_0) {
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 2000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    return aerogpu_test::Fail(kTestName, "consumer timed out");
  }

  DWORD exit_code = 1;
  if (!GetExitCodeProcess(pi.hProcess, &exit_code)) {
    exit_code = 1;
  }

  CloseHandle(pi.hThread);
  CloseHandle(pi.hProcess);
  CloseHandle(shared);

  if (exit_code != 0) {
    return aerogpu_test::Fail(kTestName, "consumer failed with exit code %lu", (unsigned long)exit_code);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  if (aerogpu_test::HasArg(argc, argv, "--consumer")) {
    return RunConsumer(argc, argv);
  }
  return RunProducer(argc, argv);
}

