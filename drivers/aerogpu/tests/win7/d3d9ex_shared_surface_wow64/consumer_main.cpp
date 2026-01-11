#include "wow64_shared_surface_common.h"

#if !defined(_WIN64) && !defined(_M_X64)
#error This target must be built as x64 (the consumer process).
#endif

using namespace d3d9ex_shared_surface_wow64;

static int RunConsumer(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface_wow64_consumer";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe --ipc-map=NAME --ready-event=NAME --done-event=NAME [--dump] [--show] [--json[=PATH]] "
        "[--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] [--allow-non-aerogpu] "
        "[--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool show = aerogpu_test::HasArg(argc, argv, "--show");
  const std::wstring dump_bmp_path =
      aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9ex_shared_surface_wow64.bmp");
  if (dump) {
    // Avoid consuming stale output from a previous run if this process fails to write a dump this time.
    DeleteFileW(dump_bmp_path.c_str());
  }

  AdapterRequirements req;
  int rc = ParseAdapterRequirements(argc, argv, kTestName, &req, &reporter);
  if (rc != 0) {
    return rc;
  }

  std::string map_name_a;
  std::string ready_name_a;
  std::string done_name_a;
  if (!aerogpu_test::GetArgValue(argc, argv, "--ipc-map", &map_name_a) || map_name_a.empty()) {
    return reporter.Fail("missing --ipc-map");
  }
  if (!aerogpu_test::GetArgValue(argc, argv, "--ready-event", &ready_name_a) || ready_name_a.empty()) {
    return reporter.Fail("missing --ready-event");
  }
  if (!aerogpu_test::GetArgValue(argc, argv, "--done-event", &done_name_a) || done_name_a.empty()) {
    return reporter.Fail("missing --done-event");
  }

  const std::wstring map_name(map_name_a.begin(), map_name_a.end());
  const std::wstring ready_name(ready_name_a.begin(), ready_name_a.end());
  const std::wstring done_name(done_name_a.begin(), done_name_a.end());

  HANDLE mapping = OpenFileMappingW(FILE_MAP_ALL_ACCESS, FALSE, map_name.c_str());
  if (!mapping) {
    return reporter.Fail("OpenFileMapping failed: %s",
                         aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }

  Wow64Ipc* ipc = (Wow64Ipc*)MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, sizeof(Wow64Ipc));
  if (!ipc) {
    DWORD err = GetLastError();
    CloseHandle(mapping);
    return reporter.Fail("MapViewOfFile failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
  }

  HANDLE ready_event = OpenEventW(SYNCHRONIZE, FALSE, ready_name.c_str());
  HANDLE done_event = OpenEventW(EVENT_MODIFY_STATE, FALSE, done_name.c_str());
  if (!ready_event || !done_event) {
    DWORD err = GetLastError();
    if (ready_event) {
      CloseHandle(ready_event);
    }
    if (done_event) {
      CloseHandle(done_event);
    }
    UnmapViewOfFile(ipc);
    CloseHandle(mapping);
    return reporter.Fail("OpenEvent failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
  }

  int exit_code = 1;
  if (ipc->magic != kIpcMagic || ipc->version != kIpcVersion) {
    exit_code = reporter.Fail("IPC header mismatch (magic=0x%08lX version=%lu)",
                              (unsigned long)ipc->magic,
                              (unsigned long)ipc->version);
    goto Exit;
  }

  DWORD wait = WaitForSingleObject(ready_event, 20000);
  if (wait != WAIT_OBJECT_0) {
    exit_code = reporter.Fail("timeout waiting for ready event (wait=%lu)", (unsigned long)wait);
    goto Exit;
  }

  const uint64_t producer_hv = ipc->producer_handle_value;
  const uint64_t shared_hv = ipc->shared_handle_value;
  aerogpu_test::PrintfStdout("INFO: %s: producer handle=%s", kTestName, FormatU64Hex(producer_hv).c_str());
  aerogpu_test::PrintfStdout("INFO: %s: consumer handle=%s (%s%s)",
                             kTestName,
                             FormatU64Hex(shared_hv).c_str(),
                             aerogpu_test::GetProcessBitnessString(),
                             aerogpu_test::GetWow64SuffixString());

  if (shared_hv == 0) {
    exit_code = reporter.Fail("shared handle is zero");
    goto Exit;
  }
  if (shared_hv == producer_hv) {
    exit_code = reporter.Fail("shared handle is numerically identical across processes (producer=%s consumer=%s)",
                              FormatU64Hex(producer_hv).c_str(),
                              FormatU64Hex(shared_hv).c_str());
    goto Exit;
  }

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurfaceWOW64_Consumer",
                                              L"AeroGPU D3D9Ex Shared Surface WOW64 (Consumer x64)",
                                              kWidth,
                                              kHeight,
                                              show);
  if (!hwnd) {
    exit_code = reporter.Fail("CreateBasicWindow failed");
    goto Exit;
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  rc = CreateD3D9ExDevice(kTestName, hwnd, &d3d, &dev, &reporter);
  if (rc != 0) {
    exit_code = rc;
    goto Exit;
  }
  rc = ValidateAdapter(kTestName, d3d.get(), req, &reporter);
  if (rc != 0) {
    exit_code = rc;
    goto Exit;
  }
  if (req.require_umd || (!req.allow_microsoft && !req.allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      exit_code = umd_rc;
      goto Exit;
    }
  }

  HANDLE open_handle = (HANDLE)(uintptr_t)shared_hv;
  ComPtr<IDirect3DTexture9> tex;
  HRESULT hr = dev->CreateTexture(kWidth,
                                  kHeight,
                                  1,
                                  D3DUSAGE_RENDERTARGET,
                                  D3DFMT_A8R8G8B8,
                                   D3DPOOL_DEFAULT,
                                   tex.put(),
                                   &open_handle);
  if (FAILED(hr)) {
    exit_code = reporter.FailHresult("CreateTexture(open shared)", hr);
    goto Exit;
  }

  if ((uint64_t)(uintptr_t)open_handle != shared_hv) {
    aerogpu_test::PrintfStdout("INFO: %s: CreateTexture updated shared handle: %s -> %s",
                               kTestName,
                               FormatU64Hex(shared_hv).c_str(),
                               FormatHandleHex(open_handle).c_str());
  }

  ComPtr<IDirect3DSurface9> surf;
  hr = tex->GetSurfaceLevel(0, surf.put());
  if (FAILED(hr)) {
    exit_code = reporter.FailHresult("IDirect3DTexture9::GetSurfaceLevel", hr);
    goto Exit;
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(
      kWidth, kHeight, D3DFMT_A8R8G8B8, D3DPOOL_SYSTEMMEM, sysmem.put(), NULL);
  if (FAILED(hr)) {
    exit_code = reporter.FailHresult("CreateOffscreenPlainSurface", hr);
    goto Exit;
  }

  hr = dev->GetRenderTargetData(surf.get(), sysmem.get());
  if (FAILED(hr)) {
    exit_code = reporter.FailHresult("GetRenderTargetData(shared)", hr);
    goto Exit;
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    exit_code = reporter.FailHresult("IDirect3DSurface9::LockRect", hr);
    goto Exit;
  }

  const uint32_t pixel = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 2, 2);

  if (dump) {
    std::string err;
    if (!aerogpu_test::WriteBmp32BGRA(dump_bmp_path,
                                     kWidth,
                                     kHeight,
                                     lr.pBits,
                                     (int)lr.Pitch,
                                     &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", kTestName, err.c_str());
    } else {
      reporter.AddArtifactPathW(dump_bmp_path);
    }
  }

  sysmem->UnlockRect();

  if ((pixel & 0x00FFFFFFu) != (kExpectedPixel & 0x00FFFFFFu)) {
    exit_code = reporter.Fail("pixel mismatch: got=0x%08lX expected=0x%08lX",
                              (unsigned long)pixel,
                              (unsigned long)kExpectedPixel);
    goto Exit;
  }

  exit_code = reporter.Pass();

Exit:
  InterlockedExchange(&ipc->consumer_exit_code, exit_code);
  InterlockedExchange(&ipc->done, 1);
  SetEvent(done_event);

  CloseHandle(ready_event);
  CloseHandle(done_event);
  UnmapViewOfFile(ipc);
  CloseHandle(mapping);
  return exit_code;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunConsumer(argc, argv);
}
