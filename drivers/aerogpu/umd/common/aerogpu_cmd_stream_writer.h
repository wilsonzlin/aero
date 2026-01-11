#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <type_traits>
#include <vector>

#include "../../protocol/aerogpu_cmd.h"

namespace aerogpu {

inline size_t align_up(size_t v, size_t a) {
  return (v + (a - 1)) & ~(a - 1);
}

enum class CmdStreamError : uint32_t {
  kOk = 0,
  kNoBuffer = 1,
  kInsufficientSpace = 2,
  kInvalidArgument = 3,
  kSizeTooLarge = 4,
};

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
    cursor_ = 0;
    error_ = CmdStreamError::kOk;
  }

  void reset() {
    cursor_ = 0;
    error_ = CmdStreamError::kOk;
    if (!buf_) {
      error_ = CmdStreamError::kNoBuffer;
      return;
    }
    if (capacity_ < sizeof(aerogpu_cmd_stream_header)) {
      error_ = CmdStreamError::kInsufficientSpace;
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

  size_t len() const {
    return bytes_used();
  }

  // Compatibility with existing `CmdWriter` call sites.
  size_t size() const {
    return bytes_used();
  }

  CmdStreamError error() const {
    return error_;
  }

  size_t bytes_remaining() const {
    if (cursor_ > capacity_) {
      return 0;
    }
    return capacity_ - cursor_;
  }

  bool empty() const {
    return cursor_ <= sizeof(aerogpu_cmd_stream_header);
  }

  void finalize() {
    if (!buf_) {
      error_ = CmdStreamError::kNoBuffer;
      return;
    }
    if (capacity_ < sizeof(aerogpu_cmd_stream_header)) {
      error_ = CmdStreamError::kInsufficientSpace;
      return;
    }

    if (cursor_ > std::numeric_limits<uint32_t>::max()) {
      error_ = CmdStreamError::kSizeTooLarge;
      return;
    }

    auto* stream = reinterpret_cast<aerogpu_cmd_stream_header*>(buf_);
    stream->size_bytes = static_cast<uint32_t>(cursor_);
  }

  template <typename T>
  T* TryAppendFixed(uint32_t opcode) {
    return append_fixed<T>(opcode);
  }

  template <typename HeaderT>
  HeaderT* TryAppendWithPayload(uint32_t opcode, const void* payload, size_t payload_size) {
    return append_with_payload<HeaderT>(opcode, payload, payload_size);
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

    if (error_ != CmdStreamError::kOk) {
      return nullptr;
    }

    if (payload_size && !payload) {
      error_ = CmdStreamError::kInvalidArgument;
      return nullptr;
    }

    if (payload_size > std::numeric_limits<size_t>::max() - sizeof(HeaderT)) {
      error_ = CmdStreamError::kSizeTooLarge;
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
    if (error_ != CmdStreamError::kOk) {
      return nullptr;
    }

    if (!buf_) {
      error_ = CmdStreamError::kNoBuffer;
      return nullptr;
    }
    if (capacity_ < sizeof(aerogpu_cmd_stream_header)) {
      error_ = CmdStreamError::kInsufficientSpace;
      return nullptr;
    }

    // If a buffer was rebound via set_buffer(), ensure the stream header is
    // re-initialized before we emit packets.
    if (cursor_ == 0) {
      reset();
      if (error_ != CmdStreamError::kOk) {
        return nullptr;
      }
    }

    if (cmd_size < sizeof(aerogpu_cmd_hdr)) {
      error_ = CmdStreamError::kInvalidArgument;
      return nullptr;
    }

    if (cmd_size > std::numeric_limits<size_t>::max() - 3) {
      error_ = CmdStreamError::kSizeTooLarge;
      return nullptr;
    }
    const size_t aligned_size = align_up(cmd_size, 4);

    if (aligned_size > std::numeric_limits<uint32_t>::max()) {
      error_ = CmdStreamError::kSizeTooLarge;
      return nullptr;
    }

    if (cursor_ > capacity_ || aligned_size > capacity_ - cursor_) {
      error_ = CmdStreamError::kInsufficientSpace;
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
  CmdStreamError error_ = CmdStreamError::kOk;
};

// Vector-backed writer used for portable bring-up builds.
class VectorCmdStreamWriter {
 public:
  void reset() {
    error_ = CmdStreamError::kOk;
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

  size_t len() const {
    return bytes_used();
  }

  size_t size() const {
    return bytes_used();
  }

  CmdStreamError error() const {
    return error_;
  }

  size_t bytes_remaining() const {
    // The vector-backed writer is effectively unbounded.
    return std::numeric_limits<size_t>::max();
  }

  bool empty() const {
    return buf_.size() <= sizeof(aerogpu_cmd_stream_header);
  }

  template <typename T>
  T* TryAppendFixed(uint32_t opcode) {
    return append_fixed<T>(opcode);
  }

  template <typename HeaderT>
  HeaderT* TryAppendWithPayload(uint32_t opcode, const void* payload, size_t payload_size) {
    return append_with_payload<HeaderT>(opcode, payload, payload_size);
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

    if (error_ != CmdStreamError::kOk) {
      return nullptr;
    }

    if (payload_size && !payload) {
      error_ = CmdStreamError::kInvalidArgument;
      return nullptr;
    }

    if (payload_size > std::numeric_limits<size_t>::max() - sizeof(HeaderT)) {
      error_ = CmdStreamError::kSizeTooLarge;
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
    if (buf_.size() > std::numeric_limits<uint32_t>::max()) {
      error_ = CmdStreamError::kSizeTooLarge;
      return;
    }
    auto* stream = reinterpret_cast<aerogpu_cmd_stream_header*>(buf_.data());
    stream->size_bytes = static_cast<uint32_t>(buf_.size());
  }

 private:
  uint8_t* append_raw(uint32_t opcode, size_t cmd_size) {
    if (error_ != CmdStreamError::kOk) {
      return nullptr;
    }

    // Ensure the stream header is present even if callers forgot to reset().
    if (buf_.empty()) {
      reset();
    }

    if (cmd_size < sizeof(aerogpu_cmd_hdr)) {
      error_ = CmdStreamError::kInvalidArgument;
      return nullptr;
    }

    const size_t aligned_size = align_up(cmd_size, 4);
    if (aligned_size > std::numeric_limits<uint32_t>::max()) {
      error_ = CmdStreamError::kSizeTooLarge;
      return nullptr;
    }

    const size_t offset = buf_.size();
    buf_.resize(offset + aligned_size, 0);

    auto* hdr = reinterpret_cast<aerogpu_cmd_hdr*>(buf_.data() + offset);
    hdr->opcode = opcode;
    hdr->size_bytes = static_cast<uint32_t>(aligned_size);
    return reinterpret_cast<uint8_t*>(hdr);
  }

  std::vector<uint8_t> buf_;
  CmdStreamError error_ = CmdStreamError::kOk;
};

// Type-erased wrapper used by the UMD. Defaults to a vector-backed stream for
// portability, but can be rebound to a span for direct WDDM DMA-buffer emission.
class CmdStreamWriter {
 public:
  CmdStreamWriter() = default;

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

  CmdStreamError Reset() {
    reset();
    return error();
  }

  void finalize() {
    if (mode_ == Mode::Span) {
      span_.finalize();
    } else {
      vec_.finalize();
    }
  }

  CmdStreamError Finalize() {
    finalize();
    return error();
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

  size_t len() const {
    return bytes_used();
  }

  size_t size() const {
    return bytes_used();
  }

  CmdStreamError error() const {
    return (mode_ == Mode::Span) ? span_.error() : vec_.error();
  }

  size_t bytes_remaining() const {
    return (mode_ == Mode::Span) ? span_.bytes_remaining() : vec_.bytes_remaining();
  }

  bool empty() const {
    return (mode_ == Mode::Span) ? span_.empty() : vec_.empty();
  }

  template <typename T>
  T* TryAppendFixed(uint32_t opcode) {
    return (mode_ == Mode::Span) ? span_.TryAppendFixed<T>(opcode) : vec_.TryAppendFixed<T>(opcode);
  }

  template <typename HeaderT>
  HeaderT* TryAppendWithPayload(uint32_t opcode, const void* payload, size_t payload_size) {
    return (mode_ == Mode::Span)
        ? span_.TryAppendWithPayload<HeaderT>(opcode, payload, payload_size)
        : vec_.TryAppendWithPayload<HeaderT>(opcode, payload, payload_size);
  }

  template <typename T>
  T* append_fixed(uint32_t opcode) {
    T* packet = TryAppendFixed<T>(opcode);
    return packet ? packet : SinkAs<T>();
  }

  template <typename HeaderT>
  HeaderT* append_with_payload(uint32_t opcode, const void* payload, size_t payload_size) {
    HeaderT* packet = TryAppendWithPayload<HeaderT>(opcode, payload, payload_size);
    return packet ? packet : SinkAs<HeaderT>();
  }

 private:
  enum class Mode : uint8_t { Vector, Span };

  template <typename T>
  T* SinkAs() {
    static_assert(sizeof(T) <= sizeof(sink_), "increase sink_ size to cover packet type");
    std::memset(sink_, 0, sizeof(sink_));
    return reinterpret_cast<T*>(sink_);
  }

  Mode mode_ = Mode::Vector;
  VectorCmdStreamWriter vec_;
  SpanCmdStreamWriter span_;

  alignas(8) uint8_t sink_[256] = {};
};

} // namespace aerogpu
