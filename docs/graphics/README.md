# Graphics docs index

This directory contains “graphics stack” implementation notes and bring-up checklists for the AeroGPU project, mostly focused on Windows 7 (WDDM 1.1) and DirectX translation.

## Windows 7 / AeroGPU driver stack

- `aerogpu-protocols.md` — overview of the different “AeroGPU” ABIs in-tree and which one is the Win7/WDDM target.
- `win7-wddm11-aerogpu-driver.md` — WDDM 1.1 KMD+UMD architecture (adapter bring-up, memory model, submission rings, fences/interrupts, scanout/vblank).
- `win7-aerogpu-validation.md` — Win7 driver stability checklist (TDR avoidance, vblank pacing expectations, debug playbook).
- `win7-vblank-present-requirements.md` — minimal vblank/present timing contract needed to keep DWM (Aero) stable.

## User-mode driver (UMD) API surfaces

- `win7-d3d9ex-umd-minimal.md` — minimal D3D9Ex UMD/DDI surface for DWM + basic D3D9 apps.
- `win7-d3d10-11-umd-minimal.md` — minimal D3D10 + D3D11 UMD/DDI surface (SM4/SM5) plus DXGI swapchain expectations (targeting FL10_0 bring-up, roadmap to FL11_0).

## Strategy / prototypes

- `guest-gpu-driver-strategy.md` — options for Windows guest GPU drivers (virtio reuse vs custom WDDM).
- `virtio-gpu-proto-proof.md` — early prototype notes for virtio-style GPU paths.
