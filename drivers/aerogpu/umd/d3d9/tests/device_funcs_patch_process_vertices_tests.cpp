#include <cstdio>

#include "aerogpu_d3d9_umd.h"

namespace {

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

bool TestDeviceFuncsIncludesPatchAndProcessVertices() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    bool has_adapter = false;
    bool has_device = false;

    ~Cleanup() {
      if (has_device && device_funcs.pfnDestroyDevice) {
        device_funcs.pfnDestroyDevice(hDevice);
      }
      if (has_adapter && adapter_funcs.pfnCloseAdapter) {
        adapter_funcs.pfnCloseAdapter(hAdapter);
      }
    }
  } cleanup;

  D3DDDIARG_OPENADAPTER2 open{};
  open.Interface = 1;
  open.Version = 1;
  D3DDDI_ADAPTERCALLBACKS callbacks{};
  D3DDDI_ADAPTERCALLBACKS2 callbacks2{};
  open.pAdapterCallbacks = &callbacks;
  open.pAdapterCallbacks2 = &callbacks2;
  open.pAdapterFuncs = &cleanup.adapter_funcs;

  HRESULT hr = ::OpenAdapter2(&open);
  if (!Check(hr == S_OK, "OpenAdapter2")) {
    return false;
  }
  if (!Check(open.hAdapter.pDrvPrivate != nullptr, "OpenAdapter2 returned adapter handle")) {
    return false;
  }
  cleanup.hAdapter = open.hAdapter;
  cleanup.has_adapter = true;

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  if (!Check(create_dev.hDevice.pDrvPrivate != nullptr, "CreateDevice returned device handle")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnDrawRectPatch != nullptr, "device_funcs.pfnDrawRectPatch is non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawTriPatch != nullptr, "device_funcs.pfnDrawTriPatch is non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeletePatch != nullptr, "device_funcs.pfnDeletePatch is non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnProcessVertices != nullptr, "device_funcs.pfnProcessVertices is non-null")) {
    return false;
  }

  return true;
}

} // namespace

int main() {
  if (!TestDeviceFuncsIncludesPatchAndProcessVertices()) {
    return 1;
  }
  return 0;
}

