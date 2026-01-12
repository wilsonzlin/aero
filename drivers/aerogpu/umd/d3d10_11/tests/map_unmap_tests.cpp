#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <mutex>
#include <thread>
#include <utility>
#include <vector>

#include "aerogpu_d3d10_11_umd.h"
#include "aerogpu_cmd.h"

namespace {

constexpr uint32_t kDxgiFormatR8G8B8A8UnormSrgb = 29; // DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
constexpr uint32_t kDxgiFormatB8G8R8A8Unorm = 87; // DXGI_FORMAT_B8G8R8A8_UNORM
constexpr uint32_t kDxgiFormatB8G8R8A8UnormSrgb = 91; // DXGI_FORMAT_B8G8R8A8_UNORM_SRGB
constexpr uint32_t kDxgiFormatB8G8R8X8UnormSrgb = 93; // DXGI_FORMAT_B8G8R8X8_UNORM_SRGB
constexpr uint32_t kDxgiFormatBc1Unorm = 71; // DXGI_FORMAT_BC1_UNORM
constexpr uint32_t kDxgiFormatBc1UnormSrgb = 72; // DXGI_FORMAT_BC1_UNORM_SRGB
constexpr uint32_t kDxgiFormatBc2Unorm = 74; // DXGI_FORMAT_BC2_UNORM
constexpr uint32_t kDxgiFormatBc2UnormSrgb = 75; // DXGI_FORMAT_BC2_UNORM_SRGB
constexpr uint32_t kDxgiFormatBc3Unorm = 77; // DXGI_FORMAT_BC3_UNORM
constexpr uint32_t kDxgiFormatBc3UnormSrgb = 78; // DXGI_FORMAT_BC3_UNORM_SRGB
constexpr uint32_t kDxgiFormatBc7Unorm = 98; // DXGI_FORMAT_BC7_UNORM
constexpr uint32_t kDxgiFormatBc7UnormSrgb = 99; // DXGI_FORMAT_BC7_UNORM_SRGB

constexpr uint32_t kD3D11BindVertexBuffer = 0x1;
constexpr uint32_t kD3D11BindIndexBuffer = 0x2;
constexpr uint32_t kD3D11BindConstantBuffer = 0x4;

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

size_t AlignUp(size_t v, size_t a) {
  return (v + (a - 1)) & ~(a - 1);
}

uint32_t DivRoundUp(uint32_t v, uint32_t d) {
  return (v + (d - 1u)) / d;
}

struct DxgiTextureFormatLayout {
  uint32_t block_width = 1;
  uint32_t block_height = 1;
  uint32_t bytes_per_block = 4;
  bool valid = true;
};

DxgiTextureFormatLayout DxgiTextureFormat(uint32_t dxgi_format) {
  switch (dxgi_format) {
    case kDxgiFormatR8G8B8A8UnormSrgb:
    case kDxgiFormatB8G8R8A8Unorm:
    case kDxgiFormatB8G8R8A8UnormSrgb:
    case kDxgiFormatB8G8R8X8UnormSrgb:
      return DxgiTextureFormatLayout{1, 1, 4, true};
    case kDxgiFormatBc1Unorm:
    case kDxgiFormatBc1UnormSrgb:
      return DxgiTextureFormatLayout{4, 4, 8, true};
    case kDxgiFormatBc2Unorm:
    case kDxgiFormatBc2UnormSrgb:
    case kDxgiFormatBc3Unorm:
    case kDxgiFormatBc3UnormSrgb:
    case kDxgiFormatBc7Unorm:
    case kDxgiFormatBc7UnormSrgb:
      return DxgiTextureFormatLayout{4, 4, 16, true};
    default:
      // Tests default to 4BPP textures; use that as a safe fallback when a DXGI
      // format isn't modeled yet.
      return DxgiTextureFormatLayout{1, 1, 4, true};
  }
}

uint32_t DxgiTextureMinRowPitchBytes(uint32_t dxgi_format, uint32_t width) {
  if (width == 0) {
    return 0;
  }
  const DxgiTextureFormatLayout layout = DxgiTextureFormat(dxgi_format);
  if (!layout.valid || layout.block_width == 0 || layout.bytes_per_block == 0) {
    return 0;
  }
  const uint32_t blocks_w = DivRoundUp(width, layout.block_width);
  const uint64_t row_bytes = static_cast<uint64_t>(blocks_w) * static_cast<uint64_t>(layout.bytes_per_block);
  if (row_bytes == 0 || row_bytes > UINT32_MAX) {
    return 0;
  }
  return static_cast<uint32_t>(row_bytes);
}

uint32_t DxgiTextureNumRows(uint32_t dxgi_format, uint32_t height) {
  if (height == 0) {
    return 0;
  }
  const DxgiTextureFormatLayout layout = DxgiTextureFormat(dxgi_format);
  if (!layout.valid || layout.block_height == 0) {
    return 0;
  }
  return DivRoundUp(height, layout.block_height);
}

uint32_t CalcFullMipLevels(uint32_t width, uint32_t height) {
  uint32_t w = width ? width : 1u;
  uint32_t h = height ? height : 1u;
  uint32_t levels = 1;
  while (w > 1 || h > 1) {
    w = (w > 1) ? (w / 2) : 1u;
    h = (h > 1) ? (h / 2) : 1u;
    levels++;
  }
  return levels;
}

struct CmdLoc {
  const aerogpu_cmd_hdr* hdr = nullptr;
  size_t offset = 0;
};

size_t StreamBytesUsed(const uint8_t* buf, size_t len) {
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  const size_t used = static_cast<size_t>(stream->size_bytes);
  if (used < sizeof(aerogpu_cmd_stream_header) || used > len) {
    // Fall back to the provided buffer length when the header is malformed. Callers that require
    // strict validation should call ValidateStream first.
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
  // Forward-compat: allow the submission buffer to be larger than the stream header's declared
  // size (the header carries bytes-used; trailing bytes are ignored).
  if (!Check(stream->size_bytes <= len, "stream size_bytes within submitted length")) {
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
  return true;
}

CmdLoc FindLastOpcode(const uint8_t* buf, size_t len, uint32_t opcode) {
  CmdLoc loc{};
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return loc;
  }

  const size_t stream_len = StreamBytesUsed(buf, len);
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

size_t CountOpcode(const uint8_t* buf, size_t len, uint32_t opcode) {
  if (!buf || len < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }

  const size_t stream_len = StreamBytesUsed(buf, len);
  size_t count = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      count++;
    }
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return count;
}

struct Allocation {
  AEROGPU_WDDM_ALLOCATION_HANDLE handle = 0;
  std::vector<uint8_t> bytes;
};

struct Harness {
  std::vector<uint8_t> last_stream;
  std::vector<AEROGPU_WDDM_ALLOCATION_HANDLE> last_allocs;
  std::vector<HRESULT> errors;

  std::vector<Allocation> allocations;
  AEROGPU_WDDM_ALLOCATION_HANDLE next_handle = 1;

  // Optional async fence model used by tests that need to validate DO_NOT_WAIT
  // behavior without a real Win7/WDDM stack.
  bool async_fences = false;
  std::atomic<uint64_t> next_fence{1};
  std::atomic<uint64_t> last_submitted_fence{0};
  std::atomic<uint64_t> completed_fence{0};
  std::atomic<uint32_t> wait_call_count{0};
  std::atomic<uint32_t> last_wait_timeout_ms{0};
  std::mutex fence_mutex;
  std::condition_variable fence_cv;

  Allocation* FindAlloc(AEROGPU_WDDM_ALLOCATION_HANDLE handle) {
    for (auto& a : allocations) {
      if (a.handle == handle) {
        return &a;
      }
    }
    return nullptr;
  }

  static HRESULT AEROGPU_APIENTRY AllocateBacking(void* user,
                                                  const AEROGPU_DDIARG_CREATERESOURCE* desc,
                                                  AEROGPU_WDDM_ALLOCATION_HANDLE* out_handle,
                                                  uint64_t* out_size_bytes,
                                                  uint32_t* out_row_pitch_bytes) {
    if (!user || !desc || !out_handle || !out_size_bytes) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);

    Allocation alloc{};
    alloc.handle = h->next_handle++;

    if (out_row_pitch_bytes) {
      *out_row_pitch_bytes = 0;
    }

    uint64_t bytes = 0;
    if (desc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER) {
      bytes = desc->ByteWidth;
    } else if (desc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D) {
      const uint32_t width = desc->Width ? desc->Width : 1u;
      const uint32_t height = desc->Height ? desc->Height : 1u;
      uint32_t mip_levels = desc->MipLevels;
      if (mip_levels == 0) {
        mip_levels = CalcFullMipLevels(width, height);
      }
      const uint32_t array_layers = desc->ArraySize ? desc->ArraySize : 1u;

      const uint32_t tight_row_pitch = DxgiTextureMinRowPitchBytes(desc->Format, width);
      const uint32_t row_pitch = static_cast<uint32_t>(AlignUp(tight_row_pitch ? tight_row_pitch : (width * 4u), 64));
      if (out_row_pitch_bytes) {
        *out_row_pitch_bytes = row_pitch;
      }

      uint64_t layer_stride = 0;
      uint32_t level_w = width;
      uint32_t level_h = height;
      for (uint32_t level = 0; level < mip_levels; ++level) {
        const uint32_t tight_pitch = DxgiTextureMinRowPitchBytes(desc->Format, level_w);
        const uint32_t pitch = (level == 0) ? row_pitch : (tight_pitch ? tight_pitch : (level_w * 4u));
        const uint32_t rows = DxgiTextureNumRows(desc->Format, level_h);
        layer_stride += static_cast<uint64_t>(pitch) * static_cast<uint64_t>(rows ? rows : level_h);
        level_w = (level_w > 1) ? (level_w / 2) : 1u;
        level_h = (level_h > 1) ? (level_h / 2) : 1u;
      }
      bytes = layer_stride * static_cast<uint64_t>(array_layers);
    } else {
      bytes = desc->ByteWidth;
    }

    // Mirror the UMD's conservative alignment expectations.
    bytes = AlignUp(static_cast<size_t>(bytes), 256);
    alloc.bytes.resize(static_cast<size_t>(bytes), 0);

    h->allocations.push_back(std::move(alloc));
    *out_handle = h->allocations.back().handle;
    *out_size_bytes = bytes;
    return S_OK;
  }

  static HRESULT AEROGPU_APIENTRY MapAllocation(void* user, AEROGPU_WDDM_ALLOCATION_HANDLE handle, void** out_cpu_ptr) {
    if (!user || !out_cpu_ptr || handle == 0) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    Allocation* alloc = h->FindAlloc(handle);
    if (!alloc) {
      return E_INVALIDARG;
    }
    *out_cpu_ptr = alloc->bytes.data();
    return S_OK;
  }

  static void AEROGPU_APIENTRY UnmapAllocation(void* user, AEROGPU_WDDM_ALLOCATION_HANDLE handle) {
    (void)user;
    (void)handle;
  }

  static HRESULT AEROGPU_APIENTRY SubmitCmdStream(void* user,
                                                   const void* cmd_stream,
                                                   uint32_t cmd_stream_size_bytes,
                                                   const AEROGPU_WDDM_ALLOCATION_HANDLE* alloc_handles,
                                                   uint32_t alloc_count,
                                                   uint64_t* out_fence) {
    if (!user || !cmd_stream || cmd_stream_size_bytes < sizeof(aerogpu_cmd_stream_header)) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    const auto* bytes = reinterpret_cast<const uint8_t*>(cmd_stream);
    h->last_stream.assign(bytes, bytes + cmd_stream_size_bytes);
    if (!alloc_handles || alloc_count == 0) {
      h->last_allocs.clear();
    } else {
      h->last_allocs.assign(alloc_handles, alloc_handles + alloc_count);
    }
    if (out_fence) {
      if (h->async_fences) {
        const uint64_t fence = h->next_fence.fetch_add(1, std::memory_order_relaxed);
        h->last_submitted_fence.store(fence, std::memory_order_relaxed);
        *out_fence = fence;
      } else {
        *out_fence = 0;
      }
    }
    return S_OK;
  }

  static uint64_t AEROGPU_APIENTRY QueryCompletedFence(void* user) {
    if (!user) {
      return 0;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    return h->completed_fence.load(std::memory_order_relaxed);
  }

  static HRESULT AEROGPU_APIENTRY WaitForFence(void* user, uint64_t fence, uint32_t timeout_ms) {
    if (!user) {
      return E_INVALIDARG;
    }
    auto* h = reinterpret_cast<Harness*>(user);
    h->wait_call_count.fetch_add(1, std::memory_order_relaxed);
    h->last_wait_timeout_ms.store(timeout_ms, std::memory_order_relaxed);
    if (fence == 0) {
      return S_OK;
    }

    auto ready = [&]() { return h->completed_fence.load(std::memory_order_relaxed) >= fence; };
    if (ready()) {
      return S_OK;
    }
    if (timeout_ms == 0) {
      // `HRESULT_FROM_NT(STATUS_TIMEOUT)` is a SUCCEEDED() HRESULT on Win7-era
      // stacks; the UMD should still treat it as "not ready yet" for DO_NOT_WAIT.
      return static_cast<HRESULT>(0x10000102L);
    }

    std::unique_lock<std::mutex> lock(h->fence_mutex);
    if (timeout_ms == ~0u) {
      h->fence_cv.wait(lock, ready);
      return S_OK;
    }
    if (!h->fence_cv.wait_for(lock, std::chrono::milliseconds(timeout_ms), ready)) {
      // Match Win7-era status semantics used by the UMD poll path.
      return static_cast<HRESULT>(0x10000102L);
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
  Harness harness;

  D3D10DDI_HADAPTER hAdapter = {};
  D3D10DDI_ADAPTERFUNCS adapter_funcs = {};

  D3D10DDI_HDEVICE hDevice = {};
  AEROGPU_D3D10_11_DEVICEFUNCS device_funcs = {};
  std::vector<uint8_t> device_mem;

  AEROGPU_D3D10_11_DEVICECALLBACKS callbacks = {};
};

bool InitTestDevice(TestDevice* out, bool want_backing_allocations, bool async_fences) {
  if (!out) {
    return false;
  }

  out->harness.async_fences = async_fences;

  out->callbacks.pUserContext = &out->harness;
  out->callbacks.pfnSubmitCmdStream = &Harness::SubmitCmdStream;
  out->callbacks.pfnSetError = &Harness::SetError;
  if (async_fences) {
    out->callbacks.pfnWaitForFence = &Harness::WaitForFence;
  }
  if (want_backing_allocations) {
    out->callbacks.pfnAllocateBacking = &Harness::AllocateBacking;
    out->callbacks.pfnMapAllocation = &Harness::MapAllocation;
    out->callbacks.pfnUnmapAllocation = &Harness::UnmapAllocation;
  }

  D3D10DDIARG_OPENADAPTER open = {};
  open.pAdapterFuncs = &out->adapter_funcs;
  HRESULT hr = OpenAdapter10(&open);
  if (!Check(hr == S_OK, "OpenAdapter10")) {
    return false;
  }
  out->hAdapter = open.hAdapter;

  // CreateDevice contract.
  D3D10DDIARG_CREATEDEVICE create = {};
  create.hDevice.pDrvPrivate = nullptr;
  const SIZE_T dev_size = out->adapter_funcs.pfnCalcPrivateDeviceSize(out->hAdapter, &create);
  if (!Check(dev_size >= sizeof(void*), "CalcPrivateDeviceSize returned a non-trivial size")) {
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

struct TestResource {
  D3D10DDI_HRESOURCE hResource = {};
  std::vector<uint8_t> storage;
};

struct TestShaderResourceView {
  D3D10DDI_HSHADERRESOURCEVIEW hView = {};
  std::vector<uint8_t> storage;
};

bool CreateBuffer(TestDevice* dev,
                  uint32_t byte_width,
                  uint32_t usage,
                  uint32_t bind_flags,
                  uint32_t cpu_access_flags,
                  TestResource* out) {
  if (!dev || !out) {
    return false;
  }

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER;
  desc.BindFlags = bind_flags;
  desc.MiscFlags = 0;
  desc.Usage = usage;
  desc.CPUAccessFlags = cpu_access_flags;
  desc.ByteWidth = byte_width;
  desc.StructureByteStride = 0;
  desc.pInitialData = nullptr;
  desc.InitialDataCount = 0;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  if (!Check(hr == S_OK, "CreateResource(buffer)")) {
    return false;
  }
  return true;
}

bool CreateStagingBuffer(TestDevice* dev,
                         uint32_t byte_width,
                         uint32_t cpu_access_flags,
                         TestResource* out) {
  return CreateBuffer(dev,
                      byte_width,
                      AEROGPU_D3D11_USAGE_STAGING,
                      /*bind_flags=*/0,
                      cpu_access_flags,
                      out);
}

bool CreateBufferWithInitialData(TestDevice* dev,
                                 uint32_t byte_width,
                                 uint32_t usage,
                                 uint32_t bind_flags,
                                 uint32_t cpu_access_flags,
                                 const void* initial_bytes,
                                 TestResource* out) {
  if (!dev || !out || !initial_bytes) {
    return false;
  }

  AEROGPU_DDI_SUBRESOURCE_DATA init = {};
  init.pSysMem = initial_bytes;
  init.SysMemPitch = 0;
  init.SysMemSlicePitch = 0;

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER;
  desc.BindFlags = bind_flags;
  desc.MiscFlags = 0;
  desc.Usage = usage;
  desc.CPUAccessFlags = cpu_access_flags;
  desc.ByteWidth = byte_width;
  desc.StructureByteStride = 0;
  desc.pInitialData = &init;
  desc.InitialDataCount = 1;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  if (!Check(hr == S_OK, "CreateResource(buffer initial data)")) {
    return false;
  }
  return true;
}

bool CreateStagingTexture2DWithFormatAndDesc(TestDevice* dev,
                                             uint32_t width,
                                             uint32_t height,
                                             uint32_t dxgi_format,
                                             uint32_t cpu_access_flags,
                                             uint32_t mip_levels,
                                             uint32_t array_size,
                                             TestResource* out) {
  if (!dev || !out) {
    return false;
  }

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  desc.BindFlags = 0;
  desc.MiscFlags = 0;
  desc.Usage = AEROGPU_D3D11_USAGE_STAGING;
  desc.CPUAccessFlags = cpu_access_flags;
  desc.Width = width;
  desc.Height = height;
  desc.MipLevels = mip_levels;
  desc.ArraySize = array_size;
  desc.Format = dxgi_format;
  desc.pInitialData = nullptr;
  desc.InitialDataCount = 0;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  if (!Check(hr == S_OK, "CreateResource(tex2d)")) {
    return false;
  }
  return true;
}

bool CreateStagingTexture2DWithFormat(TestDevice* dev,
                                      uint32_t width,
                                      uint32_t height,
                                      uint32_t dxgi_format,
                                      uint32_t cpu_access_flags,
                                      TestResource* out) {
  return CreateStagingTexture2DWithFormatAndDesc(dev,
                                                 width,
                                                 height,
                                                 dxgi_format,
                                                 cpu_access_flags,
                                                 /*mip_levels=*/1,
                                                 /*array_size=*/1,
                                                 out);
}

bool CreateStagingTexture2D(TestDevice* dev,
                            uint32_t width,
                            uint32_t height,
                            uint32_t cpu_access_flags,
                            TestResource* out) {
  return CreateStagingTexture2DWithFormat(dev, width, height, kDxgiFormatB8G8R8A8Unorm, cpu_access_flags, out);
}

bool CreateShaderResourceView(TestDevice* dev, TestResource* tex, TestShaderResourceView* out) {
  if (!dev || !tex || !out) {
    return false;
  }

  AEROGPU_DDIARG_CREATESHADERRESOURCEVIEW desc = {};
  desc.hResource = tex->hResource;
  desc.Format = 0;
  desc.ViewDimension = AEROGPU_DDI_SRV_DIMENSION_TEXTURE2D;
  desc.MostDetailedMip = 0;
  desc.MipLevels = 1;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateShaderResourceViewSize(dev->hDevice, &desc);
  // Unlike resources (which must at least hold a pointer-sized `hResource.pDrvPrivate`),
  // a view's private storage can be smaller than `sizeof(void*)` (our current SRV
  // backing struct is 4 bytes). Still require a non-zero size so the function is
  // implemented.
  if (!Check(size != 0, "CalcPrivateShaderResourceViewSize returned a non-zero size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hView.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateShaderResourceView(dev->hDevice, &desc, out->hView);
  if (!Check(hr == S_OK, "CreateShaderResourceView")) {
    return false;
  }
  return true;
}

bool CreateTexture2DWithInitialData(TestDevice* dev,
                                    uint32_t width,
                                    uint32_t height,
                                    uint32_t usage,
                                    uint32_t bind_flags,
                                    uint32_t cpu_access_flags,
                                    const void* initial_bytes,
                                    uint32_t initial_row_pitch,
                                    TestResource* out,
                                    uint32_t dxgi_format = kDxgiFormatB8G8R8A8Unorm) {
  if (!dev || !out || !initial_bytes) {
    return false;
  }

  AEROGPU_DDI_SUBRESOURCE_DATA init = {};
  init.pSysMem = initial_bytes;
  init.SysMemPitch = initial_row_pitch;
  init.SysMemSlicePitch = 0;

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  desc.BindFlags = bind_flags;
  desc.MiscFlags = 0;
  desc.Usage = usage;
  desc.CPUAccessFlags = cpu_access_flags;
  desc.Width = width;
  desc.Height = height;
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = dxgi_format;
  desc.pInitialData = &init;
  desc.InitialDataCount = 1;

  const SIZE_T size = dev->device_funcs.pfnCalcPrivateResourceSize(dev->hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }

  out->storage.assign(static_cast<size_t>(size), 0);
  out->hResource.pDrvPrivate = out->storage.data();

  const HRESULT hr = dev->device_funcs.pfnCreateResource(dev->hDevice, &desc, out->hResource);
  if (!Check(hr == S_OK, "CreateResource(tex2d initial data)")) {
    return false;
  }
  return true;
}

bool TestHostOwnedBufferUnmapUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(host-owned)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &buf), "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map(WRITE) host-owned")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }

  const uint8_t expected[16] = {
      0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF};
  std::memcpy(mapped.pData, expected, sizeof(expected));

  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after Unmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned Unmap should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned Unmap should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_BUFFER backing_alloc_id == 0")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(expected), "UPLOAD_RESOURCE size_bytes == 16")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  const size_t payload_size = static_cast<size_t>(upload_cmd->size_bytes);
  if (!Check(payload_offset + payload_size <= stream_len, "UPLOAD_RESOURCE payload fits in stream")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected, payload_size) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedTextureUnmapUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(host-owned tex2d)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_WRITE, &tex),
             "CreateStagingTexture2D")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      tex.hResource,
                                                      /*subresource=*/0,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) host-owned tex2d")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch == 12, "RowPitch == width*4 for host-owned tex2d")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bpp = 4;
  const uint32_t bytes_per_row = width * bpp;
  const uint32_t row_pitch = mapped.RowPitch;
  const size_t total_bytes = static_cast<size_t>(row_pitch) * height;
  std::vector<uint8_t> expected(total_bytes, 0);

  auto* dst = static_cast<uint8_t*>(mapped.pData);
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      const uint8_t v = static_cast<uint8_t>((y * 17u) + x);
      dst[static_cast<size_t>(y) * row_pitch + x] = v;
      expected[static_cast<size_t>(y) * row_pitch + x] = v;
    }
  }

  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after tex2d Unmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned tex2d Unmap should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned tex2d Unmap should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_TEXTURE2D backing_alloc_id == 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes == row_pitch, "CREATE_TEXTURE2D row_pitch_bytes matches Map pitch")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == expected.size(), "UPLOAD_RESOURCE size matches tex2d bytes")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  const size_t payload_size = static_cast<size_t>(upload_cmd->size_bytes);
  if (!Check(payload_offset + payload_size <= stream_len, "UPLOAD_RESOURCE payload fits in stream")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected.data(), payload_size) == 0,
             "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned tex2d submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestCreateTexture2dSrgbFormatEncodesSrgbAerogpuFormat() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(create tex2d sRGB)")) {
    return false;
  }

  constexpr uint32_t width = 5;
  constexpr uint32_t height = 7;
  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                              width,
                                              height,
                                              kDxgiFormatB8G8R8A8UnormSrgb,
                                              /*cpu_access_flags=*/0,
                                              &tex),
             "CreateStagingTexture2DWithFormat(sRGB)")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource(sRGB tex2d)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }

  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->width == width, "CREATE_TEXTURE2D width matches")) {
    return false;
  }
  if (!Check(create_cmd->height == height, "CREATE_TEXTURE2D height matches")) {
    return false;
  }
  if (!Check(create_cmd->format == AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB,
             "CREATE_TEXTURE2D format is AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedBufferUnmapDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(guest-backed)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &buf), "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map(WRITE) guest-backed")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }

  const uint8_t expected[16] = {
      0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x10, 0x32, 0x54, 0x76, 0x98, 0xBA, 0xDC, 0xFE};
  std::memcpy(mapped.pData, expected, sizeof(expected));

  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after Unmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed Unmap should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed Unmap should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sizeof(expected), "RESOURCE_DIRTY_RANGE size_bytes == 16")) {
    return false;
  }

  bool found_alloc = false;
  for (auto h : dev.harness.last_allocs) {
    if (h == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed submit alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists in harness")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= sizeof(expected), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected, sizeof(expected)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedTextureUnmapDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(guest-backed tex2d)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_WRITE, &tex),
             "CreateStagingTexture2D")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      tex.hResource,
                                                      /*subresource=*/0,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) guest-backed tex2d")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch != 0, "Map returned non-zero RowPitch")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bpp = 4;
  const uint32_t bytes_per_row = width * bpp;
  const uint32_t row_pitch = mapped.RowPitch;
  const size_t total_bytes = static_cast<size_t>(row_pitch) * height;
  std::vector<uint8_t> expected(total_bytes, 0xCD);

  auto* dst = static_cast<uint8_t*>(mapped.pData);
  for (uint32_t y = 0; y < height; y++) {
    uint8_t* row = dst + static_cast<size_t>(y) * row_pitch;
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      const uint8_t v = static_cast<uint8_t>((y * 31u) + x);
      row[x] = v;
      expected[static_cast<size_t>(y) * row_pitch + x] = v;
    }
    if (row_pitch > bytes_per_row) {
      std::memset(row + bytes_per_row, 0xCD, row_pitch - bytes_per_row);
    }
  }

  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after tex2d Unmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed tex2d Unmap should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed tex2d Unmap should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes == row_pitch, "CREATE_TEXTURE2D row_pitch_bytes matches Map pitch")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == expected.size(), "RESOURCE_DIRTY_RANGE size matches tex2d bytes")) {
    return false;
  }

  bool found_alloc = false;
  for (auto h : dev.harness.last_allocs) {
    if (h == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed tex2d submit alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists in harness")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= expected.size(), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0,
             "guest-backed allocation bytes reflect CPU writes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedBcTextureUnmapDirtyRange() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(guest-backed bc tex2d)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC1_UNORM_SRGB", kDxgiFormatBc1UnormSrgb, AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, 8},
      {"DXGI_FORMAT_BC2_UNORM", kDxgiFormatBc2Unorm, AEROGPU_FORMAT_BC2_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC2_UNORM_SRGB", kDxgiFormatBc2UnormSrgb, AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC3_UNORM", kDxgiFormatBc3Unorm, AEROGPU_FORMAT_BC3_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC3_UNORM_SRGB", kDxgiFormatBc3UnormSrgb, AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC7_UNORM_SRGB", kDxgiFormatBc7UnormSrgb, AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &tex),
               "CreateStagingTexture2DWithFormat(guest-backed bc)")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        tex.hResource,
                                                        /*subresource=*/0,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) guest-backed bc tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map returned non-zero RowPitch")) {
      return false;
    }

    const uint32_t blocks_w = div_round_up(kWidth, 4);
    const uint32_t blocks_h = div_round_up(kHeight, 4);
    const uint32_t required_row_bytes = blocks_w * c.block_bytes;
    if (!Check(mapped.RowPitch >= required_row_bytes, "Map RowPitch large enough for BC row")) {
      return false;
    }
    const uint32_t expected_depth_pitch = mapped.RowPitch * blocks_h;
    if (!Check(mapped.DepthPitch == expected_depth_pitch, "Map DepthPitch matches BC block rows")) {
      return false;
    }

    const uint32_t row_pitch = mapped.RowPitch;
    std::vector<uint8_t> expected(static_cast<size_t>(expected_depth_pitch), 0xCD);
    auto* dst = static_cast<uint8_t*>(mapped.pData);
    for (uint32_t y = 0; y < blocks_h; y++) {
      uint8_t* row = dst + static_cast<size_t>(y) * row_pitch;
      for (uint32_t x = 0; x < required_row_bytes; x++) {
        const uint8_t v = static_cast<uint8_t>((y + 1u) * 31u + x);
        row[x] = v;
        expected[static_cast<size_t>(y) * row_pitch + x] = v;
      }
      if (row_pitch > required_row_bytes) {
        std::memset(row + required_row_bytes, 0xCD, row_pitch - required_row_bytes);
      }
    }

    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);
    hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after guest-backed bc tex2d Unmap")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
               "guest-backed bc tex2d Unmap should not emit UPLOAD_RESOURCE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
               "guest-backed bc tex2d Unmap should emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
    if (!Check(create_cmd->format == c.expected_format, "CREATE_TEXTURE2D format matches expected")) {
      return false;
    }
    if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
      return false;
    }
    if (!Check(create_cmd->row_pitch_bytes == row_pitch, "CREATE_TEXTURE2D row_pitch_bytes matches Map pitch")) {
      return false;
    }

    CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
      return false;
    }
    const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
    if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
      return false;
    }
    if (!Check(dirty_cmd->size_bytes == expected.size(), "RESOURCE_DIRTY_RANGE size matches BC tex2d bytes")) {
      return false;
    }

    bool found_alloc = false;
    for (auto h : dev.harness.last_allocs) {
      if (h == create_cmd->backing_alloc_id) {
        found_alloc = true;
      }
    }
    if (!Check(found_alloc, "guest-backed bc tex2d submit alloc list contains backing alloc")) {
      return false;
    }

    Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
    if (!Check(alloc != nullptr, "backing allocation exists in harness")) {
      return false;
    }
    if (!Check(alloc->bytes.size() >= expected.size(), "backing allocation large enough")) {
      return false;
    }
    if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0,
               "guest-backed allocation bytes reflect CPU writes")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestMapUsageValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(validation)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_READ, &buf), "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  const HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                             buf.hResource,
                                             /*subresource=*/0,
                                             AEROGPU_DDI_MAP_WRITE,
                                             /*map_flags=*/0,
                                             &mapped);
  if (!Check(hr == E_INVALIDARG, "Map(WRITE) on READ-only staging resource should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestMapFlagsValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(map flags)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &buf), "CreateStagingBuffer")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  const HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                             buf.hResource,
                                             /*subresource=*/0,
                                             AEROGPU_DDI_MAP_WRITE,
                                             /*map_flags=*/0x1,
                                             &mapped);
  if (!Check(hr == E_INVALIDARG, "Map with unknown MapFlags bits should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestMapDoNotWaitReportsStillDrawing() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/true),
             "InitTestDevice(map DO_NOT_WAIT)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_READ, &buf), "CreateStagingBuffer")) {
    return false;
  }

  dev.harness.completed_fence.store(0, std::memory_order_relaxed);
  const HRESULT flush_hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(flush_hr == S_OK, "Flush to create pending fence")) {
    return false;
  }
  const uint64_t pending_fence = dev.harness.last_submitted_fence.load(std::memory_order_relaxed);
  if (!Check(pending_fence != 0, "Flush returned a non-zero fence")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  dev.harness.wait_call_count.store(0, std::memory_order_relaxed);
  dev.harness.last_wait_timeout_ms.store(~0u, std::memory_order_relaxed);
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_READ,
                                       AEROGPU_D3D11_MAP_FLAG_DO_NOT_WAIT,
                                       &mapped);
  if (!Check(hr == DXGI_ERROR_WAS_STILL_DRAWING, "Map(DO_NOT_WAIT) should return DXGI_ERROR_WAS_STILL_DRAWING")) {
    return false;
  }
  if (!Check(dev.harness.wait_call_count.load(std::memory_order_relaxed) == 1,
             "Map(DO_NOT_WAIT) should issue exactly one fence wait poll")) {
    return false;
  }
  if (!Check(dev.harness.last_wait_timeout_ms.load(std::memory_order_relaxed) == 0,
             "Map(DO_NOT_WAIT) should pass timeout_ms=0 to fence wait")) {
    return false;
  }

  // Mark the fence complete and retry; DO_NOT_WAIT should now succeed.
  dev.harness.completed_fence.store(pending_fence, std::memory_order_relaxed);
  dev.harness.fence_cv.notify_all();

  mapped = {};
  dev.harness.wait_call_count.store(0, std::memory_order_relaxed);
  dev.harness.last_wait_timeout_ms.store(~0u, std::memory_order_relaxed);
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               buf.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_READ,
                               AEROGPU_D3D11_MAP_FLAG_DO_NOT_WAIT,
                               &mapped);
  if (!Check(hr == S_OK, "Map(DO_NOT_WAIT) should succeed once fence is complete")) {
    return false;
  }
  if (!Check(dev.harness.wait_call_count.load(std::memory_order_relaxed) == 1,
             "Map(DO_NOT_WAIT) retry should poll fence once")) {
    return false;
  }
  if (!Check(dev.harness.last_wait_timeout_ms.load(std::memory_order_relaxed) == 0,
             "Map(DO_NOT_WAIT) retry should still pass timeout_ms=0")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned a non-null pointer")) {
    return false;
  }
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestMapBlockingWaitUsesInfiniteTimeout() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/true),
             "InitTestDevice(map blocking wait)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_READ, &buf), "CreateStagingBuffer")) {
    return false;
  }

  dev.harness.completed_fence.store(0, std::memory_order_relaxed);
  const HRESULT flush_hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(flush_hr == S_OK, "Flush to create pending fence")) {
    return false;
  }
  const uint64_t pending_fence = dev.harness.last_submitted_fence.load(std::memory_order_relaxed);
  if (!Check(pending_fence != 0, "Flush returned a non-zero fence")) {
    return false;
  }

  // Simulate completion so a blocking Map can succeed, but still force the UMD
  // to call into the wait callback (its pre-check uses the UMD's internal fence
  // cache, not the harness value).
  dev.harness.completed_fence.store(pending_fence, std::memory_order_relaxed);

  dev.harness.wait_call_count.store(0, std::memory_order_relaxed);
  dev.harness.last_wait_timeout_ms.store(0, std::memory_order_relaxed);

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  const HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                             buf.hResource,
                                             /*subresource=*/0,
                                             AEROGPU_DDI_MAP_READ,
                                             /*map_flags=*/0,
                                             &mapped);
  if (!Check(hr == S_OK, "Map(READ) should succeed once fence is complete")) {
    return false;
  }
  if (!Check(dev.harness.wait_call_count.load(std::memory_order_relaxed) == 1,
             "Map(READ) should issue exactly one blocking fence wait")) {
    return false;
  }
  if (!Check(dev.harness.last_wait_timeout_ms.load(std::memory_order_relaxed) == ~0u,
             "Map(READ) should pass timeout_ms=~0u to fence wait")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned a non-null pointer")) {
    return false;
  }
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestInvalidUnmapReportsError() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(invalid unmap)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &buf), "CreateStagingBuffer")) {
    return false;
  }

  dev.harness.errors.clear();
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);
  if (!Check(dev.harness.errors.size() == 1, "Unmap without Map should report one error")) {
    return false;
  }
  if (!Check(dev.harness.errors[0] == E_INVALIDARG, "Unmap without Map should report E_INVALIDARG")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       buf.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map after invalid Unmap")) {
    return false;
  }

  dev.harness.errors.clear();
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/1);
  if (!Check(dev.harness.errors.size() == 1, "Unmap with wrong subresource should report one error")) {
    return false;
  }
  if (!Check(dev.harness.errors[0] == E_INVALIDARG, "Unmap wrong subresource should report E_INVALIDARG")) {
    return false;
  }

  dev.harness.errors.clear();
  dev.device_funcs.pfnUnmap(dev.hDevice, buf.hResource, /*subresource=*/0);
  if (!Check(dev.harness.errors.empty(), "Valid Unmap should not report errors")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestDynamicMapFlagsValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(dynamic map flags)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindVertexBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &buf),
             "CreateBuffer(dynamic VB)")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  const HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                             buf.hResource,
                                             /*subresource=*/0,
                                             AEROGPU_DDI_MAP_WRITE_DISCARD,
                                             /*map_flags=*/0x1,
                                             &mapped);
  if (!Check(hr == E_INVALIDARG, "MapDiscard with unknown MapFlags bits should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedDynamicIABufferUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(dynamic ia host-owned)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindVertexBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &buf),
             "CreateBuffer(dynamic VB)")) {
    return false;
  }

  void* data = nullptr;
  HRESULT hr = dev.device_funcs.pfnDynamicIABufferMapDiscard(dev.hDevice, buf.hResource, &data);
  if (!Check(hr == S_OK, "DynamicIABufferMapDiscard host-owned")) {
    return false;
  }
  if (!Check(data != nullptr, "DynamicIABufferMapDiscard returned data")) {
    return false;
  }

  uint8_t expected[32] = {};
  for (size_t i = 0; i < sizeof(expected); i++) {
    expected[i] = static_cast<uint8_t>(i * 7u);
  }
  std::memcpy(data, expected, sizeof(expected));

  dev.device_funcs.pfnDynamicIABufferUnmap(dev.hDevice, buf.hResource);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after DynamicIABufferUnmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned dynamic ia Unmap should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned dynamic ia Unmap should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "dynamic VB CREATE_BUFFER backing_alloc_id == 0")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(expected), "UPLOAD_RESOURCE size matches dynamic VB")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  const size_t payload_size = static_cast<size_t>(upload_cmd->size_bytes);
  if (!Check(payload_offset + payload_size <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected, payload_size) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned dynamic ia submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedDynamicIABufferDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(dynamic ia guest-backed)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindVertexBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &buf),
             "CreateBuffer(dynamic VB)")) {
    return false;
  }

  void* data = nullptr;
  HRESULT hr = dev.device_funcs.pfnDynamicIABufferMapDiscard(dev.hDevice, buf.hResource, &data);
  if (!Check(hr == S_OK, "DynamicIABufferMapDiscard guest-backed")) {
    return false;
  }
  if (!Check(data != nullptr, "DynamicIABufferMapDiscard returned data")) {
    return false;
  }

  uint8_t expected[32] = {};
  for (size_t i = 0; i < sizeof(expected); i++) {
    expected[i] = static_cast<uint8_t>(0xA0u + i);
  }
  std::memcpy(data, expected, sizeof(expected));

  dev.device_funcs.pfnDynamicIABufferUnmap(dev.hDevice, buf.hResource);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after DynamicIABufferUnmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed dynamic ia Unmap should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed dynamic ia Unmap should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "dynamic VB CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sizeof(expected), "RESOURCE_DIRTY_RANGE size matches dynamic VB")) {
    return false;
  }

  bool found_alloc = false;
  for (auto h : dev.harness.last_allocs) {
    if (h == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed dynamic ia submit alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists in harness")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= sizeof(expected), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected, sizeof(expected)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestDynamicBufferUsageValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(dynamic validation)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DEFAULT,
                          kD3D11BindVertexBuffer,
                          /*cpu_access_flags=*/0,
                          &buf),
             "CreateBuffer(default VB)")) {
    return false;
  }

  void* data = nullptr;
  const HRESULT hr = dev.device_funcs.pfnDynamicIABufferMapDiscard(dev.hDevice, buf.hResource, &data);
  if (!Check(hr == E_INVALIDARG, "DynamicIABufferMapDiscard on non-dynamic resource should fail")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedDynamicConstantBufferUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(dynamic cb host-owned)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindConstantBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &buf),
             "CreateBuffer(dynamic CB)")) {
    return false;
  }

  void* data = nullptr;
  HRESULT hr = dev.device_funcs.pfnDynamicConstantBufferMapDiscard(dev.hDevice, buf.hResource, &data);
  if (!Check(hr == S_OK, "DynamicConstantBufferMapDiscard host-owned")) {
    return false;
  }
  if (!Check(data != nullptr, "DynamicConstantBufferMapDiscard returned data")) {
    return false;
  }

  uint8_t expected[32] = {};
  for (size_t i = 0; i < sizeof(expected); i++) {
    expected[i] = static_cast<uint8_t>(0x20u + i);
  }
  std::memcpy(data, expected, sizeof(expected));

  dev.device_funcs.pfnDynamicConstantBufferUnmap(dev.hDevice, buf.hResource);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after DynamicConstantBufferUnmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned dynamic CB Unmap should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned dynamic CB Unmap should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "dynamic CB CREATE_BUFFER backing_alloc_id == 0")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(expected), "UPLOAD_RESOURCE size matches dynamic CB")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  const size_t payload_size = static_cast<size_t>(upload_cmd->size_bytes);
  if (!Check(payload_offset + payload_size <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected, payload_size) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned dynamic CB submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedDynamicConstantBufferDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(dynamic cb guest-backed)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindConstantBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &buf),
             "CreateBuffer(dynamic CB)")) {
    return false;
  }

  void* data = nullptr;
  HRESULT hr = dev.device_funcs.pfnDynamicConstantBufferMapDiscard(dev.hDevice, buf.hResource, &data);
  if (!Check(hr == S_OK, "DynamicConstantBufferMapDiscard guest-backed")) {
    return false;
  }
  if (!Check(data != nullptr, "DynamicConstantBufferMapDiscard returned data")) {
    return false;
  }

  uint8_t expected[32] = {};
  for (size_t i = 0; i < sizeof(expected); i++) {
    expected[i] = static_cast<uint8_t>(0xC0u + i);
  }
  std::memcpy(data, expected, sizeof(expected));

  dev.device_funcs.pfnDynamicConstantBufferUnmap(dev.hDevice, buf.hResource);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after DynamicConstantBufferUnmap")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed dynamic CB Unmap should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed dynamic CB Unmap should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "dynamic CB CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sizeof(expected), "RESOURCE_DIRTY_RANGE size matches dynamic CB")) {
    return false;
  }

  bool found_alloc = false;
  for (auto h : dev.harness.last_allocs) {
    if (h == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed dynamic CB submit alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists in harness")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= sizeof(expected), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected, sizeof(expected)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedCopyResourceBufferReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(copy buffer host-owned)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src), "CreateStagingBuffer(src)")) {
    return false;
  }
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_READ, &dst), "CreateStagingBuffer(dst)")) {
    return false;
  }

  const uint8_t expected[16] = {0x5A, 0x4B, 0x3C, 0x2D, 0x1E, 0x0F, 0xAA, 0xBB,
                                0xCC, 0xDD, 0xEE, 0xFF, 0x10, 0x20, 0x30, 0x40};

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       src.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map(WRITE) src buffer")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  std::memcpy(mapped.pData, expected, sizeof(expected));
  dev.device_funcs.pfnUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               dst.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_READ,
                               /*map_flags=*/0,
                               &readback);
  if (!Check(hr == S_OK, "Map(READ) dst buffer")) {
    return false;
  }
  if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
    return false;
  }
  if (!Check(std::memcmp(readback.pData, expected, sizeof(expected)) == 0, "CopyResource buffer bytes")) {
    return false;
  }
  dev.device_funcs.pfnUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_BUFFER) == 1, "COPY_BUFFER emitted")) {
    return false;
  }
  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_BUFFER);
  if (!Check(copy_loc.hdr != nullptr, "COPY_BUFFER location")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_buffer*>(stream + copy_loc.offset);
  if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) == 0, "COPY_BUFFER must not have WRITEBACK_DST flag")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSubmitAllocListTracksBoundConstantBuffer() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(track CB alloc)")) {
    return false;
  }

  TestResource cb{};
  if (!Check(CreateBuffer(&dev,
                          /*byte_width=*/32,
                          AEROGPU_D3D11_USAGE_DYNAMIC,
                          kD3D11BindConstantBuffer,
                          AEROGPU_D3D11_CPU_ACCESS_WRITE,
                          &cb),
             "CreateBuffer(dynamic CB)")) {
    return false;
  }

  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource(dynamic CB)")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd =
      reinterpret_cast<const aerogpu_cmd_create_buffer*>(dev.harness.last_stream.data() + create_loc.offset);
  const AEROGPU_WDDM_ALLOCATION_HANDLE backing = create_cmd->backing_alloc_id;
  if (!Check(backing != 0, "CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  // Flush clears the device's referenced allocation list. Binding the CB should
  // repopulate it before the next submission.
  D3D10DDI_HRESOURCE buffers[1] = {cb.hResource};
  dev.device_funcs.pfnVsSetConstantBuffers(dev.hDevice, /*start_slot=*/0, /*buffer_count=*/1, buffers);

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after VsSetConstantBuffers")) {
    return false;
  }

  bool found = false;
  for (auto h : dev.harness.last_allocs) {
    if (h == backing) {
      found = true;
      break;
    }
  }
  if (!Check(found, "submit alloc list contains bound constant buffer allocation")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, cb.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedCopyResourceTextureReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(copy tex2d host-owned)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src),
             "CreateStagingTexture2D(src)")) {
    return false;
  }
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_READ, &dst),
             "CreateStagingTexture2D(dst)")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      src.hResource,
                                                      /*subresource=*/0,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src tex2d")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch != 0, "Map returned RowPitch")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  const uint32_t row_pitch = mapped.RowPitch;
  auto* src_bytes = static_cast<uint8_t*>(mapped.pData);
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      src_bytes[static_cast<size_t>(y) * row_pitch + x] = static_cast<uint8_t>((y + 1) * 19u + x);
    }
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              dst.hResource,
                                              /*subresource=*/0,
                                              AEROGPU_DDI_MAP_READ,
                                              /*map_flags=*/0,
                                              &readback);
  if (!Check(hr == S_OK, "StagingResourceMap(READ) dst tex2d")) {
    return false;
  }
  if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
    return false;
  }
  if (!Check(readback.RowPitch == row_pitch, "dst RowPitch matches src RowPitch")) {
    return false;
  }

  const auto* dst_bytes = static_cast<const uint8_t*>(readback.pData);
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      const uint8_t expected = static_cast<uint8_t>((y + 1) * 19u + x);
      if (!Check(dst_bytes[static_cast<size_t>(y) * row_pitch + x] == expected, "CopyResource tex2d pixel bytes")) {
        return false;
      }
    }
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
    return false;
  }
  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
  if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
  if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) == 0, "COPY_TEXTURE2D must not have WRITEBACK_DST flag")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedCopyResourceBcTextureReadback() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(copy bc tex2d host-owned)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, 8},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    TestResource src{};
    TestResource dst{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &src),
               "CreateStagingTexture2DWithFormat(src bc)")) {
      return false;
    }
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &dst),
               "CreateStagingTexture2DWithFormat(dst bc)")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        src.hResource,
                                                        /*subresource=*/0,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src bc tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map returned RowPitch")) {
      return false;
    }

    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const uint32_t row_pitch = mapped.RowPitch;
    const uint32_t depth_pitch = mapped.DepthPitch;
    if (!Check(row_pitch == row_bytes, "Map RowPitch matches tight BC row bytes (host-owned)")) {
      return false;
    }
    if (!Check(depth_pitch == row_pitch * blocks_h, "Map DepthPitch matches BC block rows")) {
      return false;
    }

    std::vector<uint8_t> expected(static_cast<size_t>(depth_pitch), 0);
    auto* src_bytes = static_cast<uint8_t*>(mapped.pData);
    for (uint32_t y = 0; y < blocks_h; y++) {
      for (uint32_t x = 0; x < row_bytes; x++) {
        const uint8_t v = static_cast<uint8_t>((y + 1u) * 19u + x);
        src_bytes[static_cast<size_t>(y) * row_pitch + x] = v;
        expected[static_cast<size_t>(y) * row_pitch + x] = v;
      }
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

    dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

    AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
    hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                dst.hResource,
                                                /*subresource=*/0,
                                                AEROGPU_DDI_MAP_READ,
                                                /*map_flags=*/0,
                                                &readback);
    if (!Check(hr == S_OK, "StagingResourceMap(READ) dst bc tex2d")) {
      return false;
    }
    if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
      return false;
    }
    if (!Check(readback.RowPitch == row_pitch, "dst RowPitch matches src RowPitch")) {
      return false;
    }
    if (!Check(readback.DepthPitch == depth_pitch, "dst DepthPitch matches src DepthPitch")) {
      return false;
    }
    if (!Check(std::memcmp(readback.pData, expected.data(), expected.size()) == 0, "CopyResource bc tex2d bytes")) {
      return false;
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
      return false;
    }
    CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
    if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
      return false;
    }
    const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
    if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) == 0,
               "COPY_TEXTURE2D must not have WRITEBACK_DST flag")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
    dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestHostOwnedCopySubresourceRegionBcTextureReadback() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(copy subresource bc tex2d host-owned)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, 8},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    TestResource src{};
    TestResource dst{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &src),
               "CreateStagingTexture2DWithFormat(src bc)")) {
      return false;
    }
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &dst),
               "CreateStagingTexture2DWithFormat(dst bc)")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        src.hResource,
                                                        /*subresource=*/0,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src bc tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map returned RowPitch")) {
      return false;
    }

    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const uint32_t row_pitch = mapped.RowPitch;
    const uint32_t depth_pitch = mapped.DepthPitch;
    if (!Check(row_pitch == row_bytes, "Map RowPitch matches tight BC row bytes (host-owned)")) {
      return false;
    }
    if (!Check(depth_pitch == row_pitch * blocks_h, "Map DepthPitch matches BC block rows")) {
      return false;
    }

    std::vector<uint8_t> expected(static_cast<size_t>(depth_pitch), 0);
    auto* src_bytes = static_cast<uint8_t*>(mapped.pData);
    for (uint32_t y = 0; y < blocks_h; y++) {
      for (uint32_t x = 0; x < row_bytes; x++) {
        const uint8_t v = static_cast<uint8_t>((y + 1u) * 19u + x);
        src_bytes[static_cast<size_t>(y) * row_pitch + x] = v;
        expected[static_cast<size_t>(y) * row_pitch + x] = v;
      }
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

    hr = dev.device_funcs.pfnCopySubresourceRegion(dev.hDevice,
                                                   dst.hResource,
                                                   /*dst_subresource=*/0,
                                                   /*dst_x=*/0,
                                                   /*dst_y=*/0,
                                                   /*dst_z=*/0,
                                                   src.hResource,
                                                   /*src_subresource=*/0,
                                                   /*pSrcBox=*/nullptr);
    if (!Check(hr == S_OK, "CopySubresourceRegion(bc) returns S_OK")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
    hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                dst.hResource,
                                                /*subresource=*/0,
                                                AEROGPU_DDI_MAP_READ,
                                                /*map_flags=*/0,
                                                &readback);
    if (!Check(hr == S_OK, "StagingResourceMap(READ) dst bc tex2d")) {
      return false;
    }
    if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
      return false;
    }
    if (!Check(readback.RowPitch == row_pitch, "dst RowPitch matches src RowPitch")) {
      return false;
    }
    if (!Check(readback.DepthPitch == depth_pitch, "dst DepthPitch matches src DepthPitch")) {
      return false;
    }
    if (!Check(std::memcmp(readback.pData, expected.data(), expected.size()) == 0,
               "CopySubresourceRegion bc tex2d bytes")) {
      return false;
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
      return false;
    }
    CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
    if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
      return false;
    }
    const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
    if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) == 0,
               "COPY_TEXTURE2D must not have WRITEBACK_DST flag")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
    dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestSubmitAllocListTracksBoundShaderResource() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(track SRV alloc)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, /*cpu_access_flags=*/0, &tex),
             "CreateStagingTexture2D")) {
    return false;
  }

  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource(texture)")) {
    return false;
  }

  CmdLoc create_loc =
      FindLastOpcode(dev.harness.last_stream.data(), dev.harness.last_stream.size(), AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd =
      reinterpret_cast<const aerogpu_cmd_create_texture2d*>(dev.harness.last_stream.data() + create_loc.offset);
  const AEROGPU_WDDM_ALLOCATION_HANDLE backing = create_cmd->backing_alloc_id;
  if (!Check(backing != 0, "CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }

  TestShaderResourceView srv{};
  if (!Check(CreateShaderResourceView(&dev, &tex, &srv), "CreateShaderResourceView")) {
    return false;
  }

  D3D10DDI_HSHADERRESOURCEVIEW views[1] = {srv.hView};
  dev.device_funcs.pfnVsSetShaderResources(dev.hDevice, /*start_slot=*/0, /*view_count=*/1, views);

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after VsSetShaderResources")) {
    return false;
  }

  bool found = false;
  for (auto h : dev.harness.last_allocs) {
    if (h == backing) {
      found = true;
      break;
    }
  }
  if (!Check(found, "submit alloc list contains bound shader resource allocation")) {
    return false;
  }

  dev.device_funcs.pfnDestroyShaderResourceView(dev.hDevice, srv.hView);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCopyResourceBufferReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(copy buffer)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src), "CreateStagingBuffer(src)")) {
    return false;
  }
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, AEROGPU_D3D11_CPU_ACCESS_READ, &dst), "CreateStagingBuffer(dst)")) {
    return false;
  }

  const uint8_t expected[16] = {0x5A, 0x4B, 0x3C, 0x2D, 0x1E, 0x0F, 0xAA, 0xBB,
                                0xCC, 0xDD, 0xEE, 0xFF, 0x10, 0x20, 0x30, 0x40};

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnMap(dev.hDevice,
                                       src.hResource,
                                       /*subresource=*/0,
                                       AEROGPU_DDI_MAP_WRITE,
                                       /*map_flags=*/0,
                                       &mapped);
  if (!Check(hr == S_OK, "Map(WRITE) src buffer")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  std::memcpy(mapped.pData, expected, sizeof(expected));
  dev.device_funcs.pfnUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
  hr = dev.device_funcs.pfnMap(dev.hDevice,
                               dst.hResource,
                               /*subresource=*/0,
                               AEROGPU_DDI_MAP_READ,
                               /*map_flags=*/0,
                               &readback);
  if (!Check(hr == S_OK, "Map(READ) dst buffer")) {
    return false;
  }
  if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
    return false;
  }
  if (!Check(std::memcmp(readback.pData, expected, sizeof(expected)) == 0, "CopyResource buffer bytes")) {
    return false;
  }
  dev.device_funcs.pfnUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_BUFFER) == 1, "COPY_BUFFER emitted")) {
    return false;
  }
  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_BUFFER);
  if (!Check(copy_loc.hdr != nullptr, "COPY_BUFFER location")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_buffer*>(stream + copy_loc.offset);
  if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0, "COPY_BUFFER has WRITEBACK_DST flag")) {
    return false;
  }

  std::vector<uint32_t> backing_ids;
  size_t off = sizeof(aerogpu_cmd_stream_header);
  while (off + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(stream + off);
    if (hdr->opcode == AEROGPU_CMD_CREATE_BUFFER) {
      const auto* cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + off);
      backing_ids.push_back(cmd->backing_alloc_id);
    }
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > stream_len - off) {
      break;
    }
    off += hdr->size_bytes;
  }
  if (!Check(backing_ids.size() == 2, "expected exactly 2 CREATE_BUFFER commands")) {
    return false;
  }
  for (uint32_t id : backing_ids) {
    bool found = false;
    for (auto h : dev.harness.last_allocs) {
      if (h == id) {
        found = true;
      }
    }
    if (!Check(found, "submit alloc list contains backing allocation")) {
      return false;
    }
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCopyResourceTextureReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(copy tex2d)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src),
             "CreateStagingTexture2D(src)")) {
    return false;
  }
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_READ, &dst),
             "CreateStagingTexture2D(dst)")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      src.hResource,
                                                      /*subresource=*/0,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src tex2d")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch != 0, "Map returned RowPitch")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  const uint32_t row_pitch = mapped.RowPitch;
  auto* src_bytes = static_cast<uint8_t*>(mapped.pData);
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      src_bytes[static_cast<size_t>(y) * row_pitch + x] = static_cast<uint8_t>((y + 1) * 19u + x);
    }
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              dst.hResource,
                                              /*subresource=*/0,
                                              AEROGPU_DDI_MAP_READ,
                                              /*map_flags=*/0,
                                              &readback);
  if (!Check(hr == S_OK, "StagingResourceMap(READ) dst tex2d")) {
    return false;
  }
  if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
    return false;
  }
  if (!Check(readback.RowPitch == row_pitch, "dst RowPitch matches src RowPitch")) {
    return false;
  }

  const auto* dst_bytes = static_cast<const uint8_t*>(readback.pData);
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      const uint8_t expected = static_cast<uint8_t>((y + 1) * 19u + x);
      if (!Check(dst_bytes[static_cast<size_t>(y) * row_pitch + x] == expected, "CopyResource tex2d pixel bytes")) {
        return false;
      }
    }
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
    return false;
  }
  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
  if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
  if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0, "COPY_TEXTURE2D has WRITEBACK_DST flag")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCopyResourceBcTextureReadback() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(copy bc tex2d guest-backed)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, 8},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    TestResource src{};
    TestResource dst{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &src),
               "CreateStagingTexture2DWithFormat(src bc guest-backed)")) {
      return false;
    }
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &dst),
               "CreateStagingTexture2DWithFormat(dst bc guest-backed)")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        src.hResource,
                                                        /*subresource=*/0,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src bc tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map returned RowPitch")) {
      return false;
    }

    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const uint32_t row_pitch = mapped.RowPitch;
    const uint32_t depth_pitch = mapped.DepthPitch;
    if (!Check(row_pitch >= row_bytes, "Map RowPitch >= tight BC row bytes")) {
      return false;
    }
    if (!Check(depth_pitch == row_pitch * blocks_h, "Map DepthPitch matches BC block rows")) {
      return false;
    }

    std::vector<uint8_t> expected(static_cast<size_t>(depth_pitch), 0);
    auto* src_bytes = static_cast<uint8_t*>(mapped.pData);
    for (uint32_t y = 0; y < blocks_h; y++) {
      for (uint32_t x = 0; x < row_bytes; x++) {
        const uint8_t v = static_cast<uint8_t>((y + 1u) * 19u + x);
        src_bytes[static_cast<size_t>(y) * row_pitch + x] = v;
        expected[static_cast<size_t>(y) * row_pitch + x] = v;
      }
      // Leave padding bytes untouched (they are initially zero); expected remains zero.
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

    dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

    AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
    hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                dst.hResource,
                                                /*subresource=*/0,
                                                AEROGPU_DDI_MAP_READ,
                                                /*map_flags=*/0,
                                                &readback);
    if (!Check(hr == S_OK, "StagingResourceMap(READ) dst bc tex2d")) {
      return false;
    }
    if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
      return false;
    }
    if (!Check(readback.RowPitch == row_pitch, "dst RowPitch matches src RowPitch")) {
      return false;
    }
    if (!Check(readback.DepthPitch == depth_pitch, "dst DepthPitch matches src DepthPitch")) {
      return false;
    }
    if (!Check(std::memcmp(readback.pData, expected.data(), expected.size()) == 0, "CopyResource bc tex2d bytes")) {
      return false;
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
      return false;
    }
    CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
    if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
      return false;
    }
    const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
    if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0, "COPY_TEXTURE2D has WRITEBACK_DST flag")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
    dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestGuestBackedCopySubresourceRegionBcTextureReadback() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(copy subresource bc tex2d guest-backed)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, 8},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    TestResource src{};
    TestResource dst{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &src),
               "CreateStagingTexture2DWithFormat(src bc guest-backed)")) {
      return false;
    }
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &dst),
               "CreateStagingTexture2DWithFormat(dst bc guest-backed)")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        src.hResource,
                                                        /*subresource=*/0,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src bc tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map returned RowPitch")) {
      return false;
    }

    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const uint32_t row_pitch = mapped.RowPitch;
    const uint32_t depth_pitch = mapped.DepthPitch;
    if (!Check(row_pitch >= row_bytes, "Map RowPitch >= tight BC row bytes")) {
      return false;
    }
    if (!Check(depth_pitch == row_pitch * blocks_h, "Map DepthPitch matches BC block rows")) {
      return false;
    }

    std::vector<uint8_t> expected(static_cast<size_t>(depth_pitch), 0);
    auto* src_bytes = static_cast<uint8_t*>(mapped.pData);
    for (uint32_t y = 0; y < blocks_h; y++) {
      for (uint32_t x = 0; x < row_bytes; x++) {
        const uint8_t v = static_cast<uint8_t>((y + 1u) * 19u + x);
        src_bytes[static_cast<size_t>(y) * row_pitch + x] = v;
        expected[static_cast<size_t>(y) * row_pitch + x] = v;
      }
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

    hr = dev.device_funcs.pfnCopySubresourceRegion(dev.hDevice,
                                                   dst.hResource,
                                                   /*dst_subresource=*/0,
                                                   /*dst_x=*/0,
                                                   /*dst_y=*/0,
                                                   /*dst_z=*/0,
                                                   src.hResource,
                                                   /*src_subresource=*/0,
                                                   /*pSrcBox=*/nullptr);
    if (!Check(hr == S_OK, "CopySubresourceRegion(bc guest-backed) returns S_OK")) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE readback = {};
    hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                dst.hResource,
                                                /*subresource=*/0,
                                                AEROGPU_DDI_MAP_READ,
                                                /*map_flags=*/0,
                                                &readback);
    if (!Check(hr == S_OK, "StagingResourceMap(READ) dst bc tex2d")) {
      return false;
    }
    if (!Check(readback.pData != nullptr, "Map(READ) returned non-null pData")) {
      return false;
    }
    if (!Check(readback.RowPitch == row_pitch, "dst RowPitch matches src RowPitch")) {
      return false;
    }
    if (!Check(readback.DepthPitch == depth_pitch, "dst DepthPitch matches src DepthPitch")) {
      return false;
    }
    if (!Check(std::memcmp(readback.pData, expected.data(), expected.size()) == 0,
               "CopySubresourceRegion bc tex2d bytes")) {
      return false;
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
      return false;
    }
    CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
    if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
      return false;
    }
    const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
    if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0, "COPY_TEXTURE2D has WRITEBACK_DST flag")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
    dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestHostOwnedUpdateSubresourceUPBufferUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP buffer host-owned)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, /*cpu_access_flags=*/0, &buf), "CreateStagingBuffer")) {
    return false;
  }

  const uint8_t expected[16] = {0x00, 0x02, 0x04, 0x06, 0x10, 0x20, 0x30, 0x40,
                                0x55, 0x66, 0x77, 0x88, 0x99, 0xAB, 0xBC, 0xCD};
  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          buf.hResource,
                                          /*dst_subresource=*/0,
                                          /*pDstBox=*/nullptr,
                                          expected,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned UpdateSubresourceUP should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned UpdateSubresourceUP should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_BUFFER backing_alloc_id == 0")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(expected), "UPLOAD_RESOURCE size_bytes matches")) {
    return false;
  }
  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + sizeof(expected) <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, expected, sizeof(expected)) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned UpdateSubresourceUP submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedUpdateSubresourceUPBufferDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP buffer guest-backed)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, /*cpu_access_flags=*/0, &buf), "CreateStagingBuffer")) {
    return false;
  }

  const uint8_t expected[16] = {0xF0, 0xE1, 0xD2, 0xC3, 0xB4, 0xA5, 0x96, 0x87,
                                0x78, 0x69, 0x5A, 0x4B, 0x3C, 0x2D, 0x1E, 0x0F};
  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          buf.hResource,
                                          /*dst_subresource=*/0,
                                          /*pDstBox=*/nullptr,
                                          expected,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed UpdateSubresourceUP should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed UpdateSubresourceUP should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sizeof(expected), "RESOURCE_DIRTY_RANGE size_bytes matches")) {
    return false;
  }

  bool found_alloc = false;
  for (auto h : dev.harness.last_allocs) {
    if (h == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed submit alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= sizeof(expected), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected, sizeof(expected)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedUpdateSubresourceUPTextureUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP tex2d host-owned)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, /*cpu_access_flags=*/0, &tex), "CreateStagingTexture2D")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  std::vector<uint8_t> sysmem(static_cast<size_t>(bytes_per_row) * height);
  for (uint32_t i = 0; i < sysmem.size(); i++) {
    sysmem[i] = static_cast<uint8_t>(0x40u + i);
  }

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          /*pDstBox=*/nullptr,
                                          sysmem.data(),
                                          /*SysMemPitch=*/bytes_per_row,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned tex2d UpdateSubresourceUP should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned tex2d UpdateSubresourceUP should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_TEXTURE2D backing_alloc_id == 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes == bytes_per_row, "CREATE_TEXTURE2D row_pitch_bytes tight")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sysmem.size(), "UPLOAD_RESOURCE size_bytes matches")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + sysmem.size() <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, sysmem.data(), sysmem.size()) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned tex2d submit alloc list should be empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedUpdateSubresourceUPTextureDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP tex2d guest-backed)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, /*cpu_access_flags=*/0, &tex), "CreateStagingTexture2D")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  std::vector<uint8_t> sysmem(static_cast<size_t>(bytes_per_row) * height);
  for (uint32_t i = 0; i < sysmem.size(); i++) {
    sysmem[i] = static_cast<uint8_t>(0x90u + i);
  }

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          /*pDstBox=*/nullptr,
                                          sysmem.data(),
                                          /*SysMemPitch=*/bytes_per_row,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed tex2d UpdateSubresourceUP should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed tex2d UpdateSubresourceUP should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes != 0, "CREATE_TEXTURE2D row_pitch_bytes non-zero")) {
    return false;
  }

  const uint32_t row_pitch = create_cmd->row_pitch_bytes;
  const size_t total_bytes = static_cast<size_t>(row_pitch) * height;

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == total_bytes, "RESOURCE_DIRTY_RANGE size_bytes includes padding")) {
    return false;
  }

  bool found_alloc = false;
  for (auto h : dev.harness.last_allocs) {
    if (h == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed tex2d submit alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= total_bytes, "backing allocation large enough")) {
    return false;
  }

  std::vector<uint8_t> expected(total_bytes, 0);
  for (uint32_t y = 0; y < height; y++) {
    std::memcpy(expected.data() + static_cast<size_t>(y) * row_pitch,
                sysmem.data() + static_cast<size_t>(y) * bytes_per_row,
                bytes_per_row);
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedUpdateSubresourceUPBcTextureUploads() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/false,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP bc tex2d host-owned)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC1_UNORM_SRGB", kDxgiFormatBc1UnormSrgb, AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, 8},
      {"DXGI_FORMAT_BC2_UNORM", kDxgiFormatBc2Unorm, AEROGPU_FORMAT_BC2_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC2_UNORM_SRGB", kDxgiFormatBc2UnormSrgb, AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC3_UNORM", kDxgiFormatBc3Unorm, AEROGPU_FORMAT_BC3_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC3_UNORM_SRGB", kDxgiFormatBc3UnormSrgb, AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC7_UNORM_SRGB", kDxgiFormatBc7UnormSrgb, AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/0,
                                                &tex),
               "CreateStagingTexture2DWithFormat(bc)")) {
      return false;
    }

    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const size_t total_bytes = static_cast<size_t>(row_bytes) * blocks_h;
    std::vector<uint8_t> sysmem(total_bytes);
    for (size_t i = 0; i < sysmem.size(); i++) {
      sysmem[i] = static_cast<uint8_t>(0x40u + (i & 0x3Fu));
    }

    dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                            tex.hResource,
                                            /*dst_subresource=*/0,
                                            /*pDstBox=*/nullptr,
                                            sysmem.data(),
                                            /*SysMemPitch=*/row_bytes,
                                            /*SysMemSlicePitch=*/0);
    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(bc)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
               "host-owned bc tex2d UpdateSubresourceUP should not emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
               "host-owned bc tex2d UpdateSubresourceUP should emit UPLOAD_RESOURCE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
    if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_TEXTURE2D backing_alloc_id == 0")) {
      return false;
    }

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D row_pitch_bytes matches expected for %s", c.name);
    if (!Check(create_cmd->row_pitch_bytes == row_bytes, msg)) {
      return false;
    }

    CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
    if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
      return false;
    }
    const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
    if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
      return false;
    }
    if (!Check(upload_cmd->size_bytes == sysmem.size(), "UPLOAD_RESOURCE size_bytes matches")) {
      return false;
    }

    const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
    if (!Check(payload_offset + sysmem.size() <= stream_len, "UPLOAD_RESOURCE payload fits")) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "UPLOAD_RESOURCE payload bytes match for %s", c.name);
    if (!Check(std::memcmp(stream + payload_offset, sysmem.data(), sysmem.size()) == 0, msg)) {
      return false;
    }

    if (!Check(dev.harness.last_allocs.empty(), "host-owned UpdateSubresourceUP(bc) alloc list empty")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestGuestBackedUpdateSubresourceUPBcTextureDirtyRange() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/true,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP bc tex2d guest-backed)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC1_UNORM_SRGB", kDxgiFormatBc1UnormSrgb, AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, 8},
      {"DXGI_FORMAT_BC2_UNORM", kDxgiFormatBc2Unorm, AEROGPU_FORMAT_BC2_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC2_UNORM_SRGB", kDxgiFormatBc2UnormSrgb, AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC3_UNORM", kDxgiFormatBc3Unorm, AEROGPU_FORMAT_BC3_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC3_UNORM_SRGB", kDxgiFormatBc3UnormSrgb, AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC7_UNORM_SRGB", kDxgiFormatBc7UnormSrgb, AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/0,
                                                &tex),
               "CreateStagingTexture2DWithFormat(bc guest-backed)")) {
      return false;
    }

    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const size_t sysmem_size = static_cast<size_t>(row_bytes) * blocks_h;
    std::vector<uint8_t> sysmem(sysmem_size);
    for (size_t i = 0; i < sysmem.size(); i++) {
      sysmem[i] = static_cast<uint8_t>(0x90u + (i & 0x3Fu));
    }

    dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                            tex.hResource,
                                            /*dst_subresource=*/0,
                                            /*pDstBox=*/nullptr,
                                            sysmem.data(),
                                            /*SysMemPitch=*/row_bytes,
                                            /*SysMemSlicePitch=*/0);
    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(bc guest-backed)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
               "guest-backed bc tex2d UpdateSubresourceUP should not emit UPLOAD_RESOURCE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
               "guest-backed bc tex2d UpdateSubresourceUP should emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }
    if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
      return false;
    }
    if (!Check(create_cmd->row_pitch_bytes >= row_bytes, "CREATE_TEXTURE2D row_pitch_bytes >= row_bytes")) {
      return false;
    }

    const uint32_t row_pitch = create_cmd->row_pitch_bytes;
    const size_t total_bytes = static_cast<size_t>(row_pitch) * blocks_h;

    CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
      return false;
    }
    const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
    if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
      return false;
    }
    if (!Check(dirty_cmd->size_bytes == total_bytes, "RESOURCE_DIRTY_RANGE size_bytes matches BC bytes")) {
      return false;
    }

    bool found_alloc = false;
    for (auto h : dev.harness.last_allocs) {
      if (h == create_cmd->backing_alloc_id) {
        found_alloc = true;
      }
    }
    if (!Check(found_alloc, "guest-backed bc tex2d submit alloc list contains backing alloc")) {
      return false;
    }

    Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
    if (!Check(alloc != nullptr, "backing allocation exists")) {
      return false;
    }
    if (!Check(alloc->bytes.size() >= total_bytes, "backing allocation large enough")) {
      return false;
    }

    std::vector<uint8_t> expected(total_bytes, 0);
    for (uint32_t y = 0; y < blocks_h; y++) {
      std::memcpy(expected.data() + static_cast<size_t>(y) * row_pitch,
                  sysmem.data() + static_cast<size_t>(y) * row_bytes,
                  row_bytes);
    }
    std::snprintf(msg, sizeof(msg), "backing allocation bytes match expected for %s", c.name);
    if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0, msg)) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestHostOwnedUpdateSubresourceUPBufferBoxUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP box buffer host-owned)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, /*cpu_access_flags=*/0, &buf), "CreateStagingBuffer")) {
    return false;
  }

  const uint8_t patch[8] = {0xDE, 0xC0, 0xAD, 0xDE, 0xBE, 0xEF, 0xCA, 0xFE};
  AEROGPU_DDI_BOX box{};
  box.left = 4;
  box.right = 12;
  box.top = 0;
  box.bottom = 1;
  box.front = 0;
  box.back = 1;

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          buf.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          patch,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned UpdateSubresourceUP(box) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned UpdateSubresourceUP(box) should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 4, "UPLOAD_RESOURCE offset_bytes matches box.left")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(patch), "UPLOAD_RESOURCE size_bytes matches box span")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + sizeof(patch) <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, patch, sizeof(patch)) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned UpdateSubresourceUP(box) alloc list empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedUpdateSubresourceUPTextureBoxUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP box tex2d host-owned)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, /*cpu_access_flags=*/0, &tex), "CreateStagingTexture2D")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;

  // Update only the second row.
  uint8_t row[12] = {};
  for (uint32_t i = 0; i < sizeof(row); i++) {
    row[i] = static_cast<uint8_t>(0xA0u + i);
  }

  AEROGPU_DDI_BOX box{};
  box.left = 0;
  box.right = width;
  box.top = 1;
  box.bottom = 2;
  box.front = 0;
  box.back = 1;

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          row,
                                          /*SysMemPitch=*/bytes_per_row,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned tex2d UpdateSubresourceUP(box) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned tex2d UpdateSubresourceUP(box) should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == bytes_per_row, "UPLOAD_RESOURCE offset_bytes == RowPitch*top")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(row), "UPLOAD_RESOURCE size_bytes matches one row")) {
    return false;
  }

  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + sizeof(row) <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, row, sizeof(row)) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned tex2d UpdateSubresourceUP(box) alloc list empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedUpdateSubresourceUPBufferBoxDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP box buffer guest-backed)")) {
    return false;
  }

  TestResource buf{};
  if (!Check(CreateStagingBuffer(&dev, /*byte_width=*/16, /*cpu_access_flags=*/0, &buf), "CreateStagingBuffer")) {
    return false;
  }

  const uint8_t patch[8] = {0x11, 0x33, 0x55, 0x77, 0x99, 0xBB, 0xDD, 0xFF};
  AEROGPU_DDI_BOX box{};
  box.left = 4;
  box.right = 12;
  box.top = 0;
  box.bottom = 1;
  box.front = 0;
  box.back = 1;

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          buf.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          patch,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed UpdateSubresourceUP(box) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed UpdateSubresourceUP(box) should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == 16, "RESOURCE_DIRTY_RANGE size_bytes == full buffer")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= 16, "backing allocation large enough")) {
    return false;
  }

  uint8_t expected[16] = {};
  std::memcpy(expected + 4, patch, sizeof(patch));
  if (!Check(std::memcmp(alloc->bytes.data(), expected, sizeof(expected)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedUpdateSubresourceUPTextureBoxDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(UpdateSubresourceUP box tex2d guest-backed)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, /*cpu_access_flags=*/0, &tex), "CreateStagingTexture2D")) {
    return false;
  }

  const uint8_t pixel[4] = {0x10, 0x20, 0x30, 0x40};
  AEROGPU_DDI_BOX box{};
  box.left = 1;
  box.right = 2;
  box.top = 0;
  box.bottom = 1;
  box.front = 0;
  box.back = 1;

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          pixel,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);
  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed tex2d UpdateSubresourceUP(box) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed tex2d UpdateSubresourceUP(box) should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }
  const uint32_t row_pitch = create_cmd->row_pitch_bytes;
  if (!Check(row_pitch != 0, "CREATE_TEXTURE2D row_pitch_bytes non-zero")) {
    return false;
  }

  const size_t total_bytes = static_cast<size_t>(row_pitch) * 2u;

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == total_bytes, "RESOURCE_DIRTY_RANGE size_bytes == full texture bytes")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= total_bytes, "backing allocation large enough")) {
    return false;
  }

  std::vector<uint8_t> expected(total_bytes, 0);
  const size_t dst_offset = 0u * row_pitch + 1u * 4u;
  std::memcpy(expected.data() + dst_offset, pixel, sizeof(pixel));
  if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedUpdateSubresourceUPBcTextureBoxUploads() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/false,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP box bc tex2d host-owned)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
  };

  // Upload the bottom-right 4x4 block (aligned left/top, edge-aligned right/bottom).
  AEROGPU_DDI_BOX box{};
  box.left = 4;
  box.right = kWidth;
  box.top = 4;
  box.bottom = kHeight;
  box.front = 0;
  box.back = 1;

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/0,
                                                &tex),
               "CreateStagingTexture2DWithFormat(bc box)")) {
      return false;
    }

    std::vector<uint8_t> sysmem(c.block_bytes);
    for (size_t i = 0; i < sysmem.size(); i++) {
      sysmem[i] = static_cast<uint8_t>(0x55u + (i & 0x3Fu));
    }

    dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                            tex.hResource,
                                            /*dst_subresource=*/0,
                                            &box,
                                            sysmem.data(),
                                            /*SysMemPitch=*/0,
                                            /*SysMemSlicePitch=*/0);
    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(box bc)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
               "host-owned bc tex2d UpdateSubresourceUP(box) should not emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
               "host-owned bc tex2d UpdateSubresourceUP(box) should emit UPLOAD_RESOURCE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_TEXTURE2D backing_alloc_id == 0")) {
      return false;
    }

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }

    const uint32_t row_pitch = create_cmd->row_pitch_bytes;
    const uint32_t expected_row_pitch = 2u * c.block_bytes;
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D row_pitch_bytes matches expected for %s", c.name);
    if (!Check(row_pitch == expected_row_pitch, msg)) {
      return false;
    }

    CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
    if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
      return false;
    }
    const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
    if (!Check(upload_cmd->offset_bytes == row_pitch, "UPLOAD_RESOURCE offset_bytes == row_pitch (second block row)")) {
      return false;
    }
    if (!Check(upload_cmd->size_bytes == row_pitch, "UPLOAD_RESOURCE size_bytes == row_pitch (one block row)")) {
      return false;
    }

    const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
    if (!Check(payload_offset + static_cast<size_t>(row_pitch) <= stream_len, "UPLOAD_RESOURCE payload fits")) {
      return false;
    }

    std::vector<uint8_t> expected(static_cast<size_t>(row_pitch), 0);
    // block_left=1 => offset = block_bytes
    std::memcpy(expected.data() + c.block_bytes, sysmem.data(), sysmem.size());
    std::snprintf(msg, sizeof(msg), "UPLOAD_RESOURCE payload bytes match expected for %s", c.name);
    if (!Check(std::memcmp(stream + payload_offset, expected.data(), expected.size()) == 0, msg)) {
      return false;
    }

    if (!Check(dev.harness.last_allocs.empty(), "host-owned UpdateSubresourceUP(box bc) alloc list empty")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestGuestBackedUpdateSubresourceUPBcTextureBoxDirtyRange() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/true,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP box bc tex2d guest-backed)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
  };

  // Upload the bottom-right 4x4 block (aligned left/top, edge-aligned right/bottom).
  AEROGPU_DDI_BOX box{};
  box.left = 4;
  box.right = kWidth;
  box.top = 4;
  box.bottom = kHeight;
  box.front = 0;
  box.back = 1;

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/0,
                                                &tex),
               "CreateStagingTexture2DWithFormat(bc guest-backed box)")) {
      return false;
    }

    std::vector<uint8_t> sysmem(c.block_bytes);
    for (size_t i = 0; i < sysmem.size(); i++) {
      sysmem[i] = static_cast<uint8_t>(0x99u + (i & 0x3Fu));
    }

    dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                            tex.hResource,
                                            /*dst_subresource=*/0,
                                            &box,
                                            sysmem.data(),
                                            /*SysMemPitch=*/0,
                                            /*SysMemSlicePitch=*/0);
    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(box bc guest-backed)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
               "guest-backed bc tex2d UpdateSubresourceUP(box) should not emit UPLOAD_RESOURCE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
               "guest-backed bc tex2d UpdateSubresourceUP(box) should emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }
    if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
      return false;
    }
    if (!Check(create_cmd->row_pitch_bytes != 0, "CREATE_TEXTURE2D row_pitch_bytes non-zero")) {
      return false;
    }

    const uint32_t row_pitch = create_cmd->row_pitch_bytes;
    const uint32_t blocks_h = 2;
    const size_t total_bytes = static_cast<size_t>(row_pitch) * blocks_h;

    CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
      return false;
    }
    const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
    if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
      return false;
    }
    if (!Check(dirty_cmd->size_bytes == total_bytes, "RESOURCE_DIRTY_RANGE size_bytes == full texture bytes")) {
      return false;
    }

    Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
    if (!Check(alloc != nullptr, "backing allocation exists")) {
      return false;
    }
    if (!Check(alloc->bytes.size() >= total_bytes, "backing allocation large enough")) {
      return false;
    }

    std::vector<uint8_t> expected(total_bytes, 0);
    const size_t dst_offset = 1u * static_cast<size_t>(row_pitch) + c.block_bytes;
    std::memcpy(expected.data() + dst_offset, sysmem.data(), sysmem.size());
    std::snprintf(msg, sizeof(msg), "backing allocation bytes match expected for %s", c.name);
    if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0, msg)) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestHostOwnedUpdateSubresourceUPBcTextureBoxRejectsMisaligned() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/false,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP invalid box bc tex2d host-owned)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                              /*width=*/5,
                                              /*height=*/5,
                                              kDxgiFormatBc7Unorm,
                                              /*cpu_access_flags=*/0,
                                              &tex),
             "CreateStagingTexture2DWithFormat(BC7)")) {
    return false;
  }

  // Misaligned left (must be multiple of 4 for BC formats).
  AEROGPU_DDI_BOX box{};
  box.left = 1;
  box.right = 5;
  box.top = 0;
  box.bottom = 4;
  box.front = 0;
  box.back = 1;

  const uint8_t junk[16] = {};
  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          junk,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(invalid bc box)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D) == 1, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "invalid BC UpdateSubresourceUP(box) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "invalid BC UpdateSubresourceUP(box) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestGuestBackedUpdateSubresourceUPBcTextureBoxRejectsMisaligned() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/true,
                            /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP invalid box bc tex2d guest-backed)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                              /*width=*/5,
                                              /*height=*/5,
                                              kDxgiFormatBc7Unorm,
                                              /*cpu_access_flags=*/0,
                                              &tex),
             "CreateStagingTexture2DWithFormat(BC7 guest-backed)")) {
    return false;
  }

  // Misaligned left (must be multiple of 4 for BC formats).
  AEROGPU_DDI_BOX box{};
  box.left = 1;
  box.right = 5;
  box.top = 0;
  box.bottom = 4;
  box.front = 0;
  box.back = 1;

  const uint8_t junk[16] = {};
  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          /*dst_subresource=*/0,
                                          &box,
                                          junk,
                                          /*SysMemPitch=*/0,
                                          /*SysMemSlicePitch=*/0);

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(invalid bc box guest-backed)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D) == 1, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "invalid BC UpdateSubresourceUP(box) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "invalid BC UpdateSubresourceUP(box) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestHostOwnedCreateBufferInitialDataUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(CreateResource initial buffer host-owned)")) {
    return false;
  }

  const uint8_t initial[16] = {0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF,
                               0x10, 0x32, 0x54, 0x76, 0x98, 0xBA, 0xDC, 0xFE};

  TestResource buf{};
  if (!Check(CreateBufferWithInitialData(&dev,
                                         /*byte_width=*/sizeof(initial),
                                         AEROGPU_D3D11_USAGE_DEFAULT,
                                         /*bind_flags=*/0,
                                         /*cpu_access_flags=*/0,
                                         initial,
                                         &buf),
             "CreateBufferWithInitialData")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned CreateResource(initial) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned CreateResource(initial) should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_BUFFER backing_alloc_id == 0")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(initial), "UPLOAD_RESOURCE size_bytes matches initial buffer")) {
    return false;
  }
  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + sizeof(initial) <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, initial, sizeof(initial)) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned CreateResource(initial) alloc list empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCreateBufferInitialDataDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(CreateResource initial buffer guest-backed)")) {
    return false;
  }

  const uint8_t initial[16] = {0xFE, 0xDC, 0xBA, 0x98, 0x76, 0x54, 0x32, 0x10,
                               0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23, 0x01};

  TestResource buf{};
  if (!Check(CreateBufferWithInitialData(&dev,
                                         /*byte_width=*/sizeof(initial),
                                         AEROGPU_D3D11_USAGE_DEFAULT,
                                         /*bind_flags=*/0,
                                         /*cpu_access_flags=*/0,
                                         initial,
                                         &buf),
             "CreateBufferWithInitialData")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed CreateResource(initial) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed CreateResource(initial) should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_loc.hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_buffer*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_BUFFER backing_alloc_id != 0")) {
    return false;
  }

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sizeof(initial), "RESOURCE_DIRTY_RANGE size_bytes matches initial buffer")) {
    return false;
  }

  bool found_alloc = false;
  for (auto h : dev.harness.last_allocs) {
    if (h == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed CreateResource(initial) alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= sizeof(initial), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data(), initial, sizeof(initial)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, buf.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedCreateTextureInitialDataUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false), "InitTestDevice(CreateResource initial tex2d host-owned)")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  std::vector<uint8_t> initial(static_cast<size_t>(bytes_per_row) * height);
  for (size_t i = 0; i < initial.size(); i++) {
    initial[i] = static_cast<uint8_t>(0x11u + i);
  }

  TestResource tex{};
  if (!Check(CreateTexture2DWithInitialData(&dev,
                                            width,
                                            height,
                                            AEROGPU_D3D11_USAGE_DEFAULT,
                                            /*bind_flags=*/0,
                                            /*cpu_access_flags=*/0,
                                            initial.data(),
                                            bytes_per_row,
                                            &tex),
             "CreateTexture2DWithInitialData")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
             "host-owned CreateResource(initial tex2d) should not emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
             "host-owned CreateResource(initial tex2d) should emit UPLOAD_RESOURCE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_TEXTURE2D backing_alloc_id == 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes == bytes_per_row, "CREATE_TEXTURE2D row_pitch_bytes tight")) {
    return false;
  }

  CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
  if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == initial.size(), "UPLOAD_RESOURCE size_bytes matches initial tex2d")) {
    return false;
  }
  const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
  if (!Check(payload_offset + initial.size() <= stream_len, "UPLOAD_RESOURCE payload fits")) {
    return false;
  }
  if (!Check(std::memcmp(stream + payload_offset, initial.data(), initial.size()) == 0, "UPLOAD_RESOURCE payload bytes")) {
    return false;
  }

  if (!Check(dev.harness.last_allocs.empty(), "host-owned CreateResource(initial tex2d) alloc list empty")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCreateTextureInitialDataDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false), "InitTestDevice(CreateResource initial tex2d guest-backed)")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  std::vector<uint8_t> initial(static_cast<size_t>(bytes_per_row) * height);
  for (size_t i = 0; i < initial.size(); i++) {
    initial[i] = static_cast<uint8_t>(0x80u + i);
  }

  TestResource tex{};
  if (!Check(CreateTexture2DWithInitialData(&dev,
                                            width,
                                            height,
                                            AEROGPU_D3D11_USAGE_DEFAULT,
                                            /*bind_flags=*/0,
                                            /*cpu_access_flags=*/0,
                                            initial.data(),
                                            bytes_per_row,
                                            &tex),
             "CreateTexture2DWithInitialData")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }

  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed CreateResource(initial tex2d) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed CreateResource(initial tex2d) should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes != 0, "CREATE_TEXTURE2D row_pitch_bytes non-zero")) {
    return false;
  }

  const uint32_t row_pitch = create_cmd->row_pitch_bytes;
  const size_t dirty_bytes = static_cast<size_t>(row_pitch) * height;

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == dirty_bytes, "RESOURCE_DIRTY_RANGE size_bytes matches initial tex2d bytes")) {
    return false;
  }

  bool found_alloc = false;
  for (auto h : dev.harness.last_allocs) {
    if (h == create_cmd->backing_alloc_id) {
      found_alloc = true;
    }
  }
  if (!Check(found_alloc, "guest-backed CreateResource(initial tex2d) alloc list contains backing alloc")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= dirty_bytes, "backing allocation large enough")) {
    return false;
  }

  std::vector<uint8_t> expected(dirty_bytes, 0);
  for (uint32_t y = 0; y < height; y++) {
    std::memcpy(expected.data() + static_cast<size_t>(y) * row_pitch,
                initial.data() + static_cast<size_t>(y) * bytes_per_row,
                bytes_per_row);
  }
  if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestHostOwnedCreateBcTextureInitialDataUploads() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/false,
                            /*async_fences=*/false),
             "InitTestDevice(CreateResource initial BC tex2d host-owned)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC1_UNORM_SRGB", kDxgiFormatBc1UnormSrgb, AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, 8},
      {"DXGI_FORMAT_BC2_UNORM", kDxgiFormatBc2Unorm, AEROGPU_FORMAT_BC2_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC2_UNORM_SRGB", kDxgiFormatBc2UnormSrgb, AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC3_UNORM", kDxgiFormatBc3Unorm, AEROGPU_FORMAT_BC3_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC3_UNORM_SRGB", kDxgiFormatBc3UnormSrgb, AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC7_UNORM_SRGB", kDxgiFormatBc7UnormSrgb, AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const size_t total_bytes = static_cast<size_t>(row_bytes) * blocks_h;
    std::vector<uint8_t> initial(total_bytes);
    for (size_t i = 0; i < initial.size(); i++) {
      initial[i] = static_cast<uint8_t>(0x11u + (i & 0x3Fu));
    }

    TestResource tex{};
    if (!Check(CreateTexture2DWithInitialData(&dev,
                                              kWidth,
                                              kHeight,
                                              AEROGPU_D3D11_USAGE_DEFAULT,
                                              /*bind_flags=*/0,
                                              /*cpu_access_flags=*/0,
                                              initial.data(),
                                              row_bytes,
                                              &tex,
                                              c.dxgi_format),
               "CreateTexture2DWithInitialData(BC)")) {
      return false;
    }

    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after CreateResource(initial BC tex2d)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 0,
               "host-owned CreateResource(initial BC tex2d) should not emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 1,
               "host-owned CreateResource(initial BC tex2d) should emit UPLOAD_RESOURCE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
    if (!Check(create_cmd->backing_alloc_id == 0, "host-owned CREATE_TEXTURE2D backing_alloc_id == 0")) {
      return false;
    }

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D row_pitch_bytes matches expected for %s", c.name);
    if (!Check(create_cmd->row_pitch_bytes == row_bytes, msg)) {
      return false;
    }

    CmdLoc upload_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE);
    if (!Check(upload_loc.hdr != nullptr, "UPLOAD_RESOURCE emitted")) {
      return false;
    }
    const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(stream + upload_loc.offset);
    if (!Check(upload_cmd->offset_bytes == 0, "UPLOAD_RESOURCE offset_bytes == 0")) {
      return false;
    }
    if (!Check(upload_cmd->size_bytes == initial.size(), "UPLOAD_RESOURCE size_bytes matches initial BC tex2d")) {
      return false;
    }

    const size_t payload_offset = upload_loc.offset + sizeof(*upload_cmd);
    if (!Check(payload_offset + initial.size() <= stream_len, "UPLOAD_RESOURCE payload fits")) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "UPLOAD_RESOURCE payload bytes match for %s", c.name);
    if (!Check(std::memcmp(stream + payload_offset, initial.data(), initial.size()) == 0, msg)) {
      return false;
    }

    if (!Check(dev.harness.last_allocs.empty(), "host-owned CreateResource(initial BC) alloc list empty")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestGuestBackedCreateBcTextureInitialDataDirtyRange() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev,
                            /*want_backing_allocations=*/true,
                            /*async_fences=*/false),
             "InitTestDevice(CreateResource initial BC tex2d guest-backed)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC1_UNORM_SRGB", kDxgiFormatBc1UnormSrgb, AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, 8},
      {"DXGI_FORMAT_BC2_UNORM", kDxgiFormatBc2Unorm, AEROGPU_FORMAT_BC2_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC2_UNORM_SRGB", kDxgiFormatBc2UnormSrgb, AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC3_UNORM", kDxgiFormatBc3Unorm, AEROGPU_FORMAT_BC3_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC3_UNORM_SRGB", kDxgiFormatBc3UnormSrgb, AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC7_UNORM_SRGB", kDxgiFormatBc7UnormSrgb, AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };
  const uint32_t blocks_w = div_round_up(kWidth, 4);
  const uint32_t blocks_h = div_round_up(kHeight, 4);

  for (const auto& c : kCases) {
    const uint32_t row_bytes = blocks_w * c.block_bytes;
    const size_t initial_size = static_cast<size_t>(row_bytes) * blocks_h;
    std::vector<uint8_t> initial(initial_size);
    for (size_t i = 0; i < initial.size(); i++) {
      initial[i] = static_cast<uint8_t>(0x80u + (i & 0x3Fu));
    }

    TestResource tex{};
    if (!Check(CreateTexture2DWithInitialData(&dev,
                                              kWidth,
                                              kHeight,
                                              AEROGPU_D3D11_USAGE_DEFAULT,
                                              /*bind_flags=*/0,
                                              /*cpu_access_flags=*/0,
                                              initial.data(),
                                              row_bytes,
                                              &tex,
                                              c.dxgi_format),
               "CreateTexture2DWithInitialData(BC guest-backed)")) {
      return false;
    }

    const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after CreateResource(initial BC tex2d)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }

    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
               "guest-backed CreateResource(initial BC tex2d) should not emit UPLOAD_RESOURCE")) {
      return false;
    }
    if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
               "guest-backed CreateResource(initial BC tex2d) should emit RESOURCE_DIRTY_RANGE")) {
      return false;
    }

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
    if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
      return false;
    }
    if (!Check(create_cmd->row_pitch_bytes != 0, "CREATE_TEXTURE2D row_pitch_bytes non-zero")) {
      return false;
    }

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }

    const uint32_t row_pitch = create_cmd->row_pitch_bytes;
    const size_t dirty_bytes = static_cast<size_t>(row_pitch) * blocks_h;

    CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
    if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
      return false;
    }
    const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
    if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
      return false;
    }
    if (!Check(dirty_cmd->size_bytes == dirty_bytes, "RESOURCE_DIRTY_RANGE size_bytes matches BC tex2d bytes")) {
      return false;
    }

    bool found_alloc = false;
    for (auto h : dev.harness.last_allocs) {
      if (h == create_cmd->backing_alloc_id) {
        found_alloc = true;
      }
    }
    if (!Check(found_alloc, "guest-backed CreateResource(initial BC) alloc list contains backing alloc")) {
      return false;
    }

    Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
    if (!Check(alloc != nullptr, "backing allocation exists")) {
      return false;
    }
    if (!Check(alloc->bytes.size() >= dirty_bytes, "backing allocation large enough")) {
      return false;
    }

    std::vector<uint8_t> expected(dirty_bytes, 0);
    for (uint32_t y = 0; y < blocks_h; y++) {
      std::memcpy(expected.data() + static_cast<size_t>(y) * row_pitch,
                  initial.data() + static_cast<size_t>(y) * row_bytes,
                  row_bytes);
    }
    std::snprintf(msg, sizeof(msg), "backing allocation bytes match expected for %s", c.name);
    if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0, msg)) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestSrgbTexture2DFormatPropagation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(srgb format propagation)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
  };

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_B8G8R8A8_UNORM_SRGB", kDxgiFormatB8G8R8A8UnormSrgb,
#if AEROGPU_ABI_MINOR >= 2
       AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB
#else
       AEROGPU_FORMAT_B8G8R8A8_UNORM
#endif
      },
      {"DXGI_FORMAT_B8G8R8X8_UNORM_SRGB", kDxgiFormatB8G8R8X8UnormSrgb,
#if AEROGPU_ABI_MINOR >= 2
       AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB
#else
       AEROGPU_FORMAT_B8G8R8X8_UNORM
#endif
      },
      {"DXGI_FORMAT_R8G8B8A8_UNORM_SRGB", kDxgiFormatR8G8B8A8UnormSrgb,
#if AEROGPU_ABI_MINOR >= 2
       AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB
#else
       AEROGPU_FORMAT_R8G8B8A8_UNORM
#endif
      },
  };

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/4,
                                                /*height=*/4,
                                                c.dxgi_format,
                                                // Staging textures require CPU access flags in real D3D11; keep the
                                                // descriptor valid so this test doesn't start failing if stricter
                                                // CreateResource validation is added later.
                                                /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &tex),
               "CreateStagingTexture2DWithFormat(srgb)")) {
      return false;
    }

    HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after CreateResource(srgb tex2d)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestSrgbTexture2DFormatPropagationGuestBacked() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(srgb format propagation guest-backed)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
  };

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_B8G8R8A8_UNORM_SRGB", kDxgiFormatB8G8R8A8UnormSrgb, AEROGPU_FORMAT_B8G8R8A8_UNORM_SRGB},
      {"DXGI_FORMAT_B8G8R8X8_UNORM_SRGB", kDxgiFormatB8G8R8X8UnormSrgb, AEROGPU_FORMAT_B8G8R8X8_UNORM_SRGB},
      {"DXGI_FORMAT_R8G8B8A8_UNORM_SRGB", kDxgiFormatR8G8B8A8UnormSrgb, AEROGPU_FORMAT_R8G8B8A8_UNORM_SRGB},
  };

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/4,
                                                /*height=*/4,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_READ,
                                                &tex),
               "CreateStagingTexture2DWithFormat(srgb guest-backed)")) {
      return false;
    }

    HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after CreateResource(srgb tex2d guest-backed)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }
    if (!Check(create_cmd->backing_alloc_id != 0, "guest-backed CREATE_TEXTURE2D backing_alloc_id != 0")) {
      return false;
    }

    bool found = false;
    for (auto h : dev.harness.last_allocs) {
      if (h == create_cmd->backing_alloc_id) {
        found = true;
        break;
      }
    }
    if (!Check(found, "submit alloc list contains guest-backed sRGB texture allocation")) {
      return false;
    }

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedTexture2DMipArrayCreateEncodesMipAndArray() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(mip+array create guest-backed)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     /*cpu_access_flags=*/0,
                                                     /*mip_levels=*/0, // full chain
                                                     /*array_size=*/2,
                                                     &tex),
             "CreateStagingTexture2DWithFormatAndDesc(mip+array)")) {
    return false;
  }

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource(mip+array tex2d)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->width == 4, "CREATE_TEXTURE2D width == 4")) {
    return false;
  }
  if (!Check(create_cmd->height == 4, "CREATE_TEXTURE2D height == 4")) {
    return false;
  }
  if (!Check(create_cmd->mip_levels == 3, "CREATE_TEXTURE2D mip_levels full chain (4x4 => 3)")) {
    return false;
  }
  if (!Check(create_cmd->array_layers == 2, "CREATE_TEXTURE2D array_layers == 2")) {
    return false;
  }
  const uint32_t expected_row_pitch = static_cast<uint32_t>(AlignUp(static_cast<size_t>(4u * 4u), 64));
  if (!Check(create_cmd->row_pitch_bytes == expected_row_pitch, "CREATE_TEXTURE2D row_pitch_bytes (mip0)")) {
    return false;
  }
  if (!Check(create_cmd->backing_alloc_id != 0, "CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }

  bool found_alloc = false;
  for (auto h : dev.harness.last_allocs) {
    if (h == create_cmd->backing_alloc_id) {
      found_alloc = true;
      break;
    }
  }
  if (!Check(found_alloc, "submit alloc list contains mip+array backing allocation")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCreateTexture2DMipArrayInitialDataDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(CreateResource initial mip+array tex2d guest-backed)")) {
    return false;
  }

  static constexpr uint32_t kWidth = 4;
  static constexpr uint32_t kHeight = 4;
  static constexpr uint32_t kMipLevels = 3;
  static constexpr uint32_t kArraySize = 2;

  struct SubInit {
    std::vector<uint8_t> bytes;
    uint32_t row_bytes = 0;
    uint32_t height = 0;
  };

  std::vector<SubInit> sub_inits;
  std::vector<AEROGPU_DDI_SUBRESOURCE_DATA> inits;
  sub_inits.reserve(static_cast<size_t>(kMipLevels) * kArraySize);
  inits.reserve(static_cast<size_t>(kMipLevels) * kArraySize);

  for (uint32_t layer = 0; layer < kArraySize; ++layer) {
    uint32_t level_w = kWidth;
    uint32_t level_h = kHeight;
    for (uint32_t mip = 0; mip < kMipLevels; ++mip) {
      SubInit sub{};
      sub.row_bytes = level_w * 4u;
      sub.height = level_h;
      sub.bytes.resize(static_cast<size_t>(sub.row_bytes) * static_cast<size_t>(sub.height));
      const uint8_t seed = static_cast<uint8_t>(0x40u + layer * 0x20u + mip * 0x08u);
      for (size_t i = 0; i < sub.bytes.size(); ++i) {
        sub.bytes[i] = static_cast<uint8_t>(seed + (i & 0x7u));
      }
      sub_inits.push_back(std::move(sub));

      AEROGPU_DDI_SUBRESOURCE_DATA init = {};
      init.pSysMem = sub_inits.back().bytes.data();
      init.SysMemPitch = sub_inits.back().row_bytes;
      init.SysMemSlicePitch = 0;
      inits.push_back(init);

      level_w = (level_w > 1) ? (level_w / 2) : 1u;
      level_h = (level_h > 1) ? (level_h / 2) : 1u;
    }
  }

  AEROGPU_DDIARG_CREATERESOURCE desc = {};
  desc.Dimension = AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D;
  desc.BindFlags = 0;
  desc.MiscFlags = 0;
  desc.Usage = AEROGPU_D3D11_USAGE_DEFAULT;
  desc.CPUAccessFlags = 0;
  desc.Width = kWidth;
  desc.Height = kHeight;
  desc.MipLevels = kMipLevels;
  desc.ArraySize = kArraySize;
  desc.Format = kDxgiFormatB8G8R8A8Unorm;
  desc.pInitialData = inits.data();
  desc.InitialDataCount = static_cast<uint32_t>(inits.size());

  TestResource tex{};
  const SIZE_T size = dev.device_funcs.pfnCalcPrivateResourceSize(dev.hDevice, &desc);
  if (!Check(size >= sizeof(void*), "CalcPrivateResourceSize returned a non-trivial size")) {
    return false;
  }
  tex.storage.assign(static_cast<size_t>(size), 0);
  tex.hResource.pDrvPrivate = tex.storage.data();

  HRESULT hr = dev.device_funcs.pfnCreateResource(dev.hDevice, &desc, tex.hResource);
  if (!Check(hr == S_OK, "CreateResource(tex2d mip+array initial data)")) {
    return false;
  }

  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CreateResource(mip+array initial data)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed CreateResource(mip+array initial tex2d) should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed CreateResource(mip+array initial tex2d) should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->mip_levels == kMipLevels, "CREATE_TEXTURE2D mip_levels matches")) {
    return false;
  }
  if (!Check(create_cmd->array_layers == kArraySize, "CREATE_TEXTURE2D array_layers matches")) {
    return false;
  }
  if (!Check(create_cmd->backing_alloc_id != 0, "CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }
  if (!Check(create_cmd->row_pitch_bytes != 0, "CREATE_TEXTURE2D row_pitch_bytes != 0")) {
    return false;
  }

  const uint32_t row_pitch0 = create_cmd->row_pitch_bytes;
  const uint64_t mip0_size = static_cast<uint64_t>(row_pitch0) * kHeight;
  const uint64_t mip1_size = static_cast<uint64_t>((kWidth / 2) * 4u) * (kHeight / 2);
  const uint64_t mip2_size = static_cast<uint64_t>(4u); // 1x1 RGBA8
  const uint64_t layer_stride = mip0_size + mip1_size + mip2_size;
  const uint64_t total_bytes = layer_stride * kArraySize;

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == 0, "RESOURCE_DIRTY_RANGE offset_bytes == 0")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == total_bytes, "RESOURCE_DIRTY_RANGE covers full mip+array chain")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= total_bytes, "backing allocation large enough")) {
    return false;
  }

  std::vector<uint8_t> expected(static_cast<size_t>(total_bytes), 0);
  size_t init_index = 0;
  size_t dst_offset = 0;
  for (uint32_t layer = 0; layer < kArraySize; ++layer) {
    uint32_t level_w = kWidth;
    uint32_t level_h = kHeight;
    for (uint32_t mip = 0; mip < kMipLevels; ++mip) {
      const uint32_t src_pitch = inits[init_index].SysMemPitch;
      const uint32_t dst_pitch = (mip == 0) ? row_pitch0 : src_pitch;
      const uint32_t row_bytes = src_pitch;
      const size_t sub_size = static_cast<size_t>(dst_pitch) * static_cast<size_t>(level_h);
      for (uint32_t y = 0; y < level_h; ++y) {
        std::memcpy(expected.data() + dst_offset + static_cast<size_t>(y) * dst_pitch,
                    static_cast<const uint8_t*>(inits[init_index].pSysMem) + static_cast<size_t>(y) * src_pitch,
                    row_bytes);
      }
      dst_offset += sub_size;
      init_index++;
      level_w = (level_w > 1) ? (level_w / 2) : 1u;
      level_h = (level_h > 1) ? (level_h / 2) : 1u;
    }
  }

  if (!Check(std::memcmp(alloc->bytes.data(), expected.data(), expected.size()) == 0,
             "backing allocation bytes match all subresource initial data")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedTexture2DMipArrayMapUnmapDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(mip+array map/unmap guest-backed)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                     /*mip_levels=*/3,
                                                     /*array_size=*/2,
                                                     &tex),
             "CreateStagingTexture2DWithFormatAndDesc(map/unmap mip+array)")) {
    return false;
  }

  const uint32_t subresource = 4; // mip1 layer1 when mip_levels=3.

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      tex.hResource,
                                                      subresource,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) guest-backed mip+array")) {
    return false;
  }
  if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
    return false;
  }
  if (!Check(mapped.RowPitch == 8, "Map RowPitch tight for mip1")) {
    return false;
  }
  if (!Check(mapped.DepthPitch == 16, "Map DepthPitch == RowPitch*height")) {
    return false;
  }

  uint8_t expected[16] = {};
  for (size_t i = 0; i < sizeof(expected); ++i) {
    expected[i] = static_cast<uint8_t>(0xD0u + i);
  }
  std::memcpy(mapped.pData, expected, sizeof(expected));
  void* mapped_ptr = mapped.pData;

  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, subresource);
  hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after StagingResourceUnmap(mip+array)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "mip+array Unmap should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->mip_levels == 3, "CREATE_TEXTURE2D mip_levels == 3")) {
    return false;
  }
  if (!Check(create_cmd->array_layers == 2, "CREATE_TEXTURE2D array_layers == 2")) {
    return false;
  }
  if (!Check(create_cmd->backing_alloc_id != 0, "CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }

  const uint32_t row_pitch0 = create_cmd->row_pitch_bytes;
  const uint64_t mip0_size = static_cast<uint64_t>(row_pitch0) * 4u;
  const uint64_t mip1_size = static_cast<uint64_t>(8u) * 2u;
  const uint64_t mip2_size = 4u;
  const uint64_t layer_stride = mip0_size + mip1_size + mip2_size;
  const uint64_t expected_offset = layer_stride + mip0_size;

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == expected_offset, "RESOURCE_DIRTY_RANGE offset matches subresource layout")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sizeof(expected), "RESOURCE_DIRTY_RANGE size matches subresource layout")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= expected_offset + sizeof(expected), "backing allocation large enough")) {
    return false;
  }

  const uint8_t* alloc_base = alloc->bytes.data();
  if (!Check(static_cast<uint8_t*>(mapped_ptr) == alloc_base + expected_offset, "Map pData points at subresource offset")) {
    return false;
  }
  if (!Check(std::memcmp(alloc_base + expected_offset, expected, sizeof(expected)) == 0, "backing allocation bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedUpdateSubresourceUPTexture2DMipArrayDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(UpdateSubresourceUP mip+array tex2d guest-backed)")) {
    return false;
  }

  TestResource tex{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     /*cpu_access_flags=*/0,
                                                     /*mip_levels=*/3,
                                                     /*array_size=*/2,
                                                     &tex),
             "CreateStagingTexture2DWithFormatAndDesc(UpdateSubresourceUP mip+array)")) {
    return false;
  }

  const uint32_t dst_subresource = 4; // mip1 layer1 when mip_levels=3.
  std::vector<uint8_t> sysmem(16);
  for (size_t i = 0; i < sysmem.size(); ++i) {
    sysmem[i] = static_cast<uint8_t>(0x70u + i);
  }

  dev.device_funcs.pfnUpdateSubresourceUP(dev.hDevice,
                                          tex.hResource,
                                          dst_subresource,
                                          /*pDstBox=*/nullptr,
                                          sysmem.data(),
                                          /*SysMemPitch=*/8,
                                          /*SysMemSlicePitch=*/0);

  const HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after UpdateSubresourceUP(mip+array)")) {
    return false;
  }

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "guest-backed mip+array UpdateSubresourceUP should not emit UPLOAD_RESOURCE")) {
    return false;
  }
  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE) == 1,
             "guest-backed mip+array UpdateSubresourceUP should emit RESOURCE_DIRTY_RANGE")) {
    return false;
  }

  CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);
  if (!Check(create_cmd->backing_alloc_id != 0, "CREATE_TEXTURE2D backing_alloc_id != 0")) {
    return false;
  }

  const uint32_t row_pitch0 = create_cmd->row_pitch_bytes;
  const uint64_t mip0_size = static_cast<uint64_t>(row_pitch0) * 4u;
  const uint64_t mip1_size = static_cast<uint64_t>(8u) * 2u;
  const uint64_t mip2_size = 4u;
  const uint64_t layer_stride = mip0_size + mip1_size + mip2_size;
  const uint64_t expected_offset = layer_stride + mip0_size;

  CmdLoc dirty_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty_loc.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(stream + dirty_loc.offset);
  if (!Check(dirty_cmd->offset_bytes == expected_offset, "RESOURCE_DIRTY_RANGE offset matches subresource layout")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == sysmem.size(), "RESOURCE_DIRTY_RANGE size matches sysmem upload")) {
    return false;
  }

  Allocation* alloc = dev.harness.FindAlloc(create_cmd->backing_alloc_id);
  if (!Check(alloc != nullptr, "backing allocation exists")) {
    return false;
  }
  if (!Check(alloc->bytes.size() >= expected_offset + sysmem.size(), "backing allocation large enough")) {
    return false;
  }
  if (!Check(std::memcmp(alloc->bytes.data() + expected_offset, sysmem.data(), sysmem.size()) == 0, "backing bytes")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCopySubresourceRegionTexture2DMipArrayReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(copy subresource mip+array tex2d guest-backed)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                     /*mip_levels=*/3,
                                                     /*array_size=*/2,
                                                     &src),
             "CreateStagingTexture2DWithFormatAndDesc(src mip+array)")) {
    return false;
  }
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     AEROGPU_D3D11_CPU_ACCESS_READ,
                                                     /*mip_levels=*/3,
                                                     /*array_size=*/2,
                                                     &dst),
             "CreateStagingTexture2DWithFormatAndDesc(dst mip+array)")) {
    return false;
  }

  const uint32_t src_subresource = 1; // mip1 layer0 when mip_levels=3.
  const uint32_t dst_subresource = 4; // mip1 layer1 when mip_levels=3.

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped_src = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      src.hResource,
                                                      src_subresource,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped_src);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src mip+array")) {
    return false;
  }
  if (!Check(mapped_src.pData != nullptr, "Map src returned non-null pData")) {
    return false;
  }
  if (!Check(mapped_src.RowPitch == 8, "Map src RowPitch tight for mip1")) {
    return false;
  }
  if (!Check(mapped_src.DepthPitch == 16, "Map src DepthPitch == RowPitch*height")) {
    return false;
  }

  std::vector<uint8_t> expected(static_cast<size_t>(mapped_src.DepthPitch));
  for (size_t i = 0; i < expected.size(); ++i) {
    expected[i] = static_cast<uint8_t>(0x30u + i);
  }
  std::memcpy(mapped_src.pData, expected.data(), expected.size());
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, src_subresource);

  hr = dev.device_funcs.pfnCopySubresourceRegion(dev.hDevice,
                                                 dst.hResource,
                                                 dst_subresource,
                                                 /*dst_x=*/0,
                                                 /*dst_y=*/0,
                                                 /*dst_z=*/0,
                                                 src.hResource,
                                                 src_subresource,
                                                 /*pSrcBox=*/nullptr);
  if (!Check(hr == S_OK, "CopySubresourceRegion(mip+array) returns S_OK")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped_dst = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              dst.hResource,
                                              dst_subresource,
                                              AEROGPU_DDI_MAP_READ,
                                              /*map_flags=*/0,
                                              &mapped_dst);
  if (!Check(hr == S_OK, "StagingResourceMap(READ) dst mip+array")) {
    return false;
  }
  if (!Check(mapped_dst.pData != nullptr, "Map dst returned non-null pData")) {
    return false;
  }
  if (!Check(mapped_dst.RowPitch == 8, "Map dst RowPitch tight for mip1")) {
    return false;
  }
  if (!Check(mapped_dst.DepthPitch == 16, "Map dst DepthPitch == RowPitch*height")) {
    return false;
  }
  if (!Check(std::memcmp(mapped_dst.pData, expected.data(), expected.size()) == 0, "CopySubresourceRegion bytes")) {
    return false;
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, dst_subresource);

  if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = dev.harness.last_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 1, "COPY_TEXTURE2D emitted")) {
    return false;
  }
  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
  if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
  if (!Check(copy_cmd->dst_mip_level == 1, "COPY_TEXTURE2D dst_mip_level == 1")) {
    return false;
  }
  if (!Check(copy_cmd->dst_array_layer == 1, "COPY_TEXTURE2D dst_array_layer == 1")) {
    return false;
  }
  if (!Check(copy_cmd->src_mip_level == 1, "COPY_TEXTURE2D src_mip_level == 1")) {
    return false;
  }
  if (!Check(copy_cmd->src_array_layer == 0, "COPY_TEXTURE2D src_array_layer == 0")) {
    return false;
  }
  if (!Check(copy_cmd->width == 2 && copy_cmd->height == 2, "COPY_TEXTURE2D width/height match mip1 dims")) {
    return false;
  }
  if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0, "COPY_TEXTURE2D has WRITEBACK_DST flag")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestGuestBackedCopyResourceTexture2DMipArrayReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true, /*async_fences=*/false),
             "InitTestDevice(copy mip+array tex2d guest-backed)")) {
    return false;
  }

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                     /*mip_levels=*/3,
                                                     /*array_size=*/2,
                                                     &src),
             "CreateStagingTexture2DWithFormatAndDesc(src mip+array)")) {
    return false;
  }
  if (!Check(CreateStagingTexture2DWithFormatAndDesc(&dev,
                                                     /*width=*/4,
                                                     /*height=*/4,
                                                     kDxgiFormatB8G8R8A8Unorm,
                                                     AEROGPU_D3D11_CPU_ACCESS_READ,
                                                     /*mip_levels=*/3,
                                                     /*array_size=*/2,
                                                     &dst),
             "CreateStagingTexture2DWithFormatAndDesc(dst mip+array)")) {
    return false;
  }

  // Fill each src subresource with a distinct byte pattern (pixel bytes only; padding stays zero).
  for (uint32_t subresource = 0; subresource < 6; ++subresource) {
    const uint32_t mip = subresource % 3;
    const uint32_t mip_w = (mip == 0) ? 4u : (mip == 1) ? 2u : 1u;
    const uint32_t mip_h = (mip == 0) ? 4u : (mip == 1) ? 2u : 1u;
    const uint32_t tight_row_bytes = mip_w * 4u;
    const uint8_t fill = static_cast<uint8_t>(0x10u + subresource);

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                        src.hResource,
                                                        subresource,
                                                        AEROGPU_DDI_MAP_WRITE,
                                                        /*map_flags=*/0,
                                                        &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src subresource")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map src returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map src returned RowPitch")) {
      return false;
    }

    auto* bytes = static_cast<uint8_t*>(mapped.pData);
    const uint32_t row_pitch = mapped.RowPitch;
    for (uint32_t y = 0; y < mip_h; ++y) {
      std::memset(bytes + static_cast<size_t>(y) * row_pitch, fill, tight_row_bytes);
      if (row_pitch > tight_row_bytes) {
        std::memset(bytes + static_cast<size_t>(y) * row_pitch + tight_row_bytes, 0, row_pitch - tight_row_bytes);
      }
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, subresource);
  }

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  // Force submission so we can validate the COPY_TEXTURE2D count once.
  HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
  if (!Check(hr == S_OK, "Flush after CopyResource(mip+array)")) {
    return false;
  }
  const std::vector<uint8_t> submitted_stream = dev.harness.last_stream;

  // Validate readback of each destination subresource.
  for (uint32_t subresource = 0; subresource < 6; ++subresource) {
    const uint32_t mip = subresource % 3;
    const uint32_t mip_w = (mip == 0) ? 4u : (mip == 1) ? 2u : 1u;
    const uint32_t mip_h = (mip == 0) ? 4u : (mip == 1) ? 2u : 1u;
    const uint32_t tight_row_bytes = mip_w * 4u;
    const uint8_t fill = static_cast<uint8_t>(0x10u + subresource);

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                dst.hResource,
                                                subresource,
                                                AEROGPU_DDI_MAP_READ,
                                                /*map_flags=*/0,
                                                &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(READ) dst subresource")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map dst returned non-null pData")) {
      return false;
    }
    if (!Check(mapped.RowPitch != 0, "Map dst returned RowPitch")) {
      return false;
    }

    const auto* bytes = static_cast<const uint8_t*>(mapped.pData);
    const uint32_t row_pitch = mapped.RowPitch;
    for (uint32_t y = 0; y < mip_h; ++y) {
      for (uint32_t x = 0; x < tight_row_bytes; ++x) {
        if (!Check(bytes[static_cast<size_t>(y) * row_pitch + x] == fill, "CopyResource subresource bytes")) {
          return false;
        }
      }
      for (uint32_t x = tight_row_bytes; x < row_pitch; ++x) {
        if (!Check(bytes[static_cast<size_t>(y) * row_pitch + x] == 0, "CopyResource subresource padding bytes")) {
          return false;
        }
      }
    }

    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, subresource);
  }

  if (!Check(ValidateStream(submitted_stream.data(), submitted_stream.size()), "ValidateStream")) {
    return false;
  }
  const uint8_t* stream = submitted_stream.data();
  const size_t stream_len = StreamBytesUsed(stream, submitted_stream.size());

  if (!Check(CountOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D) == 6, "COPY_TEXTURE2D emitted per subresource")) {
    return false;
  }
  CmdLoc copy_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_COPY_TEXTURE2D);
  if (!Check(copy_loc.hdr != nullptr, "COPY_TEXTURE2D location")) {
    return false;
  }
  const auto* copy_cmd = reinterpret_cast<const aerogpu_cmd_copy_texture2d*>(stream + copy_loc.offset);
  if (!Check((copy_cmd->flags & AEROGPU_COPY_FLAG_WRITEBACK_DST) != 0, "COPY_TEXTURE2D has WRITEBACK_DST flag")) {
    return false;
  }

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

bool TestBcTexture2DLayout() {
#if AEROGPU_ABI_MINOR < 2
  // ABI 1.2 adds BC formats.
  return true;
#else
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/false),
             "InitTestDevice(bc texture layout)")) {
    return false;
  }

  struct Case {
    const char* name;
    uint32_t dxgi_format;
    uint32_t expected_format;
    uint32_t block_bytes;
  };

  static constexpr uint32_t kWidth = 5;
  static constexpr uint32_t kHeight = 5;

  static constexpr Case kCases[] = {
      {"DXGI_FORMAT_BC1_UNORM", kDxgiFormatBc1Unorm, AEROGPU_FORMAT_BC1_RGBA_UNORM, 8},
      {"DXGI_FORMAT_BC1_UNORM_SRGB", kDxgiFormatBc1UnormSrgb, AEROGPU_FORMAT_BC1_RGBA_UNORM_SRGB, 8},
      {"DXGI_FORMAT_BC2_UNORM", kDxgiFormatBc2Unorm, AEROGPU_FORMAT_BC2_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC2_UNORM_SRGB", kDxgiFormatBc2UnormSrgb, AEROGPU_FORMAT_BC2_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC3_UNORM", kDxgiFormatBc3Unorm, AEROGPU_FORMAT_BC3_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC3_UNORM_SRGB", kDxgiFormatBc3UnormSrgb, AEROGPU_FORMAT_BC3_RGBA_UNORM_SRGB, 16},
      {"DXGI_FORMAT_BC7_UNORM", kDxgiFormatBc7Unorm, AEROGPU_FORMAT_BC7_RGBA_UNORM, 16},
      {"DXGI_FORMAT_BC7_UNORM_SRGB", kDxgiFormatBc7UnormSrgb, AEROGPU_FORMAT_BC7_RGBA_UNORM_SRGB, 16},
  };

  auto div_round_up = [](uint32_t v, uint32_t d) -> uint32_t { return (v + d - 1) / d; };

  for (const auto& c : kCases) {
    TestResource tex{};
    if (!Check(CreateStagingTexture2DWithFormat(&dev,
                                                /*width=*/kWidth,
                                                /*height=*/kHeight,
                                                c.dxgi_format,
                                                /*cpu_access_flags=*/AEROGPU_D3D11_CPU_ACCESS_WRITE,
                                                &tex),
               "CreateStagingTexture2DWithFormat(bc)")) {
      return false;
    }

    HRESULT hr = dev.device_funcs.pfnFlush(dev.hDevice);
    if (!Check(hr == S_OK, "Flush after CreateResource(bc tex2d)")) {
      return false;
    }

    if (!Check(ValidateStream(dev.harness.last_stream.data(), dev.harness.last_stream.size()), "ValidateStream")) {
      return false;
    }
    const uint8_t* stream = dev.harness.last_stream.data();
    const size_t stream_len = StreamBytesUsed(stream, dev.harness.last_stream.size());

    CmdLoc create_loc = FindLastOpcode(stream, stream_len, AEROGPU_CMD_CREATE_TEXTURE2D);
    if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
      return false;
    }
    const auto* create_cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(stream + create_loc.offset);

    const uint32_t expected_row_pitch = div_round_up(kWidth, 4) * c.block_bytes;
    const uint32_t expected_rows = div_round_up(kHeight, 4);
    const uint32_t expected_depth_pitch = expected_row_pitch * expected_rows;

    char msg[256] = {};
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D format matches expected for %s", c.name);
    if (!Check(create_cmd->format == c.expected_format, msg)) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "CREATE_TEXTURE2D row_pitch_bytes matches expected for %s", c.name);
    if (!Check(create_cmd->row_pitch_bytes == expected_row_pitch, msg)) {
      return false;
    }

    AEROGPU_DDI_MAPPED_SUBRESOURCE mapped = {};
    hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                tex.hResource,
                                                /*subresource=*/0,
                                                AEROGPU_DDI_MAP_WRITE,
                                                /*map_flags=*/0,
                                                &mapped);
    if (!Check(hr == S_OK, "StagingResourceMap(WRITE) bc tex2d")) {
      return false;
    }
    if (!Check(mapped.pData != nullptr, "Map returned non-null pData")) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "Map RowPitch matches expected for %s", c.name);
    if (!Check(mapped.RowPitch == expected_row_pitch, msg)) {
      return false;
    }
    std::snprintf(msg, sizeof(msg), "Map DepthPitch matches expected for %s", c.name);
    if (!Check(mapped.DepthPitch == expected_depth_pitch, msg)) {
      return false;
    }
    dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, tex.hResource, /*subresource=*/0);

    dev.device_funcs.pfnDestroyResource(dev.hDevice, tex.hResource);
  }

  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
#endif
}

bool TestMapDoNotWaitRespectsFenceCompletion() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false, /*async_fences=*/true),
             "InitTestDevice(map do_not_wait async fences)")) {
    return false;
  }
  dev.callbacks.pfnWaitForFence = nullptr;
  dev.callbacks.pfnQueryCompletedFence = &Harness::QueryCompletedFence;

  TestResource src{};
  TestResource dst{};
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_WRITE, &src),
             "CreateStagingTexture2D(src)")) {
    return false;
  }
  if (!Check(CreateStagingTexture2D(&dev, /*width=*/3, /*height=*/2, AEROGPU_D3D11_CPU_ACCESS_READ, &dst),
             "CreateStagingTexture2D(dst)")) {
    return false;
  }

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped_src = {};
  HRESULT hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                                      src.hResource,
                                                      /*subresource=*/0,
                                                      AEROGPU_DDI_MAP_WRITE,
                                                      /*map_flags=*/0,
                                                      &mapped_src);
  if (!Check(hr == S_OK, "StagingResourceMap(WRITE) src tex2d")) {
    return false;
  }
  if (!Check(mapped_src.pData != nullptr, "Map src returned non-null pData")) {
    return false;
  }
  if (!Check(mapped_src.RowPitch != 0, "Map src returned RowPitch")) {
    return false;
  }

  const uint32_t width = 3;
  const uint32_t height = 2;
  const uint32_t bytes_per_row = width * 4u;
  const uint32_t src_pitch = mapped_src.RowPitch;
  auto* src_bytes = static_cast<uint8_t*>(mapped_src.pData);
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      src_bytes[static_cast<size_t>(y) * src_pitch + x] = static_cast<uint8_t>((y + 1) * 0x10u + x);
    }
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, src.hResource, /*subresource=*/0);

  dev.device_funcs.pfnCopyResource(dev.hDevice, dst.hResource, src.hResource);

  AEROGPU_DDI_MAPPED_SUBRESOURCE mapped_dst = {};
  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              dst.hResource,
                                              /*subresource=*/0,
                                              AEROGPU_DDI_MAP_READ,
                                              AEROGPU_D3D11_MAP_FLAG_DO_NOT_WAIT,
                                              &mapped_dst);
  if (!Check(hr == DXGI_ERROR_WAS_STILL_DRAWING, "Map(READ, DO_NOT_WAIT) returns still drawing")) {
    return false;
  }

  const uint64_t fence = dev.harness.last_submitted_fence.load(std::memory_order_relaxed);
  if (!Check(fence != 0, "async submit produced a non-zero fence")) {
    return false;
  }

  dev.harness.completed_fence.store(fence, std::memory_order_relaxed);
  dev.harness.fence_cv.notify_all();

  hr = dev.device_funcs.pfnStagingResourceMap(dev.hDevice,
                                              dst.hResource,
                                              /*subresource=*/0,
                                              AEROGPU_DDI_MAP_READ,
                                              /*map_flags=*/0,
                                              &mapped_dst);
  if (!Check(hr == S_OK, "Map(READ) succeeds after fence completion")) {
    return false;
  }
  if (!Check(mapped_dst.pData != nullptr, "Map dst returned non-null pData")) {
    return false;
  }
  if (!Check(mapped_dst.RowPitch == src_pitch, "Map dst RowPitch matches src")) {
    return false;
  }

  const auto* dst_bytes = static_cast<const uint8_t*>(mapped_dst.pData);
  const uint32_t dst_pitch = mapped_dst.RowPitch;
  for (uint32_t y = 0; y < height; y++) {
    for (uint32_t x = 0; x < bytes_per_row; x++) {
      const uint8_t expected = static_cast<uint8_t>((y + 1) * 0x10u + x);
      if (!Check(dst_bytes[static_cast<size_t>(y) * dst_pitch + x] == expected, "Map dst bytes match")) {
        return false;
      }
    }
  }
  dev.device_funcs.pfnStagingResourceUnmap(dev.hDevice, dst.hResource, /*subresource=*/0);

  dev.device_funcs.pfnDestroyResource(dev.hDevice, dst.hResource);
  dev.device_funcs.pfnDestroyResource(dev.hDevice, src.hResource);
  dev.device_funcs.pfnDestroyDevice(dev.hDevice);
  dev.adapter_funcs.pfnCloseAdapter(dev.hAdapter);
  return true;
}

} // namespace

int main() {
  bool ok = true;
  ok &= TestHostOwnedBufferUnmapUploads();
  ok &= TestHostOwnedTextureUnmapUploads();
  ok &= TestCreateTexture2dSrgbFormatEncodesSrgbAerogpuFormat();
  ok &= TestGuestBackedBufferUnmapDirtyRange();
  ok &= TestGuestBackedTextureUnmapDirtyRange();
  ok &= TestGuestBackedBcTextureUnmapDirtyRange();
  ok &= TestMapUsageValidation();
  ok &= TestMapFlagsValidation();
  ok &= TestMapDoNotWaitReportsStillDrawing();
  ok &= TestMapBlockingWaitUsesInfiniteTimeout();
  ok &= TestInvalidUnmapReportsError();
  ok &= TestDynamicMapFlagsValidation();
  ok &= TestHostOwnedDynamicIABufferUploads();
  ok &= TestGuestBackedDynamicIABufferDirtyRange();
  ok &= TestDynamicBufferUsageValidation();
  ok &= TestHostOwnedDynamicConstantBufferUploads();
  ok &= TestGuestBackedDynamicConstantBufferDirtyRange();
  ok &= TestSubmitAllocListTracksBoundConstantBuffer();
  ok &= TestSubmitAllocListTracksBoundShaderResource();
  ok &= TestHostOwnedCopyResourceBufferReadback();
  ok &= TestHostOwnedCopyResourceTextureReadback();
  ok &= TestHostOwnedCopyResourceBcTextureReadback();
  ok &= TestHostOwnedCopySubresourceRegionBcTextureReadback();
  ok &= TestGuestBackedCopyResourceBufferReadback();
  ok &= TestGuestBackedCopyResourceTextureReadback();
  ok &= TestGuestBackedCopyResourceBcTextureReadback();
  ok &= TestGuestBackedCopySubresourceRegionBcTextureReadback();
  ok &= TestHostOwnedUpdateSubresourceUPBufferUploads();
  ok &= TestGuestBackedUpdateSubresourceUPBufferDirtyRange();
  ok &= TestHostOwnedUpdateSubresourceUPTextureUploads();
  ok &= TestGuestBackedUpdateSubresourceUPTextureDirtyRange();
  ok &= TestHostOwnedUpdateSubresourceUPBcTextureUploads();
  ok &= TestGuestBackedUpdateSubresourceUPBcTextureDirtyRange();
  ok &= TestHostOwnedUpdateSubresourceUPBufferBoxUploads();
  ok &= TestHostOwnedUpdateSubresourceUPTextureBoxUploads();
  ok &= TestGuestBackedUpdateSubresourceUPBufferBoxDirtyRange();
  ok &= TestGuestBackedUpdateSubresourceUPTextureBoxDirtyRange();
  ok &= TestHostOwnedUpdateSubresourceUPBcTextureBoxUploads();
  ok &= TestGuestBackedUpdateSubresourceUPBcTextureBoxDirtyRange();
  ok &= TestHostOwnedUpdateSubresourceUPBcTextureBoxRejectsMisaligned();
  ok &= TestGuestBackedUpdateSubresourceUPBcTextureBoxRejectsMisaligned();
  ok &= TestHostOwnedCreateBufferInitialDataUploads();
  ok &= TestGuestBackedCreateBufferInitialDataDirtyRange();
  ok &= TestHostOwnedCreateTextureInitialDataUploads();
  ok &= TestGuestBackedCreateTextureInitialDataDirtyRange();
  ok &= TestHostOwnedCreateBcTextureInitialDataUploads();
  ok &= TestGuestBackedCreateBcTextureInitialDataDirtyRange();
  ok &= TestSrgbTexture2DFormatPropagation();
  ok &= TestSrgbTexture2DFormatPropagationGuestBacked();
  ok &= TestGuestBackedTexture2DMipArrayCreateEncodesMipAndArray();
  ok &= TestGuestBackedCreateTexture2DMipArrayInitialDataDirtyRange();
  ok &= TestGuestBackedTexture2DMipArrayMapUnmapDirtyRange();
  ok &= TestGuestBackedUpdateSubresourceUPTexture2DMipArrayDirtyRange();
  ok &= TestGuestBackedCopySubresourceRegionTexture2DMipArrayReadback();
  ok &= TestGuestBackedCopyResourceTexture2DMipArrayReadback();
  ok &= TestBcTexture2DLayout();
  ok &= TestMapDoNotWaitRespectsFenceCompletion();

  if (!ok) {
    return 1;
  }
  std::fprintf(stderr, "PASS: aerogpu_d3d10_11_map_unmap_tests\n");
  return 0;
}
