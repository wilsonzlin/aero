# Windows 7 audio smoke test (in-box HDA driver)

This is a **manual**, **reproducible** smoke test to validate that Windows 7’s **in-box HD Audio (Intel HDA) driver stack** works end-to-end once the emulated HDA controller is wired into the real Aero worker runtime:

- Windows enumerates the **PCI controller** (“High Definition Audio Controller”)
- Windows enumerates the **audio function** (“High Definition Audio Device”)
- **Playback** works (system sounds / WAV playback reaches the host speakers)
- **Recording** works (browser `getUserMedia` mic capture reaches the guest recording endpoint)

> Windows 7 uses the inbox `hdaudbus.sys` (bus) + `hdaudio.sys` (function) drivers.
> The goal of this smoke test is to confirm we do **not** need a custom driver just to get basic sound.

Important:

- This checklist is for the **baseline HDA device**.
  - If your VM profile/device model is set to **virtio-snd**, Windows will enumerate a different device and this checklist won’t apply.

---

## Prerequisites (host/browser)

- Use a Chromium-based browser for initial bring-up (Chrome/Edge).
- The page must be **cross-origin isolated** (`COOP` + `COEP`) so `SharedArrayBuffer` works (needed for low-latency audio rings).
- **Host audio output** must be functional (speakers/headphones connected, not muted).
- **Host microphone** available (for capture test) and permission can be granted.

Quick sanity checks (DevTools Console):

```js
crossOriginIsolated;
typeof SharedArrayBuffer;
typeof AudioContext;
```

### Windows 7 media

Per [`AGENTS.md`](../../AGENTS.md), a Win7 ISO is available on the agent machine:

```
/state/win7.iso
```

Do **not** commit or redistribute Windows media/images.

---

## 1) Boot the Windows 7 test image

This section describes what to do in the Aero **web UI** (the exact page/controls may change over time; the intent is what matters).

### 1.1 Start the web app

From the repo root:

```bash
npm ci
npm run dev:web
```

Open the printed local URL (usually `http://127.0.0.1:5173/`).

Notes:

- `npm run dev:web` runs the **web UI** under `web/`, which includes the **Disks** + **Workers** panels used below.
- The repo-root harness (`npm run dev`) is primarily used by CI/Playwright and may not expose the full Win7 boot UI.

### 1.2 Ensure audio can start (autoplay policy)

Most browsers require a **user gesture** to start audio output. Before expecting guest audio:

- In the UI, click the **Audio** panel’s **“Init audio output …”** button (exact label may vary by build).
- Confirm the host-side audio status shows:
  - `AudioContext: running` (not `suspended`)
  - Ring buffer counters are visible (see §3.2).

### 1.3 Boot Windows 7

Use the UI’s **Disks** panel + **Workers** panel to boot Windows:

- In **Disks**:
  - Click **Import file…** and select the ISO:
    - Agent path: `/state/win7.iso` (see §Prerequisites)
  - Create/import a **Windows 7 system disk** as an HDD (`*.img`), *or* create a blank HDD and mount the ISO as a CD to install once.
  - Mount:
    - Use **Mount HDD** for the system disk
    - Use **Mount CD** for the Win7 ISO (the UI infers `*.iso` as `kind=cd`)
- In **Workers**:
  - Click **Start workers** (or **Start VM**).
- Wait for Windows to reach the desktop.

**If you are installing from ISO:** once Windows is installed and boots successfully from the HDD, **unmount the ISO/CD** for subsequent boots so the test is faster and more deterministic.

---

## 2) Driver enumeration (Device Manager)

Inside the Windows 7 guest:

1. Open **Device Manager**:
   - `Start` → right-click `Computer` → `Manage` → `Device Manager`
2. Confirm the following entries exist and have **no yellow warning icon**:
   - **System devices** → `High Definition Audio Controller`
   - **Sound, video and game controllers** → `High Definition Audio Device`

3. Confirm Windows is using the **in-box Microsoft driver**:
   - Right-click `High Definition Audio Device` → **Properties** → **Driver** tab
   - Expected: **Driver Provider** is `Microsoft`
   - Optional: click **Driver Details** and confirm `hdaudio.sys` is present

4. Optional (CLI verification):

```cmd
wmic path Win32_SoundDevice get Name,Manufacturer,Status,PNPDeviceID
```

5. Optional (bus driver verification):
   - In Device Manager: **System devices** → `High Definition Audio Controller` → **Properties** → **Driver Details**
   - Expected: `hdaudbus.sys` is present (Win7 inbox HDA bus driver).

6. Optional (DxDiag report):

```cmd
dxdiag /t %TEMP%\\dxdiag.txt
notepad %TEMP%\\dxdiag.txt
```

Expected outcome:

- Windows should use the **Microsoft** inbox driver stack automatically.
- `Control Panel → Sound` should show at least one **Playback** device and at least one **Recording** device (names may vary, but should map to the HDA device).

---

## 3) Playback test (system sounds / WAV)

### 3.1 Trigger playback (guest)

In Windows 7:

1. Open `Control Panel → Sound` (or run `mmsys.cpl`).
2. **Playback tab**:
   - Select the default playback device (often `Speakers` / `High Definition Audio Device`).
   - (If needed) click **Set Default**.
    - Click **Test**.
   - Tip: if you don’t see any devices, right-click the empty area and enable:
     - **Show Disabled Devices**
     - **Show Disconnected Devices**

Alternative (also deterministic):

- `Control Panel → Sound → Sounds tab` → select any “Program Events” sound → click **Test**.

Alternative (deterministic WAV file path present on all Win7 installs):

- Run this in the guest (PowerShell):

```powershell
(New-Object Media.SoundPlayer "C:\\Windows\\Media\\tada.wav").PlaySync()
```

Expected outcome:

- You hear the test sound on the host speakers/headphones.
- Windows shows playback activity (level meter/volume mixer).
  - If you don’t see the green level meter move, check **Volume Mixer** and make sure the app/device is not muted.

### 3.2 Observe host-side metrics (AudioWorklet ring buffer)

While the guest is producing sound, capture the host-side metrics:

**Required:**

- **Ring buffer level** stays non-zero while audio plays (buffer is being filled/consumed).
- **Underruns** do not rapidly increase.
- **Overruns** remain `0` (or do not increase).
- (Optional but useful) **readFrameIndex** and **writeFrameIndex** advance over time.

Suggested pass criteria (rule-of-thumb):

- Let playback run for ~10 seconds.
- `overrunCount` should stay at `0`.
- `underrunCount` should be `0`, or at least stop increasing after startup.
  - If you see a small non-zero value at startup, treat up to **128 underrun frames** (one render quantum) as “tolerable” but still worth tracking.

Where to look:

- If the web UI has an audio status box/HUD, record the values shown:
  - `bufferLevelFrames`
  - `underrunCount` (or `underrunFrames`)
  - `overrunCount` (or `overrunFrames`)
  - `sampleRate`

If no UI exists, use DevTools Console to read the standard ring header (layout is documented in `docs/06-audio-subsystem.md`):

```js
// Find the active audio output object exposed by the host UI/runtime (name may vary by build).
const out =
  globalThis.__aeroAudioOutput ??
  globalThis.__aeroAudioOutputWorker ??
  globalThis.__aeroAudioOutputHdaDemo;

out?.getMetrics?.()

// Raw ring header counters (u32):
// 0=readFrameIndex, 1=writeFrameIndex, 2=underrunCount, 3=overrunCount
out?.ringBuffer?.header && {
  read: Atomics.load(out.ringBuffer.header, 0) >>> 0,
  write: Atomics.load(out.ringBuffer.header, 1) >>> 0,
  underruns: Atomics.load(out.ringBuffer.header, 2) >>> 0,
  overruns: Atomics.load(out.ringBuffer.header, 3) >>> 0,
};
```

Quick interpretation tips:

- If `writeFrameIndex` is **not** increasing: the emulator side is not producing audio (guest DMA not progressing, HDA stream not running, etc).
- If `readFrameIndex` is **not** increasing: the AudioWorklet consumer is not running (AudioContext suspended, worklet not connected, etc).

### 3.3 Observe guest DMA progress (if debug UI exists)

If the build exposes HDA stream debug state, confirm that **guest-visible DMA progress advances** while the sound plays:

- Output stream `LPIB` increases (and wraps modulo `CBL` for cyclic buffers)
- If the **position buffer** is enabled, its value changes over time

If DMA progress is stuck (e.g. `LPIB` never moves), the guest may “think” it is playing while the host produces silence.

---

## 4) Recording test (microphone capture)

### 4.1 Grant browser microphone permission (host)

In the Aero web UI:

1. Click **Start microphone** (or equivalent).
2. Approve the browser permission prompt (this is a `getUserMedia({ audio: … })` request).
3. If the UI shows mic ring stats, confirm they update (e.g. buffered samples increases, dropped samples stays low).

### 4.2 Verify the guest recording endpoint (Windows)

In Windows 7:

1. Open `Control Panel → Sound → Recording` tab.
2. Confirm a recording device exists (typically `Microphone` / `High Definition Audio Device`).
   - Tip: if you don’t see any devices, right-click the empty area and enable:
     - **Show Disabled Devices**
     - **Show Disconnected Devices**
3. Speak into the host microphone and observe the **level meter** moves.

Optional end-to-end verification (more obvious than a level meter):

- Launch **Sound Recorder** in Windows, record ~3 seconds, and play it back.

---

## 5) Common failure modes + what to collect

### A) Driver / enumeration failures

- **No “High Definition Audio Controller” in Device Manager**
  - Likely PCI wiring/identity issue (class code, BAR sizing, IRQ/MSI, device not on PCI bus)
- **Yellow bang / “Unknown device”**
  - Likely config space mismatch or missing required capabilities
  - Often appears as `Multimedia Audio Controller` under `Other devices`
- **Controller exists but no “High Definition Audio Device”**
  - HDA codec enumeration or CORB/RIRB verb path issue (bus driver loaded, function driver can’t bring up codec)

Collect:

- Screenshot of Device Manager showing the device tree + error icons.
- Device Properties → Details → “Hardware Ids” for the failing entry (copy text).
- Device Properties → Driver tab (Provider, Version, Driver Details).

### B) Playback failures

- **No sound, but Windows shows playback activity**
  - Browser audio output not started (`AudioContext` is `suspended` / autoplay blocked)
  - Output ring underrunning (producer not keeping up)
  - Guest DMA progress stuck (stream not actually running)
- **No sound and no activity meter**
  - Windows output device may not be the default (set default in `mmsys.cpl`)
  - The app/tab may be muted:
    - Windows: `sndvol.exe` (Volume Mixer)
    - Chrome: tab mute / site mute
  - Host OS output device may be incorrect (headphones vs speakers)
- **No playback devices appear in `Control Panel → Sound`**
  - Driver enumeration may be incomplete, or Windows Audio services may be stopped
  - Check `services.msc`:
    - `Windows Audio`
    - `Windows Audio Endpoint Builder`
- **Clicks/stutter**
  - Frequent underruns (buffer too small or CPU stalls)
- **Overruns increasing**
  - Producer writing too fast or not respecting backpressure
- **Ring indices don’t move**
  - `writeFrameIndex` stuck: emulator isn’t writing to the ring (guest DMA / HDA stream / wiring issue)
  - `readFrameIndex` stuck: AudioWorklet not consuming (AudioContext suspended / node not connected)

Collect:

- Audio ring metrics: buffer level + underrun/overrun counters + `AudioContext.state` + `sampleRate`.
- If available: runtime/worker “producer” counters (how full the emulator-to-worklet ring is from the producer’s point of view).
- If available: guest HDA stream debug (`LPIB`, `CBL`, position buffer).
- Browser console logs (preserve timestamps; include `[cpu]`/`[io]` worker logs if present).
- If Perf tracing is enabled in the build, export a trace (it should include `audio.*` counters when the host is sampling audio metrics).
  - See: [`docs/16-perf-tracing.md`](../16-perf-tracing.md)

### C) Microphone failures

- **No recording device in Windows**
  - Capture pin/stream not exposed or codec topology incomplete
- **Device exists but level meter never moves**
  - `getUserMedia` denied / not started, host mic muted, or capture ring not being drained into guest DMA
  - Browser may be blocking mic access:
    - Chrome: site icon → **Site settings** → Microphone → Allow
  - Another application may already be using the microphone exclusively (host OS dependent).

Collect:

- Browser permission status + any `getUserMedia` error shown in the console.
- Mic ring stats (buffered/dropped) if available in UI.
- Screenshot of the guest Recording tab.

### D) Snapshot for bug reports (when available)

If the runtime exposes a snapshot/save-state UI, save a snapshot immediately after reproducing:

- Note the snapshot path (often `state/worker-vm-autosave.snap` in OPFS).
- Export it for sharing. Example DevTools snippet (OPFS):

```js
const root = await navigator.storage.getDirectory();
const fh = await root.getFileHandle("state/worker-vm-autosave.snap");
const file = await fh.getFile();
const url = URL.createObjectURL(file);
const a = document.createElement("a");
a.href = url;
a.download = "worker-vm-autosave.snap";
a.click();
```

---

## Appendix: what to include in a bug report

When reporting a Win7 audio regression, include (at minimum):

- **Build info**
  - Git commit SHA (or release version)
    - Recommended: open `/<origin>/aero.version.json` (same origin as the web UI) and attach the JSON.
    - Or in DevTools Console (if present): `__AERO_BUILD_INFO__`
  - Browser + version (e.g. Chrome 123)
  - OS + audio output device (optional but helpful)
- **Host capability checks**
  - `crossOriginIsolated` + `typeof SharedArrayBuffer` (see §Prerequisites)
  - Output `AudioContext.sampleRate` (from `out.getMetrics().sampleRate` if available)
- **Guest evidence**
  - Device Manager screenshot showing:
    - `High Definition Audio Controller`
    - `High Definition Audio Device`
  - Driver Provider on the audio function device (expected: `Microsoft`)
- **Runtime metrics/logs**
  - Ring buffer counters (buffer level + underruns + overruns)
  - Browser console log output during the repro (including worker-prefixed logs like `[cpu]`, `[io]` if present)
  - If tracing is available, a **trace export** taken during the repro (Perf HUD → Trace Start/Stop → Trace JSON)
    - See: [`docs/16-perf-tracing.md`](../16-perf-tracing.md)
  - Snapshot file exported after repro (if available)
- For driver binding issues: a snippet from `C:\Windows\inf\setupapi.dev.log` around the audio device’s Hardware ID can be extremely helpful.
