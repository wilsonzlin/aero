#include "aerogpu_wddm_alloc.h"

#include <cstring>
#include <type_traits>
#include <utility>

namespace aerogpu {

#if defined(_WIN32) && defined(AEROGPU_D3D9_USE_WDK_DDI) && AEROGPU_D3D9_USE_WDK_DDI

namespace {

template <typename Fn>
struct fn_first_param;

template <typename Ret, typename Arg0, typename... Rest>
struct fn_first_param<Ret(__stdcall*)(Arg0, Rest...)> {
  using type = Arg0;
};

template <typename Ret, typename Arg0, typename... Rest>
struct fn_first_param<Ret(*)(Arg0, Rest...)> {
  using type = Arg0;
};

template <typename T>
struct is_function_pointer
    : std::bool_constant<std::is_pointer_v<T> && std::is_function_v<std::remove_pointer_t<T>>> {};

template <typename T, typename = void>
struct has_pfnCreateAllocationCb : std::false_type {};

template <typename T>
struct has_pfnCreateAllocationCb<T, std::void_t<decltype(std::declval<T>().pfnCreateAllocationCb)>>
    : is_function_pointer<decltype(std::declval<T>().pfnCreateAllocationCb)> {};

template <typename T, typename = void>
struct has_pfnAllocateCb : std::false_type {};

template <typename T>
struct has_pfnAllocateCb<T, std::void_t<decltype(std::declval<T>().pfnAllocateCb)>>
    : is_function_pointer<decltype(std::declval<T>().pfnAllocateCb)> {};

template <typename T, typename = void>
struct has_pfnDestroyAllocationCb : std::false_type {};

template <typename T>
struct has_pfnDestroyAllocationCb<T, std::void_t<decltype(std::declval<T>().pfnDestroyAllocationCb)>>
    : is_function_pointer<decltype(std::declval<T>().pfnDestroyAllocationCb)> {};

template <typename T, typename = void>
struct has_pfnDeallocateCb : std::false_type {};

template <typename T>
struct has_pfnDeallocateCb<T, std::void_t<decltype(std::declval<T>().pfnDeallocateCb)>>
    : is_function_pointer<decltype(std::declval<T>().pfnDeallocateCb)> {};

template <typename T, typename = void>
struct has_pfnLockCb : std::false_type {};

template <typename T>
struct has_pfnLockCb<T, std::void_t<decltype(std::declval<T>().pfnLockCb)>>
    : is_function_pointer<decltype(std::declval<T>().pfnLockCb)> {};

template <typename T, typename = void>
struct has_pfnUnlockCb : std::false_type {};

template <typename T>
struct has_pfnUnlockCb<T, std::void_t<decltype(std::declval<T>().pfnUnlockCb)>>
    : is_function_pointer<decltype(std::declval<T>().pfnUnlockCb)> {};

template <typename T, typename = void>
struct has_member_hDevice : std::false_type {};

template <typename T>
struct has_member_hDevice<T, std::void_t<decltype(std::declval<T&>().hDevice)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hContext : std::false_type {};

template <typename T>
struct has_member_hContext<T, std::void_t<decltype(std::declval<T&>().hContext)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NumAllocations : std::false_type {};

template <typename T>
struct has_member_NumAllocations<T, std::void_t<decltype(std::declval<T&>().NumAllocations)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pAllocationInfo : std::false_type {};

template <typename T>
struct has_member_pAllocationInfo<T, std::void_t<decltype(std::declval<T&>().pAllocationInfo)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hResource : std::false_type {};

template <typename T>
struct has_member_hResource<T, std::void_t<decltype(std::declval<T&>().hResource)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hKMResource : std::false_type {};

template <typename T>
struct has_member_hKMResource<T, std::void_t<decltype(std::declval<T&>().hKMResource)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Size : std::false_type {};

template <typename T>
struct has_member_Size<T, std::void_t<decltype(std::declval<T&>().Size)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SizeInBytes : std::false_type {};

template <typename T>
struct has_member_SizeInBytes<T, std::void_t<decltype(std::declval<T&>().SizeInBytes)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Alignment : std::false_type {};

template <typename T>
struct has_member_Alignment<T, std::void_t<decltype(std::declval<T&>().Alignment)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_AlignmentInBytes : std::false_type {};

template <typename T>
struct has_member_AlignmentInBytes<T, std::void_t<decltype(std::declval<T&>().AlignmentInBytes)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SupportedReadSegmentSet : std::false_type {};

template <typename T>
struct has_member_SupportedReadSegmentSet<T, std::void_t<decltype(std::declval<T&>().SupportedReadSegmentSet)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SupportedWriteSegmentSet : std::false_type {};

template <typename T>
struct has_member_SupportedWriteSegmentSet<T, std::void_t<decltype(std::declval<T&>().SupportedWriteSegmentSet)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pPrivateDriverData : std::false_type {};

template <typename T>
struct has_member_pPrivateDriverData<T, std::void_t<decltype(std::declval<T&>().pPrivateDriverData)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_PrivateDriverDataSize : std::false_type {};

template <typename T>
struct has_member_PrivateDriverDataSize<T, std::void_t<decltype(std::declval<T&>().PrivateDriverDataSize)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_hAllocation : std::false_type {};

template <typename T>
struct has_member_hAllocation<T, std::void_t<decltype(std::declval<T&>().hAllocation)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_phAllocationList : std::false_type {};

template <typename T>
struct has_member_phAllocationList<T, std::void_t<decltype(std::declval<T&>().phAllocationList)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pAllocationList : std::false_type {};

template <typename T>
struct has_member_pAllocationList<T, std::void_t<decltype(std::declval<T&>().pAllocationList)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_HandleList : std::false_type {};

template <typename T>
struct has_member_HandleList<T, std::void_t<decltype(std::declval<T&>().HandleList)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_phAllocations : std::false_type {};

template <typename T>
struct has_member_phAllocations<T, std::void_t<decltype(std::declval<T&>().phAllocations)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_AllocationCount : std::false_type {};

template <typename T>
struct has_member_AllocationCount<T, std::void_t<decltype(std::declval<T&>().AllocationCount)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Offset : std::false_type {};

template <typename T>
struct has_member_Offset<T, std::void_t<decltype(std::declval<T&>().Offset)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_OffsetToLock : std::false_type {};

template <typename T>
struct has_member_OffsetToLock<T, std::void_t<decltype(std::declval<T&>().OffsetToLock)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_SizeToLock : std::false_type {};

template <typename T>
struct has_member_SizeToLock<T, std::void_t<decltype(std::declval<T&>().SizeToLock)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_pData : std::false_type {};

template <typename T>
struct has_member_pData<T, std::void_t<decltype(std::declval<T&>().pData)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Flags : std::false_type {};

template <typename T>
struct has_member_Flags<T, std::void_t<decltype(std::declval<T&>().Flags)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_flags : std::false_type {};

template <typename T>
struct has_member_flags<T, std::void_t<decltype(std::declval<T&>().flags)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Value : std::false_type {};

template <typename T>
struct has_member_Value<T, std::void_t<decltype(std::declval<T&>().Value)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Discard : std::false_type {};

template <typename T>
struct has_member_Discard<T, std::void_t<decltype(std::declval<T&>().Discard)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NoOverwrite : std::false_type {};

template <typename T>
struct has_member_NoOverwrite<T, std::void_t<decltype(std::declval<T&>().NoOverwrite)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_NoOverWrite : std::false_type {};

template <typename T>
struct has_member_NoOverWrite<T, std::void_t<decltype(std::declval<T&>().NoOverWrite)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_ReadOnly : std::false_type {};

template <typename T>
struct has_member_ReadOnly<T, std::void_t<decltype(std::declval<T&>().ReadOnly)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_DoNotWait : std::false_type {};

template <typename T>
struct has_member_DoNotWait<T, std::void_t<decltype(std::declval<T&>().DoNotWait)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_DonotWait : std::false_type {};

template <typename T>
struct has_member_DonotWait<T, std::void_t<decltype(std::declval<T&>().DonotWait)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_CpuVisible : std::false_type {};

template <typename T>
struct has_member_CpuVisible<T, std::void_t<decltype(std::declval<T&>().CpuVisible)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_Primary : std::false_type {};

template <typename T>
struct has_member_Primary<T, std::void_t<decltype(std::declval<T&>().Primary)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_CreateResource : std::false_type {};

template <typename T>
struct has_member_CreateResource<T, std::void_t<decltype(std::declval<T&>().CreateResource)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_CreateShared : std::false_type {};

template <typename T>
struct has_member_CreateShared<T, std::void_t<decltype(std::declval<T&>().CreateShared)>> : std::true_type {};

template <typename T, typename = void>
struct has_member_CreateSharedResource : std::false_type {};

template <typename T>
struct has_member_CreateSharedResource<T, std::void_t<decltype(std::declval<T&>().CreateSharedResource)>> : std::true_type {};

// D3DLOCK_* flags (numeric values from d3d9.h). Only the bits we care about are
// defined here to keep the allocation helper self-contained.
constexpr uint32_t kD3DLOCK_READONLY = 0x00000010u;
constexpr uint32_t kD3DLOCK_DISCARD = 0x00002000u;
constexpr uint32_t kD3DLOCK_NOOVERWRITE = 0x00001000u;
constexpr uint32_t kD3DLOCK_DONOTWAIT = 0x00004000u;

template <typename FlagsT>
void set_lock_flags(FlagsT& flags, uint32_t lock_flags) {
  const bool discard = (lock_flags & kD3DLOCK_DISCARD) != 0;
  const bool no_overwrite = (lock_flags & kD3DLOCK_NOOVERWRITE) != 0;
  const bool read_only = (lock_flags & kD3DLOCK_READONLY) != 0;
  const bool do_not_wait = (lock_flags & kD3DLOCK_DONOTWAIT) != 0;

  constexpr bool has_bitfields = has_member_Discard<FlagsT>::value ||
                                 has_member_NoOverwrite<FlagsT>::value ||
                                 has_member_NoOverWrite<FlagsT>::value ||
                                 has_member_ReadOnly<FlagsT>::value ||
                                 has_member_DoNotWait<FlagsT>::value ||
                                 has_member_DonotWait<FlagsT>::value;
  if constexpr (has_bitfields) {
    if constexpr (has_member_Discard<FlagsT>::value) {
      flags.Discard = discard ? 1u : 0u;
    }
    if constexpr (has_member_NoOverwrite<FlagsT>::value) {
      flags.NoOverwrite = no_overwrite ? 1u : 0u;
    }
    if constexpr (has_member_NoOverWrite<FlagsT>::value) {
      flags.NoOverWrite = no_overwrite ? 1u : 0u;
    }
    if constexpr (has_member_ReadOnly<FlagsT>::value) {
      flags.ReadOnly = read_only ? 1u : 0u;
    }
    if constexpr (has_member_DoNotWait<FlagsT>::value) {
      flags.DoNotWait = do_not_wait ? 1u : 0u;
    }
    if constexpr (has_member_DonotWait<FlagsT>::value) {
      flags.DonotWait = do_not_wait ? 1u : 0u;
    }
  } else if constexpr (has_member_Value<FlagsT>::value &&
                       std::is_integral_v<std::remove_reference_t<decltype(std::declval<FlagsT&>().Value)>>) {
    // Fallback: if the flag struct only exposes a `Value` member, assume it uses
    // the D3DLOCK bit layout (Win7-compatible).
    flags.Value = static_cast<decltype(flags.Value)>(lock_flags);
  } else if constexpr (std::is_integral_v<std::remove_reference_t<FlagsT>>) {
    flags = static_cast<std::remove_reference_t<FlagsT>>(lock_flags);
  } else {
    // Unknown representation; leave flags as zero.
    (void)lock_flags;
  }
}

template <typename FlagsT>
void set_allocate_flags(FlagsT& flags, bool is_shared) {
  constexpr bool has_bitfields =
      has_member_CreateResource<FlagsT>::value ||
      has_member_CreateShared<FlagsT>::value ||
      has_member_CreateSharedResource<FlagsT>::value;
  if constexpr (has_bitfields) {
    if constexpr (has_member_CreateResource<FlagsT>::value) {
      flags.CreateResource = 1u;
    }
    if constexpr (has_member_CreateShared<FlagsT>::value) {
      flags.CreateShared = is_shared ? 1u : 0u;
    }
    if constexpr (has_member_CreateSharedResource<FlagsT>::value) {
      flags.CreateSharedResource = is_shared ? 1u : 0u;
    }
  } else if constexpr (has_member_Value<FlagsT>::value &&
                       std::is_integral_v<std::remove_reference_t<decltype(std::declval<FlagsT&>().Value)>>) {
    // Best-effort fallback: assume CreateResource is bit 0 and CreateShared is
    // bit 1 (matches Win7-era DXGK/D3D flag layouts).
    uint32_t value = 0;
    value |= 0x1u;
    if (is_shared) {
      value |= 0x2u;
    }
    flags.Value = static_cast<decltype(flags.Value)>(value);
  } else if constexpr (std::is_integral_v<std::remove_reference_t<FlagsT>>) {
    uint32_t value = 0x1u;
    if (is_shared) {
      value |= 0x2u;
    }
    flags = static_cast<std::remove_reference_t<FlagsT>>(value);
  } else {
    (void)flags;
    (void)is_shared;
  }
}

template <typename FlagsT>
void set_allocation_info_flags(FlagsT& flags) {
  // AeroGPU's Win7 MVP uses a single CPU-visible system-memory segment, so
  // marking allocations as CPU-visible keeps Lock/Unlock paths simple.
  if constexpr (has_member_CpuVisible<FlagsT>::value) {
    flags.CpuVisible = 1u;
  }

  // Do not force Primary here; callers should rely on the runtime's standard
  // allocation path for real primaries. Backbuffer allocations created by the
  // D3D9 UMD are still treated as generic resources.
  if constexpr (has_member_Primary<FlagsT>::value) {
    flags.Primary = 0u;
  }
}

} // namespace

template <typename CallbackFn>
HRESULT invoke_create_allocation_cb(CallbackFn cb,
                                    WddmHandle hDevice,
                                    WddmHandle hContext,
                                    uint64_t size_bytes,
                                    const aerogpu_wddm_alloc_priv* priv,
                                    uint32_t priv_size,
                                    WddmAllocationHandle* hAllocationOut) {
  using Fn = CallbackFn;
  using ArgPtr = typename fn_first_param<Fn>::type;
  using Args = std::remove_pointer_t<ArgPtr>;
  Args data{};
  const bool is_shared = (priv && (priv->flags & AEROGPU_WDDM_ALLOC_PRIV_FLAG_IS_SHARED) != 0);

  if constexpr (has_member_hDevice<Args>::value) {
    data.hDevice = hDevice;
  }
  if constexpr (has_member_hContext<Args>::value) {
    data.hContext = hContext;
  }
  if constexpr (has_member_hResource<Args>::value) {
    data.hResource = 0;
  }
  if constexpr (has_member_hKMResource<Args>::value) {
    data.hKMResource = 0;
  }
  if constexpr (has_member_Flags<Args>::value) {
    set_allocate_flags(data.Flags, is_shared);
  } else if constexpr (has_member_flags<Args>::value) {
    set_allocate_flags(data.flags, is_shared);
  }

  if constexpr (!has_member_pAllocationInfo<Args>::value) {
    return E_NOTIMPL;
  } else {
    using InfoPtr = decltype(std::declval<Args&>().pAllocationInfo);
    using Info = std::remove_pointer_t<InfoPtr>;
    Info info{};
    std::memset(&info, 0, sizeof(info));

    if constexpr (has_member_Size<Info>::value) {
      info.Size = static_cast<decltype(info.Size)>(size_bytes);
    } else if constexpr (has_member_SizeInBytes<Info>::value) {
      info.SizeInBytes = static_cast<decltype(info.SizeInBytes)>(size_bytes);
    } else {
      return E_NOTIMPL;
    }

    if constexpr (has_member_Alignment<Info>::value) {
      info.Alignment = 0;
    } else if constexpr (has_member_AlignmentInBytes<Info>::value) {
      info.AlignmentInBytes = 0;
    }

    if constexpr (has_member_SupportedReadSegmentSet<Info>::value) {
      info.SupportedReadSegmentSet = 1;
    }
    if constexpr (has_member_SupportedWriteSegmentSet<Info>::value) {
      info.SupportedWriteSegmentSet = 1;
    }

    if constexpr (has_member_Flags<Info>::value) {
      set_allocation_info_flags(info.Flags);
    } else if constexpr (has_member_flags<Info>::value) {
      set_allocation_info_flags(info.flags);
    }

    if constexpr (has_member_pPrivateDriverData<Info>::value) {
      info.pPrivateDriverData = const_cast<void*>(reinterpret_cast<const void*>(priv));
    } else {
      return E_NOTIMPL;
    }
    if constexpr (has_member_PrivateDriverDataSize<Info>::value) {
      info.PrivateDriverDataSize = priv_size;
    } else {
      return E_NOTIMPL;
    }

    if constexpr (has_member_NumAllocations<Args>::value) {
      data.NumAllocations = 1;
    } else if constexpr (has_member_AllocationCount<Args>::value) {
      data.AllocationCount = 1;
    } else {
      return E_NOTIMPL;
    }
    data.pAllocationInfo = &info;

    const HRESULT hr = cb(&data);
    if (FAILED(hr)) {
      return hr;
    }

    if constexpr (has_member_hAllocation<Info>::value) {
      *hAllocationOut = info.hAllocation;
    } else {
      return E_NOTIMPL;
    }

    return (*hAllocationOut != 0) ? S_OK : E_FAIL;
  }
}

HRESULT wddm_create_allocation(const WddmDeviceCallbacks& callbacks,
                               WddmHandle hDevice,
                               uint64_t size_bytes,
                               const aerogpu_wddm_alloc_priv* priv,
                               uint32_t priv_size,
                               WddmAllocationHandle* hAllocationOut,
                               WddmHandle hContext) noexcept {
  // Wrap runtime callback calls so unexpected C++ exceptions cannot escape into
  // callers (including `noexcept` destructors during teardown).
  try {
    if (!hAllocationOut) {
      return E_INVALIDARG;
    }
    *hAllocationOut = 0;
    if (!hDevice || size_bytes == 0 || !priv || priv_size == 0) {
      return E_INVALIDARG;
    }

    // The Win7 D3D9 runtime uses pfnAllocateCb/pfnDeallocateCb for WDDM allocation
    // management. Some newer header sets also expose CreateAllocation/DestroyAllocation
    // spellings. Prefer the Win7 names but keep a fallback for compatibility.

    if constexpr (has_pfnAllocateCb<WddmDeviceCallbacks>::value) {
      if (callbacks.pfnAllocateCb) {
        return invoke_create_allocation_cb(callbacks.pfnAllocateCb,
                                           hDevice,
                                           hContext,
                                           size_bytes,
                                           priv,
                                           priv_size,
                                           hAllocationOut);
      }
    }

    if constexpr (has_pfnCreateAllocationCb<WddmDeviceCallbacks>::value) {
      if (callbacks.pfnCreateAllocationCb) {
        return invoke_create_allocation_cb(callbacks.pfnCreateAllocationCb,
                                           hDevice,
                                           hContext,
                                           size_bytes,
                                           priv,
                                           priv_size,
                                           hAllocationOut);
      }
    }

    return E_FAIL;
  } catch (...) {
    return E_FAIL;
  }
}

template <typename CallbackFn>
HRESULT invoke_destroy_allocation_cb(CallbackFn cb,
                                     WddmHandle hDevice,
                                     WddmHandle hContext,
                                     WddmAllocationHandle hAllocation) {
  using Fn = CallbackFn;
  using ArgPtr = typename fn_first_param<Fn>::type;
  using Args = std::remove_pointer_t<ArgPtr>;
  Args data{};
  std::memset(&data, 0, sizeof(data));

  if constexpr (has_member_hDevice<Args>::value) {
    data.hDevice = hDevice;
  }
  if constexpr (has_member_hContext<Args>::value) {
    data.hContext = hContext;
  }
  if constexpr (has_member_hResource<Args>::value) {
    data.hResource = 0;
  }
  if constexpr (has_member_hKMResource<Args>::value) {
    data.hKMResource = 0;
  }

  WddmAllocationHandle allocs[1] = {hAllocation};

  if constexpr (has_member_NumAllocations<Args>::value) {
    data.NumAllocations = 1;
  } else if constexpr (has_member_AllocationCount<Args>::value) {
    data.AllocationCount = 1;
  } else {
    return E_NOTIMPL;
  }

  if constexpr (has_member_phAllocationList<Args>::value) {
    data.phAllocationList = allocs;
  } else if constexpr (has_member_pAllocationList<Args>::value) {
    data.pAllocationList = allocs;
  } else if constexpr (has_member_HandleList<Args>::value) {
    data.HandleList = allocs;
  } else if constexpr (has_member_phAllocations<Args>::value) {
    data.phAllocations = allocs;
  } else {
    return E_NOTIMPL;
  }

  return cb(&data);
}

HRESULT wddm_destroy_allocation(const WddmDeviceCallbacks& callbacks,
                                   WddmHandle hDevice,
                                   WddmAllocationHandle hAllocation,
                                   WddmHandle hContext) noexcept {
  try {
    if (!hDevice || !hAllocation) {
      return E_INVALIDARG;
    }

    if constexpr (has_pfnDeallocateCb<WddmDeviceCallbacks>::value) {
      if (callbacks.pfnDeallocateCb) {
        return invoke_destroy_allocation_cb(callbacks.pfnDeallocateCb, hDevice, hContext, hAllocation);
      }
    }

    if constexpr (has_pfnDestroyAllocationCb<WddmDeviceCallbacks>::value) {
      if (callbacks.pfnDestroyAllocationCb) {
        return invoke_destroy_allocation_cb(callbacks.pfnDestroyAllocationCb, hDevice, hContext, hAllocation);
      }
    }

    return E_FAIL;
  } catch (...) {
    return E_FAIL;
  }
}

HRESULT wddm_lock_allocation(const WddmDeviceCallbacks& callbacks,
                             WddmHandle hDevice,
                             WddmAllocationHandle hAllocation,
                             uint64_t offset_bytes,
                             uint64_t size_bytes,
                             uint32_t lock_flags,
                             void** out_ptr,
                             WddmHandle hContext) noexcept {
  try {
    if (!out_ptr) {
      return E_INVALIDARG;
    }
    *out_ptr = nullptr;
    if (!hDevice || !hAllocation) {
      return E_INVALIDARG;
    }

    if constexpr (!has_pfnLockCb<WddmDeviceCallbacks>::value) {
      return E_NOTIMPL;
    } else {
      if (!callbacks.pfnLockCb) {
        return E_FAIL;
      }

      using Fn = decltype(callbacks.pfnLockCb);
      using ArgPtr = typename fn_first_param<Fn>::type;
      using Args = std::remove_pointer_t<ArgPtr>;
      Args data{};
      std::memset(&data, 0, sizeof(data));

      if constexpr (has_member_hDevice<Args>::value) {
        data.hDevice = hDevice;
      }
      if constexpr (has_member_hContext<Args>::value) {
        data.hContext = hContext;
      }
      if constexpr (has_member_hAllocation<Args>::value) {
        data.hAllocation = hAllocation;
      }

      if constexpr (has_member_Offset<Args>::value) {
        data.Offset = offset_bytes;
      } else if constexpr (has_member_OffsetToLock<Args>::value) {
        data.OffsetToLock = offset_bytes;
      }

      if constexpr (has_member_Size<Args>::value) {
        data.Size = static_cast<decltype(data.Size)>(size_bytes);
      } else if constexpr (has_member_SizeToLock<Args>::value) {
        data.SizeToLock = static_cast<decltype(data.SizeToLock)>(size_bytes);
      }

      if constexpr (has_member_Flags<Args>::value) {
        set_lock_flags(data.Flags, lock_flags);
      } else if constexpr (has_member_flags<Args>::value) {
        set_lock_flags(data.flags, lock_flags);
      }

      const HRESULT hr = callbacks.pfnLockCb(&data);
      if (FAILED(hr)) {
        return hr;
      }

      if constexpr (has_member_pData<Args>::value) {
        *out_ptr = data.pData;
        return (*out_ptr != nullptr) ? S_OK : E_FAIL;
      } else {
        return E_NOTIMPL;
      }
    }
  } catch (...) {
    return E_FAIL;
  }
}

HRESULT wddm_unlock_allocation(const WddmDeviceCallbacks& callbacks,
                               WddmHandle hDevice,
                               WddmAllocationHandle hAllocation,
                               WddmHandle hContext) noexcept {
  try {
    if (!hDevice || !hAllocation) {
      return E_INVALIDARG;
    }

    if constexpr (!has_pfnUnlockCb<WddmDeviceCallbacks>::value) {
      return E_NOTIMPL;
    } else {
      if (!callbacks.pfnUnlockCb) {
        return E_FAIL;
      }

      using Fn = decltype(callbacks.pfnUnlockCb);
      using ArgPtr = typename fn_first_param<Fn>::type;
      using Args = std::remove_pointer_t<ArgPtr>;
      Args data{};
      std::memset(&data, 0, sizeof(data));

      if constexpr (has_member_hDevice<Args>::value) {
        data.hDevice = hDevice;
      }
      if constexpr (has_member_hContext<Args>::value) {
        data.hContext = hContext;
      }
      if constexpr (has_member_hAllocation<Args>::value) {
        data.hAllocation = hAllocation;
      }

      return callbacks.pfnUnlockCb(&data);
    }
  } catch (...) {
    return E_FAIL;
  }
}

#else

HRESULT wddm_create_allocation(const WddmDeviceCallbacks&,
                               WddmHandle,
                               uint64_t,
                               const aerogpu_wddm_alloc_priv*,
                               uint32_t,
                               WddmAllocationHandle*,
                               WddmHandle) noexcept {
  return E_NOTIMPL;
}

HRESULT wddm_destroy_allocation(const WddmDeviceCallbacks&, WddmHandle, WddmAllocationHandle, WddmHandle) noexcept {
  return E_NOTIMPL;
}

HRESULT wddm_lock_allocation(const WddmDeviceCallbacks&,
                             WddmHandle,
                             WddmAllocationHandle,
                             uint64_t,
                             uint64_t,
                             uint32_t,
                             void**,
                             WddmHandle) noexcept {
  return E_NOTIMPL;
}

HRESULT wddm_unlock_allocation(const WddmDeviceCallbacks&, WddmHandle, WddmAllocationHandle, WddmHandle) noexcept {
  return E_NOTIMPL;
}

#endif

} // namespace aerogpu
