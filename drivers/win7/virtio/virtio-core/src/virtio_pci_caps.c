#include "../include/virtio_pci_modern.h"
#include "../portable/virtio_pci_cap_parser.h"

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

_IRQL_requires_max_(PASSIVE_LEVEL)
NTSTATUS
VirtioPciCapsDiscover(_In_ PPCI_BUS_INTERFACE_STANDARD PciInterface,
                      _In_ const ULONGLONG BarBases[VIRTIO_PCI_MAX_BARS],
                      _Out_ PVIRTIO_PCI_CAPS Caps)
{
    UCHAR cfg[256];
    ULONG bytesRead;
    uint64_t bar_addrs[VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT];
    virtio_pci_parsed_caps_t parsed;
    virtio_pci_cap_parse_result_t parseRes;
    ULONG i;

    if (PciInterface == NULL || Caps == NULL || BarBases == NULL) {
        return STATUS_INVALID_PARAMETER;
    }

    RtlZeroMemory(Caps, sizeof(*Caps));
    RtlZeroMemory(cfg, sizeof(cfg));

    bytesRead = VirtioPciReadConfig(PciInterface, cfg, 0, sizeof(cfg));
    if (bytesRead != sizeof(cfg)) {
        VIRTIO_CORE_PRINT("PCI config read failed (%lu/%lu)\n", bytesRead, (ULONG)sizeof(cfg));
        return STATUS_DEVICE_DATA_ERROR;
    }

    for (i = 0; i < VIRTIO_PCI_CAP_PARSER_PCI_BAR_COUNT; i++) {
        bar_addrs[i] = (uint64_t)BarBases[i];
    }

    parseRes = virtio_pci_cap_parse(cfg, sizeof(cfg), bar_addrs, &parsed);
    if (parseRes != VIRTIO_PCI_CAP_PARSE_OK) {
        VIRTIO_CORE_PRINT("Virtio PCI capability parse failed: %s (%d)\n",
                          virtio_pci_cap_parse_result_str(parseRes),
                          (int)parseRes);
        return STATUS_DEVICE_CONFIGURATION_ERROR;
    }

    Caps->CommonCfg.Present = TRUE;
    Caps->CommonCfg.CfgType = VIRTIO_PCI_CAP_COMMON_CFG;
    Caps->CommonCfg.Bar = (UCHAR)parsed.common_cfg.bar;
    Caps->CommonCfg.Offset = (ULONG)parsed.common_cfg.offset;
    Caps->CommonCfg.Length = (ULONG)parsed.common_cfg.length;

    Caps->NotifyCfg.Present = TRUE;
    Caps->NotifyCfg.CfgType = VIRTIO_PCI_CAP_NOTIFY_CFG;
    Caps->NotifyCfg.Bar = (UCHAR)parsed.notify_cfg.bar;
    Caps->NotifyCfg.Offset = (ULONG)parsed.notify_cfg.offset;
    Caps->NotifyCfg.Length = (ULONG)parsed.notify_cfg.length;

    Caps->IsrCfg.Present = TRUE;
    Caps->IsrCfg.CfgType = VIRTIO_PCI_CAP_ISR_CFG;
    Caps->IsrCfg.Bar = (UCHAR)parsed.isr_cfg.bar;
    Caps->IsrCfg.Offset = (ULONG)parsed.isr_cfg.offset;
    Caps->IsrCfg.Length = (ULONG)parsed.isr_cfg.length;

    Caps->DeviceCfg.Present = TRUE;
    Caps->DeviceCfg.CfgType = VIRTIO_PCI_CAP_DEVICE_CFG;
    Caps->DeviceCfg.Bar = (UCHAR)parsed.device_cfg.bar;
    Caps->DeviceCfg.Offset = (ULONG)parsed.device_cfg.offset;
    Caps->DeviceCfg.Length = (ULONG)parsed.device_cfg.length;

    Caps->NotifyOffMultiplier = (ULONG)parsed.notify_off_multiplier;

    /*
     * The portable parser returns the required modern capabilities, but not an
     * itemized list of every virtio vendor capability. Populate All[] with the
     * selected required capabilities so VirtioPciModernMapBars knows which
     * BARs to map.
     */
    Caps->AllCount = 0;
    Caps->All[Caps->AllCount++] = Caps->CommonCfg;
    Caps->All[Caps->AllCount++] = Caps->NotifyCfg;
    Caps->All[Caps->AllCount++] = Caps->IsrCfg;
    Caps->All[Caps->AllCount++] = Caps->DeviceCfg;

    return STATUS_SUCCESS;
}
