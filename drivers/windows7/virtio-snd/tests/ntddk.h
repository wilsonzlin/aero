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

#if defined(_MSC_VER)
#include <intrin.h>
#endif

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
#if defined(_WIN32)
/* Match WDK: LONG is a 32-bit signed long on Windows. */
typedef long LONG;
#else
typedef int32_t LONG;
#endif
typedef int64_t LONGLONG;
typedef uint64_t ULONGLONG;
typedef uintptr_t ULONG_PTR;
typedef uintptr_t UINT_PTR;
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

/* ---- Work item shims ---- */

typedef void (*PWORKER_THREAD_ROUTINE)(PVOID Parameter);

typedef enum _WORK_QUEUE_TYPE {
    DelayedWorkQueue = 0,
} WORK_QUEUE_TYPE;

typedef struct _WORK_QUEUE_ITEM {
    PWORKER_THREAD_ROUTINE WorkerRoutine;
    PVOID Parameter;
} WORK_QUEUE_ITEM, *PWORK_QUEUE_ITEM;

/*
 * Forward declaration so ExQueueWorkItem can temporarily adjust the simulated
 * IRQL before the KIRQL typedef is introduced later in this header.
 */
extern volatile unsigned char g_virtiosnd_test_current_irql;

static __forceinline void ExInitializeWorkItem(_Out_ PWORK_QUEUE_ITEM Item, _In_ PWORKER_THREAD_ROUTINE Routine, _In_opt_ PVOID Parameter)
{
    Item->WorkerRoutine = Routine;
    Item->Parameter = Parameter;
}

static __forceinline void ExQueueWorkItem(_Inout_ PWORK_QUEUE_ITEM Item, _In_ WORK_QUEUE_TYPE QueueType)
{
    PWORKER_THREAD_ROUTINE routine;
    PVOID parameter;
    unsigned char old_irql;

    UNREFERENCED_PARAMETER(QueueType);

    routine = Item->WorkerRoutine;
    parameter = Item->Parameter;

    /*
     * Work items run at PASSIVE_LEVEL on a system worker thread. Unit tests are
     * single-threaded, so temporarily drop the simulated IRQL while running the
     * callback.
     */
    old_irql = g_virtiosnd_test_current_irql;
    g_virtiosnd_test_current_irql = 0u; /* PASSIVE_LEVEL */
    routine(parameter);
    g_virtiosnd_test_current_irql = old_irql;
}

/* ---- Interlocked operations (single-process host tests) ---- */

#if defined(_MSC_VER)
static __forceinline LONG InterlockedIncrement(volatile LONG *addend) { return _InterlockedIncrement(addend); }
static __forceinline LONG InterlockedDecrement(volatile LONG *addend) { return _InterlockedDecrement(addend); }
static __forceinline LONG InterlockedExchange(volatile LONG *target, LONG value) { return _InterlockedExchange(target, value); }
static __forceinline LONGLONG InterlockedExchange64(volatile LONGLONG *target, LONGLONG value) { return _InterlockedExchange64(target, value); }
static __forceinline LONG InterlockedCompareExchange(volatile LONG *dest, LONG exchange, LONG comparand)
{
    return _InterlockedCompareExchange(dest, exchange, comparand);
}
#elif defined(_WIN32)
/*
 * Host unit tests are currently single-threaded; keep a Windows fallback that
 * does not depend on GCC/Clang __sync builtins.
 */
static __forceinline LONG InterlockedIncrement(volatile LONG *addend) { return ++(*addend); }
static __forceinline LONG InterlockedDecrement(volatile LONG *addend) { return --(*addend); }
static __forceinline LONG InterlockedExchange(volatile LONG *target, LONG value)
{
    LONG old = *target;
    *target = value;
    return old;
}
static __forceinline LONGLONG InterlockedExchange64(volatile LONGLONG *target, LONGLONG value)
{
    LONGLONG old = *target;
    *target = value;
    return old;
}
static __forceinline LONG InterlockedCompareExchange(volatile LONG *dest, LONG exchange, LONG comparand)
{
    LONG old = *dest;
    if (old == comparand) {
        *dest = exchange;
    }
    return old;
}
#else
static __forceinline LONG InterlockedIncrement(volatile LONG *addend) { return __sync_add_and_fetch(addend, 1); }
static __forceinline LONG InterlockedDecrement(volatile LONG *addend) { return __sync_sub_and_fetch(addend, 1); }
static __forceinline LONG InterlockedExchange(volatile LONG *target, LONG value) { return __sync_lock_test_and_set(target, value); }
static __forceinline LONGLONG InterlockedExchange64(volatile LONGLONG *target, LONGLONG value)
{
    return __sync_lock_test_and_set(target, value);
}
static __forceinline LONG InterlockedCompareExchange(volatile LONG *dest, LONG exchange, LONG comparand)
{
    return __sync_val_compare_and_swap(dest, comparand, exchange);
}
#endif

/* ---- IRQL/spinlock shims ---- */

typedef uint8_t KIRQL;
typedef ULONG KSPIN_LOCK;

#define PASSIVE_LEVEL ((KIRQL)0u)
#define DISPATCH_LEVEL ((KIRQL)2u)

/*
 * Host-test IRQL model:
 *
 * The kernel has the concept of a "current IRQL", which changes as code enters
 * interrupt/DPC context or acquires spinlocks. For host (user-mode) unit tests we
 * model this with a mutable global so tests can intentionally exercise
 * DISPATCH_LEVEL code paths.
 *
 * Default is PASSIVE_LEVEL (defined in test_proto.c).
 */
extern volatile KIRQL g_virtiosnd_test_current_irql;

static __forceinline KIRQL KeGetCurrentIrql(void) { return g_virtiosnd_test_current_irql; }

static __forceinline KIRQL KeRaiseIrqlToDpcLevel(void)
{
    KIRQL old = KeGetCurrentIrql();
    g_virtiosnd_test_current_irql = DISPATCH_LEVEL;
    return old;
}

static __forceinline void KeLowerIrql(KIRQL NewIrql) { g_virtiosnd_test_current_irql = NewIrql; }

static __forceinline void KeInitializeSpinLock(KSPIN_LOCK *lock) { UNREFERENCED_PARAMETER(lock); }

static __forceinline void KeAcquireSpinLock(KSPIN_LOCK *lock, KIRQL *old_irql)
{
    UNREFERENCED_PARAMETER(lock);
    if (old_irql != NULL) {
        *old_irql = KeGetCurrentIrql();
    }
    g_virtiosnd_test_current_irql = DISPATCH_LEVEL;
}

static __forceinline void KeReleaseSpinLock(KSPIN_LOCK *lock, KIRQL old_irql)
{
    UNREFERENCED_PARAMETER(lock);
    g_virtiosnd_test_current_irql = old_irql;
}

static __forceinline void KeMemoryBarrier(void)
{
#if defined(_MSC_VER)
    /* Interlocked operations act as full barriers on Windows. */
    volatile LONG barrier = 0;
    (void)InterlockedCompareExchange(&barrier, 0, 0);
#elif defined(_WIN32)
    /* Best-effort compiler barrier for other Windows toolchains. */
    asm volatile("" ::: "memory");
#else
    __sync_synchronize();
#endif
}

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

typedef void (*VIRTIOSND_TEST_KE_SET_EVENT_HOOK)(KEVENT *event);
extern VIRTIOSND_TEST_KE_SET_EVENT_HOOK g_virtiosnd_test_ke_set_event_hook;

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
    if (g_virtiosnd_test_ke_set_event_hook != NULL) {
        g_virtiosnd_test_ke_set_event_hook(event);
    }
    return old;
}

static __forceinline void KeClearEvent(KEVENT *event) { event->signaled = 0; }
static __forceinline LONG KeReadStateEvent(KEVENT *event) { return event->signaled; }

typedef struct _LARGE_INTEGER {
    LONGLONG QuadPart;
} LARGE_INTEGER;

#if defined(_WIN32)
/*
 * Minimal Win32 declarations for monotonic time/sleep without pulling in
 * <windows.h> (which would clash with our WDK shim typedefs).
 */
#ifndef WINAPI
#define WINAPI __stdcall
#endif
typedef int WINBOOL;
extern WINBOOL WINAPI QueryPerformanceCounter(LARGE_INTEGER *lpPerformanceCount);
extern WINBOOL WINAPI QueryPerformanceFrequency(LARGE_INTEGER *lpFrequency);
extern void WINAPI Sleep(unsigned long dwMilliseconds);

static __forceinline uint64_t virtiosnd_test_monotonic_time_ns(void)
{
    LARGE_INTEGER counter;
    uint64_t freq;
    uint64_t ticks;
    uint64_t sec;
    uint64_t rem;
    uint64_t ns;

    /*
     * QueryPerformanceFrequency() is constant for the lifetime of the process.
     * Cache it to avoid repeated syscalls.
     */
    static uint64_t cached_freq = 0;
    if (cached_freq == 0) {
        LARGE_INTEGER f;
        if (QueryPerformanceFrequency(&f) == 0 || f.QuadPart <= 0) {
            cached_freq = 1; /* avoid division by zero */
        } else {
            cached_freq = (uint64_t)f.QuadPart;
        }
    }

    freq = cached_freq;
    (void)QueryPerformanceCounter(&counter);

    ticks = (uint64_t)counter.QuadPart;
    sec = ticks / freq;
    rem = ticks % freq;

    /* (rem * 1e9) fits in uint64_t since rem < freq and freq <= ~10^9 on Windows. */
    ns = sec * 1000ull * 1000ull * 1000ull + (rem * 1000ull * 1000ull * 1000ull) / freq;
    return ns;
}

static __forceinline void virtiosnd_test_sleep_ns(uint64_t ns)
{
    unsigned long ms;

    if (ns == 0) {
        return;
    }

    /*
     * Sleep() granularity is milliseconds. Round up to guarantee forward progress
     * (the polling loop uses this as a backoff, not for precise timing).
     */
    ms = (unsigned long)((ns + 999999ull) / 1000000ull);
    if (ms == 0) {
        ms = 1;
    }
    Sleep(ms);
}
#else
static __forceinline uint64_t virtiosnd_test_monotonic_time_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000ull * 1000ull * 1000ull + (uint64_t)ts.tv_nsec;
}

static __forceinline void virtiosnd_test_sleep_ns(uint64_t ns)
{
    struct timespec req;
    req.tv_sec = (time_t)(ns / (1000ull * 1000ull * 1000ull));
    req.tv_nsec = (long)(ns % (1000ull * 1000ull * 1000ull));
    (void)nanosleep(&req, NULL);
}
#endif

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

    start_ns = virtiosnd_test_monotonic_time_ns();

    for (;;) {
        uint64_t now_ns;

        if (event->signaled != 0) {
            return STATUS_SUCCESS;
        }

        now_ns = virtiosnd_test_monotonic_time_ns();
        if (now_ns - start_ns >= timeout_ns) {
            return STATUS_TIMEOUT;
        }

        /* Small backoff to keep polling behavior deterministic without busy-spinning. */
        virtiosnd_test_sleep_ns(50ull * 1000ull); /* 50us */
    }
}

/*
 * KeQueryInterruptTime: return monotonic time in 100ns units.
 * Only used for control request timeout calculation in host tests.
 */
static __forceinline ULONGLONG KeQueryInterruptTime(void)
{
    uint64_t now_ns = virtiosnd_test_monotonic_time_ns();
    return (ULONGLONG)(now_ns / 100ull);
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
