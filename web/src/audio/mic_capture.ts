import {
  createMicRingBuffer,
  DROPPED_SAMPLES_INDEX,
  micRingBufferWrite,
  READ_POS_INDEX,
  type MicRingBuffer,
  WRITE_POS_INDEX,
} from "./mic_ring.js";
import { formatOneLineError } from "../text";
// Load the AudioWorklet processor module via a URL derived from `import.meta.url`.
//
// Avoid Vite-only `?worker&url` imports so this module can be imported in non-bundled
// environments (e.g. Node unit tests) without custom transforms.
const micWorkletProcessorUrl = new URL("./mic-worklet-processor.js", import.meta.url).toString();

export { createMicRingBuffer, micRingBufferReadInto, type MicRingBuffer } from "./mic_ring.js";

export type MicCaptureState =
  | "inactive"
  | "starting"
  | "active"
  | "muted"
  | "denied"
  | "error";

export interface MicCaptureOptions {
  /** Desired sample rate for the capture graph. Browser may ignore. */
  sampleRate?: number;
  /** Ring buffer capacity in milliseconds. */
  bufferMs?: number;
  /** Prefer AudioWorklet (low latency). Falls back to ScriptProcessorNode. */
  preferWorklet?: boolean;

  deviceId?: string;
  echoCancellation?: boolean;
  noiseSuppression?: boolean;
  autoGainControl?: boolean;
}

type ResolvedMicCaptureOptions = Required<Omit<MicCaptureOptions, "deviceId">> &
  Pick<MicCaptureOptions, "deviceId">;

function isAudioWorkletSupported(): boolean {
  return typeof AudioWorkletNode !== "undefined";
}

export class MicCapture extends EventTarget {
  readonly options: ResolvedMicCaptureOptions;
  ringBuffer: MicRingBuffer;
  /** Actual sample rate of the capture graph (AudioContext.sampleRate). */
  actualSampleRate: number;

  state: MicCaptureState = "inactive";

  private audioContext: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private sourceNode: MediaStreamAudioSourceNode | null = null;
  private workletNode: AudioWorkletNode | null = null;
  private scriptNode: ScriptProcessorNode | null = null;
  private muteRequested = false;

  private permissionStatus: PermissionStatus | null = null;
  private deviceChangeListener: (() => void) | null = null;

  private backend: "worklet" | "script" | null = null;
  private workletInitError: string | null = null;
  private trackLabel: string | null = null;
  private trackEnabled: boolean | null = null;
  private trackMuted: boolean | null = null;
  private trackReadyState: MediaStreamTrackState | null = null;
  private trackSettings: MediaTrackSettings | null = null;
  private trackConstraints: MediaTrackConstraints | null = null;
  private trackCapabilities: MediaTrackCapabilities | null = null;

  constructor(opts: MicCaptureOptions = {}) {
    super();
    this.options = {
      sampleRate: opts.sampleRate ?? 48000,
      bufferMs: opts.bufferMs ?? 100,
      preferWorklet: opts.preferWorklet ?? true,
      deviceId: opts.deviceId ?? "",
      echoCancellation: opts.echoCancellation ?? true,
      noiseSuppression: opts.noiseSuppression ?? true,
      autoGainControl: opts.autoGainControl ?? true,
    };

    // The browser may ignore our requested sample rate when constructing the
    // AudioContext. We'll resize the ring buffer during `start()` once we know
    // the actual capture rate.
    this.actualSampleRate = this.options.sampleRate;
    const capacitySamples = Math.max(1, Math.floor((this.actualSampleRate * this.options.bufferMs) / 1000));
    this.ringBuffer = createMicRingBuffer(capacitySamples);
  }

  async start(): Promise<void> {
    if (this.state === "starting" || this.state === "active" || this.state === "muted") return;

    this.setState("starting");

    try {
      await this.attachPermissionListener();

      const constraints: MediaStreamConstraints = {
        audio: {
          deviceId: this.options.deviceId ? { exact: this.options.deviceId } : undefined,
          channelCount: 1,
          echoCancellation: this.options.echoCancellation,
          noiseSuppression: this.options.noiseSuppression,
          autoGainControl: this.options.autoGainControl,
        },
        video: false,
      };

      // Permission must be requested from an explicit user action, which is the
      // responsibility of the caller (they call start() on click).
      const stream = await navigator.mediaDevices.getUserMedia(constraints);
      this.stream = stream;

      const track = stream.getAudioTracks()[0] ?? null;
      this.trackLabel = track ? track.label || null : null;
      this.trackEnabled = track ? Boolean(track.enabled) : null;
      this.trackMuted = track ? Boolean(track.muted) : null;
      this.trackReadyState = track ? track.readyState : null;
      this.trackSettings = track && typeof track.getSettings === "function" ? track.getSettings() : null;
      this.trackConstraints = track && typeof track.getConstraints === "function" ? track.getConstraints() : null;
      this.trackCapabilities = null;
      if (track && typeof (track as unknown as { getCapabilities?: unknown }).getCapabilities === "function") {
        try {
          // Some browsers may throw; best-effort only.
          this.trackCapabilities = (track as unknown as { getCapabilities: () => MediaTrackCapabilities }).getCapabilities();
        } catch {
          this.trackCapabilities = null;
        }
      }

      const audioContext = new AudioContext({
        sampleRate: this.options.sampleRate,
        latencyHint: "interactive",
      });
      this.audioContext = audioContext;
      this.actualSampleRate = audioContext.sampleRate;

      // Ensure the ring buffer duration matches the actual capture rate.
      const capacitySamples = Math.max(1, Math.floor((this.actualSampleRate * this.options.bufferMs) / 1000));
      if (this.ringBuffer.capacity !== capacitySamples) {
        this.ringBuffer = createMicRingBuffer(capacitySamples);
      }

      // Clear indices/counters from any previous run.
      Atomics.store(this.ringBuffer.header, WRITE_POS_INDEX, 0);
      Atomics.store(this.ringBuffer.header, READ_POS_INDEX, 0);
      Atomics.store(this.ringBuffer.header, DROPPED_SAMPLES_INDEX, 0);

      // Ensure the graph runs (required by some browsers) and that we never
      // leak mic audio to speakers.
      const sinkGain = audioContext.createGain();
      sinkGain.gain.value = 0;
      sinkGain.connect(audioContext.destination);

      const source = audioContext.createMediaStreamSource(stream);
      this.sourceNode = source;

      const preferWorklet = this.options.preferWorklet && isAudioWorkletSupported();
      let workletErr: unknown = null;
      if (preferWorklet) {
        try {
          this.backend = "worklet";
          this.workletInitError = null;
          await audioContext.audioWorklet.addModule(micWorkletProcessorUrl);
          const node = new AudioWorkletNode(audioContext, "aero-mic-capture", {
            numberOfInputs: 1,
            numberOfOutputs: 1,
            outputChannelCount: [1],
            processorOptions: { ringBuffer: this.ringBuffer.sab },
          });
          node.port.onmessage = (ev) => {
            this.dispatchEvent(new MessageEvent("message", { data: ev.data }));
          };
          source.connect(node);
          node.connect(sinkGain);
          this.workletNode = node;
        } catch (err) {
          workletErr = err;
          this.workletInitError = formatOneLineError(err, 512);
          // Fall back to ScriptProcessorNode when possible. This keeps microphone capture working
          // in environments where AudioWorklet is present but fails to initialize (e.g. older
          // browsers or restrictive CSP).
          this.backend = null;
          try {
            this.workletNode?.disconnect();
          } catch {
            // ignore
          }
          this.workletNode = null;
        }
      }

      if (!this.workletNode) {
        this.backend = "script";
        if (typeof audioContext.createScriptProcessor !== "function") {
          const message = formatOneLineError(workletErr ?? "unknown", 512);
          throw new Error(
            `Microphone capture cannot initialize: AudioWorklet failed (${message}) and ScriptProcessorNode is unavailable.`,
          );
        }
        const node = audioContext.createScriptProcessor(2048, 1, 1);
        let statsCounter = 0;
        node.onaudioprocess = (ev) => {
          if (!this.muteRequested) {
            const input = ev.inputBuffer.getChannelData(0);
            micRingBufferWrite(this.ringBuffer, input);
          }

          // Keep UI/debug stats behavior consistent with the AudioWorklet backend.
          // AudioProcessingEvent frequency is much lower than AudioWorklet render quanta, so we
          // can post on every few callbacks without spamming the main thread.
          if ((statsCounter++ & 0x3) === 0) {
            const writePos = Atomics.load(this.ringBuffer.header, WRITE_POS_INDEX) >>> 0;
            const readPos = Atomics.load(this.ringBuffer.header, READ_POS_INDEX) >>> 0;
            const buffered = (writePos - readPos) >>> 0;
            this.dispatchEvent(
              new MessageEvent("message", {
                data: {
                  type: "stats",
                  buffered,
                  dropped: Atomics.load(this.ringBuffer.header, DROPPED_SAMPLES_INDEX) >>> 0,
                },
              }),
            );
          }
        };
        source.connect(node);
        node.connect(sinkGain);
        this.scriptNode = node;
      }

      // Track end covers device removal, permission revocation, etc.
      track?.addEventListener("ended", () => {
        // When this happens mid-session, surface a clear state to the UI.
        // Consumers can call start() again to re-request permission.
        void this.stop();
      });

      // Monitor device changes so the UI can prompt the user to reselect.
      this.deviceChangeListener = () => {
        this.dispatchEvent(new Event("devicechange"));
      };
      navigator.mediaDevices.addEventListener("devicechange", this.deviceChangeListener);

      await audioContext.resume();

      this.setState(this.muteRequested ? "muted" : "active");
    } catch (err: any) {
      const name = err?.name ?? "";
      if (name === "NotAllowedError" || name === "SecurityError") {
        this.setState("denied");
      } else {
        this.setState("error");
      }
      await this.stop();
      throw err;
    }
  }

  async stop(): Promise<void> {
    if (this.deviceChangeListener) {
      navigator.mediaDevices.removeEventListener("devicechange", this.deviceChangeListener);
      this.deviceChangeListener = null;
    }

    if (this.permissionStatus) {
      this.permissionStatus.onchange = null;
      this.permissionStatus = null;
    }

    this.workletNode?.disconnect();
    this.workletNode = null;
    this.scriptNode?.disconnect();
    this.scriptNode = null;
    this.sourceNode?.disconnect();
    this.sourceNode = null;

    this.stream?.getTracks().forEach((t) => t.stop());
    this.stream = null;

    if (this.audioContext) {
      try {
        await this.audioContext.close();
      } catch {
        // ignore
      }
      this.audioContext = null;
    }

    this.backend = null;
    this.workletInitError = null;
    this.trackLabel = null;
    this.trackEnabled = null;
    this.trackMuted = null;
    this.trackReadyState = null;
    this.trackSettings = null;
    this.trackConstraints = null;
    this.trackCapabilities = null;

    if (this.state !== "denied" && this.state !== "error") {
      this.setState("inactive");
    }
  }

  setMuted(muted: boolean): void {
    this.muteRequested = muted;
    if (this.workletNode) {
      this.workletNode.port.postMessage({ type: "set_muted", muted });
    }
    if (this.state === "active" || this.state === "muted") {
      this.setState(muted ? "muted" : "active");
    }
  }

  getDebugInfo(): {
    backend: "worklet" | "script" | null;
    audioContextState: AudioContextState | null;
    workletInitError: string | null;
    trackLabel: string | null;
    trackEnabled: boolean | null;
    trackMuted: boolean | null;
    trackReadyState: MediaStreamTrackState | null;
    trackSettings: MediaTrackSettings | null;
    trackConstraints: MediaTrackConstraints | null;
    trackCapabilities: MediaTrackCapabilities | null;
  } {
    const audioContextState =
      this.audioContext && typeof (this.audioContext as unknown as { state?: unknown }).state === "string"
        ? ((this.audioContext as unknown as { state: string }).state as AudioContextState)
        : null;
    return {
      backend: this.backend,
      audioContextState,
      workletInitError: this.workletInitError,
      trackLabel: this.trackLabel,
      trackEnabled: this.trackEnabled,
      trackMuted: this.trackMuted,
      trackReadyState: this.trackReadyState,
      trackSettings: this.trackSettings,
      trackConstraints: this.trackConstraints,
      trackCapabilities: this.trackCapabilities,
    };
  }

  private async attachPermissionListener(): Promise<void> {
    // Permissions API is not available in all browsers. Best-effort only.
    const navAny: any = navigator;
    if (!navAny.permissions?.query) return;
    try {
      const status = (await navAny.permissions.query({ name: "microphone" })) as PermissionStatus;
      this.permissionStatus = status;
      status.onchange = () => {
        if (status.state === "denied") {
          this.setState("denied");
          void this.stop();
        }
      };
    } catch {
      // ignore
    }
  }

  private setState(state: MicCaptureState): void {
    if (this.state === state) return;
    this.state = state;
    this.dispatchEvent(new Event("statechange"));
  }
}
