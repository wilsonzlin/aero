#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

struct Vertex {
  float x;
  float y;
  float z;
  float rhw;
  DWORD color;
};

struct VertexXyzDiffuse {
  float x;
  float y;
  float z;
  DWORD color;
};

static HRESULT CreateDeviceExWithFallback(IDirect3D9Ex* d3d,
                                          HWND hwnd,
                                          D3DPRESENT_PARAMETERS* pp,
                                          DWORD create_flags,
                                          IDirect3DDevice9Ex** out_dev) {
  if (!d3d || !pp || !out_dev) {
    return E_INVALIDARG;
  }

  HRESULT hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                                   D3DDEVTYPE_HAL,
                                   hwnd,
                                   create_flags,
                                   pp,
                                   NULL,
                                   out_dev);
  if (FAILED(hr)) {
    DWORD fallback_flags = create_flags;
    fallback_flags &= ~D3DCREATE_HARDWARE_VERTEXPROCESSING;
    fallback_flags |= D3DCREATE_SOFTWARE_VERTEXPROCESSING;
    hr = d3d->CreateDeviceEx(D3DADAPTER_DEFAULT,
                             D3DDEVTYPE_HAL,
                             hwnd,
                             fallback_flags,
                             pp,
                             NULL,
                             out_dev);
  }
  return hr;
}

static int RunD3D9ProcessVerticesSanity(int argc, char** argv) {
  const char* kTestName = "d3d9_process_vertices_sanity";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--hidden] [--json[=PATH]] [--require-vid=0x####] [--require-did=0x####] "
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

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
  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ProcessVerticesSanity",
                                              L"AeroGPU D3D9 ProcessVertices Sanity",
                                              kWidth,
                                              kHeight,
                                              !hidden);
  if (!hwnd) {
    return reporter.Fail("CreateBasicWindow failed");
  }

  ComPtr<IDirect3D9Ex> d3d;
  HRESULT hr = Direct3DCreate9Ex(D3D_SDK_VERSION, d3d.put());
  if (FAILED(hr) || !d3d) {
    return reporter.FailHresult("Direct3DCreate9Ex", hr);
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
  DWORD create_flags = D3DCREATE_HARDWARE_VERTEXPROCESSING | D3DCREATE_NOWINDOWCHANGES;
  hr = CreateDeviceExWithFallback(d3d.get(), hwnd, &pp, create_flags, dev.put());
  if (FAILED(hr) || !dev) {
    return reporter.FailHresult("IDirect3D9Ex::CreateDeviceEx", hr);
  }

  // Basic adapter sanity check to avoid false PASS when AeroGPU isn't active.
  {
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
  }

  if (require_umd || (!allow_microsoft && !allow_non_aerogpu)) {
    int umd_rc = aerogpu_test::RequireAeroGpuD3D9UmdLoaded(&reporter, kTestName);
    if (umd_rc != 0) {
      return umd_rc;
    }
  }

  // Build a small vertex buffer and process it into another buffer.
  const Vertex src_verts[2] = {
      {10.0f, 20.0f, 0.25f, 1.0f, D3DCOLOR_XRGB(255, 0, 0)},
      {30.0f, 40.0f, 0.75f, 0.5f, D3DCOLOR_XRGB(0, 255, 0)},
  };

  ComPtr<IDirect3DVertexBuffer9> src_vb;
  hr = dev->CreateVertexBuffer(sizeof(src_verts),
                               0,
                               D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                               D3DPOOL_DEFAULT,
                               src_vb.put(),
                               NULL);
  if (FAILED(hr) || !src_vb) {
    return reporter.FailHresult("CreateVertexBuffer(src)", hr);
  }

  void* src_ptr = NULL;
  hr = src_vb->Lock(0, sizeof(src_verts), &src_ptr, 0);
  if (FAILED(hr) || !src_ptr) {
    return reporter.FailHresult("src_vb->Lock", hr);
  }
  memcpy(src_ptr, src_verts, sizeof(src_verts));
  hr = src_vb->Unlock();
  if (FAILED(hr)) {
    return reporter.FailHresult("src_vb->Unlock", hr);
  }

  // Use a system-memory destination buffer so we can validate bytes deterministically.
  ComPtr<IDirect3DVertexBuffer9> dst_vb;
  hr = dev->CreateVertexBuffer(sizeof(src_verts),
                               0,
                               D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                               D3DPOOL_SYSTEMMEM,
                               dst_vb.put(),
                               NULL);
  if (FAILED(hr) || !dst_vb) {
    return reporter.FailHresult("CreateVertexBuffer(dst)", hr);
  }

  // Output declaration: POSITIONT(float4) + COLOR0(D3DCOLOR).
  const D3DVERTEXELEMENT9 decl_elems[] = {
      {0, 0, D3DDECLTYPE_FLOAT4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITIONT, 0},
      {0, 16, D3DDECLTYPE_D3DCOLOR, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR, 0},
      D3DDECL_END(),
  };
  ComPtr<IDirect3DVertexDeclaration9> decl;
  hr = dev->CreateVertexDeclaration(decl_elems, decl.put());
  if (FAILED(hr) || !decl) {
    return reporter.FailHresult("CreateVertexDeclaration", hr);
  }

  hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetFVF", hr);
  }
  hr = dev->SetStreamSource(0, src_vb.get(), 0, sizeof(Vertex));
  if (FAILED(hr)) {
    return reporter.FailHresult("SetStreamSource", hr);
  }

  hr = dev->ProcessVertices(/*SrcStartIndex=*/0,
                            /*DestIndex=*/0,
                            /*VertexCount=*/2,
                            dst_vb.get(),
                            decl.get(),
                            /*Flags=*/0);
  if (FAILED(hr)) {
    return reporter.FailHresult("IDirect3DDevice9::ProcessVertices", hr);
  }

  void* dst_ptr = NULL;
  hr = dst_vb->Lock(0, sizeof(src_verts), &dst_ptr, D3DLOCK_READONLY);
  if (FAILED(hr) || !dst_ptr) {
    return reporter.FailHresult("dst_vb->Lock", hr);
  }

  const bool bytes_match = (memcmp(dst_ptr, src_verts, sizeof(src_verts)) == 0);
  hr = dst_vb->Unlock();
  if (FAILED(hr)) {
    return reporter.FailHresult("dst_vb->Unlock", hr);
  }

  if (!bytes_match) {
    return reporter.Fail("ProcessVertices output bytes did not match expected output");
  }

  // Also validate a simple XYZ->XYZRHW fixed-function transform case.
  //
  // Use identity transforms and a tiny viewport so the expected output is
  // deterministic and exactly representable as IEEE floats.
  {
    const D3DVIEWPORT9 vp = {0u, 0u, 2u, 2u, 0.0f, 1.0f};
    hr = dev->SetViewport(&vp);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetViewport", hr);
    }

    D3DMATRIX ident;
    ZeroMemory(&ident, sizeof(ident));
    ident.m[0][0] = 1.0f;
    ident.m[1][1] = 1.0f;
    ident.m[2][2] = 1.0f;
    ident.m[3][3] = 1.0f;
    hr = dev->SetTransform(D3DTS_WORLD, &ident);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTransform(WORLD)", hr);
    }
    hr = dev->SetTransform(D3DTS_VIEW, &ident);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTransform(VIEW)", hr);
    }
    hr = dev->SetTransform(D3DTS_PROJECTION, &ident);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetTransform(PROJECTION)", hr);
    }

    const VertexXyzDiffuse src = {0.0f, 0.0f, 0.5f, D3DCOLOR_XRGB(1, 2, 3)};

    ComPtr<IDirect3DVertexBuffer9> src_xyz_vb;
    hr = dev->CreateVertexBuffer(sizeof(src),
                                 0,
                                 D3DFVF_XYZ | D3DFVF_DIFFUSE,
                                 D3DPOOL_DEFAULT,
                                 src_xyz_vb.put(),
                                 NULL);
    if (FAILED(hr) || !src_xyz_vb) {
      return reporter.FailHresult("CreateVertexBuffer(src_xyz)", hr);
    }

    void* src_xyz_ptr = NULL;
    hr = src_xyz_vb->Lock(0, sizeof(src), &src_xyz_ptr, 0);
    if (FAILED(hr) || !src_xyz_ptr) {
      return reporter.FailHresult("src_xyz_vb->Lock", hr);
    }
    memcpy(src_xyz_ptr, &src, sizeof(src));
    hr = src_xyz_vb->Unlock();
    if (FAILED(hr)) {
      return reporter.FailHresult("src_xyz_vb->Unlock", hr);
    }

    ComPtr<IDirect3DVertexBuffer9> dst_xyz_vb;
    hr = dev->CreateVertexBuffer(sizeof(Vertex),
                                 0,
                                 D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                                 D3DPOOL_SYSTEMMEM,
                                 dst_xyz_vb.put(),
                                 NULL);
    if (FAILED(hr) || !dst_xyz_vb) {
      return reporter.FailHresult("CreateVertexBuffer(dst_xyz)", hr);
    }

    hr = dev->SetFVF(D3DFVF_XYZ | D3DFVF_DIFFUSE);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetFVF(XYZ|DIFFUSE)", hr);
    }
    hr = dev->SetStreamSource(0, src_xyz_vb.get(), 0, sizeof(VertexXyzDiffuse));
    if (FAILED(hr)) {
      return reporter.FailHresult("SetStreamSource(src_xyz)", hr);
    }

    hr = dev->ProcessVertices(/*SrcStartIndex=*/0,
                              /*DestIndex=*/0,
                              /*VertexCount=*/1,
                              dst_xyz_vb.get(),
                              decl.get(),
                              /*Flags=*/0);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9::ProcessVertices(xyz->xyzrhw)", hr);
    }

    void* dst_xyz_ptr = NULL;
    hr = dst_xyz_vb->Lock(0, sizeof(Vertex), &dst_xyz_ptr, D3DLOCK_READONLY);
    if (FAILED(hr) || !dst_xyz_ptr) {
      return reporter.FailHresult("dst_xyz_vb->Lock", hr);
    }

    const Vertex expected = {0.5f, 0.5f, 0.5f, 1.0f, src.color};
    const bool xyz_bytes_match = (memcmp(dst_xyz_ptr, &expected, sizeof(expected)) == 0);

    hr = dst_xyz_vb->Unlock();
    if (FAILED(hr)) {
      return reporter.FailHresult("dst_xyz_vb->Unlock", hr);
    }

    if (!xyz_bytes_match) {
      return reporter.Fail("ProcessVertices XYZ->XYZRHW output bytes did not match expected output");
    }
  }

  aerogpu_test::PrintfStdout("INFO: %s: ProcessVertices OK", kTestName);
  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9ProcessVerticesSanity(argc, argv);
}
