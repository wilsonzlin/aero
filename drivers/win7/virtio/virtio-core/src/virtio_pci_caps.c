#include "../include/virtio_pci_modern.h"

#ifndef PCI_WHICHSPACE_CONFIG
#define PCI_WHICHSPACE_CONFIG 0
#endif

static ULONG
VirtioPciReadConfig(
    _In_ PPCI_BUS_INTERFACE_STANDARD PciInterface,
    _Out_writes_bytes_(Length) PVOID Buffer,
    _In_ ULONG Offset,
    _In_ ULONG Length)
{
    if (PciInterface->ReadConfig != NULL) {
        return PciInterface->ReadConfig(
            PciInterface->Context, PCI_WHICHSPACE_CONFIG, Buffer, Offset, Length);
    }

    return 0;
}

static VOID
VirtioPciCapsRecordAll(_Inout_ PVIRTIO_PCI_CAPS Caps, _In_ const VIRTIO_PCI_CAP_INFO *CapInfo)
{
    if (Caps->AllCount >= VIRTIO_PCI_MAX_CAPS) {
        return;
    }

    Caps->All[Caps->AllCount] = *CapInfo;
    Caps->AllCount++;
}

static VOID
VirtioPciCapsSelectBest(
    _Inout_ PVIRTIO_PCI_CAP_INFO Selected,
    _In_ const VIRTIO_PCI_CAP_INFO *Candidate,
    _In_ PCSTR Name)
{
    if (!Selected->Present) {
        *Selected = *Candidate;
        return;
    }

    VIRTIO_CORE_PRINT("Duplicate virtio capability %s (cfg_type=%u) at 0x%02x\n",
                      Name,
                      (UINT)Candidate->CfgType,
                      (UINT)Candidate->CapOffset);

    /* Prefer the larger window if duplicates exist. */
    if (Candidate->Length > Selected->Length) {
        *Selected = *Candidate;
    }
}

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciCapsDiscover(_In_ PPCI_BUS_INTERFACE_STANDARD PciInterface, _Out_ PVIRTIO_PCI_CAPS Caps)
{
    UCHAR cfg[256];
    UCHAR visited[256];
    ULONG bytesRead;
    ULONG iterations;
    ULONG capOffset;
    USHORT statusReg;

    if (PciInterface == NULL || Caps == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Caps, sizeof(*Caps));
    RtlZeroMemory(cfg, sizeof(cfg));

    bytesRead = VirtioPciReadConfig(PciInterface, cfg, 0, sizeof(cfg));
    if (bytesRead != sizeof(cfg)) {
        VIRTIO_CORE_PRINT("PCI config read failed (%lu/%lu)\n", bytesRead, (ULONG)sizeof(cfg));
        return STATUS_DEVICE_DATA_ERROR;
    }

    /*
     * Standard PCI Status register bit 4 indicates presence of the capability
     * list. Be permissive (we still try CapabilitiesPtr), but emit a hint.
     */
    statusReg = *(UNALIGNED USHORT *)&cfg[0x06];
    if ((statusReg & 0x0010) == 0) {
        VIRTIO_CORE_PRINT("PCI status CAP_LIST not set (status=0x%04x)\n", (UINT)statusReg);
    }

    capOffset = cfg[0x34] & 0xFC;
    if (capOffset == 0) {
        VIRTIO_CORE_PRINT("PCI CapabilitiesPtr is 0\n");
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    RtlZeroMemory(visited, sizeof(visited));
    iterations = 0;

    while (capOffset != 0) {
        UCHAR capId;
        UCHAR capNext;

        if (iterations++ > 64) {
            VIRTIO_CORE_PRINT("PCI capability list too long / loop suspected\n");
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        if (capOffset < 0x40 || capOffset > 0xFC) {
            VIRTIO_CORE_PRINT("PCI capability pointer out of range: 0x%02x\n", (UINT)capOffset);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        if (visited[capOffset]) {
            VIRTIO_CORE_PRINT("PCI capability list loop detected at 0x%02x\n", (UINT)capOffset);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }
        visited[capOffset] = 1;

        capId = cfg[capOffset];
        capNext = cfg[capOffset + 1] & 0xFC;

        if (capNext != 0 && capNext <= capOffset) {
            VIRTIO_CORE_PRINT("PCI capability next pointer goes backwards (0x%02x -> 0x%02x)\n",
                              (UINT)capOffset,
                              (UINT)capNext);
            return STATUS_DEVICE_CONFIGURATION_ERROR;
        }

        if (capId == VIRTIO_PCI_CAP_ID_VENDOR_SPECIFIC) {
            UCHAR capLen;
            struct virtio_pci_cap vcap;
            VIRTIO_PCI_CAP_INFO capInfo;

            capLen = cfg[capOffset + 2];
            if (capLen < sizeof(struct virtio_pci_cap)) {
                VIRTIO_CORE_PRINT("Vendor capability at 0x%02x has short cap_len=%u\n",
                                  (UINT)capOffset,
                                  (UINT)capLen);
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }

            if ((capOffset + capLen) > sizeof(cfg)) {
                VIRTIO_CORE_PRINT("Vendor capability at 0x%02x overruns config space (len=%u)\n",
                                  (UINT)capOffset,
                                  (UINT)capLen);
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }

            RtlCopyMemory(&vcap, &cfg[capOffset], sizeof(vcap));

            if (vcap.CapVndr != VIRTIO_PCI_CAP_ID_VENDOR_SPECIFIC) {
                VIRTIO_CORE_PRINT("Vendor cap mismatch at 0x%02x (CapVndr=%u)\n",
                                  (UINT)capOffset,
                                  (UINT)vcap.CapVndr);
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }

            if (vcap.Bar > 5) {
                VIRTIO_CORE_PRINT("Virtio cap at 0x%02x has invalid BAR index %u\n",
                                  (UINT)capOffset,
                                  (UINT)vcap.Bar);
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }

            if (vcap.Length == 0) {
                VIRTIO_CORE_PRINT("Virtio cap at 0x%02x has zero length\n", (UINT)capOffset);
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }

            if (((ULONGLONG)vcap.Offset + (ULONGLONG)vcap.Length) > 0xFFFFFFFFui64) {
                VIRTIO_CORE_PRINT("Virtio cap at 0x%02x has offset+length overflow\n", (UINT)capOffset);
                return STATUS_DEVICE_CONFIGURATION_ERROR;
            }

            RtlZeroMemory(&capInfo, sizeof(capInfo));
            capInfo.Present = TRUE;
            capInfo.CfgType = vcap.CfgType;
            capInfo.Bar = vcap.Bar;
            capInfo.Id = vcap.Id;
            capInfo.CapLen = capLen;
            capInfo.CapOffset = capOffset;
            capInfo.Offset = vcap.Offset;
            capInfo.Length = vcap.Length;

            VirtioPciCapsRecordAll(Caps, &capInfo);

            switch (vcap.CfgType) {
            case VIRTIO_PCI_CAP_COMMON_CFG:
                VirtioPciCapsSelectBest(&Caps->CommonCfg, &capInfo, "COMMON_CFG");
                break;
            case VIRTIO_PCI_CAP_NOTIFY_CFG: {
                struct virtio_pci_notify_cap ncap;
                if (capLen < sizeof(ncap)) {
                    VIRTIO_CORE_PRINT("NOTIFY_CFG cap at 0x%02x has short cap_len=%u\n",
                                      (UINT)capOffset,
                                      (UINT)capLen);
                    return STATUS_DEVICE_CONFIGURATION_ERROR;
                }

                RtlCopyMemory(&ncap, &cfg[capOffset], sizeof(ncap));

                VirtioPciCapsSelectBest(&Caps->NotifyCfg, &capInfo, "NOTIFY_CFG");
                if (Caps->NotifyCfg.CapOffset == capOffset) {
                    Caps->NotifyOffMultiplier = ncap.NotifyOffMultiplier;
                }
                break;
            }
            case VIRTIO_PCI_CAP_ISR_CFG:
                VirtioPciCapsSelectBest(&Caps->IsrCfg, &capInfo, "ISR_CFG");
                break;
            case VIRTIO_PCI_CAP_DEVICE_CFG:
                VirtioPciCapsSelectBest(&Caps->DeviceCfg, &capInfo, "DEVICE_CFG");
                break;
            default:
                /* Keep in All[], ignore for required set. */
                break;
            }
        }

        capOffset = capNext;
    }

    if (!Caps->CommonCfg.Present || !Caps->NotifyCfg.Present || !Caps->IsrCfg.Present ||
        !Caps->DeviceCfg.Present) {
        VIRTIO_CORE_PRINT(
            "Missing required virtio modern capabilities: common=%u notify=%u isr=%u device=%u\n",
            (UINT)Caps->CommonCfg.Present,
            (UINT)Caps->NotifyCfg.Present,
            (UINT)Caps->IsrCfg.Present,
            (UINT)Caps->DeviceCfg.Present);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    return STATUS_SUCCESS;
}
