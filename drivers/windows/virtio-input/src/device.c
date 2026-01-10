#include "virtio_input.h"

#include <wdmguid.h>

#include "virtio_pci_cap_parser.h"

#if DBG
#define VIOINPUT_TRACE_INFO(...) \
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_INFO_LEVEL, "virtio-input: " __VA_ARGS__)
#define VIOINPUT_TRACE_ERROR(...) \
    DbgPrintEx(DPFLTR_IHVDRIVER_ID, DPFLTR_ERROR_LEVEL, "virtio-input: " __VA_ARGS__)
#else
#define VIOINPUT_TRACE_INFO(...) ((void)0)
#define VIOINPUT_TRACE_ERROR(...) ((void)0)
#endif

static ULONG VioInputReadLe32(_In_reads_(4) const UCHAR* p)
{
    return ((ULONG)p[0]) | ((ULONG)p[1] << 8) | ((ULONG)p[2] << 16) | ((ULONG)p[3] << 24);
}

static NTSTATUS VioInputQueryBusInterface(_In_ WDFDEVICE Device, _Out_ BUS_INTERFACE_STANDARD* BusInterface)
{
    NTSTATUS status;

    RtlZeroMemory(BusInterface, sizeof(*BusInterface));

    status = WdfFdoQueryForInterface(
        Device,
        &GUID_BUS_INTERFACE_STANDARD,
        (PINTERFACE)BusInterface,
        sizeof(*BusInterface),
        1,
        NULL);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    if (BusInterface->InterfaceDereference == NULL) {
        RtlZeroMemory(BusInterface, sizeof(*BusInterface));
        return STATUS_NOT_SUPPORTED;
    }

    if (BusInterface->GetBusData == NULL) {
        BusInterface->InterfaceDereference(BusInterface->Context);
        RtlZeroMemory(BusInterface, sizeof(*BusInterface));
        return STATUS_NOT_SUPPORTED;
    }

    return STATUS_SUCCESS;
}

static NTSTATUS VioInputReadPciConfig(_In_ WDFDEVICE Device, _Out_writes_bytes_(Length) UCHAR* Buffer, _In_ ULONG Offset, _In_ ULONG Length)
{
    BUS_INTERFACE_STANDARD busInterface;
    NTSTATUS status;
    ULONG bytes;

    status = VioInputQueryBusInterface(Device, &busInterface);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    bytes = busInterface.GetBusData(busInterface.Context, PCI_WHICHSPACE_CONFIG, Buffer, Offset, Length);
    busInterface.InterfaceDereference(busInterface.Context);

    if (bytes != Length) {
        return STATUS_IO_DEVICE_ERROR;
    }

    return STATUS_SUCCESS;
}

static VOID VioInputBuildBarAddresses(_In_reads_bytes_(CfgSpaceLen) const UCHAR* CfgSpace,
                                      _In_ size_t CfgSpaceLen,
                                      _Out_writes_(VIRTIO_PCI_BAR_COUNT) UINT64 BarAddrs[VIRTIO_PCI_BAR_COUNT])
{
    ULONG i;

    UNREFERENCED_PARAMETER(CfgSpaceLen);

    for (i = 0; i < VIRTIO_PCI_BAR_COUNT; i++) {
        BarAddrs[i] = 0;
    }

    for (i = 0; i < VIRTIO_PCI_BAR_COUNT; i++) {
        ULONG barOffset;
        ULONG barValue;
        ULONG barType;
        UINT64 base;

        barOffset = 0x10 + (i * 4);
        barValue = VioInputReadLe32(CfgSpace + barOffset);

        if (barValue == 0) {
            continue;
        }

        if ((barValue & 0x1) != 0) {
            continue;
        }

        barType = (barValue >> 1) & 0x3;
        base = (UINT64)(barValue & ~0xFULL);

        if ((barType == 0x2) && (i < (VIRTIO_PCI_BAR_COUNT - 1))) {
            ULONG hiValue;

            hiValue = VioInputReadLe32(CfgSpace + barOffset + 4);
            base |= ((UINT64)hiValue << 32);
            BarAddrs[i] = base;
            i++;
        } else {
            BarAddrs[i] = base;
        }
    }
}

static NTSTATUS VioInputMapBars(_In_ WDFCMRESLIST ResourcesTranslated,
                               _In_reads_(VIRTIO_PCI_BAR_COUNT) const UINT64 BarAddrs[VIRTIO_PCI_BAR_COUNT],
                               _Inout_ VIRTIO_PCI_BAR Bars[VIRTIO_PCI_BAR_COUNT])
{
    ULONG count;
    ULONG i;
    ULONG barIndex;

    count = WdfCmResourceListGetCount(ResourcesTranslated);

    for (barIndex = 0; barIndex < VIRTIO_PCI_BAR_COUNT; barIndex++) {
        UINT64 barAddr;
        PCM_PARTIAL_RESOURCE_DESCRIPTOR desc;

        barAddr = BarAddrs[barIndex];
        if (barAddr == 0) {
            continue;
        }

        for (i = 0; i < count; i++) {
            desc = WdfCmResourceListGetDescriptor(ResourcesTranslated, i);
            if (desc == NULL || desc->Type != CmResourceTypeMemory) {
                continue;
            }

            if ((UINT64)desc->u.Memory.Start.QuadPart == barAddr) {
                Bars[barIndex].Base = desc->u.Memory.Start;
                Bars[barIndex].Length = desc->u.Memory.Length;
                Bars[barIndex].Va = MmMapIoSpace(desc->u.Memory.Start, desc->u.Memory.Length, MmNonCached);
                if (Bars[barIndex].Va == NULL) {
                    VIOINPUT_TRACE_ERROR("MmMapIoSpace failed for BAR%lu (0x%I64x, len=%lu)\n",
                                         barIndex,
                                         (unsigned long long)barAddr,
                                         desc->u.Memory.Length);
                    return STATUS_INSUFFICIENT_RESOURCES;
                }

                break;
            }
        }

        if (Bars[barIndex].Va == NULL) {
            VIOINPUT_TRACE_ERROR("missing translated memory resource for BAR%lu (0x%I64x)\n",
                                 barIndex,
                                 (unsigned long long)barAddr);
            return STATUS_RESOURCE_TYPE_NOT_FOUND;
        }
    }

    return STATUS_SUCCESS;
}

static VOID VioInputUnmapBars(_Inout_ VIRTIO_PCI_BAR Bars[VIRTIO_PCI_BAR_COUNT])
{
    ULONG i;

    for (i = 0; i < VIRTIO_PCI_BAR_COUNT; i++) {
        if (Bars[i].Va != NULL) {
            MmUnmapIoSpace(Bars[i].Va, Bars[i].Length);
            Bars[i].Va = NULL;
        }

        Bars[i].Base.QuadPart = 0;
        Bars[i].Length = 0;
    }
}

static VOID VioInputEvtConfigChange(_In_ WDFDEVICE Device, _In_opt_ PVOID Context)
{
    UNREFERENCED_PARAMETER(Device);
    UNREFERENCED_PARAMETER(Context);

    VIOINPUT_TRACE_INFO("config change interrupt\n");
}

static VOID VioInputEvtDrainQueue(_In_ WDFDEVICE Device, _In_ ULONG QueueIndex, _In_opt_ PVOID Context)
{
    UNREFERENCED_PARAMETER(Device);
    UNREFERENCED_PARAMETER(Context);

    VIOINPUT_TRACE_INFO("queue interrupt: index=%lu\n", QueueIndex);
}

static void VirtioInputReportReady(_In_ void *context)
{
    WDFDEVICE device = (WDFDEVICE)context;
    PDEVICE_CONTEXT deviceContext = VirtioInputGetDeviceContext(device);
    struct virtio_input_report report;

    while (virtio_input_try_pop_report(&deviceContext->InputDevice, &report)) {
        if (report.len == 0) {
            continue;
        }

        (VOID)VirtioInputReportArrived(device, report.data[0], report.data, report.len);
    }
}

NTSTATUS VirtioInputEvtDriverDeviceAdd(_In_ WDFDRIVER Driver, _Inout_ PWDFDEVICE_INIT DeviceInit)
{
    WDF_PNPPOWER_EVENT_CALLBACKS pnpPowerCallbacks;
    WDF_OBJECT_ATTRIBUTES attributes;
    WDFDEVICE device;
    NTSTATUS status;

    UNREFERENCED_PARAMETER(Driver);

    PAGED_CODE();

    WDF_PNPPOWER_EVENT_CALLBACKS_INIT(&pnpPowerCallbacks);
    pnpPowerCallbacks.EvtDevicePrepareHardware = VirtioInputEvtDevicePrepareHardware;
    pnpPowerCallbacks.EvtDeviceReleaseHardware = VirtioInputEvtDeviceReleaseHardware;
    pnpPowerCallbacks.EvtDeviceD0Entry = VirtioInputEvtDeviceD0Entry;
    pnpPowerCallbacks.EvtDeviceD0Exit = VirtioInputEvtDeviceD0Exit;
    WdfDeviceInitSetPnpPowerEventCallbacks(DeviceInit, &pnpPowerCallbacks);

    WdfDeviceInitSetIoType(DeviceInit, WdfDeviceIoBuffered);

    status = VirtioInputFileConfigure(DeviceInit);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    WDF_OBJECT_ATTRIBUTES_INIT_CONTEXT_TYPE(&attributes, DEVICE_CONTEXT);

    status = WdfDeviceCreate(&DeviceInit, &attributes, &device);
    if (!NT_SUCCESS(status)) {
        return status;
    }

    {
        PDEVICE_CONTEXT deviceContext = VirtioInputGetDeviceContext(device);
        status = VirtioInputReadReportQueuesInitialize(device);
        if (!NT_SUCCESS(status)) {
            return status;
        }

        virtio_input_device_init(&deviceContext->InputDevice, VirtioInputReportReady, (void *)device);
    }

    return VirtioInputQueueInitialize(device);
}

NTSTATUS VirtioInputEvtDevicePrepareHardware(
    _In_ WDFDEVICE Device,
    _In_ WDFCMRESLIST ResourcesRaw,
    _In_ WDFCMRESLIST ResourcesTranslated)
{
    PDEVICE_CONTEXT deviceContext;
    UCHAR cfgSpace[256];
    UINT64 barAddrs[VIRTIO_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t caps;
    virtio_pci_cap_parse_result_t parseResult;
    NTSTATUS status;

    PAGED_CODE();

    deviceContext = VirtioInputGetDeviceContext(Device);
    RtlZeroMemory(deviceContext->Bars, sizeof(deviceContext->Bars));
    deviceContext->CommonCfg = NULL;
    deviceContext->IsrStatus = NULL;
    RtlZeroMemory(&deviceContext->Interrupts, sizeof(deviceContext->Interrupts));

    status = VioInputReadPciConfig(Device, cfgSpace, 0, sizeof(cfgSpace));
    if (!NT_SUCCESS(status)) {
        return status;
    }

    VioInputBuildBarAddresses(cfgSpace, sizeof(cfgSpace), barAddrs);

    parseResult = virtio_pci_cap_parse(cfgSpace, sizeof(cfgSpace), barAddrs, &caps);
    if (parseResult != VIRTIO_PCI_CAP_PARSE_OK) {
        VIOINPUT_TRACE_ERROR("virtio_pci_cap_parse failed: %s\n", virtio_pci_cap_parse_result_str(parseResult));
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    status = VioInputMapBars(ResourcesTranslated, barAddrs, deviceContext->Bars);
    if (!NT_SUCCESS(status)) {
        VioInputUnmapBars(deviceContext->Bars);
        return status;
    }

    if (deviceContext->Bars[caps.common_cfg.bar].Va == NULL || deviceContext->Bars[caps.isr_cfg.bar].Va == NULL) {
        VIOINPUT_TRACE_ERROR("missing BAR mapping for common/isr cfg\n");
        VioInputUnmapBars(deviceContext->Bars);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    {
        ULONG commonMinBytes;

        commonMinBytes = FIELD_OFFSET(VIRTIO_PCI_COMMON_CFG, queue_msix_vector) + sizeof(USHORT);
        if ((caps.common_cfg.length < commonMinBytes) ||
            (caps.common_cfg.offset + (UINT64)commonMinBytes > (UINT64)deviceContext->Bars[caps.common_cfg.bar].Length)) {
            VIOINPUT_TRACE_ERROR("common_cfg capability too small for MSI-X vector programming\n");
            VioInputUnmapBars(deviceContext->Bars);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        if ((caps.isr_cfg.length < 1) ||
            (caps.isr_cfg.offset + 1u > (UINT64)deviceContext->Bars[caps.isr_cfg.bar].Length)) {
            VIOINPUT_TRACE_ERROR("isr_cfg capability too small\n");
            VioInputUnmapBars(deviceContext->Bars);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }
    }

    deviceContext->CommonCfg =
        (volatile VIRTIO_PCI_COMMON_CFG*)((PUCHAR)deviceContext->Bars[caps.common_cfg.bar].Va + caps.common_cfg.offset);
    deviceContext->IsrStatus = (volatile UCHAR*)((PUCHAR)deviceContext->Bars[caps.isr_cfg.bar].Va + caps.isr_cfg.offset);

    status = VirtioPciInterruptsPrepareHardware(
        Device,
        &deviceContext->Interrupts,
        ResourcesRaw,
        ResourcesTranslated,
        VIRTIO_INPUT_QUEUE_COUNT,
        deviceContext->IsrStatus,
        VioInputEvtConfigChange,
        VioInputEvtDrainQueue,
        deviceContext);
    if (!NT_SUCCESS(status)) {
        VirtioPciInterruptsReleaseHardware(&deviceContext->Interrupts);
        VioInputUnmapBars(deviceContext->Bars);
        return status;
    }

    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputEvtDeviceReleaseHardware(
    _In_ WDFDEVICE Device,
    _In_ WDFCMRESLIST ResourcesTranslated)
{
    UNREFERENCED_PARAMETER(ResourcesTranslated);

    PAGED_CODE();

    {
        PDEVICE_CONTEXT deviceContext = VirtioInputGetDeviceContext(Device);
        VirtioPciInterruptsReleaseHardware(&deviceContext->Interrupts);
        VioInputUnmapBars(deviceContext->Bars);
        deviceContext->CommonCfg = NULL;
        deviceContext->IsrStatus = NULL;
    }

    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputEvtDeviceD0Entry(
    _In_ WDFDEVICE Device,
    _In_ WDF_POWER_DEVICE_STATE PreviousState)
{
    UNREFERENCED_PARAMETER(PreviousState);

    {
        PDEVICE_CONTEXT deviceContext;
        NTSTATUS status;

        deviceContext = VirtioInputGetDeviceContext(Device);
        status = VirtioPciInterruptsProgramMsixVectors(&deviceContext->Interrupts, deviceContext->CommonCfg);
        if (!NT_SUCCESS(status)) {
            VIOINPUT_TRACE_ERROR("VirtioPciInterruptsProgramMsixVectors failed: 0x%08X\n", status);
            return status;
        }
    }

    return STATUS_SUCCESS;
}

NTSTATUS VirtioInputEvtDeviceD0Exit(_In_ WDFDEVICE Device, _In_ WDF_POWER_DEVICE_STATE TargetState)
{
    UNREFERENCED_PARAMETER(Device);
    UNREFERENCED_PARAMETER(TargetState);

    /*
     * Clear any latched state (prevents "stuck keys" when the device is power
     * cycled or the VM focus changes) and emit an all-zero report to the HID
     * stacks.
     */
    {
        PDEVICE_CONTEXT deviceContext = VirtioInputGetDeviceContext(Device);
        virtio_input_device_reset_state(&deviceContext->InputDevice, true);
    }

    return STATUS_SUCCESS;
}
