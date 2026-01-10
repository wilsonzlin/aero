# Optional GPU fast path (Windows 7 WDDM 1.1) — design notes

This document is intentionally high-level: implementing a production WDDM driver stack is a large project.

## Goal

Provide a paravirtual “Aero GPU” device and driver stack that:

1. Presents a WDDM 1.1 display adapter to Windows 7.
2. Accepts Direct3D/DirectX workloads via the normal Windows graphics stack.
3. Intercepts/translates GPU command streams to the browser-side renderer (WebGPU) efficiently.

The in-tree Win7 driver stack lives under `drivers/aerogpu/` (start at `drivers/aerogpu/README.md`).

## Practical approach (staged)

### Stage 0: Basic display

- WDDM 1.1 display-only driver (DOD) to get a modern desktop resolution without SVGA hacks.
- Focus: modesetting + framebuffer present.

### Stage 1: Command transport

- Introduce a paravirtual command channel (PCI BAR or virtqueue) between guest and emulator.
- Add a UMD that marshals a simplified command stream.
- Emulator translates to WebGPU.

### Stage 2: D3D9/DXGI translation path

- Provide a D3D9 UMD (and optionally DXGI/D3D10/11) that translates guest API calls into a stable intermediate protocol.
- Maintain strict versioning of the protocol (backwards compatible when possible).

## Notes / constraints

- Windows 7 is WDDM 1.1. The driver interfaces differ significantly from newer WDDM versions.
- A production-quality graphics driver requires:
  - robust memory management
  - synchronization primitives and fence handling
  - recovery/reset paths
  - extensive testing across apps and the DWM compositor

For now, virtio is the priority path for “system usability” (storage/network/input/audio). The GPU fast path becomes valuable once the emulator can run 3D apps.
