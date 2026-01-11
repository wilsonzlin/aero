/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "adapter_context.h"

#include "trace.h"

typedef struct _VIRTIOSND_ADAPTER_CONTEXT_ENTRY {
    LIST_ENTRY ListEntry;
    PUNKNOWN UnknownAdapter;
    PVIRTIOSND_DEVICE_EXTENSION Dx;
    BOOLEAN ForceNullBackend;
} VIRTIOSND_ADAPTER_CONTEXT_ENTRY, *PVIRTIOSND_ADAPTER_CONTEXT_ENTRY;

static LIST_ENTRY g_VirtIoSndAdapterContextList = {&g_VirtIoSndAdapterContextList, &g_VirtIoSndAdapterContextList};
static KSPIN_LOCK g_VirtIoSndAdapterContextLock = 0;

static _Ret_maybenull_ PUNKNOWN
VirtIoSndAdapterContext_Canonicalize(
    _In_ PUNKNOWN UnknownAdapter,
    _Out_ BOOLEAN* NeedsRelease)
{
    PUNKNOWN canonical;

    if (NeedsRelease != NULL) {
        *NeedsRelease = FALSE;
    }

    if (UnknownAdapter == NULL) {
        return UnknownAdapter;
    }

    /*
     * QueryInterface can only be called at PASSIVE_LEVEL. If we're invoked at a
     * higher IRQL (e.g. from an unexpected miniport path), fall back to raw
     * pointer identity.
     */
    if (KeGetCurrentIrql() != PASSIVE_LEVEL) {
        return UnknownAdapter;
    }

    canonical = NULL;
    if (NT_SUCCESS(IUnknown_QueryInterface(UnknownAdapter, &IID_IUnknown, (PVOID*)&canonical)) && canonical != NULL) {
        if (NeedsRelease != NULL) {
            *NeedsRelease = TRUE;
        }
        return canonical;
    }

    return UnknownAdapter;
}

static _Ret_maybenull_ PVIRTIOSND_ADAPTER_CONTEXT_ENTRY
VirtIoSndAdapterContext_FindLocked(_In_ PUNKNOWN UnknownAdapter)
{
    PLIST_ENTRY link;

    for (link = g_VirtIoSndAdapterContextList.Flink; link != &g_VirtIoSndAdapterContextList; link = link->Flink) {
        PVIRTIOSND_ADAPTER_CONTEXT_ENTRY entry;

        entry = CONTAINING_RECORD(link, VIRTIOSND_ADAPTER_CONTEXT_ENTRY, ListEntry);
        if (entry->UnknownAdapter == UnknownAdapter) {
            return entry;
        }
    }

    return NULL;
}

_Use_decl_annotations_
VOID
VirtIoSndAdapterContext_Initialize(VOID)
{
    InitializeListHead(&g_VirtIoSndAdapterContextList);

    /*
     * NOTE: KSPIN_LOCK is semantically initialized by KeInitializeSpinLock.
     * While the loader zeros BSS (and 0 is the unlocked state today), calling
     * KeInitializeSpinLock keeps the intent explicit and avoids relying on
     * undocumented initialization behavior.
     */
    KeInitializeSpinLock(&g_VirtIoSndAdapterContextLock);
}

_Use_decl_annotations_
NTSTATUS
VirtIoSndAdapterContext_Register(PUNKNOWN UnknownAdapter, PVIRTIOSND_DEVICE_EXTENSION Dx, BOOLEAN ForceNullBackend)
{
    NTSTATUS status;
    PVIRTIOSND_ADAPTER_CONTEXT_ENTRY newEntry;
    PUNKNOWN key;
    BOOLEAN keyNeedsRelease;
    KIRQL oldIrql;

    if (UnknownAdapter == NULL || Dx == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    status = STATUS_SUCCESS;
    newEntry = NULL;
    keyNeedsRelease = FALSE;
    key = VirtIoSndAdapterContext_Canonicalize(UnknownAdapter, &keyNeedsRelease);

    KeAcquireSpinLock(&g_VirtIoSndAdapterContextLock, &oldIrql);
    {
        PVIRTIOSND_ADAPTER_CONTEXT_ENTRY existing;

        existing = VirtIoSndAdapterContext_FindLocked(key);
        if (existing != NULL) {
            existing->Dx = Dx;
            existing->ForceNullBackend = ForceNullBackend;
            KeReleaseSpinLock(&g_VirtIoSndAdapterContextLock, oldIrql);
            if (keyNeedsRelease) {
                IUnknown_Release(key);
            }
            return STATUS_SUCCESS;
        }
    }
    KeReleaseSpinLock(&g_VirtIoSndAdapterContextLock, oldIrql);

    /*
     * Hold a reference so the mapping can survive after VirtIoSndStartDevice drops
     * its local PcGetAdapterCommon reference.
     *
     * Note: If Canonicalize() succeeded, the QueryInterface call already took a
     * reference on `key`. Otherwise we must AddRef explicitly.
     */
    if (!keyNeedsRelease) {
        IUnknown_AddRef(key);
        keyNeedsRelease = TRUE;
    }

    newEntry = (PVIRTIOSND_ADAPTER_CONTEXT_ENTRY)ExAllocatePoolWithTag(NonPagedPool, sizeof(*newEntry), VIRTIOSND_POOL_TAG);
    if (newEntry == NULL) {
        if (keyNeedsRelease) {
            IUnknown_Release(key);
        }
        return STATUS_INSUFFICIENT_RESOURCES;
    }

    RtlZeroMemory(newEntry, sizeof(*newEntry));
    newEntry->UnknownAdapter = key;
    newEntry->Dx = Dx;
    newEntry->ForceNullBackend = ForceNullBackend;

    KeAcquireSpinLock(&g_VirtIoSndAdapterContextLock, &oldIrql);
    {
        PVIRTIOSND_ADAPTER_CONTEXT_ENTRY existing;

        existing = VirtIoSndAdapterContext_FindLocked(key);
        if (existing != NULL) {
            existing->Dx = Dx;
            existing->ForceNullBackend = ForceNullBackend;
            status = STATUS_SUCCESS;
        } else {
            InsertTailList(&g_VirtIoSndAdapterContextList, &newEntry->ListEntry);
            newEntry = NULL; /* consumed */
            status = STATUS_SUCCESS;
        }
    }
    KeReleaseSpinLock(&g_VirtIoSndAdapterContextLock, oldIrql);

    if (newEntry != NULL) {
        IUnknown_Release(key);
        ExFreePoolWithTag(newEntry, VIRTIOSND_POOL_TAG);
    }

    return status;
}

_Use_decl_annotations_
VOID
VirtIoSndAdapterContext_Unregister(PUNKNOWN UnknownAdapter)
{
    PVIRTIOSND_ADAPTER_CONTEXT_ENTRY entry;
    PUNKNOWN key;
    BOOLEAN keyNeedsRelease;
    KIRQL oldIrql;

    if (UnknownAdapter == NULL) {
        return;
    }

    entry = NULL;
    keyNeedsRelease = FALSE;
    key = VirtIoSndAdapterContext_Canonicalize(UnknownAdapter, &keyNeedsRelease);

    KeAcquireSpinLock(&g_VirtIoSndAdapterContextLock, &oldIrql);
    {
        entry = VirtIoSndAdapterContext_FindLocked(key);
        if (entry != NULL) {
            RemoveEntryList(&entry->ListEntry);
            InitializeListHead(&entry->ListEntry);
        }
    }
    KeReleaseSpinLock(&g_VirtIoSndAdapterContextLock, oldIrql);

    if (entry != NULL) {
        IUnknown_Release(entry->UnknownAdapter);
        ExFreePoolWithTag(entry, VIRTIOSND_POOL_TAG);
    }

    if (keyNeedsRelease) {
        IUnknown_Release(key);
    }
}

_Use_decl_annotations_
PVIRTIOSND_DEVICE_EXTENSION
VirtIoSndAdapterContext_Lookup(PUNKNOWN UnknownAdapter, BOOLEAN* ForceNullBackendOut)
{
    PVIRTIOSND_DEVICE_EXTENSION dx;
    PUNKNOWN key;
    BOOLEAN keyNeedsRelease;
    KIRQL oldIrql;

    if (ForceNullBackendOut != NULL) {
        *ForceNullBackendOut = FALSE;
    }

    if (UnknownAdapter == NULL) {
        return NULL;
    }

    dx = NULL;
    keyNeedsRelease = FALSE;
    key = VirtIoSndAdapterContext_Canonicalize(UnknownAdapter, &keyNeedsRelease);

    KeAcquireSpinLock(&g_VirtIoSndAdapterContextLock, &oldIrql);
    {
        PVIRTIOSND_ADAPTER_CONTEXT_ENTRY entry;

        entry = VirtIoSndAdapterContext_FindLocked(key);
        if (entry != NULL) {
            dx = entry->Dx;
            if (ForceNullBackendOut != NULL) {
                *ForceNullBackendOut = entry->ForceNullBackend;
            }
        }
    }
    KeReleaseSpinLock(&g_VirtIoSndAdapterContextLock, oldIrql);

    if (keyNeedsRelease) {
        IUnknown_Release(key);
    }

    return dx;
}

_Use_decl_annotations_
VOID
VirtIoSndAdapterContext_UnregisterAndStop(PUNKNOWN UnknownAdapter, BOOLEAN MarkRemoved)
{
    PVIRTIOSND_ADAPTER_CONTEXT_ENTRY entry;
    PUNKNOWN key;
    BOOLEAN keyNeedsRelease;
    KIRQL oldIrql;

    if (UnknownAdapter == NULL) {
        return;
    }

    entry = NULL;
    keyNeedsRelease = FALSE;
    key = VirtIoSndAdapterContext_Canonicalize(UnknownAdapter, &keyNeedsRelease);

    KeAcquireSpinLock(&g_VirtIoSndAdapterContextLock, &oldIrql);
    {
        entry = VirtIoSndAdapterContext_FindLocked(key);
        if (entry != NULL) {
            RemoveEntryList(&entry->ListEntry);
            InitializeListHead(&entry->ListEntry);
        }
    }
    KeReleaseSpinLock(&g_VirtIoSndAdapterContextLock, oldIrql);

    if (entry == NULL) {
        if (keyNeedsRelease) {
            IUnknown_Release(key);
        }
        return;
    }

    if (entry->Dx != NULL) {
        if (MarkRemoved) {
            entry->Dx->Removed = TRUE;
        }

        VirtIoSndStopHardware(entry->Dx);
    }

    IUnknown_Release(entry->UnknownAdapter);
    ExFreePoolWithTag(entry, VIRTIOSND_POOL_TAG);

    if (keyNeedsRelease) {
        IUnknown_Release(key);
    }
}
