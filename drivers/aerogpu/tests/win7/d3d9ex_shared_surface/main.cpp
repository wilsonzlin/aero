#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_kmt.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;
using aerogpu_test::kmt::D3DKMT_FUNCS;
using aerogpu_test::kmt::D3DKMT_HANDLE;
using aerogpu_test::kmt::NTSTATUS;

static void DumpBytesToFile(const char* test_name,
                            aerogpu_test::TestReporter* reporter,
                            const wchar_t* file_name,
                            const void* data,
                            UINT byte_count) {
  if (!file_name || !data || byte_count == 0) {
    return;
  }
  const std::wstring dir = aerogpu_test::GetModuleDir();
  const std::wstring path = aerogpu_test::JoinPath(dir, file_name);
  HANDLE h =
      CreateFileW(path.c_str(), GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
  if (h == INVALID_HANDLE_VALUE) {
    aerogpu_test::PrintfStdout("INFO: %s: dump CreateFileW(%ls) failed: %s",
                               test_name,
                               file_name,
                               aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    return;
  }
  DWORD written = 0;
  if (!WriteFile(h, data, byte_count, &written, NULL) || written != byte_count) {
    aerogpu_test::PrintfStdout("INFO: %s: dump WriteFile(%ls) failed: %s",
                               test_name,
                               file_name,
                               aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: dumped %u bytes to %ls",
                               test_name,
                               (unsigned)byte_count,
                               path.c_str());
    if (reporter) {
      reporter->AddArtifactPathW(path);
    }
  }
  CloseHandle(h);
}

static void DumpTightBgra32(const char* test_name,
                            aerogpu_test::TestReporter* reporter,
                            const wchar_t* file_name,
                            const void* data,
                            int row_pitch,
                            int width,
                            int height) {
  if (!data || width <= 0 || height <= 0 || row_pitch < width * 4) {
    return;
  }
  std::vector<uint8_t> tight((size_t)width * (size_t)height * 4u, 0);
  for (int y = 0; y < height; ++y) {
    const uint8_t* src_row = (const uint8_t*)data + (size_t)y * (size_t)row_pitch;
    memcpy(&tight[(size_t)y * (size_t)width * 4u], src_row, (size_t)width * 4u);
  }
  DumpBytesToFile(test_name, reporter, file_name, &tight[0], (UINT)tight.size());
}

static bool MapSharedHandleToken(HWND hwnd, HANDLE shared_handle, uint32_t* out_token, std::string* err) {
  if (out_token) {
    *out_token = 0;
  }
  if (!hwnd || !shared_handle) {
    if (err) {
      *err = "invalid hwnd/shared_handle";
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
  if (!aerogpu_test::kmt::OpenAdapterFromHwnd(&kmt, hwnd, &adapter, &kmt_err)) {
    aerogpu_test::kmt::UnloadD3DKMT(&kmt);
    if (err) {
      *err = kmt_err;
    }
    return false;
  }

  uint32_t token = 0;
  NTSTATUS st = 0;
  const bool ok = aerogpu_test::kmt::AerogpuMapSharedHandleDebugToken(
      &kmt, adapter, (unsigned long long)(uintptr_t)shared_handle, &token, &st);

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

struct Vertex {
  float x;
  float y;
  float z;
  float rhw;
  DWORD color;
};

struct AdapterRequirements {
  bool allow_microsoft;
  bool allow_non_aerogpu;
  bool require_umd;
  bool has_require_vid;
  bool has_require_did;
  uint32_t require_vid;
  uint32_t require_did;
};

enum SharedResourceKind {
  kSharedTexture = 0,
  kSharedRenderTarget = 1,
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

static DWORD RemainingTimeoutMs(DWORD start_ticks, DWORD timeout_ms) {
  const DWORD now = GetTickCount();
  const DWORD elapsed = now - start_ticks;
  if (elapsed >= timeout_ms) {
    return 0;
  }
  return timeout_ms - elapsed;
}

static int CheckD3D9Adapter(aerogpu_test::TestReporter* reporter,
                            const char* test_name,
                            IDirect3D9Ex* d3d,
                            const AdapterRequirements& req) {
  D3DADAPTER_IDENTIFIER9 ident;
  ZeroMemory(&ident, sizeof(ident));
  HRESULT hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
  if (SUCCEEDED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: adapter: %s (VID=0x%04X DID=0x%04X)",
                               test_name,
                               ident.Description,
                               (unsigned)ident.VendorId,
                               (unsigned)ident.DeviceId);
    if (reporter) {
      reporter->SetAdapterInfoA(ident.Description, ident.VendorId, ident.DeviceId);
    }
    if (!req.allow_microsoft && ident.VendorId == 0x1414) {
      if (reporter) {
        return reporter->Fail(
            "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
            "Install AeroGPU driver or pass --allow-microsoft.",
            (unsigned)ident.VendorId,
            (unsigned)ident.DeviceId);
      }
      return aerogpu_test::Fail(
          test_name,
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
    return aerogpu_test::FailHresult(test_name, "GetAdapterIdentifier (required for --require-vid/--require-did)", hr);
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

  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);

  out_d3d->reset(d3d.detach());
  out_dev->reset(dev.detach());
  return 0;
}

static int RenderTriangleToSurface(aerogpu_test::TestReporter* reporter,
                                   const char* test_name,
                                   IDirect3DDevice9Ex* dev,
                                   IDirect3DSurface9* surface,
                                   int width,
                                   int height) {
  if (!dev || !surface) {
    if (reporter) {
      return reporter->Fail("internal: RenderTriangleToSurface called with NULL");
    }
    return aerogpu_test::Fail(test_name, "internal: RenderTriangleToSurface called with NULL");
  }

  ComPtr<IDirect3DSurface9> old_rt;
  HRESULT hr = dev->GetRenderTarget(0, old_rt.put());
  if (FAILED(hr)) {
    if (reporter) {
      return reporter->FailHresult("IDirect3DDevice9Ex::GetRenderTarget", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::GetRenderTarget", hr);
  }

  hr = dev->SetRenderTarget(0, surface);
  if (FAILED(hr)) {
    if (reporter) {
      return reporter->FailHresult("IDirect3DDevice9Ex::SetRenderTarget(shared)", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::SetRenderTarget(shared)", hr);
  }

  D3DVIEWPORT9 vp;
  vp.X = 0;
  vp.Y = 0;
  vp.Width = (DWORD)width;
  vp.Height = (DWORD)height;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  hr = dev->SetViewport(&vp);
  if (FAILED(hr)) {
    dev->SetRenderTarget(0, old_rt.get());
    if (reporter) {
      return reporter->FailHresult("IDirect3DDevice9Ex::SetViewport", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::SetViewport", hr);
  }

  const DWORD kRed = D3DCOLOR_XRGB(255, 0, 0);
  // Use a non-symmetric vertex color so we catch D3DCOLOR channel-ordering regressions
  // (e.g. BGRA-in-memory vs RGBA-in-shader).
  const DWORD kBlue = D3DCOLOR_XRGB(0, 0, 255);

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kRed, 1.0f, 0);
  if (FAILED(hr)) {
    dev->SetRenderTarget(0, old_rt.get());
    if (reporter) {
      return reporter->FailHresult("IDirect3DDevice9Ex::Clear", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::Clear", hr);
  }

  Vertex verts[3];
  // Triangle that covers the center pixel while leaving the top-left corner untouched.
  verts[0].x = (float)width * 0.25f;
  verts[0].y = (float)height * 0.25f;
  verts[0].z = 0.5f;
  verts[0].rhw = 1.0f;
  verts[0].color = kBlue;
  verts[1].x = (float)width * 0.75f;
  verts[1].y = (float)height * 0.25f;
  verts[1].z = 0.5f;
  verts[1].rhw = 1.0f;
  verts[1].color = kBlue;
  verts[2].x = (float)width * 0.5f;
  verts[2].y = (float)height * 0.75f;
  verts[2].z = 0.5f;
  verts[2].rhw = 1.0f;
  verts[2].color = kBlue;

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    dev->SetRenderTarget(0, old_rt.get());
    if (reporter) {
      return reporter->FailHresult("IDirect3DDevice9Ex::BeginScene", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::BeginScene", hr);
  }

  hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
  if (FAILED(hr)) {
    dev->EndScene();
    dev->SetRenderTarget(0, old_rt.get());
    if (reporter) {
      return reporter->FailHresult("IDirect3DDevice9Ex::SetFVF", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::SetFVF", hr);
  }

  hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLELIST, 1, verts, sizeof(Vertex));
  if (FAILED(hr)) {
    dev->EndScene();
    dev->SetRenderTarget(0, old_rt.get());
    if (reporter) {
      return reporter->FailHresult("IDirect3DDevice9Ex::DrawPrimitiveUP", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::DrawPrimitiveUP", hr);
  }

  hr = dev->EndScene();
  if (FAILED(hr)) {
    dev->SetRenderTarget(0, old_rt.get());
    if (reporter) {
      return reporter->FailHresult("IDirect3DDevice9Ex::EndScene", hr);
    }
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::EndScene", hr);
  }

  dev->SetRenderTarget(0, old_rt.get());
  return 0;
}

static int ValidateSurfacePixels(aerogpu_test::TestReporter* reporter,
                                 const char* test_name,
                                 const wchar_t* dump_name,
                                 bool dump,
                                 IDirect3DDevice9Ex* dev,
                                 IDirect3DSurface9* surface) {
  if (!dev || !surface) {
    if (reporter) {
      return reporter->Fail("internal: ValidateSurfacePixels called with NULL");
    }
    return aerogpu_test::Fail(test_name, "internal: ValidateSurfacePixels called with NULL");
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

  const int cx = (int)desc.Width / 2;
  const int cy = (int)desc.Height / 2;
  const uint32_t center = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, cx, cy);
  const uint32_t corner = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 5, 5);

  if (dump && dump_name) {
    std::string err;
    const std::wstring bmp_path = aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), dump_name);
    if (!aerogpu_test::WriteBmp32BGRA(bmp_path,
                                      (int)desc.Width,
                                      (int)desc.Height,
                                      lr.pBits,
                                      (int)lr.Pitch,
                                      &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", test_name, err.c_str());
    } else if (reporter) {
      reporter->AddArtifactPathW(bmp_path);
    }

    std::wstring bin_name(dump_name);
    size_t dot = bin_name.rfind(L'.');
    if (dot != std::wstring::npos) {
      bin_name = bin_name.substr(0, dot) + L".bin";
    } else {
      bin_name += L".bin";
    }
    DumpTightBgra32(test_name,
                    reporter,
                    bin_name.c_str(),
                    lr.pBits,
                    (int)lr.Pitch,
                    (int)desc.Width,
                    (int)desc.Height);
  }

  sysmem->UnlockRect();

  const uint32_t expected_center = 0xFF0000FFu;  // BGRA = (255, 0, 0, 255) = blue.
  const uint32_t expected_corner = 0xFFFF0000u;  // BGRA = (0, 0, 255, 255).

  if ((center & 0x00FFFFFFu) != (expected_center & 0x00FFFFFFu) ||
      (corner & 0x00FFFFFFu) != (expected_corner & 0x00FFFFFFu)) {
    if (reporter) {
      return reporter->Fail("pixel mismatch: center=0x%08lX expected 0x%08lX; corner(5,5)=0x%08lX expected 0x%08lX",
                            (unsigned long)center,
                            (unsigned long)expected_center,
                            (unsigned long)corner,
                            (unsigned long)expected_corner);
    }
    return aerogpu_test::Fail(test_name,
                              "pixel mismatch: center=0x%08lX expected 0x%08lX; corner(5,5)=0x%08lX expected 0x%08lX",
                              (unsigned long)center,
                              (unsigned long)expected_center,
                              (unsigned long)corner,
                              (unsigned long)expected_corner);
  }

  return 0;
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

static int RunChild(aerogpu_test::TestReporter* reporter,
                    int argc,
                    char** argv,
                    const AdapterRequirements& req,
                    bool dump,
                    bool validate_sharing) {
  const char* kTestName = "d3d9ex_shared_surface(child)";

  std::string handle_str;
  if (!aerogpu_test::GetArgValue(argc, argv, "--shared-handle", &handle_str)) {
    return reporter->Fail("missing required --shared-handle in --child mode");
  }
  std::string ready_event_str;
  std::string opened_event_str;
  std::string done_event_str;
  aerogpu_test::GetArgValue(argc, argv, "--ready-event", &ready_event_str);
  aerogpu_test::GetArgValue(argc, argv, "--opened-event", &opened_event_str);
  aerogpu_test::GetArgValue(argc, argv, "--done-event", &done_event_str);

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
      return reporter->Fail("invalid --expected-debug-token: %s", parse_err.c_str());
    }
    has_expected_debug_token = true;
  }

  SharedResourceKind kind = kSharedTexture;
  std::string kind_str;
  if (aerogpu_test::GetArgValue(argc, argv, "--resource", &kind_str)) {
    if (aerogpu_test::StrIContainsA(kind_str.c_str(), "rendertarget") ||
        aerogpu_test::StrIContainsA(kind_str.c_str(), "rt")) {
      kind = kSharedRenderTarget;
    } else if (aerogpu_test::StrIContainsA(kind_str.c_str(), "texture") ||
               aerogpu_test::StrIContainsA(kind_str.c_str(), "tex")) {
      kind = kSharedTexture;
    } else {
      return reporter->Fail("invalid --resource (expected texture|rendertarget)");
    }
  }

  uintptr_t handle_value = 0;
  std::string err;
  if (!ParseUintPtr(handle_str, &handle_value, &err) || handle_value == 0) {
    return reporter->Fail("invalid --shared-handle: %s", err.c_str());
  }

  const HANDLE shared_handle = (HANDLE)handle_value;
  const bool shared_handle_is_nt = IsLikelyNtHandle(shared_handle);
  aerogpu_test::PrintfStdout("INFO: %s: shared handle=%p", kTestName, shared_handle);

  HANDLE ready_event = NULL;
  HANDLE opened_event = NULL;
  HANDLE done_event = NULL;
  if (!ready_event_str.empty() || !opened_event_str.empty() || !done_event_str.empty()) {
    if (ready_event_str.empty() || opened_event_str.empty() || done_event_str.empty()) {
      return reporter->Fail(
          "internal: incomplete event args (ready/opened/done all required when any are used)");
    }
    std::wstring ready_name(ready_event_str.begin(), ready_event_str.end());
    std::wstring opened_name(opened_event_str.begin(), opened_event_str.end());
    std::wstring done_name(done_event_str.begin(), done_event_str.end());

    ready_event = OpenEventW(SYNCHRONIZE, FALSE, ready_name.c_str());
    if (!ready_event) {
      return reporter->Fail("OpenEvent(ready) failed: %s",
                            aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    }
    opened_event = OpenEventW(EVENT_MODIFY_STATE, FALSE, opened_name.c_str());
    if (!opened_event) {
      CloseHandle(ready_event);
      return reporter->Fail("OpenEvent(opened) failed: %s",
                            aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    }
    done_event = OpenEventW(EVENT_MODIFY_STATE, FALSE, done_name.c_str());
    if (!done_event) {
      CloseHandle(opened_event);
      CloseHandle(ready_event);
      return reporter->Fail("OpenEvent(done) failed: %s",
                            aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    }
  }

  const int kWidth = 64;
  const int kHeight = 64;
  const D3DFORMAT kFormat = D3DFMT_X8R8G8B8;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurfaceChild",
                                              L"AeroGPU D3D9Ex Shared Surface (Child)",
                                              kWidth,
                                              kHeight,
                                              false);
  if (!hwnd) {
    return reporter->Fail("CreateBasicWindow(child) failed");
  }

  if (has_expected_debug_token) {
    uint32_t token = 0;
    std::string map_err;
    if (!MapSharedHandleToken(hwnd, shared_handle, &token, &map_err)) {
      return reporter->Fail("MAP_SHARED_HANDLE failed: %s", map_err.c_str());
    }
    aerogpu_test::PrintfStdout("INFO: %s: MAP_SHARED_HANDLE debug_token=%lu (expected=%lu)",
                               kTestName,
                               (unsigned long)token,
                               (unsigned long)expected_debug_token);
    if (token != expected_debug_token) {
      return reporter->Fail("MAP_SHARED_HANDLE token mismatch: got=%lu expected=%lu",
                            (unsigned long)token,
                            (unsigned long)expected_debug_token);
    }
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(reporter, kTestName, hwnd, kWidth, kHeight, req, &d3d, &dev);
  if (rc != 0) {
    return rc;
  }

  HANDLE open_handle = shared_handle;
  ComPtr<IDirect3DSurface9> surface;
  HRESULT hr = S_OK;
  if (kind == kSharedTexture) {
    ComPtr<IDirect3DTexture9> tex;
    hr = dev->CreateTexture(kWidth,
                            kHeight,
                            1,
                            D3DUSAGE_RENDERTARGET,
                            kFormat,
                            D3DPOOL_DEFAULT,
                            tex.put(),
                            &open_handle);
    if (FAILED(hr)) {
      const HRESULT create_hr = hr;
      open_handle = shared_handle;
      HRESULT open_hr =
          dev->OpenSharedResource(shared_handle,
                                  IID_IDirect3DTexture9,
                                  reinterpret_cast<void**>(tex.put()));
      if (FAILED(open_hr)) {
        return reporter->Fail(
            "CreateTexture(open shared) failed with %s; OpenSharedResource(shared texture) failed with %s",
            aerogpu_test::HresultToString(create_hr).c_str(),
            aerogpu_test::HresultToString(open_hr).c_str());
      }
      aerogpu_test::PrintfStdout("INFO: %s: CreateTexture(open shared) failed; OpenSharedResource(texture) succeeded", kTestName);
    }
    hr = tex->GetSurfaceLevel(0, surface.put());
    if (FAILED(hr)) {
      return reporter->FailHresult("IDirect3DTexture9::GetSurfaceLevel", hr);
    }
  } else {
    hr = dev->CreateRenderTargetEx(kWidth,
                                   kHeight,
                                   kFormat,
                                   D3DMULTISAMPLE_NONE,
                                   0,
                                   FALSE,
                                   surface.put(),
                                   &open_handle,
                                   0);
    if (FAILED(hr)) {
      const HRESULT create_hr = hr;
      open_handle = shared_handle;
      HRESULT open_hr =
          dev->OpenSharedResource(shared_handle,
                                  IID_IDirect3DSurface9,
                                  reinterpret_cast<void**>(surface.put()));
      if (FAILED(open_hr)) {
        return reporter->Fail(
            "CreateRenderTargetEx(open shared) failed with %s; OpenSharedResource(shared surface) failed with %s",
            aerogpu_test::HresultToString(create_hr).c_str(),
            aerogpu_test::HresultToString(open_hr).c_str());
      }
      aerogpu_test::PrintfStdout("INFO: %s: CreateRenderTargetEx(open shared) failed; OpenSharedResource(surface) succeeded", kTestName);
    }
  }

  if (opened_event) {
    SetEvent(opened_event);
  }
  if (ready_event) {
    // Allow the parent to take up to ~25s total (it enforces its own end-to-end budget).
    DWORD wait = WaitForSingleObject(ready_event, 25000);
    if (wait != WAIT_OBJECT_0) {
      if (done_event) {
        SetEvent(done_event);
      }
      if (done_event) {
        CloseHandle(done_event);
      }
      if (opened_event) {
        CloseHandle(opened_event);
      }
      CloseHandle(ready_event);
      return reporter->Fail("WaitForSingleObject(ready) failed: 0x%08lX", (unsigned long)wait);
    }
  }

  // Exercise a minimal GPU operation that references the opened resource without disturbing the
  // pixels we validate (corner + center). This helps validate the "open + submit" path without
  // needing full rendering.
  RECT touch = {kWidth - 4, kHeight - 4, kWidth, kHeight};
  hr = dev->ColorFill(surface.get(), &touch, D3DCOLOR_XRGB(0, 128, 255));
  if (FAILED(hr)) {
    if (done_event) {
      SetEvent(done_event);
    }
    if (done_event) {
      CloseHandle(done_event);
    }
    if (opened_event) {
      CloseHandle(opened_event);
    }
    if (ready_event) {
      CloseHandle(ready_event);
    }
    return reporter->FailHresult("IDirect3DDevice9Ex::ColorFill(opened surface)", hr);
  }
  hr = dev->Flush();
  if (FAILED(hr)) {
    if (done_event) {
      SetEvent(done_event);
    }
    if (done_event) {
      CloseHandle(done_event);
    }
    if (opened_event) {
      CloseHandle(opened_event);
    }
    if (ready_event) {
      CloseHandle(ready_event);
    }
    return reporter->FailHresult("IDirect3DDevice9Ex::Flush", hr);
  }

  if (validate_sharing) {
    rc = ValidateSurfacePixels(reporter,
                               kTestName,
                               L"d3d9ex_shared_surface_child.bmp",
                               dump,
                               dev.get(),
                               surface.get());
    if (rc != 0) {
      // Still signal done_event so the parent can proceed to collect the child's exit code.
      goto cleanup;
    }
  }

cleanup:
  if (open_handle && open_handle != shared_handle && IsLikelyNtHandle(open_handle)) {
    CloseHandle(open_handle);
  }
  if (shared_handle_is_nt) {
    CloseHandle(shared_handle);
  }
  if (done_event) {
    SetEvent(done_event);
    CloseHandle(done_event);
  }
  if (opened_event) {
    CloseHandle(opened_event);
  }
  if (ready_event) {
    CloseHandle(ready_event);
  }

  if (rc == 0 && reporter) {
    return reporter->Pass();
  }
  if (rc == 0) {
    aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  }
  return rc;
}

static int RunParent(aerogpu_test::TestReporter* reporter,
                     int argc,
                     char** argv,
                     const AdapterRequirements& req,
                     bool dump,
                     bool hidden,
                     bool validate_sharing) {
  const char* kTestName = "d3d9ex_shared_surface";
  const std::wstring child_bmp_path =
      aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), L"d3d9ex_shared_surface_child.bmp");
  if (dump) {
    // Ensure we don't report a stale BMP from a previous run if the child fails before dumping.
    DeleteFileW(child_bmp_path.c_str());
  }

  const int kWidth = 64;
  const int kHeight = 64;
  const D3DFORMAT kFormat = D3DFMT_X8R8G8B8;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurface",
                                              L"AeroGPU D3D9Ex Shared Surface",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return reporter->Fail("CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(reporter, kTestName, hwnd, kWidth, kHeight, req, &d3d, &dev);
  if (rc != 0) {
    return rc;
  }

  SharedResourceKind kind = kSharedTexture;
  HANDLE shared_handle = NULL;
  bool shared_handle_is_nt = false;
  ComPtr<IDirect3DTexture9> tex;
  ComPtr<IDirect3DSurface9> surface;

  // Prefer a shared render-target texture. If texture sharing is unavailable, fall back to a
  // shareable render-target surface.
  HRESULT hr = dev->CreateTexture(kWidth,
                                  kHeight,
                                  1,
                                  D3DUSAGE_RENDERTARGET,
                                  kFormat,
                                  D3DPOOL_DEFAULT,
                                  tex.put(),
                                  &shared_handle);
  if (SUCCEEDED(hr) && tex && shared_handle) {
    kind = kSharedTexture;
    shared_handle_is_nt = IsLikelyNtHandle(shared_handle);
    hr = tex->GetSurfaceLevel(0, surface.put());
    if (FAILED(hr)) {
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      return reporter->FailHresult("IDirect3DTexture9::GetSurfaceLevel", hr);
    }
  } else {
    tex.reset();
    shared_handle = NULL;
    kind = kSharedRenderTarget;
    hr = dev->CreateRenderTargetEx(kWidth,
                                   kHeight,
                                   kFormat,
                                   D3DMULTISAMPLE_NONE,
                                   0,
                                   FALSE,
                                   surface.put(),
                                   &shared_handle,
                                   0);
    if (FAILED(hr)) {
      return reporter->FailHresult("CreateRenderTargetEx(create shared)", hr);
    }
    if (!shared_handle) {
      return reporter->Fail("CreateRenderTargetEx(create shared) succeeded but returned NULL shared handle");
    }
    shared_handle_is_nt = IsLikelyNtHandle(shared_handle);
  }

  // Always do a minimal GPU op so the resource is initialized before the child opens it.
  hr = dev->ColorFill(surface.get(), NULL, D3DCOLOR_XRGB(0, 0, 255));
  if (FAILED(hr)) {
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    return reporter->FailHresult("IDirect3DDevice9Ex::ColorFill(parent init)", hr);
  }
  hr = dev->Flush();
  if (FAILED(hr)) {
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    return reporter->FailHresult("IDirect3DDevice9Ex::Flush(parent init)", hr);
  }

  aerogpu_test::PrintfStdout("INFO: %s: parent shared handle=%s (%s)",
                             kTestName,
                             FormatHandleHex(shared_handle).c_str(),
                             (kind == kSharedTexture) ? "texture" : "rendertarget");

  // Ensure the shared handle is not inherited: the child should only observe it via DuplicateHandle
  // into the child process (which is closer to how DWM consumes app surfaces).
  if (shared_handle_is_nt) {
    SetHandleInformation(shared_handle, HANDLE_FLAG_INHERIT, 0);
  }

  uint32_t debug_token = 0;
  bool have_debug_token = false;
  std::string map_err;
  if (shared_handle_is_nt) {
    have_debug_token = MapSharedHandleToken(hwnd, shared_handle, &debug_token, &map_err);
    if (have_debug_token) {
      aerogpu_test::PrintfStdout(
          "INFO: %s: MAP_SHARED_HANDLE debug_token=%lu", kTestName, (unsigned long)debug_token);
    } else {
      aerogpu_test::PrintfStdout("INFO: %s: MAP_SHARED_HANDLE unavailable (%s); skipping token validation",
                                 kTestName,
                                 map_err.c_str());
    }
  } else {
    aerogpu_test::PrintfStdout(
        "INFO: %s: shared handle is not a real NT handle; skipping MAP_SHARED_HANDLE token validation",
        kTestName);
  }

  std::wstring exe_path = GetModulePath();
  if (exe_path.empty()) {
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    return reporter->Fail("GetModuleFileNameW failed");
  }

  HANDLE ready_event = NULL;
  HANDLE opened_event = NULL;
  HANDLE done_event = NULL;
  wchar_t ready_name[128];
  wchar_t opened_name[128];
  wchar_t done_name[128];
  if (validate_sharing) {
    const DWORD pid = GetCurrentProcessId();
    const DWORD tick = GetTickCount();
    _snwprintf(ready_name,
               ARRAYSIZE(ready_name),
               L"AeroGPU_%lu_%lu_d3d9ex_shared_ready",
               (unsigned long)pid,
               (unsigned long)tick);
    ready_name[ARRAYSIZE(ready_name) - 1] = 0;
    _snwprintf(opened_name,
               ARRAYSIZE(opened_name),
               L"AeroGPU_%lu_%lu_d3d9ex_shared_opened",
               (unsigned long)pid,
               (unsigned long)tick);
    opened_name[ARRAYSIZE(opened_name) - 1] = 0;
    _snwprintf(done_name,
               ARRAYSIZE(done_name),
               L"AeroGPU_%lu_%lu_d3d9ex_shared_done",
               (unsigned long)pid,
               (unsigned long)tick);
    done_name[ARRAYSIZE(done_name) - 1] = 0;

    ready_event = CreateEventW(NULL, TRUE, FALSE, ready_name);
    opened_event = CreateEventW(NULL, TRUE, FALSE, opened_name);
    done_event = CreateEventW(NULL, TRUE, FALSE, done_name);
    if (!ready_event || !opened_event || !done_event) {
      if (ready_event) {
        CloseHandle(ready_event);
      }
      if (opened_event) {
        CloseHandle(opened_event);
      }
      if (done_event) {
        CloseHandle(done_event);
      }
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      return reporter->Fail("CreateEvent failed: %s",
                            aerogpu_test::Win32ErrorToString(GetLastError()).c_str());
    }
  }

  const std::string placeholder_hex = FormatHandleHex((HANDLE)0);
  std::wstring cmdline = L"\"";
  cmdline += exe_path;
  cmdline += L"\" --child --resource=";
  cmdline += (kind == kSharedTexture) ? L"texture" : L"rendertarget";
  cmdline += L" --shared-handle=";
  cmdline += std::wstring(placeholder_hex.begin(), placeholder_hex.end());
  if (have_debug_token) {
    wchar_t token_buf[32];
    _snwprintf(token_buf, ARRAYSIZE(token_buf), L"0x%08lX", (unsigned long)debug_token);
    token_buf[ARRAYSIZE(token_buf) - 1] = 0;
    cmdline += L" --expected-debug-token=";
    cmdline += token_buf;
  }
  cmdline += L" --hidden";
  if (dump) {
    cmdline += L" --dump";
  }
  if (validate_sharing) {
    cmdline += L" --validate-sharing";
  } else {
    cmdline += L" --no-validate-sharing";
  }
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
  if (validate_sharing) {
    cmdline += L" --ready-event=";
    cmdline += ready_name;
    cmdline += L" --opened-event=";
    cmdline += opened_name;
    cmdline += L" --done-event=";
    cmdline += done_name;
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
    if (ready_event) {
      CloseHandle(ready_event);
    }
    if (opened_event) {
      CloseHandle(opened_event);
    }
    if (done_event) {
      CloseHandle(done_event);
    }
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    if (reporter) {
      return reporter->Fail("CreateProcessW failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
    }
    return aerogpu_test::Fail(kTestName,
                              "CreateProcessW failed: %s",
                              aerogpu_test::Win32ErrorToString(err).c_str());
  }

  job = CreateJobObjectW(NULL, NULL);
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

  std::string patch_err;
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
    aerogpu_test::PrintfStdout("INFO: %s: duplicated handle into child as %s",
                               kTestName,
                               child_handle_hex.c_str());
  } else {
    child_handle_hex = FormatHandleHex(shared_handle);
    aerogpu_test::PrintfStdout("INFO: %s: DuplicateHandle(into child) failed (%s); passing raw handle %s",
                               kTestName,
                               aerogpu_test::Win32ErrorToString(duplicate_err).c_str(),
                               child_handle_hex.c_str());
  }

  if (!PatchChildCommandLineSharedHandle(pi.hProcess, child_handle_hex, &patch_err)) {
    TerminateProcess(pi.hProcess, 1);
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (ready_event) {
      CloseHandle(ready_event);
    }
    if (opened_event) {
      CloseHandle(opened_event);
    }
    if (done_event) {
      CloseHandle(done_event);
    }
    if (job) {
      CloseHandle(job);
    }
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    if (reporter) {
      return reporter->Fail("failed to patch child command line: %s", patch_err.c_str());
    }
    return aerogpu_test::Fail(kTestName, "failed to patch child command line: %s", patch_err.c_str());
  }

  ResumeThread(pi.hThread);

  // Keep this comfortably below the suite's default per-test timeout (30s) so that if the child
  // hangs, we can still terminate it before aerogpu_timeout_runner.exe kills the parent, which
  // would otherwise leave an orphaned child process behind.
  const DWORD kChildTimeoutMs = 25000;
  const DWORD start_ticks = GetTickCount();

  if (validate_sharing) {
    HANDLE wait_open[2] = {opened_event, pi.hProcess};
    DWORD wait_budget = RemainingTimeoutMs(start_ticks, kChildTimeoutMs);
    DWORD opened_wait = WaitForMultipleObjects(2, wait_open, FALSE, wait_budget);
    if (opened_wait != WAIT_OBJECT_0) {
      DWORD exit_code = 1;
      GetExitCodeProcess(pi.hProcess, &exit_code);
      TerminateProcess(pi.hProcess, 124);
      WaitForSingleObject(pi.hProcess, 5000);
      CloseHandle(pi.hThread);
      CloseHandle(pi.hProcess);
      CloseHandle(ready_event);
      CloseHandle(opened_event);
      CloseHandle(done_event);
      if (job) {
        CloseHandle(job);
      }
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      if (opened_wait == WAIT_OBJECT_0 + 1) {
        if (reporter) {
          return reporter->Fail("child exited early (exit_code=%lu)", (unsigned long)exit_code);
        }
        return aerogpu_test::Fail(kTestName, "child exited early (exit_code=%lu)", (unsigned long)exit_code);
      }
      if (reporter) {
        return reporter->Fail("timeout waiting for child to open shared resource");
      }
      return aerogpu_test::Fail(kTestName, "timeout waiting for child to open shared resource");
    }

    rc = RenderTriangleToSurface(reporter, kTestName, dev.get(), surface.get(), kWidth, kHeight);
    if (rc != 0) {
      TerminateProcess(pi.hProcess, 1);
      WaitForSingleObject(pi.hProcess, 5000);
      CloseHandle(pi.hThread);
      CloseHandle(pi.hProcess);
      CloseHandle(ready_event);
      CloseHandle(opened_event);
      CloseHandle(done_event);
      if (job) {
        CloseHandle(job);
      }
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      return rc;
    }

    rc = ValidateSurfacePixels(
        reporter, kTestName, L"d3d9ex_shared_surface_parent.bmp", dump, dev.get(), surface.get());
    if (rc != 0) {
      TerminateProcess(pi.hProcess, 1);
      WaitForSingleObject(pi.hProcess, 5000);
      CloseHandle(pi.hThread);
      CloseHandle(pi.hProcess);
      CloseHandle(ready_event);
      CloseHandle(opened_event);
      CloseHandle(done_event);
      if (job) {
        CloseHandle(job);
      }
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      return rc;
    }

    SetEvent(ready_event);

    HANDLE wait_done[2] = {done_event, pi.hProcess};
    wait_budget = RemainingTimeoutMs(start_ticks, kChildTimeoutMs);
    DWORD done_wait = WaitForMultipleObjects(2, wait_done, FALSE, wait_budget);
    if (done_wait != WAIT_OBJECT_0) {
      DWORD exit_code = 1;
      GetExitCodeProcess(pi.hProcess, &exit_code);
      TerminateProcess(pi.hProcess, 124);
      WaitForSingleObject(pi.hProcess, 5000);
      CloseHandle(pi.hThread);
      CloseHandle(pi.hProcess);
      CloseHandle(ready_event);
      CloseHandle(opened_event);
      CloseHandle(done_event);
      if (job) {
        CloseHandle(job);
      }
      if (shared_handle_is_nt) {
        CloseHandle(shared_handle);
      }
      if (done_wait == WAIT_OBJECT_0 + 1) {
        if (reporter) {
          return reporter->Fail("child exited early (exit_code=%lu)", (unsigned long)exit_code);
        }
        return aerogpu_test::Fail(kTestName, "child exited early (exit_code=%lu)", (unsigned long)exit_code);
      }
      if (reporter) {
        return reporter->Fail("timeout waiting for child completion");
      }
      return aerogpu_test::Fail(kTestName, "timeout waiting for child completion");
    }
  }

  DWORD wait_budget = RemainingTimeoutMs(start_ticks, kChildTimeoutMs);
  DWORD wait = WaitForSingleObject(pi.hProcess, wait_budget);
  if (wait == WAIT_TIMEOUT) {
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (ready_event) {
      CloseHandle(ready_event);
    }
    if (opened_event) {
      CloseHandle(opened_event);
    }
    if (done_event) {
      CloseHandle(done_event);
    }
    if (job) {
      CloseHandle(job);
    }
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    if (reporter) {
      return reporter->Fail("child timed out");
    }
    return aerogpu_test::Fail(kTestName, "child timed out");
  }
  if (wait != WAIT_OBJECT_0) {
    DWORD err = GetLastError();
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (ready_event) {
      CloseHandle(ready_event);
    }
    if (opened_event) {
      CloseHandle(opened_event);
    }
    if (done_event) {
      CloseHandle(done_event);
    }
    if (job) {
      CloseHandle(job);
    }
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    if (reporter) {
      return reporter->Fail("WaitForSingleObject(child) failed: %s",
                            aerogpu_test::Win32ErrorToString(err).c_str());
    }
    return aerogpu_test::Fail(kTestName,
                              "WaitForSingleObject(child) failed: %s",
                              aerogpu_test::Win32ErrorToString(err).c_str());
  }

  DWORD exit_code = 1;
  if (!GetExitCodeProcess(pi.hProcess, &exit_code)) {
    DWORD err = GetLastError();
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    if (ready_event) {
      CloseHandle(ready_event);
    }
    if (opened_event) {
      CloseHandle(opened_event);
    }
    if (done_event) {
      CloseHandle(done_event);
    }
    if (job) {
      CloseHandle(job);
    }
    if (shared_handle_is_nt) {
      CloseHandle(shared_handle);
    }
    if (reporter) {
      return reporter->Fail("GetExitCodeProcess failed: %s",
                            aerogpu_test::Win32ErrorToString(err).c_str());
    }
    return aerogpu_test::Fail(kTestName,
                              "GetExitCodeProcess failed: %s",
                              aerogpu_test::Win32ErrorToString(err).c_str());
  }

  CloseHandle(pi.hThread);
  CloseHandle(pi.hProcess);
  if (ready_event) {
    CloseHandle(ready_event);
  }
  if (opened_event) {
    CloseHandle(opened_event);
  }
  if (done_event) {
    CloseHandle(done_event);
  }
  if (job) {
    CloseHandle(job);
  }
  if (shared_handle_is_nt) {
    CloseHandle(shared_handle);
  }

  if (dump && reporter) {
    reporter->AddArtifactPathIfExistsW(child_bmp_path);
  }
  if (exit_code != 0) {
    if (reporter) {
      return reporter->Fail("child failed with exit code %lu", (unsigned long)exit_code);
    }
    return aerogpu_test::Fail(kTestName, "child failed with exit code %lu", (unsigned long)exit_code);
  }
  if (reporter) {
    return reporter->Pass();
  }
  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

static int RunSharedSurfaceTest(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--show] [--json[=PATH]] [--validate-sharing] [--no-validate-sharing] "
        "[--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    aerogpu_test::PrintfStdout("Note: pixel sharing is validated by default; pass --no-validate-sharing to skip readback validation.");
    aerogpu_test::PrintfStdout("Note: --dump implies --validate-sharing.");
    aerogpu_test::PrintfStdout("Note: window is shown by default; pass --hidden to hide it.");
    aerogpu_test::PrintfStdout(
        "Internal: %s.exe --child --resource=texture|rendertarget --shared-handle=0x... "
        "[--expected-debug-token=0x...] [--ready-event=NAME --opened-event=NAME --done-event=NAME] [--require-umd] (used by parent)",
        kTestName);
    return 0;
  }

  const bool child = aerogpu_test::HasArg(argc, argv, "--child");
  const char* report_name = child ? "d3d9ex_shared_surface(child)" : kTestName;
  aerogpu_test::TestReporter reporter(report_name, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  bool validate_sharing = !aerogpu_test::HasArg(argc, argv, "--no-validate-sharing");
  if (aerogpu_test::HasArg(argc, argv, "--validate-sharing") || dump) {
    validate_sharing = true;
  }
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");
  // --show is a d3d9ex_shared_surface-specific override, useful when running the suite with
  // --hidden but wanting to observe this particular test.
  if (aerogpu_test::HasArg(argc, argv, "--show")) {
    hidden = false;
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
      return reporter.Fail("invalid --require-vid: %s", err.c_str());
    }
    req.has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &req.require_did, &err)) {
      return reporter.Fail("invalid --require-did: %s", err.c_str());
    }
    req.has_require_did = true;
  }

  if (child) {
    return RunChild(&reporter, argc, argv, req, dump, validate_sharing);
  }
  return RunParent(&reporter, argc, argv, req, dump, hidden, validate_sharing);
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunSharedSurfaceTest(argc, argv);
}
