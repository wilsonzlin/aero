#include <cstdint>
#include <cstdio>

#include "aerogpu_wddm_submit_buffer_utils.h"

namespace {

bool Check(bool cond, const char* msg) {
  if (!cond) {
    std::fprintf(stderr, "FAIL: %s\n", msg);
    return false;
  }
  return true;
}

} // namespace

int main() {
  void* base = reinterpret_cast<void*>(static_cast<uintptr_t>(0x1000));

  if (!Check(aerogpu::AdjustCommandBufferSizeFromDmaBuffer(base, base, 64) == 64, "same ptr")) {
    return 1;
  }
  if (!Check(aerogpu::AdjustCommandBufferSizeFromDmaBuffer(base, reinterpret_cast<void*>(static_cast<uintptr_t>(0x1010)), 64) == 48,
             "offset within range")) {
    return 1;
  }
  if (!Check(aerogpu::AdjustCommandBufferSizeFromDmaBuffer(base, reinterpret_cast<void*>(static_cast<uintptr_t>(0x1040)), 64) == 0,
             "offset == size")) {
    return 1;
  }
  if (!Check(aerogpu::AdjustCommandBufferSizeFromDmaBuffer(base, reinterpret_cast<void*>(static_cast<uintptr_t>(0x1050)), 64) == 64,
             "offset > size leaves unchanged")) {
    return 1;
  }
  if (!Check(aerogpu::AdjustCommandBufferSizeFromDmaBuffer(base, reinterpret_cast<void*>(static_cast<uintptr_t>(0x0ff0)), 64) == 64,
             "cmd < base leaves unchanged")) {
    return 1;
  }
  if (!Check(aerogpu::AdjustCommandBufferSizeFromDmaBuffer(nullptr, base, 64) == 64, "null dma ptr")) {
    return 1;
  }
  if (!Check(aerogpu::AdjustCommandBufferSizeFromDmaBuffer(base, nullptr, 64) == 64, "null cmd ptr")) {
    return 1;
  }
  if (!Check(aerogpu::AdjustCommandBufferSizeFromDmaBuffer(base, base, 0) == 0, "zero size")) {
    return 1;
  }

  return 0;
}

