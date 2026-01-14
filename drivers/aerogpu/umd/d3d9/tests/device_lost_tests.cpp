#include <cstdint>
#include <cstdio>
#include <cstring>
 
#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_test_entrypoints.h"
   
namespace aerogpu {

namespace {
 
// Stable device-lost HRESULT expected from hot DDIs.
//
// Portable builds return D3DERR_DEVICELOST; WDK builds may surface DDI-level
// device-hung codes that are more specific.
#if defined(D3DDDIERR_DEVICEHUNG)
constexpr HRESULT kExpectedDeviceLostHr = D3DDDIERR_DEVICEHUNG;
constexpr HRESULT kStoredDeviceLostHr = D3DDDIERR_DEVICEHUNG;
#else
constexpr HRESULT kExpectedDeviceLostHr = 0x88760868L; // D3DERR_DEVICELOST
// Any failing HRESULT is fine here; the driver maps it to D3DERR_DEVICELOST.
constexpr HRESULT kStoredDeviceLostHr = E_FAIL;
#endif
 
bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}
 
bool TestDeviceLostDdiReturnsStableError() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3D9DDI_HQUERY hQuery{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_query = false;
 
    ~Cleanup() {
      if (has_query && device_funcs.pfnDestroyQuery) {
        device_funcs.pfnDestroyQuery(hDevice, hQuery);
      }
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
 
  // Create an EVENT query so we can validate GetQueryData behavior. Do this
  // before marking the device as lost.
  if (!Check(cleanup.device_funcs.pfnCreateQuery != nullptr, "CreateQuery must be available")) {
    return false;
  }
  D3D9DDIARG_CREATEQUERY create_query{};
  create_query.type = 8u; // D3DQUERYTYPE_EVENT
  hr = cleanup.device_funcs.pfnCreateQuery(create_dev.hDevice, &create_query);
  if (!Check(hr == S_OK, "CreateQuery(EVENT)")) {
    return false;
  }
  if (!Check(create_query.hQuery.pDrvPrivate != nullptr, "CreateQuery returned query handle")) {
    return false;
  }
  cleanup.hQuery = create_query.hQuery;
  cleanup.has_query = true;
 
  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }
  
  // Force device-lost state (portable build can't trigger real WDDM submission failures).
  hr = device_test_force_device_lost(create_dev.hDevice, kStoredDeviceLostHr);
  if (!Check(hr == S_OK, "device_test_force_device_lost")) {
    return false;
  }
  
  if (!Check(cleanup.device_funcs.pfnCheckDeviceState != nullptr, "CheckDeviceState must be available")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnCheckDeviceState(create_dev.hDevice, nullptr);
  if (!Check(hr == kExpectedDeviceLostHr, "CheckDeviceState returns DEVICELOST when device is lost")) {
    return false;
  }
 
  if (!Check(cleanup.device_funcs.pfnFlush != nullptr, "Flush must be available")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnFlush(create_dev.hDevice);
  if (!Check(hr == kExpectedDeviceLostHr, "Flush returns DEVICELOST when device is lost")) {
    return false;
  }
 
  if (!Check(cleanup.device_funcs.pfnGetQueryData != nullptr, "GetQueryData must be available")) {
    return false;
  }
  uint32_t query_data = 0xDEADBEEFu;
  D3D9DDIARG_GETQUERYDATA get_query_data{};
  get_query_data.hQuery = cleanup.hQuery;
  get_query_data.pData = &query_data;
  get_query_data.data_size = sizeof(query_data);
  get_query_data.flags = 0;
  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &get_query_data);
  if (!Check(hr == kExpectedDeviceLostHr, "GetQueryData returns DEVICELOST when device is lost")) {
    return false;
  }
  if (!Check(query_data == 0, "GetQueryData zeros output buffer when device is lost")) {
    return false;
  }
 
  if (!Check(cleanup.device_funcs.pfnDrawPrimitive != nullptr, "DrawPrimitive must be available")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnDrawPrimitive(create_dev.hDevice, D3DDDIPT_TRIANGLELIST, 0, 0);
  if (!Check(hr == kExpectedDeviceLostHr, "DrawPrimitive returns DEVICELOST when device is lost")) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnDrawRectPatch != nullptr, "DrawRectPatch must be available")) {
    return false;
  }
  float rect_segs[4] = {1.0f, 1.0f, 1.0f, 1.0f};
  D3DRECTPATCH_INFO rect_info{};
  rect_info.StartVertexOffset = 0;
  rect_info.NumVertices = 16;
  rect_info.Basis = D3DBASIS_BEZIER;
  rect_info.Degree = D3DDEGREE_CUBIC;
  D3DDDIARG_DRAWRECTPATCH draw_rect{};
  draw_rect.Handle = 1;
  draw_rect.pNumSegs = rect_segs;
  draw_rect.pRectPatchInfo = &rect_info;
  hr = cleanup.device_funcs.pfnDrawRectPatch(create_dev.hDevice, &draw_rect);
  if (!Check(hr == kExpectedDeviceLostHr, "DrawRectPatch returns DEVICELOST when device is lost")) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnDrawTriPatch != nullptr, "DrawTriPatch must be available")) {
    return false;
  }
  float tri_segs[3] = {1.0f, 1.0f, 1.0f};
  D3DTRIPATCH_INFO tri_info{};
  tri_info.StartVertexOffset = 0;
  tri_info.NumVertices = 10;
  tri_info.Basis = D3DBASIS_BEZIER;
  tri_info.Degree = D3DDEGREE_CUBIC;
  D3DDDIARG_DRAWTRIPATCH draw_tri{};
  draw_tri.Handle = 2;
  draw_tri.pNumSegs = tri_segs;
  draw_tri.pTriPatchInfo = &tri_info;
  hr = cleanup.device_funcs.pfnDrawTriPatch(create_dev.hDevice, &draw_tri);
  if (!Check(hr == kExpectedDeviceLostHr, "DrawTriPatch returns DEVICELOST when device is lost")) {
    return false;
  }

  if (!Check(cleanup.device_funcs.pfnPresent != nullptr, "Present must be available")) {
    return false;
  }
  D3D9DDIARG_PRESENT present{};
  hr = cleanup.device_funcs.pfnPresent(create_dev.hDevice, &present);
  if (!Check(hr == kExpectedDeviceLostHr, "Present returns DEVICELOST when device is lost")) {
    return false;
  }
 
  if (!Check(cleanup.device_funcs.pfnPresentEx != nullptr, "PresentEx must be available")) {
    return false;
  }
  D3D9DDIARG_PRESENTEX present_ex{};
  hr = cleanup.device_funcs.pfnPresentEx(create_dev.hDevice, &present_ex);
  if (!Check(hr == kExpectedDeviceLostHr, "PresentEx returns DEVICELOST when device is lost")) {
    return false;
  }
 
  return true;
}
 
} // namespace
} // namespace aerogpu
 
int main() {
  return aerogpu::TestDeviceLostDdiReturnsStableError() ? 0 : 1;
}
