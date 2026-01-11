#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

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

} // namespace
} // namespace aerogpu

int main() {
  int failures = 0;
  failures += !aerogpu::TestHeaderFieldsAndFinalize();
  failures += !aerogpu::TestAlignmentAndPadding();
  failures += !aerogpu::TestUnknownOpcodeSkipBySize();
  failures += !aerogpu::TestOutOfSpaceReturnsNullptrAndSetsError();
  failures += !aerogpu::TestFixedPacketPadding();
  failures += !aerogpu::TestOwnedAndBorrowedStreamsMatch();
  return failures ? 1 : 0;
}

