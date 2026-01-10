# Win7 (WDDM 1.1) vblank & present-timing requirements (minimal contract)

This document defines the **minimal vertical-blank (vblank) + present timing contract** needed for a Windows 7 SP1 **WDDM 1.1** display driver to keep **Desktop Window Manager (DWM)** composition (Aero) stable.

It is written for the AeroGPU project (emulator + Windows 7 kernel-mode driver), but the requirements map directly onto standard WDDM/DDI concepts (`DxgkDdi*` entry points and `D3DKMT*` thunks).

> Scope: **timing + interrupt semantics only**. It does not define the rendering command protocol.

---

## Why vblank matters on Windows 7

On Windows 7, both **applications** and **DWM** rely on the display stack being able to:

1. **Block until the next vblank**, via `D3DKMTWaitForVerticalBlankEvent`.
2. **Schedule / throttle presentation** to the scanout cadence (vsync), via `D3DKMTPresent` / Direct3D `Present`/`PresentEx` with `SyncInterval >= 1`.

If the driver cannot deliver vblank events reliably, callers waiting on vblank can stall indefinitely, which cascades into:

* DWM composition not advancing (stutter / apparent hang)
* Present throttling breaking (either runaway rendering or total stall, depending on how the runtime falls back)

In real hardware, vblank is a **free-running property of scanout**. The safest contract for a virtual display device is to emulate that: **a periodic vblank tick independent of Present**.

---

## Terminology (as used here)

* **vblank / vsync**: the display vertical blanking interval; a periodic event at the refresh rate.
* **VidPn source**: a single scanout pipeline (assume **one** source for MVP unless multi-monitor is required).
* **SyncInterval**: the number of vblank intervals to wait before presenting (Direct3D 9: `D3DPRESENT_INTERVAL_*`, DXGI: `SyncInterval` argument).
* **vblank counter**: a monotonically increasing sequence number incremented once per vblank.
* **vblank timestamp**: monotonically increasing time of the **most recent** vblank (in a device-defined timebase).

---

## What Windows expects (contract-level, not policy)

These expectations follow from the behavior of the public kernel thunks and the WDDM interrupt model:

### Vblank delivery must exist

* `D3DKMTWaitForVerticalBlankEvent` is fundamentally “wait until the next vblank for source X”.
* The kernel can only complete this wait if the miniport driver **notifies** a vblank interrupt for that source.

Therefore: **a WDDM driver that advertises a working display must be able to generate vblank notifications** for each enabled source.

### Vblank must be periodic (not present-driven)

Windows makes no guarantee that vblank waits only happen “because something is presenting”.

DWM in particular may:
* wait for vblank to pace its composition loop
* present only when there are invalid regions / animations

If vblank is generated *only when Present occurs*, any path that waits for vblank before deciding whether to present can deadlock.

Therefore the minimal safe model is a **free-running vblank tick** while the output is enabled.

### SyncInterval must not complete “early”

For `SyncInterval >= 1`, present completion must not occur *before* the appropriate vblank boundary.

Windows (and many apps) tolerate jitter and occasional missed frames, but they do not tolerate presents that claim to be vsynced while completing immediately.

Therefore: treat vsynced presents as “complete on vblank N”.

---

## Minimal contract (crisp requirements)

This is the “do exactly this and stop guessing” section.

### 1) Vblank cadence

**MUST:** Provide a periodic vblank event for each enabled VidPn source.

**MUST:** Default to **60 Hz** (16.666… ms period) unless the selected mode specifies otherwise.

**SHOULD:** Keep long-term average frequency close to the target. Per-tick jitter is acceptable; multi-100ms gaps are not.

**MUST NOT:** Make vblank conditional on `Present`/flip activity.

Rationale: avoids deadlocks and keeps DWM pacing sane even when the desktop is idle.

### 2) Vblank enable/disable semantics

**MUST:** Support an interrupt enable/disable mechanism (mirrors `DxgkDdiControlInterrupt`).

**MUST:** When vblank interrupts are disabled, do not deliver guest interrupts for vblank.

**SHOULD:** Continue advancing the **vblank counter/timestamp** even when interrupts are disabled (scanout continues even if nobody listens).

Rationale: dxgkrnl enables/disables interrupts based on whether there are waiters; correct masking prevents interrupt storms.

### 3) WaitForVerticalBlankEvent semantics

**MUST:** Each call to `D3DKMTWaitForVerticalBlankEvent` for an enabled source must complete on the **next** vblank after the call begins waiting.

**MUST:** If multiple vblanks occur while the guest is delayed, it is acceptable to coalesce (wake once), but the next wake must still occur promptly.

Rationale: DWM and games use this to pace; indefinite waits break the desktop.

### 4) Present + SyncInterval behavior (minimum viable)

This section is deliberately conservative: it aims for “works everywhere” over perfect fidelity.

**SyncInterval = 0 (immediate):**
* **MUST:** return/complete without waiting for vblank.
* **MAY:** still latch the displayed image on the next vblank (no tearing simulation required for MVP).

**SyncInterval = 1 (vsync):**
* **MUST:** do not complete earlier than the next vblank.
* **SHOULD:** complete *on* that vblank (or very shortly after).

**SyncInterval > 1:**
* **MUST:** complete no earlier than that many vblanks after the first eligible boundary.
* **MAY:** clamp to 1 for MVP if higher intervals are rare, but document it if you do.

Rationale: DWM is effectively a SyncInterval=1 workload; other values are less critical for “Aero desktop works”.

### 5) Monotonic counters / timestamps

**MUST:** Provide at least one monotonic vblank sequence counter per source (`u64`).

**MUST:** Provide at least one monotonic timestamp of the most recent vblank per source (`u64`), using a stable timebase:
* “nanoseconds since device boot” is fine, or
* “100ns units since device boot” is fine.

**MUST:** Counters/timestamps must never go backwards.

Rationale: user-mode components and diagnostics often sanity-check monotonicity; monotonic clocks are also needed for scanline emulation.

### 6) Occlusion / minimization / idle desktop

**MUST:** Continue producing vblank ticks while the output is enabled, even if:
* no windows are moving
* no presents occur for long periods
* the desktop is “idle”

**MAY:** If the OS explicitly disables the source (DPMS off / monitor removed), you may pause vblank, but on re-enable the vblank clock must resume immediately.

Rationale: avoid coupling vblank to rendering activity.

---

## WDDM 1.1 implementation guidance (KMD)

This maps the minimal contract onto the WDDM miniport responsibilities. Names are from the WDK; the exact call graph is managed by dxgkrnl.

### Interrupt path

When a vblank occurs (device-side):

1. Miniport ISR (`DxgkDdiInterruptRoutine`) reads device interrupt status.
2. If the interrupt is vblank for source *S*:
   * acknowledge/clear the device interrupt
   * notify dxgkrnl via `DxgkCbNotifyInterrupt` with a vblank/vsync interrupt type and the source id
   * queue the miniport DPC via `DxgkCbQueueDpc` (if required by the platform/WDK contract)
3. Miniport DPC (`DxgkDdiDpcRoutine`) performs any deferred work and calls the appropriate dxgkrnl DPC notification callback(s).

The key requirement is that dxgkrnl observes “vblank happened for source S” so it can:
* release threads waiting in `D3DKMTWaitForVerticalBlankEvent`
* advance present/flip bookkeeping that is keyed to vblank

### Interrupt enable/disable (`DxgkDdiControlInterrupt`)

dxgkrnl uses this entrypoint to mask vblank interrupts when there are no waiters.

For a virtual device:
* translate “enable vblank interrupt” into setting the device IRQ mask bit(s)
* translate “disable” into clearing them

### GetScanLine (optional but recommended)

Many D3D9-era apps call `IDirect3DDevice9::GetRasterStatus`, which maps down to `D3DKMTGetScanLine` → `DxgkDdiGetScanLine`.

For an emulator, a simple implementation is:
* compute `(now - last_vblank_timestamp) mod period`
* map that into a scanline number `[0, total_lines)` and an `InVBlank` flag

Even an approximate scanline is usually sufficient as long as:
* it is monotonic within a frame
* it resets near vblank
* `InVBlank` is consistent with the scanline range you define

---

## Recommended minimal vblank simulation model (emulator/device)

This is the concrete implementation model that downstream tasks should implement.

### Model summary (single-source MVP)

* Refresh rate: **60 Hz**
* Maintain per-source state:
  * `vblank_seq: u64` (increments by 1 each vblank tick)
  * `last_vblank_time_ns: u64` (monotonic timestamp)
  * `irq_enabled: bool`
  * `irq_pending: bool` (level-triggered status bit)

On each vblank tick:

1. `vblank_seq += 1`
2. `last_vblank_time_ns = now_ns`
3. If `irq_enabled`:
   * set `irq_pending = true`
   * set the device IRQ status bit for vblank
   * assert the guest interrupt line

On IRQ acknowledge (guest writes W1C to an IRQ-ACK register):

1. clear the vblank status bit
2. clear `irq_pending`
3. if no other IRQs pending, deassert interrupt line

### Timer implementation (host side)

For good long-term stability, avoid `setInterval(period)` drift. Instead:

* Keep a `next_deadline = t0 + period`
* On each host tick:
  * while `now >= next_deadline`:
    * emit one vblank tick
    * `next_deadline += period`

This naturally “catches up” if the host stalls, while preserving an average cadence.

### Present interaction

Do not use Present to *generate* vblank.

Instead, treat vblank as the “display latch point”:
* for vsynced presents, latch/swap the scanout surface on the next vblank
* for immediate presents, you may latch immediately or on next vblank, but **do not block the caller**

---

## Protocol addendum (AeroGPU)

For Win7 pacing, AeroGPU needs:

* a vblank IRQ bit integrated into the existing IRQ status/enable/W1C-ack mechanism, and
* monotonic vblank sequence + timestamp registers (per scanout source).

`aerogpu_pci.h` already defines `AEROGPU_IRQ_SCANOUT_VBLANK` and the `IRQ_STATUS/IRQ_ENABLE/IRQ_ACK` registers; this addendum focuses on the missing timing registers and semantics.

See: `drivers/aerogpu/protocol/vblank.md`.

---

## Suggested validation experiments (Win7 VM)

Even if the initial implementation is “spec-driven”, it is worth validating the pacing path early because vblank issues often present as non-obvious hangs or theme fallback.

### DWM pacing sanity check (cheap, guest-side)

The repository includes a guest-side test that measures whether DWM is actually pacing on a refresh-like cadence:

* `drivers/aerogpu/tests/win7/dwm_flush_pacing`

It times successive `DwmFlush()` calls and fails if:

* `DwmFlush()` returns “almost immediately” (suggesting composition is disabled or not vsync paced), or
* there are multi-hundred-millisecond gaps (often a missing/broken vblank/interrupt path).

Run it in a Win7 VM:

```cmd
cd drivers\aerogpu\tests\win7
build_all_vs2010.cmd
run_all.cmd
```

### ETW/GPUView (deeper, for root-cause)

If you need to distinguish “vblank not firing” vs “presents not completing” vs “scheduler stalls”, collect a GPUView-compatible ETW trace with at least:

* `Microsoft-Windows-DxgKrnl`
* `Microsoft-Windows-Dwm-Core`

Then inspect for:

* regular vsync/vblank cadence,
* `Present`/flip completions aligned to vblank boundaries,
* long gaps where `dwm.exe` is blocked waiting on vblank or GPU progress.

## Final recommendation (implement this)

Implement a **free-running 60 Hz vblank clock** (per active VidPn source), with:

1. **Periodic vblank interrupts** delivered only when enabled by the guest driver.
2. A **monotonic vblank sequence counter** and **monotonic vblank timestamp** exposed via registers.
3. Present behavior where:
   * `SyncInterval=0` does not block
   * `SyncInterval=1` completes on the next vblank (or very shortly after), never earlier

This model matches the assumptions baked into the Windows 7 WDDM + D3D stack and is the minimal, robust contract to keep DWM composition stable.
