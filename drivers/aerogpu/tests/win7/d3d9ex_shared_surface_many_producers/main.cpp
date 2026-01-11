#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

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

struct IpcPayload {
  // Numeric handle value usable in the parent process (either duplicated into the parent, or a
  // global/shared token style handle that can be passed by value).
  uint64_t shared_handle;
  uint32_t ok;            // 1 on success.
  uint32_t is_nt_handle;  // 1 if the parent should CloseHandle(shared_handle).
  uint32_t win32_error;   // Optional: GetLastError() from failure site.
  uint32_t hr;            // Optional: HRESULT from failure site.
  uint32_t reserved;
};

static bool IsLikelyNtHandle(HANDLE h) {
  if (!h) {
    return false;
  }
  HANDLE dup = NULL;
  if (!DuplicateHandle(GetCurrentProcess(), h, GetCurrentProcess(), &dup, 0, FALSE, DUPLICATE_SAME_ACCESS) ||
      !dup) {
    return false;
  }
  CloseHandle(dup);
  return true;
}

static DWORD RemainingTimeoutMs(DWORD start_ticks, DWORD timeout_ms) {
  const DWORD now = GetTickCount();
  const DWORD elapsed = now - start_ticks;
  if (elapsed >= timeout_ms) {
    return 0;
  }
  return timeout_ms - elapsed;
}

static std::wstring WidenAscii(const std::string& s) {
  return std::wstring(s.begin(), s.end());
}

static std::wstring FormatPciIdHexW(uint32_t v) {
  wchar_t buf[16];
  _snwprintf(buf, ARRAYSIZE(buf), L"0x%04X", (unsigned)v);
  buf[ARRAYSIZE(buf) - 1] = 0;
  return std::wstring(buf);
}

static int CheckD3D9Adapter(aerogpu_test::TestReporter* reporter,
                            const char* test_name,
                            IDirect3D9Ex* d3d,
                            const AdapterRequirements& req) {
  if (!d3d) {
    if (reporter) {
      return reporter->Fail("internal: d3d == NULL");
    }
    return aerogpu_test::Fail(test_name, "internal: d3d == NULL");
  }

  D3DADAPTER_IDENTIFIER9 ident;
  ZeroMemory(&ident, sizeof(ident));
  HRESULT hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
  if (SUCCEEDED(hr)) {
    if (reporter) {
      reporter->SetAdapterInfoA(ident.Description, (uint32_t)ident.VendorId, (uint32_t)ident.DeviceId);
    }
    aerogpu_test::PrintfStdout("INFO: %s: adapter: %s (VID=0x%04X DID=0x%04X)",
                               test_name,
                               ident.Description,
                               (unsigned)ident.VendorId,
                               (unsigned)ident.DeviceId);
    if (!req.allow_microsoft && ident.VendorId == 0x1414) {
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
    if (req.has_require_vid && ident.VendorId != req.require_vid) {
      if (reporter) {
        return reporter->Fail("adapter VID mismatch: got 0x%04X expected 0x%04X",
                              (unsigned)ident.VendorId,
                              (unsigned)req.require_vid);
      }
      return aerogpu_test::Fail(test_name,
                                "adapter VID mismatch: got 0x%04X expected 0x%04X",
                                (unsigned)ident.VendorId,
                                (unsigned)req.require_vid);
    }
    if (req.has_require_did && ident.DeviceId != req.require_did) {
      if (reporter) {
        return reporter->Fail("adapter DID mismatch: got 0x%04X expected 0x%04X",
                              (unsigned)ident.DeviceId,
                              (unsigned)req.require_did);
      }
      return aerogpu_test::Fail(test_name,
                                "adapter DID mismatch: got 0x%04X expected 0x%04X",
                                (unsigned)ident.DeviceId,
                                (unsigned)req.require_did);
    }
    if (!req.allow_non_aerogpu && !req.has_require_vid && !req.has_require_did &&
        !(ident.VendorId == 0x1414 && req.allow_microsoft) &&
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
  } else if (req.has_require_vid || req.has_require_did) {
    if (reporter) {
      return reporter->FailHresult("GetAdapterIdentifier (required for --require-vid/--require-did)", hr);
    }
    return aerogpu_test::FailHresult(test_name,
                                     "GetAdapterIdentifier (required for --require-vid/--require-did)",
                                     hr);
  }

  return 0;
}

static int CreateD3D9ExDevice(aerogpu_test::TestReporter* reporter,
                              const char* test_name,
                              HWND hwnd,
                              int width,
                              int height,
                              const AdapterRequirements& req,
                              ComPtr<IDirect3D9Ex>* out_d3d,
                              ComPtr<IDirect3DDevice9Ex>* out_dev) {
  if (!out_d3d || !out_dev) {
    if (reporter) {
      return reporter->Fail("internal: CreateD3D9ExDevice out params are NULL");
    }
    return aerogpu_test::Fail(test_name, "internal: CreateD3D9ExDevice out params are NULL");
  }

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
    if (reporter) {
      return reporter->FailHresult("IDirect3D9Ex::CreateDeviceEx", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3D9Ex::CreateDeviceEx", hr);
  }

  int rc = CheckD3D9Adapter(reporter, test_name, d3d.get(), req);
  if (rc != 0) {
    return rc;
  }

  if (req.require_umd || (!req.allow_microsoft && !req.allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(reporter, test_name);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  out_d3d->reset(d3d.detach());
  out_dev->reset(dev.detach());
  return 0;
}

static D3DCOLOR UniqueColorForIndex(uint32_t idx) {
  // Deterministic pseudo-random colors that are obviously distinct for small idx.
  const uint32_t x = idx + 1;
  const uint8_t r = (uint8_t)(0x30u + (x * 37u) % 0xC0u);
  const uint8_t g = (uint8_t)(0x20u + (x * 67u) % 0xD0u);
  const uint8_t b = (uint8_t)(0x10u + (x * 97u) % 0xE0u);
  return D3DCOLOR_ARGB(0xFF, r, g, b);
}

static int WaitForGpuEvent(aerogpu_test::TestReporter* reporter,
                           const char* test_name,
                           IDirect3DDevice9Ex* dev,
                           DWORD timeout_ms) {
  if (!dev) {
    if (reporter) {
      return reporter->Fail("internal: WaitForGpuEvent dev == NULL");
    }
    return aerogpu_test::Fail(test_name, "internal: WaitForGpuEvent dev == NULL");
  }

  ComPtr<IDirect3DQuery9> q;
  HRESULT hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, q.put());
  if (FAILED(hr) || !q) {
    if (reporter) {
      return reporter->FailHresult("CreateQuery(D3DQUERYTYPE_EVENT)", hr);
    }
    return aerogpu_test::FailHresult(test_name, "CreateQuery(D3DQUERYTYPE_EVENT)", hr);
  }
  hr = q->Issue(D3DISSUE_END);
  if (FAILED(hr)) {
    if (reporter) {
      return reporter->FailHresult("IDirect3DQuery9::Issue(D3DISSUE_END)", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3DQuery9::Issue(D3DISSUE_END)", hr);
  }

  const DWORD start = GetTickCount();
  for (;;) {
    hr = q->GetData(NULL, 0, D3DGETDATA_FLUSH);
    if (hr == S_OK) {
      return 0;
    }
    if (hr != S_FALSE) {
      if (reporter) {
        return reporter->FailHresult("IDirect3DQuery9::GetData", hr);
      }
      return aerogpu_test::FailHresult(test_name, "IDirect3DQuery9::GetData", hr);
    }
    if (GetTickCount() - start > timeout_ms) {
      if (reporter) {
        return reporter->Fail("GPU event query timed out");
      }
      return aerogpu_test::Fail(test_name, "GPU event query timed out");
    }
    Sleep(0);
  }
}

static bool ParseAdapterRequirements(aerogpu_test::TestReporter* reporter,
                                     const char* test_name,
                                     int argc,
                                     char** argv,
                                     AdapterRequirements* out_req) {
  if (!out_req) {
    if (reporter) {
      reporter->Fail("internal: ParseAdapterRequirements out_req == NULL");
    } else {
      aerogpu_test::Fail(test_name, "internal: ParseAdapterRequirements out_req == NULL");
    }
    return false;
  }
  ZeroMemory(out_req, sizeof(*out_req));

  out_req->allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  out_req->allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  out_req->require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");

  std::string require_vid_str;
  std::string require_did_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--require-vid", &require_vid_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_vid_str, &out_req->require_vid, &err)) {
      if (reporter) {
        reporter->Fail("invalid --require-vid: %s", err.c_str());
      } else {
        aerogpu_test::Fail(test_name, "invalid --require-vid: %s", err.c_str());
      }
      return false;
    }
    out_req->has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &out_req->require_did, &err)) {
      if (reporter) {
        reporter->Fail("invalid --require-did: %s", err.c_str());
      } else {
        aerogpu_test::Fail(test_name, "invalid --require-did: %s", err.c_str());
      }
      return false;
    }
    out_req->has_require_did = true;
  }
  return true;
}

static std::wstring MakeIpcBaseName(DWORD parent_pid, DWORD tick, uint32_t index) {
  wchar_t buf[128];
  _snwprintf(buf,
             ARRAYSIZE(buf),
             L"Local\\AeroGPU_%lu_%lu_manyprod_%u",
             (unsigned long)parent_pid,
             (unsigned long)tick,
             (unsigned)index);
  buf[ARRAYSIZE(buf) - 1] = 0;
  return std::wstring(buf);
}

static int RunProducer(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface_many_producers_producer";

  AdapterRequirements req;
  if (!ParseAdapterRequirements(NULL, kTestName, argc, argv, &req)) {
    return 1;
  }

  std::string parent_pid_str;
  if (!aerogpu_test::GetArgValue(argc, argv, "--parent-pid", &parent_pid_str)) {
    return aerogpu_test::Fail(kTestName, "missing --parent-pid");
  }
  uint32_t parent_pid = 0;
  std::string parse_err;
  if (!aerogpu_test::ParseUint32(parent_pid_str, &parent_pid, &parse_err) || parent_pid == 0) {
    return aerogpu_test::Fail(kTestName, "invalid --parent-pid: %s", parse_err.c_str());
  }

  std::string ipc_name_str;
  if (!aerogpu_test::GetArgValue(argc, argv, "--ipc-name", &ipc_name_str) || ipc_name_str.empty()) {
    return aerogpu_test::Fail(kTestName, "missing --ipc-name");
  }
  const std::wstring ipc_base = WidenAscii(ipc_name_str);

  std::string index_str;
  uint32_t index = 0;
  if (aerogpu_test::GetArgValue(argc, argv, "--index", &index_str) && !index_str.empty()) {
    if (!aerogpu_test::ParseUint32(index_str, &index, &parse_err)) {
      return aerogpu_test::Fail(kTestName, "invalid --index: %s", parse_err.c_str());
    }
  }

  const std::wstring mapping_name = ipc_base + L"_map";
  const std::wstring ready_name = ipc_base + L"_ready";

  const DWORD kPayloadSize = (DWORD)sizeof(IpcPayload);

  HANDLE mapping = OpenFileMappingW(FILE_MAP_WRITE | FILE_MAP_READ, FALSE, mapping_name.c_str());
  if (!mapping) {
    return aerogpu_test::Fail(
        kTestName, "OpenFileMappingW failed: %s", aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  }
  IpcPayload* payload =
      (IpcPayload*)MapViewOfFile(mapping, FILE_MAP_WRITE | FILE_MAP_READ, 0, 0, kPayloadSize);
  if (!payload) {
    DWORD err = GetLastError();
    CloseHandle(mapping);
    return aerogpu_test::Fail(
        kTestName, "MapViewOfFile failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
  }

  HANDLE ready_event = OpenEventW(EVENT_MODIFY_STATE, FALSE, ready_name.c_str());
  if (!ready_event) {
    DWORD err = GetLastError();
    UnmapViewOfFile(payload);
    CloseHandle(mapping);
    return aerogpu_test::Fail(
        kTestName, "OpenEventW(ready) failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
  }

  ZeroMemory(payload, sizeof(*payload));

  const int kWidth = 64;
  const int kHeight = 64;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurfaceManyProducers_Producer",
                                              L"AeroGPU D3D9Ex Shared Surface Many Producers (Producer)",
                                              kWidth,
                                              kHeight,
                                              false);
  if (!hwnd) {
    payload->win32_error = GetLastError();
    payload->ok = 0;
    SetEvent(ready_event);
    CloseHandle(ready_event);
    UnmapViewOfFile(payload);
    CloseHandle(mapping);
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(NULL, kTestName, hwnd, kWidth, kHeight, req, &d3d, &dev);
  if (rc != 0) {
    payload->ok = 0;
    payload->hr = 0x80004005u;
    SetEvent(ready_event);
    CloseHandle(ready_event);
    UnmapViewOfFile(payload);
    CloseHandle(mapping);
    return rc;
  }

  HANDLE shared_handle = NULL;
  ComPtr<IDirect3DTexture9> tex;
  HRESULT hr = dev->CreateTexture(kWidth,
                                  kHeight,
                                  1,
                                  D3DUSAGE_RENDERTARGET,
                                  D3DFMT_A8R8G8B8,
                                  D3DPOOL_DEFAULT,
                                  tex.put(),
                                  &shared_handle);
  if (FAILED(hr) || !shared_handle) {
    payload->ok = 0;
    payload->hr = (uint32_t)hr;
    SetEvent(ready_event);
    CloseHandle(ready_event);
    UnmapViewOfFile(payload);
    CloseHandle(mapping);
    return aerogpu_test::FailHresult(kTestName, "CreateTexture(shared)", hr);
  }

  // Ensure the producer-side allocation is realized before handing the surface to the compositor.
  ComPtr<IDirect3DSurface9> surf;
  hr = tex->GetSurfaceLevel(0, surf.put());
  if (FAILED(hr)) {
    payload->ok = 0;
    payload->hr = (uint32_t)hr;
    SetEvent(ready_event);
    CloseHandle(ready_event);
    UnmapViewOfFile(payload);
    CloseHandle(mapping);
    return aerogpu_test::FailHresult(kTestName, "IDirect3DTexture9::GetSurfaceLevel", hr);
  }
  const D3DCOLOR init_color = UniqueColorForIndex(index);
  hr = dev->ColorFill(surf.get(), NULL, init_color);
  if (FAILED(hr)) {
    payload->ok = 0;
    payload->hr = (uint32_t)hr;
    SetEvent(ready_event);
    CloseHandle(ready_event);
    UnmapViewOfFile(payload);
    CloseHandle(mapping);
    return aerogpu_test::FailHresult(kTestName, "ColorFill(producer init)", hr);
  }
  hr = dev->Flush();
  if (FAILED(hr)) {
    payload->ok = 0;
    payload->hr = (uint32_t)hr;
    SetEvent(ready_event);
    CloseHandle(ready_event);
    UnmapViewOfFile(payload);
    CloseHandle(mapping);
    return aerogpu_test::FailHresult(kTestName, "Flush(producer init)", hr);
  }
  rc = WaitForGpuEvent(NULL, kTestName, dev.get(), 5000);
  if (rc != 0) {
    payload->ok = 0;
    SetEvent(ready_event);
    CloseHandle(ready_event);
    UnmapViewOfFile(payload);
    CloseHandle(mapping);
    return rc;
  }

  HANDLE shared_in_parent = shared_handle;
  const bool is_nt_handle = IsLikelyNtHandle(shared_handle);
  DWORD dup_err = 0;
  if (is_nt_handle) {
    HANDLE parent_proc = OpenProcess(PROCESS_DUP_HANDLE, FALSE, (DWORD)parent_pid);
    if (!parent_proc) {
      payload->ok = 0;
      payload->win32_error = GetLastError();
      SetEvent(ready_event);
      CloseHandle(ready_event);
      UnmapViewOfFile(payload);
      CloseHandle(mapping);
      return aerogpu_test::Fail(
          kTestName, "OpenProcess(PROCESS_DUP_HANDLE) failed: %s", aerogpu_test::Win32ErrorToString(payload->win32_error).c_str());
    }
    HANDLE dup = NULL;
    if (!DuplicateHandle(GetCurrentProcess(),
                         shared_handle,
                         parent_proc,
                         &dup,
                         0,
                         FALSE,
                         DUPLICATE_SAME_ACCESS) ||
        !dup) {
      dup_err = GetLastError();
      CloseHandle(parent_proc);
      payload->ok = 0;
      payload->win32_error = dup_err;
      SetEvent(ready_event);
      CloseHandle(ready_event);
      UnmapViewOfFile(payload);
      CloseHandle(mapping);
      return aerogpu_test::Fail(
          kTestName, "DuplicateHandle(into parent) failed: %s", aerogpu_test::Win32ErrorToString(dup_err).c_str());
    }
    CloseHandle(parent_proc);
    shared_in_parent = dup;
  }

  payload->shared_handle = (uint64_t)(uintptr_t)shared_in_parent;
  payload->is_nt_handle = is_nt_handle ? 1u : 0u;
  payload->ok = 1u;
  payload->win32_error = 0;
  payload->hr = 0;

  FlushViewOfFile(payload, kPayloadSize);
  SetEvent(ready_event);

  CloseHandle(ready_event);
  UnmapViewOfFile(payload);
  CloseHandle(mapping);

  // The compositor holds the shared handle; this process can exit immediately.
  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

static int ValidateSurfaceColor(aerogpu_test::TestReporter* reporter,
                                const char* test_name,
                                IDirect3DDevice9Ex* dev,
                                IDirect3DSurface9* surface,
                                uint32_t index,
                                D3DCOLOR expected_color) {
  if (!dev || !surface) {
    if (reporter) {
      return reporter->Fail("internal: ValidateSurfaceColor called with NULL");
    }
    return aerogpu_test::Fail(test_name, "internal: ValidateSurfaceColor called with NULL");
  }

  D3DSURFACE_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  HRESULT hr = surface->GetDesc(&desc);
  if (FAILED(hr)) {
    if (reporter) {
      return reporter->FailHresult("IDirect3DSurface9::GetDesc", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3DSurface9::GetDesc", hr);
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(desc.Width,
                                        desc.Height,
                                        desc.Format,
                                        D3DPOOL_SYSTEMMEM,
                                        sysmem.put(),
                                        NULL);
  if (FAILED(hr)) {
    if (reporter) {
      return reporter->FailHresult("CreateOffscreenPlainSurface", hr);
    }
    return aerogpu_test::FailHresult(test_name, "CreateOffscreenPlainSurface", hr);
  }

  hr = dev->GetRenderTargetData(surface, sysmem.get());
  if (FAILED(hr)) {
    if (reporter) {
      return reporter->FailHresult("GetRenderTargetData", hr);
    }
    return aerogpu_test::FailHresult(test_name, "GetRenderTargetData", hr);
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    if (reporter) {
      return reporter->FailHresult("IDirect3DSurface9::LockRect", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3DSurface9::LockRect", hr);
  }
  const uint32_t pixel = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 2, 2);
  sysmem->UnlockRect();

  if ((pixel & 0x00FFFFFFu) != (expected_color & 0x00FFFFFFu)) {
    if (reporter) {
      return reporter->Fail("surface[%u] pixel mismatch: got=0x%08lX expected=0x%08lX",
                            (unsigned)index,
                            (unsigned long)pixel,
                            (unsigned long)expected_color);
    }
    return aerogpu_test::Fail(test_name,
                              "surface[%u] pixel mismatch: got=0x%08lX expected=0x%08lX",
                              (unsigned)index,
                              (unsigned long)pixel,
                              (unsigned long)expected_color);
  }
  return 0;
}

static int RunCompositor(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface_many_producers";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--producers=N] [--hidden] [--show] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    aerogpu_test::PrintfStdout(
        "Internal: %s.exe --producer --parent-pid=PID --ipc-name=NAME [--index=N] (used by compositor)",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  AdapterRequirements req;
  if (!ParseAdapterRequirements(&reporter, kTestName, argc, argv, &req)) {
    return 1;
  }

  uint32_t producer_count = 8;
  std::string producers_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--producers", &producers_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(producers_str, &producer_count, &err) || producer_count == 0 ||
        producer_count > 32) {
      return reporter.Fail("invalid --producers: %s", err.c_str());
    }
  }

  // Default is hidden; --show is opt-in (useful when running manually).
  const bool show = aerogpu_test::HasArg(argc, argv, "--show");

  const int kWidth = 64;
  const int kHeight = 64;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurfaceManyProducers_Compositor",
                                              L"AeroGPU D3D9Ex Shared Surface Many Producers",
                                              kWidth,
                                              kHeight,
                                              show);
  if (!hwnd) {
    return reporter.Fail("CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(&reporter, kTestName, hwnd, kWidth, kHeight, req, &d3d, &dev);
  if (rc != 0) {
    return rc;
  }

  wchar_t exe_buf[MAX_PATH];
  DWORD exe_len = GetModuleFileNameW(NULL, exe_buf, ARRAYSIZE(exe_buf));
  if (!exe_len || exe_len >= ARRAYSIZE(exe_buf)) {
    return reporter.Fail("GetModuleFileNameW failed");
  }
  const std::wstring exe_path(exe_buf, exe_buf + exe_len);

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
    }
  }

  struct ProducerInstance {
    uint32_t index;
    std::wstring ipc_base;
    HANDLE mapping;
    IpcPayload* payload;
    HANDLE ready_event;
    PROCESS_INFORMATION pi;
  };

  std::vector<ProducerInstance> producers;
  producers.reserve((size_t)producer_count);

  const DWORD parent_pid = GetCurrentProcessId();
  const DWORD tick = GetTickCount();
  const DWORD kProducerTimeoutMs = 25000;
  const DWORD start_ticks = GetTickCount();

  for (uint32_t i = 0; i < producer_count; ++i) {
    ProducerInstance p;
    p.index = i;
    p.ipc_base = MakeIpcBaseName(parent_pid, tick, i);
    p.mapping = NULL;
    p.payload = NULL;
    p.ready_event = NULL;
    ZeroMemory(&p.pi, sizeof(p.pi));

    const std::wstring mapping_name = p.ipc_base + L"_map";
    const std::wstring ready_name = p.ipc_base + L"_ready";

    const DWORD kPayloadSize = (DWORD)sizeof(IpcPayload);
    p.mapping =
        CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_READWRITE, 0, kPayloadSize, mapping_name.c_str());
    if (!p.mapping) {
      if (job) {
        CloseHandle(job);
      }
      return reporter.Fail("CreateFileMappingW failed: %s",
                           aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    }
    p.payload = (IpcPayload*)MapViewOfFile(p.mapping, FILE_MAP_WRITE | FILE_MAP_READ, 0, 0, kPayloadSize);
    if (!p.payload) {
      DWORD err = GetLastError();
      CloseHandle(p.mapping);
      if (job) {
        CloseHandle(job);
      }
      return reporter.Fail("MapViewOfFile failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
    }
    ZeroMemory(p.payload, sizeof(*p.payload));

    p.ready_event = CreateEventW(NULL, TRUE, FALSE, ready_name.c_str());
    if (!p.ready_event) {
      DWORD err = GetLastError();
      UnmapViewOfFile(p.payload);
      CloseHandle(p.mapping);
      if (job) {
        CloseHandle(job);
      }
      return reporter.Fail("CreateEventW failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
    }

    wchar_t pid_buf[32];
    _snwprintf(pid_buf, ARRAYSIZE(pid_buf), L"%lu", (unsigned long)parent_pid);
    pid_buf[ARRAYSIZE(pid_buf) - 1] = 0;

    wchar_t idx_buf[32];
    _snwprintf(idx_buf, ARRAYSIZE(idx_buf), L"%u", (unsigned)i);
    idx_buf[ARRAYSIZE(idx_buf) - 1] = 0;

    std::wstring cmdline = L"\"";
    cmdline += exe_path;
    cmdline += L"\" --producer --parent-pid=";
    cmdline += pid_buf;
    cmdline += L" --ipc-name=";
    cmdline += p.ipc_base;
    cmdline += L" --index=";
    cmdline += idx_buf;
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
      cmdline += L" --require-vid=";
      cmdline += FormatPciIdHexW(req.require_vid);
    }
    if (req.has_require_did) {
      cmdline += L" --require-did=";
      cmdline += FormatPciIdHexW(req.require_did);
    }

    std::vector<wchar_t> cmdline_buf(cmdline.begin(), cmdline.end());
    cmdline_buf.push_back(0);

    STARTUPINFOW si;
    ZeroMemory(&si, sizeof(si));
    si.cb = sizeof(si);

    BOOL ok = CreateProcessW(exe_path.c_str(),
                             &cmdline_buf[0],
                             NULL,
                             NULL,
                             FALSE,
                             CREATE_SUSPENDED,
                             NULL,
                             NULL,
                             &si,
                             &p.pi);
    if (!ok) {
      DWORD err = GetLastError();
      CloseHandle(p.ready_event);
      UnmapViewOfFile(p.payload);
      CloseHandle(p.mapping);
      if (job) {
        CloseHandle(job);
      }
      return reporter.Fail("CreateProcessW(producer %u) failed: %s",
                           (unsigned)i,
                           aerogpu_test::Win32ErrorToString(err).c_str());
    }

    if (job) {
      if (!AssignProcessToJobObject(job, p.pi.hProcess)) {
        aerogpu_test::PrintfStdout("INFO: %s: AssignProcessToJobObject failed: %s",
                                   kTestName,
                                   aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
      }
    }

    ResumeThread(p.pi.hThread);
    CloseHandle(p.pi.hThread);
    p.pi.hThread = NULL;

    producers.push_back(p);
  }

  for (uint32_t i = 0; i < producer_count; ++i) {
    ProducerInstance& p = producers[i];
    const DWORD remaining = RemainingTimeoutMs(start_ticks, kProducerTimeoutMs);
    if (remaining == 0) {
      if (job) {
        CloseHandle(job);
      }
      return reporter.Fail("timeout waiting for producers");
    }

    HANDLE wait_handles[2] = {p.ready_event, p.pi.hProcess};
    DWORD wait = WaitForMultipleObjects(2, wait_handles, FALSE, remaining);
    if (wait != WAIT_OBJECT_0) {
      DWORD exit_code = 1;
      GetExitCodeProcess(p.pi.hProcess, &exit_code);
      if (wait == WAIT_OBJECT_0 + 1) {
        if (job) {
          CloseHandle(job);
        }
        return reporter.Fail("producer %u exited early (exit_code=%lu)",
                             (unsigned)i,
                             (unsigned long)exit_code);
      }
      if (wait == WAIT_TIMEOUT) {
        TerminateProcess(p.pi.hProcess, 124);
        WaitForSingleObject(p.pi.hProcess, 2000);
        if (job) {
          CloseHandle(job);
        }
        return reporter.Fail("producer %u timed out", (unsigned)i);
      }
      DWORD err = GetLastError();
      TerminateProcess(p.pi.hProcess, 124);
      WaitForSingleObject(p.pi.hProcess, 2000);
      if (job) {
        CloseHandle(job);
      }
      return reporter.Fail("WaitForMultipleObjects(producer %u) failed: %s",
                           (unsigned)i,
                           aerogpu_test::Win32ErrorToString(err).c_str());
    }

    if (!p.payload->ok) {
      DWORD werr = (DWORD)p.payload->win32_error;
      HRESULT phr = (HRESULT)p.payload->hr;
      if (job) {
        CloseHandle(job);
      }
      return reporter.Fail("producer %u reported failure (win32=%s hr=%s)",
                           (unsigned)i,
                           werr ? aerogpu_test::Win32ErrorToString(werr).c_str() : "0",
                           phr ? aerogpu_test::HresultToString(phr).c_str() : "0");
    }

    if (!p.payload->shared_handle) {
      if (job) {
        CloseHandle(job);
      }
      return reporter.Fail("producer %u returned NULL shared handle", (unsigned)i);
    }
  }

  // Open all shared surfaces in the compositor process.
  std::vector<IDirect3DTexture9*> opened_textures;
  std::vector<IDirect3DSurface9*> opened_surfaces;
  std::vector<HANDLE> shared_handles_to_close;
  opened_textures.assign((size_t)producer_count, (IDirect3DTexture9*)NULL);
  opened_surfaces.assign((size_t)producer_count, (IDirect3DSurface9*)NULL);

  for (uint32_t i = 0; i < producer_count; ++i) {
    ProducerInstance& p = producers[i];
    HANDLE h = (HANDLE)(uintptr_t)p.payload->shared_handle;
    HANDLE open_handle = h;

    IDirect3DTexture9* tex = NULL;
    HRESULT hr = dev->CreateTexture(kWidth,
                                    kHeight,
                                    1,
                                    D3DUSAGE_RENDERTARGET,
                                    D3DFMT_A8R8G8B8,
                                    D3DPOOL_DEFAULT,
                                    &tex,
                                    &open_handle);
    if (FAILED(hr) || !tex) {
      if (job) {
        CloseHandle(job);
      }
      return reporter.FailHresult("CreateTexture(open shared)", hr);
    }

    IDirect3DSurface9* surf = NULL;
    hr = tex->GetSurfaceLevel(0, &surf);
    if (FAILED(hr) || !surf) {
      tex->Release();
      if (job) {
        CloseHandle(job);
      }
      return reporter.FailHresult("IDirect3DTexture9::GetSurfaceLevel(opened)", hr);
    }

    opened_textures[i] = tex;
    opened_surfaces[i] = surf;

    if (p.payload->is_nt_handle) {
      shared_handles_to_close.push_back(h);
    }
  }

  // Use all opened shared surfaces in a single command stream, then Flush once. This stresses
  // per-submit allocation table building (DWM-like: many producer allocations referenced together).
  for (uint32_t i = 0; i < producer_count; ++i) {
    const D3DCOLOR c = UniqueColorForIndex(i);
    HRESULT hr = dev->ColorFill(opened_surfaces[i], NULL, c);
    if (FAILED(hr)) {
      if (job) {
        CloseHandle(job);
      }
      return reporter.FailHresult("ColorFill(compositor)", hr);
    }
  }

  HRESULT flush_hr = dev->Flush();
  if (FAILED(flush_hr)) {
    if (job) {
      CloseHandle(job);
    }
    return reporter.FailHresult("Flush(compositor)", flush_hr);
  }

  rc = WaitForGpuEvent(&reporter, kTestName, dev.get(), 10000);
  if (rc != 0) {
    if (job) {
      CloseHandle(job);
    }
    return rc;
  }

  // Validate that each shared surface was independently opened and updated.
  for (uint32_t i = 0; i < producer_count; ++i) {
    const D3DCOLOR expected = UniqueColorForIndex(i);
    rc = ValidateSurfaceColor(&reporter, kTestName, dev.get(), opened_surfaces[i], i, expected);
    if (rc != 0) {
      if (job) {
        CloseHandle(job);
      }
      return rc;
    }
  }

  // Wait for producers to exit (they should exit immediately after signaling readiness).
  for (uint32_t i = 0; i < producer_count; ++i) {
    WaitForSingleObject(producers[i].pi.hProcess, 5000);
    CloseHandle(producers[i].pi.hProcess);
    producers[i].pi.hProcess = NULL;
    CloseHandle(producers[i].ready_event);
    producers[i].ready_event = NULL;
    UnmapViewOfFile(producers[i].payload);
    producers[i].payload = NULL;
    CloseHandle(producers[i].mapping);
    producers[i].mapping = NULL;
  }

  for (uint32_t i = 0; i < producer_count; ++i) {
    if (opened_surfaces[i]) {
      opened_surfaces[i]->Release();
      opened_surfaces[i] = NULL;
    }
    if (opened_textures[i]) {
      opened_textures[i]->Release();
      opened_textures[i] = NULL;
    }
  }

  for (size_t i = 0; i < shared_handles_to_close.size(); ++i) {
    CloseHandle(shared_handles_to_close[i]);
  }

  if (job) {
    CloseHandle(job);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();

  if (aerogpu_test::HasArg(argc, argv, "--producer")) {
    return RunProducer(argc, argv);
  }
  return RunCompositor(argc, argv);
}
