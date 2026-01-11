#pragma once

#include "../include/aerogpu_d3d9_umd.h"

#include <cstdint>
#include <limits>
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
// - `alloc_id` is carried in the per-allocation private driver data blob
//   (`aerogpu_wddm_alloc_priv`), consumed by the KMD and stored in
//   `DXGK_ALLOCATION::AllocationId`.
// - The KMD builds an allocation table (`aerogpu_alloc_table_header` in
//   `aerogpu_ring.h`) for each submission from the WDDM allocation list and uses
//   `AllocationId` as the lookup key. The host/emulator then resolves
//   alloc_id -> guest physical pages without requiring any relocations.
// - Since the patch-location list is unused, the allocation-list slot-id field
//   is assigned densely (0..N-1) and is not required to match `alloc_id`.
//
// This helper builds the per-submit D3DDDI_ALLOCATIONLIST array, deduplicating
// allocations referenced by a submission, and tracking read/write intent via the
// WDDM WriteOperation flag.

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
  AllocationListTracker() = default;
  AllocationListTracker(D3DDDI_ALLOCATIONLIST* list_base,
                        UINT list_capacity,
                        UINT max_allocation_list_slot_id = std::numeric_limits<UINT>::max());

  // Rebinds the tracker to a new runtime-provided allocation list for a fresh
  // submission. Clears any tracked state and reserves internal maps to the new
  // capacity.
  void rebind(D3DDDI_ALLOCATIONLIST* list_base,
              UINT list_capacity,
              UINT max_allocation_list_slot_id = 0xFFFFu);

  void reset();

  // Rebinds the tracker to a new allocation-list buffer (e.g. if the runtime
  // rotates DMA buffers and returns new list pointers after a submission).
  void rebind(D3DDDI_ALLOCATIONLIST* list_base, UINT list_capacity);

  UINT list_len() const {
    return list_len_;
  }
  UINT list_capacity() const {
    return list_capacity_;
  }

   D3DDDI_ALLOCATIONLIST* list_base() const {
     return list_base_;
   }

  // `share_token` is required to disambiguate alloc_id aliases (same shared allocation opened
  // multiple times) from alloc_id collisions (different allocations accidentally sharing an ID).
  //
  // For non-shared allocations, pass share_token=0.
  AllocRef track_buffer_read(WddmAllocationHandle hAllocation, UINT alloc_id, uint64_t share_token);
  AllocRef track_texture_read(WddmAllocationHandle hAllocation, UINT alloc_id, uint64_t share_token);
  AllocRef track_render_target_write(WddmAllocationHandle hAllocation, UINT alloc_id, uint64_t share_token);

 private:
  struct Entry {
    UINT list_index = 0;
    UINT alloc_id = 0;
    uint64_t share_token = 0;
  };

  AllocRef track_common(WddmAllocationHandle hAllocation, UINT alloc_id, uint64_t share_token, bool write);

  D3DDDI_ALLOCATIONLIST* list_base_ = nullptr;
  UINT list_capacity_ = 0;
  UINT list_len_ = 0;
  UINT max_allocation_list_slot_id_ = 0xFFFFu;

  std::unordered_map<uint64_t, Entry> handle_to_entry_;
  std::unordered_map<UINT, uint64_t> alloc_id_to_handle_;
};

} // namespace aerogpu
