<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->

# SOURCES (virtio-snd Windows 7 driver)

This file tracks the specifications and public references used to design and
implement the Windows 7 `virtio-snd` driver under `drivers/windows7/virtio-snd/`.

It exists to satisfy the clean-room/source tracking policy in
`drivers/windows7/LEGAL.md` §2.6.

## Specifications (virtio)

Virtio is the normative source for device layout, feature bits, queue formats, and
request/response structures.

- **OASIS Virtio Specification (Virtio 1.x / Virtio 1.0+ transport)** — particularly:
  - *Sound Device* (`virtio-snd`): PCM control requests, PCM data queues, stream and
    format structures.
  - *Virtio over PCI Bus* (`virtio-pci` modern transport): PCI capability discovery,
    common configuration, notification mechanism, and interrupt routing.
  - *Split Virtqueues*: descriptor table, available ring, used ring, and memory
    ordering requirements.
  - URL: https://docs.oasis-open.org/virtio/ (select the specific 1.x revision used by
    the project/emulator)

## Aero-specific contract

Aero constrains virtio to a small, testable subset. The definitive contract is:

- `docs/windows7-virtio-driver-contract.md`
  - §1: virtio-pci modern transport subset
  - §2: split-ring virtqueue subset
  - §3.4: virtio-snd device contract (queue layout, minimum feature set, minimal PCM)

## In-repo implementation guides consulted

- Virtqueue split-ring implementation guide (Windows 7): `docs/virtio/virtqueue-split-ring-win7.md`
- virtio-pci modern interrupts on Windows 7 (MSI-X vs INTx): `docs/windows/virtio-pci-modern-interrupts.md`

## Microsoft public documentation (PortCls / WaveRT)

The driver’s Windows-facing behavior is derived from publicly available WDK
documentation (Microsoft Learn). No text or code is copied into Aero; these pages
are used to understand required interfaces and call sequences.

- PortCls system driver overview:  
  https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/portcls-system-driver
- Port Class (PortCls) audio drivers:  
  https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/port-class-audio-drivers
- WaveRT (PortCls + miniport) overview:  
  https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/wavert-portcls-and-minport-drivers
- WaveRT miniport drivers:  
  https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/wavert-miniport-drivers
- WDK DDI reference (IMiniportWaveRT / IMiniportWaveRTStream, etc):  
  https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/
- Kernel Streaming overview (KSSTATE_* and related concepts):  
  https://learn.microsoft.com/en-us/windows-hardware/drivers/stream/kernel-streaming
- KeQueryPerformanceCounter (QPC timebase for a virtual playback clock):  
  https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-kequeryperformancecounter

## Samples and third-party code

- **Code copied from external samples/repositories:** none.
- **Permissively licensed sample repositories consulted (reference-only):** none so far.

If, in the future, permissively licensed samples are consulted (for example the
MIT-licensed `sysvad` sample in `microsoft/Windows-driver-samples`), record the
exact upstream URL + revision/date here and ensure any required notices are preserved.

### In-repo shared code reused by this driver
 
The virtio-snd driver is linked against in-repo virtio support code (no copying;
the WDK build pulls these sources directly). Key paths:
 
- `drivers/windows/virtio/common/virtqueue_split.c` (plus headers in that directory)
- `drivers/win7/virtio/virtio-core/portable/virtio_pci_cap_parser.c`
  (plus `virtio_pci_cap_parser.h`)
- `drivers/win7/virtio/virtio-core/portable/virtio_pci_identity.c`
  (plus `virtio_pci_identity.h`)
- `drivers/win7/virtio/virtio-core/include/virtio_pci_modern.h`
  (plus other `virtio-core/include` headers used for spec constants/layouts)
- `drivers/windows7/virtio/common/src/virtio_pci_contract.c`
- `drivers/windows7/virtio/common/include/virtqueue_split.h`
  (SG entry definitions used by `virtiosnd_sg_core`; this is independent of the
  `drivers/windows/virtio/common` split-virtqueue implementation linked into the driver)
