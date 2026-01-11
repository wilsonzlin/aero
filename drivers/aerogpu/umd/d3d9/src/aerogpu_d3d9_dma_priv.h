#pragma once

#include <cstdint>
#include <cstring>

#include "aerogpu_win7_abi.h"

namespace aerogpu {

// Initializes the runtime-provided DMA private-data blob (UMD -> dxgkrnl -> KMD)
// for the upcoming submission.
//
// The bytes are copied by dxgkrnl into kernel mode for each submit. This helper
// ensures that the copied prefix is always deterministic and never contains
// uninitialized user-mode bytes.
//
// Returns false if the pointer/size are invalid for the Win7 ABI.
inline bool InitWin7DmaBufferPrivateData(void* pDmaBufferPrivateData, uint32_t DmaBufferPrivateDataSize, bool is_present) {
  if (!pDmaBufferPrivateData) {
    return false;
  }
  if (DmaBufferPrivateDataSize < AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES) {
    return false;
  }

  // Security: always zero the ABI prefix first (matches the D3D10/11 WDDM submit
  // path and guarantees that any bytes not explicitly written remain 0).
  std::memset(pDmaBufferPrivateData, 0, AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES);

  // Stamp a deterministic header so SubmitCommandCb-only runtimes still convey a
  // valid submission type to DxgkDdiSubmitCommand.
  AEROGPU_DMA_PRIV priv{};
  priv.Type = is_present ? AEROGPU_SUBMIT_PRESENT : AEROGPU_SUBMIT_RENDER;
  priv.Reserved0 = 0;
  priv.MetaHandle = 0;
  std::memcpy(pDmaBufferPrivateData, &priv, sizeof(priv));
  return true;
}

inline uint32_t ClampWin7DmaBufferPrivateDataSize(uint32_t bytes) {
  if (bytes > AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES) {
    return AEROGPU_WIN7_DMA_BUFFER_PRIVATE_DATA_SIZE_BYTES;
  }
  return bytes;
}

} // namespace aerogpu

