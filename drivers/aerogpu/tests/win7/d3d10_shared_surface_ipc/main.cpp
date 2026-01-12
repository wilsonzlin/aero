#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d10.h>
#include <dxgi.h>

#include <cerrno>
#include <string>
#include <vector>

using aerogpu_test::ComPtr;
using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static bool MapSharedHandleToken(HANDLE shared_handle, uint32_t* out_token, std::string* err) {
  if (out_token) {
    *out_token = 0;
  }
  if (!shared_handle) {
    if (err) {
      *err = "invalid shared_handle";
    }
    return false;
  }

  D3DKMT_FUNCS kmt;
  std::string kmt_err;
  if (!aerogpu_test::kmt::LoadD3DKMT(&kmt, &kmt_err)) {
    if (err) {
      *err = kmt_err;
    }
    return false;
  }

  D3DKMT_HANDLE adapter = 0;
  if (!aerogpu_test::kmt::OpenPrimaryAdapter(&kmt, &adapter, &kmt_err)) {
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    if (err) {
      *err = kmt_err;
    }
    return false;
  }

  uint32_t token = 0;
  NTSTATUS st = 0;
  const bool ok = aerogpu_test::kmt::AerogpuMapSharedHandleDebugToken(&kmt,
                                                                     adapter,
                                                                     (unsigned long long)(uintptr_t)shared_handle,
                                                                     &token,
                                                                     &st);

  aerogpu_test::kmt::CloseAdapter(&kmt, adapter);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);

  if (!ok) {
    if (err) {
      if (st == 0) {
        *err = "MAP_SHARED_HANDLE returned debug_token=0";
      } else {
        char buf[96];
        _snprintf(buf, sizeof(buf), "D3DKMTEscape(map-shared-handle) failed (NTSTATUS=0x%08lX)", (unsigned long)st);
        buf[sizeof(buf) - 1] = 0;
        *err = buf;
      }
    }
    return false;
  }

  if (out_token) {
    *out_token = token;
  }
  return token != 0;
}

static int FailD3D10WithRemovedReason(aerogpu_test::TestReporter* reporter,
                                      const char* test_name,
                                      const char* what,
                                      HRESULT hr,
                                      ID3D10Device* device) {
  if (device) {
    HRESULT reason = device->GetDeviceRemovedReason();
    if (FAILED(reason)) {
      aerogpu_test::PrintfStdout("INFO: %s: device removed reason: %s",
                                 test_name,
                                 aerogpu_test::HresultToString(reason).c_str());
    }
  }
  if (reporter) {
    return reporter->FailHresult(what, hr);
  }
  return aerogpu_test::FailHresult(test_name, what, hr);
}

// Minimal NT structures needed to patch a suspended child process command line in-place.
// Keep this self-contained (avoid winternl.h) so the test builds cleanly with the VS2010 + Win7 SDK toolchain.
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
      *err =
          "ReadProcessMemory(CommandLine) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
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

  for (size_t i = 0; i < digits_len; ++i) {
    if (cmdline[digits_pos + i] == 0) {
      if (err) {
        *err = "Child command line placeholder is too short";
      }
      return false;
    }
    cmd_buf[digits_pos + i] = digits[i];
  }

  const size_t write_offset_bytes = digits_pos * sizeof(wchar_t);
  const size_t write_size_bytes = digits_len * sizeof(wchar_t);

  bytes = 0;
  if (!WriteProcessMemory(child_process,
                          (BYTE*)params.CommandLine.Buffer + write_offset_bytes,
                          &cmd_buf[digits_pos],
                          write_size_bytes,
                          &bytes) ||
      bytes != write_size_bytes) {
    if (err) {
      *err = "WriteProcessMemory(CommandLine digits) failed: " + aerogpu_test::Win32ErrorToString(GetLastError());
    }
    return false;
  }

  return true;
}

static int CreateD3D10Device(aerogpu_test::TestReporter* reporter,
                             const char* test_name,
                             ComPtr<ID3D10Device>* out_device) {
  if (!out_device) {
    return reporter ? reporter->Fail("CreateD3D10Device: out_device is NULL") : 1;
  }

  ComPtr<ID3D10Device> device;
  HRESULT hr = D3D10CreateDevice(NULL,
                                 D3D10_DRIVER_TYPE_HARDWARE,
                                 NULL,
                                 D3D10_CREATE_DEVICE_BGRA_SUPPORT,
                                 D3D10_SDK_VERSION,
                                 device.put());
  if (FAILED(hr)) {
    return FailD3D10WithRemovedReason(reporter, test_name, "D3D10CreateDevice(HARDWARE)", hr, NULL);
  }

  // Ensure we are exercising the D3D10 runtime path (d3d10.dll) which uses the
  // UMD's OpenAdapter10 entrypoint.
  if (!GetModuleHandleW(L"d3d10.dll")) {
    return reporter ? reporter->Fail("d3d10.dll is not loaded") : 1;
  }

  out_device->reset(device.detach());
  return 0;
}

static int ValidateAdapter(aerogpu_test::TestReporter* reporter,
                           const char* test_name,
                           ID3D10Device* device,
                           bool allow_microsoft,
                           bool allow_non_aerogpu,
                           bool has_require_vid,
                           uint32_t require_vid,
                           bool has_require_did,
                           uint32_t require_did) {
  if (!device) {
    return reporter ? reporter->Fail("ValidateAdapter: device is NULL") : 1;
  }

  ComPtr<IDXGIDevice> dxgi_device;
  HRESULT hr = device->QueryInterface(__uuidof(IDXGIDevice), (void**)dxgi_device.put());
  if (FAILED(hr)) {
    if (has_require_vid || has_require_did) {
      return reporter ? reporter->FailHresult("QueryInterface(IDXGIDevice) (required for --require-vid/--require-did)", hr) : 1;
    }
    return 0;
  }

  ComPtr<IDXGIAdapter> adapter;
  HRESULT hr_adapter = dxgi_device->GetAdapter(adapter.put());
  if (FAILED(hr_adapter)) {
    if (has_require_vid || has_require_did) {
      return reporter ? reporter->FailHresult("IDXGIDevice::GetAdapter (required for --require-vid/--require-did)", hr_adapter) : 1;
    }
    return 0;
  }

  DXGI_ADAPTER_DESC ad;
  ZeroMemory(&ad, sizeof(ad));
  HRESULT hr_desc = adapter->GetDesc(&ad);
  if (FAILED(hr_desc)) {
    if (has_require_vid || has_require_did) {
      return reporter ? reporter->FailHresult("IDXGIAdapter::GetDesc (required for --require-vid/--require-did)", hr_desc) : 1;
    }
    return 0;
  }

  aerogpu_test::PrintfStdout("INFO: %s: adapter: %ls (VID=0x%04X DID=0x%04X)",
                             test_name,
                             ad.Description,
                             (unsigned)ad.VendorId,
                             (unsigned)ad.DeviceId);
  if (reporter) {
    reporter->SetAdapterInfoW(ad.Description, ad.VendorId, ad.DeviceId);
  }

  if (!allow_microsoft && ad.VendorId == 0x1414) {
    return reporter ? reporter->Fail(
                          "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). Install AeroGPU driver or pass --allow-microsoft.",
                          (unsigned)ad.VendorId,
                          (unsigned)ad.DeviceId)
                    : 1;
  }
  if (has_require_vid && ad.VendorId != require_vid) {
    return reporter ? reporter->Fail("adapter VID mismatch: got 0x%04X expected 0x%04X",
                                     (unsigned)ad.VendorId,
                                     (unsigned)require_vid)
                    : 1;
  }
  if (has_require_did && ad.DeviceId != require_did) {
    return reporter ? reporter->Fail("adapter DID mismatch: got 0x%04X expected 0x%04X",
                                     (unsigned)ad.DeviceId,
                                     (unsigned)require_did)
                    : 1;
  }
  if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
      !(ad.VendorId == 0x1414 && allow_microsoft) &&
      !aerogpu_test::StrIContainsW(ad.Description, L"AeroGPU")) {
    return reporter ? reporter->Fail(
                          "adapter does not look like AeroGPU: %ls (pass --allow-non-aerogpu or use --require-vid/--require-did)",
                          ad.Description)
                    : 1;
  }

  return 0;
}

static int ReadbackExpectedPixel(aerogpu_test::TestReporter* reporter,
                                 const char* test_name,
                                 ID3D10Device* device,
                                 ID3D10Texture2D* tex,
                                 bool dump,
                                 const std::wstring& dump_bmp_path,
                                 uint32_t* out_pixel) {
  if (!device || !tex || !out_pixel) {
    return reporter ? reporter->Fail("ReadbackExpectedPixel: invalid args") : 1;
  }

  D3D10_TEXTURE2D_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  tex->GetDesc(&desc);

  D3D10_TEXTURE2D_DESC st_desc = desc;
  st_desc.BindFlags = 0;
  st_desc.MiscFlags = 0;
  st_desc.Usage = D3D10_USAGE_STAGING;
  st_desc.CPUAccessFlags = D3D10_CPU_ACCESS_READ;

  ComPtr<ID3D10Texture2D> staging;
  HRESULT hr = device->CreateTexture2D(&st_desc, NULL, staging.put());
  if (FAILED(hr)) {
    return FailD3D10WithRemovedReason(reporter, test_name, "CreateTexture2D(STAGING)", hr, device);
  }

  device->CopyResource(staging.get(), tex);

  // Ensure the copy is complete before mapping.
  ComPtr<ID3D10Query> query;
  D3D10_QUERY_DESC qd;
  qd.Query = D3D10_QUERY_EVENT;
  qd.MiscFlags = 0;
  hr = device->CreateQuery(&qd, query.put());
  if (FAILED(hr)) {
    return FailD3D10WithRemovedReason(reporter, test_name, "CreateQuery(EVENT)", hr, device);
  }
  query->End();
  device->Flush();
  while (S_FALSE == query->GetData(NULL, 0, 0)) {
    Sleep(0);
  }

  D3D10_MAPPED_TEXTURE2D map;
  ZeroMemory(&map, sizeof(map));
  hr = staging->Map(0, D3D10_MAP_READ, 0, &map);
  if (FAILED(hr)) {
    return FailD3D10WithRemovedReason(reporter, test_name, "Map(STAGING)", hr, device);
  }

  const uint32_t pixel = aerogpu_test::ReadPixelBGRA(map.pData, (int)map.RowPitch, 0, 0);
  *out_pixel = pixel;

  if (dump) {
    std::string err;
    if (!aerogpu_test::WriteBmp32BGRA(dump_bmp_path,
                                      (int)desc.Width,
                                      (int)desc.Height,
                                      map.pData,
                                      (int)map.RowPitch,
                                      &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", test_name, err.c_str());
    } else if (reporter) {
      reporter->AddArtifactPathW(dump_bmp_path);
    }
  }

  staging->Unmap(0);
  return 0;
}

static int RunConsumer(int argc, char** argv) {
  const char* kTestName = "d3d10_shared_surface_ipc_consumer";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe --consumer --shared-handle=0xNNNN [--expected-debug-token=0x########] [--dump] [--json[=PATH]] [--require-vid=0x####] "
        "[--require-did=0x####] [--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const std::wstring dump_bmp_path =
      aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d10_shared_surface_ipc.bmp");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");

  uint32_t expected_debug_token = 0;
  bool has_expected_debug_token = false;
  std::string expected_token_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--expected-debug-token", &expected_token_str) &&
      !expected_token_str.empty()) {
    std::string err;
    if (!aerogpu_test::ParseUint32(expected_token_str, &expected_debug_token, &err) || expected_debug_token == 0) {
      return reporter.Fail("invalid --expected-debug-token: %s", err.c_str());
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

  if (has_expected_debug_token) {
    uint32_t token = 0;
    std::string map_err;
    if (!MapSharedHandleToken(shared_handle, &token, &map_err)) {
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

  ComPtr<ID3D10Device> device;
  int rc = CreateD3D10Device(&reporter, kTestName, &device);
  if (rc != 0) {
    return rc;
  }

  rc = ValidateAdapter(&reporter,
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

  ComPtr<ID3D10Texture2D> shared_tex;
  HRESULT hr = device->OpenSharedResource(shared_handle, __uuidof(ID3D10Texture2D), (void**)shared_tex.put());
  if (FAILED(hr)) {
    // Some implementations may return an ID3D10Resource; fall back to opening that and QI for Texture2D.
    ComPtr<ID3D10Resource> res;
    HRESULT hr_res = device->OpenSharedResource(shared_handle, __uuidof(ID3D10Resource), (void**)res.put());
    if (FAILED(hr_res)) {
      return reporter.FailHresult("OpenSharedResource(ID3D10Texture2D/ID3D10Resource)", hr);
    }
    hr_res = res->QueryInterface(__uuidof(ID3D10Texture2D), (void**)shared_tex.put());
    if (FAILED(hr_res)) {
      return reporter.FailHresult("QueryInterface(ID3D10Texture2D) after OpenSharedResource", hr_res);
    }
  }
  if (!shared_tex) {
    return reporter.Fail("OpenSharedResource returned NULL texture");
  }

  uint32_t pixel = 0;
  rc = ReadbackExpectedPixel(&reporter, kTestName, device.get(), shared_tex.get(), dump, dump_bmp_path, &pixel);
  if (rc != 0) {
    return rc;
  }

  const uint32_t expected = 0xFF112233u;
  if ((pixel & 0x00FFFFFFu) != (expected & 0x00FFFFFFu)) {
    return reporter.Fail("readback mismatch: got=0x%08lX expected=0x%08lX",
                         (unsigned long)pixel,
                         (unsigned long)expected);
  }

  return reporter.Pass();
}

static int RunProducer(int argc, char** argv) {
  const char* kTestName = "d3d10_shared_surface_ipc";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const std::wstring dump_bmp_path =
      aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d10_shared_surface_ipc.bmp");
  if (dump) {
    // Ensure we don't report a stale BMP from a previous run if the consumer fails before dumping.
    DeleteFileW(dump_bmp_path.c_str());
  }
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

  ComPtr<ID3D10Device> device;
  int rc = CreateD3D10Device(&reporter, kTestName, &device);
  if (rc != 0) {
    return rc;
  }

  rc = ValidateAdapter(&reporter,
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

  const int kWidth = 64;
  const int kHeight = 64;

  D3D10_TEXTURE2D_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  desc.Width = kWidth;
  desc.Height = kHeight;
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
  desc.SampleDesc.Count = 1;
  desc.SampleDesc.Quality = 0;
  desc.Usage = D3D10_USAGE_DEFAULT;
  desc.BindFlags = D3D10_BIND_RENDER_TARGET;
  desc.CPUAccessFlags = 0;
  desc.MiscFlags = D3D10_RESOURCE_MISC_SHARED;

  ComPtr<ID3D10Texture2D> tex;
  HRESULT hr = device->CreateTexture2D(&desc, NULL, tex.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateTexture2D(shared)", hr);
  }
  if (!tex) {
    return reporter.Fail("CreateTexture2D(shared) returned NULL texture");
  }

  ComPtr<IDXGIResource> dxgi_res;
  hr = tex->QueryInterface(__uuidof(IDXGIResource), (void**)dxgi_res.put());
  if (FAILED(hr) || !dxgi_res) {
    return reporter.FailHresult("QueryInterface(IDXGIResource)", hr);
  }

  HANDLE shared = NULL;
  hr = dxgi_res->GetSharedHandle(&shared);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDXGIResource::GetSharedHandle", hr);
  }
  if (!shared) {
    return reporter.Fail("IDXGIResource::GetSharedHandle returned NULL");
  }
  aerogpu_test::PrintfStdout("INFO: %s: created shared texture handle=%p", kTestName, shared);

  ComPtr<ID3D10RenderTargetView> rtv;
  hr = device->CreateRenderTargetView(tex.get(), NULL, rtv.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateRenderTargetView(shared)", hr);
  }

  ID3D10RenderTargetView* rtvs[] = {rtv.get()};
  device->OMSetRenderTargets(1, rtvs, NULL);

  // Clear to 0xFF112233 (AARRGGBB). BGRA bytes = (0x33,0x22,0x11,0xFF).
  const float clear_rgba[4] = {
      0x11 / 255.0f,
      0x22 / 255.0f,
      0x33 / 255.0f,
      1.0f,
  };
  device->ClearRenderTargetView(rtv.get(), clear_rgba);

  // Ensure the clear has completed before the consumer opens/reads the surface.
  uint32_t local_pixel = 0;
  rc = ReadbackExpectedPixel(NULL, kTestName, device.get(), tex.get(), false, dump_bmp_path, &local_pixel);
  if (rc != 0) {
    return rc;
  }
  const uint32_t expected = 0xFF112233u;
  if ((local_pixel & 0x00FFFFFFu) != (expected & 0x00FFFFFFu)) {
    return reporter.Fail("producer local readback mismatch: got=0x%08lX expected=0x%08lX",
                         (unsigned long)local_pixel,
                         (unsigned long)expected);
  }

  wchar_t exe_path[MAX_PATH];
  DWORD exe_len = GetModuleFileNameW(NULL, exe_path, ARRAYSIZE(exe_path));
  if (!exe_len || exe_len >= ARRAYSIZE(exe_path)) {
    return reporter.Fail("GetModuleFileNameW failed");
  }

  // Create the consumer suspended with a fixed-width placeholder for --shared-handle=0x...
  // We patch the placeholder digits in the child's command line before resuming it.
  std::wstring cmdline = std::wstring(L"\"") + exe_path +
                         L"\" --consumer --shared-handle=0x0000000000000000";
  if (dump) {
    cmdline += L" --dump";
  }
  uint32_t debug_token = 0;
  std::string map_err;
  const bool have_debug_token = MapSharedHandleToken(shared, &debug_token, &map_err);
  if (have_debug_token) {
    aerogpu_test::PrintfStdout("INFO: %s: MAP_SHARED_HANDLE debug_token=%lu", kTestName, (unsigned long)debug_token);
    wchar_t token_buf[32];
    _snwprintf(token_buf, ARRAYSIZE(token_buf), L"0x%08lX", (unsigned long)debug_token);
    token_buf[ARRAYSIZE(token_buf) - 1] = 0;
    cmdline += L" --expected-debug-token=";
    cmdline += token_buf;
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: MAP_SHARED_HANDLE unavailable (%s); skipping token validation",
                               kTestName,
                               map_err.c_str());
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

  // The shared handle is an NT handle; duplicate it into the consumer process so the consumer uses
  // the (potentially different) child handle value. This catches bugs where the driver incorrectly
  // treats the numeric handle value as a stable cross-process token.
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
    const bool strict_mode = (require_umd || (!allow_microsoft && !allow_non_aerogpu));
    if (strict_mode) {
      TerminateProcess(pi.hProcess, 2);
      CloseHandle(pi.hThread);
      CloseHandle(pi.hProcess);
      if (job) {
        CloseHandle(job);
      }
      CloseHandle(shared);
      return reporter.Fail("DuplicateHandle(shared) failed: %s", aerogpu_test::Win32ErrorToString(werr).c_str());
    }

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
      bool got_different_value = false;
      for (int attempt = 0; attempt < 8; ++attempt) {
        HANDLE tmp = NULL;
        ok = DuplicateHandle(GetCurrentProcess(),
                             shared,
                             pi.hProcess,
                             &tmp,
                             0,
                             FALSE,
                             DUPLICATE_SAME_ACCESS);
        if (!ok || tmp == NULL) {
          break;
        }
        shared_in_child = tmp;
        if ((uintptr_t)shared_in_child != (uintptr_t)shared) {
          got_different_value = true;
          aerogpu_test::PrintfStdout(
              "INFO: %s: re-duplicated shared handle to avoid numeric collision: now %p (consumer)",
              kTestName,
              shared_in_child);
          break;
        }
      }
      if (!got_different_value) {
        aerogpu_test::PrintfStdout(
            "INFO: %s: duplicated shared handle is numerically identical across processes; continuing anyway",
            kTestName);
      }
    }
  }

  std::string patch_err;
  if (!PatchRemoteCommandLineHandleDigits(pi.hProcess, L"--shared-handle=0x", shared_in_child, &patch_err)) {
    TerminateProcess(pi.hProcess, 2);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (job) {
      CloseHandle(job);
    }
    return reporter.Fail("failed to patch child command line: %s", patch_err.c_str());
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

  if (exit_code != 0) {
    return reporter.Fail("consumer failed: exit_code=%lu", (unsigned long)exit_code);
  }

  if (dump) {
    reporter.AddArtifactPathIfExistsW(dump_bmp_path);
  }

  // Close our copy of the shared handle (the texture and consumer remain live while needed).
  CloseHandle(shared);
  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  if (aerogpu_test::HasArg(argc, argv, "--consumer")) {
    return RunConsumer(argc, argv);
  }
  return RunProducer(argc, argv);
}
