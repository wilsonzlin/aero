#include "..\\common\\aerogpu_test_common.h"

#include <d3d9.h>

using aerogpu_test::ComPtr;

static double QpcToMs(LONGLONG qpc_delta, LONGLONG qpc_freq) {
  if (qpc_freq <= 0) {
    return 0.0;
  }
  return (double)qpc_delta * 1000.0 / (double)qpc_freq;
}

static int RunD3D9ExEventQuery(int argc, char** argv) {
  const char* kTestName = "d3d9ex_event_query";
  if (aerogpu_test::HasHelpArg(argc, argv)) {
    aerogpu_test::PrintfStdout(
        "Usage: %s.exe [--hidden] [--require-vid=0x####] [--require-did=0x####] [--allow-microsoft] [--allow-non-aerogpu]",
        kTestName);
    aerogpu_test::PrintfStdout("Creates a D3DQUERYTYPE_EVENT query and polls GetData() until it signals.");
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

  HWND hwnd = aerogpu_test::CreateBasicWindow(L"AeroGPU_D3D9ExEventQuery",
                                              L"AeroGPU D3D9Ex Event Query",
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
    return aerogpu_test::FailHresult(kTestName, "GetAdapterIdentifier (required for --require-vid/--require-did)", hr);
  }

  // Ensure there is at least some queued work before we issue the EVENT query.
  hr = dev->Clear(0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB(0, 0, 0), 1.0f, 0);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::Clear", hr);
  }

  ComPtr<IDirect3DQuery9> query;
  hr = dev->CreateQuery(D3DQUERYTYPE_EVENT, query.put());
  if (FAILED(hr) || !query) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DDevice9Ex::CreateQuery(D3DQUERYTYPE_EVENT)", hr);
  }

  hr = query->Issue(D3DISSUE_END);
  if (FAILED(hr)) {
    return aerogpu_test::FailHresult(kTestName, "IDirect3DQuery9::Issue(END)", hr);
  }

  LARGE_INTEGER qpc_freq_li;
  if (!QueryPerformanceFrequency(&qpc_freq_li) || qpc_freq_li.QuadPart <= 0) {
    return aerogpu_test::Fail(kTestName, "QueryPerformanceFrequency failed");
  }
  const LONGLONG qpc_freq = qpc_freq_li.QuadPart;

  const DWORD start_ms = GetTickCount();
  uint32_t polls = 0;
  bool saw_not_ready = false;

  for (;;) {
    ++polls;

    BOOL done = FALSE;
    LARGE_INTEGER t0;
    LARGE_INTEGER t1;
    QueryPerformanceCounter(&t0);
    hr = query->GetData(&done, sizeof(done), D3DGETDATA_FLUSH);
    QueryPerformanceCounter(&t1);

    const double call_ms = QpcToMs(t1.QuadPart - t0.QuadPart, qpc_freq);
    if (call_ms > 50.0) {
      return aerogpu_test::Fail(kTestName, "GetData blocked for %.3fms (expected non-blocking)", call_ms);
    }

    if (hr == S_OK) {
      if (!done) {
        return aerogpu_test::Fail(kTestName, "GetData returned S_OK but done=FALSE");
      }
      break;
    }
    if (hr == S_FALSE) {
      saw_not_ready = true;
    } else {
      return aerogpu_test::FailHresult(kTestName, "IDirect3DQuery9::GetData", hr);
    }

    if ((GetTickCount() - start_ms) > 5000) {
      return aerogpu_test::Fail(kTestName, "timed out waiting for EVENT query to signal");
    }

    // Yield to avoid pegging a CPU in the polling loop.
    Sleep(0);
  }

  aerogpu_test::PrintfStdout("INFO: %s: EVENT query signaled after %u polls%s",
                             kTestName,
                             (unsigned)polls,
                             saw_not_ready ? "" : " (signaled immediately)");
  aerogpu_test::PrintfStdout("PASS: %s", kTestName);
  return 0;
}

int main(int argc, char** argv) {
  aerogpu_test::ConfigureProcessForAutomation();
  return RunD3D9ExEventQuery(argc, argv);
}

