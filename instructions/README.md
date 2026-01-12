# Workstream Instructions

This directory contains the onboarding/task instructions for each parallel development
workstream. Start with [`AGENTS.md`](../AGENTS.md) (operational guidance + interface
contracts), then choose the workstream you are working on:

| Workstream | File | Focus |
|------------|------|-------|
| **A: CPU/JIT** | [`cpu-jit.md`](./cpu-jit.md) | CPU emulation, decoder, JIT, memory |
| **B: Graphics** | [`graphics.md`](./graphics.md) | VGA, DirectX 9/10/11, WebGPU |
| **C: Windows Drivers** | [`windows-drivers.md`](./windows-drivers.md) | AeroGPU, virtio drivers |
| **D: Storage** | [`io-storage.md`](./io-storage.md) | AHCI, NVMe, OPFS, streaming |
| **E: Network** | [`network.md`](./network.md) | E1000, L2 proxy, TCP/UDP |
| **F: USB/Input** | [`usb-input.md`](./usb-input.md) | PS/2, USB HID, keyboard/mouse |
| **G: Audio** | [`audio.md`](./audio.md) | HD Audio, AudioWorklet |
| **H: Integration** | [`integration.md`](./integration.md) | BIOS, ACPI, PCI, boot |

For cross-workstream planning and a higher-level roadmap, see:

- [`docs/15-agent-task-breakdown.md`](../docs/15-agent-task-breakdown.md)
- [`docs/01-architecture-overview.md`](../docs/01-architecture-overview.md)
