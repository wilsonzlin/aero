#include "..\\common\\aerogpu_test_common.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

static int RunD3D9ExSharedAllocations(int argc, char** argv) {
  const char* kTestName = "d3d9ex_shared_allocations";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--hidden] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu]",
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
    std::string err;
    if (!aerogpu_test::ParseUint32(require_vid_str, &require_vid, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-vid: %s", err.c_str());
    }
    has_require_vid = true;
  }
  if (aerogpu_test::GetArgValue(argc, argv, "--require-did", &require_did_str)) {
    std::string err;
    if (!aerogpu_test::ParseUint32(require_did_str, &require_did, &err)) {
      return aerogpu_test::Fail(kTestName, "invalid --require-did: %s", err.c_str());
    }
    has_require_did = true;
  }

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExSharedAllocations",
                                              L"AeroGPU D3D9Ex Shared Allocations",
                                              64,
                                              64,
                                              !hidden);
  if (!hwnd) {
    return aerogpu_test::Fail(kTestName, "CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "Direct3DCreate9Ex", hr);
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
    return aerogpu_test::FailHresult(kTestName, "IDirect3D9Ex::CreateDeviceEx", hr);
  }

  D3DADAPTER_IDENTIFIER9 ident;
  ZeroMemory(&ident, sizeof(ident));
  hr = d3d->GetAdapterIdentifier(D3DADAPTER_DEFAULT, 0, &ident);
  if (SUCCEEDED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: adapter: %s (VID=0x%04X DID=0x%04X)",
                               kTestName,
                               ident.Description,
                               (unsigned)ident.VendorId,
                               (unsigned)ident.DeviceId);
    if (!allow_microsoft && ident.VendorId == 0x1414) {
      return aerogpu_test::Fail(kTestName,
                                "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). "
                                "Install AeroGPU driver or pass --allow-microsoft.",
                                (unsigned)ident.VendorId,
                                (unsigned)ident.DeviceId);
    }
    if (has_require_vid && ident.VendorId != require_vid) {
      return aerogpu_test::Fail(kTestName,
                                "adapter VID mismatch: got 0x%04X expected 0x%04X",
                                (unsigned)ident.VendorId,
                                (unsigned)require_vid);
    }
    if (has_require_did && ident.DeviceId != require_did) {
      return aerogpu_test::Fail(kTestName,
                                "adapter DID mismatch: got 0x%04X expected 0x%04X",
                                (unsigned)ident.DeviceId,
                                (unsigned)require_did);
    }
    if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
        !(ident.VendorId == 0x1414 && allow_microsoft) &&
        !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
      return aerogpu_test::Fail(kTestName,
                                "adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu "
                                "or use --require-vid/--require-did)",
                                ident.Description);
    }
  } else if (has_require_vid || has_require_did) {
    return aerogpu_test::FailHresult(
        kTestName,
        "GetAdapterIdentifier (required for --require-vid/--require-did)",
        hr);
  }

  // ---------------------------------------------------------------------------
  // Case 0: non-shared texture with multiple mip levels (Levels>1).
  //
  // This is a useful baseline even if the driver chooses to reject shared mip
  // chains: if the KMD logs show NumAllocations>1 here, shared mips are very
  // likely multi-allocation as well.
  // ---------------------------------------------------------------------------
  ComPtr<IDirect3DTexture9> non_shared_mip_tex;
  hr = dev->CreateTexture(128,
                          128,
                          4,
                          0,
                          D3DFMT_A8R8G8B8,
                          D3DPOOL_DEFAULT,
                          non_shared_mip_tex.put(),
                          NULL);
  if (SUCCEEDED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: non-shared mip texture created (Levels=4)", kTestName);
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: CreateTexture(non-shared mips) failed with %s",
                               kTestName,
                               aerogpu_test::HresultToString(hr).c_str());
  }

  // ---------------------------------------------------------------------------
  // Case A: shared render-target surface.
  // ---------------------------------------------------------------------------
  HANDLE shared_rt_surface_handle = NULL;
  ComPtr<IDirect3DSurface9> shared_rt_surface;
  hr = dev->CreateRenderTarget(256,
                               256,
                               D3DFMT_A8R8G8B8,
                               D3DMULTISAMPLE_NONE,
                               0,
                               FALSE,
                               shared_rt_surface.put(),
                               &shared_rt_surface_handle);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "CreateRenderTarget(shared)", hr);
  }
  aerogpu_test::PrintfStdout("INFO: %s: shared RT surface handle=%p", kTestName, shared_rt_surface_handle);
  if (!shared_rt_surface_handle) {
    return aerogpu_test::Fail(kTestName, "CreateRenderTarget(shared) returned NULL shared handle");
  }

  ComPtr<IDirect3DSurface9> opened_rt_surface;
  hr = dev->OpenSharedResource(shared_rt_surface_handle,
                               IID_IDirect3DSurface9,
                               reinterpret_cast<void**>(opened_rt_surface.put()));
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "OpenSharedResource(shared render target surface)", hr);
  }

  // ---------------------------------------------------------------------------
  // Case B: shared texture with multiple mip levels (Levels>1).
  // ---------------------------------------------------------------------------
  HANDLE shared_mip_handle = NULL;
  ComPtr<IDirect3DTexture9> shared_mip_tex;
  hr = dev->CreateTexture(128,
                          128,
                          4,
                          0,
                          D3DFMT_A8R8G8B8,
                          D3DPOOL_DEFAULT,
                          shared_mip_tex.put(),
                          &shared_mip_handle);
  if (SUCCEEDED(hr)) {
    aerogpu_test::PrintfStdout("INFO: %s: shared mip texture handle=%p", kTestName, shared_mip_handle);
    if (!shared_mip_handle) {
      return aerogpu_test::Fail(kTestName, "CreateTexture(shared mips) succeeded but returned NULL shared handle");
    }

    ComPtr<IDirect3DTexture9> opened_mip_tex;
    hr = dev->OpenSharedResource(shared_mip_handle,
                                 IID_IDirect3DTexture9,
                                 reinterpret_cast<void**>(opened_mip_tex.put()));
    if (FAILED(hr)) {
      return aerogpu_test::FailHresult(kTestName, "OpenSharedResource(shared mips)", hr);
    }
  } else {
    // This may be rejected by the driver if shared multi-mip resources are not supported.
    aerogpu_test::PrintfStdout("INFO: %s: CreateTexture(shared mips) failed with %s",
                               kTestName,
                               aerogpu_test::HresultToString(hr).c_str());
  }

  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9ExSharedAllocations(argc, argv);
}
