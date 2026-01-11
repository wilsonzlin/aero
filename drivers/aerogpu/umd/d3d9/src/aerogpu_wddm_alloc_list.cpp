#include "aerogpu_wddm_alloc_list.h"

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

void AllocationListTracker::reset() {
  list_len_ = 0;
  handle_to_entry_.clear();
  alloc_id_to_handle_.clear();
}

AllocRef AllocationListTracker::track_buffer_read(WddmAllocationHandle hAllocation, UINT alloc_id) {
  return track_common(hAllocation, alloc_id, /*write=*/false);
}

AllocRef AllocationListTracker::track_texture_read(WddmAllocationHandle hAllocation, UINT alloc_id) {
  return track_common(hAllocation, alloc_id, /*write=*/false);
}

AllocRef AllocationListTracker::track_render_target_write(WddmAllocationHandle hAllocation, UINT alloc_id) {
  return track_common(hAllocation, alloc_id, /*write=*/true);
}

AllocRef AllocationListTracker::track_common(WddmAllocationHandle hAllocation, UINT alloc_id, bool write) {
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
  if (alloc_id > max_allocation_list_slot_id_) {
    out.status = AllocRefStatus::kAllocIdOutOfRange;
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
    // as an alias and deduplicate by alloc_id.
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

  const UINT idx = list_len_++;
  D3DDDI_ALLOCATIONLIST& entry = list_base_[idx];
  std::memset(&entry, 0, sizeof(entry));
  entry.hAllocation = hAllocation;

  // Default: read-only.
  set_write_operation(entry, write);
  set_allocation_list_slot_id(entry, alloc_id);

  handle_to_entry_.emplace(key, Entry{idx, alloc_id});
  alloc_id_to_handle_.emplace(alloc_id, key);

  out.alloc_id = alloc_id;
  out.list_index = idx;
  out.status = AllocRefStatus::kOk;
  return out;
}

} // namespace aerogpu
