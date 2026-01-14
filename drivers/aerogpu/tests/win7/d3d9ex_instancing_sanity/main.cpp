#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>
#include <algorithm>
#include <cstring>
#include <string>

using aerogpu_test::ComPtr;

struct Vec4 {
  float x;
  float y;
  float z;
  float w;
};

struct InstanceData {
  Vec4 offset;
  Vec4 color;
};

// Vertex shader (vs_2_0):
//   add r0, v0, v1
//   mov oPos, r0
//   mov oD0, v2
//   end
static const DWORD kVsInstancing[] = {
    0xFFFE0200u, // vs_2_0
    0x03000002u, 0x000F0000u, 0x10E40000u, 0x10E40001u, // add r0, v0, v1
    0x02000001u, 0x400F0000u, 0x00E40000u, // mov oPos, r0
    0x02000001u, 0x500F0000u, 0x10E40002u, // mov oD0, v2
    0x0000FFFFu, // end
};

// Pixel shader (ps_2_0):
//   mov oC0, v0
//   end
static const DWORD kPsPassthroughColor[] = {
    0xFFFF0200u, // ps_2_0
    0x02000001u, 0x000F0800u, 0x10E40000u, // mov oC0, v0
    0x0000FFFFu, // end
};

static HRESULT CreateDeviceExWithFallback(IDirect3D9Ex* d3d,
                                         HWND hwnd,
                                         D3DPRESENT_PARAMETERS* pp,
                                         IDirect3DDevice9Ex** out_dev) {
  if (!d3d || !pp || !out_dev) {
    return E_INVALIDARG;
  }

  DWORD create_flags = D3DCREATE_HARDWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
  HRESULT hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                                   D3DDEVTYPE_HAL,
                                   hwnd,
                                   create_flags,
                                   pp,
                                   NULL,
                                   out_dev);
  if (FAILED(hr)) {
    create_flags = D3DCREATE_SOFTWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
    hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                             D3DDEVTYPE_HAL,
                             hwnd,
                             create_flags,
                             pp,
                             NULL,
                             out_dev);
  }
  return hr;
}

static void DumpBackbufferBmpIfEnabled(const char* test_name,
                                       aerogpu_test::TestReporter* reporter,
                                       bool dump,
                                       const wchar_t* bmp_name,
                                       const void* data,
                                       int row_pitch,
                                       int width,
                                       int height) {
  if (!dump || !bmp_name || !data || width <= 0 || height <= 0 || row_pitch <= 0) {
    return;
  }
  std::string err;
  const std::wstring bmp_path = aerogpu_test::JoinPath(aerogpu_test::GetModuleDir(), bmp_name);
  if (aerogpu_test::WriteBmp32BGRA(bmp_path, width, height, data, row_pitch, &err)) {
    if (reporter) {
      reporter->AddArtifactPathW(bmp_path);
    }
  } else {
    aerogpu_test::PrintfStdout("INFO: %s: BMP dump failed: %s", test_name ? test_name : "<null>", err.c_str());
  }
}

static int RunD3D9ExInstancingSanity(int argc, char** argv) {
  const char* kTestName = "d3d9ex_instancing_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--dump] [--hidden] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool dump = aerogpu_test::HasArg(argc, argv, "--dump");
  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
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

  const int kWidth = 256;
  const int kHeight = 256;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExInstancingSanity",
                                              L"AeroGPU D3D9Ex instancing sanity",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return reporter.Fail("CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("Direct3DCreate9Ex", hr);
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
    reporter.SetAdapterInfoA(ident.Description, ident.VendorId, ident.DeviceId);
    if (!allow_microsoft && ident.VendorId == 0x1414) {
      return reporter.Fail(
          "refusing to run on Microsoft adapter (VID=0x%04X DID=0x%04X). Install AeroGPU driver or pass --allow-microsoft.",
          (unsigned)ident.VendorId,
          (unsigned)ident.DeviceId);
    }
    if (has_require_vid && ident.VendorId != require_vid) {
      return reporter.Fail("adapter VID mismatch: got 0x%04X expected 0x%04X",
                           (unsigned)ident.VendorId,
                           (unsigned)require_vid);
    }
    if (has_require_did && ident.DeviceId != require_did) {
      return reporter.Fail("adapter DID mismatch: got 0x%04X expected 0x%04X",
                           (unsigned)ident.DeviceId,
                           (unsigned)require_did);
    }
    if (!allow_non_aerogpu && !has_require_vid && !has_require_did &&
        !(ident.VendorId == 0x1414 && allow_microsoft) &&
        !aerogpu_test::StrIContainsA(ident.Description, "AeroGPU")) {
      return reporter.Fail(
          "adapter does not look like AeroGPU: %s (pass --allow-non-aerogpu or use --require-vid/--require-did)",
          ident.Description);
    }
  } else if (has_require_vid || has_require_did) {
    return reporter.FailHresult("GetAdapterIdentifier (required for --require-vid/--require-did)", hr);
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  D3DPRESENT_PARAMETERS pp;
  ZeroMemory(&pp, sizeof(pp));
  pp.BackBufferWidth = kWidth;
  pp.BackBufferHeight = kHeight;
  pp.BackBufferFormat = D3DFMT_X8R8G8B8;
  pp.BackBufferCount = 1;
  pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
  pp.hDeviceWindow = hwnd;
  pp.Windowed = TRUE;
  pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;

  ComPtr<IDirect3DDevice9Ex> dev;
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, dev.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateDeviceEx", hr);
  }

  dev->SetRenderState(D3DRS_CULLMODE, D3DCULL_NONE);
  dev->SetRenderState(D3DRS_LIGHTING, FALSE);
  dev->SetRenderState(D3DRS_ALPHABLENDENABLE, FALSE);
  dev->SetRenderState(D3DRS_ZENABLE, FALSE);

  ComPtr<IDirect3DVertexShader9> vs;
  hr = dev->CreateVertexShader(kVsInstancing, vs.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexShader", hr);
  }
  ComPtr<IDirect3DPixelShader9> ps;
  hr = dev->CreatePixelShader(kPsPassthroughColor, ps.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreatePixelShader", hr);
  }
  hr = dev->SetVertexShader(vs.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexShader", hr);
  }
  hr = dev->SetPixelShader(ps.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetPixelShader", hr);
  }

  const D3DVERTEXELEMENT9 decl_elems[] = {
      {0, 0, D3DDECLTYPE_FLOAT4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
      {1, 0, D3DDECLTYPE_FLOAT4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0}, // per-instance offset
      {1, 16, D3DDECLTYPE_FLOAT4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR, 0}, // per-instance color
      D3DDECL_END(),
  };
  ComPtr<IDirect3DVertexDeclaration9> decl;
  hr = dev->CreateVertexDeclaration(decl_elems, decl.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexDeclaration", hr);
  }
  hr = dev->SetVertexDeclaration(decl.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetVertexDeclaration", hr);
  }

  // Triangle centered at the origin in clip space. Instances apply X offsets.
  const Vec4 vertices[3] = {
      {-0.3f, -0.6f, 0.5f, 1.0f},
      {0.3f, -0.6f, 0.5f, 1.0f},
      {0.0f, 0.6f, 0.5f, 1.0f},
  };

  // Two instances: left = red, right = green.
  const InstanceData instances[2] = {
      {{-0.5f, 0.0f, 0.0f, 0.0f}, {1.0f, 0.0f, 0.0f, 1.0f}},
      {{0.5f, 0.0f, 0.0f, 0.0f}, {0.0f, 1.0f, 0.0f, 1.0f}},
  };

  const WORD indices[3] = {0, 1, 2};

  ComPtr<IDirect3DVertexBuffer9> vb0;
  hr = dev->CreateVertexBuffer(sizeof(vertices), D3DUSAGE_WRITEONLY, 0, D3DPOOL_DEFAULT, vb0.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexBuffer(stream0)", hr);
  }
  ComPtr<IDirect3DVertexBuffer9> vb1;
  hr = dev->CreateVertexBuffer(sizeof(instances), D3DUSAGE_WRITEONLY, 0, D3DPOOL_DEFAULT, vb1.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateVertexBuffer(stream1)", hr);
  }
  ComPtr<IDirect3DIndexBuffer9> ib;
  hr = dev->CreateIndexBuffer(sizeof(indices), D3DUSAGE_WRITEONLY, D3DFMT_INDEX16, D3DPOOL_DEFAULT, ib.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateIndexBuffer", hr);
  }

  void* p = NULL;
  hr = vb0->Lock(0, sizeof(vertices), &p, 0);
  if (FAILED(hr) || !p) {
    return reporter.FailHresult("vb0 Lock", FAILED(hr) ? hr : E_FAIL);
  }
  memcpy(p, vertices, sizeof(vertices));
  vb0->Unlock();

  p = NULL;
  hr = vb1->Lock(0, sizeof(instances), &p, 0);
  if (FAILED(hr) || !p) {
    return reporter.FailHresult("vb1 Lock", FAILED(hr) ? hr : E_FAIL);
  }
  memcpy(p, instances, sizeof(instances));
  vb1->Unlock();

  p = NULL;
  hr = ib->Lock(0, sizeof(indices), &p, 0);
  if (FAILED(hr) || !p) {
    return reporter.FailHresult("ib Lock", FAILED(hr) ? hr : E_FAIL);
  }
  memcpy(p, indices, sizeof(indices));
  ib->Unlock();

  hr = dev->SetStreamSource(0, vb0.get(), 0, sizeof(Vec4));
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource(stream0)", hr);
  }
  hr = dev->SetStreamSource(1, vb1.get(), 0, sizeof(InstanceData));
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource(stream1)", hr);
  }
  hr = dev->SetIndices(ib.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("SetIndices", hr);
  }

  // Instancing state.
  hr = dev->SetStreamSourceFreq(0, D3DSTREAMSOURCE_INDEXEDDATA | 2u);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSourceFreq(stream0)", hr);
  }
  hr = dev->SetStreamSourceFreq(1, D3DSTREAMSOURCE_INSTANCEDATA | 1u);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSourceFreq(stream1)", hr);
  }

  const D3DCOLOR clear = D3DCOLOR_XRGB(8, 8, 8);
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, clear, 1.0f, 0);
  if (FAILED(hr)) {
    return reporter.FailHresult("Clear", hr);
  }

  hr = dev->BeginScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginScene", hr);
  }

  hr = dev->DrawIndexedPrimitive(D3DPT_TRIANGLELIST, 0, 0, 3, 0, 1);
  if (FAILED(hr)) {
    dev->EndScene();
    return reporter.FailHresult("DrawIndexedPrimitive(instanced)", hr);
  }

  hr = dev->EndScene();
  if (FAILED(hr)) {
    return reporter.FailHresult("EndScene", hr);
  }

  dev->Flush();

  // Read back the backbuffer before PresentEx: with D3DSWAPEFFECT_DISCARD contents after Present are undefined.
  ComPtr<IDirect3DSurface9> backbuffer;
  hr = dev->GetBackBuffer(0, 0, D3DBACKBUFFER_TYPE_MONO, backbuffer.put());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetBackBuffer", hr);
  }

  D3DSURFACE_DESC desc;
  ZeroMemory(&desc, sizeof(desc));
  hr = backbuffer->GetDesc(&desc);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DSurface9::GetDesc", hr);
  }

  ComPtr<IDirect3DSurface9> sysmem;
  hr = dev->CreateOffscreenPlainSurface(desc.Width, desc.Height, desc.Format, D3DPOOL_SYSTEMMEM, sysmem.put(), NULL);
  if (FAILED(hr)) {
    return reporter.FailHresult("CreateOffscreenPlainSurface", hr);
  }

  hr = dev->GetRenderTargetData(backbuffer.get(), sysmem.get());
  if (FAILED(hr)) {
    return reporter.FailHresult("GetRenderTargetData", hr);
  }

  D3DLOCKED_RECT lr;
  ZeroMemory(&lr, sizeof(lr));
  hr = sysmem->LockRect(&lr, NULL, D3DLOCK_READONLY);
  if (FAILED(hr)) {
    return reporter.FailHresult("sysmem LockRect", hr);
  }

  const int lx = std::max(0, (int)desc.Width / 4);
  const int rx = std::min((int)desc.Width - 1, (int)desc.Width * 3 / 4);
  const int cy = std::min((int)desc.Height - 1, (int)desc.Height / 2);
  const uint32_t left = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, lx, cy);
  const uint32_t right = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, rx, cy);
  const uint32_t corner = aerogpu_test::ReadPixelBGRA(lr.pBits, (int)lr.Pitch, 5, 5);

  DumpBackbufferBmpIfEnabled(kTestName,
                             &reporter,
                             dump,
                             L"d3d9ex_instancing_sanity.bmp",
                             lr.pBits,
                             (int)lr.Pitch,
                             (int)desc.Width,
                             (int)desc.Height);

  sysmem->UnlockRect();

  const uint32_t expected_left = 0xFFFF0000u;  // BGRA = red.
  const uint32_t expected_right = 0xFF00FF00u; // BGRA = green.

  if ((left & 0x00FFFFFFu) != (expected_left & 0x00FFFFFFu) ||
      (right & 0x00FFFFFFu) != (expected_right & 0x00FFFFFFu)) {
    return reporter.Fail("pixel mismatch: left(%d,%d)=0x%08lX expected 0x%08lX; right(%d,%d)=0x%08lX expected 0x%08lX",
                         lx,
                         cy,
                         (unsigned long)left,
                         (unsigned long)expected_left,
                         rx,
                         cy,
                         (unsigned long)right,
                         (unsigned long)expected_right);
  }

  // Ensure the background stayed as the clear color.
  if ((corner & 0x00FFFFFFu) != (clear & 0x00FFFFFFu)) {
    return reporter.Fail("corner mismatch: got 0x%08lX expected clear 0x%08lX", (unsigned long)corner, (unsigned long)clear);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  const int rc = RunD3D9ExInstancingSanity(argc, argv);
  Sleep(30);
  return rc;
}
