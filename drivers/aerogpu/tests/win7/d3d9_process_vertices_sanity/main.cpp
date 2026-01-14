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

struct VertexXyzDiffuseTex1 {
  float x;
  float y;
  float z;
  DWORD color;
  float u;
  float v;
};

struct VertexXyzTex1 {
  float x;
  float y;
  float z;
  float u;
  float v;
};

struct VertexXyzrhwTex1 {
  float x;
  float y;
  float z;
  float rhw;
  float u;
  float v;
};

struct VertexXyzrhwDiffuseTex1 {
  float x;
  float y;
  float z;
  float rhw;
  DWORD color;
  float u;
  float v;
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
        "[--allow-microsoft] [--allow-non-aerogpu] [--require-umd] [--allow-remote]",
        kTestName);
    return 0;
  }

  aerogpu_test::TestReporter reporter(kTestName, argc, argv);

  const bool allow_microsoft = aerogpu_test::HasArg(argc, argv, "--allow-microsoft");
  const bool allow_non_aerogpu = aerogpu_test::HasArg(argc, argv, "--allow-non-aerogpu");
  const bool require_umd = aerogpu_test::HasArg(argc, argv, "--require-umd");
  const bool hidden = aerogpu_test::HasArg(argc, argv, "--hidden");
  const bool allow_remote = aerogpu_test::HasArg(argc, argv, "--allow-remote");

  if (GetSystemMetrics(SM_REMOTESESSION)) {
    if (allow_remote) {
      aerogpu_test::PrintfStdout("INFO: %s: remote session detected; skipping", kTestName);
      reporter.SetSkipped("remote_session");
      return reporter.Pass();
    }
    return reporter.Fail("running in a remote session (SM_REMOTESESSION=1). Re-run with --allow-remote to skip.");
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

  // Validate non-zero SrcStartIndex/DestIndex in the common XYZRHW passthrough case.
  {
    const Vertex src_off_verts[3] = {
        {1.0f, 2.0f, 0.0f, 1.0f, D3DCOLOR_XRGB(1, 2, 3)},
        {3.0f, 4.0f, 0.5f, 0.5f, D3DCOLOR_XRGB(4, 5, 6)},
        {5.0f, 6.0f, 1.0f, 2.0f, D3DCOLOR_XRGB(7, 8, 9)},
    };

    ComPtr<IDirect3DVertexBuffer9> src_off_vb;
    hr = dev->CreateVertexBuffer(sizeof(src_off_verts),
                                 0,
                                 D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                                 D3DPOOL_DEFAULT,
                                 src_off_vb.put(),
                                 NULL);
    if (FAILED(hr) || !src_off_vb) {
      return reporter.FailHresult("CreateVertexBuffer(src_off)", hr);
    }

    void* src_off_ptr = NULL;
    hr = src_off_vb->Lock(0, sizeof(src_off_verts), &src_off_ptr, 0);
    if (FAILED(hr) || !src_off_ptr) {
      return reporter.FailHresult("src_off_vb->Lock", hr);
    }
    memcpy(src_off_ptr, src_off_verts, sizeof(src_off_verts));
    hr = src_off_vb->Unlock();
    if (FAILED(hr)) {
      return reporter.FailHresult("src_off_vb->Unlock", hr);
    }

    ComPtr<IDirect3DVertexBuffer9> dst_off_vb;
    hr = dev->CreateVertexBuffer(sizeof(src_off_verts),
                                 0,
                                 D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                                 D3DPOOL_SYSTEMMEM,
                                 dst_off_vb.put(),
                                 NULL);
    if (FAILED(hr) || !dst_off_vb) {
      return reporter.FailHresult("CreateVertexBuffer(dst_off)", hr);
    }

    void* dst_off_ptr = NULL;
    hr = dst_off_vb->Lock(0, sizeof(src_off_verts), &dst_off_ptr, 0);
    if (FAILED(hr) || !dst_off_ptr) {
      return reporter.FailHresult("dst_off_vb->Lock", hr);
    }
    memset(dst_off_ptr, 0xCD, sizeof(src_off_verts));
    hr = dst_off_vb->Unlock();
    if (FAILED(hr)) {
      return reporter.FailHresult("dst_off_vb->Unlock", hr);
    }

    hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetFVF(XYZRHW|DIFFUSE) (offset case)", hr);
    }
    hr = dev->SetStreamSource(0, src_off_vb.get(), 0, sizeof(Vertex));
    if (FAILED(hr)) {
      return reporter.FailHresult("SetStreamSource(src_off)", hr);
    }

    hr = dev->ProcessVertices(/*SrcStartIndex=*/1,
                              /*DestIndex=*/2,
                              /*VertexCount=*/1,
                              dst_off_vb.get(),
                              decl.get(),
                              /*Flags=*/0);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9::ProcessVertices(offset)", hr);
    }

    hr = dst_off_vb->Lock(0, sizeof(src_off_verts), &dst_off_ptr, D3DLOCK_READONLY);
    if (FAILED(hr) || !dst_off_ptr) {
      return reporter.FailHresult("dst_off_vb->Lock (read)", hr);
    }

    const size_t stride = sizeof(Vertex);
    unsigned char expected_prefix[2 * sizeof(Vertex)];
    memset(expected_prefix, 0xCD, sizeof(expected_prefix));
    const bool prefix_ok = (memcmp(dst_off_ptr, expected_prefix, sizeof(expected_prefix)) == 0);
    const bool written_ok = (memcmp((const unsigned char*)dst_off_ptr + (2 * stride), &src_off_verts[1], stride) == 0);

    hr = dst_off_vb->Unlock();
    if (FAILED(hr)) {
      return reporter.FailHresult("dst_off_vb->Unlock (read)", hr);
    }

    if (!prefix_ok || !written_ok) {
      return reporter.Fail("ProcessVertices offset case mismatch (SrcStartIndex/DestIndex handling)");
    }
  }

  // Also validate a simple XYZ->XYZRHW fixed-function transform case.
  //
  // Use identity transforms and a tiny viewport so the expected output is
  // deterministic and exactly representable as IEEE floats.
  {
    // Use a non-default depth range to ensure ProcessVertices output Z remains in
    // normalized depth (NDC 0..1) rather than being pre-mapped to MinZ/MaxZ.
    const D3DVIEWPORT9 vp = {0u, 0u, 2u, 2u, 0.25f, 0.75f};
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

    const VertexXyzDiffuse srcs[2] = {
        {0.0f, 0.0f, 0.0f, D3DCOLOR_XRGB(1, 2, 3)},
        {0.0f, 0.0f, 0.0f, D3DCOLOR_XRGB(4, 5, 6)},
    };

    ComPtr<IDirect3DVertexBuffer9> src_xyz_vb;
    hr = dev->CreateVertexBuffer(sizeof(srcs),
                                 0,
                                 D3DFVF_XYZ | D3DFVF_DIFFUSE,
                                 D3DPOOL_DEFAULT,
                                 src_xyz_vb.put(),
                                 NULL);
    if (FAILED(hr) || !src_xyz_vb) {
      return reporter.FailHresult("CreateVertexBuffer(src_xyz)", hr);
    }

    void* src_xyz_ptr = NULL;
    hr = src_xyz_vb->Lock(0, sizeof(srcs), &src_xyz_ptr, 0);
    if (FAILED(hr) || !src_xyz_ptr) {
      return reporter.FailHresult("src_xyz_vb->Lock", hr);
    }
    memcpy(src_xyz_ptr, srcs, sizeof(srcs));
    hr = src_xyz_vb->Unlock();
    if (FAILED(hr)) {
      return reporter.FailHresult("src_xyz_vb->Unlock", hr);
    }

    ComPtr<IDirect3DVertexBuffer9> dst_xyz_vb;
    hr = dev->CreateVertexBuffer(sizeof(Vertex) * 2,
                                 0,
                                 D3DFVF_XYZRHW | D3DFVF_DIFFUSE,
                                 D3DPOOL_SYSTEMMEM,
                                 dst_xyz_vb.put(),
                                 NULL);
    if (FAILED(hr) || !dst_xyz_vb) {
      return reporter.FailHresult("CreateVertexBuffer(dst_xyz)", hr);
    }

    void* dst_xyz_init_ptr = NULL;
    hr = dst_xyz_vb->Lock(0, sizeof(Vertex) * 2, &dst_xyz_init_ptr, 0);
    if (FAILED(hr) || !dst_xyz_init_ptr) {
      return reporter.FailHresult("dst_xyz_vb->Lock (init)", hr);
    }
    memset(dst_xyz_init_ptr, 0xCD, sizeof(Vertex) * 2);
    hr = dst_xyz_vb->Unlock();
    if (FAILED(hr)) {
      return reporter.FailHresult("dst_xyz_vb->Unlock (init)", hr);
    }

    hr = dev->SetFVF(D3DFVF_XYZ | D3DFVF_DIFFUSE);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetFVF(XYZ|DIFFUSE)", hr);
    }
    hr = dev->SetStreamSource(0, src_xyz_vb.get(), 0, sizeof(VertexXyzDiffuse));
    if (FAILED(hr)) {
      return reporter.FailHresult("SetStreamSource(src_xyz)", hr);
    }

    hr = dev->ProcessVertices(/*SrcStartIndex=*/1,
                              /*DestIndex=*/1,
                              /*VertexCount=*/1,
                              dst_xyz_vb.get(),
                              decl.get(),
                              /*Flags=*/0);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9::ProcessVertices(xyz->xyzrhw)", hr);
    }

    void* dst_xyz_ptr = NULL;
    hr = dst_xyz_vb->Lock(0, sizeof(Vertex) * 2, &dst_xyz_ptr, D3DLOCK_READONLY);
    if (FAILED(hr) || !dst_xyz_ptr) {
      return reporter.FailHresult("dst_xyz_vb->Lock", hr);
    }

    const Vertex expected = {0.5f, 0.5f, 0.0f, 1.0f, srcs[1].color};
    unsigned char xyz_prefix_expected[sizeof(Vertex)];
    memset(xyz_prefix_expected, 0xCD, sizeof(xyz_prefix_expected));
    const bool xyz_prefix_ok = (memcmp(dst_xyz_ptr, xyz_prefix_expected, sizeof(xyz_prefix_expected)) == 0);
    const bool xyz_written_ok = (memcmp((const unsigned char*)dst_xyz_ptr + sizeof(Vertex), &expected, sizeof(expected)) == 0);

    hr = dst_xyz_vb->Unlock();
    if (FAILED(hr)) {
      return reporter.FailHresult("dst_xyz_vb->Unlock", hr);
    }

    if (!xyz_prefix_ok || !xyz_written_ok) {
      return reporter.Fail("ProcessVertices XYZ->XYZRHW output bytes did not match expected output");
    }
  }

  // Validate TEX1 variant: XYZ|DIFFUSE|TEX1 -> XYZRHW|DIFFUSE|TEX1.
  {
    const VertexXyzDiffuseTex1 src = {0.0f, 0.0f, 0.0f, D3DCOLOR_XRGB(10, 20, 30), 0.25f, 0.75f};

    ComPtr<IDirect3DVertexBuffer9> src_vb_tex;
    hr = dev->CreateVertexBuffer(sizeof(src),
                                 0,
                                 D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1,
                                 D3DPOOL_DEFAULT,
                                 src_vb_tex.put(),
                                 NULL);
    if (FAILED(hr) || !src_vb_tex) {
      return reporter.FailHresult("CreateVertexBuffer(src_tex)", hr);
    }

    void* src_tex_ptr = NULL;
    hr = src_vb_tex->Lock(0, sizeof(src), &src_tex_ptr, 0);
    if (FAILED(hr) || !src_tex_ptr) {
      return reporter.FailHresult("src_vb_tex->Lock", hr);
    }
    memcpy(src_tex_ptr, &src, sizeof(src));
    hr = src_vb_tex->Unlock();
    if (FAILED(hr)) {
      return reporter.FailHresult("src_vb_tex->Unlock", hr);
    }

    ComPtr<IDirect3DVertexBuffer9> dst_vb_tex;
    hr = dev->CreateVertexBuffer(sizeof(VertexXyzrhwDiffuseTex1),
                                 0,
                                 D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1,
                                 D3DPOOL_SYSTEMMEM,
                                 dst_vb_tex.put(),
                                 NULL);
    if (FAILED(hr) || !dst_vb_tex) {
      return reporter.FailHresult("CreateVertexBuffer(dst_tex)", hr);
    }

    // Output declaration: POSITIONT(float4) + COLOR0(D3DCOLOR) + TEXCOORD0(float2).
    const D3DVERTEXELEMENT9 decl_tex_elems[] = {
        {0, 0, D3DDECLTYPE_FLOAT4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITIONT, 0},
        {0, 16, D3DDECLTYPE_D3DCOLOR, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR, 0},
        {0, 20, D3DDECLTYPE_FLOAT2, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0},
        D3DDECL_END(),
    };
    ComPtr<IDirect3DVertexDeclaration9> decl_tex;
    hr = dev->CreateVertexDeclaration(decl_tex_elems, decl_tex.put());
    if (FAILED(hr) || !decl_tex) {
      return reporter.FailHresult("CreateVertexDeclaration(tex)", hr);
    }

    hr = dev->SetFVF(D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1);
    if (FAILED(hr)) {
      return reporter.FailHresult("SetFVF(XYZ|DIFFUSE|TEX1)", hr);
    }
    hr = dev->SetStreamSource(0, src_vb_tex.get(), 0, sizeof(VertexXyzDiffuseTex1));
    if (FAILED(hr)) {
      return reporter.FailHresult("SetStreamSource(src_tex)", hr);
    }

    hr = dev->ProcessVertices(/*SrcStartIndex=*/0,
                              /*DestIndex=*/0,
                              /*VertexCount=*/1,
                              dst_vb_tex.get(),
                              decl_tex.get(),
                              /*Flags=*/0);
    if (FAILED(hr)) {
      return reporter.FailHresult("IDirect3DDevice9::ProcessVertices(xyz|tex1)", hr);
    }

    void* dst_tex_ptr = NULL;
    hr = dst_vb_tex->Lock(0, sizeof(VertexXyzrhwDiffuseTex1), &dst_tex_ptr, D3DLOCK_READONLY);
    if (FAILED(hr) || !dst_tex_ptr) {
      return reporter.FailHresult("dst_vb_tex->Lock", hr);
    }

    const VertexXyzrhwDiffuseTex1 expected = {0.5f, 0.5f, 0.0f, 1.0f, src.color, src.u, src.v};
    const bool tex_bytes_match = (memcmp(dst_tex_ptr, &expected, sizeof(expected)) == 0);

    hr = dst_vb_tex->Unlock();
    if (FAILED(hr)) {
      return reporter.FailHresult("dst_vb_tex->Unlock", hr);
    }

    if (!tex_bytes_match) {
      return reporter.Fail("ProcessVertices XYZ|TEX1 output bytes did not match expected output");
    }

    // Validate XYZ|TEX1 -> XYZRHW|DIFFUSE|TEX1 when the source vertex format does
    // not include a DIFFUSE color. Fixed-function behavior should treat DIFFUSE
    // as white.
    {
      const VertexXyzTex1 src_white = {0.0f, 0.0f, 0.0f, 0.25f, 0.75f};

      ComPtr<IDirect3DVertexBuffer9> src_white_vb;
      hr = dev->CreateVertexBuffer(sizeof(src_white),
                                   0,
                                   D3DFVF_XYZ | D3DFVF_TEX1,
                                   D3DPOOL_DEFAULT,
                                   src_white_vb.put(),
                                   NULL);
      if (FAILED(hr) || !src_white_vb) {
        return reporter.FailHresult("CreateVertexBuffer(src_white)", hr);
      }

      void* src_white_ptr = NULL;
      hr = src_white_vb->Lock(0, sizeof(src_white), &src_white_ptr, 0);
      if (FAILED(hr) || !src_white_ptr) {
        return reporter.FailHresult("src_white_vb->Lock", hr);
      }
      memcpy(src_white_ptr, &src_white, sizeof(src_white));
      hr = src_white_vb->Unlock();
      if (FAILED(hr)) {
        return reporter.FailHresult("src_white_vb->Unlock", hr);
      }

      ComPtr<IDirect3DVertexBuffer9> dst_white_vb;
      hr = dev->CreateVertexBuffer(sizeof(VertexXyzrhwDiffuseTex1),
                                   0,
                                   D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1,
                                   D3DPOOL_SYSTEMMEM,
                                   dst_white_vb.put(),
                                   NULL);
      if (FAILED(hr) || !dst_white_vb) {
        return reporter.FailHresult("CreateVertexBuffer(dst_white)", hr);
      }

      void* dst_white_init_ptr = NULL;
      hr = dst_white_vb->Lock(0, sizeof(VertexXyzrhwDiffuseTex1), &dst_white_init_ptr, 0);
      if (FAILED(hr) || !dst_white_init_ptr) {
        return reporter.FailHresult("dst_white_vb->Lock (init)", hr);
      }
      memset(dst_white_init_ptr, 0xCD, sizeof(VertexXyzrhwDiffuseTex1));
      hr = dst_white_vb->Unlock();
      if (FAILED(hr)) {
        return reporter.FailHresult("dst_white_vb->Unlock (init)", hr);
      }

      hr = dev->SetFVF(D3DFVF_XYZ | D3DFVF_TEX1);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetFVF(XYZ|TEX1)", hr);
      }
      hr = dev->SetStreamSource(0, src_white_vb.get(), 0, sizeof(VertexXyzTex1));
      if (FAILED(hr)) {
        return reporter.FailHresult("SetStreamSource(src_white)", hr);
      }

      hr = dev->ProcessVertices(/*SrcStartIndex=*/0,
                                /*DestIndex=*/0,
                                /*VertexCount=*/1,
                                dst_white_vb.get(),
                                decl_tex.get(),
                                /*Flags=*/0);
      if (FAILED(hr)) {
        return reporter.FailHresult("IDirect3DDevice9::ProcessVertices(xyz|tex1 white)", hr);
      }

      void* dst_white_ptr = NULL;
      hr = dst_white_vb->Lock(0, sizeof(VertexXyzrhwDiffuseTex1), &dst_white_ptr, D3DLOCK_READONLY);
      if (FAILED(hr) || !dst_white_ptr) {
        return reporter.FailHresult("dst_white_vb->Lock (read)", hr);
      }

      const VertexXyzrhwDiffuseTex1 expected_white = {0.5f,
                                                      0.5f,
                                                      0.0f,
                                                      1.0f,
                                                      D3DCOLOR_XRGB(255, 255, 255),
                                                      src_white.u,
                                                      src_white.v};
      const bool white_bytes_match = (memcmp(dst_white_ptr, &expected_white, sizeof(expected_white)) == 0);

      hr = dst_white_vb->Unlock();
      if (FAILED(hr)) {
        return reporter.FailHresult("dst_white_vb->Unlock (read)", hr);
      }

      if (!white_bytes_match) {
        return reporter.Fail("ProcessVertices XYZ|TEX1 missing-diffuse case did not match expected output");
      }
    }

    // Validate XYZRHW|TEX1 -> XYZRHW|DIFFUSE|TEX1 when the source vertex format
    // does not include a DIFFUSE color. Fixed-function behavior should treat
    // DIFFUSE as white and pass through POSITIONT + TEX0.
    {
      const VertexXyzrhwTex1 src_white = {10.0f, 20.0f, 0.5f, 2.0f, 0.25f, 0.75f};

      ComPtr<IDirect3DVertexBuffer9> src_white_vb;
      hr = dev->CreateVertexBuffer(sizeof(src_white),
                                   0,
                                   D3DFVF_XYZRHW | D3DFVF_TEX1,
                                   D3DPOOL_DEFAULT,
                                   src_white_vb.put(),
                                   NULL);
      if (FAILED(hr) || !src_white_vb) {
        return reporter.FailHresult("CreateVertexBuffer(src_xyzw_tex1_white)", hr);
      }

      void* src_white_ptr = NULL;
      hr = src_white_vb->Lock(0, sizeof(src_white), &src_white_ptr, 0);
      if (FAILED(hr) || !src_white_ptr) {
        return reporter.FailHresult("src_xyzw_tex1_white->Lock", hr);
      }
      memcpy(src_white_ptr, &src_white, sizeof(src_white));
      hr = src_white_vb->Unlock();
      if (FAILED(hr)) {
        return reporter.FailHresult("src_xyzw_tex1_white->Unlock", hr);
      }

      ComPtr<IDirect3DVertexBuffer9> dst_white_vb;
      hr = dev->CreateVertexBuffer(sizeof(VertexXyzrhwDiffuseTex1),
                                   0,
                                   D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1,
                                   D3DPOOL_SYSTEMMEM,
                                   dst_white_vb.put(),
                                   NULL);
      if (FAILED(hr) || !dst_white_vb) {
        return reporter.FailHresult("CreateVertexBuffer(dst_xyzw_tex1_white)", hr);
      }

      void* dst_white_init_ptr = NULL;
      hr = dst_white_vb->Lock(0, sizeof(VertexXyzrhwDiffuseTex1), &dst_white_init_ptr, 0);
      if (FAILED(hr) || !dst_white_init_ptr) {
        return reporter.FailHresult("dst_xyzw_tex1_white->Lock (init)", hr);
      }
      memset(dst_white_init_ptr, 0xCD, sizeof(VertexXyzrhwDiffuseTex1));
      hr = dst_white_vb->Unlock();
      if (FAILED(hr)) {
        return reporter.FailHresult("dst_xyzw_tex1_white->Unlock (init)", hr);
      }

      hr = dev->SetFVF(D3DFVF_XYZRHW | D3DFVF_TEX1);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetFVF(XYZRHW|TEX1)", hr);
      }
      hr = dev->SetStreamSource(0, src_white_vb.get(), 0, sizeof(VertexXyzrhwTex1));
      if (FAILED(hr)) {
        return reporter.FailHresult("SetStreamSource(src_xyzw_tex1_white)", hr);
      }

      hr = dev->ProcessVertices(/*SrcStartIndex=*/0,
                                /*DestIndex=*/0,
                                /*VertexCount=*/1,
                                dst_white_vb.get(),
                                decl_tex.get(),
                                /*Flags=*/0);
      if (FAILED(hr)) {
        return reporter.FailHresult("IDirect3DDevice9::ProcessVertices(xyzw|tex1 white)", hr);
      }

      void* dst_white_ptr = NULL;
      hr = dst_white_vb->Lock(0, sizeof(VertexXyzrhwDiffuseTex1), &dst_white_ptr, D3DLOCK_READONLY);
      if (FAILED(hr) || !dst_white_ptr) {
        return reporter.FailHresult("dst_xyzw_tex1_white->Lock (read)", hr);
      }

      const VertexXyzrhwDiffuseTex1 expected_white = {src_white.x,
                                                      src_white.y,
                                                      src_white.z,
                                                      src_white.rhw,
                                                      D3DCOLOR_XRGB(255, 255, 255),
                                                      src_white.u,
                                                      src_white.v};
      const bool white_bytes_match = (memcmp(dst_white_ptr, &expected_white, sizeof(expected_white)) == 0);

      hr = dst_white_vb->Unlock();
      if (FAILED(hr)) {
        return reporter.FailHresult("dst_xyzw_tex1_white->Unlock (read)", hr);
      }

      if (!white_bytes_match) {
        return reporter.Fail("ProcessVertices XYZRHW|TEX1 missing-diffuse case did not match expected output");
      }
    }

    // Validate Flags=D3DPV_DONOTCOPYDATA: output should update only POSITIONT and
    // leave non-position bytes (DIFFUSE/TEX0) untouched.
    {
      ComPtr<IDirect3DVertexBuffer9> dst_flags_vb;
      hr = dev->CreateVertexBuffer(sizeof(VertexXyzrhwDiffuseTex1),
                                   0,
                                   D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1,
                                   D3DPOOL_SYSTEMMEM,
                                   dst_flags_vb.put(),
                                   NULL);
      if (FAILED(hr) || !dst_flags_vb) {
        return reporter.FailHresult("CreateVertexBuffer(dst_flags)", hr);
      }

      void* dst_flags_init_ptr = NULL;
      hr = dst_flags_vb->Lock(0, sizeof(VertexXyzrhwDiffuseTex1), &dst_flags_init_ptr, 0);
      if (FAILED(hr) || !dst_flags_init_ptr) {
        return reporter.FailHresult("dst_flags_vb->Lock (init)", hr);
      }
      memset(dst_flags_init_ptr, 0xCD, sizeof(VertexXyzrhwDiffuseTex1));
      hr = dst_flags_vb->Unlock();
      if (FAILED(hr)) {
        return reporter.FailHresult("dst_flags_vb->Unlock (init)", hr);
      }

      hr = dev->SetFVF(D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetFVF(XYZ|DIFFUSE|TEX1) (flags)", hr);
      }
      hr = dev->SetStreamSource(0, src_vb_tex.get(), 0, sizeof(VertexXyzDiffuseTex1));
      if (FAILED(hr)) {
        return reporter.FailHresult("SetStreamSource(src_tex) (flags)", hr);
      }

      hr = dev->ProcessVertices(/*SrcStartIndex=*/0,
                                /*DestIndex=*/0,
                                /*VertexCount=*/1,
                                dst_flags_vb.get(),
                                decl_tex.get(),
                                /*Flags=*/D3DPV_DONOTCOPYDATA);
      if (FAILED(hr)) {
        return reporter.FailHresult("IDirect3DDevice9::ProcessVertices(flags)", hr);
      }

      void* dst_flags_ptr = NULL;
      hr = dst_flags_vb->Lock(0, sizeof(VertexXyzrhwDiffuseTex1), &dst_flags_ptr, D3DLOCK_READONLY);
      if (FAILED(hr) || !dst_flags_ptr) {
        return reporter.FailHresult("dst_flags_vb->Lock (read)", hr);
      }

      const VertexXyzrhwDiffuseTex1 expected_pos = {0.5f, 0.5f, 0.0f, 1.0f, 0, 0, 0};
      const bool pos_ok = (memcmp(dst_flags_ptr, &expected_pos, 16) == 0);
      unsigned char tail_expected[sizeof(VertexXyzrhwDiffuseTex1) - 16];
      memset(tail_expected, 0xCD, sizeof(tail_expected));
      const bool tail_ok = (memcmp((const unsigned char*)dst_flags_ptr + 16, tail_expected, sizeof(tail_expected)) == 0);

      hr = dst_flags_vb->Unlock();
      if (FAILED(hr)) {
        return reporter.FailHresult("dst_flags_vb->Unlock (read)", hr);
      }

      if (!pos_ok || !tail_ok) {
        return reporter.Fail("ProcessVertices Flags=D3DPV_DONOTCOPYDATA mismatch (non-position bytes were modified)");
      }
    }

    // Also validate SrcStartIndex/DestIndex offsets for the TEX1 path.
    {
      const VertexXyzDiffuseTex1 srcs[2] = {
          {0.0f, 0.0f, 0.0f, D3DCOLOR_XRGB(1, 2, 3), 0.10f, 0.20f},
          {2.0f, 0.0f, 0.0f, D3DCOLOR_XRGB(4, 5, 6), 0.30f, 0.40f},
      };

      ComPtr<IDirect3DVertexBuffer9> src_off;
      hr = dev->CreateVertexBuffer(sizeof(srcs),
                                   0,
                                   D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1,
                                   D3DPOOL_DEFAULT,
                                   src_off.put(),
                                   NULL);
      if (FAILED(hr) || !src_off) {
        return reporter.FailHresult("CreateVertexBuffer(src_tex_off)", hr);
      }

      void* src_off_ptr = NULL;
      hr = src_off->Lock(0, sizeof(srcs), &src_off_ptr, 0);
      if (FAILED(hr) || !src_off_ptr) {
        return reporter.FailHresult("src_tex_off->Lock", hr);
      }
      memcpy(src_off_ptr, srcs, sizeof(srcs));
      hr = src_off->Unlock();
      if (FAILED(hr)) {
        return reporter.FailHresult("src_tex_off->Unlock", hr);
      }

      // Destination: 3 vertices so we can write into DestIndex=2.
      ComPtr<IDirect3DVertexBuffer9> dst_off;
      hr = dev->CreateVertexBuffer(sizeof(VertexXyzrhwDiffuseTex1) * 3,
                                   0,
                                   D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_TEX1,
                                   D3DPOOL_SYSTEMMEM,
                                   dst_off.put(),
                                   NULL);
      if (FAILED(hr) || !dst_off) {
        return reporter.FailHresult("CreateVertexBuffer(dst_tex_off)", hr);
      }

      void* dst_off_ptr = NULL;
      hr = dst_off->Lock(0, sizeof(VertexXyzrhwDiffuseTex1) * 3, &dst_off_ptr, 0);
      if (FAILED(hr) || !dst_off_ptr) {
        return reporter.FailHresult("dst_tex_off->Lock (init)", hr);
      }
      memset(dst_off_ptr, 0xCD, sizeof(VertexXyzrhwDiffuseTex1) * 3);
      hr = dst_off->Unlock();
      if (FAILED(hr)) {
        return reporter.FailHresult("dst_tex_off->Unlock (init)", hr);
      }

      hr = dev->SetFVF(D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1);
      if (FAILED(hr)) {
        return reporter.FailHresult("SetFVF(XYZ|DIFFUSE|TEX1) (offset case)", hr);
      }
      hr = dev->SetStreamSource(0, src_off.get(), 0, sizeof(VertexXyzDiffuseTex1));
      if (FAILED(hr)) {
        return reporter.FailHresult("SetStreamSource(src_tex_off)", hr);
      }

      hr = dev->ProcessVertices(/*SrcStartIndex=*/1,
                                /*DestIndex=*/2,
                                /*VertexCount=*/1,
                                dst_off.get(),
                                decl_tex.get(),
                                /*Flags=*/0);
      if (FAILED(hr)) {
        return reporter.FailHresult("IDirect3DDevice9::ProcessVertices(tex1 offset)", hr);
      }

      hr = dst_off->Lock(0, sizeof(VertexXyzrhwDiffuseTex1) * 3, &dst_off_ptr, D3DLOCK_READONLY);
      if (FAILED(hr) || !dst_off_ptr) {
        return reporter.FailHresult("dst_tex_off->Lock (read)", hr);
      }

      unsigned char prefix_expected[sizeof(VertexXyzrhwDiffuseTex1) * 2];
      memset(prefix_expected, 0xCD, sizeof(prefix_expected));
      const bool prefix_ok = (memcmp(dst_off_ptr, prefix_expected, sizeof(prefix_expected)) == 0);

      const VertexXyzrhwDiffuseTex1 expected_off = {2.5f, 0.5f, 0.0f, 1.0f, srcs[1].color, srcs[1].u, srcs[1].v};
      const bool written_ok =
          (memcmp((const unsigned char*)dst_off_ptr + sizeof(VertexXyzrhwDiffuseTex1) * 2,
                  &expected_off,
                  sizeof(expected_off)) == 0);

      hr = dst_off->Unlock();
      if (FAILED(hr)) {
        return reporter.FailHresult("dst_tex_off->Unlock (read)", hr);
      }

      if (!prefix_ok || !written_ok) {
        return reporter.Fail("ProcessVertices TEX1 offset case mismatch (SrcStartIndex/DestIndex handling)");
      }
    }
  }

  aerogpu_test::PrintfStdout("INFO: %s: ProcessVertices OK", kTestName);
  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9ProcessVerticesSanity(argc, argv);
}
