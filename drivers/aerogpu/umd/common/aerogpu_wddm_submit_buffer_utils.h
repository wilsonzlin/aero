#pragma once

#include <cstdint>

namespace aerogpu {

// When a WDDM callback exposes a base DMA buffer pointer + size (pDmaBuffer /
// DmaBufferSize) *and* an explicit command buffer pointer (pCommandBuffer), the
// command buffer may be an offset within the DMA buffer.
//
// In that case, the effective writable command-buffer capacity is reduced by
// the offset so command emission does not overrun the runtime's reserved prefix
// bytes.
//
// If the pointers do not form a clear "command buffer is within DMA buffer"
// relationship, this returns the input size unchanged.
inline uint32_t AdjustCommandBufferSizeFromDmaBuffer(void* dma_buffer, void* command_buffer, uint32_t dma_buffer_bytes) {
  if (!dma_buffer || !command_buffer) {
    return dma_buffer_bytes;
  }
  if (dma_buffer_bytes == 0) {
    return 0;
  }
  if (dma_buffer == command_buffer) {
    return dma_buffer_bytes;
  }

  const uintptr_t base = reinterpret_cast<uintptr_t>(dma_buffer);
  const uintptr_t cmd = reinterpret_cast<uintptr_t>(command_buffer);
  if (cmd < base) {
    return dma_buffer_bytes;
  }

  const uintptr_t offset = cmd - base;
  if (offset > static_cast<uintptr_t>(dma_buffer_bytes)) {
    return dma_buffer_bytes;
  }

  return dma_buffer_bytes - static_cast<uint32_t>(offset);
}

} // namespace aerogpu

