/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "portcls_compat.h"

#include "adapter_context.h"
#include "aero_virtio_snd_ioport.h"
#include "topology.h"
#include "trace.h"
#include "wavert.h"

DRIVER_INITIALIZE DriverEntry;

static DRIVER_ADD_DEVICE VirtIoSndIoPortAddDevice;
static DRIVER_DISPATCH VirtIoSndIoPortDispatchPnp;
static NTSTATUS VirtIoSndIoPortStartDevice(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp, _In_ PRESOURCELIST ResourceList);

_Use_decl_annotations_
NTSTATUS
DriverEntry(PDRIVER_OBJECT DriverObject, PUNICODE_STRING RegistryPath)
{
    NTSTATUS status;

    VIRTIOSND_TRACE("DriverEntry (ioport legacy)\n");

    VirtIoSndAdapterContext_Initialize();
    VirtIoSndTopology_Initialize();

    status = PcInitializeAdapterDriver(DriverObject, RegistryPath, VirtIoSndIoPortAddDevice);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    // Wrap PortCls PnP handling so we can stop/unregister virtio transport cleanly
    // on STOP/REMOVE. All other PnP IRPs are forwarded to PcDispatchIrp.
    DriverObject->MajorFunction[IRP_MJ_PNP] = VirtIoSndIoPortDispatchPnp;
    return STATUS_SUCCESS;
}

_Use_decl_annotations_
static NTSTATUS
VirtIoSndIoPortAddDevice(PDRIVER_OBJECT DriverObject, PDEVICE_OBJECT PhysicalDeviceObject)
{
    VIRTIOSND_TRACE("AddDevice (ioport legacy)\n");

    return PcAddAdapterDevice(
        DriverObject,
        PhysicalDeviceObject,
        VirtIoSndIoPortStartDevice,
        2, // max miniports/subdevices
        sizeof(AEROVIOSND_DEVICE_EXTENSION));
}

static VOID
VirtIoSndSafeRelease(_In_opt_ PUNKNOWN Unknown)
{
    if (Unknown != NULL) {
        IUnknown_Release(Unknown);
    }
}

static BOOLEAN
VirtIoSndQueryDwordValue(_In_ HANDLE Key, _In_ PCWSTR ValueNameW, _Out_ ULONG* ValueOut)
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

static BOOLEAN
VirtIoSndTryReadRegistryDword(_In_ PDEVICE_OBJECT DeviceObject,
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

static BOOLEAN
VirtIoSndReadForceNullBackend(_In_ PDEVICE_OBJECT DeviceObject)
{
    ULONG value;
    value = 0;

    if (DeviceObject == NULL) {
        return FALSE;
    }

    /*
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

static NTSTATUS
VirtIoSndIoPortDispatchPnp(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp)
{
    PIO_STACK_LOCATION stack;
    PAEROVIOSND_DEVICE_EXTENSION dx;
    NTSTATUS status;

    stack = IoGetCurrentIrpStackLocation(Irp);
    dx = (PAEROVIOSND_DEVICE_EXTENSION)DeviceObject->DeviceExtension;

    if (dx == NULL || dx->DeviceObject != DeviceObject) {
        return PcDispatchIrp(DeviceObject, Irp);
    }

    switch (stack->MinorFunction) {
    case IRP_MN_STOP_DEVICE:
    case IRP_MN_SURPRISE_REMOVAL:
    case IRP_MN_REMOVE_DEVICE:
    {
        const BOOLEAN isSurpriseRemoval = (stack->MinorFunction == IRP_MN_SURPRISE_REMOVAL);
        PUNKNOWN unknownAdapter;

        unknownAdapter = NULL;

        if (isSurpriseRemoval) {
            /*
             * Prevent any further I/O port touches from our ISR/DPC path while
             * PortCls tears down the audio stack.
             */
            dx->Started = FALSE;
            dx->Vdev.IoBase = NULL;
            dx->Vdev.IoLength = 0;
        }

        status = PcDispatchIrp(DeviceObject, Irp);

        // Best-effort unregistration allows clean STOP/START cycles and ensures
        // subdevices are not left registered after REMOVE.
        (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_WAVE);
        (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_TOPOLOGY);

        if (NT_SUCCESS(PcGetAdapterCommon(DeviceObject, &unknownAdapter))) {
            VirtIoSndAdapterContext_Unregister(unknownAdapter);
            VirtIoSndSafeRelease(unknownAdapter);
        }

        VirtIoSndHwStop(dx);
        return status;
    }

    default:
        break;
    }

    return PcDispatchIrp(DeviceObject, Irp);
}

_Use_decl_annotations_
static NTSTATUS
VirtIoSndIoPortStartDevice(PDEVICE_OBJECT DeviceObject, PIRP Irp, PRESOURCELIST ResourceList)
{
    NTSTATUS status;
    PAEROVIOSND_DEVICE_EXTENSION dx;
    BOOLEAN hwStarted;
    BOOLEAN adapterContextRegistered;
    BOOLEAN topologyRegistered;
    BOOLEAN waveRegistered;
    BOOLEAN forceNullBackend;
    PUNKNOWN unknownAdapter;
    PUNKNOWN unknownWave;
    PUNKNOWN unknownWavePort;
    PPORTWAVERT portWaveRt;
    PUNKNOWN unknownTopo;
    PUNKNOWN unknownTopoPort;
    PPORTTOPOLOGY portTopology;

    UNREFERENCED_PARAMETER(ResourceList);

    VIRTIOSND_TRACE("StartDevice (ioport legacy)\n");

    dx = (PAEROVIOSND_DEVICE_EXTENSION)DeviceObject->DeviceExtension;
    hwStarted = FALSE;
    adapterContextRegistered = FALSE;
    topologyRegistered = FALSE;
    waveRegistered = FALSE;
    forceNullBackend = FALSE;
    unknownAdapter = NULL;
    unknownWave = NULL;
    unknownWavePort = NULL;
    portWaveRt = NULL;
    unknownTopo = NULL;
    unknownTopoPort = NULL;
    portTopology = NULL;

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

    dx->DeviceObject = DeviceObject;

    /*
     * Policy: fail StartDevice if the virtio-snd transport cannot be brought up.
     * If ForceNullBackend is set, allow bring-up to continue so the WaveRT endpoint
     * can be exercised using the null backend.
     */
    forceNullBackend = VirtIoSndReadForceNullBackend(DeviceObject);
    status = VirtIoSndHwStart(dx, Irp);
    if (!NT_SUCCESS(status)) {
        VIRTIOSND_TRACE_ERROR("VirtIoSndHwStart failed: 0x%08X\n", (UINT)status);
        VirtIoSndHwStop(dx); // best-effort cleanup of partial allocations
        if (!forceNullBackend) {
            goto Exit;
        }
        VIRTIOSND_TRACE("ForceNullBackend=1: continuing without virtio transport\n");
        status = STATUS_SUCCESS;
    } else {
        hwStarted = TRUE;
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
        VIRTIOSND_TRACE_ERROR("PcRegisterPhysicalConnection failed: 0x%08X\n", (UINT)status);
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
        if (hwStarted) {
            VirtIoSndHwStop(dx);
        }
    }

    return status;
}
