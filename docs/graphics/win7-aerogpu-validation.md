# Win7 AeroGPU (WDDM 1.1) Driver Validation & Stability Checklist
*(TDR / vblank / perf baseline / debug playbook)*

This document is a practical “bring-up and keep-it-stable” checklist for the **AeroGPU** WDDM 1.1 driver stack on **Windows 7 SP1** (VM or emulator). It assumes you are iterating on a custom WDDM miniport + user-mode driver and that the most common failures are:

- **TDRs** (Timeout Detection & Recovery) → device removed, resets, or BSODs
- **DWM falling back to Basic theme** (“The color scheme has been changed…”) → no Aero glass
- **Random hangs** (often vblank waits or fence/interrupt issues) → frozen desktop, deadlocked threads

Protocol source of truth:
`drivers/aerogpu/protocol/*` (see `drivers/aerogpu/protocol/README.md` and
`docs/graphics/aerogpu-protocols.md`).

The goal is repeatability: a developer should be able to follow this in a VM and converge on a **stable** driver before chasing correctness/performance.

---

## 0) Test environment assumptions (so results are comparable)

These aren’t requirements, but if you deviate you should explicitly note it in bug reports.

- **OS:** Windows 7 SP1 (x86 or x64), fully booting to desktop.
- **Session:** Local console session (avoid Remote Desktop for Aero validation; RDP frequently changes the composition path).
- **Single monitor** at a conservative mode first (e.g., **1024×768 @ 60 Hz**).
- **Symbol/debug access:** Either kernel debugger, serial output, or at least user-mode logs + Event Viewer.
- **Driver build:** test-signed or test mode enabled.

### 0.1 Validation record (copy/paste template)

When claiming “works on Win7” (or filing a bug), capture a minimal, comparable record. This makes it much easier to spot regressions across commits and across x86/x64 guests.

```text
Date:
Repo commit (host/emulator + drivers):
Driver package version (INF/UMD/KMD build id):
Device HWID: VID=0xA3A0 / DID=0x0001
Test signing: (on/off)
Session: (local console / RDP / other)
Guest Tools drive letter: (mounted ISO/zip, e.g. D)

Guest OS:
- Win7 SP1 x86: (winver string, e.g. 6.1.7601 Service Pack 1 Build 7601)
- Win7 SP1 x64: (winver string, e.g. 6.1.7601 Service Pack 1 Build 7601)

Test suite:
- x86 command:
  bin\\aerogpu_test_runner.exe --json --log-dir=logs --dbgctl=<GuestToolsDrive>:\\drivers\\x86\\aerogpu\\tools\\win7_dbgctl\\bin\\aerogpu_dbgctl.exe --require-vid=0xA3A0 --require-did=0x0001 --require-umd
- x64 command:
  bin\\aerogpu_test_runner.exe --json --log-dir=logs --dbgctl=<GuestToolsDrive>:\\drivers\\amd64\\aerogpu\\tools\\win7_dbgctl\\bin\\aerogpu_dbgctl.exe --require-vid=0xA3A0 --require-did=0x0001 --require-umd

Results summary:
- Win7 x86: PASS=  FAIL=  SKIP=
- Win7 x64: PASS=  FAIL=  SKIP=

Artifacts collected:
- report.json (path):
- per-test JSON outputs (dir):
 - per-test stdout/stderr logs (dir):
  - dbgctl (`aerogpu_dbgctl --status`) snapshots (dbgctl_<test>_status.txt) (dir):
   - failing test --dump outputs (BMP/bin/dxbc) (dir):
  - dbgctl last-submission cmd dump (`aerogpu_dbgctl --dump-last-submit` / `--dump-last-cmd`) outputs (for example: `cmd.bin` + `cmd.bin.alloc_table.bin` + `cmd.bin.txt`; or for multi-dump: `cmd_*.bin` + siblings) (when available):
  - Event Viewer: dxgkrnl/display events around failures (exported EVTX):
  - KMD snapshots: aerogpu_dbgctl.exe --query-fence/--dump-ring/--dump-vblank/--dump-createalloc (outputs saved):
   Notes:
```

Note: when the suite runner is invoked with `--json`, failing tests in `report.json` include an `artifacts` array
listing relevant files (for example: per-test logs and dbgctl dumps when `--dbgctl=...` is enabled), so downstream CI
can locate the most relevant outputs without additional heuristics.

`--dbgctl=...` is optional. In packaged installs, `aerogpu_dbgctl.exe` is shipped under the AeroGPU driver directory:
 - Guest Tools ISO/zip: `drivers\\amd64\\aerogpu\\tools\\win7_dbgctl\\bin\\aerogpu_dbgctl.exe` (Win7 x64) and `drivers\\x86\\aerogpu\\tools\\win7_dbgctl\\bin\\aerogpu_dbgctl.exe` (Win7 x86)
  - Optional top-level tools payload (when present): `tools\\aerogpu_dbgctl.exe` (or `tools\\<arch>\\aerogpu_dbgctl.exe`)
  - CI-staged packages: `out\\packages\\aerogpu\\x64\\tools\\win7_dbgctl\\bin\\aerogpu_dbgctl.exe` (and `...\\x86\\...`)

If you prefer `--dbgctl=aerogpu_dbgctl.exe`, copy the tool next to `bin\\aerogpu_test_runner.exe`.
Use `--dbgctl-timeout-ms=NNNN` to bound how long the suite runner will wait for the dbgctl process itself (default: 5000ms).

### 0.2 AGPU-WIRE-004 — vblank + vsynced present pacing validation (canonical browser runtime)

This repo’s canonical *browser* runtime uses:

- a CPU worker running the Rust `Machine` (including the AeroGPU device model), and
- a GPU worker executing AeroGPU command streams out-of-process via the “submission bridge”.

For Win7/WDDM stability, **vsync-paced present fences must complete on vblank**, and `D3DKMTWaitForVerticalBlankEvent`
must reliably block until the *next* vblank.

#### Repro (Win7 guest)

Build and run the focused pacing tests:

```bat
cd drivers\aerogpu\tests\win7
build_all_vs2010.cmd

wait_vblank_pacing\bin\wait_vblank_pacing.exe --samples=120 --wait-timeout-ms=2000
vblank_state_sanity\bin\vblank_state_sanity.exe --samples=10 --interval-ms=100
dwm_flush_pacing\bin\dwm_flush_pacing.exe --samples=120
```

Or run the full suite:

```bat
run_all.cmd
```

#### Results (fill in when validating on a Win7 VM)

| Test | PASS/FAIL | avg (ms) | min (ms) | max (ms) | Notes |
|---|---:|---:|---:|---:|---|
| `wait_vblank_pacing` | TBD | TBD | TBD | TBD | `D3DKMTWaitForVerticalBlankEvent` cadence |
| `vblank_state_sanity` | TBD | TBD | TBD | TBD | vblank seq/time monotonicity + period sanity |
| `dwm_flush_pacing` | TBD | TBD | TBD | TBD | end-to-end DWM vsync pacing |

#### Implementation note (why this matters in the browser runtime)

In the browser runtime, do **not** rely on the GPU worker’s `requestAnimationFrame`-driven tick loop to “fake vsync” by
delaying `submit_complete`. The AeroGPU device model has a free-running vblank clock; vsync pacing should be keyed to
that clock so it remains correct even when host ticks are jittery or throttled.

---

## 1) Bring-up checklist (smoke tests in strict order)

The fastest way to make progress is to **never skip steps**. Each step has a “pass criteria” and common failure hints.

### 1.1 Driver loads

**Goal:** the WDDM stack loads without Code 43, repeated resets, or boot loops.

**How to check**

- `devmgmt.msc` → Display adapters → AeroGPU device → **“This device is working properly.”**
- `dxdiag` → Display tab → **Driver Model: WDDM 1.1** (or at least WDDM).
- Event Viewer → **Windows Logs → System**:
  - No repeated **Display / dxgkrnl** errors during boot.
- Kernel debug log (WinDbg / serial / DebugView “Capture Kernel”):
  - Confirm BAR0 selection is not dependent on PCI resource ordering:
    - `aerogpu-kmd: StartDevice: BAR0 probe inspected ... selected mem[...] ... len=... magic=...`
      - Expect the selected BAR0 `len` to be the small MMIO window (typically **64 KiB**), not the large BAR1 VRAM aperture (typically **64 MiB**).
    - `aerogpu-kmd: StartDevice: ABI=v1 magic=...` should still appear for AGPU devices.

**Pass criteria**

- Desktop is usable; no periodic screen flicker/reset; no immediate “display driver stopped responding” toast.

**If it fails**

- Code 43: usually init failure, capability mismatch, missing UMD, or wrong INF/registry.
- Boot loop or instant TDR: submission/fence interrupt path broken (see TDR section).

---

### 1.2 Mode set

**Goal:** the driver sets a mode and the OS uses it (not stuck on Standard VGA).

**How to check**

- Screen Resolution settings show expected modes.
- Switch between two known modes (e.g., 800×600 and 1024×768) and back.
  - Or run `drivers/aerogpu/tests/win7/modeset_roundtrip_sanity` to validate a mode switch + scanout state roundtrip in an automated way.

**Pass criteria**

- Mode changes succeed repeatedly without corrupting scanout or hanging.

**If it fails**

- Black screen after mode set: scanout surface address/stride/pitch mismatch, or no vblank/flip completion.
- “Reverts after 15 seconds”: driver didn’t report completion/ownership correctly.

---

### 1.3 Scanout visible (stable framebuffer updates)

**Goal:** actual pixels reliably change on-screen.

**How to check**

- Drag windows around; open Notepad; resize Explorer windows.
- Run a simple animation (e.g., open the Start menu repeatedly).

**Pass criteria**

- No persistent corruption; no tearing that escalates into a hang; no “stuck” frames.

**If it fails**

- Corruption only during movement: present/flip or blit path broken, or wrong synchronization.
- “Occasionally updates then freezes”: vblank wait or fence completion stuck.

---

### 1.4 DWM starts (composition infrastructure is alive)

**Goal:** Desktop Window Manager is running and able to create a D3D device.

**How to check**

- `services.msc` → **Desktop Window Manager Session Manager** (UxSms) is Running.
- Task Manager → `dwm.exe` exists for the interactive session.

**Pass criteria**

- No immediate crash of `dwm.exe` after logon.
- No perpetual “Windows is checking for a solution…” dialogs.

**If it fails**

- DWM crash at startup often indicates: device creation failing (feature caps), UMD load failure, or present path returning errors.

---

### 1.5 Aero enabled

**Goal:** Windows can enable Aero Glass and keep it enabled under light interaction.

**How to check**

- Right click desktop → Personalize → pick an **Aero theme**.
- Confirm glass transparency, live thumbnails (Alt-Tab), and taskbar previews.

**Pass criteria**

- Aero stays enabled for **10+ minutes** of normal window interaction.

**If it fails**

- If it flips to Basic with a notification: usually a TDR/device-removed event, or DWM couldn’t meet composition timing (vblank/sync issues).

---

### 1.6 D3D sample passes (minimum 3D sanity)

**Goal:** the 3D API path works end-to-end and survives device-lost handling.

**How to check (pick at least one)**

- `dxdiag` → Display → **Test Direct3D** (D3D9/10/11 as applicable).
- A minimal internal sample that:
  - creates a device,
  - draws a rotating triangle,
  - presents at vsync (or at least repeatedly),
  - exits cleanly.
- For D3D11 compute bring-up (FL10\_0): run `drivers/aerogpu/tests/win7/d3d11_compute_smoke` to validate
  `ID3D11ComputeShader` creation, SRV/UAV buffer binding, `Dispatch`, and staging readback.

**Pass criteria**

- The sample runs without triggering a TDR and without leaking resources across multiple runs.

**If it fails**

- `DXGI_ERROR_DEVICE_HUNG` / `DEVICE_REMOVED`: see TDR strategy and common error codes.

---

## 2) TDR strategy (design + dev-only knobs)

Windows 7’s GPU scheduler expects **regular forward progress**. If it believes the GPU is hung, it triggers **TDR**:

- Default timeout is small (commonly ~2 seconds), and **Aero/DWM will trip it quickly** if your driver blocks progress.
- After recovery, applications see **device removed/reset**, and Windows may drop to **Basic** theme.

### 2.1 Design rules that prevent TDRs

#### Rule A — Never submit “unbounded” work

Every DMA buffer / command submission must have a predictable upper bound on completion time.

- Split large workloads into smaller submissions.
- Avoid “compile/translate everything” in a single submit path.
- If a translation layer (e.g., D3D → WebGPU) can stall on shader compilation, pipeline cache misses, or GPU queue backpressure, **do that work asynchronously** and keep each scheduled unit small.

**Practical target:** make 99.9% of submissions complete well under **100 ms** during bring-up. You can relax later.

#### Rule B — Fences must be monotonic and observable

WDDM stability depends on “is the GPU alive?” signals.

- Ensure the OS-visible completion fence for each submission:
  - strictly increases,
  - is eventually signaled,
  - is signaled via the correct interrupt/DPC path.
- Don’t rely on polling in hot paths; make the interrupt path reliable first.

If your “GPU” is virtual, it’s still the same requirement: **the KMD must complete what it told dxgkrnl it queued**.

#### Rule C — Don’t block inside the scheduling/DDI callbacks

As a rule of thumb:

- `SubmitCommand`-style paths should enqueue work and return quickly.
- Long waits inside KMD DDIs can deadlock dxgkrnl/DWM and *look like* a GPU hang.

If you must wait (e.g., for a host-side queue), enforce a very short timeout and fail fast with a controlled error rather than hanging.

#### Rule D — Implement `ResetFromTimeout` as a “real reset”

When TDR does happen, Windows calls the driver’s reset callback (WDDM: `DxgkDdiResetFromTimeout`).

Your reset implementation must:

- stop the engines / worker threads cleanly,
- reset ring pointers and internal queue state,
- signal any stuck fences as failed so the OS can unwind,
- resume and accept new work.

If reset fails, you often get **0x116 VIDEO_TDR_FAILURE** BSOD instead of recovery.

---

### 2.2 Dev-only registry tweaks (use sparingly, always revert)

These help you debug without constant resets. They are **not** for shipping drivers.

Registry path:

`HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers`

Suggested development settings:

```bat
:: Increase TDR timeouts (dev-only).
reg add "HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers" /v TdrDelay    /t REG_DWORD /d 10 /f
reg add "HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers" /v TdrDdiDelay /t REG_DWORD /d 20 /f

:: Keep recovery enabled (default is typically 3 = Recover).
reg add "HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers" /v TdrLevel    /t REG_DWORD /d 3 /f
```

Hard stop option (last resort):

- `TdrLevel = 0` disables TDR and can turn “driver bug” into “whole VM hard hang”.
- Only use it to capture additional logs when you *already know* the hang is coming and you have an out-of-band debugger.

To revert to defaults (recommended before any “stability” claims):

```bat
reg delete "HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers" /v TdrDelay    /f
reg delete "HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers" /v TdrDdiDelay /f
reg delete "HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers" /v TdrLevel    /f
```

**Notes**

- Reboot is typically required for changes to fully apply.
- If you ship with elevated TDR timeouts you will mask real hangs and regress user experience.

---

## 3) Vblank / vsync expectations (what Win7 + DWM want)

On Windows 7, DWM composition is paced by vertical blank and expects:

- A **steady cadence** (usually 60 Hz, but must match the mode you report).
- A working **WaitForVBlank** style path (directly or indirectly).
- Reasonable jitter (small jitter causes stutter; large gaps can cause hangs/timeouts).

If vblank delivery is broken, common symptoms:

- DWM “spins” and pegs a CPU core (if waits return immediately).
- DWM times out and disables Aero (Basic theme).
- Present calls block forever (if you wait on an event that never fires).

For the concrete “minimal contract” and a recommended device/emulator model, see:
* `docs/graphics/win7-vblank-present-requirements.md`

For quick guest-side sanity checks:

* DWM pacing (end-to-end compositor path): `drivers/aerogpu/tests/win7/dwm_flush_pacing`
* Direct vblank interrupt/wait path (independent of DWM): `drivers/aerogpu/tests/win7/wait_vblank_pacing` (targets VidPn source 0; tune hang detection via `--wait-timeout-ms`)
  * Escape ABI/device identity (`QUERY_DEVICE(_V2)`): `drivers/aerogpu/tests/win7/device_state_sanity`
  * WDDM segment budgets (`D3DKMTQueryAdapterInfo(GETSEGMENTGROUPSIZE)` / best-effort `QUERYSEGMENT`): `drivers/aerogpu/tests/win7/segment_budget_sanity` (validates `HKR\Parameters\NonLocalMemorySizeMB` takes effect)
  * Scanline/raster status plumbing (`D3DKMTGetScanLine` → `DxgkDdiGetScanLine`): `drivers/aerogpu/tests/win7/get_scanline_sanity`
  * Vblank counter/timestamp registers (`AEROGPU_ESCAPE_OP_QUERY_VBLANK`): `drivers/aerogpu/tests/win7/vblank_state_sanity`
  * Fence counters (`AEROGPU_ESCAPE_OP_QUERY_FENCE`): `drivers/aerogpu/tests/win7/fence_state_sanity`
  * Ring snapshot (`AEROGPU_ESCAPE_OP_DUMP_RING_V2`): `drivers/aerogpu/tests/win7/ring_state_sanity`
  * Scanout mode caching vs MMIO scanout state (`DxgkDdiCommitVidPn`): `drivers/aerogpu/tests/win7/scanout_state_sanity`
  * Mode set roundtrip (switch to an alternate reported mode and back): `drivers/aerogpu/tests/win7/modeset_roundtrip_sanity`
  * Cursor MMIO state (`AEROGPU_ESCAPE_OP_QUERY_CURSOR`): `drivers/aerogpu/tests/win7/cursor_state_sanity`
  * CreateAllocation trace dump (`AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION`): `drivers/aerogpu/tests/win7/dump_createalloc_sanity`
  * Debug-only shared-handle mapping escape stress (`AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE`): `drivers/aerogpu/tests/win7/map_shared_handle_stress` (skips when unsupported or gated off)
  * Dbgctl escape security gating (negative coverage for `READ_GPA` / `MAP_SHARED_HANDLE`): `drivers/aerogpu/tests/win7/dbgctl_escape_security_sanity` (skips when unsupported or gated off)
  * UMDRIVERPRIVATE discovery blob (`D3DKMTQueryAdapterInfo`): `drivers/aerogpu/tests/win7/umd_private_sanity`
  * D3D9 raster status (`IDirect3DDevice9::GetRasterStatus`): `drivers/aerogpu/tests/win7/d3d9_raster_status_sanity` and `drivers/aerogpu/tests/win7/d3d9_raster_status_pacing`
  * D3D9 fixed-function bring-up (legacy apps + DWM fixed-function paths): `drivers/aerogpu/tests/win7/d3d9_fixedfunc_wvp_triangle`, `drivers/aerogpu/tests/win7/d3d9_fixedfunc_textured_wvp`, and `drivers/aerogpu/tests/win7/d3d9_fixedfunc_lighting_directional`
  * D3D9Ex EVENT query behavior (non-blocking `GetData(D3DGETDATA_DONOTFLUSH)` + eventual signal; window hidden by default): `drivers/aerogpu/tests/win7/d3d9ex_event_query`
  * D3D9Ex per-submit fence stress (validates monotonic submit fences + EVENT query completion + PresentEx throttling; on AGPU also validates ring descriptor `AEROGPU_SUBMIT_FLAG_PRESENT` + non-zero `alloc_table_gpa` for presents): `drivers/aerogpu/tests/win7/d3d9ex_submit_fence_stress`
  * Alloc table READONLY propagation (validates Win7/WDDM `WriteOperation` metadata is surfaced as `AEROGPU_ALLOC_FLAG_READONLY` so the host can reject unintended guest-memory writeback): `drivers/aerogpu/tests/win7/alloc_table_readonly_sanity`
  * D3D9Ex DWM-critical device probes (must be non-blocking; checks `CheckDeviceState`, `PresentEx` throttling, `WaitForVBlank`, `GetPresentStats`, residency, GPU thread priority, etc.): `drivers/aerogpu/tests/win7/d3d9ex_dwm_ddi_sanity`
  * Cross-process shared surface open (validates `DxgkDdiOpenAllocation` + ShareToken-backed shared-surface interop across runtimes):
    * Canonical `share_token` contract: `docs/graphics/win7-shared-surfaces-share-token.md`
    * D3D9Ex: `drivers/aerogpu/tests/win7/d3d9ex_shared_surface` and `drivers/aerogpu/tests/win7/d3d9ex_shared_surface_ipc`
    * D3D9Ex (open/close churn stress; catches hangs/crashes under repeated create→open→destroy): `drivers/aerogpu/tests/win7/d3d9ex_shared_surface_stress`
    * D3D9Ex (Win7 x64 cross-bitness; DWM scenario): `drivers/aerogpu/tests/win7/d3d9ex_shared_surface_wow64`
    * D3D9Ex (alloc_id collision/persistence stress): `drivers/aerogpu/tests/win7/d3d9ex_shared_surface_many_producers` and `drivers/aerogpu/tests/win7/d3d9ex_alloc_id_persistence`
    * D3D9Ex (shared allocation policy: shared surfaces must be single-allocation): `drivers/aerogpu/tests/win7/d3d9ex_shared_allocations`
    * D3D10: `drivers/aerogpu/tests/win7/d3d10_shared_surface_ipc`
    * D3D10.1: `drivers/aerogpu/tests/win7/d3d10_1_shared_surface_ipc`
    * D3D11: `drivers/aerogpu/tests/win7/d3d11_shared_surface_ipc`
    * Validates cross-process pixel sharing via readback by default; for `d3d9ex_shared_surface`, pass `--no-validate-sharing` to focus on open + submit only (`--dump` always validates).

### 3.1 Recommended options (ranked by bring-up stability)

#### Option 1 — Fake periodic vblank IRQ (best for stability)

Generate a synthetic vblank interrupt at the refresh rate you advertise.

Properties:

- Decoupled from actual rendering → composition stays alive even if rendering slows.
- Easy to make “always forward-progress” → fewer random hangs.

Guidelines:

- Use a kernel timer (or a host-driven tick) to trigger at ~16.67 ms for 60 Hz.
- Keep it running continuously (do not “pause when idle” during bring-up).
- Ensure your interrupt/DPC work is lightweight; missed vblanks should be counted and logged.

#### Option 2 — Software waitable timer inside your vblank wait path

If you can’t (yet) deliver real/synthetic IRQs, make `WaitForVBlank` block on a periodic timer rather than on a hardware interrupt that never arrives.

Properties:

- Avoids deadlocks in DWM and apps that call vblank waits.
- Less realistic timing, but usually good enough to keep Aero enabled.

Guidelines:

- Ensure the wait is interruptible and won’t deadlock if called at high frequency.
- Avoid returning immediately (busy loops will destroy performance and may trigger watchdogs).

#### Option 3 — “Vsync interval ignored but stable” (acceptable early bring-up)

For the earliest pipeline stages you may ignore vsync intervals in present, but you must still keep the system stable:

- Presents must complete (no infinite blocking).
- Vblank waits must not spin.
- DWM must see a believable “frame pacing” signal (even if it’s approximate).

This option is often used to validate everything *except* timing correctness.

### 3.2 What to log for vblank correctness

At minimum, log:

- current mode (resolution + reported refresh),
- vblank count (monotonic),
- last vblank timestamp (QPC or KeQueryPerformanceCounter),
- vblank interval stats (avg/min/max over 5–10 seconds),
- number of “missed” vblanks (timer overruns or delayed delivery).

---

## 4) Performance baseline (what to measure and how to capture it)

Before optimizing, establish a baseline that answers:

1. **Is the driver stable under load?**
2. **Where is time going (CPU vs “GPU” vs synchronization)?**
3. **Is DWM pacing behaving?**

### 4.1 Metrics to log (minimum set)

| Metric | Why it matters | How to interpret |
|---|---|---|
| Present rate (presents/sec) | Confirms DWM/app pacing | Should be near refresh rate when vsync’d; if wildly higher, you’re probably spinning |
| Fence latency (submit → complete) | Direct TDR risk indicator | Track avg + p95/p99; long tail causes TDRs |
| Queue depth (pending submissions) | Backpressure and “stutter” | Growing without bound indicates the backend isn’t draining |
| Command throughput (bytes/sec) | Detects pathological spam | Useful for spotting unbatched state changes or command storms |
| Vblank jitter (ms) | DWM smoothness | High jitter correlates with stutter and theme fallback risk |
| Reset/TDR count | Stability KPI | Should be 0 in “stable” runs |

### 4.2 Capturing logs (practical in a VM)

#### Option A — AeroGPU internal logging (recommended for bring-up)

Prefer driver-owned logs that include your private state (ring pointers, fence values, etc.).

- KMD: log to a circular buffer + explicit dump via `aerogpu_dbgctl` (see below).
- UMD: log to `OutputDebugString` and/or a file (make file logging opt-in).

#### Option B — ETW trace for system correlation (good once “mostly stable”)

Use ETW to correlate DWM/dxgkrnl events with your internal logs.

- Collect: `DxgKrnl`, `DWM`, and your own provider if you have one.
- Analyze with Windows Performance Toolkit / GPUView (if available in your setup).

Even if you can’t run GPUView in the VM, a saved ETL is still valuable for offline analysis.

---

## 5) Debug playbook (with `aerogpu_dbgctl`)

`aerogpu_dbgctl.exe` is assumed to be a small command-line tool shipped with the driver package that talks to the AeroGPU KMD via a **driver-private escape** path (`DxgkDdiEscape` via `D3DKMTEscape`).

### 5.1 Where to find dbgctl on Guest Tools media

On a default Aero Guest Tools ISO/zip mount (often `X:`), dbgctl is shipped at:

- Win7 x64: `X:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`
- Win7 x86: `X:\drivers\x86\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`
- Optional top-level tools payload (when present): `X:\tools\aerogpu_dbgctl.exe` (or under `X:\tools\<arch>\aerogpu_dbgctl.exe`)

**Packaged location (relative to the `aerogpu` driver directory / package root):**

- Tool: `tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`
- Docs: `tools\win7_dbgctl\README.md`

Packaged locations:

- Guest Tools ISO/zip:
  - x64: `drivers\\amd64\\aerogpu\\tools\\win7_dbgctl\\bin\\aerogpu_dbgctl.exe`
  - x86: `drivers\\x86\\aerogpu\\tools\\win7_dbgctl\\bin\\aerogpu_dbgctl.exe`
  - Optional top-level tools payload (when present): `tools\\aerogpu_dbgctl.exe` (or `tools\\<arch>\\aerogpu_dbgctl.exe`)
- CI-staged packages (after `ci/make-catalogs.ps1`):
  - x64: `out\\packages\\aerogpu\\x64\\tools\\win7_dbgctl\\bin\\aerogpu_dbgctl.exe`
  - x86: `out\\packages\\aerogpu\\x86\\tools\\win7_dbgctl\\bin\\aerogpu_dbgctl.exe`

You can run it in-place from those directories, copy it somewhere on `PATH`, or pass the full path to callers like `aerogpu_test_runner.exe --dbgctl=<path>`.

**Bitness policy:** dbgctl is shipped as a single **x86** binary and copied into both the x86 and x64 driver packages.
On Windows 7 x64 it runs via **WOW64**. This ensures Windows 7 x86 users can always run the shipped tool.

Example (run directly from a mounted Guest Tools ISO/zip; replace `<GuestToolsDrive>` with the drive letter, e.g. `D`):

```bat
cd /d <GuestToolsDrive>:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin
aerogpu_dbgctl.exe --status
```

If your Guest Tools ISO is mounted as `X:` (common), you can also run it as a single copy/paste command:

```bat
:: Win7 x64:
X:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --status
:: Win7 x86:
X:\drivers\x86\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --status
```

All commands below assume you either run `aerogpu_dbgctl.exe` via a full path, or `cd` into the directory containing `aerogpu_dbgctl.exe` first (as above).

### 5.2 Typical workflow

1. **Turn on verbose logs** (checked build via WinDbg / `DbgPrintEx` filtering; dbgctl log-level control is optional/driver-specific)
2. **Reproduce the issue quickly** (Aero enable, dxdiag test, your sample).
3. **Snapshot state immediately after**
   ```bat
   aerogpu_dbgctl.exe --query-version
   aerogpu_dbgctl.exe --query-umd-private
   aerogpu_dbgctl.exe --query-fence
   aerogpu_dbgctl.exe --dump-ring --ring-id 0
   aerogpu_dbgctl.exe --dump-last-submit --cmd-out C:\cmd.bin --alloc-out C:\alloc.bin
   aerogpu_dbgctl.exe --dump-createalloc
   aerogpu_dbgctl.exe --dump-vblank
   aerogpu_dbgctl.exe --query-perf
    ```
4. **If the desktop is frozen but the VM is alive**, dump again (to see if anything advances).

Optional: if you suspect vblank pacing/jitter, sample a few times:

```bat
aerogpu_dbgctl.exe --dump-vblank --vblank-samples 10 --vblank-interval-ms 200
```

If you suspect the OS is not receiving vblank events at all (interrupt/wait wiring issues), measure the WDDM vblank wait path directly:

```bat
aerogpu_dbgctl.exe --wait-vblank --vblank-samples 120 --timeout-ms 2000
```

Follow-up check:

- Run `aerogpu_dbgctl.exe --dump-vblank` and look at `IRQ_ENABLE` and `vblank_interrupt_type`.
  - If `vblank_interrupt_type` is `(not enabled or not reported)` even after calling `--wait-vblank`, dxgkrnl may not be enabling vblank interrupts for the adapter (or the KMD is not seeing `DxgkDdiControlInterrupt` calls / is too old to report it).
  - If `vblank_interrupt_type` is set but `IRQ_ENABLE` does not include `VBLANK`, the KMD is likely not programming the device IRQ enable mask correctly.
  - On Win7/WDDM 1.1, the vblank interrupt type should correspond to `DXGK_INTERRUPT_TYPE_CRTC_VSYNC` (see `drivers/aerogpu/kmd/tools/wdk_abi_probe` if you need the numeric enum value).

And if you suspect scanline/vblank state queries are broken, sample `D3DKMTGetScanLine`:

```bat
aerogpu_dbgctl.exe --query-scanline --vblank-samples 50 --vblank-interval-ms 10
```

If you need to debug a hang or incorrect rendering without attaching WinDbg, capture a last-submission cmd stream + alloc table dump and decode it on the host (see [Dumping the last AeroGPU submission (cmd stream and alloc table)](../windows7-driver-troubleshooting.md#dumping-the-last-aerogpu-submission-cmd-stream-and-alloc-table)).

### 5.3 Suggested `aerogpu_dbgctl` commands (implemented today)

For the canonical, up-to-date command list and global options, see:
`drivers/aerogpu/tools/win7_dbgctl/README.md` (source tree) or `tools\win7_dbgctl\README.md` inside CI packages / Guest Tools.

| Command | What it should report/do | When to use |
|---|---|---|
| `--status` / `--query-version` (alias: `--query-device`) | combined snapshot: device/ABI + UMDRIVERPRIVATE summary + fences + ring0 + scanout0 + cursor (when supported) + vblank + CreateAllocation trace summary | first command in any bug report |
| `--query-umd-private` | KMD-provided `UMDRIVERPRIVATE` blob (ABI + feature discovery used by UMDs) | diagnosing ABI/feature mismatches |
| `--query-segments` | WDDM segment list + segment group budgets (Local/NonLocal) via `D3DKMTQueryAdapterInfo`. Useful for diagnosing allocation budget failures (`E_OUTOFMEMORY` / `D3DERR_OUTOFVIDEOMEMORY`). | diagnosing segment budget / residency issues |
| `--query-fence` | last submitted + last completed fence, plus sticky error counters (`error_irq_count` / `last_error_fence`) | “fence stuck” / device-hung diagnosis |
| `--watch-fence --samples N --interval-ms M [--timeout-ms T]` | polls `--query-fence` in a loop and prints one line per sample (deltas + estimated rate + stall warnings) | quickly confirm whether fences are progressing |
| `--query-perf` (alias: `--perf`) | KMD-provided perf/health counter snapshot (fence/ring progress, submit/IRQ/reset counts, vblank counters) | baseline collection and regression triage |
| `--dump-ring --ring-id N` | ring head/tail + recent submission descriptors (newest-at-tail window on AGPU) | hangs/TDR triage |
| `--dump-last-submit` *(alias: `--dump-last-cmd`)* `--cmd-out <cmd.bin> [--alloc-out <alloc.bin>] [--index-from-tail K] [--count N] [--force]` | dumps the most recent cmd stream submission(s) to file(s); also writes `<cmd_path>.txt` metadata; on AGPU may also dump alloc tables (either the default `<cmd_path>.alloc_table.bin` or the `--alloc-out` path when provided with `--count 1`) (requires `AEROGPU_ESCAPE_OP_READ_GPA`) | capture bytes for offline cmd-stream/alloc-table decode |
| `--watch-ring --samples N --interval-ms M [--ring-id N]` | polls ring head/tail in a loop and prints one line per sample (pending count + last fence/flags when available) | diagnose “ring not draining” / stuck submit paths |
| `--query-scanout` | cached scanout mode/visibility vs best-effort MMIO snapshot (`SCANOUT0_*`, including framebuffer GPA) | diagnosing blank output, mode/pitch mismatches, scanline bounds issues |
| `--dump-scanout-bmp <path>` | dumps the current scanout framebuffer to an uncompressed 32bpp BMP (requires `AEROGPU_ESCAPE_OP_READ_GPA`) | capture pixels without host-side capture (blank/corrupt display debugging) |
| `--dump-scanout-png <path>` | dumps the current scanout framebuffer to a PNG (RGBA8; requires `AEROGPU_ESCAPE_OP_READ_GPA`) | same as above, but PNG output (may be slightly larger than BMP due to uncompressed deflate) |
| `--query-cursor` (alias: `--dump-cursor`) | cursor MMIO state (`CURSOR_*` registers) | diagnosing cursor bring-up issues |
| `--dump-cursor-bmp <path>` | dumps the current cursor image to an uncompressed 32bpp BMP (requires `AEROGPU_ESCAPE_OP_READ_GPA`) | capture cursor pixels (cursor image/pitch/fb_gpa debugging) |
| `--dump-cursor-png <path>` | dumps the current cursor image to a PNG (RGBA8; requires `AEROGPU_ESCAPE_OP_READ_GPA`) | same as above, but PNG output (preserves alpha) |
| `--dump-createalloc` *(aliases: `--dump-createallocation`, `--dump-allocations`)* | recent `DxgkDdiCreateAllocation` trace entries (optionally `--csv <path>` for stable machine parsing) | diagnosing allocation flag mismatches and shared-surface ID issues |
| `--map-shared-handle <HANDLE>` | maps a process-local NT shared handle (section object; common for DXGI shared handles) to a stable 32-bit **debug token** (requires `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE`) | proving cross-process handle duplication/inheritance is working (debug-only; not the protocol `share_token`) |
| `--dump-vblank` (alias: `--query-vblank`) | IRQ enable/status + vblank seq/time/period; prints `vblank_interrupt_type` when dxgkrnl has enabled vblank delivery via `DxgkDdiControlInterrupt` | DWM stutter / Basic fallback |
| `--wait-vblank` | WDDM vblank wait pacing via `D3DKMTWaitForVerticalBlankEvent` (bounded by `--timeout-ms`) | verifying vblank interrupts/waits work |
| `--query-scanline` | `D3DKMTGetScanLine` (scanline + vblank state) | sanity-check scanline/vblank state queries |

Note: commands that read guest memory (`--read-gpa`, `--dump-scanout-bmp`, `--dump-scanout-png`, `--dump-cursor-bmp`, `--dump-cursor-png`, `--dump-last-submit`) rely on
`AEROGPU_ESCAPE_OP_READ_GPA`, which is disabled by default in the Win7 KMD. Enable via
`HKLM\\SYSTEM\\CurrentControlSet\\Services\\aerogpu\\Parameters\\EnableReadGpaEscape = 1` (`REG_DWORD`) and reboot/reload the driver.
The caller must also be privileged (**Administrator** and/or `SeDebugPrivilege`), otherwise the KMD will return `STATUS_NOT_SUPPORTED`.
See `drivers/aerogpu/tools/win7_dbgctl/README.md` for the full gating/security behavior.

Note: `--map-shared-handle` relies on `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE`, which is also disabled by default. Enable via
`HKLM\\SYSTEM\\CurrentControlSet\\Services\\aerogpu\\Parameters\\EnableMapSharedHandleEscape = 1` (`REG_DWORD`) and reboot/reload the driver.
The caller must also be privileged (**Administrator** and/or `SeDebugPrivilege`), otherwise the KMD returns `STATUS_NOT_SUPPORTED`.

Common global options:

- `--timeout-ms N` bounds driver-private escape calls (and each `--wait-vblank` wait).
- `--vblank-samples N` + `--vblank-interval-ms N` let you sample `--dump-vblank` / `--query-scanline` over time.
- `--samples N` + `--interval-ms N` are used by `--watch-fence` and `--watch-ring`.
- `--ring-id N` selects which ring to dump/watch (default: 0).
- `--display \\.\DISPLAY1` selects which adapter to query (use `--list-displays` to enumerate).

The key is: **one command to capture “where are my fences and why aren’t they moving?”**

#### Future work / optional extensions (not implemented by `aerogpu_dbgctl` today)

Some bring-up playbooks mention additional “debug knobs” (runtime log verbosity control, hang injection, forced reset, start/stop perf recording to a file, etc.).
Those are **not implemented** by the in-tree Win7 dbgctl tool today; treat them as driver-specific extensions if/when added.
For lightweight, snapshot-style counters, use `aerogpu_dbgctl.exe --query-perf`.

### 5.4 Common error codes and likely causes

| Symptom | Error / signal | Likely causes | First checks |
|---|---|---|---|
| Toast: “Display driver stopped responding and has recovered” | Event Viewer: **Display** Event ID **4101** | fence/interrupt not progressing, submission too long, deadlock in KMD | `--query-fence`, look for stuck completed fence; verify vblank still ticking |
| Apps start failing after a stutter | `DXGI_ERROR_DEVICE_REMOVED` (`0x887A0005`) | TDR recovery occurred; device reset | check System log for 4101 near the time; ensure reset path restores state |
| A 3D app crashes during heavy draw | `DXGI_ERROR_DEVICE_HUNG` (`0x887A0006`) / `D3DERR_DEVICELOST` | invalid command stream, out-of-bounds memory, shader/translation bug, long-running batch | enable validation in UMD, log last submission bytes/opcodes, bisect by disabling features |
| Allocations fail “too early” (while the guest still has free RAM) | `E_OUTOFMEMORY` / `D3DERR_OUTOFVIDEOMEMORY` | WDDM segment budget too small (AeroGPU’s memory is system-RAM-backed, but dxgkrnl still enforces a segment budget) | increase the reported budget via `HKR\Parameters\NonLocalMemorySizeMB` (see appendix); reboot |
| BSOD during/after TDR | **0x116 VIDEO_TDR_FAILURE** | `ResetFromTimeout` failed or returned inconsistent state | instrument reset path; ensure worker threads stop; avoid touching freed allocations |
| Aero turns off (Basic theme) without a BSOD | “The color scheme has been changed…” notification | DWM device removed/reset, vblank pacing broken, present failures | verify `dwm.exe` still running; check 4101; check `--dump-vblank` cadence (seq/time monotonic) |
| Desktop freezes with no recovery | no logs after a point; hard hang | TdrLevel disabled, deadlock at DISPATCH_LEVEL, interrupt storm, spinlock inversion | re-enable TDR; add watchdog logs; check vblank generator independence from render thread |

### 5.5 Backend/submission failure (AEROGPU_IRQ_ERROR) expectations

If the host backend/emulator detects a submission failure and raises `AEROGPU_IRQ_ERROR`, the Win7 KMD should surface this as a **DMA fault** to dxgkrnl. In a manual repro on a Win7 guest, this typically shows up as:

- Application/device failures such as:
  - `DXGI_ERROR_DEVICE_HUNG` (`0x887A0006`) / `DXGI_ERROR_DEVICE_REMOVED` (`0x887A0005`)
  - `D3DERR_DEVICELOST` (D3D9)
- System log entries consistent with a graphics fault/TDR (often **Display** Event ID **4101** near the time of the error).
- `aerogpu_dbgctl.exe --query-fence` reporting a non-zero `error_irq_count` and a `last_error_fence` near the failing submission.
- `aerogpu_dbgctl.exe --status` or `--query-perf` printing a `Last error:` line with the device-latched error code + fence + error counter when supported by the installed KMD/device ABI (ABI 1.3+).

---

## “Known good” stability recipe (recommended target for first stable milestone)

If you’re unsure what to implement first, aim for this minimal set:

1. **Single display mode** (1024×768 @ 60 Hz) that never blacks out.
2. **Synthetic vblank** at 60 Hz with logging and missed-count tracking.
3. **Small, bounded submissions** (split work; fence per submission; completion <100 ms).
4. A `ResetFromTimeout` implementation that can recover without BSOD.
5. `aerogpu_dbgctl.exe --query-fence/--dump-ring/--dump-vblank` works even after a failure.

Once this recipe is stable, expand: more modes, more features, better pacing accuracy, and higher throughput.

---

## Appendix: WDDM segment budget override (`NonLocalMemorySizeMB`)

AeroGPU is a **system-memory-only** WDDM adapter: allocations are backed by **guest RAM**, not
dedicated VRAM. (The device may still expose BAR1 as a legacy VGA/VBE compatibility aperture.)
However, Win7’s
dxgkrnl still relies on the KMD-reported “non-local” segment size as an **allocation budget**. If the reported budget is too small,
D3D9/D3D11 can fail resource creation with `E_OUTOFMEMORY` / `D3DERR_OUTOFVIDEOMEMORY` even when the guest still has free RAM.

Quick validation:

- Run `drivers/aerogpu/tests/win7/segment_budget_sanity` to print the current WDDM budgets and verify that the KMD-reported
  `NonLocalMemorySize` matches your configured `NonLocalMemorySizeMB` (after reboot / device restart).
- If you installed Aero Guest Tools, re-run `verify.cmd` and check `C:\AeroGuestTools\report.txt` / `report.json` for the effective
  `NonLocalMemorySizeMB` value (after clamping).

The AeroGPU Win7 KMD allows overriding this budget via a device registry parameter:

- **Key:** `HKR\Parameters\NonLocalMemorySizeMB`
- **Type:** `REG_DWORD`
- **Unit:** megabytes (MB)
- **Default:** 512
- **Clamped:** min 128; max 2048 on x64; max 1024 on x86

Notes:

- This is a **budget hint** only; it does **not** reserve memory up front.
- Setting it too high can increase guest RAM consumption and paging pressure under heavy workloads.

### When to tune it

Consider increasing `NonLocalMemorySizeMB` when:

- D3D workloads that allocate many large textures/buffers fail early with out-of-memory style errors, and
- the guest still has significant free RAM (i.e., you are hitting the *budget*, not real exhaustion).

Recommended starting points:

- **Win7 x64:** 1024–2048 (depending on guest RAM and workload)
- **Win7 x86:** 256–1024 (larger values are clamped to 1024)

### How to set it (example)

On Win7, `HKR` maps to the AeroGPU adapter’s driver key under the display class GUID:

`HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\000X\`

Create/open a `Parameters` subkey and set `NonLocalMemorySizeMB`. Example (replace `000X`):

```bat
reg add "HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\000X\Parameters" ^
  /v NonLocalMemorySizeMB /t REG_DWORD /d 2048 /f
```

Reboot the guest (or disable/enable the AeroGPU device) for the new budget to take effect.
