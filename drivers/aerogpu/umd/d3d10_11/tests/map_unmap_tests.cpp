#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <utility>
#include <vector>

#include "aerogpu_d3d10_11_umd.h"
#include "aerogpu_cmd.h"

namespace {

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
  if (!Check(stream->size_bytes == len, "stream size_bytes matches submitted length")) {
    return false;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset < stream->size_bytes) {
    if (!Check(stream->size_bytes - offset >= sizeof(aerogpu_cmd_hdr), "packet header fits")) {
      return false;
    }
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (!Check(hdr->size_bytes >= sizeof(aerogpu_cmd_hdr), "packet size >= header")) {
      return false;
    }
    if (!Check((hdr->size_bytes & 3u) == 0, "packet size is 4-byte aligned")) {
      return false;
    }
    if (!Check(hdr->size_bytes <= stream->size_bytes - offset, "packet size within stream")) {
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

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      loc.hdr = hdr;
      loc.offset = offset;
    }
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > len - offset) {
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

  size_t count = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      count++;
    }
    if (hdr->size_bytes < sizeof(aerogpu_cmd_hdr) || hdr->size_bytes > len - offset) {
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

  std::vector<Allocation> allocations;
  AEROGPU_WDDM_ALLOCATION_HANDLE next_handle = 1;

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

    uint64_t bytes = 0;
    if (desc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_BUFFER) {
      bytes = desc->ByteWidth;
    } else if (desc->Dimension == AEROGPU_DDI_RESOURCE_DIMENSION_TEX2D) {
      // For tests we only need buffers; keep a safe fallback in case future
      // tests allocate a texture by mistake.
      const uint64_t width = desc->Width ? desc->Width : 1u;
      const uint64_t height = desc->Height ? desc->Height : 1u;
      const uint32_t bytes_per_row = static_cast<uint32_t>(width * 4u);
      const uint32_t row_pitch = static_cast<uint32_t>(AlignUp(bytes_per_row, 64));
      bytes = static_cast<uint64_t>(row_pitch) * height;
      if (out_row_pitch_bytes) {
        *out_row_pitch_bytes = row_pitch;
      }
    } else {
      bytes = desc->ByteWidth;
    }

    // Mirror the UMD's conservative alignment expectations.
    bytes = AlignUp(static_cast<size_t>(bytes), 256);
    alloc.bytes.resize(static_cast<size_t>(bytes), 0);

    h->allocations.push_back(std::move(alloc));
    *out_handle = h->allocations.back().handle;
    *out_size_bytes = bytes;
    if (out_row_pitch_bytes && *out_row_pitch_bytes == 0) {
      *out_row_pitch_bytes = 0;
    }
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
      *out_fence = 0;
    }
    return S_OK;
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

bool InitTestDevice(TestDevice* out, bool want_backing_allocations) {
  if (!out) {
    return false;
  }

  out->callbacks.pUserContext = &out->harness;
  out->callbacks.pfnSubmitCmdStream = &Harness::SubmitCmdStream;
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

constexpr uint32_t kDxgiFormatB8G8R8A8Unorm = 87; // DXGI_FORMAT_B8G8R8A8_UNORM

constexpr uint32_t kD3D11BindVertexBuffer = 0x1;
constexpr uint32_t kD3D11BindIndexBuffer = 0x2;
constexpr uint32_t kD3D11BindConstantBuffer = 0x4;

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

bool CreateStagingTexture2D(TestDevice* dev,
                            uint32_t width,
                            uint32_t height,
                            uint32_t cpu_access_flags,
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
  desc.MipLevels = 1;
  desc.ArraySize = 1;
  desc.Format = kDxgiFormatB8G8R8A8Unorm;
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

bool TestHostOwnedBufferUnmapUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false), "InitTestDevice(host-owned)")) {
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
  const size_t stream_len = dev.harness.last_stream.size();

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
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false), "InitTestDevice(host-owned tex2d)")) {
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
  const size_t stream_len = dev.harness.last_stream.size();

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

bool TestGuestBackedBufferUnmapDirtyRange() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true), "InitTestDevice(guest-backed)")) {
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
  const size_t stream_len = dev.harness.last_stream.size();

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
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true), "InitTestDevice(guest-backed tex2d)")) {
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
  const size_t stream_len = dev.harness.last_stream.size();

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

bool TestMapUsageValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false), "InitTestDevice(validation)")) {
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
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false), "InitTestDevice(map flags)")) {
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

bool TestDynamicMapFlagsValidation() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false), "InitTestDevice(dynamic map flags)")) {
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
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false), "InitTestDevice(dynamic ia host-owned)")) {
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
  const size_t stream_len = dev.harness.last_stream.size();

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
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true), "InitTestDevice(dynamic ia guest-backed)")) {
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
  const size_t stream_len = dev.harness.last_stream.size();

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
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false), "InitTestDevice(dynamic validation)")) {
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

bool TestGuestBackedCopyResourceBufferReadback() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true), "InitTestDevice(copy buffer)")) {
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
  const size_t stream_len = dev.harness.last_stream.size();

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
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true), "InitTestDevice(copy tex2d)")) {
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
  const size_t stream_len = dev.harness.last_stream.size();

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

bool TestHostOwnedUpdateSubresourceUPBufferUploads() {
  TestDevice dev{};
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false), "InitTestDevice(UpdateSubresourceUP buffer host-owned)")) {
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
  const size_t stream_len = dev.harness.last_stream.size();

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
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true), "InitTestDevice(UpdateSubresourceUP buffer guest-backed)")) {
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
  const size_t stream_len = dev.harness.last_stream.size();

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
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/false), "InitTestDevice(UpdateSubresourceUP tex2d host-owned)")) {
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
  const size_t stream_len = dev.harness.last_stream.size();

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
  if (!Check(InitTestDevice(&dev, /*want_backing_allocations=*/true), "InitTestDevice(UpdateSubresourceUP tex2d guest-backed)")) {
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
  const size_t stream_len = dev.harness.last_stream.size();

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

} // namespace

int main() {
  bool ok = true;
  ok &= TestHostOwnedBufferUnmapUploads();
  ok &= TestHostOwnedTextureUnmapUploads();
  ok &= TestGuestBackedBufferUnmapDirtyRange();
  ok &= TestGuestBackedTextureUnmapDirtyRange();
  ok &= TestMapUsageValidation();
  ok &= TestMapFlagsValidation();
  ok &= TestDynamicMapFlagsValidation();
  ok &= TestHostOwnedDynamicIABufferUploads();
  ok &= TestGuestBackedDynamicIABufferDirtyRange();
  ok &= TestDynamicBufferUsageValidation();
  ok &= TestGuestBackedCopyResourceBufferReadback();
  ok &= TestGuestBackedCopyResourceTextureReadback();
  ok &= TestHostOwnedUpdateSubresourceUPBufferUploads();
  ok &= TestGuestBackedUpdateSubresourceUPBufferDirtyRange();
  ok &= TestHostOwnedUpdateSubresourceUPTextureUploads();
  ok &= TestGuestBackedUpdateSubresourceUPTextureDirtyRange();

  if (!ok) {
    return 1;
  }
  std::fprintf(stderr, "PASS: aerogpu_d3d10_11_map_unmap_tests\n");
  return 0;
}
