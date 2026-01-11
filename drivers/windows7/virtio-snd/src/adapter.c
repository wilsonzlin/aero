/* SPDX-License-Identifier: MIT OR Apache-2.0 */

#include <ntddk.h>

#include "portcls_compat.h"

#include "adapter_context.h"
#include "topology.h"
#include "trace.h"
#include "virtio_pci_contract.h"
#include "virtiosnd.h"
#include "virtiosnd_intx.h"
#include "wavert.h"

DRIVER_INITIALIZE DriverEntry;

static DRIVER_ADD_DEVICE VirtIoSndAddDevice;
static DRIVER_DISPATCH VirtIoSndDispatchPnp;
static NTSTATUS VirtIoSndStartDevice(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp, _In_ PRESOURCELIST ResourceList);

_Use_decl_annotations_
NTSTATUS DriverEntry(PDRIVER_OBJECT DriverObject, PUNICODE_STRING RegistryPath)
{
    NTSTATUS status;

    VIRTIOSND_TRACE("DriverEntry\n");

    VirtIoSndAdapterContext_Initialize();

    status = PcInitializeAdapterDriver(DriverObject, RegistryPath, VirtIoSndAddDevice);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    // Wrap PortCls PnP handling so we can stop/unregister virtio transport cleanly
    // on STOP/REMOVE. All other PnP IRPs are forwarded to PcDispatchIrp.
    DriverObject->MajorFunction[IRP_MJ_PNP] = VirtIoSndDispatchPnp;
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

static BOOLEAN VirtIoSndReadForceNullBackend(_In_ PDEVICE_OBJECT DeviceObject)
{
    HANDLE key;
    UNICODE_STRING valueName;
    UCHAR buf[sizeof(KEY_VALUE_PARTIAL_INFORMATION) + sizeof(ULONG)];
    PKEY_VALUE_PARTIAL_INFORMATION info;
    ULONG resultLen;
    BOOLEAN forceNullBackend;

    forceNullBackend = FALSE;
    key = NULL;

    if (DeviceObject == NULL) {
        return FALSE;
    }

    if (!NT_SUCCESS(IoOpenDeviceRegistryKey(DeviceObject, PLUGPLAY_REGKEY_DEVICE, KEY_READ, &key)) || key == NULL) {
        return FALSE;
    }

    RtlInitUnicodeString(&valueName, L"ForceNullBackend");
    info = (PKEY_VALUE_PARTIAL_INFORMATION)buf;
    RtlZeroMemory(buf, sizeof(buf));
    resultLen = 0;

    if (NT_SUCCESS(ZwQueryValueKey(key, &valueName, KeyValuePartialInformation, info, sizeof(buf), &resultLen)) &&
        info->Type == REG_DWORD && info->DataLength >= sizeof(ULONG)) {
        forceNullBackend = (*(UNALIGNED const ULONG*)info->Data) ? TRUE : FALSE;
    }

    ZwClose(key);
    return forceNullBackend;
}

static NTSTATUS
VirtIoSndDispatchPnp(_In_ PDEVICE_OBJECT DeviceObject, _In_ PIRP Irp)
{
    PIO_STACK_LOCATION stack;
    PVIRTIOSND_DEVICE_EXTENSION dx;
    NTSTATUS status;

    stack = IoGetCurrentIrpStackLocation(Irp);
    dx = (PVIRTIOSND_DEVICE_EXTENSION)DeviceObject->DeviceExtension;

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
             *  - disconnect INTx early so the ISR doesn't read the virtio ISR byte
             *    on a shared vector
             *  - invalidate cached notify addresses so late virtqueue kicks don't
             *    write to BAR-mapped memory
             */
            dx->Started = FALSE;
            dx->Intx.IsrStatusRegister = NULL;
            for (q = 0; q < VIRTIOSND_QUEUE_COUNT; ++q) {
                dx->QueueSplit[q].NotifyAddr = NULL;
            }
            VirtIoSndIntxDisconnect(dx);
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

    /* Initialize INTx DPC state before any best-effort StopHardware calls. */
    VirtIoSndIntxInitialize(dx);

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

    if (!NT_SUCCESS(status) && hwStarted) {
        if (waveRegistered) {
            (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_WAVE);
        }
        if (topologyRegistered) {
            (VOID)PcUnregisterSubdevice(DeviceObject, VIRTIOSND_SUBDEVICE_TOPOLOGY);
        }
        VirtIoSndStopHardware(dx);
    }
    return status;
}
