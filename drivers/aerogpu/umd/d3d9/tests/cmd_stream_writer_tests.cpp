#include <cstddef>
#include <cstdint>
#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <limits>
#include <mutex>
#include <thread>
#include <vector>
#include <condition_variable>

#include "aerogpu_d3d9_objects.h"
#include "aerogpu_d3d9_submit.h"
#include "aerogpu_kmd_query.h"

#include "aerogpu_cmd_stream_writer.h"
#include "aerogpu_pci.h"
#include "aerogpu_wddm_alloc.h"

namespace aerogpu {

namespace {

// D3DERR_INVALIDCALL from d3d9.h.
constexpr HRESULT kD3DErrInvalidCall = 0x8876086CUL;
constexpr uint32_t kD3d9ShaderStageVs = 0u;
constexpr D3DDDIFORMAT kD3dFmtIndex16 = static_cast<D3DDDIFORMAT>(101); // D3DFMT_INDEX16

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

struct unknown_cmd_fixed {
  aerogpu_cmd_hdr hdr;
  uint32_t value;
};

struct CmdLoc {
  const aerogpu_cmd_hdr* hdr = nullptr;
  size_t offset = 0;
};

size_t StreamBytesUsed(const uint8_t* buf, size_t capacity) {
  if (!buf || capacity < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }

  // Forward-compat: `aerogpu_cmd_stream_header.size_bytes` is bytes-used. Callers may provide a
  // backing buffer (capacity) larger than `size_bytes` (page rounding / reuse). Helpers must only
  // walk the declared prefix and ignore trailing bytes.
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  const size_t used = stream->size_bytes;
  if (used < sizeof(aerogpu_cmd_stream_header) || used > capacity) {
    return 0;
  }
  return used;
}

CmdLoc FindLastOpcode(const uint8_t* buf, size_t capacity, uint32_t opcode) {
  CmdLoc loc{};
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (stream_len == 0) {
    return loc;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      loc.hdr = hdr;
      loc.offset = offset;
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return loc;
}

size_t CountOpcode(const uint8_t* buf, size_t capacity, uint32_t opcode) {
  const size_t stream_len = StreamBytesUsed(buf, capacity);
  if (stream_len == 0) {
    return 0;
  }

  size_t count = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= stream_len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      count++;
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > stream_len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return count;
}

bool ValidateStream(const uint8_t* buf, size_t capacity) {
  if (!Check(buf != nullptr, "buffer must be non-null")) {
    return false;
  }
  if (!Check(capacity >= sizeof(aerogpu_cmd_stream_header), "buffer must contain stream header")) {
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
  if (!Check(stream->size_bytes <= capacity, "stream size_bytes within capacity")) {
    return false;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset < stream->size_bytes) {
    if (!Check((offset & 3u) == 0, "packet offset 4-byte aligned")) {
      return false;
    }
    if (!Check(offset + sizeof(aerogpu_cmd_hdr) <= stream->size_bytes, "packet header within stream")) {
      return false;
    }

    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (!Check(hdr->size_bytes >= sizeof(aerogpu_cmd_hdr), "packet size >= hdr")) {
      return false;
    }
    if (!Check((hdr->size_bytes & 3u) == 0, "packet size 4-byte aligned")) {
      return false;
    }
    if (!Check(offset + hdr->size_bytes <= stream->size_bytes, "packet fits within stream")) {
      return false;
    }

    offset += hdr->size_bytes;
  }
  return Check(offset == stream->size_bytes, "parser consumed entire stream");
}

bool TestHeaderFieldsAndFinalize() {
  uint8_t buf[256];
  std::memset(buf, 0xCD, sizeof(buf));

  SpanCmdStreamWriter w(buf, sizeof(buf));
  w.reset();

  if (!Check(w.error() == CmdStreamError::kOk, "reset error == kOk")) {
    return false;
  }

  if (!Check(w.bytes_used() == sizeof(aerogpu_cmd_stream_header), "bytes_used after reset")) {
    return false;
  }
  if (!Check(w.bytes_remaining() == sizeof(buf) - sizeof(aerogpu_cmd_stream_header), "bytes_remaining after reset")) {
    return false;
  }
  if (!Check(w.empty(), "empty after reset")) {
    return false;
  }

  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  if (!Check(stream->magic == AEROGPU_CMD_STREAM_MAGIC, "header magic")) {
    return false;
  }
  if (!Check(stream->abi_version == AEROGPU_ABI_VERSION_U32, "header abi_version")) {
    return false;
  }
  if (!Check(stream->flags == AEROGPU_CMD_STREAM_FLAG_NONE, "header flags")) {
    return false;
  }
  if (!Check(stream->size_bytes == sizeof(aerogpu_cmd_stream_header), "header size_bytes after reset")) {
    return false;
  }

  auto* present = w.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  if (!Check(present != nullptr, "append_fixed(PRESENT)")) {
    return false;
  }
  present->scanout_id = 0;
  present->flags = AEROGPU_PRESENT_FLAG_NONE;

  const size_t expected = sizeof(aerogpu_cmd_stream_header) + AlignUp(sizeof(aerogpu_cmd_present), 4);
  if (!Check(w.bytes_used() == expected, "bytes_used after append")) {
    return false;
  }
  if (!Check(!w.empty(), "not empty after append")) {
    return false;
  }

  w.finalize();
  if (!Check(stream->size_bytes == expected, "header size_bytes after finalize")) {
    return false;
  }

  return ValidateStream(buf, sizeof(buf));
}

bool TestAlignmentAndPadding() {
  uint8_t buf[256];
  std::memset(buf, 0xAB, sizeof(buf));

  SpanCmdStreamWriter w(buf, sizeof(buf));
  w.reset();

  const uint8_t payload[3] = {0x01, 0x02, 0x03};
  auto* cmd = w.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, payload, sizeof(payload));
  if (!Check(cmd != nullptr, "append_with_payload(CREATE_SHADER_DXBC)")) {
    return false;
  }

  cmd->shader_handle = 42;
  cmd->stage = AEROGPU_SHADER_STAGE_PIXEL;
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sizeof(payload));
  cmd->reserved0 = 0;

  const size_t cmd_size = sizeof(aerogpu_cmd_create_shader_dxbc) + sizeof(payload);
  const size_t aligned_size = AlignUp(cmd_size, 4);
  if (!Check(cmd->hdr.size_bytes == aligned_size, "cmd hdr.size_bytes aligned")) {
    return false;
  }

  const size_t payload_off = sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_create_shader_dxbc);
  if (!Check(std::memcmp(buf + payload_off, payload, sizeof(payload)) == 0, "payload bytes match")) {
    return false;
  }

  // Validate padding bytes are zeroed.
  for (size_t i = cmd_size; i < aligned_size; i++) {
    if (!Check(buf[sizeof(aerogpu_cmd_stream_header) + i] == 0, "payload padding is zero")) {
      return false;
    }
  }

  w.finalize();
  return ValidateStream(buf, sizeof(buf));
}

bool TestUnknownOpcodeSkipBySize() {
  uint8_t buf[256] = {};

  SpanCmdStreamWriter w(buf, sizeof(buf));
  w.reset();

  auto* u = w.append_fixed<unknown_cmd_fixed>(0xDEADBEEFu);
  if (!Check(u != nullptr, "append_fixed(unknown opcode)")) {
    return false;
  }
  u->value = 0x12345678u;

  auto* present = w.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  if (!Check(present != nullptr, "append_fixed(PRESENT)")) {
    return false;
  }
  present->scanout_id = 0;
  present->flags = AEROGPU_PRESENT_FLAG_NONE;

  w.finalize();
  return ValidateStream(buf, sizeof(buf));
}

bool TestOutOfSpaceReturnsNullptrAndSetsError() {
  uint8_t buf[sizeof(aerogpu_cmd_stream_header) + 4] = {};

  SpanCmdStreamWriter w(buf, sizeof(buf));
  w.reset();
  if (!Check(w.empty(), "empty after reset")) {
    return false;
  }

  auto* present = w.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  if (!Check(present == nullptr, "append_fixed returns nullptr on overflow")) {
    return false;
  }
  if (!Check(w.error() == CmdStreamError::kInsufficientSpace, "overflow sets kInsufficientSpace")) {
    return false;
  }
  if (!Check(w.bytes_used() == sizeof(aerogpu_cmd_stream_header), "bytes_used unchanged after overflow")) {
    return false;
  }

  w.finalize();
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  return Check(stream->size_bytes == sizeof(aerogpu_cmd_stream_header), "finalize keeps size_bytes at header");
}

bool TestCmdStreamWriterOverflowReturnsNullAndSetsError() {
  std::vector<uint8_t> buf(sizeof(aerogpu_cmd_stream_header) + 4, 0);

  CmdStreamWriter w;
  w.set_span(buf.data(), buf.size());

  if (!Check(w.empty(), "CmdStreamWriter empty after set_span")) {
    return false;
  }

  auto* present = w.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  if (!Check(present == nullptr, "CmdStreamWriter append_fixed returns nullptr on overflow")) {
    return false;
  }
  if (!Check(w.error() == CmdStreamError::kInsufficientSpace, "CmdStreamWriter overflow sets kInsufficientSpace")) {
    return false;
  }
  if (!Check(w.bytes_used() == sizeof(aerogpu_cmd_stream_header), "CmdStreamWriter bytes_used unchanged after overflow")) {
    return false;
  }

  w.finalize();
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf.data());
  return Check(stream->size_bytes == sizeof(aerogpu_cmd_stream_header), "CmdStreamWriter finalize keeps size_bytes at header");
}

bool TestFixedPacketPadding() {
  uint8_t buf[256];
  std::memset(buf, 0xEF, sizeof(buf));

#pragma pack(push, 1)
  struct odd_fixed {
    aerogpu_cmd_hdr hdr;
    uint16_t v;
  };
#pragma pack(pop)

  if (!Check(sizeof(odd_fixed) == 10, "odd_fixed packed size")) {
    return false;
  }

  SpanCmdStreamWriter w(buf, sizeof(buf));
  w.reset();

  auto* cmd = w.append_fixed<odd_fixed>(0x9000u);
  if (!Check(cmd != nullptr, "append_fixed(odd_fixed)")) {
    return false;
  }
  cmd->v = 0xBEEFu;

  if (!Check(cmd->hdr.size_bytes == 12, "odd_fixed size_bytes padded to 12")) {
    return false;
  }

  const size_t cmd_off = sizeof(aerogpu_cmd_stream_header);
  if (!Check(buf[cmd_off + sizeof(odd_fixed) + 0] == 0, "padding byte 0 zero")) {
    return false;
  }
  if (!Check(buf[cmd_off + sizeof(odd_fixed) + 1] == 0, "padding byte 1 zero")) {
    return false;
  }

  w.finalize();
  return ValidateStream(buf, sizeof(buf));
}

bool EmitRepresentativeCommands(CmdStreamWriter& w, const uint8_t* dxbc, size_t dxbc_len) {
  w.reset();

  auto* create_buf = w.append_fixed<aerogpu_cmd_create_buffer>(AEROGPU_CMD_CREATE_BUFFER);
  if (!Check(create_buf != nullptr, "CREATE_BUFFER")) {
    return false;
  }
  create_buf->buffer_handle = 0x100;
  create_buf->usage_flags = AEROGPU_RESOURCE_USAGE_VERTEX_BUFFER;
  create_buf->size_bytes = 4096;
  create_buf->backing_alloc_id = 0;
  create_buf->backing_offset_bytes = 0;
  create_buf->reserved0 = 0;

  auto* create_tex = w.append_fixed<aerogpu_cmd_create_texture2d>(AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_tex != nullptr, "CREATE_TEXTURE2D")) {
    return false;
  }
  create_tex->texture_handle = 0x200;
  create_tex->usage_flags = AEROGPU_RESOURCE_USAGE_TEXTURE;
  create_tex->format = AEROGPU_FORMAT_B8G8R8A8_UNORM;
  create_tex->width = 128;
  create_tex->height = 64;
  create_tex->mip_levels = 1;
  create_tex->array_layers = 1;
  create_tex->row_pitch_bytes = 128 * 4;
  create_tex->backing_alloc_id = 0;
  create_tex->backing_offset_bytes = 0;
  create_tex->reserved0 = 0;

  auto* create_shader = w.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, dxbc, dxbc_len);
  if (!Check(create_shader != nullptr, "CREATE_SHADER_DXBC")) {
    return false;
  }
  create_shader->shader_handle = 0x300;
  create_shader->stage = AEROGPU_SHADER_STAGE_VERTEX;
  create_shader->dxbc_size_bytes = static_cast<uint32_t>(dxbc_len);
  create_shader->reserved0 = 0;

  auto* present = w.append_fixed<aerogpu_cmd_present_ex>(AEROGPU_CMD_PRESENT_EX);
  if (!Check(present != nullptr, "PRESENT_EX")) {
    return false;
  }
  present->scanout_id = 0;
  present->flags = AEROGPU_PRESENT_FLAG_VSYNC;
  present->d3d9_present_flags = 0x1234u;
  present->reserved0 = 0;

  auto* export_shared = w.append_fixed<aerogpu_cmd_export_shared_surface>(AEROGPU_CMD_EXPORT_SHARED_SURFACE);
  if (!Check(export_shared != nullptr, "EXPORT_SHARED_SURFACE")) {
    return false;
  }
  export_shared->resource_handle = 0x200;
  export_shared->reserved0 = 0;
  export_shared->share_token = 0x1122334455667788ull;

  w.finalize();
  return Check(w.error() == CmdStreamError::kOk, "writer error == kOk");
}

bool TestOwnedAndBorrowedStreamsMatch() {
  const uint8_t dxbc[] = {0x44, 0x58, 0x42, 0x43, 0x01, 0x02, 0x03};

  CmdStreamWriter owned;
  owned.set_vector();
  if (!EmitRepresentativeCommands(owned, dxbc, sizeof(dxbc))) {
    return false;
  }

  std::vector<uint8_t> span_buf(4096, 0xCD);
  CmdStreamWriter borrowed;
  borrowed.set_span(span_buf.data(), span_buf.size());
  if (!EmitRepresentativeCommands(borrowed, dxbc, sizeof(dxbc))) {
    return false;
  }

  if (!Check(owned.bytes_used() == borrowed.bytes_used(), "owned and borrowed sizes match")) {
    return false;
  }
  if (!Check(std::memcmp(owned.data(), borrowed.data(), owned.bytes_used()) == 0, "owned and borrowed bytes match")) {
    return false;
  }

  return ValidateStream(borrowed.data(), span_buf.size()) && ValidateStream(owned.data(), owned.bytes_used());
}

bool TestEventQueryGetDataSemantics() {
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

  hr = cleanup.device_funcs.pfnClear(create_dev.hDevice,
                                     /*flags=*/0x1u,
                                     /*color_rgba8=*/0xFFFFFFFFu,
                                     /*depth=*/1.0f,
                                     /*stencil=*/0);
  if (!Check(hr == S_OK, "Clear")) {
    return false;
  }

  // D3DQUERYTYPE_EVENT = 8 (public D3D9 encoding). The UMD also accepts 0.
  D3D9DDIARG_CREATEQUERY create_query{};
  create_query.type = 8u;
  hr = cleanup.device_funcs.pfnCreateQuery(create_dev.hDevice, &create_query);
  if (!Check(hr == S_OK, "CreateQuery(EVENT)")) {
    return false;
  }
  if (!Check(create_query.hQuery.pDrvPrivate != nullptr, "CreateQuery returned query handle")) {
    return false;
  }
  cleanup.hQuery = create_query.hQuery;
  cleanup.has_query = true;
 
  auto* adapter = reinterpret_cast<Adapter*>(open.hAdapter.pDrvPrivate);
  auto* query = reinterpret_cast<Query*>(create_query.hQuery.pDrvPrivate);
  uint64_t base_render_submits = 0;
  uint64_t base_present_submits = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    base_render_submits = adapter->render_submit_count;
    base_present_submits = adapter->present_submit_count;
  }
 
  // Some D3D9Ex callers have been observed to pass 0 for END, so cover both the
  // explicit D3DISSUE_END bit and the 0-valued encoding.
  D3D9DDIARG_ISSUEQUERY issue{};
  issue.hQuery = create_query.hQuery;
  issue.flags = 0; // END (0 encoding)
  hr = cleanup.device_funcs.pfnIssueQuery(create_dev.hDevice, &issue);
  if (!Check(hr == S_OK, "IssueQuery(END=0)")) {
    return false;
  }
  // IssueQuery(END) should submit recorded work so fence-based tests can observe
  // a real submission (Win7: d3d9ex_submit_fence_stress). It must be classified
  // as a render submission (not present).
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (!Check(adapter->render_submit_count >= base_render_submits + 1,
               "IssueQuery(END) triggers at least one render submission")) {
      return false;
    }
    if (!Check(adapter->present_submit_count == base_present_submits,
               "IssueQuery(END) does not increment present submission count")) {
      return false;
    }
  }
 
  const uint64_t fence_value0 = query->fence_value.load(std::memory_order_acquire);
  if (!Check(fence_value0 != 0, "event query fence_value (END=0)")) {
    return false;
  }

  // Issue again with the explicit END bit so we lock in both paths.
  hr = cleanup.device_funcs.pfnClear(create_dev.hDevice,
                                     /*flags=*/0x1u,
                                     /*color_rgba8=*/0xFFFFFFFFu,
                                     /*depth=*/1.0f,
                                     /*stencil=*/0);
  if (!Check(hr == S_OK, "Clear (before IssueQuery(D3DISSUE_END))")) {
    return false;
  }

  issue.flags = 0x1u; // D3DISSUE_END
  hr = cleanup.device_funcs.pfnIssueQuery(create_dev.hDevice, &issue);
  if (!Check(hr == S_OK, "IssueQuery(D3DISSUE_END)")) {
    return false;
  }

  const uint64_t fence_value1 = query->fence_value.load(std::memory_order_acquire);
  if (!Check(fence_value1 >= fence_value0, "event query fence_value monotonic (END=1)")) {
    return false;
  }

  // Some DDI paths use 0x2 to mean END. Cover that encoding as well.
  hr = cleanup.device_funcs.pfnClear(create_dev.hDevice,
                                     /*flags=*/0x1u,
                                     /*color_rgba8=*/0xFFFFFFFFu,
                                     /*depth=*/1.0f,
                                     /*stencil=*/0);
  if (!Check(hr == S_OK, "Clear (before IssueQuery(END=2))")) {
    return false;
  }

  issue.flags = 0x2u;
  hr = cleanup.device_funcs.pfnIssueQuery(create_dev.hDevice, &issue);
  if (!Check(hr == S_OK, "IssueQuery(END=2)")) {
    return false;
  }

  const uint64_t fence_value = query->fence_value.load(std::memory_order_acquire);
  if (!Check(fence_value >= fence_value1, "event query fence_value monotonic (END=2)")) {
    return false;
  }

  // Force the query into the "not ready" state.
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    adapter->completed_fence = 0;
  }

  uint32_t done = 0;
  D3D9DDIARG_GETQUERYDATA get_data{};
  get_data.hQuery = create_query.hQuery;
  get_data.pData = &done;
  get_data.data_size = sizeof(done);
  get_data.flags = 0;

  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &get_data);
  if (!Check(hr == S_FALSE, "GetQueryData not-ready returns S_FALSE")) {
    return false;
  }

  // D3D9Ex clients (including DWM) often poll EVENT queries with D3DGETDATA_FLUSH
  // while other threads are concurrently submitting work. Ensure our GetQueryData
  // implementation does not block on the device mutex in that scenario.
  {
    auto* device = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
    if (!Check(device != nullptr, "device pointer")) {
      return false;
    }

    std::mutex state_mutex;
    std::condition_variable state_cv;
    bool started = false;
    bool finished = false;
    HRESULT thread_hr = E_FAIL;

    std::unique_lock<std::mutex> dev_lock(device->mutex);
    std::thread t([&] {
      {
        std::lock_guard<std::mutex> lk(state_mutex);
        started = true;
      }
      state_cv.notify_one();

      uint32_t thread_done = 0;
      D3D9DDIARG_GETQUERYDATA gd = get_data;
      gd.pData = &thread_done;
      gd.flags = 0x1u; // D3DGETDATA_FLUSH
      thread_hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &gd);

      {
        std::lock_guard<std::mutex> lk(state_mutex);
        finished = true;
      }
      state_cv.notify_one();
    });

    // Wait until the thread is actually running while still holding device->mutex.
    {
      std::unique_lock<std::mutex> lk(state_mutex);
       if (!state_cv.wait_for(lk, std::chrono::milliseconds(500), [&] { return started; })) {
         dev_lock.unlock();
         t.join();
         return Check(false, "GetQueryData(FLUSH) thread failed to start");
       }
       // Now ensure it finishes even though device->mutex is held.
       if (!state_cv.wait_for(lk, std::chrono::milliseconds(200), [&] { return finished; })) {
         // Avoid a deadlock: release the mutex so the thread can complete, then fail.
         dev_lock.unlock();
         t.join();
         return Check(false, "GetQueryData(FLUSH) blocked on device mutex");
       }
    }
    dev_lock.unlock();
    t.join();

    if (!Check(thread_hr == S_FALSE, "GetQueryData(FLUSH) under device mutex returns S_FALSE")) {
      return false;
    }
  }

  // D3D9 allows polling readiness without providing an output buffer.
  D3D9DDIARG_GETQUERYDATA get_no_data = get_data;
  get_no_data.pData = nullptr;
  get_no_data.data_size = 0;
  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &get_no_data);
  if (!Check(hr == S_FALSE, "GetQueryData (no buffer) not-ready returns S_FALSE")) {
    return false;
  }

  // Invalid pointer/size combinations should fail even if the query is not ready.
  D3D9DDIARG_GETQUERYDATA get_bad = get_data;
  get_bad.pData = nullptr;
  get_bad.data_size = sizeof(done);
  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &get_bad);
  if (!Check(hr == D3DERR_INVALIDCALL, "GetQueryData rejects null pData with non-zero size")) {
    return false;
  }

  get_bad.pData = &done;
  get_bad.data_size = 0;
  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &get_bad);
  if (!Check(hr == D3DERR_INVALIDCALL, "GetQueryData rejects non-null pData with zero size")) {
    return false;
  }

  uint16_t small = 0;
  get_bad.pData = &small;
  get_bad.data_size = sizeof(small);
  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &get_bad);
  if (!Check(hr == D3DERR_INVALIDCALL, "GetQueryData rejects undersized buffer")) {
    return false;
  }

  // Mark the fence complete and re-poll.
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    adapter->completed_fence = fence_value;
  }

  // The UMD may defer making an EVENT query "visible" to GetData(DONOTFLUSH)
  // until an explicit flush boundary is observed. Even if the fence is already
  // complete, the query should remain not-ready until a flush/submission
  // boundary arms it.
  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &get_no_data);
  if (!Check(hr == S_FALSE, "GetQueryData (no buffer) fence complete but unsubmitted returns S_FALSE")) {
    return false;
  }

  // GetData(FLUSH) should arm the query without blocking and then report
  // readiness based on the fence.
  get_no_data.flags = 0x1u; // D3DGETDATA_FLUSH
  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &get_no_data);
  if (!Check(hr == S_OK, "GetQueryData(FLUSH) (no buffer) ready returns S_OK")) {
    return false;
  }
  if (!Check(query->submitted.load(std::memory_order_acquire), "event query marked submitted after FLUSH")) {
    return false;
  }

  get_no_data.flags = 0;
  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &get_no_data);
  if (!Check(hr == S_OK, "GetQueryData (no buffer) ready returns S_OK after submit")) {
    return false;
  }

  done = 0;
  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &get_data);
  if (!Check(hr == S_OK, "GetQueryData ready returns S_OK")) {
    return false;
  }
  if (!Check(done != 0, "GetQueryData ready writes TRUE")) {
    return false;
  }

  // Validate argument checking for the D3D9 GetData contract: pData must be NULL
  // iff data_size is 0.
  D3D9DDIARG_GETQUERYDATA invalid_args = get_data;
  invalid_args.pData = &done;
  invalid_args.data_size = 0;
  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &invalid_args);
  if (!Check(hr == D3DERR_INVALIDCALL, "GetQueryData pData!=NULL but size==0 returns INVALIDCALL")) {
    return false;
  }

  invalid_args.pData = nullptr;
  invalid_args.data_size = sizeof(done);
  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &invalid_args);
  if (!Check(hr == D3DERR_INVALIDCALL, "GetQueryData pData==NULL but size!=0 returns INVALIDCALL")) {
    return false;
  }

  invalid_args.pData = nullptr;
  invalid_args.data_size = 0;
  hr = cleanup.device_funcs.pfnGetQueryData(create_dev.hDevice, &invalid_args);
  if (!Check(hr == S_OK, "GetQueryData pData==NULL and size==0 returns S_OK when ready")) {
    return false;
  }

  return true;
}

bool TestAdapterCapsAndQueryAdapterInfo() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    bool has_adapter = false;
    ~Cleanup() {
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

  if (!Check(cleanup.adapter_funcs.pfnGetCaps != nullptr, "pfnGetCaps is non-null")) {
    return false;
  }
  if (!Check(cleanup.adapter_funcs.pfnQueryAdapterInfo != nullptr, "pfnQueryAdapterInfo is non-null")) {
    return false;
  }

  D3DCAPS9 caps{};
  D3D9DDIARG_GETCAPS get_caps{};
  get_caps.Type = D3DDDICAPS_GETD3D9CAPS;
  get_caps.pData = &caps;
  get_caps.DataSize = sizeof(caps);
  hr = cleanup.adapter_funcs.pfnGetCaps(open.hAdapter, &get_caps);
  if (!Check(hr == S_OK, "GetCaps(GETD3D9CAPS)")) {
    return false;
  }
  if (!Check((caps.Caps2 & D3DCAPS2_CANRENDERWINDOWED) != 0, "Caps2 includes CANRENDERWINDOWED")) {
    return false;
  }
  if (!Check((caps.Caps2 & D3DCAPS2_CANSHARERESOURCE) != 0, "Caps2 includes CANSHARERESOURCE")) {
    return false;
  }
  if (!Check(caps.VertexShaderVersion >= D3DVS_VERSION(2, 0), "VertexShaderVersion >= 2.0")) {
    return false;
  }
  if (!Check(caps.PixelShaderVersion >= D3DPS_VERSION(2, 0), "PixelShaderVersion >= 2.0")) {
    return false;
  }

  uint32_t format_count = 0;
  D3D9DDIARG_GETCAPS get_fmt_count{};
  get_fmt_count.Type = D3DDDICAPS_GETFORMATCOUNT;
  get_fmt_count.pData = &format_count;
  get_fmt_count.DataSize = sizeof(format_count);
  hr = cleanup.adapter_funcs.pfnGetCaps(open.hAdapter, &get_fmt_count);
  if (!Check(hr == S_OK, "GetCaps(GETFORMATCOUNT)")) {
    return false;
  }
  if (!Check(format_count == 9, "format_count == 9")) {
    return false;
  }

  struct GetFormatPayload {
    uint32_t index;
    uint32_t format;
    uint32_t ops;
  };

  constexpr uint32_t kD3DUsageRenderTarget = 0x00000001u;
  constexpr uint32_t kD3DUsageDepthStencil = 0x00000002u;
  constexpr uint32_t kExpectedFormats[9] = {
      22u, // D3DFMT_X8R8G8B8
      21u, // D3DFMT_A8R8G8B8
      32u, // D3DFMT_A8B8G8R8
      75u, // D3DFMT_D24S8
      static_cast<uint32_t>(kD3dFmtDxt1), // D3DFMT_DXT1
      static_cast<uint32_t>(kD3dFmtDxt2), // D3DFMT_DXT2
      static_cast<uint32_t>(kD3dFmtDxt3), // D3DFMT_DXT3
      static_cast<uint32_t>(kD3dFmtDxt4), // D3DFMT_DXT4
      static_cast<uint32_t>(kD3dFmtDxt5), // D3DFMT_DXT5
  };

  for (uint32_t i = 0; i < format_count; ++i) {
    GetFormatPayload payload{};
    payload.index = i;
    payload.format = 0;
    payload.ops = 0;

    D3D9DDIARG_GETCAPS get_fmt{};
    get_fmt.Type = D3DDDICAPS_GETFORMAT;
    get_fmt.pData = &payload;
    get_fmt.DataSize = sizeof(payload);
    hr = cleanup.adapter_funcs.pfnGetCaps(open.hAdapter, &get_fmt);
    if (!Check(hr == S_OK, "GetCaps(GETFORMAT)")) {
      return false;
    }
    if (!Check(payload.format == kExpectedFormats[i], "format enumeration matches expected list")) {
      return false;
    }

    uint32_t expected_ops = (payload.format == 75u) ? kD3DUsageDepthStencil : kD3DUsageRenderTarget;
    if (payload.format == static_cast<uint32_t>(kD3dFmtDxt1) ||
        payload.format == static_cast<uint32_t>(kD3dFmtDxt2) ||
        payload.format == static_cast<uint32_t>(kD3dFmtDxt3) ||
        payload.format == static_cast<uint32_t>(kD3dFmtDxt4) ||
        payload.format == static_cast<uint32_t>(kD3dFmtDxt5)) {
      expected_ops = 0;
    }
    if (!Check(payload.ops == expected_ops, "format ops mask matches expected usage")) {
      return false;
    }
  }

  D3DADAPTER_IDENTIFIER9 ident{};
  D3D9DDIARG_QUERYADAPTERINFO query_ident{};
  query_ident.Type = D3DDDIQUERYADAPTERINFO_GETADAPTERIDENTIFIER;
  query_ident.pPrivateDriverData = &ident;
  query_ident.PrivateDriverDataSize = sizeof(ident);
  hr = cleanup.adapter_funcs.pfnQueryAdapterInfo(open.hAdapter, &query_ident);
  if (!Check(hr == S_OK, "QueryAdapterInfo(GETADAPTERIDENTIFIER)")) {
    return false;
  }
  if (!Check(ident.Driver[0] != '\0', "identifier Driver is non-empty")) {
    return false;
  }
  if (!Check(ident.VendorId == AEROGPU_PCI_VENDOR_ID, "identifier VendorId matches AeroGPU")) {
    return false;
  }
  if (!Check(ident.DeviceId == AEROGPU_PCI_DEVICE_ID, "identifier DeviceId matches AeroGPU")) {
    return false;
  }

  LUID luid{};
  D3D9DDIARG_QUERYADAPTERINFO query_luid{};
  query_luid.Type = D3DDDIQUERYADAPTERINFO_GETADAPTERLUID;
  query_luid.pPrivateDriverData = &luid;
  query_luid.PrivateDriverDataSize = sizeof(luid);
  hr = cleanup.adapter_funcs.pfnQueryAdapterInfo(open.hAdapter, &query_luid);
  if (!Check(hr == S_OK, "QueryAdapterInfo(GETADAPTERLUID)")) {
    return false;
  }
  return true;
}

bool TestAdapterMultisampleQualityLevels() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    bool has_adapter = false;
    ~Cleanup() {
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

  struct GetMultisampleQualityLevelsPayload {
    uint32_t format;
    uint32_t multisample_type;
    uint32_t flags;
    uint32_t quality_levels;
  };

  GetMultisampleQualityLevelsPayload payload{};
  payload.format = 22u; // D3DFMT_X8R8G8B8 (supported)
  payload.multisample_type = 0;
  payload.flags = 0;
  payload.quality_levels = 0;

  D3D9DDIARG_GETCAPS get_caps{};
  get_caps.Type = D3DDDICAPS_GETMULTISAMPLEQUALITYLEVELS;
  get_caps.pData = &payload;
  get_caps.DataSize = sizeof(payload);
  hr = cleanup.adapter_funcs.pfnGetCaps(open.hAdapter, &get_caps);
  if (!Check(hr == S_OK, "GetCaps(GETMULTISAMPLEQUALITYLEVELS)")) {
    return false;
  }
  if (!Check(payload.quality_levels == 1, "quality_levels==1 for NONE on supported format")) {
    return false;
  }

  payload.multisample_type = 1;
  payload.quality_levels = 0xCDCDCDCDu;
  hr = cleanup.adapter_funcs.pfnGetCaps(open.hAdapter, &get_caps);
  if (!Check(hr == S_OK, "GetCaps(GETMULTISAMPLEQUALITYLEVELS) non-zero type")) {
    return false;
  }
  if (!Check(payload.quality_levels == 0, "quality_levels==0 for non-zero multisample type")) {
    return false;
  }

  payload.format = 0xFFFFFFFFu;
  payload.multisample_type = 0;
  payload.quality_levels = 0xCDCDCDCDu;
  hr = cleanup.adapter_funcs.pfnGetCaps(open.hAdapter, &get_caps);
  if (!Check(hr == S_OK, "GetCaps(GETMULTISAMPLEQUALITYLEVELS) unsupported format")) {
    return false;
  }
  if (!Check(payload.quality_levels == 0, "quality_levels==0 for unsupported format")) {
    return false;
  }

  struct GetMultisampleQualityLevelsPayloadV1 {
    uint32_t format;
    uint32_t multisample_type;
    uint32_t quality_levels;
  };

  GetMultisampleQualityLevelsPayloadV1 payload_v1{};
  payload_v1.format = 21u; // D3DFMT_A8R8G8B8 (supported)
  payload_v1.multisample_type = 0;
  payload_v1.quality_levels = 0;

  get_caps.pData = &payload_v1;
  get_caps.DataSize = sizeof(payload_v1);
  hr = cleanup.adapter_funcs.pfnGetCaps(open.hAdapter, &get_caps);
  if (!Check(hr == S_OK, "GetCaps(GETMULTISAMPLEQUALITYLEVELS) v1 payload")) {
    return false;
  }
  return Check(payload_v1.quality_levels == 1, "quality_levels==1 for v1 payload");
}

bool TestAdapterCachingUpdatesCallbacks() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    bool has_adapter = false;
    ~Cleanup() {
      if (has_adapter && adapter_funcs.pfnCloseAdapter) {
        adapter_funcs.pfnCloseAdapter(hAdapter);
      }
    }
  } cleanup1, cleanup2;

  D3DDDIARG_OPENADAPTER2 open1{};
  open1.Interface = 1;
  open1.Version = 1;
  D3DDDI_ADAPTERCALLBACKS callbacks1{};
  D3DDDI_ADAPTERCALLBACKS2 callbacks1_2{};
  callbacks1.pfnDummy = reinterpret_cast<void*>(static_cast<uintptr_t>(0x11111111u));
  callbacks1_2.pfnDummy = reinterpret_cast<void*>(static_cast<uintptr_t>(0x22222222u));
  open1.pAdapterCallbacks = &callbacks1;
  open1.pAdapterCallbacks2 = &callbacks1_2;
  open1.pAdapterFuncs = &cleanup1.adapter_funcs;

  HRESULT hr = ::OpenAdapter2(&open1);
  if (!Check(hr == S_OK, "OpenAdapter2 (first)")) {
    return false;
  }
  if (!Check(open1.hAdapter.pDrvPrivate != nullptr, "OpenAdapter2 (first) returned adapter handle")) {
    return false;
  }
  cleanup1.hAdapter = open1.hAdapter;
  cleanup1.has_adapter = true;

  auto* adapter = reinterpret_cast<Adapter*>(open1.hAdapter.pDrvPrivate);
  if (!Check(adapter != nullptr, "adapter pointer")) {
    return false;
  }

  const LUID luid = adapter->luid;

  if (!Check(adapter->adapter_callbacks_valid, "adapter_callbacks_valid after first open")) {
    return false;
  }
  if (!Check(adapter->adapter_callbacks2_valid, "adapter_callbacks2_valid after first open")) {
    return false;
  }
  if (!Check(adapter->adapter_callbacks_copy.pfnDummy == callbacks1.pfnDummy, "adapter_callbacks_copy matches first")) {
    return false;
  }
  if (!Check(adapter->adapter_callbacks2_copy.pfnDummy == callbacks1_2.pfnDummy, "adapter_callbacks2_copy matches first")) {
    return false;
  }

  D3DDDIARG_OPENADAPTERFROMLUID open2{};
  open2.Interface = 1;
  open2.Version = 1;
  open2.AdapterLuid = luid;
  D3DDDI_ADAPTERCALLBACKS callbacks2{};
  D3DDDI_ADAPTERCALLBACKS2 callbacks2_2{};
  callbacks2.pfnDummy = reinterpret_cast<void*>(static_cast<uintptr_t>(0x33333333u));
  callbacks2_2.pfnDummy = reinterpret_cast<void*>(static_cast<uintptr_t>(0x44444444u));
  open2.pAdapterCallbacks = &callbacks2;
  open2.pAdapterCallbacks2 = &callbacks2_2;
  open2.pAdapterFuncs = &cleanup2.adapter_funcs;

  hr = ::OpenAdapterFromLuid(&open2);
  if (!Check(hr == S_OK, "OpenAdapterFromLuid (second)")) {
    return false;
  }
  if (!Check(open2.hAdapter.pDrvPrivate != nullptr, "OpenAdapterFromLuid returned adapter handle")) {
    return false;
  }
  cleanup2.hAdapter = open2.hAdapter;
  cleanup2.has_adapter = true;

  if (!Check(open2.hAdapter.pDrvPrivate == open1.hAdapter.pDrvPrivate, "adapter cached across opens")) {
    return false;
  }

  if (!Check(adapter->adapter_callbacks_copy.pfnDummy == callbacks2.pfnDummy, "adapter_callbacks_copy updated on re-open")) {
    return false;
  }
  return Check(adapter->adapter_callbacks2_copy.pfnDummy == callbacks2_2.pfnDummy,
               "adapter_callbacks2_copy updated on re-open");
}

bool TestCreateResourceRejectsUnsupportedGpuFormat() {
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
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnCreateResource != nullptr, "CreateResource must be available")) {
    return false;
  }

  // Use an obviously invalid D3D9 format value to ensure the UMD rejects unknown
  // GPU formats in the default pool (rather than emitting invalid host commands).
  D3D9DDIARG_CREATERESOURCE create_res{};
  create_res.type = 0;
  create_res.format = 0xFFFFFFFFu;
  create_res.width = 4;
  create_res.height = 4;
  create_res.depth = 1;
  create_res.mip_levels = 1;
  create_res.usage = 0;
  create_res.pool = 0;
  create_res.size = 0;
  create_res.hResource.pDrvPrivate = nullptr;
  create_res.pSharedHandle = nullptr;
  create_res.pPrivateDriverData = nullptr;
  create_res.PrivateDriverDataSize = 0;
  create_res.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_res);
  if (!Check(hr == D3DERR_INVALIDCALL, "CreateResource rejects unsupported GPU format")) {
    return false;
  }
  return Check(create_res.hResource.pDrvPrivate == nullptr, "CreateResource failure does not return a handle");
}

bool TestCreateResourceComputesBcTexturePitchAndSize() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE hResource{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_resource = false;

    ~Cleanup() {
      if (has_resource && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hResource);
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
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Bind a span-backed command buffer so we can validate CREATE_TEXTURE2D output.
  std::vector<uint8_t> dma(4096, 0);
  dev->cmd.set_span(dma.data(), dma.size());
  dev->cmd.reset();

  D3D9DDIARG_CREATERESOURCE create_res{};
  create_res.type = 0;
  create_res.format = static_cast<uint32_t>(kD3dFmtDxt1); // D3DFMT_DXT1 (BC1)
  create_res.width = 7;
  create_res.height = 5;
  create_res.depth = 1;
  create_res.mip_levels = 3;
  create_res.usage = 0;
  create_res.pool = 0; // default pool (GPU resource)
  create_res.size = 0;
  create_res.hResource.pDrvPrivate = nullptr;
  create_res.pSharedHandle = nullptr;
  create_res.pPrivateDriverData = nullptr;
  create_res.PrivateDriverDataSize = 0;
  create_res.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_res);
  if (!Check(hr == S_OK, "CreateResource(DXT1)")) {
    return false;
  }
  cleanup.hResource = create_res.hResource;
  cleanup.has_resource = true;

  auto* res = reinterpret_cast<Resource*>(create_res.hResource.pDrvPrivate);
  if (!Check(res != nullptr, "resource pointer")) {
    return false;
  }

  // DXT1/BC1: 4x4 blocks, 8 bytes per block.
  // width=7,height=5 => blocks_w=2, blocks_h=2 => row_pitch=16, slice_pitch=32.
  // mip chain:
  //  - 7x5 => 32 bytes
  //  - 3x2 =>  8 bytes
  //  - 1x1 =>  8 bytes
  // total = 48 bytes.
  if (!Check(res->row_pitch == 16u, "DXT1 row_pitch bytes")) {
    return false;
  }
  if (!Check(res->slice_pitch == 32u, "DXT1 slice_pitch bytes")) {
    return false;
  }
  if (!Check(res->size_bytes == 48u, "DXT1 mip chain size_bytes")) {
    return false;
  }

  dev->cmd.finalize();
  if (!Check(ValidateStream(dma.data(), dma.size()), "stream validates")) {
    return false;
  }

  const CmdLoc create_loc = FindLastOpcode(dma.data(), dma.size(), AEROGPU_CMD_CREATE_TEXTURE2D);
  if (!Check(create_loc.hdr != nullptr, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_create_texture2d*>(create_loc.hdr);
  if (!Check(cmd->format == AEROGPU_FORMAT_BC1_RGBA_UNORM, "CREATE_TEXTURE2D format==BC1")) {
    return false;
  }
  if (!Check(cmd->row_pitch_bytes == 16u, "CREATE_TEXTURE2D row_pitch_bytes")) {
    return false;
  }
  return Check(cmd->mip_levels == 3u, "CREATE_TEXTURE2D mip_levels");
}

bool TestCreateResourceIgnoresStaleAllocPrivDataForNonShared() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE hResource{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_resource = false;

    ~Cleanup() {
      if (has_resource && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hResource);
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
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnCreateResource != nullptr, "CreateResource must be available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  std::vector<uint8_t> dma(4096, 0);
  dev->cmd.set_span(dma.data(), dma.size());

  // Simulate stale output-buffer contents: prior to
  // `fix(aerogpu-d3d9): avoid consuming uninitialized alloc privdata` the driver
  // would incorrectly consume these bytes and treat the resource as shared even
  // though the runtime did not request sharing.
  aerogpu_wddm_alloc_priv stale{};
  stale.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
  stale.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
  stale.alloc_id = 0x4242u;
  stale.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;
  stale.share_token = 0x1122334455667788ull;
  stale.size_bytes = 0x1000u;

  D3D9DDIARG_CREATERESOURCE create_res{};
  create_res.type = 0;
  create_res.format = 22u; // D3DFMT_X8R8G8B8
  create_res.width = 32;
  create_res.height = 32;
  create_res.depth = 1;
  create_res.mip_levels = 1;
  create_res.usage = 0x00000001u; // D3DUSAGE_RENDERTARGET
  create_res.pool = 0;
  create_res.size = 0;
  create_res.hResource.pDrvPrivate = nullptr;
  create_res.pSharedHandle = nullptr; // not a shared resource
  create_res.pKmdAllocPrivateData = &stale;
  create_res.KmdAllocPrivateDataSize = sizeof(stale);
  create_res.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_res);
  if (!Check(hr == S_OK, "CreateResource(non-shared)")) {
    return false;
  }
  cleanup.hResource = create_res.hResource;
  cleanup.has_resource = true;

  auto* res = reinterpret_cast<Resource*>(create_res.hResource.pDrvPrivate);
  if (!Check(res != nullptr, "resource pointer")) {
    return false;
  }
  if (!Check(!res->is_shared, "non-shared CreateResource does not become is_shared via stale privdata")) {
    return false;
  }
  if (!Check(res->share_token == 0, "non-shared CreateResource does not inherit share_token via stale privdata")) {
    return false;
  }

  dev->cmd.finalize();
  if (!Check(ValidateStream(dma.data(), dma.size()), "stream validates")) {
    return false;
  }
  if (!Check(CountOpcode(dma.data(), dma.size(), AEROGPU_CMD_EXPORT_SHARED_SURFACE) == 0,
             "non-shared CreateResource does not emit EXPORT_SHARED_SURFACE")) {
    return false;
  }

  // Make cleanup safe: switch back to vector mode so subsequent destroy calls
  // can't fail due to span-buffer capacity constraints.
  dev->cmd.set_vector();
  return true;
}

bool TestCreateResourceAllowsNullPrivateDataWhenNotAllocBacked() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE hResource{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_resource = false;

    ~Cleanup() {
      if (has_resource && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hResource);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Simulate WDDM-enabled mode but do NOT supply a WDDM allocation handle. The
  // driver should fall back to host-allocated resources and must not require a
  // runtime private-driver-data buffer in this case.
  dev->wddm_context.hContext = 1;
  D3DDDI_ALLOCATIONLIST list[4] = {};
  dev->alloc_list_tracker.rebind(list, 4, 0xFFFFu);

  D3D9DDIARG_CREATERESOURCE create_res{};
  create_res.type = 0;
  create_res.format = 22u; // D3DFMT_X8R8G8B8
  create_res.width = 16;
  create_res.height = 16;
  create_res.depth = 1;
  create_res.mip_levels = 1;
  create_res.usage = 0;
  create_res.pool = 0;
  create_res.size = 0;
  create_res.hResource.pDrvPrivate = nullptr;
  create_res.pSharedHandle = nullptr;
  create_res.pPrivateDriverData = nullptr;
  create_res.PrivateDriverDataSize = 0;
  create_res.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_res);
  if (!Check(hr == S_OK, "CreateResource(no privdata, no hAllocation)")) {
    return false;
  }
  if (!Check(create_res.hResource.pDrvPrivate != nullptr, "CreateResource returned resource handle")) {
    return false;
  }
  cleanup.hResource = create_res.hResource;
  cleanup.has_resource = true;

  auto* res = reinterpret_cast<Resource*>(create_res.hResource.pDrvPrivate);
  if (!Check(res != nullptr, "resource pointer")) {
    return false;
  }
  if (!Check(res->wddm_hAllocation == 0, "resource remains non-alloc-backed")) {
    return false;
  }
  return Check(res->backing_alloc_id == 0, "resource remains host-allocated (alloc_id == 0)");
}

bool TestAllocBackedUnlockEmitsDirtyRange() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE hResource{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_resource = false;

    ~Cleanup() {
      if (has_resource && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hResource);
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
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnCreateResource != nullptr, "CreateResource must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnLock != nullptr && cleanup.device_funcs.pfnUnlock != nullptr, "Lock/Unlock must be available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Simulate a WDDM-enabled device so allocation-list tracking and alloc-backed
  // dirty-range updates are enabled in portable builds.
  dev->wddm_context.hContext = 1;
  D3DDDI_ALLOCATIONLIST list[4] = {};
  dev->alloc_list_tracker.rebind(list, 4, 0xFFFFu);

  aerogpu_wddm_alloc_priv priv{};
  std::memset(&priv, 0, sizeof(priv));

  D3D9DDIARG_CREATERESOURCE create_res{};
  create_res.type = 6u; // D3DRTYPE_VERTEXBUFFER
  create_res.format = 0;
  create_res.width = 0;
  create_res.height = 0;
  create_res.depth = 1;
  create_res.mip_levels = 1;
  create_res.usage = 0;
  create_res.pool = 0;
  create_res.size = 64;
  create_res.hResource.pDrvPrivate = nullptr;
  create_res.pSharedHandle = nullptr;
  create_res.pKmdAllocPrivateData = &priv;
  create_res.KmdAllocPrivateDataSize = sizeof(priv);
  create_res.wddm_hAllocation = 0xABCDu;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_res);
  if (!Check(hr == S_OK, "CreateResource(alloc-backed VB)")) {
    return false;
  }
  if (!Check(create_res.hResource.pDrvPrivate != nullptr, "CreateResource returned resource handle")) {
    return false;
  }
  cleanup.hResource = create_res.hResource;
  cleanup.has_resource = true;

  auto* res = reinterpret_cast<Resource*>(create_res.hResource.pDrvPrivate);
  if (!Check(res != nullptr, "resource pointer")) {
    return false;
  }
  if (!Check(res->backing_alloc_id != 0, "alloc-backed resource backing_alloc_id non-zero")) {
    return false;
  }
  if (!Check(res->wddm_hAllocation == create_res.wddm_hAllocation, "resource preserves WDDM hAllocation")) {
    return false;
  }

  // Portable builds don't have a WDDM lock callback; resize CPU shadow storage
  // so Lock/Unlock can proceed while still exercising the alloc-backed update path.
  if (res->storage.size() < res->size_bytes) {
    res->storage.resize(res->size_bytes);
  }

  constexpr uint32_t kOffset = 4;
  constexpr uint32_t kSize = 16;

  D3D9DDIARG_LOCK lock{};
  lock.hResource = create_res.hResource;
  lock.offset_bytes = kOffset;
  lock.size_bytes = kSize;
  lock.flags = 0;
  D3DDDI_LOCKEDBOX box{};
  hr = cleanup.device_funcs.pfnLock(create_dev.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(alloc-backed VB)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock returns pData")) {
    return false;
  }
  std::memset(box.pData, 0xCD, kSize);

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = create_res.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(create_dev.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(alloc-backed VB)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(FindLastOpcode(buf, len, AEROGPU_CMD_CREATE_BUFFER).hdr != nullptr, "CREATE_BUFFER emitted")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0,
             "alloc-backed Unlock does not emit UPLOAD_RESOURCE")) {
    return false;
  }

  const CmdLoc dirty = FindLastOpcode(buf, len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty.hdr != nullptr, "RESOURCE_DIRTY_RANGE emitted")) {
    return false;
  }
  const auto* dirty_cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(dirty.hdr);
  if (!Check(dirty_cmd->resource_handle == res->handle, "RESOURCE_DIRTY_RANGE resource_handle")) {
    return false;
  }
  if (!Check(dirty_cmd->offset_bytes == kOffset, "RESOURCE_DIRTY_RANGE offset")) {
    return false;
  }
  if (!Check(dirty_cmd->size_bytes == kSize, "RESOURCE_DIRTY_RANGE size")) {
    return false;
  }

  if (!Check(dev->alloc_list_tracker.list_len() == 1, "allocation list has 1 entry")) {
    return false;
  }
  if (!Check(list[0].hAllocation == create_res.wddm_hAllocation, "allocation list carries hAllocation")) {
    return false;
  }
  if (!Check(list[0].WriteOperation == 0, "allocation list entry remains read-only for buffer CPU write")) {
    return false;
  }
  return Check(list[0].AllocationListSlotId == 0, "allocation list slot id == 0");
}

bool TestSharedResourceCreateAndOpenEmitsExportImport() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE hResource{};
    D3DDDI_HRESOURCE hAlias{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_resource = false;
    bool has_alias = false;

    ~Cleanup() {
      if (has_alias && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hAlias);
      }
      if (has_resource && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hResource);
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
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnCreateResource != nullptr, "CreateResource must be available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Use a span-backed buffer so we can inspect the exact packets emitted for
  // shared-surface create/open. Note: CreateResource(shared) forces an immediate
  // submission to make the EXPORT visible to other processes; that resets the
  // stream header but leaves the packet bytes intact in the span buffer.
  std::vector<uint8_t> dma(4096, 0);
  dev->cmd.set_span(dma.data(), dma.size());

  aerogpu_wddm_alloc_priv priv{};
  std::memset(&priv, 0, sizeof(priv));
  HANDLE shared_handle = nullptr;

  D3D9DDIARG_CREATERESOURCE create_shared{};
  create_shared.type = 0;
  create_shared.format = 22u; // D3DFMT_X8R8G8B8
  create_shared.width = 32;
  create_shared.height = 32;
  create_shared.depth = 1;
  create_shared.mip_levels = 1;
  create_shared.usage = 0x00000001u; // D3DUSAGE_RENDERTARGET
  create_shared.pool = 0;
  create_shared.size = 0;
  create_shared.hResource.pDrvPrivate = nullptr;
  create_shared.pSharedHandle = &shared_handle;
  create_shared.pKmdAllocPrivateData = &priv;
  create_shared.KmdAllocPrivateDataSize = sizeof(priv);
  create_shared.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_shared);
  if (!Check(hr == S_OK, "CreateResource(shared)")) {
    return false;
  }
  cleanup.hResource = create_shared.hResource;
  cleanup.has_resource = true;

  auto* res = reinterpret_cast<Resource*>(create_shared.hResource.pDrvPrivate);
  if (!Check(res != nullptr, "shared resource pointer")) {
    return false;
  }
  if (!Check(res->is_shared, "resource is_shared")) {
    return false;
  }
  if (!Check(!res->is_shared_alias, "shared create is not an alias")) {
    return false;
  }
  if (!Check(res->share_token != 0, "shared resource share_token non-zero")) {
    return false;
  }
  if (!Check(res->backing_alloc_id != 0, "shared resource backing_alloc_id non-zero")) {
    return false;
  }

  if (!Check(priv.magic == AEROGPU_WDDM_ALLOC_PRIV_MAGIC, "alloc priv magic")) {
    return false;
  }
  if (!Check(priv.version == AEROGPU_WDDM_ALLOC_PRIV_VERSION, "alloc priv version")) {
    return false;
  }
  if (!Check((priv.flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED) != 0, "alloc priv shared flag")) {
    return false;
  }
  if (!Check(priv.alloc_id == res->backing_alloc_id, "alloc priv alloc_id matches resource")) {
    return false;
  }
  if (!Check(priv.share_token == res->share_token, "alloc priv share_token matches resource")) {
    return false;
  }
  if (!Check(priv.size_bytes != 0, "alloc priv size_bytes non-zero")) {
    return false;
  }
  if (!Check(AEROGPU_WDDM_ALLOC_PRIV_DESC_PRESENT(priv.reserved0), "alloc priv desc present")) {
    return false;
  }
  if (!Check(AEROGPU_WDDM_ALLOC_PRIV_DESC_FORMAT(priv.reserved0) == create_shared.format, "alloc priv desc format")) {
    return false;
  }
  if (!Check(AEROGPU_WDDM_ALLOC_PRIV_DESC_WIDTH(priv.reserved0) == create_shared.width, "alloc priv desc width")) {
    return false;
  }
  if (!Check(AEROGPU_WDDM_ALLOC_PRIV_DESC_HEIGHT(priv.reserved0) == create_shared.height, "alloc priv desc height")) {
    return false;
  }

  // The shared create path should emit CREATE_TEXTURE2D + EXPORT_SHARED_SURFACE.
  if (!Check(CountOpcode(dma.data(), dma.size(), AEROGPU_CMD_CREATE_TEXTURE2D) == 1, "CREATE_TEXTURE2D emitted")) {
    return false;
  }
  if (!Check(CountOpcode(dma.data(), dma.size(), AEROGPU_CMD_EXPORT_SHARED_SURFACE) == 1, "EXPORT_SHARED_SURFACE emitted")) {
    return false;
  }
  const CmdLoc export_loc = FindLastOpcode(dma.data(), dma.size(), AEROGPU_CMD_EXPORT_SHARED_SURFACE);
  if (!Check(export_loc.hdr != nullptr, "EXPORT_SHARED_SURFACE packet present")) {
    return false;
  }
  const auto* export_cmd = reinterpret_cast<const aerogpu_cmd_export_shared_surface*>(export_loc.hdr);
  if (!Check(export_cmd->resource_handle == res->handle, "EXPORT_SHARED_SURFACE resource_handle matches")) {
    return false;
  }
  if (!Check(export_cmd->share_token == res->share_token, "EXPORT_SHARED_SURFACE share_token matches")) {
    return false;
  }

  // Now simulate opening the shared resource in another process: caller passes a
  // non-null shared handle value plus the preserved allocation private data blob.
  std::memset(dma.data(), 0, dma.size());
  dev->cmd.set_span(dma.data(), dma.size());

  // Accept both v1 and v2 allocation private data blobs (the KMD may return v2
  // when the caller provided a large-enough buffer).
  aerogpu_wddm_alloc_priv_v2 priv_open{};
  std::memset(&priv_open, 0, sizeof(priv_open));
  std::memcpy(&priv_open, &priv, sizeof(priv));
  priv_open.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION_2;
  HANDLE open_handle = reinterpret_cast<HANDLE>(static_cast<uintptr_t>(0x1));

  D3D9DDIARG_CREATERESOURCE open_shared{};
  open_shared.type = create_shared.type;
  open_shared.format = create_shared.format;
  open_shared.width = create_shared.width;
  open_shared.height = create_shared.height;
  open_shared.depth = create_shared.depth;
  open_shared.mip_levels = create_shared.mip_levels;
  open_shared.usage = create_shared.usage;
  open_shared.pool = create_shared.pool;
  open_shared.size = 0;
  open_shared.hResource.pDrvPrivate = nullptr;
  open_shared.pSharedHandle = &open_handle;
  open_shared.pKmdAllocPrivateData = &priv_open;
  open_shared.KmdAllocPrivateDataSize = sizeof(priv_open);
  open_shared.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &open_shared);
  if (!Check(hr == S_OK, "CreateResource(open shared)")) {
    return false;
  }
  cleanup.hAlias = open_shared.hResource;
  cleanup.has_alias = true;

  auto* alias = reinterpret_cast<Resource*>(open_shared.hResource.pDrvPrivate);
  if (!Check(alias != nullptr, "alias resource pointer")) {
    return false;
  }
  if (!Check(alias->is_shared, "alias is_shared")) {
    return false;
  }
  if (!Check(alias->is_shared_alias, "alias is_shared_alias")) {
    return false;
  }
  if (!Check(alias->share_token == res->share_token, "alias share_token matches original")) {
    return false;
  }
  if (!Check(alias->backing_alloc_id == res->backing_alloc_id, "alias backing_alloc_id matches original")) {
    return false;
  }

  dev->cmd.finalize();
  if (!Check(ValidateStream(dma.data(), dma.size()), "import stream validates")) {
    return false;
  }
  if (!Check(CountOpcode(dma.data(), dma.size(), AEROGPU_CMD_IMPORT_SHARED_SURFACE) == 1, "IMPORT_SHARED_SURFACE emitted")) {
    return false;
  }
  if (!Check(CountOpcode(dma.data(), dma.size(), AEROGPU_CMD_CREATE_TEXTURE2D) == 0, "open shared does not CREATE_TEXTURE2D")) {
    return false;
  }

  const CmdLoc import_loc = FindLastOpcode(dma.data(), dma.size(), AEROGPU_CMD_IMPORT_SHARED_SURFACE);
  if (!Check(import_loc.hdr != nullptr, "IMPORT_SHARED_SURFACE packet present")) {
    return false;
  }
  const auto* import_cmd = reinterpret_cast<const aerogpu_cmd_import_shared_surface*>(import_loc.hdr);
  if (!Check(import_cmd->out_resource_handle == alias->handle, "IMPORT_SHARED_SURFACE out_resource_handle matches")) {
    return false;
  }
  if (!Check(import_cmd->share_token == alias->share_token, "IMPORT_SHARED_SURFACE share_token matches")) {
    return false;
  }

  const aerogpu_handle_t original_handle = res->handle;
  const aerogpu_handle_t alias_handle = alias->handle;

  // Validate that DestroyResource emits DESTROY_RESOURCE even for shared surfaces.
  auto check_destroy_stream = [&](aerogpu_handle_t expected_handle, const char* which) -> bool {
    dev->cmd.finalize();
    if (!Check(ValidateStream(dma.data(), dma.size()), which)) {
      return false;
    }
    if (!Check(CountOpcode(dma.data(), dma.size(), AEROGPU_CMD_DESTROY_RESOURCE) >= 1, which)) {
      return false;
    }
    const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(dma.data());
    size_t offset = sizeof(aerogpu_cmd_stream_header);
    while (offset + sizeof(aerogpu_cmd_hdr) <= stream->size_bytes) {
      const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(dma.data() + offset);
      if (hdr->opcode == AEROGPU_CMD_DESTROY_RESOURCE) {
        const auto* cmd = reinterpret_cast<const aerogpu_cmd_destroy_resource*>(hdr);
        if (cmd->resource_handle == expected_handle) {
          return true;
        }
      }
      if (hdr->size_bytes == 0 || hdr->size_bytes > stream->size_bytes - offset) {
        break;
      }
      offset += hdr->size_bytes;
    }
    std::fprintf(stderr, "FAIL: %s missing expected handle %u\n", which, static_cast<unsigned>(expected_handle));
    return false;
  };

  std::memset(dma.data(), 0, dma.size());
  dev->cmd.set_span(dma.data(), dma.size());
  if (cleanup.device_funcs.pfnDestroyResource) {
    cleanup.device_funcs.pfnDestroyResource(create_dev.hDevice, cleanup.hAlias);
    cleanup.has_alias = false;
  }
  if (!check_destroy_stream(alias_handle, "DestroyResource(alias) emits DESTROY_RESOURCE")) {
    dev->cmd.set_vector();
    return false;
  }

  std::memset(dma.data(), 0, dma.size());
  dev->cmd.set_span(dma.data(), dma.size());
  if (cleanup.device_funcs.pfnDestroyResource) {
    cleanup.device_funcs.pfnDestroyResource(create_dev.hDevice, cleanup.hResource);
    cleanup.has_resource = false;
  }
  if (!check_destroy_stream(original_handle, "DestroyResource(original) emits DESTROY_RESOURCE")) {
    dev->cmd.set_vector();
    return false;
  }

  // Make cleanup safe: switch back to vector mode so subsequent destroy calls
  // can't fail due to span-buffer capacity constraints.
  dev->cmd.set_vector();
  return true;
}

bool TestPresentStatsAndFrameLatency() {
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
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnPresentEx != nullptr, "PresentEx must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnGetPresentStats != nullptr, "GetPresentStats must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnGetLastPresentCount != nullptr, "GetLastPresentCount must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetMaximumFrameLatency != nullptr, "SetMaximumFrameLatency must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnGetMaximumFrameLatency != nullptr, "GetMaximumFrameLatency must be available")) {
    return false;
  }

  D3D9DDI_PRESENTSTATS stats{};
  hr = cleanup.device_funcs.pfnGetPresentStats(create_dev.hDevice, &stats);
  if (!Check(hr == S_OK, "GetPresentStats initial")) {
    return false;
  }
  if (!Check(stats.PresentCount == 0, "PresentCount initial == 0")) {
    return false;
  }

  uint32_t last_present = 123;
  hr = cleanup.device_funcs.pfnGetLastPresentCount(create_dev.hDevice, &last_present);
  if (!Check(hr == S_OK, "GetLastPresentCount initial")) {
    return false;
  }
  if (!Check(last_present == 0, "LastPresentCount initial == 0")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetMaximumFrameLatency(create_dev.hDevice, 0);
  if (!Check(hr == E_INVALIDARG, "SetMaximumFrameLatency rejects 0")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetMaximumFrameLatency(create_dev.hDevice, 1);
  if (!Check(hr == S_OK, "SetMaximumFrameLatency(1)")) {
    return false;
  }

  uint32_t max_latency = 0;
  hr = cleanup.device_funcs.pfnGetMaximumFrameLatency(create_dev.hDevice, &max_latency);
  if (!Check(hr == S_OK, "GetMaximumFrameLatency")) {
    return false;
  }
  if (!Check(max_latency == 1, "GetMaximumFrameLatency returns 1")) {
    return false;
  }

  D3D9DDIARG_PRESENTEX present{};
  present.hSrc.pDrvPrivate = nullptr;
  present.hWnd = nullptr;
  present.sync_interval = 1;
  present.d3d9_present_flags = 0;
  hr = cleanup.device_funcs.pfnPresentEx(create_dev.hDevice, &present);
  if (!Check(hr == S_OK, "PresentEx first")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnGetPresentStats(create_dev.hDevice, &stats);
  if (!Check(hr == S_OK, "GetPresentStats after PresentEx")) {
    return false;
  }
  if (!Check(stats.PresentCount == 1, "PresentCount == 1 after PresentEx")) {
    return false;
  }
  if (!Check(stats.PresentRefreshCount == 1, "PresentRefreshCount == 1 after PresentEx")) {
    return false;
  }
  if (!Check(stats.SyncRefreshCount == 1, "SyncRefreshCount == 1 after PresentEx")) {
    return false;
  }
  if (!Check(stats.SyncQPCTime != 0, "SyncQPCTime non-zero after PresentEx")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnGetLastPresentCount(create_dev.hDevice, &last_present);
  if (!Check(hr == S_OK, "GetLastPresentCount after PresentEx")) {
    return false;
  }
  if (!Check(last_present == 1, "LastPresentCount == 1 after PresentEx")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* adapter = reinterpret_cast<Adapter*>(open.hAdapter.pDrvPrivate);
  if (!Check(dev != nullptr && adapter != nullptr, "device/adapter pointers")) {
    return false;
  }

  uint64_t first_present_fence = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->inflight_present_fences.size() == 1, "inflight_present_fences contains one fence")) {
      return false;
    }
    first_present_fence = dev->inflight_present_fences.front();
  }
  if (!Check(first_present_fence != 0, "present fence value")) {
    return false;
  }

  // Force the present fence into the "not completed" state so we can validate
  // D3DPRESENT_DONOTWAIT throttling.
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    adapter->completed_fence = 0;
  }

  present.d3d9_present_flags = 0x1u; // D3DPRESENT_DONOTWAIT
  hr = cleanup.device_funcs.pfnPresentEx(create_dev.hDevice, &present);
  if (!Check(hr == D3DERR_WASSTILLDRAWING, "PresentEx DONOTWAIT returns WASSTILLDRAWING when throttled")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnGetLastPresentCount(create_dev.hDevice, &last_present);
  if (!Check(hr == S_OK, "GetLastPresentCount after throttled PresentEx")) {
    return false;
  }
  if (!Check(last_present == 1, "LastPresentCount unchanged after throttled PresentEx")) {
    return false;
  }

  // Mark the fence complete and confirm presents proceed again.
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    adapter->completed_fence = first_present_fence;
  }

  present.d3d9_present_flags = 0;
  hr = cleanup.device_funcs.pfnPresentEx(create_dev.hDevice, &present);
  if (!Check(hr == S_OK, "PresentEx after fence completion")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnGetPresentStats(create_dev.hDevice, &stats);
  if (!Check(hr == S_OK, "GetPresentStats after second PresentEx")) {
    return false;
  }
  if (!Check(stats.PresentCount == 2, "PresentCount == 2 after second PresentEx")) {
    return false;
  }
  return true;
}

bool TestPresentExSubmitsOnceWhenNoPendingRenderWork() {
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
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnPresentEx != nullptr, "PresentEx must be available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* adapter = reinterpret_cast<Adapter*>(open.hAdapter.pDrvPrivate);
  if (!Check(dev != nullptr && adapter != nullptr, "device/adapter pointers")) {
    return false;
  }

  uint64_t base_fence = 0;
  uint64_t base_render_submits = 0;
  uint64_t base_present_submits = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    base_fence = adapter->last_submitted_fence;
    base_render_submits = adapter->render_submit_count;
    base_present_submits = adapter->present_submit_count;
  }

  D3D9DDIARG_PRESENTEX present{};
  present.hSrc.pDrvPrivate = nullptr;
  present.hWnd = nullptr;
  present.sync_interval = 1;
  present.d3d9_present_flags = 0;
  hr = cleanup.device_funcs.pfnPresentEx(create_dev.hDevice, &present);
  if (!Check(hr == S_OK, "PresentEx")) {
    return false;
  }

  uint64_t final_fence = 0;
  uint64_t final_render_submits = 0;
  uint64_t final_present_submits = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    final_fence = adapter->last_submitted_fence;
    final_render_submits = adapter->render_submit_count;
    final_present_submits = adapter->present_submit_count;
  }
  if (!Check(final_fence == base_fence + 1, "PresentEx submits exactly once when no render work is pending")) {
    return false;
  }
  if (!Check(final_render_submits == base_render_submits, "PresentEx (idle) does not issue a render submit")) {
    return false;
  }
  if (!Check(final_present_submits == base_present_submits + 1, "PresentEx (idle) issues exactly one present submit")) {
    return false;
  }

  uint64_t present_fence = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->inflight_present_fences.size() == 1, "inflight_present_fences contains one fence")) {
      return false;
    }
    present_fence = dev->inflight_present_fences.front();
  }
  return Check(present_fence == base_fence + 1, "present fence matches single submission");
}

bool TestPresentSubmitsOnceWhenNoPendingRenderWork() {
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
  cleanup.hAdapter = open.hAdapter;
  cleanup.has_adapter = true;

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnPresent != nullptr, "Present must be available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* adapter = reinterpret_cast<Adapter*>(open.hAdapter.pDrvPrivate);
  if (!Check(dev != nullptr && adapter != nullptr, "device/adapter pointers")) {
    return false;
  }

  uint64_t base_fence = 0;
  uint64_t base_render_submits = 0;
  uint64_t base_present_submits = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    base_fence = adapter->last_submitted_fence;
    base_render_submits = adapter->render_submit_count;
    base_present_submits = adapter->present_submit_count;
  }

  D3D9DDIARG_PRESENT present{};
  present.hSrc.pDrvPrivate = nullptr;
  present.hSwapChain.pDrvPrivate = nullptr;
  present.hWnd = nullptr;
  present.sync_interval = 1;
  present.flags = 0;
  hr = cleanup.device_funcs.pfnPresent(create_dev.hDevice, &present);
  if (!Check(hr == S_OK, "Present")) {
    return false;
  }

  uint64_t final_fence = 0;
  uint64_t final_render_submits = 0;
  uint64_t final_present_submits = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    final_fence = adapter->last_submitted_fence;
    final_render_submits = adapter->render_submit_count;
    final_present_submits = adapter->present_submit_count;
  }
  if (!Check(final_fence == base_fence + 1, "Present submits exactly once when no render work is pending")) {
    return false;
  }
  if (!Check(final_render_submits == base_render_submits, "Present (idle) does not issue a render submit")) {
    return false;
  }
  if (!Check(final_present_submits == base_present_submits + 1, "Present (idle) issues exactly one present submit")) {
    return false;
  }

  uint64_t present_fence = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->inflight_present_fences.size() == 1, "inflight_present_fences contains one fence")) {
      return false;
    }
    present_fence = dev->inflight_present_fences.front();
  }
  return Check(present_fence == base_fence + 1, "present fence matches single submission");
}

bool TestPresentExSplitsRenderAndPresentSubmissions() {
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
  cleanup.hAdapter = open.hAdapter;
  cleanup.has_adapter = true;

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnClear != nullptr, "Clear must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnPresentEx != nullptr, "PresentEx must be available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* adapter = reinterpret_cast<Adapter*>(open.hAdapter.pDrvPrivate);
  if (!Check(dev != nullptr && adapter != nullptr, "device/adapter pointers")) {
    return false;
  }

  uint64_t base_fence = 0;
  uint64_t base_render_submits = 0;
  uint64_t base_present_submits = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    base_fence = adapter->last_submitted_fence;
    base_render_submits = adapter->render_submit_count;
    base_present_submits = adapter->present_submit_count;
  }

  // Emit a render command so PresentEx must flush it via a Render submission
  // before issuing the Present submission.
  hr = cleanup.device_funcs.pfnClear(create_dev.hDevice,
                                     /*flags=*/0,
                                     /*color_rgba8=*/0,
                                     /*depth=*/1.0f,
                                     /*stencil=*/0);
  if (!Check(hr == S_OK, "Clear")) {
    return false;
  }

  bool has_pending_render = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    has_pending_render = !dev->cmd.empty();
  }
  if (!Check(has_pending_render, "Clear emits pending render work")) {
    return false;
  }

  D3D9DDIARG_PRESENTEX present{};
  present.hSrc.pDrvPrivate = nullptr;
  present.hWnd = nullptr;
  present.sync_interval = 1;
  present.d3d9_present_flags = 0;
  hr = cleanup.device_funcs.pfnPresentEx(create_dev.hDevice, &present);
  if (!Check(hr == S_OK, "PresentEx")) {
    return false;
  }

  uint64_t final_fence = 0;
  uint64_t final_render_submits = 0;
  uint64_t final_present_submits = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    final_fence = adapter->last_submitted_fence;
    final_render_submits = adapter->render_submit_count;
    final_present_submits = adapter->present_submit_count;
  }

  if (!Check(final_fence == base_fence + 2,
             "PresentEx flushes render work then presents (two submissions)")) {
    return false;
  }
  if (!Check(final_render_submits == base_render_submits + 1, "PresentEx flush issues exactly one render submit")) {
    return false;
  }
  if (!Check(final_present_submits == base_present_submits + 1, "PresentEx issues exactly one present submit")) {
    return false;
  }

  uint64_t present_fence = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->inflight_present_fences.size() == 1, "inflight_present_fences contains one fence")) {
      return false;
    }
    present_fence = dev->inflight_present_fences.front();
  }
  return Check(present_fence == base_fence + 2, "present fence corresponds to second submission");
}

bool TestConcurrentPresentExReturnsDistinctFences() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs1{};
    D3D9DDI_DEVICEFUNCS device_funcs2{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice1{};
    D3DDDI_HDEVICE hDevice2{};
    bool has_adapter = false;
    bool has_device1 = false;
    bool has_device2 = false;
 
    ~Cleanup() {
      if (has_device1 && device_funcs1.pfnDestroyDevice) {
        device_funcs1.pfnDestroyDevice(hDevice1);
      }
      if (has_device2 && device_funcs2.pfnDestroyDevice) {
        device_funcs2.pfnDestroyDevice(hDevice2);
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
 
  D3D9DDIARG_CREATEDEVICE create_dev1{};
  create_dev1.hAdapter = open.hAdapter;
  create_dev1.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev1, &cleanup.device_funcs1);
  if (!Check(hr == S_OK, "CreateDevice(device1)")) {
    return false;
  }
  cleanup.hDevice1 = create_dev1.hDevice;
  cleanup.has_device1 = true;
 
  D3D9DDIARG_CREATEDEVICE create_dev2{};
  create_dev2.hAdapter = open.hAdapter;
  create_dev2.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev2, &cleanup.device_funcs2);
  if (!Check(hr == S_OK, "CreateDevice(device2)")) {
    return false;
  }
  cleanup.hDevice2 = create_dev2.hDevice;
  cleanup.has_device2 = true;
 
  if (!Check(cleanup.device_funcs1.pfnPresentEx != nullptr, "PresentEx must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs2.pfnPresentEx != nullptr, "PresentEx must be available (device2)")) {
    return false;
  }
 
  auto* dev1 = reinterpret_cast<Device*>(create_dev1.hDevice.pDrvPrivate);
  auto* dev2 = reinterpret_cast<Device*>(create_dev2.hDevice.pDrvPrivate);
  auto* adapter = reinterpret_cast<Adapter*>(open.hAdapter.pDrvPrivate);
  if (!Check(dev1 != nullptr && dev2 != nullptr && adapter != nullptr, "device/adapter pointers")) {
    return false;
  }
 
  uint64_t base_fence = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    base_fence = adapter->last_submitted_fence;
  }
 
  struct Gate {
    std::mutex mutex;
    std::condition_variable cv;
    int ready = 0;
    bool go = false;
  } gate;
 
  uint64_t fence1 = 0;
  uint64_t fence2 = 0;
  HRESULT hr1 = E_FAIL;
  HRESULT hr2 = E_FAIL;
 
  auto run_present = [&](D3DDDI_HDEVICE hDevice, Device* dev, D3D9DDI_DEVICEFUNCS* funcs, uint64_t* out_fence,
                         HRESULT* out_hr) {
    {
      std::unique_lock<std::mutex> lock(gate.mutex);
      gate.ready++;
      gate.cv.notify_all();
      gate.cv.wait(lock, [&] { return gate.go; });
    }
 
    D3D9DDIARG_PRESENTEX present{};
    present.hSrc.pDrvPrivate = nullptr;
    present.hWnd = nullptr;
    present.sync_interval = 1;
    present.d3d9_present_flags = 0;
    const HRESULT local_hr = funcs->pfnPresentEx(hDevice, &present);
 
    uint64_t local_fence = 0;
    {
      std::lock_guard<std::mutex> lock(dev->mutex);
      local_fence = dev->last_submission_fence;
    }
 
    if (out_fence) {
      *out_fence = local_fence;
    }
    if (out_hr) {
      *out_hr = local_hr;
    }
  };
 
  std::thread t1(run_present, create_dev1.hDevice, dev1, &cleanup.device_funcs1, &fence1, &hr1);
  std::thread t2(run_present, create_dev2.hDevice, dev2, &cleanup.device_funcs2, &fence2, &hr2);
 
  {
    std::unique_lock<std::mutex> lock(gate.mutex);
    if (!gate.cv.wait_for(lock, std::chrono::milliseconds(500), [&] { return gate.ready == 2; })) {
      gate.go = true;
      gate.cv.notify_all();
      lock.unlock();
      t1.join();
      t2.join();
      return Check(false, "PresentEx threads failed to start");
    }
    gate.go = true;
    gate.cv.notify_all();
  }
 
  t1.join();
  t2.join();
 
  if (!Check(hr1 == S_OK, "PresentEx(device1)")) {
    return false;
  }
  if (!Check(hr2 == S_OK, "PresentEx(device2)")) {
    return false;
  }
  if (!Check(fence1 != 0 && fence2 != 0, "PresentEx returns non-zero fences")) {
    return false;
  }
  if (!Check(fence1 != fence2, "Concurrent PresentEx submissions return distinct fences")) {
    return false;
  }
  if (!Check(fence1 > base_fence && fence2 > base_fence, "Concurrent PresentEx fences advance")) {
    return false;
  }
 
  const uint64_t max_fence = std::max(fence1, fence2);
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    if (!Check(adapter->last_submitted_fence >= max_fence, "adapter last_submitted_fence >= max PresentEx fence")) {
      return false;
    }
  }
  return true;
}

bool TestPresentSplitsRenderAndPresentSubmissions() {
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
  cleanup.hAdapter = open.hAdapter;
  cleanup.has_adapter = true;

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnClear != nullptr, "Clear must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnPresent != nullptr, "Present must be available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* adapter = reinterpret_cast<Adapter*>(open.hAdapter.pDrvPrivate);
  if (!Check(dev != nullptr && adapter != nullptr, "device/adapter pointers")) {
    return false;
  }

  uint64_t base_fence = 0;
  uint64_t base_render_submits = 0;
  uint64_t base_present_submits = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    base_fence = adapter->last_submitted_fence;
    base_render_submits = adapter->render_submit_count;
    base_present_submits = adapter->present_submit_count;
  }

  // Emit a render command so Present must flush it via a Render submission before
  // issuing the Present submission.
  hr = cleanup.device_funcs.pfnClear(create_dev.hDevice,
                                     /*flags=*/0,
                                     /*color_rgba8=*/0,
                                     /*depth=*/1.0f,
                                     /*stencil=*/0);
  if (!Check(hr == S_OK, "Clear")) {
    return false;
  }

  bool has_pending_render = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    has_pending_render = !dev->cmd.empty();
  }
  if (!Check(has_pending_render, "Clear emits pending render work")) {
    return false;
  }

  D3D9DDIARG_PRESENT present{};
  present.hSrc.pDrvPrivate = nullptr;
  present.hSwapChain.pDrvPrivate = nullptr;
  present.hWnd = nullptr;
  present.sync_interval = 1;
  present.flags = 0;
  hr = cleanup.device_funcs.pfnPresent(create_dev.hDevice, &present);
  if (!Check(hr == S_OK, "Present")) {
    return false;
  }

  uint64_t final_fence = 0;
  uint64_t final_render_submits = 0;
  uint64_t final_present_submits = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    final_fence = adapter->last_submitted_fence;
    final_render_submits = adapter->render_submit_count;
    final_present_submits = adapter->present_submit_count;
  }

  if (!Check(final_fence == base_fence + 2,
             "Present flushes render work then presents (two submissions)")) {
    return false;
  }
  if (!Check(final_render_submits == base_render_submits + 1, "Present flush issues exactly one render submit")) {
    return false;
  }
  if (!Check(final_present_submits == base_present_submits + 1, "Present issues exactly one present submit")) {
    return false;
  }

  uint64_t present_fence = 0;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->inflight_present_fences.size() == 1, "inflight_present_fences contains one fence")) {
      return false;
    }
    present_fence = dev->inflight_present_fences.front();
  }
  return Check(present_fence == base_fence + 2, "present fence corresponds to second submission");
}

bool TestFlushNoopsOnEmptyCommandBuffer() {
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
  cleanup.hAdapter = open.hAdapter;
  cleanup.has_adapter = true;

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnFlush != nullptr, "Flush must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnClear != nullptr, "Clear must be available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* adapter = reinterpret_cast<Adapter*>(open.hAdapter.pDrvPrivate);
  if (!Check(dev != nullptr && adapter != nullptr, "device/adapter pointers")) {
    return false;
  }

  uint64_t base_fence = 0;
  uint64_t base_render_submits = 0;
  uint64_t base_present_submits = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    base_fence = adapter->last_submitted_fence;
    base_render_submits = adapter->render_submit_count;
    base_present_submits = adapter->present_submit_count;
  }

  hr = cleanup.device_funcs.pfnFlush(create_dev.hDevice);
  if (!Check(hr == S_OK, "Flush(empty)")) {
    return false;
  }

  uint64_t after_empty_flush = 0;
  uint64_t after_empty_render_submits = 0;
  uint64_t after_empty_present_submits = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    after_empty_flush = adapter->last_submitted_fence;
    after_empty_render_submits = adapter->render_submit_count;
    after_empty_present_submits = adapter->present_submit_count;
  }
  if (!Check(after_empty_flush == base_fence, "Flush(empty) does not submit")) {
    return false;
  }
  if (!Check(after_empty_render_submits == base_render_submits, "Flush(empty) does not issue render submits")) {
    return false;
  }
  if (!Check(after_empty_present_submits == base_present_submits, "Flush(empty) does not issue present submits")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnClear(create_dev.hDevice,
                                     /*flags=*/0,
                                     /*color_rgba8=*/0,
                                     /*depth=*/1.0f,
                                     /*stencil=*/0);
  if (!Check(hr == S_OK, "Clear")) {
    return false;
  }

  bool has_pending_render = false;
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    has_pending_render = !dev->cmd.empty();
  }
  if (!Check(has_pending_render, "Clear emits pending render work")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnFlush(create_dev.hDevice);
  if (!Check(hr == S_OK, "Flush(non-empty)")) {
    return false;
  }

  uint64_t after_flush = 0;
  uint64_t after_render_submits = 0;
  uint64_t after_present_submits = 0;
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    after_flush = adapter->last_submitted_fence;
    after_render_submits = adapter->render_submit_count;
    after_present_submits = adapter->present_submit_count;
  }
  if (!Check(after_flush == base_fence + 1, "Flush submits once when command buffer is non-empty")) {
    return false;
  }
  if (!Check(after_render_submits == base_render_submits + 1, "Flush(non-empty) issues exactly one render submit")) {
    return false;
  }
  return Check(after_present_submits == base_present_submits, "Flush(non-empty) does not issue present submits");
}

bool TestGetDisplayModeExReturnsPrimaryMode() {
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
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnGetDisplayModeEx != nullptr, "GetDisplayModeEx must be available")) {
    return false;
  }

  D3DDDI_DISPLAYMODEEX mode{};
  D3DDDI_ROTATION rotation = D3DDDI_ROTATION_IDENTITY;
  D3D9DDIARG_GETDISPLAYMODEEX args{};
  args.swapchain = 0;
  args.pMode = &mode;
  args.pRotation = &rotation;

  hr = cleanup.device_funcs.pfnGetDisplayModeEx(create_dev.hDevice, &args);
  if (!Check(hr == S_OK, "GetDisplayModeEx")) {
    return false;
  }
  if (!Check(mode.Size == sizeof(D3DDDI_DISPLAYMODEEX), "display mode size field")) {
    return false;
  }
  if (!Check(mode.Width != 0 && mode.Height != 0, "display mode dimensions non-zero")) {
    return false;
  }
  if (!Check(mode.RefreshRate != 0, "display mode refresh non-zero")) {
    return false;
  }
  if (!Check(mode.Format == 22u, "display mode format is X8R8G8B8")) {
    return false;
  }
  if (!Check(mode.ScanLineOrdering == 1u, "display mode scanline progressive")) {
    return false;
  }
  return Check(rotation == D3DDDI_ROTATION_IDENTITY, "display rotation identity");
}

bool TestDeviceMiscExApisSucceed() {
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
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnCheckDeviceState != nullptr, "CheckDeviceState must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnWaitForVBlank != nullptr, "WaitForVBlank must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetGPUThreadPriority != nullptr, "SetGPUThreadPriority must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnGetGPUThreadPriority != nullptr, "GetGPUThreadPriority must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCheckResourceResidency != nullptr, "CheckResourceResidency must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnQueryResourceResidency != nullptr, "QueryResourceResidency must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnComposeRects != nullptr, "ComposeRects must be available")) {
    return false;
  }

  // DWM frequently probes device state without a window handle in some paths.
  hr = cleanup.device_funcs.pfnCheckDeviceState(create_dev.hDevice, nullptr);
  if (!Check(hr == S_OK, "CheckDeviceState(NULL)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetGPUThreadPriority(create_dev.hDevice, 100);
  if (!Check(hr == S_OK, "SetGPUThreadPriority(100)")) {
    return false;
  }
  int32_t priority = 0;
  hr = cleanup.device_funcs.pfnGetGPUThreadPriority(create_dev.hDevice, &priority);
  if (!Check(hr == S_OK, "GetGPUThreadPriority")) {
    return false;
  }
  if (!Check(priority == 7, "GPU thread priority clamps to +7")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetGPUThreadPriority(create_dev.hDevice, -100);
  if (!Check(hr == S_OK, "SetGPUThreadPriority(-100)")) {
    return false;
  }
  priority = 0;
  hr = cleanup.device_funcs.pfnGetGPUThreadPriority(create_dev.hDevice, &priority);
  if (!Check(hr == S_OK, "GetGPUThreadPriority after clamp")) {
    return false;
  }
  if (!Check(priority == -7, "GPU thread priority clamps to -7")) {
    return false;
  }

  // Residency queries should succeed and report resident in the system-memory
  // model.
  uint32_t residency[2] = {0, 0};
  D3D9DDIARG_QUERYRESOURCERESIDENCY query{};
  query.pResources = nullptr;
  query.resource_count = 2;
  query.pResidencyStatus = residency;
  hr = cleanup.device_funcs.pfnQueryResourceResidency(create_dev.hDevice, &query);
  if (!Check(hr == S_OK, "QueryResourceResidency")) {
    return false;
  }
  if (!Check(residency[0] == 1 && residency[1] == 1, "QueryResourceResidency reports resident")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnCheckResourceResidency(create_dev.hDevice, nullptr, 0);
  if (!Check(hr == S_OK, "CheckResourceResidency(0)")) {
    return false;
  }

  // ComposeRects is a D3D9Ex compositor helper; our bring-up path treats it as a
  // no-op but must still succeed.
  D3D9DDIARG_COMPOSERECTS compose{};
  compose.reserved0 = 0;
  compose.reserved1 = 0;
  hr = cleanup.device_funcs.pfnComposeRects(create_dev.hDevice, &compose);
  if (!Check(hr == S_OK, "ComposeRects")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnWaitForVBlank(create_dev.hDevice, 0);
  return Check(hr == S_OK, "WaitForVBlank");
}

bool TestAllocationListSplitResetsOnEmptySubmit() {
  // Repro for a subtle WDDM-only failure mode:
  //
  // Allocation list tracking may request a "flush/split" before we've emitted any
  // command packets in the new submission (e.g. because state-setting packets are
  // elided due to caching). In that situation submit() must still reset the
  // submission-local allocation tracking state even though it should not issue an
  // empty DMA submission.
  Adapter adapter;
  Device dev(&adapter);

  dev.wddm_context.hContext = 1; // enable tracking in portable builds

  D3DDDI_ALLOCATIONLIST list[1] = {};
  dev.alloc_list_tracker.rebind(list, 1, 0xFFFFu);

  auto r0 = dev.alloc_list_tracker.track_buffer_read(/*hAllocation=*/1, /*alloc_id=*/1, /*share_token=*/0);
  if (!Check(r0.status == AllocRefStatus::kOk, "track_buffer_read first")) {
    return false;
  }
  if (!Check(dev.cmd.empty(), "command stream still empty after tracking")) {
    return false;
  }
  if (!Check(dev.alloc_list_tracker.list_len() == 1, "allocation list full")) {
    return false;
  }

  // submit() should not issue an empty DMA submission, but it must still reset
  // submission-local allocation tracking state so we can continue tracking in a
  // new submission.
  {
    std::lock_guard<std::mutex> lock(dev.mutex);
    (void)submit_locked(&dev);
  }

  if (!Check(dev.alloc_list_tracker.list_len() == 0, "allocation list reset after empty submit")) {
    return false;
  }
  auto r1 = dev.alloc_list_tracker.track_buffer_read(/*hAllocation=*/2, /*alloc_id=*/2, /*share_token=*/0);
  if (!Check(r1.status == AllocRefStatus::kOk, "track_buffer_read after empty submit")) {
    return false;
  }
  if (!Check(dev.alloc_list_tracker.list_len() == 1, "allocation list len after re-track")) {
    return false;
  }
  if (!Check(list[0].hAllocation == 2, "allocation list entry points at second allocation")) {
    return false;
  }
  return true;
}

bool TestDrawStateTrackingPreSplitRetainsAllocs() {
  // Repro for a subtle WDDM-only failure mode:
  //
  // If the current submission's allocation list already contains entries from
  // earlier commands, draw-state tracking can exhaust the remaining capacity and
  // trigger a split mid-tracking. If that happens, we must ensure the new
  // submission re-tracks *all* draw allocations (not just those encountered
  // after the split) so host-side alloc-table lookups remain valid.
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE hDummy{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_dummy = false;

    ~Cleanup() {
      if (has_dummy && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hDummy);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnCreateResource != nullptr, "CreateResource must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "SetFVF must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitiveUP != nullptr, "DrawPrimitiveUP must be available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Enable allocation-list tracking in a portable build and constrain capacity so
  // draw-state tracking must pre-split if there is an outstanding tracked alloc.
  dev->wddm_context.hContext = 1;
  D3DDDI_ALLOCATIONLIST alloc_list[2] = {};
  dev->alloc_list_tracker.rebind(alloc_list, 2, 0xFFFFu);
  dev->alloc_list_tracker.reset();

  aerogpu_wddm_alloc_priv priv{};
  std::memset(&priv, 0, sizeof(priv));

  D3D9DDIARG_CREATERESOURCE create_res{};
  create_res.type = 6u; // D3DRTYPE_VERTEXBUFFER
  create_res.format = 0;
  create_res.width = 0;
  create_res.height = 0;
  create_res.depth = 1;
  create_res.mip_levels = 1;
  create_res.usage = 0;
  create_res.pool = 0;
  create_res.size = 64;
  create_res.hResource.pDrvPrivate = nullptr;
  create_res.pSharedHandle = nullptr;
  create_res.pKmdAllocPrivateData = &priv;
  create_res.KmdAllocPrivateDataSize = sizeof(priv);
  create_res.wddm_hAllocation = 0x1111u;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_res);
  if (!Check(hr == S_OK, "CreateResource(dummy alloc-backed VB)")) {
    return false;
  }
  if (!Check(create_res.hResource.pDrvPrivate != nullptr, "CreateResource returned resource handle")) {
    return false;
  }
  cleanup.hDummy = create_res.hResource;
  cleanup.has_dummy = true;

  // Ensure the dummy resource consumed one allocation-list entry.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->alloc_list_tracker.list_len() == 1, "allocation list has 1 entry after CreateResource")) {
      return false;
    }
  }
  if (!Check(alloc_list[0].hAllocation == create_res.wddm_hAllocation, "allocation list contains dummy hAllocation")) {
    return false;
  }

  // Bind two distinct alloc-backed resources in draw state. (We don't need to
  // emit SetRenderTarget/SetTexture packets; we only need the pointers for
  // allocation tracking.)
  Resource rt{};
  rt.kind = ResourceKind::Texture2D;
  rt.handle = 0x2000u;
  rt.backing_alloc_id = 1;
  rt.share_token = 0;
  rt.wddm_hAllocation = 0x2000u;

  Resource tex{};
  tex.kind = ResourceKind::Texture2D;
  tex.handle = 0x3000u;
  tex.backing_alloc_id = 2;
  tex.share_token = 0;
  tex.wddm_hAllocation = 0x3000u;

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    dev->render_targets[0] = &rt;
    dev->textures[0] = &tex;
  }

  D3DDDIVIEWPORTINFO vp{};
  vp.X = 0.0f;
  vp.Y = 0.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  hr = cleanup.device_funcs.pfnSetViewport(create_dev.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport")) {
    return false;
  }

  // D3DFVF_XYZRHW (0x4) | D3DFVF_DIFFUSE (0x40).
  hr = cleanup.device_funcs.pfnSetFVF(create_dev.hDevice, 0x44u);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  struct Vertex {
    float x;
    float y;
    float z;
    float rhw;
    uint32_t color;
  };

  constexpr uint32_t kGreen = 0xFF00FF00u;
  Vertex verts[3]{};
  verts[0] = {256.0f * 0.25f, 256.0f * 0.25f, 0.5f, 1.0f, kGreen};
  verts[1] = {256.0f * 0.75f, 256.0f * 0.25f, 0.5f, 1.0f, kGreen};
  verts[2] = {256.0f * 0.50f, 256.0f * 0.75f, 0.5f, 1.0f, kGreen};

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(create_dev.hDevice, D3DDDIPT_TRIANGLELIST, 1, verts, sizeof(Vertex));
  if (!Check(hr == S_OK, "DrawPrimitiveUP")) {
    return false;
  }

  // After the draw, the allocation list should contain *all* draw dependencies.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->alloc_list_tracker.list_len() == 2, "allocation list contains draw deps after split")) {
      return false;
    }
  }
  if (!Check(alloc_list[0].hAllocation == rt.wddm_hAllocation, "allocation list contains draw RT mapping")) {
    return false;
  }
  if (!Check(alloc_list[0].WriteOperation == 1, "allocation list marks draw RT as write")) {
    return false;
  }
  if (!Check(alloc_list[1].hAllocation == tex.wddm_hAllocation, "allocation list contains draw texture mapping")) {
    return false;
  }
  return Check(alloc_list[1].WriteOperation == 0, "allocation list marks draw texture as read");
}

bool TestRenderTargetTrackingPreSplitRetainsAllocs() {
  // Similar to TestDrawStateTrackingPreSplitRetainsAllocs, but for Clear(): the
  // render-target tracking helper must not drop earlier tracked render targets
  // if allocation-list tracking needs to split.
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE hDummy{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_dummy = false;

    ~Cleanup() {
      if (has_dummy && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hDummy);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnCreateResource != nullptr, "CreateResource must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnClear != nullptr, "Clear must be available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->wddm_context.hContext = 1;
  D3DDDI_ALLOCATIONLIST alloc_list[2] = {};
  dev->alloc_list_tracker.rebind(alloc_list, 2, 0xFFFFu);
  dev->alloc_list_tracker.reset();

  aerogpu_wddm_alloc_priv priv{};
  std::memset(&priv, 0, sizeof(priv));

  D3D9DDIARG_CREATERESOURCE create_res{};
  create_res.type = 6u; // D3DRTYPE_VERTEXBUFFER
  create_res.format = 0;
  create_res.width = 0;
  create_res.height = 0;
  create_res.depth = 1;
  create_res.mip_levels = 1;
  create_res.usage = 0;
  create_res.pool = 0;
  create_res.size = 64;
  create_res.hResource.pDrvPrivate = nullptr;
  create_res.pSharedHandle = nullptr;
  create_res.pKmdAllocPrivateData = &priv;
  create_res.KmdAllocPrivateDataSize = sizeof(priv);
  create_res.wddm_hAllocation = 0x1111u;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_res);
  if (!Check(hr == S_OK, "CreateResource(dummy alloc-backed VB)")) {
    return false;
  }
  if (!Check(create_res.hResource.pDrvPrivate != nullptr, "CreateResource returned resource handle")) {
    return false;
  }
  cleanup.hDummy = create_res.hResource;
  cleanup.has_dummy = true;

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->alloc_list_tracker.list_len() == 1, "allocation list has 1 entry after CreateResource")) {
      return false;
    }
  }

  Resource rt0{};
  rt0.kind = ResourceKind::Texture2D;
  rt0.handle = 0x2000u;
  rt0.backing_alloc_id = 1;
  rt0.share_token = 0;
  rt0.wddm_hAllocation = 0x2000u;

  Resource rt1{};
  rt1.kind = ResourceKind::Texture2D;
  rt1.handle = 0x2001u;
  rt1.backing_alloc_id = 2;
  rt1.share_token = 0;
  rt1.wddm_hAllocation = 0x2001u;

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    dev->render_targets[0] = &rt0;
    dev->render_targets[1] = &rt1;
    dev->render_targets[2] = nullptr;
    dev->render_targets[3] = nullptr;
    dev->depth_stencil = nullptr;
  }

  hr = cleanup.device_funcs.pfnClear(create_dev.hDevice,
                                     /*flags=*/0x1u,
                                     /*color_rgba8=*/0xFF0000FFu,
                                     /*depth=*/1.0f,
                                     /*stencil=*/0);
  if (!Check(hr == S_OK, "Clear")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->alloc_list_tracker.list_len() == 2, "allocation list contains MRT deps after split")) {
      return false;
    }
  }
  if (!Check(alloc_list[0].hAllocation == rt0.wddm_hAllocation, "allocation list contains RT0 mapping")) {
    return false;
  }
  if (!Check(alloc_list[0].WriteOperation == 1, "allocation list marks RT0 as write")) {
    return false;
  }
  if (!Check(alloc_list[1].hAllocation == rt1.wddm_hAllocation, "allocation list contains RT1 mapping")) {
    return false;
  }
  return Check(alloc_list[1].WriteOperation == 1, "allocation list marks RT1 as write");
}

bool TestDrawStateTrackingDedupsSharedAllocIds() {
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
  cleanup.hAdapter = open.hAdapter;
  cleanup.has_adapter = true;

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "SetFVF must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitiveUP != nullptr, "DrawPrimitiveUP must be available")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // When the same shared allocation is opened multiple times, the D3D9 runtime
  // can hand us distinct WDDM allocation handles that alias the same alloc_id.
  // The allocation list (and host-side alloc table) is keyed by alloc_id, so a
  // draw referencing both handles should still only consume a single allocation
  // list entry.
  dev->wddm_context.hContext = 1;
  D3DDDI_ALLOCATIONLIST alloc_list[1] = {};
  dev->alloc_list_tracker.rebind(alloc_list, 1, 0xFFFFu);

  Resource rt{};
  rt.kind = ResourceKind::Texture2D;
  rt.handle = 1;
  rt.backing_alloc_id = 1;
  rt.share_token = 0x1122334455667788ull;
  rt.wddm_hAllocation = 100;

  Resource tex{};
  tex.kind = ResourceKind::Texture2D;
  tex.handle = 2;
  tex.backing_alloc_id = 1;
  tex.share_token = 0x1122334455667788ull;
  tex.wddm_hAllocation = 200;

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    dev->render_targets[0] = &rt;
    dev->textures[0] = &tex;
  }

  D3DDDIVIEWPORTINFO vp{};
  vp.X = 0.0f;
  vp.Y = 0.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  hr = cleanup.device_funcs.pfnSetViewport(create_dev.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport")) {
    return false;
  }

  // D3DFVF_XYZRHW (0x4) | D3DFVF_DIFFUSE (0x40).
  hr = cleanup.device_funcs.pfnSetFVF(create_dev.hDevice, 0x44u);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  struct Vertex {
    float x;
    float y;
    float z;
    float rhw;
    uint32_t color;
  };

  constexpr uint32_t kGreen = 0xFF00FF00u;
  Vertex verts[3]{};
  verts[0] = {256.0f * 0.25f, 256.0f * 0.25f, 0.5f, 1.0f, kGreen};
  verts[1] = {256.0f * 0.75f, 256.0f * 0.25f, 0.5f, 1.0f, kGreen};
  verts[2] = {256.0f * 0.50f, 256.0f * 0.75f, 0.5f, 1.0f, kGreen};

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(create_dev.hDevice, D3DDDIPT_TRIANGLELIST, 1, verts, sizeof(Vertex));
  if (!Check(hr == S_OK, "DrawPrimitiveUP")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->alloc_list_tracker.list_len() == 1, "draw tracking dedups shared alloc_id")) {
      return false;
    }
  }

  if (!Check(alloc_list[0].hAllocation == rt.wddm_hAllocation, "allocation list uses first tracked handle")) {
    return false;
  }
  return Check(alloc_list[0].WriteOperation == 1, "render-target write upgrades allocation list entry");
}

bool TestRotateResourceIdentitiesTrackingPreSplitRetainsAllocs() {
  // RotateResourceIdentities may need to emit multiple rebinding packets (RTs +
  // rotated textures/streams/index). Allocation tracking can split the submission
  // when the list is full; ensure we pre-split so earlier tracked allocations are
  // not dropped.
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
  cleanup.hAdapter = open.hAdapter;
  cleanup.has_adapter = true;

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnRotateResourceIdentities != nullptr, "RotateResourceIdentities entrypoint")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  dev->wddm_context.hContext = 1;
  D3DDDI_ALLOCATIONLIST alloc_list[2] = {};
  dev->alloc_list_tracker.rebind(alloc_list, 2, 0xFFFFu);
  dev->alloc_list_tracker.reset();

  // Pre-fill the allocation list to simulate other work already tracked in the
  // submission. This should force RotateResourceIdentities to split before it
  // begins tracking its own dependencies.
  const AllocRef dummy_ref = dev->alloc_list_tracker.track_buffer_read(/*hAllocation=*/0x9999u,
                                                                       /*alloc_id=*/0x999u,
                                                                       /*share_token=*/0);
  if (!Check(dummy_ref.status == AllocRefStatus::kOk, "dummy allocation tracked")) {
    return false;
  }
  if (!Check(dev->alloc_list_tracker.list_len() == 1, "allocation list has 1 pre-filled entry")) {
    return false;
  }

  Resource rt{};
  rt.kind = ResourceKind::Texture2D;
  rt.handle = 0x2000u;
  rt.backing_alloc_id = 1;
  rt.share_token = 0;
  rt.wddm_hAllocation = 0x2000u;

  Resource tex0{};
  tex0.kind = ResourceKind::Texture2D;
  tex0.type = 0;
  tex0.format = 22u; // D3DFMT_X8R8G8B8
  tex0.width = 16;
  tex0.height = 16;
  tex0.depth = 1;
  tex0.mip_levels = 1;
  tex0.usage = 0;
  tex0.pool = 0;
  tex0.size_bytes = 16u * 16u * 4u;
  tex0.row_pitch = 16u * 4u;
  tex0.slice_pitch = tex0.size_bytes;
  tex0.handle = 0x3000u;
  tex0.backing_alloc_id = 2;
  tex0.share_token = 0;
  tex0.wddm_hAllocation = 0x3000u;

  Resource tex1 = tex0;
  tex1.handle = 0x3001u;
  tex1.backing_alloc_id = 3;
  tex1.wddm_hAllocation = 0x3001u;

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    dev->cmd.reset();
    dev->render_targets[0] = &rt;
    dev->render_targets[1] = nullptr;
    dev->render_targets[2] = nullptr;
    dev->render_targets[3] = nullptr;
    dev->depth_stencil = nullptr;
    dev->textures[0] = &tex0;
    for (uint32_t i = 1; i < 16; ++i) {
      dev->textures[i] = nullptr;
    }
    for (uint32_t i = 0; i < 16; ++i) {
      dev->streams[i].vb = nullptr;
    }
    dev->index_buffer = nullptr;
  }

  D3DDDI_HRESOURCE rotate[2]{};
  rotate[0].pDrvPrivate = &tex0;
  rotate[1].pDrvPrivate = &tex1;

  hr = cleanup.device_funcs.pfnRotateResourceIdentities(create_dev.hDevice, rotate, 2);
  if (!Check(hr == S_OK, "RotateResourceIdentities")) {
    return false;
  }

  // The allocation list should contain both the RT and the rotated texture (now
  // bound to stage 0), with the render target marked as WriteOperation.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->alloc_list_tracker.list_len() == 2, "allocation list contains rotate rebind deps after split")) {
      return false;
    }
  }
  if (!Check(alloc_list[0].hAllocation == rt.wddm_hAllocation, "allocation list contains RT mapping")) {
    return false;
  }
  if (!Check(alloc_list[0].WriteOperation == 1, "allocation list marks RT as write")) {
    return false;
  }
  if (!Check(alloc_list[1].hAllocation == tex0.wddm_hAllocation, "allocation list contains rotated texture mapping")) {
    return false;
  }
  return Check(alloc_list[1].WriteOperation == 0, "allocation list marks rotated texture as read");
}

bool TestOpenResourceCapturesWddmAllocationForTracking() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE hResource{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_resource = false;

    ~Cleanup() {
      if (has_resource && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hResource);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Enable allocation-list tracking in a portable build.
  dev->wddm_context.hContext = 1;
  D3DDDI_ALLOCATIONLIST alloc_list[4] = {};
  dev->alloc_list_tracker.rebind(alloc_list, 4, 0xFFFFu);
  dev->alloc_list_tracker.reset();

  aerogpu_wddm_alloc_priv priv{};
  priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
  priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
  priv.alloc_id = 1;
  priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;
  priv.share_token = 0x1122334455667788ull;
  priv.size_bytes = 16ull * 16ull * 4ull;
  priv.reserved0 = 0;

  D3D9DDIARG_OPENRESOURCE open_res{};
  open_res.pPrivateDriverData = &priv;
  open_res.private_driver_data_size = sizeof(priv);
  open_res.type = 0;
  open_res.format = 22u; // D3DFMT_X8R8G8B8
  open_res.width = 16;
  open_res.height = 16;
  open_res.depth = 1;
  open_res.mip_levels = 1;
  open_res.usage = 0;
  open_res.size = 0;
  open_res.hResource.pDrvPrivate = nullptr;
  open_res.wddm_hAllocation = 0x1234u;

  if (!Check(cleanup.device_funcs.pfnOpenResource != nullptr, "OpenResource entrypoint")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnOpenResource(create_dev.hDevice, &open_res);
  if (!Check(hr == S_OK, "OpenResource")) {
    return false;
  }
  if (!Check(open_res.hResource.pDrvPrivate != nullptr, "OpenResource returned resource")) {
    return false;
  }
  cleanup.hResource = open_res.hResource;
  cleanup.has_resource = true;

  auto* res = reinterpret_cast<Resource*>(open_res.hResource.pDrvPrivate);
  if (!Check(res->backing_alloc_id == priv.alloc_id, "OpenResource captures alloc_id")) {
    return false;
  }
  if (!Check(res->wddm_hAllocation == open_res.wddm_hAllocation, "OpenResource captures wddm_hAllocation")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetRenderTarget(create_dev.hDevice, 0, open_res.hResource);
  if (!Check(hr == S_OK, "SetRenderTarget")) {
    return false;
  }

  // Clear forces render-target allocation tracking; this should succeed when
  // OpenResource supplies wddm_hAllocation.
  hr = cleanup.device_funcs.pfnClear(create_dev.hDevice,
                                     /*flags=*/0x1u,
                                     /*color_rgba8=*/0xFFFFFFFFu,
                                     /*depth=*/1.0f,
                                     /*stencil=*/0);
  if (!Check(hr == S_OK, "Clear")) {
    return false;
  }

  if (!Check(dev->alloc_list_tracker.list_len() == 1, "allocation list includes imported RT")) {
    return false;
  }
  if (!Check(alloc_list[0].hAllocation == open_res.wddm_hAllocation, "tracked allocation handle matches")) {
    return false;
  }
  if (!Check(alloc_list[0].WriteOperation == 1, "tracked allocation is marked WriteOperation")) {
    return false;
  }
  return true;
}

bool TestInvalidPayloadArgs() {
  uint8_t buf[256] = {};

  SpanCmdStreamWriter w(buf, sizeof(buf));
  w.reset();

  auto* cmd =
      w.append_with_payload<aerogpu_cmd_create_shader_dxbc>(AEROGPU_CMD_CREATE_SHADER_DXBC, nullptr, 4);
  if (!Check(cmd == nullptr, "append_with_payload rejects null payload")) {
    return false;
  }
  if (!Check(w.error() == CmdStreamError::kInvalidArgument, "null payload sets kInvalidArgument")) {
    return false;
  }

  w.reset();
  const size_t too_large = std::numeric_limits<size_t>::max();
  cmd = w.append_with_payload<aerogpu_cmd_create_shader_dxbc>(AEROGPU_CMD_CREATE_SHADER_DXBC, buf, too_large);
  if (!Check(cmd == nullptr, "append_with_payload rejects oversized payload")) {
    return false;
  }
  if (!Check(w.error() == CmdStreamError::kSizeTooLarge, "oversized payload sets kSizeTooLarge")) {
    return false;
  }

  // Cover the edge case where `payload_size` would not overflow the
  // `payload_size + sizeof(HeaderT)` check, but would overflow padding/alignment
  // when rounding up to 4 bytes.
  w.reset();
  const size_t near_max = std::numeric_limits<size_t>::max() - sizeof(aerogpu_cmd_create_shader_dxbc);
  cmd = w.append_with_payload<aerogpu_cmd_create_shader_dxbc>(AEROGPU_CMD_CREATE_SHADER_DXBC, buf, near_max);
  if (!Check(cmd == nullptr, "append_with_payload rejects near-max payload")) {
    return false;
  }
  if (!Check(w.error() == CmdStreamError::kSizeTooLarge, "near-max payload sets kSizeTooLarge")) {
    return false;
  }

  VectorCmdStreamWriter vec;
  vec.reset();
  cmd = vec.append_with_payload<aerogpu_cmd_create_shader_dxbc>(AEROGPU_CMD_CREATE_SHADER_DXBC, buf, near_max);
  if (!Check(cmd == nullptr, "VectorCmdStreamWriter rejects near-max payload")) {
    return false;
  }
  return Check(vec.error() == CmdStreamError::kSizeTooLarge, "VectorCmdStreamWriter near-max payload sets kSizeTooLarge");
}

bool TestDestroyBoundShaderUnbinds() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3D9DDI_HSHADER hShader{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_shader = false;

    ~Cleanup() {
      if (has_shader && device_funcs.pfnDestroyShader) {
        device_funcs.pfnDestroyShader(hDevice, hShader);
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

  const uint8_t dxbc[] = {0x44, 0x58, 0x42, 0x43, 0x00, 0x01, 0x02, 0x03};
  D3D9DDI_HSHADER hShader{};
  hr = cleanup.device_funcs.pfnCreateShader(create_dev.hDevice,
                                            kD3d9ShaderStageVs,
                                            dxbc,
                                            static_cast<uint32_t>(sizeof(dxbc)),
                                            &hShader);
  if (!Check(hr == S_OK, "CreateShader(VS)")) {
    return false;
  }
  if (!Check(hShader.pDrvPrivate != nullptr, "CreateShader returned shader handle")) {
    return false;
  }
  cleanup.hShader = hShader;
  cleanup.has_shader = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* sh = reinterpret_cast<Shader*>(hShader.pDrvPrivate);

  hr = cleanup.device_funcs.pfnSetShader(create_dev.hDevice, kD3d9ShaderStageVs, hShader);
  if (!Check(hr == S_OK, "SetShader(VS)")) {
    return false;
  }
  if (!Check(dev->vs == sh, "SetShader updates cached vs pointer")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDestroyShader(create_dev.hDevice, hShader);
  if (!Check(hr == S_OK, "DestroyShader")) {
    return false;
  }
  cleanup.has_shader = false;

  if (!Check(dev->vs == nullptr, "DestroyShader clears cached vs pointer")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  const CmdLoc bind = FindLastOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(bind.hdr != nullptr, "bind_shaders emitted")) {
    return false;
  }

  const auto* bind_cmd = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(bind.hdr);
  if (!Check(bind_cmd->vs == 0, "bind_shaders clears vs handle")) {
    return false;
  }

  const CmdLoc destroy = FindLastOpcode(buf, len, AEROGPU_CMD_DESTROY_SHADER);
  if (!Check(destroy.hdr != nullptr, "destroy_shader emitted")) {
    return false;
  }
  return Check(bind.offset < destroy.offset, "unbind occurs before destroy");
}

bool TestDestroyBoundVertexDeclUnbinds() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3D9DDI_HVERTEXDECL hDecl{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_decl = false;

    ~Cleanup() {
      if (has_decl && device_funcs.pfnDestroyVertexDecl) {
        device_funcs.pfnDestroyVertexDecl(hDevice, hDecl);
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

  const uint8_t blob[] = {0x01, 0x02, 0x03, 0x04};
  D3D9DDI_HVERTEXDECL hDecl{};
  hr = cleanup.device_funcs.pfnCreateVertexDecl(create_dev.hDevice,
                                                blob,
                                                static_cast<uint32_t>(sizeof(blob)),
                                                &hDecl);
  if (!Check(hr == S_OK, "CreateVertexDecl")) {
    return false;
  }
  if (!Check(hDecl.pDrvPrivate != nullptr, "CreateVertexDecl returned handle")) {
    return false;
  }
  cleanup.hDecl = hDecl;
  cleanup.has_decl = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* decl = reinterpret_cast<VertexDecl*>(hDecl.pDrvPrivate);

  hr = cleanup.device_funcs.pfnSetVertexDecl(create_dev.hDevice, hDecl);
  if (!Check(hr == S_OK, "SetVertexDecl")) {
    return false;
  }
  if (!Check(dev->vertex_decl == decl, "SetVertexDecl updates cached decl pointer")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDestroyVertexDecl(create_dev.hDevice, hDecl);
  if (!Check(hr == S_OK, "DestroyVertexDecl")) {
    return false;
  }
  cleanup.has_decl = false;

  if (!Check(dev->vertex_decl == nullptr, "DestroyVertexDecl clears cached decl pointer")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  const CmdLoc set_layout = FindLastOpcode(buf, len, AEROGPU_CMD_SET_INPUT_LAYOUT);
  if (!Check(set_layout.hdr != nullptr, "set_input_layout emitted")) {
    return false;
  }
  const auto* set_cmd = reinterpret_cast<const aerogpu_cmd_set_input_layout*>(set_layout.hdr);
  if (!Check(set_cmd->input_layout_handle == 0, "set_input_layout clears handle")) {
    return false;
  }

  const CmdLoc destroy = FindLastOpcode(buf, len, AEROGPU_CMD_DESTROY_INPUT_LAYOUT);
  if (!Check(destroy.hdr != nullptr, "destroy_input_layout emitted")) {
    return false;
  }
  return Check(set_layout.offset < destroy.offset, "unbind occurs before destroy");
}

bool TestFvfXyzrhwDiffuseDrawPrimitiveUpEmitsFixedfuncCommands() {
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
  cleanup.hAdapter = open.hAdapter;
  cleanup.has_adapter = true;

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "SetFVF must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawPrimitiveUP != nullptr, "DrawPrimitiveUP must be available")) {
    return false;
  }

  D3DDDIVIEWPORTINFO vp{};
  vp.X = 0.0f;
  vp.Y = 0.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  hr = cleanup.device_funcs.pfnSetViewport(create_dev.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport")) {
    return false;
  }

  // D3DFVF_XYZRHW (0x4) | D3DFVF_DIFFUSE (0x40).
  hr = cleanup.device_funcs.pfnSetFVF(create_dev.hDevice, 0x44u);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  struct Vertex {
    float x;
    float y;
    float z;
    float rhw;
    uint32_t color;
  };

  constexpr uint32_t kGreen = 0xFF00FF00u;
  Vertex verts[3]{};
  verts[0] = {256.0f * 0.25f, 256.0f * 0.25f, 0.5f, 1.0f, kGreen};
  verts[1] = {256.0f * 0.75f, 256.0f * 0.25f, 0.5f, 1.0f, kGreen};
  verts[2] = {256.0f * 0.50f, 256.0f * 0.75f, 0.5f, 1.0f, kGreen};

  hr = cleanup.device_funcs.pfnDrawPrimitiveUP(
      create_dev.hDevice, D3DDDIPT_TRIANGLELIST, 1, verts, sizeof(Vertex));
  if (!Check(hr == S_OK, "DrawPrimitiveUP")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) >= 2,
             "fixed-function fallback creates shaders")) {
    return false;
  }

  const CmdLoc bind = FindLastOpcode(buf, len, AEROGPU_CMD_BIND_SHADERS);
  if (!Check(bind.hdr != nullptr, "bind_shaders emitted")) {
    return false;
  }
  const auto* bind_cmd = reinterpret_cast<const aerogpu_cmd_bind_shaders*>(bind.hdr);
  if (!Check(bind_cmd->vs != 0 && bind_cmd->ps != 0, "bind_shaders uses non-zero VS/PS handles")) {
    return false;
  }

  const CmdLoc upload = FindLastOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload.hdr != nullptr, "upload_resource emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(upload.hdr);
  if (!Check(upload_cmd->offset_bytes == 0, "upload_resource offset is 0")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(verts), "upload_resource size matches vertex data")) {
    return false;
  }

  const uint8_t* payload = reinterpret_cast<const uint8_t*>(upload_cmd) + sizeof(*upload_cmd);
  float x0 = 0.0f;
  float y0 = 0.0f;
  float z0 = 0.0f;
  float w0 = 0.0f;
  uint32_t c0 = 0;
  std::memcpy(&x0, payload + 0, sizeof(float));
  std::memcpy(&y0, payload + 4, sizeof(float));
  std::memcpy(&z0, payload + 8, sizeof(float));
  std::memcpy(&w0, payload + 12, sizeof(float));
  std::memcpy(&c0, payload + 16, sizeof(uint32_t));

  const float expected_x0 = ((verts[0].x + 0.5f - vp.X) / vp.Width) * 2.0f - 1.0f;
  const float expected_y0 = 1.0f - ((verts[0].y + 0.5f - vp.Y) / vp.Height) * 2.0f;
  if (!Check(std::fabs(x0 - expected_x0) < 1e-6f, "XYZRHW->clip: x0 matches half-pixel convention")) {
    return false;
  }
  if (!Check(std::fabs(y0 - expected_y0) < 1e-6f, "XYZRHW->clip: y0 matches half-pixel convention")) {
    return false;
  }
  if (!Check(std::fabs(z0 - verts[0].z) < 1e-6f, "XYZRHW->clip: z preserved")) {
    return false;
  }
  if (!Check(std::fabs(w0 - 1.0f) < 1e-6f, "XYZRHW->clip: w preserved")) {
    return false;
  }
  return Check(c0 == kGreen, "XYZRHW->clip: diffuse color preserved");
}

bool TestFvfXyzrhwDiffuseDrawPrimitiveEmulationConvertsVertices() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE hVb{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_vb = false;

    ~Cleanup() {
      if (has_vb && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hVb);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "SetFVF must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateResource != nullptr, "CreateResource must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnLock != nullptr && cleanup.device_funcs.pfnUnlock != nullptr, "Lock/Unlock must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "SetStreamSource must be available")) {
    return false;
  }

  D3DDDIVIEWPORTINFO vp{};
  vp.X = 0.0f;
  vp.Y = 0.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  hr = cleanup.device_funcs.pfnSetViewport(create_dev.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport")) {
    return false;
  }

  // D3DFVF_XYZRHW (0x4) | D3DFVF_DIFFUSE (0x40).
  hr = cleanup.device_funcs.pfnSetFVF(create_dev.hDevice, 0x44u);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  struct Vertex {
    float x;
    float y;
    float z;
    float rhw;
    uint32_t color;
  };

  constexpr uint32_t kGreen = 0xFF00FF00u;
  Vertex verts[3]{};
  verts[0] = {256.0f * 0.25f, 256.0f * 0.25f, 0.5f, 1.0f, kGreen};
  verts[1] = {256.0f * 0.75f, 256.0f * 0.25f, 0.5f, 1.0f, kGreen};
  verts[2] = {256.0f * 0.50f, 256.0f * 0.75f, 0.5f, 1.0f, kGreen};

  D3D9DDIARG_CREATERESOURCE create_res{};
  create_res.type = 0;
  create_res.format = 0;
  create_res.width = 0;
  create_res.height = 0;
  create_res.depth = 0;
  create_res.mip_levels = 1;
  create_res.usage = 0;
  create_res.pool = 0;
  create_res.size = sizeof(verts);
  create_res.hResource.pDrvPrivate = nullptr;
  create_res.pSharedHandle = nullptr;
  create_res.pKmdAllocPrivateData = nullptr;
  create_res.KmdAllocPrivateDataSize = 0;
  create_res.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_res);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer)")) {
    return false;
  }
  cleanup.hVb = create_res.hResource;
  cleanup.has_vb = true;

  D3D9DDIARG_LOCK lock{};
  lock.hResource = create_res.hResource;
  lock.offset_bytes = 0;
  lock.size_bytes = 0;
  lock.flags = 0;
  D3DDDI_LOCKEDBOX box{};
  hr = cleanup.device_funcs.pfnLock(create_dev.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(vertex buffer)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock returns pData")) {
    return false;
  }
  std::memcpy(box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = create_res.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(create_dev.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(vertex buffer)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(create_dev.hDevice, 0, create_res.hResource, 0, sizeof(Vertex));
  if (!Check(hr == S_OK, "SetStreamSource")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawPrimitive(create_dev.hDevice, D3DDDIPT_TRIANGLELIST, 0, 1);
  if (!Check(hr == S_OK, "DrawPrimitive")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_CREATE_SHADER_DXBC) >= 2,
             "fixed-function fallback creates shaders")) {
    return false;
  }

  const CmdLoc upload = FindLastOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload.hdr != nullptr, "upload_resource emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(upload.hdr);
  if (!Check(upload_cmd->size_bytes == sizeof(verts), "upload_resource size matches vertex data")) {
    return false;
  }

  const uint8_t* payload = reinterpret_cast<const uint8_t*>(upload_cmd) + sizeof(*upload_cmd);
  float x0 = 0.0f;
  float y0 = 0.0f;
  float z0 = 0.0f;
  float w0 = 0.0f;
  uint32_t c0 = 0;
  std::memcpy(&x0, payload + 0, sizeof(float));
  std::memcpy(&y0, payload + 4, sizeof(float));
  std::memcpy(&z0, payload + 8, sizeof(float));
  std::memcpy(&w0, payload + 12, sizeof(float));
  std::memcpy(&c0, payload + 16, sizeof(uint32_t));

  const float expected_x0 = ((verts[0].x + 0.5f - vp.X) / vp.Width) * 2.0f - 1.0f;
  const float expected_y0 = 1.0f - ((verts[0].y + 0.5f - vp.Y) / vp.Height) * 2.0f;
  if (!Check(std::fabs(x0 - expected_x0) < 1e-6f, "DrawPrimitive: x0 matches half-pixel convention")) {
    return false;
  }
  if (!Check(std::fabs(y0 - expected_y0) < 1e-6f, "DrawPrimitive: y0 matches half-pixel convention")) {
    return false;
  }
  if (!Check(std::fabs(z0 - verts[0].z) < 1e-6f, "DrawPrimitive: z preserved")) {
    return false;
  }
  if (!Check(std::fabs(w0 - 1.0f) < 1e-6f, "DrawPrimitive: w preserved")) {
    return false;
  }
  return Check(c0 == kGreen, "DrawPrimitive: diffuse color preserved");
}

bool TestDrawIndexedPrimitiveUpEmitsIndexBufferCommands() {
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
  cleanup.hAdapter = open.hAdapter;
  cleanup.has_adapter = true;

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "SetFVF must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetViewport != nullptr, "SetViewport must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawIndexedPrimitive2 != nullptr, "DrawIndexedPrimitive2 must be available")) {
    return false;
  }

  D3DDDIVIEWPORTINFO vp{};
  vp.X = 0.0f;
  vp.Y = 0.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  hr = cleanup.device_funcs.pfnSetViewport(create_dev.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport")) {
    return false;
  }

  // D3DFVF_XYZRHW (0x4) | D3DFVF_DIFFUSE (0x40).
  hr = cleanup.device_funcs.pfnSetFVF(create_dev.hDevice, 0x44u);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  struct Vertex {
    float x;
    float y;
    float z;
    float rhw;
    uint32_t color;
  };

  constexpr uint32_t kRed = 0xFFFF0000u;
  Vertex verts[3]{};
  verts[0] = {256.0f * 0.25f, 256.0f * 0.25f, 0.5f, 1.0f, kRed};
  verts[1] = {256.0f * 0.75f, 256.0f * 0.25f, 0.5f, 1.0f, kRed};
  verts[2] = {256.0f * 0.50f, 256.0f * 0.75f, 0.5f, 1.0f, kRed};

  const uint16_t indices[3] = {0, 1, 2};

  D3DDDIARG_DRAWINDEXEDPRIMITIVE2 draw{};
  draw.PrimitiveType = D3DDDIPT_TRIANGLELIST;
  draw.PrimitiveCount = 1;
  draw.MinIndex = 0;
  draw.NumVertices = 3;
  draw.pIndexData = indices;
  draw.IndexDataFormat = kD3dFmtIndex16;
  draw.pVertexStreamZeroData = verts;
  draw.VertexStreamZeroStride = sizeof(Vertex);

  hr = cleanup.device_funcs.pfnDrawIndexedPrimitive2(create_dev.hDevice, &draw);
  if (!Check(hr == S_OK, "DrawIndexedPrimitive2")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }
  if (!Check(dev->up_vertex_buffer != nullptr, "up_vertex_buffer allocated")) {
    return false;
  }
  if (!Check(dev->up_index_buffer != nullptr, "up_index_buffer allocated")) {
    return false;
  }
  const aerogpu_handle_t vb_handle = dev->up_vertex_buffer->handle;
  const aerogpu_handle_t ib_handle = dev->up_index_buffer->handle;
  if (!Check(vb_handle != 0, "up_vertex_buffer handle non-zero")) {
    return false;
  }
  if (!Check(ib_handle != 0, "up_index_buffer handle non-zero")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  size_t vb_uploaded_bytes = 0;
  size_t ib_uploaded_bytes = 0;
  // Buffer uploads are padded to 4-byte alignment so host-side WebGPU copies
  // remain valid for non-4-byte-sized payloads (e.g. 3x u16 indices).
  const size_t expected_ib_bytes = AlignUp(sizeof(indices), 4);
  std::vector<uint8_t> ib_upload(expected_ib_bytes, 0);
  bool saw_set_ib = false;

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= len) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == AEROGPU_CMD_UPLOAD_RESOURCE) {
      const auto* upload = reinterpret_cast<const aerogpu_cmd_upload_resource*>(hdr);
      if (upload->resource_handle == vb_handle) {
        vb_uploaded_bytes += upload->size_bytes;
      }
      if (upload->resource_handle == ib_handle) {
        ib_uploaded_bytes += upload->size_bytes;
        const size_t payload_bytes = upload->size_bytes;
        if (!Check(upload->offset_bytes + payload_bytes <= expected_ib_bytes, "upload_resource(IB) bounds")) {
          return false;
        }
        if (!Check(sizeof(*upload) + payload_bytes <= hdr->size_bytes, "upload_resource(IB) payload bounds")) {
          return false;
        }

        const uint8_t* payload = reinterpret_cast<const uint8_t*>(upload) + sizeof(*upload);
        std::memcpy(ib_upload.data() + upload->offset_bytes, payload, payload_bytes);
      }
    } else if (hdr->opcode == AEROGPU_CMD_SET_INDEX_BUFFER) {
      const auto* set_ib = reinterpret_cast<const aerogpu_cmd_set_index_buffer*>(hdr);
      if (set_ib->buffer == ib_handle) {
        saw_set_ib = true;
        if (!Check(set_ib->format == AEROGPU_INDEX_FORMAT_UINT16, "set_index_buffer format")) {
          return false;
        }
        if (!Check(set_ib->offset_bytes == 0, "set_index_buffer offset")) {
          return false;
        }
      }
    }

    if (hdr->size_bytes == 0 || hdr->size_bytes > len - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }

  if (!Check(vb_uploaded_bytes == sizeof(verts), "VB upload emitted")) {
    return false;
  }
  if (!Check(ib_uploaded_bytes == expected_ib_bytes, "IB upload emitted (aligned)")) {
    return false;
  }
  if (!Check(saw_set_ib, "SET_INDEX_BUFFER emitted for UP IB")) {
    return false;
  }

  if (!Check(std::memcmp(ib_upload.data(), indices, sizeof(indices)) == 0, "IB upload payload matches indices")) {
    return false;
  }
  for (size_t i = sizeof(indices); i < expected_ib_bytes; ++i) {
    if (!Check(ib_upload[i] == 0, "IB upload padding is zero")) {
      return false;
    }
  }

  const CmdLoc draw_loc = FindLastOpcode(buf, len, AEROGPU_CMD_DRAW_INDEXED);
  if (!Check(draw_loc.hdr != nullptr, "DRAW_INDEXED emitted")) {
    return false;
  }
  const auto* draw_cmd = reinterpret_cast<const aerogpu_cmd_draw_indexed*>(draw_loc.hdr);
  if (!Check(draw_cmd->index_count == 3, "DRAW_INDEXED index_count")) {
    return false;
  }
  if (!Check(draw_cmd->first_index == 0, "DRAW_INDEXED first_index")) {
    return false;
  }
  return Check(draw_cmd->base_vertex == 0, "DRAW_INDEXED base_vertex");
}

bool TestFvfXyzrhwDiffuseDrawIndexedPrimitiveEmulationConvertsVertices() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE hVb{};
    D3DDDI_HRESOURCE hIb{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_vb = false;
    bool has_ib = false;

    ~Cleanup() {
      if (has_ib && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hIb);
      }
      if (has_vb && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hVb);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnSetFVF != nullptr, "SetFVF must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnCreateResource != nullptr, "CreateResource must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnLock != nullptr && cleanup.device_funcs.pfnUnlock != nullptr, "Lock/Unlock must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetStreamSource != nullptr, "SetStreamSource must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnSetIndices != nullptr, "SetIndices must be available")) {
    return false;
  }
  if (!Check(cleanup.device_funcs.pfnDrawIndexedPrimitive != nullptr, "DrawIndexedPrimitive must be available")) {
    return false;
  }

  D3DDDIVIEWPORTINFO vp{};
  vp.X = 0.0f;
  vp.Y = 0.0f;
  vp.Width = 256.0f;
  vp.Height = 256.0f;
  vp.MinZ = 0.0f;
  vp.MaxZ = 1.0f;
  hr = cleanup.device_funcs.pfnSetViewport(create_dev.hDevice, &vp);
  if (!Check(hr == S_OK, "SetViewport")) {
    return false;
  }

  // D3DFVF_XYZRHW (0x4) | D3DFVF_DIFFUSE (0x40).
  hr = cleanup.device_funcs.pfnSetFVF(create_dev.hDevice, 0x44u);
  if (!Check(hr == S_OK, "SetFVF(XYZRHW|DIFFUSE)")) {
    return false;
  }

  struct Vertex {
    float x;
    float y;
    float z;
    float rhw;
    uint32_t color;
  };

  constexpr uint32_t kBlue = 0xFF0000FFu;
  Vertex verts[3]{};
  verts[0] = {256.0f * 0.25f, 256.0f * 0.25f, 0.5f, 1.0f, kBlue};
  verts[1] = {256.0f * 0.75f, 256.0f * 0.25f, 0.5f, 1.0f, kBlue};
  verts[2] = {256.0f * 0.50f, 256.0f * 0.75f, 0.5f, 1.0f, kBlue};

  const uint16_t indices[3] = {0, 1, 2};

  // Create and fill VB.
  D3D9DDIARG_CREATERESOURCE create_vb{};
  create_vb.type = 0;
  create_vb.format = 0;
  create_vb.width = 0;
  create_vb.height = 0;
  create_vb.depth = 0;
  create_vb.mip_levels = 1;
  create_vb.usage = 0;
  create_vb.pool = 0;
  create_vb.size = sizeof(verts);
  create_vb.hResource.pDrvPrivate = nullptr;
  create_vb.pSharedHandle = nullptr;
  create_vb.pKmdAllocPrivateData = nullptr;
  create_vb.KmdAllocPrivateDataSize = 0;
  create_vb.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_vb);
  if (!Check(hr == S_OK, "CreateResource(vertex buffer)")) {
    return false;
  }
  cleanup.hVb = create_vb.hResource;
  cleanup.has_vb = true;

  D3D9DDIARG_LOCK lock{};
  lock.hResource = create_vb.hResource;
  lock.offset_bytes = 0;
  lock.size_bytes = 0;
  lock.flags = 0;
  D3DDDI_LOCKEDBOX box{};
  hr = cleanup.device_funcs.pfnLock(create_dev.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(vertex buffer)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock(VB) returns pData")) {
    return false;
  }
  std::memcpy(box.pData, verts, sizeof(verts));

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = create_vb.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(create_dev.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(vertex buffer)")) {
    return false;
  }

  // Create and fill IB.
  D3D9DDIARG_CREATERESOURCE create_ib{};
  create_ib.type = 0;
  create_ib.format = 0;
  create_ib.width = 0;
  create_ib.height = 0;
  create_ib.depth = 0;
  create_ib.mip_levels = 1;
  create_ib.usage = 0;
  create_ib.pool = 0;
  create_ib.size = sizeof(indices);
  create_ib.hResource.pDrvPrivate = nullptr;
  create_ib.pSharedHandle = nullptr;
  create_ib.pKmdAllocPrivateData = nullptr;
  create_ib.KmdAllocPrivateDataSize = 0;
  create_ib.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_ib);
  if (!Check(hr == S_OK, "CreateResource(index buffer)")) {
    return false;
  }
  cleanup.hIb = create_ib.hResource;
  cleanup.has_ib = true;

  lock.hResource = create_ib.hResource;
  lock.offset_bytes = 0;
  lock.size_bytes = 0;
  lock.flags = 0;
  std::memset(&box, 0, sizeof(box));
  hr = cleanup.device_funcs.pfnLock(create_dev.hDevice, &lock, &box);
  if (!Check(hr == S_OK, "Lock(index buffer)")) {
    return false;
  }
  if (!Check(box.pData != nullptr, "Lock(IB) returns pData")) {
    return false;
  }
  std::memcpy(box.pData, indices, sizeof(indices));

  unlock.hResource = create_ib.hResource;
  unlock.offset_bytes = 0;
  unlock.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(create_dev.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(index buffer)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(create_dev.hDevice, 0, create_vb.hResource, 0, sizeof(Vertex));
  if (!Check(hr == S_OK, "SetStreamSource")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetIndices(create_dev.hDevice, create_ib.hResource, kD3dFmtIndex16, 0);
  if (!Check(hr == S_OK, "SetIndices")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnDrawIndexedPrimitive(create_dev.hDevice,
                                                    D3DDDIPT_TRIANGLELIST,
                                                    /*base_vertex=*/0,
                                                    /*min_index=*/0,
                                                    /*num_vertices=*/3,
                                                    /*start_index=*/0,
                                                    /*primitive_count=*/1);
  if (!Check(hr == S_OK, "DrawIndexedPrimitive")) {
    return false;
  }

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }
  if (!Check(dev->up_vertex_buffer != nullptr, "up_vertex_buffer allocated")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  const CmdLoc upload = FindLastOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload.hdr != nullptr, "upload_resource emitted")) {
    return false;
  }
  const auto* upload_cmd = reinterpret_cast<const aerogpu_cmd_upload_resource*>(upload.hdr);
  if (!Check(upload_cmd->resource_handle == dev->up_vertex_buffer->handle, "upload_resource targets UP VB")) {
    return false;
  }
  if (!Check(upload_cmd->size_bytes == sizeof(verts), "upload_resource size matches expanded vertex data")) {
    return false;
  }

  const uint8_t* payload = reinterpret_cast<const uint8_t*>(upload_cmd) + sizeof(*upload_cmd);
  float x0 = 0.0f;
  float y0 = 0.0f;
  float z0 = 0.0f;
  float w0 = 0.0f;
  uint32_t c0 = 0;
  std::memcpy(&x0, payload + 0, sizeof(float));
  std::memcpy(&y0, payload + 4, sizeof(float));
  std::memcpy(&z0, payload + 8, sizeof(float));
  std::memcpy(&w0, payload + 12, sizeof(float));
  std::memcpy(&c0, payload + 16, sizeof(uint32_t));

  const float expected_x0 = ((verts[0].x + 0.5f - vp.X) / vp.Width) * 2.0f - 1.0f;
  const float expected_y0 = 1.0f - ((verts[0].y + 0.5f - vp.Y) / vp.Height) * 2.0f;
  if (!Check(std::fabs(x0 - expected_x0) < 1e-6f, "DrawIndexedPrimitive: x0 matches half-pixel convention")) {
    return false;
  }
  if (!Check(std::fabs(y0 - expected_y0) < 1e-6f, "DrawIndexedPrimitive: y0 matches half-pixel convention")) {
    return false;
  }
  if (!Check(std::fabs(z0 - verts[0].z) < 1e-6f, "DrawIndexedPrimitive: z preserved")) {
    return false;
  }
  if (!Check(std::fabs(w0 - 1.0f) < 1e-6f, "DrawIndexedPrimitive: w preserved")) {
    return false;
  }
  return Check(c0 == kBlue, "DrawIndexedPrimitive: diffuse color preserved");
}

bool TestResetShrinkUnbindsBackbuffer() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3D9DDI_HSWAPCHAIN hSwapChain{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_swapchain = false;

    ~Cleanup() {
      if (has_swapchain && device_funcs.pfnDestroySwapChain) {
        device_funcs.pfnDestroySwapChain(hDevice, hSwapChain);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  D3D9DDIARG_CREATESWAPCHAIN create_sc{};
  create_sc.present_params.backbuffer_width = 64;
  create_sc.present_params.backbuffer_height = 64;
  create_sc.present_params.backbuffer_format = 22u; // D3DFMT_X8R8G8B8
  create_sc.present_params.backbuffer_count = 2;
  create_sc.present_params.swap_effect = 1;
  create_sc.present_params.flags = 0;
  create_sc.present_params.hDeviceWindow = nullptr;
  create_sc.present_params.windowed = TRUE;
  create_sc.present_params.presentation_interval = 1;

  hr = cleanup.device_funcs.pfnCreateSwapChain(create_dev.hDevice, &create_sc);
  if (!Check(hr == S_OK, "CreateSwapChain")) {
    return false;
  }
  if (!Check(create_sc.hSwapChain.pDrvPrivate != nullptr, "CreateSwapChain returned swapchain handle")) {
    return false;
  }
  cleanup.hSwapChain = create_sc.hSwapChain;
  cleanup.has_swapchain = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* sc = reinterpret_cast<SwapChain*>(create_sc.hSwapChain.pDrvPrivate);
  if (!Check(sc->backbuffers.size() == 2, "swapchain has 2 backbuffers")) {
    return false;
  }

  Resource* bb0 = sc->backbuffers[0];
  Resource* bb1 = sc->backbuffers[1];

  D3DDDI_HRESOURCE hRt{};
  hRt.pDrvPrivate = bb1;
  hr = cleanup.device_funcs.pfnSetRenderTarget(create_dev.hDevice, 0, hRt);
  if (!Check(hr == S_OK, "SetRenderTarget(backbuffer1)")) {
    return false;
  }
  if (!Check(dev->render_targets[0] == bb1, "render target points at backbuffer1")) {
    return false;
  }

  D3D9DDIARG_RESET reset{};
  reset.present_params = create_sc.present_params;
  reset.present_params.backbuffer_count = 1;

  hr = cleanup.device_funcs.pfnReset(create_dev.hDevice, &reset);
  if (!Check(hr == S_OK, "Reset shrink")) {
    return false;
  }

  if (!Check(sc->backbuffers.size() == 1, "swapchain shrink to 1 backbuffer")) {
    return false;
  }
  if (!Check(dev->render_targets[0] == bb0, "render target rebounds to backbuffer0")) {
    return false;
  }
  return Check(dev->render_targets[0] != bb1, "render target no longer points at removed backbuffer");
}

bool TestRotateResourceIdentitiesRebindsChangedHandles() {
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
        for (auto& hRes : resources) {
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  auto create_buffer = [&](uint32_t size_bytes) -> D3DDDI_HRESOURCE {
    D3D9DDIARG_CREATERESOURCE args{};
    args.type = 0;
    args.format = 0;
    args.width = 0;
    args.height = 0;
    args.depth = 0;
    args.mip_levels = 1;
    args.usage = 0;
    args.pool = 0;
    args.size = size_bytes;
    args.hResource.pDrvPrivate = nullptr;
    args.pSharedHandle = nullptr;
    args.pKmdAllocPrivateData = nullptr;
    args.KmdAllocPrivateDataSize = 0;

    HRESULT hr_local = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &args);
    if (hr_local != S_OK) {
      std::fprintf(stderr, "FAIL: CreateResource(buffer) hr=0x%08x\n", static_cast<unsigned>(hr_local));
      return {};
    }
    cleanup.resources.push_back(args.hResource);
    return args.hResource;
  };

  auto create_surface = [&](uint32_t w, uint32_t h) -> D3DDDI_HRESOURCE {
    D3D9DDIARG_CREATERESOURCE args{};
    args.type = 0;
    args.format = 22u; // D3DFMT_X8R8G8B8
    args.width = w;
    args.height = h;
    args.depth = 1;
    args.mip_levels = 1;
    args.usage = 0;
    args.pool = 0;
    args.size = 0;
    args.hResource.pDrvPrivate = nullptr;
    args.pSharedHandle = nullptr;
    args.pKmdAllocPrivateData = nullptr;
    args.KmdAllocPrivateDataSize = 0;

    HRESULT hr_local = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &args);
    if (hr_local != S_OK) {
      std::fprintf(stderr, "FAIL: CreateResource(surface) hr=0x%08x\n", static_cast<unsigned>(hr_local));
      return {};
    }
    cleanup.resources.push_back(args.hResource);
    return args.hResource;
  };

  D3DDDI_HRESOURCE hVb0 = create_buffer(256);
  D3DDDI_HRESOURCE hVb1 = create_buffer(256);
  if (!Check(hVb0.pDrvPrivate != nullptr && hVb1.pDrvPrivate != nullptr, "vertex buffers created")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(create_dev.hDevice, 0, hVb0, 0, 16);
  if (!Check(hr == S_OK, "SetStreamSource")) {
    return false;
  }

  D3DDDI_HRESOURCE hTex0 = create_surface(32, 32);
  D3DDDI_HRESOURCE hTex1 = create_surface(32, 32);
  if (!Check(hTex0.pDrvPrivate != nullptr && hTex1.pDrvPrivate != nullptr, "textures created")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(create_dev.hDevice, 0, hTex0);
  if (!Check(hr == S_OK, "SetTexture")) {
    return false;
  }

  D3DDDI_HRESOURCE hIb0 = create_buffer(128);
  D3DDDI_HRESOURCE hIb1 = create_buffer(128);
  if (!Check(hIb0.pDrvPrivate != nullptr && hIb1.pDrvPrivate != nullptr, "index buffers created")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetIndices(create_dev.hDevice, hIb0, kD3dFmtIndex16, 4);
  if (!Check(hr == S_OK, "SetIndices")) {
    return false;
  }

  auto reset_stream = [&]() {
    std::lock_guard<std::mutex> lock(dev->mutex);
    dev->cmd.reset();
  };

  // Rotate vertex buffers: must re-emit SET_VERTEX_BUFFERS for stream 0 using the
  // new handle.
  reset_stream();
  auto* vb0 = reinterpret_cast<Resource*>(hVb0.pDrvPrivate);
  auto* vb1 = reinterpret_cast<Resource*>(hVb1.pDrvPrivate);
  vb0->backing_alloc_id = 101;
  vb1->backing_alloc_id = 202;
  vb0->backing_offset_bytes = 1;
  vb1->backing_offset_bytes = 2;
  vb0->wddm_hAllocation = 0x101;
  vb1->wddm_hAllocation = 0x202;
  vb0->storage[0] = 0xA0;
  vb1->storage[0] = 0xB0;
  const aerogpu_handle_t vb0_before = vb0->handle;
  const aerogpu_handle_t vb1_before = vb1->handle;
  D3DDDI_HRESOURCE vb_rotate[2] = {hVb0, hVb1};
  hr = cleanup.device_funcs.pfnRotateResourceIdentities(create_dev.hDevice, vb_rotate, 2);
  if (!Check(hr == S_OK, "RotateResourceIdentities(vb)")) {
    return false;
  }
  if (!Check(vb0->handle == vb1_before && vb1->handle == vb0_before, "vertex buffer handles rotated")) {
    return false;
  }
  if (!Check(vb0->backing_alloc_id == 202 && vb1->backing_alloc_id == 101, "vertex buffer alloc_id rotated")) {
    return false;
  }
  if (!Check(vb0->backing_offset_bytes == 2 && vb1->backing_offset_bytes == 1, "vertex buffer backing_offset_bytes rotated")) {
    return false;
  }
  if (!Check(vb0->wddm_hAllocation == 0x202 && vb1->wddm_hAllocation == 0x101, "vertex buffer hAllocation rotated")) {
    return false;
  }
  if (!Check(vb0->storage[0] == 0xB0 && vb1->storage[0] == 0xA0, "vertex buffer storage rotated")) {
    return false;
  }

  dev->cmd.finalize();
  {
    const CmdLoc loc = FindLastOpcode(dev->cmd.data(), dev->cmd.bytes_used(), AEROGPU_CMD_SET_VERTEX_BUFFERS);
    if (!Check(loc.hdr != nullptr, "SET_VERTEX_BUFFERS emitted after rotate")) {
      return false;
    }
    const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_vertex_buffers*>(loc.hdr);
    if (!Check(cmd->start_slot == 0 && cmd->buffer_count == 1, "SET_VERTEX_BUFFERS header fields")) {
      return false;
    }
    const auto* binding = reinterpret_cast<const aerogpu_vertex_buffer_binding*>(
        reinterpret_cast<const uint8_t*>(cmd) + sizeof(*cmd));
    if (!Check(binding->buffer == vb0->handle, "SET_VERTEX_BUFFERS uses rotated handle")) {
      return false;
    }
  }

  // Rotate textures: must re-emit SET_TEXTURE for stage 0 using the new handle.
  reset_stream();
  auto* tex0 = reinterpret_cast<Resource*>(hTex0.pDrvPrivate);
  auto* tex1 = reinterpret_cast<Resource*>(hTex1.pDrvPrivate);
  tex0->backing_alloc_id = 303;
  tex1->backing_alloc_id = 404;
  tex0->backing_offset_bytes = 3;
  tex1->backing_offset_bytes = 4;
  tex0->wddm_hAllocation = 0x303;
  tex1->wddm_hAllocation = 0x404;
  tex0->storage[0] = 0xC0;
  tex1->storage[0] = 0xD0;
  const aerogpu_handle_t tex0_before = tex0->handle;
  const aerogpu_handle_t tex1_before = tex1->handle;
  D3DDDI_HRESOURCE tex_rotate[2] = {hTex0, hTex1};
  hr = cleanup.device_funcs.pfnRotateResourceIdentities(create_dev.hDevice, tex_rotate, 2);
  if (!Check(hr == S_OK, "RotateResourceIdentities(tex)")) {
    return false;
  }
  if (!Check(tex0->handle == tex1_before && tex1->handle == tex0_before, "texture handles rotated")) {
    return false;
  }
  if (!Check(tex0->backing_alloc_id == 404 && tex1->backing_alloc_id == 303, "texture alloc_id rotated")) {
    return false;
  }
  if (!Check(tex0->backing_offset_bytes == 4 && tex1->backing_offset_bytes == 3, "texture backing_offset_bytes rotated")) {
    return false;
  }
  if (!Check(tex0->wddm_hAllocation == 0x404 && tex1->wddm_hAllocation == 0x303, "texture hAllocation rotated")) {
    return false;
  }
  if (!Check(tex0->storage[0] == 0xD0 && tex1->storage[0] == 0xC0, "texture storage rotated")) {
    return false;
  }

  dev->cmd.finalize();
  {
    const CmdLoc loc = FindLastOpcode(dev->cmd.data(), dev->cmd.bytes_used(), AEROGPU_CMD_SET_TEXTURE);
    if (!Check(loc.hdr != nullptr, "SET_TEXTURE emitted after rotate")) {
      return false;
    }
    const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_texture*>(loc.hdr);
    if (!Check(cmd->slot == 0 && cmd->texture == tex0->handle, "SET_TEXTURE uses rotated handle")) {
      return false;
    }
  }

  // Rotate index buffers: must re-emit SET_INDEX_BUFFER with the new handle.
  reset_stream();
  auto* ib0 = reinterpret_cast<Resource*>(hIb0.pDrvPrivate);
  auto* ib1 = reinterpret_cast<Resource*>(hIb1.pDrvPrivate);
  ib0->backing_alloc_id = 505;
  ib1->backing_alloc_id = 606;
  ib0->backing_offset_bytes = 5;
  ib1->backing_offset_bytes = 6;
  ib0->wddm_hAllocation = 0x505;
  ib1->wddm_hAllocation = 0x606;
  ib0->storage[0] = 0xE0;
  ib1->storage[0] = 0xF0;
  const aerogpu_handle_t ib0_before = ib0->handle;
  const aerogpu_handle_t ib1_before = ib1->handle;
  D3DDDI_HRESOURCE ib_rotate[2] = {hIb0, hIb1};
  hr = cleanup.device_funcs.pfnRotateResourceIdentities(create_dev.hDevice, ib_rotate, 2);
  if (!Check(hr == S_OK, "RotateResourceIdentities(ib)")) {
    return false;
  }
  if (!Check(ib0->handle == ib1_before && ib1->handle == ib0_before, "index buffer handles rotated")) {
    return false;
  }
  if (!Check(ib0->backing_alloc_id == 606 && ib1->backing_alloc_id == 505, "index buffer alloc_id rotated")) {
    return false;
  }
  if (!Check(ib0->backing_offset_bytes == 6 && ib1->backing_offset_bytes == 5, "index buffer backing_offset_bytes rotated")) {
    return false;
  }
  if (!Check(ib0->wddm_hAllocation == 0x606 && ib1->wddm_hAllocation == 0x505, "index buffer hAllocation rotated")) {
    return false;
  }
  if (!Check(ib0->storage[0] == 0xF0 && ib1->storage[0] == 0xE0, "index buffer storage rotated")) {
    return false;
  }

  dev->cmd.finalize();
  {
    const CmdLoc loc = FindLastOpcode(dev->cmd.data(), dev->cmd.bytes_used(), AEROGPU_CMD_SET_INDEX_BUFFER);
    if (!Check(loc.hdr != nullptr, "SET_INDEX_BUFFER emitted after rotate")) {
      return false;
    }
    const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_index_buffer*>(loc.hdr);
    if (!Check(cmd->buffer == ib0->handle, "SET_INDEX_BUFFER uses rotated handle")) {
      return false;
    }
    if (!Check(cmd->offset_bytes == 4, "SET_INDEX_BUFFER preserves offset")) {
      return false;
    }
  }

  return true;
}

bool TestPresentBackbufferRotationUndoOnSmallCmdBuffer() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3D9DDI_HSWAPCHAIN hSwapChain{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_swapchain = false;

    ~Cleanup() {
      if (has_swapchain && device_funcs.pfnDestroySwapChain) {
        device_funcs.pfnDestroySwapChain(hDevice, hSwapChain);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  D3D9DDIARG_CREATESWAPCHAIN create_sc{};
  create_sc.present_params.backbuffer_width = 64;
  create_sc.present_params.backbuffer_height = 64;
  create_sc.present_params.backbuffer_format = 22u; // D3DFMT_X8R8G8B8
  create_sc.present_params.backbuffer_count = 2;
  create_sc.present_params.swap_effect = 1;
  create_sc.present_params.flags = 0;
  create_sc.present_params.hDeviceWindow = nullptr;
  create_sc.present_params.windowed = TRUE;
  create_sc.present_params.presentation_interval = 0;

  hr = cleanup.device_funcs.pfnCreateSwapChain(create_dev.hDevice, &create_sc);
  if (!Check(hr == S_OK, "CreateSwapChain")) {
    return false;
  }
  if (!Check(create_sc.hSwapChain.pDrvPrivate != nullptr, "CreateSwapChain returned swapchain handle")) {
    return false;
  }
  cleanup.hSwapChain = create_sc.hSwapChain;
  cleanup.has_swapchain = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* sc = reinterpret_cast<SwapChain*>(create_sc.hSwapChain.pDrvPrivate);
  if (!Check(sc->backbuffers.size() == 2, "swapchain has 2 backbuffers")) {
    return false;
  }

  const aerogpu_handle_t h0 = sc->backbuffers[0]->handle;
  const aerogpu_handle_t h1 = sc->backbuffers[1]->handle;

  D3D9DDIARG_PRESENTEX present{};
  present.hSrc.pDrvPrivate = nullptr;
  present.hWnd = nullptr;
  present.sync_interval = 0;
  present.d3d9_present_flags = 0;

  // Small span-backed DMA buffer: PresentEx fits, but the post-submit render-target
  // rebind used by flip-style backbuffer rotation does not.
  uint8_t small_dma[sizeof(aerogpu_cmd_stream_header) + 32] = {};
  dev->cmd.set_span(small_dma, sizeof(small_dma));

  hr = cleanup.device_funcs.pfnPresentEx(create_dev.hDevice, &present);
  if (!Check(hr == S_OK, "PresentEx (small cmd buffer)")) {
    return false;
  }
  if (!Check(sc->backbuffers[0]->handle == h0 && sc->backbuffers[1]->handle == h1,
             "present rotation undone when RT rebind cannot be emitted")) {
    return false;
  }

  // Vector-backed buffer: rotation should succeed and swap handles.
  dev->cmd.set_vector();
  hr = cleanup.device_funcs.pfnPresentEx(create_dev.hDevice, &present);
  if (!Check(hr == S_OK, "PresentEx (vector cmd buffer)")) {
    return false;
  }
  return Check(sc->backbuffers[0]->handle == h1 && sc->backbuffers[1]->handle == h0,
               "present rotation occurs when RT rebind succeeds");
}

bool TestPresentBackbufferRotationUndoOnSmallAllocList() {
  // Backbuffer rotation rebinding can touch multiple guest-backed allocations
  // (render target + bound textures). If the allocation list cannot fit all
  // referenced allocations, the UMD must undo the rotation rather than emit
  // commands with an incomplete allocation table.
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3D9DDI_HSWAPCHAIN hSwapChain{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_swapchain = false;

    ~Cleanup() {
      if (has_swapchain && device_funcs.pfnDestroySwapChain) {
        device_funcs.pfnDestroySwapChain(hDevice, hSwapChain);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  if (!Check(cleanup.device_funcs.pfnPresentEx != nullptr, "PresentEx must be available")) {
    return false;
  }

  D3D9DDIARG_CREATESWAPCHAIN create_sc{};
  create_sc.present_params.backbuffer_width = 64;
  create_sc.present_params.backbuffer_height = 64;
  create_sc.present_params.backbuffer_format = 22u; // D3DFMT_X8R8G8B8
  create_sc.present_params.backbuffer_count = 2;
  create_sc.present_params.swap_effect = 1;
  create_sc.present_params.flags = 0;
  create_sc.present_params.hDeviceWindow = nullptr;
  create_sc.present_params.windowed = TRUE;
  create_sc.present_params.presentation_interval = 0;

  hr = cleanup.device_funcs.pfnCreateSwapChain(create_dev.hDevice, &create_sc);
  if (!Check(hr == S_OK, "CreateSwapChain")) {
    return false;
  }
  cleanup.hSwapChain = create_sc.hSwapChain;
  cleanup.has_swapchain = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* sc = reinterpret_cast<SwapChain*>(create_sc.hSwapChain.pDrvPrivate);
  if (!Check(dev && sc, "swapchain/device pointers")) {
    return false;
  }
  if (!Check(sc->backbuffers.size() == 2, "swapchain has 2 backbuffers")) {
    return false;
  }

  const aerogpu_handle_t h0 = sc->backbuffers[0]->handle;
  const aerogpu_handle_t h1 = sc->backbuffers[1]->handle;

  dev->wddm_context.hContext = 1;
  D3DDDI_ALLOCATIONLIST alloc_list[1] = {};
  dev->alloc_list_tracker.rebind(alloc_list, 1, 0xFFFFu);
  dev->alloc_list_tracker.reset();

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    // Ensure the rebinding sequence references two distinct alloc-backed
    // resources: RT0 = backbuffer0, texture0 = backbuffer1.
    sc->backbuffers[0]->backing_alloc_id = 1;
    sc->backbuffers[0]->wddm_hAllocation = 0x1111u;
    sc->backbuffers[1]->backing_alloc_id = 2;
    sc->backbuffers[1]->wddm_hAllocation = 0x2222u;

    dev->render_targets[0] = sc->backbuffers[0];
    dev->render_targets[1] = nullptr;
    dev->render_targets[2] = nullptr;
    dev->render_targets[3] = nullptr;
    dev->textures[0] = sc->backbuffers[1];
    for (uint32_t i = 1; i < 16; ++i) {
      dev->textures[i] = nullptr;
    }
  }

  D3D9DDIARG_PRESENTEX present{};
  present.hSrc.pDrvPrivate = nullptr;
  present.hWnd = nullptr;
  present.sync_interval = 0;
  present.d3d9_present_flags = 0;

  hr = cleanup.device_funcs.pfnPresentEx(create_dev.hDevice, &present);
  if (!Check(hr == S_OK, "PresentEx (small alloc list)")) {
    return false;
  }

  if (!Check(sc->backbuffers[0]->handle == h0 && sc->backbuffers[1]->handle == h1,
             "present rotation undone when alloc list cannot fit rebind deps")) {
    return false;
  }
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->alloc_list_tracker.list_len() == 0, "allocation list cleared when present rotation undone")) {
      return false;
    }
  }
  return true;
}

bool TestPresentBackbufferRotationRebindsBackbufferTexture() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3D9DDI_HSWAPCHAIN hSwapChain{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_swapchain = false;

    ~Cleanup() {
      if (has_swapchain && device_funcs.pfnDestroySwapChain) {
        device_funcs.pfnDestroySwapChain(hDevice, hSwapChain);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  D3D9DDIARG_CREATESWAPCHAIN create_sc{};
  create_sc.present_params.backbuffer_width = 64;
  create_sc.present_params.backbuffer_height = 64;
  create_sc.present_params.backbuffer_format = 22u; // D3DFMT_X8R8G8B8
  create_sc.present_params.backbuffer_count = 2;
  create_sc.present_params.swap_effect = 1;
  create_sc.present_params.flags = 0;
  create_sc.present_params.hDeviceWindow = nullptr;
  create_sc.present_params.windowed = TRUE;
  create_sc.present_params.presentation_interval = 0;

  hr = cleanup.device_funcs.pfnCreateSwapChain(create_dev.hDevice, &create_sc);
  if (!Check(hr == S_OK, "CreateSwapChain")) {
    return false;
  }
  cleanup.hSwapChain = create_sc.hSwapChain;
  cleanup.has_swapchain = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* sc = reinterpret_cast<SwapChain*>(create_sc.hSwapChain.pDrvPrivate);
  if (!Check(dev && sc, "swapchain/device pointers")) {
    return false;
  }
  if (!Check(sc->backbuffers.size() == 2, "swapchain has 2 backbuffers")) {
    return false;
  }

  const aerogpu_handle_t h0 = sc->backbuffers[0]->handle;
  const aerogpu_handle_t h1 = sc->backbuffers[1]->handle;

  D3DDDI_HRESOURCE hTex{};
  hTex.pDrvPrivate = sc->backbuffers[0];

  D3D9DDIARG_PRESENTEX present{};
  present.hSrc.pDrvPrivate = nullptr;
  present.hWnd = nullptr;
  present.sync_interval = 0;
  present.d3d9_present_flags = 0;

  // Small span-backed DMA buffer. PresentEx itself fits, and SET_RENDER_TARGETS
  // fits, but SET_RENDER_TARGETS + the required SET_TEXTURE rebind does not.
  uint8_t small_dma[sizeof(aerogpu_cmd_stream_header) + 64] = {};
  dev->cmd.set_span(small_dma, sizeof(small_dma));

  hr = cleanup.device_funcs.pfnSetTexture(create_dev.hDevice, 0, hTex);
  if (!Check(hr == S_OK, "SetTexture(backbuffer)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnPresentEx(create_dev.hDevice, &present);
  if (!Check(hr == S_OK, "PresentEx (small cmd buffer)")) {
    return false;
  }

  if (!Check(sc->backbuffers[0]->handle == h0 && sc->backbuffers[1]->handle == h1,
             "present rotation undone when texture rebind cannot be emitted")) {
    return false;
  }

  // Vector-backed buffer: rotation should succeed and emit a SET_TEXTURE rebind
  // that references the rotated handle.
  dev->cmd.set_vector();
  hr = cleanup.device_funcs.pfnSetTexture(create_dev.hDevice, 0, hTex);
  if (!Check(hr == S_OK, "SetTexture(backbuffer) (vector)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnPresentEx(create_dev.hDevice, &present);
  if (!Check(hr == S_OK, "PresentEx (vector cmd buffer)")) {
    return false;
  }

  if (!Check(sc->backbuffers[0]->handle == h1 && sc->backbuffers[1]->handle == h0,
             "present rotation occurs when rebind succeeds")) {
    return false;
  }

  dev->cmd.finalize();
  const CmdLoc loc = FindLastOpcode(dev->cmd.data(), dev->cmd.bytes_used(), AEROGPU_CMD_SET_TEXTURE);
  if (!Check(loc.hdr != nullptr, "SET_TEXTURE emitted after present rotation")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_texture*>(loc.hdr);
  if (!Check(cmd->slot == 0, "SET_TEXTURE slot 0")) {
    return false;
  }
  return Check(cmd->texture == sc->backbuffers[0]->handle, "SET_TEXTURE uses rotated backbuffer handle");
}

bool TestSetRenderTargetRejectsGaps() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3D9DDI_HSWAPCHAIN hSwapChain{};
    D3DDDI_HRESOURCE hResource{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_swapchain = false;
    bool has_resource = false;

    ~Cleanup() {
      if (has_resource && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hResource);
      }
      if (has_swapchain && device_funcs.pfnDestroySwapChain) {
        device_funcs.pfnDestroySwapChain(hDevice, hSwapChain);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  D3D9DDIARG_CREATESWAPCHAIN create_sc{};
  create_sc.present_params.backbuffer_width = 64;
  create_sc.present_params.backbuffer_height = 64;
  create_sc.present_params.backbuffer_format = 22u; // D3DFMT_X8R8G8B8
  create_sc.present_params.backbuffer_count = 1;
  create_sc.present_params.swap_effect = 1;
  create_sc.present_params.flags = 0;
  create_sc.present_params.hDeviceWindow = nullptr;
  create_sc.present_params.windowed = TRUE;
  create_sc.present_params.presentation_interval = 1;

  hr = cleanup.device_funcs.pfnCreateSwapChain(create_dev.hDevice, &create_sc);
  if (!Check(hr == S_OK, "CreateSwapChain")) {
    return false;
  }
  cleanup.hSwapChain = create_sc.hSwapChain;
  cleanup.has_swapchain = true;

  D3D9DDIARG_CREATERESOURCE create_rt{};
  create_rt.type = 0;
  create_rt.format = 22u; // D3DFMT_X8R8G8B8
  create_rt.width = 16;
  create_rt.height = 16;
  create_rt.depth = 1;
  create_rt.mip_levels = 1;
  create_rt.usage = 1u; // D3DUSAGE_RENDERTARGET
  create_rt.pool = 0;
  create_rt.size = 0;
  create_rt.hResource.pDrvPrivate = nullptr;
  create_rt.pSharedHandle = nullptr;
  create_rt.pKmdAllocPrivateData = nullptr;
  create_rt.KmdAllocPrivateDataSize = 0;
  create_rt.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_rt);
  if (!Check(hr == S_OK, "CreateResource(render target)")) {
    return false;
  }
  cleanup.hResource = create_rt.hResource;
  cleanup.has_resource = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->render_targets[0] != nullptr, "render target 0 bound by swapchain")) {
      return false;
    }
    if (!Check(dev->render_targets[1] == nullptr, "render target 1 initially null")) {
      return false;
    }
    if (!Check(dev->render_targets[2] == nullptr, "render target 2 initially null")) {
      return false;
    }
    dev->cmd.reset();
  }

  // Binding slot 2 while slot 1 is null creates a gap. The host rejects gapped
  // SET_RENDER_TARGETS commands, so the UMD should reject this call.
  hr = cleanup.device_funcs.pfnSetRenderTarget(create_dev.hDevice, 2, create_rt.hResource);
  if (!Check(hr == kD3DErrInvalidCall, "SetRenderTarget rejects gaps")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    if (!Check(dev->render_targets[2] == nullptr, "render target 2 not cached on invalid call")) {
      return false;
    }
  }

  dev->cmd.finalize();
  const CmdLoc loc = FindLastOpcode(dev->cmd.data(), dev->cmd.bytes_used(), AEROGPU_CMD_SET_RENDER_TARGETS);
  return Check(loc.hdr == nullptr, "no SET_RENDER_TARGETS emitted for invalid gap binding");
}

bool TestRotateResourceIdentitiesUndoOnSmallCmdBuffer() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE resources[2]{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_resources = false;

    ~Cleanup() {
      if (has_resources && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, resources[0]);
        device_funcs.pfnDestroyResource(hDevice, resources[1]);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  D3D9DDIARG_CREATERESOURCE create_res{};
  create_res.type = 0;
  create_res.format = 22u; // D3DFMT_X8R8G8B8
  create_res.width = 16;
  create_res.height = 16;
  create_res.depth = 1;
  create_res.mip_levels = 1;
  create_res.usage = 0;
  create_res.pool = 0;
  create_res.size = 0;
  create_res.hResource.pDrvPrivate = nullptr;
  create_res.pSharedHandle = nullptr;
  create_res.pPrivateDriverData = nullptr;
  create_res.PrivateDriverDataSize = 0;
  create_res.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_res);
  if (!Check(hr == S_OK, "CreateResource(0)")) {
    return false;
  }
  cleanup.resources[0] = create_res.hResource;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_res);
  if (!Check(hr == S_OK, "CreateResource(1)")) {
    return false;
  }
  cleanup.resources[1] = create_res.hResource;
  cleanup.has_resources = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* res0 = reinterpret_cast<Resource*>(cleanup.resources[0].pDrvPrivate);
  auto* res1 = reinterpret_cast<Resource*>(cleanup.resources[1].pDrvPrivate);

  const aerogpu_handle_t h0 = res0->handle;
  const aerogpu_handle_t h1 = res1->handle;
  res0->backing_alloc_id = 111;
  res1->backing_alloc_id = 222;
  res0->backing_offset_bytes = 4;
  res1->backing_offset_bytes = 8;
  res0->wddm_hAllocation = 0xABC;
  res1->wddm_hAllocation = 0xDEF;
  if (!res0->storage.empty()) {
    res0->storage[0] = 0xA1;
  }
  if (!res1->storage.empty()) {
    res1->storage[0] = 0xB2;
  }
  res0->shared_private_driver_data = {0x01, 0x02, 0x03};
  res1->shared_private_driver_data = {0x04, 0x05};

  // Too small for SET_RENDER_TARGETS (48 bytes), so rotate should fail and restore.
  uint8_t small_dma[sizeof(aerogpu_cmd_stream_header) + 32] = {};
  dev->cmd.set_span(small_dma, sizeof(small_dma));

  hr = cleanup.device_funcs.pfnRotateResourceIdentities(create_dev.hDevice, cleanup.resources, 2);
  if (!Check(hr == E_OUTOFMEMORY, "RotateResourceIdentities returns E_OUTOFMEMORY on small cmd buffer")) {
    return false;
  }
  if (!Check(res0->handle == h0 && res1->handle == h1, "rotate identities restored handles on failure")) {
    return false;
  }
  if (!Check(res0->backing_alloc_id == 111 && res1->backing_alloc_id == 222, "rotate identities restored alloc_id on failure")) {
    return false;
  }
  if (!Check(res0->backing_offset_bytes == 4 && res1->backing_offset_bytes == 8,
             "rotate identities restored backing_offset_bytes on failure")) {
    return false;
  }
  if (!Check(res0->wddm_hAllocation == 0xABC && res1->wddm_hAllocation == 0xDEF, "rotate identities restored hAllocation on failure")) {
    return false;
  }
  if (!Check(!res0->storage.empty() && res0->storage[0] == 0xA1, "rotate identities restored storage[0] for res0 on failure")) {
    return false;
  }
  if (!Check(!res1->storage.empty() && res1->storage[0] == 0xB2, "rotate identities restored storage[0] for res1 on failure")) {
    return false;
  }
  if (!Check(res0->shared_private_driver_data.size() == 3 && res0->shared_private_driver_data[0] == 0x01,
             "rotate identities restored shared_private_driver_data for res0 on failure")) {
    return false;
  }
  if (!Check(res1->shared_private_driver_data.size() == 2 && res1->shared_private_driver_data[0] == 0x04,
             "rotate identities restored shared_private_driver_data for res1 on failure")) {
    return false;
  }

  dev->cmd.set_vector();
  hr = cleanup.device_funcs.pfnRotateResourceIdentities(create_dev.hDevice, cleanup.resources, 2);
  if (!Check(hr == S_OK, "RotateResourceIdentities succeeds with vector cmd buffer")) {
    return false;
  }
  if (!Check(res0->handle == h1 && res1->handle == h0, "rotate identities swaps handles on success")) {
    return false;
  }
  if (!Check(res0->backing_alloc_id == 222 && res1->backing_alloc_id == 111, "rotate identities swaps alloc_id on success")) {
    return false;
  }
  if (!Check(res0->backing_offset_bytes == 8 && res1->backing_offset_bytes == 4,
             "rotate identities swaps backing_offset_bytes on success")) {
    return false;
  }
  if (!Check(res0->wddm_hAllocation == 0xDEF && res1->wddm_hAllocation == 0xABC, "rotate identities swaps hAllocation on success")) {
    return false;
  }
  if (!Check(!res0->storage.empty() && res0->storage[0] == 0xB2, "rotate identities swaps storage[0] for res0 on success")) {
    return false;
  }
  if (!Check(!res1->storage.empty() && res1->storage[0] == 0xA1, "rotate identities swaps storage[0] for res1 on success")) {
    return false;
  }
  if (!Check(res0->shared_private_driver_data.size() == 2 && res0->shared_private_driver_data[0] == 0x04,
             "rotate identities swaps shared_private_driver_data for res0 on success")) {
    return false;
  }
  return Check(res1->shared_private_driver_data.size() == 3 && res1->shared_private_driver_data[0] == 0x01,
               "rotate identities swaps shared_private_driver_data for res1 on success");
}

bool TestResetRebindsBackbufferTexture() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3D9DDI_HSWAPCHAIN hSwapChain{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_swapchain = false;

    ~Cleanup() {
      if (has_swapchain && device_funcs.pfnDestroySwapChain) {
        device_funcs.pfnDestroySwapChain(hDevice, hSwapChain);
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  D3D9DDIARG_CREATESWAPCHAIN create_sc{};
  create_sc.present_params.backbuffer_width = 64;
  create_sc.present_params.backbuffer_height = 64;
  create_sc.present_params.backbuffer_format = 22u; // D3DFMT_X8R8G8B8
  create_sc.present_params.backbuffer_count = 1;
  create_sc.present_params.swap_effect = 1;
  create_sc.present_params.flags = 0;
  create_sc.present_params.hDeviceWindow = nullptr;
  create_sc.present_params.windowed = TRUE;
  create_sc.present_params.presentation_interval = 1;

  hr = cleanup.device_funcs.pfnCreateSwapChain(create_dev.hDevice, &create_sc);
  if (!Check(hr == S_OK, "CreateSwapChain")) {
    return false;
  }
  cleanup.hSwapChain = create_sc.hSwapChain;
  cleanup.has_swapchain = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* sc = reinterpret_cast<SwapChain*>(create_sc.hSwapChain.pDrvPrivate);
  auto* bb = reinterpret_cast<Resource*>(create_sc.hBackBuffer.pDrvPrivate);
  if (!Check(dev && sc && bb, "swapchain/device pointers")) {
    return false;
  }
  if (!Check(!sc->backbuffers.empty() && sc->backbuffers[0] == bb, "backbuffer[0]")) {
    return false;
  }

  const aerogpu_handle_t old_handle = bb->handle;

  D3DDDI_HRESOURCE hTex{};
  hTex.pDrvPrivate = bb;
  hr = cleanup.device_funcs.pfnSetTexture(create_dev.hDevice, 0, hTex);
  if (!Check(hr == S_OK, "SetTexture(backbuffer)")) {
    return false;
  }

  D3D9DDIARG_RESET reset{};
  reset.present_params = create_sc.present_params;
  hr = cleanup.device_funcs.pfnReset(create_dev.hDevice, &reset);
  if (!Check(hr == S_OK, "Reset")) {
    return false;
  }

  const aerogpu_handle_t new_handle = bb->handle;
  if (!Check(new_handle != old_handle, "Reset recreates backbuffer handle")) {
    return false;
  }

  dev->cmd.finalize();
  const CmdLoc loc = FindLastOpcode(dev->cmd.data(), dev->cmd.bytes_used(), AEROGPU_CMD_SET_TEXTURE);
  if (!Check(loc.hdr != nullptr, "SET_TEXTURE emitted after reset")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_set_texture*>(loc.hdr);
  if (!Check(cmd->slot == 0, "SET_TEXTURE slot 0")) {
    return false;
  }
  return Check(cmd->texture == new_handle, "SET_TEXTURE uses recreated backbuffer handle");
}

bool TestOpenResourceTracksWddmAllocationHandle() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE hResource{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_resource = false;

    ~Cleanup() {
      if (has_resource && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hResource);
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

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Simulate a WDDM-enabled device so allocation-list tracking is active in
  // portable builds.
  dev->wddm_context.hContext = 1;
  D3DDDI_ALLOCATIONLIST list[4] = {};
  dev->alloc_list_tracker.rebind(list, 4, 0xFFFFu);

  aerogpu_wddm_alloc_priv priv{};
  priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
  priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
  priv.alloc_id = 0x1234u;
  priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;
  priv.share_token = 0x1122334455667788ull;
  priv.size_bytes = 64ull * 64ull * 4ull;
  priv.reserved0 = AEROGPU_WDDM_ALLOC_PRIV_DESC_PACK(/*format=*/22u, /*width=*/64u, /*height=*/64u);

  D3D9DDIARG_OPENRESOURCE open_res{};
  open_res.pPrivateDriverData = &priv;
  open_res.private_driver_data_size = sizeof(priv);
  open_res.type = 0;
  open_res.format = 0; // reconstructed from alloc priv desc
  open_res.width = 0;
  open_res.height = 0;
  open_res.depth = 1;
  open_res.mip_levels = 1;
  open_res.usage = 0;
  open_res.size = 0;
  open_res.hResource.pDrvPrivate = nullptr;
  open_res.wddm_hAllocation = 0xABCDu;

  hr = cleanup.device_funcs.pfnOpenResource(create_dev.hDevice, &open_res);
  if (!Check(hr == S_OK, "OpenResource")) {
    return false;
  }
  if (!Check(open_res.hResource.pDrvPrivate != nullptr, "OpenResource returned resource handle")) {
    return false;
  }

  cleanup.hResource = open_res.hResource;
  cleanup.has_resource = true;

  auto* res = reinterpret_cast<Resource*>(open_res.hResource.pDrvPrivate);
  if (!Check(res != nullptr, "resource pointer")) {
    return false;
  }
  if (!Check(res->backing_alloc_id == priv.alloc_id, "OpenResource preserves alloc_id from private data")) {
    return false;
  }
  if (!Check(res->wddm_hAllocation == open_res.wddm_hAllocation, "OpenResource captures WDDM hAllocation")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetRenderTarget(create_dev.hDevice, 0, open_res.hResource);
  if (!Check(hr == S_OK, "SetRenderTarget(opened resource)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnClear(create_dev.hDevice,
                                     /*flags=*/0x1u,
                                     /*color_rgba8=*/0xFF00FF00u,
                                     /*depth=*/1.0f,
                                     /*stencil=*/0);
  if (!Check(hr == S_OK, "Clear tracks allocation list")) {
    return false;
  }

  if (!Check(dev->alloc_list_tracker.list_len() == 1, "allocation list has 1 entry")) {
    return false;
  }
  if (!Check(list[0].hAllocation == open_res.wddm_hAllocation, "allocation list carries hAllocation")) {
    return false;
  }
  if (!Check(list[0].WriteOperation == 1, "allocation list entry is write")) {
    return false;
  }
  return Check(list[0].AllocationListSlotId == 0, "allocation list slot id == 0");
}

bool TestOpenResourceAcceptsAllocPrivV2() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3DDDI_HADAPTER hAdapter{};
    D3DDDI_HDEVICE hDevice{};
    D3DDDI_HRESOURCE hResource{};
    bool has_adapter = false;
    bool has_device = false;
    bool has_resource = false;

    ~Cleanup() {
      if (has_resource && device_funcs.pfnDestroyResource) {
        device_funcs.pfnDestroyResource(hDevice, hResource);
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

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  // Simulate a WDDM-enabled device so allocation-list tracking is active in
  // portable builds.
  dev->wddm_context.hContext = 1;
  D3DDDI_ALLOCATIONLIST list[4] = {};
  dev->alloc_list_tracker.rebind(list, 4, 0xFFFFu);

  aerogpu_wddm_alloc_priv_v2 priv{};
  std::memset(&priv, 0, sizeof(priv));
  priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
  priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION_2;
  priv.alloc_id = 0x1234u;
  priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;
  priv.share_token = 0x1122334455667788ull;
  priv.size_bytes = 64ull * 64ull * 4ull;
  priv.reserved0 = AEROGPU_WDDM_ALLOC_PRIV_DESC_PACK(/*format=*/22u, /*width=*/64u, /*height=*/64u);

  D3D9DDIARG_OPENRESOURCE open_res{};
  open_res.pPrivateDriverData = &priv;
  open_res.private_driver_data_size = sizeof(priv);
  open_res.type = 0;
  open_res.format = 0; // reconstructed from alloc priv desc
  open_res.width = 0;
  open_res.height = 0;
  open_res.depth = 1;
  open_res.mip_levels = 1;
  open_res.usage = 0;
  open_res.size = 0;
  open_res.hResource.pDrvPrivate = nullptr;
  open_res.wddm_hAllocation = 0xABCDu;

  hr = cleanup.device_funcs.pfnOpenResource(create_dev.hDevice, &open_res);
  if (!Check(hr == S_OK, "OpenResource(v2)")) {
    return false;
  }
  if (!Check(open_res.hResource.pDrvPrivate != nullptr, "OpenResource(v2) returned resource handle")) {
    return false;
  }

  cleanup.hResource = open_res.hResource;
  cleanup.has_resource = true;

  auto* res = reinterpret_cast<Resource*>(open_res.hResource.pDrvPrivate);
  if (!Check(res != nullptr, "resource pointer")) {
    return false;
  }
  if (!Check(res->backing_alloc_id == priv.alloc_id, "OpenResource(v2) preserves alloc_id from private data")) {
    return false;
  }
  if (!Check(res->wddm_hAllocation == open_res.wddm_hAllocation, "OpenResource(v2) captures WDDM hAllocation")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetRenderTarget(create_dev.hDevice, 0, open_res.hResource);
  if (!Check(hr == S_OK, "SetRenderTarget(opened resource)")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnClear(create_dev.hDevice,
                                     /*flags=*/0x1u,
                                     /*color_rgba8=*/0xFF00FF00u,
                                     /*depth=*/1.0f,
                                     /*stencil=*/0);
  if (!Check(hr == S_OK, "Clear tracks allocation list")) {
    return false;
  }

  if (!Check(dev->alloc_list_tracker.list_len() == 1, "allocation list has 1 entry")) {
    return false;
  }
  if (!Check(list[0].hAllocation == open_res.wddm_hAllocation, "allocation list carries hAllocation")) {
    return false;
  }
  if (!Check(list[0].WriteOperation == 1, "allocation list entry is write")) {
    return false;
  }
  return Check(list[0].AllocationListSlotId == 0, "allocation list slot id == 0");
}

bool TestGuestBackedUnlockEmitsDirtyRangeNotUpload() {
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
        for (auto& hRes : resources) {
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

  aerogpu_wddm_alloc_priv priv{};
  priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
  priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
  priv.alloc_id = 0x1234u;
  priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;
  priv.share_token = 0x1122334455667788ull;
  priv.size_bytes = 64;
  priv.reserved0 = 0;

  D3D9DDIARG_OPENRESOURCE open_res{};
  open_res.pPrivateDriverData = &priv;
  open_res.private_driver_data_size = sizeof(priv);
  open_res.type = 0;
  open_res.format = 0;
  open_res.width = 0;
  open_res.height = 0;
  open_res.depth = 1;
  open_res.mip_levels = 1;
  open_res.usage = 0;
  open_res.size = 64;
  open_res.hResource.pDrvPrivate = nullptr;
  open_res.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnOpenResource(create_dev.hDevice, &open_res);
  if (!Check(hr == S_OK, "OpenResource(guest-backed buffer)")) {
    return false;
  }
  if (!Check(open_res.hResource.pDrvPrivate != nullptr, "OpenResource returned resource handle")) {
    return false;
  }
  cleanup.resources.push_back(open_res.hResource);

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* res = reinterpret_cast<Resource*>(open_res.hResource.pDrvPrivate);
  if (!Check(dev != nullptr && res != nullptr, "device/resource pointers")) {
    return false;
  }
  if (!Check(res->backing_alloc_id == priv.alloc_id, "resource backing_alloc_id populated from private data")) {
    return false;
  }
  if (!Check(res->handle != 0, "guest-backed resource has non-zero handle")) {
    return false;
  }

  D3D9DDIARG_LOCK lock{};
  lock.hResource = open_res.hResource;
  lock.offset_bytes = 8;
  lock.size_bytes = 16;
  lock.flags = 0;

  D3DDDI_LOCKEDBOX locked{};
  hr = cleanup.device_funcs.pfnLock(create_dev.hDevice, &lock, &locked);
  if (!Check(hr == S_OK, "Lock(guest-backed)")) {
    return false;
  }
  if (!Check(locked.pData != nullptr, "Lock returns non-null pData")) {
    return false;
  }

  std::memset(locked.pData, 0xAB, lock.size_bytes);

  D3D9DDIARG_UNLOCK unlock{};
  unlock.hResource = open_res.hResource;
  unlock.offset_bytes = lock.offset_bytes;
  unlock.size_bytes = lock.size_bytes;
  hr = cleanup.device_funcs.pfnUnlock(create_dev.hDevice, &unlock);
  if (!Check(hr == S_OK, "Unlock(guest-backed)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  const CmdLoc upload = FindLastOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE);
  if (!Check(upload.hdr == nullptr, "guest-backed unlock must not emit UPLOAD_RESOURCE")) {
    return false;
  }

  const CmdLoc dirty = FindLastOpcode(buf, len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty.hdr != nullptr, "guest-backed unlock emits RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(dirty.hdr);
  if (!Check(cmd->hdr.size_bytes == sizeof(aerogpu_cmd_resource_dirty_range), "dirty_range packet size_bytes")) {
    return false;
  }
  if (!Check(cmd->resource_handle == res->handle, "dirty_range resource_handle matches")) {
    return false;
  }
  if (!Check(cmd->offset_bytes == lock.offset_bytes, "dirty_range offset_bytes matches")) {
    return false;
  }
  if (!Check(cmd->size_bytes == lock.size_bytes, "dirty_range size_bytes matches")) {
    return false;
  }

  return ValidateStream(buf, len);
}

bool TestGuestBackedDirtyRangeSubmitsWhenCmdBufferFull() {
#if defined(_WIN32)
  // Portable CI builds do not exercise the WDDM DMA-buffer split behavior.
  // Skip this test on Windows where the D3D9 UMD is expected to run in the real
  // WDDM DMA-buffer path.
  return true;
#else
  std::vector<uint8_t> dma_buf(64, 0xCD);

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
        for (auto& hRes : resources) {
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;

  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  aerogpu_wddm_alloc_priv priv{};
  priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
  priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
  priv.alloc_id = 0x4242u;
  priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;
  priv.share_token = 0x1122334455667788ull;
  priv.size_bytes = 32;
  priv.reserved0 = 0;

  D3D9DDIARG_OPENRESOURCE open_res{};
  open_res.pPrivateDriverData = &priv;
  open_res.private_driver_data_size = sizeof(priv);
  open_res.type = 0;
  open_res.format = 0;
  open_res.width = 0;
  open_res.height = 0;
  open_res.depth = 1;
  open_res.mip_levels = 1;
  open_res.usage = 0;
  open_res.size = 32;
  open_res.hResource.pDrvPrivate = nullptr;
  open_res.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnOpenResource(create_dev.hDevice, &open_res);
  if (!Check(hr == S_OK, "OpenResource(guest-backed buffer)")) {
    return false;
  }
  cleanup.resources.push_back(open_res.hResource);

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  auto* res = reinterpret_cast<Resource*>(open_res.hResource.pDrvPrivate);
  if (!Check(dev != nullptr && res != nullptr, "device/resource pointers")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    dev->cmd.set_span(dma_buf.data(), dma_buf.size());
    dev->cmd.reset();

    auto* filler = dev->cmd.TryAppendFixed<unknown_cmd_fixed>(0xDEADBEEFu);
    if (!Check(filler != nullptr, "append filler cmd")) {
      return false;
    }
    filler->value = 0xDEAD1234u;
  }

  D3D9DDIARG_LOCK lock_args{};
  lock_args.hResource = open_res.hResource;
  lock_args.offset_bytes = 0;
  lock_args.size_bytes = 4;
  lock_args.flags = 0;

  D3DDDI_LOCKEDBOX locked{};
  hr = cleanup.device_funcs.pfnLock(create_dev.hDevice, &lock_args, &locked);
  if (!Check(hr == S_OK, "Lock(guest-backed)")) {
    return false;
  }
  std::memset(locked.pData, 0xEF, lock_args.size_bytes);

  D3D9DDIARG_UNLOCK unlock_args{};
  unlock_args.hResource = open_res.hResource;
  unlock_args.offset_bytes = lock_args.offset_bytes;
  unlock_args.size_bytes = lock_args.size_bytes;
  hr = cleanup.device_funcs.pfnUnlock(create_dev.hDevice, &unlock_args);
  if (!Check(hr == S_OK, "Unlock(guest-backed)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  const size_t expected_len = sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_resource_dirty_range);
  if (!Check(len == expected_len, "dirty range flush leaves a single packet in the command buffer")) {
    return false;
  }

  if (!Check(ValidateStream(buf, len), "dirty-range stream validates")) {
    return false;
  }

  if (!Check(CountOpcode(buf, len, 0xDEADBEEFu) == 0, "filler packet was flushed")) {
    return false;
  }
  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0, "no upload_resource emitted")) {
    return false;
  }

  const CmdLoc dirty = FindLastOpcode(buf, len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty.hdr != nullptr, "dirty_range emitted")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(dirty.hdr);
  if (!Check(cmd->resource_handle == res->handle, "dirty_range resource_handle matches")) {
    return false;
  }

  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    dev->cmd.set_vector();
  }

  return true;
#endif
}

bool TestGuestBackedUpdateSurfaceEmitsDirtyRangeNotUpload() {
#if defined(_WIN32)
  // Portable tests exercise the non-WDK code paths; skip on Windows where this
  // D3D9 UMD is expected to run against the real WDDM runtime.
  return true;
#else
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
        for (auto& hRes : resources) {
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  D3DDDI_ALLOCATIONLIST alloc_list[8] = {};

  // Create a CPU-only system-memory source surface.
  D3D9DDIARG_CREATERESOURCE create_src{};
  create_src.type = 0;
  create_src.format = 22u; // D3DFMT_X8R8G8B8
  create_src.width = 4;
  create_src.height = 4;
  create_src.depth = 1;
  create_src.mip_levels = 1;
  create_src.usage = 0;
  create_src.pool = 2u; // D3DPOOL_SYSTEMMEM
  create_src.size = 0;
  create_src.hResource.pDrvPrivate = nullptr;
  create_src.pSharedHandle = nullptr;
  create_src.pKmdAllocPrivateData = nullptr;
  create_src.KmdAllocPrivateDataSize = 0;
  create_src.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_src);
  if (!Check(hr == S_OK, "CreateResource(systemmem src surface)")) {
    return false;
  }
  if (!Check(create_src.hResource.pDrvPrivate != nullptr, "CreateResource returned src resource")) {
    return false;
  }
  cleanup.resources.push_back(create_src.hResource);

  // Enable allocation-list tracking after creating the systemmem resource. The
  // portable build does not emulate WDDM allocation mapping for systemmem
  // surfaces, but we still want to validate allocation tracking for the
  // guest-backed destination below.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    dev->wddm_context.hContext = 1;
    dev->alloc_list_tracker.rebind(alloc_list, 8, 0xFFFFu);
    dev->alloc_list_tracker.reset();
  }

  // Fill the source surface with some bytes.
  auto* src_res = reinterpret_cast<Resource*>(create_src.hResource.pDrvPrivate);
  if (!Check(src_res != nullptr && src_res->handle == 0, "systemmem src surface has no GPU handle")) {
    return false;
  }
  if (!Check(src_res->backing_alloc_id == 0, "systemmem src surface backing_alloc_id == 0")) {
    return false;
  }
  D3D9DDIARG_LOCK lock_src{};
  lock_src.hResource = create_src.hResource;
  lock_src.offset_bytes = 0;
  lock_src.size_bytes = 0;
  lock_src.flags = 0;

  D3DDDI_LOCKEDBOX locked_src{};
  hr = cleanup.device_funcs.pfnLock(create_dev.hDevice, &lock_src, &locked_src);
  if (!Check(hr == S_OK, "Lock(src systemmem)")) {
    return false;
  }
  if (!Check(locked_src.pData != nullptr, "Lock returns src pointer")) {
    return false;
  }
  if (!Check(src_res != nullptr && src_res->size_bytes != 0, "src resource size")) {
    return false;
  }
  std::memset(locked_src.pData, 0x7E, src_res->size_bytes);

  D3D9DDIARG_UNLOCK unlock_src{};
  unlock_src.hResource = create_src.hResource;
  unlock_src.offset_bytes = 0;
  unlock_src.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(create_dev.hDevice, &unlock_src);
  if (!Check(hr == S_OK, "Unlock(src systemmem)")) {
    return false;
  }

  // Create a guest-backed destination surface via OpenResource.
  aerogpu_wddm_alloc_priv priv{};
  priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
  priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
  priv.alloc_id = 0x7777u;
  priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;
  priv.share_token = 0xAABBCCDDEEFF0011ull;
  priv.size_bytes = 4ull * 4ull * 4ull;
  priv.reserved0 = 0;

  D3D9DDIARG_OPENRESOURCE open_dst{};
  open_dst.pPrivateDriverData = &priv;
  open_dst.private_driver_data_size = sizeof(priv);
  open_dst.type = 0;
  open_dst.format = 22u; // D3DFMT_X8R8G8B8
  open_dst.width = 4;
  open_dst.height = 4;
  open_dst.depth = 1;
  open_dst.mip_levels = 1;
  open_dst.usage = 0;
  open_dst.size = 0;
  open_dst.hResource.pDrvPrivate = nullptr;
  open_dst.wddm_hAllocation = 0x1234u;

  hr = cleanup.device_funcs.pfnOpenResource(create_dev.hDevice, &open_dst);
  if (!Check(hr == S_OK, "OpenResource(guest-backed dst surface)")) {
    return false;
  }
  if (!Check(open_dst.hResource.pDrvPrivate != nullptr, "OpenResource returned dst resource")) {
    return false;
  }
  cleanup.resources.push_back(open_dst.hResource);

  auto* dst_res = reinterpret_cast<Resource*>(open_dst.hResource.pDrvPrivate);
  if (!Check(dst_res != nullptr && dst_res->backing_alloc_id == priv.alloc_id, "dst backing_alloc_id")) {
    return false;
  }

  RECT src_rect{};
  src_rect.left = 0;
  src_rect.top = 0;
  src_rect.right = 4;
  src_rect.bottom = 2;
  POINT dst_point{};
  dst_point.x = 0;
  dst_point.y = 1;

  D3D9DDIARG_UPDATESURFACE update{};
  update.hSrc = create_src.hResource;
  update.pSrcRect = &src_rect;
  update.hDst = open_dst.hResource;
  update.pDstPoint = &dst_point;
  update.flags = 0;

  hr = cleanup.device_funcs.pfnUpdateSurface(create_dev.hDevice, &update);
  if (!Check(hr == S_OK, "UpdateSurface(guest-backed dst)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0, "UpdateSurface must not emit UPLOAD_RESOURCE")) {
    return false;
  }

  const CmdLoc dirty = FindLastOpcode(buf, len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty.hdr != nullptr, "UpdateSurface emits RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(dirty.hdr);
  if (!Check(cmd->resource_handle == dst_res->handle, "dirty_range handle")) {
    return false;
  }
  const uint64_t expected_offset = 1ull * 16ull; // dst_point.y * row_pitch (4*4)
  const uint64_t expected_size = 2ull * 16ull;   // 2 rows
  if (!Check(cmd->offset_bytes == expected_offset, "dirty_range offset")) {
    return false;
  }
  if (!Check(cmd->size_bytes == expected_size, "dirty_range size")) {
    return false;
  }

  if (!Check(dev->alloc_list_tracker.list_len() == 1, "allocation list contains dst mapping")) {
    return false;
  }
  if (!Check(alloc_list[0].hAllocation == open_dst.wddm_hAllocation, "allocation list hAllocation matches")) {
    return false;
  }
  if (!Check(alloc_list[0].WriteOperation == 0, "dirty range tracks allocation as read")) {
    return false;
  }

  return ValidateStream(buf, len);
#endif
}

bool TestGuestBackedUpdateTextureEmitsDirtyRangeNotUpload() {
#if defined(_WIN32)
  return true;
#else
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
        for (auto& hRes : resources) {
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

  D3D9DDIARG_CREATEDEVICE create_dev{};
  create_dev.hAdapter = open.hAdapter;
  create_dev.Flags = 0;
  hr = cleanup.adapter_funcs.pfnCreateDevice(&create_dev, &cleanup.device_funcs);
  if (!Check(hr == S_OK, "CreateDevice")) {
    return false;
  }
  cleanup.hDevice = create_dev.hDevice;
  cleanup.has_device = true;

  auto* dev = reinterpret_cast<Device*>(create_dev.hDevice.pDrvPrivate);
  if (!Check(dev != nullptr, "device pointer")) {
    return false;
  }

  D3DDDI_ALLOCATIONLIST alloc_list[8] = {};

  // Source: system-memory pool texture-like surface.
  D3D9DDIARG_CREATERESOURCE create_src{};
  create_src.type = 0;
  create_src.format = 22u; // D3DFMT_X8R8G8B8
  create_src.width = 4;
  create_src.height = 4;
  create_src.depth = 1;
  create_src.mip_levels = 1;
  create_src.usage = 0;
  create_src.pool = 2u; // D3DPOOL_SYSTEMMEM
  create_src.size = 0;
  create_src.hResource.pDrvPrivate = nullptr;
  create_src.pSharedHandle = nullptr;
  create_src.pKmdAllocPrivateData = nullptr;
  create_src.KmdAllocPrivateDataSize = 0;
  create_src.wddm_hAllocation = 0;

  hr = cleanup.device_funcs.pfnCreateResource(create_dev.hDevice, &create_src);
  if (!Check(hr == S_OK, "CreateResource(systemmem src)")) {
    return false;
  }
  cleanup.resources.push_back(create_src.hResource);

  // Enable allocation-list tracking after creating the systemmem resource. The
  // portable build does not emulate WDDM allocation mapping for systemmem
  // surfaces, but we still want to validate allocation tracking for the
  // guest-backed destination below.
  {
    std::lock_guard<std::mutex> lock(dev->mutex);
    dev->wddm_context.hContext = 1;
    dev->alloc_list_tracker.rebind(alloc_list, 8, 0xFFFFu);
    dev->alloc_list_tracker.reset();
  }

  auto* src_res = reinterpret_cast<Resource*>(create_src.hResource.pDrvPrivate);
  if (!Check(src_res != nullptr && src_res->handle == 0, "systemmem src has no GPU handle")) {
    return false;
  }
  if (!Check(src_res->backing_alloc_id == 0, "systemmem src backing_alloc_id == 0")) {
    return false;
  }
  if (!Check(src_res != nullptr && src_res->size_bytes != 0, "src size")) {
    return false;
  }

  D3D9DDIARG_LOCK lock_src{};
  lock_src.hResource = create_src.hResource;
  lock_src.offset_bytes = 0;
  lock_src.size_bytes = 0;
  lock_src.flags = 0;

  D3DDDI_LOCKEDBOX locked_src{};
  hr = cleanup.device_funcs.pfnLock(create_dev.hDevice, &lock_src, &locked_src);
  if (!Check(hr == S_OK, "Lock(src)")) {
    return false;
  }
  std::memset(locked_src.pData, 0x3C, src_res->size_bytes);

  D3D9DDIARG_UNLOCK unlock_src{};
  unlock_src.hResource = create_src.hResource;
  unlock_src.offset_bytes = 0;
  unlock_src.size_bytes = 0;
  hr = cleanup.device_funcs.pfnUnlock(create_dev.hDevice, &unlock_src);
  if (!Check(hr == S_OK, "Unlock(src)")) {
    return false;
  }

  // Destination: guest-backed surface via OpenResource.
  aerogpu_wddm_alloc_priv priv{};
  priv.magic = AEROGPU_WDDM_ALLOC_PRIV_MAGIC;
  priv.version = AEROGPU_WDDM_ALLOC_PRIV_VERSION;
  priv.alloc_id = 0x8888u;
  priv.flags = AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED;
  priv.share_token = 0xCAFEBABEDEADBEEFull;
  priv.size_bytes = 4ull * 4ull * 4ull;
  priv.reserved0 = 0;

  D3D9DDIARG_OPENRESOURCE open_dst{};
  open_dst.pPrivateDriverData = &priv;
  open_dst.private_driver_data_size = sizeof(priv);
  open_dst.type = 0;
  open_dst.format = 22u;
  open_dst.width = 4;
  open_dst.height = 4;
  open_dst.depth = 1;
  open_dst.mip_levels = 1;
  open_dst.usage = 0;
  open_dst.size = 0;
  open_dst.hResource.pDrvPrivate = nullptr;
  open_dst.wddm_hAllocation = 0x4321u;

  hr = cleanup.device_funcs.pfnOpenResource(create_dev.hDevice, &open_dst);
  if (!Check(hr == S_OK, "OpenResource(dst guest-backed)")) {
    return false;
  }
  cleanup.resources.push_back(open_dst.hResource);

  auto* dst_res = reinterpret_cast<Resource*>(open_dst.hResource.pDrvPrivate);
  if (!Check(dst_res != nullptr && dst_res->backing_alloc_id == priv.alloc_id, "dst backing_alloc_id")) {
    return false;
  }

  D3D9DDIARG_UPDATETEXTURE update{};
  update.hSrc = create_src.hResource;
  update.hDst = open_dst.hResource;
  update.flags = 0;

  hr = cleanup.device_funcs.pfnUpdateTexture(create_dev.hDevice, &update);
  if (!Check(hr == S_OK, "UpdateTexture(guest-backed dst)")) {
    return false;
  }

  dev->cmd.finalize();
  const uint8_t* buf = dev->cmd.data();
  const size_t len = dev->cmd.bytes_used();

  if (!Check(CountOpcode(buf, len, AEROGPU_CMD_UPLOAD_RESOURCE) == 0, "UpdateTexture must not emit UPLOAD_RESOURCE")) {
    return false;
  }

  const CmdLoc dirty = FindLastOpcode(buf, len, AEROGPU_CMD_RESOURCE_DIRTY_RANGE);
  if (!Check(dirty.hdr != nullptr, "UpdateTexture emits RESOURCE_DIRTY_RANGE")) {
    return false;
  }
  const auto* cmd = reinterpret_cast<const aerogpu_cmd_resource_dirty_range*>(dirty.hdr);
  if (!Check(cmd->resource_handle == dst_res->handle, "dirty_range handle")) {
    return false;
  }
  if (!Check(cmd->offset_bytes == 0, "dirty_range offset 0")) {
    return false;
  }
  if (!Check(cmd->size_bytes == dst_res->size_bytes, "dirty_range size matches dst size")) {
    return false;
  }

  if (!Check(dev->alloc_list_tracker.list_len() == 1, "allocation list contains dst mapping")) {
    return false;
  }
  if (!Check(alloc_list[0].hAllocation == open_dst.wddm_hAllocation, "allocation list hAllocation matches")) {
    return false;
  }
  if (!Check(alloc_list[0].WriteOperation == 0, "dirty range tracks allocation as read")) {
    return false;
  }

  return ValidateStream(buf, len);
#endif
}

bool TestKmdQueryGetScanLineClearsOutputsOnFailure() {
  AerogpuKmdQuery query;
  bool in_vblank = true;
  uint32_t scan_line = 123;

  const bool ok = query.GetScanLine(/*vid_pn_source_id=*/0, &in_vblank, &scan_line);
  if (!Check(!ok, "GetScanLine returns false when adapter is not initialized")) {
    return false;
  }
  if (!Check(in_vblank == false, "GetScanLine clears in_vblank on failure")) {
    return false;
  }
  return Check(scan_line == 0, "GetScanLine clears scan_line on failure");
}

} // namespace
} // namespace aerogpu

int main() {
  int failures = 0;
  failures += !aerogpu::TestHeaderFieldsAndFinalize();
  failures += !aerogpu::TestAlignmentAndPadding();
  failures += !aerogpu::TestUnknownOpcodeSkipBySize();
  failures += !aerogpu::TestOutOfSpaceReturnsNullptrAndSetsError();
  failures += !aerogpu::TestCmdStreamWriterOverflowReturnsNullAndSetsError();
  failures += !aerogpu::TestFixedPacketPadding();
  failures += !aerogpu::TestOwnedAndBorrowedStreamsMatch();
  failures += !aerogpu::TestEventQueryGetDataSemantics();
  failures += !aerogpu::TestAdapterCapsAndQueryAdapterInfo();
  failures += !aerogpu::TestAdapterMultisampleQualityLevels();
  failures += !aerogpu::TestAdapterCachingUpdatesCallbacks();
  failures += !aerogpu::TestCreateResourceRejectsUnsupportedGpuFormat();
  failures += !aerogpu::TestCreateResourceComputesBcTexturePitchAndSize();
  failures += !aerogpu::TestCreateResourceIgnoresStaleAllocPrivDataForNonShared();
  failures += !aerogpu::TestCreateResourceAllowsNullPrivateDataWhenNotAllocBacked();
  failures += !aerogpu::TestAllocBackedUnlockEmitsDirtyRange();
  failures += !aerogpu::TestSharedResourceCreateAndOpenEmitsExportImport();
  failures += !aerogpu::TestPresentStatsAndFrameLatency();
  failures += !aerogpu::TestPresentExSubmitsOnceWhenNoPendingRenderWork();
  failures += !aerogpu::TestPresentSubmitsOnceWhenNoPendingRenderWork();
  failures += !aerogpu::TestPresentExSplitsRenderAndPresentSubmissions();
  failures += !aerogpu::TestConcurrentPresentExReturnsDistinctFences();
  failures += !aerogpu::TestPresentSplitsRenderAndPresentSubmissions();
  failures += !aerogpu::TestFlushNoopsOnEmptyCommandBuffer();
  failures += !aerogpu::TestGetDisplayModeExReturnsPrimaryMode();
  failures += !aerogpu::TestDeviceMiscExApisSucceed();
  failures += !aerogpu::TestAllocationListSplitResetsOnEmptySubmit();
  failures += !aerogpu::TestDrawStateTrackingPreSplitRetainsAllocs();
  failures += !aerogpu::TestRenderTargetTrackingPreSplitRetainsAllocs();
  failures += !aerogpu::TestDrawStateTrackingDedupsSharedAllocIds();
  failures += !aerogpu::TestRotateResourceIdentitiesTrackingPreSplitRetainsAllocs();
  failures += !aerogpu::TestOpenResourceCapturesWddmAllocationForTracking();
  failures += !aerogpu::TestOpenResourceAcceptsAllocPrivV2();
  failures += !aerogpu::TestInvalidPayloadArgs();
  failures += !aerogpu::TestDestroyBoundShaderUnbinds();
  failures += !aerogpu::TestDestroyBoundVertexDeclUnbinds();
  failures += !aerogpu::TestFvfXyzrhwDiffuseDrawPrimitiveUpEmitsFixedfuncCommands();
  failures += !aerogpu::TestFvfXyzrhwDiffuseDrawPrimitiveEmulationConvertsVertices();
  failures += !aerogpu::TestDrawIndexedPrimitiveUpEmitsIndexBufferCommands();
  failures += !aerogpu::TestFvfXyzrhwDiffuseDrawIndexedPrimitiveEmulationConvertsVertices();
  failures += !aerogpu::TestResetShrinkUnbindsBackbuffer();
  failures += !aerogpu::TestRotateResourceIdentitiesRebindsChangedHandles();
  failures += !aerogpu::TestPresentBackbufferRotationUndoOnSmallCmdBuffer();
  failures += !aerogpu::TestPresentBackbufferRotationUndoOnSmallAllocList();
  failures += !aerogpu::TestPresentBackbufferRotationRebindsBackbufferTexture();
  failures += !aerogpu::TestSetRenderTargetRejectsGaps();
  failures += !aerogpu::TestRotateResourceIdentitiesUndoOnSmallCmdBuffer();
  failures += !aerogpu::TestResetRebindsBackbufferTexture();
  failures += !aerogpu::TestOpenResourceTracksWddmAllocationHandle();
  failures += !aerogpu::TestGuestBackedUnlockEmitsDirtyRangeNotUpload();
  failures += !aerogpu::TestGuestBackedDirtyRangeSubmitsWhenCmdBufferFull();
  failures += !aerogpu::TestGuestBackedUpdateSurfaceEmitsDirtyRangeNotUpload();
  failures += !aerogpu::TestGuestBackedUpdateTextureEmitsDirtyRangeNotUpload();
  failures += !aerogpu::TestKmdQueryGetScanLineClearsOutputsOnFailure();
  return failures ? 1 : 0;
}
