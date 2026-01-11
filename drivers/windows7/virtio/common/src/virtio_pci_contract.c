#include "../include/virtio_pci_contract.h"

#include "virtio_pci_identity.h"

static NTSTATUS
AeroVirtioPciGetBusSlotFromPdo(_In_ PDEVICE_OBJECT PhysicalDeviceObject, _Out_ ULONG *BusNumberOut, _Out_ ULONG *SlotNumberOut)
{
    NTSTATUS status;
    ULONG len;
    ULONG busNumber;
    ULONG slotNumber;

    if (PhysicalDeviceObject == NULL || BusNumberOut == NULL || SlotNumberOut == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    busNumber = 0;
    len = 0;
    status = IoGetDeviceProperty(PhysicalDeviceObject, DevicePropertyBusNumber, sizeof(busNumber), &busNumber, &len);
    if (!NT_SUCCESS(status) || len != sizeof(busNumber)) {
        return STATUS_DEVICE_DATA_ERROR;
    }

    slotNumber = 0;
    len = 0;
    status = IoGetDeviceProperty(PhysicalDeviceObject, DevicePropertyAddress, sizeof(slotNumber), &slotNumber, &len);
    if (!NT_SUCCESS(status) || len != sizeof(slotNumber)) {
        return STATUS_DEVICE_DATA_ERROR;
    }

    *BusNumberOut = busNumber;
    *SlotNumberOut = slotNumber;
    return STATUS_SUCCESS;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
AeroVirtioPciValidateContractV1BusSlot(_In_ ULONG BusNumber,
                                       _In_ ULONG SlotNumber,
                                       _In_reads_opt_(AllowedDeviceIdCount) const USHORT *AllowedDeviceIds,
                                       _In_ ULONG AllowedDeviceIdCount)
{
    UCHAR cfg[0x30];
    ULONG bytesRead;
    virtio_pci_identity_t id;
    virtio_pci_identity_result_t res;

    RtlZeroMemory(cfg, sizeof(cfg));
    RtlZeroMemory(&id, sizeof(id));

    bytesRead = HalGetBusDataByOffset(PCIConfiguration, BusNumber, SlotNumber, cfg, 0, sizeof(cfg));
    if (bytesRead != sizeof(cfg)) {
        DbgPrintEx(
            DPFLTR_IHVDRIVER_ID,
            DPFLTR_ERROR_LEVEL,
            "[aero-virtio] HalGetBusDataByOffset(PCI) failed (%lu/%lu)\n",
            bytesRead,
            (ULONG)sizeof(cfg));
        return STATUS_DEVICE_DATA_ERROR;
    }

    res = virtio_pci_identity_validate_aero_contract_v1(
        cfg, sizeof(cfg), AllowedDeviceIds, (size_t)AllowedDeviceIdCount, &id);

    if (res != VIRTIO_PCI_IDENTITY_OK) {
        DbgPrintEx(
            DPFLTR_IHVDRIVER_ID,
            DPFLTR_ERROR_LEVEL,
            "[aero-virtio] AERO-W7-VIRTIO contract identity mismatch: vendor=%04x device=%04x rev=%02x (%s)\n",
            (UINT)id.vendor_id,
            (UINT)id.device_id,
            (UINT)id.revision_id,
            virtio_pci_identity_result_str(res));
        return STATUS_NOT_SUPPORTED;
    }

    return STATUS_SUCCESS;
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
AeroVirtioPciValidateContractV1Pdo(_In_ PDEVICE_OBJECT PhysicalDeviceObject,
                                  _In_reads_opt_(AllowedDeviceIdCount) const USHORT *AllowedDeviceIds,
                                  _In_ ULONG AllowedDeviceIdCount)
{
    NTSTATUS status;
    ULONG busNumber;
    ULONG slotNumber;

    busNumber = 0;
    slotNumber = 0;

    status = AeroVirtioPciGetBusSlotFromPdo(PhysicalDeviceObject, &busNumber, &slotNumber);
    if (!NT_SUCCESS(status)) {
        DbgPrintEx(
            DPFLTR_IHVDRIVER_ID,
            DPFLTR_ERROR_LEVEL,
            "[aero-virtio] failed to query PCI bus/slot for contract identity check: %!STATUS!\n",
            status);
        return status;
    }

    return AeroVirtioPciValidateContractV1BusSlot(busNumber, slotNumber, AllowedDeviceIds, AllowedDeviceIdCount);
}

