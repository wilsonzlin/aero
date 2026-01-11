# Windows Driver Development

This directory collects practical notes for writing/debugging Windows 7 (WDM/KMDF) drivers used by Aero (virtio, device bring-up, etc.).

## Index

- [Virtio PCI modern transport bring-up (Windows 7, WDM + INTx)](./virtio-pci-modern-wdm.md)
- [Windows 7 miniport guide: virtio-pci modern (NDIS/StorPort)](./win7-miniport-virtio-pci-modern.md)
- [Virtio PCI (modern) interrupts implementation guide (Windows 7, KMDF)](./virtio-pci-modern-interrupts.md)
- [Virtio PCI modern interrupt bring-up/debugging (Windows 7, MSI-X vs INTx)](./virtio-pci-modern-interrupt-debugging.md)
- [Virtio 1.0 split virtqueue implementation guide (Win7 KMDF)](../virtio/virtqueue-split-ring-win7.md)
- [Virtqueue DMA strategy (Windows 7 KMDF)](../windows-drivers/virtio/virtqueue-dma-strategy.md)
- [Windows 7 `virtio-snd` PortCls + WaveRT (render-only) driver design](./virtio-snd-portcls-wavert.md)
