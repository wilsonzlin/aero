# AeroGPU Win7 debug/control utility (Escape-based)

`aerogpu_dbgctl.exe` is a small user-mode console tool intended for bring-up and debugging of the AeroGPU WDDM kernel-mode driver (KMD) on **Windows 7 SP1**.

It talks to the installed AeroGPU driver via **`DxgkDdiEscape`** using `D3DKMTEscape` (driver-private escape packets).

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
  a cursor MMIO summary (when supported),
  a scanout0 vblank timing snapshot (when available), and a short CreateAllocation trace summary (`write_index` / `entry_count`).

- `aerogpu_dbgctl --status` *(alias for `--query-version`)*  
  Prints a short combined snapshot (device/ABI + UMDRIVERPRIVATE summary + fences + ring0 + scanout0 + cursor + scanout0 vblank + CreateAllocation trace summary).

- `aerogpu_dbgctl --query-umd-private`  
  Calls `D3DKMTQueryAdapterInfo(KMTQAITYPE_UMDRIVERPRIVATE)` and prints the `aerogpu_umd_private_v1` blob used by UMDs to discover the active ABI + feature bits.

- `aerogpu_dbgctl --query-fence`  
  Prints the last submitted fence and last completed fence.

- `aerogpu_dbgctl --watch-fence --samples N --interval-ms M [--timeout-ms T]`  
  Polls `--query-fence` in a loop and prints **one line per sample** with:
  - the current submitted/completed fences,
  - deltas since the previous sample,
  - an observed completed-fence rate (fences/sec), and
  - a stall warning if the completed fence does not advance for multiple intervals while work is pending.

- `aerogpu_dbgctl --query-perf` *(alias: `--perf`)*  
  Dumps a KMD-provided perf/health counter snapshot (fence/ring progress, submit counts, IRQ counts, reset counts, vblank counters).
  On older KMD builds this may print `(not supported)`; upgrade the driver to enable it.

- `aerogpu_dbgctl --query-scanout`  
  Dumps scanout 0 state as seen by the KMD, including:
  - cached mode (`CurrentWidth/Height/Format/Pitch`) and visibility (`enable`)
  - best-effort MMIO snapshot (`SCANOUT0_*` registers), including the current framebuffer GPA
  
  Useful for diagnosing mode/pitch mismatches (e.g. scanline bounds issues or a blank display even though fences advance).

- `aerogpu_dbgctl --dump-scanout-bmp C:\scanout.bmp`  
  Dumps the current scanout 0 framebuffer contents to an **uncompressed 32bpp BMP** using:
  - `AEROGPU_ESCAPE_OP_QUERY_SCANOUT` to discover width/height/format/pitch/fb_gpa
  - `AEROGPU_ESCAPE_OP_READ_GPA` to read framebuffer bytes from guest physical memory
  
  Useful for diagnosing pitch/format/fb_gpa bugs **without screen capture / RDP**.
  Note: this requires the installed KMD to support `AEROGPU_ESCAPE_OP_READ_GPA`; if unsupported, dbgctl will fail with
  `STATUS_NOT_SUPPORTED` (`0xC00000BB`).

- `aerogpu_dbgctl --query-cursor` *(alias: `--dump-cursor`)*  
  Dumps cursor MMIO state (`CURSOR_*` registers), including:
  - enable, position, hot spot
  - size, format, pitch, framebuffer GPA
  
  Useful for diagnosing Win7 hardware cursor bring-up (e.g. cursor enabled but off-screen, wrong hot spot, wrong pitch).

- `aerogpu_dbgctl --read-gpa GPA --size N [--out FILE] [--force]`  
  Reads a **bounded** slice of guest physical memory (GPA) from buffers that the KMD/device tracks (for example: scanout framebuffer,
  cursor framebuffer, ring buffers, and driver-owned DMA buffers for pending submissions).
  
  Notes:
  - The KMD enforces a hard ABI maximum of `AEROGPU_DBGCTL_READ_GPA_MAX_BYTES` (currently 4096 bytes).
  - dbgctl refuses reads larger than 256 bytes unless `--force` is specified.
  - By default dbgctl prints a hex dump to stdout; use `--out` to also write raw bytes to a file.

- `aerogpu_dbgctl --dump-ring`  
  Dumps ring head/tail + recent submissions. Fields include:
  - `signal_fence`
  - `cmd_gpa` / `cmd_size_bytes`
  - `flags`
  - (AGPU only) `alloc_table_gpa` / `alloc_table_size_bytes`
  
  Note: for the AGPU (versioned) ring format, the v2 dump returns a **recent tail window** of descriptors ending at `tail-1`
  (newest is `desc[desc_count-1]`). This is intentionally not limited to the pending `[head, tail)` region so very fast
  devices/emulators still expose the most recent submission(s) to tooling/tests.

- `aerogpu_dbgctl --dump-last-cmd --out <path>`
  Dumps the raw bytes of the most recent command stream buffer (`cmd_gpa .. cmd_gpa+cmd_size_bytes`) from the ring
  into `<path>` (binary). Use `--index-from-tail K` to select older submissions (0 = newest).
  On AGPU rings, if the submission has an allocation table (`alloc_table_gpa/alloc_table_size_bytes`), it is also dumped
  to `<path>.alloc_table.bin`.

  Safety: by default dbgctl refuses to dump buffers larger than 1 MiB; use `--force` to override.

- `aerogpu_dbgctl --watch-ring --samples N --interval-ms M [--ring-id N]`  
  Polls ring head/tail in a loop and prints **one line per sample** with:
  - `head`, `tail`, and `pending` (queue depth, computed as `tail-head` / best-effort legacy wraparound), and
  - (when available) the newest descriptor's `fence` and `flags` for quick correlation with fence progression.
  
  Useful for diagnosing whether the emulator/backend is draining submissions (head catches up to tail) and whether the guest is over-submitting.

- `aerogpu_dbgctl --dump-createalloc` *(aliases: `--dump-createallocation`, `--dump-allocations`)*  
  Dumps a small KMD-maintained ring buffer of recent `DxgkDdiCreateAllocation` events, including:
  - the incoming `DXGK_ALLOCATIONINFO::Flags.Value` from dxgkrnl/runtime
  - the final flags after the miniport applies required bits (currently `CpuVisible` + `Aperture`)

  Structured export:
  - `--csv <path>` writes a stable, machine-parseable CSV file (header row + one row per trace entry), including `write_index` and `entry_capacity` metadata for wrap-around detection.
  - `--json <path>` writes a stable, machine-parseable JSON file with:
    - top-level `{ "schema_version": 1, "write_index": <u32>, "entry_capacity": <u32>, "entries": [...] }`
    - one JSON object per entry with all fields from `aerogpu_dbgctl_createallocation_desc`
    - note: flag fields are encoded as hex strings (e.g. `"0x00000001"`), and 64-bit values are encoded as strings (e.g. `"0x0000000000000000"` / `"4096"`) to avoid JSON integer precision issues in some parsers.

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

- `aerogpu_dbgctl --selftest`  
  Triggers a simple KMD-side self-test.

## Usage

```
aerogpu_dbgctl [--display \\.\DISPLAY1] [--ring-id N] [--timeout-ms N] \
               [--vblank-samples N] [--vblank-interval-ms N] \
               [--samples N] [--interval-ms N] \
               [--size N] [--out FILE] [--force] <command>
```

Examples:

```
aerogpu_dbgctl --list-displays
aerogpu_dbgctl --status
aerogpu_dbgctl --query-version
aerogpu_dbgctl --query-umd-private
aerogpu_dbgctl --query-fence
aerogpu_dbgctl --watch-fence --samples 120 --interval-ms 250 --timeout-ms 30000
aerogpu_dbgctl --query-perf
aerogpu_dbgctl --query-scanout
aerogpu_dbgctl --dump-scanout-bmp C:\scanout.bmp
aerogpu_dbgctl --read-gpa 0x12340000 --size 256
aerogpu_dbgctl --read-gpa 0x12340000 --size 4096 --force --out dump.bin
aerogpu_dbgctl --query-cursor
aerogpu_dbgctl --dump-ring --ring-id 0
aerogpu_dbgctl --watch-ring --ring-id 0 --samples 200 --interval-ms 50
aerogpu_dbgctl --dump-last-cmd --out last_cmd.bin
aerogpu_dbgctl --dump-last-cmd --index-from-tail 1 --out prev_cmd.bin
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
```

Notes:
- Unknown feature bits are reported as `unknown_bit_<n>` (bit index in the 128-bit `FEATURES_LO/HI` set).
- `IRQ_ACTIVE` is `IRQ_STATUS & IRQ_ENABLE` (i.e. causes that can currently assert the interrupt line).
- Some environments may return a non-zero `VidPnSourceId` from `D3DKMTOpenAdapterFromHdc`; dbgctl retries `--dump-vblank`, `--wait-vblank`, and `--query-scanline` with source 0 if needed (AeroGPU currently implements a single source).
- If `vblank_interrupt_type` prints `(not enabled or not reported)`, either dxgkrnl has not enabled vblank interrupt delivery for the adapter yet *or* the installed KMD predates `vblank_interrupt_type` reporting.
- `--timeout-ms` also bounds driver-private `D3DKMTEscape` calls used by commands like `--query-fence` / `--dump-ring`, and `D3DKMTQueryAdapterInfo` calls used by `--query-umd-private`. On timeout, dbgctl may skip `D3DKMTCloseAdapter` to avoid deadlocking inside a stuck kernel thunk.

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
  user32.lib gdi32.lib
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
- `AEROGPU_ESCAPE_OP_QUERY_SCANOUT` → `--query-scanout`, `--dump-scanout-bmp`
- `AEROGPU_ESCAPE_OP_QUERY_CURSOR` → `--query-cursor`
- `AEROGPU_ESCAPE_OP_DUMP_RING_V2` (fallback: `AEROGPU_ESCAPE_OP_DUMP_RING`) → `--dump-ring`, `--watch-ring`, `--dump-last-cmd`
- `AEROGPU_ESCAPE_OP_READ_GPA` → `--read-gpa`, `--dump-scanout-bmp`, `--dump-last-cmd`
- `AEROGPU_ESCAPE_OP_DUMP_CREATEALLOCATION` → `--dump-createalloc`
- `AEROGPU_ESCAPE_OP_QUERY_VBLANK` (alias: `AEROGPU_ESCAPE_OP_DUMP_VBLANK`) → `--dump-vblank`
- `AEROGPU_ESCAPE_OP_MAP_SHARED_HANDLE` → `--map-shared-handle`
- `AEROGPU_ESCAPE_OP_SELFTEST` → `--selftest`

Additional WDDM queries (do not use the escape channel):

- `--query-umd-private` uses `D3DKMTQueryAdapterInfo(KMTQAITYPE_UMDRIVERPRIVATE)` to query the KMD-provided discovery blob.
- `--wait-vblank` uses `D3DKMTWaitForVerticalBlankEvent` to measure vblank delivery from the OS.
- `--query-scanline` uses `D3DKMTGetScanLine` to report the current scanline and vblank state.

## Notes / troubleshooting

- If `D3DKMTOpenAdapterFromHdc` or `D3DKMTEscape` cannot be resolved from `gdi32.dll`, the OS is too old or the environment is not WDDM-capable.
- If `D3DKMTEscape` returns an error, ensure the AeroGPU driver is installed and exposes the required escapes.
- If `--wait-vblank` times out, the process may skip `D3DKMTCloseAdapter` to avoid deadlocking on broken vblank implementations.

## Not implemented (intentional)

The in-tree Win7 dbgctl tool is focused on snapshots/queries and compatibility with older KMD builds.
It does **not** currently provide:

- runtime log verbosity controls
- long-running perf capture / start-stop recording (dbgctl only supports snapshot-style counters via `--query-perf`)
- hang injection or forced reset helpers

If/when we add those as new driver-private escape ops, this README should be updated alongside the implementation.
