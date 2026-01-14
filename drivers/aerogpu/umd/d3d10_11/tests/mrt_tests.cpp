#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

#include "aerogpu_d3d10_11_umd.h"
#include "aerogpu_d3d10_11_internal.h"
#include "aerogpu_cmd.h"

namespace {

using aerogpu::d3d10_11::kDxgiFormatB8G8R8A8Unorm;
using aerogpu::d3d10_11::kDxgiFormatD24UnormS8Uint;
using aerogpu::d3d10_11::kD3D11BindShaderResource;
using aerogpu::d3d10_11::kD3D11BindRenderTarget;
using aerogpu::d3d10_11::kD3D11BindDepthStencil;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

struct CmdLoc {
  const aerogpu_cmd_hdr* hdr = nullptr;
  size_t offset = 0;
};

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

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  const size_t stream_len = static_cast<size_t>(stream->size_bytes);
  while (offset < stream_len) {
    if (!Check(stream_len - offset >= sizeof(aerogpu_cmd_hdr), "packet header fits")) {
      return false;
    }
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (!Check(hdr->size_bytes >= sizeof(aerogpu_cmd_hdr), "packet size >= header")) {
      return false;
    }
    if (!Check((hdr->size_bytes & 3u) == 0, "packet size 4-byte aligned")) {
      return false;
    }
    if (!Check(hdr->size_bytes <= stream_len - offset, "packet within stream")) {
      return false;
    }
    offset += hdr->size_bytes;
  }
  return Check(offset == stream_len, "parser consumed stream");
}

CmdLoc FindLastOpcode(const uint8_t* buf, size_t len, uint32_t opcode) {
  CmdLoc loc{};
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return loc;
  }
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  const size_t stream_len = (stream->size_bytes >= sizeof(aerogpu_cmd_stream_header) && stream->size_bytes <= len)
                                ? static_cast<size_t>(stream->size_bytes)
                                : len;

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      loc.hdr = hdr;
      loc.offset = offset;
    }
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return loc;
}

std::vector<aerogpu_handle_t> CollectCreateTexture2DHandles(const uint8_t* buf, size_t len) {
  std::vector<aerogpu_handle_t> handles;
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return handles;
  }
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  const size_t stream_len = (stream->size_bytes >= sizeof(aerogpu_cmd_stream_header) && stream->size_bytes <= len)
                                ? static_cast<size_t>(stream->size_bytes)
                                : len;

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_CREATE_TEXTURE2D) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(hdr);
      handles.push_back(cmd->texture_handle);
    }
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return handles;
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
    auto* h = reinterpret_cast<Harness*>(user);
    h->errors.push_back(hr);
  }
};

struct TestDevice {
  D3D10DDI_ADAPTERFUNCS adapter_funcs{};
  AEROGPU_D3D10_11_DEVICEFUNCS device_funcs{};
  AEROGPU_D3D10_11_DEVICECALLBACKS callbacks{};
  Harness harness;

  D3D10DDI_HADAPTER hAdapter{};
  D3D10DDI_HDEVICE hDevice{};
  std::vector<uint8_t> device_mem;
};

struct TestResource {
  D3D10DDI_HRESOURCE hResource{};
  std::vector<uint8_t> storage;
};

struct TestRtv {
  D3D10DDI_HRENDERTARGETVIEW hRtv{};
  std::vector<uint8_t> storage;
};

struct TestDsv {
  D3D10DDI_HDEPTHSTENCILVIEW hDsv{};
  std::vector<uint8_t> storage;
};

struct TestSrv {
  D3D10DDI_HSHADERRESOURCEVIEW hSrv{};
  std::vector<uint8_t> storage;
};

bool CreateDevice(TestDevice* out) {
  if (!out) {
    return false;
  }
  out->callbacks.pUserContext = &out->harness;
  out->callbacks.pfnSubmitCmdStream = &Harness::SubmitCmdStream;
  out->callbacks.pfnSetError = &Harness::SetError;

  D3D10DDIARG_OPENADAPTER open{};
  open.pAdapterFuncs = &out->adapter_funcs;
  HRESULT hr = OpenAdapter10(&open);
  if (!Check(hr == S_OK, "OpenAdapter10")) {
    return false;
  }
  out->hAdapter = open.hAdapter;

  D3D10DDIARG_CREATEDEVICE create{};
  create.hDevice.pDrvPrivate = nullptr;
  const SIZE_T dev_size = out->adapter_funcs.pfnCalcPrivateDeviceSize(out->hAdapter, &create);
  if (!Check(dev_size >= sizeof(void*), "CalcPrivateDeviceSize returned non-trivial size")) {
    return false;
  }
  out->device_mem.assign(static_cast<size_t>(dev_size), 0);
  create.hDevice.pDrvPrivate = out->device_mem.data();
  create.pDeviceFuncs = &out->device_funcs;
  create.pDeviceCallbacks = &out->callbacks;

  hr = out->adapter_funcs.pfnCreateDevice(out->hAdapter, &create);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  out->hDevice = create.hDevice;
  return true;
}

bool CreateTexture2D(TestDevice* dev,
                     uint32_t bind_flags,
                     uint32_t format,
                     uint32_t width,
                     uint32_t height,
                     TestResource* out) {
  if (!dev || !out) {
    return false;
  }

  AEROGPU_DDIARG_CREATERESOURCE desc{};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  desc.BindFlags = bind_flags;
  desc.MiscFlags = 0;
  desc.Usage = AEROGPU_D3D11_USAGE_DEFAULT;
  desc.CPUAccessFlags = 0;
  desc.Width = width;
  desc.Height = height;
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = format;
  desc.pInitialData = nullptr;
  desc.InitialDataCount = 0;
  desc.SampleDescCount = 1;
  desc.SampleDescQuality = 0;
  desc.ResourceFlags = 0;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned non-trivial size")) {
    return false;
  }
  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  return Check(hr == S_OK, "CreateResource(tex2d)");
}

bool CreateRenderTargetTexture2D(TestDevice* dev, uint32_t width, uint32_t height, TestResource* out) {
  return CreateTexture2D(dev, kD3D11BindRenderTarget, kDxgiFormatB8G8R8A8Unorm, width, height, out);
}

bool CreateRTV(TestDevice* dev, const TestResource* res, TestRtv* out) {
  if (!dev || !res || !out) {
    return false;
  }
  AEROGPU_DDIARG_CREATERENDERTARGETVIEW desc{};
  desc.hResource = res->hResource;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateRTVSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateRTVSize returned non-trivial size")) {
    return false;
  }
  out->storage.assign(static_cast<size_t>(size), 0);
  out->hRtv.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateRTV(dev->hDevice, &desc, out->hRtv);
  return Check(hr == S_OK, "CreateRTV");
}

bool CreateDSV(TestDevice* dev, const TestResource* res, TestDsv* out) {
  if (!dev || !res || !out) {
    return false;
  }
  AEROGPU_DDIARG_CREATEDEPTHSTENCILVIEW desc{};
  desc.hResource = res->hResource;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateDSVSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateDSVSize returned non-trivial size")) {
    return false;
  }
  out->storage.assign(static_cast<size_t>(size), 0);
  out->hDsv.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateDSV(dev->hDevice, &desc, out->hDsv);
  return Check(hr == S_OK, "CreateDSV");
}

bool CreateSRV(TestDevice* dev, const TestResource* res, TestSrv* out) {
  if (!dev || !res || !out) {
    return false;
  }
  AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW desc{};
  desc.hResource = res->hResource;
  desc.Format = 0; // use resource format
  desc.ViewDimension = AEROGPU_DDI_SRV_DIMENSION_TEXTURE2D;
  desc.MostDetailedMip = 0;
  desc.MipLevels = 1;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateShaderResourceViewSize(dev->hDevice, &desc);
  if (!Check(size != 0, "CalcPrivateShaderResourceViewSize returned non-zero size")) {
    return false;
  }
  out->storage.assign(static_cast<size_t>(size), 0);
  out->hSrv.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateShaderResourceView(dev->hDevice, &desc, out->hSrv);
  return Check(hr == S_OK, "CreateShaderResourceView");
}

bool TestSetRenderTargetsEncodesMrtAndClamps() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  // Create more than the protocol max so we can validate clamping to
  // AEROGPU_MAX_RENDER_TARGETS.
  constexpr uint32_t kRequestedRtvs = AEROGPU_MAX_RENDER_TARGETS + 1;
  std::vector<TestResource> textures(kRequestedRtvs);
  std::vector<TestRtv> rtvs(kRequestedRtvs);

  for (uint32_t i = 0; i < kRequestedRtvs; ++i) {
    if (!CreateRenderTargetTexture2D(&dev, /*width=*/4, /*height=*/4, &textures[i])) {
      return false;
    }
    if (!CreateRTV(&dev, &textures[i], &rtvs[i])) {
      return false;
    }
  }

  std::vector<D3D10DDI_HRENDERTARGETVIEW> rtv_handles;
  rtv_handles.reserve(kRequestedRtvs);
  for (const auto& r : rtvs) {
    rtv_handles.push_back(r.hRtv);
  }

  D3D10DDI_HDEPTHSTENCILVIEW null_dsv{};
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice,
                                       kRequestedRtvs,
                                       rtv_handles.data(),
                                       null_dsv);

  const HRESULT flush_hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(flush_hr == S_OK, "Flush")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured")) {
    return false;
  }
  const uint8_t* buf = dev.harness.last_stream.data();
  const size_t len = dev.harness.last_stream.size();
  if (!ValidateStream(buf, len)) {
    return false;
  }

  const std::vector<aerogpu_handle_t> created = CollectCreateTexture2DHandles(buf, len);
  if (!Check(created.size() >= kRequestedRtvs, "captured CREATE_TEXTURE2D handles")) {
    return false;
  }

  const CmdLoc loc = FindLastOpcode(buf, len, AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);

  if (!Check(set_rt->color_count == AEROGPU_MAX_RENDER_TARGETS, "SET_RENDER_TARGETS color_count clamped to 8")) {
    return false;
  }
  if (!Check(set_rt->depth_stencil == 0, "SET_RENDER_TARGETS depth_stencil == 0")) {
    return false;
  }
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (!Check(set_rt->colors[i] == created[i], "SET_RENDER_TARGETS colors[i] matches created texture handle")) {
      return false;
    }
  }

  for (uint32_t i = 0; i < kRequestedRtvs; ++i) {
    dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtvs[i].hRtv);
    dev.device_funcs.pfnDestroyResource(dev.hDevice, textures[i].hResource);
  }
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetRenderTargetsPreservesNullEntries() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  TestResource tex0{};
  TestResource tex1{};
  TestRtv rtv0{};
  TestRtv rtv1{};

  if (!CreateRenderTargetTexture2D(&dev, /*width=*/4, /*height=*/4, &tex0) ||
      !CreateRenderTargetTexture2D(&dev, /*width=*/4, /*height=*/4, &tex1)) {
    return false;
  }
  if (!CreateRTV(&dev, &tex0, &rtv0) || !CreateRTV(&dev, &tex1, &rtv1)) {
    return false;
  }

  D3D10DDI_HRENDERTARGETVIEW rtvs[3] = {rtv0.hRtv, D3D10DDI_HRENDERTARGETVIEW{}, rtv1.hRtv};
  D3D10DDI_HDEPTHSTENCILVIEW null_dsv{};

  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/3, rtvs, null_dsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after SetRenderTargets with null)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after SetRenderTargets with null)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const std::vector<aerogpu_handle_t> created =
      CollectCreateTexture2DHandles(dev.harness.last_stream.data(), dev.harness.last_stream.size());
  if (!Check(created.size() >= 2, "captured CREATE_TEXTURE2D handles (2)")) {
    return false;
  }

  const CmdLoc loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after SetRenderTargets with null)")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
  if (!Check(set_rt->color_count == 3, "SET_RENDER_TARGETS color_count==3 (null slot preserved)")) {
    return false;
  }
  if (!Check(set_rt->colors[0] == created[0], "SET_RENDER_TARGETS colors[0] (null slot preserved)")) {
    return false;
  }
  if (!Check(set_rt->colors[1] == 0, "SET_RENDER_TARGETS colors[1]==0 (null slot)")) {
    return false;
  }
  if (!Check(set_rt->colors[2] == created[1], "SET_RENDER_TARGETS colors[2] (null slot preserved)")) {
    return false;
  }
  for (uint32_t i = 3; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (!Check(set_rt->colors[i] == 0, "SET_RENDER_TARGETS colors[i]==0 (trailing)")) {
      return false;
    }
  }

  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv0.hRtv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv1.hRtv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex0.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex1.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSrvBindingUnbindsOnlyAliasedRtv() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  TestResource tex0{};
  TestResource tex1{};
  TestRtv rtv0{};
  TestRtv rtv1{};
  TestSrv srv0{};

  if (!CreateRenderTargetTexture2D(&dev, /*width=*/4, /*height=*/4, &tex0) ||
      !CreateRenderTargetTexture2D(&dev, /*width=*/4, /*height=*/4, &tex1)) {
    return false;
  }
  if (!CreateRTV(&dev, &tex0, &rtv0) || !CreateRTV(&dev, &tex1, &rtv1)) {
    return false;
  }
  if (!CreateSRV(&dev, &tex0, &srv0)) {
    return false;
  }

  D3D10DDI_HRENDERTARGETVIEW rtvs[2] = {rtv0.hRtv, rtv1.hRtv};
  D3D10DDI_HDEPTHSTENCILVIEW null_dsv{};

  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/2, rtvs, null_dsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after SetRenderTargets)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after SetRenderTargets)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const std::vector<aerogpu_handle_t> created =
      CollectCreateTexture2DHandles(dev.harness.last_stream.data(), dev.harness.last_stream.size());
  if (!Check(created.size() >= 2, "captured CREATE_TEXTURE2D handles (2)")) {
    return false;
  }
  {
    const CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
    if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after SetRenderTargets)")) {
      return false;
    }
    const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
    if (!Check(set_rt->color_count == 2, "SET_RENDER_TARGETS color_count==2 (initial bind)")) {
      return false;
    }
    if (!Check(set_rt->colors[0] == created[0], "SET_RENDER_TARGETS colors[0] (initial bind)")) {
      return false;
    }
    if (!Check(set_rt->colors[1] == created[1], "SET_RENDER_TARGETS colors[1] (initial bind)")) {
      return false;
    }
  }

  // Binding a SRV that aliases RTV[0] must unbind RTV[0], but should preserve
  // RTV[1] (null entries are encoded in SET_RENDER_TARGETS.colors[]).
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv0.hSrv};
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after PSSetShaderResources)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after PSSetShaderResources)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after PSSetShaderResources)")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
  if (!Check(set_rt->color_count == 2, "SET_RENDER_TARGETS color_count==2 (RTV[1] preserved)")) {
    return false;
  }
  if (!Check(set_rt->colors[0] == 0, "SET_RENDER_TARGETS colors[0]==0 (aliased RTV[0] unbound)")) {
    return false;
  }
  if (!Check(set_rt->colors[1] == created[1], "SET_RENDER_TARGETS colors[1]==RTV[1] (preserved)")) {
    return false;
  }
  for (uint32_t i = 2; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (!Check(set_rt->colors[i] == 0, "SET_RENDER_TARGETS colors[i]==0 (trailing)")) {
      return false;
    }
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv0.hSrv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv0.hRtv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv1.hRtv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex0.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex1.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSrvBindingUnbindsAliasedDsv() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  TestResource depth{};
  TestDsv dsv{};
  TestSrv srv{};

  if (!CreateTexture2D(&dev,
                       /*bind_flags=*/kD3D11BindDepthStencil | kD3D11BindShaderResource,
                       /*format=*/kDxgiFormatD24UnormS8Uint,
                       /*width=*/4,
                       /*height=*/4,
                       &depth)) {
    return false;
  }
  if (!CreateDSV(&dev, &depth, &dsv)) {
    return false;
  }
  if (!CreateSRV(&dev, &depth, &srv)) {
    return false;
  }

  // Bind only DSV, no RTVs.
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/0, /*pViews=*/nullptr, dsv.hDsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after SetRenderTargets DSV-only)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after SetRenderTargets DSV-only)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const std::vector<aerogpu_handle_t> created =
      CollectCreateTexture2DHandles(dev.harness.last_stream.data(), dev.harness.last_stream.size());
  if (!Check(created.size() >= 1, "captured CREATE_TEXTURE2D handles (1)")) {
    return false;
  }

  {
    const CmdLoc loc =
        FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
    if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after DSV-only bind)")) {
      return false;
    }
    const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
    if (!Check(set_rt->color_count == 0, "SET_RENDER_TARGETS color_count==0 (DSV-only bind)")) {
      return false;
    }
    if (!Check(set_rt->depth_stencil == created[0], "SET_RENDER_TARGETS depth_stencil matches created texture handle")) {
      return false;
    }
  }

  // Binding a SRV that aliases the DSV must unbind the DSV.
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv.hSrv};
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after PSSetShaderResources alias DSV)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after PSSetShaderResources alias DSV)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after PSSetShaderResources alias DSV)")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
  if (!Check(set_rt->color_count == 0, "SET_RENDER_TARGETS color_count==0 (DSV-only unbound)")) {
    return false;
  }
  if (!Check(set_rt->depth_stencil == 0, "SET_RENDER_TARGETS depth_stencil==0 (aliased DSV unbound)")) {
    return false;
  }
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (!Check(set_rt->colors[i] == 0, "SET_RENDER_TARGETS colors[i]==0 (DSV-only unbound)")) {
      return false;
    }
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv.hSrv);
  dev.device_funcs.pfnDestroyDSV(dev.hDevice, dsv.hDsv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, depth.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

} // namespace

int main() {
  bool ok = true;
  ok &= TestSetRenderTargetsEncodesMrtAndClamps();
  ok &= TestSetRenderTargetsPreservesNullEntries();
  ok &= TestSrvBindingUnbindsOnlyAliasedRtv();
  ok &= TestSrvBindingUnbindsAliasedDsv();
  if (!ok) {
    return 1;
  }
  std::fprintf(stderr, "PASS: aerogpu_d3d10_11_mrt_tests\n");
  return 0;
}
