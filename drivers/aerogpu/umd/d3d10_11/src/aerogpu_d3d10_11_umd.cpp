// AeroGPU Windows 7 D3D10/11 UMD (minimal milestone implementation).
//
// This implementation focuses on the smallest working surface area required for
// D3D11 FL10_0 triangle-style samples.
//
// Key design: D3D10/11 DDIs are translated into the same AeroGPU command stream
// ("AeroGPU IR") used by the D3D9 UMD:
//   drivers/aerogpu/protocol/aerogpu_cmd.h
//
// The real Windows 7 build should be compiled with WDK headers and wired to the
// KMD submission path. For repository builds (no WDK), this code uses a minimal
// DDI ABI subset declared in `include/aerogpu_d3d10_11_umd.h`.

#include "../include/aerogpu_d3d10_11_umd.h"

#include <atomic>
#include <condition_variable>
#include <cstring>
#include <mutex>
#include <new>
#include <vector>

#include "aerogpu_cmd_writer.h"

namespace {

constexpr aerogpu_handle_t kInvalidHandle = 0;

// D3D11_BIND_* subset (numeric values from d3d11.h).
constexpr uint32_t kD3D11BindVertexBuffer = 0x1;
constexpr uint32_t kD3D11BindIndexBuffer = 0x2;
constexpr uint32_t kD3D11BindConstantBuffer = 0x4;
constexpr uint32_t kD3D11BindShaderResource = 0x8;
constexpr uint32_t kD3D11BindRenderTarget = 0x20;
constexpr uint32_t kD3D11BindDepthStencil = 0x40;

// DXGI_FORMAT subset (numeric values from dxgiformat.h).
constexpr uint32_t kDxgiFormatR8G8B8A8Unorm = 28;
constexpr uint32_t kDxgiFormatD32Float = 40;
constexpr uint32_t kDxgiFormatD24UnormS8Uint = 45;
constexpr uint32_t kDxgiFormatR16Uint = 57;
constexpr uint32_t kDxgiFormatR32Uint = 42;
constexpr uint32_t kDxgiFormatB8G8R8A8Unorm = 87;
constexpr uint32_t kDxgiFormatB8G8R8X8Unorm = 88;

uint32_t f32_bits(float v) {
  uint32_t bits = 0;
  static_assert(sizeof(bits) == sizeof(v), "float must be 32-bit");
  std::memcpy(&bits, &v, sizeof(bits));
  return bits;
}

// FNV-1a 32-bit hash for stable semantic name IDs.
uint32_t HashSemanticName(const char* s) {
  if (!s) {
    return 0;
  }
  uint32_t hash = 2166136261u;
  for (const unsigned char* p = reinterpret_cast<const unsigned char*>(s); *p; ++p) {
    hash ^= *p;
    hash *= 16777619u;
  }
  return hash;
}

uint32_t dxgi_format_to_aerogpu(uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatB8G8R8A8Unorm:
      return AEROGPU_FORMAT_B8G8R8A8_UNORM;
    case kDxgiFormatB8G8R8X8Unorm:
      return AEROGPU_FORMAT_B8G8R8X8_UNORM;
    case kDxgiFormatR8G8B8A8Unorm:
      return AEROGPU_FORMAT_R8G8B8A8_UNORM;
    case kDxgiFormatD24UnormS8Uint:
      return AEROGPU_FORMAT_D24_UNORM_S8_UINT;
    case kDxgiFormatD32Float:
      return AEROGPU_FORMAT_D32_FLOAT;
    default:
      return AEROGPU_FORMAT_INVALID;
  }
}

uint32_t bytes_per_pixel_aerogpu(uint32_t aerogpu_format) {
  switch (aerogpu_format) {
    case AEROGPU_FORMAT_B8G8R8A8_UNORM:
    case AEROGPU_FORMAT_B8G8R8X8_UNORM:
    case AEROGPU_FORMAT_R8G8B8A8_UNORM:
    case AEROGPU_FORMAT_R8G8B8X8_UNORM:
    case AEROGPU_FORMAT_D24_UNORM_S8_UINT:
    case AEROGPU_FORMAT_D32_FLOAT:
      return 4;
    case AEROGPU_FORMAT_B5G6R5_UNORM:
    case AEROGPU_FORMAT_B5G5R5A1_UNORM:
      return 2;
    default:
      return 4;
  }
}

uint32_t dxgi_index_format_to_aerogpu(uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatR32Uint:
      return AEROGPU_INDEX_FORMAT_UINT32;
    case kDxgiFormatR16Uint:
    default:
      return AEROGPU_INDEX_FORMAT_UINT16;
  }
}

uint32_t bind_flags_to_usage_flags(uint32_t bind_flags) {
  uint32_t usage = AEROGPU_RESOURCE_USAGE_NONE;
  if (bind_flags & kD3D11BindVertexBuffer) {
    usage |= AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
  }
  if (bind_flags & kD3D11BindIndexBuffer) {
    usage |= AEROGPU_RESOURCE_USAGE_INDEX_BUFFER;
  }
  if (bind_flags & kD3D11BindConstantBuffer) {
    usage |= AEROGPU_RESOURCE_USAGE_CONSTANT_BUFFER;
  }
  if (bind_flags & kD3D11BindShaderResource) {
    usage |= AEROGPU_RESOURCE_USAGE_TEXTURE;
  }
  if (bind_flags & kD3D11BindRenderTarget) {
    usage |= AEROGPU_RESOURCE_USAGE_RENDER_TARGET;
  }
  if (bind_flags & kD3D11BindDepthStencil) {
    usage |= AEROGPU_RESOURCE_USAGE_DEPTH_STENCIL;
  }
  return usage;
}

enum class ResourceKind : uint32_t {
  Unknown = 0,
  Buffer = 1,
  Texture2D = 2,
};

struct AeroGpuAdapter {
  std::atomic<uint32_t> next_handle{1};

  std::mutex fence_mutex;
  std::condition_variable fence_cv;
  uint64_t next_fence = 1;
  uint64_t completed_fence = 0;
};

struct AeroGpuResource {
  aerogpu_handle_t handle = 0;
  ResourceKind kind = ResourceKind::Unknown;

  uint32_t bind_flags = 0;
  uint32_t misc_flags = 0;

  // Buffer fields.
  uint64_t size_bytes = 0;

  // Texture2D fields.
  uint32_t width = 0;
  uint32_t height = 0;
  uint32_t mip_levels = 1;
  uint32_t array_size = 1;
  uint32_t dxgi_format = 0;
  uint32_t row_pitch_bytes = 0;

  // CPU-visible backing storage for resource uploads.
  //
  // The initial milestone keeps resource data management very conservative:
  // - Buffers can be initialized at CreateResource time.
  // - Texture2D initial data is supported for the common {mips=1, array=1} case.
  //
  // A real WDDM build should map these updates onto real allocations.
  std::vector<uint8_t> storage;
};

struct AeroGpuShader {
  aerogpu_handle_t handle = 0;
  uint32_t stage = AEROGPU_SHADER_STAGE_VERTEX;
  std::vector<uint8_t> dxbc;
};

struct AeroGpuInputLayout {
  aerogpu_handle_t handle = 0;
  std::vector<uint8_t> blob;
};

struct AeroGpuRenderTargetView {
  aerogpu_handle_t texture = 0;
};

struct AeroGpuDepthStencilView {
  aerogpu_handle_t texture = 0;
};

// The initial milestone treats pipeline state objects as opaque handles. They
// are accepted and can be bound, but the host translator currently relies on
// conservative defaults for any state not explicitly encoded in the command
// stream.
struct AeroGpuBlendState {
  uint32_t dummy = 0;
};
struct AeroGpuRasterizerState {
  uint32_t dummy = 0;
};
struct AeroGpuDepthStencilState {
  uint32_t dummy = 0;
};

struct AeroGpuDevice {
  AeroGpuAdapter* adapter = nullptr;
  std::mutex mutex;

  aerogpu::CmdWriter cmd;

  // Cached state.
  aerogpu_handle_t current_rtv = 0;
  aerogpu_handle_t current_dsv = 0;
  aerogpu_handle_t current_vs = 0;
  aerogpu_handle_t current_ps = 0;
  aerogpu_handle_t current_input_layout = 0;
  uint32_t current_topology = AEROGPU_TOPOLOGY_TRIANGLELIST;

  AeroGpuDevice() {
    cmd.reset();
  }
};

template <typename THandle, typename TObject>
TObject* FromHandle(THandle h) {
  return reinterpret_cast<TObject*>(h.pDrvPrivate);
}

uint64_t submit_locked(AeroGpuDevice* dev) {
  if (!dev || dev->cmd.empty()) {
    return 0;
  }

  AeroGpuAdapter* adapter = dev->adapter;
  if (!adapter) {
    return 0;
  }

  dev->cmd.finalize();

  uint64_t fence = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    fence = adapter->next_fence++;
    adapter->completed_fence = fence;
  }
  adapter->fence_cv.notify_all();

  dev->cmd.reset();
  return fence;
}

HRESULT flush_locked(AeroGpuDevice* dev) {
  submit_locked(dev);
  return S_OK;
}

// -------------------------------------------------------------------------------------------------
// Device DDI (plain functions to ensure the correct calling convention)
// -------------------------------------------------------------------------------------------------

void AEROGPU_APIENTRY DestroyDevice(D3D10DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  dev->~AeroGpuDevice();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateResourceSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERESOURCE*) {
  return sizeof(AeroGpuResource);
}

HRESULT AEROGPU_APIENTRY CreateResource(D3D10DDI_HDEVICE hDevice,
                                         const AEROGPU_DDIARG_CREATERESOURCE* pDesc,
                                         D3D10DDI_HRESOURCE hResource) {
  if (!hDevice.pDrvPrivate || !pDesc || !hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER) {
    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = dev->adapter->next_handle.fetch_add(1);
    res->kind = ResourceKind::Buffer;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    res->size_bytes = pDesc->ByteWidth;

    if (pDesc->pInitialData && pDesc->InitialDataCount) {
      const auto& init = pDesc->pInitialData[0];
      if (!init.pSysMem) {
        res->~AeroGpuResource();
        return E_INVALIDARG;
      }
      try {
        res->storage.resize(static_cast<size_t>(res->size_bytes));
      } catch (...) {
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }
      std::memcpy(res->storage.data(), init.pSysMem, static_cast<size_t>(res->size_bytes));
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
    cmd->buffer_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags);
    cmd->size_bytes = res->size_bytes;
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if (!res->storage.empty()) {
      auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      dirty->resource_handle = res->handle;
      dirty->reserved0 = 0;
      dirty->offset_bytes = 0;
      dirty->size_bytes = res->size_bytes;
    }
    return S_OK;
  }

  if (pDesc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D) {
    if (pDesc->ArraySize != 1) {
      return E_NOTIMPL;
    }

    const uint32_t aer_fmt = dxgi_format_to_aerogpu(pDesc->Format);
    if (aer_fmt == AEROGPU_FORMAT_INVALID) {
      return E_NOTIMPL;
    }

    auto* res = new (hResource.pDrvPrivate) AeroGpuResource();
    res->handle = dev->adapter->next_handle.fetch_add(1);
    res->kind = ResourceKind::Texture2D;
    res->bind_flags = pDesc->BindFlags;
    res->misc_flags = pDesc->MiscFlags;
    res->width = pDesc->Width;
    res->height = pDesc->Height;
    res->mip_levels = pDesc->MipLevels ? pDesc->MipLevels : 1;
    res->array_size = pDesc->ArraySize;
    res->dxgi_format = pDesc->Format;
    res->row_pitch_bytes = res->width * bytes_per_pixel_aerogpu(aer_fmt);

    if (pDesc->pInitialData && pDesc->InitialDataCount) {
      if (res->mip_levels != 1 || res->array_size != 1) {
        res->~AeroGpuResource();
        return E_NOTIMPL;
      }

      const auto& init = pDesc->pInitialData[0];
      if (!init.pSysMem) {
        res->~AeroGpuResource();
        return E_INVALIDARG;
      }

      const uint64_t total_bytes = static_cast<uint64_t>(res->row_pitch_bytes) * static_cast<uint64_t>(res->height);
      if (total_bytes > static_cast<uint64_t>(SIZE_MAX)) {
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }
      try {
        res->storage.resize(static_cast<size_t>(total_bytes));
      } catch (...) {
        res->~AeroGpuResource();
        return E_OUTOFMEMORY;
      }

      const uint8_t* src = static_cast<const uint8_t*>(init.pSysMem);
      const size_t src_pitch = init.SysMemPitch ? static_cast<size_t>(init.SysMemPitch)
                                                : static_cast<size_t>(res->row_pitch_bytes);
      for (uint32_t y = 0; y < res->height; y++) {
        std::memcpy(res->storage.data() + static_cast<size_t>(y) * res->row_pitch_bytes,
                    src + static_cast<size_t>(y) * src_pitch,
                    res->row_pitch_bytes);
      }
    }

    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
    cmd->texture_handle = res->handle;
    cmd->usage_flags = bind_flags_to_usage_flags(res->bind_flags) | AEROGPU_RESOURCE_USAGE_TEXTURE;
    cmd->format = aer_fmt;
    cmd->width = res->width;
    cmd->height = res->height;
    cmd->mip_levels = res->mip_levels;
    cmd->array_layers = 1;
    cmd->row_pitch_bytes = res->row_pitch_bytes;
    cmd->backing_alloc_id = 0;
    cmd->backing_offset_bytes = 0;
    cmd->reserved0 = 0;

    if (!res->storage.empty()) {
      auto* dirty = dev->cmd.append_fixed<aerogpu_cmd_resource_dirty_range>(AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
      dirty->resource_handle = res->handle;
      dirty->reserved0 = 0;
      dirty->offset_bytes = 0;
      dirty->size_bytes = res->storage.size();
    }
    return S_OK;
  }

  return E_NOTIMPL;
}

void AEROGPU_APIENTRY DestroyResource(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hResource) {
  if (!hDevice.pDrvPrivate || !hResource.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hResource);
  if (!dev || !res) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (res->handle != kInvalidHandle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_resource>(AEROGPU_CMD_DESTROY_RESOURCE);
    cmd->resource_handle = res->handle;
    cmd->reserved0 = 0;
  }
  res->~AeroGpuResource();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateShaderSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATESHADER*) {
  return sizeof(AeroGpuShader);
}

static HRESULT CreateShaderCommon(D3D10DDI_HDEVICE hDevice,
                                  const AEROGPU_DDIARG_CREATESHADER* pDesc,
                                  D3D10DDI_HSHADER hShader,
                                  uint32_t stage) {
  if (!hDevice.pDrvPrivate || !pDesc || !hShader.pDrvPrivate || !pDesc->pCode || !pDesc->CodeSize) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* sh = new (hShader.pDrvPrivate) AeroGpuShader();
  sh->handle = dev->adapter->next_handle.fetch_add(1);
  sh->stage = stage;
  sh->dxbc.resize(pDesc->CodeSize);
  std::memcpy(sh->dxbc.data(), pDesc->pCode, pDesc->CodeSize);

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, sh->dxbc.data(), sh->dxbc.size());
  cmd->shader_handle = sh->handle;
  cmd->stage = stage;
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sh->dxbc.size());
  cmd->reserved0 = 0;
  return S_OK;
}

HRESULT AEROGPU_APIENTRY CreateVertexShader(D3D10DDI_HDEVICE hDevice,
                                            const AEROGPU_DDIARG_CREATESHADER* pDesc,
                                            D3D10DDI_HSHADER hShader) {
  return CreateShaderCommon(hDevice, pDesc, hShader, AEROGPU_SHADER_STAGE_VERTEX);
}

HRESULT AEROGPU_APIENTRY CreatePixelShader(D3D10DDI_HDEVICE hDevice,
                                           const AEROGPU_DDIARG_CREATESHADER* pDesc,
                                           D3D10DDI_HSHADER hShader) {
  return CreateShaderCommon(hDevice, pDesc, hShader, AEROGPU_SHADER_STAGE_PIXEL);
}

void AEROGPU_APIENTRY DestroyShader(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hShader) {
  if (!hDevice.pDrvPrivate || !hShader.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* sh = FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hShader);
  if (!dev || !sh) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (sh->handle != kInvalidHandle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_shader>(AEROGPU_CMD_DESTROY_SHADER);
    cmd->shader_handle = sh->handle;
    cmd->reserved0 = 0;
  }
  sh->~AeroGpuShader();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateInputLayoutSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATEINPUTLAYOUT*) {
  return sizeof(AeroGpuInputLayout);
}

HRESULT AEROGPU_APIENTRY CreateInputLayout(D3D10DDI_HDEVICE hDevice,
                                           const AEROGPU_DDIARG_CREATEINPUTLAYOUT* pDesc,
                                           D3D10DDI_HELEMENTLAYOUT hLayout) {
  if (!hDevice.pDrvPrivate || !pDesc || !hLayout.pDrvPrivate || (!pDesc->NumElements && pDesc->pElements)) {
    return E_INVALIDARG;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev || !dev->adapter) {
    return E_FAIL;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* layout = new (hLayout.pDrvPrivate) AeroGpuInputLayout();
  layout->handle = dev->adapter->next_handle.fetch_add(1);

  const size_t blob_size = sizeof(aerogpu_input_layout_blob_header) +
                           static_cast<size_t>(pDesc->NumElements) * sizeof(aerogpu_input_layout_element_dxgi);
  layout->blob.resize(blob_size);

  auto* hdr = reinterpret_cast<aerogpu_input_layout_blob_header*>(layout->blob.data());
  hdr->magic = AEROGPU_INPUT_LAYOUT_BLOB_MAGIC;
  hdr->version = AEROGPU_INPUT_LAYOUT_BLOB_VERSION;
  hdr->element_count = pDesc->NumElements;
  hdr->reserved0 = 0;

  auto* elems = reinterpret_cast<aerogpu_input_layout_element_dxgi*>(layout->blob.data() + sizeof(*hdr));
  for (uint32_t i = 0; i < pDesc->NumElements; i++) {
    const auto& e = pDesc->pElements[i];
    elems[i].semantic_name_hash = HashSemanticName(e.SemanticName);
    elems[i].semantic_index = e.SemanticIndex;
    elems[i].dxgi_format = e.Format;
    elems[i].input_slot = e.InputSlot;
    elems[i].aligned_byte_offset = e.AlignedByteOffset;
    elems[i].input_slot_class = e.InputSlotClass;
    elems[i].instance_data_step_rate = e.InstanceDataStepRate;
  }

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_create_input_layout>(
      AEROGPU_CMD_CREATE_INPUT_LAYOUT, layout->blob.data(), layout->blob.size());
  cmd->input_layout_handle = layout->handle;
  cmd->blob_size_bytes = static_cast<uint32_t>(layout->blob.size());
  cmd->reserved0 = 0;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyInputLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
  if (!hLayout.pDrvPrivate) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  auto* layout = FromHandle<D3D10DDI_HELEMENTLAYOUT, AeroGpuInputLayout>(hLayout);
  if (!dev || !layout) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (layout->handle) {
    auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_destroy_input_layout>(AEROGPU_CMD_DESTROY_INPUT_LAYOUT);
    cmd->input_layout_handle = layout->handle;
    cmd->reserved0 = 0;
  }
  layout->~AeroGpuInputLayout();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRTVSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERENDERTARGETVIEW*) {
  return sizeof(AeroGpuRenderTargetView);
}

HRESULT AEROGPU_APIENTRY CreateRTV(D3D10DDI_HDEVICE hDevice,
                                   const AEROGPU_DDIARG_CREATERENDERTARGETVIEW* pDesc,
                                   D3D10DDI_HRENDERTARGETVIEW hRtv) {
  if (!hDevice.pDrvPrivate || !pDesc || !hRtv.pDrvPrivate || !pDesc->hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  auto* rtv = new (hRtv.pDrvPrivate) AeroGpuRenderTargetView();
  rtv->texture = res ? res->handle : 0;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRTV(D3D10DDI_HDEVICE, D3D10DDI_HRENDERTARGETVIEW hRtv) {
  if (!hRtv.pDrvPrivate) {
    return;
  }
  auto* rtv = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv);
  rtv->~AeroGpuRenderTargetView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDSVSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATEDEPTHSTENCILVIEW*) {
  return sizeof(AeroGpuDepthStencilView);
}

HRESULT AEROGPU_APIENTRY CreateDSV(D3D10DDI_HDEVICE hDevice,
                                   const AEROGPU_DDIARG_CREATEDEPTHSTENCILVIEW* pDesc,
                                   D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  if (!hDevice.pDrvPrivate || !pDesc || !hDsv.pDrvPrivate || !pDesc->hResource.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* res = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pDesc->hResource);
  auto* dsv = new (hDsv.pDrvPrivate) AeroGpuDepthStencilView();
  dsv->texture = res ? res->handle : 0;
  return S_OK;
}

void AEROGPU_APIENTRY DestroyDSV(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  if (!hDsv.pDrvPrivate) {
    return;
  }
  auto* dsv = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv);
  dsv->~AeroGpuDepthStencilView();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateBlendStateSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATEBLENDSTATE*) {
  return sizeof(AeroGpuBlendState);
}

HRESULT AEROGPU_APIENTRY CreateBlendState(D3D10DDI_HDEVICE hDevice,
                                          const AEROGPU_DDIARG_CREATEBLENDSTATE*,
                                          D3D10DDI_HBLENDSTATE hState) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuBlendState();
  return S_OK;
}

void AEROGPU_APIENTRY DestroyBlendState(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HBLENDSTATE, AeroGpuBlendState>(hState);
  s->~AeroGpuBlendState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateRasterizerStateSize(D3D10DDI_HDEVICE, const AEROGPU_DDIARG_CREATERASTERIZERSTATE*) {
  return sizeof(AeroGpuRasterizerState);
}

HRESULT AEROGPU_APIENTRY CreateRasterizerState(D3D10DDI_HDEVICE hDevice,
                                               const AEROGPU_DDIARG_CREATERASTERIZERSTATE*,
                                               D3D10DDI_HRASTERIZERSTATE hState) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuRasterizerState();
  return S_OK;
}

void AEROGPU_APIENTRY DestroyRasterizerState(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HRASTERIZERSTATE, AeroGpuRasterizerState>(hState);
  s->~AeroGpuRasterizerState();
}

SIZE_T AEROGPU_APIENTRY CalcPrivateDepthStencilStateSize(D3D10DDI_HDEVICE,
                                                         const AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE*) {
  return sizeof(AeroGpuDepthStencilState);
}

HRESULT AEROGPU_APIENTRY CreateDepthStencilState(D3D10DDI_HDEVICE hDevice,
                                                 const AEROGPU_DDIARG_CREATEDEPTHSTENCILSTATE*,
                                                 D3D10DDI_HDEPTHSTENCILSTATE hState) {
  if (!hDevice.pDrvPrivate || !hState.pDrvPrivate) {
    return E_INVALIDARG;
  }
  new (hState.pDrvPrivate) AeroGpuDepthStencilState();
  return S_OK;
}

void AEROGPU_APIENTRY DestroyDepthStencilState(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE hState) {
  if (!hState.pDrvPrivate) {
    return;
  }
  auto* s = FromHandle<D3D10DDI_HDEPTHSTENCILSTATE, AeroGpuDepthStencilState>(hState);
  s->~AeroGpuDepthStencilState();
}

void AEROGPU_APIENTRY SetRenderTargets(D3D10DDI_HDEVICE hDevice,
                                       D3D10DDI_HRENDERTARGETVIEW hRtv,
                                       D3D10DDI_HDEPTHSTENCILVIEW hDsv) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_handle_t rtv_handle = 0;
  aerogpu_handle_t dsv_handle = 0;
  if (hRtv.pDrvPrivate) {
    rtv_handle = FromHandle<D3D10DDI_HRENDERTARGETVIEW, AeroGpuRenderTargetView>(hRtv)->texture;
  }
  if (hDsv.pDrvPrivate) {
    dsv_handle = FromHandle<D3D10DDI_HDEPTHSTENCILVIEW, AeroGpuDepthStencilView>(hDsv)->texture;
  }

  dev->current_rtv = rtv_handle;
  dev->current_dsv = dsv_handle;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_render_targets>(AEROGPU_CMD_SET_RENDER_TARGETS);
  cmd->color_count = 1;
  cmd->depth_stencil = dsv_handle;
  for (uint32_t i = 0; i < AEROGPU_MAX_RENDER_TARGETS; i++) {
    cmd->colors[i] = 0;
  }
  cmd->colors[0] = rtv_handle;
}

void AEROGPU_APIENTRY ClearRTV(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRENDERTARGETVIEW, const float rgba[4]) {
  if (!hDevice.pDrvPrivate || !rgba) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  cmd->flags = AEROGPU_CLEAR_COLOR;
  cmd->color_rgba_f32[0] = f32_bits(rgba[0]);
  cmd->color_rgba_f32[1] = f32_bits(rgba[1]);
  cmd->color_rgba_f32[2] = f32_bits(rgba[2]);
  cmd->color_rgba_f32[3] = f32_bits(rgba[3]);
  cmd->depth_f32 = f32_bits(1.0f);
  cmd->stencil = 0;
}

void AEROGPU_APIENTRY ClearDSV(D3D10DDI_HDEVICE hDevice,
                               D3D10DDI_HDEPTHSTENCILVIEW,
                               uint32_t clear_flags,
                               float depth,
                               uint8_t stencil) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  uint32_t flags = 0;
  if (clear_flags & AEROGPU_DDI_CLEAR_DEPTH) {
    flags |= AEROGPU_CLEAR_DEPTH;
  }
  if (clear_flags & AEROGPU_DDI_CLEAR_STENCIL) {
    flags |= AEROGPU_CLEAR_STENCIL;
  }

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_clear>(AEROGPU_CMD_CLEAR);
  cmd->flags = flags;
  cmd->color_rgba_f32[0] = 0;
  cmd->color_rgba_f32[1] = 0;
  cmd->color_rgba_f32[2] = 0;
  cmd->color_rgba_f32[3] = 0;
  cmd->depth_f32 = f32_bits(depth);
  cmd->stencil = stencil;
}

void AEROGPU_APIENTRY SetInputLayout(D3D10DDI_HDEVICE hDevice, D3D10DDI_HELEMENTLAYOUT hLayout) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_handle_t handle = 0;
  if (hLayout.pDrvPrivate) {
    handle = FromHandle<D3D10DDI_HELEMENTLAYOUT, AeroGpuInputLayout>(hLayout)->handle;
  }
  dev->current_input_layout = handle;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_input_layout>(AEROGPU_CMD_SET_INPUT_LAYOUT);
  cmd->input_layout_handle = handle;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY SetVertexBuffer(D3D10DDI_HDEVICE hDevice,
                                      D3D10DDI_HRESOURCE hBuffer,
                                      uint32_t stride,
                                      uint32_t offset) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_vertex_buffer_binding binding{};
  binding.buffer = hBuffer.pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hBuffer)->handle : 0;
  binding.stride_bytes = stride;
  binding.offset_bytes = offset;
  binding.reserved0 = 0;

  auto* cmd = dev->cmd.append_with_payload<aerogpu_cmd_set_vertex_buffers>(AEROGPU_CMD_SET_VERTEX_BUFFERS,
                                                                           &binding,
                                                                           sizeof(binding));
  cmd->start_slot = 0;
  cmd->buffer_count = 1;
}

void AEROGPU_APIENTRY SetIndexBuffer(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE hBuffer, uint32_t format, uint32_t offset) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_index_buffer>(AEROGPU_CMD_SET_INDEX_BUFFER);
  cmd->buffer = hBuffer.pDrvPrivate ? FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(hBuffer)->handle : 0;
  cmd->format = dxgi_index_format_to_aerogpu(format);
  cmd->offset_bytes = offset;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY SetViewport(D3D10DDI_HDEVICE hDevice, const AEROGPU_DDI_VIEWPORT* pVp) {
  if (!hDevice.pDrvPrivate || !pVp) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_viewport>(AEROGPU_CMD_SET_VIEWPORT);
  cmd->x_f32 = f32_bits(pVp->TopLeftX);
  cmd->y_f32 = f32_bits(pVp->TopLeftY);
  cmd->width_f32 = f32_bits(pVp->Width);
  cmd->height_f32 = f32_bits(pVp->Height);
  cmd->min_depth_f32 = f32_bits(pVp->MinDepth);
  cmd->max_depth_f32 = f32_bits(pVp->MaxDepth);
}

void AEROGPU_APIENTRY SetDrawState(D3D10DDI_HDEVICE hDevice, D3D10DDI_HSHADER hVs, D3D10DDI_HSHADER hPs) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  aerogpu_handle_t vs = hVs.pDrvPrivate ? FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hVs)->handle : 0;
  aerogpu_handle_t ps = hPs.pDrvPrivate ? FromHandle<D3D10DDI_HSHADER, AeroGpuShader>(hPs)->handle : 0;
  dev->current_vs = vs;
  dev->current_ps = ps;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_bind_shaders>(AEROGPU_CMD_BIND_SHADERS);
  cmd->vs = vs;
  cmd->ps = ps;
  cmd->cs = 0;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY SetBlendState(D3D10DDI_HDEVICE, D3D10DDI_HBLENDSTATE) {
  // Stub (state objects are accepted but not yet encoded).
}

void AEROGPU_APIENTRY SetRasterizerState(D3D10DDI_HDEVICE, D3D10DDI_HRASTERIZERSTATE) {
  // Stub (state objects are accepted but not yet encoded).
}

void AEROGPU_APIENTRY SetDepthStencilState(D3D10DDI_HDEVICE, D3D10DDI_HDEPTHSTENCILSTATE) {
  // Stub (state objects are accepted but not yet encoded).
}

void AEROGPU_APIENTRY SetPrimitiveTopology(D3D10DDI_HDEVICE hDevice, uint32_t topology) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  if (dev->current_topology == topology) {
    return;
  }
  dev->current_topology = topology;

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_set_primitive_topology>(AEROGPU_CMD_SET_PRIMITIVE_TOPOLOGY);
  cmd->topology = topology;
  cmd->reserved0 = 0;
}

void AEROGPU_APIENTRY Draw(D3D10DDI_HDEVICE hDevice, uint32_t vertex_count, uint32_t start_vertex) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw>(AEROGPU_CMD_DRAW);
  cmd->vertex_count = vertex_count;
  cmd->instance_count = 1;
  cmd->first_vertex = start_vertex;
  cmd->first_instance = 0;
}

void AEROGPU_APIENTRY DrawIndexed(D3D10DDI_HDEVICE hDevice, uint32_t index_count, uint32_t start_index, int32_t base_vertex) {
  if (!hDevice.pDrvPrivate) {
    return;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_draw_indexed>(AEROGPU_CMD_DRAW_INDEXED);
  cmd->index_count = index_count;
  cmd->instance_count = 1;
  cmd->first_index = start_index;
  cmd->base_vertex = base_vertex;
  cmd->first_instance = 0;
}

HRESULT AEROGPU_APIENTRY Present(D3D10DDI_HDEVICE hDevice, const AEROGPU_DDIARG_PRESENT* pPresent) {
  if (!hDevice.pDrvPrivate || !pPresent) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  auto* cmd = dev->cmd.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  cmd->scanout_id = 0;
  cmd->flags = (pPresent->SyncInterval == 1) ? AEROGPU_PRESENT_FLAG_VSYNC : AEROGPU_PRESENT_FLAG_NONE;

  submit_locked(dev);
  return S_OK;
}

HRESULT AEROGPU_APIENTRY Flush(D3D10DDI_HDEVICE hDevice) {
  if (!hDevice.pDrvPrivate) {
    return E_INVALIDARG;
  }
  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return E_INVALIDARG;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);
  return flush_locked(dev);
}

void AEROGPU_APIENTRY RotateResourceIdentities(D3D10DDI_HDEVICE hDevice, D3D10DDI_HRESOURCE* pResources, uint32_t numResources) {
  if (!hDevice.pDrvPrivate || !pResources || numResources < 2) {
    return;
  }

  auto* dev = FromHandle<D3D10DDI_HDEVICE, AeroGpuDevice>(hDevice);
  if (!dev) {
    return;
  }

  std::lock_guard<std::mutex> lock(dev->mutex);

  // Rotate AeroGPU handles in-place. This approximates DXGI swapchain buffer
  // flipping without requiring a full allocation-table/patching implementation.
  auto* first = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[0]);
  if (!first) {
    return;
  }
  const aerogpu_handle_t saved = first->handle;

  for (uint32_t i = 0; i + 1 < numResources; ++i) {
    auto* dst = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[i]);
    auto* src = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[i + 1]);
    if (!dst || !src) {
      return;
    }
    dst->handle = src->handle;
  }

  auto* last = FromHandle<D3D10DDI_HRESOURCE, AeroGpuResource>(pResources[numResources - 1]);
  if (last) {
    last->handle = saved;
  }
}

// -------------------------------------------------------------------------------------------------
// Adapter DDI
// -------------------------------------------------------------------------------------------------

SIZE_T AEROGPU_APIENTRY CalcPrivateDeviceSize(D3D10DDI_HADAPTER, const D3D10DDIARG_CREATEDEVICE*) {
  return sizeof(AeroGpuDevice);
}

HRESULT AEROGPU_APIENTRY CreateDevice(D3D10DDI_HADAPTER hAdapter, const D3D10DDIARG_CREATEDEVICE* pCreateDevice) {
  if (!pCreateDevice || !pCreateDevice->hDevice.pDrvPrivate || !pCreateDevice->pDeviceFuncs) {
    return E_INVALIDARG;
  }

  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  if (!adapter) {
    return E_FAIL;
  }

  auto* device = new (pCreateDevice->hDevice.pDrvPrivate) AeroGpuDevice();
  device->adapter = adapter;

  AEROGPU_D3D10_11_DEVICEFUNCS funcs = {};
  funcs.pfnDestroyDevice = &DestroyDevice;

  funcs.pfnCalcPrivateResourceSize = &CalcPrivateResourceSize;
  funcs.pfnCreateResource = &CreateResource;
  funcs.pfnDestroyResource = &DestroyResource;

  funcs.pfnCalcPrivateShaderSize = &CalcPrivateShaderSize;
  funcs.pfnCreateVertexShader = &CreateVertexShader;
  funcs.pfnCreatePixelShader = &CreatePixelShader;
  funcs.pfnDestroyShader = &DestroyShader;

  funcs.pfnCalcPrivateInputLayoutSize = &CalcPrivateInputLayoutSize;
  funcs.pfnCreateInputLayout = &CreateInputLayout;
  funcs.pfnDestroyInputLayout = &DestroyInputLayout;

  funcs.pfnCalcPrivateRTVSize = &CalcPrivateRTVSize;
  funcs.pfnCreateRTV = &CreateRTV;
  funcs.pfnDestroyRTV = &DestroyRTV;

  funcs.pfnCalcPrivateDSVSize = &CalcPrivateDSVSize;
  funcs.pfnCreateDSV = &CreateDSV;
  funcs.pfnDestroyDSV = &DestroyDSV;

  funcs.pfnCalcPrivateBlendStateSize = &CalcPrivateBlendStateSize;
  funcs.pfnCreateBlendState = &CreateBlendState;
  funcs.pfnDestroyBlendState = &DestroyBlendState;

  funcs.pfnCalcPrivateRasterizerStateSize = &CalcPrivateRasterizerStateSize;
  funcs.pfnCreateRasterizerState = &CreateRasterizerState;
  funcs.pfnDestroyRasterizerState = &DestroyRasterizerState;

  funcs.pfnCalcPrivateDepthStencilStateSize = &CalcPrivateDepthStencilStateSize;
  funcs.pfnCreateDepthStencilState = &CreateDepthStencilState;
  funcs.pfnDestroyDepthStencilState = &DestroyDepthStencilState;

  funcs.pfnSetRenderTargets = &SetRenderTargets;
  funcs.pfnClearRTV = &ClearRTV;
  funcs.pfnClearDSV = &ClearDSV;

  funcs.pfnSetInputLayout = &SetInputLayout;
  funcs.pfnSetVertexBuffer = &SetVertexBuffer;
  funcs.pfnSetIndexBuffer = &SetIndexBuffer;
  funcs.pfnSetViewport = &SetViewport;
  funcs.pfnSetDrawState = &SetDrawState;
  funcs.pfnSetBlendState = &SetBlendState;
  funcs.pfnSetRasterizerState = &SetRasterizerState;
  funcs.pfnSetDepthStencilState = &SetDepthStencilState;
  funcs.pfnSetPrimitiveTopology = &SetPrimitiveTopology;

  funcs.pfnDraw = &Draw;
  funcs.pfnDrawIndexed = &DrawIndexed;
  funcs.pfnPresent = &Present;
  funcs.pfnFlush = &Flush;
  funcs.pfnRotateResourceIdentities = &RotateResourceIdentities;

  *pCreateDevice->pDeviceFuncs = funcs;
  return S_OK;
}

void AEROGPU_APIENTRY CloseAdapter(D3D10DDI_HADAPTER hAdapter) {
  auto* adapter = FromHandle<D3D10DDI_HADAPTER, AeroGpuAdapter>(hAdapter);
  delete adapter;
}

// -------------------------------------------------------------------------------------------------
// Exported OpenAdapter entrypoints
// -------------------------------------------------------------------------------------------------

HRESULT OpenAdapterCommon(D3D10DDIARG_OPENADAPTER* pOpenData) {
  if (!pOpenData || !pOpenData->pAdapterFuncs) {
    return E_INVALIDARG;
  }

  auto* adapter = new AeroGpuAdapter();
  pOpenData->hAdapter.pDrvPrivate = adapter;

  D3D10DDI_ADAPTERFUNCS funcs = {};
  funcs.pfnCalcPrivateDeviceSize = &CalcPrivateDeviceSize;
  funcs.pfnCreateDevice = &CreateDevice;
  funcs.pfnCloseAdapter = &CloseAdapter;

  *pOpenData->pAdapterFuncs = funcs;
  return S_OK;
}

} // namespace

extern "C" {

HRESULT AEROGPU_APIENTRY OpenAdapter10(D3D10DDIARG_OPENADAPTER* pOpenData) {
  return OpenAdapterCommon(pOpenData);
}

HRESULT AEROGPU_APIENTRY OpenAdapter10_2(D3D10DDIARG_OPENADAPTER* pOpenData) {
  return OpenAdapterCommon(pOpenData);
}

HRESULT AEROGPU_APIENTRY OpenAdapter11(D3D10DDIARG_OPENADAPTER* pOpenData) {
  return OpenAdapterCommon(pOpenData);
}

} // extern "C"
