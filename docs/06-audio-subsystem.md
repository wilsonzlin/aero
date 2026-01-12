# 06 - Audio Subsystem

## Overview

Windows 7 uses HD Audio (High Definition Audio, Intel HDA) as the primary audio interface. We must emulate this and translate to the Web Audio API.

## Manual Windows 7 smoke test (in-box HDA driver)

Once the HDA controller is wired into the real worker runtime, use the manual checklist at:

- [`docs/testing/audio-windows7.md`](./testing/audio-windows7.md)

It covers Win7 boot, Device Manager enumeration (“High Definition Audio Controller” + “High Definition Audio Device”),
playback/recording validation, and the host-side metrics to capture (AudioWorklet ring buffer level + underrun/overrun counters).

Canonical implementation pointers (to avoid duplicated stacks):

- `crates/aero-audio/src/hda.rs` — canonical HDA device model (playback + capture) + PCM helpers.
- `crates/aero-virtio/src/devices/snd.rs` — canonical virtio-snd device model.
- `docs/windows7-virtio-driver-contract.md` — definitive Windows 7 virtio device/transport contract (`AERO-W7-VIRTIO`, includes virtio-snd).
- `crates/platform/src/audio/worklet_bridge.rs` — AudioWorklet output ring buffer (`SharedArrayBuffer`) layout.
- `crates/platform/src/audio/mic_bridge.rs` — microphone capture ring buffer (`SharedArrayBuffer`) layout.
- `web/src/platform/audio.ts` + `web/src/platform/audio-worklet-processor.js` — AudioWorklet consumer implementation.
- `web/src/audio/mic_capture.ts` + `web/src/audio/mic-worklet-processor.js` — microphone capture producer implementation.

The older `crates/emulator` audio stack is retained behind the `emulator/legacy-audio` feature for reference and targeted tests.

---

## Browser demo + CI coverage

To validate the *real* HDA DMA path end-to-end in the browser (guest PCM DMA → HDA model → `WorkletBridge` → AudioWorklet),
the repo includes a small demo harness:

- **UI button**: click `#init-audio-hda-demo` (“Init audio output (HDA demo)”) in either:
  - the repo-root harness (`src/main.ts`, used by Playwright at `http://127.0.0.1:4173/`), or
  - the production host (`web/src/main.ts`).
- **Implementation**:
  - The CPU worker (`web/src/workers/cpu.worker.ts`) instantiates the WASM export
    `HdaPlaybackDemo` and keeps the AudioWorklet ring buffer ~200ms full.
  - The demo programs a looping guest PCM buffer + BDL and uses the *real* HDA device model
    (`aero_audio::hda::HdaController`) to generate output.
- **E2E test**: `tests/e2e/audio-worklet-hda-demo.spec.ts` asserts that:
  - `AudioContext` reaches `running`,
  - the ring buffer write index advances over time (i.e. real audio data is being produced),
  - underruns stay bounded and overruns remain 0.

---

## Audio Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    Audio Stack                                   │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Windows 7                                                       │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Applications (DirectSound, WASAPI, MME)                 │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Windows Audio Session API (WASAPI)                      │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Audio Engine (audiodg.exe)                              │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  HD Audio Class Driver (hdaudio.sys)                     │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
└───────┼─────────────────────────────────────────────────────────┘
        │  ◄── Emulation Boundary
        ▼
┌─────────────────────────────────────────────────────────────────┐
│                    Aero Audio Emulation                          │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  HD Audio Controller Emulation                           │    │
│  │    - CORB/RIRB (Command/Response Ring Buffers)          │    │
│  │    - Stream Descriptors                                  │    │
│  │    - DMA Position Buffers                                │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Audio Processing                                        │    │
│  │    - Sample Rate Conversion                              │    │
│  │    - Format Conversion                                   │    │
│  │    - Mixing (multiple streams)                           │    │
│  └─────────────────────────────────────────────────────────┘    │
│       │                                                          │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Web Audio API / AudioWorklet                            │    │
│  │    - Low-latency audio output                            │    │
│  │    - Hardware acceleration                               │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Snapshot/Restore (Save States)

Audio snapshots must capture guest-visible progress (DMA positions, buffer state) while treating the Web Audio pipeline as a host resource that may need reinitialization.

### What must be captured

- **Guest-visible HDA controller state**
  - Global registers: `GCTL`, `WAKEEN`, `STATESTS`, `INTCTL`, `INTSTS`, DMA position buffer base (`DPLBASE/DPUBASE`).
  - CORB/RIRB: base addresses, size selectors, read/write pointers, control/status, `RINTCNT`.
  - Stream descriptor registers for each stream: `CTL`, `LPIB`, `CBL`, `LVI`, `FIFOW`, `FIFOS`, `FMT`, `BDPL/BDPU`.
  - Codec runtime state that affects what Windows sees via verbs (converter stream id/channel, converter format,
    amp gain/mute, pin widget control, pin/AFG power state, etc.).
  - Stream DMA runtime progress needed to continue deterministically after restore (BDL index + byte offset and
    resampler fractional position; capture also restores the capture-frame accumulator used for rate conversion).
- Host sample rates used by the controller:
  - output/time base (`output_rate_hz`)
  - mic input (`capture_sample_rate_hz`)
  These are required to restore stream resampler state deterministically (even though they aren't guest-visible directly).
- **Host-side audio plumbing (recreated)**
  - AudioWorklet ring *indices* (`read_pos` / `write_pos` monotonic frame counters + capacity). The ring buffer
    contents are not serialized.
    - Helpers: `aero_platform::audio::worklet_bridge::{WorkletBridge, InterleavedRingBuffer}::snapshot_state()`.

### Restore semantics / limitations

- The browser `AudioContext` / `AudioWorkletNode` is not serializable; on restore the host pipeline is recreated.
- Any buffered host audio (worklet ring contents, decoded/resampled frames) is **not** restored. The device
  reconstructs enough internal bookkeeping to keep *guest-visible* DMA progress deterministic, but the output may
  glitch (typically a short silence/underrun) immediately after restore.
- The goal is *guest-visible determinism*: after restore, Windows should see the same HDA register state and DMA
  position evolution as if execution had continued without a save/load cycle.

## HD Audio Controller Emulation

### Controller Registers

```rust
pub struct HdaController {
    // Global registers
    gcap: u16,       // Global Capabilities
    vmin: u8,        // Minor Version
    vmaj: u8,        // Major Version
    outpay: u16,     // Output Payload Capability
    inpay: u16,      // Input Payload Capability
    gctl: u32,       // Global Control
    wakeen: u16,     // Wake Enable
    statests: u16,   // State Change Status
    gsts: u16,       // Global Status
    outstrmpay: u16, // Output Stream Payload Capability
    instrmpay: u16,  // Input Stream Payload Capability
    intctl: u32,     // Interrupt Control
    intsts: u32,     // Interrupt Status
    
    // CORB registers
    corblbase: u32,  // CORB Lower Base Address
    corbubase: u32,  // CORB Upper Base Address
    corbwp: u16,     // CORB Write Pointer
    corbrp: u16,     // CORB Read Pointer
    corbctl: u8,     // CORB Control
    corbsts: u8,     // CORB Status
    corbsize: u8,    // CORB Size
    
    // RIRB registers
    rirblbase: u32,  // RIRB Lower Base Address
    rirbubase: u32,  // RIRB Upper Base Address
    rirbwp: u16,     // RIRB Write Pointer
    rintcnt: u16,    // Response Interrupt Count
    rirbctl: u8,     // RIRB Control
    rirbsts: u8,     // RIRB Status
    rirbsize: u8,    // RIRB Size
    
    // Stream descriptors
    streams: [StreamDescriptor; 8],
    
    // Codec
    codec: HdaCodec,
    
    // Audio buffer
    audio_buffer: AudioRingBuffer,
}

#[repr(C)]
pub struct StreamDescriptor {
    ctl: u32,        // Control (24 bits) + Status (8 bits)
    lpib: u32,       // Link Position in Buffer
    cbl: u32,        // Cyclic Buffer Length
    lvi: u16,        // Last Valid Index
    reserved: u16,
    fifos: u16,      // FIFO Size
    fmt: u16,        // Format
    reserved2: u32,
    bdpl: u32,       // Buffer Descriptor List Pointer Lower
    bdpu: u32,       // Buffer Descriptor List Pointer Upper
}
```

### CORB/RIRB Processing

```rust
impl HdaController {
    pub fn process_corb(&mut self, memory: &MemoryBus) {
        // Check if CORB is running
        if self.corbctl & HDA_CORBCTL_RUN == 0 {
            return;
        }
        
        let corb_base = ((self.corbubase as u64) << 32) | (self.corblbase as u64);
        
        // Process commands until read pointer catches up to write pointer
        while self.corbrp != self.corbwp {
            // Advance read pointer
            self.corbrp = (self.corbrp + 1) % self.corb_size();
            
            // Read command from CORB
            let cmd_addr = corb_base + (self.corbrp as u64) * 4;
            let command = memory.read_u32(cmd_addr);
            
            // Execute command and get response
            let response = self.execute_codec_command(command);
            
            // Write response to RIRB
            self.write_rirb_response(response, memory);
        }
    }
    
    fn execute_codec_command(&mut self, command: u32) -> u64 {
        let codec_addr = (command >> 28) & 0xF;
        let nid = (command >> 20) & 0x7F;
        let verb = command & 0xFFFFF;
        
        if codec_addr != 0 {
            return 0;  // Only codec 0 exists
        }
        
        self.codec.execute_verb(nid, verb)
    }
    
    fn write_rirb_response(&mut self, response: u64, memory: &mut MemoryBus) {
        let rirb_base = ((self.rirbubase as u64) << 32) | (self.rirblbase as u64);
        
        // Advance write pointer
        self.rirbwp = (self.rirbwp + 1) % self.rirb_size();
        
        // Write response
        let resp_addr = rirb_base + (self.rirbwp as u64) * 8;
        memory.write_u64(resp_addr, response);
        
        // Check if interrupt is needed
        if self.rirbctl & HDA_RIRBCTL_INTCTL != 0 {
            self.intsts |= HDA_INTSTS_CIS;
            self.raise_irq();
        }
    }
}
```

### HD Audio Codec Emulation

```rust
pub struct HdaCodec {
    // Widget tree
    nodes: HashMap<u8, HdaWidget>,
    
    // Codec state
    vendor_id: u32,
    revision_id: u32,
    subsystem_id: u32,
}

pub enum HdaWidget {
    AudioOutput(AudioOutputWidget),
    AudioInput(AudioInputWidget),
    AudioMixer(AudioMixerWidget),
    AudioSelector(AudioSelectorWidget),
    PinComplex(PinComplexWidget),
    VolumeKnob(VolumeKnobWidget),
    PowerWidget(PowerWidget),
    Root(RootWidget),
    AudioFunctionGroup(AudioFunctionGroupWidget),
}

pub struct AudioOutputWidget {
    nid: u8,
    capabilities: u32,
    supported_formats: u32,
    supported_rates: u32,
    // Current state
    format: u16,
    stream_channel: u8,
    gain_left: u8,
    gain_right: u8,
    mute: bool,
}

impl HdaCodec {
    pub fn execute_verb(&mut self, nid: u8, verb: u32) -> u64 {
        // Check for root node queries first
        if nid == 0 {
            return self.handle_root_verb(verb);
        }
        
        let widget = match self.nodes.get_mut(&nid) {
            Some(w) => w,
            None => return 0,
        };
        
        let cmd = (verb >> 8) & 0xFFF;
        let payload = verb & 0xFF;
        
        match cmd {
            // Get Parameter
            0xF00 => self.get_parameter(widget, payload),
            
            // Get Connection Select
            0xF01 => self.get_connection_select(widget),
            
            // Set Connection Select
            0x701 => self.set_connection_select(widget, payload),
            
            // Get Amplifier Gain/Mute
            0xB00 => self.get_amplifier_gain(widget, payload),
            
            // Set Amplifier Gain/Mute
            0x300..=0x3FF => self.set_amplifier_gain(widget, verb & 0xFFFF),
            
            // Get Converter Format
            0xA00 => self.get_converter_format(widget),
            
            // Set Converter Format
            0x200..=0x2FF => self.set_converter_format(widget, verb & 0xFFFF),
            
            // Get Converter Stream/Channel
            0xF06 => self.get_stream_channel(widget),
            
            // Set Converter Stream/Channel
            0x706 => self.set_stream_channel(widget, payload),
            
            // Get Pin Widget Control
            0xF07 => self.get_pin_control(widget),
            
            // Set Pin Widget Control
            0x707 => self.set_pin_control(widget, payload),
            
            // Get Configuration Default
            0xF1C => self.get_config_default(widget),
            
            _ => {
                log::debug!("Unknown verb {:03x} for NID {}", cmd, nid);
                0
            }
        }
    }
    
    fn get_parameter(&self, widget: &HdaWidget, param_id: u32) -> u64 {
        match param_id {
            HDA_PARAM_VENDOR_ID => self.vendor_id as u64,
            HDA_PARAM_REVISION_ID => self.revision_id as u64,
            HDA_PARAM_NODE_COUNT => self.get_node_count(widget),
            HDA_PARAM_FUNC_GROUP_TYPE => self.get_function_group_type(widget),
            HDA_PARAM_AUDIO_CAPS => self.get_audio_caps(widget),
            HDA_PARAM_SUPPORTED_FORMATS => self.get_supported_formats(widget),
            HDA_PARAM_SUPPORTED_RATES => self.get_supported_rates(widget),
            HDA_PARAM_PIN_CAPS => self.get_pin_caps(widget),
            HDA_PARAM_AMP_IN_CAPS => self.get_amp_in_caps(widget),
            HDA_PARAM_AMP_OUT_CAPS => self.get_amp_out_caps(widget),
            HDA_PARAM_CONN_LIST_LEN => self.get_connection_list_len(widget),
            HDA_PARAM_GPIO_COUNT => 0,
            _ => {
                log::debug!("Unknown parameter {:02x}", param_id);
                0
            }
        }
    }
}
```

---

## Audio Stream Processing

### Stream Descriptor Processing

```rust
impl HdaController {
    pub fn process_stream(&mut self, stream_id: usize, memory: &MemoryBus) {
        let stream = &mut self.streams[stream_id];
        
        // Check if stream is running
        if stream.ctl & HDA_STREAM_CTL_RUN == 0 {
            return;
        }
        
        // Parse format
        let format = StreamFormat::from_raw(stream.fmt);
        
        // Get BDL (Buffer Descriptor List)
        let bdl_base = ((stream.bdpu as u64) << 32) | (stream.bdpl as u64);
        
        // Process buffer entries
        let lvi = stream.lvi as usize;
        let current_entry = (stream.lpib / self.entry_size(stream)) as usize;
        
        // Read buffer entry
        let entry_addr = bdl_base + (current_entry as u64) * 16;
        let buffer_addr = memory.read_u64(entry_addr);
        let buffer_len = memory.read_u32(entry_addr + 8);
        let ioc = (memory.read_u32(entry_addr + 12) & 1) != 0;  // Interrupt on Completion
        
        // Calculate how much data to process
        let bytes_to_process = self.calculate_bytes_to_process(stream, buffer_len);
        
        // Read audio data from guest memory
        let mut audio_data = vec![0u8; bytes_to_process as usize];
        memory.read_bulk(buffer_addr + (stream.lpib as u64 % buffer_len as u64), &mut audio_data);
        
        // Convert and send to audio output
        self.process_audio_data(&audio_data, &format);
        
        // Update position
        stream.lpib = (stream.lpib + bytes_to_process) % stream.cbl;
        
        // Check for buffer completion
        if stream.lpib == 0 || current_entry == lvi {
            if ioc {
                self.raise_stream_interrupt(stream_id);
            }
        }
    }
}
```

### Sample Format Conversion

```rust
pub struct StreamFormat {
    pub sample_rate: u32,
    pub bits_per_sample: u8,
    pub channels: u8,
}

impl StreamFormat {
    pub fn from_raw(fmt: u16) -> Self {
        // HDA format register encoding:
        // Bits 15-14: Stream type
        // Bits 13-11: Sample base rate
        // Bit 10: Base rate multiply
        // Bits 9-8: Base rate divisor
        // Bits 7-4: Bits per sample
        // Bits 3-0: Number of channels - 1
        
        let base = if fmt & (1 << 14) != 0 { 44100 } else { 48000 };
        let mult = match (fmt >> 11) & 0x7 {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => 4,
            _ => 1,
        };
        let div = match (fmt >> 8) & 0x7 {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => 4,
            4 => 5,
            5 => 6,
            6 => 7,
            7 => 8,
            _ => 1,
        };
        
        let bits = match (fmt >> 4) & 0x7 {
            0 => 8,
            1 => 16,
            2 => 20,
            3 => 24,
            4 => 32,
            _ => 16,
        };
        
        let channels = ((fmt & 0xF) + 1) as u8;
        
        Self {
            sample_rate: (base * mult) / div,
            bits_per_sample: bits,
            channels,
        }
    }
}

pub fn convert_audio_samples(
    input: &[u8],
    input_format: &StreamFormat,
    output_format: &StreamFormat,
) -> Vec<f32> {
    let samples = decode_samples(input, input_format);
    
    // Resample if needed
    let resampled = if input_format.sample_rate != output_format.sample_rate {
        resample(&samples, input_format.sample_rate, output_format.sample_rate)
    } else {
        samples
    };
    
    // Channel conversion if needed
    let remixed = if input_format.channels != output_format.channels {
        remix_channels(&resampled, input_format.channels, output_format.channels)
    } else {
        resampled
    };
    
    remixed
}

fn decode_samples(data: &[u8], format: &StreamFormat) -> Vec<f32> {
    let bytes_per_sample = (format.bits_per_sample / 8) as usize;
    let sample_count = data.len() / bytes_per_sample;
    
    let mut samples = Vec::with_capacity(sample_count);
    
    for i in 0..sample_count {
        let offset = i * bytes_per_sample;
        let sample = match format.bits_per_sample {
            8 => {
                // Unsigned 8-bit
                let val = data[offset] as f32;
                (val - 128.0) / 128.0
            }
            16 => {
                // Signed 16-bit little-endian
                let val = i16::from_le_bytes([data[offset], data[offset + 1]]);
                val as f32 / 32768.0
            }
            24 => {
                // Signed 24-bit little-endian
                let val = i32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], 0]);
                (val >> 8) as f32 / 8388608.0
            }
            32 => {
                // 32-bit float
                f32::from_le_bytes([data[offset], data[offset + 1], data[offset + 2], data[offset + 3]])
            }
            _ => 0.0,
        };
        samples.push(sample);
    }
    
    samples
}
```

---

## Web Audio API Integration

### SharedArrayBuffer output ring buffer semantics

Audio output is bridged from the emulator thread to the `AudioWorkletProcessor`
via a `SharedArrayBuffer` SPSC ring buffer (producer = emulator, consumer =
AudioWorklet).

Overrun/backpressure policy for **output/playback** rings is **drop-new**:

- The producer never advances the consumer-owned read index to "make room".
- Writes are truncated to the available free space.
- Telemetry counts overruns as the number of frames dropped because the buffer
  was full (new frames not written).

This matches the semantics in:

- `crates/platform/src/audio/worklet_bridge.rs` (wasm producer)
- `web/src/platform/audio.ts` (JS producer)
- `crates/aero-audio/src/ring.rs` (native test ring)

### AudioWorklet Processor

```javascript
// audio-processor.js - runs in AudioWorklet context
class AeroAudioProcessor extends AudioWorkletProcessor {
    constructor() {
        super();
        this.buffer = new Float32Array(0);
        this.bufferOffset = 0;
        
        this.port.onmessage = (event) => {
            if (event.data.type === 'audio_data') {
                this.appendBuffer(event.data.samples);
            }
        };
    }
    
    appendBuffer(samples) {
        const newBuffer = new Float32Array(this.buffer.length - this.bufferOffset + samples.length);
        newBuffer.set(this.buffer.subarray(this.bufferOffset));
        newBuffer.set(samples, this.buffer.length - this.bufferOffset);
        this.buffer = newBuffer;
        this.bufferOffset = 0;
    }
    
    process(inputs, outputs, parameters) {
        const output = outputs[0];
        const samplesNeeded = output[0].length;
        
        for (let channel = 0; channel < output.length; channel++) {
            const outputChannel = output[channel];
            
            for (let i = 0; i < samplesNeeded; i++) {
                const sampleIndex = this.bufferOffset + i * output.length + channel;
                
                if (sampleIndex < this.buffer.length) {
                    outputChannel[i] = this.buffer[sampleIndex];
                } else {
                    outputChannel[i] = 0;  // Underrun - output silence
                }
            }
        }
        
        this.bufferOffset += samplesNeeded * output.length;
        
        // Report buffer level back to main thread
        this.port.postMessage({
            type: 'buffer_level',
            samples: this.buffer.length - this.bufferOffset,
        });
        
        return true;  // Keep processor alive
    }
}

registerProcessor('aero-audio-processor', AeroAudioProcessor);
```

### Rust Audio Manager

```rust
pub struct AudioManager {
    audio_context: AudioContext,
    worklet_node: Option<AudioWorkletNode>,
    sample_rate: u32,
    
    // Ring buffer for audio data
    ring_buffer: SharedArrayBuffer,
    write_pos: AtomicU32,
    read_pos: AtomicU32,
}

impl AudioManager {
    pub async fn initialize() -> Result<Self> {
        // Create audio context
        let audio_context = AudioContext::new(&AudioContextOptions {
            sample_rate: Some(48000.0),
            latency_hint: "interactive",
        })?;
        
        // Load and register AudioWorklet
        audio_context.audio_worklet()
            .add_module("audio-processor.js")
            .await?;
        
        // Create worklet node
        let worklet_node = AudioWorkletNode::new(
            &audio_context,
            "aero-audio-processor",
            &AudioWorkletNodeOptions {
                output_channel_count: vec![2],  // Stereo
            },
        )?;
        
        // Connect to destination
        worklet_node.connect(&audio_context.destination())?;
        
        Ok(Self {
            audio_context,
            worklet_node: Some(worklet_node),
            sample_rate: 48000,
            ring_buffer: SharedArrayBuffer::new(4 * 48000),  // 1 second buffer
            write_pos: AtomicU32::new(0),
            read_pos: AtomicU32::new(0),
        })
    }
    
    pub fn queue_audio(&self, samples: &[f32]) {
        // Send to AudioWorklet
        if let Some(ref node) = self.worklet_node {
            node.port().post_message(&AudioMessage {
                msg_type: "audio_data",
                samples: samples.to_vec(),
            });
        }
    }
    
    pub fn get_buffer_level(&self) -> usize {
        let write = self.write_pos.load(Ordering::Acquire);
        let read = self.read_pos.load(Ordering::Acquire);
        
        if write >= read {
            (write - read) as usize
        } else {
            (self.ring_buffer.byte_length() as u32 - read + write) as usize
        }
    }
}
```

---

## Audio Input (Microphone)

Microphone capture is bridged from the browser to the guest via a SharedArrayBuffer ring buffer:

- **Main thread**: requests permission on explicit user action and manages lifecycle/UI state
- **AudioWorklet** (preferred): pulls mic PCM frames with low latency and writes them into the ring
- **Emulator worker**: reads from the ring and feeds the emulated capture device (HDA input pin or
  virtio-snd capture stream)

Reference implementation files:

- `web/src/audio/mic_capture.ts` (permission + lifecycle + ring buffer allocation)
- `web/src/audio/mic_ring.js` (canonical mic ring buffer layout + read/write helpers shared by main thread/worklet/workers)
- `web/src/audio/mic-worklet-processor.js` (AudioWorklet capture writer)
- `crates/platform/src/audio/mic_bridge.rs` (ring buffer layout + wrap-around math)
- `crates/aero-audio/src/capture.rs` (`AudioCaptureSource` trait + adapters for mic ring buffers)
- `crates/aero-audio/src/hda.rs` (HDA capture stream DMA + mic pin exposure)
- `crates/aero-virtio/src/devices/snd.rs` (virtio-snd capture stream + RX queue)

### HDA capture exposure (guest)

The canonical `aero-audio` HDA model exposes one capture stream and a microphone pin widget:

- **Stream DMA**: `SD1` (input stream 0) DMA-writes captured PCM bytes into guest memory via BDL entries.
- **Codec topology**: an input converter widget (`NID 4`) plus a mic pin widget (`NID 5`) so Windows can
  enumerate a recording endpoint.

Host code provides microphone samples via `aero_audio::capture::AudioCaptureSource` (implemented for
`aero_platform::audio::mic_bridge::MicBridge` on wasm) and advances the device model via
`HdaController::process_*_with_capture(...)`.

### virtio-snd capture exposure (guest)

Guest-visible virtio-snd behavior (stream ids, queues, formats) is specified by the
[`AERO-W7-VIRTIO` contract](./windows7-virtio-driver-contract.md#34-virtio-snd-audio).

The canonical `aero-virtio` virtio-snd device model exposes an additional fixed-format capture stream:

- Stream id `1`, S16_LE mono @ 48kHz.
- Captured data is delivered to the guest via the virtio-snd RX queue (`VIRTIO_SND_QUEUE_RX`).

In browser builds, the device can be backed by the mic SharedArrayBuffer ring buffer via
`aero_platform::audio::mic_bridge::MicBridge` (through the capture-source adapter in
`crates/aero-virtio/src/devices/snd.rs`).

### Ring buffer layout
  
The microphone ring buffer is a mono `Float32Array` backed by a `SharedArrayBuffer`:

| Offset | Type | Meaning |
|--------|------|---------|
| 0      | u32  | `write_pos` (monotonic sample counter) |
| 4      | u32  | `read_pos` (monotonic sample counter) |
| 8      | u32  | `dropped` (samples dropped due to buffer full) |
| 12     | u32  | `capacity` (samples in the data section) |
| 16..   | f32[]| PCM samples, index = `(write_pos % capacity)` |

### Playback ring buffer layout (AudioWorklet output)

Audio output uses a separate `SharedArrayBuffer` ring buffer consumed by an `AudioWorkletProcessor`.
Unlike the microphone ring, this ring buffer uses **frame indices** (not sample indices); each frame
contains `channelCount` interleaved `f32` samples.

**Important: sample rate mismatches**

Browsers may ignore the requested `AudioContext` sample rate (Safari/iOS commonly runs at 44.1kHz).
The AudioWorklet consumes frames at `AudioContext.sampleRate`, so the device models must be configured
to produce audio at that *actual* rate:

- HDA (`aero_audio::hda::HdaController`):
  - set `output_rate_hz` to the output `AudioContext.sampleRate`
  - if microphone capture uses a different audio graph/sample rate, set `capture_sample_rate_hz` too
- virtio-snd (`aero_virtio::devices::snd::VirtioSnd`):
  - set `host_sample_rate_hz` to the output `AudioContext.sampleRate`
  - if microphone capture uses a different sample rate, set `capture_sample_rate_hz` too

| Offset | Type | Meaning |
|--------|------|---------|
| 0      | u32  | `readFrameIndex` (monotonic frame counter, consumer-owned) |
| 4      | u32  | `writeFrameIndex` (monotonic frame counter, producer-owned) |
| 8      | u32  | `underrunCount` (total missing output frames rendered as silence due to underruns; wraps at 2^32) |
| 12     | u32  | `overrunCount` (frames dropped by the producer due to buffer full; wraps at 2^32) |
| 16..   | f32[]| Interleaved PCM samples (`L0, R0, L1, R1, ...`) |

Canonical implementation:

- `web/src/platform/audio.ts` (producer)
- `web/src/platform/audio-worklet-processor.js` (consumer)

### Capture constraints (echo/noise)

Capture uses `getUserMedia` audio constraints to expose user-facing toggles:

- `echoCancellation`
- `noiseSuppression`
- `autoGainControl`

These are applied when the stream is created (and may be updated later via
`MediaStreamTrack.applyConstraints` when supported).

---

## Latency Management

### Buffer Sizing

```rust
const MIN_BUFFER_MS: u32 = 10;   // Minimum latency
const TARGET_BUFFER_MS: u32 = 50; // Target latency
const MAX_BUFFER_MS: u32 = 200;  // Maximum before dropping

impl AudioManager {
    pub fn calculate_buffer_size(&self, sample_rate: u32) -> usize {
        (sample_rate * TARGET_BUFFER_MS / 1000) as usize
    }
    
    pub fn adjust_playback_rate(&mut self) {
        let buffer_level = self.get_buffer_level();
        let target = self.calculate_buffer_size(self.sample_rate);
        
        if buffer_level < target / 2 {
            // Buffer running low - slow down slightly
            self.playback_rate = 0.99;
        } else if buffer_level > target * 2 {
            // Buffer too full - speed up slightly
            self.playback_rate = 1.01;
        } else {
            self.playback_rate = 1.0;
        }
    }
}
```

---

## AC'97 Fallback (Legacy)

AC'97 is **not** part of the canonical Aero audio stack. The only AC'97 device
model in this repository lives in the legacy `crates/emulator` audio stack, which
is gated behind `emulator/legacy-audio` (see ADR 0010).

The canonical path is:

- HDA: `crates/aero-audio/src/hda.rs`
- virtio-snd: `crates/aero-virtio/src/devices/snd.rs`

The snippet below is illustrative of the legacy AC'97 model:

```rust
pub struct Ac97Controller {
    // Mixer registers
    master_volume: u16,
    pcm_volume: u16,
    mic_volume: u16,
    
    // Buffer descriptors
    pcm_out_bdbar: u32,
    pcm_out_civ: u8,
    pcm_out_lvi: u8,
    pcm_out_sr: u16,
    pcm_out_cr: u8,
}

impl Ac97Controller {
    pub fn read_mixer(&self, offset: u32) -> u16 {
        match offset {
            AC97_RESET => self.get_capabilities(),
            AC97_MASTER_VOL => self.master_volume,
            AC97_PCM_OUT_VOL => self.pcm_volume,
            AC97_MIC_VOL => self.mic_volume,
            AC97_VENDOR_ID1 => 0x8384,  // Intel
            AC97_VENDOR_ID2 => 0x7600,  // ICH6
            _ => 0,
        }
    }
}
```

---

## Performance Targets

| Metric | Target | Measurement |
|--------|--------|-------------|
| Audio Latency | < 50ms | Round-trip time |
| Sample Rate | 44.1/48 kHz | Native support |
| Bit Depth | 16/24-bit | Format support |
| Underruns | < 1/minute | Buffer monitoring |
| CPU Usage | < 5% | Audio processing overhead |

---

## Next Steps

- See [Networking](./07-networking.md) for network stack emulation
- See [Browser APIs](./11-browser-apis.md) for Web Audio details
- See [Task Breakdown](./15-agent-task-breakdown.md) for audio tasks
