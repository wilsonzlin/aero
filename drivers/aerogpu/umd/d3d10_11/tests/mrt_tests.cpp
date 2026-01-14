#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <utility>
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

const AEROGPU_WDDM_SUBMIT_ALLOCATION* FindSubmitAlloc(const std::vector<AEROGPU_WDDM_SUBMIT_ALLOCATION>& allocs,
                                                      AEROGPU_WDDM_ALLOCATION_HANDLE handle) {
  for (const auto& a : allocs) {
    if (a.handle == handle) {
      return &a;
    }
  }
  return nullptr;
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

CmdLoc FindLastSetTexture(const uint8_t* buf, size_t len, uint32_t shader_stage, uint32_t slot) {
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
    if (hdr->opcode == AEROGPU_CMD_SET_TEXTURE && hdr->size_bytes >= sizeof(aerogpu_cmd_set_texture)) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_texture*>(hdr);
      if (cmd->shader_stage == shader_stage && cmd->slot == slot) {
        loc.hdr = hdr;
        loc.offset = offset;
      }
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
  std::vector<AEROGPU_WDDM_SUBMIT_ALLOCATION> last_allocs;
  std::vector<HRESULT> errors;

  struct Allocation {
    AEROGPU_WDDM_ALLOCATION_HANDLE handle = 0;
    std::vector<uint8_t> storage;
  };

  std::vector<AEROGPU_WDDM_ALLOCATION_HANDLE> alloc_sequence;
  size_t alloc_index = 0;
  std::vector<Allocation> allocations;

  static uint64_t EstimateAllocSizeBytes(const AEROGPU_DDIARG_CREATERESOURCE* pDesc) {
    if (!pDesc) {
      return 0;
    }
    if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER) {
      return pDesc->ByteWidth;
    }
    if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D) {
      const uint64_t width = pDesc->Width ? static_cast<uint64_t>(pDesc->Width) : 1ull;
      const uint64_t height = pDesc->Height ? static_cast<uint64_t>(pDesc->Height) : 1ull;
      const uint32_t mip_levels = pDesc->MipLevels ? pDesc->MipLevels : 1u;
      const uint32_t array_layers = pDesc->ArraySize ? pDesc->ArraySize : 1u;

      // Test harness sizing heuristic:
      // - Only used by unit tests in this directory.
      // - Current tests allocate B8G8R8A8 and D24S8 resources which are 4 bytes/texel.
      constexpr uint64_t bytes_per_texel = 4ull;

      uint64_t total = 0;
      uint64_t mip_w = width;
      uint64_t mip_h = height;
      for (uint32_t mip = 0; mip < mip_levels; ++mip) {
        if (mip_w > UINT64_MAX / bytes_per_texel) {
          return 0;
        }
        const uint64_t row_bytes = mip_w * bytes_per_texel;
        if (mip_h != 0 && row_bytes > UINT64_MAX / mip_h) {
          return 0;
        }
        const uint64_t mip_bytes = row_bytes * mip_h;
        if (mip_bytes > UINT64_MAX - total) {
          return 0;
        }
        total += mip_bytes;
        mip_w = std::max<uint64_t>(1ull, mip_w / 2ull);
        mip_h = std::max<uint64_t>(1ull, mip_h / 2ull);
      }
      if (array_layers != 0 && total > UINT64_MAX / static_cast<uint64_t>(array_layers)) {
        return 0;
      }
      return total * static_cast<uint64_t>(array_layers);
    }
    return 0;
  }

  static Allocation* FindAlloc(Harness* h, AEROGPU_WDDM_ALLOCATION_HANDLE handle) {
    if (!h || handle == 0) {
      return nullptr;
    }
    for (auto& alloc : h->allocations) {
      if (alloc.handle == handle) {
        return &alloc;
      }
    }
    return nullptr;
  }

  static HRESULT AEROGPU_APIENTRY AllocateBacking(void* user,
                                                  const AEROGPU_DDIARG_CREATERESOURCE* pDesc,
                                                  AEROGPU_WDDM_ALLOCATION_HANDLE* out_alloc_handle,
                                                  uint64_t* out_alloc_size_bytes,
                                                  uint32_t* out_row_pitch_bytes) {
    if (!user || !pDesc || !out_alloc_handle || !out_alloc_size_bytes || !out_row_pitch_bytes) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    if (h->alloc_index >= h->alloc_sequence.size()) {
      return E_FAIL;
    }
    const AEROGPU_WDDM_ALLOCATION_HANDLE handle = h->alloc_sequence[h->alloc_index++];
    if (handle == 0) {
      return E_FAIL;
    }

    uint64_t size_bytes = EstimateAllocSizeBytes(pDesc);
    if (size_bytes == 0) {
      // Fallback: keep tests robust if new formats are added.
      size_bytes = 4096;
    }
    if (size_bytes > static_cast<uint64_t>(SIZE_MAX)) {
      return E_OUTOFMEMORY;
    }

    *out_alloc_handle = handle;
    *out_alloc_size_bytes = size_bytes;
    *out_row_pitch_bytes = 0; // use the default row pitch computed by the UMD

    Allocation* alloc = FindAlloc(h, handle);
    if (!alloc) {
      Allocation a{};
      a.handle = handle;
      a.storage.assign(static_cast<size_t>(size_bytes), 0);
      h->allocations.push_back(std::move(a));
    } else if (alloc->storage.size() < static_cast<size_t>(size_bytes)) {
      alloc->storage.resize(static_cast<size_t>(size_bytes), 0);
    }
    return S_OK;
  }

  static HRESULT AEROGPU_APIENTRY MapAllocation(void* user,
                                                AEROGPU_WDDM_ALLOCATION_HANDLE alloc_handle,
                                                void** out_cpu_ptr) {
    if (!user || !out_cpu_ptr || alloc_handle == 0) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    Allocation* alloc = FindAlloc(h, alloc_handle);
    if (!alloc) {
      return E_FAIL;
    }
    if (alloc->storage.empty()) {
      alloc->storage.resize(4096, 0);
    }
    *out_cpu_ptr = alloc->storage.data();
    return S_OK;
  }

  static void AEROGPU_APIENTRY UnmapAllocation(void* user, AEROGPU_WDDM_ALLOCATION_HANDLE alloc_handle) {
    (void)user;
    (void)alloc_handle;
  }

  static HRESULT AEROGPU_APIENTRY SubmitCmdStream(void* user,
                                                  const void* cmd_stream,
                                                  uint32_t cmd_stream_size_bytes,
                                                  const AEROGPU_WDDM_SUBMIT_ALLOCATION* allocs,
                                                  uint32_t alloc_count,
                                                  uint64_t* out_fence) {
    if (!user || !cmd_stream || cmd_stream_size_bytes < sizeof(aerogpu_cmd_stream_header)) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    const auto* bytes = reinterpret_cast<const uint8_t*>(cmd_stream);
    h->last_stream.assign(bytes, bytes + cmd_stream_size_bytes);
    if (!allocs || alloc_count == 0) {
      h->last_allocs.clear();
    } else {
      h->last_allocs.assign(allocs, allocs + alloc_count);
    }
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

bool TestCreateSrvNotImplIsSafeToDestroy() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  // Force an older device ABI so texture-view opcodes are disabled. This keeps
  // the test deterministic even if the portable UMD defaults to a newer ABI
  // (where mip/array slicing is supported and MostDetailedMip != 0 would no
  // longer be an E_NOTIMPL condition).
  auto* adapter = reinterpret_cast<aerogpu::d3d10_11::Adapter*>(dev.hAdapter.pDrvPrivate);
  if (!Check(adapter != nullptr, "adapter private pointer")) {
    return false;
  }
  adapter->umd_private_valid = true;
  adapter->umd_private.device_abi_version_u32 = (AEROGPU_ABI_MAJOR << 16) | 3; // ABI 1.3 (no texture views)

  // Create a valid shader-resource texture.
  TestResource tex{};
  if (!CreateTexture2D(&dev, kD3D11BindShaderResource, kDxgiFormatB8G8R8A8Unorm, /*width=*/4, /*height=*/4, &tex)) {
    return false;
  }

  // Trigger E_NOTIMPL by requesting a view that slices mips.
  AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW desc{};
  desc.hResource = tex.hResource;
  desc.Format = 0; // use resource format
  desc.ViewDimension = AEROGPU_DDI_SRV_DIMENSION_TEXTURE2D;
  desc.MostDetailedMip = 1; // non-zero => E_NOTIMPL
  desc.MipLevels = 1;

  const SIZE_T size = dev.device_funcs.pfnCalcPrivateShaderResourceViewSize(dev.hDevice, &desc);
  if (!Check(size != 0, "CalcPrivateShaderResourceViewSize returned non-zero size")) {
    return false;
  }

  std::vector<uint8_t> storage(static_cast<size_t>(size), 0xCC);
  D3D10DDI_HSHADERRESOURCEVIEW hView{};
  hView.pDrvPrivate = storage.data();

  const HRESULT hr = dev.device_funcs.pfnCreateShaderResourceView(dev.hDevice, &desc, hView);
  if (!Check(hr == E_NOTIMPL, "CreateShaderResourceView should return E_NOTIMPL for MostDetailedMip != 0")) {
    return false;
  }

  // Even on failure, the view should be constructed so that Destroy is safe.
  if (!Check(storage.size() >= sizeof(void*) + sizeof(aerogpu_handle_t), "srv storage has expected size")) {
    return false;
  }
  void* expected_resource = nullptr;
  aerogpu_handle_t expected_handle = 0;
  if (!Check(std::memcmp(storage.data(), &expected_resource, sizeof(expected_resource)) == 0, "srv resource ptr initialized to null on failure")) {
    return false;
  }
  if (!Check(std::memcmp(storage.data() + sizeof(void*), &expected_handle, sizeof(expected_handle)) == 0,
             "srv handle initialized to 0 on failure")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, hView);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
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

  const CmdLoc ps_tex_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_PIXEL, /*slot=*/0);
  if (!Check(ps_tex_loc.hdr != nullptr, "SET_TEXTURE present (PS slot 0) after PSSetShaderResources")) {
    return false;
  }
  const auto* set_ps = reinterpret_cast<const aerogpu_cmd_set_texture*>(ps_tex_loc.hdr);
  if (!Check(set_ps->texture == created[0], "PS slot0 bound to SRV texture handle")) {
    return false;
  }
  if (!Check(loc.offset < ps_tex_loc.offset, "hazard unbind (SET_RENDER_TARGETS) happens before PS SRV bind")) {
    return false;
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

bool TestSrvBindingUnbindsOnlyAllocAliasedRtv() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  dev.callbacks.pfnAllocateBacking = &Harness::AllocateBacking;
  dev.callbacks.pfnMapAllocation = &Harness::MapAllocation;
  dev.callbacks.pfnUnmapAllocation = &Harness::UnmapAllocation;
  dev.harness.alloc_sequence = {100, 101, 100}; // tex0 and tex_alias share allocation

  TestResource tex0{};
  TestResource tex1{};
  TestResource tex_alias{};
  TestRtv rtv0{};
  TestRtv rtv1{};
  TestSrv srv_alias{};

  if (!CreateRenderTargetTexture2D(&dev, /*width=*/4, /*height=*/4, &tex0) ||
      !CreateRenderTargetTexture2D(&dev, /*width=*/4, /*height=*/4, &tex1) ||
      !CreateTexture2D(&dev, kD3D11BindShaderResource, kDxgiFormatB8G8R8A8Unorm, /*width=*/4, /*height=*/4, &tex_alias)) {
    return false;
  }
  if (!CreateRTV(&dev, &tex0, &rtv0) || !CreateRTV(&dev, &tex1, &rtv1)) {
    return false;
  }
  if (!CreateSRV(&dev, &tex_alias, &srv_alias)) {
    return false;
  }

  // Bind MRTs first.
  D3D10DDI_HRENDERTARGETVIEW rtvs[2] = {rtv0.hRtv, rtv1.hRtv};
  D3D10DDI_HDEPTHSTENCILVIEW null_dsv{};
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/2, rtvs, null_dsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after SetRenderTargets alloc-aliased)")) {
    return false;
  }

  std::vector<uint8_t> first_stream = dev.harness.last_stream;
  if (!Check(!first_stream.empty(), "submission captured (after SetRenderTargets alloc-aliased)")) {
    return false;
  }
  if (!ValidateStream(first_stream.data(), first_stream.size())) {
    return false;
  }
  const std::vector<aerogpu_handle_t> created = CollectCreateTexture2DHandles(first_stream.data(), first_stream.size());
  if (!Check(created.size() >= 3, "captured CREATE_TEXTURE2D handles (3 alloc-aliased)")) {
    return false;
  }

  // Binding an SRV whose underlying allocation aliases RTV[0] must unbind RTV[0],
  // but should preserve RTV[1].
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv_alias.hSrv};
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after PSSetShaderResources alloc-aliased)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after PSSetShaderResources alloc-aliased)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after PSSetShaderResources alloc-aliased)")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
  if (!Check(set_rt->color_count == 2, "SET_RENDER_TARGETS color_count==2 (alloc-aliased unbind)")) {
    return false;
  }
  if (!Check(set_rt->colors[0] == 0, "SET_RENDER_TARGETS colors[0]==0 (alloc-aliased RTV[0] unbound)")) {
    return false;
  }
  if (!Check(set_rt->colors[1] == created[1], "SET_RENDER_TARGETS colors[1]==RTV[1] (alloc-aliased preserved)")) {
    return false;
  }
  for (uint32_t i = 2; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (!Check(set_rt->colors[i] == 0, "SET_RENDER_TARGETS colors[i]==0 (alloc-aliased trailing)")) {
      return false;
    }
  }

  const CmdLoc ps_tex_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_PIXEL, /*slot=*/0);
  if (!Check(ps_tex_loc.hdr != nullptr, "SET_TEXTURE present (PS slot 0) after PSSetShaderResources alloc-aliased")) {
    return false;
  }
  const auto* set_ps = reinterpret_cast<const aerogpu_cmd_set_texture*>(ps_tex_loc.hdr);
  if (!Check(set_ps->texture == created[2], "PS slot0 bound to alloc-aliased SRV texture handle")) {
    return false;
  }
  if (!Check(loc.offset < ps_tex_loc.offset, "alloc-aliased hazard unbind (SET_RENDER_TARGETS) happens before PS SRV bind")) {
    return false;
  }

  const auto* alloc100 = FindSubmitAlloc(dev.harness.last_allocs, 100);
  if (!Check(alloc100 != nullptr, "alloc list contains handle 100 (SRV/RTV alias)")) {
    return false;
  }
  if (!Check(alloc100->write == 0, "alloc 100 marked read-only after RTV[0] hazard unbind")) {
    return false;
  }
  const auto* alloc101 = FindSubmitAlloc(dev.harness.last_allocs, 101);
  if (!Check(alloc101 != nullptr, "alloc list contains handle 101 (RTV[1])")) {
    return false;
  }
  if (!Check(alloc101->write == 1, "alloc 101 marked write (RTV[1] still bound)")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv_alias.hSrv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv0.hRtv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv1.hRtv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex_alias.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex0.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex1.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetRenderTargetsUnbindsAliasedSrvsForMrt() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  TestResource tex0{};
  TestResource tex1{};
  TestRtv rtv0{};
  TestRtv rtv1{};
  TestSrv srv1{};

  const uint32_t bind_flags = kD3D11BindRenderTarget | kD3D11BindShaderResource;
  if (!CreateTexture2D(&dev, bind_flags, kDxgiFormatB8G8R8A8Unorm, /*width=*/4, /*height=*/4, &tex0) ||
      !CreateTexture2D(&dev, bind_flags, kDxgiFormatB8G8R8A8Unorm, /*width=*/4, /*height=*/4, &tex1)) {
    return false;
  }
  if (!CreateRTV(&dev, &tex0, &rtv0) || !CreateRTV(&dev, &tex1, &rtv1)) {
    return false;
  }
  if (!CreateSRV(&dev, &tex1, &srv1)) {
    return false;
  }

  // Bind the aliased SRV first (both VS and PS). Binding the resource as an
  // output later must evict SRVs across all stages.
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv1.hSrv};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after PSSetShaderResources bind SRV)")) {
    return false;
  }

  std::vector<uint8_t> first_stream = dev.harness.last_stream;
  if (!Check(!first_stream.empty(), "submission captured (after bind SRV)")) {
    return false;
  }
  if (!ValidateStream(first_stream.data(), first_stream.size())) {
    return false;
  }
  const std::vector<aerogpu_handle_t> created = CollectCreateTexture2DHandles(first_stream.data(), first_stream.size());
  if (!Check(created.size() >= 2, "captured CREATE_TEXTURE2D handles (2)")) {
    return false;
  }

  // Binding the resource as RTV[1] must unbind the SRV first.
  D3D10DDI_HRENDERTARGETVIEW rtvs[2] = {rtv0.hRtv, rtv1.hRtv};
  D3D10DDI_HDEPTHSTENCILVIEW null_dsv{};
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/2, rtvs, null_dsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after SetRenderTargets MRT)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after SetRenderTargets MRT)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc vs_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_VERTEX, /*slot=*/0);
  if (!Check(vs_loc.hdr != nullptr, "SET_TEXTURE present (VS slot 0) after SetRenderTargets")) {
    return false;
  }
  const auto* set_vs = reinterpret_cast<const aerogpu_cmd_set_texture*>(vs_loc.hdr);
  if (!Check(set_vs->texture == 0, "VS SRV slot 0 unbound before MRT bind")) {
    return false;
  }

  const CmdLoc ps_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_PIXEL, /*slot=*/0);
  if (!Check(ps_loc.hdr != nullptr, "SET_TEXTURE present (PS slot 0) after SetRenderTargets")) {
    return false;
  }
  const auto* set_ps = reinterpret_cast<const aerogpu_cmd_set_texture*>(ps_loc.hdr);
  if (!Check(set_ps->texture == 0, "PS SRV slot 0 unbound before MRT bind")) {
    return false;
  }

  const CmdLoc rt_loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(rt_loc.hdr != nullptr, "SET_RENDER_TARGETS present (after SetRenderTargets MRT)")) {
    return false;
  }
  if (!Check(vs_loc.offset < rt_loc.offset, "VS SRV unbind occurs before MRT bind")) {
    return false;
  }
  if (!Check(ps_loc.offset < rt_loc.offset, "PS SRV unbind occurs before MRT bind")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(rt_loc.hdr);
  if (!Check(set_rt->color_count == 2, "SET_RENDER_TARGETS color_count==2 (after MRT bind)")) {
    return false;
  }
  if (!Check(set_rt->colors[0] == created[0], "SET_RENDER_TARGETS colors[0] (after MRT bind)")) {
    return false;
  }
  if (!Check(set_rt->colors[1] == created[1], "SET_RENDER_TARGETS colors[1] (after MRT bind)")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv1.hSrv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv0.hRtv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv1.hRtv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex0.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex1.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetRenderTargetsUnbindsAliasedSrvsForDsv() {
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

  // Bind the aliased SRV first (both VS and PS). Binding the resource as a DSV
  // later must evict SRVs across all stages.
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv.hSrv};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after PSSetShaderResources bind depth SRV)")) {
    return false;
  }

  std::vector<uint8_t> first_stream = dev.harness.last_stream;
  if (!Check(!first_stream.empty(), "submission captured (after bind depth SRV)")) {
    return false;
  }
  if (!ValidateStream(first_stream.data(), first_stream.size())) {
    return false;
  }
  const std::vector<aerogpu_handle_t> created = CollectCreateTexture2DHandles(first_stream.data(), first_stream.size());
  if (!Check(created.size() >= 1, "captured CREATE_TEXTURE2D handles (depth)")) {
    return false;
  }

  // Binding the resource as the DSV must unbind the SRV first.
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/0, /*pViews=*/nullptr, dsv.hDsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after SetRenderTargets DSV)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after SetRenderTargets DSV)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc vs_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_VERTEX, /*slot=*/0);
  if (!Check(vs_loc.hdr != nullptr, "SET_TEXTURE present (VS slot 0) after SetRenderTargets DSV")) {
    return false;
  }
  const auto* set_vs = reinterpret_cast<const aerogpu_cmd_set_texture*>(vs_loc.hdr);
  if (!Check(set_vs->texture == 0, "VS SRV slot 0 unbound before DSV bind")) {
    return false;
  }

  const CmdLoc ps_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_PIXEL, /*slot=*/0);
  if (!Check(ps_loc.hdr != nullptr, "SET_TEXTURE present (PS slot 0) after SetRenderTargets DSV")) {
    return false;
  }
  const auto* set_ps = reinterpret_cast<const aerogpu_cmd_set_texture*>(ps_loc.hdr);
  if (!Check(set_ps->texture == 0, "PS SRV slot 0 unbound before DSV bind")) {
    return false;
  }

  const CmdLoc rt_loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(rt_loc.hdr != nullptr, "SET_RENDER_TARGETS present (after DSV bind)")) {
    return false;
  }
  if (!Check(vs_loc.offset < rt_loc.offset, "VS SRV unbind occurs before DSV bind")) {
    return false;
  }
  if (!Check(ps_loc.offset < rt_loc.offset, "PS SRV unbind occurs before DSV bind")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(rt_loc.hdr);
  if (!Check(set_rt->color_count == 0, "SET_RENDER_TARGETS color_count==0 (after DSV bind)")) {
    return false;
  }
  if (!Check(set_rt->depth_stencil == created[0], "SET_RENDER_TARGETS depth_stencil (after DSV bind)")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv.hSrv);
  dev.device_funcs.pfnDestroyDSV(dev.hDevice, dsv.hDsv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, depth.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetRenderTargetsUnbindsAllocAliasedSrvsForDsv() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  dev.callbacks.pfnAllocateBacking = &Harness::AllocateBacking;
  dev.callbacks.pfnMapAllocation = &Harness::MapAllocation;
  dev.callbacks.pfnUnmapAllocation = &Harness::UnmapAllocation;
  dev.harness.alloc_sequence = {200, 200}; // depth + alias share allocation

  TestResource depth{};
  TestResource depth_alias{};
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
  if (!CreateTexture2D(&dev,
                       /*bind_flags=*/kD3D11BindShaderResource,
                       /*format=*/kDxgiFormatD24UnormS8Uint,
                       /*width=*/4,
                       /*height=*/4,
                       &depth_alias)) {
    return false;
  }
  if (!CreateDSV(&dev, &depth, &dsv)) {
    return false;
  }
  if (!CreateSRV(&dev, &depth_alias, &srv)) {
    return false;
  }

  // Bind the aliased SRV first (both VS and PS). Binding the resource as a DSV
  // later must evict SRVs across all stages even if the SRV comes from a
  // different resource handle.
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv.hSrv};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after bind alloc-aliased depth SRV)")) {
    return false;
  }

  std::vector<uint8_t> first_stream = dev.harness.last_stream;
  if (!Check(!first_stream.empty(), "submission captured (after bind alloc-aliased depth SRV)")) {
    return false;
  }
  if (!ValidateStream(first_stream.data(), first_stream.size())) {
    return false;
  }
  const std::vector<aerogpu_handle_t> created = CollectCreateTexture2DHandles(first_stream.data(), first_stream.size());
  if (!Check(created.size() >= 2, "captured CREATE_TEXTURE2D handles (depth + alias)")) {
    return false;
  }

  // Binding the resource as the DSV must unbind the SRVs first.
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/0, /*pViews=*/nullptr, dsv.hDsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after SetRenderTargets alloc-aliased DSV)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after SetRenderTargets alloc-aliased DSV)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc vs_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_VERTEX, /*slot=*/0);
  if (!Check(vs_loc.hdr != nullptr, "SET_TEXTURE present (VS slot 0) after alloc-aliased SetRenderTargets DSV")) {
    return false;
  }
  const auto* set_vs = reinterpret_cast<const aerogpu_cmd_set_texture*>(vs_loc.hdr);
  if (!Check(set_vs->texture == 0, "VS SRV slot 0 unbound before alloc-aliased DSV bind")) {
    return false;
  }

  const CmdLoc ps_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_PIXEL, /*slot=*/0);
  if (!Check(ps_loc.hdr != nullptr, "SET_TEXTURE present (PS slot 0) after alloc-aliased SetRenderTargets DSV")) {
    return false;
  }
  const auto* set_ps = reinterpret_cast<const aerogpu_cmd_set_texture*>(ps_loc.hdr);
  if (!Check(set_ps->texture == 0, "PS SRV slot 0 unbound before alloc-aliased DSV bind")) {
    return false;
  }

  const CmdLoc rt_loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(rt_loc.hdr != nullptr, "SET_RENDER_TARGETS present (after alloc-aliased DSV bind)")) {
    return false;
  }
  if (!Check(vs_loc.offset < rt_loc.offset, "VS SRV unbind occurs before alloc-aliased DSV bind")) {
    return false;
  }
  if (!Check(ps_loc.offset < rt_loc.offset, "PS SRV unbind occurs before alloc-aliased DSV bind")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(rt_loc.hdr);
  if (!Check(set_rt->color_count == 0, "SET_RENDER_TARGETS color_count==0 (after alloc-aliased DSV bind)")) {
    return false;
  }
  if (!Check(set_rt->depth_stencil == created[0], "SET_RENDER_TARGETS depth_stencil (after alloc-aliased DSV bind)")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv.hSrv);
  dev.device_funcs.pfnDestroyDSV(dev.hDevice, dsv.hDsv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, depth_alias.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, depth.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetRenderTargetsUnbindsOnlyAliasedSrvs() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  TestResource tex0{};
  TestResource tex1{};
  TestRtv rtv1{};
  TestSrv srv0{};
  TestSrv srv1{};

  const uint32_t bind_flags = kD3D11BindRenderTarget | kD3D11BindShaderResource;
  if (!CreateTexture2D(&dev, bind_flags, kDxgiFormatB8G8R8A8Unorm, /*width=*/4, /*height=*/4, &tex0) ||
      !CreateTexture2D(&dev, bind_flags, kDxgiFormatB8G8R8A8Unorm, /*width=*/4, /*height=*/4, &tex1)) {
    return false;
  }
  if (!CreateRTV(&dev, &tex1, &rtv1)) {
    return false;
  }
  if (!CreateSRV(&dev, &tex0, &srv0) || !CreateSRV(&dev, &tex1, &srv1)) {
    return false;
  }

  // Bind SRVs in both stages:
  // - slot0 = tex0 (non-aliased)
  // - slot1 = tex1 (aliased with upcoming RTV bind)
  D3D10DDI_HSHADERRESOURCEVIEW srvs[2] = {srv0.hSrv, srv1.hSrv};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/2, srvs);
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/2, srvs);

  // Bind the aliased resource as an RTV. This must unbind SRVs that alias tex1,
  // but should leave tex0 SRVs untouched.
  D3D10DDI_HRENDERTARGETVIEW rtvs[1] = {rtv1.hRtv};
  D3D10DDI_HDEPTHSTENCILVIEW null_dsv{};
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/1, rtvs, null_dsv);

  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after combined SRV + SetRenderTargets)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after combined SRV + SetRenderTargets)")) {
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
  const aerogpu_handle_t handle_tex0 = created[0];
  const aerogpu_handle_t handle_tex1 = created[1];

  // tex0 should remain bound in slot0 for both stages.
  const CmdLoc vs0 = FindLastSetTexture(dev.harness.last_stream.data(),
                                       dev.harness.last_stream.size(),
                                       AEROGPU_SHADER_STAGE_VERTEX,
                                       /*slot=*/0);
  const CmdLoc ps0 = FindLastSetTexture(dev.harness.last_stream.data(),
                                       dev.harness.last_stream.size(),
                                       AEROGPU_SHADER_STAGE_PIXEL,
                                       /*slot=*/0);
  if (!Check(vs0.hdr != nullptr, "SET_TEXTURE present (VS slot0)")) {
    return false;
  }
  if (!Check(ps0.hdr != nullptr, "SET_TEXTURE present (PS slot0)")) {
    return false;
  }
  if (!Check(reinterpret_cast<const aerogpu_cmd_set_texture*>(vs0.hdr)->texture == handle_tex0,
             "VS slot0 remains bound to non-aliased tex0")) {
    return false;
  }
  if (!Check(reinterpret_cast<const aerogpu_cmd_set_texture*>(ps0.hdr)->texture == handle_tex0,
             "PS slot0 remains bound to non-aliased tex0")) {
    return false;
  }

  // tex1 must be unbound from slot1 for both stages.
  const CmdLoc vs1 = FindLastSetTexture(dev.harness.last_stream.data(),
                                       dev.harness.last_stream.size(),
                                       AEROGPU_SHADER_STAGE_VERTEX,
                                       /*slot=*/1);
  const CmdLoc ps1 = FindLastSetTexture(dev.harness.last_stream.data(),
                                       dev.harness.last_stream.size(),
                                       AEROGPU_SHADER_STAGE_PIXEL,
                                       /*slot=*/1);
  if (!Check(vs1.hdr != nullptr, "SET_TEXTURE present (VS slot1)")) {
    return false;
  }
  if (!Check(ps1.hdr != nullptr, "SET_TEXTURE present (PS slot1)")) {
    return false;
  }
  if (!Check(reinterpret_cast<const aerogpu_cmd_set_texture*>(vs1.hdr)->texture == 0,
             "VS slot1 unbound for aliased tex1")) {
    return false;
  }
  if (!Check(reinterpret_cast<const aerogpu_cmd_set_texture*>(ps1.hdr)->texture == 0,
             "PS slot1 unbound for aliased tex1")) {
    return false;
  }

  const CmdLoc rt_loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(rt_loc.hdr != nullptr, "SET_RENDER_TARGETS present (after combined bind)")) {
    return false;
  }
  if (!Check(vs1.offset < rt_loc.offset, "VS slot1 unbind occurs before RTV bind")) {
    return false;
  }
  if (!Check(ps1.offset < rt_loc.offset, "PS slot1 unbind occurs before RTV bind")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(rt_loc.hdr);
  if (!Check(set_rt->color_count == 1, "SET_RENDER_TARGETS color_count==1 (after combined bind)")) {
    return false;
  }
  if (!Check(set_rt->colors[0] == handle_tex1, "SET_RENDER_TARGETS colors[0]==tex1 (after combined bind)")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv1.hSrv);
  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv0.hSrv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv1.hRtv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex1.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex0.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSrvBindingUnbindsOnlyAliasedRtvVs() {
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

  // Binding a VS SRV that aliases RTV[0] must unbind RTV[0], but should preserve
  // RTV[1] (null entries are encoded in SET_RENDER_TARGETS.colors[]).
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv0.hSrv};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after VSSetShaderResources)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after VSSetShaderResources)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after VSSetShaderResources)")) {
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

  const CmdLoc vs_tex_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_VERTEX, /*slot=*/0);
  if (!Check(vs_tex_loc.hdr != nullptr, "SET_TEXTURE present (VS slot 0) after VSSetShaderResources")) {
    return false;
  }
  const auto* set_vs = reinterpret_cast<const aerogpu_cmd_set_texture*>(vs_tex_loc.hdr);
  if (!Check(set_vs->texture == created[0], "VS slot0 bound to SRV texture handle")) {
    return false;
  }
  if (!Check(loc.offset < vs_tex_loc.offset, "hazard unbind (SET_RENDER_TARGETS) happens before VS SRV bind")) {
    return false;
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

bool TestSrvBindingUnbindsOnlyAllocAliasedRtvVs() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  dev.callbacks.pfnAllocateBacking = &Harness::AllocateBacking;
  dev.callbacks.pfnMapAllocation = &Harness::MapAllocation;
  dev.callbacks.pfnUnmapAllocation = &Harness::UnmapAllocation;
  dev.harness.alloc_sequence = {100, 101, 100}; // tex0 and tex_alias share allocation

  TestResource tex0{};
  TestResource tex1{};
  TestResource tex_alias{};
  TestRtv rtv0{};
  TestRtv rtv1{};
  TestSrv srv_alias{};

  if (!CreateRenderTargetTexture2D(&dev, /*width=*/4, /*height=*/4, &tex0) ||
      !CreateRenderTargetTexture2D(&dev, /*width=*/4, /*height=*/4, &tex1) ||
      !CreateTexture2D(&dev, kD3D11BindShaderResource, kDxgiFormatB8G8R8A8Unorm, /*width=*/4, /*height=*/4, &tex_alias)) {
    return false;
  }
  if (!CreateRTV(&dev, &tex0, &rtv0) || !CreateRTV(&dev, &tex1, &rtv1)) {
    return false;
  }
  if (!CreateSRV(&dev, &tex_alias, &srv_alias)) {
    return false;
  }

  D3D10DDI_HRENDERTARGETVIEW rtvs[2] = {rtv0.hRtv, rtv1.hRtv};
  D3D10DDI_HDEPTHSTENCILVIEW null_dsv{};
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/2, rtvs, null_dsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after SetRenderTargets alloc-aliased VS)")) {
    return false;
  }

  std::vector<uint8_t> first_stream = dev.harness.last_stream;
  if (!Check(!first_stream.empty(), "submission captured (after SetRenderTargets alloc-aliased VS)")) {
    return false;
  }
  if (!ValidateStream(first_stream.data(), first_stream.size())) {
    return false;
  }
  const std::vector<aerogpu_handle_t> created = CollectCreateTexture2DHandles(first_stream.data(), first_stream.size());
  if (!Check(created.size() >= 3, "captured CREATE_TEXTURE2D handles (3 alloc-aliased VS)")) {
    return false;
  }

  // Binding a VS SRV whose allocation aliases RTV[0] must unbind RTV[0], but
  // should preserve RTV[1].
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv_alias.hSrv};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after VSSetShaderResources alloc-aliased)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after VSSetShaderResources alloc-aliased)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after VSSetShaderResources alloc-aliased)")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
  if (!Check(set_rt->color_count == 2, "SET_RENDER_TARGETS color_count==2 (alloc-aliased VS unbind)")) {
    return false;
  }
  if (!Check(set_rt->colors[0] == 0, "SET_RENDER_TARGETS colors[0]==0 (alloc-aliased VS RTV[0] unbound)")) {
    return false;
  }
  if (!Check(set_rt->colors[1] == created[1], "SET_RENDER_TARGETS colors[1]==RTV[1] (alloc-aliased VS preserved)")) {
    return false;
  }
  for (uint32_t i = 2; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (!Check(set_rt->colors[i] == 0, "SET_RENDER_TARGETS colors[i]==0 (alloc-aliased VS trailing)")) {
      return false;
    }
  }

  const CmdLoc vs_tex_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_VERTEX, /*slot=*/0);
  if (!Check(vs_tex_loc.hdr != nullptr, "SET_TEXTURE present (VS slot 0) after VSSetShaderResources alloc-aliased")) {
    return false;
  }
  const auto* set_vs = reinterpret_cast<const aerogpu_cmd_set_texture*>(vs_tex_loc.hdr);
  if (!Check(set_vs->texture == created[2], "VS slot0 bound to alloc-aliased SRV texture handle")) {
    return false;
  }
  if (!Check(loc.offset < vs_tex_loc.offset, "alloc-aliased hazard unbind (SET_RENDER_TARGETS) happens before VS SRV bind")) {
    return false;
  }

  const auto* alloc100 = FindSubmitAlloc(dev.harness.last_allocs, 100);
  if (!Check(alloc100 != nullptr, "alloc list contains handle 100 (VS SRV/RTV alias)")) {
    return false;
  }
  if (!Check(alloc100->write == 0, "alloc 100 marked read-only after VS RTV[0] hazard unbind")) {
    return false;
  }
  const auto* alloc101 = FindSubmitAlloc(dev.harness.last_allocs, 101);
  if (!Check(alloc101 != nullptr, "alloc list contains handle 101 (VS RTV[1])")) {
    return false;
  }
  if (!Check(alloc101->write == 1, "alloc 101 marked write (VS RTV[1] still bound)")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv_alias.hSrv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv0.hRtv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv1.hRtv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex_alias.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex0.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex1.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSrvBindingUnbindsAllAliasedRtvSlots() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  TestResource tex{};
  TestRtv rtv{};
  TestSrv srv{};

  if (!CreateRenderTargetTexture2D(&dev, /*width=*/4, /*height=*/4, &tex)) {
    return false;
  }
  if (!CreateRTV(&dev, &tex, &rtv)) {
    return false;
  }
  if (!CreateSRV(&dev, &tex, &srv)) {
    return false;
  }

  // Bind the same resource in multiple RTV slots.
  D3D10DDI_HRENDERTARGETVIEW rtvs[2] = {rtv.hRtv, rtv.hRtv};
  D3D10DDI_HDEPTHSTENCILVIEW null_dsv{};
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/2, rtvs, null_dsv);

  // Binding a SRV on the same resource must unbind it from *all* RTV slots.
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv.hSrv};
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);

  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after duplicate RTV + PS SRV bind)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after duplicate RTV + PS SRV bind)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(),
                                    dev.harness.last_stream.size(),
                                    AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after SRV bind)")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
  if (!Check(set_rt->color_count == 2, "color_count preserved when unbinding duplicate RTV slots")) {
    return false;
  }
  if (!Check(set_rt->colors[0] == 0, "colors[0]==0 after unbinding duplicate RTV slots")) {
    return false;
  }
  if (!Check(set_rt->colors[1] == 0, "colors[1]==0 after unbinding duplicate RTV slots")) {
    return false;
  }

  const std::vector<aerogpu_handle_t> created =
      CollectCreateTexture2DHandles(dev.harness.last_stream.data(), dev.harness.last_stream.size());
  if (!Check(created.size() >= 1, "captured CREATE_TEXTURE2D handles (1)")) {
    return false;
  }

  const CmdLoc ps_tex_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_PIXEL, /*slot=*/0);
  if (!Check(ps_tex_loc.hdr != nullptr, "SET_TEXTURE present (PS slot0) after SRV bind")) {
    return false;
  }
  const auto* set_ps = reinterpret_cast<const aerogpu_cmd_set_texture*>(ps_tex_loc.hdr);
  if (!Check(set_ps->texture == created[0], "PS slot0 bound to SRV texture handle")) {
    return false;
  }
  if (!Check(loc.offset < ps_tex_loc.offset, "hazard unbind (SET_RENDER_TARGETS) happens before PS SRV bind")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv.hSrv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv.hRtv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSrvBindingUnbindsAllAllocAliasedRtvSlots() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  dev.callbacks.pfnAllocateBacking = &Harness::AllocateBacking;
  dev.callbacks.pfnMapAllocation = &Harness::MapAllocation;
  dev.callbacks.pfnUnmapAllocation = &Harness::UnmapAllocation;
  dev.harness.alloc_sequence = {100, 100}; // both RTVs share the same allocation

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

  // Bind two RTVs that alias the same backing allocation.
  D3D10DDI_HRENDERTARGETVIEW rtvs[2] = {rtv0.hRtv, rtv1.hRtv};
  D3D10DDI_HDEPTHSTENCILVIEW null_dsv{};
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/2, rtvs, null_dsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after SetRenderTargets alloc-aliased RTVs)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after SetRenderTargets alloc-aliased RTVs)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }
  const std::vector<aerogpu_handle_t> created =
      CollectCreateTexture2DHandles(dev.harness.last_stream.data(), dev.harness.last_stream.size());
  if (!Check(created.size() >= 2, "captured CREATE_TEXTURE2D handles (2 alloc-aliased RTVs)")) {
    return false;
  }
  const aerogpu_handle_t handle0 = created[0];

  // Binding an SRV that aliases the backing allocation must unbind *both* RTV slots.
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv0.hSrv};
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after PSSetShaderResources alloc-aliased RTVs)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after PSSetShaderResources alloc-aliased RTVs)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc rt_loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(rt_loc.hdr != nullptr, "SET_RENDER_TARGETS present (after alloc-aliased RTV unbind)")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(rt_loc.hdr);
  if (!Check(set_rt->color_count == 2, "color_count preserved when unbinding alloc-aliased RTV slots")) {
    return false;
  }
  if (!Check(set_rt->colors[0] == 0, "colors[0]==0 after unbinding alloc-aliased RTV slots")) {
    return false;
  }
  if (!Check(set_rt->colors[1] == 0, "colors[1]==0 after unbinding alloc-aliased RTV slots")) {
    return false;
  }
  for (uint32_t i = 2; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (!Check(set_rt->colors[i] == 0, "colors[i]==0 after unbinding alloc-aliased RTV slots")) {
      return false;
    }
  }

  const CmdLoc ps_tex_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_PIXEL, /*slot=*/0);
  if (!Check(ps_tex_loc.hdr != nullptr, "SET_TEXTURE present (PS slot0) after alloc-aliased RTV hazard unbind")) {
    return false;
  }
  const auto* set_ps = reinterpret_cast<const aerogpu_cmd_set_texture*>(ps_tex_loc.hdr);
  if (!Check(set_ps->texture == handle0, "PS slot0 bound to SRV texture handle (alloc-aliased)")) {
    return false;
  }
  if (!Check(rt_loc.offset < ps_tex_loc.offset, "alloc-aliased RTV unbind happens before PS SRV bind")) {
    return false;
  }

  const auto* alloc100 = FindSubmitAlloc(dev.harness.last_allocs, 100);
  if (!Check(alloc100 != nullptr, "alloc list contains handle 100 (alloc-aliased RTVs)")) {
    return false;
  }
  if (!Check(alloc100->write == 0, "alloc 100 marked read-only after unbinding alloc-aliased RTVs")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv0.hSrv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv0.hRtv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv1.hRtv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex1.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex0.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestRotateResourceIdentitiesRemapsSrvsAndViews() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  TestResource a{};
  TestResource b{};
  TestResource c{};
  TestSrv srv_a{};
  TestSrv srv_b{};

  const uint32_t bind_flags = kD3D11BindRenderTarget | kD3D11BindShaderResource;
  if (!CreateTexture2D(&dev, bind_flags, kDxgiFormatB8G8R8A8Unorm, /*width=*/4, /*height=*/4, &a) ||
      !CreateTexture2D(&dev, bind_flags, kDxgiFormatB8G8R8A8Unorm, /*width=*/4, /*height=*/4, &b) ||
      !CreateTexture2D(&dev, bind_flags, kDxgiFormatB8G8R8A8Unorm, /*width=*/4, /*height=*/4, &c)) {
    return false;
  }
  if (!CreateSRV(&dev, &a, &srv_a) || !CreateSRV(&dev, &b, &srv_b)) {
    return false;
  }

  // Bind SRVs to VS/PS slots 0..1.
  D3D10DDI_HSHADERRESOURCEVIEW srvs[2] = {srv_a.hSrv, srv_b.hSrv};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/2, srvs);
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/2, srvs);

  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after initial SRV bind)")) {
    return false;
  }
  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after initial SRV bind)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }
  const std::vector<aerogpu_handle_t> created =
      CollectCreateTexture2DHandles(dev.harness.last_stream.data(), dev.harness.last_stream.size());
  if (!Check(created.size() >= 3, "captured CREATE_TEXTURE2D handles (>=3)")) {
    return false;
  }
  const aerogpu_handle_t handle_a = created[created.size() - 3];
  const aerogpu_handle_t handle_b = created[created.size() - 2];
  const aerogpu_handle_t handle_c = created[created.size() - 1];

  // Rotate [A, B, C] so A takes B's identity and B takes C's identity.
  D3D10DDI_HRESOURCE rotation[3] = {a.hResource, b.hResource, c.hResource};
  dev.device_funcs.pfnRotateResourceIdentities(dev.hDevice, rotation, 3);

  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after RotateResourceIdentities)")) {
    return false;
  }
  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after RotateResourceIdentities)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  // SRV slots should be remapped:
  // - slot0 was handle_a -> now handle_b
  // - slot1 was handle_b -> now handle_c
  const CmdLoc vs0 = FindLastSetTexture(dev.harness.last_stream.data(),
                                       dev.harness.last_stream.size(),
                                       AEROGPU_SHADER_STAGE_VERTEX,
                                       /*slot=*/0);
  const CmdLoc vs1 = FindLastSetTexture(dev.harness.last_stream.data(),
                                       dev.harness.last_stream.size(),
                                       AEROGPU_SHADER_STAGE_VERTEX,
                                       /*slot=*/1);
  const CmdLoc ps0 = FindLastSetTexture(dev.harness.last_stream.data(),
                                       dev.harness.last_stream.size(),
                                       AEROGPU_SHADER_STAGE_PIXEL,
                                       /*slot=*/0);
  const CmdLoc ps1 = FindLastSetTexture(dev.harness.last_stream.data(),
                                       dev.harness.last_stream.size(),
                                       AEROGPU_SHADER_STAGE_PIXEL,
                                       /*slot=*/1);
  if (!Check(vs0.hdr && vs1.hdr && ps0.hdr && ps1.hdr, "SET_TEXTURE present for VS/PS slots 0..1 after rotation")) {
    return false;
  }
  if (!Check(reinterpret_cast<const aerogpu_cmd_set_texture*>(vs0.hdr)->texture == handle_b, "VS slot0 remapped to B")) {
    return false;
  }
  if (!Check(reinterpret_cast<const aerogpu_cmd_set_texture*>(vs1.hdr)->texture == handle_c, "VS slot1 remapped to C")) {
    return false;
  }
  if (!Check(reinterpret_cast<const aerogpu_cmd_set_texture*>(ps0.hdr)->texture == handle_b, "PS slot0 remapped to B")) {
    return false;
  }
  if (!Check(reinterpret_cast<const aerogpu_cmd_set_texture*>(ps1.hdr)->texture == handle_c, "PS slot1 remapped to C")) {
    return false;
  }

  // Now unbind the SRV slots and rebind using the *same SRV view handles*. The
  // SRV view implementation should follow the rotated resource handle (view ->
  // resource pointer), not the pre-rotation handle snapshot.
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/2, /*pViews=*/nullptr);
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/2, /*pViews=*/nullptr);
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/2, srvs);
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/2, srvs);

  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after unbind + rebind SRV views post-rotation)")) {
    return false;
  }
  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after rebind SRV views post-rotation)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc vs0b = FindLastSetTexture(dev.harness.last_stream.data(),
                                        dev.harness.last_stream.size(),
                                        AEROGPU_SHADER_STAGE_VERTEX,
                                        /*slot=*/0);
  const CmdLoc vs1b = FindLastSetTexture(dev.harness.last_stream.data(),
                                        dev.harness.last_stream.size(),
                                        AEROGPU_SHADER_STAGE_VERTEX,
                                        /*slot=*/1);
  const CmdLoc ps0b = FindLastSetTexture(dev.harness.last_stream.data(),
                                        dev.harness.last_stream.size(),
                                        AEROGPU_SHADER_STAGE_PIXEL,
                                        /*slot=*/0);
  const CmdLoc ps1b = FindLastSetTexture(dev.harness.last_stream.data(),
                                        dev.harness.last_stream.size(),
                                        AEROGPU_SHADER_STAGE_PIXEL,
                                        /*slot=*/1);
  if (!Check(vs0b.hdr && vs1b.hdr && ps0b.hdr && ps1b.hdr, "SET_TEXTURE present after SRV view rebind")) {
    return false;
  }
  if (!Check(reinterpret_cast<const aerogpu_cmd_set_texture*>(vs0b.hdr)->texture == handle_b,
             "VS slot0 rebind uses rotated handle (B)")) {
    return false;
  }
  if (!Check(reinterpret_cast<const aerogpu_cmd_set_texture*>(vs1b.hdr)->texture == handle_c,
             "VS slot1 rebind uses rotated handle (C)")) {
    return false;
  }
  if (!Check(reinterpret_cast<const aerogpu_cmd_set_texture*>(ps0b.hdr)->texture == handle_b,
             "PS slot0 rebind uses rotated handle (B)")) {
    return false;
  }
  if (!Check(reinterpret_cast<const aerogpu_cmd_set_texture*>(ps1b.hdr)->texture == handle_c,
             "PS slot1 rebind uses rotated handle (C)")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv_b.hSrv);
  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv_a.hSrv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, c.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, b.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, a.hResource);
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
  const CmdLoc ps_tex_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_PIXEL, /*slot=*/0);
  if (!Check(ps_tex_loc.hdr != nullptr, "SET_TEXTURE present (PS slot 0) after PSSetShaderResources alias DSV")) {
    return false;
  }
  const auto* set_ps = reinterpret_cast<const aerogpu_cmd_set_texture*>(ps_tex_loc.hdr);
  if (!Check(set_ps->texture == created[0], "PS slot0 bound to depth SRV handle")) {
    return false;
  }
  if (!Check(loc.offset < ps_tex_loc.offset, "hazard unbind (SET_RENDER_TARGETS) happens before PS SRV bind (DSV)")) {
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

bool TestSrvBindingUnbindsAllocAliasedDsv() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  dev.callbacks.pfnAllocateBacking = &Harness::AllocateBacking;
  dev.callbacks.pfnMapAllocation = &Harness::MapAllocation;
  dev.callbacks.pfnUnmapAllocation = &Harness::UnmapAllocation;
  dev.harness.alloc_sequence = {200, 200}; // depth + alias share allocation

  TestResource depth{};
  TestResource depth_alias{};
  TestDsv dsv{};
  TestSrv srv_alias{};

  if (!CreateTexture2D(&dev,
                       /*bind_flags=*/kD3D11BindDepthStencil | kD3D11BindShaderResource,
                       /*format=*/kDxgiFormatD24UnormS8Uint,
                       /*width=*/4,
                       /*height=*/4,
                       &depth)) {
    return false;
  }
  if (!CreateTexture2D(&dev,
                       /*bind_flags=*/kD3D11BindShaderResource,
                       /*format=*/kDxgiFormatD24UnormS8Uint,
                       /*width=*/4,
                       /*height=*/4,
                       &depth_alias)) {
    return false;
  }
  if (!CreateDSV(&dev, &depth, &dsv)) {
    return false;
  }
  if (!CreateSRV(&dev, &depth_alias, &srv_alias)) {
    return false;
  }

  // Bind only DSV, no RTVs.
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/0, /*pViews=*/nullptr, dsv.hDsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after SetRenderTargets alloc-aliased DSV-only)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after alloc-aliased DSV-only bind)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const std::vector<aerogpu_handle_t> created =
      CollectCreateTexture2DHandles(dev.harness.last_stream.data(), dev.harness.last_stream.size());
  if (!Check(created.size() >= 2, "captured CREATE_TEXTURE2D handles (depth + alias)")) {
    return false;
  }
  {
    const CmdLoc loc =
        FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
    if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after alloc-aliased DSV-only bind)")) {
      return false;
    }
    const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
    if (!Check(set_rt->color_count == 0, "SET_RENDER_TARGETS color_count==0 (alloc-aliased DSV-only bind)")) {
      return false;
    }
    if (!Check(set_rt->depth_stencil == created[0], "SET_RENDER_TARGETS depth_stencil matches created depth handle")) {
      return false;
    }
  }

  // Binding a PS SRV whose backing allocation aliases the DSV must unbind the DSV.
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv_alias.hSrv};
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after PSSetShaderResources alloc-aliased DSV)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after PSSetShaderResources alloc-aliased DSV)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after PSSetShaderResources alloc-aliased DSV)")) {
    return false;
  }
  const CmdLoc ps_tex_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_PIXEL, /*slot=*/0);
  if (!Check(ps_tex_loc.hdr != nullptr, "SET_TEXTURE present (PS slot 0) after PSSetShaderResources alloc-aliased DSV")) {
    return false;
  }
  const auto* set_ps = reinterpret_cast<const aerogpu_cmd_set_texture*>(ps_tex_loc.hdr);
  if (!Check(set_ps->texture == created[1], "PS slot0 bound to alloc-aliased depth SRV handle")) {
    return false;
  }
  if (!Check(loc.offset < ps_tex_loc.offset, "alloc-aliased hazard unbind (SET_RENDER_TARGETS) happens before PS SRV bind (DSV)")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
  if (!Check(set_rt->color_count == 0, "SET_RENDER_TARGETS color_count==0 (alloc-aliased DSV unbound)")) {
    return false;
  }
  if (!Check(set_rt->depth_stencil == 0, "SET_RENDER_TARGETS depth_stencil==0 (alloc-aliased DSV unbound)")) {
    return false;
  }
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (!Check(set_rt->colors[i] == 0, "SET_RENDER_TARGETS colors[i]==0 (alloc-aliased DSV unbound)")) {
      return false;
    }
  }

  const auto* alloc200 = FindSubmitAlloc(dev.harness.last_allocs, 200);
  if (!Check(alloc200 != nullptr, "alloc list contains handle 200 (DSV/SRV alias)")) {
    return false;
  }
  if (!Check(alloc200->write == 0, "alloc 200 marked read-only after DSV hazard unbind")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv_alias.hSrv);
  dev.device_funcs.pfnDestroyDSV(dev.hDevice, dsv.hDsv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, depth_alias.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, depth.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSrvBindingUnbindsAliasedDsvVs() {
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

  // Binding a VS SRV that aliases the DSV must unbind the DSV.
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv.hSrv};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after VSSetShaderResources alias DSV)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after VSSetShaderResources alias DSV)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after VSSetShaderResources alias DSV)")) {
    return false;
  }
  const CmdLoc vs_tex_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_VERTEX, /*slot=*/0);
  if (!Check(vs_tex_loc.hdr != nullptr, "SET_TEXTURE present (VS slot 0) after VSSetShaderResources alias DSV")) {
    return false;
  }
  const auto* set_vs = reinterpret_cast<const aerogpu_cmd_set_texture*>(vs_tex_loc.hdr);
  if (!Check(set_vs->texture == created[0], "VS slot0 bound to depth SRV handle")) {
    return false;
  }
  if (!Check(loc.offset < vs_tex_loc.offset, "hazard unbind (SET_RENDER_TARGETS) happens before VS SRV bind (DSV)")) {
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

bool TestSrvBindingUnbindsAllocAliasedDsvVs() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  dev.callbacks.pfnAllocateBacking = &Harness::AllocateBacking;
  dev.callbacks.pfnMapAllocation = &Harness::MapAllocation;
  dev.callbacks.pfnUnmapAllocation = &Harness::UnmapAllocation;
  dev.harness.alloc_sequence = {200, 200}; // depth + alias share allocation

  TestResource depth{};
  TestResource depth_alias{};
  TestDsv dsv{};
  TestSrv srv_alias{};

  if (!CreateTexture2D(&dev,
                       /*bind_flags=*/kD3D11BindDepthStencil | kD3D11BindShaderResource,
                       /*format=*/kDxgiFormatD24UnormS8Uint,
                       /*width=*/4,
                       /*height=*/4,
                       &depth)) {
    return false;
  }
  if (!CreateTexture2D(&dev,
                       /*bind_flags=*/kD3D11BindShaderResource,
                       /*format=*/kDxgiFormatD24UnormS8Uint,
                       /*width=*/4,
                       /*height=*/4,
                       &depth_alias)) {
    return false;
  }
  if (!CreateDSV(&dev, &depth, &dsv)) {
    return false;
  }
  if (!CreateSRV(&dev, &depth_alias, &srv_alias)) {
    return false;
  }

  // Bind only DSV, no RTVs.
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/0, /*pViews=*/nullptr, dsv.hDsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after SetRenderTargets alloc-aliased DSV-only)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after alloc-aliased DSV-only bind)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const std::vector<aerogpu_handle_t> created =
      CollectCreateTexture2DHandles(dev.harness.last_stream.data(), dev.harness.last_stream.size());
  if (!Check(created.size() >= 2, "captured CREATE_TEXTURE2D handles (depth + alias)")) {
    return false;
  }
  {
    const CmdLoc loc =
        FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
    if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after alloc-aliased DSV-only bind)")) {
      return false;
    }
    const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
    if (!Check(set_rt->color_count == 0, "SET_RENDER_TARGETS color_count==0 (alloc-aliased DSV-only bind)")) {
      return false;
    }
    if (!Check(set_rt->depth_stencil == created[0], "SET_RENDER_TARGETS depth_stencil matches created depth handle")) {
      return false;
    }
  }

  // Binding a VS SRV whose backing allocation aliases the DSV must unbind the DSV.
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv_alias.hSrv};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after VSSetShaderResources alloc-aliased DSV)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after VSSetShaderResources alloc-aliased DSV)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(loc.hdr != nullptr, "SET_RENDER_TARGETS present (after VSSetShaderResources alloc-aliased DSV)")) {
    return false;
  }
  const CmdLoc vs_tex_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_VERTEX, /*slot=*/0);
  if (!Check(vs_tex_loc.hdr != nullptr, "SET_TEXTURE present (VS slot 0) after VSSetShaderResources alloc-aliased DSV")) {
    return false;
  }
  const auto* set_vs = reinterpret_cast<const aerogpu_cmd_set_texture*>(vs_tex_loc.hdr);
  if (!Check(set_vs->texture == created[1], "VS slot0 bound to alloc-aliased depth SRV handle")) {
    return false;
  }
  if (!Check(loc.offset < vs_tex_loc.offset, "alloc-aliased hazard unbind (SET_RENDER_TARGETS) happens before VS SRV bind (DSV)")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(loc.hdr);
  if (!Check(set_rt->color_count == 0, "SET_RENDER_TARGETS color_count==0 (alloc-aliased DSV unbound)")) {
    return false;
  }
  if (!Check(set_rt->depth_stencil == 0, "SET_RENDER_TARGETS depth_stencil==0 (alloc-aliased DSV unbound)")) {
    return false;
  }
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; ++i) {
    if (!Check(set_rt->colors[i] == 0, "SET_RENDER_TARGETS colors[i]==0 (alloc-aliased DSV unbound)")) {
      return false;
    }
  }

  const auto* alloc200 = FindSubmitAlloc(dev.harness.last_allocs, 200);
  if (!Check(alloc200 != nullptr, "alloc list contains handle 200 (VS DSV/SRV alias)")) {
    return false;
  }
  if (!Check(alloc200->write == 0, "alloc 200 marked read-only after VS DSV hazard unbind")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv_alias.hSrv);
  dev.device_funcs.pfnDestroyDSV(dev.hDevice, dsv.hDsv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, depth_alias.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, depth.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSetRenderTargetsUnbindsAllocAliasedSrvsForMrt() {
  TestDevice dev{};
  if (!CreateDevice(&dev)) {
    return false;
  }

  dev.callbacks.pfnAllocateBacking = &Harness::AllocateBacking;
  dev.callbacks.pfnMapAllocation = &Harness::MapAllocation;
  dev.callbacks.pfnUnmapAllocation = &Harness::UnmapAllocation;
  dev.harness.alloc_sequence = {100, 101, 100}; // tex0 and tex_alias share allocation

  TestResource tex0{};
  TestResource tex1{};
  TestResource tex_alias{};
  TestRtv rtv0{};
  TestRtv rtv1{};
  TestSrv srv_alias{};

  if (!CreateRenderTargetTexture2D(&dev, /*width=*/4, /*height=*/4, &tex0) ||
      !CreateRenderTargetTexture2D(&dev, /*width=*/4, /*height=*/4, &tex1) ||
      !CreateTexture2D(&dev, kD3D11BindShaderResource, kDxgiFormatB8G8R8A8Unorm, /*width=*/4, /*height=*/4, &tex_alias)) {
    return false;
  }
  if (!CreateRTV(&dev, &tex0, &rtv0) || !CreateRTV(&dev, &tex1, &rtv1)) {
    return false;
  }
  if (!CreateSRV(&dev, &tex_alias, &srv_alias)) {
    return false;
  }

  // Bind the aliased SRV first (both VS and PS). Binding tex0 as RTV later must
  // evict the SRV from both stages even though it is a distinct handle.
  D3D10DDI_HSHADERRESOURCEVIEW srvs[1] = {srv_alias.hSrv};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  dev.device_funcs.pfnPsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*num_views=*/1, srvs);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after bind alloc-aliased SRV)")) {
    return false;
  }

  std::vector<uint8_t> first_stream = dev.harness.last_stream;
  if (!Check(!first_stream.empty(), "submission captured (after bind alloc-aliased SRV)")) {
    return false;
  }
  if (!ValidateStream(first_stream.data(), first_stream.size())) {
    return false;
  }
  const std::vector<aerogpu_handle_t> created = CollectCreateTexture2DHandles(first_stream.data(), first_stream.size());
  if (!Check(created.size() >= 3, "captured CREATE_TEXTURE2D handles (3 alloc-aliased)")) {
    return false;
  }

  // Bind MRTs; this must unbind SRVs that alias tex0's allocation.
  D3D10DDI_HRENDERTARGETVIEW rtvs[2] = {rtv0.hRtv, rtv1.hRtv};
  D3D10DDI_HDEPTHSTENCILVIEW null_dsv{};
  dev.device_funcs.pfnSetRenderTargets(dev.hDevice, /*num_views=*/2, rtvs, null_dsv);
  if (!Check(dev.device_funcs.pfnFlush(dev.hDevice) == S_OK, "Flush (after SetRenderTargets alloc-aliased MRT)")) {
    return false;
  }

  if (!Check(!dev.harness.last_stream.empty(), "submission captured (after SetRenderTargets alloc-aliased MRT)")) {
    return false;
  }
  if (!ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size())) {
    return false;
  }

  const CmdLoc vs_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_VERTEX, /*slot=*/0);
  if (!Check(vs_loc.hdr != nullptr, "SET_TEXTURE present (VS slot 0) after alloc-aliased SetRenderTargets")) {
    return false;
  }
  const auto* set_vs = reinterpret_cast<const aerogpu_cmd_set_texture*>(vs_loc.hdr);
  if (!Check(set_vs->texture == 0, "VS SRV slot 0 unbound before alloc-aliased MRT bind")) {
    return false;
  }

  const CmdLoc ps_loc =
      FindLastSetTexture(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_SHADER_STAGE_PIXEL, /*slot=*/0);
  if (!Check(ps_loc.hdr != nullptr, "SET_TEXTURE present (PS slot 0) after alloc-aliased SetRenderTargets")) {
    return false;
  }
  const auto* set_ps = reinterpret_cast<const aerogpu_cmd_set_texture*>(ps_loc.hdr);
  if (!Check(set_ps->texture == 0, "PS SRV slot 0 unbound before alloc-aliased MRT bind")) {
    return false;
  }

  const CmdLoc rt_loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_SET_RENDER_TARGETS);
  if (!Check(rt_loc.hdr != nullptr, "SET_RENDER_TARGETS present (after alloc-aliased MRT bind)")) {
    return false;
  }
  if (!Check(vs_loc.offset < rt_loc.offset, "VS SRV unbind occurs before alloc-aliased MRT bind")) {
    return false;
  }
  if (!Check(ps_loc.offset < rt_loc.offset, "PS SRV unbind occurs before alloc-aliased MRT bind")) {
    return false;
  }
  const auto* set_rt = reinterpret_cast<const aerogpu_cmd_set_render_targets*>(rt_loc.hdr);
  if (!Check(set_rt->color_count == 2, "SET_RENDER_TARGETS color_count==2 (after alloc-aliased MRT bind)")) {
    return false;
  }
  if (!Check(set_rt->colors[0] == created[0], "SET_RENDER_TARGETS colors[0] (after alloc-aliased MRT bind)")) {
    return false;
  }
  if (!Check(set_rt->colors[1] == created[1], "SET_RENDER_TARGETS colors[1] (after alloc-aliased MRT bind)")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv_alias.hSrv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv0.hRtv);
  dev.device_funcs.pfnDestroyRTV(dev.hDevice, rtv1.hRtv);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex_alias.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex0.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex1.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

} // namespace

int main() {
  bool ok = true;
  ok &= TestCreateSrvNotImplIsSafeToDestroy();
  ok &= TestSetRenderTargetsEncodesMrtAndClamps();
  ok &= TestSetRenderTargetsPreservesNullEntries();
  ok &= TestSetRenderTargetsUnbindsAliasedSrvsForMrt();
  ok &= TestSetRenderTargetsUnbindsAllocAliasedSrvsForMrt();
  ok &= TestSetRenderTargetsUnbindsAliasedSrvsForDsv();
  ok &= TestSetRenderTargetsUnbindsAllocAliasedSrvsForDsv();
  ok &= TestSetRenderTargetsUnbindsOnlyAliasedSrvs();
  ok &= TestSrvBindingUnbindsOnlyAliasedRtv();
  ok &= TestSrvBindingUnbindsOnlyAllocAliasedRtv();
  ok &= TestSrvBindingUnbindsOnlyAliasedRtvVs();
  ok &= TestSrvBindingUnbindsOnlyAllocAliasedRtvVs();
  ok &= TestSrvBindingUnbindsAllAliasedRtvSlots();
  ok &= TestSrvBindingUnbindsAllAllocAliasedRtvSlots();
  ok &= TestRotateResourceIdentitiesRemapsSrvsAndViews();
  ok &= TestSrvBindingUnbindsAliasedDsv();
  ok &= TestSrvBindingUnbindsAllocAliasedDsv();
  ok &= TestSrvBindingUnbindsAliasedDsvVs();
  ok &= TestSrvBindingUnbindsAllocAliasedDsvVs();
  if (!ok) {
    return 1;
  }
  std::fprintf(stderr, "PASS: aerogpu_d3d10_11_mrt_tests\n");
  return 0;
}
