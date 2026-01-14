# AeroGPU Win7 debug/control utility (Escape-based)

`aerogpu_dbgctl.exe` is a small user-mode console tool intended for bring-up and debugging of the AeroGPU WDDM kernel-mode driver (KMD) on **Windows 7 SP1**.

It talks to the installed AeroGPU driver via **`DxgkDdiEscape`** using `D3DKMTEscape` (driver-private escape packets).

**Bitness policy:** dbgctl is built and shipped as a single **x86** executable so it runs on both Win7 x86 and Win7 x64
(via WOW64). Do not ship an x64-only dbgctl binary; it will not run on Win7 x86.

## Where to find `aerogpu_dbgctl.exe` (packaged outputs)

If you are using CI-produced artifacts or Guest Tools (instead of building dbgctl from source), the binary is shipped
alongside the AeroGPU driver package:

- Guest Tools ISO/zip:
  - `<GuestToolsDrive>:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`
  - `<GuestToolsDrive>:\drivers\x86\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`
  - Optional top-level tools payload (when present):
    - `<GuestToolsDrive>:\tools\aerogpu_dbgctl.exe`
    - `<GuestToolsDrive>:\tools\<arch>\aerogpu_dbgctl.exe`
- CI-staged driver packages:
  - `out\packages\aerogpu\x64\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`
  - `out\packages\aerogpu\x86\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe`

Example (run directly from a mounted Guest Tools ISO/zip; Win7 x64; replace `<GuestToolsDrive>` with your drive letter, e.g. `D`):

```bat
cd /d <GuestToolsDrive>:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin
aerogpu_dbgctl.exe --status
```

If your Guest Tools ISO is mounted as `X:` (common), these are copy/pastable:

```bat
:: Win7 x64:
X:\drivers\amd64\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --status
:: Win7 x86:
X:\drivers\x86\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --status
```

In this README, examples use `aerogpu_dbgctl ...` for brevity; if the tool is not on `PATH`, run it as `aerogpu_dbgctl.exe ...` from one of the directories above.

## Supported device models / ABIs

The in-tree AeroGPU Win7 KMD supports both the **versioned** and **legacy bring-up** AeroGPU PCI devices, auto-detected
via BAR0 MMIO magic:

- **Versioned ABI device**: `PCI\VEN_A3A0&DEV_0001` ("AGPU")  
  Ring = `aerogpu_ring_header` + `aerogpu_submit_desc` slots.
- **Legacy bring-up ABI device**: legacy bring-up PCI identity ("ARGP", deprecated; see `docs/abi/aerogpu-pci-identity.md`)  
  Ring = legacy `aerogpu_legacy_ring_entry` entries (see `drivers/aerogpu/kmd/include/aerogpu_legacy_abi.h` and `docs/abi/aerogpu-pci-identity.md`).
  Note: the emulator legacy device model is optional (feature `emulator/aerogpu-legacy`).

Note: the shipped Win7 driver packages (`drivers/aerogpu/packaging/win7`) bind to the canonical `PCI\VEN_A3A0&DEV_0001`
HWID only. Installing against the legacy bring-up device model requires the legacy INFs under
`drivers/aerogpu/packaging/win7/legacy/` and enabling the emulator legacy device model (feature `emulator/aerogpu-legacy`).

Most commands rely on dbgctl escape packets implemented by the installed KMD (for example `--query-version` (alias:
`--query-device`), `--query-fence`, `--dump-ring`, etc.). The tool is primarily developed against the versioned ("AGPU")
path, but
keeps best-effort compatibility decoding for legacy ("ARGP") devices and older KMD builds.

## Features

Minimum supported commands:

- `aerogpu_dbgctl --list-displays`  
  Prints the available `\\.\DISPLAY*` names to use with `--display`.

- `aerogpu_dbgctl --query-version` (alias: `--query-device`)  
  Prints the detected AeroGPU device ABI (**legacy ARGP** vs **new AGPU**), ABI version, and (when exposed) device feature bits.
  Also prints an UMDRIVERPRIVATE summary (type + ABI + features), a fence snapshot, a ring0 snapshot, a scanout0 snapshot (cached vs MMIO),
  a last-error snapshot (when supported; includes `latched=true/false` on newer KMDs),
  a cursor MMIO summary (when supported),
  a scanout0 vblank timing snapshot (when available), and a short CreateAllocation trace summary (`write_index` / `entry_count`).

- `aerogpu_dbgctl --status` *(alias for `--query-version`)*  
  Prints a short combined snapshot (device/ABI + UMDRIVERPRIVATE summary + fences + ring0 + scanout0 + cursor + scanout0 vblank + CreateAllocation trace summary).

- `aerogpu_dbgctl --query-umd-private`  
  Calls `D3DKMTQueryAdapterInfo(KMTQAITYPE_UMDRIVERPRIVATE)` and prints the `aerogpu_umd_private_v1` blob used by UMDs to discover the active ABI + feature bits.

- `aerogpu_dbgctl --query-segments`  
  Calls `D3DKMTQueryAdapterInfo(KMTQAITYPE_QUERYSEGMENT)` and `D3DKMTQueryAdapterInfo(KMTQAITYPE_GETSEGMENTGROUPSIZE)` to print the WDDM segment list and segment group budgets (Local/NonLocal memory sizes).

- `aerogpu_dbgctl --query-fence`  
  Prints the last submitted fence and last completed fence, plus sticky error
  counters (`error_irq_count` / `last_error_fence`) when supported by the KMD.
  On newer KMDs, this command also prints whether the device error is currently
  latched (device-lost state; best-effort via `--query-error`).

- `aerogpu_dbgctl --query-error`  
  Dumps the last recorded device error snapshot via `AEROGPU_ESCAPE_OP_QUERY_ERROR`, including:
  - whether the device error is currently latched (`latched=true/false` on newer KMDs)
  - an error code (when available; ABI 1.3+ devices expose `ERROR_CODE`)
  - the associated fence (when available; ABI 1.3+ devices expose `ERROR_FENCE`)
  - an error count (when available; ABI 1.3+ devices expose `ERROR_COUNT` and the KMD also tracks `error_irq_count`)

- `aerogpu_dbgctl --watch-fence --samples N --interval-ms M [--timeout-ms T]`  
  Polls `--query-fence` in a loop and prints **one line per sample** with:
  - the current submitted/completed fences,
  - deltas since the previous sample,
  - an observed completed-fence rate (fences/sec), and
  - a stall warning if the completed fence does not advance for multiple intervals while work is pending, and
  - sticky IRQ_ERROR diagnostics (`error_irq_count` / `last_error_fence`) and the current device error latch state (`latched=true/false`
    when supported by the installed KMD).

- `aerogpu_dbgctl --query-perf` *(alias: `--perf`)*  
  Dumps a KMD-provided perf/health counter snapshot, including:
  - fence + ring progress (ring0 head/tail),
  - submit counters (total / render / present / internal),
  - submit-path contiguous allocation pool counters (hit/miss/bytes_saved, when supported),
  - `DxgkDdiGetScanLine` (GetRasterStatus) telemetry (cache hits vs MMIO polls) when supported (DBG-only on newer KMDs),
  - ring push failures (submission path failures before reaching the device),
  - IRQ delivery counters (fence/vblank/spurious + error IRQ snapshot when available; dbgctl may fall back to `--query-fence` on older builds),
  - reset counts, vblank counters, and selftest stats (count + last error when available), and
  - sticky device error state (`device_error.latched` + `device_error.last_time_10ms`; packed into `reserved0` for ABI stability).
  It also prints a best-effort last-error snapshot (QUERY_ERROR) when supported; on newer KMDs this includes
  `latched=true/false` to distinguish the current device-lost state from historical error counters.
  On older KMD builds this may print `(not supported)`; upgrade the driver to enable it.

- `aerogpu_dbgctl --query-scanout`  
  Dumps scanout 0 state as seen by the KMD, including:
  - cached mode (`CurrentWidth/Height/Format/Pitch`) and visibility (`enable`)
  - best-effort MMIO snapshot (`SCANOUT0_*` registers), including the current framebuffer GPA
  
  Useful for diagnosing mode/pitch mismatches (e.g. scanline bounds issues or a blank display even though fences advance).

- `aerogpu_dbgctl --dump-scanout-bmp <path>`  
  Dumps the current scanout framebuffer to an **uncompressed 32bpp BMP** by:
  1) querying scanout state via `AEROGPU_ESCAPE_OP_QUERY_SCANOUT`, then
  2) reading framebuffer bytes from the reported `fb_gpa` via `AEROGPU_ESCAPE_OP_READ_GPA`, row-by-row (respecting pitch).
   
  Useful for diagnosing pitch/format/fb_gpa bugs **without screen capture / RDP**.
  Note: this requires the installed KMD to support `AEROGPU_ESCAPE_OP_READ_GPA` and have it enabled (see “Escape gating / security gating” below);
  if disabled/unsupported, dbgctl will fail with `STATUS_NOT_SUPPORTED` (`0xC00000BB`).
   
  Supported formats include:
  - `B8G8R8X8_UNORM` / `B8G8R8A8_UNORM`
  - `R8G8B8X8_UNORM` / `R8G8B8A8_UNORM`
  - `*_SRGB` variants of the above
  - `B5G6R5_UNORM`
  - `B5G5R5A1_UNORM`

- `aerogpu_dbgctl --dump-scanout-png C:\scanout.png`  
  Same as `--dump-scanout-bmp`, but writes a PNG (RGBA8). The encoder uses stored (uncompressed) deflate blocks for simplicity,
  so the PNG may be slightly larger than the BMP.

- `aerogpu_dbgctl --query-cursor` *(alias: `--dump-cursor`)*  
  Dumps cursor MMIO state (`CURSOR_*` registers), including:
  - enable, position, hot spot
  - size, format, pitch, framebuffer GPA
  
  Useful for diagnosing Win7 hardware cursor bring-up (e.g. cursor enabled but off-screen, wrong hot spot, wrong pitch).

- `aerogpu_dbgctl --dump-cursor-bmp C:\cursor.bmp`  
  Dumps the current cursor framebuffer contents to an **uncompressed 32bpp BMP** using:
  - `AEROGPU_ESCAPE_OP_QUERY_CURSOR` to discover width/height/format/pitch/fb_gpa
  - `AEROGPU_ESCAPE_OP_READ_GPA` to read cursor bytes from guest physical memory
   
  Useful for diagnosing cursor image/pitch/fb_gpa bugs without relying on host-side captures.
  Note: this requires the installed KMD to support `AEROGPU_ESCAPE_OP_READ_GPA` and have it enabled (see “Escape gating / security gating” below);
  if disabled/unsupported, dbgctl will fail with `STATUS_NOT_SUPPORTED` (`0xC00000BB`).

- `aerogpu_dbgctl --dump-cursor-png C:\cursor.png`  
  Same as `--dump-cursor-bmp`, but writes a PNG (RGBA8; preserves alpha).

- `aerogpu_dbgctl --read-gpa GPA --size N [--out FILE] [--force]`  
  `aerogpu_dbgctl --read-gpa GPA N [--out FILE] [--force]`  
  Reads a **bounded** slice of guest physical memory (GPA) from buffers that the KMD/device tracks (for example: scanout framebuffer,
  cursor framebuffer, ring buffers, and driver-owned DMA buffers for pending submissions).
   
  Note: this requires the installed KMD to support `AEROGPU_ESCAPE_OP_READ_GPA` and have it enabled (see “Escape gating / security gating” below);
  if disabled/unsupported, dbgctl will fail with `STATUS_NOT_SUPPORTED` (`0xC00000BB`).
  
  Notes:
  - The KMD enforces a per-escape maximum of `AEROGPU_DBGCTL_READ_GPA_MAX_BYTES` (currently 4096 bytes); dbgctl chunks reads when `--out` is used.
  - Without `--out`, dbgctl prints up to 256 bytes by default; use `--force` to print up to 4096 bytes.
  - With `--out`, dbgctl writes the full requested range to a file.
  - On failure (including `STATUS_PARTIAL_COPY`), dbgctl best-effort deletes the `--out` path so callers do not see partial/truncated artifacts.
  - With `--json`, `data_hex` is capped to a bounded prefix; see `request.size_bytes_effective` and `response.truncated`.
    If the KMD returns `STATUS_SUCCESS` but copies fewer bytes than requested, dbgctl treats it as an error (`ok:false`, `response.short_read:true`).

- `aerogpu_dbgctl --dump-ring`  
  Dumps ring head/tail + recent submissions. Fields include:
  - `signal_fence`
  - `cmd_gpa` / `cmd_size_bytes`
  - `flags`
  - (AGPU only) `alloc_table_gpa` / `alloc_table_size_bytes`
  
  Note: for the AGPU (versioned) ring format, the v2 dump returns a **recent tail window** of descriptors ending at `tail-1`
  (newest is `desc[desc_count-1]`). This is intentionally not limited to the pending `[head, tail)` region so very fast
  devices/emulators still expose the most recent submission(s) to tooling/tests.

- `aerogpu_dbgctl --dump-last-submit` *(alias: `--dump-last-cmd`)*  
  `--cmd-out <path> [--alloc-out <path>] [--index-from-tail K] [--count N] [--force]`
  Dumps the raw bytes of the most recent command stream buffer (`cmd_gpa .. cmd_gpa+cmd_size_bytes`) from the ring
  into `<path>` (binary). Use `--index-from-tail K` to select older submissions (0 = newest).
  For backwards compatibility with older usage, `--out <path>` is also accepted as an alias for `--cmd-out <path>`
  for this command (for example: `--dump-last-cmd --out C:\\cmd.bin`).
  Use `--count N` to dump the last N submissions in one run (starting at `index_from_tail=K`).
  When dumping multiple submissions, `<path>` is treated as a base and output files are written as
  `<base>_<index_from_tail>.bin` (for example `last_cmd_0.bin`, `last_cmd_1.bin`). If `<path>` ends in `.bin`,
  the trailing `.bin` is stripped before appending `_<index_from_tail>.bin`.
  On AGPU rings, if the submission has an allocation table (`alloc_table_gpa/alloc_table_size_bytes`), it is also dumped
  to `<cmd_path>.alloc_table.bin` (one file per dumped submission). Alternatively, for a single-submission dump
  (`--count 1`), you can override the alloc table output path via `--alloc-out <path>`.
  If the selected submission has no alloc table (or the ring format does not expose it), and `--alloc-out` is provided,
  dbgctl writes an empty file to `--alloc-out` for scripting convenience.

  dbgctl also writes a small metadata summary to `<cmd_path>.txt` (ring/fence/GPAs/sizes) when possible (one file per dumped submission).
  Note: this is appended to the full cmd path, so dumping `last_cmd_0.bin` produces `last_cmd_0.bin.txt`.

  Note: this requires the installed KMD to support `AEROGPU_ESCAPE_OP_READ_GPA` and have it enabled (see “Escape gating / security gating” below);
  if disabled/unsupported, dbgctl will fail with `STATUS_NOT_SUPPORTED` (`0xC00000BB`).

  Safety: by default dbgctl refuses to dump buffers larger than 1 MiB; use `--force` to override.
  On failure while dumping bytes, dbgctl best-effort deletes partially-written `.bin` outputs (`--cmd-out` / `--alloc-out`).

- `aerogpu_dbgctl --watch-ring --samples N --interval-ms M [--ring-id N]`  
  Polls ring head/tail in a loop and prints **one line per sample** with:
  - `head`, `tail`, and `pending` (queue depth, computed as `tail-head` / best-effort legacy wraparound),
  - `d_pending` (delta pending since the previous sample),
  - a stall warning if `pending` stays non-zero and unchanged for multiple intervals, and
  - (when available) the newest descriptor's `fence` and `flags` for quick correlation with fence progression.
  
  Useful for diagnosing whether the emulator/backend is draining submissions (head catches up to tail) and whether the guest is over-submitting.
- `aerogpu_dbgctl --dump-createalloc` *(aliases: `--dump-createallocation`, `--dump-allocations`)*  
  Dumps a small KMD-maintained ring buffer of recent `DxgkDdiCreateAllocation` events, including:
  - the incoming `DXGK_ALLOCATIONINFO::Flags.Value` from dxgkrnl/runtime
  - the final flags after the miniport applies required bits (currently `CpuVisible` + `Aperture`)

  Structured export:
  - `--csv <path>` writes a stable, machine-parseable CSV file (header row + one row per trace entry), including `write_index` and `entry_capacity` metadata for wrap-around detection.
  - For JSON output, use the global `--json[=PATH]` flag (for example: `aerogpu_dbgctl --dump-createalloc --json=C:\\createalloc.json`).

- `aerogpu_dbgctl --dump-vblank`  
  Dumps vblank timing counters (seq/last time/period) and IRQ status/enable masks.
  Works on both legacy and new AeroGPU devices as long as the device exposes
  `AEROGPU_FEATURE_VBLANK` in `FEATURES_LO/HI`.
  When available, it also prints `vblank_interrupt_type` (the `DXGK_INTERRUPT_TYPE`
  value dxgkrnl enabled via `DxgkDdiControlInterrupt`). On Win7/WDDM 1.1 this should
  correspond to `DXGK_INTERRUPT_TYPE_CRTC_VSYNC` (see `drivers/aerogpu/kmd/tools/wdk_abi_probe`
  if you need the numeric enum value).
  Use `--vblank-samples` to observe changes over time and estimate the effective Hz/jitter.
  If `vblank_seq` stays at 0, ensure scanout is enabled/visible (some device models only tick vblank
  counters while scanout is enabled).

  Alias: `aerogpu_dbgctl --query-vblank`

- `aerogpu_dbgctl --wait-vblank`  
  Calls `D3DKMTWaitForVerticalBlankEvent` in a loop (with `--timeout-ms` safety timeout) and prints the observed pacing.

- `aerogpu_dbgctl --query-scanline`  
  Calls `D3DKMTGetScanLine` and prints the current scanline and vblank state.

- `aerogpu_dbgctl --map-shared-handle HANDLE`  
  Calls the KMD escape `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE` to map a process-local Win32 shared handle to a stable 32-bit **debug token**.
  Note: this is *not* the `u64 share_token` used by `EXPORT_SHARED_SURFACE` / `IMPORT_SHARED_SURFACE`.
  Also note: the handle must be valid in the `aerogpu_dbgctl.exe` process (inherit/duplicate the handle into this process if needed).
  Note: this escape is disabled by default; see “Escape gating / security gating” below.

- `aerogpu_dbgctl --selftest`  
  Triggers a simple KMD-side self-test.
  The KMD currently validates:
  - ring submission + ring head advancement (basic GPU forward progress), and
  - when `AEROGPU_FEATURE_VBLANK` is present and scanout is enabled:
    - vblank sequence counter advances (monotonic),
    - vblank IRQ enable/latch/ack logic works (best-effort), and
    - vblank IRQ delivery reaches the ISR and DPC (IRQ plumbing sanity),
  - when `AEROGPU_FEATURE_CURSOR` is present: cursor MMIO regs are in-range and read back after a small test config write.
  Exit code is **0** on PASS; on failure it returns the KMD-provided selftest error code (`error_code`).
  If the escape transport fails (e.g. `D3DKMTEscape` / `D3DKMTCloseAdapter` failure), it returns **254**.
  If `--timeout-ms` is too small for all subtests to run, the KMD may fail with `TIME_BUDGET_EXHAUSTED`.

### Escape gating / security gating (`--read-gpa` and `--map-shared-handle`)

`--read-gpa` and dependent commands (`--dump-scanout-bmp`, `--dump-scanout-png`, `--dump-cursor-bmp`, `--dump-cursor-png`,
`--dump-last-cmd`, `--dump-last-submit`) are intentionally **locked down**:

- Require an explicit registry opt-in:
  - `HKLM\\SYSTEM\\CurrentControlSet\\Services\\aerogpu\\Parameters\\EnableReadGpaEscape = 1` (REG_DWORD)
- The caller must be privileged (**Administrator** and/or have **SeDebugPrivilege** enabled).
- If not enabled/authorized, the KMD returns `STATUS_NOT_SUPPORTED` (`0xC00000BB`).
- Note: since this is loaded from the miniport service key in `DriverEntry`, you typically need to reboot (or otherwise fully reload the driver)
  after changing it.

### Security gating for `--map-shared-handle`

`--map-shared-handle` (dbgctl escape `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE`) is also gated:

- Require an explicit registry opt-in:
  - `HKLM\\SYSTEM\\CurrentControlSet\\Services\\aerogpu\\Parameters\\EnableMapSharedHandleEscape = 1` (REG_DWORD)
- The caller must be privileged (**Administrator** and/or have **SeDebugPrivilege** enabled).
- If not enabled/authorized, the KMD returns `STATUS_NOT_SUPPORTED` (`0xC00000BB`).

## Usage

```
aerogpu_dbgctl [--display \\.\DISPLAY1] [--ring-id N] [--timeout-ms N] [--json[=PATH]] [--pretty] \
               [--vblank-samples N] [--vblank-interval-ms N] \
               [--samples N] [--interval-ms N] \
               [--size N] [--out FILE] [--count N] [--force] [--cmd-out FILE] [--alloc-out FILE] <command>
```

Examples:

```
aerogpu_dbgctl --help
aerogpu_dbgctl --list-displays
aerogpu_dbgctl --status
aerogpu_dbgctl --status --json
aerogpu_dbgctl --status --pretty
aerogpu_dbgctl --status --json=C:\\status.json
aerogpu_dbgctl --query-version
aerogpu_dbgctl --query-device
aerogpu_dbgctl --query-umd-private
aerogpu_dbgctl --query-segments
aerogpu_dbgctl --query-fence
aerogpu_dbgctl --watch-fence --samples 120 --interval-ms 250 --timeout-ms 30000
aerogpu_dbgctl --query-perf
aerogpu_dbgctl --query-scanout
aerogpu_dbgctl --dump-scanout-bmp C:\scanout.bmp
aerogpu_dbgctl --dump-scanout-png C:\scanout.png
aerogpu_dbgctl --read-gpa 0x12340000 --size 256
aerogpu_dbgctl --read-gpa 0x12340000 65536 --out C:\dump.bin
aerogpu_dbgctl --query-cursor
aerogpu_dbgctl --dump-cursor-bmp C:\cursor.bmp
aerogpu_dbgctl --dump-cursor-png C:\cursor.png
aerogpu_dbgctl --dump-ring --ring-id 0
aerogpu_dbgctl --watch-ring --ring-id 0 --samples 200 --interval-ms 50
aerogpu_dbgctl --dump-last-submit --cmd-out last_cmd.bin --alloc-out last_alloc.bin
aerogpu_dbgctl --dump-last-submit --count 4 --cmd-out last_cmd.bin
aerogpu_dbgctl --dump-last-submit --index-from-tail 1 --cmd-out prev_cmd.bin
aerogpu_dbgctl --dump-createalloc
aerogpu_dbgctl --dump-createalloc --csv C:\createalloc.csv
aerogpu_dbgctl --dump-createalloc --json C:\createalloc.json
aerogpu_dbgctl --dump-vblank
aerogpu_dbgctl --dump-vblank --vblank-samples 10 --vblank-interval-ms 200
aerogpu_dbgctl --wait-vblank --vblank-samples 120 --timeout-ms 2000
aerogpu_dbgctl --query-scanline --vblank-samples 20 --vblank-interval-ms 10
aerogpu_dbgctl --map-shared-handle 0x1234
aerogpu_dbgctl --selftest --timeout-ms 2000
```

Example output excerpt (`--status`):

```
AeroGPU features:
  raw: lo=0x1f hi=0x0
  decoded: cursor, scanout, vblank, fence_page, transfer
Last error: code=0 (NONE) fence=0x0 count=0
```

Notes:
- Unknown feature bits are reported as `unknown_bit_<n>` (bit index in the 128-bit `FEATURES_LO/HI` set).
- `IRQ_ACTIVE` is `IRQ_STATUS & IRQ_ENABLE` (i.e. causes that can currently assert the interrupt line).
- Some environments may return a non-zero `VidPnSourceId` from `D3DKMTOpenAdapterFromHdc`; dbgctl retries `--dump-vblank`, `--wait-vblank`, and `--query-scanline` with source 0 if needed (AeroGPU currently implements a single source).
- If `vblank_interrupt_type` prints `(not enabled or not reported)`, either dxgkrnl has not enabled vblank interrupt delivery for the adapter yet *or* the installed KMD predates `vblank_interrupt_type` reporting.
- `--timeout-ms` also bounds driver-private `D3DKMTEscape` calls used by commands like `--query-fence` / `--dump-ring`, `D3DKMTQueryAdapterInfo` calls used by `--query-umd-private`, `D3DKMTGetScanLine` calls used by `--query-scanline`, and adapter open/close calls (`D3DKMTOpenAdapterFromHdc` / `D3DKMTCloseAdapter`). On timeout, dbgctl may skip `D3DKMTCloseAdapter` to avoid deadlocking inside a stuck kernel thunk.

## Manual validation: dumping scanout framebuffer bytes

The most common use of `--read-gpa` is to inspect the scanout framebuffer without a kernel debugger:

1. Run `aerogpu_dbgctl --query-scanout` and note `mmio_fb_gpa` and `mmio_pitch_bytes`.
2. Dump a small header slice (first cache line/page):
   ```
   aerogpu_dbgctl --read-gpa <mmio_fb_gpa> --size 256
   ```
3. Optionally dump a full page to a file for offline analysis:
   ```
   aerogpu_dbgctl --read-gpa <mmio_fb_gpa> --size 4096 --force --out scanout_page0.bin
   ```

If scanout is configured for `B8G8R8X8`, the first pixels should match the expected desktop contents (little-endian BGRA/XRGB).
If the dump is all zeros, check that scanout is enabled/visible and that `mmio_fb_gpa` is non-zero.

## Manual validation: dumping cursor framebuffer bytes

`--read-gpa` can also be used to inspect the hardware cursor backing store:

1. Run `aerogpu_dbgctl --query-cursor` and note `fb_gpa`, `pitch`, and `size` (`width`×`height`).
2. Dump a small header slice:
   ```
   aerogpu_dbgctl --read-gpa <fb_gpa> --size 256
   ```
3. If you need more than 256 bytes (up to 4096), use `--force`:
   ```
   aerogpu_dbgctl --read-gpa <fb_gpa> --size 4096 --force --out cursor_page0.bin
   ```

Note: the installed KMD may disable hardware cursor support depending on the device feature bits; in that case
`--query-cursor` will report `(not supported)` or a zero GPA.

## JSON output (`--json[=PATH]`)

`aerogpu_dbgctl` can emit **machine-readable JSON** to enable automation (CI/test runners) without fragile text parsing.

- `--json` prints JSON to stdout.
- `--json=PATH` writes JSON to `PATH` (UTF-8).
- `--pretty` pretty-prints JSON (implies `--json`).

`--status --json` is intended as a one-shot “bug report snapshot” and includes nested sections like:
`device`, `features`, `fences`, `perf`, `ring0`, `scanout0`, `cursor`, `vblank`, `last_error`, and `createallocation_trace`.

### Schema stability

All JSON outputs include:

```json
{ "schema_version": 1, "command": "<name>", "ok": true }
```

- `schema_version` is incremented only for **breaking schema changes**.
- New fields may be added over time; existing fields keep their meaning.
- 64-bit counters are emitted as strings or `{ "hex": "...", "dec": "..." }` objects to avoid precision loss in JS runtimes.
- On failure, `ok` is `false`. For parse/transport failures, an `error` object is present. Some commands also expose a command-specific status field (for example `read-gpa.response.status`).
- If argument parsing/usage fails and `--json`/`--pretty` is present anywhere on the command line, dbgctl still emits a JSON error object (so automation does not need to scrape usage text). When no specific command is selected yet, it uses `command: "parse-args"`; otherwise it may use a command-specific value like `command: "read-gpa"` or `command: "watch-ring"`.
- For bounded “watch” commands, JSON output is capped to 10,000 samples to avoid excessive memory usage (dbgctl builds the full JSON payload in memory).

### JSON-supported commands

This tool currently supports JSON output for snapshot-style commands and other bounded-output commands:

- `--help` (also `-h`, `/?`)
- `--status` (aliases: `--query-version`, `--query-device`)
- `--query-fence`
- `--query-error`
- `--watch-fence`
- `--query-perf`
- `--query-umd-private`
- `--query-segments`
- `--query-scanout`
- `--dump-scanout-bmp`, `--dump-scanout-png`
- `--query-cursor`
- `--dump-cursor-bmp`, `--dump-cursor-png`
- `--dump-ring`
- `--watch-ring`
- `--dump-last-submit` (alias: `--dump-last-cmd`)
- `--dump-createalloc` (still supports `--csv`)
- `--dump-vblank`
- `--wait-vblank`
- `--query-scanline`
- `--map-shared-handle`
- `--read-gpa`
- `--selftest`
- `--list-displays`

Notes:
- `--query-perf --json` includes a best-effort `last_error` object (via `AEROGPU_ESCAPE_OP_QUERY_ERROR`) when supported by the installed KMD.
- `--query-perf --json` includes a `contig_pool` object when the installed KMD exposes the appended contig-pool fields.
- `--query-perf --json` includes a `get_scanline` object when the installed KMD exposes the appended GetScanLine counters (DBG-only on newer KMDs).

## Build (Windows 7)

This tool intentionally **does not require the WDK**: it dynamically loads the required `D3DKMT*` entrypoints from `gdi32.dll`.

### Build with the Visual Studio Developer Command Prompt

From the repo root:

```
cd drivers\aerogpu\tools\win7_dbgctl
build_vs2010.cmd
```

Or build directly with `cl.exe`:

```
cd drivers\aerogpu\tools\win7_dbgctl
cl /nologo /W4 /EHsc /DUNICODE /D_UNICODE ^
  /I ..\..\protocol ^
  src\aerogpu_dbgctl.cpp ^
  /Feaerogpu_dbgctl.exe ^
  user32.lib gdi32.lib ^
  /link /MACHINE:X86
```

Outputs:

- `build_vs2010.cmd` → `drivers\\aerogpu\\tools\\win7_dbgctl\\bin\\aerogpu_dbgctl.exe`
- direct `cl` invocation → `drivers\\aerogpu\\tools\\win7_dbgctl\\aerogpu_dbgctl.exe`

## Protocol

Packet definitions consumed by this tool live in:

- `drivers/aerogpu/protocol/aerogpu_escape.h` (base Escape header + `AEROGPU_ESCAPE_OP_QUERY_DEVICE` fallback)
- `drivers/aerogpu/protocol/aerogpu_dbgctl_escape.h` (dbgctl ops, including `AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2`)

The AeroGPU KMD is expected to implement `DxgkDdiEscape` handling for these packets (driver-private escape).

Escape ops used:

- `AEROGPU_ESCAPE_OP_QUERY_DEVICE_V2` (fallback: `AEROGPU_ESCAPE_OP_QUERY_DEVICE`) → `--query-version` / `--query-device`
- `AEROGPU_ESCAPE_OP_QUERY_FENCE` → `--query-fence`, `--watch-fence`
- `AEROGPU_ESCAPE_OP_QUERY_PERF` → `--query-perf`
- `AEROGPU_ESCAPE_OP_QUERY_ERROR` → reported inline by `--status` and `--query-perf` (last latched device error, if supported)
- `AEROGPU_ESCAPE_OP_QUERY_SCANOUT` → `--query-scanout`, `--dump-scanout-bmp`, `--dump-scanout-png`
- `AEROGPU_ESCAPE_OP_QUERY_CURSOR` → `--query-cursor`, `--dump-cursor-bmp`, `--dump-cursor-png`
- `AEROGPU_ESCAPE_OP_DUMP_RING_V2` (fallback: `AEROGPU_ESCAPE_OP_DUMP_RING`) → `--dump-ring`, `--watch-ring`, `--dump-last-cmd`, `--dump-last-submit`
- `AEROGPU_ESCAPE_OP_READ_GPA` → `--read-gpa`, `--dump-scanout-bmp`, `--dump-scanout-png`, `--dump-cursor-bmp`, `--dump-cursor-png`, `--dump-last-cmd`, `--dump-last-submit`
- `AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION` → `--dump-createalloc`
- `AEROGPU_ESCAPE_OP_QUERY_VBLANK` (alias: `AEROGPU_ESCAPE_OP_DUMP_VBLANK`) → `--dump-vblank`
- `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE` → `--map-shared-handle`
- `AEROGPU_ESCAPE_OP_SELFTEST` → `--selftest`

Additional WDDM queries (do not use the escape channel):

- `--query-umd-private` uses `D3DKMTQueryAdapterInfo(KMTQAITYPE_UMDRIVERPRIVATE)` to query the KMD-provided discovery blob.
- `--query-segments` uses `D3DKMTQueryAdapterInfo(KMTQAITYPE_QUERYSEGMENT)` and `D3DKMTQueryAdapterInfo(KMTQAITYPE_GETSEGMENTGROUPSIZE)` to query the KMD-advertised segment list and segment group sizes.
- `--wait-vblank` uses `D3DKMTWaitForVerticalBlankEvent` to measure vblank delivery from the OS.
- `--query-scanline` uses `D3DKMTGetScanLine` to report the current scanline and vblank state.

## Notes / troubleshooting

- If `D3DKMTOpenAdapterFromHdc` or `D3DKMTEscape` cannot be resolved from `gdi32.dll`, the OS is too old or the environment is not WDDM-capable.
- If `D3DKMTEscape` returns an error, ensure the AeroGPU driver is installed and exposes the required escapes.
- If `--wait-vblank` times out, the process may skip `D3DKMTCloseAdapter` to avoid deadlocking on broken vblank implementations.
- For the cmd-stream dump + decode workflow used to debug hangs/incorrect rendering without attaching WinDbg, see:
  - [`docs/windows7-driver-troubleshooting.md` (dumping the last submission)](../../../../docs/windows7-driver-troubleshooting.md#dumping-the-last-aerogpu-submission-cmd-stream-and-alloc-table)

## Not implemented (intentional)

The in-tree Win7 dbgctl tool is focused on snapshots/queries and compatibility with older KMD builds.
It does **not** currently provide:

- runtime log verbosity controls
- long-running perf capture / start-stop recording (dbgctl only supports snapshot-style counters via `--query-perf`)
- hang injection or forced reset helpers

If/when we add those as new driver-private escape ops, this README should be updated alongside the implementation.
