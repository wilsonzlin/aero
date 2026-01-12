#include "..\\common\\aerogpu_test_common.h"
#include "..\\common\\aerogpu_test_report.h"

#include <d3d9.h>
#include <cstring>

using aerogpu_test::ComPtr;

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

static bool NearlyEqual(float a, float b, float eps) {
  float d = a - b;
  if (d < 0.0f) {
    d = -d;
  }
  return d <= eps;
}

static bool MatrixNearlyEqual(const D3DMATRIX& a, const D3DMATRIX& b, float eps) {
  const float* pa = reinterpret_cast<const float*>(&a);
  const float* pb = reinterpret_cast<const float*>(&b);
  for (int i = 0; i < 16; ++i) {
    if (!NearlyEqual(pa[i], pb[i], eps)) {
      return false;
    }
  }
  return true;
}

static D3DMATRIX MakeTestMatrix(float base) {
  D3DMATRIX m;
  ZeroMemory(&m, sizeof(m));

  m._11 = 1.0f + base;
  m._12 = 0.1f + base;
  m._13 = 0.2f + base;
  m._14 = 0.3f + base;

  m._21 = 0.4f + base;
  m._22 = 1.5f + base;
  m._23 = 0.6f + base;
  m._24 = 0.7f + base;

  m._31 = 0.8f + base;
  m._32 = 0.9f + base;
  m._33 = 2.0f + base;
  m._34 = 1.1f + base;

  m._41 = 3.0f + base;
  m._42 = 4.0f + base;
  m._43 = 5.0f + base;
  m._44 = 1.0f;

  return m;
}

static int RunD3D9ExFixedFuncStateRoundtrip(int argc, char** argv) {
  const char* kTestName = "d3d9ex_fixedfunc_state_roundtrip";
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

  const int kWidth = 64;
  const int kHeight = 64;

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExFixedFuncStateRoundtrip",
                                              L"AeroGPU D3D9Ex FixedFunc State Roundtrip",
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
    return reporter.FailHresult("IDirect3D9Ex::CreateDeviceEx", hr);
  }

  // --- Transform roundtrip ---
  const D3DMATRIX m_a = MakeTestMatrix(0.0f);
  hr = dev->SetTransform(D3DTS_WORLD, &m_a);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTransform(D3DTS_WORLD)", hr);
  }

  D3DMATRIX got_m_a;
  ZeroMemory(&got_m_a, sizeof(got_m_a));
  hr = dev->GetTransform(D3DTS_WORLD, &got_m_a);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetTransform(D3DTS_WORLD)", hr);
  }

  if (!MatrixNearlyEqual(got_m_a, m_a, 1e-6f)) {
    return reporter.Fail("GetTransform mismatch after SetTransform");
  }

  // --- Texture stage state roundtrip ---
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_ADD);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(stage0, COLOROP)", hr);
  }
  DWORD got_tss = 0;
  hr = dev->GetTextureStageState(0, D3DTSS_COLOROP, &got_tss);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetTextureStageState(stage0, COLOROP)", hr);
  }
  if (got_tss != (DWORD)D3DTOP_ADD) {
    return reporter.Fail("GetTextureStageState(stage0, COLOROP) mismatch: got=%lu expected=%lu",
                         (unsigned long)got_tss,
                         (unsigned long)D3DTOP_ADD);
  }

  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SUBTRACT);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(stage0, ALPHAOP)", hr);
  }
  got_tss = 0;
  hr = dev->GetTextureStageState(0, D3DTSS_ALPHAOP, &got_tss);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetTextureStageState(stage0, ALPHAOP)", hr);
  }
  if (got_tss != (DWORD)D3DTOP_SUBTRACT) {
    return reporter.Fail("GetTextureStageState(stage0, ALPHAOP) mismatch: got=%lu expected=%lu",
                         (unsigned long)got_tss,
                         (unsigned long)D3DTOP_SUBTRACT);
  }

  // --- StateBlock restore for fixed-function cached state ---
  const D3DMATRIX m_base = MakeTestMatrix(1.0f);
  hr = dev->SetTransform(D3DTS_WORLD, &m_base);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTransform(base)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_MODULATE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(base COLOROP)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(base ALPHAOP)", hr);
  }

  ComPtr<IDirect3DStateBlock9> sb;
  hr = dev->BeginStateBlock();
  if (FAILED(hr)) {
    return reporter.FailHresult("BeginStateBlock", hr);
  }

  const D3DMATRIX m_record = MakeTestMatrix(2.0f);
  const DWORD tss_record_colorop = (DWORD)D3DTOP_SUBTRACT;
  const DWORD tss_record_alphaop = (DWORD)D3DTOP_ADD;

  hr = dev->SetTransform(D3DTS_WORLD, &m_record);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTransform(record)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, tss_record_colorop);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(record COLOROP)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAOP, tss_record_alphaop);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(record ALPHAOP)", hr);
  }

  hr = dev->EndStateBlock(sb.put());
  if (FAILED(hr) || !sb) {
    return reporter.FailHresult("EndStateBlock", FAILED(hr) ? hr : E_FAIL);
  }

  // Mutate away again.
  const D3DMATRIX m_mutate = MakeTestMatrix(3.0f);
  hr = dev->SetTransform(D3DTS_WORLD, &m_mutate);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTransform(mutate)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_COLOROP, D3DTOP_DISABLE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(mutate COLOROP)", hr);
  }
  hr = dev->SetTextureStageState(0, D3DTSS_ALPHAOP, D3DTOP_DISABLE);
  if (FAILED(hr)) {
    return reporter.FailHresult("SetTextureStageState(mutate ALPHAOP)", hr);
  }

  // Apply the recorded block; should restore the recorded values.
  hr = sb->Apply();
  if (FAILED(hr)) {
    return reporter.FailHresult("StateBlock Apply", hr);
  }

  D3DMATRIX got_m_record;
  ZeroMemory(&got_m_record, sizeof(got_m_record));
  hr = dev->GetTransform(D3DTS_WORLD, &got_m_record);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetTransform(after Apply)", hr);
  }
  if (!MatrixNearlyEqual(got_m_record, m_record, 1e-6f)) {
    return reporter.Fail("GetTransform mismatch after StateBlock Apply");
  }

  DWORD got_colorop = 0;
  hr = dev->GetTextureStageState(0, D3DTSS_COLOROP, &got_colorop);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetTextureStageState(after Apply COLOROP)", hr);
  }
  if (got_colorop != tss_record_colorop) {
    return reporter.Fail("GetTextureStageState(COLOROP) mismatch after Apply: got=%lu expected=%lu",
                         (unsigned long)got_colorop,
                         (unsigned long)tss_record_colorop);
  }

  DWORD got_alphaop = 0;
  hr = dev->GetTextureStageState(0, D3DTSS_ALPHAOP, &got_alphaop);
  if (FAILED(hr)) {
    return reporter.FailHresult("GetTextureStageState(after Apply ALPHAOP)", hr);
  }
  if (got_alphaop != tss_record_alphaop) {
    return reporter.Fail("GetTextureStageState(ALPHAOP) mismatch after Apply: got=%lu expected=%lu",
                         (unsigned long)got_alphaop,
                         (unsigned long)tss_record_alphaop);
  }

  return reporter.Pass();
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9ExFixedFuncStateRoundtrip(argc, argv);
}

