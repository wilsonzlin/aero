#include "virtio_input.h"

static BOOLEAN VirtioInputAsciiEqualsInsensitive(_In_reads_bytes_(Len) const CHAR *A, _In_ size_t Len, _In_z_ const CHAR *B)
{
    size_t i;
    for (i = 0; i < Len; i++) {
        CHAR ca = A[i];
        CHAR cb = B[i];

        if (cb == '\0') {
            return FALSE;
        }

        if (ca >= 'A' && ca <= 'Z') {
            ca = (CHAR)(ca - 'A' + 'a');
        }

        if (cb >= 'A' && cb <= 'Z') {
            cb = (CHAR)(cb - 'A' + 'a');
        }

        if (ca != cb) {
            return FALSE;
        }
    }

    return B[Len] == '\0';
}

static ULONG VirtioInputGetCollectionNumberFromCreateRequest(_In_ WDFREQUEST Request)
{
    /*
     * IRP_MJ_CREATE can originate from user mode. The EA buffer pointer in the
     * create parameters may then reference user memory. This driver only needs
     * collection EAs for HIDCLASS/kernel opens, so ignore user-mode EAs to avoid
     * dereferencing untrusted pointers.
     */
    if (WdfRequestGetRequestorMode(Request) == UserMode) {
        return 0;
    }

    PIRP irp = WdfRequestWdmGetIrp(Request);
    PIO_STACK_LOCATION irpSp = IoGetCurrentIrpStackLocation(irp);

    if (irpSp == NULL) {
        return 0;
    }

    PUCHAR eaBuffer = (PUCHAR)irpSp->Parameters.Create.EaBuffer;
    ULONG eaLength = irpSp->Parameters.Create.EaLength;

    if (eaBuffer == NULL || eaLength < FIELD_OFFSET(FILE_FULL_EA_INFORMATION, EaName)) {
        return 0;
    }

    PUCHAR cursor = eaBuffer;
    PUCHAR end = eaBuffer + eaLength;

    while (cursor + FIELD_OFFSET(FILE_FULL_EA_INFORMATION, EaName) <= end) {
        PFILE_FULL_EA_INFORMATION entry = (PFILE_FULL_EA_INFORMATION)cursor;

        ULONG entrySize = entry->NextEntryOffset;
        if (entrySize == 0) {
            entrySize = (ULONG)(end - cursor);
        }

        if (entrySize < FIELD_OFFSET(FILE_FULL_EA_INFORMATION, EaName) || cursor + entrySize > end) {
            break;
        }

        ULONG required = FIELD_OFFSET(FILE_FULL_EA_INFORMATION, EaName) + entry->EaNameLength + 1 + entry->EaValueLength;
        if (required > entrySize) {
            break;
        }

        const CHAR *eaName = (const CHAR *)entry->EaName;
        const UCHAR *eaValue = (const UCHAR *)(entry->EaName + entry->EaNameLength + 1);

        if (VirtioInputAsciiEqualsInsensitive(eaName, entry->EaNameLength, "HidCollection") ||
            VirtioInputAsciiEqualsInsensitive(eaName, entry->EaNameLength, "HID_COLLECTION") ||
            VirtioInputAsciiEqualsInsensitive(eaName, entry->EaNameLength, "HidCollectionNumber") ||
            VirtioInputAsciiEqualsInsensitive(eaName, entry->EaNameLength, "HID_COLLECTION_NUMBER")) {

            if (entry->EaValueLength >= sizeof(ULONG)) {
                return *(UNALIGNED const ULONG *)eaValue;
            }
            if (entry->EaValueLength >= sizeof(USHORT)) {
                return *(UNALIGNED const USHORT *)eaValue;
            }
            if (entry->EaValueLength >= sizeof(UCHAR)) {
                return *(UNALIGNED const UCHAR *)eaValue;
            }

            return 0;
        }

        if (entry->NextEntryOffset == 0) {
            break;
        }

        cursor += entry->NextEntryOffset;
    }

    return 0;
}

static VOID VirtioInputEvtDeviceFileCreate(_In_ WDFDEVICE Device, _In_ WDFREQUEST Request, _In_ WDFFILEOBJECT FileObject)
{
    PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(Device);
    PVIRTIO_INPUT_FILE_CONTEXT fileCtx = VirtioInputGetFileContext(FileObject);

    PIRP irp = WdfRequestWdmGetIrp(Request);
    PIO_STACK_LOCATION irpSp = (irp == NULL) ? NULL : IoGetCurrentIrpStackLocation(irp);
    fileCtx->HasCollectionEa =
        (WdfRequestGetRequestorMode(Request) == KernelMode && irpSp != NULL && irpSp->Parameters.Create.EaBuffer != NULL &&
         irpSp->Parameters.Create.EaLength != 0);

    fileCtx->CollectionNumber = VirtioInputGetCollectionNumberFromCreateRequest(Request);

    switch (fileCtx->CollectionNumber) {
    case 1:
        if (devCtx->DeviceKind == VioInputDeviceKindMouse) {
            fileCtx->DefaultReportId = VIRTIO_INPUT_REPORT_ID_MOUSE;
        } else if (devCtx->DeviceKind == VioInputDeviceKindKeyboard) {
            fileCtx->DefaultReportId = VIRTIO_INPUT_REPORT_ID_KEYBOARD;
        } else {
            fileCtx->DefaultReportId = VIRTIO_INPUT_REPORT_ID_ANY;
        }
        break;
    default:
        fileCtx->DefaultReportId = VIRTIO_INPUT_REPORT_ID_ANY;
        break;
    }

    WdfRequestComplete(Request, STATUS_SUCCESS);
}

static VOID VirtioInputEvtFileClose(_In_ WDFFILEOBJECT FileObject)
{
    UNREFERENCED_PARAMETER(FileObject);
}

static VOID VirtioInputEvtFileCleanup(_In_ WDFFILEOBJECT FileObject)
{
    WDFDEVICE device = WdfFileObjectGetDevice(FileObject);
    PDEVICE_CONTEXT devCtx = VirtioInputGetDeviceContext(device);

    UCHAR i;
    for (i = 0; i <= VIRTIO_INPUT_MAX_REPORT_ID; i++) {
        if (devCtx->ReadReportQueue[i] == NULL) {
            continue;
        }

        for (;;) {
            WDFREQUEST found = NULL;
            NTSTATUS status = WdfIoQueueFindRequest(devCtx->ReadReportQueue[i], NULL, FileObject, NULL, &found);
            if (!NT_SUCCESS(status)) {
                break;
            }

            WDFREQUEST request = NULL;
            status = WdfIoQueueRetrieveFoundRequest(devCtx->ReadReportQueue[i], found, &request);
            WdfObjectDereference(found);

            if (!NT_SUCCESS(status)) {
                break;
            }

            VioInputCounterInc(&devCtx->Counters.ReadReportCancelled);
            VioInputCounterDec(&devCtx->Counters.ReadReportQueueDepth);
            VIOINPUT_LOG(
                VIOINPUT_LOG_QUEUE,
                "READ_REPORT cancelled (file cleanup): pending=%ld\n",
                devCtx->Counters.ReadReportQueueDepth);

            WdfRequestComplete(request, STATUS_CANCELLED);
        }
    }
}

NTSTATUS VirtioInputFileConfigure(_Inout_ WDFDEVICE_INIT *DeviceInit)
{
    WDF_FILEOBJECT_CONFIG fileConfig;
    WDF_OBJECT_ATTRIBUTES fileAttributes;

    WDF_FILEOBJECT_CONFIG_INIT(&fileConfig, VirtioInputEvtDeviceFileCreate, VirtioInputEvtFileClose, VirtioInputEvtFileCleanup);

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&fileAttributes, VIRTIO_INPUT_FILE_CONTEXT);

    WdfDeviceInitSetFileObjectConfig(DeviceInit, &fileConfig, &fileAttributes);

    return STATUS_SUCCESS;
}
