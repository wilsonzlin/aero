#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>

#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_umd.h"

namespace {

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg ? msg : "(null)");
    return false;
  }
  return true;
}

bool TestCursorNotCapturedByStateBlocks() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    std::vector<D3DDDI_HRESOURCE> resources;
    std::vector<D3D9DDI_HSTATEBLOCK> stateblocks;
    bool has_adapter = false;
    bool has_device = false;

    ~Cleanup() {
      if (has_device && device_funcs.pfnDeleteStateBlock) {
        for (D3D9DDI_HSTATEBLOCK sb : stateblocks) {
          if (sb.pDrvPrivate) {
            device_funcs.pfnDeleteStateBlock(hDevice, sb);
          }
        }
      }
      if (has_device && device_funcs.pfnDestroyResource) {
        for (D3DDDI_HRESOURCE hRes : resources) {
          if (hRes.pDrvPrivate) {
            device_funcs.pfnDestroyResource(hDevice, hRes);
          }
        }
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
  cleanup.hAdapter = open.hAdapter;
  cleanup.has_adapter = true;

  if (!Check(cleanup.adapter_funcs.pfnCreateDevice != nullptr, "pfnCreateDevice non-null")) {
    return false;
  }

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnCreateResource != nullptr, "pfnCreateResource non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetCursorProperties != nullptr, "pfnSetCursorProperties non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetCursorPosition != nullptr, "pfnSetCursorPosition non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnShowCursor != nullptr, "pfnShowCursor non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateStateBlock != nullptr, "pfnCreateStateBlock non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnApplyStateBlock != nullptr, "pfnApplyStateBlock non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDeleteStateBlock != nullptr, "pfnDeleteStateBlock non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnBeginStateBlock != nullptr, "pfnBeginStateBlock non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnEndStateBlock != nullptr, "pfnEndStateBlock non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetRenderState != nullptr, "pfnSetRenderState non-null")) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device handle must contain Device*")) {
    return false;
  }

  // Create two systemmem cursor bitmaps.
  constexpr uint32_t kD3dRTypeSurface = 1u;
  constexpr uint32_t kD3dFmtA8R8G8B8 = 21u;
  constexpr uint32_t kD3dPoolSystemmem = 2u;

  D3D9DDIARG_CREATERESOURCE cursor_a{};
  cursor_a.type = kD3dRTypeSurface;
  cursor_a.format = kD3dFmtA8R8G8B8;
  cursor_a.width = 2;
  cursor_a.height = 2;
  cursor_a.depth = 1;
  cursor_a.mip_levels = 1;
  cursor_a.usage = 0;
  cursor_a.pool = kD3dPoolSystemmem;
  cursor_a.size = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &cursor_a);
  if (!Check(hr == S_OK, "CreateResource cursor_a")) {
    return false;
  }
  cleanup.resources.push_back(cursor_a.hResource);

  D3D9DDIARG_CREATERESOURCE cursor_b = cursor_a;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &cursor_b);
  if (!Check(hr == S_OK, "CreateResource cursor_b")) {
    return false;
  }
  cleanup.resources.push_back(cursor_b.hResource);

  auto* cursor_a_res = reinterpret_cast<aerogpu::Resource*>(cursor_a.hResource.pDrvPrivate);
  auto* cursor_b_res = reinterpret_cast<aerogpu::Resource*>(cursor_b.hResource.pDrvPrivate);
  if (!Check(cursor_a_res != nullptr && cursor_b_res != nullptr, "cursor resources must be non-null")) {
    return false;
  }

  // Baseline cursor state (A).
  hr = cleanup.device_funcs.pfnSetCursorProperties(cleanup.hDevice, 0, 0, cursor_a.hResource);
  if (!Check(hr == S_OK, "SetCursorProperties(cursor_a)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetCursorPosition(cleanup.hDevice, 1, 2, 0);
  if (!Check(hr == S_OK, "SetCursorPosition(1,2)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnShowCursor(cleanup.hDevice, TRUE);
  if (!Check(hr == S_OK, "ShowCursor(TRUE)")) {
    return false;
  }

  // Create a state block snapshot (D3DSBT_ALL = 1).
  D3D9DDI_HSTATEBLOCK sb_all{};
  hr = cleanup.device_funcs.pfnCreateStateBlock(cleanup.hDevice, 1u, &sb_all);
  if (!Check(hr == S_OK, "CreateStateBlock(D3DSBT_ALL)")) {
    return false;
  }
  cleanup.stateblocks.push_back(sb_all);

  // Change cursor state (B).
  hr = cleanup.device_funcs.pfnSetCursorProperties(cleanup.hDevice, 1, 1, cursor_b.hResource);
  if (!Check(hr == S_OK, "SetCursorProperties(cursor_b)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetCursorPosition(cleanup.hDevice, 10, 20, 0);
  if (!Check(hr == S_OK, "SetCursorPosition(10,20)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnShowCursor(cleanup.hDevice, FALSE);
  if (!Check(hr == S_OK, "ShowCursor(FALSE)")) {
    return false;
  }

  // Applying the state block should NOT clobber cursor state.
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, sb_all);
  if (!Check(hr == S_OK, "ApplyStateBlock(D3DSBT_ALL)")) {
    return false;
  }
  if (!Check(dev->cursor_bitmap == cursor_b_res, "ApplyStateBlock must not restore cursor bitmap")) {
    return false;
  }
  if (!Check(dev->cursor_hot_x == 1 && dev->cursor_hot_y == 1, "ApplyStateBlock must not restore cursor hot spot")) {
    return false;
  }
  if (!Check(dev->cursor_x == 10 && dev->cursor_y == 20, "ApplyStateBlock must not restore cursor position")) {
    return false;
  }
  if (!Check(dev->cursor_visible == FALSE, "ApplyStateBlock must not restore cursor visibility")) {
    return false;
  }

  // Begin/EndStateBlock recording should also ignore cursor DDIs.
  hr = cleanup.device_funcs.pfnShowCursor(cleanup.hDevice, TRUE);
  if (!Check(hr == S_OK, "ShowCursor(TRUE) pre-record")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnBeginStateBlock(cleanup.hDevice);
  if (!Check(hr == S_OK, "BeginStateBlock")) {
    return false;
  }
  // Record some render state (ALPHABLENDENABLE=27).
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, 27u, TRUE);
  if (!Check(hr == S_OK, "SetRenderState(ALPHABLENDENABLE) during record")) {
    return false;
  }
  // Call cursor DDI during recording. This should modify current cursor state but
  // must not be captured into the state block.
  hr = cleanup.device_funcs.pfnShowCursor(cleanup.hDevice, FALSE);
  if (!Check(hr == S_OK, "ShowCursor(FALSE) during record")) {
    return false;
  }

  D3D9DDI_HSTATEBLOCK sb_recorded{};
  hr = cleanup.device_funcs.pfnEndStateBlock(cleanup.hDevice, &sb_recorded);
  if (!Check(hr == S_OK, "EndStateBlock")) {
    return false;
  }
  cleanup.stateblocks.push_back(sb_recorded);

  // Cursor is currently hidden due to ShowCursor(FALSE) above. Flip it back on,
  // then apply the recorded state block: cursor should stay visible.
  hr = cleanup.device_funcs.pfnShowCursor(cleanup.hDevice, TRUE);
  if (!Check(hr == S_OK, "ShowCursor(TRUE) post-record")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnApplyStateBlock(cleanup.hDevice, sb_recorded);
  if (!Check(hr == S_OK, "ApplyStateBlock(recorded)")) {
    return false;
  }
  if (!Check(dev->cursor_visible == TRUE, "ApplyStateBlock must not replay ShowCursor from recording")) {
    return false;
  }

  return true;
}

} // namespace

int main() {
  if (!TestCursorNotCapturedByStateBlocks()) {
    return 1;
  }
  return 0;
}

