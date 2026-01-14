#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <cstdio>
#include <cstring>
#include <vector>

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_umd.h"
#include "aerogpu_d3d9_test_entrypoints.h"

namespace {

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

bool DebugEnabled() {
  const char* v = std::getenv("AEROGPU_D3D9_CURSOR_TEST_DEBUG");
  return v && *v && std::strcmp(v, "0") != 0;
}

void Debug(const char* msg) {
  if (DebugEnabled()) {
    std::fprintf(stderr, "DEBUG: %s\n", msg ? msg : "(null)");
  }
}

size_t StreamBytesUsed(const uint8_t* buf, size_t capacity) {
  if (!buf || capacity < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  const size_t used = stream->size_bytes;
  if (used < sizeof(aerogpu_cmd_stream_header) || used > capacity) {
    return 0;
  }
  return used;
}

bool HasDrawBeforePresentEx(const uint8_t* buf, size_t capacity) {
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (stream_len == 0) {
    return false;
  }

  bool saw_draw = false;
  bool saw_present = false;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_DRAW) {
      saw_draw = true;
    }
    if (hdr->opcode == AEROGPU_CMD_PRESENT_EX) {
      saw_present = true;
      break;
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }

  return saw_present && saw_draw;
}

struct CursorOverlayRenderStateObservations {
  bool saw_present = false;
  bool saw_draw = false;

  bool saw_alpha_blend_enable = false;
  bool saw_alpha_blend_disable_after_draw = false;

  bool saw_src_blend_src_alpha = false;
  bool saw_dst_blend_inv_src_alpha = false;
};

CursorOverlayRenderStateObservations ObserveCursorOverlayRenderStates(const uint8_t* buf, size_t capacity) {
  CursorOverlayRenderStateObservations out{};
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (stream_len == 0) {
    return out;
  }

  // D3D9 render state IDs / values (numeric values from d3d9types.h).
  constexpr uint32_t kD3d9RsAlphaBlendEnable = 27;
  constexpr uint32_t kD3d9RsSrcBlend = 19;
  constexpr uint32_t kD3d9RsDestBlend = 20;
  constexpr uint32_t kD3d9BlendSrcAlpha = 5;
  constexpr uint32_t kD3d9BlendInvSrcAlpha = 6;

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_DRAW) {
      out.saw_draw = true;
    }

    if (hdr->opcode == AEROGPU_CMD_SET_RENDER_STATE &&
        hdr->size_bytes >= sizeof(aerogpu_cmd_set_render_state)) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_render_state*>(buf + offset);
      if (!out.saw_draw) {
        // Expect the cursor overlay to enable blending before it draws.
        if (cmd->state == kD3d9RsAlphaBlendEnable && cmd->value != 0) {
          out.saw_alpha_blend_enable = true;
        }
        if (cmd->state == kD3d9RsSrcBlend && cmd->value == kD3d9BlendSrcAlpha) {
          out.saw_src_blend_src_alpha = true;
        }
        if (cmd->state == kD3d9RsDestBlend && cmd->value == kD3d9BlendInvSrcAlpha) {
          out.saw_dst_blend_inv_src_alpha = true;
        }
      } else {
        // After the overlay draw, the driver should restore alpha blending to its previous state.
        if (cmd->state == kD3d9RsAlphaBlendEnable && cmd->value == 0) {
          out.saw_alpha_blend_disable_after_draw = true;
        }
      }
    }

    if (hdr->opcode == AEROGPU_CMD_PRESENT_EX) {
      out.saw_present = true;
      break;
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }

  return out;
}

bool TestCursorOverlayPresentEx() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    std::vector<D3DDDI_HRESOURCE> resources;
    bool has_adapter = false;
    bool has_device = false;

    ~Cleanup() {
      if (has_device && device_funcs.pfnDestroyResource) {
        Debug("cleanup: destroy resources");
        for (size_t i = 0; i < resources.size(); ++i) {
          const D3DDDI_HRESOURCE hRes = resources[i];
          if (hRes.pDrvPrivate) {
            if (DebugEnabled()) {
              std::fprintf(stderr, "DEBUG: cleanup: destroy resource[%zu]=%p\n", i, hRes.pDrvPrivate);
            }
            device_funcs.pfnDestroyResource(hDevice, hRes);
          }
        }
      }
      if (has_device && device_funcs.pfnDestroyDevice) {
        Debug("cleanup: destroy device");
        device_funcs.pfnDestroyDevice(hDevice);
      }
      if (has_adapter && adapter_funcs.pfnCloseAdapter) {
        Debug("cleanup: close adapter");
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
  Debug("after OpenAdapter2");
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
  Debug("after CreateDevice");
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnSetCursorProperties != nullptr, "pfnSetCursorProperties non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetCursorPosition != nullptr, "pfnSetCursorPosition non-null")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnShowCursor != nullptr, "pfnShowCursor non-null")) {
    return false;
  }

  auto* dev = reinterpret_cast<aerogpu::Device*>(cleanup.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device handle must contain Device*")) {
    return false;
  }
  Debug("after device pointer");

  std::vector<uint8_t> submit_buf(1024 * 1024);
  dev->cmd.set_span(submit_buf.data(), submit_buf.size());
  struct CmdRestore {
    aerogpu::Device* dev = nullptr;
    ~CmdRestore() {
      // Switch back to vector mode so cleanup (DestroyResource/DestroyDevice)
      // cannot write into a span buffer that may be freed when this test exits.
      if (dev) {
        dev->cmd.set_vector();
      }
    }
  } cmd_restore{dev};
  Debug("after cmd.set_span");

  // Create a render-target surface to act as the present source/backbuffer.
  D3D9DDIARG_CREATERESOURCE backbuffer{};
  backbuffer.type = 1;   // surface-ish
  backbuffer.format = 21; // D3DFMT_A8R8G8B8
  backbuffer.width = 64;
  backbuffer.height = 64;
  backbuffer.depth = 1;
  backbuffer.mip_levels = 1;
  backbuffer.usage = 0x00000001u; // D3DUSAGE_RENDERTARGET
  backbuffer.pool = 0;           // D3DPOOL_DEFAULT
  backbuffer.size = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &backbuffer);
  if (!Check(hr == S_OK, "CreateResource backbuffer")) {
    return false;
  }
  Debug("after CreateResource backbuffer");
  cleanup.resources.push_back(backbuffer.hResource);

  // Create a dummy texture to bind at stage 0 so we can validate state restoration.
  D3D9DDIARG_CREATERESOURCE dummy_tex{};
  dummy_tex.type = 1;
  dummy_tex.format = 21; // D3DFMT_A8R8G8B8
  dummy_tex.width = 1;
  dummy_tex.height = 1;
  dummy_tex.depth = 1;
  dummy_tex.mip_levels = 1;
  dummy_tex.usage = 0;
  dummy_tex.pool = 0;
  dummy_tex.size = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &dummy_tex);
  if (!Check(hr == S_OK, "CreateResource dummy_tex")) {
    return false;
  }
  Debug("after CreateResource dummy_tex");
  cleanup.resources.push_back(dummy_tex.hResource);

  // Bind some state that the cursor overlay must preserve.
  hr = cleanup.device_funcs.pfnSetRenderTarget(cleanup.hDevice, 0, backbuffer.hResource);
  if (!Check(hr == S_OK, "SetRenderTarget(0)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetTexture(cleanup.hDevice, 0, dummy_tex.hResource);
  if (!Check(hr == S_OK, "SetTexture(0)")) {
    return false;
  }
  Debug("after binding baseline state");

  // Viewport/scissor + a few render/sampler states.
  D3DDDIVIEWPORTINFO vp{};
  vp.X = 1.0f;
  vp.Y = 2.0f;
  vp.Width = 30.0f;
  vp.Height = 40.0f;
  vp.MinZ = 0.1f;
  vp.MaxZ = 0.9f;
  hr = cleanup.device_funcs.pfnSetViewport(cleanup.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport")) {
    return false;
  }
  RECT scissor = {3, 4, 20, 21};
  hr = cleanup.device_funcs.pfnSetScissorRect(cleanup.hDevice, &scissor, TRUE);
  if (!Check(hr == S_OK, "SetScissorRect")) {
    return false;
  }
  Debug("after viewport/scissor");
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, 174, TRUE); // D3DRS_SCISSORTESTENABLE
  if (!Check(hr == S_OK, "SetRenderState(SCISSORTESTENABLE)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetRenderState(cleanup.hDevice, 27, FALSE); // D3DRS_ALPHABLENDENABLE
  if (!Check(hr == S_OK, "SetRenderState(ALPHABLENDENABLE)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetSamplerState(cleanup.hDevice, 0, 1, 1); // D3DSAMP_ADDRESSU = WRAP
  if (!Check(hr == S_OK, "SetSamplerState(ADDRESSU)")) {
    return false;
  }
  hr = cleanup.device_funcs.pfnSetSamplerState(cleanup.hDevice, 0, 6, 2); // D3DSAMP_MINFILTER = LINEAR
  if (!Check(hr == S_OK, "SetSamplerState(MINFILTER)")) {
    return false;
  }

  // Snapshot state that must survive PresentEx (cursor overlay must restore).
  const aerogpu::Resource* saved_rt0 = dev->render_targets[0];
  const aerogpu::Resource* saved_tex0 = dev->textures[0];
  const D3DDDIVIEWPORTINFO saved_vp = dev->viewport;
  const RECT saved_scissor = dev->scissor_rect;
  const BOOL saved_scissor_enabled = dev->scissor_enabled;
  const uint32_t saved_rs_scissor = dev->render_states[174];
  const uint32_t saved_rs_alpha_blend = dev->render_states[27];
  const uint32_t saved_samp_addr_u = dev->sampler_states[0][1];
  const uint32_t saved_samp_min = dev->sampler_states[0][6];
  const uint32_t saved_rs_src_blend = dev->render_states[19]; // D3DRS_SRCBLEND
  const uint32_t saved_rs_dst_blend = dev->render_states[20]; // D3DRS_DESTBLEND

  // Create a systemmem cursor bitmap (as per D3D9 API requirements).
  D3D9DDIARG_CREATERESOURCE cursor{};
  cursor.type = 1;
  cursor.format = 21; // D3DFMT_A8R8G8B8
  cursor.width = 2;
  cursor.height = 2;
  cursor.depth = 1;
  cursor.mip_levels = 1;
  cursor.usage = 0;
  cursor.pool = 2; // D3DPOOL_SYSTEMMEM
  cursor.size = 0;
  hr = cleanup.device_funcs.pfnCreateResource(cleanup.hDevice, &cursor);
  if (!Check(hr == S_OK, "CreateResource cursor")) {
    return false;
  }
  Debug("after CreateResource cursor");
  cleanup.resources.push_back(cursor.hResource);

  auto* cursor_res = reinterpret_cast<aerogpu::Resource*>(cursor.hResource.pDrvPrivate);
  if (!Check(cursor_res != nullptr, "cursor resource ptr")) {
    return false;
  }
  if (!Check(cursor_res->storage.size() >= 2 * 2 * 4, "cursor resource storage allocated")) {
    return false;
  }

  // Fill the cursor bitmap with some alpha so the overlay path must enable blending.
  // Format: A8R8G8B8 (bytes: B,G,R,A).
  std::memset(cursor_res->storage.data(), 0, cursor_res->storage.size());
  // Top-left pixel: red with 50% alpha.
  cursor_res->storage[0] = 0x00;  // B
  cursor_res->storage[1] = 0x00;  // G
  cursor_res->storage[2] = 0xFF;  // R
  cursor_res->storage[3] = 0x80;  // A

  hr = cleanup.device_funcs.pfnSetCursorProperties(cleanup.hDevice, 0, 0, cursor.hResource);
  if (!Check(hr == S_OK, "SetCursorProperties")) {
    return false;
  }
  Debug("after SetCursorProperties");
  hr = cleanup.device_funcs.pfnSetCursorPosition(cleanup.hDevice, 5, 6, 0);
  if (!Check(hr == S_OK, "SetCursorPosition")) {
    return false;
  }
  Debug("after SetCursorPosition");
  hr = cleanup.device_funcs.pfnShowCursor(cleanup.hDevice, TRUE);
  if (!Check(hr == S_OK, "ShowCursor(TRUE)")) {
    return false;
  }
  Debug("after ShowCursor");

  // PresentEx should emit an overlay draw before PRESENT_EX and must not corrupt state.
  D3D9DDIARG_PRESENTEX present{};
  present.hSrc = backbuffer.hResource;
  present.hWnd = nullptr;
  present.sync_interval = 0;
  present.d3d9_present_flags = 0;
  Debug("before PresentEx");
  hr = cleanup.device_funcs.pfnPresentEx(cleanup.hDevice, &present);
  if (!Check(hr == S_OK, "PresentEx")) {
    return false;
  }
  Debug("after PresentEx");

  if (!Check(HasDrawBeforePresentEx(submit_buf.data(), submit_buf.size()),
             "cursor overlay must emit DRAW before PRESENT_EX")) {
    return false;
  }
  Debug("after opcode check");

  const CursorOverlayRenderStateObservations rs = ObserveCursorOverlayRenderStates(submit_buf.data(), submit_buf.size());
  if (!Check(rs.saw_present && rs.saw_draw, "cursor overlay stream must contain DRAW + PRESENT_EX")) {
    return false;
  }
  if (!Check(rs.saw_alpha_blend_enable, "cursor overlay must enable alpha blending before DRAW")) {
    return false;
  }
  if (!Check(rs.saw_src_blend_src_alpha, "cursor overlay must set SRCBLEND=SRCALPHA before DRAW")) {
    return false;
  }
  if (!Check(rs.saw_dst_blend_inv_src_alpha, "cursor overlay must set DESTBLEND=INVSRCALPHA before DRAW")) {
    return false;
  }
  if (!Check(rs.saw_alpha_blend_disable_after_draw, "cursor overlay must restore ALPHABLENDENABLE after DRAW")) {
    return false;
  }

  // Cached device state must match pre-present values.
  if (!Check(dev->render_targets[0] == saved_rt0, "render target[0] restored")) {
    return false;
  }
  if (!Check(dev->textures[0] == saved_tex0, "texture[0] restored")) {
    return false;
  }
  if (!Check(std::memcmp(&dev->viewport, &saved_vp, sizeof(saved_vp)) == 0, "viewport restored")) {
    return false;
  }
  if (!Check(std::memcmp(&dev->scissor_rect, &saved_scissor, sizeof(saved_scissor)) == 0, "scissor rect restored")) {
    return false;
  }
  if (!Check(dev->scissor_enabled == saved_scissor_enabled, "scissor enabled restored")) {
    return false;
  }
  if (!Check(dev->render_states[174] == saved_rs_scissor, "render state scissor restored")) {
    return false;
  }
  if (!Check(dev->render_states[27] == saved_rs_alpha_blend, "render state alpha blend restored")) {
    return false;
  }
  if (!Check(dev->sampler_states[0][1] == saved_samp_addr_u, "sampler ADDRESSU restored")) {
    return false;
  }
  if (!Check(dev->sampler_states[0][6] == saved_samp_min, "sampler MINFILTER restored")) {
    return false;
  }
  if (!Check(dev->render_states[19] == saved_rs_src_blend, "render state SRCBLEND restored")) {
    return false;
  }
  if (!Check(dev->render_states[20] == saved_rs_dst_blend, "render state DESTBLEND restored")) {
    return false;
  }

  // If the cursor path is handled via the KMD hardware cursor registers, the UMD
  // should not also draw a software cursor overlay during PresentEx. (Double
  // cursor bugs are extremely user-visible; keep this behavior locked in.)
  hr = aerogpu::device_test_set_cursor_hw_active(cleanup.hDevice, TRUE);
  if (!Check(hr == S_OK, "device_test_set_cursor_hw_active(TRUE)")) {
    return false;
  }
  Debug("before PresentEx (cursor_hw_active=true)");
  hr = cleanup.device_funcs.pfnPresentEx(cleanup.hDevice, &present);
  if (!Check(hr == S_OK, "PresentEx with cursor_hw_active=true")) {
    return false;
  }
  Debug("after PresentEx (cursor_hw_active=true)");

  const CursorOverlayRenderStateObservations rs_hw = ObserveCursorOverlayRenderStates(submit_buf.data(), submit_buf.size());
  if (!Check(rs_hw.saw_present, "hardware cursor path must still emit PRESENT_EX")) {
    return false;
  }
  if (!Check(!rs_hw.saw_draw, "hardware cursor path must not emit DRAW overlay before PRESENT_EX")) {
    return false;
  }

  Debug("before return true");
  return true;
}

} // namespace

int main() {
  if (!TestCursorOverlayPresentEx()) {
    return 1;
  }
  return 0;
}
