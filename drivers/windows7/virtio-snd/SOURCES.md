# SOURCES (virtio-snd Windows 7 driver)

This file tracks the specifications and public references used to design and
implement the Windows 7 `virtio-snd` driver under `drivers/windows7/virtio-snd/`.

It exists to satisfy the clean-room/source tracking policy in
`drivers/windows7/LEGAL.md`.

## Specifications

Virtio is the normative source for device layout, feature bits, and request
formats.

- **OASIS Virtio Specification (1.x)** — particularly:
  - *Sound Device* (`virtio-snd`): PCM control requests, PCM data queues, stream
    and format structures.
  - *Virtio over PCI Bus* (`virtio-pci` modern transport): PCI capability
    discovery, common configuration, notification mechanism, MSI-X usage.
  - *Split Virtqueues*: descriptor table, available ring, used ring, and memory
    ordering requirements.
  - URL: https://docs.oasis-open.org/virtio/ (choose the specific 1.x revision
    used by the project/emulator)

## Microsoft public documentation (PortCls / WaveRT)

The driver’s Windows-facing behavior is derived from publicly available WDK
documentation (MS Learn). No text or code is copied into Aero; these pages are
used to understand required interfaces and call sequences.

- **Port Class (PortCls) Audio Drivers**  
  https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/port-class-audio-drivers
- **WaveRT Miniport Drivers**  
  https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/wavert-miniport-drivers
- **IMiniportWaveRT / IMiniportWaveRTStream** interface documentation  
  https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ (search for the interface names)
- **Kernel Streaming (KS) states (KSSTATE\_*)**  
  https://learn.microsoft.com/en-us/windows-hardware/drivers/stream/kernel-streaming
- **KeQueryPerformanceCounter** (QPC timebase for virtual playback clock)  
  https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-kequeryperformancecounter

## Samples and third-party code

- **Code copied from samples:** none.
- **MIT-licensed samples consulted while writing Aero code:** none.

If, in the future, permissively licensed samples are consulted (for example the
MIT-licensed `sysvad` sample in `microsoft/Windows-driver-samples`), record the
exact upstream URL + revision here and ensure any required notices are preserved.

