#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <type_traits>
#include <vector>

#include "aerogpu_cmd.h"

namespace aerogpu {

inline size_t align_up(size_t v, size_t a) {
  return (v + (a - 1)) & ~(a - 1);
}

// Span-backed command stream writer.
//
// Writes AeroGPU command packets directly into a caller-provided buffer (e.g.
// WDDM DMA command buffer). All packets are 4-byte aligned as required by the
// protocol (`aerogpu_cmd_hdr::size_bytes`).
class SpanCmdStreamWriter {
 public:
  SpanCmdStreamWriter() = default;
  SpanCmdStreamWriter(uint8_t* buf, size_t capacity) : buf_(buf), capacity_(capacity) {
    reset();
  }

  void set_buffer(uint8_t* buf, size_t capacity) {
    buf_ = buf;
    capacity_ = capacity;
  }

  void reset() {
    cursor_ = 0;
    if (!buf_ || capacity_ < sizeof(aerogpu_cmd_stream_header)) {
      return;
    }

    std::memset(buf_, 0, sizeof(aerogpu_cmd_stream_header));
    auto* stream = reinterpret_cast<aerogpu_cmd_stream_header*>(buf_);
    stream->magic = AEROGPU_CMD_STREAM_MAGIC;
    stream->abi_version = AEROGPU_ABI_VERSION_U32;
    stream->size_bytes = static_cast<uint32_t>(sizeof(aerogpu_cmd_stream_header));
    stream->flags = AEROGPU_CMD_STREAM_FLAG_NONE;
    stream->reserved0 = 0;
    stream->reserved1 = 0;

    cursor_ = sizeof(aerogpu_cmd_stream_header);
  }

  uint8_t* data() {
    return buf_;
  }
  const uint8_t* data() const {
    return buf_;
  }

  size_t bytes_used() const {
    return cursor_;
  }

  // Compatibility with existing `CmdWriter` call sites.
  size_t size() const {
    return bytes_used();
  }

  size_t bytes_remaining() const {
    if (cursor_ > capacity_) {
      return 0;
    }
    return capacity_ - cursor_;
  }

  bool empty() const {
    return cursor_ == sizeof(aerogpu_cmd_stream_header);
  }

  void finalize() {
    if (!buf_ || capacity_ < sizeof(aerogpu_cmd_stream_header)) {
      return;
    }

    if (cursor_ > std::numeric_limits<uint32_t>::max()) {
      return;
    }

    auto* stream = reinterpret_cast<aerogpu_cmd_stream_header*>(buf_);
    stream->size_bytes = static_cast<uint32_t>(cursor_);
  }

  template <typename T>
  T* append_fixed(uint32_t opcode) {
    static_assert(std::is_trivial<T>::value, "packets must be POD");
    static_assert(sizeof(T) >= sizeof(aerogpu_cmd_hdr), "packets must contain aerogpu_cmd_hdr");
    return reinterpret_cast<T*>(append_raw(opcode, sizeof(T)));
  }

  template <typename HeaderT>
  HeaderT* append_with_payload(uint32_t opcode, const void* payload, size_t payload_size) {
    static_assert(std::is_trivial<HeaderT>::value, "packets must be POD");
    static_assert(sizeof(HeaderT) >= sizeof(aerogpu_cmd_hdr), "packets must contain aerogpu_cmd_hdr");

    if (payload_size && !payload) {
      return nullptr;
    }

    if (payload_size > std::numeric_limits<size_t>::max() - sizeof(HeaderT)) {
      return nullptr;
    }

    const size_t cmd_size = sizeof(HeaderT) + payload_size;
    uint8_t* base = append_raw(opcode, cmd_size);
    if (!base) {
      return nullptr;
    }

    if (payload_size) {
      std::memcpy(base + sizeof(HeaderT), payload, payload_size);
    }
    return reinterpret_cast<HeaderT*>(base);
  }

 private:
  uint8_t* append_raw(uint32_t opcode, size_t cmd_size) {
    if (!buf_ || capacity_ < sizeof(aerogpu_cmd_stream_header)) {
      return nullptr;
    }

    if (cmd_size < sizeof(aerogpu_cmd_hdr)) {
      return nullptr;
    }

    if (cmd_size > std::numeric_limits<size_t>::max() - 3) {
      return nullptr;
    }
    const size_t aligned_size = align_up(cmd_size, 4);

    if (aligned_size > std::numeric_limits<uint32_t>::max()) {
      return nullptr;
    }

    if (cursor_ > capacity_ || aligned_size > capacity_ - cursor_) {
      return nullptr;
    }

    uint8_t* ptr = buf_ + cursor_;
    std::memset(ptr, 0, aligned_size);

    auto* hdr = reinterpret_cast<aerogpu_cmd_hdr*>(ptr);
    hdr->opcode = opcode;
    hdr->size_bytes = static_cast<uint32_t>(aligned_size);

    cursor_ += aligned_size;
    return ptr;
  }

  uint8_t* buf_ = nullptr;
  size_t capacity_ = 0;
  size_t cursor_ = 0;
};

// Vector-backed writer used for portable bring-up builds.
class VectorCmdStreamWriter {
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

  uint8_t* data() {
    return buf_.data();
  }
  const uint8_t* data() const {
    return buf_.data();
  }

  size_t bytes_used() const {
    return buf_.size();
  }

  size_t size() const {
    return bytes_used();
  }

  size_t bytes_remaining() const {
    // The vector-backed writer is effectively unbounded.
    return std::numeric_limits<size_t>::max();
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

  template <typename HeaderT>
  HeaderT* append_with_payload(uint32_t opcode, const void* payload, size_t payload_size) {
    static_assert(std::is_trivial<HeaderT>::value, "packets must be POD");
    static_assert(sizeof(HeaderT) >= sizeof(aerogpu_cmd_hdr), "packets must contain aerogpu_cmd_hdr");

    if (payload_size && !payload) {
      return nullptr;
    }

    if (payload_size > std::numeric_limits<size_t>::max() - sizeof(HeaderT)) {
      return nullptr;
    }

    const size_t cmd_size = sizeof(HeaderT) + payload_size;
    uint8_t* base = append_raw(opcode, cmd_size);
    if (!base) {
      return nullptr;
    }
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

// Type-erased wrapper used by the UMD. Defaults to a vector-backed stream for
// portability, but can be rebound to a span for direct WDDM DMA-buffer emission.
class CmdStreamWriter {
 public:
  CmdStreamWriter() {
    reset();
  }

  explicit CmdStreamWriter(uint8_t* buf, size_t capacity) {
    set_span(buf, capacity);
  }

  void set_span(uint8_t* buf, size_t capacity) {
    mode_ = Mode::Span;
    span_.set_buffer(buf, capacity);
    span_.reset();
  }

  void set_vector() {
    mode_ = Mode::Vector;
    vec_.reset();
  }

  void reset() {
    if (mode_ == Mode::Span) {
      span_.reset();
    } else {
      vec_.reset();
    }
  }

  void finalize() {
    if (mode_ == Mode::Span) {
      span_.finalize();
    } else {
      vec_.finalize();
    }
  }

  uint8_t* data() {
    return (mode_ == Mode::Span) ? span_.data() : vec_.data();
  }
  const uint8_t* data() const {
    return (mode_ == Mode::Span) ? span_.data() : vec_.data();
  }

  size_t bytes_used() const {
    return (mode_ == Mode::Span) ? span_.bytes_used() : vec_.bytes_used();
  }

  size_t size() const {
    return bytes_used();
  }

  size_t bytes_remaining() const {
    return (mode_ == Mode::Span) ? span_.bytes_remaining() : vec_.bytes_remaining();
  }

  bool empty() const {
    return (mode_ == Mode::Span) ? span_.empty() : vec_.empty();
  }

  template <typename T>
  T* append_fixed(uint32_t opcode) {
    return (mode_ == Mode::Span) ? span_.append_fixed<T>(opcode) : vec_.append_fixed<T>(opcode);
  }

  template <typename HeaderT>
  HeaderT* append_with_payload(uint32_t opcode, const void* payload, size_t payload_size) {
    return (mode_ == Mode::Span)
        ? span_.append_with_payload<HeaderT>(opcode, payload, payload_size)
        : vec_.append_with_payload<HeaderT>(opcode, payload, payload_size);
  }

 private:
  enum class Mode : uint8_t { Vector, Span };

  Mode mode_ = Mode::Vector;
  VectorCmdStreamWriter vec_;
  SpanCmdStreamWriter span_;
};

} // namespace aerogpu

