#include <cstddef>
#include <cstdint>
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

#include "aerogpu_cmd_stream_writer.h"

namespace aerogpu {

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

struct unknown_cmd_fixed {
  aerogpu_cmd_hdr hdr;
  uint32_t value;
};

struct CmdLoc {
  const aerogpu_cmd_hdr* hdr = nullptr;
  size_t offset = 0;
};

CmdLoc FindLastOpcode(const uint8_t* buf, size_t size, uint32_t opcode) {
  CmdLoc loc{};
  if (!buf || size < sizeof(aerogpu_cmd_stream_header)) {
    return loc;
  }

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= size) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      loc.hdr = hdr;
      loc.offset = offset;
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > size - offset) {
      break;
    }
    offset += hdr->size_bytes;
  }
  return loc;
}

size_t CountOpcode(const uint8_t* buf, size_t size, uint32_t opcode) {
  if (!buf || size < sizeof(aerogpu_cmd_stream_header)) {
    return 0;
  }

  size_t count = 0;
  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset + sizeof(aerogpu_cmd_hdr) <= size) {
    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    if (hdr->opcode == opcode) {
      count++;
    }
    if (hdr->size_bytes == 0 || hdr->size_bytes > size - offset) {
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
    D3D9DDI_HADAPTER hAdapter{};
    D3D9DDI_HDEVICE hDevice{};
    AEROGPU_D3D9DDI_HQUERY hQuery{};
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
  AEROGPU_D3D9DDIARG_CREATEQUERY create_query{};
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

  AEROGPU_D3D9DDIARG_ISSUEQUERY issue{};
  issue.hQuery = create_query.hQuery;
  issue.flags = 0x1u; // D3DISSUE_END
  hr = cleanup.device_funcs.pfnIssueQuery(create_dev.hDevice, &issue);
  if (!Check(hr == S_OK, "IssueQuery(END)")) {
    return false;
  }

  auto* adapter = reinterpret_cast<Adapter*>(open.hAdapter.pDrvPrivate);
  auto* query = reinterpret_cast<Query*>(create_query.hQuery.pDrvPrivate);
  const uint64_t fence_value = query->fence_value.load(std::memory_order_acquire);
  if (!Check(fence_value != 0, "event query fence_value")) {
    return false;
  }

  // Force the query into the "not ready" state.
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    adapter->completed_fence = 0;
  }

  uint32_t done = 0;
  AEROGPU_D3D9DDIARG_GETQUERYDATA get_data{};
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
      AEROGPU_D3D9DDIARG_GETQUERYDATA gd = get_data;
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

  // Mark the fence complete and re-poll.
  {
    std::lock_guard<std::mutex> lock(adapter->fence_mutex);
    adapter->completed_fence = fence_value;
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
  AEROGPU_D3D9DDIARG_GETQUERYDATA invalid_args = get_data;
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

  auto r0 = dev.alloc_list_tracker.track_buffer_read(/*hAllocation=*/1, /*alloc_id=*/1);
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
  auto r1 = dev.alloc_list_tracker.track_buffer_read(/*hAllocation=*/2, /*alloc_id=*/2);
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
    D3D9DDI_HADAPTER hAdapter{};
    D3D9DDI_HDEVICE hDevice{};
    AEROGPU_D3D9DDI_HSHADER hShader{};
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
  AEROGPU_D3D9DDI_HSHADER hShader{};
  hr = cleanup.device_funcs.pfnCreateShader(create_dev.hDevice,
                                            AEROGPU_D3D9DDI_SHADER_STAGE_VS,
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

  hr = cleanup.device_funcs.pfnSetShader(create_dev.hDevice, AEROGPU_D3D9DDI_SHADER_STAGE_VS, hShader);
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
    D3D9DDI_HADAPTER hAdapter{};
    D3D9DDI_HDEVICE hDevice{};
    AEROGPU_D3D9DDI_HVERTEXDECL hDecl{};
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
  AEROGPU_D3D9DDI_HVERTEXDECL hDecl{};
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
    D3D9DDI_HADAPTER hAdapter{};
    D3D9DDI_HDEVICE hDevice{};
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

  AEROGPU_D3D9DDI_VIEWPORT vp{};
  vp.x = 0.0f;
  vp.y = 0.0f;
  vp.w = 256.0f;
  vp.h = 256.0f;
  vp.min_z = 0.0f;
  vp.max_z = 1.0f;
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
      create_dev.hDevice, AEROGPU_D3D9DDI_PRIM_TRIANGLELIST, 1, verts, sizeof(Vertex));
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

  const float expected_x0 = ((verts[0].x + 0.5f - vp.x) / vp.w) * 2.0f - 1.0f;
  const float expected_y0 = 1.0f - ((verts[0].y + 0.5f - vp.y) / vp.h) * 2.0f;
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

bool TestResetShrinkUnbindsBackbuffer() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3D9DDI_HADAPTER hAdapter{};
    D3D9DDI_HDEVICE hDevice{};
    AEROGPU_D3D9DDI_HSWAPCHAIN hSwapChain{};
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

  AEROGPU_D3D9DDIARG_CREATESWAPCHAIN create_sc{};
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

  AEROGPU_D3D9DDI_HRESOURCE hRt{};
  hRt.pDrvPrivate = bb1;
  hr = cleanup.device_funcs.pfnSetRenderTarget(create_dev.hDevice, 0, hRt);
  if (!Check(hr == S_OK, "SetRenderTarget(backbuffer1)")) {
    return false;
  }
  if (!Check(dev->render_targets[0] == bb1, "render target points at backbuffer1")) {
    return false;
  }

  AEROGPU_D3D9DDIARG_RESET reset{};
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
    D3D9DDI_HADAPTER hAdapter{};
    D3D9DDI_HDEVICE hDevice{};
    std::vector<AEROGPU_D3D9DDI_HRESOURCE> resources;
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

  auto create_buffer = [&](uint32_t size_bytes) -> AEROGPU_D3D9DDI_HRESOURCE {
    AEROGPU_D3D9DDIARG_CREATERESOURCE args{};
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

  auto create_surface = [&](uint32_t w, uint32_t h) -> AEROGPU_D3D9DDI_HRESOURCE {
    AEROGPU_D3D9DDIARG_CREATERESOURCE args{};
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

  AEROGPU_D3D9DDI_HRESOURCE hVb0 = create_buffer(256);
  AEROGPU_D3D9DDI_HRESOURCE hVb1 = create_buffer(256);
  if (!Check(hVb0.pDrvPrivate != nullptr && hVb1.pDrvPrivate != nullptr, "vertex buffers created")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetStreamSource(create_dev.hDevice, 0, hVb0, 0, 16);
  if (!Check(hr == S_OK, "SetStreamSource")) {
    return false;
  }

  AEROGPU_D3D9DDI_HRESOURCE hTex0 = create_surface(32, 32);
  AEROGPU_D3D9DDI_HRESOURCE hTex1 = create_surface(32, 32);
  if (!Check(hTex0.pDrvPrivate != nullptr && hTex1.pDrvPrivate != nullptr, "textures created")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetTexture(create_dev.hDevice, 0, hTex0);
  if (!Check(hr == S_OK, "SetTexture")) {
    return false;
  }

  AEROGPU_D3D9DDI_HRESOURCE hIb0 = create_buffer(128);
  AEROGPU_D3D9DDI_HRESOURCE hIb1 = create_buffer(128);
  if (!Check(hIb0.pDrvPrivate != nullptr && hIb1.pDrvPrivate != nullptr, "index buffers created")) {
    return false;
  }

  hr = cleanup.device_funcs.pfnSetIndices(create_dev.hDevice, hIb0, AEROGPU_D3D9DDI_INDEX_FORMAT_U16, 4);
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
  const aerogpu_handle_t vb0_before = vb0->handle;
  const aerogpu_handle_t vb1_before = vb1->handle;
  AEROGPU_D3D9DDI_HRESOURCE vb_rotate[2] = {hVb0, hVb1};
  hr = cleanup.device_funcs.pfnRotateResourceIdentities(create_dev.hDevice, vb_rotate, 2);
  if (!Check(hr == S_OK, "RotateResourceIdentities(vb)")) {
    return false;
  }
  if (!Check(vb0->handle == vb1_before && vb1->handle == vb0_before, "vertex buffer handles rotated")) {
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
  const aerogpu_handle_t tex0_before = tex0->handle;
  const aerogpu_handle_t tex1_before = tex1->handle;
  AEROGPU_D3D9DDI_HRESOURCE tex_rotate[2] = {hTex0, hTex1};
  hr = cleanup.device_funcs.pfnRotateResourceIdentities(create_dev.hDevice, tex_rotate, 2);
  if (!Check(hr == S_OK, "RotateResourceIdentities(tex)")) {
    return false;
  }
  if (!Check(tex0->handle == tex1_before && tex1->handle == tex0_before, "texture handles rotated")) {
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
  const aerogpu_handle_t ib0_before = ib0->handle;
  const aerogpu_handle_t ib1_before = ib1->handle;
  AEROGPU_D3D9DDI_HRESOURCE ib_rotate[2] = {hIb0, hIb1};
  hr = cleanup.device_funcs.pfnRotateResourceIdentities(create_dev.hDevice, ib_rotate, 2);
  if (!Check(hr == S_OK, "RotateResourceIdentities(ib)")) {
    return false;
  }
  if (!Check(ib0->handle == ib1_before && ib1->handle == ib0_before, "index buffer handles rotated")) {
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
    D3D9DDI_HADAPTER hAdapter{};
    D3D9DDI_HDEVICE hDevice{};
    AEROGPU_D3D9DDI_HSWAPCHAIN hSwapChain{};
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

  AEROGPU_D3D9DDIARG_CREATESWAPCHAIN create_sc{};
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

  AEROGPU_D3D9DDIARG_PRESENTEX present{};
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

bool TestRotateResourceIdentitiesUndoOnSmallCmdBuffer() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3D9DDI_HADAPTER hAdapter{};
    D3D9DDI_HDEVICE hDevice{};
    AEROGPU_D3D9DDI_HRESOURCE resources[2]{};
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

  AEROGPU_D3D9DDIARG_CREATERESOURCE create_res{};
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

  dev->cmd.set_vector();
  hr = cleanup.device_funcs.pfnRotateResourceIdentities(create_dev.hDevice, cleanup.resources, 2);
  if (!Check(hr == S_OK, "RotateResourceIdentities succeeds with vector cmd buffer")) {
    return false;
  }
  return Check(res0->handle == h1 && res1->handle == h0, "rotate identities swaps handles on success");
}

bool TestResetRebindsBackbufferTexture() {
  struct Cleanup {
    D3D9DDI_ADAPTERFUNCS adapter_funcs{};
    D3D9DDI_DEVICEFUNCS device_funcs{};
    D3D9DDI_HADAPTER hAdapter{};
    D3D9DDI_HDEVICE hDevice{};
    AEROGPU_D3D9DDI_HSWAPCHAIN hSwapChain{};
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

  AEROGPU_D3D9DDIARG_CREATESWAPCHAIN create_sc{};
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

  AEROGPU_D3D9DDI_HRESOURCE hTex{};
  hTex.pDrvPrivate = bb;
  hr = cleanup.device_funcs.pfnSetTexture(create_dev.hDevice, 0, hTex);
  if (!Check(hr == S_OK, "SetTexture(backbuffer)")) {
    return false;
  }

  AEROGPU_D3D9DDIARG_RESET reset{};
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
  failures += !aerogpu::TestAllocationListSplitResetsOnEmptySubmit();
  failures += !aerogpu::TestInvalidPayloadArgs();
  failures += !aerogpu::TestDestroyBoundShaderUnbinds();
  failures += !aerogpu::TestDestroyBoundVertexDeclUnbinds();
  failures += !aerogpu::TestFvfXyzrhwDiffuseDrawPrimitiveUpEmitsFixedfuncCommands();
  failures += !aerogpu::TestResetShrinkUnbindsBackbuffer();
  failures += !aerogpu::TestRotateResourceIdentitiesRebindsChangedHandles();
  failures += !aerogpu::TestPresentBackbufferRotationUndoOnSmallCmdBuffer();
  failures += !aerogpu::TestRotateResourceIdentitiesUndoOnSmallCmdBuffer();
  failures += !aerogpu::TestResetRebindsBackbufferTexture();
  return failures ? 1 : 0;
}
