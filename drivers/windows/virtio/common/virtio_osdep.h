#ifndef VIRTIO_OSDEP_H_
#define VIRTIO_OSDEP_H_

/*
 * Tiny portability layer for the Windows virtio common modules.
 *
 * This header is intentionally minimal and avoids WDF dependencies. When built
 * as a Windows kernel-mode driver, the including translation unit is expected
 * to have the usual WDK headers available. For user-mode unit tests, we
 * provide lightweight stand-ins for common WDK types and NTSTATUS values.
 */

#include <stddef.h>

/* -------------------------------------------------------------------------- */
/* Basic WDK-like types for user-mode tests                                   */
/* -------------------------------------------------------------------------- */

/*
 * WDK headers define _NTDEF_. If it's not present, assume we are building a
 * user-mode test harness (or a non-Windows build) and provide compatible
 * typedefs.
 */
#ifndef _NTDEF_
#include <stdint.h>

typedef uint8_t UINT8;
typedef uint16_t UINT16;
typedef uint32_t UINT32;
typedef uint64_t UINT64;

typedef int32_t NTSTATUS;
typedef int BOOLEAN;
typedef void VOID;

#ifndef TRUE
#define TRUE 1
#endif
#ifndef FALSE
#define FALSE 0
#endif

#ifndef NT_SUCCESS
#define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)
#endif

#ifndef STATUS_SUCCESS
#define STATUS_SUCCESS ((NTSTATUS)0x00000000L)
#endif
#ifndef STATUS_INVALID_PARAMETER
#define STATUS_INVALID_PARAMETER ((NTSTATUS)0xC000000DL)
#endif
#ifndef STATUS_INSUFFICIENT_RESOURCES
#define STATUS_INSUFFICIENT_RESOURCES ((NTSTATUS)0xC000009AL)
#endif
#ifndef STATUS_NOT_FOUND
#define STATUS_NOT_FOUND ((NTSTATUS)0xC0000225L)
#endif

#endif /* !_NTDEF_ */

/* -------------------------------------------------------------------------- */
/* Static assertions                                                          */
/* -------------------------------------------------------------------------- */

#if defined(__STDC_VERSION__) && (__STDC_VERSION__ >= 201112L)
#define VIRTIO_STATIC_ASSERT(cond, msg) _Static_assert(cond, #msg)
#else
/*
 * Pre-C11 fallback. msg must be a valid identifier.
 * Note: "typedef in block scope" is legal and the common pattern used in WDK.
 */
#define VIRTIO_STATIC_ASSERT(cond, msg) \
	typedef char virtio_static_assert_##msg[(cond) ? 1 : -1]
#endif

/* -------------------------------------------------------------------------- */
/* Alignment helper                                                           */
/* -------------------------------------------------------------------------- */

#define VIRTIO_ALIGN_UP(val, align) (((val) + ((align) - 1)) & ~((align) - 1))

/* -------------------------------------------------------------------------- */
/* Memory barriers                                                            */
/* -------------------------------------------------------------------------- */

/*
 * Split virtqueues are accessed concurrently by driver and device. Ordering is
 * handled explicitly via barriers around ring index updates.
 *
 * In kernel-mode builds, prefer KeMemoryBarrier() (available since Win7).
 * In user-mode tests, use C11 atomics.
 */
#if defined(_WIN32) && defined(_KERNEL_MODE)
/* WDK build */
#include <ntddk.h>

#define VIRTIO_MB() KeMemoryBarrier()
#define VIRTIO_RMB() KeMemoryBarrier()
#define VIRTIO_WMB() KeMemoryBarrier()
#else
/* User-mode / non-Windows test build */
#include <stdatomic.h>

#define VIRTIO_MB() atomic_thread_fence(memory_order_seq_cst)
#define VIRTIO_RMB() atomic_thread_fence(memory_order_seq_cst)
#define VIRTIO_WMB() atomic_thread_fence(memory_order_seq_cst)
#endif

/* -------------------------------------------------------------------------- */
/* Memory helpers                                                             */
/* -------------------------------------------------------------------------- */

/*
 * Prefer WDK primitives in kernel-mode builds to avoid CRT dependencies.
 */
#if defined(_WIN32) && defined(_KERNEL_MODE)
static __inline VOID VirtioZeroMemory(void *dst, size_t len)
{
	RtlZeroMemory(dst, len);
}
#else
#include <string.h>
static __inline VOID VirtioZeroMemory(void *dst, size_t len)
{
	memset(dst, 0, len);
}
#endif

/* -------------------------------------------------------------------------- */
/* Volatile (READ_ONCE/WRITE_ONCE-style) accessors                             */
/* -------------------------------------------------------------------------- */

static __inline UINT16 VirtioReadU16(const volatile UINT16 *p) { return *p; }
static __inline VOID VirtioWriteU16(volatile UINT16 *p, UINT16 v) { *p = v; }

static __inline UINT32 VirtioReadU32(const volatile UINT32 *p) { return *p; }
static __inline VOID VirtioWriteU32(volatile UINT32 *p, UINT32 v) { *p = v; }

static __inline UINT64 VirtioReadU64(const volatile UINT64 *p) { return *p; }
static __inline VOID VirtioWriteU64(volatile UINT64 *p, UINT64 v) { *p = v; }

#endif /* VIRTIO_OSDEP_H_ */
