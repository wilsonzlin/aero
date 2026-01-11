# AeroGPU Command Protocol (toy/prototype surface ABI, v0.1) — DEPRECATED

> **DEPRECATED / PROTOTYPE ABI**
>
> This file previously documented a **surface-centric** AeroGPU command protocol
> (`CREATE_SURFACE` / `UPDATE_SURFACE` / `PRESENT`) used during early bring-up by
> the legacy prototype device model in `crates/aero-emulator/src/devices/aerogpu/`.
> That prototype is now removed or feature-gated; this is **not** the Windows 7
> WDDM AeroGPU ABI.
>
> It also used stale placeholder PCI IDs (deprecated vendor `VEN_1AE0`) and must
> not be treated as a Windows driver contract. See
> `docs/abi/aerogpu-pci-identity.md`.
>
> **Canonical (supported) Win7/WDDM AeroGPU ABI:**
> - `drivers/aerogpu/protocol/README.md`
> - `drivers/aerogpu/protocol/aerogpu_pci.h`
> - `drivers/aerogpu/protocol/aerogpu_ring.h`
> - `drivers/aerogpu/protocol/aerogpu_cmd.h`
> - `docs/graphics/aerogpu-protocols.md` (overview)
>
> The archived prototype spec lives at
> `docs/legacy/aerogpu-prototype-command-protocol.md`.
