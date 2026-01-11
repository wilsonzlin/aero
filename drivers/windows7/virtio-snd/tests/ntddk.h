/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Minimal ntddk.h shim for host-buildable virtio-snd protocol unit tests.
 *
 * The Windows 7 virtio-snd driver sources are written against WDK headers.
 * For host CI (Linux) we provide just enough of the WDK surface area to compile
 * and exercise the protocol engines (control/tx/rx) in user mode.
 *
 * This file is ONLY intended for tests under drivers/windows7/virtio-snd/tests/.
 */

#pragma once

#if !defined(_POSIX_C_SOURCE)
/* For clock_gettime/nanosleep declarations when compiling as strict C99. */
#define _POSIX_C_SOURCE 200809L
#endif

#include <assert.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- Basic Windows types ---- */

typedef void VOID;
typedef void *PVOID;
typedef uint8_t BOOLEAN;
typedef uint8_t UCHAR;
typedef uint16_t USHORT;
typedef uint32_t ULONG;
typedef int32_t LONG;
typedef int64_t LONGLONG;
typedef uint64_t ULONGLONG;
typedef unsigned int UINT;
typedef uint8_t UINT8;
typedef uint16_t UINT16;
typedef uint32_t UINT32;
typedef uint64_t UINT64;
typedef size_t SIZE_T;
typedef const char *PCSTR;
typedef char *PCHAR;
typedef UCHAR *PUCHAR;
typedef const UCHAR *PCUCHAR;
typedef USHORT *PUSHORT;
typedef ULONG *PULONG;
typedef LONG *PLONG;

#ifndef TRUE
#define TRUE ((BOOLEAN)1u)
#endif
#ifndef FALSE
#define FALSE ((BOOLEAN)0u)
#endif

typedef int32_t NTSTATUS;

#define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)

/* A small subset of NTSTATUS values used by the protocol engines. */
#define STATUS_SUCCESS ((NTSTATUS)0x00000000L)
#define STATUS_PENDING ((NTSTATUS)0x00000103L)
#define STATUS_TIMEOUT ((NTSTATUS)0x00000102L)

#define STATUS_UNSUCCESSFUL ((NTSTATUS)0xC0000001L)
#define STATUS_INVALID_PARAMETER ((NTSTATUS)0xC000000DL)
#define STATUS_INVALID_DEVICE_STATE ((NTSTATUS)0xC0000184L)
#define STATUS_INSUFFICIENT_RESOURCES ((NTSTATUS)0xC000009AL)
#define STATUS_INVALID_BUFFER_SIZE ((NTSTATUS)0xC0000206L)
#define STATUS_INTEGER_OVERFLOW ((NTSTATUS)0xC0000095L)
#define STATUS_BUFFER_TOO_SMALL ((NTSTATUS)0xC0000023L)
#define STATUS_CANCELLED ((NTSTATUS)0xC0000120L)
#define STATUS_IO_TIMEOUT ((NTSTATUS)0xC00000B5L)
#define STATUS_NOT_SUPPORTED ((NTSTATUS)0xC00000BBL)
#define STATUS_DEVICE_PROTOCOL_ERROR ((NTSTATUS)0xC000018EL)

/* ---- SAL / WDK annotation stubs ---- */

#define _In_
#define _In_opt_
#define _Inout_
#define _Inout_opt_
#define _Out_
#define _Out_opt_
#define _In_reads_(n)
#define _In_reads_bytes_(n)
#define _Out_writes_(n)
#define _Out_writes_bytes_(n)
#define _Out_writes_bytes_to_(cap, len)
#define _Use_decl_annotations_
#define _Must_inspect_result_
#define _IRQL_requires_(level)
#define _IRQL_requires_max_(level)

#ifndef __forceinline
#define __forceinline inline
#endif

#ifndef UNREFERENCED_PARAMETER
#define UNREFERENCED_PARAMETER(P) ((void)(P))
#endif

/* ---- Compile-time helpers used by headers ---- */

#ifndef RTL_NUMBER_OF
#define RTL_NUMBER_OF(a) (sizeof(a) / sizeof((a)[0]))
#endif

#ifndef FIELD_OFFSET
#define FIELD_OFFSET(type, field) offsetof(type, field)
#endif

#define _C_ASSERT_CONCAT_INNER(a, b) a##b
#define _C_ASSERT_CONCAT(a, b) _C_ASSERT_CONCAT_INNER(a, b)
#define C_ASSERT(expr) typedef char _C_ASSERT_CONCAT(_c_assert_, __LINE__)[(expr) ? 1 : -1]

#ifndef ALIGN_UP_BY
#define ALIGN_UP_BY(value, alignment) (((value) + ((alignment) - 1u)) & ~((alignment) - 1u))
#endif

#ifndef UNALIGNED
#define UNALIGNED
#endif

/* ---- Memory helpers ---- */

#define RtlZeroMemory(dst, len) memset((dst), 0, (len))
#define RtlCopyMemory(dst, src, len) memcpy((dst), (src), (len))

/* ---- Pool allocation shims ---- */

#define NonPagedPool 0

static __forceinline void *ExAllocatePoolWithTag(int pool_type, SIZE_T size, ULONG tag)
{
    UNREFERENCED_PARAMETER(pool_type);
    UNREFERENCED_PARAMETER(tag);
    return malloc(size);
}

static __forceinline void ExFreePoolWithTag(void *ptr, ULONG tag)
{
    UNREFERENCED_PARAMETER(tag);
    free(ptr);
}

/* ---- Interlocked operations (single-process host tests) ---- */

static __forceinline LONG InterlockedIncrement(volatile LONG *addend) { return __sync_add_and_fetch(addend, 1); }
static __forceinline LONG InterlockedDecrement(volatile LONG *addend) { return __sync_sub_and_fetch(addend, 1); }
static __forceinline LONG InterlockedExchange(volatile LONG *target, LONG value) { return __sync_lock_test_and_set(target, value); }
static __forceinline LONG InterlockedCompareExchange(volatile LONG *dest, LONG exchange, LONG comparand)
{
    return __sync_val_compare_and_swap(dest, comparand, exchange);
}

/* ---- IRQL/spinlock shims ---- */

typedef uint8_t KIRQL;
typedef ULONG KSPIN_LOCK;

#define PASSIVE_LEVEL ((KIRQL)0u)
#define DISPATCH_LEVEL ((KIRQL)2u)

static __forceinline KIRQL KeGetCurrentIrql(void) { return PASSIVE_LEVEL; }

static __forceinline void KeInitializeSpinLock(KSPIN_LOCK *lock) { UNREFERENCED_PARAMETER(lock); }

static __forceinline void KeAcquireSpinLock(KSPIN_LOCK *lock, KIRQL *old_irql)
{
    UNREFERENCED_PARAMETER(lock);
    if (old_irql != NULL) {
        *old_irql = PASSIVE_LEVEL;
    }
}

static __forceinline void KeReleaseSpinLock(KSPIN_LOCK *lock, KIRQL old_irql)
{
    UNREFERENCED_PARAMETER(lock);
    UNREFERENCED_PARAMETER(old_irql);
}

static __forceinline void KeMemoryBarrier(void) { __sync_synchronize(); }

/* ---- LIST_ENTRY (doubly-linked list) ---- */

typedef struct _LIST_ENTRY {
    struct _LIST_ENTRY *Flink;
    struct _LIST_ENTRY *Blink;
} LIST_ENTRY, *PLIST_ENTRY;

static __forceinline void InitializeListHead(PLIST_ENTRY list)
{
    list->Flink = list;
    list->Blink = list;
}

static __forceinline BOOLEAN IsListEmpty(const LIST_ENTRY *list) { return (list->Flink == list) ? TRUE : FALSE; }

static __forceinline void InsertTailList(PLIST_ENTRY head, PLIST_ENTRY entry)
{
    PLIST_ENTRY blink = head->Blink;
    entry->Flink = head;
    entry->Blink = blink;
    blink->Flink = entry;
    head->Blink = entry;
}

static __forceinline PLIST_ENTRY RemoveHeadList(PLIST_ENTRY head)
{
    PLIST_ENTRY first = head->Flink;
    PLIST_ENTRY next = first->Flink;
    head->Flink = next;
    next->Blink = head;
    first->Flink = NULL;
    first->Blink = NULL;
    return first;
}

static __forceinline void RemoveEntryList(PLIST_ENTRY entry)
{
    PLIST_ENTRY blink = entry->Blink;
    PLIST_ENTRY flink = entry->Flink;
    blink->Flink = flink;
    flink->Blink = blink;
    entry->Flink = NULL;
    entry->Blink = NULL;
}

#ifndef CONTAINING_RECORD
#define CONTAINING_RECORD(address, type, field) ((type *)((char *)(address) - offsetof(type, field)))
#endif

/* ---- Synchronization primitives used by virtiosnd_control ---- */

typedef struct _FAST_MUTEX {
    int unused;
} FAST_MUTEX;

static __forceinline void ExInitializeFastMutex(FAST_MUTEX *m) { UNREFERENCED_PARAMETER(m); }
static __forceinline void ExAcquireFastMutex(FAST_MUTEX *m) { UNREFERENCED_PARAMETER(m); }
static __forceinline void ExReleaseFastMutex(FAST_MUTEX *m) { UNREFERENCED_PARAMETER(m); }

typedef enum _EVENT_TYPE {
    NotificationEvent = 0,
    SynchronizationEvent = 1,
} EVENT_TYPE;

typedef struct _KEVENT {
    volatile LONG signaled;
} KEVENT;

#define IO_NO_INCREMENT 0

static __forceinline void KeInitializeEvent(KEVENT *event, EVENT_TYPE type, BOOLEAN state)
{
    UNREFERENCED_PARAMETER(type);
    event->signaled = state ? 1 : 0;
}

static __forceinline LONG KeSetEvent(KEVENT *event, int increment, BOOLEAN wait)
{
    LONG old = event->signaled;
    UNREFERENCED_PARAMETER(increment);
    UNREFERENCED_PARAMETER(wait);
    event->signaled = 1;
    return old;
}

static __forceinline void KeClearEvent(KEVENT *event) { event->signaled = 0; }
static __forceinline LONG KeReadStateEvent(KEVENT *event) { return event->signaled; }

typedef struct _LARGE_INTEGER {
    LONGLONG QuadPart;
} LARGE_INTEGER;

/*
 * KeWaitForSingleObject: minimal event wait.
 *
 * - Supports KEVENT objects only.
 * - Supports relative timeouts via negative QuadPart (100ns units).
 * - Implemented as a polling loop to avoid platform threading dependencies.
 */
static __forceinline NTSTATUS KeWaitForSingleObject(
    void *object,
    int reason,
    int wait_mode,
    BOOLEAN alertable,
    const LARGE_INTEGER *timeout_opt)
{
    KEVENT *event = (KEVENT *)object;
    uint64_t timeout_ns = 0;
    uint64_t start_ns = 0;

    UNREFERENCED_PARAMETER(reason);
    UNREFERENCED_PARAMETER(wait_mode);
    UNREFERENCED_PARAMETER(alertable);

    if (event == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (event->signaled != 0) {
        return STATUS_SUCCESS;
    }

    if (timeout_opt == NULL) {
        /* Best-effort "infinite" wait; cap to avoid hanging unit tests. */
        timeout_ns = 5ull * 1000ull * 1000ull * 1000ull;
    } else if (timeout_opt->QuadPart < 0) {
        /* Relative timeout in 100ns units. */
        timeout_ns = (uint64_t)(-timeout_opt->QuadPart) * 100ull;
    } else {
        /* Absolute timeouts are not needed by current tests. */
        timeout_ns = (uint64_t)timeout_opt->QuadPart * 100ull;
    }

    {
        struct timespec ts;
        clock_gettime(CLOCK_MONOTONIC, &ts);
        start_ns = (uint64_t)ts.tv_sec * 1000ull * 1000ull * 1000ull + (uint64_t)ts.tv_nsec;
    }

    for (;;) {
        struct timespec ts;
        uint64_t now_ns;

        if (event->signaled != 0) {
            return STATUS_SUCCESS;
        }

        clock_gettime(CLOCK_MONOTONIC, &ts);
        now_ns = (uint64_t)ts.tv_sec * 1000ull * 1000ull * 1000ull + (uint64_t)ts.tv_nsec;
        if (now_ns - start_ns >= timeout_ns) {
            return STATUS_TIMEOUT;
        }

        {
            struct timespec req;
            req.tv_sec = 0;
            req.tv_nsec = 50 * 1000; /* 50us */
            (void)nanosleep(&req, NULL);
        }
    }
}

/*
 * KeQueryInterruptTime: return monotonic time in 100ns units.
 * Only used for control request timeout calculation in host tests.
 */
static __forceinline ULONGLONG KeQueryInterruptTime(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (ULONGLONG)ts.tv_sec * 10000000ull + (ULONGLONG)(ts.tv_nsec / 100u);
}

/* ---- Assertions ---- */

#define NT_ASSERT(expr)                                                                                                  \
    do {                                                                                                                 \
        if (!(expr)) {                                                                                                   \
            fprintf(stderr, "NT_ASSERT failed at %s:%d: %s\n", __FILE__, __LINE__, #expr);                               \
            abort();                                                                                                      \
        }                                                                                                                \
    } while (0)

/* ---- Misc WDK types referenced by headers but unused by host tests ---- */

typedef void *PDEVICE_OBJECT;
typedef void *PDMA_ADAPTER;

/* ---- DbgPrintEx stubs (compiled out in free builds, but define anyway) ---- */

#define DPFLTR_IHVDRIVER_ID 0
#define DPFLTR_INFO_LEVEL 0
#define DPFLTR_ERROR_LEVEL 0

static __forceinline int DbgPrintEx(int comp_id, int level, const char *fmt, ...)
{
    UNREFERENCED_PARAMETER(comp_id);
    UNREFERENCED_PARAMETER(level);
    UNREFERENCED_PARAMETER(fmt);
    return 0;
}

/* ---- Wait enums (ignored by shims) ---- */

#define Executive 0
#define KernelMode 0

typedef LARGE_INTEGER PHYSICAL_ADDRESS;

#ifdef __cplusplus
} /* extern "C" */
#endif
