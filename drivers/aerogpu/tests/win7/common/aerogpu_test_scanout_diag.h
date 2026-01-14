#pragma once

#include "aerogpu_test_kmt.h"

namespace aerogpu_test {

struct AerogpuScanoutDiag {
  bool ok;
  bool flags_valid;
  bool post_display_ownership_released;
  uint32_t flags_u32;
  uint32_t cached_enable;
  uint32_t mmio_enable;
};

static inline void InitAerogpuScanoutDiag(AerogpuScanoutDiag* out_diag) {
  if (!out_diag) {
    return;
  }
  out_diag->ok = false;
  out_diag->flags_valid = false;
  out_diag->post_display_ownership_released = false;
  out_diag->flags_u32 = 0;
  out_diag->cached_enable = 0;
  out_diag->mmio_enable = 0;
}

static inline bool TryQueryAerogpuScanoutDiagWithKmt(const aerogpu_test::kmt::D3DKMT_FUNCS* kmt,
                                                     uint32_t adapter,
                                                     uint32_t vidpn_source_id,
                                                     AerogpuScanoutDiag* out_diag) {
  if (!kmt || !out_diag) {
    return false;
  }
  InitAerogpuScanoutDiag(out_diag);

  aerogpu_escape_query_scanout_out_v2 q;
  aerogpu_test::kmt::NTSTATUS st = 0;
  const bool ok = aerogpu_test::kmt::AerogpuQueryScanoutV2(
      kmt, (aerogpu_test::kmt::D3DKMT_HANDLE)adapter, vidpn_source_id, &q, &st);
  if (!ok) {
    return false;
  }

  out_diag->ok = true;
  out_diag->cached_enable = (uint32_t)q.base.cached_enable;
  out_diag->mmio_enable = (uint32_t)q.base.mmio_enable;
  out_diag->flags_u32 = (uint32_t)q.base.reserved0;

  const bool have_v2 = (q.base.hdr.size >= sizeof(aerogpu_escape_query_scanout_out_v2));
  const bool flags_valid = have_v2 && ((out_diag->flags_u32 & AEROGPU_DBGCTL_QUERY_SCANOUT_FLAGS_VALID) != 0);
  out_diag->flags_valid = flags_valid;
  if (flags_valid) {
    out_diag->post_display_ownership_released =
        ((out_diag->flags_u32 & AEROGPU_DBGCTL_QUERY_SCANOUT_FLAG_POST_DISPLAY_OWNERSHIP_RELEASED) != 0);
  }
  return true;
}

static inline bool TryQueryAerogpuScanoutDiag(uint32_t adapter,
                                              uint32_t vidpn_source_id,
                                              AerogpuScanoutDiag* out_diag) {
  InitAerogpuScanoutDiag(out_diag);
  if (!out_diag) {
    return false;
  }

  aerogpu_test::kmt::D3DKMT_FUNCS kmt;
  std::string kmt_err;
  if (!aerogpu_test::kmt::LoadD3DKMT(&kmt, &kmt_err)) {
    return false;
  }

  const bool ok = TryQueryAerogpuScanoutDiagWithKmt(&kmt, adapter, vidpn_source_id, out_diag);
  aerogpu_test::kmt::UnloadD3DKMT(&kmt);
  return ok;
}

}  // namespace aerogpu_test
