#include <cassert>
#include <cstddef>
#include <cstdint>
#include <cstring>

#include "aerogpu_cmd_stream_writer.h"

namespace aerogpu {
namespace {

struct unknown_cmd_fixed {
  aerogpu_cmd_hdr hdr;
  uint32_t value;
};

static void validate_stream(const uint8_t* buf, size_t capacity) {
  (void)capacity;
  assert(buf != nullptr);

  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  assert(stream->magic == AEROGPU_CMD_STREAM_MAGIC);
  assert(stream->abi_version == AEROGPU_ABI_VERSION_U32);
  assert(stream->flags == AEROGPU_CMD_STREAM_FLAG_NONE);
  assert(stream->size_bytes >= sizeof(aerogpu_cmd_stream_header));

  size_t offset = sizeof(aerogpu_cmd_stream_header);
  while (offset < stream->size_bytes) {
    assert((offset & 3u) == 0);

    const auto* hdr = reinterpret_cast<const aerogpu_cmd_hdr*>(buf + offset);
    assert(hdr->size_bytes >= sizeof(aerogpu_cmd_hdr));
    assert((hdr->size_bytes & 3u) == 0);

    offset += hdr->size_bytes;
  }
  assert(offset == stream->size_bytes);
}

static void test_header_fields_and_finalize() {
  uint8_t buf[256];
  std::memset(buf, 0xCD, sizeof(buf));

  SpanCmdStreamWriter w(buf, sizeof(buf));
  w.reset();

  assert(w.bytes_used() == sizeof(aerogpu_cmd_stream_header));
  assert(w.bytes_remaining() == sizeof(buf) - sizeof(aerogpu_cmd_stream_header));
  assert(w.empty());

  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  assert(stream->magic == AEROGPU_CMD_STREAM_MAGIC);
  assert(stream->abi_version == AEROGPU_ABI_VERSION_U32);
  assert(stream->flags == AEROGPU_CMD_STREAM_FLAG_NONE);
  assert(stream->size_bytes == sizeof(aerogpu_cmd_stream_header));

  auto* present = w.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  assert(present != nullptr);
  present->scanout_id = 0;
  present->flags = AEROGPU_PRESENT_FLAG_NONE;

  const size_t expected = sizeof(aerogpu_cmd_stream_header) + align_up(sizeof(aerogpu_cmd_present), 4);
  assert(w.bytes_used() == expected);
  assert(!w.empty());

  w.finalize();
  assert(stream->size_bytes == expected);
  validate_stream(buf, sizeof(buf));
}

static void test_alignment_and_padding() {
  uint8_t buf[256];
  std::memset(buf, 0xAB, sizeof(buf));

  SpanCmdStreamWriter w(buf, sizeof(buf));
  w.reset();

  const uint8_t payload[3] = {0x01, 0x02, 0x03};
  auto* cmd = w.append_with_payload<aerogpu_cmd_create_shader_dxbc>(
      AEROGPU_CMD_CREATE_SHADER_DXBC, payload, sizeof(payload));
  assert(cmd != nullptr);

  cmd->shader_handle = 42;
  cmd->stage = AEROGPU_SHADER_STAGE_PIXEL;
  cmd->dxbc_size_bytes = static_cast<uint32_t>(sizeof(payload));
  cmd->reserved0 = 0;

  const size_t cmd_size = sizeof(aerogpu_cmd_create_shader_dxbc) + sizeof(payload);
  const size_t aligned_size = align_up(cmd_size, 4);
  assert(cmd->hdr.size_bytes == aligned_size);
  assert((cmd->hdr.size_bytes & 3u) == 0);

  const size_t payload_off = sizeof(aerogpu_cmd_stream_header) + sizeof(aerogpu_cmd_create_shader_dxbc);
  assert(std::memcmp(buf + payload_off, payload, sizeof(payload)) == 0);

  // Validate padding bytes are zeroed.
  for (size_t i = cmd_size; i < aligned_size; i++) {
    assert(buf[sizeof(aerogpu_cmd_stream_header) + i] == 0);
  }

  w.finalize();
  validate_stream(buf, sizeof(buf));
}

static void test_unknown_opcode_skip_by_size() {
  uint8_t buf[256] = {};

  SpanCmdStreamWriter w(buf, sizeof(buf));
  w.reset();

  auto* u = w.append_fixed<unknown_cmd_fixed>(0xDEADBEEFu);
  assert(u != nullptr);
  u->value = 0x12345678u;

  auto* present = w.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  assert(present != nullptr);
  present->scanout_id = 0;
  present->flags = AEROGPU_PRESENT_FLAG_NONE;

  w.finalize();
  validate_stream(buf, sizeof(buf));
}

static void test_out_of_space_returns_nullptr() {
  uint8_t buf[sizeof(aerogpu_cmd_stream_header) + 4] = {};

  SpanCmdStreamWriter w(buf, sizeof(buf));
  w.reset();
  assert(w.empty());

  auto* present = w.append_fixed<aerogpu_cmd_present>(AEROGPU_CMD_PRESENT);
  assert(present == nullptr);
  assert(w.bytes_used() == sizeof(aerogpu_cmd_stream_header));

  w.finalize();
  const auto* stream = reinterpret_cast<const aerogpu_cmd_stream_header*>(buf);
  assert(stream->size_bytes == sizeof(aerogpu_cmd_stream_header));
}

} // namespace
} // namespace aerogpu

int main() {
  aerogpu::test_header_fields_and_finalize();
  aerogpu::test_alignment_and_padding();
  aerogpu::test_unknown_opcode_skip_by_size();
  aerogpu::test_out_of_space_returns_nullptr();
  return 0;
}

