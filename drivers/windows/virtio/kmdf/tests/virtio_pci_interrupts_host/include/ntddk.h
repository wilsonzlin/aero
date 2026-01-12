#pragma once

/*
 * Extremely small subset of the Windows WDK `ntddk.h` needed to compile and run
 * `drivers/windows/virtio/kmdf/virtio_pci_interrupts.c` as a host-side unit test
 * binary (Linux CI).
 *
 * This is intentionally minimal: only what the interrupt helper uses is stubbed.
 */

#include <stdint.h>
#include <stddef.h>
#include <string.h>

/* SAL / WDK annotation stubs */
#ifndef _In_
#define _In_
#endif
#ifndef _In_opt_
#define _In_opt_
#endif
#ifndef _Inout_
#define _Inout_
#endif
#ifndef _Inout_opt_
#define _Inout_opt_
#endif
#ifndef _Out_
#define _Out_
#endif
#ifndef _Out_opt_
#define _Out_opt_
#endif
#ifndef _In_reads_
#define _In_reads_(n)
#endif
#ifndef _Use_decl_annotations_
#define _Use_decl_annotations_
#endif
#ifndef _IRQL_requires_max_
#define _IRQL_requires_max_(level)
#endif

#ifndef PASSIVE_LEVEL
#define PASSIVE_LEVEL 0
#endif

#ifndef __forceinline
#if defined(_MSC_VER)
#define __forceinline __forceinline
#else
#define __forceinline inline __attribute__((always_inline))
#endif
#endif

#ifndef UNREFERENCED_PARAMETER
#define UNREFERENCED_PARAMETER(x) (void)(x)
#endif

/* Basic WDK-ish typedefs */
typedef uint8_t UCHAR;
typedef uint16_t USHORT;
typedef uint32_t ULONG;
typedef uint64_t ULONGLONG;
typedef int32_t LONG;
typedef void VOID;
typedef void* PVOID;
typedef uint8_t BOOLEAN;

#ifndef TRUE
#define TRUE ((BOOLEAN)1u)
#endif
#ifndef FALSE
#define FALSE ((BOOLEAN)0u)
#endif

/* NTSTATUS */
typedef int32_t NTSTATUS;

#ifndef NT_SUCCESS
#define NT_SUCCESS(Status) (((NTSTATUS)(Status)) >= 0)
#endif

/* Common status codes used by the helper */
#ifndef STATUS_SUCCESS
#define STATUS_SUCCESS ((NTSTATUS)0x00000000)
#endif
#ifndef STATUS_INVALID_PARAMETER
#define STATUS_INVALID_PARAMETER ((NTSTATUS)0xC000000Du)
#endif
#ifndef STATUS_NOT_SUPPORTED
#define STATUS_NOT_SUPPORTED ((NTSTATUS)0xC00000BBu)
#endif
#ifndef STATUS_RESOURCE_TYPE_NOT_FOUND
#define STATUS_RESOURCE_TYPE_NOT_FOUND ((NTSTATUS)0xC00000EFu)
#endif
#ifndef STATUS_DEVICE_CONFIGURATION_ERROR
#define STATUS_DEVICE_CONFIGURATION_ERROR ((NTSTATUS)0xC0000182u)
#endif
#ifndef STATUS_DEVICE_HARDWARE_ERROR
#define STATUS_DEVICE_HARDWARE_ERROR ((NTSTATUS)0xC0000183u)
#endif
#ifndef STATUS_NOT_FOUND
#define STATUS_NOT_FOUND ((NTSTATUS)0xC0000225u)
#endif

/* Pool types (only what the helper uses) */
typedef enum _POOL_TYPE {
    NonPagedPool = 0,
} POOL_TYPE;

static __forceinline VOID RtlZeroMemory(_Out_ PVOID Destination, _In_ size_t Length)
{
    (VOID)memset(Destination, 0, Length);
}

/*
 * Host-test instrumentation hooks for register reads.
 *
 * The interrupt helper's INTx ISR must always perform a read-to-ack from the
 * ISR status byte. Tests can validate that behavior by observing these globals.
 */
extern unsigned int WdfTestReadRegisterUcharCount;
extern volatile const UCHAR* WdfTestLastReadRegisterUcharAddress;

/* Register access helpers (very small volatile load/store stubs). */
static __forceinline UCHAR READ_REGISTER_UCHAR(_In_ volatile const UCHAR* Register)
{
    WdfTestReadRegisterUcharCount++;
    WdfTestLastReadRegisterUcharAddress = Register;
    return *Register;
}

static __forceinline USHORT READ_REGISTER_USHORT(_In_ volatile const USHORT* Register)
{
    return *Register;
}

static __forceinline VOID WRITE_REGISTER_UCHAR(_Out_ volatile UCHAR* Register, _In_ UCHAR Value)
{
    *Register = Value;
}

static __forceinline VOID WRITE_REGISTER_USHORT(_Out_ volatile USHORT* Register, _In_ USHORT Value)
{
    *Register = Value;
}

static __forceinline VOID WRITE_REGISTER_ULONG(_Out_ volatile ULONG* Register, _In_ ULONG Value)
{
    *Register = Value;
}

/* Interlocked operations (implemented using GCC/Clang atomics). */
static __forceinline LONG InterlockedIncrement(_Inout_ volatile LONG* Addend)
{
    return __atomic_add_fetch(Addend, 1, __ATOMIC_SEQ_CST);
}

static __forceinline LONG InterlockedExchange(_Inout_ volatile LONG* Target, _In_ LONG Value)
{
    return __atomic_exchange_n(Target, Value, __ATOMIC_SEQ_CST);
}

static __forceinline LONG InterlockedOr(_Inout_ volatile LONG* Target, _In_ LONG Value)
{
    return __atomic_fetch_or(Target, Value, __ATOMIC_SEQ_CST);
}

static __forceinline LONG InterlockedCompareExchange(
    _Inout_ volatile LONG* Destination,
    _In_ LONG Exchange,
    _In_ LONG Comperand)
{
    LONG expected = Comperand;
    (VOID)__atomic_compare_exchange_n(Destination, &expected, Exchange, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
    return expected;
}

