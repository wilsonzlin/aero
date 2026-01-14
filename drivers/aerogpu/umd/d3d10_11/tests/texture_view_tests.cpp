#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

#include "aerogpu_d3d10_11_umd.h"
#include "aerogpu_d3d10_11_internal.h"
#include "aerogpu_cmd.h"

namespace {

constexpr uint32_t kDxgiFormatB8G8R8A8Unorm = 87; // DXGI_FORMAT_B8G8R8A8_UNORM
constexpr uint32_t kD3D11BindShaderResource = 0x8;
constexpr uint32_t kD3D11BindRenderTarget = 0x20;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

size_t StreamBytesUsed(const uint8_t* buf, size_t len) {
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  const size_t used = static_cast<size_t>(stream->size_bytes);
  if (used < sizeof(aerogpu_cmd_stream_header) || used > len) {
    return len;
  }
  return used;
}

bool ValidateStream(const uint8_t* buf, size_t len) {
  if (!Check(buf != nullptr, "stream buffer must be non-null")) {
    return false;
  }
  if (!Check(len >= sizeof(aerogpu_cmd_stream_header), "stream must contain header")) {
    return false;
  }

  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  if (!Check(stream->magic == AEROGPU_CMD_STREAM_MAGIC, "stream magic")) {
    return false;
  }
  if (!Check(stream->abi_version == AEROGPU_ABI_VERSION_U32, "stream abi_version")) {
    return false;
  }
  if (!Check(stream->flags == AEROGPU_CMD_STREAM_FLAG_NONE, "stream flags")) {
    return false;
  }
  if (!Check(stream->size_bytes >= sizeof(aerogpu_cmd_stream_header), "stream size_bytes >= header")) {
    return false;
  }
  if (!Check(stream->size_bytes <= len, "stream size_bytes within buffer")) {
    return false;
  }

  const size_t stream_len = static_cast<size_t>(stream->size_bytes);
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset < stream_len) {
    if (!Check(stream_len - offset >= sizeof(aerogpu_cmd_hdr), "packet header fits")) {
      return false;
    }
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (!Check(hdr->size_bytes >= sizeof(aerogpu_cmd_hdr), "packet size >= header")) {
      return false;
    }
    if (!Check((hdr->size_bytes & 3u) == 0, "packet size is 4-byte aligned")) {
      return false;
    }
    if (!Check(hdr->size_bytes <= stream_len - offset, "packet size within stream")) {
      return false;
    }
    offset += hdr->size_bytes;
  }
  return Check(offset == stream_len, "parser consumed stream");
}

struct Harness {
  std::vector<uint8_t> last_stream;
  std::vector<HRESULT> errors;

  static HRESULT AEROGPU_APIENTRY SubmitCmdStream(void* user,
                                                  const void* cmd_stream,
                                                  uint32_t cmd_stream_size_bytes,
                                                  const AEROGPU_WDDM_SUBMIT_ALLOCATION*,
                                                  uint32_t,
                                                  uint64_t* out_fence) {
    if (!user || !cmd_stream || cmd_stream_size_bytes < sizeof(aerogpu_cmd_stream_header)) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    const auto* bytes = reinterpret_cast<const uint8_t*>(cmd_stream);
    h->last_stream.assign(bytes, bytes + cmd_stream_size_bytes);
    if (out_fence) {
      *out_fence = 0;
    }
    return S_OK;
  }

  static void AEROGPU_APIENTRY SetError(void* user, HRESULT hr) {
    if (!user) {
      return;
    }
    reinterpret_cast<Harness*>(user)->errors.push_back(hr);
  }
};

struct TestDevice {
  Harness harness{};

  D3D10DDI_HADAPTER hAdapter{};
  D3D10DDI_ADAPTERFUNCS adapter_funcs{};

  D3D10DDI_HDEVICE hDevice{};
  AEROGPU_D3D10_11_DEVICEFUNCS device_funcs{};
  std::vector<uint8_t> device_mem;

  AEROGPU_D3D10_11_DEVICECALLBACKS callbacks{};
};

bool InitTestDevice(TestDevice* out) {
  if (!out) {
    return false;
  }

  out->callbacks.pUserContext = &out->harness;
  out->callbacks.pfnSubmitCmdStream = &Harness::SubmitCmdStream;
  out->callbacks.pfnSetError = &Harness::SetError;

  D3D10DDIARG_OPENADAPTER open{};
  open.pAdapterFuncs = &out->adapter_funcs;
  const HRESULT hr = OpenAdapter10(&open);
  if (!Check(hr == S_OK, "OpenAdapter10")) {
    return false;
  }
  out->hAdapter = open.hAdapter;

  D3D10DDIARG_CREATEDEVICE create{};
  create.hDevice.pDrvPrivate = nullptr;
  const SIZE_T dev_size = out->adapter_funcs.pfnCalcPrivateDeviceSize(out->hAdapter, &create);
  if (!Check(dev_size >= sizeof(void*), "CalcPrivateDeviceSize returned a non-trivial size")) {
    return false;
  }

  out->device_mem.assign(static_cast<size_t>(dev_size), 0);
  create.hDevice.pDrvPrivate = out->device_mem.data();
  create.pDeviceFuncs = &out->device_funcs;
  create.pDeviceCallbacks = &out->callbacks;

  const HRESULT create_hr = out->adapter_funcs.pfnCreateDevice(out->hAdapter, &create);
  if (!Check(create_hr == S_OK, "CreateDevice")) {
    return false;
  }

  out->hDevice = create.hDevice;
  return true;
}

struct TestResource {
  D3D10DDI_HRESOURCE hResource{};
  std::vector<uint8_t> storage;
};

struct TestRtv {
  D3D10DDI_HRENDERTARGETVIEW hRtv{};
  std::vector<uint8_t> storage;
};

struct TestSrv {
  D3D10DDI_HSHADERRESOURCEVIEW hSrv{};
  std::vector<uint8_t> storage;
};

bool TestTextureViewsEmitBindDestroy() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev), "InitTestDevice")) {
    return false;
  }

  // Enable texture-view support (ABI gated).
  auto* adapter = reinterpret_cast<aerogpu::d3d10_11::Adapter*>(dev.hAdapter.pDrvPrivate);
  if (!Check(adapter != nullptr, "adapter private pointer")) {
    return false;
  }
  adapter->umd_private_valid = true;
  adapter->umd_private.device_abi_version_u32 = (AEROGPU_ABI_MAJOR << 16) | 4; // ABI 1.4 (texture views)

  // Create a Texture2D with multiple mips and array layers.
  AEROGPU_DDIARG_CREATERESOURCE tex_desc{};
  tex_desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  tex_desc.BindFlags = kD3D11BindShaderResource | kD3D11BindRenderTarget;
  tex_desc.MiscFlags = 0;
  tex_desc.Usage = AEROGPU_D3D11_USAGE_DEFAULT;
  tex_desc.CPUAccessFlags = 0;
  tex_desc.Width = 4;
  tex_desc.Height = 4;
  tex_desc.MipLevels = 3;
  tex_desc.ArraySize = 2;
  tex_desc.Format = kDxgiFormatB8G8R8A8Unorm;
  tex_desc.SampleDescCount = 1;
  tex_desc.SampleDescQuality = 0;
  tex_desc.ResourceFlags = 0;
  tex_desc.pInitialData = nullptr;
  tex_desc.InitialDataCount = 0;

  TestResource tex{};
  const SIZE_T tex_size = dev.device_funcs.pfnCalcPrivateResourceSize(dev.hDevice, &tex_desc);
  if (!Check(tex_size >= sizeof(void*), "CalcPrivateResourceSize(tex2d)")) {
    return false;
  }
  tex.storage.assign(static_cast<size_t>(tex_size), 0);
  tex.hResource.pDrvPrivate = tex.storage.data();
  if (!Check(dev.device_funcs.pfnCreateResource(dev.hDevice, &tex_desc, tex.hResource) == S_OK, "CreateResource(tex2d)")) {
    return false;
  }

  // Create an SRV that selects mip1 + slice1.
  AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW srv_desc{};
  srv_desc.hResource = tex.hResource;
  srv_desc.Format = 0; // use resource format
  srv_desc.ViewDimension = AEROGPU_DDI_SRV_DIMENSION_TEXTURE2DARRAY;
  srv_desc.MostDetailedMip = 1;
  srv_desc.MipLevels = 1;
  srv_desc.FirstArraySlice = 1;
  srv_desc.ArraySize = 1;

  TestSrv srv{};
  const SIZE_T srv_size = dev.device_funcs.pfnCalcPrivateShaderResourceViewSize(dev.hDevice, &srv_desc);
  if (!Check(srv_size >= sizeof(void*), "CalcPrivateShaderResourceViewSize")) {
    return false;
  }
  srv.storage.assign(static_cast<size_t>(srv_size), 0);
  srv.hSrv.pDrvPrivate = srv.storage.data();
  if (!Check(dev.device_funcs.pfnCreateShaderResourceView(dev.hDevice, &srv_desc, srv.hSrv) == S_OK, "CreateShaderResourceView")) {
    return false;
  }

  // Create an RTV that selects mip1 + slice1.
  AEROGPU_DDIARG_CREATERENDERTARGETVIEW rtv_desc{};
  rtv_desc.hResource = tex.hResource;
  rtv_desc.Format = 0; // use resource format
  rtv_desc.ViewDimension = 4u; // Texture2DArray (portable ABI)
  rtv_desc.MipSlice = 1;
  rtv_desc.FirstArraySlice = 1;
  rtv_desc.ArraySize = 1;

  TestRtv rtv{};
  const SIZE_T rtv_size = dev.device_funcs.pfnCalcPrivateRTVSize(dev.hDevice, &rtv_desc);
  if (!Check(rtv_size >= sizeof(void*), "CalcPrivateRTVSize")) {
    return false;
  }
  rtv.storage.assign(static_cast<size_t>(rtv_size), 0);
  rtv.hRtv.pDrvPrivate = rtv.storage.data();
  if (!Check(dev.device_funcs.pfnCreateRTV(dev.hDevice, &rtv_desc, rtv.hRtv) == S_OK, "CreateRTV")) {
    return false;
  }

  // Bind SRV then bind RTV. Binding the RTV must auto-unbind the SRV based on the
  // underlying base resource (view handles differ).
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*view_count=*/1, &srv.hSrv);

  D3D10DDI_HRENDERTARGETVIEW rtvs[1] = {rtv.hRtv};
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/1, rtvs, D3D10DDI_HDEPTHSTENCILVIEW{});

  const HRESULT flush_hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(flush_hr == S_OK, "Flush after binding views")) {
    return false;
  }

  const std::vector<uint8_t> stream0 = dev.harness.last_stream;
  if (!Check(ValidateStream(stream0.data(), stream0.size()), "ValidateStream(bind)")) {
    return false;
  }

  aerogpu_handle_t tex_handle = 0;
  std::vector<aerogpu_cmd_create_texture_view> create_views;
  std::vector<aerogpu_handle_t> ps_slot0_values;
  const aerogpu_cmd_set_render_targets* last_set_rt = nullptr;

  const size_t used0 = StreamBytesUsed(stream0.data(), stream0.size());
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= used0) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(stream0.data() + offset);
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > used0 - offset) {
      break;
    }
    if (hdr->opcode == AEROGPU_CMD_CREATE_TEXTURE2D) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(hdr);
      tex_handle = cmd->texture_handle;
    } else if (hdr->opcode == AEROGPU_CMD_CREATE_TEXTURE_VIEW) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_create_texture_view*>(hdr);
      create_views.push_back(*cmd);
    } else if (hdr->opcode == AEROGPU_CMD_SET_TEXTURE) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
      if (cmd->shader_stage == AEROGPU_SHADER_STAGE_PIXEL && cmd->slot == 0) {
        ps_slot0_values.push_back(cmd->texture);
      }
    } else if (hdr->opcode == AEROGPU_CMD_SET_RENDER_TARGETS) {
      last_set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(hdr);
    }
    offset += hdr->size_bytes;
  }

  if (!Check(tex_handle != 0, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  if (!Check(create_views.size() == 2, "expected exactly 2 CREATE_TEXTURE_VIEW packets")) {
    return false;
  }

  auto view_handle_present = [&](aerogpu_handle_t h) -> bool {
    for (const auto& v : create_views) {
      if (v.view_handle == h) {
        return true;
      }
    }
    return false;
  };

  for (const auto& v : create_views) {
    if (!Check(v.texture_handle == tex_handle, "CREATE_TEXTURE_VIEW.texture_handle matches base texture")) {
      return false;
    }
    if (!Check(v.format == AEROGPU_FORMAT_B8G8R8A8_UNORM, "CREATE_TEXTURE_VIEW.format")) {
      return false;
    }
    if (!Check(v.base_mip_level == 1, "CREATE_TEXTURE_VIEW.base_mip_level")) {
      return false;
    }
    if (!Check(v.mip_level_count == 1, "CREATE_TEXTURE_VIEW.mip_level_count")) {
      return false;
    }
    if (!Check(v.base_array_layer == 1, "CREATE_TEXTURE_VIEW.base_array_layer")) {
      return false;
    }
    if (!Check(v.array_layer_count == 1, "CREATE_TEXTURE_VIEW.array_layer_count")) {
      return false;
    }
  }

  if (!Check(ps_slot0_values.size() >= 2, "expected >=2 PS slot0 SET_TEXTURE packets (bind + hazard unbind)")) {
    return false;
  }
  const aerogpu_handle_t bound_srv_handle = ps_slot0_values.front();
  if (!Check(bound_srv_handle != 0, "first PS slot0 SET_TEXTURE binds non-null SRV")) {
    return false;
  }
  if (!Check(view_handle_present(bound_srv_handle), "SET_TEXTURE uses a CREATE_TEXTURE_VIEW handle for SRV")) {
    return false;
  }
  bool saw_unbind = false;
  for (size_t i = 1; i < ps_slot0_values.size(); ++i) {
    if (ps_slot0_values[i] == 0) {
      saw_unbind = true;
      break;
    }
  }
  if (!Check(saw_unbind, "binding RTV unbound aliasing SRV (SET_TEXTURE texture=0)")) {
    return false;
  }

  if (!Check(last_set_rt != nullptr, "SET_RENDER_TARGETS emitted")) {
    return false;
  }
  const aerogpu_handle_t bound_rtv_handle = last_set_rt->colors[0];
  if (!Check(bound_rtv_handle != 0, "SET_RENDER_TARGETS.colors[0] non-null")) {
    return false;
  }
  if (!Check(bound_rtv_handle != tex_handle, "SET_RENDER_TARGETS binds view handle (not base texture handle)")) {
    return false;
  }
  if (!Check(view_handle_present(bound_rtv_handle), "SET_RENDER_TARGETS binds a CREATE_TEXTURE_VIEW handle")) {
    return false;
  }
  if (!Check(bound_rtv_handle != bound_srv_handle, "RTV and SRV view handles are distinct")) {
    return false;
  }

  // Now destroy views and ensure DESTROY_TEXTURE_VIEW packets are emitted.
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv.hRtv);
  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv.hSrv);
  const HRESULT flush_destroy_hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(flush_destroy_hr == S_OK, "Flush after destroying views")) {
    return false;
  }

  const std::vector<uint8_t> stream1 = dev.harness.last_stream;
  if (!Check(ValidateStream(stream1.data(), stream1.size()), "ValidateStream(destroy)")) {
    return false;
  }

  size_t destroy_count = 0;
  std::vector<aerogpu_handle_t> destroyed;
  const size_t used1 = StreamBytesUsed(stream1.data(), stream1.size());
  offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= used1) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(stream1.data() + offset);
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > used1 - offset) {
      break;
    }
    if (hdr->opcode == AEROGPU_CMD_DESTROY_TEXTURE_VIEW) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_destroy_texture_view*>(hdr);
      destroy_count++;
      destroyed.push_back(cmd->view_handle);
    }
    offset += hdr->size_bytes;
  }

  if (!Check(destroy_count == 2, "expected 2 DESTROY_TEXTURE_VIEW packets")) {
    return false;
  }
  if (!Check(destroyed.size() == 2, "destroy packet capture")) {
    return false;
  }
  if (!Check(view_handle_present(destroyed[0]) && view_handle_present(destroyed[1]),
             "DESTROY_TEXTURE_VIEW handles match previously created view handles")) {
    return false;
  }
  if (!Check(destroyed[0] != destroyed[1], "DESTROY_TEXTURE_VIEW handles are distinct")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

} // namespace

int main() {
  bool ok = true;
  ok &= TestTextureViewsEmitBindDestroy();
  return ok ? 0 : 1;
}
