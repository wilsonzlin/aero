#include "..\\common\\aerogpu_test_common.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

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

  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);

  out_d3d->reset(d3d.detach());
  out_dev->reset(dev.detach());
  return 0;
}

static int RenderTriangleToSurface(const char* test_name,
                                  IDirect3DDevice9Ex* dev,
                                  IDirect3DSurface9* surface,
                                  int width,
                                  int height) {
  if (!dev || !surface) {
    return aerogpu_test::Fail(test_name, "internal: RenderTriangleToSurface called with NULL");
  }

  ComPtr<IDirect3DSurface9> old_rt;
  HRESULT hr = dev->GetRenderTarget(0, old_rt.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::GetRenderTarget", hr);
  }

  hr = dev->SetRenderTarget(0, surface);
  if (FAILED(hr)) {
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
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::SetViewport", hr);
  }

  const DWORD kRed = D3DCOLOR_XRGB(255, 0, 0);
  const DWORD kGreen = D3DCOLOR_XRGB(0, 255, 0);

  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, kRed, 1.0f, 0);
  if (FAILED(hr)) {
    dev->SetRenderTarget(0, old_rt.get());
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::Clear", hr);
  }

  Vertex verts[3];
  // Triangle that covers the center pixel while leaving the top-left corner untouched.
  verts[0].x = (float)width * 0.25f;
  verts[0].y = (float)height * 0.25f;
  verts[0].z = 0.5f;
  verts[0].rhw = 1.0f;
  verts[0].color = kGreen;
  verts[1].x = (float)width * 0.75f;
  verts[1].y = (float)height * 0.25f;
  verts[1].z = 0.5f;
  verts[1].rhw = 1.0f;
  verts[1].color = kGreen;
  verts[2].x = (float)width * 0.5f;
  verts[2].y = (float)height * 0.75f;
  verts[2].z = 0.5f;
  verts[2].rhw = 1.0f;
  verts[2].color = kGreen;

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    dev->SetRenderTarget(0, old_rt.get());
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::BeginScene", hr);
  }

  hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
  if (FAILED(hr)) {
    dev->EndScene();
    dev->SetRenderTarget(0, old_rt.get());
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::SetFVF", hr);
  }

  hr = dev->DrawPrimitiveUP(D3DPT_TRIANGLELIST, 1, verts, sizeof(Vertex));
  if (FAILED(hr)) {
    dev->EndScene();
    dev->SetRenderTarget(0, old_rt.get());
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::DrawPrimitiveUP", hr);
  }

  hr = dev->EndScene();
  if (FAILED(hr)) {
    dev->SetRenderTarget(0, old_rt.get());
    return aerogpu_test::FailHresult(test_name, "IDirect3DDevice9Ex::EndScene", hr);
  }

  dev->SetRenderTarget(0, old_rt.get());
  return 0;
}

static int ValidateSurfacePixels(const char* test_name,
                                const wchar_t* dump_name,
                                bool dump,
                                IDirect3DDevice9Ex* dev,
                                IDirect3DSurface9* surface) {
  if (!dev || !surface) {
    return aerogpu_test::Fail(test_name, "internal: ValidateSurfacePixels called with NULL");
  }

  D3DSURFACE_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  HRESULT hr = surface->GetDesc(&desc);
  if (FAILED(hr)) {
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
    return aerogpu_test::FailHresult(test_name, "CreateOffscreenPlainSurface", hr);
  }

  hr = dev->GetRenderTargetData(surface, sysmem.get());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(test_name, "GetRenderTargetData", hr);
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(test_name, "IDirect3DSurface9::LockRect", hr);
  }

  const int cx = (int)desc.Width / 2;
  const int cy = (int)desc.Height / 2;
  const uint32_t center = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, cx, cy);
  const uint32_t corner = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 5, 5);

  if (dump && dump_name) {
    std::string err;
    if (!aerogpu_test::WriteBmp32BGRA(
            aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), dump_name),
            (int)desc.Width,
            (int)desc.Height,
            lr.pBits,
            (int)lr.Pitch,
            &err)) {
      aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", test_name, err.c_str());
    }
  }

  sysmem->UnlockRect();

  const uint32_t expected_center = 0xFF00FF00u;  // BGRA = (0, 255, 0, 255).
  const uint32_t expected_corner = 0xFFFF0000u;  // BGRA = (0, 0, 255, 255).

  if ((center & 0x00FFFFFFu) != (expected_center & 0x00FFFFFFu) ||
      (corner & 0x00FFFFFFu) != (expected_corner & 0x00FFFFFFu)) {
    return aerogpu_test::Fail(test_name,
                              "pixel mismatch: center=0x%08lX corner(5,5)=0x%08lX",
                              (unsigned long)center,
                              (unsigned long)corner);
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

static int RunChild(int argc, char** argv, const AdapterRequirements& req, bool dump) {
  const char* kTestName = "d3d9ex_shared_surface(child)";

  std::string handle_str;
  if (!aerogpu_test::GetArgValue(argc, argv, "--shared-handle", &handle_str)) {
    return aerogpu_test::Fail(kTestName, "missing required --shared-handle in --child mode");
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
      return aerogpu_test::Fail(kTestName, "invalid --resource (expected texture|rendertarget)");
    }
  }

  uintptr_t handle_value = 0;
  std::string err;
  if (!ParseUintPtr(handle_str, &handle_value, &err) || handle_value == 0) {
    return aerogpu_test::Fail(kTestName, "invalid --shared-handle: %s", err.c_str());
  }

  const HANDLE shared_handle = (HANDLE)handle_value;
  aerogpu_test::PrintfStdout("INFO: %s: shared handle=%p", kTestName, shared_handle);

  const int kWidth = 64;
  const int kHeight = 64;
  const D3DFORMAT kFormat = D3DFMT_X8R8G8B8;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurfaceChild",
                                              L"AeroGPU D3D9Ex Shared Surface (Child)",
                                              kWidth,
                                              kHeight,
                                              false);
  if (!hwnd) {
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow(child) failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(kTestName, hwnd, kWidth, kHeight, req, &d3d, &dev);
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
      return aerogpu_test::FailHresult(kTestName, "CreateTexture(open shared)", hr);
    }
    hr = tex->GetSurfaceLevel(0, surface.put());
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DTexture9::GetSurfaceLevel", hr);
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
      return aerogpu_test::FailHresult(kTestName, "CreateRenderTargetEx(open shared)", hr);
    }
  }

  // Exercise a minimal GPU operation that references the opened resource without disturbing the
  // pixels we validate (corner + center). This helps validate the "open + submit" path without
  // needing full rendering.
  RECT touch = {kWidth - 4, kHeight - 4, kWidth, kHeight};
  hr = dev->ColorFill(surface.get(), &touch, D3DCOLOR_XRGB(0, 128, 255));
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::ColorFill(opened surface)", hr);
  }
  hr = dev->Flush();
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::Flush", hr);
  }

  rc = ValidateSurfacePixels(kTestName,
                             L"d3d9ex_shared_surface_child.bmp",
                             dump,
                             dev.get(),
                             surface.get());
  if (rc != 0) {
    return rc;
  }

  CloseHandle(shared_handle);

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

static int RunParent(int argc,
                     char** argv,
                     const AdapterRequirements& req,
                     bool dump,
                     bool hidden) {
  const char* kTestName = "d3d9ex_shared_surface";

  const int kWidth = 64;
  const int kHeight = 64;
  const D3DFORMAT kFormat = D3DFMT_X8R8G8B8;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedSurface",
                                              L"AeroGPU D3D9Ex Shared Surface",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  ComPtr<IDirect3DDevice9Ex> dev;
  int rc = CreateD3D9ExDevice(kTestName, hwnd, kWidth, kHeight, req, &d3d, &dev);
  if (rc != 0) {
    return rc;
  }

  SharedResourceKind kind = kSharedTexture;
  HANDLE shared_handle = NULL;
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
    hr = tex->GetSurfaceLevel(0, surface.put());
    if (FAILED(hr)) {
      CloseHandle(shared_handle);
      return aerogpu_test::FailHresult(kTestName, "IDirect3DTexture9::GetSurfaceLevel", hr);
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
      return aerogpu_test::FailHresult(kTestName, "CreateRenderTargetEx(create shared)", hr);
    }
    if (!shared_handle) {
      return aerogpu_test::Fail(kTestName,
                                "CreateRenderTargetEx(create shared) succeeded but returned NULL shared handle");
    }
  }

  rc = RenderTriangleToSurface(kTestName, dev.get(), surface.get(), kWidth, kHeight);
  if (rc != 0) {
    CloseHandle(shared_handle);
    return rc;
  }

  rc = ValidateSurfacePixels(
      kTestName, L"d3d9ex_shared_surface_parent.bmp", dump, dev.get(), surface.get());
  if (rc != 0) {
    CloseHandle(shared_handle);
    return rc;
  }

  aerogpu_test::PrintfStdout("INFO: %s: parent shared handle=%s (%s)",
                             kTestName,
                             FormatHandleHex(shared_handle).c_str(),
                             (kind == kSharedTexture) ? "texture" : "rendertarget");

  // Ensure the shared handle is not inherited: the child should only observe it via DuplicateHandle
  // into the child process (which is closer to how DWM consumes app surfaces).
  SetHandleInformation(shared_handle, HANDLE_FLAG_INHERIT, 0);

  std::wstring exe_path = GetModulePath();
  if (exe_path.empty()) {
    CloseHandle(shared_handle);
    return aerogpu_test::Fail(kTestName, "GetModuleFileNameW failed");
  }

  const std::string placeholder_hex = FormatHandleHex((HANDLE)0);
  std::wstring cmdline = L"\"";
  cmdline += exe_path;
  cmdline += L"\" --child --resource=";
  cmdline += (kind == kSharedTexture) ? L"texture" : L"rendertarget";
  cmdline += L" --shared-handle=";
  cmdline += std::wstring(placeholder_hex.begin(), placeholder_hex.end());
  cmdline += L" --hidden";
  if (dump) {
    cmdline += L" --dump";
  }
  if (req.allow_microsoft) {
    cmdline += L" --allow-microsoft";
  }
  if (req.allow_non_aerogpu) {
    cmdline += L" --allow-non-aerogpu";
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
    CloseHandle(shared_handle);
    return aerogpu_test::Fail(
        kTestName, "CreateProcessW failed: %s", aerogpu_test::Win32ErrorToString(err).c_str());
  }

  HANDLE child_handle_value = NULL;
  if (!DuplicateHandle(GetCurrentProcess(),
                       shared_handle,
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
    CloseHandle(shared_handle);
    return aerogpu_test::Fail(kTestName,
                              "DuplicateHandle(into child) failed: %s",
                              aerogpu_test::Win32ErrorToString(err).c_str());
  }

  std::string patch_err;
  const std::string child_handle_hex = FormatHandleHex(child_handle_value);
  aerogpu_test::PrintfStdout("INFO: %s: duplicated handle into child as %s",
                             kTestName,
                             child_handle_hex.c_str());
  if (!PatchChildCommandLineSharedHandle(pi.hProcess, child_handle_hex, &patch_err)) {
    TerminateProcess(pi.hProcess, 1);
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    CloseHandle(shared_handle);
    return aerogpu_test::Fail(kTestName, "failed to patch child command line: %s", patch_err.c_str());
  }

  ResumeThread(pi.hThread);

  // Keep this comfortably below the suite's default per-test timeout (30s) so that if the child
  // hangs, we can still terminate it before aerogpu_timeout_runner.exe kills the parent, which
  // would otherwise leave an orphaned child process behind.
  const DWORD kChildTimeoutMs = 20000;
  DWORD wait = WaitForSingleObject(pi.hProcess, kChildTimeoutMs);
  if (wait == WAIT_TIMEOUT) {
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    CloseHandle(shared_handle);
    return aerogpu_test::Fail(kTestName, "child timed out");
  }
  if (wait != WAIT_OBJECT_0) {
    DWORD err = GetLastError();
    TerminateProcess(pi.hProcess, 124);
    WaitForSingleObject(pi.hProcess, 5000);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    CloseHandle(shared_handle);
    return aerogpu_test::Fail(kTestName,
                              "WaitForSingleObject(child) failed: %s",
                              aerogpu_test::Win32ErrorToString(err).c_str());
  }

  DWORD exit_code = 1;
  if (!GetExitCodeProcess(pi.hProcess, &exit_code)) {
    DWORD err = GetLastError();
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    CloseHandle(shared_handle);
    return aerogpu_test::Fail(kTestName,
                              "GetExitCodeProcess failed: %s",
                              aerogpu_test::Win32ErrorToString(err).c_str());
  }

  CloseHandle(pi.hThread);
  CloseHandle(pi.hProcess);
  CloseHandle(shared_handle);

  if (exit_code != 0) {
    return aerogpu_test::Fail(kTestName, "child failed with exit code %lu", (unsigned long)exit_code);
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

static int RunSharedSurfaceTest(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_surface";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu]",
        kTestName);
    aerogpu_test::PrintfStdout("Internal: %s.exe --child --shared-handle=0x... (used by parent)", kTestName);
    return 0;
  }

  const bool child = aerogpu_test::HasArg(argc, argv, "--child");
  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");

  AdapterRequirements req;
  ZeroMemory(&req, sizeof(req));
  req.allow_microsoft = allow_microsoft;
  req.allow_non_aerogpu = allow_non_aerogpu;

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
    return RunChild(argc, argv, req, dump);
  }
  return RunParent(argc, argv, req, dump, hidden);
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunSharedSurfaceTest(argc, argv);
}
