# Win7 WDDM 1.1 “Minimal Viable” Driver Architecture for AeroGPU
 
**Target guest OS:** Windows 7 SP1 (x86 + x64)  
**Driver model:** WDDM 1.1  
**GPU model:** AeroGPU (virtual PCI device implemented by the Aero emulator)  
**Goal:** Provide a minimal-but-real WDDM stack (KMD + UMD) that enables DWM/Aero and D3D9 applications, with commands executed by the emulator (ultimately translated to WebGPU on the host).
 
---
 
## 0. Executive summary
  
We will implement a **WDDM 1.1 display miniport driver (KMD)** plus a **D3D9Ex user-mode display driver (UMD)** that together provide:
 
1. **Display bring-up + modesetting** (single monitor, fixed EDID).
2. **A simple memory model** (system-memory-only allocations; no dedicated VRAM; no hardware paging).
3. **A “command transport boundary”** between the guest driver and the emulator:
   - Virtual PCI device
   - Small MMIO register block
   - Shared submission ring(s) in guest physical memory
   - Fence + interrupt completion
4. **A present/scanout path** that keeps Windows 7 DWM stable (vblank simulation, SetVidPnSourceAddress-driven scanout).
 
The key architectural choice is to **avoid implementing a traditional, hardware-specific DMA instruction stream**. Instead, the UMD emits an **AeroGPU-specific command stream** (an IR) that the emulator consumes and translates to the host graphics API (WebGPU).
 
This doc is the implementation spec for the Win7 driver stack. It intentionally focuses on the minimal surface area needed to achieve DWM + D3D9 stability before expanding into D3D10/11.

Implementation + build tooling lives under `drivers/aerogpu/` (start at `drivers/aerogpu/README.md`).
  
---
 
## 1. Why WDDM 1.1 (Win7) + what minimal success looks like
 
### 1.1 Why WDDM 1.1?
 
Windows 7’s modern desktop experience (Aero glass, DWM composition, most GPU-accelerated UI) assumes a **WDDM** driver. You *can* “boot to desktop” with legacy VGA/VESA paths, but you cannot get a practical Windows 7 UX without a WDDM driver that supports D3D9Ex.
 
WDDM 1.1 is the native driver model for Windows 7 SP1:
 
- DWM uses **D3D9Ex** and expects a functional WDDM KMD+UMD pair.
- WDDM provides a standardized memory + scheduling contract (VidMM + scheduler).
 
### 1.2 Minimal success levels (phased)
 
We define success in three tiers (each tier must remain stable over time):
 
1. **Tier A — “Boot to desktop”**
   - Windows reaches desktop with a usable display.
   - This can be achieved with VGA/SVGA, but for the AeroGPU driver it means:
     - AeroGPU KMD loads and binds
     - Basic modeset works
     - No Code 43 / no TDR loop
 
2. **Tier B — “Aero/DWM enabled” (MVP target)**
   - Desktop Window Manager (DWM) runs with composition enabled.
   - Theme switching to “Windows 7” enables Aero (glass effects may be simplified initially).
   - Window animations and resizing do not hang or trigger TDR.
 
3. **Tier C — “D3D apps” (MVP target for graphics stack)**
   - A simple D3D9 app renders and presents (e.g., spinning triangle).
   - Presentation is stable at a fixed refresh rate.
   - dxdiag reports Direct3D acceleration enabled (as appropriate for reported caps).
 
**Not MVP:** D3D10/11, multi-monitor, advanced power mgmt, full DXVA/video decode, OpenGL ICD.
 
---
 
## 2. Driver package components
 
### 2.1 Kernel-mode driver (KMD)
 
**Binary:** `aerogpu.sys`  
**Type:** WDDM display miniport driver (kernel-mode)  
**Responsibilities:** Adapter bring-up, VidPN/modeset, memory segments/allocations, command submission to the emulator, interrupt handling, TDR reset.
 
### 2.2 User-mode drivers (UMDs)
 
We ship UMDs as separate DLLs because Windows loads them per-API/runtime.
 
 **Phase 1 (MVP):**
  
 - **D3D9Ex UMD**
  - 64-bit: `aerogpu_d3d9_x64.dll` (loaded by 64-bit apps on x64)
  - 32-bit: `aerogpu_d3d9.dll` (loaded by 32-bit apps under WOW64 on x64, and as primary on x86)
 
**Later phases:**

- D3D10/10.1 UMD (DXGI + D3D10 runtime integration)
- D3D11 UMD (if/when we implement DXGI 1.1 path and D3D11 DDI)

For a minimal D3D10/D3D11 UMD bring-up checklist (DDI entrypoints, FL10_0 target, DXGI swapchain/present expectations), see:

- `docs/graphics/win7-d3d10-11-umd-minimal.md`
 
### 2.3 INF + packaging
 
 We ship a standard display driver package:
 
 - `aerogpu.inf` — device installation + registry configuration
 - `aerogpu.cat` — signed catalog
 - `aerogpu.sys` — KMD
 - `aerogpu_d3d9_x64.dll` / `aerogpu_d3d9.dll` — D3D9 UMDs (x64 + WOW64/x86)
  
 **INF essentials (Win7 WDDM):**
 
- Device is class `Display` (`{4d36e968-e325-11ce-bfc1-08002be10318}`)
- Bind by PCI vendor/device ID (AeroGPU’s virtual PCI IDs)
- Register UMD(s) via the expected OpenGL/D3D registry keys for WDDM 1.1 (exact key names are per-WDK docs and must match the D3D9 runtime’s expectations).
 
**Clean-room note:** do not copy an existing vendor INF. Use the WDK documentation to build the required sections from scratch.
 
---
 
## 3. High-level architecture (guest ↔ emulator boundary)
 
### 3.1 Control/data path diagram
 
```
App/DWM
  │  D3D9Ex
  ▼
Microsoft D3D9 runtime (user-mode)
  │  D3DDDI calls
  ▼
AeroGPU D3D9 UMD (aerogpu_d3d9*.dll)
  │  builds AeroGPU command stream + allocation list
  │  uses D3DKMT thunk (user→kernel)
  ▼
dxgkrnl.sys / dxgmms1.sys (VidMM + scheduler)
  │  calls DxgkDdi* entrypoints
  ▼
AeroGPU KMD (aerogpu.sys)
  │  writes submission descriptors to shared ring
  │  pokes MMIO doorbells / scanout regs
  ▼
┌──────────────────────────────────────────────────────────────┐
│                 Emulator device model (AeroGPU)              │
│  PCI config + MMIO regs + shared rings in guest phys memory  │
│  Executes command stream and translates to host WebGPU        │
└──────────────────────────────────────────────────────────────┘
  │  fence complete / vblank interrupts
  ▼
KMD interrupt → dxgkrnl → UMD fences complete
```
 
### 3.2 Why an AeroGPU-specific command stream?
 
Traditional GPU drivers emit a hardware ISA-like DMA stream and rely on complex:
 
- patch location lists (relocations)
 
For AeroGPU we control both sides (guest driver + emulator). The MVP should:
  
- Keep the KMD thin (mostly plumbing + bookkeeping)
- Keep the UMD as the main “translator” from D3D9 state to an emulator-friendly IR
- Avoid patch lists by using **stable allocation IDs (`alloc_id`)** (see §5)
 
---

## 3.3 UMD architecture (D3D9Ex MVP)

### Responsibilities

The D3D9Ex UMD is responsible for translating the Microsoft D3D9 runtime’s DDI calls into:

1. **Kernel allocations** (via `D3DKMTCreateAllocation` / `D3DKMTCreateContext` as routed through the runtime)
2. **AeroGPU command streams** suitable for execution by the emulator
3. **Accurate capability reporting** to keep DWM and typical D3D9 apps on supported code paths

### UMD execution model

- The UMD maintains:
  - A per-device command buffer builder
  - A small state cache (current shaders, render targets, blend state, etc.)
  - A resource table mapping runtime handles to `alloc_id`/metadata
- On each draw/dispatch boundary, the UMD appends commands to a DMA buffer that is ultimately submitted through dxgkrnl to the KMD.
- The UMD **must** be able to run under:
  - 32-bit (Windows 7 x86, and WOW64 on x64)
  - 64-bit (Windows 7 x64)

### Capabilities: what we claim for MVP

To get DWM compositing and basic D3D9 apps running, the UMD should expose a conservative, minimal set of caps:

- Shader model: at least `vs_2_0` / `ps_2_0` equivalents (DWM heavily relies on pixel shaders)
- Render target formats: at least `X8R8G8B8` and `A8R8G8B8`
- Textures: 2D textures in common formats; no volume textures required for DWM
- Depth/stencil: optional for DWM; required for many apps (can be introduced after triangle test passes)

The UMD should explicitly *not* advertise features it cannot execute correctly (to avoid runtime selecting unsupported paths that later hang/TDR).

## 4. KMD responsibilities and required DxgkDdi entrypoints (Win7 WDDM 1.1)
 
This section lists the **minimum DxgkDdi callbacks** we will implement for a working WDDM 1.1 driver on Windows 7, grouped by responsibility.
 
> **Implementation rule:** The exact prototype and “required vs optional” status must be validated against **WDK 7.1** headers (`d3dkmddi.h` / `dispmprt.h`) during implementation. The list below is the architectural contract we will target.
 
For each entrypoint:
 
- **Purpose**: why Windows calls it
- **AeroGPU MVP behavior**: what we do now
- **Can be deferred**: what we intentionally don’t implement in MVP (while still returning stable results)
 
### 4.1 Adapter bring-up / PnP lifecycle

#### `DxgkDdiAddDevice`
 
- **Purpose:** Create per-adapter context when PnP enumerates the PCI device.
- **AeroGPU MVP behavior:** Allocate/initialize `AEROGPU_ADAPTER` object; store PDO; prepare for resource mapping in StartDevice.
- **Can be deferred:** Nothing major. Keep minimal allocations; don’t start hardware here.
 
#### `DxgkDdiStartDevice`
 
- **Purpose:** Map BARs/interrupts; publish adapter caps; ready to service Dxgk callbacks.
- **AeroGPU MVP behavior:**
  - Map AeroGPU MMIO BAR (register block).
  - Configure interrupt vector.
  - Initialize submission rings (allocate shared pages; write ring base to MMIO).
  - Initialize default mode set state (single output).
- **Can be deferred:** Advanced power states, multiple nodes/engines, MSI/MSI-X (use line-based if simplest).
 
#### `DxgkDdiStopDevice`
 
- **Purpose:** Stop hardware access during PnP stop/remove.
- **AeroGPU MVP behavior:** Disable interrupts, stop vblank timer (in emulator), free ring allocations, unmap MMIO.
- **Can be deferred:** Sophisticated draining; MVP may force-reset the virtual GPU.
 
#### `DxgkDdiRemoveDevice`
 
- **Purpose:** Destroy per-adapter allocations after PnP removal.
- **AeroGPU MVP behavior:** Free `AEROGPU_ADAPTER`.
- **Can be deferred:** N/A.
 
#### `DxgkDdiQueryAdapterInfo`
 
- **Purpose:** dxgkrnl queries caps, memory segments, and other adapter metadata.
- **AeroGPU MVP behavior:**
  - Report **one system-memory segment** (see §5).
  - Report **one VidPN source** and **one target**.
  - Report scheduling/preemption caps as “minimal” (single queue, no preemption beyond DMA buffer boundaries).
- **Can be deferred:** Dedicated VRAM segments, aperture segments, CPU/GPU sync optimizations.
 
#### `DxgkDdiQueryInterface`
 
- **Purpose:** Provide dxgkrnl with interface pointers used for callbacks/coordination.
- **AeroGPU MVP behavior:** Return only the supported/required interfaces for WDDM 1.1; keep versioning strict.
- **Can be deferred:** Optional interfaces not required for MVP.

#### `DxgkDdiSetPowerState`

- **Purpose:** Handle device power transitions (Dx, Sx).
- **AeroGPU MVP behavior:** Support the minimal path required for boot/shutdown:
  - Treat `D0` as “on”, everything else as “off”
  - On power-down: stop interrupts, stop vsync generation
  - On power-up: re-init MMIO/rings (or full virtual reset)
- **Can be deferred:** Fine-grained Dx states, fast resume, power budgeting.

#### `DxgkDdiDispatchIoRequest` (optional but useful)
 
- **Purpose:** Handle private IOCTLs (escape, diagnostics).
- **AeroGPU MVP behavior:** Implement only what we need:
  - `DXGK_ESCAPE` passthrough for debug channels (optional).
  - Otherwise return `STATUS_NOT_SUPPORTED`.
- **Can be deferred:** Full debug tooling.

#### `DxgkDdiEscape` (optional; useful for bring-up)

- **Purpose:** Kernel escape channel used by user-mode (`D3DKMTEscape`) for private queries/commands.
- **AeroGPU MVP behavior:** Support a tiny set of escapes for diagnostics:
  - Query driver/device version
  - Dump ring/fence state (debug builds)
  - Force virtual GPU reset (debug builds)
- **Can be deferred:** Any production control panel / extensive escape surface.

### 4.2 VidPN / modesetting (single monitor)

#### `DxgkDdiQueryChildRelations`

- **Purpose:** Enumerate display “children” (connectors/monitors).
- **AeroGPU MVP behavior:** Report exactly one child corresponding to the single virtual monitor.
- **Can be deferred:** Multiple connectors/monitors.

#### `DxgkDdiQueryChildStatus`

- **Purpose:** Report connection status, rotation/orientation changes, etc.
- **AeroGPU MVP behavior:** Always report “connected/present” for the single monitor (unless emulator explicitly disables output).
- **Can be deferred:** Hotplug/unplug events.

#### `DxgkDdiRecommendFunctionalVidPn`
 
- **Purpose:** Provide a baseline VidPN topology/mode set.
- **AeroGPU MVP behavior:** Recommend a single path: source 0 → target 0, with a preferred mode (e.g., 1024×768@60, and optionally 1280×720@60, 1366×768@60).
- **Can be deferred:** Complex mode pruning, rotation, scaling.
 
#### `DxgkDdiEnumVidPnCofuncModality`
 
- **Purpose:** Validate/enumerate compatible modes/topologies for a given VidPN.
- **AeroGPU MVP behavior:** Accept only:
  - 1 source, 1 target
  - Progressive scan
  - A small whitelist of modes and pixel formats
- **Can be deferred:** Interlaced, custom timings, multiple paths.
 
#### `DxgkDdiIsSupportedVidPn`
 
- **Purpose:** Quick check if a VidPN proposal is supported.
- **AeroGPU MVP behavior:** Return supported iff it matches the MVP constraints above.
- **Can be deferred:** None (keep strict).
 
#### `DxgkDdiCommitVidPn`
 
- **Purpose:** Apply a modeset (resolution/format) to hardware.
- **AeroGPU MVP behavior:**
  - Save current mode in adapter state.
  - Program emulator-facing MMIO regs (mode width/height/stride/format).
  - Ensure a scanout allocation is set (see `SetVidPnSourceAddress`).
- **Can be deferred:** Seamless mode transitions, panning, color calibration.
 
#### `DxgkDdiUpdateActiveVidPnPresentPath`
 
- **Purpose:** Update path parameters without a full modeset.
- **AeroGPU MVP behavior:** Treat as a no-op if it doesn’t change the single-path invariants; otherwise return not supported.
- **Can be deferred:** Dynamic scaling/rotation.
 
#### `DxgkDdiSetVidPnSourceAddress`
 
- **Purpose:** Point scanout at a given primary surface allocation (flip).
- **AeroGPU MVP behavior:**
  - Extract the allocation’s guest-physical backing (PFNs).
  - Write scanout base address + pitch + format to AeroGPU MMIO.
  - This becomes the source the emulator displays to the host canvas.
- **Can be deferred:** Multi-plane overlay, stereo, rotation.
 
#### `DxgkDdiSetVidPnSourceVisibility`
 
- **Purpose:** Enable/disable scanout for a source (blanking).
- **AeroGPU MVP behavior:** Toggle a `VISIBLE` bit in MMIO; emulator will present black when not visible.
- **Can be deferred:** DPMS, advanced power gating.
 
#### `DxgkDdiQueryVidPnHardwareCapability`
 
- **Purpose:** Report capabilities like scaling/rotation support.
- **AeroGPU MVP behavior:** Report minimal: no rotation, no scaling, no overlays.
- **Can be deferred:** Everything beyond single-path scanout.
 
#### `DxgkDdiRecommendMonitorModes` + `DxgkDdiQueryDeviceDescriptor`
 
- **Purpose:** Provide EDID/mode list for the virtual monitor.
- **AeroGPU MVP behavior:**
  - Provide a fixed EDID blob (or a simplified descriptor) matching our supported modes.
  - Provide a small stable mode list; keep it deterministic to reduce OS edge-cases.
- **Can be deferred:** Real EDID parsing, hotplug events.

#### `DxgkDdiStopDeviceAndReleasePostDisplayOwnership` / `DxgkDdiAcquirePostDisplayOwnership` (if required)

- **Purpose:** Hand off display ownership during boot/bugcheck/transition scenarios.
- **AeroGPU MVP behavior:**
  - Implement conservative handoff: blank display or keep last scanout, depending on what dxgkrnl expects.
  - Prefer a full virtual reset on reacquire to avoid stale scanout pointers.
- **Can be deferred:** Seamless transitions.

#### `DxgkDdiGetScanLine` + `DxgkDdiControlInterrupt`
 
- **Purpose:** Support vblank/vsync timing (DWM stability) and enable/disable interrupts.
- **AeroGPU MVP behavior:**
  - `ControlInterrupt`: gate whether the emulator generates vsync interrupts.
  - `GetScanLine`: return a simulated scanline based on a host timer (or return “in vblank”).
- **Can be deferred:** Accurate scanline emulation.
 
### 4.3 Memory segments and allocations (system-memory-only MVP)
 
#### `DxgkDdiCreateDevice` / `DxgkDdiDestroyDevice`
 
- **Purpose:** Create/destroy per-graphics-device objects used by the scheduler.
- **AeroGPU MVP behavior:** Minimal bookkeeping object; associate with adapter; allocate per-device submission state if needed.
- **Can be deferred:** Multi-engine, per-process isolation beyond what dxgkrnl provides.
 
#### `DxgkDdiCreateAllocation`
 
- **Purpose:** Create GPU allocations (textures, render targets, vertex buffers, command buffers, etc).
- **AeroGPU MVP behavior:**
  - Create allocations backed by **locked system memory pages** (nonpaged) so the emulator can safely read them.
  - Store:
    - allocation size, format, pitch (if surface)
    - PFN array / guest physical addresses
    - usage hints (render target vs texture vs buffer)
- **Can be deferred:** Dedicated VRAM placement, compression/tiling, swizzling.
 
#### `DxgkDdiDestroyAllocation`
 
- **Purpose:** Free allocation backing.
- **AeroGPU MVP behavior:** Free locked pages and allocation metadata.
- **Can be deferred:** Deferred destruction queues.
 
#### `DxgkDdiDescribeAllocation` / `DxgkDdiGetStandardAllocationDriverData` (as needed)
 
- **Purpose:** Let dxgkrnl understand allocation properties (primary, shadow, etc).
- **AeroGPU MVP behavior:** Provide only what’s required for primary surfaces and generic allocations; keep flags conservative.
- **Can be deferred:** Specialized standard allocations (stereo, overlays).
 
#### `DxgkDdiOpenAllocation` / `DxgkDdiCloseAllocation`
 
- **Purpose:** Share/open allocations across processes/devices.
- **AeroGPU MVP behavior:** Support the minimal cases required by D3D9Ex + DWM; otherwise fail gracefully.
- **Can be deferred:** General cross-process resource sharing.
 
#### `DxgkDdiLock` / `DxgkDdiUnlock`
 
- **Purpose:** Map/unmap allocation memory for CPU access.
- **AeroGPU MVP behavior:** Since allocations are system-memory-backed, lock returns a CPU VA; unlock is a no-op besides bookkeeping.
- **Can be deferred:** Cache management, write-combined mappings.
 
#### `DxgkDdiBuildPagingBuffer` (MVP: “no paging”)
 
- **Purpose:** VidMM requests driver to build DMA buffers to page/move allocations.
- **AeroGPU MVP behavior:**
  - Advertise no dedicated VRAM, so most paging operations become unnecessary.
  - Implement as a *validated no-op* for operations that target our system segment.
  - If asked to move between segments we don’t expose, return `STATUS_NOT_SUPPORTED`.
- **Can be deferred:** Any real paging/copy engine support.
 
### 4.4 Render + present submission
 
> Terminology: dxgkrnl submits “DMA buffers” produced by the UMD/runtime. For AeroGPU these buffers contain an **AeroGPU command stream**, not a vendor ISA.
 
#### `DxgkDdiCreateContext` / `DxgkDdiDestroyContext`
 
- **Purpose:** Create per-context state used for command submission/scheduling.
- **AeroGPU MVP behavior:** Allocate a `AEROGPU_CONTEXT` holding:
  - context ID
  - last submitted fence
  - lightweight state for debugging/validation
- **Can be deferred:** Context priority, preemption granularity, virtualization.
 
#### `DxgkDdiRender` (or `DxgkDdiSubmitCommand` depending on WDK 7.1 DDI)
 
- **Purpose:** Submit a command buffer plus its referenced allocations to the GPU.
- **AeroGPU MVP behavior:**
   1. Validate the submission (bounds, known opcodes, allocation list sizes).
   2. Build a **sideband allocation table** for the emulator (optional but recommended; see `drivers/aerogpu/protocol/aerogpu_ring.h`):
      - `alloc_id` → {guest physical base address, size_bytes, flags}
   3. Write a submission descriptor into the shared ring and ring the doorbell.
   4. Return a fence ID to dxgkrnl.
- **Can be deferred:** Patch-location processing (we design the command stream to avoid it), hardware scheduling, multiple queues.

#### `DxgkDdiPreemptCommand` / `DxgkDdiCancelCommand` (if required by the scheduler)

- **Purpose:** Allow dxgkrnl to preempt/cancel in-flight submissions.
- **AeroGPU MVP behavior:**
  - Define preemption granularity as “DMA buffer boundary”.
  - If asked to preempt a buffer that hasn’t started, remove it from the ring (if still queued).
  - If already executing, allow it to complete and report completion normally (MVP simplification).
- **Can be deferred:** Instruction-level preemption, mid-buffer cancellation.

#### `DxgkDdiPresent`
 
- **Purpose:** Handle present operations associated with VidPN source(s).
- **AeroGPU MVP behavior:**
  - Prefer to route present through the same submission path (UMD emits a PRESENT command).
  - If dxgkrnl calls `Present` for legacy/GDI reasons, translate it into:
    - a blit/copy into the current scanout allocation, or
    - a flip via `SetVidPnSourceAddress`, depending on present model.
- **Can be deferred:** Overlay presents, multi-plane composition, complex color space handling.
 
#### `DxgkDdiPatch` (optional; MVP should avoid needing it)
 
- **Purpose:** Apply patch location list relocations to a DMA buffer.
- **AeroGPU MVP behavior:** Design UMD command stream so patch list is empty.
  - If patch list is non-empty, fail the submission (debug build) or return not supported.
- **Can be deferred:** Full relocation support.
 
### 4.5 Interrupts, DPC, and TDR
 
#### `DxgkDdiInterruptRoutine`
 
- **Purpose:** Handle device interrupts at DIRQL and notify dxgkrnl.
- **AeroGPU MVP behavior:**
  - Read MMIO interrupt status:
    - `VSYNC` event
    - `FENCE_COMPLETE` event (with completed fence value)
  - Acknowledge/clear in MMIO.
  - Call the appropriate Dxgk callback to queue DPC work and report interrupt type.
- **Can be deferred:** Multiple interrupt sources beyond vsync + fence.
 
#### `DxgkDdiDpcRoutine`
 
- **Purpose:** Complete interrupt handling at DISPATCH_LEVEL.
- **AeroGPU MVP behavior:**
  - For completed fences: report progress to dxgkrnl (so waiting UMD threads wake).
  - For vsync: notify dxgkrnl of vblank (DWM scheduling).
- **Can be deferred:** Fine-grained telemetry.
 
#### `DxgkDdiResetFromTimeout` (TDR)
 
- **Purpose:** Recover from a “GPU hang” detected by Windows TDR.
- **AeroGPU MVP behavior:**
  - Reset the virtual GPU via MMIO (clear rings, reset fence).
  - Mark contexts as reset as required by WDDM contract.
  - Ensure future submissions work.
- **Can be deferred:** Per-engine resets, advanced hang diagnosis.

#### `DxgkDdiCollectDbgInfo` (optional)

- **Purpose:** Provide Windows with debug info after a TDR/hang.
- **AeroGPU MVP behavior:** Return minimal information (ring pointers, last submitted/completed fences) to aid debugging.
- **Can be deferred:** Full hardware state dumps.
 
### 4.6 Pointer (hardware cursor)
 
#### `DxgkDdiSetPointerShape`
 
- **Purpose:** Provide cursor bitmap/shape to hardware.
- **AeroGPU MVP behavior:**
  - Store cursor image in a small internal buffer (or a dedicated “cursor allocation” in system memory).
  - Write cursor metadata to MMIO so emulator can composite cursor in the scanout.
- **Can be deferred:** Color cursor formats beyond ARGB, animated cursor.
 
#### `DxgkDdiSetPointerPosition`
 
- **Purpose:** Update cursor position/visibility.
- **AeroGPU MVP behavior:** Write x/y/visible to MMIO; emulator composites.
- **Can be deferred:** Multi-monitor cursor constraints.
 
---
 
## 5. Memory model (minimal)
 
### 5.1 Segments
 
**MVP segment plan:**
 
- Expose **exactly one** memory segment to VidMM:
  - **Segment 1:** System memory (`D3DKMDT_MEMORY_SEGMENT_TYPE_SYSTEM`)
  - CPU-visible, GPU-visible (for our virtual GPU “GPU-visible” just means “emulator can read guest physical memory”)
  - No dedicated VRAM, no aperture, no tiling/swizzling
 
This keeps the KMD simple and allows the emulator to access all resources directly from guest RAM.
 
### 5.2 Allocation backing and “guest physical” mapping
  
For each allocation created by the KMD:
  
- Back it with locked system pages (nonpaged) to avoid paging complexity.
- Track:
  - Guest physical base address + size in bytes (for MVP, allocate physically-contiguous backing so this is a single range)
  - A non-zero stable `alloc_id` assigned by the KMD
  
**Emulator access model:**
  
- The emulator already implements guest physical memory (it must for CPU/MMU).
- For each submission, KMD sends the emulator a sideband table mapping **`alloc_id` → guest physical address + size** so the emulator can read textures/buffers and write render targets.

`alloc_id` must be stable across shared-handle opens. The KMD persists it in **WDDM allocation private driver data** and returns it to the UMD on both allocation create and open (`DxgkDdiCreateAllocation` / `DxgkDdiOpenAllocation`), so multiple guest processes can compute consistent IDs for the same underlying shared allocation.
  
### 5.3 Avoiding complex patch lists
  
Traditional WDDM drivers rely on `PATCHLOCATIONLIST` to relocate GPU addresses inside DMA buffers. We avoid this by designing the AeroGPU command stream to use **stable allocation IDs (`alloc_id`)**, not absolute addresses:
  
**In the command stream:**
  
- Resources that are backed by guest memory reference their backing allocation via `alloc_id` (for example the `backing_alloc_id` fields in `drivers/aerogpu/protocol/aerogpu_cmd.h`).
- Offsets are explicit byte offsets from the start of that allocation.
  
**Per-submit sideband table (built by KMD):**
  
```
struct aerogpu_alloc_entry {
  u32 alloc_id;
  u32 flags;
  u64 gpa;
  u64 size_bytes;
};
```
  
This yields:
  
- No relocation logic in KMD.
- Minimal KMD validation (bounds check: `offset + size <= alloc.size_bytes`).
- Emulator can resolve resource addresses by `alloc_id` quickly.
 
---
 
## 6. Present + scanout path (single output)
 
### 6.1 Single output contract
 
MVP assumes:
 
- One VidPN source: `SourceId = 0`
 
- One target/monitor: `TargetId = 0`
 
- One scanout surface active at a time.
 
### 6.2 How scanout is updated
 
Windows flips/sets scanout via `DxgkDdiSetVidPnSourceAddress`. In our driver:
 
1. dxgkrnl passes the primary allocation handle and presentation parameters.
2. KMD resolves that allocation to:
   - guest physical address list (PFNs)
   - pitch
   - pixel format
3. KMD programs AeroGPU MMIO “scanout registers” with this info.
4. Emulator reads scanout surface from guest memory and displays it.
 
### 6.3 Vblank/vsync simulation (DWM stability)
  
DWM’s scheduling expects periodic vblank events. Because AeroGPU is virtual:
 
- The emulator will generate a **fixed-rate vsync** (default 60Hz) using its host timer.
- On each vsync:
  - Emulator raises the AeroGPU interrupt
  - KMD `InterruptRoutine` reports a VSYNC interrupt for Source 0
 
`GetScanLine` may be implemented as:
 
- A simple time-based estimate: `scanline = (t % frame_time) * height / frame_time`
- Or a constant “in vblank” response if acceptable for early bring-up
  
**MVP requirement:** vsync interrupts must be regular enough that DWM does not hang or TDR due to missed presents.

For the concrete “minimal contract” (what Win7 expects) and the recommended device model/registers, see:

- `docs/graphics/win7-vblank-present-requirements.md`
- `drivers/aerogpu/protocol/vblank.md` (adds `AEROGPU_IRQ_SCANOUT_VBLANK` + `AEROGPU_MMIO_REG_SCANOUT0_VBLANK_*` timing registers)
  
---
 
## 7. Command transport boundary (device ↔ emulator)
 
### 7.1 PCI device model
 
The guest sees a PCI display controller:
 
- PCI class: `0x03` (display), subclass `0x00` (VGA-compatible optional; pure display also acceptable)
- BAR0: MMIO register block (4KB is enough for MVP)
- Interrupt: line-based IRQ is acceptable for MVP; MSI can be added later
 
### 7.2 MMIO register block (conceptual)
 
Minimum registers:
 
- **Identification**
  - `REG_DEVICE_ID`, `REG_VERSION`
 
- **Submission ring**
  - `REG_RING_GPA` (guest physical address of ring buffer)
  - `REG_RING_SIZE`
  - `REG_RING_HEAD`, `REG_RING_TAIL`
  - `REG_DOORBELL` (write to notify new work)
 
- **Fence / completion**
  - `REG_FENCE_NEXT` (optional; KMD can allocate fences itself)
  - `REG_FENCE_COMPLETED` (written by emulator)
 
- **Interrupts**
  - `REG_INT_STATUS`
  - `REG_INT_MASK`
  - `REG_INT_ACK`
 
- **Scanout**
  - `REG_SCANOUT_MODE` (w/h/format)
  - `REG_SCANOUT_PITCH`
  - `REG_SCANOUT_SURFACE_GPA` (or pointer to page list blob)
 
- **Cursor (optional but recommended)**
  - `REG_CURSOR_ENABLE`, `REG_CURSOR_XY`
  - `REG_CURSOR_HOTSPOT`
  - `REG_CURSOR_SURFACE_GPA`
 
### 7.3 Shared submission ring
 
The ring lives in guest RAM so both KMD and emulator can access it.
 
Each ring entry (“submission descriptor”) contains:
 
```
struct AerogpuSubmitDesc {
  u64 fence_id;
 
  // Command stream buffer
  u64 cmd_buf_gpa;
  u32 cmd_buf_size;
 
  // Sideband allocation refs (out-of-line blob)
  u64 alloc_table_gpa;
  u32 alloc_table_size;
 
  // Optional: context id, flags
  u32 context_id;
  u32 flags;
};
```
 
### 7.4 Fence/completion signaling path
 
1. UMD submits work → dxgkrnl → KMD `Render`/`SubmitCommand`.
2. KMD writes a `AerogpuSubmitDesc` with a monotonically increasing `fence_id`.
3. Emulator executes and then:
   - updates `REG_FENCE_COMPLETED = fence_id`
   - raises interrupt with `INT_FENCE_COMPLETE`
4. KMD interrupt routine + DPC:
   - reads completed fence
   - notifies dxgkrnl so waiting threads unblock and the scheduler advances
 
---
 
## 8. Scope control (explicit non-goals for MVP)
 
We will **not** implement these in the first functional driver (they must return “not supported” cleanly and deterministically):
 
- Multi-monitor / multiple VidPN sources/targets
- Display rotation, scaling, color management, HDR
- Overlay planes / multi-plane overlay composition
- Dedicated VRAM segments, aperture segments, hardware paging, eviction
- Memory compression, tiling/swizzling
- Advanced scheduling: multiple engines, priorities, fine-grained preemption
- Power management beyond “always on” (no DxgkDdiSetPowerState complexity)
- Video decode/processing acceleration (DXVA)
- OpenGL ICD
- Kernel/user shared surface optimizations beyond correctness (performance later)
 
---
 
## 9. Toolchain / build / signing (Windows 7 SP1)
 
### 9.1 Supported build environment
 
**KMD (aerogpu.sys):**
  
- **WDK:** Windows Driver Kit 7.1 (targets Windows 7 / WDDM 1.1)
- **Compiler:** VS2008 SP1 or VS2010 toolchain as used by WDK 7.1 build environments
- **Build system:** WDK `build` (recommended for reproducibility)
  
**UMDs (aerogpu_d3d9*.dll, optional aerogpu_d3d10*.dll):**
  
- **MSBuild:** Visual Studio Build Tools / Visual Studio (see `drivers/aerogpu/build/README.md` for the exact supported versions)
- **Build entrypoint:** `drivers\aerogpu\build\build_all.cmd`
 
### 9.2 Test signing + installation workflow
 
 For development we rely on **test signing**.
 
1. Create a test certificate (self-signed is fine for local testing).
 2. Sign:
    - `aerogpu.sys`
    - `aerogpu_d3d9_x64.dll` and `aerogpu_d3d9.dll`
    - generate/sign `aerogpu.cat`
3. Enable test signing:
 
```bat
bcdedit /set testsigning on
shutdown /r /t 0
```
 
4. Install the certificate into:
   - Trusted Root Certification Authorities
   - Trusted Publishers
 
 5. Install the driver package:
 
```bat
 pnputil -i -a aerogpu.inf
 ```
 
**Note:** x64 requires proper signing/test mode; do not rely on “F8 disable enforcement” as a workflow.
 
---
 
## 10. Validation strategy (minimal acceptance tests)
 
### 10.1 Smoke tests (must pass before deeper work)
 
1. **Driver loads**
   - Device Manager shows AeroGPU without Code 43
   - `dxdiag` runs without crashing
 
2. **Mode set works**
   - Resolution can be changed to at least one non-default mode
   - No black screen; no reboot loop
 
3. **Vsync path stable**
   - System stays responsive on desktop for 5+ minutes
   - No TDR popups (`Display driver stopped responding...`)
 
### 10.2 DWM/Aero tests (MVP target)
 
4. **Enable Aero**
   - Switch to “Windows 7” theme; DWM composition stays enabled
   - Move/resize windows repeatedly without hangs
 
5. **Flip/present stability**
   - Drag windows quickly; ensure no flicker storms or repeated TDR
 
### 10.3 D3D9 app tests (MVP target)
 
6. **D3D9Ex triangle**
   - A tiny D3D9Ex sample renders a rotating triangle at 60Hz (or stable frame pacing)
   - Present works for both windowed and fullscreen exclusive (fullscreen can be deferred if it adds complexity; document exact MVP choice)
 
7. **Resource sanity**
   - Create/destroy textures repeatedly without leaking kernel resources
   - Basic lock/unlock/mapping works
 
### 10.4 Regression tests to add early
 
- “TDR torture”: intentionally stall GPU worker and verify `ResetFromTimeout` recovers.
- “Present spam”: loop Present in a tight loop and ensure fences complete monotonically.
 
---
 
## 11. Clean-room / licensing constraints
 
- Do **not** copy proprietary Microsoft or vendor driver code.
- WDK headers define the public DDI contracts and are acceptable to use.
- The WDK ships sample drivers; using them as a behavioral reference can be helpful, but:
  - **Do not copy/paste** sample code into this project.
  - Treat samples as “read-only reference”; re-implement independently.
  - If any sample-derived logic is necessary, document the behavior and implement from first principles.
 
