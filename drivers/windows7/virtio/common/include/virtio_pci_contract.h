#pragma once

#include <ntddk.h>

/*
 * AERO-W7-VIRTIO contract identity enforcement helpers.
 *
 * Contract v1 encodes the major version in PCI Revision ID (0x01). Drivers MUST
 * refuse to bind to unknown revision IDs and MUST only accept modern virtio-pci
 * device IDs (0x1040+).
 *
 * These helpers are intentionally transport-agnostic: they validate PCI config
 * space identity before drivers map BARs / touch virtqueues.
 */

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
AeroVirtioPciValidateContractV1BusSlot(_In_ ULONG BusNumber,
                                       _In_ ULONG SlotNumber,
                                       _In_reads_opt_(AllowedDeviceIdCount) const USHORT *AllowedDeviceIds,
                                       _In_ ULONG AllowedDeviceIdCount);

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
AeroVirtioPciValidateContractV1Pdo(_In_ PDEVICE_OBJECT PhysicalDeviceObject,
                                  _In_reads_opt_(AllowedDeviceIdCount) const USHORT *AllowedDeviceIds,
                                  _In_ ULONG AllowedDeviceIdCount);
