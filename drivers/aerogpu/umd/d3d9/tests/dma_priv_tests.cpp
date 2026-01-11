#include <cstdint>
#include <cstdio>
#include <cstring>

#include "aerogpu_d3d9_dma_priv.h"

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
  alignas(8) uint8_t buf[32];
  std::memset(buf, 0xCC, sizeof(buf));

  if (!Check(aerogpu::InitWin7DmaBufferPrivateData(buf, /*bytes=*/16, /*is_present=*/false),
             "InitWin7DmaBufferPrivateData(render)")) {
    return 1;
  }

  const auto* priv = reinterpret_cast<const AEROGPU_DMA_PRIV*>(buf);
  if (!Check(priv->Type == AEROGPU_SUBMIT_RENDER, "render Type")) {
    return 1;
  }
  if (!Check(priv->Reserved0 == 0, "render Reserved0")) {
    return 1;
  }
  if (!Check(priv->MetaHandle == 0, "render MetaHandle")) {
    return 1;
  }
  if (!Check(buf[16] == 0xCC && buf[31] == 0xCC, "bytes beyond ABI prefix untouched")) {
    return 1;
  }

  std::memset(buf, 0xCC, sizeof(buf));
  if (!Check(aerogpu::InitWin7DmaBufferPrivateData(buf, /*bytes=*/16, /*is_present=*/true),
             "InitWin7DmaBufferPrivateData(present)")) {
    return 1;
  }
  priv = reinterpret_cast<const AEROGPU_DMA_PRIV*>(buf);
  if (!Check(priv->Type == AEROGPU_SUBMIT_PRESENT, "present Type")) {
    return 1;
  }

  std::memset(buf, 0xCC, sizeof(buf));
  if (!Check(!aerogpu::InitWin7DmaBufferPrivateData(buf, /*bytes=*/8, /*is_present=*/false),
             "reject too-small private data size")) {
    return 1;
  }

  if (!Check(aerogpu::ClampWin7DmaBufferPrivateDataSize(8) == 8, "Clamp: below expected")) {
    return 1;
  }
  if (!Check(aerogpu::ClampWin7DmaBufferPrivateDataSize(16) == 16, "Clamp: equal")) {
    return 1;
  }
  if (!Check(aerogpu::ClampWin7DmaBufferPrivateDataSize(64) == 16, "Clamp: above expected")) {
    return 1;
  }

  return 0;
}

