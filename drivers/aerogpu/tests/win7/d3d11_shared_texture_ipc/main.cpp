#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d11.h>
#include <dxgi.h>

#include <errno.h>

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
      buf[sizeof(buf) - 1] = 0;
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
      *err =
          "ReadProcessMemory(ProcessParameters) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
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
      *err = "WriteProcessMemory(CommandLine digits) failed: " +
             aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  return true;
}

static bool ParseUint64(const std::string& s, unsigned __int64* out, std::string* err) {
  if (out) {
    *out = 0;
  }
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
  if (out) {
    *out = v;
  }
  return true;
}

static int ValidateAdapter(aerogpu_test::TestReporter* reporter,
                           const char* test_name,
                           ID3D11Device* device,
                           bool allow_microsoft,
                           bool allow_non_aerogpu,
                           bool has_require_vid,
                           uint32_t require_vid,
                           bool has_require_did,
                           uint32_t require_did) {
  if (!device) {
    return reporter->Fail("ValidateAdapter: device == NULL");
  }

  ComPtr<IDXGIDevice> dxgi_device;
  HRESULT hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (FAILED(hr) || !dxgi_device) {
    if (has_require_vid || has_require_did) {
      return reporter->FailHresult("QueryInterface(IDXGIDevice) (required for --require-vid/--require-did)", hr);
    }
    return 0;
  }

  ComPtr<IDXGIAdapter> adapter;
  hr = dxgi_device->GetAdapter(adapter.put());
  if (FAILED(hr) || !adapter) {
    if (has_require_vid || has_require_did) {
      return reporter->FailHresult("IDXGIDevice::GetAdapter (required for --require-vid/--require-did)", hr);
    }
    return 0;
  }

  DXGI_ADAPTER_DESC ad;
  ZeroMemory(&ad, sizeof(ad));
  hr = adapter->GetDesc(&ad);
  if (FAILED(hr)) {
    if (has_require_vid || has_require_did) {
      return reporter->FailHresult("IDXGIAdapter::GetDesc (required for --require-vid/--require-did)", hr);
    }
    return 0;
  }

  aerogpu_test::PrintfStdout("INFO: %s: adapter: %ls (VID=0x%04X DID=0x%04X)",
                             test_name,
                             ad.Description,
                             (unsigned)ad.VendorId,
                             (unsigned)ad.DeviceId);
  reporter->SetAdapterInfoW(ad.Description, ad.VendorId, ad.DeviceId);

  if (!allow_microsoft && ad.VendorId == 0x1414) {
    return reporter->Fail(
        "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). Install AeroGPU driver or pass --allow-microsoft.",
        (unsigned)ad.VendorId,
        (unsigned)ad.DeviceId);
  }
  if (has_require_vid && ad.VendorId != require_vid) {
    return reporter->Fail("adapter VID mismatch: got 0x%04X expected 0x%04X",
                          (unsigned)ad.VendorId,
                          (unsigned)require_vid);
  }
  if (has_require_did && ad.DeviceId != require_did) {
    return reporter->Fail("adapter DID mismatch: got 0x%04X expected 0x%04X",
                          (unsigned)ad.DeviceId,
                          (unsigned)require_did);
  }
  if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
      !(ad.VendorId == 0x1414 && allow_microsoft) &&
      !aerogpu_test::StrIContainsW(ad.Description, L"AeroGPU")) {
    return reporter->Fail("adapter does not look like AeroGPU: %ls (pass --allow-non-aerogpu or use --require-vid/--require-did)",
                          ad.Description);
  }
  return 0;
}

static int WaitForGpuIdle(aerogpu_test::TestReporter* reporter,
                          const char* test_name,
                          ID3D11Device* device,
                          ID3D11DeviceContext* context) {
  if (!device || !context) {
    return reporter->Fail("WaitForGpuIdle: invalid device/context");
  }

  D3D11_QUERY_DESC qdesc;
  ZeroMemory(&qdesc, sizeof(qdesc));
  qdesc.Query = D3D11_QUERY_EVENT;
  qdesc.MiscFlags = 0;

  ComPtr<ID3D11Query> q;
  HRESULT hr = device->CreateQuery(&qdesc, q.put());
  if (FAILED(hr) || !q) {
    return reporter->FailHresult("ID3D11Device::CreateQuery(D3D11_QUERY_EVENT)", hr);
  }
  context->End(q.get());

  const DWORD start = GetTickCount();
  for (;;) {
    hr = context->GetData(q.get(), NULL, 0, 0);
    if (hr == S_OK) {
      break;
    }
    if (hr != S_FALSE) {
      return reporter->FailHresult("ID3D11DeviceContext::GetData(D3D11_QUERY_EVENT)", hr);
    }
    if (GetTickCount() - start > 5000) {
      return reporter->Fail("GPU event query timed out");
    }
    Sleep(0);
  }
  return 0;
}

static int RunConsumer(int argc, char** argv) {
  const char* kTestName = "d3d11_shared_texture_ipc_consumer";
  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  uint32_t require_vid = 0;
  uint32_t require_did = 0;
  bool has_require_vid = false;
  bool has_require_did = false;
  std::string require_vid_str;
  std::string require_did_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--require-vid", &require_vid_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_vid_str, &require_vid, &err)) {
      return reporter.Fail("invalid --require-vid: %s", err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &err)) {
      return reporter.Fail("invalid --require-did: %s", err.c_str());
    }
    has_require_did = true;
  }

  std::string handle_str;
  if (!aerogpu_test::GetArgValue(argc, argv, "--shared-handle", &handle_str) || handle_str.empty()) {
    return reporter.Fail("missing --shared-handle");
  }

  unsigned __int64 handle_u64 = 0;
  std::string parse_err;
  if (!ParseUint64(handle_str, &handle_u64, &parse_err)) {
    return reporter.Fail("invalid --shared-handle: %s", parse_err.c_str());
  }
  HANDLE shared = (HANDLE)(uintptr_t)handle_u64;

  uint32_t expected_pixel = 0xFFFF0000u;  // BGRA8 = {0,0,255,255} (red).
  std::string expected_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--expected-pixel", &expected_str) && !expected_str.empty()) {
    std::string err;
    if (!aerogpu_test::ParseUint32(expected_str, &expected_pixel, &err)) {
      return reporter.Fail("invalid --expected-pixel: %s", err.c_str());
    }
  }

  D3D_FEATURE_LEVEL feature_levels[] = {D3D_FEATURE_LEVEL_11_0,
                                       D3D_FEATURE_LEVEL_10_1,
                                       D3D_FEATURE_LEVEL_10_0,
                                       D3D_FEATURE_LEVEL_9_3,
                                       D3D_FEATURE_LEVEL_9_2,
                                       D3D_FEATURE_LEVEL_9_1};

  ComPtr<ID3D11Device> device;
  ComPtr<ID3D11DeviceContext> context;
  D3D_FEATURE_LEVEL chosen_level = (D3D_FEATURE_LEVEL)0;
  HRESULT hr = D3D11CreateDevice(NULL,
                                 D3D_DRIVER_TYPE_HARDWARE,
                                 NULL,
                                 D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                 feature_levels,
                                 ARRAYSIZE(feature_levels),
                                 D3D11_SDK_VERSION,
                                 device.put(),
                                 &chosen_level,
                                 context.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("D3D11CreateDevice(HARDWARE)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);

  int rc = ValidateAdapter(&reporter,
                           kTestName,
                           device.get(),
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
    int umd_rc = aerogpu_test::RequireAeroGpuD3D10UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  ComPtr<ID3D11Texture2D> shared_tex;
  hr = device->OpenSharedResource(shared, __uuidof(ID3D11Texture2D), (void**)shared_tex.put());
  if (FAILED(hr) || !shared_tex) {
    return reporter.FailHresult("ID3D11Device::OpenSharedResource(ID3D11Texture2D)", hr);
  }

  // The shared handle can be closed after opening the resource.
  CloseHandle(shared);

  D3D11_TEXTURE2D_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  shared_tex->GetDesc(&desc);

  if (desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM) {
    return reporter.Fail("unexpected texture format: %u (expected DXGI_FORMAT_B8G8R8A8_UNORM=%u)",
                         (unsigned)desc.Format,
                         (unsigned)DXGI_FORMAT_B8G8R8A8_UNORM);
  }

  D3D11_TEXTURE2D_DESC staging_desc = desc;
  staging_desc.Usage = D3D11_USAGE_STAGING;
  staging_desc.BindFlags = 0;
  staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
  staging_desc.MiscFlags = 0;

  ComPtr<ID3D11Texture2D> staging;
  hr = device->CreateTexture2D(&staging_desc, NULL, staging.put());
  if (FAILED(hr) || !staging) {
    return reporter.FailHresult("ID3D11Device::CreateTexture2D(staging)", hr);
  }

  context->CopyResource(staging.get(), shared_tex.get());

  rc = WaitForGpuIdle(&reporter, kTestName, device.get(), context.get());
  if (rc != 0) {
    return rc;
  }

  D3D11_MAPPED_SUBRESOURCE mapped;
  ZeroMemory(&mapped, sizeof(mapped));
  hr = context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &mapped);
  if (FAILED(hr)) {
    return reporter.FailHresult("ID3D11DeviceContext::Map(staging, READ)", hr);
  }

  if (!mapped.pData || mapped.RowPitch < desc.Width * 4u) {
    context->Unmap(staging.get(), 0);
    return reporter.Fail("invalid mapped staging texture: pData=%p RowPitch=%u width=%u",
                         mapped.pData,
                         (unsigned)mapped.RowPitch,
                         (unsigned)desc.Width);
  }

  uint32_t pixel = 0;
  memcpy(&pixel, mapped.pData, sizeof(pixel));
  context->Unmap(staging.get(), 0);

  if (pixel != expected_pixel) {
    return reporter.Fail("pixel mismatch: got 0x%08X expected 0x%08X", (unsigned)pixel, (unsigned)expected_pixel);
  }

  aerogpu_test::PrintfStdout("INFO: %s: OK: pixel=0x%08X", kTestName, (unsigned)pixel);
  return 0;
}

static int RunProducer(int argc, char** argv) {
  const char* kTestName = "d3d11_shared_texture_ipc";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  uint32_t require_vid = 0;
  uint32_t require_did = 0;
  bool has_require_vid = false;
  bool has_require_did = false;
  std::string require_vid_str;
  std::string require_did_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--require-vid", &require_vid_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_vid_str, &require_vid, &err)) {
      return reporter.Fail("invalid --require-vid: %s", err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &err)) {
      return reporter.Fail("invalid --require-did: %s", err.c_str());
    }
    has_require_did = true;
  }

  D3D_FEATURE_LEVEL feature_levels[] = {D3D_FEATURE_LEVEL_11_0,
                                       D3D_FEATURE_LEVEL_10_1,
                                       D3D_FEATURE_LEVEL_10_0,
                                       D3D_FEATURE_LEVEL_9_3,
                                       D3D_FEATURE_LEVEL_9_2,
                                       D3D_FEATURE_LEVEL_9_1};

  ComPtr<ID3D11Device> device;
  ComPtr<ID3D11DeviceContext> context;
  D3D_FEATURE_LEVEL chosen_level = (D3D_FEATURE_LEVEL)0;
  HRESULT hr = D3D11CreateDevice(NULL,
                                 D3D_DRIVER_TYPE_HARDWARE,
                                 NULL,
                                 D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                 feature_levels,
                                 ARRAYSIZE(feature_levels),
                                 D3D11_SDK_VERSION,
                                 device.put(),
                                 &chosen_level,
                                 context.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("D3D11CreateDevice(HARDWARE)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: feature level 0x%04X", kTestName, (unsigned)chosen_level);

  int rc = ValidateAdapter(&reporter,
                           kTestName,
                           device.get(),
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
    int umd_rc = aerogpu_test::RequireAeroGpuD3D10UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  const uint32_t expected_pixel = 0xFFFF0000u;  // BGRA8 = {0,0,255,255} (red).

  D3D11_TEXTURE2D_DESC tex_desc;
  ZeroMemory(&tex_desc, sizeof(tex_desc));
  tex_desc.Width = 64;
  tex_desc.Height = 64;
  tex_desc.MipLevels = 1;
  tex_desc.ArraySize = 1;
  tex_desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  tex_desc.SampleDesc.Count = 1;
  tex_desc.SampleDesc.Quality = 0;
  tex_desc.Usage = D3D11_USAGE_DEFAULT;
  tex_desc.BindFlags = D3D11_BIND_RENDER_TARGET;
  tex_desc.CPUAccessFlags = 0;
  tex_desc.MiscFlags = D3D11_RESOURCE_MISC_SHARED;

  ComPtr<ID3D11Texture2D> tex;
  hr = device->CreateTexture2D(&tex_desc, NULL, tex.put());
  if (FAILED(hr) || !tex) {
    return reporter.FailHresult("ID3D11Device::CreateTexture2D(shared)", hr);
  }

  ComPtr<ID3D11RenderTargetView> rtv;
  hr = device->CreateRenderTargetView(tex.get(), NULL, rtv.put());
  if (FAILED(hr) || !rtv) {
    return reporter.FailHresult("ID3D11Device::CreateRenderTargetView(shared)", hr);
  }

  const FLOAT clear_color[4] = {1.0f, 0.0f, 0.0f, 1.0f};  // red
  context->ClearRenderTargetView(rtv.get(), clear_color);

  rc = WaitForGpuIdle(&reporter, kTestName, device.get(), context.get());
  if (rc != 0) {
    return rc;
  }

  ComPtr<IDXGIResource> dxgi_res;
  hr = tex->QueryInterface(__uuidof(IDXGIResource), (void**)dxgi_res.put());
  if (FAILED(hr) || !dxgi_res) {
    return reporter.FailHresult("ID3D11Texture2D::QueryInterface(IDXGIResource)", hr);
  }

  HANDLE shared = NULL;
  hr = dxgi_res->GetSharedHandle(&shared);
  if (FAILED(hr) || !shared) {
    return reporter.FailHresult("IDXGIResource::GetSharedHandle", hr);
  }

  wchar_t exe_path[MAX_PATH];
  DWORD exe_len = GetModuleFileNameW(NULL, exe_path, ARRAYSIZE(exe_path));
  if (!exe_len || exe_len >= ARRAYSIZE(exe_path)) {
    return reporter.Fail("GetModuleFileNameW failed");
  }

  // Create the consumer suspended with a fixed-width placeholder for --shared-handle=0x...
  // We patch the placeholder digits in the child's command line before resuming it.
  std::wstring cmdline = std::wstring(L"\"") + exe_path +
                         L"\" --consumer --shared-handle=0x0000000000000000 --expected-pixel=0xFFFF0000";
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
    return reporter.Fail("consumer timed out (WaitForSingleObject=%lu)", (unsigned long)wait);
  }

  DWORD exit_code = 0;
  GetExitCodeProcess(pi.hProcess, &exit_code);

  CloseHandle(pi.hThread);
  CloseHandle(pi.hProcess);
  if (job) {
    CloseHandle(job);
  }

  if (exit_code != 0) {
    return reporter.Fail("consumer failed: exit code %lu", (unsigned long)exit_code);
  }

  aerogpu_test::PrintfStdout("INFO: %s: OK: consumer exit code %lu pixel=0x%08X",
                             kTestName,
                             (unsigned long)exit_code,
                             (unsigned)expected_pixel);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  if (aerogpu_test::HasArg(argc, argv, "--consumer")) {
    return RunConsumer(argc, argv);
  }
  return RunProducer(argc, argv);
}

