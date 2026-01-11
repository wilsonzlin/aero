#pragma once

#include "../include/aerogpu_d3d9_umd.h"

#include <cstdint>
#include <type_traits>
#include <unordered_map>
#include <utility>

// -----------------------------------------------------------------------------
// WDDM allocation-list tracking (Win7 / WDDM 1.1)
// -----------------------------------------------------------------------------
//
// AeroGPU intentionally uses a "no patch list" submission strategy:
// - The UMD leaves the WDDM patch-location list empty.
// - All GPU commands reference allocations by a stable 32-bit `alloc_id`.
// - The UMD writes `alloc_id` into the WDDM allocation-list "slot id" field
//   (D3DDDI_ALLOCATIONLIST::AllocationListSlotId on WDDM 1.1).
// - dxgkrnl may reorder/deduplicate the allocation list before passing it to the
//   KMD, but it preserves the slot id. The KMD can therefore build a sideband
//   lookup table keyed by `AllocationListSlotId`, and the emulator can resolve
//   alloc_id -> guest physical pages without any relocations.
//
// This helper builds the per-submit D3DDDI_ALLOCATIONLIST array, deduplicating
// allocations referenced by a submission, and tracking read/write intent via the
// WDDM WriteOperation flag.

// If we are not building with the Win7 WDK headers, provide a tiny compat
// surface for the types we use so the repo can build in non-WDK environments.
#if !defined(AEROGPU_D3D9_USE_WDK_DDI)
typedef uint32_t D3DKMT_HANDLE;

typedef struct _D3DDDI_ALLOCATIONLIST {
  D3DKMT_HANDLE hAllocation;
  union {
    struct {
      UINT WriteOperation : 1;
      UINT DoNotRetireInstance : 1;
      UINT Offer : 1;
      UINT Reserved : 29;
    };
    UINT Value;
  };
  UINT AllocationListSlotId;
} D3DDDI_ALLOCATIONLIST;
#endif

namespace aerogpu {

using WddmAllocationHandle = decltype(std::declval<D3DDDI_ALLOCATIONLIST>().hAllocation);

// Result of tracking an allocation reference.
enum class AllocRefStatus : uint32_t {
  kOk = 0,
  // The caller must flush/split and start a new submission, then retry.
  kNeedFlush,
  kInvalidArgument,
  kAllocIdOutOfRange,
  kAllocIdCollision,
  kAllocIdMismatch,
};

struct AllocRef {
  UINT alloc_id = 0;
  UINT list_index = 0;
  AllocRefStatus status = AllocRefStatus::kOk;
};

class AllocationListTracker {
 public:
  AllocationListTracker(D3DDDI_ALLOCATIONLIST* list_base,
                        UINT list_capacity,
                        UINT max_allocation_list_slot_id = 0xFFFFu);

  void reset();

  UINT list_len() const {
    return list_len_;
  }
  UINT list_capacity() const {
    return list_capacity_;
  }

  D3DDDI_ALLOCATIONLIST* list_base() const {
    return list_base_;
  }

  AllocRef track_buffer_read(WddmAllocationHandle hAllocation, UINT alloc_id);
  AllocRef track_texture_read(WddmAllocationHandle hAllocation, UINT alloc_id);
  AllocRef track_render_target_write(WddmAllocationHandle hAllocation, UINT alloc_id);

 private:
  struct Entry {
    UINT list_index = 0;
    UINT alloc_id = 0;
  };

  AllocRef track_common(WddmAllocationHandle hAllocation, UINT alloc_id, bool write);

  D3DDDI_ALLOCATIONLIST* list_base_ = nullptr;
  UINT list_capacity_ = 0;
  UINT list_len_ = 0;
  UINT max_allocation_list_slot_id_ = 0;

  std::unordered_map<uint64_t, Entry> handle_to_entry_;
  std::unordered_map<UINT, uint64_t> alloc_id_to_handle_;
};

} // namespace aerogpu
