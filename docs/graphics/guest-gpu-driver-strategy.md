# Guest GPU driver strategy (Windows 7): virtio-gpu reuse vs custom WDDM

## Status (current canonical direction)

This repository’s **canonical** Windows 7 graphics stack is the custom **AeroGPU** WDDM device +
driver package:

- PCI device/ABI contract: `PCI\\VEN_A3A0&DEV_0001` (see `docs/abi/aerogpu-pci-identity.md` and
  `drivers/aerogpu/protocol/*`)
- Canonical machine wiring: `crates/aero-machine` (`MachineConfig::enable_aerogpu=true` at `00:07.0`)
- In-tree Win7 driver package: `drivers/aerogpu/packaging/win7/`

The virtio-gpu reuse path described below is retained as **historical context / optional prototype**
work (for example, `crates/virtio-gpu-proto`). It is **not** the Windows driver binding contract
used by the canonical machine or Guest Tools.

## Goal

Choose the guest GPU driver path that gets to a **working Windows 7 desktop (and eventually Aero)** with the best leverage:

1. **Reuse an existing Windows virtio-gpu WDDM driver** (ideal if it exists, is signed, and is usable on Win7), or
2. **Build a custom “AeroGPU” WDDM driver stack** (highest control, highest cost).

This document focuses on what matters for Aero:

- Windows 7’s Desktop Window Manager (DWM) is built on **Direct3D 9Ex**.
- To get the “Aero Glass” experience, the guest must have a **WDDM driver path** that allows DWM’s D3D usage to function acceptably.
- In the browser, the host-side accelerated API is **WebGPU**, so the long-term plan is effectively “some guest GPU API → WebGPU”.

## Constraints / assumptions

- No proprietary Microsoft code can be copied.
- Shipping drivers is preferable only if licensing is permissive (MIT/Apache/BSD). If not permissive, the project should at minimum avoid embedding code and instead document how users obtain it themselves.
- Driver signing is a practical constraint:
  - **Using an already-signed third-party driver is a large time saver**.
  - A custom kernel-mode WDDM driver will require a signing strategy (test signing for dev; WHQL/attestation for distribution).

## Candidate Windows guest display driver options (survey)

This section is intentionally pragmatic: which drivers are “real” and installable on Windows 7 without heroic effort.

### WDDM version note (Windows 7)

- Windows 7 is fundamentally a **WDDM 1.1** operating system.
- Some modern “display-only driver” models (often abbreviated **DOD**) target newer WDDM versions and may not work on Win7.
- For Aero specifically, Win7 historically expects a functional WDDM driver path that allows DWM’s D3D9Ex usage to operate.

### 1) virtio-win “viogpu/viogpudo” (virtio-gpu)

**What it is:** A Windows display driver intended for the `virtio-gpu` / `virtio-vga` device family (PCI vendor `0x1af4`, device `0x1050`).

**Reality check:** The virtio-win ecosystem definitely ships signed storage/net/balloon drivers. GPU driver support exists in some virtio-win distributions, but **Windows 7 support and feature level are the key unknowns**:

- Some virtio-win GPU drivers are **display-only** (“DOD”) designs intended for newer Windows versions; Windows 7’s WDDM model is older and may not be compatible with a DOD-style driver.
- If the virtio-win GPU driver is not WDDM 1.0/1.1 compatible, it won’t enable Win7 Aero.

**Licensing:** virtio-win sources are open, but licenses vary by component. Before committing to shipping any binaries, verify the exact license for the GPU driver component (and whether binaries are redistributable).

**Device model requirements (2D scanout path):**

- virtio-pci transport (modern or transitional)
- `controlq` virtqueue for:
  - `GET_DISPLAY_INFO`
  - `RESOURCE_CREATE_2D`
  - `RESOURCE_ATTACH_BACKING`
  - `TRANSFER_TO_HOST_2D`
  - `SET_SCANOUT`
  - `RESOURCE_FLUSH`
- optional `cursorq` virtqueue for cursor plane (can be stubbed initially)

**How it maps to D3D9 → WebGPU:**

- If the driver is display-only, it likely does **not** provide a D3D9 acceleration path; it just presents a framebuffer.
- If it supports 3D, it will likely do so via an existing virtio-gpu 3D protocol (e.g. “virgl”-style). That does **not** naturally map to “D3D9 command stream → WebGPU”; it maps to a *different* 3D API surface that would also need translation.

### 2) SPICE/QXL WDDM driver (alternative reference point, not virtio-gpu)

**What it is:** The SPICE ecosystem historically shipped Windows QXL drivers, including WDDM variants that support Windows 7 in QEMU.

**Why it matters here:** Even if we choose virtio-gpu long-term, QXL is a useful comparator because it answers: “can an open driver get us to a Windows 7 desktop quickly?”

**Licensing:** open source, but often copyleft (verify exact terms). This may be acceptable as an optional user-provided driver, but is usually not ideal for direct inclusion in a permissively-licensed project.

**Device model requirements (high level):**

- QXL is *not* virtio; it has its own PCI device model and command/VRAM interfaces.
- Common expectations include:
  - a VRAM-like region for surfaces
  - a command ring / command queue with interrupts (“kick”/doorbell)
  - surface creation/destruction, blits, and cursor updates

**How it maps to D3D9 → WebGPU:** Similar to display-only: it’s fundamentally a 2D scanout/command approach, not a clean D3D9 command capture path.

### 3) VMware/VirtualBox WDDM drivers (not viable for Aero)

- VMware SVGA and VirtualBox guest additions provide working Aero in many VMs, but:
  - binaries are typically proprietary or under licenses not compatible with this project’s distribution goals.
  - the device models are complex and not tailored to “browser-hosted WebGPU”.

### 4) “Standard VGA/VBE”

- Windows built-in VGA/VBE paths are required for boot and early UI.
- They do **not** provide a WDDM path suitable for Aero.

## Option A: reuse virtio-gpu + existing Windows driver (recommended first leverage if Win7-compatible driver exists)

### Why it’s attractive

- **Time-to-first-desktop** is dominated by driver complexity, not device-model complexity.
- virtio-gpu 2D scanout is comparatively small and well-specified.
- If the Windows driver is **already signed**, it eliminates the single biggest real-world blocker for distributing a usable Win7 image.

### What we must build (host/device side)

At minimum for a basic desktop scanout:

1. virtio-pci plumbing (PCI config space, BARs/capabilities, MSI/MSI-X or INTx)
2. virtqueue implementation (descriptor walking, avail/used rings)
3. virtio-gpu controlq command processing:
   - `GET_DISPLAY_INFO` (reports one enabled scanout mode)
   - `RESOURCE_CREATE_2D` (BGRA8888)
   - `RESOURCE_ATTACH_BACKING` (guest memory backing)
   - `TRANSFER_TO_HOST_2D` (copy guest → host resource)
   - `SET_SCANOUT` (bind resource to scanout)
   - `RESOURCE_FLUSH` (present)
4. scanout to browser surface (Canvas/WebGPU texture upload)

### Fit with D3D9 → WebGPU translator

This path is excellent for “pixels on the screen”, but ambiguous for Aero acceleration:

- If the guest driver provides only a framebuffer, DWM composition may still fall back to software or a non-Aero theme.
- If the guest driver provides 3D, it likely does so through a protocol that is *not* “D3D9 command stream”.

**Net:** reuse virtio-gpu is the fastest route to a *working display*, but it does not guarantee the intended **D3D9→WebGPU** translation architecture will be usable without additional work.

## Option B: custom “AeroGPU” WDDM driver stack (highest control, highest risk)

### Why it’s attractive

- The **cleanest conceptual mapping** to the project’s end state:
  - D3D9/D3D9Ex work submitted by Windows → captured in a known command stream
  - Host translates those commands to WebGPU
- Avoids needing to implement/translate an intermediate 3D protocol (e.g. OpenGL/virgl).

### Why it’s risky / slow

- WDDM (Win7-era) requires a **kernel-mode miniport** + **user-mode display driver**, and for acceleration, the relevant 3D user-mode driver interfaces.
- Requires a **driver signing plan** (test signing for development; production signing to ship anything usable).
- Debugging kernel drivers inside an emulator in a browser is an extreme integration challenge.

### Device model requirements (high level)

A custom stack gets to define its own “hardware”, but the device model must still be implementable efficiently in the browser/WASM runtime:

- PCI display controller (or similar) for enumeration
- one or more BARs for:
  - control registers / doorbells
  - shared-memory command ring (or use virtio transport instead)
  - shared “VRAM” aperture or explicit resource upload/download commands
- interrupts (MSI/MSI-X preferred) for completion notification
- a well-specified, versioned command protocol (so the host can translate to WebGPU deterministically)

### Fit with D3D9 → WebGPU translator

Best possible fit, but only once the driver stack exists.

## Decision / recommended path

### At-a-glance comparison

| Path | Time to “desktop pixels” | Time to “Aero” | Biggest risk | Best leverage |
|------|--------------------------|----------------|--------------|---------------|
| Reuse virtio-gpu Windows driver | Low (if Win7-compatible driver exists) | Unclear | Win7/WDDM compatibility + limited 3D | Avoid writing WDDM KMD/UMD early; likely signed driver |
| Custom AeroGPU WDDM | Very high | Very high | WDDM complexity + signing | Perfect D3D9→WebGPU mapping if completed |

### Recommendation (current repo): custom AeroGPU WDDM is the canonical path

The repo now has an explicit, versioned **AeroGPU** ABI and an in-tree Win7 WDDM driver stack.
New work that targets the Windows 7 graphics path should generally build on that canonical contract.

The virtio-gpu reuse path can still be useful as a *separate* “pixels on screen” exploration, but it
should not be treated as the project’s Windows 7 acceleration plan or binding contract.

If you are working on virtio-gpu experiments, keep them clearly labeled as prototypes and avoid
mixing their IDs/contracts with the canonical AeroGPU device (`A3A0:0001`).

Keeping these efforts separate reduces risk:
- We can still get early “pixels on screen” feedback from a framebuffer-style virtio-gpu prototype
  without changing the Windows driver contract.
- We do not prematurely lock the acceleration architecture to virgl/OpenGL-like semantics if the
  long-term goal is D3D9→WebGPU via AeroGPU.

## Prototype (in this repo)

This repository includes a narrow virtio-gpu 2D command-processing prototype:

- `crates/virtio-gpu-proto`
  - Implements the control-queue subset needed for basic scanout:
    - `GET_DISPLAY_INFO`
    - `GET_EDID` (returns a minimal EDID blob)
    - `RESOURCE_CREATE_2D`
    - `RESOURCE_ATTACH_BACKING`
    - `TRANSFER_TO_HOST_2D`
    - `SET_SCANOUT`
    - `RESOURCE_FLUSH`
  - Supports multi-entry (scatter/gather) backing and a small set of 32-bit formats (BGRA/BGRX variants).
  - Validation test:
  - `cargo test --locked -p virtio-gpu-proto`
  - `basic_2d_scanout_roundtrip` simulates a guest writing a BGRA framebuffer in “guest memory”, transferring it to the device, and flushing to scanout; the resulting scanout buffer is byte-for-byte verified.
  - Proof snippet: [virtio-gpu-proto-proof.md](./virtio-gpu-proto-proof.md)

In addition, there is an end-to-end virtqueue/virtio-pci integration test:

- `crates/aero-virtio/src/devices/gpu.rs`
  - Wraps `virtio-gpu-proto` in the project’s virtio-pci + split-virtqueue transport (`aero-virtio`).
  - Intended as the “device model hooks” starting point for wiring into the emulator.
- `crates/aero-virtio/tests/virtio_gpu.rs`
  - `cargo test --locked -p aero-virtio virtio_gpu_2d_scanout_via_virtqueue`
  - Exercises the full controlq sequence through virtqueues and verifies scanout bytes after flush.

This prototype is not a Windows driver and does not claim Win7 driver compatibility; it is a **device-model foundation** that can be wired into the emulator once PCI/virtqueue infrastructure exists.

## Win7 validation checklist (manual, once integrated)

When the emulator has PCI + virtio-pci + scanout wiring, validate the virtio-gpu reuse path in a real Windows 7 guest:

1. Boot Windows 7 with VGA/VBE first (baseline display must work).
2. Expose a `virtio-gpu` PCI function (vendor `0x1af4`, device `0x1050`) and confirm it enumerates in Device Manager.
3. Install the candidate virtio-gpu Windows driver (virtio-win package) and confirm:
   - the driver binds to the device (no Code 12/Code 28/Code 43)
   - the display switches to the virtio-gpu adapter
   - resolution changes work (at least a fixed 1024×768 mode)
4. Confirm DWM/Aero behavior:
   - whether Aero can be enabled
   - whether D3D9Ex applications create devices successfully

If step (3) fails due to driver support gaps (or step (4) fails due to lack of acceleration), treat virtio-gpu as a “bring-up display” only and continue with the acceleration-specific plan (custom command stream / custom WDDM).

## Risks / unknowns to resolve next

1. **Does a signed virtio-gpu WDDM driver actually support Windows 7 Aero?**
   - If not, virtio-gpu still helps for early pixels, but a separate plan is required for Aero.
2. **Driver licensing / redistribution**
   - Confirm the license for any virtio-gpu Windows driver we plan to ship or bundle.
3. **3D protocol choice**
   - If using virtio-gpu 3D, decide whether to translate virgl/gfxstream → WebGPU (large) vs building a custom command stream (also large).
4. **Driver signing strategy for any custom WDDM work**
   - Without a credible signing plan, “custom WDDM” remains a research topic rather than a shipping path.
