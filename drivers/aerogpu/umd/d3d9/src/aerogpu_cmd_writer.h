#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <type_traits>
#include <vector>

#include "aerogpu_cmd.h"

namespace aerogpu {

inline size_t align_up(size_t v, size_t a) {
  return (v + (a - 1)) & ~(a - 1);
}

class CmdWriter {
 public:
  void reset() {
    buf_.clear();
    buf_.resize(sizeof(aerogpu_cmd_stream_header), 0);

    auto* stream = reinterpret_cast<aerogpu_cmd_stream_header*>(buf_.data());
    stream->magic = AEROGPU_CMD_STREAM_MAGIC;
    stream->abi_version = AEROGPU_ABI_VERSION_U32;
    stream->size_bytes = static_cast<uint32_t>(buf_.size());
    stream->flags = AEROGPU_CMD_STREAM_FLAG_NONE;
    stream->reserved0 = 0;
    stream->reserved1 = 0;
  }

  const uint8_t* data() const {
    return buf_.data();
  }

  size_t size() const {
    return buf_.size();
  }

  bool empty() const {
    return buf_.size() <= sizeof(aerogpu_cmd_stream_header);
  }

  template <typename T>
  T* append_fixed(uint32_t opcode) {
    static_assert(std::is_trivial<T>::value, "packets must be POD");
    static_assert(sizeof(T) >= sizeof(aerogpu_cmd_hdr), "packets must contain aerogpu_cmd_hdr");
    return reinterpret_cast<T*>(append_raw(opcode, sizeof(T)));
  }

  // Append a command with a fixed-size header + variable-sized payload.
  //
  // The returned pointer is to the header structure (type HeaderT) stored
  // inside the command buffer.
  template <typename HeaderT>
  HeaderT* append_with_payload(uint32_t opcode, const void* payload, size_t payload_size) {
    static_assert(std::is_trivial<HeaderT>::value, "packets must be POD");
    static_assert(sizeof(HeaderT) >= sizeof(aerogpu_cmd_hdr), "packets must contain aerogpu_cmd_hdr");

    const size_t cmd_size = sizeof(HeaderT) + payload_size;
    uint8_t* base = append_raw(opcode, cmd_size);
    auto* packet = reinterpret_cast<HeaderT*>(base);
    if (payload_size) {
      std::memcpy(base + sizeof(HeaderT), payload, payload_size);
    }
    return packet;
  }

  void finalize() {
    if (buf_.size() < sizeof(aerogpu_cmd_stream_header)) {
      return;
    }
    auto* stream = reinterpret_cast<aerogpu_cmd_stream_header*>(buf_.data());
    stream->size_bytes = static_cast<uint32_t>(buf_.size());
  }

 private:
  uint8_t* append_raw(uint32_t opcode, size_t cmd_size) {
    const size_t aligned_size = align_up(cmd_size, 4);

    const size_t offset = buf_.size();
    buf_.resize(offset + aligned_size, 0);

    auto* hdr = reinterpret_cast<aerogpu_cmd_hdr*>(buf_.data() + offset);
    hdr->opcode = opcode;
    hdr->size_bytes = static_cast<uint32_t>(aligned_size);
    return reinterpret_cast<uint8_t*>(hdr);
  }

  std::vector<uint8_t> buf_;
};

} // namespace aerogpu
