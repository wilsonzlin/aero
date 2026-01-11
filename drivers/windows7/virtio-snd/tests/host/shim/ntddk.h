/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#pragma once

/*
 * Minimal ntddk.h shim for building a subset of the virtio-snd driver in a host
 * unit test environment.
 *
 * This is *not* a full WDK replacement. It only provides the types/macros used
 * by the protocol engine code that we compile under tests/host/.
 */

#include <assert.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* Basic types */
typedef void VOID;
typedef uint8_t UCHAR;
typedef uint16_t USHORT;
typedef uint32_t ULONG;
typedef uint64_t ULONGLONG;
typedef int32_t LONG;
typedef int64_t LONGLONG;
typedef uint32_t UINT32;
typedef uint64_t UINT64;
typedef unsigned int UINT;
typedef uint8_t BOOLEAN;
typedef size_t SIZE_T;
typedef int32_t NTSTATUS;
typedef const char* PCSTR;

typedef void* PVOID;
typedef const void* PCVOID;
typedef UCHAR* PUCHAR;

typedef uint8_t KIRQL;

/* Common boolean constants */
#ifndef TRUE
#define TRUE ((BOOLEAN)1u)
#endif
#ifndef FALSE
#define FALSE ((BOOLEAN)0u)
#endif

/* IRQL levels used by the protocol engines. */
#define PASSIVE_LEVEL ((KIRQL)0u)
#define DISPATCH_LEVEL ((KIRQL)2u)

/* Pool types (ignored by the host shim allocator). */
#define NonPagedPool 0u

/* NTSTATUS helpers */
#define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)

/* A minimal set of NTSTATUS values used by the host-built code. */
#ifndef STATUS_SUCCESS
#define STATUS_SUCCESS ((NTSTATUS)0x00000000L)
#endif
#ifndef STATUS_TIMEOUT
#define STATUS_TIMEOUT ((NTSTATUS)0x00000102L)
#endif
#ifndef STATUS_PENDING
#define STATUS_PENDING ((NTSTATUS)0x00000103L)
#endif
#ifndef STATUS_UNSUCCESSFUL
#define STATUS_UNSUCCESSFUL ((NTSTATUS)0xC0000001L)
#endif
#ifndef STATUS_INVALID_PARAMETER
#define STATUS_INVALID_PARAMETER ((NTSTATUS)0xC000000DL)
#endif
#ifndef STATUS_NOT_SUPPORTED
#define STATUS_NOT_SUPPORTED ((NTSTATUS)0xC00000BBL)
#endif
#ifndef STATUS_INVALID_DEVICE_STATE
#define STATUS_INVALID_DEVICE_STATE ((NTSTATUS)0xC0000184L)
#endif
#ifndef STATUS_INSUFFICIENT_RESOURCES
#define STATUS_INSUFFICIENT_RESOURCES ((NTSTATUS)0xC000009AL)
#endif
#ifndef STATUS_INVALID_BUFFER_SIZE
#define STATUS_INVALID_BUFFER_SIZE ((NTSTATUS)0xC0000206L)
#endif
#ifndef STATUS_INTEGER_OVERFLOW
#define STATUS_INTEGER_OVERFLOW ((NTSTATUS)0xC0000095L)
#endif
#ifndef STATUS_BUFFER_TOO_SMALL
#define STATUS_BUFFER_TOO_SMALL ((NTSTATUS)0xC0000023L)
#endif
#ifndef STATUS_IO_TIMEOUT
#define STATUS_IO_TIMEOUT ((NTSTATUS)0xC00000B5L)
#endif
#ifndef STATUS_CANCELLED
#define STATUS_CANCELLED ((NTSTATUS)0xC0000120L)
#endif
#ifndef STATUS_DEVICE_PROTOCOL_ERROR
#define STATUS_DEVICE_PROTOCOL_ERROR ((NTSTATUS)0xC0000185L)
#endif

/* Assertions */
#define NT_ASSERT(expr) assert(expr)

/* SAL annotations (ignored on host). */
#define _In_
#define _In_opt_
#define _Inout_
#define _Inout_opt_
#define _Out_
#define _Out_opt_
#define _In_reads_(n)
#define _Out_writes_(n)
#define _In_reads_bytes_(n)
#define _Out_writes_bytes_(n)
#define _In_reads_bytes_opt_(n)
#define _Out_writes_bytes_opt_(n)
#define _Must_inspect_result_
#define _Use_decl_annotations_
#define _IRQL_requires_(level)
#define _IRQL_requires_max_(level)
#define _IRQL_requires_min_(level)
#define _IRQL_requires_same_

/* Misc helper macros */
#ifndef UNREFERENCED_PARAMETER
#define UNREFERENCED_PARAMETER(x) ((void)(x))
#endif

#ifndef RTL_NUMBER_OF
#define RTL_NUMBER_OF(arr) (sizeof(arr) / sizeof((arr)[0]))
#endif

#ifndef FIELD_OFFSET
#define FIELD_OFFSET(type, field) offsetof(type, field)
#endif

#ifndef C_ASSERT
#define _C_ASSERT_GLUE(a, b) a##b
#define _C_ASSERT_XGLUE(a, b) _C_ASSERT_GLUE(a, b)
#define C_ASSERT(expr) typedef char _C_ASSERT_XGLUE(_c_assert_, __LINE__)[(expr) ? 1 : -1]
#endif

/* Force-inlining used by driver code. */
#ifndef __forceinline
#if defined(__GNUC__) || defined(__clang__)
#define __forceinline __attribute__((always_inline)) inline
#else
#define __forceinline inline
#endif
#endif

#ifndef UNALIGNED
#define UNALIGNED
#endif

/*
 * Windows LIST_ENTRY helpers used by the tx/rx engines.
 * These are sufficient for the single-threaded host tests.
 */
typedef struct _LIST_ENTRY {
    struct _LIST_ENTRY* Flink;
    struct _LIST_ENTRY* Blink;
} LIST_ENTRY, *PLIST_ENTRY;

static __forceinline VOID InitializeListHead(_Out_ PLIST_ENTRY ListHead)
{
    ListHead->Flink = ListHead;
    ListHead->Blink = ListHead;
}

static __forceinline BOOLEAN IsListEmpty(_In_ const LIST_ENTRY* ListHead)
{
    return (ListHead->Flink == ListHead) ? TRUE : FALSE;
}

static __forceinline VOID InsertTailList(_Inout_ PLIST_ENTRY ListHead, _Inout_ PLIST_ENTRY Entry)
{
    PLIST_ENTRY blink = ListHead->Blink;
    Entry->Flink = ListHead;
    Entry->Blink = blink;
    blink->Flink = Entry;
    ListHead->Blink = Entry;
}

static __forceinline PLIST_ENTRY RemoveHeadList(_Inout_ PLIST_ENTRY ListHead)
{
    PLIST_ENTRY first = ListHead->Flink;
    PLIST_ENTRY next = first->Flink;
    ListHead->Flink = next;
    next->Blink = ListHead;
    first->Flink = first;
    first->Blink = first;
    return first;
}

static __forceinline VOID RemoveEntryList(_Inout_ PLIST_ENTRY Entry)
{
    PLIST_ENTRY blink = Entry->Blink;
    PLIST_ENTRY flink = Entry->Flink;
    blink->Flink = flink;
    flink->Blink = blink;
    Entry->Flink = Entry;
    Entry->Blink = Entry;
}

#ifndef CONTAINING_RECORD
#define CONTAINING_RECORD(address, type, field) ((type*)((char*)(address)-offsetof(type, field)))
#endif

/* Spinlock shims (no-op for host tests). */
typedef struct _KSPIN_LOCK {
    int _unused;
} KSPIN_LOCK, *PKSPIN_LOCK;

static __forceinline VOID KeInitializeSpinLock(_Out_ PKSPIN_LOCK SpinLock)
{
    UNREFERENCED_PARAMETER(SpinLock);
}

static __forceinline KIRQL KeGetCurrentIrql(VOID)
{
    return PASSIVE_LEVEL;
}

static __forceinline VOID KeAcquireSpinLock(_Inout_ PKSPIN_LOCK SpinLock, _Out_ KIRQL* OldIrql)
{
    UNREFERENCED_PARAMETER(SpinLock);
    if (OldIrql != NULL) {
        *OldIrql = PASSIVE_LEVEL;
    }
}

static __forceinline VOID KeReleaseSpinLock(_Inout_ PKSPIN_LOCK SpinLock, _In_ KIRQL OldIrql)
{
    UNREFERENCED_PARAMETER(SpinLock);
    UNREFERENCED_PARAMETER(OldIrql);
}

static __forceinline VOID KeMemoryBarrier(VOID)
{
#if defined(__GNUC__) || defined(__clang__)
    __sync_synchronize();
#else
    /* Best-effort fallback. */
    (void)0;
#endif
}

/* Interlocked primitives (single-threaded tests; implemented with compiler builtins). */
static __forceinline LONG InterlockedIncrement(_Inout_ volatile LONG* Addend)
{
#if defined(__GNUC__) || defined(__clang__)
    return __sync_add_and_fetch(Addend, 1);
#else
    return ++(*Addend);
#endif
}

static __forceinline LONG InterlockedDecrement(_Inout_ volatile LONG* Addend)
{
#if defined(__GNUC__) || defined(__clang__)
    return __sync_sub_and_fetch(Addend, 1);
#else
    return --(*Addend);
#endif
}

/* Memory helpers */
#ifndef RtlZeroMemory
#define RtlZeroMemory(Destination, Length) memset((Destination), 0, (Length))
#endif
#ifndef RtlCopyMemory
#define RtlCopyMemory(Destination, Source, Length) memcpy((Destination), (Source), (Length))
#endif

/* Pool helpers */
static __forceinline PVOID ExAllocatePoolWithTag(_In_ ULONG PoolType, _In_ SIZE_T NumberOfBytes, _In_ ULONG Tag)
{
    UNREFERENCED_PARAMETER(PoolType);
    UNREFERENCED_PARAMETER(Tag);
    return malloc(NumberOfBytes);
}

static __forceinline VOID ExFreePoolWithTag(_In_ PVOID P, _In_ ULONG Tag)
{
    UNREFERENCED_PARAMETER(Tag);
    free(P);
}

/* Opaque kernel structs referenced by headers. */
typedef struct _DEVICE_OBJECT DEVICE_OBJECT;
typedef DEVICE_OBJECT* PDEVICE_OBJECT;

typedef struct _DMA_ADAPTER DMA_ADAPTER;
typedef DMA_ADAPTER* PDMA_ADAPTER;

typedef struct _PHYSICAL_ADDRESS {
    LONGLONG QuadPart;
} PHYSICAL_ADDRESS;

