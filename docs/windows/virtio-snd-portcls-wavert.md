# Windows 7 `virtio-snd` PortCls + WaveRT driver design

This document is a **clean-room design reference** for a minimal Windows 7 audio driver for the repo’s [`virtio-snd`](../virtio-snd.md) device model. It describes the **PortCls + WaveRT** surface (COM interfaces, KS descriptors, properties, and INF) required to enumerate functional render + capture endpoints in Windows 7.

The goal is to make the first bring-up deterministic: if the driver matches the shapes described here, Windows 7 should create “Speakers” (render) and “Microphone” (capture) endpoints and the audio engine should be able to transition streams to `RUN`.

Note: the current in-tree Windows 7 virtio-snd driver (`drivers/windows7/virtio-snd/`) supports both **render**
and **capture** streams per `AERO-W7-VIRTIO` v1 (stream id 0 via `txq`, stream id 1 via `rxq`). When a virtio-snd
implementation advertises a **superset** of capabilities in `PCM_INFO`, the driver can optionally expose additional
formats/rates/channel counts to Windows via dynamic WaveRT pin data ranges (while still requiring and preferring the
contract-v1 baseline).

## Scope / assumptions (minimum viable endpoint)

* **Windows target:** Windows 7 SP1 (x86/x64). The driver can be built with a Win7-era WinDDK (7600) layout or with newer WDKs (CI uses WDK10/MSBuild).
* **Device model:** `virtio-snd` PCI function (`PCI\VEN_1AF4&DEV_1059&REV_01`; Aero `AERO-W7-VIRTIO` v1).
* **Audio direction:** render + capture.
* **Streams:** contract v1 defines **2** virtio-snd streams:
  * Stream 0: playback/output (render)
  * Stream 1: capture/input (capture)
* **Format:** contract v1 baseline PCM:
  * render: **stereo (2ch), 48 kHz, signed 16-bit LE PCM (S16_LE)**
  * capture: **mono (1ch), 48 kHz, signed 16-bit LE PCM (S16_LE)**
  * *(Optional/non-contract)*: additional formats/rates/channels may be exposed when a device advertises them via
    `PCM_INFO`.
* **Mixing:** Windows audio engine (`audiodg.exe`) does mixing; the miniport is a single shared-mode endpoint.
* **Virtio transport:** virtio-pci **modern-only** (PCI vendor-specific capabilities + BAR0 MMIO).
* **Virtio feature bits:** `VIRTIO_F_VERSION_1` + `VIRTIO_F_RING_INDIRECT_DESC` only.
* **Virtqueues (contract v1):** `controlq=64`, `eventq=64`, `txq=256`, `rxq=64`.
* **Interrupts:** PCI **INTx** baseline (required by `AERO-W7-VIRTIO` v1). MSI/MSI-X is an optional enhancement; the in-tree driver prefers message interrupts when Windows grants them (INF opt-in), programs virtio MSI-X routing (`msix_config`, `queue_msix_vector`), and falls back to INTx if message interrupts cannot be used/programmed.

Authoritative virtio contract:

* [`docs/windows7-virtio-driver-contract.md`](../windows7-virtio-driver-contract.md) (AERO-W7-VIRTIO v1)

Non-goals:

* Multiple endpoints per direction, sample rate conversion, offload, jack sensing, power management beyond “works”.

---

## 1) Architecture overview

### Windows audio stack (where this driver sits)

At a high level, the stack for a PCI `virtio-snd` endpoint looks like:

```
User apps (WASAPI / DirectSound / MME)
  ↓
Windows Audio Engine (audiodg.exe) + AudioSrv
  ↓
KS proxies (wdmaud.sys / sysaudio.sys / ks.sys)
  ↓
PortCls port driver (portcls.sys)
  ↓
Our adapter driver (virtio-snd PCI function driver)
  ↓
Miniports inside our driver:
  - WaveRT miniport (streaming render + capture pins)
  - Topology miniport (bridge pin + controls)
  ↓
virtio-pci transport + virtqueues
  ↓
Emulator device model (`docs/virtio-snd.md`) → browser audio backend
```

### Driver-internal decomposition

The **PortCls adapter driver** is a single `.sys` that:

1. Binds to the `virtio-snd` PCI function (PnP start/stop).
2. Initializes the virtio transport (BAR mapping, virtqueues, interrupts (MSI/MSI-X preferred with INTx fallback) or polling-only in bring-up mode).
3. Registers two PortCls subdevices:
   * `Wave` (WaveRT streaming filter factory)
   * `Topology` (topology filter factory)
4. Routes WaveRT stream operations into a **software-DMA loop** that submits PCM to the virtio-snd TX virtqueue (render) and posts buffers to the RX virtqueue (capture).

Most implementations share hardware state via an “adapter common” object referenced by both miniports:

```
           +---------------------+
           | AdapterCommon       |
           | - virtio state      |
           | - stream state      |
           | - DMA timer/DPC     |
           +----------+----------+
                      ^
      +---------------+---------------+
      |                               |
+-----+----------------+  +-----------+-----------+
| WaveRT miniport      |  | Topology miniport     |
| (IMiniportWaveRT)    |  | (IMiniportTopology)   |
+----------------------+  +-----------------------+
```

---

## 2) Required COM interfaces and responsibilities (Win7 bring-up)

PortCls miniports are COM-style objects (kernel-mode, `IUnknown`-like). For Windows 7 WaveRT render/capture bring-up, the following interfaces are required.

### 2.1 Base: `IUnknown`

All miniport and stream objects must implement:

* `QueryInterface`
* `AddRef`
* `Release`

Notes:

* Avoid pageable code in any method that can be called at `DISPATCH_LEVEL`.
* Keep lifetime rules simple: the port owns the miniport; the miniport owns streams; streams hold a ref to the adapter common.

### 2.2 Base: `IMiniport`

Both the WaveRT and topology miniports implement `IMiniport` as a base.

**Win7-critical responsibilities:**

* **`Init`**: capture the `UnknownAdapter`/resource list, create or reference the shared adapter-common object, and store the port interface pointer (`IPortWaveRT` or `IPortTopology`).
* **`GetDescription`**: return the **PortCls/KS descriptor** for the subdevice (pins, nodes, connections, categories).
* **`DataRangeIntersection`**: answer `KSPROPERTY_PIN_DATAINTERSECTION` for the streaming pin (format negotiation).
  * For a fixed-format endpoint, this can be a strict matcher.
  * When exposing multiple formats/rates/channels, it typically returns a `KSDATAFORMAT_WAVEFORMATEXTENSIBLE`
    corresponding to the selected `KSDATARANGE_AUDIO` (and validates the request is compatible with it).

Why this matters: Windows uses `DataRangeIntersection` aggressively during endpoint construction. Returning “close enough” formats tends to cause user-mode format failures later (or silent format conversion you didn’t plan for).

### 2.3 Wave: `IMiniportWaveRT`

The WaveRT miniport owns the streaming pin implementation.

**Win7 bring-up surface (must work):**

* **Stream creation (`NewStream`)**
  * Validate pin id (render or capture).
  * Validate data format (baseline fixed PCM formats, or a dynamically generated supported-format table).
  * Create and return an `IMiniportWaveRTStream` instance.
* **Hardware/stream capabilities**
  * Report that the stream is **render** or **capture** depending on pin id.
  * Provide consistent buffer alignment requirements (typically frame-aligned).

**Common implementation pattern:**

* `NewStream` creates a stream object configured with:
  * `FramesPerSecond` (sample rate)
  * `BlockAlign` (channels * bytes_per_sample)
  * ring buffer size in frames/bytes
  * notification “period” in frames

### 2.4 Stream: `IMiniportWaveRTStream`

This is the most sensitive part of WaveRT. The Windows 7 audio engine expects the stream object to implement a coherent model of:

* **Buffer provisioning / mapping**
  * Provide (or coordinate) a cyclic DMA buffer that the audio engine can write into (render) or read from (capture).
  * For WaveRT this is typically a **locked kernel buffer** that PortCls maps into user mode.
* **Position reporting**
  * Report “play position” in bytes/frames monotonically while running.
  * Do not jump backwards except on STOP/RESET.
* **Notification event**
  * Accept a kernel event object from PortCls/user-mode and signal it at each period boundary.
  * If notifications do not fire, shared-mode audio often never starts (or glitches heavily).
* **State transitions**
  * Handle `KSSTATE_STOP`, `KSSTATE_ACQUIRE`, `KSSTATE_PAUSE`, `KSSTATE_RUN` transitions.
  * Bring-up ordering that tends to work:
    1. `STOP → ACQUIRE`: allocate buffer + init counters
    2. `ACQUIRE → PAUSE`: arm timers, prime virtio stream (`PREPARE`)
    3. `PAUSE → RUN`: send `START`, start periodic DPC submissions, begin advancing play cursor
    4. any `* → STOP`: stop DPC, send `STOP`/`RELEASE`, release resources

The exact method names in WDK vary slightly by WaveRT revision, but the above responsibilities map to:

* stream `SetState`
* stream `GetPosition` / “presentation position”
* stream “register notification event”
* stream “allocate/free buffer” (or callback invoked by the WaveRT port during `KSPROPERTY_RTAUDIO_BUFFER`)

### 2.5 Topology: `IMiniportTopology`

Topology miniport is “controls + wiring” for the endpoint. For a minimal fixed-format endpoint (render + capture) it can be mostly static.

**Win7 bring-up surface (must work):**

* Expose a topology filter descriptor with:
  * a **render bridge pin** to connect to the wave filter
  * a **capture bridge pin** to connect to the wave filter
  * a **speaker node** and optionally a **microphone node** (recommended)
* Implement (or allow PortCls to implement) minimal properties:
  * `KSPROPERTY_AUDIO_CHANNEL_CONFIG` (report stereo on render, mono on capture)
  * optionally `KSPROPSETID_Jack` stubs (fixed “connected”)

---

## 3) KS filter/pin/node descriptors and categories (concrete sketch)

The driver registers **two KS filter factories** via PortCls:

* WaveRT filter factory (streaming)
* Topology filter factory (controls/graph)

The descriptors below are a “shape reference”; the exact C structures are in WDK (`portcls.h`, `ks.h`, `ksmedia.h`). The important part is that the pin ids, categories, and data ranges are consistent.

### 3.1 WaveRT filter (render + capture)

**Filter categories (must include):**

* `KSCATEGORY_AUDIO` — tells SysAudio/WDMAud “this is audio”.
* `KSCATEGORY_RENDER` — tells Windows this factory provides a render endpoint.
* `KSCATEGORY_CAPTURE` — tells Windows this factory provides a capture endpoint.
* `KSCATEGORY_REALTIME` — strongly recommended for WaveRT (low-latency path and endpoint heuristics).

**Pins:**

| Pin ID | Name (suggested) | Role | Dataflow | Communication | Exposed to user mode |
|-------:|------------------|------|----------|---------------|----------------------|
| 0 | `Render` | streaming render pin | `KSPIN_DATAFLOW_IN` | `KSPIN_COMMUNICATION_SINK` | yes (apps open this) |
| 1 | `Bridge` | render connection to topology filter | (usually `OUT`) | `KSPIN_COMMUNICATION_BRIDGE` | no |
| 2 | `Capture` | streaming capture pin | `KSPIN_DATAFLOW_OUT` | `KSPIN_COMMUNICATION_SOURCE` | yes (apps open this) |
| 3 | `BridgeCapture` | capture connection to topology filter | (usually `IN`) | `KSPIN_COMMUNICATION_BRIDGE` | no |

**Streaming pin data ranges:**

* MajorFormat: `KSDATAFORMAT_TYPE_AUDIO`
* SubFormat: `KSDATAFORMAT_SUBTYPE_PCM`
* Specifier: `KSDATAFORMAT_SPECIFIER_WAVEFORMATEX` (but return `WAVEFORMATEXTENSIBLE` from intersection)

For a fixed-format endpoint, `KSDATARANGE_AUDIO` should constrain:

* `MaximumChannels = MinimumChannels = 2`
* `MinimumBitsPerSample = MaximumBitsPerSample = 16`
* `MinimumSampleFrequency = MaximumSampleFrequency = 48000`

Capture pin data range is identical except:

* `MaximumChannels = MinimumChannels = 1`

**Why fixed ranges instead of “wildcards”:** the simplest stable bring-up is to avoid Windows picking unexpected formats.
Once stable, widen supported ranges deliberately (for example by generating pin data ranges from `PCM_INFO`).

#### WaveRT filter descriptor sketch (pseudo-C)

This is a concrete “shape” of the descriptor data that `IMiniport::GetDescription` should return for the WaveRT subdevice:

```c
// Filter categories (factory-level)
static const GUID* const kWaveCategories[] = {
    &KSCATEGORY_AUDIO,
    &KSCATEGORY_RENDER,
    &KSCATEGORY_CAPTURE,
    &KSCATEGORY_REALTIME,
};

// Supported streaming formats for pin 0 ("Render") and pin 2 ("Capture")
static const KSDATARANGE_AUDIO kRenderDataRanges[] = {
    {
        .DataRange = {
            .FormatSize = sizeof(KSDATARANGE_AUDIO),
            .Flags = 0,
            .SampleSize = 0,
            .MajorFormat = KSDATAFORMAT_TYPE_AUDIO,
            .SubFormat = KSDATAFORMAT_SUBTYPE_PCM,
            .Specifier = KSDATAFORMAT_SPECIFIER_WAVEFORMATEX,
        },
        .MaximumChannels = 2,
        .MinimumChannels = 2,
        .MaximumBitsPerSample = 16,
        .MinimumBitsPerSample = 16,
        .MaximumSampleFrequency = 48000,
        .MinimumSampleFrequency = 48000,
    },
};

static const KSDATARANGE_AUDIO kCaptureDataRanges[] = {
    {
        .DataRange = {
            .FormatSize = sizeof(KSDATARANGE_AUDIO),
            .Flags = 0,
            .SampleSize = 0,
            .MajorFormat = KSDATAFORMAT_TYPE_AUDIO,
            .SubFormat = KSDATAFORMAT_SUBTYPE_PCM,
            .Specifier = KSDATAFORMAT_SPECIFIER_WAVEFORMATEX,
        },
        .MaximumChannels = 1,
        .MinimumChannels = 1,
        .MaximumBitsPerSample = 16,
        .MinimumBitsPerSample = 16,
        .MaximumSampleFrequency = 48000,
        .MinimumSampleFrequency = 48000,
    },
};

// Pins: 0 = Render, 1 = Bridge, 2 = Capture, 3 = BridgeCapture
static const PCPIN_DESCRIPTOR kWavePins[] = {
    // Pin 0: render streaming pin (user visible)
    {
        .DataFlow = KSPIN_DATAFLOW_IN,
        .Communication = KSPIN_COMMUNICATION_SINK,
        .Category = &KSCATEGORY_AUDIO, // some drivers also set a pin category
        .Name = NULL,
        .DataRanges = (PKSDATARANGE)kRenderDataRanges,
        .DataRangesCount = ARRAYSIZE(kRenderDataRanges),
        .InstancesPossible = 1,
        .InstancesNecessary = 1,
    },
    // Pin 1: bridge pin (internal link to topology)
    {
        .DataFlow = KSPIN_DATAFLOW_OUT,
        .Communication = KSPIN_COMMUNICATION_BRIDGE,
        .Category = NULL,
        .Name = NULL,
        .DataRanges = NULL,
        .DataRangesCount = 0,
        .InstancesPossible = 1,
        .InstancesNecessary = 1,
    },
    // Pin 2: capture streaming pin (user visible)
    {
        .DataFlow = KSPIN_DATAFLOW_OUT,
        .Communication = KSPIN_COMMUNICATION_SOURCE,
        .Category = &KSCATEGORY_AUDIO,
        .Name = NULL,
        .DataRanges = (PKSDATARANGE)kCaptureDataRanges,
        .DataRangesCount = ARRAYSIZE(kCaptureDataRanges),
        .InstancesPossible = 1,
        .InstancesNecessary = 1,
    },
    // Pin 3: bridge pin for capture (internal link to topology)
    {
        .DataFlow = KSPIN_DATAFLOW_IN,
        .Communication = KSPIN_COMMUNICATION_BRIDGE,
        .Category = NULL,
        .Name = NULL,
        .DataRanges = NULL,
        .DataRangesCount = 0,
        .InstancesPossible = 1,
        .InstancesNecessary = 1,
    },
};

static const PCFILTER_DESCRIPTOR kWaveFilterDescriptor = {
    .Version = 1,
    .Flags = 0,
    .PinCount = ARRAYSIZE(kWavePins),
    .PinDescriptor = kWavePins,
    .CategoryCount = ARRAYSIZE(kWaveCategories),
    .Category = kWaveCategories,
    // .AutomationTable = ... (see section 4)
};
```

Exact field names differ between `PCPIN_DESCRIPTOR` variants (`PCPIN_DESCRIPTOR`, `PCPIN_DESCRIPTOR_EX`, etc.). The important part is: pin 0 advertises only the one PCM data range, and pin ids are stable.

### 3.2 Topology filter (render endpoint wiring)

**Filter categories (must include):**

* `KSCATEGORY_TOPOLOGY`

**Pins and nodes (minimal recommended):**

| Pin ID | Name (suggested) | Role |
|-------:|------------------|------|
| 0 | `BridgeIn` | bridge pin to WaveRT filter bridge pin |
| 1 | `SpeakerOut` (optional but recommended) | “physical” output connector |

**Nodes (optional but recommended):**

* `KSNODETYPE_SPEAKER` — lets Windows label the endpoint as “Speakers” and enables standard speaker properties.

**Connections (example):**

* `BridgeIn` → `Speaker` node → `SpeakerOut`

If you omit `SpeakerOut`, keep at least:

* a bridge pin, and
* a speaker node connected to the bridge

In practice, “bridge-only topology” is fragile across Windows versions and tools; the extra output connector pin tends to make SysAudio’s graph building less surprising.

#### Topology filter descriptor sketch (pseudo-C)

```c
static const GUID* const kTopoCategories[] = {
    &KSCATEGORY_TOPOLOGY,
};

// Pins: 0 = BridgeIn, 1 = SpeakerOut (connector)
static const PCPIN_DESCRIPTOR kTopoPins[] = {
    // Pin 0: bridge from wave
    {
        .DataFlow = KSPIN_DATAFLOW_IN,
        .Communication = KSPIN_COMMUNICATION_BRIDGE,
        .Category = NULL,
        .Name = NULL,
        .DataRanges = NULL,
        .DataRangesCount = 0,
        .InstancesPossible = 1,
        .InstancesNecessary = 1,
    },
    // Pin 1: speaker connector pin (optional but recommended)
    {
        .DataFlow = KSPIN_DATAFLOW_OUT,
        .Communication = KSPIN_COMMUNICATION_NONE,
        .Category = &KSCATEGORY_AUDIO,
        .Name = NULL,
        .DataRanges = NULL,
        .DataRangesCount = 0,
        .InstancesPossible = 1,
        .InstancesNecessary = 1,
    },
};

static const PCNODE_DESCRIPTOR kTopoNodes[] = {
    // Node 0: speaker
    { .Type = &KSNODETYPE_SPEAKER },
};

// BridgeIn → Speaker node → SpeakerOut
static const KSTOPOLOGY_CONNECTION kTopoConnections[] = {
    // From filter pin 0 (BridgeIn) to speaker node 0.
    { .FromNode = KSFILTER_NODE, .FromPin = 0, .ToNode = 0, .ToPin = 0 },
    // From speaker node 0 back to filter pin 1 (SpeakerOut).
    { .FromNode = 0, .FromPin = 0, .ToNode = KSFILTER_NODE, .ToPin = 1 },
};

static const PCFILTER_DESCRIPTOR kTopoFilterDescriptor = {
    .Version = 1,
    .Flags = 0,
    .PinCount = ARRAYSIZE(kTopoPins),
    .PinDescriptor = kTopoPins,
    .NodeCount = ARRAYSIZE(kTopoNodes),
    .NodeDescriptor = kTopoNodes,
    .ConnectionCount = ARRAYSIZE(kTopoConnections),
    .ConnectionDescriptor = kTopoConnections,
    .CategoryCount = ARRAYSIZE(kTopoCategories),
    .Category = kTopoCategories,
    // .AutomationTable = ... (see section 4)
};
```

Again, treat this as a descriptor “shape” reference. The key is that the topology miniport provides a bridge pin (pin 0) and advertises a speaker-ish graph that SysAudio can reason about.

---

## 4) Property sets / automation tables (minimum for Win7 stability)

This section lists the **property surface** that Windows 7 will exercise during endpoint enumeration and streaming.

### 4.1 Pin/dataformat intersection (`KSPROPSETID_Pin`)

**Goal:** ensure format negotiation converges to *exactly* the one supported PCM format.

Minimum expected properties:

* `KSPROPERTY_PIN_DATARANGES` (GET)
  * Usually satisfied by the pin descriptor’s data ranges; KS will expose them.
* `KSPROPERTY_PIN_DATAINTERSECTION` (GET)
  * PortCls forwards to `IMiniport::DataRangeIntersection`.
  * **Implementation:** accept only the fixed `WAVEFORMATEXTENSIBLE` and return a full `KSDATAFORMAT_WAVEFORMATEXTENSIBLE`.

Stub strategy:

* Do not attempt to synthesize arbitrary intersections.
* If the caller provides `WAVEFORMATEX` vs `WAVEFORMATEXTENSIBLE`, you can either:
  * reject it (strict), or
  * accept it only if it describes the same PCM format and still return an extensible format.

### 4.2 WaveRT buffer + position reporting (`KSPROPSETID_RtAudio`)

WaveRT relies on the `KSPROPSETID_RtAudio` property set to:

1. **Negotiate and map the cyclic buffer**.
2. **Configure a notification event**.
3. **Report position** with low overhead.

In practice, a minimal Win7 WaveRT render endpoint should be prepared to handle (directly or via the WaveRT port) at least:

* `KSPROPERTY_RTAUDIO_BUFFER` (GET) — allocate/describe the cyclic buffer and notification granularity.
* One of:
  * `KSPROPERTY_RTAUDIO_POSITIONREGISTER` (GET), or
  * `KSPROPERTY_RTAUDIO_POSITIONFUNCTION` (GET)
* A notification-event registration path (commonly surfaced as an RtAudio property that carries an event handle).

Commonly queried (nice to have; can often be fixed/stubbed):

* `KSPROPERTY_RTAUDIO_HWLATENCY` (GET)
* `KSPROPERTY_RTAUDIO_PRESENTATION_POSITION` (GET)

#### 4.2.1 Buffer property (allocation + mapping)

Windows issues a property request that effectively asks:

* “What is the cyclic buffer size and where is it?”
* “How many notifications per buffer?”

**Driver responsibilities:**

* Provide a kernel buffer that:
  * is nonpaged
  * is stable for the lifetime of the stream while in `ACQUIRE/PAUSE/RUN`
  * is aligned to frames (`BlockAlign`)
* Decide a buffer size policy (see section 5).
* Return the address/MDL information expected by the WaveRT port so it can map to user mode.

#### 4.2.2 Position reporting: register-based vs method-based

Windows 7 supports two practical patterns for WaveRT position:

* **Register-based** (`KSPROPERTY_RTAUDIO_POSITIONREGISTER`): expose a position register description (user-mode reads it directly).
* **Method-based** (`KSPROPERTY_RTAUDIO_POSITIONFUNCTION`): user-mode/port queries position through a kernel call path.

For a software device (virtio-snd) there is no real hardware register. The simplest stable approach is:

* implement **method-based** position reporting (always available)
* optionally also expose a “register” that points at a shared memory location updated by the driver (emulated register), if you want to reduce call overhead later

**What must be consistent:**

* Position must advance at 48 kHz while `RUN` and stop advancing otherwise.
* Reported position must be frame-accurate enough that the audio engine’s padding math doesn’t oscillate.

#### 4.2.3 Notification event

Windows will provide an event handle/object and expect it to be signaled at the configured period. In a software-DMA model, the DPC/timer loop is typically responsible for:

* setting the event each time the play cursor crosses the next period boundary
* clearing/coalescing appropriately (events are level-triggered)

Failure mode: if events never signal, shared-mode render often stays stuck in `PAUSE` or starves immediately.

### 4.3 Topology channel config (`KSPROPSETID_Audio`)

Minimum property:

* `KSPROPERTY_AUDIO_CHANNEL_CONFIG` (GET/SET) on the appropriate topology node (speaker) or filter.

Recommended semantics for the minimal endpoint:

* GET: always return stereo speaker mask:
  * `SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT`
* SET: accept only that same value; otherwise return `STATUS_NOT_SUPPORTED` or `STATUS_INVALID_PARAMETER`

Why it matters: Windows control panel and the audio engine query/adjust channel config; returning inconsistent values can cause “speaker setup” UI to break or channel masks to be misapplied.

### 4.4 Optional jack properties (`KSPROPSETID_Jack`)

These are not strictly required for audio to play, but Windows 7 UX and some apps probe them.

Minimal safe stubs (fixed values):

* `KSPROPERTY_JACK_DESCRIPTION` (GET)
  * return a single “jack” describing a speaker/line-out, always present
* `KSPROPERTY_JACK_DESCRIPTION2` (GET)
  * can be zeroed/defaulted if not supported
* `KSPROPERTY_JACK_CONTAINERID` (GET)
  * optional; return a stable GUID if implemented

Stub policy:

* If you don’t implement a jack property, return `STATUS_NOT_SUPPORTED` (not success with garbage).
* If you do implement, keep results stable across boots (container id especially).

---

## 5) Timer/DPC software DMA model (WaveRT → virtio TX)

Because the virtio-snd device model is not a real bus-mastering audio controller, the WaveRT miniport behaves like a “software DMA” engine:

* user mode writes PCM into the WaveRT cyclic buffer
* the driver periodically copies/submits a period of frames to virtio-snd TX
* the driver advances a play cursor and signals notification events

### 5.1 Period and buffer sizing policy (recommended defaults)

For a first bring-up, pick conservative values:

* **Period:** 10 ms
  * frames/period = `48000 * 10ms = 480` frames
  * bytes/period = `480 frames * 4 B/frame = 1920` bytes
* **Buffer:** 100 ms (10 periods)
  * frames/buffer = `4800`
  * bytes/buffer = `19200`

Rationale:

* 10 ms is a common Windows shared-mode period and keeps scheduling overhead manageable.
* 100 ms gives slack for occasional TX backpressure without immediate underruns.

You may later tune down (e.g., 3 ms) once stability is proven.

### 5.2 Cursor math (ring buffer)

Maintain cursors in **frames** (not bytes) to avoid off-by-block-align mistakes:

* `bufferFrames` = total frames in the WaveRT cyclic buffer
* `playCursor` = next frame index the driver will submit to hardware (mod `bufferFrames`)
* `notificationCursor` = next frame index at which to signal the notification event (mod `bufferFrames`)

On each tick:

1. Determine `framesToSend` (usually `periodFrames`).
2. Read `framesToSend` frames from `WaveRtBuffer[playCursor..]` with wrap handling.
3. Submit those bytes to virtio TX.
4. Advance `playCursor = (playCursor + framesToSend) % bufferFrames`.
5. If `playCursor` crossed `notificationCursor`, signal event and advance `notificationCursor += periodFrames`.

### 5.3 Notification signaling

The notification event is conceptually “the hardware consumed another period”. In a software device you have two choices:

* **Submit-driven:** signal when you successfully submit a period to virtio TX (simple, but conflates “accepted” with “played”).
* **Time-driven:** signal based on wall clock at 48 kHz, but clamp by submitted frames (more accurate, slightly more code).

For initial bring-up, submit-driven signaling is acceptable, as long as:

* you do not advance position without also “consuming” data, and
* you handle TX backpressure by *not* advancing/signaling.

### 5.4 Backpressure when virtio TX is full

When the TX virtqueue has no free descriptors:

* Do **not** block in DPC.
* Do **not** advance `playCursor` (the device did not consume data).
* Keep the timer running; retry next tick.

Optional improvements (later):

* submit smaller chunks when descriptors are low
* keep a small “always available” silent buffer to avoid hard stalls
* expose glitch counters via debug output

---

## 6) `virtio-snd` backend mapping

This driver maps WaveRT streams to the `virtio-snd` device model described in [`docs/virtio-snd.md`](../virtio-snd.md):

- render: `virtio-snd` stream id `0` (TX)
- capture: `virtio-snd` stream id `1` (RX)

### 6.1 Control queue command sequence

The minimal state machine for stream id `0` (render) is:

1. **During device start (PnP START / adapter init):**
   * Optionally query `PCM_INFO` to confirm stream 0 exists.
2. **When the first WaveRT stream is created / format is committed:**
   * `PCM_SET_PARAMS` with:
     * `channels = 2`
     * `format = S16_LE`
     * `rate = 48000`
     * `period_bytes` / `buffer_bytes` consistent with section 5
3. **When transitioning to `PAUSE` (or `ACQUIRE → PAUSE`):**
   * `PCM_PREPARE`
4. **When transitioning to `RUN`:**
   * `PCM_START`
5. **When transitioning out of `RUN`:**
   * `PCM_STOP`
6. **When stream is closed or transitions to `STOP`:**
   * `PCM_RELEASE`

The capture stream (id `1`) uses the same control flow, but with:

* `channels = 1` (mono)
* capture buffers submitted via the virtio-snd RX queue (`rxq`)

### 6.2 TX descriptor chain payload

Each TX submission is a virtqueue descriptor chain:

* OUT:
  1. `virtio_snd_pcm_xfer` header (8 bytes)
     * `stream_id: u32` (0)
     * `reserved: u32` (0)
  2. raw PCM bytes (interleaved stereo S16_LE)
* IN:
  1. `virtio_snd_pcm_status` (8 bytes)
     * `status: u32`
     * `latency_bytes: u32` (device model currently returns 0)

The miniport should treat non-OK statuses as a stream fault and transition to `STOP`.

### 6.3 RX descriptor chain payload

Each RX submission is a virtqueue descriptor chain:

* OUT:
  1. `virtio_snd_pcm_xfer` header (8 bytes)
     * `stream_id: u32` (1)
     * `reserved: u32` (0)
* IN:
  1. raw PCM bytes (mono S16_LE) written by the device
  2. `virtio_snd_pcm_status` (8 bytes)
     * `status: u32`
     * `latency_bytes: u32` (device model currently returns 0)

The miniport should treat non-OK statuses as a stream fault and transition to `STOP`.

### 6.4 IRQL constraints (what can run where)

Practical constraints for a stable Win7 driver:

* **Virtio control commands** (`PCM_SET_PARAMS`, `PREPARE`, `START`, `STOP`, `RELEASE`) should run at **`PASSIVE_LEVEL`** because they typically:
  * wait for a response
  * allocate/init buffers
  * may touch pageable code paths
* **TX submissions** can run at **`DISPATCH_LEVEL`** (DPC) *if*:
  * all buffers are nonpaged
  * virtqueue bookkeeping uses spin locks / interlocked ops only
  * no blocking waits occur

If your virtqueue implementation requires `PASSIVE_LEVEL` (e.g., uses KMDF DMA APIs that are passive-only), use a dedicated worker thread:

* DPC only schedules work (queues a work item) and updates cursors conservatively.
* Worker thread performs TX submissions and signals events.

---

## 7) INF requirements (Windows 7 audio miniport installation)

Windows 7 enumerates WDM audio endpoints through `sysaudio.sys` + `wdmaud.sys` conventions. The INF must:

* Install a **PCI function driver service** for the device.
* Register **wave** and **topology** subdevices via `HKR,Drivers\...` keys.
* Register the **KS device interfaces** that SysAudio/MMDevice enumerates for render + capture.
* Include/need the standard KS and WDMAudio registration sections.

### 7.1 Minimal INF outline (key directives)

At minimum:

* `Include=ks.inf, wdmaudio.inf`
* `Needs=KS.Registration, WDMAUDIO.Registration`

And an interfaces section that includes both directions, for example:

* `AddInterface = %KSCATEGORY_RENDER%,  %KSNAME_Wave%, AeroVirtioSnd.Wave.Interface`
* `AddInterface = %KSCATEGORY_CAPTURE%, %KSNAME_Wave%, AeroVirtioSnd.Capture.Interface`
* `AddInterface = %KSCATEGORY_TOPOLOGY%, %KSNAME_Topology%, AeroVirtioSnd.Topology.Interface`

And in `AddReg`:

* `HKR,,DevLoader,,*ntkern`
* `HKR,,NTMPDriver,,aero_virtio_snd.sys`
* `HKR,Drivers,SubClasses,, "wave,topology"`
* `HKR,Drivers\wave,Driver,,aero_virtio_snd.sys`
* `HKR,Drivers\wave,Description,,%AeroVirtioSnd.EndpointDesc%`
* `HKR,Drivers\topology,Driver,,aero_virtio_snd.sys`
* `HKR,Drivers\topology,Description,,%AeroVirtioSnd.TopologyDesc%`

If you want Windows 7 to allocate **message-signaled interrupts** (MSI/MSI-X), you must opt in via INF `HKR` keys under:

* `Interrupt Management\\MessageSignaledInterruptProperties`

The in-tree Aero virtio-snd INFs include this opt-in (recommended).

### 7.2 Hardware IDs

Match at least:

* `PCI\VEN_1AF4&DEV_1059&REV_01` (virtio-snd, Aero contract v1)

For the Aero Windows 7 virtio contract (`AERO-W7-VIRTIO` v1), the in-tree Win7 `virtio-snd`
package is intentionally **revision-gated** and matches only:

* `PCI\VEN_1AF4&DEV_1059&REV_01`

Optionally add more specific matches for your emulator’s subsystem ids if used:

* `PCI\VEN_1AF4&DEV_1059&SUBSYS_XXXXXXXXYYYYYYYY&REV_01` (example placeholder)
* `PCI\VEN_1AF4&DEV_1059&SUBSYS_00191AF4&REV_01` (Aero contract v1 example; also present as a commented-out match in `aero_virtio_snd.inf`)

When testing under QEMU, note that virtio devices may default to `REV_00`. For strict Aero
contract-v1 binding you typically need:

* `-device virtio-sound-pci,disable-legacy=on,x-pci-revision=0x01`

### 7.3 Worked INF example (AddReg excerpt)

This is an example fragment showing only the Win7-critical keys (not a complete INF):

```ini
; --- Registration glue so SysAudio/WDMAud bind correctly ---
[AeroVirtioSnd_Install.NT]
Include=ks.inf,wdmaudio.inf
Needs=KS.Registration,WDMAUDIO.Registration
CopyFiles=AeroVirtioSnd.CopyFiles
AddReg=AeroVirtioSnd.AddReg

[AeroVirtioSnd_Install.NT.Interfaces]
AddInterface=%KSCATEGORY_RENDER%,%KSNAME_Wave%,AeroVirtioSnd.Wave.Interface
AddInterface=%KSCATEGORY_CAPTURE%,%KSNAME_Wave%,AeroVirtioSnd.Capture.Interface
AddInterface=%KSCATEGORY_TOPOLOGY%,%KSNAME_Topology%,AeroVirtioSnd.Topology.Interface

[AeroVirtioSnd.AddReg]
HKR,,DevLoader,,*ntkern
HKR,,NTMPDriver,,aero_virtio_snd.sys

; Tell wdmaud/sysaudio which miniports exist in this driver.
HKR,Drivers,SubClasses,,"wave,topology"

; WaveRT subdevice
HKR,Drivers\wave,Driver,,aero_virtio_snd.sys
HKR,Drivers\wave,Description,,%AeroVirtioSnd.EndpointDesc%

; Topology subdevice
HKR,Drivers\topology,Driver,,aero_virtio_snd.sys
HKR,Drivers\topology,Description,,%AeroVirtioSnd.TopologyDesc%
```

Notes:

* `SubClasses` is a comma-separated string list. Keep the tokens lowercase (`wave`, `topology`) to match common tooling expectations.
* Real drivers often add `AssociatedFilters`, `FriendlyName`, `WaveRT` flags, etc. Start minimal, then add only when you know why.

### 7.4 Optional: MSI/MSI-X opt-in (`Interrupt Management`)

Windows 7 typically allocates MSI/MSI-X only when explicitly requested via INF:

```inf
[AeroVirtioSnd_Install.NT.HW]
AddReg = AeroVirtioSnd_InterruptManagement_AddReg, AeroVirtioSnd_Parameters_AddReg

[AeroVirtioSnd_InterruptManagement_AddReg]
HKR, "Interrupt Management",,0x00000010
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported,        0x00010001, 1
; virtio-snd uses 4 virtqueues + a config interrupt = 5 vectors; request a little extra:
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit,  0x00010001, 8

; Per-device bring-up toggles (defaults):
[AeroVirtioSnd_Parameters_AddReg]
HKR,Parameters,,0x00000010
HKR,Parameters,ForceNullBackend,0x00010001,0
HKR,Parameters,AllowPollingOnly,0x00010001,0
```

Notes:

* `MessageNumberLimit` is a request; Windows may allocate fewer messages.
* `HKR` in a `.NT.HW` section is relative to the device instance’s **Device Parameters** key:
  * `HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters`
  * The bring-up toggles above therefore live under:
    * `...\Device Parameters\Parameters\ForceNullBackend`
    * `...\Device Parameters\Parameters\AllowPollingOnly`
  * Find `<DeviceInstancePath>` via **Device Manager → device → Details → “Device instance path”**.
* When message interrupts are used, drivers must still program virtio MSI-X routing (`msix_config`, `queue_msix_vector`).
  - On Aero contract devices, if MSI-X is enabled at the PCI layer but a virtio MSI-X selector remains
    `VIRTIO_PCI_MSI_NO_VECTOR` (`0xFFFF`) (or the MSI-X entry is masked/unprogrammed), interrupts for that source are
    **suppressed** (no MSI-X message and no INTx fallback).
  - Therefore, if virtio vector programming fails, drivers must not “wait for INTx”; they must either disable MSI-X and
    use INTx (if the platform/resources allow it) or treat the failure as fatal.

---

## 8) Debugging checklist (bring-up and “no sound” triage)

### 8.1 Endpoint enumeration

Checklist:

1. Device Manager:
   * Device appears under **Sound, video and game controllers**.
   * No Code 10 (start failed) / Code 52 (signature enforcement).
2. Sound control panel:
    * A playback device appears (often “Speakers”).
    * A recording device appears (often “Microphone”).
    * Default format lists (at least) 16-bit, 48000 Hz.

Where to look when it fails:

* `C:\Windows\inf\setupapi.dev.log` — INF processing, copy failures, signature issues.
* `C:\Windows\System32\drivers\` — confirm the `.sys` copied.
* Code 52: test-signing / certificate install is wrong (see driver signing docs).

### 8.2 State transitions to `RUN`

Instrument (DbgPrint / WPP) the following points:

* miniport `Init`
* WaveRT `NewStream`
* stream `SetState` transitions
* control queue calls (`SET_PARAMS`, `PREPARE`, `START`, `STOP`, `RELEASE`)

Expected sequence on first playback:

1. stream created (format validated)
2. `SET_PARAMS`
3. `PREPARE`
4. `START`
5. periodic “tick” logs while `RUN`

### 8.3 Periodic notification events

Symptoms if broken:

* playback starts then immediately stops
* audio engine stays in “silent” / no device activity
* apps report success but no sound

Confirm:

* event registration happened (log event pointer)
* event is signaled once per period while `RUN`
* play cursor advances monotonically

### 8.4 TX submission counts

Confirm in logs:

* how many TX buffers were enqueued per second
  * for 10 ms period: ~100 submissions/sec
* how many completions are observed
* whether TX stalls due to “virtqueue full”

When TX stalls:

* verify interrupt/DPC handling for virtqueue completions
* verify the host/device model is consuming descriptors

### 8.5 Useful tools

* **DbgView** for `DbgPrint` output (user-mode collection).
* **Kernel debugger (WinDbg/KD)** for:
  * breakpoints in `SetState`
  * inspecting KS objects and IRPs
* **KSStudio** (WDK) to inspect filters/pins/categories and validate the descriptor surface.

---

## Appendix A: Worked `WAVEFORMATEXTENSIBLE` example (stereo/48k/16-bit)

This is the canonical format the driver should accept and return from data intersection:

```c
// 2ch, 48kHz, 16-bit PCM (WAVEFORMATEXTENSIBLE)
static const WAVEFORMATEXTENSIBLE kVirtioSndWfx = {
    .Format = {
        .wFormatTag = WAVE_FORMAT_EXTENSIBLE,
        .nChannels = 2,
        .nSamplesPerSec = 48000,
        .nAvgBytesPerSec = 48000 * 2 * 2, // 192000
        .nBlockAlign = 2 * 2,             // 4 bytes per frame
        .wBitsPerSample = 16,
        .cbSize = 22,
    },
    .Samples = { .wValidBitsPerSample = 16 },
    .dwChannelMask = SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
    .SubFormat = KSDATAFORMAT_SUBTYPE_PCM,
};
```

If the caller asks for `WAVEFORMATEX` with the same values, you can either reject it (strict) or accept it and still return `WAVEFORMATEXTENSIBLE` (recommended for consistency).
