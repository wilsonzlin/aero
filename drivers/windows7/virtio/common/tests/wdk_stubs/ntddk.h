/* SPDX-License-Identifier: MIT OR Apache-2.0 */
/*
 * Minimal host-buildable ntddk.h stub for virtio common host-side unit tests.
 *
 * This is NOT a complete WDK replacement. It only provides the small subset of
 * WDK surface area required by the test targets under
 * `drivers/windows7/virtio/common/tests/` (e.g. virtio_pci_intx_wdm and
 * virtio_pci_modern_miniport).
 *
 * Note: There are multiple `ntddk.h` shims in this repository for different
 * test suites. Each CMake test target must add this directory to its include
 * path (and ideally with `BEFORE`) to ensure it compiles against the intended
 * stub header.
 */

#pragma once

#include <stdint.h>
#include <stddef.h>
#include <string.h>

/* Basic WDK-like types */
typedef void VOID;
typedef uint8_t UCHAR;
typedef uint16_t USHORT;
typedef uint32_t ULONG;
typedef uint64_t ULONG64;
typedef int32_t LONG;
typedef int64_t LONGLONG;
typedef UCHAR BOOLEAN;
typedef void* PVOID;
typedef UCHAR* PUCHAR;
typedef const UCHAR* PCUCHAR;
typedef uint64_t ULONGLONG;
typedef uintptr_t ULONG_PTR;
typedef uintptr_t UINT_PTR;

#ifndef TRUE
#define TRUE ((BOOLEAN)1u)
#endif
#ifndef FALSE
#define FALSE ((BOOLEAN)0u)
#endif

/* NTSTATUS */
typedef int32_t NTSTATUS;

#define STATUS_SUCCESS ((NTSTATUS)0x00000000)
#define STATUS_INVALID_PARAMETER ((NTSTATUS)0xC000000Du)
#define STATUS_INVALID_DEVICE_STATE ((NTSTATUS)0xC0000184u)
#define STATUS_NOT_SUPPORTED ((NTSTATUS)0xC00000BBu)
#define STATUS_INSUFFICIENT_RESOURCES ((NTSTATUS)0xC000009Au)
#define STATUS_BUFFER_TOO_SMALL ((NTSTATUS)0xC0000023u)
#define STATUS_DEVICE_CONFIGURATION_ERROR ((NTSTATUS)0xC0000182u)
#define STATUS_IO_TIMEOUT ((NTSTATUS)0xC00000B5u)
#define STATUS_NOT_FOUND ((NTSTATUS)0xC0000225u)
#define STATUS_IO_DEVICE_ERROR ((NTSTATUS)0xC0000185u)

#define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)

/* IRQL */
typedef uint8_t KIRQL;
typedef uintptr_t KAFFINITY;

#define PASSIVE_LEVEL ((KIRQL)0u)
#define DISPATCH_LEVEL ((KIRQL)2u)

/* Processor mode */
typedef enum _KPROCESSOR_MODE {
    KernelMode = 0,
    UserMode = 1,
} KPROCESSOR_MODE;

/* LARGE_INTEGER */
typedef struct _LARGE_INTEGER {
    int64_t QuadPart;
} LARGE_INTEGER, *PLARGE_INTEGER;

/* Device object (opaque in tests). */
typedef struct _DEVICE_OBJECT {
    int unused;
} DEVICE_OBJECT, *PDEVICE_OBJECT;

/* Interrupt descriptor bits */
#define CmResourceTypeInterrupt 2u
#define CM_RESOURCE_INTERRUPT_LATCHED 0x0001u
#ifndef CM_RESOURCE_INTERRUPT_MESSAGE
#define CM_RESOURCE_INTERRUPT_MESSAGE 0x0004u
#endif

#define IO_NO_INCREMENT 0

/* SAL annotations -> empty for host build */
#define _In_
#define _Inout_
#define _In_opt_
#define _Inout_opt_
#define _In_reads_bytes_(x)
#define _In_reads_bytes_opt_(x)
#define _Out_writes_(x)
#define _Out_writes_bytes_(x)
#define _Must_inspect_result_
#define _Out_
#define _Out_opt_
#define _IRQL_requires_max_(x)

/* Misc helpers/macros */
#define UNREFERENCED_PARAMETER(P) ((VOID)(P))

/* Always-on ASSERT for host tests (do not depend on NDEBUG). */
#define ASSERT(expr)                                                                                                      \
    do {                                                                                                                 \
        if (!(expr)) {                                                                                                   \
            __builtin_trap();                                                                                             \
        }                                                                                                                \
    } while (0)

#define RtlZeroMemory(Destination, Length) (void)memset((Destination), 0, (Length))
#define RtlCopyMemory(Destination, Source, Length) (void)memcpy((Destination), (Source), (Length))

/* __forceinline used by the production code for small helpers. */
#ifndef __forceinline
#if defined(__GNUC__) || defined(__clang__)
#define __forceinline __inline__ __attribute__((always_inline))
#else
#define __forceinline inline
#endif
#endif

/*
 * MMIO hook layer.
 *
 * Some unit tests need register accesses to behave like real devices (e.g.
 * virtio modern selector-based registers). Tests can install a handler that
 * emulates these semantics. If no handler is installed, accesses fall back to
 * raw memory operations.
 */
typedef BOOLEAN (*WDK_MMIO_READ_HANDLER)(_In_ const volatile VOID* Register, _In_ size_t Width, _Out_ ULONGLONG* ValueOut);
typedef BOOLEAN (*WDK_MMIO_WRITE_HANDLER)(_In_ volatile VOID* Register, _In_ size_t Width, _In_ ULONGLONG Value);

VOID WdkSetMmioHandlers(_In_opt_ WDK_MMIO_READ_HANDLER ReadHandler, _In_opt_ WDK_MMIO_WRITE_HANDLER WriteHandler);
BOOLEAN WdkMmioRead(_In_ const volatile VOID* Register, _In_ size_t Width, _Out_ ULONGLONG* ValueOut);
BOOLEAN WdkMmioWrite(_In_ volatile VOID* Register, _In_ size_t Width, _In_ ULONGLONG Value);

/*
 * Register access.
 *
 * Default READ_REGISTER_UCHAR behaviour is read-to-clear to preserve existing
 * virtio INTx ISR unit tests. Handlers can override this for non-ISR registers.
 */
static __forceinline UCHAR READ_REGISTER_UCHAR(volatile UCHAR* Register)
{
    ULONGLONG v;
    if (WdkMmioRead((const volatile VOID*)Register, sizeof(UCHAR), &v) != FALSE) {
        return (UCHAR)v;
    }

    /* Legacy default: read-to-clear (virtio ISR ACK). */
    {
        UCHAR raw = *Register;
        *Register = 0;
        return raw;
    }
}

static __forceinline USHORT READ_REGISTER_USHORT(volatile USHORT* Register)
{
    ULONGLONG v;
    if (WdkMmioRead((const volatile VOID*)Register, sizeof(USHORT), &v) != FALSE) {
        return (USHORT)v;
    }
    return *Register;
}

static __forceinline ULONG READ_REGISTER_ULONG(volatile ULONG* Register)
{
    ULONGLONG v;
    if (WdkMmioRead((const volatile VOID*)Register, sizeof(ULONG), &v) != FALSE) {
        return (ULONG)v;
    }
    return *Register;
}

static __forceinline ULONG64 READ_REGISTER_ULONG64(volatile ULONG64* Register)
{
    ULONGLONG v;
    if (WdkMmioRead((const volatile VOID*)Register, sizeof(ULONG64), &v) != FALSE) {
        return (ULONG64)v;
    }
    return *Register;
}

static __forceinline VOID WRITE_REGISTER_UCHAR(volatile UCHAR* Register, UCHAR Value)
{
    if (WdkMmioWrite((volatile VOID*)Register, sizeof(UCHAR), (ULONGLONG)Value) != FALSE) {
        return;
    }
    *Register = Value;
}

static __forceinline VOID WRITE_REGISTER_USHORT(volatile USHORT* Register, USHORT Value)
{
    if (WdkMmioWrite((volatile VOID*)Register, sizeof(USHORT), (ULONGLONG)Value) != FALSE) {
        return;
    }
    *Register = Value;
}

static __forceinline VOID WRITE_REGISTER_ULONG(volatile ULONG* Register, ULONG Value)
{
    if (WdkMmioWrite((volatile VOID*)Register, sizeof(ULONG), (ULONGLONG)Value) != FALSE) {
        return;
    }
    *Register = Value;
}

static __forceinline VOID WRITE_REGISTER_ULONG64(volatile ULONG64* Register, ULONG64 Value)
{
    if (WdkMmioWrite((volatile VOID*)Register, sizeof(ULONG64), (ULONGLONG)Value) != FALSE) {
        return;
    }
    *Register = Value;
}

/* Memory barrier + spinlock primitives (sufficient for single-threaded host tests). */
typedef struct _KSPIN_LOCK {
    LONG locked;
} KSPIN_LOCK, *PKSPIN_LOCK;

static __forceinline VOID KeMemoryBarrier(VOID)
{
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
}

static __forceinline VOID KeInitializeSpinLock(_Out_ PKSPIN_LOCK SpinLock)
{
    if (SpinLock == NULL) {
        return;
    }
    SpinLock->locked = 0;
}

static __forceinline VOID KeAcquireSpinLock(_Inout_ PKSPIN_LOCK SpinLock, _Out_ KIRQL* OldIrql)
{
    if (OldIrql != NULL) {
        *OldIrql = PASSIVE_LEVEL;
    }
    if (SpinLock == NULL) {
        return;
    }

    while (__atomic_exchange_n(&SpinLock->locked, 1, __ATOMIC_ACQUIRE) != 0) {
        /* host tests are single-threaded; this should not spin. */
    }
}

static __forceinline VOID KeReleaseSpinLock(_Inout_ PKSPIN_LOCK SpinLock, _In_ KIRQL OldIrql)
{
    (void)OldIrql;
    if (SpinLock == NULL) {
        return;
    }
    __atomic_store_n(&SpinLock->locked, 0, __ATOMIC_RELEASE);
}

VOID WdkTestOnKeStallExecutionProcessor(_In_ ULONG Microseconds);

static __forceinline VOID KeStallExecutionProcessor(_In_ ULONG Microseconds)
{
    WdkTestOnKeStallExecutionProcessor(Microseconds);
    /* Deterministic host tests: do not actually sleep. */
}

/* Interlocked primitives (single-process host tests). */
static __forceinline LONG InterlockedIncrement(volatile LONG* Addend)
{
    return __atomic_add_fetch((LONG*)Addend, 1, __ATOMIC_SEQ_CST);
}

static __forceinline LONG InterlockedDecrement(volatile LONG* Addend)
{
    return __atomic_sub_fetch((LONG*)Addend, 1, __ATOMIC_SEQ_CST);
}

static __forceinline LONG InterlockedExchange(volatile LONG* Target, LONG Value)
{
    return __atomic_exchange_n((LONG*)Target, Value, __ATOMIC_SEQ_CST);
}

static __forceinline LONG InterlockedOr(volatile LONG* Destination, LONG Value)
{
    return __atomic_fetch_or((LONG*)Destination, Value, __ATOMIC_SEQ_CST);
}

static __forceinline LONG InterlockedCompareExchange(volatile LONG* Destination, LONG Exchange, LONG Comperand)
{
    LONG expected = Comperand;
    (void)__atomic_compare_exchange_n((LONG*)Destination, &expected, Exchange, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
    return expected;
}

/* KINTERRUPT */
typedef struct _KINTERRUPT KINTERRUPT, *PKINTERRUPT;

typedef BOOLEAN (*PKSERVICE_ROUTINE)(_In_ PKINTERRUPT Interrupt, _In_ PVOID ServiceContext);

/* KDPC */
typedef struct _KDPC KDPC, *PKDPC;

typedef VOID (*PKDEFERRED_ROUTINE)(_In_ PKDPC Dpc,
                                  _In_ PVOID DeferredContext,
                                  _In_opt_ PVOID SystemArgument1,
                                  _In_opt_ PVOID SystemArgument2);

struct _KDPC {
    PKDEFERRED_ROUTINE DeferredRoutine;
    PVOID DeferredContext;
    PVOID SystemArgument1;
    PVOID SystemArgument2;
    BOOLEAN Inserted;
};

typedef enum _KINTERRUPT_MODE {
    LevelSensitive = 0,
    Latched = 1,
} KINTERRUPT_MODE;

struct _KINTERRUPT {
    PKSERVICE_ROUTINE ServiceRoutine;
    PVOID ServiceContext;
    ULONG Vector;
    KIRQL Irql;
    KIRQL SynchronizeIrql;
    KINTERRUPT_MODE InterruptMode;
    BOOLEAN ShareVector;
    KAFFINITY ProcessorEnableMask;
};

/* CM_PARTIAL_RESOURCE_DESCRIPTOR (minimal interrupt subset). */
typedef struct _CM_PARTIAL_RESOURCE_DESCRIPTOR {
    UCHAR Type;
    UCHAR ShareDisposition;
    USHORT Flags;
    union {
        struct {
            ULONG Vector;
            ULONG Level;
            ULONG Affinity;
        } Interrupt;
    } u;
} CM_PARTIAL_RESOURCE_DESCRIPTOR, *PCM_PARTIAL_RESOURCE_DESCRIPTOR;

/* Stubbed WDK routines implemented in wdk_stubs.c */
NTSTATUS IoConnectInterrupt(_Out_ PKINTERRUPT* InterruptObject,
                            _In_ PKSERVICE_ROUTINE ServiceRoutine,
                            _In_ PVOID ServiceContext,
                            _In_opt_ PVOID SpinLock,
                            _In_ ULONG Vector,
                            _In_ KIRQL Irql,
                            _In_ KIRQL SynchronizeIrql,
                            _In_ KINTERRUPT_MODE InterruptMode,
                            _In_ BOOLEAN ShareVector,
                            _In_ KAFFINITY ProcessorEnableMask,
                            _In_ BOOLEAN FloatingSave);

VOID IoDisconnectInterrupt(_In_ PKINTERRUPT InterruptObject);

VOID KeInitializeDpc(_Out_ PKDPC Dpc, _In_ PKDEFERRED_ROUTINE DeferredRoutine, _In_opt_ PVOID DeferredContext);
BOOLEAN KeInsertQueueDpc(_Inout_ PKDPC Dpc, _In_opt_ PVOID SystemArgument1, _In_opt_ PVOID SystemArgument2);
BOOLEAN KeRemoveQueueDpc(_Inout_ PKDPC Dpc);

KIRQL KeGetCurrentIrql(VOID);

NTSTATUS KeDelayExecutionThread(_In_ KPROCESSOR_MODE WaitMode, _In_ BOOLEAN Alertable, _In_opt_ PLARGE_INTEGER Interval);

/* Debug / time helpers used by some virtio code paths. */
ULONGLONG KeQueryInterruptTime(VOID);

#ifndef DPFLTR_IHVDRIVER_ID
#define DPFLTR_IHVDRIVER_ID 0u
#endif
#ifndef DPFLTR_ERROR_LEVEL
#define DPFLTR_ERROR_LEVEL 0u
#endif

ULONG DbgPrintEx(_In_ ULONG ComponentId, _In_ ULONG Level, _In_ const char* Format, ...);

/*
 * Test-only helpers for driving the stubs deterministically.
 *
 * These are not part of the real WDK API, but are used by host tests to invoke
 * "hardware" events.
 */
BOOLEAN WdkTestTriggerInterrupt(_In_ PKINTERRUPT InterruptObject);
BOOLEAN WdkTestRunQueuedDpc(_Inout_ PKDPC Dpc);

/* Test-only hooks for injecting stub failures. */
VOID WdkTestSetIoConnectInterruptStatus(_In_ NTSTATUS Status);

/* Test-only hooks for controlling IRQL and observing debug output. */
VOID WdkTestSetCurrentIrql(_In_ KIRQL Irql);
ULONG WdkTestGetDbgPrintExCount(VOID);
VOID WdkTestResetDbgPrintExCount(VOID);

ULONG WdkTestGetKeDelayExecutionThreadCount(VOID);
VOID WdkTestResetKeDelayExecutionThreadCount(VOID);

ULONG WdkTestGetKeStallExecutionProcessorCount(VOID);
VOID WdkTestResetKeStallExecutionProcessorCount(VOID);

ULONG WdkTestGetIoConnectInterruptCount(VOID);
VOID WdkTestResetIoConnectInterruptCount(VOID);
ULONG WdkTestGetIoDisconnectInterruptCount(VOID);
VOID WdkTestResetIoDisconnectInterruptCount(VOID);
