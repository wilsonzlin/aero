/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "portcls_compat.h"

#include "adapter_context.h"
#include "topology.h"
#include "trace.h"
#include "virtio_pci_contract.h"
#include "virtiosnd.h"
#include "aero_virtio_snd_diag.h"
#include "virtiosnd_control_proto.h"
#include "virtiosnd_intx.h"
#include "wavert.h"

DRIVER_INITIALIZE DriverEntry;

static DRIVER_ADD_DEVICE VirtIoSndAddDevice;
static DRIVER_DISPATCH VirtIoSndDispatchPnp;
static DRIVER_DISPATCH VirtIoSndDispatchCreate;
static DRIVER_DISPATCH VirtIoSndDispatchCleanup;
static DRIVER_DISPATCH VirtIoSndDispatchClose;
static DRIVER_DISPATCH VirtIoSndDispatchDeviceControl;
static DRIVER_DISPATCH VirtIoSndDispatchUnsupported;
static NTSTATUS VirtIoSndStartDevice(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp, _In_ PRESOURCELIST ResourceList);

/* Dedicated diag device object extension (\\.\aero_virtio_snd_diag). */
#define VIRTIOSND_DIAG_SIGNATURE 'gDdV' /* 'VdDg' */
typedef struct _VIRTIOSND_DIAG_DEVICE_EXTENSION {
    ULONG Signature;
    PVIRTIOSND_DEVICE_EXTENSION TargetDx;
} VIRTIOSND_DIAG_DEVICE_EXTENSION, *PVIRTIOSND_DIAG_DEVICE_EXTENSION;

static NTSTATUS VirtIoSndDiagCreate(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);
static VOID VirtIoSndDiagDestroy(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx);

static NTSTATUS VirtIoSndCompleteIrp(_Inout_ PIRP Irp, _In_ NTSTATUS Status, _In_ ULONG_PTR Information);

_Use_decl_annotations_
NTSTATUS DriverEntry(PDRIVER_OBJECT DriverObject, PUNICODE_STRING RegistryPath)
{
    NTSTATUS status;

    VIRTIOSND_TRACE("DriverEntry\n");

    VirtIoSndAdapterContext_Initialize();
    VirtIoSndTopology_Initialize();

    status = PcInitializeAdapterDriver(DriverObject, RegistryPath, VirtIoSndAddDevice);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    // Wrap PortCls PnP handling so we can stop/unregister virtio transport cleanly
    // on STOP/REMOVE. All other PnP IRPs are forwarded to PcDispatchIrp.
    DriverObject->MajorFunction[IRP_MJ_PNP] = VirtIoSndDispatchPnp;
    DriverObject->MajorFunction[IRP_MJ_CREATE] = VirtIoSndDispatchCreate;
    DriverObject->MajorFunction[IRP_MJ_CLEANUP] = VirtIoSndDispatchCleanup;
    DriverObject->MajorFunction[IRP_MJ_CLOSE] = VirtIoSndDispatchClose;
    DriverObject->MajorFunction[IRP_MJ_DEVICE_CONTROL] = VirtIoSndDispatchDeviceControl;
    /*
     * The optional diagnostic device (\\.\aero_virtio_snd_diag) is a standalone
     * control device object and is not part of the PortCls device stack. Ensure
     * unexpected IRPs (ReadFile/WriteFile/etc.) do not get forwarded to PortCls.
     */
    DriverObject->MajorFunction[IRP_MJ_READ] = VirtIoSndDispatchUnsupported;
    DriverObject->MajorFunction[IRP_MJ_WRITE] = VirtIoSndDispatchUnsupported;
    DriverObject->MajorFunction[IRP_MJ_QUERY_INFORMATION] = VirtIoSndDispatchUnsupported;
    DriverObject->MajorFunction[IRP_MJ_SET_INFORMATION] = VirtIoSndDispatchUnsupported;
    DriverObject->MajorFunction[IRP_MJ_QUERY_VOLUME_INFORMATION] = VirtIoSndDispatchUnsupported;
    DriverObject->MajorFunction[IRP_MJ_FLUSH_BUFFERS] = VirtIoSndDispatchUnsupported;
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
static NTSTATUS VirtIoSndAddDevice(PDRIVER_OBJECT DriverObject, PDEVICE_OBJECT PhysicalDeviceObject)
{
    VIRTIOSND_TRACE("AddDevice\n");

    return PcAddAdapterDevice(
        DriverObject,
        PhysicalDeviceObject,
        VirtIoSndStartDevice,
        2, // max miniports/subdevices
        sizeof(VIRTIOSND_DEVICE_EXTENSION) // device extension size
    );
}

static VOID VirtIoSndSafeRelease(_In_opt_ PUNKNOWN Unknown)
{
    if (Unknown != NULL) {
        IUnknown_Release(Unknown);
    }
}

static BOOLEAN VirtIoSndQueryDwordValue(_In_ HANDLE Key, _In_ PCWSTR ValueNameW, _Out_ ULONG* ValueOut)
{
    UNICODE_STRING valueName;
    UCHAR buf[sizeof(KEY_VALUE_PARTIAL_INFORMATION) + sizeof(ULONG)];
    PKEY_VALUE_PARTIAL_INFORMATION info;
    ULONG resultLen;

    if (ValueOut != NULL) {
        *ValueOut = 0;
    }

    if (Key == NULL || ValueNameW == NULL || ValueOut == NULL) {
        return FALSE;
    }

    RtlInitUnicodeString(&valueName, ValueNameW);
    info = (PKEY_VALUE_PARTIAL_INFORMATION)buf;
    RtlZeroMemory(buf, sizeof(buf));
    resultLen = 0;

    if (NT_SUCCESS(ZwQueryValueKey(Key, &valueName, KeyValuePartialInformation, info, sizeof(buf), &resultLen)) &&
        info->Type == REG_DWORD && info->DataLength >= sizeof(ULONG)) {
        *ValueOut = *(UNALIGNED const ULONG*)info->Data;
        return TRUE;
    }

    return FALSE;
}

static BOOLEAN VirtIoSndTryReadRegistryDword(_In_ PDEVICE_OBJECT DeviceObject,
                                             _In_ ULONG RootKeyType,
                                             _In_ PCWSTR ValueNameW,
                                             _Out_ ULONG* ValueOut)
{
    HANDLE rootKey;
    HANDLE paramsKey;
    UNICODE_STRING paramsSubkeyName;
    OBJECT_ATTRIBUTES oa;
    ULONG value;

    rootKey = NULL;
    paramsKey = NULL;
    value = 0;

    if (ValueOut != NULL) {
        *ValueOut = 0;
    }

    if (DeviceObject == NULL || ValueNameW == NULL || ValueOut == NULL) {
        return FALSE;
    }

    if (!NT_SUCCESS(IoOpenDeviceRegistryKey(DeviceObject, RootKeyType, KEY_READ, &rootKey)) || rootKey == NULL) {
        return FALSE;
    }

    RtlInitUnicodeString(&paramsSubkeyName, L"Parameters");
    InitializeObjectAttributes(&oa, &paramsSubkeyName, OBJ_CASE_INSENSITIVE | OBJ_KERNEL_HANDLE, rootKey, NULL);
    if (NT_SUCCESS(ZwOpenKey(&paramsKey, KEY_READ, &oa)) && paramsKey != NULL) {
        if (VirtIoSndQueryDwordValue(paramsKey, ValueNameW, &value)) {
            *ValueOut = value;
            ZwClose(paramsKey);
            ZwClose(rootKey);
            return TRUE;
        }
        ZwClose(paramsKey);
    }

    if (VirtIoSndQueryDwordValue(rootKey, ValueNameW, &value)) {
        *ValueOut = value;
        ZwClose(rootKey);
        return TRUE;
    }

    ZwClose(rootKey);
    return FALSE;
}

static BOOLEAN VirtIoSndReadForceNullBackend(_In_ PDEVICE_OBJECT DeviceObject)
{
    ULONG value;

    value = 0;

    if (DeviceObject == NULL) {
        return FALSE;
    }

    /*
     * Read ForceNullBackend from the per-device registry key.
     *
     * Preferred location (per-device, under the device instance key):
     *   HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend
     *   (REG_DWORD)
     *
     * Fallback: also accept the value in the driver key (PLUGPLAY_REGKEY_DRIVER)
     * for backwards compatibility with older installs.
     */
    if (VirtIoSndTryReadRegistryDword(DeviceObject, PLUGPLAY_REGKEY_DEVICE, L"ForceNullBackend", &value) ||
        VirtIoSndTryReadRegistryDword(DeviceObject, PLUGPLAY_REGKEY_DRIVER, L"ForceNullBackend", &value)) {
        return value ? TRUE : FALSE;
    }

    return FALSE;
}

static BOOLEAN VirtIoSndReadAllowPollingOnly(_In_ PDEVICE_OBJECT DeviceObject)
{
    ULONG value;

    value = 0;

    if (DeviceObject == NULL) {
        return FALSE;
    }

    /*
     * Read AllowPollingOnly from the per-device registry key.
     *
     * Preferred location (per-device, under the device instance key):
     *   HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\AllowPollingOnly
     *   (REG_DWORD)
     *
     * Fallback: also accept the value in the driver key (PLUGPLAY_REGKEY_DRIVER)
     * for backwards compatibility with older installs.
     */
    if (VirtIoSndTryReadRegistryDword(DeviceObject, PLUGPLAY_REGKEY_DEVICE, L"AllowPollingOnly", &value) ||
        VirtIoSndTryReadRegistryDword(DeviceObject, PLUGPLAY_REGKEY_DRIVER, L"AllowPollingOnly", &value)) {
        return value ? TRUE : FALSE;
    }

    return FALSE;
}

static NTSTATUS
VirtIoSndDispatchPnp(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp)
{
    PIO_STACK_LOCATION stack;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    NTSTATUS status;

    stack = IoGetCurrentIrpStackLocation(Irp);
    dx = (PVIRTIOSND_DEVICE_EXTENSION)DeviceObject->DeviceExtension;

    {
        PVIRTIOSND_DIAG_DEVICE_EXTENSION diag;
        diag = (PVIRTIOSND_DIAG_DEVICE_EXTENSION)(DeviceObject ? DeviceObject->DeviceExtension : NULL);
        if (diag != NULL && diag->Signature == VIRTIOSND_DIAG_SIGNATURE) {
            return VirtIoSndCompleteIrp(Irp, STATUS_INVALID_DEVICE_REQUEST, 0);
        }
    }

    if (dx == NULL || dx->Signature != VIRTIOSND_DX_SIGNATURE || dx->Self != DeviceObject) {
        return PcDispatchIrp(DeviceObject, Irp);
    }

    switch (stack->MinorFunction) {
    case IRP_MN_STOP_DEVICE:
    case IRP_MN_SURPRISE_REMOVAL:
    case IRP_MN_REMOVE_DEVICE:
    {
        const BOOLEAN isSurpriseRemoval = (stack->MinorFunction == IRP_MN_SURPRISE_REMOVAL);
        const BOOLEAN isRemoveDevice = (stack->MinorFunction == IRP_MN_REMOVE_DEVICE);
        const BOOLEAN removing = (isSurpriseRemoval || isRemoveDevice);
        PUNKNOWN unknownAdapter;
        ULONG q;

        unknownAdapter = NULL;

        /*
         * Let PortCls quiesce/close pins first so the WaveRT period timer is
         * stopped before we tear down the virtio transport.
         *
         * On SURPRISE_REMOVAL, mark the device removed before PortCls interacts
         * with the miniports to avoid touching BAR-mapped registers after the
         * device is gone.
         */
        if (isSurpriseRemoval) {
            dx->Removed = TRUE;

             /*
              * SURPRISE_REMOVAL means the device may no longer be present on the
              * bus. Prevent any further MMIO touches while PortCls tears down the
              * audio stack:
              *  - disconnect interrupts early so no ISR/DPC path touches BAR-mapped
              *    registers (e.g. INTx read-to-ack on a shared vector)
              *  - invalidate cached notify addresses so late virtqueue kicks don't
              *    write to BAR-mapped memory
              */
            dx->Started = FALSE;
            dx->Intx.IsrStatusRegister = NULL;
            for (q = 0; q < VIRTIOSND_QUEUE_COUNT; ++q) {
                dx->QueueSplit[q].NotifyAddr = NULL;
            }
            VirtIoSndInterruptDisconnect(dx);
        }

        status = PcDispatchIrp(DeviceObject, Irp);

        /*
         * Best-effort unregistration allows clean STOP/START cycles and ensures
         * subdevices are not left registered after REMOVE.
         */
        (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_WAVE);
        (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_TOPOLOGY);

        if (NT_SUCCESS(PcGetAdapterCommon(DeviceObject, &unknownAdapter))) {
            VirtIoSndAdapterContext_Unregister(unknownAdapter);
            VirtIoSndSafeRelease(unknownAdapter);
        }

        if (removing) {
            dx->Removed = TRUE;
        }

        /* Best-effort teardown of the optional diagnostic device. */
        VirtIoSndDiagDestroy(dx);

        VirtIoSndStopHardware(dx);

        if (removing) {
            if (dx->Pdo != NULL && dx->Pdo != dx->LowerDeviceObject) {
                ObDereferenceObject(dx->Pdo);
            }
            if (dx->LowerDeviceObject != NULL) {
                ObDereferenceObject(dx->LowerDeviceObject);
            }
            dx->Pdo = NULL;
            dx->LowerDeviceObject = NULL;
        }

        return status;
    }

    default:
        break;
    }

    return PcDispatchIrp(DeviceObject, Irp);
}

static NTSTATUS VirtIoSndCompleteIrp(_Inout_ PIRP Irp, _In_ NTSTATUS Status, _In_ ULONG_PTR Information)
{
    if (Irp == NULL) {
        return Status;
    }
    Irp->IoStatus.Status = Status;
    Irp->IoStatus.Information = Information;
    IoCompleteRequest(Irp, IO_NO_INCREMENT);
    return Status;
}

_Use_decl_annotations_
static NTSTATUS VirtIoSndDispatchUnsupported(PDEVICE_OBJECT DeviceObject, PIRP Irp)
{
    PVIRTIOSND_DIAG_DEVICE_EXTENSION diag;

    diag = (PVIRTIOSND_DIAG_DEVICE_EXTENSION)(DeviceObject ? DeviceObject->DeviceExtension : NULL);
    if (diag != NULL && diag->Signature == VIRTIOSND_DIAG_SIGNATURE) {
        return VirtIoSndCompleteIrp(Irp, STATUS_INVALID_DEVICE_REQUEST, 0);
    }
    return PcDispatchIrp(DeviceObject, Irp);
}

_Use_decl_annotations_
static NTSTATUS VirtIoSndDispatchCreate(PDEVICE_OBJECT DeviceObject, PIRP Irp)
{
    PVIRTIOSND_DIAG_DEVICE_EXTENSION diag;

    diag = (PVIRTIOSND_DIAG_DEVICE_EXTENSION)(DeviceObject ? DeviceObject->DeviceExtension : NULL);
    if (diag != NULL && diag->Signature == VIRTIOSND_DIAG_SIGNATURE) {
        return VirtIoSndCompleteIrp(Irp, STATUS_SUCCESS, 0);
    }
    return PcDispatchIrp(DeviceObject, Irp);
}

_Use_decl_annotations_
static NTSTATUS VirtIoSndDispatchCleanup(PDEVICE_OBJECT DeviceObject, PIRP Irp)
{
    PVIRTIOSND_DIAG_DEVICE_EXTENSION diag;

    diag = (PVIRTIOSND_DIAG_DEVICE_EXTENSION)(DeviceObject ? DeviceObject->DeviceExtension : NULL);
    if (diag != NULL && diag->Signature == VIRTIOSND_DIAG_SIGNATURE) {
        return VirtIoSndCompleteIrp(Irp, STATUS_SUCCESS, 0);
    }
    return PcDispatchIrp(DeviceObject, Irp);
}

_Use_decl_annotations_
static NTSTATUS VirtIoSndDispatchClose(PDEVICE_OBJECT DeviceObject, PIRP Irp)
{
    PVIRTIOSND_DIAG_DEVICE_EXTENSION diag;

    diag = (PVIRTIOSND_DIAG_DEVICE_EXTENSION)(DeviceObject ? DeviceObject->DeviceExtension : NULL);
    if (diag != NULL && diag->Signature == VIRTIOSND_DIAG_SIGNATURE) {
        return VirtIoSndCompleteIrp(Irp, STATUS_SUCCESS, 0);
    }
    return PcDispatchIrp(DeviceObject, Irp);
}

static VOID VirtIoSndDiagFillInfo(_In_ PVIRTIOSND_DEVICE_EXTENSION Dx, _Out_ PAERO_VIRTIO_SND_DIAG_INFO Info)
{
    ULONG i;
    ULONG irqCount;
    ULONG dpcCount;

    RtlZeroMemory(Info, sizeof(*Info));
    Info->Size = sizeof(*Info);
    Info->Version = AERO_VIRTIO_SND_DIAG_VERSION;

    if (Dx->MessageInterruptsActive) {
        Info->IrqMode = AERO_VIRTIO_SND_DIAG_IRQ_MODE_MSIX;
        Info->MessageCount = Dx->MessageInterruptCount;
        Info->MsixConfigVector = Dx->MsixConfigVector;
        for (i = 0; i < AERO_VIRTIO_SND_DIAG_QUEUE_COUNT; ++i) {
            Info->QueueMsixVector[i] = (i < VIRTIOSND_QUEUE_COUNT) ? Dx->MsixQueueVectors[i] : VIRTIO_PCI_MSI_NO_VECTOR;
        }

        irqCount = (ULONG)InterlockedCompareExchange(&Dx->MessageIsrCount, 0, 0);
        dpcCount = (ULONG)InterlockedCompareExchange(&Dx->MessageDpcCount, 0, 0);
    } else if (Dx->Intx.InterruptObject != NULL) {
        Info->IrqMode = AERO_VIRTIO_SND_DIAG_IRQ_MODE_INTX;
        Info->MessageCount = 0;
        Info->MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
        for (i = 0; i < AERO_VIRTIO_SND_DIAG_QUEUE_COUNT; ++i) {
            Info->QueueMsixVector[i] = VIRTIO_PCI_MSI_NO_VECTOR;
        }

        irqCount = (ULONG)InterlockedCompareExchange(&Dx->Intx.IsrCount, 0, 0);
        dpcCount = (ULONG)InterlockedCompareExchange(&Dx->Intx.DpcCount, 0, 0);
    } else {
        Info->IrqMode = AERO_VIRTIO_SND_DIAG_IRQ_MODE_NONE;
        Info->MessageCount = 0;
        Info->MsixConfigVector = VIRTIO_PCI_MSI_NO_VECTOR;
        for (i = 0; i < AERO_VIRTIO_SND_DIAG_QUEUE_COUNT; ++i) {
            Info->QueueMsixVector[i] = VIRTIO_PCI_MSI_NO_VECTOR;
        }
        irqCount = 0;
        dpcCount = 0;
    }

    Info->InterruptCount = irqCount;
    Info->DpcCount = dpcCount;

    for (i = 0; i < AERO_VIRTIO_SND_DIAG_QUEUE_COUNT; ++i) {
        Info->QueueDrainCount[i] = (ULONG)InterlockedCompareExchange(&Dx->QueueDrainCount[i], 0, 0);
    }
}

_Use_decl_annotations_
static NTSTATUS VirtIoSndDispatchDeviceControl(PDEVICE_OBJECT DeviceObject, PIRP Irp)
{
    PIO_STACK_LOCATION stack;
    PVIRTIOSND_DIAG_DEVICE_EXTENSION diag;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    ULONG code;
    ULONG outLen;

    stack = IoGetCurrentIrpStackLocation(Irp);
    diag = (PVIRTIOSND_DIAG_DEVICE_EXTENSION)(DeviceObject ? DeviceObject->DeviceExtension : NULL);

    if (diag == NULL || diag->Signature != VIRTIOSND_DIAG_SIGNATURE) {
        return PcDispatchIrp(DeviceObject, Irp);
    }

    dx = diag->TargetDx;
    if (dx == NULL || dx->Signature != VIRTIOSND_DX_SIGNATURE) {
        return VirtIoSndCompleteIrp(Irp, STATUS_INVALID_DEVICE_STATE, 0);
    }

    code = stack ? stack->Parameters.DeviceIoControl.IoControlCode : 0;
    outLen = stack ? stack->Parameters.DeviceIoControl.OutputBufferLength : 0;

    switch (code) {
    case IOCTL_AERO_VIRTIO_SND_DIAG_QUERY:
    {
        AERO_VIRTIO_SND_DIAG_INFO info;

        if (outLen < sizeof(info) || Irp->AssociatedIrp.SystemBuffer == NULL) {
            return VirtIoSndCompleteIrp(Irp, STATUS_BUFFER_TOO_SMALL, sizeof(info));
        }

        VirtIoSndDiagFillInfo(dx, &info);
        RtlCopyMemory(Irp->AssociatedIrp.SystemBuffer, &info, sizeof(info));
        return VirtIoSndCompleteIrp(Irp, STATUS_SUCCESS, sizeof(info));
    }
    default:
        return VirtIoSndCompleteIrp(Irp, STATUS_INVALID_DEVICE_REQUEST, 0);
    }
}

static NTSTATUS VirtIoSndDiagCreate(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    NTSTATUS status;
    PDEVICE_OBJECT diagDevice;
    PVIRTIOSND_DIAG_DEVICE_EXTENSION diagExt;
    UNICODE_STRING deviceName;
    UNICODE_STRING symLink;

    if (Dx == NULL || Dx->Self == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    if (Dx->DiagDeviceObject != NULL) {
        return STATUS_SUCCESS;
    }

    RtlInitUnicodeString(&deviceName, L"\\Device\\AeroVirtioSndDiag");
    diagDevice = NULL;
    status = IoCreateDevice(
        Dx->Self->DriverObject,
        (ULONG)sizeof(VIRTIOSND_DIAG_DEVICE_EXTENSION),
        &deviceName,
        FILE_DEVICE_UNKNOWN,
        0,
        FALSE,
        &diagDevice);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    diagDevice->Flags |= DO_BUFFERED_IO;

    diagExt = (PVIRTIOSND_DIAG_DEVICE_EXTENSION)diagDevice->DeviceExtension;
    RtlZeroMemory(diagExt, sizeof(*diagExt));
    diagExt->Signature = VIRTIOSND_DIAG_SIGNATURE;
    diagExt->TargetDx = Dx;

    RtlInitUnicodeString(&symLink, L"\\DosDevices\\aero_virtio_snd_diag");
    status = IoCreateSymbolicLink(&symLink, &deviceName);
    if (!NT_SUCCESS(status)) {
        IoDeleteDevice(diagDevice);
        return status;
    }

    diagDevice->Flags &= ~DO_DEVICE_INITIALIZING;
    Dx->DiagDeviceObject = diagDevice;
    return STATUS_SUCCESS;
}

static VOID VirtIoSndDiagDestroy(_Inout_ PVIRTIOSND_DEVICE_EXTENSION Dx)
{
    UNICODE_STRING symLink;
    PVIRTIOSND_DIAG_DEVICE_EXTENSION diagExt;

    if (Dx == NULL) {
        return;
    }

    if (Dx->DiagDeviceObject == NULL) {
        return;
    }

    /*
     * If a user-mode handle is still open, IoDeleteDevice() will defer final
     * deletion until the last reference goes away. Ensure any late IOCTLs do
     * not dereference a freed adapter device extension by nulling out the
     * pointer first.
     */
    diagExt = (PVIRTIOSND_DIAG_DEVICE_EXTENSION)Dx->DiagDeviceObject->DeviceExtension;
    if (diagExt != NULL && diagExt->Signature == VIRTIOSND_DIAG_SIGNATURE) {
        diagExt->TargetDx = NULL;
    }

    RtlInitUnicodeString(&symLink, L"\\DosDevices\\aero_virtio_snd_diag");
    (VOID)IoDeleteSymbolicLink(&symLink);

    IoDeleteDevice(Dx->DiagDeviceObject);
    Dx->DiagDeviceObject = NULL;
}

#if defined(AERO_VIRTIO_SND_LEGACY)
/*
 * Legacy/transitional validation for the optional QEMU build:
 * - Bind via INF to the transitional virtio-snd PCI ID (PCI\VEN_1AF4&DEV_1018).
 * - Do not require the Aero contract Revision ID (REV_01).
 *
 * This keeps the default (contract) build strict while allowing bring-up on
 * stock QEMU defaults.
 */
static NTSTATUS
VirtIoSndValidateTransitionalPciPdo(_In_ PDEVICE_OBJECT PhysicalDeviceObject)
{
    NTSTATUS status;
    ULONG len;
    ULONG busNumber;
    ULONG slotNumber;
    UCHAR cfg[0x30];
    ULONG cfgLen;
    ULONG bytesRead;
    USHORT vendorId;
    USHORT deviceId;
    UCHAR revisionId;

    if (PhysicalDeviceObject == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    busNumber = 0;
    len = 0;
    status = IoGetDeviceProperty(PhysicalDeviceObject, DevicePropertyBusNumber, (ULONG)sizeof(busNumber), &busNumber, &len);
    if (!NT_SUCCESS(status) || len != (ULONG)sizeof(busNumber)) {
        DbgPrintEx(DPFLTR_IHVDRIVER_ID,
                   DPFLTR_ERROR_LEVEL,
                   "[aero-virtio] failed to query PCI bus number for transitional identity check: %!STATUS!\n",
                   status);
        return STATUS_DEVICE_DATA_ERROR;
    }

    slotNumber = 0;
    len = 0;
    status = IoGetDeviceProperty(PhysicalDeviceObject, DevicePropertyAddress, (ULONG)sizeof(slotNumber), &slotNumber, &len);
    if (!NT_SUCCESS(status) || len != (ULONG)sizeof(slotNumber)) {
        DbgPrintEx(DPFLTR_IHVDRIVER_ID,
                   DPFLTR_ERROR_LEVEL,
                   "[aero-virtio] failed to query PCI slot number for transitional identity check: %!STATUS!\n",
                   status);
        return STATUS_DEVICE_DATA_ERROR;
    }

    RtlZeroMemory(cfg, sizeof(cfg));
    cfgLen = (ULONG)sizeof(cfg);
    bytesRead = HalGetBusDataByOffset(PCIConfiguration, busNumber, slotNumber, cfg, 0, cfgLen);
    if (bytesRead != cfgLen) {
        DbgPrintEx(DPFLTR_IHVDRIVER_ID,
                   DPFLTR_ERROR_LEVEL,
                   "[aero-virtio] HalGetBusDataByOffset(PCI) failed for transitional identity check (%lu/%lu)\n",
                   bytesRead,
                   cfgLen);
        return STATUS_DEVICE_DATA_ERROR;
    }

    vendorId = *(const USHORT *)(cfg + 0x00);
    deviceId = *(const USHORT *)(cfg + 0x02);
    revisionId = *(const UCHAR *)(cfg + 0x08);

    if (vendorId != 0x1af4u || deviceId != 0x1018u) {
        DbgPrintEx(DPFLTR_IHVDRIVER_ID,
                   DPFLTR_ERROR_LEVEL,
                   "[aero-virtio] unexpected PCI ID for transitional virtio-snd build: vendor=%04x device=%04x rev=%02x\n",
                   (UINT)vendorId,
                   (UINT)deviceId,
                   (UINT)revisionId);
        return STATUS_NOT_SUPPORTED;
    }

    return STATUS_SUCCESS;
}
#endif
_Use_decl_annotations_
static NTSTATUS VirtIoSndStartDevice(PDEVICE_OBJECT DeviceObject, PIRP Irp, PRESOURCELIST ResourceList)
{
    NTSTATUS status;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    BOOLEAN hwStarted;
    BOOLEAN adapterContextRegistered;
    BOOLEAN topologyRegistered;
    BOOLEAN waveRegistered;
    BOOLEAN forceNullBackend;
    BOOLEAN allowPollingOnly;
    PUNKNOWN unknownAdapter;
    PUNKNOWN unknownWave;
    PUNKNOWN unknownWavePort;
    PPORTWAVERT portWaveRt;
    PUNKNOWN unknownTopo;
    PUNKNOWN unknownTopoPort;
    PPORTTOPOLOGY portTopology;
    PCM_RESOURCE_LIST raw;
    PCM_RESOURCE_LIST translated;
    PIO_STACK_LOCATION stack;

    VIRTIOSND_TRACE("StartDevice\n");

    dx = (PVIRTIOSND_DEVICE_EXTENSION)DeviceObject->DeviceExtension;
    hwStarted = FALSE;
    adapterContextRegistered = FALSE;
    topologyRegistered = FALSE;
    waveRegistered = FALSE;
    forceNullBackend = FALSE;
    allowPollingOnly = FALSE;
    unknownAdapter = NULL;
    unknownWave = NULL;
    unknownWavePort = NULL;
    portWaveRt = NULL;
    unknownTopo = NULL;
    unknownTopoPort = NULL;
    portTopology = NULL;
    raw = NULL;
    translated = NULL;
    stack = NULL;

    status = PcGetAdapterCommon(DeviceObject, &unknownAdapter);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcGetAdapterCommon failed: 0x%08X\n", (UINT)status);
        return status;
    }

    status = PcRegisterAdapterPowerManagement(unknownAdapter, DeviceObject);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterAdapterPowerManagement failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    if (dx == NULL) {
        status = STATUS_DEVICE_CONFIGURATION_ERROR;
        goto Exit;
    }

    if (dx->Signature != VIRTIOSND_DX_SIGNATURE) {
        RtlZeroMemory(dx, sizeof(*dx));
        dx->Signature = VIRTIOSND_DX_SIGNATURE;
    }

    dx->Self = DeviceObject;
    dx->Removed = FALSE;

    /*
     * Initialize jack state before miniports query KSPROPERTY_JACK_DESCRIPTION.
     *
     * Default to "connected" so behaviour matches the previous static topology
     * when the device never emits jack events.
     */
    VirtIoSndJackStateInit(&dx->JackState);

    /* Initialize interrupt state before any best-effort StopHardware calls. */
    VirtIoSndInterruptInitialize(dx);
    /* Clean up any stale diagnostic device from a previous STOP/START cycle. */
    VirtIoSndDiagDestroy(dx);

    /*
     * Initialize PCM capability/cache state to the contract-v1 baseline.
     *
     * If VirtIoSndStartHardware succeeds, we overwrite these with the device's
     * PCM_INFO-reported formats/rates (filtered to what this driver supports).
     */
    RtlZeroMemory(dx->PcmInfo, sizeof(dx->PcmInfo));
    dx->PcmSupportedFormats[VIRTIO_SND_PLAYBACK_STREAM_ID] = VIRTIO_SND_PCM_FMT_MASK_S16;
    dx->PcmSupportedRates[VIRTIO_SND_PLAYBACK_STREAM_ID] = VIRTIO_SND_PCM_RATE_MASK_48000;
    dx->PcmSupportedFormats[VIRTIO_SND_CAPTURE_STREAM_ID] = VIRTIO_SND_PCM_FMT_MASK_S16;
    dx->PcmSupportedRates[VIRTIO_SND_CAPTURE_STREAM_ID] = VIRTIO_SND_PCM_RATE_MASK_48000;
    dx->PcmSelectedFormat[VIRTIO_SND_PLAYBACK_STREAM_ID] = (UCHAR)VIRTIO_SND_PCM_FMT_S16;
    dx->PcmSelectedRate[VIRTIO_SND_PLAYBACK_STREAM_ID] = (UCHAR)VIRTIO_SND_PCM_RATE_48000;
    dx->PcmSelectedFormat[VIRTIO_SND_CAPTURE_STREAM_ID] = (UCHAR)VIRTIO_SND_PCM_FMT_S16;
    dx->PcmSelectedRate[VIRTIO_SND_CAPTURE_STREAM_ID] = (UCHAR)VIRTIO_SND_PCM_RATE_48000;

    if (dx->LowerDeviceObject == NULL || dx->Pdo == NULL) {
        PDEVICE_OBJECT base = IoGetDeviceAttachmentBaseRef(DeviceObject);
        if (base == NULL) {
            status = STATUS_NO_SUCH_DEVICE;
            goto Exit;
        }

        /* For PortCls adapter drivers the base of the stack is the PDO. */
        dx->Pdo = base;
        dx->LowerDeviceObject = base;
    }

    {
#if defined(AERO_VIRTIO_SND_LEGACY)
        status = VirtIoSndValidateTransitionalPciPdo(dx->Pdo);
#else
        static const USHORT allowedIds[] = {0x1059u};
        status = AeroVirtioPciValidateContractV1Pdo(dx->Pdo, allowedIds, RTL_NUMBER_OF(allowedIds));
#endif
        if (!NT_SUCCESS(status)) {
#if defined(AERO_VIRTIO_SND_LEGACY)
            VIRTIOSND_TRACE_ERROR("virtio-snd PCI identity check failed: 0x%08X\n", (UINT)status);
#else
            VIRTIOSND_TRACE_ERROR("AERO-W7-VIRTIO contract identity check failed: 0x%08X\n", (UINT)status);
#endif
            goto Exit;
        }
    }

    if (Irp == NULL) {
        status = STATUS_INVALID_PARAMETER;
        goto Exit;
    }

    stack = IoGetCurrentIrpStackLocation(Irp);
    if (stack != NULL) {
        raw = stack->Parameters.StartDevice.AllocatedResources;
        translated = stack->Parameters.StartDevice.AllocatedResourcesTranslated;
    }
    if (raw == NULL || translated == NULL) {
        VIRTIOSND_TRACE_ERROR("StartDevice missing CM resources (raw=%p translated=%p)\n", raw, translated);
        status = STATUS_DEVICE_CONFIGURATION_ERROR;
        goto Exit;
    }

    /*
     * Policy: fail StartDevice if the virtio-snd transport cannot be brought up.
     * This surfaces as a Code 10 in Device Manager rather than enumerating a
     * "null backend" audio endpoint.
     *
     * If the per-device ForceNullBackend registry flag is set, allow bring-up to
     * continue even if transport initialization fails, so the WaveRT endpoint can
     * still be exercised using the null backend.
     */
    forceNullBackend = VirtIoSndReadForceNullBackend(DeviceObject);
    allowPollingOnly = VirtIoSndReadAllowPollingOnly(DeviceObject);
    dx->AllowPollingOnly = allowPollingOnly;
    status = VirtIoSndStartHardware(dx, raw, translated);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("VirtIoSndStartHardware failed: 0x%08X\n", (UINT)status);
        VirtIoSndStopHardware(dx); // best-effort cleanup of partial allocations
        if (!forceNullBackend) {
            goto Exit;
        }

        VIRTIOSND_TRACE("ForceNullBackend=1: continuing without virtio transport\n");
        status = STATUS_SUCCESS;
    } else {
        hwStarted = TRUE;

        /*
         * Capability discovery / sanity check:
         *
         * Query VIRTIO_SND_R_PCM_INFO during START_DEVICE so we fail fast if the
         * device model doesn't expose any format/rate/channel combination that
         * the Win7 virtio-snd driver can operate with. This avoids discovering
         * mismatches later during SET_PARAMS / PREPARE / START.
         */
         if (dx->Started) {
             VIRTIO_SND_PCM_INFO playbackInfo;
             VIRTIO_SND_PCM_INFO captureInfo;

             RtlZeroMemory(&playbackInfo, sizeof(playbackInfo));
             RtlZeroMemory(&captureInfo, sizeof(captureInfo));

             /*
              * Cache capabilities into dx->Control.Caps and negotiate a single
              * (channels, format, rate) tuple per stream (VIO-020), preferring
              * the legacy contract-v1 default (S16/48kHz) when available.
              */
             status = VirtioSndCtrlPcmInfoAll(&dx->Control, &playbackInfo, &captureInfo);
             if (!NT_SUCCESS(status)) {
                 VIRTIOSND_TRACE_ERROR("PCM_INFO sanity check failed: 0x%08X\n", (UINT)status);

                 /* Ensure no partially-started transport state remains. */
                 VirtIoSndStopHardware(dx);
                hwStarted = FALSE;

                if (!forceNullBackend) {
                    goto Exit;
                }

                VIRTIOSND_TRACE("ForceNullBackend=1: continuing without virtio transport\n");
                status = STATUS_SUCCESS;
             } else {
                 VIRTIOSND_TRACE(
                     "PCM_INFO stream %lu: dir=%u ch=[%u..%u] formats=0x%I64x rates=0x%I64x\n",
                     (ULONG)playbackInfo.stream_id,
                     (UINT)playbackInfo.direction,
                    (UINT)playbackInfo.channels_min,
                      (UINT)playbackInfo.channels_max,
                      (ULONGLONG)playbackInfo.formats,
                      (ULONGLONG)playbackInfo.rates);

                  VIRTIOSND_TRACE(
                      "PCM_INFO stream %lu: dir=%u ch=[%u..%u] formats=0x%I64x rates=0x%I64x\n",
                      (ULONG)captureInfo.stream_id,
                      (UINT)captureInfo.direction,
                      (UINT)captureInfo.channels_min,
                      (UINT)captureInfo.channels_max,
                      (ULONGLONG)captureInfo.formats,
                      (ULONGLONG)captureInfo.rates);

                  /* Cache stream capabilities for WaveRT pin reporting + SET_PARAMS selection. */
                  dx->PcmInfo[VIRTIO_SND_PLAYBACK_STREAM_ID] = playbackInfo;
                  dx->PcmSupportedFormats[VIRTIO_SND_PLAYBACK_STREAM_ID] = playbackInfo.formats & VIRTIOSND_PCM_DRIVER_SUPPORTED_FORMATS;
                  dx->PcmSupportedRates[VIRTIO_SND_PLAYBACK_STREAM_ID] = playbackInfo.rates & VIRTIOSND_PCM_DRIVER_SUPPORTED_RATES;

                  dx->PcmInfo[VIRTIO_SND_CAPTURE_STREAM_ID] = captureInfo;
                  dx->PcmSupportedFormats[VIRTIO_SND_CAPTURE_STREAM_ID] = captureInfo.formats & VIRTIOSND_PCM_DRIVER_SUPPORTED_FORMATS;
                  dx->PcmSupportedRates[VIRTIO_SND_CAPTURE_STREAM_ID] = captureInfo.rates & VIRTIOSND_PCM_DRIVER_SUPPORTED_RATES;
              }
           }
       }

    if (hwStarted && dx->Started) {
        NTSTATUS diagStatus = VirtIoSndDiagCreate(dx);
        if (!NT_SUCCESS(diagStatus)) {
            VIRTIOSND_TRACE_ERROR("diag: failed to create \\Device\\AeroVirtioSndDiag: 0x%08X\n", (UINT)diagStatus);
        }
    }

    status = VirtIoSndAdapterContext_Register(unknownAdapter, dx, forceNullBackend);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("VirtIoSndAdapterContext_Register failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }
    adapterContextRegistered = TRUE;

    status = VirtIoSndMiniportTopology_Create(&unknownTopo);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("Create topology miniport failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = PcNewPort(&unknownTopoPort, CLSID_PortTopology);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcNewPort(Topology) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = IUnknown_QueryInterface(unknownTopoPort, &IID_IPortTopology, (PVOID*)&portTopology);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("QueryInterface(IPortTopology) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = IPortTopology_Init(portTopology, DeviceObject, Irp, unknownTopo, unknownAdapter, ResourceList);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("IPortTopology::Init failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = PcRegisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_TOPOLOGY, unknownTopoPort, unknownTopo);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterSubdevice(topology) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }
    topologyRegistered = TRUE;

    status = VirtIoSndMiniportWaveRT_Create(&unknownWave);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("Create waveRT miniport failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = PcNewPort(&unknownWavePort, CLSID_PortWaveRT);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcNewPort(WaveRT) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = IUnknown_QueryInterface(unknownWavePort, &IID_IPortWaveRT, (PVOID*)&portWaveRt);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("QueryInterface(IPortWaveRT) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = IPortWaveRT_Init(portWaveRt, DeviceObject, Irp, unknownWave, unknownAdapter, ResourceList);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("IPortWaveRT::Init failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = PcRegisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_WAVE, unknownWavePort, unknownWave);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterSubdevice(wave) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }
    waveRegistered = TRUE;

    status = PcRegisterPhysicalConnection(
        DeviceObject,
        VIRTIOSND_SUBDEVICE_TOPOLOGY,
        VIRTIOSND_TOPO_PIN_BRIDGE,
        VIRTIOSND_SUBDEVICE_WAVE,
        VIRTIOSND_WAVE_PIN_BRIDGE);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterPhysicalConnection(render) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

    status = PcRegisterPhysicalConnection(
        DeviceObject,
        VIRTIOSND_SUBDEVICE_TOPOLOGY,
        VIRTIOSND_TOPO_PIN_BRIDGE_CAPTURE,
        VIRTIOSND_SUBDEVICE_WAVE,
        VIRTIOSND_WAVE_PIN_BRIDGE_CAPTURE);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("PcRegisterPhysicalConnection(capture) failed: 0x%08X\n", (UINT)status);
        goto Exit;
    }

Exit:
    if (!NT_SUCCESS(status) && adapterContextRegistered) {
        VirtIoSndAdapterContext_Unregister(unknownAdapter);
        adapterContextRegistered = FALSE;
    }

    VirtIoSndSafeRelease((PUNKNOWN)portWaveRt);
    VirtIoSndSafeRelease(unknownWavePort);
    VirtIoSndSafeRelease(unknownWave);

    VirtIoSndSafeRelease((PUNKNOWN)portTopology);
    VirtIoSndSafeRelease(unknownTopoPort);
    VirtIoSndSafeRelease(unknownTopo);

    VirtIoSndSafeRelease(unknownAdapter);

    if (!NT_SUCCESS(status)) {
        if (waveRegistered) {
            (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_WAVE);
        }
        if (topologyRegistered) {
            (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_TOPOLOGY);
        }
        /* Ensure the optional diagnostic device does not leak on StartDevice failure. */
        VirtIoSndDiagDestroy(dx);
        if (hwStarted) {
            VirtIoSndStopHardware(dx);
        }
    }
    return status;
}
