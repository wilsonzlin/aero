#pragma once

#include <cstdint>
#include <cstring>
#include <type_traits>

#if defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS
  #include <d3dkmthk.h>
  #include <windows.h>
#endif

namespace aerogpu {

// Small WDDM 1.1 allocation-list helper shared by the Win7 D3D10/11 UMDs.
//
// This module intentionally avoids relying on a specific WDK header revision:
// different WDKs use different field names/layouts for D3DDDI_ALLOCATIONLIST.
namespace wddm {
#if !(defined(_WIN32) && defined(AEROGPU_UMD_USE_WDK_HEADERS) && AEROGPU_UMD_USE_WDK_HEADERS)
inline void InitAllocationListEntry(...) {}
#else
namespace detail {

template <typename T, typename = void>
struct has_member_allocation_list_slot_id : std::false_type {};
template <typename T>
struct has_member_allocation_list_slot_id<T, std::void_t<decltype(std::declval<T&>().AllocationListSlotId)>> : std::true_type {};

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
inline void set_write_operation_in_flags(FlagsT& flags, bool write) {
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
inline void set_allocation_list_slot_id(AllocationListT& entry, UINT slot_id) {
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
inline void set_write_operation(AllocationListT& entry, bool write) {
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

} // namespace detail

inline void InitAllocationListEntry(D3DDDI_ALLOCATIONLIST& entry, decltype(entry.hAllocation) hAllocation, UINT slot_id, bool write) {
  std::memset(&entry, 0, sizeof(entry));
  entry.hAllocation = hAllocation;

  // Default to read-only; upgrade to write when required.
  detail::set_write_operation(entry, write);
  detail::set_allocation_list_slot_id(entry, slot_id);
}

#endif
} // namespace wddm
} // namespace aerogpu
