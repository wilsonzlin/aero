#include "aerogpu_wddm_alloc_list.h"

#include <algorithm>
#include <cstring>

namespace aerogpu {
namespace {

template <typename T, typename = void>
struct has_member_allocation_list_slot_id : std::false_type {};

template <typename T>
struct has_member_allocation_list_slot_id<T, std::void_t<decltype(std::declval<T&>().AllocationListSlotId)>>
    : std::true_type {};

template <typename T, typename = void>
struct has_member_slot_id : std::false_type {};

template <typename T>
struct has_member_slot_id<T, std::void_t<decltype(std::declval<T&>().SlotId)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_value : std::false_type {};

template <typename T>
struct has_member_value<T, std::void_t<decltype(std::declval<T&>().Value)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_write_operation : std::false_type {};

template <typename T>
struct has_member_write_operation<T, std::void_t<decltype(std::declval<T&>().WriteOperation)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_flags : std::false_type {};

template <typename T>
struct has_member_flags<T, std::void_t<decltype(std::declval<T&>().Flags)>> : std::true_type {};

template <typename FlagsT>
void set_write_operation_in_flags(FlagsT& flags, bool write) {
  if constexpr (std::is_integral_v<std::remove_reference_t<FlagsT>>) {
    if (write) {
      flags |= 0x1u;
    } else {
      flags &= ~0x1u;
    }
  } else if constexpr (has_member_value<FlagsT>::value &&
                       std::is_integral_v<std::remove_reference_t<decltype(std::declval<FlagsT&>().Value)>>) {
    if (write) {
      flags.Value |= 0x1u;
    } else {
      flags.Value &= ~0x1u;
    }
  } else if constexpr (has_member_write_operation<FlagsT>::value) {
    flags.WriteOperation = write ? 1u : 0u;
  } else {
    static_assert(has_member_write_operation<FlagsT>::value,
                  "D3DDDI_ALLOCATIONLIST flags are missing WriteOperation (WDDM 1.1 required)");
  }
}

template <typename AllocationListT>
void set_allocation_list_slot_id(AllocationListT& entry, UINT slot_id) {
  // NOTE: This is intentionally a function template so the non-selected branch
  // of `if constexpr` can be ill-formed when compiling without WDK headers.
  if constexpr (has_member_allocation_list_slot_id<AllocationListT>::value) {
    entry.AllocationListSlotId = slot_id;
  } else if constexpr (has_member_slot_id<AllocationListT>::value) {
    entry.SlotId = slot_id;
  } else {
    static_assert(has_member_allocation_list_slot_id<AllocationListT>::value || has_member_slot_id<AllocationListT>::value,
                  "D3DDDI_ALLOCATIONLIST is missing a slot-id field (WDDM 1.1 required)");
  }
}

template <typename AllocationListT>
void set_write_operation(AllocationListT& entry, bool write) {
  if constexpr (has_member_value<AllocationListT>::value) {
    set_write_operation_in_flags(entry.Value, write);
  } else if constexpr (has_member_flags<AllocationListT>::value) {
    set_write_operation_in_flags(entry.Flags, write);
  } else if constexpr (has_member_write_operation<AllocationListT>::value) {
    entry.WriteOperation = write ? 1u : 0u;
  } else {
    static_assert(has_member_value<AllocationListT>::value || has_member_flags<AllocationListT>::value ||
                      has_member_write_operation<AllocationListT>::value,
                  "D3DDDI_ALLOCATIONLIST is missing a write-operation flag field (WDDM 1.1 required)");
  }
}

template <typename FlagsT>
bool get_write_operation_from_flags(const FlagsT& flags) {
  if constexpr (std::is_integral_v<std::remove_reference_t<FlagsT>>) {
    return (static_cast<uint32_t>(flags) & 0x1u) != 0;
  } else if constexpr (has_member_value<FlagsT>::value &&
                       std::is_integral_v<std::remove_reference_t<decltype(std::declval<FlagsT&>().Value)>>) {
    return (static_cast<uint32_t>(flags.Value) & 0x1u) != 0;
  } else if constexpr (has_member_write_operation<FlagsT>::value) {
    return flags.WriteOperation != 0;
  } else {
    static_assert(has_member_write_operation<FlagsT>::value,
                  "D3DDDI_ALLOCATIONLIST flags are missing WriteOperation (WDDM 1.1 required)");
  }
}

template <typename AllocationListT>
bool get_write_operation(const AllocationListT& entry) {
  if constexpr (has_member_value<AllocationListT>::value) {
    return get_write_operation_from_flags(entry.Value);
  } else if constexpr (has_member_flags<AllocationListT>::value) {
    return get_write_operation_from_flags(entry.Flags);
  } else if constexpr (has_member_write_operation<AllocationListT>::value) {
    return entry.WriteOperation != 0;
  } else {
    static_assert(has_member_value<AllocationListT>::value || has_member_flags<AllocationListT>::value ||
                      has_member_write_operation<AllocationListT>::value,
                  "D3DDDI_ALLOCATIONLIST is missing a write-operation flag field (WDDM 1.1 required)");
  }
}

uint64_t handle_key(WddmAllocationHandle hAllocation) {
  // On Win7/WDDM 1.1 this is a 32-bit D3DKMT_HANDLE. Treat it as a 64-bit key to
  // keep the hash maps trivially portable across builds.
  return static_cast<uint64_t>(hAllocation);
}

} // namespace

AllocationListTracker::AllocationListTracker(D3DDDI_ALLOCATIONLIST* list_base,
                                             UINT list_capacity,
                                             UINT max_allocation_list_slot_id)
    : list_base_(list_base),
      list_capacity_(list_capacity),
      list_len_(0),
      max_allocation_list_slot_id_(max_allocation_list_slot_id) {
  handle_to_entry_.reserve(list_capacity_);
  alloc_id_to_handle_.reserve(list_capacity_);
}

void AllocationListTracker::rebind(D3DDDI_ALLOCATIONLIST* list_base,
                                   UINT list_capacity,
                                   UINT max_allocation_list_slot_id) {
  list_base_ = list_base;
  list_capacity_ = list_capacity;
  max_allocation_list_slot_id_ = max_allocation_list_slot_id;

  reset();

  handle_to_entry_.reserve(list_capacity_);
  alloc_id_to_handle_.reserve(list_capacity_);
}

void AllocationListTracker::reset() {
  list_len_ = 0;
  handle_to_entry_.clear();
  alloc_id_to_handle_.clear();
}

void AllocationListTracker::rebind(D3DDDI_ALLOCATIONLIST* list_base, UINT list_capacity) {
  list_base_ = list_base;
  list_capacity_ = list_capacity;

  // Preserve the current max slot id; callers can construct a new tracker if
  // they need different semantics.
  handle_to_entry_.reserve(list_capacity_);
  alloc_id_to_handle_.reserve(list_capacity_);

  reset();
}

AllocRef AllocationListTracker::track_buffer_read(WddmAllocationHandle hAllocation,
                                                  UINT alloc_id,
                                                  uint64_t share_token) {
  return track_common(hAllocation, alloc_id, share_token, /*write=*/false);
}

AllocRef AllocationListTracker::track_texture_read(WddmAllocationHandle hAllocation,
                                                   UINT alloc_id,
                                                   uint64_t share_token) {
  return track_common(hAllocation, alloc_id, share_token, /*write=*/false);
}

AllocRef AllocationListTracker::track_render_target_write(WddmAllocationHandle hAllocation,
                                                          UINT alloc_id,
                                                          uint64_t share_token) {
  return track_common(hAllocation, alloc_id, share_token, /*write=*/true);
}

AllocRef AllocationListTracker::track_common(WddmAllocationHandle hAllocation,
                                             UINT alloc_id,
                                             uint64_t share_token,
                                             bool write) {
  AllocRef out{};

  if (!list_base_ || list_capacity_ == 0) {
    out.status = AllocRefStatus::kInvalidArgument;
    return out;
  }
  if (hAllocation == 0) {
    out.status = AllocRefStatus::kInvalidArgument;
    return out;
  }
  if (alloc_id == 0) {
    // Reserve alloc_id=0 as "null" for command-stream fields.
    out.status = AllocRefStatus::kInvalidArgument;
    return out;
  }
  const uint64_t key = handle_key(hAllocation);
  auto it = handle_to_entry_.find(key);
  if (it != handle_to_entry_.end()) {
    const Entry& e = it->second;
    if (e.alloc_id != alloc_id) {
      out.status = AllocRefStatus::kAllocIdMismatch;
      return out;
    }
    if (e.share_token != share_token) {
      out.status = AllocRefStatus::kAllocIdMismatch;
      return out;
    }

    out.alloc_id = e.alloc_id;
    out.list_index = e.list_index;
    out.status = AllocRefStatus::kOk;

    if (write) {
      // Upgrade read->write if needed; never downgrade.
      set_write_operation(list_base_[e.list_index], true);
    }
    return out;
  }

  auto slot_it = alloc_id_to_handle_.find(alloc_id);
  if (slot_it != alloc_id_to_handle_.end()) {
    // Another handle already claimed this alloc_id in the current submission.
    //
    // This can legitimately happen for shared resources (same kernel allocation
    // opened multiple times, yielding distinct per-process handles). We treat it
    // as an alias and deduplicate by alloc_id, but only if it refers to the same
    // underlying shared allocation (identified by share_token). Otherwise this is
    // a collision and must be surfaced as a deterministic error (never silently
    // alias distinct allocations).
    const uint64_t existing_key = slot_it->second;
    auto existing_it = handle_to_entry_.find(existing_key);
    if (existing_it == handle_to_entry_.end()) {
      out.status = AllocRefStatus::kInvalidArgument;
      return out;
    }
    const Entry& existing = existing_it->second;
    if (existing.alloc_id != alloc_id) {
      out.status = AllocRefStatus::kAllocIdMismatch;
      return out;
    }
    if (existing.share_token == 0 || share_token == 0 || existing.share_token != share_token) {
      out.status = AllocRefStatus::kAllocIdCollision;
      return out;
    }

    handle_to_entry_.emplace(key, existing);

    out.alloc_id = alloc_id;
    out.list_index = existing.list_index;
    out.status = AllocRefStatus::kOk;

    if (write) {
      set_write_operation(list_base_[existing.list_index], true);
    }
    return out;
  }

  if (list_len_ == list_capacity_) {
    out.status = AllocRefStatus::kNeedFlush;
    return out;
  }

  if (list_len_ > max_allocation_list_slot_id_) {
    // Slot IDs are assigned densely (0..N-1) so exceeding the KMD-advertised
    // max slot id just means the submission must be split.
    out.status = AllocRefStatus::kNeedFlush;
    return out;
  }

  const UINT idx = list_len_++;
  D3DDDI_ALLOCATIONLIST& entry = list_base_[idx];
  std::memset(&entry, 0, sizeof(entry));
  entry.hAllocation = hAllocation;

  // Default: read-only.
  set_write_operation(entry, write);
  // WDDM uses AllocationListSlotId to index patch list relocations. AeroGPU uses
  // a no-patch-list submission strategy, but some runtimes still validate the
  // slot-id range. Use the list index as the slot id and carry the stable
  // `alloc_id` separately in the allocation's private driver data.
  set_allocation_list_slot_id(entry, idx);

  handle_to_entry_.emplace(key, Entry{idx, alloc_id, share_token});
  alloc_id_to_handle_.emplace(alloc_id, key);

  out.alloc_id = alloc_id;
  out.list_index = idx;
  out.status = AllocRefStatus::kOk;
  return out;
}

std::vector<AllocationListTracker::TrackedAllocation> AllocationListTracker::snapshot_tracked_allocations() const {
  std::vector<TrackedAllocation> out;
  if (!list_base_ || list_len_ == 0) {
    return out;
  }

  out.resize(list_len_);
  std::vector<uint8_t> seen(list_len_, 0);
  for (const auto& it : handle_to_entry_) {
    const Entry& e = it.second;
    if (e.list_index >= list_len_) {
      continue;
    }
    if (seen[e.list_index]) {
      continue;
    }
    seen[e.list_index] = 1;
    const D3DDDI_ALLOCATIONLIST& list_entry = list_base_[e.list_index];
    out[e.list_index] = TrackedAllocation{
        list_entry.hAllocation,
        e.alloc_id,
        e.share_token,
        get_write_operation(list_entry),
    };
  }

  // If something went wrong and we didn't see all indices, compact the output so
  // callers don't observe uninitialized entries.
  if (std::find(seen.begin(), seen.end(), 0) != seen.end()) {
    std::vector<TrackedAllocation> compact;
    compact.reserve(list_len_);
    for (UINT i = 0; i < list_len_; ++i) {
      if (seen[i]) {
        compact.push_back(out[i]);
      }
    }
    return compact;
  }

  return out;
}

bool AllocationListTracker::replay_tracked_allocations(const std::vector<TrackedAllocation>& allocs) {
  if (allocs.empty()) {
    return true;
  }
  if (!list_base_ || list_capacity_ == 0) {
    return false;
  }
  // Reserve space for a conservative number of unique allocations: snapshot
  // entries correspond to allocation-list slots, so the required capacity is the
  // snapshot size.
  if (allocs.size() > list_capacity_effective()) {
    return false;
  }

  for (const TrackedAllocation& a : allocs) {
    AllocRef r{};
    if (a.write) {
      r = track_render_target_write(a.hAllocation, a.alloc_id, a.share_token);
    } else {
      r = track_buffer_read(a.hAllocation, a.alloc_id, a.share_token);
    }
    if (r.status != AllocRefStatus::kOk) {
      return false;
    }
  }
  return true;
}

} // namespace aerogpu
